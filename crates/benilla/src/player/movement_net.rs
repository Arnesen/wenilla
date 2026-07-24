//! Outbound self-movement → the wire — the mirror of [`crate::net::motion`] (which integrates *remote*
//! movers). [`stream_self_movement`] diffs this frame's CMovement move-flags against last frame's and
//! emits a `MSG_MOVE_*` per movement-*axis* transition (start/stop forward-back, strafe, turn), the
//! jump/fall lifecycle (JUMP launch, FALL_LAND, step-off heartbeat), a periodic heartbeat while moving,
//! and a rate-limited SET_FACING when turning in place — each carrying the live `MovementInfo`
//! (decisions 0052 + 0053). Split out of the controller: the wire stream is its own concern.
//!
//! **Invariant — the wire mirrors the avatar's *actual* local motion** (decision 0056). vmangos relays
//! what we send verbatim and observers extrapolate it from the moveFlags, so any divergence strands them
//! on stale state: a flag we set but never clear is a *phantom* walk/spin, and an out-of-range value is
//! silently dropped before relay (vmangos rejects `|orientation| > 4π` in `VerifyMovementInfo`,
//! regardless of anticheat). Two rules keep us honest, both enforced here at the wire boundary:
//! - **Every outbound `orientation` is normalized into `[0, 2π)`** — `face_yaw`/`cam.yaw` are unbounded
//!   accumulators, but the real client always sends a normalized facing and the server's validity gate
//!   demands it.
//! - **When the controller stops driving locomotion, the mover is *parked*** — [`park_mover`] flushes a
//!   Stop and clears our flags on entering free-fly; the held frames of a post-teleport settle stream
//!   zeroed flags (so [`stream_self_movement`]'s own diff emits the Stop) — so observers never
//!   extrapolate motion that isn't happening locally.

use benilla_assets::coords::bevy_to_wow;
use benilla_protocol::{JumpInfo, TransportPose};
use crossbeam_channel::Sender;

use crate::creature_anim::move_flags;
use crate::net::{ClientCommand, MoveKind};

use super::Player;

/// How often (s) we send a `MSG_MOVE_HEARTBEAT` while moving. **VERIFIED** against wow-5875-re
/// (collision node, "the move-send cadence"): the local-player send-deadline `mgr+0x130` is armed to
/// `clientTime + 500 ms` (`0x615b80`) — the wire report is the per-transition broadcast plus this
/// ~500 ms-paced heartbeat, independent of the 250 ms physics substeps.
const HEARTBEAT_INTERVAL: f32 = 0.5;
/// Minimum interval (s) between `MSG_MOVE_SET_FACING` packets while turning in place — caps a continuous
/// mouse-turn to ~10 Hz on the wire (smooth for observers, light on bandwidth).
const FACING_INTERVAL: f32 = 0.1;
/// Facing change (rad) below which a standing turn isn't worth a `SET_FACING` packet.
const FACING_EPSILON: f32 = 0.02;
/// The move-flag bits we put on the wire — the base directional / turn / walk set **plus `FALLING`**
/// (= `MOVEFLAG_JUMPING` 0x2000): we serialize the jump tail (`zspeed, cos, sin, xyspeed`) whenever it's
/// set, so observers replay our jump as a ballistic arc (decision 0053) — **`FALLING_FAR`**
/// (`MOVEFLAG_FALLINGFAR` 0x4000, latched mid-arc past the 1/9-yd descent): the real client's live
/// flags carry it and vmangos reads it (anticheat, PointMovementGenerator), so ours does too; it
/// changes no opcode (the axis differ below keys on direction bits only) and just rides whatever
/// packets the arc sends — **and `SWIMMING`** (0x200000): the swim-pitch tail is now serialized
/// symmetrically ([`MovementInfo`](benilla_protocol) — the decision-0052 swim follow-up), so setting
/// the flag no longer desyncs the server's parse; the controller supplies the live pitch alongside it.
const OUTBOUND_FLAG_MASK: u32 = move_flags::FORWARD
    | move_flags::BACKWARD
    | move_flags::STRAFE_LEFT
    | move_flags::STRAFE_RIGHT
    | move_flags::TURN_LEFT
    | move_flags::TURN_RIGHT
    | move_flags::WALK_MODE
    | move_flags::FALLING
    | move_flags::FALLING_FAR
    | move_flags::SWIMMING
    | move_flags::ON_TRANSPORT;

