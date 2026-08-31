//! The player-trade arc's drain-side arm bodies for [`super::apply_net_updates`]'s dispatch match
//! (decision 0592 P1). Each `pub(super)` fn here is one `SessionEvent` arm's body, driving the
//! [`crate::ui_trade::TradeSession`] state machine the feed reads; the match at the call site stays
//! the dispatcher. The `UiScript` events these ultimately drive are fired by
//! [`crate::ui_trade::feed_trade`] (the feed owns the VM), so these arms only mutate the session +
//! send the auto-reply.

use benilla_protocol::messages::{TradeStatus, TradeStatusExtended};

use crate::ui_trade::TradeSession;

/// `SessionEvent::TradeStatus` (`SMSG_TRADE_STATUS`) — the trade state machine (decision 0592 P1).
/// `BEGIN_TRADE` records the incoming request **without answering it** (decision 1764 — the reply
/// is a policy with five gates and the player's own consent, so
/// [`crate::ui_trade::answer_trade_request`] owns it); `OPEN_WINDOW` opens the window;
/// `TRADE_ACCEPT`/`BACK_TO_TRADE` drive the accept glow; the terminal and refusal codes close it.
/// The refusal statuses' red error text is decision 0592 P3; `ONLY_CONJURED` (a rejected item
/// placement, not a window close) and any out-of-range code are inert.
pub(super) fn trade_status(status: TradeStatus, trade: &mut TradeSession) {
    bevy::log::info!(target: "trade", "SMSG_TRADE_STATUS {status:?}");
    match status {
        TradeStatus::BeginTrade { partner } => trade.request(partner),
        TradeStatus::OpenWindow => trade.open_window(),
        TradeStatus::Accept => trade.partner_accepted(),
        TradeStatus::BackToTrade => trade.back_to_trade(),
        // Terminal (complete/cancel/close) + every initiate/in-trade refusal → close the window.
        //
        // **Two statuses sit outside the reference's own clear set, and neither is reachable on
        // vmangos** (decision 1764, noticed rather than chased). The reference clears its trade
        // cells in the status handler's common tail on 0, 3, 5, 6, 8, 10, 11, 12, 13 and 14–21;
        // this list is that set minus `Rejected` (9), which we close on and it does not, plus
        // `Unknown13` (13), which it clears on and we treat as inert below. vmangos sends neither,
        // so there is no observable to settle it against — which is exactly why it is written down
        // here instead of being guessed at in either direction.
        TradeStatus::Canceled
        | TradeStatus::Complete
        | TradeStatus::CloseWindow { .. }
        | TradeStatus::Rejected
        | TradeStatus::Busy
        | TradeStatus::Busy2
        | TradeStatus::NoTarget
        | TradeStatus::TargetTooFar
        | TradeStatus::WrongFaction
        | TradeStatus::IgnoreYou
        | TradeStatus::YouStunned
        | TradeStatus::TargetStunned
        | TradeStatus::YouDead
        | TradeStatus::TargetDead
        | TradeStatus::YouLogout
        | TradeStatus::TargetLogout
        | TradeStatus::TrialAccount => trade.close(),
        // "You can only trade conjured items" bounces the placement, not the window; UNKNOWN_13 and
        // any out-of-range code are inert.
        TradeStatus::OnlyConjured { .. } | TradeStatus::Unknown13 | TradeStatus::Unknown(_) => {}
    }
}

/// `SessionEvent::TradeStatusExtended` (`SMSG_TRADE_STATUS_EXTENDED`) — replace one side's item/gold
/// snapshot (decision 0592 P1); the feed repaints (`TRADE_UPDATE`) on the change.
pub(super) fn trade_status_extended(ext: &TradeStatusExtended, trade: &mut TradeSession) {
    bevy::log::info!(
        target: "trade",
        "SMSG_TRADE_STATUS_EXTENDED their_window={} gold={} items={}",
        ext.their_window,
        ext.gold,
        ext.slots.iter().filter(|s| s.is_some()).count(),
    );
    trade.set_offer(ext);
}
