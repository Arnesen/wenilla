//! Unit tests for the pure motion kernels — the spline sampler, the dead-reckoning integrator,
//! the jump ballistics, and the facing turn (each child module's math, exercised together here
//! like [`super`]'s original single-file block).

use std::time::{Duration, Instant};

use benilla_protocol::{JumpInfo, MonsterMoveFacing, MoveSpeeds};

use crate::creature_anim::move_flags;
use crate::player::{GRAVITY, TERMINAL_VELOCITY};

use super::facing::{resolve_facing, turn_toward};
use super::remote::{facing_lerp, fall_arc_step, jump_seed, reconcile_lerp};
use super::spline::monster_move_spline;
use super::{RelayClock, RemoteMotion, Spline};

fn speeds() -> MoveSpeeds {
    MoveSpeeds {
        walk: 2.5,
        run: 7.0,
        run_back: 4.5,
        swim: 4.0,
        swim_back: 0.0,
        turn_rate: std::f32::consts::PI,
    }
}

#[test]
fn remote_fall_arc_reports_height_only_on_the_landing_edge() {
    // Takeoff (grounded → FALLING): snapshot this Z, report nothing yet.
    assert_eq!(fall_arc_step(false, true, None, 100.0), (Some(100.0), None));
    // Still airborne (FALLING → FALLING): hold the takeoff Z, still nothing.
    assert_eq!(
        fall_arc_step(true, true, Some(100.0), 80.0),
        (Some(100.0), None)
    );
    // Landing (FALLING → grounded) with a known takeoff: report the fall height (WoW Z up, so
    // takeoff − landing), and clear the reference.
    assert_eq!(
        fall_arc_step(true, false, Some(100.0), 70.0),
        (None, Some(30.0))
    );
    // Landing after entering view mid-fall (no takeoff seen): no height reference → no prediction.
    assert_eq!(fall_arc_step(true, false, None, 70.0), (None, None));
    // Grounded → grounded: nothing tracked, nothing reported.
    assert_eq!(fall_arc_step(false, false, None, 70.0), (None, None));
}

fn motion(flags: u32, orientation: f32) -> RemoteMotion {
    RemoteMotion {
        wow_pos: [0.0, 0.0, 0.0],
        pending: std::collections::VecDeque::new(),
        orientation,
        flags,
        pitch: 0.0,
        speed: 0.0,
        vertical_velocity: 0.0,
        jump_xy_vel: [0.0, 0.0],
        fall_start_z: None,
    }
}

#[test]
fn swim_dead_reckon_folds_the_pitch_into_the_travel() {
    // A swimmer's wire pitch folds into the travel direction the way the client's swim velocity
    // basis does (`0x7c5880`): vertical sin(pitch)·swim speed, horizontal scaled by cos(pitch).
    let pitch = 0.5_f32;
    let mut rm = motion(move_flags::SWIMMING | move_flags::FORWARD, 0.0);
    rm.pitch = pitch;
    let (pos, _, vertical, speed) = rm.advance(speeds(), 1.0);
    // Facing 0 = WoW +X; swim speed 4.0 for 1 s.
    assert!(
        (pos[0] - 4.0 * pitch.cos()).abs() < 1e-4,
        "horizontal shrinks by cos(pitch): {}",
        pos[0]
    );
    assert!(
        (pos[2] - 4.0 * pitch.sin()).abs() < 1e-4,
        "the dive/climb is sin(pitch)·speed: {}",
        pos[2]
    );
    assert_eq!(
        vertical, 0.0,
        "no ballistic vertical persists for a swimmer"
    );
    assert!((speed - 4.0).abs() < 1e-5, "anim rate reads the 3D speed");

    // Level swim (pitch 0) stays flat; an idle floater (no direction bits) doesn't drift.
    let level = motion(move_flags::SWIMMING | move_flags::FORWARD, 0.0);
    let (pos, ..) = level.advance(speeds(), 1.0);
    assert_eq!(pos[2], 0.0);
    let mut idle = motion(move_flags::SWIMMING, 0.0);
    idle.pitch = -1.0;
    let (pos, _, _, speed) = idle.advance(speeds(), 1.0);
    assert_eq!((pos[0], pos[2], speed), (0.0, 0.0, 0.0));
}

