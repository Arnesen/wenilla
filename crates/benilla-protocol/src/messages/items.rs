//! Item messages — the T2 container groundwork (decision 0068's tier ladder: Bagnon needs item
//! identity), widened to the **full** 1.12.1 item template (decision 0274 P1: the tooltip builder
//! needs every line the real client can render). The 1.12 wire carries no item *templates* in
//! descriptors — like unit names, they answer a query pair: `CMSG_ITEM_QUERY_SINGLE` (entry + guid)
//! → `SMSG_ITEM_QUERY_SINGLE_RESPONSE` (VERIFIED vmangos `HandleItemQuerySingleOpcode`; opcodes
//! 86/88 `Opcodes_1_12_1.h`).
//!
//! [`ItemInfo`] now carries the response whole: identity (class/subclass/name — 4 name slots, the
//! server sends 1 + 3 empties — displayInfoID, quality, inventoryType, sheath), the buy/sell
//! economy, every requirement gate (level/skill/spell/honor rank/city rank/reputation,
//! allowable class/race), stacking (maxCount/stackable/containerSlots), the full 10-slot stat
//! block, all 5 damage blocks (block 0 also mirrors into the legacy `dmg_min`/`dmg_max`/`dmg_type`
//! fields existing consumers already key on), armor plus the 6-wide resistance run, ranged data,
//! all 5 spell-trigger slots (the first ON_USE slot still surfaces separately as `use_spell` — the
//! client's own cooldown-scan key), bonding, description, page/lock/material/random-property/set,
//! durability, and the area/map/bagFamily tail (VERIFIED field order vmangos
//! `HandleItemQuerySingleOpcode`, `ItemHandler.cpp:269-415`; every `SUPPORTED_CLIENT_BUILD`
//! conditional in that function evaluates *included* for build 5875). A **miss** (undiscovered/
//! unknown entry) is the lone `u32` of `entry | 0x8000_0000`, the same shape as the creature miss.

use std::io;

use crate::wire::{read_cstring, read_f32_le, read_i32_le, read_u32_le, read_u64_le, read_u8};

