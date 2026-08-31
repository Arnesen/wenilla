//! The **party quest-share** wire (opcodes 411-413 + 630, VERIFIED vmangos `Opcodes_1_12_1.h`) —
//! decision 1733, the third slice of the quest arc after [`super::giver`] and [`super::log`].
//!
//! Two independent flows share this file because they share the same server-side latch (vmangos
//! `Player::SetQuestShareInfo`/`ClearQuestShareInfo`, one `{sharerGuid, questId}` pair per player):
//!
//! 1. **The push** — the quest log's *Share Quest* button. `CMSG_PUSHQUESTTOPARTY{questId}` → the server
//!    walks the group and, per member, sends the SHARER a `MSG_QUEST_PUSH_RESULT`
//!    verdict; an eligible member additionally gets an ordinary `SMSG_QUESTGIVER_QUEST_DETAILS`
//!    whose `npcGuid` is the **sharer's player guid** (vmangos `QuestHandler.cpp:403-459`). That is
//!    the whole reason the receiver needs no new inbound opcode: a shared quest arrives on the
//!    detail panel already built, and Accept answers it with the ordinary
//!    `CMSG_QUESTGIVER_ACCEPT_QUEST` addressed to that player guid (the server's own
//!    `TYPEID_PLAYER` arm, `QuestHandler.cpp:111-114`, gated on `Player::CanShareQuest`).
//!    A **decline** is the one verdict the client itself originates: `MSG_QUEST_PUSH_RESULT` back
//!    up, which the server relays to the sharer with the *receiver's* guid substituted
//!    (`QuestHandler.cpp:461-478`).
//! 2. **The escort confirm** — `QUEST_FLAGS_PARTY_ACCEPT` (0x2). When a party member accepts such
//!    a quest, every other eligible member gets `SMSG_QUEST_CONFIRM_ACCEPT` and answers with
//!    `CMSG_QUEST_CONFIRM_ACCEPT` if they say yes (vmangos `QuestHandler.cpp:172-193`,
//!    `Player.cpp:14571-14594`). Saying *no* sends nothing at all — the server's latch is cleared
//!    by the next thing that touches it.
//!
//! **Who the guid names flips direction on `MSG_QUEST_PUSH_RESULT`, and that is the one trap here.**
//! The opcode is `MSG_*` — the same number both ways — but the `u64` never means the same thing
//! twice: going *up* it is the sharer the receiver is answering; coming *down* it is the OTHER
//! party (`Player::SendPushToPartyResponse` writes `pPlayer`'s guid, the member the verdict is
//! *about*, `Player.cpp:14596-14608`), so the sharer's `%s` fills from it directly.

use std::io;

use crate::wire::{read_cstring, read_u32_le, read_u64_le, read_u8};

/// `QuestShareMessages` (vmangos `QuestDef.h:62-70`) — the `u8` verdict `MSG_QUEST_PUSH_RESULT`
/// carries in both directions. The server originates 0/1/4/5/6/7/8 while walking the group, and 2
/// when the receiver accepts; the client originates 3 when the receiver declines.
///
/// Kept as a plain `u8` newtype rather than an enum with a `TryFrom` because the *display* mapping
/// (which `ERR_QUEST_PUSH_*` message id each value selects, and on which surface) belongs to the
/// app layer's message table, not to the wire — and an unknown value must survive the parse the way
/// every other unmapped code in this crate does, not become a parse error that drops the packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QuestShareMsg(pub u8);

impl QuestShareMsg {
    /// `QUEST_PARTY_MSG_SHARING_QUEST` — the server's opening "the push went out to this member".
    pub const SHARING_QUEST: Self = Self(0);
    /// `QUEST_PARTY_MSG_CANT_TAKE_QUEST` — the member fails `CanTakeQuest` (level, race, prereqs).
    pub const CANT_TAKE_QUEST: Self = Self(1);
    /// `QUEST_PARTY_MSG_ACCEPT_QUEST` — the member accepted (server-originated, on their
    /// `CMSG_QUESTGIVER_ACCEPT_QUEST`).
    pub const ACCEPT_QUEST: Self = Self(2);
    /// `QUEST_PARTY_MSG_DECLINE_QUEST` — **the one value the client sends**.
    pub const DECLINE_QUEST: Self = Self(3);
    /// `QUEST_PARTY_MSG_TOO_FAR` — outside `QUEST_SHARE_DISTANCE` (14.0 yd, vmangos `Object.h:72`).
    pub const TOO_FAR: Self = Self(4);
    /// `QUEST_PARTY_MSG_BUSY` — the member already has a share latched.
    pub const BUSY: Self = Self(5);
    /// `QUEST_PARTY_MSG_LOG_FULL` — the member's quest log is full.
    pub const LOG_FULL: Self = Self(6);
    /// `QUEST_PARTY_MSG_HAVE_QUEST` — the member is already on the quest.
    pub const HAVE_QUEST: Self = Self(7);
    /// `QUEST_PARTY_MSG_FINISH_QUEST` — the member has already completed the quest.
    pub const FINISH_QUEST: Self = Self(8);
}

/// `MSG_QUEST_PUSH_RESULT` as the **server** sends it (vmangos
/// `Server/Packets/Quest.cpp:81-85`): `u64 guid, u8 msg`.
///
/// `guid` is the party member the verdict is *about* — never the sharer, and never us (see the
/// module header's direction trap). It is what fills the `%s` of every `ERR_QUEST_PUSH_*` string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestPushResult {
    /// The party member the verdict concerns.
    pub member: u64,
    /// The verdict.
    pub msg: QuestShareMsg,
}

