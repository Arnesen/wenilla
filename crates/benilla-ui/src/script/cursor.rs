//! The cursor payload (decision 0216, slice 1) — the client's payload-mode global `[0xb4d900]`
//! as a typed enum, and the drag-gesture mechanics (`RegisterForDrag`/`OnDragStart`/`OnDragStop`/
//! `OnReceiveDrag`) that transition it. One seam for every surface — bags, the paper doll
//! ([`doll`], decision 0208 phase 1b), action bars ([`bar`], decision 0216 §7/0218 §4), and the
//! spellbook ([`super::spellbook`]'s `PickupSpell`, slice 5 — the [`CursorSpell`] producer) — so
//! sounds, `CURSOR_UPDATE`, grid events, and lock display can't drift apart per window.
//!
//! [`container`](super::container)'s `pickup_container_item`, [`doll`]'s
//! `pickup_inventory_item`, and [`bar`]'s `pickup_action`/`place_action` (the surface-specific
//! transition bodies) all route through [`queue_cursor_update`]/[`queue_lock_changed`], the one
//! place those events fire from. [`bar`], [`doll`], and [`drag`] are split out purely for size —
//! this file keeps the payload types, the shared transition seam, and `install`.

use mlua::{Lua, Value};

use super::{Model, ScriptValue};

mod bar;
mod doll;
mod drag;

pub(crate) use bar::place_action;
pub(crate) use drag::{arm_drag, maybe_start_drag, take_drag, DragGesture, DragRelease};

/// The sentinel bag id that folds the player's EQUIPPED slots into the ONE payload space
/// (decision 0216 §1, extended to the paper doll by decision 0208 phase 1b): a
/// [`CursorItem`]/[`super::container::ContainerMove`] whose `bag` is `EQUIPMENT_BAG` addresses
/// `slot` as a 1-based live-API inventory slot id — `GetInventorySlotInfo`'s own numbering
/// (HeadSlot=1 … TabardSlot=19, `char_stats::SLOT_INFO`'s convention), not a bag's contents.
/// Disjoint from every real bag id — including the ERA's own negative container ids, which are
/// real API surface (−1 = `BANK_CONTAINER`, decision 0604; −2 = the keyring) — so a `match` on
/// `bag` can never confuse the spaces (its original value −1 collided with the bank the day the
/// bank landed; the sentinel is engine-internal and never crosses the Lua boundary as a value,
/// verified across the XML fleet at the change). Ammo (id 0) stays OUT of this space — a named
/// deferral ([`doll::pickup_inventory_item`]'s own range guard).
pub const EQUIPMENT_BAG: i64 = -100;

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The payload
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// What the cursor carries — the client's payload-mode global [0xb4d900] as a typed enum
/// (wow-re cursor-dragdrop-payload.md §1: 1 = live item, 3 = spell; our Action arm is the
/// client's bar-slot pickup; the money/macro/preview arms stay unbuilt). One transition seam for
/// every surface, so sounds, CURSOR_UPDATE, and lock display can't drift apart per window
/// (decision 0216).
#[derive(Clone, Debug, PartialEq)]
pub enum CursorPayload {
    Item(CursorItem),
    Spell(CursorSpell),
    Action(CursorAction),
}