/// A full item-template answer (decision 0274 P1: the tooltip builder's source of truth; every
/// field the wire carries, none discarded). (`PartialEq` only: several fields are wire floats —
/// the damage bounds and `ranged_mod_range`.)
#[derive(Debug, Clone, PartialEq)]
pub struct ItemInfo {
    pub class: u32,
    pub subclass: u32,
    pub name: String,
    /// `ItemDisplayInfo.dbc` key — the icon/model resolve.
    pub display_info_id: u32,
    /// 0 poor … 6 artifact (the RF-55 quality-color table's index).
    pub quality: u32,
    /// `ItemPrototypeFlags` bitmask (conjured, lootable, indestructible, wrapper, no-equip-cooldown,
    /// …) — the tooltip's "Unique"/no-sell/no-disenchant lines key on bits here.
    pub flags: u32,
    /// `BuyPrice` — what a vendor charges per [`crate::messages::VendorItem::buy_count`]-sized
    /// stack, in copper.
    pub buy_price: u32,
    /// `SellPrice` — what a vendor pays per unit, in copper (the bag tooltip's money row while a
    /// merchant is open; 0 = unsellable → the "No sell price" line).
    pub sell_price: u32,
    /// `InventoryType` — the equip-slot family (1 head, 21/22 main/off-hand weapon, …); drives
    /// which paperdoll slot an item can go in and which visual-item field it feeds.
    pub inventory_type: u32,
    /// `AllowableClass` — a class bitmask; the all-bits-set sentinel (`-1`) means no class
    /// restriction, so this stays signed rather than reading as the unsigned `0xFFFF_FFFF`.
    pub allowable_class: i32,
    /// `AllowableRace` — the same bitmask shape as [`Self::allowable_class`], races instead.
    pub allowable_race: i32,
    /// `ItemLevel` — the repair-cost formula's `DurabilityCosts.dbc` row key (also a tooltip line).
    pub item_level: u32,
    /// `RequiredLevel` — the tooltip's "Requires Level N" line; 0 = no level requirement.
    pub required_level: u32,
    /// `RequiredSkill` — id from `SkillLine.dbc`; 0 = no skill requirement.
    pub required_skill: u32,
    /// `RequiredSkillRank` — the skill value [`Self::required_skill`] must meet or exceed.
    pub required_skill_rank: u32,
    /// `RequiredSpell` — id from `Spell.dbc`; the item is unusable without knowing this spell.
    pub required_spell: u32,
    /// `RequiredHonorRank`/`RequiredCityRank` — two more requirement gates the wire carries; the
    /// tooltip's requirement-line law for these two is unverified (folds in with decision 0274's
    /// §5 line-order dispatch).
    pub required_honor_rank: u32,
    pub required_city_rank: u32,
    /// `RequiredReputationFaction` — id from `Faction.dbc`; 0 = no reputation requirement.
    pub required_rep_faction: u32,
    /// `RequiredReputationRank` — the wire's own gate: the server sends 0 whenever
    /// [`Self::required_rep_faction`] is 0, even if the row has a nonzero rank (VERIFIED vmangos
    /// `ItemHandler.cpp:321-322`).
    pub required_rep_rank: u32,
    /// `MaxCount` — the account-wide cap this item enforces (0 = uncapped); the tooltip's "Unique"
    /// family of lines derive from this and [`Self::flags`].
    pub max_count: u32,
    /// `Stackable` — the max stack size a single slot can hold (1 = doesn't stack).
    pub stackable: u32,
    /// `ContainerSlots` — nonzero only for bag items (the number of slots the bag itself grants).
    pub container_slots: u32,
    /// The 10-slot `ItemStat` block, `(type, value)`, **filtered to nonzero entries** (type or
    /// value nonzero) in wire order — the tooltip's "+N Stat" lines (`ItemModType` at this build:
    /// 0 mana, 1 health, 3 agility, 4 strength, 5 intellect, 6 spirit, 7 stamina).
    pub stats: Vec<(u32, i32)>,
    /// The 5-slot `Damage` block, **filtered to entries with `max > 0`**, in wire order —
    /// secondary damage lines (e.g. a Fiery weapon's bonus Fire line) beyond the primary
    /// [`Self::dmg_min`]/[`Self::dmg_max`]/[`Self::dmg_type`], which always mirror block 0 whether
    /// or not it clears this filter.
    pub damages: Vec<ItemDamage>,
    /// Damage block 0's per-hit minimum (the tooltip's "X - Y Damage" line; 0 for non-weapons) —
    /// kept mirrored from `damages` block 0 for existing consumers.
    pub dmg_min: f32,
    /// Damage block 0's per-hit maximum.
    pub dmg_max: f32,
    /// Damage block 0's school (0 physical, 1 Holy … 6 Arcane — the tooltip's school suffix).
    pub dmg_type: u32,
    /// `Armor` — the first slot of the wire's 7-wide resistance run.
    pub armor: u32,
    /// The remaining 6 slots of the resistance run, in wire order: `[holy, fire, nature, frost,
    /// shadow, arcane]` (`int32` on the wire in vmangos's own `ItemPrototype` — a template's
    /// resistance can't go negative in practice, but the sign rides along).
    pub resistances: [i32; 6],
    /// Attack delay in milliseconds (the tooltip's "Speed" = delay / 1000).
    pub delay_ms: u32,
    /// `AmmoType` — the projectile family a ranged weapon consumes (0 none, 2 arrow, 3 bullet).
    pub ammo_type: u32,
    /// `RangedModRange` — a ranged weapon's range multiplier; the tooltip never shows this raw, it
    /// feeds the range formula.
    pub ranged_mod_range: f32,
    /// The 5-slot `ItemSpell` block, **filtered to entries with `spell_id != 0`**, in wire order —
    /// every "Use:"/"Equip:"/"Chance on hit:" trigger line the tooltip can render.
    pub spells: Vec<ItemSpellEntry>,
    /// The first ON_USE (`SpellTrigger == 0`) spell block — what a right-click/action-bar use
    /// casts, and the key the item's cooldown tracks (the client's own 5-slot scan: spell id > 0,
    /// trigger == 0 — wow-re `wave-cooldown.md` `GetItemCooldown 0x6e2ed0`). `None` for items with
    /// no use effect. A stored view onto [`Self::spells`] (rather than a derived accessor) so
    /// existing cooldown/tooltip consumers reading `.use_spell` are untouched.
    pub use_spell: Option<ItemUseSpell>,
    /// `Bonding` — `ItemBondingType` (0 none … 4 quest-bind); the tooltip's "Binds when picked
    /// up"/"equipped"/"used" line.
    pub bonding: u32,
    /// The item's flavor text (the tooltip's italic line under the stat block); empty = none.
    pub description: String,
    /// `PageText` — a readable item's `PageText.wdb` id (0 = not a book/readable).
    pub page_text: u32,
    /// `LanguageID` — id from `Languages.dbc`; which in-game language a readable's text renders in.
    pub language_id: u32,
    /// `PageMaterial` — id from `PageTextMaterial.dbc`; the book-frame background/texture.
    pub page_material: u32,
    /// `StartQuest` — a quest-starter item's quest id (0 = doesn't start a quest).
    pub start_quest: u32,
    /// `LockID` — id from `Lock.dbc`; nonzero means the item (a chest/junkbox) needs picking/keying
    /// open.
    pub lock_id: u32,
    /// `Material` — id from `Material.dbc`; drives the item's equip/drop/footstep sound set.
    pub material: u32,
    /// `Sheath` — the holster style a drawn weapon of this type renders with (vmangos
    /// `ItemPrototype::Sheath`; the same vocabulary as [`super::update_object::ObjectFields`]'s
    /// virtual-item sheath byte).
    pub sheath: u32,
    /// `RandomProperty` — id from `ItemRandomProperties.dbc`; a "of the Whale"-style suffix roll
    /// (the concrete roll lives on the item *instance*, not the template — this is just which
    /// property table applies).
    pub random_property: u32,
    /// `Block` — a shield's block value (two `u32`s past `Sheath`, after `RandomProperty`).
    pub block: u32,
    /// `ItemSet` — id from `ItemSet.dbc`; 0 = not part of a set.
    pub item_set: u32,
    /// `MaxDurability` — 0 for items without durability (never repairable).
    pub max_durability: u32,
    /// `Area` — id from `AreaTable.dbc`; a zone-bound item's required zone (0 = anywhere).
    pub area: u32,
    /// `Map` — id from `Map.dbc`; a map-bound item's required map (0 = anywhere).
    pub map: u32,
    /// `BagFamily` — a bag's accepted-item-type bitmask (soul bag, herb bag, …; 0 = a normal bag).
    pub bag_family: u32,
}

