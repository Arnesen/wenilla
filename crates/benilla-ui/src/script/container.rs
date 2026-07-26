//! The container bindings (decision 0068 T2) — the `C_Container` namespace, the Era addon
//! surface for bags (measured on the target addons: `GetContainerItemInfo` and
//! `GetContainerNumSlots` dominate; Bagnon's data engine is built on exactly these). Same
//! two-way seam as [`super::action`]: the app pushes a **container snapshot** per bag id
//! ([`UiScript::set_container`] — slots already resolved to icon/count/quality by the app's
//! item stores; the engine holds no item knowledge), and `UseContainerItem` queues an outbound
//! **intent** the app drains into the wire ([`UiScript::take_container_uses`]).
//!
//! Bag ids are the live API's space: `0` the backpack, `1..4` the equipped bags (bank later).
//! Slots are 1-based. One deliberate divergence from the 1.14 documentation:
//! `containerInfo.iconFileID` carries our icon **texture path** (`Interface\Icons\…`), not a
//! numeric FileDataID — 5875 has no FileDataIDs, and every measured consumer feeds the value
//! straight to `SetTexture`, which takes both shapes in the live client.

use mlua::{Lua, Table, Value};

use super::cursor::{self, CursorItem, CursorPayload};
use super::Model;

/// One occupied bag slot, resolved by the app (icon from ItemDisplayInfo, count/quality from the
/// item object + template). Plain data.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContainerSlot {
    /// Icon texture path (`Interface\Icons\…`); `None` while the template answer is in flight.
    pub texture: Option<String>,
    pub count: u32,
    /// Item quality 0..6; `None` while unresolved (the API reports `nil`).
    pub quality: Option<u32>,
    /// The item's template entry (`containerInfo.itemID`); 0 while unresolved.
    pub item_id: u32,
    /// An `|Hitem:…|h[Name]|h` link once the name is known; Bagnon's search reads it.
    pub link: Option<String>,
    pub locked: bool,
    /// The 1-based live-API inventory slot ids this item could be EQUIPPED into (empty = not
    /// equippable) — the app-resolved "fit rule" decision 0208 phase 1b's cursor arc needs
    /// (`cursor::CursorItem::equip_slots` captures it at pickup). Derived from the item
    /// template's `inventoryType` via `ui_items::find_equip_slot`; the engine holds no item
    /// knowledge of its own.
    pub equip_slots: Vec<u8>,
    /// Whether this item may be placed on an ACTION-BAR slot — app-resolved from the template
    /// exactly like [`Self::equip_slots`] (the engine holds no item knowledge of its own).
    /// `PlaceAction`'s only item filter, byte-read: an on-use spell OR equippable
    /// (`ItemInfo::placeable_on_action_bar`, wow-re `action-item-slot.md` §5 — decision 0666).
    pub bar_placeable: bool,
    /// The instance's live durability `(current, max)` — `ITEM_FIELD_DURABILITY`/`MAXDURABILITY`
    /// off the streamed item object; `None` for indestructible items (max 0) or while the create
    /// hasn't landed. The real-instance tooltip's "Durability X / Y" line reads it (a template
    /// hover keeps the template's full max/max).
    pub durability: Option<(u32, u32)>,
    /// The item's running use-cooldown as `(start_ms on the GetTime clock, duration_ms,
    /// enabled)` — the same app-computed triple [`super::ActionState::cooldown`] carries;
    /// `None` = cold. Stored at push ([`super::UiScript::set_container`]) so
    /// `GetContainerItemCooldown` answers the reference's `(start, duration, enable)`.
    pub cooldown: Option<(i64, u32, bool)>,
    /// Right-clicking reads this item (an instance carrying `ITEM_FIELD_ITEM_TEXT_ID` — a mail
    /// permanent copy). `GetContainerItemInfo`'s `isReadable`; the bag hover shows the Inspect
    /// magnifier off it (ref ContainerFrame.lua l.638 `this.readable → ShowInspectCursor()`),
    /// and the tooltip's WRITTEN_BY/READABLE gates key on it.
    pub readable: bool,
    /// The RESOLVED `ITEM_FIELD_CREATOR` name (app: ask-once name cache) — the tooltip's
    /// "Written by %s" (a letter) / "<Made by %s>" (anything crafted) line. `None` = authorless
    /// or the name query is in flight (no line; the re-push repaints the hover when it lands).
    pub creator: Option<String>,
    /// The instance's `ITEM_FIELD_FLAGS` — the tooltip's openable lock sub-gate reads
    /// UNLOCKED `0x4`, the wrapped-gift arm WRAPPED `0x8` (see
    /// [`super::char_stats::InvSlotView::flags`], the doll twin).
    pub flags: u32,
}

/// One bag's snapshot: its name, capacity, and occupied slots (1-based).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContainerState {
    /// `GetBagName` (the bag item's own name; the backpack is the client-local "Backpack").
    pub name: Option<String>,
    pub num_slots: u32,
    pub slots: std::collections::HashMap<u32, ContainerSlot>,
}

