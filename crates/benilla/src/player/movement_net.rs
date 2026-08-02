//! Outbound self-movement → the wire — the mirror of [`crate::net::motion`] (which integrates *remote*
//! movers). [`stream_self_movement`] diffs this frame's CMovement move-flags against last frame's and
//! emits a `MSG_MOVE_*` per movement-*axis* transition (start/stop forward-back, strafe, turn), the
//! jump/fall lifecycle (JUMP launch, FALL_LAND, step-off heartbeat), a periodic heartbeat while moving,
//! and a SET_FACING every frame the facing changes off the turn axis — each carrying the live
//! `MovementInfo` (decisions 0052 + 0053 + 0617). Split out of the controller: the wire stream is its
//! own concern.
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
    // …**and `LEVITATING`** (0x400, GM flight — decision 0726). Echoing it is not cosmetic: the
    // reference's packet builder reads the same `[cmov+0x40]` the server's flags merged into, so a
    // real client sends it straight back, and vmangos refreshes its `m_movementInfo` from whatever
    // we report. Drop it from our stream and the server's copy loses the bit — then the next
    // server-authored move (a `.go forward`, a forced speed change) echoes a LEVITATING-less word
    // back at us, our merge clears the mode, and we fall out of the sky mid-flight.
    | move_flags::LEVITATING
    // …**and the rest of the granted movement-mode family** (decision 0866) — root, water-walk,
    // safe-fall, hover: the same reasoning one step stronger. These four are the *ack'd* modes, so
    // the server holds an explicit record of granting each one and notices our stream disagreeing.
    // `MOVEFLAG_ROOT` especially: vmangos re-adds it to anything it writes for a rooted mover
    // (`MovementHandler.cpp:1064`), and its absence from an apply-ack is a kick.
    | move_flags::ROOT
    | move_flags::WATER_WALKING
    | move_flags::SAFE_FALL
    | move_flags::HOVER
    | move_flags::ON_TRANSPORT;

/// This frame's **arc edges** — the airborne lifecycle as the send law reads it. One struct rather
/// than four adjacent bools in the argument list, where a miscount is silent and the symptom is a
/// wrong opcode on the wire.
pub(super) struct ArcEdges {
    /// A jump launched this frame → `MSG_MOVE_JUMP`, carrying the ballistic tail.
    pub(super) jumped: bool,
    /// The standstill air nudge fired ([`super::mover::step`]) — the one mid-air press that really
    /// moves us, and so the one that breaks the airborne send silence (decision 0627).
    pub(super) air_nudged: bool,
    /// The arc ended this frame → `MSG_MOVE_FALL_LAND`.
    pub(super) landed: bool,
    /// An arc began with no jump opcode (a walk-off) → an immediate heartbeat, so observers start
    /// the arc promptly instead of waiting for the periodic one.
    pub(super) started_falling: bool,
    /// ms since take-off — the caller's snapshot, because the landing frame's arc bookkeeping has
    /// already cleared `airborne_since` and the FALL_LAND must still report the accumulated time.
    pub(super) fall_time: u32,
}

