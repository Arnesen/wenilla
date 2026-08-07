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
use crate::ui_chat::{ChatEvent, ChatEventKind, ChatLog};
use crate::ui_script::UiInput;

/// The feed pass — runs **after [`crate::schedule::WorldStage::Net`]** (the feeds snapshot state
/// the net apply writes; unordered, `apply_net_updates` could land BETWEEN two feeds, and a
/// synchronous event fired by the later one then re-read the earlier one's pre-mutation push —
/// the spellbook's cooldown pie stayed cold until a manual reopen, reproduced live 2026-07-31)
/// and before [`UiInput`], so the snapshot + events it produces are in place when the VM ticks
/// and dispatches this frame. A named set so the demo override ([`crate::ui_script`]) can order
/// itself after it. Configured in [`UiUnitPlugin`] — the set's home.
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
    /// The last `(restState, restPool, resting)` triple pushed, for the `UPDATE_EXHAUSTION` and
    /// `PLAYER_UPDATE_RESTING` triggers — player-globals like the XP pair (decision 1082). Pushed
    /// as one snapshot so `GetRestState`/`GetXPExhaustion`/`IsResting` never read it half-updated.
    last_rest: Option<(u8, u32, bool)>,
    /// The last `(count, banked-target guid)` pair pushed, for the `PLAYER_COMBO_POINTS` trigger —
    /// player-globals (`PLAYER_FIELD_BYTES` byte 1 + `PLAYER_FIELD_COMBO_TARGET`), not per-unit-
    /// token fields. Diffed as a pair because the server writes them as one (decision 0875).
    last_combo: Option<(u8, u64)>,
    /// The self unit's last in-combat flag, for the `PLAYER_REGEN_DISABLED`/`ENABLED` triggers
    /// (`None` until first seen — first sight fires only when already IN combat, so logging in
    /// at peace never announces "Leaving Combat").
    in_combat: Option<bool>,
    /// The self unit's last `PLAYER_FLAGS_PVP_DESIRED` bit, for the toggle announcement
    /// (decision 0652). `None` until first seen: the reference reacts to a *changed*-bits mask, so
    /// the descriptor that first carries the flag at login announces nothing.
    pvp_desired: Option<bool>,
    /// Whether we have already warned that our own faction template names no side (decision 0657).
    /// Re-arms when a side resolves again, so a `.gm on` / `.gm off` cycle logs once each way.
    warned_sideless: bool,
}

/// Adds the per-frame unit feed. The `Unit*` bindings themselves live in `benilla-ui`; this only
/// supplies their data (and the events) from ECS state.
pub(crate) struct UiUnitPlugin;

impl Plugin for UiUnitPlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(
            Update,
            UnitFeed.after(crate::schedule::WorldStage::Net), // the set's own doc: the why
        )
        .init_resource::<UnitFeedState>()
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
        )
        .add_systems(Update, drain_pvp_toggles.after(UiInput));
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
        // The UI-facing health/power getters, not the raw fields (decision 1022): a unit carrying
        // `UNIT_DYNFLAG_DEAD` — a feigning hunter — answers 0 to `UnitHealth 0x5174d0` and
        // `UnitMana 0x517670`, while the *max* getters stay ungated, so its bars read 0/max (empty)
        // for itself and for everyone watching. The zeroes ride the ordinary per-field diff in
        // [`fire_transitions`], which fires `UNIT_HEALTH` + the power event on the flag's edge —
        // exactly the pair the reference's `UNIT_DYNAMIC_FLAGS` watcher fires there
        // (`0x6004c5`/`0x6004f0`, event `0x10` and `0x11 + powerType`). The power pair also carries
        // the raw→display divide the same two getters do (decision 1034) — rage rides the wire ×10
        // and pet happiness ×1000, so these are the numbers the reference shows, not the wire's.
        health: store.0.unit_shown_health().unwrap_or(0),
        max_health: store.0.unit_max_health().unwrap_or(0),
        level: store.0.unit_level().unwrap_or(0),
        power_type,
        power: store.0.unit_shown_power(power_type).unwrap_or(0),
        max_power: store.0.unit_shown_max_power(power_type).unwrap_or(0),
        // `UnitIsDead 0x517ac0` — health ≤ 0 **or** the dead-looking flag, so a feigning unit is
        // dead to the API, to the target frame's DEAD text and to the greyed portrait alike.
        dead: store.0.unit_reads_dead(),
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
        // Free-for-all PvP (decision 0646 §1): `PLAYER_FLAGS` bit 7, the same field the ghost
        // predicate above reads (vmangos `Player.h:322` `PLAYER_FLAGS_FFA_PVP`, cross-read against
        // 0633's byte-level `[+0xe68]+8` bit-7). Zero on creatures, which have no PLAYER_FLAGS —
        // and `UnitIsPVPFreeForAll` is false for them in the reference too.
        is_pvp_ffa: store.0.player_flags() & 0x80 != 0,
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
        // The client's one rank getter, both gates (`gated_rank`, decision 0782) — never `rec.rank`
        // directly: an enslaved elite reads rank 0, so it loses its border dragon, its ELITE
        // tooltip word and its world-boss skull together, exactly as in the reference.
        state.rank = crate::names::gated_rank(Some(rec), Some(store));
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

