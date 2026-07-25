//! The app-side **unit snapshot + event feed** (decision 0068 §3): the bridge that turns live ECS
//! game state into the plain data the engine-free `Unit*` Lua bindings read, and into the WoW events
//! that drive a frame's `OnEvent`.
//!
//! The architecture is deliberate (decisions 0006/0061): the Lua game-state API must **not** reach
//! into the ECS. Instead this runs each frame, *before* the VM's tick/event dispatch
//! ([`crate::ui_script::UiInput`]), and pushes a [`UnitState`] snapshot for each unit token into the
//! VM via [`UiScript::set_unit`]. The `"player"` token reads our own avatar's [`ObjectStore`] (tagged
//! [`SelfPlayer`]); `"target"` reads the [`Selection`]'s entity. Both are found by their ECS entity,
//! not by re-deriving a guid — the ECS already owns the guid↔entity map.
//!
//! Names come from the [`crate::names::NameCache`] (the 1.12 wire has no descriptor names — the
//! query-cache seam): the feed resolves each token's guid, which asks the server once on a miss and
//! fills in a later frame; the transition fires `UNIT_NAME_UPDATE` so frames repaint.
//!
//! The event surface is the Era set, fired per field on transitions: `UNIT_HEALTH`/`UNIT_MAXHEALTH`/
//! `UNIT_LEVEL` (arg1 = token), `UNIT_POWER_UPDATE`/`UNIT_MAXPOWER` (token, power token e.g.
//! `"MANA"`), `UNIT_DISPLAYPOWER` (power *type* changed), `UNIT_NAME_UPDATE`, plus
//! `PLAYER_ENTERING_WORLD` once and `PLAYER_TARGET_CHANGED` on selection change. A token appearing
//! counts as a transition of every present field (frames also pull on target change, so either path
//! populates).

use std::collections::HashMap;

use bevy::prelude::*;

use benilla_ui::script::{power_token, ScriptValue, UiScript, UnitState};

use crate::names::NameCache;
use crate::net::{Guid, NetCommands, ObjectStore, Reputations, SelfPlayer};
use crate::target::{ring_reaction, Factions, Selection};
use crate::ui_script::UiInput;

/// The feed pass — runs before [`UiInput`] so the snapshot + events it produces are in place when the
/// VM ticks and dispatches this frame. A named set so the demo override ([`crate::ui_script`]) can
/// order itself after it.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct UnitFeed;

/// One combat occurrence over a unit — the `UNIT_COMBAT` event feed (decision 0576: the portrait
/// hit indicator's wire; the shipped `CombatFeedback.lua` is the consumer). **§5-verified**
/// (wow-re `object-layer/scratch/unit-combat-event-law.md`): the one emitter `0x494600` fires
/// `(token, action, descriptor, amount, type)` once per live token the unit maps to, with **no
/// self-suppression and no cvar gate** — the worldtext Gate A's inverse. `type` (arg5) is the
/// damage school on the melee/spell-damage paths; the miss and heal wrappers hard-code 0. The
/// melee victim event is **deferred to the swing impact keyframe** (it rides inside `0x6243e0`,
/// reached only from the `0x624530` victim dispatcher — C2 CONFIRMED). ENERGIZE never fires in
/// 5875 (string absent binary-wide). Producers: melee at [`melee_unit_combat`], spells/heals at
/// packet receive (`net/apply/combat_log.rs`). Consumed by [`fire_unit_combat`], which resolves
/// the entity to its live unit tokens.
#[derive(Message, Clone, Copy)]
pub(crate) struct UnitCombatFeedback {
    pub(crate) unit: Entity,
    /// `arg2` — the action: `WOUND`/`MISS`/`DODGE`/`PARRY`/`BLOCK`/`EVADE`/`IMMUNE`/`DEFLECT`/
    /// `RESIST`/`ABSORB`/`REFLECT`/`HEAL`/`ENERGIZE`.
    pub(crate) action: &'static str,
    /// `arg3` — the descriptor: `CRITICAL`/`CRUSHING`/`GLANCING`/`ABSORB`/`BLOCK`/`RESIST`, or `""`.
    pub(crate) flags: &'static str,
    /// `arg4` — the amount (damage/heal/energize; 0 for pure words).
    pub(crate) amount: u32,
    /// `arg5` — the school int (0 = physical; the Lua's `type > 0` draws the number spell-yellow).
    pub(crate) school: u32,
}

