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

use crate::creature_anim::wrap_pi;
use crate::net::Embodied;
use benilla_assets::materials::WowModelMaterial;
use benilla_world::interact::{WorldClick, WorldRightClick, WorldRightPress};
use benilla_world::model_fade::{
    self_model_fade_alpha, FadeMaterials, PendingAppearFade, RenderFade, SELF_FADE_WINDOW,
};
use benilla_world::view::CAM_NEAR;

/// The reference's up-edge click predicate, in **camera degrees and milliseconds** — the whole
/// orbit-vs-select law (decision 1122; wow-re `world-click-drag-arbitration.md`, §5 fan-out
/// 2026-08-08). Verbatim, from `0x514ae0`, which returns 1 = suppress / 0 = dispatch:
///
/// ```text
/// isClick = elapsed < 200ms
///        || (elapsed < 800ms && yaw_travel < 2.25° && pitch_travel < 2.0°)
/// ```
///
/// **The `ms` on those two numbers is now VERIFIED, where 1122 could only infer it** (wow-re
/// `ui/scratch/button-doubleclick-law.md`, 2026-08-11, a side effect of the `OnDoubleClick` §5):
/// the clock both constants are compared against is `0x42c010` → `0x42b790`, whose counter is the
/// `KERNEL32!GetTickCount` import at `[0x7ff310]` and whose scale is stored as `1.0/freq × 1000.0`
/// — milliseconds in either counter mode. Nothing here changes; the units simply stopped being a
/// guess.
///
/// **A press under 200 world selects however far the mouse swept.** That arm is the entire bug report
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
/// [`benilla_world::model_fade::self_model_fade_alpha`]). The out-of-box max is **15** (`15 × 1`) —
/// the reference's, and since 1804 ours. This file shipped the factor fully raised (30 yd) from
/// the day it was written — a taste call ("a wider view") that nobody had weighed against the
/// client it imitates. The slider is still there and still reaches 30; it just is not where a
/// fresh install starts. Our starting zoom
/// is 15 — the reference's own shipped `cameraDistance` is 5.55 (wow-re
/// `camera-settings-persistence.md` §2), a divergence this file has always carried in its own words
/// ("pulled back a bit further than vanilla's own default for a wider view") and one 1804 leaves
/// alone: it is the camera's *initial state*, not a settings row, and it is the director's look.
pub(super) const CAM_DIST_MIN: f32 = 0.0;
/// The reference's `cameraDistanceMax` — the BASE the factor multiplies (registrar default 15).
/// Not exposed: 1.12's panel offers only the factor, so this stays the constant it is there.
pub(super) const CAM_DIST_BASE_MAX: f32 = 15.0;
/// `cameraDistanceMaxFactor`'s slider range — 1.12's own (MAX_FOLLOW_DIST: 1 … 2, step 0.1).
pub(crate) const CAM_DIST_FACTOR_RANGE: std::ops::RangeInclusive<f32> = 1.0..=2.0;
/// The orbit ceiling at the factor slider's **top** — the furthest any saved view or restored
/// camera pose may sit. Not the shipped default any more (1804): [`ZoomLimit::default`] is the
/// slider at rest, `1 × 15`. This is what a distance read back off disk is clamped to, which has
/// to admit the whole slider range rather than only its resting point.
pub(super) const CAM_DIST_MAX: f32 = CAM_DIST_BASE_MAX * 2.0;
pub(super) const CAM_DIST_DEFAULT: f32 = 15.0;

/// The max-orbit knob (decision 1140) — 1.12's `cameraDistanceMaxFactor` over the base above.
/// A fourth frozen constant made reachable: [`CAM_DIST_MAX`] was the only zoom ceiling there was.
///
/// **The default is the reference's 1.0** — `15 yd`, byte-pinned (wow-re
/// `ui/scratch/follow-camera.md`: "cameraDistanceMax 15.0, cameraDistanceMaxFactor 1.0"). It was
/// 2.0 from 1140 until 1804, which is the whole reason that record exists: the raised factor was a
/// reasonable taste call on its day and it was never weighed as a *default*, so benilla shipped a
/// camera that started 15 yd further out than the client it imitates. Raising the slider re-clamps
/// nothing; lowering it re-clamps the live target on the next frame, so the view comes in rather
/// than waiting for the next wheel notch.
#[derive(Resource)]
pub(crate) struct ZoomLimit {
    /// Max orbit distance in yards — `CAM_DIST_BASE_MAX × factor`, hard-capped like the client's 50.
    pub(crate) max: f32,
}

impl Default for ZoomLimit {
    fn default() -> Self {
        // The slider at rest — `cameraDistanceMaxFactor` 1.0 × the 15 yd base, i.e. the reference's
        // own out-of-box ceiling. `CAM_DIST_MAX` is the slider's TOP, and belongs to the clamps.
        Self {
            max: CAM_DIST_BASE_MAX,
        }
    }
}

impl ZoomLimit {
    /// Set from the CVar's factor, clamped to the reference's slider range first.
    pub(crate) fn set_factor(&mut self, factor: f32) {
        let f = factor.clamp(*CAM_DIST_FACTOR_RANGE.start(), *CAM_DIST_FACTOR_RANGE.end());
        self.max = CAM_DIST_BASE_MAX * f;
    }

    /// The live factor — what the CVar table and the config file carry.
    pub(crate) fn factor(&self) -> f32 {
        self.max / CAM_DIST_BASE_MAX
    }
}
/// Yards the wheel moves the target per notch — `CameraZoomIn`/`CameraZoomOut`'s default `amount`
/// (VERIFIED 1.0 in `WoW.exe`).
const CAM_ZOOM_STEP: f32 = 1.0;
/// Camera zoom speed in **yards/second** — `cameraDistanceMoveSpeed` (VERIFIED default 8.33). Vanilla
/// glides the distance toward the wheel target at this *constant velocity* (linear, frame-delta-scaled
/// — `FUN_005112d0` in `WoW.exe`), **not** an exponential ease.
const CAM_MOVE_SPEED: f32 = 8.33;
/// Mouse-look sensitivity at the slider's neutral notch — radians of camera rotation per pixel of
/// mouse motion. [`LookConfig::sensitivity`] scales it; this is the ×1.0 case, and it is what the
/// client felt like before there was a slider at all (decision 1140).
const LOOK_SENSITIVITY: f32 = 0.003;
/// The `mousespeed` slider's range — 1.12's own (UIOptionsFrameSliders' MOUSE_SENSITIVITY row:
/// 0.5 … 1.5, step 0.05). A multiplier over [`LOOK_SENSITIVITY`], so the registered default 1.0
/// reproduces the shipped feel exactly.
pub(crate) const MOUSE_SPEED_RANGE: std::ops::RangeInclusive<f32> = 0.5..=1.5;

/// The mouse-look player knobs (decision 0961): `mouseInvertPitch` is 1.12's own Interface
/// Options checkbox (UIOptionsFrame.lua index 1, CVar-backed), settable from the Options
/// window's Controls page through the CVar store (0954). Inverted, moving the mouse up pitches
/// the camera down — the delta.y term flips sign at the one apply site, both drag styles alike.
///
/// `sensitivity` is 1.12's `mousespeed` slider (1140), the same story one layer down: the rate was
/// a frozen constant with no way to reach it. It multiplies [`LOOK_SENSITIVITY`] at BOTH apply
/// sites — the rotation itself and the click-vs-drag travel budget — because the reference scales
/// the device delta once, upstream of everything that reads it, and a budget measured in unscaled
/// pixels would make a click's drag threshold drift as the slider moved.
#[derive(Resource, Clone, Copy)]
pub(crate) struct LookConfig {
    pub(crate) invert_pitch: bool,
    pub(crate) sensitivity: f32,
}

impl Default for LookConfig {
    fn default() -> Self {
        Self {
            invert_pitch: false,
            sensitivity: 1.0,
        }
    }
}

