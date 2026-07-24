//! `AreaTable.dbc` — the area catalog: id → (map, parent zone, explore bit, name). The world map
//! (decision 0203 phase 2) resolves the player's MCNK `areaId` to its **top-level zone** through
//! the parent chain here (`SetMapToCurrentZone`), and labels zones with the localized `AreaName`
//! (the WorldMapArea string is the art *folder* — "Elwynn" — while the display name lives here —
//! "Elwynn Forest"). Phase 3's exploration overlays key off [`AreaTableRow::explore_flag`].
//!
//! Layout — VERIFIED against build 5875 (2026-07-07, real-row decode: Northshire Valley id 9 →
//! zone 12 = Elwynn Forest; Booty Bay id 35 → zone 33 = Stranglethorn Vale; cross-checked against
//! mangoszero's `AreaTableEntry`): **25 × u32 cols**: `ID(0), MapID(1), ZoneID(2 — the parent
//! area, 0 = this row IS a top-level zone), ExploreFlag(3), Flags(4), … AreaName(11)` + loc
//! block, `FactionGroupMask(20)`. Sibling readers of the same file: `quest_headers.rs` (id →
//! name only), `area_sound.rs` (the audio columns).
//!
//! The PvP columns (zone-splash arc, decision 0287) — census of the real 5875 table:
//! `FactionGroupMask` is 2/4/0 (Alliance/Horde/neither) on zone rows and ~always 0 on subzone
//! rows; `Flags` bit `0x80` marks exactly the three FFA duel pits (Battle Ring 2177, The Rumble
//! Cage 2857, The Maul 3217 + its UNUSED twin) — *not* the enclosing "Gurubashi Arena" row.

use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::chain::Chain;
use crate::dbc::{parse, str_at, u32_at};

const AREA_TABLE: &str = "DBFilesClient\\AreaTable.dbc";

/// One `AreaTable.dbc` row (the columns the map arc reads).
#[derive(Clone, Debug)]
pub struct AreaTableRow {
    /// `Map.dbc` id the area lives on.
    pub map_id: u32,
    /// The parent area's id — `0` means this row is itself a top-level zone.
    pub zone_id: u32,
    /// The exploration bit index (`PLAYER_EXPLORED_ZONES` bitset / `WorldMapOverlay` join,
    /// phase 3).
    pub explore_flag: u32,
    /// Area flags (col 4). Bit `0x80` = FFA duel pit ("PvP Area" / `isArena`, decision 0287);
    /// bit `0x1` = snow; capitals carry `0x138`.
    pub flags: u32,
    /// `FactionGroup.dbc` mask of the owning side (col 20): 2 = Alliance, 4 = Horde, 0 = neither
    /// (contested — or a subzone row deferring to its zone).
    pub faction_group_mask: u32,
    /// The localized display name ("Elwynn Forest").
    pub name: String,
}

/// `AreaTable.dbc` rows keyed by id.
pub struct AreaTableCatalog {
    by_id: HashMap<u32, AreaTableRow>,
}

impl AreaTableCatalog {
    pub fn get(&self, id: u32) -> Option<&AreaTableRow> {
        self.by_id.get(&id)
    }

    /// The display name for `id`, or `None`.
    pub fn name(&self, id: u32) -> Option<&str> {
        self.by_id.get(&id).map(|r| r.name.as_str())
    }

    /// The top-level zone containing `area_id`: walk the `zone_id` parent chain until a row whose
    /// parent is 0 (possibly `area_id` itself). `None` for an unknown id. Depth-guarded — the
    /// 5875 chains are 1–2 deep, anything deeper is data corruption, not geography.
    pub fn top_zone(&self, area_id: u32) -> Option<u32> {
        let mut id = area_id;
        for _ in 0..8 {
            let row = self.by_id.get(&id)?;
            if row.zone_id == 0 {
                return Some(id);
            }
            id = row.zone_id;
        }
        None
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

/// 25 u32-wide columns; only `ID/MapID/ZoneID/ExploreFlag/Flags/AreaName/FactionGroupMask` are
/// read (see module doc).
fn schema() -> Schema {
    let mut s = Schema::new("AreaTable");
    for i in 0..25 {
        match i {
            11 => s.add_field(SchemaField::new("AreaName", FieldType::String)),
            _ => s.add_field(SchemaField::new(format!("c{i}"), FieldType::UInt32)),
        }
    }
    s
}

/// Read `AreaTable.dbc` off the patch chain into an [`AreaTableCatalog`].
pub fn load_area_table_catalog(chain: &mut Chain) -> Result<AreaTableCatalog> {
    let bytes = chain
        .read_file(AREA_TABLE)
        .context("reading AreaTable.dbc")?;
    let rs = parse(&bytes, schema(), "AreaTable")?;
    let mut by_id = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let (Some(id), Some(map_id), Some(zone_id), Some(explore_flag), Some(flags), Some(name)) = (
            u32_at(r, 0),
            u32_at(r, 1),
            u32_at(r, 2),
            u32_at(r, 3),
            u32_at(r, 4),
            str_at(&rs, r, 11),
        ) else {
            continue;
        };
        let faction_group_mask = u32_at(r, 20).unwrap_or(0);
        by_id.insert(
            id,
            AreaTableRow {
                map_id,
                zone_id,
                explore_flag,
                flags,
                faction_group_mask,
                name,
            },
        );
    }
    Ok(AreaTableCatalog { by_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real 5875 table: the verified parent chains (Northshire → Elwynn, Booty Bay →
    /// Stranglethorn), a top-level zone resolving to itself, and the display-vs-art-folder name
    /// split. Skips without client data.
    #[test]
    fn real_area_table_parent_chains_and_names() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_area_table_catalog(&mut chain).expect("load AreaTable");
        assert!(
            cat.len() > 1000,
            "5875 ships ~1800 areas, got {}",
            cat.len()
        );

        let northshire = cat.get(9).expect("Northshire Valley (id 9)");
        assert_eq!(northshire.zone_id, 12, "Northshire's parent is Elwynn");
        assert_eq!(northshire.name, "Northshire Valley");
        assert_eq!(cat.top_zone(9), Some(12));

        // A leaf two hops down: Booty Bay → Stranglethorn Vale (top-level).
        assert_eq!(cat.top_zone(35), Some(33));

        // A top-level zone resolves to itself; its display name differs from the art folder.
        assert_eq!(cat.top_zone(12), Some(12));
        assert_eq!(cat.name(12), Some("Elwynn Forest"));

        // The phase-3 explore bit is populated (Elwynn's is 126 in 5875).
        assert_eq!(cat.get(12).expect("Elwynn").explore_flag, 126);

        // The PvP columns (decision 0287): ownership masks on zone rows…
        assert_eq!(cat.get(12).expect("Elwynn").faction_group_mask, 2);
        assert_eq!(cat.get(14).expect("Durotar").faction_group_mask, 4);
        assert_eq!(cat.get(33).expect("Stranglethorn").faction_group_mask, 0);
        // …and the FFA-pit bit on the leaf pit rows, NOT the enclosing arena row.
        assert_ne!(cat.get(2177).expect("Battle Ring").flags & 0x80, 0);
        assert_eq!(cat.get(1741).expect("Gurubashi Arena").flags & 0x80, 0);
    }
}
