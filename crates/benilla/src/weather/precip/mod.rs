//! Precipitation — the pooled rain/snow simulation, its ground layers, and the mist companion.
//! Spawn kinematics from wow-re's byte-exact transcriptions (`crates/lighting/src/
//! wx_rainspawn.rs`, `wx_snowspawn.rs`, `weather_init.rs`, `wx_leaf_fp.rs`, `weather_scalars.rs`);
//! render laws from the §5 finding `system/lighting/scratch/rf-weather-render.md` (the
//! render-law fold-back record). Implemented idiomatically (f32 math, Bevy space) — the *laws*
//! (constants, rates, gates, blend states) are the reference's, the x87 quirks are not.
//!
//! The verified structure:
//! - **Rain**: drops scatter on the CAMERA-EYE plane across 130×130 yd, then **back-project up
//!   the velocity to the box top** — `T = −37.5/Vz`, spawn = seed − T·V, so every drop starts
//!   **37.5 yd above the eye** and passes its eye-plane scatter point at t = T (`0x674df6–e53`;
//!   rf-weather-emission-timeline Q5 — the old "spawns AT eye height" read killed only the box's
//!   z-RANDOM, not the constant lift, and made density camera-angle-dependent). Each drop dies
//!   at the **terrain** under its spawn column and leaves a 0.25 s *patter* splash **1:1** (calm
//!   wind only). Streaks draw as fixed-size comet-tail TRIANGLES (0.1 wide × 2.0 long) with
//!   **no vertex colour** — the look is `RainDrop01.blp` (authored grey-128-neutral) under
//!   **Mod2x** (`2·src·dst`) and a **forced grey fog** over 70..75 yd that IS the distance fade.
//! - **Snow**: same rails, 90×90 box, spawn plane +30, slow wandering kinematics, alpha-blended
//!   flake quads (half 1/12), fog off.
//! - **Mist**: every precip type carries a companion mist — its own object in the reference
//!   (ctor `0x67a5b0`, spawn `0x67a990`, render `0x67ae20`); the law lives in [`mist`].
//! - Ground heights come from a **lazy height cache** of the reference's weather ground
//!   oracle (`0x67c760 → mgr+0x34 → 0x6b7070`): **WMO/doodad-AWARE** (round 3 Q-B, refuting
//!   the round-2 "terrain-only" read) — after the terrain sample it probes the chunk's static
//!   object refs and MAXes the hit (`CMapObj::IntersectSegment 0x6a37b0`, `0x6b7237–4a`), so
//!   drops **land on roofs**: splashes on the inn roof, never inside, from any camera.
//!   Independently, "no weather indoors" is the global weather-visible flag (`[0xca80c4]`) —
//!   it kills the DRAW alone (`0x677380`/`0x6790b0`/`0x67a520`; simulation always runs — round-6
//!   Q-H(b)); force-set with the camera outdoors (`0x6811d4`), and set from inside whenever an
//!   exterior group survives the view-clipped portal-window pass (`0x6b42d9`) — rain shows
//!   through a doorway the camera can SEE. See [`gate_weather_indoors`].
//! - **Drift heading is WORLD-FIXED**: both spawn kernels centre the per-drop horizontal drift
//!   on the constant azimuth −1.57 rad (`0x80ffbc`, R3/R3a) with a grade-scaled spread. The
//!   **wind** — the local PLAYER's trailing-149-ms average velocity (`0x67c150`, a true yd/s
//!   vector; the camera orbiting does not stir it) — enters ONLY through the spawn-box lead
//!   (×1.75) and the streak APEX tilt (`lerp(0°, 45°, sat(|wind|/30))`, verified DOWNWIND) —
//!   never the drift heading. (R10's spawn-plane tilt keys on the zone-ambient wind, which
//!   benilla doesn't model yet; the patter gate keys on ridden-transport velocity — no gate on
//!   foot.)
//!
//! - **The packet pipeline** (rf-weather-emission-timeline, rounds 2–3): drops are RECORDED
//!   into an open packet, which renders NOTHING until it seals (shader legs draw only the
//!   close-baked buffer) and its `baseTime = open + 6144/rate` stamp passes — a delay line
//!   that hides an upswing's sparse early drizzle (first visible rain stochastic, mostly
//!   ~8.5–14 s, AFTER the fog and mist) and lets committed rain persist through a same-type
//!   clear-down. A wire TYPE change instead cuts: emission stops at once and the unreplayed
//!   pipeline is discarded (`pool::Pool::cut`). See `pool` and [`SHADER_LEG`].
//!
//! Meshes are rebuilt per frame from the live pools (the `particles.rs` pattern) on
//! [`WowParticleMaterial`]; rain's Mod2x blend + forced fog ride the material's `mod2x` key and
//! forced-fog params.

