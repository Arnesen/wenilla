//! `AreaPOI.dbc` — the world-map's named points of interest: inns, flight masters, dungeon
//! entrances, battleground nodes — everything the minimap's nearest-3 POI blips (decision 0203
//! phase 3) and the world map's POI icons draw from.
//!
//! **Client-only: vmangos carries no struct for this table** (never loaded server-side). Layout
//! pinned by inspection this session (header + full 339-row decode, cross-checked against a
//! community reference struct for the same build, 2026-07-07): **339 × 29 × 116 B**:
//! `ID(0), Importance(1), Icon(2), FactionID(3), Pos[3](4-6, world x/y/z), ContinentID(7),
//! Flags(8), AreaID(9), Name_lang(10, enUS string) + 7 other-locale slots(11-17) + NameFlags(18),
//! Description_lang(19, enUS string) + 7 other-locale slots(20-26) + DescriptionFlags(27),
//! WorldStateID(28)` — the field the task brief's 28-column enumeration left unaccounted for.
//!
//! Every field past `Pos`/`Name`/`Description` was proven by scanning all 339 rows, not assumed:
//! `Importance` ∈ `{0,2,3,4}` and `Icon` takes 43 distinct small values (both enum-shaped);
//! `FactionID` ∈ `{0,83,84}` (a `FactionTemplate` FK used for only the faction-tinted rows);
//! `ContinentID` ∈ `{0,1,30,37,529}` — Azeroth/Kalimdor plus three battleground map ids;
//! `AreaID` ranges `−1..=3427` including both `−1` (raw, stored as `u32::MAX` — the
//! `WMOAreaTable`-style "no group" sentinel) and a distinct `0`; every row's `Pos.x`/`Pos.y` falls
//! well inside `±17066` (the world-map half-extent); every row's enUS `Name_lang` is non-empty;
//! and **field 28 is `0` on 209 rows and a small nonzero id on the other 130**, each of those 130
//! landing in one of a handful of tight numeric bands (`1301-1304`, `1325-1340`, …) — exactly the
//! shape of a `WorldState` id, confirmed on a named example: id 1613 "Stables" (an Arathi Basin
//! node, `ContinentID` 529) carries `WorldStateID` 1770 and `Description_lang` "In Conflict", the
//! live capture-state label a BG node's map icon shows.

use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{f32_at, parse, str_at, u32_at};
use crate::Chain;

const AREA_POI: &str = "DBFilesClient\\AreaPOI.dbc";

/// One `AreaPOI.dbc` row — a named world-map point of interest.
#[derive(Clone, Debug)]
pub struct AreaPoi {
    /// Map-icon prominence tier (`{0,2,3,4}` in 5875 — higher shows at more zoomed-out levels).
    pub importance: u32,
    /// The map-icon art index (43 distinct icons in 5875).
    pub icon: u32,
    /// `FactionTemplate.dbc` FK for a faction-tinted icon; `0` = untinted (the common case).
    pub faction_id: u32,
    /// World position `(x, y, z)`.
    pub pos: [f32; 3],
    /// The map id this POI's `pos` is expressed in (Azeroth `0`, Kalimdor `1`, or a
    /// battleground/instance map id).
    pub continent_id: u32,
    pub flags: u32,
    /// `AreaTable.dbc` id, or `u32::MAX` (raw `−1`) for a continent-wide POI — distinct in this
    /// data from a plain `0` (see the module doc).
    pub area_id: u32,
    /// enUS display name (locale slot 0) — non-empty on every 5875 row.
    pub name: String,
    /// enUS description (locale slot 0) — empty on the ~2/3 of rows with no live status text.
    pub description: String,
    /// `WorldState` id driving this POI's live label/icon (e.g. a battleground node's capture
    /// state); `0` = no live state (209 of 339 rows).
    pub world_state_id: u32,
}

/// `AreaPOI.dbc` rows keyed by `ID`.
pub struct AreaPoiCatalog {
    rows: HashMap<u32, AreaPoi>,
}

impl AreaPoiCatalog {
    pub fn get(&self, id: u32) -> Option<&AreaPoi> {
        self.rows.get(&id)
    }

