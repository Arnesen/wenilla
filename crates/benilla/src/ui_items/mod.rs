//! The app-side **container feed** (decision 0068 T2) — the inward half of the container seam
//! around [`benilla_ui::script`]'s `container` module.
//!
//! Each frame, the player's own descriptor names the bag layout (the PRIVATE `PACK_SLOT` array =
//! the backpack; `INV_SLOT` 19–22 = the equipped bags, each a container object with its own
//! `CONTAINER_FIELD_SLOT` array; `KEYRING_SLOT` 81.. = the keyring, decision 0765 — a container
//! with no container *object*, exactly like the bank), the item store ([`crate::items::Items`]) resolves slot guids to
//! instances (entry, stack count), the template cache resolves entries to name/quality (ask-once
//! `ITEM_QUERY_SINGLE` — a slot whose answer is in flight shows as an unresolved occupied slot and
//! fills in when it lands), and `ItemDisplayInfo.dbc` turns the template's display id into the
//! icon. The assembled per-bag [`ContainerState`](benilla_ui::script::ContainerState)s are diffed
//! against what the VM holds and pushed with `BAG_UPDATE(bagID)` per changed bag (+ one trailing
//! `BAG_UPDATE_DELAYED`, the live client's batch-end signal that bag addons coalesce on) — the
//! [`feed`] submodule.
//!
//! The outward half ([`drain`]) drains `UseContainerItem(bagID, slot)` intents into the wire,
//! mapping the Lua bag space onto the wire's (backpack = bag 255 + the player-array slot 23…;
//! equipped bags = their own array slot 19–22 + 0-based inner slot — VERIFIED vmangos `Player.h`
//! enums + `UseItem::ReadFromWorldPacket`, shared by every drain here as [`wire_pos`]) and making
//! the real client's **equip-vs-use fork**: an *equippable* item (template `inventoryType != 0`,
//! and not a quest-starter) goes out as `CMSG_AUTOEQUIP_ITEM` — a helm click puts the helm on —
//! everything else through the one use fork ([`item_use_command`], our `CGItem::Use`), which sends
//! `CMSG_QUESTGIVER_QUERY_QUEST` for a quest-starter and `CMSG_USE_ITEM` for everything else
//! (decision 0664). Server refusals (`SMSG_INVENTORY_CHANGE_FAILURE` → [`EquipErrors`]) surface on
//! the red UI error line with the client's message strings. The cursor-payload drains (decision
//! 0216, whole-space since slice 2) ride the same wire map: a queued pick/place/swap move →
//! `CMSG_SWAP_INV_ITEM` (backpack-internal) or `CMSG_SWAP_ITEM` (either end an equipped bag) /
//! `CMSG_SPLIT_ITEM` (`drain::drain_container_moves`), a popup-confirmed destroy →
//! `CMSG_DESTROYITEM` (`drain::drain_container_destroys`).
//!
//! **The pending-op lock** ([`crate::pending_item_ops::PendingItemOps`], decision 0216 §4 /
//! byte-verified 0218 §3): every move/split/destroy drain locks the live-API `(bag, slot)`
//! positions it sends — both ends of a move/split ("a send locks both ends"), the one slot of a
//! destroy — until the descriptor's own field-update stream resolves the slot or the server
//! answers a non-zero `SMSG_INVENTORY_CHANGE_FAILURE`. [`feed::feed_containers`] reads it into
//! each pushed `ContainerSlot::locked` and fires `ITEM_LOCK_CHANGED` on every transition the
//! drains don't already fire themselves.

use benilla_protocol::messages::{BAG_PLAYER_INVENTORY, SLOT_BAG_FIRST, SLOT_PACK_FIRST};
use benilla_protocol::ObjectFields;
use benilla_ui::script::EQUIPMENT_BAG;
use bevy::prelude::*;

use crate::items::Items;
use crate::net::{NetCommands, ObjectStore};
use crate::pending_item_ops::{LockClearedByFailure, PendingItemOps};
use crate::ui_script::UiInput;
use crate::ui_unit::UnitFeed;

mod drain;
mod equip_error;
mod feed;

use drain::{
    drain_container_autoequips, drain_container_destroys, drain_container_moves,
    drain_container_uses, drain_inventory_uses,
};
use feed::{feed_containers, feed_item_sets, feed_item_stats, feed_player_req};

/// The backpack's fixed capacity (`PLAYER_FIELD_PACK_SLOT_1..` — 16 slots on the 1.12 wire).
pub(super) const PACK_SLOTS: u8 = 16;
/// The worn-equipment slots (`INV_SLOT` 0..18 — head through tabard, vmangos `PlayerSlots`), the
/// first region of the reference's inventory walk (wow-re `action-item-slot.md` §8.2).
pub(super) const EQUIPMENT_SLOTS: u8 = 19;
/// The first equipped-bag inventory slot (`INV_SLOT` 19..22 hold bags 1..4).
pub(super) const BAG_SLOT_FIRST: u8 = 19;
/// Equipped bag count (live-API bag ids 1..=4).
pub(super) const BAGS: u8 = 4;
/// The bank's generic capacity (`PLAYER_FIELD_BANK_SLOT_1..` — 24 slots, wire 39..62; decision
/// 0604: streamed at login like the backpack, the window only reveals them).
pub(super) const BANK_SLOTS: u8 = 24;
/// The first bank generic slot in the player array (vmangos `BANK_SLOT_ITEM_START`).
pub(super) const BANK_SLOT_FIRST: u8 = 39;
/// The first bank-bag slot in the player array (vmangos `BANK_SLOT_BAG_START`; wire 63..68 hold
/// bank bags 1..6, and — like equipped bags — a bank bag's own slot number IS its wire bag byte).
pub(super) const BANK_BAG_SLOT_FIRST: u8 = 63;
/// Bank bag count (live-API bag ids [`BANK_BAG_ID_FIRST`]..=10).
pub(super) const BANK_BAGS: u8 = 6;
/// The bank's live-API container id (`BANK_CONTAINER`, the reference `BankFrame.lua:1`).
pub(crate) const BANK_CONTAINER: i64 = -1;
/// The first bank-bag live-API container id (bank bags are containers 5..=10, the reference id
/// space: `NUM_BAG_SLOTS + 1 ..`).
pub(crate) const BANK_BAG_ID_FIRST: i64 = 5;
/// The keyring's live-API container id (`KEYRING_CONTAINER`, the reference
/// `MainMenuBarBagButtons.lua:1`; decision 0765).
pub(crate) const KEYRING_CONTAINER: i64 = -2;
/// The first keyring slot in the player array (vmangos `KEYRING_SLOT_START`).
pub(super) const KEYRING_SLOT_FIRST: u8 = 81;
/// Addressable keyring positions on this wire — vmangos `KEYRING_SLOT_END 97`, i.e. slots 81..96.
/// The descriptor *array* is 32 guids wide and the client's inventory walker scans all 32 (81–112,
/// `player_keyring_slot`'s note), but 97.. is not a valid position: the server's own enum comment
/// is "32 slots (only 16 are visible/accessible in UI)". How many of the 16 a player may actually
/// use is level-gated — [`keyring_size`].
pub(super) const KEYRING_SLOTS: u8 = 16;
/// `BagFamily` 9 = `BAG_FAMILY_KEYS` — the enum value that makes an item a *key*: what the server
/// routes into the keyring (`Player::_CanStoreItem`'s `pProto->BagFamily == BAG_FAMILY_KEYS` arm)
/// and what the reference's `HasKey` searches for ([`has_key`]).
pub(super) const BAG_FAMILY_KEYS: u32 = 9;

/// How many keyring slots a level-`level` player may use — the reference's own `GetKeyRingSize`
/// (`ContainerFrame.lua:773`), which the server enforces with the identical ladder
/// (`Player::GetMaxKeyringSize`, `Player.h:985`): **4** below 40, **8** at 40, **12** at 50,
/// **16** above 60 — that last rung is unreachable at 1.12's level cap of 60, and is transcribed
/// only because both the client and the server carry it. Both sides agreeing on the ladder is why
/// benilla can compute this rather than being told it.
///
/// The reference recomputes it in Lua from `UnitLevel("player")` at every use; benilla computes it
/// once here, feeds it as the keyring container's `num_slots`, and lets Lua's `GetKeyRingSize()`
/// read that back — one formula, one place, the same number.
pub(crate) fn keyring_size(level: u32) -> u32 {
    match level {
        61.. => 16,
        50..=60 => 12,
        40..=49 => 8,
        _ => 4,
    }
}

