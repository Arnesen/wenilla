//! Ground clutter: grass/flowers/pebbles scattered per MCNK cell (authored `GroundEffect*.dbc`
//! placement). At tile-load each chunk's tufts are **scattered** (MCSH/normal-baked) into a per-chunk
//! [`ClutterChunk`]; [`stream_chunk_clutter`] then **builds** its merged meshes lazily only while the
//! chunk is within the ~70 yd detail-doodad horizon and tears them down past it — the reference's
//! per-chunk `CDetailDoodadInst` lifecycle (`ground-effects.md` §7), which bounds live grass to a bubble
//! and spreads the build across frames. [`ClutterPlugin`] owns the catalog + the lazy build lifecycle
//! independently of the terrain streamer; whichever streamer is active does the per-tile *scatter* (it
//! has the tile's chunks) into the `ClutterChunk`s this builds — so the streamer can be swapped.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use crate::assets::LockRecover;
use crate::assets::{AssetSet, WorldAssets};
use crate::player::WorldCamera;
use crate::terrain::WowModelMaterial;
use benilla_assets::coords::wow_to_bevy;
use benilla_formats::{
    load_ground_effect_catalog, load_m2_mesh, scatter_ground_doodads, ChunkMesh,
    GroundDoodadPlacement, GroundEffectCatalog, RenderSubmesh, SHADOW_MAP_SIZE,
};

/// Ground clutter as its own subsystem: loads the `GroundEffect*` catalog + config at startup, and runs
/// the lazy per-chunk build/teardown every frame. The per-tile *scatter* is driven by whichever terrain
/// streamer is active (it has the tile's chunks); this plugin owns the catalog and the build lifecycle
/// so they don't depend on the terrain streamer's own setup (which lets the streamer be swapped).
pub(crate) struct ClutterPlugin;

impl Plugin for ClutterPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_clutter.after(AssetSet::Open))
            .add_systems(Update, stream_chunk_clutter);
    }
}

/// Startup: load the ground-effect (clutter) catalog off the shared chain + insert the live config and
/// the per-chunk geometry cache. (Moved out of the terrain streamer's `setup_terrain`.)
fn setup_clutter(mut commands: Commands, world_assets: Option<ResMut<WorldAssets>>) {
    let Some(world_assets) = world_assets else {
        return;
    };
    // Per-chunk clutter geometry cache, read by the lazy `stream_chunk_clutter` build.
    commands.insert_resource(ClutterGeometry::default());
    commands.insert_resource(ClutterConfig::default());
    let mut chain = world_assets.chain.lock_recover();
    // key_by_internal_id = true: the faithful GroundEffectDoodad keying (see load_ground_effect_catalog).
    match load_ground_effect_catalog(&mut chain, true) {
        Ok(catalog) => {
            info!("ground-effect catalog: {} effects", catalog.len());
            commands.insert_resource(GroundClutter { catalog });
        }
        Err(e) => warn!("ground-effect catalog unavailable, no ground clutter: {e:#}"),
    }
}

/// Ground-clutter rendering: the `effectId → (models, density)` catalog (GroundEffect* DBCs).
/// Optional — if the DBCs fail to load, the ground simply has no detail doodads. Clutter is scattered
/// per chunk into [`ClutterChunk`] units at tile-load (tracked with the tile so it streams out with the
/// terrain) and built lazily by [`stream_chunk_clutter`]. The raw model geometry is cached in
/// [`ClutterGeometry`].
#[derive(Resource)]
pub(crate) struct GroundClutter {
    pub(crate) catalog: GroundEffectCatalog,
}

/// The client's detail-doodad alpha-test reference: `detailDoodadAlpha`, default **128/255 ≈ 0.5**
/// (RE'd in `WoW.exe`). Foliage is an alpha-tested cutout at
/// this ref. We default clutter to it instead of the general (higher) vanilla model key, which was
/// over-cutting the thin grass blades in the shared grass/flower atlas (they vanished while the solid
/// flowers survived).
pub(crate) const DETAIL_DOODAD_ALPHA_REF: f32 = 128.0 / 255.0;

/// The client's detail-doodad draw distance — a **hardcoded 70 yd** (RE'd `_DAT_00867958`); clutter
/// fades out over the last quarter (full to 52.5 yd = 0.75×, gone by 70 yd). This is why the real
/// client's clutter "appears as you walk into it" rather than being drawn to the horizon.
pub(crate) const DETAIL_DOODAD_FADE_FAR: f32 = 70.0;

