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
//!   same zone.) It also runs the **landmark pass** — the reference's `0x4a67a0` builder
//!   ([`landmark_gates_pass`], decision 1586): the `AreaPOI.dbc` rows the displayed level admits,
//!   then the guard-directions marker. That pass is keyed rather than per-frame ([`LandmarkKey`]),
//!   because the reference rebuilds it on events, not on a clock.
//!
//! …and one dev affordance beside them, [`dev_map_jump`]: **Alt+click the map to go there.**

use bevy::ecs::system::NonSendMut;
use bevy::prelude::*;

use benilla_assets::coords::bevy_to_wow;
use benilla_formats::{
    load_world_map_area_catalog, load_world_map_continent_catalog, load_world_map_overlay_catalog,
    load_zone_map, WorldMapArea,
};
use benilla_ui::script::{
    UiScript, WorldMapContinentView, WorldMapLandmarkView, WorldMapOverlayView, WorldMapZoneView,
};

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

/// Which of the map's three levels is displayed — the reference's `(continent, zone)` globals
/// `[0x84506c]`/`[0x845070]` reduced to the thing every landmark gate actually branches on.
///
/// The reference has a fourth, `ORPHAN` (`continent == -2`, a map selected directly rather than
/// through the continent/zone pair). We have no way to reach it: [`UiScript::world_map_selection`]
/// is a `(u32, u32)` pair with `0` for "whole", so a direct-area selection is unrepresentable.
/// Where the reference's gates test "zone **or** orphan", ours test zone — the same answer for
/// every state our map can be in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MapLevel {
    /// Both continents on one sheet (the reference's `continent == -1`).
    World,
    /// One continent, no zone selected (`continent >= 0, zone == -1`).
    Continent,
    /// One zone (`continent >= 0, zone >= 0`).
    Zone,
}

impl MapLevel {
    /// Our 1-based `(continent, zone)` selection, `0` = "whole", mapped onto the reference's
    /// `-1`-for-whole pair.
    fn of(continent: u32, zone: u32) -> Self {
        match (continent, zone) {
            (0, _) => Self::World,
            (_, 0) => Self::Continent,
            _ => Self::Zone,
        }
    }
}

/// The inputs the landmark list is a pure function of — the rebuild edge.
///
/// The reference does not rebuild per frame: `0x4a67a0` runs from thirteen callers, and they are
/// exactly this set — a map selection change, a world-state push (`0x48fa0d`), a
/// `PLAYER_EXPLORED_ZONES` descriptor change (`0x4a6477`, registered as an UpdateFields callback
/// on byte offset `0xe6c`), and the three gossip-marker set/clear sites. Keying on them keeps our
/// pass event-driven the same way, which is what stops a 339-row walk and ~40 string clones from
/// running 60 times a second with nothing changed.
///
/// `explored` is held whole rather than hashed: it is 64 dwords, it is compared far more often
/// than it is stored, and a hash would trade an exact answer for a collision risk on the one
/// input that silently hides map icons when it is wrong.
#[derive(PartialEq, Eq)]
struct LandmarkKey {
    selection: (u32, u32),
    map: u32,
    states: u64,
    /// Owned rather than borrowed, and compared before it is ever cloned — see
    /// [`LandmarkKey::matches`]. Cloning it every frame to build a throwaway key would have cost
    /// the allocation this whole mechanism exists to avoid.
    explored: Vec<u32>,
    /// The guard-directions marker's identity — `(continent, pos, icon)`. Its position is held as
    /// raw `f32` bits: "the same marker" here means the same wire packet, and the coordinates are
    /// copied from it rather than computed, so a bit compare is the exact question.
    marker: Option<(u32, [u32; 3], u32)>,
}

impl LandmarkKey {
    /// Is this key still the current one? Takes `explored` by slice so the common case — nothing
    /// changed — compares without allocating.
    fn matches(
        &self,
        selection: (u32, u32),
        map: u32,
        states: u64,
        explored: &[u32],
        marker: Option<(u32, [u32; 3], u32)>,
    ) -> bool {
        self.selection == selection
            && self.map == map
            && self.states == states
            && self.marker == marker
            && self.explored == explored
    }
}

