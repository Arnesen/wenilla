//! Questgiver + quest-log messages, split along the two decision-scoped slices: [`giver`] — the
//! "accept/turn-in a quest at an NPC" family (opcodes 386-402, vmangos `Opcodes_1_12_1.h`,
//! VERIFIED; decision 0088) — and [`log`] — the quest-log wire: `CMSG_QUEST_QUERY`/
//! `SMSG_QUEST_QUERY_RESPONSE` (92/93), `CMSG_QUESTLOG_SWAP_QUEST`/`CMSG_QUESTLOG_REMOVE_QUEST`/
//! `SMSG_QUESTLOG_FULL` (403-405), and the `SMSG_QUESTUPDATE_*` progress pushes (406-410). CMSG
//! bodies from vmangos `Server/Packets/Quest.cpp:6-78`; SMSG layouts from the same file's
//! `AppendBodyTo` writers (line citations inline in each submodule). The third slice — `share`,
//! `CMSG_QUEST_CONFIRM_ACCEPT` / `SMSG_QUEST_CONFIRM_ACCEPT` / `CMSG_PUSHQUESTTOPARTY` /
//! `MSG_QUEST_PUSH_RESULT` (411-413 + 630) — is the party quest-share flow (decision 1733). It is
//! its own file and not `giver`'s, even though a shared quest lands on the giver's own DETAILS
//! panel: what is new here is the *verdict* wire between sharer and receiver, and `giver` never
//! learns that a share exists — it only ever sees a giver guid that happens to be a player.

mod giver;
mod log;
mod share;

pub use giver::{
    dialog_status, questgiver_accept_quest, questgiver_choose_reward, questgiver_complete_quest,
    questgiver_hello, questgiver_query_quest, questgiver_request_reward, questgiver_status_query,
    QuestComplete, QuestDetails, QuestGiverList, QuestListEntry, QuestOfferReward,
    QuestRequestItems, QuestRequiredItem, QuestRewardItem, QUEST_EMOTE_COUNT,
};
pub use log::{
    quest_flags, quest_query, questlog_remove_quest, questlog_swap_quest, QuestObjective,
    QuestTemplate, QUEST_OBJECTIVES_COUNT, QUEST_REWARDS_COUNT, QUEST_REWARD_CHOICES_COUNT,
};
pub use share::{
    push_quest_to_party, quest_confirm_accept, quest_push_result, QuestConfirmAccept,
    QuestPushResult, QuestShareMsg,
};

pub(super) use giver::{
    read_questgiver_offer_reward, read_questgiver_quest_complete, read_questgiver_quest_details,
    read_questgiver_quest_failed, read_questgiver_quest_invalid, read_questgiver_quest_list,
    read_questgiver_request_items, read_questgiver_status,
};
pub(super) use log::{
    read_quest_query_response, read_quest_update_add_item, read_quest_update_add_kill,
    read_quest_update_complete, read_quest_update_failed, read_quest_update_failedtimer,
};
pub(super) use share::{read_quest_confirm_accept, read_quest_push_result};
