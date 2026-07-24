//! Footsteps: the terrain-type chain (decision 0070 slice 3) —
//! `ground texture → GroundEffectTexture.TerrainType → TerrainType.SoundID ×
//! CreatureSoundData.FootstepID → FootstepTerrainLookup → SoundEntries (dry | splash)`.
//!
//! Layouts — VERIFIED against build 5875 (headers + row decodes, 2026-07-02):
//! - `TerrainType` **11 × 6 × 24 B**: `ID, Desc(str), FootstepSprayRun, FootstepSprayWalk,
//!   SoundID, Flags` — the full domain decoded: Dirt→1, Metallic→2, Stone→3, Snow→4, Wood→5,
//!   Grass→6, Leaves→7, Sand→8, Soggy→9, DustyGrass→6, None→0.
//! - `FootstepTerrainLookup` **179 × 5 × 20 B**: `ID, CreatureFootstepID, TerrainSoundID,
//!   SoundID(dry), SoundIDSplash`. Spot-checks: class 8 × terrain-sound 2 (Metallic) →
//!   650/1063; class 8 × 3 (Stone) → 653/1057.
//! - `GroundEffectTexture` field 6 is the `TerrainType` FK (the clutter catalog reads the same
//!   table for doodads and rightly skips doodad-less rows; footsteps need every row, so this
//!   module re-reads it into its own map).
//!
//! Class semantics (data-verified 2026-07-02; class-0 gate byte-confirmed at `0x6233ec`,
//! wow-re `benilla-pins.md` B11a): **class 7 is the humanoid/character class** — its ten rows
//! are exactly the `CharacterMediumLarge*` kits; characters reach it through the ordinary
//! display→sound data chain (`creature_sound`, the model fallback), never a code default.
//! **Class 0 means "no footstep sounds"** — the client bails before any lookup; the lookup's
//! class-0 rows are the Ancient Protector's stomps (kit 661), reached only by a *nonzero* class
//! on its own row. A position with **no ground-effect layer is silent** — the client's sentinel
//! is −1 and the kit lookup's signed bounds check rejects it (`0x458450`, B5-verified; the
//! audible fingerprint is vanilla's famously quiet dirt roads).

use std::collections::HashMap;

use crate::Chain;
use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, u32_at};

/// The joined footstep tables, resolvable from `(footstep class, ground effect id)`.
pub struct FootstepCatalog {
    /// `GroundEffectTexture` id → `TerrainType` id (every row, including doodad-less ones).
    effect_terrain: HashMap<u32, u32>,
    /// `TerrainType` id → its `SoundID` class (the lookup's `TerrainSoundID` axis).
    terrain_sound: HashMap<u32, u32>,
    /// `(CreatureFootstepID, TerrainSoundID)` → `(dry kit, splash kit)`.
    lookup: HashMap<(u32, u32), (u32, u32)>,
}

impl FootstepCatalog {
    /// The `(dry, splash)` SoundEntries kits for a creature footstep class standing on the given
    /// ground-effect layer. `None` = silence: no/unknown effect layer (the client's −1 sentinel,
    /// module docs), or no lookup row for `(class, terrain)`.
    pub fn resolve(&self, footstep_class: u32, effect_id: Option<u32>) -> Option<(u32, u32)> {
        let terrain = self.effect_terrain.get(&effect_id?).copied()?;
        let sound_class = self.terrain_sound.get(&terrain).copied()?;
        self.lookup.get(&(footstep_class, sound_class)).copied()
    }

    pub fn len(&self) -> usize {
        self.lookup.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lookup.is_empty()
    }
}

fn n_u32_schema(name: &str, n: usize, string_fields: &[usize]) -> Schema {
    let mut s = Schema::new(name);
    for i in 0..n {
        let ty = if string_fields.contains(&i) {
            FieldType::String
        } else {
            FieldType::UInt32
        };
        s.add_field(SchemaField::new(format!("f{i}"), ty));
    }
    s
}

