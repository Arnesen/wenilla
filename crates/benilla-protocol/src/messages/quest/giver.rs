//! `SMSG_QUESTGIVER_*` / `CMSG_QUESTGIVER_*` — the "accept/turn-in a quest at an NPC" family
//! (opcodes 386-402, vmangos `Opcodes_1_12_1.h`, VERIFIED). This is the *giver panel* slice
//! (decision 0088): the four questgiver windows carry their own text, so it needs none of the full
//! `SMSG_QUEST_QUERY_RESPONSE` template — that lives in the sibling [`super::log`] slice. CMSG
//! bodies from vmangos `Server/Packets/Quest.cpp:6-78`; SMSG layouts from the same file's
//! `AppendBodyTo` writers (line citations inline).
//!
//! Wire traps this parser is written against the *byte stream* to survive (never the C++ control
//! flow):
//! - **QUEST_LIST icon is a `u32`** — `QuestListEntry`'s `icon` field is `uint32` (`Quest.h:270`),
//!   not the `u8` the gossip-menu option icon uses.
//! - **OFFER_REWARD emotes are `{delay, emote}`** — reversed from DETAILS' `{emote, delay}`
//!   (`Quest.cpp:271` vs `:308-309`).
//! - **DETAILS rewards parse is uniform across the HIDDEN_REWARDS fork** — the hidden branch writes
//!   three `u32` zeros (`Quest.cpp:235-237`), which a count-prefixed reader consumes as
//!   `choiceCount=0, rewCount=0, money=0`, identical in shape to an empty rewards block. The parser
//!   can't see the quest flag, and it doesn't need to.

use std::io;

use crate::wire::{read_cstring, read_i32_le, read_u32_le, read_u64_le, read_u8};

/// `QUEST_EMOTE_COUNT` (vmangos `QuestDef.h:43`) — DETAILS always writes exactly this many emote
/// pairs; OFFER_REWARD writes a variable count (0..=4), so only DETAILS relies on the constant.
pub const QUEST_EMOTE_COUNT: u32 = 4;

/// `DIALOG_STATUS_*` (vmangos `QuestDef.h:120-129`) — the questgiver dialog status a
/// `SMSG_QUESTGIVER_STATUS` carries per NPC guid, and the per-quest `icon` in a `QUEST_LIST` /
/// gossip quest entry. Drives the `!`/`?` over the NPC head (a world-marker concern, out of the
/// panel slice — parsed + stored per guid now, rendered later) and the greeting panel's
/// active-vs-available split.
pub mod dialog_status {
    pub const NONE: u32 = 0;
    pub const UNAVAILABLE: u32 = 1;
    pub const CHAT: u32 = 2;
    pub const INCOMPLETE: u32 = 3;
    pub const REWARD_REP: u32 = 4;
    pub const AVAILABLE: u32 = 5;
    pub const REWARD_OLD: u32 = 6;
    pub const REWARD2: u32 = 7;
}

/// One reward/choice item on a questgiver panel (`{itemId u32, count u32, displayId u32}` triple,
/// as written in DETAILS/OFFER_REWARD — vmangos `Quest.cpp:244-249` etc.). `display_id` →
/// `ItemDisplayInfo.dbc` icon, exactly like a vendor row; a `0` id means the server had no template
/// (writes a zero triple, `Quest.cpp:248-249`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestRewardItem {
    pub item_id: u32,
    pub count: u32,
    pub display_id: u32,
}

/// One required item on the progress panel (`SMSG_QUESTGIVER_REQUEST_ITEMS`), same triple shape as
/// [`QuestRewardItem`] (vmangos `Quest.cpp:376-381`).
pub type QuestRequiredItem = QuestRewardItem;

/// One entry in a `SMSG_QUESTGIVER_QUEST_LIST` (the greeting panel's quest rows). `icon` is a
/// [`dialog_status`] value (**`u32`** — `Quest.h:270`, the list-entry trap), `level` the quest's
/// display level, `title` the row label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestListEntry {
    pub quest_id: u32,
    pub icon: u32,
    pub level: u32,
    pub title: String,
}

