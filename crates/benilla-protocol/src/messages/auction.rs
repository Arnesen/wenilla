//! The auction house arc's wire layer (decision 1511 phase P0): the auctioneer hello, the three
//! list pages (browse / your bids / your auctions), the sell/bid/cancel verbs, the one command
//! verdict, and the three notifications (vmangos `Handlers/AuctionHouseHandler.cpp`,
//! `AuctionHouse/AuctionHouseMgr.{h,cpp}`, `Server/Packets/AuctionHouse.{h,cpp}`,
//! `Opcodes_1_12_1.h`, VERIFIED — read twice independently at decision time and re-read against
//! the source while writing this module).
//!
//! **There is no `*_LIST_PENDING_SALES` opcode on 5875** — the symbol does not exist anywhere in
//! vmangos's opcode tables (0x264/0x265 sit between `SMSG_SPELLORDAMAGE_IMMUNE` and
//! `SMSG_SET_FLAT_SPELL_MODIFIER` with no gap for one), and the matching mail answer
//! `AUCTION_SALE_PENDING(6)` is dead in the tree. The same shape of recorded fact as `mail`'s
//! missing `SMSG_SHOW_MAILBOX`: a pane that exists in later clients has no wire here to build.
//!
//! Two things hold across every packet below. **Every guid is a plain 8-byte LE `u64`, never a
//! packed guid** — vmangos's `ObjectGuid` stream operators write and read `GetRawValue()` whole
//! (`ObjectGuid.cpp`), and `PackedGuid` never appears on the auction path. And **the auctioneer
//! guid rides at the head of every CMSG** because the server re-validates the 5 yd interact
//! distance on each one independently (`GetCheckedAuctionHouseForAuctioneer`): the hello is not a
//! session token.

use std::io;

use crate::wire::{read_i32_le, read_u32_le, read_u64_le};

/// `SMSG_AUCTION_COMMAND_RESULT`'s `action` field — which verb the verdict answers (VERIFIED
/// vmangos `AuctionHouseMgr.h:53-58`, `AuctionAction`).
pub mod auction_action {
    /// Answers [`super::auction_sell_item`] — a new listing was created.
    pub const STARTED: u32 = 0;
    /// Answers [`super::auction_remove_item`] — a listing was cancelled.
    pub const REMOVED: u32 = 1;
    /// Answers [`super::auction_place_bid`]. A *buyout* rides this action too: buyout is inferred
    /// server-side (`price >= buyout && buyout != 0`), never flagged on the wire.
    pub const BID_PLACED: u32 = 2;
}

/// `SMSG_AUCTION_COMMAND_RESULT`'s `error` field (VERIFIED vmangos `AuctionHouseMgr.h:40-51`,
/// `AuctionError`). **6, 8, 9, 11 and 12 have no name**: vmangos has no enumerator for them and
/// never sends them, and the real client's `ERR_AUCTION_*` table falls through to a generic
/// failure branch for anything it doesn't recognize — so an unknown code here is a generic
/// refusal to report, not a parse failure.
pub mod auction_error {
    /// Success — read [`super::auction_action`] to know *what* succeeded.
    pub const OK: u32 = 0;
    /// The only error carrying an [`super::AuctionCommandTail::Inventory`] tail: an
    /// `InventoryResult` (`EQUIP_ERR_*`) code, the same table the bag/equip refusals use.
    pub const INVENTORY: u32 = 1;
    /// `ERR_AUCTION_DATABASE_ERROR` — also vmangos's catch-all refusal (a bad `etime` lands here).
    pub const DATABASE: u32 = 2;
    pub const NOT_ENOUGH_MONEY: u32 = 3;
    pub const ITEM_NOT_FOUND: u32 = 4;
    /// Someone outbid us mid-flight; carries the [`super::AuctionCommandTail::HigherBid`] tail.
    pub const HIGHER_BID: u32 = 5;
    /// The bid did not clear the minimum increment (5% of the current bid, floored, min 1 copper).
    pub const BID_INCREMENT: u32 = 7;
    /// You cannot bid on your own auction.
    pub const BID_OWN: u32 = 10;
    pub const RESTRICTED_ACCOUNT: u32 = 13;
}

