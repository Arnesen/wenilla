//! The inward half of the container seam (see the parent module doc): the per-frame push from the
//! self player's descriptor into the VM's `ContainerState`/`ContainerSlot` snapshots, plus the
//! shared item-tooltip stat feed the bags/paper-doll/etc. all read from.

use std::collections::HashMap;

use bevy::prelude::*;

use benilla_protocol::ItemInfo;
use benilla_ui::script::{ContainerSlot, ContainerState, ScriptValue, UiScript};

use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::net::{NetCommands, ObjectStore, SelfPlayer};
use crate::pending_item_ops::{LockClearedByFailure, PendingItemOps};

use super::equip_error::equip_error_key;
use super::{
    find_equip_slot, has_key, item_link, keyring_size, slot_guid_count, EquipErrors, BAGS,
    BAG_SLOT_FIRST, BANK_BAGS, BANK_BAG_ID_FIRST, BANK_BAG_SLOT_FIRST, BANK_CONTAINER, BANK_SLOTS,
    KEYRING_CONTAINER, KEYRING_SLOTS, PACK_SLOTS,
};

/// The feed's memory of what it last pushed, for per-bag change events. No cooldown-churn gate:
/// the pushed triple carries the ABSOLUTE start, which is frame-stable for a running cooldown.
#[derive(Default)]
pub(super) struct FeedMemory {
    pushed: HashMap<i64, ContainerState>,
    /// The last `HasKey()` pushed — kept only so the transition can be logged once instead of
    /// every frame. It is the gate the entire keyring UI hangs off, and "the keyring never
    /// appeared" is otherwise indistinguishable from "the button is mis-anchored".
    had_key: bool,
}

/// A spell's tooltip text: the $-substituted description (the real "Use: Restores 392 to 653
/// health over 21 sec" shape). Shared by the item trigger lines and the set-bonus lines.
///
/// **An empty description yields `None`, and the caller drops the whole line** — byte-verified at
/// the item builder `0x52d8a0`: it runs the $-expander into a buffer (`0x52da24` → `0x5075f0`),
/// tests the FIRST BYTE of the result (`0x52da29 mov al,[ebp-0x4f0]` / `0x52da2f test al,al`) and
/// on zero **jumps clean past the entire trigger block** (`0x52da31 je 0x52dd3d`) — past the
/// ONUSE/ONEQUIP/ONPROC prefix select and the `"%s %s"` join alike. No prefix, no line.
///
/// This used to fall back to the spell's bare NAME, which is where "Use: Opening" on a dungeon key
/// came from (director-reported): `Opening` (3365/3366/6247/6477) carries no description in
/// Spell.dbc, so the real client prints nothing and we printed the name. The fallback was an
/// invention, not a transcription — no such branch exists in the builder.
fn spell_desc_text(
    spells: Option<&crate::ui_action::Spells>,
    id: u32,
    home_area: Option<&str>,
) -> Option<String> {
    let sp = spells?;
    let d = sp.catalog.get(id)?;
    match d.description.as_deref().filter(|t| !t.is_empty()) {
        // The $-expander can still yield an empty string from a non-empty template; the builder's
        // test is on the EXPANDED text, so ours is too.
        Some(desc) => {
            let ctx = benilla_formats::TokenContext {
                durations: &sp.durations,
                radii: &sp.radii,
                lookup: &|i| sp.catalog.get(i),
                home_area,
            };
            Some(benilla_formats::substitute(desc, d, &ctx)).filter(|t| !t.is_empty())
        }
        None => None,
    }
}

/// The tooltip's "N Charge(s)" count — 0 = no line. The real builder's charge gate
/// (byte-VERIFIED, tooltip builder `0x52d8a0`): per spell slot it normalizes a template charge
/// value of 0 to the `-1` sentinel (`0x52da01-0x52da0d`), skips the ITEM_SPELL_CHARGES line
/// entirely when the resolved value is `-1` (`0x52db51-54`), and otherwise prints `abs(value)`
/// (the cdq/xor/sub at `0x52db56-5b`). So a consumable's `-1` — "the item IS the last charge":
/// food, water, potions — shows NO line, while a real charge pool (a wand-style `-5`) shows
/// "5 Charges". We emit one line for the first surviving slot — the builder prints per slot,
/// but no item in the data carries two charge pools (VERIFIED against the live vmangos
/// `item_template`: 111 rows with a pool, none with a second).
fn charges_count(spells: &[benilla_protocol::messages::ItemSpellEntry]) -> i32 {
    spells
        .iter()
        .find(|s| s.spell_id != 0 && s.charges != 0 && s.charges != -1)
        .map(|s| s.charges.abs())
        .unwrap_or(0)
}

/// A reputation rank (0..=7) → its enUS standing label (GlobalStrings
/// `FACTION_STANDING_LABEL1..8`) — the "Requires <Faction> - <Standing>" tail.
fn standing_label(rank: u32) -> &'static str {
    [
        "Hated",
        "Hostile",
        "Unfriendly",
        "Neutral",
        "Friendly",
        "Honored",
        "Revered",
        "Exalted",
    ][rank.min(7) as usize]
}

