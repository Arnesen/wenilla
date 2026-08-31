//! Oracle-free golden tests for the instance/raid **lockout message** family (decision 1748): the
//! six inbound bodies, the one outbound body, and the two enums the client's own dispatch tables
//! define.
//!
//! Every layout here is the *client's* read order, taken from the five handlers registered at
//! `0x498680`-`0x4986cf` plus `0x4e7e60`, and cross-checked against vmangos
//! `Server/Packets/Misc.{h,cpp}`. `hx(...)` bodies round-tripped through `parse_server`, the
//! idiom of `tests/bank.rs`.

use benilla_protocol::events::{decode, SessionEvent};
use benilla_protocol::messages::{
    self, opcode, InstanceResetFailure, RaidInstanceMessage, RaidInstanceWarning,
};
use benilla_protocol::ServerPacket;

fn hx(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// `CMSG_RESET_INSTANCES` — empty, both in the binding (`0x48a6b0` writes no field before its
/// send) and in vmangos's handler (which reads none).
#[test]
fn reset_instances_body_is_empty() {
    assert_eq!(messages::reset_instances(), Vec::<u8>::new());
}

/// `SMSG_RAID_INSTANCE_MESSAGE`: `u32 type`, `u32 mapId`, `u32 secondsUntilReset`, in that order
/// — the client reads the three at `0x49e1cd`/`0x49e1d8`/`0x49e1e3` and uses the SECOND for the
/// `Map.dbc` lookup, which is what fixes the order (a type/map swap would name the wrong instance
/// and pick the wrong template, silently).
#[test]
fn raid_instance_message_wire() {
    // type 4 (WELCOME), map 409 (Molten Core), 3 d 2 h 0 m = 266400 s.
    let body = hx(concat!("04000000", "99010000", "a0100400"));
    let p = messages::parse_server(opcode::SMSG_RAID_INSTANCE_MESSAGE, &body).unwrap();
    match &p {
        ServerPacket::RaidInstanceMessage { message } => {
            assert_eq!(
                *message,
                RaidInstanceMessage {
                    message_type: 4,
                    map: 409,
                    reset: 266_400,
                }
            );
        }
        other => panic!("expected RaidInstanceMessage, got {}", other.name()),
    }
    match decode(p).as_slice() {
        [SessionEvent::RaidInstanceMessage { message }] => {
            assert_eq!(message.map, 409);
            assert_eq!(message.reset, 266_400);
        }
        other => panic!("raid instance message decode: {other:?}"),
    }

    // A body one field short is a parse failure, not a zero-filled message.
    assert!(
        messages::parse_server(opcode::SMSG_RAID_INSTANCE_MESSAGE, &hx("0400000099010000"))
            .is_err()
    );
}

/// The four warning types and the two the reference's jump table drops. `dec eax; cmp eax,3; ja`
/// at `0x49e246` means 0 and ≥ 5 leave the handler in silence — including the value later clients
/// call `RAID_INSTANCE_EXPIRED` (5).
#[test]
fn raid_instance_warning_types() {
    use RaidInstanceWarning as W;
    assert_eq!(W::from_wire(1), Some(W::Hours));
    assert_eq!(W::from_wire(2), Some(W::Minutes));
    assert_eq!(W::from_wire(3), Some(W::MinutesSoon));
    assert_eq!(W::from_wire(4), Some(W::Welcome));
    assert_eq!(W::from_wire(0), None);
    assert_eq!(
        W::from_wire(5),
        None,
        "RAID_INSTANCE_EXPIRED prints nothing"
    );
    assert_eq!(W::from_wire(u32::MAX), None);

    assert_eq!(W::Hours.token(), "RAID_INSTANCE_WARNING_HOURS");
    assert_eq!(W::Minutes.token(), "RAID_INSTANCE_WARNING_MIN");
    assert_eq!(W::MinutesSoon.token(), "RAID_INSTANCE_WARNING_MIN_SOON");
    assert_eq!(W::Welcome.token(), "RAID_INSTANCE_WELCOME");
}

/// `SMSG_INSTANCE_RESET`: one `u32` map id (`0x49e481`).
#[test]
fn instance_reset_wire() {
    let p = messages::parse_server(opcode::SMSG_INSTANCE_RESET, &hx("24000000")).unwrap();
    match &p {
        ServerPacket::InstanceReset { map } => assert_eq!(*map, 36), // Deadmines
        other => panic!("expected InstanceReset, got {}", other.name()),
    }
    match decode(p).as_slice() {
        [SessionEvent::InstanceReset { map: 36 }] => {}
        other => panic!("instance reset decode: {other:?}"),
    }
    assert!(messages::parse_server(opcode::SMSG_INSTANCE_RESET, &hx("2400")).is_err());
}

/// `SMSG_INSTANCE_RESET_FAILED`: `u32 reason` FIRST, then `u32 mapId` (`0x49e54d`/`0x49e558`) —
/// the opposite order to the reset success packet's single field, and the pair vmangos writes in
/// `InstanceResetFailed::AppendBodyTo`.
#[test]
fn instance_reset_failed_wire() {
    let body = hx(concat!("01000000", "24000000")); // OFFLINE, Deadmines
    let p = messages::parse_server(opcode::SMSG_INSTANCE_RESET_FAILED, &body).unwrap();
    match &p {
        ServerPacket::InstanceResetFailed { failure } => {
            assert_eq!(failure.reason, 1);
            assert_eq!(failure.map, 36);
        }
        other => panic!("expected InstanceResetFailed, got {}", other.name()),
    }
    match decode(p).as_slice() {
        [SessionEvent::InstanceResetFailed { failure }] => {
            assert_eq!(
                InstanceResetFailure::from_wire(failure.reason),
                Some(InstanceResetFailure::Offline)
            );
        }
        other => panic!("instance reset failed decode: {other:?}"),
    }
}

/// The three refusal reasons, and the silent tail. vmangos's own enum names 3
/// `INSTANCERESET_FAIL_SILENTLY` "as well as any above this"; the reference falls through those
/// to its chat call with an uninitialized buffer, which we deliberately do not reproduce.
#[test]
fn instance_reset_failure_reasons() {
    use InstanceResetFailure as F;
    assert_eq!(F::from_wire(0), Some(F::General));
    assert_eq!(F::from_wire(1), Some(F::Offline));
    assert_eq!(F::from_wire(2), Some(F::Zoning));
    assert_eq!(F::from_wire(3), None, "INSTANCERESET_FAIL_SILENTLY");
    assert_eq!(F::from_wire(99), None);

    assert_eq!(F::General.token(), "INSTANCE_RESET_FAILED");
    assert_eq!(F::Offline.token(), "INSTANCE_RESET_FAILED_OFFLINE");
    assert_eq!(F::Zoning.token(), "INSTANCE_RESET_FAILED_ZONING");
}

/// The three bare-`u32` bodies that are not a map name: the save-created flag (`0x4e7e6c`), the
/// last-instance map (`0x49e676`) and the ownership flag (`0x49e6c6`). The ownership flag is
/// narrowed to a bool at the event layer because the reference's reader is a `test eax,eax`.
#[test]
fn save_created_last_instance_and_ownership_wire() {
    let p = messages::parse_server(opcode::SMSG_INSTANCE_SAVE_CREATED, &hx("00000000")).unwrap();
    match decode(p).as_slice() {
        [SessionEvent::InstanceSaveCreated { flag: 0 }] => {}
        other => panic!("save created decode: {other:?}"),
    }

    let p = messages::parse_server(opcode::SMSG_UPDATE_LAST_INSTANCE, &hx("99010000")).unwrap();
    match decode(p).as_slice() {
        [SessionEvent::UpdateLastInstance { map: 409 }] => {}
        other => panic!("last instance decode: {other:?}"),
    }

    for (body, owns) in [("00000000", false), ("01000000", true), ("07000000", true)] {
        let p = messages::parse_server(opcode::SMSG_UPDATE_INSTANCE_OWNERSHIP, &hx(body)).unwrap();
        match decode(p).as_slice() {
            [SessionEvent::UpdateInstanceOwnership { owns: got }] => assert_eq!(*got, owns),
            other => panic!("ownership decode: {other:?}"),
        }
    }
}
