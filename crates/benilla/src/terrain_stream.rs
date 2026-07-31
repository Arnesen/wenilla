//! The terrain streamer — the world's sole terrain owner, built on `benilla-assets`.
//!
//! It **streams** the ground around the player through the standard `AssetServer` (the `mpq://` source +
//! `AdtLoader`): it loads a `Handle<AdtTile>` for every tile in range and drops it when the tile leaves
//! range — so residency, async loading, the decode cache, and refcounted unloading are all the engine's
//! job now, not a bespoke worker/finalize pipeline. Per loaded tile it spawns: the merged terrain mesh +
//! splat [`TerrainMaterial`]; an avian static terrain collider (a trimesh from the same merged mesh,
//! riding the tile entity's lifecycle); the doodad/WMO placements (from `Handle<M2Model>`/`Handle<WmoModel>`,
//! deduped by the `AssetServer`, each with its own static collider entity, with the same
//! [`WowModelMaterial`] + [`DoodadFade`] components the legacy path used — so the existing
//! visibility/fade/lighting systems govern them unchanged); the MCLQ water surfaces; and the ground
//! clutter (scattered per chunk into `ClutterChunk`s the shared `stream_chunk_clutter` builds lazily).
//! It also publishes loading-screen residency. Current-map directory + id come from
//! [`CurrentMap`]/[`MapCatalogRes`] ([`crate::world_map`]); the clutter catalog + build lifecycle from
//! [`crate::clutter::ClutterPlugin`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use benilla_assets::coords::{bevy_to_wow, placement_rotation, wow_to_bevy};
use benilla_assets::{AdtTile, M2Model, WdtIndex, WmoModel};
use benilla_formats::{world_to_tile, Doodad, WmoInstance};
use bevy::pbr::ExtendedMaterial;
use bevy::prelude::*;

use crate::assets::RenderConfig;
use crate::char_select::{ClientState, Roster};
use crate::clutter::{scatter_tile_clutter, ClutterConfig, GroundClutter};
use crate::collision::{GroundDecalSurface, PickOccluder};
use crate::interior::WmoResidency;
use crate::lighting::SharedLightBuffer;
use crate::liquid::{spawn_liquids, LiquidAssets};
use crate::loading_screen::WorldLoadProgress;
use crate::model_render::{m2_url, wmo_url, MaterialCache};
use crate::player::{Player, WorldCamera};
use crate::schedule::WorldStage;
use crate::terrain::{TerrainExtension, TerrainMaterial};
use crate::world_map::{CurrentMap, MapCatalogRes};
use crate::SPAWN_XY;

mod collider;
mod queries;
mod spawn;

use collider::{finish_colliders, terrain_collider_data};
use spawn::spawn_loaded_placements;
// The shared placed-model assembler + the off-thread collider build — also the WMO-gameobject
// doodad-prop path's spawner (`crate::entities`' `wmo_props`: the ship's sails ride the streamed
// gameobject entity, and its cargo hulls ride the boat's kinematic body).
pub(crate) use collider::{build_collider_task, placement_collider_data, PendingCollider};
pub(crate) use spawn::{m2_fade, point_light, spawn_model_entities};
// The position queries + area authority (their home is `queries`; paths stay `terrain_stream::X`).
use queries::update_current_area;
pub(crate) use queries::{
    doodad_ground_shade, ground_effect_under, terrain_height_under, AreaAuthoritySet, CurrentArea,
    ShadeResolve,
};

/// Wall-clock spent per frame spawning streamed-in geometry (terrain tiles in [`stream_terrain`],
/// doodad/WMO placements in [`spawn_loaded_placements`]) before deferring the rest to the next frame.
/// Building a static collider is a synchronous parry trimesh/QBVH build; without this cap a cold-start
/// load spawns the whole ring at once and blocks the main thread (window beachball) for seconds. ~4 ms
/// keeps each frame responsive while still streaming the world in quickly.
const SPAWN_BUDGET: Duration = Duration::from_millis(4);

/// The new streamer's residency state: which tiles are loaded and the entity each was spawned as.
/// `pub(crate)` (fields private): the sound subsystem resolves ground lookups through
/// [`ground_effect_under`] against the resident tiles.
#[derive(Resource, Default)]
pub(crate) struct TerrainStreamer {
    /// Loaded tiles by `(tile_x, tile_y)`. The [`Handle<AdtTile>`] keeps the asset resident; dropping
    /// it (on unload / map swap) lets the `AssetServer` release the tile and its sub-assets.
    tiles: HashMap<(i32, i32), TileState>,
    /// The map directory the loaded tiles are for (e.g. `"Azeroth"`); a change means a cross-map
    /// teleport, so every tile is dropped and re-streamed for the new map.
    map_dir: Option<String>,
    /// The current map's WDT tile index (decision 0476), requested with the map: ADT requests wait
    /// for it and consult its `MAIN` grid — open ocean authors no tiles to ask for.
    wdt: Option<Handle<WdtIndex>>,
    /// The WDT failed to load (unheard of for a shipped map): stream ungated like pre-0476 rather
    /// than showing no world. Reset on map change.
    wdt_ungated: bool,
    /// `true` once this map's global WMO has been registered as a placement (decision 0688) — on a
    /// WMO-only map that one building is the whole world, so there is nothing else to stream.
    /// Cleared on map change, which also releases the placement.
    global_wmo: bool,
}

/// The placement id the map-global WMO is registered under. A WMO-only map authors **no** ADT tiles
/// at all, so it contributes no MODF/MDDF uniqueIds this could collide with — and the value is the
/// one the file itself carries in the dead `uniqueId` slot (the reference overwrites it from its own
/// counter at `0xc9a320`, having no more use for it than we do).
const GLOBAL_WMO_UID: u32 = u32::MAX;

impl TerrainStreamer {
    /// Residency counts for the debug panel's World readout: `(spawned, requested)` — tiles whose
    /// terrain entity exists vs. every tile in the stream window (spawned + still loading).
    pub(crate) fn residency(&self) -> (usize, usize) {
        let spawned = self.tiles.values().filter(|t| t.entity.is_some()).count();
        (spawned, self.tiles.len())
    }
}