/// One center-combat-text message — the `COMBAT_TEXT_UPDATE` event feed (decision 0578; the
/// Blizzard_CombatText transcription is the consumer). **§5-verified** (wow-re
/// `playername/scratch/combat-text-update-emission-law.md`): event id 0x21E, fired via the
/// formatted SignalEvent `0x703f50` from the UnitCombatLog_C.cpp emit helpers — every producer
/// fires **at packet parse** (the melee one too: `0x6255b0 → 0x629d30`, one call stack — NOT the
/// impact-keyframe deferral, which belongs to the worldtext/UNIT_COMBAT victim dispatch).
/// `message_type` is the addon's vocabulary (`DAMAGE`/`DAMAGE_CRIT`/`SPELL_DAMAGE`/`HEAL`/…);
/// `data`/`extra` mirror `arg2`/`arg3` (all strings on the real wire — the fmt is `"%s..%d.."`).
/// Producers gate on the SELF recipient — the ref's emit is co-gated with the chat combat-log
/// category scope (participants beyond self CAN fire it there); the exact participant rule is an
/// open residual (decision 0580), and self-only is the display-equivalent conservative cut.
#[derive(Message, Clone)]
pub(crate) struct CombatTextEvent {
    pub(crate) message_type: &'static str,
    pub(crate) data: Option<String>,
    pub(crate) extra: Option<String>,
}

/// The feed's change-tracking memory: what we last told the VM, so we only fire events on transitions.
#[derive(Resource, Default)]
struct UnitFeedState {
    /// Whether `PLAYER_ENTERING_WORLD` has been fired (once at startup).
    entered_world: bool,
    /// Per token, the last snapshot we pushed — the per-field event triggers diff against it.
    last: HashMap<String, UnitState>,
    /// The last selection guid, for the `PLAYER_TARGET_CHANGED` trigger.
    target_guid: Option<u64>,
    /// The last `(PLAYER_XP, PLAYER_NEXT_LEVEL_XP)` pair pushed, for the `PLAYER_XP_UPDATE` trigger —
    /// the XP bar's feed is a player-global (like coinage), not a per-unit-token field.
    last_xp: Option<(u32, u32)>,
    /// The self unit's last in-combat flag, for the `PLAYER_REGEN_DISABLED`/`ENABLED` triggers
    /// (`None` until first seen — first sight fires only when already IN combat, so logging in
    /// at peace never announces "Leaving Combat").
    in_combat: Option<bool>,
}

/// Adds the per-frame unit feed. The `Unit*` bindings themselves live in `benilla-ui`; this only
/// supplies their data (and the events) from ECS state.
pub(crate) struct UiUnitPlugin;

impl Plugin for UiUnitPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UnitFeedState>()
            .add_message::<UnitCombatFeedback>()
            .add_message::<CombatTextEvent>()
            .add_systems(
                Update,
                (
                    feed_units,
                    melee_unit_combat,
                    fire_unit_combat,
                    fire_combat_text,
                )
                    .chain()
                    .in_set(UnitFeed)
                    .before(UiInput),
            );
    }
}

