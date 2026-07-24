//! Vendor messages — `CMSG_LIST_INVENTORY`/buy/sell (opcodes 414-421, vmangos `Opcodes_1_12_1.h`,
//! VERIFIED). Bodies from vmangos `Item.{h,cpp}` + the hand-serialized `ItemHandler.cpp`. Lives
//! beside, not inside, [`super::items`] (the item-*template* query pair): vendor rows carry their
//! own wire shape and the buy/sell result vocabulary, unrelated to
//! `SMSG_ITEM_QUERY_SINGLE_RESPONSE`.

use std::io;

use crate::wire::{read_u32_le, read_u64_le, read_u8};

/// One vendor row (`SMSG_LIST_INVENTORY`, vmangos `ItemHandler.cpp:741-810`). `slot` (the wire's
/// `muid`) is the row's 1-based position in the vendor's list — **not** what buying uses; buying
/// addresses the item by [`VendorItem::entry`] (see [`buy_item`]). `current_count` of
/// `0xFFFF_FFFF` means unlimited stock; `price` is already reputation-discounted server-side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorItem {
    pub slot: u32,
    pub entry: u32,
    /// `item_template.display_id` → icon via `ItemDisplayInfo.dbc`.
    pub display_id: u32,
    pub current_count: u32,
    /// Buy price in copper.
    pub price: u32,
    pub max_durability: u32,
    /// Stack size delivered per purchase (`item_template.buy_count`).
    pub buy_count: u32,
}

/// `BuyResult` (vmangos `ItemDefines.h:120-141`) — the `u8` reason on `SMSG_BUY_FAILED`.
pub mod buy_result {
    pub const CANT_FIND_ITEM: u8 = 0;
    pub const ITEM_ALREADY_SOLD: u8 = 1;
    pub const NOT_ENOUGH_MONEY: u8 = 2;
    pub const SELLER_DONT_LIKE_YOU: u8 = 4;
    pub const DISTANCE_TOO_FAR: u8 = 5;
    pub const ITEM_SOLD_OUT: u8 = 7;
    pub const CANT_CARRY_MORE: u8 = 8;
    pub const RANK_REQUIRE: u8 = 11;
    pub const REPUTATION_REQUIRE: u8 = 12;
}

/// `SellResult` (vmangos `ItemDefines.h:120-141`) — the `u8` reason on `SMSG_SELL_ITEM`. A
/// *successful* sell sends no packet at all (the item vanishes + coinage rises via
/// `UPDATE_OBJECT`); only the error path (`SendSellError`, `Player.cpp:11723`) uses this shape.
pub mod sell_result {
    pub const CANT_FIND_ITEM: u8 = 1;
    pub const CANT_SELL_ITEM: u8 = 2;
    pub const CANT_FIND_VENDOR: u8 = 3;
    pub const YOU_DONT_OWN_THAT_ITEM: u8 = 4;
    pub const UNK: u8 = 5;
    pub const ONLY_EMPTY_BAG: u8 = 6;
}