/// One loaded tile: the resident asset handle, the terrain entity, and the placement ids it references.
struct TileState {
    handle: Handle<AdtTile>,
    /// `None` until the `AdtTile` finishes loading and the terrain mesh entity is spawned (which is also
    /// when this tile's placements are registered).
    entity: Option<Entity>,
    /// uniqueIds of the doodad/WMO placements this tile references — registered once the tile's `AdtTile`
    /// loads, refcount-released when the tile unloads.
    placements: Vec<u32>,
    /// The tile's water-surface entities (tile-exclusive, like the terrain mesh — despawned on unload).
    liquid: Vec<Entity>,
    /// The tile's per-chunk `ClutterChunk` entities (tile-exclusive; their lazily-built clutter meshes
    /// are children, so despawning these cascades to them).
    clutter: Vec<Entity>,
}

/// Doodad/WMO placements spawned **once** and shared across the tiles that reference them — the client's
/// own cross-tile dedup (a building straddling N tiles is spawned once and refcounted). The model assets
/// are loaded by `Handle`, so the `AssetServer` dedups the *decode*; this dedups the *instance*.
#[derive(Resource, Default)]
struct Placements {
    /// By MDDF/MODF uniqueId: the placement + how many loaded tiles reference it.
    by_id: HashMap<u32, Placement>,
    /// Material dedup, so submeshes sharing a (texture, blend, sidedness, kind, fade-variant) share one
    /// `WowModelMaterial` handle — what lets Bevy batch them (mirrors the old `model_material` cache).
    materials: MaterialCache,
}

/// A shared placement: its resident model handle, world transform, and spawned submesh entities. Doodad
/// vs WMO is read off the [`ModelHandle`] variant, so it isn't stored separately.
struct Placement {
    model: ModelHandle,
    transform: Transform,
    /// Spawned submesh entities — empty until the model asset finishes loading (`spawned` then `true`).
    /// WMO doodad-prop submeshes ([`Placement::doodads`]) are appended here too, so they despawn together.
    entities: Vec<Entity>,
    /// `true` once we've spawned (or determined there's nothing to spawn) — so we don't retry.
    spawned: bool,
    /// MODF doodad-set index (WMOs only; 0 for M2 doodads) — which extra prop set to show beyond set 0.
    doodad_set: u16,
    /// MODF name-set index (WMOs only) — the placement's `WMOAreaTable.NameSetID` audio variant.
    name_set: u16,
    /// WMO doodad props (candle stands, banners) resolved once the WMO root loads — each its own M2,
    /// spawned across frames as its asset arrives. Empty for M2-doodad placements.
    doodads: Vec<WmoDoodadInst>,
    /// The placement's [`crate::wmo_portal::WmoPortalInstance`] entity, spawned with the building's
    /// groups. Held so the props — which spawn later, as their own M2 assets land — can be tagged
    /// with the same instance and cull alongside the group that owns them.
    portal_instance: Option<Entity>,
    /// How many loaded tiles reference this placement; despawned when it hits zero.
    refs: u32,
}

/// One WMO doodad prop instance: its M2 handle and the **world** transform (the WMO instance
/// transform composed with the doodad's WMO-local transform), spawned once the M2 asset loads.
struct WmoDoodadInst {
    handle: Handle<M2Model>,
    transform: Transform,
    /// Every WMO group whose MODR names this prop — the portal-cull key, so a prop is hidden with
    /// the rooms it furnishes and drawn while any one of them is visible. Empty for a MODD no group
    /// references (the reference never instantiates one at all; we still show it, uncullable,
    /// rather than change what draws today).
    groups: Arc<[u16]>,
    /// The prop's lighting ([`PropLight`], from `WmoModel::doodad_base` composed with this
    /// placement): exterior sky-lit, or the interior MODD-colour base + its owning group's MOLR
    /// lights placed in world space — folded into the prop's SH probe once its M2 loads (the fold
    /// reference point needs the M2 bounds).
    light: PropLight,
    spawned: bool,
}

/// A WMO prop's placement-resolved lighting: the asset-level [`DoodadBase`] with the owning group's
/// MOLR lights already transformed to WORLD (Bevy) space — so the spawn-time SH fold needs only the
/// loaded M2's bounds (its reference point) and nothing from the WMO asset.
enum PropLight {
    Exterior,
    Interior {
        /// `cap96(MODD.colour)` — the ambient word (0–1 RGB).
        ambient: [f32; 3],
        /// `floor112(MODD.colour)` — the diffuse word, committed on the fixed interior axis.
        diffuse: [f32; 3],
        /// The owning group's MOLR omni lights: world (Bevy) position, colour × intensity, and the
        /// disk `attenStart`/`attenEnd` window (the fold's range gate).
        lights: Vec<PropLobeLight>,
    },
}

/// One MOLR-referenced light as the interior fold consumes it (world Bevy space, colour
/// pre-multiplied by the authored intensity). Shared by the MODD prop spawn fold and the
/// GameObject footprint lane ([`crate::interior`] via [`crate::wmo_portal`]'s verdict).
pub(crate) struct PropLobeLight {
    pub(crate) pos: Vec3,
    pub(crate) color_i: [f32; 3],
    pub(crate) atten_start: f32,
    pub(crate) atten_end: f32,
}

/// Fold one interior committed light into its 7-row SH probe: the ambient word + the diffuse word
/// as a directional on the FIXED interior axis + each MOLR lobe windowed by its disk
/// attenStart/attenEnd from `ref_point` (the byte-verified `0x69e1c0` falloff: d ≤ start → 1;
/// d ≥ end → excluded; else linear). One definition for both interior lanes — the MODD prop
/// (spawn-time, MODD-colour words) and the GameObject footprint (classify-time, MOCV-derived
/// words); the SH closed form itself is [`prop_probe_coeffs`].
pub(crate) fn fold_interior_probe(
    ambient: [f32; 3],
    diffuse: [f32; 3],
    ref_point: Vec3,
    lights: &[PropLobeLight],
) -> [bevy::math::Vec4; 7] {
    // Toward-light, Bevy space: wow (0.30822, 0.30822, 0.9) → (−y, z, −x).
    let mut lobes: Vec<(Vec3, [f32; 3])> = vec![(Vec3::new(-0.30822, 0.9, -0.30822), diffuse)];
    for l in lights {
        let dv = l.pos - ref_point;
        let dist = dv.length();
        let gain = if dist <= l.atten_start {
            1.0
        } else if dist >= l.atten_end || l.atten_end <= l.atten_start {
            0.0
        } else {
            1.0 - (dist - l.atten_start) / (l.atten_end - l.atten_start)
        };
        if gain > 0.0 {
            lobes.push((dv / dist.max(1e-4), l.color_i.map(|c| c * gain)));
        }
    }
    crate::lighting::prop_probe_coeffs(ambient, &lobes)
}

