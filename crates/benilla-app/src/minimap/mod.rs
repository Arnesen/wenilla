//! The HUD minimap renderer (decision 0203 phase 1) — the app half of the `<Minimap>` widget.
//!
//! The engine side (benilla-ui) carries the widget's rect + zoom and emits a
//! `QuadContent::Minimap` hole at the frame's own draw slot; `ui_script::extract::drive_script` parks that
//! in [`MinimapWidget`], and [`emit_minimap`] (in the [`UiQuadAppend`] window) fills it: the
//! streamed tile window around the player, clipped to the widget rect and masked to the
//! `MinimapMask.blp` circle at present time, with the player arrow rotating on top. Children of
//! the widget (border art, buttons, zone text — `MinimapCluster.xml`) draw above per the normal
//! z order.
//!
//! Mechanism per wow-re's T3 minimap node (byte-verified there, transcribed here):
//! - **Tile grid**: one 256² minimap BLP per ADT tile (533.33 yd), named `map<X>_<Y>.blp` in the
//!   map's directory and resolved through `md5translate.trs` to a hashed file under
//!   `textures\Minimap\` ([`benilla_formats::MinimapTranslate`]). Index order = the ADT order
//!   (chain-verified, see the formats re-export note).
//! - **Zoom → world radius** (`zoom_to_scale` 0x6da9b0): the client keeps **two** zoom indices,
//!   selected by whether the player is inside a WMO. **Outdoors** the chunk-count table
//!   `{14,12,10,8,6,4} · 0.5 · 33.333` yd of half-extent; **indoors** the radius table
//!   `{150,120,90,60,40,25}` yd outright ([`INTERIOR_ZOOM_RADIUS`]). Each index persists separately
//!   (CVars `minimapZoom` / `minimapInsideZoom`).
//! - **North-up orientation**: screen up = world +X (north), screen left = world +Y (west).
//!
//! Tiles stream through the `mpq://` async asset source (the terrain streamer's hitch-free bulk
//! path); handles cache per tile in [`MinimapTileCache`] and reset on a map change.
//!
//! Submodules: [`interior`] — the WMO-interior group selection (portal flood-fill) + tile-name
//! stem; [`blips`] — the phase-3 blip layer (AreaPOI landmark arrows, quest-giver dots, the
//! hover tooltip).

mod blips;
mod interior;

use std::collections::HashMap;

use bevy::math::Rect;
use bevy::prelude::*;

use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use benilla_assets::minimap_grid::group_axis_grid;
use benilla_assets::WmoModel;
use benilla_formats::{tile_to_world, world_to_tile, AreaPoiCatalog, MinimapTranslate};

use interior::{interior_group_selection, wmo_minimap_stem};

use crate::assets::{AssetSet, LockRecover, WorldAssets};
use crate::player::Player;
use crate::ui_pass::{UiQuad, UiQuadAppend, UiQuadMask, UiQuads};
use crate::wmo_portal::{
    down_ray_seeds, terrain_z_local, WmoPortalInstance, INTERIOR_PROBE_HEIGHT,
};
use crate::world_map::{CurrentMap, MapCatalogRes};

/// Yards per ADT tile / per MCNK chunk (16 chunks per tile edge).
const TILE_YARDS: f32 = 533.333_3;
const CHUNK_YARDS: f32 = TILE_YARDS / 16.0;

/// The north-up zoom table (wow-re minimap node, `0x8116d0`): view **diameter** in chunks per
/// zoom index; half-extent = `chunks · 0.5 · 33.333` yd (`zoom_to_scale` 0x6da9b0's unlocked leg).
/// This is the **outdoor** zoom basis.
const ZOOM_CHUNKS: [f32; 6] = [14.0, 12.0, 10.0, 8.0, 6.0, 4.0];

/// The **interior** minimap view radius per *indoor* zoom index, in yards — the client's radius table
/// `0x8116e8`, indexed by the separate indoor zoom index `0x86f69c` (CVar `minimapInsideZoom`).
/// Indoors the minimap has its **own zoom state**: a different index, a different table, and a
/// radius in raw yards rather than the outdoor chunk half-extent. That is the "different zoom states
/// inside vs outside" the director reported (2026-07-09).
///
/// On-screen radius is exactly this value (wow-re `wmo-interior-minimap.md` finding 2 **Q7
/// CORRECTION**, VERIFIED: the client composites the interior to an offscreen target at `1.5·c` and
/// blits its middle two-thirds, netting `1.0·c`). The **same `c`** sizes the tile-selection query box
/// (Sub-Q4b), so selection and draw stay coherent.
///
/// NB an earlier reading of this same node claimed the interior scale was a compile-time constant
/// (`10.0f` ⇒ a fixed 15 yd, zoom-independent). That was WRONG — it measured the *static
/// initializer*, missing the per-frame write `mov [esi+0xc], radiusTable[indoorZoom]` that reaches
/// the field through a computed pointer. Superseded in wow-re; do not "restore" a constant here.
const INTERIOR_ZOOM_RADIUS: [f32; 6] = [150.0, 120.0, 90.0, 60.0, 40.0, 25.0];