/// Drain the `TogglePVP` intents into `CMSG_TOGGLE_PVP` (decision 0646 §3) — `/pvp` is the only
/// caller the reference has (`SlashCmdList["PVP"]`), and the only one we have.
///
/// This rides the unit feed's plugin rather than a `ui_pvp` of its own: the family's whole client
/// state is one remembered bit ([`UnitFeedState::pvp_desired`]), which lives with the feed's other
/// self-flag edge (`in_combat`) rather than in a plugin of its own.
fn drain_pvp_toggles(script: Option<NonSendMut<UiScript>>, commands: Res<NetCommands>) {
    let Some(mut script) = script else {
        return;
    };
    for _ in 0..script.take_pvp_toggles() {
        let _ = commands.0.send(crate::net::ClientCommand::TogglePvp);
    }
}

/// `PLAYER_FLAGS_PVP_DESIRED` — the PvP *preference* bit (vmangos `PlayerDefines.h`), which is what
/// `CMSG_TOGGLE_PVP` flips. Not to be confused with `UNIT_FIELD_FLAGS`' `PVP` bit `0x1000`, the flag
/// the icon draws: the preference clears instantly, the flag lingers for the server's timer.
const PLAYER_FLAGS_PVP_DESIRED: u32 = 0x200;

/// The PvP-preference announcement rule (decision 0652): `(toast, verbose)` on a real change of the
/// bit, `None` otherwise.
///
/// `was: None` is first sight and stays silent — the reference's handler is driven by a
/// *changed-bits* mask (`new ^ old`), so the descriptor that first carries the flag at login says
/// nothing. The two texts are verbatim 1.12 `GlobalStrings.lua`: the `ERR_PVP_TOGGLE_*` toast
/// (l.1788-1789) and the `PVP_TOGGLE_*_VERBOSE` chat sentence (l.3221-3222). Both are
/// argument-free, so there is no format step.
fn pvp_announcement(was: Option<bool>, now: bool) -> Option<(&'static str, &'static str)> {
    if was? == now {
        return None;
    }
    Some(if now {
        (
            "PvP combat toggled on",
            "You are now flagged for PvP combat and will remain so until toggled off.",
        )
    } else {
        (
            "PvP combat toggled off",
            "You will be unflagged for PvP combat after five minutes of non-PvP action in friendly territory.",
        )
    })
}

