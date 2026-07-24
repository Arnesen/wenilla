//! Questgiver-panel + quest-log arm bodies for [`super::apply_net_updates`]'s dispatch match — the
//! largest arm family, split out on its own (decision 0088's panels + its deferred quest-log/toast
//! slice). Each `pub(super)` fn here is exactly one arm's body; the match at the call site stays the
//! dispatcher, one call per arm.

use benilla_protocol::messages::{
    QuestComplete, QuestDetails, QuestGiverList, QuestOfferReward, QuestRequestItems, QuestTemplate,
};
use bevy::prelude::*;

use crate::ui_chat::ChatLog;
use crate::ui_quest::QuestGiver;
use crate::ui_quest_log::QuestLog;

use super::super::NetCommands;

/// A questgiver dialog status for one NPC (`SMSG_QUESTGIVER_STATUS`) — the `!`/`?` marker's
/// [`crate::messages::dialog_status`] value. Stored per guid now; the world marker is a later
/// slice (decision 0088).
pub(super) fn quest_giver_status(npc: u64, status: u32, quest: &mut QuestGiver) {
    quest.set_status(npc, status);
}

/// The greeting panel: an NPC's offered/active quest rows (`SMSG_QUESTGIVER_QUEST_LIST`).
pub(super) fn quest_greeting(list: QuestGiverList, quest: &mut QuestGiver) {
    debug!(
        "net: quest greeting on {:#x} — {} quests",
        list.npc,
        list.quests.len()
    );
    quest.open(list.npc, crate::ui_quest::QuestView::Greeting(list));
}

/// The accept panel: full quest text + rewards on offer (`SMSG_QUESTGIVER_QUEST_DETAILS`).
pub(super) fn quest_detail(d: QuestDetails, quest: &mut QuestGiver) {
    debug!("net: quest detail — quest {} on {:#x}", d.quest_id, d.npc);
    quest.open(d.npc, crate::ui_quest::QuestView::Detail(d));
}

/// The progress panel: "bring me these" text + required items/money + completability
/// (`SMSG_QUESTGIVER_REQUEST_ITEMS`).
pub(super) fn quest_progress(p: QuestRequestItems, quest: &mut QuestGiver) {
    debug!(
        "net: quest progress — quest {} on {:#x} (complete: {})",
        p.quest_id, p.npc, p.is_complete
    );
    quest.open(p.npc, crate::ui_quest::QuestView::Progress(p));
}

/// The reward panel: turn-in text + rewards to grant (`SMSG_QUESTGIVER_OFFER_REWARD`).
pub(super) fn quest_offer(o: QuestOfferReward, quest: &mut QuestGiver) {
    debug!(
        "net: quest reward offer — quest {} on {:#x}",
        o.quest_id, o.npc
    );
    quest.open(o.npc, crate::ui_quest::QuestView::Reward(o));
}

/// The turn-in result: XP/money granted + fixed items (`SMSG_QUESTGIVER_QUEST_COMPLETE`).
pub(super) fn quest_complete(c: QuestComplete, quest: &mut QuestGiver) {
    // The completion fanfare (QUESTCOMPLETED kit → iQuestComplete.wav) — the client's C++ plays
    // it on exactly this packet; the giver feed drains the flag into the UI sound path.
    quest.completed_fanfare = true;
    // The turn-in result: log the reward summary and close the window (the XP/money/item
    // grants arrive separately via UPDATE_OBJECT + ITEM_PUSH_RESULT).
    debug!(
        "net: quest {} complete — +{} XP, +{} copper, {} item(s)",
        c.quest_id,
        c.xp,
        c.money,
        c.items.len()
    );
    quest.clear();
}

/// The full quest template (`SMSG_QUEST_QUERY_RESPONSE`, answering our `CMSG_QUEST_QUERY`) — the
/// quest log's ask-once detail source, cached by `quest_id`.
pub(super) fn quest_template(t: Box<QuestTemplate>, quest_log: &mut QuestLog) {
    debug!("net: quest template {} ({})", t.quest_id, t.title);
    quest_log.insert_template(*t);
}

