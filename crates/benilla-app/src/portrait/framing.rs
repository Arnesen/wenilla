//! Portrait **framing** — how the booth camera is placed for a model.
//!
//! Both booth families frame through the model's **own authored camera**, and neither fits anything
//! (the mechanism — see the module docs in [`super`]):
//!
//! - [`frame`] — a ROUND portrait: `cameraLookup[0]`, verbatim (wow-re portrait-render §4). Its
//!   fallback for a camera-less model is a heuristic head closeup, anchored by the bind-pose bone
//!   walk [`head_anchor`] (which the posed booth also uses to seat riders on real joints).
//! - [`body_frame`] — a `<PlayerModel>` body PANE: raw `cameras[1]`, verbatim, else the client's own
//!   synthesized *fixed* rig (wow-re `modelframe-camera-law.md`, decision 1089). No fit, no
//!   normalization, no bone anchor anywhere on this path.

use bevy::camera::{CameraProjection, PerspectiveProjection, Projection, SubCameraView};
use bevy::math::Vec3A;
use bevy::prelude::*;

/// **Fallback** camera vertical FOV (radians), used only for a camera-less model — a mild ~29° so
/// the heuristic head framing has little perspective distortion. A model with an authored portrait
/// camera brings its own FOV (see [`frame`]).
pub(super) const PORTRAIT_FOV: f32 = 0.5;

/// The aspect the client's portrait bake feeds `0x5c3cc0` — **exactly `1.0`, on every screen**
/// (decision 1543).
///
/// `0x524f60` builds it as `(G44·64/W)/(G48·64/H)` = `(G44/G48)·(H/W)`, and `G44`/`G48`
/// (`[0x832a44]`/`[0x832a48]`) are the *live* normalized screen-aspect direction cosines
/// (`s/√(1+s²)` and `1/√(1+s²)`), so their ratio IS the screen aspect `s` and the `H/W` cancels it
/// exactly. The same pair divides back out of the viewport locals (`0x41ade0`), which is why the
/// bake's viewport is a square `64×64` px box on any target — both observed in the real client's
/// own GL stream at two different screen aspects (1543).
///
/// So the crop is `1/√(1²+1)` and **`fovy = fov/√2 = 0.7071·fov`**.
///
/// **The "fov × 0.6" legend is `1/√((4/3)²+1)`** — what this quantity would be if the aspect were
/// `4/3`, which it never is. Carrying it here after 0163 had already corrected the matrix to
/// isotropic is what left benilla's portraits 18.7% too tight (director report, 2026-08-22).
pub(super) const PORTRAIT_ASPECT: f32 = 1.0;

/// A camera record's *diagonal* fov → the **vertical** opening angle at a given aspect:
/// `fov/√(a²+1)`, the client's own `0x5c3cc0` relation (`t = tan((fov/2)/√(a²+1))`, `m11 = 1/t`, so
/// `tan(fovy/2) = t` exactly). The one place a plain Bevy `PerspectiveProjection` needs it.
///
/// `a` is `0x5c3cc0`'s single `aspect` — the same one that sets the squeeze, never a second number
/// (1543). [`PORTRAIT_ASPECT`] for the round bake; the pane's own rect for a `<PlayerModel>`.
pub(super) fn diag_to_vert(fov: f32, aspect: f32) -> f32 {
    fov / (aspect * aspect + 1.0).sqrt()
}