impl LookConfig {
    /// Radians of camera rotation per pixel of mouse motion, this session. ONE function because
    /// both readers must agree: the look rotation itself and the click-vs-drag travel budget that
    /// decides whether a press was a click. Splitting them would let the slider move the drag
    /// threshold out from under the gesture (decision 1140).
    pub(super) fn rate(self) -> f32 {
        LOOK_SENSITIVITY * self.sensitivity
    }
}
/// The auto-follow's angular rate — 1.12's `cameraYawSmoothSpeed`, registrar default **180 °/s**
/// (`WoW.exe` `[0xbe1070]`, read at `0x512d75`). It is not a slew rate but a *duration* divisor:
/// the transition below lasts `|Δyaw| / rate × factor`, so 180 °/s is the **average** rate at
/// factor 1, and the cosine profile peaks at π/2 × that. 1.12 exposes it as the AUTO_FOLLOW_SPEED
/// slider (`UIOptionsFrameSliders`, 90 … 270 by 10), disabled while the style is Never.
pub(crate) const FOLLOW_SPEED_DEFAULT: f32 = 180.0;
/// The 1.12 slider's own range for [`FollowConfig::yaw_speed`].
pub(crate) const FOLLOW_SPEED_RANGE: std::ops::RangeInclusive<f32> = 90.0..=270.0;
/// `cameraSmoothTimeMin`/`Max` — the transition's duration floor and ceiling, **VERIFIED
/// 0.1 s / 2.0 s** (`[0xbe105c]`/`[0xbe1038]`, clamped at `0x510f4d`). These are why the felt
/// rate is *not* 180 °/s at the ends: a 5° correction still takes 0.1 s, and a lazy `Track`
/// return (factor 10) is capped at 2 s however far it has to come.
const FOLLOW_TIME_MIN: f32 = 0.1;
const FOLLOW_TIME_MAX: f32 = 2.0;
/// The "already there / already arming this" epsilon — the reference's own `0.001`
/// (`[0x801360]`, `0x512ce4`/`0x512d41`). A float-equality guard, not a perceptible deadzone.
const FOLLOW_EPS: f32 = 1.0e-3;

/// **Camera Following Style** — 1.12's `cameraSmoothStyle` (decisions 1493/1502): does the camera
/// return to behind the character on its own?
///
/// benilla shipped the reference behaviour *removed* from the day the camera was written (the
/// orbit offset simply persisted, a director's call) — this is the setting that gives it back, at
/// the reference's own default: **Smart**.
///
/// **The enum is the ENGINE's, `0 = Never · 1 = Smart · 2 = Always`** — byte-verified twice over
/// (wow-re `ui/scratch/camera-smooth-style.md` §2: the registration loop walks
/// `{"Never","Smart","Always"}` filling three blocks in order, and both consumers index by
/// `style × stride`). 1.12's own *dropdown* writes `1/2/3` instead (`UIOptionsFrameCameraDropDown`
/// — Smart 1, Always 2, Never **3**), and 3 is not a style at all: the validator accepts it
/// (`0x50b330(v, 0, 3)`) but the terrain-tilt consumer does not bound-check and indexes 360 bytes
/// past its table. So the reference's shipped UI writes an out-of-range value for Never, and a
/// client must clamp to 0..2. We do: our own dropdown writes the engine's numbers, and a stray
/// `3` is *read* as Never — what whoever wrote it meant.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum FollowStyle {
    /// Never auto-adjust: the orbit offset stays exactly where the hand left it. This is what
    /// benilla did unconditionally before 1493. It does **not** stop the rigid keyboard-turn
    /// carry — in the reference that is a different mechanism entirely (the camera yaw is stored
    /// *relative* to the followed unit's facing), and Never leaves it running.
    Never,
    /// Stay where placed, *except* while the character is being driven — the reference's own
    /// "(Recommended Mode)" and its registrar default.
    #[default]
    Smart,
    /// Always prefer being behind the character: every input edge arms a return, standing still
    /// included.
    Always,
}

impl FollowStyle {
    /// From the CVar's number. `3` is the shipped-UI's Never (see the type doc); anything else off
    /// the ladder reads as the registrar default rather than as a dead camera.
    pub(crate) fn from_cvar(v: f32) -> Self {
        match v as i32 {
            0 => Self::Never,
            2 => Self::Always,
            // The 1.12 dropdown's own Never. Out of range for the engine's tables — accepted here
            // because a config or addon carrying it means Never, not "surprise me".
            3 => Self::Never,
            _ => Self::Smart,
        }
    }

    /// The CVar string this style is — the value the table and `config.toml` carry.
    pub(crate) fn cvar(self) -> &'static str {
        match self {
            Self::Never => "0",
            Self::Smart => "1",
            Self::Always => "2",
        }
    }

    /// The `cameraSmooth<Style><State>{Delay,Factor}` row — family A at `[0xbe0e70]`, dumped at
    /// its defaults in wow-re `camera-smooth-style.md` §3. Factor `0` means *cancel*: the armed
    /// transition is dropped and the camera keeps the offset it has.
    ///
    /// Family B (`cameraSmoothViewData<Style>Yaw{Delay,Factor}`) multiplies in: its Yaw factor is
    /// `1.0` under Smart and Always and `0.0` under Never, and its delay is `0.0` at every style —
    /// so for the yaw channel the composition is the identity and the table below is the answer.
    fn row(self, state: FollowState) -> (f32, f32) {
        match self {
            // Every row is 0/0 — and family B's Never Yaw factor is 0.0 as well, twice over.
            Self::Never => (0.0, 0.0),
            Self::Smart => match state {
                // The two states that make Smart *smart*: nothing returns while you stand or stop.
                FollowState::Idle | FollowState::Stop => (0.0, 0.0),
                // Driven from outside (a taxi, a spline, a fear): a lazy return — 0.4 s of delay
                // and factor 10, which is 18 °/s of average rate before the 2 s cap bites.
                FollowState::Track | FollowState::Fear => (0.4, 10.0),
                FollowState::Move | FollowState::Strafe | FollowState::Turn => (0.0, 1.0),
            },
            // Always is factor 1.0 in every state, Idle and Stop included.
            Self::Always => (0.0, 1.0),
        }
    }
}

/// The seven arming states of 1.12's auto-return classifier (`0x510960`, wow-re
/// `camera-smooth-style.md` §6.2), **in the reference's own priority order — highest first**. The
/// winner is the highest-priority bit set, and the states are read off the *camera's* input
/// command word, not off the character's velocity: right-mouse alone is a `Turn`, a turn key
/// under right-mouse is a `Strafe`, and both mouse buttons are a `Move`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum FollowState {
    /// Driven by something that is not us and is not a spline — the reference's external-control
    /// flag `[cam+0x90] & 0x1000`.
    Fear,
    Turn,
    Strafe,
    Move,
    /// Externally-driven movement (taxi / server spline) — `[cam+0x90] & 0x100`.
    Track,
    /// This edge released a movement input (the reference's `stopping` argument). Carries the same
    /// `(0, 0)` / `(0, 1)` rows as [`FollowState::Idle`] at every shipped style, so the two are
    /// currently indistinguishable in behaviour — kept apart because the *matrix* keeps them apart.
    Stop,
    Idle,
}

/// The camera's input command word — 1.12's `[InputControl+0x4]` bit for bit (wow-re
/// `camera-smooth-style.md` §6.1), because the state classifier and the *edge* that arms a
/// transition are both defined on it. benilla has no PitchUp/PitchDown bindings, so those two bits
/// are simply never set.
pub(super) mod follow_cmd {
    /// `TurnOrAction` — right mouse / mouselook.
    pub(in crate::player) const RIGHT_MOUSE: u32 = 0x1;
    /// `CameraOrSelectOrMove` — left mouse.
    pub(in crate::player) const LEFT_MOUSE: u32 = 0x2;
    pub(in crate::player) const FORWARD: u32 = 0x10;
    pub(in crate::player) const BACKWARD: u32 = 0x20;
    pub(in crate::player) const STRAFE_LEFT: u32 = 0x40;
    pub(in crate::player) const STRAFE_RIGHT: u32 = 0x80;
    pub(in crate::player) const TURN_LEFT: u32 = 0x100;
    pub(in crate::player) const TURN_RIGHT: u32 = 0x200;
    pub(in crate::player) const AUTORUN: u32 = 0x1000;
    /// The union of bits 20/21/23, which the reference folds into the camera's `Track` flag —
    /// externally-driven movement.
    pub(in crate::player) const TRACK: u32 = 0x100000;
    /// External control (the reference's own `[cam+0x90] & 0x1000`, not an InputControl bit —
    /// carried here so one word carries every edge the arming function reacts to).
    pub(in crate::player) const FEAR: u32 = 0x2000_0000;

    /// The bits `Move` reads: forward, backward, autorun.
    pub(in crate::player) const MOVE_BITS: u32 = FORWARD | BACKWARD | AUTORUN;
    pub(in crate::player) const STRAFE_BITS: u32 = STRAFE_LEFT | STRAFE_RIGHT;
    pub(in crate::player) const TURN_BITS: u32 = TURN_LEFT | TURN_RIGHT;
}

