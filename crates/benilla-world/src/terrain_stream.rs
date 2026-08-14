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
use std::time::{Duration, Instant};

use benilla_assets::coords::{bevy_to_wow, placement_rotation, wow_to_bevy};
use benilla_assets::{AdtTile, M2Model, WdtIndex, WmoModel};
use benilla_formats::{world_to_tile, Doodad, WmoInstance};
use bevy::pbr::ExtendedMaterial;
use bevy::prelude::*;
use bevy::render::render_resource::Face;

use crate::clutter::{scatter_tile_clutter, ClutterConfig, GroundClutter};
use crate::collision::{walk_layers, GroundDecalSurface, PickOccluder};
use crate::interior::WmoResidency;
use crate::lighting::SharedLightBuffer;
use crate::liquid::{spawn_liquids, LiquidAssets};
use crate::model_render::MaterialCache;
use crate::schedule::WorldStage;
use crate::view::WorldCamera;
use crate::world_map::CurrentMap;
use benilla_assets::materials::{TerrainExtension, TerrainMaterial};
use benilla_assets::MapCatalogRes;
use benilla_assets::RenderConfig;
use benilla_assets::{m2_url, wmo_url};

mod collider;
mod furnish;
mod queries;
mod spawn;

use collider::{finish_colliders, impassable_wall_data, terrain_collider_data};
use furnish::furnish_tile_cells;
use spawn::prop_light::WmoDoodadInst;
use spawn::spawn_loaded_placements;
// The WMO prop-light machinery lives spawn-side (0830's named carve, executed in 0832); the two
// outside consumers — `crate::interior` and `crate::entities`' `wmo_props` — keep their
// `terrain_stream::X` paths, same as the `queries` items below.
pub use spawn::prop_light::{fold_interior_probe, PropLobeLight};
// The shared placed-model assembler + the off-thread collider build — also the WMO-gameobject
// doodad-prop path's spawner (`crate::entities`' `wmo_props`: the ship's sails ride the streamed
// gameobject entity, and its cargo hulls ride the boat's kinematic body).
pub use collider::{build_collider_task, placement_collider_data, PendingCollider};
pub use spawn::{m2_anim_bound, m2_fade, point_light, spawn_model_entities};
// The position queries + area authority (their home is `queries`; paths stay `terrain_stream::X`).
use queries::update_current_area;
pub use queries::{
    area_id_under, doodad_ground_shade, ground_effect_under, terrain_height_under,
    AreaAuthoritySet, CurrentArea, ShadeResolve,
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
pub struct TerrainStreamer {
    /// Loaded tiles by `(tile_x, tile_y)`. The [`Handle<AdtTile>`] keeps the asset resident; dropping
    /// it (on unload / map swap) lets the `AssetServer` release the tile and its sub-assets.
    tiles: HashMap<(i32, i32), TileState>,
    /// The map directory the loaded tiles are for (e.g. `"Azeroth"`); a change means a cross-map
    /// teleport, so every tile is dropped and re-streamed for the new map.
    map_dir: Option<String>,
    /// The focus tile [`stream_terrain`] last streamed around — the furnisher's nearest-first
    /// ordering key, published here so it never recomputes the focus ladder a second time.
    focus: (i32, i32),
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
    /// Residency counts for the debug panel's World readout: `(spawned, requested)` — tiles fully
    /// up (root + cells furnished) vs. every tile in the stream window (up + loading/furnishing).
    pub fn residency(&self) -> (usize, usize) {
        let spawned = self.tiles.values().filter(|t| t.furnished).count();
        (spawned, self.tiles.len())
    }
}

/// One loaded tile: the resident asset handle, the terrain entity, and the placement ids it references.
struct TileState {
    handle: Handle<AdtTile>,
    /// `None` until the `AdtTile` finishes loading and the terrain root entity is spawned (which is also
    /// when this tile's placements are registered).
    entity: Option<Entity>,
    /// The tile's terrain material, made at root spawn and shared by every cell the furnisher hangs
    /// off the root (one material per tile — the loader's arrays make that possible).
    material: Option<Handle<TerrainMaterial>>,
    /// The furnishing cursor: the next `AdtTile::chunks` index whose cell [`furnish::furnish_tile_cells`]
    /// should build. Cells land a few per frame while live (decision 0832) — the mesh-asset creation
    /// IS the pacing point; everything downstream (extract, prepare, upload) follows its rate.
    next_cell: usize,
    /// `true` once every cell is up (or cells are ablated off): the "this tile is really resident"
    /// bit the loading-screen accounting reads, so a reveal never shows a root with bare ground.
    furnished: bool,
    /// uniqueIds of the doodad/WMO placements this tile references — registered once the tile's `AdtTile`
    /// loads, refcount-released when the tile unloads.
    placements: Vec<u32>,
    /// The tile's water-surface entities (tile-exclusive, like the terrain mesh — despawned on unload).
    liquid: Vec<Entity>,
    /// The tile's impassable-chunk wall collider, if it authors any (decision 1266). Its own static
    /// body rather than a child of the root, matching the per-WMO walk/camera bakes: two colliders
    /// with different audiences are two bodies, not a nested one.
    wall: Option<Entity>,
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

/// How much of the world the streamer wants around the view focus is actually there. Written each
/// frame by [`stream_terrain`] (tiles + placements) and [`finish_colliders`] (the attach queue).
///
/// It lived in `loading_screen` — the engine publishing its own residency fact to the wrong side
/// of 1160's line (1163's finding). Two readers, and only one of them is a screen: the loading
/// bar reads it to draw itself, and the **player's post-snap hold** reads it to decide when to let
/// go (decision 0737 — the physics hold keys on this signal, never on ground contact). Both are
/// the game asking the world what it has; neither is the world asking the game anything.
#[derive(Resource, Default)]
pub struct WorldLoadProgress {
    /// Units done, of [`Self::total`]: desired tiles spawned + focus-neighbourhood placements up.
    pub ready: usize,
    pub total: usize,
    /// Whether the tile under the view focus (the nearest desired tile) is spawned. `false` until the
    /// streamer first runs, and again whenever a teleport/worldport drops the focus onto unloaded
    /// ground — the backstop trigger (covers startup, cross-map worldport, AND far same-map
    /// teleports like a cross-continent `.tele`, which don't change `CurrentMap`). During normal
    /// streaming the focus tile is always resident, so it never fires spuriously.
    pub focus_resident: bool,
    /// The scene term (0737): the focus tile **and its 8 neighbours** are spawned with every
    /// placement they reference (WMOs, doodads, WMO props) — "the buildings and trees around you
    /// exist", which is what makes a reveal read as a world rather than bare terrain. Collider
    /// quiet is deliberately *not* folded in here (it is published a stage apart); consumers use
    /// [`Self::presentable`].
    pub scene_ready: bool,
    /// Outstanding collider attaches (decision 0610's queue) — published by `finish_colliders`.
    pub colliders_pending: usize,
    /// Focus-neighbourhood placements not yet spawned (the wait instrument's term).
    pub placements_pending: usize,
}

impl WorldLoadProgress {
    /// 0..1 residency fraction for the bar — **0.0 when nothing is wanted yet** (the cold-start /
    /// pre-`desired` frame), so the bar starts empty rather than flashing full. (Distinct from the
    /// clear test [`Self::is_ready`], which treats `total == 0` as not-ready.)
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.ready as f32 / self.total as f32).clamp(0.0, 1.0)
        }
    }

    /// True once every wanted unit is resident. Requires `total > 0` so the cold-start / post-swap
    /// frame (before `desired` is computed) doesn't read as "done".
    pub fn is_ready(&self) -> bool {
        self.total > 0 && self.ready >= self.total
    }

    /// The world at the focus is presentable — the scene term plus a quiet collider queue. What
    /// both consumers (the screen clear here, the settle release in the streamer) key on.
    pub fn presentable(&self) -> bool {
        self.scene_ready && self.colliders_pending == 0
    }
}

