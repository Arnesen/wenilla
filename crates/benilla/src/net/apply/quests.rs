//! Questgiver-panel + quest-log arm bodies for [`super::apply_net_updates`]'s dispatch match — the
//! largest arm family, split out on its own (decision 0088's panels + its deferred quest-log/toast
//! slice). Each `pub(super)` fn here is exactly one arm's body; the match at the call site stays the
//! dispatcher, one call per arm.

use benilla_protocol::messages::{
    QuestComplete, QuestDetails, QuestGiverList, QuestOfferReward, QuestRequestItems, QuestTemplate,
};
use bevy::prelude::*;

use crate::ui_action::UiError;
use crate::ui_chat::ChatLog;
use crate::ui_quest::{MsgSurface, QuestGiver};
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

/// The log refused a new quest — no free slot (`SMSG_QUESTLOG_FULL`). The ref's `0x195` arm is a
/// bare `DisplayError(153)` and nothing else (no panel close): `ERR_QUEST_LOG_FULL` is a kind-2
/// record, so it is the RED line, not a chat line (decision 0669 — it used to be a hardcoded
/// English chat push here).
pub(super) fn quest_log_full(quest: &mut QuestGiver) {
    debug!("net: quest log full");
    quest.push_message(MsgSurface::Error, UiError::key("ERR_QUEST_LOG_FULL"));
}

/// The giver won't offer the quest (`SMSG_QUESTGIVER_QUEST_INVALID`, ref handler `0x5dbca0`): one
/// `QuestFailedReason` code, no quest id. Its chat line comes from [`questgiver_invalid_key`]'s
/// byte-verified table — reason 13 is "You are already on that quest", the line the director's
/// bare `Quest failed (0x0d).` used to be.
///
/// Then the ref **closes the window**: both refusal handlers end in `0x501130(0,0)`, which zeroes
/// the current questgiver guid (`0xbe0810`) and signals Lua event `0x130` = `QUEST_FINISHED` —
/// our [`QuestGiver::clear`] plus the feed's own `QUEST_FINISHED` on the cleared view. Without
/// this the panel sat open on a refused accept (decision 0669).
pub(super) fn quest_giver_invalid(reason: u32, quest: &mut QuestGiver) {
    debug!("net: questgiver refused to offer the quest (reason {reason})");
    quest.push_message(
        MsgSurface::Chat,
        UiError::key(crate::ui_quest::questgiver_invalid_key(reason)),
    );
    quest.clear();
}

/// The accept failed on a quest the giver DID offer (`SMSG_QUESTGIVER_QUEST_FAILED`, ref handler
/// `0x5dc840`): `{questId, reason}`, and the line names the quest — the ref pushes the quest
/// record's title (`+0x9c`) as the format's `%s`, so we fill it from the open view (the panel that
/// is refusing IS this quest) and fall back to the template cache. A full-bag refusal shows a
/// SECOND line, the ref's bare `DisplayError(0)` = `ERR_INV_FULL` on the red surface. Closes the
/// window like [`quest_giver_invalid`].
pub(super) fn quest_giver_failed(
    quest_id: u32,
    reason: u32,
    quest: &mut QuestGiver,
    quest_log: &mut QuestLog,
    net_commands: &NetCommands,
) {
    debug!("net: quest {quest_id} accept failed (reason {reason})");
    let title = quest
        .view_title(quest_id)
        .or_else(|| {
            quest_log
                .template(quest_id, net_commands)
                .map(|t| t.title.clone())
        })
        .unwrap_or_default();
    quest.push_message(
        MsgSurface::Chat,
        UiError {
            key: crate::ui_quest::questgiver_failed_key(reason),
            fill_s: Some(title),
            fill_d: None,
        },
    );
    if matches!(reason, 4 | 50) {
        quest.push_message(MsgSurface::Error, UiError::key("ERR_INV_FULL"));
    }
    quest.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_detail(quest_id: u32) -> QuestGiver {
        let mut giver = QuestGiver::default();
        giver.open(
            0x4000_0000_0000_0BAD,
            crate::ui_quest::QuestView::Detail(QuestDetails {
                npc: 0x4000_0000_0000_0BAD,
                quest_id,
                title: "A Threat Within".into(),
                details: String::new(),
                objectives: String::new(),
                auto_finish: 0,
                choices: vec![],
                rewards: vec![],
                money: 0,
                reward_spell: 0,
            }),
        );
        giver
    }

    /// The director's repro: the quest is already in the log, so the accept comes back
    /// `QUEST_INVALID` reason 13. The line must be the ref's string key — never the bare
    /// `Quest failed (0x0d).` — and the panel must CLOSE (`0x501130(0,0)` → `QUEST_FINISHED`),
    /// which is what left it sitting open before decision 0669.
    #[test]
    fn an_already_on_refusal_speaks_and_closes_the_panel() {
        let mut giver = open_detail(373);
        quest_giver_invalid(13, &mut giver);
        let msgs = giver.take_messages();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].0, MsgSurface::Chat);
        assert_eq!(msgs[0].1, UiError::key("ERR_QUEST_ALREADY_ON"));
        assert!(!giver.is_open(), "the ref closes the window on a refusal");
    }

    /// The named half: the `%s` comes off the open panel, and a full bag adds the ref's second
    /// line (`DisplayError(0)` = `ERR_INV_FULL`) on the RED surface.
    #[test]
    fn a_full_bag_refusal_names_the_quest_and_adds_the_inventory_line() {
        let mut giver = open_detail(373);
        let mut log = QuestLog::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let commands = NetCommands(tx);
        quest_giver_failed(373, 4, &mut giver, &mut log, &commands);
        let msgs = giver.take_messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].0, MsgSurface::Chat);
        assert_eq!(
            msgs[0].1,
            UiError {
                key: "ERR_QUEST_FAILED_BAG_FULL_S",
                fill_s: Some("A Threat Within".into()),
                fill_d: None,
            }
        );
        assert_eq!(msgs[1].0, MsgSurface::Error);
        assert_eq!(msgs[1].1, UiError::key("ERR_INV_FULL"));
        assert!(!giver.is_open());
    }

    /// `SMSG_QUESTLOG_FULL` is the odd one out: the ref's arm is a bare `DisplayError(153)` — the
    /// RED line, and NO close.
    #[test]
    fn a_full_log_takes_the_red_line_and_leaves_the_panel_alone() {
        let mut giver = open_detail(373);
        quest_log_full(&mut giver);
        let msgs = giver.take_messages();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].0, MsgSurface::Error);
        assert_eq!(msgs[0].1, UiError::key("ERR_QUEST_LOG_FULL"));
        assert!(
            giver.is_open(),
            "the ref's 0x195 arm does not close the panel"
        );
    }

    /// A refusal for a quest the open panel is NOT showing leaves the `%s` empty rather than
    /// naming the wrong quest.
    #[test]
    fn a_refusal_for_another_quest_does_not_borrow_the_open_title() {
        let mut giver = open_detail(373);
        let mut log = QuestLog::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let commands = NetCommands(tx);
        quest_giver_failed(999, 17, &mut giver, &mut log, &commands);
        let msgs = giver.take_messages();
        assert_eq!(msgs[0].1.key, "ERR_QUEST_FAILED_MAX_COUNT_S");
        assert_eq!(msgs[0].1.fill_s.as_deref(), Some(""));
    }
}
