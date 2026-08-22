//! Oracle-free golden tests for the auction house arc's protocol layer (decision 1511 phase P0):
//! byte-exact goldens for all seven CMSG bodies, the `MSG_AUCTION_HELLO` reply, the four
//! `SMSG_AUCTION_COMMAND_RESULT` tail shapes, the one 64-byte record and one frame the three list
//! results share (including that `total_count` rides *after* the records, and that a body shorter
//! than its own `count` claims still yields the records it does carry), the two deliberately
//! different notifications, and the removal notice. Same idioms as `tests/mail.rs` — `hx(...)`
//! golden CMSG bodies, hand-built SMSG bodies round-tripped through `parse_server`, and a
//! `decode()` bridge assertion.

use benilla_protocol::events::{decode, SessionEvent};
use benilla_protocol::messages::{
    self, auction_action, auction_duration, auction_error, auction_filter,
    AuctionBidderNotification, AuctionCommandTail, AuctionListEntry, AuctionOwnerNotification,
    AUCTION_RECORD_BYTES,
};
use benilla_protocol::ServerPacket;

fn hx(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

const AUCTIONEER: u64 = 0x00F1_3000_0000_0055;

#[test]
fn auction_send_bodies_golden() {
    // MSG_AUCTION_HELLO: one full auctioneer guid, nothing else.
    assert_eq!(
        messages::auction_hello(AUCTIONEER),
        hx("550000000030f100"),
        "MSG_AUCTION_HELLO body"
    );

    // CMSG_AUCTION_SELL_ITEM: u64 auctioneer, u64 itemGuid, u32 bid, u32 buyout, u32 etime.
    assert_eq!(
        messages::auction_sell_item(
            AUCTIONEER,
            0x0000_0000_0100_00AB,
            10_000,
            50_000,
            auction_duration::MEDIUM_MINUTES,
        ),
        hx(concat!(
            "550000000030f100", // auctioneer
            "ab00000100000000", // item guid
            "10270000",         // bid 10000
            "50c30000",         // buyout 50000
            "e0010000",         // etime 480 minutes
        )),
        "CMSG_AUCTION_SELL_ITEM body"
    );

    // CMSG_AUCTION_REMOVE_ITEM: u64 auctioneer, u32 auctionId.
    assert_eq!(
        messages::auction_remove_item(AUCTIONEER, 4242),
        hx("550000000030f10092100000"),
        "CMSG_AUCTION_REMOVE_ITEM body"
    );

    // CMSG_AUCTION_PLACE_BID: u64 auctioneer, u32 auctionId, u32 price.
    assert_eq!(
        messages::auction_place_bid(AUCTIONEER, 4242, 12_345),
        hx("550000000030f1009210000039300000"),
        "CMSG_AUCTION_PLACE_BID body"
    );

    // CMSG_AUCTION_LIST_OWNER_ITEMS: u64 auctioneer, u32 listfrom.
    assert_eq!(
        messages::auction_list_owner_items(AUCTIONEER, 50),
        hx("550000000030f10032000000"),
        "CMSG_AUCTION_LIST_OWNER_ITEMS body"
    );
}

#[test]
fn auction_list_items_body_golden() {
    // CMSG_AUCTION_LIST_ITEMS with every filter set: u64 auctioneer, u32 listfrom, cstr name,
    // u8 levelmin, u8 levelmax, u32 slotId, u32 mainCategory, u32 subCategory, u32 quality,
    // u8 usable. TEN fields — no sort column, no sort count, no trailing padding on 5875.
    assert_eq!(
        messages::auction_list_items(AUCTIONEER, 50, "Copper", 10, 20, 1, 2, 3, 4, 1),
        hx(concat!(
            "550000000030f100", // auctioneer
            "32000000",         // listfrom 50
            "436f7070657200",   // "Copper\0"
            "0a",               // levelmin 10
            "14",               // levelmax 20
            "01000000",         // slotId
            "02000000",         // mainCategory
            "03000000",         // subCategory
            "04000000",         // quality (a MINIMUM, not an equality)
            "01",               // usable
        )),
        "CMSG_AUCTION_LIST_ITEMS body (filters set)"
    );

    // The default browse: every filter at its sentinel and an EMPTY search name — which is just
    // the lone NUL byte between `listfrom` and `levelmin`, nothing more. This exact body is the
    // one that puts vmangos on its no-filter fast path.
    let body = messages::auction_list_items(
        AUCTIONEER,
        0,
        "",
        auction_filter::ANY_LEVEL,
        auction_filter::ANY_LEVEL,
        auction_filter::ANY,
        auction_filter::ANY,
        auction_filter::ANY,
        auction_filter::ANY,
        auction_filter::ANY_USABILITY,
    );
    assert_eq!(
        body,
        hx(concat!(
            "550000000030f100", // auctioneer
            "00000000",         // listfrom 0
            "00",               // "" — the lone NUL, and the whole of the name field
            "00",               // levelmin 0
            "00",               // levelmax 0
            "ffffffff",         // slotId    ANY
            "ffffffff",         // mainCategory ANY
            "ffffffff",         // subCategory  ANY
            "ffffffff",         // quality      ANY
            "00",               // usable 0
        )),
        "CMSG_AUCTION_LIST_ITEMS body (empty name, all sentinels)"
    );
    // 8 + 4 + 1 + 1 + 1 + 4*4 + 1 — the tightest possible browse body. Stated as a number so an
    // invented trailing field cannot slip in behind a hex string nobody re-counts.
    assert_eq!(body.len(), 32, "no trailing sort bytes ride this opcode");
}

#[test]
fn auction_list_bidder_items_body_golden() {
    // CMSG_AUCTION_LIST_BIDDER_ITEMS with NO refresh ids: u64 auctioneer, u32 listfrom, u32 0.
    // The count field is always present even when the list is empty.
    assert_eq!(
        messages::auction_list_bidder_items(AUCTIONEER, 0, &[]),
        hx("550000000030f1000000000000000000"),
        "CMSG_AUCTION_LIST_BIDDER_ITEMS body (no ids)"
    );

    // ...and with several: the count, then that many u32 auction ids.
    assert_eq!(
        messages::auction_list_bidder_items(AUCTIONEER, 50, &[7, 4242, 0xDEAD_BEEF]),
        hx(concat!(
            "550000000030f100", // auctioneer
            "32000000",         // listfrom 50
            "03000000",         // 3 ids follow
            "07000000",         // id 7
            "92100000",         // id 4242
            "efbeadde",         // id 0xDEADBEEF
        )),
        "CMSG_AUCTION_LIST_BIDDER_ITEMS body (3 ids)"
    );
}

#[test]
fn auction_hello_reply_wire() {
    // MSG_AUCTION_HELLO's reply (same opcode as our request): u64 auctioneerGuid, u32 houseId.
    let mut body = AUCTIONEER.to_le_bytes().to_vec();
    body.extend_from_slice(&6u32.to_le_bytes()); // houseId 6 (Orgrimmar), AuctionHouse.dbc 1..7
    match messages::parse_server(messages::opcode::MSG_AUCTION_HELLO, &body).unwrap() {
        ServerPacket::AuctionHello {
            auctioneer,
            house_id,
        } => assert_eq!((auctioneer, house_id), (AUCTIONEER, 6)),
        other => panic!("auction hello, got {}", other.name()),
    }
    // The decode() bridge carries both fields through unchanged.
    let packet = messages::parse_server(messages::opcode::MSG_AUCTION_HELLO, &body).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::AuctionHello {
            auctioneer,
            house_id,
        } => assert_eq!((auctioneer, house_id), (AUCTIONEER, 6)),
        other => panic!("auction hello event, got {other:?}"),
    }
}