/// **Where the world should stream from** — the engine's one required input besides
/// [`crate::world_map::CurrentMap`], written by whatever owns a viewer and read by every
/// streaming lane (terrain, the WDL ring, the area authority, the form furnisher).
///
/// The streamer used to read `player::Player` directly, which is 1160's wire (a): the engine
/// asking the game where the avatar is. Inverted, the question becomes the game's to answer, and
/// a second program with no avatar at all (`benilla-worldview`) answers it from its free-fly
/// camera without stubbing a player.
///
/// The ladder it encodes, unchanged (`0x695650`-era behaviour, decision 0772): a live attached
/// avatar wins; with no avatar, the picked character's entry row; else the camera; else the spawn
/// point. A **detached** eye (free-fly) skips straight to the camera — but keeps publishing the
/// body's position, because the zone authority follows the character and not the camera.
#[derive(Resource, Default, Clone, Copy)]
pub struct ViewFocus {
    /// The avatar's world position (wow coords) when one is live — present even while detached.
    body: Option<[f32; 3]>,
    /// Is the eye following the body? `false` in free-fly, where the stream follows the camera.
    attached: bool,
    /// The picked character's map + position, for the entry window before an avatar exists.
    entry: Option<(u32, [f32; 3])>,
    /// Is the focus **settled** — steady enough to pace spawning behind the per-frame caps?
    /// `false` through entry, a teleport and a world swap, when the loading cover is absorbing the
    /// burst and a cap would only lengthen the reveal.
    pub(crate) paced: bool,
}

