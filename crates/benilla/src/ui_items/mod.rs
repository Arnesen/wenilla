//! The app-side **container feed** (decision 0068 T2) — the inward half of the container seam
//! around [`benilla_ui::script`]'s `container` module.
//!
//! Each frame, the player's own descriptor names the bag layout (the PRIVATE `PACK_SLOT` array =
//! the backpack; `INV_SLOT` 19–22 = the equipped bags, each a container object with its own
//! `CONTAINER_FIELD_SLOT` array), the item store ([`crate::items::Items`]) resolves slot guids to
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
//! the real client's **equip-vs-use fork**: an *equippable* item (template `inventoryType != 0`)
//! goes out as `CMSG_AUTOEQUIP_ITEM` — a helm click puts the helm on — everything else as
//! `CMSG_USE_ITEM`. Server refusals (`SMSG_INVENTORY_CHANGE_FAILURE` → [`EquipErrors`]) surface on
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
use crate::net::ObjectStore;
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
/// so dragging a bag onto a bank bag slot routes through the existing swap drain unchanged).
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
        EQUIPMENT_BAG if (1..=23).contains(&slot1) || (64..=69).contains(&slot1) => {
            Some((BAG_PLAYER_INVENTORY, slot0))
        }
        _ => None,
    }
}

/// Inventory refusals (`SMSG_INVENTORY_CHANGE_FAILURE`) queued by the net bridge for the UI error
/// line — `(reason, required_level)`; the equip twin of [`crate::ui_action::CastErrors`].
#[derive(Resource, Default)]
pub(crate) struct EquipErrors(pub Vec<(u8, Option<u32>)>);

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

/// The FIRST bag position (wire `(bag_index, 0-based slot)`, [`wire_pos`]'s own output shape)
/// holding item `entry` — the backpack searched before the equipped bags, each in slot order
/// (matches [`count_of`]'s own walk order). `None` if nothing matches. The action-bar item-use
/// drain's resolve (decision 0216 §7, slice 4): an ITEM-kind action names an item id, not a bag
/// position, so clicking it must find SOME instance to act on. The real client's own resolve
/// order (does it prefer the backpack, or slot order at all?) is unverified — a §5 CONFIRM if a
/// future pin disagrees; this is the simplest reading that needs no new wire round-trip.
pub(crate) fn first_bag_slot(store: &ObjectFields, items: &Items, entry: u32) -> Option<(u8, u8)> {
    for i in 0..PACK_SLOTS {
        let guid = store.player_pack_slot(i).unwrap_or(0);
        if guid == 0 {
            continue;
        }
        if items.object(guid).and_then(|f| f.object_entry()) == Some(entry) {
            return Some((BAG_PLAYER_INVENTORY, SLOT_PACK_FIRST + i));
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
            if items.object(guid).and_then(|f| f.object_entry()) == Some(entry) {
                return Some((SLOT_BAG_FIRST + bag - 1, j));
            }
        }
    }
    None
}

/// The client's quality→color escape for an item link (`GetItemQualityColor`'s table) — shared by
/// [`feed`]'s bag links and [`crate::ui_char`]'s doll-slot links (one table, no drifting twins).
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
                    feed_containers.in_set(UnitFeed).before(UiInput),
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
    use super::{find_equip_slot, wire_pos};
    use benilla_ui::script::EQUIPMENT_BAG;

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
