//! The PvP-flag `WorldWriter` send — the one outbound verb the whole PvP-flag family has
//! (decision 0646).
//!
//! There is no body builder in [`crate::messages`] to mirror here: the packet is empty, which is
//! itself the meaningful choice (see [`opcode::CMSG_TOGGLE_PVP`] — a one-byte body would mean
//! "set this exact state" instead).

use anyhow::Result;

use crate::messages::opcode;

use super::WorldWriter;

impl WorldWriter {
    /// Ask the server to flip our own PvP flag (`CMSG_TOGGLE_PVP`, empty body) — `/pvp` and the
    /// unit popup's PvP row. There is no ack and no immediate local effect: flagging *on* comes
    /// back as the `UNIT_FIELD_FLAGS` PvP bit within the next descriptor update, while flagging
    /// *off* only clears the preference — the flag itself survives until vmangos' 300 s drop
    /// timer expires (`Player::UpdatePvP`). The client predicts neither.
    pub fn toggle_pvp(&mut self) -> Result<()> {
        self.send(opcode::CMSG_TOGGLE_PVP, &[])
    }
}
