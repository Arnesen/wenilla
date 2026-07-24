//! Remote-player dead-reckoning ([`RemoteMotion`], relayed `MSG_MOVE_*`) — the player half of
//! [`super`]'s motion model (decision 0053): flag-driven ground locomotion between the ~2 Hz
//! heartbeats, and the jump as a locally-played ballistic event.

use benilla_assets::coords::wow_to_bevy;
use benilla_protocol::{JumpInfo, MoveSpeeds};
use bevy::prelude::*;

use crate::creature_anim::move_flags;
use crate::player::{GRAVITY, TERMINAL_VELOCITY};

use super::super::{SelfPlayer, UnitSpeeds};
use super::{yaw_of, Spline};

/// The server-authoritative movement state of a *remote* mover (another player), set from each relayed
/// `MSG_MOVE_*` packet ([`benilla_protocol::SessionEvent::UnitMove`]). Between packets — which arrive at
/// the mover's heartbeat rate (~2 Hz) plus each transition — [`extrapolate_remote_units`] integrates the
/// pose from `flags` so motion is smooth rather than a 2 Hz snap. Holds the canonical WoW-space pose (the
/// entity `Transform` is derived from it each frame); a packet overwrites it (a correction/snap). Not
/// added to our own avatar (the controller drives that) nor to creatures (they ride a server [`Spline`]).
///
/// **A jump is a ballistic event, not flag-driven walking** (decision 0053): while `JUMPING`
/// ([`move_flags::FALLING`]) is set, the horizontal velocity is *frozen* at the launch
/// ([`Self::jump_xy_vel`]) and the height follows a parabola under gravity ([`Self::vertical_velocity`])
/// — the launch played out locally — rather than the ground locomotion the direction flags imply. Each
/// relayed jump packet re-seeds both from its [`JumpInfo`] tail (a correction); a non-jumping packet
/// (e.g. `FALL_LAND`) clears them and resumes ground extrapolation.
#[derive(Component, Clone)]
pub(crate) struct RemoteMotion {
    /// Last authoritative position (raw WoW yards), advanced by extrapolation between packets.
    pub(crate) wow_pos: [f32; 3],
    /// Facing (WoW orientation, radians), advanced while a `TURN_*` flag is set (on the ground).
    pub(crate) orientation: f32,
    /// Live CMovement `moveFlags` (matches [`move_flags`]) — the direction/mode the mover last reported.
    pub(crate) flags: u32,
    /// The swim pitch (radians, +up) the mover last reported — the `MovementInfo` tail present while
    /// `SWIMMING` is set (`0.0` otherwise). The swim dead-reckon applies it the way the client's swim
    /// velocity basis does (`0x7c5880`, pitch folded into the travel direction): vertical
    /// `sin(pitch)·swim speed`, horizontal scaled by `cos(pitch)`.
    pub(crate) pitch: f32,
    /// Current horizontal ground speed (yd/s) the extrapolation is applying — read by the animation
    /// selector ([`crate::creature_anim`]) to choose + rate-scale the gait, the way a [`Spline`]'s speed
    /// does for a creature.
    pub(crate) speed: f32,
    /// Current vertical speed (yd/s, WoW +Z up) while airborne — seeded from a jump packet's `zspeed`
    /// minus gravity over its `fall_time`, then integrated down by gravity each frame. `0` on the ground.
    pub(crate) vertical_velocity: f32,
    /// Frozen horizontal velocity (world XY yd/s) during a jump — `(cos, sin)·xyspeed` from the launch.
    /// Replaces the flag-driven horizontal while `JUMPING` (you can't change direction mid-air). `[0; 2]`
    /// on the ground.
    pub(crate) jump_xy_vel: [f32; 2],
    /// WoW-Z of this airborne arc's takeoff — snapshotted when a packet first sets `FALLING`, held
    /// across the arc, cleared on landing. `None` on the ground (or if the mover was already airborne
    /// when it entered view — no takeoff seen, so no fall-height reference). Feeds the remote
    /// **landing predictor**: on the `FALLING → grounded` edge, `fall_start_z − landing_z` is the fall
    /// height that gates the grunt + dust puff (decision 0415; the launch-height apex proxy the
    /// self-player path uses, applied identically to observed movers).
    pub(crate) fall_start_z: Option<f32>,
    /// Not-yet-due relayed moves, fire-time ascending — the reference's per-unit move-event queue
    /// (`CMovement+0x150`): a remote's packet is **scheduled**, not applied at arrival (decision
    /// 0601; wow-re `remote-apply-timing.md`). [`drain_pending_moves`] applies each head when the
    /// clock reaches its [`PendingMove::fire_ms`]; until then the dead-reckon covers the mover's
    /// own timeline, so the residual at apply time is structurally small.
    pub(crate) pending: std::collections::VecDeque<PendingMove>,
}