#[test]
fn turn_toward_caps_and_takes_short_way() {
    use std::f32::consts::{FRAC_PI_2, PI, TAU};
    // Big turn, small cap: advances by exactly the cap, positive (short) direction (0 → +π/2 caps).
    let a = turn_toward(0.0, FRAC_PI_2, 0.1);
    assert!((a - 0.1).abs() < 1e-5, "caps the step: {a}");
    // Short way is negative: from 0.1 toward (2π − 0.1) the client turns −, not + all the way round.
    let b = turn_toward(0.1, TAU - 0.1, 0.05);
    assert!((b - 0.05).abs() < 1e-5, "turns the short (−) way: {b}");
    // Within a step of the goal → lands on the goal (no overshoot/oscillation).
    let c = turn_toward(0.0, 0.03, 1.0);
    assert!((c - 0.03).abs() < 1e-5, "reaches the goal: {c}");
    // Already facing (Δ≈0) → unchanged.
    let d = turn_toward(PI, PI, 1.0);
    assert!((d - PI).abs() < 1e-4, "no turn when aligned: {d}");
}

#[test]
fn resolve_facing_angle_spot_and_target() {
    let none = |_g: u64| None;
    // Angle is verbatim.
    assert_eq!(
        resolve_facing(MonsterMoveFacing::Angle(1.25), [0.0; 3], none),
        Some(1.25)
    );
    // Spot due WoW +X (north) from the unit → orientation 0.
    assert_eq!(
        resolve_facing(MonsterMoveFacing::Spot([5.0, 0.0, 0.0]), [0.0; 3], none),
        Some(0.0)
    );
    // Spot due WoW +Y (west) → orientation +π/2.
    assert_eq!(
        resolve_facing(MonsterMoveFacing::Spot([0.0, 5.0, 0.0]), [0.0; 3], none),
        Some(std::f32::consts::FRAC_PI_2)
    );
    // Target resolves through the lookup; the bearing uses the unit's own position as origin.
    assert_eq!(
        resolve_facing(MonsterMoveFacing::Target(0x42), [1.0, 1.0, 0.0], |g| {
            (g == 0x42).then_some([1.0, 6.0, 0.0])
        }),
        Some(std::f32::consts::FRAC_PI_2)
    );
    // None, an unknown target, and a coincident point all yield no facing (never a spin-to-0).
    assert_eq!(
        resolve_facing(MonsterMoveFacing::None, [0.0; 3], none),
        None
    );
    assert_eq!(
        resolve_facing(MonsterMoveFacing::Target(0x1), [0.0; 3], none),
        None
    );
    assert_eq!(
        resolve_facing(
            MonsterMoveFacing::Spot([0.0, 0.0, 9.0]),
            [0.0, 0.0, 0.0],
            none
        ),
        None,
        "a point directly above (no horizontal delta) is degenerate"
    );
}

#[test]
fn remote_motion_runs_forward_along_facing() {
    // Facing WoW +X (orientation 0), moving forward for 1s at run 7 → +7 in X, no Y, no turn.
    let (pos, o, _vz, speed) = motion(move_flags::FORWARD, 0.0).advance(speeds(), 1.0);
    assert!((pos[0] - 7.0).abs() < 1e-3, "forward advances +X: {pos:?}");
    assert!(pos[1].abs() < 1e-3, "no lateral drift: {pos:?}");
    assert_eq!(o, 0.0, "forward doesn't turn");
    assert_eq!(speed, 7.0, "uses run speed");
}