    /// Every row, in no particular order — the nearest-3 POI selection (decision 0203 phase 3)
    /// filters this by `continent_id` + distance itself.
    pub fn rows(&self) -> impl Iterator<Item = (u32, &AreaPoi)> {
        self.rows.iter().map(|(&id, poi)| (id, poi))
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// 29 fields per the module doc: the two loc-string blocks are 9 dwords each (a string + 7
/// other-locale string slots + a flags dword), matching the `TaxiNodes` schema pattern
/// (`crate::schema_for`).
fn schema() -> Schema {
    let mut s = Schema::new("AreaPOI");
    for name in ["ID", "Importance", "Icon", "FactionID"] {
        s.add_field(SchemaField::new(name, FieldType::UInt32));
    }
    s.add_field(SchemaField::new_array("Pos", FieldType::Float32, 3));
    for name in ["ContinentID", "Flags", "AreaID"] {
        s.add_field(SchemaField::new(name, FieldType::UInt32));
    }
    for (name_col, flags_col) in [("Name", "NameFlags"), ("Description", "DescriptionFlags")] {
        s.add_field(SchemaField::new(name_col, FieldType::String)); // enUS (locale 0)
        s.add_field(SchemaField::new_array(
            format!("{name_col}OtherLocales"),
            FieldType::String,
            7,
        ));
        s.add_field(SchemaField::new(flags_col, FieldType::UInt32));
    }
    s.add_field(SchemaField::new("WorldStateID", FieldType::UInt32));
    s
}

/// Read `AreaPOI.dbc` off the patch chain into an [`AreaPoiCatalog`].
pub fn load_area_poi_catalog(chain: &mut Chain) -> Result<AreaPoiCatalog> {
    let bytes = chain.read_file(AREA_POI).context("reading AreaPOI.dbc")?;
    let rs = parse(&bytes, schema(), "AreaPOI")?;
    let mut rows = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        let (Some(x), Some(y), Some(z)) = (f32_at(r, 4), f32_at(r, 5), f32_at(r, 6)) else {
            continue;
        };
        let poi = AreaPoi {
            importance: u32_at(r, 1).unwrap_or(0),
            icon: u32_at(r, 2).unwrap_or(0),
            faction_id: u32_at(r, 3).unwrap_or(0),
            pos: [x, y, z],
            continent_id: u32_at(r, 7).unwrap_or(0),
            flags: u32_at(r, 8).unwrap_or(0),
            area_id: u32_at(r, 9).unwrap_or(0),
            name: str_at(&rs, r, 10).unwrap_or_default(),
            description: str_at(&rs, r, 19).unwrap_or_default(),
            world_state_id: u32_at(r, 28).unwrap_or(0),
        };
        rows.insert(id, poi);
    }
    Ok(AreaPoiCatalog { rows })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real 5875 table proves the layout: every row's enUS name is readable text, continent
    /// ids and world positions land in the expected ranges, and the pinned Arathi Basin "Stables"
    /// example resolves its `WorldStateID` + live status description. Skips without client data.
    #[test]
    fn real_area_poi_layout_sanity() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_area_poi_catalog(&mut chain).expect("load AreaPOI");
        assert_eq!(cat.len(), 339, "all 339 rows load");

        let mut nonempty_names = 0;
        let mut worldstate_nonzero = 0;
        for (_, poi) in cat.rows() {
            if !poi.name.is_empty() {
                nonempty_names += 1;
            }
            assert!(
                poi.pos[0].abs() <= 17066.0 && poi.pos[1].abs() <= 17066.0,
                "world x/y within the map half-extent: {:?}",
                poi.pos
            );
            // 0/1 are the continents; the rest are battleground/instance map ids — all small.
            assert!(poi.continent_id < 10_000, "continentId is a small map id");
            if poi.world_state_id != 0 {
                worldstate_nonzero += 1;
            }
        }
        assert_eq!(nonempty_names, 339, "every row has a readable enUS name");
        assert!(
            worldstate_nonzero >= 100,
            "a meaningful share of rows carry a live WorldStateID: {worldstate_nonzero}"
        );

        // The pinned Arathi Basin "Stables" node.
        let stables = cat.get(1613).expect("id 1613 (Stables)");
        assert_eq!(stables.name, "Stables");
        assert_eq!(stables.continent_id, 529, "Arathi Basin's map id");
        assert_eq!(stables.world_state_id, 1770);
        assert_eq!(stables.description, "In Conflict");
    }
}
