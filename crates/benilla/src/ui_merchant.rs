//! The app-side **merchant feed** (decision 0081 phase 4) — the inward half of the merchant seam
//! around [`benilla_ui::script`]'s `merchant` module, the twin of [`crate::ui_gossip`]'s gossip seam
//! and [`crate::ui_items`]'s container seam.
//!
//! The net bridge fills [`MerchantOpen`] from the wire (`SMSG_LIST_INVENTORY` → the vendor's rows;
//! `SMSG_BUY_ITEM` → a stock update; the two refusals → [`MerchantErrors`]). Each frame
//! [`feed_merchant`] resolves each wire [`VendorItem`] to a Lua-facing [`MerchantItem`] (name via the
//! ask-once item-template cache, icon straight from the wire `display_id` through
//! `ItemDisplayInfo.dbc` — the same catalog the bags use), pushes the snapshot
//! ([`benilla_ui::script::UiScript::set_merchant`]), and fires `MERCHANT_SHOW` on open /
//! `MERCHANT_UPDATE` on a content change / `MERCHANT_CLOSED` on clear. It also pushes the player's
//! purse (`PLAYER_FIELD_COINAGE` via `set_money`) each frame it changes, firing `PLAYER_MONEY` with
//! it (the money displays repaint on the event). [`drain_merchant`] pulls the Lua intents back out:
//! `BuyMerchantItem(index)` → [`ClientCommand::BuyItem`] (mapped from the 1-based row to the item
//! *entry* the wire addresses — buy is by entry, not the vendor `muid`), and `CloseMerchant` → a
//! local clear (vanilla's client-side close sends no packet). The standardized NPC-session range
//! guard ([`crate::ui_session`]) applies the same client-side close when the player walks out of
//! the NPC-service range (or the vendor despawns).
//!
//! The sell affordance (decision 0081 v1) lives one module over in [`crate::ui_items`]: while a
//! merchant is open, a bag-slot click sells the slot's item instead of using/equipping it.

use benilla_protocol::messages::{buy_result, sell_result, VendorItem};
use bevy::prelude::*;

use benilla_ui::script::{ItemStatsHead, MerchantItem, MerchantState, ScriptValue, UiScript};

use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::names::NameCache;
use crate::net::{ClientCommand, NetCommands, ObjectStore, SelfPlayer};
use crate::ui_script::UiInput;
use crate::ui_session::{close_npc_session_out_of_range, npc_switched, NpcSession};

/// The wire's "unlimited stock" sentinel for a vendor row's `current_count` (vmangos
/// `ItemHandler.cpp`); the Lua side sees it as `numAvailable == -1`.
const STOCK_UNLIMITED: u32 = 0xFFFF_FFFF;

/// `UNIT_NPC_FLAG_REPAIR` — the vendor-can-repair service bit `CanMerchantRepair` tests (wow-re
/// repair-machinery.md `0x4fadb0`: a merchant-frame gate, deliberately never in the cursor ladder).
const NPC_FLAG_REPAIR: u32 = 0x4000;

/// The wire's first absolute buyback inventory slot (`BUYBACK_SLOT_START`; slots 69–80).
const BUYBACK_SLOT_FIRST: u32 = 69;

/// The client-side repair-cost tables (`DurabilityCosts.dbc` + `DurabilityQuality.dbc`), loaded
/// with the entity catalogs ([`crate::entities`]). Optional resource — absent, every repair cost
/// displays 0 and the repair-all button stays disabled (the wire still works).
#[derive(Resource)]
pub(crate) struct RepairTables(pub(crate) benilla_formats::DurabilityTables);

/// The open merchant, filled by the net bridge ([`crate::net`]) and read by [`feed_merchant`]. Holds
/// the vendor guid and its rows exactly as the wire delivered them (`SMSG_LIST_INVENTORY`); the feed
/// resolves each to a display row and the drain maps a clicked 1-based row to its item entry. Cleared
/// on a client-side close and on disconnect.
#[derive(Resource, Default)]
pub(crate) struct MerchantOpen {
    /// The vendor whose window is open; `None` = no vendor open.
    pub(crate) vendor: Option<u64>,
    /// The vendor's rows (wire order = 1-based display order).
    pub(crate) items: Vec<VendorItem>,
}