/// One `Damage` block ([`ItemInfo::damages`] — block 0 is also mirrored into
/// [`ItemInfo::dmg_min`]/[`ItemInfo::dmg_max`]/[`ItemInfo::dmg_type`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ItemDamage {
    pub min: f32,
    pub max: f32,
    /// 0 physical, 1 Holy … 6 Arcane (`Resistances.dbc` id).
    pub school: u32,
}

/// One item-template spell block ([`ItemInfo::spells`]) — the full 6-word wire shape, not just the
/// resolved ON_USE cooldown pair ([`ItemUseSpell`]). `charges`: positive = consumed only while
/// charges last, negative = the item itself is consumed once charges run out (vmangos
/// `ItemPrototype::_ItemSpell::SpellCharges`). The cooldown pair is **server-resolved** the same way
/// as [`ItemUseSpell`]'s (VERIFIED vmangos `ItemHandler.cpp:354-391`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemSpellEntry {
    pub spell_id: u32,
    /// `ItemSpelltriggerType`: 0 ON_USE, 1 ON_EQUIP, 2 CHANCE_ON_HIT.
    pub trigger: u32,
    pub charges: i32,
    /// Use-cooldown ms; negative = the spell's own `RecoveryTime`.
    pub cooldown_ms: i32,
    /// Shared-cooldown category (potions 4, …); the wire's resolved value.
    pub category: u32,
    /// Category cooldown ms; negative = the spell's own `CategoryRecoveryTime`.
    pub category_cooldown_ms: i32,
}

