//! Unit tests for the pure motion kernels — the spline sampler, the dead-reckoning integrator,
//! the jump ballistics, and the facing turn (each child module's math, exercised together here
//! like [`super`]'s original single-file block).

use std::time::{Duration, Instant};

use benilla_protocol::{JumpInfo, MonsterMoveFacing, MoveSpeeds};
use bevy::prelude::Quat;

use crate::creature_anim::move_flags;
use crate::player::{GRAVITY, TERMINAL_VELOCITY};

use super::facing::{resolve_facing, turn_toward};
use super::relay::RelayChain;
use super::remote::{facing_lerp, fall_arc_step, jump_seed, reconcile_lerp};
use super::spline::monster_move_spline;
use super::{RemoteMotion, Spline};

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
        relay: Default::default(),
        last_apply_ms: 0.0,
        last_apply_pos: [0.0, 0.0, 0.0],
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

/// Flags that make the chain treat a mover as mid-motion (`0x20ff`'s FORWARD bit is enough).
const MOVING: u32 = move_flags::FORWARD;

/// **The headline property** (decision 0615, the reference's `0x618c30`): while a mover is moving,
/// replay is paced by the *sender's* stamps — `fire = prev fire + wire step` — so however the packets
/// clumped in flight, they replay at the spacing the stamps carry. Here four 500 ms-apart stamps
/// arrive at 0 / 520 / 1450 / 1460 ms (one late, then a two-packet burst) and still fire 500 ms apart.
#[test]
fn relay_chain_replays_on_the_senders_cadence() {
    let mut chain = RelayChain::default();
    let script = [(1000, 0.0), (1500, 520.0), (2000, 1450.0), (2500, 1460.0)];
    let fires: Vec<f64> = script
        .iter()
        .map(|&(wire, now)| chain.schedule(wire, now, MOVING, true))
        .collect();
    assert_eq!(
        fires,
        vec![0.0, 500.0, 1000.0, 1500.0],
        "the burst is de-clumped onto the sender's cadence"
    );
}

/// The chain's two seeds and its guards: the first packet anchors both cells and fires at arrival; a
/// stale/duplicate stamp contributes no step (`@0x618cb8`'s `jle`); and the server's `u32` ms clock
/// wrapping mid-session is just another forward step.
#[test]
fn relay_chain_seeds_holds_stale_stamps_and_survives_the_clock_wrap() {
    let mut chain = RelayChain::default();
    assert_eq!(
        chain.schedule(7_000, 4_000.0, MOVING, true),
        4_000.0,
        "first packet: fire at arrival, whatever the server's clock reads"
    );
    // A re-sent / out-of-order stamp: no forward step, so the chain doesn't advance past the
    // previous fire (and the reference stamp is left alone — the next real step measures from it).
    assert_eq!(chain.schedule(7_000, 4_100.0, MOVING, true), 4_000.0);
    assert_eq!(chain.schedule(6_900, 4_200.0, MOVING, true), 4_000.0);
    assert_eq!(
        chain.schedule(7_300, 4_300.0, MOVING, true),
        4_300.0,
        "the next forward stamp steps 300 ms from the last one that counted"
    );
    // The wrap: 150 is 251 ms after u32::MAX − 100 on a wrapping ms clock.
    let mut chain = RelayChain::default();
    chain.schedule(u32::MAX - 100, 0.0, MOVING, true);
    assert_eq!(
        chain.schedule(150, 10.0, MOVING, true),
        251.0,
        "the u32 wrap is a forward step, not a 49-day jump backwards"
    );
}