/// Lua (bag, 1-based slot) → wire `(bag_index, slot)` — the one mapping every drain shares
/// (uses/moves/splits/destroys/autoequips, decision 0216 §6, extended to [`EQUIPMENT_BAG`] by
/// decision 0208 phase 1b): bag `0` (the backpack) → the player's own grid
/// ([`BAG_PLAYER_INVENTORY`] + [`SLOT_PACK_FIRST`] + the 0-based slot); bags `1..=4` (an equipped
/// bag) → that bag's own player-array slot ([`SLOT_BAG_FIRST`] + `bag - 1`) + the 0-based inner
/// slot; [`EQUIPMENT_BAG`] (a doll slot, live ids 1..=23 — the 19 equipment slots plus the four
/// equipped-bag icons Bag0Slot=20..Bag3Slot=23; ammo 0 stays a named deferral) → the SAME player
/// grid, `slot1 - 1` directly (`GetInventorySlotInfo`'s live id minus one IS the wire slot —
/// HeadSlot 1 → wire 0 … Tabard 19 → wire 18, Bag0Slot 20 → wire 19 = `INVENTORY_SLOT_BAG_START`
/// … Bag3Slot 23 → wire 22). Both backpack AND doll positions land on [`BAG_PLAYER_INVENTORY`], so
/// the existing move drain's "both ends 255 ⇒ `CMSG_SWAP_INV_ITEM`" branch already routes
/// doll↔backpack, doll↔doll, and a bag dragged from the backpack onto a bag slot (the equip) with
/// no change of its own. The bank (decision 0604) rides the same player-array convention:
/// [`BANK_CONTAINER`] (the 24 generic slots) → `(255, 39..62)`; bank bags 5..=10 → the bag's own
/// player-array slot 63..68 as the wire bag byte (exactly the equipped-bag rule); and the doll
/// space grows the bank-bag *buttons* as live ids 64..69 (the same "live id − 1 = wire slot" law,
/// so dragging a bag onto a bank bag slot routes through the existing swap drain unchanged). The
/// **keyring** ([`KEYRING_CONTAINER`], decision 0765) is the plainest case of all: its slots ARE
/// player-array slots ([`KEYRING_SLOT_FIRST`] + the 0-based slot), so every keyring move lands on
/// [`BAG_PLAYER_INVENTORY`] and rides the existing `CMSG_SWAP_INV_ITEM` branch to/from the
/// backpack and the doll, or `CMSG_SWAP_ITEM` when the other end is an equipped bag — no drain
/// changed for it. Ranged at the wire's [`KEYRING_SLOTS`] (81..96), NOT the level-gated
/// [`keyring_size`]: a click past the unlocked count is the server's refusal to give, not ours to
/// pre-empt (and the window never draws those slots anyway).
/// `None` for `slot1 == 0` or a slot past the bag's/doll's range.
pub(crate) fn wire_pos(bag: i64, slot1: u32) -> Option<(u8, u8)> {
    let slot0 = u8::try_from(slot1.checked_sub(1)?).ok()?;
    match bag {
        0 if slot0 < PACK_SLOTS => Some((BAG_PLAYER_INVENTORY, SLOT_PACK_FIRST + slot0)),
        1..=4 if slot0 < 36 => Some((SLOT_BAG_FIRST + (bag as u8 - 1), slot0)),
        BANK_CONTAINER if slot0 < BANK_SLOTS => {
            Some((BAG_PLAYER_INVENTORY, BANK_SLOT_FIRST + slot0))
        }
        5..=10 if slot0 < 36 => {
            Some((BANK_BAG_SLOT_FIRST + (bag - BANK_BAG_ID_FIRST) as u8, slot0))
        }
        KEYRING_CONTAINER if slot0 < KEYRING_SLOTS => {
            Some((BAG_PLAYER_INVENTORY, KEYRING_SLOT_FIRST + slot0))
        }
        EQUIPMENT_BAG if (1..=23).contains(&slot1) || (64..=69).contains(&slot1) => {
            Some((BAG_PLAYER_INVENTORY, slot0))
        }
        _ => None,
    }
}

/// One inventory refusal off the wire — everything the error line's two argument-taking reasons
/// need. Both fills are per-reason and neither is ever set for the other's code, so they ride as
/// plain fields rather than an enum (decision 0916: exactly two of the 67 reasons format an
/// argument, and the reference sources them from different places).
#[derive(Debug, Clone, Copy)]
pub(crate) struct EquipError {
    /// The wire `InventoryResult`.
    pub reason: u8,
    /// Reason 1's `%d` — the packet's own `requiredLevel` field.
    pub required_level: Option<u32>,
    /// Reason 16's `%s` source — the destination bag's ABSOLUTE player slot (255 = the player's
    /// own array, which names no bag). Not read off the wire for the message: it is a *slot*, and
    /// the drain resolves it to the bag's `BagFamily` name.
    pub bag_slot: u8,
}

/// Inventory refusals (`SMSG_INVENTORY_CHANGE_FAILURE`) queued by the net bridge for the UI error
/// line — the equip twin of [`crate::ui_action::CastErrors`].
#[derive(Resource, Default)]
pub(crate) struct EquipErrors(pub Vec<EquipError>);

/// Pre-formatted red error lines from the net drain (`UI_ERROR_MESSAGE` verbatim text — the
/// death durability notice today; anything whose message isn't a code map). Drained beside
/// [`EquipErrors`] by the container feed.
#[derive(bevy::prelude::Resource, Default)]
pub(crate) struct UiErrorLines(pub Vec<String>);

/// The item guid in a Lua-space bag slot, read off the player descriptor (backpack), the bag
/// object's own slot array, or — [`EQUIPMENT_BAG`], decision 0208 phase 1b — the player
/// descriptor's own `INV_SLOT` array directly (`slot0` is already the wire `EQUIPMENT_SLOT_*`
/// id, [`wire_pos`]'s own convention). The same resolution the feed does.
pub(crate) fn slot_guid(store: &ObjectFields, bag: i64, slot0: u8, items: &Items) -> Option<u64> {
    match bag {
        0 => store.player_pack_slot(slot0).filter(|g| *g != 0),
        1..=4 => {
            let bag_guid = store
                .player_inv_slot(BAG_SLOT_FIRST + bag as u8 - 1)
                .filter(|g| *g != 0)?;
            items
                .object(bag_guid)?
                .container_slot(slot0)
                .filter(|g| *g != 0)
        }
        BANK_CONTAINER => store.player_bank_slot(slot0).filter(|g| *g != 0),
        KEYRING_CONTAINER => store.player_keyring_slot(slot0).filter(|g| *g != 0),
        5..=10 => {
            let bag_guid = store
                .player_bank_bag_slot((bag - BANK_BAG_ID_FIRST) as u8)
                .filter(|g| *g != 0)?;
            items
                .object(bag_guid)?
                .container_slot(slot0)
                .filter(|g| *g != 0)
        }
        // The doll: equipment/bag icons read the INV array (its accessor caps at 23); the
        // bank-bag *buttons* (wire 63..68) read their own descriptor array (decision 0604).
        EQUIPMENT_BAG
            if (BANK_BAG_SLOT_FIRST..BANK_BAG_SLOT_FIRST + BANK_BAGS).contains(&slot0) =>
        {
            store
                .player_bank_bag_slot(slot0 - BANK_BAG_SLOT_FIRST)
                .filter(|g| *g != 0)
        }
        EQUIPMENT_BAG => store.player_inv_slot(slot0).filter(|g| *g != 0),
        _ => None,
    }
}

/// `(item guid, stack count)` at a Lua-space `(bag, 1-based slot)` — [`PendingItemOps`]'s baseline
/// unit ([`slot_guid`] plus the count field, since a partial split-merge/destroy changes only the
/// count, never the guid; see `crate::pending_item_ops`'s doc on why the lock tracks both). `(0,
/// 0)` for an empty slot, an absent player, or a slot past the bag's range.
pub(crate) fn slot_guid_count(
    store: Option<&ObjectStore>,
    bag: i64,
    slot1: u32,
    items: &Items,
) -> (u64, u32) {
    let Some(store) = store else {
        return (0, 0);
    };
    let slot0 = slot1.saturating_sub(1) as u8;
    match slot_guid(&store.0, bag, slot0, items) {
        Some(guid) => {
            let count = items
                .object(guid)
                .and_then(|f| f.item_stack_count())
                .unwrap_or(1);
            (guid, count)
        }
        None => (0, 0),
    }
}

/// Count of item `entry` across the backpack + equipped bags. The quest-log feed
/// ([`crate::ui_quest_log`]) needs this for an item-collection objective: unlike a creature/GO
/// objective, item-objective progress is *not* one of the `PLAYER_QUEST_LOG` slot's 6-bit counters
/// — the wire pin's finding is that the real client counts bag items itself, so this walks the same
/// slot arrays the feed does and sums matching entries' stack counts. An unresolved slot (its item
/// template still in flight) can't be matched to an entry and is skipped this frame; it counts once
/// the answer lands and the feed reruns.
pub(crate) fn count_of(store: &ObjectFields, items: &Items, entry: u32) -> u32 {
    let mut total = 0u32;
    for i in 0..PACK_SLOTS {
        let guid = store.player_pack_slot(i).unwrap_or(0);
        if guid == 0 {
            continue;
        }
        if let Some(fields) = items.object(guid) {
            if fields.object_entry() == Some(entry) {
                total += fields.item_stack_count().unwrap_or(1);
            }
        }
    }
    for bag in 1..=BAGS {
        let bag_guid = store.player_inv_slot(BAG_SLOT_FIRST + bag - 1).unwrap_or(0);
        if bag_guid == 0 {
            continue;
        }
        let Some(bag_fields) = items.object(bag_guid) else {
            continue;
        };
        let num_slots = bag_fields.container_num_slots().unwrap_or(0).min(36) as u8;
        for j in 0..num_slots {
            let guid = bag_fields.container_slot(j).unwrap_or(0);
            if guid == 0 {
                continue;
            }
            if let Some(fields) = items.object(guid) {
                if fields.object_entry() == Some(entry) {
                    total += fields.item_stack_count().unwrap_or(1);
                }
            }
        }
    }
    total
}

