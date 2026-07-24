//! Bank messages — the six-opcode wire (`CMSG_BANKER_ACTIVATE`/`SMSG_SHOW_BANK`/
//! `CMSG_BUY_BANK_SLOT`/`SMSG_BUY_BANK_SLOT_RESULT`/`CMSG_AUTOSTORE_BANK_ITEM`/
//! `CMSG_AUTOBANK_ITEM`, opcodes 439-442,642-643, vmangos `Opcodes_1_12_1.h`, VERIFIED). Bodies
//! from vmangos `Server/Packets/Npc.{h,cpp}` + `Server/Packets/Item.{h,cpp}`, all read this
//! session. Decision 0604 — the vault itself is already streamed through the ordinary player
//! descriptor (bank slots 39–62, bank bags 63–68); these opcodes only open the window, buy a bag
//! slot, and move items in/out.

use std::io;

use crate::wire::{read_u32_le, read_u64_le};

/// `SMSG_BUY_BANK_SLOT_RESULT`'s `u32` reason (vmangos `Player.h:91-94`,
/// `BANK_SLOT_*` enum). Sent only on failure — success is signaled purely by the
/// `PLAYER_BYTES_2` bank-bag-count byte advancing + the coinage delta, both via `UPDATE_OBJECT`
/// (decision 0604).
pub mod bank_slot_result {
    pub const FAILED_TOO_MANY: u32 = 0;
    pub const INSUFFICIENT_FUNDS: u32 = 1;
    pub const NOTBANKER: u32 = 2;
    pub const OK: u32 = 3;
}

/// Body of `CMSG_BANKER_ACTIVATE` (vmangos `Npc.h:58-66`): one full 8-byte banker guid. Sent on a
/// right-click of a *pure* banker (bits 0-7 of `UNIT_NPC_FLAGS` clear, bit 8 `0x100` set) — a
/// gossip-flagged banker instead opens the gossip menu and the bank arrives through the
/// `GOSSIP_OPTION_BANKER` option's own `SMSG_SHOW_BANK` (decision 0604).
pub fn banker_activate(banker_guid: u64) -> Vec<u8> {
    banker_guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_BUY_BANK_SLOT` (vmangos `Item.h:157-163`): one full 8-byte banker guid. The
/// server buys slot `purchased_count + 1` itself (no slot index on the wire) — the client's job
/// is only knowing the next price, read from `BankBagSlotPrices.dbc` by `benilla-formats`
/// (decision 0604).
pub fn buy_bank_slot(banker_guid: u64) -> Vec<u8> {
    banker_guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_AUTOBANK_ITEM` (vmangos `Item.h:108-116`): `u8 srcbag, u8 srcslot`. Deposit
/// only — moves a non-bank item into the first free bank slot. Handler
/// `HandleAutoBankItemOpcode` (`ItemHandler.cpp:874`).
pub fn autobank_item(bag: u8, slot: u8) -> Vec<u8> {
    vec![bag, slot]
}

/// Body of `CMSG_AUTOSTORE_BANK_ITEM` (vmangos `Item.h:118-126`): `u8 srcbag, u8 srcslot`.
/// Direction-tolerant: vmangos's `HandleAutoStoreBankItemOpcode` (`ItemHandler.cpp:971`)
/// withdraws to inventory when `srcbag`/`srcslot` name a bank position, else deposits — the same
/// opcode drives both halves of the reference client's right-click auto-move while the bank
/// window is open.
pub fn autostore_bank_item(bag: u8, slot: u8) -> Vec<u8> {
    vec![bag, slot]
}

/// Read `SMSG_SHOW_BANK` (vmangos `Npc.cpp:94`, `buffer << bankerGuid`): one full 8-byte banker
/// guid. Opens the bank window; the server also sends this **unprompted** for the gossip bank
/// option (`GOSSIP_OPTION_BANKER` → `SendShowBank`, vmangos `Player.cpp:12426`) — a handler must
/// not assume it only ever follows our own [`banker_activate`].
pub(super) fn read_show_bank(r: &mut &[u8]) -> io::Result<u64> {
    read_u64_le(r)
}

/// Read `SMSG_BUY_BANK_SLOT_RESULT` (vmangos `Item.cpp:137-140`): one `u32` [`bank_slot_result`]
/// code. vmangos sends this only on failure; a successful buy is silent, visible only as the
/// `PLAYER_BYTES_2` bank-bag-count byte advancing plus the coinage drop, both via `UPDATE_OBJECT`.
pub(super) fn read_buy_bank_slot_result(r: &mut &[u8]) -> io::Result<u32> {
    read_u32_le(r)
}