/// **Where the world streams from**, in WoW coordinates — one rule, shared by the detailed
/// streamer and the distant WDL ring (which used to carry its own copy under a comment promising
/// they agreed).
///
/// The order is a ladder of decreasing certainty:
///
/// 1. **The avatar**, once it is in the world and we are behind it — the answer for all of play.
/// 2. **The character you picked**, from its own roster row, while the world is loading and the
///    server has not yet sent the destination snap. This window is short (~265 ms measured on a
///    local server) but it is precisely the window in which the entry's IO budget is scarcest, and
///    without this leg the streamer spends it on [`SPAWN_XY`] — Northshire — for a character who
///    may be in Stratholme (decision 0777).
/// 3. **The camera**, which is the focus while free-flying (`detached`) and the only focus a
///    server-less capture run has.
/// 4. [`SPAWN_XY`], if there is not even a camera.
///
/// `entry` is [`Roster::pending_entry`] — passed as plain data rather than the roster itself, so
/// the ladder is a pure function of the four things that decide it, and testable as one.
pub(crate) fn view_focus(
    player: &Player,
    camera: Option<Vec3>,
    entry: Option<(u32, [f32; 3])>,
) -> [f32; 3] {
    if player.active && !player.detached {
        return bevy_to_wow(player.pos);
    }
    if !player.active {
        if let Some((_, pos)) = entry {
            return pos;
        }
    }
    match camera {
        Some(t) => bevy_to_wow(t),
        None => [SPAWN_XY.0, SPAWN_XY.1, 0.0],
    }
}

/// **Which map** the streamers should be loading — [`CurrentMap`] once the server has told us,
/// and the picked character's own map for the entry window before that (the [`view_focus`] ladder's
/// second rung, and it must agree with it: a position from one map against the tile grid of
/// another names tiles that exist but are somewhere else entirely).
pub(crate) fn focus_map(
    current: Option<u32>,
    player: &Player,
    entry: Option<(u32, [f32; 3])>,
) -> u32 {
    if !player.active {
        if let Some((map, _)) = entry {
            return map;
        }
    }
    current.unwrap_or(0)
}

/// A placement's model asset handle — an M2 doodad or a WMO building. Keeps the asset resident.
enum ModelHandle {
    M2(Handle<M2Model>),
    Wmo(Handle<WmoModel>),
}

/// The terrain streamer plugin (added by `main` as the world's terrain owner).
pub(crate) struct TerrainPlugin;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TerrainStreamer>()
            .init_resource::<Placements>()
            .init_resource::<CurrentArea>()
            // **The streaming chain lives in `WorldStage::Stream`** — the load-bearing half of the
            // frame ordering contract (schedule.rs): the teleport snap (Input) → the streamer
            // recomputes focus + publishes residency HERE → the loading screen covers (Present),
            // all in one frame, so a swap never renders uncovered. The legacy streamer carried
            // this membership and the cutover to this one silently dropped it — the executor was
            // then free to run the streamer *before* the snap, and the cover landed a frame late
            // on exactly the burst frame (the `.tele` destination flash; decision 0738).
            // `finish_colliders` heads the chain so the collider-queue depth the settle release
            // and the Present-stage clear read is this frame's, not last frame's.
            .add_systems(
                Update,
                (
                    finish_colliders,
                    stream_terrain,
                    spawn_loaded_placements,
                    sync_interior_volumes,
                )
                    .chain()
                    .in_set(WorldStage::Stream)
                    // **No world until a character is in one** (decision 0777). The whole chain,
                    // not just the request side: with nothing streamed there is nothing to spawn,
                    // no collider to finish and no WMO volume to mirror, and gating the head alone
                    // would leave three systems walking empty queries to prove it every frame.
                    .run_if(crate::schedule::world_is_live),
            )
            // Leaving the world releases it. Without this, a `/logout` back to character select
            // would leave the whole streamed world resident and unowned — the gate above stops it
            // being *maintained*, which is precisely what makes an abandoned world immortal.
            .add_systems(OnExit(ClientState::InWorld), release_world)
            // Within-map art residency (decision 0793): the placement material dedup expires by
            // distance, so a long flight on one continent stops ratcheting. Ungated by
            // `world_is_live` on purpose — a cache still holding the last world's art is exactly
            // what should be draining while you sit at character select.
            .add_systems(Update, scope_placement_art)
            .add_systems(
                Update,
                // After the interior claim so the leaf override reads THIS frame's claim —
                // the client resolves leaf + indoor + names from ONE node state in one pass
                // (`0x67e510`); a stale-leaf/fresh-claim frame let the abbey login big-splash
                // (the A ≠ subzone gate saw "Northshire Valley" against the abbey's name).
                // `AreaAuthoritySet` lets the zone-text feed order after this in turn.
                update_current_area
                    .after(crate::wmo_portal::WmoPvsSet)
                    .in_set(AreaAuthoritySet),
            );
    }
}

