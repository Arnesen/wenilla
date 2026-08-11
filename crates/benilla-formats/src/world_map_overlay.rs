//! `WorldMapOverlay.dbc` — the per-area zoomed-in overlay art: for a `WorldMapArea` row, up to four
//! `AreaTable` sub-regions plus the texture (and its placement/hit-rect) that draws over the base
//! map when a sub-region is discovered or its highlight is hovered (decision 0203 phase 3's
//! discovery overlays + phase 2's `map_proj` overlay hit-test).
//!
//! Layout — VERIFIED against build 5875 (header + full 526-row decode, cross-checked against
//! vmangos's *commented-out* `WorldMapOverlayEntry` (`src/game/Database/DBCStructure.h:752` — the
//! struct only keeps `ID`/`areatableID[4]`, the rest is server-unused and left as index comments)
//! and independently against a community reference struct for the same build, 2026-07-07: **526 ×
//! 17 × 68 B**: `ID(0), WorldMapAreaID(1), AreaID[4](2-5), MapPointX(6), MapPointY(7),
//! TextureName(8, string), TextureWidth(9), TextureHeight(10), OffsetX(11), OffsetY(12),
//! HitRectTop(13), HitRectLeft(14), HitRectBottom(15), HitRectRight(16)`. Sanity-checked over all
//! 526 rows: `HitRectTop < HitRectBottom` holds for 525/526, `HitRectLeft < HitRectRight` holds for
//! all 526, and `TextureWidth`/`TextureHeight` track the hit-rect span's rough magnitude — all
//! consistent with the field identities above. `MapPointX`/`MapPointY` are `0` on **every** row in
//! this data (matches vmangos's own `// to implement [?]` note on the struct: authored but unused
//! in 1.12).

use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, str_at, u32_at};
use crate::Chain;

const WORLD_MAP_OVERLAY: &str = "DBFilesClient\\WorldMapOverlay.dbc";

/// One `WorldMapOverlay.dbc` row: a sub-region's overlay texture + placement.
#[derive(Clone, Debug)]
pub struct WorldMapOverlay {
    /// `WorldMapArea.dbc` id this overlay draws over.
    pub world_map_area_id: u32,
    /// Up to 4 `AreaTable.dbc` ids this overlay's highlight covers (`0` = unused slot).
    pub area_id: [u32; 4],
    /// Always `0` in 5875 — an authored-but-unused map-point (see the module doc).
    pub map_point_x: u32,
    pub map_point_y: u32,
    /// The overlay texture's base name (no path/extension) under
    /// `Interface\WorldMap\<WorldMapArea name>\`.
    pub texture_name: String,
    pub texture_width: u32,
    pub texture_height: u32,
    pub offset_x: u32,
    pub offset_y: u32,
    pub hit_rect_top: u32,
    pub hit_rect_left: u32,
    pub hit_rect_bottom: u32,
    pub hit_rect_right: u32,
}

/// `WorldMapOverlay.dbc` rows grouped by `WorldMapAreaID` — the join key against
/// [`crate::WorldMapAreaCatalog`].
pub struct WorldMapOverlayCatalog {
    by_area: HashMap<u32, Vec<WorldMapOverlay>>,
}

impl WorldMapOverlayCatalog {
    /// The overlays for `world_map_area_id`, or an empty slice if none.
    pub fn for_area(&self, world_map_area_id: u32) -> &[WorldMapOverlay] {
        self.by_area
            .get(&world_map_area_id)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn len(&self) -> usize {
        self.by_area.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.by_area.is_empty()
    }
}

/// 17 fields per the module doc.
fn schema() -> Schema {
    let mut s = Schema::new("WorldMapOverlay");
    for name in ["ID", "WorldMapAreaID"] {
        s.add_field(SchemaField::new(name, FieldType::UInt32));
    }
    s.add_field(SchemaField::new_array("AreaID", FieldType::UInt32, 4));
    for name in ["MapPointX", "MapPointY"] {
        s.add_field(SchemaField::new(name, FieldType::UInt32));
    }
    s.add_field(SchemaField::new("TextureName", FieldType::String));
    for name in [
        "TextureWidth",
        "TextureHeight",
        "OffsetX",
        "OffsetY",
        "HitRectTop",
        "HitRectLeft",
        "HitRectBottom",
        "HitRectRight",
    ] {
        s.add_field(SchemaField::new(name, FieldType::UInt32));
    }
    s
}

/// Read `WorldMapOverlay.dbc` off the patch chain into a [`WorldMapOverlayCatalog`].
pub fn load_world_map_overlay_catalog(chain: &mut Chain) -> Result<WorldMapOverlayCatalog> {
    let bytes = chain
        .read_file(WORLD_MAP_OVERLAY)
        .context("reading WorldMapOverlay.dbc")?;
    let rs = parse(&bytes, schema(), "WorldMapOverlay")?;
    let mut by_area: HashMap<u32, Vec<WorldMapOverlay>> = HashMap::new();
    for r in rs.records() {
        let Some(world_map_area_id) = u32_at(r, 1) else {
            continue;
        };
        let area_id = [
            u32_at(r, 2).unwrap_or(0),
            u32_at(r, 3).unwrap_or(0),
            u32_at(r, 4).unwrap_or(0),
            u32_at(r, 5).unwrap_or(0),
        ];
        let overlay = WorldMapOverlay {
            world_map_area_id,
            area_id,
            map_point_x: u32_at(r, 6).unwrap_or(0),
            map_point_y: u32_at(r, 7).unwrap_or(0),
            texture_name: str_at(&rs, r, 8).unwrap_or_default(),
            texture_width: u32_at(r, 9).unwrap_or(0),
            texture_height: u32_at(r, 10).unwrap_or(0),
            offset_x: u32_at(r, 11).unwrap_or(0),
            offset_y: u32_at(r, 12).unwrap_or(0),
            hit_rect_top: u32_at(r, 13).unwrap_or(0),
            hit_rect_left: u32_at(r, 14).unwrap_or(0),
            hit_rect_bottom: u32_at(r, 15).unwrap_or(0),
            hit_rect_right: u32_at(r, 16).unwrap_or(0),
        };
        by_area.entry(world_map_area_id).or_default().push(overlay);
    }
    Ok(WorldMapOverlayCatalog { by_area })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real 5875 table: Teldrassil's `WorldMapArea` id (41) carries overlays with non-empty
    /// texture names (e.g. the "Bane Thillhollow"/"Darnassus" sub-zone highlights). Skips without
    /// client data.
    #[test]
    fn real_world_map_overlay_has_nonempty_textures_for_teldrassil() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_world_map_overlay_catalog(&mut chain).expect("load WorldMapOverlay");
        assert_eq!(cat.len(), 526, "all 526 rows load");

        let teldrassil = cat.for_area(41);
        assert!(
            !teldrassil.is_empty(),
            "Teldrassil (WorldMapArea 41) has overlay rows"
        );
        assert!(
            teldrassil.iter().all(|o| !o.texture_name.is_empty()),
            "every overlay row names a texture"
        );
        assert!(
            teldrassil.iter().any(|o| o.texture_name == "DARNASSUS"),
            "the Darnassus sub-zone highlight is present"
        );
    }
}
