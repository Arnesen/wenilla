//! The world-map data feed (decision 0203 phase 2) — the app half behind
//! `assets/ui/WorldMapFrame.xml` and benilla-ui's `script/worldmap.rs` bindings.
//!
//! Two systems, the quest-log seam shape:
//! - [`load_world_map_ui`] (once, when the chain + VM + Map.dbc catalog all exist): builds the
//!   static **catalog** from `WorldMapArea` × `AreaTable` × `WorldMapContinent` × `Map` ×
//!   the `.zmp` bitmaps and pushes it into the engine. Every ordering/naming rule is the
//!   wow-re-verified one (Q1/Q3 verdicts, 2026-07-07): continents in WorldMapArea **file
//!   order** (Kalimdor, then EK — the `0x4a5d00` builder's walk), displayed under their
//!   `Map.dbc` localized names ("Eastern Kingdoms", not the art folder's "Azeroth"); zones
//!   sorted case-insensitively by AreaTable localized name (the `0x4a6390` comparator's
//!   `SStrCmpI`); the zone grids remapped from raw AreaTable ids by the client's one-hop parent
//!   rollup + (mapId, areaId) match — here straight to 1-based zone indices.
//! - [`feed_world_map`] (every frame): reads the engine-owned selection back, projects the
//!   player's world position onto the displayed map via [`benilla_world::map_proj`] (world sheet:
//!   WorldMapContinent constants; continent/zone: the WorldMapArea rect lerp), resolves the
//!   player's `(continent, zone)` from `CurrentArea` through the AreaTable parent chain, and
//!   pushes the trio + facing. (The client matches its zone-level area global directly —
//!   `0x4a6650`; our MCNK `CurrentArea` is the leaf sub-area, so the parent walk lands on the
//!   same zone.)
//!
//! …and one dev affordance beside them, [`dev_map_jump`]: **Alt+click the map to go there.**

use bevy::ecs::system::NonSendMut;
use bevy::prelude::*;

use benilla_assets::coords::bevy_to_wow;
use benilla_formats::{
    load_world_map_area_catalog, load_world_map_continent_catalog, load_world_map_overlay_catalog,
    load_zone_map, WorldMapArea,
};
use benilla_ui::script::{UiScript, WorldMapContinentView, WorldMapOverlayView, WorldMapZoneView};

use crate::net::{ObjectStore, SelfPlayer};
use crate::player::Player;
use crate::ui_script::UiInput;
use benilla_assets::MapCatalogRes;
use benilla_assets::{LockRecover, WorldAssets};
use benilla_world::map_proj::{self, WorldProj, ZoneRect};
use benilla_world::world_map::CurrentMap;

/// The app-side mirror of the pushed catalog — the projection data (rects + world-sheet
/// constants) per continent/zone, in the SAME order as the engine's copy (indices must agree).
/// The AreaTable itself is the shared [`crate::area::AreaTableRes`] (decision 0287).
#[derive(Resource)]
struct WorldMapUiData {
    continents: Vec<ContinentEntry>,
}

struct ContinentEntry {
    map_id: u32,
    proj: Option<WorldProj>,
    rect: ZoneRect,
    zones: Vec<ZoneEntry>,
}

struct ZoneEntry {
    area_id: u32,
    rect: ZoneRect,
}

fn zone_rect(a: &WorldMapArea) -> ZoneRect {
    ZoneRect {
        left: a.loc_left,
        right: a.loc_right,
        top: a.loc_top,
        bottom: a.loc_bottom,
    }
}