/// `SMSG_QUESTGIVER_QUEST_LIST` (vmangos `Quest.cpp:159-203`): the multi-quest greeting panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestGiverList {
    pub npc: u64,
    pub greeting: String,
    pub emote_delay: u32,
    pub emote: u32,
    pub quests: Vec<QuestListEntry>,
}

/// `SMSG_QUESTGIVER_QUEST_DETAILS` (vmangos `Quest.cpp:205-273`): the accept panel — full quest
/// text + the rewards on offer. `money` is the reward money (`GetRewOrReqMoney`, `i32`; negative on
/// a money-*sink* quest, which DETAILS shows as a required cost — rare, kept signed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestDetails {
    pub npc: u64,
    pub quest_id: u32,
    pub title: String,
    pub details: String,
    pub objectives: String,
    pub auto_finish: u32,
    pub choices: Vec<QuestRewardItem>,
    pub rewards: Vec<QuestRewardItem>,
    pub money: i32,
    pub reward_spell: u32,
}

/// `SMSG_QUESTGIVER_OFFER_REWARD` (vmangos `Quest.cpp:275-339`): the reward panel — the turn-in
/// text + the rewards to grant. Note the emote pairs are `{delay, emote}` (reversed vs DETAILS) and
/// are consumed for alignment only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestOfferReward {
    pub npc: u64,
    pub quest_id: u32,
    pub title: String,
    pub offer_text: String,
    pub auto_finish: u32,
    pub choices: Vec<QuestRewardItem>,
    pub rewards: Vec<QuestRewardItem>,
    pub money: i32,
    pub quest_flags: u32,
    pub reward_spell: u32,
}

/// `SMSG_QUESTGIVER_REQUEST_ITEMS` (vmangos `Quest.cpp:341-391`): the progress panel — the "bring
/// me these" text + required items/money, and the four completion flags the client ANDs to enable
/// the Continue button. `is_complete` is derived from the second flag word (`0x03` complete /
/// `0x00` not — `Quest.cpp:388`), the only place the completability reaches the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestRequestItems {
    pub npc: u64,
    pub quest_id: u32,
    pub title: String,
    pub request_text: String,
    pub emote: u32,
    pub close_on_cancel: u32,
    pub required_money: u32,
    pub required_items: Vec<QuestRequiredItem>,
    pub is_complete: bool,
}

/// `SMSG_QUESTGIVER_QUEST_COMPLETE` (vmangos `Quest.cpp:96-108`): the turn-in result — the XP +
/// money granted and the fixed items handed over. The reward *item* rows here carry only
/// `{itemId, count}` (no display id — the client already knows them from the offer panel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestComplete {
    pub quest_id: u32,
    pub xp: u32,
    pub money: u32,
    pub items: Vec<(u32, u32)>,
}

// ── CMSG encoders (all trivial, readers at vmangos `Quest.cpp:6-50`) ──────────────────────────────

/// Body of `CMSG_QUESTGIVER_STATUS_QUERY` (`Quest.cpp:11-14`): one `u64` NPC guid. Answered by
/// `SMSG_QUESTGIVER_STATUS`.
pub fn questgiver_status_query(npc: u64) -> Vec<u8> {
    npc.to_le_bytes().to_vec()
}

/// Body of `CMSG_QUESTGIVER_HELLO` (`Quest.cpp:16-19`): one `u64` NPC guid. The same
/// `SendPreparedGossip` path as `CMSG_GOSSIP_HELLO` (vmangos `QuestHandler.cpp:103`), so benilla's
/// interact route can keep sending gossip-hello; this opener exists for completeness/parity.
pub fn questgiver_hello(npc: u64) -> Vec<u8> {
    npc.to_le_bytes().to_vec()
}