/// How far [`find_item`] looks, and which copies count — the two mode bits the reference's own
/// callers pass into the inventory walker `0x622420` (wow-re `action-item-slot.md` §8.2).
#[derive(Clone, Copy, Default)]
pub(crate) struct ItemSearch {
    /// Mode `1` alone: the **equipment slots only** (0–18), no expansion. The equip-vs-use fork's
    /// first stage — "is a copy of this already worn?" (`0x4e5fe7`'s `push 1`).
    pub(crate) equipment_only: bool,
    /// Mode bit `0x20`: skip a copy whose live `ITEM_FIELD_SPELL_CHARGES[0]` is `0` — the use
    /// leg sets it when the TEMPLATE says the item has finite charges, so a click reaches a copy
    /// that still has uses left instead of a spent one. Containers are never skipped by it.
    pub(crate) live_charges_only: bool,
}

/// Where a copy of item `entry` is: the wire `(bag_index, 0-based slot)` pair ([`wire_pos`]'s own
/// output shape) plus the **instance guid** that occupies it, since the use fork needs it
/// ([`item_use_command`]). This is the reference's inventory search, byte-verified (wow-re
/// `action-item-slot.md` §8.2: the walker `0x622420` over `PLAYER_FIELD_INV_SLOT_HEAD`, predicate
/// `OBJECT_FIELD_ENTRY` equality).
///
/// **Order is load-bearing and is the reference's, not a guess** (it was one until 2026-07-26 —
/// decision 0666 supersedes 0216 §7's "unverified but necessary" note): a single ascending pass
/// over the player's own slot array, recursing depth-first into each container as it is passed —
///
/// > equipment 0–18 → bag slot 19 (then all of bag 1's contents) → 20 (+contents) → 21 (+contents)
/// > → 22 (+contents) → backpack 23–38 → keyring 81–112.
///
/// Two things that fall out and both matter: **equipment is searched FIRST** (the old walk never
/// looked at it at all, so an equipped trinket's action button was inert), and **a bag's contents
/// come before the backpack** (the old walk had the backpack first). Bank and buyback are never
/// searched — the walker's default mode expands to `0x47`, which omits the bank's `0x08`, and no
/// bit exists for buyback at all. The **keyring** (mode bit `0x40`, in that `0x47`) is last in the
/// order and is walked here now that benilla models it (decision 0765) — a key with an on-use
/// spell dropped on the action bar has to find its copy somewhere, and the keyring is the only
/// place the server ever puts one.
pub(crate) fn find_item(
    store: &ObjectFields,
    items: &Items,
    entry: u32,
    search: ItemSearch,
) -> Option<(u8, u8, u64)> {
    // One candidate: the entry must match, and — under the charges filter — the instance must
    // have uses left. A container is exempt from the charge test (the walker's own carve-out).
    let hit = |guid: u64| -> bool {
        if guid == 0 {
            return false;
        }
        let Some(f) = items.object(guid) else {
            return false;
        };
        if f.object_entry() != Some(entry) {
            return false;
        }
        if search.live_charges_only && f.container_num_slots().is_none_or(|n| n == 0) {
            return f.item_spell_charges(0).is_none_or(|c| c != 0);
        }
        true
    };

    // Equipment 0–18, then the four equipped-bag slots 19–22 — the bag OBJECT is a candidate in
    // its own right before its contents are (a bag on the bar is a real, placeable action).
    for i in 0..EQUIPMENT_SLOTS {
        let guid = store.player_inv_slot(i).unwrap_or(0);
        if hit(guid) {
            return Some((BAG_PLAYER_INVENTORY, i, guid));
        }
    }
    if search.equipment_only {
        return None;
    }
    for bag in 0..BAGS {
        let bag_slot = BAG_SLOT_FIRST + bag;
        let bag_guid = store.player_inv_slot(bag_slot).unwrap_or(0);
        if hit(bag_guid) {
            return Some((BAG_PLAYER_INVENTORY, bag_slot, bag_guid));
        }
        let Some(bag_fields) = items.object(bag_guid) else {
            continue;
        };
        let num_slots = bag_fields.container_num_slots().unwrap_or(0).min(36) as u8;
        for j in 0..num_slots {
            let guid = bag_fields.container_slot(j).unwrap_or(0);
            if hit(guid) {
                return Some((bag_slot, j, guid));
            }
        }
    }
    for i in 0..PACK_SLOTS {
        let guid = store.player_pack_slot(i).unwrap_or(0);
        if hit(guid) {
            return Some((BAG_PLAYER_INVENTORY, SLOT_PACK_FIRST + i, guid));
        }
    }
    // The keyring band, last (mode bit 0x40). Walked over the ADDRESSABLE 16, not the level-gated
    // count: a slot the level hasn't unlocked can't hold anything, so the extra reads cost nothing
    // and the walk needs no level in hand.
    for i in 0..KEYRING_SLOTS {
        let guid = store.player_keyring_slot(i).unwrap_or(0);
        if hit(guid) {
            return Some((BAG_PLAYER_INVENTORY, KEYRING_SLOT_FIRST + i, guid));
        }
    }
    None
}

/// The reference's **`HasKey()`** (`0x48ae90`) — "does this player own a key at all?", the one
/// gate that decides whether the keyring exists in the UI (decision 0765). Byte-read: it fetches
/// the active player, then runs the same inventory walker `find_item` transcribes
/// (`0x6223a0` → `0x622420`) with predicate `0x6223d0` — `ItemTemplate.BagFamily == 9`
/// ([`BAG_FAMILY_KEYS`]; `template+0x1d0` is the record's last int32, and `BagFamily` is the last
/// field of `SMSG_ITEM_QUERY_SINGLE_RESPONSE`) — and pushes `1` on a hit, `nil` otherwise.
///
/// **The mode is `0x4f`, not the walker's default `0x47`**: equipment `0x01` | bag slots `0x02` |
/// backpack `0x04` | **bank + bank bags `0x08`** | keyring `0x40`. So a key sitting in the *bank*
/// still shows the keyring — the one region the ordinary item search skips. Buyback has no bit and
/// is never searched, here or anywhere.
///
/// A slot whose item template is still in flight can't be judged and reads as "not a key"; the
/// answer lands within a frame or two and the feed re-pushes.
pub(crate) fn has_key(store: &ObjectFields, items: &mut Items, commands: &NetCommands) -> bool {
    // Every guid mode 0x4f reaches, in the walker's own order — a container is recursed into as it
    // is passed (the depth-first rule), which is why each bag's contents follow its own slot.
    // Collected first, then judged: the template lookup needs `items` mutably (the ask-once query).
    fn contents(bag_guid: u64, items: &Items, out: &mut Vec<u64>) {
        let Some(f) = items.object(bag_guid) else {
            return;
        };
        let n = f.container_num_slots().unwrap_or(0).min(36) as u8;
        out.extend((0..n).map(|j| f.container_slot(j).unwrap_or(0)));
    }
    let mut guids = Vec::new();
    for i in 0..EQUIPMENT_SLOTS {
        guids.push(store.player_inv_slot(i).unwrap_or(0));
    }
    for bag in 0..BAGS {
        let bag_guid = store.player_inv_slot(BAG_SLOT_FIRST + bag).unwrap_or(0);
        guids.push(bag_guid);
        contents(bag_guid, items, &mut guids);
    }
    for i in 0..PACK_SLOTS {
        guids.push(store.player_pack_slot(i).unwrap_or(0));
    }
    for i in 0..BANK_SLOTS {
        guids.push(store.player_bank_slot(i).unwrap_or(0));
    }
    for bag in 0..BANK_BAGS {
        let bag_guid = store.player_bank_bag_slot(bag).unwrap_or(0);
        guids.push(bag_guid);
        contents(bag_guid, items, &mut guids);
    }
    for i in 0..KEYRING_SLOTS {
        guids.push(store.player_keyring_slot(i).unwrap_or(0));
    }
    guids.into_iter().any(|guid| {
        if guid == 0 {
            return false;
        }
        let Some(entry) = items.object(guid).and_then(|f| f.object_entry()) else {
            return false;
        };
        items
            .template(entry, guid, commands)
            .is_some_and(|t| t.bag_family == BAG_FAMILY_KEYS)
    })
}

/// One resolved item-use click, as [`send_item_use`] needs it: the wire position, the two
/// template scalars the fork reads, and the on-use SPELL the cast tail is keyed on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ItemUse {
    /// The live instance's guid (`None` = it didn't resolve; the fork falls through to a plain use).
    pub(crate) guid: Option<u64>,
    /// Template `StartQuest` — non-zero diverts to the quest offer, ahead of the cast tail.
    pub(crate) start_quest: u32,
    /// The wire position (`255` = the player array).
    pub(crate) bag_index: u8,
    /// The wire slot, 0-based.
    pub(crate) slot: u8,
    /// The template spell BLOCK ordinal the server should cast (decision 0666).
    pub(crate) spell_index: u8,
    /// The template's ON_USE spell id — `0x5d8c80`'s answer: the first template block whose
    /// `SpellId != 0` **and** `SpellTrigger == 0`. `None` = there is no such block.
    pub(crate) use_spell: Option<u32>,
    /// The GameObject this use is aimed at — the key-in-a-lock arm (decision 0769), which is
    /// `CGItem::Use`'s own target argument. `None` for every ordinary click.
    pub(crate) on_object: Option<u64>,
}