/// Build one `SMSG_AUCTION_COMMAND_RESULT` head: `u32 auctionId, u32 action, u32 error`.
fn command_head(auction_id: u32, action: u32, error: u32) -> Vec<u8> {
    let mut body = auction_id.to_le_bytes().to_vec();
    body.extend_from_slice(&action.to_le_bytes());
    body.extend_from_slice(&error.to_le_bytes());
    body
}

fn parse_command(body: &[u8]) -> (u32, u32, u32, AuctionCommandTail) {
    match messages::parse_server(messages::opcode::SMSG_AUCTION_COMMAND_RESULT, body).unwrap() {
        ServerPacket::AuctionCommandResult {
            auction_id,
            action,
            error,
            tail,
        } => (auction_id, action, error, tail),
        other => panic!("auction command result, got {}", other.name()),
    }
}

#[test]
fn auction_command_result_bare_wire() {
    // The bare 3-u32 form: OK + STARTED carries NO tail (the server's OK arm writes one only for
    // BID_PLACED). This is the shape a successful listing and a successful cancel both take.
    let body = command_head(4242, auction_action::STARTED, auction_error::OK);
    assert_eq!(body.len(), 12);
    assert_eq!(
        parse_command(&body),
        (
            4242,
            auction_action::STARTED,
            auction_error::OK,
            AuctionCommandTail::Empty
        )
    );

    // OK + REMOVED, likewise bare.
    assert_eq!(
        parse_command(&command_head(
            4242,
            auction_action::REMOVED,
            auction_error::OK
        ))
        .3,
        AuctionCommandTail::Empty
    );

    // A named error with no tail of its own (NOT_ENOUGH_MONEY) — bare too, and `auction_id` is
    // `0` because the server had no AuctionEntry to name.
    assert_eq!(
        parse_command(&command_head(
            0,
            auction_action::BID_PLACED,
            auction_error::NOT_ENOUGH_MONEY
        )),
        (
            0,
            auction_action::BID_PLACED,
            auction_error::NOT_ENOUGH_MONEY,
            AuctionCommandTail::Empty
        )
    );

    // An UNNAMED error code (6, 8, 9, 11, 12 have no vmangos enumerator) also decodes bare rather
    // than erroring — the real client falls through to a generic failure branch for these.
    assert_eq!(
        parse_command(&command_head(0, auction_action::BID_PLACED, 9)).3,
        AuctionCommandTail::Empty
    );

    // The decode() bridge, asserted on this shape.
    let packet = messages::parse_server(
        messages::opcode::SMSG_AUCTION_COMMAND_RESULT,
        &command_head(4242, auction_action::STARTED, auction_error::OK),
    )
    .unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::AuctionCommandResult {
            auction_id,
            action,
            error,
            tail,
        } => assert_eq!(
            (auction_id, action, error, tail),
            (
                4242,
                auction_action::STARTED,
                auction_error::OK,
                AuctionCommandTail::Empty
            )
        ),
        other => panic!("auction command result event, got {other:?}"),
    }
}

