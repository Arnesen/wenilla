//! The third-person camera rig: the two mouse-look modes (right-drag turns the character, left-drag
//! orbits the camera), the wheel-zoom glide, the collision-swept boom that seats the camera behind the
//! avatar's framing [`CameraPivot`], and the self-avatar zoom-in fade as the boom pulls into first
//! person. Split out of the controller — this owns the camera's pose and input session, not the
//! avatar/movement/networking [`super::control`] drives with it.

use bevy::ecs::entity::EntityHashSet;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions};

use avian3d::prelude::*;

use crate::collision::camera_query_filter;
use crate::interact::{WorldClick, WorldRightClick, WorldRightPress};
use crate::model_fade::{
    self_model_fade_alpha, FadeMaterials, PendingAppearFade, RenderFade, SELF_FADE_WINDOW,
};
use crate::net::SelfPlayer;
use crate::terrain::WowModelMaterial;

/// The reference's up-edge click predicate, in **camera degrees and milliseconds** — the whole
/// orbit-vs-select law (decision 1122; wow-re `world-click-drag-arbitration.md`, §5 fan-out
/// 2026-08-08). Verbatim, from `0x514ae0`, which returns 1 = suppress / 0 = dispatch:
///
/// ```text
/// isClick = elapsed < 200ms
///        || (elapsed < 800ms && yaw_travel < 2.25° && pitch_travel < 2.0°)
/// ```
///
/// **A press under 200 ms selects however far the mouse swept.** That arm is the entire bug report
/// (ledger B226): flick the cursor across a mob and click on arrival with the hand still moving, and
/// the reference targets it — the camera has been orbiting since the first motion sample and selects
/// anyway. The two mechanisms are independent and share only the button state.
///
/// **The orbit has no threshold at all** — it engages on the *down* edge (`0x51491f`, guarded only by
/// the 0→1 held transition) and the first motion sample already turns the camera. benilla deferred it
/// behind 4 px of travel and, worse, *destroyed* the pending click on crossing; there is no such
/// thing in the reference. The travel numbers below gate the **click**, never the orbit.
///
/// The literals in the binary are 8.0 event-units per axis against `Σ|0.8·Δx|` / `Σ|0.6·Δy|` of raw
/// device counts. Copying those would bind us to the reference's device scaling, which is itself
/// only INFERRED ("1 unit = 1 mouse count" is not established). So we copy the **angle** they mean,
/// which is exact and device-independent: 8.0 event-units is `Σ|Δx| = 10.0` raw ⇒
/// `cameraYawMoveSpeed·10/800` = **2.25°** of yaw at the shipped default 180; and `Σ|Δy| = 13.333`
/// raw ⇒ `cameraPitchMoveSpeed·13.333/600` = **2.0°** of pitch at the default 90.
///
/// Per axis, independently, and **accumulated absolute travel** — not net displacement from the
/// press point (`0x514400`: `fabs; fadd; fstp`, no subtraction and no stored origin). A shake that
/// returns to where it started still spends the budget.
const CLICK_HOLD_CEILING: f32 = 0.800;
const CLICK_FREE_WINDOW: f32 = 0.200;
const CLICK_YAW_TRAVEL: f32 = 2.25 * std::f32::consts::PI / 180.0;
const CLICK_PITCH_TRAVEL: f32 = 2.0 * std::f32::consts::PI / 180.0;

/// A primary button's press, while it is still undecided — the reference's world-input state
/// (`[0xbe1148]`): press instant `+0x14`, and the two independent motion accumulators zeroed at
/// `0x514910`/`0x514913`. Our accumulators are in **radians of camera rotation** rather than device
/// counts, for the reason in [`CLICK_HOLD_CEILING`]'s note.
#[derive(Clone, Copy)]
pub(super) struct PressGesture {
    /// Seconds on the app clock when the button went down.
    at: f32,
    /// Accumulated |Δyaw| and |Δpitch| this press has asked the camera for, radians.
    yaw_travel: f32,
    pitch_travel: f32,
}

impl PressGesture {
    fn new(now: f32) -> Self {
        Self {
            at: now,
            yaw_travel: 0.0,
            pitch_travel: 0.0,
        }
    }

    /// `0x514ae0`, minimised: the free window first, then the bounded-travel window.
    fn is_click(&self, now: f32) -> bool {
        let elapsed = now - self.at;
        elapsed < CLICK_FREE_WINDOW
            || (elapsed < CLICK_HOLD_CEILING
                && self.yaw_travel < CLICK_YAW_TRAVEL
                && self.pitch_travel < CLICK_PITCH_TRAVEL)
    }
}

/// Third-person orbit-distance limits (yards). **VERIFIED from `WoW.exe` 5875** (`FUN_005112d0` +
/// the camera CVars, wow-re `follow-camera`): max orbit = `cameraDistanceMax × cameraDistanceMaxFactor`,
/// **hard-capped at 50**; the low clamp is **0** — zoom-to-first-person (at distance 0 the eye sits at
/// the framing pivot, inside the head, and the avatar fades to invisible — see
/// [`crate::model_fade::self_model_fade_alpha`]). The out-of-box *default* max is 15 (`15 × 1`); we use
/// **30** as the max zoom-out — the "Max Camera Distance" setting fully raised (`cameraDistanceMax 15 ×
/// cameraDistanceMaxFactor 2.0`, well under the 50 cap), matched against the reference client. (The 2.0
/// factor cap is inferred from the ref-client comparison, not byte-verified — the RE pinned the CVar
/// *defaults* and the 50 hard cap, not the factor slider's own max.) Our starting zoom is 15 — pulled
/// back a bit further than vanilla's own default for a wider view.
pub(super) const CAM_DIST_MIN: f32 = 0.0;
pub(super) const CAM_DIST_MAX: f32 = 30.0;
pub(super) const CAM_DIST_DEFAULT: f32 = 15.0;
/// Yards the wheel moves the target per notch — `CameraZoomIn`/`CameraZoomOut`'s default `amount`
/// (VERIFIED 1.0 in `WoW.exe`).
const CAM_ZOOM_STEP: f32 = 1.0;
/// Camera zoom speed in **yards/second** — `cameraDistanceMoveSpeed` (VERIFIED default 8.33). Vanilla
/// glides the distance toward the wheel target at this *constant velocity* (linear, frame-delta-scaled
/// — `FUN_005112d0` in `WoW.exe`), **not** an exponential ease.
const CAM_MOVE_SPEED: f32 = 8.33;
/// Mouse-look sensitivity, radians of camera rotation per pixel of mouse motion.
const LOOK_SENSITIVITY: f32 = 0.003;

