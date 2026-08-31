//! The auction house's wire→session bridge (decision 1511 P1) — every `SMSG_AUCTION_*` lands here
//! and becomes state on [`AuctionOpen`], which [`crate::ui_auction::feed_auction`] turns into the
//! window's events on the next frame. Nothing here touches the VM: the feed owns the script.

use benilla_protocol::messages::{
    auction_action, auction_error, AuctionBidderNotification, AuctionCommandTail, AuctionListEntry,
    AuctionOwnerNotification,
};
use benilla_ui::script::{BIDDER, LIST, OWNER};

use crate::ui_auction::{AuctionMessage, AuctionOpen};

/// `MSG_AUCTION_HELLO`'s reply — **this**, not our send, is what opens the window (wow-re: the
/// window's opener runs inside the hello handler). The house id keys the deposit rate.
pub(super) fn auction_hello(auctioneer: u64, house_id: u32, auction: &mut AuctionOpen) {
    auction.open(auctioneer, house_id);
}

/// One of the three list results. They share a frame and a record; only the tab differs.
pub(super) fn auction_list(
    which: usize,
    auctions: Vec<AuctionListEntry>,
    total_count: u32,
    auction: &mut AuctionOpen,
) {
    // Tallied before the drop below, because "it arrived and there was nowhere to put it" and "it
    // never arrived" are different answers (`AuctionWireLog`).
    auction.wire.list_results[which] += 1;
    // A list can arrive for a window that just closed (we walked away while the page was in
    // flight); dropping it is the same thing the reference does by having nowhere to put it.
    if auction.auctioneer.is_none() {
        return;
    }
    auction.set_list(which, auctions, total_count);
}

pub(super) fn auction_list_result(
    auctions: Vec<AuctionListEntry>,
    total_count: u32,
    auction: &mut AuctionOpen,
) {
    auction_list(LIST, auctions, total_count, auction);
}

pub(super) fn auction_owner_list_result(
    auctions: Vec<AuctionListEntry>,
    total_count: u32,
    auction: &mut AuctionOpen,
) {
    auction_list(OWNER, auctions, total_count, auction);
}

/// The bidder page. The server emits the explicitly-refreshed ids first and then every auction we
/// currently hold the bid on, so **one auction can appear twice in a page** — deduped here by
/// auction id, keeping the first occurrence, because a duplicated row would be two rows the player
/// can click that address the same auction.
pub(super) fn auction_bidder_list_result(
    auctions: Vec<AuctionListEntry>,
    total_count: u32,
    auction: &mut AuctionOpen,
) {
    let mut seen = std::collections::HashSet::with_capacity(auctions.len());
    let deduped: Vec<AuctionListEntry> = auctions
        .into_iter()
        .filter(|e| seen.insert(e.auction_id))
        .collect();
    auction_list(BIDDER, deduped, total_count, auction);
}

/// `SMSG_AUCTION_COMMAND_RESULT` — the verdict on a sell, a cancel or a bid.
///
/// Two shapes of failure the UI has to survive: `auction_id` is **`0`** on most failure paths (the
/// server writes `auc ? auc->Id : 0`), so it is not a correlation handle on an error; and several
/// refusals send **no packet at all** (a bid the player cannot afford, a cancel whose cut they
/// cannot pay), which is why nothing in this arc blocks its UI waiting for an ack.
pub(super) fn auction_command_result(
    auction_id: u32,
    action: u32,
    error: u32,
    tail: &AuctionCommandTail,
    auction: &mut AuctionOpen,
) {
    let _ = tail;
    // The verdict itself, kept for the live probe only (`AuctionWireLog`): a SUCCESSFUL result
    // turns into a re-query below and otherwise leaves no trace, so without this a probe cannot
    // tell "the server said STARTED/OK" from "the server said nothing at all".
    auction.wire.last_command = Some((auction_id, action, error));
    if error == auction_error::OK {
        // A successful sell/cancel/bid changes a list we are showing. The reference re-queries
        // rather than patching its local copy, and so do we — the server is the only thing that
        // knows what the page looks like now. Each also says so, in **chat** — the success arm
        // shows `0x178`/`0x179`/`0x17f` keyed on the action field, with zero varargs
        // (wow-re §11.2/§11.3).
        match action {
            auction_action::STARTED => {
                // The item is gone from the bag; the sell slot must stop claiming to hold it.
                auction.sell_slot_taken();
                auction.refresh_owner();
                auction
                    .messages
                    .push(AuctionMessage::chat("ERR_AUCTION_STARTED"));
            }
            auction_action::REMOVED => {
                auction.refresh_owner();
                auction
                    .messages
                    .push(AuctionMessage::chat("ERR_AUCTION_REMOVED"));
            }
            auction_action::BID_PLACED => {
                auction.refresh_bidder();
                auction
                    .messages
                    .push(AuctionMessage::chat("ERR_AUCTION_BID_PLACED"));
            }
            _ => {}
        }
        return;
    }
    // **HIGHER_BID says nothing here.** Its arm (`0x4cc672`) reads two extra fields and takes the
    // live *outbid update* path — it patches the row rather than raising a message, and the line
    // the player actually sees is `ERR_AUCTION_OUTBID_S`, off the bidder notification. Printing a
    // refusal here would double it (wow-re §11.2).
    if error == auction_error::HIGHER_BID {
        auction.refresh_bidder();
        return;
    }
    if let Some(key) = command_error_key(error) {
        auction.messages.push(AuctionMessage::error(key));
    }
}