impl MerchantOpen {
    /// Open (or replace) the window with a vendor's freshly-listed stock.
    pub(crate) fn open(&mut self, vendor: u64, items: Vec<VendorItem>) {
        self.vendor = Some(vendor);
        self.items = items;
    }

    /// Whether a vendor window is currently open.
    pub(crate) fn is_open(&self) -> bool {
        self.vendor.is_some()
    }

    /// The item **entry** at a 1-based display row — what `CMSG_BUY_ITEM` addresses.
    pub(crate) fn entry_at(&self, index_1based: u32) -> Option<u32> {
        index_1based
            .checked_sub(1)
            .and_then(|i| self.items.get(i as usize))
            .map(|it| it.entry)
    }

    /// Apply a post-purchase stock update (`SMSG_BUY_ITEM`): the row's remaining count changes. The
    /// wire keys it by the 1-based vendor `slot`; the purchased item itself lands via the normal
    /// item-create path (already handled), so only the count display moves here.
    pub(crate) fn update_stock(&mut self, slot: u32, new_count: u32) {
        if let Some(it) = self.items.iter_mut().find(|it| it.slot == slot) {
            it.current_count = new_count;
        }
    }

    /// Close the open window (a client-side close). Keeps nothing — a re-open re-lists.
    pub(crate) fn clear(&mut self) {
        self.vendor = None;
        self.items.clear();
    }

    /// Disconnect: drop the open window (mirrors the gossip/item session clears).
    pub(crate) fn clear_session(&mut self) {
        self.clear();
    }
}

/// A merchant refusal (`SMSG_BUY_FAILED` / `SMSG_SELL_ITEM`'s error path) queued by the net bridge for
/// the UI error line — the merchant twin of [`crate::ui_items::EquipErrors`]. A *successful* buy/sell
/// never lands here (a sell is silent, a buy answers `SMSG_BUY_ITEM`); only the error path does.
#[derive(Resource, Default)]
pub(crate) struct MerchantErrors(pub Vec<MerchantRefusal>);

/// One refusal: a buy or a sell, carrying the wire's `u8` reason code.
pub(crate) enum MerchantRefusal {
    Buy(u8),
    Sell(u8),
}

pub(crate) struct UiMerchantPlugin;

impl Plugin for UiMerchantPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MerchantOpen>()
            .init_resource::<MerchantErrors>()
            .add_systems(
                Update,
                (
                    // Range-close before the feed so the clear turns into MERCHANT_CLOSED the same
                    // frame; push before the input pass so an open/close is on screen the same
                    // frame; drain after it so a buy's intent goes out the same frame (mirrors
                    // ui_gossip/ui_items). After the UnitFeed set: the paint MERCHANT_SHOW
                    // triggers reads the engine's item-template + player-req stores
                    // (GetMerchantItemInfo's isUsable), so this frame's pushes must land first —
                    // unordered, the first paint races feed_item_stats/feed_player_req and the
                    // usable reds drop until the next content repaint.
                    close_npc_session_out_of_range::<MerchantOpen>.before(feed_merchant),
                    feed_merchant
                        .after(crate::ui_unit::UnitFeed)
                        .before(UiInput),
                    drain_merchant.after(UiInput),
                ),
            );
    }
}

/// The client's message string for a `BuyResult` refusal (`SMSG_BUY_FAILED`); values from
/// [`buy_result`] (vmangos `ItemDefines.h`). Unknown reasons print their code.
fn buy_error_text(reason: u8) -> String {
    match reason {
        buy_result::CANT_FIND_ITEM => "That item does not exist.".into(),
        buy_result::ITEM_ALREADY_SOLD => "That item has already been sold.".into(),
        buy_result::NOT_ENOUGH_MONEY => "You don't have enough money.".into(),
        buy_result::SELLER_DONT_LIKE_YOU => "The vendor doesn't like you.".into(),
        buy_result::DISTANCE_TOO_FAR => "You are too far away.".into(),
        buy_result::ITEM_SOLD_OUT => "The vendor is sold out of that item.".into(),
        buy_result::CANT_CARRY_MORE => "You can't carry any more of those items.".into(),
        buy_result::RANK_REQUIRE => "You don't have the required rank for that item.".into(),
        buy_result::REPUTATION_REQUIRE => {
            "You don't have the required reputation for that item.".into()
        }
        other => format!("Purchase failed ({other})."),
    }
}

