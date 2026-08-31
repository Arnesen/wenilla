//! The app-side **character-window feed** (decision 0208 §3/§4): the bridge that turns our own
//! avatar's descriptor + the item stores into the plain snapshots the paper doll's Lua bindings
//! read, and into the Era events that drive its `OnEvent` repaints — the [`crate::ui_unit`]
//! pattern, for the combat-stats/inventory surface.
//!
//! Three jobs, each frame, before the VM ticks ([`UiInput`]):
//!
//! - **[`UnitCombatStats`]** from the self player's [`ObjectStore`] — the stat/resistance/
//!   damage/attack-power fields (all OWNER_ONLY, verified streamed in 0208) plus the three resolved
//!   skill pairs: the app picks WHICH skill line the equipped main-hand/ranged item uses
//!   (`weapon_subclass_skill` — vmangos `Item.cpp`'s table; no weapon = unarmed 162), reads its
//!   `PLAYER_SKILL_INFO` triplet and the Defense line's ([`SKILL_DEFENSE`]), so the engine-free
//!   bindings just serve the pairs.
//!
//!   The descriptor half is [`unit_combat_stats`], which any unit's store goes through — the pet
//!   paper doll's feed ([`crate::ui_pet_doll`], decision 1057) is the second caller. What
//!   [`combat_stats`] adds on top is exactly the player-only half: the four values derived from the
//!   equipped items, and the skill pairs, neither of which a creature has.
//! - **`InventorySlots`** from the inv-slot guids → [`Items`] objects → templates → the
//!   [`ItemDisplays`] icon — index 0 the ammo slot (`PLAYER_AMMO_ID`, its count summed across the
//!   backpack + the four equipped bags), 1..=19 the equipment slots, 20..=23 the four equipped-bag
//!   ICONS (decision 0216 slice 2's bag bar — the bag item itself, `INV_SLOT` 19..22, not its
//!   contents), all by the client's `GetInventorySlotInfo` ids. A slot whose template answer is
//!   still in flight carries its `item_id` with `icon: None` and fills on a later frame (the
//!   ask-once cache repaints via `UNIT_INVENTORY_CHANGED` when the view changes). Each equipment
//!   slot (1..=23) also carries an `|Hitem:…|h[Name]|h` `link`, its `PendingItemOps`-derived
//!   `locked` (decision 0208 phase 1b — the doll twin of `ui_items::feed_containers`'s bag-slot
//!   `.locked`), and its resolved `equip_slots` (`ui_items::find_equip_slot` over the item
//!   template's `inventoryType` — the cursor arc's "fit rule", decision 0216/0218).
//! - **The paper-doll booth yaw**: the pane's `Model:SetRotation` transcription writes the VM-side
//!   value ([`UiScript::paperdoll_yaw`]); the feed mirrors it onto [`PaperDollBooth`] so the booth
//!   re-bakes at the new facing (decision 0208 §5).
//!
//! Events, fired on snapshot transitions (grouped by the ref's own registration set,
//! `PaperDollFrame.lua:14-28` — arg1 `"player"`): `UNIT_STATS`, `UNIT_RESISTANCES`, `UNIT_DAMAGE`,
//! `UNIT_ATTACK_SPEED`, `UNIT_ATTACK_POWER`, `UNIT_RANGED_ATTACK_POWER`, `UNIT_RANGEDDAMAGE`,
//! `UNIT_ATTACK` (the weapon-skill pairs) — [`fire_stat_transitions`], shared with the pet feed —
//! and `UNIT_INVENTORY_CHANGED` on any slot-view change. A first snapshot counts as a transition of
//! every group (the `ui_unit` rule).

use bevy::prelude::*;

use benilla_protocol::messages::PLAYER_SKILL_SLOTS;
use benilla_ui::script::{
    weapon_subclass_skill, BankBagSlots, InvSlotView, InventorySlots, ScriptValue, SkillEntry,
    SkillsState, UiScript, UnitCombatStats, EQUIPMENT_BAG, SKILL_DEFENSE, SKILL_UNARMED,
};

use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::net::{ClientCommand, NetCommands, ObjectStore, SelfPlayer};
use crate::pending_item_ops::PendingItemOps;
use crate::portrait::PaperDollBooth;
use crate::ui_items::{find_equip_slot, item_link};
use crate::ui_script::{gate, UiInput};

/// Equipment slots, 0-based (`EQUIPMENT_SLOT_*`): the inv-slot array index of the main hand,
/// off hand, and ranged slots the weapon-skill / offhand / wand resolutions read.
const SLOT_MAIN_HAND: u8 = 15;
const SLOT_OFF_HAND: u8 = 16;
const SLOT_RANGED: u8 = 17;
/// `ItemPrototype` class 2 = weapon (vmangos `ItemPrototype.h`); the offhand-speed gate.
const ITEM_CLASS_WEAPON: u32 = 2;
/// Weapon subclass 19 = wand (`HasWandEquipped`).
const SUBCLASS_WAND: u32 = 19;

/// A stable comparison key for an f32 snapshot field (the event-transition diffs): quantized to
/// 1/32 so equal wire values always compare equal and real changes always register.
fn fixed(v: f32) -> i64 {
    (v * 32.0).round() as i64
}

/// The feed's change-tracking memory (the [`crate::ui_unit::UnitFeedState`] shape): the last
/// snapshots pushed, so events fire on transitions only.
///
/// All of it is what-we-told-the-VM, so all of it lives behind a [`crate::ui_script::VmMemo`]: a
/// memory about one VM is unreadable against the next (1290), so a `/reload` (1291) — which
/// replaces the VM without despawning the avatar — re-pushes both snapshots and re-fires their
/// events exactly as a fresh login does.
#[derive(Resource, Default)]
struct CharFeedState {
    vm: crate::ui_script::VmMemo<CharFeedMemo>,
}

/// The per-VM half of [`CharFeedState`] — the transition-diff bases.
#[derive(Default)]
struct CharFeedMemo {
    last_stats: Option<UnitCombatStats>,
    last_inv: Option<InventorySlots>,
    last_bank_bags: Option<BankBagSlots>,
    /// The gate's counter memories (1439) — the stores whose lazy resolves poison `is_changed`
    /// for this feed (the equipment templates' ask-once, the enchant-name creator lookups).
    items_objects: gate::Watch,
    items_templates: gate::Watch,
    names_generation: gate::Watch,
    /// `Items::enchant_display_epoch` — one step per displayable countdown change (the slot
    /// views read second-floored countdowns), including the last elapse's collapsing push.
    enchant_deadlines: gate::Watch,
}

