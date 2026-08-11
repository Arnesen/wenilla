//! `SpellRange.dbc` — the `GetMinMaxRange 0x6e3480` inputs a spell's `rangeIndex` column
//! ([`crate::spells::SpellDisplay::range_index`]) resolves against: a min/max yard pair per row,
//! plus a melee-family flag whose branch substitutes the combat-reach sum for the authored pair.

use std::collections::HashMap;

use crate::Chain;
use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{f32_at, parse, u32_at};

/// One `SpellRange.dbc` row — the `GetMinMaxRange 0x6e3480` inputs (wow-re `wave-cooldown.md`,
/// VERIFIED: min f32 `+0x4`, max f32 `+0x8`, flags `+0xc` with **bit 0 = melee**, whose branch
/// substitutes the combat-reach sum floored at 5.0 for the authored pair). Pinned on the extracted
/// 5875 file (28 records × 22 fields): row 2 = {0, 5, flags 1} (melee), 114 = {8, 35} (Auto Shot),
/// 95 = {8, 25} (Charge), 35 = {0, 35} (Fireball).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpellRange {
    pub min: f32,
    pub max: f32,
    /// Bit 0: the melee range family (combat-reach based, not the authored min/max).
    pub flags: u32,
}

impl SpellRange {
    /// The melee-range family (flags bit 0) — `GetMinMaxRange`'s reach-sum branch.
    pub fn is_melee(&self) -> bool {
        self.flags & 1 != 0
    }
}

/// `SpellRange.dbc`, by row id ([`SpellDisplay::range_index`]).
#[derive(Default)]
pub struct SpellRangeCatalog {
    ranges: HashMap<u32, SpellRange>,
}

impl SpellRangeCatalog {
    pub fn get(&self, index: u32) -> Option<&SpellRange> {
        self.ranges.get(&index)
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }
}

const SPELL_RANGE: &str = "DBFilesClient\\SpellRange.dbc";
const SPELL_RANGE_FIELDS: usize = 22;

/// Load `SpellRange.dbc` off the patch chain ([`SpellRange`]'s row law).
pub fn load_spell_ranges(chain: &mut Chain) -> Result<SpellRangeCatalog> {
    let bytes = chain
        .read_file(SPELL_RANGE)
        .context("reading SpellRange.dbc")?;
    let mut schema = Schema::new("SpellRange");
    for i in 0..SPELL_RANGE_FIELDS {
        match i {
            1 => schema.add_field(SchemaField::new("MinRange", FieldType::Float32)),
            2 => schema.add_field(SchemaField::new("MaxRange", FieldType::Float32)),
            _ => schema.add_field(SchemaField::new(format!("F{i}"), FieldType::UInt32)),
        }
    }
    let set = parse(&bytes, schema, "SpellRange.dbc")?;
    let mut ranges = HashMap::new();
    for r in set.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        ranges.insert(
            id,
            SpellRange {
                min: f32_at(r, 1).unwrap_or(0.0),
                max: f32_at(r, 2).unwrap_or(0.0),
                flags: u32_at(r, 3).unwrap_or(0),
            },
        );
    }
    Ok(SpellRangeCatalog { ranges })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SpellRange.dbc` on the real data — the byte law's own probe rows (`GetMinMaxRange
    /// 0x6e3480`): row 2 is the melee family (flags bit 0), 114 = Auto Shot's 8–35, 95 =
    /// Charge's 8–25. Skips without client data.
    #[test]
    fn real_spell_ranges_read_the_byte_laws_rows() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let ranges = load_spell_ranges(&mut chain).expect("load SpellRange");

        let melee = ranges.get(2).expect("row 2");
        assert_eq!((melee.min, melee.max), (0.0, 5.0));
        assert!(melee.is_melee(), "row 2 carries the melee flag");

        let auto_shot = ranges.get(114).expect("row 114");
        assert_eq!((auto_shot.min, auto_shot.max), (8.0, 35.0));
        assert!(!auto_shot.is_melee());

        let charge = ranges.get(95).expect("row 95");
        assert_eq!((charge.min, charge.max), (8.0, 25.0));

        // A min-0 nuke row reads a true 0.0 min (row 4: Shadow Bolt, Frostbolt, wand Shoot) —
        // the fcomp-vs-0.0 guard's input, so the min/max field mapping can't silently shift
        // (0426: a manufactured min range refused point-blank casts).
        let nuke = ranges.get(4).expect("row 4");
        assert_eq!((nuke.min, nuke.max), (0.0, 30.0));
        assert!(!nuke.is_melee());
    }
}
