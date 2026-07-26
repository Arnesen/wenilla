//! The character-window stats + equipment seam (decision 0208 §3) — the paper doll's data feed.
//!
//! Same engine-free shape as [`super::unit`]: the app pushes a **player combat-stats snapshot**
//! ([`super::UiScript::set_player_combat_stats`]) and an **inventory-slot snapshot**
//! ([`super::UiScript::set_inventory_slots`]) each frame they change, and the stat/slot globals
//! here read that plain data. Every stat binding is **player-token-only, faithfully**: the 1.12
//! fields behind them (`UNIT_FIELD_STAT*`, `POSSTAT`/`NEGSTAT`, the damage/AP block) are
//! PRIVATE/OWNER_ONLY — no other unit ever streams them — so any non-`"player"` token serves the
//! absent shape (zeros / nils), exactly like `UnitXP` (decision 0208 §3).
//!
//! Return shapes are the ref Lua's own inverse math (ref-PaperDollFrame): the descriptor carries
//! the *effective* value plus the split positive/negative buff deltas; `base = effective − pos −
//! neg`. The negative deltas arrive **negative-or-zero** (vmangos `Player.h:1505`
//! `ApplyStatBuffMod` routes a debuff's already-negative amount into `NEGSTAT`;
//! `StatSystem.cpp:335-336` writes the AP mods' negative half the same way), matching the ref's
//! `negBuff < 0` tests.

use mlua::{Lua, Value};

use super::Model;

/// The 1.12 weapon-subclass → `SkillLine.dbc` id table, transcribed from vmangos
/// `ItemPrototype::GetProficiencySkill`'s `item_weapon_skills` (`Objects/Item.cpp:700-707`;
/// subclass ids `Objects/ItemPrototype.h:190-213`, skill ids `SharedDefines.h:951-1038`) — item
/// class 2 (weapon) subclasses `0..=20`. `None` = no proficiency skill backs the subclass (the
/// obsolete/exotic/misc rows) or out of range. The app resolves the paper doll's weapon-skill
/// line ("Both Hands"/"Ranged") through this, falling back to [`SKILL_UNARMED`] with no weapon
/// (vmangos `Player::GetBaseWeaponSkillValue`, `Objects/Player.cpp:20175-20186`).
pub fn weapon_subclass_skill(subclass: u32) -> Option<u32> {
    const TABLE: [u32; 21] = [
        44,  // 0 axe → SKILL_AXES
        172, // 1 two-hand axe → SKILL_2H_AXES
        45,  // 2 bow → SKILL_BOWS
        46,  // 3 gun → SKILL_GUNS
        54,  // 4 mace → SKILL_MACES
        160, // 5 two-hand mace → SKILL_2H_MACES
        229, // 6 polearm → SKILL_POLEARMS
        43,  // 7 sword → SKILL_SWORDS
        55,  // 8 two-hand sword → SKILL_2H_SWORDS
        0,   // 9 obsolete
        136, // 10 staff → SKILL_STAVES
        0,   // 11 exotic
        0,   // 12 exotic2
        162, // 13 fist weapon → SKILL_UNARMED (vmangos's own row — fists ride the unarmed skill)
        0,   // 14 misc
        173, // 15 dagger → SKILL_DAGGERS
        176, // 16 thrown → SKILL_THROWN
        253, // 17 spear → SKILL_ASSASSINATION (vmangos's own row)
        226, // 18 crossbow → SKILL_CROSSBOWS
        228, // 19 wand → SKILL_WANDS
        356, // 20 fishing pole → SKILL_FISHING
    ];
    TABLE.get(subclass as usize).copied().filter(|&s| s != 0)
}

/// `SKILL_UNARMED` (162, vmangos `SharedDefines.h:987`) — the melee weapon-skill line's fallback
/// when no weapon is equipped (`Player::GetBaseWeaponSkillValue`, `Objects/Player.cpp:20184`).
pub const SKILL_UNARMED: u32 = 162;

/// The player's combat-stats snapshot behind the paper doll's stat pane — plain data the app
/// derives from the descriptor accessors ([`UnitStat`]/[`UnitResistance`]/… read it). Arrays are
/// in field order: stats 0..4 = Str/Agi/Stam/Int/Spi (`UNIT_FIELD_STAT0..4`), schools 0..6 with
/// `[0]` = armor/physical. The `*_neg` values are **negative-or-zero** (the stored wire sign; see
/// the module doc).
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerCombatStats {
    /// Effective (post-buff) primary stats (`UNIT_FIELD_STAT0..4`).
    pub stats: [i32; 5],
    /// Positive stat buff deltas (`PLAYER_FIELD_POSSTAT0..4`, ≥ 0).
    pub stat_pos: [i32; 5],
    /// Negative stat buff deltas (`PLAYER_FIELD_NEGSTAT0..4`, ≤ 0).
    pub stat_neg: [i32; 5],
    /// Effective resistances (`UNIT_FIELD_RESISTANCES`, `[0]` = armor).
    pub resistances: [i32; 7],
    /// Positive resistance buffs (`PLAYER_FIELD_RESISTANCEBUFFMODSPOSITIVE`, ≥ 0).
    pub resistance_pos: [i32; 7],
    /// Negative resistance buffs (`PLAYER_FIELD_RESISTANCEBUFFMODSNEGATIVE`, ≤ 0).
    pub resistance_neg: [i32; 7],
    /// Mainhand damage range (`UNIT_FIELD_MINDAMAGE`/`MAXDAMAGE`).
    pub min_damage: f32,
    pub max_damage: f32,
    /// Offhand damage range (`UNIT_FIELD_MINOFFHANDDAMAGE`/`MAXOFFHANDDAMAGE`).
    pub min_offhand_damage: f32,
    pub max_offhand_damage: f32,
    /// Physical damage-done bonuses (`PLAYER_FIELD_MOD_DAMAGE_DONE_POS[0]` / `_NEG[0]`, neg ≤ 0).
    pub physical_bonus_pos: i32,
    pub physical_bonus_neg: i32,
    /// The physical damage-done multiplier (`PLAYER_FIELD_MOD_DAMAGE_DONE_PCT[0]`, a true float —
    /// the app fills `1.0` while the field hasn't streamed; the default here is `1.0` too).
    pub damage_percent: f32,
    /// Attack speeds in ms (`UNIT_FIELD_BASEATTACKTIME[0..2]`).
    pub main_attack_time_ms: u32,
    pub offhand_attack_time_ms: u32,
    /// Whether an offhand **weapon** is equipped — gates `UnitAttackSpeed`'s second return (the
    /// app decides from the inv slots; the offhand fields stream regardless).
    pub has_offhand: bool,
    /// Melee AP + its split mods (`UNIT_FIELD_ATTACK_POWER` / `_MODS`, neg ≤ 0).
    pub attack_power: i32,
    pub attack_power_pos: i32,
    pub attack_power_neg: i32,
    /// Ranged AP + its split mods.
    pub ranged_attack_power: i32,
    pub ranged_attack_power_pos: i32,
    pub ranged_attack_power_neg: i32,
    /// Ranged attack speed in ms (`UNIT_FIELD_RANGEDATTACKTIME`).
    pub ranged_attack_time_ms: u32,
    /// Ranged damage range (`UNIT_FIELD_MINRANGEDDAMAGE`/`MAXRANGEDDAMAGE`).
    pub ranged_min_damage: f32,
    pub ranged_max_damage: f32,
    /// The equipped main-hand weapon's skill line as `(value, temp+perm bonus)` — the app resolves
    /// WHICH skill via [`weapon_subclass_skill`] (unarmed = [`SKILL_UNARMED`]) and reads the pair
    /// from `PLAYER_SKILL_INFO`; `UnitAttackBothHands` serves it verbatim.
    pub main_weapon_skill: (u32, i32),
    /// The ranged weapon's skill pair (`UnitRangedAttack`).
    pub ranged_weapon_skill: (u32, i32),
    /// Whether a wand is equipped (`HasWandEquipped` — the ref swaps the ranged-attack action for
    /// wand Shoot on it).
    pub has_wand: bool,
}