/// Read the three tables off the patch chain.
pub fn load_footstep_catalog(chain: &mut Chain) -> Result<FootstepCatalog> {
    let bytes = chain
        .read_file("DBFilesClient\\GroundEffectTexture.dbc")
        .context("reading GroundEffectTexture.dbc")?;
    let rs = parse(
        &bytes,
        n_u32_schema("GroundEffectTexture", 7, &[]),
        "GroundEffectTexture",
    )?;
    let mut effect_terrain = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        if let (Some(id), Some(tt)) = (u32_at(r, 0), u32_at(r, 6)) {
            effect_terrain.insert(id, tt);
        }
    }

    let bytes = chain
        .read_file("DBFilesClient\\TerrainType.dbc")
        .context("reading TerrainType.dbc")?;
    let rs = parse(&bytes, n_u32_schema("TerrainType", 6, &[1]), "TerrainType")?;
    let mut terrain_sound = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        if let (Some(id), Some(class)) = (u32_at(r, 0), u32_at(r, 4)) {
            terrain_sound.insert(id, class);
        }
    }

    let bytes = chain
        .read_file("DBFilesClient\\FootstepTerrainLookup.dbc")
        .context("reading FootstepTerrainLookup.dbc")?;
    let rs = parse(
        &bytes,
        n_u32_schema("FootstepTerrainLookup", 5, &[]),
        "FootstepTerrainLookup",
    )?;
    let mut lookup = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let (Some(class), Some(ts)) = (u32_at(r, 1), u32_at(r, 2)) else {
            continue;
        };
        lookup.insert(
            (class, ts),
            (u32_at(r, 3).unwrap_or(0), u32_at(r, 4).unwrap_or(0)),
        );
    }

    Ok(FootstepCatalog {
        effect_terrain,
        terrain_sound,
        lookup,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vanilla_data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
    }

    /// The full chain resolves on real 5875 data: a Metallic-terrain effect under footstep
    /// class 8 yields the byte-decoded kits (650 dry / 1063 splash); the humanoid class 7
    /// resolves the `CharacterMediumLarge*` kits (560 Dirt with no effect, 562 Grass on a
    /// grass-terrain effect — the character-in-Elwynn case); an unknown class stays silent.
    #[test]
    fn real_footstep_chain_resolves() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_footstep_catalog(&mut chain).expect("load footstep catalog");
        assert_eq!(cat.len(), 179, "all lookup rows load");

        // Some effect whose terrain is Metallic (TerrainType 1 → sound class 2).
        let metallic = cat
            .effect_terrain
            .iter()
            .find(|(_, &tt)| tt == 1)
            .map(|(&e, _)| e);
        if let Some(e) = metallic {
            assert_eq!(
                cat.resolve(8, Some(e)),
                Some((650, 1063)),
                "class 8 on metallic → the byte-decoded row"
            );
        }
        // The humanoid class (7): the dirt kit on a dirt-terrain effect, the grass kit on a
        // grass one (class 0 is the Ancient Protector, module docs).
        let effect_with_sound_class = |sc: u32| {
            cat.effect_terrain
                .iter()
                .find(|(_, tt)| cat.terrain_sound.get(tt) == Some(&sc))
                .map(|(&e, _)| e)
        };
        if let Some(e) = effect_with_sound_class(1) {
            assert_eq!(
                cat.resolve(7, Some(e)).map(|(dry, _)| dry),
                Some(560),
                "class 7 on dirt → CharacterMediumLargeDirt"
            );
        }
        if let Some(e) = effect_with_sound_class(6) {
            assert_eq!(
                cat.resolve(7, Some(e)).map(|(dry, _)| dry),
                Some(562),
                "class 7 on grass → CharacterMediumLargeGrass"
            );
        }
        // No ground-effect layer = silence (the −1 sentinel, B5); unknown class too.
        assert_eq!(cat.resolve(7, None), None);
        assert_eq!(cat.resolve(9999, None), None);
    }
}
