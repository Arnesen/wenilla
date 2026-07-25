//! The duel family's `WorldWriter` sends — the only two the client ever makes (decision 0633).
//!
//! There is no "start a duel" opcode: the challenge goes out as an ordinary `CMSG_CAST_SPELL` of
//! the spellbook spell whose `Effect[0]` is `SPELL_EFFECT_DUEL`, which is why nothing here builds
//! one. Both bodies carry the duel-arbiter guid, byte-verified against WoW.exe's own
//! `AcceptDuel 0x4d4830` / `CancelDuel 0x4d48b0` (bodies in [`crate::messages`]'s `duel_*`
//! builders).

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Accept a duel challenge (`CMSG_DUEL_ACCEPTED`) — the popup's Accept, and the challenger's
    /// own immediate auto-accept of the request it just triggered.
    pub fn duel_accepted(&mut self, arbiter: u64) -> Result<()> {
        self.send(
            opcode::CMSG_DUEL_ACCEPTED,
            &messages::duel_accepted(arbiter),
        )
    }

    /// Decline or abandon a duel (`CMSG_DUEL_CANCELLED`) — the popup's Decline, a cancel during
    /// the countdown, and `/forfeit` once the duel is under way. The server reads the intent from
    /// the duel's own state, not from the packet.
    pub fn duel_cancelled(&mut self, arbiter: u64) -> Result<()> {
        self.send(
            opcode::CMSG_DUEL_CANCELLED,
            &messages::duel_cancelled(arbiter),
        )
    }
}