/// The "don't filter" sentinels [`auction_list_items`]'s browse filters use (VERIFIED vmangos
/// `AuctionHouseObject::BuildListAuctionItems`, which compares each filter against exactly these
/// before applying it — and takes a whole-table fast path when *all* of them are unset).
pub mod auction_filter {
    /// `slot_id` / `main_category` / `sub_category` / `quality`: `0xFFFF_FFFF` = no filter.
    pub const ANY: u32 = 0xFFFF_FFFF;
    /// `level_min` / `level_max`: `0` = no filter. The pair gates `RequiredLevel`, **not** item
    /// level, and `level_max` is only consulted when `level_min` is set.
    pub const ANY_LEVEL: u8 = 0;
    /// `usable`: `0` = no filter. Nonzero drops everything `CanUseItem` refuses (and recipes we
    /// already know).
    pub const ANY_USABILITY: u8 = 0;
}

/// The three `etime` values `CMSG_AUCTION_SELL_ITEM` may carry, in **minutes** (VERIFIED vmangos
/// `HandleAuctionSellItem`, `AuctionHouseHandler.cpp:281-296`: `etime * MINUTE` is switched
/// against 1/4/12 × `MIN_AUCTION_TIME` — and `MIN_AUCTION_TIME` is `2*HOUR`). Anything else is
/// refused with [`auction_error::DATABASE`]. The same 1/4/12 multiple also scales the deposit.
pub mod auction_duration {
    pub const SHORT_MINUTES: u32 = 120;
    pub const MEDIUM_MINUTES: u32 = 480;
    pub const LONG_MINUTES: u32 = 1440;
}

/// Records per list page. Every server list builder stops at 50 (`count < 50`), so paging is
/// `list_from = page * AUCTION_PAGE_SIZE` and the pager's "of N" comes from the list result's
/// trailing `total_count`, which is counted *before* this cap.
pub const AUCTION_PAGE_SIZE: u32 = 50;

/// One list-result record's fixed width: **64** bytes — twelve 4-byte fields plus the two 8-byte
/// guids (`owner_guid`, `bidder_guid`). Decision 1511's prose says 60; that is an arithmetic slip
/// in the summary, not in its field list, which sums to 64 (VERIFIED by re-reading the single
/// producer, `AuctionEntry::BuildAuctionInfo`, `AuctionHouseMgr.cpp:811-842`, whose writes are
/// `7×u32, u64, 4×u32, u64, u32`). Load-bearing rather than decorative — it is the bound
/// [`read_auction_list_result`] uses to survive a body shorter than its own `count` claims.
pub const AUCTION_RECORD_BYTES: usize = 64;

/// One row of any of the three auction list results. `SMSG_AUCTION_LIST_RESULT`,
/// `SMSG_AUCTION_OWNER_LIST_RESULT` and `SMSG_AUCTION_BIDDER_LIST_RESULT` share **one** record
/// layout and one frame — all three call the same producer (VERIFIED vmangos
/// `AuctionEntry::BuildAuctionInfo`). Fixed width ([`AUCTION_RECORD_BYTES`]), no cstrings: the
/// item's *name* is not on the wire at all, and comes from the ordinary item-template cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuctionListEntry {
    /// The auction's server id — the handle [`auction_place_bid`]/[`auction_remove_item`] name.
    pub auction_id: u32,
    pub item_entry: u32,
    /// `PERM_ENCHANTMENT_SLOT` only. 1.12 carries no other enchant information on an auction row:
    /// vmangos's own `[-ZERO] no other infos about enchantment in 1.12 [?]` comment sits on the
    /// commented-out loop that would have written the durations and charges a later client reads.
    pub perm_enchant: u32,
    /// **Signed** — negative is a random *suffix* id, positive a random *property* id.
    /// `Item::GetItemRandomPropertyId` returns `int32` and the server casts it through `uint32`,
    /// so the wire bytes are a two's-complement `i32` and reading them unsigned turns every
    /// suffixed item ("of the Bear") into a ~4.29-billion property id that resolves to nothing.
    pub random_property_id: i32,
    pub suffix_factor: u32,
    /// The stack size.
    pub count: u32,
    /// **Signed** — a negative charge count means "N charges, then destroy the item".
    /// `Item::GetSpellCharges` returns `int32` and the server casts it through `uint32` (its own
    /// comment on the line reads `item->charge FFFFFFF`).
    pub spell_charges: i32,
    pub owner_guid: u64,
    /// The seller's **opening** price, captured at listing time and constant for the auction's
    /// life. NOT the current bid ([`Self::current_bid`]) and not the minimum next bid — vmangos's
    /// own field comment calls it "maybe useless", but it *is* what the first bid must meet.
    pub start_bid: u32,
    /// How much above [`Self::current_bid`] the next bid must go: 5% of the current bid, floored,
    /// minimum 1 copper. **`0` while nobody has bid** — the server writes
    /// `bid ? GetAuctionOutBid() : 0`, so a UI must fall back to [`Self::start_bid`] for the
    /// opening bid rather than reading a `0` here as "any amount will do".
    pub min_increment: u32,
    /// `0` = no buyout. Buyout is *inferred*, never flagged: a bid of `>= buyout` on a nonzero
    /// buyout is a buyout.
    pub buyout: u32,
    /// **Milliseconds** remaining, and **unclamped**. The server writes
    /// `(expireTime - now) * IN_MILLISECONDS` with no floor and only sweeps expiry on its own
    /// timer, so an expired-but-still-listed auction wraps to a value near 4.29 billion. Read
    /// faithfully here — a consumer must treat an implausibly large value as **expired**, not as
    /// "very long".
    pub time_left_ms: u32,
    /// `0` = nobody has bid.
    pub bidder_guid: u64,
    /// `0` = no bid yet.
    pub current_bid: u32,
}

