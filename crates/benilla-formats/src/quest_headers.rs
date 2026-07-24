//! Quest-log **header names**: the quest template's `ZoneOrSort` → the section title the list
//! groups under ("Northshire Valley", "Warlock", …).
//!
//! The 1.12 client resolves the sign split the same way: **positive** = an `AreaTable.dbc` id
//! (the zone/subzone name), **negative** = a `QuestSort.dbc` id (class/profession/seasonal sort
//! names). Layouts:
//! - `AreaTable.dbc` 25 × u32 cols: `ID(0) … AreaName(11)` + loc block (the audio catalog's
//!   `area_sound.rs` documents the head; only ID + name matter here).
//! - `QuestSort.dbc` 10 × u32 cols: `ID(0), SortName(1)` + the rest of the loc block.

use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::chain::Chain;
use crate::dbc::{parse, str_at, u32_at};

/// The resolved `ZoneOrSort → name` lookup ([`load_quest_header_names`]).
#[derive(Debug, Default)]
pub struct QuestHeaderNames {
    /// `AreaTable.ID → AreaName` (positive ZoneOrSort).
    zones: HashMap<u32, String>,
    /// `QuestSort.ID → SortName` (negative ZoneOrSort, negated).
    sorts: HashMap<u32, String>,
}

impl QuestHeaderNames {
    /// The header title for a quest's `ZoneOrSort`, or `None` for an id neither table knows
    /// (including 0 — the caller picks its own fallback bucket).
    pub fn resolve(&self, zone_or_sort: i32) -> Option<&str> {
        if zone_or_sort > 0 {
            self.zones.get(&(zone_or_sort as u32)).map(String::as_str)
        } else if zone_or_sort < 0 {
            self.sorts
                .get(&(zone_or_sort.unsigned_abs()))
                .map(String::as_str)
        } else {
            None
        }
    }
}

/// A minimal `ID(0) … name(name_col) …` schema of `cols` u32-wide columns (strings are one u32
/// ref) — enough for the two id→name reads this module does.
fn id_name_schema(what: &str, name_col: usize, cols: usize) -> Schema {
    let mut s = Schema::new(what);
    for i in 0..cols {
        if i == name_col {
            s.add_field(SchemaField::new("Name", FieldType::String));
        } else {
            s.add_field(SchemaField::new(format!("c{i}"), FieldType::UInt32));
        }
    }
    s
}

/// Load both name tables through the patch chain.
pub fn load_quest_header_names(chain: &mut Chain) -> Result<QuestHeaderNames> {
    let mut read =
        |file: &str, name_col: usize, cols: usize, what: &str| -> Result<HashMap<u32, String>> {
            let bytes = chain
                .read_file(file)
                .with_context(|| format!("reading {file}"))?;
            let rs = parse(&bytes, id_name_schema(what, name_col, cols), what)?;
            let mut out = HashMap::with_capacity(rs.records().len());
            for r in rs.records() {
                let (Some(id), Some(name)) = (u32_at(r, 0), str_at(&rs, r, name_col)) else {
                    continue;
                };
                if !name.is_empty() {
                    out.insert(id, name);
                }
            }
            Ok(out)
        };
    Ok(QuestHeaderNames {
        zones: read("DBFilesClient\\AreaTable.dbc", 11, 25, "AreaTable")?,
        sorts: read("DBFilesClient\\QuestSort.dbc", 1, 10, "QuestSort")?,
    })
}