/// The item currently held on the cursor (`PickupContainerItem`/`SplitContainerItem`/
/// `PickupInventoryItem` set it). The real client's drag state: it names *where the item was
/// picked from* (so a second click routes the swap) and carries the icon the app draws at the
/// mouse. Purely transient — cleared on place/cancel; the server's field updates do the actual
/// move.
#[derive(Clone, Debug, PartialEq)]
pub struct CursorItem {
    /// Live-API bag id the item was picked up from (0 = backpack, 1..=4 an equipped bag,
    /// [`EQUIPMENT_BAG`] the player's own equipped slots).
    pub bag: i64,
    /// 1-based slot the item was picked up from.
    pub slot: u32,
    /// The picked item's template entry (`GetCursorInfo`'s itemID).
    pub item_id: u32,
    /// The icon texture path to draw at the mouse (`Interface\Icons\…`); `None` if unresolved.
    pub texture: Option<String>,
    /// The item link (`GetCursorInfo`'s itemLink), when known.
    pub link: Option<String>,
    /// A split carry (`SplitContainerItem` picked up `n` of the stack); `None` = the whole stack.
    pub count: Option<u32>,
    /// The item's quality (0..6) at pickup, carried so `DELETE_ITEM_CONFIRM` (a world drop) can
    /// report it without a container round-trip.
    pub quality: Option<u32>,
    /// The 1-based live-API inventory slot ids this item could be EQUIPPED into (empty = not
    /// equippable), captured at pickup from the source's own `equip_slots` (the container slot's
    /// or the doll slot view's — decision 0208 phase 1b's "the fit rule").
    /// [`doll::pickup_inventory_item`] reads it to decide a place-onto-doll-slot's fit;
    /// [`doll::cursor_can_go_in_slot`] serves it straight to `CURSOR_UPDATE`'s highlight.
    pub equip_slots: Vec<u8>,
    /// Whether this item may be placed on an ACTION-BAR slot, captured at pickup from the
    /// source's own `bar_placeable` — `PlaceAction`'s only item filter (decision 0666).
    pub bar_placeable: bool,
}

/// A spell payload ([`super::spellbook`]'s `PickupSpell` produces it): the spellbook slot it was
/// picked from, its book (`"spell"`/`"pet"`, the Era `GetCursorInfo` shape), and the resolved
/// spell id.
#[derive(Clone, Debug, PartialEq)]
pub struct CursorSpell {
    pub book_slot: u32,
    pub book_type: String,
    pub spell_id: u32,
    pub texture: Option<String>,
    /// `Attributes & 0x40` (`SPELL_ATTR_PASSIVE`) — a passive cannot go on the action bar
    /// (`PlaceAction`'s other filter, `0x4e63ad`; decision 0666).
    pub passive: bool,
}

/// An action-slot payload ([`bar`]'s `PickupAction`/`place_action` hop produces it): the source
/// action slot, the packed action's kind byte (SPELL/MACRO/ITEM — decision 0216's
/// `CMSG_SET_ACTION_BUTTON` type byte), and the action id itself.
#[derive(Clone, Debug, PartialEq)]
pub struct CursorAction {
    pub src_slot: u32,
    pub kind: u8,
    pub action: u32,
    pub texture: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The one transition seam: CURSOR_UPDATE / ITEM_LOCK_CHANGED / DELETE_ITEM_CONFIRM
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// Queue `CURSOR_UPDATE` — fired on EVERY payload transition (pickup, place/clear, cancel,
/// `ClearCursor`, `DeleteCursorItem`, a world-drop clear). One push per transition.
///
/// Also derives `ACTIONBAR_SHOWGRID`/`ACTIONBAR_HIDEGRID` (decision 0216 §7) off
/// [`Model::cursor_grid_shown`]'s mirror against the CURRENT `model.cursor` (already the
/// post-transition state at every call site — every caller mutates `model.cursor` before calling
/// this): a None→Some edge (any payload arm, any surface — bags/doll/actions alike) shows the
/// bar's drop grid, Some→None hides it, Some→Some (the action hop) touches neither, so one
/// gesture never churns HIDE+SHOW. This is the one seam every pickup/place/clear already routes
/// through, so no call site needs to know about grid events at all.
pub(crate) fn queue_cursor_update(model: &mut Model) {
    model
        .pending_events
        .push(("CURSOR_UPDATE".to_string(), Vec::new()));
    let now_held = model.cursor.is_some();
    if now_held != model.cursor_grid_shown {
        model.cursor_grid_shown = now_held;
        let event = if now_held {
            "ACTIONBAR_SHOWGRID"
        } else {
            "ACTIONBAR_HIDEGRID"
        };
        model.pending_events.push((event.to_string(), Vec::new()));
    }
}

/// Queue `ITEM_LOCK_CHANGED(bag, slot)` — a pickup locking a source slot, or a place/cancel/
/// clear/destroy unlocking it (decision 0216 §4: the engine-derived held-here lock,
/// `container.rs`'s `GetContainerItemInfo` `held_here` check).
pub(crate) fn queue_lock_changed(model: &mut Model, bag: i64, slot: u32) {
    model.pending_events.push((
        "ITEM_LOCK_CHANGED".to_string(),
        vec![ScriptValue::Int(bag), ScriptValue::Int(i64::from(slot))],
    ));
}

/// Queue `DELETE_ITEM_CONFIRM(name, quality)` for an item payload dropped on the world (the popup
/// flow transcribed from `StaticPopup.lua`, decision 0216 §3). `name` is parsed out of the link's
/// `[...]` segment (empty string with no link); the payload is left untouched here — the popup's
/// `OnAccept`/`OnCancel` (`DeleteCursorItem`/`ClearCursor`) own clearing it.
fn queue_delete_item_confirm(model: &mut Model, item: &CursorItem) {
    let name = item_link_name(item.link.as_deref());
    let quality = item.quality.unwrap_or(0);
    model.pending_events.push((
        "DELETE_ITEM_CONFIRM".to_string(),
        vec![ScriptValue::Str(name), ScriptValue::Int(i64::from(quality))],
    ));
}

/// Parse an item link's display name out of its `|h[Name]|h` segment; empty with no link or an
/// unrecognized shape.
pub(super) fn item_link_name(link: Option<&str>) -> String {
    link.and_then(|l| l.split_once('['))
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(name, _)| name.to_string())
        .unwrap_or_default()
}