/// Stream `AdtTile`s around the view focus: drop tiles that left range (releasing their placements),
/// request newly-in-range tiles, and — as each finishes loading — spawn its terrain mesh + material and
/// register its doodad/WMO placements. The desired square is gated on the map's WDT `MAIN` grid
/// (decision 0476): a tile the map doesn't author is never requested — no NotFound error spam on
/// open-ocean crossings, and the loading screen's ready/total counts only tiles that can exist.
#[allow(clippy::too_many_arguments, clippy::type_complexity)] // the bundled asset_stores tuple
fn stream_terrain(
    mut commands: Commands,
    mut state: ResMut<TerrainStreamer>,
    placements: ResMut<Placements>,
    asset_server: Res<AssetServer>,
    tiles: Res<Assets<AdtTile>>,
    // Bundled into one tuple param to stay within Bevy's 16-element system-param limit; destructured
    // to the same bindings below.
    asset_stores: (
        ResMut<Assets<TerrainMaterial>>,
        ResMut<Assets<Mesh>>,
        Res<Assets<WdtIndex>>,
        Res<Time>,
        ResMut<crate::perf::StreamActivity>,
    ),
    liquid_assets: Option<Res<LiquidAssets>>,
    clutter: Option<Res<GroundClutter>>,
    clutter_cfg: Option<Res<ClutterConfig>>,
    mut player: ResMut<Player>,
    camera: Query<&Transform, With<WorldCamera>>,
    shared_light: Option<Res<SharedLightBuffer>>,
    cfg: Option<Res<RenderConfig>>,
    // "Where are we" — the server's map, the catalog that names its directory, and the roster row
    // of a pick still in flight (the entry window's focus). Bundled for the same param-limit
    // reason as `asset_stores`.
    location: (
        Option<Res<CurrentMap>>,
        Option<Res<MapCatalogRes>>,
        Option<Res<Roster>>,
    ),
    mut load_progress: Option<ResMut<WorldLoadProgress>>,
) {
    let (mut materials, mut meshes, wdts, time, mut activity) = asset_stores;
    let (current_map, map_catalog, roster) = location;
    // The shared light buffer + map catalog are set up by other plugins' startup; until they exist
    // there's nothing to stream against, so idle.
    let (Some(shared_light), Some(map_catalog)) = (shared_light, map_catalog) else {
        return;
    };
    let t0 = Instant::now();
    let placements = placements.into_inner();
    let map_id = focus_map(
        current_map.map(|m| m.0),
        &player,
        roster.as_deref().and_then(Roster::pending_entry),
    );
    let Some(dir) = map_catalog.0.directory(map_id).map(str::to_string) else {
        return;
    };

    // Cross-map teleport: the map directory changed → drop every loaded tile (despawn + release the
    // handles + placements) so the new map streams in fresh, and request the new map's WDT (the
    // tile-existence index every ADT request below consults).
    if state.map_dir.as_deref() != Some(dir.as_str()) {
        // Say which map's tiles we are about to spend the IO budget on, and how many we are
        // throwing away to do it. Silent before, which is exactly why nobody noticed that every
        // world entry began by streaming Northshire regardless of where the character was.
        info!(
            "terrain: streaming map {dir} (was {:?}, {} tiles released)",
            state.map_dir,
            state.tiles.len()
        );
        drop_streamed_world(&mut commands, &mut state, placements, &mut activity);
        state.map_dir = Some(dir.clone());
        state.wdt = Some(asset_server.load(format!("mpq://World/Maps/{dir}/{dir}.wdt")));
        state.wdt_ungated = false;
    }
    // The WDT gate (0476). A missing/failed WDT (unheard of for a shipped map) falls back to the
    // old ungated probing, warned once per map — a broken index must never mean "no world".
    let wdt_index = state.wdt.as_ref().and_then(|h| wdts.get(h));
    if wdt_index.is_none() && !state.wdt_ungated {
        if let Some(h) = &state.wdt {
            if matches!(
                asset_server.load_state(h),
                bevy::asset::LoadState::Failed(_)
            ) {
                warn!("terrain: no WDT for map {dir} — streaming ungated (every tile probed)");
                state.wdt_ungated = true;
            }
        }
    }

    // A WMO-only map (decision 0688): the WDT says this map authors no terrain and its entire world
    // is one building. Register it exactly like any ADT-placed WMO — same `Placement`, same
    // spawn/collider/doodad/portal path — because that is what the reference does: its WDT branch
    // hands the global MODF entry to the SAME consumer (`0x695650`) an ADT's MCRF walk does. It is
    // registered once for the map and released on the map change above; nothing streams it in or
    // out, since there is no "range" for a building that is the map.
    if let Some(g) = wdt_index.and_then(|w| w.global_wmo()) {
        if !state.global_wmo {
            info!(
                "terrain: map {dir} has no tiles — its world is one WMO ({})",
                g.model
            );
            register_wmo(
                placements,
                &asset_server,
                &WmoInstance {
                    model: g.model.clone(),
                    position: g.position,
                    rotation: g.rotation,
                    unique_id: GLOBAL_WMO_UID,
                    doodad_set: g.doodad_set,
                    name_set: g.name_set,
                },
            );
            state.global_wmo = true;
        }
    }

    // Stream around the *view focus* — see [`view_focus`] for the ladder.
    let center = view_focus(
        &player,
        camera.single().ok().map(|c| c.translation),
        roster.as_deref().and_then(Roster::pending_entry),
    );
    let radius = cfg.as_ref().map(|c| c.tile_radius as i32).unwrap_or(2);
    let tiling = cfg.as_ref().map(|c| c.tex_tiles).unwrap_or(8.0);
    let (cx, cy) = world_to_tile(center[0], center[1]);
    let (cx, cy) = (cx as i32, cy as i32);

    // Desired set: the (2r+1)² square around the focus, clamped to the 64×64 tile grid — and
    // filtered to tiles the WDT says exist (open ocean authors none). The filter also fixes the
    // loading screen's accounting: `ready == total` is reachable on a coast, and any fallback-era
    // entry for a nonexistent tile turns stale and unloads below.
    let mut desired: Vec<(i32, i32)> = Vec::new();
    for dx in -radius..=radius {
        for dy in -radius..=radius {
            let (tx, ty) = (cx + dx, cy + dy);
            if (0..=63).contains(&tx) && (0..=63).contains(&ty) {
                desired.push((tx, ty));
            }
        }
    }
    if let Some(w) = wdt_index {
        desired.retain(|&(tx, ty)| w.has_tile(tx as u32, ty as u32));
    }

    // Unload tiles no longer desired: despawn the terrain entity, release the placements, drop the
    // handle. **Budgeted per frame** (B181): dropping the whole trailing row on the crossing frame
    // freed ~1300 mesh assets — each tile's 256 chunk cells, CPU copies included — plus the tiles'
    // decoded chunks and their placements' models, all in one frame. Measured (`WOW_STREAM_TRACE`,
    // Emerald Dream flat-map pin): that free wave is the 2–3-dropped-interval spike on every ADT
    // boundary crossing, while this chain's own systems — request, spawn, collider attach, each
    // already budgeted — stayed under 3 ms. So the drop lane gets the same treatment: at most
    // `unload_budget` tiles release per frame, farthest from the focus first. The un-drained
    // remainder just stays resident a few frames longer — behind the direction of travel, and a
    // re-cross inside the drain window finds its tile still loaded (no re-decode, no re-spawn).
    // A cross-map swap is NOT budgeted (`drop_streamed_world` above): the loading screen covers
    // it, and holding a dead map's tiles through a fresh map's IO burst helps nothing.
    // **The keep-band** (B181): a tile is requested inside `radius` but released only past
    // `radius + 1` — one tile of hysteresis. Without it, a body straddling a tile line cycles a
    // whole row per step: the reporter's own minimal repro (`.go` half a yard across, and back)
    // re-decoded and re-spawned five tiles each way, and the row's landing — not the streamer's
    // own budgeted systems — is the frame spike (a fresh Undercity row lands ~3900 placements
    // and ~1300 mesh assets). A band tile stays fully resident, so a re-cross finds it and
    // streams NOTHING; the band's dedup entries need no art-sweep accommodation (0793's
    // `radius_floor` is about art the streamer will re-fetch — a band tile is already spawned
    // and never re-consults the caches).
    //
    // Ablation switch (`WOW_NO_TILE_DROP=1`): never release stale tiles — a crossing becomes a
    // pure asset-ADD event and a re-cross a pure no-op, separating the load lane from the free
    // lane when measuring a streaming cost. Residency grows for the life of the run; dev-only.
    let unload_budget = cfg.as_ref().map(|c| c.unload_budget).unwrap_or(1);
    let keep = radius + 1;
    let mut stale: Vec<(i32, i32)> = if std::env::var("WOW_NO_TILE_DROP").is_ok() {
        Vec::new()
    } else {
        state
            .tiles
            .keys()
            .copied()
            .filter(|&(tx, ty)| (tx - cx).abs().max((ty - cy).abs()) > keep)
            .collect()
    };
    if unload_budget > 0 && stale.len() > unload_budget {
        stale.sort_by_key(|&(tx, ty)| std::cmp::Reverse((tx - cx).abs().max((ty - cy).abs())));
        stale.truncate(unload_budget);
    }
    for c in stale {
        if let Some(t) = state.tiles.remove(&c) {
            despawn_tile_owned(&mut commands, &t);
            activity.tiles_dropped += 1;
            for uid in t.placements {
                release_placement(&mut commands, placements, uid, &mut activity);
            }
        }
    }

    // Request newly-desired tiles (the `AssetServer` dedups, so re-requesting a loaded one is
    // free) — only once the WDT has answered (or failed into the ungated fallback): a request
    // fired before the index lands could probe a tile that doesn't exist.
    //
    // **Staggered while live** (B181): a whole fresh row requested in one frame decodes in
    // parallel and LANDS near-together — ~1300 mesh assets plus the texture arrays hitting the
    // render world's prepare in one or two frames, a wave no app-side budget downstream of the
    // request can spread. One fresh request per frame, nearest first, staggers the landings for
    // the only cost of the window edge (~2 tiles out, in fog) filling over a few frames. World
    // entry and teleports stay unstaggered: the loading screen exists to absorb that burst, and
    // the settle release waits on exactly these tiles.
    if wdt_index.is_some() || state.wdt_ungated {
        let live = player.active && !player.settling && !player.world_stale;
        let mut fresh: Vec<(i32, i32)> = desired
            .iter()
            .copied()
            .filter(|c| !state.tiles.contains_key(c))
            .collect();
        if live && fresh.len() > 1 {
            fresh.sort_by_key(|&(tx, ty)| (tx - cx).abs().max((ty - cy).abs()));
            fresh.truncate(1);
        }
        for (tx, ty) in fresh {
            activity.tiles_requested += 1;
            state.tiles.insert(
                (tx, ty),
                TileState {
                    handle: asset_server
                        .load(format!("mpq://World/Maps/{dir}/{dir}_{tx}_{ty}.adt")),
                    entity: None,
                    placements: Vec::new(),
                    liquid: Vec::new(),
                    clutter: Vec::new(),
                },
            );
        }
    }

    // Spawn any loaded-but-unspawned tile with the production terrain material, and register its
    // placements. The material references the ONE shared global-light buffer (updated in place each
    // frame), so a freshly-streamed tile is correctly lit + fogged on its first frame. Budgeted per
    // frame (a tile's terrain collider is a big trimesh/QBVH build): on cold start the whole ring
    // finishes loading near-together, and spawning every tile in one frame stalls the main thread.
    let tile_deadline = Instant::now() + SPAWN_BUDGET;
    for (&(tx, ty), tile) in state.tiles.iter_mut() {
        if tile.entity.is_some() {
            continue;
        }
        let Some(adt) = tiles.get(&tile.handle) else {
            continue; // not loaded yet (or missing) — try again next frame
        };
        let material = materials.add(ExtendedMaterial {
            base: terrain_base_material(),
            extension: TerrainExtension {
                layer_array: adt.layer_array.clone(),
                alpha_array: adt.alpha_array.clone(),
                shadow_array: adt.shadow_array.clone(),
                params: Vec4::new(tiling, 0.0, 0.0, 0.0),
                light_buf: shared_light.0.clone(),
            },
        });
        // Terrain collider (decision 0009): ONE static trimesh per tile, welded from the same decoded
        // chunks the drawn cells are built from (so you stand on the visible ground), built off-thread
        // (attached by `finish_colliders`) so a tile streaming in never hitches the frame. It rides the
        // tile root's lifecycle — gone when the tile despawns, no separate bookkeeping.
        let collider_data = terrain_collider_data(&adt.chunks);
        // The tile ROOT draws nothing: it carries the collider and the surface roles, and its MCNK
        // cells hang off it as children — one drawn object per 33.333 yd chunk. That is the unit the
        // exterior-scene cull needs (decision 0780; a 533 yd slab the camera stands on intersects
        // every portal window, so it could never be hidden), and it also gives Bevy's own frustum cull
        // something smaller than half a kilometre to reject. Despawning the root takes the cells.
        let mut tile_ent = commands.spawn((Transform::IDENTITY, Visibility::default()));
        // Ablation switch (`WOW_NO_TILE_CELLS=1`): spawn the tile — asset residency, collider,
        // placements, liquid, clutter — without its per-chunk cell entities, isolating the
        // render-entity lane from the asset lane when measuring a streaming cost. The B181 carve
        // ran on it (a crossing's spike survived zero cells, exonerating the entity lane); the
        // `WOW_NO_LIQUID`/`WOW_NO_PARTICLES` family's pattern.
        if std::env::var("WOW_NO_TILE_CELLS").is_err() {
            tile_ent.with_children(|cells| {
                for mesh in &adt.chunk_meshes {
                    cells.spawn((
                        Mesh3d(mesh.clone()),
                        MeshMaterial3d(material.clone()),
                        Transform::IDENTITY, // chunk meshes are already in absolute world coords
                        // ADT terrain is exterior scene: from inside a WMO a cell draws only through a
                        // portal window (`0x683bf0`, fed solely by the per-window walk `0x682fa0` — see
                        // `crate::exterior_cull`).
                        crate::exterior_cull::ExteriorScene,
                    ));
                }
            });
        }
        if let Some((verts, tris)) = collider_data {
            // `GroundDecalSurface`: terrain receives the selection ring (see `crate::collision`).
            // `PickOccluder`: terrain clamps the mouse pick (the reference's world trace).
            tile_ent.insert((
                PendingCollider::new(build_collider_task(verts, tris), None, true),
                GroundDecalSurface,
                PickOccluder,
            ));
        }
        tile.entity = Some(tile_ent.id());
        activity.tiles_spawned += 1;

        // Register this tile's doodad/WMO placements (deduped + refcounted by uniqueId across tiles).
        // The MCSH ground-shade is NOT sampled here: a doodad straddles several tiles and this one may
        // not contain its origin, so the shade is resolved at spawn via a global lookup (see
        // `doodad_ground_shade` / `spawn_loaded_placements`) — the reference's own per-frame model.
        for d in &adt.doodads {
            register_doodad(placements, &asset_server, d);
            tile.placements.push(d.unique_id);
        }
        for w in &adt.wmos {
            register_wmo(placements, &asset_server, w);
            tile.placements.push(w.unique_id);
        }

        // Spawn this tile's water surfaces (tile-exclusive — despawned with the tile, like its mesh).
        let mut liquid_ents = Vec::new();
        spawn_liquids(
            &mut commands,
            adt.chunks.iter().filter_map(|c| c.liquid.as_ref()),
            liquid_assets.as_deref(),
            &mut meshes,
            &mut liquid_ents,
        );
        tile.liquid = liquid_ents;

        // Scatter this tile's ground clutter into per-chunk `ClutterChunk` units (tile-owned; the
        // shared `stream_chunk_clutter` builds + tears down their meshes lazily within the ~70 yd
        // detail-doodad horizon, same as for the old streamer's chunks). Needs the app-side
        // ground-effect catalog + density; absent before they're set up, so skipped until then.
        if let (Some(clutter), Some(clutter_cfg)) = (clutter.as_ref(), clutter_cfg.as_ref()) {
            let mut clutter_ents = Vec::new();
            scatter_tile_clutter(
                &mut commands,
                &adt.chunks,
                tx as u32,
                ty as u32,
                &clutter.catalog,
                clutter_cfg.density,
                &mut clutter_ents,
            );
            tile.clutter = clutter_ents;
        }

        // One tile spawned; if that used up the frame's budget, leave the rest for next frame (the
        // `tile.entity.is_some()` guard above makes this re-entrant). The loading bar reflects the
        // partial residency below, so it animates rather than jumping to full after a stall.
        if Instant::now() >= tile_deadline {
            break;
        }
    }

    // Publish residency for the loading screen: how many desired tiles are actually spawned,
    // whether the tile under the view focus is up (the backstop trigger), and the scene term
    // (decision 0737) — the focus tile + its 8 neighbours with every placement they reference, so
    // "ready to reveal" means the buildings and trees around you exist, not just bare terrain. A
    // focus tile that doesn't exist (map edge) counts as resident so we never get stuck waiting
    // for ground that isn't there.
    if let Some(p) = load_progress.as_mut() {
        let spawned = |c: &(i32, i32)| state.tiles.get(c).is_some_and(|t| t.entity.is_some());
        p.total = desired.len();
        p.ready = desired.iter().filter(|c| spawned(c)).count();
        p.focus_resident = state
            .tiles
            .get(&(cx, cy))
            .is_none_or(|t| t.entity.is_some());
        // The focus neighbourhood's placements join the accounting (bar + scene term). A placement
        // is *up* once its own model spawned AND its WMO props (each an M2 arriving on its own
        // schedule) have — the furniture the reveal would otherwise pop in. Placements of tiles not
        // yet spawned aren't known yet; the missing tile itself holds `scene_ready` down.
        let mut near_pending = 0usize;
        let mut near_tile_missing = false;
        for dx in -1..=1i32 {
            for dy in -1..=1i32 {
                let c = (cx + dx, cy + dy);
                if !desired.contains(&c) {
                    continue; // off-grid or WDT says no tile — nothing to wait for
                }
                match state.tiles.get(&c) {
                    Some(t) if t.entity.is_some() => {
                        for uid in &t.placements {
                            if let Some(pl) = placements.by_id.get(uid) {
                                let up = pl.spawned && pl.doodads.iter().all(|d| d.spawned);
                                p.total += 1;
                                p.ready += usize::from(up);
                                near_pending += usize::from(!up);
                            }
                        }
                    }
                    _ => near_tile_missing = true,
                }
            }
        }
        p.placements_pending = near_pending;
        p.scene_ready = p.focus_resident && !near_tile_missing && near_pending == 0;
        // Until the WDT answers we don't yet know what this map is made of, so nothing under the
        // focus can be called resident. Without this the post-worldport frames before the index
        // lands read as "ground is up" — harmless on an ADT map (the tile entries below close the
        // gap a frame later) but on a WMO-only map, where `desired` is empty forever, it is the
        // difference between a loading screen and a glimpse of the void.
        if wdt_index.is_none() && !state.wdt_ungated {
            p.focus_resident = false;
            p.scene_ready = false;
        }
        // A WMO-only map's residency IS its one building (0688) — there are no tiles to count, so
        // the bar and the clear-condition ride the placement instead. Counting it keeps
        // `total > 0`, which is what `is_ready` requires before it will clear the screen at all.
        // The backstop/`world_stale` term takes the building's spawn; the scene term also waits
        // for its props (the Stockade's 740 candles are the reveal).
        if state.global_wmo {
            let pl = placements.by_id.get(&GLOBAL_WMO_UID);
            let up = pl.is_some_and(|p| p.spawned);
            let full = pl.is_some_and(|p| p.spawned && p.doodads.iter().all(|d| d.spawned));
            p.total += 1;
            p.ready += usize::from(full);
            p.focus_resident &= up;
            p.scene_ready &= full;
        }
        // The streamer is the only authority on *which map* the colliders under the player belong
        // to, so it is what lifts the mover's post-snap hold ([`Player::world_stale`]). Residency
        // here means this map's own tile (or, on a WMO-only map, its one building) is spawned —
        // which is only reachable after the swap above drained every tile of the map we left.
        if p.focus_resident && p.total > 0 {
            player.world_stale = false;
        }
        // The settle release (decision 0737): the post-snap hold ends when the destination's world
        // — scene AND colliders — is resident, never on ground contact (feet-on-ground dragged the
        // whole mover-mode matrix into a loading decision; a flyer or swimmer never touched it).
        // It lives HERE, not in a mover, precisely so every mover mode releases the same way.
        // While the resident colliders still belong to the map we just left the deadline is pushed
        // (0710's fail-closed law: a world that never arrives keeps the hold and the screen).
        // `colliders_pending` is `finish_colliders`' published count — fresh this frame (it heads
        // this chain).
        if player.settling {
            let now = time.elapsed_secs();
            if player.world_stale {
                player.settle_deadline = now + crate::player::SETTLE_TIMEOUT;
            } else if p.scene_ready && p.colliders_pending == 0 {
                player.end_settle(true, now);
            } else if now >= player.settle_deadline {
                player.end_settle(false, now);
            }
        }
    }
    activity.stream_ms += t0.elapsed().as_secs_f32() * 1000.0;
}