impl ViewFocus {
    /// No viewer at all — follow the camera, or the spawn point if there is not even one.
    pub fn camera() -> Self {
        Self::default()
    }

    /// A live avatar at `wow`, with the eye on it. `paced` is the settled bit.
    pub fn body(wow: [f32; 3], paced: bool) -> Self {
        Self {
            body: Some(wow),
            attached: true,
            entry: None,
            paced,
        }
    }

    /// A live avatar whose eye has been **detached** (free-fly): the stream follows the camera,
    /// the zone authority still follows the body.
    pub fn detached(wow: [f32; 3], paced: bool) -> Self {
        Self {
            attached: false,
            ..Self::body(wow, paced)
        }
    }

    /// No avatar yet — the picked character's own map and position, so world entry streams the
    /// destination rather than wherever the camera was parked.
    pub fn entry(map: u32, wow: [f32; 3]) -> Self {
        Self {
            entry: Some((map, wow)),
            ..Self::default()
        }
    }

    /// The point to stream around, wow coords.
    pub(crate) fn resolve(&self, camera: Option<Vec3>) -> [f32; 3] {
        if let (Some(pos), true) = (self.body, self.attached) {
            return pos;
        }
        if self.body.is_none() {
            if let Some((_, pos)) = self.entry {
                return pos;
            }
        }
        match camera {
            Some(t) => bevy_to_wow(t),
            None => [SPAWN_XY.0, SPAWN_XY.1, 0.0],
        }
    }

    /// Which map the streamers should be loading — `current` once the server has said, and the
    /// picked character's own map for the entry window before that. It **must** agree with
    /// [`Self::resolve`]: a position from one map against the tile grid of another names tiles
    /// that exist but are somewhere else entirely.
    pub(crate) fn map(&self, current: Option<u32>) -> u32 {
        if self.body.is_none() {
            if let Some((map, _)) = self.entry {
                return map;
            }
        }
        current.unwrap_or(0)
    }

    /// The **body's** own position when there is one — what the zone/area authority measures from,
    /// which is the character and never the free-fly camera.
    pub(crate) fn body_pos(&self) -> Option<[f32; 3]> {
        self.body
    }

    /// Is there a body **and** is the world under it the one it is standing in?
    ///
    /// The area authority's gate (1287) — the same bit [`ViewFocus::paced`] carries, read through
    /// its own name because it answers a different question here. Through entry, a teleport and a
    /// world swap the body has been snapped but its destination is still streaming, so the leaf
    /// area under it is whatever tile happens to be resident: at first login that is the pre-snap
    /// position's — the map centre, Eastern Plaguelands on map 0 and The Barrens on map 1 — and a
    /// beat later the outdoor parent of an interior whose WMO claim has not landed yet. Publishing
    /// those makes the client announce, play the music of, and join the channels of zones the
    /// player was never in.
    pub(crate) fn body_settled(&self) -> bool {
        self.body.is_some() && self.paced
    }
}

