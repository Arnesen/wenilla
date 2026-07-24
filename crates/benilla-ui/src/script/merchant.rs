//! The merchant bindings (decision 0081 phase 4) — the Era-shaped vendor surface, the same two-way
//! seam as [`super::container`]/[`super::gossip`]: the app pushes a **merchant snapshot**
//! ([`UiScript::set_merchant`] — the vendor rows already resolved from the wire to name/icon/price
//! by the app's item stores), and the Lua `BuyMerchantItem`/`CloseMerchant` calls queue outbound
//! **intents** the app drains ([`UiScript::take_merchant_buys`] / [`UiScript::take_merchant_close`]).
//! The engine holds no vendor knowledge — a row is "a name, an icon path, a price, a stack size,
//! how many are left, and the tooltip's stat head" ([`ItemStatsHead`], read by
//! `BenillaGetMerchantItemStats`).
//!
//! ## The 5875 API shape
//!
//! 1.12's `GetMerchantItemInfo(index)` returns a flat **6-value** tuple the FrameXML reads
//! positionally — `name, texture, price, quantity, numAvailable, isUsable` (byte-verified,
//! `0x4fb150`; the ref's own `MerchantFrame_UpdateMerchantInfo` destructures exactly these six.
//! The `isPurchasable`/`extendedCost` extras an earlier draft carried are TBC-era, not 5875).
//! `index` is **1-based**; an invalid index answers the binding's fixed tuple
//! `(nil, nil, 0, 1, 0, 1)` — same shape as `GetBuybackItemInfo`'s (`0x4fb2be`). `numAvailable`
//! is `-1` for unlimited stock (the wire's `0xFFFF_FFFF` through `fild`, mapped by the app).
//! `isUsable` is `1`/`nil` (the client pushes a number or nil, never a boolean): the
//! [`super::item_stats::item_usable`] gate over the row's template — a template still in flight
//! reads usable, the getter's null-record skip (`0x4fb298`).
//!
//! Buying addresses the row by its **1-based list position** here (`BuyMerchantItem(index)`); the app
//! maps that position to the item *entry* the wire's `CMSG_BUY_ITEM` needs (buy is by entry, not the
//! vendor `muid` — decision 0081). vanilla's client-side `CloseMerchant()` sends no packet, so it
//! just flags the app to clear its local state (the gossip pattern).

use mlua::{Lua, MultiValue, Value};

use super::container::UiCursorMode;
use super::Model;

/// One vendor row, resolved by the app from the wire `VendorItem` (decision 0081). Plain data —
/// 1-based order in the window is its position in [`MerchantState::items`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MerchantItem {
    /// The item name (`GetMerchantItemInfo`'s first return); `None` while the ask-once item-template
    /// query is still in flight (the API reports `nil`, the XML shows a placeholder).
    pub name: Option<String>,
    /// Icon texture path (`Interface\Icons\…`); `None` while the template answer is in flight.
    pub texture: Option<String>,
    /// Buy price in copper (already reputation-discounted server-side).
    pub price: u32,
    /// Stack size delivered per purchase (`item_template.buy_count`, the wire's `buy_count`).
    pub quantity: u32,
    /// Remaining stock, or `-1` for unlimited (the app maps the wire's `0xFFFF_FFFF`).
    pub num_available: i32,
    /// The item's template **entry** — what `CMSG_BUY_ITEM` addresses (the app maps the clicked
    /// 1-based row to this; the Lua side never sees it).
    pub item_id: u32,
    /// The stat head the hover tooltip renders, from the same ask-once template answer as `name`
    /// (`None` while it's in flight — the tooltip shows nothing it can't know yet). Still plain
    /// data: the engine renders whatever numbers the app resolved, it holds no item model.
    pub stats: Option<ItemStatsHead>,
}

