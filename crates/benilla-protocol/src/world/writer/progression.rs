//! The character-progression `WorldWriter` send — the talent spend, mirroring
//! [`crate::messages::progression`]. Split out of [`super::spells`] by decision 0640.
//!
//! The rest of that family is inbound only: XP awards and the level-up summary arrive as packets,
//! and the points they grant go back out through this one verb — plus the one that gives them all
//! back, the respec answer (decision 1580). The skills half of "what I have learned" has its own
//! opcode and its own file, [`super::skills`].

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

    /// Answer a class trainer's respec question (`MSG_TALENT_WIPE_CONFIRM` outbound, layout in
    /// [`messages::talent_wipe_confirm`]) — the `CONFIRM_TALENT_WIPE` dialog's Accept, and the only
    /// packet in the flow that unlearns anything: the question that raised the dialog arrived on
    /// this same opcode and changed nothing. The server answers by resetting the talents and having
    /// the trainer cast 14867, which lands as the un-learn of every rank spell plus the refreshed
    /// `PLAYER_CHARACTER_POINTS1`; declining sends nothing at all (decision 1580).
    pub fn talent_wipe_confirm(&mut self, trainer_guid: u64) -> Result<()> {
        self.send(
            opcode::MSG_TALENT_WIPE_CONFIRM,
            &messages::talent_wipe_confirm(trainer_guid),
        )
    }
}