/// **Where the world streams from when nothing else can say** — the Human start (Northshire),
/// where `one`/`One` logs in, so a viewer sits in the middle of the loaded block rather than at
/// Stormwind's edge.
///
/// The last rung of [`ViewFocus`]'s ladder, and the reason it lives here rather than at the crate
/// root: it is the *streamer's* fallback, and an engine that cannot answer "where do I stream
/// from" with no game attached is not an engine. It was a `lib.rs` const, which made it invisible
/// to both walls — neither could see a crate-root item at all until the scan learned to.
pub const SPAWN_XY: (f32, f32) = (-8949.95, -132.49);

/// A placement's model asset handle — an M2 doodad or a WMO building. Keeps the asset resident.
enum ModelHandle {
    M2(Handle<M2Model>),
    Wmo(Handle<WmoModel>),
}

/// The terrain streamer plugin (added by `main` as the world's terrain owner).
/// **Per-frame streamer activity** — what this streamer did this frame, bumped by the
/// `WorldStage::Stream` chain as it works (a handful of integer adds + five self-timers,
/// maintained unconditionally and zeroed by whoever reads them).
///
/// Exists because B181's symptom — a frame spike on every ADT tile-boundary crossing — is keyed to
/// *what the streamer did that frame*, which no 1 Hz journal row can attribute: the spike frame
/// needs its own row saying what was dropped, requested and spawned beside what the frame cost.
/// `perf`'s stream trace is that reader, and it used to own this resource too, which meant the
/// engine's own streaming account only existed if an instrument was installed (decision 1164 —
/// the same mis-filing as `terrain_stream` writing its residency into `loading_screen`). The
/// writer owns it now; the instrument reads it.
#[derive(Resource, Default)]
pub struct StreamActivity {
    /// Tiles whose owned entities (terrain root + cells, liquid, clutter) were despawned.
    pub tiles_dropped: u32,
    /// Shared placements whose refcount hit zero (their entities despawned with them).
    pub placements_dropped: u32,
    /// Entity handles despawned with those placements (submeshes, colliders, lights, props).
    pub placement_entities_dropped: u32,
    /// New ADT asset requests fired.
    pub tiles_requested: u32,
    /// Loaded tiles spawned (root + collider kicked off + placements registered + liquid + clutter).
    pub tiles_spawned: u32,
    /// MCNK cell meshes built + spawned by the paced furnisher (decision 0832).
    pub cells_spawned: u32,
    /// Model submesh render forms built by the paced model furnisher (decision 0834).
    pub model_meshes_built: u32,
    /// Placements (or WMO props) whose model landed and spawned.
    pub placements_spawned: u32,
    /// Off-thread colliders attached.
    pub colliders_attached: u32,
    /// Self-time of `stream_terrain` / `furnish_tile_cells` / `furnish_model_forms` /
    /// `spawn_loaded_placements` / `finish_colliders` (ms).
    pub stream_ms: f32,
    pub furnish_ms: f32,
    pub mfurnish_ms: f32,
    pub spawn_ms: f32,
    pub collider_ms: f32,
}

impl StreamActivity {
    /// Did the streamer *do* anything this frame? (The self-timers don't count: the desired-set
    /// scan runs every frame and would make every row an "event".)
    pub fn any_event(&self) -> bool {
        self.tiles_dropped
            + self.placements_dropped
            + self.tiles_requested
            + self.tiles_spawned
            + self.cells_spawned
            + self.model_meshes_built
            + self.placements_spawned
            + self.colliders_attached
            > 0
    }
}

pub(crate) struct TerrainPlugin;