/// The client's message string for a `SellResult` refusal (`SMSG_SELL_ITEM`'s error path); values
/// from [`sell_result`] (vmangos `ItemDefines.h`).
fn sell_error_text(reason: u8) -> String {
    match reason {
        sell_result::CANT_FIND_ITEM => "That item does not exist.".into(),
        sell_result::CANT_SELL_ITEM => "You can't sell that item.".into(),
        sell_result::CANT_FIND_VENDOR => "That vendor can't be found.".into(),
        sell_result::YOU_DONT_OWN_THAT_ITEM => "You don't own that item.".into(),
        sell_result::ONLY_EMPTY_BAG => "You can only sell an empty bag.".into(),
        other => format!("Sale failed ({other})."),
    }
}

/// Resolve one wire [`VendorItem`] into the Lua-facing [`MerchantItem`]: the icon comes straight from
/// the wire `display_id` (no template wait — the row shows its icon immediately), the name + the
/// tooltip stat head from the ask-once template cache (`None` while in flight — the row shows a
/// placeholder and fills in when the answer lands, exactly like a bag slot).
fn resolve_item(
    item: &VendorItem,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
) -> MerchantItem {
    let template = items.template(item.entry, 0, commands);
    let name = template.map(|t| t.name.clone());
    let stats = template.map(|t| ItemStatsHead {
        quality: t.quality,
        inventory_type: t.inventory_type,
        class: t.class,
        subclass: t.subclass,
        dmg_min: t.dmg_min,
        dmg_max: t.dmg_max,
        dmg_type: t.dmg_type,
        delay_ms: t.delay_ms,
        armor: t.armor,
        block: t.block,
        sell_price: t.sell_price,
    });
    let texture = icons
        .and_then(|i| i.catalog.get(item.display_id))
        .and_then(|d| d.icon.clone());
    let num_available = if item.current_count == STOCK_UNLIMITED {
        -1
    } else {
        item.current_count as i32
    };
    MerchantItem {
        name,
        texture,
        price: item.price,
        quantity: item.buy_count,
        num_available,
        item_id: item.entry,
        stats,
    }
}

/// The occupied buyback slots' player-descriptor indices (0–11) in the client's display order —
/// timestamp-ascending, oldest first (wow-re `0x4fafd0`: scan slots 69–80 for a non-zero
/// guid+price pair, sort by the timestamp fields). Index `i` here is Lua's `GetBuybackItemInfo(i+1)`;
/// the wire's absolute slot for an entry is `BUYBACK_SLOT_FIRST + index`.
fn buyback_order(store: &benilla_protocol::ObjectFields) -> Vec<u8> {
    let mut v: Vec<(u8, u32)> = (0..12u8)
        .filter_map(|i| {
            let guid = store.player_buyback_slot(i).unwrap_or(0);
            let price = store.player_buyback_price(i).unwrap_or(0);
            (guid != 0 && price != 0).then(|| (i, store.player_buyback_timestamp(i).unwrap_or(0)))
        })
        .collect();
    v.sort_by_key(|&(_, ts)| ts);
    v.into_iter().map(|(i, _)| i).collect()
}

