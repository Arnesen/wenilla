//! `/follow` — the auto-follow movement mode (decision 0890).
//!
//! The finding that shapes this module: **follow synthesizes keyboard input.** It owns no
//! translation of its own. Every state change in the reference funnels through the movement
//! singleton's setter (`0x60e790` sets the move-forward bit `0x100000`, `0x60e7f0` clears it) — the
//! same setter the MoveForward keybinding drives, one of ~40 stubs in the keybinding command table
//! at `0x513de0`-`0x514273`. So follow is "hold W for me, and steer", and the right implementation
//! reuses the controller wholesale rather than growing a second mover (wow-re
//! `object-layer/scratch/targeting-by-name.md` PART D, §5-cross-checked).
//!
//! It also sends **nothing on the wire** — corroborated against vmangos, which has no follow opcode
//! in 1.12.1. The server only ever sees the ordinary movement stream our synthesized input produces.
//!
//! ## The law
//!
//! - **Facing is steered, never snapped** (`0x6103d0`/`0x6108xx`): turn toward the followee at
//!   [`TURN_RATE`] = π rad/s (180°/s), clamped to `min(remaining, rate × elapsed)`, and stop turning
//!   inside a [`TURN_DEADZONE`] of 0.001 rad. That rate limit is why follow *reads* like a character
//!   running rather than a camera snapping.
//! - **Beeline, re-aimed every tick** (`0x610e40`): the guid and both positions are re-resolved every
//!   tick and nothing is cached. No path, no spline — `0x670630` is a *getter* over the click-to-move
//!   order, which follow reads and never writes.
//! - **It never reads the followee's speed** (VERIFIED negative, whole-graph scan). You move at
//!   whatever a held forward key yields for *you*, which is why follow falls behind a faster runner.
//! - **A hysteresis band, not a stop distance** — see [`should_move`]. Arrive is inclusive, resume is
//!   one-directional. The gap between the two is why follow visibly starts and stops rather than
//!   juddering on a single threshold.
//!
//! ## The one INFERRED rule
//!
//! The reference cancels follow when the player turns ~180° away (band 160°-220°), but wow-re could
//! not trace *what that angle is measured against* — it is explicitly flagged unresolved. Measuring
//! it against the live bearing alone cannot be right: starting a follow while facing away from the
//! followee would then cancel on the first tick, before the steer had a chance to turn us.
//!
//! So this module latches [`FollowState::aligned`] the first time the facing comes inside the band
//! and only cancels *after* that. Observable behaviour: starting a follow facing any direction works
//! and turns you around; deliberately turning around once you are following drops it. That matches
//! what the band is plainly *for*, and is marked INFERRED until the reference angle is pinned.

use std::f32::consts::PI;

use bevy::prelude::*;

use crate::net::GuidIndex;

use super::state::{MoveSpeed, Player};

/// Follow's facing-turn rate, rad/s — `0xc4d93c`, seeded `0x40490fdb` = π at `0x6111f9`. Its own
/// constant, distinct from the keyboard turn rate.
const TURN_RATE: f32 = PI;

/// Inside this many radians of the bearing, follow stops turning (`0x6108bb` → `0x60e920`).
const TURN_DEADZONE: f32 = 0.001;

/// The speed the distance thresholds are normalised by — `.rdata 0x80c4d0` = 7.0, which is also
/// vanilla's base run speed, so at normal speed the band is exactly 3.0 / 4.5 yd.
const SPEED_NORM: f32 = 7.0;

/// The base stop distance — `.rdata 0x80c4c0` = 3.0, a compile-time constant (its three writers are
/// CRT dynamic initializers behind `_initterm`), **not** a cvar.
const STOP_DISTANCE: f32 = 3.0;

/// Resume is 1.5× the stop distance, and its speed scale is floored at 1.0 — so the band never
/// closes below 3.0 / 4.5 yd however slowly you are moving.
const RESUME_FACTOR: f32 = 1.5;