/// The corpse blip's edge as a fraction of the widget side (the POIIcons cell is authored 16px on
/// a 140px minimap ≈ 0.11; INTERIM eyeball beside [`ARROW_FRACTION`]'s).
const CORPSE_BLIP_FRACTION: f32 = 0.11;

/// The day-night tint the reference MODULATEs the **outdoor** (ADT) minimap tiles by before drawing
/// — the tiles are NOT drawn at full white (that reads too bright). Verified in the CWorldFrame
/// minimap draw (`wow-5875-re` minimap node, tile draw `0x4eccdd`–`0x4ecd69`): from the two global
/// day-night light colours — `color_a` = the Direct/Diffuse band (`LightIntBand` 0 = the light
/// table's `table[0]`), `color_b` = the Ambient band (band 1 = `table[1]`):
///
/// ```text
///   L  = luma601(color_b)                # (r·77 + g·151 + b·28) >> 8, on 0..255 bytes
///   t  = min(L + 96, 255) / 256          # a +96 floor: even pitch-dark tints ~0.375 toward white
///   B' = lerp(color_b, white, t)
///   A' = lerp(color_a, B', 0.75) = 0.25·color_a + 0.75·B'
/// ```
///
/// Inputs and output are **gamma-space** (`WowLighting`'s sRGB 0..1 convention); handed to the UI
/// quad as its vertex colour, whose own linearize→re-encode reproduces the client's gamma-space
/// MODULATE (decision 0089). Interior (WMO) tiles are drawn full white and skip this.
fn minimap_day_tint(ambient: [f32; 3], diffuse: [f32; 3]) -> [f32; 3] {
    let (color_a, color_b) = (diffuse, ambient);
    // Rec.601 luma on 0..255 bytes: the weights (77,151,28) sum to 256, so the parenthesised sum is
    // a 0..1 weighted average; ×255 lifts it to the byte the client's `>> 8` produces.
    let l_byte = 255.0 * (color_b[0] * 77.0 + color_b[1] * 151.0 + color_b[2] * 28.0) / 256.0;
    let t = (l_byte + 96.0).min(255.0) / 256.0;
    let mut out = [0.0_f32; 3];
    for c in 0..3 {
        let b_prime = color_b[c] + (1.0 - color_b[c]) * t; // lerp(color_b, white, t)
        out[c] = color_a[c] * 0.25 + b_prime * 0.75; // lerp(color_a, B', 0.75)
    }
    out
}

/// A `.map()` adaptor over a `md5translate.trs` hit: stream the hashed tile off-thread (like a
/// terrain tile) but decode it as a UI **sprite** (sRGB, clamp, mip 0). The `WorldArt` default
/// (gamma-space `Rgba8Unorm`, repeat) would draw the tile ~2× too bright through the UI pass and
/// bleed tile edges — the invariant `benilla-assets`' `minimap_tile_settings_reach_the_async_loader`
/// guards. Shared by the terrain and interior tile paths.
fn load_tile(asset_server: &AssetServer) -> impl Fn(&str) -> Handle<Image> + '_ {
    move |hash: &str| {
        asset_server.load_with_settings(
            format!("mpq://textures/Minimap/{hash}"),
            |s: &mut benilla_assets::BlpLoaderSettings| {
                s.variant = benilla_assets::BlpVariant::Sprite;
            },
        )
    }
}

/// This frame's extracted `<Minimap>` widget slot, written by `ui_script::extract::drive_script` (the
/// `QuadContent::Minimap` arm) — `None` when no Minimap widget is visible (cluster hidden, no XML).
#[derive(Resource, Default)]
pub(crate) struct MinimapWidget(pub(crate) Option<MinimapSlot>);

/// One extracted Minimap widget: where it sits on screen (y-down logical px), its paint key, and
/// its live widget state.
pub(crate) struct MinimapSlot {
    pub(crate) rect: Rect,
    pub(crate) z: u64,
    /// The outdoor zoom index (chunk table); `inside_zoom` is its indoor twin (radius table). The
    /// client persists both and picks by WMO containment — see [`INTERIOR_ZOOM_RADIUS`].
    pub(crate) zoom: u8,
    pub(crate) inside_zoom: u8,
    pub(crate) alpha: f32,
}