/// The conditional tail `SMSG_AUCTION_COMMAND_RESULT` carries after its three dwords — which
/// shape follows is decided by `error` first and `action` only inside the `OK` arm (VERIFIED
/// vmangos `WorldSession::SendAuctionCommandResult`, `AuctionHouseHandler.cpp:70-96`, whose
/// `switch` is on the error code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuctionCommandTail {
    /// Every other (error, action) pair — the three dwords are the whole packet. This is the
    /// common case: a plain `OK` + `STARTED`/`REMOVED`, and every named error but two.
    Empty,
    /// [`auction_error::OK`] **and** [`auction_action::BID_PLACED`]: the outbid step the *next*
    /// bid must clear, recomputed from the bid just accepted.
    BidPlaced { new_min_outbid: u32 },
    /// [`auction_error::INVENTORY`]: an `InventoryResult` (`EQUIP_ERR_*`) code.
    Inventory { result: u32 },
    /// [`auction_error::HIGHER_BID`]: we were outbid between reading the page and bidding — who
    /// holds it now, at what bid, and the step the next bid must clear.
    HigherBid {
        new_bidder_guid: u64,
        new_bid: u32,
        new_min_outbid: u32,
    },
}

/// `SMSG_AUCTION_BIDDER_NOTIFICATION` — "you won" / "you were outbid", pushed to the *bidder*
/// (VERIFIED vmangos `AuctionBidderNotification::AppendBodyTo`,
/// `Server/Packets/AuctionHouse.cpp:78-87`). Deliberately **not** the same shape as
/// [`AuctionOwnerNotification`]: this one leads with the house id and puts the guid third, the
/// owner one has no house id and puts the guid fourth. Two structs and two readers, on purpose —
/// sharing either would silently mis-decode one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuctionBidderNotification {
    /// `AuctionHouse.dbc` row (1..7) — the same id [`read_auction_hello`] answers with.
    pub house_id: u32,
    pub auction_id: u32,
    /// The bidder the notice is *about* — us.
    pub bidder_guid: u64,
    /// **`0` means WON**, not "no bid" (vmangos writes `won ? 0 : auction->bid`, and the real
    /// client branches `ERR_AUCTION_WON_S` vs `ERR_AUCTION_OUTBID_S` on exactly this). Nonzero is
    /// the bid that beat us.
    pub bid_or_zero: u32,
    /// The step the next bid must clear (`GetAuctionOutBid()`).
    pub out_bid: u32,
    pub item_entry: u32,
    /// **Signed**, as on [`AuctionListEntry::random_property_id`] — the same
    /// `Item::GetItemRandomPropertyId()` value, cast through vmangos's `uint32` packet field.
    /// `0` when the item is gone.
    pub random_property_id: i32,
}