/// `ClearCursor()` — drops whatever the cursor holds, any arm (the right-click/ESC cancel). An
/// item payload also un-locks its source slot; an already-empty cursor is not a transition (no
/// event).
pub(crate) fn clear_cursor(model: &mut Model) {
    match model.cursor.take() {
        Some(CursorPayload::Item(item)) => {
            queue_cursor_update(model);
            queue_lock_changed(model, item.bag, item.slot);
        }
        Some(CursorPayload::Spell(_) | CursorPayload::Action(_)) => {
            queue_cursor_update(model);
        }
        None => {}
    }
}

/// What the app's world pick resolves under the cursor this frame — the reference's click-time
/// pick state `[this+0x350]` (0 = nothing / 1 = terrain / 2 = object; `0x481f60`, decisions
/// 0571 + 0574). Fed per frame by the app ([`super::UiScript::set_world_pick`]; stays
/// [`WorldPick::Nothing`] in tests/captures). Routes the left world-drop
/// ([`world_drop_click`]): over an `Object` no payload drops at all (the object leg `0x492ce0`
/// clears only the displayId-PREVIEW gate `[0xb4b41c]` — an arm benilla doesn't carry — and
/// dispatches SELECT/INTERACT with the payload still held); over `Terrain` an ITEM drops (the
/// `DELETE_ITEM_CONFIRM` popup via `0x5e0320`) and a spell/action payload clears silently
/// (decision 0843's deliberate divergence — the reference keeps it there); over `Nothing` the
/// item pops and any other payload silently clears (`0x492d30`).
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum WorldPick {
    /// No world hit at all (sky / beyond the world) — the reference's state 0.
    #[default]
    Nothing,
    /// The cursor ray hits the world (terrain/WMO/doodad) but no unit or GameObject — state 1.
    Terrain,
    /// A unit or GameObject is the pick — state 2.
    Object,
}

