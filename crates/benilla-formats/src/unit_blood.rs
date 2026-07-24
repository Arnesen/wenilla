//! **UnitBlood.dbc / UnitBloodLevels.dbc** — the melee blood-spurt tables (decision 0137 phase 3).
//!
//! The byte-verified chain (wow-re `melee-blood-spurt.md`, `0x624530 → 0x625010`): a melee hit with
//! `HitInfo & 0x2`, nonzero damage, and victimState ∈ {1,4} resolves the victim's blood id
//! (`CreatureDisplayInfo.BloodLevel` override, else `CreatureModelData.BloodID`; ≤ 0 = bloodless) →
//! a **UnitBloodLevels** row, whose three columns are the bloodID per `violenceLevel` (0–2) — the
//! vanilla censorship table: a red-blooded creature at violence 1 bleeds *green*, at 0 nothing —
//! → a **UnitBlood** row whose first four fields are `SpellVisualEffectName` ids. Column order
//! pinned empirically from the effect names (`Combat Blood Spurt <Front|Back> <Small|Large> <color>`,
//! 12/12 rows consistent): **[FrontSmall, FrontLarge, BackSmall, BackLarge]** — front/back from
//! `sign(victimForward · (attackerPos − victimPos))`, Large on the *crushing* bit `HitInfo & 0x2000`
//! (not crit — crit `0x80` belongs to the wound flinch). Fields 5–9 are the ground-splat decal
//! textures (`textures\BloodSplats\…`) — a separate mechanism, not read here (no consumer yet).

use std::collections::HashMap;

use crate::Chain;
use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, u32_at};

const UNIT_BLOOD: &str = "DBFilesClient\\UnitBlood.dbc";
const UNIT_BLOOD_LEVELS: &str = "DBFilesClient\\UnitBloodLevels.dbc";

/// The two blood tables, resolving `(blood id, violence level, front, crushing)` → the spurt's
/// `SpellVisualEffectName` id (whose path the spell-visual catalog resolves like any kit effect).
pub struct BloodCatalog {
    /// UnitBloodLevels: id → the bloodID per violence level 0/1/2 (`0` = no blood at that level).
    levels: HashMap<u32, [u32; 3]>,
    /// UnitBlood: id → `[FrontSmall, FrontLarge, BackSmall, BackLarge]` effect ids.
    rows: HashMap<u32, [u32; 4]>,
}

impl BloodCatalog {
    /// The spurt effect for a victim: `blood_id` is the creature's resolved UnitBloodLevels key
    /// (≤ 0 = a bloodless model — `None`), `violence` the gore level (0–2, clamped), `front`/`large`
    /// the hit geometry/weight. `None` = no spurt (bloodless, censored at this level, or absent row).
    pub fn effect_id(
        &self,
        blood_id: i32,
        violence: usize,
        front: bool,
        large: bool,
    ) -> Option<u32> {
        if blood_id <= 0 {
            return None;
        }
        let level = self.levels.get(&(blood_id as u32))?[violence.min(2)];
        if level == 0 {
            return None;
        }
        let row = self.rows.get(&level)?;
        let idx = match (front, large) {
            (true, false) => 0,
            (true, true) => 1,
            (false, false) => 2,
            (false, true) => 3,
        };
        Some(row[idx]).filter(|&id| id != 0)
    }

    /// Row counts (levels, rows) for logging/diagnostics.
    pub fn len(&self) -> (usize, usize) {
        (self.levels.len(), self.rows.len())
    }
}

/// UnitBloodLevels.dbc — 4 fields in build 5875: ID + the three per-violence-level bloodIDs.
fn unit_blood_levels_schema() -> Schema {
    let mut s = Schema::new("UnitBloodLevels");
    for name in ["ID", "Violence0", "Violence1", "Violence2"] {
        s.add_field(SchemaField::new(name, FieldType::UInt32));
    }
    s
}