use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::ExtendedMaterial;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::render_resource::Buffer;

use avian3d::prelude::{SpatialQuery, SpatialQueryFilter};

use crate::collision::player_query_filter;
use crate::lighting::{SharedLightBuffer, WowLighting};
use crate::particles::{WowParticleExt, WowParticleMaterial};
use crate::player::WorldCamera;
use crate::wmo_portal::{CameraInteriorClaim, WmoPvsSet};

use super::{WeatherKind, WeatherState, WeatherTick};

mod mist;
mod pool;
mod render;
mod wind;
use mist::{build_mist_mesh, run_mist, Mist, MIST_CAP};
use pool::{run_kind, HeightCache, Pool};
use render::{build_flake_mesh, build_patter_mesh, build_streak_mesh};
pub(crate) use wind::{wow_azimuth_to_bevy, WeatherWind};

// ===== The reference's constants (byte-cited; render laws from wow-re rf-weather-render.md,
// spawn kinematics from the wx_* transcriptions, packet pipeline from
// rf-weather-emission-timeline) =====

/// Which weather **leg** we run — the reference picks per the `useWeatherShaders` CVar
/// (default "1", registered `0x67b81d`) AND a rain.bls/patter.bls ARB validation (`0x58b360`);
/// the verdict lands in `effect+0`. **SHADER — settled in round 3** (0333, retracting 0332's
/// fixed-func inference): the shader leg's ~10 s onset is real — close-only packet visibility
/// (`0x6752b0` bakes at flush alone), the packet-1 forecast orphan, and the RNE parity stick
/// (`0x6754cc`) stack into a stochastic onset (~75% in the 8.5–14 s band, ~25% at ~5 s), and
/// only the shader populations (rain 35000, patters CAP-saturated) reproduce the director's
/// observed density. LIVE-CAPTURE flag: repeated reference upswings should OCCASIONALLY show
/// a ~5 s onset; an always-10 s reference means an unfound parity bias.
const SHADER_LEG: bool = true;
/// Per-packet record capacity — `0x1800` = 6144 (`0x80ffc8` as float; the open stamp divides it
/// by the live rate). NOT a live cap: packets form a growing list (`effect+0x8c`, TSList at
/// `effect+0x70`), so the steady population is demand-bounded (`rate · fall_time`).
const PACKET_CAP: usize = 0x1800;
/// Benilla's live+pending bound per effect (the reference has none — allocator-bounded): the
/// leg's steady demand plus one packet of pipeline lag, with headroom.
const POOL: usize = if SHADER_LEG { 0xC000 } else { 0x4000 };
// Density gain `K` (`rate = K·P·grade` per second) is the live `weatherDensity` setting's table
// entry — `WeatherState::density_gain()` (`0x67b870`: 0.1/0.33/0.66/1.0 for setting 0–3). The
// transcriptions carried quality 2's 0.66 as a baked constant; the reference runs at 3 (K=1.0).
/// Rain population `P` — fixed-func `0x80ffa8` = 6500, shader `0x80ffac` = 35000. (The old
/// `_DAY/_NIGHT` labels were a misread; `effect+0` is the leg flag — see [`SHADER_LEG`].)
const RAIN_P: f32 = if SHADER_LEG { 35000.0 } else { 6500.0 };
/// Snow population `P` — fixed-func `0x80ffdc` = 1300, shader `0x80ffe0` = 14000.
const SNOW_P: f32 = if SHADER_LEG { 14000.0 } else { 1300.0 };
/// A patter lives 0.25 s (`patter_record_init 0x675280`: `lifetime = now + 0.25`).
const PATTER_LIFE: f32 = 0.25;
/// Per-frame spawn budget uses `min(dt, 1/60)` (`wx_leaf_fp.rs::update_dt_accum 0x80ffcc`).
const DT_CAP: f32 = 1.0 / 60.0;
/// The spawn box leads the camera by `wind_motion · 1.75` (the spawn kernels' R11, `0x8680f8`).
const WIND_LEAD: f32 = 1.75;
/// The drift heading CENTRE — the bit-cited constant −1.57 rad (`0x80ffbc`, rain R3 / snow R3a):
/// a **fixed WORLD azimuth** in the WoW frame, shared by rain and snow. The 0311-era code steered
/// the drift by the live wind heading (falling back to 0 when calm) — the director's "rain
/// direction seems different": calm-camera rain leaned toward an arbitrary Bevy axis instead of
/// the reference's world-anchored one.
const DRIFT_AZ_CENTER: f32 = -1.57;
/// Rain fall speed: `vz = −28 − 4w − 2w·r` (`wx_rainspawn.rs` R6: 28.0/4.0/−2.0 bit-cited).
const RAIN_VZ_BASE: f32 = 28.0;
const RAIN_VZ_W: f32 = 4.0;
const RAIN_VZ_RNG: f32 = 2.0;
/// Rain horizontal drift: `((2r−1) + 9.49)·w + 0.01` (R4), heading spread `w·0.209 + 0.052` rad
/// (R3) about the wind heading.
const RAIN_DRIFT_BASE: f32 = 9.49;
const RAIN_DRIFT_EPS: f32 = 0.01;
const RAIN_SPREAD_W: f32 = f32::from_bits(0x3e56_7750); // 0x80ffc4 ≈ 0.20944 (12°)
const RAIN_SPREAD_BIAS: f32 = f32::from_bits(0x3d56_7750); // 0x80ffc0 ≈ 0.05236 (3°)
/// The streak triangle (rf-weather-render Q1): base verts `head ∓ 0.05·RIGHT` (`0x80ff78`),
/// apex `head + tilt·(2.0·antiVel̂)` (`0x80ff74`) — FIXED world-space sizes, not |vel|-scaled.
/// No vertex colour/alpha; the look is the texture under Mod2x + the forced fog.
const STREAK_HALF_W: f32 = 0.05;
const STREAK_TAIL: f32 = 2.0;
/// Rain's forced fog window (render-state 0x0a/0x0b, CPU leg): start 70, end 75 — under Mod2x
/// the grey-0.5 fog colour is NEUTRAL, so this IS the streak distance fade.
const RAIN_FOG_START: f32 = 70.0;
const RAIN_FOG_END: f32 = 75.0;
/// Patter triangle half-edges: `view_right/12` × `view_up/6` (`0x80e004`/`0x803568`, CONFIRMED).
const PATTER_RIGHT: f32 = 1.0 / 12.0;
const PATTER_UP: f32 = 1.0 / 6.0;
/// Spawn-box horizontal half-extents (driver `0x67be40` ctor args): rain 130×130 (`0x43020000`),
/// snow 90×90 (`0x42b40000`).
const RAIN_HALF_XY: f32 = 65.0;
const SNOW_HALF_XY: f32 = 45.0;
/// Spawn-plane lift above the camera eye — HALF the ctor vertical extent (rain 75 → +37.5,
/// snow 60 → +30). Q5-CORRECTED (wow-re rf-weather-emission-timeline): the box's z-RANDOM is
/// dead (`·0.0`) but the constant lift is not — `0x674df6–e53` builds `T = −37.5/Vz` and seeds
/// `local.z = −T·Vz = +37.5`, back-projecting the drop so its trajectory passes the eye-plane
/// scatter point at t = T. The earlier "spawns AT eye height" read (rf-weather-render Q4) was
/// the bug behind camera-angle-dependent density, the precip-free view above the horizon, and
/// uphill terrain getting no rain (director-caught, 2026-07-12).
const RAIN_Z_OFF: f32 = 37.5;
const SNOW_Z_OFF: f32 = 30.0;
/// Snow fall speed: `vz = −2 − 3.5m − m·r` (`wx_snowspawn.rs` R3c: 2.0/3.5 bit-cited) — calm
/// flakes sink at 2 yd/s, a full blizzard at up to 6.5.
const SNOW_VZ_BASE: f32 = 2.0;
const SNOW_VZ_W: f32 = 3.5;
/// Snow horizontal drift: `((r − 0.5) + 5.985)·m + 0.015` (R3b: 5.985/0.015 bit-cited).
const SNOW_DRIFT_OFF: f32 = 5.985;
const SNOW_DRIFT_EPS: f32 = 0.015;
/// Snow heading spread: `2π − 5.934·m` (R3a, `0x80fff4`) — calm snow wanders in ANY direction,
/// a blizzard's flakes align to ±10° of the wind.
const SNOW_SPREAD_W: f32 = f32::from_bits(0x40bd_e44f); // ≈ 5.9341197
/// Snow flake quad half-size — the snow quad pass builds its corner basis ·(1/12) (`0x678960`
/// region 1; the 0.05 previously here was the RAIN streak half-width — the ledger's rain/snow
/// mislabel, corrected by rf-weather-render).
const SNOW_HALF: f32 = 1.0 / 12.0;
/// A settled flake fades over 0.25 s (`snow_patter_value`: `max(t,0) + 0.25`).
const SNOW_SETTLE_LIFE: f32 = 0.25;
/// Snow blend = mode 2 (SrcAlpha, 1−SrcAlpha), FOG OFF (rf-weather-render Q3) — the alpha blend
/// benilla first gave rain belongs to snow. Flake alpha is the texture's own (vertex white).
const SNOW_ALPHA: f32 = 1.0;