/// Ground-clutter tunables (live-editable in the debug panel → applied by `apply_clutter`):
/// `density` multiplies the per-chunk cell-visit count (the client's `frillDensity`, faithful=16 at ×1);
/// `scale` resizes each doodad model; `alpha_ref` is the alpha-test cutout threshold
/// ([`DETAIL_DOODAD_ALPHA_REF`]); `fade_far` is the clutter draw-distance horizon (yd,
/// [`DETAIL_DOODAD_FADE_FAR`]; fade starts at 0.75×). Initial values from `$WOW_CLUTTER_DENSITY` /
/// `$WOW_CLUTTER_SCALE` / `$WOW_CLUTTER_ALPHA` / `$WOW_CLUTTER_FADE`; `density 0` disables clutter.
#[derive(Resource, Clone, Copy)]
pub(crate) struct ClutterConfig {
    pub(crate) density: f32,
    pub(crate) scale: f32,
    pub(crate) alpha_ref: f32,
    pub(crate) fade_far: f32,
}

impl Default for ClutterConfig {
    fn default() -> Self {
        let env = |k: &str| std::env::var(k).ok().and_then(|s| s.parse::<f32>().ok());
        Self {
            // Default ×3 = frillDensity 48 = the client's High world-detail preset, which matches the
            // reference (the user's reference runs High). ×1 would be the CVar's bare default (16/Low).
            density: env("WOW_CLUTTER_DENSITY").unwrap_or(3.0).max(0.0),
            scale: env("WOW_CLUTTER_SCALE").unwrap_or(1.0).max(0.01),
            alpha_ref: env("WOW_CLUTTER_ALPHA")
                .unwrap_or(DETAIL_DOODAD_ALPHA_REF)
                .clamp(0.0, 1.0),
            fade_far: env("WOW_CLUTTER_FADE")
                .unwrap_or(DETAIL_DOODAD_FADE_FAR)
                .max(1.0),
        }
    }
}

/// Decoded clutter-model geometry cache (model path → submeshes), keyed by normalized path so each
/// tiny detail M2 is loaded once and then *merged* per chunk. Lives as a resource (not on the streamer)
/// so the lazy per-chunk build system can reach it independently of `WorldAssets`. Replaces the
/// streamer's old per-tile `clutter_cache`.
#[derive(Resource, Default)]
pub(crate) struct ClutterGeometry(pub(crate) HashMap<String, Vec<RenderSubmesh>>);

/// One MCNK chunk's ground clutter as a **lazily-built** unit — the faithful per-chunk `CDetailDoodadInst`
/// lifecycle (`ground-effects.md` §7): scattered + MCSH/normal-baked at tile-load, but its meshes are
/// built only while the chunk is within the detail-doodad horizon and torn down past it. This bounds live
/// grass to a ~70 yd bubble (vs every loaded tile) and spreads the mesh-build over frames instead of one
/// per-tile hitch. Owned by its tile (despawned on unload, which cascades to `built`).
#[derive(Component)]
pub(crate) struct ClutterChunk {
    /// Chunk centre in Bevy space — its distance from the camera gates build vs teardown.
    center: Vec3,
    /// Scattered placements grouped by model path; each becomes one merged mesh per submesh when built.
    models: Vec<(String, Vec<ShadedPlacement>)>,
    /// The spawned merged meshes while built (children of this entity); empty ⇒ not currently built.
    built: Vec<Entity>,
}

/// A chunk centre this far (yd) beyond the fade horizon still has near-corner grass inside the chunk, so
/// we build out to here. ≈ the 33.3 yd chunk's half-diagonal. At the horizon the fade ramp is already
/// alpha 0, so building/tearing down at this range is invisible (no pop) — the same reason the reference
/// ties the build distance to the fade end.
const CLUTTER_BUILD_MARGIN: f32 = 24.0;

/// Hysteresis (yd) between the build distance and the teardown distance, so a chunk hovering at the
/// boundary doesn't thrash build↔despawn every frame.
const CLUTTER_TEARDOWN_HYSTERESIS: f32 = 6.0;

/// Build at most this many chunks' clutter per frame, so entering a dense area (or a teleport) spreads
/// the mesh-build + GPU upload over a few frames instead of one stutter.
const CLUTTER_BUILDS_PER_FRAME: usize = 8;

