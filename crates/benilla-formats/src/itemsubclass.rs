//! ItemSubClass.dbc — per `(class, subclass)`: the alternate-proficiency fields and the
//! display gate the item tooltip's slot|type line reads (wow-re builder `0x52b650`, the
//! `0xc0db90` row cache).
//!
//! The builder consumes exactly three fields beyond the key: **prerequisiteProficiency@2 /
//! postrequisiteProficiency@3** (−1 = none; a weapon whose own subclass bit is missing from the
//! player's proficiency mask is still usable when the alternate's bit is set — the slot cell's
//! red instead of the type cell's), and **displayFlags@5** bit 0 (suppress the type name — the
//! "Miscellaneous" family: rings, trinkets, shirts never print an armor type).
//!
//! Record layout (no id column; keyed by class+subclass, 28 fields): class@0, subClass@1,
//! prerequisiteProficiency@2, postrequisiteProficiency@3, flags@4, displayFlags@5,
//! weaponParrySeq@6, weaponReadySeq@7, weaponAttackSeq@8, weaponSwingSize@9, displayName
//! 8+1 @10..18, verboseName 8+1 @19..27 — the offsets the builder reads (`[row+2]`, `[row+3]`,
//! byte of `[row+5]`, `[row+locale+10]`) land on exactly this shape.

use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{i32_at, parse, u32_at};
use crate::Chain;

const ITEM_SUB_CLASS: &str = "DBFilesClient\\ItemSubClass.dbc";

/// One row's tooltip-relevant fields.
#[derive(Debug, Clone, Copy)]
pub struct ItemSubClassInfo {
    /// Alternate subclasses whose proficiency also permits use (−1 = none). The builder's
    /// short-circuit: prerequisite wins when present; postrequisite is only consulted when
    /// prerequisite is −1.
    pub prerequisite_proficiency: i32,
    pub postrequisite_proficiency: i32,
    /// Bit 0 = never print the type name on the slot|type line.
    pub display_flags: u32,
}

/// ItemSubClass.dbc keyed by `(class, subclass)`.
pub struct ItemSubClassCatalog {
    rows: HashMap<(u32, u32), ItemSubClassInfo>,
    /// The crafting book's header vocabulary (0437's TU-B fold-back): the resolved display name,
    /// by the client's own byte law — **VerboseName** (`row + locale·4 + 0x4c`, enUS column 19)
    /// when non-empty, else **DisplayName** (`+0x28`, column 10). "One-Handed Swords" over
    /// "Sword"; plain "Cloth" where no verbose form exists.
    names: HashMap<(u32, u32), String>,
}

impl ItemSubClassCatalog {
    /// The alternate proficiency subclass for `(class, subclass)` — the builder's exact
    /// sentinel walk: prerequisite if not −1, else postrequisite if not −1, else `None`.
    pub fn proficiency_alt(&self, class: u32, subclass: u32) -> Option<u32> {
        let r = self.rows.get(&(class, subclass))?;
        [r.prerequisite_proficiency, r.postrequisite_proficiency]
            .into_iter()
            .find(|&v| v != -1)
            .map(|v| v as u32)
    }

    /// The subclass display name (verbose-first, the wow-re `tradeskill` node's byte law) — the
    /// crafting book's group header text; `None` for an unknown key.
    pub fn name(&self, class: u32, subclass: u32) -> Option<&str> {
        self.names.get(&(class, subclass)).map(String::as_str)
    }

    /// Whether the type name is suppressed (displayFlags bit 0).
    pub fn hides_name(&self, class: u32, subclass: u32) -> bool {
        self.rows
            .get(&(class, subclass))
            .is_some_and(|r| r.display_flags & 1 != 0)
    }