/// Resolve one buyback slot to its Lua-facing row: identity from the parked item object (it stays
/// streamed while in the buyback slots), name/stats/icon from its template, the price from the
/// player's BUYBACK_PRICE field.
fn resolve_buyback(
    idx: u8,
    store: &benilla_protocol::ObjectFields,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
) -> MerchantItem {
    let guid = store.player_buyback_slot(idx).unwrap_or(0);
    let obj = items.object(guid);
    let entry = obj.and_then(|o| o.object_entry()).unwrap_or(0);
    let quantity = obj.and_then(|o| o.item_stack_count()).unwrap_or(1).max(1);
    let template = items.template(entry, guid, commands);
    let name = template.map(|t| t.name.clone());
    let stats = template.map(|t| ItemStatsHead {
        quality: t.quality,
        inventory_type: t.inventory_type,
        class: t.class,
        subclass: t.subclass,
        dmg_min: t.dmg_min,
        dmg_max: t.dmg_max,
        dmg_type: t.dmg_type,
        delay_ms: t.delay_ms,
        armor: t.armor,
        block: t.block,
        sell_price: t.sell_price,
    });
    let display_id = template.map(|t| t.display_info_id).unwrap_or(0);
    let texture = icons
        .and_then(|i| i.catalog.get(display_id))
        .and_then(|d| d.icon.clone());
    MerchantItem {
        name,
        texture,
        price: store.player_buyback_price(idx).unwrap_or(0),
        quantity,
        num_available: 0,
        item_id: entry,
        stats,
    }
}

/// One item's displayed repair cost — durability off the item object, the head off its template,
/// the arithmetic in [`benilla_formats::DurabilityTables`] (the client's own `0x4faf30` chain).
/// No reputation model yet → discount 0.
fn item_repair_cost(
    guid: u64,
    items: &mut Items,
    tables: &RepairTables,
    commands: &NetCommands,
) -> u32 {
    let Some(obj) = items.object(guid) else {
        return 0;
    };
    let cur = obj.item_durability().unwrap_or(0);
    let max = obj.item_max_durability().unwrap_or(0);
    let entry = obj.object_entry().unwrap_or(0);
    if max == 0 || cur >= max || entry == 0 {
        return 0;
    }
    let points = max - cur;
    let Some(t) = items.template(entry, guid, commands) else {
        return 0;
    };
    let (level, quality, class, subclass) = (t.item_level, t.quality, t.class, t.subclass);
    tables
        .0
        .repair_cost(points, level, quality, class, subclass, 0.0)
}

/// The repair-all total: the client's three sweeps (wow-re repair-machinery.md `0x4fbd60`) —
/// equipped 0–18, the backpack, and the CONTENTS of the 4 equipped bags (never bank/keyring/
/// buyback).
fn repair_all_cost(
    store: &benilla_protocol::ObjectFields,
    items: &mut Items,
    tables: &RepairTables,
    commands: &NetCommands,
) -> u32 {
    let mut total: u64 = 0;
    let mut add = |guid: u64, items: &mut Items| {
        if guid != 0 {
            total += u64::from(item_repair_cost(guid, items, tables, commands));
        }
    };
    for i in 0..19u8 {
        add(store.player_inv_slot(i).unwrap_or(0), items);
    }
    for i in 0..16u8 {
        add(store.player_pack_slot(i).unwrap_or(0), items);
    }
    for bag in 19..23u8 {
        let bag_guid = store.player_inv_slot(bag).unwrap_or(0);
        if bag_guid == 0 {
            continue;
        }
        let slots: Vec<u64> = items
            .object(bag_guid)
            .map(|b| {
                let n = b.container_num_slots().unwrap_or(0).min(36) as u8;
                (0..n).filter_map(|i| b.container_slot(i)).collect()
            })
            .unwrap_or_default();
        for guid in slots {
            add(guid, items);
        }
    }
    total.min(u64::from(u32::MAX)) as u32
}

/// Build the Lua-facing snapshot from [`MerchantOpen`] + the player/vendor descriptors — `None`
/// when no vendor is open.
#[allow(clippy::too_many_arguments)]
fn snapshot(
    open: &MerchantOpen,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
    player: Option<&benilla_protocol::ObjectFields>,
    vendor_npc_flags: u32,
    tables: Option<&RepairTables>,
) -> Option<MerchantState> {
    open.vendor?;
    let buyback = player
        .map(|store| {
            buyback_order(store)
                .into_iter()
                .map(|idx| resolve_buyback(idx, store, items, icons, commands))
                .collect()
        })
        .unwrap_or_default();
    let can_repair = vendor_npc_flags & NPC_FLAG_REPAIR != 0;
    let repair_cost = match (can_repair, player, tables) {
        (true, Some(store), Some(t)) => repair_all_cost(store, items, t, commands),
        _ => 0,
    };
    Some(MerchantState {
        items: open
            .items
            .iter()
            .map(|it| resolve_item(it, items, icons, commands))
            .collect(),
        buyback,
        can_repair,
        repair_all_cost: repair_cost,
    })
}

