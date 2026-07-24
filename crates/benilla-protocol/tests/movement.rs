//! Player-movement relay + teleport-ack wire tests: the `[packed guid][MovementInfo]` relay shape
//! that rebroadcasts other players' `MSG_MOVE_*` locomotion (including the jump ballistic tail), and
//! `MSG_MOVE_TELEPORT_ACK`'s server-form pose. Split out of the former `tests/messages.rs` — see
//! `tests/common` for the shared fixtures and methodology note.

mod common;

use benilla_protocol::events::{decode, SessionEvent};
use benilla_protocol::messages::{self, MovementInfo, TransportPose};
use benilla_protocol::wire::{write_packed_guid, Vector3d};
use benilla_protocol::ServerPacket;
use common::hx;

#[test]
fn movement_relay_decodes_to_unit_move() {
    // A relayed player move: [packed mover guid][MovementInfo]. Packed guid 0xAA = mask 0x01 + byte
    // 0xAA; the MovementInfo body is the same 28-byte FORWARD info pinned in `client_bodies_golden`
    // (flags 0x1, time, pos, orientation 1.25, fall_time). The server rebroadcasts with the mover's
    // opcode — here MSG_MOVE_START_FORWARD — which is how another player's walking reaches us.
    let body = hx("01aa0100000004030201cdd70bc6357e04c3f90fa7420000a03f00000000");
    match messages::parse_server(messages::opcode::MSG_MOVE_START_FORWARD, &body).unwrap() {
        ServerPacket::PlayerMove {
            guid,
            opcode,
            flags,
            position,
            orientation,
            jump,
            ..
        } => {
            assert_eq!(guid, 0xAA);
            assert_eq!(opcode, messages::opcode::MSG_MOVE_START_FORWARD);
            assert_eq!(flags, 0x1);
            assert_eq!(
                (position.x, position.y, position.z),
                (-8949.95, -132.493, 83.5312)
            );
            assert_eq!(orientation, 1.25);
            assert_eq!(jump, None, "a non-jumping relay carries no jump tail");
        }
        p => panic!("expected PlayerMove, got {}", p.name()),
    }
    // The whole relay family routes through the same body shape — spot-check a couple more opcodes and
    // the fan-out into a SessionEvent the ECS consumes.
    for op in [
        messages::opcode::MSG_MOVE_HEARTBEAT,
        messages::opcode::MSG_MOVE_SET_FACING,
        messages::opcode::MSG_MOVE_STOP,
    ] {
        let events = decode(messages::parse_server(op, &body).unwrap());
        assert!(
            matches!(
                events.as_slice(),
                [SessionEvent::UnitMove {
                    guid: 0xAA,
                    orientation,
                    flags: 0x1,
                    ..
                }] if *orientation == 1.25
            ),
            "opcode {op:#06x} should decode to one UnitMove, got {events:?}"
        );
    }
}

#[test]
fn jump_relay_round_trips_the_ballistic_tail() {
    // A jumping MovementInfo (JUMPING 0x2000 | FORWARD) carries the ballistic launch tail. Round-trip it
    // through write → parse to pin the wire layout: `fall_time` is a u32 (ms), and the jump tail order is
    // `zspeed, cosAngle, sinAngle, xyspeed` — cos *before* sin (VERIFIED vmangos `MovementInfo::Read`).
    let mi = MovementInfo {
        flags: 0x2000 | 0x1,
        timestamp: 0x0102_0304,
        position: Vector3d {
            x: 1.0,
            y: 2.0,
            z: 3.0,
        },
        orientation: 0.5,
        transport: None, // not on a transport — no transport tail is written
        pitch: 0.0,      // not swimming — no pitch tail is written
        fall_time: 250,
        jump: Some(messages::JumpInfo {
            zspeed: 7.955_547,
            cos_angle: 0.25,
            sin_angle: 0.75,
            xy_speed: 7.0,
        }),
    };
    let mut body = hx("01aa"); // [packed guid 0xAA][MovementInfo] — the relay shape.
    body.extend_from_slice(&messages::movement(&mi));
    match messages::parse_server(messages::opcode::MSG_MOVE_JUMP, &body).unwrap() {
        ServerPacket::PlayerMove {
            flags,
            fall_time,
            jump,
            ..
        } => {
            assert_eq!(flags, 0x2001);
            assert_eq!(fall_time, 250, "fall_time is a u32 ms count, not an f32");
            let j = jump.expect("a JUMPING relay carries the ballistic tail");
            assert_eq!(j.zspeed, 7.955_547);
            assert_eq!(
                j.cos_angle, 0.25,
                "cosAngle is the 2nd jump float (before sin)"
            );
            assert_eq!(j.sin_angle, 0.75, "sinAngle is the 3rd jump float");
            assert_eq!(j.xy_speed, 7.0);
        }
        p => panic!("expected PlayerMove, got {}", p.name()),
    }
}