/// The mouse-look player knobs (decision 0961): `mouseInvertPitch` is 1.12's own Interface
/// Options checkbox (UIOptionsFrame.lua index 1, CVar-backed), settable from the Options
/// window's Controls page through the CVar store (0954). Inverted, moving the mouse up pitches
/// the camera down — the delta.y term flips sign at the one apply site, both drag styles alike.
#[derive(Resource, Default)]
pub(crate) struct LookConfig {
    pub(crate) invert_pitch: bool,
}
/// Camera pitch clamp (radians) — **VERIFIED ±89.00°** (`WoW.exe` `0x8089d8`/`0x8089dc` =
/// 1.5533430576 rad; the pitch integrate `FUN_00510120`, wow-re `follow-camera`). A single uniform
/// clamp at every zoom level — the reference has **no** distinct first-person look-down limit.
pub(super) const CAM_PITCH_LIMIT: f32 = 89.0 * std::f32::consts::PI / 180.0;
/// Camera-collision probe radius (yd): a small sphere swept from the camera pivot toward the desired
/// camera seat each frame. Its radius is the margin kept between the camera and the surface it stops
/// at, so the near plane doesn't poke through the wall. Smaller than the player capsule — the camera
/// threads gaps the body can't fit.
pub(super) const CAM_COLLISION_RADIUS: f32 = 0.3;
/// How fast the camera glides back out to the player's chosen zoom once an obstruction clears (1/s).
/// Pull-*in* is instant (a wall must never sit between the camera and the character); only the
/// push-*out* eases — the vanilla feel of the camera snapping close past an obstacle and easing back.
const CAM_RETURN_RATE: f32 = 6.0;
/// The camera framing pivot — the point the boom looks at + seats behind, and the first-person eye at
/// zoom 0 — sits at `feet + H` where **H is model-derived** (not a fixed height): VERIFIED
/// `H = (attach17.z + 0.0972) × scale` from **M2 attachment id 17** (`WoW.exe` `0x50cbc0`, wow-re
/// `follow-camera`) — ~neck height on every character (1.90 human / 0.88 gnome), with a `0.9 × vertex-box`
/// fallback only for models lacking that attachment. Floored at [`CAM_PIVOT_FLOOR`]. The per-model
/// pre-scale height rides on [`CameraPivot`], stamped at attach; `control` multiplies the live scale and
/// floors. The collision sweep still starts from the *head* (not the pivot), so a jump in a low room
/// stops the camera under the ceiling — see `control`.
///
/// Floor (yd) on the world pivot height — VERIFIED `5/6` (`0x50e570`'s `max(hi, target)` lower bound).
pub(super) const CAM_PIVOT_FLOOR: f32 = 5.0 / 6.0;
/// Pivot height used before the avatar model has attached (so `CameraPivot` isn't on the entity yet):
/// a human's ~neck height, so the first frames of third-person don't ride high. Replaced by the exact
/// model-derived value the moment the body attaches.
pub(super) const CAM_PIVOT_FALLBACK: f32 = 1.8;

/// World head height above a modeled unit's feet — its model-local [`CameraPivot`] (`attach17`-derived
/// head/eye for a character, else `0.9 × bbox-z`) × the live scale, floored at [`CAM_PIVOT_FLOOR`], or
/// the neck-height [`CAM_PIVOT_FALLBACK`] before the body attaches. The single definition shared by the
/// two things that sit at the character's head: the third-person framing pivot and the 3D-audio
/// listener (the client's `SoundListenerAtCharacter=1` default, wow-re benilla-pins B14).
pub(crate) fn head_height(pivot: Option<&CameraPivot>, scale: f32) -> f32 {
    pivot.map_or(CAM_PIVOT_FALLBACK, |p| {
        (p.height_local * scale).max(CAM_PIVOT_FLOOR)
    })
}
/// The camera **near-plane** distance (yd) — the reference's own **1/9**, hardcoded in its camera
/// ctor (`0x50a6c0`: `+0x38 = 0x3de38e39`; the `nearclip` console cvar stores to a global with zero
/// readers — dead plumbing; wow-re `water-frame-straddle` §4d). Shared by the projection ([`setup`])
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
pub(crate) const CAM_NEAR: f32 = 1.0 / 9.0;
/// The camera's vertical field of view (radians) — one constant shared by the projection
/// ([`setup`]) and every consumer that needs the near rectangle's true shape. 45°, the value the
/// projection has always used (Bevy's `PerspectiveProjection` default, ≈ the reference's 44.1° —
/// [`crate::sun`]'s projection note); naming it here just stops the consumers drifting apart.
pub(crate) const CAM_FOVY: f32 = std::f32::consts::FRAC_PI_4;

/// A small sphere swept from the camera pivot toward the desired camera seat each frame to keep walls
/// from sliding between the camera and the character (camera collision). Built once at startup like
/// [`PlayerCapsule`]; smaller than the body capsule so the camera can thread gaps the player can't.
#[derive(Resource)]
pub(super) struct CameraProbe(pub(super) Collider);

/// Which mouse button is driving mouse-look, if any — the two vanilla look modes. While looking, the
/// OS cursor is hidden + locked in place (relative motion drives the camera); `cursor_stash` is the
/// position it's restored to on release so it reappears exactly where the user pressed. `pub(crate)`
/// so the [`crate::cursor`] subsystem can hide the cursor while looking (`is_looking`).
#[derive(Resource, Default)]
pub(crate) struct CameraControl {
    /// Current third-person orbit distance (yards) — eased toward `target_distance` each frame so the
    /// wheel zoom glides instead of snapping (like the real client).
    pub(super) distance: f32,
    /// Where the wheel set the orbit distance; `distance` chases this.
    pub(super) target_distance: f32,
    /// Effective length of the camera arm (from the head pivot out to the camera) after world
    /// collision. Pulled in instantly when geometry intrudes (so a wall never sits between the camera
    /// and the character), eased back out when it clears. Kept separate from the zoom `distance` so the
    /// player's chosen zoom is preserved while obstructed and restored once the view is open again.
    pub(super) collision_distance: f32,
    /// The button currently held for look, or `None`.
    pub(super) look: Option<LookButton>,
    /// Logical cursor position captured when look began, to restore on release.
    pub(super) cursor_stash: Option<Vec2>,
    /// The self-avatar's render alpha for this frame, from the camera-to-pivot distance
    /// ([`crate::model_fade::self_model_fade_alpha`]): `1.0` third-person (opaque), ramping to `0.0` as
    /// the camera zooms into the head (first-person). `control` computes it (it owns the pivot + camera
    /// pose); [`apply_self_model_fade`] applies it to the body parts. Starts opaque.
    pub(super) self_fade_alpha: f32,
}