/// Melee swing → the `UNIT_COMBAT` vocabulary (§5-verified shape, wow-re
/// `unit-combat-event-law.md`): the action comes from the melee wrapper's per-victim-state table
/// (`0x4946d0` → `actionTable@0x83de28`), the descriptor from HitInfo bits **keyed on the
/// amount's sign** — `amount > 0` picks among CRITICAL `0x80` / GLANCING `0x4000` / CRUSHING
/// `0x8000`, `amount ≤ 0` among ABSORB `0x20` / BLOCK `0x800` / RESIST `0x40`, else `""`.
fn melee_feedback(hit_info: u32, victim_state: u32, damage: u32) -> (&'static str, &'static str) {
    match victim_state {
        2 => ("DODGE", ""),
        3 => ("PARRY", ""),
        5 => ("BLOCK", ""),
        6 => ("EVADE", ""),
        7 => ("IMMUNE", ""),
        8 => ("DEFLECT", ""),
        // 0 UNAFFECTED / 1 NORMAL / 4 INTERRUPT: WOUND, descriptor by the amount-sign key.
        _ => {
            if damage > 0 {
                if hit_info & 0x80 != 0 {
                    ("WOUND", "CRITICAL")
                } else if hit_info & 0x4000 != 0 {
                    ("WOUND", "GLANCING")
                } else if hit_info & 0x8000 != 0 {
                    ("WOUND", "CRUSHING")
                } else {
                    ("WOUND", "")
                }
            } else if hit_info & 0x20 != 0 {
                ("WOUND", "ABSORB") // full absorb
            } else if hit_info & 0x800 != 0 {
                ("WOUND", "BLOCK") // full block, when the bridge didn't rewrite the state
            } else if hit_info & 0x40 != 0 {
                ("WOUND", "RESIST") // full resist
            } else {
                ("MISS", "")
            }
        }
    }
}

/// The melee `UNIT_COMBAT` producer: rides the swing's impact keyframe ([`SwingImpact`]) with the
/// rest of the victim feedback — including `text_only` flushes, which the client fires the text
/// channel for. Every victim qualifies (no Gate A, no source class on the portrait path); token
/// resolution happens in [`fire_unit_combat`], so a swing on an un-tokened bystander simply
/// fires nothing. The center combat text does NOT ride here — the client fires it synchronously
/// at packet parse (§5-corrected, decision 0580; the producer lives in `net/apply/combat.rs`).
fn melee_unit_combat(
    mut impacts: MessageReader<crate::creature_anim::SwingImpact>,
    mut out: MessageWriter<UnitCombatFeedback>,
) {
    for crate::creature_anim::SwingImpact { swing: s, .. } in impacts.read() {
        let Some(victim) = s.victim else { continue };
        let (action, flags) = melee_feedback(s.hit_info, s.victim_state, s.damage);
        out.write(UnitCombatFeedback {
            unit: victim,
            action,
            flags,
            amount: s.damage,
            school: 0, // melee is physical (the wire's sub-damage school is not carried — always 0 here)
        });
    }
}

/// Drain [`CombatTextEvent`] into the VM: `COMBAT_TEXT_UPDATE(messageType, data, extra)` — the
/// arg shape the shipped Blizzard_CombatText `OnEvent` reads (`arg1..arg3`).
fn fire_combat_text(
    script: Option<NonSendMut<UiScript>>,
    mut events: MessageReader<CombatTextEvent>,
) {
    let Some(mut script) = script else {
        return;
    };
    for ev in events.read() {
        let arg = |v: &Option<String>| v.clone().map_or(ScriptValue::Nil, ScriptValue::Str);
        script.fire_event(
            "COMBAT_TEXT_UPDATE",
            vec![
                ScriptValue::Str(ev.message_type.to_string()),
                arg(&ev.data),
                arg(&ev.extra),
            ],
        );
    }
}