/// The knobs behind the auto-follow (decision 1502) — the style, the style the *externally-driven*
/// states use instead, and the rate. All three are 1.12 CVars with the reference's own defaults.
#[derive(Resource, Clone, Copy, PartialEq, Debug)]
pub(crate) struct FollowConfig {
    /// `cameraSmoothStyle`.
    pub(crate) style: FollowStyle,
    /// `cameraSmoothTrackingStyle` — the selector the reference swaps in when the state mask so
    /// much as *contains* `Track` or `Fear` (`0x510a51 test bl,0x44`, before the priority scan),
    /// indexing the very same matrices. Registrar default `"1"` = Smart, like its sibling.
    pub(crate) tracking_style: FollowStyle,
    /// `cameraYawSmoothSpeed`, °/s — see [`FOLLOW_SPEED_DEFAULT`].
    pub(crate) yaw_speed: f32,
}

impl Default for FollowConfig {
    fn default() -> Self {
        Self {
            style: FollowStyle::default(),
            tracking_style: FollowStyle::default(),
            yaw_speed: FOLLOW_SPEED_DEFAULT,
        }
    }
}

/// What [`seat_camera`] needs to run the auto-follow this frame: the knobs, where "behind" is, and
/// the input word whose *edges* arm a return. Bundled because they are one question, and
/// `seat_camera` is already at its argument ceiling.
pub(super) struct FollowInput {
    pub(super) cfg: FollowConfig,
    /// The character's own facing (same yaw convention as [`FlyCam::yaw`] — a right-drag couples
    /// the two directly), so the camera's *offset* is `cam.yaw − face_yaw`.
    pub(super) face_yaw: f32,
    /// This frame's [`follow_cmd`] word.
    pub(super) command: u32,
}

impl FollowInput {
    /// The winning state — the reference's descending priority scan, highest bit first.
    fn state(&self, stopping: bool) -> FollowState {
        let mf = self.command;
        let held = |bits: u32| mf & bits != 0;
        if held(follow_cmd::FEAR) {
            FollowState::Fear
        } else if held(follow_cmd::TURN_BITS) || held(follow_cmd::RIGHT_MOUSE) {
            FollowState::Turn
        } else if held(follow_cmd::STRAFE_BITS)
            || (held(follow_cmd::RIGHT_MOUSE) && held(follow_cmd::TURN_BITS))
        {
            FollowState::Strafe
        } else if held(follow_cmd::MOVE_BITS)
            || (held(follow_cmd::RIGHT_MOUSE) && held(follow_cmd::LEFT_MOUSE))
        {
            FollowState::Move
        } else if held(follow_cmd::TRACK) {
            FollowState::Track
        } else if stopping {
            FollowState::Stop
        } else {
            FollowState::Idle
        }
    }

    /// Which style picks the row: the *tracking* style whenever the mask so much as contains
    /// `Track` or `Fear`, even when a higher-priority state supplies the row.
    fn style(&self) -> FollowStyle {
        if self.command & (follow_cmd::TRACK | follow_cmd::FEAR) != 0 {
            self.cfg.tracking_style
        } else {
            self.cfg.style
        }
    }
}

/// The armed yaw transition — the reference's descriptor `[+0x208 startMs, +0x20c dur,
/// +0x210 target, +0x214 start]`, in seconds and in *offset* space.
#[derive(Clone, Copy, Debug)]
struct FollowArm {
    /// The offset the transition started from, radians (camera yaw minus character facing).
    from: f32,
    /// Where it is going. `0.0` — directly behind — at the shipped defaults, because
    /// `cameraYawSmoothMin`/`Max` are both `0.0` and the reference *substitutes* the crossed bound
    /// for the saved view yaw whenever the live offset is outside the band.
    to: f32,
    /// Seconds the move takes, already clamped to `[FOLLOW_TIME_MIN, FOLLOW_TIME_MAX]`.
    dur: f32,
    /// Seconds of dead time before it starts (`Track`/`Fear` under Smart: 0.4).
    delay: f32,
    /// Seconds since it was armed.
    elapsed: f32,
    /// What it was armed with — the reference's re-arm memo (`[+0x218, +0x21c]`), so a repeated
    /// edge asking for the same transition is a no-op instead of restarting the swing.
    armed_with: (f32, f32),
}

/// The auto-follow's own state on the rig: the input word we last saw (edges are what arm a
/// return) and the transition in flight, if any.
#[derive(Default)]
pub(super) struct FollowRig {
    last_command: Option<u32>,
    arm: Option<FollowArm>,
}

impl FollowRig {
    /// Run the auto-follow for a frame and return the camera yaw it wants, if it wants one.
    ///
    /// The shape is the reference's, and the shape is the point (wow-re `camera-smooth-style.md`
    /// §6/§8): a transition is **armed on an input edge**, from a snapshot taken at that instant,
    /// and then plays out unattended — it is *not* a per-frame chase of a moving target. That is
    /// why "drag the camera aside, then press W" swings you back over one smooth arc, while
    /// holding W changes nothing at all.
    fn advance(
        &mut self,
        input: &FollowInput,
        cam_yaw: f32,
        dt: f32,
        look_held: bool,
    ) -> Option<f32> {
        let word = input.command;
        let previous = self.last_command.replace(word);
        // A held drag owns the camera: the yaw channel is frozen (`0x50f623`), arming is gated
        // (`0x510850`'s `!([cam+0x90] & 1)`), and **entering** mouse-look cancels whatever was in
        // flight outright (`0x50fe30` zeroes the descriptors). So the return does not resume when
        // the button comes up — it begins at the next input edge after the release, which is
        // usually the release itself (the mouse bits are part of the word).
        if look_held {
            self.arm = None;
            return None;
        }
        // The EDGE: any movement/camera binding changing state re-evaluates the transition. The
        // reference re-evaluates on the binding call itself, not on a change of the classified
        // state, which is why releasing the drag while standing still is an Idle *arming* under
        // Always and an Idle *cancel* under Smart.
        if let Some(p) = previous.filter(|p| *p != word) {
            // `stopping` — the reference's second argument, set by the Stop half of a movement
            // binding. Our equivalent is the edge itself: a movement bit went away.
            let stopping = (p & !word)
                & (follow_cmd::MOVE_BITS | follow_cmd::STRAFE_BITS | follow_cmd::TURN_BITS)
                != 0;
            self.arm(input, cam_yaw, stopping);
        }
        let arm = self.arm.as_mut()?;
        arm.elapsed += dt;
        let t = arm.elapsed - arm.delay;
        if t < 0.0 {
            return None; // still inside the delay window
        }
        let s = t / arm.dur;
        let offset = if s >= 1.0 {
            let to = arm.to;
            self.arm = None;
            to
        } else {
            // The reference's kernel `0x5b7bb0` — a **cosine** smoothstep, eased at both ends:
            // `a + (b − a)·(1 − cos(πs))/2`.
            let e = (1.0 - (std::f32::consts::PI * s).cos()) * 0.5;
            arm.from + (arm.to - arm.from) * e
        };
        Some(wrap_pi(input.face_yaw + offset))
    }

    /// What the transition is doing, for `WOW_CAM_DUMP` — `None` when nothing is armed, else
    /// `(elapsed, delay, duration)` in seconds. The auto-follow is the one camera behaviour with
    /// no headless retest (nothing synthesizes a mouse drag), so the trace line is how a run says
    /// what it armed and when.
    fn probe(&self) -> Option<(f32, f32, f32)> {
        self.arm.map(|a| (a.elapsed, a.delay, a.dur))
    }

