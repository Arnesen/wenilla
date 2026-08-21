//! **The view** — who is looking, with what optics, and how far the detailed world is drawn.
//!
//! Three things, one owner. [`WorldCamera`] marks *the* camera the scene is rendered through;
//! [`CAM_NEAR`] and [`CAM_FOVY`] are its optics; [`ViewDistance`] is the faithful `farclip`.
//!
//! The marker and the optics were in `player::camera` until decision 1160's stage zero, and that
//! was the single largest edge across the engine/game line: **26 engine files** — terrain
//! streaming, the portal PVS, sun follow, picking, every effect sim — reached into the *player
//! controller* to ask which camera to read. None of them care about a player; they care about the
//! viewer, which a world editor and a serverless viewer have without one. `farclip` itself was
//! promoted out of the debug panel earlier, for the same reason at a smaller scale: it was a
//! `ModelDebug` field doing double duty, so it read as a debug knob rather than as config, and
//! subsystems each kept their own idea of it.
//!
//! Read by: the hard far-clip **wall** (terrain/model/liquid/WDL/particle shaders, pushed as
//! `fog_params.w` by `lighting::apply_wow_lighting`), the per-object **cull**
//! (`debug_panel::apply_model_visibility`), the particle **draw-set gate**
//! (`particles::sim::simulate_particles`) — both through [`within_farclip`] — and
//! — once `terrain_stream` is split — the tile
//! **stream radius** (which should derive its coverage from `farclip`). Live-editable via the debug
//! panel slider (the same A/B lever as before, now writing this resource).

use bevy::prelude::*;

/// **The viewer's own body**, as the world needs it — 1160's wire (a), the half that is not about
/// where to stream.
///
/// Three engine lanes read the game's avatar for the same three facts and behind the *same*
/// predicate (`active && !detached`): the WMO interior probe wants the eye's world point, the
/// water foam wants a wading body, the precipitation slab wants the commanded planar speed its
/// tilt keys on. None of them wants a `Player` — they want a body that may or may not be there.
///
/// `None` on [`Self::at`] means exactly what each of those sites used to spell out by hand: no
/// live avatar, or an eye that has been detached from it. A program with no avatar at all leaves
/// this defaulted and every lane takes its no-body branch, which is what the world viewer needs.
#[derive(Resource, Clone, Copy)]
pub struct Viewer {
    /// The avatar's position in **Bevy** space, when one is live and the eye is on it.
    pub at: Option<Vec3>,
    /// Its last-streamed CMovement flags (`MOVEMENTFLAGS`, cached at `unit+0x9e8`). `0` with no
    /// body. Read through [`Self::translating`] / [`Self::turning`] rather than masked at each
    /// site — the bit values are the wire's, and one restatement of them is one too many already.
    pub move_flags: u32,
    /// The **commanded** planar speed in yd/s (`[[player+0x118]+0x84]`): exactly zero with no
    /// direction key held, live rather than a decayed measurement.
    pub planar_speed: f32,
    /// Its collision cylinder height in yards — the foam ring's radius input.
    pub height: f32,
    /// The **first-person feather**: how opaque the viewer's own body is as the camera zooms into
    /// it. `1.0` normally, ramping to `0.0` at full zoom-in. A property of the eye's relationship
    /// to the body, which is why it rides here and not on the body.
    pub self_fade: f32,
    /// **Drunkenness**, `0.0..=1.0` — `PLAYER_BYTES_3` byte 1 clamped at 100. The full-screen haze
    /// is a property of the eye, not of any body in the scene.
    pub drunk: f32,
    /// Is the viewer a **ghost**? While the flag is up the active `LightParams` slot is 4 — the
    /// death profile, applied instantly (decision 0308 §7, byte-verified `death-light.md`).
    pub ghost: bool,
    /// Is a loading cover over the world right now, so the viewer has not actually *seen* anything
    /// yet? The appear ramp arms on this falling edge — the faithful trigger is "the player can
    /// see the entity", and the residency proxy goes true well before the cover drops now that
    /// the clear waits for the whole scene (decision 0737).
    pub world_covered: bool,
}

