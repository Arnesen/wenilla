//! The tutorial system's sends (decision 1976).

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Acknowledge one tutorial (`CMSG_TUTORIAL_FLAG`): the 0-based id — `FlagTutorial`'s and the
    /// six auto-acknowledge sites' packet, sent only when the acknowledged bank's bit was clear.
    pub fn tutorial_flag(&mut self, id: u32) -> Result<()> {
        self.send(opcode::CMSG_TUTORIAL_FLAG, &messages::tutorial_flag(id))
    }

    /// Mark every tutorial acknowledged (`CMSG_TUTORIAL_CLEAR`, empty) — `ClearTutorials()`.
    pub fn tutorial_clear(&mut self) -> Result<()> {
        self.send(opcode::CMSG_TUTORIAL_CLEAR, &[])
    }

    /// Forget every tutorial (`CMSG_TUTORIAL_RESET`, empty) — `ResetTutorials()`.
    pub fn tutorial_reset(&mut self) -> Result<()> {
        self.send(opcode::CMSG_TUTORIAL_RESET, &[])
    }
}
