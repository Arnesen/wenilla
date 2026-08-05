//! The app-side **trade feed** (decision 0592 P1) — the inward half of the trade seam around
//! [`benilla_ui::script`]'s `trade` module, the two-sided twin of [`crate::ui_mail`]'s mailbox seam.
//!
//! Trade is entirely server-driven: a right-click → **Trade** ([`crate::ui_trade`] via the UnitPopup
//! row) sends `CMSG_INITIATE_TRADE`; the net bridge ([`crate::net::apply::trade`]) then folds the two
//! status packets into [`TradeSession`] — `SMSG_TRADE_STATUS` drives the open/accept/close state
//! machine, `SMSG_TRADE_STATUS_EXTENDED` replaces one side's item/gold snapshot. Each frame
//! [`feed_trade`] resolves both sides' wire [`TradeItem`]s to Lua-facing
//! [`benilla_ui::script::TradeSlotItem`]s (name/quality via the ask-once item-template cache, icon via
//! the wire `display_id` + `ItemDisplayInfo.dbc`) and the partner's name (via the name cache), pushes
//! the snapshot ([`benilla_ui::script::UiScript::set_trade`]), and fires the events the reference Lua
//! drives — `TRADE_SHOW` on open, `TRADE_UPDATE` on any offer change, `TRADE_CLOSED` on close, and
//! `TRADE_ACCEPT_UPDATE(myState, theirState)` on an accept-glow change. [`drain_trade`] pulls the Lua
//! intents (`InitiateTrade`/`AcceptTrade`/`CancelTradeAccept`/`CloseTrade`) back out into the trade
//! `CMSG`s.
//!
//! **The partner's portrait** rides the shared `"npc"` booth token: [`TradeSession`] implements
//! [`NpcSession`] so [`crate::ui_session::feed_interact_npc`] points the `"npc"` portrait at the
//! partner's live entity while the window is open — the partner is a nearby player (within
//! `TRADE_DISTANCE`, always streamed), so the booth bakes their dressed look exactly as it bakes a
//! vendor's. Trade is deliberately **excluded** from the range-guard auto-close
//! ([`crate::ui_session::close_npc_session_out_of_range`]): unlike the NPC windows (a no-packet local
//! clear), a trade's out-of-range cancel is server-driven (vmangos sends `CANCELED` past
//! `TRADE_DISTANCE`), and the walk-away/death/disconnect unwinding is decision 0592 P3.
//!
//! **Scope (decision 0592 P1).** The window opens, mirrors both sides' items + gold live, and drives
//! the accept glow (the accept verb is a wire pass-through: click Trade → `CMSG_ACCEPT_TRADE`, and a
//! 200 ms scam-delay bounce comes back as `BACK_TO_TRADE` and drops the glow honestly). **Setting**
//! items/gold onto the window — the drag-drop slots + the editable money widget — is decision 0592 P2.

use benilla_protocol::messages::{TradeItem, TradeStatusExtended, TRADE_SLOT_COUNT};
use bevy::prelude::*;

use benilla_ui::script::{ScriptValue, TradeSideState, TradeSlotItem, TradeState, UiScript};

use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::names::NameCache;
use crate::net::{ClientCommand, NetCommands, ObjectStore, SelfPlayer};
use crate::target::Selection;
use crate::ui_party::GroupState;
use crate::ui_script::UiInput;
use crate::ui_session::NpcSession;

/// One side's offer as the wire delivered it — the seven slots (index 0 = trade slot 1 … index 6 =
/// the non-traded / enchant slot) plus the gold and the enchant-slot spell. Filled from
/// `SMSG_TRADE_STATUS_EXTENDED`; the feed resolves the [`TradeItem`]s to display rows.
#[derive(Default)]
struct TradeOffer {
    slots: [Option<TradeItem>; TRADE_SLOT_COUNT],
    gold: u32,
    /// The spell applied to this side's enchant slot (`0` = none) — decision 0592 P3.
    enchant_spell_id: u32,
}