/// **Step 8 — per-doodad MCSH shadow tint.** Vanilla draws detail doodads UNLIT with a hardcoded
/// DIFFUSE constant: `0xFFFFFFFF ≈ 1.0` (lit) or `0xFFC0C0C0 ≈ 0.753` (MCSH-shadowed). Picked
/// per-vertex from the chunk's static MCSH map at the doodad's placement coord. q12 RE:
/// no day/night, no ambient, no N·L — the only shading
/// input is this grey. Baked into vertex `ATTRIBUTE_COLOR` at scatter time so the model shader
/// can `texture × vertex_color` in gamma space (the faithful MODULATE 1×).
const MCSH_LIT: f32 = 1.0;
const MCSH_SHADOWED: f32 = 192.0 / 255.0; // 0xC0 / 0xFF

/// Sample the chunk's MCSH map at a world-space placement coord. Returns the per-doodad shadow
/// tint (1.0 lit, ≈0.753 shadowed). Falls back to lit when the chunk has no MCSH (most chunks
/// don't) or when the position somehow lands outside the chunk's footprint. Coord math: the
/// chunk's NW corner is `chunk.positions[0]`; +X is north (south distance = `nw_x − wx`), +Y is
/// west (east distance = `nw_y − wy`); MCSH is row-major over the chunk's 0..1 UV with row=south,
/// col=east, 64×64 texels.
fn mcsh_tint_at(chunk: &ChunkMesh, world: [f32; 3]) -> f32 {
    let Some(shadow) = chunk.shadow.as_ref() else {
        return MCSH_LIT;
    };
    let nw_x = chunk.positions[0][0];
    let nw_y = chunk.positions[0][1];
    let south = (nw_x - world[0]) / benilla_formats::TILE_SIZE * 16.0; // 0..1 across the chunk
    let east = (nw_y - world[1]) / benilla_formats::TILE_SIZE * 16.0;
    if !(0.0..=1.0).contains(&south) || !(0.0..=1.0).contains(&east) {
        return MCSH_LIT;
    }
    let n = SHADOW_MAP_SIZE as usize;
    let row = ((south * n as f32) as usize).min(n - 1);
    let col = ((east * n as f32) as usize).min(n - 1);
    if shadow[row * n + col] >= 128 {
        MCSH_SHADOWED
    } else {
        MCSH_LIT
    }
}

/// Sample the chunk's MCNR terrain normal (WoW coords) nearest a world-space position — the **ground
/// normal under the tuft**. Lighting clutter by this (instead of a uniform world-up) makes a tuft
/// follow the terrain beneath it: darker on a slope facing away from the sun, like the dirt under it
/// — the per-position sun/shade variation the reference shows. Nearest outer-grid vertex (≈2 yd
/// spacing) is plenty; falls back to WoW-up (`+Z`) when the chunk has no MCNR.
fn terrain_normal_at(chunk: &ChunkMesh, world: [f32; 3]) -> [f32; 3] {
    if chunk.normals.len() < 145 || chunk.positions.is_empty() {
        return [0.0, 0.0, 1.0];
    }
    let nw_x = chunk.positions[0][0];
    let nw_y = chunk.positions[0][1];
    let south = ((nw_x - world[0]) / benilla_formats::TILE_SIZE * 16.0).clamp(0.0, 1.0);
    let east = ((nw_y - world[1]) / benilla_formats::TILE_SIZE * 16.0).clamp(0.0, 1.0);
    let row = (south * 8.0).round() as usize; // outer 9×9 grid in the stride-17 MCVT layout
    let col = (east * 8.0).round() as usize;
    chunk.normals[row * 17 + col]
}

/// A scattered placement plus its baked MCSH tint and ground normal — what the merge iterates over.
struct ShadedPlacement {
    placement: GroundDoodadPlacement,
    tint: f32,
    /// Terrain (ground) normal under the tuft, **Bevy space** — baked onto every vertex so clutter is
    /// lit by the ground it stands on (`wow_model.wgsl` ground-normal N·L). Same value for all of a
    /// tuft's vertices ⇒ no per-face view-dependence. VERIFIED faithful: WoW writes the terrain
    /// quadrant-plane normal onto the clutter vertex normal channel.
    ground_normal: [f32; 3],
}