/// Build an item template's tooltip view (decision 0274 P1): the full wire fields plus the
/// display strings only the app can resolve — trigger-spell TEXT off the spell catalog (the
/// spell's $-substituted description when it has one, per the verified law's item `$`-expander
/// `0x506f70`; its bare name otherwise), the skill requirement's name off `SkillLine.dbc`, the
/// reputation requirement off `Faction.dbc` names (the red check is the engine's, against the
/// player's rank map).
fn template_view(
    t: &ItemInfo,
    spells: Option<&crate::ui_action::Spells>,
    skill_lines: Option<&benilla_formats::SkillLineCatalog>,
    home_area: Option<&str>,
    factions: Option<&benilla_formats::FactionCatalog>,
    sub_classes: Option<&benilla_formats::ItemSubClassCatalog>,
) -> benilla_ui::script::ItemTemplateView {
    let spell_name = |id: u32| -> Option<String> {
        spells
            .and_then(|s| s.catalog.get(id))
            .map(|sd| sd.name.clone())
    };
    let spell_text = |id: u32| spell_desc_text(spells, id, home_area);
    benilla_ui::script::ItemTemplateView {
        name: t.name.clone(),
        quality: t.quality,
        class: t.class,
        subclass: t.subclass,
        inventory_type: t.inventory_type,
        proficiency_alt: sub_classes.and_then(|c| c.proficiency_alt(t.class, t.subclass)),
        hide_subclass: sub_classes.is_some_and(|c| c.hides_name(t.class, t.subclass)),
        flags: t.flags,
        bonding: t.bonding,
        max_count: t.max_count,
        start_quest: t.start_quest,
        container_slots: t.container_slots,
        stats: t.stats.clone(),
        damages: t.damages.iter().map(|d| (d.min, d.max, d.school)).collect(),
        delay_ms: t.delay_ms,
        armor: t.armor,
        block: t.block,
        resistances: t.resistances,
        max_durability: t.max_durability,
        required_level: t.required_level,
        allowable_class: t.allowable_class,
        allowable_race: t.allowable_race,
        required_skill: t.required_skill,
        required_skill_rank: t.required_skill_rank,
        required_skill_name: (t.required_skill != 0)
            .then(|| {
                skill_lines
                    .and_then(|sl| sl.line(t.required_skill))
                    .map(|l| l.name.clone())
            })
            .flatten(),
        required_spell: t.required_spell,
        required_spell_name: (t.required_spell != 0)
            .then(|| spell_name(t.required_spell))
            .flatten(),
        required_honor_rank: t.required_honor_rank,
        required_city_rank: t.required_city_rank,
        required_rep_line: (t.required_rep_faction != 0)
            .then(|| {
                factions
                    .and_then(|c| c.faction_name(t.required_rep_faction))
                    .map(|f| format!("Requires {f} - {}", standing_label(t.required_rep_rank)))
            })
            .flatten(),
        required_rep_faction: t.required_rep_faction,
        required_rep_rank: t.required_rep_rank,
        lock_id: t.lock_id,
        spell_triggers: t
            .spells
            .iter()
            .filter(|s| s.spell_id != 0)
            .filter_map(|s| spell_text(s.spell_id).map(|n| (s.trigger, s.spell_id, n)))
            .collect(),
        charges: charges_count(&t.spells),
        description: t.description.clone(),
        page_text: t.page_text,
        sell_price: t.sell_price,
        item_set: t.item_set,
        random_property: t.random_property,
    }
}

/// The §22 SET-block feed: answer the engine's ask-once set ids from ItemSet.dbc, joining
/// member NAMES from the template cache (each miss fires its own `CMSG_ITEM_QUERY` through
/// [`Items::template`], like the real client querying set members) and bonus TEXT from the
/// spell catalog's `$`-engine. A set with members still in flight stays pending and re-pushes
/// as answers land; it leaves the pending map once every member resolved. (`TokenContext.home_
/// area` is `None` here: set bonuses are equip auras — no `$z`; the passthrough would surface
/// one if the data ever carried it.)
pub(super) fn feed_item_sets(
    script: Option<NonSendMut<UiScript>>,
    sets: Option<Res<super::ItemSets>>,
    mut items: ResMut<Items>,
    commands: Res<NetCommands>,
    spells: Option<Res<crate::ui_action::Spells>>,
    skill_lines: Option<Res<crate::ui_spellbook::SkillLines>>,
    mut pending: Local<std::collections::HashMap<u32, benilla_ui::script::ItemSetView>>,
) {
    let Some(mut script) = script else {
        return;
    };
    for id in script.take_item_set_asks() {
        pending.entry(id).or_default();
    }
    if pending.is_empty() {
        return;
    }
    let Some(sets) = sets.as_deref() else {
        return; // catalog absent (no client data): the asks stay parked, the block stays off
    };
    let spell_res = spells.as_deref();
    let skill_catalog = skill_lines.as_deref().map(|s| &s.catalog);
    let mut done: Vec<u32> = Vec::new();
    let mut push: Vec<(u32, benilla_ui::script::ItemSetView)> = Vec::new();
    for (&set_id, last) in pending.iter_mut() {
        let Some(row) = sets.0.set(set_id) else {
            done.push(set_id); // no such row — drop the ask for good
            continue;
        };
        let members: Vec<(u32, Option<String>)> = row
            .items
            .iter()
            .map(|&id| (id, items.template(id, 0, &commands).map(|t| t.name.clone())))
            .collect();
        let view = benilla_ui::script::ItemSetView {
            name: row.name.clone(),
            bonuses: row
                .bonuses
                .iter()
                .filter_map(|&(n, spell)| {
                    spell_desc_text(spell_res, spell, None).map(|text| (n, text))
                })
                .collect(),
            required_skill: row.required_skill,
            required_skill_rank: row.required_skill_rank,
            required_skill_name: (row.required_skill != 0)
                .then(|| {
                    skill_catalog
                        .and_then(|sl| sl.line(row.required_skill))
                        .map(|l| l.name.clone())
                })
                .flatten(),
            members,
        };
        if view.members.iter().all(|(_, n)| n.is_some()) {
            done.push(set_id);
        }
        if *last != view {
            *last = view.clone();
            push.push((set_id, view));
        }
    }
    for (id, view) in push {
        script.set_item_set(id, view);
    }
    for id in done {
        pending.remove(&id);
    }
}