/// A relayed rider on a transport: `MOVEFLAG_ON_TRANSPORT | MOVEFLAG_SWIMMING | MOVEFLAG_JUMPING` all
/// set, so the transport pose, swim pitch, `fall_time`, and jump tail all ride the same packet — the
/// exact regression the old discard comment warned about (`messages/movement.rs`'s prior "the
/// transport ... tail is parsed to stay aligned but discarded"): a wrong transport-tail length would
/// misalign every tail after it. `write()` never serializes a transport tail (benilla doesn't set the
/// flag outbound — riding is decision 0438 phase 2), so this fixture is hand-built to the exact byte
/// order `read_movement_info` implements.
#[test]
fn movement_relay_surfaces_transport_pose_and_keeps_the_rest_aligned() {
    // An elevator (HIGH_TRANSPORT, 0xF120): template entry 900, DB spawn low guid 4242 — the standard
    // entry layout (guid.rs `entry()`).
    let transport_guid: u64 = 4242 | (900u64 << 24) | (0xF120u64 << 48);

    let mut body = Vec::new();
    write_packed_guid(0xAA, &mut body).unwrap(); // mover guid
    let flags: u32 = 0x0200_0000 | 0x20_0000 | 0x2000 | 0x1; // ON_TRANSPORT | SWIMMING | JUMPING | FORWARD
    body.extend_from_slice(&flags.to_le_bytes());
    body.extend_from_slice(&0x1122_3344u32.to_le_bytes()); // timestamp
    body.extend_from_slice(&10.0f32.to_le_bytes()); // position.x
    body.extend_from_slice(&20.0f32.to_le_bytes()); // position.y
    body.extend_from_slice(&30.0f32.to_le_bytes()); // position.z
    body.extend_from_slice(&0.75f32.to_le_bytes()); // orientation
    body.extend_from_slice(&transport_guid.to_le_bytes()); // ON_TRANSPORT tail: FULL u64 guid
    body.extend_from_slice(&1.0f32.to_le_bytes()); // local x
    body.extend_from_slice(&2.0f32.to_le_bytes()); // local y
    body.extend_from_slice(&3.0f32.to_le_bytes()); // local z
    body.extend_from_slice(&0.5f32.to_le_bytes()); // local o
    body.extend_from_slice(&(-0.3f32).to_le_bytes()); // SWIMMING tail: pitch
    body.extend_from_slice(&999u32.to_le_bytes()); // fall_time (u32 — must land right after pitch)
    body.extend_from_slice(&7.9f32.to_le_bytes()); // JUMPING tail: zspeed
    body.extend_from_slice(&0.6f32.to_le_bytes()); // cos_angle
    body.extend_from_slice(&0.8f32.to_le_bytes()); // sin_angle
    body.extend_from_slice(&5.0f32.to_le_bytes()); // xy_speed

    let want_transport = TransportPose {
        guid: transport_guid,
        pos: Vector3d {
            x: 1.0,
            y: 2.0,
            z: 3.0,
        },
        orientation: 0.5,
    };

    match messages::parse_server(messages::opcode::MSG_MOVE_HEARTBEAT, &body).unwrap() {
        ServerPacket::PlayerMove {
            orientation,
            transport,
            fall_time,
            jump,
            ..
        } => {
            assert_eq!(
                orientation, 0.75,
                "position/orientation land before the tail, unaffected"
            );
            assert_eq!(
                transport,
                Some(want_transport),
                "the ON_TRANSPORT tail surfaces as the rider's local pose"
            );
            assert_eq!(
                fall_time, 999,
                "fall_time still lands right after the transport + swim-pitch tails"
            );
            let j =
                jump.expect("the jump tail still parses after the transport/pitch/fall_time run");
            assert_eq!(j.zspeed, 7.9);
            assert_eq!(j.cos_angle, 0.6);
            assert_eq!(j.sin_angle, 0.8);
            assert_eq!(j.xy_speed, 5.0);
        }
        p => panic!("expected PlayerMove, got {}", p.name()),
    }

    // The same fixture through the SessionEvent fan-out: UnitMove carries the same transport pose and
    // the same post-tail fields.
    let events =
        decode(messages::parse_server(messages::opcode::MSG_MOVE_HEARTBEAT, &body).unwrap());
    match events.as_slice() {
        [SessionEvent::UnitMove {
            transport,
            fall_time,
            jump,
            ..
        }] => {
            assert_eq!(transport, &Some(want_transport));
            assert_eq!(*fall_time, 999);
            assert!(jump.is_some());
        }
        other => panic!("expected one UnitMove, got {other:?}"),
    }
}