/// A world drop: a completed left CLICK on the game world while a payload is held — press and
/// release both over no frame, no drag (the byte-verified trigger, decision 0218: the client's
/// `0x495300` runs on the WorldFrame click release only; a drag released over the world routes
/// as a drag and keeps carrying) — routed by [`Model::world_pick`] (decisions 0571 + 0574,
/// byte-verified wow-re cursor-dragdrop-payload.md §11):
///
/// - `Object`: NOTHING drops — the object leg (`0x492ce0`) keeps every real payload and
///   dispatches SELECT instead.
/// - `Terrain`: an item payload fires `DELETE_ITEM_CONFIRM(name, quality)` and STAYS held (the
///   reference popup's `OnAccept` calls `DeleteCursorItem`, `OnCancel` `ClearCursor`, and its
///   `OnUpdate` auto-hides when the cursor empties — the engine must not pre-clear); a
///   spell/action payload clears silently — a DELIBERATE divergence (decision 0843, the
///   director's call): the reference's terrain leg keeps a non-item payload on the left click
///   (`0x492c90`/`0x5e0320` clear non-items only on the right button), which leaves a spell
///   stuck to the cursor with no left-handed way off it.
/// - `Nothing`: the item pops the same popup; any other payload clears silently (`0x492d30`'s
///   non-item arm — here the reference agrees).
///
/// Returns whether the click was consumed as a drop (an empty cursor or an `Object` pick
/// consume nothing).
pub(crate) fn world_drop_click(model: &mut Model) -> bool {
    match (&model.cursor, model.world_pick) {
        (Some(CursorPayload::Item(item)), WorldPick::Terrain | WorldPick::Nothing) => {
            let item = item.clone();
            queue_delete_item_confirm(model, &item);
            true
        }
        (
            Some(CursorPayload::Spell(_) | CursorPayload::Action(_)),
            WorldPick::Terrain | WorldPick::Nothing,
        ) => {
            clear_cursor(model);
            true
        }
        _ => false,
    }
}

/// `SplitContainerItem(bag, slot, count)` — a stack-split pickup (`StackSplitFrame.lua`'s
/// `SplitStack`): only from an EMPTY cursor, onto a resolved and unlocked slot, `count >= 1`.
/// `count` at or past the whole stack degrades to a plain whole-stack pickup (`count: None` — the
/// client never "splits" a full stack; vmangos rejects `count == stack` as cheat). Returns
/// whether a payload was picked up (the repaint gate, matching `pickup_container_item`'s
/// contract).
pub(crate) fn split_container_item(model: &mut Model, bag: i64, slot: u32, count: i64) -> bool {
    if model.cursor.is_some() || count < 1 {
        return false;
    }
    let Some(s) = model.containers.get(&bag).and_then(|c| c.slots.get(&slot)) else {
        return false;
    };
    if s.item_id == 0 || s.locked {
        return false;
    }
    let n = count.min(i64::from(s.count)) as u32;
    let whole = n >= s.count;
    let item = CursorItem {
        bag,
        slot,
        item_id: s.item_id,
        texture: s.texture.clone(),
        link: s.link.clone(),
        quality: s.quality,
        count: if whole { None } else { Some(n) },
        bar_placeable: s.bar_placeable,
        equip_slots: s.equip_slots.clone(),
    };
    model.cursor = Some(CursorPayload::Item(item));
    queue_cursor_update(model);
    queue_lock_changed(model, bag, slot);
    true
}

