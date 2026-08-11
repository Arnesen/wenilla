//! LoadingScreens.dbc loader: resolves a `LoadingScreenID` (the FK in `Map.dbc` field 38, see
//! [`crate::MapCatalog::loading_screen_id`]) to the full-screen load-art **BLP path**.
//!
//! Layout — VERIFIED against build 5875 (raw-byte decode of extracted `DBFilesClient\
//! LoadingScreens.dbc`, 2026-06-02): the WDBC header reports **40 records · 3 fields · 12 B/record**
//! (3 × 4 = 12, and `20 + 40·12 + 2926 == 3426` file bytes). Fields = `(ID, Name string, FileName
//! string)`; field 0 = id, field 2 = BLP path (e.g. `Interface\Glues\LoadingScreens\
//! LoadScreenEasternKingdom.blp`). Open-world art is continent-wide — id 3 = Kalimdor, id 4 =
//! Azeroth/EasternKingdom — the other 38 rows are per-instance. The art is a 512×512 square scene
//! with the WoW logo composited in; the client stretch/crop-fills it to the screen.

use std::collections::HashMap;

use crate::Chain;
use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, str_at, u32_at};

const LOADING_SCREENS: &str = "DBFilesClient\\LoadingScreens.dbc";

/// Resolved `LoadingScreenID → BLP path` map, built once at startup off the client's
/// LoadingScreens.dbc.
pub struct LoadingScreenCatalog {
    paths: HashMap<u32, String>,
}

impl LoadingScreenCatalog {
    /// The load-art BLP path for `id` (a `Map.dbc` `LoadingScreenID`), or `None` if absent.
    pub fn path(&self, id: u32) -> Option<&str> {
        self.paths.get(&id).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// 3 fields: ID (0), Name string (1, unused), FileName string (2 = BLP path).
fn schema() -> Schema {
    let mut s = Schema::new("LoadingScreens");
    s.add_field(SchemaField::new("ID", FieldType::UInt32));
    s.add_field(SchemaField::new("Name", FieldType::String));
    s.add_field(SchemaField::new("FileName", FieldType::String));
    s
}

/// Read LoadingScreens.dbc off the patch chain into a [`LoadingScreenCatalog`].
pub fn load_loading_screens(chain: &mut Chain) -> Result<LoadingScreenCatalog> {
    let bytes = chain
        .read_file(LOADING_SCREENS)
        .with_context(|| format!("reading {LOADING_SCREENS}"))?;
    let rs = parse(&bytes, schema(), "LoadingScreens")?;
    let mut paths = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        if let (Some(id), Some(path)) = (u32_at(r, 0), str_at(&rs, r, 2)) {
            paths.insert(id, path);
        }
    }
    Ok(LoadingScreenCatalog { paths })
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::load_map_catalog;

    /// Golden test of the full Map.dbc→LoadingScreens.dbc→BLP FK chain against the real client.
    /// Verified pairs (decoded 2026-06-02): mapId 0 (Azeroth) → 4 → EasternKingdom art; mapId 1
    /// (Kalimdor) → 3 → Kalimdor art. Skips when the client isn't present.
    #[test]
    fn resolves_open_world_loading_art() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let maps = load_map_catalog(&mut chain).expect("Map.dbc");
        let screens = load_loading_screens(&mut chain).expect("LoadingScreens.dbc");

        // FK values (Map.dbc field 38).
        assert_eq!(maps.loading_screen_id(0), Some(4), "Azeroth → 4");
        assert_eq!(maps.loading_screen_id(1), Some(3), "Kalimdor → 3");

        // FK → BLP path (LoadingScreens.dbc field 2).
        assert_eq!(
            screens.path(4),
            Some("Interface\\Glues\\LoadingScreens\\LoadScreenEasternKingdom.blp")
        );
        assert_eq!(
            screens.path(3),
            Some("Interface\\Glues\\LoadingScreens\\LoadScreenKalimdor.blp")
        );

        // End-to-end resolution for both continents.
        let resolve = |map_id| {
            maps.loading_screen_id(map_id)
                .and_then(|id| screens.path(id))
        };
        assert!(resolve(0)
            .unwrap()
            .ends_with("LoadScreenEasternKingdom.blp"));
        assert!(resolve(1).unwrap().ends_with("LoadScreenKalimdor.blp"));
    }
}