/// The first ON_USE spell block ([`ItemInfo::use_spell`]) — a resolved-cooldown view of whichever
/// [`ItemSpellEntry`] has `trigger == 0`. The cooldown pair is **server-resolved** (VERIFIED vmangos
/// `ItemHandler.cpp:354-380`: the `item_template` override when its value is `>= 0`, else the
/// spell's own `RecoveryTime`/`Category`/`CategoryRecoveryTime`) — but a lone `-1` can still ride
/// next to a set override, so the fields stay signed and a negative means "use the spell's own
/// Spell.dbc value" (the client's `>= 0` pick in `StartCooldown 0x6e2c60`, wow-re
/// `wave-cooldown.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemUseSpell {
    pub spell_id: u32,
    /// Use-cooldown ms; negative = the spell's own `RecoveryTime`.
    pub cooldown_ms: i32,
    /// Shared-cooldown category (potions 4, …); the wire's resolved value.
    pub category: u32,
    /// Category cooldown ms; negative = the spell's own `CategoryRecoveryTime`.
    pub category_cooldown_ms: i32,
}

/// Read `SMSG_ITEM_QUERY_SINGLE_RESPONSE` → `(entry, Some(head))`, or `(entry, None)` on a miss
/// (VERIFIED field order vmangos `HandleItemQuerySingleOpcode`, `ItemHandler.cpp:269-415`).
pub(super) fn read_item_query_response(r: &mut &[u8]) -> io::Result<(u32, Option<ItemInfo>)> {
    let entry = read_u32_le(r)?;
    if entry & 0x8000_0000 != 0 {
        return Ok((entry & 0x7FFF_FFFF, None));
    }
    let class = read_u32_le(r)?;
    let subclass = read_u32_le(r)?;
    let name = read_cstring(r)?;
    for _ in 0..3 {
        let _ = read_cstring(r)?; // name2..name4 — the server sends empties
    }
    let display_info_id = read_u32_le(r)?;
    let quality = read_u32_le(r)?;
    let flags = read_u32_le(r)?;
    let buy_price = read_u32_le(r)?;
    let sell_price = read_u32_le(r)?;
    let inventory_type = read_u32_le(r)?;
    let allowable_class = read_i32_le(r)?;
    let allowable_race = read_i32_le(r)?;
    let item_level = read_u32_le(r)?;
    let required_level = read_u32_le(r)?;
    let required_skill = read_u32_le(r)?;
    let required_skill_rank = read_u32_le(r)?;
    let required_spell = read_u32_le(r)?;
    let required_honor_rank = read_u32_le(r)?;
    let required_city_rank = read_u32_le(r)?;
    let required_rep_faction = read_u32_le(r)?;
    let required_rep_rank = read_u32_le(r)?;
    let max_count = read_u32_le(r)?;
    let stackable = read_u32_le(r)?;
    let container_slots = read_u32_le(r)?;

    // 10x ItemStat { type, value } — kept only where either half is nonzero (an all-zero slot is a
    // genuinely unused one), wire order preserved.
    let mut stats = Vec::new();
    for _ in 0..10 {
        let stat_type = read_u32_le(r)?;
        let stat_value = read_i32_le(r)?;
        if stat_type != 0 || stat_value != 0 {
            stats.push((stat_type, stat_value));
        }
    }

    // 5x Damage { min f32, max f32, type u32 }, wire order. Block 0 is the tooltip's primary
    // damage line — always mirrored into the legacy dmg_min/dmg_max/dmg_type fields for existing
    // consumers, whether or not it clears the `max > 0` filter below (a non-weapon's block 0 is a
    // real 0/0/0, not a missing value).
    let dmg_min = read_f32_le(r)?;
    let dmg_max = read_f32_le(r)?;
    let dmg_type = read_u32_le(r)?;
    let mut damages = Vec::new();
    if dmg_max > 0.0 {
        damages.push(ItemDamage {
            min: dmg_min,
            max: dmg_max,
            school: dmg_type,
        });
    }
    for _ in 0..4 {
        let min = read_f32_le(r)?;
        let max = read_f32_le(r)?;
        let school = read_u32_le(r)?;
        if max > 0.0 {
            damages.push(ItemDamage { min, max, school });
        }
    }

    // Armor is its own field; the remaining 6-wide resistance run (Holy/Fire/Nature/Frost/
    // Shadow/Arcane) lands in `resistances` in wire order.
    let armor = read_u32_le(r)?;
    let holy_res = read_i32_le(r)?;
    let fire_res = read_i32_le(r)?;
    let nature_res = read_i32_le(r)?;
    let frost_res = read_i32_le(r)?;
    let shadow_res = read_i32_le(r)?;
    let arcane_res = read_i32_le(r)?;
    let resistances = [
        holy_res, fire_res, nature_res, frost_res, shadow_res, arcane_res,
    ];

    let delay_ms = read_u32_le(r)?;
    let ammo_type = read_u32_le(r)?;
    let ranged_mod_range = read_f32_le(r)?;

    // 5x Spell block { SpellId, SpellTrigger, SpellCharges, Cooldown, Category, CategoryCooldown }
    // (VERIFIED vmangos `ItemHandler.cpp:354-391`) — the server always writes all six words; a slot
    // with no resolvable spell sends the sentinel 0,0,0,-1,0,-1. Kept in `spells` wherever
    // `spell_id != 0`; the first ON_USE (trigger 0) slot also surfaces as `use_spell` — the
    // client's own 5-slot scan.
    let mut spells = Vec::new();
    let mut use_spell = None;
    for _ in 0..5 {
        let spell_id = read_u32_le(r)?;
        let trigger = read_u32_le(r)?;
        let charges = read_i32_le(r)?;
        let cooldown_ms = read_i32_le(r)?;
        let category = read_u32_le(r)?;
        let category_cooldown_ms = read_i32_le(r)?;
        if spell_id != 0 {
            spells.push(ItemSpellEntry {
                spell_id,
                trigger,
                charges,
                cooldown_ms,
                category,
                category_cooldown_ms,
            });
            if use_spell.is_none() && trigger == 0 {
                use_spell = Some(ItemUseSpell {
                    spell_id,
                    cooldown_ms,
                    category,
                    category_cooldown_ms,
                });
            }
        }
    }

    let bonding = read_u32_le(r)?;
    let description = read_cstring(r)?;
    let page_text = read_u32_le(r)?;
    let language_id = read_u32_le(r)?;
    let page_material = read_u32_le(r)?;
    let start_quest = read_u32_le(r)?;
    let lock_id = read_u32_le(r)?;
    let material = read_u32_le(r)?;
    let sheath = read_u32_le(r)?;
    let random_property = read_u32_le(r)?;
    let block = read_u32_le(r)?;
    let item_set = read_u32_le(r)?;
    let max_durability = read_u32_le(r)?;
    let area = read_u32_le(r)?;
    let map = read_u32_le(r)?;
    let bag_family = read_u32_le(r)?;

    Ok((
        entry,
        Some(ItemInfo {
            class,
            subclass,
            name,
            display_info_id,
            quality,
            flags,
            buy_price,
            sell_price,
            inventory_type,
            allowable_class,
            allowable_race,
            item_level,
            required_level,
            required_skill,
            required_skill_rank,
            required_spell,
            required_honor_rank,
            required_city_rank,
            required_rep_faction,
            required_rep_rank,
            max_count,
            stackable,
            container_slots,
            stats,
            damages,
            dmg_min,
            dmg_max,
            dmg_type,
            armor,
            resistances,
            delay_ms,
            ammo_type,
            ranged_mod_range,
            spells,
            use_spell,
            bonding,
            description,
            page_text,
            language_id,
            page_material,
            start_quest,
            lock_id,
            material,
            sheath,
            random_property,
            block,
            item_set,
            max_durability,
            area,
            map,
            bag_family,
        }),
    ))
}