/// **What the chain would do with a server-authored SELF move** — the measurement decision 0725's
/// inline apply rests on, rather than an assertion about it. The reference has one move machine and
/// routes a self-addressed `MSG_MOVE_*` (a GM `.go forward`, a `.cheat fly` toggle, an anticheat
/// snap-back) through this same chain, so the honest question is what fire-time it hands one.
///
/// Two arms, and the second is the one that matters. **Fresh chain → arrival**: the local mover's
/// chain is fed by these packets and nothing else, and in ordinary play none arrive at all, so the
/// first one is a seed and fires immediately. **Seeded chain → the sender's cadence, held**: once
/// there are stamps to pace against, a packet that arrives *earlier* than the previous fire plus
/// the wire step is deliberately delayed to preserve the spacing the stamps carry. That is the
/// chain's headline property working exactly as designed — and it is the property benilla's self
/// arm skips, because reproducing the "cadence" between one GM command and the next buys nothing
/// while delaying a correction to our own pose.
///
/// The stamps are vmangos's own: `SetAsServerSide` writes a fresh `WorldTimer::getMSTime()` into
/// `stime`, so the wire steps track the real time between the commands.
#[test]
fn the_chain_paces_a_server_authored_self_move_and_would_hold_an_early_one() {
    let mut chain = RelayChain::default();
    // Standing still, nothing queued — the state a GM command finds us in.
    assert_eq!(
        chain.schedule(1_000, 0.0, 0, true),
        0.0,
        "the first server-authored move seeds the chain and fires at arrival"
    );
    // 30 s later, arriving 140 ms behind the chain's pacing: the pacing law says fire at arrival
    // (a late packet is already overdue), and the lateness enters the window.
    assert_eq!(chain.schedule(31_000, 30_140.0, 0, true), 30_140.0);
    // 30 s later again, arriving 90 ms *ahead* of the pacing. The chain holds it back to keep the
    // sender's spacing — so the reference would apply this one 90 ms after it landed.
    assert_eq!(
        chain.schedule(61_000, 60_050.0, 0, true),
        60_140.0,
        "an early arrival is held to the sender's cadence — the pacing benilla's self arm skips"
    );
}

/// The de-jitter buffer is re-sized **only** on a standing mover with an empty queue (`@0x618ce4` /
/// `@0x618cf3`), and it is sized by the window's worst lateness (`0x618b50`) — then held, not
/// re-charged: the ring stores lateness *relative to the base*, so a spike already absorbed doesn't
/// buy a second helping of buffer on the next idle packet.
#[test]
fn relay_chain_rebases_the_buffer_only_when_idle_and_unqueued() {
    // A 200 ms-late packet enters the window while the mover is moving: no re-base, no buffer.
    let mut chain = RelayChain::default();
    chain.schedule(0, 0.0, MOVING, true);
    assert_eq!(chain.schedule(500, 700.0, MOVING, true), 500.0);
    // Still moving when the next one lands: the chain stays glued to the sender's cadence.
    let mut moving = chain.clone();
    assert_eq!(
        moving.schedule(1000, 1000.0, MOVING, true),
        1000.0,
        "mid-motion: the 200 ms spike buys no buffer"
    );
    // The same packet on a mover that has come to a stop with nothing queued: NOW the chain
    // re-bases, and the buffer it takes is exactly the window's worst lateness.
    let mut idle = chain.clone();
    assert_eq!(
        idle.schedule(1000, 1000.0, 0, true),
        1200.0,
        "idle + empty: re-based by the window max (200 ms late)"
    );
    // ...and the next idle packet holds that buffer rather than charging the spike again.
    assert_eq!(
        idle.schedule(1500, 1500.0, 0, true),
        1700.0,
        "the absorbed spike is not re-charged: still 200 ms of lead"
    );
    // A queued event blocks the re-base even when the mover is idle.
    let mut queued = chain.clone();
    assert_eq!(
        queued.schedule(1000, 1000.0, 0, false),
        1000.0,
        "a non-empty queue defers the re-base"
    );
}

/// The reference's skew clamp (`@0x618d0d`/`@0x618d49`): a fire never lands more than 1000 ms after
/// its packet's arrival, nor more than 500 ms before it.
#[test]
fn relay_chain_holds_the_offset_inside_the_reference_clamp() {
    // A 2 s wire step delivered 100 ms after the last fire would schedule 1.9 s out — capped.
    let mut chain = RelayChain::default();
    chain.schedule(0, 0.0, MOVING, true);
    assert_eq!(chain.schedule(2000, 100.0, MOVING, true), 1100.0);
    // A stalled sender (no forward step) whose packet lands 4 s after the last fire would schedule
    // 4 s in the past — floored at arrival − 500 ms, which is due-on-arrival either way.
    let mut chain = RelayChain::default();
    chain.schedule(0, 1000.0, MOVING, true);
    assert_eq!(chain.schedule(0, 5000.0, MOVING, true), 4500.0);
}