/// The shared item-tooltip feed, both halves. **Push**: every template that lands in the app
/// cache goes to the UI store unprompted (`Items::take_fresh`) — so by the time an item's name is
/// on screen its tooltip stats are already there, and the first hover never misses (the real
/// client reads one item cache synchronously; a store the UI only fills on a read miss re-created
/// the "hover twice" flake this replaces). **Ask**: a renderer read of an id the app never
/// resolved still records a miss, which triggers the `CMSG_ITEM_QUERY` here and lands via the
/// same push when the answer arrives.
#[allow(clippy::too_many_arguments)]
pub(super) fn feed_item_stats(
    script: Option<NonSendMut<UiScript>>,
    mut items: ResMut<Items>,
    commands: Res<NetCommands>,
    spells: Option<Res<crate::ui_action::Spells>>,
    skill_lines: Option<Res<crate::ui_spellbook::SkillLines>>,
    // The $z token's inputs: the bind-point area id (SMSG_BINDPOINTUPDATE) named through the
    // AreaTable catalog the quest-log/zone-text already carry.
    home_bind: Option<Res<crate::net::HomeBind>>,
    area_names: Option<Res<crate::ui_quest_log::QuestHeaderNamesRes>>,
    factions: Option<Res<crate::target::Factions>>,
    sub_classes: Option<Res<super::ItemSubClasses>>,
    mut pending: Local<std::collections::HashSet<u32>>,
    mut last_home: Local<Option<String>>,
) {
    let Some(mut script) = script else {
        return;
    };
    pending.extend(items.take_fresh());
    pending.extend(script.take_item_stat_asks());
    if pending.is_empty() {
        return;
    }
    let spell_res = spells.as_deref();
    let skill_catalog = skill_lines.as_deref().map(|s| &s.catalog);
    let home_area: Option<String> = home_bind
        .as_deref()
        .and_then(|b| b.0)
        .and_then(|id| area_names.as_deref()?.0.resolve(id as i32))
        .map(str::to_string);
    // A bind-point change re-substitutes every held view: templates pushed before the login's
    // SMSG_BINDPOINTUPDATE landed carry a raw $z otherwise (the hearthstone's login race).
    if *last_home != home_area {
        *last_home = home_area.clone();
        pending.extend(items.cached_template_ids());
    }
    let ready: Vec<u32> = pending
        .iter()
        .copied()
        .filter(|&id| items.template(id, 0, &commands).is_some())
        .collect();
    for id in ready {
        pending.remove(&id);
        let Some(t) = items.template(id, 0, &commands) else {
            continue;
        };
        let view = template_view(
            &t.clone(),
            spell_res,
            skill_catalog,
            home_area.as_deref(),
            factions.as_deref().map(|f| f.catalog()),
            sub_classes.as_deref().map(|s| &s.0),
        );
        script.set_item_template(id, view);
    }
}

/// The red-line law's player state (decision 0274 P1): level + class/race ids (the allowable-mask
/// bits) + the full skill-rank map, read off the self player's descriptor, plus the equip
/// proficiencies (`SMSG_SET_PROFICIENCY`) and the faction → reputation-rank map (DBC base for our
/// race/class + the `SMSG_INITIALIZE_FACTIONS` standing, ranked) — pushed on change.
#[allow(clippy::too_many_arguments)] // a Bevy system's full input set
pub(super) fn feed_player_req(
    script: Option<NonSendMut<UiScript>>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    proficiencies: Res<crate::net::Proficiencies>,
    reputations: Res<crate::net::Reputations>,
    factions: Option<Res<crate::target::Factions>>,
    actions: Res<crate::ui_action::PlayerActions>,
    spells: Option<Res<crate::ui_action::Spells>>,
    mut last: Local<Option<benilla_ui::script::PlayerReqState>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let Some(store) = self_q.iter().next() else {
        return;
    };
    let mut skills = std::collections::HashMap::new();
    for slot in 0..benilla_protocol::messages::PLAYER_SKILL_SLOTS {
        let Some(s) = store.0.player_skill(slot) else {
            continue;
        };
        if s.skill_id != 0 {
            // The gate laws (`0x5eaae0`, `0x5ea930`) read value + PERM bonus (the talent half;
            // the temp half never counts toward requirements). The client zero-extends the bonus
            // word before a signed compare — a negative perm bonus can't happen in live data, so
            // the sane saturating add mirrors every reachable case.
            skills.insert(
                u32::from(s.skill_id),
                u32::from(s.value).saturating_add_signed(i32::from(s.perm_bonus)),
            );
        }
    }
    let race = store.0.unit_race().unwrap_or(0);
    let class = store.0.unit_class().unwrap_or(0);
    let mut rep_ranks = std::collections::HashMap::new();
    if let Some(cat) = factions.as_deref().map(|f| f.catalog()) {
        for (id, info) in cat.reputation_factions() {
            let standing = usize::try_from(info.rep_index)
                .ok()
                .and_then(|i| reputations.0.get(i))
                .map(|&(_, s)| s)
                .unwrap_or(0);
            rep_ranks.insert(
                id,
                benilla_formats::reputation_rank(info.base_for(race, class) + standing),
            );
        }
    }
    // The client's `0xc4d770`: set when a spell whose Effect[0] is 40 (SPELL_EFFECT_DUAL_WIELD)
    // is learned, cleared on its unlearn — mirrored as a spellbook scan.
    let can_dual_wield = spells.as_deref().is_some_and(|s| {
        actions
            .spells
            .iter()
            .any(|&id| s.catalog.get(id).is_some_and(|sd| sd.effects[0] == 40))
    });
    let state = benilla_ui::script::PlayerReqState {
        level: store.0.unit_level().unwrap_or(0),
        class_id: u32::from(class),
        race_id: u32::from(race),
        skills,
        proficiency: proficiencies.0.clone(),
        rep_ranks,
        can_dual_wield,
        honor_rank: store.0.player_honor_rank().unwrap_or(0),
    };
    if last.as_ref() != Some(&state) {
        *last = Some(state.clone());
        script.set_player_req_state(state);
    }
}

