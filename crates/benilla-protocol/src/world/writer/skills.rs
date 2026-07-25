//! The skills pane's `WorldWriter` send — one verb, mirroring [`crate::messages::skills`]: the
//! abandon. Split out of `writer/mod.rs` (decision 0636).
//!
//! There is no "learn a skill" counterpart and no skill-up send: skills are granted server-side
//! (by a trainer purchase, a quest, or a use-based tick) and arrive as `PLAYER_SKILL_INFO` field
//! updates, so abandoning is the only skill verb the client ever initiates.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Unlearn a whole skill line (`CMSG_UNLEARN_SKILL`, layout in [`messages::unlearn_skill`]) —
    /// the skills pane's abandon. No ack: the server's `SetSkill(id, 0, 0)` comes back as a
    /// `PLAYER_SKILL_INFO` field update.
    pub fn unlearn_skill(&mut self, skill_id: u32) -> Result<()> {
        self.send(
            opcode::CMSG_UNLEARN_SKILL,
            &messages::unlearn_skill(skill_id),
        )
    }
}