impl Default for PlayerCombatStats {
    /// All-zeros except `damage_percent` = `1.0` — the multiplicative identity, so the absent
    /// shape never feeds the ref's `bonus / percent` math a division by zero.
    fn default() -> Self {
        PlayerCombatStats {
            stats: [0; 5],
            stat_pos: [0; 5],
            stat_neg: [0; 5],
            resistances: [0; 7],
            resistance_pos: [0; 7],
            resistance_neg: [0; 7],
            min_damage: 0.0,
            max_damage: 0.0,
            min_offhand_damage: 0.0,
            max_offhand_damage: 0.0,
            physical_bonus_pos: 0,
            physical_bonus_neg: 0,
            damage_percent: 1.0,
            main_attack_time_ms: 0,
            offhand_attack_time_ms: 0,
            has_offhand: false,
            attack_power: 0,
            attack_power_pos: 0,
            attack_power_neg: 0,
            ranged_attack_power: 0,
            ranged_attack_power_pos: 0,
            ranged_attack_power_neg: 0,
            ranged_attack_time_ms: 0,
            ranged_min_damage: 0.0,
            ranged_max_damage: 0.0,
            main_weapon_skill: (0, 0),
            ranged_weapon_skill: (0, 0),
            has_wand: false,
        }
    }
}

/// One resolved equipment/ammo slot view (the `GetInventoryItem*` family reads it), resolved by
/// the app like [`super::container::ContainerSlot`]: icon from the display catalog, count/quality
/// from the item object + template. Plain data.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InvSlotView {
    /// The item's template entry (`GetInventoryItemID`).
    pub item_id: u32,
    /// The doll twin of [`super::container::ContainerSlot::bar_placeable`] — an equipped item
    /// dragged straight to the bar goes through the same filter.
    pub bar_placeable: bool,
    /// Icon texture path (`Interface\Icons\…`); `None` while the template answer is in flight.
    pub icon: Option<String>,
    /// Stack count (`GetInventoryItemCount` — 1 for equipment, the bag-summed count for ammo).
    pub count: u32,
    /// Item quality 0..6 (`GetInventoryItemQuality`).
    pub quality: i32,
    /// The item's name, when known (tooltip/link consumers).
    pub name: Option<String>,
    /// The instance's live durability `(current, max)` — the equipped-item tooltip's
    /// "Durability X / Y" line (see [`super::container::ContainerSlot::durability`]).
    pub durability: Option<(u32, u32)>,
    /// `ITEM_FIELD_FLAGS` (wire field 21) — the alert/broken laws read two bits (VERIFIED wow-re
    /// inventory-alert-law `0x4c7ee0`): `0x08` wrapped (a gift — never alerts, never broken),
    /// `0x10` force-red (alert status 4 regardless of durability).
    pub flags: u32,
    /// An `|Hitem:…|h[Name]|h` link once the name is known — the doll twin of
    /// `ContainerSlot::link`; carried onto the cursor payload so a world-drop `DELETE_ITEM_CONFIRM`
    /// off an equipped item can report its name (decision 0208 phase 1b).
    pub link: Option<String>,
    /// Whether an outstanding pending op covers `(EQUIPMENT_BAG, this slot)` — the app's
    /// `PendingItemOps` feed (decision 0216 §4/0218 §3), fed by `ui_char.rs::feed_char` the same
    /// way `ContainerSlot::locked` is. `IsInventoryItemLocked` ORs this with the payload-held-here
    /// check.
    pub locked: bool,
    /// The 1-based live-API inventory slot ids this item could be EQUIPPED into (empty = not
    /// equippable; an equipped ring's own two finger slots, say) — decision 0208 phase 1b's "the
    /// fit rule", resolved app-side via `ui_items::find_equip_slot`.
    pub equip_slots: Vec<u8>,
    /// The RESOLVED `ITEM_FIELD_CREATOR` name — the equipped-item tooltip's green
    /// "<Made by %s>" line (see [`super::container::ContainerSlot::creator`], the bag twin).
    pub creator: Option<String>,
}

/// The inventory-slot snapshot: index 0 = ammo, 1..=19 the equipment slots, 20..=23 the four
/// equipped-bag icons (all the client's `GetInventorySlotInfo` slot ids — decision 0216 slice 2's
/// bag bar). `None` = an empty slot.
pub const INVENTORY_SLOT_COUNT: usize = 24;
pub type InventorySlots = [Option<InvSlotView>; INVENTORY_SLOT_COUNT];