#[test]
fn remote_motion_backpedal_uses_run_back_speed() {
    // Facing +X, BACKWARD with no forward override → moves −X at the slower run-back speed.
    let (pos, _o, _vz, speed) = motion(move_flags::BACKWARD, 0.0).advance(speeds(), 1.0);
    assert!(
        (pos[0] + 4.5).abs() < 1e-3,
        "backpedal advances −X by run_back: {pos:?}"
    );
    assert_eq!(speed, 4.5);
}

#[test]
fn remote_motion_swim_backpedal_takes_min_of_the_swim_pair() {
    // The byte law (`0x7c4c90`'s backward arms, swim-feel §5 TU-H): backward speed is
    // `min(back, forward)` for both pairs — the plain back speed whenever it's the slower
    // (always, at vanilla values), clamped if a server force-sets it above the forward speed.
    let mut s = speeds();
    s.swim_back = 2.5;
    let (pos, _o, _vz, speed) =
        motion(move_flags::SWIMMING | move_flags::BACKWARD, 0.0).advance(s, 1.0);
    assert!(
        (pos[0] + 2.5).abs() < 1e-3,
        "swim backpedal advances −X by swim_back: {pos:?}"
    );
    assert_eq!(speed, 2.5);
    s.swim_back = 9.0; // above forward swim (4.0) — the min clamps to swim
    let (_pos, _o, _vz, speed) =
        motion(move_flags::SWIMMING | move_flags::BACKWARD, 0.0).advance(s, 1.0);
    assert_eq!(
        speed, 4.0,
        "swimBack above swim clamps to swim (the min law)"
    );
}

#[test]
fn remote_motion_strafe_left_moves_90deg_left() {
    // Facing +X (north), strafe-left is +90° → +Y (west in WoW), at run speed.
    let (pos, o, _vz, _s) = motion(move_flags::STRAFE_LEFT, 0.0).advance(speeds(), 1.0);
    assert!(
        (pos[1] - 7.0).abs() < 1e-3,
        "strafe-left advances +Y: {pos:?}"
    );
    assert!(pos[0].abs() < 1e-3, "no forward drift: {pos:?}");
    assert_eq!(o, 0.0, "strafe doesn't turn the facing");
}

#[test]
fn remote_motion_turn_in_place_rotates_facing_only() {
    // TURN_LEFT with no translation: facing rotates by +turn_rate·dt; no position change, speed 0.
    let (pos, o, _vz, speed) = motion(move_flags::TURN_LEFT, 0.0).advance(speeds(), 0.5);
    assert!(
        (o - std::f32::consts::FRAC_PI_2).abs() < 1e-3,
        "turn-left raises facing by turn_rate·dt: {o}"
    );
    assert_eq!(
        pos,
        [0.0, 0.0, 0.0],
        "no translation while turning in place"
    );
    assert_eq!(speed, 0.0);
}

#[test]
fn remote_motion_stationary_when_no_move_flags() {
    let (pos, o, _vz, speed) = motion(0, 1.0).advance(speeds(), 1.0);
    assert_eq!(pos, [0.0, 0.0, 0.0]);
    assert_eq!(o, 1.0);
    assert_eq!(speed, 0.0);
}

