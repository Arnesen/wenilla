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

use anyhow::Result;

use crate::chain::Chain;
use crate::dbc::load_id_name_table;

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

/// Load both name tables through the patch chain.
pub fn load_quest_header_names(chain: &mut Chain) -> Result<QuestHeaderNames> {
    Ok(QuestHeaderNames {
        zones: load_id_name_table(chain, "DBFilesClient\\AreaTable.dbc", 11, 25, "AreaTable")?,
        sorts: load_id_name_table(chain, "DBFilesClient\\QuestSort.dbc", 1, 10, "QuestSort")?,
    })
}
