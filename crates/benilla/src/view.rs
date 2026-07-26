//! The faithful **view distance** (`farclip`) — one source of truth for everything that keys off how
//! far the detailed world is drawn. Promoted out of the debug panel (it was a `ModelDebug` field doing
//! double duty) so it's *config*, not a debug knob, and so subsystems read one value instead of
//! several uncoordinated ones.
//!
//! Read by: the hard far-clip **wall** (terrain/model/liquid/WDL/particle shaders, pushed as
//! `fog_params.w` by `lighting::apply_wow_lighting`), the per-object **cull**
//! (`debug_panel::apply_model_visibility`), the particle **draw-set gate**
//! (`particles::sim::simulate_particles`) — both through [`within_farclip`] — and
//! — once `terrain_stream` is split — the tile
//! **stream radius** (which should derive its coverage from `farclip`). Live-editable via the debug
//! panel slider (the same A/B lever as before, now writing this resource).

use bevy::prelude::*;

/// View distance in yards. `farclip` = WoW's `farclip` CVar — the projection far plane for the detailed
/// world; geometry beyond it is clipped per-pixel (the wall) and the WDL horizon fills in beyond.
/// Default **777** (the vanilla max-view clamp `[177, 777]`; matches the reference `Config.wtf`).
#[derive(Resource, Clone, Copy)]
pub struct ViewDistance {
    pub farclip: f32,
}

/// The settable range of [`ViewDistance::farclip`], shared by the debug-panel slider and the
/// `$WOW_FARCLIP` env knob so the two can't drift. The vanilla `farclip` CVar clamp is `[177, 777]`
/// (validate callback `0x688d40`, wow-re `terrain.md` "Camera-distance CVars"; its default is 350, ours
/// is the max) — the upper end here runs past that ONLY as an A/B lever against the old
/// "draw everything in the tile window" look.
pub const FARCLIP_RANGE: std::ops::RangeInclusive<f32> = 177.0..=1200.0;

impl Default for ViewDistance {
    /// `$WOW_FARCLIP` (yd, clamped to [`FARCLIP_RANGE`]) overrides the 777 default. The panel slider is
    /// the live lever, but a headless capture has no hands — and a horizon or fog report almost always
    /// arrives with the director's slider somewhere other than the default (the 0684 gap was invisible
    /// at 777 and glaring at 320), so reproducing one must not need a human. Read once at startup, like
    /// the other capture-side knobs.
    fn default() -> Self {
        let farclip = std::env::var("WOW_FARCLIP")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .map_or(777.0, |v| {
                v.clamp(*FARCLIP_RANGE.start(), *FARCLIP_RANGE.end())
            });
        Self { farclip }
    }
}

/// Is a world bounding sphere inside the far-clip wall — i.e. does the detailed world still draw it?
///
/// **The one spelling of "is it nearer than `farclip`", shared by every CPU-side consumer.** The test is
/// planar depth along the camera-forward axis (`(center − eye)·fwd`) of the sphere's NEAREST point, which
/// is deliberately the *same coordinate* the per-pixel wall uses in the shaders (`terrain.wgsl` /
/// `wow_model.wgsl` / `wow_particle.wgsl` all discard on eye-Z past `fog_params.w`). Agreeing on the
/// coordinate is what makes an object straddling the boundary **dissolve** through it instead of popping
/// when its origin crosses.
///
/// Radial distance would be the obvious alternative and it is wrong: it disagrees with the wall off-axis,
/// so a wide object at the edge of the frame pops while its pixels were still being drawn.
///
/// ## Why this is not the camera's far plane
/// The world camera's projection far is ~3000 yd — far *beyond* `farclip` on purpose, so the coarse WDL
/// horizon can draw behind the wall. So the frustum's own far plane is **not** the reference's far plane,
/// and a `Frustum::intersects_sphere(.., intersect_far = true)` is not a substitute for this test. In the
/// reference there is one projection far plane at `farclip` and it bounds the detailed world; here that
/// bound is this function plus the shaders' per-pixel discard, and nothing else.
pub fn within_farclip(
    farclip: f32,
    cam_pos: Vec3,
    cam_fwd: Vec3,
    center: Vec3,
    radius: f32,
) -> bool {
    (center - cam_pos).dot(cam_fwd) - radius <= farclip
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wall is planar depth along camera-forward, measured to the sphere's nearest point.
    #[test]
    fn planar_depth_of_the_nearest_point() {
        let eye = Vec3::ZERO;
        let fwd = Vec3::NEG_Z; // Bevy's camera looks down −Z
        let at = |d: f32, r: f32| within_farclip(777.0, eye, fwd, Vec3::new(0.0, 0.0, -d), r);
        assert!(at(700.0, 0.0));
        assert!(at(777.0, 0.0)); // exactly at the wall still draws (the shader discards past it)
        assert!(!at(778.0, 0.0));
        // A big object straddling the wall stays in: its near side is still inside, and the
        // per-pixel wall dissolves the far half. This is the no-pop property.
        assert!(at(800.0, 30.0));
        assert!(!at(900.0, 30.0));
    }

    /// Off-axis is where radial distance and the shader's eye-Z part company — the wall is a PLANE,
    /// so a point 700 yd forward and 700 yd sideways (radially ~990) is still inside it.
    #[test]
    fn the_wall_is_a_plane_not_a_sphere() {
        let eye = Vec3::ZERO;
        let fwd = Vec3::NEG_Z;
        let off = Vec3::new(700.0, 0.0, -700.0);
        assert!(off.length() > 777.0, "radially outside");
        assert!(
            within_farclip(777.0, eye, fwd, off, 0.0),
            "but inside the planar wall — must match the shader, which discards on eye-Z"
        );
    }

    /// Behind the camera is trivially inside the wall (negative depth); the lateral frustum planes,
    /// not this test, are what reject it. Pinned so nobody "fixes" this into an abs().
    #[test]
    fn behind_the_camera_is_not_this_tests_job() {
        let eye = Vec3::ZERO;
        assert!(within_farclip(
            777.0,
            eye,
            Vec3::NEG_Z,
            Vec3::new(0.0, 0.0, 5000.0),
            0.0
        ));
    }
}