/// **The item-use fork** — our `CGItem::Use` (`0x5d8d00`): the one place that decides what "using"
/// an item actually sends. The reference has exactly one such function and every use surface calls
/// it — the bag click (`Script::UseContainerItem` @ `0x4fa430`), the doll click
/// (`Script::UseInventoryItem` → `0x4c7af0`) and the action bar (`UseAction`'s engine @ `0x4e607b`)
/// — so the fork lives here rather than in any one drain (decision 0664: three call sites each
/// re-deriving it is exactly how the quest fork came to be missing from all three).
///
/// A template whose **`StartQuest`** is non-zero never goes out as `CMSG_USE_ITEM`: the client
/// sends `CMSG_QUESTGIVER_QUERY_QUEST{the ITEM's own guid, StartQuest}` (byte-verified — the
/// `[rec+0x1a8] != 0` branch at `0x5d8dcc` calls the `0x186` builder `0x5eab80` with the CGItem's
/// guid), and the server answers `SMSG_QUESTGIVER_QUEST_DETAILS` with the item as the giver
/// (vmangos `HandleQuestgiverQueryQuestOpcode` resolves an `HIGHGUID_ITEM` guid through
/// `TYPEMASK_CREATURE_GAMEOBJECT_OR_ITEM`) — i.e. the quest's accept panel. Sending `CMSG_USE_ITEM`
/// instead is what draws *"The item was not found."*: `HandleUseItemOpcode` refuses any item whose
/// `Spells[spellSlot].SpellId` is 0 with `EQUIP_ERR_ITEM_NOT_FOUND`, and **no** quest-starter
/// carries an on-use spell (0 of the 215 in live `mangos.item_template`).
///
/// `guid: None` (the template/instance didn't resolve) falls through to the plain use, whose
/// refusal is at least visible — the same fallback the equip fork makes.
///
/// [`ItemUseRoute`] is that decision as a value — the pure fork, split from [`send_item_use`] so
/// the law is testable without a World.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ItemUseRoute {
    /// Arm #3 (`0x5d8dd2`): a non-zero `StartQuest` offers its quest and **returns**, long before
    /// the cast tail at `0x5d9249`. An offer is not a cast.
    QuestOffer { npc: u64, quest: u32 },
    /// The **toggle-cancel** arm (`0x5d9234`–`0x5d9246`): the item's own ON_USE spell is live on
    /// the caster as a cancelable active-icon aura, so the click sends `CMSG_CANCEL_AURA` for it
    /// and **returns `false` with no cast** — the item seam's separately-compiled twin of the
    /// action button's spell-branch toggle (`0x4e55f0`/`0x4e60c1`, [`crate::ui_action::toggle`]).
    /// This is what makes clicking your mount item while mounted **dismount** you.
    ToggleCancel(u32),
    /// The cast tail (`0x5d9249`–`0x5d9258` → `0x6e5a90` → `TryCast`): run the whole ladder.
    Cast(u32),
    /// `0x5d8c80` found no ON_USE block, so it returns 0 and the tail hands `TryCast` spell id
    /// **0** — whose rec lookup `[0xc0d788 + 0]` is null and whose `6e4bac` bail returns false
    /// with **nothing sent**. (Before decision 0914 we sent anyway, and vmangos answered
    /// `EQUIP_ERR_ITEM_NOT_FOUND` — a red "Item not found." the reference never shows.)
    Nothing,
}

/// [`ItemUseRoute`]'s decision. `guid: None` (the instance never resolved) cannot address a
/// questgiver, so it falls through to the ordinary path — the same fallback the equip fork makes.
///
/// `aura_cancels` is the toggle predicate applied to the item's ON_USE spell id — the caller's
/// [`crate::ui_action::toggle::active_action_toggle`], passed in so this stays a pure function of
/// the click. The **order is the reference's**: the quest offer forks first (`0x5d8dcc`, well
/// above), then the toggle scan (`0x5d9157`), then the cast tail (`0x5d9249`).
pub(crate) fn item_use_route(it: ItemUse, aura_cancels: impl Fn(u32) -> bool) -> ItemUseRoute {
    if let Some(npc) = it.guid.filter(|_| it.start_quest != 0) {
        return ItemUseRoute::QuestOffer {
            npc,
            quest: it.start_quest,
        };
    }
    match it.use_spell {
        Some(spell) if aura_cancels(spell) => ItemUseRoute::ToggleCancel(spell),
        Some(spell) => ItemUseRoute::Cast(spell),
        None => ItemUseRoute::Nothing,
    }
}

/// **The item-use fork's cast tail** — the only place a `CMSG_USE_ITEM` leaves benilla, and the
/// seam where an item use enters the *one* cast ladder.
///
/// The reference has no separate item-send path at all. `CGItem::Use 0x5d8d00`'s ordinary tail
/// (`5d9249`–`5d9258`) calls `0x6e5a90`, whose whole 54-byte body is `call 0x6e4b60` —
/// **`TryCast`**, the same function the spellbook, `/cast` and the action bar's SPELL arm run,
/// with the **item as an ordinary argument** (`ret 0xc`). Inside TryCast that argument is read
/// exactly twice, and neither read skips a gate: `6e4d76` computes a display flag ahead of the
/// IsCasting check, and `6e4f33` forwards the item to the requirement validator `0x6094f0`. The
/// commit `SendCast 0x6e54f0` then picks the opcode from it (`0x6e57d8 push 0xab`). So an item
/// use takes the whole ladder — cooldown, GCD, in-flight, mounted, moving, form, reagents, target
/// bind, range — and [`crate::ui_action::CastLadder::send`] is where all of that lives (decision
/// 0914; verified at the bytes in wow-re's `system/ui/scratch/disasm-full.txt`, corroborated by
/// its `action-item-slot.md` §8 and `cursor-system.md` §8.4a).
///
/// Decision 0908 put only the in-flight rung here — the fix for the director's B200, where
/// double-clicking a mount shipped a second `CMSG_USE_ITEM`, vmangos answered it
/// `SPELL_FAILED_SPELL_IN_PROGRESS`, and the failure (spell-id-keyed, naming the *same* spell as
/// the running cast) red-faded the running bar while the cast completed anyway. 0914 finishes the
/// job: the rest of the ladder was still bypassed, so a bag click ignored cooldowns, a mounted or
/// moving item click sent a doomed packet, and the bar's ITEM arm carried a private duplicate of
/// the cooldown rung the bag and doll clicks never got.
///
/// The **toggle-cancel** arm is this seam's other half, and it is why clicking your mount item
/// while mounted dismounts you rather than drawing "You are mounted". `CGItem::Use` carries a
/// separately-compiled twin of the action button's spell-branch toggle, `0x5d9157`–`0x5d9246`,
/// sitting *above* the cast tail:
///
/// ```text
/// 5d915a  edi = OnUseSpellId(item)              ; 0x5d8c80, the same block scan use_spell reads
/// 5d917c  SpellRec[edi] + 0x1d8 (ActiveIconID) != 0   else fall through to the cast
/// 5d9197  for slot in 0..0x30:                  ; UNIT_FIELD_AURA[48], block +0xa4
/// 5d919a      aura[slot] == edi
/// 5d91b7   && AURAFLAGS nibble bit0 set         ; block +0x164, 2 slots per byte
/// 5d91c5      -> 5d9234: CancelAura(edi) (0x6e7040 -> CMSG_CANCEL_AURA 0x136), return false
/// ```
///
/// It is the identical predicate to `0x4e55f0` — [`crate::ui_action::toggle::active_action_toggle`]
/// — just reached with the *item's* spell instead of the slot's, so the one predicate serves both
/// (wow-re `shapeshift-plaincast-toggle.md`'s own `0x6e7040` call-site census lists `0x5d9237`
/// under `0x5d8d00` as "action button, container-**item** branch").
///
/// Returns whether anything left for the server — `false` for the reference's silent no-op.
pub(crate) fn send_item_use(
    it: ItemUse,
    ctx: &crate::ui_action::cast_target::CastContext,
    ladder: &mut crate::ui_action::CastLadder,
) -> bool {
    // The toggle predicate, resolved once against the caster's live aura slots. Both inputs are
    // already here: the spell's ActiveIconID column and the caster's descriptor.
    let aura_cancels = |spell: u32| {
        let Some(d) = ladder.spells.as_ref().and_then(|s| s.catalog.get(spell)) else {
            return false;
        };
        ctx.rel
            .self_store
            .is_some_and(|store| crate::ui_action::toggle::active_action_toggle(spell, d, store))
    };
    match item_use_route(it, aura_cancels) {
        ItemUseRoute::ToggleCancel(spell) => {
            debug!("ui_items: item use {spell} re-pressed — its aura cancels, no cast");
            let _ = ladder
                .commands
                .0
                .send(crate::net::ClientCommand::CancelAura { spell_id: spell });
            true
        }
        ItemUseRoute::QuestOffer { npc, quest } => {
            let _ = ladder
                .commands
                .0
                .send(crate::net::ClientCommand::QuestgiverQuery { npc, quest });
            true
        }
        ItemUseRoute::Nothing => {
            debug!(
                "ui_items: the item at wire {}/{} has no ON_USE block — nothing sent (TryCast's null-rec bail)",
                it.bag_index, it.slot
            );
            false
        }
        ItemUseRoute::Cast(spell) => {
            ladder.send(
                spell,
                ctx,
                crate::ui_action::CastCommit::Item {
                    bag_index: it.bag_index,
                    slot: it.slot,
                    spell_index: it.spell_index,
                    on_object: it.on_object,
                },
            );
            true
        }
    }
}

