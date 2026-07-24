//! Liquid (water) rendering: the animated lake/river/ocean surfaces the reference draws over MCLQ
//! geometry. The parse + flat mesh live in `benilla_formats::liquid` (built into each `ChunkMesh.liquid`
//! by the terrain loader); this is the Bevy render glue — one shared [`LiquidMaterial`] per
//! [`LiquidKind`], a `texture_2d_array` of its animated frames, and a 24 fps frame-index cycler.
//!
//! Faithful model (RE'd from `WoW.exe` + `ocean0_s.bls` + apitrace WoW.17 program 159, all agree).
//! `ocean0_s.bls`: `rgb = primary·colorTex.rgb + detailTex.rgb + (secondary+0.25)·detailTex.a`,
//! `alpha = colorTex.a`. The body colour is **`primary · waterTint`**, where:
//! - **`waterTint`** is a plain **2-endpoint linear lerp** of the zone's dedicated `Light.dbc` water
//!   rows, RAW (no ×0.711): IntBand rows 16/17 (river/lake) or 14/15 (ocean), shallow→deep, by the
//!   per-vertex depth `V` (river/lake `V = clamp(byte/42)`, VERIFIED `c81768`/`FUN_0068d790`; saturates
//!   ~5 yd so the channel middle reaches the deep/teal row). Swatch builder VERIFIED: WoW.exe
//!   `FUN_0068a830`, golden-vector-matched to the apitrace swatch ≤1/255 over all 64 rows. (The earlier
//!   "reflected sky × 0.711 via `FUN_0068c250`" model fingered the WRONG builder — a separate grey edge
//!   texture never bound on the water unit; and `byte/255` was the wrong LUT → river never went teal.)
//! - **`primary`** is the lit vertex colour `clamp(ambient + N·L·sun)`.
//! - the animated `lake_a`/`ocean_h` frame is the **`detailTex`** (near-black RGB + ripple alpha): a
//!   faint flat lift + an achromatic shimmer on crests — NOT the body colour. Mipped + 16× aniso so the
//!   ripple averages out at distance (near-field samples mip 0, so near sparkle is the term itself).
//! - **opacity** = the SAME `V` indexes both colour and alpha (one swatch row → RGB + A): a ramp between
//!   the LightParams shallow/deep alphas — river 0.5→1.0, ocean 0.75→1.0 (VERIFIED WoW.exe `FUN_0068a830`
//!   α = `127+2·row`). The river channel reaches α=1.0 (opaque, deep teal) by byte 42 ≈ 5 yd; the shore
//!   stays see-through (the pale edge band, faithful — the bottom shows).
//!
//! River/lake `V = clamp(byte/42)` (steep `c81768` LUT, `FUN_0068d790`) — NOT `byte/255` (the `c7fcd8`
//! LUT, a different draw list the from-above river path doesn't use; it left the river middle stuck on
//! the shallow green row). Ocean uses a non-LUT UV path → placeholder `/255` pending its own RE+A/B.
//! (Earlier cuts: ripple-as-colour → black; `×8` → "deep too early"; FLAT colour → "completely gone";
//! sky × 0.711 → wrong builder; `byte/255` → wrong LUT, no teal centre. Faithful = rows 14–17 raw lerp
//! + the /42 V. 2026-05-31.)
//!
//! Two-sided, alpha-blended, depth-write off (Bevy's transparent pass = the verified MCLQ water render
//! state).
//!
//! The frame-flip is the client's first render animation — a deliberate **one-off** (a frame-index
//! uniform off Bevy real `Time`), NOT a general animation system. Two clocks: animation =
//! wall-clock; day/night = server game-time.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::{ExtendedMaterial, MaterialPlugin};
use bevy::prelude::*;

use crate::assets::LockRecover;
use crate::assets::{liquid_frame_array, AssetSet, RenderConfig, WorldAssets};
use crate::lighting::{
    WowLighting, OCEAN_SHALLOW_ALPHA, RIVER_SHALLOW_ALPHA, WATER_DEEP_ALPHA, WATER_SHININESS,
};
use crate::player::WorldCamera;
use crate::terrain::{LiquidExt, LiquidMaterial};
use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use benilla_formats::{read_texture_mip_chain, BlpMipChain, LiquidKind, LiquidMesh};