/// An item template's tooltip stat head (the app's `ItemInfo` view, resolved per row): what the
/// vendor hover tooltip needs beyond name/icon/price. The real client's `GameTooltip:SetMerchantItem`
/// reads the same template fields C++-side; benilla feeds them to Lua through
/// `BenillaGetMerchantItemStats(index)` (no 1.12 Lua API carries damage/armor — tooltip content was
/// C++'s alone — so the feed is benilla-named, not Era-shaped).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ItemStatsHead {
    /// 0 poor … 6 artifact — colours the tooltip's name line.
    pub quality: u32,
    /// `InventoryType` — the tooltip's slot line ("Main Hand", "Chest", …); 0 = no slot line.
    pub inventory_type: u32,
    /// Item class (2 weapon, 4 armor, 6 projectile) — with `subclass`, the slot line's right column.
    pub class: u32,
    /// Item subclass within `class` (7 = sword, 1 = cloth, …).
    pub subclass: u32,
    /// Damage block 0 per-hit minimum (0 for non-weapons).
    pub dmg_min: f32,
    /// Damage block 0 per-hit maximum.
    pub dmg_max: f32,
    /// Damage block 0 school (0 physical, 1 Holy … 6 Arcane).
    pub dmg_type: u32,
    /// Attack delay in milliseconds ("Speed" = delay / 1000).
    pub delay_ms: u32,
    /// The armor line's value; 0 = no line.
    pub armor: u32,
    /// A shield's block line; 0 = no line.
    pub block: u32,
    /// `SellPrice` — what a vendor pays per unit (the bag tooltip's money row while a merchant is
    /// open; 0 = the "No sell price" line).
    pub sell_price: u32,
}

/// One open merchant window: the vendor's rows, the buyback slots, and the repair head. Pushed
/// whole by the app; `None` means no vendor is open (the window is closed).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MerchantState {
    pub items: Vec<MerchantItem>,
    /// The buyback slots, oldest-first (index 1 = the oldest; the LAST entry is the most recent
    /// sale — the one the merchant page's single buyback slot shows, ref
    /// `GetBuybackItemInfo(GetNumBuybackItems())`). Resolved by the app from the player's
    /// VENDORBUYBACK descriptor fields + the item stores; `price` here is the buyback price
    /// field, `quantity` the stored item's stack count.
    pub buyback: Vec<MerchantItem>,
    /// Whether this vendor repairs (UNIT_NPC_FLAGS repair bit) — shows the repair buttons.
    pub can_repair: bool,
    /// The repair-all cost in copper the app computed (0 = nothing to repair → the repair-all
    /// button disables, ref MerchantFrame_OnShow).
    pub repair_all_cost: u32,
}

impl super::UiScript {
    /// Push (or clear, with `None`) the open merchant's stock snapshot.
    pub fn set_merchant(&mut self, state: Option<MerchantState>) {
        self.model_mut().merchant = state;
    }

    /// Drain the `(index, quantity)` buy intents queued by `BuyMerchantItem` since the last call.
    /// `index` is the 1-based row position; the app maps it to the item entry the wire needs.
    pub fn take_merchant_buys(&mut self) -> Vec<(u32, u32)> {
        std::mem::take(&mut self.model_mut().merchant_buys)
    }

    /// Whether `CloseMerchant` was called since the last drain (and clear the flag). vanilla's
    /// client-side close sends no packet — the app just clears its local merchant state.
    pub fn take_merchant_close(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().merchant_close)
    }

    /// Drain the 1-based buyback-slot intents queued by `BuybackItem` since the last call. The app
    /// maps each to the wire's absolute buyback inventory slot.
    pub fn take_merchant_buybacks(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().merchant_buybacks)
    }

    /// Whether `RepairAllItems` was called since the last drain (and clear the flag). The app
    /// sends the repair-all wire message.
    pub fn take_repair_all(&mut self) -> bool {
        std::mem::take(&mut self.model_mut().repair_all)
    }

    /// The client-side repair-mode latch (`ShowRepairCursor`/`HideRepairCursor`/`InRepairMode`):
    /// while set, a bag/equipment click means "repair this item" — the app reads this to route
    /// clicks and swap the hardware cursor.
    pub fn repair_mode(&self) -> bool {
        self.model_ref().repair_mode
    }
}

/// A row's `isUsable`, the real getters' exact read: the item-usable gate over the row's
/// template from the shared store, keyed by the row's item id — usable while the template is
/// still in flight (the null-record skip, `0x4fb298`). Spell knowledge comes from the engine's
/// spellbook mirror, the same source the tooltip's "Requires <spell>" red reads.
fn is_usable(model: &Model, item_id: u32) -> bool {
    model.item_templates.get(&item_id).is_none_or(|v| {
        super::item_stats::item_usable(v, &model.player_req, |id| {
            model.spellbook.slots.iter().any(|s| s.spell_id == id)
        })
    })
}