/// Body of `CMSG_QUESTGIVER_QUERY_QUEST` (`Quest.cpp:27-31`): `u64 guid, u32 quest`. Answered by
/// `SMSG_QUESTGIVER_QUEST_DETAILS` (vmangos `QuestHandler.cpp:224`, always — this is "look at an
/// available quest"). The click a greeting/gossip quest row makes.
pub fn questgiver_query_quest(npc: u64, quest: u32) -> Vec<u8> {
    guid_quest(npc, quest)
}

/// Body of `CMSG_QUESTGIVER_ACCEPT_QUEST` (`Quest.cpp:21-25`): `u64 guid, u32 quest`. Adds the quest
/// to the log, then the server closes the gossip window (`SMSG_GOSSIP_COMPLETE`).
pub fn questgiver_accept_quest(npc: u64, quest: u32) -> Vec<u8> {
    guid_quest(npc, quest)
}

/// Body of `CMSG_QUESTGIVER_COMPLETE_QUEST` (`Quest.cpp:46-50`): `u64 guid, u32 quest`. Asks the
/// server to show the turn-in **progress** panel (`SMSG_QUESTGIVER_REQUEST_ITEMS`, vmangos
/// `QuestHandler.cpp:383-397`) — the alternate entry from a quest-log completable quest.
pub fn questgiver_complete_quest(npc: u64, quest: u32) -> Vec<u8> {
    guid_quest(npc, quest)
}

/// Body of `CMSG_QUESTGIVER_REQUEST_REWARD` (`Quest.cpp:40-44`): `u64 guid, u32 quest`. The progress
/// panel's Continue button — advances to the **reward** panel (`SMSG_QUESTGIVER_OFFER_REWARD`,
/// vmangos `QuestHandler.cpp:286-312`, which first `CompleteQuest`s if it can).
pub fn questgiver_request_reward(npc: u64, quest: u32) -> Vec<u8> {
    guid_quest(npc, quest)
}

/// Body of `CMSG_QUESTGIVER_CHOOSE_REWARD` (`Quest.cpp:33-38`): `u64 guid, u32 quest, u32 reward`.
/// `reward` is the **index** into the quest's choice items (0-based; ignored when the quest has no
/// choice rewards — pass 0). The reward panel's Complete button — grants the reward and answers
/// `SMSG_QUESTGIVER_QUEST_COMPLETE` (vmangos `QuestHandler.cpp:239-284`).
pub fn questgiver_choose_reward(npc: u64, quest: u32, reward: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(&npc.to_le_bytes());
    body.extend_from_slice(&quest.to_le_bytes());
    body.extend_from_slice(&reward.to_le_bytes());
    body
}

/// The shared `{u64 guid, u32 quest}` body of the five simple questgiver CMSGs.
fn guid_quest(npc: u64, quest: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&npc.to_le_bytes());
    body.extend_from_slice(&quest.to_le_bytes());
    body
}

// ── SMSG parsers ──────────────────────────────────────────────────────────────────────────────────

/// Read a `{itemId u32, count u32, displayId u32}` reward/choice/required triple (the shape shared
/// by DETAILS/OFFER_REWARD/REQUEST_ITEMS).
fn read_item_triple(r: &mut &[u8]) -> io::Result<QuestRewardItem> {
    Ok(QuestRewardItem {
        item_id: read_u32_le(r)?,
        count: read_u32_le(r)?,
        display_id: read_u32_le(r)?,
    })
}

/// Read a count-prefixed run of item triples (`u32 count` then that many triples).
fn read_item_block(r: &mut &[u8]) -> io::Result<Vec<QuestRewardItem>> {
    let count = read_u32_le(r)?;
    let mut items = Vec::with_capacity(count as usize);
    for _ in 0..count {
        items.push(read_item_triple(r)?);
    }
    Ok(items)
}

/// Read `SMSG_QUESTGIVER_STATUS` (vmangos `Quest.cpp:153-157`): `u64 npcGuid, u32 status`.
pub(in crate::messages) fn read_questgiver_status(r: &mut &[u8]) -> io::Result<(u64, u32)> {
    Ok((read_u64_le(r)?, read_u32_le(r)?))
}

