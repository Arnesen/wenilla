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
    /// The `Flags` column — the *other* half of what this three-column table is for. It decides
    /// the metal/wood split of every weapon impact ([`MaterialCatalog::is_metal`]) and, on a
    /// player's chest, which armor slot the victim presents
    /// ([`MaterialCatalog::armor_impact_slot`]).
    flags: HashMap<u32, u32>,
}

impl MaterialCatalog {
    /// The foley kit for a material id, or `None` when the material has no foley (five of the
    /// eight shipped rows) or the id names no row. The reference's own two misses — a negative
    /// id and an id past the table's max — land in the same place (`0x4584e6`/`0x4584ea`), so
    /// callers need no sentinel of their own.
    pub fn foley_kit(&self, material: u32) -> Option<u32> {
        self.foley.get(&material).copied().filter(|&k| k != 0)
    }

    /// Is this material **metal-bodied** — `Flags & 0x1`, the reference's `0x5d9a50`.
    ///
    /// It is the only thing that picks the metal vs wood half of `WeaponImpactSounds` (both the
    /// weapon's own impact row and the victim's parry slot: `0x457e80` computes it, `0x457dc0`
    /// asks it twice). The flag is set on metal (1), jewelry (4), chain (5) and plate (6) — so
    /// it is genuinely "is this thing metal", not "is this thing not wood": leather, cloth and
    /// liquid all read as non-metal, and so does an **unknown or absent material**, which is the
    /// reference's answer for id 0 (no row) and the reason a guess of `material != WOOD` gets a
    /// materialless creature weapon wrong.
    pub fn is_metal(&self, material: u32) -> bool {
        self.flags.get(&material).is_some_and(|f| f & 0x1 != 0)
    }

    /// The `WeaponImpactSounds` **target slot** a body wearing this material presents —
    /// `0x62fb70`, the CGPlayer override of the victim's impact type, read off the same `Flags`
    /// column: bit 1 → 2 (plate), else bit 2 → 1 (chain), else 0 (flesh).
    ///
    /// On the shipped table that is exactly plate → plate and chain → chain, with **leather and
    /// cloth landing on flesh** — armor that does not ring. Creatures do not come through here:
    /// their slot is a `CreatureSoundData` column remapped through `{0, 8, 7, 9}` (`0x6238f0`),
    /// i.e. flesh/stone/wood/ethereal, a disjoint half of the same ten.
    pub fn armor_impact_slot(&self, material: u32) -> u32 {
        let Some(&flags) = self.flags.get(&material) else {
            return 0;
        };
        if flags & 0x2 != 0 {
            2
        } else {
            (flags & 0x4) >> 2
        }
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
    let mut flags = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        if let Some(id) = u32_at(r, 0) {
            foley.insert(id, u32_at(r, 2).unwrap_or(0));
            flags.insert(id, u32_at(r, 1).unwrap_or(0));
        }
    }
    Ok(MaterialCatalog { foley, flags })
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

    /// The `Flags` column's two consumers, on the shipped table. **`is_metal` is not
    /// `!= wood`**: leather and cloth are non-metal too, and so is a missing material — the case
    /// a creature with no virtual-item info actually hits.
    #[test]
    fn the_flags_column_splits_metal_and_names_the_armor_slot() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_material_catalog(&mut chain).expect("load materials");

        for metal in [1, 4, 5, 6] {
            assert!(cat.is_metal(metal), "material {metal} is metal-bodied");
        }
        for soft in [2, 3, 7, 8] {
            assert!(!cat.is_metal(soft), "material {soft} is not metal");
        }
        assert!(!cat.is_metal(0), "an absent material is not metal");
        assert!(!cat.is_metal(99), "an unknown material is not metal");

        // The armor slot a chest presents: plate rings as plate, chain as chain, everything
        // else — leather and cloth included — as flesh.
        assert_eq!(cat.armor_impact_slot(6), 2, "plate");
        assert_eq!(cat.armor_impact_slot(5), 1, "chain");
        for flesh in [0, 1, 2, 3, 4, 7, 8, 99] {
            assert_eq!(cat.armor_impact_slot(flesh), 0, "material {flesh}");
        }
    }
}