impl CameraControl {
    /// True while a mouse-look drag is active (right- or left-button). The cursor is hidden then.
    pub(crate) fn is_looking(&self) -> bool {
        self.look.is_some()
    }

    /// The self-avatar's render alpha this frame (`1.0` third-person → `0.0` first-person). The
    /// blob shadow multiplies it in for the self unit — the reference's shadow diffuse rides the
    /// same model fade slot the body does (`[model+0x180]`, wow-re unit-blob-shadow RE).
    pub(crate) fn self_fade(&self) -> f32 {
        self.self_fade_alpha
    }
}

/// The active mouse-look mode. `Right` turns the character (movement follows the camera heading);
/// `Left` orbits the camera around a stationary character (vanilla left-drag look).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum LookButton {
    Right,
    Left,
}

impl LookButton {
    fn button(self) -> MouseButton {
        match self {
            LookButton::Right => MouseButton::Right,
            LookButton::Left => MouseButton::Left,
        }
    }
}

#[derive(Component)]
pub(super) struct FlyCam {
    pub(super) yaw: f32,
    pub(super) pitch: f32,
    pub(super) speed: f32,
}

/// Marks **the world camera** — the one flying the scene. Every "where is the viewer" consumer
/// (terrain streaming, PVS, sun follow, sound listener, picking, the capture pin, …) filters on this,
/// NOT on `Camera3d`: since the portrait booths ([`crate::portrait`]) there are multiple `Camera3d`s,
/// and a bare `With<WorldCamera>` query silently reads (or writes!) an off-screen booth camera — exactly
/// how the capture pin once yanked the booths to the scenario eye and blanked every portrait.
#[derive(Component)]
pub(crate) struct WorldCamera;

/// The per-model camera-pivot height in **model-local yards, pre-scale** — `attach17.z + 0.0972` (M2
/// attachment id 17) for a character, else `0.9 × vertex-box Z-extent`; the reference's camera-target
/// height (`0x50cbc0`, wow-re `follow-camera`). Stamped on every modeled unit at attach
/// ([`crate::entities`]); `control` reads it off the [`SelfPlayer`], multiplies the live avatar scale,
/// and floors at [`CAM_PIVOT_FLOOR`] to get the world pivot the third-person camera looks at (and the
/// first-person eye). `0.0` for a bounds-less display (→ floor).
#[derive(Component, Clone, Copy)]
pub(crate) struct CameraPivot {
    pub height_local: f32,
}

