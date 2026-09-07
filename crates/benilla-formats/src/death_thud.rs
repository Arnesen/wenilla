//! The **death thud** — the body-fall impact a corpse makes as it lands, fired on the `$DTH`
//! animation event (`0x6236e0`, the sibling of the same event's camera shake `0x625c30`).
//!
//! It rides the **same terrain chain the footsteps do** ([`crate::FootstepCatalog`]) — the
//! surface under the unit → `TerrainType` → `TerrainType.SoundID` — and joins it against the
//! creature's **size class** rather than its footstep class:
//!
//! ```text
//! (CreatureDisplayInfo.SizeClass ?? CreatureModelData.SizeClass) × TerrainType.SoundID
//!     → DeathThudLookups → SoundEntries (land | water)
//! ```
//!
//! Layouts — VERIFIED against build 5875 (headers + row decodes, 2026-09-02):
//! - `DeathThudLookups` **45 × 5 × 20 B**: `ID, SizeClass, TerrainTypeSoundID, SoundEntryID,
//!   SoundEntryIDWater`.
//! - `TerrainTypeSounds` **9 × 1 × 4 B**: a bare id enum, `1..=9`, and nothing else — the axis
//!   `TerrainType.SoundID` and `FootstepTerrainLookup.TerrainSoundID` are both keyed on. It is
//!   parsed for its **domain**: the reference bakes this table into a per-size-class array
//!   dimensioned by it and bounds-checks `terrainSoundId >= count` before the lookup
//!   (`0x623771`), and the census wants the empty columns as much as the full ones.
//!
//! **The five size classes are the five audible sizes**, and their kits name themselves:
//! `0 Small · 1 Medium · 2 Large · 3 Giant · 4 Colossal` (`DeathThudSmallDirt` … through
//! `DeathThudColossalWood`, all in `Sound\Effects\DeathImpacts`, one file each). So this is
//! **not** a big-creature-only effect the way the camera shake is — every creature that carries a
//! `$DTH` key thuds, and only the *sample* scales with the body. The water column is coarser: four
//! kits (`DeathThudWaterSmall/Medium/Giant/Colossal`, `…\DeathImpacts\InWater`) shared across the
//! whole terrain axis, since a splash does not care what is under the water.
//!
//! Only five of the nine terrain sounds carry the full class sweep (Dirt 1, Stone 3, Snow 4,
//! Wood 5, Grass 6); the rest (Metallic 2, Leaves 7, Sand 8, Soggy 9) were filled in later against
//! the *same* 25 kits — Metallic borrows Stone, Leaves borrows Dirt — and those rows carry a
//! **water column of 0**, which is silence and not a fallback to land (see [`DeathThudCatalog::kit`]).

use std::collections::{BTreeSet, HashMap};

use crate::Chain;
use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, u32_at};

/// `DeathThudLookups.dbc` joined with the `TerrainTypeSounds.dbc` id domain.
pub struct DeathThudCatalog {
    /// `(SizeClass, TerrainTypeSoundID)` → `(land kit, water kit)`.
    lookup: HashMap<(u32, u32), (u32, u32)>,
    /// Every `TerrainTypeSounds.dbc` id, ascending — the lookup's terrain axis in full, including
    /// the ids no `DeathThudLookups` row names.
    terrain_sounds: BTreeSet<u32>,
}

impl DeathThudCatalog {
    /// The `(land, water)` `SoundEntries` kits for a size class landing on a terrain-sound class.
    /// `None` when the pair has no row — the reference's own answer for an out-of-domain index,
    /// and silence.
    pub fn resolve(&self, size_class: u32, terrain_sound: u32) -> Option<(u32, u32)> {
        self.lookup.get(&(size_class, terrain_sound)).copied()
    }

    /// The one kit that actually plays: the water column when the corpse is in liquid, the land
    /// column otherwise. **A zero is silence, never a fallback to the other column** — the
    /// reference plays whatever dword it read, and `SoundEntries` has no row 0, so the 18 rows
    /// whose water column is 0 are simply mute in water.
    pub fn kit(&self, size_class: u32, terrain_sound: u32, in_water: bool) -> Option<u32> {
        let (land, water) = self.resolve(size_class, terrain_sound)?;
        let kit = if in_water { water } else { land };
        (kit != 0).then_some(kit)
    }

    /// Every `TerrainTypeSounds` id, ascending — the census's column headings.
    pub fn terrain_sounds(&self) -> impl Iterator<Item = u32> + '_ {
        self.terrain_sounds.iter().copied()
    }

    /// Every size class a `DeathThudLookups` row names, ascending — the census's rows. The
    /// shipped data is exactly `0..=4`, which is also why the reference's `sizeClass >= 5` gate
    /// (`0x623744`) never fires on it.
    pub fn size_classes(&self) -> BTreeSet<u32> {
        self.lookup.keys().map(|(sc, _)| *sc).collect()
    }

    pub fn len(&self) -> usize {
        self.lookup.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lookup.is_empty()
    }
}