/// A unit's PvP faction group — `UnitFactionGroup`'s pair, as the icon law reads it (decision
/// 0646 §1/§3): `UNIT_FIELD_FACTIONTEMPLATE` → `FactionTemplate.dbc`'s group mask → the
/// `FactionGroup.dbc` name of its **side** bit.
///
/// The `& 6` is the whole rule and it is load-bearing, not a tidy-up: every playable race's
/// template carries `Player|<side>` (mask 3 Alliance, 5 Horde) and so do the PvP-flagged city
/// guards, while `FactionGroup.dbc`'s Player and Monster rows have EMPTY localized names and no
/// `UI-PVP-Player`/`UI-PVP-Monster` texture ships. Masking to the two side bits before the lookup
/// is therefore the only reading the shipped art admits — a lowest-bit walk would answer "Player"
/// for every player in the game.
pub(crate) fn faction_group(store: &ObjectStore, factions: Option<&Factions>) -> Option<String> {
    let catalog = factions?.catalog();
    let template = catalog.template(store.0.unit_faction_template()?)?;
    catalog
        .faction_group_name(template.group_mask & 6)
        .map(str::to_string)
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
    // UNIT_CLASSIFICATION_CHANGED (decision 0782) — the target frame's border-art repaint wire, and
    // the only frame that registers it in the reference. Edge-fired on the gated rank, which IS the
    // classification (`classification_word` is a pure table index), so this fires exactly when the
    // border would change: the creature query landing on a freshly-seen elite, and a mob being
    // enslaved or released. Without it the border would only be right on re-target, because the
    // ref's own `TargetFrame_Update` is the sole other caller of CheckClassification.
    if prev.is_none_or(|p| p.rank != cur.rank) {
        script.fire_event("UNIT_CLASSIFICATION_CHANGED", vec![tok()]);
    }
    // UNIT_FACTION (decision 0646 §2) — the PvP-icon repaint wire, fired on the three fields the
    // icon law reads. Exactly the three frames that draw the icon register it in the reference
    // (player, target, party member) and nothing else does. Edge-fired, so the player frame's
    // `igPVPUpdate` sounds once per flag change rather than once per repaint.
    if prev.is_none_or(|p| {
        (p.pvp, p.is_pvp_ffa, &p.faction_group) != (cur.pvp, cur.is_pvp_ffa, &cur.faction_group)
    }) {
        script.fire_event("UNIT_FACTION", vec![tok()]);
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
    mut chat: ResMut<ChatLog>,
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
        s.faction_group = faction_group(store, factions.as_deref());
        s
    });
    // The GM-mode confound, made audible (decision 0657). A self player whose faction template
    // names no side is almost always GM mode: vmangos swaps a GM to faction template 35, whose
    // `FactionTemplate.dbc` group mask is 0, and every faction-derived surface then has no side to
    // draw — the PvP flag icon most visibly, which simply hides. That is faithful (the only side
    // art that ships is Alliance/Horde/FFA), and it is indistinguishable on screen from the icon
    // being broken, which has now cost two separate sessions an investigation. So it is a BENCH
    // diagnostic, not UI: nothing appears on screen, exactly as in the reference.
    if let Some(p) = &player {
        let sideless = p.faction_group.is_none();
        if sideless && !feed.warned_sideless {
            warn!(
                "faction: our own template names no side (usually GM mode — vmangos forces \
                 template 35, group mask 0). Faction-derived UI cannot resolve a side while this \
                 holds: the PvP flag icon stays hidden however flagged you are. `.gm off` restores it."
            );
        }
        feed.warned_sideless = sideless;
    }
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
        s.faction_group = faction_group(store, factions.as_deref());
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

    // The PvP toggle's own feedback (decision 0652). The reference's local-player PLAYER_FLAGS
    // change handler reacts to the PVP_DESIRED bit — and to nothing else about PvP — with two
    // lines: a yellow UI_INFO_MESSAGE toast, then the verbose sentence as a system chat line. It is
    // keyed on the *preference*, not on the flag the icon draws, which is exactly why it matters:
    // toggling OFF changes no visible flag for five minutes, so without these two lines the key
    // reads as dead. The reference only announces its own player (the whole branch sits behind a
    // guid == localPlayer gate) and only on a change, never on the descriptor that first carries it.
    if let Some((store, _)) = self_pair {
        let desired = store.0.player_flags() & PLAYER_FLAGS_PVP_DESIRED != 0;
        if let Some((toast, verbose)) = pvp_announcement(feed.pvp_desired, desired) {
            script.fire_event("UI_INFO_MESSAGE", vec![ScriptValue::Str(toast.to_string())]);
            chat.push_event(ChatEvent::text_only(
                ChatEventKind::System,
                verbose.to_string(),
            ));
        }
        feed.pvp_desired = Some(desired);
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

    // The rest feed (decision 1082): the `PLAYER_BYTES_2` rest-state byte, the
    // `PLAYER_REST_STATE_EXPERIENCE` pool and the `PLAYER_FLAGS_RESTING` bit, pushed as one
    // snapshot. Two watches, the reference's own grain: `UPDATE_EXHAUSTION` on a state-or-pool
    // change (the exhaustion tick and the bar's rested/normal color both register it),
    // `PLAYER_UPDATE_RESTING` on the flag toggling (the player frame's zzz). Absent fields read
    // 0 / not-resting — a fresh descriptor's zero default, which `GetRestState` renders as Normal.
    if let Some((store, _)) = self_q.iter().next() {
        let rest = (
            store.0.player_rest_state().unwrap_or(0),
            store.0.player_rest_state_experience().unwrap_or(0),
            store.0.player_is_resting(),
        );
        if feed.last_rest != Some(rest) {
            let prev = feed.last_rest;
            feed.last_rest = Some(rest);
            script.set_rest_state(rest.0, rest.1, rest.2);
            if prev.map(|p| (p.0, p.1)) != Some((rest.0, rest.1)) {
                script.fire_event("UPDATE_EXHAUSTION", vec![]);
            }
            if prev.map(|p| p.2) != Some(rest.2) {
                script.fire_event("PLAYER_UPDATE_RESTING", vec![]);
            }
        }
    }

    // The combo-point feed: `PLAYER_FIELD_BYTES` byte 1 and the `PLAYER_FIELD_COMBO_TARGET` GUID
    // it is banked against, both PRIVATE, pushed as the pair the server writes (0869, 0875).
    //
    // Both halves are PUSHED whenever either moves — `GetComboPoints` reads both, so a stale
    // target would make it lie on the next call. Only the COUNT fires the event: the client's
    // `PLAYER_COMBO_POINTS` (event 202, `0x5ddff0`) is registered at `0x5dd9d9` as a **one-byte
    // field-change watch on `+0x1029`** and nothing else — the combo-target field has no watch of
    // its own (0879). The watch carries no value test, so it fires on the drop back to zero too,
    // which is the edge `ComboFrame` needs to hide itself when the server's 4 s
    // `REACTIVE_OVERPOWER` timer clears the point.
    //
    // The reader's other input — which unit is selected — reaches it through
    // `PLAYER_TARGET_CHANGED` above, the event the reference registers `ComboFrame` for precisely
    // because `GetComboPoints` consults the current target.
    if let Some((store, _)) = self_q.iter().next() {
        let banked = (
            store.0.player_combo_points().unwrap_or(0),
            store.0.player_combo_target(),
        );
        if let Some(fire) = combo_edge(feed.last_combo, banked) {
            feed.last_combo = Some(banked);
            script.set_combo_points(banked.0, banked.1);
            if fire {
                script.fire_event("PLAYER_COMBO_POINTS", vec![]);
            }
        }
    }
}