/// The loaded minimap fixtures: the tile hash catalog + the circular mask + the arrow art.
/// Inserted at startup once the chain is open; absent = the minimap draws nothing (its XML
/// children still render).
#[derive(Resource)]
struct MinimapAssets {
    translate: MinimapTranslate,
    mask: Option<Handle<Image>>,
    arrow: Option<Handle<Image>>,
    /// The shared POI atlas (`Interface\Minimap\POIIcons`) — the corpse blip's skull cell
    /// (decision 0308 §5) and any later POI rides it.
    poi: Option<Handle<Image>>,
    /// The landmark-arrow art — the flat `.blp` of the reference's `minimapArrowModel`
    /// (`Rotating-MinimapArrow.mdl`); a stand-in like the player arrow's (0203's flagged
    /// simplification, pending the dispatched §5 draw-law verdict).
    landmark: Option<Handle<Image>>,
    /// The unit-blip atlas (`Interface\Minimap\ObjectIcons`, five 32-px dot cells) — the
    /// quest-giver dots.
    object_icons: Option<Handle<Image>>,
    /// The `AreaPOI.dbc` catalog the landmark selection draws from; `None` = the DBC failed
    /// to load (no landmark blips, everything else intact).
    pois: Option<AreaPoiCatalog>,
    /// `SpellShapeshiftForm.dbc` — the tracking dots' creature-type override (a cat-form
    /// druid is a Beast; decision 0564). `None` = no override (unshifted resolution only).
    forms: Option<HashMap<u32, benilla_formats::ShapeshiftForm>>,
}

/// Async tile handles by ADT index, for the [`CurrentMap`] it was filled on. `None` = the tile has
/// no authored minimap art (open ocean) — cached so the translate lookup doesn't re-run per frame.
/// The interior half caches the WMO tiles by `(group, col, row)` for the WMO whose `stem` (its
/// `md5translate.trs` path stem) is resident, cleared when the player enters a different building.
#[derive(Resource, Default)]
struct MinimapTileCache {
    map_id: Option<u32>,
    tiles: HashMap<(u32, u32), Option<Handle<Image>>>,
    interior_stem: Option<String>,
    interior: HashMap<(usize, u32, u32), Option<Handle<Image>>>,
}

/// Loads the translate catalog + the mask/arrow art once the patch chain is open.
fn setup_minimap(
    mut commands: Commands,
    world_assets: Option<ResMut<WorldAssets>>,
    mut images: ResMut<Assets<Image>>,
) {
    let Some(mut assets) = world_assets else {
        return;
    };
    let translate = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_minimap_translate(&mut chain)
    };
    match translate {
        Ok(translate) => {
            info!("minimap: md5translate.trs — {} tiles", translate.len());
            let mask = assets.mask_texture("Textures\\MinimapMask", &mut images);
            let arrow = assets.sprite_texture("Interface\\Minimap\\MinimapArrow", &mut images);
            let poi = assets.sprite_texture("Interface\\Minimap\\POIIcons", &mut images);
            let landmark =
                assets.sprite_texture("Interface\\Minimap\\ROTATING-MINIMAPARROW", &mut images);
            let object_icons =
                assets.sprite_texture("Interface\\Minimap\\ObjectIcons", &mut images);
            let pois = {
                let mut chain = assets.chain.lock_recover();
                match benilla_formats::load_area_poi_catalog(&mut chain) {
                    Ok(cat) => Some(cat),
                    Err(e) => {
                        warn!("minimap: AreaPOI.dbc failed — no landmark blips: {e:#}");
                        None
                    }
                }
            };
            if mask.is_none() {
                warn!("minimap: MinimapMask.blp missing — the map will draw square");
            }
            let forms = {
                let mut chain = assets.chain.lock_recover();
                match benilla_formats::load_shapeshift_forms(&mut chain) {
                    Ok(f) => Some(f),
                    Err(e) => {
                        warn!("minimap: SpellShapeshiftForm.dbc failed — no shapeshift creature-type override: {e:#}");
                        None
                    }
                }
            };
            commands.insert_resource(MinimapAssets {
                translate,
                mask,
                arrow,
                poi,
                landmark,
                object_icons,
                pois,
                forms,
            });
        }
        Err(e) => error!("minimap: md5translate.trs failed, minimap disabled: {e:#}"),
    }
}