#[test]
fn auction_command_result_bid_placed_tail_wire() {
    // OK **and** BID_PLACED: one trailing u32 newMinOutBid. Both fields have to match — an OK on
    // any other action carries nothing (asserted in the bare test above).
    let mut body = command_head(4242, auction_action::BID_PLACED, auction_error::OK);
    body.extend_from_slice(&617u32.to_le_bytes());
    assert_eq!(
        parse_command(&body),
        (
            4242,
            auction_action::BID_PLACED,
            auction_error::OK,
            AuctionCommandTail::BidPlaced {
                new_min_outbid: 617
            }
        )
    );
}

#[test]
fn auction_command_result_inventory_tail_wire() {
    // INVENTORY: one trailing u32 InventoryResult (EQUIP_ERR_*). Keyed on the ERROR alone — the
    // action here is STARTED, which carries no tail under OK.
    let mut body = command_head(0, auction_action::STARTED, auction_error::INVENTORY);
    body.extend_from_slice(&2u32.to_le_bytes()); // EQUIP_ERR_* code
    assert_eq!(
        parse_command(&body),
        (
            0,
            auction_action::STARTED,
            auction_error::INVENTORY,
            AuctionCommandTail::Inventory { result: 2 }
        )
    );
}

#[test]
fn auction_command_result_higher_bid_tail_wire() {
    // HIGHER_BID: u64 newBidder, u32 newBid, u32 newMinOutBid — 16 trailing bytes, the longest of
    // the three tails and the one a reader most easily under-reads.
    let mut body = command_head(4242, auction_action::BID_PLACED, auction_error::HIGHER_BID);
    body.extend_from_slice(&0x0000_0000_0000_07D1u64.to_le_bytes()); // new bidder
    body.extend_from_slice(&25_000u32.to_le_bytes()); // new bid
    body.extend_from_slice(&1_250u32.to_le_bytes()); // new min outbid
    assert_eq!(body.len(), 12 + 16);
    assert_eq!(
        parse_command(&body),
        (
            4242,
            auction_action::BID_PLACED,
            auction_error::HIGHER_BID,
            AuctionCommandTail::HigherBid {
                new_bidder_guid: 0x07D1,
                new_bid: 25_000,
                new_min_outbid: 1_250,
            }
        )
    );
}

