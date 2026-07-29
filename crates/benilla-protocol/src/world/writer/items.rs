//! The bag/equipment family's `WorldWriter` sends — the item-template ask, use, auto-equip, the
//! ammo fork, the three move/swap/split shapes, and destroy. Bodies in
//! [`crate::messages::items`], whose scope this mirrors. Split out of `writer/mod.rs`
//! (decision 0636).
//!
//! Three distinct swap opcodes exist because the addressable space differs, not the intent:
//! `CMSG_SWAP_INV_ITEM` only ever names two slots in the player's own grid, while `CMSG_SWAP_ITEM`
//! and `CMSG_SPLIT_ITEM` take a `(bag, slot)` pair on each side so either endpoint may be an
//! equipped bag (decision 0216 §6). Every one of them refuses the same way —
//! `SMSG_INVENTORY_CHANGE_FAILURE` — and succeeds silently, as values deltas on both slots.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Ask an item template's display head (`CMSG_ITEM_QUERY_SINGLE`: entry + item guid, 0 for
    /// template-only asks). Answered by `SMSG_ITEM_QUERY_SINGLE_RESPONSE` (an `ItemTemplate`
    /// event) — the T2 container groundwork.
    pub fn item_query(&mut self, entry: u32, guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_ITEM_QUERY_SINGLE,
            &messages::item_query(entry, guid),
        )
    }

    /// Use an item by bag position (`CMSG_USE_ITEM`, layout in [`messages::use_item`]) — eat the
    /// food, drink the potion, hearthstone home. `go_target` aims the use at a GameObject, which is
    /// how a KEY opens a locked door (decision 0769). The server answers with the effect (values
    /// deltas, a stack decrement/destroy) or `SMSG_CAST_RESULT` on refusal.
    pub fn use_item(
        &mut self,
        bag_index: u8,
        slot: u8,
        spell_slot: u8,
        go_target: Option<u64>,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_USE_ITEM,
            &messages::use_item(bag_index, slot, spell_slot, go_target),
        )
    }

    /// Equip a bag item (`CMSG_AUTOEQUIP_ITEM`, layout in [`messages::auto_equip_item`]) — the
    /// server picks the destination slot. Success arrives as inventory-slot values deltas (and the
    /// visible-item change everyone renders); refusal as `SMSG_INVENTORY_CHANGE_FAILURE`.
    pub fn auto_equip_item(&mut self, bag_index: u8, slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_AUTOEQUIP_ITEM,
            &messages::auto_equip_item(bag_index, slot),
        )
    }

    /// Load ammo into the ammo slot (`CMSG_SET_AMMO`, layout in [`messages::set_ammo`]) — the
    /// client's own auto-equip fork for ammo-class items (wow-re `cursor-dragdrop-slots.md`).
    /// Addressed by item *entry*, not a bag slot; the stack stays in the bag and `PLAYER_AMMO_ID`
    /// starts referencing it. A wrong/absent ranged weapon refuses via
    /// `SMSG_INVENTORY_CHANGE_FAILURE`. Decision 0526.
    pub fn set_ammo(&mut self, entry: u32) -> Result<()> {
        self.send(opcode::CMSG_SET_AMMO, &messages::set_ammo(entry))
    }

    /// Swap two of the player's own inventory slots (`CMSG_SWAP_INV_ITEM`, layout in
    /// [`messages::swap_inv_item`]) — the wire for a backpack-internal pick/place/swap (both slots
    /// are `INVENTORY_SLOT_ITEM_START`+i). An empty destination is a move; the server settles both
    /// slots with values deltas, or refuses via `SMSG_INVENTORY_CHANGE_FAILURE`.
    pub fn swap_inv_item(&mut self, src_slot: u8, dst_slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_SWAP_INV_ITEM,
            &messages::swap_inv_item(src_slot, dst_slot),
        )
    }

    /// The general bag↔bag move (`CMSG_SWAP_ITEM`, layout in [`messages::swap_item`]): either
    /// endpoint may be an equipped bag (unlike [`Self::swap_inv_item`], which only ever addresses
    /// the player's own grid) — the wire for a whole-space bag-window pick/place/swap (decision
    /// 0216 §6, slice 2). An empty destination is a move, same as `swap_inv_item`; refusal answers
    /// `SMSG_INVENTORY_CHANGE_FAILURE`.
    pub fn swap_item(
        &mut self,
        dst_bag: u8,
        dst_slot: u8,
        src_bag: u8,
        src_slot: u8,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_SWAP_ITEM,
            &messages::swap_item(dst_bag, dst_slot, src_bag, src_slot),
        )
    }

    /// Split a stack (`CMSG_SPLIT_ITEM`, layout in [`messages::split_item`]): carry `count` off
    /// `(src_bag, src_slot)` onto `(dst_bag, dst_slot)` — either endpoint may be an equipped bag
    /// (unlike [`Self::swap_inv_item`]). Success settles both slots via values deltas; refusal
    /// answers `SMSG_INVENTORY_CHANGE_FAILURE`.
    pub fn split_item(
        &mut self,
        src_bag: u8,
        src_slot: u8,
        dst_bag: u8,
        dst_slot: u8,
        count: u8,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_SPLIT_ITEM,
            &messages::split_item(src_bag, src_slot, dst_bag, dst_slot, count),
        )
    }

    /// Destroy a bag item (`CMSG_DESTROYITEM`, layout in [`messages::destroy_item`]): `count` 0 =
    /// the whole stack. The delete-confirm popup's `OnAccept` (decision 0216 §3) — no dedicated
    /// answer packet; the item's disappearance is the ordinary field-update stream.
    pub fn destroy_item(&mut self, bag: u8, slot: u8, count: u8) -> Result<()> {
        self.send(
            opcode::CMSG_DESTROYITEM,
            &messages::destroy_item(bag, slot, count),
        )
    }
}