/// Fills the extracted widget hole: the visible tile quads (clipped to the widget, masked to the
/// circle) and the player arrow, appended at the widget's own z (stable sort keeps append order
/// within a key, so the arrow rides above the tiles and below the widget's children).
#[allow(clippy::too_many_arguments)]
fn emit_minimap(
    widget: Res<MinimapWidget>,
    assets: Option<Res<MinimapAssets>>,
    mut cache: ResMut<MinimapTileCache>,
    map: Option<Res<CurrentMap>>,
    catalog: Option<Res<MapCatalogRes>>,
    player: Res<Player>,
    lighting: Option<Res<crate::lighting::WowLighting>>,
    instances: Query<&WmoPortalInstance>,
    wmos: Res<Assets<WmoModel>>,
    streamer: Res<crate::terrain_stream::TerrainStreamer>,
    adt_tiles: Res<Assets<benilla_assets::AdtTile>>,
    asset_server: Res<AssetServer>,
    death_net: Res<crate::death::DeathNet>,
    blip_inputs: blips::BlipInputs,
    mut quads: ResMut<UiQuads>,
) {
    let (
        quest,
        guids,
        unit_pos,
        window,
        mut blip_hover,
        ui_scale,
        group,
        tracked,
        self_store,
        names,
        go_templates,
        locks,
    ) = blip_inputs;
    // Hover resets every frame; the blip pass below re-establishes it while the map draws.
    *blip_hover = blips::MinimapBlipHover::None;
    let (Some(slot), Some(assets), Some(map), Some(catalog)) =
        (widget.0.as_ref(), assets, map, catalog)
    else {
        return;
    };

    let side = slot.rect.width().min(slot.rect.height());
    if side <= 0.0 {
        return;
    }
    // `WOW_MM_ZOOM=0..5` forces the zoom level of whichever map is showing — a capture instrument
    // (pairs with the `WOW_MM_PROBE` interior probe). Indoors and outdoors each carry their own
    // persisted index, so the override stands in for both.
    let zoom_override = std::env::var("WOW_MM_ZOOM")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .map(|z| z.min(5));
    let zoom = zoom_override.unwrap_or(usize::from(slot.zoom.min(5)));
    let inside_zoom = zoom_override.unwrap_or(usize::from(slot.inside_zoom.min(5)));
    let center = (slot.rect.min + slot.rect.max) * 0.5;

    let wow = bevy_to_wow(player.pos);
    let (wx, wy) = (wow[0], wow[1]);
    // The active branch's world→px scale, for the point blips drawn after the tiles (the corpse
    // marker below; both branches share the same north-up point mapping around `center`).
    let mut blip_px_per_yd = 0.0_f32;

    let mask = assets.mask.as_ref().map(|m| UiQuadMask {
        texture: m.clone(),
        rect: slot.rect,
    });

    // The reference hard-switches map families on interior containment: standing inside a WMO group,
    // it draws that building's OWN minimap tiles and SUPPRESSES the terrain (mutually exclusive, not
    // a transparent overlay). Find the WMO the player is in via the same down-ray as the interior
    // audio/zone tracker (`wmo_portal`), plus its `md5translate.trs` path stem.
    let interior = (player.active && !player.detached).then(|| {
        let eye = player.pos + Vec3::Y * INTERIOR_PROBE_HEIGHT;
        // The down-ray races the terrain, exactly as the interior/zone tracker does — standing on the
        // grass above a mine's tunnels is not standing in the mine.
        let terrain = crate::terrain_stream::terrain_height_under(&streamer, &adt_tiles, eye);
        instances.iter().find_map(|inst| {
            let model = wmos.get(&inst.handle)?;
            if model.wmo_id == 0 {
                return None;
            }
            let local_from_world = inst.world_from_local.inverse();
            let eye_local = bevy_to_wow(local_from_world.transform_point3(eye));
            let terrain_local = terrain.map(|z| terrain_z_local(&local_from_world, eye, z));
            let in_group = down_ray_seeds(model, eye_local, terrain_local).in_group?;
            let stem = asset_server
                .get_path(inst.handle.id())
                .and_then(|p| wmo_minimap_stem(&p.path().to_string_lossy()))?;
            Some((inst.world_from_local, model, stem, in_group))
        })
    });

    // The player's containment verdict, kept as a bool for the quest-dot grey (the branch
    // below consumes `interior` itself).
    let player_indoors = matches!(&interior, Some(Some(_)));
    if let Some(Some((world_from_local, model, stem, in_group))) = interior {
        // INTERIOR: the WMO's own per-group tiles, drawn FULL WHITE (the day-night tint is outdoor-
        // only). The tiles are baked in the WMO's MODEL frame (north = model +X, sized to the model
        // footprint — verified against the 97°-yaw Goldshire Inn: group 3's tile is 64×32 px = its
        // model bbox, not the world AABB). So place each tile at its model-space centre mapped
        // through the placement, and rotate the WHOLE set by ONE placement-yaw angle — not per-tile
        // world AABBs (`wow-5875-re` minimap node Sub-Q6). Cached by `(group, col, row)`.
        if cache.interior_stem.as_deref() != Some(stem.as_str()) {
            cache.interior.clear();
            cache.interior_stem = Some(stem.clone());
        }
        // Black disc behind the (mostly-transparent) tiles — the reference clears the interior
        // minimap to black. Appended first (same z) so the stable sort keeps it under the tiles.
        quads.overlays.push(UiQuad {
            rect: slot.rect,
            z_key: slot.z,
            color: [0.0, 0.0, 0.0, slot.alpha],
            clip: Some(slot.rect),
            mask: mask.clone(),
            ..default()
        });

        // INTERIOR ZOOM: indoors has its OWN zoom index and its own table — the view radius is
        // `radiusTable[inside_zoom]` in raw yards (150 widest … 25 tightest), not the outdoor chunk
        // half-extent. The zoom buttons drive `inside_zoom` while you're inside, and it persists
        // separately from the outdoor level (wow-re finding 2 Q7 CORRECTION).
        let radius = INTERIOR_ZOOM_RADIUS[inside_zoom];
        let px_per_yd = (side * 0.5) / radius;
        blip_px_per_yd = px_per_yd;

        // A model point → its north-up minimap screen px (through the placement to world, then the
        // same world→screen map the terrain tiles use: screen up = world +X north, left = +Y west).
        let to_screen = |m: [f32; 3]| {
            let w = bevy_to_wow(world_from_local.transform_point3(wow_to_bevy(m)));
            Vec2::new(
                center.x + (wy - w[1]) * px_per_yd,
                center.y - (w[0] - wx) * px_per_yd,
            )
        };
        // The one placement rotation: where the model +X axis points on the screen (same per tile).
        let x_axis = to_screen([1.0, 0.0, 0.0]) - to_screen([0.0, 0.0, 0.0]);
        let rotation = x_axis.y.atan2(x_axis.x);

        // GROUP SELECTION: the portal flood-fill from the player's current group (wow-re
        // `wmo-interior-minimap.md` Sub-Q4b, byte-verified) — NOT draw-every-group. Only the groups
        // reached through portals within the query box, whose bbox overlaps the view in XY, are drawn.
        // This is what stops floors the player can't reach (or that are far outside the view) from
        // painting over the current one.
        // The selection query box uses the SAME `c` as the draw radius (Sub-Q4b) — so we never load
        // tiles we cannot show, and zooming in indoors tightens the box's Z extent too, which is what
        // trims how many stacked floors bleed through.
        let drawable =
            interior_group_selection(model, &world_from_local, player.pos, radius, in_group);
        // Draw ORDER (wow-re finding 2 Q2, VERIFIED): the composite is Z-sorted ascending by
        // `Zmidpoint − playerZ`, with the player's OWN group forced to the top (the client keys it
        // FLT_MAX). So floors below draw at the bottom, floors above over them, and the player's
        // current floor LAST of all — a stacked storey shows only through its transparent stairwell
        // gaps, never occluding the room you're in ("basement on top the kitchen", director).
        let player_z = bevy_to_wow(world_from_local.inverse().transform_point3(player.pos))[2];
        let sort_key = |gi: usize| -> f32 {
            if gi == in_group {
                f32::MAX
            } else {
                let gn = &model.group_nav[gi];
                0.5 * (gn.bbox_min[2] + gn.bbox_max[2]) - player_z
            }
        };
        let mut order: Vec<usize> = (0..model.group_nav.len())
            .filter(|&gi| drawable[gi])
            .collect();
        order.sort_by(|&a, &b| sort_key(a).total_cmp(&sort_key(b)));
        for gi in order {
            let gn = &model.group_nav[gi];
            let (nx, tw_x) = group_axis_grid(gn.bbox_max[0] - gn.bbox_min[0]);
            let (ny, tw_y) = group_axis_grid(gn.bbox_max[1] - gn.bbox_min[1]);
            let mid_z = 0.5 * (gn.bbox_min[2] + gn.bbox_max[2]);
            for col in 0..nx {
                for row in 0..ny {
                    let mcx = gn.bbox_min[0] + (col as f32 + 0.5) * tw_x;
                    let mcy = gn.bbox_min[1] + (row as f32 + 0.5) * tw_y;
                    let sc = to_screen([mcx, mcy, mid_z]);
                    // Window cull: skip tiles whose centre lands well outside the disc.
                    if sc.distance(center) > side * 0.5 + tw_x.max(tw_y) * px_per_yd {
                        continue;
                    }
                    let handle = cache.interior.entry((gi, col, row)).or_insert_with(|| {
                        let key = format!("{stem}_{gi:03}_{col:02}_{row:02}.blp");
                        assets.translate.get(&key).map(load_tile(&asset_server))
                    });
                    let Some(handle) = handle else {
                        continue; // this group cell has no authored tile
                    };
                    quads.overlays.push(UiQuad {
                        rect: Rect::from_center_size(
                            sc,
                            Vec2::new(tw_x * px_per_yd, tw_y * px_per_yd),
                        ),
                        z_key: slot.z,
                        texture: Some(handle.clone()),
                        color: [1.0, 1.0, 1.0, slot.alpha],
                        rotation,
                        mask: mask.clone(),
                        ..default()
                    });
                }
            }
        }
    } else if let Some(dir) = catalog.0.directory(map.0) {
        // OUTDOOR: the ADT terrain tiles, MODULATEd by the day-night light tint (not full white —
        // else too bright, the reference's CWorldFrame minimap draw). Absent lighting ⇒ white.
        let half_extent = ZOOM_CHUNKS[zoom] * 0.5 * CHUNK_YARDS;
        let px_per_yd = (side * 0.5) / half_extent;
        blip_px_per_yd = px_per_yd;
        if cache.map_id != Some(map.0) {
            cache.tiles.clear();
            cache.map_id = Some(map.0);
        }
        let tint = lighting
            .as_ref()
            .map(|l| minimap_day_tint(l.ambient, l.diffuse))
            .unwrap_or([1.0, 1.0, 1.0]);
        // World coords shrink as tile indices grow, so the view square's max-corner gives the low
        // indices. `world_to_tile` clamps to the 64×64 grid.
        let (tx_lo, ty_lo) = world_to_tile(wx + half_extent, wy + half_extent);
        let (tx_hi, ty_hi) = world_to_tile(wx - half_extent, wy - half_extent);
        for ty in ty_lo..=ty_hi {
            for tx in tx_lo..=tx_hi {
                let handle = cache.tiles.entry((tx, ty)).or_insert_with(|| {
                    assets
                        .translate
                        .tile(dir, tx, ty)
                        .map(load_tile(&asset_server))
                });
                let Some(handle) = handle else {
                    continue; // unauthored tile (open ocean) — the mask shows the clear color
                };
                // The tile's max-x/max-y world corner is its north-west corner = screen top-left.
                let (tile_north, tile_west) = tile_to_world(tx, ty);
                let left = center.x + (wy - tile_west) * px_per_yd;
                let top = center.y + (tile_north - wx) * -px_per_yd;
                let size = TILE_YARDS * px_per_yd;
                let rect = Rect::new(left, top, left + size, top + size);
                if rect.intersect(slot.rect).is_empty() {
                    continue;
                }
                quads.overlays.push(UiQuad {
                    rect,
                    z_key: slot.z,
                    texture: Some(handle.clone()),
                    color: [tint[0], tint[1], tint[2], slot.alpha],
                    clip: Some(slot.rect),
                    mask: mask.clone(),
                    ..default()
                });
            }
        }
    }

    // ── The blip layer (decision 0203 phase 3; byte law per the 0337 fold-back): landmarks
    // draw under the player arrow; the quest dots draw LAST — above it (the client's own draw
    // order). Hover lands in [`blips::MinimapBlipHover`] for the tooltip drive.
    // Our own descriptor's tracking state (PRIVATE fields — only ever on the self entity).
    let tracking = self_store
        .iter()
        .next()
        .map(|s| blips::SelfTracking {
            creatures: s.0.player_track_creatures(),
            resources: s.0.player_track_resources(),
            stealthed: s.0.player_track_stealthed(),
        })
        .unwrap_or_default();
    let blip_ctx = (blip_px_per_yd > 0.0).then(|| {
        if std::env::var("WOW_MM_BLIP_PROBE").is_ok() {
            eprintln!(
                "BLIP-PROBE: landmark_tex={} pois={} map={} wx={wx:.0} wy={wy:.0} px_per_yd={blip_px_per_yd:.3} track_c={:#x} track_r={:#x} track_s={}",
                assets.landmark.is_some(),
                assets.pois.as_ref().map(|c| c.len()).unwrap_or(0),
                map.0,
                tracking.creatures,
                tracking.resources,
                tracking.stealthed,
            );
        }
        let win = window.iter().next();
        let cursor = win.and_then(|w| w.cursor_position());
        blips::BlipCtx {
            center,
            side,
            px_per_yd: blip_px_per_yd,
            radius_yd: (side * 0.5) / blip_px_per_yd,
            z: slot.z,
            alpha: slot.alpha,
            wx,
            wy,
            wz: wow[2],
            cursor,
            // The same point in UI space (y-up, ÷s through the 0582/0584 seam — the tooltip's
            // anchor resolves in the VM's 768-virtual units, not window px): the cursor seat.
            cursor_ui: cursor.zip(win).map(|(c, w)| {
                let s = crate::ui_script::seam_scale(w.height(), ui_scale.0);
                Vec2::new(c.x / s, (w.height() - c.y) / s)
            }),
        }
    });
    let mut hover = blips::MinimapBlipHover::None;
    if let Some(ctx) = &blip_ctx {
        if let (Some(tex), Some(pois)) = (&assets.landmark, &assets.pois) {
            blips::emit_landmarks(
                ctx,
                pois,
                map.0,
                tex,
                assets.poi.as_ref(),
                &mut quads,
                &mut hover,
            );
        }
        // The party/corpse rim arrows (0434 phase 6b, `place_party_raid_blips`' out-of-range
        // half) draw with the POI arrows — before the player arrow, per the client's order.
        if let Some(arrow) = &assets.arrow {
            let corpse = death_net
                .corpse
                .filter(|cp| cp.display_map == map.0 as i32)
                .map(|cp| cp.position);
            blips::emit_party_arrows(ctx, &group, &guids, &unit_pos, corpse, arrow, &mut quads);
        }
    }

    // The player arrow: centered, spun to the facing. WoW orientation 0 = north (screen up),
    // growing counterclockwise (toward west = screen left); our quad rotation is clockwise on
    // screen, so the arrow angle is the negated facing.
    if let Some(arrow) = &assets.arrow {
        // Byte-pinned quad (blips::PLAYER_ARROW_QUAD_PX): the MinimapArrow.m2 single quad at
        // 1280 px/unit on the frozen 140.8 basis, its authored centre offset rotating with
        // the facing (clockwise screen rotation, so the offset rotates by the same angle).
        let s = side * (blips::PLAYER_ARROW_QUAD_PX / blips::BLIP_BASIS_PX);
        let rotation = -player.facing();
        let (sin, cos) = rotation.sin_cos();
        let off = blips::PLAYER_ARROW_OFFSET_PX * (side / blips::BLIP_BASIS_PX);
        let off = Vec2::new(off.x * cos - off.y * sin, off.x * sin + off.y * cos);
        let rect = Rect::from_center_size(center + off, Vec2::splat(s));
        quads.overlays.push(UiQuad {
            rect,
            z_key: slot.z,
            texture: Some(arrow.clone()),
            color: [1.0, 1.0, 1.0, slot.alpha],
            rotation,
            ..default()
        });
    }

    // The object dots draw LAST — above the player arrow (the client's draw order, 0x4ed7b7):
    // tracking dots (cells 0/1) first, then quest dots (cell 3) and party dots (cell 4) — the
    // draw's own cell-list order.
    if let Some(ctx) = &blip_ctx {
        if let Some(icons) = &assets.object_icons {
            blips::emit_tracking_dots(
                ctx,
                tracking,
                &tracked,
                quest.statuses(),
                &names,
                &go_templates,
                locks.as_deref().map(|l| &l.0),
                assets.forms.as_ref(),
                icons,
                player_indoors,
                |feet| {
                    crate::wmo_portal::indoors_at(
                        &wmos,
                        instances.iter(),
                        &streamer,
                        &adt_tiles,
                        feet,
                    )
                },
                &mut quads,
                &mut hover,
            );
            blips::emit_quest_dots(
                ctx,
                quest.statuses(),
                &guids,
                &unit_pos,
                icons,
                player_indoors,
                // A dot NPC's own containment — the same faces-only down-ray the entity light
                // classifier stands on (dots are few; the per-frame rays are cheap).
                |feet| {
                    crate::wmo_portal::indoors_at(
                        &wmos,
                        instances.iter(),
                        &streamer,
                        &adt_tiles,
                        feet,
                    )
                },
                &mut quads,
                &mut hover,
            );
            // The in-range party dots (blue cell 4, 1.3×) draw with the object dots — last.
            blips::emit_party_dots(ctx, &group, &guids, &unit_pos, icons, &mut quads);
        }
    }
    *blip_hover = hover;

    // The corpse blip (decision 0308 §5): in range, the POIIcons skull cell (the same art the
    // ref's world-map corpse uses; the engine-drawn in-range minimap corpse art is INTERIM until
    // named) at the corpse's true position, through the same north-up point mapping as the
    // tiles. OUT of range the corpse is the fifth `place_party_raid_blips` slot — the rotating
    // rim arrow drawn with the party arrows above (the byte law replaced the old 0.92
    // edge-clamped skull). Same-map only (the display coords are the entrance for a dungeon
    // corpse).
    if let (Some(poi), Some(cp)) = (&assets.poi, death_net.corpse) {
        if cp.display_map == map.0 as i32 && blip_px_per_yd > 0.0 {
            let off = Vec2::new(
                (wy - cp.position[1]) * blip_px_per_yd,
                -(cp.position[0] - wx) * blip_px_per_yd,
            );
            if off.length() <= side * 0.5 * 0.8 {
                let s = side * CORPSE_BLIP_FRACTION;
                let rect = Rect::from_center_size(center + off, Vec2::splat(s));
                quads.overlays.push(UiQuad {
                    rect,
                    z_key: slot.z,
                    texture: Some(poi.clone()),
                    uv: crate::ui_pass::UvRect::from_tex_coords([0.875, 1.0, 0.0, 0.125]),
                    color: [1.0, 1.0, 1.0, slot.alpha],
                    ..default()
                });
            }
        }
    }
}