/// One scheduled relayed move — the payload of [`benilla_protocol::SessionEvent::UnitMove`] plus
/// the client fire-time [`RelayClock`] mapped from its server timestamp. The reference's queued
/// move-event node (`0x617570`: fire-time at `node[+8]`, pose at `node[+0x10]`).
#[derive(Clone)]
pub(crate) struct PendingMove {
    /// When to apply, on [`Time::elapsed`]'s ms clock (the reference's `ev[+8]`).
    pub(crate) fire_ms: f64,
    pub(crate) position: [f32; 3],
    pub(crate) orientation: f32,
    pub(crate) flags: u32,
    pub(crate) pitch: f32,
    pub(crate) fall_time: u32,
    pub(crate) jump: Option<JumpInfo>,
    pub(crate) transport: Option<benilla_protocol::TransportPose>,
    /// `MSG_MOVE_HEARTBEAT` — excluded from the pre-fire reconcile lerp (the reference's
    /// `0x619090` skips tag `0x26`); it applies as an outright snap.
    pub(crate) heartbeat: bool,
}

/// The relayed-move clock map (decision 0601): estimates the offset between the server's
/// `MovementInfo` ms clock (vmangos stamps `stime` at receipt — one coherent clock for all movers)
/// and our render clock, so each relayed move gets a client **fire-time** — the reference's
/// scheduled replay (`ev[+8] = mapped time + clamped skew`, wow-re `remote-apply-timing.md`).
/// The mapping is an EWMA of per-packet `(now − stime)` plus a jitter margin, so a typical packet
/// arrives just *before* its fire-time and waits in the unit's queue; the exact reference cursor
/// arithmetic (`mgr+0x128/+0x140`) is the un-traced residual on wow-re's board, so this estimator
/// is ours — bounded by the reference's verified `[−500, +1000] ms` skew clamp.
#[derive(Resource, Default)]
pub(crate) struct RelayClock {
    /// EWMA of `(now_ms − stime)` over recent relays; `None` until the first.
    mean: Option<f64>,
    /// EWMA of `|sample − mean|` — the jitter estimate feeding the scheduling margin.
    mad: f64,
}

/// EWMA weight for the clock mean/jitter — ~16-packet memory (a few seconds of one mover's
/// heartbeats), quick to settle at login, slow enough to ride out one outlier.
const CLOCK_ALPHA: f64 = 1.0 / 16.0;
/// A sample this far off the mean is a re-anchor (server restart, session change), not jitter.
const CLOCK_REANCHOR_MS: f64 = 2000.0;
/// Cap on the jitter-derived scheduling margin: never buffer more than this beyond the mean.
const CLOCK_MARGIN_MAX_MS: f64 = 200.0;
/// The reference's skew clamp ceiling: a fire-time never lands more than this past arrival
/// (`0x618c30 @0x618d0d/49`: skew held in `[−500, +1000]` ms; the floor needs no mirror — an
/// already-due fire-time applies at arrival either way).
const FIRE_DEFER_MAX_MS: f64 = 1000.0;

impl RelayClock {
    /// Map a relayed move's server timestamp to a client fire-time (ms on [`Time::elapsed`]),
    /// updating the offset estimate with this packet's sample.
    pub(crate) fn fire_time(&mut self, stime: u32, now_ms: f64) -> f64 {
        let sample = now_ms - f64::from(stime);
        let mean = match self.mean {
            Some(m) if (sample - m).abs() < CLOCK_REANCHOR_MS => {
                let m = m + (sample - m) * CLOCK_ALPHA;
                self.mad += ((sample - m).abs() - self.mad) * CLOCK_ALPHA;
                m
            }
            _ => {
                self.mad = 0.0;
                sample
            }
        };
        self.mean = Some(mean);
        // Schedule at the mean-latency timeline plus a jitter margin, so most packets arrive
        // early and wait — the buffering that absorbs relay jitter — capped by the reference's
        // +1000 ms skew ceiling relative to arrival.
        let margin = (3.0 * self.mad).min(CLOCK_MARGIN_MAX_MS);
        (f64::from(stime) + mean + margin).min(now_ms + FIRE_DEFER_MAX_MS)
    }
}