/// Mouse-look session state machine — start/stop/hand-off between the two look buttons, cursor
/// grab/stash/restore, and the two click tests that emit [`WorldClick`]/[`WorldRightClick`]. Also
/// applies this frame's accumulated motion as look rotation while a button is held (right-drag syncs
/// the character facing too). Called once per frame from [`super::control`]; `both_buttons` is
/// vanilla's both-button run (steers like a right-drag without its own click test).
///
/// **Orbit and select are independent** (decision 1122): every primary press engages its look
/// session immediately and *also* arms a click test, and the release decides the click on
/// [`PressGesture::is_click`] alone. There is no "promotion" and nothing cancels the click for
/// having moved — the pending click used to be destroyed the moment the cursor crossed a 4 px
/// threshold, which is why a drag could never select (ledger B226).
#[allow(clippy::too_many_arguments)]
pub(super) fn run_look_session(
    buttons: &ButtonInput<MouseButton>,
    mouse_motion: &AccumulatedMouseMotion,
    both_buttons: bool,
    rig: &mut CameraControl,
    cam: &mut FlyCam,
    face_yaw: &mut f32,
    window: &mut Window,
    cursor_opts: &mut CursorOptions,
    camera: &Camera,
    pointer_over_ui: bool,
    inspect_enabled: bool,
    // A left press this frame the UI already consumed as a cursor-payload world drop (0216 §3) —
    // the left click test must yield to it exactly as it yields to a UI hover, so dropping a held
    // item never also selects.
    click_consumed: bool,
    world_click: &mut MessageWriter<WorldClick>,
    world_right_click: &mut MessageWriter<WorldRightClick>,
    world_right_press: &mut MessageWriter<WorldRightPress>,
    left_click: &mut Option<PressGesture>,
    right_click: &mut Option<PressGesture>,
    invert_pitch: bool,
    // Seconds on the app clock — the press predicate's two time gates are measured against it.
    now: f32,
) {
    // The right button's DOWN edge, before any click-vs-drag classification — the reference's
    // WorldFrame OnMouseDown fires at the press whether it becomes a click or a turn. It belongs
    // to the world when the press lands in the viewport off the UI, or whenever a look session
    // already owns the (hidden, locked) cursor — a right join into a left-orbit is still a world
    // press. Ground-targeting's cancel reads this edge (decision 0792).
    if buttons.just_pressed(MouseButton::Right)
        && (rig.look.is_some() || (cursor_in_viewport(window, camera) && !pointer_over_ui))
    {
        world_right_press.write(WorldRightPress);
    }
    // A chord — both primaries down — is a both-button run, never a select. The reference kills the
    // pending click on the *second* press and refuses to arm a new one while another primary is held
    // (`0x514ac1`, `0x51481a`), so neither release of a chord can dispatch. Cancel both tests.
    if buttons.pressed(MouseButton::Left) && buttons.pressed(MouseButton::Right) {
        *left_click = None;
        *right_click = None;
    }
    // Both tests just ride their session, accumulating the camera rotation the press has asked for;
    // the release decides. The travel is charged from the *input* delta, before the pitch clamp —
    // the reference accumulates raw device motion, so a drag pinned at the pitch limit still spends
    // its budget.
    let dyaw = (mouse_motion.delta.x * LOOK_SENSITIVITY).abs();
    let dpitch = (mouse_motion.delta.y * LOOK_SENSITIVITY).abs();
    for test in [&mut *left_click, &mut *right_click].into_iter().flatten() {
        test.yaw_travel += dyaw;
        test.pitch_travel += dpitch;
    }

    // Mouse-look start/stop + cursor grab. **Both** buttons engage their look session on the DOWN
    // edge — the reference has no deferral and no engage threshold (`0x51491f`), and the first
    // motion sample already turns the camera. The click test is a separate rider decided at the
    // release, which is exactly what lets one gesture orbit *and* select. Either button hides +
    // locks the cursor in place (so it can't drift out of the window while we turn) and restores it
    // where it was on release. A press that begins over the debug panel is egui's, not ours.
    if let Some(active) = rig.look {
        if !buttons.pressed(active.button()) {
            // The session's button went up: settle its click test against the reference predicate.
            // A handoff release (the other button still held) never fires — the chord already
            // cancelled both tests above.
            let test = match active {
                LookButton::Left => left_click.take(),
                LookButton::Right => right_click.take(),
            };
            if let Some(test) = test {
                if test.is_click(now) {
                    match active {
                        LookButton::Left => {
                            world_click.write(WorldClick);
                        }
                        LookButton::Right => {
                            world_right_click.write(WorldRightClick);
                        }
                    }
                }
            }
            // The latched button went up. If the *other* look button is still held (both-button run
            // → single-button), hand the look session off to it rather than ending it — vanilla keeps
            // turning/orbiting seamlessly on the remaining button, cursor staying hidden throughout.
            let other = match active {
                LookButton::Right => LookButton::Left,
                LookButton::Left => LookButton::Right,
            };
            if buttons.pressed(other.button()) {
                rig.look = Some(other);
            } else {
                rig.look = None;
                cursor_opts.grab_mode = CursorGrabMode::None;
                // Show the cursor again (cross-platform; on macOS hiding is the cursor subsystem's job).
                cursor_opts.visible = true;
                if let Some(pos) = rig.cursor_stash.take() {
                    window.set_cursor_position(Some(pos));
                }
            }
        }
    } else {
        // A press over the egui dev UI (the overlaid debug panel, the perf pill) or outside the world
        // viewport is not ours — this keeps a slider-drag from grabbing the cursor into mouse-look.
        let world_press = cursor_in_viewport(window, camera) && !pointer_over_ui;
        // Right-drag turn. Arms its context-click test too; not armed when left is already down (a
        // chord is never a click).
        if buttons.just_pressed(MouseButton::Right) && world_press {
            rig.look = Some(LookButton::Right);
            rig.cursor_stash = window.cursor_position();
            cursor_opts.grab_mode = CursorGrabMode::Locked;
            cursor_opts.visible = false;
            *right_click = (!buttons.pressed(MouseButton::Left)).then(|| PressGesture::new(now));
        } else if buttons.just_pressed(MouseButton::Left) && world_press && !inspect_enabled {
            // Left-drag orbit — engaged on the press, exactly like right, because the reference
            // engages on the press (`0x51491f`). The select is not deferred behind it; it rides
            // along and settles at the release. While the inspector is armed left belongs to it
            // (its own copy-on-click handler), so neither the orbit nor the test starts.
            rig.look = Some(LookButton::Left);
            rig.cursor_stash = window.cursor_position();
            cursor_opts.grab_mode = CursorGrabMode::Locked;
            cursor_opts.visible = false;
            // A press the UI already consumed as a cursor-payload world drop (0216 §3) still orbits
            // — the reference's orbit is unconditional on the down edge — but must not also select.
            *right_click = None;
            *left_click = (!click_consumed && !buttons.pressed(MouseButton::Right))
                .then(|| PressGesture::new(now));
        }
    }

    // Apply this frame's accumulated motion as look rotation while a button is held. Right-drag also
    // turns the character (its facing tracks the camera yaw); left-drag leaves the character facing.
    if let Some(active) = rig.look {
        let delta = mouse_motion.delta;
        cam.yaw -= delta.x * LOOK_SENSITIVITY;
        // `mouseInvertPitch` flips only the pitch axis (the 1.12 checkbox's whole meaning).
        let dy = if invert_pitch { -delta.y } else { delta.y };
        cam.pitch = (cam.pitch - dy * LOOK_SENSITIVITY).clamp(-CAM_PITCH_LIMIT, CAM_PITCH_LIMIT);
        if active == LookButton::Right || both_buttons {
            *face_yaw = cam.yaw;
        }
    }
}

/// Wheel-zoom: the CAMERAZOOMIN/OUT bindings set a new target orbit distance, and the actual
/// distance glides toward it at a constant `cameraDistanceMoveSpeed` (vanilla's linear,
/// frame-delta-scaled glide — not an ease). Runs every frame regardless of active/detached state,
/// mirroring the reference camera. `scroll` is this frame's net zoom-in amount (wheel notches in
/// line-equivalents — the binding dispatch normalizes trackpad pixels — or the 1.12 key step of
/// 1.0 per press; positive = closer), so a rebound zoom key feels exactly like a wheel notch.
pub(super) fn apply_zoom_scroll(scroll: f32, dt: f32, rig: &mut CameraControl) {
    if scroll != 0.0 {
        rig.target_distance =
            (rig.target_distance - scroll * CAM_ZOOM_STEP).clamp(CAM_DIST_MIN, CAM_DIST_MAX);
    }
    // Glide the actual distance toward the wheel target at a constant `cameraDistanceMoveSpeed` yd/s,
    // stopping exactly there — the verified vanilla behavior (linear, frame-delta-scaled; not an ease).
    let max_step = CAM_MOVE_SPEED * dt;
    rig.distance += (rig.target_distance - rig.distance).clamp(-max_step, max_step);
}