/// The real client's portrait/model **projection**: gxumath `0x5c3cc0`, a *diagonal-FOV*
/// perspective — half-angle `θ = (fov/2)/√(aspect²+1)`, `m11 = 1/tan θ`, `m00 = m11/aspect`.
///
/// **One `aspect`, two jobs — because `0x5c3cc0` has exactly one.** The same variable sets the
/// horizontal squeeze *and* the diagonal→vertical crop, so a rig that used different numbers for
/// the two builds a matrix the client cannot produce. benilla carried precisely that split for six
/// weeks (0163 corrected the matrix to isotropic and left the crop on `4/3`), and decision 1543
/// closed it: the field below is singular on purpose, and must stay that way.
///
/// - **Round portrait:** `1.0` ([`PORTRAIT_ASPECT`]) — the client's own value, and our sampling
///   region is square by construction, so it is also the number that leaves the UI's stretch
///   nothing to cancel.
/// - **Model pane:** the pane's own width÷height — the client renders straight into the pane rect
///   (`318×224 → 0.576·fov`), and our booth's square target is stretched onto that rect by the UI,
///   so the one number does both jobs there too (decision 1069).
///
/// Carried as a custom [`CameraProjection`] rather than a `PerspectiveProjection` because the
/// camera system re-derives `aspect_ratio` from the (square) render target on every projection
/// write — it would stomp ours. `update` is a deliberate no-op, and that is faithful: the client's
/// bake emits the *same* matrix at 1152×648 and at 1280×800 (1543).
#[derive(Debug, Clone)]
pub(super) struct WowPortraitProjection {
    /// The M2 record's fov (radians) — a *diagonal* angle in the client's convention, NOT fovy.
    pub(super) fov: f32,
    pub(super) near: f32,
    pub(super) far: f32,
    /// `0x5c3cc0`'s one `aspect` — it drives BOTH the horizontal squeeze (`m00 = m11/aspect`) and
    /// the diagonal→vertical crop (`fovy = fov/√(aspect²+1)`). See the type docs for which value
    /// each path takes and why they are the same number.
    ///
    /// It also happens to be the term that cancels the UI's stretch: a booth renders into a
    /// *square* target that the UI stretches to fill whatever rect the sampling region resolves to
    /// (`extract`'s `UvRect::FULL`), so on-screen proportions are only true when the projection
    /// runs at the destination's aspect. Rendering at 1.0 into a 316×351 pane is what made every
    /// dressing-room character 11% too tall (director report, 2026-08-06 — decision 1069). That the
    /// client's own aspect and our destination's agree on both paths is not luck: the round
    /// portrait's region is square *because* the client baked it square.
    pub(super) aspect: f32,
}

impl WowPortraitProjection {
    /// Full vertical opening angle — `2θ` where `θ = (fov/2)/√(aspect²+1)` is the client's own
    /// half-angle (`0x5c3cc0`: `t = tan θ`, `m11 = 1/t`, so `tan(fovy/2) = tan θ` exactly, no
    /// small-angle step anywhere).
    fn fovy(&self) -> f32 {
        diag_to_vert(self.fov, self.aspect)
    }
}

impl CameraProjection for WowPortraitProjection {
    fn get_clip_from_view(&self) -> Mat4 {
        // The client's matrix, with Bevy-native reverse-z infinite depth (the record far — 27.8 on
        // HumanMale — never clips a model-local portrait). The round portrait's `aspect` is 1.0, so
        // there is no anamorphic squeeze: the director's 2026-07-06 A/B said so before the bytes
        // did, and 1543 measured `m00 == m11` in the client's own GL stream.
        Mat4::perspective_infinite_reverse_rh(self.fovy(), self.aspect, self.near)
    }

    fn get_clip_from_view_for_sub(&self, _sub_view: &SubCameraView) -> Mat4 {
        self.get_clip_from_view() // the booth never renders sub-camera views
    }

    fn update(&mut self, _width: f32, _height: f32) {} // target-size-independent, like the ref

    fn far(&self) -> f32 {
        self.far
    }

    fn get_frustum_corners(&self, z_near: f32, z_far: f32) -> [Vec3A; 8] {
        // Mirrors PerspectiveProjection's corner layout at our fovy, widened by `aspect` in x
        // exactly as the matrix is — a corner set that disagreed with the matrix would cull models
        // that do render (and vice versa).
        let tan_half_fovy = (self.fovy() * 0.5).tan();
        let a = z_near.abs() * tan_half_fovy;
        let b = z_far.abs() * tan_half_fovy;
        let (ax, bx) = (a * self.aspect, b * self.aspect);
        [
            Vec3A::new(ax, -a, z_near),  // bottom right
            Vec3A::new(ax, a, z_near),   // top right
            Vec3A::new(-ax, a, z_near),  // top left
            Vec3A::new(-ax, -a, z_near), // bottom left
            Vec3A::new(bx, -b, z_far),   // bottom right
            Vec3A::new(bx, b, z_far),    // top right
            Vec3A::new(-bx, b, z_far),   // top left
            Vec3A::new(-bx, -b, z_far),  // bottom left
        ]
    }
}