impl Default for Viewer {
    /// No body, and **nothing covering the world** — the defaults a program with no game boots
    /// with. `self_fade` is `1.0`: absent an eye-to-body relationship, nothing is feathered.
    fn default() -> Self {
        Self {
            at: None,
            move_flags: 0,
            planar_speed: 0.0,
            height: 0.0,
            self_fade: 1.0,
            drunk: 0.0,
            ghost: false,
            world_covered: false,
        }
    }
}

impl Viewer {
    /// Is the body **translating**? The four direction bits (`& 0xf`) — the same test the
    /// reference's water-ripple driver runs (`0x5fa760`).
    pub(crate) fn translating(&self) -> bool {
        self.move_flags & 0xf != 0
    }

    /// Is it **turning in place**? The two keyboard turn bits (`& 0x30`). Strafe slides without
    /// turning and is covered by [`Self::translating`]; a mouse-look body-step sets no flag at all.
    pub(crate) fn turning(&self) -> bool {
        self.move_flags & 0x30 != 0
    }
}

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
/// `wow_model.wgsl` / `wow_effect.wgsl` all discard on eye-Z past `fog_params.w`). Agreeing on the
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

/// Marks **the world camera** — the one flying the scene. Every "where is the viewer" consumer
/// (terrain streaming, PVS, sun follow, sound listener, picking, the capture pin, …) filters on this,
/// NOT on `Camera3d`: since the portrait booths (the portrait booths) there are multiple `Camera3d`s,
/// and a bare `With<WorldCamera>` query silently reads (or writes!) an off-screen booth camera — exactly
/// how the capture pin once yanked the booths to the scenario eye and blanked every portrait.
#[derive(Component)]
pub struct WorldCamera;

/// The camera **near-plane** distance (yd) — the reference's own **1/9**, hardcoded in its camera
/// ctor (`0x50a6c0`: `+0x38 = 0x3de38e39`; the `nearclip` console cvar stores to a global with zero
/// readers — dead plumbing; wow-re `water-frame-straddle` §4d). Shared by the projection (the camera spawn)
/// and the self-avatar fade's `nearclip` ([`crate::model_fade::self_model_fade_alpha`]) so the model
/// finishes fading exactly as the near plane would begin to slice it — the reference couples the
/// two the same way (`cam+0x38 ≈ 0.1`, set per frame in the driver `0x511bc0`).
///
/// It was 1.0 from 0062 to 0905 "for depth precision" — a rationale that predates knowing the
/// pipeline: the projection is `perspective_infinite_reverse_rh` on a float depth buffer
/// ([`crate::capture::depth_probe`]'s tests draw with the real one), where `depth = near/z` makes
/// relative precision — and our ULP-relative bias ladder ([`crate::sky_order`]) — independent of
/// the near value. The small near is what keeps the whole waterline-crossing band (the corner-min
/// submersion probe, `liquid::detect_submersion`) inches tall instead of a yard.
pub const CAM_NEAR: f32 = 1.0 / 9.0;
/// The camera's vertical field of view (radians) — one constant shared by the projection
/// (the camera spawn) and every consumer that needs the near rectangle's true shape. 45°, the value the
/// projection has always used (Bevy's `PerspectiveProjection` default, ≈ the reference's 44.1° —
/// [`crate::sun`]'s projection note); naming it here just stops the consumers drifting apart.
pub const CAM_FOVY: f32 = std::f32::consts::FRAC_PI_4;

/// **The world camera's world pose, made current before anything in `Update` reads it** — the
/// set every `Update`-stage viewer authority must order after.
///
/// Bevy propagates `GlobalTransform` in `PostUpdate`. For the whole of `Update`, therefore, a
/// camera's `GlobalTransform` is the pose it had **last** frame, while its `Transform` — written
/// by the controller in [`crate::schedule::WorldStage::Input`] — is this frame's. Walking, the two
/// differ by centimetres and nothing shows. On the frame a teleport snaps they differ by the whole
/// jump, and an authority that decides *what may draw* off the stale one gates a frame drawn from
/// the new pose with a verdict about the old place (decision 1503).
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CameraPoseSet;

