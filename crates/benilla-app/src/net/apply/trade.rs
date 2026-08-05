//! The player-trade arc's drain-side arm bodies for [`super::apply_net_updates`]'s dispatch match
//! (decision 0592 P1). Each `pub(super)` fn here is one `SessionEvent` arm's body, driving the
//! [`crate::ui_trade::TradeSession`] state machine the feed reads; the match at the call site stays
//! the dispatcher. The `UiScript` events these ultimately drive are fired by
//! [`crate::ui_trade::feed_trade`] (the feed owns the VM), so these arms only mutate the session +
//! send the auto-reply.

use benilla_protocol::messages::{TradeStatus, TradeStatusExtended};

use crate::net::{ClientCommand, NetCommands};
use crate::ui_trade::TradeSession;

/// `SessionEvent::TradeStatus` (`SMSG_TRADE_STATUS`) — the trade state machine (decision 0592 P1).
/// `BEGIN_TRADE` records the partner and auto-answers `CMSG_BEGIN_TRADE` (the reference client's
/// auto-reply, vmangos `TradeHandler.cpp` — it makes the server emit `OPEN_WINDOW` to both sides);
/// `OPEN_WINDOW` opens the window; `TRADE_ACCEPT`/`BACK_TO_TRADE` drive the accept glow; the terminal
/// and refusal codes close it. The refusal statuses' red error text is decision 0592 P3;
/// `ONLY_CONJURED` (a rejected item placement, not a window close) and any out-of-range code are inert.
pub(super) fn trade_status(status: TradeStatus, trade: &mut TradeSession, commands: &NetCommands) {
    bevy::log::info!(target: "trade", "SMSG_TRADE_STATUS {status:?}");
    match status {
        TradeStatus::BeginTrade { partner } => {
            trade.begin(partner);
            let _ = commands.0.send(ClientCommand::BeginTrade);
        }
        TradeStatus::OpenWindow => trade.open_window(),
        TradeStatus::Accept => trade.partner_accepted(),
        TradeStatus::BackToTrade => trade.back_to_trade(),
        // Terminal (complete/cancel/close) + every initiate/in-trade refusal → close the window.
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
