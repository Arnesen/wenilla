//! Portrait **framing** — how the booth camera is placed for a model.
//!
//! The primary path is the model's **authored portrait camera** (the mechanism — see the module
//! docs in [`super`]): [`frame`] takes the rig verbatim, transform + projection. The rest of this
//! file is the heuristic fallback for camera-less models (head-anchor closeup) and the bind-pose
//! bone walk [`head_anchor`] anchors the fallback with (the posed booth seats riders on real joints).

use bevy::camera::{CameraProjection, PerspectiveProjection, Projection, SubCameraView};
use bevy::math::Vec3A;
use bevy::prelude::*;

/// **Fallback** camera vertical FOV (radians), used only for a camera-less model — a mild ~29° so
/// the heuristic head framing has little perspective distortion. A model with an authored portrait
/// camera brings its own FOV (see [`frame`]).
pub(super) const PORTRAIT_FOV: f32 = 0.5;

/// `1/√(aspect²+1)` at the diagonal-FOV convention's aspect 4/3 = 3/5 exactly — the record-fov →
/// vertical-half-angle factor (the community's "fov × 0.6" legend, emergent not prescaled). The
/// 4/3 enters ONLY this crop factor now: the projection matrix itself runs aspect 1.0 (square-true
/// proportions — the director's ref A/B falsified the on-screen anamorphic squeeze; wow-re §4
/// reconciliation dispatched).
pub(super) const DIAG_TO_VERT: f32 = 0.6;

/// The real client's portrait **projection** (wow-re portrait-render §4, corrected `aa186e79`):
/// gxumath `0x5c3cc0` is a *diagonal-FOV* perspective — half-angle `θ = (fov/2)/√(aspect²+1)`,
/// `m11 = 1/tan θ`, `m00 = m11/aspect` — and the portrait bake feeds it `aspect = 4/3`
/// ([`PORTRAIT_ASPECT`]). Net: vertical half-angle **`0.3·fov`** (the refs' tight crop; 1.72–1.75×
/// tighter than a naive `tan(fov/2)` read) plus a deliberate **3:4 anamorphic squeeze** (spheres
/// render as 4:3-tall ellipses in every real portrait).
///
/// Carried as a custom [`CameraProjection`] rather than a `PerspectiveProjection` because the
/// camera system re-derives `aspect_ratio` from the (square) render target on every projection
/// write — it would stomp the 4/3. `update` is a deliberate no-op: the ref mapping is
/// target-size-independent too (its 64×64 surface is forced by a hardcoded scissor).
#[derive(Debug, Clone)]
pub(super) struct WowPortraitProjection {
    /// The M2 record's fov (radians) — a *diagonal* angle in the client's convention, NOT fovy.
    pub(super) fov: f32,
    pub(super) near: f32,
    pub(super) far: f32,
}

impl WowPortraitProjection {
    /// Full vertical opening angle: `2θ = 0.6·fov`.
    fn fovy(&self) -> f32 {
        DIAG_TO_VERT * self.fov
    }
}

impl CameraProjection for WowPortraitProjection {
    fn get_clip_from_view(&self) -> Mat4 {
        // Aspect 1.0 — square-true proportions. The 4/3 anamorphic squeeze wow-re's §4 verdict
        // prescribed rendered visibly TALLER faces than the reference's on-screen portrait (the
        // director's A/B, 2026-07-06) — the director's capture is ground truth, so the squeeze is
        // out pending wow-re's reconciliation (their first reading WAS aspect ≈ 1; the display
        // path may also compensate). The 0.6 diag→vert factor stays: the crop tightness matched
        // refs and is aspect-derivation-independent on screen. Bevy-native reverse-z infinite
        // depth (the record far — 27.8 — never clips a model-local portrait).
        Mat4::perspective_infinite_reverse_rh(self.fovy(), 1.0, self.near)
    }

    fn get_clip_from_view_for_sub(&self, _sub_view: &SubCameraView) -> Mat4 {
        self.get_clip_from_view() // the booth never renders sub-camera views
    }

    fn update(&mut self, _width: f32, _height: f32) {} // target-size-independent, like the ref

    fn far(&self) -> f32 {
        self.far
    }