/// The client's own `PaperDollItemFrame.dbc` rows (re-verified against the 1.12.1 (build 5875)
/// MPQ this session, byte-exact — 36 records, 3 `u32` fields each: `SlotName` offset,
/// `SlotTexture` offset, `SlotID`): `(slotName, slotId, empty-slot art suffix)` for the 24 rows
/// `GetInventorySlotInfo` actually names — the 20 equipment+ammo rows plus `Bag0Slot`..`Bag3Slot`
/// (ids **20..23**, confirmed this session — every one points at the same
/// `interface\paperdoll\UI-PaperDoll-Slot-Bag.blp`, matching the reference `CharacterBag0Slot`
/// bag-bar buttons' `GetID()`s). The DBC's remaining 12 rows (`"Bag1".."Bag12"`, ids 64..75) are a
/// different, unrelated numbering — not `GetInventorySlotInfo` names, out of scope here — left
/// untranscribed. The oddballs among the 24: `BackSlot` shows the **Chest** art and `AmmoSlot` the
/// **Ranged** art (both confirmed by their `SlotTexture` offset pointing at that other row's
/// string, not a fresh one).
const SLOT_INFO: [(&str, i64, &str); 24] = [
    ("AmmoSlot", 0, "Ranged"),
    ("HeadSlot", 1, "Head"),
    ("NeckSlot", 2, "Neck"),
    ("ShoulderSlot", 3, "Shoulder"),
    ("ShirtSlot", 4, "Shirt"),
    ("ChestSlot", 5, "Chest"),
    ("WaistSlot", 6, "Waist"),
    ("LegsSlot", 7, "Legs"),
    ("FeetSlot", 8, "Feet"),
    ("WristSlot", 9, "Wrists"),
    ("HandsSlot", 10, "Hands"),
    ("Finger0Slot", 11, "Finger"),
    ("Finger1Slot", 12, "Finger"),
    ("Trinket0Slot", 13, "Trinket"),
    ("Trinket1Slot", 14, "Trinket"),
    ("BackSlot", 15, "Chest"),
    ("MainHandSlot", 16, "MainHand"),
    ("SecondaryHandSlot", 17, "SecondaryHand"),
    ("RangedSlot", 18, "Ranged"),
    ("TabardSlot", 19, "Tabard"),
    ("Bag0Slot", 20, "Bag"),
    ("Bag1Slot", 21, "Bag"),
    ("Bag2Slot", 22, "Bag"),
    ("Bag3Slot", 23, "Bag"),
];

/// The durability-alert regions in the client's own slot table (`0x806eb8`, VERIFIED wow-re
/// inventory-alert-law — 12 entries): alert index 1..=11 → the live-id equipment slot (Head,
/// Shoulders, Chest, Waist, Legs, Feet, Wrists, Hands, Weapon, Shield, Ranged — the ref
/// FrameXML's `INVENTORY_ALERT_STATUS_SLOTS` order), index 12 → the client's low-ammo region
/// (slot -1 in its table; FrameXML never reads it, the binding answers it). Our live-id 0 IS
/// the ammo view, so the 12th entry maps there.
const ALERT_SLOTS: [usize; 12] = [1, 3, 5, 6, 7, 8, 9, 10, 16, 17, 18, 0];

/// The broken classification the doll tint (`GetInventoryItemBroken`) and alert status 4 share
/// (the recompute `0x4c7ee0`, byte-verified): a wrapped item (`flags & 0x08`) is never broken;
/// `flags & 0x10` forces broken regardless of durability; else broken ⇔ tracks durability
/// (max > 0) at 0.
fn slot_is_broken(v: &InvSlotView) -> bool {
    if v.item_id == 0 || v.flags & 0x08 != 0 {
        return false;
    }
    v.flags & 0x10 != 0 || matches!(v.durability, Some((0, max)) if max > 0)
}

/// One region's `GetInventoryAlertStatus` value — the recompute `0x4c7ee0`'s per-item
/// classification (VERIFIED wow-re inventory-alert-law, §5-cross-checked; the enum's own name
/// is `INV_ALERT_STATUS`): `4` (red) = broken per [`slot_is_broken`]; `3` (yellow) = tracks
/// durability and **`1..=5` points left — an ABSOLUTE count, no percentage, maxDurability is
/// only the tracks-durability gate** (`cmp [D+0xa0],5`); `0` otherwise. Statuses 1/2 are the
/// temp-weapon-enchant alerts (present/expiring-in-30s) — no enchant feed yet, and the 1.12
/// FrameXML disables their colors anyway; a named deferral.
fn alert_status(slot: &Option<InvSlotView>) -> u8 {
    let Some(v) = slot.as_ref().filter(|v| v.item_id != 0) else {
        return 0;
    };
    if v.flags & 0x10 != 0 {
        return 4;
    }
    if v.flags & 0x08 != 0 {
        return 0;
    }
    match v.durability {
        Some((0, max)) if max > 0 => 4,
        Some((1..=5, max)) if max > 0 => 3,
        _ => 0,
    }
}

/// The 12th region's low-ammo status (`0x4c7ee0`'s slot −1 arm): an equipped ammo item whose
/// carried count is `<= 20` reads `3`; no ammo (or plenty) reads `0`. The ammo view's count is
/// already the bag-summed carried total.
fn ammo_alert_status(slot: &Option<InvSlotView>) -> u8 {
    match slot.as_ref().filter(|v| v.item_id != 0) {
        Some(v) if v.count <= 20 => 3,
        _ => 0,
    }
}

impl super::UiScript {
    /// Push (or clear, with `None`) the player's combat-stats snapshot — the app calls this each
    /// frame any of the backing descriptor fields changed (decision 0208 §3).
    pub fn set_player_combat_stats(&mut self, stats: Option<PlayerCombatStats>) {
        self.model_mut().player_combat_stats = stats;
    }

    /// Push the equipment/ammo slot snapshot (index 0 = ammo, 1..=19 equipment). Recomputes the
    /// 11 durability-alert region statuses off the live pairs; a change fires
    /// `UPDATE_INVENTORY_ALERTS` — the DurabilityFrame's own repaint signal (the real engine
    /// computes these statuses itself and fires the same event).
    pub fn set_inventory_slots(&mut self, slots: InventorySlots) {
        {
            let mut model = self.model_mut();
            let alerts: [u8; 12] = std::array::from_fn(|i| {
                if i == 11 {
                    ammo_alert_status(&slots[ALERT_SLOTS[i]])
                } else {
                    alert_status(&slots[ALERT_SLOTS[i]])
                }
            });
            model.inventory_slots = slots;
            model.inventory_alerts = alerts;
        }
        // Unconditionally, the client's own shape (VERIFIED `0x4c7ee0`: the event fires at the
        // tail of EVERY recompute, never diffed against prior state) — SetAlerts re-derives the
        // whole frame from the statuses each time, so glyph choices (the off-hand
        // shield/off-weapon swap) can never go stale. The app pushes only on real slot-view
        // change, so this stays quiet between changes.
        self.fire_event("UPDATE_INVENTORY_ALERTS", vec![]);
    }