/// Read `SMSG_QUESTGIVER_QUEST_LIST` (vmangos `Quest.cpp:159-203`): `u64 npcGuid, cstr greeting,
/// u32 emoteDelay, u32 emote, u8 count`, then per quest `{u32 questId, u32 icon, u32 level, cstr
/// title}`. Note the entry `icon` is a **`u32`** (the list-entry trap), unlike the gossip option's
/// `u8` icon.
pub(in crate::messages) fn read_questgiver_quest_list(r: &mut &[u8]) -> io::Result<QuestGiverList> {
    let npc = read_u64_le(r)?;
    let greeting = read_cstring(r)?;
    let emote_delay = read_u32_le(r)?;
    let emote = read_u32_le(r)?;
    let count = read_u8(r)?;
    let mut quests = Vec::with_capacity(count as usize);
    for _ in 0..count {
        quests.push(QuestListEntry {
            quest_id: read_u32_le(r)?,
            icon: read_u32_le(r)?,
            level: read_u32_le(r)?,
            title: read_cstring(r)?,
        });
    }
    Ok(QuestGiverList {
        npc,
        greeting,
        emote_delay,
        emote,
        quests,
    })
}

/// Read `SMSG_QUESTGIVER_QUEST_DETAILS` (vmangos `Quest.cpp:205-273`): `u64 npcGuid, u32 questId,
/// cstr title, cstr details, cstr objectives, u32 autoFinish`, then the count-prefixed choice block,
/// the count-prefixed reward block, `i32 money`, `u32 rewSpell`, `u32 emoteCount (=4)` and that many
/// `{u32 emote, u32 delay}` pairs (consumed for alignment). The HIDDEN_REWARDS fork writes three
/// `u32` zeros in place of the two blocks + money, which this count-prefixed reader consumes
/// identically to an empty rewards block (the uniform-parse trap).
pub(in crate::messages) fn read_questgiver_quest_details(
    r: &mut &[u8],
) -> io::Result<QuestDetails> {
    let npc = read_u64_le(r)?;
    let quest_id = read_u32_le(r)?;
    let title = read_cstring(r)?;
    let details = read_cstring(r)?;
    let objectives = read_cstring(r)?;
    let auto_finish = read_u32_le(r)?;
    let choices = read_item_block(r)?;
    let rewards = read_item_block(r)?;
    let money = read_i32_le(r)?;
    let reward_spell = read_u32_le(r)?;
    let emote_count = read_u32_le(r)?;
    for _ in 0..emote_count {
        let _emote = read_u32_le(r)?;
        let _delay = read_u32_le(r)?;
    }
    Ok(QuestDetails {
        npc,
        quest_id,
        title,
        details,
        objectives,
        auto_finish,
        choices,
        rewards,
        money,
        reward_spell,
    })
}

/// Read `SMSG_QUESTGIVER_OFFER_REWARD` (vmangos `Quest.cpp:275-339`): `u64 npcGuid, u32 questId,
/// cstr title, cstr offerText, u32 autoFinish, u32 emoteCount` then that many `{u32 delay, u32
/// emote}` pairs (**delay first** — reversed vs DETAILS), the choice block, the reward block, `i32
/// money, u32 questFlags, u32 rewSpell` (the spell is guarded by build > 1.4.2 server-side, which
/// 5875 satisfies — always present).
pub(in crate::messages) fn read_questgiver_offer_reward(
    r: &mut &[u8],
) -> io::Result<QuestOfferReward> {
    let npc = read_u64_le(r)?;
    let quest_id = read_u32_le(r)?;
    let title = read_cstring(r)?;
    let offer_text = read_cstring(r)?;
    let auto_finish = read_u32_le(r)?;
    let emote_count = read_u32_le(r)?;
    for _ in 0..emote_count {
        let _delay = read_u32_le(r)?; // delay FIRST here (reversed vs DETAILS)
        let _emote = read_u32_le(r)?;
    }
    let choices = read_item_block(r)?;
    let rewards = read_item_block(r)?;
    let money = read_i32_le(r)?;
    let quest_flags = read_u32_le(r)?;
    let reward_spell = read_u32_le(r)?;
    Ok(QuestOfferReward {
        npc,
        quest_id,
        title,
        offer_text,
        auto_finish,
        choices,
        rewards,
        money,
        quest_flags,
        reward_spell,
    })
}