/// The combo feed's edge, given the last `(count, banked-target)` pair pushed and the current one:
/// `None` = nothing moved, `Some(fire)` = push the pair, and whether `PLAYER_COMBO_POINTS` fires.
///
/// The two halves diverge because the client's watches do. Event 202 is registered at `0x5dd9d9`
/// as a **one-byte field-change watch on `+0x1029`** — the count — and `PLAYER_FIELD_COMBO_TARGET`
/// carries no watch of its own (§5-VERIFIED, decision 0879). So a re-bank onto a different unit at
/// an unchanged count moves the *value* `GetComboPoints` reads without announcing itself, and the
/// UI hears about it through `PLAYER_TARGET_CHANGED` instead. The watch has no value test, so the
/// drop back to zero fires like any other change — the edge that takes the dots down.
fn combo_edge(last: Option<(u8, u64)>, now: (u8, u64)) -> Option<bool> {
    (last != Some(now)).then(|| last.map(|(count, _)| count) != Some(now.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The combo feed pushes on either half moving but speaks only for the count — the client's
    /// watch is on that byte alone (decision 0879).
    #[test]
    fn only_the_count_fires_the_combo_event() {
        const A: u64 = 0xF130_0000_0000_0001;
        const B: u64 = 0xF130_0000_0000_0002;

        assert_eq!(combo_edge(Some((1, A)), (1, A)), None, "nothing moved");
        assert_eq!(
            combo_edge(Some((1, A)), (2, A)),
            Some(true),
            "a builder lands: push and speak"
        );
        assert_eq!(
            combo_edge(Some((5, A)), (0, 0)),
            Some(true),
            "the clear speaks too — the falling edge is what hides the dots"
        );
        assert_eq!(
            combo_edge(Some((1, A)), (1, B)),
            Some(false),
            "re-banked onto another unit at the same count: pushed, but silent like the client"
        );
        assert_eq!(
            combo_edge(None, (0, 0)),
            Some(true),
            "first sight announces once, as the descriptor block's first write does"
        );
    }

    /// **A feigning hunter's frame reads as a corpse's** (decision 1022) — the symptom the whole
    /// change exists for: the wire says nothing but `UNIT_DYNFLAG_DEAD`, and the snapshot has to
    /// turn that into empty bars against a real maximum plus `UnitIsDead`. Asserted through
    /// `snapshot` rather than the field getters (which have their own byte-law test) because what
    /// can regress here is the *wiring* — a future edit reaching for `unit_health` again.
    #[test]
    fn a_feigning_unit_snapshots_empty_bars_over_a_real_maximum() {
        use benilla_protocol::messages::ObjectFields;

        /// `UNIT_FIELD_HEALTH` / `MAXHEALTH` / `POWER1` / `MAXPOWER1` / `DYNAMIC_FLAGS`.
        const HEALTH: u16 = 22;
        const POWER1: u16 = 23;
        const MAXHEALTH: u16 = 28;
        const MAXPOWER1: u16 = 29;
        const DYNFLAGS: u16 = 143;

        let vitals = [
            (HEALTH, 1200),
            (MAXHEALTH, 1500),
            (POWER1, 300),
            (MAXPOWER1, 900),
        ];
        let alive = snapshot(
            &ObjectStore(ObjectFields::from_pairs(&vitals)),
            Some("Hunter".into()),
            0,
        );
        assert_eq!((alive.health, alive.max_health), (1200, 1500));
        assert_eq!((alive.power, alive.max_power), (300, 900));
        assert!(!alive.dead);

        let feigning = snapshot(
            &ObjectStore(ObjectFields::from_pairs(
                &[vitals.as_slice(), &[(DYNFLAGS, 0x20)]].concat(),
            )),
            Some("Hunter".into()),
            0,
        );
        assert_eq!(
            (feigning.health, feigning.max_health),
            (0, 1500),
            "UnitHealth 0x5174d0 zeroes, UnitHealthMax 0x5175b0 does not — an EMPTY bar, not a gone one"
        );
        assert_eq!(
            (feigning.power, feigning.max_power),
            (0, 900),
            "UnitMana 0x517670 zeroes, UnitManaMax 0x5177e0 does not"
        );
        assert!(feigning.dead, "UnitIsDead 0x517ac0's dynflag leg");
        assert!(
            !feigning.ghost,
            "feign is not a ghost — PLAYER_FLAGS is clear"
        );

        // …and the flag moves the two fields [`fire_transitions`] diffs, so the edge announces
        // itself as `UNIT_HEALTH` + the power event — the very pair the reference's own dynamic-
        // flags watcher fires there (`0x6004c5`, `0x6004f0`). Nothing else about the unit moved,
        // which is why routing the flag through the getters is enough: no extra watcher needed.
        assert_ne!(alive.health, feigning.health);
        assert_ne!(alive.power, feigning.power);
        assert_eq!(
            (alive.max_health, alive.max_power, alive.level),
            (feigning.max_health, feigning.max_power, feigning.level),
        );
    }

    /// The rank getter's two gates (`0x605620`, decision 0782) as they reach a snapshot. The pet
    /// gate is the interesting half: a charmed or enslaved elite reports rank 0, so it loses its
    /// dragon border AND its ELITE tooltip word AND (at rank 3) its world-boss skull together.
    /// Asserted through `enrich_unit` rather than `gated_rank` directly, because the thing that
    /// can regress is the *wiring* — a future edit reaching for `rec.rank` again.
    #[test]
    fn the_rank_gate_zeroes_a_pet_or_charm() {
        use benilla_protocol::messages::ObjectFields;

        const ENTRY: u32 = 12397; // Ol' Sooty, a rank-1 elite
        const GUID: u64 = (0xF130u64 << 48) | ((ENTRY as u64) << 24) | 0x42;
        /// `UNIT_FIELD_PETNUMBER` — absolute descriptor index (`OBJECT_END(6) + 0x85`).
        const PETNUMBER: u16 = 139;

        let mut names = NameCache::default();
        names.insert_creature(
            ENTRY,
            Some(crate::names::CreatureRecord {
                name: "Ol' Sooty".into(),
                subname: None,
                creature_type: 1,
                pet_family: 4, // Bear — a real tameable family, so the record is a plausible one
                rank: 1,
                type_flags: 0,
                civilian: false,
                racial_leader: false,
            }),
        );

        let rank_of = |fields: &[(u16, u32)]| {
            let store = ObjectStore(ObjectFields::from_pairs(fields));
            let mut s = UnitState::default();
            enrich_unit(&mut s, GUID, &names, &store, None, None);
            s.rank
        };

        assert_eq!(rank_of(&[]), 1, "a free elite keeps its template rank");
        assert_eq!(
            rank_of(&[(PETNUMBER, 0)]),
            1,
            "an explicit zero pet number is not a pet"
        );
        assert_eq!(
            rank_of(&[(PETNUMBER, 7)]),
            0,
            "a non-zero pet number forces rank 0 — no dragon on an enslaved elite"
        );

        // The record gate, the getter's other half: no cached template at all → rank 0, and the
        // border stays plain until the creature query answers.
        let store = ObjectStore(ObjectFields::from_pairs(&[]));
        let mut s = UnitState::default();
        enrich_unit(&mut s, GUID ^ (1 << 24), &names, &store, None, None);
        assert_eq!(s.rank, 0, "an un-queried creature has no classification");
    }

    /// The PvP-preference announcement law (decision 0652), as the reference's changed-bits handler
    /// runs it: silent on first sight, one pair per real edge, and the OFF text is the one that
    /// explains the five-minute wait — the whole reason the toggle doesn't read as dead.
    #[test]
    fn pvp_announcement_speaks_only_on_an_edge() {
        assert_eq!(
            pvp_announcement(None, false),
            None,
            "first sight, unflagged"
        );
        assert_eq!(pvp_announcement(None, true), None, "first sight, flagged");
        assert_eq!(pvp_announcement(Some(true), true), None, "no change");
        assert_eq!(pvp_announcement(Some(false), false), None, "no change");

        let (toast, verbose) = pvp_announcement(Some(false), true).expect("turning it on speaks");
        assert_eq!(toast, "PvP combat toggled on"); // GlobalStrings ERR_PVP_TOGGLE_ON
        assert_eq!(
            verbose,
            "You are now flagged for PvP combat and will remain so until toggled off."
        );

        let (toast, verbose) = pvp_announcement(Some(true), false).expect("turning it off speaks");
        assert_eq!(toast, "PvP combat toggled off"); // GlobalStrings ERR_PVP_TOGGLE_OFF
        assert!(
            verbose.contains("five minutes"),
            "the OFF sentence is what tells the player the flag lingers: {verbose}"
        );
    }
}