/// `SMSG_AUCTION_OWNER_NOTIFICATION` — "your auction sold" / a bid landed, pushed to the *seller*
/// (VERIFIED vmangos `AuctionOwnerNotification::AppendBodyTo`,
/// `Server/Packets/AuctionHouse.cpp:89-97`). **No `house_id`, and the guid sits fourth** — see
/// [`AuctionBidderNotification`] for why the two are kept apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuctionOwnerNotification {
    pub auction_id: u32,
    /// The current bid.
    pub bid: u32,
    /// The step the next bid must clear (`GetAuctionOutBid()`).
    pub out_bid: u32,
    /// Who bid. **All-zero on a sale**: vmangos fills this field only when the auction has *not*
    /// sold yet (`if (!sold)`), so a zero guid here is the "sold" signal, not a missing bidder.
    pub bidder_guid: u64,
    pub item_entry: u32,
    /// **Signed**, as on [`AuctionListEntry::random_property_id`]. `0` when the item is gone.
    pub random_property_id: i32,
}

/// Body of `MSG_AUCTION_HELLO` (VERIFIED vmangos `AuctionHello::ReadFromWorldPacket`,
/// `Server/Packets/AuctionHouse.cpp:3-6`): one full 8-byte auctioneer guid. **Same opcode both
/// directions** — the reply ([`read_auction_hello`]) echoes the guid and adds the house id, and it
/// is the *reply* that opens the window, not our send.
pub fn auction_hello(auctioneer: u64) -> Vec<u8> {
    auctioneer.to_le_bytes().to_vec()
}

/// Body of `CMSG_AUCTION_SELL_ITEM` (VERIFIED vmangos `AuctionSellItem::ReadFromWorldPacket`,
/// `Server/Packets/AuctionHouse.cpp:42-49`): `u64 auctioneer, u64 itemGuid, u32 bid, u32 buyout,
/// u32 etime`.
///
/// `etime_minutes` may only be one of [`auction_duration`]'s three values; anything else is
/// refused with [`auction_error::DATABASE`]. A `bid` or `etime_minutes` of `0` is answered with
/// **nothing at all** (`HandleAuctionSellItem`'s silent `return`), as is a bid the player cannot
/// pay the deposit for — a UI that blocks waiting for an ack will hang.
pub fn auction_sell_item(
    auctioneer: u64,
    item_guid: u64,
    bid: u32,
    buyout: u32,
    etime_minutes: u32,
) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + 8 + 4 + 4 + 4);
    body.extend_from_slice(&auctioneer.to_le_bytes());
    body.extend_from_slice(&item_guid.to_le_bytes());
    body.extend_from_slice(&bid.to_le_bytes());
    body.extend_from_slice(&buyout.to_le_bytes());
    body.extend_from_slice(&etime_minutes.to_le_bytes());
    body
}

/// Body of `CMSG_AUCTION_REMOVE_ITEM` (VERIFIED vmangos `AuctionRemoveItem::ReadFromWorldPacket`,
/// `Server/Packets/AuctionHouse.cpp:36-40`): `u64 auctioneer, u32 auctionId` — the seller's
/// cancel. Answered by `SMSG_AUCTION_COMMAND_RESULT` (action [`auction_action::REMOVED`]) *except*
/// when the auction already carries a bid and the seller cannot pay the 5% cut out of pocket, in
/// which case the server refuses **silently**.
pub fn auction_remove_item(auctioneer: u64, auction_id: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + 4);
    body.extend_from_slice(&auctioneer.to_le_bytes());
    body.extend_from_slice(&auction_id.to_le_bytes());
    body
}