/// The failed command's GlobalStrings key — resolved to text in the feed against the player's own
/// table, never carried as English here (decisions 0669 / 1190).
///
/// **INTERIM on three arms.** wow-re §11.2 reads the dispatch as: code 1 computes its id from the
/// packet's own second field through the *inventory*-result formatter (`0x622630`) — a different
/// message family, whose keys are not carved; code 4 raises `0x17`; code 13 raises `0x1be`. Those
/// three ids are outside the `ERR_AUCTION_*` block and their GlobalStrings names are not recorded
/// yet, so they fall to the catch-all here rather than being invented. Everything else is the
/// dispatch table verbatim, including that `2, 6, 8, 9, 11, 12` and anything above 13 are the
/// reference's own `ja` default.
fn command_error_key(error: u32) -> Option<&'static str> {
    Some(match error {
        auction_error::NOT_ENOUGH_MONEY => "ERR_NOT_ENOUGH_MONEY", // `0x25`, the shared id
        auction_error::BID_INCREMENT => "ERR_AUCTION_BID_INCREMENT", // `0x174`
        auction_error::BID_OWN => "ERR_AUCTION_BID_OWN",           // `0x173`
        // `0x172` — the server's catch-all, and the reference's default arm for every code
        // without one of its own.
        _ => "ERR_AUCTION_DATABASE_ERROR",
    })
}

/// `SMSG_AUCTION_BIDDER_NOTIFICATION` — we won, or we were outbid.
///
/// **`bid_or_zero == 0` means WON**, not "no bid" — the server overloads the field, and reading it
/// the obvious way turns every win into an outbid notice.
pub(super) fn auction_bidder_notification(
    notice: &AuctionBidderNotification,
    auction: &mut AuctionOpen,
) {
    let won = notice.bid_or_zero == 0;
    auction.messages.push(AuctionMessage::chat_item(
        if won {
            "ERR_AUCTION_WON_S"
        } else {
            "ERR_AUCTION_OUTBID_S"
        },
        notice.item_entry,
    ));
    auction.refresh_bidder();
}

/// `SMSG_AUCTION_OWNER_NOTIFICATION` — one of ours sold, or took a bid. An all-zero bidder guid is
/// the "sold" signal (the server zeroes it on a sale).
pub(super) fn auction_owner_notification(
    notice: &AuctionOwnerNotification,
    auction: &mut AuctionOpen,
) {
    // **Two stages, and the first one decides whether anything is said at all** (wow-re §11.4).
    // A NON-zero bidder guid is "somebody bid on your auction": the row updates and the client says
    // nothing — `[0x4cd25f, 0x4cd3bd)` holds no display call. Only a zeroed guid reaches the message
    // path, and there the *bid* picks the line: non-zero sold, zero expired.
    if notice.bidder_guid == 0 {
        auction.messages.push(AuctionMessage::chat_item(
            if notice.bid != 0 {
                "ERR_AUCTION_SOLD_S"
            } else {
                "ERR_AUCTION_EXPIRED_S"
            },
            notice.item_entry,
        ));
    }
    auction.refresh_owner();
}

