//! Avatar + camera + input. Free-flies until the server reports our position, then takes third-person
//! control of the avatar (WASD walks it, height-following the terrain) and streams our movement to the
//! server as the confirmed mover. Owns the camera entity. The cursor itself is owned by the
//! [`crate::cursor`] subsystem; this module only hides it during mouselook (`CursorOptions.visible`).
//!
//! Mouse control mirrors vanilla's two look modes (grounded in the WoW 1.12 camera CVars / mouselook
//! API): **right-drag turns the character** (movement then
//! follows the camera heading), **left-drag orbits the camera** around a stationary character; either
//! locks + hides the cursor and restores it on release. **Both buttons held together run the character
//! forward** (vanilla's "both-button move"), steering with the mouse like a right-drag. The **scroll
//! wheel** zooms the third-person
//! distance (clamped to the vanilla `cameraDistanceMax` range; the camera *glides* to the new
//! distance). A left-drag orbit offset *persists* — the vanilla `cameraSmoothStyle` auto-follow that
//! swung the camera back behind the character while moving is deliberately removed (director's call).
//! The **dev chord + `F`** toggles free-fly (1043); the **dev chord + `G`** lands the avatar where
//! the camera is ([`land`]).
//!
//! Movement is a thin kinematic capsule controller over avian's `MoveAndSlide` (decision 0009).

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorOptions, PrimaryWindow};

use crate::creature_anim::{move_flags, wrap_pi, BodyTwist, MovementState};
use crate::debug_panel::InspectMode;
use crate::net::{ClientCommand, NetCommands, SelfPlayer, TeleportMessage, WorldportMessage};
use crate::ui_script::PointerOverUi;
use benilla_assets::AssetSet;
use benilla_world::interact::{WorldClick, WorldRightClick, WorldRightPress};
use benilla_world::schedule::WorldStage;

mod arc;
pub(crate) mod camera;
mod world_focus;
// The remembered camera pose (decision 1131) — it lives inside `player/` so it can read the rig's
// own `pub(super)` fields instead of widening them for a module outside.
mod camera_saved;
mod drunk;
mod follow;

mod gait;
// The land-here affordance (free-fly's other half). `pub(crate)` for its `LandHere` message, which
// the debug panel's button writes.
pub(crate) mod land;
mod move_trace;
mod movement_net;
// The kinematic mover step. `pub(crate)` because the grounded walk resolve is **not** the local
// player's alone: a remote mover's dead-reckon (`crate::net::motion::remote`) runs its extrapolated
// step through the very same code, the way the reference runs every mover through one controller
// (decision 0059's byte trail).
pub(crate) mod mover;
mod probe_cam;
mod probe_look;
mod server_ride;
mod setup;
mod state;
/// The step-up diagnostic probe — the blocked-frame report behind the `stup` trace tag.
pub(crate) mod step_probe;
mod swim;
mod wire_in;

// `apply_self_model_fade` is `pub(crate)`-visible: it is the LAST writer of a self body part's
// render-alpha field, so the unit-lane material-alpha compose (`entities::apply_unit_mat_alpha`)
// orders itself before it and lets that documented override stand.
pub(crate) use camera::apply_self_model_fade;
// `/follow` (decision 0890): chat asks with the message, `crate::target` resolves the subject into
// the state, and this module owns the motion.
use camera::{
    apply_zoom_scroll, run_look_session, seat_camera, CameraProbe, FlyCam, LookButton,
    CAM_COLLISION_RADIUS, CAM_DIST_DEFAULT, CAM_PIVOT_FALLBACK,
};
pub(crate) use camera::{head_height, CameraControl, CameraPivot};
pub(crate) use follow::{FollowRequest, FollowState};
// The shared avatar state + movement constants live in [`state`]; the private re-imports below are
// what lets this module and the concern modules beside it keep naming them `super::X` unchanged.
use state::{
    MoveSpeed, PlayerRide, AIR_NUDGE_SPEED, CAPSULE_RADIUS, FALL_FAR_DROP, FALL_FAR_TIME,
    FOOT_CONE_HEIGHT, GROUND_COS, GROUND_PROBE, JUMP_SPEED, LAND_PROBE, MOUSELOOK_PITCH_CLAMP,
    RUN_BACK_RATIO, SKIN_WIDTH, STATIONARY_CHASE_RATE, STEP_SLOPE_RATIO, STEP_SNAP_SLACK,
    STEP_UP_ADVANCE, STEP_UP_HEIGHT, TURN_RATE, TURN_RATE_MOVING, WEDGE_MIN_FALL,
    WEDGE_STALL_RATIO, WEDGE_STILL_FRAMES,
};
// `SETTLE_TIMEOUT` is `pub(crate)`: the settle release lives in the terrain streamer (decision
// 0737 — residency releases the hold, not ground contact), which owns the deadline push while the
// resident world is still the departed map's (0710).
pub(crate) use state::{
    Player, PlayerCapsule, CAPSULE_HEIGHT, DEFAULT_COLLISION_HEIGHT, FEATHER_TERMINAL_VELOCITY,
    GRAVITY, HOVER_CLIMB_RATE, HOVER_HEIGHT, SETTLE_TIMEOUT, TERMINAL_VELOCITY,
};
/// The swim boundary `0.75·h` — and therefore the **wade ceiling**, since wading is the implicit
/// in-liquid-but-not-swimming state and the two cannot be different numbers. Read by the creature
/// swim marker and the footstep splash slot, which have no `Player` of their own.
pub(crate) use swim::swim_enter_depth;

/// **`UNIT_FLAG_STUNNED`** — the `UNIT_FIELD_FLAGS` bit that freezes a character's *turning*
/// (decision 0872). Not a movement flag and not an aura: the reference reads it straight off the
/// descriptor block at `[[unit+0x110]+0xa0]` (predicate `0x5145b0` — note the inverted
/// `not/shr/and` form, which a census grepping only `test …,0x40000` misses — consumed at
/// `0x514755`, which skips both the turn and pitch emitters and force-stops either in flight).
///
/// It is the **other half** of a stun. vmangos's `HandleModStun` sets this flag *and* calls
/// `SetRooted(true)`, so `SPELL_AURA_MOD_STUN` grants both: root kills translation, this kills the
/// pivot. A pure root (Frost Nova, Entangling Roots) sets only the first — which is why a rooted
/// player can still turn and a stunned one cannot, the distinction B179 was reporting.
///
/// It has a second consumer beyond the turn: the idle/fidget selectors bail on it
/// (`0x5eb4f2`/`0x5ec219`), which is what stops even the idle twitch — see
/// [`crate::creature_anim::MovementState::stunned`] (decision 0880).
pub(crate) const UNIT_FLAG_STUNNED: u32 = 0x0004_0000;

/// Ask for a **stand state** — the client's `SetStandState(newState)` (`0x5ed430`: send
/// `CMSG_STANDSTATECHANGE` + apply locally through `0x6127b0`), as a message so every path that
/// wants a posture funnels through the ONE setter in [`control`] (the [`crate::creature_anim::
/// SheathRequest`] posture, decision 0080).
///
/// Two senders today: the `X` key reads the toggle inline in [`control`], and the **posture emotes**
/// (`/sit`, `/sleep`, `/kneel`, `/stand`, `/lay`) send this — `DoEmote`'s `EmoteSpecProc == 1`
/// branch calls the same `0x5ed430` the key does (wow-re `object-layer/scratch/emote-posture-
/// gate.md` §1, decision 0881). Routing them here is what makes `/sit` sit at all: the *server*
/// does nothing for a STATE text emote (vmangos `HandleTextEmoteOpcode` breaks out of the switch
/// for SIT/SLEEP/KNEEL), so the posture is the client's own to set.
///
/// The state values are `UnitStandStateType`: 0 STAND · 1 SIT · 3 SLEEP · 8 KNEEL.
#[derive(bevy::ecs::message::Message, Clone, Copy, Debug)]
pub(crate) struct StandStateRequest {
    pub(crate) state: u8,
}