/// Body of `CMSG_AUCTION_LIST_ITEMS` — the Browse tab's search (VERIFIED vmangos
/// `AuctionListItems::ReadFromWorldPacket`, `Server/Packets/AuctionHouse.cpp:51-63`): `u64
/// auctioneer, u32 listfrom, cstr searchedname, u8 levelmin, u8 levelmax, u32 auctionSlotID, u32
/// auctionMainCategory, u32 auctionSubCategory, u32 quality, u8 usable`.
///
/// **Ten fields and nothing else: no sort columns, no sort count, no trailing padding.** That is
/// the field an implementer invents, because later expansions do carry sort bytes here — on 5875
/// *filtering* is entirely server-side and *sorting* is entirely client-side. (Corroborated
/// beyond the reader: vmangos runs `VerifyPacketWasCorrectlyRead` after every `ClientPacket`
/// parse and error-logs a body it did not consume to the end, which a real 5875 client appending
/// sort bytes would trip on every search.)
///
/// The sentinels are in [`auction_filter`]: `0xFFFF_FFFF` for the four u32 filters, `0` for the
/// three small ones, and an empty `searched_name` is just the lone NUL. `quality` is a
/// **minimum**, not an equality (`proto->Quality < query.quality` is what drops a row).
/// `level_min`/`level_max` gate `RequiredLevel`, not item level. The name match is a
/// case-insensitive substring against the localized name *with the random-property suffix
/// appended*. `list_from` pages by [`AUCTION_PAGE_SIZE`].
#[allow(clippy::too_many_arguments)]
pub fn auction_list_items(
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
) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + 4 + searched_name.len() + 1 + 1 + 1 + 4 + 4 + 4 + 4 + 1);
    body.extend_from_slice(&auctioneer.to_le_bytes());
    body.extend_from_slice(&list_from.to_le_bytes());
    body.extend_from_slice(searched_name.as_bytes());
    body.push(0);
    body.push(level_min);
    body.push(level_max);
    body.extend_from_slice(&slot_id.to_le_bytes());
    body.extend_from_slice(&main_category.to_le_bytes());
    body.extend_from_slice(&sub_category.to_le_bytes());
    body.extend_from_slice(&quality.to_le_bytes());
    body.push(usable);
    body
}

/// Body of `CMSG_AUCTION_LIST_OWNER_ITEMS` — the Auctions tab (VERIFIED vmangos
/// `AuctionListOwnerItems::ReadFromWorldPacket`, `Server/Packets/AuctionHouse.cpp:23-27`): `u64
/// auctioneer, u32 listfrom`. Answered by `SMSG_AUCTION_OWNER_LIST_RESULT`. The server matches on
/// the **account**, then filters to this character's guid.
pub fn auction_list_owner_items(auctioneer: u64, list_from: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + 4);
    body.extend_from_slice(&auctioneer.to_le_bytes());
    body.extend_from_slice(&list_from.to_le_bytes());
    body
}

/// Body of `CMSG_AUCTION_PLACE_BID` (VERIFIED vmangos `AuctionPlaceBid::ReadFromWorldPacket`,
/// `Server/Packets/AuctionHouse.cpp:29-34`): `u64 auctioneer, u32 auctionId, u32 price`. The
/// buyout verb too — send `price = buyout` and the server takes it as one; there is no separate
/// opcode and no flag. A bid the player cannot afford is refused **silently**.
pub fn auction_place_bid(auctioneer: u64, auction_id: u32, price: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + 4 + 4);
    body.extend_from_slice(&auctioneer.to_le_bytes());
    body.extend_from_slice(&auction_id.to_le_bytes());
    body.extend_from_slice(&price.to_le_bytes());
    body
}

/// Body of `CMSG_AUCTION_LIST_BIDDER_ITEMS` — the Bid tab (VERIFIED vmangos
/// `AuctionListBidderItem::ReadFromWorldPacket`, `Server/Packets/AuctionHouse.cpp:8-21`): `u64
/// auctioneer, u32 listfrom, u32 count`, then `count` × `u32 auctionId`.
///
/// The trailing id list is a **refresh set**, not a filter: the server emits a record for each id
/// named here *first* and then appends every auction this player is the current bidder on, both
/// feeding one `count`/`total_count` — so an id that is also still ours appears twice in the page.
/// Send it empty (`&[]`) for a plain page fetch; the reference client fills it with the auctions
/// it has been outbid on so their rows re-sync.
pub fn auction_list_bidder_items(auctioneer: u64, list_from: u32, auction_ids: &[u32]) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + 4 + 4 + 4 * auction_ids.len());
    body.extend_from_slice(&auctioneer.to_le_bytes());
    body.extend_from_slice(&list_from.to_le_bytes());
    body.extend_from_slice(&(auction_ids.len() as u32).to_le_bytes());
    for id in auction_ids {
        body.extend_from_slice(&id.to_le_bytes());
    }
    body
}