/// The client's quality→color escape for an item link (`GetItemQualityColor`'s table) — shared by
/// [`item_link`] and everything downstream of it (one table, no drifting twins).
///
/// VERIFIED against the 1.12.1 binary (`WoW.exe` 5875): `0x52ad90` indexes the seven-pointer table
/// at `0x854124` into the `|cffRRGGBB` literals at `0x8546dc`, and **clamps anything `>= 7` to
/// index 1** (white) — which is what the catch-all arm below is, not a defensive default.
pub(super) fn quality_color(quality: u32) -> &'static str {
    match quality {
        0 => "ff9d9d9d",
        2 => "ff1eff00",
        3 => "ff0070dd",
        4 => "ffa335ee",
        5 => "ffff8000",
        6 => "ffe6cc80",
        _ => "ffffffff",
    }
}

/// Build one item hyperlink — **the** item-link builder, the single owner of the escape shape.
///
/// VERIFIED against the 1.12.1 binary: `0x52adb0` is `SStrPrintf(dst, 0x400, fmt, …)` over
/// `fmt @0x8549c8 = "%s|Hitem:%d:%d:%d:%d|h[%s]|h%s"`, with the leading `%s` the [`quality_color`]
/// escape, the four `%d` = (item id, enchant id, random-property id, suffix factor), and the
/// trailing `%s` the `"|r"` reset (`0x844538`). Every link the client shows — bag, paperdoll,
/// inspect, loot-roll announcement, "You receive loot" — comes out of this one function, so ours
/// does too: five hand-rolled `format!` twins of this string is how one site (the receive line)
/// silently shipped a bare, uncoloured name.
pub(super) fn item_link_full(
    item_id: u32,
    enchant_id: u32,
    random_property_id: u32,
    suffix_factor: u32,
    name: &str,
    quality: u32,
) -> String {
    format!(
        "|c{}|Hitem:{item_id}:{enchant_id}:{random_property_id}:{suffix_factor}|h[{name}]|h|r",
        quality_color(quality)
    )
}

/// [`item_link_full`] for a caller that has no enchant/random-property ids in hand.
///
/// **Stated approximation (documented gap).** Bag, paperdoll and inspect items *do* carry an
/// enchant and a random property on the wire; we do not thread either into the link yet, and the
/// real client additionally appends the `ItemRandomProperties.dbc` suffix to the **name**
/// (`0x5d8b00`) so a random-suffix green reads "Chipped Claw of the Bear". Both are one
/// random-suffix arc, not per-call-site drift — which is why the zeros live here, once.
pub(super) fn item_link(item_id: u32, name: &str, quality: u32) -> String {
    item_link_full(item_id, 0, 0, 0, name, quality)
}

/// `INVTYPE_AMMO` — the projectile/ammo inventory type (arrows, bullets). Loaded via
/// `CMSG_SET_AMMO`, not the equip-swap wire (decision 0526); the equip drains fork on it.
pub(super) const INVTYPE_AMMO: u32 = 24;

/// The INVTYPE → live-API equip-slot(s) map decision 0208 phase 1b's "the fit rule" needs
/// (`cursor::CursorItem`/`InvSlotView`/`ContainerSlot`'s `equip_slots`), transcribed from vmangos
/// `ItemPrototype::GetAllowedEquipSlots` (`Objects/Item.cpp:577-696`) — the table
/// `Player::FindEquipSlot` (`Objects/Player.cpp:8440-8479`) walks to answer "where can this go".
/// Returns 1-based live ids (`GetInventorySlotInfo`'s own numbering, wire `EQUIPMENT_SLOT_*` + 1
/// — HeadSlot=1 … TabardSlot=19, the equipped-bag icons 20..23); empty = not equippable
/// (consumables, quest items, armor tokens with no vanilla slot, …).
///
/// Two named simplifications from the server's own function (0218 §4's residual: the client's
/// terminal equip-fit check, `0x5da1d0`, was never byte-pinned — this is the best-available
/// authority, corrected if a future pin disagrees):
/// - **`INVTYPE_WEAPON` always offers BOTH main and off hand** (the server's `canDualWield` gate
///   dropped): the real server only suggests the offhand slot when the class already knows dual
///   wield, but getting that wrong here only over-permits a `CURSOR_UPDATE` highlight — the
///   actual equip still round-trips through `SMSG_INVENTORY_CHANGE_FAILURE`
///   (`EQUIP_ERR_CANT_DUAL_WIELD`) if the class can't. Simpler than threading class into every
///   caller for a highlight-only consequence.
/// - **`INVTYPE_RELIC` answers no slots** (the server's own table gates it per-class onto the
///   ranged slot for Paladin/Druid/Shaman/Warlock librams/idols/totems): decision 0208 already
///   established the relic slot is vanilla-UI-invisible (`UnitHasRelicSlot` always false, no
///   relic slot ever shows on the 1.12 paper doll), so resolving this precisely drives no visible
///   interaction — a named, harmless gap rather than threading class through for a slot nothing
///   ever shows.
pub(super) fn find_equip_slot(inventory_type: u32) -> Vec<u8> {
    // Live-API ids (`char_stats::SLOT_INFO`'s own numbering): wire `EQUIPMENT_SLOT_*` + 1. The
    // ammo slot is the client's own `GetInventorySlotInfo("AmmoSlot")` == 0 (not a real equip slot;
    // ammo loads by entry via `CMSG_SET_AMMO`, decision 0526) — it just names the fit-rule target.
    const AMMO: u8 = 0;
    const HEAD: u8 = 1;
    const NECK: u8 = 2;
    const SHOULDERS: u8 = 3;
    const BODY: u8 = 4; // the shirt slot (EQUIPMENT_SLOT_BODY)
    const CHEST: u8 = 5;
    const WAIST: u8 = 6;
    const LEGS: u8 = 7;
    const FEET: u8 = 8;
    const WRISTS: u8 = 9;
    const HANDS: u8 = 10;
    const FINGER1: u8 = 11;
    const FINGER2: u8 = 12;
    const TRINKET1: u8 = 13;
    const TRINKET2: u8 = 14;
    const BACK: u8 = 15;
    const MAINHAND: u8 = 16;
    const OFFHAND: u8 = 17;
    const RANGED: u8 = 18;
    const TABARD: u8 = 19;
    const BAG0: u8 = 20;
    const BAG1: u8 = 21;
    const BAG2: u8 = 22;
    const BAG3: u8 = 23;

    match inventory_type {
        1 => vec![HEAD],                    // INVTYPE_HEAD
        2 => vec![NECK],                    // INVTYPE_NECK
        3 => vec![SHOULDERS],               // INVTYPE_SHOULDERS
        4 => vec![BODY],                    // INVTYPE_BODY (the shirt)
        5 | 20 => vec![CHEST],              // INVTYPE_CHEST / INVTYPE_ROBE (same slot)
        6 => vec![WAIST],                   // INVTYPE_WAIST
        7 => vec![LEGS],                    // INVTYPE_LEGS
        8 => vec![FEET],                    // INVTYPE_FEET
        9 => vec![WRISTS],                  // INVTYPE_WRISTS
        10 => vec![HANDS],                  // INVTYPE_HANDS
        11 => vec![FINGER1, FINGER2],       // INVTYPE_FINGER
        12 => vec![TRINKET1, TRINKET2],     // INVTYPE_TRINKET
        13 => vec![MAINHAND, OFFHAND],      // INVTYPE_WEAPON (dual-wield simplified, see doc)
        14 => vec![OFFHAND],                // INVTYPE_SHIELD
        15 => vec![RANGED],                 // INVTYPE_RANGED
        16 => vec![BACK],                   // INVTYPE_CLOAK
        17 => vec![MAINHAND],               // INVTYPE_2HWEAPON
        18 => vec![BAG0, BAG1, BAG2, BAG3], // INVTYPE_BAG
        19 => vec![TABARD],                 // INVTYPE_TABARD
        21 => vec![MAINHAND],               // INVTYPE_WEAPONMAINHAND
        22 => vec![OFFHAND],                // INVTYPE_WEAPONOFFHAND
        23 => vec![OFFHAND],                // INVTYPE_HOLDABLE
        24 => vec![AMMO],                   // INVTYPE_AMMO → the ammo slot (loaded via SET_AMMO)
        25 => vec![RANGED],                 // INVTYPE_THROWN
        26 => vec![RANGED],                 // INVTYPE_RANGEDRIGHT
        // INVTYPE_NON_EQUIP(0), INVTYPE_QUIVER(27), INVTYPE_RELIC(28, see doc), and anything past
        // MAX_INVTYPE(29): not equippable.
        _ => Vec::new(),
    }
}

/// The ItemSet.dbc catalog — the tooltip SET block's row source (name/members/bonuses/skill).
#[derive(Resource)]
pub(crate) struct ItemSets(pub(crate) benilla_formats::ItemSetCatalog);

