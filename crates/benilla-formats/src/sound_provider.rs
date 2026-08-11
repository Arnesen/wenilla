//! `SoundProviderPreferences.dbc` — the zone reverb presets (EAX listener properties).
//!
//! Layout — VERIFIED against build 5875 (header + full row dump, 2026-07-03): **38 × 24 × 96 B**:
//! `ID(0), Description(1,str), Flags(2), EAXEnvironmentSelection(3), EAXDecayTime(4,f),
//! EAX2EnvironmentSize(5,f), EAX2EnvironmentDiffusion(6,f), EAX2Room(7,i), EAX2RoomHF(8,i),
//! EAX2DecayHFRatio(9,f), EAX2Reflections(10,i), EAX2ReflectionsDelay(11,f), EAX2Reverb(12,i),
//! EAX2ReverbDelay(13,f), EAX2RoomRolloff(14,f), EAX2AirAbsorption(15,f), EAX3RoomLF(16,i),
//! EAX3DecayLFRatio(17,f), EAX3EchoTime(18,f), EAX3EchoDepth(19,f), EAX3ModulationTime(20,f),
//! EAX3ModulationDepth(21,f), EAX3HFReference(22,f), EAX3LFReference(23,f)`.
//! External cross-check: rows 66–92 are the canonical EAX preset table (`PRESET_GENERIC` decay
//! 1.49 s, Room −1000 mB, Reflections −2602 mB @ 0.007 s, Reverb 200 mB @ 0.011 s — the published
//! EAX SDK values, byte-exact), which pins every column's meaning independently of the wiki.
//! Levels are **millibels** (dB × 100), times in seconds.
//!
//! Who references these: `AreaTable` cols 5/6 (dry/underwater — in 1.12 only 8 areas carry a dry
//! pref, all dungeon floors; 568 carry underwater pref 11) and `WMOAreaTable` cols 4/5 (the real
//! payload: ~4 000 interior group rows, CAVE/AUDITORIUM/ARENA — wired when WMO interior
//! containment lands). The struct carries the EAX2 core the mixer's reverb consumes; the EAX3
//! extras (cols 16–23) are near-constant defaults in 1.12 and stay unparsed until a backend uses
//! them.

use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{f32_at, parse, str_at, u32_at};
use crate::Chain;

/// One reverb preset — EAX2 listener properties, in EAX units (mB levels, seconds).
pub struct SoundProvider {
    pub id: u32,
    /// Preset name ("PRESET_CAVE", "Underwater", …) — debug/display only.
    pub name: String,
    pub flags: u32,
    /// Reverberation decay time, seconds (EAX range 0.1–20).
    pub decay_time: f32,
    /// Master room effect level, mB (−10000..0).
    pub room: i32,
    /// Room effect high-frequency level, mB (−10000..0) — the muffle of the wet signal.
    pub room_hf: i32,
    /// High-frequency to overall decay ratio (0.1–2; <1 = highs die faster).
    pub decay_hf_ratio: f32,
    /// Early-reflections level, mB (−10000..1000).
    pub reflections: i32,
    /// Late-reverberation level, mB (−10000..2000).
    pub reverb: i32,
    /// Environment diffusion (0–1; low = echoey, high = smooth).
    pub env_diffusion: f32,
    /// Apparent room size, meters-ish (EAX environment size, 1–100).
    pub env_size: f32,
}

/// All presets by id.
pub struct SoundProviderCatalog {
    providers: HashMap<u32, SoundProvider>,
}

impl SoundProviderCatalog {
    pub fn get(&self, id: u32) -> Option<&SoundProvider> {
        self.providers.get(&id)
    }

