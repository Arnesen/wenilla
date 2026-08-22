//! The auction house's `WorldWriter` sends (decision 1511 P0): the auctioneer greeting, the three
//! list pages (browse / your bids / your auctions), and the three market verbs — list an item,
//! bid or buy out, cancel. Bodies in [`crate::messages`]'s `auction` builders (layout VERIFIED
//! against vmangos `Handlers/AuctionHouseHandler.cpp` + `Server/Packets/AuctionHouse.cpp`). Split
//! out per `writer/mod.rs`'s rule — the module a body builder lives in names the module its send
//! lives in.
//!
//! Two things every caller here has to know. **The auctioneer guid rides on every send**, not
//! just the hello: the server re-validates the 5 yd interact distance on each one independently
//! (`GetCheckedAuctionHouseForAuctioneer`), so there is no session to hold and a walk-away
//! invalidates the next verb rather than the window. And **several refusals are silent** — a sell
//! with a zero bid or duration, a bid the player cannot afford, a cancel whose 5% cut they cannot
//! pay, and any list request while one is already in flight all get *no packet at all*. A UI that
//! blocks waiting for an ack hangs.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Greet an auctioneer (`MSG_AUCTION_HELLO`, layout in [`messages::auction_hello`]) — one
    /// auctioneer guid. **Two-way opcode**: the reply carries the guid back plus the
    /// `AuctionHouse.dbc` house id (an `AuctionHello` event), and it is that reply which opens the
    /// window.
    pub fn auction_hello(&mut self, auctioneer: u64) -> Result<()> {
        self.send(
            opcode::MSG_AUCTION_HELLO,
            &messages::auction_hello(auctioneer),
        )
    }

    /// Ask a Browse page (`CMSG_AUCTION_LIST_ITEMS`, layout in
    /// [`messages::auction_list_items`]) — the search's filters as the server reads them, with
    /// [`messages::auction_filter`]'s sentinels for the ones left unset. **No sort rides the
    /// wire**: sorting the returned page is entirely ours. `list_from` pages by
    /// [`messages::AUCTION_PAGE_SIZE`]. Answered by `SMSG_AUCTION_LIST_RESULT`.
    #[allow(clippy::too_many_arguments)]
    pub fn auction_list_items(
        &mut self,
        auctioneer: u64,
        list_from: u32,
        searched_name: &str,
        level_min: u8,
        level_max: u8,
        slot_id: u32,
        main_category: u32,
        sub_category: u32,
        quality: u32,
        usable: u8,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_AUCTION_LIST_ITEMS,
            &messages::auction_list_items(
                auctioneer,
                list_from,
                searched_name,
                level_min,
                level_max,
                slot_id,
                main_category,
                sub_category,
                quality,
                usable,
            ),
        )
    }

    /// Ask the Auctions tab page (`CMSG_AUCTION_LIST_OWNER_ITEMS`, layout in
    /// [`messages::auction_list_owner_items`]) — our own listings. Answered by
    /// `SMSG_AUCTION_OWNER_LIST_RESULT`.
    pub fn auction_list_owner_items(&mut self, auctioneer: u64, list_from: u32) -> Result<()> {
        self.send(
            opcode::CMSG_AUCTION_LIST_OWNER_ITEMS,
            &messages::auction_list_owner_items(auctioneer, list_from),
        )
    }

    /// Ask the Bid tab page (`CMSG_AUCTION_LIST_BIDDER_ITEMS`, layout in
    /// [`messages::auction_list_bidder_items`]). `auction_ids` is a **refresh set**, not a filter:
    /// the server prepends a record for each id named and then appends every auction we are the
    /// current bidder on. Pass `&[]` for a plain page. Answered by
    /// `SMSG_AUCTION_BIDDER_LIST_RESULT`.
    pub fn auction_list_bidder_items(
        &mut self,
        auctioneer: u64,
        list_from: u32,
        auction_ids: &[u32],
    ) -> Result<()> {
        self.send(
            opcode::CMSG_AUCTION_LIST_BIDDER_ITEMS,
            &messages::auction_list_bidder_items(auctioneer, list_from, auction_ids),
        )
    }

    /// List an item for auction (`CMSG_AUCTION_SELL_ITEM`, layout in
    /// [`messages::auction_sell_item`]) — the Create Auction pane's Create button.
    /// `etime_minutes` must be one of [`messages::auction_duration`]'s three values (120/480/1440);
    /// the deposit is charged immediately out of the player's purse. Answered by
    /// `SMSG_AUCTION_COMMAND_RESULT` (action [`messages::auction_action::STARTED`]) — or by
    /// silence, if the bid or duration is zero or the deposit is unaffordable.
    pub fn auction_sell_item(
        &mut self,
        auctioneer: u64,
        item_guid: u64,
        bid: u32,
        buyout: u32,
        etime_minutes: u32,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_AUCTION_SELL_ITEM,
            &messages::auction_sell_item(auctioneer, item_guid, bid, buyout, etime_minutes),
        )
    }

    /// Bid on — or buy out — an auction (`CMSG_AUCTION_PLACE_BID`, layout in
    /// [`messages::auction_place_bid`]). One verb for both: the server reads a `price` at or above
    /// a nonzero buyout *as* a buyout; there is no separate opcode and no flag. Answered by
    /// `SMSG_AUCTION_COMMAND_RESULT` (action [`messages::auction_action::BID_PLACED`]), or by
    /// silence if the player cannot afford the price.
    pub fn auction_place_bid(
        &mut self,
        auctioneer: u64,
        auction_id: u32,
        price: u32,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_AUCTION_PLACE_BID,
            &messages::auction_place_bid(auctioneer, auction_id, price),
        )
    }

    /// Cancel one of our own auctions (`CMSG_AUCTION_REMOVE_ITEM`, layout in
    /// [`messages::auction_remove_item`]) — the Auctions tab's Cancel button. The deposit is
    /// **forfeit**, and if the auction already carries a bid the 5% cut comes out of the seller's
    /// pocket (and the cancel is refused *silently* when they cannot pay it). Answered by
    /// `SMSG_AUCTION_COMMAND_RESULT` (action [`messages::auction_action::REMOVED`]).
    pub fn auction_remove_item(&mut self, auctioneer: u64, auction_id: u32) -> Result<()> {
        self.send(
            opcode::CMSG_AUCTION_REMOVE_ITEM,
            &messages::auction_remove_item(auctioneer, auction_id),
        )
    }
}