/// Register one M2 doodad placement: bump the refcount if it's already known, else load its model and
/// record it (spawned later by [`spawn_loaded_placements`] once the asset is ready).
fn register_doodad(placements: &mut Placements, asset_server: &AssetServer, d: &Doodad) {
    if let Some(p) = placements.by_id.get_mut(&d.unique_id) {
        p.refs += 1;
        return;
    }
    let handle: Handle<M2Model> = asset_server.load(m2_url(&d.model));
    placements.by_id.insert(
        d.unique_id,
        Placement {
            model: ModelHandle::M2(handle),
            transform: Transform {
                translation: wow_to_bevy(d.position),
                rotation: placement_rotation(d.rotation),
                scale: Vec3::splat(d.scale),
            },
            entities: Vec::new(),
            spawned: false,
            doodad_set: 0, // M2 doodads carry no doodad set
            name_set: 0,
            doodads: Vec::new(),
            portal_instance: None,
            refs: 1,
        },
    );
}

/// Register one WMO building placement (vanilla scale is 1, so no per-placement scale).
fn register_wmo(placements: &mut Placements, asset_server: &AssetServer, w: &WmoInstance) {
    if let Some(p) = placements.by_id.get_mut(&w.unique_id) {
        p.refs += 1;
        return;
    }
    let handle: Handle<WmoModel> = asset_server.load(wmo_url(&w.model));
    placements.by_id.insert(
        w.unique_id,
        Placement {
            model: ModelHandle::Wmo(handle),
            transform: Transform {
                translation: wow_to_bevy(w.position),
                rotation: placement_rotation(w.rotation),
                scale: Vec3::ONE,
            },
            entities: Vec::new(),
            spawned: false,
            doodad_set: w.doodad_set,
            name_set: w.name_set,
            doodads: Vec::new(),
            portal_instance: None,
            refs: 1,
        },
    );
}