/// Push the player's WMO-containment state onto the Minimap widget (the client's `0xceaa60`), so the
/// zoom buttons drive the **indoor** zoom index while indoors and the outdoor one while outside, each
/// persisting across the transition. Runs before the script tick, so a `SetZoom` fired from a button
/// handler this frame routes to the right index. `CurrentWmoInterior` is the same containment test the
/// interior audio/zone tracker uses.
/// The state push is unconditional (a Minimap widget created *after* a transition — the cluster XML
/// loads late — would otherwise never be told, and the arena scan is nothing next to a frame). On the
/// actual inside↔outside *edge* we also fire `MINIMAP_UPDATE_ZOOM`: the active zoom index just switched
/// to the other (independent) level, so the cluster must re-sync the +/- buttons' enabled state to it
/// — the client's own signal for "the effective zoom changed" (FrameXML `Minimap_OnEvent`). Without it
/// the buttons keep the level you left (e.g. `ZoomIn` greyed from an outdoor max-zoom, still greyed
/// indoors at level 3), which is the director's report (2026-07-09).
fn feed_minimap_inside(
    script: Option<bevy::ecs::system::NonSendMut<benilla_ui::script::UiScript>>,
    interior: Res<crate::wmo_portal::CurrentWmoInterior>,
    mut was_inside: Local<Option<bool>>,
) {
    let Some(mut script) = script else { return };
    let inside = interior.0.is_some();
    script.set_minimap_inside(inside);
    if *was_inside != Some(inside) {
        *was_inside = Some(inside);
        script.fire_event("MINIMAP_UPDATE_ZOOM", vec![]);
    }
}