/// Drain [`UnitCombatFeedback`] into the VM: fire `UNIT_COMBAT` once per live token the entity
/// maps to (`"player"`, `"target"` — the same tokens [`feed_units`] feeds). The frames filter by
/// `arg1` exactly like the real client's; in 1.12 only PlayerFrame/PetFrame register it, so a
/// `"target"` fire is API surface, not pixels.
fn fire_unit_combat(
    script: Option<NonSendMut<UiScript>>,
    mut events: MessageReader<UnitCombatFeedback>,
    self_q: Query<(), With<SelfPlayer>>,
    selection: Res<Selection>,
) {
    let Some(mut script) = script else {
        return;
    };
    for ev in events.read() {
        let mut fire = |token: &str| {
            script.fire_event(
                "UNIT_COMBAT",
                vec![
                    ScriptValue::Str(token.to_string()),
                    ScriptValue::Str(ev.action.to_string()),
                    ScriptValue::Str(ev.flags.to_string()),
                    ScriptValue::Int(i64::from(ev.amount)),
                    ScriptValue::Int(i64::from(ev.school)),
                ],
            );
        };
        if self_q.contains(ev.unit) {
            fire("player");
        }
        if selection.target == Some(ev.unit) {
            fire("target");
        }
    }
}

/// The 1.12 race id → (localized display, `raceFile` token) — `UnitRace`'s two returns. The file
/// token is the client's internal name (undead = `"Scourge"`, the space dropped from `"NightElf"`),
/// the same vocabulary the 2D portrait stand-in files use (`portrait::temporary_portrait`). The
/// display column is also what `$R`/`$r` expand to ([`crate::npc_text`]) — one table, both readers.
pub(crate) fn race_names(race: u8) -> Option<(&'static str, &'static str)> {
    Some(match race {
        1 => ("Human", "Human"),
        2 => ("Orc", "Orc"),
        3 => ("Dwarf", "Dwarf"),
        4 => ("Night Elf", "NightElf"),
        5 => ("Undead", "Scourge"),
        6 => ("Tauren", "Tauren"),
        7 => ("Gnome", "Gnome"),
        8 => ("Troll", "Troll"),
        _ => return None,
    })
}

/// The 1.12 class id → (localized display, `classFileName`) — `UnitClass`'s two returns. The file
/// name is uppercase (the ref's `strupper(classFileName)` tooltip lookups index GlobalStrings keys
/// like `WARRIOR_STRENGTH_TOOLTIP` directly with it).
pub(crate) fn class_names(class: u8) -> Option<(&'static str, &'static str)> {
    Some(match class {
        1 => ("Warrior", "WARRIOR"),
        2 => ("Paladin", "PALADIN"),
        3 => ("Hunter", "HUNTER"),
        4 => ("Rogue", "ROGUE"),
        5 => ("Priest", "PRIEST"),
        7 => ("Shaman", "SHAMAN"),
        8 => ("Mage", "MAGE"),
        9 => ("Warlock", "WARLOCK"),
        11 => ("Druid", "DRUID"),
        _ => return None,
    })
}

/// Resolve a UnitPopup unit token to the **player guid** it names — `"target"` through the
/// selection iff it really is a player (the target frame's PLAYER menu), a `"partyN"` token through
/// the roster (the party frame's PARTY menu). `"player"` (yourself) and any unresolved token answer
/// `None`.
///
/// Shared by every popup verb that acts on another player: trade's `InitiateTrade` (decision 0592
/// P1) and inspect's `NotifyInspect` (decision 0631) both need exactly this step, so it lives here
/// rather than once per window.
pub(crate) fn player_token_guid(
    token: &str,
    selection: &Selection,
    group: &crate::ui_party::GroupState,
) -> Option<u64> {
    match token {
        "target" => selection
            .guid
            .filter(|g| benilla_protocol::guid::is_player(*g)),
        "player" => None,
        tok => tok
            .strip_prefix("party")
            .and_then(|n| n.parse::<usize>().ok())
            .and_then(|n| n.checked_sub(1))
            .and_then(|n| group.party_slots().nth(n))
            .map(|m| m.guid),
    }
}

