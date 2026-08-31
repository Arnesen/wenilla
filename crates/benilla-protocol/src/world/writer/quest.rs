//! The quest family's `WorldWriter` sends — the questgiver dialog walk (status probe → detail →
//! accept → progress → reward panel → complete) and the two quest-log verbs. Bodies in
//! [`crate::messages::quest`], whose `giver`/`log` split this mirrors as one file. Split out of
//! `writer/mod.rs` (decision 0636).
//!
//! The dialog is a **client-driven state machine**: each panel the player advances to is another
//! send, and the server only ever answers the panel that was asked for. Note the two ways a quest's
//! text is fetched — [`WorldWriter::questgiver_query_quest`] needs an NPC guid (it's the click on a
//! greeting row), while [`WorldWriter::quest_query`] takes only the id and is the quest log's own
//! ask-once source.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Greet a questgiver (`CMSG_QUESTGIVER_HELLO`, layout in [`messages::questgiver_hello`]) —
    /// answered with `SMSG_QUESTGIVER_QUEST_LIST` (or the gossip menu, server's choice). Sent by
    /// the *end* of a quest session on a non-gossip NPC, which is how the reference puts the
    /// player back in the giver's list after a decline (decision 1738).
    pub fn questgiver_hello(&mut self, npc: u64) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_HELLO,
            &messages::questgiver_hello(npc),
        )
    }

    /// Ask a quest's detail panel (`CMSG_QUESTGIVER_QUERY_QUEST`, layout in
    /// [`messages::questgiver_query_quest`]) — the click a greeting/gossip quest row makes.
    /// Answered by `SMSG_QUESTGIVER_QUEST_DETAILS` (a `QuestDetail` event).
    pub fn questgiver_query_quest(&mut self, npc: u64, quest: u32) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_QUERY_QUEST,
            &messages::questgiver_query_quest(npc, quest),
        )
    }

    /// Accept a quest (`CMSG_QUESTGIVER_ACCEPT_QUEST`, layout in
    /// [`messages::questgiver_accept_quest`]) — the detail panel's Accept button. Adds it to the log;
    /// the server closes the gossip window (`SMSG_GOSSIP_COMPLETE`).
    pub fn questgiver_accept_quest(&mut self, npc: u64, quest: u32) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_ACCEPT_QUEST,
            &messages::questgiver_accept_quest(npc, quest),
        )
    }

    /// Ask a quest's turn-in progress panel (`CMSG_QUESTGIVER_COMPLETE_QUEST`, layout in
    /// [`messages::questgiver_complete_quest`]) — answered by `SMSG_QUESTGIVER_REQUEST_ITEMS`
    /// (a `QuestProgress` event), or OFFER_REWARD when there are no required items.
    pub fn questgiver_complete_quest(&mut self, npc: u64, quest: u32) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_COMPLETE_QUEST,
            &messages::questgiver_complete_quest(npc, quest),
        )
    }

    /// Advance from the progress panel to the reward panel (`CMSG_QUESTGIVER_REQUEST_REWARD`, layout
    /// in [`messages::questgiver_request_reward`]) — the progress panel's Continue button. Answered
    /// by `SMSG_QUESTGIVER_OFFER_REWARD` (a `QuestOffer` event).
    pub fn questgiver_request_reward(&mut self, npc: u64, quest: u32) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_REQUEST_REWARD,
            &messages::questgiver_request_reward(npc, quest),
        )
    }

    /// Choose a reward and finish the quest (`CMSG_QUESTGIVER_CHOOSE_REWARD`, layout in
    /// [`messages::questgiver_choose_reward`]; `reward` = choice index) — the reward panel's Complete
    /// button. Answered by `SMSG_QUESTGIVER_QUEST_COMPLETE` (a `QuestComplete` event) + the
    /// XP/money/item grants via `UPDATE_OBJECT`.
    pub fn questgiver_choose_reward(&mut self, npc: u64, quest: u32, reward: u32) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_CHOOSE_REWARD,
            &messages::questgiver_choose_reward(npc, quest, reward),
        )
    }

    /// Ask a quest's full template (`CMSG_QUEST_QUERY`, layout in [`messages::quest_query`]) — the
    /// quest-log detail pane's ask-once source, distinct from [`Self::questgiver_query_quest`]
    /// (which needs an NPC guid, not just the quest id). Answered by `SMSG_QUEST_QUERY_RESPONSE`
    /// (a `QuestTemplate` event).
    pub fn quest_query(&mut self, quest_id: u32) -> Result<()> {
        self.send(opcode::CMSG_QUEST_QUERY, &messages::quest_query(quest_id))
    }

    /// Ask an NPC's questgiver dialog status (`CMSG_QUESTGIVER_STATUS_QUERY`, layout in
    /// [`messages::questgiver_status_query`]) — the overhead `!`/`?` marker's value, answered by
    /// `SMSG_QUESTGIVER_STATUS`.
    pub fn questgiver_status_query(&mut self, npc: u64) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_STATUS_QUERY,
            &messages::questgiver_status_query(npc),
        )
    }

    /// Abandon a quest-log slot (`CMSG_QUESTLOG_REMOVE_QUEST`, layout in
    /// [`messages::questlog_remove_quest`]) — no ack SMSG; the server clears the `PLAYER_QUEST_LOG`
    /// slot fields directly.
    pub fn questlog_remove_quest(&mut self, slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTLOG_REMOVE_QUEST,
            &messages::questlog_remove_quest(slot),
        )
    }

    /// Share the quest with the party (`CMSG_PUSHQUESTTOPARTY`, layout in
    /// [`messages::push_quest_to_party`]) — the quest log's *Share Quest* button (decision 1733).
    /// No member guid: the server walks the group itself and answers with one
    /// `MSG_QUEST_PUSH_RESULT` per member (often two — the `SHARING_QUEST` opener, then the
    /// outcome).
    pub fn push_quest_to_party(&mut self, quest_id: u32) -> Result<()> {
        self.send(
            opcode::CMSG_PUSHQUESTTOPARTY,
            &messages::push_quest_to_party(quest_id),
        )
    }

    /// Answer the escort confirm with Yes (`CMSG_QUEST_CONFIRM_ACCEPT`, layout in
    /// [`messages::quest_confirm_accept`]) — a `QUEST_FLAGS_PARTY_ACCEPT` quest a party member
    /// started. Saying **No** sends nothing at all, so there is no counterpart method.
    pub fn quest_confirm_accept(&mut self, quest_id: u32) -> Result<()> {
        self.send(
            opcode::CMSG_QUEST_CONFIRM_ACCEPT,
            &messages::quest_confirm_accept(quest_id),
        )
    }

    /// Report our verdict on a quest a party member shared with us (`MSG_QUEST_PUSH_RESULT`,
    /// layout in [`messages::quest_push_result`]) — the receiver's leg of the share, relayed by the
    /// server to the sharer. `sharer` is the giver guid the shared
    /// `SMSG_QUESTGIVER_QUEST_DETAILS` arrived under (a *player* guid, which is what makes that
    /// panel a share).
    pub fn quest_push_result(&mut self, sharer: u64, msg: messages::QuestShareMsg) -> Result<()> {
        self.send(
            opcode::MSG_QUEST_PUSH_RESULT,
            &messages::quest_push_result(sharer, msg),
        )
    }
}