/// Copy the world camera's `Transform` into its `GlobalTransform` (see [`CameraPoseSet`]).
///
/// **Root cameras only.** The world camera is spawned unparented (both the fallback and the real
/// one), and for a root the propagation Bevy will run in `PostUpdate` is exactly this copy — so
/// this makes the same value available earlier and `PostUpdate` recomputes it identically. A
/// camera that ever acquires a parent keeps Bevy's propagated value and its old one-frame lag,
/// which is no worse than before and never silently wrong: the pose it reports is a real pose the
/// camera had, just not this frame's.
///
/// Written through `set_if_neq` so a still camera does not flag `Changed<GlobalTransform>` every
/// frame for the consumers that key on it.
/// The root world cameras and their two transforms — see [`publish_camera_pose`].
type RootCameraPose<'w, 's> =
    Query<'w, 's, (&'static Transform, &'static mut GlobalTransform), RootCamera>;
/// A world camera Bevy will propagate as a root (no parent to inherit from).
type RootCamera = (With<WorldCamera>, Without<ChildOf>);

pub(crate) fn publish_camera_pose(mut cam: RootCameraPose) {
    for (t, mut g) in &mut cam {
        g.set_if_neq(GlobalTransform::from(*t));
    }
}

/// The view lane's plugin — today, [`publish_camera_pose`] and its ordering contract.
pub(crate) struct ViewPlugin;

impl Plugin for ViewPlugin {
    fn build(&self, app: &mut App) {
        plugin(app);
    }
}

pub(crate) fn plugin(app: &mut App) {
    app.add_systems(
        Update,
        publish_camera_pose
            .in_set(CameraPoseSet)
            // After the controller that writes the camera's `Transform`
            // ([`crate::schedule::WorldStage::Input`]) — the point of the whole system is to
            // publish the pose the controller just chose, not the one before it.
            .after(crate::schedule::WorldStage::Input),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    /// The contract of [`CameraPoseSet`]: a system ordered after it, in the same `Update`, reads
    /// the pose the controller wrote **this** frame — not the one Bevy will propagate at the end
    /// of it. Written as the frame it fixes: move the camera the way a teleport snap does, and
    /// assert the downstream authority sees the destination.
    #[test]
    fn a_reader_after_the_pose_set_sees_this_frames_camera() {
        #[derive(Resource, Default)]
        struct Seen(Vec3);

        fn snap_the_camera(mut cam: Query<&mut Transform, With<WorldCamera>>) {
            for mut t in &mut cam {
                t.translation = Vec3::new(500.0, 0.0, 0.0);
            }
        }
        fn read_the_pose(cam: Query<&GlobalTransform, With<WorldCamera>>, mut seen: ResMut<Seen>) {
            for g in &cam {
                seen.0 = g.translation();
            }
        }

        let mut app = App::new();
        app.init_resource::<Seen>();
        app.configure_sets(Update, crate::schedule::WorldStage::Input);
        app.add_systems(
            Update,
            snap_the_camera.in_set(crate::schedule::WorldStage::Input),
        );
        plugin(&mut app);
        app.add_systems(Update, read_the_pose.after(CameraPoseSet));
        app.world_mut().spawn((
            WorldCamera,
            Transform::default(),
            GlobalTransform::default(),
        ));

        app.update();
        assert_eq!(
            app.world().resource::<Seen>().0,
            Vec3::new(500.0, 0.0, 0.0),
            "an Update-stage viewer authority must see the snapped pose, not the frame-old one"
        );
    }

    /// A **parented** camera is left to Bevy's propagation: a plain copy of a child's local
    /// `Transform` would be a wrong pose, which is worse than a frame-old right one.
    #[test]
    fn a_parented_camera_is_left_alone() {
        let mut app = App::new();
        plugin(&mut app);
        let parent = app.world_mut().spawn(Transform::default()).id();
        let child = app
            .world_mut()
            .spawn((
                WorldCamera,
                Transform::from_xyz(7.0, 0.0, 0.0),
                GlobalTransform::default(),
            ))
            .id();
        app.world_mut().entity_mut(parent).add_child(child);

        app.world_mut()
            .run_system_once(publish_camera_pose)
            .unwrap();
        assert_eq!(
            app.world()
                .entity(child)
                .get::<GlobalTransform>()
                .unwrap()
                .translation(),
            Vec3::ZERO,
            "the child's local transform is not a world pose"
        );
    }
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