/// Build a unit snapshot from a streamed object's descriptor (decision 0061's `ObjectFields`) plus
/// its cache-resolved name and its `UnitReaction` value (`1..8`, or `0` for tokens whose reaction we
/// don't resolve — everything but `"target"`; see [`feed_units`]).
pub(crate) fn snapshot(store: &ObjectStore, name: Option<String>, reaction: u8) -> UnitState {
    let power_type = store.0.unit_power_type();
    let race = store.0.unit_race().and_then(race_names);
    let class = store.0.unit_class().and_then(class_names);
    UnitState {
        exists: true,
        name,
        health: store.0.unit_health().unwrap_or(0),
        max_health: store.0.unit_max_health().unwrap_or(0),
        level: store.0.unit_level().unwrap_or(0),
        power_type,
        power: store.0.unit_power(power_type).unwrap_or(0),
        max_power: store.0.unit_max_power(power_type).unwrap_or(0),
        dead: store.0.unit_is_dead(),
        // The released-ghost predicate (decision 0308 §1): PLAYER_FLAGS bit 0x10 — a ghost's
        // health is 1, so `dead` above is false for it. Zero/absent on creatures.
        ghost: store.0.player_is_ghost(),
        reaction,
        race: race.map(|(n, _)| n.to_string()),
        race_file: race.map(|(_, f)| f.to_string()),
        class: class.map(|(n, _)| n.to_string()),
        class_file: class.map(|(_, f)| f.to_string()),
        // The descriptor's gender byte (0 male, 1 female) on the API's `UnitSex` scale (2 male,
        // 3 female; 0 = unknown → the binding's nil).
        sex: match store.0.unit_gender() {
            Some(0) => 2,
            Some(1) => 3,
            _ => 0,
        },
        // The tooltip flag lines (decision 0276's unit law): PvP + Skinnable straight off
        // UNIT_FIELD_FLAGS (vmangos UnitDefines.h: 0x1000 / 0x04000000, VERIFIED).
        pvp: store.0.unit_flags() & 0x1000 != 0,
        skinnable: store.0.unit_flags() & 0x0400_0000 != 0,
        // is_player + the creature-record extras (subtitle/type/rank/civilian) are the caller's
        // guid-keyed enrichment — [`enrich_unit`].
        ..Default::default()
    }
}

/// Fill a snapshot's guid-keyed tooltip fields (decision 0276's unit law): players flag
/// `is_player` (the "Race Class (Player)" level line); creatures pull subtitle/type/rank/
/// civilian/leader from the ask-once template record, and resolve the faction-name line.
/// The type word is `CreatureType.dbc`'s enUS display list (ids 1..9 — a fixed 1.12
/// vocabulary; 10 "Not specified" shows nothing).
pub(crate) fn enrich_unit(
    state: &mut UnitState,
    guid: u64,
    names: &NameCache,
    store: &ObjectStore,
    factions: Option<&Factions>,
    self_store: Option<&ObjectStore>,
) {
    if benilla_protocol::guid::is_player(guid) {
        state.is_player = true;
        // No faction line for players — their PLAYER,* factions carry no reputation slot, so
        // the builder's rep-index gate always drops the line (byte-identical to resolving it).
        return;
    }
    let Some(entry) = benilla_protocol::guid::entry(guid) else {
        return;
    };
    if let Some(rec) = names.creature_record(entry) {
        state.subtitle = rec.subname.clone();
        state.creature_type_name = creature_type_word(rec.creature_type).map(str::to_string);
        state.rank = rec.rank;
        state.civilian = rec.civilian;
        state.racial_leader = rec.racial_leader;
        // The faction-name line ("Stormwind", between level and PvP) — the unit builder's tail
        // block, every gate transcribed: the record's HIDE_FACTION_TOOLTIP type flag (0x10, the
        // `0x612610` gate), the template → Faction.dbc hop, the reputation-slot gate
        // (rep_index ≥ 0), and the race/class slot walk with its hidden flag (0x4). The record
        // gate also stands in for the bytes' "no creature info → pass": before the query
        // answers we have no name line either, and the tooltip rebuilds when it lands.
        if rec.type_flags & 0x10 == 0 {
            state.faction_name = (|| {
                let catalog = factions?.catalog();
                let faction_id = catalog.template(store.0.unit_faction_template()?)?.faction;
                let info = catalog.reputation_faction(faction_id)?;
                let self_store = self_store?;
                let race = self_store.0.unit_race().unwrap_or(0);
                let class = self_store.0.unit_class().unwrap_or(0);
                info.tooltip_shows_for(race, class)
                    .then(|| catalog.faction_name(faction_id).map(str::to_string))
                    .flatten()
            })();
        }
    }
}