/// `WOW_REMOTE_SNAP=1` — the A/B escape: apply every relayed move at arrival as a raw snap
/// (pre-0601 behavior), bypassing the scheduled queue and the reconcile lerp.
pub(in crate::net) fn arrival_snap() -> bool {
    static SNAP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SNAP.get_or_init(|| std::env::var_os("WOW_REMOTE_SNAP").is_some())
}

/// The reconcile-arm tolerance (squared yards): predicted-vs-event disagreement below this needs
/// no correction. The reference's `0x80c744` = 7.716e-4 ≈ (0.0278 yd)², compared in 2D — Z joins
/// only while SWIMMING (`0x619090`, wow-re `spec-driver-A.md`).
const RECONCILE_TOL_SQ: f32 = 7.716e-4;

/// One frame of the pre-fire reconcile (the reference's `0x619090` arm + `0x6191c0` lerp): if
/// `predicted` (the dead-reckoned pose at the event's fire-time) misses `target` by ≥ the
/// tolerance (2D; Z joins while `swimming`), blend `pos` toward `target` by this frame's share of
/// the time left — linear in time, landing exactly on `target` as the clock reaches the
/// fire-time. Below tolerance the prediction agrees and `pos` returns untouched.
pub(super) fn reconcile_lerp(
    mut pos: [f32; 3],
    predicted: [f32; 3],
    target: [f32; 3],
    swimming: bool,
    dt: f32,
    remaining_s: f32,
) -> [f32; 3] {
    let d = [
        predicted[0] - target[0],
        predicted[1] - target[1],
        predicted[2] - target[2],
    ];
    let dist_sq = d[0] * d[0] + d[1] * d[1] + if swimming { d[2] * d[2] } else { 0.0 };
    if dist_sq < RECONCILE_TOL_SQ {
        return pos;
    }
    // This frame spans dt of the (dt + remaining) window from the previous frame to the fire.
    let f = dt / (dt + remaining_s);
    for (p, t) in pos.iter_mut().zip(target) {
        *p += (t - *p) * f;
    }
    pos
}

/// The remote facing-interp dead-zone: an angular step below this isn't worth turning for — the
/// reference's `0x8026bc` = 9.5367e-7 guard on the `0x618f80` angular velocity.
const FACING_DEAD_ZONE: f32 = 9.5367e-7;

/// One frame of the pre-fire facing interp — the reference's remote facing smoothing
/// (`0x618f80` shortest-arc ω into `+0x144`, integrated by `0x7c4f30`: the **only** smoothed
/// facing path a remote unit has — wow-re `body-facing-pipeline.md` §4; every other facing write
/// is a snap). Rotate `orientation` along the shortest arc toward the queued event's `target`,
/// linear in time so it lands exactly as the clock reaches the fire-time; the apply then snaps
/// the (structurally zero) remainder and clears the interp, as `0x617e90` zeroes `+0x148`. The
/// ±π fold picks the short way around; the dead-zone skips a negligible turn.
pub(super) fn facing_lerp(orientation: f32, target: f32, dt: f32, remaining_s: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let mut d = (target - orientation) % TAU;
    if d > PI {
        d -= TAU;
    } else if d < -PI {
        d += TAU;
    }
    if d.abs() < FACING_DEAD_ZONE {
        return orientation;
    }
    orientation + d * (dt / (dt + remaining_s))
}