/// The precipitation state: per-kind pools + the mist companion + the shared xorshift RNG.
#[derive(Resource)]
pub(super) struct Precip {
    rain: Pool,
    snow: Pool,
    mist: Mist,
    rng: u32,
}

/// Is weather killed by the camera's room? The client's weather-visible flag `[0xca80c4]`
/// inverted: cleared per frame (`0x6b38c1`), force-set with the camera outdoors (`0x6811d4`),
/// and it gates the ENTIRE weather update+render dispatch (`0x677380`/`0x6790b0`/`0x67a520`:
/// load, `je → ret`) — drops, patters, and mist together, with the wire state untouched. The
/// flag's SECOND leg (recorded at `0x6b42d9`, folded 2026-07-13): with the camera in an
/// interior it re-sets if an EXTERIOR-flagged group (`&0x148`) is portal-reachable — rain
/// stays visible (and falling) through a doorway's frame; the reference through the inn's
/// open door shows the storm running (director ref-shot). Only a room with no reachable
/// outside kills the effect — there it FREEZES (drops hang, unrendered) and resumes on
/// stepping out. Exact writer semantics are the Q-H carve (round 6); this is the recorded
/// shape.
#[derive(Resource, Default)]
pub(super) struct WeatherIndoors(bool);

/// Resolve [`WeatherIndoors`] from the camera's PVS-flood interior claim
/// ([`CameraInteriorClaim`], written by the portal pass this frame) and flip the layer
/// entities' visibility: hidden only when the claimed room reaches no exterior.
fn gate_weather_indoors(
    claim: Res<CameraInteriorClaim>,
    mut indoors: ResMut<WeatherIndoors>,
    mut layers: Query<&mut Visibility, With<PrecipLayer>>,
) {
    let now_hidden = claim.0.is_some_and(|c| !c.exterior_visible);
    if now_hidden != indoors.0 {
        indoors.0 = now_hidden;
        for mut vis in &mut layers {
            *vis = if now_hidden {
                Visibility::Hidden
            } else {
                Visibility::Inherited
            };
        }
    }
}