/// Despawn a tile's exclusively-owned entities — the terrain mesh, water surfaces, and per-chunk
/// `ClutterChunk`s (whose built meshes are children, so the despawn cascades). Shared doodad/WMO
/// placements are NOT touched here; they're refcount-released separately.
/// Drop every streamed tile and the placements they reference — the world, released.
///
/// One body, two triggers, because they are the same event seen from different sides: a cross-map
/// teleport ends the world you were in, and so does leaving for character select. The material
/// dedup goes with the placements it deduped for — its strong handles are what kept every
/// previous map's materials (and their textures) resident forever (the #bugs teleport leak), so it
/// is cleared *here*, sharing the exact trigger of the teardown it belongs to, rather than hanging
/// off `world_map::MapChange`.
fn drop_streamed_world(
    commands: &mut Commands,
    state: &mut TerrainStreamer,
    placements: &mut Placements,
    activity: &mut crate::perf::StreamActivity,
) {
    for ((_tx, _ty), t) in state.tiles.drain() {
        despawn_tile_owned(commands, &t);
        activity.tiles_dropped += 1;
        for uid in t.placements {
            release_placement(commands, placements, uid, activity);
        }
    }
    if std::mem::take(&mut state.global_wmo) {
        release_placement(commands, placements, GLOBAL_WMO_UID, activity);
    }
    placements.materials.clear();
}