/// The framing the entities cache hands the booth for a display id
/// ([`Creatures::display_anchors`]). All **model-local at scale 1** (the ref bakes with root scale
/// reset, RE `0x47a230`), matching the booth's identity-transform parts.
pub(crate) struct PortraitAnchors {
    /// The model's **authored portrait camera** — the real client's exact framing rig for a ROUND
    /// portrait (`cameraLookup[0]`, module docs). `None` for a camera-less model → the heuristic
    /// fields below take over.
    pub(crate) camera: Option<benilla_assets::PortraitCamera>,
    /// The model's **authored pane camera** — raw camera-table index 1, the rig a `<PlayerModel>`
    /// body pane renders through ([`benilla_assets::M2Model::pane_camera`], decision 1089). `None`
    /// for a model with fewer than two cameras → [`body_frame`]'s fixed fallback.
    pub(crate) pane_camera: Option<benilla_assets::PortraitCamera>,
    /// The MD20 header bbox **centre** (Bevy model-local) — the look-at target of the client's fixed
    /// fallback camera, and the ONLY model-derived quantity on the body-pane path when there is no
    /// authored camera to use.
    pub(crate) bbox_center: Vec3,
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
pub(crate) fn attachment_point(
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
/// ([`WowPortraitProjection`] at [`PORTRAIT_ASPECT`] — diagonal-FOV, isotropic, `fovy = fov/√2`).
/// No yaw, no window math — the artist already framed it.
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
                // The client's own bake aspect, which is also ours: its `(G44/G48)·(H/W)` cancels
                // to 1.0 on any screen, and our sampling region is square by construction (the
                // circular mask is inscribed in it), so there is nothing to cancel either. One
                // number, both jobs — `fovy = fov/√2`.
                aspect: PORTRAIT_ASPECT,
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

/// The client's **renormalize-to-4:3** model-root factor — `G48·(5/3)` where
/// `G48 = [0x832a48] = 1/√(a²+1)` (`0x41ad10 SetScale`) and the `5/3` is the `.rdata` literal
/// `[0x80655c]`. Since `f32(5/3) == f32(√((4/3)²+1))` exactly (3·4·5), it is
/// `√((4/3)²+1)/√(a²+1)`: **1.0 at 4:3**, 0.8833 at 16:10, **0.8171 at 16:9**, 1.0412 at 5:4.
///
/// `a` is the **`gxResolution` CVar's width/height**, gated by the `widescreen` CVar (registered at
/// `0x63a74f` with default `"1"` — on by default); with `widescreen = 0` the client uses `4/3` on
/// any monitor. (wow-re `ui/scratch/modelframe-camera-law.md` §11.)
///
/// **It applies to exactly one path, and this is the subtle part.** The widget's model root is
/// `T(pos)·R(facing)·S(s)`, and `0x71439b` composes the *camera's* publish through that same root
/// (`0x718960`) — so on the AUTHORED path `s` **cancels exactly** between camera and geometry, and a
/// client that scaled the geometry without the camera would be wrong by `1/s` (+22% at 16:9). The
/// fixed fallback camera is the exception: `0x505890`'s synth leg writes its eye/target as
/// model-local literals onto a fresh `CCamera` (`0x7ac930`) that is *not* in the model's camera
/// array, so the publish never reaches it. There the geometry shrinks and the camera does not — and
/// because it still looks at the *unscaled* bbox centre, the model also sits low by
/// `(1−s)·centre.y`, which falls out of the transform rather than being coded.
pub(super) fn pane_model_scale(display_aspect: f32) -> f32 {
    const REF: f32 = 4.0 / 3.0;
    ((REF * REF + 1.0) / (display_aspect * display_aspect + 1.0)).sqrt()
}

/// The client's **synthesized fallback camera** for a model frame whose model carries fewer than two
/// cameras — a *fixed* rig, not a fit (wow-re `ui/scratch/modelframe-camera-law.md`, VERIFIED
/// `0x505890`): eye at a constant point in WoW model space, look-at the MD20 header bbox centre, and
/// its own fov/near/far. The only model-derived quantity anywhere in it is the target.
///
/// A small model therefore renders *small* in the pane and a large one overflows it — there is no
/// normalization step. Both of the numbers here are literals in the binary.
/// `(200/36, 0, 87/36)` — the same `/36` family as the clip planes below, which is what the
/// binary's literals decode to (`5.5555558` / `2.4166667` as f32).
const PANE_FIXED_EYE_WOW: [f32; 3] = [200.0 / 36.0, 0.0, 87.0 / 36.0];
/// The fixed camera's field of view, radians — a *diagonal* angle in the client's convention
/// ([`WowPortraitProjection`]). Also the fov the pipeline-warm pass compiles with.
pub(super) const PANE_FIXED_FOV: f32 = 0.5;
/// `1/36` and `5000.0` — the fixed camera's clip planes, verbatim.
const PANE_FIXED_NEAR: f32 = 1.0 / 36.0;
const PANE_FIXED_FAR: f32 = 5000.0;

/// The **body pane** booth camera rig — a 1.12 `<PlayerModel>` widget's own camera, verbatim
/// (decision 1089; wow-re `ui/scratch/modelframe-camera-law.md`).
///
/// **There is no fit.** The widget renders through a frozen snapshot of the model's *authored*
/// camera at **raw table index 1** — the `type == 1` "characterinfo" camera — and when the model has
/// fewer than two cameras it synthesizes the fixed rig above. Nothing on that path reads a bone, an
/// attachment, a bounding sphere or a look-target height for camera purposes; the only model-derived
/// quantity is the fixed camera's bbox-centre target. So the pane is **not** normalized: the panes
/// merely *look* normalized because Blizzard authored per-model standoffs — `GnomeFemale` eye
/// x = 2.16 · `HumanMale` 3.66 · `TaurenMale` 4.43 · `Boar` 4.86.
///
/// **What this replaces, and why it was wrong.** 0208 §5 built a bounds fit off a *head/neck height
/// signal* on the stated premise that the client uses "the engine's default bounds-fit camera". Both
/// halves are refuted: there is no bounds fit, and the head signal is a humanoid assumption. It read
/// plausibly on every biped we had looked at and came out too large and sitting too high the first
/// time a quadruped went in a pane — the director's boar (2026-08-07).
///
/// Yaw is still applied to the model *root* by the caller (the ref's `Model:SetRotation`, which
/// writes the model's facing, not the camera), so this rig stays yaw-independent and pure.
///
/// `aspect` is the destination pane's width ÷ height, and on this path it is **both** of the
/// projection's aspects: the client renders straight into the pane rect, so the same number sets the
/// matrix and the diagonal→vertical crop (`318×224 → 0.576·fov`).
///
/// The **model-root scale** that goes with it is [`pane_root_scale`] — `1.0` whenever the model has
/// its own camera, and the display-aspect renormalize factor when it does not.
pub(super) fn body_frame(a: &PortraitAnchors, aspect: f32) -> (Transform, Projection) {
    let cam = pane_camera(a);
    // `lookAt(eye, target, up)` with up = +Y rolled about the view axis — every vanilla camera
    // track holds a single key of (0,0,0), so roll is 0 in practice and the eye/target ARE the
    // record's base vectors (transcribed for fidelity, not observed nonzero).
    let fwd = (cam.target - cam.eye).normalize_or_zero();
    let up = Quat::from_axis_angle(fwd, cam.roll) * Vec3::Y;
    (
        Transform::from_translation(cam.eye).looking_at(cam.target, up),
        Projection::custom(pane_projection(&cam, aspect)),
    )
}

/// The **model-root scale** a body pane applies alongside the yaw — the other half of
/// [`body_frame`], kept a separate call because the booth re-latches it on a window resize without
/// re-baking. `1.0` on the authored path: not because the client omits the factor there, but because
/// it CANCELS against the camera's own publish through the same root. Only the fixed fallback camera
/// escapes that composition, and there it is real. See [`pane_model_scale`].
pub(super) fn pane_root_scale(a: &PortraitAnchors, display_aspect: f32) -> f32 {
    if a.pane_camera.is_some() {
        1.0
    } else {
        pane_model_scale(display_aspect)
    }
}

/// The camera a body pane renders through: the model's authored `cameras[1]`, else the client's
/// synthesized fixed rig aimed at the bbox centre. See [`body_frame`].
pub(super) fn pane_camera(a: &PortraitAnchors) -> benilla_assets::PortraitCamera {
    a.pane_camera.unwrap_or(benilla_assets::PortraitCamera {
        eye: benilla_assets::coords::wow_to_bevy(PANE_FIXED_EYE_WOW),
        target: a.bbox_center,
        roll: 0.0,
        fov: PANE_FIXED_FOV,
        near: PANE_FIXED_NEAR,
        far: PANE_FIXED_FAR,
    })
}

/// A pane camera's projection: the record's scalars at the pane's aspect — which does both the
/// squeeze and the diagonal→vertical crop, because the client renders straight into the pane rect.
/// See [`WowPortraitProjection::aspect`].
///
/// **1543's cancellation does NOT reach here, and this was checked, not assumed** (wow-re
/// `bcd1f2c2`, a §5 round run blind to decision 1089). Both paths build through the same
/// `0x7ada40`/`0x7ac640`, so the question is only what each caller puts in the aspect rect: the
/// *bake* constructs `{0, 0, 1.0, D}` whose only size terms are its fixed 64/64, which is why its
/// screen terms cancel to `1.0`; the *widget* (`0x76d42d`) hands over the frame's **raw cached
/// layout rect** verbatim (`0x768320`, a 4-dword copy of `[layoutFrame+0x40]`), so the same
/// cancellation removes only the screen terms and the pane's own shape survives —
/// `aspect = (W/H)·(a/a_screen)` = `W/H` whenever the configured and actual display aspects agree,
/// which 1543's `widescreen` census showed is every state the client can reach. The `0x41ade0`
/// unscales on this path (`0x76d45b`/`0x76d46e`) run *after* the camera build and serve the
/// viewport only.
///
/// The falsifier, if anyone is tempted to propagate the portrait's `1.0` here anyway: the viewport
/// comes from that same rect, so projection and viewport aspects agree by construction, and a
/// transplanted `1.0` would render a sphere **1.42× wider than tall** in the 318×224 pet pane.
pub(super) fn pane_projection(
    cam: &benilla_assets::PortraitCamera,
    aspect: f32,
) -> WowPortraitProjection {
    WowPortraitProjection {
        fov: cam.fov,
        near: cam.near,
        far: cam.far,
        aspect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Perspective-divided clip-space XY of a model-local point through a `body_frame` rig — the
    /// values the rasteriser clips against (inside the frame ⇔ both `|ndc| < 1`).
    fn ndc(transform: &Transform, proj: &WowPortraitProjection, p: Vec3) -> Vec2 {
        let clip = proj.get_clip_from_view() * transform.to_matrix().inverse() * p.extend(1.0);
        Vec2::new(clip.x / clip.w, clip.y / clip.w)
    }

    fn ndc_y(transform: &Transform, proj: &WowPortraitProjection, p: Vec3) -> f32 {
        ndc(transform, proj, p).y
    }

    /// The M2 `cameras[1]` records the RE reports, in Bevy space — `wow_to_bevy` is `(-y, z, -x)`.
    fn cam(eye_wow: [f32; 3], target_wow: [f32; 3], fov: f32) -> benilla_assets::PortraitCamera {
        benilla_assets::PortraitCamera {
            eye: benilla_assets::coords::wow_to_bevy(eye_wow),
            target: benilla_assets::coords::wow_to_bevy(target_wow),
            roll: 0.0,
            fov,
            near: 0.222_222,
            far: 27.777_8,
        }
    }

    fn anchors_with(
        pane_camera: Option<benilla_assets::PortraitCamera>,
        bbox_center: Vec3,
    ) -> PortraitAnchors {
        PortraitAnchors {
            // Every heuristic field the RETIRED fit read is left deliberately populated and
            // deliberately unused: if any of them ever reaches the rig again, these tests are the
            // tripwire.
            camera: Some(cam(
                [0.6335, -0.3879, 1.8867],
                [0.0627, 0.0343, 1.8636],
                0.785,
            )),
            pane_camera,
            bbox_center,
            head: Some(Vec3::new(0.0, 1.75, 0.0)),
            pivot_height: 1.90,
            ground_radius: 0.35,
        }
    }

    /// **The authored pane camera is taken verbatim** (decision 1089): eye, look-at and all three
    /// projection scalars are the record's, at every pane aspect. Nothing is fitted, nothing is
    /// derived from the model's size.
    #[test]
    fn a_pane_takes_the_models_own_camera_untouched() {
        // HumanMale's `cameras[1]`, per the RE.
        let human = cam([3.6585, 0.0338, 0.9227], [-0.3644, 0.0291, 0.9873], 0.97991);
        for &aspect in &[1.0_f32, 318.0 / 224.0, 233.0 / 224.0, 316.0 / 351.0] {
            let (transform, projection) =
                body_frame(&anchors_with(Some(human), Vec3::ZERO), aspect);
            assert!(
                transform.translation.distance(human.eye) < 1e-5,
                "eye moved: {:?} vs {:?}",
                transform.translation,
                human.eye
            );
            // Looking AT the record's target: the forward axis points from eye to target.
            let want = (human.target - human.eye).normalize();
            assert!(
                transform.forward().dot(want) > 0.9999,
                "not aimed at the record's target (dot {})",
                transform.forward().dot(want)
            );
            // The projection is the record's scalars with the pane's aspect in both roles — the
            // client renders straight into the pane rect, so the same number sets the matrix AND
            // the diagonal→vertical crop. Compared as the CLIP MATRIX, which is what the
            // rasteriser sees (and what a wrong field would move).
            let want = pane_projection(&human, aspect);
            assert_eq!(
                (want.fov, want.near, want.far),
                (human.fov, human.near, human.far)
            );
            assert_eq!(want.aspect, aspect);
            assert_eq!(
                projection.get_clip_from_view(),
                want.get_clip_from_view(),
                "the rig's projection is not the record's"
            );
        }
    }

    /// **The correction itself: the pane does NOT normalize.** A boar's authored camera stands off
    /// 4.86yd and a gnome's 2.16, so a small model renders small and a large one large — the panes
    /// only *look* normalized because Blizzard calibrated each standoff by hand. The retired fit
    /// solved a distance from a head-height signal instead, which is why a quadruped came out too
    /// big and sitting too high (director, 2026-08-07).
    ///
    /// Measured as apparent height: the on-screen span of one model yard at the subject.
    #[test]
    fn a_pane_is_not_normalized_across_models() {
        let aspect = 318.0 / 224.0; // the pet pane
        let bodies = [
            // (name, cameras[1] eye, target, fov, the model's own standing height)
            ("GnomeFemale", [2.1591_f32, 0.0, 0.6136], 0.6136, 0.85_f32),
            ("HumanMale", [3.6585, 0.0338, 0.9227], 0.9227, 1.85),
            ("TaurenMale", [4.4317, -0.0213, 1.0861], 1.0861, 2.60),
        ];
        let mut apparent = Vec::new();
        for (name, eye, target_z, _height) in bodies {
            let c = cam(eye, [-0.2, 0.0, target_z], 0.9);
            let (transform, _) = body_frame(&anchors_with(Some(c), Vec3::ZERO), aspect);
            let proj = WowPortraitProjection {
                fov: c.fov,
                near: c.near,
                far: c.far,
                aspect,
            };
            // One yard of height at the subject's own standing point.
            let base = Vec3::new(0.0, c.target.y, 0.0);
            let span =
                (ndc_y(&transform, &proj, base + Vec3::Y) - ndc_y(&transform, &proj, base)).abs();
            apparent.push((name, span));
        }
        // A yard subtends LESS of the frame the further out the artist put the camera — which is
        // exactly "no normalization". Under the retired fit every model was solved to the same
        // window, so this ordering was flat.
        for w in apparent.windows(2) {
            assert!(
                w[0].1 > w[1].1,
                "{} should show a yard larger than {} ({} vs {})",
                w[0].0,
                w[1].0,
                w[0].1,
                w[1].1
            );
        }
    }

    /// The **fixed fallback** for a model with fewer than two cameras: the client's own literal eye,
    /// aimed at the MD20 bbox centre, with its own fov/clips — and, the point of the test, *the same
    /// camera whatever the model*. Only the look-at target moves.
    #[test]
    fn a_camera_less_model_gets_the_clients_fixed_rig() {
        let want_eye = Vec3::new(0.0, 87.0 / 36.0, -200.0 / 36.0);
        let mut eyes = Vec::new();
        for centre in [
            Vec3::ZERO,
            Vec3::new(0.0, 0.44, 0.0),
            Vec3::new(0.1, 1.3, 0.0),
        ] {
            let (transform, projection) = body_frame(&anchors_with(None, centre), 318.0 / 224.0);
            assert!(
                transform.translation.distance(want_eye) < 1e-4,
                "fixed eye drifted: {:?}",
                transform.translation
            );
            let want = (centre - want_eye).normalize();
            assert!(
                transform.forward().dot(want) > 0.9999,
                "the fixed camera must look at the bbox centre"
            );
            let fixed = pane_camera(&anchors_with(None, centre));
            assert_eq!(fixed.fov, PANE_FIXED_FOV);
            assert!((fixed.near - 1.0 / 36.0).abs() < 1e-6 && fixed.far == 5000.0);
            assert_eq!(
                projection.get_clip_from_view(),
                pane_projection(&fixed, 318.0 / 224.0).get_clip_from_view()
            );
            eyes.push(transform.translation);
        }
        assert!(
            eyes.windows(2).all(|w| w[0] == w[1]),
            "the fallback camera is FIXED — the model's size must not move it"
        );
    }

    /// The **renormalize-to-4:3** model-root factor, against the values the RE reports —
    /// `√((4/3)²+1)/√(a²+1)`, which is `G48·(5/3)` with `G48 = 1/√(a²+1)`.
    #[test]
    fn the_model_root_renormalizes_to_four_thirds() {
        let close = |a: f32, b: f32| (a - b).abs() < 1e-5;
        assert!(
            close(pane_model_scale(4.0 / 3.0), 1.0),
            "the reference aspect"
        );
        assert!(close(pane_model_scale(16.0 / 10.0), 0.883_332));
        assert!(close(pane_model_scale(16.0 / 9.0), 0.817_102));
        assert!(close(pane_model_scale(5.0 / 4.0), 1.041_158));
    }

    /// **Which path the factor applies to, which is the whole subtlety.** The widget composes the
    /// camera's publish through the SAME model root, so on the authored path `s` cancels exactly and
    /// scaling the geometry alone would be wrong by `1/s` (+22% at 16:9). Only the fixed fallback
    /// camera — written model-local onto a `CCamera` outside the model's array — escapes that.
    #[test]
    fn the_root_factor_applies_only_where_it_does_not_cancel() {
        let human = cam([3.6585, 0.0338, 0.9227], [-0.3644, 0.0291, 0.9873], 0.97991);
        for &a in &[4.0 / 3.0, 16.0 / 9.0, 16.0 / 10.0, 5.0 / 4.0] {
            assert_eq!(
                pane_root_scale(&anchors_with(Some(human), Vec3::ZERO), a),
                1.0,
                "an authored camera cancels the factor at aspect {a}"
            );
            assert_eq!(
                pane_root_scale(&anchors_with(None, Vec3::new(0.0, 0.62, 0.0)), a),
                pane_model_scale(a),
                "the fixed fallback camera does not"
            );
        }
    }

    /// The client's diagonal→vertical crop is **aspect-dependent**: `fovy = fov/√(a²+1)`. The pet
    /// pane's 318×224 crops to 0.576·fov and the character pane's 233×224 to 0.693·fov — and the
    /// round portrait, whose aspect is 1, to `1/√2`. The famous `0.6` is the value at 4/3 and
    /// nothing in the portrait path is ever at 4/3 (1543).
    #[test]
    fn the_diagonal_crop_follows_the_aspect() {
        let close = |a: f32, b: f32| (a - b).abs() < 5e-4;
        assert!(close(diag_to_vert(1.0, 4.0 / 3.0), 0.6), "the 4/3 legend");
        assert!(close(diag_to_vert(1.0, 318.0 / 224.0), 0.575_86));
        assert!(close(diag_to_vert(1.0, 233.0 / 224.0), 0.693_05));
        assert!(close(
            diag_to_vert(1.0, PORTRAIT_ASPECT),
            std::f32::consts::FRAC_1_SQRT_2
        ));
    }

    /// **The portrait bake's projection, pinned against the real client's own GL stream**
    /// (decision 1543). Two apitrace captures of 1.12.1 — `WoW-wade-northshire-20260708.trace` at
    /// 1152×648 (calls 1073945–1075996) and `WoW.trace` at 1280×800 (calls 5592698–5594820) — both
    /// upload the SAME bake matrix for HumanMale's `cameraLookup[0]` (`fov = π/4`, `far = 27.78`):
    ///
    /// ```text
    /// m00 = 3.508226   m11 = 3.508225   (m00 == m11 → isotropic, aspect = 1)
    /// ```
    ///
    /// This is the ground truth that refutes BOTH recorded readings of the bake aspect (4/3, and
    /// `a²`), and the number benilla was missing: `1/3.508226 = tan((fov/2)/√2)`. If a future
    /// "correction" moves the portrait crop off `1/√2`, this test is what should stop it.
    #[test]
    fn the_portrait_bake_matches_the_clients_own_matrix() {
        const OBSERVED_M11: f32 = 3.508_226;
        let proj = WowPortraitProjection {
            fov: std::f32::consts::FRAC_PI_4,
            near: 0.111_57,
            far: 27.778,
            aspect: PORTRAIT_ASPECT,
        };
        let m = proj.get_clip_from_view();
        assert!(
            (m.x_axis.x - OBSERVED_M11).abs() < 1e-4,
            "m00 {} is not the client's {OBSERVED_M11}",
            m.x_axis.x
        );
        assert!(
            (m.y_axis.y - OBSERVED_M11).abs() < 1e-4,
            "m11 {} is not the client's {OBSERVED_M11}",
            m.y_axis.y
        );
        // The old 4/3 crop — what made the portraits 18.7% too tight — must not come back.
        let tight = WowPortraitProjection {
            aspect: 4.0 / 3.0,
            ..proj
        };
        assert!(
            tight.get_clip_from_view().y_axis.y > OBSERVED_M11 * 1.15,
            "the 4/3 crop should be visibly tighter than the client's"
        );
    }

    /// **The 1069 defect, pinned.** A booth bakes into a *square* target that the UI stretches into
    /// the pane's rect, so a figure only keeps its proportions when the projection runs at the
    /// pane's aspect. Measured the way the eye does: screen pixels per model yard, sideways vs
    /// upward, at the framed figure's own centre. Before 1069 the dressing room's 316×351 pane read
    /// 1.111 — an 11% vertical stretch, which is exactly what the director saw.
    #[test]
    fn a_pane_shows_the_figure_unstretched_at_any_aspect() {
        for &(w, h) in &[(316.0_f32, 351.0_f32), (233.0, 224.0), (512.0, 512.0)] {
            let aspect = w / h;
            let c = cam([3.6585, 0.0338, 0.9227], [-0.3644, 0.0291, 0.9873], 0.97991);
            let (transform, _) = body_frame(&anchors_with(Some(c), Vec3::ZERO), aspect);
            let proj = WowPortraitProjection {
                fov: c.fov,
                near: c.near,
                far: c.far,
                aspect,
            };
            // A small cross at the framing centre — the on-screen size of each arm, in pane pixels.
            let mid = Vec3::new(0.0, transform.translation.y, 0.0);
            let step = 0.05_f32;
            let o = ndc(&transform, &proj, mid);
            let px_x = (ndc(&transform, &proj, mid + Vec3::X * step).x - o.x).abs() * 0.5 * w;
            let px_y = (ndc(&transform, &proj, mid + Vec3::Y * step).y - o.y).abs() * 0.5 * h;
            let stretch = px_y / px_x;
            assert!(
                (stretch - 1.0).abs() < 0.005,
                "a {w}×{h} pane stretches the figure by {stretch} (1.0 = round)"
            );
        }
    }
}