/// Frame-flip rate — 30 frames over 1.25 s (VERIFIED `FUN_0068aac0`), i.e. 24 fps, real wall-clock.
const ANIM_FPS: f32 = 24.0;

/// The water subsystem: load the per-kind frame arrays + shared materials at startup, then cycle the
/// animation frame each update. Spawning the per-chunk surfaces happens in the terrain streamer (via
/// [`spawn_liquids`], water lives *with* its tile), reading [`LiquidAssets`].
pub(crate) struct LiquidPlugin;

impl Plugin for LiquidPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<LiquidMaterial>::default())
            .init_resource::<Underwater>()
            .add_systems(Startup, setup_liquid.after(AssetSet::Open))
            .add_systems(Update, (animate_liquid, detect_submersion));
    }
}

/// Whether the camera eye is currently below a water surface. Set by [`detect_submersion`]; read by
/// `lighting::update_time_lighting`, which (when true) samples the **underwater** Light param so the
/// whole scene gets the dense teal fog + cool tint + teal clear colour (VERIFIED apitrace WoW.18 —
/// the murk is fog + light-tint, no overlay quad). Two clocks aside, this is the one cross-feed from
/// the water subsystem into lighting.
#[derive(Resource, Default)]
pub(crate) struct Underwater(pub(crate) bool);

/// Per-water-chunk footprint for submersion detection: the WoW-space XY bounds of the wet area + the
/// surface height. Attached to each [`LiquidSurface`] so [`detect_submersion`] can find the water
/// under the camera; despawns with its tile, so no manual lifecycle.
#[derive(Component)]
pub(crate) struct WaterChunkInfo {
    min_x: f32,
    max_x: f32,
    min_y: f32,
    max_y: f32,
    /// The water surface height (WoW Z) — the highest referenced (wet) vertex of the chunk.
    surface_z: f32,
}

impl WaterChunkInfo {
    pub(crate) fn new(min_x: f32, max_x: f32, min_y: f32, max_y: f32, surface_z: f32) -> Self {
        WaterChunkInfo {
            min_x,
            max_x,
            min_y,
            max_y,
            surface_z,
        }
    }

    /// Is this WoW-space XY inside the chunk's wet footprint?
    pub(crate) fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.min_x && x <= self.max_x && y >= self.min_y && y <= self.max_y
    }

    /// Does this WoW-space XY box overlap the chunk's wet footprint?
    pub(crate) fn overlaps(&self, lo_x: f32, hi_x: f32, lo_y: f32, hi_y: f32) -> bool {
        hi_x >= self.min_x && lo_x <= self.max_x && hi_y >= self.min_y && lo_y <= self.max_y
    }

    pub(crate) fn surface_z(&self) -> f32 {
        self.surface_z
    }
}

/// The chunk's wet liquid-lattice triangles in **raw WoW coords** (positions + indices, verbatim
/// from the [`LiquidMesh`]) — the geometry source for the water-foam decals ([`crate::water_fx`]),
/// which build each decal's static patch from the wet cells overlapping its box (foam clips at
/// banks, exactly like the reference's liquid-face query, decision 0264). Sits beside [`WaterChunkInfo`] on the
/// liquid surface entity; despawns with its tile.
#[derive(Component)]
pub(crate) struct FoamPatch {
    /// Vertex positions `[x, y, z]` in WoW yards (the 9×9 grid; dry verts present, unreferenced).
    pub(crate) positions: Vec<[f32; 3]>,
    /// Triangle indices into `positions` — 6 per wet cell.
    pub(crate) indices: Vec<u32>,
}

/// The water-surface height (WoW Z) over a **WoW-space** position, if it lies inside any loaded
/// water footprint — the shared query under submersion, wading splashes, and enter-water sounds.
/// Ignores Z: answers "is there water at this XY, and how high does it sit".
pub(crate) fn water_surface_at<'a>(
    water: impl Iterator<Item = &'a WaterChunkInfo>,
    wow: [f32; 3],
) -> Option<f32> {
    water
        .filter(|w| {
            wow[0] >= w.min_x && wow[0] <= w.max_x && wow[1] >= w.min_y && wow[1] <= w.max_y
        })
        .map(|w| w.surface_z)
        .next()
}