/// Seat the third-person camera: orient it, orbit it behind the avatar's torso with a collision
/// sweep from the head to the ideal seat (snap-in instantly, ease back out), write the resulting
/// transform, and compute the self-avatar zoom-in fade from the realized camera-to-pivot distance.
/// A **keyboard** turn (or the drunk veer, which rides `turn_delta` the same way — decision 1018)
/// carries the camera rigidly — the character's own turns only: a transport
/// deck turning under the rider is frame motion and is applied to `cam.yaw` at the ride block in
/// [`super::control`], bypassing this function's look-session gate (routing it here was the
/// right-drag drift bug — the gate ate the deck's share while a drag was held). A left-drag orbit
/// offset otherwise **persists** (no auto-follow — director's call, see the body).
/// `head`/`player_pos` are precomputed by [`super::control`] (which owns the avatar capsule
/// constants); `cam_pivot_height` is the world pivot height it derived from [`CameraPivot`] this
/// frame.
#[allow(clippy::too_many_arguments)]
pub(super) fn seat_camera(
    dt: f32,
    turn_delta: f32,
    player_pos: Vec3,
    head: Vec3,
    cam_pivot_height: f32,
    rig: &mut CameraControl,
    cam: &mut FlyCam,
    cam_t: &mut Transform,
    move_and_slide: &MoveAndSlide,
    cam_probe: &Collider,
) {
    // A keyboard turn carries the camera RIGIDLY (char and camera rotate as one — the reference
    // look, director's call closing 0050's open "camera follow on turn"): an eased chase of a
    // continuously-turning facing lags by ω/rate, which read as the char angled on screen while
    // run-turning and a release-snap landing off-camera. A drag (`rig.look` held) owns the camera
    // — no INPUT-turn carry against the user's hand. (A transport deck's turn is not an input and
    // never arrives here — the ride block applies it to `cam.yaw` directly, drag or no drag.)
    //
    // A left-drag orbit offset now **persists** once released: the vanilla `cameraSmoothStyle`
    // auto-follow that eased the camera back behind the character while moving/turning is removed
    // (director's call — we don't want it even though it's faithful). The camera stays where you
    // put it; only a fresh drag, or a right-drag/movement that resyncs `face_yaw`, moves it.
    if rig.look.is_none() {
        cam.yaw += turn_delta;
    }
    // Orient the camera, then orbit it behind the avatar's torso. The framing **pivot** is
    // `feet + cam_pivot_height` (model-derived, ~neck height — [`CameraPivot`]); the camera looks at
    // it and, at zoom 0, sits *on* it (first-person eye inside the head). Camera collision is a single
    // sweep of the probe sphere from the player's *head* (the capsule's top hemisphere centre) out to
    // the ideal camera seat (`pivot - fwd·zoom`). The camera rides along that sweep, stopping at the
    // first surface (held off it by the probe radius). Rooting the arm at the head is what makes it
    // robust: body collision keeps the head inside the room — even mid-jump it can't pass the ceiling
    // — so the swept camera can never end up on the far side of a wall or ceiling. That is why a jump
    // in a low room no longer pushes it through the roof: the sweep just stops under the ceiling
    // instead of overshooting (the old min-distance floor used to force the camera *past* a too-close
    // hit — gone; collision wins outright). `cast_move` ignores origin penetration, so a head grazing
    // a surface still casts outward.
    cam_t.rotation = Quat::from_euler(EulerRot::YXZ, cam.yaw, cam.pitch, 0.0);
    let cam_fwd = *cam_t.forward();
    let pivot = player_pos + Vec3::Y * cam_pivot_height;
    let seat = pivot - cam_fwd * rig.distance;
    let boom = seat - head;
    let boom_len = boom.length().max(1.0e-3);
    // The camera collides with the WMO *camera/LOS* faces (keeps DETAIL overhangs like forge pipes,
    // drops NOCAMCOLLIDE) + terrain/doodads/GameObjects — its own audience, not the walking mesh.
    let open = move_and_slide
        .cast_move(
            cam_probe,
            head,
            Quat::IDENTITY,
            boom,
            0.0,
            &camera_query_filter(),
        )
        .map_or(boom_len, |h| h.distance);
    // Snap in instantly when geometry intrudes (a wall must never sit between camera and character);
    // ease back out to the open arm length once it clears — the vanilla snap-close-then-glide-back.
    rig.collision_distance = if open < rig.collision_distance {
        open
    } else {
        let t = 1.0 - (-CAM_RETURN_RATE * dt).exp();
        rig.collision_distance + (open - rig.collision_distance) * t
    };
    let frac = (rig.collision_distance / boom_len).clamp(0.0, 1.0);
    cam_t.translation = head + boom * frac;
    // No waterline handling here — deliberately. The reference NEVER moves the eye for liquid
    // (verified negative, wow-re `water-frame-straddle` §4a: zero liquid-height queries in the
    // camera TU); the no-straddle experience is the *submersion probe's* — the frame flips
    // submerged the moment the lowest near-plane corner reaches the surface
    // (`liquid::detect_submersion`, the corner-min probe), and with [`CAM_NEAR`] at the
    // reference's 1/9 the whole crossing band is a few inches tall. 0905's eye snap — the local
    // compensation for the old 1.0-yd near plane — is removed with its cause (its record is
    // superseded; see the 0905-successor decision).
    // `WOW_CAM_DUMP=frame`: the REALIZED pose, per frame, bit-exact — not the pose that was asked for.
    //
    // Every scripted probe sets `yaw`/`pitch`/`distance` and we then reason as though the camera is
    // therefore where we put it. It is not: `collision_distance` is an exponentially-eased chase of a
    // per-frame collision CAST, so a grazing hit that alternates gives an arm that snaps in and eases
    // back out, and the camera keeps moving for as long as that lasts — with the scripted pose
    // perfectly constant the whole time. B38's "the camera is static by construction, so nothing
    // camera-derived can be the cause" (0671) rests entirely on that being untrue, and it was never
    // measured. `open` is printed beside the eased arm so a hit/miss alternation in the CAST is
    // visible even on a frame where the ease has not yet moved the camera far enough to see.
    if std::env::var_os("WOW_CAM_DUMP").is_some() {
        eprintln!(
            "[cam] yaw {:.6} pitch {:.6} dist {:.6} open {:.6} coll {:.6} frac {:.6} \
             pos [{:.6},{:.6},{:.6}] bits [{:08x},{:08x},{:08x}]",
            cam.yaw,
            cam.pitch,
            rig.distance,
            open,
            rig.collision_distance,
            frac,
            cam_t.translation.x,
            cam_t.translation.y,
            cam_t.translation.z,
            cam_t.translation.x.to_bits(),
            cam_t.translation.y.to_bits(),
            cam_t.translation.z.to_bits(),
        );
    }

    // Fade the avatar as the camera nears its pivot (zoom-in / a wall pulling the boom in): opaque
    // in third-person, ramping to invisible in first-person. Keyed off the *realized* camera→pivot
    // distance (collision-pulled), so backing into a wall also thins you — the faithful behavior.
    rig.self_fade_alpha = self_model_fade_alpha(
        (cam_t.translation - pivot).length(),
        CAM_NEAR,
        SELF_FADE_WINDOW,
    );
}