/// Under a scripted jitter pattern — a steady stream, a stalled tail, a catch-up burst, an idle
/// resync, then motion again — the chain stays well-formed: fire-times never go backwards (which is
/// what lets the queue be a plain FIFO with no re-sort), and the lead over arrival stays inside the
/// reference's clamp.
#[test]
fn relay_chain_stays_monotone_and_bounded_under_scripted_jitter() {
    // (wire stamp, arrival, moving?) — 500 ms stamps throughout; the arrivals are the abuse.
    let script: [(u32, f64, bool); 14] = [
        (500, 100.0, true),    // first packet
        (1000, 600.0, true),   // steady
        (1500, 1100.0, true),  // steady
        (2000, 2400.0, true),  // a 1.3 s stall
        (2500, 2410.0, true),  // burst catch-up
        (3000, 2420.0, true),  // burst catch-up
        (3500, 2430.0, true),  // burst catch-up
        (4000, 4000.0, false), // stopped, queue drained: resync
        (4500, 4500.0, false), // idle heartbeats
        (5000, 5000.0, false),
        (5500, 5480.0, true), // moving again, slightly early
        (6000, 6050.0, true), // slightly late
        (6500, 6500.0, true),
        (7000, 7100.0, true),
    ];
    let mut chain = RelayChain::default();
    let mut prev_fire = f64::NEG_INFINITY;
    for (wire, now, moving) in script {
        let fire = chain.schedule(wire, now, if moving { MOVING } else { 0 }, true);
        assert!(
            fire >= prev_fire,
            "fire-times must never reorder: {fire} after {prev_fire}"
        );
        let lead = fire - now;
        assert!(
            (-500.0..=1000.0).contains(&lead),
            "lead {lead} outside the reference clamp at wire={wire}"
        );
        prev_fire = fire;
    }
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

/// The frame loop as [`crate::net`] chains it: `apply_net_updates` routes each arriving packet
/// (applied at arrival, or queued), then `drain_pending_moves` empties everything due — apply
/// **before** drain, both on the same frame clock. Returns the order packets were actually applied
/// in, tagged by their `fall_time`, plus the flags left on the unit at the end.
fn replay_frames(script: &[(u32, f64, u32)]) -> (Vec<u32>, u32) {
    use super::relay::{PendingMove, RelayMove};
    let mut rm = motion(MOVING, 0.0);
    let mut applied = Vec::new();
    let apply = |rm: &mut RemoteMotion, mv: &RelayMove, applied: &mut Vec<u32>| {
        rm.flags = mv.flags; // the one bit of `apply_move` this ordering question turns on
        applied.push(mv.fall_time);
    };
    for (id, &(wire_ms, arrival_ms, flags)) in script.iter().enumerate() {
        let now = arrival_ms;
        let mv = RelayMove {
            wire_ms,
            position: [0.0; 3],
            orientation: 0.0,
            flags,
            pitch: 0.0,
            fall_time: id as u32, // the packet's identity, carried through the queue
            jump: None,
            transport: None,
            heartbeat: false,
        };
        let (live, empty) = (rm.flags, rm.pending.is_empty());
        let fire_ms = rm.relay.schedule(wire_ms, now, live, empty);
        if rm.fires_at_arrival(fire_ms, now) {
            apply(&mut rm, &mv, &mut applied);
        } else {
            rm.pending.push_back(PendingMove { fire_ms, mv });
        }
        while rm.pending.front().is_some_and(|p| p.fire_ms <= now) {
            let ev = rm.pending.pop_front().expect("front checked");
            apply(&mut rm, &ev.mv, &mut applied);
        }
    }
    (applied, rm.flags)
}

#[test]
fn a_due_arrival_never_jumps_the_queue() {
    // **The runaway-mover regression** (decision 0618). A mover's packets are applied in the order
    // they arrived, always — even when the newest one is already due on arrival while older ones sit
    // in the queue. Fire-times are monotone (0615), so a due arrival means everything queued is due
    // too: applying the arrival *directly* writes the newest state, and the drain then replays the
    // older queued packets over it in the same frame. Last write wins, last write is stale.
    //
    // The script: a 60 Hz burst that builds a one-deep queue, then a delivery stall (packet 4 lands
    // 64 ms late) so its fire-time — chained at 48 + 16 = 64 ms — is already past by arrival at 96.
    // Packet 4 is the STOP; packet 3, still queued in front of it, is FORWARD.
    let script: [(u32, f64, u32); 5] = [
        (1000, 0.0, MOVING),  // 0 — seeds the chain, fires at arrival
        (1016, 0.0, MOVING),  // 1 — same frame; fire 16, queued
        (1032, 16.0, MOVING), // 2 — fire 32, queued; the drain releases 1
        (1048, 32.0, MOVING), // 3 — fire 48, queued; the drain releases 2
        (1064, 96.0, 0),      // 4 — the STOP. Fire 64, already due; 3 is still queued
    ];
    let (order, flags) = replay_frames(&script);
    assert_eq!(
        order,
        vec![0, 1, 2, 3, 4],
        "relayed moves apply in arrival order — a due arrival waits behind the queue it belongs after"
    );
    assert_eq!(
        flags, 0,
        "the mover ends STOPPED: the Stop is the last write, not the FORWARD packet queued in front \
         of it — a still player then sends nothing, so a stale FORWARD here runs off forever"
    );
}

// ── GameObject placement: the `GAMEOBJECT_ROTATION` quaternion (decision 1459) ────────────────

/// The seven `nightelfsignpostpointer02` arms of the Ravenwind post (Feralas, The Forgotten Coast)
/// as vmangos' `gameobject` table spawns them: `(entry, position, orientation, rotation0..3)`.
/// Six carry the pure-yaw encoding; entry 152580 — the one bug B89 was reported on — authors a
/// 70° tilt in `rotation0/1`.
const RAVENWIND_POST: [(u32, [f32; 3], f32, [f32; 4]); 7] = [
    (
        152574,
        [-4446.32, 2055.25, 46.2946],
        -1.20428,
        [0.0, 0.0, -0.566406, 0.824126],
    ),
    (
        152575,
        [-4446.34, 2055.23, 45.5724],
        -1.20428,
        [0.0, 0.0, -0.566406, 0.824126],
    ),
    (
        152576,
        [-4446.4, 2055.31, 46.2764],
        0.401426,
        [0.0, 0.0, 0.199368, 0.979925],
    ),
    (
        152577,
        [-4446.41, 2055.24, 46.2863],
        1.97222,
        [0.0, 0.0, 0.833886, 0.551937],
    ),
    (
        152578,
        [-4446.41, 2055.24, 45.6197],
        1.97222,
        [0.0, 0.0, 0.833886, 0.551937],
    ),
    (
        152579,
        [-4446.38, 2055.25, 44.954],
        1.97222,
        [0.0, 0.0, 0.833886, 0.551937],
    ),
    (
        152580,
        [-4445.28, 2058.18, 44.9976],
        -0.767946,
        [0.468413, 0.332221, -0.472813, 0.668331],
    ),
];

/// The middle of the pointer plank in the model's OWN space (WoW axes, Z up) — the mid-point of
/// `nightelfsignpostpointer02.m2`'s collision hull, `benilla-extract m2coll`: x ±0.197,
/// y 0.338‥2.075, z 3.062‥3.624. It is that offset from the origin — 3.3 yd up, ~1.2 yd out — that
/// turns a dropped rotation into a *displaced* plank.
const PLANK_MID_MODEL: [f32; 3] = [0.0, 1.206, 3.343];

/// Where the post itself stands (the six untilted arms all spawn on its axis).
const POST_AXIS: [f32; 2] = [-4446.4, 2055.25];

/// Put a model-space point (WoW axes) through a spawn transform and read the answer back in WoW
/// world coordinates — the mesh is baked into Bevy space, so the point converts on the way in.
fn plank_in_world(position: [f32; 3], rotation: Quat) -> [f32; 3] {
    let t = super::pose_transform(position, rotation);
    benilla_assets::coords::bevy_to_wow(
        t.transform_point(benilla_assets::coords::wow_to_bevy(PLANK_MID_MODEL)),
    )
}

/// The pure-yaw quaternion vmangos writes for a spawn with no authored tilt IS `rot_z(facing)`, so
/// placing by the quaternion must leave those spawns exactly where the facing put them. This is the
/// whole safety argument for the switch: 54 747 of the 56 632 live spawn rows are this case, and if
/// the two paths disagreed by even a hair, every prop in the world would shift.
#[test]
fn a_pure_yaw_gameobject_quaternion_places_exactly_like_the_facing() {
    for orientation in [0.0_f32, 0.401426, -0.767946, -1.20428, 1.97222, 3.0, -2.7] {
        let (s, c) = (orientation * 0.5).sin_cos();
        let by_quat = super::gameobject_rotation(Some([0.0, 0.0, s, c]), orientation);
        let by_yaw = super::wire_yaw(orientation);
        assert!(
            by_quat.dot(by_yaw).abs() > 0.99999,
            "orientation {orientation}: quat {by_quat:?} vs yaw {by_yaw:?}"
        );
    }
    // …and at the reported site, on the six real spawn rows that carry no tilt.
    for (entry, position, orientation, quat) in RAVENWIND_POST.iter().take(6) {
        let by_quat = plank_in_world(
            *position,
            super::gameobject_rotation(Some(*quat), *orientation),
        );
        let by_yaw = plank_in_world(*position, super::wire_yaw(*orientation));
        let moved = (0..3)
            .map(|i| (by_quat[i] - by_yaw[i]).powi(2))
            .sum::<f32>()
            .sqrt();
        assert!(moved < 1e-3, "entry {entry} moved {moved} yd");
    }
}

/// Entry 152580 — bug B89. Its authored 70° tilt is the difference between a plank ON the post and
/// a plank 4.3 yd away in mid-air, and the spawn point itself is 3 yd off the post (the tilt is what
/// carries the plank back onto it), so the facing-only placement cannot even land in the right
/// neighbourhood.
#[test]
fn the_tilted_ravenwind_pointer_lands_on_its_post() {
    let (_, position, orientation, quat) = RAVENWIND_POST[6];
    let by_quat = plank_in_world(
        position,
        super::gameobject_rotation(Some(quat), orientation),
    );
    let by_yaw = plank_in_world(position, super::wire_yaw(orientation));

    // The golden: hand-derived from the spawn row and the model's own hull, WoW world coordinates.
    for (got, want) in by_quat.iter().zip([-4444.139_f32, 2055.174, 46.512]) {
        assert!((got - want).abs() < 0.01, "{by_quat:?} vs the golden");
    }
    // What it means: the plank hangs off the post's axis at arm's length…
    let radius = |p: [f32; 3]| (p[0] - POST_AXIS[0]).hypot(p[1] - POST_AXIS[1]);
    assert!(
        radius(by_quat) < 2.5,
        "on the post: r = {}",
        radius(by_quat)
    );
    // …where the facing-only placement flings it clear of the post entirely, which is the report.
    assert!(radius(by_yaw) > 4.0, "off the post: r = {}", radius(by_yaw));
    let apart = (0..3)
        .map(|i| (by_quat[i] - by_yaw[i]).powi(2))
        .sum::<f32>()
        .sqrt();
    assert!(
        (apart - 4.29).abs() < 0.05,
        "the two placements are {apart} yd apart"
    );
}

/// No usable quaternion ⇒ the facing, not a `NaN`. A create block folds absent fields to zero, so
/// "the wire sent nothing" can reach here as an all-zero quat — which has no length to normalize and
/// would blank the object rather than mis-place it.
#[test]
fn a_gameobject_without_a_usable_quaternion_falls_back_to_its_facing() {
    for quat in [None, Some([0.0; 4])] {
        let r = super::gameobject_rotation(quat, 1.97222);
        assert!(
            r.dot(super::wire_yaw(1.97222)).abs() > 0.99999,
            "{quat:?} → {r:?}"
        );
    }
}
