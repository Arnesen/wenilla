//! The summon answer — the client's one send in the summon family (decision 1747).
//!
//! There is no "summon me" verb and no decline verb: the flow starts with the *server's*
//! `SMSG_SUMMON_REQUEST` (a warlock's ritual, a meeting stone, a GM) and this is the Yes.
//! Declining is silence — the server auto-declines when its own two-minute window runs out — which
//! is why this file has exactly one function and its twin does not exist.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Accept a summon (`CMSG_SUMMON_RESPONSE`) — the `CONFIRM_SUMMON` dialog's Accept, carrying
    /// the guid the question arrived with.
    ///
    /// The server answers by teleporting us to the summon point it recorded when it asked. It does
    /// **not** check the guid (vmangos reads it and calls `Player::SummonIfPossible(true)` off its
    /// own `m_summon_expire`), so what makes a late accept harmless is that timer, not this field.
    pub fn summon_response(&mut self, summoner_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_SUMMON_RESPONSE,
            &messages::summon_response(summoner_guid),
        )
    }
}