/// xorshift32 → [0,1) — the reference's lagged-table generator is byte-known but its
/// distribution is uniform; the mechanism needs uniformity, not that exact stream.
fn rand01(rng: &mut u32) -> f32 {
    let mut x = *rng;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *rng = x;
    (x >> 8) as f32 / (1u32 << 24) as f32
}

/// Marks the layer entities. The sim writes the camera anchor into their `Transform` each
/// frame and builds vertices anchor-relative — the `particles.rs` convention: the transparent
/// pass sorts (and the GPU transforms) per entity, and a mesh whose entity sits at the world
/// origin ~9000 yd from its absolute-coordinate vertices does not survive the view transform
/// intact (the streak/flake fragments vanished; anchor-relative verts render).
#[derive(Component)]
struct PrecipLayer;

/// Ground-layer capacity — the reference's patter pool is 0x1800 slots (`Packet<Patter,Rain>`),
/// same as the drop pool; at grade 1 the 1:1 landing spawns keep ~rate·0.25 s ≈ 3–6k alive.
const GROUND_CAP: usize = 0x1800;

/// Pad the vertex arrays + indices to a **fixed capacity** with degenerate (zero-area, alpha-0)
/// geometry, so the mesh's GPU buffers keep one size for life (no per-frame reallocation as the
/// pools breathe). The padding indices all reference vertex 0 → zero-area triangles, no
/// fragments.
fn pad_mesh(
    pos: &mut Vec<[f32; 3]>,
    uv: &mut Vec<[f32; 2]>,
    col: &mut Vec<[f32; 4]>,
    idx: &mut Vec<u32>,
    vert_cap: usize,
    idx_cap: usize,
) {
    pos.resize(vert_cap, [0.0; 3]);
    uv.resize(vert_cap, [0.0; 2]);
    col.resize(vert_cap, [0.0; 4]);
    idx.resize(idx_cap, 0);
}