/// Apply the self-avatar zoom-in fade ([`CameraControl::self_fade_alpha`], computed in [`control`]) to
/// the player's own body parts **and every attach-model descendant** (held items, helm, shoulders —
/// [`crate::entities::BoneAttach`] rides them several levels down through the joint hierarchy), so you
/// go translucent then invisible — weapon and armor included — as the camera zooms into the head. Drives
/// the same per-instance render-alpha channel as [`crate::model_fade::apply_render_fade`] — the `MeshTag`
/// alpha field on the blend-twin material — and hard-hides via [`Visibility`] at α 0 (true
/// first-person; cheaper + cleaner than a ≈0-alpha head sitting on the camera).
///
/// Runs **after** the interior classifier + the appear/despawn fades so its override wins the frame; it
/// overrides while fading (`α < 1`) and, on the frame the fade ends, **releases** the channel back
/// (decision 0213): the classifier skips settled parts and rewrites only on a classification change, so
/// without an explicit hand-back a fade episode that ends in a jump past 1 (a hitch frame closing the
/// camera ease in one step, a pivot jump) left the avatar latched on the blend twin at its last low alpha
/// — stuck translucent until the player happened to cross a room boundary. At steady `α ≥ 1` it does
/// nothing, leaving the classifier the sole steady-state author. Parts mid appear/despawn fade
/// (`RenderFade`/`PendingAppearFade`) are left to that fade — it's brief, owns the channel, and performs
/// its own release on completion.
///
/// Walks the **full** descendant tree from the avatar root rather than just its direct children: body
/// submeshes are direct children of the root, but a held item / helm / shoulder is a child of a joint
/// entity (itself a descendant of the root, at varying depth) — an earlier direct-`Children`-only version
/// silently skipped every attach model. The self-player entity is singular, so a per-frame tree walk over
/// its handful of joints + submeshes is nil cost.
///
/// **The tree is not the whole model.** An M2's BILLBOARD batches can't be tree children — their mesh is
/// centred on the bone pivot and their transform belongs to the billboard system, so every one of them is
/// a world ROOT entity that merely *follows* an anchor inside the tree (decision 0153). The descendant
/// walk therefore cannot see them, and the night-elf eye glow — two additive `…EYEGLOW.BLP` billboard
/// quads at head height — went on burning in mid-air after the body it belongs to had gone (reported
/// first-hand; ledger B71). Cards are picked up here by testing their follow-anchor against the walked
/// set, and folded into the same α — the idiom [`crate::blob_shadow`] already uses for the other
/// world-root follower of the self avatar ("the self first-person fade rides the same model-fade slot in
/// the reference"). One multiply covers both halves: it feathers with the body, and at α 0 the additive
/// compose (`wow_model.wgsl`: `out_rgb *= faded_alpha`) takes the card to black, which for an ADD blend
/// is gone. That deliberately avoids `Visibility`, which the card's own hidden-owner mirror authors every
/// frame in a different system.
#[allow(clippy::type_complexity, clippy::too_many_arguments)] // one Bevy system's full input set
pub(crate) fn apply_self_model_fade(
    rig: Res<CameraControl>,
    self_player: Query<(Entity, Option<&crate::aura_visual::AuraNodes>), With<SelfPlayer>>,
    children_of: Query<&Children>,
    mut parts: Query<
        (
            &FadeMaterials,
            &mut MeshTag,
            &mut MeshMaterial3d<WowModelMaterial>,
            &mut Visibility,
            Option<&crate::interior::InteriorLit>,
            Has<crate::model_render::FarSideOfWater>,
        ),
        (
            Without<RenderFade>,
            Without<PendingAppearFade>,
            // Disjointness for the card query below (both want `&mut MeshTag`). A card carries
            // `FadeMaterials` too since 0836, so this now genuinely diverts them — into the loop
            // at the end, which applies the same law without touching `Visibility` (a card's own
            // hidden-owner mirror authors that in a different system).
            Without<crate::billboard::BillboardCard>,
        ),
    >,
    mut cards: Query<(
        &crate::billboard::BillboardCard,
        &mut MeshTag,
        Option<&crate::doodad_anim::MatAnim>,
        Option<&FadeMaterials>,
        Option<&mut MeshMaterial3d<WowModelMaterial>>,
        Option<&crate::interior::InteriorLit>,
        Has<crate::model_render::FarSideOfWater>,
    )>,
    // The water-plane axis, composed into every pick below (`far_resolved`) like every other
    // owner of the handle channel — the feather and the classifier converge, never re-swap.
    far_twins: Res<crate::model_render::FarSideTwins>,
    mut reauthor: ResMut<crate::interior::InteriorReauthor>,
    mut was_fading: Local<bool>,
) {
    let fading = rig.self_fade_alpha < 1.0;
    if !fading && !*was_fading {
        // Steady opaque: nothing to author and nothing to release.
        return;
    }
    let Ok((root, aura)) = self_player.single() else {
        *was_fading = false;
        return;
    };
    // Our own live aura translucency (stealth, invisibility, ghost — `crate::aura_visual`) is a
    // FACTOR of this fade, not a rival author: this system runs last on the self body and writes the
    // alpha field verbatim, so a zoom-in while stealthed must carry the aura's term or the feather
    // would silently re-opaque the character to 1.0 × the camera ramp. Folding it in here also makes
    // the release edge honest — it releases at the *product*, so a fade ending while still stealthed
    // hands the material back only if the body is genuinely opaque again.
    let feather = rig.self_fade_alpha * crate::aura_visual::root_alpha(aura);
    // The walked set doubles as "which anchors belong to this model" for the card pass — built only
    // while fading, so the steady state (the early return above) never pays for it.
    let mut walked = EntityHashSet::default();
    apply_self_fade_to_descendants(
        root,
        feather,
        &children_of,
        &mut parts,
        &far_twins,
        &mut reauthor,
        &mut walked,
    );
    let alpha = feather.clamp(0.0, 1.0);
    for (card, mut tag, anim, fm, mat, lit, far_side) in &mut cards {
        if !card
            .follows()
            .is_some_and(|anchor| walked.contains(&anchor))
        {
            continue;
        }
        // The card's steady author is `entities::apply_unit_mat_alpha`, ordered before this system,
        // which writes the batch's per-sequence factor every frame — so composing from `current`
        // (not from the tag we'd read back) keeps that animation alive under the fade, and the
        // release frame's `α = 1` write lands exactly on the value it would have had.
        let authored = anim.map_or(1.0, |a| a.current);
        let bits = crate::mesh_tag::with_alpha(tag.0, authored * alpha);
        if tag.0 != bits {
            tag.0 = bits;
        }
        // …and the blend twin while feathering, exactly as a mesh part does. The alpha alone is
        // enough for an ADDITIVE card (`wow_model.wgsl` folds it into the colour, so α 0 is black
        // is gone), but an OPAQUE one — a pauldron's camera-facing trim, a chain link — ignores it
        // entirely and stayed solid in first person until the card carried a twin to swap to
        // (decision 0836). No `Visibility` here: that channel belongs to the card's hidden-owner
        // mirror in another system.
        if let (Some(fm), Some(mut mat)) = (fm, mat) {
            let want = crate::model_render::far_resolved(
                fm.material_for(lit, alpha < 1.0),
                far_side,
                &far_twins,
            );
            if mat.0 != *want {
                mat.0 = want.clone();
            }
        }
    }
    *was_fading = fading;
}

