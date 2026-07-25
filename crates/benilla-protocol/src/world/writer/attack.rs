//! The auto-attack family's `WorldWriter` sends — start and stop the melee swing, and stop the
//! ranged auto-repeat. Bodies in [`crate::messages::attack`], whose scope this mirrors (the two
//! stops are bodyless). Split out of [`super::spells`] by decision 0640, when the messages side
//! grew an `attack` family of its own.
//!
//! Melee and ranged sit together because they are two halves of one toggle: the client runs at most
//! one auto-attack, and switching weapons hands off between `CMSG_ATTACKSTOP` and
//! `CMSG_CANCEL_AUTO_REPEAT_SPELL`.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Start melee auto-attack on `guid` (`CMSG_ATTACKSWING`, a full 8-byte guid — vmangos
    /// `AttackSwing::ReadFromWorldPacket`). Echoed back as `SMSG_ATTACKSTART`.
    pub fn attack_swing(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_ATTACKSWING, &messages::attack_swing(guid))
    }

    /// Stop melee auto-attack (`CMSG_ATTACKSTOP`, empty body). Echoed as `SMSG_ATTACKSTOP`.
    pub fn attack_stop(&mut self) -> Result<()> {
        self.send(opcode::CMSG_ATTACKSTOP, &[])
    }

    /// Stop our ranged auto-repeat (`CMSG_CANCEL_AUTO_REPEAT_SPELL`, empty body) — the ack every
    /// local cancel sends (the client's one send site `0x6ea0c6`, inside the cancel `0x6ea080`).
    pub fn cancel_auto_repeat(&mut self) -> Result<()> {
        self.send(opcode::CMSG_CANCEL_AUTO_REPEAT_SPELL, &[])
    }
}