/// Read `SMSG_QUESTGIVER_REQUEST_ITEMS` (vmangos `Quest.cpp:341-391`): `u64 npcGuid, u32 questId,
/// cstr title, cstr requestText, u32 emoteDelay(0), u32 emote, u32 closeOnCancel, u32 requiredMoney,
/// u32 reqCount` then that many `{u32 itemId, u32 count, u32 displayId}` triples, then four `u32`
/// completion flags (`0x02, isComplete?0x03:0x00, 0x04, 0x08`). `is_complete` comes from the second
/// flag word — the only completability signal on this wire.
pub(in crate::messages) fn read_questgiver_request_items(
    r: &mut &[u8],
) -> io::Result<QuestRequestItems> {
    let npc = read_u64_le(r)?;
    let quest_id = read_u32_le(r)?;
    let title = read_cstring(r)?;
    let request_text = read_cstring(r)?;
    let _emote_delay = read_u32_le(r)?; // always 0
    let emote = read_u32_le(r)?;
    let close_on_cancel = read_u32_le(r)?;
    let required_money = read_u32_le(r)?;
    let required_items = read_item_block(r)?;
    let _flag0 = read_u32_le(r)?; // 0x02
    let flag_complete = read_u32_le(r)?; // 0x03 complete / 0x00 not
    let _flag2 = read_u32_le(r)?; // 0x04
    let _flag3 = read_u32_le(r)?; // 0x08
    Ok(QuestRequestItems {
        npc,
        quest_id,
        title,
        request_text,
        emote,
        close_on_cancel,
        required_money,
        required_items,
        is_complete: flag_complete != 0,
    })
}

/// Read `SMSG_QUESTGIVER_QUEST_COMPLETE` (vmangos `Quest.cpp:96-108`): `u32 questId, u32 unknown,
/// u32 xp, u32 money, u32 itemCount` then that many `{u32 itemId, u32 count}` pairs (no display id
/// on this wire).
pub(in crate::messages) fn read_questgiver_quest_complete(
    r: &mut &[u8],
) -> io::Result<QuestComplete> {
    let quest_id = read_u32_le(r)?;
    let _unknown = read_u32_le(r)?;
    let xp = read_u32_le(r)?;
    let money = read_u32_le(r)?;
    let count = read_u32_le(r)?;
    let mut items = Vec::with_capacity(count as usize);
    for _ in 0..count {
        items.push((read_u32_le(r)?, read_u32_le(r)?));
    }
    Ok(QuestComplete {
        quest_id,
        xp,
        money,
        items,
    })
}

/// Read `SMSG_QUESTGIVER_QUEST_INVALID` (vmangos `Quest.cpp:126`): one `u32` message code.
pub(in crate::messages) fn read_questgiver_quest_invalid(r: &mut &[u8]) -> io::Result<u32> {
    read_u32_le(r)
}