/// The builder's near-zero skip (`0x4a6868`/`0x4a687a`): a landmark whose projected UV is `0` on
/// **both** axes is dropped.
///
/// That is the level/continent filter, not a paranoia check. [`map_proj::zone_uv`] answers
/// `(0, 0)` for a position outside the displayed rect or on the wrong continent, and `world_uv`'s
/// caller answers `None` for a map with no `WorldMapContinent` row — so "did it project to zero"
/// IS "does this POI belong on the map that is showing". Both axes, never either: a POI genuinely
/// on the top-left edge of a rect (`u = 0`, `v = 0.5`) survives, as it does in the reference.
///
/// The epsilon is the binary's own `2.384e-7`.
fn is_degenerate(uv: (f32, f32)) -> bool {
    const EPS: f32 = 2.384e-7;
    uv.0.abs() < EPS && uv.1.abs() < EPS
}

/// The three gates `0x4a67a0`'s AreaPOI walk applies, in the reference's order (wow-re
/// `system/ui/scratch/gossip-poi-marker.md` §7 + §8.2).
///
/// 1. **The level flag** (`0x4a79b0`'s fall-through chain, `0x4a79cb`–`0x4a7a05`): a row must
///    carry `0x04` to show at zone level, `0x08` at continent level, and **both `0x10` and
///    `0x08`** at world level — the chain is not a switch, so the world leg falls through the
///    continent leg. The shipped data agrees and could have disagreed: of the eleven `Flags`
///    values in `AreaPOI.dbc`, the only two carrying `0x10` (`0x1d`, `0x98`) also carry `0x08`.
/// 2. **Exploration** (`0x4a6890`–`0x4a68f3`): a row whose `AreaID` is `> 0` (signed — `-1`
///    arrives here as `u32::MAX` and means continent-wide) must resolve in `AreaTable`, and if
///    that row's `ExplorationLevel` is `>= 0` the player must have discovered it. This is why
///    Stormwind's icon is on the map from the first login (its row is continent-wide) while a
///    tower in a zone you have never walked is not.
/// 3. **World state** (`0x4a6903`): a row carrying a `WorldStateID` shows only while
///    [`crate::world_state::WorldStates`] reads that key non-zero. This is the whole Eastern
///    Plaguelands tower mechanism — rows 1749–1777 are gated on states 2352–2379, one row per
///    (tower, owner, contested/progressing) combination, so the server flipping a state swaps
///    which icon exists rather than editing one.
///
/// **Not tested here, and deliberately:** `ContinentID`, `Importance`, `FactionID`, `Icon`. The
/// reference's loop reads none of them — the continent filter is the projection (see
/// [`is_degenerate`]), and `Importance` gates nothing on the world map at all.
fn landmark_gates_pass(
    poi: &benilla_formats::AreaPoi,
    level: MapLevel,
    areas: &benilla_formats::AreaTableCatalog,
    explored: &[u32],
    world_states: &crate::world_state::WorldStates,
) -> bool {
    let level_ok = match level {
        MapLevel::Zone => poi.flags & 0x04 != 0,
        MapLevel::Continent => poi.flags & 0x08 != 0,
        MapLevel::World => poi.flags & 0x10 != 0 && poi.flags & 0x08 != 0,
    };
    if !level_ok {
        return false;
    }
    // `AreaID > 0`, signed. A row the table does not carry cannot be tested for exploration, so
    // it passes — the reference's own bounds check on `[0xc0e048]`/`[0xc0e04c]` guards the read,
    // not the landmark. (Moot on 5875 data: all 171 gated rows resolve.)
    if poi.area_id as i32 > 0 {
        if let Some(area) = areas.get(poi.area_id) {
            if area.exploration_level >= 0 && !explored_bit(explored, area.explore_flag) {
                return false;
            }
        }
    }
    poi.world_state_id == 0 || world_states.get(poi.world_state_id) != 0
}