/// `SMSG_QUEST_CONFIRM_ACCEPT` (vmangos `Server/Packets/Quest.cpp:131-136`): `u32 questId, cstr
/// questTitle, u64 senderGuid` — the `QUEST_FLAGS_PARTY_ACCEPT` (escort) confirm box.
///
/// The **title travels on the wire** rather than being looked up from the client's template cache,
/// because the server localizes it per receiver session (`Player::SendQuestConfirmAccept`,
/// `Player.cpp:14575-14586`) — and because the receiver may never have queried this quest at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestConfirmAccept {
    /// The quest being started — echoed back verbatim in [`quest_confirm_accept`].
    pub quest_id: u32,
    /// The quest title, already localized by the server.
    pub title: String,
    /// The party member who accepted it — the first `%s` of `QUEST_ACCEPT`.
    pub sender: u64,
}

// ── CMSG encoders ─────────────────────────────────────────────────────────────────────────────────

/// Body of `CMSG_PUSHQUESTTOPARTY` (vmangos `Quest.cpp:68-71`): one `u32` quest id. The quest log's
/// *Share Quest* button; the server addresses the group itself, so no member guid rides along.
pub fn push_quest_to_party(quest_id: u32) -> Vec<u8> {
    quest_id.to_le_bytes().to_vec()
}

/// Body of `CMSG_QUEST_CONFIRM_ACCEPT` (vmangos `Quest.cpp:63-66`): one `u32` quest id — the
/// escort confirm's Yes.
pub fn quest_confirm_accept(quest_id: u32) -> Vec<u8> {
    quest_id.to_le_bytes().to_vec()
}

/// Body of `MSG_QUEST_PUSH_RESULT` as the **client** sends it (vmangos `Quest.cpp:73-77`): `u64
/// guid, u8 msg`. `guid` is the *sharer* we are answering — the `npcGuid` the shared
/// `SMSG_QUESTGIVER_QUEST_DETAILS` arrived under.
///
/// vmangos ignores the guid entirely (it re-derives the sharer from its own latch,
/// `QuestHandler.cpp:461-467`), so a wrong value here would go unnoticed against this server. It is
/// sent correctly anyway: the field is the protocol's, not the server's.
pub fn quest_push_result(sharer: u64, msg: QuestShareMsg) -> Vec<u8> {
    let mut b = Vec::with_capacity(9);
    b.extend_from_slice(&sharer.to_le_bytes());
    b.push(msg.0);
    b
}

// ── SMSG readers ──────────────────────────────────────────────────────────────────────────────────

/// Read `MSG_QUEST_PUSH_RESULT` (see [`QuestPushResult`]).
pub(in crate::messages) fn read_quest_push_result(r: &mut &[u8]) -> io::Result<QuestPushResult> {
    Ok(QuestPushResult {
        member: read_u64_le(r)?,
        msg: QuestShareMsg(read_u8(r)?),
    })
}

/// Read `SMSG_QUEST_CONFIRM_ACCEPT` (see [`QuestConfirmAccept`]).
pub(in crate::messages) fn read_quest_confirm_accept(
    r: &mut &[u8],
) -> io::Result<QuestConfirmAccept> {
    Ok(QuestConfirmAccept {
        quest_id: read_u32_le(r)?,
        title: read_cstring(r)?,
        sender: read_u64_le(r)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_confirm_bodies_are_bare_quest_ids() {
        assert_eq!(
            push_quest_to_party(0x0123_4567),
            vec![0x67, 0x45, 0x23, 0x01]
        );
        assert_eq!(quest_confirm_accept(9), vec![9, 0, 0, 0]);
    }

    #[test]
    fn push_result_body_is_guid_then_msg() {
        let b = quest_push_result(0x0000_0000_0000_002A, QuestShareMsg::DECLINE_QUEST);
        assert_eq!(b, vec![0x2A, 0, 0, 0, 0, 0, 0, 0, 3]);
    }

    #[test]
    fn push_result_reads_member_then_msg() {
        // The server's `AppendBodyTo`: guid first, verdict byte last.
        let mut b = 0x0000_0000_0000_00AAu64.to_le_bytes().to_vec();
        b.push(8);
        let r = read_quest_push_result(&mut b.as_slice()).unwrap();
        assert_eq!(
            r,
            QuestPushResult {
                member: 0xAA,
                msg: QuestShareMsg::FINISH_QUEST,
            }
        );
    }

    /// An unmapped verdict byte must survive the parse — the display table decides what (if
    /// anything) to show, exactly as every other unmapped wire code in this crate is handled.
    #[test]
    fn unknown_verdict_byte_parses() {
        let mut b = 1u64.to_le_bytes().to_vec();
        b.push(0x7F);
        let r = read_quest_push_result(&mut b.as_slice()).unwrap();
        assert_eq!(r.msg, QuestShareMsg(0x7F));
    }

    #[test]
    fn confirm_accept_reads_id_title_sender() {
        let mut b = 1234u32.to_le_bytes().to_vec();
        b.extend_from_slice(b"Escort Duty\0");
        b.extend_from_slice(&0xF130_0000_0000_0001u64.to_le_bytes());
        let c = read_quest_confirm_accept(&mut b.as_slice()).unwrap();
        assert_eq!(c.quest_id, 1234);
        assert_eq!(c.title, "Escort Duty");
        assert_eq!(c.sender, 0xF130_0000_0000_0001);
    }
}