/// Append one 64-byte list-result record in `AuctionEntry::BuildAuctionInfo`'s order.
fn push_record(body: &mut Vec<u8>, e: &AuctionListEntry) {
    let before = body.len();
    body.extend_from_slice(&e.auction_id.to_le_bytes());
    body.extend_from_slice(&e.item_entry.to_le_bytes());
    body.extend_from_slice(&e.perm_enchant.to_le_bytes());
    body.extend_from_slice(&e.random_property_id.to_le_bytes());
    body.extend_from_slice(&e.suffix_factor.to_le_bytes());
    body.extend_from_slice(&e.count.to_le_bytes());
    body.extend_from_slice(&e.spell_charges.to_le_bytes());
    body.extend_from_slice(&e.owner_guid.to_le_bytes());
    body.extend_from_slice(&e.start_bid.to_le_bytes());
    body.extend_from_slice(&e.min_increment.to_le_bytes());
    body.extend_from_slice(&e.buyout.to_le_bytes());
    body.extend_from_slice(&e.time_left_ms.to_le_bytes());
    body.extend_from_slice(&e.bidder_guid.to_le_bytes());
    body.extend_from_slice(&e.current_bid.to_le_bytes());
    // The record's width is load-bearing (it is the reader's short-buffer bound) and decision
    // 1511's prose miscounts it as 60; pin it here so the two can never drift apart silently.
    assert_eq!(body.len() - before, AUCTION_RECORD_BYTES);
    assert_eq!(AUCTION_RECORD_BYTES, 64);
}

/// A no-bids row: `min_increment`/`bidder_guid`/`current_bid` all zero, a buyout set.
fn unbid_row(auction_id: u32) -> AuctionListEntry {
    AuctionListEntry {
        auction_id,
        item_entry: 2589, // Linen Cloth
        perm_enchant: 0,
        random_property_id: 0,
        suffix_factor: 0,
        count: 20,
        spell_charges: 0,
        owner_guid: 0x0000_0000_0000_0101,
        start_bid: 5_000,
        min_increment: 0,
        buyout: 20_000,
        time_left_ms: 7_200_000,
        bidder_guid: 0,
        current_bid: 0,
    }
}

fn parse_list(opcode: u16, body: &[u8]) -> (Vec<AuctionListEntry>, u32) {
    match messages::parse_server(opcode, body).unwrap() {
        ServerPacket::AuctionListResult {
            auctions,
            total_count,
        }
        | ServerPacket::AuctionOwnerListResult {
            auctions,
            total_count,
        }
        | ServerPacket::AuctionBidderListResult {
            auctions,
            total_count,
        } => (auctions, total_count),
        other => panic!("auction list result, got {}", other.name()),
    }
}

