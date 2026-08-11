//! SoundEntries.dbc loader — the central **sound-kit** table every audio trigger resolves through
//! (decision 0070): a kit = up to 10 weighted variation files + volume/flags/distance parameters.
//! Kits are playable by id and **by name** (the client's `PlaySoundByName`, RE `0x458030`, is a
//! name-hash into this table — the Lua `PlaySound("igMainMenuOpen")` path).
//!
//! Layout — VERIFIED against build 5875 (xxd + row decode of the extracted file, 2026-07-02): the
//! WDBC header reports **4623 records · 29 fields · 116 B/record**. Fields:
//! `ID(0), SoundType(1), Name(2, str), File[10](3..12, str), Freq[10](13..22),
//! DirectoryBase(23, str), Volume(24, f32), Flags(25), MinDistance(26, f32),
//! DistanceCutoff(27, f32), EAXDef(28)`. Spot-checked on row 3: `type 1 "Invisibility Impact",
//! "Dispel_Low_Base.wav" ×1, dir "Sound\Spells", vol 1.0, flags 0, min 8, cutoff 45, EAX 2`.
//! NOTE the wowdev-wiki 5875 struct claims a 30-column layout (separate `maxDistance` +
//! `soundEntriesAdvancedID`) — it is **wrong**; trust this byte-verified one (decision 0070).

use std::collections::HashMap;

use crate::Chain;
use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{f32_at, parse, str_at, u32_at};

const SOUND_ENTRIES: &str = "DBFilesClient\\SoundEntries.dbc";

/// `Flags` bits observed in the 5875 data (domain: 0/1/0x20/0x21/0x22/0x200/0x201/0x220/0x400/
/// 0x420). The DBC word is copied **raw** into the runtime kit flag word (`0x45c139`, wow-re
/// `benilla-pins.md` B2, VERIFIED) and the two variation gates read separate bits: `0x400` =
/// pitch variation (`0x458da0`), `0x800` = volume variation (`0x458c60`). No 5875 kit sets
/// `0x800` — volume variation is dormant in this build's data. `0x20` no-duplicates, `0x200`
/// looping (0.5.3-era wowdev meanings, behavior-consistent).
pub mod sound_kit_flags {
    pub const NO_DUPLICATES: u32 = 0x20;
    pub const LOOPING: u32 = 0x200;
    pub const VARY_PITCH: u32 = 0x400;
    pub const VARY_VOLUME: u32 = 0x800;
}

/// One sound kit: the resolved variation list + the playback parameters the kit player consumes.
pub struct SoundKit {
    pub id: u32,
    /// `SoundType` — the kit's category (1 spells, 2 UI, 3 footsteps, … 28 zone music, 50 zone
    /// ambience). Drives the volume-category pick and the specialized runtime caches.
    pub sound_type: u32,
    /// The `PlaySoundByName` key (e.g. `"igMainMenuOpen"`, `"LevelUp"`).
    pub name: String,
    /// Variation files as `(full MPQ path, weight)` — `DirectoryBase\File[i]` joined here so
    /// consumers never re-derive paths; only non-empty slots, weight from the matching `Freq[i]`.
    pub files: Vec<(String, u32)>,
    /// Base volume `[0,1]` (the per-shot variation math scales this — wow-re `0x458c60`).
    pub volume: f32,
    pub flags: u32,
    /// Full-volume radius fed to the backend's min/max rolloff (FMOD `Sample_SetMinMaxDistance`).
    pub min_distance: f32,
    /// Selection/cull radius: the `d² < cutoff²` audibility gate + per-frame virtualization
    /// (wow-re `0x45cdf0`/`0x7a5000`). `0` = non-positional (no 3D cull).
    pub distance_cutoff: f32,
    /// `SoundSamplePreferences.dbc` FK — the per-channel EAX wet send (0/1/2 in the data;
    /// 2 072 kits carry 0, 2 549 carry 2, 2 carry 1). **`0` means dry, not "default"**: that DBC
    /// holds only ids 1 and 2, so the client's id-indexed slot lookup (`0x45cdc0`) returns NULL
    /// and `FSOUND_Reverb_SetChannelProperties` (`0x7a5bf0`) skips before it even tests the
    /// 3D-open flag. Authored dryness — and it is how NPC voice lines stay out of an interior's
    /// reverb: **all 275 `SoundType 17` rows are `EAXDef 0`** (creature barks split 706 wet /
    /// 285 dry). wow-re `reverb-default-and-eax-hardware.md`; benilla decision 1155.
    pub eax_def: u32,
}

