//! The trainer window's `WorldWriter` sends — the service-list refresh and the purchase. Bodies in
//! [`crate::messages::trainer`], whose scope this mirrors. Split out of `writer/mod.rs`
//! (decision 0636).
//!
//! [`WorldWriter::trainer_list`] is the *refresh* verb, not the open: the window first appears off
//! the gossip trainer option's own `SMSG_TRAINER_LIST`. It exists because the server does **not**
//! auto-resend the list after a purchase (VERIFIED vmangos `NPCHandler.cpp:92-95`), so repainting
//! the bought row green→gray takes a standalone re-request.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Ask (or re-ask) a trainer's service list (`CMSG_TRAINER_LIST`, layout in
    /// [`messages::trainer_list`]) — one 8-byte trainer guid. The window first *opens* off the
    /// gossip trainer option's `SMSG_TRAINER_LIST`; this is the *refresh* verb, re-requested after a
    /// purchase to repaint the bought row green→gray (vmangos `HandleTrainerListOpcode` honors a
    /// standalone re-request while the player can still interact — VERIFIED `NPCHandler.cpp:92-95`,
    /// the server does not auto-resend on a buy). Answered by `SMSG_TRAINER_LIST` (a `TrainerList`
    /// event).
    pub fn trainer_list(&mut self, trainer_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_TRAINER_LIST,
            &messages::trainer_list(trainer_guid),
        )
    }

    /// Buy (learn) a trainer service (`CMSG_TRAINER_BUY_SPELL`, layout in
    /// [`messages::trainer_buy_spell`]): the trainer guid + the service's spell id. Success answers
    /// `SMSG_TRAINER_BUY_SUCCEEDED` and delivers the spell via the learn effect's `SMSG_LEARNED_SPELL`
    /// (the green→gray repaint then needs a `CMSG_TRAINER_LIST` re-request); refusal answers
    /// `SMSG_TRAINER_BUY_FAILED` with a [`messages::train_fail`] code.
    pub fn trainer_buy_spell(&mut self, trainer_guid: u64, spell_id: u32) -> Result<()> {
        self.send(
            opcode::CMSG_TRAINER_BUY_SPELL,
            &messages::trainer_buy_spell(trainer_guid, spell_id),
        )
    }
}