/// Apply one relayed move to a unit — the pose snap + integrator re-seed the reference performs
/// at arrival (`0x7c6420`) or at scheduled fire (`0x617e90` → `0x7c69a0`): position/facing/flags
/// committed outright onto the ONE simulated pose, ballistic re-seeded from the jump tail, the
/// landing predictor stepped, and the rider tail (decision 0438) re-anchored. Shared by the
/// arrival path ([`crate::net::apply`]'s `unit_move`) and the queue drain ([`drain_pending_moves`]).
pub(in crate::net) fn apply_move(
    e: Entity,
    ev: &PendingMove,
    rm: &mut RemoteMotion,
    commands: &mut Commands,
    landings: &mut MessageWriter<crate::creature_anim::HardLanding>,
) {
    use crate::creature_anim::move_flags::FALLING;
    // The rider tail: a mover ON a transport carries its local pose — store it so
    // `compose_riders` re-anchors it through the boat's live matrix each frame; a tail-less
    // packet from a known rider means they stepped off.
    match &ev.transport {
        Some(t) => {
            commands.entity(e).insert(crate::transport::TransportRider {
                transport_guid: t.guid,
                local_pos: [t.pos.x, t.pos.y, t.pos.z],
                local_orientation: t.orientation,
            });
        }
        None => {
            commands
                .entity(e)
                .remove::<crate::transport::TransportRider>();
        }
    }
    let (vertical_velocity, jump_xy_vel) = jump_seed(ev.jump, ev.fall_time);
    let now_falling = ev.flags & FALLING != 0;
    // The remote landing predictor (decision 0415): on the FALLING → grounded edge the fall
    // height gates the grunt + dust puff, exactly as the self controller does for us.
    let was_falling = rm.flags & FALLING != 0;
    let (new_start, descent) =
        fall_arc_step(was_falling, now_falling, rm.fall_start_z, ev.position[2]);
    rm.fall_start_z = new_start;
    if let Some(descent) = descent {
        landings.write(crate::creature_anim::HardLanding { entity: e, descent });
    }
    rm.wow_pos = ev.position;
    rm.orientation = ev.orientation;
    rm.flags = ev.flags;
    rm.pitch = ev.pitch;
    rm.vertical_velocity = vertical_velocity;
    rm.jump_xy_vel = jump_xy_vel;
}

/// Fire every due queued move (the reference's drain `0x615c30`: bail while `now < ev[+8]`,
/// dequeue + dispatch once due). Runs before [`extrapolate_remote_units`], which then advances
/// the freshly-applied state and runs the pre-fire reconcile lerp against the next queued head.
#[allow(clippy::type_complexity)]
pub(in crate::net) fn drain_pending_moves(
    time: Res<Time>,
    mut commands: Commands,
    mut landings: MessageWriter<crate::creature_anim::HardLanding>,
    mut q: Query<(Entity, &mut RemoteMotion), (Without<Spline>, Without<SelfPlayer>)>,
) {
    let now_ms = time.elapsed_secs_f64() * 1000.0;
    for (e, mut rm) in &mut q {
        while rm.pending.front().is_some_and(|ev| ev.fire_ms <= now_ms) {
            let ev = rm.pending.pop_front().expect("front checked");
            apply_move(e, &ev, &mut rm, &mut commands, &mut landings);
        }
    }
}