/// The ItemSubClass.dbc catalog — the slot|type line's alternate-proficiency and hidden-name
/// gates ([`feed`]'s template resolve).
#[derive(Resource)]
pub(crate) struct ItemSubClasses(pub(crate) benilla_formats::ItemSubClassCatalog);

/// The ItemBagFamily.dbc catalog — reason 16's `%s`, i.e. what a specialised bag accepts
/// ("Only Arrows can be placed in that."). See [`feed`]'s `bag_family_name`, decision 0916.
#[derive(Resource)]
pub(crate) struct ItemBagFamilies(pub(crate) benilla_formats::ItemBagFamilyCatalog);

/// Startup (after the MPQ chain opens): the item-tooltip DBCs. On failure a resource is simply
/// absent — set items render without their SET block, subclass gates read as absent.
fn load_item_dbcs(mut commands: Commands, world_assets: Option<Res<crate::assets::WorldAssets>>) {
    use crate::assets::LockRecover;
    let Some(world_assets) = world_assets else {
        return;
    };
    let mut chain = world_assets.chain.lock_recover();
    match benilla_formats::load_item_sets(&mut chain) {
        Ok(cat) => {
            info!("ui_items: ItemSet.dbc loaded ({} sets)", cat.len());
            commands.insert_resource(ItemSets(cat));
        }
        Err(e) => warn!("ui_items: ItemSet.dbc failed to load: {e:#}"),
    }
    match benilla_formats::load_item_sub_classes(&mut chain) {
        Ok(cat) => {
            info!("ui_items: ItemSubClass.dbc loaded ({} rows)", cat.len());
            commands.insert_resource(ItemSubClasses(cat));
        }
        Err(e) => warn!("ui_items: ItemSubClass.dbc failed to load: {e:#}"),
    }
    match benilla_formats::load_item_bag_families(&mut chain) {
        Ok(cat) => {
            info!(
                "ui_items: ItemBagFamily.dbc loaded ({} families)",
                cat.len()
            );
            commands.insert_resource(ItemBagFamilies(cat));
        }
        Err(e) => warn!("ui_items: ItemBagFamily.dbc failed to load: {e:#}"),
    }
}

pub(crate) struct UiItemsPlugin;