/// Push the current merchant into the VM and fire the show/update/close events on a transition (or a
/// content change — the async name landing, a post-buy stock update). Also surfaces refusals on the
/// red error line and pushes the player's purse each frame it changes. Diffed against a `Local`
/// memory, exactly like the gossip/container feeds.
#[allow(clippy::too_many_arguments)]
fn feed_merchant(
    script: Option<NonSendMut<UiScript>>,
    open: Res<MerchantOpen>,
    mut items: ResMut<Items>,
    icons: Option<Res<ItemDisplays>>,
    commands: Res<NetCommands>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    units: Query<(&crate::net::Guid, &ObjectStore), Without<SelfPlayer>>,
    tables: Option<Res<RepairTables>>,
    mut names: ResMut<NameCache>,
    mut errors: ResMut<MerchantErrors>,
    mut last: Local<Option<MerchantState>>,
    mut last_money: Local<Option<u64>>,
    mut last_name: Local<Option<String>>,
    mut last_vendor: Local<Option<u64>>,
) {
    let Some(mut script) = script else {
        return;
    };
    // Refusals surface as the client's red error line (the equip/cast path's exact shape).
    for refusal in errors.0.drain(..) {
        let text = match refusal {
            MerchantRefusal::Buy(r) => buy_error_text(r),
            MerchantRefusal::Sell(r) => sell_error_text(r),
        };
        script.fire_event("UI_ERROR_MESSAGE", vec![ScriptValue::Str(text)]);
    }
    // The purse: push it only when it changes (u64 copper straight from PLAYER_FIELD_COINAGE),
    // and fire the real client's PLAYER_MONEY event with it — the money displays (bag + merchant
    // purses) repaint on the event rather than riding some window-content repaint that happens to
    // coincide (a sell repainted the bag purse only because it also moved bag contents).
    if let Some(store) = self_q.iter().next() {
        if let Some(copper) = store.0.player_money() {
            let copper = u64::from(copper);
            if *last_money != Some(copper) {
                script.set_money(copper);
                script.fire_event("PLAYER_MONEY", vec![]);
                *last_money = Some(copper);
            }
        }
    }

    // The vendor's service bits gate the repair pair; the player's descriptor carries buyback.
    let vendor_npc_flags = open
        .vendor
        .and_then(|g| units.iter().find(|(guid, _)| guid.0 == g))
        .map(|(_, store)| store.0.unit_npc_flags())
        .unwrap_or(0);
    let player = self_q.iter().next().map(|s| &s.0);
    let fresh = snapshot(
        &open,
        &mut items,
        icons.as_deref(),
        &commands,
        player,
        vendor_npc_flags,
        tables.as_deref(),
    );
    // The vendor's name resolves through the NameCache (a creature-name query, ask-once — the gossip
    // feed's pattern). `None`/empty while in flight; the title shows "Merchant" until it lands, then
    // re-fires MERCHANT_UPDATE with the name (the diff below tracks the name too, so a name-only
    // change still repaints the title). It rides an event arg rather than a `MerchantState` field so
    // no benilla-ui engine change is needed for the title.
    let vendor_name = open
        .vendor
        .and_then(|g| names.resolve(g, &commands).map(str::to_string));
    let name_changed = *last_name != vendor_name;
    // A different vendor while the window is already open is a real close+open (the client's
    // ShowUIPanel early-returns when visible, so the open sound only re-plays after a hide —
    // decision 0096 / [`crate::ui_session::npc_switched`]).
    let switched = npc_switched(*last_vendor, open.vendor);
    if fresh == *last && !name_changed && !switched {
        return;
    }
    script.set_merchant(fresh.clone());
    let name_arg = || vec![ScriptValue::Str(vendor_name.clone().unwrap_or_default())];
    if switched {
        // Close the old vendor, open the new: the frame hides then shows, playing the close then
        // open kits (and closing/reopening the bag). The MERCHANT_CLOSED routes through the window's
        // OnHide → CloseMerchant (decision 0095), which queues a close intent — consume it here so
        // the drain does NOT clear the vendor we just re-opened to.
        script.fire_event("MERCHANT_CLOSED", vec![]);
        script.fire_event("MERCHANT_SHOW", name_arg());
        let _ = script.take_merchant_close();
    } else {
        match (&*last, &fresh) {
            (None, Some(_)) => script.fire_event("MERCHANT_SHOW", name_arg()),
            // A content change while open (async item/vendor name landed, stock moved) → repaint.
            (Some(_), Some(_)) => script.fire_event("MERCHANT_UPDATE", name_arg()),
            (Some(_), None) => script.fire_event("MERCHANT_CLOSED", vec![]),
            (None, None) => {}
        }
    }
    *last = fresh;
    *last_name = vendor_name;
    *last_vendor = open.vendor;
}