/// Expire the placement material dedup by **distance** (decision 0793) — the within-map half of
/// [`drop_streamed_world`]'s clear. This is the big one: `mats` 2603 → 26786 over ten minutes on a
/// same-map tour (decision 0785) is overwhelmingly submesh materials of doodads and buildings whose
/// tiles unloaded thousands of yards ago. The placements themselves are already refcount-released;
/// only the dedup kept their materials — and through them their textures — alive.
fn scope_placement_art(mut scope: crate::art_scope::ArtScope, mut placements: ResMut<Placements>) {
    scope.apply(
        &mut placements.materials,
        crate::art_scope::ArtSlot::PlaceMats,
    );
}

/// Leaving `InWorld` (a `/logout` back to character select): release the world.
///
/// `map_dir`/`wdt` are cleared too, so the next entry reads as a fresh map rather than a resumed
/// one — otherwise a re-entry on the SAME map would take the "nothing changed" path and stream
/// against a `TerrainStreamer` whose tiles have all been despawned.
fn release_world(
    mut commands: Commands,
    mut state: ResMut<TerrainStreamer>,
    mut placements: ResMut<Placements>,
    mut progress: Option<ResMut<WorldLoadProgress>>,
    mut activity: ResMut<crate::perf::StreamActivity>,
) {
    if state.tiles.is_empty() && state.map_dir.is_none() {
        return;
    }
    info!(
        "terrain: leaving the world — releasing {} tiles",
        state.tiles.len()
    );
    drop_streamed_world(&mut commands, &mut state, &mut placements, &mut activity);
    state.map_dir = None;
    state.wdt = None;
    state.wdt_ungated = false;
    // The residency the loading screen reads is about a world that no longer exists; leaving it
    // standing would let the next entry clear its cover against the previous world's counts.
    if let Some(p) = progress.as_mut() {
        **p = WorldLoadProgress::default();
    }
}