    /// The arming half (`0x510960` → `0x512c70`): pick the row, then either cancel outright or
    /// snapshot a transition to the target offset.
    fn arm(&mut self, input: &FollowInput, cam_yaw: f32, stopping: bool) {
        let (delay, factor) = input.style().row(input.state(stopping));
        if factor == 0.0 {
            // Cancel: the target becomes the live yaw and the channel disarms — the camera simply
            // keeps the offset it has. This is Smart standing still, and it is all of Never.
            self.arm = None;
            return;
        }
        // The target: directly behind, at the shipped defaults (see [`FollowArm::to`]).
        let to = 0.0;
        let from = wrap_pi(cam_yaw - input.face_yaw);
        let gap = (to - from).abs();
        if gap < FOLLOW_EPS {
            return; // already there — the reference returns without arming
        }
        // The re-arm memo: an edge asking for the transition already in flight is a no-op, so a
        // second keypress mid-swing does not restart it from the current angle.
        if self.arm.is_some_and(|a| {
            a.to == to
                && (a.armed_with.0 - delay).abs() < FOLLOW_EPS
                && (a.armed_with.1 - factor).abs() < FOLLOW_EPS
        }) {
            return;
        }
        let rate = input.cfg.yaw_speed.to_radians().max(FOLLOW_EPS);
        let dur = (gap / rate * factor).clamp(FOLLOW_TIME_MIN, FOLLOW_TIME_MAX);
        self.arm = Some(FollowArm {
            from,
            to,
            dur,
            delay,
            elapsed: 0.0,
            armed_with: (delay, factor),
        });
    }
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
/// Floor (yd) on the world pivot height — VERIFIED `5/6` (`0x50ca90`'s per-preset clamp, and
/// `0x50e570`'s corridor lower bound).
pub(super) const CAM_PIVOT_FLOOR: f32 = 5.0 / 6.0;
/// Ceiling (yd) on the world pivot height — VERIFIED `15.0` (`[0x8089c8]`, the upper arm of the same
/// per-preset clamp in `0x50ca90`). A giant's scale cannot walk the framing pivot off into the sky.
pub(super) const CAM_PIVOT_CEIL: f32 = 15.0;
/// Pivot height used before the avatar model has attached (so `CameraPivot` isn't on the entity yet):
/// a human's ~neck height, so the first frames of third-person don't ride high. Replaced by the exact
/// model-derived value the moment the body attaches — as a **snap**, not a glide ([`PivotGlide`]).
pub(super) const CAM_PIVOT_FALLBACK: f32 = 1.8;

/// One modeled unit's world head height: its model-local [`CameraPivot`] × the given scale, clamped
/// to `[CAM_PIVOT_FLOOR, CAM_PIVOT_CEIL]` — the reference's per-preset clamp in `0x50ca90`.
pub(super) fn model_pivot_height(pivot: &CameraPivot, scale: f32) -> f32 {
    (pivot.height_local * scale).clamp(CAM_PIVOT_FLOOR, CAM_PIVOT_CEIL)
}

/// World head height above a modeled unit's feet — [`model_pivot_height`], or the neck-height
/// [`CAM_PIVOT_FALLBACK`] when the body has no model yet. The single definition shared by the things
/// that sit at the character's head: the framing-pivot *target*, the far-sight subject's pivot, and
/// the 3D-audio listener (the client's `SoundListenerAtCharacter=1` default, wow-re benilla-pins B14).
///
/// **Which `scale` to pass is a fidelity question with a verified answer** (wow-re
/// `pivot-height-glide.md`, C3): the reference's pivot preset multiplies the **raw**
/// `OBJECT_FIELD_SCALE_X` descriptor (vtable slot 7 = `0x469f10`, `fld [descriptors+0x10]`), *not*
/// the 2 s-eased render scale — the two are deliberately split in the binary (`0x4833d3` folds the
/// eased one in for a selection-ring consumer, and only there). So the camera passes
/// [`crate::net::NetEntity::scale`] and the pivot moves in **one** step per model event, which
/// [`PivotGlide`] then walks; multiplying by the eased scale instead would stack a second, slower
/// ease on top of the first and is what made a shapeshift snap *and* drift. The audio listener still
/// passes the rendered scale — it tracks the drawn body, and nothing verified says otherwise.
pub(crate) fn head_height(pivot: Option<&CameraPivot>, scale: f32) -> f32 {
    pivot.map_or(CAM_PIVOT_FALLBACK, |p| model_pivot_height(p, scale))
}

/// `cameraHeightSmoothSpeed` (yd/s) — the pivot channel's rate, VERIFIED registrar default `"1.2"`.
/// The duration of a move is `|Δh| / this` (`0x51276c`/`0x512777`), so it is an *average* rate: the
/// cosine profile peaks at `π/2 ×` it in the middle and is zero at both ends. There is no duration
/// clamp on this channel (unlike the yaw channel's `[0.1 s, 2.0 s]`).
const CAM_PIVOT_SMOOTH_SPEED: f32 = 1.2;
/// The pivot setter's "already there / already arming this" epsilon — VERIFIED `0.001` (`[0x801360]`,
/// `0x5126b0`). It is what makes the per-frame re-arm a no-op in steady state.
const CAM_PIVOT_EPS: f32 = 0.001;

/// **The camera's pivot-height channel** — the height the framing pivot actually rides, chasing the
/// model-derived target with a cosine smoothstep instead of taking it raw.
///
/// The reference's live `cam+0xfc` chasing target `cam+0x1c8`: armed by `0x5126b0` → `0x512790`,
/// stepped by `0x50f160`'s `[0x50f36a, 0x50f417)` block (wow-re `pivot-height-glide.md`, §5 round).
/// **This is why a druid shapeshift does not snap the reference's camera**, and it glides in *both*
/// directions: the solver's `max(target, live)` (`0x50e5a9`) is only the collision-corridor seed, and
/// the far chain clamps the result back down to the live value (`0x50e767`), so an unobstructed pivot
/// simply *is* `cam+0xfc` — rising or falling.
///
/// Two structural facts, both load-bearing:
/// - **The first arm of a camera's life snaps** (`0x5127d4`; the latch bit `0x80` is never cleared —
///   image-wide census). So logging in establishes the height instantly, and everything after it
///   glides. Nothing else re-snaps: a target-GUID change, a mount, a morph, `SetView` — all glide.
/// - **A model that has not resolved yet holds the channel**, it does not re-aim it (the reference
///   skips the whole camera update while the preset is stale, `0x50e907`). Ours is the `None` target:
///   during the frames a swapped-in model is loading, the pivot stays where it is and one glide runs
///   when the new height lands.
pub(super) struct PivotGlide {
    /// The live height — what the camera uses (`cam+0xfc`).
    live: f32,
    /// Where the move started (`cam+0x1cc`) and where it is going (`cam+0x1c8`).
    from: f32,
    to: f32,
    /// Seconds since arming, and the move's total (`cam+0x1c0`/`+0x1c4`); `None` = nothing in
    /// flight (the reference's armed bit `[cam+0x90] & 0x20000000`).
    flight: Option<(f32, f32)>,
    /// Has the channel ever been armed? The reference's latch bit `0x80` — false only until the
    /// first model-derived height arrives, which is therefore a snap.
    seeded: bool,
}

impl Default for PivotGlide {
    fn default() -> Self {
        Self {
            live: CAM_PIVOT_FALLBACK,
            from: CAM_PIVOT_FALLBACK,
            to: CAM_PIVOT_FALLBACK,
            flight: None,
            seeded: false,
        }
    }
}

impl PivotGlide {
    /// Arm the channel with this frame's model-derived target (`None` while the subject has no
    /// model — hold), step whatever is in flight, and return the height to frame at.
    ///
    /// Called every frame, which is the reference's own cadence (`0x50f880` from the driver tail
    /// `0x50f011`): the epsilon tests below turn a steady target into a no-op, so "arm per frame"
    /// and "arm on change" are the same thing except at the instant the target actually moves.
    pub(super) fn advance(&mut self, target: Option<f32>, dt: f32) -> f32 {
        if let Some(target) = target {
            self.arm(target);
        }
        if let Some((elapsed, dur)) = self.flight.as_mut() {
            *elapsed += dt;
            let s = *elapsed / *dur;
            if s >= 1.0 {
                self.live = self.to;
                self.flight = None;
            } else {
                // The reference's kernel `0x5b7bb0` — the same cosine smoothstep the yaw channel
                // and the render-scale ease use: `a + (b − a)·(1 − cos(πs))/2`.
                let e = (1.0 - (std::f32::consts::PI * s).cos()) * 0.5;
                self.live = self.from + (self.to - self.from) * e;
            }
        }
        self.live
    }

    /// The arming half (`0x5126b0` → `0x512790`), in its own order: the re-arm memo, the
    /// already-there test, then the duration — and the once-per-camera snap.
    fn arm(&mut self, target: f32) {
        if !self.seeded {
            // The latch: the first height a camera ever sees is established, not travelled to.
            self.seeded = true;
            self.live = target;
            self.to = target;
            self.from = target;
            self.flight = None;
            return;
        }
        // Already arming exactly this — a no-op, so a per-frame re-arm cannot restart the move
        // from its own midpoint (which would stretch it forever, asymptotically never arriving).
        if self.flight.is_some() && (self.to - target).abs() < CAM_PIVOT_EPS {
            return;
        }
        if (self.live - target).abs() < CAM_PIVOT_EPS {
            // Already there: park the target and disarm. The steady-state path, every frame.
            self.to = target;
            self.flight = None;
            return;
        }
        self.from = self.live;
        self.to = target;
        self.flight = Some((0.0, (target - self.live).abs() / CAM_PIVOT_SMOOTH_SPEED));
    }