/// Body of `CMSG_ITEM_QUERY_SINGLE` (vmangos `QueryItem::ReadFromWorldPacket`): the template
/// `entry` + a full 8-byte item guid (0 when asking about a template with no instance in hand) —
/// the exact shape of the creature query.
pub fn item_query(entry: u32, guid: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&entry.to_le_bytes());
    body.extend_from_slice(&guid.to_le_bytes());
    body
}

/// The wire's "the player's own inventory" bag index (`INVENTORY_SLOT_BAG_0`): with it, `slot`
/// addresses the player descriptor's item array directly — equipment 0–18, bag slots 19–22, the
/// backpack 23–38 (VERIFIED vmangos `Player.h` slot enums; the same 23-slot base the descriptor's
/// `PACK_SLOT_1` offset encodes).
pub const BAG_PLAYER_INVENTORY: u8 = 255;
/// The backpack's first player-array slot (`INVENTORY_SLOT_ITEM_START`).
pub const SLOT_PACK_FIRST: u8 = 23;
/// The first equipped-bag player-array slot (`INVENTORY_SLOT_BAG_START`; bags occupy 19–22).
pub const SLOT_BAG_FIRST: u8 = 19;

/// Body of `CMSG_USE_ITEM` (VERIFIED vmangos `UseItem::ReadFromWorldPacket` + opcode 171
/// `Opcodes_1_12_1.h`): `bagIndex` (a bag's player-array slot 19–22, or [`BAG_PLAYER_INVENTORY`]
/// with an absolute `slot`), `slot` (0-based within the bag), `spellSlot` (which of the template's
/// 5 spell effects — 0, the "use" effect, for a plain use), then a self-shaped cast-target block
/// (mask 0 — consumables resolve their own implicit target server-side).
pub fn use_item(bag_index: u8, slot: u8, spell_slot: u8) -> Vec<u8> {
    let mut body = Vec::with_capacity(5);
    body.push(bag_index);
    body.push(slot);
    body.push(spell_slot);
    body.extend_from_slice(&0u16.to_le_bytes());
    body
}

