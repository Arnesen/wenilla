//! The outward half of the container seam (see the parent module doc): the per-frame drains that
//! turn queued Lua intents (`UseContainerItem`, the cursor pick/place/swap/split moves, the
//! delete-confirm popup's destroy) into `ClientCommand`s on the wire, locking the slots each send
//! touches ([`crate::pending_item_ops::PendingItemOps`]) along the way.

use bevy::prelude::*;

use benilla_protocol::messages::BAG_PLAYER_INVENTORY;
use benilla_ui::script::{ScriptValue, UiScript};

use crate::items::Items;
use crate::net::{ClientCommand, NetCommands, ObjectStore, SelfPlayer};
use crate::pending_item_ops::PendingItemOps;

use super::{slot_guid, slot_guid_count, wire_pos, INVTYPE_AMMO};

/// Drain the `(bag, slot)` sources `AutoEquipCursorItem` queued (decision 0208 phase 1b: the
/// model-pane's click-with-payload path) and send `CMSG_AUTOEQUIP_ITEM` — the engine's own
/// contract (`cursor::doll::auto_equip_cursor_item`) already guarantees only a whole-stack,
/// CONTAINER-sourced Item payload (`bag >= 0`) ever reaches this queue.
///
/// The same ammo sub-fork as [`drain_container_uses`] (wow-re `cursor-dragdrop-slots.md`: the one
/// auto-equip sender forks ammo-class → `CMSG_SET_AMMO`): a dropped ammo-class item loads by entry
/// instead, which is also the wire for the ammo slot's own drop (the XML routes it here via
/// `AutoEquipCursorItem` — decision 0526).
///
/// No pending-lock recording here (unlike the move/split/destroy drains) — matching this
/// codebase's own existing precedent for the SAME wire send: `drain_container_uses`'s
/// equip-vs-use fork already sends `AutoEquipItem` for an equippable bag-slot click with no lock
/// bookkeeping of its own. A real gap either way (an in-flight autoequip's source slot isn't
/// visibly dimmed), pre-existing and out of this slice's scope to fix.
pub(super) fn drain_container_autoequips(
    script: Option<NonSendMut<UiScript>>,
    mut items: ResMut<Items>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    commands: Res<NetCommands>,
) {
    let Some(mut script) = script else {
        return;
    };
    for (bag, slot) in script.take_container_autoequips() {
        let Some((bag_index, wire_slot)) = wire_pos(bag, slot) else {
            debug!("ui_items: autoequip ({bag}, {slot}) out of range — ignored");
            continue;
        };
        // Resolve the dropped item's template (source is a 0-based inner slot) and fork ammo →
        // SET_AMMO{entry}. Unresolved (rare — the bag needed the template for the icon) falls back
        // to AUTOEQUIP, whose refusal is at least visible.
        let slot0 = u8::try_from(slot.saturating_sub(1)).unwrap_or(0);
        let ammo_entry = self_q
            .iter()
            .next()
            .and_then(|store| slot_guid(&store.0, bag, slot0, &items))
            .and_then(|guid| {
                let entry = items.object(guid)?.object_entry()?;
                let t = items.template(entry, guid, &commands)?;
                (t.inventory_type == INVTYPE_AMMO).then_some(entry)
            });
        if let Some(entry) = ammo_entry {
            debug!("ui_items: set ammo entry {entry} (drop, lua bag {bag} slot {slot})");
            let _ = commands.0.send(ClientCommand::SetAmmo { entry });
        } else {
            debug!("ui_items: autoequip lua {bag}/{slot} (wire {bag_index}/{wire_slot})");
            let _ = commands.0.send(ClientCommand::AutoEquipItem {
                bag_index,
                slot: wire_slot,
            });
        }
    }
}