/// Adds the per-frame character-window feed. The bindings live in `benilla-ui`'s `char_stats`;
/// this supplies their data (and the events) from ECS state.
pub(crate) struct UiCharPlugin;

impl Plugin for UiCharPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CharFeedState>().add_systems(
            Update,
            (
                // `.before(feed_containers)`: the container feed's `BAG_UPDATE` handlers read the
                // inventory surface synchronously — an addon mirroring a bag asks the bag ITEM's
                // own row (`GetInventoryItemLink("player", ContainerIDToInventoryID(bag))`,
                // Bagnon_Forever's size row) — so the inventory push must land first. Unordered,
                // the first in-world burst raced it and recorded every equipped bag linkless
                // (`s = "8,0,"` in the director's Bagnon_Forever data). The house "push before
                // fire" rule, across the two feeds.
                feed_char
                    .before(crate::ui_items::feed::feed_containers)
                    .before(UiInput),
                watch_skill_ups.before(UiInput),
                feed_skills.before(UiInput),
                drain_skill_abandons.after(UiInput),
            ),
        );
    }
}

/// The skill block [`watch_skill_ups`] last announced: the self guid it was read off, and that
/// character's `skillId → value` map — the diff base for both the chat lines and the event.
/// `None` = nothing announced yet.
type SkillBlock = Option<(u64, std::collections::HashMap<u16, u16>)>;

