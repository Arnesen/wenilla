//! The vendor window's `WorldWriter` sends — list the stock, buy, sell, buy back, repair. Bodies in
//! [`crate::messages::vendor`], whose scope this mirrors. Split out of `writer/mod.rs`
//! (decision 0636).
//!
//! Note the two different item identities the family uses: buying names the item **template
//! entry** (not the vendor row's `muid`), while selling and repairing name the **item guid** of the
//! thing in our bags, and buyback names an absolute player-array slot 69–80. Only the two failures
//! answer with a packet (`SMSG_BUY_FAILED` / `SMSG_SELL_ITEM`'s error shape); every success is
//! ordinary `UPDATE_OBJECT` traffic.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Ask a vendor's stock (`CMSG_LIST_INVENTORY`, layout in [`messages::list_inventory`]) — the
    /// server requires `UNIT_NPC_FLAG_VENDOR` + the player alive (`ItemHandler.cpp:693`). Answered
    /// by `SMSG_LIST_INVENTORY` (a `VendorInventory` event).
    pub fn list_inventory(&mut self, vendor_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_LIST_INVENTORY,
            &messages::list_inventory(vendor_guid),
        )
    }

    /// Buy from a vendor (`CMSG_BUY_ITEM`, layout in [`messages::buy_item`]): `entry` is the item
    /// **template** id (not the vendor row's `muid`), `count` the number of stacks. Auto-places
    /// into the first free bag slot. Success updates the vendor stock (`SMSG_BUY_ITEM`) and
    /// delivers the item via the normal item-create path; refusal answers `SMSG_BUY_FAILED`.
    pub fn buy_item(&mut self, vendor_guid: u64, entry: u32, count: u8) -> Result<()> {
        self.send(
            opcode::CMSG_BUY_ITEM,
            &messages::buy_item(vendor_guid, entry, count),
        )
    }

    /// Sell an item to a vendor (`CMSG_SELL_ITEM`, layout in [`messages::sell_item`]): `count` 0 =
    /// sell the whole stack. Success is silent (the item vanishes + coinage rises via
    /// `UPDATE_OBJECT`); refusal answers `SMSG_SELL_ITEM`'s error shape (a `VendorSellFailed`
    /// event).
    pub fn sell_item(&mut self, vendor_guid: u64, item_guid: u64, count: u8) -> Result<()> {
        self.send(
            opcode::CMSG_SELL_ITEM,
            &messages::sell_item(vendor_guid, item_guid, count),
        )
    }

    /// Buy a sold item back (`CMSG_BUYBACK_ITEM`, layout in [`messages::buyback_item`]): `slot` is
    /// the absolute player-array buyback slot 69–80. Success is the item re-creating + coinage
    /// falling via `UPDATE_OBJECT`; refusal answers `SMSG_BUY_FAILED`.
    pub fn buyback_item(&mut self, vendor_guid: u64, slot: u32) -> Result<()> {
        self.send(
            opcode::CMSG_BUYBACK_ITEM,
            &messages::buyback_item(vendor_guid, slot),
        )
    }

    /// Repair at a repair-capable vendor (`CMSG_REPAIR_ITEM`, layout in
    /// [`messages::repair_item`]): `item_guid` 0 = repair everything. No dedicated answer packet —
    /// durability rises + coinage falls via `UPDATE_OBJECT`.
    pub fn repair_item(&mut self, vendor_guid: u64, item_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_REPAIR_ITEM,
            &messages::repair_item(vendor_guid, item_guid),
        )
    }
}