/// Integrate every remote mover's pose from its [`RemoteMotion`] each frame, so another player walks
/// *smoothly* between the sparse relay packets instead of snapping at the ~2 Hz heartbeat rate. The
/// horizontal velocity is derived from the live `moveFlags` in the mover's facing frame at its run /
/// run-back / swim speed; the `TURN_*` flags rotate the facing at `turn_rate`. Z is left to the next
/// packet's correction (the relay carries the authoritative height). A creature [`Spline`] (server-
/// authored path) and our own avatar ([`SelfPlayer`]) are excluded — they have their own motion source.
///
/// This is the client's own dead-reckoning, in miniature: extrapolate from the last reported state,
/// snap to the truth when the next packet lands. The pose lives canonically in WoW space on the
/// component; the [`Transform`] is derived from it (translation + facing only — scale is preserved).
#[allow(clippy::type_complexity)]
pub(in crate::net) fn extrapolate_remote_units(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<
        (
            Entity,
            &mut Transform,
            &mut RemoteMotion,
            Option<&UnitSpeeds>,
            Option<&mut crate::creature_anim::BodyTwist>,
            Has<super::FacingStep>,
        ),
        (Without<Spline>, Without<SelfPlayer>),
    >,
) {
    use crate::creature_anim::{ease_strafe_yaw, strafe_body_offset, wrap_pi};
    let dt = time.delta_secs();
    let now_ms = time.elapsed_secs_f64() * 1000.0;
    for (e, mut t, mut rm, speeds, twist, latched) in &mut q {
        let s = speeds.map_or_else(MoveSpeeds::default, |u| u.0);
        let (mut pos, mut orientation, vertical_velocity, speed) = rm.advance(s, dt);
        // The pre-fire reconcile toward the queued head (decisions 0601/0602/0603):
        // - **Facing** interpolates toward a NON-heartbeat event's facing (the reference's
        //   `0x618f80` ω armed by `0x619030`, integrated by `0x7c4f30` — its only smoothed
        //   facing path; a mouse-turning mover streams facing in SET_FACING packets, so without
        //   this a watched turn snaps per-packet — the director's "snappy turning").
        // - **Position** (the 0x619090 arm + 0x6191c0 lerp): while a NON-heartbeat event waits and
        //   the prediction at its fire-time would miss its position by ≥ the 0.0278-yd tolerance,
        //   blend the SIMULATED pose so it lands on the event position at the fire-time.
        // A heartbeat is excluded from both arms (`0x619030 @0x61904b` / `0x619090 @0x6190bb`
        // skip tag 0x26) — it snaps at fire, and by then the scheduled dead-reckon has
        // structurally converged.
        if let Some(ev) = rm.pending.front() {
            let remaining_s = ((ev.fire_ms - now_ms) / 1000.0) as f32;
            // A heartbeat is excluded from BOTH pre-fire blends — the reference's facing arm
            // `0x619030` skips tag 0x26 exactly as the position arm `0x619090` does (wow-re
            // `remote-air-facing.md`, decision 0603) — so it applies as an outright snap at
            // fire; the smoothed facings are the transition/SET_FACING family's.
            if remaining_s > 0.0 && !ev.heartbeat {
                orientation = facing_lerp(orientation, ev.orientation, dt, remaining_s);
                // Predict from the pre-frame state to the fire-time (this frame's dt + what's left).
                let (predicted, ..) = rm.advance(s, dt + remaining_s);
                let swimming = ev.flags & move_flags::SWIMMING != 0;
                pos = reconcile_lerp(pos, predicted, ev.position, swimming, dt, remaining_s);
            }
        }
        // The standing mouse-turn shuffle: a mouse-turning mover streams NO turn flag — only its
        // SET_FACING packets — so while the pre-fire facing blend is still covering meaningful
        // yaw on a stationary, grounded mover, latch [`super::FacingStep`] and the anim layer
        // plays ShuffleLeft/Right exactly as the idle re-face does. (In the reference the
        // standing turn-anim is the display-facing chase's toggle — `0x607ed0` → `+0xd58`
        // `0x800/0x1000` → `0x712090`, anims 11/12 — not the movement interp, whose integrator
        // doesn't run flag-less; our display-facing layer models the chase with this latch, the
        // same confirmed outcome. A keyboard turner needs none of it — its TURN flags pick the
        // shuffle already.) Dropped the frame the yaw settles (the apply snaps the remainder).
        let step = rm.pending.front().map_or(0.0, |ev| {
            let grounded_still = rm.flags
                & (move_flags::ANY_MOVE
                    | move_flags::TURN_LEFT
                    | move_flags::TURN_RIGHT
                    | move_flags::FALLING
                    | move_flags::SWIMMING)
                == 0;
            if grounded_still {
                wrap_pi(ev.orientation - orientation)
            } else {
                0.0
            }
        });
        if step.abs() > super::facing::FACING_SETTLED {
            commands.entity(e).insert(super::FacingStep(step));
        } else if latched {
            commands.entity(e).remove::<super::FacingStep>();
        }
        rm.wow_pos = pos;
        rm.orientation = orientation;
        rm.vertical_velocity = vertical_velocity;
        rm.speed = speed;
        t.translation = wow_to_bevy(pos);
        // The strafe body pose, same as our own avatar's (the client's display-facing blend): a
        // strafing remote player renders its body at `orientation ± 90°/45°`, eased in aim-relative
        // offset space (a left↔right flip swings around the front, never the 180°-tie back path),
        // with the SpineLow/Head counter-twist walking the upper body back onto its aim.
        // SWIMMING snaps the display facing to the aim instead — no strafe offset, no ease (the
        // client's facing SNAP list: dead or swimming, wow-re `body-facing-pipeline.md`
        // `mov [esi+0xc94],[esi+0xc98]`) — same gate the local controller applies.
        let swimming = rm.flags & move_flags::SWIMMING != 0;
        let offset = if swimming {
            0.0
        } else {
            strafe_body_offset(rm.flags)
        };
        let yaw = if offset != 0.0 {
            ease_strafe_yaw(yaw_of(t.rotation), orientation, offset, dt)
        } else {
            orientation
        };
        // The swim body pitch (TU-A, `0x60a110`→`0x710620`): a swimmer moving fwd/back renders its
        // root pitched by the reported swim pitch (nose-up positive) about the body's local X;
        // strafe-only and idle swims render level — the same per-frame gate the client's `+0x3c`
        // model-transform sync branches on.
        t.rotation = if swimming && rm.flags & (move_flags::FORWARD | move_flags::BACKWARD) != 0 {
            Quat::from_rotation_y(yaw) * Quat::from_rotation_x(rm.pitch)
        } else {
            Quat::from_rotation_y(yaw)
        };
        if let Some(mut twist) = twist {
            twist.yaw_gap = wrap_pi(orientation - yaw);
        }
    }
}