/// One queued backpack pick/place/swap/split: move the item from `(src_bag, src_slot)` to
/// `(dst_bag, dst_slot)` (live-API space, 1-based slots). The app maps a backpack-internal move
/// (both bags 0) onto `CMSG_SWAP_INV_ITEM` player-array slots. `count`: `None` = a whole-stack
/// move/swap (including a same-item merge — the wire tops the stack up itself); `Some(n)` = a
/// split placement (`SplitContainerItem`) the app maps onto `CMSG_SPLIT_ITEM` instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerMove {
    pub src_bag: i64,
    pub src_slot: u32,
    pub dst_bag: i64,
    pub dst_slot: u32,
    pub count: Option<u32>,
}

impl super::UiScript {
    /// Push (or remove, with `None`) one bag's snapshot, keyed by live-API bag id (0 = backpack).
    /// Each slot's cooldown arrives with its absolute start already on the `GetTime` clock (ms) —
    /// storing is a pure unit conversion, the same seam shape as
    /// [`super::UiScript::set_action_state`] — so `GetContainerItemCooldown` answers absolute
    /// `(start, duration, enable)` without drift.
    pub fn set_container(&mut self, bag: i64, state: Option<ContainerState>) {
        let mut model = self.model_mut();
        model.container_cooldowns.retain(|&(b, _), _| b != bag);
        match state {
            Some(s) => {
                for (&slot, cs) in &s.slots {
                    let Some((start_ms, duration_ms, enabled)) = cs.cooldown else {
                        continue;
                    };
                    let abs = (
                        start_ms as f64 / 1000.0,
                        f64::from(duration_ms) / 1000.0,
                        enabled,
                    );
                    model.container_cooldowns.insert((bag, slot), abs);
                }
                model.containers.insert(bag, s);
            }
            None => {
                model.containers.remove(&bag);
            }
        }
    }

    /// Drain the `(bag, slot)` pairs queued by `UseContainerItem` since the last call.
    pub fn take_container_uses(&mut self) -> Vec<(i64, u32)> {
        std::mem::take(&mut self.model_mut().container_uses)
    }

    /// Drain the pick/place/swap moves queued by `PickupContainerItem` since the last call.
    pub fn take_container_moves(&mut self) -> Vec<ContainerMove> {
        std::mem::take(&mut self.model_mut().container_moves)
    }

    /// Drain the `(bag, slot)` sources `AutoEquipCursorItem` queued (decision 0208 phase 1b) —
    /// the app resolves each to wire bag/slot and sends `CMSG_AUTOEQUIP_ITEM`.
    pub fn take_container_autoequips(&mut self) -> Vec<(i64, u32)> {
        std::mem::take(&mut self.model_mut().container_autoequips)
    }

    /// Drain the `(bag, slot)` repair clicks the repair-mode pickup intercept queued — the app
    /// resolves each to its item guid and sends `CMSG_REPAIR_ITEM`.
    pub fn take_container_repairs(&mut self) -> Vec<(i64, u32)> {
        std::mem::take(&mut self.model_mut().container_repairs)
    }

    /// Drain the `(bag, slot, count)` destroys `DeleteCursorItem` queued (`count == 0` = the
    /// whole stack) — the app resolves each to its item guid and sends `CMSG_DESTROYITEM`.
    pub fn take_container_destroys(&mut self) -> Vec<(i64, u32, u32)> {
        std::mem::take(&mut self.model_mut().container_destroys)
    }

    /// The Lua-set **displayed-cursor override** (`Show*SellCursor`/`ShowInspectCursor`/
    /// `ResetCursor`): `None` = show the base (world-classifier) mode, else the mode the FrameXML
    /// cursor family armed. The app overlays it over the world classifier's Point — the 5875
    /// mechanism (wow-re cursor-system.md §7: the displayed mode swaps only when the base mode is
    /// Point; `ResetCursor` restores displayed = base).
    pub fn ui_cursor(&self) -> Option<UiCursorMode> {
        self.model_ref().ui_cursor
    }
}

/// A **displayed-cursor override** the FrameXML cursor family arms (wow-re cursor-system.md §7): the
/// single "displayed mode" (`0xbe2c2c`) the real client swaps to while a UI element wants a non-base
/// cursor, restored to the base mode by `ResetCursor`. The app maps each to the matching
/// `Interface\Cursor\*` art over the world classifier's Point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UiCursorMode {
    /// Buy(3) — the coin/pouch: selling a bag item to a vendor (`ShowContainerSellCursor`, no
    /// affordability gate), or a merchant/buyback item you can afford (`Show*SellCursor`).
    Buy,
    /// UnableBuy(23) — the grayed coin: a merchant/buyback item you can't afford (`Show*SellCursor`,
    /// player coin < price).
    UnableBuy,
    /// Inspect(7) — the magnifier the Ctrl-hover shows over an item (`ShowInspectCursor`).
    Inspect,
}