/// `DeleteCursorItem()` — the delete popup's `OnAccept` (decision 0216 §3): an Item payload
/// queues its `(bag, slot, count)` destroy (`count == 0` = the whole stack) and clears; any other
/// payload (or an empty cursor) is a no-op — the client's contract for a stale/mismatched confirm.
pub(crate) fn delete_cursor_item(model: &mut Model) {
    let Some(CursorPayload::Item(item)) = &model.cursor else {
        return;
    };
    let item = item.clone();
    model.cursor = None;
    model
        .container_destroys
        .push((item.bag, item.slot, item.count.unwrap_or(0)));
    queue_cursor_update(model);
    queue_lock_changed(model, item.bag, item.slot);
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// UiScript accessors
// ─────────────────────────────────────────────────────────────────────────────────────────────

impl super::UiScript {
    /// The item currently held on the cursor, or `None` — the Item-arm projection of
    /// [`UiScript::cursor_payload`], kept for the app's existing extract/sound watchers
    /// (`ui_script/extract.rs`, `sound/ui.rs`), which only ever cared about items.
    pub fn cursor_item(&self) -> Option<CursorItem> {
        match self.model_ref().cursor.clone() {
            Some(CursorPayload::Item(item)) => Some(item),
            _ => None,
        }
    }

    /// The full cursor payload, any arm — what the app's hardware-cursor drive
    /// (`benilla::cursor`) and capture stand-in draw, and what the world-click consumers gate on.
    pub fn cursor_payload(&self) -> Option<CursorPayload> {
        self.model_ref().cursor.clone()
    }

    /// Feed: what the app's world pick resolves under the cursor this frame
    /// ([`Model::world_pick`], decisions 0571 + 0574) — routes [`world_drop_click`]'s legs
    /// (object keeps everything, terrain drops items only, nothing drops any arm).
    pub fn set_world_pick(&mut self, pick: WorldPick) {
        self.model_mut().world_pick = pick;
    }

    /// `ClearCursor()`'s Rust seam — drops whatever the cursor holds, any arm, silently (fires
    /// `CURSOR_UPDATE` + the item source un-lock, exactly the Lua `ClearCursor`). The app's
    /// world-click router calls it for a clean RIGHT-click over empty world (`0x492c90`/
    /// `0x492d30`'s action-4 leg: `ClearCursor 0x495190(1,1)` unconditionally — decision 0571).
    pub fn clear_cursor_payload(&mut self) {
        clear_cursor(&mut self.model_mut());
    }

    /// Test-only: set the cursor payload directly, bypassing every Lua binding — drives the
    /// Spell/Action arms in tests before a spellbook/action-bar slice can produce them.
    #[cfg(test)]
    pub(crate) fn set_cursor_for_test(&mut self, payload: CursorPayload) {
        self.model_mut().cursor = Some(payload);
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Lua globals: GetCursorInfo / CursorHasItem / CursorHasSpell / ClearCursor / SplitContainerItem /
// DeleteCursorItem — all top-level (the real client's `GetCursorInfo` &c. are not namespaced).
// The paper-doll globals (`PickupInventoryItem` &c.) install from [`doll`]; the action-bar
// globals (`PickupAction`/`PlaceAction`) install from [`bar`].
// ─────────────────────────────────────────────────────────────────────────────────────────────

pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // GetCursorInfo() → per arm: Item ("item", itemID, itemLink); Spell ("spell", book_slot,
    // book_type, spell_id) — the Era shape; Action ("action", src_slot). Empty ⇒ all nils. A
    // fixed 4-return shape (padded with nil past each arm's own fields) so one function signature
    // covers every arm; callers destructuring fewer names simply ignore the tail.
    g.set(
        "GetCursorInfo",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            let str_or_nil = |lua: &Lua, s: &Option<String>| -> mlua::Result<Value> {
                match s {
                    Some(s) => Ok(Value::String(lua.create_string(s)?)),
                    None => Ok(Value::Nil),
                }
            };
            match &model.cursor {
                Some(CursorPayload::Item(c)) => Ok((
                    Value::String(lua.create_string("item")?),
                    Value::Integer(i64::from(c.item_id)),
                    str_or_nil(lua, &c.link)?,
                    Value::Nil,
                )),
                Some(CursorPayload::Spell(s)) => Ok((
                    Value::String(lua.create_string("spell")?),
                    Value::Integer(i64::from(s.book_slot)),
                    Value::String(lua.create_string(&s.book_type)?),
                    Value::Integer(i64::from(s.spell_id)),
                )),
                Some(CursorPayload::Action(a)) => Ok((
                    Value::String(lua.create_string("action")?),
                    Value::Integer(i64::from(a.src_slot)),
                    Value::Nil,
                    Value::Nil,
                )),
                None => Ok((Value::Nil, Value::Nil, Value::Nil, Value::Nil)),
            }
        })?,
    )?;

    g.set(
        "CursorHasItem",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(matches!(model.cursor, Some(CursorPayload::Item(_))))
        })?,
    )?;
    g.set(
        "CursorHasSpell",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(matches!(model.cursor, Some(CursorPayload::Spell(_))))
        })?,
    )?;
    g.set(
        "ClearCursor",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            clear_cursor(&mut model);
            Ok(())
        })?,
    )?;

    g.set(
        "SplitContainerItem",
        lua.create_function(|lua, (bag, slot, count): (i64, u32, i64)| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            Ok(split_container_item(&mut model, bag, slot, count))
        })?,
    )?;
    g.set(
        "DeleteCursorItem",
        lua.create_function(|lua, ()| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            delete_cursor_item(&mut model);
            Ok(())
        })?,
    )?;

    doll::install(lua)?;
    bar::install(lua)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CursorAction, CursorItem, CursorPayload, CursorSpell};
    use crate::script::UiScript;

    /// A one-item, one-slot backpack (mirrors `container::tests::backpack` closely enough for
    /// these payload-shape tests, which don't need its in-flight slot).
    fn one_item_backpack() -> crate::script::container::ContainerState {
        let mut slots = std::collections::HashMap::new();
        slots.insert(
            1,
            crate::script::container::ContainerSlot {
                bar_placeable: true,
                durability: None,
                texture: Some("Interface\\Icons\\INV_Misc_Food_16".into()),
                count: 5,
                quality: Some(3),
                item_id: 117,
                link: Some("|cffffffff|Hitem:117|h[Tough Jerky]|h|r".into()),
                locked: false,
                equip_slots: Vec::new(),
                cooldown: None,
                readable: false,
                creator: None,
                flags: 0,
                enchants: Vec::new(),
            },
        );
        crate::script::container::ContainerState {
            name: Some("Backpack".into()),
            num_slots: 16,
            slots,
        }
    }

    #[test]
    fn get_cursor_info_and_has_checks_per_arm() {
        let mut s = UiScript::new().unwrap();

        // Empty: all nils, both Has* false.
        assert!(s
            .eval::<bool>("local k = GetCursorInfo() return k == nil")
            .unwrap());
        assert!(!s.eval::<bool>("return CursorHasItem()").unwrap());
        assert!(!s.eval::<bool>("return CursorHasSpell()").unwrap());

        // Item arm.
        s.set_cursor_for_test(CursorPayload::Item(CursorItem {
            bar_placeable: true,
            bag: 0,
            slot: 1,
            item_id: 117,
            texture: None,
            link: Some("|Hitem:117|h[Tough Jerky]|h".into()),
            count: None,
            quality: Some(3),
            equip_slots: Vec::new(),
        }));
        let (kind, id, link) = s
            .eval::<(String, i64, String)>("local k, id, link = GetCursorInfo() return k, id, link")
            .unwrap();
        assert_eq!(
            (kind.as_str(), id, link.as_str()),
            ("item", 117, "|Hitem:117|h[Tough Jerky]|h")
        );
        assert!(s.eval::<bool>("return CursorHasItem()").unwrap());
        assert!(!s.eval::<bool>("return CursorHasSpell()").unwrap());

        // Spell arm — the Era 4-tuple shape.
        s.set_cursor_for_test(CursorPayload::Spell(CursorSpell {
            passive: false,
            book_slot: 3,
            book_type: "spell".into(),
            spell_id: 133,
            texture: None,
        }));
        let (kind, slot, book, spell_id) = s
            .eval::<(String, i64, String, i64)>(
                "local k, slot, book, id = GetCursorInfo() return k, slot, book, id",
            )
            .unwrap();
        assert_eq!(
            (kind.as_str(), slot, book.as_str(), spell_id),
            ("spell", 3, "spell", 133)
        );
        assert!(!s.eval::<bool>("return CursorHasItem()").unwrap());
        assert!(s.eval::<bool>("return CursorHasSpell()").unwrap());

        // Action arm.
        s.set_cursor_for_test(CursorPayload::Action(CursorAction {
            src_slot: 12,
            kind: 0,
            action: 133,
            texture: None,
        }));
        let (kind, slot) = s
            .eval::<(String, i64)>("local k, slot = GetCursorInfo() return k, slot")
            .unwrap();
        assert_eq!((kind.as_str(), slot), ("action", 12));
        assert!(!s.eval::<bool>("return CursorHasItem()").unwrap());
        assert!(!s.eval::<bool>("return CursorHasSpell()").unwrap());
    }

    #[test]
    fn clear_cursor_widens_to_any_arm() {
        let mut s = UiScript::new().unwrap();
        s.set_cursor_for_test(CursorPayload::Spell(CursorSpell {
            passive: false,
            book_slot: 1,
            book_type: "spell".into(),
            spell_id: 1,
            texture: None,
        }));
        assert!(s.cursor_payload().is_some());
        s.run("ClearCursor()").unwrap();
        assert!(s.cursor_payload().is_none());
    }

    /// The world-drop pick routing (decisions 0571 + 0574 — wow-re §11, amended by 0843): a
    /// spell/action payload clears silently on BOTH empty-world legs — terrain included, the
    /// 0843 divergence (the reference keeps it on terrain; the director wants the left click to
    /// dismiss) — while an item keeps its byte-faithful popup flow.
    #[test]
    fn world_drop_terrain_and_nothing_both_clear_a_spell_payload() {
        use super::{world_drop_click, WorldPick};
        let mut s = UiScript::new().unwrap();
        for pick in [WorldPick::Terrain, WorldPick::Nothing] {
            s.set_cursor_for_test(CursorPayload::Spell(CursorSpell {
                passive: false,
                book_slot: 3,
                book_type: "spell".into(),
                spell_id: 133,
                texture: None,
            }));
            s.model_mut().world_pick = pick;
            assert!(
                world_drop_click(&mut s.model_mut()),
                "{pick:?}: the dismiss consumes the click"
            );
            assert!(
                s.cursor_payload().is_none(),
                "{pick:?}: the spell clears silently"
            );
        }

        // An item drops (the popup path) on BOTH empty-world legs, never over an object.
        let item = CursorPayload::Item(CursorItem {
            bar_placeable: true,
            bag: 0,
            slot: 1,
            item_id: 117,
            texture: None,
            link: None,
            count: None,
            quality: None,
            equip_slots: Vec::new(),
        });
        s.set_cursor_for_test(item.clone());
        s.model_mut().world_pick = WorldPick::Terrain;
        assert!(
            world_drop_click(&mut s.model_mut()),
            "terrain: the item pops the popup"
        );
        assert!(s.cursor_item().is_some(), "and stays held");
        s.model_mut().world_pick = WorldPick::Object;
        assert!(
            !world_drop_click(&mut s.model_mut()),
            "object: nothing drops at all"
        );
        assert!(s.cursor_item().is_some());
    }

    #[test]
    fn delete_cursor_item_queues_destroy_and_clears() {
        let mut s = UiScript::new().unwrap();
        s.set_container(0, Some(one_item_backpack()));
        s.run("C_Container.PickupContainerItem(0, 1)").unwrap();
        assert!(s.cursor_item().is_some());

        s.run("DeleteCursorItem()").unwrap();
        assert!(s.cursor_item().is_none(), "the payload clears");
        assert_eq!(s.take_container_destroys(), vec![(0, 1, 0)]);
        assert!(s.take_container_destroys().is_empty(), "drained");
    }

    #[test]
    fn delete_cursor_item_is_a_no_op_off_the_item_arm() {
        let mut s = UiScript::new().unwrap();
        s.set_cursor_for_test(CursorPayload::Action(CursorAction {
            src_slot: 1,
            kind: 0,
            action: 1,
            texture: None,
        }));
        s.run("DeleteCursorItem()").unwrap();
        assert!(
            s.cursor_payload().is_some(),
            "non-item payload is untouched"
        );
        assert!(s.take_container_destroys().is_empty());
    }

    #[test]
    fn split_of_the_whole_stack_degrades_to_a_plain_pickup() {
        let mut s = UiScript::new().unwrap();
        s.set_container(0, Some(one_item_backpack())); // slot 1: a 5-stack
        assert!(s
            .eval::<bool>("return SplitContainerItem(0, 1, 5)")
            .unwrap());
        let held = s.cursor_item().expect("picked up");
        assert_eq!(held.count, None, "count>=stack ⇒ whole-stack pickup");
    }
}