/// **Scatter** a tile's ground clutter into per-chunk [`ClutterChunk`] units — WITHOUT building any
/// meshes. Each chunk's placements are MCSH/normal-baked here (while the parent chunk is in scope; the
/// static per-vertex shading matches the real client, baked at map-compile time) and stored on a
/// `ClutterChunk` entity owned by the tile. [`stream_chunk_clutter`] builds the meshes lazily as chunks
/// come within the detail-doodad horizon. Empty chunks are skipped (no entity). Replaces the old
/// build-everything-at-tile-load path.
pub(crate) fn scatter_tile_clutter(
    commands: &mut Commands,
    chunks: &[ChunkMesh],
    tile_x: u32,
    tile_y: u32,
    catalog: &GroundEffectCatalog,
    density: f32,
    entities: &mut Vec<Entity>,
) {
    for chunk in chunks {
        let mut by_model: HashMap<String, Vec<ShadedPlacement>> = HashMap::new();
        for placement in scatter_ground_doodads(chunk, catalog, tile_x, tile_y, density) {
            let tint = mcsh_tint_at(chunk, placement.position);
            let ground_normal =
                wow_to_bevy(terrain_normal_at(chunk, placement.position)).to_array();
            by_model
                .entry(placement.model.clone())
                .or_default()
                .push(ShadedPlacement {
                    placement,
                    tint,
                    ground_normal,
                });
        }
        if by_model.is_empty() {
            continue;
        }
        // Chunk centre (average of its vertices) in Bevy space — what the LOD system measures.
        let n = chunk.positions.len().max(1) as f32;
        let sum = chunk
            .positions
            .iter()
            .fold([0.0f32; 3], |a, p| [a[0] + p[0], a[1] + p[1], a[2] + p[2]]);
        let center = wow_to_bevy([sum[0] / n, sum[1] / n, sum[2] / n]);
        entities.push(
            commands
                .spawn(ClutterChunk {
                    center,
                    models: by_model.into_iter().collect(),
                    built: Vec::new(),
                })
                .id(),
        );
    }
}

/// **Lazily build** one chunk's clutter: merge every instance of each model (within this chunk) into one
/// mesh per submesh — baking each tuft's yaw/pos/scale + MCSH tint + ground normal into the vertices —
/// and spawn them as children of `chunk_entity` (so a tile unload cascades to them). Returns the spawned
/// entities (tracked on the `ClutterChunk` for distance teardown). Same merge as the old per-tile path,
/// now per-chunk so only the ~70 yd bubble is ever built/drawn.
#[allow(clippy::too_many_arguments)]
fn build_chunk_clutter(
    chunk_entity: Entity,
    models: &[(String, Vec<ShadedPlacement>)],
    scale: f32,
    alpha_ref: f32,
    fade_far: f32,
    geometry: &mut ClutterGeometry,
    assets: &mut WorldAssets,
    meshes: &mut Assets<Mesh>,
    images: &mut Assets<Image>,
    materials: &mut Assets<WowModelMaterial>,
    commands: &mut Commands,
) -> Vec<Entity> {
    let mut out = Vec::new();
    for (model_path, placements) in models {
        let subs = geometry.0.entry(model_path.clone()).or_insert_with(|| {
            load_m2_mesh(&mut assets.chain.lock_recover(), model_path).unwrap_or_default()
        });
        if subs.is_empty() {
            continue;
        }
        // One merged mesh per submesh (each submesh has its own texture/blend → material).
        for sub in subs.iter() {
            let vcount = sub.positions.len() * placements.len();
            let mut positions = Vec::with_capacity(vcount);
            let mut uvs = Vec::with_capacity(vcount);
            // Clutter normals = the GROUND normal under each tuft (the terrain quadrant normal WoW
            // bakes onto the clutter vertex channel — NOT the grass-blade M2 normal). Same value on
            // every vertex of a tuft ⇒ no per-face view-dependence; the shader lights it like the
            // terrain beneath.
            let mut normals = Vec::with_capacity(vcount);
            let mut colors = Vec::with_capacity(vcount);
            let mut indices = Vec::with_capacity(sub.indices.len() * placements.len());
            for sp in placements {
                let d = &sp.placement;
                let base = positions.len() as u32;
                let origin = wow_to_bevy(d.position);
                let rot = Quat::from_rotation_y(d.yaw);
                // Per-instance scale (client's random [0.9,1.1]) × the debug-panel size multiplier.
                let inst_scale = scale * d.scale;
                // MCSH tint per placement — same value on every vertex of THIS instance, so all
                // tris of one grass clump share the lit/shadowed colour. The shader multiplies
                // `texture × vertex_color` in gamma space (the faithful MODULATE 1×).
                let tint = [sp.tint, sp.tint, sp.tint, 1.0];
                for (i, p) in sub.positions.iter().enumerate() {
                    positions.push((rot * (wow_to_bevy(*p) * inst_scale) + origin).to_array());
                    uvs.push(sub.uvs[i]);
                    normals.push(sp.ground_normal);
                    colors.push(tint);
                }
                indices.extend(sub.indices.iter().map(|idx| base + idx));
            }
            let mut mesh = Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::default(),
            );
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
            mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
            // Step 8: per-vertex MCSH tint (1.0 lit / ≈0.753 shadowed). Triggers Bevy's
            // `VERTEX_COLORS` shader def → `pbr_input.material.base_color` becomes
            // `vertex_color × texture`. Read by the clutter path in `wow_model.wgsl`.
            mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
            mesh.insert_indices(Indices::U32(indices));
            let mesh = meshes.add(mesh);
            let material = assets.model_material(
                sub.texture.as_deref(),
                sub.blend,
                sub.two_sided,
                Some(alpha_ref), // detail-doodad alpha-test ref (≈0.5), not the higher general key
                Some(fade_far),  // detail-doodad distance fade (≈70 yd horizon — opacity, dc30e1f)
                false,           // clutter is not WMO (keeps its ground-normal N·L path)
                false, // not a doodad fade twin (clutter's own clutter_fade path handles it)
                images,
                materials,
            );
            out.push(
                commands
                    .spawn((Mesh3d(mesh), MeshMaterial3d(material), Transform::IDENTITY))
                    .id(),
            );
        }
    }
    if !out.is_empty() {
        // Parent the freshly-built clutter to its chunk — but tolerate the chunk having been despawned
        // before this command applies. A cross-map teleport (or any tile unload) despawns the tile,
        // which cascades to its `ClutterChunk` children; a plain `commands.entity(..).add_children`
        // then PANICS on the dead parent (the Ashenvale-teleport crash). Check at apply time, and if the
        // parent is gone, despawn the orphan clutter we just spawned so it doesn't leak as parentless
        // (world-baked) geometry.
        let children = out.clone();
        commands.queue(
            move |world: &mut World| match world.get_entity_mut(chunk_entity) {
                Ok(mut parent) => {
                    parent.add_children(&children);
                }
                Err(_) => {
                    for e in children {
                        if let Ok(child) = world.get_entity_mut(e) {
                            child.despawn();
                        }
                    }
                }
            },
        );
    }
    out
}