/// The player/camera subsystem: spawns the camera + move/avatar resources at startup, drives the
/// third-person/free-fly controller each frame. (The cursor is the [`crate::cursor`] subsystem.)
pub(crate) struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        if let Some(look) = probe_look::from_env() {
            app.insert_resource(look);
        }
        if let Some(cam) = probe_cam::from_env() {
            app.insert_resource(cam);
        }
        follow::plugin(app);
        camera_saved::plugin(app);
        // 1160's wire (a), both directions (see `world_focus`): the game answers the world's
        // "where do I stream from" before the stream stage, and reads the residency the world
        // publishes after it to end its own post-snap hold.
        app.add_systems(
            Update,
            (world_focus::publish_viewer, world_focus::publish_view_focus)
                .before(benilla_world::schedule::WorldStage::Stream),
        )
        .add_systems(
            Update,
            world_focus::release_post_snap_hold.after(benilla_world::schedule::WorldStage::Stream),
        );
        app.init_resource::<camera::LookConfig>();
        app.init_resource::<camera::ZoomLimit>();
        app.add_systems(Startup, setup::setup_player.after(AssetSet::Open))
            // The world camera renders only when the world can be seen (decision 0540): in world,
            // or under the opaque loading screen (whose covered render is what compiles the
            // world's pipelines before the first visible frame). At the glue screens the fully
            // streamed world otherwise burns real GPU time behind an opaque fullscreen scene.
            .add_systems(Update, setup::gate_world_camera)
            // In capture mode the harness ([`crate::capture`]) pins the camera (and thus the stream
            // focus), so `control` must not also drive it — gate it off when capturing. In-world
            // only (decision 0193): at the character-select glue screen the controller must not
            // grab the cursor, fly the camera, or queue movement sends behind the overlay.
            .add_systems(
                Update,
                control
                    .in_set(WorldStage::Input)
                    .run_if(not(resource_exists::<crate::capture::CaptureMode>))
                    .run_if(in_state(crate::char_select::ClientState::InWorld)),
            )
            // The posture setter's queue (the `/sit` family — decision 0881; `control` is the sole
            // executor, like the sheath queue).
            .add_message::<StandStateRequest>()
            // Land-here ([`land`]): the ask, and the re-attach when the server's teleport lands.
            // Before `control` so the frame that applies the teleport is the frame that takes
            // third-person control back — `control` reads `detached` after this has cleared it.
            .add_message::<land::LandHere>()
            .add_systems(
                Update,
                land::land_here
                    .in_set(WorldStage::Input)
                    .before(control)
                    .run_if(in_state(crate::char_select::ClientState::InWorld)),
            )
            // The scripted mouse-turn (`WOW_PROBE_LOOK`, decision 0621): rotates the aim before
            // `control` reads it, so the frame that sees the new facing is the frame that streams
            // it. Registered only when the env var asks for it — inert otherwise.
            .add_systems(
                Update,
                probe_look::drive_probe_look
                    .in_set(WorldStage::Input)
                    .before(control)
                    .run_if(resource_exists::<probe_look::ProbeLook>)
                    .run_if(in_state(crate::char_select::ClientState::InWorld)),
            )
            // The scripted camera park (`WOW_PROBE_CAM`, decision 0653): holds yaw/pitch/zoom so an
            // unattended burst can frame a subject that isn't on the ground. Same slot as the turn
            // above — `control` reads the rig right after — and equally inert without the env var.
            .add_systems(
                Update,
                probe_cam::drive_probe_cam
                    .in_set(WorldStage::Input)
                    .before(control)
                    .run_if(resource_exists::<probe_cam::ProbeCam>)
                    .run_if(in_state(crate::char_select::ClientState::InWorld)),
            )
            // A server-authored spline (Charge/knockback/taxi) driving our own player is mirrored into
            // `Player` here, *before* `control` reads `pos` to seat the camera and skip input. Same
            // gates as `control` (not while capturing; in-world only).
            .add_systems(
                Update,
                server_ride::drive_self_ride
                    .in_set(WorldStage::Input)
                    .before(control)
                    .run_if(not(resource_exists::<crate::capture::CaptureMode>))
                    .run_if(in_state(crate::char_select::ClientState::InWorld)),
            )
            // A confirmed `/logout` releases the avatar: the streamed entity is despawned by the
            // net drain, and dropping `active` re-arms the take-control latch for the next login
            // (possibly a different character). Ungated — the message lands as the state flips.
            .add_systems(Update, wire_in::release_on_logout.in_set(WorldStage::Input))
            // Mirror our body's collision height onto `Player` for the swim arm (decision 0645).
            // A *continuous* sync rather than a one-shot at take-control, for the reason the
            // take-control branch itself records: a cross-map worldport re-streams the `SelfPlayer`
            // entity, so anything latched on that edge is lost on transfer. Before `control`, which
            // is where the swim depth lines are evaluated.
            .add_systems(
                Update,
                mirror_self_collision_height
                    .in_set(WorldStage::Input)
                    .before(control),
            )
            // `/follow` (decision 0890): steer the facing and decide this tick's synthesized forward
            // input immediately BEFORE the controller, which folds the flag into its forward axis.
            // The player's own turn input therefore runs after us and wins, which is exactly what
            // makes the turn-away cancel reachable.
            .add_systems(
                Update,
                follow::steer_follow
                    .in_set(WorldStage::Input)
                    .before(control)
                    .run_if(in_state(crate::char_select::ClientState::InWorld)),
            )
            // The self-avatar zoom-in fade rides the same `MeshTag`/material channel as the interior
            // classifier + the appear/despawn fades, so it must run *after* both to win the frame while
            // fading (and yield to them otherwise). It also writes `Visibility` (the first-person
            // hide), so it must run after the model-`Visibility` authority
            // (`debug_panel::apply_model_visibility`) too — otherwise whichever system Bevy's
            // arbitrary sort ran last would win, and the authority could re-show the body in
            // first-person. First-person correctness outranks the dev creature-toggle for these few
            // submeshes. Gated off in capture mode alongside `control` (whose per-frame
            // `self_fade_alpha` it consumes), so a pinned capture never hides the avatar.
            .add_systems(
                Update,
                apply_self_model_fade
                    .after(benilla_world::interior::classify_entity_interior)
                    .after(benilla_world::model_fade::apply_render_fade)
                    .after(benilla_world::model_render::ModelVisSet)
                    .run_if(not(resource_exists::<crate::capture::CaptureMode>)),
            );
    }
}

/// Mirror the streamed `SelfPlayer`'s [`crate::entities::CollisionHeight`] onto [`Player`] — the
/// avatar's swim depth lines are fractions of it (decision 0645), and the swim arm runs off the
/// resource, not the entity. One entity, one copy: no work until our body streams in, and it
/// re-syncs itself after a worldport re-streams that entity under a new one.
fn mirror_self_collision_height(
    mut player: ResMut<Player>,
    self_player: Query<&crate::entities::CollisionHeight, With<SelfPlayer>>,
) {
    if let Ok(&h) = self_player.single() {
        if player.collision_height != h {
            player.collision_height = h;
        }
    }
}