/// Read `MSG_AUCTION_HELLO`'s **reply** (VERIFIED vmangos `AuctionHelloResponse::AppendBodyTo`,
/// `Server/Packets/AuctionHouse.cpp:72-76`): `u64 auctioneerGuid, u32 houseId`. Nothing follows
/// the house id on 5875.
///
/// `house_id` indexes `AuctionHouse.dbc` (rows 1..7: the six faction houses, then Blackwater at 7)
/// and is the field the sell pane needs — the deposit and cut rates are read out of that row, so a
/// window that assumes a house shows the neutral 25% where it should show 5%. It is also the id
/// the auction invoice mails arrive under (`MailListEntry::sender_id` on a
/// [`super::mail_message_type::AUCTION`] mail).
pub(super) fn read_auction_hello(r: &mut &[u8]) -> io::Result<(u64, u32)> {
    Ok((read_u64_le(r)?, read_u32_le(r)?))
}

/// One [`AuctionListEntry`] — [`AUCTION_RECORD_BYTES`] of fixed-width fields, 14 reads, in
/// `AuctionEntry::BuildAuctionInfo`'s order.
fn read_auction_list_entry(r: &mut &[u8]) -> io::Result<AuctionListEntry> {
    Ok(AuctionListEntry {
        auction_id: read_u32_le(r)?,
        item_entry: read_u32_le(r)?,
        perm_enchant: read_u32_le(r)?,
        random_property_id: read_i32_le(r)?,
        suffix_factor: read_u32_le(r)?,
        count: read_u32_le(r)?,
        spell_charges: read_i32_le(r)?,
        owner_guid: read_u64_le(r)?,
        start_bid: read_u32_le(r)?,
        min_increment: read_u32_le(r)?,
        buyout: read_u32_le(r)?,
        time_left_ms: read_u32_le(r)?,
        bidder_guid: read_u64_le(r)?,
        current_bid: read_u32_le(r)?,
    })
}

/// Read the body all three list results share — `SMSG_AUCTION_LIST_RESULT`,
/// `SMSG_AUCTION_OWNER_LIST_RESULT`, `SMSG_AUCTION_BIDDER_LIST_RESULT` (VERIFIED vmangos's async
/// `AuctionHouseClientQueryTask`, `AuctionHouseHandler.cpp:651-701`, which builds all three and
/// differs only in `SetOpcode`): `u32 count`, then `count` × [`AuctionListEntry`], then `u32
/// totalCount`.
///
/// **`total_count` rides at the very END, after the records.** The leading `count` is written as
/// a placeholder and patched with `put<uint32>` once the records are in; the total is appended
/// last. It is the match count *before* the [`AUCTION_PAGE_SIZE`] cap — the pager's "of N" — so
/// it is routinely larger than the number of records, and reading it in the wrong place silently
/// yields a record field.
///
/// Returns `(records, total_count)`.
pub(super) fn read_auction_list_result(r: &mut &[u8]) -> io::Result<(Vec<AuctionListEntry>, u32)> {
    let count = read_u32_le(r)?;
    // Bound the allocation by what the buffer could actually hold, not by a `count` we do not
    // trust (see the loop guard below).
    let mut auctions = Vec::with_capacity((count as usize).min(r.len() / AUCTION_RECORD_BYTES));
    for _ in 0..count {
        // `count` is an UPPER BOUND, not a record count. vmangos's *browse fast
        // path* (the no-filter branch of `BuildListAuctionItems`, `AuctionHouseMgr.cpp:716-735`)
        // does `BuildAuctionInfo(data); if ((++count) >= 50) break;` — discarding a `false`
        // return. A stale auction whose item row is gone therefore writes ZERO bytes and still
        // counts, so a real server sends `count = N` with N-k records. Stop cleanly on a short
        // buffer and return what we did get: the layout is fixed-width, so a dropped record
        // shifts nothing, and every record we read is intact.
        if r.len() < AUCTION_RECORD_BYTES {
            break;
        }
        auctions.push(read_auction_list_entry(r)?);
    }
    // A body truncated past the last record has no trailing total either; fall back to what we
    // actually read rather than failing the whole page over the pager's hint.
    let total_count = if r.len() >= 4 {
        read_u32_le(r)?
    } else {
        auctions.len() as u32
    };
    Ok((auctions, total_count))
}

