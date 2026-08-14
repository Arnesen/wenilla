//! The reputation pane's `WorldWriter` sends, mirroring [`crate::messages::reputation`]: the at-war
//! toggle, the inactive toggle, and the watched faction. Split out of `writer/mod.rs` (decision 0636).
//!
//! All three address a faction by its **reputation-list slot**, and none is acked — see the messages
//! module for the shapes and for why the watched slot is signed.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Declare or withdraw war on a faction (`CMSG_SET_FACTION_ATWAR`, layout in
    /// [`messages::set_faction_at_war`]) — the pane's crossed-swords checkbox. vmangos DROPS this
    /// while the player is in combat, so a flip mid-fight is silently nothing.
    pub fn set_faction_at_war(&mut self, rep_list_id: u32, at_war: bool) -> Result<()> {
        self.send(
            opcode::CMSG_SET_FACTION_ATWAR,
            &messages::set_faction_at_war(rep_list_id, at_war),
        )
    }

    /// Move a faction to (or out of) the pane's inactive bucket (`CMSG_SET_FACTION_INACTIVE`,
    /// layout in [`messages::set_faction_inactive`]).
    pub fn set_faction_inactive(&mut self, rep_list_id: u32, inactive: bool) -> Result<()> {
        self.send(
            opcode::CMSG_SET_FACTION_INACTIVE,
            &messages::set_faction_inactive(rep_list_id, inactive),
        )
    }

    /// Watch a faction on the main bar, or [`messages::WATCHED_FACTION_NONE`] to stop
    /// (`CMSG_SET_WATCHED_FACTION`, layout in [`messages::set_watched_faction`]). The answer comes
    /// back as a `PLAYER_FIELD_WATCHED_FACTION_INDEX` descriptor update, not an ack.
    pub fn set_watched_faction(&mut self, rep_list_id: i32) -> Result<()> {
        self.send(
            opcode::CMSG_SET_WATCHED_FACTION,
            &messages::set_watched_faction(rep_list_id),
        )
    }
}