/// A fresh mesh with the particle vertex layout (`particles.rs` convention), pre-filled to its
/// full fixed capacity with degenerate geometry so the buffers hold ONE size for life (no
/// per-frame reallocation, and the render side's first extraction is already final-sized).
fn blank_mesh_sized(vert_cap: usize, idx_cap: usize) -> Mesh {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::default(),
    );
    let (mut pos, mut uv, mut col, mut idx) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    pad_mesh(&mut pos, &mut uv, &mut col, &mut idx, vert_cap, idx_cap);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; pos.len()]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, col);
    mesh.insert_indices(Indices::U32(idx));
    mesh
}

/// Gates a pool's per-frame mesh write on its **liveness edge**. An idle pool re-writing its
/// already all-degenerate fixed-capacity buffer is not free: `Assets::get_mut` alone marks the
/// asset Modified, and the renderer re-extracts and re-uploads the whole capacity-sized buffer
/// every frame — capacity-priced, not demand-priced (at the shader leg's `POOL` = 0xC000 that
/// was ~5 ms/frame on a clear noon — the 0353 fps hunt). Write while live, write ONCE more on
/// the live→empty edge (so the last drops actually clear from the GPU), then leave the asset
/// untouched. Empty content is all-degenerate and camera-independent, so the skip is exact.
#[derive(Default)]
pub(super) struct WriteGate {
    written_live: bool,
}

impl WriteGate {
    /// `Some(mesh)` when this frame's write must happen (live content, or the clearing write).
    pub(super) fn write<'a>(
        &mut self,
        meshes: &'a mut Assets<Mesh>,
        mesh: &Handle<Mesh>,
        live: bool,
    ) -> Option<&'a mut Mesh> {
        let write = live || self.written_live;
        self.written_live = live;
        write.then(|| meshes.get_mut(mesh)).flatten()
    }
}

/// The three verified weather render states (rf-weather-render Q3).
enum WeatherBlend {
    /// Rain streak/patter: **Mod2x** (`2·src·dst`) + the FORCED fog — grey-0.5 over 70..75 yd,
    /// regardless of scene fog. Under Mod2x grey-0.5 is neutral, so the fog IS the distance fade.
    RainMod2x,
    /// Snow: standard alpha blend, fog OFF.
    SnowAlpha,
    /// Mist: standard alpha blend, fog OFF (the puff RGB is the fog colour already).
    MistAlpha,
}