/// `CreatureType.dbc` id → the enUS display word (the level line's class slot for creatures).
fn creature_type_word(t: u32) -> Option<&'static str> {
    Some(match t {
        1 => "Beast",
        2 => "Dragonkin",
        3 => "Demon",
        4 => "Elemental",
        5 => "Giant",
        6 => "Undead",
        7 => "Humanoid",
        8 => "Critter",
        9 => "Mechanical",
        _ => return None, // 10 "Not specified" shows nothing
    })
}

/// Diff a token's fresh snapshot against the last one pushed and fire the per-field Era events.
/// `prev = None` (the token just appeared) treats every present field as a transition.
pub(crate) fn fire_transitions(
    script: &mut UiScript,
    token: &str,
    prev: Option<&UnitState>,
    cur: &UnitState,
) {
    let tok = || ScriptValue::Str(token.to_string());
    let ptok = || ScriptValue::Str(power_token(cur.power_type).to_string());
    let changed = |f: fn(&UnitState) -> u64| prev.is_none_or(|p| f(p) != f(cur));

    if changed(|u| u64::from(u.health)) {
        script.fire_event("UNIT_HEALTH", vec![tok()]);
    }
    if changed(|u| u64::from(u.max_health)) {
        script.fire_event("UNIT_MAXHEALTH", vec![tok()]);
    }
    if changed(|u| u64::from(u.level)) {
        script.fire_event("UNIT_LEVEL", vec![tok()]);
    }
    if changed(|u| u64::from(u.power_type)) {
        script.fire_event("UNIT_DISPLAYPOWER", vec![tok()]);
    }
    if changed(|u| u64::from(u.power)) {
        script.fire_event("UNIT_POWER_UPDATE", vec![tok(), ptok()]);
    }
    if changed(|u| u64::from(u.max_power)) {
        script.fire_event("UNIT_MAXPOWER", vec![tok(), ptok()]);
    }
    if prev.is_none_or(|p| p.name != cur.name) {
        script.fire_event("UNIT_NAME_UPDATE", vec![tok()]);
    }
}

