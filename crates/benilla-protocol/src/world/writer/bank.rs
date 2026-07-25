//! The bank window's `WorldWriter` sends — open the bank, buy the next bag slot, deposit,
//! withdraw. Bodies in [`crate::messages::bank`], whose scope this mirrors. Split out of
//! `writer/mod.rs` (decision 0636).
//!
//! There is no dedicated withdraw opcode in the sense the names suggest: vmangos routes
//! `CMSG_AUTOBANK_ITEM` / `CMSG_AUTOSTORE_BANK_ITEM` by whether the source `(bag, slot)` *is* a
//! bank position, so it tolerates either direction and the pair is really "move it across the bank
//! boundary, server picks the destination" (decision 0604).

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Open the bank (`CMSG_BANKER_ACTIVATE`, layout in [`messages::banker_activate`]) — one
    /// 8-byte banker guid. Answered by `SMSG_SHOW_BANK` (decision 0604).
    pub fn banker_activate(&mut self, banker_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_BANKER_ACTIVATE,
            &messages::banker_activate(banker_guid),
        )
    }

    /// Buy the next bank-bag slot (`CMSG_BUY_BANK_SLOT`, layout in [`messages::buy_bank_slot`]).
    /// No packet on success (the PLAYER_BYTES_2 count + coinage deltas are the confirmation);
    /// refusal answers `SMSG_BUY_BANK_SLOT_RESULT`.
    pub fn buy_bank_slot(&mut self, banker_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_BUY_BANK_SLOT,
            &messages::buy_bank_slot(banker_guid),
        )
    }

    /// Deposit an item into the bank (`CMSG_AUTOBANK_ITEM`, layout in
    /// [`messages::autobank_item`]): the wire `(bag, slot)` of the source item.
    pub fn autobank_item(&mut self, bag: u8, slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_AUTOBANK_ITEM,
            &messages::autobank_item(bag, slot),
        )
    }

    /// Withdraw a bank item into the bags (`CMSG_AUTOSTORE_BANK_ITEM`, layout in
    /// [`messages::autostore_bank_item`]): the wire `(bag, slot)` of the bank item (vmangos
    /// routes by whether the source is a bank position, so it tolerates either direction).
    pub fn autostore_bank_item(&mut self, bag: u8, slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_AUTOSTORE_BANK_ITEM,
            &messages::autostore_bank_item(bag, slot),
        )
    }
}