    /// What the channel is doing, for `WOW_CAM_DUMP`: `(live, target)`. A pivot question is a
    /// *timing* question — "does it snap?" is answered by these two columns on a trace, never by
    /// watching a capture (method: timing is measured, never eyeballed).
    pub(super) fn probe(&self) -> (f32, f32) {
        (self.live, self.to)
    }
}

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
    /// ([`benilla_world::model_fade::self_model_fade_alpha`]): `1.0` third-person (opaque), ramping to `0.0` as
    /// the camera zooms into the head (first-person). `control` computes it (it owns the pivot + camera
    /// pose); [`apply_self_model_fade`] applies it to the body parts. Starts opaque.
    pub(super) self_fade_alpha: f32,
    /// The auto-follow's own state (decision 1502): the input word we last saw, and the armed
    /// return in flight. It lives on the rig rather than beside the knob because it is *pose*, not
    /// setting — a transition survives the frame, not the session.
    pub(super) follow: FollowRig,
    /// The framing pivot's height channel — smoothed, not taken raw ([`PivotGlide`]). On the rig
    /// for the same reason `follow` is, and for one more: it belongs to the **camera**, not to the
    /// body, which is why it glides *through* a change of subject (a shapeshift, a far-sight
    /// switch) instead of being reset by one.
    pub(super) pivot: PivotGlide,
}

impl CameraControl {
    /// Park the orbit distance at `d` — **both** the live value and the wheel target.
    ///
    /// Both, or the wheel glide eases `distance` back toward the old target every frame and a
    /// parked shot drifts through the whole zoom while the burst is running.
    ///
    /// The scripted camera park (`capture::probe_cam`, decision 0653) is the only caller. It gets a
    /// named method rather than `pub(crate)` fields because an instrument reaching into gameplay is
    /// the allowed direction but not a licence to open gameplay's internals to the whole crate
    /// (decision 1174) — this is the entire surface the probe needs.
    pub(crate) fn park_distance(&mut self, d: f32) {
        self.distance = d;
        self.target_distance = d;
    }

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
// `pub(crate)` on the TYPE only — the scripted camera park has to name it in a query. Its fields
// stay `pub(super)`; [`FlyCam::park`] is the whole surface an instrument gets (decision 1174).
pub(crate) struct FlyCam {
    pub(super) yaw: f32,
    pub(super) pitch: f32,
    pub(super) speed: f32,
}

impl FlyCam {
    /// Point the rig at an absolute world yaw/pitch — the scripted camera park's one lever
    /// (`capture::probe_cam`, decision 0653). From here on this is the identical path a mouse-drag
    /// takes.
    pub(crate) fn park(&mut self, yaw: f32, pitch: f32) {
        self.yaw = yaw;
        self.pitch = pitch;
    }
}

/// The per-model camera-pivot height in **model-local yards, pre-scale** — `attach17.z + 0.0972` (M2
/// attachment id 17) for a character, else `0.9 × vertex-box Z-extent`; the reference's camera-target
/// height (`0x50cbc0`, wow-re `follow-camera`). Stamped on every modeled unit at attach
/// ([`crate::entities`]); `control` reads it off the [`Embodied`], multiplies that body's live scale,
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
    look_cfg: LookConfig,
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
    let rate = look_cfg.rate();
    let dyaw = (mouse_motion.delta.x * rate).abs();
    let dpitch = (mouse_motion.delta.y * rate).abs();
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
        cam.yaw -= delta.x * rate;
        // `mouseInvertPitch` flips only the pitch axis (the 1.12 checkbox's whole meaning).
        let dy = if look_cfg.invert_pitch {
            -delta.y
        } else {
            delta.y
        };
        cam.pitch = (cam.pitch - dy * rate).clamp(-CAM_PITCH_LIMIT, CAM_PITCH_LIMIT);
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
pub(super) fn apply_zoom_scroll(scroll: f32, dt: f32, rig: &mut CameraControl, max: f32) {
    if scroll != 0.0 {
        rig.target_distance =
            (rig.target_distance - scroll * CAM_ZOOM_STEP).clamp(CAM_DIST_MIN, max);
    }
    // Re-clamp every frame, not just on a notch: lowering the Max Camera Distance slider has to
    // pull a camera already sitting past the new ceiling back in, and the glide below then eases
    // it there at the same yd/s a wheel notch would.
    rig.target_distance = rig.target_distance.min(max);
    // Glide the actual distance toward the wheel target at a constant `cameraDistanceMoveSpeed` yd/s,
    // stopping exactly there — the verified vanilla behavior (linear, frame-delta-scaled; not an ease).
    let max_step = CAM_MOVE_SPEED * dt;
    rig.distance += (rig.target_distance - rig.distance).clamp(-max_step, max_step);
}

/// Seat the camera on **whatever the rig is orbiting this frame** — our own body, or a far-sight
/// subject (B151: Mind Vision, Sentry Totem, and Mind Control's camera half in B211) while
/// `PLAYER_FARSIGHT` names one. Three substitutions and then [`seat_camera`]: the orbit centre,
/// the collision sweep's origin, and the pivot height's target.
///
/// It exists because the controller seats the camera from **two** places — the ordinary driving
/// path, and the stand-down path where a spline, a possession or a reseat window owns the body —
/// and far sight outlives all of those (Sentry Totem carries no interrupt flags at all, so you can
/// board a taxi with your view still on the totem). Two copies of the substitution is how one of
/// them silently stops honouring it.
///
/// `feet`/`head` are the caller's, because the head offset is the avatar capsule's and those
/// constants are a movement concern; `body_pivot` is the target height read off the driven body
/// this frame, used only when nothing else is being watched.
#[allow(clippy::too_many_arguments)]
pub(super) fn seat_on_subject(
    dt: f32,
    turn_delta: f32,
    feet: Vec3,
    head: Vec3,
    body_pivot: Option<f32>,
    view: &super::view_subject::ViewSubject,
    rig: &mut CameraControl,
    cam: &mut FlyCam,
    cam_t: &mut Mut<Transform>,
    collide: &benilla_world::collision::WorldCollision<'_, '_>,
    cam_probe: &Collider,
    follow: &FollowInput,
) {
    // The sweep origin moves with the subject too; rooting it at our own head would cast the boom
    // across the world and jam it on the first wall in between
    // ([`super::view_subject::RemoteView::sweep_origin`]).
    let (orbit_pos, sweep_from) = match view.remote {
        Some(v) => (v.feet, v.sweep_origin()),
        None => (feet, head),
    };
    // The framing height is the **channel's**, not this frame's target: it eases there over
    // `|Δh| / 1.2` s with a cosine profile, so a shapeshift, a mount, a growth aura or a far-sight
    // switch move the camera smoothly instead of teleporting it ([`PivotGlide`]; wow-re
    // `pivot-height-glide.md`). A far-sight subject supplies the target the same way the body
    // does — one channel, whatever it is looking at.
    let orbit_pivot = rig
        .pivot
        .advance(view.remote.map(|v| v.pivot_height).or(body_pivot), dt);
    seat_camera(
        dt,
        turn_delta,
        orbit_pos,
        sweep_from,
        orbit_pivot,
        rig,
        cam,
        cam_t,
        collide,
        cam_probe,
        follow,
    );
}

/// Seat the third-person camera: orient it, orbit it behind the avatar's torso with a collision
/// sweep from the head to the ideal seat (snap-in instantly, ease back out), write the resulting
/// transform, and compute the self-avatar zoom-in fade from the realized camera-to-pivot distance.
/// A **keyboard** turn (or the drunk veer, which rides `turn_delta` the same way — decision 1018)
/// carries the camera rigidly — the character's own turns only: a transport
/// deck turning under the rider is frame motion and is applied to `cam.yaw` at the ride block in
/// [`super::control`], bypassing this function's look-session gate (routing it here was the
/// right-drag drift bug — the gate ate the deck's share while a drag was held). A left-drag orbit
/// offset is then reeled back in by the **auto-follow**, on the player's `cameraSmoothStyle`
/// setting ([`FollowStyle`], decision 1493) — or kept forever, on Never.
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
    cam_t: &mut Mut<Transform>,
    collide: &benilla_world::collision::WorldCollision<'_, '_>,
    cam_probe: &Collider,
    follow: &FollowInput,
) {
    // A keyboard turn carries the camera RIGIDLY (char and camera rotate as one — the reference
    // look, director's call closing 0050's open "camera follow on turn"): an eased chase of a
    // continuously-turning facing lags by ω/rate, which read as the char angled on screen while
    // run-turning and a release-snap landing off-camera. A drag (`rig.look` held) owns the camera
    // — no INPUT-turn carry against the user's hand. (A transport deck's turn is not an input and
    // never arrives here — the ride block applies it to `cam.yaw` directly, drag or no drag.)
    //
    // **The auto-follow** (1.12's `cameraSmoothStyle`, decisions 1493/1502) rides the same gate
    // for the same reason — a held drag owns the camera, hand on it. It is NOT a per-frame chase:
    // an input edge arms a cosine-smoothstep return to directly-behind and that transition then
    // plays out unattended ([`FollowRig::advance`]). It writes an absolute yaw because our camera
    // stores one; the reference stores the *offset* and re-adds the facing at render time, which
    // is the same picture and a different mechanism (wow-re `camera-smooth-style.md` §10 — and
    // the reason Never must not, and here does not, touch the rigid carry above).
    let look_held = rig.look.is_some();
    if !look_held {
        cam.yaw += turn_delta;
    }
    if let Some(yaw) = rig.follow.advance(follow, cam.yaw, dt, look_held) {
        cam.yaw = yaw;
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
    let rotation = Quat::from_euler(EulerRot::YXZ, cam.yaw, cam.pitch, 0.0);
    // `Transform::forward()` is exactly `rotation * -Z` (no renormalize), computed here from the
    // local so the write below can be gated.
    let cam_fwd = rotation * Vec3::NEG_Z;
    let pivot = player_pos + Vec3::Y * cam_pivot_height;
    let seat = pivot - cam_fwd * rig.distance;
    let boom = seat - head;
    let boom_len = boom.length().max(1.0e-3);
    // The camera collides with the WMO *camera/LOS* faces (keeps DETAIL overhangs like forge pipes,
    // drops NOCAMCOLLIDE) + terrain/doodads/GameObjects — its own audience, not the walking mesh.
    let open = collide
        .cast_camera(cam_probe, head, Quat::IDENTITY, boom, 0.0)
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
    let translation = head + boom * frac;
    // The no-op write gate (decision 1362 — 1355's clamp lesson, at the camera): a parked
    // camera's pose is bit-stable once the collision ease settles, but writing it anyway marked
    // the camera's transform changed every frame — which re-ran its propagation and told every
    // camera-watching gate in the app that the view moved when it hadn't. Bit equality, not an
    // epsilon: a real sub-epsilon drift must still land.
    {
        let t = cam_t.bypass_change_detection();
        if t.rotation != rotation || t.translation != translation {
            t.rotation = rotation;
            t.translation = translation;
            cam_t.set_changed();
        }
    }
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
        // `follow=` is the auto-follow's own reading (1502): the offset the return is animating,
        // the state the input word classifies to, and — once armed — how far through the
        // transition this frame is. `off` moving while `arm` reads `-` means something other than
        // the follow moved the camera.
        let (elapsed, delay, dur) = rig.follow.probe().unwrap_or((-1.0, -1.0, -1.0));
        eprintln!(
            "[cam] yaw {:.6} pitch {:.6} dist {:.6} open {:.6} coll {:.6} frac {:.6} \
             pos [{:.6},{:.6},{:.6}] bits [{:08x},{:08x},{:08x}] \
             follow off {:.6} state {:?} word {:06x} arm {:.3}/{:.3}+{:.3}",
            cam.yaw,
            cam.pitch,
            rig.distance,
            open,
            rig.collision_distance,
            frac,
            translation.x,
            translation.y,
            translation.z,
            translation.x.to_bits(),
            translation.y.to_bits(),
            translation.z.to_bits(),
            wrap_pi(cam.yaw - follow.face_yaw),
            follow.state(false),
            follow.command,
            elapsed,
            dur,
            delay,
        );
    }

    // Fade the avatar as the camera nears its pivot (zoom-in / a wall pulling the boom in): opaque
    // in third-person, ramping to invisible in first-person. Keyed off the *realized* camera→pivot
    // distance (collision-pulled), so backing into a wall also thins you — the faithful behavior.
    rig.self_fade_alpha =
        self_model_fade_alpha((translation - pivot).length(), CAM_NEAR, SELF_FADE_WINDOW);
}