/// `SMSG_AUCTION_REMOVED_NOTIFICATION` — an auction we had bid on was cancelled by its seller.
///
/// One id, unconditionally (wow-re §11.4: `0x4cd480` has exactly one `call 0x496720` and no branch
/// selecting an id).
pub(super) fn auction_removed_notification(item_entry: u32, auction: &mut AuctionOpen) {
    auction.messages.push(AuctionMessage::chat_item(
        "ERR_AUCTION_REMOVED_S",
        item_entry,
    ));
    auction.refresh_bidder();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_action::MsgKind;

    fn keys(auction: &AuctionOpen) -> Vec<(&'static str, MsgKind, Option<u32>)> {
        auction
            .messages
            .iter()
            .map(|m| (m.key, benilla_ui::messages::kind_of(m.key), m.item))
            .collect()
    }

    fn owner(bidder_guid: u64, bid: u32) -> AuctionOwnerNotification {
        AuctionOwnerNotification {
            auction_id: 7,
            bid,
            out_bid: 5,
            bidder_guid,
            item_entry: 2589,
            random_property_id: 0,
        }
    }

    fn bidder(bid_or_zero: u32) -> AuctionBidderNotification {
        AuctionBidderNotification {
            house_id: 1,
            auction_id: 7,
            bidder_guid: 0x6C,
            bid_or_zero,
            out_bid: 5,
            item_entry: 2589,
            random_property_id: 0,
        }
    }

    /// The owner notification's **two-stage** discrimination (wow-re §11.4), and the first stage is
    /// the one a re-implementation gets wrong: a NON-zero bidder guid means "somebody bid on your
    /// auction", and the client says **nothing at all** — the row updates and that is the whole
    /// response. Only a zeroed guid reaches the message path, and there the *bid* picks the line.
    #[test]
    fn a_bid_on_your_auction_updates_the_row_and_says_nothing() {
        let mut a = AuctionOpen::default();
        auction_owner_notification(&owner(0x6C, 500), &mut a);
        assert!(
            a.messages.is_empty(),
            "a live bid raises no message; `[0x4cd25f, 0x4cd3bd)` holds no display call"
        );

        // Sold: guid zeroed, and a price paid.
        let mut a = AuctionOpen::default();
        auction_owner_notification(&owner(0, 10_000), &mut a);
        assert_eq!(
            keys(&a),
            vec![("ERR_AUCTION_SOLD_S", MsgKind::Chat, Some(2589))]
        );

        // Expired: guid zeroed, and nobody paid anything.
        let mut a = AuctionOpen::default();
        auction_owner_notification(&owner(0, 0), &mut a);
        assert_eq!(
            keys(&a),
            vec![("ERR_AUCTION_EXPIRED_S", MsgKind::Chat, Some(2589))],
            "a zero bid on a closed auction is an expiry, not a sale"
        );
    }

    /// The bidder side's discriminator is the zero bid field and nothing else.
    #[test]
    fn a_zero_bid_field_means_you_won() {
        let mut a = AuctionOpen::default();
        auction_bidder_notification(&bidder(0), &mut a);
        assert_eq!(
            keys(&a),
            vec![("ERR_AUCTION_WON_S", MsgKind::Chat, Some(2589))]
        );

        let mut a = AuctionOpen::default();
        auction_bidder_notification(&bidder(9_000), &mut a);
        assert_eq!(
            keys(&a),
            vec![("ERR_AUCTION_OUTBID_S", MsgKind::Chat, Some(2589))]
        );
    }

    /// Every one of the eight OUTCOMES goes to CHAT, which is the director's report of 2026-08-22
    /// ("I just saw a red center screen message that one of my auc sold, pretty sure that the wrong
    /// place"). Catalog rows `0x178`-`0x17f` are kind 0; the twelve refusals below are kind 2.
    #[test]
    fn the_outcomes_are_chat_and_the_refusals_are_the_error_frame() {
        let tail = AuctionCommandTail::Empty;
        for (action, key) in [
            (auction_action::STARTED, "ERR_AUCTION_STARTED"),
            (auction_action::REMOVED, "ERR_AUCTION_REMOVED"),
            (auction_action::BID_PLACED, "ERR_AUCTION_BID_PLACED"),
        ] {
            let mut a = AuctionOpen::default();
            auction_command_result(7, action, auction_error::OK, &tail, &mut a);
            assert_eq!(
                keys(&a),
                vec![(key, MsgKind::Chat, None)],
                "a successful {action} says so in chat, with no item fill"
            );
        }

        let mut a = AuctionOpen::default();
        auction_removed_notification(2589, &mut a);
        assert_eq!(
            keys(&a),
            vec![("ERR_AUCTION_REMOVED_S", MsgKind::Chat, Some(2589))]
        );

        // A refusal, on the red frame.
        let mut a = AuctionOpen::default();
        auction_command_result(0, auction_action::BID_PLACED, 10, &tail, &mut a);
        assert_eq!(
            keys(&a),
            vec![("ERR_AUCTION_BID_OWN", MsgKind::Error, None)]
        );

        // HIGHER_BID is the live outbid UPDATE path, not a message — the line the player sees is
        // ERR_AUCTION_OUTBID_S off the bidder notification, and printing here would double it.
        let mut a = AuctionOpen::default();
        auction_command_result(7, auction_action::BID_PLACED, 5, &tail, &mut a);
        assert!(
            a.messages.is_empty(),
            "code 5 patches the row, it does not talk"
        );
    }

    /// A notice outlives the window. "Your auction sold" arrives wherever the player is standing,
    /// and closing an auctioneer's window between the packet and the next feed must not swallow it.
    #[test]
    fn closing_the_window_does_not_swallow_a_pending_notice() {
        let mut a = AuctionOpen::default();
        a.open(0x42, 1);
        auction_owner_notification(&owner(0, 10_000), &mut a);
        a.clear();
        assert_eq!(
            keys(&a),
            vec![("ERR_AUCTION_SOLD_S", MsgKind::Chat, Some(2589))]
        );
    }
}