/// The ballistic seed a relayed jump packet implies: the current vertical speed (yd/s, **+Z up**) and
/// the frozen horizontal velocity `(cos, sin)·xyspeed` (world XY). `None` (a non-jumping packet — a
/// ground move or `FALL_LAND`) → grounded: zero vertical, no horizontal freeze.
///
/// **The wire `zspeed` is *down-positive*** — the real 1.12.1 client sends `-7.955547` for a rising
/// jump (VERIFIED, vanilla-sniffs `dwarf_rogue_dun_morogh` MSG_MOVE_JUMP; vmangos likewise forces
/// `+7.958` *up* via the opcode, discarding the wire value). So the take-off **up**-speed is `-zspeed`,
/// and the current up-speed is `-zspeed - g·t`. Mirrors vmangos `Unit.cpp` `ExtrapolateMovement`
/// (`z = start.z + jumpInitialSpeed·t - ½g·t²`, `jumpInitialSpeed = -zspeed`) under the same `gravity`
/// (decision 0053; sign corrected by the sniff — decision 0054).
pub(in crate::net) fn jump_seed(jump: Option<JumpInfo>, fall_time: u32) -> (f32, [f32; 2]) {
    match jump {
        Some(j) => {
            let t = fall_time as f32 / 1000.0;
            let vertical = (-j.zspeed - GRAVITY * t).max(-TERMINAL_VELOCITY);
            (
                vertical,
                [j.cos_angle * j.xy_speed, j.sin_angle * j.xy_speed],
            )
        }
        None => (0.0, [0.0, 0.0]),
    }
}

/// The remote landing predictor's per-packet arc step (decision 0415) — pure so it's unit-tested.
/// Given the mover's prior/new `FALLING` state, the takeoff Z tracked so far, and this packet's Z,
/// return `(new fall_start_z, landing descent)`. `descent` is `Some(fall height)` **only** on the
/// `FALLING → grounded` edge with a known takeoff — the value that gates the grunt + dust puff. WoW
/// Z is up, so the height is `takeoff − landing`; `wow_to_bevy` preserves that magnitude, matching
/// the self path's Bevy-Y descent. A mover that entered view mid-fall (no takeoff seen) yields
/// `None` and simply doesn't predict for that arc.
pub(in crate::net) fn fall_arc_step(
    was_falling: bool,
    now_falling: bool,
    fall_start_z: Option<f32>,
    packet_z: f32,
) -> (Option<f32>, Option<f32>) {
    match (was_falling, now_falling) {
        (false, true) => (Some(packet_z), None), // takeoff: this arc's launch height
        (true, true) => (fall_start_z, None),    // still airborne: keep the reference
        (true, false) => (None, fall_start_z.map(|start| start - packet_z)), // landing edge
        (false, false) => (None, None),          // grounded: nothing to track
    }
}