/// Apply the self-avatar zoom-in fade ([`CameraControl::self_fade_alpha`], computed in [`control`]) to
/// the player's own body parts **and every attach-model descendant** (held items, helm, shoulders —
/// [`crate::entities::BoneAttach`] rides them several levels down through the joint hierarchy), so you
/// go translucent then invisible — weapon and armor included — as the camera zooms into the head. Drives
/// the same per-instance render-alpha channel as [`benilla_world::model_fade::apply_render_fade`] — the `MeshTag`
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
    self_player: Query<(Entity, Option<&crate::aura_visual::AuraNodes>), With<Embodied>>,
    children_of: Query<&Children>,
    mut parts: Query<
        (
            &FadeMaterials,
            &mut MeshTag,
            &mut MeshMaterial3d<WowModelMaterial>,
            &mut Visibility,
            Option<&benilla_world::interior::InteriorLit>,
            Has<benilla_world::model_render::FarSideOfWater>,
        ),
        (
            Without<RenderFade>,
            Without<PendingAppearFade>,
            // Disjointness for the card query below (both want `&mut MeshTag`). A card carries
            // `FadeMaterials` too since 0836, so this now genuinely diverts them — into the loop
            // at the end, which applies the same law without touching `Visibility` (a card's own
            // hidden-owner mirror authors that in a different system).
            Without<benilla_world::billboard::BillboardCard>,
        ),
    >,
    mut cards: Query<(
        &benilla_world::billboard::BillboardCard,
        &mut MeshTag,
        Option<&benilla_world::doodad_anim::MatAnim>,
        Option<&FadeMaterials>,
        Option<&mut MeshMaterial3d<WowModelMaterial>>,
        Option<&benilla_world::interior::InteriorLit>,
        Has<benilla_world::model_render::FarSideOfWater>,
    )>,
    // The water-plane axis, composed into every pick below (`far_resolved`) like every other
    // owner of the handle channel — the feather and the classifier converge, never re-swap.
    far_twins: Res<benilla_world::model_render::FarSideTwins>,
    mut reauthor: ResMut<benilla_world::interior::InteriorReauthor>,
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
        let bits = benilla_world::mesh_tag::with_alpha(tag.0, authored * alpha);
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
            let want = benilla_world::model_render::far_resolved(
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
            Option<&benilla_world::interior::InteriorLit>,
            Has<benilla_world::model_render::FarSideOfWater>,
        ),
        (
            Without<RenderFade>,
            Without<PendingAppearFade>,
            Without<benilla_world::billboard::BillboardCard>,
        ),
    >,
    far_twins: &benilla_world::model_render::FarSideTwins,
    reauthor: &mut benilla_world::interior::InteriorReauthor,
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
            let bits = benilla_world::mesh_tag::with_alpha(tag.0, 1.0);
            if tag.0 != bits {
                tag.0 = bits;
            }
            let want = benilla_world::model_render::far_resolved(
                fm.material_for(lit, false),
                far_side,
                far_twins,
            );
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
            let bits = benilla_world::mesh_tag::with_alpha(tag.0, alpha);
            if tag.0 != bits {
                tag.0 = bits;
            }
            // A bake-classified part feathers on the PROBE-lit blend twin — the room light
            // rides the fade (the tag re-lane keeps the slot alongside the alpha, 0355); the
            // exterior twin at shade byte 0 read as full outdoor intensity deep indoors
            // (director-caught, 2026-07-13). Shared with the appear/despawn ramp since 0755, so
            // the two can never disagree about which twin a law wants.
            let want = benilla_world::model_render::far_resolved(
                fm.material_for(lit, true),
                far_side,
                far_twins,
            );
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

    use benilla_world::billboard::BillboardCard;
    use benilla_world::mesh_tag::alpha_bits;

    /// Step a [`PivotGlide`] at 60 Hz for `secs`, holding the target, and return the heights it
    /// passed through (one per frame).
    fn glide_run(g: &mut PivotGlide, target: Option<f32>, secs: f32) -> Vec<f32> {
        let dt = 1.0 / 60.0;
        (0..(secs / dt).round() as usize)
            .map(|_| g.advance(target, dt))
            .collect()
    }

    /// **The report** (the director, on the reference vs ours): shifting form on the real client
    /// *glides* the camera to the new body's height; ours snapped there and then drifted. The
    /// channel's whole job is that this is one smooth move, in **both** directions — the solver's
    /// `max(target, live)` is only a collision seed, and the far chain clamps back to the live
    /// value (`0x50e767`), so nothing about a *rising* target arrives early (wow-re
    /// `pivot-height-glide.md`, C2).
    #[test]
    fn a_shapeshift_glides_the_pivot_both_ways_and_never_snaps() {
        // Tauren → cat: the heights measured off a live probe run.
        let (tauren, cat) = (2.4659_f32, 1.0552_f32);
        let mut g = PivotGlide::default();
        // The first height a camera ever sees is established, not travelled to (`0x5127d4`).
        assert_eq!(
            g.advance(Some(tauren), 1.0 / 60.0),
            tauren,
            "the first arm snaps"
        );

        for (from, to) in [(tauren, cat), (cat, tauren)] {
            let expected = (to - from).abs() / CAM_PIVOT_SMOOTH_SPEED;
            let frames = glide_run(&mut g, Some(to), expected * 2.0);
            // It arrives, and only at the end.
            assert!((frames.last().copied().unwrap() - to).abs() < CAM_PIVOT_EPS);
            let arrived = frames
                .iter()
                .position(|h| (h - to).abs() < CAM_PIVOT_EPS)
                .unwrap();
            let took = arrived as f32 / 60.0;
            assert!(
                (took - expected).abs() < 0.05,
                "|Δh| / 1.2 yd/s = {expected:.3} s, took {took:.3} s ({from} → {to})"
            );
            // No frame jumps: the biggest single step is the cosine's own midpoint rate, which
            // for these Δ is far under half the total. A snap would put the whole Δ in one frame.
            let biggest = frames
                .windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0, f32::max);
            assert!(
                biggest < (to - from).abs() * 0.5,
                "the pivot must never teleport: biggest step {biggest} of Δ {}",
                (to - from).abs()
            );
        }
    }

    /// A model that has not resolved yet **holds** the channel — the reference skips the camera
    /// update outright while the preset is stale (`0x50e907`), which is what makes a display swap
    /// read as a pause and then one glide. Aiming at a placeholder in the meantime would send the
    /// camera on a round trip.
    #[test]
    fn a_body_with_no_model_holds_the_pivot_instead_of_re_aiming_it() {
        let mut g = PivotGlide::default();
        g.advance(Some(2.4659), 1.0 / 60.0);
        let held = glide_run(&mut g, None, 0.5);
        assert!(
            held.iter().all(|h| *h == 2.4659),
            "no target ⇒ no motion (the model is still loading)"
        );
        // …and the glide that follows starts from where it held, not from a placeholder.
        let frames = glide_run(&mut g, Some(1.0552), 2.0);
        assert!(frames[0] < 2.4659 && frames[0] > 2.4);
    }

    /// The per-frame re-arm has to be a **no-op** in steady state (the setter's 0.001 epsilon,
    /// `0x5126b0`). Restarting the move from its own midpoint every frame would stretch it
    /// asymptotically and it would never arrive — the classic re-arm bug this epsilon prevents.
    #[test]
    fn re_arming_the_same_target_every_frame_does_not_stretch_the_glide() {
        let mut g = PivotGlide::default();
        g.advance(Some(1.0), 1.0 / 60.0);
        let frames = glide_run(&mut g, Some(2.2), 2.0);
        assert!(
            (frames.last().copied().unwrap() - 2.2).abs() < CAM_PIVOT_EPS,
            "a per-frame re-arm must still arrive"
        );
    }

    /// The pivot target is the model height × the **raw** scale, clamped to the reference's own
    /// `[5/6, 15]` band (`0x50ca90`) — a giant's aura cannot walk the framing pivot into the sky,
    /// and a shrink cannot bury it in the floor.
    #[test]
    fn the_pivot_target_is_clamped_to_the_references_band() {
        let p = CameraPivot { height_local: 2.0 };
        assert_eq!(model_pivot_height(&p, 1.0), 2.0);
        assert_eq!(model_pivot_height(&p, 0.01), CAM_PIVOT_FLOOR);
        assert_eq!(model_pivot_height(&p, 100.0), CAM_PIVOT_CEIL);
    }

    /// A press that has travelled `yaw`/`pitch` **degrees** of camera rotation.
    fn press(yaw_deg: f32, pitch_deg: f32) -> PressGesture {
        PressGesture {
            at: 0.0,
            yaw_travel: yaw_deg.to_radians(),
            pitch_travel: pitch_deg.to_radians(),
        }
    }

    /// The report this whole change exists for (ledger B226, decision 1122): **a fast click
    /// selects however far the mouse swept.** Under 200 world the reference asks nothing about
    /// motion at all (`0x514ae0`'s first arm, `0x514b24`) — which is the gesture people actually
    /// make, flicking the cursor at a mob and clicking on arrival with the hand still moving.
    /// benilla used to destroy the pending click after 4 px of travel, so this case never fired.
    #[test]
    fn a_fast_click_selects_however_far_the_mouse_swept() {
        // A whole screen's worth of sweep — orders of magnitude past the travel gate.
        let swept = press(90.0, 45.0);
        assert!(
            swept.is_click(0.199),
            "under 200 world, travel is not consulted"
        );
        // And the camera is expected to have orbited through all of it: the two are independent,
        // which is the half that makes the reference's gesture possible at all.
    }

    /// The `mousespeed` slider is a MULTIPLIER over the shipped per-pixel rate (decision 1140), and
    /// the neutral notch has to reproduce the old constant exactly — the whole point of registering
    /// the default at 1.0 is that nobody's feel changes until they move the slider. Both the look
    /// rotation and the click-vs-drag travel budget read this one function, so the drag threshold
    /// scales with the pointer instead of drifting away from it.
    #[test]
    fn the_sensitivity_slider_is_a_multiplier_over_the_shipped_rate() {
        assert_eq!(LookConfig::default().rate(), LOOK_SENSITIVITY);
        let fast = LookConfig {
            sensitivity: 1.5,
            ..Default::default()
        };
        assert_eq!(fast.rate(), LOOK_SENSITIVITY * 1.5);
        let slow = LookConfig {
            sensitivity: *MOUSE_SPEED_RANGE.start(),
            ..Default::default()
        };
        assert_eq!(slow.rate(), LOOK_SENSITIVITY * 0.5);
    }

    /// **The auto-follow** (decisions 1493/1502) — 1.12's `cameraSmoothStyle`, the setting benilla
    /// spent its whole life behaving as "Never". The properties that a re-derivation gets wrong,
    /// and that the byte-verified mechanism (wow-re `camera-smooth-style.md`) turns on:
    /// it is armed by an input **edge** and then plays out unattended (not a per-frame chase of a
    /// moving target), the profile is a **cosine** smoothstep, the duration is `|Δ| / rate ×
    /// factor` **clamped to [0.1 s, 2.0 s]**, and Smart's `Idle`/`Stop` rows are a *cancel* — which
    /// is what "stays where you put it, except while you're moving" actually is.
    #[test]
    fn the_auto_follow_is_armed_by_an_input_edge_and_eases_home() {
        const DT: f32 = 1.0 / 120.0;
        // A quarter turn of orbit offset, left there by a drag.
        const OFFSET: f32 = std::f32::consts::FRAC_PI_2;

        fn cfg(style: FollowStyle) -> FollowConfig {
            FollowConfig {
                style,
                tracking_style: style,
                yaw_speed: FOLLOW_SPEED_DEFAULT,
            }
        }
        /// Run `secs` of frames at a fixed input word; returns the camera yaw it ends on.
        fn run(rig: &mut FollowRig, cfg: FollowConfig, word: u32, cam_yaw: f32, secs: f32) -> f32 {
            let mut yaw = cam_yaw;
            for _ in 0..((secs / DT).round() as i32).max(0) {
                let input = FollowInput {
                    cfg,
                    face_yaw: 0.0,
                    command: word,
                };
                if let Some(y) = rig.advance(&input, yaw, DT, false) {
                    yaw = y;
                }
            }
            yaw
        }

        // ── Smart: standing still with the camera dragged aside, nothing happens; the moment the
        // W edge lands, one armed transition brings it home over |Δ|/180°/s = 0.5 s.
        let mut rig = FollowRig::default();
        let c = cfg(FollowStyle::Smart);
        let parked = run(&mut rig, c, 0, OFFSET, 1.0);
        assert_eq!(
            parked, OFFSET,
            "Smart standing still leaves the camera alone"
        );
        let half = run(&mut rig, c, follow_cmd::FORWARD, parked, 0.25);
        assert!(
            half < OFFSET * 0.75 && half > OFFSET * 0.25,
            "mid-swing, eased: {half}"
        );
        let home = run(&mut rig, c, follow_cmd::FORWARD, half, 0.3);
        assert!(home.abs() < 1.0e-4, "arrived behind the character: {home}");
        // And holding W changes nothing further — the transition is spent, not a standing chase.
        let still_home = run(&mut rig, c, follow_cmd::FORWARD, home + 0.4, 1.0);
        assert_eq!(
            still_home,
            home + 0.4,
            "a HELD key re-arms nothing: only edges arm"
        );

        // ── Smart: releasing W (an edge into Stop) cancels rather than arming, so a camera nudged
        // while stopping stays nudged.
        let mut rig = FollowRig::default();
        let held = run(&mut rig, c, follow_cmd::FORWARD, 0.0, 0.1);
        let released = run(&mut rig, c, 0, held + OFFSET, 0.5);
        assert_eq!(released, held + OFFSET, "Stop is a cancel under Smart");

        // ── Always arms on that very same Idle edge — the one row where the two styles differ.
        let mut rig = FollowRig::default();
        let a = cfg(FollowStyle::Always);
        let held = run(&mut rig, a, follow_cmd::FORWARD, 0.0, 0.1);
        let returned = run(&mut rig, a, 0, held + OFFSET, 1.0);
        assert!(
            returned.abs() < 1.0e-4,
            "Always returns even from a standstill: {returned}"
        );

        // ── Never is inert, edge or no edge.
        let mut rig = FollowRig::default();
        let n = cfg(FollowStyle::Never);
        let _ = run(&mut rig, n, 0, OFFSET, 0.1);
        assert_eq!(
            run(&mut rig, n, follow_cmd::FORWARD, OFFSET, 2.0),
            OFFSET,
            "Never never arms"
        );

        // ── The duration floor: a 5° correction is 0.028 s of travel at 180 °/s, and the
        // reference's 0.1 s minimum overrides it — so it is NOT finished after 0.05 s.
        let mut rig = FollowRig::default();
        let small = 5.0_f32.to_radians();
        let _ = run(&mut rig, c, 0, small, DT);
        let mid = run(&mut rig, c, follow_cmd::FORWARD, small, 0.05);
        assert!(
            mid.abs() > 1.0e-4,
            "the 0.1 s floor is doing the work: {mid}"
        );
        assert!(run(&mut rig, c, follow_cmd::FORWARD, mid, 0.06).abs() < 1.0e-4);

        // ── Track (a taxi, a spline): Smart takes it lazily — 0.4 s of dead time first, then a
        // factor-10 return the 2 s ceiling caps.
        let mut rig = FollowRig::default();
        let _ = run(&mut rig, c, 0, OFFSET, DT);
        let delayed = run(&mut rig, c, follow_cmd::TRACK, OFFSET, 0.3);
        assert_eq!(delayed, OFFSET, "nothing moves inside the 0.4 s delay");
        let crawling = run(&mut rig, c, follow_cmd::TRACK, delayed, 0.6);
        assert!(
            crawling > OFFSET * 0.5,
            "a factor-10 return is a crawl, not a swing: {crawling}"
        );
        assert!(
            run(&mut rig, c, follow_cmd::TRACK, crawling, 2.0).abs() < 1.0e-4,
            "and it does arrive, inside the 2 s cap"
        );

        // ── A held drag freezes the channel outright: the hand owns the camera.
        let mut rig = FollowRig::default();
        let input = FollowInput {
            cfg: c,
            face_yaw: 0.0,
            command: follow_cmd::FORWARD,
        };
        assert!(rig.advance(&input, OFFSET, DT, true).is_none());
        assert!(rig.advance(&input, OFFSET, DT, true).is_none());

        // ── …and entering the drag **cancels** what was in flight (`0x50fe30` zeroes the
        // descriptors), so the return does not simply resume when the button comes up. Grab the
        // camera mid-swing and it stays where the hand left it until the next input edge.
        let mut rig = FollowRig::default();
        let _ = run(&mut rig, c, 0, OFFSET, DT);
        let mid = run(&mut rig, c, follow_cmd::FORWARD, OFFSET, 0.1);
        assert!(mid < OFFSET && mid > 0.0, "mid-swing: {mid}");
        let dragging = FollowInput {
            cfg: c,
            face_yaw: 0.0,
            command: follow_cmd::FORWARD | follow_cmd::LEFT_MOUSE,
        };
        assert!(rig.advance(&dragging, mid, DT, true).is_none());
        // The word is unchanged from the drag frame's, so nothing re-arms on its own…
        let parked = {
            let mut yaw = mid;
            for _ in 0..120 {
                let input = FollowInput {
                    cfg: c,
                    face_yaw: 0.0,
                    command: follow_cmd::FORWARD | follow_cmd::LEFT_MOUSE,
                };
                if let Some(y) = rig.advance(&input, yaw, DT, false) {
                    yaw = y;
                }
            }
            yaw
        };
        assert_eq!(parked, mid, "the cancelled transition does not resume");
        // …and the release's own edge (the mouse bit leaving the word) is what starts a new one.
        assert!(
            run(&mut rig, c, follow_cmd::FORWARD, parked, 1.0).abs() < 1.0e-4,
            "the release edge arms a fresh return"
        );
    }

    /// The state classifier's three vanilla input rules (wow-re `camera-smooth-style.md` §6.2) —
    /// the ones a client that keys off character *velocity* cannot reproduce, because they are
    /// read off the camera's own command word: right-mouse alone is a `Turn`, a turn key held
    /// **under** right-mouse is a `Strafe`, and both mouse buttons together are a `Move`. Plus the
    /// priority order, which is what decides the row when several are true at once.
    #[test]
    fn the_follow_state_reads_the_camera_input_word_not_the_character() {
        let state = |command: u32, stopping: bool| {
            FollowInput {
                cfg: FollowConfig::default(),
                face_yaw: 0.0,
                command,
            }
            .state(stopping)
        };
        assert_eq!(state(0, false), FollowState::Idle);
        assert_eq!(state(0, true), FollowState::Stop);
        assert_eq!(state(follow_cmd::RIGHT_MOUSE, false), FollowState::Turn);
        assert_eq!(
            state(follow_cmd::RIGHT_MOUSE | follow_cmd::TURN_LEFT, false),
            FollowState::Turn,
            "Turn outranks the Strafe its own condition also satisfies"
        );
        assert_eq!(state(follow_cmd::STRAFE_LEFT, false), FollowState::Strafe);
        assert_eq!(
            state(follow_cmd::RIGHT_MOUSE | follow_cmd::LEFT_MOUSE, false),
            FollowState::Turn,
            "both buttons satisfy Move, but Turn outranks it"
        );
        assert_eq!(state(follow_cmd::LEFT_MOUSE, false), FollowState::Idle);
        assert_eq!(state(follow_cmd::AUTORUN, false), FollowState::Move);
        assert_eq!(state(follow_cmd::TRACK, false), FollowState::Track);
        assert_eq!(
            state(follow_cmd::TRACK | follow_cmd::FORWARD, false),
            FollowState::Move,
            "Move outranks Track"
        );
        assert_eq!(
            state(follow_cmd::FEAR | follow_cmd::FORWARD, false),
            FollowState::Fear,
            "Fear outranks everything"
        );
        // …and the tracking style is the selector the moment Track or Fear is merely PRESENT,
        // even when a higher-priority state supplies the row.
        let mixed = FollowInput {
            cfg: FollowConfig {
                style: FollowStyle::Never,
                tracking_style: FollowStyle::Always,
                yaw_speed: FOLLOW_SPEED_DEFAULT,
            },
            face_yaw: 0.0,
            command: follow_cmd::TRACK | follow_cmd::FORWARD,
        };
        assert_eq!(mixed.style(), FollowStyle::Always);
        assert_eq!(mixed.state(false), FollowState::Move);
    }

    /// The enum is the ENGINE's (0/1/2), not the 1/2/3 the reference's own dropdown writes — and
    /// that stray `3` still has to mean Never, because that is what whoever wrote it meant.
    #[test]
    fn the_follow_style_enum_is_the_engines_and_tolerates_the_dropdowns_stray() {
        assert_eq!(FollowStyle::from_cvar(0.0), FollowStyle::Never);
        assert_eq!(FollowStyle::from_cvar(1.0), FollowStyle::Smart);
        assert_eq!(FollowStyle::from_cvar(2.0), FollowStyle::Always);
        assert_eq!(FollowStyle::from_cvar(3.0), FollowStyle::Never);
        // Off the ladder entirely: the registrar default, not a dead camera.
        assert_eq!(FollowStyle::from_cvar(-1.0), FollowStyle::Smart);
        assert_eq!(FollowStyle::from_cvar(9.0), FollowStyle::Smart);
        for style in [FollowStyle::Never, FollowStyle::Smart, FollowStyle::Always] {
            assert_eq!(
                FollowStyle::from_cvar(style.cvar().parse::<f32>().unwrap()),
                style,
                "the string round-trips"
            );
        }
        assert_eq!(FollowStyle::default(), FollowStyle::Smart);
    }

    /// The second arm: between 200 and 800 world the travel gate applies, per axis and independently
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

    /// The 800 world ceiling (`0x514aeb lea eax,[edx-0x320]`) is absolute — a long hold is never a
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
        app.init_resource::<benilla_world::interior::InteriorReauthor>();
        // The water-plane twin map the feather composes with (empty — no water in a fixture).
        app.init_resource::<benilla_world::model_render::FarSideTwins>();
        app.add_systems(Update, apply_self_model_fade);

        // The avatar: root -> joint (the eye-glow bone). Its card follows the joint.
        let avatar = app.world_mut().spawn(Embodied).id();
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
