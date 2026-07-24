//! The action bar (decision 0216 §7, slice 4; byte-verified 0218 §2/§4): `PickupAction`/
//! `PlaceAction` join the ONE payload space as the [`CursorAction`] arm. Two things set this
//! surface apart from bags/doll:
//!
//! - **The bar is client-authoritative and actions HOP** (0218 §4, `PlaceAction 0x4e62e0`) — the
//!   opposite of the post-0218 item swap: placing onto an occupied slot puts the DISPLACED action
//!   on the cursor rather than clearing, because there is no server round-trip to wait on (the
//!   120-slot table is ours; [`place_action`] IS the mutation, not a request for one).
//! - **The engine owns the payload + transition, the app owns the 120-table** (0216 §7's
//!   ownership split): `model.actions` here is the engine's own OPTIMISTIC mirror of the app's
//!   authoritative store, kept only so `HasAction`/`GetActionTexture`/&c. read right the instant a
//!   local pickup/place happens, without waiting a frame for the app to re-feed. Every mutation
//!   also queues `(lua id, packed)` onto [`Model::action_sets`] — the wire intent the app drains
//!   into `CMSG_SET_ACTION_BUTTON`, one send per queued entry (0218 §4: a drag-swap is two sends,
//!   never atomic — this module never coalesces them).

use mlua::Lua;

use crate::script::action::{ACTION_KIND_ITEM, ACTION_KIND_SPELL};
use crate::script::{ActionSlot, Model};

use super::{queue_cursor_update, CursorAction, CursorPayload};

/// Pack `(kind, action)` into the wire's `u32` slot word (`kind<<24 | action`, decision 0216 §1).
fn pack(kind: u8, action: u32) -> u32 {
    (u32::from(kind) << 24) | (action & 0x00FF_FFFF)
}

/// `PickupAction(id)` — the action-bar slot's shift-click/drag-start entry point (ref
/// `ActionBarFrame.xml:12-38`'s `IsShiftKeyDown()` fork, `OnDragStart`).
///
/// - **A payload is already held** → falls through to [`place_action`] (the reference's own
///   contract: a shift-click while carrying just places, `ActionButtonTemplate`'s `OnClick` never
///   special-cases it).
/// - **Empty cursor, an occupied slot** → picks it up: payload `Action { src_slot: id, kind,
///   action, texture }`, the slot removed from `model.actions` (optimistic — the app re-pushes an
///   agreeing snapshot once `action_sets` lands), and `action_sets.push((id, 0))` queued — picking
///   an action OFF the bar empties it immediately on the wire too (the classic drag-off).
/// - **Empty cursor, an empty slot** → no-op (nothing to pick up).
///
/// Returns whether the caller should repaint (mirrors the container/doll pickup contract).
pub(super) fn pickup_action(model: &mut Model, id: u32) -> bool {
    if model.cursor.is_some() {
        return place_action(model, id);
    }
    let Some(slot) = model.actions.get(&id) else {
        return false;
    };
    let payload = CursorAction {
        src_slot: id,
        kind: slot.kind,
        action: slot.action,
        texture: slot.texture.clone(),
    };
    model.actions.remove(&id);
    model.cursor = Some(CursorPayload::Action(payload));
    model.action_sets.push((id, 0));
    queue_cursor_update(model);
    true
}

/// `PlaceAction(id)` — the action-bar slot's click-with-payload/`OnReceiveDrag` entry point (ref
/// `UseAction(id, checkCursor=1)`'s place fork, `ActionBarFrame.xml`'s `OnReceiveDrag`).
///
/// Every arm writes the held action into `model.actions[id]` optimistically and queues
/// `action_sets.push((id, packed))`; what happens to the cursor afterward is the byte-verified
/// divergence from every other surface (0218 §4): an OCCUPIED destination puts the DISPLACED
/// action on the cursor (referencing `id` as its new `src_slot` — the slot it can now be placed
/// FROM), an empty destination just clears.
///
/// - Payload **Action** → `(kind, action)` straight off the held payload.
/// - Payload **Item** → `packed = item_id | ITEM<<24`; the item came from a BAG and STAYS there —
///   a bar item action is a reference, not a move, so no container move is queued here (the app's
///   `drain_action_sets` never touches `container_moves` either).
/// - Payload **Spell** → `packed = spell_id | SPELL<<24` (the producer, `PickupSpell`, lands in
///   slice 5 — this arm already works once something populates the payload).
/// - **Empty cursor** → no-op.
///
/// Returns whether the caller should repaint.
pub(crate) fn place_action(model: &mut Model, id: u32) -> bool {
    let Some(held) = model.cursor.take() else {
        return false;
    };
    let (kind, action, texture) = match &held {
        CursorPayload::Action(a) => (a.kind, a.action, a.texture.clone()),
        CursorPayload::Item(i) => (ACTION_KIND_ITEM, i.item_id, i.texture.clone()),
        CursorPayload::Spell(s) => (ACTION_KIND_SPELL, s.spell_id, s.texture.clone()),
    };
    let displaced = model.actions.insert(
        id,
        ActionSlot {
            texture,
            kind,
            action,
            count: 0, // the app's next-frame re-feed resolves the real bag count for an ITEM slot
        },
    );
    model.action_sets.push((id, pack(kind, action)));
    model.cursor = displaced.map(|d| {
        CursorPayload::Action(CursorAction {
            src_slot: id,
            kind: d.kind,
            action: d.action,
            texture: d.texture,
        })
    });
    queue_cursor_update(model);
    true
}