impl Plugin for TerrainPlugin {
    fn build(&self, app: &mut App) {
        // The engine's own input resources, defaulted HERE so a second program that
        // registers no game at all still boots: with no writer, `ViewFocus::camera()` is the
        // free-fly answer and the streamer follows the camera (this is what `benilla-worldview`
        // does, and why the spike needed a stub `Player` before this inversion).
        app.init_resource::<ViewFocus>()
            .init_resource::<WorldLoadProgress>()
            .init_resource::<TerrainStreamer>()
            .init_resource::<StreamActivity>()
            .init_resource::<Placements>()
            .init_resource::<crate::model_forms::ModelForms>()
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
                    furnish_tile_cells,
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
            // The model-forms furnisher (decision 0834) runs UNGATED, unlike the chain above: the
            // glue screens' character preview requests forms pre-world (`update_display_models`
            // serves the glue stage too), and a world-live gate would starve it forever. Ordered
            // before the placement spawner so a form completed this frame can land this frame;
            // everything it does is demand-driven, so with no requests it is a no-op.
            .add_systems(
                Update,
                crate::model_forms::furnish_model_forms
                    .in_set(WorldStage::Stream)
                    .before(spawn_loaded_placements),
            )
            // Leaving the world releases it. Without this, a `/logout` back to character select
            // would leave the whole streamed world resident and unowned — the gate above stops it
            // being *maintained*, which is precisely what makes an abandoned world immortal.
            .add_systems(
                Update,
                release_world
                    .run_if(crate::schedule::world_left)
                    // Between the wire drain and the stream stage, so the release happens on the
                    // frame the world went away — the promptness `OnExit` used to give it. The
                    // session's publisher runs before `Net`, so this reads THIS frame's edge.
                    .after(crate::schedule::WorldStage::Net)
                    .before(crate::schedule::WorldStage::Stream),
            )
            // Within-map art residency (decision 0793): the placement material dedup expires by
            // distance, so a long flight on one continent stops ratcheting. Ungated by
            // `world_is_live` on purpose — a cache still holding the last world's art is exactly
            // what should be draining while you sit at character select.
            .add_systems(Update, scope_placement_art)
            // The WorldDetail re-scatter runs UNGATED: out of world the tile map is empty (a
            // no-op walk) and the density tracker must never miss a boot-time config apply.
            .add_systems(Update, rescatter_clutter)
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
        ResMut<StreamActivity>,
    ),
    liquid_assets: Option<Res<LiquidAssets>>,
    clutter: Option<Res<GroundClutter>>,
    clutter_cfg: Option<Res<ClutterConfig>>,
    focus: Res<ViewFocus>,
    camera: Query<&Transform, With<WorldCamera>>,
    shared_light: Option<Res<SharedLightBuffer>>,
    cfg: Option<Res<RenderConfig>>,
    // "Where are we" — the server's map, the catalog that names its directory, and the roster row
    // of a pick still in flight (the entry window's focus). Bundled for the same param-limit
    // reason as `asset_stores`.
    location: (Option<Res<CurrentMap>>, Option<Res<MapCatalogRes>>),
    mut load_progress: Option<ResMut<WorldLoadProgress>>,
) {
    let (mut materials, mut meshes, wdts, _time, mut activity) = asset_stores;
    let (current_map, map_catalog) = location;
    // The shared light buffer + map catalog are set up by other plugins' startup; until they exist
    // there's nothing to stream against, so idle.
    let (Some(shared_light), Some(map_catalog)) = (shared_light, map_catalog) else {
        return;
    };
    let t0 = Instant::now();
    let placements = placements.into_inner();
    let map_id = focus.map(current_map.map(|m| m.0));
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

    // Stream around the *view focus* — see [`ViewFocus`] for the ladder.
    let center = focus.resolve(camera.single().ok().map(|c| c.translation));
    let radius = cfg.as_ref().map(|c| c.tile_radius as i32).unwrap_or(2);
    let tiling = cfg.as_ref().map(|c| c.tex_tiles).unwrap_or(8.0);
    let (cx, cy) = world_to_tile(center[0], center[1]);
    let (cx, cy) = (cx as i32, cy as i32);
    state.focus = (cx, cy);

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
        let live = focus.paced;
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
                    material: None,
                    next_cell: 0,
                    furnished: false,
                    placements: Vec::new(),
                    liquid: Vec::new(),
                    wall: None,
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
        // …and, separately, the tile's impassable-chunk fences (decision 1266, report B129). A
        // SECOND collider because the audience is the point: the reference's fence is emitted only
        // into the movement box gather, and its segment/ray path never reads the flag at all. So —
        // the walk layer, where the body sees it and the camera boom does not, and unmarked, so it
        // neither takes the selection ring nor clamps the mouse pick. An authored wall is not scenery.
        let wall_data = impassable_wall_data(&adt.chunks);
        // The tile ROOT draws nothing: it carries the collider and the surface roles, and its MCNK
        // cells hang off it as children — one drawn object per 33.333 yd chunk. That is the unit the
        // exterior-scene cull needs (decision 0780; a 533 yd slab the camera stands on intersects
        // every portal window, so it could never be hidden), and it also gives Bevy's own frustum cull
        // something smaller than half a kilometre to reject. Despawning the root takes the cells.
        //
        // The cells themselves do NOT spawn here (decision 0832): a whole row of tiles finishes
        // decoding in one frame, and spawning ~1300 loader-built cell meshes at once handed the
        // render world their entire extract/prepare/upload as one 80–105 ms (process-CPU) frame —
        // the B181 first-contact spike. `furnish_tile_cells` (chained right after this system)
        // builds them from `AdtTile::chunks` + `shading` a few per frame while live.
        let mut tile_ent = commands.spawn((Transform::IDENTITY, Visibility::default()));
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
        tile.wall = wall_data.map(|(verts, tris)| {
            commands
                .spawn(PendingCollider::new(
                    build_collider_task(verts, tris),
                    Some(walk_layers()),
                    true,
                ))
                .id()
        });
        tile.material = Some(material);
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
            adt.chunks.iter().flat_map(|c| c.liquids.iter()),
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
        // "Spawned" means FURNISHED (root + every cell): the furnisher is uncapped while the
        // cover is up, so this costs entry nothing — but it keeps a reveal from ever showing a
        // root whose ground is still landing (`furnished` lags root spawn by one frame here,
        // since the furnisher runs after this system in the same chain).
        let spawned = |c: &(i32, i32)| state.tiles.get(c).is_some_and(|t| t.furnished);
        p.total = desired.len();
        p.ready = desired.iter().filter(|c| spawned(c)).count();
        p.focus_resident = state.tiles.get(&(cx, cy)).is_none_or(|t| t.furnished);
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
                    Some(t) if t.furnished => {
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
        // Residency is *published* here and read by the mover's post-snap hold one stage later
        // (`player::release_post_snap_hold`). The streamer is still the only authority on which
        // map the colliders under the avatar belong to — it just states the fact now instead of
        // reaching across 1160's line to act on it (decision 0737's release, inverted).
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
    activity: &mut StreamActivity,
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
    mut forms: ResMut<crate::model_forms::ModelForms>,
    mut progress: Option<ResMut<WorldLoadProgress>>,
    mut activity: ResMut<StreamActivity>,
) {
    if state.tiles.is_empty() && state.map_dir.is_none() {
        return;
    }
    info!(
        "terrain: leaving the world — releasing {} tiles",
        state.tiles.len()
    );
    // The model-form cache frees per asset on `Unused` events; leaving the world drops the whole
    // cache in one deterministic sweep instead — nothing world-scoped may survive a logout, and
    // an entry whose asset another holder keeps resident would otherwise pin its meshes forever.
    forms.clear();
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
    for e in t.entity.into_iter().chain(t.wall) {
        commands.entity(e).try_despawn();
    }
    for &e in t.liquid.iter().chain(&t.clutter) {
        commands.entity(e).try_despawn();
    }
}

/// The `WorldDetail` re-scatter (0992): 1.12's own setter law — writing the density CVar
/// tail-calls the chunk-rebuild walk (`0x6725a0` → `0x6b1d20`, wow-re terrain.md), so a change
/// re-scatters the LOADED tiles too, not just future streams. The fresh `ClutterChunk`s spawn
/// unbuilt and the lazy builder re-meshes the ~70 yd bubble over the next frames. Watches the
/// VALUE, not `is_changed()`: the cvar sync's `Knobs` construction deref-muts every knob
/// resource whenever any cvar moves, so the flag over-fires (cvars.rs notes the same trap).
/// First sight only arms.
fn rescatter_clutter(
    mut commands: Commands,
    mut streamer: ResMut<TerrainStreamer>,
    adts: Res<Assets<AdtTile>>,
    clutter: Option<Res<GroundClutter>>,
    cfg: Res<ClutterConfig>,
    mut last: Local<Option<f32>>,
) {
    let prev = last.replace(cfg.density);
    let (Some(prev), Some(clutter)) = (prev, clutter) else {
        return;
    };
    if prev == cfg.density {
        return;
    }
    let mut n = 0;
    for (&(tx, ty), tile) in streamer.tiles.iter_mut() {
        if tile.entity.is_none() {
            continue;
        }
        let Some(adt) = adts.get(&tile.handle) else {
            continue;
        };
        for e in tile.clutter.drain(..) {
            commands.entity(e).try_despawn();
        }
        scatter_tile_clutter(
            &mut commands,
            &adt.chunks,
            tx as u32,
            ty as u32,
            &clutter.catalog,
            cfg.density,
            &mut tile.clutter,
        );
        n += 1;
    }
    if n > 0 {
        info!(
            "clutter: world detail x{} — re-scattered {n} tiles",
            cfg.density
        );
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
    activity: &mut StreamActivity,
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

/// The terrain base material (the `TerrainExtension` does the real work).
///
/// **Single-sided, backface-culled** — the ground is see-through from underneath, exactly as the
/// real client renders it (decision 0960; B193: testers see through 1.12.1's terrain from below, the
/// way a WMO exterior reads from inside a cave; we drew it solid). The reference never sets
/// `EGxRs 0x14` (`GL_CULL_FACE`) in its terrain-chunk pass `0x684510`/`0x6beb50`, so terrain inherits
/// the device baseline `0x14 = 1` written by `0x593bf0` — culling ON, and wow-re's census of all 39
/// `0x14` setters proves none leaks in unbracketed ahead of the terrain drain. Only the passes that
/// *want* two sides clear it inside their own `Push`/`PopRenderState` bracket: the four liquid
/// passes, and the WDL mesh `0x6bd780` (cull 0 at `0x6bd79d`) — which is why `wdl.rs` stays
/// `cull_mode: None` while this one culls. Measured, not only derived: in the reference's own
/// apitrace all 103 MCNK-format draws run cull-enabled and all 20 WDL draws cull-disabled.
/// `double_sided: true` here was a Bevy default that no decision ever stood behind.
///
/// Rests on the winding: `terrain_fans_wind_ccw_seen_from_above` (benilla-formats) pins every MCNK
/// fan CCW-from-above in WoW space, and `transform_is_a_proper_rotation` pins the WoW→Bevy map at
/// det +1 — so the ground's front face points up under wgpu's `FrontFace::Ccw`. Flip either and
/// terrain disappears from *above*.
fn terrain_base_material() -> StandardMaterial {
    StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 1.0,
        double_sided: false,
        cull_mode: Some(Face::Back),
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
        let idle = ViewFocus::entry(PICK.0, PICK.1);
        assert_eq!(
            idle.map(Some(0)),
            329,
            "the map must be the picked character's, not the boot default"
        );
        assert_eq!(idle.resolve(cam_at_northshire()), PICK.1);
    }

    #[test]
    fn the_avatar_outranks_the_pick_once_it_is_in_the_world() {
        // `pending_pick` deliberately survives world entry (0065's seamless reconnect re-answers
        // with it), so a stale entry row must never pull the focus off a walking avatar.
        let walking = ViewFocus::body([100.0, 200.0, 30.0], true);
        assert_eq!(walking.map(Some(1)), 1);
        let f = walking.resolve(cam_at_northshire());
        assert!(
            (f[0] - 100.0).abs() < 0.01 && (f[1] - 200.0).abs() < 0.01,
            "{f:?}"
        );
    }

    #[test]
    fn free_flight_follows_the_camera_and_a_server_less_run_still_has_one() {
        let flying = ViewFocus::detached([100.0, 200.0, 30.0], true);
        let f = flying.resolve(cam_at_northshire());
        assert!((f[0] - SPAWN_XY.0).abs() < 0.01, "{f:?}");
        // …and the body is still published, because the zone authority follows the character.
        assert_eq!(flying.body_pos(), Some([100.0, 200.0, 30.0]));
        // A capture run: no roster, no avatar — the camera is the only answer, and the last-ditch
        // fallback must still be the anchor rather than the origin.
        assert!((ViewFocus::camera().resolve(cam_at_northshire())[0] - SPAWN_XY.0).abs() < 0.01);
        assert_eq!(
            ViewFocus::camera().resolve(None),
            [SPAWN_XY.0, SPAWN_XY.1, 0.0]
        );
    }
}