    fn get_frustum_corners(&self, z_near: f32, z_far: f32) -> [Vec3A; 8] {
        // Mirrors PerspectiveProjection's corner layout at our fovy + fixed 4/3 aspect.
        let tan_half_fovy = (self.fovy() * 0.5).tan();
        let a = z_near.abs() * tan_half_fovy;
        let b = z_far.abs() * tan_half_fovy;
        [
            Vec3A::new(a, -a, z_near),  // bottom right
            Vec3A::new(a, a, z_near),   // top right
            Vec3A::new(-a, a, z_near),  // top left
            Vec3A::new(-a, -a, z_near), // bottom left
            Vec3A::new(b, -b, z_far),   // bottom right
            Vec3A::new(b, b, z_far),    // top right
            Vec3A::new(-b, b, z_far),   // top left
            Vec3A::new(-b, -b, z_far),  // bottom left
        ]
    }
}

/// The framing the entities cache hands the booth for a display id
/// ([`Creatures::display_anchors`]). All **model-local at scale 1** (the ref bakes with root scale
/// reset, RE `0x47a230`), matching the booth's identity-transform parts.
pub(crate) struct PortraitAnchors {
    /// The model's **authored portrait camera** — the real client's exact framing rig (module
    /// docs). `None` for a camera-less model → the heuristic fields below take over.
    pub(crate) camera: Option<benilla_assets::PortraitCamera>,
    /// The bind-pose **head anchor** (Bevy space): the KeyBone-6 head-bone pivot, else the helm
    /// attach point (id 11) — [`head_anchor`]. The heuristic fallback framing looks here; `None`
    /// (a head-less model — props) falls back to a pivot-derived guess.
    pub(crate) head: Option<Vec3>,
    /// Neck height (the follow-camera pivot) — the head-less fallback + the framing-size hint.
    pub(crate) pivot_height: f32,
    /// Footprint radius — a size hint for the fallback camera standoff.
    pub(crate) ground_radius: f32,
}

/// A bone's **bind-pose global position** in Bevy space: the rest skeleton is pure translations
/// (benilla-assets `build_skeleton` — locals telescope to the bone pivot), so a parent-chain walk
/// reconstructs the pivot exactly. Cycle-guarded by the joint count (`None` = malformed skeleton).
fn bind_bone_global(skeleton: &benilla_assets::ModelSkeleton, bone: u16) -> Option<Vec3> {
    let mut pos = Vec3::ZERO;
    let mut idx = usize::from(bone);
    for _ in 0..=skeleton.joints.len() {
        let j = skeleton.joints.get(idx)?;
        pos += j.local_translation;
        match usize::try_from(j.parent) {
            Ok(p) => idx = p,
            Err(_) => return Some(pos), // -1 = root reached
        }
    }
    None
}

/// A model attachment's **bind-pose position** in Bevy model space (its bone's pivot + the
/// bind-relative offset) — the glue background scenes anchor the create character on their
/// attachment 0 this way (the stage spot sitting on camera 0's axis in every UI_* scene audited).
pub(super) fn attachment_point(
    skeleton: &benilla_assets::ModelSkeleton,
    attachments: &[benilla_assets::ModelAttachment],
    id: u16,
) -> Option<Vec3> {
    let a = attachments.iter().find(|a| a.id == id)?;
    Some(bind_bone_global(skeleton, a.bone)? + a.offset)
}

/// A model's bind-pose head anchor in Bevy space: the head key-bone's pivot (KeyBoneID 6 — the same
/// bone the display-facing twist drives), else the helm attach point (M2 attachment id 11, its bone's
/// pivot + offset). See [`bind_bone_global`] for the walk.
pub(crate) fn head_anchor(
    skeleton: &benilla_assets::ModelSkeleton,
    attachments: &[benilla_assets::ModelAttachment],
) -> Option<Vec3> {
    if let Some(head) = skeleton.head_bone {
        return bind_bone_global(skeleton, head);
    }
    attachments
        .iter()
        .find(|a| a.id == 11)
        .and_then(|a| Some(bind_bone_global(skeleton, a.bone)? + a.offset))
}

/// **Fallback** camera yaw off dead-front (radians) — a slight three-quarter view for the rare
/// camera-less model. Orbits about +Y from the model's front (−Z after the WoW→Bevy map).
const PORTRAIT_YAW: f32 = 0.42;
/// Fallback framed-window height as a fraction of the neck-pivot height (head + a bit of neck).
const WINDOW_OF_PIVOT: f32 = 0.34;
/// Fallback window clamp (model-local yards) — floors tiny models (a whelp still fills the circle)
/// and caps giants (a devilsaur's head still fits).
const WINDOW_MIN: f32 = 0.55;
const WINDOW_MAX: f32 = 1.1;