/// One bag slot from its item guid: the instance (store) + template (cache, ask-once) + icon
/// (DBC) + the use-spell's running cooldown (`GetContainerItemCooldown`'s data — the same store
/// read the action feed's ITEM arm does). `None` = the slot is empty (guid 0/unsent) — an
/// *unresolved* occupied slot is `Some` with empty fields instead, so the bag shows the item
/// exists before its query answers.
#[allow(clippy::too_many_arguments)] // the slot resolve's full read set (stores + both clocks)
fn resolve_slot(
    guid: u64,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    enchants: Option<&crate::items::Enchants>,
    commands: &NetCommands,
    cooldowns: &crate::cooldowns::Cooldowns,
    spells: Option<&benilla_formats::SpellCatalog>,
    names: &mut crate::names::NameCache,
    now: std::time::Instant,
    ui_now: f64,
) -> Option<ContainerSlot> {
    if guid == 0 {
        return None;
    }
    // The temporary-enchant countdowns, read off the deadline store BEFORE the object borrow below
    // (both live on `Items`): one `Option<ms>` per enchant slot.
    let enchant_ms: [Option<u64>; 7] =
        std::array::from_fn(|s| items.enchant_remaining_ms(guid, s as u32));
    let (entry, count, durability, readable, creator, flags, enchant_lines) =
        match items.object(guid) {
            Some(fields) => (
                fields.object_entry().unwrap_or(0),
                fields.item_stack_count().unwrap_or(1),
                // The live instance pair — the wire updates ITEM_FIELD_DURABILITY on damage/repair
                // (death 10%, spirit healer 25%); max 0 = indestructible ⇒ no line.
                fields
                    .item_durability()
                    .zip(fields.item_max_durability())
                    .filter(|&(_, max)| max > 0),
                // The instance carries letter text (a mail permanent copy) — the same gate the
                // right-click reader fork uses (`ui_items::drain`); the hover magnifier keys off it.
                // Template-`PageText` books join when the books read path lands (decision 0572).
                fields.item_text_id().is_some_and(|id| id != 0),
                // `ITEM_FIELD_CREATOR` → the ask-once name cache: the tooltip's "Written by %s" /
                // "<Made by %s>" line. `None` while the query is in flight — the changed re-push
                // repaints the hover when the answer lands (the ref's cache-callback shape).
                fields
                    .item_creator()
                    .filter(|&g| g != 0)
                    .and_then(|g| names.resolve(g, commands).map(str::to_string)),
                // `ITEM_FIELD_FLAGS` — the tooltip's UNLOCKED (0x4) / WRAPPED (0x8) sub-gates.
                fields.item_flags().unwrap_or(0),
                // The instance's own 7 enchant slots — the tooltip's enchant lines (0915/0920). An
                // item we hold streams as an OBJECT, so all seven are here, with their charges and
                // their `SMSG_ITEM_ENCHANT_TIME_UPDATE` countdowns; the wire's 2-slot broadcast is
                // what everyone ELSE's items are limited to.
                crate::items::enchant_lines(
                    (0..7).map(|s| {
                        (
                            s,
                            fields.item_enchant(s).unwrap_or(0),
                            fields.item_enchant_charges(s),
                            enchant_ms[usize::from(s)],
                        )
                    }),
                    enchants,
                ),
            ),
            // The player descriptor references a guid whose create hasn't landed (yet) —
            // occupied, unresolved.
            None => return Some(ContainerSlot::default()),
        };
    if entry == 0 {
        return Some(ContainerSlot::default());
    }
    let template: Option<ItemInfo> = items.template(entry, guid, commands).cloned();
    let Some(t) = template else {
        // Asked (or a cached negative); show the slot occupied while the answer is in flight.
        return Some(ContainerSlot {
            durability: None,
            item_id: entry,
            count,
            readable,
            creator,
            flags,
            enchants: enchant_lines,
            ..Default::default()
        });
    };
    Some(ContainerSlot {
        texture: icons
            .and_then(|i| i.catalog.get(t.display_info_id))
            .and_then(|d| d.icon.clone()),
        count,
        durability,
        quality: Some(t.quality),
        item_id: entry,
        link: Some(item_link(entry, &t.name, t.quality)),
        locked: false,
        readable,
        creator,
        flags,
        enchants: enchant_lines,
        equip_slots: find_equip_slot(t.inventory_type),
        bar_placeable: t.placeable_on_action_bar(),
        cooldown: t.use_spell.and_then(|u| {
            let sd = spells.and_then(|s| s.get(u.spell_id));
            cooldowns
                .info(u.spell_id, entry, sd, now)
                .ui_triple(now, ui_now)
        }),
    })
}