    /// Number of rows (for logging/diagnostics).
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether no rows loaded.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

fn item_sub_class_schema() -> Schema {
    let mut s = Schema::new("ItemSubClass");
    s.add_field(SchemaField::new("Class", FieldType::UInt32));
    s.add_field(SchemaField::new("SubClass", FieldType::UInt32));
    s.add_field(SchemaField::new(
        "PrerequisiteProficiency",
        FieldType::Int32,
    ));
    s.add_field(SchemaField::new(
        "PostrequisiteProficiency",
        FieldType::Int32,
    ));
    s.add_field(SchemaField::new("Flags", FieldType::UInt32));
    s.add_field(SchemaField::new("DisplayFlags", FieldType::UInt32));
    for name in ["ParrySeq", "ReadySeq", "AttackSeq", "SwingSize"] {
        s.add_field(SchemaField::new(format!("Weapon{name}"), FieldType::UInt32));
    }
    for i in 0..8 {
        s.add_field(SchemaField::new(
            format!("DisplayName{i}"),
            FieldType::String,
        ));
    }
    s.add_field(SchemaField::new("DisplayNameFlags", FieldType::UInt32));
    for i in 0..8 {
        s.add_field(SchemaField::new(
            format!("VerboseName{i}"),
            FieldType::String,
        ));
    }
    s.add_field(SchemaField::new("VerboseNameFlags", FieldType::UInt32));
    s
}

/// Load ItemSubClass.dbc from the patch chain.
pub fn load_item_sub_classes(chain: &mut Chain) -> Result<ItemSubClassCatalog> {
    let bytes = chain
        .read_file(ITEM_SUB_CLASS)
        .with_context(|| format!("reading {ITEM_SUB_CLASS}"))?;
    let rs = parse(&bytes, item_sub_class_schema(), "ItemSubClass")?;
    let mut rows = HashMap::with_capacity(rs.records().len());
    let mut names = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let (Some(class), Some(subclass)) = (u32_at(r, 0), u32_at(r, 1)) else {
            continue;
        };
        rows.insert(
            (class, subclass),
            ItemSubClassInfo {
                prerequisite_proficiency: i32_at(r, 2).unwrap_or(-1),
                postrequisite_proficiency: i32_at(r, 3).unwrap_or(-1),
                display_flags: u32_at(r, 5).unwrap_or(0),
            },
        );
        // VerboseName enUS (col 19) first, DisplayName enUS (col 10) fallback — the struct doc's
        // byte law. Empty both → no name row (the header renders blank, faithfully unlikely).
        let name = crate::dbc::str_at(&rs, r, 19)
            .filter(|n| !n.is_empty())
            .or_else(|| crate::dbc::str_at(&rs, r, 10).filter(|n| !n.is_empty()));
        if let Some(name) = name {
            names.insert((class, subclass), name);
        }
    }
    Ok(ItemSubClassCatalog { rows, names })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header-name law on the real 5875 file (0446's TU-B fold-back): verbose-first,
    /// display fallback. Skips without client data.
    #[test]
    fn real_subclass_names_resolve_verbose_first() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_item_sub_classes(&mut chain).expect("load ItemSubClass.dbc");
        assert_eq!(cat.name(2, 7), Some("One-Handed Swords"), "verbose wins");
        assert_eq!(cat.name(4, 1), Some("Cloth"));
        assert_eq!(
            cat.name(5, 0),
            Some("Reagent"),
            "display fallback when verbose empty"
        );
        assert_eq!(cat.name(0, 0), Some("Consumable"));
        assert_eq!(cat.name(99, 0), None);
    }

    /// Data-gated on the real 5875 DBC. Prints the live prereq/postreq and displayFlags rows so
    /// a schema slip is visible, and pins the known shape. Skips without client data.
    #[test]
    fn item_sub_classes_load_from_the_chain() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_item_sub_classes(&mut chain).expect("ItemSubClass.dbc loads");
        assert!(!cat.is_empty());
        // The live alt pairs come in prerequisite/postrequisite couples per weapon family:
        // 2H Axe (2,1) ← 1H Axe via prerequisite, 1H Mace (2,4) → 2H Mace via POSTrequisite
        // (the sentinel short-circuit's second leg), 2H Sword (2,8) ← 1H Sword, and Shield
        // (4,6) ← Buckler. Print the full list so a schema slip is visible.
        for ((c, sc), r) in {
            let mut v: Vec<_> = cat.rows.iter().map(|(&k, v)| (k, *v)).collect();
            v.sort_by_key(|&((c, sc), _)| (c, sc));
            v
        } {
            if r.prerequisite_proficiency != -1 || r.postrequisite_proficiency != -1 {
                eprintln!(
                    "alt: class {c} sub {sc} pre {} post {}",
                    r.prerequisite_proficiency, r.postrequisite_proficiency
                );
            }
        }
        assert_eq!(cat.proficiency_alt(2, 1), Some(0));
        assert_eq!(cat.proficiency_alt(2, 4), Some(5));
        assert_eq!(cat.proficiency_alt(2, 8), Some(7));
        assert_eq!(cat.proficiency_alt(4, 6), Some(5));
        // Daggers stand alone — no other proficiency softens a dagger's red.
        assert_eq!(cat.proficiency_alt(2, 15), None);
        // displayFlags bit 0: Miscellaneous armor (rings/trinkets/shirts) hides its type name;
        // ordinary armor and weapons show theirs.
        assert!(cat.hides_name(4, 0));
        assert!(!cat.hides_name(4, 1), "Cloth prints");
        assert!(!cat.hides_name(2, 7), "Sword prints");
    }
}