/// The client-generated skill-up feedback (decision 0437, landed with phase 2; gate corrected by
/// 1309): the server sends NO skill-up message at all — skills mutate only as
/// `PLAYER_SKILL_INFO` descriptor deltas (verified at the vmangos source: no chat send anywhere
/// in `UpdateSkill*`/`SetSkill`) — so the client diff-watches its own skill block exactly as the
/// real one does (the rank-field UpdateField watcher `0x5de180` → message facility `0x496720`,
/// tokens `ERR_SKILL_UP_SI` / `ERR_SKILL_GAINED_S` — wow-re tradeskill TU-E, correcting 0437's
/// "SKILL_RANK_UP" token guess), prints the GlobalStrings lines on the Skill chat channel, and
/// fires `SKILL_LINES_CHANGED` on ANY block change (the skills pane's repaint event; TU-E: the
/// real client fires it from a separate skill-manager TU on add/remove only — ours riding the
/// same watcher is a benign coarsening, the pane just repaints). **The message gate** (B19/B245,
/// decisions 1309/1314): both lines are skipped when the line's `SkillRaceClassInfo.flags`
/// carries `0x402` (`SkillRaceClass::skill_up_silent`) — the class spec lines, racials,
/// `GENERIC (DND)`, `Dual Wield` and the mount lines — **or when no row admits this race/class
/// at all** (the resolve's own empty branch, `0x5de352`) — which is why a real 1.12 ding
/// announces no skill at all (the level-up movers are all flagged) while weapon/profession
/// skill-ups still print. The
/// login fill (empty → populated) and a character switch (self guid change) re-seed silently —
/// the event fires, the lines don't. Still open: the exact chat channel of the real emitter
/// (TU-E left `0x496720`'s routing untraced; Skill is the `ChatTypeInfo` family's own key).
fn watch_skill_ups(
    script: Option<NonSendMut<UiScript>>,
    self_guid: Res<crate::net::SelfGuid>,
    self_store: Query<&ObjectStore, With<SelfPlayer>>,
    skill_lines: Option<Res<crate::ui_spellbook::SkillLines>>,
    mut chat: ResMut<crate::ui_chat::ChatLog>,
    mut prev: Local<crate::ui_script::VmMemo<SkillBlock>>,
) {
    let (Some(guid), Ok(store)) = (self_guid.0, self_store.single()) else {
        return;
    };
    // The memo is about the VM — it gates `SKILL_LINES_CHANGED`, the pane's only repaint wire — so
    // it re-seeds with the VM (decision 1290). With no VM there is nothing to announce to and
    // nothing worth remembering: the diff would only queue chat lines for a VM that does not exist
    // yet, which is a stale skill-up replayed at the next login. The next VM re-seeds silently.
    let Some(mut script) = script else {
        return;
    };
    let prev = prev.get(&script);
    let mut cur = std::collections::HashMap::new();
    for i in 0..PLAYER_SKILL_SLOTS {
        if let Some(s) = store.0.player_skill(i) {
            if s.skill_id != 0 {
                cur.insert(s.skill_id, s.value);
            }
        }
    }
    match prev.as_ref() {
        // First fill for this character: seed silently (no lines), announce the block once.
        None => {
            if cur.is_empty() {
                return;
            }
            script.fire_event("SKILL_LINES_CHANGED", vec![]);
            *prev = Some((guid, cur));
        }
        Some((prev_guid, _)) if *prev_guid != guid => {
            // A different character logged in — re-seed, never diff across characters.
            script.fire_event("SKILL_LINES_CHANGED", vec![]);
            *prev = Some((guid, cur));
        }
        Some((_, prev_map)) => {
            if *prev_map == cur {
                return;
            }
            // The 0x402 message gate resolves through the player's own race/class row, the same
            // lookup the pane's display law makes ([`skills_row`]).
            let (race, class) = (
                store.0.unit_race().unwrap_or(0),
                store.0.unit_class().unwrap_or(0),
            );
            for (&id, &value) in &cur {
                let line_id = u32::from(id);
                let name = || {
                    skill_lines
                        .as_ref()
                        .and_then(|s| s.catalog.line(line_id))
                        .map(|l| l.name.clone())
                };
                // The fn doc's message gate; `false` with no catalog (nothing to name it by
                // either — the line stays silent, as before the catalog loads).
                let announces = || {
                    skill_lines
                        .as_ref()
                        .is_some_and(|s| s.catalog.announces_skill_ups(line_id, race, class))
                };
                match prev_map.get(&id) {
                    // A rank-up: the ERR_SKILL_UP_SI line (GlobalStrings.lua:1838).
                    Some(&old) if value > old => {
                        if let Some(name) = name() {
                            // Both verdicts are logged — the retest's instrument: a moved line
                            // either announces or names the gate that held it.
                            if announces() {
                                debug!("chat: skill-up announced ({name} {old}→{value})");
                                chat.push_event(crate::ui_chat::ChatEvent::text_only(
                                    crate::ui_chat::ChatEventKind::Skill,
                                    format!("Your skill in {name} has increased to {value}."),
                                ));
                            } else {
                                debug!("chat: skill-up silenced ({name} {old}→{value}, the 0x402 gate)");
                            }
                        }
                    }
                    // A NEW line: the ERR_SKILL_GAINED_S line (GlobalStrings.lua:1837) — the
                    // fold-back's second token (wow-re `tradeskill` TU-E: the watcher emits
                    // both, first-gain vs rank-up).
                    None => {
                        if let Some(name) = name() {
                            if announces() {
                                debug!("chat: skill-gain announced ({name} at {value})");
                                chat.push_event(crate::ui_chat::ChatEvent::text_only(
                                    crate::ui_chat::ChatEventKind::Skill,
                                    format!("You have gained the {name} skill."),
                                ));
                            } else {
                                debug!(
                                    "chat: skill-gain silenced ({name} at {value}, the 0x402 gate)"
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
            script.fire_event("SKILL_LINES_CHANGED", vec![]);
            *prev = Some((guid, cur));
        }
    }
}

/// The skills-pane feed (decision 0437 phase 4, corrected by 1091): every `PLAYER_SKILL_INFO` line
/// the real client would LIST, resolved to a flat [`SkillEntry`] — name off `SkillLine.dbc`, group
/// off the line's `categoryId` × `SkillLineCategory.dbc` (name + displayOrder) — pushed via
/// `set_skills` on change; the ENGINE groups/sorts/folds.
///
/// The inclusion predicate is the client's own list build (`0x4d2cb0`, wow-re
/// `system/tradeskill/scratch/skillframe-display-list.md`), transcribed in its order: a line needs
/// a `SkillLine.dbc` row, an admitting `SkillRaceClassInfo` row and a real `SkillLineCategory` row;
/// `flags & 0x2` drops it outright; and a line held at **rank 0** appears only if its flags admit
/// it at this player's level. Note what is NOT a filter: the `Not Displayed` category (12). We used
/// to hide on it, which happened to hide `GENERIC (DND)` for the right-looking reason — the client
/// drops that line by its `0x2` bit like `Dual Wield` and the racials, and lists a category-12 line
/// without `0x2` under its own header (decision 1091).
fn feed_skills(
    script: Option<NonSendMut<UiScript>>,
    self_store: Query<&ObjectStore, With<SelfPlayer>>,
    skill_lines: Option<Res<crate::ui_spellbook::SkillLines>>,
    mut last: Local<crate::ui_script::VmMemo<Option<SkillsState>>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let last = last.get(&script);
    let (Ok(store), Some(skill_lines)) = (self_store.single(), skill_lines.as_deref()) else {
        return;
    };
    // The display predicate reads the player's own race/class (the SkillRaceClassInfo row-match —
    // the spellbook's own General-collapse inputs, `ui_spellbook::build_book`) and level (the
    // untrained gate).
    let (race, class) = (
        store.0.unit_race().unwrap_or(0),
        store.0.unit_class().unwrap_or(0),
    );
    let level = store.0.unit_level().unwrap_or(0);
    let mut entries = Vec::new();
    for i in 0..PLAYER_SKILL_SLOTS {
        let Some(slot) = store.0.player_skill(i) else {
            continue;
        };
        if let Some(e) = skills_row(&slot, &skill_lines.catalog, race, class, level) {
            entries.push(e);
        }
    }
    let fresh = SkillsState { entries };
    if last.as_ref() == Some(&fresh) {
        return;
    }
    script.set_skills(fresh.clone());
    *last = Some(fresh);
}

/// One `PLAYER_SKILL_INFO` slot as the Skills tab's row — or `None` when the real client's list
/// build would not list it at all ([`feed_skills`]'s doc for the citations; the order of the tests
/// is the client's own, `0x4d2cb0`). Pure, so the whole display predicate is testable against real
/// DBC data and a real character's skill block (`ui_script::skills_frame_tests`).
pub(crate) fn skills_row(
    slot: &benilla_protocol::messages::PlayerSkillSlot,
    catalog: &benilla_formats::SkillLineCatalog,
    race: u8,
    class: u8,
    level: u32,
) -> Option<SkillEntry> {
    if slot.skill_id == 0 {
        return None;
    }
    let id = u32::from(slot.skill_id);
    // No SkillLine row — nothing to name it by, and the client lists nothing.
    let line = catalog.line(id)?;
    // No row admitting this race/class → the client drops the line (its `!srci → continue`).
    let rc = catalog.race_class(id, race, class)?;
    // A category that must really exist (the client's own null-row test) — and note the category
    // ITSELF is never a filter: `Not Displayed` (12) is a header like any other.
    let (category_name, category_order) = catalog.category(line.category_id)?;
    // The hide bit: `Dual Wield`, the racials, the mount lines, `GENERIC (DND)`.
    if rc.hidden() {
        return None;
    }
    // An untrained line shows only where the flags admit it at this level.
    if slot.value == 0 && !rc.displays_untrained(level) {
        return None;
    }
    Some(SkillEntry {
        skill_id: id,
        name: line.name.clone(),
        value: u32::from(slot.value),
        max: u32::from(slot.max),
        temp_bonus: i32::from(slot.temp_bonus),
        perm_bonus: i32::from(slot.perm_bonus),
        min_level: rc.min_level,
        cost_index: rc.cost_index,
        category_id: line.category_id,
        category_name: category_name.to_string(),
        category_order,
        description: line.description.clone(),
        // The client ANDs the DBC's unlearnable bit with a nonzero skill STEP (`0x4d3953`).
        abandonable: slot.step != 0 && rc.unlearnable(),
        mono: rc.mono(),
    })
}

/// The abandon drain (the skills pane's unlearn button → the `UNLEARN_SKILL` popup's accept →
/// the engine's `AbandonSkill` queue): each queued skill line id becomes one
/// `CMSG_UNLEARN_SKILL`. Nothing else happens client-side — the server's `SetSkill(id, 0, 0)`
/// returns as a `PLAYER_SKILL_INFO` delta and [`feed_skills`] re-pushes the shrunken list
/// (vmangos `SkillHandler.cpp`; the gossip/trainer drains' own shape).
fn drain_skill_abandons(script: Option<NonSendMut<UiScript>>, commands: Res<NetCommands>) {
    let Some(mut script) = script else {
        return;
    };
    for skill_id in script.take_skill_abandons() {
        debug!("ui_char: abandon skill line {skill_id}");
        let _ = commands.0.send(ClientCommand::UnlearnSkill { skill_id });
    }
}

/// Resolve one equipped slot's item template entry (`None` = empty slot, or the item object /
/// template hasn't streamed yet).
fn slot_entry(store: &ObjectStore, items: &Items, slot0: u8) -> Option<u32> {
    let guid = store.0.player_inv_slot(slot0)?;
    items.object(guid)?.object_entry()
}

/// The equipped item's weapon-skill line id: the vmangos `Item.cpp` subclass table for a class-2
/// item, unarmed (162) for an empty hand or a non-weapon / unmapped subclass.
fn weapon_skill_id(
    store: &ObjectStore,
    items: &mut Items,
    commands: &NetCommands,
    slot0: u8,
) -> u32 {
    let Some(entry) = slot_entry(store, items, slot0) else {
        return SKILL_UNARMED;
    };
    let Some(t) = items.template(entry, 0, commands) else {
        return SKILL_UNARMED; // in flight — refined next frame when the template lands
    };
    if t.class != ITEM_CLASS_WEAPON {
        return SKILL_UNARMED;
    }
    weapon_subclass_skill(t.subclass).unwrap_or(SKILL_UNARMED)
}

/// Read a skill line's `(value, temp+perm bonus)` pair from the `PLAYER_SKILL_INFO` triplets;
/// `(0, 0)` when the line isn't known (a ranged pair with nothing equipped).
fn skill_pair(store: &ObjectStore, skill_id: u32) -> (u32, i32) {
    for i in 0..PLAYER_SKILL_SLOTS {
        if let Some(s) = store.0.player_skill(i) {
            if u32::from(s.skill_id) == skill_id {
                return (
                    u32::from(s.value),
                    i32::from(s.temp_bonus) + i32::from(s.perm_bonus),
                );
            }
        }
    }
    (0, 0)
}

/// Build the **descriptor-only** combat-stats snapshot for ANY unit's store (field mappings pinned
/// in decision 0208; the absent shapes are the descriptor's zero defaults, `percent` 1.0).
///
/// Nothing here reads inventory, templates or skill lines — so it is exactly the part a pet can go
/// through ([`crate::ui_pet_doll`], decision 1057). The `player_*` reads are deliberately left in:
/// a creature has no PLAYER block, so they read absent and fall to their defaults, which is the
/// right pet answer (no buff decomposition, `damage_percent` the divide-safe 1.0) rather than a
/// second unit-only copy of the same twenty lines.
///
/// **That premise is load-bearing, and it was false until decision 1081.** A created store used to
/// answer `Some(0)` for *any* absent index, PLAYER block or not, so on a live pet every default
/// here was dead code — `damage_percent` came through as `0` and the ref's `damage / percent` made
/// the pet sheet read `inf - inf` / `nan`. The protocol layer now bounds "absent = 0" to the
/// object's own descriptor, which is what makes this shared core honest for a creature.
/// A swing time in ms, with `0` (and absent) read as "no swing time yet" → the vanilla 2000. Never
/// zero, because it is the reference's `damage / speed` divisor.
fn base_swing(streamed: Option<u32>) -> u32 {
    streamed.filter(|&ms| ms != 0).unwrap_or(2000)
}

pub(crate) fn unit_combat_stats(store: &ObjectStore) -> UnitCombatStats {
    let f = &store.0;

    let mut stats = [0i32; 5];
    let mut stat_pos = [0i32; 5];
    let mut stat_neg = [0i32; 5];
    for i in 0..5u8 {
        stats[usize::from(i)] = f.unit_stat(i).unwrap_or(0) as i32;
        // The four buff-split arrays are INT on the wire (decision 1397) — they used to be read as
        // f32 and rounded, which turned every real value into `0` and is why B165/B251's stats
        // never went green.
        stat_pos[usize::from(i)] = f.player_posstat(i).unwrap_or(0);
        stat_neg[usize::from(i)] = f.player_negstat(i).unwrap_or(0);
    }
    let mut resistances = [0i32; 7];
    let mut resistance_pos = [0i32; 7];
    let mut resistance_neg = [0i32; 7];
    for i in 0..7u8 {
        resistances[usize::from(i)] = f.unit_resistance(i).unwrap_or(0);
        resistance_pos[usize::from(i)] = f.player_resistance_buff_pos(i).unwrap_or(0);
        resistance_neg[usize::from(i)] = f.player_resistance_buff_neg(i).unwrap_or(0);
    }

    let (ap_pos, ap_neg) = f.unit_attack_power_mods();
    let (rap_pos, rap_neg) = f.unit_ranged_attack_power_mods();

    UnitCombatStats {
        stats,
        stat_pos,
        stat_neg,
        resistances,
        resistance_pos,
        resistance_neg,
        min_damage: f.unit_min_damage().unwrap_or(0.0),
        max_damage: f.unit_max_damage().unwrap_or(0.0),
        min_offhand_damage: f.unit_min_offhand_damage().unwrap_or(0.0),
        max_offhand_damage: f.unit_max_offhand_damage().unwrap_or(0.0),
        physical_bonus_pos: f.player_mod_damage_done_pos(0).unwrap_or(0),
        physical_bonus_neg: f.player_mod_damage_done_neg(0).unwrap_or(0),
        damage_percent: f.player_mod_damage_done_pct(0).unwrap_or(1.0),
        // 2000 ms is the vanilla base swing when the field hasn't streamed — keeps the ref's
        // damage/speed division finite on the first frames. **`0` counts as unstreamed**, not as a
        // swing time: on a created store an absent in-block field reads `Some(0)` (the descriptor's
        // zero), so a plain `unwrap_or` would never fire and `damage / speed` would go to infinity
        // (the shape of the bug decision 1081 is about, one field over).
        main_attack_time_ms: base_swing(f.unit_base_attack_time(0)),
        offhand_attack_time_ms: base_swing(f.unit_base_attack_time(1)),
        attack_power: f.unit_attack_power().unwrap_or(0),
        attack_power_pos: i32::from(ap_pos),
        attack_power_neg: i32::from(ap_neg),
        ranged_attack_power: f.unit_ranged_attack_power().unwrap_or(0),
        ranged_attack_power_pos: i32::from(rap_pos),
        ranged_attack_power_neg: i32::from(rap_neg),
        ranged_attack_time_ms: base_swing(f.unit_ranged_attack_time()),
        ranged_min_damage: f.unit_min_ranged_damage().unwrap_or(0.0),
        ranged_max_damage: f.unit_max_ranged_damage().unwrap_or(0.0),
        // The equipment- and skill-derived half is the PLAYER feed's ([`combat_stats`]); a unit
        // with neither keeps these defaults.
        has_offhand: false,
        has_wand: false,
        main_weapon_skill: (0, 0),
        ranged_weapon_skill: (0, 0),
        defense_skill: (0, 0),
    }
}

/// The player's snapshot: [`unit_combat_stats`] plus the half only *we* have — the two values that
/// need the equipped items' templates, and the three `PLAYER_SKILL_INFO` pairs.
fn combat_stats(store: &ObjectStore, items: &mut Items, commands: &NetCommands) -> UnitCombatStats {
    // The offhand-speed gate is an offhand *weapon* (a shield doesn't swing); the wand check is
    // the ranged item's subclass. Both read the equipped item's template (ask-once in flight →
    // false this frame, refined when it lands).
    let has_offhand = slot_entry(store, items, SLOT_OFF_HAND)
        .and_then(|e| items.template(e, 0, commands))
        .is_some_and(|t| t.class == ITEM_CLASS_WEAPON);
    let has_wand = slot_entry(store, items, SLOT_RANGED)
        .and_then(|e| items.template(e, 0, commands))
        .is_some_and(|t| t.class == ITEM_CLASS_WEAPON && t.subclass == SUBCLASS_WAND);

    let main_skill = weapon_skill_id(store, items, commands, SLOT_MAIN_HAND);
    let ranged_skill_id = slot_entry(store, items, SLOT_RANGED)
        .and_then(|e| items.template(e, 0, commands))
        .filter(|t| t.class == ITEM_CLASS_WEAPON)
        .and_then(|t| weapon_subclass_skill(t.subclass));

    UnitCombatStats {
        has_offhand,
        has_wand,
        main_weapon_skill: skill_pair(store, main_skill),
        ranged_weapon_skill: ranged_skill_id.map_or((0, 0), |id| skill_pair(store, id)),
        // `UnitDefense`'s pair. Its repaint wire is `SKILL_LINES_CHANGED` (which the ref's
        // `PaperDollFrame` registers, l.28, and `watch_skill_ups` already fires), NOT a
        // `UNIT_DEFENSE` — the character sheet never registers one.
        defense_skill: skill_pair(store, SKILL_DEFENSE),
        ..unit_combat_stats(store)
    }
}

/// One equipped slot's view: entry + count from the item object, icon/name/quality/link/
/// equip_slots from the ask-once template (icon `None` while the answer is in flight — the
/// empty-slot art shows and the landing template flips the view, which fires
/// `UNIT_INVENTORY_CHANGED`), `locked` from the app's `PendingItemOps` (decision 0208 phase 1b —
/// the same feed `ui_items::feed_containers` reads for a bag slot's own `.locked`).
///
/// Takes the item's **guid and its live-API id** rather than a wire slot index, because the two
/// bands that use it resolve their guid out of two different descriptor arrays: the doll's
/// `PLAYER_FIELD_INV_SLOT_*` (live id = wire slot + 1, so equipment 1..=19 and the four
/// equipped-bag icons 20..=23) and the bank bags' own `PLAYER_FIELD_BANK_BAG_SLOT_*` (live ids
/// 64..=69). Everything past the guid — template, enchants, durability, the pending lock — is
/// identical for both, which is the whole reason there is one function here and not two.
#[allow(clippy::too_many_arguments)] // the slot resolve's full read set — the bag feed's twin
fn slot_view(
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    rolls: crate::items::RollCatalogs,
    commands: &NetCommands,
    pending: &PendingItemOps,
    names: &mut crate::names::NameCache,
    guid: u64,
    live_id: u32,
) -> Option<InvSlotView> {
    // The temp-enchant countdowns, read before the object borrow (both live on `Items`) — the bag
    // feed's twin.
    let enchant_ms: [Option<u64>; 7] =
        std::array::from_fn(|s| items.enchant_remaining_display_ms(guid, s as u32));
    let obj = items.object(guid)?;
    let entry = obj.object_entry()?;
    let count = obj.item_stack_count().unwrap_or(1).max(1);
    // A worn ammo container's contents, collected while the bag object is borrowed — applied
    // below once the template's class says quiver.
    let content_guids: Vec<u64> = {
        let slots = obj.container_num_slots().unwrap_or(0).min(36) as u8;
        (0..slots).filter_map(|s| obj.container_slot(s)).collect()
    };
    // The live durability pair (see the bag feed's twin) — the equipped tooltip's line.
    let durability = obj
        .item_durability()
        .zip(obj.item_max_durability())
        .filter(|&(_, max)| max > 0);
    // ITEM_FIELD_FLAGS — the broken/alert laws' wrapped (0x08) and force-red (0x10) bits.
    let flags = obj.item_flags().unwrap_or(0);
    // `0x5da2c0` — soulbound, or carrying a binding enchant: the equipped tooltip's §6 Soulbound
    // override (B310 — the doll is exactly where it was reported). Read off the raw descriptor,
    // never off the enchant LINES below.
    let already_bound = crate::items::already_bound(obj, rolls.enchants);
    // ITEM_FIELD_CREATOR → the ask-once name cache — the equipped tooltip's "<Made by %s>"
    // line (see the bag feed's twin in `ui_items::feed::resolve_slot`).
    let creator = obj
        .item_creator()
        .filter(|&g| g != 0)
        .and_then(|g| names.resolve(g, commands).map(str::to_string));
    // The item's own 7 enchant slots — the equipped tooltip's enchant lines (decision 0915). OUR
    // gear streams as item objects, so this is the full array; an INSPECTED player's tooltip sees
    // only the 2 slots their descriptor broadcasts (`ui_inspect`), as the reference does.
    let enchants = crate::items::enchant_lines(
        (0..7).map(|s| {
            (
                s,
                obj.item_enchant(s).unwrap_or(0),
                obj.item_enchant_charges(s),
                enchant_ms[usize::from(s)],
            )
        }),
        rolls.enchants,
    );
    // `ITEM_FIELD_RANDOM_PROPERTIES_ID` — the roll behind the NAME's "of the Bear" (1547); the
    // roll's enchants are already in the slots read just above.
    let roll = obj.item_random_properties_id();
    let t = items.template(entry, guid, commands);
    let (name, quality, display, link, equip_slots, class, bar_placeable) = match t {
        Some(t) => {
            let name = rolls.name(&t.name, roll);
            (
                Some(name.clone()),
                t.quality as i32,
                t.display_info_id,
                Some(crate::ui_items::item_link_full(
                    entry, 0, roll, 0, &name, t.quality,
                )),
                find_equip_slot(t.inventory_type),
                t.class,
                t.placeable_on_action_bar(),
            )
        }
        None => (None, 0, 0, None, Vec::new(), 0, false),
    };
    // The quiver's bag-bar count is what's INSIDE it: the ref's `GetInventoryItemCount("player",
    // bagSlot)` returns the ammo left in a worn ammo container — the "162" on the bag bar — and
    // the item's own stack (1) for any other bag, which the count text then hides. Gated on
    // ITEM_CLASS_QUIVER 11 (INFERRED from the ref UI's observable behavior — regular bags never
    // show a contents sum; named in the decision record).
    let count = if class == 11 {
        content_guids
            .iter()
            .filter_map(|&g| items.object(g))
            .map(|o| o.item_stack_count().unwrap_or(1).max(1))
            .sum()
    } else {
        count
    };
    let icon = icons
        .and_then(|i| i.catalog.get(display))
        .and_then(|d| d.icon.clone());
    Some(InvSlotView {
        item_id: entry,
        icon,
        count,
        quality,
        name,
        link,
        durability,
        flags,
        already_bound,
        locked: pending.contains(EQUIPMENT_BAG, live_id),
        equip_slots,
        bar_placeable,
        creator,
        enchants,
    })
}

/// Total carried ammo matching `ammo_id`: the backpack's 16 slots + the four equipped bags'
/// contents (`GetInventoryItemCount("player", ammo)` counts what you can actually shoot — bank
/// and buyback never join).
fn ammo_count(store: &ObjectStore, items: &Items, ammo_id: u32) -> u32 {
    let mut n = 0;
    let mut add = |guid: Option<u64>| {
        if let Some(o) = guid.and_then(|g| items.object(g)) {
            if o.object_entry() == Some(ammo_id) {
                n += o.item_stack_count().unwrap_or(1);
            }
        }
    };
    for i in 0..16u8 {
        add(store.0.player_pack_slot(i));
    }
    for b in 19..23u8 {
        if let Some(bag) = store.0.player_inv_slot(b).and_then(|g| items.object(g)) {
            let slots = bag.container_num_slots().unwrap_or(0).min(36) as u8;
            for s in 0..slots {
                if let Some(o) = bag.container_slot(s).and_then(|g| items.object(g)) {
                    if o.object_entry() == Some(ammo_id) {
                        n += o.item_stack_count().unwrap_or(1);
                    }
                }
            }
        }
    }
    n
}

/// Build the 24-wide inventory snapshot: `[0]` ammo, `[1..=19]` the equipment slots, `[20..=23]`
/// the four equipped-bag icons (decision 0216 slice 2's bag bar — the bag ITEM occupying `INV_SLOT`
/// 19..22, not its contents) — all the client's 1-based `GetInventorySlotInfo` ids over the
/// 0-based inv-slot array.
fn inventory_slots(
    store: &ObjectStore,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    rolls: crate::items::RollCatalogs,
    commands: &NetCommands,
    pending: &PendingItemOps,
    names: &mut crate::names::NameCache,
) -> InventorySlots {
    let mut inv: InventorySlots = Default::default();
    let ammo_id = store.0.player_ammo_id().unwrap_or(0);
    if ammo_id != 0 {
        let t = items.template(ammo_id, 0, commands);
        let (name, quality, display, link) = match t {
            Some(t) => (
                Some(t.name.clone()),
                t.quality as i32,
                t.display_info_id,
                Some(item_link(ammo_id, &t.name, t.quality)),
            ),
            None => (None, 0, 0, None),
        };
        let icon = icons
            .and_then(|i| i.catalog.get(display))
            .and_then(|d| d.icon.clone());
        inv[0] = Some(InvSlotView {
            durability: None,
            flags: 0,
            // Ammo is the one doll slot with no item OBJECT behind it (it is built from the
            // template alone), so the runtime-bind predicate has nothing to read.
            already_bound: false,
            item_id: ammo_id,
            icon,
            count: ammo_count(store, items, ammo_id),
            quality,
            name,
            link,
            // The ammo slot is a named deferral this slice (decision 0208 phase 1b): no click
            // path wires it, so there's nothing for a pending op to lock and nowhere for it to
            // fit — both stay the inert default.
            locked: false,
            equip_slots: Vec::new(),
            // Ammo is a named deferral too: no drag path reaches the bar from this slot.
            bar_placeable: false,
            creator: None,
            // Ammo carries no enchant slots to read: the wire never streams an ammo instance
            // here, only the template id off the player descriptor.
            enchants: Vec::new(),
        });
    }
    // Equipment 1..=19, then Bag0Slot..Bag3Slot (ids 20..23, PaperDollItemFrame.dbc-verified) —
    // the bag bar's icon source: the equipped bag ITEM at INV_SLOT 19..22 (BAG_SLOT_FIRST../
    // SLOT_BAG_FIRST in the container feed's own numbering). One band: the doll's live id is its
    // wire slot plus one across the whole range.
    for live in 1..=23u32 {
        let Some(guid) = store.0.player_inv_slot(live as u8 - 1).filter(|g| *g != 0) else {
            continue;
        };
        inv[live as usize] = slot_view(items, icons, rolls, commands, pending, names, guid, live);
    }
    inv
}

/// The six bank-bag slots' views (live ids 64..=69) — the same [`slot_view`] resolution as any
/// doll slot, off the descriptor's own `PLAYER_FIELD_BANK_BAG_SLOT_*` guids.
///
/// These are inventory slots, not bank-*window* state: the reference's bank paints and picks up a
/// bank bag through `GetInventoryItemTexture`/`PickupBagFromSlot` at
/// `BankButtonIDToInvSlotID(i, 1)` (BankFrame.lua:28-35, 194-198), and the guids stream in the
/// player descriptor whether or not a banker is open. So they ride the inventory feed, beside the
/// doll snapshot, rather than the `BankState` the bank window pushes.
#[allow(clippy::too_many_arguments)] // [`slot_view`]'s read set, one band over
fn bank_bag_slots(
    store: &ObjectStore,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    rolls: crate::items::RollCatalogs,
    commands: &NetCommands,
    pending: &PendingItemOps,
    names: &mut crate::names::NameCache,
) -> BankBagSlots {
    let mut bags: BankBagSlots = Default::default();
    for (i, bag) in bags.iter_mut().enumerate() {
        let Some(guid) = store.0.player_bank_bag_slot(i as u8).filter(|g| *g != 0) else {
            continue;
        };
        *bag = slot_view(
            items,
            icons,
            rolls,
            commands,
            pending,
            names,
            guid,
            BANK_BAG_LIVE_FIRST + i as u32,
        );
    }
    bags
}

/// `BankButtonIDToInvSlotID(1, isBag)` — the live-API id of the FIRST bank-bag slot. The band is
/// six wide (`BANK_BAG_SLOT_COUNT`).
const BANK_BAG_LIVE_FIRST: u32 = 64;

/// Fire the paper doll's per-group repaint events for one unit's snapshot transition, `arg1 =
/// token` — the [`crate::ui_unit::fire_transitions`] shape, for the combat-stats surface.
///
/// The groups ARE the ref's own event registrations (`PaperDollFrame.lua:14-28`, which
/// `PetPaperDollFrame.lua:12-20` repeats verbatim for the pet page), so each event fires exactly
/// when a value some handler of it reads has moved; `prev == None` (a first snapshot) counts as a
/// transition of every group, the `ui_unit` rule.
///
/// Shared by both feeds rather than transcribed twice (decision 1057): the pet page calls the same
/// `PaperDollFrame_Set*` helpers off the same events, so two copies of this grouping could only
/// ever drift into one page repainting on an edge the other misses.
pub(crate) fn fire_stat_transitions(
    script: &mut UiScript,
    token: &str,
    prev: Option<&UnitCombatStats>,
    stats: &UnitCombatStats,
) {
    let tok = || ScriptValue::Str(token.to_string());
    let changed = |sel: fn(&UnitCombatStats) -> Vec<i64>| prev.is_none_or(|p| sel(p) != sel(stats));
    if changed(|s| {
        let mut v: Vec<i64> = s.stats.iter().map(|&x| i64::from(x)).collect();
        v.extend(
            s.stat_pos
                .iter()
                .chain(s.stat_neg.iter())
                .map(|&x| i64::from(x)),
        );
        v
    }) {
        script.fire_event("UNIT_STATS", vec![tok()]);
    }
    if changed(|s| {
        s.resistances
            .iter()
            .chain(s.resistance_pos.iter())
            .chain(s.resistance_neg.iter())
            .map(|&x| i64::from(x))
            .collect()
    }) {
        script.fire_event("UNIT_RESISTANCES", vec![tok()]);
    }
    if changed(|s| {
        vec![
            fixed(s.min_damage),
            fixed(s.max_damage),
            fixed(s.min_offhand_damage),
            fixed(s.max_offhand_damage),
            i64::from(s.physical_bonus_pos),
            i64::from(s.physical_bonus_neg),
            fixed(s.damage_percent),
        ]
    }) {
        script.fire_event("UNIT_DAMAGE", vec![tok()]);
    }
    if changed(|s| {
        vec![
            i64::from(s.main_attack_time_ms),
            i64::from(s.offhand_attack_time_ms),
            i64::from(s.has_offhand),
        ]
    }) {
        script.fire_event("UNIT_ATTACK_SPEED", vec![tok()]);
    }
    if changed(|s| {
        vec![
            i64::from(s.attack_power),
            i64::from(s.attack_power_pos),
            i64::from(s.attack_power_neg),
        ]
    }) {
        script.fire_event("UNIT_ATTACK_POWER", vec![tok()]);
    }
    if changed(|s| {
        vec![
            i64::from(s.ranged_attack_power),
            i64::from(s.ranged_attack_power_pos),
            i64::from(s.ranged_attack_power_neg),
            i64::from(s.has_wand),
        ]
    }) {
        script.fire_event("UNIT_RANGED_ATTACK_POWER", vec![tok()]);
    }
    if changed(|s| {
        vec![
            fixed(s.ranged_min_damage),
            fixed(s.ranged_max_damage),
            i64::from(s.ranged_attack_time_ms),
        ]
    }) {
        script.fire_event("UNIT_RANGEDDAMAGE", vec![tok()]);
    }
    if changed(|s| {
        vec![
            i64::from(s.main_weapon_skill.0),
            i64::from(s.main_weapon_skill.1),
            i64::from(s.ranged_weapon_skill.0),
            i64::from(s.ranged_weapon_skill.1),
        ]
    }) {
        script.fire_event("UNIT_ATTACK", vec![tok()]);
    }
}

#[allow(clippy::too_many_arguments)]
fn feed_char(
    script: Option<NonSendMut<UiScript>>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    changed_self: Query<(), (With<SelfPlayer>, Changed<ObjectStore>)>,
    mut items: ResMut<Items>,
    icons: Option<Res<ItemDisplays>>,
    // `SpellItemEnchantment`'s name column — the equipped tooltip's enchant lines (decision 0915).
    enchants: Option<Res<crate::items::Enchants>>,
    // `ItemRandomProperties` — the roll behind an equipped item's "of the Monkey" name (1547).
    props: Option<Res<crate::items::RandomProperties>>,
    commands: Res<NetCommands>,
    mut feed: ResMut<CharFeedState>,
    mut booth: ResMut<PaperDollBooth>,
    pending: Res<PendingItemOps>,
    mut names: ResMut<crate::names::NameCache>,
) {
    let Some(mut script) = script else {
        return;
    };
    // The pane's Model:SetRotation transcription owns the yaw; the booth mirrors it (0208 §5).
    // Write-if-different: an unconditional write would mark the booth changed every frame.
    let yaw = script.paperdoll_yaw();
    if booth.yaw != yaw {
        booth.yaw = yaw;
    }

    // Resolved against THIS VM (1290/1291): a `/reload`'s replacement reads a fresh memo, so both
    // snapshots below re-push and their events re-fire for it.
    let (memo, vm_reset) = feed.vm.get_reset(&script);

    let Some(store) = self_q.iter().next() else {
        // The self store's absence is NO DATA, never "the player has nothing equipped". The one
        // window this branch runs with a previously-fed VM is the logout despawn frames —
        // `SMSG_LOGOUT_COMPLETE` despawns the entity a full Update before `OnExit(InWorld)` runs
        // the shutdown, and the `PLAYER_LOGOUT` handlers that follow still read equipment
        // (`GetInventoryItemLink("player", …)` at logout is a stock addon save pattern; the
        // reference keeps it valid through its shutdown). The clear this branch used to push was
        // a pre-1290 relic: one VM then lived across logouts and the next character must not see
        // this one's inventory — now the VM dies with the session and a fresh one starts empty
        // ([`crate::ui_script::end_ui_session`]), so the clear only ever fired into a VM about to
        // run its logout handlers. Same law as the container feed
        // (`ui_items::feed::apply_container_source`).
        return;
    };

    // `GetWeaponEnchantInfo`'s whole data path (the buff bar's TemporaryEnchantFrame row, plus 8
    // corpus addons). Pushed EVERY frame and change-gated by nothing — including the 1439 gate
    // below: the remaining time is a live countdown, so riding a snapshot diff would fire
    // `UNIT_INVENTORY_CHANGED` on every tick. The reference recomputes it per call for exactly
    // the same reason (`0x5d9d00`). Hoisted above the gate (it used to sit last): the push is
    // order-free of the two snapshots, and a handler they fire reads the fresher value this way.
    script.set_weapon_enchants(
        weapon_enchant(store, &items, EQUIPMENT_SLOT_MAINHAND),
        weapon_enchant(store, &items, EQUIPMENT_SLOT_OFFHAND),
    );

    // The gate (1439), around the two snapshot builds only: the self descriptor (equipment
    // slots, every stat field), the item stores behind the equipped templates and enchant
    // names, the two catalogs, and the pending-op locks (held open while in flight; the
    // resolving frame's own field update moves the object epoch).
    let objects_moved = memo.items_objects.moved(items.object_epoch());
    let templates_moved = memo.items_templates.moved(items.template_epoch());
    let names_moved = memo.names_generation.moved(names.generation());
    // The DISPLAY epoch, not a per-frame hold-open: the snapshot reads second-floored countdowns
    // (`enchant_remaining_display_ms`), so it can only differ when this counter steps — once a
    // second per ticking enchant, plus one final step at the elapse. Holding the gate open on
    // `live_enchant_deadlines() > 0` (the first 1439 shape) rebuilt both snapshots and fired
    // `UNIT_INVENTORY_CHANGED` at frame rate for the whole life of a poison.
    let deadlines_moved = memo.enchant_deadlines.moved(items.enchant_display_epoch());
    let self_changed = !changed_self.is_empty();
    // `is_added` for the icon catalog: the feeds read only its load-once icon column;
    // its model-cache half churns every frame (the containers gate's own note).
    let icons_added = icons.as_ref().is_some_and(|r| r.is_added());
    // Both DBC catalogs load once, at startup — one input covers the pair.
    let enchants_changed = enchants.as_ref().is_some_and(|r| r.is_changed())
        || props.as_ref().is_some_and(|r| r.is_changed());
    let pending_held = !pending.is_empty();
    gate::trace(
        "feed_char",
        &[
            ("vm_reset", vm_reset),
            ("objects", objects_moved),
            ("templates", templates_moved),
            ("names", names_moved),
            ("deadlines", deadlines_moved),
            ("self", self_changed),
            ("icons", icons_added),
            ("enchants", enchants_changed),
            ("pending", pending_held),
        ],
    );
    let gate = gate::Gate::new(
        vm_reset
            || objects_moved
            || templates_moved
            || names_moved
            || deadlines_moved
            || self_changed
            || icons_added
            || enchants_changed
            || pending_held,
    );
    if gate.skip() {
        return;
    }

    let stats = combat_stats(store, &mut items, &commands);
    if memo.last_stats.as_ref() != Some(&stats) {
        gate.audit("feed_char", "the combat-stats snapshot");
        // PUSH before firing: event dispatch runs the Lua handlers synchronously, so the snapshot
        // must already be in the VM when they repaint (the ui_unit rule — a fire-first ordering
        // paints the OLD values and, being transition-gated, never corrects itself).
        let prev = memo.last_stats.take();
        script.set_player_combat_stats(Some(stats.clone()));
        fire_stat_transitions(&mut script, "player", prev.as_ref(), &stats);
        memo.last_stats = Some(stats);
    }

    let inv = inventory_slots(
        store,
        &mut items,
        icons.as_deref(),
        crate::items::RollCatalogs {
            enchants: enchants.as_deref(),
            props: props.as_deref(),
        },
        &commands,
        &pending,
        &mut names,
    );
    let bank_bags = bank_bag_slots(
        store,
        &mut items,
        icons.as_deref(),
        crate::items::RollCatalogs {
            enchants: enchants.as_deref(),
            props: props.as_deref(),
        },
        &commands,
        &pending,
        &mut names,
    );
    // One transition for both bands: they are the same descriptor read at two offsets, and
    // `UNIT_INVENTORY_CHANGED` is the one repaint signal either has. Pushing them separately
    // would fire it twice for a single wire update.
    if memo.last_inv.as_ref() != Some(&inv) || memo.last_bank_bags.as_ref() != Some(&bank_bags) {
        gate.audit("feed_char", "the inventory snapshot");
        script.set_inventory_slots(inv.clone());
        script.set_bank_bag_slots(bank_bags.clone());
        script.fire_event(
            "UNIT_INVENTORY_CHANGED",
            vec![ScriptValue::Str("player".to_string())],
        );
        memo.last_inv = Some(inv);
        memo.last_bank_bags = Some(bank_bags);
    }
}

/// The client's own 0-based `EQUIPMENT_SLOT_*` ids for the two weapon slots — the numbers
/// `GetWeaponEnchantInfo 0x4c9790` hands its container lookup (`push 0xf` at `0x4c97c3`,
/// `push 0x10` at `0x4c988b`). The live-API `GetInventorySlotInfo` ids the *frames* use are one
/// higher (16 `MainHandSlot`, 17 `SecondaryHandSlot`).
const EQUIPMENT_SLOT_MAINHAND: u8 = 15;
const EQUIPMENT_SLOT_OFFHAND: u8 = 16;

/// `ITEM_FIELD_ENCHANTMENT`'s temporary slot (0 = permanent, 1 = temporary, 2..6 random property)
/// — the slot index the reference passes to its remaining-time reader (`push 0x1`, `0x4c9828`).
const TEMP_ENCHANTMENT_SLOT: u8 = 1;

/// One weapon's temporary enchantment for [`benilla_ui::script::WeaponEnchant`], read off the
/// equipped item object.
///
/// **The RAW enchantment triple, not [`InvSlotView::enchants`]**: that view is tooltip-shaped and
/// drops both an id the `SpellItemEnchantment` catalog cannot name and the whole `Flags & 0x2`
/// print-no-line family (decision 0928 — the totem weapon imbues), which is exactly the set the
/// enchant row exists to show. The binary's gate is `[descriptor+0x4c] != 0`, the id and nothing
/// else, and `item_enchant` already answers `None` for a zero id.
fn weapon_enchant(
    store: &ObjectStore,
    items: &Items,
    slot0: u8,
) -> Option<benilla_ui::script::WeaponEnchant> {
    let guid = store.0.player_inv_slot(slot0)?;
    // Before the object borrow — the deadlines and the objects both live on `Items`. The
    // *deadline* read, not the tooltip's: an elapsed timer must answer 0, not "no timer".
    let remaining_ms = items.enchant_deadline_ms(guid, u32::from(TEMP_ENCHANTMENT_SLOT));
    let obj = items.object(guid)?;
    obj.item_enchant(TEMP_ENCHANTMENT_SLOT)?;
    Some(benilla_ui::script::WeaponEnchant {
        remaining_ms,
        charges: obj.item_enchant_charges(TEMP_ENCHANTMENT_SLOT),
    })
}
