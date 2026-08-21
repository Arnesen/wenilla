//! The **`PLAYER_FLAGS` toggles** — the three empty-bodied sends by which the player asks the
//! server to flip one bit of their own `PLAYER_FLAGS`: the PvP flag (decision 0646), and the two
//! equipment-display preferences, show-helm and show-cloak (decision 1472).
//!
//! What makes these one family, and unlike [`super::pose`]'s client-volunteered body state: **the
//! server owns the bit, and the only answer is the descriptor.** Nothing here has an ack, nothing
//! here has a body — the packet *is* the verb — and each one's effect comes back as a field in the
//! next `SMSG_UPDATE_OBJECT`. So there is no body builder in [`crate::messages`] to mirror, and a
//! caller that wants a *specific* state rather than a flip must compare against the flag it is
//! already holding and send only on a difference.

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

    /// Ask the server to flip our show-helm preference (`CMSG_TOGGLE_HELM`, empty body) — the
    /// Options window's *Show Helm* row, through the VM's intent queue. The answer is
    /// `PLAYER_FLAGS`' `HIDE_HELM` bit in the next descriptor update, which is what every client
    /// in range dresses our body from (decision 1472).
    pub fn toggle_helm(&mut self) -> Result<()> {
        self.send(opcode::CMSG_TOGGLE_HELM, &[])
    }

    /// The cloak half of [`Self::toggle_helm`] (`CMSG_TOGGLE_CLOAK`, empty body).
    pub fn toggle_cloak(&mut self) -> Result<()> {
        self.send(opcode::CMSG_TOGGLE_CLOAK, &[])
    }
}