/// The pick/place/swap gesture behind `PickupContainerItem` (decision 0216 §2, amended by 0218;
/// unit-testable without Lua). Mirrors the real client's single entry point:
/// - an empty cursor over a resolved, UNLOCKED slot picks it up (a locked slot refuses — the real
///   client's refusal; today only the app's `locked` flag drives this, the engine's own
///   pending-move lock lands with slice 2);
/// - holding, a click on the SAME slot cancels (a split carry included);
/// - holding the WHOLE stack (`count: None`), a click elsewhere queues the move and CLEARS —
///   empty, same-item (the wire merges), and different-item (the wire swaps: the displaced item
///   lands where the held one came from) all alike. The displaced item never hops onto the
///   cursor: 0216 §2 shipped that exchange off 0091's gloss; the director's eye caught it and
///   the same-day §5 verdict refuted it at the bytes — no `SetCursorItem` on the place branch
///   (`0x5e0c40`/`0x4f9b30`), `ClearCursor(0)`, one put-down sound (decision 0218; wow-re
///   cursor-dragdrop-slots.md). Bag placements are server-authoritative; only the ACTION bar
///   hops its displaced payload (client-authoritative — the slice-4 note in 0218).
/// - holding a SPLIT carry (`count: Some(n)`), a click onto an empty/unresolved/same-item
///   destination queues the split move and clears; onto a DIFFERENT item it's a no-op (kept —
///   you can't swap a partial stack).
/// - a spell/action payload refuses a bag slot outright (no-op, kept).
///
/// Returns whether the caller should repaint (the source-slot lock or the held payload changed).
fn pickup_container_item(model: &mut super::Model, bag: i64, slot: u32) -> bool {
    match model.cursor.take() {
        None => {
            let picked = model
                .containers
                .get(&bag)
                .and_then(|c| c.slots.get(&slot))
                .filter(|s| s.item_id != 0 && !s.locked)
                .map(|s| CursorItem {
                    bag,
                    slot,
                    item_id: s.item_id,
                    texture: s.texture.clone(),
                    link: s.link.clone(),
                    quality: s.quality,
                    count: None,
                    bar_placeable: s.bar_placeable,
                    equip_slots: s.equip_slots.clone(),
                });
            match picked {
                Some(item) => {
                    model.cursor = Some(CursorPayload::Item(item));
                    cursor::queue_cursor_update(model);
                    cursor::queue_lock_changed(model, bag, slot);
                    true
                }
                None => false,
            }
        }
        Some(CursorPayload::Item(held)) if held.bag == bag && held.slot == slot => {
            cursor::queue_cursor_update(model);
            cursor::queue_lock_changed(model, held.bag, held.slot);
            true
        }
        Some(CursorPayload::Item(held)) => {
            // Cloned out from under `model.containers` so the moves below can borrow `model`
            // mutably; a slot present but unresolved (item_id == 0, an in-flight template
            // answer) reads as absent, same as a truly empty slot.
            let dest = model
                .containers
                .get(&bag)
                .and_then(|c| c.slots.get(&slot))
                .cloned()
                .filter(|d| d.item_id != 0);
            match (held.count, dest) {
                // A split carry onto a DIFFERENT item can't swap a partial stack — no-op, kept.
                (Some(_), Some(d)) if d.item_id != held.item_id => {
                    model.cursor = Some(CursorPayload::Item(held));
                    false
                }
                // Every other placement queues its move and clears: empty and same-item take the
                // carry's count through to the wire (a split placement or a merge the server
                // tops up); a whole-stack place onto a different item is the plain SWAP — the
                // displaced item lands where the held one came from, and the cursor empties
                // (decision 0218, the director's eye over 0216 §2's hop).
                (count, _) => {
                    queue_move(model, &held, bag, slot, count);
                    cursor::queue_cursor_update(model);
                    cursor::queue_lock_changed(model, held.bag, held.slot);
                    true
                }
            }
        }
        Some(other @ (CursorPayload::Spell(_) | CursorPayload::Action(_))) => {
            model.cursor = Some(other);
            false
        }
    }
}

/// Queue one pick/place/swap/split move (the shared tail of every clearing branch above).
fn queue_move(
    model: &mut super::Model,
    held: &CursorItem,
    dst_bag: i64,
    dst_slot: u32,
    count: Option<u32>,
) {
    model.container_moves.push(ContainerMove {
        src_bag: held.bag,
        src_slot: held.slot,
        dst_bag,
        dst_slot,
        count,
    });
}