/// Depth-first helper for [`apply_self_model_fade`]: apply the fade (or, at `α ≥ 1`, the release) to
/// `entity` if it's a fadeable part, then recurse into its children regardless (a joint or an attach-model
/// root carries no `FadeMaterials` itself but must still be descended through to reach the mesh leaves
/// under it). Every entity visited — parts, joints, attach roots, billboard anchors alike — is recorded
/// in `walked`, which the caller uses to recognise the world-root billboard cards that follow this model.
#[allow(clippy::type_complexity)]
fn apply_self_fade_to_descendants(
    entity: Entity,
    alpha: f32,
    children_of: &Query<&Children>,
    parts: &mut Query<
        (
            &FadeMaterials,
            &mut MeshTag,
            &mut MeshMaterial3d<WowModelMaterial>,
            &mut Visibility,
            Option<&crate::interior::InteriorLit>,
            Has<crate::model_render::FarSideOfWater>,
        ),
        (
            Without<RenderFade>,
            Without<PendingAppearFade>,
            Without<crate::billboard::BillboardCard>,
        ),
    >,
    far_twins: &crate::model_render::FarSideTwins,
    reauthor: &mut crate::interior::InteriorReauthor,
    walked: &mut EntityHashSet,
) {
    walked.insert(entity);
    if let Ok((fm, mut tag, mut mat, mut vis, lit, far_side)) = parts.get_mut(entity) {
        if alpha >= 1.0 {
            // The release edge (runs once, on the frame the fade ends — decision 0213): un-hide,
            // restore the alpha field this system owns, and hand the material back to the part's
            // law. The alpha restore is unconditional: the classifier's payload writes carry the
            // tag's alpha through since 0755 (that is what lets a part re-lane mid-fade), so
            // leaning on its re-author to *also* re-opaque the avatar — as this used to — would
            // leave it stuck translucent, the exact 0213 bug.
            if *vis != Visibility::Inherited {
                *vis = Visibility::Inherited;
            }
            let bits = crate::mesh_tag::with_alpha(tag.0, 1.0);
            if tag.0 != bits {
                tag.0 = bits;
            }
            let want =
                crate::model_render::far_resolved(fm.material_for(lit, false), far_side, far_twins);
            if mat.0 != *want {
                mat.0 = want.clone();
            }
            // A classifier-lit part is still enqueued so the next run re-asserts its full payload
            // (probe slot / fog bit) over whatever this feather episode wrote — 0734's queue.
            if lit.is_some() {
                reauthor.0.push(entity);
            }
        } else if alpha <= 0.0 {
            // First-person: hide outright. Leave tag/material to the classifier (not drawn anyway).
            if *vis != Visibility::Hidden {
                *vis = Visibility::Hidden;
            }
        } else {
            if *vis != Visibility::Inherited {
                *vis = Visibility::Inherited;
            }
            // Feathering: ride the blend twin with the alpha packed into the tag's alpha field
            // (the cutout ignores α; `with_alpha` preserves the ground-shade byte so a shadowed
            // avatar doesn't flash lit while zooming).
            let bits = crate::mesh_tag::with_alpha(tag.0, alpha);
            if tag.0 != bits {
                tag.0 = bits;
            }
            // A bake-classified part feathers on the PROBE-lit blend twin — the room light
            // rides the fade (the tag re-lane keeps the slot alongside the alpha, 0355); the
            // exterior twin at shade byte 0 read as full outdoor intensity deep indoors
            // (director-caught, 2026-07-13). Shared with the appear/despawn ramp since 0755, so
            // the two can never disagree about which twin a law wants.
            let want =
                crate::model_render::far_resolved(fm.material_for(lit, true), far_side, far_twins);
            if mat.0 != *want {
                mat.0 = want.clone();
            }
        }
    }
    if let Ok(children) = children_of.get(entity) {
        for &child in children {
            apply_self_fade_to_descendants(
                child,
                alpha,
                children_of,
                parts,
                far_twins,
                reauthor,
                walked,
            );
        }
    }
}

/// True if the OS pointer is over the world camera's render area. The world camera now fills the window
/// (the debug panel overlays rather than insetting), so this is really just "is the cursor inside the
/// window?"; the panel itself is excluded by `PointerOverUi` at the call site. Kept viewport-aware in
/// case anything insets the camera again.
fn cursor_in_viewport(window: &Window, camera: &Camera) -> bool {
    let Some(cursor) = window.physical_cursor_position() else {
        return false;
    };
    match &camera.viewport {
        Some(vp) => {
            let min = vp.physical_position.as_vec2();
            let max = min + vp.physical_size.as_vec2();
            cursor.x >= min.x && cursor.y >= min.y && cursor.x < max.x && cursor.y < max.y
        }
        None => true,
    }
}