#[test]
fn auction_list_result_empty_wire() {
    // count 0, no records, then the trailing totalCount — which can still be nonzero (page 2 of a
    // one-page result set asked past the end).
    let mut body = 0u32.to_le_bytes().to_vec();
    body.extend_from_slice(&37u32.to_le_bytes());
    assert_eq!(body.len(), 8);

    // All three list opcodes share this frame and this record; assert on each so a future arm
    // cannot be wired to the wrong reader.
    for opcode in [
        messages::opcode::SMSG_AUCTION_LIST_RESULT,
        messages::opcode::SMSG_AUCTION_OWNER_LIST_RESULT,
        messages::opcode::SMSG_AUCTION_BIDDER_LIST_RESULT,
    ] {
        let (auctions, total_count) = parse_list(opcode, &body);
        assert!(auctions.is_empty());
        assert_eq!(total_count, 37);
    }
}

#[test]
fn auction_list_result_records_wire() {
    // Three rows through the real parse_server entry point, then the trailing totalCount.
    let rows = [
        unbid_row(1),
        AuctionListEntry {
            auction_id: 2,
            item_entry: 12_640, // Lionheart Helm
            perm_enchant: 2_504,
            random_property_id: 0,
            suffix_factor: 0,
            count: 1,
            spell_charges: 0,
            owner_guid: 0x0000_0000_0000_0202,
            start_bid: 1_000_000,
            min_increment: 60_000,
            buyout: 0, // no buyout
            time_left_ms: 86_400_000,
            bidder_guid: 0x0000_0000_0000_0303,
            current_bid: 1_200_000,
        },
        unbid_row(3),
    ];
    let mut body = (rows.len() as u32).to_le_bytes().to_vec();
    for row in &rows {
        push_record(&mut body, row);
    }
    body.extend_from_slice(&129u32.to_le_bytes()); // totalCount, well past the 50-row page cap
    assert_eq!(body.len(), 4 + 3 * AUCTION_RECORD_BYTES + 4);

    let (auctions, total_count) = parse_list(messages::opcode::SMSG_AUCTION_LIST_RESULT, &body);
    assert_eq!(auctions, rows);
    assert_eq!(total_count, 129);

    // Field semantics worth pinning rather than assuming: `min_increment` is 0 while nobody has
    // bid, `buyout` 0 means no buyout, and `start_bid` is NOT the current bid.
    assert_eq!(auctions[0].min_increment, 0);
    assert_eq!(auctions[0].current_bid, 0);
    assert_eq!(auctions[1].buyout, 0);
    assert_ne!(auctions[1].start_bid, auctions[1].current_bid);

    // The decode() bridge carries the rows and the total through unchanged.
    let packet = messages::parse_server(messages::opcode::SMSG_AUCTION_LIST_RESULT, &body).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::AuctionListResult {
            auctions,
            total_count,
        } => {
            assert_eq!(auctions.len(), 3);
            assert_eq!(total_count, 129);
        }
        other => panic!("auction list result event, got {other:?}"),
    }

    // The owner/bidder opcodes land on their own variants off the same bytes.
    let packet =
        messages::parse_server(messages::opcode::SMSG_AUCTION_OWNER_LIST_RESULT, &body).unwrap();
    assert!(matches!(
        decode(packet).pop().unwrap(),
        SessionEvent::AuctionOwnerListResult { .. }
    ));
    let packet =
        messages::parse_server(messages::opcode::SMSG_AUCTION_BIDDER_LIST_RESULT, &body).unwrap();
    assert!(matches!(
        decode(packet).pop().unwrap(),
        SessionEvent::AuctionBidderListResult { .. }
    ));
}