/// How far off the bearing the facing must sit for a deliberate turn to drop the follow. The
/// reference's band is 160°-220°; wrapped into `[0, π]` that is "at least 160° off".
const CANCEL_ANGLE: f32 = 160.0 * PI / 180.0;

/// Who we are following, and the hysteresis latch — the reference's followed-guid pair `0xc4d980`
/// (armed `0x6111c9`, cleared `0x60fc1e`) plus the state its band implies.
#[derive(Resource, Default)]
pub(crate) struct FollowState {
    /// The followee's guid, or `None` when not following.
    pub(crate) guid: Option<u64>,
    /// Whether the synthesized forward input is currently held. The band's latch: which threshold
    /// applies depends on which side we are already on.
    moving: bool,
    /// Whether the facing has come inside [`CANCEL_ANGLE`] at least once since the follow began —
    /// the guard on the INFERRED turn-away cancel (see the module header).
    aligned: bool,
    /// `WOW_FOLLOW_TRACE` bookkeeping: when the last trace line went out, and where we were.
    /// `None` until a tick has seeded it.
    traced_at: f64,
    traced_pos: Option<Vec3>,
}

impl FollowState {
    /// Begin following `guid`, from a clean band.
    pub(crate) fn start(&mut self, guid: u64) {
        self.guid = Some(guid);
        self.moving = false;
        self.aligned = false;
        self.traced_at = 0.0;
        self.traced_pos = None;
    }

    /// Stop following. Returns whether we actually were.
    pub(crate) fn stop(&mut self) -> bool {
        self.moving = false;
        self.aligned = false;
        self.guid.take().is_some()
    }
}

/// `/follow [name]` — start following. `None` is the bare form (`FollowUnit("target")`), whose
/// subject is the current selection; a name resolves **players only** and additionally through the
/// reference's filter mode 2 (`CanAssist` + alive), which is [`crate::target`]'s to apply.
#[derive(bevy::ecs::message::Message, Clone, Debug)]
pub(crate) struct FollowRequest {
    pub(crate) name: Option<String>,
}

/// The distance at which follow **arrives** and lets go of the key (`0x610ad2`-`0x610b1b`):
/// `(speed / 7.0) × 3.0`, tested inclusively (`test ah,0x41; jp` — the `<=` edge).
fn arrive_distance(speed: f32) -> f32 {
    (speed / SPEED_NORM) * STOP_DISTANCE
}

/// The distance at which a stopped follow **resumes** (`0x610bc4`-`0x610c2b`):
/// `3.0 × 1.5 × max(speed / 7.0, 1.0)` = 4.5 yd at normal run speed. Only consulted while stopped.
fn resume_distance(speed: f32) -> f32 {
    STOP_DISTANCE * RESUME_FACTOR * (speed / SPEED_NORM).max(1.0)
}

/// Should the synthesized forward key be held this tick? The band, as a pure function of which side
/// we are already on — the whole reason follow starts and stops instead of juddering.
fn should_move(was_moving: bool, distance: f32, speed: f32) -> bool {
    if was_moving {
        // Arrive is inclusive, so we keep going only while strictly beyond it.
        distance > arrive_distance(speed)
    } else {
        distance >= resume_distance(speed)
    }
}

/// Wrap an angle into `(-π, π]`.
fn wrap_pi(a: f32) -> f32 {
    let t = std::f32::consts::TAU;
    let x = (a + PI).rem_euclid(t);
    x - PI
}

/// The `face_yaw` that points at a horizontal delta in **Bevy** space.
///
/// Derived from the controller's own forward vector rather than from a coordinate convention:
/// `control` builds `move_fwd = Quat::from_rotation_y(face_yaw) * NEG_Z`, which expands to
/// `(-sin y, 0, -cos y)`. Solving `(-sin y, -cos y) ∝ (dx, dz)` gives `y = atan2(-dx, -dz)`. Tied to
/// the expression it must agree with, so it cannot drift out of sign with it.
fn bearing_to(delta: Vec3) -> f32 {
    (-delta.x).atan2(-delta.z)
}