/// Deepest water (yd) that still counts as **wading** — feet below the surface but not yet swimming.
/// Beyond it the unit swims: footfalls go silent and the wading effects (splash sound, surface
/// ripple) stop. Shared by every wading consumer ([`crate::sound::footsteps`], the enter-water
/// splash, and [`crate::water_fx`]). B7 folded in (decision 0226: the real boundary is
/// `0.75·collisionHeight` off feet-to-surface depth, not a flat constant) and the local player now
/// runs a real WALKING↔SWIMMING mode on it (`player::swim`) — but this 2-yard proxy was never
/// retired: it remains the only signal for units with no swim mode of their own (creatures, which
/// still carry no wire swim flag — the `CreatureModelData.collisionHeight` plumb 0464 leaves
/// open), AND these consumers still read it for the local player too instead of `Player::swimming`
/// — a named follow-up in decision 0530.
pub(crate) const WADE_MAX: f32 = 2.0;

/// Eye-submersion accept margin (VERIFIED `FUN_0069b6d0`: `eye.z < surface + 0.01`).
const SUBMERSION_EPS: f32 = 0.01;

/// Set [`Underwater`] from the camera vs the water surfaces: the eye is submerged if it's inside a
/// water chunk's XY footprint and below its surface (`FUN_0069b6d0`, per-chunk flat approximation of
/// the binary's bilinear 9×9 sample — water is near-flat per chunk). One pass over the loaded water
/// surfaces (a few hundred, cheap).
fn detect_submersion(
    mut underwater: ResMut<Underwater>,
    camera: Query<&Transform, With<WorldCamera>>,
    water: Query<&WaterChunkInfo>,
) {
    let Ok(cam) = camera.single() else {
        return;
    };
    let eye = bevy_to_wow(cam.translation); // [x, y, z] WoW yards
    underwater.0 = water.iter().any(|w| {
        eye[0] >= w.min_x
            && eye[0] <= w.max_x
            && eye[1] >= w.min_y
            && eye[1] <= w.max_y
            && eye[2] < w.surface_z + SUBMERSION_EPS
    });
}

/// The shared liquid materials, one per [`LiquidKind`], plus each one's animated frame count (for
/// the modulo in `animate_liquid`). Read by the terrain streamer (via [`spawn_liquids`]) to material
/// the per-chunk water meshes. Absent when the client has no data (no `WorldAssets`).
#[derive(Resource, Default)]
pub(crate) struct LiquidAssets {
    materials: HashMap<LiquidKind, LiquidEntry>,
}

struct LiquidEntry {
    material: Handle<LiquidMaterial>,
    frame_count: u32,
}

impl LiquidAssets {
    /// The shared material for a liquid kind, if its frames loaded.
    pub(crate) fn material(&self, kind: LiquidKind) -> Option<Handle<LiquidMaterial>> {
        self.materials.get(&kind).map(|e| e.material.clone())
    }

    /// `(kind, material handle)` for each loaded kind — so `lighting::apply_wow_lighting` can push the
    /// per-kind water colours + alpha onto the right shared material each light change.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (LiquidKind, &Handle<LiquidMaterial>)> {
        self.materials.iter().map(|(k, e)| (*k, &e.material))
    }
}

/// Marks a spawned water surface (one per liquid MCNK chunk), so it can be queried/culled as a group.
#[derive(Component)]
pub(crate) struct LiquidSurface;