#[test]
fn auction_list_result_total_count_rides_after_the_records() {
    // `totalCount` is at the very END of the body, not beside the leading `count`. Build ONE
    // record whose every field differs from the total, so a reader that took the total from the
    // front (or from any record field) is caught: the leading count is 1, the total is 9999, and
    // no record field holds 9999.
    let row = AuctionListEntry {
        auction_id: 11,
        item_entry: 12,
        perm_enchant: 13,
        random_property_id: 14,
        suffix_factor: 15,
        count: 16,
        spell_charges: 17,
        owner_guid: 18,
        start_bid: 19,
        min_increment: 20,
        buyout: 21,
        time_left_ms: 22,
        bidder_guid: 23,
        current_bid: 24,
    };
    let mut body = 1u32.to_le_bytes().to_vec();
    push_record(&mut body, &row);
    body.extend_from_slice(&9_999u32.to_le_bytes());

    let (auctions, total_count) = parse_list(messages::opcode::SMSG_AUCTION_LIST_RESULT, &body);
    assert_eq!(auctions, [row], "the record reads intact");
    assert_eq!(
        total_count, 9_999,
        "totalCount comes from AFTER the records, not from the head or a record field"
    );
}

#[test]
fn auction_list_record_round_trips_negative_signed_fields() {
    // `random_property_id` and `spell_charges` are SIGNED (the server casts int32 through uint32).
    // A negative random property id is a random *suffix* ("of the Bear"); a negative charge count
    // means "N charges then destroy". Read unsigned, both become ~4.29-billion nonsense.
    let row = AuctionListEntry {
        auction_id: 77,
        item_entry: 7_078,
        perm_enchant: 0,
        random_property_id: -19, // a suffix id
        suffix_factor: 143,
        count: 1,
        spell_charges: -5, // 5 charges, then destroy
        owner_guid: 0x0000_0000_0000_0404,
        start_bid: 1,
        min_increment: 0,
        buyout: 0,
        time_left_ms: 1_000,
        bidder_guid: 0,
        current_bid: 0,
    };
    let mut body = 1u32.to_le_bytes().to_vec();
    push_record(&mut body, &row);
    body.extend_from_slice(&1u32.to_le_bytes());

    let (auctions, _) = parse_list(messages::opcode::SMSG_AUCTION_LIST_RESULT, &body);
    assert_eq!(auctions[0].random_property_id, -19);
    assert_eq!(auctions[0].spell_charges, -5);
    assert_eq!(auctions[0].suffix_factor, 143);

    // And the unclamped time_left: an expired-but-unswept auction wraps rather than clamping to
    // 0. The reader must pass it through faithfully; judging it is the consumer's job.
    let mut expired = unbid_row(78);
    expired.time_left_ms = u32::MAX - 500; // (expire - now) * 1000 on a negative difference
    let mut body = 1u32.to_le_bytes().to_vec();
    push_record(&mut body, &expired);
    body.extend_from_slice(&1u32.to_le_bytes());
    let (auctions, _) = parse_list(messages::opcode::SMSG_AUCTION_LIST_RESULT, &body);
    assert_eq!(auctions[0].time_left_ms, u32::MAX - 500);
}

#[test]
fn auction_list_result_survives_a_count_larger_than_the_records() {
    // vmangos's browse fast path increments `count` for a record it then fails to write (a stale
    // auction whose item row is gone writes ZERO bytes and still counts). A body claiming 3 and
    // carrying 2 must yield those 2 rather than failing the whole page.
    let rows = [unbid_row(1), unbid_row(2)];
    let mut body = 3u32.to_le_bytes().to_vec(); // the server's inflated count
    for row in &rows {
        push_record(&mut body, row);
    }
    body.extend_from_slice(&2u32.to_le_bytes()); // totalCount still rides at the end

    let (auctions, total_count) = parse_list(messages::opcode::SMSG_AUCTION_LIST_RESULT, &body);
    assert_eq!(auctions, rows, "the records that DID arrive come back");
    assert_eq!(total_count, 2);

    // The harsher variant: truncated past the last record, so even the trailing total is missing.
    // Still not an error — the total falls back to what we read.
    let mut body = 3u32.to_le_bytes().to_vec();
    for row in &rows {
        push_record(&mut body, row);
    }
    let (auctions, total_count) = parse_list(messages::opcode::SMSG_AUCTION_LIST_RESULT, &body);
    assert_eq!(auctions, rows);
    assert_eq!(total_count, 2, "falls back to the records actually read");

    // And a count that is pure nonsense (a hostile/desynced body) must not allocate on its word
    // alone or error — it yields the one record the buffer holds.
    let mut body = u32::MAX.to_le_bytes().to_vec();
    push_record(&mut body, &rows[0]);
    let (auctions, _) = parse_list(messages::opcode::SMSG_AUCTION_LIST_RESULT, &body);
    assert_eq!(auctions, [rows[0]]);
}

