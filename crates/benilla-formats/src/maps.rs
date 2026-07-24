//! Map.dbc loader: resolves a server-side `mapId` to its **MPQ directory name** (`0` →
//! `"Azeroth"`, `1` → `"Kalimdor"`, `36` → `"DeadminesInstance"`, …) so the cross-map teleport
//! handler can call `MapTiles::load(chain, dir)` for the new world.
//!
//! Layout — VERIFIED against build 5875 (`xxd` on extracted `DBFilesClient\Map.dbc`,
//! 2026-05-29; field 38 confirmed 2026-06-02): the WDBC header reports **44 records · 42 fields
//! · 168 B/record**. Load-bearing for us: `ID` (0), `Directory` (1), and **`LoadingScreenID`
//! (38)** — an FK into `LoadingScreens.dbc` (see [`crate::LoadingScreenCatalog`]) selecting the
//! map's full-screen load art. Verified empirically: field 38 ∈ the LoadingScreens id-set for 39
//! of 44 maps (the 5 zeros are dev/test maps), with exact known pairs (mapId 0 `Azeroth` → 4,
//! mapId 1 `Kalimdor` → 3, `DeadminesInstance` → 142). The remaining dwords are `MapName_loc`
//! (9), `MapDescription0_loc` (9), `MapDescription1_loc` (9), plus a few ints we don't need.
//! Reading them as `UInt32` placeholders is fine — DBC fields are 4 bytes regardless of declared
//! type, so the schema only has to add up to 42 × 4 = 168.

use std::collections::HashMap;

use crate::Chain;
use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, str_at, u32_at};

const MAP: &str = "DBFilesClient\\Map.dbc";

/// Field index of the `LoadingScreenID` FK (see module docs). 0 means "no art" (dev/test maps).
const LOADING_SCREEN_FIELD: usize = 38;

/// Resolved per-map data, built once at startup off the client's Map.dbc.
pub struct MapCatalog {
    dirs: HashMap<u32, String>,
    /// `mapId → MapName_Lang[enUS]` (field 4, `+0x10` — the offset the binary's map-name reader
    /// `0x4a65a0` uses against this same patch-2 layout). The world map's continent dropdown
    /// displays THIS ("Eastern Kingdoms"), not the WorldMapArea art-folder string ("Azeroth") —
    /// wow-re Q3 verdict, 2026-07-07.
    names: HashMap<u32, String>,
    /// `mapId → LoadingScreenID` FK into `LoadingScreens.dbc` (only maps with a non-zero FK).
    loading_screens: HashMap<u32, u32>,
}

impl MapCatalog {
    /// MPQ directory name (the `Directory` column) for `map_id`, or `None` if the DBC has no
    /// such row. Use with `MapTiles::load(chain, dir)`.
    pub fn directory(&self, map_id: u32) -> Option<&str> {
        self.dirs.get(&map_id).map(String::as_str)
    }

    /// The localized display name (`MapName_Lang`, enUS) for `map_id` — "Eastern Kingdoms",
    /// "Kalimdor", "The Deadmines", …
    pub fn name(&self, map_id: u32) -> Option<&str> {
        self.names.get(&map_id).map(String::as_str)
    }

    /// `LoadingScreenID` for `map_id` — the FK to resolve against [`crate::LoadingScreenCatalog`]
    /// for the load-art BLP. `None` for dev/test maps that carry no screen. This is the same
    /// mechanism for *every* map kind (open world, instance, battleground) — only the art row
    /// differs.
    pub fn loading_screen_id(&self, map_id: u32) -> Option<u32> {
        self.loading_screens.get(&map_id).copied()
    }

    pub fn len(&self) -> usize {
        self.dirs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }
}

/// Field index of the enUS `MapName_Lang` string (`+0x10`, see [`MapCatalog::names`]).
const MAP_NAME_FIELD: usize = 4;

/// 42 fields total; field 0 = ID, field 1 = Directory string, field 4 = MapName (enUS),
/// field 38 = LoadingScreenID (FK). Remaining fields are placeholders (4-byte dwords; the schema
/// only needs to total 168 B).
fn map_schema() -> Schema {
    let mut s = Schema::new("Map");
    s.add_field(SchemaField::new("ID", FieldType::UInt32));
    s.add_field(SchemaField::new("Directory", FieldType::String));
    for i in 2..42 {
        match i {
            MAP_NAME_FIELD => s.add_field(SchemaField::new("MapName", FieldType::String)),
            LOADING_SCREEN_FIELD => {
                s.add_field(SchemaField::new("LoadingScreenID", FieldType::UInt32))
            }
            _ => s.add_field(SchemaField::new(format!("_pad{i}"), FieldType::UInt32)),
        }
    }
    s
}

/// Read Map.dbc off the patch chain into a [`MapCatalog`].
pub fn load_map_catalog(chain: &mut Chain) -> Result<MapCatalog> {
    let bytes = chain
        .read_file(MAP)
        .with_context(|| format!("reading {MAP}"))?;
    let rs = parse(&bytes, map_schema(), "Map")?;
    let mut dirs = HashMap::with_capacity(rs.records().len());
    let mut names = HashMap::with_capacity(rs.records().len());
    let mut loading_screens = HashMap::new();
    for r in rs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        if let Some(dir) = str_at(&rs, r, 1) {
            dirs.insert(id, dir);
        }
        if let Some(name) = str_at(&rs, r, MAP_NAME_FIELD).filter(|n| !n.is_empty()) {
            names.insert(id, name);
        }
        // 0 = "no loading screen" (dev/test maps); only record real FKs.
        if let Some(ls) = u32_at(r, LOADING_SCREEN_FIELD).filter(|&v| v != 0) {
            loading_screens.insert(id, ls);
        }
    }
    Ok(MapCatalog {
        dirs,
        names,
        loading_screens,
    })
}