/// `PLAYER_EXPLORED_ZONES` bit `bit` — the reference's `0x4a9a40`, which indexes the descriptor
/// bitfield bytewise (`[base + bit/8] & (1 << (bit & 7))`). Our slots are the same bits as
/// little-endian dwords, so the dword form below is the same test.
fn explored_bit(explored: &[u32], bit: u32) -> bool {
    let (slot, within) = (bit as usize / 32, bit % 32);
    explored.get(slot).is_some_and(|w| w & (1 << within) != 0)
}

/// `GetMapLandmarkInfo`'s `textureIndex` leg (`0x4a8848`–`0x4a8877`): the row's own `Icon`, except
/// that at **zone** level a row without `Flags & 0x80` is substituted with the constant **15**.
///
/// The substitution and the world-state gate are one designed mechanism, and the shipped data
/// proves it: across all 339 rows, `Flags & 0x80` is set exactly when `WorldStateID != 0`. A
/// battleground node or an Eastern Plaguelands tower whose icon *means* something — Alliance vs
/// Horde vs contested — has to escape a substitution that would flatten all of them to one cell.
fn landmark_texture_index(poi: &benilla_formats::AreaPoi, level: MapLevel) -> u32 {
    /// The generic zone-level POI cell of `Interface\Minimap\POIIcons`.
    const ZONE_SUBSTITUTE: u32 = 15;
    match poi.flags & 0x80 != 0 || level != MapLevel::Zone {
        true => poi.icon,
        false => ZONE_SUBSTITUTE,
    }
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
    poi_marker: Res<crate::poi_marker::PoiMarker>,
    // The party blips' two position sources (B320): the roster + the wire's per-member stats, and
    // the streamed-entity index that beats them when a member is actually in the world with us.
    group: Res<crate::ui_party::GroupState>,
    guids: Res<crate::net::GuidIndex>,
    unit_pos: Query<&GlobalTransform, With<crate::net::NetEntity>>,
    pois: Option<Res<crate::area_poi::AreaPoiRes>>,
    world_states: Res<crate::world_state::WorldStates>,
    mut last_explored: Local<crate::ui_script::VmMemo<Option<Vec<u32>>>>,
    mut last_landmarks: Local<crate::ui_script::VmMemo<Option<LandmarkKey>>>,
) {
    let (Some(mut script), Some(data), Some(map), Some(areas)) = (script, data, map, areas) else {
        return;
    };
    // The discovery bitset (PLAYER_EXPLORED_ZONES, PRIVATE — only our own avatar carries it):
    // push on change (including the first stream-in); the engine setter queues the
    // WORLD_MAP_UPDATE that fills newly explored art into an open zone map. It is also the
    // landmark pass's exploration gate below, so it is resolved before either use.
    let explored: Vec<u32> = self_q
        .iter()
        .next()
        .map(|store| {
            (0..benilla_protocol::messages::PLAYER_EXPLORED_ZONES_SLOTS)
                .map(|i| store.0.player_explored_zone_slot(i))
                .collect()
        })
        .unwrap_or_default();
    {
        let last_explored = last_explored.get(&script);
        if !explored.is_empty() && last_explored.as_ref() != Some(&explored) {
            *last_explored = Some(explored.clone());
            script.set_world_map_explored(explored.clone());
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

    // ── The map's POI icons (`GetNumMapLandmarks`) — the reference's landmark-list builder
    // `0x4a67a0`, rebuilt only when one of its inputs moves (see [`LandmarkKey`]).
    let marker = poi_marker
        .on_map(map.0)
        .map(|m| (m.continent_id, m.pos.map(f32::to_bits), m.icon));
    let states_gen = world_states.generation();
    let unchanged = last_landmarks
        .get(&script)
        .as_ref()
        .is_some_and(|k| k.matches((c, z), map.0, states_gen, &explored, marker));
    if !unchanged {
        let level = MapLevel::of(c, z);
        let mut landmarks = Vec::new();
        // The DBC rows first, in file order, each through the builder's gate chain — that order
        // IS the Lua landmark index (`0x4a6819`'s walk over `[0xc0e054]`).
        if let Some(pois) = pois.as_ref() {
            for (_, poi) in pois.0.rows() {
                if !landmark_gates_pass(poi, level, &areas.0, &explored, &world_states) {
                    continue;
                }
                let Some(uv) = project(poi.continent_id, poi.pos[0], poi.pos[1]) else {
                    continue;
                };
                if is_degenerate(uv) {
                    continue;
                }
                landmarks.push(WorldMapLandmarkView {
                    name: poi.name.clone(),
                    description: poi.description.clone(),
                    texture_index: landmark_texture_index(poi, level),
                    uv,
                });
            }
        }
        // Then the guard-directions marker (`crate::poi_marker`), appended last exactly as
        // `0x4a69c7` does and — like it — exempt from every gate above: no level flag, no
        // exploration bit, no world state, its `Icon` verbatim with no level-15 substitution
        // (wow-re `gossip-poi-marker.md` §8, the `+0x10 == 1` element). Only the same
        // non-degenerate projection test applies.
        if let Some(poi) = poi_marker.on_map(map.0) {
            if let Some(uv) = project(poi.continent_id, poi.pos[0], poi.pos[1]) {
                if !is_degenerate(uv) {
                    landmarks.push(WorldMapLandmarkView {
                        name: poi.name.clone(),
                        description: poi.description.clone(),
                        texture_index: poi.icon,
                        uv,
                    });
                }
            }
        }
        *last_landmarks.get(&script) = Some(LandmarkKey {
            selection: (c, z),
            map: map.0,
            states: states_gen,
            explored: explored.clone(),
            marker,
        });
        script.set_world_map_landmarks(landmarks);
    }

    // The party slots' blips (report B320) — `party1..4` in the same order the frames and the
    // unit tokens use (`GroupState::party_slots`).
    //
    // Two position sources, the minimap's own law (`minimap::blips::party_member_pos`): a member
    // who is STREAMED has a real transform, and anyone else has the `(x, y)` their
    // `SMSG_PARTY_MEMBER_STATS` carried — an `i16` pair, so a far member's blip is yard-accurate
    // and no better, which is all the reference has for them either.
    //
    // Which MAP that position belongs to is the part the stats packet does not say: it carries a
    // zone, not a map id. So the member's zone is looked up in the catalog and the continent that
    // owns it supplies the map; a zone we cannot place (an instance, a zone missing from the
    // catalog) falls back to ours, which is right for the overwhelmingly common case of a party
    // spread across one continent and merely projects off-rect — the (0,0) hide — when it is not.
    let party_uv: Vec<Option<(f32, f32)>> = group
        .party_slots()
        .map(|m| {
            let (px, py) = crate::minimap::party_member_pos(m, &group, &guids, &unit_pos)?;
            let member_map = group
                .stats
                .get(&m.guid)
                .and_then(|st| st.zone)
                .and_then(|zone| {
                    data.continents
                        .iter()
                        .find(|cont| cont.zones.iter().any(|z| z.area_id == u32::from(zone)))
                        .map(|cont| cont.map_id)
                })
                .unwrap_or(map.0);
            project(member_map, px, py).filter(|uv| *uv != (0.0, 0.0))
        })
        .collect();

    script.set_world_map_feed(player_zone, uv, player.facing(), corpse_uv, party_uv);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world_state::WorldStates;
    use benilla_formats::AreaPoi;

    /// A bare row; each test sets only the columns its gate reads.
    fn poi(flags: u32, area_id: u32, world_state_id: u32) -> AreaPoi {
        AreaPoi {
            importance: 0,
            icon: 4,
            faction_id: 0,
            pos: [0.0, 0.0, 0.0],
            continent_id: 0,
            flags,
            area_id,
            name: String::new(),
            description: String::new(),
            world_state_id,
        }
    }

    fn empty_areas() -> benilla_formats::AreaTableCatalog {
        benilla_formats::AreaTableCatalog::from_rows(Vec::new())
    }

    /// The level-flag chain, in the shape §7 says it has: `0x04` for zone, `0x08` for continent,
    /// and world demanding **both** `0x10` and `0x08` because the tests fall through rather than
    /// switch. A row with `0x10` alone is invisible everywhere — the case the shipped data can't
    /// exhibit (no 5875 row sets `0x10` without `0x08`) and the one a switch would get wrong.
    #[test]
    fn the_level_flag_chain_falls_through_at_world_level() {
        let (areas, states, explored) = (empty_areas(), WorldStates::default(), Vec::new());
        let pass = |flags, level| {
            landmark_gates_pass(&poi(flags, 0, 0), level, &areas, &explored, &states)
        };

        // A town (0x0d = 0x01|0x04|0x08): zone and continent, not the world sheet.
        assert!(pass(0x0d, MapLevel::Zone));
        assert!(pass(0x0d, MapLevel::Continent));
        assert!(!pass(0x0d, MapLevel::World));

        // A capital (0x1d = 0x0d|0x10): all three.
        assert!(pass(0x1d, MapLevel::Zone));
        assert!(pass(0x1d, MapLevel::Continent));
        assert!(pass(0x1d, MapLevel::World));

        // An Eastern Plaguelands tower (0x87 = 0x01|0x02|0x04|0x80): zone only.
        assert!(pass(0x87, MapLevel::Zone));
        assert!(!pass(0x87, MapLevel::Continent));
        assert!(!pass(0x87, MapLevel::World));

        // `0x10` alone: the fall-through means world level still wants `0x08`, so nowhere.
        assert!(!pass(0x10, MapLevel::World));
        assert!(!pass(0x10, MapLevel::Continent));
        assert!(!pass(0x10, MapLevel::Zone));
    }

    /// The world-state gate — the Eastern Plaguelands tower mechanism. A row with no
    /// `WorldStateID` is ungated; a row with one appears exactly while that key reads non-zero,
    /// so flipping the state swaps which of the sibling rows exists.
    #[test]
    fn a_world_state_row_appears_only_while_its_key_is_set() {
        let (areas, explored) = (empty_areas(), Vec::new());
        let mut states = WorldStates::default();
        // 2372 / 2373 are the real pair for Northpass Tower: Alliance-held / Horde-held.
        let alliance = poi(0x87, 0, 2372);
        let horde = poi(0x87, 0, 2373);
        let plain = poi(0x87, 0, 0);
        let pass = |p: &AreaPoi, st: &WorldStates| {
            landmark_gates_pass(p, MapLevel::Zone, &areas, &explored, st)
        };

        assert!(
            pass(&plain, &states),
            "an ungated row is always a candidate"
        );
        assert!(!pass(&alliance, &states), "nothing received yet");
        assert!(!pass(&horde, &states));

        states.write(&[(2372, 1)]);
        assert!(pass(&alliance, &states));
        assert!(!pass(&horde, &states));

        // The server flips the tower: the Alliance row's state goes to 0, the Horde row's to 1.
        states.write(&[(2372, 0), (2373, 1)]);
        assert!(!pass(&alliance, &states));
        assert!(pass(&horde, &states));
    }

    /// The exploration gate and its two exemptions: a continent-wide row (`AreaID` `-1`, which
    /// arrives as `u32::MAX`) and a row whose area has `ExplorationLevel < 0` are never gated;
    /// everything else waits for the bit.
    #[test]
    fn the_exploration_gate_reads_the_right_bit_and_exempts_the_right_rows() {
        use benilla_formats::AreaTableRow;
        let row = |explore_flag, exploration_level| AreaTableRow {
            map_id: 0,
            zone_id: 0,
            explore_flag,
            flags: 0,
            faction_group_mask: 0,
            exploration_level,
            name: String::new(),
        };
        let areas = benilla_formats::AreaTableCatalog::from_rows(vec![
            (139, row(40, 0)),  // gated
            (200, row(41, -1)), // exempt by ExplorationLevel
        ]);
        let states = WorldStates::default();
        // Bit 40 → slot 1, bit 8 within it.
        let unexplored = vec![0u32; 4];
        let explored = {
            let mut v = vec![0u32; 4];
            v[1] = 1 << 8;
            v
        };
        let pass =
            |p: &AreaPoi, ex: &[u32]| landmark_gates_pass(p, MapLevel::Zone, &areas, ex, &states);

        assert!(!pass(&poi(0x04, 139, 0), &unexplored), "not discovered yet");
        assert!(pass(&poi(0x04, 139, 0), &explored));
        assert!(
            pass(&poi(0x04, 200, 0), &unexplored),
            "ExplorationLevel -1 exempts the row"
        );
        assert!(
            pass(&poi(0x04, u32::MAX, 0), &unexplored),
            "AreaID -1 (continent-wide) is not > 0, so no gate"
        );
        assert!(pass(&poi(0x04, 0, 0), &unexplored), "AreaID 0 likewise");
        assert!(
            pass(&poi(0x04, 999, 0), &unexplored),
            "an AreaID the table does not carry cannot be tested"
        );
    }

    /// `GetMapLandmarkInfo`'s texture leg: the level-15 substitution bites only at zone level and
    /// only on a row without `Flags & 0x80` — which is exactly the rows with no live world state.
    #[test]
    fn the_zone_level_icon_substitution_spares_the_world_state_rows() {
        let town = poi(0x0d, 0, 0);
        let tower = poi(0x87, 0, 2372);
        assert_eq!(landmark_texture_index(&town, MapLevel::Zone), 15);
        assert_eq!(
            landmark_texture_index(&town, MapLevel::Continent),
            town.icon
        );
        assert_eq!(landmark_texture_index(&town, MapLevel::World), town.icon);
        for level in [MapLevel::Zone, MapLevel::Continent, MapLevel::World] {
            assert_eq!(
                landmark_texture_index(&tower, level),
                tower.icon,
                "Flags & 0x80 escapes the substitution at every level"
            );
        }
    }

    /// The near-zero skip is on **both** axes, not either — a POI on the top edge of a rect
    /// (`v == 0`) stays on the map.
    #[test]
    fn only_a_both_axes_zero_projection_is_dropped() {
        assert!(is_degenerate((0.0, 0.0)));
        assert!(is_degenerate((1e-8, -1e-8)));
        assert!(!is_degenerate((0.0, 0.5)));
        assert!(!is_degenerate((0.5, 0.0)));
        assert!(!is_degenerate((1e-6, 0.0)));
    }

    /// Our `(continent, zone)` pair, `0` = whole, onto the reference's levels.
    #[test]
    fn the_selection_pair_maps_onto_the_reference_levels() {
        assert_eq!(MapLevel::of(0, 0), MapLevel::World);
        assert_eq!(MapLevel::of(0, 3), MapLevel::World, "continent 0 wins");
        assert_eq!(MapLevel::of(2, 0), MapLevel::Continent);
        assert_eq!(MapLevel::of(2, 7), MapLevel::Zone);
    }

    /// The REAL `AreaPOI.dbc`, run through the whole gate chain the way the feed does — the
    /// report B190 asks about, answered off the shipped table rather than a fixture.
    ///
    /// Two claims: the Eastern Plaguelands towers exist as world-state-gated zone-level rows and
    /// change with the state; and the capitals are continent-level rows that need no exploration
    /// bit (which is why a fresh character sees Stormwind on the Eastern Kingdoms map).
    /// Skips without client data.
    #[test]
    fn the_real_table_gates_the_epl_towers_and_the_city_icons() {
        let data = benilla_formats::wow_data_or_skip!();
        let mut chain = benilla_formats::open_chain(&data).expect("chain");
        let pois = benilla_formats::load_area_poi_catalog(&mut chain).expect("AreaPOI");
        let areas = benilla_formats::load_area_table_catalog(&mut chain).expect("AreaTable");
        let nothing_explored = vec![0u32; 64];

        // ── The towers. Northpass Tower's Alliance-held row (1768, state 2372).
        let (_, tower) = pois
            .rows()
            .find(|(id, _)| *id == 1768)
            .expect("AreaPOI 1768");
        assert_eq!(tower.name, "Northpass Tower");
        assert_eq!(tower.world_state_id, 2372);
        assert_eq!(
            tower.flags & 0x80,
            0x80,
            "escapes the level-15 substitution"
        );
        assert_eq!(tower.area_id, 139, "Eastern Plaguelands");

        let mut states = WorldStates::default();
        let explored_epl = {
            let bit = areas.get(139).expect("EPL area row").explore_flag;
            let mut v = vec![0u32; 64];
            v[bit as usize / 32] |= 1 << (bit % 32);
            v
        };
        let tower_shows = |st: &WorldStates, ex: &[u32]| {
            landmark_gates_pass(tower, MapLevel::Zone, &areas, ex, st)
        };
        assert!(
            !tower_shows(&states, &explored_epl),
            "no world states received — no tower icon (this IS report B190)"
        );
        states.write(&[(2372, 1)]);
        assert!(tower_shows(&states, &explored_epl), "Alliance holds it");
        assert!(
            !tower_shows(&states, &nothing_explored),
            "and it still needs the zone discovered"
        );
        assert!(
            !landmark_gates_pass(tower, MapLevel::Continent, &areas, &explored_epl, &states),
            "a tower is a zone-level row (Flags 0x87 carries no 0x08)"
        );

        // ── The capitals: continent-level, and exploration-exempt.
        let states = WorldStates::default();
        for (id, name) in [
            (16u32, "Stormwind"),
            (8, "Ironforge"),
            (18, "The Undercity"),
        ] {
            let (_, city) = pois.rows().find(|(r, _)| *r == id).expect("city row");
            assert_eq!(city.name, name);
            assert_eq!(
                city.area_id,
                u32::MAX,
                "continent-wide, so no exploration gate"
            );
            for level in [MapLevel::World, MapLevel::Continent, MapLevel::Zone] {
                assert!(
                    landmark_gates_pass(city, level, &areas, &nothing_explored, &states),
                    "{name} shows at every level on a fresh character"
                );
            }
            assert_eq!(
                landmark_texture_index(city, MapLevel::Continent),
                city.icon,
                "the city icon is its own at continent level"
            );
            assert_eq!(
                landmark_texture_index(city, MapLevel::Zone),
                15,
                "and the generic cell inside a zone map"
            );
        }
    }

    /// File order is the landmark index: the catalog must hand rows back in the DBC's own record
    /// order, not a hash permutation (which would also make the feed's change-diff fire forever).
    /// Skips without client data.
    #[test]
    fn the_catalog_preserves_dbc_file_order() {
        let data = benilla_formats::wow_data_or_skip!();
        let mut chain = benilla_formats::open_chain(&data).expect("chain");
        let a: Vec<u32> = benilla_formats::load_area_poi_catalog(&mut chain)
            .expect("AreaPOI")
            .rows()
            .map(|(id, _)| id)
            .collect();
        let b: Vec<u32> = benilla_formats::load_area_poi_catalog(&mut chain)
            .expect("AreaPOI")
            .rows()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(
            a, b,
            "two loads agree — the order is the file's, not a hash's"
        );
        assert!(a.len() > 300);
        assert_ne!(
            a,
            {
                let mut sorted = a.clone();
                sorted.sort_unstable();
                sorted
            },
            "and it is genuinely file order, not id order"
        );
    }
}