#[test]
fn jump_seed_derives_velocity_and_clamps() {
    // The wire zspeed is DOWN-positive (a rising jump is negative — VERIFIED, the real client sends
    // -7.955547): the take-off UP-speed is `-zspeed`. Horizontal = (cos,sin)·xyspeed (world XY).
    let j = JumpInfo {
        zspeed: -7.955_547,
        cos_angle: 1.0,
        sin_angle: 0.0,
        xy_speed: 7.0,
    };
    let (vz, xy) = jump_seed(Some(j), 0);
    assert!(
        (vz - 7.955_547).abs() < 1e-3,
        "take-off up-speed = -zspeed (positive, rising): {vz}"
    );
    assert!(
        (xy[0] - 7.0).abs() < 1e-3 && xy[1].abs() < 1e-3,
        "horizontal +X: {xy:?}"
    );
    // Mid-fall (1s in): up-speed = -zspeed − g·t (now negative, descending).
    let (vz1, _) = jump_seed(Some(j), 1000);
    assert!(
        (vz1 - (7.955_547 - GRAVITY)).abs() < 1e-3,
        "vertical decays by gravity: {vz1}"
    );
    // A long fall is clamped to terminal velocity.
    let (vzt, _) = jump_seed(Some(j), 10_000);
    assert!(
        (vzt + TERMINAL_VELOCITY).abs() < 1e-3,
        "clamped to −terminal: {vzt}"
    );
    // A non-jumping packet → grounded: no vertical, no horizontal freeze.
    assert_eq!(jump_seed(None, 0), (0.0, [0.0, 0.0]));
}

#[test]
fn remote_motion_jump_is_a_parabola_not_flag_walking() {
    // Airborne (JUMPING) with a frozen +X launch of 7 yd/s and +Z 7.955547 yd/s. Even though the
    // FORWARD flag is set, the horizontal is the *frozen* launch (not run speed), and the height
    // follows the arc under gravity — the launch played out locally, not flag-driven walking.
    let mut rm = motion(move_flags::FALLING | move_flags::FORWARD, 0.0);
    rm.vertical_velocity = 7.955_547;
    rm.jump_xy_vel = [7.0, 0.0];
    let (pos, o, vz, speed) = rm.advance(speeds(), 0.5);
    assert!(
        (pos[0] - 3.5).abs() < 1e-3,
        "horizontal coasts at the frozen 7 yd/s: {pos:?}"
    );
    assert!(pos[1].abs() < 1e-3, "no lateral drift: {pos:?}");
    assert!(
        (pos[2] - 7.955_547 * 0.5).abs() < 1e-3,
        "height integrates v·dt: {pos:?}"
    );
    assert!(
        (vz - (7.955_547 - GRAVITY * 0.5)).abs() < 1e-3,
        "vertical speed decays by gravity: {vz}"
    );
    assert_eq!(o, 0.0, "no in-air turn");
    assert!(
        (speed - 7.0).abs() < 1e-3,
        "anim speed is the frozen horizontal: {speed}"
    );
}

#[test]
fn spline_interpolates_constant_speed_and_faces_travel() {
    // Two legs: 10 yd east (+X), then 10 yd north-ish (+Y), over 4s total (constant speed → 2s/leg).
    let start = Instant::now();
    let s = Spline {
        points: vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [10.0, 10.0, 0.0]],
        start,
        duration: Duration::from_secs(4),
        id: 0,
        grounded: true,
    };
    let close = |a: [f32; 3], b: [f32; 3]| (0..3).all(|i| (a[i] - b[i]).abs() < 0.05);

    let (p0, f0, pitch0) = s.sample(start);
    assert!(
        close(p0, [0.0, 0.0, 0.0]),
        "start at first point, got {p0:?}"
    );
    assert!(f0.unwrap().abs() < 1e-3, "faces +X, got {f0:?}");
    assert_eq!(pitch0, 0.0, "a level segment has no travel pitch");

    let (p1, _, _) = s.sample(start + Duration::from_secs(1));
    assert!(close(p1, [5.0, 0.0, 0.0]), "mid leg 1, got {p1:?}");

    let (p3, f3, _) = s.sample(start + Duration::from_secs(3));
    assert!(close(p3, [10.0, 5.0, 0.0]), "mid leg 2, got {p3:?}");
    assert!(
        (f3.unwrap() - std::f32::consts::FRAC_PI_2).abs() < 1e-3,
        "faces +Y, got {f3:?}"
    );

    let (pe, _, _) = s.sample(start + Duration::from_secs(10));
    assert!(
        close(pe, [10.0, 10.0, 0.0]),
        "clamps to last point, got {pe:?}"
    );
}