/// Stream this frame's movement to the server the way the real client does: a `MSG_MOVE_*` per movement-
/// *axis* transition (start/stop forward-back, strafe, turn), a JUMP on take-off, a SET_FACING every
/// frame the facing changes off the turn axis, and a HEARTBEAT every ~500 ms while moving — each
/// carrying the current `MovementInfo`. **VERIFIED** against wow-5875-re (collision "move-send cadence"):
/// the move-state-change broadcaster `0x61a820` selects the wire opcode *from the flag delta*
/// (`0x619f00`), and the flag report is exactly "per-transition broadcast + ~500 ms heartbeat" — with
/// the *facing* report its own independent emitter alongside it (decision 0617: in the 1.12.1 sniff
/// SET_FACING outnumbers every other client-sent movement opcode combined, moving or standing).
/// vmangos relays it all to nearby players, who extrapolate from the flags — how they see us walk/turn/
/// strafe. (We claimed the mover with CMSG_SET_ACTIVE_MOVER at login.) **Airborne is its own send law** (VERIFIED, vanilla-sniffs `dwarf_rogue_dun_morogh`): the
/// fwd/back/strafe transitions and the periodic heartbeat go silent while FALLING — the live flag state
/// rides the packets that do go out (turn transitions, SET_FACING, the FALL_LAND) — so a normal jump is
/// exactly JUMP → \[turn/facing\] → FALL_LAND, with the landing flags telling observers what the keys
/// say *now*. The one press that breaks that silence is the standstill air nudge (`air_nudged`,
/// decision 0627): it is the case the reference's own deferral excludes, and the only mid-air input
/// that actually moves us. Sends are fire-and-forget; a down thread no-ops. Mutates `player`'s last-sent flags/facing/
/// heartbeat so next frame can diff against them.
// Eight, down from twelve: the arc edges are one struct now (`ArcEdges`). The rest are distinct
// types the compiler can tell apart, so the remaining count is noise rather than a miscount risk.
#[allow(clippy::too_many_arguments)]
pub(super) fn stream_self_movement(
    sender: &Sender<ClientCommand>,
    player: &mut Player,
    move_flags_now: u32,
    swim_pitch: f32,
    arc: ArcEdges,
    now: f32,
    speed_acks: &[crate::net::SpeedChangeMessage],
    transport: Option<TransportPose>,
) {
    let ArcEdges {
        jumped,
        air_nudged,
        landed,
        started_falling,
        fall_time,
    } = arc;
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
            super::move_trace::sent($kind, wire_flags, facing, wow_pos);
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
    //
    // **The standstill air nudge is the other exception** (decision 0627), and it is the *same* rule,
    // not a carve-out: the wire mirrors actual motion (0056). The reference's airborne silence is a
    // real flags-side mechanism — while FALLING, `StartMove 0x7c6ae0` defers a new press into an inert
    // latch (`0x20000`/`0x40000`) instead of flipping the direction bit, "**unless nothing is
    // currently moving**" (wow-re `hvel-fall-arc.md` Q3, VERIFIED bytes) — and the broadcaster
    // `0x61a820` picks its opcode from the *flag delta*, so a deferred press produces no delta and
    // no packet. In the one non-deferred case the bit really flips, so the transition really
    // broadcasts. That case is exactly our nudge (`mover::step`: airborne, nothing moving, a
    // direction pressed — the one input that genuinely re-seeds the arc's horizontal velocity), and
    // it must go out: the packet carries the fresh `JumpInfo` tail, which is the only way an observer
    // — integrating our arc from a JUMP that said `xy_speed = 0` — can ever learn we started moving
    // before the FALL_LAND lands.
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
    // Forward/back axis — silent while airborne (the flag state rides the next packet instead),
    // except on the nudge frame. The **stop** arms stay silent while falling no matter what: a
    // mid-air release is the deferred-latch case, always, and the nudge frame is a press by
    // definition — so this only keeps a same-frame release on the *other* axis from slipping out.
    if !falling || air_nudged {
        if added & move_flags::FORWARD != 0 {
            send_move!(MoveKind::StartForward);
        } else if added & move_flags::BACKWARD != 0 {
            send_move!(MoveKind::StartBackward);
        } else if !falling && removed & FB != 0 && wire_flags & FB == 0 {
            send_move!(MoveKind::Stop);
        }
        // Strafe axis — same airborne silence, same one exception.
        if added & move_flags::STRAFE_LEFT != 0 {
            send_move!(MoveKind::StartStrafeLeft);
        } else if added & move_flags::STRAFE_RIGHT != 0 {
            send_move!(MoveKind::StartStrafeRight);
        } else if !falling && removed & STRAFE != 0 && wire_flags & STRAFE == 0 {
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
    // The facing report: one `MSG_MOVE_SET_FACING` per frame in which the facing actually changed —
    // **the single biggest thing our wire stream was missing** (decision 0617). **VERIFIED** against the
    // real 1.12.1.5875 sniff (`dwarf_rogue_dun_morogh`): 179 of its 336 client-sent movement packets are
    // SET_FACING — more than every other movement opcode combined — streamed at *frame* cadence (median
    // 41 ms between them, p25 23 ms, minimum 17 ms) and, decisively, **while moving**: 116 of the 179
    // carry a direction bit (`Forward` ×68, `StrafeRight` ×12, `Forward+StrafeRight` ×9, `Backward` ×12,
    // `Forward+Falling` ×4, …). There is no rate limit and no angular epsilon — wow-re's `0x617100`
    // (SetFacing-then-send) reports whenever `0x617170`'s **exact-equality** change detector says the
    // facing differs at all, so a frame that didn't move the mouse sends nothing and a frame that did
    // sends one packet. (Our own `face_yaw` is likewise only written by real input, so the exact
    // comparison neither floods nor misses.)
    //
    // The one exclusion is the **turn axis**: not a single SET_FACING in the capture carries `TURN_LEFT`
    // or `TURN_RIGHT`. A keyboard turn is already fully described by its flag — observers rotate the
    // mover at the turn rate for as long as it's set — so the facing needs no separate carrier, and the
    // STOP_TURN closes the arc with the final angle.
    //
    // Not gated on `sent`: the capture repeatedly shows a SET_FACING sharing a millisecond with a
    // transition (`+1563` SET_FACING & STOP_STRAFE, `+3009` SET_FACING & STOP, `+4971` SET_FACING &
    // START_STRAFE_RIGHT). They are two independent emitters in the reference — the input-phase facing
    // report and the move-state broadcaster — not one prioritized channel.
    if wire_flags & TURN == 0 && facing != player.last_facing {
        send_move!(MoveKind::SetFacing);
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
    if sent {
        player.last_heartbeat = now;
    }
    // The change detector's reference is the **previous frame's** facing, not the last one we reported:
    // the reference compares the new facing against the unit's live facing cell, which its setter
    // updates on every change whether or not a packet went out. So the turn-axis frames above, which
    // deliberately send nothing, still leave no catch-up packet behind when the key releases.
    player.last_facing = facing;
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
    player.last_facing = facing;
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
            ArcEdges {
                jumped: false,
                air_nudged: false,
                landed: false,
                started_falling: false,
                fall_time: 0,
            },
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
            0,
            // grounded again: no FALLING/FALLINGFAR on the land packet itself
            0.0,
            ArcEdges { jumped: false, air_nudged: false, landed: true, started_falling: // landed
            false, fall_time: 1700 },
            // the snapshot: ~1.7 s of fall (> the 1229 ms damage gate)
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
            ArcEdges {
                jumped: false,
                air_nudged: false,
                landed: false,
                started_falling: false,
                fall_time: 300,
            },
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
            ArcEdges {
                jumped: false,
                air_nudged: false,
                landed: true,
                started_falling: false,
                fall_time: 800,
            },
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
    fn the_standstill_air_nudge_is_the_one_press_that_breaks_the_airborne_silence() {
        // Decision 0627. The airborne silence is the reference's *deferral* (`StartMove 0x7c6ae0`
        // latches a mid-air press instead of flipping the direction bit) and the deferral has one
        // byte-verified exclusion: "unless nothing is currently moving". That case really flips the
        // bit, so the broadcaster really sends — and it must, because it is the only case where the
        // press changed our motion, and the observer is integrating an arc that says xy_speed = 0.
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut player = Player {
            move_flags: move_flags::FALLING, // the standing jump: airborne, no direction
            // Post-nudge: the mover re-seeded it this frame (Bevy −Z = WoW +X, so |xy| = 2.5).
            horiz_vel: bevy::prelude::Vec3::new(0.0, 0.0, -2.5),
            ..Default::default()
        };
        stream_self_movement(
            &tx,
            &mut player,
            move_flags::FORWARD | move_flags::FALLING,
            0.0,
            ArcEdges {
                jumped: false,
                air_nudged: true,
                landed: false,
                started_falling: false,
                fall_time: 300,
            },
            0.3,
            &[],
            None,
        );
        let ClientCommand::Move {
            kind, flags, jump, ..
        } = rx.try_recv().expect("the nudge broadcasts its transition")
        else {
            panic!("expected a Move command");
        };
        assert_eq!(kind, MoveKind::StartForward);
        assert_ne!(flags & move_flags::FALLING, 0, "it rides the arc");
        let tail = jump.expect("an airborne packet carries the ballistic tail");
        assert!(
            (tail.xy_speed - 2.5).abs() < 1.0e-3,
            "the tail carries the RE-SEEDED horizontal — the whole point: an observer whose \
             arc says xy_speed = 0 learns the mover started moving, got {}",
            tail.xy_speed
        );
        assert!(rx.try_recv().is_err(), "one packet, not a burst");

        // The other side of the same rule: a press that did NOT nudge (a jump taken with momentum —
        // the reference defers it into an inert latch) stays silent, exactly as the sniff shows.
        let mut moving = Player {
            move_flags: move_flags::FORWARD | move_flags::FALLING,
            ..Default::default()
        };
        stream_self_movement(
            &tx,
            &mut moving,
            move_flags::FORWARD | move_flags::STRAFE_LEFT | move_flags::FALLING,
            0.0,
            ArcEdges {
                jumped: false,
                air_nudged: false,
                landed: false,
                started_falling: false,
                fall_time: 300,
            },
            0.3,
            &[],
            None,
        );
        assert!(
            rx.try_recv().is_err(),
            "a deferred mid-air press sends nothing"
        );
    }

    #[test]
    fn airborne_turn_transitions_and_facing_still_stream() {
        // The two things that DO go out mid-air (sniff-verified): the turn axis (turning works
        // airborne — START_TURN_RIGHT/STOP_TURN with Falling set) and SET_FACING for a mouse
        // jump-turn — the capture's `8193 (Forward, Falling)` SET_FACINGs, which carry **no** turn
        // bit. No heartbeats stream while FALLING, so the facing needs its own carrier.
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
            ArcEdges {
                jumped: false,
                air_nudged: false,
                landed: false,
                started_falling: false,
                fall_time: 200,
            },
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
        assert!(rx.try_recv().is_err(), "the turn axis carries the facing");
        // Mid-air mouse-turn with the turn key released: the periodic heartbeat stays suppressed
        // while FALLING, and the facing streams via SET_FACING once the turn flag is gone.
        player.face_yaw = 1.0;
        stream_self_movement(
            &tx,
            &mut player,
            move_flags::FORWARD | move_flags::FALLING,
            0.0,
            ArcEdges {
                jumped: false,
                air_nudged: false,
                landed: false,
                started_falling: false,
                fall_time: 900,
            },
            1.5,
            &[],
            None,
        );
        let kinds: Vec<_> = rx
            .try_iter()
            .map(|c| match c {
                ClientCommand::Move { kind, .. } => kind,
                _ => panic!("expected Move commands"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![MoveKind::StopTurn, MoveKind::SetFacing],
            "the turn closes, then the facing reports — no heartbeat while FALLING"
        );
    }

    #[test]
    fn facing_streams_while_running() {
        // **The regression this law exists for** (decision 0617). A mouse-turn while running used to
        // put nothing on the wire until the next ~500 ms heartbeat, so observers dead-reckoned us in a
        // stale direction for half a second and then watched us snap. The reference streams SET_FACING
        // *while moving* — 116 of the capture's 179 carry a direction bit — so every frame the facing
        // moves, one packet goes out carrying the live direction flags.
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut player = Player {
            move_flags: move_flags::FORWARD, // already running: no transition this frame
            ..Default::default()
        };
        player.face_yaw = 0.7;
        stream_self_movement(
            &tx,
            &mut player,
            move_flags::FORWARD,
            0.0,
            ArcEdges {
                jumped: false,
                air_nudged: false,
                landed: false,
                started_falling: false,
                fall_time: 0,
            },
            0.05,
            &[],
            None,
        );
        let ClientCommand::Move { kind, flags, .. } = rx
            .try_recv()
            .expect("a mouse-turn while running reports its facing")
        else {
            panic!("expected a Move command");
        };
        assert_eq!(kind, MoveKind::SetFacing);
        assert_eq!(
            flags,
            move_flags::FORWARD,
            "the facing report carries the live direction flags"
        );
        assert!(
            rx.try_recv().is_err(),
            "exactly one packet per changed frame"
        );
        // A frame that didn't move the mouse sends nothing: the detector is exact equality, not a
        // rate limit — and the heartbeat deadline (0.5 s) has not come round.
        stream_self_movement(
            &tx,
            &mut player,
            move_flags::FORWARD,
            0.0,
            ArcEdges {
                jumped: false,
                air_nudged: false,
                landed: false,
                started_falling: false,
                fall_time: 0,
            },
            0.06,
            &[],
            None,
        );
        assert!(rx.try_recv().is_err(), "an unchanged facing is silent");
    }

    #[test]
    fn the_turn_axis_carries_its_own_facing() {
        // Not one SET_FACING in the capture carries TURN_LEFT/TURN_RIGHT: a keyboard turn is fully
        // described by its flag (observers rotate at the turn rate while it's set), so the facing it
        // sweeps needs no packet of its own — and, crucially, the suppressed frames leave no catch-up
        // packet behind, because the change detector tracks the previous frame's facing regardless.
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut player = Player {
            move_flags: move_flags::FORWARD | move_flags::TURN_RIGHT,
            ..Default::default()
        };
        for (i, yaw) in [0.3_f32, 0.6, 0.9].into_iter().enumerate() {
            player.face_yaw = yaw;
            stream_self_movement(
                &tx,
                &mut player,
                move_flags::FORWARD | move_flags::TURN_RIGHT,
                0.0,
                ArcEdges {
                    jumped: false,
                    air_nudged: false,
                    landed: false,
                    started_falling: false,
                    fall_time: 0,
                },
                0.05 * (i as f32 + 1.0),
                &[],
                None,
            );
            assert!(
                rx.try_recv().is_err(),
                "the turn axis is silent (frame {i})"
            );
        }
        // Release the turn key with the facing unchanged since the last swept frame: the STOP_TURN
        // goes out and nothing follows it.
        stream_self_movement(
            &tx,
            &mut player,
            move_flags::FORWARD,
            0.0,
            ArcEdges {
                jumped: false,
                air_nudged: false,
                landed: false,
                started_falling: false,
                fall_time: 0,
            },
            0.2,
            &[],
            None,
        );
        let ClientCommand::Move { kind, .. } = rx.try_recv().expect("the STOP_TURN") else {
            panic!("expected a Move command");
        };
        assert_eq!(kind, MoveKind::StopTurn);
        assert!(
            rx.try_recv().is_err(),
            "no catch-up SET_FACING after a suppressed turn"
        );
    }

    #[test]
    fn a_transition_does_not_swallow_the_facing_report() {
        // The capture repeatedly shows both in one millisecond (`+4971` SET_FACING & START_STRAFE_
        // RIGHT). They are two independent emitters in the reference, so a frame that both turns and
        // changes axis puts both on the wire.
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut player = Player {
            move_flags: move_flags::FORWARD,
            ..Default::default()
        };
        player.face_yaw = 1.2;
        stream_self_movement(
            &tx,
            &mut player,
            move_flags::FORWARD | move_flags::STRAFE_RIGHT,
            0.0,
            ArcEdges {
                jumped: false,
                air_nudged: false,
                landed: false,
                started_falling: false,
                fall_time: 0,
            },
            0.05,
            &[],
            None,
        );
        let kinds: Vec<_> = rx
            .try_iter()
            .map(|c| match c {
                ClientCommand::Move { kind, .. } => kind,
                _ => panic!("expected Move commands"),
            })
            .collect();
        assert_eq!(kinds, vec![MoveKind::StartStrafeRight, MoveKind::SetFacing]);
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