/// The open trade window's session, filled by the net bridge ([`crate::net::apply::trade`]) and read
/// by [`feed_trade`]. Holds the partner guid, the open flag, both sides' wire offers, and the two
/// accept flags. Cleared on cancel/complete/close and on disconnect.
///
/// `partner` is set the moment the trade begins (the initiator records its target at
/// `CMSG_INITIATE_TRADE` time, since the later `OPEN_WINDOW` carries no guid; the target records it
/// from `BEGIN_TRADE`); `open` flips on `OPEN_WINDOW`. `npc()` is gated on `open` so the `"npc"`
/// portrait bakes only while the window is up.
#[derive(Resource, Default)]
pub(crate) struct TradeSession {
    /// The trade partner's guid; `Some` from the initiate/begin onward, `None` = no trade.
    partner: Option<u64>,
    /// The window is up (`OPEN_WINDOW` arrived). `partner` can be `Some` before this (an initiate in
    /// flight the server hasn't opened yet).
    open: bool,
    /// Our own offer (the wire's `their_window == false` snapshot).
    our: TradeOffer,
    /// The partner's offer (the wire's `their_window == true` snapshot).
    their: TradeOffer,
    /// We pressed Trade (our accept glow) — client-tracked, optimistic like the reference's
    /// button-disable; a `BACK_TO_TRADE`/offer-change/close drops it.
    our_accept: bool,
    /// The partner pressed Trade (their accept glow) — set by `TRADE_STATUS_TRADE_ACCEPT`.
    their_accept: bool,
}

impl TradeSession {
    /// The initiator records its target the moment it sends `CMSG_INITIATE_TRADE` — `OPEN_WINDOW`
    /// carries no guid, so this is where the initiator's side learns the partner. Resets any stale
    /// prior offer.
    pub(crate) fn initiate(&mut self, target: u64) {
        *self = TradeSession {
            partner: Some(target),
            ..Default::default()
        };
    }

    /// `BEGIN_TRADE(initiator guid)` — the target learns the partner and (via the drain-adjacent
    /// bridge) auto-answers `CMSG_BEGIN_TRADE`. Resets any stale prior offer.
    pub(crate) fn begin(&mut self, partner: u64) {
        *self = TradeSession {
            partner: Some(partner),
            ..Default::default()
        };
    }

    /// `OPEN_WINDOW` — both windows go up; the feed's open edge fires `TRADE_SHOW`.
    pub(crate) fn open_window(&mut self) {
        self.open = true;
    }

    /// A fresh `SMSG_TRADE_STATUS_EXTENDED` for one side — replace that side's offer and reset **both**
    /// accepts (any offer change invalidates prior accepts, the server's own rule; on open the initial
    /// snapshot's reset is a no-op since accepts are already clear).
    pub(crate) fn set_offer(&mut self, ext: &TradeStatusExtended) {
        let side = if ext.their_window {
            &mut self.their
        } else {
            &mut self.our
        };
        side.slots = ext.slots;
        side.gold = ext.gold;
        side.enchant_spell_id = ext.enchant_spell_id;
        self.our_accept = false;
        self.their_accept = false;
    }

    /// Place our own item into trade slot `id_1based` **optimistically** (decision 0592 P2): vmangos
    /// echoes a `SET_TRADE_ITEM` only to the *partner* (`TradeData::SetItem` → `Update(true)`), never
    /// to the placer, so our own column is client-side — the drain resolves the bag item and fills
    /// `our` here, which the feed then paints exactly like the partner's server-driven side. Changing
    /// our offer resets both accepts, mirroring the server's `SetAccepted(false)` on both.
    pub(crate) fn place_own_item(&mut self, id_1based: u32, item: TradeItem) {
        if let Some(slot) = id_1based
            .checked_sub(1)
            .and_then(|i| self.our.slots.get_mut(i as usize))
        {
            *slot = Some(item);
            self.our_offer_changed();
        }
    }

    /// Clear our own trade slot `id_1based` optimistically (the twin of [`Self::place_own_item`]).
    pub(crate) fn clear_own_item(&mut self, id_1based: u32) {
        if let Some(slot) = id_1based
            .checked_sub(1)
            .and_then(|i| self.our.slots.get_mut(i as usize))
        {
            *slot = None;
            self.our_offer_changed();
        }
    }