/// Stream this frame's movement to the server the way the real client does: a `MSG_MOVE_*` per movement-
/// *axis* transition (start/stop forward-back, strafe, turn), a JUMP on take-off, a SET_FACING when we
/// turn in place, and a HEARTBEAT every ~500 ms while moving — each carrying the current `MovementInfo`.
/// **VERIFIED** against wow-5875-re (collision "move-send cadence"): the move-state-change broadcaster
/// `0x61a820` selects the wire opcode *from the flag delta* (`0x619f00`), and the report is exactly
/// "per-transition broadcast + ~500 ms heartbeat". vmangos relays it to nearby players, who extrapolate
/// from the flags — how they see us walk/turn/strafe. (We claimed the mover with CMSG_SET_ACTIVE_MOVER at
/// login.) **Airborne is its own send law** (VERIFIED, vanilla-sniffs `dwarf_rogue_dun_morogh`): the
/// fwd/back/strafe transitions and the periodic heartbeat go silent while FALLING — the live flag state
/// rides the packets that do go out (turn transitions, SET_FACING, the FALL_LAND) — so a normal jump is
/// exactly JUMP → \[turn/facing\] → FALL_LAND, with the landing flags telling observers what the keys
/// say *now*. Sends are fire-and-forget; a down thread no-ops. Mutates `player`'s last-sent flags/facing/
/// heartbeat so next frame can diff against them.
#[allow(clippy::too_many_arguments)]
pub(super) fn stream_self_movement(
    sender: &Sender<ClientCommand>,
    player: &mut Player,
    move_flags_now: u32,
    swim_pitch: f32,
    jumped: bool,
    landed: bool,
    started_falling: bool,
    fall_time: u32,
    now: f32,
    speed_acks: &[crate::net::SpeedChangeMessage],
    transport: Option<TransportPose>,
) {
    let wow_pos = bevy_to_wow(player.pos);
    // Normalize the facing into [0, 2π) before it goes on the wire. `face_yaw` is an unbounded
    // accumulator (mouse-look and A/D turning keep growing it), but the real client always sends a
    // normalized orientation, and vmangos's `VerifyMovementInfo` → `IsValidMapCoord` rejects any
    // movement packet with `|o| > 4π`. Past that bound every packet — including the Stop/StopTurn that
    // ends a run or turn — is silently dropped, stranding observers on the last-relayed flags (a
    // phantom spin or run-off that only clears once we turn back in range and emit a fresh transition).
    let facing = player.face_yaw.rem_euclid(std::f32::consts::TAU);
    let wire_flags = move_flags_now & OUTBOUND_FLAG_MASK;
    // The ballistic launch tail, sent on every airborne packet (decision 0053): `zspeed` is the
    // constant take-off vertical speed, the horizontal is the frozen `horiz_vel` mapped to world XY,
    // and `xyspeed` its magnitude. Present iff JUMPING is in `wire_flags` — the serializer gates the
    // tail on the same bit, so the two never disagree. `fall_time` (ms since take-off) is the
    // caller's snapshot: on the landing frame the arc bookkeeping has already cleared
    // `airborne_since`, but the FALL_LAND must still report the accumulated fall time — vmangos
    // `Player::HandleFall` deals fall damage only when the land packet's fallTime ≥ 1229 ms.
    let wire_jump = (wire_flags & move_flags::FALLING != 0).then(|| {
        let v = bevy_to_wow(player.horiz_vel); // WoW velocity [vx, vy, 0] (the transform is linear)
        let xy = v[0].hypot(v[1]);
        let (cos_angle, sin_angle) = if xy > 1.0e-4 {
            (v[0] / xy, v[1] / xy)
        } else {
            (facing.cos(), facing.sin()) // a standing jump: direction is moot (xy 0); use the facing
        };
        JumpInfo {
            // The wire zspeed is DOWN-positive (the real client sends -7.955547 for a rising jump —
            // VERIFIED, vanilla-sniffs), so negate our +up `jump_zspeed`. A real-client observer reads
            // the up-speed as `-zspeed`; sending +up here would make them see us sink (decision 0054).
            zspeed: -player.jump_zspeed,
            cos_angle,
            sin_angle,
            xy_speed: xy,
        }
    });
    // Forced speed changes owe their ack inside the server's ~4 s window: echo kind/guid/counter/
    // speed with EXACTLY this frame's wire payload — the same flags/pose/tails a Move packet would
    // carry, so the server's relocation and anticheat position tests see our honest live state.
    for ack in speed_acks {
        let _ = sender.send(ClientCommand::ForceSpeedAck {
            kind: ack.kind,
            guid: ack.guid,
            counter: ack.counter,
            speed: ack.speed,
            flags: wire_flags,
            pos: wow_pos,
            orientation: facing,
            pitch: swim_pitch,
            fall_time,
            jump: wire_jump,
            transport,
        });
    }
    let prev = player.move_flags;
    let added = wire_flags & !prev;
    let removed = prev & !wire_flags;
    let mut sent = false;
    macro_rules! send_move {
        ($kind:expr) => {{
            if *crate::net::CAST_TRACE {
                bevy::log::info!(
                    "cast-trace: SEND move {:?} flags={:#x} pos=[{:.3},{:.3},{:.3}] o={:.3}",
                    $kind,
                    wire_flags,
                    wow_pos[0],
                    wow_pos[1],
                    wow_pos[2],
                    facing
                );
            }
            let _ = sender.send(ClientCommand::Move {
                kind: $kind,
                flags: wire_flags,
                pos: wow_pos,
                orientation: facing,
                // The serializer writes the pitch iff SWIMMING is in `wire_flags`, so a non-swimming
                // packet ignores this; while swimming it's the live swim heading pitch.
                pitch: swim_pitch,
                fall_time,
                jump: wire_jump,
                // Written iff ON_TRANSPORT is in `wire_flags` — the rider's boat-local pose.
                transport,
            });
            sent = true;
        }};
    }
    const FB: u32 = move_flags::FORWARD | move_flags::BACKWARD;
    const STRAFE: u32 = move_flags::STRAFE_LEFT | move_flags::STRAFE_RIGHT;
    const TURN: u32 = move_flags::TURN_LEFT | move_flags::TURN_RIGHT;
    // Airborne, the fwd/back/strafe axes go SILENT on the wire while the flag state stays live —
    // VERIFIED (vanilla-sniffs `dwarf_rogue_dun_morogh`): a strafe pressed mid-air emits no
    // START_STRAFE yet the landing FALL_LAND carries `(Forward, StrafeLeft)`; an S→W swap mid-air
    // emits no transition yet lands as `(Forward)`. The keys don't move an airborne avatar (the
    // arc's momentum froze at takeoff), so their transitions aren't motion changes — the live bits
    // simply ride every packet that does go out. The TURN axis is the exception (below): turning
    // genuinely works mid-air, and the sniff shows START_TURN_RIGHT/STOP_TURN with `Falling` set.
    let falling = wire_flags & move_flags::FALLING != 0;
    // The airborne lifecycle: a JUMP launch (carries the ballistic tail), a FALL_LAND that closes the
    // arc, or — for a step-off with no jump opcode — an immediate heartbeat so observers start the arc
    // promptly instead of waiting for the periodic one. A mid-air key release updated the flag state
    // silently (above), so the landing frame's diff has no direction edge left and the FALL_LAND goes
    // out alone — the real client sends no trailing Stop after it (sniff-verified).
    if jumped {
        send_move!(MoveKind::Jump);
    } else if landed {
        send_move!(MoveKind::FallLand);
    } else if started_falling {
        send_move!(MoveKind::Heartbeat);
    }
    // Swim transition: the real client announces entering/leaving the water with a dedicated
    // MSG_MOVE_START_SWIM (0xca) / STOP_SWIM (0xcb) the frame the `SWIMMING` bit flips (VERIFIED, wow-re
    // swim-transition — the local `0x6030c0` decision enqueues it), rather than letting the flag ride
    // the next heartbeat. Airborne and swimming are mutually exclusive, so this never races the arc
    // lifecycle above.
    if added & move_flags::SWIMMING != 0 {
        send_move!(MoveKind::StartSwim);
    } else if removed & move_flags::SWIMMING != 0 {
        send_move!(MoveKind::StopSwim);
    }
    // Forward/back axis — silent while airborne (the flag state rides the next packet instead).
    if !falling {
        if added & move_flags::FORWARD != 0 {
            send_move!(MoveKind::StartForward);
        } else if added & move_flags::BACKWARD != 0 {
            send_move!(MoveKind::StartBackward);
        } else if removed & FB != 0 && wire_flags & FB == 0 {
            send_move!(MoveKind::Stop);
        }
        // Strafe axis — same airborne silence.
        if added & move_flags::STRAFE_LEFT != 0 {
            send_move!(MoveKind::StartStrafeLeft);
        } else if added & move_flags::STRAFE_RIGHT != 0 {
            send_move!(MoveKind::StartStrafeRight);
        } else if removed & STRAFE != 0 && wire_flags & STRAFE == 0 {
            send_move!(MoveKind::StopStrafe);
        }
    }
    // Turn axis (keyboard A/D when not mouse-looking).
    if added & move_flags::TURN_LEFT != 0 {
        send_move!(MoveKind::StartTurnLeft);
    } else if added & move_flags::TURN_RIGHT != 0 {
        send_move!(MoveKind::StartTurnRight);
    } else if removed & TURN != 0 && wire_flags & TURN == 0 {
        send_move!(MoveKind::StopTurn);
    }
    // Board/deboard: the ON_TRANSPORT flip has no axis opcode of its own, so if nothing else went
    // out this frame, a heartbeat announces it promptly — the server learns the new frame (and its
    // local-pose tail, or its absence) the frame it changes rather than on the next natural packet.
    if !sent && (added | removed) & move_flags::ON_TRANSPORT != 0 {
        send_move!(MoveKind::Heartbeat);
    }
    // Heartbeat keeps a moving/turning mover's position + facing flowing between transitions. While
    // riding, ON_TRANSPORT alone keeps this stream alive — the deck carries us, so our world pose
    // really is changing (decision 0056: the wire mirrors actual motion) and observers on reference
    // clients keep a fresh compose anchor. **Not while FALLING** — the JUMP packet seeded the whole
    // arc and observers integrate it locally, so the real client sends a normal-length jump with no
    // mid-air packet at all (sniff-verified; each extra heartbeat is a smoothing-free snap-apply on
    // a reference observer). The very long fall's sparse ~3–4 s mid-air sends the sniff shows ride
    // an untraced trigger — a wow-re question on the board; until it lands, a long fall streams
    // nothing between JUMP/step-off and FALL_LAND.
    if !sent && wire_flags != 0 && !falling && now - player.last_heartbeat >= HEARTBEAT_INTERVAL {
        send_move!(MoveKind::Heartbeat);
    }
    // The facing changed with no packet to carry it — standing (a mouse-turn in place), or mid-air
    // (a jump-turn: no heartbeats stream while FALLING, yet observers must see the turn — the sniff
    // shows the real client's mid-air SET_FACING with `(Forward, Falling)` and the jump tail).
    // Rate-limited so a continuous turn doesn't flood the wire.
    if !sent
        && (wire_flags == 0 || falling)
        && (facing - player.last_sent_facing).abs() > FACING_EPSILON
        && now - player.last_heartbeat >= FACING_INTERVAL
    {
        send_move!(MoveKind::SetFacing);
    }
    if sent {
        player.last_heartbeat = now;
        player.last_sent_facing = facing;
    }
    player.move_flags = wire_flags;
}