#[test]
fn teleport_ack_parses_pose() {
    // MSG_MOVE_TELEPORT_ACK (server→client form): [packed guid][counter u32][MovementInfo]. Exercises
    // the refactored read_movement_info (non-transport path) end to end.
    let body = hx("01aa070000000100000004030201cdd70bc6357e04c3f90fa7420000a03f00000000");
    match messages::parse_server(messages::opcode::MSG_MOVE_TELEPORT_ACK, &body).unwrap() {
        ServerPacket::Teleport {
            guid,
            counter,
            position,
            orientation,
        } => {
            assert_eq!(guid, 0xAA);
            assert_eq!(counter, 7);
            assert_eq!(
                (position.x, position.y, position.z),
                (-8949.95, -132.493, 83.5312)
            );
            assert_eq!(orientation, 1.25);
        }
        p => panic!("expected Teleport, got {}", p.name()),
    }
}

/// The force-speed-change family, byte-exact both directions (a new wire body never lands without
/// a golden — method): the SMSG body is `[packed guid][u32 counter][f32 speed]` (vmangos
/// `SendSpeedChangeToController`, 5875 branch), and the ACK body is `[u64 FULL guid][u32 counter]
/// [MovementInfo][f32 speed]` (`MoveSpeedAck::ReadFromWorldPacket` — a plain `ObjectGuid`
/// extraction is a raw u64, `ObjectGuid.cpp:180`, NOT packed like the SMSG's).
#[test]
fn force_speed_change_parses_and_ack_body_golden() {
    use benilla_protocol::messages::SpeedKind;

    // SMSG_FORCE_RUN_SPEED_CHANGE: packed guid 8 (mask 0x01, one byte), counter 7, speed 14.0.
    let body = hx("01080700000000006041");
    let packet =
        messages::parse_server(messages::opcode::SMSG_FORCE_RUN_SPEED_CHANGE, &body).unwrap();
    match &packet {
        ServerPacket::ForceSpeedChange {
            guid: 8,
            kind: SpeedKind::Run,
            counter: 7,
            speed,
        } => assert_eq!(*speed, 14.0),
        _ => panic!("expected ForceSpeedChange (run, guid 8, counter 7)"),
    }
    match decode(packet).as_slice() {
        [SessionEvent::ForceSpeedChange {
            guid: 8,
            kind: SpeedKind::Run,
            counter: 7,
            speed,
        }] => assert_eq!(*speed, 14.0),
        other => panic!("expected one ForceSpeedChange event, got {other:?}"),
    }
    // Every kind maps to its own SMSG opcode (the walk/swim-back/turn-rate trio lives in the
    // 730-735 block, not the 226-231 run).
    for (op, kind) in [
        (
            messages::opcode::SMSG_FORCE_WALK_SPEED_CHANGE,
            SpeedKind::Walk,
        ),
        (
            messages::opcode::SMSG_FORCE_SWIM_SPEED_CHANGE,
            SpeedKind::Swim,
        ),
        (
            messages::opcode::SMSG_FORCE_TURN_RATE_CHANGE,
            SpeedKind::TurnRate,
        ),
    ] {
        match messages::parse_server(op, &body).unwrap() {
            ServerPacket::ForceSpeedChange { kind: k, .. } => assert_eq!(k, kind),
            _ => panic!("expected ForceSpeedChange for opcode {op:#06x}"),
        }
    }

    // The ACK body golden: full 8-byte guid, counter, a stationary MovementInfo (flags 0 => no
    // conditional tails), then the echoed speed.
    let info = MovementInfo {
        flags: 0,
        timestamp: 12345,
        position: Vector3d {
            x: 1.0,
            y: 2.0,
            z: 3.0,
        },
        orientation: 0.5,
        transport: None,
        pitch: 0.0,
        fall_time: 42,
        jump: None,
    };
    assert_eq!(
        messages::force_speed_ack(8, 7, &info, 14.0),
        hx("08000000000000000700000000000000393000000000803f00000040000040400000003f2a00000000006041"),
        "CMSG_FORCE_*_SPEED_CHANGE_ACK body"
    );
    // The ack opcode per kind — the run ack is 227, the walk ack lives in the high block (731).
    assert_eq!(SpeedKind::Run.ack_opcode(), 0x00E3);
    assert_eq!(SpeedKind::Walk.ack_opcode(), 0x02DB);
    assert_eq!(SpeedKind::TurnRate.ack_opcode(), 0x02DF);
}

