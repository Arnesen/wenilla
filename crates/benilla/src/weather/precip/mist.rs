//! The companion mist — every precip type owns one in the reference (its own object: ctor
//! `0x67a5b0`, spawn `0x67a990`, render `0x67ae20`; rain/snow texture `SnowMist01.blp`, sand
//! `WeatherMistGrainy01.blp`). Spawn law: wow-re's **difftested** `mist.rs` transcription;
//! lifetime/motion/fade semantics: the §5 ten-agent finding
//! `system/lighting/scratch/rf-weather-mist-motion.md` (byte-arbitrated) — every constant here
//! is now VERIFIED.
//!
//! The mechanism: up to 128 puffs, spawned at `2·max(density−0.5, 0)·K·Q` nodes/s (zero below
//! the density knee), each born from draws in the **wind-heading frame** — a polar motion
//! basis `dir` (per-type radius, azimuth −1.57±0.175, a gentle rise) and a box scatter
//! (±22 × ±22 × ±12.5, ctor args stored verbatim) — placed `1.5·dir` upstream of the camera
//! with `z = max(ground, seeded z) + 6`. Nodes live **2.7 ± 0.15 s** (the 1024 Hz weather
//! clock), stream at `pos += dt·dir` (≈5 yd/s rain, ≈9 snow) plus a small isotropic
//! `0.5·dt²·tail` rise-assist, and fade over **0.4 s** ramps to **full opacity** — a churning
//! VOLUME of fog-coloured 12×12 quads through the camera's height band. (The *constant near
//! haze* of reference rain is NOT these puffs — they are invisible within 6 yd — it is the
//! weather→scene-fog coupling: the zone's clear↔storm LightParams fog lerped at
//! `bcc = min(1, density·4)`, which `lighting::update_time_lighting` already applies.)

use avian3d::prelude::{SpatialQuery, SpatialQueryFilter};
use bevy::mesh::Indices;
use bevy::prelude::*;

use crate::weather::{WeatherKind, WeatherState};

use super::pool::{HeightCache, CELL};
use super::{pad_mesh, rand01, wow_azimuth_to_bevy, WeatherWind, WriteGate};