/// Body of `CMSG_AUTOEQUIP_ITEM` (VERIFIED vmangos `AutoEquipItem::ReadFromWorldPacket`,
/// `Server/Packets/Item.cpp:17-21` + `.h:31-39`; opcode 266 `Opcodes_1_12_1.h:269`): source
/// `srcbag`/`srcslot` (both `uint8`), the same bag addressing as [`use_item`]. The real client
/// sends this — not USE_ITEM — when the clicked bag item is *equippable* (the equip-vs-use fork is
/// client-side); the server picks the destination slot itself. Refusals answer
/// `SMSG_INVENTORY_CHANGE_FAILURE`.
pub fn auto_equip_item(bag_index: u8, slot: u8) -> Vec<u8> {
    vec![bag_index, slot]
}

/// Body of `CMSG_AUTOSTORE_BAG_ITEM` (VERIFIED vmangos `AutoStoreBagItem::ReadFromWorldPacket`,
/// `Server/Packets/Item.cpp:23-28` + `.h:41-49`; opcode 267 `Opcodes_1_12_1.h:270`): `srcbag`,
/// `srcslot`, `dstbag` — all `uint8`. "Auto-store this item into that bag, server picks the slot."
/// Builder only tonight (backpack-internal moves take [`swap_inv_item`]); no UI path yet.
pub fn auto_store_bag_item(src_bag: u8, src_slot: u8, dst_bag: u8) -> Vec<u8> {
    vec![src_bag, src_slot, dst_bag]
}

/// Body of `CMSG_SWAP_ITEM` (VERIFIED vmangos `SwapItem::ReadFromWorldPacket`,
/// `Server/Packets/Item.cpp:30-36` + `.h:51-61`; opcode 268 `Opcodes_1_12_1.h:271`): `dstbag`,
/// `dstslot`, `srcbag`, `srcslot` — all `uint8`, **destination FIRST**. The general bag↔bag move
/// (either endpoint an equipped bag). Builder only tonight; the windowed backpack's internal moves
/// go out as [`swap_inv_item`].
pub fn swap_item(dst_bag: u8, dst_slot: u8, src_bag: u8, src_slot: u8) -> Vec<u8> {
    vec![dst_bag, dst_slot, src_bag, src_slot]
}