    /// Set our own offered gold optimistically (the money input's value — the same client-side reason
    /// the items are; `SetMoney` also only echoes to the partner).
    pub(crate) fn set_own_gold(&mut self, copper: u32) {
        self.our.gold = copper;
        self.our_offer_changed();
    }

    /// Any change to our own offer resets both accepts, exactly as vmangos does server-side.
    fn our_offer_changed(&mut self) {
        self.our_accept = false;
        self.their_accept = false;
    }

    /// `TRADE_STATUS_TRADE_ACCEPT` — the partner pressed Trade.
    pub(crate) fn partner_accepted(&mut self) {
        self.their_accept = true;
    }

    /// `TRADE_STATUS_BACK_TO_TRADE` — an accept was withdrawn (a change, the partner's un-accept, or
    /// the 200 ms scam-delay bounce): drop both accepts.
    pub(crate) fn back_to_trade(&mut self) {
        self.our_accept = false;
        self.their_accept = false;
    }

    /// We pressed Trade (the drained `AcceptTrade` intent) — optimistic local accept beside the
    /// `CMSG_ACCEPT_TRADE` send.
    pub(crate) fn accept(&mut self) {
        self.our_accept = true;
    }

    /// We dropped our accept (`CancelTradeAccept`) — local twin of the `CMSG_UNACCEPT_TRADE` send.
    pub(crate) fn unaccept(&mut self) {
        self.our_accept = false;
    }

    /// Close the trade — `CANCELED` / `COMPLETE` / `CLOSE_WINDOW` / a refusal, or the local close
    /// verb. Clears everything.
    pub(crate) fn close(&mut self) {
        *self = TradeSession::default();
    }

    /// Disconnect: drop the open window (mirrors the merchant/mail session clears).
    pub(crate) fn clear_session(&mut self) {
        *self = TradeSession::default();
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }
}

/// Trade points the shared `"npc"` portrait booth at the partner while the window is open (the
/// partner is a live nearby player entity — see the module doc). It is **not** registered with the
/// range-guard auto-close: a trade's cancel is server-driven, not a local clear.
impl NpcSession for TradeSession {
    fn npc(&self) -> Option<u64> {
        self.open.then_some(self.partner).flatten()
    }

    fn close(&mut self) {
        self.close();
    }
}

pub(crate) struct UiTradePlugin;

impl Plugin for UiTradePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<TradeSession>().add_systems(
            Update,
            (
                // Feed before the input pass so an open/close/mirror is on screen the same frame;
                // drain after it so a click's intent (initiate, accept, close) goes out the same
                // frame (the ui_mail ordering exactly). After the UnitFeed set so the resolved
                // item-template store is landed. No range-guard registration — a trade's cancel is
                // server-driven (the module doc).
                feed_trade.after(crate::ui_unit::UnitFeed).before(UiInput),
                drain_trade.after(UiInput),
            ),
        );
    }
}

/// Resolve one wire [`TradeItem`] into the Lua-facing [`TradeSlotItem`]: name/quality through the
/// ask-once item-template cache, icon through the wire `display_id` + `ItemDisplayInfo.dbc`. `None`s
/// stay `None` while a template query is in flight — the slot shows the icon (from the wire) and fills
/// in the name/quality when the answer lands (the mail/merchant pattern). The enchant-slot spell name
/// is decision 0592 P3.
fn resolve_slot(
    item: &TradeItem,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
) -> TradeSlotItem {
    let template = items.template(item.entry, 0, commands);
    let name = template.map(|t| t.name.clone());
    let quality = template.map(|t| t.quality);
    let texture = icons
        .and_then(|i| i.catalog.get(item.display_id))
        .and_then(|d| d.icon.clone());
    TradeSlotItem {
        item_id: item.entry,
        name,
        texture,
        count: item.count.max(1),
        quality,
        enchantment: None,
    }
}