impl RemoteMotion {
    /// Advance one frame of dead-reckoning: the new `(WoW position, facing, vertical speed, horizontal
    /// speed)` given the unit's `speeds` and `dt`. On the **ground**, integrates the velocity the current
    /// `flags` imply in the facing frame (forward/back/strafe summed, normalized, at the run / run-back /
    /// walk / swim speed the flags pick) and rotates the facing while a `TURN_*` flag is set. **Airborne**
    /// (`JUMPING`/`FALLING`), it's a ballistic event instead (decision 0053): the frozen launch horizontal
    /// ([`Self::jump_xy_vel`]) plus a parabola under gravity ([`Self::vertical_velocity`]) — the launch
    /// played out locally, not flag-driven walking. Pure, so the signs + speed choice + arc are
    /// unit-tested (mirrors [`Spline::sample`]); the system writes the result back + to the transform.
    pub(super) fn advance(&self, speeds: MoveSpeeds, dt: f32) -> ([f32; 3], f32, f32, f32) {
        // A jump/fall is one ballistic event: horizontal frozen at the launch, height a parabola under
        // gravity (the same `g` the controller uses). Direction can't change mid-air, so the ground
        // direction flags are ignored here; the facing is corrected by packets (no in-air turn).
        if self.flags & move_flags::FALLING != 0 {
            let mut pos = self.wow_pos;
            pos[0] += self.jump_xy_vel[0] * dt;
            pos[1] += self.jump_xy_vel[1] * dt;
            pos[2] += self.vertical_velocity * dt;
            let vertical = (self.vertical_velocity - GRAVITY * dt).max(-TERMINAL_VELOCITY);
            let speed = self.jump_xy_vel[0].hypot(self.jump_xy_vel[1]);
            return (pos, self.orientation, vertical, speed);
        }

        // Turn-in-place / turning while moving: TURN_LEFT raises the facing, TURN_RIGHT lowers it
        // (matching the controller's A/D turn and the WoW orientation convention).
        let mut turn = 0.0f32;
        if self.flags & move_flags::TURN_LEFT != 0 {
            turn += 1.0;
        }
        if self.flags & move_flags::TURN_RIGHT != 0 {
            turn -= 1.0;
        }
        let orientation = self.orientation + turn * speeds.turn_rate * dt;

        // Travel direction in the facing frame (WoW: forward = (cos o, sin o), left = +90°).
        // Swimming, the FORWARD axis is pitched by the reported swim pitch — the client's swim
        // velocity basis `0x7c5880` writes `(cosY·cosP, sinY·cosP, sinP)` — so a diving swimmer
        // descends between packets instead of sliding flat; the STRAFE axis stays level, and a
        // backward swimmer travels along the negated pitched axis (nose-up backpedal descends).
        let swimming = self.flags & move_flags::SWIMMING != 0;
        let (hp, vp) = if swimming {
            (self.pitch.cos(), self.pitch.sin())
        } else {
            (1.0, 0.0)
        };
        let (fwd, left) = (
            [orientation.cos(), orientation.sin()],
            [-orientation.sin(), orientation.cos()],
        );
        let mut fwd_amt = 0.0f32;
        if self.flags & move_flags::FORWARD != 0 {
            fwd_amt += 1.0;
        }
        if self.flags & move_flags::BACKWARD != 0 {
            fwd_amt -= 1.0;
        }
        let mut left_amt = 0.0f32;
        if self.flags & move_flags::STRAFE_LEFT != 0 {
            left_amt += 1.0;
        }
        if self.flags & move_flags::STRAFE_RIGHT != 0 {
            left_amt -= 1.0;
        }
        let dx = fwd_amt * fwd[0] * hp + left_amt * left[0];
        let dy = fwd_amt * fwd[1] * hp + left_amt * left[1];
        let dz = fwd_amt * vp;

        let len = (dx * dx + dy * dy + dz * dz).sqrt();
        // The speed the move-flags imply — the ref's `GetCurrentSpeed 0x7c4c90` (the swim §5's
        // TU-H): swimming → swim, backward bit → `min(swimBack, swim)`; on land a net-backward
        // move (S with no forward override) → `min(runBack, run)`; a /walk-toggled mover → walk;
        // otherwise run. The min is the byte law — a plain back-speed select whenever it's the
        // slower (always, at vanilla values).
        let backpedal =
            self.flags & move_flags::BACKWARD != 0 && self.flags & move_flags::FORWARD == 0;
        let base = if swimming {
            if backpedal {
                speeds.swim_back.min(speeds.swim)
            } else {
                speeds.swim
            }
        } else if backpedal {
            speeds.run_back.min(speeds.run)
        } else if self.flags & move_flags::WALK_MODE != 0 {
            speeds.walk
        } else {
            speeds.run
        };
        let mut pos = self.wow_pos;
        let speed = if len > 1.0e-4 {
            let step = base * dt / len; // normalize the 3D direction, then advance by base·dt
            pos[0] += dx * step;
            pos[1] += dy * step;
            pos[2] += dz * step;
            base
        } else {
            0.0
        };
        // Grounded/floating: no ballistic vertical (a jump/fall arc returns earlier; a swimmer's
        // vertical is the pitched axis above, position-integrated, not a persisted velocity).
        (pos, orientation, 0.0, speed)
    }
}