/// All kits, resolvable by id or (case-insensitively) by name.
pub struct SoundKitCatalog {
    kits: HashMap<u32, SoundKit>,
    /// Lowercased `Name` → id — the client's name lookup is a case-insensitive hash.
    by_name: HashMap<String, u32>,
}

impl SoundKitCatalog {
    pub fn get(&self, id: u32) -> Option<&SoundKit> {
        self.kits.get(&id)
    }

    /// `PlaySoundByName` parity: resolve a kit by its `Name` column, case-insensitive.
    pub fn by_name(&self, name: &str) -> Option<&SoundKit> {
        self.by_name
            .get(&name.to_ascii_lowercase())
            .and_then(|id| self.kits.get(id))
    }

    pub fn len(&self) -> usize {
        self.kits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kits.is_empty()
    }

    /// An empty catalog for consumers' pure-logic unit tests (selector state machines that need
    /// a catalog-shaped owner but no data).
    pub fn empty_for_tests() -> Self {
        Self {
            kits: HashMap::new(),
            by_name: HashMap::new(),
        }
    }
}

/// 29 fields — module docs carry the verified layout.
fn sound_entries_schema() -> Schema {
    let mut s = Schema::new("SoundEntries");
    s.add_field(SchemaField::new("ID", FieldType::UInt32));
    s.add_field(SchemaField::new("SoundType", FieldType::UInt32));
    s.add_field(SchemaField::new("Name", FieldType::String));
    for i in 0..10 {
        s.add_field(SchemaField::new(format!("File{i}"), FieldType::String));
    }
    for i in 0..10 {
        s.add_field(SchemaField::new(format!("Freq{i}"), FieldType::UInt32));
    }
    s.add_field(SchemaField::new("DirectoryBase", FieldType::String));
    s.add_field(SchemaField::new("Volume", FieldType::Float32));
    s.add_field(SchemaField::new("Flags", FieldType::UInt32));
    s.add_field(SchemaField::new("MinDistance", FieldType::Float32));
    s.add_field(SchemaField::new("DistanceCutoff", FieldType::Float32));
    s.add_field(SchemaField::new("EAXDef", FieldType::UInt32));
    s
}