fn n_u32_schema(name: &str, n: usize) -> Schema {
    let mut s = Schema::new(name);
    for i in 0..n {
        s.add_field(SchemaField::new(format!("f{i}"), FieldType::UInt32));
    }
    s
}

/// Read the two tables off the patch chain.
pub fn load_death_thud_catalog(chain: &mut Chain) -> Result<DeathThudCatalog> {
    let bytes = chain
        .read_file("DBFilesClient\\TerrainTypeSounds.dbc")
        .context("reading TerrainTypeSounds.dbc")?;
    let rs = parse(
        &bytes,
        n_u32_schema("TerrainTypeSounds", 1),
        "TerrainTypeSounds",
    )?;
    let terrain_sounds: BTreeSet<u32> = rs.records().iter().filter_map(|r| u32_at(r, 0)).collect();

    let bytes = chain
        .read_file("DBFilesClient\\DeathThudLookups.dbc")
        .context("reading DeathThudLookups.dbc")?;
    let rs = parse(
        &bytes,
        n_u32_schema("DeathThudLookups", 5),
        "DeathThudLookups",
    )?;
    let mut lookup = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let (Some(size_class), Some(ts)) = (u32_at(r, 1), u32_at(r, 2)) else {
            continue;
        };
        lookup.insert(
            (size_class, ts),
            (u32_at(r, 3).unwrap_or(0), u32_at(r, 4).unwrap_or(0)),
        );
    }

    Ok(DeathThudCatalog {
        lookup,
        terrain_sounds,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped 5875 tables, decoded: the full 45-row join, the 9-id terrain domain, the five
    /// size classes, and the two spot-checks that pin the column order (a swapped SizeClass /
    /// TerrainTypeSoundID pair would still load, and would still be 45 rows).
    #[test]
    fn real_death_thud_chain_resolves() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_death_thud_catalog(&mut chain).expect("load death thud catalog");

        assert_eq!(cat.len(), 45, "all DeathThudLookups rows load");
        assert_eq!(
            cat.terrain_sounds().collect::<Vec<_>>(),
            (1..=9).collect::<Vec<_>>(),
            "TerrainTypeSounds is the bare 1..=9 enum"
        );
        assert_eq!(cat.size_classes(), (0..=4).collect(), "Small..Colossal");

        // Size class 0 (Small) on terrain sound 1 (Dirt) → `DeathThudSmallDirt` 907 /
        // `DeathThudWaterSmall` 1266. Size class 4 (Colossal) on 6 (Grass) →
        // `DeathThudColossalGrass` 928 / `DeathThudWaterColossal` 1269.
        assert_eq!(cat.resolve(0, 1), Some((907, 1266)));
        assert_eq!(cat.resolve(4, 6), Some((928, 1269)));
        assert_eq!(cat.kit(4, 6, false), Some(928));
        assert_eq!(cat.kit(4, 6, true), Some(1269));

        // The later-filled rows: Metallic (2) borrows the Stone kits, and its water column is 0 —
        // silence in water, NOT a fall back to the land kit.
        assert_eq!(
            cat.resolve(0, 2),
            Some((910, 0)),
            "Small Metallic → Stone kit"
        );
        assert_eq!(cat.kit(0, 2, true), None, "and nothing in water");
        assert_eq!(cat.kit(0, 2, false), Some(910));

        // Out of domain both ways: terrain sound 0 is `TerrainType "None"`'s SoundID and is not a
        // TerrainTypeSounds row at all; size class 5 is past the table.
        assert_eq!(cat.resolve(0, 0), None);
        assert_eq!(cat.resolve(5, 1), None);
    }

    /// Every terrain-sound id a `TerrainType` row names is a real `TerrainTypeSounds` row (or the
    /// `0` of `"None"`), and every `DeathThudLookups` terrain axis value is one too — the join the
    /// reference does by array index, checked as data.
    #[test]
    fn the_terrain_axis_agrees_across_the_three_tables() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let thuds = load_death_thud_catalog(&mut chain).expect("load death thud catalog");
        let steps = crate::load_footstep_catalog(&mut chain).expect("load footstep catalog");
        let domain: BTreeSet<u32> = thuds.terrain_sounds().collect();

        for (_, terrain_sound) in thuds.lookup.keys() {
            assert!(
                domain.contains(terrain_sound),
                "DeathThudLookups names terrain sound {terrain_sound}, which TerrainTypeSounds lacks"
            );
        }
        // `TerrainType 10 "None"` is the unauthored default and its SoundID is 0 — not a row, and
        // the reason a building's floor with no material makes no thud.
        for terrain in 0..=10 {
            let sound = steps
                .sound_class_of(terrain)
                .expect("every TerrainType row");
            assert!(
                sound == 0 || domain.contains(&sound),
                "TerrainType {terrain} names terrain sound {sound}"
            );
        }
        assert_eq!(steps.sound_class_of(10), Some(0), r#"TerrainType "None""#);
    }
}