/// `1`/`nil` — how the client pushes a usable flag (`pushnumber(1.0)` / `pushnil`).
fn usable_value(usable: bool) -> Value {
    if usable {
        Value::Integer(1)
    } else {
        Value::Nil
    }
}

/// The invalid-index tuple both getters answer (`0x4fb2be` / the buyback equivalent):
/// `(nil, nil, 0, 1, 0, 1)` — still six values.
fn invalid_tuple() -> Vec<Value> {
    vec![
        Value::Nil,
        Value::Nil,
        Value::Integer(0),
        Value::Integer(1),
        Value::Integer(0),
        Value::Integer(1),
    ]
}

/// Register the merchant globals.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // → the number of rows the open vendor has (0 when no vendor is open).
    g.set(
        "GetMerchantNumItems",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model.merchant.as_ref().map_or(0, |m| m.items.len()) as i64)
        })?,
    )?;

    // GetMerchantItemInfo(index) → name, texture, price, quantity, numAvailable, isUsable — the
    // byte-verified 5875 6-tuple (0x4fb150; see the module doc). `index` is 1-based; an invalid
    // index answers the fixed tuple (nil, nil, 0, 1, 0, 1).
    g.set(
        "GetMerchantItemInfo",
        lua.create_function(|lua, index: usize| {
            let (item, usable) = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                let item = model
                    .merchant
                    .as_ref()
                    .and_then(|m| index.checked_sub(1).and_then(|n| m.items.get(n)))
                    .cloned();
                let usable = item.as_ref().is_none_or(|it| is_usable(&model, it.item_id));
                (item, usable)
            };
            let Some(it) = item else {
                return Ok(MultiValue::from_vec(invalid_tuple()));
            };
            let name = match &it.name {
                Some(n) => Value::String(lua.create_string(n)?),
                None => Value::Nil,
            };
            let texture = match &it.texture {
                Some(t) => Value::String(lua.create_string(t)?),
                None => Value::Nil,
            };
            Ok(MultiValue::from_vec(vec![
                name,
                texture,
                Value::Integer(i64::from(it.price)),
                Value::Integer(i64::from(it.quantity)),
                Value::Integer(i64::from(it.num_available)),
                usable_value(usable),
            ]))
        })?,
    )?;

    // BenillaGetMerchantItemStats(index) → quality, invType, class, subclass, dmgMin, dmgMax,
    // dmgType, delayMs, armor, block — the tooltip stat head ([`ItemStatsHead`]), or nil while
    // the row's template answer is in flight / the index is out of range. Benilla-named: 1.12's Lua
    // API never carried these (item tooltip content was C++'s alone — `SetMerchantItem 0x534080`).
    g.set(
        "BenillaGetMerchantItemStats",
        lua.create_function(|lua, index: usize| {
            let stats = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model
                    .merchant
                    .as_ref()
                    .and_then(|m| index.checked_sub(1).and_then(|n| m.items.get(n)))
                    .and_then(|it| it.stats)
            };
            let Some(s) = stats else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            Ok(MultiValue::from_vec(vec![
                Value::Integer(i64::from(s.quality)),
                Value::Integer(i64::from(s.inventory_type)),
                Value::Integer(i64::from(s.class)),
                Value::Integer(i64::from(s.subclass)),
                Value::Number(f64::from(s.dmg_min)),
                Value::Number(f64::from(s.dmg_max)),
                Value::Integer(i64::from(s.dmg_type)),
                Value::Integer(i64::from(s.delay_ms)),
                Value::Integer(i64::from(s.armor)),
                Value::Integer(i64::from(s.block)),
            ]))
        })?,
    )?;

    // BuyMerchantItem(index [, quantity]) — queue the 1-based row + stack count (default 1); the app
    // maps the row to the item entry the wire addresses.
    g.set(
        "BuyMerchantItem",
        lua.create_function(|lua, (index, quantity): (u32, Option<u32>)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.merchant_buys.push((index, quantity.unwrap_or(1)));
            Ok(())
        })?,
    )?;

    // CloseMerchant() — client-side close (no packet, vanilla): flag it so the app clears its state.
    g.set(
        "CloseMerchant",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.merchant_close = true;
            Ok(())
        })?,
    )?;

    // → how many buyback slots are filled (ref GetNumBuybackItems; 0 with no vendor open).
    g.set(
        "GetNumBuybackItems",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model.merchant.as_ref().map_or(0, |m| m.buyback.len()) as i64)
        })?,
    )?;

    // GetBuybackItemInfo(index) → name, texture, price, quantity, numAvailable, isUsable (the
    // same 6-tuple; 0x4fb310, wow-re ui/scratch/buyback-data-path.md). `index` is 1-based
    // oldest-first; an invalid index answers the fixed tuple (nil, nil, 0, 1, 0, 1). isUsable is
    // the same 0x5ea930 gate over the sold item's template (0x4fb4f7) — yes, even a just-sold
    // item reds if the seller can't use it (a mule selling a wrong-class drop).
    g.set(
        "GetBuybackItemInfo",
        lua.create_function(|lua, index: usize| {
            let (item, usable) = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                let item = model
                    .merchant
                    .as_ref()
                    .and_then(|m| index.checked_sub(1).and_then(|n| m.buyback.get(n)))
                    .cloned();
                let usable = item.as_ref().is_none_or(|it| is_usable(&model, it.item_id));
                (item, usable)
            };
            let Some(it) = item else {
                return Ok(MultiValue::from_vec(invalid_tuple()));
            };
            let name = match &it.name {
                Some(n) => Value::String(lua.create_string(n)?),
                None => Value::Nil,
            };
            let texture = match &it.texture {
                Some(t) => Value::String(lua.create_string(t)?),
                None => Value::Nil,
            };
            Ok(MultiValue::from_vec(vec![
                name,
                texture,
                Value::Integer(i64::from(it.price)),
                Value::Integer(i64::from(it.quantity)),
                Value::Integer(i64::from(it.num_available)),
                usable_value(usable),
            ]))
        })?,
    )?;

    // BenillaGetBuybackItemStats(index) — the buyback hover's tooltip stat head, same shape and
    // reason as BenillaGetMerchantItemStats.
    g.set(
        "BenillaGetBuybackItemStats",
        lua.create_function(|lua, index: usize| {
            let stats = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model
                    .merchant
                    .as_ref()
                    .and_then(|m| index.checked_sub(1).and_then(|n| m.buyback.get(n)))
                    .and_then(|it| it.stats)
            };
            let Some(s) = stats else {
                return Ok(MultiValue::from_vec(vec![Value::Nil]));
            };
            Ok(MultiValue::from_vec(vec![
                Value::Integer(i64::from(s.quality)),
                Value::Integer(i64::from(s.inventory_type)),
                Value::Integer(i64::from(s.class)),
                Value::Integer(i64::from(s.subclass)),
                Value::Number(f64::from(s.dmg_min)),
                Value::Number(f64::from(s.dmg_max)),
                Value::Integer(i64::from(s.dmg_type)),
                Value::Integer(i64::from(s.delay_ms)),
                Value::Integer(i64::from(s.armor)),
                Value::Integer(i64::from(s.block)),
            ]))
        })?,
    )?;

    // BuybackItem(index) — queue the 1-based buyback slot; the app maps it to the wire's absolute
    // inventory slot.
    g.set(
        "BuybackItem",
        lua.create_function(|lua, index: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.merchant_buybacks.push(index);
            Ok(())
        })?,
    )?;

    // CanMerchantRepair() → 1/nil (the Era boolean shape) — whether the open vendor repairs.
    g.set(
        "CanMerchantRepair",
        lua.create_function(|lua, ()| {
            let can = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model.merchant.as_ref().is_some_and(|m| m.can_repair)
            };
            Ok(if can { Value::Integer(1) } else { Value::Nil })
        })?,
    )?;

    // GetRepairAllCost() → cost, canRepair — the app-computed repair-all total; canRepair is
    // "there is damage to pay for" (ref MerchantFrame_OnShow enables the button on it).
    g.set(
        "GetRepairAllCost",
        lua.create_function(|lua, ()| {
            let cost = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model.merchant.as_ref().map_or(0, |m| m.repair_all_cost)
            };
            Ok(MultiValue::from_vec(vec![
                Value::Integer(i64::from(cost)),
                Value::Boolean(cost > 0),
            ]))
        })?,
    )?;

    // RepairAllItems() — flag the repair-all intent for the app to send.
    g.set(
        "RepairAllItems",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.repair_all = true;
            Ok(())
        })?,
    )?;

    // The repair-mode latch trio (ref MerchantRepairItemButton OnClick): ShowRepairCursor arms
    // "next item click repairs", HideRepairCursor disarms, InRepairMode() → 1/nil reads it. The
    // app reads UiScript::repair_mode to route clicks + swap the hardware cursor.
    g.set(
        "ShowRepairCursor",
        lua.create_function(|lua, ()| {
            lua.app_data_mut::<Model>()
                .expect("model app_data")
                .repair_mode = true;
            Ok(())
        })?,
    )?;
    g.set(
        "HideRepairCursor",
        lua.create_function(|lua, ()| {
            lua.app_data_mut::<Model>()
                .expect("model app_data")
                .repair_mode = false;
            Ok(())
        })?,
    )?;
    g.set(
        "InRepairMode",
        lua.create_function(|lua, ()| {
            let mode = lua
                .app_data_ref::<Model>()
                .expect("model app_data")
                .repair_mode;
            Ok(if mode { Value::Integer(1) } else { Value::Nil })
        })?,
    )?;

    // ShowMerchantSellCursor(index) (5875 `0x4fbab0`, "buying from vendor") and
    // ShowBuybackSellCursor(index) (`0x4fbbb0`, "re-buying") — the vendor-item hover cursor
    // (wow-re cursor-system.md §7). Despite the "Sell" in the names, these arm the BUY cursor with
    // an affordability gate: player coin vs the row's price → Buy(3) if `coin >= price`, else the
    // grayed UnableBuy(23). The merchant frame's OnUpdate re-arms this every frame while an item is
    // hovered (Ctrl-hover swaps to `ShowInspectCursor` instead); OnLeave `ResetCursor`s it. An
    // unresolvable index leaves the cursor unchanged — the binary's every-fail-path `ret` with no
    // `CursorSetMode` (the in-flight item lock and base-mode-Point gates live app-side).
    g.set(
        "ShowMerchantSellCursor",
        lua.create_function(|lua, index: usize| {
            arm_vendor_cursor(lua, |m| {
                m.items.get(index.wrapping_sub(1)).map(|it| it.price)
            });
            Ok(())
        })?,
    )?;
    g.set(
        "ShowBuybackSellCursor",
        lua.create_function(|lua, index: usize| {
            arm_vendor_cursor(lua, |m| {
                m.buyback.get(index.wrapping_sub(1)).map(|it| it.price)
            });
            Ok(())
        })?,
    )?;

    Ok(())
}