    pub fn len(&self) -> usize {
        self.providers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

fn schema() -> Schema {
    let mut s = Schema::new("SoundProviderPreferences");
    s.add_field(SchemaField::new("ID", FieldType::UInt32));
    s.add_field(SchemaField::new("Description", FieldType::String));
    s.add_field(SchemaField::new("Flags", FieldType::UInt32));
    s.add_field(SchemaField::new(
        "EAXEnvironmentSelection",
        FieldType::UInt32,
    ));
    s.add_field(SchemaField::new("EAXDecayTime", FieldType::Float32));
    s.add_field(SchemaField::new("EAX2EnvironmentSize", FieldType::Float32));
    s.add_field(SchemaField::new(
        "EAX2EnvironmentDiffusion",
        FieldType::Float32,
    ));
    s.add_field(SchemaField::new("EAX2Room", FieldType::UInt32));
    s.add_field(SchemaField::new("EAX2RoomHF", FieldType::UInt32));
    s.add_field(SchemaField::new("EAX2DecayHFRatio", FieldType::Float32));
    s.add_field(SchemaField::new("EAX2Reflections", FieldType::UInt32));
    s.add_field(SchemaField::new("EAX2ReflectionsDelay", FieldType::Float32));
    s.add_field(SchemaField::new("EAX2Reverb", FieldType::UInt32));
    s.add_field(SchemaField::new("EAX2ReverbDelay", FieldType::Float32));
    s.add_field(SchemaField::new("EAX2RoomRolloff", FieldType::Float32));
    s.add_field(SchemaField::new("EAX2AirAbsorption", FieldType::Float32));
    for name in [
        "EAX3RoomLF",
        "EAX3DecayLFRatio",
        "EAX3EchoTime",
        "EAX3EchoDepth",
        "EAX3ModulationTime",
        "EAX3ModulationDepth",
        "EAX3HFReference",
        "EAX3LFReference",
    ] {
        s.add_field(SchemaField::new(name, FieldType::UInt32));
    }
    s
}

/// Read `SoundProviderPreferences.dbc` off the patch chain.
pub fn load_sound_provider_catalog(chain: &mut Chain) -> Result<SoundProviderCatalog> {
    let bytes = chain
        .read_file("DBFilesClient\\SoundProviderPreferences.dbc")
        .context("reading SoundProviderPreferences.dbc")?;
    let rs = parse(&bytes, schema(), "SoundProviderPreferences")?;
    let mut providers = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        let i32_at = |i: usize| u32_at(r, i).unwrap_or(0) as i32;
        providers.insert(
            id,
            SoundProvider {
                id,
                name: str_at(&rs, r, 1).unwrap_or_default(),
                flags: u32_at(r, 2).unwrap_or(0),
                decay_time: f32_at(r, 4).unwrap_or(0.0),
                room: i32_at(7),
                room_hf: i32_at(8),
                decay_hf_ratio: f32_at(r, 9).unwrap_or(1.0),
                reflections: i32_at(10),
                reverb: i32_at(12),
                env_diffusion: f32_at(r, 6).unwrap_or(1.0),
                env_size: f32_at(r, 5).unwrap_or(1.0),
            },
        );
    }
    Ok(SoundProviderCatalog { providers })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real 5875 table: PRESET_GENERIC (67) carries the published EAX SDK values and the
    /// Underwater preset (11) the aggressive HF kill — the two rows the column map hangs on.
    /// Skips without client data.
    #[test]
    fn real_provider_table_decodes() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_sound_provider_catalog(&mut chain).expect("load providers");
        assert_eq!(cat.len(), 38);

        let generic = cat.get(67).expect("PRESET_GENERIC");
        assert_eq!(generic.name, "PRESET_GENERIC");
        assert!((generic.decay_time - 1.49).abs() < 1e-3);
        assert_eq!(generic.room, -1000);
        assert_eq!(generic.room_hf, -100);
        assert_eq!(generic.reflections, -2602);
        assert_eq!(generic.reverb, 200);

        let underwater = cat.get(11).expect("Underwater");
        assert_eq!(underwater.name, "Underwater");
        assert_eq!(underwater.room_hf, -10000);
        assert_eq!(underwater.reverb, 1700);
    }
}
