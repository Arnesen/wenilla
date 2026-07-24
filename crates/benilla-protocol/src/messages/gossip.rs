//! Gossip + NPC-text messages — the "right-click a friendly NPC → dialog" family (opcodes 379-384,
//! vmangos `Opcodes_1_12_1.h`, VERIFIED). Bodies from vmangos `Npc.{h,cpp}` + the hand-serialized
//! `GossipDef.cpp`. Vendor bodies live beside this in [`super::vendor`] — a different wire family
//! (`CMSG_LIST_INVENTORY`/buy/sell) that a gossip option can lead into but doesn't share a shape with.
//! Quest-giver flows (`SMSG_QUESTGIVER_*`) are a separate, out-of-scope arc; the quest-option block
//! riding inside `SMSG_GOSSIP_MESSAGE` is parsed here only to stay byte-aligned.

use std::io;

use crate::wire::{read_cstring, read_f32_le, read_u32_le, read_u64_le, read_u8};

/// One gossip menu entry (`SMSG_GOSSIP_MESSAGE`'s option list, 1.12 shape — no box-money field,
/// that's TBC+). `index` is the value the client echoes back as `gossipListId` on select; `icon` is
/// a `GOSSIP_ICON_*` (0 chat bubble, 1 vendor, 2 taxi, 3 trainer, …); `coded` marks a password-gated
/// option (petition signing etc. — v1 sends an empty code only for non-coded options, see
/// [`gossip_select_option`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GossipOption {
    pub index: u32,
    pub icon: u8,
    pub coded: bool,
    pub message: String,
}

/// One quest-giver entry riding the same packet (`SMSG_GOSSIP_MESSAGE`'s quest-option list). Parsed
/// for byte alignment only — quest-giver flows are out of scope for this arc (gossip/vendor arc
/// brief §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestOption {
    pub quest_id: u32,
    pub icon: u32,
    pub level: u32,
    pub title: String,
}

/// Body of `CMSG_GOSSIP_HELLO` (vmangos `Npc.cpp:3`): one full 8-byte NPC guid. Works on *any*
/// interactable creature, not only gossip-flagged ones — the server's `CanInteractWithNPC` passes
/// `UNIT_NPC_FLAG_NONE` for this opcode (`Player.cpp:347`).
pub fn gossip_hello(npc_guid: u64) -> Vec<u8> {
    npc_guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_GOSSIP_SELECT_OPTION` (vmangos `Npc.cpp:78-86`): guid, the `gossipListId`
/// (= the chosen [`GossipOption::index`]), then an **optional** trailing code cstring — appended
/// only for a `coded` option carrying a real code; the server reads it only when the buffer is
/// non-empty, so a non-coded select must omit it entirely rather than send an empty string.
/// Handler dispatch: `NPCHandler.cpp:370`.
pub fn gossip_select_option(npc_guid: u64, gossip_list_id: u32, code: Option<&str>) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&npc_guid.to_le_bytes());
    body.extend_from_slice(&gossip_list_id.to_le_bytes());
    if let Some(code) = code {
        body.extend_from_slice(code.as_bytes());
        body.push(0);
    }
    body
}

/// Body of `CMSG_NPC_TEXT_QUERY` (vmangos `Npc.cpp:8-12`): `u32 textID`, `u64 guid`. Sent on
/// receiving a gossip menu to fetch the greeting text for `textId` (ask-once cacheable, like the
/// item template query).
pub fn npc_text_query(text_id: u32, guid: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&text_id.to_le_bytes());
    body.extend_from_slice(&guid.to_le_bytes());
    body
}

/// Read `SMSG_GOSSIP_MESSAGE` (vmangos `GossipDef.cpp:180-225`, the 1.12 shape — build 5875 predates
/// the TBC box-money field): `u64 objectGuid, u32 textId, u32 optionCount` + options
/// (`u32 index, u8 icon, u8 coded, cstr message`), then `u32 questCount` + quest options
/// (`u32 questId, u32 icon, u32 level, cstr title`). Returns `(npc_guid, text_id, options, quests)`.
pub(super) fn read_gossip_message(
    r: &mut &[u8],
) -> io::Result<(u64, u32, Vec<GossipOption>, Vec<QuestOption>)> {
    let npc_guid = read_u64_le(r)?;
    let text_id = read_u32_le(r)?;
    let option_count = read_u32_le(r)?;
    let mut options = Vec::with_capacity(option_count as usize);
    for _ in 0..option_count {
        options.push(GossipOption {
            index: read_u32_le(r)?,
            icon: read_u8(r)?,
            coded: read_u8(r)? != 0,
            message: read_cstring(r)?,
        });
    }
    let quest_count = read_u32_le(r)?;
    let mut quests = Vec::with_capacity(quest_count as usize);
    for _ in 0..quest_count {
        quests.push(QuestOption {
            quest_id: read_u32_le(r)?,
            icon: read_u32_le(r)?,
            level: read_u32_le(r)?,
            title: read_cstring(r)?,
        });
    }
    Ok((npc_guid, text_id, options, quests))
}

/// Read `SMSG_NPC_TEXT_UPDATE` (vmangos `GossipDef.cpp:298-369`): `u32 textID` then always exactly 8
/// blocks of `{f32 probability, cstr text0 (male), cstr text1 (female), u32 languageId, 3x(u32
/// emoteDelay, u32 emoteId)}`. The client only needs one line for the greeting: the highest-
/// probability block's `text0`, falling back to `text1` when `text0` is empty (vmangos's own default
/// row, when a text is missing entirely, is literally `"Greetings $N"`). Returns
/// `(text_id, greeting)`; the emote/language tails and the losing blocks are parsed for alignment
/// and dropped.
pub(super) fn read_npc_text_update(r: &mut &[u8]) -> io::Result<(u32, String)> {
    let text_id = read_u32_le(r)?;
    let mut best: Option<(f32, String)> = None;
    for _ in 0..8 {
        let probability = read_f32_le(r)?;
        let text0 = read_cstring(r)?;
        let text1 = read_cstring(r)?;
        let _language_id = read_u32_le(r)?;
        for _ in 0..3 {
            let _emote_delay = read_u32_le(r)?;
            let _emote_id = read_u32_le(r)?;
        }
        let text = if text0.is_empty() { text1 } else { text0 };
        let is_new_best = match &best {
            Some((best_probability, _)) => probability > *best_probability,
            None => true,
        };
        if is_new_best {
            best = Some((probability, text));
        }
    }
    Ok((text_id, best.map(|(_, text)| text).unwrap_or_default()))
}