/// Body of `CMSG_LIST_INVENTORY` (vmangos `Item.cpp:94`): one full 8-byte vendor guid. The server
/// gates on `UNIT_NPC_FLAG_VENDOR` + the player being alive (`ItemHandler.cpp:693`).
pub fn list_inventory(vendor_guid: u64) -> Vec<u8> {
    vendor_guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_BUY_ITEM` (vmangos `Item.cpp:104-110`): `u64 vendorGuid, u32 item` (the template
/// **entry**, not the vendor row's `muid`), `u8 count` (stacks to buy), `u8 unk1` (always 0).
/// Auto-places into the first free bag slot — the specific-bag-slot variant
/// (`CMSG_BUY_ITEM_IN_SLOT`, [`super::opcode::CMSG_BUY_ITEM_IN_SLOT`]) is out of scope for v1
/// (decision 0081). Handler → `BuyItemFromVendor` (`ItemHandler.cpp:688`).
pub fn buy_item(vendor_guid: u64, entry: u32, count: u8) -> Vec<u8> {
    let mut body = Vec::with_capacity(14);
    body.extend_from_slice(&vendor_guid.to_le_bytes());
    body.extend_from_slice(&entry.to_le_bytes());
    body.push(count);
    body.push(0); // unk1
    body
}

/// Body of `CMSG_SELL_ITEM` (vmangos `Item.cpp:87-92`): `u64 vendorGuid, u64 itemGuid, u8 count`
/// (0 = sell the whole stack). Handler `ItemHandler.cpp:442`.
pub fn sell_item(vendor_guid: u64, item_guid: u64, count: u8) -> Vec<u8> {
    let mut body = Vec::with_capacity(17);
    body.extend_from_slice(&vendor_guid.to_le_bytes());
    body.extend_from_slice(&item_guid.to_le_bytes());
    body.push(count);
    body
}

/// Body of `CMSG_BUYBACK_ITEM` (VERIFIED wow-re ui/scratch/buyback-data-path.md, `0x4fb950`:
/// `u64 vendorGuid, u32 slot` — the slot is the **absolute** player-array buyback slot 69–80
/// (`BUYBACK_SLOT_START` + index), sent raw with no re-base; vmangos `HandleBuybackItem` reads it
/// the same way).
pub fn buyback_item(vendor_guid: u64, slot: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&vendor_guid.to_le_bytes());
    body.extend_from_slice(&slot.to_le_bytes());
    body
}

/// Body of `CMSG_REPAIR_ITEM` (VERIFIED wow-re ui/scratch/repair-machinery.md, all 4 client send
/// sites: `u64 vendorGuid, u64 itemGuid`; `itemGuid == 0` = repair everything. vmangos
/// `HandleRepairItemOpcode` corroborates).
pub fn repair_item(vendor_guid: u64, item_guid: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(&vendor_guid.to_le_bytes());
    body.extend_from_slice(&item_guid.to_le_bytes());
    body
}

/// Read `SMSG_LIST_INVENTORY` (vmangos `ItemHandler.cpp:741-810`): `u64 vendorGuid, u8 count`
/// (≤ `MAX_VENDOR_ITEMS` 128) then `count` rows of [`VendorItem`]. The empty-vendor case sends
/// `count = 0` followed by a trailing `u8` error byte (`ItemHandler.cpp:728-733,806-809`); this
/// parser tolerates it by construction — the row loop simply doesn't run, so the byte is left
/// harmlessly unconsumed rather than misread as a row.
pub(super) fn read_list_inventory(r: &mut &[u8]) -> io::Result<(u64, Vec<VendorItem>)> {
    let vendor_guid = read_u64_le(r)?;
    let count = read_u8(r)?;
    let mut items = Vec::with_capacity(count as usize);
    for _ in 0..count {
        items.push(VendorItem {
            slot: read_u32_le(r)?,
            entry: read_u32_le(r)?,
            display_id: read_u32_le(r)?,
            current_count: read_u32_le(r)?,
            price: read_u32_le(r)?,
            max_durability: read_u32_le(r)?,
            buy_count: read_u32_le(r)?,
        });
    }
    Ok((vendor_guid, items))
}

/// Read `SMSG_BUY_ITEM` (vmangos `Item.cpp:190-196`, sent from `Player.cpp:18637`): `u64
/// vendorGuid, u32 vendorSlot` (1-based), `u32 newCount` (new stock; `0xFFFF_FFFF` unlimited),
/// `u32 purchaseCount`. Only updates the vendor stock display — the purchased item itself arrives
/// through the normal `SMSG_ITEM_PUSH_RESULT` + `UPDATE_OBJECT` path (`SendNewItem`,
/// `Player.cpp:18644`), which benilla's inventory already handles.
pub(super) fn read_buy_item(r: &mut &[u8]) -> io::Result<(u64, u32, u32, u32)> {
    Ok((
        read_u64_le(r)?,
        read_u32_le(r)?,
        read_u32_le(r)?,
        read_u32_le(r)?,
    ))
}

/// Read `SMSG_SELL_ITEM` (vmangos `Item.cpp:183-188`): `u64 vendorGuid, u64 itemGuid, u8 reason`
/// (a [`sell_result`] code). Only the error path sends this packet at all — a successful sell is
/// silent, visible only as the item's removal + coinage rising via `UPDATE_OBJECT`.
pub(super) fn read_sell_item(r: &mut &[u8]) -> io::Result<(u64, u64, u8)> {
    Ok((read_u64_le(r)?, read_u64_le(r)?, read_u8(r)?))
}

/// Read `SMSG_BUY_FAILED` (vmangos `Item.h:277`): `u64 vendorGuid, u32 itemEntry, u8 reason`
/// (a [`buy_result`] code).
pub(super) fn read_buy_failed(r: &mut &[u8]) -> io::Result<(u64, u32, u8)> {
    Ok((read_u64_le(r)?, read_u32_le(r)?, read_u8(r)?))
}
