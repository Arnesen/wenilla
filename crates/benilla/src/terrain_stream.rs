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

use crate::assets::RenderConfig;
use crate::clutter::{scatter_tile_clutter, ClutterConfig, GroundClutter};
use crate::collision::{GroundDecalSurface, PickOccluder};
use crate::interior::WmoResidency;
use crate::lighting::SharedLightBuffer;
use crate::liquid::{spawn_liquids, LiquidAssets};
use crate::loading_screen::WorldLoadProgress;
use crate::model_render::{m2_url, wmo_url, MaterialCache};
use crate::player::{Player, WorldCamera};
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
    /// The prop's owning WMO group (its MODR referencer) — the portal-cull key, so a prop is hidden
    /// with the room it furnishes. `None` for a MODD no group references (the reference never
    /// instantiates one at all; we still show it, uncullable, rather than change what draws today).
    group: Option<u16>,
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
            .add_systems(
                Update,
                (
                    stream_terrain,
                    spawn_loaded_placements,
                    sync_interior_volumes,
                )
                    .chain(),
            )
            // Attaches off-thread-built colliders when ready (independent of the streaming chain).
            .add_systems(
                Update,
                (
                    finish_colliders,
                    // After the interior claim so the leaf override reads THIS frame's claim —
                    // the client resolves leaf + indoor + names from ONE node state in one pass
                    // (`0x67e510`); a stale-leaf/fresh-claim frame let the abbey login big-splash
                    // (the A ≠ subzone gate saw "Northshire Valley" against the abbey's name).
                    // `AreaAuthoritySet` lets the zone-text feed order after this in turn.
                    update_current_area
                        .after(crate::wmo_portal::WmoPvsSet)
                        .in_set(AreaAuthoritySet),
                ),
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
    ),
    liquid_assets: Option<Res<LiquidAssets>>,
    clutter: Option<Res<GroundClutter>>,
    clutter_cfg: Option<Res<ClutterConfig>>,
    player: Res<Player>,
    camera: Query<&Transform, With<WorldCamera>>,
    shared_light: Option<Res<SharedLightBuffer>>,
    cfg: Option<Res<RenderConfig>>,
    current_map: Option<Res<CurrentMap>>,
    map_catalog: Option<Res<MapCatalogRes>>,
    mut load_progress: Option<ResMut<WorldLoadProgress>>,
) {
    let (mut materials, mut meshes, wdts) = asset_stores;
    // The shared light buffer + map catalog are set up by other plugins' startup; until they exist
    // there's nothing to stream against, so idle.
    let (Some(shared_light), Some(map_catalog)) = (shared_light, map_catalog) else {
        return;
    };
    let placements = placements.into_inner();
    let map_id = current_map.map(|m| m.0).unwrap_or(0);
    let Some(dir) = map_catalog.0.directory(map_id).map(str::to_string) else {
        return;
    };

    // Cross-map teleport: the map directory changed → drop every loaded tile (despawn + release the
    // handles + placements) so the new map streams in fresh, and request the new map's WDT (the
    // tile-existence index every ADT request below consults).
    if state.map_dir.as_deref() != Some(dir.as_str()) {
        for ((_tx, _ty), t) in state.tiles.drain() {
            despawn_tile_owned(&mut commands, &t);
            for uid in t.placements {
                release_placement(&mut commands, placements, uid);
            }
        }
        if std::mem::take(&mut state.global_wmo) {
            release_placement(&mut commands, placements, GLOBAL_WMO_UID);
        }
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

    // Stream around the *view focus*: the avatar in third-person, but the free-flying camera itself
    // while detached or before we've connected — so the world loads around wherever you look.
    let center = if player.active && !player.detached {
        bevy_to_wow(player.pos)
    } else if let Ok(cam) = camera.single() {
        bevy_to_wow(cam.translation)
    } else {
        [SPAWN_XY.0, SPAWN_XY.1, 0.0]
    };
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

    // Unload tiles no longer desired: despawn the terrain entity, release the placements, drop the handle.
    let stale: Vec<(i32, i32)> = state
        .tiles
        .keys()
        .copied()
        .filter(|c| !desired.contains(c))
        .collect();
    for c in stale {
        if let Some(t) = state.tiles.remove(&c) {
            despawn_tile_owned(&mut commands, &t);
            for uid in t.placements {
                release_placement(&mut commands, placements, uid);
            }
        }
    }

    // Request newly-desired tiles (the `AssetServer` dedups, so re-requesting a loaded one is
    // free) — only once the WDT has answered (or failed into the ungated fallback): a request
    // fired before the index lands could probe a tile that doesn't exist.
    if wdt_index.is_some() || state.wdt_ungated {
        for &(tx, ty) in &desired {
            state.tiles.entry((tx, ty)).or_insert_with(|| TileState {
                handle: asset_server.load(format!("mpq://World/Maps/{dir}/{dir}_{tx}_{ty}.adt")),
                entity: None,
                placements: Vec::new(),
                liquid: Vec::new(),
                clutter: Vec::new(),
            });
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
        // Terrain collider (decision 0009): a static trimesh from the SAME merged world-space verts that
        // are drawn (so you stand on the visible ground), built off-thread (attached by
        // `finish_colliders`) so a tile streaming in never hitches the frame. It rides the tile entity's
        // lifecycle — gone when the tile despawns, no separate bookkeeping.
        let collider_data = meshes.get(&adt.mesh).and_then(terrain_collider_data);
        let mut tile_ent = commands.spawn((
            Mesh3d(adt.mesh.clone()),
            MeshMaterial3d(material),
            Transform::IDENTITY, // the merged mesh is already in absolute world coords
        ));
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

    // Publish residency for the loading screen: how many desired tiles are actually spawned, and
    // whether the tile under the view focus is up (the screen clears once it is — covers the cold-start
    // burst + the post-teleport gap). A focus tile that doesn't exist (map edge) counts as resident so
    // we never get stuck waiting for ground that isn't there.
    if let Some(p) = load_progress.as_mut() {
        let spawned = |c: &(i32, i32)| state.tiles.get(c).is_some_and(|t| t.entity.is_some());
        p.total = desired.len();
        p.ready = desired.iter().filter(|c| spawned(c)).count();
        p.focus_resident = state
            .tiles
            .get(&(cx, cy))
            .is_none_or(|t| t.entity.is_some());
        // Until the WDT answers we don't yet know what this map is made of, so nothing under the
        // focus can be called resident. Without this the post-worldport frames before the index
        // lands read as "ground is up" — harmless on an ADT map (the tile entries below close the
        // gap a frame later) but on a WMO-only map, where `desired` is empty forever, it is the
        // difference between a loading screen and a glimpse of the void.
        if wdt_index.is_none() && !state.wdt_ungated {
            p.focus_resident = false;
        }
        // A WMO-only map's residency IS its one building (0688) — there are no tiles to count, so
        // the bar and the clear-condition ride the placement instead. Counting it keeps
        // `total > 0`, which is what `is_ready` requires before it will clear the screen at all.
        if state.global_wmo {
            let up = placements
                .by_id
                .get(&GLOBAL_WMO_UID)
                .is_some_and(|p| p.spawned);
            p.total += 1;
            p.ready += usize::from(up);
            p.focus_resident &= up;
        }
    }
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
fn release_placement(commands: &mut Commands, placements: &mut Placements, uid: u32) {
    let drop_it = match placements.by_id.get_mut(&uid) {
        Some(p) => {
            p.refs -= 1;
            p.refs == 0
        }
        None => false,
    };
    if drop_it {
        if let Some(p) = placements.by_id.remove(&uid) {
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
