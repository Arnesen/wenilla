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

/// One of the 8 greeting variants in an `SMSG_NPC_TEXT_UPDATE` record. Both gender columns are kept:
/// which one is read is decided **per NPC**, not per block — see [`select_greeting`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NpcTextBlock {
    /// The block's draw weight. Not a rank — the reference sums these and draws (see
    /// [`select_greeting`]); vmangos's own fallback record sets all eight to `0.0`.
    pub probability: f32,
    /// `text0` — shown when the NPC is male, or genderless.
    pub male: String,
    /// `text1` — shown when the NPC is female.
    pub female: String,
}

impl NpcTextBlock {
    /// This block's text in the gender column [`select_greeting`] chose for the NPC.
    fn column(&self, female: bool) -> &str {
        if female {
            &self.female
        } else {
            &self.male
        }
    }
}

/// The number of greeting blocks in an `SMSG_NPC_TEXT_UPDATE` record — always exactly 8.
pub const NPC_TEXT_BLOCKS: usize = 8;

/// Read `SMSG_NPC_TEXT_UPDATE` (vmangos `GossipDef.cpp:298-369`): `u32 textID` then always exactly 8
/// blocks of `{f32 probability, cstr text0 (male), cstr text1 (female), u32 languageId, 3x(u32
/// emoteDelay, u32 emoteId)}`. Returns `(text_id, blocks)` — **all** of them, undecided.
///
/// This layer used to pick the greeting here (highest probability, with a per-block fall back to the
/// other gender column). Both halves of that were wrong, and neither could be fixed here: the real
/// choice needs the NPC and a die roll, which the wire layer has neither of. It belongs at the
/// moment the frame opens, where the reference does it — [`select_greeting`], called from the app.
///
/// The language and emote tails are still parsed for alignment and dropped. Language is the input to
/// the reference's garble pass (`0x49b560`), which is inert at `lang == 0`; we do not garble at all,
/// so a non-zero language would read plainly here. No such record is known in 1.12 data.
pub(super) fn read_npc_text_update(r: &mut &[u8]) -> io::Result<(u32, Vec<NpcTextBlock>)> {
    let text_id = read_u32_le(r)?;
    let mut blocks = Vec::with_capacity(NPC_TEXT_BLOCKS);
    for _ in 0..NPC_TEXT_BLOCKS {
        let probability = read_f32_le(r)?;
        let male = read_cstring(r)?;
        let female = read_cstring(r)?;
        let _language_id = read_u32_le(r)?;
        for _ in 0..3 {
            let _emote_delay = read_u32_le(r)?;
            let _emote_id = read_u32_le(r)?;
        }
        blocks.push(NpcTextBlock {
            probability,
            male,
            female,
        });
    }
    Ok((text_id, blocks))
}

/// Pick the greeting line the way the reference does (`0x4e2010`, wow-re
/// `system/ui/scratch/gossip-npctext-law.md`) — a **weighted random draw**, not the highest weight:
///
/// ```text
/// sum = Σ probability  over blocks whose CHOSEN-COLUMN text is non-empty
/// thr = (2.0 - roll) * sum          ∈ (0, sum]   for roll ∈ [1.0, 2.0)
/// walk the same blocks in order, acc += probability, take the first where thr <= acc
/// ```
///
/// Three details are load-bearing, and each is the opposite of what we assumed:
///
/// - **The column is chosen once, from the NPC's own gender** (`UNIT_FIELD_BYTES_0` byte 2, tested
///   `== 1` for female — so genderless `2` reads as male), before any block is examined. It is never
///   chosen per block, never from the emptiness of a string, and never from the *player*, whom the
///   reference resolves only as an existence check and never dereferences for data.
/// - **There is no fallback to the other column.** A block whose chosen-column text is empty is
///   excluded from the draw outright; if that empties the record, the reference takes its error path
///   and shows no greeting at all, rather than reading the other gender's line.
/// - **The predicate is `<=`, not `<`** (the emitted jcc is `jne` on `test ah,0x41`, so the equal
///   case selects). That is what makes an all-`0.0` record — vmangos's fallback, and much of the
///   1.12 table — deterministically select block 0 instead of nothing.
///
/// `roll` is the reference's uniform float in `[1.0, 2.0)` (it builds one by stuffing PRNG bits into
/// a mantissa: `(rand & 0x7fffff) | 0x3f800000`); passing it in keeps this pure and lets tests pin
/// each branch. Accumulation is `f64` deliberately: the reference's accumulators live on the x87
/// stack at 53-bit precision and round to `f32` exactly once, at the end.
///
/// `None` = the record names no greeting for this NPC (the reference's `"Missing gossip text!"`).
pub fn select_greeting(blocks: &[NpcTextBlock], npc_gender: u8, roll: f32) -> Option<&str> {
    let female = npc_gender == 1;
    let drawn = || blocks.iter().filter(|b| !b.column(female).is_empty());
    let sum: f64 = drawn().map(|b| b.probability as f64).sum();
    let threshold = (2.0 - roll as f64) * sum;
    let mut acc = 0.0f64;
    for block in drawn() {
        acc += block.probability as f64;
        if threshold <= acc {
            return Some(block.column(female));
        }
    }
    None
}