/// Build + push the static catalog once the patch chain, the VM, and the Map.dbc catalog all
/// exist (Update-gated rather than Startup-ordered, like every feed that needs the script).
fn load_world_map_ui(
    mut done: Local<crate::ui_script::VmMemo<bool>>,
    script: Option<NonSendMut<UiScript>>,
    world_assets: Option<ResMut<WorldAssets>>,
    maps: Option<Res<MapCatalogRes>>,
    areas: Option<Res<crate::area::AreaTableRes>>,
    mut commands: Commands,
) {
    let (Some(mut script), Some(assets), Some(maps), Some(areas)) =
        (script, world_assets, maps, areas)
    else {
        return;
    };
    // Once per **VM** (1290), not once per process: the catalog is static, the VM it is pushed
    // into is not — a login builds a fresh one, and without this the map window has no continents.
    if !done.claim(&script) {
        return;
    }
    let areas = &areas.0;

    let mut chain = assets.chain.lock_recover();
    let loaded = load_world_map_area_catalog(&mut chain).and_then(|wma| {
        let wmc = load_world_map_continent_catalog(&mut chain)?;
        let wmo = load_world_map_overlay_catalog(&mut chain)?;
        Ok((wma, wmc, wmo))
    });
    let (wma, wmc, wmo) = match loaded {
        Ok(t) => t,
        Err(e) => {
            error!("world map: DBC load failed, map window disabled: {e:#}");
            return;
        }
    };

    // Continents: the areaId==0 rows in WorldMapArea FILE order — the 0x4a5d00 builder's walk,
    // which defines the Lua continent index (Kalimdor, then EK in 5875; wow-re Q1(d) verdict).
    let cont_rows: Vec<(u32, &WorldMapArea)> = wma.iter().filter(|(_, a)| a.area_id == 0).collect();

    let mut entries = Vec::with_capacity(cont_rows.len());
    let mut views = Vec::with_capacity(cont_rows.len());
    for (_, cont) in cont_rows {
        // Zones: this continent's areaId!=0 rows, display-named from AreaTable, sorted
        // case-insensitively by that name (the 0x4a6390 comparator's SStrCmpI — this order IS
        // the Lua zone index). Instance rows (BGs) live on other map ids and drop out naturally.
        let mut zones: Vec<(u32, &WorldMapArea, String)> = wma
            .iter()
            .filter(|(_, a)| a.map_id == cont.map_id && a.area_id != 0)
            .map(|(id, a)| {
                let name = areas.name(a.area_id).unwrap_or(a.name.as_str()).to_string();
                (id, a, name)
            })
            .collect();
        zones.sort_by_key(|(_, _, name)| name.to_lowercase());

        let proj = wmc.get(cont.map_id).map(|c| WorldProj {
            offset_u: c.offset_x,
            offset_v: c.offset_y,
            scale: c.scale,
        });
        // The continent's rect on the world sheet: the 0x4a5d00 kernel over the WorldMapContinent
        // tile bounds (the world-level click's AABB — disjoint per continent, unlike the art
        // rects).
        let world_rect = wmc
            .get(cont.map_id)
            .zip(proj)
            .map(|(c, p)| {
                map_proj::continent_sheet_rect(
                    (
                        c.left_boundary,
                        c.right_boundary,
                        c.top_boundary,
                        c.bottom_boundary,
                    ),
                    p,
                )
            })
            .unwrap_or((0.0, 0.0, 0.0, 0.0));
        let rect = zone_rect(cont);

        // The continent's area bitmap, remapped from raw AreaTable ids to 1-based zone indices —
        // the client's load-time remap (one-hop parent rollup, then the (mapId, areaId) match;
        // wow-re Q1(b)), fused with its zone-index resolution since our engine consumes indices.
        let zone_grid: Vec<u16> = match load_zone_map(&mut chain, &cont.name) {
            Ok(grid) => grid
                .iter()
                .map(|&raw| {
                    let mut area_id = raw;
                    if let Some(row) = areas.get(area_id) {
                        if row.zone_id != 0 {
                            area_id = row.zone_id; // one-hop rollup, verbatim (not a full walk)
                        }
                    }
                    zones
                        .iter()
                        .position(|(_, a, _)| a.area_id == area_id)
                        .map(|i| i as u16 + 1)
                        .unwrap_or(0)
                })
                .collect(),
            Err(e) => {
                // The client tolerates a missing bitmap (grid stays zero → hover/click inert).
                warn!("world map: no zone bitmap for {}: {e:#}", cont.name);
                Vec::new()
            }
        };

        views.push(WorldMapContinentView {
            // Display name = Map.dbc's localized MapName ("Eastern Kingdoms"), never the art
            // folder (wow-re Q3(a)); the folder stays the map_file.
            name: maps
                .0
                .name(cont.map_id)
                .unwrap_or(cont.name.as_str())
                .to_string(),
            map_file: cont.name.clone(),
            world_rect,
            loc_rect: (cont.loc_left, cont.loc_right, cont.loc_top, cont.loc_bottom),
            zone_grid,
            zones: zones
                .iter()
                .map(|(wma_id, a, name)| WorldMapZoneView {
                    name: name.clone(),
                    area_id: a.area_id,
                    map_file: a.name.clone(),
                    loc_rect: (a.loc_left, a.loc_right, a.loc_top, a.loc_bottom),
                    // The zone's discovery overlays: WorldMapOverlay rows joined by the zone's
                    // WMA id, their reveal bits = each covered area's AreaTable exploreFlag.
                    overlays: wmo
                        .for_area(*wma_id)
                        .iter()
                        .map(|o| WorldMapOverlayView {
                            texture: format!("Interface\\WorldMap\\{}\\{}", a.name, o.texture_name),
                            width: o.texture_width,
                            height: o.texture_height,
                            offset_x: o.offset_x,
                            offset_y: o.offset_y,
                            explore_bits: o
                                .area_id
                                .iter()
                                .filter(|&&aid| aid != 0)
                                .filter_map(|&aid| areas.get(aid).map(|r| r.explore_flag))
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
        });
        entries.push(ContinentEntry {
            map_id: cont.map_id,
            proj,
            rect,
            zones: zones
                .iter()
                .map(|(_, a, _)| ZoneEntry {
                    area_id: a.area_id,
                    rect: zone_rect(a),
                })
                .collect(),
        });
    }
    drop(chain);

    info!(
        "world map: catalog — {} continents, {} zones",
        views.len(),
        views.iter().map(|c| c.zones.len()).sum::<usize>()
    );
    script.set_world_map_catalog(views);
    commands.insert_resource(WorldMapUiData {
        continents: entries,
    });
}

/// Per frame: selection read-back → projection → feed push (see the module doc).
#[allow(clippy::too_many_arguments)]
fn feed_world_map(
    script: Option<NonSendMut<UiScript>>,
    data: Option<Res<WorldMapUiData>>,
    player: Res<Player>,
    map: Option<Res<CurrentMap>>,
    world: benilla_world::world_point::WorldPoint,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    areas: Option<Res<crate::area::AreaTableRes>>,
    death_net: Res<crate::death::DeathNet>,
    mut last_explored: Local<crate::ui_script::VmMemo<Option<Vec<u32>>>>,
) {
    let (Some(mut script), Some(data), Some(map), Some(areas)) = (script, data, map, areas) else {
        return;
    };
    let last_explored = last_explored.get(&script);

    // The discovery bitset (PLAYER_EXPLORED_ZONES, PRIVATE — only our own avatar carries it):
    // push on change (including the first stream-in); the engine setter queues the
    // WORLD_MAP_UPDATE that fills newly explored art into an open zone map.
    if let Some(store) = self_q.iter().next() {
        let explored: Vec<u32> = (0..benilla_protocol::messages::PLAYER_EXPLORED_ZONES_SLOTS)
            .map(|i| store.0.player_explored_zone_slot(i))
            .collect();
        if last_explored.as_ref() != Some(&explored) {
            *last_explored = Some(explored.clone());
            script.set_world_map_explored(explored);
        }
    }
    let wow = bevy_to_wow(player.pos);
    let (wx, wy) = (wow[0], wow[1]);

    // The player's (continent, zone) — CurrentArea's MCNK areaId walked up the AreaTable parent
    // chain to its top-level zone, matched against the displayed catalog (1-based indices).
    let player_zone = world
        .area()
        .and_then(|aid| areas.0.top_zone(aid))
        .and_then(|top| {
            data.continents.iter().enumerate().find_map(|(ci, cont)| {
                if cont.map_id != map.0 {
                    return None;
                }
                let zi = cont.zones.iter().position(|z| z.area_id == top)?;
                Some((ci as u32 + 1, zi as u32 + 1))
            })
        });

    // The player's UV on the DISPLAYED map. Off-map (wrong continent, outside the rect, or an
    // instance map) resolves to None/(0,0) — the reference's hide-the-blip sentinel.
    let (c, z) = script.world_map_selection();
    // One projection law for every blip on the displayed map (the player now, the corpse below):
    // world-sheet mode projects through the POSITION's own map's continent constants; zone mode
    // through the selected rect, gated to that continent's map. Off-map → None → the (0,0) hide.
    let project = |pos_map: u32, px: f32, py: f32| match (c, z) {
        (0, _) => data
            .continents
            .iter()
            .find(|cont| cont.map_id == pos_map)
            .and_then(|cont| cont.proj)
            .map(|p| map_proj::world_uv(p, px, py)),
        (c, z) => data
            .continents
            .get(c as usize - 1)
            .filter(|cont| cont.map_id == pos_map)
            .and_then(|cont| match z {
                0 => Some(cont.rect),
                z => cont.zones.get(z as usize - 1).map(|zone| zone.rect),
            })
            .map(|rect| map_proj::zone_uv(rect, px, py)),
    };
    let uv = project(map.0, wx, wy);
    // The corpse marker (decision 0308 §5): the query answer's DISPLAY position/map (a dungeon
    // corpse projects at its entrance — the server rewrote it). `zone_uv`'s outside-the-rect
    // (0,0) and the None here both land on the reference's hide sentinel.
    let corpse_uv = death_net.corpse.and_then(|cp| {
        project(
            u32::try_from(cp.display_map).unwrap_or(u32::MAX),
            cp.position[0],
            cp.position[1],
        )
    });

    script.set_world_map_feed(player_zone, uv, player.facing(), corpse_uv);
}

/// **Alt+click the world map to go there** — the dev jump.
///
/// The inverse of the blip projection above: the click's UV inside the map art
/// ([`UiScript::world_map_uv_at`], the reference's own normalization) run back through the
/// displayed rect to a world `(x, y)`, sent as vmangos's `.go xy x y <mapid>` — the **no-Z** form,
/// so the server resolves the ground (`GetWaterOrGroundLevel`) instead of us guessing a height off
/// a 2-D sheet. A jump to another continent's sheet is a cross-map worldport, handled like any
/// other ([`crate::player::wire_in`]).
///
/// **Zone and continent sheets only.** At the *world* level (both continents on one sheet) the
/// client's own UV→world law `0x4a7100` is not the inverse of its world→UV law — a confirmed,
/// reproduced anomaly (wow-re Q2 verdict; see [`map_proj::world_click_world`]) — so a click there
/// would land somewhere real but wrong by ~1.33× the sheet offset. Rather than invent a corrected
/// inverse the reference doesn't have, the jump declines and says to zoom in first.
///
/// The click is NOT consumed: the faithful path (`WorldMapButton_OnClick` → `ProcessMapClick`)
/// also runs and drills into the clicked zone, which is what you want anyway — you arrive, and the
/// map is showing where you arrived. Adding a modifier fork to the reference's click law to
/// suppress that would be a dev affordance rewriting a faithful one.
#[allow(clippy::too_many_arguments)]
fn dev_map_jump(
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    ui_scale: Res<crate::ui_script::UiScaleCvar>,
    script: Option<NonSendMut<UiScript>>,
    data: Option<Res<WorldMapUiData>>,
    net: Res<crate::net::NetCommands>,
) {
    // A dev affordance living in a gameplay module — it names no dev root, so nothing about it
    // fails to compile in a player build, and it shipped in 1174's (decision 1179). Alt-click is
    // free-fly's closest sibling: it moves the player's body across the continent.
    if !crate::run_mode::dev_affordances() {
        return;
    }
    let alt = keys.any_pressed([KeyCode::AltLeft, KeyCode::AltRight]);
    if !alt || !buttons.just_pressed(MouseButton::Left) {
        return;
    }
    let (Some(script), Some(data), Ok(window)) = (script, data, windows.single()) else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    // Window px (y-down) → the VM's y-up 768-virtual units, exactly as the pointer feed converts
    // them (`ui_script::input`) — `world_map_uv_at` hit-tests in that space.
    let s = crate::ui_script::seam_scale(window.height(), ui_scale.0);
    let Some((u, v)) = script.world_map_uv_at(cursor.x / s, (window.height() - cursor.y) / s)
    else {
        return;
    };
    let (c, z) = script.world_map_selection();
    let Some(cont) = c
        .checked_sub(1)
        .and_then(|i| data.continents.get(i as usize))
    else {
        info!(
            "map-jump: no exact click→world law at the world level — zoom into a continent first"
        );
        return;
    };
    let rect = match z.checked_sub(1) {
        None => cont.rect,
        Some(i) => match cont.zones.get(i as usize) {
            Some(zone) => zone.rect,
            None => return,
        },
    };
    // `zone_world` lerps BOTH axes by its single `t` (the binary's own shape) — so it is called
    // once per axis, which is how the reference's own callers consume it.
    let (_, wy) = map_proj::zone_world(rect, u);
    let (wx, _) = map_proj::zone_world(rect, v);
    let text = format!(".go xy {wx:.2} {wy:.2} {}", cont.map_id);
    info!("map-jump: {text}");
    let _ = net.0.send(crate::net::ClientCommand::Chat {
        kind: crate::net::ChatKind::Say,
        target: None,
        text,
    });
}

/// The world-map data feed (decision 0203 phase 2) — see the module doc.
pub(crate) struct WorldMapUiPlugin;

impl Plugin for WorldMapUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                load_world_map_ui,
                // After the script tick (UiInput), like the minimap's zone feed: the projection
                // for a selection changed THIS tick lands next tick — invisible at frame rate.
                feed_world_map.after(UiInput),
                // Same slot, and for the same reason from the other side: the jump hit-tests
                // against the frame rects THIS tick's resolve produced.
                dev_map_jump.after(UiInput),
            ),
        );
    }
}