/// A liquid surface as the **above-water ambient-loop system** hears it (wow-re
/// `liquid-ambience-loop.md`, decision 0506): the wet footprint + the sound-class nibble the
/// driver resolves through `SoundWaterType.dbc`. Attached to **every** liquid surface — water
/// AND the fullbright kinds (the Ironforge lava rumble, Undercity slime), which deliberately
/// carry no [`WaterChunkInfo`]/[`FoamPatch`] (the swim/foam stack is water-only, see
/// [`spawn_wmo_liquids`]) — so it duplicates the footprint numbers rather than entangle the
/// magma/slime kinds with the water-interaction components. Despawns with its tile/placement.
#[derive(Component)]
pub(crate) struct LiquidSoundSource {
    /// The surface's sound-class nibble (`class = n & 3`, `FluidSpeed = n & 0xc`).
    pub(crate) nibble: u8,
    min_x: f32,
    max_x: f32,
    min_y: f32,
    max_y: f32,
    surface_z: f32,
}

impl LiquidSoundSource {
    fn new(nibble: u8, info: &WaterChunkInfo) -> Self {
        LiquidSoundSource {
            nibble,
            min_x: info.min_x,
            max_x: info.max_x,
            min_y: info.min_y,
            max_y: info.max_y,
            surface_z: info.surface_z,
        }
    }

    /// The wet footprint's nearest point to a WoW-space XY, ON the surface plane — the loop
    /// emitter's slew target (the ref positions the channel at the nearest liquid cell; the
    /// AABB clamp is our cell-level approximation, noted in 0506).
    pub(crate) fn nearest_point_wow(&self, x: f32, y: f32) -> [f32; 3] {
        [
            x.clamp(self.min_x, self.max_x),
            y.clamp(self.min_y, self.max_y),
            self.surface_z,
        ]
    }
}

/// The **world-space** submersion footprint of a liquid surface: the WoW-space XY bounds + surface
/// height over the WET (referenced) verts only, with `transform` mapping the mesh's local space into
/// the world. For MCLQ water `lq.positions` are already absolute WoW and `transform` is `IDENTITY` —
/// `bevy_to_wow(wow_to_bevy(p))` is an exact round-trip (a pure axis permutation with sign flips), so
/// the bounds are the raw positions. For WMO liquid the positions are model-local and `transform` is
/// the building's MODF placement, so each wet vertex is carried local-WoW → local-Bevy → world-Bevy →
/// world-WoW before it enters the bounds. `surface_z` is the highest referenced vertex — exact for a
/// horizontal surface (all MCLQ water, and WMO water under a yaw-only building placement, which keeps
/// the plane level), a slight over-approximation only if a placement genuinely tilts the plane (rare;
/// the same flat approximation MCLQ already makes per chunk).
fn wet_footprint(lq: &LiquidMesh, transform: &Transform) -> WaterChunkInfo {
    let mut info = WaterChunkInfo {
        min_x: f32::MAX,
        max_x: f32::MIN,
        min_y: f32::MAX,
        max_y: f32::MIN,
        surface_z: f32::MIN,
    };
    for &i in &lq.indices {
        let p = world_wow(transform, lq.positions[i as usize]);
        info.min_x = info.min_x.min(p[0]);
        info.max_x = info.max_x.max(p[0]);
        info.min_y = info.min_y.min(p[1]);
        info.max_y = info.max_y.max(p[1]);
        info.surface_z = info.surface_z.max(p[2]);
    }
    info
}

/// A liquid vertex's world-space WoW position: **local-WoW → local-Bevy → world-Bevy → world-WoW**.
/// The one place the placement transform is baked into raw liquid coords — shared by the submersion
/// footprint ([`wet_footprint`]) and the wade-foam wet-cell lattice ([`spawn_wmo_liquids`]). For MCLQ
/// water the transform is `IDENTITY`, so this is `bevy_to_wow(wow_to_bevy(p))` = `p` exactly.
fn world_wow(transform: &Transform, local: [f32; 3]) -> [f32; 3] {
    bevy_to_wow(transform.transform_point(wow_to_bevy(local)))
}