/// Body of `CMSG_SWAP_INV_ITEM` (VERIFIED vmangos `SwapInvItem::ReadFromWorldPacket`,
/// `Server/Packets/Item.cpp:38-42` + `.h:63-72`; opcode 269 `Opcodes_1_12_1.h:272`): `srcslot`,
/// `dstslot` — two `uint8` player-array slots, both implicitly on the player itself
/// (`INVENTORY_SLOT_BAG_0`). This is the wire for a backpack-internal pick/place/swap: both slots
/// are `INVENTORY_SLOT_ITEM_START`+i (see [`SLOT_PACK_FIRST`]). An empty destination is still a
/// swap on this wire — the server treats it as a move.
pub fn swap_inv_item(src_slot: u8, dst_slot: u8) -> Vec<u8> {
    vec![src_slot, dst_slot]
}

/// Body of `CMSG_SPLIT_ITEM` (VERIFIED vmangos `SplitItem::ReadFromWorldPacket`,
/// `Server/Packets/Item.cpp:44-51` + `.h:74-85`; opcode 270 `Opcodes_1_12_1.h:273`): `srcbag`,
/// `srcslot`, `dstbag`, `dstslot`, `count` — all `uint8`. Builder only: the UI split dialog is out
/// of scope, but the wire is pinned so a later stack-split slice has a byte-exact starting point.
pub fn split_item(src_bag: u8, src_slot: u8, dst_bag: u8, dst_slot: u8, count: u8) -> Vec<u8> {
    vec![src_bag, src_slot, dst_bag, dst_slot, count]
}

/// Body of `CMSG_DESTROYITEM` (VERIFIED vmangos `Packets/Item.cpp:59-68`; opcode 273
/// `Opcodes_1_12_1.h`): `bag`, `slot`, `count` (0 = the whole stack — matches [`split_item`]'s
/// count and the app's `container_destroys` triple), then THREE more `uint8`s the server reads
/// off the wire and discards — the real client sends them, so the body stays 6 bytes rather than
/// a shorter, non-matching one. Decision 0216 §3: the delete-confirm popup's `OnAccept`
/// (`DeleteCursorItem`).
pub fn destroy_item(bag: u8, slot: u8, count: u8) -> Vec<u8> {
    vec![bag, slot, count, 0, 0, 0]
}

/// Body of `CMSG_SET_AMMO` (VERIFIED wow-re `cursor-dragdrop-slots.md`: the client's auto-equip
/// sender `0x5e1480` forks ammo-class → opcode `0x268`, body `{itemEntry}` (a single `u32`); the
/// vmangos handler `HandleSetAmmoOpcode` reads the same lone `uint32` entry). Unlike every other
/// item CMSG this is NOT a `(bag, slot)` address — ammo is loaded by item *entry*, and the stack
/// stays put in the bag (`PLAYER_AMMO_ID` just references it). The server refuses a mismatch
/// (`EQUIP_ERR_ONLY_AMMO_CAN_GO_HERE` &c.) via `SMSG_INVENTORY_CHANGE_FAILURE`. Decision 0526.
pub fn set_ammo(entry: u32) -> Vec<u8> {
    entry.to_le_bytes().to_vec()
}