/// Register the `C_Container` namespace.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let c = lua.create_table()?;

    c.set(
        "GetContainerNumSlots",
        lua.create_function(|lua, bag: i64| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model.containers.get(&bag).map_or(0, |c| c.num_slots))
        })?,
    )?;

    // → free, bagFamily (0 = a plain bag; profession-bag families aren't modeled).
    c.set(
        "GetContainerNumFreeSlots",
        lua.create_function(|lua, bag: i64| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let free = model.containers.get(&bag).map_or(0, |c| {
                c.num_slots
                    .saturating_sub(c.slots.keys().filter(|&&s| s <= c.num_slots).count() as u32)
            });
            Ok((free, 0))
        })?,
    )?;

    c.set(
        "GetBagName",
        lua.create_function(|lua, bag: i64| {
            let name = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model.containers.get(&bag).and_then(|c| c.name.clone())
            };
            match name {
                Some(n) => Ok(Value::String(lua.create_string(&n)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;

    c.set(
        "GetContainerItemID",
        lua.create_function(|lua, (bag, slot): (i64, u32)| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            match model
                .containers
                .get(&bag)
                .and_then(|c| c.slots.get(&slot))
                .map(|s| s.item_id)
            {
                Some(id) if id != 0 => Ok(Value::Integer(i64::from(id))),
                _ => Ok(Value::Nil),
            }
        })?,
    )?;

    // The reference's `GetContainerItemCooldown(bag, slot)` — the bag twin of
    // `GetActionCooldown`, identical conventions: `GetTime`-clock `(start, duration, enable)`,
    // enable 0 = an on-hold record (parked, full duration), and the cold-at-expiry guard so an
    // event-driven re-feed can never replay the finish flash.
    c.set(
        "GetContainerItemCooldown",
        lua.create_function(|lua, (bag, slot): (i64, u32)| {
            let now: f64 = lua.globals().get("__benilla_now").unwrap_or(0.0);
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(match model.container_cooldowns.get(&(bag, slot)) {
                Some(&(start, duration, enabled)) if start + duration > now || !enabled => {
                    (start, duration, i32::from(enabled))
                }
                _ => (0.0, 0.0, 1),
            })
        })?,
    )?;

    c.set(
        "GetContainerItemLink",
        lua.create_function(|lua, (bag, slot): (i64, u32)| {
            let link = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                model
                    .containers
                    .get(&bag)
                    .and_then(|c| c.slots.get(&slot))
                    .and_then(|s| s.link.clone())
            };
            match link {
                Some(l) => Ok(Value::String(lua.create_string(&l)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;

    // → containerInfo table (1.14 shape) or nil for an empty/unknown slot.
    c.set(
        "GetContainerItemInfo",
        lua.create_function(|lua, (bag, slot): (i64, u32)| {
            let (info, held_here) = {
                let model = lua.app_data_ref::<Model>().expect("model app_data");
                let held = matches!(
                    &model.cursor,
                    Some(CursorPayload::Item(c)) if c.bag == bag && c.slot == slot
                );
                (
                    model
                        .containers
                        .get(&bag)
                        .and_then(|c| c.slots.get(&slot))
                        .cloned(),
                    held,
                )
            };
            let Some(s) = info else { return Ok(Value::Nil) };
            let t: Table = lua.create_table()?;
            match &s.texture {
                Some(p) => t.set("iconFileID", p.as_str())?,
                None => t.set("iconFileID", Value::Nil)?,
            }
            t.set("stackCount", s.count)?;
            // The picked-up source slot reads locked (the real client dims it while the cursor
            // carries the item) — derived from the cursor, not a mutated snapshot, so the app's
            // per-frame re-push can't wipe it.
            t.set("isLocked", s.locked || held_here)?;
            match s.quality {
                Some(q) => t.set("quality", q)?,
                None => t.set("quality", Value::Nil)?,
            }
            t.set("isReadable", s.readable)?;
            t.set("hasLoot", false)?;
            match &s.link {
                Some(l) => t.set("hyperlink", l.as_str())?,
                None => t.set("hyperlink", Value::Nil)?,
            }
            t.set("isFiltered", false)?;
            t.set("hasNoValue", false)?;
            t.set("itemID", s.item_id)?;
            t.set("isBound", false)?;
            Ok(Value::Table(t))
        })?,
    )?;

    c.set(
        "UseContainerItem",
        lua.create_function(|lua, (bag, slot, _rest): (i64, u32, mlua::MultiValue)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.container_uses.push((bag, slot));
            Ok(())
        })?,
    )?;

    // The left-click drag gesture: pick up from an occupied slot, or place/swap when holding.
    // Returns whether the caller should repaint (the source-slot lock changed) — the OnClick calls
    // the bag's `_Update` on true so the picked slot dims / un-dims immediately (no server round
    // trip for the local cursor state).
    c.set(
        "PickupContainerItem",
        lua.create_function(|lua, (bag, slot): (i64, u32)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            // The repair-mode intercept lives IN the pickup path, exactly the real client
            // (`0x4f9c7b`, wow-re repair-machinery.md): while the repair cursor is armed, the
            // click means "repair this item" — queued for the app to send — and nothing is
            // picked up. The mode STICKS across clicks (only HideRepairCursor/merchant-close
            // clears it).
            if model.repair_mode {
                model.container_repairs.push((bag, slot));
                return Ok(false);
            }
            // The item-targeted-cast intercept, same seam (byte-VERIFIED — wow-re `tradeskill`
            // TU-F: the enchant pick reroutes the generic bag click inside `PickupContainerItem
            // 0x4f9b30` while the spell-targeting cursor is up): the click means "cast the armed
            // spell AT this item" — queued for the app to complete — and nothing is picked up.
            // One-shot: the app clears the arm on completion/close.
            if model.item_pick_armed {
                model.item_picks.push((bag, slot));
                return Ok(false);
            }
            Ok(pickup_container_item(&mut model, bag, slot))
        })?,
    )?;

    lua.globals().set("C_Container", c)?;

    // The cursor globals (`GetCursorInfo`/`CursorHasItem`/`CursorHasSpell`/`ClearCursor`/
    // `SplitContainerItem`/`DeleteCursorItem` — real client: all top-level, not in C_Container)
    // live in [`super::cursor`], the one payload seam every surface routes through.

    // ShowContainerSellCursor(bag, slot) — arm the pouch cursor for a sellable hover (5875
    // `0x4fa460`, wow-re cursor-system.md §7: Buy(3) only when the slot actually holds an item —
    // an empty slot leaves the cursor unchanged; no Unable twin, no SellPrice check — selling
    // never grays). The spell-targeting and base-mode-Point gates live app-side where those states
    // are known.
    lua.globals().set(
        "ShowContainerSellCursor",
        lua.create_function(|lua, (bag, slot): (i64, u32)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            let occupied = model
                .containers
                .get(&bag)
                .and_then(|c| c.slots.get(&slot))
                .is_some();
            if occupied {
                model.ui_cursor = Some(UiCursorMode::Buy);
            }
            Ok(())
        })?,
    )?;
    // ShowInspectCursor() — the Ctrl-hover magnifier (5875 `0x48ac60`: an unconditional
    // `CursorSetMode(Inspect=7)`). Shared across surfaces (the merchant window's Ctrl-hover, a bag
    // item's Ctrl-hover, the generic UIParent item hover), so it lives here beside `ResetCursor`
    // rather than in any one seam. It takes no arguments and reads no state.
    lua.globals().set(
        "ShowInspectCursor",
        lua.create_function(|lua, ()| {
            lua.app_data_mut::<Model>()
                .expect("model app_data")
                .ui_cursor = Some(UiCursorMode::Inspect);
            Ok(())
        })?,
    )?;
    // ResetCursor — displayed mode back to the base (the world classifier's) mode (5875
    // `0x48ac70` → `0x523d30`): clear the whole override, whichever `Show*Cursor` armed it.
    lua.globals().set(
        "ResetCursor",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            model.ui_cursor = None;
            Ok(())
        })?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ContainerMove, ContainerSlot, ContainerState};
    use crate::script::UiScript;

    fn backpack() -> ContainerState {
        let mut slots = std::collections::HashMap::new();
        slots.insert(
            1,
            ContainerSlot {
                bar_placeable: true,
                durability: None,
                texture: Some("Interface\\Icons\\INV_Misc_Food_16".into()),
                count: 5,
                quality: Some(1),
                item_id: 117,
                link: Some("|cffffffff|Hitem:117|h[Tough Jerky]|h|r".into()),
                locked: false,
                equip_slots: Vec::new(),
                cooldown: None,
                readable: false,
                creator: None,
                flags: 0,
            },
        );
        // An in-flight slot: the create arrived, the template answer hasn't.
        slots.insert(3, ContainerSlot::default());
        // A readable letter (a mail permanent copy — instance item-text): isReadable true.
        slots.insert(
            4,
            ContainerSlot {
                item_id: 8383,
                count: 1,
                readable: true,
                ..Default::default()
            },
        );
        ContainerState {
            name: Some("Backpack".into()),
            num_slots: 16,
            slots,
        }
    }

    #[test]
    fn container_snapshot_reads() {
        let mut s = UiScript::new().unwrap();
        // No bag pushed: capacity 0, info nil.
        assert_eq!(
            s.eval::<i64>("return C_Container.GetContainerNumSlots(0)")
                .unwrap(),
            0
        );
        assert!(s
            .eval::<bool>("return C_Container.GetContainerItemInfo(0, 1) == nil")
            .unwrap());

        s.set_container(0, Some(backpack()));
        assert_eq!(
            s.eval::<i64>("return C_Container.GetContainerNumSlots(0)")
                .unwrap(),
            16
        );
        assert_eq!(
            s.eval::<i64>("return (C_Container.GetContainerNumFreeSlots(0))")
                .unwrap(),
            13
        );
        assert_eq!(
            s.eval::<String>("return C_Container.GetBagName(0)")
                .unwrap(),
            "Backpack"
        );
        assert_eq!(
            s.eval::<i64>("return C_Container.GetContainerItemID(0, 1)")
                .unwrap(),
            117
        );
        let (icon, count, quality, item_id) = s
            .eval::<(String, i64, i64, i64)>(
                "local i = C_Container.GetContainerItemInfo(0, 1)\n\
                 return i.iconFileID, i.stackCount, i.quality, i.itemID",
            )
            .unwrap();
        assert_eq!(icon, "Interface\\Icons\\INV_Misc_Food_16");
        assert_eq!((count, quality, item_id), (5, 1, 117));
        assert!(s
            .eval::<bool>("return C_Container.GetContainerItemLink(0, 1) ~= nil")
            .unwrap());
        // isReadable mirrors the slot's readable bit (the letter in 4, not the jerky in 1).
        assert!(!s
            .eval::<bool>("return C_Container.GetContainerItemInfo(0, 1).isReadable")
            .unwrap());
        assert!(s
            .eval::<bool>("return C_Container.GetContainerItemInfo(0, 4).isReadable")
            .unwrap());

        // The in-flight slot answers a table with nil icon/quality, id nil (0 = unresolved).
        assert!(s
            .eval::<bool>(
                "local i = C_Container.GetContainerItemInfo(0, 3)\n\
                 return i ~= nil and i.iconFileID == nil and i.quality == nil",
            )
            .unwrap());
        assert!(s
            .eval::<bool>("return C_Container.GetContainerItemID(0, 3) == nil")
            .unwrap());
        // Empty slot: nil.
        assert!(s
            .eval::<bool>("return C_Container.GetContainerItemInfo(0, 2) == nil")
            .unwrap());
    }

    #[test]
    fn use_container_item_queues_intents() {
        let mut s = UiScript::new().unwrap();
        s.set_container(0, Some(backpack()));
        s.run("C_Container.UseContainerItem(0, 1)").unwrap();
        s.run("C_Container.UseContainerItem(0, 3, 'target')")
            .unwrap();
        assert_eq!(s.take_container_uses(), vec![(0, 1), (0, 3)]);
        assert!(s.take_container_uses().is_empty(), "drained");
    }

    #[test]
    fn pickup_then_place_swaps_and_clears_cursor() {
        let mut s = UiScript::new().unwrap();
        s.set_container(0, Some(backpack())); // slot 1 = Tough Jerky (resolved), slot 3 = in-flight
        assert!(s.cursor_item().is_none());
        assert!(!s.eval::<bool>("return CursorHasItem()").unwrap());

        // Pick up slot 1 → cursor holds it, the source slot reads locked, no move queued yet.
        assert!(s
            .eval::<bool>("return C_Container.PickupContainerItem(0, 1)")
            .unwrap());
        let held = s.cursor_item().expect("cursor holds the picked item");
        assert_eq!((held.bag, held.slot, held.item_id), (0, 1, 117));
        assert_eq!(
            held.texture.as_deref(),
            Some("Interface\\Icons\\INV_Misc_Food_16")
        );
        assert!(s.eval::<bool>("return CursorHasItem()").unwrap());
        assert!(s
            .eval::<bool>("local i = C_Container.GetContainerItemInfo(0, 1) return i.isLocked")
            .unwrap());
        assert!(s.take_container_moves().is_empty());

        // GetCursorInfo reports the item.
        let (kind, id) = s
            .eval::<(String, i64)>("local k, id = GetCursorInfo() return k, id")
            .unwrap();
        assert_eq!((kind.as_str(), id), ("item", 117));

        // Place onto slot 5 → a move (0,1)->(0,5) queues, cursor clears, source un-locks.
        assert!(s
            .eval::<bool>("return C_Container.PickupContainerItem(0, 5)")
            .unwrap());
        assert!(s.cursor_item().is_none());
        assert!(!s.eval::<bool>("return CursorHasItem()").unwrap());
        assert_eq!(
            s.take_container_moves(),
            vec![ContainerMove {
                src_bag: 0,
                src_slot: 1,
                dst_bag: 0,
                dst_slot: 5,
                count: None,
            }]
        );
        assert!(!s
            .eval::<bool>("local i = C_Container.GetContainerItemInfo(0, 1) return i.isLocked")
            .unwrap());
    }

    /// The sibling of the test above onto an OCCUPIED, DIFFERENT-item slot: the plain SWAP
    /// (decision 0218, superseding 0216 §2's hop — byte-verified: no `SetCursorItem` on the place
    /// branch; the wire swaps and the cursor empties, exactly like an empty destination). The
    /// displaced item never lands on the cursor.
    #[test]
    fn pickup_place_onto_occupied_different_item_swaps_and_clears() {
        let mut s = UiScript::new().unwrap();
        let mut state = backpack(); // slot 1 = item 117 (A)
        state.slots.insert(
            5,
            ContainerSlot {
                bar_placeable: true,
                durability: None,
                texture: Some("Interface\\Icons\\INV_Misc_Gem_01".into()),
                count: 1,
                quality: Some(2),
                item_id: 200, // item B, a different id
                link: Some("|Hitem:200|h[Shiny Gem]|h".into()),
                locked: false,
                equip_slots: Vec::new(),
                cooldown: None,
                readable: false,
                creator: None,
                flags: 0,
            },
        );
        s.set_container(0, Some(state));

        assert!(s
            .eval::<bool>("return C_Container.PickupContainerItem(0, 1)")
            .unwrap());
        assert_eq!(s.cursor_item().unwrap().item_id, 117);

        // Place A onto occupied slot 5 (item B) → move (1→5) queues, the cursor EMPTIES (the
        // server's swap lands B in slot 1), and the source un-locks.
        assert!(s
            .eval::<bool>("return C_Container.PickupContainerItem(0, 5)")
            .unwrap());
        assert!(
            s.cursor_item().is_none(),
            "a swap clears the cursor — the displaced item never hops on"
        );
        assert_eq!(
            s.take_container_moves(),
            vec![ContainerMove {
                src_bag: 0,
                src_slot: 1,
                dst_bag: 0,
                dst_slot: 5,
                count: None,
            }]
        );
        assert!(!s
            .eval::<bool>("local i = C_Container.GetContainerItemInfo(0, 1) return i.isLocked")
            .unwrap());
    }

    /// Placing onto a SAME-item_id destination merges (the wire tops the stack up itself) —
    /// still a plain clear-on-move.
    #[test]
    fn pickup_place_onto_same_item_merges_and_clears() {
        let mut s = UiScript::new().unwrap();
        let mut state = backpack();
        state.slots.insert(
            5,
            ContainerSlot {
                durability: None,
                item_id: 117, // same id as slot 1
                count: 2,
                ..Default::default()
            },
        );
        s.set_container(0, Some(state));

        s.run("C_Container.PickupContainerItem(0, 1)").unwrap();
        assert!(s
            .eval::<bool>("return C_Container.PickupContainerItem(0, 5)")
            .unwrap());
        assert!(
            s.cursor_item().is_none(),
            "same-item merge clears the cursor"
        );
        assert_eq!(
            s.take_container_moves(),
            vec![ContainerMove {
                src_bag: 0,
                src_slot: 1,
                dst_bag: 0,
                dst_slot: 5,
                count: None,
            }]
        );
    }

    #[test]
    fn pickup_refuses_a_locked_slot() {
        let mut s = UiScript::new().unwrap();
        let mut state = backpack();
        state.slots.get_mut(&1).unwrap().locked = true;
        s.set_container(0, Some(state));

        assert!(!s
            .eval::<bool>("return C_Container.PickupContainerItem(0, 1)")
            .unwrap());
        assert!(s.cursor_item().is_none());
    }

    #[test]
    fn split_container_item_carries_a_partial_stack() {
        let mut s = UiScript::new().unwrap();
        let mut state = backpack(); // slot 1: a 5-stack of item 117
        state.slots.insert(
            7,
            ContainerSlot {
                durability: None,
                item_id: 117,
                count: 1,
                ..Default::default()
            },
        );
        state.slots.insert(
            9,
            ContainerSlot {
                durability: None,
                item_id: 999, // a different item
                count: 1,
                ..Default::default()
            },
        );
        s.set_container(0, Some(state));

        assert!(s
            .eval::<bool>("return SplitContainerItem(0, 1, 3)")
            .unwrap());
        let held = s.cursor_item().expect("the split carry");
        assert_eq!(
            (held.bag, held.slot, held.item_id, held.count),
            (0, 1, 117, Some(3))
        );

        // Placed onto an empty slot: the move carries the split count, and clears.
        assert!(s
            .eval::<bool>("return C_Container.PickupContainerItem(0, 2)")
            .unwrap());
        assert!(s.cursor_item().is_none());
        assert_eq!(
            s.take_container_moves(),
            vec![ContainerMove {
                src_bag: 0,
                src_slot: 1,
                dst_bag: 0,
                dst_slot: 2,
                count: Some(3),
            }]
        );

        // Re-split, then place onto a DIFFERENT item: no-op, the split carry is kept.
        s.run("SplitContainerItem(0, 1, 3)").unwrap();
        assert!(!s
            .eval::<bool>("return C_Container.PickupContainerItem(0, 9)")
            .unwrap());
        let held = s.cursor_item().expect("kept — can't swap a partial stack");
        assert_eq!(held.count, Some(3));
        assert!(s.take_container_moves().is_empty());

        // Placed onto a SAME-item slot: the server merges — the move queues, cursor clears.
        assert!(s
            .eval::<bool>("return C_Container.PickupContainerItem(0, 7)")
            .unwrap());
        assert!(s.cursor_item().is_none());
        assert_eq!(
            s.take_container_moves(),
            vec![ContainerMove {
                src_bag: 0,
                src_slot: 1,
                dst_bag: 0,
                dst_slot: 7,
                count: Some(3),
            }]
        );
    }

    #[test]
    fn split_of_the_whole_stack_is_a_plain_pickup() {
        let mut s = UiScript::new().unwrap();
        s.set_container(0, Some(backpack())); // slot 1: a 5-stack
        assert!(s
            .eval::<bool>("return SplitContainerItem(0, 1, 5)")
            .unwrap());
        assert_eq!(s.cursor_item().unwrap().count, None);
        // Split of MORE than the stack clamps the same way.
        s.run("ClearCursor()").unwrap();
        assert!(s
            .eval::<bool>("return SplitContainerItem(0, 1, 99)")
            .unwrap());
        assert_eq!(s.cursor_item().unwrap().count, None);
    }

    #[test]
    fn pickup_same_slot_cancels_no_move() {
        let mut s = UiScript::new().unwrap();
        s.set_container(0, Some(backpack()));
        assert!(s
            .eval::<bool>("return C_Container.PickupContainerItem(0, 1)")
            .unwrap());
        assert!(s.cursor_item().is_some());
        // Click the same slot again → cancel: cursor clears, nothing queued.
        assert!(s
            .eval::<bool>("return C_Container.PickupContainerItem(0, 1)")
            .unwrap());
        assert!(s.cursor_item().is_none());
        assert!(s.take_container_moves().is_empty());
    }

    #[test]
    fn pickup_empty_slot_holds_nothing() {
        let mut s = UiScript::new().unwrap();
        s.set_container(0, Some(backpack()));
        // Slot 2 is empty, slot 3 is in-flight (unresolved, item_id 0) — neither is pickable.
        assert!(!s
            .eval::<bool>("return C_Container.PickupContainerItem(0, 2)")
            .unwrap());
        assert!(s.cursor_item().is_none());
        assert!(!s
            .eval::<bool>("return C_Container.PickupContainerItem(0, 3)")
            .unwrap());
        assert!(s.cursor_item().is_none());
    }

    #[test]
    fn clear_cursor_drops_the_held_item() {
        let mut s = UiScript::new().unwrap();
        s.set_container(0, Some(backpack()));
        s.run("C_Container.PickupContainerItem(0, 1)").unwrap();
        assert!(s.cursor_item().is_some());
        s.run("ClearCursor()").unwrap();
        assert!(s.cursor_item().is_none());
        assert!(s.take_container_moves().is_empty());
    }

    #[test]
    fn removing_a_bag_empties_it() {
        let mut s = UiScript::new().unwrap();
        s.set_container(2, Some(backpack()));
        s.set_container(2, None);
        assert_eq!(
            s.eval::<i64>("return C_Container.GetContainerNumSlots(2)")
                .unwrap(),
            0
        );
    }

    /// `GetContainerItemCooldown` — the `GetActionCooldown` conventions on the bag twin: the
    /// absolute-start triple stored verbatim in `GetTime` seconds, the cold `(0, 0, 1)` shape
    /// for no-cooldown slots, the stale-refeed guard at expiry, and the re-push replacing a
    /// bag's prior triples.
    #[test]
    fn container_item_cooldown_reads_the_gettime_triple() {
        let mut s = UiScript::new().unwrap();
        s.tick(100.0); // an arbitrary clock epoch
        let mut state = backpack();
        // Slot 1's item is 4 s into a 10 s use-cooldown: started at GetTime 96.
        state.slots.get_mut(&1).unwrap().cooldown = Some((96_000, 10_000, true));
        s.set_container(0, Some(state));

        // The pushed absolute start reads back verbatim in seconds.
        assert!(s
            .eval::<bool>(
                "local s, d, e = C_Container.GetContainerItemCooldown(0, 1)\n\
                 return s == 96 and d == 10 and e == 1"
            )
            .unwrap());
        // A slot with no cooldown reads cold.
        assert!(s
            .eval::<bool>(
                "local s, d, e = C_Container.GetContainerItemCooldown(0, 3)\n\
                 return s == 0 and d == 0 and e == 1"
            )
            .unwrap());
        // Past expiry, the same stored triple reads cold — the no-replayed-flash guard.
        s.tick(7.0);
        assert!(s
            .eval::<bool>(
                "local s, d, e = C_Container.GetContainerItemCooldown(0, 1)\n\
                 return s == 0 and d == 0 and e == 1"
            )
            .unwrap());
        // A re-push without the cooldown clears the stored triple.
        s.set_container(0, Some(backpack()));
        assert!(s
            .eval::<bool>(
                "local s, d = C_Container.GetContainerItemCooldown(0, 1)\n\
                 return s == 0 and d == 0"
            )
            .unwrap());
    }
}