/// Drain the inventory-slot ids `UseInventoryItem` queued (decision 0208 phase 1b: the doll
/// slot's right-click) and send `CMSG_USE_ITEM` directly against the equipped position (bag 255
/// plus the 0-based wire slot — `HandleUseItemOpcode` takes equipped positions the same as bag
/// ones, vmangos `ItemHandler.cpp`). Ids outside 1..=19 (ammo, the bag icons) are a no-op — the
/// engine's own queue never receives them from the shipped XML (only the 19 named slot buttons
/// wire `UseInventoryItem` this slice), but a stray Lua call is still refused rather than sent as
/// nonsense.
pub(super) fn drain_inventory_uses(
    script: Option<NonSendMut<UiScript>>,
    commands: Res<NetCommands>,
) {
    let Some(mut script) = script else {
        return;
    };
    for id in script.take_inventory_uses() {
        if !(1..=19).contains(&id) {
            debug!("ui_items: UseInventoryItem({id}) out of range — ignored");
            continue;
        }
        let slot = (id - 1) as u8;
        debug!("ui_items: use equipped item, lua slot {id} (wire 255/{slot})");
        let _ = commands.0.send(ClientCommand::UseItem {
            bag_index: BAG_PLAYER_INVENTORY,
            slot,
        });
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn drain_container_uses(
    script: Option<NonSendMut<UiScript>>,
    mut items: ResMut<Items>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    commands: Res<NetCommands>,
    merchant: Res<crate::ui_merchant::MerchantOpen>,
    bank: Res<crate::ui_bank::BankOpen>,
    mut equip_sound: MessageWriter<crate::sound::AutoEquipSound>,
    mut item_text: ResMut<crate::ui_item_text::ItemTextOpen>,
) {
    let Some(mut script) = script else {
        return;
    };
    // Repair-mode clicks (the engine's pickup intercept — the real client's `0x4f9c7b` route):
    // resolve the clicked slot's item guid and send its single-item repair. The affordability
    // pre-check the client does (error 0x25) is left to the server's own refusal.
    for (bag, slot) in script.take_container_repairs() {
        let Some(vendor) = merchant.vendor else {
            continue;
        };
        let slot0 = u8::try_from(slot.saturating_sub(1)).unwrap_or(0);
        let item_guid = self_q
            .iter()
            .next()
            .and_then(|store| slot_guid(&store.0, bag, slot0, &items));
        match item_guid {
            Some(guid) => {
                debug!("ui_items: repair lua bag {bag} slot {slot} (item {guid:#x})");
                let _ = commands.0.send(ClientCommand::RepairItem {
                    vendor,
                    item_guid: guid,
                });
            }
            None => debug!("ui_items: repair on empty slot (bag {bag} slot {slot}) — ignored"),
        }
    }
    for (bag, slot) in script.take_container_uses() {
        // Lua (bagID, 1-based slot) → the wire's player-array addressing.
        let slot0 = u8::try_from(slot.saturating_sub(1)).ok();
        // Sell affordance (decision 0081 v1): while a merchant is open, a bag-slot click sells the
        // slot's item instead of using/equipping it (`CMSG_SELL_ITEM`, count 0 = the whole stack —
        // the item is addressed by its concrete guid, not a bag slot). An empty slot has no guid, so
        // the click is a harmless no-op.
        if let (true, Some(vendor)) = (merchant.is_open(), merchant.vendor) {
            let item_guid = self_q
                .iter()
                .next()
                .and_then(|store| slot_guid(&store.0, bag, slot0.unwrap_or(0), &items));
            match item_guid {
                Some(guid) => {
                    debug!("ui_items: sell lua bag {bag} slot {slot} (item {guid:#x})");
                    let _ = commands.0.send(ClientCommand::SellItem {
                        vendor,
                        item_guid: guid,
                        count: 0,
                    });
                }
                None => debug!("ui_items: sell on empty slot (bag {bag} slot {slot}) — ignored"),
            }
            continue;
        }
        let Some((bag_index, wire_slot)) = wire_pos(bag, slot) else {
            debug!("ui_items: UseContainerItem({bag}, {slot}) out of range — ignored");
            continue;
        };
        // The deposit/withdraw affordance (decision 0604): while the bank is open, a container
        // click routes as the reference's at-bank auto-move instead of using/equipping — a bank
        // position (the vault or a bank bag) withdraws (`CMSG_AUTOSTORE_BANK_ITEM`), a carried
        // bag's item deposits (`CMSG_AUTOBANK_ITEM`). Which of the two opcodes the reference
        // fires per direction is INFERRED (0604) — vmangos routes AUTOSTORE by source position,
        // so either choice lands correctly. An empty slot refuses server-side, harmlessly.
        // Doll clicks never reach this drain (they flow through `drain_inventory_uses`), so
        // equipped gear keeps its plain use at the bank, like the reference.
        if bank.is_open() {
            let withdrawing = bag == super::BANK_CONTAINER || (5..=10).contains(&bag);
            if withdrawing {
                debug!("ui_items: withdraw (lua bag {bag} → wire {bag_index}/{wire_slot})");
                let _ = commands.0.send(ClientCommand::AutoStoreBankItem {
                    bag: bag_index,
                    slot: wire_slot,
                });
            } else {
                debug!("ui_items: deposit (lua bag {bag} → wire {bag_index}/{wire_slot})");
                let _ = commands.0.send(ClientCommand::AutoBankItem {
                    bag: bag_index,
                    slot: wire_slot,
                });
            }
            continue;
        }
        // The readable fork: an item INSTANCE carrying `ITEM_FIELD_ITEM_TEXT_ID` (a mail-made
        // permanent letter) opens the reader instead of a use — client-side, no permission
        // packet (vmangos' `CMSG_READ_ITEM` handler gates on the *template*'s PageText, which is
        // 0 for the Plain Letter; the text rides the ask-once `CMSG_ITEM_TEXT_QUERY` instead).
        let read = self_q
            .iter()
            .next()
            .and_then(|store| slot_guid(&store.0, bag, slot0.unwrap_or(0), &items))
            .and_then(|guid| {
                items
                    .object(guid)
                    .and_then(|o| o.item_text_id())
                    .filter(|&t| t != 0)
                    .map(|t| (guid, t))
            });
        if let Some((guid, text_id)) = read {
            debug!("ui_items: read item {guid:#x} (text {text_id}, lua bag {bag} slot {slot})");
            item_text.open(guid, text_id);
            continue;
        }
        // The real client's equip-vs-use fork, with the ammo sub-fork wow-re
        // `cursor-dragdrop-slots.md` pins: the auto-equip sender `0x5e1480` sends `CMSG_SET_AMMO`
        // (the item entry) for an ammo-class item, `CMSG_AUTOEQUIP_ITEM` for any other equippable
        // (inventoryType != 0 — weapons, armor, bags), and only a non-equippable is a USE (food,
        // potions, hearthstone). The template is all but always cached by click time (the bag
        // needed it for the icon); unresolved falls back to USE, whose refusal is at least visible.
        // display_id feeds the synthetic pickup→place auto-equip sound (this path never moves the
        // cursor; a drag already gets that pair via the cursor-payload transitions).
        let resolved = self_q
            .iter()
            .next()
            .and_then(|store| slot_guid(&store.0, bag, slot0.unwrap_or(0), &items))
            .and_then(|guid| {
                let entry = items.object(guid)?.object_entry()?;
                let t = items.template(entry, guid, &commands)?;
                Some((entry, t.inventory_type, t.display_info_id))
            });
        match resolved {
            // Ammo loads by entry (`CMSG_SET_AMMO`), NOT the equip swap wire — the stack stays in
            // the bag and `PLAYER_AMMO_ID` references it (decision 0526). The server refuses a
            // wrong/absent ranged weapon via `SMSG_INVENTORY_CHANGE_FAILURE`.
            Some((entry, INVTYPE_AMMO, display_id)) => {
                debug!("ui_items: set ammo entry {entry} (lua bag {bag} slot {slot})");
                let _ = commands.0.send(ClientCommand::SetAmmo { entry });
                equip_sound.write(crate::sound::AutoEquipSound { display_id });
            }
            Some((_, inv_type, display_id)) if inv_type != 0 => {
                debug!("ui_items: auto-equip (lua bag {bag} → wire {bag_index}/{wire_slot})");
                let _ = commands.0.send(ClientCommand::AutoEquipItem {
                    bag_index,
                    slot: wire_slot,
                });
                equip_sound.write(crate::sound::AutoEquipSound { display_id });
            }
            _ => {
                debug!("ui_items: use item (lua bag {bag} → wire {bag_index}/{wire_slot})");
                let _ = commands.0.send(ClientCommand::UseItem {
                    bag_index,
                    slot: wire_slot,
                });
            }
        }
    }
}

/// Drain the pick/place/swap/split moves `PickupContainerItem`/`SplitContainerItem` queued and
/// send them on the wire (decision 0216 §6, whole-space since slice 2).
///
/// `count: None` (a whole-stack move/swap): both ends map through [`wire_pos`]. Both landing on
/// [`BAG_PLAYER_INVENTORY`] (the player's own grid — equipment, bag buttons, and the backpack) is
/// a `CMSG_SWAP_INV_ITEM` on the two player-array slots, unchanged; otherwise (either end an
/// equipped bag 1..4) it's the general `CMSG_SWAP_ITEM` — VERIFIED vmangos
/// `Server/Packets/Item.cpp:30-36`: body order is **dstbag, dstslot, srcbag, srcslot** (opcode
/// `0x10C`; the builder's arg order and the golden in `messages/items.rs::swap_item_body_destination_first`
/// already match this). An empty destination is still a swap on either wire (a move).
///
/// `count: Some(n)` (a split placement): both ends map through [`wire_pos`] (all five bags valid,
/// since `SplitContainerItem`'s pickup already resolved a real slot) → `CMSG_SPLIT_ITEM`, `count`
/// clamped to the wire's `u8`.
///
/// Every send locks the Lua-space slots it touches — both ends (decision 0218 §3: "a send locks
/// both ends") — recording each slot's CURRENT item guid as the resolving clear's baseline
/// ([`PendingItemOps::add`]) and firing `ITEM_LOCK_CHANGED` immediately, so a bag window's own
/// synchronous post-click repaint and every later frame agree (only the SOURCE slot's repaint at
/// the exact moment of THIS click can still show briefly stale — see the parent module doc —
/// corrected by this same-frame event).
pub(super) fn drain_container_moves(
    script: Option<NonSendMut<UiScript>>,
    commands: Res<NetCommands>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    items: Res<Items>,
    mut pending: ResMut<PendingItemOps>,
) {
    let Some(mut script) = script else {
        return;
    };
    let store = self_q.iter().next();
    for mv in script.take_container_moves() {
        let (Some(src), Some(dst)) = (
            wire_pos(mv.src_bag, mv.src_slot),
            wire_pos(mv.dst_bag, mv.dst_slot),
        ) else {
            debug!("ui_items: container move {mv:?} out of range — ignored");
            continue;
        };
        match mv.count {
            None => {
                let (src_wire_bag, src_slot) = src;
                let (dst_wire_bag, dst_slot) = dst;
                if src_wire_bag == BAG_PLAYER_INVENTORY && dst_wire_bag == BAG_PLAYER_INVENTORY {
                    debug!(
                        "ui_items: swap backpack lua {}→{} (wire 255 slot {src_slot}↔{dst_slot})",
                        mv.src_slot, mv.dst_slot
                    );
                    let _ = commands
                        .0
                        .send(ClientCommand::SwapInvItem { src_slot, dst_slot });
                } else {
                    debug!(
                        "ui_items: swap whole-space lua {}/{}→{}/{} (wire {src_wire_bag}/{src_slot}→{dst_wire_bag}/{dst_slot})",
                        mv.src_bag, mv.src_slot, mv.dst_bag, mv.dst_slot
                    );
                    let _ = commands.0.send(ClientCommand::SwapItem {
                        dst_bag: dst_wire_bag,
                        dst_slot,
                        src_bag: src_wire_bag,
                        src_slot,
                    });
                }
            }
            Some(n) => {
                let (src_bag, src_slot) = src;
                let (dst_bag, dst_slot) = dst;
                let count = n.min(u32::from(u8::MAX)) as u8;
                debug!(
                    "ui_items: split lua {}/{}→{}/{} × {count} (wire {src_bag}/{src_slot}→{dst_bag}/{dst_slot})",
                    mv.src_bag, mv.src_slot, mv.dst_bag, mv.dst_slot
                );
                let _ = commands.0.send(ClientCommand::SplitItem {
                    src_bag,
                    src_slot,
                    dst_bag,
                    dst_slot,
                    count,
                });
            }
        }
        // The pending lock: both ends, baselined on their CURRENT (guid, count) — the resolving
        // clear then watches for either to move (an empty destination baselines (0, 0) and watches
        // for an item to land there).
        let (src_guid, src_count) = slot_guid_count(store, mv.src_bag, mv.src_slot, &items);
        let (dst_guid, dst_count) = slot_guid_count(store, mv.dst_bag, mv.dst_slot, &items);
        pending.add([
            (mv.src_bag, mv.src_slot, src_guid, src_count),
            (mv.dst_bag, mv.dst_slot, dst_guid, dst_count),
        ]);
        for (bag, slot) in [(mv.src_bag, mv.src_slot), (mv.dst_bag, mv.dst_slot)] {
            script.fire_event(
                "ITEM_LOCK_CHANGED",
                vec![ScriptValue::Int(bag), ScriptValue::Int(i64::from(slot))],
            );
        }
    }
}

/// Drain the `(bag, slot, count)` destroys `DeleteCursorItem` queued (the delete-confirm popup's
/// accept, decision 0216 §3) and send `CMSG_DESTROYITEM`. `count == 0` is the engine's "whole
/// stack" convention — it rides straight onto the wire, which shares the same convention.
///
/// Locks the one slot touched — baselined on its CURRENT `(guid, count)`, same as
/// [`drain_container_moves`] — and fires `ITEM_LOCK_CHANGED` immediately. Unlike a move/split
/// there is no second "displaced" slot: a destroy only ever removes from where it's aimed.
pub(super) fn drain_container_destroys(
    script: Option<NonSendMut<UiScript>>,
    commands: Res<NetCommands>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    items: Res<Items>,
    mut pending: ResMut<PendingItemOps>,
) {
    let Some(mut script) = script else {
        return;
    };
    let store = self_q.iter().next();
    for (bag, slot, count) in script.take_container_destroys() {
        let Some((bag_index, wire_slot)) = wire_pos(bag, slot) else {
            debug!("ui_items: destroy ({bag}, {slot}) out of range — ignored");
            continue;
        };
        let count = count.min(u32::from(u8::MAX)) as u8;
        debug!("ui_items: destroy lua {bag}/{slot} × {count} (wire {bag_index}/{wire_slot})");
        let _ = commands.0.send(ClientCommand::DestroyItem {
            bag_index,
            slot: wire_slot,
            count,
        });
        let (guid, stack) = slot_guid_count(store, bag, slot, &items);
        pending.add([(bag, slot, guid, stack)]);
        script.fire_event(
            "ITEM_LOCK_CHANGED",
            vec![ScriptValue::Int(bag), ScriptValue::Int(i64::from(slot))],
        );
    }
}
