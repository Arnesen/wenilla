//! ItemClass.dbc — what an item's **class** is *called*: "Weapon", "Armor", "Container",
//! "Trade Goods", "Quiver".
//!
//! One consumer, and it is what makes the table worth loading: `GetItemInfo`'s fifth return
//! (`itemType`). **Byte-verified** at the registered binding `0x48e070`, which reads the cache
//! record's class (`[record+0]`), bounds-checks it against the row count at `ds:0xc0dc28`,
//! indexes the row array at `ds:0xc0dc24`, and pushes `[row + 4*[0xc0e080] + 0xc]` — field 3 at
//! the active locale, i.e. `ClassName_Lang`. An out-of-range class, or a null row, pushes the
//! empty string at `0x882748` instead (`0x48e236`).
//!
//! Record layout (16 rows in the shipped 5875 file, verified by loading it): `ClassID@0`,
//! `SubClassMapID@1`, `Flags@2`, `ClassName_Lang@3..10`, `ClassNameFlags@11`. Ids are 0..=15 and
//! contiguous, but this is keyed rather than indexed for the same reason every other catalog here
//! is: a `class` value off the wire is data, not an index we control.
//!
//! Five of the sixteen names carry an `(OBSOLETE)` suffix in the shipped data (`Jewelry`,
//! `Generic`, `Money`, `Permanent`, plus `Quiver`'s first two subclasses elsewhere). They are
//! transcribed as-is: the reference pushes the row's string unfiltered, and an addon comparing
//! against what the live client returns must see the same bytes.

use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, str_at, u32_at};
use crate::Chain;

const ITEM_CLASS: &str = "DBFilesClient\\ItemClass.dbc";

/// ItemClass.dbc keyed by the `class` an item template carries.
pub struct ItemClassCatalog {
    names: HashMap<u32, String>,
}

impl ItemClassCatalog {
    /// What class `id` is *called* — `GetItemInfo`'s `itemType`. `None` for a class with no row,
    /// which the reference renders as the empty string (see the module header).
    pub fn name(&self, id: u32) -> Option<&str> {
        self.names.get(&id).map(String::as_str)
    }

    /// Row count, for the load log.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

fn item_class_schema() -> Schema {
    let mut s = Schema::new("ItemClass");
    s.add_field(SchemaField::new("ClassID", FieldType::UInt32));
    s.add_field(SchemaField::new("SubClassMapID", FieldType::UInt32));
    s.add_field(SchemaField::new("Flags", FieldType::UInt32));
    for i in 0..8 {
        s.add_field(SchemaField::new(format!("Name{i}"), FieldType::String));
    }
    s.add_field(SchemaField::new("NameFlags", FieldType::UInt32));
    s
}

/// Load ItemClass.dbc from the patch chain.
pub fn load_item_classes(chain: &mut Chain) -> Result<ItemClassCatalog> {
    let bytes = chain
        .read_file(ITEM_CLASS)
        .with_context(|| format!("reading {ITEM_CLASS}"))?;
    let rs = parse(&bytes, item_class_schema(), "ItemClass")?;
    let mut names = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let (Some(id), Some(name)) = (u32_at(r, 0), str_at(&rs, r, 3).filter(|n| !n.is_empty()))
        else {
            continue;
        };
        names.insert(id, name);
    }
    Ok(ItemClassCatalog { names })
}

#[cfg(test)]
mod tests {
    use super::load_item_classes;

    /// The shipped 5875 table, read as data. The four names pinned here are the ones the corpus
    /// asserts against *by hand*: `Bagnon_Core/localization.lua:94-95` ships
    /// `BAGNON_ITEMTYPE_CONTAINER = "Container"` / `BAGNON_ITEMTYPE_QUIVER = "Quiver"` as the
    /// values it expects `GetItemInfo`'s fifth return to have, and re-derives them at runtime from
    /// items 4500 (a backpack) and 8218 (a quiver) — so these strings are the live client's own
    /// answer, checked by an addon author against a real 1.12 realm.
    ///
    /// The `(OBSOLETE)` suffixes are pinned too: they are in the shipped bytes and the reference
    /// does not strip them. Skips without client data.
    #[test]
    fn the_shipped_classes_are_named_as_getiteminfo_returns_them() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_item_classes(&mut chain).expect("ItemClass.dbc");

        assert_eq!(cat.name(0), Some("Consumable"));
        assert_eq!(cat.name(1), Some("Container"), "Bagnon's own expectation");
        assert_eq!(cat.name(2), Some("Weapon"));
        assert_eq!(cat.name(4), Some("Armor"));
        assert_eq!(cat.name(7), Some("Trade Goods"));
        assert_eq!(cat.name(11), Some("Quiver"), "Bagnon's own expectation");
        assert_eq!(cat.name(15), Some("Miscellaneous"));
        // Transcribed, not tidied — the reference pushes the row string unfiltered.
        assert_eq!(cat.name(3), Some("Jewelry(OBSOLETE)"));
        assert_eq!(cat.name(10), Some("Money(OBSOLETE)"));
        // Past the table: no row, which the binding renders as "" (`0x48e236`).
        assert_eq!(cat.name(16), None);
        assert_eq!(cat.len(), 16, "the whole shipped table");
    }
}