/// Spawn a set of water surfaces — one flat mesh per [`LiquidMesh`], on its [`LiquidKind`]'s shared
/// animated material. Used by the `AdtTile` pipeline (`terrain_stream`). No-op when the client has no
/// data (`liquid_assets` absent) or a kind's frames didn't load. Spawned entities are pushed onto
/// `entities` so they despawn with their tile.
pub(crate) fn spawn_liquids<'a>(
    commands: &mut Commands,
    liquids: impl Iterator<Item = &'a LiquidMesh>,
    liquid_assets: Option<&LiquidAssets>,
    meshes: &mut Assets<Mesh>,
    entities: &mut Vec<Entity>,
) {
    let Some(liquid) = liquid_assets else {
        return;
    };
    for lq in liquids {
        let Some(material) = liquid.material(lq.kind) else {
            continue; // this kind's frames failed to load (warned at setup)
        };
        // Submersion footprint over the WET verts (MCLQ positions are already absolute WoW, so the
        // IDENTITY transform is a no-op round-trip). surface_z = the highest referenced vertex.
        let info = wet_footprint(lq, &Transform::IDENTITY);
        let sound = LiquidSoundSource::new(lq.sound_nibble, &info);
        let foam_patch = FoamPatch {
            positions: lq.positions.clone(),
            indices: lq.indices.clone(),
        };
        entities.push(
            commands
                .spawn((
                    Mesh3d(meshes.add(liquid_bevy_mesh(lq))),
                    MeshMaterial3d(material),
                    Transform::IDENTITY,
                    LiquidSurface,
                    info,
                    sound,
                    foam_patch,
                ))
                .id(),
        );
    }
}

/// Build the Bevy render mesh for one [`LiquidMesh`]: positions mapped WoW→Bevy (`lq.positions` are
/// raw WoW coords — absolute for MCLQ, WMO-model-local for WMO liquid), a flat up normal, the tiling
/// UVs, and the per-vertex swatch `V` packed into UV1.x for the shader's colour/opacity ramp. The
/// caller decides the surface's world placement via the spawned entity's `Transform` (`IDENTITY` for
/// absolute MCLQ water; the WMO placement transform for WMO liquid).
fn liquid_bevy_mesh(lq: &LiquidMesh) -> Mesh {
    let positions: Vec<[f32; 3]> = lq
        .positions
        .iter()
        .map(|p| wow_to_bevy(*p).to_array())
        .collect();
    let n = positions.len();
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    // Flat surface: WoW up (0,0,1) → Bevy up (0,1,0). The shader lights against this (rotated into
    // world by the entity transform) + the sun.
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; n]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, lq.uvs.clone());
    // UV1.x carries the per-vertex swatch depth (0..1) for the shader's opacity ramp.
    let uv1: Vec<[f32; 2]> = lq.depths.iter().map(|&d| [d, 0.0]).collect();
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, uv1);
    mesh.insert_indices(Indices::U32(lq.indices.clone()));
    mesh
}

