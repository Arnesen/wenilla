//! The precip **stream pushers** — streak/patter/flake geometry emitted each frame from the
//! live pools straight into the shared effect stream (0733; world-space — the lane rebases
//! camera-relative render-side). Split from `precip`'s root; geometry only, no sim state.
//! Empty pools push nothing, so an idle sky costs zero here — the structural replacement for
//! the old fixed-capacity meshes' `WriteGate` (the 0353 fps hunt).

use bevy::prelude::*;

use crate::particles::buffer::EffectVertex;

use super::pool::{Drop, Patter};
use super::*;

/// Falling-drop streaks — the verified triangle law (rf-weather-render Q1): per drop, base
/// verts `head ∓ 0.05·RIGHT` (RIGHT = normalize(cross(toCam, antiVel)), camera-facing width
/// axis), apex `head + M·(2.0·antiVel̂)` with M the wind-tilt applied to the APEX ONLY. UVs
/// (0,1)/(1,1)/(0.5,0). No vertex colour/alpha (white; the look is the texture under Mod2x).
pub(super) fn push_streaks(out: &mut Vec<EffectVertex>, drops: &[Drop], tilt: Quat, cam: Vec3) {
    let white = [1.0, 1.0, 1.0, 1.0];
    for d in drops.iter().take(POOL) {
        let anti_vel = -d.vel.normalize_or(Vec3::NEG_Y);
        let to_cam = (cam - d.pos).normalize_or(Vec3::X);
        let right = to_cam.cross(anti_vel).normalize_or(Vec3::X) * STREAK_HALF_W;
        let apex = d.pos + tilt * (anti_vel * STREAK_TAIL);
        for (pos, uv) in [
            (d.pos - right, [0.0, 1.0]),
            (d.pos + right, [1.0, 1.0]),
            (apex, [0.5, 0.0]),
        ] {
            out.push(EffectVertex {
                pos: pos.to_array(),
                uv,
                color: white,
            });
        }
    }
}

/// Ground patters: one camera-facing **triangle** per splash (the byte geometry: corners
/// `center − right`, `center + up`, `center + right` with `right = view_right/12`,
/// `up = view_up/6`), animated left→right across its atlas row over the 0.25 s life.
pub(super) fn push_patters(
    out: &mut Vec<EffectVertex>,
    patters: &[Patter],
    cam_right: Vec3,
    cam_up: Vec3,
) {
    let right = cam_right * PATTER_RIGHT;
    let up = cam_up * PATTER_UP;
    for p in patters.iter().take(GROUND_CAP) {
        let t = (p.age / PATTER_LIFE).clamp(0.0, 1.0);
        let frame = ((t * 4.0) as u32).min(3) as f32;
        let (u0, v0) = (frame * 0.25, f32::from(p.variant) * 0.25);
        // No vertex alpha (Mod2x has none): the atlas's 4 growth frames are the animation, and
        // the texture's grey-128 background is neutral.
        // The byte texcoord law (`wx_rainrender.rs` step 8): base-left, apex, base-right.
        for (pos, uv) in [
            (p.pos - right, [u0, v0 + 0.25]),
            (p.pos + up, [u0 + 0.125, v0 + 0.043]),
            (p.pos + right, [u0 + 0.25, v0 + 0.25]),
        ] {
            out.push(EffectVertex {
                pos: pos.to_array(),
                uv,
                color: [1.0, 1.0, 1.0, 1.0],
            });
        }
    }
}

/// Snow flakes: camera-facing quads (the byte pass builds a face-camera basis; half-size 1/12,
/// jittered per flake), pushed in the stream's perimeter corner order. `drops` = falling
/// flakes; `settled` = landed ones fading out over the `+0.25 s` window.
pub(super) fn push_flakes(
    out: &mut Vec<EffectVertex>,
    drops: &[Drop],
    settled: &[Patter],
    cam_right: Vec3,
    cam_up: Vec3,
) {
    let mut quad = |center: Vec3, half: f32, alpha: f32| {
        let r = cam_right * half;
        let u = cam_up * half;
        // Perimeter order (bl, br, tr, tl) — the stream's quad-index pattern closes it.
        for (pos, uv) in [
            (center - r - u, [0.0, 1.0]),
            (center + r - u, [1.0, 1.0]),
            (center + r + u, [1.0, 0.0]),
            (center - r + u, [0.0, 0.0]),
        ] {
            out.push(EffectVertex {
                pos: pos.to_array(),
                uv,
                color: [1.0, 1.0, 1.0, alpha],
            });
        }
    };
    for d in drops.iter().take(POOL) {
        quad(d.pos, SNOW_HALF * d.size, SNOW_ALPHA);
    }
    for s in settled.iter().take(GROUND_CAP) {
        let t = (s.age / SNOW_SETTLE_LIFE).clamp(0.0, 1.0);
        quad(s.pos, SNOW_HALF, SNOW_ALPHA * (1.0 - t));
    }
}