/// The per-chunk clutter LOD: build a chunk's clutter when its centre comes within the detail-doodad
/// horizon (+ a chunk-radius margin) and tear it down when it leaves — the reference's `CDetailDoodad`
/// lifecycle (`ground-effects.md` §7). Bounds live grass to a ~70 yd bubble (not every loaded tile) and,
/// with the per-frame build cap, spreads the cost so a tile-load no longer builds 256 chunks at once.
/// Build/teardown happen where the fade ramp is already alpha 0, so they're invisible.
#[allow(clippy::too_many_arguments)]
pub(crate) fn stream_chunk_clutter(
    mut commands: Commands,
    cam: Query<&GlobalTransform, With<WorldCamera>>,
    mut chunks: Query<(Entity, &mut ClutterChunk)>,
    cfg: Res<ClutterConfig>,
    mut geometry: ResMut<ClutterGeometry>,
    mut assets: ResMut<WorldAssets>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<WowModelMaterial>>,
) {
    let Some(cam_pos) = cam.iter().next().map(|t| t.translation()) else {
        return;
    };
    let build_d2 = (cfg.fade_far + CLUTTER_BUILD_MARGIN).powi(2);
    let drop_d2 = (cfg.fade_far + CLUTTER_BUILD_MARGIN + CLUTTER_TEARDOWN_HYSTERESIS).powi(2);
    let mut budget = CLUTTER_BUILDS_PER_FRAME;
    for (ent, mut cc) in &mut chunks {
        let d2 = cam_pos.distance_squared(cc.center);
        if cc.built.is_empty() {
            if d2 <= build_d2 && budget > 0 {
                budget -= 1;
                let built = build_chunk_clutter(
                    ent,
                    &cc.models,
                    cfg.scale,
                    cfg.alpha_ref,
                    cfg.fade_far,
                    &mut geometry,
                    &mut assets,
                    &mut meshes,
                    &mut images,
                    &mut materials,
                    &mut commands,
                );
                cc.built = built;
            }
        } else if d2 > drop_d2 {
            for e in cc.built.drain(..) {
                commands.entity(e).try_despawn();
            }
        }
    }
}