/// Spawn a WMO group's embedded liquid surfaces (Stormwind's canals + fountains, the Ironforge lava,
/// dungeon pools) at the building's placement `transform`, on the shared per-kind liquid material —
/// the same animated water render as MCLQ, but its geometry is WMO-model-local (built by
/// `benilla_formats::wmo_group_liquid_mesh`) so the placement transform lifts it into the world.
///
/// No-op when the client has no data (`liquid_assets` absent) or a kind's frames didn't load. Each
/// WATER surface also carries a world-space [`WaterChunkInfo`] + [`FoamPatch`] (both built by baking the
/// placement transform into the raw liquid coords, [`world_wow`]) so the whole water-interaction stack
/// sees WMO liquid exactly like MCLQ: swimming ([`crate::player::swim`]), the underwater murk
/// ([`detect_submersion`]), the wading splash/footstep sounds, AND the `CWater0Ripple` wade wake /
/// standing ring ([`crate::water_fx`], which builds each foam decal from the wet-cell lattice). The
/// foam's world-axis texgen + per-triangle overlap consume the transformed cells fine, so a rotated
/// canal's ring is still correctly world-oriented. Spawned entities are pushed onto `entities` so they
/// despawn with the placement.
pub(crate) fn spawn_wmo_liquids<'a>(
    commands: &mut Commands,
    liquids: impl Iterator<Item = &'a LiquidMesh>,
    liquid_assets: Option<&LiquidAssets>,
    meshes: &mut Assets<Mesh>,
    transform: Transform,
    entities: &mut Vec<Entity>,
) {
    let Some(liquid) = liquid_assets else {
        return;
    };
    for lq in liquids {
        let Some(material) = liquid.material(lq.kind) else {
            continue; // this kind's frames failed to load (warned at setup)
        };
        let surface = commands
            .spawn((
                Mesh3d(meshes.add(liquid_bevy_mesh(lq))),
                MeshMaterial3d(material),
                transform,
                LiquidSurface,
                // The ambient-loop source rides EVERY kind — the fullbright lava/slime hum too
                // (0506); footprint duplicated so magma never enters the water-only swim/foam set.
                LiquidSoundSource::new(lq.sound_nibble, &wet_footprint(lq, &transform)),
            ))
            .id();
        // Interaction components — WATER kinds only. `WaterChunkInfo`/`FoamPatch` carry no kind, and
        // the shared swim/underwater/foam path is water-coloured, so tagging magma/slime (the fullbright
        // kinds) would let the player "swim" in the Great Forge's lava under a teal *water* murk with
        // white foam. Lava/slime submersion + its damage/fog is its own system (none present in
        // Stormwind's still water). Both components live in world space — the local wet-cell lattice
        // carried through the placement transform, so `water_fx`/`swim`/`detect_submersion` treat a
        // rotated canal like any world-aligned lake.
        if !lq.kind.is_fullbright() {
            let positions: Vec<[f32; 3]> = lq
                .positions
                .iter()
                .map(|&p| world_wow(&transform, p))
                .collect();
            commands.entity(surface).insert((
                wet_footprint(lq, &transform),
                FoamPatch {
                    positions,
                    indices: lq.indices.clone(),
                },
            ));
        }
        entities.push(surface);
    }
}

/// Each kind's animated frame set: `(kind, XTextures subdir, file stem, frame count on disk)`.
/// Frames are `XTextures\<dir>\<stem>.<1..=count>.blp` (256² RGBA, RGB dark + alpha ripple).
const FRAME_SETS: &[(LiquidKind, &str, &str, u32)] = &[
    (LiquidKind::Still, "river", "lake_a", 30),
    (LiquidKind::Rapids, "river", "fast_a", 16),
    (LiquidKind::Ocean, "ocean", "ocean_h", 30),
    // WMO-liquid-only kinds (magma/slime carry no MCLQ data). Opaque + fullbright: the animated
    // texture IS the body colour (VERIFIED wow-re — magma vert-fill = constant 1.0, no depth LUT).
    (LiquidKind::Magma, "lava", "lava", 30),
    (LiquidKind::Slime, "slime", "slime", 30),
];