/// Free-fly (pre-connect or `F`-detached): aim from the look angles, move the camera directly —
/// WASD in the camera basis, Space/C up/down, Ctrl 5× boost. The avatar stays frozen where it was;
/// [`super::control`] parks the mover before calling so the wire never extrapolates a phantom walk.
pub(super) fn fly_free(
    dt: f32,
    keys: &ButtonInput<KeyCode>,
    typing: bool,
    rig: &mut CameraControl,
    cam: &mut FlyCam,
    cam_t: &mut Transform,
) {
    let keys_pressed = |k: KeyCode| !typing && keys.pressed(k);
    // Detached / pre-connect: keep the avatar fully opaque (you flew off to look at it — no fade).
    rig.self_fade_alpha = 1.0;
    cam_t.rotation = Quat::from_euler(EulerRot::YXZ, cam.yaw, cam.pitch, 0.0);
    let forward = *cam_t.forward();
    let right = *cam_t.right();
    let mut dir = Vec3::ZERO;
    if keys_pressed(KeyCode::KeyW) {
        dir += forward;
    }
    if keys_pressed(KeyCode::KeyS) {
        dir -= forward;
    }
    if keys_pressed(KeyCode::KeyD) {
        dir += right;
    }
    if keys_pressed(KeyCode::KeyA) {
        dir -= right;
    }
    if keys_pressed(KeyCode::Space) {
        dir += Vec3::Y;
    }
    if keys_pressed(KeyCode::KeyC) {
        dir -= Vec3::Y;
    }
    if dir != Vec3::ZERO {
        let boost = if keys_pressed(KeyCode::ControlLeft) {
            5.0
        } else {
            1.0
        };
        cam_t.translation += dir.normalize() * cam.speed * boost * dt;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_assets::BillboardInfo;
    use benilla_formats::BillboardKind;

    use crate::billboard::BillboardCard;
    use crate::mesh_tag::alpha_bits;

    /// A press that has travelled `yaw`/`pitch` **degrees** of camera rotation.
    fn press(yaw_deg: f32, pitch_deg: f32) -> PressGesture {
        PressGesture {
            at: 0.0,
            yaw_travel: yaw_deg.to_radians(),
            pitch_travel: pitch_deg.to_radians(),
        }
    }

    /// The report this whole change exists for (ledger B226, decision 1122): **a fast click
    /// selects however far the mouse swept.** Under 200 ms the reference asks nothing about
    /// motion at all (`0x514ae0`'s first arm, `0x514b24`) — which is the gesture people actually
    /// make, flicking the cursor at a mob and clicking on arrival with the hand still moving.
    /// benilla used to destroy the pending click after 4 px of travel, so this case never fired.
    #[test]
    fn a_fast_click_selects_however_far_the_mouse_swept() {
        // A whole screen's worth of sweep — orders of magnitude past the travel gate.
        let swept = press(90.0, 45.0);
        assert!(
            swept.is_click(0.199),
            "under 200 ms, travel is not consulted"
        );
        // And the camera is expected to have orbited through all of it: the two are independent,
        // which is the half that makes the reference's gesture possible at all.
    }

    /// The second arm: between 200 and 800 ms the travel gate applies, per axis and independently
    /// (`0x514ae0`'s `both accums < 8.0` arm). The thresholds are the reference's 2.25° of yaw and
    /// 2.0° of pitch — see [`CLICK_HOLD_CEILING`] for why we hold the angle, not the raw literal.
    #[test]
    fn between_the_windows_a_steady_hand_still_clicks_but_a_drag_does_not() {
        assert!(press(2.0, 1.5).is_click(0.5), "inside both travel gates");
        assert!(
            !press(2.3, 1.5).is_click(0.5),
            "yaw alone spends the budget"
        );
        assert!(!press(2.0, 2.1).is_click(0.5), "pitch alone spends it too");
        // Exactly at a threshold is a drag: the reference's compare is `< 8.0`, not `<=`.
        assert!(!press(2.25, 0.0).is_click(0.5), "the yaw gate is exclusive");
        assert!(
            !press(0.0, 2.0).is_click(0.5),
            "the pitch gate is exclusive"
        );
    }

    /// The 800 ms ceiling (`0x514aeb lea eax,[edx-0x320]`) is absolute — a long hold is never a
    /// click, however still the hand was. This is the arm that keeps a deliberate camera orbit from
    /// re-targeting whatever it started on, and it is the reason the fix could not simply be
    /// "always select on release".
    #[test]
    fn a_long_hold_is_never_a_click_however_still() {
        let motionless = press(0.0, 0.0);
        assert!(motionless.is_click(0.799), "just inside the ceiling");
        assert!(!motionless.is_click(0.8), "the ceiling is exclusive");
        assert!(
            !motionless.is_click(5.0),
            "a long motionless hold is a drag"
        );
    }

    /// The self-avatar's zoom-to-first-person fade reaches its BILLBOARD cards — the night-elf eye
    /// glow (ledger B71: two additive quads left burning in mid-air after the body was hidden).
    /// A card is a world ROOT following an anchor inside the model (decision 0153), so the fade's
    /// descendant walk can only claim it through that anchor — and must claim ONLY its own: every
    /// brazier and lamppost in the zone is a card too, and dimming those with the player's zoom
    /// would be a far worse bug than the one being fixed.
    #[test]
    fn self_fade_reaches_the_avatars_billboard_cards_and_no_others() {
        let info = BillboardInfo {
            bone: 0,
            pivot: Vec3::new(0.0, 2.14, 0.0), // the eye-glow bone, head height
            kind: BillboardKind::Spherical,
            scale_anim: None,
            seq_translations: vec![],
        };
        let mut app = App::new();
        app.init_resource::<CameraControl>();
        app.init_resource::<crate::interior::InteriorReauthor>();
        // The water-plane twin map the feather composes with (empty — no water in a fixture).
        app.init_resource::<crate::model_render::FarSideTwins>();
        app.add_systems(Update, apply_self_model_fade);

        // The avatar: root -> joint (the eye-glow bone). Its card follows the joint.
        let avatar = app.world_mut().spawn(SelfPlayer).id();
        let joint = app.world_mut().spawn(Transform::default()).id();
        app.world_mut().entity_mut(avatar).add_child(joint);
        let eye_glow = app
            .world_mut()
            .spawn((
                BillboardCard::following_joint(&info, joint),
                MeshTag(alpha_bits(1.0)),
            ))
            .id();
        // A brazier across the square: same mechanism, another model entirely.
        let brazier_anchor = app.world_mut().spawn(Transform::default()).id();
        let brazier = app
            .world_mut()
            .spawn((
                BillboardCard::following(&info, brazier_anchor),
                MeshTag(alpha_bits(1.0)),
            ))
            .id();

        let tag_of = |app: &App, e: Entity| app.world().entity(e).get::<MeshTag>().unwrap().0;

        // Mid-feather: the glow rides the body's alpha down.
        app.world_mut()
            .resource_mut::<CameraControl>()
            .self_fade_alpha = 0.5;
        app.update();
        assert_eq!(
            tag_of(&app, eye_glow),
            alpha_bits(0.5),
            "the avatar's card feathers with the body"
        );
        assert_eq!(
            tag_of(&app, brazier),
            alpha_bits(1.0),
            "another model's card is untouched by the player's zoom"
        );

        // First person: the additive compose (`out_rgb *= faded_alpha`) takes it to black.
        app.world_mut()
            .resource_mut::<CameraControl>()
            .self_fade_alpha = 0.0;
        app.update();
        assert_eq!(tag_of(&app, eye_glow), alpha_bits(0.0));

        // Back out to third person — the release frame hands the authored value back.
        app.world_mut()
            .resource_mut::<CameraControl>()
            .self_fade_alpha = 1.0;
        app.update();
        assert_eq!(
            tag_of(&app, eye_glow),
            alpha_bits(1.0),
            "the release edge restores the card, like the body parts"
        );
    }
}