/// Park our mover on the wire: flush a single `MSG_MOVE_STOP` (flags cleared) so the server — and the
/// observers extrapolating from it — drop whatever locomotion flags we last reported, then zero our
/// bookkeeping. Called when the controller stops driving the avatar with stale flags still live on the
/// wire — entering free-fly (`F`), where [`stream_self_movement`] no longer runs each frame, so nothing
/// else would ever clear them and observers would extrapolate a phantom walk/spin until we re-attach.
/// **Idempotent**: a no-op once we've already reported stopped, so it's safe to call every free-fly
/// frame. The frozen pose + `[0, 2π)`-normalized facing follow the module invariant.
pub(super) fn park_mover(sender: &Sender<ClientCommand>, player: &mut Player) {
    if player.move_flags == 0 {
        return;
    }
    let facing = player.face_yaw.rem_euclid(std::f32::consts::TAU);
    let _ = sender.send(ClientCommand::Move {
        kind: MoveKind::Stop,
        flags: 0,
        pos: bevy_to_wow(player.pos),
        orientation: facing,
        pitch: 0.0, // flags cleared → not swimming → no pitch tail written
        fall_time: 0,
        jump: None,
        transport: None, // flags cleared → no transport tail written
    });
    player.move_flags = 0;
    player.last_sent_facing = facing;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    #[test]
    fn wire_orientation_is_normalized_into_0_2pi() {
        // `face_yaw` is an unbounded accumulator, but vmangos's `VerifyMovementInfo` rejects any
        // movement packet whose orientation has `|o| > 4π` (≈ 12.566) — so a large yaw must leave the
        // controller wrapped into [0, 2π), matching the real client. A fresh FORWARD press emits a
        // StartForward carrying the current orientation, so we can read back what went on the wire.
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut player = Player {
            face_yaw: 100.0, // ~15.9 turns — far past the 4π reject bound
            ..Default::default()
        };
        stream_self_movement(
            &tx,
            &mut player,
            move_flags::FORWARD,
            0.0,
            false,
            false,
            false,
            0,
            0.0,
            &[],
            None,
        );

        let ClientCommand::Move { orientation, .. } = rx
            .try_recv()
            .expect("a StartForward is sent on first FORWARD")
        else {
            panic!("expected a Move command");
        };
        assert!(
            (0.0..TAU).contains(&orientation),
            "orientation must be normalized into [0, 2π), got {orientation}"
        );
        assert!(
            (orientation - 100.0_f32.rem_euclid(TAU)).abs() < 1e-4,
            "the wrap preserves the angle (100 mod 2π): got {orientation}"
        );
    }

    #[test]
    fn fall_land_reports_the_accumulated_fall_time() {
        // The landing frame's FALL_LAND must carry the arc's accumulated fall clock — vmangos's
        // `Player::HandleFall` only deals fall damage when the land packet's fallTime ≥ 1229 ms, so
        // a zeroed clock silently disables fall damage. The controller snapshots the clock before
        // its arc bookkeeping clears `airborne_since`; this pins that the snapshot — not a re-read
        // of the (already-cleared) arc state — is what goes on the wire.
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut player = Player::default(); // airborne_since already cleared, as on a landing frame
        stream_self_movement(
            &tx,
            &mut player,
            0, // grounded again: no FALLING/FALLINGFAR on the land packet itself
            0.0,
            false,
            true, // landed
            false,
            1700, // the snapshot: ~1.7 s of fall (> the 1229 ms damage gate)
            0.0,
            &[],
            None,
        );

        let ClientCommand::Move {
            kind, fall_time, ..
        } = rx.try_recv().expect("a FALL_LAND is sent on landing")
        else {
            panic!("expected a Move command");
        };
        assert_eq!(kind, MoveKind::FallLand);
        assert_eq!(
            fall_time, 1700,
            "the FALL_LAND carries the accumulated fall time, not a cleared clock"
        );
    }

    #[test]
    fn airborne_direction_release_is_silent_and_the_landing_sends_only_fall_land() {
        // The sniff-verified airborne send law: releasing W mid-air emits NO packet (the flag
        // state updates silently and rides the next packet), and the landing then sends exactly
        // one FALL_LAND — never a trailing Stop (the real client sends none; the extra Stop was
        // what re-picked the observer's landing anim away).
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut player = Player {
            move_flags: move_flags::FORWARD | move_flags::FALLING, // last sent: the JUMP's flags
            ..Default::default()
        };
        // Mid-air, W released: flags drop FORWARD but keep FALLING — silence.
        stream_self_movement(
            &tx,
            &mut player,
            move_flags::FALLING,
            0.0,
            false,
            false,
            false,
            300,
            0.2,
            &[],
            None,
        );
        assert!(rx.try_recv().is_err(), "a mid-air release sends nothing");
        assert_eq!(
            player.move_flags,
            move_flags::FALLING,
            "the flag state still updated silently"
        );
        // The landing frame: grounded, no keys — one FALL_LAND, flags 0, and nothing after it.
        stream_self_movement(
            &tx,
            &mut player,
            0,
            0.0,
            false,
            true,
            false,
            800,
            0.8,
            &[],
            None,
        );
        let ClientCommand::Move { kind, flags, .. } = rx.try_recv().expect("the landing packet")
        else {
            panic!("expected a Move command");
        };
        assert_eq!(kind, MoveKind::FallLand);
        assert_eq!(flags, 0, "the FALL_LAND carries the live (released) flags");
        assert!(
            rx.try_recv().is_err(),
            "no trailing Stop after the FALL_LAND"
        );
    }

    #[test]
    fn airborne_turn_transitions_and_facing_still_stream() {
        // The two things that DO go out mid-air (sniff-verified): the turn axis (turning works
        // airborne — START_TURN_RIGHT/STOP_TURN with Falling set) and SET_FACING for a mouse
        // jump-turn (no heartbeats stream while FALLING, so facing needs its own carrier).
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut player = Player {
            move_flags: move_flags::FORWARD | move_flags::FALLING,
            ..Default::default()
        };
        stream_self_movement(
            &tx,
            &mut player,
            move_flags::FORWARD | move_flags::TURN_LEFT | move_flags::FALLING,
            0.0,
            false,
            false,
            false,
            200,
            0.2,
            &[],
            None,
        );
        let ClientCommand::Move { kind, flags, .. } =
            rx.try_recv().expect("a mid-air turn broadcasts")
        else {
            panic!("expected a Move command");
        };
        assert_eq!(kind, MoveKind::StartTurnLeft);
        assert_ne!(flags & move_flags::FALLING, 0, "the packet rides the arc");
        // Mid-air mouse-turn, flags unchanged, heartbeat interval long past: the periodic
        // heartbeat stays suppressed while FALLING and the facing streams via SET_FACING instead.
        player.face_yaw = 1.0;
        stream_self_movement(
            &tx,
            &mut player,
            move_flags::FORWARD | move_flags::TURN_LEFT | move_flags::FALLING,
            0.0,
            false,
            false,
            false,
            900,
            1.5,
            &[],
            None,
        );
        let ClientCommand::Move { kind, .. } = rx.try_recv().expect("the mid-air facing packet")
        else {
            panic!("expected a Move command");
        };
        assert_eq!(kind, MoveKind::SetFacing, "SET_FACING, not a heartbeat");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn park_mover_flushes_a_stop_and_clears_stale_flags() {
        // We were last streaming FORWARD when the controller stopped driving us (free-fly). Parking must
        // send one Stop with flags cleared (so observers drop the phantom walk) and a normalized facing.
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut player = Player {
            move_flags: move_flags::FORWARD,
            face_yaw: -3.0, // out-of-range / negative — must still go out normalized
            ..Default::default()
        };
        park_mover(&tx, &mut player);

        let ClientCommand::Move {
            flags, orientation, ..
        } = rx
            .try_recv()
            .expect("a Stop is flushed when flags were stale")
        else {
            panic!("expected a Move command");
        };
        assert_eq!(flags, 0, "the parked Stop clears the move-flags");
        assert!(
            (0.0..TAU).contains(&orientation),
            "the parked facing is normalized into [0, 2π), got {orientation}"
        );
        assert_eq!(player.move_flags, 0, "bookkeeping is zeroed after parking");
    }

    #[test]
    fn park_mover_is_a_noop_once_already_stopped() {
        // Idempotent: with no stale flags there's nothing to clear, so no packet goes out — safe to call
        // every free-fly frame.
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut player = Player::default(); // move_flags == 0
        park_mover(&tx, &mut player);
        assert!(
            rx.try_recv().is_err(),
            "no Stop is sent when we were already reported stopped"
        );
    }
}