fn weather_material(
    materials: &mut Assets<WowParticleMaterial>,
    texture: Handle<Image>,
    light_buf: Buffer,
    blend: WeatherBlend,
) -> Handle<WowParticleMaterial> {
    // AlphaMode::Blend keeps every layer in the transparent pass with depth-write off (the
    // reference's 0x12 = 0); the Mod2x key swaps the blend equation in specialize().
    let (params, mod2x) = match blend {
        WeatherBlend::RainMod2x => (Vec4::new(0.0, 1.0, RAIN_FOG_START, RAIN_FOG_END), true),
        WeatherBlend::SnowAlpha | WeatherBlend::MistAlpha => (Vec4::ZERO, false),
    };
    materials.add(ExtendedMaterial {
        base: StandardMaterial {
            base_color_texture: Some(texture),
            unlit: true,
            alpha_mode: AlphaMode::Blend,
            cull_mode: None, // the reference's 0x14 = 0, two-sided
            ..default()
        },
        extension: WowParticleExt {
            light_buf,
            params,
            mod2x,
        },
    })
}

fn setup_precip(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<WowParticleMaterial>>,
    light: Option<Res<SharedLightBuffer>>,
    asset_server: Res<AssetServer>,
    cam: Query<&GlobalTransform, With<WorldCamera>>,
    existing: Option<Res<Precip>>,
) {
    // Lazy spawn on the first frame with a live camera (the same lifecycle as every other
    // particle entity — M2 emitters spawn when terrain streams). No asset chain (data-less
    // run) → no shared light buffer → no weather visuals.
    if existing.is_some() || cam.single().is_err() {
        return;
    }
    let Some(light) = light else { return };

    // Constant per-purpose buffer sizes: streaks are 3-vert triangles, flakes 4-vert quads,
    // ground layers their own caps. Fixed-size buffers skip per-frame reallocation and keep the
    // render side's first extraction at final size.
    let rain_drop_mesh = meshes.add(blank_mesh_sized(POOL * 3, POOL * 3));
    let rain_ground_mesh = meshes.add(blank_mesh_sized(GROUND_CAP * 3, GROUND_CAP * 3));
    let snow_drop_mesh = meshes.add(blank_mesh_sized(POOL * 4, POOL * 6));
    let snow_ground_mesh = meshes.add(blank_mesh_sized(GROUND_CAP * 4, GROUND_CAP * 6));
    let mist_mesh = meshes.add(blank_mesh_sized(MIST_CAP * 4, MIST_CAP * 6));
    // Streaks keep the authored mips (a solid bar survives minification); the flake + splash
    // cut-outs load mip-0-only (`BlpVariant::Effect`) — their thin arms collapse to near-zero
    // alpha under the mip chain and vanish at a few pixels (the reference's point-sprite path
    // never minifies them).
    let effect = |s: &mut benilla_assets::BlpLoaderSettings| {
        s.variant = benilla_assets::BlpVariant::Effect;
    };
    let streak_tex: Handle<Image> = asset_server.load("mpq://textures/weather/raindrop01.blp");
    let splash_tex: Handle<Image> =
        asset_server.load_with_settings("mpq://textures/weather/raindropsplash01.blp", effect);
    let flake_tex: Handle<Image> =
        asset_server.load_with_settings("mpq://textures/weather/snowflake01.blp", effect);
    let mist_tex: Handle<Image> =
        asset_server.load_with_settings("mpq://textures/weather/snowmist01.blp", effect);

    // The verified render states (rf-weather-render Q3): rain streak + patter draw Mod2x under
    // the forced grey fog (their textures are authored grey-128-neutral — checked: both
    // backgrounds mean exactly RGB 128); snow and mist are standard alpha blends with fog off.
    for (mesh, tex, blend) in [
        (&rain_drop_mesh, streak_tex, WeatherBlend::RainMod2x),
        (&rain_ground_mesh, splash_tex, WeatherBlend::RainMod2x),
        (&snow_drop_mesh, flake_tex.clone(), WeatherBlend::SnowAlpha),
        (&snow_ground_mesh, flake_tex, WeatherBlend::SnowAlpha),
        (&mist_mesh, mist_tex, WeatherBlend::MistAlpha),
    ] {
        commands.spawn((
            Mesh3d(mesh.clone()),
            MeshMaterial3d(weather_material(
                &mut materials,
                tex,
                light.0.clone(),
                blend,
            )),
            Transform::IDENTITY,
            NoFrustumCulling, // a camera-tracking cloud; its AABB is meaningless
            PrecipLayer,
        ));
    }

    commands.insert_resource(Precip {
        rain: Pool::new(rain_drop_mesh, rain_ground_mesh),
        snow: Pool::new(snow_drop_mesh, snow_ground_mesh),
        mist: Mist::new(mist_mesh),
        rng: 0x9E37_79B9,
    });
}