    /// Drain the inventory-slot ids `UseInventoryItem` queued (decision 0208 phase 1b) — the app
    /// resolves each to the equipped item's guid and sends `CMSG_USE_ITEM` (bag 255 + the
    /// 0-based wire slot).
    pub fn take_inventory_uses(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.model_mut().inventory_uses)
    }

    /// The paper-doll model pane's yaw in radians (`BenillaPaperDollModel_SetFacing` wrote it) —
    /// a persistent value the app samples each frame to pose the booth's body bake (decision 0208
    /// §5), not a drain. `0.0` until Lua sets it (the ref's own default, 0.61, is authored in the
    /// window's OnLoad).
    pub fn paperdoll_yaw(&self) -> f32 {
        self.model_ref().paperdoll_yaw
    }
}

/// Read the player combat-stats snapshot under a short model borrow — but only for the
/// `"player"` token; any other token (and a not-yet-pushed snapshot) reads the default (the
/// absent shape: zeros, `damage_percent` 1.0). The faithful player-only gate (module doc).
fn with_player_stats<T>(
    lua: &Lua,
    token: &Option<String>,
    f: impl FnOnce(&PlayerCombatStats) -> T,
) -> T {
    let model = lua.app_data_ref::<Model>().expect("model app_data");
    let absent = PlayerCombatStats::default();
    let stats = match token.as_deref() {
        Some("player") => model.player_combat_stats.as_ref().unwrap_or(&absent),
        _ => &absent,
    };
    f(stats)
}

/// Read inventory slot `slot` under a short borrow, cloned out so the caller holds no borrow.
/// `None` = empty / out of range / a token with no equipment source.
///
/// **Unit-keyed, and that is the reference's own shape** (decision 0631): the engine answers
/// `GetInventoryItemTexture(unit, slot)` for any unit whose item data it holds, which is why the
/// inspect paper doll reuses the very same bindings rather than a parallel `GetInspectItem*`
/// family. Two sources, by token:
///
/// - `"player"` → the self feed ([`Model::inventory_slots`]), resolved from our PRIVATE
///   `PLAYER_FIELD_INV_SLOT_*` guids → item objects → templates. Carries counts, durability,
///   locks — everything an owned item object knows.
/// - the **inspected** token → [`Model::inspect`], resolved from the target's PUBLIC
///   `PLAYER_VISIBLE_ITEM_*` entries. No item objects exist for a foreign player (their inventory
///   guids are server-private), so those views carry entry/icon/name/quality and nothing else —
///   which is exactly why the reference's inspect window shows no stack counts or durability.
fn player_inv_slot(lua: &Lua, token: &Option<String>, slot: i64) -> Option<InvSlotView> {
    let token = token.as_deref()?;
    let idx = usize::try_from(slot).ok()?;
    let model = lua.app_data_ref::<Model>().expect("model app_data");
    model.inv_slot(token, idx).cloned()
}

impl Model {
    /// The equipment slot `token` exposes at live-API id `slot`, or `None`. The one place the
    /// two-source routing above is decided — shared by the `GetInventoryItem*` getters and
    /// `GameTooltip:SetInventoryItem`, so a tooltip can never disagree with the icon under it.
    pub(super) fn inv_slot(&self, token: &str, slot: usize) -> Option<&InvSlotView> {
        let slots = if token.eq_ignore_ascii_case("player") {
            &self.inventory_slots
        } else {
            &self.inspect.as_ref().filter(|v| v.unit == token)?.slots
        };
        slots.get(slot)?.as_ref()
    }
}