/// Resolve the bag item at a cursor-space `(bag, 1-based slot)` into a wire [`TradeItem`] for our own
/// **optimistic** display (decision 0592 P2): entry from the descriptor, display id from the ask-once
/// item template (cached already — the item was showing in the bag), count from the slot. `None` for an
/// empty or not-yet-streamed slot. The item stays in the bag until the trade completes; this is only
/// what WE show in our own column, since vmangos never echoes a placement back to the placer.
fn own_item_at(
    bag: i64,
    slot: u32,
    store: Option<&ObjectStore>,
    items: &mut Items,
    commands: &NetCommands,
) -> Option<TradeItem> {
    let (guid, count) = crate::ui_items::slot_guid_count(store, bag, slot, items);
    if guid == 0 {
        return None;
    }
    let entry = items.object(guid).and_then(|f| f.object_entry())?;
    let display_id = items
        .template(entry, guid, commands)
        .map(|t| t.display_info_id)
        .unwrap_or(0);
    Some(TradeItem {
        entry,
        display_id,
        count,
        wrapped: false,
        gift_creator: 0,
        perm_enchant: 0,
        creator: 0,
        charges: 0,
        suffix_factor: 0,
        random_prop_id: 0,
        lock_id: 0,
        max_durability: 0,
        durability: 0,
    })
}

/// Resolve one side's wire offer into the Lua-facing side state.
fn resolve_side(
    offer: &TradeOffer,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
) -> TradeSideState {
    let mut slots: [Option<TradeSlotItem>; TRADE_SLOT_COUNT] = Default::default();
    for (i, s) in offer.slots.iter().enumerate() {
        slots[i] = s.map(|it| resolve_slot(&it, items, icons, commands));
    }
    TradeSideState {
        slots,
        gold: offer.gold,
    }
}

/// Build the Lua-facing snapshot from [`TradeSession`] — `None` when no trade window is open.
fn snapshot(
    trade: &TradeSession,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    names: &mut NameCache,
    commands: &NetCommands,
) -> Option<TradeState> {
    if !trade.open {
        return None;
    }
    Some(TradeState {
        player: resolve_side(&trade.our, items, icons, commands),
        target: resolve_side(&trade.their, items, icons, commands),
        partner_name: trade
            .partner
            .and_then(|g| names.resolve(g, commands).map(str::to_string)),
    })
}

/// Push the current trade into the VM and fire the show/update/close + accept-glow events on a
/// transition or content change (an async item/name landing, a fresh offer, an accept flip). Diffed
/// against `Local` memory, exactly like the mail/merchant feeds. The accept-update fires **after** the
/// show/update block so an open frame's `TRADE_SHOW` (which hides the highlights) is followed by the
/// glow, not overwritten by it.
#[allow(clippy::too_many_arguments)]
fn feed_trade(
    script: Option<NonSendMut<UiScript>>,
    trade: Res<TradeSession>,
    mut items: ResMut<Items>,
    icons: Option<Res<ItemDisplays>>,
    mut names: ResMut<NameCache>,
    commands: Res<NetCommands>,
    mut last: Local<Option<TradeState>>,
    mut last_open: Local<bool>,
    mut last_accept: Local<(bool, bool)>,
    mut last_player_gold: Local<u32>,
) {
    let Some(mut script) = script else {
        return;
    };

    let fresh = snapshot(&trade, &mut items, icons.as_deref(), &mut names, &commands);
    let opened = !*last_open && trade.is_open();
    let closed = *last_open && !trade.is_open();
    let changed = fresh != *last;
    if changed {
        script.set_trade(fresh.clone());
    }
    if opened {
        script.fire_event("TRADE_SHOW", vec![]);
    } else if closed {
        script.fire_event("TRADE_CLOSED", vec![]);
    } else if trade.is_open() && changed {
        script.fire_event("TRADE_UPDATE", vec![]);
    }

    // The accept glow: fire on any change while open (the reference `TRADE_ACCEPT_UPDATE(my, his)`).
    let accept = (trade.our_accept, trade.their_accept);
    if trade.is_open() && accept != *last_accept {
        script.fire_event(
            "TRADE_ACCEPT_UPDATE",
            vec![
                ScriptValue::Int(i64::from(accept.0)),
                ScriptValue::Int(i64::from(accept.1)),
            ],
        );
    }
    if closed {
        *last_accept = (false, false);
    } else {
        *last_accept = accept;
    }

    // Our accepted gold echoed to the money input via a dedicated PLAYER_TRADE_MONEY (the reference's
    // own event, ref TradePlayerInputMoneyFrame l.565) — kept off the TRADE_UPDATE repaint so it never
    // overwrites what the player is mid-typing; the input's diff-guarded SetCopper reflects the value.
    let player_gold = trade.our.gold;
    if trade.is_open() && player_gold != *last_player_gold {
        script.fire_event("PLAYER_TRADE_MONEY", vec![]);
    }
    *last_player_gold = if trade.is_open() { player_gold } else { 0 };

    *last = fresh;
    *last_open = trade.is_open();
}