/// `WOW_WEATHER_PROBE=1`: spawn a STATIC one-off grid of flake quads at fixed offsets ahead of
/// the spawn point — built once, never touched again. Splits "per-frame rebuild breaks it" from
/// "material/texture/distance breaks it" (diagnosis-only; costs nothing when the env is unset).
/// Per frame: wind, spawn budgets, integrate, land, and rebuild the four meshes.
#[allow(clippy::too_many_arguments)]
fn simulate_precip(
    time: Res<Time>,
    weather: Res<WeatherState>,
    lighting: Res<WowLighting>,
    cam: Query<&GlobalTransform, With<WorldCamera>>,
    player: Query<&Transform, (With<crate::net::SelfPlayer>, Without<PrecipLayer>)>,
    spatial: SpatialQuery,
    indoors: Res<WeatherIndoors>,
    mut wind: ResMut<WeatherWind>,
    mut heights: ResMut<HeightCache>,
    mut precip: Option<ResMut<Precip>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut layers: Query<&mut Transform, With<PrecipLayer>>,
    mut last_cut: Local<u32>,
) {
    let Some(precip) = precip.as_deref_mut() else {
        return;
    };
    // Indoors the whole effect freezes — the reference gates the update+render dispatch on
    // `[0xca80c4]` (`je → ret`), so nothing spawns, integrates, ages, or rebuilds; the meshes
    // are already hidden by [`gate_weather_indoors`].
    if indoors.0 {
        return;
    }
    let Ok(cam_tf) = cam.single() else { return };
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    let now = time.elapsed_secs();
    let cam_pos = cam_tf.translation();
    // The wind tracks the local PLAYER's motion (object 0x8fe) — orbiting the camera does not
    // stir it. The camera stands in only when no player entity exists (captures, data-less).
    let wind_pos = player.single().map_or(cam_pos, |t| t.translation);
    wind.update(wind_pos, dt);

    let filter = player_query_filter();
    let Precip {
        rain,
        snow,
        mist,
        rng,
        ..
    } = precip;

    // The Q-D type-change cut: a wire TYPE change (fine included) stops emission at once and
    // discards the not-yet-replaying pipeline; playing cohorts and falling drops finish. The
    // mist's scheduled-but-unborn nodes retire with it (`0x67b234`, round 7).
    if weather.cut_seq != *last_cut {
        *last_cut = weather.cut_seq;
        rain.cut(now);
        snow.cut(now);
        mist.cut();
    }

    run_kind(
        rain,
        WeatherKind::Rain,
        &weather,
        &wind,
        &mut heights,
        &spatial,
        &filter,
        rng,
        now,
        dt,
        cam_pos,
    );
    run_kind(
        snow,
        WeatherKind::Snow,
        &weather,
        &wind,
        &mut heights,
        &spatial,
        &filter,
        rng,
        now,
        dt,
        cam_pos,
    );

    // Throttled 1 Hz state log — the capture/live diagnosis instrument (debug builds only
    // chatter when something is falling or queued in the packet pipeline). Second-resolution
    // deliberately: the pipeline's visible-onset instant is what look-checks time against.
    if (weather.effect_density > 0.0
        || !rain.drops.is_empty()
        || !snow.drops.is_empty()
        || rain.pending_len() > 0
        || snow.pending_len() > 0)
        && time.elapsed_secs().fract() < dt
    {
        debug!(
            "weather pools: rain {} (+{} pipe, +{} ground) snow {} (+{} pipe, +{} ground), density {:.2}",
            rain.drops.len(),
            rain.pending_len(),
            rain.patters.len(),
            snow.drops.len(),
            snow.pending_len(),
            snow.patters.len(),
            weather.effect_density,
        );
    }

    // ===== the mist companion (rf-weather-render Q6) =====
    {
        let Precip { mist, rng, .. } = precip;
        run_mist(
            mist,
            &weather,
            &wind,
            &mut heights,
            &spatial,
            &filter,
            rng,
            dt,
            cam_pos,
        );
    }

    // ===== rebuild the meshes (verts anchor-relative; the entities ride the anchor) =====
    let Precip {
        rain, snow, mist, ..
    } = precip;
    let anchor = cam_pos;
    for mut tf in &mut layers {
        tf.translation = anchor;
    }
    let cam_right = cam_tf.right().as_vec3();
    let cam_up = cam_tf.up().as_vec3();
    let rain_drops_live = !rain.drops.is_empty();
    build_streak_mesh(
        rain.drop_written
            .write(&mut meshes, &rain.drop_mesh, rain_drops_live),
        &rain.drops,
        wind.tilt,
        cam_pos,
        anchor,
    );
    let rain_ground_live = !rain.patters.is_empty();
    build_patter_mesh(
        rain.ground_written
            .write(&mut meshes, &rain.ground_mesh, rain_ground_live),
        &rain.patters,
        cam_right,
        cam_up,
        anchor,
    );
    let snow_drops_live = !snow.drops.is_empty();
    build_flake_mesh(
        snow.drop_written
            .write(&mut meshes, &snow.drop_mesh, snow_drops_live),
        &snow.drops,
        &[],
        cam_right,
        cam_up,
        anchor,
        POOL * 4,
        POOL * 6,
    );
    let snow_ground_live = !snow.patters.is_empty();
    build_flake_mesh(
        snow.ground_written
            .write(&mut meshes, &snow.ground_mesh, snow_ground_live),
        &[],
        &snow.patters,
        cam_right,
        cam_up,
        anchor,
        GROUND_CAP * 4,
        GROUND_CAP * 6,
    );
    let mist_live = mist.is_live();
    build_mist_mesh(
        mist.written.write(&mut meshes, &mist.mesh, mist_live),
        mist,
        cam_right,
        cam_up,
        cam_pos,
        anchor,
        lighting.fog_color,
    );
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<WeatherWind>()
        .init_resource::<HeightCache>()
        .init_resource::<WeatherIndoors>()
        .add_systems(
            Update,
            (
                setup_precip,
                gate_weather_indoors.after(setup_precip).after(WmoPvsSet),
                simulate_precip
                    .after(WeatherTick)
                    .after(gate_weather_indoors),
            ),
        );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The idle skip is edge-exact: untouched from birth, written while live, written ONCE
    /// more on the live→empty edge (the clearing write), then untouched again — so a quiet
    /// scene never re-uploads the fixed-capacity buffers (the 0353 fps hunt).
    #[test]
    fn write_gate_touches_on_live_and_the_clearing_edge_only() {
        let mut meshes = Assets::<Mesh>::default();
        let handle = meshes.add(blank_mesh_sized(4, 6));
        let mut gate = WriteGate::default();
        assert!(gate.write(&mut meshes, &handle, false).is_none());
        assert!(gate.write(&mut meshes, &handle, true).is_some());
        assert!(gate.write(&mut meshes, &handle, true).is_some());
        assert!(gate.write(&mut meshes, &handle, false).is_some());
        assert!(gate.write(&mut meshes, &handle, false).is_none());
    }
}