/// The booth camera rig for a model: transform + projection.
///
/// **Authored path** (the mechanism, module docs): the model's own portrait camera, verbatim —
/// `lookAt(eye, target, up)` with up = +Y rolled about the view axis (roll is `0.0` on every
/// portrait camera audited), and the client's exact projection of the record's fov/near/far
/// ([`WowPortraitProjection`] — diagonal-FOV at the fixed 4/3 aspect). No yaw, no window math —
/// the artist already framed it.
///
/// **Fallback path** (camera-less models): a heuristic face closeup aimed at the head anchor (else
/// just above the neck pivot) from a slight three-quarter angle, sized by the model's own height
/// with a footprint floor so long-bodied quadrupeds don't crop to a nostril. Model-local yards.
pub(super) fn frame(a: &PortraitAnchors) -> (Transform, Projection) {
    if let Some(cam) = a.camera {
        let fwd = (cam.target - cam.eye).normalize_or_zero();
        let up = Quat::from_axis_angle(fwd, cam.roll) * Vec3::Y;
        return (
            Transform::from_translation(cam.eye).looking_at(cam.target, up),
            Projection::custom(WowPortraitProjection {
                fov: cam.fov,
                near: cam.near,
                far: cam.far,
            }),
        );
    }
    // Bounds-less/model-less fallback: a generic human-ish neck height.
    let neck = if a.pivot_height > 0.01 {
        a.pivot_height
    } else {
        1.8
    };
    let target = a.head.unwrap_or(Vec3::new(0.0, 1.05 * neck, 0.0));
    let window = (WINDOW_OF_PIVOT * neck)
        .max(0.9 * a.ground_radius)
        .clamp(WINDOW_MIN, WINDOW_MAX);
    let dist = (window * 0.5) / (PORTRAIT_FOV * 0.5).tan();
    // Orbit the front axis by the three-quarter yaw, eye level with the head.
    let offset = Quat::from_rotation_y(PORTRAIT_YAW) * Vec3::new(0.0, 0.0, -dist);
    (
        Transform::from_translation(target + offset).looking_at(target, Vec3::Y),
        Projection::from(PerspectiveProjection {
            fov: PORTRAIT_FOV,
            near: 0.02,
            far: 100.0,
            ..default()
        }),
    )
}

/// The paper-doll pane's **body framing** feeds [`WowPortraitProjection`] this fov (a *diagonal*
/// angle in that projection's convention — the on-screen vertical opening is `0.6·BODY_FOV`, so the
/// vertical half-angle is `0.3·BODY_FOV ≈ 14.6°`). Mild on purpose: a standing figure should read
/// with little head-to-feet perspective divergence. NOT the portrait path's authored per-model fov —
/// the paper doll frames the *whole* body from bounds (decision 0208 §5), never camera 0.
pub(super) const BODY_FOV: f32 = 0.85;

/// The framed window's **top**, as a multiple of the figure's head/neck height signal. The signals
/// (neck pivot / authored-camera target / head bone) all land around the throat/face — ~0.9 of the
/// standing height — so `> 1.0` clears the crown with air above it.
const BODY_HEADROOM: f32 = 1.24;
/// The framed window's **bottom**, a small fraction of the figure height *below* the feet plane
/// (Y=0), so the boots have ground room and never touch the frame edge.
const BODY_FOOTROOM: f32 = 0.10;
/// Side safety: how much of the footprint half-width to keep framed when width would bind (a wide
/// stance / a held weapon poking sideways). Height binds for a normal humanoid; this only floors it.
const BODY_WIDTH_MARGIN: f32 = 1.15;