/// Register the action-bar's cursor globals — top-level, matching the reference
/// (`PickupAction`/`PlaceAction` are not namespaced any more than `PickupContainerItem`'s cursor
/// siblings are).
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    g.set(
        "PickupAction",
        lua.create_function(|lua, id: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            Ok(pickup_action(&mut model, id))
        })?,
    )?;
    g.set(
        "PlaceAction",
        lua.create_function(|lua, id: u32| {
            let mut model = lua.app_data_mut::<Model>().expect("model app_data");
            Ok(place_action(&mut model, id))
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::script::cursor::{CursorAction, CursorItem, CursorPayload, CursorSpell};
    use crate::script::{ActionSlot, UiScript};

    fn action_slot(texture: &str, kind: u8, action: u32) -> ActionSlot {
        ActionSlot {
            texture: Some(texture.into()),
            kind,
            action,
            count: 0,
        }
    }

    #[test]
    fn pickup_action_empties_the_slot_and_queues_a_clear() {
        let mut s = UiScript::new().unwrap();
        s.set_action(1, Some(action_slot("Interface\\Icons\\Spell_A", 0x00, 133)));

        assert!(s.eval::<bool>("return PickupAction(1)").unwrap());
        assert!(
            !s.eval::<bool>("return HasAction(1)").unwrap(),
            "removed from the engine's optimistic mirror"
        );
        let (kind, id) = s
            .eval::<(String, i64)>("local k, slot = GetCursorInfo() return k, slot")
            .unwrap();
        assert_eq!((kind.as_str(), id), ("action", 1));
        assert_eq!(s.take_action_sets(), vec![(1, 0)]);
    }

    #[test]
    fn pickup_action_on_an_empty_slot_is_a_no_op() {
        let mut s = UiScript::new().unwrap();
        assert!(!s.eval::<bool>("return PickupAction(5)").unwrap());
        assert!(s.cursor_payload().is_none());
        assert!(s.take_action_sets().is_empty());
    }

    #[test]
    fn place_action_onto_empty_writes_the_slot_and_clears_cursor() {
        let mut s = UiScript::new().unwrap();
        s.set_cursor_for_test(CursorPayload::Action(CursorAction {
            src_slot: 3,
            kind: 0x00,
            action: 133,
            texture: Some("Interface\\Icons\\Spell_A".into()),
        }));

        assert!(s.eval::<bool>("return PlaceAction(7)").unwrap());
        assert!(s.cursor_payload().is_none(), "empty destination clears");
        assert!(s.eval::<bool>("return HasAction(7)").unwrap());
        assert_eq!(
            s.eval::<String>("return GetActionTexture(7)").unwrap(),
            "Interface\\Icons\\Spell_A"
        );
        // 0x00<<24 | 133 == 133.
        assert_eq!(s.take_action_sets(), vec![(7, 133)]);
    }

    /// The byte-verified divergence from bags/doll (0218 §4): placing onto an OCCUPIED action
    /// slot HOPS the displaced action onto the cursor — two `action_sets` entries across the
    /// gesture (the pickup's clear, then the place's write), never a container move.
    #[test]
    fn place_action_onto_occupied_hops_the_displaced_action() {
        let mut s = UiScript::new().unwrap();
        s.set_action(1, Some(action_slot("Interface\\Icons\\Spell_A", 0x00, 111)));
        s.set_action(2, Some(action_slot("Interface\\Icons\\Spell_B", 0x00, 222)));

        assert!(s.eval::<bool>("return PickupAction(1)").unwrap());
        assert_eq!(s.take_action_sets(), vec![(1, 0)]);

        assert!(s.eval::<bool>("return PlaceAction(2)").unwrap());
        // Slot 2 now shows the placed action (111).
        assert_eq!(
            s.eval::<String>("return GetActionTexture(2)").unwrap(),
            "Interface\\Icons\\Spell_A"
        );
        // The displaced action (222) is now the held payload, sourced from slot 2.
        let (kind, src) = s
            .eval::<(String, i64)>("local k, slot = GetCursorInfo() return k, slot")
            .unwrap();
        assert_eq!((kind.as_str(), src), ("action", 2));
        assert_eq!(
            s.cursor_payload(),
            Some(CursorPayload::Action(CursorAction {
                src_slot: 2,
                kind: 0x00,
                action: 222,
                texture: Some("Interface\\Icons\\Spell_B".into()),
            }))
        );
        assert_eq!(s.take_action_sets(), vec![(2, 111)]);

        // A hop is Some→Some: no HIDE+SHOW churn out of one gesture.
        // (Exercised directly by the SHOWGRID/HIDEGRID test below.)
    }

    #[test]
    fn place_action_item_payload_packs_the_item_kind_and_leaves_the_bag_untouched() {
        let mut s = UiScript::new().unwrap();
        s.set_cursor_for_test(CursorPayload::Item(CursorItem {
            bag: 0,
            slot: 1,
            item_id: 117,
            texture: Some("Interface\\Icons\\INV_Misc_Food_16".into()),
            link: None,
            count: None,
            quality: Some(1),
            equip_slots: Vec::new(),
        }));

        assert!(s.eval::<bool>("return PlaceAction(4)").unwrap());
        assert!(s.cursor_payload().is_none());
        // 0x80<<24 | 117.
        assert_eq!(s.take_action_sets(), vec![(4, 0x8000_0000 | 117)]);
        assert!(
            s.take_container_moves().is_empty(),
            "a bar item action is a reference, not a move"
        );
    }

    #[test]
    fn place_action_spell_payload_packs_the_spell_kind() {
        let mut s = UiScript::new().unwrap();
        s.set_cursor_for_test(CursorPayload::Spell(CursorSpell {
            book_slot: 3,
            book_type: "spell".into(),
            spell_id: 133,
            texture: Some("Interface\\Icons\\Spell_A".into()),
        }));

        assert!(s.eval::<bool>("return PlaceAction(9)").unwrap());
        assert_eq!(s.take_action_sets(), vec![(9, 133)]); // 0x00<<24 | 133
    }

    #[test]
    fn place_action_with_an_empty_cursor_is_a_no_op() {
        let mut s = UiScript::new().unwrap();
        s.set_action(1, Some(action_slot("Interface\\Icons\\Spell_A", 0x00, 111)));
        assert!(!s.eval::<bool>("return PlaceAction(1)").unwrap());
        assert!(s.eval::<bool>("return HasAction(1)").unwrap(), "untouched");
        assert!(s.take_action_sets().is_empty());
    }

    /// Shift-click while ALREADY holding just places (the reference's own `OnClick` never
    /// special-cases it): `PickupAction` on a held cursor routes straight to [`super::place_action`].
    #[test]
    fn pickup_action_with_a_payload_held_falls_through_to_place() {
        let mut s = UiScript::new().unwrap();
        s.set_cursor_for_test(CursorPayload::Action(CursorAction {
            src_slot: 1,
            kind: 0x00,
            action: 111,
            texture: Some("Interface\\Icons\\Spell_A".into()),
        }));
        assert!(s.eval::<bool>("return PickupAction(5)").unwrap());
        assert!(s.cursor_payload().is_none(), "placed onto the empty slot 5");
        assert!(s.eval::<bool>("return HasAction(5)").unwrap());
        assert_eq!(s.take_action_sets(), vec![(5, 111)]);
    }

    /// `ACTIONBAR_SHOWGRID`/`ACTIONBAR_HIDEGRID` fire on the cursor's None↔Some edges (decision
    /// 0216 §7) — a pickup shows the grid, a place-onto-empty hides it, and a HOP (Some→Some)
    /// fires neither (no HIDE+SHOW churn out of one gesture).
    #[test]
    fn showgrid_hidegrid_fire_on_gain_and_loss_not_on_a_hop() {
        let mut s = UiScript::new().unwrap();
        s.set_action(1, Some(action_slot("Interface\\Icons\\Spell_A", 0x00, 111)));
        s.set_action(2, Some(action_slot("Interface\\Icons\\Spell_B", 0x00, 222)));
        s.run(
            r#"
            shows, hides = 0, 0
            local f = CreateFrame("Frame", "GridListener")
            f:RegisterEvent("ACTIONBAR_SHOWGRID")
            f:RegisterEvent("ACTIONBAR_HIDEGRID")
            f:SetScript("OnEvent", function()
                if event == "ACTIONBAR_SHOWGRID" then shows = shows + 1 end
                if event == "ACTIONBAR_HIDEGRID" then hides = hides + 1 end
            end)
            "#,
        )
        .unwrap();

        s.run("PickupAction(1)").unwrap(); // None -> Some: SHOW
        s.tick(0.01);
        assert_eq!(s.eval::<i64>("return shows").unwrap(), 1);
        assert_eq!(s.eval::<i64>("return hides").unwrap(), 0);

        s.run("PlaceAction(2)").unwrap(); // Some -> Some (hop): neither
        s.tick(0.01);
        assert_eq!(s.eval::<i64>("return shows").unwrap(), 1);
        assert_eq!(s.eval::<i64>("return hides").unwrap(), 0);

        s.run("ClearCursor()").unwrap(); // Some -> None: HIDE
        s.tick(0.01);
        assert_eq!(s.eval::<i64>("return shows").unwrap(), 1);
        assert_eq!(s.eval::<i64>("return hides").unwrap(), 1);
    }
}
