//! The action bar's `WorldWriter` send — one verb, mirroring [`crate::messages::action_bar`]. Split
//! out of [`super::spells`] by decision 0640.
//!
//! One verb is the whole family because the bar is **client-authoritative** (decisions 0216 §7 /
//! 0218 §4): the server stores the slots and hands them back at login and never edits them in
//! normal play. So every pickup, place and hop the player makes is exactly one of these — a
//! drag-swap is two sends, never atomic.
//!
//! The second verb is the mirror image of the first: [`WorldWriter::set_actionbar_toggles`] posts a
//! byte the client is **not** allowed to write locally.

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

    /// Post the four extra bars' visibility byte (`CMSG_SET_ACTIONBAR_TOGGLES`, layout in
    /// [`messages::set_actionbar_toggles`]) — `PLAYER_FIELD_BYTES` byte 2, VERIFIED wow-re
    /// `system/ui/scratch/action-bar-toggles.md`.
    ///
    /// **The opposite arrangement to [`Self::set_action_button`]**, and the difference is the whole
    /// point. The bar's *contents* are client-authoritative; the bar's *visibility* is
    /// server-owned: the real client never stores this byte itself (§4.1 — the one `+0x102a`
    /// instruction image-wide is a read), so the value only becomes true when the server's
    /// `SMSG_UPDATE_OBJECT` echoes it back into the descriptor. There is no answer packet and no
    /// field-change notification (§4.2), which is why the reference UI keeps its own optimistic
    /// copy in Lua and re-reads the field exactly once, at `PLAYER_ENTERING_WORLD`.
    ///
    /// Sending while disconnected is a **silent no-op**, not an error, matching the reference: the
    /// binding gates nothing and drops the packet at `0x5ab637` (no connection) or `0x5379ab`
    /// (connection state ≠ 6), returning zero Lua values either way, so nothing upstream can tell.
    pub fn set_actionbar_toggles(&mut self, toggles: u8) -> Result<()> {
        self.send(
            opcode::CMSG_SET_ACTIONBAR_TOGGLES,
            &messages::set_actionbar_toggles(toggles),
        )
    }
}