/// Register the paper-doll stat/slot globals (decision 0208 §3).
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // UnitStat("player", i /*1..=5*/) → (stat, effectiveStat, posBuff, negBuff): effective is the
    // UNIT_FIELD_STAT value, pos/neg the POSSTAT/NEGSTAT deltas (neg ≤ 0), and stat (base) their
    // inverse — the ref computes base = effective − pos − neg and branches on negBuff < 0.
    // Out-of-range i (the ref never passes one) serves the absent zeros.
    g.set(
        "UnitStat",
        lua.create_function(|lua, (token, i): (Option<String>, i64)| {
            Ok(with_player_stats(lua, &token, |s| {
                let Some(idx) = i.checked_sub(1).and_then(|v| usize::try_from(v).ok()) else {
                    return (0, 0, 0, 0);
                };
                if idx >= 5 {
                    return (0, 0, 0, 0);
                }
                let (eff, pos, neg) = (s.stats[idx], s.stat_pos[idx], s.stat_neg[idx]);
                (
                    i64::from(eff - pos - neg),
                    i64::from(eff),
                    i64::from(pos),
                    i64::from(neg),
                )
            }))
        })?,
    )?;

    // UnitResistance("player", school /*0..=6*/) → (base, resistance, positive, negative) — the
    // same inverse math per school ([0] = armor).
    g.set(
        "UnitResistance",
        lua.create_function(|lua, (token, school): (Option<String>, i64)| {
            Ok(with_player_stats(lua, &token, |s| {
                let Some(idx) = usize::try_from(school).ok().filter(|&v| v < 7) else {
                    return (0, 0, 0, 0);
                };
                let (eff, pos, neg) = (
                    s.resistances[idx],
                    s.resistance_pos[idx],
                    s.resistance_neg[idx],
                );
                (
                    i64::from(eff - pos - neg),
                    i64::from(eff),
                    i64::from(pos),
                    i64::from(neg),
                )
            }))
        })?,
    )?;

    // UnitArmor("player") → (base, effectiveArmor, armor, posBuff, negBuff) — school 0 of the
    // resistance block; the ref reads effectiveArmor and armor equivalently (both the total).
    g.set(
        "UnitArmor",
        lua.create_function(|lua, token: Option<String>| {
            Ok(with_player_stats(lua, &token, |s| {
                let (eff, pos, neg) = (s.resistances[0], s.resistance_pos[0], s.resistance_neg[0]);
                (
                    i64::from(eff - pos - neg),
                    i64::from(eff),
                    i64::from(eff),
                    i64::from(pos),
                    i64::from(neg),
                )
            }))
        })?,
    )?;

    // UnitDamage("player") → (minDamage, maxDamage, minOffHandDamage, maxOffHandDamage,
    // physicalBonusPos, physicalBonusNeg, percent) — the damage fields verbatim plus the
    // school-0 MOD_DAMAGE_DONE decomposition (percent 1.0 when absent; decision 0208 "to
    // confirm" flags the buffed decomposition as inferred).
    g.set(
        "UnitDamage",
        lua.create_function(|lua, token: Option<String>| {
            Ok(with_player_stats(lua, &token, |s| {
                (
                    f64::from(s.min_damage),
                    f64::from(s.max_damage),
                    f64::from(s.min_offhand_damage),
                    f64::from(s.max_offhand_damage),
                    i64::from(s.physical_bonus_pos),
                    i64::from(s.physical_bonus_neg),
                    f64::from(s.damage_percent),
                )
            }))
        })?,
    )?;

    // UnitAttackSpeed("player") → (mainSpeed, offhandSpeed|nil) in seconds (BASEATTACKTIME /
    // 1000). Offhand is nil unless an offhand weapon is equipped — the snapshot's has_offhand,
    // the app's call (the field itself streams regardless).
    g.set(
        "UnitAttackSpeed",
        lua.create_function(|lua, token: Option<String>| {
            Ok(with_player_stats(lua, &token, |s| {
                (
                    f64::from(s.main_attack_time_ms) / 1000.0,
                    s.has_offhand
                        .then_some(f64::from(s.offhand_attack_time_ms) / 1000.0),
                )
            }))
        })?,
    )?;

    // UnitAttackPower("player") → (base, posBuff, negBuff): base = UNIT_FIELD_ATTACK_POWER, the
    // buffs the MODS field's split signed halves (neg ≤ 0 — StatSystem.cpp:335-336).
    g.set(
        "UnitAttackPower",
        lua.create_function(|lua, token: Option<String>| {
            Ok(with_player_stats(lua, &token, |s| {
                (
                    i64::from(s.attack_power),
                    i64::from(s.attack_power_pos),
                    i64::from(s.attack_power_neg),
                )
            }))
        })?,
    )?;

    // UnitRangedAttackPower("player") → (base, posBuff, negBuff) — the ranged twin.
    g.set(
        "UnitRangedAttackPower",
        lua.create_function(|lua, token: Option<String>| {
            Ok(with_player_stats(lua, &token, |s| {
                (
                    i64::from(s.ranged_attack_power),
                    i64::from(s.ranged_attack_power_pos),
                    i64::from(s.ranged_attack_power_neg),
                )
            }))
        })?,
    )?;

    // UnitAttackBothHands("player") → (base, modifier) — the main-hand weapon-skill line: skill
    // value + its temp+perm bonus, resolved app-side via the weapon_subclass_skill table (unarmed
    // = SKILL_UNARMED when nothing's equipped).
    g.set(
        "UnitAttackBothHands",
        lua.create_function(|lua, token: Option<String>| {
            Ok(with_player_stats(lua, &token, |s| {
                (
                    i64::from(s.main_weapon_skill.0),
                    i64::from(s.main_weapon_skill.1),
                )
            }))
        })?,
    )?;

    // UnitRangedAttack("player") → (base, modifier) — the ranged weapon's skill pair.
    g.set(
        "UnitRangedAttack",
        lua.create_function(|lua, token: Option<String>| {
            Ok(with_player_stats(lua, &token, |s| {
                (
                    i64::from(s.ranged_weapon_skill.0),
                    i64::from(s.ranged_weapon_skill.1),
                )
            }))
        })?,
    )?;

    // UnitRangedDamage("player") → (speed, minDamage, maxDamage, physicalBonusPos,
    // physicalBonusNeg, percent) — RANGEDATTACKTIME/1000 + the ranged damage range + the same
    // school-0 mods as UnitDamage.
    g.set(
        "UnitRangedDamage",
        lua.create_function(|lua, token: Option<String>| {
            Ok(with_player_stats(lua, &token, |s| {
                (
                    f64::from(s.ranged_attack_time_ms) / 1000.0,
                    f64::from(s.ranged_min_damage),
                    f64::from(s.ranged_max_damage),
                    i64::from(s.physical_bonus_pos),
                    i64::from(s.physical_bonus_neg),
                    f64::from(s.damage_percent),
                )
            }))
        })?,
    )?;

    // HasWandEquipped() → boolean (the ref's ranged block swaps "Shoot" in on it). No unit arg —
    // the live global is player-implicit.
    g.set(
        "HasWandEquipped",
        lua.create_function(|lua, ()| {
            let model = lua.app_data_ref::<Model>().expect("model app_data");
            Ok(model
                .player_combat_stats
                .as_ref()
                .is_some_and(|s| s.has_wand))
        })?,
    )?;

    // GetInventoryItemID("player", slot) → itemId | nil (slot per GetInventorySlotInfo: 0 ammo,
    // 1..=19 equipment). Player-token-only like the stat block — the INV_SLOT guids are PRIVATE.
    g.set(
        "GetInventoryItemID",
        lua.create_function(|lua, (token, slot): (Option<String>, i64)| {
            match player_inv_slot(lua, &token, slot) {
                Some(v) if v.item_id != 0 => Ok(Value::Integer(i64::from(v.item_id))),
                _ => Ok(Value::Nil),
            }
        })?,
    )?;

    // GetInventoryItemTexture("player", slot) → icon path | nil (nil while the template answer is
    // in flight — the button shows the empty-slot art until the push lands).
    g.set(
        "GetInventoryItemTexture",
        lua.create_function(|lua, (token, slot): (Option<String>, i64)| {
            match player_inv_slot(lua, &token, slot).and_then(|v| v.icon) {
                Some(icon) => Ok(Value::String(lua.create_string(&icon)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;

    // GetInventoryItemCount("player", slot) → the stack count; **0 for an empty slot** (the
    // wiki's documented shape — warcraft.wiki.gg: "1 for most items", 0 when nothing's there;
    // the ammo slot carries the bag-summed count the app resolves).
    g.set(
        "GetInventoryItemCount",
        lua.create_function(|lua, (token, slot): (Option<String>, i64)| {
            Ok(player_inv_slot(lua, &token, slot).map_or(0i64, |v| i64::from(v.count)))
        })?,
    )?;

    // GetInventoryItemQuality("player", slot) → 0..6 | nil (nil for an empty slot — the wiki
    // shape; the ref keys quality borders on it).
    g.set(
        "GetInventoryItemQuality",
        lua.create_function(|lua, (token, slot): (Option<String>, i64)| {
            match player_inv_slot(lua, &token, slot) {
                Some(v) if v.item_id != 0 => Ok(Value::Integer(i64::from(v.quality))),
                _ => Ok(Value::Nil),
            }
        })?,
    )?;

    // GetInventoryItemBroken("player", slot) → 1 | nil — the shared broken classification
    // ([`slot_is_broken`]: durability 0 with a max, or the force-red flag bit; wrapped never).
    // The ref's PaperDollItemSlotButton_Update keys the red slot tint on it
    // (PaperDollFrame.lua:670-676).
    g.set(
        "GetInventoryItemBroken",
        lua.create_function(|lua, (token, slot): (Option<String>, i64)| {
            match player_inv_slot(lua, &token, slot) {
                Some(v) if slot_is_broken(&v) => Ok(Value::Integer(1)),
                _ => Ok(Value::Nil),
            }
        })?,
    )?;

    // GetInventoryAlertStatus(index) → the region's alert status (`INV_ALERT_STATUS`: 0 none,
    // 3 damaged/low-ammo, 4 broken; 1/2 = temp-enchant alerts, unfed) — DurabilityFrame's
    // armor-guy read. Valid 1..=12 (the client's own 12-entry table; index 12 = low ammo, which
    // the 1.12 FrameXML never reads); out-of-range reads 0.
    g.set(
        "GetInventoryAlertStatus",
        lua.create_function(|lua, index: usize| {
            let model = lua.app_data_mut::<Model>().expect("model app_data");
            Ok(if (1..=12).contains(&index) {
                i64::from(model.inventory_alerts[index - 1])
            } else {
                0
            })
        })?,
    )?;

    // OffhandHasWeapon() → 1 | nil — whether the off-hand slot holds a WEAPON (item class 2)
    // rather than a shield/held-in-off-hand; DurabilityFrame.lua swaps the shield glyph for the
    // off-weapon glyph on it. Read off the equipped slot view + the template store.
    g.set(
        "OffhandHasWeapon",
        lua.create_function(|lua, (): ()| {
            let model = lua.app_data_mut::<Model>().expect("model app_data");
            let is_weapon = model.inventory_slots[17]
                .as_ref()
                .filter(|v| v.item_id != 0)
                .and_then(|v| model.item_templates.get(&v.item_id))
                .is_some_and(|t| t.class == 2);
            Ok(is_weapon.then_some(1i64))
        })?,
    )?;

    // GetInventorySlotInfo(slotName) → (slotId, textureName, checkRelic) — the client's own
    // PaperDollItemFrame.dbc rows (SLOT_INFO above). checkRelic is always false in vanilla
    // (UnitHasRelicSlot is a later-era concept; the 0208 deferral). An unknown name is a Lua
    // error, the client's own behavior.
    g.set(
        "GetInventorySlotInfo",
        lua.create_function(|lua, name: String| {
            let Some((_, id, art)) = SLOT_INFO.iter().find(|(n, _, _)| *n == name) else {
                return Err(mlua::Error::runtime(format!(
                    "GetInventorySlotInfo: unknown slot name '{name}'"
                )));
            };
            Ok((
                *id,
                Value::String(
                    lua.create_string(format!("Interface\\Paperdoll\\UI-PaperDoll-Slot-{art}"))?,
                ),
                false,
            ))
        })?,
    )?;

    // BenillaPaperDollModel_SetFacing(radians) — the model pane's rotate buttons write the bake
    // yaw here (persistent; the app samples UiScript::paperdoll_yaw each frame — decision 0208
    // §5's rotate-adjusts-the-bake). Benilla-named: the real client's Model:SetFacing is a widget
    // method on a live 3D pane; our doctrine-consistent still carries one scalar.
    g.set(
        "BenillaPaperDollModel_SetFacing",
        lua.create_function(|lua, radians: f32| {
            lua.app_data_mut::<Model>()
                .expect("model app_data")
                .paperdoll_yaw = radians;
            Ok(())
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{weapon_subclass_skill, SKILL_UNARMED};
    use crate::script::{InvSlotView, PlayerCombatStats, UiScript};

    /// A filled snapshot exercising every field the bindings read.
    fn stats() -> PlayerCombatStats {
        PlayerCombatStats {
            stats: [25, 20, 22, 10, 11],
            stat_pos: [4, 0, 0, 0, 0],
            stat_neg: [0, -2, 0, 0, 0],
            resistances: [150, 0, 20, 0, 0, 0, -5],
            resistance_pos: [30, 0, 25, 0, 0, 0, 0],
            resistance_neg: [-10, 0, -5, 0, 0, 0, -5],
            min_damage: 12.5,
            max_damage: 19.5,
            min_offhand_damage: 5.0,
            max_offhand_damage: 9.0,
            physical_bonus_pos: 25,
            physical_bonus_neg: -3,
            damage_percent: 1.1,
            main_attack_time_ms: 2900,
            offhand_attack_time_ms: 1500,
            has_offhand: false,
            attack_power: 78,
            attack_power_pos: 30,
            attack_power_neg: -10,
            ranged_attack_power: 52,
            ranged_attack_power_pos: 0,
            ranged_attack_power_neg: -3,
            ranged_attack_time_ms: 2800,
            ranged_min_damage: 31.0,
            ranged_max_damage: 47.0,
            main_weapon_skill: (25, 2),
            ranged_weapon_skill: (18, 0),
            has_wand: false,
        }
    }

    #[test]
    fn weapon_skill_table_matches_vmangos_item_cpp() {
        // The populated rows (Item.cpp:700-707).
        assert_eq!(weapon_subclass_skill(0), Some(44)); // axe
        assert_eq!(weapon_subclass_skill(1), Some(172)); // 2h axe
        assert_eq!(weapon_subclass_skill(2), Some(45)); // bow
        assert_eq!(weapon_subclass_skill(3), Some(46)); // gun
        assert_eq!(weapon_subclass_skill(4), Some(54)); // mace
        assert_eq!(weapon_subclass_skill(5), Some(160)); // 2h mace
        assert_eq!(weapon_subclass_skill(6), Some(229)); // polearm
        assert_eq!(weapon_subclass_skill(7), Some(43)); // sword
        assert_eq!(weapon_subclass_skill(8), Some(55)); // 2h sword
        assert_eq!(weapon_subclass_skill(10), Some(136)); // staff
        assert_eq!(weapon_subclass_skill(13), Some(SKILL_UNARMED)); // fist
        assert_eq!(weapon_subclass_skill(15), Some(173)); // dagger
        assert_eq!(weapon_subclass_skill(16), Some(176)); // thrown
        assert_eq!(weapon_subclass_skill(17), Some(253)); // spear
        assert_eq!(weapon_subclass_skill(18), Some(226)); // crossbow
        assert_eq!(weapon_subclass_skill(19), Some(228)); // wand
        assert_eq!(weapon_subclass_skill(20), Some(356)); // fishing pole
                                                          // The skill-less rows + out of range.
        for sub in [9u32, 11, 12, 14, 21, 100] {
            assert_eq!(weapon_subclass_skill(sub), None, "subclass {sub}");
        }
    }

    #[test]
    fn unit_stat_serves_effective_and_the_derived_base() {
        let mut s = UiScript::new().unwrap();
        s.set_player_combat_stats(Some(stats()));
        // Str: effective 25, pos 4, neg 0 → base 21.
        assert_eq!(
            s.eval::<(i64, i64, i64, i64)>(r#"return UnitStat("player", 1)"#)
                .unwrap(),
            (21, 25, 4, 0)
        );
        // Agi: effective 20, pos 0, neg −2 → base 22 (the ref tests negBuff < 0).
        assert_eq!(
            s.eval::<(i64, i64, i64, i64)>(r#"return UnitStat("player", 2)"#)
                .unwrap(),
            (22, 20, 0, -2)
        );
        // A non-player token serves the absent zeros (no other unit streams these fields).
        assert_eq!(
            s.eval::<(i64, i64, i64, i64)>(r#"return UnitStat("target", 1)"#)
                .unwrap(),
            (0, 0, 0, 0)
        );
        // Out-of-range index: zeros, no error.
        assert_eq!(
            s.eval::<(i64, i64, i64, i64)>(r#"return UnitStat("player", 6)"#)
                .unwrap(),
            (0, 0, 0, 0)
        );
        assert_eq!(
            s.eval::<(i64, i64, i64, i64)>(r#"return UnitStat("player", 0)"#)
                .unwrap(),
            (0, 0, 0, 0)
        );
    }

    #[test]
    fn unit_resistance_and_armor_decompose_school_zero() {
        let mut s = UiScript::new().unwrap();
        s.set_player_combat_stats(Some(stats()));
        // Armor (school 0): 150 total, +30/−10 → base 130.
        assert_eq!(
            s.eval::<(i64, i64, i64, i64)>(r#"return UnitResistance("player", 0)"#)
                .unwrap(),
            (130, 150, 30, -10)
        );
        // Fire (school 2): 20 total, +25/−5 → base 0.
        assert_eq!(
            s.eval::<(i64, i64, i64, i64)>(r#"return UnitResistance("player", 2)"#)
                .unwrap(),
            (0, 20, 25, -5)
        );
        // A cursed school can read negative (arcane: −5 total, all from the debuff).
        assert_eq!(
            s.eval::<(i64, i64, i64, i64)>(r#"return UnitResistance("player", 6)"#)
                .unwrap(),
            (0, -5, 0, -5)
        );
        // UnitArmor is school 0 with the five-return shape (effectiveArmor = armor = the total).
        assert_eq!(
            s.eval::<(i64, i64, i64, i64, i64)>(r#"return UnitArmor("player")"#)
                .unwrap(),
            (130, 150, 150, 30, -10)
        );
        // Out of range / non-player: zeros.
        assert_eq!(
            s.eval::<(i64, i64, i64, i64)>(r#"return UnitResistance("player", 7)"#)
                .unwrap(),
            (0, 0, 0, 0)
        );
        assert_eq!(
            s.eval::<(i64, i64, i64, i64, i64)>(r#"return UnitArmor("target")"#)
                .unwrap(),
            (0, 0, 0, 0, 0)
        );
    }

    #[test]
    fn unit_damage_and_attack_speed_read_the_snapshot() {
        let mut s = UiScript::new().unwrap();
        s.set_player_combat_stats(Some(stats()));
        assert_eq!(
            s.eval::<(f64, f64, f64, f64, i64, i64, f64)>(r#"return UnitDamage("player")"#)
                .unwrap(),
            (12.5, 19.5, 5.0, 9.0, 25, -3, f64::from(1.1f32))
        );
        // No offhand equipped → second return nil.
        assert!(s
            .eval::<bool>(r#"local m, o = UnitAttackSpeed("player") return m == 2.9 and o == nil"#)
            .unwrap());
        // With an offhand: both speeds, in seconds.
        s.set_player_combat_stats(Some(PlayerCombatStats {
            has_offhand: true,
            ..stats()
        }));
        assert_eq!(
            s.eval::<(f64, f64)>(r#"return UnitAttackSpeed("player")"#)
                .unwrap(),
            (2.9, 1.5)
        );
        // Absent snapshot / non-player token: zeros, percent 1.0 (the div-safe identity).
        assert_eq!(
            s.eval::<(f64, f64, f64, f64, i64, i64, f64)>(r#"return UnitDamage("target")"#)
                .unwrap(),
            (0.0, 0.0, 0.0, 0.0, 0, 0, 1.0)
        );
        let fresh = UiScript::new().unwrap();
        assert_eq!(
            fresh
                .eval::<(f64, f64, f64, f64, i64, i64, f64)>(r#"return UnitDamage("player")"#)
                .unwrap(),
            (0.0, 0.0, 0.0, 0.0, 0, 0, 1.0)
        );
    }

    #[test]
    fn attack_power_and_weapon_skill_bindings_serve_the_pairs() {
        let mut s = UiScript::new().unwrap();
        s.set_player_combat_stats(Some(stats()));
        assert_eq!(
            s.eval::<(i64, i64, i64)>(r#"return UnitAttackPower("player")"#)
                .unwrap(),
            (78, 30, -10)
        );
        assert_eq!(
            s.eval::<(i64, i64, i64)>(r#"return UnitRangedAttackPower("player")"#)
                .unwrap(),
            (52, 0, -3)
        );
        assert_eq!(
            s.eval::<(i64, i64)>(r#"return UnitAttackBothHands("player")"#)
                .unwrap(),
            (25, 2)
        );
        assert_eq!(
            s.eval::<(i64, i64)>(r#"return UnitRangedAttack("player")"#)
                .unwrap(),
            (18, 0)
        );
        assert_eq!(
            s.eval::<(f64, f64, f64, i64, i64, f64)>(r#"return UnitRangedDamage("player")"#)
                .unwrap(),
            (2.8, 31.0, 47.0, 25, -3, f64::from(1.1f32))
        );
        // Non-player: the absent zeros throughout.
        assert_eq!(
            s.eval::<(i64, i64, i64)>(r#"return UnitAttackPower("target")"#)
                .unwrap(),
            (0, 0, 0)
        );
        assert_eq!(
            s.eval::<(i64, i64)>(r#"return UnitAttackBothHands("target")"#)
                .unwrap(),
            (0, 0)
        );
    }

    #[test]
    fn has_wand_equipped_reads_the_flag() {
        let mut s = UiScript::new().unwrap();
        assert!(!s.eval::<bool>("return HasWandEquipped()").unwrap());
        s.set_player_combat_stats(Some(PlayerCombatStats {
            has_wand: true,
            ..stats()
        }));
        assert!(s.eval::<bool>("return HasWandEquipped()").unwrap());
    }

    #[test]
    fn inventory_item_bindings_serve_occupied_empty_and_absent_shapes() {
        let mut s = UiScript::new().unwrap();
        let mut slots: crate::script::InventorySlots = Default::default();
        // Head (slot 1) occupied; ammo (slot 0) with a bag-summed count; the rest empty.
        slots[1] = Some(InvSlotView {
            item_id: 2263,
            icon: Some("Interface\\Icons\\INV_Misc_Bandana_01".into()),
            count: 1,
            quality: 2,
            name: Some("Brawler's Harness".into()),
            ..Default::default()
        });
        slots[0] = Some(InvSlotView {
            item_id: 2512,
            icon: Some("Interface\\Icons\\INV_Ammo_Arrow_01".into()),
            count: 200,
            quality: 1,
            name: Some("Rough Arrow".into()),
            ..Default::default()
        });
        s.set_inventory_slots(slots);

        assert_eq!(
            s.eval::<i64>(r#"return GetInventoryItemID("player", 1)"#)
                .unwrap(),
            2263
        );
        assert_eq!(
            s.eval::<String>(r#"return GetInventoryItemTexture("player", 1)"#)
                .unwrap(),
            "Interface\\Icons\\INV_Misc_Bandana_01"
        );
        assert_eq!(
            s.eval::<i64>(r#"return GetInventoryItemCount("player", 1)"#)
                .unwrap(),
            1
        );
        assert_eq!(
            s.eval::<i64>(r#"return GetInventoryItemQuality("player", 1)"#)
                .unwrap(),
            2
        );
        // The ammo slot (0) reads through the same family.
        assert_eq!(
            s.eval::<i64>(r#"return GetInventoryItemID("player", 0)"#)
                .unwrap(),
            2512
        );
        assert_eq!(
            s.eval::<i64>(r#"return GetInventoryItemCount("player", 0)"#)
                .unwrap(),
            200
        );
        // An empty slot: nil id/texture/quality, count 0 (the wiki's documented empty shape).
        assert!(s
            .eval::<bool>(r#"return GetInventoryItemID("player", 5) == nil"#)
            .unwrap());
        assert!(s
            .eval::<bool>(r#"return GetInventoryItemTexture("player", 5) == nil"#)
            .unwrap());
        assert_eq!(
            s.eval::<i64>(r#"return GetInventoryItemCount("player", 5)"#)
                .unwrap(),
            0
        );
        assert!(s
            .eval::<bool>(r#"return GetInventoryItemQuality("player", 5) == nil"#)
            .unwrap());
        // A non-player token: the empty shape (the INV_SLOT fields are PRIVATE — player only).
        assert!(s
            .eval::<bool>(r#"return GetInventoryItemID("target", 1) == nil"#)
            .unwrap());
        assert_eq!(
            s.eval::<i64>(r#"return GetInventoryItemCount("target", 1)"#)
                .unwrap(),
            0
        );
        // Out-of-range slots: the empty shape, no error.
        assert!(s
            .eval::<bool>(r#"return GetInventoryItemID("player", 25) == nil"#)
            .unwrap());
        assert!(s
            .eval::<bool>(r#"return GetInventoryItemID("player", -1) == nil"#)
            .unwrap());
    }

    #[test]
    fn get_inventory_slot_info_serves_the_dbc_rows() {
        let s = UiScript::new().unwrap();
        assert_eq!(
            s.eval::<(i64, String, bool)>(r#"return GetInventorySlotInfo("HeadSlot")"#)
                .unwrap(),
            (
                1,
                "Interface\\Paperdoll\\UI-PaperDoll-Slot-Head".into(),
                false
            )
        );
        // The DBC's oddballs: BackSlot shows the Chest art, AmmoSlot the Ranged art.
        assert_eq!(
            s.eval::<(i64, String, bool)>(r#"return GetInventorySlotInfo("BackSlot")"#)
                .unwrap(),
            (
                15,
                "Interface\\Paperdoll\\UI-PaperDoll-Slot-Chest".into(),
                false
            )
        );
        assert_eq!(
            s.eval::<(i64, String, bool)>(r#"return GetInventorySlotInfo("AmmoSlot")"#)
                .unwrap(),
            (
                0,
                "Interface\\Paperdoll\\UI-PaperDoll-Slot-Ranged".into(),
                false
            )
        );
        // The four equipped-bag icons (decision 0216 slice 2's bag bar): ids 20..23, re-verified
        // this session against the real PaperDollItemFrame.dbc — every one shares the same empty-
        // slot art.
        for (name, id) in [
            ("Bag0Slot", 20),
            ("Bag1Slot", 21),
            ("Bag2Slot", 22),
            ("Bag3Slot", 23),
        ] {
            assert_eq!(
                s.eval::<(i64, String, bool)>(&format!(r#"return GetInventorySlotInfo("{name}")"#))
                    .unwrap(),
                (
                    id,
                    "Interface\\Paperdoll\\UI-PaperDoll-Slot-Bag".into(),
                    false
                ),
                "{name}"
            );
        }
        assert_eq!(
            s.eval::<(i64, String, bool)>(r#"return GetInventorySlotInfo("SecondaryHandSlot")"#)
                .unwrap(),
            (
                17,
                "Interface\\Paperdoll\\UI-PaperDoll-Slot-SecondaryHand".into(),
                false
            )
        );
        // An unknown slot name is a Lua error (the client's own behavior).
        assert!(s
            .eval::<i64>(r#"return GetInventorySlotInfo("NoSuchSlot")"#)
            .is_err());
    }

    #[test]
    fn paperdoll_facing_persists_for_the_app_to_sample() {
        let s = UiScript::new().unwrap();
        assert_eq!(s.paperdoll_yaw(), 0.0, "unset default");
        s.run("BenillaPaperDollModel_SetFacing(0.61)").unwrap();
        assert_eq!(s.paperdoll_yaw(), 0.61);
        // Persistent, not a drain — two reads see the same value; a later write replaces it.
        assert_eq!(s.paperdoll_yaw(), 0.61);
        s.run("BenillaPaperDollModel_SetFacing(-0.5)").unwrap();
        assert_eq!(s.paperdoll_yaw(), -0.5);
    }
}