/// Turn `face` toward `bearing` by at most this tick's budget, or leave it alone inside the
/// deadzone. The reference's `min(remaining, rate × elapsed)` clamp.
fn steer(face: f32, bearing: f32, dt: f32) -> f32 {
    let remaining = wrap_pi(bearing - face);
    if remaining.abs() <= TURN_DEADZONE {
        return face;
    }
    let budget = TURN_RATE * dt;
    face + remaining.clamp(-budget, budget)
}

/// Drive the follow: re-resolve the followee, steer the facing, and decide whether the synthesized
/// forward input is held this tick. Runs immediately **before** `control`, which reads
/// [`Player::follow_forward`] as one more term of its forward axis — the reference's shape exactly,
/// where follow pushes the same move-forward bit the W key does.
///
/// The player's own turn input runs *after* this in the same frame and therefore wins; that is what
/// makes the turn-away cancel reachable at all.
pub(super) fn steer_follow(
    time: Res<Time>,
    mut follow: ResMut<FollowState>,
    mut player: ResMut<Player>,
    speed: Res<MoveSpeed>,
    index: Res<GuidIndex>,
    transforms: Query<&Transform>,
) {
    player.follow_forward = false;
    let Some(guid) = follow.guid else { return };
    // Re-resolved every tick, nothing cached — a followee that streams out ends the follow
    // (`0x610e40` → `0x6106e7`).
    let Some(target) = index
        .0
        .get(&guid)
        .and_then(|e| transforms.get(*e).ok())
        .map(|t| t.translation)
    else {
        info!("follow: the followee is gone — follow ends");
        follow.stop();
        return;
    };
    let delta = target - player.pos;
    let flat = Vec3::new(delta.x, 0.0, delta.z);
    let distance = flat.length();
    if distance < f32::EPSILON {
        return;
    }
    let bearing = bearing_to(flat);
    // The INFERRED turn-away cancel (module header): only armed once the facing has come inside the
    // band, so beginning a follow while facing away turns us around instead of cancelling.
    let off_bearing = wrap_pi(bearing - player.face_yaw).abs();
    if off_bearing < CANCEL_ANGLE {
        follow.aligned = true;
    } else if follow.aligned {
        info!("follow: turned away from the followee — follow ends");
        follow.stop();
        return;
    }
    player.face_yaw = steer(player.face_yaw, bearing, time.delta_secs());
    let moving = should_move(follow.moving, distance, speed.value);
    if moving != follow.moving {
        info!(
            "follow: {} at {distance:.1} yd (arrive {:.1}, resume {:.1}, speed {:.1})",
            if moving { "running" } else { "arrived" },
            arrive_distance(speed.value),
            resume_distance(speed.value),
            speed.value,
        );
    }
    // `WOW_FOLLOW_TRACE=1` — the field instrument for "follow won't catch up / overshoots": one
    // line a second carrying the closing distance and the ground we actually covered, so the
    // travel rate is a measured number rather than an end-to-end guess (decision 0404: timing and
    // feel are measured, never eyeballed). Gated because this one IS per-frame.
    if follow_trace_on() {
        let now = time.elapsed_secs_f64();
        if now - follow.traced_at >= 1.0 {
            // Quiet until a tick has seeded a reference point: an instrument whose opening line is
            // garbage is worse than one that says nothing for a second.
            if let Some(from) = follow.traced_pos {
                let elapsed = now - follow.traced_at;
                let moved = player.pos.distance(from);
                info!(
                    "follow-trace: {distance:.1} yd to go, covered {moved:.1} yd in {elapsed:.2} s \
                     ({:.1} yd/s), moving={moving}",
                    moved / elapsed as f32,
                );
            }
            follow.traced_at = now;
            follow.traced_pos = Some(player.pos);
        }
    }
    follow.moving = moving;
    player.follow_forward = moving;
}