fn despawn_tile_owned(commands: &mut Commands, t: &TileState) {
    if let Some(e) = t.entity {
        commands.entity(e).try_despawn();
    }
    for &e in t.liquid.iter().chain(&t.clutter) {
        commands.entity(e).try_despawn();
    }
}

/// Mirror the live, spawned WMO placement SET into [`WmoResidency`] each frame, so the interior
/// classifier ([`crate::interior`]) re-tests standing entities when a building streams in/out under
/// them (the down-ray itself reads the live portal instances). Rebuilt wholesale: few WMOs are ever
/// resident, so it's cheap.
fn sync_interior_volumes(placements: Res<Placements>, mut vols: ResMut<WmoResidency>) {
    vols.update(
        placements
            .by_id
            .values()
            .filter_map(|p| match (&p.model, p.spawned) {
                (ModelHandle::Wmo(h), true) => Some(h.id()),
                _ => None,
            }),
    );
}

/// A tile referencing this placement unloaded: drop one ref, despawning its entities (render submeshes
/// + the avian collider entity) at zero — the collider rides the placement's entity lifecycle.
fn release_placement(
    commands: &mut Commands,
    placements: &mut Placements,
    uid: u32,
    activity: &mut crate::perf::StreamActivity,
) {
    let drop_it = match placements.by_id.get_mut(&uid) {
        Some(p) => {
            p.refs -= 1;
            p.refs == 0
        }
        None => false,
    };
    if drop_it {
        if let Some(p) = placements.by_id.remove(&uid) {
            activity.placements_dropped += 1;
            activity.placement_entities_dropped += p.entities.len() as u32;
            for e in p.entities {
                commands.entity(e).try_despawn();
            }
        }
    }
}

/// The terrain base material (a copy of the streamer's — the `TerrainExtension` does the real work).
fn terrain_base_material() -> StandardMaterial {
    StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 1.0,
        double_sided: true,
        cull_mode: None,
        ..default()
    }
}

#[cfg(test)]
mod focus_tests {
    use super::*;

    /// Stratholme's coordinates, and a camera parked where `player::setup` leaves it at boot.
    const PICK: (u32, [f32; 3]) = (329, [3398.9, -3381.8, 142.7]);
    fn cam_at_northshire() -> Option<Vec3> {
        Some(wow_to_bevy([SPAWN_XY.0, SPAWN_XY.1, 100.0]))
    }

    #[test]
    fn the_pick_outranks_the_camera_until_the_avatar_exists() {
        // The regression this guards is invisible and expensive: with the camera winning, world
        // entry spends its scarcest IO on Northshire tiles for a character in Stratholme, then
        // throws them away when the snap lands (decision 0777).
        let idle = Player::default();
        assert_eq!(
            focus_map(Some(0), &idle, Some(PICK)),
            329,
            "the map must be the picked character's, not the boot default"
        );
        assert_eq!(view_focus(&idle, cam_at_northshire(), Some(PICK)), PICK.1);
    }

    #[test]
    fn the_avatar_outranks_the_pick_once_it_is_in_the_world() {
        // `pending_pick` deliberately survives world entry (0065's seamless reconnect re-answers
        // with it), so a stale entry row must never pull the focus off a walking avatar.
        let mut walking = Player::default();
        walking.active = true;
        walking.pos = wow_to_bevy([100.0, 200.0, 30.0]);
        assert_eq!(focus_map(Some(1), &walking, Some(PICK)), 1);
        let f = view_focus(&walking, cam_at_northshire(), Some(PICK));
        assert!(
            (f[0] - 100.0).abs() < 0.01 && (f[1] - 200.0).abs() < 0.01,
            "{f:?}"
        );
    }

    #[test]
    fn free_flight_follows_the_camera_and_a_server_less_run_still_has_one() {
        let mut flying = Player::default();
        flying.active = true;
        flying.detached = true;
        flying.pos = wow_to_bevy([100.0, 200.0, 30.0]);
        let f = view_focus(&flying, cam_at_northshire(), None);
        assert!((f[0] - SPAWN_XY.0).abs() < 0.01, "{f:?}");
        // A capture run: no roster, no avatar — the camera is the only answer, and the last-ditch
        // fallback must still be the anchor rather than the origin.
        assert!(
            (view_focus(&Player::default(), cam_at_northshire(), None)[0] - SPAWN_XY.0).abs()
                < 0.01
        );
        assert_eq!(
            view_focus(&Player::default(), None, None),
            [SPAWN_XY.0, SPAWN_XY.1, 0.0]
        );
    }
}