fn setup_liquid(
    mut commands: Commands,
    config: Option<Res<RenderConfig>>,
    world_assets: Option<ResMut<WorldAssets>>,
    lighting: Option<Res<WowLighting>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<LiquidMaterial>>,
) {
    let (Some(_config), Some(mut world_assets)) = (config, world_assets) else {
        return; // no client data → no terrain, so no water either
    };
    // Seed light/fog + water colours from the current light (or a sane default) so a surface renders
    // correctly on its first frame; `apply_wow_lighting` keeps these in sync afterward (same path as
    // terrain/WDL). Water colour = the per-kind Light.dbc close→far depth gradient (seeded below).
    let light = lighting
        .as_ref()
        .map(|l| l.terrain_uniforms(false))
        .unwrap_or_default();

    let mut assets = LiquidAssets::default();
    for &(kind, dir, stem, count) in FRAME_SETS {
        let Some((frames, frame_count)) =
            load_frame_array(&mut world_assets, &mut images, dir, stem, count)
        else {
            warn!("liquid: no frames for {stem} — {kind:?} water will not render");
            continue;
        };
        // Per-kind water-swatch SEED (frame 0): the Light.dbc water-row shallow→deep endpoints (river/lake
        // = IntBand 16/17, ocean = 14/15, RAW) + shallow alpha (river 0.5 / ocean 0.75; both reach deep =
        // 1.0). Daytime fallback mirroring `Atmosphere::DEFAULT`; `apply_wow_lighting` replaces it with the
        // live per-zone values on frame 1. The shader lerps both colour AND alpha by the same depth V.
        let (shallow, deep, shallow_a) = match kind {
            LiquidKind::Ocean => (
                [0.063, 0.294, 0.349],
                [0.0, 0.114, 0.161],
                OCEAN_SHALLOW_ALPHA,
            ),
            LiquidKind::Still | LiquidKind::Rapids => (
                [0.310, 0.365, 0.078],
                [0.200, 0.322, 0.333],
                RIVER_SHALLOW_ALPHA,
            ),
            // Magma/slime are fullbright (the shader takes the animated texture as the opaque body,
            // ignoring the swatch); these endpoints are unread but kept white/opaque for clarity.
            LiquidKind::Magma | LiquidKind::Slime => ([1.0, 1.0, 1.0], [1.0, 1.0, 1.0], 1.0),
        };
        let material = materials.add(ExtendedMaterial {
            base: StandardMaterial {
                // We do our own (WoW) lighting in the shader; blend + two-sided + depth-write-off
                // (Bevy's transparent pass) is exactly the verified MCLQ water render state.
                unlit: true,
                alpha_mode: AlphaMode::Blend,
                cull_mode: None,
                double_sided: true,
                ..default()
            },
            extension: LiquidExt {
                frames,
                light_ambient: light.light_ambient,
                light_diffuse: light.light_diffuse,
                light_sun: light.light_sun,
                light_spec: Vec4::new(
                    light.light_spec.x,
                    light.light_spec.y,
                    light.light_spec.z,
                    WATER_SHININESS,
                ),
                water_shallow: Vec4::new(shallow[0], shallow[1], shallow[2], shallow_a),
                water_deep: Vec4::new(deep[0], deep[1], deep[2], WATER_DEEP_ALPHA),
                fog_color: light.fog_color,
                fog_params: light.fog_params,
                // x = frame 0 (index driven by `animate_liquid`); y = frame count; z = the fullbright
                // flag (>0.5 ⇒ magma/slime: output the animated texture opaque, skip the swatch/lighting
                // — VERIFIED wow-re magma path); w unused.
                anim: Vec4::new(
                    0.0,
                    frame_count as f32,
                    if kind.is_fullbright() { 1.0 } else { 0.0 },
                    0.0,
                ),
            },
        });
        assets.materials.insert(
            kind,
            LiquidEntry {
                material,
                frame_count,
            },
        );
    }
    info!(
        "liquid: loaded {} water frame set(s)",
        assets.materials.len()
    );
    commands.insert_resource(assets);
}

/// Decode frames `1..=count` for a kind — each with its BLP **authored mip chain** — into one
/// repeating, mipmapped + anisotropic `texture_2d_array` (`assets::liquid_frame_array`; mips are what
/// stop the ripple aliasing into sparkle at distance). Stops at the first missing/non-square/
/// size-mismatched frame (the on-disk sets are contiguous 256² runs). Returns the image handle + the
/// number of frames actually loaded, or `None` if none decoded.
fn load_frame_array(
    world_assets: &mut WorldAssets,
    images: &mut Assets<Image>,
    dir: &str,
    stem: &str,
    count: u32,
) -> Option<(Handle<Image>, u32)> {
    let mut frames: Vec<BlpMipChain> = Vec::new();
    let mut size = 0u32;
    for i in 1..=count {
        let path = format!("XTextures\\{dir}\\{stem}.{i}.blp");
        let Ok(chain) = read_texture_mip_chain(&mut world_assets.chain.lock_recover(), &path)
        else {
            break;
        };
        if chain.width != chain.height {
            break; // water frames are square; bail rather than build a ragged array
        }
        if size == 0 {
            size = chain.width;
        } else if chain.width != size {
            break; // a frame at a different resolution can't share the array
        }
        frames.push(chain);
    }
    if frames.is_empty() {
        return None;
    }
    let loaded = frames.len() as u32;
    Some((images.add(liquid_frame_array(frames)), loaded))
}

