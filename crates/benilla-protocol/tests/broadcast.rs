//! World-broadcast wire decode goldens (mirrors `src/messages/broadcast.rs`):
//! `SMSG_SERVER_MESSAGE`, `SMSG_ZONE_UNDER_ATTACK`, `SMSG_DEFENSE_MESSAGE` and the bodyless
//! `SMSG_CHAT_RESTRICTED`. Bytes hand-computed from the vmangos layout (citations inline),
//! independent of the Rust decoder — see `tests/common` for the shared `hx` fixture helper.
//!
//! These four are the packets nobody asks for: they arrive because the world did something, not
//! because this client did. Three of them had no decode at all until now, which meant a shutdown
//! countdown, a civilian-kill alarm and an Eastern Plaguelands tower capture all reached benilla as
//! an unhandled opcode — the server drops you with no warning, and the tower changes hands in
//! silence.

mod common;

use benilla_protocol::events::{decode, SessionEvent};
use benilla_protocol::messages;
use benilla_protocol::ServerPacket;
use common::hx;

/// `SMSG_SERVER_MESSAGE` (VERIFIED vmangos `WorldPackets::Misc::ServerMessage::AppendBodyTo`,
/// `Server/Packets/Misc.cpp:341-345`: `buffer << messageType; buffer << text`): `u32` + cstring.
///
/// Both shapes the five `ServerMessageType` producers actually send (`World.cpp:2742`/`2761`): a
/// countdown carrying `secsToTimeString(...)` for its row's `%s`, and a cancellation carrying an
/// **empty** string, because rows 4 and 5 have no `%s` to fill. The empty text is a value, not a
/// short packet — the client's own handler tests `[ebp-0x400] == 0` at `0x49dfc8` and takes a
/// straight copy of the row instead of a format.
#[test]
fn server_message_decodes_a_countdown_and_a_cancellation() {
    let body = hx("020000003135204d696e7574657300"); // type 2 (RESTART_TIME), "15 Minutes"
    match messages::parse_server(messages::opcode::SMSG_SERVER_MESSAGE, &body).unwrap() {
        ServerPacket::ServerMessage {
            message_type,
            ref text,
        } => {
            assert_eq!(message_type, 2);
            assert_eq!(text, "15 Minutes");
        }
        other => panic!("expected ServerMessage, got {}", other.name()),
    }
    match &decode(messages::parse_server(messages::opcode::SMSG_SERVER_MESSAGE, &body).unwrap())[..]
    {
        [SessionEvent::ServerMessage { message_type, text }] => {
            assert_eq!(*message_type, 2);
            assert_eq!(text, "15 Minutes");
        }
        other => panic!("server message decode: {} events", other.len()),
    }

    let body = hx("0400000000"); // type 4 (SHUTDOWN_CANCELLED), empty text
    match messages::parse_server(messages::opcode::SMSG_SERVER_MESSAGE, &body).unwrap() {
        ServerPacket::ServerMessage {
            message_type,
            ref text,
        } => {
            assert_eq!(message_type, 4);
            assert!(text.is_empty());
        }
        other => panic!("expected ServerMessage, got {}", other.name()),
    }
}

/// `SMSG_ZONE_UNDER_ATTACK` (VERIFIED vmangos
/// `WorldPackets::Misc::ZoneUnderAttack::AppendBodyTo`, `Server/Packets/Misc.cpp:451-454`): one
/// `u32` `AreaTable.dbc` id and nothing else. 40 is Westfall.
#[test]
fn zone_under_attack_decodes() {
    let body = hx("28000000");
    let p = messages::parse_server(messages::opcode::SMSG_ZONE_UNDER_ATTACK, &body).unwrap();
    assert!(matches!(p, ServerPacket::ZoneUnderAttack { area_id: 40 }));
    assert!(matches!(
        decode(p)[..],
        [SessionEvent::ZoneUnderAttack { area_id: 40 }]
    ));
}

/// `SMSG_DEFENSE_MESSAGE` (VERIFIED vmangos `Map::SendDefenseMessage`, `Maps/Map.cpp:1868-1884`):
/// `u32 zoneId`, `u32 strlen(text) + 1`, then the NUL-terminated text. 139 is the Eastern
/// Plaguelands, whose tower captures are this packet's only vmangos sender.
///
/// The length counts the terminator (`0x30` = 48 for a 47-byte string) and is redundant with the
/// string; the reference reads and discards it too.
#[test]
fn defense_message_decodes() {
    let body = hx(
        "8b000000300000004e6f7274687061737320546f77657220686173206265656e2\
         074616b656e2062792074686520416c6c69616e63652100",
    );
    match messages::parse_server(messages::opcode::SMSG_DEFENSE_MESSAGE, &body).unwrap() {
        ServerPacket::DefenseMessage { zone_id, ref text } => {
            assert_eq!(zone_id, 139);
            assert_eq!(text, "Northpass Tower has been taken by the Alliance!");
        }
        other => panic!("expected DefenseMessage, got {}", other.name()),
    }
    match &decode(messages::parse_server(messages::opcode::SMSG_DEFENSE_MESSAGE, &body).unwrap())[..]
    {
        [SessionEvent::DefenseMessage { zone_id, text }] => {
            assert_eq!(*zone_id, 139);
            assert_eq!(text, "Northpass Tower has been taken by the Alliance!");
        }
        other => panic!("defense message decode: {} events", other.len()),
    }
}

/// `SMSG_CHAT_RESTRICTED` (VERIFIED vmangos `WorldPackets::Chat::ChatRestricted::AppendBodyTo`,
/// `Server/Packets/Chat.cpp:21-23`): an **empty** body, exactly like `SMSG_CHAT_WRONG_FACTION`.
/// The client's arm reads nothing off the wire either (`0x5e4a09` is `push 0x1c3; call 0x496720`).
#[test]
fn chat_restricted_is_bodyless() {
    let p = messages::parse_server(messages::opcode::SMSG_CHAT_RESTRICTED, &[]).unwrap();
    assert!(matches!(p, ServerPacket::ChatRestricted));
    assert!(matches!(decode(p)[..], [SessionEvent::ChatRestricted]));
}