/// The **paper-doll** booth camera rig (decision 0208 §5) — a full-body framing derived from the
/// model's own bounds, *not* the authored portrait camera. Yaw is applied to the model root by the
/// caller (the ref's `Model:SetRotation`), so this rig is yaw-independent and the fit is pure /
/// testable.
///
/// **Why not `cameraLookup[0]`.** On a player model the authored portrait camera is the face bust:
/// HumanMale's eye/target both sit at Z ≈ 1.87 (head height) with a ~0.66yd standoff (verified from
/// the M2 bytes — `benilla-formats` `m2_portrait_camera` regression pin; wow-re portrait-render §4 /
/// decision 0113). Aiming through it would crop the body to the face. The real client's paper-doll
/// pane frames a `PlayerModel` through the engine's default bounds-fit camera, not camera 0 — so we
/// do the same.
///
/// **The fit.** The figure's vertical span is `[−footroom, headroom·head_signal]` (model-local Bevy
/// yards, feet at Y=0), where `head_signal` is the tallest of the neck pivot / authored-camera look
/// target / head-bone anchor — all ~throat/face height. The camera sits in front of the model
/// (−Z — WoW +X front maps to Bevy −Z under `wow_to_bevy`) at the distance that fits that span
/// through [`WowPortraitProjection`]'s vertical opening (half-angle `0.3·BODY_FOV`, computed the
/// same way here so the fit is exact), looking at the span's centre. Projection reused from the
/// portrait path for module consistency — square-true, no anamorphic squeeze (decision 0116).
pub(super) fn body_frame(a: &PortraitAnchors) -> (Transform, Projection) {
    // Head/neck height signal (feet at Y=0). The neck pivot (attach-17.z, every character carries
    // it), the authored bust camera's look target, and the head-bone anchor all land near the
    // throat/face — take the tallest, floored off zero for a hypothetical bounds-less display.
    let head_signal = a
        .pivot_height
        .max(a.camera.map_or(0.0, |c| c.target.y))
        .max(a.head.map_or(0.0, |h| h.y))
        .max(0.1);
    let top = BODY_HEADROOM * head_signal;
    let bottom = -BODY_FOOTROOM * head_signal;
    // The square target renders aspect 1.0 (WowPortraitProjection), so the horizontal opening equals
    // the vertical — floor the fitted height by the footprint width so a wide model can't out-reach
    // the sides.
    let window = (top - bottom).max(2.0 * BODY_WIDTH_MARGIN * a.ground_radius);
    let center_y = 0.5 * (top + bottom);

    // Distance that exactly fits `window` through the projection's vertical opening. Half-angle is
    // `0.3·BODY_FOV` — `WowPortraitProjection::fovy()/2` — so the geometry matches the matrix.
    let half_angle = 0.5 * DIAG_TO_VERT * BODY_FOV;
    let dist = (0.5 * window) / half_angle.tan();

    let target = Vec3::new(0.0, center_y, 0.0);
    let eye = target + Vec3::new(0.0, 0.0, -dist); // in front of the model (−Z)
    (
        Transform::from_translation(eye).looking_at(target, Vec3::Y),
        Projection::custom(WowPortraitProjection {
            fov: BODY_FOV,
            near: 0.02,
            far: 100.0,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Perspective-divided clip-space Y of a model-local point through a `body_frame` rig — the value
    /// the rasteriser clips against (inside the frame ⇔ `|ndc_y| < 1`).
    fn ndc_y(transform: &Transform, proj: &WowPortraitProjection, p: Vec3) -> f32 {
        let clip = proj.get_clip_from_view() * transform.to_matrix().inverse() * p.extend(1.0);
        clip.y / clip.w
    }

    /// The bounds→camera fit must keep both **feet and crown** inside the frame with air to spare,
    /// across the player size range (gnome → tauren). Locks the sign (feet below centre, crown above)
    /// and the distance math against a regression (a flipped axis or a wrong half-angle would crop).
    #[test]
    fn body_frame_never_crops_feet_or_crown() {
        for &signal in &[0.88_f32, 1.90, 2.60] {
            let anchors = PortraitAnchors {
                camera: None,
                head: None,
                pivot_height: signal,
                ground_radius: 0.35,
            };
            let (transform, projection) = body_frame(&anchors);
            let proj = WowPortraitProjection {
                fov: BODY_FOV,
                near: 0.02,
                far: 100.0,
            };
            // Feet at the origin; a conservatively-high crown estimate (throat/face signal is ~0.9 of
            // standing height, so the crown sits a little above `signal`).
            let feet = ndc_y(&transform, &proj, Vec3::ZERO);
            let crown = ndc_y(&transform, &proj, Vec3::new(0.0, 1.12 * signal, 0.0));
            assert!(
                feet < 0.0,
                "feet should sit below frame centre, got ndc_y {feet}"
            );
            assert!(
                crown > 0.0,
                "crown should sit above frame centre, got ndc_y {crown}"
            );
            assert!(
                feet.abs() < 0.95,
                "feet cropped / no ground room (ndc_y {feet}) at signal {signal}"
            );
            assert!(
                crown < 0.95,
                "crown cropped / no headroom (ndc_y {crown}) at signal {signal}"
            );
            let _ = projection; // the rig carries the same projection the test rebuilds
        }
    }
}