#[allow(clippy::too_many_arguments)]
fn feed_units(
    script: Option<NonSendMut<UiScript>>,
    self_q: Query<(&ObjectStore, &Guid), With<SelfPlayer>>,
    selection: Res<Selection>,
    stores: Query<&ObjectStore>,
    mut feed: ResMut<UnitFeedState>,
    mut names: ResMut<NameCache>,
    commands: Res<NetCommands>,
    factions: Option<Res<Factions>>,
    reputations: Res<Reputations>,
    group: Res<crate::ui_party::GroupState>,
) {
    let Some(mut script) = script else {
        return;
    };

    // "player" = our own avatar's descriptor; "target" = the selected entity's. Absent → None, which
    // set_unit clears (UnitExists false), exactly as the real client reports a missing unit. Names
    // resolve through the cache — a miss queries the server once and lands on a later frame.
    let self_pair = self_q.iter().next();
    let player = self_pair.map(|(store, guid)| {
        let name = names.resolve(guid.0, &commands).map(str::to_string);
        let mut s = snapshot(store, name, 0);
        s.is_player = true;
        // Identity + the raid-target board mark (decision 0434 §5's popup gating; §6's board).
        s.guid = guid.0;
        s.raid_target = group.raid_target_index(guid.0);
        s
    });
    let target = selection.target.zip(selection.guid).and_then(|(e, guid)| {
        let store = stores.get(e).ok()?;
        let name = names.resolve(guid, &commands).map(str::to_string);
        // The target's reaction toward us, on the `UnitReaction` 1..8 scale — the same decode the
        // selection ring runs (reputation-first, else the faction-template comparator). `ring_reaction`
        // returns the raw 0..7 rank (neutral its no-data fallback), which is `UnitReaction − 1`; +1
        // lands it on the Lua scale the name-plate palette (`UnitReactionColor`) indexes.
        let reaction = ring_reaction(
            factions.as_deref(),
            &reputations,
            Some(store),
            self_pair.map(|(s, _)| s),
        ) + 1;
        let mut s = snapshot(store, name, reaction);
        s.guid = guid;
        s.raid_target = group.raid_target_index(guid);
        // The byte-confirmed CanAttack 0x606980 (decision 0172) — the same predicate TAB and the
        // combat flash run; `UnitCanAttack("player","target")` gates the target frame's
        // difficulty-colored level (ref TargetFrame_CheckLevel).
        s.can_attack = crate::target::can_attack(
            Some(store),
            factions.as_deref(),
            &reputations,
            self_pair.map(|(s, _)| s),
        );
        enrich_unit(
            &mut s,
            guid,
            &names,
            store,
            factions.as_deref(),
            self_pair.map(|(s, _)| s),
        );
        Some(s)
    });

    script.set_unit("player", player.clone());
    script.set_unit("target", target.clone());

    // Initial pull: fire PLAYER_ENTERING_WORLD once so frames do their first paint on their own.
    if !feed.entered_world {
        script.fire_event("PLAYER_ENTERING_WORLD", vec![]);
        feed.entered_world = true;
    }

    for (token, snap) in [("player", &player), ("target", &target)] {
        match snap {
            Some(cur) => {
                let prev = feed.last.get(token);
                if prev != Some(cur) {
                    fire_transitions(&mut script, token, prev, cur);
                    feed.last.insert(token.to_string(), cur.clone());
                }
            }
            None => {
                // Clearing a token isn't a UNIT_* event; the target frame reacts to
                // PLAYER_TARGET_CHANGED below.
                feed.last.remove(token);
            }
        }
    }

    // PLAYER_TARGET_CHANGED (no args, real WoW's shape) when the selection changes.
    if selection.guid != feed.target_guid {
        feed.target_guid = selection.guid;
        script.fire_event("PLAYER_TARGET_CHANGED", vec![]);
    }

    // PLAYER_REGEN_DISABLED/ENABLED: the self in-combat flag transition (`UNIT_FIELD_FLAGS`
    // bit `UNIT_FLAG_IN_COMBAT 0x00080000`, vmangos `UnitDefines.h:564`) — the center combat
    // text's ENTERING/LEAVING_COMBAT feed (decision 0578; the trigger is PROVISIONAL pending
    // the COMBAT_TEXT_UPDATE emission pin).
    if let Some((store, _)) = self_pair {
        let in_combat = store.0.unit_flags() & 0x0008_0000 != 0;
        if feed.in_combat != Some(in_combat) {
            let first_sight = feed.in_combat.is_none();
            feed.in_combat = Some(in_combat);
            if !first_sight || in_combat {
                script.fire_event(
                    if in_combat {
                        "PLAYER_REGEN_DISABLED"
                    } else {
                        "PLAYER_REGEN_ENABLED"
                    },
                    vec![],
                );
            }
        }
    }

    // The XP bar's feed: push our own avatar's PLAYER_XP / PLAYER_NEXT_LEVEL_XP (both PRIVATE, only
    // ever streamed for self) and fire PLAYER_XP_UPDATE when either changes — the coinage feed's
    // shape. Absent fields read 0 (a fresh descriptor's zero default; the bar shows empty until XP
    // streams in).
    if let Some((store, _)) = self_q.iter().next() {
        let xp = store.0.player_xp().unwrap_or(0);
        let next = store.0.player_next_level_xp().unwrap_or(0);
        if feed.last_xp != Some((xp, next)) {
            feed.last_xp = Some((xp, next));
            script.set_player_xp(xp, next);
            script.fire_event("PLAYER_XP_UPDATE", vec![]);
        }
    }
}