/// Read `SMSG_INVENTORY_CHANGE_FAILURE` (VERIFIED vmangos `InventoryChangeFailure::AppendBodyTo`):
/// `u8 reason` (`InventoryResult`; 0 = OK, no tail), then — only when failed — a `u32` required
/// level *iff* `reason == 1` (`CANT_EQUIP_LEVEL_I`), the two full item guids, and the bag subslot.
/// Returns `(reason, required_level, item_guid)`.
pub(super) fn read_inventory_change_failure(r: &mut &[u8]) -> io::Result<(u8, Option<u32>, u64)> {
    let reason = read_u8(r)?;
    if reason == 0 {
        return Ok((0, None, 0));
    }
    let required_level = if reason == 1 {
        Some(read_u32_le(r)?)
    } else {
        None
    };
    let item_guid = read_u64_le(r)?;
    let _item2 = read_u64_le(r)?;
    let _bag_subslot = read_u8(r)?;
    Ok((reason, required_level, item_guid))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The `SMSG_ITEM_QUERY_SINGLE_RESPONSE` parse goldens (hit + miss, byte-exact against the
    // vmangos field order) live in `tests/items.rs` — one home, no drifting twins.

    // Byte-exact encode goldens — the item-move CMSG bodies (VERIFIED field order + widths against
    // vmangos `Server/Packets/Item.cpp` `ReadFromWorldPacket`s; every field is a `uint8`).

    #[test]
    fn auto_equip_item_body() {
        // 266: srcbag, srcslot (Item.cpp:17-21).
        assert_eq!(auto_equip_item(255, 30), vec![255, 30]);
    }

    #[test]
    fn auto_store_bag_item_body() {
        // 267: srcbag, srcslot, dstbag (Item.cpp:23-28).
        assert_eq!(auto_store_bag_item(255, 30, 19), vec![255, 30, 19]);
    }

    #[test]
    fn swap_item_body_destination_first() {
        // 268: dstbag, dstslot, srcbag, srcslot — destination pair FIRST (Item.cpp:30-36).
        assert_eq!(swap_item(19, 3, 255, 30), vec![19, 3, 255, 30]);
    }

    #[test]
    fn swap_inv_item_body() {
        // 269: srcslot, dstslot (Item.cpp:38-42). Backpack slot 1↔2 = player-array 23↔24.
        assert_eq!(swap_inv_item(23, 24), vec![23, 24]);
    }

    #[test]
    fn set_ammo_body() {
        // 616 (0x268): a lone little-endian u32 item entry (wow-re cursor-dragdrop-slots.md).
        assert_eq!(set_ammo(0x0001_6b74), vec![0x74, 0x6b, 0x01, 0x00]);
    }

    #[test]
    fn split_item_body() {
        // 270: srcbag, srcslot, dstbag, dstslot, count (Item.cpp:44-51).
        assert_eq!(split_item(255, 23, 255, 24, 5), vec![255, 23, 255, 24, 5]);
    }

    #[test]
    fn destroy_item_body() {
        // 273: bag, slot, count, then three ignored trailing bytes (Item.cpp:59-68).
        assert_eq!(destroy_item(255, 23, 0), vec![255, 23, 0, 0, 0, 0]);
    }

    // SMSG_INVENTORY_CHANGE_FAILURE parse — both branches of the conditional `requiredLevel u32`
    // (VERIFIED vmangos `InventoryChangeFailure::AppendBodyTo`, `Item.cpp:198-209`;
    // EQUIP_ERR_CANT_EQUIP_LEVEL_I = 1, `Objects/ItemDefines.h`).

    #[test]
    fn inventory_failure_ok_reason_is_bare() {
        // reason 0 (EQUIP_ERR_OK) ships no tail.
        let buf = [0u8];
        let mut r = &buf[..];
        assert_eq!(read_inventory_change_failure(&mut r).unwrap(), (0, None, 0));
    }

    #[test]
    fn inventory_failure_level_branch_reads_the_u32() {
        // reason 1 (CANT_EQUIP_LEVEL_I): requiredLevel u32, item1Guid u64, item2Guid u64, bagSlot u8.
        let mut buf = Vec::new();
        buf.push(1u8); // reason
        buf.extend_from_slice(&40u32.to_le_bytes()); // requiredLevel
        buf.extend_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes()); // item1
        buf.extend_from_slice(&0u64.to_le_bytes()); // item2
        buf.push(7); // bagSlot
        let mut r = &buf[..];
        assert_eq!(
            read_inventory_change_failure(&mut r).unwrap(),
            (1, Some(40), 0x1122_3344_5566_7788)
        );
    }

    #[test]
    fn inventory_failure_nonlevel_branch_has_no_u32() {
        // Any failed reason != 1 skips requiredLevel: item1Guid u64, item2Guid u64, bagSlot u8.
        let mut buf = Vec::new();
        buf.push(3u8); // reason (ITEM_DOESNT_GO_TO_SLOT) — no requiredLevel
        buf.extend_from_slice(&0xDEAD_BEEF_0000_0001u64.to_le_bytes()); // item1
        buf.extend_from_slice(&0u64.to_le_bytes()); // item2
        buf.push(0); // bagSlot
        let mut r = &buf[..];
        assert_eq!(
            read_inventory_change_failure(&mut r).unwrap(),
            (3, None, 0xDEAD_BEEF_0000_0001)
        );
    }
}