/// Push the live game clock into the VM when the game minute ticks — `GetGameTime()`'s backing
/// globals (the zone-text family's shape). The reference's GameTimeFrame re-reads GetGameTime
/// every OnUpdate and compares the packed minute against its cached `timeOfDay`, so a
/// minute-granular push is exactly the API's own resolution (the binding returns no seconds).
/// Before the first `SMSG_LOGIN_SETTIMESPEED` the globals stay at their 0:00 stdlib seed.
fn feed_game_time(
    script: Option<bevy::ecs::system::NonSendMut<benilla_ui::script::UiScript>>,
    time: Res<crate::net::ServerTime>,
    mut last: Local<Option<u32>>,
) {
    let Some(script) = script else { return };
    let Some(gt) = time.0 else { return };
    let minute = gt.minute_of_day();
    if *last == Some(minute) {
        return;
    }
    *last = Some(minute);
    let globals = script.lua().globals();
    let pushed = globals
        .set("__benilla_game_hour", minute / 60)
        .and_then(|()| globals.set("__benilla_game_minute", minute % 60));
    if let Err(e) = pushed {
        warn!("minimap: game-time globals: {e}");
    }
}

/// The app half of the `<Minimap>` widget (decision 0203 phase 1) — see the module doc. The zone
/// LABEL feed lives with the rest of the zone-text data plane (`crate::area`, decision 0287's
/// fold-back): the client updates the minimap line and fires `MINIMAP_ZONE_CHANGED` from the same
/// area-update pass as the ZONE_CHANGED family (`0x494970` beside `0x494780`), and so does
/// benilla.
pub(crate) struct MinimapPlugin;