/// The merchant window is an NPC session: the standardized range guard
/// ([`crate::ui_session`]) client-side-closes it — the exact `CloseMerchant` clear — when the
/// player walks out of the vendor's service range or the vendor despawns.
impl NpcSession for MerchantOpen {
    fn npc(&self) -> Option<u64> {
        self.vendor
    }

    fn close(&mut self) {
        self.clear();
    }
}

/// Drain the Lua intents: a bought row → `CMSG_BUY_ITEM` (mapped to the row's item entry; buy is by
/// entry, not the vendor `muid` — decision 0081); a buyback → `CMSG_BUYBACK_ITEM` (the clicked
/// 1-based, timestamp-sorted list index mapped to its ABSOLUTE slot 69–80); a repair-all →
/// `CMSG_REPAIR_ITEM` with guid 0; a close → a local clear (no packet, vanilla).
fn drain_merchant(
    script: Option<NonSendMut<UiScript>>,
    mut open: ResMut<MerchantOpen>,
    commands: Res<NetCommands>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
) {
    let Some(mut script) = script else {
        return;
    };
    for index in script.take_merchant_buybacks() {
        let Some(vendor) = open.vendor else { continue };
        let Some(store) = self_q.iter().next() else {
            continue;
        };
        let order = buyback_order(&store.0);
        match usize::try_from(index)
            .ok()
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| order.get(i))
        {
            Some(&idx) => {
                let slot = BUYBACK_SLOT_FIRST + u32::from(idx);
                debug!("ui_merchant: buyback list index {index} → absolute slot {slot}");
                let _ = commands.0.send(ClientCommand::BuybackItem { vendor, slot });
            }
            None => debug!("ui_merchant: BuybackItem({index}) out of range — ignored"),
        }
    }
    if script.take_repair_all() {
        if let Some(vendor) = open.vendor {
            debug!("ui_merchant: repair all");
            let _ = commands.0.send(ClientCommand::RepairItem {
                vendor,
                item_guid: 0,
            });
        }
    }
    for (index, quantity) in script.take_merchant_buys() {
        let Some(vendor) = open.vendor else { continue };
        match open.entry_at(index) {
            Some(entry) => {
                // Stack count is a u8 on the wire (`CMSG_BUY_ITEM`); clamp the Lua quantity.
                let count = quantity.clamp(1, u32::from(u8::MAX)) as u8;
                debug!("ui_merchant: buy row {index} (entry {entry} ×{count})");
                let _ = commands.0.send(ClientCommand::BuyItem {
                    vendor,
                    entry,
                    count,
                });
            }
            None => debug!("ui_merchant: BuyMerchantItem({index}) out of range — ignored"),
        }
    }
    if script.take_merchant_close() {
        debug!("ui_merchant: client-side close (no packet)");
        open.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(entry: u32, slot: u32, count: u32) -> VendorItem {
        VendorItem {
            slot,
            entry,
            display_id: 100 + entry,
            current_count: count,
            price: 500,
            max_durability: 0,
            buy_count: 1,
        }
    }

    #[test]
    fn entry_at_maps_the_one_based_row() {
        let mut open = MerchantOpen::default();
        assert!(!open.is_open());
        open.open(0x42, vec![row(159, 1, STOCK_UNLIMITED), row(4540, 2, 5)]);
        assert!(open.is_open());
        assert_eq!(open.entry_at(1), Some(159));
        assert_eq!(open.entry_at(2), Some(4540));
        assert_eq!(open.entry_at(3), None); // out of range
        assert_eq!(open.entry_at(0), None); // 0 has no row (1-based)
    }

    #[test]
    fn resolve_maps_unlimited_stock_to_minus_one() {
        let mut items = Items::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let commands = NetCommands(tx);
        // Unlimited stock → numAvailable -1; a finite count passes through.
        let unlimited = resolve_item(&row(159, 1, STOCK_UNLIMITED), &mut items, None, &commands);
        assert_eq!(unlimited.num_available, -1);
        assert_eq!(unlimited.item_id, 159);
        let finite = resolve_item(&row(4540, 2, 5), &mut items, None, &commands);
        assert_eq!(finite.num_available, 5);
        // No template answer yet → name + tooltip stats in flight (nil), the rest present.
        assert!(finite.name.is_none());
        assert!(finite.stats.is_none());
        assert_eq!(finite.price, 500);
    }

    #[test]
    fn resolve_fills_the_tooltip_stats_from_the_template() {
        let mut items = Items::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let commands = NetCommands(tx);
        items.insert_template(
            2129,
            Some(benilla_protocol::messages::ItemInfo {
                class: 4,
                subclass: 6,
                name: "Chipped Buckler".into(),
                display_info_id: 18730,
                quality: 1,
                flags: 0,
                buy_price: 0,
                sell_price: 0,
                inventory_type: 14,
                allowable_class: -1,
                allowable_race: -1,
                item_level: 0,
                required_level: 0,
                required_skill: 0,
                required_skill_rank: 0,
                required_spell: 0,
                required_honor_rank: 0,
                required_city_rank: 0,
                required_rep_faction: 0,
                required_rep_rank: 0,
                max_count: 0,
                stackable: 1,
                container_slots: 0,
                stats: Vec::new(),
                damages: Vec::new(),
                dmg_min: 0.0,
                dmg_max: 0.0,
                dmg_type: 0,
                armor: 85,
                resistances: [0; 6],
                delay_ms: 0,
                ammo_type: 0,
                ranged_mod_range: 0.0,
                spells: Vec::new(),
                use_spell: None,
                bonding: 0,
                description: String::new(),
                page_text: 0,
                language_id: 0,
                page_material: 0,
                start_quest: 0,
                lock_id: 0,
                material: 0,
                sheath: 4,
                random_property: 0,
                block: 1,
                item_set: 0,
                max_durability: 0,
                area: 0,
                map: 0,
                bag_family: 0,
            }),
        );
        let resolved = resolve_item(&row(2129, 1, 3), &mut items, None, &commands);
        let stats = resolved.stats.expect("template answered → stats present");
        assert_eq!(
            (
                stats.quality,
                stats.inventory_type,
                stats.class,
                stats.subclass
            ),
            (1, 14, 4, 6)
        );
        assert_eq!((stats.armor, stats.block), (85, 1));
    }

    #[test]
    fn update_stock_moves_the_matching_row() {
        let mut open = MerchantOpen::default();
        open.open(0x42, vec![row(159, 1, STOCK_UNLIMITED), row(4540, 2, 5)]);
        open.update_stock(2, 4);
        assert_eq!(open.items[1].current_count, 4);
        open.update_stock(99, 0); // no such slot — no-op
        assert_eq!(open.items[0].current_count, STOCK_UNLIMITED);
    }

    #[test]
    fn clear_closes_the_window() {
        let mut open = MerchantOpen::default();
        open.open(0x42, vec![row(159, 1, 3)]);
        open.clear();
        assert!(!open.is_open());
        assert!(open.items.is_empty());
    }
}