impl Plugin for UiItemsPlugin {
    fn build(&self, app: &mut App) {
        // The icon source — `ItemDisplayInfo.dbc` — is the `ItemDisplays` resource the equipment
        // renderer already loads (one parse serves the world and the bags).
        app.init_resource::<EquipErrors>()
            .init_resource::<UiErrorLines>()
            .init_resource::<PendingItemOps>()
            .init_resource::<LockClearedByFailure>()
            // AFTER the chain opens — a bare Startup slot raced AssetSet::Open and, when it
            // won, silently skipped every item DBC (no ItemSets/ItemSubClasses resource for the
            // whole session: set tooltips lost their SET block, the crafting book its headers).
            // Exposed by 0446's header law; every other DBC loader already orders this way.
            .add_systems(Startup, load_item_dbcs.after(crate::assets::AssetSet::Open))
            .add_systems(
                Update,
                (
                    // `.before(CooldownEvents)`: the slot cooldown triples must be in the VM
                    // before `feed_action_state`'s synchronous `BAG_UPDATE_COOLDOWN` makes the
                    // bag handlers re-read them (the set's own doc — else a fresh cooldown's
                    // pie waits for the NEXT store change).
                    feed_containers
                        .in_set(UnitFeed)
                        .before(crate::ui_action::CooldownEvents)
                        .before(UiInput),
                    // The shared item-tooltip store: answer stat asks before the input pass so a
                    // re-hover the very next frame already sees them.
                    feed_item_stats.in_set(UnitFeed).before(UiInput),
                    feed_item_sets.in_set(UnitFeed).before(UiInput),
                    feed_player_req.in_set(UnitFeed).before(UiInput),
                    // After the input pass, so a click's UseContainerItem goes out the same frame.
                    drain_container_uses.after(UiInput),
                    // The left-click pick/place/split drain — a queued move → CMSG_SWAP_INV_ITEM /
                    // CMSG_SWAP_ITEM / CMSG_SPLIT_ITEM (doll↔bag/doll↔doll included, decision 0208
                    // phase 1b — same drain, EQUIPMENT_BAG rides the existing wire map).
                    drain_container_moves.after(UiInput),
                    // The delete-confirm popup's accept — a queued destroy → CMSG_DESTROYITEM.
                    drain_container_destroys.after(UiInput),
                    // AutoEquipCursorItem's queue (decision 0208 phase 1b) → CMSG_AUTOEQUIP_ITEM.
                    drain_container_autoequips.after(UiInput),
                    // UseInventoryItem's queue (decision 0208 phase 1b) → CMSG_USE_ITEM against
                    // the equipped position.
                    drain_inventory_uses.after(UiInput),
                ),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        find_equip_slot, item_use_route, keyring_size, wire_pos, ItemUse, ItemUseRoute,
        KEYRING_CONTAINER,
    };
    use benilla_ui::script::EQUIPMENT_BAG;

    /// The keyring's level ladder, both ends of every rung — the reference's `GetKeyRingSize`
    /// (ContainerFrame.lua:773) and vmangos `GetMaxKeyringSize` (Player.h:985) agree on it exactly,
    /// which is what lets benilla compute the size instead of being told it. 60 is 1.12's cap, so
    /// the 16 rung is unreachable in play — it is here because both authorities carry it.
    #[test]
    fn keyring_size_walks_the_reference_ladder() {
        assert_eq!(keyring_size(1), 4);
        assert_eq!(keyring_size(39), 4, "the rung ends at 39");
        assert_eq!(keyring_size(40), 8, "40 opens the second rung");
        assert_eq!(keyring_size(49), 8);
        assert_eq!(keyring_size(50), 12);
        assert_eq!(keyring_size(60), 12, "the level cap still sits on 12");
        assert_eq!(keyring_size(61), 16, "> 60, unreachable in 1.12");
    }

    /// The keyring's wire mapping (decision 0765): its Lua slots are player-array slots 81.., so
    /// every one lands on the player's own grid — which is what makes keyring↔backpack moves ride
    /// the existing `CMSG_SWAP_INV_ITEM` branch with no drain change. Ranged at the wire's 16
    /// addressable positions (vmangos `KEYRING_SLOT_END` 97), not the level-gated count.
    #[test]
    fn wire_pos_maps_the_keyring_onto_the_player_grid() {
        assert_eq!(wire_pos(KEYRING_CONTAINER, 1), Some((255, 81)));
        assert_eq!(wire_pos(KEYRING_CONTAINER, 16), Some((255, 96)));
        assert_eq!(
            wire_pos(KEYRING_CONTAINER, 17),
            None,
            "97 is past KEYRING_SLOT_END — not a position on this wire"
        );
        assert_eq!(wire_pos(KEYRING_CONTAINER, 0), None);
    }

    /// The pure fork ([`item_use_route`], decisions 0664/0914), all four arms: a non-zero
    /// `StartQuest` diverts to `CMSG_QUESTGIVER_QUERY_QUEST` addressed to the ITEM's guid (arm #3,
    /// `0x5d8dd2` — it returns before the cast tail); an ON_USE spell whose aura is live on the
    /// caster **cancels** (`0x5d9234`) instead of casting; an ON_USE spell otherwise runs the
    /// ladder; **no** ON_USE block sends nothing at all, because `0x5d8c80` returns 0 and TryCast's
    /// null-rec bail (`6e4bac`) refuses spell id 0. That last arm is the one 0914 changed: we used
    /// to ship a `CMSG_USE_ITEM` vmangos answers `EQUIP_ERR_ITEM_NOT_FOUND` — a red "Item not
    /// found." on every right-click of a plain trade good, which the reference never shows.
    #[test]
    fn the_item_use_fork_routes_quest_offer_cast_and_nothing() {
        // "An Unsent Letter" (entry 2874, StartQuest 373 — live `mangos.item_template`): the item
        // guid is the questgiver, not the bag position. No quest-starter carries an on-use spell
        // (0 of the 215 live ones), but the quest arm wins even if one did.
        let letter = 0x4000_0000_0000_0BAD_u64;
        let it = |guid, start_quest, use_spell| ItemUse {
            guid,
            start_quest,
            bag_index: 255,
            slot: 23,
            spell_index: 0,
            use_spell,
            on_object: None,
        };
        let never = |_| false;
        assert_eq!(
            item_use_route(it(Some(letter), 373, None), never),
            ItemUseRoute::QuestOffer {
                npc: letter,
                quest: 373
            }
        );
        assert_eq!(
            item_use_route(it(Some(letter), 0, Some(8690)), never),
            ItemUseRoute::Cast(8690),
            "a hearthstone takes the cast tail"
        );
        assert_eq!(
            item_use_route(it(Some(letter), 0, None), never),
            ItemUseRoute::Nothing,
            "no ON_USE block — the ref sends nothing"
        );
        // No resolved instance (the template is still in flight): a query against guid 0 is
        // impossible, so the ordinary path runs — the equip fork's own fallback.
        assert_eq!(
            item_use_route(it(None, 373, Some(8690)), never),
            ItemUseRoute::Cast(8690)
        );
    }

    /// **The mount click, both ways** (the director, 08-03: "clicking the mount while mounted
    /// should dismount"). `CGItem::Use`'s toggle scan sits above the cast tail, so an item whose
    /// ON_USE spell is already live on the caster cancels its aura and never casts — and the same
    /// item clicked while the aura is *not* live takes the ordinary ladder. The predicate itself
    /// (ActiveIconID ≠ 0 ∧ a cancelable aura slot) is pinned in `ui_action::toggle`; here it is
    /// stubbed, so this pins the FORK and its ORDER.
    #[test]
    fn a_live_aura_makes_the_item_click_cancel_instead_of_cast() {
        const SUMMON_HORSE: u32 = 17462;
        let it = |start_quest, use_spell| ItemUse {
            guid: Some(0x4000_0000_0000_0BAD),
            start_quest,
            bag_index: 255,
            slot: 23,
            spell_index: 0,
            use_spell,
            on_object: None,
        };
        let mounted = |spell: u32| spell == SUMMON_HORSE;
        assert_eq!(
            item_use_route(it(0, Some(SUMMON_HORSE)), mounted),
            ItemUseRoute::ToggleCancel(SUMMON_HORSE),
            "mounted: the click dismounts — CMSG_CANCEL_AURA, no CMSG_USE_ITEM",
        );
        assert_eq!(
            item_use_route(it(0, Some(SUMMON_HORSE)), |_| false),
            ItemUseRoute::Cast(SUMMON_HORSE),
            "not mounted: the very same click casts",
        );
        // Order: the quest offer forks at `0x5d8dcc`, above the toggle scan at `0x5d9157`.
        assert_eq!(
            item_use_route(it(373, Some(SUMMON_HORSE)), mounted),
            ItemUseRoute::QuestOffer {
                npc: 0x4000_0000_0000_0BAD,
                quest: 373
            },
        );
        // …and an item with no ON_USE block has nothing to cancel, whatever the predicate says.
        assert_eq!(item_use_route(it(0, None), |_| true), ItemUseRoute::Nothing);
    }

    /// A dozen representative `InventoryType`s (decision 0208 phase 1b's own ask), spanning the
    /// single-slot rows, the two-slot rows, the weapon rows' MAINHAND/OFFHAND split, and the
    /// not-equippable rows — cross-checked against vmangos
    /// `ItemPrototype::GetAllowedEquipSlots` (`Objects/Item.cpp:577-696`).
    #[test]
    fn find_equip_slot_matches_the_vmangos_table() {
        assert_eq!(find_equip_slot(1), vec![1], "HEAD");
        assert_eq!(find_equip_slot(2), vec![2], "NECK");
        assert_eq!(find_equip_slot(4), vec![4], "BODY (shirt)");
        assert_eq!(find_equip_slot(5), vec![5], "CHEST");
        assert_eq!(find_equip_slot(20), vec![5], "ROBE aliases CHEST");
        assert_eq!(find_equip_slot(11), vec![11, 12], "FINGER, two slots");
        assert_eq!(find_equip_slot(12), vec![13, 14], "TRINKET, two slots");
        assert_eq!(
            find_equip_slot(13),
            vec![16, 17],
            "WEAPON offers both hands (dual-wield simplified)"
        );
        assert_eq!(find_equip_slot(14), vec![17], "SHIELD -> off hand");
        assert_eq!(find_equip_slot(15), vec![18], "RANGED");
        assert_eq!(find_equip_slot(16), vec![15], "CLOAK -> back");
        assert_eq!(find_equip_slot(17), vec![16], "2HWEAPON -> main hand only");
        assert_eq!(find_equip_slot(19), vec![19], "TABARD");
        assert_eq!(find_equip_slot(21), vec![16], "WEAPONMAINHAND");
        assert_eq!(find_equip_slot(22), vec![17], "WEAPONOFFHAND");
        assert_eq!(find_equip_slot(18), vec![20, 21, 22, 23], "BAG");
        assert_eq!(find_equip_slot(24), vec![0], "AMMO -> the ammo slot (id 0)");
        // Not equippable: no vanilla paper-doll slot, or a named deferral (quiver/relic).
        for t in [0u32, 27, 28, 100] {
            assert!(find_equip_slot(t).is_empty(), "inventory type {t}");
        }
    }

    /// [`wire_pos`]'s [`EQUIPMENT_BAG`] branch — the doll-slot mapping decision 0208 phase 1b
    /// adds: live id `n` (1..=23) → the SAME player grid ([`benilla_protocol::messages::
    /// BAG_PLAYER_INVENTORY`]) at wire slot `n - 1` — the 19 equipment slots plus the four
    /// equipped-bag icons (20..23 → wire 19..22, the drag-to-equip target); ammo (0) is refused,
    /// matching the engine's own `pickup_inventory_item` range guard.
    #[test]
    fn wire_pos_maps_equipment_bag_to_the_player_grid() {
        assert_eq!(wire_pos(EQUIPMENT_BAG, 1), Some((255, 0)), "HeadSlot");
        assert_eq!(wire_pos(EQUIPMENT_BAG, 19), Some((255, 18)), "TabardSlot");
        assert_eq!(wire_pos(EQUIPMENT_BAG, 16), Some((255, 15)), "MainHandSlot");
        assert_eq!(wire_pos(EQUIPMENT_BAG, 0), None, "ammo — out of scope");
        // The four equipped-bag icons map onto the wire's bag inventory slots (19..22).
        assert_eq!(wire_pos(EQUIPMENT_BAG, 20), Some((255, 19)), "Bag0Slot");
        assert_eq!(wire_pos(EQUIPMENT_BAG, 23), Some((255, 22)), "Bag3Slot");
        assert_eq!(wire_pos(EQUIPMENT_BAG, 24), None, "past the bag icons");
        // Both a backpack move and a doll move land on the SAME wire bag (255) — the existing
        // drain_container_moves branch ("both ends 255 ⇒ CMSG_SWAP_INV_ITEM") already routes
        // doll↔backpack and doll↔doll correctly with no code of its own to add.
        assert_eq!(
            wire_pos(0, 1).map(|(b, _)| b),
            wire_pos(EQUIPMENT_BAG, 1).map(|(b, _)| b)
        );
    }

    /// [`wire_pos`]'s bank arms (decision 0604): the 24 generic slots land on the player grid at
    /// wire 39..62; bank bags 5..=10 use their own player-array slot 63..68 as the wire bag byte
    /// (the equipped-bag rule); and the doll space carries the bank-bag *buttons* as live 64..69
    /// (live id − 1 = wire slot, so bag-into-bank-slot drags ride the existing swap drain).
    #[test]
    fn wire_pos_maps_the_bank_spaces() {
        use super::BANK_CONTAINER;
        // The 24 generic slots: live 1..24 → (255, 39..62).
        assert_eq!(wire_pos(BANK_CONTAINER, 1), Some((255, 39)));
        assert_eq!(wire_pos(BANK_CONTAINER, 24), Some((255, 62)));
        assert_eq!(wire_pos(BANK_CONTAINER, 25), None, "past the vault");
        assert_eq!(wire_pos(BANK_CONTAINER, 0), None);
        // Bank bags: container 5 is the bag in player-array slot 63, container 10 in 68.
        assert_eq!(wire_pos(5, 1), Some((63, 0)));
        assert_eq!(wire_pos(10, 36), Some((68, 35)));
        assert_eq!(wire_pos(11, 1), None, "past the bank bags");
        // The bank-bag buttons in doll space: live 64..69 → wire 63..68; the gap 24..63 refuses.
        assert_eq!(wire_pos(EQUIPMENT_BAG, 64), Some((255, 63)), "BankBag1");
        assert_eq!(wire_pos(EQUIPMENT_BAG, 69), Some((255, 68)), "BankBag6");
        assert_eq!(wire_pos(EQUIPMENT_BAG, 63), None, "the doll-space gap");
        assert_eq!(wire_pos(EQUIPMENT_BAG, 70), None, "past the bank bags");
    }
}

/// [`find_item`] — the reference's inventory walk (decision 0666). Everything here is about
/// **order**, because order is the whole finding: the walk it replaced never looked at equipment
/// at all (an equipped trinket's action button was inert) and put the backpack ahead of the bags.
#[cfg(test)]
mod find_item_tests {
    use super::{find_item, ItemSearch};
    use crate::items::Items;
    use benilla_protocol::ObjectFields;

    // Descriptor indices, raw (the module's own consts are private; the codebase's test idiom).
    const ENTRY: u16 = 3; // OBJECT_FIELD_ENTRY
    const CHARGES: u16 = 16; // ITEM_FIELD_SPELL_CHARGES[0]
    const NUM_SLOTS: u16 = 48; // CONTAINER_FIELD_NUM_SLOTS
    const SLOT_1: u16 = 50; // CONTAINER_FIELD_SLOT_1 (2 fields per guid)
    const INV_SLOT_HEAD: u16 = 486; // PLAYER_FIELD_INV_SLOT_HEAD (2 per guid, 23 slots)
    const PACK_SLOT_1: u16 = 532; // PLAYER_FIELD_PACK_SLOT_1 (2 per guid, 16 slots)
    const KEYRING_SLOT_1: u16 = 648; // PLAYER_FIELD_KEYRING_SLOT_1 (2 per guid, player slots 81..)

    const TRINKET: u32 = 12_930;
    const BAG: u32 = 4_500;