/// The OBSERVER speed legs, byte-exact (decision 0441 — how an observed unit's mounted speed
/// reaches us; no counter, no ack on either): `SMSG_SPLINE_SET_*_SPEED` is `[packed guid]
/// [f32 speed]` (vmangos `SendSpeedChangeToAll` + the mid-spline observer branch), and
/// `MSG_MOVE_SET_*_SPEED` is `[packed guid][MovementInfo][f32 speed]`
/// (`SendSpeedChangeToObservers`, finalized branch) — a speed change AND a fresh pose, decoding
/// to a `UnitMove` + `SpeedChanged` pair.
#[test]
fn observer_speed_legs_parse_golden() {
    use benilla_protocol::messages::SpeedKind;

    // SMSG_SPLINE_SET_RUN_SPEED: packed guid 0xAA (mask 0x01, one byte), speed 14.0.
    let body = hx("01aa00006041");
    let packet =
        messages::parse_server(messages::opcode::SMSG_SPLINE_SET_RUN_SPEED, &body).unwrap();
    match &packet {
        ServerPacket::SplineSpeedChange {
            guid: 0xAA,
            kind: SpeedKind::Run,
            speed,
        } => assert_eq!(*speed, 14.0),
        p => panic!(
            "expected SplineSpeedChange (run, guid 0xAA), got {}",
            p.name()
        ),
    }
    match decode(packet).as_slice() {
        [SessionEvent::SpeedChanged {
            guid: 0xAA,
            kind: SpeedKind::Run,
            speed,
        }] => assert_eq!(*speed, 14.0),
        other => panic!("expected one SpeedChanged event, got {other:?}"),
    }
    // Each kind maps to its own opcode — spot-check the walk (769) and swim-back (770) slots,
    // whose order differs from the FORCE family's.
    for (op, kind) in [
        (
            messages::opcode::SMSG_SPLINE_SET_WALK_SPEED,
            SpeedKind::Walk,
        ),
        (
            messages::opcode::SMSG_SPLINE_SET_SWIM_BACK_SPEED,
            SpeedKind::SwimBack,
        ),
    ] {
        match messages::parse_server(op, &body).unwrap() {
            ServerPacket::SplineSpeedChange { kind: k, .. } => assert_eq!(k, kind),
            p => panic!("expected SplineSpeedChange for {op:#06x}, got {}", p.name()),
        }
    }

    // MSG_MOVE_SET_RUN_SPEED: [packed guid 0xAA][the shared 28-byte FORWARD MovementInfo golden]
    // [speed 14.0] — decodes to the pose + the speed, in one drain.
    let body = hx("01aa0100000004030201cdd70bc6357e04c3f90fa7420000a03f0000000000006041");
    let packet = messages::parse_server(messages::opcode::MSG_MOVE_SET_RUN_SPEED, &body).unwrap();
    match &packet {
        ServerPacket::MoveSetSpeed {
            guid: 0xAA,
            kind: SpeedKind::Run,
            flags: 0x1,
            orientation,
            speed,
            ..
        } => {
            assert_eq!(*orientation, 1.25);
            assert_eq!(*speed, 14.0);
        }
        p => panic!("expected MoveSetSpeed (run, guid 0xAA), got {}", p.name()),
    }
    match decode(packet).as_slice() {
        [SessionEvent::UnitMove {
            guid: 0xAA,
            flags: 0x1,
            position,
            ..
        }, SessionEvent::SpeedChanged {
            guid: 0xAA,
            kind: SpeedKind::Run,
            speed,
        }] => {
            assert_eq!(position, &[-8949.95, -132.493, 83.5312]);
            assert_eq!(*speed, 14.0);
        }
        other => panic!("expected UnitMove + SpeedChanged, got {other:?}"),
    }

    // SMSG_MOUNTRESULT / SMSG_DISMOUNTRESULT: one u32 code (OK = 10 / 3).
    match decode(
        messages::parse_server(messages::opcode::SMSG_MOUNTRESULT, &hx("0a000000")).unwrap(),
    )
    .as_slice()
    {
        [SessionEvent::MountResult {
            mount: true,
            code: 10,
        }] => {}
        other => panic!("expected MountResult(mount, 10), got {other:?}"),
    }
    match decode(
        messages::parse_server(messages::opcode::SMSG_DISMOUNTRESULT, &hx("03000000")).unwrap(),
    )
    .as_slice()
    {
        [SessionEvent::MountResult {
            mount: false,
            code: 3,
        }] => {}
        other => panic!("expected MountResult(dismount, 3), got {other:?}"),
    }

    // SMSG_MOUNTSPECIAL_ANIM: one raw u64 guid, NOT packed (vmangos
    // `HandleMountSpecialAnimOpcode` writes `data << GetObjectGuid()` — full 8 bytes).
    match decode(
        messages::parse_server(
            messages::opcode::SMSG_MOUNTSPECIAL_ANIM,
            &hx("0400000000000040"),
        )
        .unwrap(),
    )
    .as_slice()
    {
        [SessionEvent::MountSpecial {
            guid: 0x4000_0000_0000_0004,
        }] => {}
        other => panic!("expected MountSpecial(player 4), got {other:?}"),
    }
}