/// Camera + avatar controller. Free-flies until the server reports our position; then takes
/// third-person control (WASD walks the avatar; right-drag turns it, left-drag orbits the camera,
/// wheel zooms) and streams our movement to the server as the confirmed mover. The dev chord + `F`
/// toggles free-fly (decision 1043).
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn control(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    // Nested into one param to stay within Bevy's 16-element system-param tuple limit. (The
    // scroll wheel left this tuple with 0997: zoom reads the CAMERAZOOM bindings now.)
    // The pointer's motion this frame + the two knobs that scale what it does with it. Bundled
    // because a Bevy system takes at most 16 parameters and this one is at the ceiling — the
    // grouping is the existing `mouse` tuple, widened rather than a seventeenth argument.
    pointer: (
        Res<AccumulatedMouseMotion>,
        Res<camera::LookConfig>,
        Res<camera::ZoomLimit>,
    ),
    // The net bridge, bundled into one param (16-param limit): the outbound command channel + the
    // inbound teleport/worldport messages `apply_net_updates` wrote earlier this frame
    // (WorldStage::Net), + the sheath-setter queue (the Z toggle's request — decision 0080).
    mut net: (
        Res<NetCommands>,
        MessageReader<TeleportMessage>,
        MessageReader<WorldportMessage>,
        MessageWriter<crate::creature_anim::SheathRequest>,
        MessageReader<crate::net::SpeedChangeMessage>,
        // The ack'd movement-mode family (decisions 0308, 0866): root / water-walk / feather-fall /
        // hover, granted on our mover — applied to `player.modes` and acked here with the live pose.
        MessageReader<crate::net::MoveModeMessage>,
        // The landing report for the client-side hard-landing predictor (`0x602d00` — wound
        // vocal + dust; the consumers gate on the threshold, `creature_anim::env_damage`).
        MessageWriter<crate::creature_anim::HardLanding>,
        // The cast bar's local self-cancel trigger (decision 0256 open item 2): the controller
        // reports the move edges the real client's movement machine hands `AbortCast 0x6e4940`.
        ResMut<crate::ui_cast::LocalMoveStart>,
        // The mounted space-bar flourish (decision 0441 P2): our own MountSpecial(94) plays
        // locally at send time; the net drain self-suppresses any broadcast echo.
        MessageWriter<crate::creature_anim::MountFlourish>,
        // A `MSG_MOVE_*` the server addressed to our OWN mover (decision 0725) — a pose it wrote,
        // with no handshake and no ack owed. `wire_in` snaps to it.
        MessageReader<crate::net::SelfMoveMessage>,
        // The posture queue (decision 0881): `/sit` and its family ask here; the X key is read
        // inline below. One setter, either way.
        MessageReader<StandStateRequest>,
    ),
    // Nested into one param to stay within Bevy's 16-element system-param tuple limit (see `mouse`).
    speed_capsule: (
        Res<MoveSpeed>,
        Res<PlayerCapsule>,
        Res<CameraProbe>,
        Res<PointerOverUi>,
        Res<InspectMode>,
        Res<crate::ui_script::UiKeyboardCapture>,
        Res<crate::ui_script::PlayerUiClickConsumed>,
        // The binding dispatch (decision 0997): every rebindable input below reads command
        // state from here — raw `keys` remain only for the dev chord's free-fly toggle
        // (decision 1043) and the look-session mouse.
        Res<crate::bindings::BindingsState>,
    ),
    mut commands: Commands,
    mut player: ResMut<Player>,
    mut rig: ResMut<CameraControl>,
    // Avian's kinematic move-and-slide: sweeps the capsule against the streamed colliders (decision 0009).
    collide: benilla_world::collision::WorldCollision,
    mut cameras: Query<(&mut Transform, &mut FlyCam, &Camera)>,
    // The streamed self entity: we read its server pose to take control, then drive its transform
    // (feet position + facing) and feed its movement to the animation selector via `MovementState`. Its
    // body model is attached by the entity renderer through the same path as any other player (0041).
    mut self_player: Query<
        (
            Entity,
            &mut Transform,
            Option<&mut MovementState>,
            Option<&CameraPivot>,
            Option<&crate::creature_anim::AnimDriver>,
            Option<&crate::net::ObjectStore>,
            Has<crate::creature_anim::Engaged>,
            Option<&crate::net::UnitSpeeds>,
            Option<&mut BodyTwist>,
            // What is worn in each hand and in the ranged slot — the Z toggle's cycle reads it
            // (the ref's three `GetWeapon(0/1/2)` calls before the state walk, below).
            Option<&crate::creature_anim::Wielded>,
        ),
        (With<SelfPlayer>, Without<FlyCam>),
    >,
    window: Single<(&mut Window, &mut CursorOptions), With<PrimaryWindow>>,
    // Clicks go out here — left for the target picker, right for the context action (attack) — and
    // the third is the right button's raw DOWN edge (targeting's cancel, decision 0792). A press
    // engages its camera look *and* arms a click test; the release settles that test on the
    // reference's time/travel predicate, so one gesture can orbit and select both (decision 1122).
    // The locals hold each button's pending [`camera::PressGesture`] (`None` = no press pending).
    mut world_clicks: (
        MessageWriter<WorldClick>,
        MessageWriter<WorldRightClick>,
        MessageWriter<WorldRightPress>,
    ),
    mut click_test: (
        Local<Option<camera::PressGesture>>,
        Local<Option<camera::PressGesture>>,
    ),
    // World context for the mover, bundled into one param (16-param limit): the world query the
    // swim mode + buoyant float ask the liquid through (see [`swim`]), the armed transports (the
    // platform-frame carry/attach — decision 0438 phase 2; `Without`s only disjoint the borrows),
    // and the parent chain (the attach walk resolves a deck prop's collider child to the
    // transport that owns it — solid cargo, 0470).
    world_q: (
        benilla_world::world_point::WorldPoint,
        Query<
            (&Transform, &crate::net::Guid),
            (
                With<crate::transport::Transport>,
                Without<SelfPlayer>,
                Without<FlyCam>,
            ),
        >,
        Query<&ChildOf>,
    ),
) {
    let (world, transports, child_of) = (&world_q.0, &world_q.1, &world_q.2);
    let (left_click, right_click) = (&mut *click_test.0, &mut *click_test.1);
    let Ok((mut cam_t, mut cam, camera)) = cameras.single_mut() else {
        return;
    };
    let (mut window, mut cursor_opts) = window.into_inner();
    let mouse_motion = &pointer.0;
    let look_cfg = *pointer.1;
    let zoom_max = pointer.2.max;
    let (move_speed, capsule, cam_probe, pointer_over_ui, inspect, ui_capture, click_consumed) = (
        &speed_capsule.0,
        &speed_capsule.1 .0,
        &speed_capsule.2 .0,
        &speed_capsule.3,
        &speed_capsule.4,
        &speed_capsule.5,
        &speed_capsule.6,
    );
    let binds = &speed_capsule.7;
    let dt = time.delta_secs();
    // While a focused UI EditBox (the chat input, a mail field) owns the keyboard, keyboard reads see
    // "no keys held" — so the avatar isn't also driven while typing (a `.tele` command). Mouse still
    // works. The gate is `UiKeyboardCapture`, which the focused chat EditBox drives; the free-fly
    // chord below is deliberately outside it, like every dev chord ([`modkeys::dev_chord`]).
    let typing = ui_capture.0;
    // The rebindable inputs all read `binds` (decision 0997): the dispatch already enforced the
    // typing gate and 0585's exact-modifier law when it latched, so this module carries neither
    // anymore. Nothing here reads a bare key any more (decision 1043) — the free-fly toggle is on
    // the dev chord, and the Ctrl run boost is gone.

    // Both mouse buttons held together = vanilla's "both-button run": the avatar runs forward while
    // the character steers with the mouse (turns like a right-drag), regardless of which button went
    // down first. Checked directly here rather than through the single-button look mode below.
    // MOVEANDSTEER (default Middle Mouse) is the same state through a binding — 1.12's own body
    // runs the identical CameraOrSelectOrMove + TurnOrAction pair a both-button press does.
    let steer_held = binds.pressed(crate::bindings::cmd::MOVE_AND_STEER);
    let both_buttons =
        (buttons.pressed(MouseButton::Left) && buttons.pressed(MouseButton::Right)) || steer_held;

    // The look session gets a SHADOW copy of `CursorOptions`, written back only on a real change:
    // handing it the component's `Mut` directly reborrowed mutably every frame, which marks it
    // Changed regardless of writes — and bevy_winit's `changed_cursor_options` then re-applied
    // cursor state to AppKit per frame, an OS call that intermittently stalls the main thread for
    // milliseconds (the 0366 frame-tail hunt's second-biggest line).
    let mut opts_shadow = cursor_opts.bypass_change_detection().clone();
    // Snapshot for the seated-turn stand-up below: a right-drag (or both-button) look session
    // writes `face_yaw` directly — any change is a real mouse TURN of the character (a left-drag
    // orbits the camera only and never touches it).
    let yaw_before_look = player.face_yaw;
    // **Stunned** (`UNIT_FIELD_FLAGS & 0x40000` — decision 0872): read once here, because the very
    // first thing a stun suppresses is the mouse turn below. This is a descriptor bit, NOT a
    // movement flag and NOT an aura: the reference's `0x5145b0` computes `!STUNNED` straight off
    // `[[unit+0x110]+0xa0]` (`not eax; shr eax,0x12; and eax,1`) and `0x514755` consumes it to skip
    // the turn and pitch emitters outright.
    let stunned = self_player
        .single()
        .ok()
        .and_then(|(.., store, _, _, _, _)| {
            store.map(|s| s.0.unit_flags() & UNIT_FLAG_STUNNED != 0)
        })
        .unwrap_or(false);
    // Drunkenness (B210): this frame's wobble angle, computed once — the facing veer and the
    // swim-pitch porpoise below both read it. Zero while sober (`wobble` early-outs on a 0.0
    // fraction), and zero while stunned — the reference's wobble sits behind the same
    // input-allowed chain as the turn emitters (`0x60aa47` → `0x5145b0`).
    let drunk_wobble = {
        let f = self_player
            .single()
            .ok()
            .and_then(|(.., store, _, _, _, _)| store.and_then(|s| s.0.player_drunk_byte()))
            .map_or(0.0, drunk::fraction);
        if stunned {
            0.0
        } else {
            drunk::wobble(time.elapsed().as_millis() as u32, f)
        }
    };
    run_look_session(
        &buttons,
        mouse_motion,
        both_buttons,
        &mut rig,
        &mut cam,
        &mut player.face_yaw,
        &mut window,
        &mut opts_shadow,
        camera,
        pointer_over_ui.0,
        inspect.enabled,
        click_consumed.0,
        &mut world_clicks.0,
        &mut world_clicks.1,
        &mut world_clicks.2,
        left_click,
        right_click,
        look_cfg,
        time.elapsed_secs(),
    );
    // A stun freezes the BODY, not the view. The look session has already moved `cam.yaw` (and, on
    // a right-drag, coupled `face_yaw = cam.yaw`); putting the aim back leaves the camera orbiting
    // a body that does not turn — which is what a stunned character looks like, and what the
    // reference produces by never running the turn emitter at all. Restoring rather than gating
    // inside the session keeps the approved camera path (0050/0366's right-drag coupling) untouched.
    // `mouse_turned` then reads false by construction, so the seated-turn stand-up cannot fire either.
    if stunned {
        player.face_yaw = yaw_before_look;
    }
    let mouse_turned = player.face_yaw != yaw_before_look;
    {
        let cur = cursor_opts.bypass_change_detection();
        if cur.visible != opts_shadow.visible
            || cur.grab_mode != opts_shadow.grab_mode
            || cur.hit_test != opts_shadow.hit_test
        {
            *cursor_opts = opts_shadow;
        }
    }

    // Camera zoom rides the CAMERAZOOMIN/OUT bindings (0997; defaults = the wheel pair). The
    // wheel-over-UI routing lives in the dispatch now — a wheel the quest log consumed never
    // reaches these commands — and a rebound zoom KEY steps 1.0 per press, 1.12's own
    // `CameraZoomIn(1.0)` argument.
    let zoom = binds.amount(crate::bindings::cmd::CAMERA_ZOOM_IN)
        - binds.amount(crate::bindings::cmd::CAMERA_ZOOM_OUT);
    apply_zoom_scroll(zoom, dt, &mut rig, zoom_max);

    // Free-fly is a dev instrument, so it sits on the dev chord, not a bare `F` (decision 1043).
    // A bare `F` is a key the reference lets a player bind — our own store test binds it to JUMP —
    // and a dev doesn't get to squat on the game's namespace (0585, the same rule that moved the
    // perf HUD off bare `P`). Ungated on `typing` like every chord: it can't be mistaken for text.
    if benilla_world::modkeys::dev_chord(&keys, KeyCode::KeyF) {
        player.detached = !player.detached;
    }

    // Server-authored movement edges + their mandatory acks (worldport/teleport snaps, root,
    // water-walk, the take-control edge — [`wire_in`]). The returned forced-speed changes were
    // already acked pre-control/detached; controlled, the movement stream below acks them with
    // its live per-frame payload.
    let speed_acks = wire_in::apply_server_moves(
        &time,
        &mut commands,
        &mut player,
        &mut cam,
        &net.0,
        &mut net.1,
        &mut net.2,
        &mut net.4,
        &mut net.5,
        &mut net.9,
        transports,
        self_player
            .single()
            .ok()
            .map(|(_, t, ..)| (t.translation, server_ride::yaw_of(t.rotation))),
    );

    let flat = |v: Vec3| Vec3::new(v.x, 0.0, v.z).normalize_or_zero();

    // The platform carry (decision 0438 phase 2): while attached to a transport, recompose the
    // feet from the boat's THIS-frame pose (the transport tick runs on the Net→Input edge, so it's
    // fresh) before any input integrates — the deck's motion carries the standing player, and its
    // per-frame yaw delta turns them with it (applied incrementally so it composes with whatever
    // mouse-look already wrote to `face_yaw` this frame). A despawned boat (streamed out) detaches
    // into an ordinary fall from the last world pose.
    //
    // The carry is rigid for the WHOLE rider — aim (`face_yaw`), rendered body (`model_yaw`), and
    // camera (`cam.yaw`) take the same delta, all HERE. Carrying only the aim leaves the standing
    // body-chase to close the gap frame after frame, and that chase-step is exactly what latches
    // the turn-in-place foot-shuffle (whose keyframes fire step sounds): a sailing boat's spline
    // yaw drifts continuously, so the rider shuffled and clacked the whole voyage (director,
    // 2026-07-17). The deck turning under you is not you turning — the chase and its shuffle only
    // see input turns.
    //
    // The camera's share is unconditional — a deck turn is FRAME motion, not an input turn, so it
    // never routes through `seat_camera`'s look-session gate (that gate protects the camera from
    // *keyboard* turns while a drag owns it). Routing it there was the right-drag drift bug
    // (director, 2026-07-18): during a look session the gate ate the camera's share, and the
    // right-drag coupling `face_yaw = cam.yaw` (which runs first next frame) then yanked the aim
    // back to the world-fixed camera — undoing the deck carry, so with the mouse still the scene
    // swung across the screen and the rider visibly spun against the deck. Carrying all three here
    // keeps the drag's orbit offset (`cam.yaw − face_yaw`) exactly as the hand left it while the
    // whole rider assembly turns with the boat — the reference's camera rides the transport-local
    // player rig the same way.
    if let Some(ride) = player.ride.as_ref() {
        match transports.get(ride.entity) {
            Ok((boat, _)) => {
                let world = boat.translation + boat.rotation * ride.local_pos;
                let yaw_now = boat.rotation.to_euler(EulerRot::YXZ).0;
                let mut dyaw = yaw_now - ride.boat_yaw;
                // `to_euler` wraps to (−π, π]; a boat crossing that seam reads as a ±2π hop.
                dyaw = (dyaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                    - std::f32::consts::PI;
                player.pos = world;
                player.face_yaw += dyaw;
                player.model_yaw = wrap_pi(player.model_yaw + dyaw);
                cam.yaw += dyaw;
                if let Some(r) = player.ride.as_mut() {
                    r.boat_yaw = yaw_now;
                }
            }
            Err(_) => player.ride = None,
        }
    }

    if player.active && !player.detached {
        // Server ride guard: a server-authored spline (Charge/knockback/taxi) owns the avatar this
        // frame. `drive_self_ride` (ordered just before us) already synced `player.pos` + facing from
        // the `sample_splines` transform and set the run animation; here we only carry the
        // follow-camera onto the moving avatar. Input, physics, and the outbound movement stream all
        // yield until the ride ends (where `drive_self_ride` acks `CMSG_MOVE_SPLINE_DONE` and resumes).
        if player.server_riding {
            let pivot_h = self_player
                .single()
                .map(|(_, t, _, pivot, ..)| head_height(pivot, t.scale.x))
                .unwrap_or(CAM_PIVOT_FALLBACK);
            let head = player.pos + Vec3::Y * (CAPSULE_HEIGHT - CAPSULE_RADIUS);
            seat_camera(
                dt, 0.0, player.pos, head, pivot_h, &mut rig, &mut cam, &mut cam_t, &collide,
                cam_probe,
            );
            return;
        }
        // ── Autorun ── TOGGLEAUTORUN through the binding table (0997; 1.12 defaults NUMLOCK +
        // BUTTON4 — the latter is winit's `Forward`, the thumb button this toggle lived on before
        // the table existed, kept by the codec's BUTTON4 mapping). A latched mode, not a held key:
        // the keyboard chord is typing-gated at dispatch like every binding, the mouse chord is
        // not — and the reference agrees, its focus-loss handler releasing every direction bit
        // while preserving `0x1000` (`0x514490`'s `and eax,0xfffff00f`, VERIFIED).
        let mut autorun_armed = false;
        if binds.fired(crate::bindings::cmd::TOGGLE_AUTORUN) {
            player.autorun = !player.autorun;
            autorun_armed = player.autorun;
        }
        // A mouse whose extra buttons don't land on Back/Forward would otherwise fail silently, and
        // "nothing happened" is the least debuggable report there is. Name what did arrive.
        for b in buttons.get_just_pressed() {
            if let MouseButton::Other(n) = b {
                info!("mouse: unmapped button Other({n}) — bindings know BUTTON4/BUTTON5 as winit Forward/Back");
            }
        }
        // ── The cancel set ── autorun is NOT simply "held forward" — the thing that makes it its own
        // mode is what *destroys* it. Six writers clear the bit in the reference; these are the ones
        // with a benilla analog (VERIFIED, wow-re `rf79-autorun-cancel-set.md`):
        //
        // - **A W or S key-DOWN** — unconditional, and the subtle one: the directional handlers look
        //   pure (each pushes only its own bit), but they tail into the shared SET helper `0x514840`,
        //   which does `and [MOVE+4],0xffffefff` under `test cl,0x30` (fwd `0x10` | back `0x20`) at
        //   `0x514a5a`. A per-handler read answers "no" and is wrong about the behaviour. It runs
        //   *before* the axis (`0x5150a7` vs the emitter tail `0x5151a0`), so the axis never sees the
        //   combination. **Key-DOWN only**: the release path `0x514b70` restores nothing, which is why
        //   letting go of S after reversing leaves you standing rather than running again.
        // - **The transition INTO both-buttons-held** (`0x514a73`, the same helper) — engaging the
        //   both-button run replaces autorun rather than stacking with it.
        // - **Losing the mover** — death, root/stun, a taxi/charge hand-off. In the reference the
        //   emitter's gate `0x514560` goes down (health `<= 0`, `MOVEMENTFLAGS & 0x1200`, the on-taxi
        //   predicate) and writer #4 `0x514748` clears the bit as a side effect of the next emit; a
        //   level test is the faithful shape, not an edge. (Mechanism VERIFIED; the individual bit
        //   identities behind the gate are INFERRED — see the note's §4.)
        //
        // Deliberately absent, each VERIFIED as a *survivor*: a jump, a chat EditBox taking focus, and
        // a zone change. Mounting is genuinely unsettled in the reference and left alone here.
        let both_buttons_engaged = (both_buttons
            && (buttons.just_pressed(MouseButton::Left)
                || buttons.just_pressed(MouseButton::Right)))
            || binds.just_pressed(crate::bindings::cmd::MOVE_AND_STEER);
        if state::autorun_cancelled(
            binds.just_pressed(crate::bindings::cmd::MOVE_FORWARD),
            binds.just_pressed(crate::bindings::cmd::MOVE_BACKWARD),
            both_buttons_engaged,
            player.modes.rooted || player.server_riding,
        ) {
            player.autorun = false;
        }
        let autorun = player.autorun;
        // ── The forward/back axis ── one net value ([`state::forward_axis`], whose tests pin the
        // verified state table) read by every forward/back consumer below, so the direction we move,
        // the speed we pick, the swim amounts and the flags we stream can't disagree (decision 0056).
        //
        // Zero is the state no "autorun = held forward" reading can produce, and it is reachable:
        // hold S *first*, then toggle autorun — the toggle pushes X=`0x1000`, so `test cl,0x30` misses
        // and the bit survives — and the client emits MSG_MOVE_STOP with S still held. The other order
        // (autorun, then S) destroys the bit at key-down and walks you backward. Same two keys, two
        // outcomes; that asymmetry is the whole shape of the feature.
        // `/follow` enters as the FORWARD term, not a fifth source (decision 0890): the reference's
        // follow pushes the very same move-forward bit `0x100000` the W key does, through the same
        // setter, so it nets against a held S and diagonals with a strafe exactly like a held W. It
        // rides the HELD state and not the key-DOWN edge, so it never trips the autorun cancel set
        // above — which is right: synthesized input is not a keypress.
        let fwd_axis = state::forward_axis(
            binds.pressed(crate::bindings::cmd::MOVE_FORWARD) || player.follow_forward,
            binds.pressed(crate::bindings::cmd::MOVE_BACKWARD),
            both_buttons,
            autorun,
        );
        // Vanilla turn/strafe control model (decision 0050, VERIFIED wow-5875-re `0x7c5360`): W/S move
        // forward/back in the facing; **A/D turn the character** (rotate the facing at the turn rate) so
        // the body faces where it runs — UNLESS right-mouse is held (mouse-look), where A/D strafe and
        // the facing tracks the camera; **Q/E always strafe**. Movement basis is the *character* facing,
        // so left-drag (camera-only orbit) doesn't change which way W walks.
        let mouselook = both_buttons || rig.look == Some(LookButton::Right);
        // The strafe axis, **netted exactly like `fwd_axis`** — Q/E always strafe, A/D only while
        // mouse-looking. Netting is not a nicety: the two bits are mutually exclusive on the wire.
        // Holding both keys used to OR `STRAFE_LEFT | STRAFE_RIGHT` into the flags while the avatar
        // stood still (the `dir` sum below cancels), and vmangos **silently drops** every movement
        // packet carrying that pair — never relaying it to anyone (decision 0622). Measured: 48 such
        // packets in one session, 0 received by a watching client, against 0 in the reference
        // client's entire 1.12.1 capture. That is decision 0056's invariant — the wire mirrors the
        // avatar's actual motion — violated on this one axis only; the swim branch already nets.
        let strafe_left = binds.pressed(crate::bindings::cmd::STRAFE_LEFT);
        let strafe_right = binds.pressed(crate::bindings::cmd::STRAFE_RIGHT);
        let turn_left = binds.pressed(crate::bindings::cmd::TURN_LEFT);
        let turn_right = binds.pressed(crate::bindings::cmd::TURN_RIGHT);
        let side_axis = i32::from(strafe_right) - i32::from(strafe_left)
            + if mouselook {
                i32::from(turn_right) - i32::from(turn_left)
            } else {
                0
            };
        // TURNLEFT/TURNRIGHT turn the facing when not mouse-looking (yaw increases turning left,
        // matching mouse-left). …and never while stunned: the reference skips the keyboard turn
        // emitter `0x514f50` entirely (and force-stops an in-flight turn) behind the same
        // `0x514755` gate. Killing the turn here is also what ends B179's *second* half — the walk
        // animation a stunned character was still playing was the turn-in-place shuffle, which
        // `gait` derives from real yaw change.
        let turning = !mouselook && !stunned && (turn_left || turn_right);
        // The net translate state — the reference's `flags & 0xf` (its four move bits), read off
        // the *net* axes, not the keys: W+S streams no direction bit and doesn't translate.
        let translating = fwd_axis != 0 || side_axis != 0;
        // This frame's keyboard-turn rotation — `seat_camera` carries the camera by it rigidly
        // (char and camera turn as one on the reference; director's call, closing 0050's open
        // "camera follow on turn" feel item).
        let mut turn_delta = 0.0;
        if turning {
            let mut turn = 0.0;
            if turn_left {
                turn += 1.0;
            }
            if turn_right {
                turn -= 1.0;
            }
            // 0.75× while also translating — the verified `flags & 0x200f` case.
            let rate = TURN_RATE * if translating { TURN_RATE_MOVING } else { 1.0 };
            turn_delta = turn * rate * dt;
            player.face_yaw += turn_delta;
        }
        // The drunk veer (B210): while moving, the facing increments by the wobble angle every
        // frame (`0x60aa70–0x60aab7`: `facing + wobble`, 2π-wrapped, committed via the normal
        // facing pipeline `0x60de30` — so it streams on the wire like any turn). The slow sign
        // oscillation of the pulse is what makes the walk meander. Skipped while a keyboard turn
        // is held — the reference's `flags & 0x30` guard (`0x60aa5a`) — so deliberate turning
        // stays crisp; both yaw conventions increase turning left, so the add maps sign-for-sign.
        // The veer joins `turn_delta` so `seat_camera` carries the camera with it exactly like a
        // keyboard turn (char and camera turn as one) — the reference's camera follows the drunk
        // meander too (director's ref observation, decision 1018); without the carry the character
        // staggers out from under a fixed camera.
        if drunk_wobble != 0.0 && translating && !turning {
            player.face_yaw += drunk_wobble;
            turn_delta = drunk_wobble;
        }
        let face_rot = Quat::from_rotation_y(player.face_yaw);
        let move_fwd = flat(face_rot * Vec3::NEG_Z);
        let move_right = flat(face_rot * Vec3::X);
        let mut dir = Vec3::ZERO;
        // Forward/back comes from the net axis (W, S, both-button and autorun already summed) — one
        // step in its sign, never a doubled push, exactly as the emitter issues one START in
        // `sign(axis)`. (`mover::step` normalizes anyway, but the axis is the honest shape.)
        match fwd_axis.signum() {
            1 => dir += move_fwd,
            -1 => dir -= move_fwd,
            _ => {}
        }
        // Strafe slides without turning, one step in the netted sign — never a doubled push, and a
        // cancelled pair is genuinely no strafe (the same shape as the forward/back axis above).
        match side_axis.signum() {
            1 => dir += move_right,
            -1 => dir -= move_right,
            _ => {}
        }
        // **Rooted: translation intent dies here — turning above stays live.** Confirmed at the
        // bytes (decisions 0866/0872) and it is *authored*, not accidental: the reference's input
        // tick consults an allow-list (`0x615c71` → the byte table at `0x618054`) which blocks the
        // translation command ids and **explicitly permits** the turn ids 8/9/0xa, pitch, run/walk
        // and SetFacing. A character who cannot even pivot is STUNNED, a separate `UNIT_FIELD_FLAGS`
        // gate handled above — and vmangos's `HandleModStun` grants both at once, which is why Ice
        // Block freezes completely while Frost Nova lets you turn.
        if player.modes.rooted {
            dir = Vec3::ZERO;
        }
        let moving = dir != Vec3::ZERO;
        // Stand state (decision 0080c) — a real field, not a local bool: X volunteers
        // `CMSG_STANDSTATECHANGE` (sit 1 ↔ stand 0) and movement input stands us up; the
        // server's echo into `UNIT_FIELD_BYTES_1` drives the pose — ours *and* every
        // observer's. `stand_pending` is the local commit (the client's `SetStandState`
        // applies immediately and sends, one setter — `0x6127b0`), overlaid on the echoed
        // byte until it lands so the pose never waits on the round-trip.
        let stand_byte = self_player
            .single()
            .ok()
            .and_then(|(.., store, _, _, _, _)| store.map(|s| s.0.unit_stand_state()))
            .unwrap_or(0);
        if player.stand_pending == Some(stand_byte) {
            player.stand_pending = None; // the echo landed
        }
        let stand_state = player.stand_pending.unwrap_or(stand_byte);
        // The queued asks first (the `/sit` family — decision 0881), then the X key, which is the
        // reference's own precedence: a queued `SetStandState` ran during the frame's message pass,
        // the key is read now. The last writer wins, and every one of them lands on the single
        // commit-and-send below.
        let mut request_stand = net.10.read().last().map(|r| r.state);
        if binds.fired(crate::bindings::cmd::SIT_OR_STAND) {
            request_stand = Some(u8::from(stand_state == 0));
        }
        // Any movement input stands the avatar back up (the client volunteers the stand — the
        // server never auto-stands a moving player; verified vmangos MovementHandler). The input
        // set is byte-pinned (wow-re `standstate-movement-trigger.md`, §5 2026-07-14): the net
        // input axes (translation), keyboard turn, and jump all reach the guarded stand wrapper
        // `0x60be30(0)`; a left-drag camera orbit provably does not; sit(1)/chair(2)/sleep(3)
        // all stand identically (the value-agnostic `GetStandState() != 0` gate). The one open
        // corner: no static path was found for a pure right-drag MOUSE turn while seated — the
        // director's ref observation (it stands you up) is the ground truth this keeps; the
        // byte trigger is flagged LIVE-CAPTURE in the wow-re note.
        let turned = turn_delta != 0.0 || mouse_turned;
        if (moving || turned || binds.fired(crate::bindings::cmd::JUMP))
            && stand_state != 0
            && request_stand.is_none()
        {
            request_stand = Some(0);
        }
        if let Some(s) = request_stand.filter(|&s| s != stand_state) {
            player.stand_pending = Some(s);
            let _ = net.0 .0.send(ClientCommand::StandStateChange {
                state: u32::from(s),
            });
            // The sit-stow rider (the client's SetStandState → SetSheatheState(0, SNAP) —
            // wow-re `sheath-policy.md` §4): entering any stand-state ∉ {0 STAND, 2 SIT_CHAIR}
            // force-stows drawn weapons, through the anim layer's one setter.
            if s != 0 && s != 2 {
                if let Ok((e, _, _, _, drv, _, _, _, _, _)) = self_player.single() {
                    if drv.and_then(|d| d.sheath_state()).unwrap_or(0) != 0 {
                        net.3.write(crate::creature_anim::SheathRequest {
                            entity: e,
                            state: 0,
                            ceremony: false,
                        });
                    }
                }
            }
        }
        let stand_now = player.stand_pending.unwrap_or(stand_byte);
        // Sheath toggle (Z) — vanilla's draw/stow, through the anim layer's ONE setter
        // ([`crate::creature_anim::SheathRequest`], decision 0080): walk the *committed*
        // client-side state (the setter cache — attacking auto-draws and the anim reconcile
        // force-stows, which a local bool or the raw echo byte would drift from), commit + send
        // `CMSG_SETSHEATHED` there, and play the ceremony — the manual toggle is the ONLY path
        // in the whole client that plays it (`bInstant = 0` at the 4 ToggleSheath sites — wow-re
        // `sheath-policy.md`). No body model yet (no driver) drops the toggle, the client's own
        // refusal.
        if binds.fired(crate::bindings::cmd::TOGGLE_SHEATH) {
            if let Ok((e, _, _, _, Some(drv), store, engaged, _, _, wielded)) = self_player.single()
            {
                // The manual toggle's guard chain (decision 0080d) — the guards of the client's
                // 12-deep silent-refusal chain (`ToggleSheath` `0x5eb480`) whose states exist
                // today: dead · engaged in combat · not standing (`GetStandState() != 0` —
                // chairs block the toggle too, unlike the *stow rider's* {0, 2} exemption) ·
                // mid-ceremony (the 89/90 clip still playing) · MOUNTED (chain check 4,
                // `UNIT_FIELD_MOUNTDISPLAYID > 0` — wow-re `sheath-policy.md` §2, wired with
                // 0441's mounts). Stunned / channeling join when those states exist. A refused
                // press is simply dropped — no message, like the client.
                let dead = store.is_some_and(|s| s.0.unit_is_dead());
                let mounted = store.is_some_and(|s| s.0.unit_mount_display_id() != 0);
                let refused =
                    dead || mounted || engaged || stand_now != 0 || drv.sheath_ceremony_active();
                if refused {
                    debug!(
                        "sheath toggle refused (dead {dead}, mounted {mounted}, engaged {engaged}, \
                         stand {stand_now}, mid-ceremony {})",
                        drv.sheath_ceremony_active()
                    );
                } else {
                    // The cycle proper ([`crate::creature_anim::toggle_sheath_next`], byte-read):
                    // melee → ranged → stowed, gated on what is actually worn — never a
                    // two-state flip. `None` = the ref makes no call at all (nothing equipped).
                    let w = wielded.copied().unwrap_or_default();
                    let worn = (w.main.is_some() || w.off.is_some(), w.ranged.is_some());
                    let next = crate::creature_anim::toggle_sheath_next(
                        drv.sheath_state().unwrap_or(0),
                        worn,
                    );
                    if let Some(state) = next {
                        net.3.write(crate::creature_anim::SheathRequest {
                            entity: e,
                            state,
                            ceremony: true,
                        });
                    }
                }
            }
        }
        // Backpedaling is slower: the backward move-flag selects the backward speed, dominating
        // strafe (binary-VERIFIED — see RUN_BACK_RATIO). Net-backward = the S key held without a
        // forward override (W or both-button run). The backward arm is a **min**, not a plain
        // select — `0x7c4d1d` computes `min(runBack, run)` (the swim §5's TU-H; observably the
        // plain runBack whenever it's the slower, i.e. always at vanilla values, but a server
        // that force-sets runBack above run is clamped like the ref). The resulting (slower)
        // speed also feeds jump takeoff, so a backward jump lands shorter for free.
        let net_backward = fwd_axis < 0;
        // Run/runback are server-authoritative (`UnitSpeeds`: seeded by our create's LIVING block,
        // updated live by SMSG_FORCE_*_SPEED_CHANGE — so `.modify speed`, mounts and slows actually
        // move us at the server's number). `$WOW_MOVE_SPEED` stays the absolute dev override
        // (backpedal keeps the vanilla 4.5/7.0 ratio under it); pre-create frames fall back the
        // same way.
        let server_speeds = self_player.single().ok().and_then(|q| q.7).map(|s| s.0);
        let (run_speed, run_back_speed) = match server_speeds {
            Some(s) if !move_speed.env_override => (s.run, s.run_back),
            _ => (move_speed.value, move_speed.value * RUN_BACK_RATIO),
        };
        let speed = if net_backward {
            run_back_speed.min(run_speed)
        } else {
            run_speed
        };
        let mut want_jump = binds.fired(crate::bindings::cmd::JUMP) && !player.modes.rooted;

        // Swim vs walk: the water over our feet decides. Hysteresis-latched (`update_swimming`,
        // the verified `0x6030c0` boundary — B7 resolved, decision 0226) so wading the line
        // doesn't flicker between the two physics regimes.
        let surface_y = swim::surface_over_feet(world, player.pos);
        let swimming = swim::update_swimming(&mut player, surface_y, time.elapsed_secs());
        if let Some(surface) = surface_y {
            move_trace::swim(player.pos.y, surface, swimming, player.collision_height.0);
        }
        // Space while swimming = the ref's Jump routing (decision 0487, superseding 0479),
        // fired on the PRESS EDGE only — one hop per press, a held key does not re-fire
        // (decision 0498, director-verified on the ref; 0487's held-chaining was our
        // over-extension of TU-F, and near the surface its re-latch→re-fire loop bounced the
        // avatar under the waterline — the "invisible wall"). VERIFIED TU-F/TU-G (`0x7c6230`):
        // the routing has no depth gate and no swim re-route — at the surface the press
        // breaches out; submerged it's the ~1.6-yd dolphin-hop, re-latching into swim once the
        // launch velocity halves (`0x7c5de0`). The smooth way UP is aiming up in mouselook and
        // swimming forward (the 0492 pitch law). The breach exits the water mode INSIDE this
        // frame — the byte handler runs before the mover, clearing SWIMMING unconditionally —
        // so the latch drops now and this frame's mover, flags, and wire all see the leap as a
        // jump.
        let breach = swimming && want_jump;
        if breach {
            player.swimming = false;
        }
        let swimming = swimming && !breach;

        // The swim translation amounts — read by the swim mover arm AND the flag build, so the
        // two can never disagree (decision 0056: the flags mirror the avatar's motion). W/S,
        // strafe Q/E (+ mouselook A/D).
        let mut swim_fwd = 0.0_f32;
        let mut swim_side = 0.0_f32; // +right
        if swimming {
            swim_fwd += fwd_axis.signum() as f32;
            if strafe_right {
                swim_side += 1.0;
            }
            if strafe_left {
                swim_side -= 1.0;
            }
            if mouselook {
                if turn_right {
                    swim_side += 1.0;
                }
                if turn_left {
                    swim_side -= 1.0;
                }
            }
            // Rooted kills swim translation like the walk `dir` above (decision 0308's regime —
            // the water arm reads raw keys, so it needs its own cut).
            if player.modes.rooted {
                swim_fwd = 0.0;
                swim_side = 0.0;
            }
        }

        // The mounted space-bar flourish (decision 0441 P2). The gate is byte-VERIFIED — the
        // client's jump-key handler `0x60dea0` (wow-re `mount-composition.md` Q3): mounted +
        // no translational move + not turning + grounded → play MountSpecial(94) locally FIRST,
        // then send `CMSG_MOUNTSPECIAL_ANIM` (the receive side self-suppresses the echo, see
        // `net/apply.rs`); translational move → a real jump, the unmounted path; **turn-only
        // (the `0x30` turn flags) → a silent no-op** — the press is consumed, nothing plays;
        // airborne → silent no-op (the client's geometric ground-clearance test `0x605650`;
        // our airborne arc stands in — an airborne press falls through and the mover ignores
        // it, the same net silence). Swim disposition is INFERRED-moot (you can't be mounted
        // while swimming in 1.12); a swimming Space is the jump-exit above — and only that
        // (TU-F: Space is the Jump command; it is NOT a pitch or ascend input) — and never
        // reaches this walk-side gate.
        if want_jump && !moving && !swimming && player.airborne_since.is_none() {
            if let Ok((e, .., store, _, _, _, _)) = self_player.single() {
                if store.is_some_and(|s| s.0.unit_mount_display_id() != 0) {
                    want_jump = false;
                    if !turning {
                        let _ = net.0 .0.send(ClientCommand::MountSpecial);
                        net.8.write(crate::creature_anim::MountFlourish { unit: e });
                    }
                }
            }
        }

        // This frame's PRESENTED swim pitch — the persistent [`Player::swim_pitch`] while swimming
        // (held even idle, the client's `CMovement+0x20`), except leveled by the 0499 surface
        // redirect when the rest-line cap bites. Feeds the body pose and the wire pitch tail (one
        // source — the pose and the stream can't disagree); the tail only serializes with the
        // SWIMMING flag, so the walking value is inert.
        let mut swim_pitch = 0.0_f32;
        // The ground height the mover starts this frame at (pre-step feet Y). For a jump this is the
        // true takeoff height — the mover integrates one jump-tick upward *within* the step, so the
        // post-step `pos.y` is already ~0.13 yd (60 fps) above the ground and must not be used as
        // the launch height (see [`Player::advance_airborne_arc`]).
        let launch_y = player.pos.y;
        let mover::Outcome {
            held,
            grounded,
            jumped,
            air_nudged,
            ground,
        } = if breach {
            // Jump while swimming (**VERIFIED**, wow-re `swim-mechanism.md` TU-B(f)+TU-F,
            // `0x7c6230`): clears SWIMMING and enters the FALLING lifecycle *unconditionally* —
            // no swim re-route, no surface-proximity gate — seeding a take-off ~14% over a land
            // jump. At the surface this is the jump-out hop (the leap clears the water and can
            // carry onto a low bank); deep, it's the ~1.6-yd dolphin-hop — swim re-latches once
            // the upward velocity halves (`update_swimming`'s verified `0x7c5de0` gate). The wire
            // streams it as a normal JUMP: fall clock 0, the seeded zspeed in the tail —
            // `advance_airborne_arc` below snapshots it like any land jump.
            swim::breach_step(&mut player, &time, &collide, capsule)
        } else if swimming {
            // The swim pitch: HELD when unsteered (VERIFIED TU-B(c) — an idle floater keeps its
            // pitch, never auto-levels), and steered by mouselook as a DIRECT set of the camera
            // aim — **VERIFIED** (the camera-pitch §5, wow-re `swim-camera-pitch.md`, decision
            // 0492, closing 0488's INTERIM and refuting the earlier no-camera-coupling census):
            // the ref's mouse-move chain ends in `SetPitch 0x7c6f70`, an unconditional store —
            // no integrator, no rate limit — clamped ±89° ([`MOUSELOOK_PITCH_CLAMP`], the byte
            // constant; the ±π/2 clamp belongs to the unbound pitch-KEY integrator), with the
            // velocity basis rebuilt in-call: the aim re-points travel the same frame, zero
            // lag. (The ref's `fchs` negate is its own camera sign convention; ours maps aim-up
            // to pitch-up already.) A left-drag camera orbit steers NOTHING — it moves the
            // camera without turning the character (the walk rule at `move_fwd` above), so it
            // must not bend the swim either (director-reported, 2026-07-18).
            if mouselook {
                player.swim_pitch = cam
                    .pitch
                    .clamp(-MOUSELOOK_PITCH_CLAMP, MOUSELOOK_PITCH_CLAMP);
            }
            // The drunk porpoise (B210): while swimming and moving, the pitch increments by the
            // wobble ×4.0 every frame (`0x60aabc–0x60ab0a`: flag `0x200000` → `pitch +
            // wobble·[0x80306c]`, clamped, committed via the pitch pipeline `0x60de70`). Same
            // wobble as the facing veer above, so the nose and the heading meander together.
            // The clamp is the callee `0x60aba0`'s FIXED ±π/2 bounds (`0x808acc`/`0x80c5e4`),
            // NOT the mouselook set's ±89° — the reference carries both (decision 1009 §C4).
            if drunk_wobble != 0.0 && translating {
                player.swim_pitch = (player.swim_pitch
                    + drunk_wobble * drunk::SWIM_PITCH_WOBBLE_SCALE)
                    .clamp(-std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2);
            }
            swim_pitch = player.swim_pitch;
            // The travel basis (`0x7c5880`, the client's swim velocity direction): the FORWARD axis
            // is the facing pitched by the swim pitch — `(cosP·horiz-fwd + sinP·up)` — so holding W
            // with the nose down dives (and aimed up, climbs — the smooth ascend, like the
            // ref's PitchUp+Forward); the STRAFE axis stays level. There is no vertical
            // thruster and Space adds nothing here (the verified basis has no separate vertical
            // input; Space's whole swim role is the jump-exit above).
            let (sp, cp) = player.swim_pitch.sin_cos();
            let fwd_axis = move_fwd * cp + Vec3::Y * sp;
            let v = fwd_axis * swim_fwd + move_right * swim_side;
            let dir3 = v.normalize_or_zero();
            // Whether the water owns our vertical at all this frame, and where its line sits — the
            // one source both arms of the constraint read (`swim::rest_line`; `None` is GM flight).
            let rest_line = swim::rest_line(&player, surface_y);
            // Directional swim speed — **VERIFIED** (`0x7c4c90`'s swim arm, the §5's TU-H):
            // forward or strafe-only → swim; the backward bit `0x2` → `min(swimBack, swim)` —
            // byte-identical in template to the run arm's `min(runBack, run)`. Vanilla defaults
            // 4.722/2.5 (vmangos `baseMoveSpeed`).
            let (swim_speed, swim_back_speed) = match server_speeds {
                Some(s) if !move_speed.env_override => (s.swim, s.swim_back),
                _ => (swim::SWIM_SPEED, swim::SWIM_BACK_SPEED),
            };
            let dir_speed = if swim_fwd < 0.0 {
                swim_back_speed.min(swim_speed)
            } else {
                swim_speed
            };
            // The stroke's playback-rate numerator is the FLAG-scalar speed — the full
            // directional speed regardless of pitch, 0 with no translation input — never a
            // horizontal projection, which would starve a pitched stroke toward a freeze.
            // **VERIFIED** (TU-I): `0x5fe2f0` divides GetCurrentSpeed (flags + static speed
            // fields only) by the clip's moveSpeed, the same path for local and observed units.
            player.swim_stroke_speed = if dir3 == Vec3::ZERO { 0.0 } else { dir_speed };
            let out = swim::swim_step(
                &mut player,
                &time,
                &collide,
                capsule,
                dir3 * dir_speed,
                rest_line,
                |feet| swim::surface_over_feet(world, feet),
            );
            // The surface redirect (decisions 0499+0505 — a NAMED DIVERGENCE, see
            // `swim::cap_redirect`): when the rise capped at the rest line, the stroke went
            // level at full speed — present the *effective* pitch (body pose + wire tail
            // follow the motion, →0 pinned at the line), while the raw aim stays in
            // `player.swim_pitch` so a later nose-down dives instantly.
            if let Some(p) = out.surface_pitch {
                swim_pitch = p;
            }
            mover::Outcome {
                held: false,
                grounded: out.grounded,
                jumped: false,
                air_nudged: false,
                ground: None, // swimming detaches from any platform frame below
            }
        } else {
            // The kinematic mover step — walk/fall physics + the step-down snap (decisions
            // 0009/0182/0190); the mechanism lives in [`mover`].
            // **Water walking** (decision 0866): hand the mover the liquid surface as a floor. The
            // `!swimming` half is the reference's own gate — the water-walk arm at `0x63160d` is
            // skipped when `MOVEFLAG_SWIMMING` is set (`0x631617`) — so granting the aura to a
            // submerged caster does not eject them; they surface onto the water on the way out.
            // (This branch is already the non-swimming one, so `swimming` is false here; the
            // condition is written out because the gate is the mechanism, not an accident of
            // control flow.)
            let water_floor = (player.modes.water_walking && !swimming)
                .then_some(surface_y)
                .flatten();
            mover::step(
                &mut player,
                &time,
                &collide,
                capsule,
                moving,
                dir,
                speed,
                want_jump,
                water_floor,
            )
        };

        let now = time.elapsed_secs();
        // Airborne is a walk-only concept — swimming never falls, so the body-heading / anim-flags
        // logic below reads this hoisted value (false while swimming) instead of the walk branch's.
        //
        // **A root ends the arc outright** (decision 0880): `airborne` is our `MOVEFLAG_FALLING`,
        // and `SetRoot 0x7c7340`'s second act is `StopFalling 0x7c6290`, which clears FALLING and
        // FALLINGFAR together. So a root or a stun taken mid-air is not a body that lands — it is a
        // body that is no longer falling, hanging where it was ([`mover::step`]'s anchor holds the
        // position). Ground contact is left honest in `grounded` (the transport attach and the trace
        // both want the truth); it is the *arc* that the root ends.
        let airborne = !swimming && !held && !player.modes.rooted && (!grounded || jumped);
        // Transport attach/detach (decision 0438 phase 2). Attach when the walkable support is a
        // transport's collider — the boat's own hull, OR a deck prop's collider child (solid
        // cargo, 0470): the walk resolves the support upward through the parent chain to the
        // Transport that owns it, so standing on a crate is standing on the boat. Detach when
        // support resolves to world geometry or we enter the water. Airborne keeps the current
        // attachment — the carry above keeps composing, so a jump above the deck is deck-frame
        // ballistics and lands where it took off (jumping off the side detaches at whatever it
        // lands on). Then re-snapshot the local pose from this frame's FINAL world pose against
        // the boat's (unchanged-this-frame) transform, which is what next frame's carry
        // recomposes from.
        let owning_transport = |mut e: Entity| {
            for _ in 0..4 {
                if let Ok((t, g)) = transports.get(e) {
                    return Some((e, t, g));
                }
                e = child_of.get(e).ok()?.parent();
            }
            None
        };
        if swimming {
            if player.ride.take().is_some() {
                info!("transport: deboard (entered the water)");
            }
        } else if grounded {
            match ground.and_then(owning_transport) {
                Some((entity, _, guid)) => {
                    if player.ride.as_ref().map(|r| r.entity) != Some(entity) {
                        info!("transport: board {:#x} (support is its deck)", guid.0);
                    }
                    player.ride = Some(PlayerRide {
                        entity,
                        guid: guid.0,
                        local_pos: Vec3::ZERO, // filled by the snapshot just below
                        boat_yaw: 0.0,
                    });
                }
                None => {
                    if player.ride.take().is_some() {
                        info!("transport: deboard (support is world geometry)");
                    }
                }
            }
        }
        let feet = player.pos;
        // **The ride trace** (`WOW_MOVE_TRACE_TAGS=ride`) — one line per frame while attached, plus
        // the frame after a detach, because "what happened on the boat" is otherwise unanswerable:
        // the deck's own motion is in the boat's transform, the rider's in world space, and the
        // difference between them is the only thing that says whether the carry composed. The
        // director's report — *stepped off a ledge on a boat and it threw me back across the boat
        // until I landed* — is a statement about the DECK-relative path, which no other instrument
        // here records.
        if benilla_assets::trace::enabled_for("ride") {
            let boat_pose = player
                .ride
                .as_ref()
                .and_then(|r| transports.get(r.entity).ok())
                .map(|(t, _)| (t.translation, t.rotation.to_euler(EulerRot::YXZ).0));
            if let (Some(ride), Some((bpos, byaw))) = (player.ride.as_ref(), boat_pose) {
                let local = Quat::from_euler(EulerRot::YXZ, byaw, 0.0, 0.0)
                    .inverse()
                    .mul_vec3(feet - bpos);
                benilla_assets::trace::line(
                    "ride",
                    &format!(
                        "on {:#x} deck({:8.2},{:7.2},{:8.2}) yaw{:+.3} | feet({:8.2},{:7.2},{:8.2})                          local({:7.2},{:6.2},{:7.2}) | grounded={} support={} vy={:+6.2}",
                        ride.guid,
                        bpos.x,
                        bpos.y,
                        bpos.z,
                        byaw,
                        feet.x,
                        feet.y,
                        feet.z,
                        local.x,
                        local.y,
                        local.z,
                        grounded as u8,
                        match ground.and_then(owning_transport) {
                            Some(_) => "deck",
                            None if ground.is_some() => "world",
                            None => "NONE",
                        },
                        player.vel_y,
                    ),
                );
            } else if !swimming {
                // Not riding: only worth a line when something under us *is* a transport, i.e. the
                // frames where an attach should have happened and did not.
                if let Some((_, _, guid)) = ground.and_then(owning_transport) {
                    benilla_assets::trace::line(
                        "ride",
                        &format!(
                            "OFF but standing on {:#x} at ({:8.2},{:7.2},{:8.2}) grounded={}",
                            guid.0, feet.x, feet.y, feet.z, grounded as u8
                        ),
                    );
                }
            }
        }
        if let Some(ride) = player.ride.as_mut() {
            if let Ok((boat, _)) = transports.get(ride.entity) {
                ride.local_pos = boat.compute_affine().inverse().transform_point3(feet);
                ride.boat_yaw = boat.rotation.to_euler(EulerRot::YXZ).0;
            }
        }
        // The wire fall clock (ms since the airborne arc began), snapshotted HERE — before the arc
        // bookkeeping below clears `airborne_since` on the landing frame — so the MSG_MOVE_FALL_LAND
        // reports the *accumulated* fall time. vmangos `Player::HandleFall` gates fall damage on the
        // land packet's fallTime ≥ 1229 ms (the free-fall time of the 14.57-yd damage threshold); a
        // clock zeroed by the landing silently disables fall damage. The takeoff frame still sends 0
        // (`airborne_since` is not yet set at this point in that frame).
        let wire_fall_time = if jumped {
            // A jump launch starts a fresh arc — its fall clock is zero. This also covers a
            // same-frame land+relaunch, where `airborne_since` still holds the *previous* arc's
            // start; without this the bounce's JUMP would carry a stale (accumulated) fall time,
            // and a long spam-jump chain could spuriously cross the server's fall-damage gate.
            0
        } else {
            player
                .airborne_since
                .map_or(0, |t0| ((now - t0) * 1000.0).max(0.0) as u32)
        };
        // The CMovement move-flags this frame's input implies. The same bitset drives our avatar's
        // animation (below) *and* the movement stream we send the server (further down), so the two can
        // never disagree. Direction bits mirror the client's MOVEMENTFLAGS; FALLING marks the airborne
        // arc (animation-only — it is masked off before going on the wire, see the send block).
        // **Every granted mover mode rides every packet**, in or out of the water — the reference's
        // builder reads the one `[cmov+0x40]` the server's merge wrote them into, so it echoes back
        // whatever was granted for free (decisions 0726, 0866). Ours has to put them back
        // explicitly, because this word is rebuilt from state each frame; drop one and the server
        // forgets the mode, then the next server-authored move echoes a mode-less word back and
        // clears it under us. Root rides too — moving bits are what must not accompany it, and
        // rooted input can't produce any (`dir` is zeroed above, jumps refused).
        let mut move_flags_now = player.modes.wire_flags();
        // `landed`/`started_falling` gate the wire's jump/fall lifecycle; the swim branch never sets
        // them (leaving the water resumes the ground mover from rest, no airborne report).
        let landed;
        let started_falling;
        if swimming {
            // Swimming: `MOVEFLAG_SWIMMING` (the swim-pitch tail rides with it) plus the travel-direction
            // bits the swim gait selector cascades on (TU-E: turn→41, strafe→43/44, back→45, fwd→42,
            // idle→41). The bits mirror the NET swim amounts that actually drive the mover — one
            // source, so a rooted or key-cancelled swimmer can't stream a phantom direction
            // (decision 0056). Space sets nothing here — its whole swim role is the jump-exit,
            // which runs the breach arm above (TU-F). No FALLING, no airborne bookkeeping: the
            // arc state is cleared so leaving the water starts a clean walk/fall from rest.
            move_flags_now |= move_flags::SWIMMING;
            if swim_fwd < 0.0 {
                move_flags_now |= move_flags::BACKWARD;
            } else if swim_fwd > 0.0 {
                move_flags_now |= move_flags::FORWARD;
            }
            if swim_side < 0.0 {
                move_flags_now |= move_flags::STRAFE_LEFT;
            } else if swim_side > 0.0 {
                move_flags_now |= move_flags::STRAFE_RIGHT;
            }
            player.airborne_since = None;
            player.fall_far = false;
            landed = false;
            started_falling = false;
        } else {
            // Straight off the net axis, so a netted-to-zero press pair streams NO direction bit
            // (the emitter's genuine STOP) rather than a phantom FORWARD we aren't actually moving
            // in — decision 0056's law that the flags mirror the avatar's motion.
            match fwd_axis.signum() {
                1 => move_flags_now |= move_flags::FORWARD,
                -1 => move_flags_now |= move_flags::BACKWARD,
                _ => {}
            }
            // Straight off the netted strafe axis, so a cancelled press pair streams NO strafe bit —
            // the two are mutually exclusive on the wire, and both-set is silently dropped by the
            // server (decision 0622).
            match side_axis.signum() {
                -1 => move_flags_now |= move_flags::STRAFE_LEFT,
                1 => move_flags_now |= move_flags::STRAFE_RIGHT,
                _ => {}
            }
            if !mouselook {
                if binds.pressed(crate::bindings::cmd::TURN_LEFT) {
                    move_flags_now |= move_flags::TURN_LEFT;
                }
                if binds.pressed(crate::bindings::cmd::TURN_RIGHT) {
                    move_flags_now |= move_flags::TURN_RIGHT;
                }
            }
            // Airborne (a jump or a step-off a ledge) — the hoisted value above. The arc's
            // snapshot / far-latch / landing edges live in [`Player::advance_airborne_arc`] (a
            // fresh jump is always a NEW arc, even a same-frame land+relaunch — see there). FALLING
            // also rides the wire (decision 0053), so observers replay it.
            let arc = player.advance_airborne_arc(airborne, jumped, now, launch_y);
            landed = arc.landed;
            started_falling = arc.started_falling;
            if airborne {
                move_flags_now |= move_flags::FALLING;
                // Mid-air the direction flags stay LIVE — the real client's `CMovement+0x40` keeps
                // tracking the keys while airborne, and the wire proves it (VERIFIED, vanilla-sniffs
                // `dwarf_rogue_dun_morogh`: a strafe pressed mid-air rides the landing FALL_LAND as
                // `(Forward, StrafeLeft)`; an S→W swap mid-air lands as `(Forward)`). What's frozen
                // at takeoff is the *velocity basis* (the mover's momentum — `0x7c5a20` skips the
                // basis recompute while FALLING), never the reported state; the landing-anim pick
                // (`jump_land_pick`, the ref's `0x602c60`) keys on the flags *at touchdown*, so a
                // frozen wire strands observers on stale flags and they play a locomotion anim
                // instead of the landing. The ANIM path keeps the takeoff-frozen dirs (`pose_flags`
                // below — the RE'd step-off gait freeze); a new arc (re)seeds them, and the
                // standstill air nudge is the one mid-arc input that really moves us.
                if arc.new_arc || air_nudged {
                    player.airborne_dirs = move_flags_now & move_flags::ANY_MOVE;
                }
                // FALLINGFAR (latched by `advance_airborne_arc` above — the exclusive distance/timer
                // legs, decision 0179) rides the live flags: the mid-air Fall(40) pose, the
                // landing-anim gate, and the wire (heartbeats carry it; the axis differ ignores it).
                if player.fall_far {
                    move_flags_now |= move_flags::FALLING_FAR;
                }
            }
            // While `held` (post-teleport/login settle) the avatar is frozen in place with gravity off,
            // so it has no locomotion to report — clear the flags so we never stream a phantom walk/turn
            // the server would extrapolate onto observers while we sit on the settle. The frozen position
            // was already reported by the teleport Stop; a facing change still streams a harmless
            // SET_FACING below. The same bitset drives the local animation (0052), so this also keeps the
            // held avatar idle rather than moonwalking in place. (Decision 0056 — the wire mirrors the
            // avatar's actual motion.)
            if held {
                move_flags_now = 0;
            }
        }
        // The two incapacitate suppressions — rooted drops the direction bits, stunned drops the
        // turn bits — applied to the whole word in one place, whichever branch built it, and with
        // the reference's byte trail in [`state::incapacitated_flags`] (decision 0880).
        move_flags_now = state::incapacitated_flags(move_flags_now, player.modes.rooted, stunned);
        // Riding a transport: the ON_TRANSPORT bit rides every packet with its local-pose tail
        // (built at the send below). Set from the POST-attach state so flag and tail agree the
        // very frame we board or step off (decision 0438 phase 2).
        if player.ride.is_some() && !held {
            move_flags_now |= move_flags::ON_TRANSPORT;
        }

        // The animation/body-pose view of the flags: airborne it keeps the TAKEOFF-FROZEN direction
        // bits — the reference's anim layer plays the step-off gait off the takeoff-frozen
        // flags/speed until FALLINGFAR latches or the unit lands (wow-re `land-anim-height-gate.md`),
        // and a mid-air Q press must not twist the body or animate a strafe. The *wire* flags above
        // stay live (the sniff-verified send law); only the pose reads the freeze.
        let pose_flags = if airborne {
            (move_flags_now & !move_flags::ANY_MOVE) | player.airborne_dirs
        } else {
            move_flags_now
        };
        // The rendered body heading + the animation's view of the flags — the display-facing law
        // lives in [`gait::drive_body_heading`] (strafe offset ease / moving snap / the standing
        // FROZEN chase whose body-step latches the turn-in-place shuffle).
        let anim_flags = gait::drive_body_heading(
            &mut player,
            pose_flags,
            dt,
            swimming,
            moving,
            airborne,
            turning || mouselook,
        );

        // Drive the streamed self entity: its transform is the avatar's pose (feet position + body
        // heading, like every other streamed unit), and its `MovementState` is the live movement the
        // animation selector reads. Scale is left untouched (the renderer baked the display scale on).
        // `horiz_vel` is already the directional speed (runBack when backpedaling), so the backpedal
        // clip scales by it and no longer drags.
        // World camera-pivot height: `H = 0.9·bbox_z_extent·scale`, floored (see [`CameraPivot`] /
        // `CAM_PIVOT_FLOOR`). Read the avatar's model-local pivot + its live scale here (where we hold the
        // self entity); until the body attaches (no `CameraPivot` yet) fall back to a human neck height.
        let mut cam_pivot_height = CAM_PIVOT_FALLBACK;
        if let Ok((entity, mut t, motion, pivot, .., twist, _)) = self_player.single_mut() {
            t.translation = player.pos;
            // The swim body pitch (TU-A, `0x60a110`→`0x710620`): while swimming AND moving fwd/back
            // the model root renders `Rz(yaw)·Ry(−pitch)` — in Bevy axes, the yaw then a nose-up
            // pitch about the body's local X. Strafe-only, idle, and grounded all render LEVEL (the
            // ground path) — exactly the gate the client's per-frame `+0x3c` sync branches on.
            // The pitch presented is this frame's `swim_pitch` — the raw aim, except leveled by
            // the 0499 surface redirect when the rest-line cap bites (the body swims flat along
            // the surface, not pitched against it); the wire tail streams the same value.
            t.rotation =
                if swimming && move_flags_now & (move_flags::FORWARD | move_flags::BACKWARD) != 0 {
                    Quat::from_rotation_y(player.model_yaw) * Quat::from_rotation_x(swim_pitch)
                } else {
                    Quat::from_rotation_y(player.model_yaw)
                };
            // Report every landing's fall height for the client-side landing predictor
            // (`0x602d00`, decision 0412): its consumers gate on the descent and, past the HARD
            // floor, play the wound grunt + a locally-predicted dust puff at THIS frame — the
            // server's 0x1FC echo arrives ~an RTT later (the reference double-fires the dust the
            // same way). `fall_start_y` still holds this arc's launch height here (it is only
            // re-seeded at the next take-off).
            if landed {
                net.6.write(crate::creature_anim::HardLanding {
                    entity,
                    descent: player.fall_start_y - player.pos.y,
                });
            }
            cam_pivot_height = head_height(pivot, t.scale.x);
            if let Some(mut motion) = motion {
                // A swimmer's stroke rate takes the flag-scalar directional speed (full rate at
                // any pitch — a vertical climb must not freeze the stroke); the ground gaits
                // scale by the achieved horizontal speed as before.
                motion.speed = if swimming {
                    player.swim_stroke_speed
                } else {
                    player.horiz_vel.length()
                };
                motion.vertical_speed = player.vel_y;
                motion.flags = anim_flags;
                motion.stand_state = stand_now;
            }
            // The counter-twist gap: how far the aim sits from the rendered body — the strafe
            // offset while it lasts, unwinding to zero as `model_yaw` closes on `face_yaw`.
            if let Some(mut twist) = twist {
                twist.yaw_gap = wrap_pi(player.face_yaw - player.model_yaw);
            }
        }

        // `WOW_CAM_DUMP`: the per-frame INPUT signal beside `seat_camera`'s realized-pose `[cam]`
        // line — wall clock, frame dt, this frame's accumulated mouse delta, the active look mode,
        // and the yaw/pos the frame produced. A turn-feel question ("keyboard turn smooth, mouse
        // turn jittery") needs the input cadence and the output cadence on the same timeline: a
        // bursty `dx` under a steady `dt` convicts event delivery; a steady `dx` with an uneven
        // realized pose convicts everything downstream.
        if std::env::var_os("WOW_CAM_DUMP").is_some() {
            eprintln!(
                "[turn] t={:.6} dt={:.6} dx={:.3} dy={:.3} look={} face={:.6} model={:.6} \
                 pos [{:.4},{:.4},{:.4}]",
                time.elapsed_secs_f64(),
                dt,
                mouse_motion.delta.x,
                mouse_motion.delta.y,
                match rig.look {
                    Some(LookButton::Right) => "R",
                    Some(LookButton::Left) => "L",
                    None => "-",
                },
                player.face_yaw,
                player.model_yaw,
                player.pos.x,
                player.pos.y,
                player.pos.z,
            );
        }

        // The camera-collision sweep is rooted at the *head* (capsule top hemisphere centre), not the
        // framing pivot — see `seat_camera`'s doc for why. Computed here (not in `camera`) because it
        // depends on the avatar's own capsule constants, which are a movement concern.
        let head = player.pos + Vec3::Y * (CAPSULE_HEIGHT - CAPSULE_RADIUS);
        // `turn_delta` is the character's own turn this frame (keyboard turn, or the drunk veer)
        // — the deck's yaw delta was already applied to `cam.yaw` at the ride block (frame motion
        // carries the camera unconditionally; only input turns respect `seat_camera`'s
        // look-session gate).
        seat_camera(
            dt,
            turn_delta,
            player.pos,
            head,
            cam_pivot_height,
            &mut rig,
            &mut cam,
            &mut cam_t,
            &collide,
            cam_probe,
        );

        // The cast bar's local self-cancel trigger (`ui_cast::local_self_cancel`): a fresh
        // *directional* start (the same wire-axis edge the stream below turns into a
        // MSG_MOVE_START_*; diffed against the pre-stream `player.move_flags`) or a jump launch.
        // Turn-in-place and pitch deliberately absent — VERIFIED (wow-re `move-selfcancel.md`,
        // 0445): the client's interrupt mask `0x10f0` is {fwd, back, strafe L/R, autorun};
        // turn/pitch flags sit outside it and never cancel.
        // `autorun_armed` is 0445's dormant fifth mask member waking up — the `0x1000` bit IS in the
        // verified `0x10f0` interrupt mask, but **only on the ON edge**: `ToggleAutoRun` computes its
        // `setBool` as the new state, and the dispatcher short-circuits the whole interrupt block on a
        // clear edge (`0x5150c8`) *before* the mask is tested. So arming autorun kills a cast;
        // disarming it does not. It needs its own term because the flag-delta test above can't see it —
        // toggling autorun on with W already held raises no new direction bit (VERIFIED wire-silence),
        // yet the reference still cancels. (0445's row says "YES" unqualified; wow-re RF-0079 §5
        // corrects it to the ON edge.)
        if move_flags_now & move_flags::ANY_MOVE & !player.move_flags != 0
            || jumped
            || autorun_armed
        {
            net.7 .0 = true;
        }

        // Stream this frame's movement to the server — a `MSG_MOVE_*` per movement-axis transition, the
        // jump/fall lifecycle, and a ~500 ms heartbeat, each carrying the live `MovementInfo` (decisions
        // 0052 + 0053). vmangos relays it to nearby players, who extrapolate from the flags. See the
        // [`movement_net`] module (the outbound mirror of `net::motion`'s remote integration).
        // The rider's local pose for the wire's ON_TRANSPORT tail: `bevy_to_wow` is a pure basis
        // rotation, so the boat-local Bevy vector converts directly, and the local orientation is
        // `face_yaw − boat_yaw` (the GetAbsoluteFacing law in reverse), normalized like any wire
        // orientation.
        let wire_transport = player.ride.as_ref().map(|r| {
            let local = benilla_assets::coords::bevy_to_wow(r.local_pos);
            benilla_protocol::TransportPose {
                guid: r.guid,
                pos: benilla_protocol::wire::Vector3d {
                    x: local[0],
                    y: local[1],
                    z: local[2],
                },
                orientation: (player.face_yaw - r.boat_yaw).rem_euclid(std::f32::consts::TAU),
            }
        });
        movement_net::stream_self_movement(
            &net.0 .0,
            &mut player,
            move_flags_now,
            swim_pitch,
            movement_net::ArcEdges {
                jumped,
                air_nudged,
                landed,
                started_falling,
                fall_time: wire_fall_time,
            },
            now,
            &speed_acks,
            wire_transport,
        );
    } else {
        // Free fly (pre-connect or detached): aim from the look angles, move the camera directly
        // ([`camera::fly_free`]). If we just detached mid-move, the controlled branch above (which
        // owns the per-frame movement stream) has stopped running with our last move-flags still
        // live on the wire — park the mover so the server clears them, else observers extrapolate a
        // phantom walk/spin until we re-attach. No-op pre-connect / once already stopped (decision
        // 0056). The avatar stays frozen at `player.pos`.
        movement_net::park_mover(&net.0 .0, &mut player);
        camera::fly_free(dt, &keys, typing, &mut rig, &mut cam, &mut cam_t);
    }
}