/// A kill/use objective ticked (`SMSG_QUESTUPDATE_ADD_KILL`) / an item-collection tick
/// (`SMSG_QUESTUPDATE_ADD_ITEM`). The visible surface — the yellow `UI_INFO_MESSAGE` toast, the
/// ref's `ERR_QUEST_ADD_*_SII` popups — no longer fires from here: it rides the quest-log
/// objective diff (`crate::ui_quest_log::feed_quest_log`), which composes the same line from the
/// same descriptor/template/bag state the SMSG announces (the wire's item tick carries only the
/// ADDED count — the "cur/req" is client-computed either way, the wire pin's finding). The old
/// chat-line stopgap here is retired: the reference shows no chat echo for objective progress
/// (INFERRED from ref screenshots; the dispatched §5 adjudicates, and this fn is the fold-back
/// seat if the real handler does more — a sound, a distinct format).
pub(super) fn quest_objective_kill(entry: u32, count: u32, required: u32) {
    debug!("net: quest kill/use objective {entry:#x} at {count}/{required}");
}

/// See [`quest_objective_kill`] — same surface, item flavor.
pub(super) fn quest_objective_item(item_id: u32, count: u32) {
    debug!("net: quest item objective {item_id} +{count}");
}

/// Every objective on the quest is complete (`SMSG_QUESTUPDATE_COMPLETE`, 0x198). The visible
/// surface — the yellow `"%s (Complete)"` toast (`ERR_QUEST_OBJECTIVE_COMPLETE_S`, verified
/// kind-1 → UI_INFO_MESSAGE, never a chat line) — rides the quest-log diff's COMPLETE-flip
/// detection (`crate::ui_quest_log::feed_quest_log`), same as the progress toasts; the slot's
/// state byte carries the durable fact.
pub(super) fn quest_objectives_complete(quest_id: u32) {
    debug!("net: quest {quest_id} objectives complete");
}

/// The quest failed (`SMSG_QUESTUPDATE_FAILED` / `_FAILEDTIMER` — `timed` picks which): the one
/// quest-update with a CHAT surface (verified kind-0, key `ERR_QUEST_FAILED_S "%s failed."`) —
/// named with the quest title when its template is cached. Follow-up: the verified handler also
/// plays `igQuestFailed`; the apply loop has no sound seam here yet.
pub(super) fn quest_failed(
    quest_id: u32,
    timed: bool,
    quest_log: &mut QuestLog,
    net_commands: &NetCommands,
    chat_log: &mut ChatLog,
) {
    debug!("net: quest {quest_id} failed (timed: {timed})");
    let line = quest_log
        .template(quest_id, net_commands)
        .map_or("Quest failed.".into(), |t| format!("{} failed.", t.title));
    chat_log.push_event(crate::ui_chat::ChatEvent::text_only(
        crate::ui_chat::ChatEventKind::System,
        line,
    ));
}

/// The log refused a new quest — no free slot (`SMSG_QUESTLOG_FULL`).
pub(super) fn quest_log_full(chat_log: &mut ChatLog) {
    chat_log.push_event(crate::ui_chat::ChatEvent::text_only(
        crate::ui_chat::ChatEventKind::System,
        "Your quest log is full.".into(),
    ));
}

/// A questgiver interaction was rejected (`SMSG_QUESTGIVER_QUEST_INVALID` msg code /
/// `SMSG_QUESTGIVER_QUEST_FAILED` reason code).
pub(super) fn quest_giver_refused(quest_id: u32, reason: u32, chat_log: &mut ChatLog) {
    // The per-reason GlobalStrings table (QuestFailedReasons / the INVALID msg codes)
    // is unpinned — a bare fallback, same convention as ui_items.rs's equip_error_text.
    debug!("net: quest giver refused (quest {quest_id}, reason {reason})");
    chat_log.push_event(crate::ui_chat::ChatEvent::text_only(
        crate::ui_chat::ChatEventKind::System,
        format!("Quest failed ({reason:#04x})."),
    ));
}