    /// A player whose slot array points at the given `(player-array index, guid)` pairs. Covers the
    /// three bands these tests use — equipment/bag buttons 0..22, backpack 23..38, keyring 81.. —
    /// each of which is its own descriptor array; the bank/buyback bands in between have no test
    /// that needs them.
    fn player(slots: &[(u16, u64)]) -> ObjectFields {
        let mut pairs = Vec::new();
        for &(idx, guid) in slots {
            let base = if idx < 23 {
                INV_SLOT_HEAD + 2 * idx
            } else if idx < 81 {
                PACK_SLOT_1 + 2 * (idx - 23)
            } else {
                KEYRING_SLOT_1 + 2 * (idx - 81)
            };
            pairs.push((base, guid as u32));
            pairs.push((base + 1, (guid >> 32) as u32));
        }
        ObjectFields::from_pairs(&pairs)
    }

    /// A plain item instance of `entry` (optionally with live charges).
    fn item(store: &mut Items, guid: u64, entry: u32, charges: Option<i32>) {
        let mut pairs = vec![(ENTRY, entry)];
        if let Some(c) = charges {
            pairs.push((CHARGES, c as u32));
        }
        store.insert_object(guid, ObjectFields::from_pairs(&pairs));
    }

    /// A container instance holding `contents` at its own inner slots.
    fn bag(store: &mut Items, guid: u64, entry: u32, contents: &[(u8, u64)]) {
        let mut pairs = vec![(ENTRY, entry), (NUM_SLOTS, 16)];
        for &(i, item_guid) in contents {
            pairs.push((SLOT_1 + 2 * u16::from(i), item_guid as u32));
            pairs.push((SLOT_1 + 2 * u16::from(i) + 1, (item_guid >> 32) as u32));
        }
        store.insert_object(guid, ObjectFields::from_pairs(&pairs));
    }

    const ALL: ItemSearch = ItemSearch {
        equipment_only: false,
        live_charges_only: false,
    };
    const WORN: ItemSearch = ItemSearch {
        equipment_only: true,
        live_charges_only: false,
    };
    const CHARGED: ItemSearch = ItemSearch {
        equipment_only: false,
        live_charges_only: true,
    };

    /// Equipment comes FIRST — the trinket case. A copy worn in trinket slot 13 wins over an
    /// identical copy sitting in the backpack, and the wire pair is the doll's `(255, 13)`.
    #[test]
    fn equipment_is_searched_before_everything_else() {
        let mut items = Items::default();
        item(&mut items, 0xE1, TRINKET, None);
        item(&mut items, 0xB1, TRINKET, None);
        let store = player(&[(13, 0xE1), (23, 0xB1)]);
        assert_eq!(
            find_item(&store, &items, TRINKET, ALL),
            Some((255, 13, 0xE1))
        );
    }

    /// …and the equipment-only stage stops at the doll: the backpack copy is invisible to it.
    /// This is the stage that decides USE-in-place vs EQUIP.
    #[test]
    fn the_equipment_only_stage_ignores_the_bags() {
        let mut items = Items::default();
        item(&mut items, 0xB1, TRINKET, None);
        let store = player(&[(23, 0xB1)]);
        assert_eq!(find_item(&store, &items, TRINKET, WORN), None);
        assert_eq!(
            find_item(&store, &items, TRINKET, ALL),
            Some((255, 23, 0xB1))
        );
    }

    /// A bag's CONTENTS come before the backpack (the walk recurses depth-first as it passes each
    /// container) — the leg the old backpack-first walk had backwards.
    #[test]
    fn bag_contents_precede_the_backpack() {
        let mut items = Items::default();
        item(&mut items, 0xC1, TRINKET, None);
        item(&mut items, 0xB1, TRINKET, None);
        bag(&mut items, 0xBA, BAG, &[(2, 0xC1)]);
        let store = player(&[(19, 0xBA), (23, 0xB1)]);
        assert_eq!(
            find_item(&store, &items, TRINKET, ALL),
            Some((19, 2, 0xC1)),
            "bag 1's inner slot 2, addressed by the bag's own player-array index"
        );
    }

    /// The bag OBJECT is a candidate in its own right, before its contents — a bag on the action
    /// bar is a real, placeable action (`InventoryType` 18 passes PlaceAction's filter).
    #[test]
    fn an_equipped_bag_is_found_as_itself() {
        let mut items = Items::default();
        bag(&mut items, 0xBA, BAG, &[]);
        let store = player(&[(19, 0xBA)]);
        assert_eq!(find_item(&store, &items, BAG, ALL), Some((255, 19, 0xBA)));
    }

    /// The mode-`0x20` charge filter skips a SPENT copy and returns one with uses left — so a
    /// click on a charged item reaches a copy that still works.
    #[test]
    fn the_charge_filter_skips_a_spent_copy() {
        let mut items = Items::default();
        item(&mut items, 0xB1, TRINKET, Some(0));
        item(&mut items, 0xB2, TRINKET, Some(3));
        let store = player(&[(23, 0xB1), (24, 0xB2)]);
        assert_eq!(
            find_item(&store, &items, TRINKET, ALL),
            Some((255, 23, 0xB1)),
            "without the filter the first copy wins, spent or not"
        );
        assert_eq!(
            find_item(&store, &items, TRINKET, CHARGED),
            Some((255, 24, 0xB2)),
            "with it, the spent copy is skipped"
        );
    }

    /// The keyring is the walk's LAST band (mode bit `0x40`) — a key that lives only there is still
    /// found, and its wire pair is the player array's own `(255, 81 + n)`. Before decision 0765 the
    /// walker stopped at the backpack, so a key on the action bar could never resolve its copy.
    #[test]
    fn a_key_in_the_keyring_is_found_last() {
        const KEY: u32 = 7_146; // The Scarlet Key
        let mut items = Items::default();
        item(&mut items, 0xE1, KEY, None);
        let store = player(&[(81, 0xE1)]);
        assert_eq!(find_item(&store, &items, KEY, ALL), Some((255, 81, 0xE1)));

        // ...and a copy anywhere earlier still wins: the keyring really is last, not first.
        item(&mut items, 0xE2, KEY, None);
        let store = player(&[(81, 0xE1), (23, 0xE2)]);
        assert_eq!(
            find_item(&store, &items, KEY, ALL),
            Some((255, 23, 0xE2)),
            "the backpack copy precedes the keyring one"
        );
    }

    /// `HasKey()` — the gate the whole keyring UI hangs off (decision 0765), and the one place the
    /// **bank** is searched. Byte-read from the reference's `0x48ae90`: predicate `BagFamily == 9`,
    /// mode `0x4f` = equipment | bag slots | backpack | BANK | keyring. So: an ordinary item is not
    /// a key wherever it sits; a key is a key wherever it sits, the bank included; and buyback —
    /// which has no mode bit at all — is never searched.
    #[test]
    fn has_key_finds_a_key_anywhere_the_reference_looks() {
        use super::{has_key, BAG_FAMILY_KEYS};
        use crate::items::{test_template, Items};
        use crate::net::NetCommands;

        const KEY: u32 = 7_146; // The Scarlet Key (bag_family 9, live mangos.item_template)
        const BREAD: u32 = 4_540;
        let (tx, _rx) = crossbeam_channel::unbounded();
        let commands = NetCommands(tx);

        let mut items = Items::default();
        let mut key_tpl = test_template("The Scarlet Key");
        key_tpl.bag_family = BAG_FAMILY_KEYS;
        items.insert_template(KEY, Some(key_tpl));
        items.insert_template(BREAD, Some(test_template("Tough Hunk of Bread")));
        item(&mut items, 0xF1, KEY, None);
        item(&mut items, 0xF2, BREAD, None);

        // Nothing at all.
        assert!(!has_key(&player(&[]), &mut items, &commands));
        // A non-key in the backpack is not a key.
        assert!(!has_key(&player(&[(23, 0xF2)]), &mut items, &commands));
        // The director's own case: the key sitting in keyring slot 1.
        assert!(has_key(&player(&[(81, 0xF1)]), &mut items, &commands));
        // And in the backpack, before it has been filed.
        assert!(has_key(&player(&[(23, 0xF1)]), &mut items, &commands));

        // The BANK — reachable only because HasKey passes 0x4f rather than the walker's default
        // 0x47. `find_item` must NOT see the same copy (its mode omits 0x08).
        let mut banked = std::collections::HashMap::new();
        banked.insert(39u16, 0xF1u64);
        let store = bank_player(&banked);
        assert!(
            has_key(&store, &mut items, &commands),
            "a key in the bank still gives you a keyring"
        );
        assert_eq!(
            find_item(&store, &items, KEY, ALL),
            None,
            "...while the ordinary item search never reaches the bank"
        );
    }

    /// A player with items in the BANK band (`PLAYER_FIELD_BANK_SLOT_1`), which `player` above
    /// deliberately does not cover.
    fn bank_player(slots: &std::collections::HashMap<u16, u64>) -> ObjectFields {
        const BANK_SLOT_1: u16 = 564; // PLAYER_FIELD_BANK_SLOT_1 (2 per guid, player slots 39..62)
        let mut pairs = Vec::new();
        for (&idx, &guid) in slots {
            let base = BANK_SLOT_1 + 2 * (idx - 39);
            pairs.push((base, guid as u32));
            pairs.push((base + 1, (guid >> 32) as u32));
        }
        ObjectFields::from_pairs(&pairs)
    }
}