/// Read SoundEntries.dbc off the patch chain into a [`SoundKitCatalog`].
pub fn load_sound_kit_catalog(chain: &mut Chain) -> Result<SoundKitCatalog> {
    let bytes = chain
        .read_file(SOUND_ENTRIES)
        .with_context(|| format!("reading {SOUND_ENTRIES}"))?;
    let rs = parse(&bytes, sound_entries_schema(), "SoundEntries")?;
    let mut kits = HashMap::with_capacity(rs.records().len());
    let mut by_name = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        let name = str_at(&rs, r, 2).unwrap_or_default();
        let dir = str_at(&rs, r, 23).unwrap_or_default();
        let mut files = Vec::new();
        for i in 0..10 {
            let Some(file) = str_at(&rs, r, 3 + i).filter(|f| !f.is_empty()) else {
                continue;
            };
            let weight = u32_at(r, 13 + i).unwrap_or(0);
            let path = if dir.is_empty() {
                file
            } else {
                format!("{dir}\\{file}")
            };
            files.push((path, weight));
        }
        if !name.is_empty() {
            by_name.insert(name.to_ascii_lowercase(), id);
        }
        kits.insert(
            id,
            SoundKit {
                id,
                sound_type: u32_at(r, 1).unwrap_or(0),
                name,
                files,
                volume: f32_at(r, 24).unwrap_or(1.0),
                flags: u32_at(r, 25).unwrap_or(0),
                min_distance: f32_at(r, 26).unwrap_or(0.0),
                distance_cutoff: f32_at(r, 27).unwrap_or(0.0),
                eax_def: u32_at(r, 28).unwrap_or(0),
            },
        );
    }
    Ok(SoundKitCatalog { kits, by_name })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repo root's `WoW/Data` (gitignored; the real-data tests skip when absent).
    fn vanilla_data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
    }

    /// End-to-end on the **real** build-5875 table: the byte-verified 29-field layout parses all
    /// 4623 kits; the spot-checked row (ID 3) reads back exactly; the `PlaySoundByName` lookup is
    /// case-insensitive; and a kit's joined `DirectoryBase\File` path is a real, readable chain
    /// file (guards the path join against a shifted column). Skips without client data.
    #[test]
    fn real_sound_entries_parse_and_resolve() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_sound_kit_catalog(&mut chain).expect("load sound kits");
        assert_eq!(cat.len(), 4623, "all 5875 SoundEntries rows load");

        // The row decoded byte-by-byte while pinning the layout (module docs).
        let kit = cat.get(3).expect("kit 3 exists");
        assert_eq!(kit.name, "Invisibility Impact");
        assert_eq!(kit.sound_type, 1);
        assert_eq!(kit.files.len(), 1);
        assert_eq!(
            kit.files[0],
            ("Sound\\Spells\\Dispel_Low_Base.wav".into(), 1)
        );
        assert_eq!(kit.volume, 1.0);
        assert_eq!(kit.flags, 0);
        assert_eq!(kit.min_distance, 8.0);
        assert_eq!(kit.distance_cutoff, 45.0);
        assert_eq!(kit.eax_def, 2);

        // Name lookup, case-insensitive (the client name-hash ignores case).
        let ui = cat.by_name("IGMINIMAPZOOMIN").expect("UI kit by name");
        assert_eq!(ui.id, 823);
        assert_eq!(ui.sound_type, 2, "type 2 = UI");

        // The joined path of a UI kit resolves to real bytes on the chain.
        let (path, _) = &ui.files[0];
        let bytes = chain.read(path).expect("kit file readable off the chain");
        assert!(
            bytes.len() > 1000,
            "{path} is a real WAV ({} B)",
            bytes.len()
        );
    }

    /// The `EAXDef` census the reverb send is gated on (decision 1155, bug B236). `EAXDef 0` is a
    /// NULL `SoundSamplePreferences` slot in the client, i.e. a channel that never receives reverb
    /// properties — so this is the authored line between wet and dry, and it must not drift.
    /// Skips without client data.
    #[test]
    fn real_sound_entries_eaxdef_census() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_sound_kit_catalog(&mut chain).expect("load sound kits");

        // Only three values exist, and `SoundSamplePreferences.dbc` has rows 1 and 2 only — so 0
        // is "no row", never "row zero".
        let mut n = [0usize; 3];
        for k in cat.kits.values() {
            assert!(k.eax_def <= 2, "kit {} has EAXDef {}", k.id, k.eax_def);
            n[k.eax_def as usize] += 1;
        }
        assert_eq!(
            (n[0], n[1], n[2]),
            (2072, 2, 2549),
            "the 5875 EAXDef census"
        );

        // The load-bearing one: NPC voice lines (`SoundType 17`, what `NPCSounds.dbc` references)
        // are dry to a kit — this is why the Thunderbrew Distillery's NPCs carry no echo.
        let voices: Vec<_> = cat.kits.values().filter(|k| k.sound_type == 17).collect();
        assert_eq!(voices.len(), 275, "the type-17 NPC voice rows");
        assert!(
            voices.iter().all(|k| k.eax_def == 0),
            "every NPC voice kit is authored dry"
        );

        // The control: creature barks are a genuine mix, so the gate is not a no-op that happens
        // to silence everything.
        let barks: Vec<_> = cat.kits.values().filter(|k| k.sound_type == 10).collect();
        assert!(
            barks.iter().any(|k| k.eax_def != 0) && barks.iter().any(|k| k.eax_def == 0),
            "creature barks split wet/dry"
        );
    }
}