#[test]
fn auction_bidder_notification_wire() {
    // SMSG_AUCTION_BIDDER_NOTIFICATION: u32 houseId, u32 auctionId, u64 bidderGuid,
    // u32 bidOrZero, u32 outBid, u32 itemEntry, i32 randomPropertyId — houseId FIRST and the
    // guid THIRD, unlike the owner notification.
    let mut body = 6u32.to_le_bytes().to_vec(); // houseId
    body.extend_from_slice(&4242u32.to_le_bytes()); // auctionId
    body.extend_from_slice(&0x0000_0000_0000_0505u64.to_le_bytes()); // bidderGuid
    body.extend_from_slice(&15_000u32.to_le_bytes()); // bidOrZero — nonzero: OUTBID
    body.extend_from_slice(&750u32.to_le_bytes()); // outBid
    body.extend_from_slice(&12_640u32.to_le_bytes()); // itemEntry
    body.extend_from_slice(&(-19i32).to_le_bytes()); // randomPropertyId, signed

    let expected = AuctionBidderNotification {
        house_id: 6,
        auction_id: 4242,
        bidder_guid: 0x0505,
        bid_or_zero: 15_000,
        out_bid: 750,
        item_entry: 12_640,
        random_property_id: -19,
    };
    match messages::parse_server(messages::opcode::SMSG_AUCTION_BIDDER_NOTIFICATION, &body).unwrap()
    {
        ServerPacket::AuctionBidderNotification(n) => assert_eq!(n, expected),
        other => panic!("auction bidder notification, got {}", other.name()),
    }

    // bidOrZero == 0 means WON, not "no bid" — the one field whose zero is a *state*, and the
    // reason this notification cannot share a struct with the owner one.
    let mut won = body.clone();
    won[16..20].copy_from_slice(&0u32.to_le_bytes());
    match messages::parse_server(messages::opcode::SMSG_AUCTION_BIDDER_NOTIFICATION, &won).unwrap()
    {
        ServerPacket::AuctionBidderNotification(n) => {
            assert_eq!(n.bid_or_zero, 0, "0 = WON");
            assert_eq!(n.auction_id, 4242, "the rest of the row is unmoved");
            assert_eq!(n.out_bid, 750);
        }
        other => panic!("auction bidder notification (won), got {}", other.name()),
    }

    // The two notifications must never share a reader. Feed these same bytes to the OWNER opcode
    // (28 bytes needed, 32 available, so it parses rather than EOF-ing) and every field lands
    // somewhere else: the house id is eaten as the auction id and the guid slides four bytes.
    match messages::parse_server(messages::opcode::SMSG_AUCTION_OWNER_NOTIFICATION, &body).unwrap()
    {
        ServerPacket::AuctionOwnerNotification(n) => {
            assert_eq!(
                n.auction_id, 6,
                "the owner reader eats houseId as auctionId"
            );
            assert_ne!(n.bidder_guid, expected.bidder_guid, "the guid slides");
            assert_ne!(n.item_entry, expected.item_entry);
        }
        other => panic!("auction owner notification, got {}", other.name()),
    }

    let packet =
        messages::parse_server(messages::opcode::SMSG_AUCTION_BIDDER_NOTIFICATION, &body).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::AuctionBidderNotification(n) => assert_eq!(n, expected),
        other => panic!("auction bidder notification event, got {other:?}"),
    }
}