#[test]
fn spline_travel_pitch_is_the_segment_climb_angle() {
    // A 45° climbing leg (10 yd east, 10 yd up) reports pitch asin(dz/len) = π/4 (+up) — the
    // observed-mover pitch rule `asin(dir.z)` the swimming-creature body pitch renders.
    let start = Instant::now();
    let s = Spline {
        points: vec![[0.0, 0.0, 0.0], [10.0, 0.0, 10.0]],
        start,
        duration: Duration::from_secs(4),
        id: 0,
        grounded: true,
    };
    let (_, f, pitch) = s.sample(start + Duration::from_secs(1));
    assert!(f.unwrap().abs() < 1e-3, "facing is the horizontal heading");
    assert!(
        (pitch - std::f32::consts::FRAC_PI_4).abs() < 1e-3,
        "climb pitch is +π/4, got {pitch}"
    );
}

#[test]
fn monster_move_carries_every_waypoint() {
    // The whole decoded polyline rides into the spline — a curved patrol keeps its corners, not a
    // straight start→endpoint collapse. `sample` (tested above) then walks all of them constant-speed.
    let path = vec![
        [0.0, 0.0, 0.0],
        [10.0, 0.0, 0.0],
        [10.0, 10.0, 0.0],
        [10.0, 10.0, 5.0],
    ];
    let s = monster_move_spline(path.clone(), 42, false, 2000, false)
        .expect("a moving monster-move yields a spline");
    assert_eq!(
        s.points, path,
        "every waypoint survives, not just the endpoint"
    );
    assert_eq!(
        s.id, 42,
        "the spline id rides through (for the SPLINE_DONE ack)"
    );
    assert_eq!(s.duration, Duration::from_millis(2000));
    assert!(
        s.grounded,
        "a non-flying spline is a ground walk (terrain-clamped)"
    );
}

#[test]
fn monster_move_flying_spline_is_not_grounded() {
    // A FLYING path keeps the server's Z — the ground-clamp must leave it alone.
    let s = monster_move_spline(
        vec![[0.0, 0.0, 0.0], [10.0, 0.0, 50.0]],
        0,
        false,
        2000,
        true,
    )
    .expect("a flying monster-move still yields a spline");
    assert!(
        !s.grounded,
        "a flying spline keeps its own Z, never terrain-clamped"
    );
}

#[test]
fn monster_move_stop_clears_the_spline() {
    assert!(
        monster_move_spline(vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]], 0, true, 2000, false).is_none(),
        "a Stop move snaps and clears, never builds a path"
    );
}

#[test]
fn monster_move_zero_duration_clears_the_spline() {
    assert!(
        monster_move_spline(vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]], 0, false, 0, false).is_none(),
        "a zero-duration move would divide by ~0 when sampled; treat as stationary"
    );
}

#[test]
fn monster_move_without_a_travelable_path_clears_the_spline() {
    assert!(
        monster_move_spline(vec![[1.0, 2.0, 3.0]], 0, false, 2000, false).is_none(),
        "a single point is nowhere to travel — no spline"
    );
}

/// The relay clock maps a packet's server timestamp to a client fire-time on the mover's own
/// timeline (decision 0601): the first packet anchors the offset (fire = arrival), a steady
/// stream schedules by the learned mean, an early burst is capped at the reference's +1000 ms
/// defer ceiling, and a wildly-off sample re-anchors instead of poisoning the mean.
#[test]
fn relay_clock_schedules_on_the_server_timeline() {
    let mut clock = RelayClock::default();
    // First sample anchors: offset 4000, zero jitter → fire exactly at arrival.
    assert_eq!(clock.fire_time(1000, 5000.0), 5000.0);
    // A steady stream (same offset) keeps firing at arrival — no invented delay.
    assert_eq!(clock.fire_time(1500, 5500.0), 5500.0);
    // An EARLY packet (offset 1500 ms under the mean, still in-family): the raw schedule
    // stime + mean lands ~1500 ms past arrival — clamped to the reference's +1000 ceiling.
    let fire = clock.fire_time(2000, 4500.0);
    assert_eq!(fire, 5500.0, "defer capped at arrival + 1000 ms");
    // A sample 6000 ms off the mean is a re-anchor (server restart), not jitter: the clock
    // resets to it and the next fire follows the NEW timeline at arrival.
    let fire = clock.fire_time(1000, 11_000.0);
    assert_eq!(fire, 11_000.0, "re-anchored — fire at arrival again");
}

