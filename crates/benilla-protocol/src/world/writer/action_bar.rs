//! The action bar's `WorldWriter` send — one verb, mirroring [`crate::messages::action_bar`]. Split
//! out of [`super::spells`] by decision 0640.
//!
//! One verb is the whole family because the bar is **client-authoritative** (decisions 0216 §7 /
//! 0218 §4): the server stores the slots and hands them back at login and never edits them in
//! normal play. So every pickup, place and hop the player makes is exactly one of these — a
//! drag-swap is two sends, never atomic.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Set (or clear, `packed == 0`) one action-bar slot (`CMSG_SET_ACTION_BUTTON`, layout in
    /// [`messages::set_action_button`]) — decision 0216 §7/0218 §4: the bar is
    /// client-authoritative, so this is the ONLY wire traffic a local pickup/place/hop generates,
    /// one send per slot mutation (a drag-swap is two sends, never atomic). No dedicated answer
    /// packet — `SMSG_ACTION_BUTTONS` only ever re-arrives on a server-side edit (a GM command, a
    /// macro-menu save), never as our own edit's echo.
    pub fn set_action_button(&mut self, button: u8, packed: u32) -> Result<()> {
        self.send(
            opcode::CMSG_SET_ACTION_BUTTON,
            &messages::set_action_button(button, packed),
        )
    }
}
