//! The character-progression `WorldWriter` send — the talent spend, mirroring
//! [`crate::messages::progression`]. Split out of [`super::spells`] by decision 0640.
//!
//! The rest of that family is inbound only: XP awards and the level-up summary arrive as packets,
//! and the points they grant go back out through this one verb. The skills half of "what I have
//! learned" has its own opcode and its own file, [`super::skills`].

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Spend talent points (`CMSG_LEARN_TALENT`, layout in [`messages::learn_talent`]): the
    /// `Talent.dbc` row id + the requested rank (0-based, learn-up-to). No dedicated reply — the
    /// server validates silently; success arrives as the rank spell's learn effects plus the
    /// refreshed `PLAYER_CHARACTER_POINTS1` (decision 0304).
    pub fn learn_talent(&mut self, talent_id: u32, requested_rank: u32) -> Result<()> {
        self.send(
            opcode::CMSG_LEARN_TALENT,
            &messages::learn_talent(talent_id, requested_rank),
        )
    }
}