#[test]
fn auction_owner_notification_wire() {
    // SMSG_AUCTION_OWNER_NOTIFICATION: u32 auctionId, u32 bid, u32 outBid, u64 bidderGuid,
    // u32 itemEntry, i32 randomPropertyId — NO houseId, and the guid sits FOURTH. Deliberately a
    // different shape from the bidder notification above.
    let mut body = 4242u32.to_le_bytes().to_vec(); // auctionId
    body.extend_from_slice(&15_000u32.to_le_bytes()); // bid
    body.extend_from_slice(&750u32.to_le_bytes()); // outBid
    body.extend_from_slice(&0x0000_0000_0000_0505u64.to_le_bytes()); // bidderGuid
    body.extend_from_slice(&12_640u32.to_le_bytes()); // itemEntry
    body.extend_from_slice(&(-19i32).to_le_bytes()); // randomPropertyId, signed
    assert_eq!(body.len(), 28, "four bytes shorter than the bidder notice");

    let expected = AuctionOwnerNotification {
        auction_id: 4242,
        bid: 15_000,
        out_bid: 750,
        bidder_guid: 0x0505,
        item_entry: 12_640,
        random_property_id: -19,
    };
    match messages::parse_server(messages::opcode::SMSG_AUCTION_OWNER_NOTIFICATION, &body).unwrap()
    {
        ServerPacket::AuctionOwnerNotification(n) => assert_eq!(n, expected),
        other => panic!("auction owner notification, got {}", other.name()),
    }

    // An all-zero bidder guid is the "sold" signal, not a missing bidder.
    let mut sold = body.clone();
    sold[12..20].copy_from_slice(&0u64.to_le_bytes());
    match messages::parse_server(messages::opcode::SMSG_AUCTION_OWNER_NOTIFICATION, &sold).unwrap()
    {
        ServerPacket::AuctionOwnerNotification(n) => {
            assert_eq!(n.bidder_guid, 0, "0 = sold");
            assert_eq!(n.item_entry, 12_640, "the rest of the row is unmoved");
        }
        other => panic!("auction owner notification (sold), got {}", other.name()),
    }

    let packet =
        messages::parse_server(messages::opcode::SMSG_AUCTION_OWNER_NOTIFICATION, &body).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::AuctionOwnerNotification(n) => assert_eq!(n, expected),
        other => panic!("auction owner notification event, got {other:?}"),
    }
}

#[test]
fn auction_removed_notification_wire() {
    // SMSG_AUCTION_REMOVED_NOTIFICATION: u32 auctionId, u32 itemEntry, i32 randomPropertyId.
    let mut body = 4242u32.to_le_bytes().to_vec();
    body.extend_from_slice(&2589u32.to_le_bytes());
    body.extend_from_slice(&(-7i32).to_le_bytes());
    match messages::parse_server(messages::opcode::SMSG_AUCTION_REMOVED_NOTIFICATION, &body)
        .unwrap()
    {
        ServerPacket::AuctionRemovedNotification {
            auction_id,
            item_entry,
            random_property_id,
        } => assert_eq!(
            (auction_id, item_entry, random_property_id),
            (4242, 2589, -7)
        ),
        other => panic!("auction removed notification, got {}", other.name()),
    }

    let packet =
        messages::parse_server(messages::opcode::SMSG_AUCTION_REMOVED_NOTIFICATION, &body).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::AuctionRemovedNotification {
            auction_id,
            item_entry,
            random_property_id,
        } => assert_eq!(
            (auction_id, item_entry, random_property_id),
            (4242, 2589, -7)
        ),
        other => panic!("auction removed notification event, got {other:?}"),
    }
}
