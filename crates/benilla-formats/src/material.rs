//! `Material.dbc` — the **armor foley**: the rustle a body makes as it moves, separate from the
//! terrain footstep it lands on.
//!
//! A footfall is two sounds in the reference, not one. `$FSD` runs `0x623390`, whose *first* act
//! after the three state gates is `call [vt+0x8c]` — the foley (`0x623610` for a unit,
//! `0x62fa30` for a player) — and only then the terrain chain, which has gates of its own. So a
//! creature whose footstep class is 0 still rustles, and the two sounds are on different buses
//! (foley on the uncapped bus 0, the step on bus 9's cap of 6).
//!
//! Both foley paths converge on `0x4584e0`, which is this table: a `Material` **id** → the row's
//! `+0x8` field → a `SoundEntries` kit, played positionally at the unit's feet **+2.0 yd**
//! (`0x45851d fadd [0x801628]`) at volume 1.0.
//!
//! Layout — VERIFIED against the shipped build 5875 file (8 records × 3 fields × 12 B):
//! `ID(0), Flags(1), FoleySoundID(2)`. The `+0x8` the binary reads is field 2 on a 12-byte
//! record, and the `[row+4] & 1` flag test at `0x5d9a68` lands on field 1 — two independent
//! offsets agreeing on the same three-column shape.
//!
//! **Only three of the eight materials make any sound**: chain (5) → 1005 `FoleySoundChain`,
//! plate (6) → 1004 `FoleySoundPlate`, leather (8) → 1003 `FoleySoundLeather`. Metal, wood,
//! liquid, jewelry and **cloth** carry 0 — a robed mage is silent, and that is data, not a gap.

use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, u32_at};
use crate::Chain;

/// `Material.dbc` keyed by id.
pub struct MaterialCatalog {
    foley: HashMap<u32, u32>,
}

impl MaterialCatalog {
    /// The foley kit for a material id, or `None` when the material has no foley (five of the
    /// eight shipped rows) or the id names no row. The reference's own two misses — a negative
    /// id and an id past the table's max — land in the same place (`0x4584e6`/`0x4584ea`), so
    /// callers need no sentinel of their own.
    pub fn foley_kit(&self, material: u32) -> Option<u32> {
        self.foley.get(&material).copied().filter(|&k| k != 0)
    }

    pub fn len(&self) -> usize {
        self.foley.len()
    }

    pub fn is_empty(&self) -> bool {
        self.foley.is_empty()
    }
}

fn schema() -> Schema {
    let mut s = Schema::new("Material");
    for name in ["ID", "Flags", "FoleySoundID"] {
        s.add_field(SchemaField::new(name, FieldType::UInt32));
    }
    s
}

/// Read `Material.dbc` off the patch chain.
pub fn load_material_catalog(chain: &mut Chain) -> Result<MaterialCatalog> {
    let bytes = chain
        .read_file("DBFilesClient\\Material.dbc")
        .context("reading Material.dbc")?;
    let rs = parse(&bytes, schema(), "Material")?;
    let mut foley = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        if let Some(id) = u32_at(r, 0) {
            foley.insert(id, u32_at(r, 2).unwrap_or(0));
        }
    }
    Ok(MaterialCatalog { foley })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped table, whose material ids are the same ones
    /// `SMSG_ITEM_QUERY_SINGLE_RESPONSE` puts on the wire (1 metal · 2 wood · 5 chain · 6 plate ·
    /// 7 cloth · 8 leather). Skips without client data.
    #[test]
    fn real_materials_decode() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_material_catalog(&mut chain).expect("load materials");
        assert_eq!(cat.len(), 8);

        assert_eq!(cat.foley_kit(5), Some(1005), "chain");
        assert_eq!(cat.foley_kit(6), Some(1004), "plate");
        assert_eq!(cat.foley_kit(8), Some(1003), "leather");

        // Cloth is the load-bearing silence: a robe must not borrow leather's rustle.
        assert_eq!(cat.foley_kit(7), None, "cloth");
        for quiet in [1, 2, 3, 4] {
            assert_eq!(cat.foley_kit(quiet), None, "material {quiet}");
        }

        // Off the end of the table, and the "no material" id the wire sends for an empty slot.
        assert_eq!(cat.foley_kit(0), None);
        assert_eq!(cat.foley_kit(9), None);
    }
}