/// Mist spawn-rate gain `Q` — per leg (fixed-func `0x80ff9c` = 18, shader `0x80ffa0` = 38;
/// see [`super::SHADER_LEG`]). The rate is `2·max(density − 0.5, 0)·K·Q` nodes/s
/// (`rain_density_param_set` step 5 → the effect+0x3c = mist+0x20 accumulator).
const MIST_Q: f32 = if super::SHADER_LEG { 38.0 } else { 18.0 };
/// Node capacity (ctor arg; up to 128 camera-anchored puffs).
pub(super) const MIST_CAP: usize = 128;
/// Puff quad size: 12×12 world units (ctor arg) → half-extent 6.
const MIST_HALF: f32 = 6.0;
/// Puff floor bias: `z = max(grid_ground, seeded z) + 6.0` (`mist_spawn 0x67a990`: the
/// `size·0.5` term over the fog-grid sample). The **max** is what makes the mist a volume
/// rather than a sheet.
const MIST_FLOOR: f32 = 6.0;
/// Placement box extents — ctor args {44, 44, 25} stored **verbatim** at mist+0x04/08/0c
/// (VERIFIED: plain `mov`, no doubling); the spawn scatters `((m−1)−0.5)·ext` per axis in the
/// wind-heading frame → **±22 × ±22 × ±12.5** around the cluster centre.
const MIST_EXT_XY: f32 = 44.0;
const MIST_EXT_Z: f32 = 25.0;
/// The polar motion basis each node carries (`mist_spawn` draws 1–3, stored at node+0x114):
/// azimuth `−1.57 ± 0.5·0.349` in the wind-heading frame (+0x24/+0x28), a per-type radius
/// (below), rise `0.333 ± 0.5·0.0333` (`0x807a40`/`0x807334`). The node is born `1.5·dir`
/// upstream of the camera (`0x80308c` = 1.5) and streams along `dir`.
const MIST_AZ_BASE: f32 = -1.57;
const MIST_AZ_SCALE: f32 = 0.349;
/// Per-type radius base/span (mist+0x34/+0x38, ctor-verified): rain 5.0/1.2, snow 9.0/3.0.
/// (Sand is 15.0/4.5 and lands with the sand slice.)
const MIST_R_RAIN: (f32, f32) = (5.0, 1.2);
const MIST_R_SNOW: (f32, f32) = (9.0, 3.0);
const MIST_DIR_Z_BASE: f32 = 0.333_333_34;
const MIST_DIR_Z_SCALE: f32 = 0.033_333_335;
const MIST_PLACE_SCALE: f32 = 1.5;
/// Alpha (VERIFIED — there is **no 0.4 cap**): per corner,
/// `round(255 · linearstep(6, 18, corner→cam) · trapezoid)`. The distance term rises with
/// distance (puffs are invisible within 6 yd of the eye — the near haze is the scene fog, not
/// these); a mid-life puff at ≥ 18 yd is **fully opaque** fog colour.
const MIST_ALPHA_NEAR: f32 = 6.0;
const MIST_ALPHA_FAR: f32 = 18.0;
/// Node life (VERIFIED): draw 8 stamps `round((2.7 ± 0.15)·1024)` ticks of the **1024 Hz**
/// weather clock (`0xc62970`) — life uniform in [2.55, 2.85) s.
const MIST_LIFE_BASE: f32 = 2.7;
const MIST_LIFE_SCALE: f32 = 0.3;
/// The fade trapezoid (VERIFIED): `clamp(age/0.4) · clamp((life−age)/0.4)` — mist+0x30 = 0.4
/// is each ramp's DURATION in seconds (not an alpha cap); the plateau sits at 1.0.
const MIST_FADE_S: f32 = 0.4;
/// The node's tail draw (`mist_spawn` draw 7): `((m−1)−0.5)·3.333` ∈ ±1.667 — the isotropic
/// `0.5·dt²·tail` term of the per-frame advance.
const MIST_TAIL_SCALE: f32 = 3.333_333_3;
/// While a node sits below its terrain-follow target, `tail += 5/3` per FRAME (`0x80655c`,
/// `0x67b421`) — a frame-rate-dependent rise-assist in the reference, transcribed as-is.
const MIST_TAIL_RISE: f32 = 5.0 / 3.0;
/// The spawn **path lookahead** (round 7, `0x67a7a0`): up to 64 heights sampled one grid cell
/// (`1.0417 = 0x80ff98`) apart along the node's horizontal heading; count =
/// `min(64, round(planarSpeed · 0.96 / 1.0417))` (≈5 rain, ≈8 snow — a ~1 s lookahead).
const MIST_LOOKAHEAD_SPEED_S: f32 = 0.96;
const MIST_LOOKAHEAD_MAX: usize = 64;
/// The slope-validity deltas over the baked samples (`0x67a8e1–0x67a933`): group `m` is invalid
/// when `s[m+1]−s[m] > 0.5` or `s[m+2]−s[m] > 0.75` or `s[m+3]−s[m] > 1.0` — a WMO wall's
/// roof-height jump trips it, so **puffs die at the wall and never enter the room**: the first
/// invalid group truncates life to `life·(valid+1)/count`; zero valid ⇒ STILLBORN (`0x67a96c`).
const MIST_SLOPE_1: f32 = 0.5;
const MIST_SLOPE_2: f32 = 0.75;
const MIST_SLOPE_3: f32 = 1.0;
/// The follow's upward-only per-frame z-step cap (`0x67b433–5a`; the exact scalar order is
/// flagged in wow-re for a transcription re-check).
const MIST_Z_STEP_CAP: f32 = 3.0;

