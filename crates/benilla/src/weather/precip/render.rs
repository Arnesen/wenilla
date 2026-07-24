//! The precip **mesh builders** — streak/patter/flake quads rebuilt each frame from the live
//! pools. Split from `precip`'s root; geometry only, no sim state.

use bevy::prelude::*;

use super::pool::{Drop, Patter};
use super::*;

/// Falling-drop streaks — the verified triangle law (rf-weather-render Q1): per drop, base
/// verts `head ∓ 0.05·RIGHT` (RIGHT = normalize(cross(toCam, antiVel)), camera-facing width
/// axis), apex `head + M·(2.0·antiVel̂)` with M the wind-tilt applied to the APEX ONLY. UVs
/// (0,1)/(1,1)/(0.5,0). No vertex colour/alpha (white; the look is the texture under Mod2x).
pub(super) fn build_streak_mesh(
    mesh: Option<&mut Mesh>,
    drops: &[Drop],
    tilt: Quat,
    cam: Vec3,
    anchor: Vec3,
) {
    let Some(mesh) = mesh else { return };
    let n = drops.len().min(POOL);
    let mut pos = Vec::with_capacity(n * 3);
    let mut uv = Vec::with_capacity(n * 3);
    let mut col = Vec::with_capacity(n * 3);
    let mut idx = Vec::with_capacity(n * 3);
    let white = [1.0, 1.0, 1.0, 1.0];
    for d in drops.iter().take(POOL) {
        let anti_vel = -d.vel.normalize_or(Vec3::NEG_Y);
        let to_cam = (cam - d.pos).normalize_or(Vec3::X);
        let right = to_cam.cross(anti_vel).normalize_or(Vec3::X) * STREAK_HALF_W;
        let head = d.pos - anchor;
        let apex = head + tilt * (anti_vel * STREAK_TAIL);
        let base = pos.len() as u32;
        pos.extend([
            (head - right).to_array(),
            (head + right).to_array(),
            apex.to_array(),
        ]);
        uv.extend([[0.0, 1.0], [1.0, 1.0], [0.5, 0.0]]);
        col.extend([white; 3]);
        idx.extend([base, base + 1, base + 2]);
    }
    pad_mesh(&mut pos, &mut uv, &mut col, &mut idx, POOL * 3, POOL * 3);
    let normals = vec![[0.0, 1.0, 0.0]; pos.len()];
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, col);
    mesh.insert_indices(Indices::U32(idx));
}

/// Ground patters: one camera-facing **triangle** per splash (the byte geometry: corners
/// `center − right`, `center + up`, `center + right` with `right = view_right/12`,
/// `up = view_up/6`), animated left→right across its atlas row over the 0.25 s life.
pub(super) fn build_patter_mesh(
    mesh: Option<&mut Mesh>,
    patters: &[Patter],
    cam_right: Vec3,
    cam_up: Vec3,
    anchor: Vec3,
) {
    let Some(mesh) = mesh else { return };
    let right = cam_right * PATTER_RIGHT;
    let up = cam_up * PATTER_UP;
    let n = patters.len();
    let mut pos = Vec::with_capacity(n * 3);
    let mut uv = Vec::with_capacity(n * 3);
    let mut col = Vec::with_capacity(n * 3);
    let mut idx = Vec::with_capacity(n * 3);
    for p in patters.iter().take(GROUND_CAP) {
        let t = (p.age / PATTER_LIFE).clamp(0.0, 1.0);
        let frame = ((t * 4.0) as u32).min(3) as f32;
        let (u0, v0) = (frame * 0.25, f32::from(p.variant) * 0.25);
        // No vertex alpha (Mod2x has none): the atlas's 4 growth frames are the animation, and
        // the texture's grey-128 background is neutral.
        let alpha = 1.0;
        let base = pos.len() as u32;
        let c = p.pos - anchor;
        pos.extend([
            (c - right).to_array(),
            (c + up).to_array(),
            (c + right).to_array(),
        ]);
        // The byte texcoord law (`wx_rainrender.rs` step 8): base-left, apex, base-right.
        uv.extend([
            [u0, v0 + 0.25],
            [u0 + 0.125, v0 + 0.043],
            [u0 + 0.25, v0 + 0.25],
        ]);
        col.extend([[1.0, 1.0, 1.0, alpha]; 3]);
        idx.extend([base, base + 1, base + 2]);
    }
    pad_mesh(
        &mut pos,
        &mut uv,
        &mut col,
        &mut idx,
        GROUND_CAP * 3,
        GROUND_CAP * 3,
    );
    let normals = vec![[0.0, 1.0, 0.0]; pos.len()];
    // (capacity matches creation)
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, col);
    mesh.insert_indices(Indices::U32(idx));
}

/// Snow flakes: camera-facing quads (the byte pass builds a face-camera basis; half-size 0.05,
/// jittered per flake). `drops` = falling flakes; `settled` = landed ones fading out over the
/// `+0.25 s` window.
#[allow(clippy::too_many_arguments)] // one mesh-build's full input set
pub(super) fn build_flake_mesh(
    mesh: Option<&mut Mesh>,
    drops: &[Drop],
    settled: &[Patter],
    cam_right: Vec3,
    cam_up: Vec3,
    anchor: Vec3,
    vert_cap: usize,
    idx_cap: usize,
) {
    let Some(mesh) = mesh else { return };
    let n = drops.len() + settled.len();
    let mut pos = Vec::with_capacity(n * 4);
    let mut uv = Vec::with_capacity(n * 4);
    let mut col = Vec::with_capacity(n * 4);
    let mut idx = Vec::with_capacity(n * 6);
    let mut quad = |center: Vec3, half: f32, alpha: f32| {
        let r = cam_right * half;
        let u = cam_up * half;
        let base = pos.len() as u32;
        let c = center - anchor;
        pos.extend([
            (c - r - u).to_array(),
            (c + r - u).to_array(),
            (c - r + u).to_array(),
            (c + r + u).to_array(),
        ]);
        uv.extend([[0.0, 1.0], [1.0, 1.0], [0.0, 0.0], [1.0, 0.0]]);
        col.extend([[1.0, 1.0, 1.0, alpha]; 4]);
        idx.extend([base, base + 1, base + 2, base + 1, base + 3, base + 2]);
    };
    for d in drops.iter().take(POOL) {
        quad(d.pos, SNOW_HALF * d.size, SNOW_ALPHA);
    }
    for s in settled.iter().take(GROUND_CAP) {
        let t = (s.age / SNOW_SETTLE_LIFE).clamp(0.0, 1.0);
        quad(s.pos, SNOW_HALF, SNOW_ALPHA * (1.0 - t));
    }
    pad_mesh(&mut pos, &mut uv, &mut col, &mut idx, vert_cap, idx_cap);
    let normals = vec![[0.0, 1.0, 0.0]; pos.len()];
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uv);
    mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, col);
    mesh.insert_indices(Indices::U32(idx));
}
