//! The loot window's `WorldWriter` sends — open, take a row, take the coin pile, close, and vote
//! in a group roll. Bodies in [`crate::messages::loot`], whose scope this mirrors. Split out of
//! `writer/mod.rs` (decision 0636).
//!
//! Two addressing quirks worth keeping together: [`WorldWriter::loot_release`] passes a guid the
//! server *ignores* (it releases whatever loot guid it has stored for us), and
//! [`WorldWriter::loot_roll`] is addressed by the `(looted_target, item_slot)` pair the roll was
//! opened with — never by the client-internal `rollID` the reference UI keys its frames on.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Open a loot window (`CMSG_LOOT`, layout in [`messages::loot`]) — a full guid naming the
    /// corpse/creature/player to loot. Answered by `SMSG_LOOT_RESPONSE` (a `LootResponse` event
    /// on success, `LootError` on refusal).
    pub fn loot(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_LOOT, &messages::loot(guid))
    }

    /// Take one loot-window row (`CMSG_AUTOSTORE_LOOT_ITEM`, layout in
    /// [`messages::autostore_loot_item`]): `loot_slot` is the wire's 0-based row index. The server
    /// auto-places the item into the first free bag slot; success arrives via the normal
    /// item-create/values path plus `SMSG_LOOT_REMOVED` clearing the row and
    /// `SMSG_ITEM_PUSH_RESULT` naming what landed.
    pub fn autostore_loot_item(&mut self, loot_slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_AUTOSTORE_LOOT_ITEM,
            &messages::autostore_loot_item(loot_slot),
        )
    }

    /// Take the loot's coin pile (`CMSG_LOOT_MONEY`, empty body). Answered by
    /// `SMSG_LOOT_MONEY_NOTIFY` (our share) then `SMSG_LOOT_CLEAR_MONEY` (the coin line clears for
    /// every looter) plus the coinage rising via `UPDATE_OBJECT`.
    pub fn loot_money(&mut self) -> Result<()> {
        self.send(opcode::CMSG_LOOT_MONEY, &messages::loot_money())
    }

    /// Close the loot window (`CMSG_LOOT_RELEASE`, layout in [`messages::loot_release`]); the
    /// server ignores `guid` and releases whatever loot guid it has stored for us instead.
    /// Answered by `SMSG_LOOT_RELEASE_RESPONSE` (a `LootReleaseResponse` event).
    pub fn loot_release(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_LOOT_RELEASE, &messages::loot_release(guid))
    }

    /// Cast a group-loot vote (`CMSG_LOOT_ROLL`, layout in [`messages::loot_roll`]) — the roll is
    /// addressed by the `(looted_target, item_slot)` pair `SMSG_LOOT_START_ROLL` opened it with,
    /// never by the client-internal `rollID`. `roll_type` is a [`messages::roll_vote`] value; the
    /// server drops anything `>= 3` without a reply. Answered by an `SMSG_LOOT_ROLL` broadcast of
    /// our vote, then the resolution (`SMSG_LOOT_ROLL_WON` / `SMSG_LOOT_ALL_PASSED`).
    pub fn loot_roll(&mut self, looted_target: u64, item_slot: u32, roll_type: u8) -> Result<()> {
        self.send(
            opcode::CMSG_LOOT_ROLL,
            &messages::loot_roll(looted_target, item_slot, roll_type),
        )
    }
}