/// One mist puff: position, its stored motion basis, and the lifecycle.
struct MistNode {
    pos: Vec3,
    /// The wind-yawed polar direction (`mist_spawn` node+0x114): advanced `pos += dt·dir` per
    /// frame (VERIFIED — the drift scalar is the frame dt, ≈|dir| yd/s, framerate-independent).
    dir: Vec3,
    /// node+0x128: the isotropic `0.5·dt²·tail` term; grows 5/3 per frame while below the
    /// ground-follow target (the rise-assist).
    tail: f32,
    age: f32,
    /// Per-node life (2.7 ± 0.15 s on the 1024 Hz clock).
    life: f32,
    /// The spawn-baked height profile along the node's own path — one sample per grid cell
    /// (round 7, `0x67a7a0`; the render never queries the grid live — `0x67ae20` has ZERO grid
    /// reads, refuting an interim render-side hide). The follow target lerps into this array;
    /// the slope-validity walk at spawn already truncated life at the first jump, so a puff
    /// FADES OUT before reaching a wall instead of drifting into the room.
    path: Vec<f32>,
    /// Horizontal speed (yd/s) — the baked-array index advances `speed·age/1.0417`.
    planar_speed: f32,
}

/// The mist companion pool — every precip type carries one (the reference builds it inside
/// each effect ctor); it spawns only above the density 0.5 knee.
pub(super) struct Mist {
    nodes: Vec<MistNode>,
    budget: f32,
    pub(super) mesh: Handle<Mesh>,
    /// Mesh-write liveness edge — see [`WriteGate`]: an idle mist field leaves its GPU buffer
    /// untouched instead of re-uploading it every frame.
    pub(super) written: WriteGate,
}

impl Mist {
    pub(super) fn new(mesh: Handle<Mesh>) -> Self {
        Self {
            nodes: Vec::new(),
            budget: 0.0,
            mesh,
            written: WriteGate::default(),
        }
    }

    /// Whether any puff is alive (the mesh-write gate's liveness input).
    pub(super) fn is_live(&self) -> bool {
        !self.nodes.is_empty()
    }

    /// The stop-flag retire (`0x67b234`, round 7): a wire TYPE change also retires the
    /// SCHEDULED-but-unborn nodes — the spawn budget zeroes with the cut. Live nodes finish
    /// their (possibly truncated) lives normally.
    pub(super) fn cut(&mut self) {
        self.budget = 0.0;
    }
}

/// The slope-validity walk over the baked path (`0x67a8e1–0x67a933`): `Some(m)` = the first
/// invalid group's index (`m` = how many groups were valid before it), `None` = all valid.
fn first_invalid_group(path: &[f32]) -> Option<usize> {
    let n = path.len();
    for m in 0..n {
        let jump = |k: usize, lim: f32| m + k < n && path[m + k] - path[m] > lim;
        if jump(1, MIST_SLOPE_1) || jump(2, MIST_SLOPE_2) || jump(3, MIST_SLOPE_3) {
            return Some(m);
        }
    }
    None
}