/// Read `SMSG_QUESTGIVER_QUEST_FAILED` (vmangos `Quest.cpp:110`): `u32 questId, u32 reason`.
pub(in crate::messages) fn read_questgiver_quest_failed(r: &mut &[u8]) -> io::Result<(u32, u32)> {
    Ok((read_u32_le(r)?, read_u32_le(r)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Small byte-buffer builders for the hand-built parse fixtures ──────────────────────────────
    fn push_u32(b: &mut Vec<u8>, v: u32) {
        b.extend_from_slice(&v.to_le_bytes());
    }
    fn push_i32(b: &mut Vec<u8>, v: i32) {
        b.extend_from_slice(&v.to_le_bytes());
    }
    fn push_u64(b: &mut Vec<u8>, v: u64) {
        b.extend_from_slice(&v.to_le_bytes());
    }
    fn push_cstr(b: &mut Vec<u8>, s: &str) {
        b.extend_from_slice(s.as_bytes());
        b.push(0);
    }
    fn push_triple(b: &mut Vec<u8>, id: u32, count: u32, disp: u32) {
        push_u32(b, id);
        push_u32(b, count);
        push_u32(b, disp);
    }

    // ── CMSG encode goldens ──────────────────────────────────────────────────────────────────────

    #[test]
    fn cmsg_guid_only_bodies() {
        // STATUS_QUERY and HELLO are both a lone u64 guid, little-endian.
        assert_eq!(
            questgiver_status_query(0x0102_0304_0506_0708),
            vec![0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
        );
        assert_eq!(questgiver_hello(0xAABB), vec![0xBB, 0xAA, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn cmsg_guid_quest_bodies() {
        // QUERY/ACCEPT/COMPLETE/REQUEST_REWARD share the {u64 guid, u32 quest} body.
        let mut want = Vec::new();
        push_u64(&mut want, 0x42);
        push_u32(&mut want, 3);
        assert_eq!(questgiver_query_quest(0x42, 3), want);
        assert_eq!(questgiver_accept_quest(0x42, 3), want);
        assert_eq!(questgiver_complete_quest(0x42, 3), want);
        assert_eq!(questgiver_request_reward(0x42, 3), want);
    }

    #[test]
    fn cmsg_choose_reward_body() {
        // {u64 guid, u32 quest, u32 reward-index}.
        let mut want = Vec::new();
        push_u64(&mut want, 0x99);
        push_u32(&mut want, 40);
        push_u32(&mut want, 2);
        assert_eq!(questgiver_choose_reward(0x99, 40, 2), want);
    }

    // ── SMSG parse fixtures ──────────────────────────────────────────────────────────────────────

    #[test]
    fn status_parses() {
        let mut b = Vec::new();
        push_u64(&mut b, 0xF130_0000_0000_0042);
        push_u32(&mut b, dialog_status::AVAILABLE);
        let (npc, status) = read_questgiver_status(&mut b.as_slice()).unwrap();
        assert_eq!(npc, 0xF130_0000_0000_0042);
        assert_eq!(status, 5);
    }

    #[test]
    fn quest_list_reads_u32_icon() {
        // The list-entry `icon` is a u32 — exercise it by putting a value that would misalign the
        // next field if read as a u8 (0x0000_0005). Two quests.
        let mut b = Vec::new();
        push_u64(&mut b, 0xABCD);
        push_cstr(&mut b, "Greetings, hero.");
        push_u32(&mut b, 0); // emoteDelay
        push_u32(&mut b, 1); // emote
        b.push(2); // count (u8)
        push_u32(&mut b, 100); // questId
        push_u32(&mut b, dialog_status::AVAILABLE); // icon (u32!)
        push_u32(&mut b, 3); // level
        push_cstr(&mut b, "A Threat Within");
        push_u32(&mut b, 200);
        push_u32(&mut b, dialog_status::REWARD_REP);
        push_u32(&mut b, 5);
        push_cstr(&mut b, "Report to Goldshire");
        let list = read_questgiver_quest_list(&mut b.as_slice()).unwrap();
        assert_eq!(list.npc, 0xABCD);
        assert_eq!(list.greeting, "Greetings, hero.");
        assert_eq!(list.quests.len(), 2);
        assert_eq!(list.quests[0].icon, 5);
        assert_eq!(list.quests[0].level, 3);
        assert_eq!(list.quests[0].title, "A Threat Within");
        assert_eq!(list.quests[1].quest_id, 200);
        assert_eq!(list.quests[1].icon, 4);
        assert_eq!(list.quests[1].title, "Report to Goldshire");
    }

    #[test]
    fn details_rewards_block_is_uniform() {
        // A quest with one choice + one fixed reward + money. The count-prefixed reader is the same
        // shape the HIDDEN_REWARDS fork's three zeros parse as (choice 0, reward 0, money 0).
        let mut b = Vec::new();
        push_u64(&mut b, 0x42);
        push_u32(&mut b, 100); // questId
        push_cstr(&mut b, "A Threat Within");
        push_cstr(&mut b, "Kill the kobolds.");
        push_cstr(&mut b, "Slay 10 kobolds.");
        push_u32(&mut b, 1); // autoFinish
        push_u32(&mut b, 1); // choiceCount
        push_triple(&mut b, 2000, 1, 555); // choice item
        push_u32(&mut b, 1); // rewCount
        push_triple(&mut b, 3000, 2, 777); // fixed reward
        push_i32(&mut b, 1234); // money
        push_u32(&mut b, 0); // rewSpell
        push_u32(&mut b, QUEST_EMOTE_COUNT); // emoteCount (always 4)
        for i in 0..QUEST_EMOTE_COUNT {
            push_u32(&mut b, 10 + i); // emote
            push_u32(&mut b, 100 + i); // delay
        }
        let d = read_questgiver_quest_details(&mut b.as_slice()).unwrap();
        assert_eq!(d.quest_id, 100);
        assert_eq!(d.title, "A Threat Within");
        assert_eq!(d.details, "Kill the kobolds.");
        assert_eq!(d.objectives, "Slay 10 kobolds.");
        assert_eq!(
            d.choices,
            vec![QuestRewardItem {
                item_id: 2000,
                count: 1,
                display_id: 555
            }]
        );
        assert_eq!(
            d.rewards,
            vec![QuestRewardItem {
                item_id: 3000,
                count: 2,
                display_id: 777
            }]
        );
        assert_eq!(d.money, 1234);
    }

    #[test]
    fn details_hidden_rewards_fork_parses_as_empty() {
        // The HIDDEN_REWARDS branch writes exactly three u32 zeros where the blocks + money go.
        let mut b = Vec::new();
        push_u64(&mut b, 0x42);
        push_u32(&mut b, 100);
        push_cstr(&mut b, "T");
        push_cstr(&mut b, "D");
        push_cstr(&mut b, "O");
        push_u32(&mut b, 0); // autoFinish
        push_u32(&mut b, 0); // (hidden) choiceCount zero
        push_u32(&mut b, 0); // (hidden) rewCount zero
        push_u32(&mut b, 0); // (hidden) money zero
        push_u32(&mut b, 42); // rewSpell
        push_u32(&mut b, QUEST_EMOTE_COUNT);
        for _ in 0..QUEST_EMOTE_COUNT {
            push_u32(&mut b, 0);
            push_u32(&mut b, 0);
        }
        let d = read_questgiver_quest_details(&mut b.as_slice()).unwrap();
        assert!(d.choices.is_empty());
        assert!(d.rewards.is_empty());
        assert_eq!(d.money, 0);
        assert_eq!(d.reward_spell, 42);
    }

    #[test]
    fn offer_reward_emotes_are_delay_first() {
        // OFFER_REWARD's emote pairs are {delay, emote} — the reverse of DETAILS. Encode a distinctive
        // pair and prove the rest of the packet still aligns (choices/rewards/money/flags/spell land).
        let mut b = Vec::new();
        push_u64(&mut b, 0x42);
        push_u32(&mut b, 100);
        push_cstr(&mut b, "Well done.");
        push_cstr(&mut b, "Here is your reward.");
        push_u32(&mut b, 1); // autoFinish
        push_u32(&mut b, 2); // emoteCount
        push_u32(&mut b, 500); // delay (FIRST)
        push_u32(&mut b, 1); // emote
        push_u32(&mut b, 0); // delay
        push_u32(&mut b, 2); // emote
        push_u32(&mut b, 0); // choiceCount
        push_u32(&mut b, 1); // rewCount
        push_triple(&mut b, 3000, 1, 888);
        push_i32(&mut b, 500); // money
        push_u32(&mut b, 0x200); // questFlags (HIDDEN_REWARDS bit, just a value here)
        push_u32(&mut b, 7); // rewSpell
        let o = read_questgiver_offer_reward(&mut b.as_slice()).unwrap();
        assert_eq!(o.quest_id, 100);
        assert_eq!(o.offer_text, "Here is your reward.");
        assert!(o.choices.is_empty());
        assert_eq!(
            o.rewards,
            vec![QuestRewardItem {
                item_id: 3000,
                count: 1,
                display_id: 888
            }]
        );
        assert_eq!(o.money, 500);
        assert_eq!(o.quest_flags, 0x200);
        assert_eq!(o.reward_spell, 7);
    }

    #[test]
    fn request_items_reads_completability() {
        let mut b = Vec::new();
        push_u64(&mut b, 0x42);
        push_u32(&mut b, 100);
        push_cstr(&mut b, "A Threat Within");
        push_cstr(&mut b, "Bring me the tusks.");
        push_u32(&mut b, 0); // emoteDelay
        push_u32(&mut b, 1); // emote
        push_u32(&mut b, 1); // closeOnCancel
        push_u32(&mut b, 0); // requiredMoney
        push_u32(&mut b, 1); // reqCount
        push_triple(&mut b, 2001, 8, 111); // required item
        push_u32(&mut b, 0x02);
        push_u32(&mut b, 0x03); // isComplete
        push_u32(&mut b, 0x04);
        push_u32(&mut b, 0x08);
        let ri = read_questgiver_request_items(&mut b.as_slice()).unwrap();
        assert_eq!(ri.title, "A Threat Within");
        assert_eq!(ri.request_text, "Bring me the tusks.");
        assert_eq!(
            ri.required_items,
            vec![QuestRewardItem {
                item_id: 2001,
                count: 8,
                display_id: 111
            }]
        );
        assert!(ri.is_complete);

        // The not-complete fork: second flag word 0x00.
        let mut b2 = b.clone();
        let flag_off = b2.len() - 16 + 4; // second of the four trailing u32 flags
        b2[flag_off..flag_off + 4].copy_from_slice(&0u32.to_le_bytes());
        let ri2 = read_questgiver_request_items(&mut b2.as_slice()).unwrap();
        assert!(!ri2.is_complete);
    }

    #[test]
    fn quest_complete_reads_reward_summary() {
        let mut b = Vec::new();
        push_u32(&mut b, 100); // questId
        push_u32(&mut b, 0); // unknown
        push_u32(&mut b, 450); // xp
        push_u32(&mut b, 1234); // money
        push_u32(&mut b, 2); // itemCount
        push_u32(&mut b, 3000);
        push_u32(&mut b, 1);
        push_u32(&mut b, 3001);
        push_u32(&mut b, 5);
        let c = read_questgiver_quest_complete(&mut b.as_slice()).unwrap();
        assert_eq!(c.quest_id, 100);
        assert_eq!(c.xp, 450);
        assert_eq!(c.money, 1234);
        assert_eq!(c.items, vec![(3000, 1), (3001, 5)]);
    }

    #[test]
    fn quest_invalid_and_failed_parse() {
        // QUESTGIVER_QUEST_INVALID: one u32 message code.
        let mut b = Vec::new();
        push_u32(&mut b, 7);
        assert_eq!(read_questgiver_quest_invalid(&mut b.as_slice()).unwrap(), 7);

        // QUESTGIVER_QUEST_FAILED: {questId, reason}.
        let mut b2 = Vec::new();
        push_u32(&mut b2, 100);
        push_u32(&mut b2, 4);
        assert_eq!(
            read_questgiver_quest_failed(&mut b2.as_slice()).unwrap(),
            (100, 4)
        );
    }
}