impl Plugin for MinimapPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MinimapWidget>()
            .init_resource::<MinimapTileCache>()
            .init_resource::<blips::MinimapBlipHover>()
            .add_systems(Startup, setup_minimap.after(AssetSet::Open))
            .add_systems(
                Update,
                (
                    emit_minimap.in_set(UiQuadAppend),
                    // Before the script tick, so a zoom button pressed this frame routes to the
                    // indoor/outdoor index that matches where the player actually is.
                    feed_minimap_inside.before(crate::ui_script::UiInput),
                    // Before the script tick, so GameTimeFrame's OnUpdate reads this frame's
                    // minute, not last frame's.
                    feed_game_time.before(crate::ui_script::UiInput),
                    // After the world-mouseover drive (UnitFeed): a same-frame world-hover→blip
                    // transition must end with the blip tooltip shown, not the fade.
                    blips::drive_blip_tooltip
                        .after(crate::ui_unit::UnitFeed)
                        .before(crate::ui_script::UiInput),
                ),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::minimap_day_tint;

    fn approx(a: [f32; 3], b: [f32; 3]) {
        for c in 0..3 {
            assert!((a[c] - b[c]).abs() < 0.01, "{a:?} vs {b:?} @ {c}");
        }
    }

    #[test]
    fn full_white_light_leaves_tiles_at_full_brightness() {
        // Both bands white ⇒ tint white ⇒ the tile draws verbatim (the noon-ish bright case).
        approx(minimap_day_tint([1.0; 3], [1.0; 3]), [1.0; 3]);
    }

    #[test]
    fn default_light_dims_tiles_below_white() {
        // The client's no-light default: ambient = gray 0x40 (≈0.251), diffuse = white. The tint is
        // ≈0.79, NOT 1.0 — i.e. drawing tiles at flat white is ≈1.27× too bright (the director's
        // report). 0.25·1 + 0.75·lerp(0.251, 1, (64+96)/256=0.625) = 0.25 + 0.75·0.719 ≈ 0.789.
        let t = minimap_day_tint([0.251; 3], [1.0; 3]);
        approx(t, [0.789; 3]);
        assert!(t[0] < 0.95, "flat white would be too bright");
    }

    #[test]
    fn pitch_black_light_still_tints_partway_to_white() {
        // The +96 luma floor keeps the map dimly visible even with zero light: 0.75·(0+1·0.375) ≈ 0.28.
        approx(minimap_day_tint([0.0; 3], [0.0; 3]), [0.281; 3]);
    }
}