/// Advance every liquid material's frame index at [`ANIM_FPS`] off Bevy **real** `Time` (wall-clock,
/// mirroring the reference's `GetTickCount`-driven cycler — NOT the day/night game clock). Writes
/// only on the [`ANIM_FPS`] tick edge: `Assets::get_mut` alone marks the asset Modified and feeds
/// the respecialization pipeline (the mark-changed scan + `Changed<Mesh3d>` sweeps) every frame —
/// the 0353 demand-price law; between ticks the frame index cannot have changed.
fn animate_liquid(
    time: Res<Time>,
    liquid: Option<Res<LiquidAssets>>,
    mut materials: ResMut<Assets<LiquidMaterial>>,
    mut last_ticks: Local<Option<u32>>,
) {
    let Some(liquid) = liquid else {
        return;
    };
    // Captures pin the cycler to frame 0: the wall-clock at screenshot time varies with load
    // times, so any framing with open water diffs differently run to run — the flake substrate's
    // baseline redesign caught (MAE 3.97 → 0.009 pinned; decision 0600). One clause, one frame.
    let ticks = if crate::capture::scenario_active() {
        0
    } else {
        (time.elapsed_secs() * ANIM_FPS) as u32
    };
    if *last_ticks == Some(ticks) {
        return;
    }
    *last_ticks = Some(ticks);
    for entry in liquid.materials.values() {
        if let Some(m) = materials.get_mut(&entry.material) {
            m.extension.anim.x = (ticks % entry.frame_count.max(1)) as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat 10×10 yd wet quad at WoW z = `z`, all four corners referenced (two tris).
    fn flat_quad(z: f32) -> LiquidMesh {
        LiquidMesh {
            positions: vec![
                [0.0, 0.0, z],
                [10.0, 0.0, z],
                [0.0, 10.0, z],
                [10.0, 10.0, z],
            ],
            uvs: vec![[0.0, 0.0]; 4],
            depths: vec![1.0; 4],
            indices: vec![0, 1, 2, 1, 3, 2],
            sound_nibble: 0,
            kind: LiquidKind::Still,
        }
    }

    /// MCLQ water passes `IDENTITY`: `bevy_to_wow(wow_to_bevy(p))` is a pure axis permutation with sign
    /// flips, so the footprint must equal the raw wet-vertex bounds exactly (bit-for-bit — the refactor
    /// that routed MCLQ through `wet_footprint` must not move a single lake edge).
    #[test]
    fn identity_footprint_is_the_raw_bounds() {
        let info = wet_footprint(&flat_quad(5.0), &Transform::IDENTITY);
        assert_eq!((info.min_x, info.max_x), (0.0, 10.0));
        assert_eq!((info.min_y, info.max_y), (0.0, 10.0));
        assert_eq!(info.surface_z, 5.0);
    }

    /// A WMO canal under a yaw-only building placement (spin about vertical + a world lift): the water
    /// plane stays LEVEL, so a single `surface_z` is valid — it must equal the local height plus the
    /// placement's vertical lift, for EVERY yaw. This is the property the whole "one WaterChunkInfo per
    /// WMO surface" swim/submersion fix rests on. (Bevy +Y is up; a WoW z-lift is a Bevy +Y translate.)
    #[test]
    fn yaw_placement_keeps_the_surface_level() {
        let lift = 3.0_f32;
        for deg in [0.0_f32, 30.0, 90.0, 200.0, 355.0] {
            let transform = Transform {
                translation: Vec3::new(100.0, lift, -50.0), // Bevy +Y = WoW +Z lift
                rotation: Quat::from_rotation_y(deg.to_radians()), // yaw about vertical
                scale: Vec3::ONE,
            };
            let info = wet_footprint(&flat_quad(5.0), &transform);
            assert!(
                (info.surface_z - (5.0 + lift)).abs() < 1e-4,
                "yaw {deg}°: surface not level (got {})",
                info.surface_z
            );
            // The lifted, spun quad's world centre still lands inside its own footprint.
            let centre = bevy_to_wow(transform.transform_point(wow_to_bevy([5.0, 5.0, 5.0])));
            assert!(
                info.contains(centre[0], centre[1]),
                "yaw {deg}°: centre {centre:?} outside footprint"
            );
        }
    }
}