/// The mist companion's frame: spawn-rate accumulator `budget += dt·2·max(density−0.5, 0)·K·Q`
/// (the arg is the RAMPED effect density — the same `effect+0xd0` value the drop kinematics
/// read, so the knee sits on the density, not the raw wire grade), one node per unit, cap 128;
/// then every node streams along its motion basis and retires at end-of-life.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_mist(
    mist: &mut Mist,
    weather: &WeatherState,
    wind: &WeatherWind,
    heights: &mut HeightCache,
    spatial: &SpatialQuery,
    filter: &SpatialQueryFilter,
    rng: &mut u32,
    dt: f32,
    cam_pos: Vec3,
) {
    // The ground samples cast from the SAME spawn plane as the active kind's drops — the
    // reference has ONE fog grid per effect, and the grid must see ROOFS (drops die on them,
    // round 3). Casting from the camera's own height put the ray UNDER the ceiling with the
    // camera indoors: every sample saw the interior floor and the mist volume filled the room
    // at floor+6 (director-caught, 2026-07-13 — the forge's ceiling quads). Sharing the plane
    // also stops the height cache thrashing between the drop and mist passes (its clear
    // triggers on a >8 yd plane move).
    let cast_plane = cam_pos.y
        + match weather.effect_kind {
            WeatherKind::Snow => super::SNOW_Z_OFF,
            _ => super::RAIN_Z_OFF,
        };
    // Any active precip type drives its mist; sand's law differs (no 0.5 knee, no ×2) and
    // lands with the sand slice.
    let rate = if weather.effect_kind != WeatherKind::Fine {
        2.0 * (weather.effect_density - 0.5).max(0.0) * weather.density_gain() * MIST_Q
    } else {
        0.0
    };
    let (r_base, r_span) = match weather.effect_kind {
        WeatherKind::Snow => MIST_R_SNOW,
        _ => MIST_R_RAIN,
    };
    mist.budget = (mist.budget + rate * dt).min(8.0);
    let yaw = wind.mist_yaw();
    while mist.budget >= 1.0 && mist.nodes.len() < MIST_CAP {
        mist.budget -= 1.0;
        // The difftested spawn (`mist_spawn 0x67a990`): a polar motion basis + a box scatter,
        // both in the wind-heading frame; the node is born 1.5·|dir| upstream of the camera.
        let az = MIST_AZ_BASE + (rand01(rng) - 0.5) * MIST_AZ_SCALE;
        let radius = r_base + (rand01(rng) - 0.5) * r_span;
        let rise = MIST_DIR_Z_BASE + (rand01(rng) - 0.5) * MIST_DIR_Z_SCALE;
        let dir = yaw * (wow_azimuth_to_bevy(az) * radius + Vec3::Y * rise);
        let place = yaw
            * Vec3::new(
                (rand01(rng) - 0.5) * MIST_EXT_XY,
                (rand01(rng) - 0.5) * MIST_EXT_Z,
                (rand01(rng) - 0.5) * MIST_EXT_XY,
            );
        let mut pos = cam_pos - dir * MIST_PLACE_SCALE + place;
        // `z = max(ground, seeded z) + 6` — a VOLUME from ground+6 up through the camera's
        // height band, not a ground sheet. Over a building "ground" is the ROOF (the grid is
        // a max-from-above), so indoor volumes never fill.
        let ground = heights.ground_y(pos.x, pos.z, cast_plane, spatial, filter);
        pos.y = MIST_FLOOR + pos.y.max(ground);
        // The spawn path lookahead (round 7): bake the height profile one cell at a time along
        // the horizontal heading, walk the slope-validity groups, truncate life at the first
        // jump — a wall's roof step kills the puff before it can enter the room.
        let planar = Vec3::new(dir.x, 0.0, dir.z);
        let planar_speed = planar.length();
        let n = ((planar_speed * MIST_LOOKAHEAD_SPEED_S / CELL).round() as usize)
            .min(MIST_LOOKAHEAD_MAX);
        let step = if n > 0 {
            planar / planar_speed * CELL
        } else {
            Vec3::ZERO
        };
        let mut path = Vec::with_capacity(n.max(1));
        path.push(ground);
        for m in 1..n {
            let at = pos + step * m as f32;
            path.push(heights.ground_y(at.x, at.z, cast_plane, spatial, filter));
        }
        let mut life = MIST_LIFE_BASE + (rand01(rng) - 0.5) * MIST_LIFE_SCALE;
        if let Some(valid) = first_invalid_group(&path) {
            if valid == 0 {
                continue; // stillborn (`0x67a96c`)
            }
            life *= (valid + 1) as f32 / path.len() as f32;
        }
        mist.nodes.push(MistNode {
            pos,
            dir,
            tail: (rand01(rng) - 0.5) * MIST_TAIL_SCALE,
            age: 0.0,
            life,
            path,
            planar_speed,
        });
    }
    for n in &mut mist.nodes {
        n.age += dt;
        // The verified per-frame advance: `pos += dt·dir + 0.5·dt²·tail` (isotropic tail
        // term). The follow target is a LERP into the SPAWN-BAKED path profile (+6) — never a
        // live grid query (round 7, `0x67ae20` reads no grid); while below it the tail grows
        // 5/3 per frame and z steps up toward it, capped per frame.
        n.pos += n.dir * dt + Vec3::splat(0.5 * dt * dt * n.tail);
        let idx = (n.planar_speed * n.age / CELL).max(0.0);
        let (lo, hi) = (idx.floor() as usize, idx.ceil() as usize);
        let last = n.path.len() - 1;
        let (a, b) = (n.path[lo.min(last)], n.path[hi.min(last)]);
        let target = a + (b - a) * idx.fract() + MIST_FLOOR;
        if n.pos.y < target {
            n.tail += MIST_TAIL_RISE;
            n.pos.y = (n.pos.y + MIST_Z_STEP_CAP).min(target);
        }
    }
    mist.nodes.retain(|n| n.age < n.life);
}