/// Drain the Lua intents into the trade `CMSG`s (decision 0592 P1): `InitiateTrade(unit)` → resolve
/// the token → player guid → `CMSG_INITIATE_TRADE` (recording the target so `OPEN_WINDOW` can name
/// it); `AcceptTrade`/`CancelTradeAccept` → the accept/un-accept verbs (with the optimistic local
/// glow); `CloseTrade` → `CMSG_CANCEL_TRADE` + a local clear.
#[allow(clippy::too_many_arguments)]
fn drain_trade(
    script: Option<NonSendMut<UiScript>>,
    mut trade: ResMut<TradeSession>,
    commands: Res<NetCommands>,
    selection: Res<Selection>,
    group: Res<GroupState>,
    mut items: ResMut<Items>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
) {
    let Some(mut script) = script else {
        return;
    };
    // The self player's descriptor store (our bag contents) — for the optimistic own-item resolve.
    let store = self_q.iter().next();

    for token in script.take_trade_initiates() {
        match resolve_trade_target(&token, &selection, &group) {
            Some(guid) => {
                info!(target: "trade", "initiate: {token:?} -> {guid:#x}; sending CMSG_INITIATE_TRADE");
                trade.initiate(guid);
                let _ = commands
                    .0
                    .send(ClientCommand::InitiateTrade { target: guid });
            }
            None => {
                info!(target: "trade", "initiate: {token:?} did not resolve to a player guid — nothing sent");
            }
        }
    }

    // The accept verbs only act on an open window (a stray click otherwise is dropped); close is
    // idempotent (honored even with no session, to clear a stale one).
    if script.take_trade_accept() && trade.is_open() {
        trade.accept();
        let _ = commands.0.send(ClientCommand::AcceptTrade);
    }
    if script.take_trade_unaccept() && trade.is_open() {
        trade.unaccept();
        let _ = commands.0.send(ClientCommand::UnacceptTrade);
    }
    // A money offer (the input's value-changed callback) → CMSG_SET_TRADE_GOLD, only while open; the
    // server echoes our new gold back, which the feed turns into PLAYER_TRADE_MONEY (decision 0592 P2).
    if let Some(copper) = script.take_trade_money() {
        if trade.is_open() {
            trade.set_own_gold(copper); // optimistic — the placer gets no server echo of its own gold
            info!(target: "trade", "set gold: {copper} copper; sending CMSG_SET_TRADE_GOLD");
            let _ = commands.0.send(ClientCommand::SetTradeGold { copper });
        }
    }
    // Item placements: a bag item dropped onto our slot → CMSG_SET_TRADE_ITEM (the cursor's engine-space
    // bag/slot resolved to the wire position); an empty-cursor clear → CMSG_CLEAR_TRADE_ITEM. The UI id
    // is 1-based, the wire slot 0-based. Only while a window is open (decision 0592 P2).
    for (id, bag, slot) in script.take_trade_set_items() {
        if !trade.is_open() {
            continue;
        }
        let Some((wire_bag, wire_slot)) = crate::ui_items::wire_pos(bag, slot) else {
            info!(target: "trade", "set item: slot {id} <- bag {bag}/{slot} has no wire position — dropped");
            continue;
        };
        if let Some(trade_slot) = id.checked_sub(1).and_then(|n| u8::try_from(n).ok()) {
            // Optimistic own display: vmangos echoes the placement only to the partner, so resolve the
            // bag item and fill our own column client-side (decision 0592 P2).
            if let Some(item) = own_item_at(bag, slot, store, &mut items, &commands) {
                trade.place_own_item(id, item);
            }
            info!(target: "trade", "set item: slot {id} <- bag {bag}/{slot} (wire {wire_bag}/{wire_slot}); sending CMSG_SET_TRADE_ITEM");
            let _ = commands.0.send(ClientCommand::SetTradeItem {
                trade_slot,
                bag: wire_bag,
                slot: wire_slot,
            });
        }
    }
    for id in script.take_trade_clear_items() {
        if !trade.is_open() {
            continue;
        }
        if let Some(trade_slot) = id.checked_sub(1).and_then(|n| u8::try_from(n).ok()) {
            trade.clear_own_item(id); // optimistic — un-reference our own slot client-side
            info!(target: "trade", "clear item: slot {id}; sending CMSG_CLEAR_TRADE_ITEM");
            let _ = commands
                .0
                .send(ClientCommand::ClearTradeItem { trade_slot });
        }
    }
    if script.take_trade_close() {
        if trade.is_open() || trade.partner.is_some() {
            let _ = commands.0.send(ClientCommand::CancelTrade);
        }
        trade.close();
    }
}

