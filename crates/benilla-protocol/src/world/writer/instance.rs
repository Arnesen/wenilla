//! The instance/raid lockout family's one outbound verb (decision 1748).
//!
//! Everything else in the family is the server telling us something; the only thing the player can
//! *do* is press "Reset all instances" on their own portrait, which is this.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Reset every dungeon we are the owner of (`CMSG_RESET_INSTANCES`, empty body) — the
    /// `CONFIRM_RESET_INSTANCES` dialog's Yes.
    ///
    /// The answer is per instance, not one verdict: the server walks every bind and sends an
    /// `SMSG_INSTANCE_RESET` for each map that reset, so a mixed outcome is normal — and **no
    /// answer at all is the ordinary case**, because a character with nothing resettable gets
    /// nothing back. Refusals only exist on the group path: vmangos `Group::ResetInstances` sends
    /// `INSTANCERESET_FAIL_OFFLINE` (2187) and `INSTANCERESET_FAIL_GENERAL` (2205), while the solo
    /// path `Player::ResetInstances` sends successes only. `INSTANCERESET_FAIL_ZONING` is a code
    /// the client can render and this server never sends.
    ///
    /// Both sides also refuse a RAID here — `entry->mapType == MAP_RAID` is skipped outright —
    /// which is the server's face of the same party-dungeon rule `CanShowResetInstances()` gates
    /// the menu row on.
    pub fn reset_instances(&mut self) -> Result<()> {
        self.send(opcode::CMSG_RESET_INSTANCES, &messages::reset_instances())
    }
}