/// `WOW_FOLLOW_TRACE=1` — see [`steer_follow`]. One `OnceLock` read per tick when unset.
fn follow_trace_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WOW_FOLLOW_TRACE").is_some())
}

/// Register the follow mode. The steer runs in the input stage just before `control`, so the flag
/// and the facing it writes are what the controller reads this same frame.
pub(super) fn plugin(app: &mut App) {
    app.init_resource::<FollowState>()
        .add_message::<FollowRequest>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_band_is_three_and_four_point_five_yards_at_normal_speed() {
        assert_eq!(arrive_distance(7.0), 3.0);
        assert_eq!(resume_distance(7.0), 4.5);
    }

    #[test]
    fn resume_scales_with_speed_but_never_below_the_base_band() {
        // Above base speed both thresholds stretch…
        assert_eq!(arrive_distance(14.0), 6.0);
        assert_eq!(resume_distance(14.0), 9.0);
        // …below it, arrive shrinks but resume is FLOORED at the 1.0 scale (the `max` in `0x610bfd`),
        // so the band can never invert.
        assert!(arrive_distance(3.5) < 3.0);
        assert_eq!(resume_distance(3.5), 4.5);
    }

    #[test]
    fn the_hysteresis_band_latches_on_both_edges() {
        // Moving: keep going until strictly inside arrive (which is inclusive, so 3.0 stops).
        assert!(should_move(true, 3.001, 7.0));
        assert!(!should_move(true, 3.0, 7.0), "arrive is inclusive");
        // Stopped: nothing happens until resume, so the gap between 3.0 and 4.5 is dead in BOTH
        // directions — that gap is the whole point of the band.
        assert!(!should_move(false, 4.0, 7.0));
        assert!(should_move(false, 4.5, 7.0));
    }

    #[test]
    fn bearing_agrees_with_the_controllers_own_forward_vector() {
        // The invariant that keeps this from drifting out of sign with `control`.
        for (dx, dz) in [
            (0.0, -1.0),
            (1.0, 0.0),
            (0.0, 1.0),
            (-1.0, 0.0),
            (3.0, -4.0),
        ] {
            let delta = Vec3::new(dx, 0.0, dz);
            let yaw = bearing_to(delta);
            let fwd = Quat::from_rotation_y(yaw) * Vec3::NEG_Z;
            let want = delta.normalize();
            assert!(
                (fwd.x - want.x).abs() < 1e-5 && (fwd.z - want.z).abs() < 1e-5,
                "yaw {yaw} should point at ({dx}, {dz}), got ({}, {})",
                fwd.x,
                fwd.z
            );
        }
    }

    #[test]
    fn the_turn_is_rate_limited_and_has_a_deadzone() {
        // A quarter turn at 180°/s takes 0.5 s, so one 0.1 s tick covers 18°, not the whole 90°.
        let after = steer(0.0, PI / 2.0, 0.1);
        assert!(
            (after - TURN_RATE * 0.1).abs() < 1e-6,
            "clamped to the budget"
        );
        // Within the budget it lands exactly on the bearing rather than overshooting.
        assert!((steer(0.0, 0.05, 1.0) - 0.05).abs() < 1e-6);
        // Inside the deadzone it does not move at all.
        assert_eq!(steer(1.0, 1.0 + TURN_DEADZONE / 2.0, 1.0), 1.0);
    }

    #[test]
    fn steering_takes_the_short_way_round() {
        // Bearing just past -π from a facing just under π: the turn must be a small positive step,
        // not a near-full sweep back the other way.
        let after = steer(PI - 0.05, -PI + 0.05, 1.0);
        assert!(
            wrap_pi(after - (PI - 0.05)) > 0.0,
            "should wrap forward across ±π"
        );
    }
}