/// The pre-fire reconcile lerp (decision 0601; the reference's `0x619090`/`0x6191c0`): an armed
/// correction converges linearly in time and lands exactly on the event position at fire-time; a
/// sub-tolerance prediction disagrees with nothing and the pose is untouched; Z joins the arm
/// test only while swimming.
#[test]
fn reconcile_lerp_lands_on_the_event_at_its_fire_time() {
    let target = [10.0, 0.0, 0.0];
    // Five 100 ms frames toward a fire 500 ms out: linear-in-time convergence, exact landing.
    let mut pos = [0.0, 0.0, 0.0];
    for i in 1..=5 {
        let remaining_after = 0.5 - 0.1 * i as f32;
        pos = reconcile_lerp(pos, pos, target, false, 0.1, remaining_after);
    }
    assert!((pos[0] - 10.0).abs() < 1e-4, "landed on the event: {pos:?}");
    // Prediction already agrees (within the 0.0278-yd tolerance): no correction at all.
    let held = reconcile_lerp(
        [5.0, 5.0, 0.0],
        [10.0, 0.01, 0.0],
        [10.0, 0.0, 0.0],
        false,
        0.1,
        0.4,
    );
    assert_eq!(held, [5.0, 5.0, 0.0], "sub-tolerance miss arms nothing");
    // A Z-only miss arms only while swimming (the reference's 2D-vs-3D flag split).
    let dry = reconcile_lerp(
        [0.0; 3],
        [10.0, 0.0, 1.0],
        [10.0, 0.0, 0.0],
        false,
        0.1,
        0.4,
    );
    assert_eq!(dry, [0.0; 3], "grounded: Z ignored by the arm test");
    let wet = reconcile_lerp([0.0; 3], [10.0, 0.0, 1.0], [10.0, 0.0, 0.0], true, 0.1, 0.4);
    assert_ne!(wet, [0.0; 3], "swimming: Z arms the correction");
}

/// The pre-fire facing interp (the reference's `0x618f80` ω + `0x7c4f30` integrate — the only
/// smoothed facing path a remote has): linear-in-time rotation landing exactly on the event's
/// facing at fire-time, always the short way around the ±π fold, with a dead-zone for a
/// negligible turn.
#[test]
fn facing_lerp_turns_the_short_way_and_lands_at_fire_time() {
    use std::f32::consts::TAU;
    // Five 100 ms frames toward a fire 500 ms out: lands exactly on the event facing.
    let mut o = 0.0f32;
    for i in 1..=5 {
        let remaining_after = 0.5 - 0.1 * i as f32;
        o = facing_lerp(o, 1.5, 0.1, remaining_after);
    }
    assert!((o - 1.5).abs() < 1e-4, "landed on the event facing: {o}");
    // The ±π fold: from 0.1 toward 6.2 (≈ −0.083 the short way) the first frame must rotate
    // NEGATIVE (through 0), never the ~6.1-rad long way.
    let stepped = facing_lerp(0.1, 6.2, 0.1, 0.4);
    assert!(
        stepped < 0.1 && stepped > 6.2 - TAU,
        "short way around: {stepped}"
    );
    // A sub-dead-zone delta isn't worth turning for.
    let held = facing_lerp(1.0, 1.0 + 1.0e-8, 0.1, 0.4);
    assert_eq!(held, 1.0, "dead-zone: negligible turn skipped");
}