/// Mist puffs: 12×12 camera-facing quads, RGB = the CURRENT fog colour (one flat tint), alpha
/// evaluated **per corner** — `linearstep(6, 18, corner→cam) · trapezoid(age)` at full range
/// (the reference computes the distance ramp per billboard corner: `mistr_corner_dist` →
/// `mistr_color_repack`; no peak cap) — so a quad hanging beside the camera still shows its
/// far corners. Alpha-blended, fog off (the tint already IS the fog colour).
pub(super) fn build_mist_mesh(
    mesh: Option<&mut Mesh>,
    mist: &Mist,
    cam_right: Vec3,
    cam_up: Vec3,
    cam_pos: Vec3,
    anchor: Vec3,
    fog_color: [f32; 3],
) {
    let Some(mesh) = mesh else { return };
    let n = mist.nodes.len().min(MIST_CAP);
    let mut pos = Vec::with_capacity(n * 4);
    let mut uv = Vec::with_capacity(n * 4);
    let mut col: Vec<[f32; 4]> = Vec::with_capacity(n * 4);
    let mut idx = Vec::with_capacity(n * 6);
    let r = cam_right * MIST_HALF;
    let u = cam_up * MIST_HALF;
    let cam_rel = cam_pos - anchor;
    for node in mist.nodes.iter().take(MIST_CAP) {
        // trapezoid = clamp(age/0.4) · clamp((life−age)/0.4) — 0.4 s ramps, plateau 1.0. The
        // spawn lookahead already truncated `life` at the first slope jump, so a wall-bound
        // puff rides this SAME ramp to zero before the wall — no separate hide.
        let trapezoid = (node.age / MIST_FADE_S).clamp(0.0, 1.0)
            * ((node.life - node.age) / MIST_FADE_S).clamp(0.0, 1.0);
        let c = node.pos - anchor;
        let corners = [c - r - u, c + r - u, c - r + u, c + r + u];
        let base = pos.len() as u32;
        for corner in corners {
            let dist_a = ((corner.distance(cam_rel) - MIST_ALPHA_NEAR)
                / (MIST_ALPHA_FAR - MIST_ALPHA_NEAR))
                .clamp(0.0, 1.0);
            col.push([fog_color[0], fog_color[1], fog_color[2], dist_a * trapezoid]);
            pos.push(corner.to_array());
        }
        uv.extend([[0.0, 1.0], [1.0, 1.0], [0.0, 0.0], [1.0, 0.0]]);
        idx.extend([base, base + 1, base + 2, base + 1, base + 3, base + 2]);
    }
    pad_mesh(
        &mut pos,
        &mut uv,
        &mut col,
        &mut idx,
        MIST_CAP * 4,
        MIST_CAP * 6,
    );
    let normals = vec![[0.0, 1.0, 0.0]; pos.len()];
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, col);
    mesh.insert_indices(Indices::U32(idx));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The slope-validity walk (`0x67a8e1–0x67a933`): flat terrain passes whole; a roof-height
    /// jump within 3 cells of spawn stillbirths (the k=3 delta trips at m=0); farther jumps
    /// truncate at the first group that sees them.
    #[test]
    fn slope_walk_flags_the_wall() {
        assert_eq!(first_invalid_group(&[10.0, 10.2, 10.4, 10.6, 10.8]), None);
        // A wall's roof at sample 2 (+5 yd): m=0 sees s[2]−s[0] = 5 > 0.75 → stillborn.
        assert_eq!(
            first_invalid_group(&[10.0, 10.1, 15.0, 15.0, 15.0]),
            Some(0)
        );
        // The jump at sample 4 of 5: m=1 first sees it via k=3 (s[4]−s[1] > 1.0).
        assert_eq!(
            first_invalid_group(&[10.0, 10.1, 10.2, 10.3, 15.0]),
            Some(1)
        );
        // Gentle rises inside the deltas pass — the cumulative limits (0.5/0.75/1.0 over
        // 1/2/3 cells) mean sustained slopes must stay under ~0.33 yd per cell.
        assert_eq!(first_invalid_group(&[10.0, 10.3, 10.5, 10.7, 10.9]), None);
    }
}