/// Reason 16's `%s`: the destination bag's **`BagFamily` name** — "Arrows", "Soul Shards",
/// "Herbs" — for *"Only %s can be placed in that."* `None` = no bag to name, so the caller keeps
/// the generic `ERR_WRONG_BAG_TYPE` line.
///
/// `bag_slot` is the wire's ABSOLUTE player slot (see
/// `benilla_protocol::messages::items::read_inventory_change_failure`), and the reference's helper
/// `0x5ede00` bails on exactly two shapes we mirror: `slot == 0xFF` (`INVENTORY_SLOT_BAG_0`, the
/// player's own array — a backpack/equipment refusal names no container), and a slot past the
/// player's slot array. Everything else it indexes straight into that array and resolves as an
/// item.
///
/// Despite the errorId's `_SUBCLASS` name this reads the bag's `BagFamily`, **not** its
/// ItemSubClass — which is what makes a quiver say "Only Arrows can be placed in that." rather
/// than naming the quiver's own type (`benilla_formats::itembagfamily`, and the DBC read there).
///
/// The bank-bag leg (63..=68) is ours by symmetry rather than byte-pinned: the reference bounds
/// this on `[player+0x1d38]`, whose value wow-re did not resolve, so whether a bank bag reaches
/// the substitution or falls to the generic line is unverified. Both outcomes are ordinary
/// sentences; resolving it is the strictly more useful one, and it is flagged here rather than
/// silently assumed.
fn bag_family_name(
    player: Option<&ObjectStore>,
    bag_slot: u8,
    items: &mut Items,
    families: Option<&benilla_formats::ItemBagFamilyCatalog>,
    commands: &NetCommands,
) -> Option<String> {
    let store = player?;
    let families = families?;
    let guid = match bag_slot {
        // The equipped bag slots.
        s if (BAG_SLOT_FIRST..BAG_SLOT_FIRST + BAGS).contains(&s) => store.0.player_inv_slot(s),
        // The purchasable bank bag slots.
        s if (BANK_BAG_SLOT_FIRST..BANK_BAG_SLOT_FIRST + BANK_BAGS).contains(&s) => {
            store.0.player_bank_bag_slot(s - BANK_BAG_SLOT_FIRST)
        }
        // 255 = the player's own array, and anything past the bag slots: no container to name.
        _ => None,
    }
    .filter(|&g| g != 0)?;
    let entry = items.object(guid)?.object_entry().filter(|&e| e != 0)?;
    let family = items.template(entry, guid, commands)?.bag_family;
    families.name(family).map(str::to_string)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn feed_containers(
    script: Option<NonSendMut<UiScript>>,
    mut items: ResMut<Items>,
    icons: Option<Res<ItemDisplays>>,
    // `SpellItemEnchantment`'s name column — the tooltip's enchant lines (decision 0915).
    enchants: Option<Res<crate::items::Enchants>>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    commands: Res<NetCommands>,
    cooldowns: Res<crate::cooldowns::Cooldowns>,
    spells: Option<Res<crate::ui_action::Spells>>,
    mut equip_errors: ResMut<EquipErrors>,
    // Reason 16's `%s` source (decision 0916); absent = every 16 keeps the generic line.
    bag_families: Option<Res<crate::ui_items::ItemBagFamilies>>,
    mut error_lines: ResMut<crate::ui_items::UiErrorLines>,
    mut pending: ResMut<PendingItemOps>,
    mut lock_cleared: ResMut<LockClearedByFailure>,
    mut names: ResMut<crate::names::NameCache>,
    clock: Res<crate::ui_script::UiClock>,
    mut memory: Local<FeedMemory>,
) {
    let Some(mut script) = script else {
        return;
    };
    // The frame's atomic clock pair (`crate::ui_script::UiClock`): the slot resolves read the
    // cooldown store and convert triples through ONE coherent instant, so a running cooldown's
    // pushed start is frame-stable (the resource's own doc).
    let (now, ui_now) = (clock.anchor, clock.ui_now);
    let spell_catalog = spells.as_deref().map(|s| &s.catalog);
    let enchant_rows = enchants.as_deref();
    // Inventory refusals surface as the client's red error line (the cast path's exact shape):
    // the wire code keys into the VM's own GlobalStrings ([`equip_error_key`], total), reason 1
    // filling its `%d` with the packet's required level.
    //
    // **An unresolvable or empty key prints NOTHING** — this is the reference's own behaviour,
    // not a fallback: `CGGameUI::DisplayError` is called unconditionally and the sink's
    // `cmp byte [ecx],0` guard (`0x4945b4`) drops an empty string before it renders or sounds.
    // It is what silences reason 59, the lock-clear sentinel that rides alongside a real refusal
    // and used to print a second, hex-debug line over it (B198) — and it is the same law
    // `ui_action::cast_fail` already runs. No hex debug line on the player's screen: a code we
    // failed to map can't reach here (the table is total), and a key we typo'd is caught by
    // `equip_error`'s resolution test against the real `GlobalStrings.lua`, not at runtime.
    let player = self_q.iter().next();
    for e in equip_errors.0.drain(..) {
        // Reason 16's substitution — the ONE reason whose text is chosen by the app rather than
        // by the table, because the choice needs the named bag (decision 0916). The reference's
        // helper `0x5ede00(player, bagSlot)` resolves the bag and calls
        // `DisplayError(ERR_WRONG_BAG_TYPE_SUBCLASS, familyName)` *itself*, returning 1 so the
        // caller skips the generic line; a bag that doesn't resolve leaves the generic
        // `ERR_WRONG_BAG_TYPE` standing. Same fork, same fallback.
        let subclass_fill = (e.reason == 16)
            .then(|| {
                bag_family_name(
                    player,
                    e.bag_slot,
                    &mut items,
                    bag_families.as_deref().map(|c| &c.0),
                    &commands,
                )
            })
            .flatten();
        let key = match subclass_fill {
            Some(_) => "ERR_WRONG_BAG_TYPE_SUBCLASS",
            None => equip_error_key(e.reason),
        };
        let text = script
            .lua()
            .globals()
            .get::<String>(key)
            .unwrap_or_default();
        if text.is_empty() {
            continue;
        }
        // The two argument-taking reasons, each filling its own specifier. Neither code ever
        // carries the other's fill, so the order is bookkeeping, not precedence.
        let text = match e.required_level {
            Some(d) => text.replace("%d", &d.to_string()),
            None => text,
        };
        let text = match subclass_fill {
            Some(family) => text.replace("%s", &family),
            None => text,
        };
        script.fire_event("UI_ERROR_MESSAGE", vec![ScriptValue::Str(text)]);
    }
    for line in error_lines.0.drain(..) {
        script.fire_event("UI_ERROR_MESSAGE", vec![ScriptValue::Str(line)]);
    }

    // The pending-lock resolving clear (decision 0216 §4 / 0218 §3 "the field-update watcher"):
    // walk the SAME guids the slot loop below is about to read and release any op whose slots have
    // moved on. Folded in with the failure-driven clears `net/apply/loot.rs::inventory_failure`
    // queued (it has no `UiScript` to fire through) — both fire `ITEM_LOCK_CHANGED` here, the bag
    // windows' own repaint trigger (0218: the popup's No/ESC clear paths the bag never clicked
    // through, so only the event reaches it). Fired AFTER the slot loop below pushes the corrected
    // `.locked` state, so the repaint they trigger sees the unlocked slot, not stale data.
    let mut transitioned: Vec<(i64, u32)> = if player.is_some() {
        pending.resolve(|bag, slot1| slot_guid_count(player, bag, slot1, &items))
    } else {
        Vec::new()
    };
    transitioned.append(&mut lock_cleared.0);

    let mut fresh: HashMap<i64, ContainerState> = HashMap::new();
    if let Some(store) = player {
        // Bag 0: the backpack — its slots live directly in the player descriptor.
        let mut slots = HashMap::new();
        for i in 0..PACK_SLOTS {
            let guid = store.0.player_pack_slot(i).unwrap_or(0);
            if let Some(mut slot) = resolve_slot(
                guid,
                &mut items,
                icons.as_deref(),
                enchant_rows,
                &commands,
                &cooldowns,
                spell_catalog,
                &mut names,
                now,
                ui_now,
            ) {
                slot.locked = pending.contains(0, u32::from(i) + 1);
                slots.insert(u32::from(i) + 1, slot);
            }
        }
        fresh.insert(
            0,
            ContainerState {
                name: Some("Backpack".into()),
                num_slots: u32::from(PACK_SLOTS),
                slots,
            },
        );

        // Bags 1..4: each INV bag slot holds a container object with its own slot array.
        for bag in 1..=BAGS {
            let bag_guid = store
                .0
                .player_inv_slot(BAG_SLOT_FIRST + bag - 1)
                .unwrap_or(0);
            if bag_guid == 0 {
                continue; // no bag equipped → absent → GetContainerNumSlots = 0
            }
            let (entry, num_slots, slot_guids) = match items.object(bag_guid) {
                Some(f) => {
                    let n = f.container_num_slots().unwrap_or(0);
                    let guids: Vec<u64> = (0..n.min(36) as u8)
                        .map(|j| f.container_slot(j).unwrap_or(0))
                        .collect();
                    (f.object_entry().unwrap_or(0), n, guids)
                }
                None => (0, 0, Vec::new()),
            };
            let name = (entry != 0)
                .then(|| {
                    items
                        .template(entry, bag_guid, &commands)
                        .map(|t| t.name.clone())
                })
                .flatten();
            let mut slots = HashMap::new();
            for (j, &guid) in slot_guids.iter().enumerate() {
                if let Some(mut slot) = resolve_slot(
                    guid,
                    &mut items,
                    icons.as_deref(),
                    enchant_rows,
                    &commands,
                    &cooldowns,
                    spell_catalog,
                    &mut names,
                    now,
                    ui_now,
                ) {
                    slot.locked = pending.contains(i64::from(bag), j as u32 + 1);
                    slots.insert(j as u32 + 1, slot);
                }
            }
            fresh.insert(
                i64::from(bag),
                ContainerState {
                    name,
                    num_slots,
                    slots,
                },
            );
        }

        // The bank (decision 0604): container −1 = the 24 generic vault slots straight off the
        // player descriptor, containers 5..=10 = the bank bags — each a container object exactly
        // like an equipped bag. Fed unconditionally like the backpack: the descriptor streams at
        // login, the window (BANKFRAME_OPENED) is a UI concern, not a data one.
        let mut slots = HashMap::new();
        for i in 0..BANK_SLOTS {
            let guid = store.0.player_bank_slot(i).unwrap_or(0);
            if let Some(mut slot) = resolve_slot(
                guid,
                &mut items,
                icons.as_deref(),
                enchant_rows,
                &commands,
                &cooldowns,
                spell_catalog,
                &mut names,
                now,
                ui_now,
            ) {
                slot.locked = pending.contains(BANK_CONTAINER, u32::from(i) + 1);
                slots.insert(u32::from(i) + 1, slot);
            }
        }
        fresh.insert(
            BANK_CONTAINER,
            ContainerState {
                name: Some("Bank".into()),
                num_slots: u32::from(BANK_SLOTS),
                slots,
            },
        );
        for bank_bag in 0..BANK_BAGS {
            let bag_id = BANK_BAG_ID_FIRST + i64::from(bank_bag);
            let bag_guid = store.0.player_bank_bag_slot(bank_bag).unwrap_or(0);
            if bag_guid == 0 {
                continue; // no bag in the slot → absent → GetContainerNumSlots = 0
            }
            let (entry, num_slots, slot_guids) = match items.object(bag_guid) {
                Some(f) => {
                    let n = f.container_num_slots().unwrap_or(0);
                    let guids: Vec<u64> = (0..n.min(36) as u8)
                        .map(|j| f.container_slot(j).unwrap_or(0))
                        .collect();
                    (f.object_entry().unwrap_or(0), n, guids)
                }
                None => (0, 0, Vec::new()),
            };
            let name = (entry != 0)
                .then(|| {
                    items
                        .template(entry, bag_guid, &commands)
                        .map(|t| t.name.clone())
                })
                .flatten();
            let mut slots = HashMap::new();
            for (j, &guid) in slot_guids.iter().enumerate() {
                if let Some(mut slot) = resolve_slot(
                    guid,
                    &mut items,
                    icons.as_deref(),
                    enchant_rows,
                    &commands,
                    &cooldowns,
                    spell_catalog,
                    &mut names,
                    now,
                    ui_now,
                ) {
                    slot.locked = pending.contains(bag_id, j as u32 + 1);
                    slots.insert(j as u32 + 1, slot);
                }
            }
            fresh.insert(
                bag_id,
                ContainerState {
                    name,
                    num_slots,
                    slots,
                },
            );
        }

        // The keyring (decision 0765): container −2, the player array's slots 81.., no container
        // object of its own — structurally the bank's twin. Its capacity is NOT a wire field: both
        // the reference (`GetKeyRingSize`) and the server (`GetMaxKeyringSize`) derive it from the
        // player's level with the same ladder, so [`keyring_size`] computes it here and Lua's
        // `GetKeyRingSize()` reads it back off this snapshot. Slots past that count are never fed
        // (they can't hold anything — the server refuses to store there), so the window's own
        // physIndex-past-size branch hides exactly the right buttons with no keyring-specific code.
        let size = keyring_size(store.0.unit_level().unwrap_or(1));
        let mut slots = HashMap::new();
        for i in 0..size.min(u32::from(KEYRING_SLOTS)) as u8 {
            let guid = store.0.player_keyring_slot(i).unwrap_or(0);
            if let Some(mut slot) = resolve_slot(
                guid,
                &mut items,
                icons.as_deref(),
                enchant_rows,
                &commands,
                &cooldowns,
                spell_catalog,
                &mut names,
                now,
                ui_now,
            ) {
                slot.locked = pending.contains(KEYRING_CONTAINER, u32::from(i) + 1);
                slots.insert(u32::from(i) + 1, slot);
            }
        }
        fresh.insert(
            KEYRING_CONTAINER,
            ContainerState {
                name: Some("Keyring".into()),
                num_slots: size,
                slots,
            },
        );

        // `HasKey()` — the gate that decides whether the keyring exists in the UI at all. Pushed
        // beside the containers because it is the same knowledge (item templates) read over the
        // same slot arrays, and it must be fresh on exactly the frames a BAG_UPDATE fires.
        let key = has_key(&store.0, &mut items, &commands);
        if key != memory.had_key {
            debug!(
                "ui_items: HasKey() -> {key} (keyring {})",
                if key { "shown" } else { "hidden" }
            );
            memory.had_key = key;
        }
        script.set_has_key(key);
    }

    // Diff whole bags; push + fire BAG_UPDATE per transition, one BAG_UPDATE_DELAYED per batch. A
    // pending-lock transition always flips a slot's `.locked` (part of `ContainerSlot`'s equality),
    // so it always shows up here too — but `transitioned`'s ITEM_LOCK_CHANGED fires unconditionally
    // below rather than leaning on that invariant.
    let changed: Vec<i64> = fresh
        .keys()
        .chain(memory.pushed.keys())
        .copied()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .filter(|b| fresh.get(b) != memory.pushed.get(b))
        .collect();
    if !changed.is_empty() {
        for &bag in &changed {
            script.set_container(bag, fresh.get(&bag).cloned());
        }
        // Name the bags, not just the count: "3 changed" can't tell you WHICH container moved, and
        // the negative ids (−1 bank, −2 keyring) are exactly the ones you go looking for.
        debug!(
            "ui_items: fed {} changed bag(s) — {}",
            changed.len(),
            changed
                .iter()
                .map(|b| format!("{b}:{}", fresh.get(b).map_or(0, |c| c.slots.len())))
                .collect::<Vec<_>>()
                .join(" ")
        );
        for &bag in &changed {
            // The vault fires the reference's own event, per changed slot with the slot id
            // (`PLAYERBANKSLOTS_CHANGED(slot)` — BankFrame repaints the one button); everything
            // else — backpack, equipped bags, AND bank bags (ordinary container frames in the
            // reference) — fires BAG_UPDATE(bagID).
            if bag == BANK_CONTAINER {
                let empty = HashMap::new();
                let now_slots = fresh.get(&bag).map(|c| &c.slots).unwrap_or(&empty);
                let was_slots = memory.pushed.get(&bag).map(|c| &c.slots).unwrap_or(&empty);
                for slot in 1..=u32::from(BANK_SLOTS) {
                    if now_slots.get(&slot) != was_slots.get(&slot) {
                        script.fire_event(
                            "PLAYERBANKSLOTS_CHANGED",
                            vec![ScriptValue::Int(i64::from(slot))],
                        );
                    }
                }
            } else {
                script.fire_event("BAG_UPDATE", vec![ScriptValue::Int(bag)]);
            }
        }
        memory.pushed = fresh;
        script.fire_event("BAG_UPDATE_DELAYED", vec![]);
    }
    // The lock-transition event (decision 0218: the bag windows' own repaint trigger) — after the
    // container push above, so a listener's repaint reads the corrected `.locked` state.
    for (bag, slot) in transitioned {
        script.fire_event(
            "ITEM_LOCK_CHANGED",
            vec![ScriptValue::Int(bag), ScriptValue::Int(i64::from(slot))],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::charges_count;
    use benilla_protocol::messages::ItemSpellEntry;

    fn slot(spell_id: u32, charges: i32) -> ItemSpellEntry {
        ItemSpellEntry {
            index: 0,
            spell_id,
            trigger: 0,
            charges,
            cooldown_ms: -1,
            category: 0,
            category_cooldown_ms: -1,
        }
    }

    /// The real builder's charge gate (`0x52da01`/`0x52db51`): the `-1` consume-on-use sentinel
    /// prints NO line — the fix for food/water/potions (Tough Hunk of Bread's wire `-1` was
    /// rendering "1 Charge") — while a real pool prints its absolute value.
    #[test]
    fn charge_gate_matches_the_real_builder() {
        // Food/water/potions: spellcharges -1 (VERIFIED live vmangos item_template — bread 4540,
        // spring water 159, conjured water 5350 all carry `433/430, -1`). No line.
        assert_eq!(charges_count(&[slot(433, -1)]), 0, "food's -1 = no line");
        // A real charge pool, negative = item destroyed when depleted (Flame Deflector 4376:
        // `4057, -5`) — prints the absolute count.
        assert_eq!(charges_count(&[slot(4057, -5)]), 5, "wand-style pool");
        // A positive pool would print as-is (the abs is a no-op).
        assert_eq!(charges_count(&[slot(4057, 3)]), 3);
        // Chargeless slots and empty slots: no line.
        assert_eq!(charges_count(&[slot(433, 0)]), 0, "template 0 = sentinel");
        assert_eq!(charges_count(&[slot(0, -5)]), 0, "no spell = no slot");
        assert_eq!(charges_count(&[]), 0);
        // The election walks slots: a leading sentinel slot doesn't mask a later pool.
        assert_eq!(charges_count(&[slot(433, -1), slot(4057, -10)]), 10);
    }

    /// The trigger line's **empty-description law**, byte-verified at the item builder
    /// (`0x52da29`-`0x52da31`: test the expanded text's first byte, jump past the whole block on
    /// zero) — checked against the REAL 5875 Spell.dbc, because the whole point is what the actual
    /// data holds.
    ///
    /// The director's report: a dungeon key's tooltip read "Use: Opening". `Opening` is a real
    /// spell with a real name and NO description, and the old code fell back to the name.
    #[test]
    fn an_undescribed_spell_prints_no_trigger_line_on_real_data() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let spells = crate::ui_action::Spells {
            catalog: benilla_formats::load_spell_catalog(&mut chain).expect("Spell.dbc"),
            forms: benilla_formats::load_shapeshift_forms(&mut chain)
                .expect("SpellShapeshiftForm.dbc"),
            ranges: benilla_formats::load_spell_ranges(&mut chain).expect("SpellRange.dbc"),
            cast_times: benilla_formats::load_spell_cast_times(&mut chain)
                .expect("SpellCastTimes.dbc"),
            durations: benilla_formats::load_spell_durations(&mut chain)
                .expect("SpellDuration.dbc"),
            radii: benilla_formats::load_spell_radii(&mut chain).expect("SpellRadius.dbc"),
        };

        // Every "Opening"/"Closing" the lock chain can reach — the key spells (3365/3366/6247/6477
        // are all literally named "Opening") carry no description, so NO Use: line may be built.
        for id in [3365u32, 3366, 6246, 6247, 6477, 21651] {
            let d = spells.catalog.get(id).expect("a real Spell.dbc row");
            assert!(
                !d.name.is_empty(),
                "spell {id} has a name — which is exactly what must NOT leak into the tooltip"
            );
            assert_eq!(
                super::spell_desc_text(Some(&spells), id, None),
                None,
                "spell {id} ({:?}) has no description, so the reference prints no trigger line",
                d.name
            );
        }

        // The control: a described spell still produces its line, so this is a law about EMPTY
        // descriptions and not a blanket mute. Fireball (133) is the tooltip suite's own anchor.
        let fireball = super::spell_desc_text(Some(&spells), 133, None)
            .expect("a described spell still yields its line");
        assert!(
            fireball.contains("damage"),
            "expected the substituted Fireball description, got {fireball:?}"
        );
    }
}
