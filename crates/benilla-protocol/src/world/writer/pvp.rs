//! The one honor-system send: asking for another player's honor stats (decision 1512). The
//! family's other message, `SMSG_PVP_CREDIT`, is inbound only.
//!
//! **Why its own module rather than [`super::selection`]** — that module is "the two sends that
//! set our selection", and this send does not: `HandleInspectOpcode` opens with
//! `SetSelectionGuid` (vmangos `MiscHandler.cpp:944`) while `HandleInspectHonorStatsOpcode` does
//! not (`MiscHandler.cpp:962-972`), so filing it there would make that module's own doc false. It
//! also follows `writer/mod.rs`'s stated, mechanical split rule: a send lives in the module named
//! after the [`crate::messages`] module its body builder lives in, and this body builder is
//! `messages::pvp::inspect_honor_stats`.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Ask for a player's honor stats (`MSG_INSPECT_HONOR_STATS`, a raw 8-byte guid — body and
    /// server gates documented on [`messages::inspect_honor_stats`]). The reply rides the **same**
    /// opcode and surfaces as [`crate::events::SessionEvent::InspectHonorStats`].
    ///
    /// There is no failure reply: each of the server's three refusals (target offline, beyond
    /// `INSPECT_DISTANCE`, a valid attack target) is a silent `return`. A caller must therefore
    /// treat the ask as fire-and-forget and keep whatever it already had on screen, exactly as the
    /// reference pane does — it re-asks on show and simply shows nothing until an answer lands.
    pub fn inspect_honor_stats(&mut self, guid: u64) -> Result<()> {
        self.send(
            opcode::MSG_INSPECT_HONOR_STATS,
            &messages::inspect_honor_stats(guid),
        )
    }
}