/// UnitBlood.dbc — 10 fields in build 5875: ID, the four spurt `SpellVisualEffectName` ids, and
/// five ground-splat texture strings (unread — see the module doc).
fn unit_blood_schema() -> Schema {
    let mut s = Schema::new("UnitBlood");
    for name in ["ID", "FrontSmall", "FrontLarge", "BackSmall", "BackLarge"] {
        s.add_field(SchemaField::new(name, FieldType::UInt32));
    }
    for i in 0..5 {
        s.add_field(SchemaField::new(
            format!("GroundSplat{i}"),
            FieldType::String,
        ));
    }
    s
}

/// Load both blood tables from the patch chain into a [`BloodCatalog`].
pub fn load_blood_catalog(chain: &mut Chain) -> Result<BloodCatalog> {
    let levels = {
        let bytes = chain
            .read_file(UNIT_BLOOD_LEVELS)
            .with_context(|| format!("reading {UNIT_BLOOD_LEVELS}"))?;
        let rs = parse(&bytes, unit_blood_levels_schema(), "UnitBloodLevels")?;
        let mut m = HashMap::with_capacity(rs.records().len());
        for r in rs.records() {
            if let Some(id) = u32_at(r, 0) {
                m.insert(
                    id,
                    [
                        u32_at(r, 1).unwrap_or(0),
                        u32_at(r, 2).unwrap_or(0),
                        u32_at(r, 3).unwrap_or(0),
                    ],
                );
            }
        }
        m
    };
    let rows = {
        let bytes = chain
            .read_file(UNIT_BLOOD)
            .with_context(|| format!("reading {UNIT_BLOOD}"))?;
        let rs = parse(&bytes, unit_blood_schema(), "UnitBlood")?;
        let mut m = HashMap::with_capacity(rs.records().len());
        for r in rs.records() {
            if let Some(id) = u32_at(r, 0) {
                m.insert(
                    id,
                    [
                        u32_at(r, 1).unwrap_or(0),
                        u32_at(r, 2).unwrap_or(0),
                        u32_at(r, 3).unwrap_or(0),
                        u32_at(r, 4).unwrap_or(0),
                    ],
                );
            }
        }
        m
    };
    Ok(BloodCatalog { levels, rows })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repo root's `WoW/Data` (gitignored; the real-data tests skip when absent).
    fn vanilla_data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
    }

    /// The real 5875 tables, end to end: a red-blooded creature (blood 1 — HumanMale's) at max
    /// violence spurts RED (effect 109 front-small = `Particles\BloodSpurts\BloodSpurt.mdl`,
    /// 55 back-large); at violence 1 the same creature is CENSORED to green (row 2's ids); at 0
    /// nothing. Pins the empirically-derived column order against the shipped data.
    #[test]
    fn real_blood_tables_resolve_the_censorship_chain() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let blood = load_blood_catalog(&mut chain).expect("blood tables");

        // Max violence: true colors, all four quadrants of row 1 (red).
        assert_eq!(
            blood.effect_id(1, 2, true, false),
            Some(109),
            "front small red"
        );
        assert_eq!(
            blood.effect_id(1, 2, true, true),
            Some(164),
            "front large red"
        );
        assert_eq!(
            blood.effect_id(1, 2, false, false),
            Some(534),
            "back small red"
        );
        assert_eq!(
            blood.effect_id(1, 2, false, true),
            Some(55),
            "back large red"
        );
        // Violence 1: red is censored to green (UnitBloodLevels row 1 → bloodID 2).
        assert_eq!(
            blood.effect_id(1, 1, true, false),
            Some(183),
            "censored green"
        );
        // Violence 0: no blood at all; bloodless models (≤0) never spurt.
        assert_eq!(blood.effect_id(1, 0, true, false), None);
        assert_eq!(blood.effect_id(-1, 2, true, false), None);
        assert_eq!(blood.effect_id(0, 2, true, false), None);
    }
}