/// Resolve a UnitPopup unit token to the player guid to trade with (decision 0592 P1). The
/// resolution is not trade-specific — every UnitPopup verb against another player needs the same
/// token → player-guid step — so it lives in [`crate::ui_unit::player_token_guid`] and inspect
/// (decision 0631) shares it. Kept as a named local for this module's own tests.
fn resolve_trade_target(token: &str, selection: &Selection, group: &GroupState) -> Option<u64> {
    crate::ui_unit::player_token_guid(token, selection, group)
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_protocol::messages::{TradeStatus, TradeStatusExtended};

    fn wire_item(entry: u32) -> TradeItem {
        TradeItem {
            entry,
            display_id: 42,
            count: 3,
            wrapped: false,
            gift_creator: 0,
            perm_enchant: 0,
            creator: 0,
            charges: 0,
            suffix_factor: 0,
            random_prop_id: 0,
            lock_id: 0,
            max_durability: 0,
            durability: 0,
        }
    }

    #[test]
    fn open_close_state_machine() {
        let mut s = TradeSession::default();
        assert!(!s.is_open());

        // The initiator records the target before the window opens.
        s.initiate(0x1234);
        assert_eq!(s.partner, Some(0x1234));
        assert!(!s.is_open());
        // No portrait yet (window not open).
        assert_eq!(s.npc(), None);

        // OPEN_WINDOW → open; the "npc" portrait now points at the partner.
        s.open_window();
        assert!(s.is_open());
        assert_eq!(s.npc(), Some(0x1234));

        // A refusal / cancel closes and clears everything.
        s.close();
        assert!(!s.is_open());
        assert_eq!(s.partner, None);
        assert_eq!(s.npc(), None);
    }

    #[test]
    fn begin_records_partner_for_the_target_side() {
        let mut s = TradeSession::default();
        s.begin(0xABCD);
        assert_eq!(s.partner, Some(0xABCD));
        assert!(!s.is_open(), "BEGIN_TRADE does not itself open the window");
    }

    #[test]
    fn extended_fills_the_right_side_and_resets_accepts() {
        let mut s = TradeSession::default();
        s.begin(0x1);
        s.open_window();
        s.partner_accepted();
        assert!(s.their_accept);

        // Their side (their_window = true) fills `their`; the offer change resets the accept.
        let mut slots = [None; TRADE_SLOT_COUNT];
        slots[0] = Some(wire_item(2589));
        let ext = TradeStatusExtended {
            their_window: true,
            gold: 500,
            enchant_spell_id: 0,
            slots,
        };
        s.set_offer(&ext);
        assert!(s.their.slots[0].is_some());
        assert_eq!(s.their.gold, 500);
        assert_eq!(s.our.gold, 0);
        assert!(!s.their_accept, "an offer change resets accepts");

        // Our side (their_window = false) fills `our`.
        let our_ext = TradeStatusExtended {
            their_window: false,
            gold: 999,
            enchant_spell_id: 0,
            slots: [None; TRADE_SLOT_COUNT],
        };
        s.set_offer(&our_ext);
        assert_eq!(s.our.gold, 999);
        assert_eq!(s.their.gold, 500, "the other side is untouched");
    }

    #[test]
    fn accept_glow_tracks_both_sides() {
        let mut s = TradeSession::default();
        s.begin(0x1);
        s.open_window();
        s.accept();
        s.partner_accepted();
        assert_eq!((s.our_accept, s.their_accept), (true, true));
        // BACK_TO_TRADE drops both.
        s.back_to_trade();
        assert_eq!((s.our_accept, s.their_accept), (false, false));
    }

    /// Our own offer is tracked client-side (decision 0592 P2): vmangos never echoes a placement to the
    /// placer, so `place_own_item`/`clear_own_item`/`set_own_gold` fill `our` directly, and each resets
    /// both accepts exactly as the server does — the feed then paints `our` as the player column.
    #[test]
    fn own_offer_is_tracked_optimistically() {
        let mut s = TradeSession::default();
        s.begin(0x7);
        s.open_window();
        s.accept();
        s.partner_accepted();
        assert_eq!((s.our_accept, s.their_accept), (true, true));

        s.place_own_item(2, wire_item(1234));
        assert!(s.our.slots[1].is_some(), "our slot 2 shows the placed item");
        assert_eq!(
            (s.our_accept, s.their_accept),
            (false, false),
            "changing our offer resets both accepts"
        );

        s.set_own_gold(500);
        assert_eq!(s.our.gold, 500);

        s.clear_own_item(2);
        assert!(s.our.slots[1].is_none(), "clearing empties the slot");
    }

    #[test]
    fn resolve_target_reads_the_selection_and_roster() {
        // "player" (yourself) never resolves.
        let sel = Selection::default();
        let group = GroupState::default();
        assert_eq!(resolve_trade_target("player", &sel, &group), None);
        // An out-of-range party token resolves to nothing (no roster).
        assert_eq!(resolve_trade_target("party3", &sel, &group), None);
    }

    // A status-code smoke check so the enum stays wired the way the apply arm reads it.
    #[test]
    fn status_codes_are_stable() {
        assert_eq!(TradeStatus::OpenWindow.code(), 2);
        assert_eq!(TradeStatus::Accept.code(), 4);
        assert_eq!(TradeStatus::BackToTrade.code(), 7);
    }

    /// The `drain_trade` **system** end-to-end (decision 0592 P1): a queued `InitiateTrade("target")`
    /// intent plus a player [`Selection`] resolves to the target guid, records the partner, and ships
    /// `CMSG_INITIATE_TRADE`. The isolated pieces (`resolve_trade_target`, the Lua queue) are tested
    /// apart; this is the only test that runs the drain as a real Bevy system through the VM +
    /// `Selection` + `NetCommands` — the exact seam behind the director's "click Trade, nothing
    /// happens" report.
    #[test]
    fn drain_initiate_resolves_the_selection_and_ships_the_cmsg() {
        use bevy::ecs::system::RunSystemOnce;

        let (tx, rx) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.init_resource::<TradeSession>()
            .init_resource::<GroupState>()
            .init_resource::<Items>()
            .insert_resource(NetCommands(tx))
            .insert_resource(Selection {
                target: None,
                guid: Some(0x7), // a player guid (high 0x0000 → is_player)
            });
        app.insert_non_send_resource(UiScript::new().unwrap());

        // The right-click menu queued this Lua-side (the menu hit-path test proves the click gets
        // this far); the drain is the untested downstream half.
        app.world_mut()
            .non_send_resource_mut::<UiScript>()
            .run("InitiateTrade('target')")
            .unwrap();

        app.world_mut().run_system_once(drain_trade).unwrap();

        assert!(
            matches!(
                rx.try_recv(),
                Ok(ClientCommand::InitiateTrade { target: 0x7 })
            ),
            "the drain ships CMSG_INITIATE_TRADE against the resolved target guid"
        );
        assert_eq!(
            app.world().resource::<TradeSession>().partner,
            Some(0x7),
            "the initiator records the partner so OPEN_WINDOW can name it"
        );
    }

    /// Draining a `SetTradeMoney` offer ships `CMSG_SET_TRADE_GOLD` — but only while the window is
    /// open, so a stray value with no trade is dropped (decision 0592 P2).
    #[test]
    fn drain_money_ships_set_trade_gold_only_when_open() {
        use bevy::ecs::system::RunSystemOnce;

        let (tx, rx) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.init_resource::<TradeSession>()
            .init_resource::<GroupState>()
            .init_resource::<Items>()
            .insert_resource(NetCommands(tx))
            .insert_resource(Selection::default());
        app.insert_non_send_resource(UiScript::new().unwrap());

        // No window yet → the offer is dropped.
        app.world_mut()
            .non_send_resource_mut::<UiScript>()
            .run("SetTradeMoney(500)")
            .unwrap();
        app.world_mut().run_system_once(drain_trade).unwrap();
        assert!(rx.try_recv().is_err(), "no CMSG with no open trade");

        // Open the window, then a money offer ships the gold CMSG.
        {
            let mut trade = app.world_mut().resource_mut::<TradeSession>();
            trade.begin(0x7);
            trade.open_window();
        }
        app.world_mut()
            .non_send_resource_mut::<UiScript>()
            .run("SetTradeMoney(12345)")
            .unwrap();
        app.world_mut().run_system_once(drain_trade).unwrap();
        assert!(
            matches!(
                rx.try_recv(),
                Ok(ClientCommand::SetTradeGold { copper: 12_345 })
            ),
            "an open-window money offer ships CMSG_SET_TRADE_GOLD"
        );
    }

    /// Draining an item CLEAR (an empty-cursor click on our filled slot) ships `CMSG_CLEAR_TRADE_ITEM`
    /// with the 1-based UI slot mapped to the 0-based wire slot (decision 0592 P2). The SET path's
    /// wire-position mapping rides `ui_items::wire_pos` (tested there) over the `(id, bag, slot)` intent
    /// the script test proves; here we drive the cursor-free clear end to end.
    #[test]
    fn drain_item_clear_ships_clear_trade_item_zero_based() {
        use bevy::ecs::system::RunSystemOnce;

        let (tx, rx) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.init_resource::<TradeSession>()
            .init_resource::<GroupState>()
            .init_resource::<Items>()
            .insert_resource(NetCommands(tx))
            .insert_resource(Selection::default());
        app.insert_non_send_resource(UiScript::new().unwrap());
        {
            let mut trade = app.world_mut().resource_mut::<TradeSession>();
            trade.begin(0x7);
            trade.open_window();
        }

        // A pushed trade state with our slot 1 filled, then an empty-cursor click clears it.
        {
            let mut vm = app.world_mut().non_send_resource_mut::<UiScript>();
            let mut st = benilla_ui::script::TradeState::default();
            st.player.slots[0] = Some(benilla_ui::script::TradeSlotItem {
                item_id: 2589,
                count: 1,
                ..Default::default()
            });
            vm.set_trade(Some(st));
            vm.run("ClickTradeButton(1)").unwrap();
        }
        app.world_mut().run_system_once(drain_trade).unwrap();
        assert!(
            matches!(
                rx.try_recv(),
                Ok(ClientCommand::ClearTradeItem { trade_slot: 0 })
            ),
            "clearing our filled UI slot 1 ships CMSG_CLEAR_TRADE_ITEM for wire slot 0"
        );
    }
}
