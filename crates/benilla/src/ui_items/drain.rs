//! The outward half of the container seam (see the parent module doc): the per-frame drains that
//! turn queued Lua intents (`UseContainerItem`, the cursor pick/place/swap/split moves, the
//! delete-confirm popup's destroy) into `ClientCommand`s on the wire, locking the slots each send
//! touches ([`crate::pending_item_ops::PendingItemOps`]) along the way.

use bevy::prelude::*;

use benilla_protocol::messages::BAG_PLAYER_INVENTORY;
use benilla_ui::script::{ScriptValue, UiScript, EQUIPMENT_BAG};

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
/// slot's right-click) and route the equipped position (bag 255 plus the 0-based wire slot —
/// `HandleUseItemOpcode` takes equipped positions the same as bag ones, vmangos `ItemHandler.cpp`)
/// through the shared use fork ([`super::item_use_command`]): the reference's doll click lands in
/// the same `CGItem::Use` a bag click does (`0x4c7af0`), quest fork included — one of the five
/// equippable quest-starters, worn and right-clicked, offers its quest instead of casting nothing.
/// Ids outside 1..=19 (ammo, the bag icons) are a no-op — the engine's own queue never receives
/// them from the shipped XML (only the 19 named slot buttons wire `UseInventoryItem` this slice),
/// but a stray Lua call is still refused rather than sent as nonsense.
pub(super) fn drain_inventory_uses(
    script: Option<NonSendMut<UiScript>>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    targeting: crate::ui_action::cast_target::CastTargeting,
    mut ladder: crate::ui_action::CastLadder,
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
        // The doll's own slot ids ARE the wire slots (`wire_pos`'s EQUIPMENT_BAG law), so the
        // equipped instance resolves off the player's INV array directly.
        let (guid, start_quest, spell_index, use_spell) = self_q
            .iter()
            .next()
            .and_then(|store| slot_guid(&store.0, EQUIPMENT_BAG, slot, &ladder.items))
            .and_then(|guid| {
                let entry = ladder.items.object(guid)?.object_entry()?;
                let t = ladder.items.template(entry, guid, &ladder.commands)?;
                // The wire's spell byte is a template BLOCK ordinal (decision 0666) — the
                // template is already in hand here for `start_quest`, so name the real one.
                Some((
                    Some(guid),
                    t.start_quest,
                    t.use_spell_index().unwrap_or(0),
                    t.use_spell.map(|u| u.spell_id),
                ))
            })
            .unwrap_or((None, 0, 0, None));
        debug!("ui_items: use equipped item, lua slot {id} (wire 255/{slot})");
        super::send_item_use(
            super::ItemUse {
                guid,
                start_quest,
                bag_index: BAG_PLAYER_INVENTORY,
                slot,
                spell_index,
                use_spell,
                on_object: None,
            },
            &targeting.context(),
            &mut ladder,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn drain_container_uses(
    script: Option<NonSendMut<UiScript>>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    merchant: Res<crate::ui_merchant::MerchantOpen>,
    bank: Res<crate::ui_bank::BankOpen>,
    mut equip_sound: MessageWriter<crate::sound::AutoEquipSound>,
    mut item_text: ResMut<crate::ui_item_text::ItemTextOpen>,
    targeting: crate::ui_action::cast_target::CastTargeting,
    // The client-side pending ("gray") lock — the right-click-open arm arms it (decision 0916).
    mut pending_items: ResMut<PendingItemOps>,
    mut ladder: crate::ui_action::CastLadder,
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
            .and_then(|store| slot_guid(&store.0, bag, slot0, &ladder.items));
        match item_guid {
            Some(guid) => {
                debug!("ui_items: repair lua bag {bag} slot {slot} (item {guid:#x})");
                let _ = ladder.commands.0.send(ClientCommand::RepairItem {
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
                .and_then(|store| slot_guid(&store.0, bag, slot0.unwrap_or(0), &ladder.items));
            match item_guid {
                Some(guid) => {
                    debug!("ui_items: sell lua bag {bag} slot {slot} (item {guid:#x})");
                    let _ = ladder.commands.0.send(ClientCommand::SellItem {
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
                let _ = ladder.commands.0.send(ClientCommand::AutoStoreBankItem {
                    bag: bag_index,
                    slot: wire_slot,
                });
            } else {
                debug!("ui_items: deposit (lua bag {bag} → wire {bag_index}/{wire_slot})");
                let _ = ladder.commands.0.send(ClientCommand::AutoBankItem {
                    bag: bag_index,
                    slot: wire_slot,
                });
            }
            continue;
        }
        // Everything the reference's click law reads off the clicked slot, resolved once. `None` =
        // an empty slot, or a template still in flight — the click then falls all the way through
        // to a plain USE, whose refusal is at least visible (the template is all but always cached
        // by click time; the bag needed it for the icon).
        let clicked = self_q
            .iter()
            .next()
            .and_then(|store| slot_guid(&store.0, bag, slot0.unwrap_or(0), &ladder.items))
            .and_then(|guid| {
                let obj = ladder.items.object(guid)?;
                let inst_flags = obj.item_flags().unwrap_or(0);
                let item_text_id = obj.item_text_id().unwrap_or(0);
                let entry = obj.object_entry()?;
                let t = ladder.items.template(entry, guid, &ladder.commands)?;
                Some(Clicked {
                    guid,
                    entry,
                    item_text_id,
                    inventory_type: t.inventory_type,
                    display_info_id: t.display_info_id,
                    start_quest: t.start_quest,
                    // The wire's spell byte is a template BLOCK ordinal (decision 0666) — the
                    // template is right here, so send the real one rather than assuming 0.
                    spell_index: t.use_spell_index().unwrap_or(0),
                    use_spell: t.use_spell.map(|u| u.spell_id),
                    unwraps_gift: t.unwraps_gift(inst_flags),
                    opens_loot: t.opens_loot(),
                })
            });

        // The reference's equip-vs-use fork (`0x4fa3b9`/`0x4fa3bd`, wow-re `right-click-open.md`
        // §2), with the ammo sub-fork `cursor-dragdrop-slots.md` pins: the auto-equip sender
        // `0x5e1480` sends `CMSG_SET_AMMO` (the item entry) for an ammo-class item,
        // `CMSG_AUTOEQUIP_ITEM` for any other equippable (inventoryType != 0 — weapons, armor,
        // bags). display_id feeds the synthetic pickup→place auto-equip sound (this path never
        // moves the cursor; a drag already gets that pair via the cursor-payload transitions).
        //
        // The arm carries the reference's own **quest guard** (`0x4fa3bd`–`0x4fa3cc`, decision
        // 0664): it equips only when `StartQuest` (`[rec+0x1a8]`) is 0, so a quest-starter falls
        // through *whatever* its inventoryType — the five equippable ones (Pendant of Myzrael,
        // Arena Master, …) offer their quest on a right-click, they don't put themselves on.
        //
        // **Everything below this fork is `0x5d8d00`, the USE dispatcher, in ITS OWN order**
        // (wow-re `right-click-open.md` §3) — an equippable item never reaches any of it.
        if let Some(c) = clicked.filter(|c| c.start_quest == 0 && c.inventory_type != 0) {
            if c.inventory_type == INVTYPE_AMMO {
                // Ammo loads by entry (`CMSG_SET_AMMO`), NOT the equip swap wire — the stack stays
                // in the bag and `PLAYER_AMMO_ID` references it (decision 0526). The server
                // refuses a wrong/absent ranged weapon via `SMSG_INVENTORY_CHANGE_FAILURE`.
                debug!(
                    "ui_items: set ammo entry {} (lua bag {bag} slot {slot})",
                    c.entry
                );
                let _ = ladder
                    .commands
                    .0
                    .send(ClientCommand::SetAmmo { entry: c.entry });
            } else {
                debug!("ui_items: auto-equip (lua bag {bag} → wire {bag_index}/{wire_slot})");
                let _ = ladder.commands.0.send(ClientCommand::AutoEquipItem {
                    bag_index,
                    slot: wire_slot,
                });
            }
            equip_sound.write(crate::sound::AutoEquipSound {
                display_id: c.display_info_id,
            });
            continue;
        }
        // #2 — the wrapped gift (`0x5d8d92`/`0x5d8d9d` → emitter `0x5edd60`): `CMSG_OPEN_ITEM`
        // unwraps it. FIRST in the dispatcher, ahead of the quest and readable arms, so a gift
        // that also carries letter text unwraps rather than reads. (vmangos answers this one with
        // an entry swap out of `character_gifts`, not a loot window.)
        if let Some(c) = clicked.filter(|c| c.unwraps_gift) {
            debug!(
                "ui_items: unwrap gift {:#x} (lua bag {bag} → wire {bag_index}/{wire_slot})",
                c.guid
            );
            let _ = ladder.commands.0.send(ClientCommand::OpenItem {
                bag_index,
                slot: wire_slot,
            });
            continue;
        }
        // #3 — the quest-starter (`0x5d8dd2`, decision 0664): the item's own guid is the
        // questgiver. Placed at the reference's position rather than at the tail, so a starter
        // that is *also* readable or lootable offers its quest instead of reading/opening. Routed
        // through the one shared use fork, which is what turns a non-zero StartQuest into the
        // item-guid query.
        if let Some(c) = clicked.filter(|c| c.start_quest != 0) {
            debug!(
                "ui_items: quest-starter {:#x} offers quest {}",
                c.guid, c.start_quest
            );
            super::send_item_use(
                super::ItemUse {
                    guid: Some(c.guid),
                    start_quest: c.start_quest,
                    bag_index,
                    slot: wire_slot,
                    spell_index: c.spell_index,
                    use_spell: c.use_spell,
                    on_object: None,
                },
                &targeting.context(),
                &mut ladder,
            );
            continue;
        }
        // #6 — readable: an item INSTANCE carrying `ITEM_FIELD_ITEM_TEXT_ID` (a mail-made
        // permanent letter) opens the reader — client-side, no permission packet (vmangos'
        // `CMSG_READ_ITEM` handler gates on the *template*'s PageText, which is 0 for the Plain
        // Letter; the text rides the ask-once `CMSG_ITEM_TEXT_QUERY` instead).
        //
        // The dispatcher's arm **#5** — the same local open off the *template*'s `PageText`
        // (`0x5d8e4c`) — has no feed here yet (books/pages aren't built). When it lands it belongs
        // immediately above this one, and still above the open arm: the send order is the INVERSE
        // of the tooltip's, where OPENABLE wins over READABLE. A template that is both readable
        // and lootable therefore *shows* `<Right Click to Open>` and *reads* on click — the
        // reference's own inversion, byte-verified, not a slip of ours.
        if let Some(c) = clicked.filter(|c| c.item_text_id != 0) {
            debug!(
                "ui_items: read item {:#x} (text {}, lua bag {bag} slot {slot})",
                c.guid, c.item_text_id
            );
            item_text.open(c.guid, c.item_text_id);
            continue;
        }
        // #8 — the open arm (`0x5d8f7c: test al,4` → emitter `0x5edc80`): a **bare** template
        // LOOTABLE test. `CMSG_OPEN_ITEM`, not `CMSG_USE_ITEM` — the server's
        // `HandleOpenItemOpcode` is the only handler that answers with
        // `SendLoot(item guid, LOOT_CORPSE)`, i.e. a loot window over a thing in your bag; sending
        // USE_ITEM instead casts the item's (absent) on-use spell and nothing happens, which was
        // exactly the "no way to open clams" symptom.
        //
        // Deliberately looser than the tooltip line's predicate (`ItemInfo::shows_open_line`): the
        // send consults **neither** LockID nor the instance UNLOCKED bit (VERIFIED both ways — no
        // `[rec+0x1ac]` operand exists anywhere on the send path). So a still-locked junkbox DOES
        // send, and the server's `EQUIP_ERR_ITEM_LOCKED` is where the player's "Item is locked"
        // line comes from. Gating locally would eat the click in silence (decision 0896).
        if let Some(c) = clicked.filter(|c| c.opens_loot) {
            debug!(
                "ui_items: open item {:#x} (lua bag {bag} → wire {bag_index}/{wire_slot})",
                c.guid
            );
            // **The gray lock, armed before the send** — the reference's emitter `0x5edc80` calls
            // the lock setter `0x4953e0` at `0x5edcd9` and only then ships `CMSG_OPEN_ITEM`
            // (wow-re `inventory-change-failure-display.md` §8, decision 0916). So a clam,
            // lockbox or loot bag greys the instant you right-click it and stays grey until the
            // server answers — the loot landing (a resolving field update) or a refusal
            // (`EQUIP_ERR_ITEM_LOCKED` on a still-locked junkbox), both of which
            // `PendingItemOps` already clears on.
            //
            // Deliberately NOT armed on the gift-unwrap arm above, which sends the same opcode:
            // its emitter `0x5edd60` contains no setter call. That asymmetry is the reference's,
            // verified, and copying it is the point.
            let (guid, count) = self_q
                .iter()
                .next()
                .map(|store| slot_guid_count(Some(store), bag, slot, &ladder.items))
                .unwrap_or((0, 0));
            pending_items.add([(bag, slot, guid, count)]);
            script.fire_event(
                "ITEM_LOCK_CHANGED",
                vec![ScriptValue::Int(bag), ScriptValue::Int(i64::from(slot))],
            );
            let _ = ladder.commands.0.send(ClientCommand::OpenItem {
                bag_index,
                slot: wire_slot,
            });
            continue;
        }
        // The tail: a plain use (food, potions, hearthstone). The quest arm already fired above,
        // so the shared fork's own quest leg is inert here by construction.
        debug!("ui_items: use item (lua bag {bag} → wire {bag_index}/{wire_slot})");
        super::send_item_use(
            super::ItemUse {
                guid: clicked.map(|c| c.guid),
                start_quest: 0,
                bag_index,
                slot: wire_slot,
                spell_index: clicked.map_or(0, |c| c.spell_index),
                use_spell: clicked.and_then(|c| c.use_spell),
                on_object: None,
            },
            &targeting.context(),
            &mut ladder,
        );
    }
}

/// One clicked bag slot, resolved: the live instance's guid and the template scalars the
/// reference's fork chain tests, read once so the chain above is a plain ordered cascade rather
/// than five repeats of the same lookup. Copy-cheap on purpose — no borrow of [`Items`] outlives
/// the resolve.
#[derive(Clone, Copy)]
struct Clicked {
    guid: u64,
    /// `OBJECT_FIELD_ENTRY` — the ammo arm addresses by entry, not by slot (decision 0526).
    entry: u32,
    /// Instance `ITEM_FIELD_ITEM_TEXT_ID` (a mail-made permanent letter); 0 = not a letter.
    item_text_id: u32,
    inventory_type: u32,
    display_info_id: u32,
    start_quest: u32,
    spell_index: u8,
    /// The template's on-use SPELL id — what the cast tail's in-flight guard is keyed on
    /// (decision 0908); `None` = this item casts nothing.
    use_spell: Option<u32>,
    /// `ItemInfo::unwraps_gift` for this instance — dispatcher arm #2.
    unwraps_gift: bool,
    /// `ItemInfo::opens_loot` for this template — dispatcher arm #8.
    opens_loot: bool,
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
