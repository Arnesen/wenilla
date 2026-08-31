//! Golden tests for the **party quest-share** opcode family (decision 1733) — the quest log's
//! *Share Quest* push, the escort confirm, and the two-way verdict relay. CMSG bodies are asserted
//! byte-exact against the builder output; SMSG bodies are hand-built per the vmangos layouts (cited
//! inline) and round-tripped through `parse_server` + `decode`. See `tests/common` for the shared
//! `hx()` fixture helper and methodology note.
//!
//! The property worth a test file of its own is the **direction flip** on `MSG_QUEST_PUSH_RESULT`:
//! one opcode number, two meanings for its `u64`. Going up it is the sharer we are answering;
//! coming down it is the party member the verdict is *about*. Nothing in the type system can catch
//! a swap, so it is pinned here from both ends.

mod common;

use benilla_protocol::events::{decode, SessionEvent};
use benilla_protocol::messages::{self, opcode, quest_flags, QuestShareMsg};
use benilla_protocol::ServerPacket;
use common::hx;

/// The three CMSG builders, byte-exact (vmangos `Server/Packets/Quest.cpp:63-77`).
#[test]
fn cmsg_bodies_golden() {
    // CMSG_PUSHQUESTTOPARTY / CMSG_QUEST_CONFIRM_ACCEPT: one u32 quest id, nothing else — the
    // server addresses the group itself, so no member guid rides along.
    assert_eq!(
        messages::push_quest_to_party(0x1234),
        hx("34120000"),
        "CMSG_PUSHQUESTTOPARTY body"
    );
    assert_eq!(
        messages::quest_confirm_accept(0x1234),
        hx("34120000"),
        "CMSG_QUEST_CONFIRM_ACCEPT body"
    );

    // MSG_QUEST_PUSH_RESULT going UP: u64 guid, then the u8 verdict. The guid is the SHARER we are
    // answering (`Quest.cpp:73-77`).
    assert_eq!(
        messages::quest_push_result(0x1234_5678_9abc_def0, QuestShareMsg::DECLINE_QUEST),
        hx("f0debc9a7856341203"),
        "MSG_QUEST_PUSH_RESULT (client) body"
    );
}

/// `MSG_QUEST_PUSH_RESULT` coming DOWN (`Quest.cpp:81-85`, written by
/// `Player::SendPushToPartyResponse`): the same two fields, but the guid is now the party MEMBER
/// the verdict concerns — never the sharer, and never us.
#[test]
fn push_result_decodes_member_and_verdict() {
    let body = hx("f0debc9a7856341200");
    let p = messages::parse_server(opcode::MSG_QUEST_PUSH_RESULT, &body).unwrap();
    match &p {
        ServerPacket::QuestPushResult(r) => {
            assert_eq!(r.member, 0x1234_5678_9abc_def0);
            assert_eq!(r.msg, QuestShareMsg::SHARING_QUEST);
        }
        other => panic!("expected QuestPushResult, got {}", other.name()),
    }
    match decode(p).as_slice() {
        [SessionEvent::QuestPushResult { member, msg }] => {
            assert_eq!(*member, 0x1234_5678_9abc_def0);
            assert_eq!(*msg, QuestShareMsg::SHARING_QUEST);
        }
        other => panic!("push result decoded to {} events", other.len()),
    }
}

/// The whole verdict enum survives the round trip, including a value the client has no message
/// for: an unmapped byte must reach the app as data, not kill the packet (the display table is
/// where "show nothing" is decided).
#[test]
fn every_verdict_byte_round_trips() {
    for raw in [0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0xFF] {
        let mut body = 1u64.to_le_bytes().to_vec();
        body.push(raw);
        match messages::parse_server(opcode::MSG_QUEST_PUSH_RESULT, &body).unwrap() {
            ServerPacket::QuestPushResult(r) => {
                assert_eq!(r.msg, QuestShareMsg(raw), "verdict {raw}");
                assert_eq!(r.member, 1);
            }
            other => panic!("verdict {raw} parsed as {}", other.name()),
        }
    }
}

/// `SMSG_QUEST_CONFIRM_ACCEPT` (`Quest.cpp:131-136`): `u32 questId, cstr questTitle, u64
/// senderGuid` — note the title sits BETWEEN the two numbers, so a reader that grouped the
/// scalars would decode the guid out of the title's tail.
#[test]
fn confirm_accept_decodes_id_title_sender() {
    let mut body = 1234u32.to_le_bytes().to_vec();
    body.extend_from_slice(b"Escort Duty\0");
    body.extend_from_slice(&0x0000_0000_0000_002Au64.to_le_bytes());

    let p = messages::parse_server(opcode::SMSG_QUEST_CONFIRM_ACCEPT, &body).unwrap();
    match decode(p).as_slice() {
        [SessionEvent::QuestConfirmAccept(c)] => {
            assert_eq!(c.quest_id, 1234);
            assert_eq!(c.title, "Escort Duty");
            assert_eq!(c.sender, 0x2A);
        }
        other => panic!("confirm accept decoded to {} events", other.len()),
    }
}

/// The two flag bits the share flow reads, pinned against vmangos `QuestDef.h:145-160`. They are
/// the only route by which the client learns a quest is shareable or an escort — the giver panels
/// carry no flags at all.
#[test]
fn share_quest_flags_are_the_documented_bits() {
    assert_eq!(quest_flags::PARTY_ACCEPT, 0x2);
    assert_eq!(quest_flags::SHARABLE, 0x8);
}