/// Read `SMSG_AUCTION_COMMAND_RESULT` (VERIFIED vmangos `WorldSession::SendAuctionCommandResult`,
/// `AuctionHouseHandler.cpp:70-96`): `u32 auctionId, u32 action ([`auction_action`]), u32 error
/// ([`auction_error`])`, then the conditional [`AuctionCommandTail`] the error selects.
///
/// `auction_id` is **`0`** whenever the server had no `AuctionEntry` to name — which is most of
/// the failure paths, so it is not a usable correlation handle on an error. The
/// remaining-length guard on each tail mirrors `read_send_mail_result`: a body that doesn't carry
/// the tail its fields predict decodes as [`AuctionCommandTail::Empty`] rather than erroring.
///
/// Returns `(auction_id, action, error, tail)`.
pub(super) fn read_auction_command_result(
    r: &mut &[u8],
) -> io::Result<(u32, u32, u32, AuctionCommandTail)> {
    let auction_id = read_u32_le(r)?;
    let action = read_u32_le(r)?;
    let error = read_u32_le(r)?;
    // The server switches on `error`; only its OK arm then looks at `action`. Mirror that order —
    // an OK with any other action carries no tail at all.
    let tail = match error {
        auction_error::OK if action == auction_action::BID_PLACED && !r.is_empty() => {
            AuctionCommandTail::BidPlaced {
                new_min_outbid: read_u32_le(r)?,
            }
        }
        auction_error::INVENTORY if !r.is_empty() => AuctionCommandTail::Inventory {
            result: read_u32_le(r)?,
        },
        auction_error::HIGHER_BID if !r.is_empty() => AuctionCommandTail::HigherBid {
            new_bidder_guid: read_u64_le(r)?,
            new_bid: read_u32_le(r)?,
            new_min_outbid: read_u32_le(r)?,
        },
        _ => AuctionCommandTail::Empty,
    };
    Ok((auction_id, action, error, tail))
}

/// Read `SMSG_AUCTION_BIDDER_NOTIFICATION` (VERIFIED vmangos
/// `AuctionBidderNotification::AppendBodyTo`, `Server/Packets/AuctionHouse.cpp:78-87`): `u32
/// houseId, u32 auctionId, u64 bidderGuid, u32 bidOrZero, u32 outBid, u32 itemEntry, i32
/// randomPropertyId`. **Not interchangeable with [`read_auction_owner_notification`]** — see
/// [`AuctionBidderNotification`].
pub(super) fn read_auction_bidder_notification(
    r: &mut &[u8],
) -> io::Result<AuctionBidderNotification> {
    Ok(AuctionBidderNotification {
        house_id: read_u32_le(r)?,
        auction_id: read_u32_le(r)?,
        bidder_guid: read_u64_le(r)?,
        bid_or_zero: read_u32_le(r)?,
        out_bid: read_u32_le(r)?,
        item_entry: read_u32_le(r)?,
        random_property_id: read_i32_le(r)?,
    })
}

/// Read `SMSG_AUCTION_OWNER_NOTIFICATION` (VERIFIED vmangos
/// `AuctionOwnerNotification::AppendBodyTo`, `Server/Packets/AuctionHouse.cpp:89-97`): `u32
/// auctionId, u32 bid, u32 outBid, u64 bidderGuid, u32 itemEntry, i32 randomPropertyId` — a
/// different field order from the bidder notification, and no house id.
pub(super) fn read_auction_owner_notification(
    r: &mut &[u8],
) -> io::Result<AuctionOwnerNotification> {
    Ok(AuctionOwnerNotification {
        auction_id: read_u32_le(r)?,
        bid: read_u32_le(r)?,
        out_bid: read_u32_le(r)?,
        bidder_guid: read_u64_le(r)?,
        item_entry: read_u32_le(r)?,
        random_property_id: read_i32_le(r)?,
    })
}

/// Read `SMSG_AUCTION_REMOVED_NOTIFICATION` (VERIFIED vmangos
/// `AuctionRemovedNotification::AppendBodyTo`, `Server/Packets/AuctionHouse.cpp:65-70`): `u32
/// auctionId, u32 itemEntry, i32 randomPropertyId` — pushed to a *bidder* whose auction the seller
/// cancelled (the real client shows `ERR_AUCTION_REMOVED_S`).
///
/// Returns `(auction_id, item_entry, random_property_id)`.
pub(super) fn read_auction_removed_notification(r: &mut &[u8]) -> io::Result<(u32, u32, i32)> {
    Ok((read_u32_le(r)?, read_u32_le(r)?, read_i32_le(r)?))
}
