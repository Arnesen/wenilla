//! The player-trade arc's `WorldWriter` sends (decision 0592 P0): initiate, the auto-`BEGIN_TRADE`
//! reply, the decline pair (busy/ignore), set/clear an item, set gold, and accept/unaccept/cancel.
//! Bodies in [`crate::messages`]'s `trade` builders (layout VERIFIED against vmangos
//! `Handlers/TradeHandler.cpp` + `Server/Packets/Trade.cpp`). Split out of `writer/mod.rs` the same
//! way `channel`/`group`/`mail` are — one clearly separable concern among the writer's domains.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Open a trade with another player (`CMSG_INITIATE_TRADE`, layout in
    /// [`messages::initiate_trade`]): the target's guid. The server answers us on any refusal
    /// (`SMSG_TRADE_STATUS`) and, on success, sends the *target* a `BEGIN_TRADE`.
    pub fn initiate_trade(&mut self, target: u64) -> Result<()> {
        self.send(
            opcode::CMSG_INITIATE_TRADE,
            &messages::initiate_trade(target),
        )
    }

    /// Acknowledge a received `BEGIN_TRADE` (`CMSG_BEGIN_TRADE`, EMPTY body) — the client's
    /// automatic reply that makes the server emit `OPEN_WINDOW` to both sides.
    pub fn begin_trade(&mut self) -> Result<()> {
        self.send(opcode::CMSG_BEGIN_TRADE, &[])
    }

    /// Decline a trade request as busy (`CMSG_BUSY_TRADE`, EMPTY body) — the server cancels it with
    /// `TRADE_STATUS_BUSY` to the initiator.
    pub fn busy_trade(&mut self) -> Result<()> {
        self.send(opcode::CMSG_BUSY_TRADE, &[])
    }

    /// Decline a trade request as ignored (`CMSG_IGNORE_TRADE`, EMPTY body) — the server cancels it
    /// with `TRADE_STATUS_IGNORE_YOU` to the initiator.
    pub fn ignore_trade(&mut self) -> Result<()> {
        self.send(opcode::CMSG_IGNORE_TRADE, &[])
    }

    /// Put a bag item into a trade slot (`CMSG_SET_TRADE_ITEM`, layout in
    /// [`messages::set_trade_item`]): the trade slot, and the item's inventory `bag`/`slot`.
    /// Clears the partner's accept and re-arms the 200 ms scam-prevention delay server-side.
    pub fn set_trade_item(&mut self, trade_slot: u8, bag: u8, slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_SET_TRADE_ITEM,
            &messages::set_trade_item(trade_slot, bag, slot),
        )
    }

    /// Remove an item from a trade slot (`CMSG_CLEAR_TRADE_ITEM`, layout in
    /// [`messages::clear_trade_item`]): the trade slot.
    pub fn clear_trade_item(&mut self, trade_slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_CLEAR_TRADE_ITEM,
            &messages::clear_trade_item(trade_slot),
        )
    }

    /// Set the gold we are offering (`CMSG_SET_TRADE_GOLD`, layout in [`messages::set_trade_gold`]):
    /// copper. Clears the partner's accept and re-arms the scam-prevention delay server-side.
    pub fn set_trade_gold(&mut self, copper: u32) -> Result<()> {
        self.send(
            opcode::CMSG_SET_TRADE_GOLD,
            &messages::set_trade_gold(copper),
        )
    }

    /// Press Trade (`CMSG_ACCEPT_TRADE`, layout in [`messages::accept_trade`]) — accepting within
    /// 200 ms of the last change is bounced by the server as `TRADE_STATUS_BACK_TO_TRADE`; when
    /// both sides have accepted and the checks pass, the server swaps and sends both `COMPLETE`.
    pub fn accept_trade(&mut self) -> Result<()> {
        self.send(opcode::CMSG_ACCEPT_TRADE, &messages::accept_trade())
    }

    /// Un-press Trade (`CMSG_UNACCEPT_TRADE`, EMPTY body) — drops our accept back to editing.
    pub fn unaccept_trade(&mut self) -> Result<()> {
        self.send(opcode::CMSG_UNACCEPT_TRADE, &[])
    }

    /// Cancel the trade (`CMSG_CANCEL_TRADE`, EMPTY body) — the Close/Cancel path; the server
    /// unwinds both sides with `TRADE_STATUS_TRADE_CANCELED`.
    pub fn cancel_trade(&mut self) -> Result<()> {
        self.send(opcode::CMSG_CANCEL_TRADE, &[])
    }
}