/// Arm the vendor-item hover cursor from a `price` picked out of the open merchant snapshot: Buy(3)
/// if the player can afford it, UnableBuy(23) otherwise. Shared by `ShowMerchantSellCursor` and
/// `ShowBuybackSellCursor` (which differ only in which price list they read). A `None` price (no
/// vendor open, or the 1-based index out of range) leaves the cursor untouched — the binary bails
/// without a `CursorSetMode` (wow-re cursor-system.md §7).
fn arm_vendor_cursor(lua: &Lua, price_of: impl FnOnce(&MerchantState) -> Option<u32>) {
    let mut model = lua.app_data_mut::<Model>().expect("model app_data");
    let Some(price) = model.merchant.as_ref().and_then(price_of) else {
        return;
    };
    model.ui_cursor = Some(if model.money >= u64::from(price) {
        UiCursorMode::Buy
    } else {
        UiCursorMode::UnableBuy
    });
}

#[cfg(test)]
mod tests {
    use super::{ItemStatsHead, MerchantItem, MerchantState};
    use crate::script::UiScript;

    fn stock() -> MerchantState {
        MerchantState {
            items: vec![
                MerchantItem {
                    name: Some("Refreshing Spring Water".into()),
                    texture: Some("Interface\\Icons\\INV_Drink_18".into()),
                    price: 25,
                    quantity: 1,
                    num_available: -1, // unlimited
                    item_id: 159,
                    // A consumable's stat head: everything zero but the quality.
                    stats: Some(ItemStatsHead {
                        quality: 1,
                        ..Default::default()
                    }),
                },
                // An in-flight row: the vendor list arrived, the item-template answer hasn't.
                MerchantItem {
                    name: None,
                    texture: None,
                    price: 1500,
                    quantity: 1,
                    num_available: 3,
                    item_id: 4540,
                    stats: None,
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn merchant_snapshot_reads() {
        let mut s = UiScript::new().unwrap();
        // No vendor open: count 0, info nil.
        assert_eq!(s.eval::<i64>("return GetMerchantNumItems()").unwrap(), 0);
        assert!(s
            .eval::<bool>("return GetMerchantItemInfo(1) == nil")
            .unwrap());

        s.set_merchant(Some(stock()));
        assert_eq!(s.eval::<i64>("return GetMerchantNumItems()").unwrap(), 2);
        // Row 1 (resolved): the byte-verified 6-tuple (0x4fb150) — isUsable is 1, not a boolean.
        let (name, texture, price, quantity, num, usable) = s
            .eval::<(String, String, i64, i64, i64, i64)>("return GetMerchantItemInfo(1)")
            .unwrap();
        assert_eq!(name, "Refreshing Spring Water");
        assert_eq!(texture, "Interface\\Icons\\INV_Drink_18");
        assert_eq!((price, quantity, num), (25, 1, -1));
        assert_eq!(usable, 1);

        // Row 2 (in flight): name + texture nil, the rest still present — and usable (the
        // null-record skip: no template answer, nothing to judge).
        assert!(s
            .eval::<bool>(
                "local n, t, p, q, a, u = GetMerchantItemInfo(2)\n\
                 return n == nil and t == nil and p == 1500 and u == 1",
            )
            .unwrap());
        // An invalid index answers the fixed 6-tuple (nil, nil, 0, 1, 0, 1) — 0x4fb2be.
        assert!(s
            .eval::<bool>(
                "local n, t, p, q, a, u = GetMerchantItemInfo(9)\n\
                 return n == nil and t == nil and p == 0 and q == 1 and a == 0 and u == 1",
            )
            .unwrap());
    }

    /// The isUsable leg end-to-end: the gate reads the shared template store + the player req
    /// state, so a level-short player reds the row (nil), levelling past it whitens (1), and the
    /// same flag drives the buyback tuple.
    #[test]
    fn merchant_usable_tracks_the_item_gate() {
        use crate::script::{ItemTemplateView, PlayerReqState};
        let mut s = UiScript::new().unwrap();
        let mut state = stock();
        state.buyback = vec![MerchantItem {
            name: Some("Light Mail Gloves".into()),
            texture: Some("Interface\\Icons\\INV_Gauntlets_05".into()),
            price: 47,
            quantity: 1,
            item_id: 2418,
            ..Default::default()
        }];
        s.set_merchant(Some(state));
        s.set_item_template(
            159, // row 1: the spring water becomes level-gated for the test
            ItemTemplateView {
                name: "Refreshing Spring Water".into(),
                required_level: 5,
                allowable_class: -1,
                allowable_race: -1,
                ..Default::default()
            },
        );
        s.set_item_template(
            2418,
            ItemTemplateView {
                name: "Light Mail Gloves".into(),
                required_level: 5,
                allowable_class: -1,
                allowable_race: -1,
                ..Default::default()
            },
        );
        let req = |level| PlayerReqState {
            level,
            class_id: 1,
            race_id: 1,
            ..Default::default()
        };
        s.set_player_req_state(req(4));
        assert!(s
            .eval::<bool>(
                "local n, t, p, q, a, u = GetMerchantItemInfo(1)\nreturn u == nil and n ~= nil",
            )
            .unwrap());
        assert!(s
            .eval::<bool>(
                "local n, t, p, q, a, u = GetBuybackItemInfo(1)\nreturn u == nil and n ~= nil",
            )
            .unwrap());
        // Row 2 has no template pushed → still usable, whatever the player state.
        assert!(s
            .eval::<bool>("local n, t, p, q, a, u = GetMerchantItemInfo(2)\nreturn u == 1")
            .unwrap());
        // Levelling past the requirement whitens both.
        s.set_player_req_state(req(5));
        assert!(s
            .eval::<bool>("local n, t, p, q, a, u = GetMerchantItemInfo(1)\nreturn u == 1")
            .unwrap());
        assert!(s
            .eval::<bool>("local n, t, p, q, a, u = GetBuybackItemInfo(1)\nreturn u == 1")
            .unwrap());
    }

    #[test]
    fn merchant_stats_feed_reads_the_tooltip_head() {
        let mut s = UiScript::new().unwrap();
        let mut stock = stock();
        // Row 1 becomes a sword so every stat column is distinguishable.
        stock.items[0].stats = Some(ItemStatsHead {
            quality: 2,
            inventory_type: 21,
            class: 2,
            subclass: 7,
            dmg_min: 5.0,
            dmg_max: 9.0,
            dmg_type: 2,
            delay_ms: 2600,
            armor: 0,
            block: 0,
            sell_price: 0,
        });
        s.set_merchant(Some(stock));
        let (quality, inv, class, sub, dmin, dmax, dtype, delay, armor, block) = s
            .eval::<(i64, i64, i64, i64, f64, f64, i64, i64, i64, i64)>(
                "return BenillaGetMerchantItemStats(1)",
            )
            .unwrap();
        assert_eq!((quality, inv, class, sub), (2, 21, 2, 7));
        assert_eq!((dmin, dmax, dtype, delay), (5.0, 9.0, 2, 2600));
        assert_eq!((armor, block), (0, 0));
        // In flight (row 2) and out of range: nil.
        assert!(s
            .eval::<bool>("return BenillaGetMerchantItemStats(2) == nil")
            .unwrap());
        assert!(s
            .eval::<bool>("return BenillaGetMerchantItemStats(9) == nil")
            .unwrap());
    }

    #[test]
    fn buy_merchant_item_queues_intents() {
        let mut s = UiScript::new().unwrap();
        s.set_merchant(Some(stock()));
        s.run("BuyMerchantItem(1)").unwrap(); // default quantity 1
        s.run("BuyMerchantItem(2, 5)").unwrap();
        assert_eq!(s.take_merchant_buys(), vec![(1, 1), (2, 5)]);
        assert!(s.take_merchant_buys().is_empty(), "drained");
    }

    #[test]
    fn close_merchant_flags_the_intent() {
        let mut s = UiScript::new().unwrap();
        s.set_merchant(Some(stock()));
        assert!(!s.take_merchant_close());
        s.run("CloseMerchant()").unwrap();
        assert!(s.take_merchant_close());
        assert!(!s.take_merchant_close(), "drained");
    }

    /// The buyback surface: count, the ref tuple (including the 5875 binding's exact empty tuple
    /// `(nil,nil,0,1,0,1)` for an invalid index — 0x4fb310), and the BuybackItem intent drain.
    #[test]
    fn buyback_reads_and_intents() {
        let mut s = UiScript::new().unwrap();
        assert_eq!(s.eval::<i64>("return GetNumBuybackItems()").unwrap(), 0);
        assert!(s
            .eval::<bool>(
                "local n, t, p, q, a, u = GetBuybackItemInfo(1)\n\
                 return n == nil and t == nil and p == 0 and q == 1 and a == 0 and u == 1",
            )
            .unwrap());

        let mut state = stock();
        state.buyback = vec![MerchantItem {
            name: Some("Worn Dagger".into()),
            texture: Some("Interface\\Icons\\INV_Weapon_ShortBlade_01".into()),
            price: 47,
            quantity: 1,
            stats: Some(ItemStatsHead {
                quality: 1,
                ..Default::default()
            }),
            ..Default::default()
        }];
        s.set_merchant(Some(state));
        assert_eq!(s.eval::<i64>("return GetNumBuybackItems()").unwrap(), 1);
        let (name, _tex, price): (String, String, i64) =
            s.eval("return GetBuybackItemInfo(1)").unwrap();
        assert_eq!((name.as_str(), price), ("Worn Dagger", 47));
        assert!(s
            .eval::<bool>("return BenillaGetBuybackItemStats(1) ~= nil")
            .unwrap());

        s.run("BuybackItem(1)").unwrap();
        assert_eq!(s.take_merchant_buybacks(), vec![1]);
        assert!(s.take_merchant_buybacks().is_empty(), "drained");
    }

    /// The repair surface: CanMerchantRepair off the snapshot, GetRepairAllCost's (cost, canRepair)
    /// pair, the RepairAllItems intent, and the client-side repair-mode latch trio.
    #[test]
    fn repair_reads_intents_and_mode_latch() {
        let mut s = UiScript::new().unwrap();
        assert!(s.eval::<bool>("return CanMerchantRepair() == nil").unwrap());

        let mut state = stock();
        state.can_repair = true;
        state.repair_all_cost = 1234;
        s.set_merchant(Some(state));
        assert!(s.eval::<bool>("return CanMerchantRepair() == 1").unwrap());
        assert!(s
            .eval::<bool>("local c, can = GetRepairAllCost()\nreturn c == 1234 and can == true",)
            .unwrap());

        s.run("RepairAllItems()").unwrap();
        assert!(s.take_repair_all());
        assert!(!s.take_repair_all(), "drained");

        // ShowRepairCursor arms, InRepairMode reads (1/nil), HideRepairCursor disarms.
        assert!(s.eval::<bool>("return InRepairMode() == nil").unwrap());
        s.run("ShowRepairCursor()").unwrap();
        assert!(s.repair_mode());
        assert!(s.eval::<bool>("return InRepairMode() == 1").unwrap());
        s.run("HideRepairCursor()").unwrap();
        assert!(!s.repair_mode());
    }

    #[test]
    fn clearing_the_merchant_empties_it() {
        let mut s = UiScript::new().unwrap();
        s.set_merchant(Some(stock()));
        s.set_merchant(None);
        assert_eq!(s.eval::<i64>("return GetMerchantNumItems()").unwrap(), 0);
        assert!(s
            .eval::<bool>("return GetMerchantItemInfo(1) == nil")
            .unwrap());
    }

    /// The vendor-item hover cursor (`ShowMerchantSellCursor`/`ShowBuybackSellCursor`, `0x4fbab0`/
    /// `0x4fbbb0`): Buy(3) when the player can afford the row, the grayed UnableBuy(23) when they
    /// can't; `ShowInspectCursor` arms the magnifier; `ResetCursor` clears the whole override; an
    /// unresolvable index (or no vendor open) leaves the cursor unchanged.
    #[test]
    fn vendor_hover_cursor_gates_on_affordability() {
        use crate::script::UiCursorMode;
        let mut s = UiScript::new().unwrap();

        // No vendor open: the Show* calls resolve nothing and leave the cursor untouched.
        s.run("ShowMerchantSellCursor(1)").unwrap();
        assert_eq!(s.ui_cursor(), None);

        let mut state = stock(); // row 1 price 25, row 2 price 1500
        state.buyback = vec![MerchantItem {
            price: 47,
            ..Default::default()
        }];
        s.set_merchant(Some(state));
        s.set_money(100); // affords row 1 (25) + buyback (47), not row 2 (1500)

        // Row 1: coin ≥ price → Buy(3).
        s.run("ShowMerchantSellCursor(1)").unwrap();
        assert_eq!(s.ui_cursor(), Some(UiCursorMode::Buy));
        // Row 2: coin < price → the grayed UnableBuy(23).
        s.run("ShowMerchantSellCursor(2)").unwrap();
        assert_eq!(s.ui_cursor(), Some(UiCursorMode::UnableBuy));

        // The Ctrl-hover magnifier, then ResetCursor back to the base mode.
        s.run("ShowInspectCursor()").unwrap();
        assert_eq!(s.ui_cursor(), Some(UiCursorMode::Inspect));
        s.run("ResetCursor()").unwrap();
        assert_eq!(s.ui_cursor(), None);

        // Buyback affords too → Buy(3).
        s.run("ShowBuybackSellCursor(1)").unwrap();
        assert_eq!(s.ui_cursor(), Some(UiCursorMode::Buy));

        // An out-of-range index bails without touching the armed cursor (the binary's no-CursorSetMode
        // fail path) — the Buy from the buyback hover above survives.
        s.run("ShowMerchantSellCursor(99)").unwrap();
        assert_eq!(s.ui_cursor(), Some(UiCursorMode::Buy));
    }
}
