//! The app-side **unit snapshot + event feed** (decision 0068 §3): the bridge that turns live ECS
//! game state into the plain data the engine-free `Unit*` Lua bindings read, and into the WoW events
//! that drive a frame's `OnEvent`.
//!
//! The architecture is deliberate (decisions 0006/0061): the Lua game-state API must **not** reach
//! into the ECS. Instead this runs each frame, *before* the VM's tick/event dispatch
//! ([`crate::ui_script::UiInput`]), and pushes a [`UnitState`] snapshot for each unit token into the
//! VM via [`UiScript::set_unit`]. The `"player"` token reads our own avatar's [`ObjectStore`] (tagged
//! [`SelfPlayer`]); `"target"` reads the [`Selection`]'s entity. Both are found by their ECS entity,
//! not by re-deriving a guid — the ECS already owns the guid↔entity map. `"targettarget"` is the one
//! token here that IS reached by a guid: the target's own `UNIT_FIELD_TARGET`, looked up in the same
//! index (decision 1576).
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

use benilla_ui::script::{power_token, ScriptValue, UiScript, UnitState, WornDisplay};

use crate::names::NameCache;
use crate::net::{Guid, NetCommands, ObjectStore, Reputations, SelfPlayer};
use crate::target::{ring_reaction, Factions, Selection};
use crate::ui_chat::{ChatEvent, ChatEventKind, ChatLog};
use crate::ui_script::{gate, UiInput};

/// The feed pass — runs **after [`benilla_world::schedule::WorldStage::Net`]** (the feeds snapshot state
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

/// The feed's change-tracking memory: what we last told the VM, plus one server-side log-once.
///
/// The VM half lives behind a [`crate::ui_script::VmMemo`], **inside the resource** — the same
/// law 1290 wrote for `Local` memos, reached the way a `ResMut` system has to reach it: a memory
/// about what THIS VM was told is unreadable against the next VM, so a `/reload` (1291) — which
/// replaces the VM without despawning the world — re-fires `PLAYER_ENTERING_WORLD` and re-runs
/// every transition diff exactly as a fresh login does. Before this, every one of these fields
/// survived the reload and the new VM never heard the events (the logout path re-armed off the
/// self descriptor despawning, which a reload never does).
#[derive(Resource, Default)]
struct UnitFeedState {
    /// What we last told the VM — dies with the VM it was told to.
    vm: crate::ui_script::VmMemo<UnitFeedMemo>,
    /// Whether we have already warned that our own faction template names no side (decision 0657).
    /// Re-arms when a side resolves again, so a `.gm on` / `.gm off` cycle logs once each way.
    /// **Server memory, not VM memory** — deliberately outside the memo: a `/reload` must not
    /// re-log the GM-mode warning.
    warned_sideless: bool,
}

/// The per-VM half of [`UnitFeedState`] — the event-trigger diffs.
#[derive(Default)]
struct UnitFeedMemo {
    /// The gate's counter memories (1439) — the two lazy caches this feed resolves through
    /// (their per-frame `&mut` misses poison `is_changed`, the counters carry the landings).
    names_generation: gate::Watch,
    guild_generation: gate::Watch,
    /// Whether `PLAYER_ENTERING_WORLD` has been fired (once per world entry, once per VM).
    entered_world: bool,
    /// Per token, the last snapshot we pushed — the per-field event triggers diff against it.
    last: HashMap<String, UnitState>,
    /// The last selection guid, for the `PLAYER_TARGET_CHANGED` trigger.
    target_guid: Option<u64>,
    /// The last `(PLAYER_XP, PLAYER_NEXT_LEVEL_XP)` pair pushed, for the `PLAYER_XP_UPDATE` trigger —
    /// the XP bar's feed is a player-global (like coinage), not a per-unit-token field.
    last_xp: Option<(u32, u32)>,
    /// The last `(restState, restPool, PLAYER_FLAGS)` triple pushed, for the `UPDATE_EXHAUSTION`
    /// and `PLAYER_UPDATE_RESTING` triggers — player-globals like the XP pair (decisions
    /// 1082/1087). The whole flags dword, not just the resting bit: the client's `0x5ee990`
    /// fires `PLAYER_UPDATE_RESTING` on any PLAYER_FLAGS delta. Pushed as one snapshot so
    /// `GetRestState`/`GetXPExhaustion`/`IsResting` never read it half-updated.
    last_rest: Option<(u8, u32, u32)>,
    /// Our avatar's last-seen `UNIT_FIELD_LEVEL`, for the `PLAYER_LEVEL_UP` trigger (decision
    /// 1094). `None` until first seen — the first sighting is the login descriptor, not a ding.
    last_level: Option<u32>,
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
    /// The self unit's last `(HIDE_HELM, HIDE_CLOAK)` pair, for the worn-display push (decision
    /// 1472). Pushed on the **edge** and never per frame: the Options row's setter flips the VM's
    /// belief optimistically and the server's answer is a round trip away, so a per-frame push
    /// would snap the value back to the stale descriptor in between.
    worn_hidden: Option<(bool, bool)>,
    /// The self player's last-pushed `PLAYER_FIELD_BYTES` byte 2 — the four extra bars' visibility
    /// (wow-re `action-bar-toggles.md`). A player-global like the combo pair, pushed on the edge.
    /// `None` until first seen; there is no event to fire on it, because the real client registers
    /// no field-change callback anywhere near this offset.
    action_bar_toggles: Option<u8>,
}

/// Adds the per-frame unit feed. The `Unit*` bindings themselves live in `benilla-ui`; this only
/// supplies their data (and the events) from ECS state.
pub(crate) struct UiUnitPlugin;

impl Plugin for UiUnitPlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(
            Update,
            UnitFeed
                .after(benilla_world::schedule::WorldStage::Net) // the set's own doc: the why
                // …and never before the in-game UI exists (1348). The whole SET, not just
                // `feed_units`: every feed in it either fires a login one-shot or latches a
                // per-VM memo, and both are lost forever against the boot VM. The window and
                // the reference's own ordering: `ui_script::ingame_ui_pending`.
                .run_if(bevy::ecs::schedule::common_conditions::not(
                    crate::ui_script::ingame_ui_pending,
                )),
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
        .add_systems(Update, drain_pvp_toggles.after(UiInput))
        .add_systems(Update, drain_worn_display_toggles.after(UiInput))
        .add_systems(Update, drain_action_bar_toggles.after(UiInput))
        .add_systems(Update, feed_default_language.in_set(UnitFeed))
        // `load_exhaustion_rows` pushes into the VM, so it runs per VM in `Update` (1290);
        // `load_default_languages` only builds a Bevy resource and stays a one-shot.
        .add_systems(Update, load_exhaustion_rows)
        .add_systems(PostStartup, load_default_languages);
    }
}

/// The race → default-chat-language join, loaded once ([`benilla_formats::DefaultLanguages`]).
/// Absent when the tables would not load — `GetDefaultLanguage()` then answers the reference's own
/// zero-values shape rather than a made-up word.
#[derive(Resource)]
pub(crate) struct DefaultLanguagesRes(pub(crate) benilla_formats::DefaultLanguages);

/// Load `Languages.dbc` × `ChrRaces.dbc` once at startup ([`load_exhaustion_rows`]'s shape).
fn load_default_languages(
    mut commands: Commands,
    assets: Option<Res<benilla_assets::WorldAssets>>,
) {
    let Some(assets) = assets else { return };
    let loaded = {
        use benilla_assets::LockRecover;
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_default_languages(&mut chain)
    };
    match loaded {
        Ok(langs) => {
            info!("ui_unit: {} race → default-language rows", langs.len());
            commands.insert_resource(DefaultLanguagesRes(langs));
        }
        // Not fatal: the binding's contract already has an answer for "no table".
        Err(e) => warn!("ui_unit: default languages unavailable — {e:#}"),
    }
}

/// Push `GetDefaultLanguage()`'s one string, on change only.
///
/// The reference resolves it per call from the live player object; we resolve it once per race
/// change, which is the same answer with none of the per-frame churn — a race cannot change
/// without a new world entry. `None` (no player object, or no table) is the reference's zero-value
/// state, and that is what the VM stores.
///
/// **The locale column is 0.** `[0xc0e080]` is the client's locale slot; only enUS is populated in
/// the 5875 data, and every other DBC catalog here reads column 0 for the same reason.
fn feed_default_language(
    script: Option<NonSendMut<UiScript>>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    langs: Option<Res<DefaultLanguagesRes>>,
    mut pushed: Local<crate::ui_script::VmMemo<Option<Option<String>>>>,
) {
    let Some(mut script) = script else {
        return;
    };
    let pushed = pushed.get(&script);
    let name = self_q
        .iter()
        .next()
        .and_then(|store| store.0.unit_race())
        .zip(langs.as_ref())
        .and_then(|(race, langs)| langs.0.name(u32::from(race), 0))
        .map(str::to_string);
    if pushed.as_ref() != Some(&name) {
        script.set_default_language(name.clone());
        *pushed = Some(name);
    }
}

/// Seed the VM's Exhaustion.dbc table once per VM — the rest bindings' data
/// ([`benilla_ui::script::UiScript::set_exhaustion_rows`]; the ui_macro icon-catalog shape).
/// A failed load keeps the model's shipped-table fallback, so the rest surface still behaves
/// like the shipped enUS client rather than going dark.
///
/// Per VM rather than per process (1290) for the reason every seed here is: a login builds a fresh
/// VM, and this one degrades quietly — the fallback table is close enough that nobody would notice
/// it had stopped being seeded.
fn load_exhaustion_rows(
    script: Option<NonSendMut<UiScript>>,
    assets: Option<Res<benilla_assets::WorldAssets>>,
    mut seeded: Local<crate::ui_script::VmMemo<bool>>,
) {
    let (Some(mut script), Some(assets)) = (script, assets) else {
        return;
    };
    if !seeded.claim(&script) {
        return;
    }
    let loaded = {
        use benilla_assets::LockRecover;
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_exhaustion(&mut chain)
    };
    match loaded {
        Ok(rows) => {
            info!("ui_unit: {} Exhaustion.dbc rest states", rows.len());
            script.set_exhaustion_rows(
                rows.into_iter()
                    .map(|r| (r.id as u8, r.name, f64::from(r.factor)))
                    .collect(),
            );
        }
        Err(e) => error!("ui_unit: Exhaustion.dbc failed — shipped-table fallback holds: {e:#}"),
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

/// A playable race id → `UnitFactionGroup`'s token (`"Alliance"`/`"Horde"`), or `None` for a race
/// id that is not one of the eight.
///
/// **A side derived from the RACE, not from the faction template** — deliberately, and only for
/// the one window in which the template does not exist yet. [`faction_group`] is the real answer
/// and reads `UNIT_FIELD_FACTIONTEMPLATE` off the descriptor; during world entry there is no
/// descriptor, and `UnitFactionGroup("player")` answering nil there is not "no faction", it is a
/// state a real player character cannot be in. AceDB-2.0 — embedded across a large slice of the
/// corpus — builds its per-realm key as `realm .. " - " .. faction` at **file scope**, so a nil
/// side is 24 corpus addons stopping on `attempt to concatenate local 'faction'`
/// (`addon_harness::seat_a_session`, decision 1195, which seats exactly this in the survey's VM).
///
/// Every playable race has a fixed side in 1.12, so this is a lookup rather than a guess — and it
/// reads the [`crate::char_create::ALLIANCE`] column the create screen already keeps, so the two
/// cannot drift apart.
pub(crate) fn race_faction_group(race: u8) -> Option<&'static str> {
    if !(1..=8).contains(&race) {
        return None;
    }
    Some(if crate::char_create::ALLIANCE.contains(&race) {
        "Alliance"
    } else {
        "Horde"
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
        // `UnitIsCharmed 0x516cf0` — `UNIT_FIELD_CHARMEDBY != 0`, the same field `ui_aura`'s
        // charmed-unit buff leg already reads, so the two cannot disagree about who is charmed.
        charmed: store.0.unit_charmed_by().is_some(),
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
        // `UnitAffectingCombat 0x517e10` — the SAME `UNIT_FIELD_FLAGS` word, bit 19
        // (`shr ecx,0x13; test cl,1`). One flag for every token: wow-re's whole-image census of
        // that idiom found the local-player readers reading this identical bit, so there is no
        // player-specific combat latch to model beside it.
        in_combat: store.0.unit_flags() & crate::player::UNIT_FLAG_IN_COMBAT != 0,
        // Free-for-all PvP (decision 0646 §1): `PLAYER_FLAGS` bit 7, the same field the ghost
        // predicate above reads (vmangos `Player.h:322` `PLAYER_FLAGS_FFA_PVP`, cross-read against
        // 0633's byte-level `[+0xe68]+8` bit-7). Zero on creatures, which have no PLAYER_FLAGS —
        // and `UnitIsPVPFreeForAll` is false for them in the reference too.
        is_pvp_ffa: store.0.player_flags() & 0x80 != 0,
        // The unit's CURRENT honor rank (decision 1512): `PLAYER_BYTES_3` byte 3, on the internal
        // 0..=18 scale. It sits in `snapshot` — not in the caller's guid-keyed enrichment like
        // `faction_group` — because it needs nothing but the descriptor, and because it is PUBLIC:
        // the server streams it for every player in view, which is the only reason the inspect
        // pane's `UnitPVPRank("target")` can answer at all. A creature has no PLAYER block and
        // reads 0, the reference's own answer for one.
        pvp_rank: store.0.player_pvp_rank().unwrap_or(0),
        // `PLAYER_BYTES_3` byte 2 — the city-protector title, the same PUBLIC dword as the rank
        // byte above. `UnitPVPName` appends a `PVP_MEDAL<n>` line for a non-zero one; 0 is "no
        // medal", which is every character on this server (vmangos never writes the byte).
        pvp_medal: store.0.player_pvp_medal().unwrap_or(0),
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
/// state is one remembered bit ([`UnitFeedMemo::pvp_desired`]), which lives with the feed's other
/// self-flag edge (`in_combat`) rather than in a plugin of its own.
fn drain_pvp_toggles(script: Option<NonSendMut<UiScript>>, commands: Res<NetCommands>) {
    let Some(mut script) = script else {
        return;
    };
    for _ in 0..script.take_pvp_toggles() {
        let _ = commands.0.send(crate::net::ClientCommand::TogglePvp);
    }
}

/// Drain the `ShowHelm`/`ShowCloak` flips into `CMSG_TOGGLE_HELM`/`CMSG_TOGGLE_CLOAK` (decision
/// 1472) — the Options window's two equipment-display rows, and the only callers there are.
///
/// The VM has already decided *whether* a flip is needed (the setter compares the asked-for state
/// against the belief it holds and queues nothing when they agree), because only the VM knows what
/// the row just did optimistically. This end is the pure send, the PvP drain's shape exactly.
fn drain_worn_display_toggles(script: Option<NonSendMut<UiScript>>, commands: Res<NetCommands>) {
    let Some(mut script) = script else {
        return;
    };
    for which in script.take_worn_display_toggles() {
        let _ = commands.0.send(match which {
            WornDisplay::Helm => crate::net::ClientCommand::ToggleHelm,
            WornDisplay::Cloak => crate::net::ClientCommand::ToggleCloak,
        });
    }
}

/// Drain the `SetActionBarToggles` posts into `CMSG_SET_ACTIONBAR_TOGGLES` (wow-re
/// `system/ui/scratch/action-bar-toggles.md` §3) — the Options window's four extra-bar rows, and
/// the only callers there are.
///
/// **Every queued call becomes a packet**, with no did-it-change gate and no coalescing: the real
/// binding has neither (unlike `ShowHelm`/`ShowCloak`, which send only on a difference), so two
/// calls in a frame are two sends. The byte is absolute rather than a flip, so a duplicate is
/// harmless — but dropping one would be an optimisation the reference does not make, and the
/// server is the only store this preference has.
fn drain_action_bar_toggles(script: Option<NonSendMut<UiScript>>, commands: Res<NetCommands>) {
    let Some(mut script) = script else {
        return;
    };
    for toggles in script.take_action_bar_toggle_sends() {
        let _ = commands
            .0
            .send(crate::net::ClientCommand::SetActionBarToggles { toggles });
    }
}

/// `PLAYER_FLAGS_PVP_DESIRED` — the PvP *preference* bit (vmangos `PlayerDefines.h`), which is what
/// `CMSG_TOGGLE_PVP` flips. Not to be confused with `UNIT_FIELD_FLAGS`' `PVP` bit `0x1000`, the flag
/// the icon draws: the preference clears instantly, the flag lingers for the server's timer.
const PLAYER_FLAGS_PVP_DESIRED: u32 = 0x200;

/// `PLAYER_FLAGS_RESTING` — inside a rest area now (vmangos `Player.h:320`); the bit
/// `IsResting 0x516ea0` tests (`shr 5; test 1` — wow-re rested-xp-bindings.md §3).
const PLAYER_FLAGS_RESTING: u32 = 0x20;

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

/// The rest-state chat line (decision 1098; wow-re rested-xp-bindings.md §§6-10, byte-verified
/// §5): the rest-state BYTE watcher `0x5de4e0` messages only on a real old≠new transition (the
/// dispatcher's `rep cmpsb` mirror diff at `0x4655bb`), through the hard-coded 3×2 pair table
/// `0x80af50` — state 1 → `ERR_EXHAUSTION_RESTED`, state 2 → `ERR_EXHAUSTION_NORMAL`, state 0 →
/// the table's deliberate no-message sentinel (id 0x1d1), states ≥ 3 gated off before the table
/// (`cmp esi,3; jae`), so the beta tiers never speak even though their strings ship. The line is
/// a plain yellow SYSTEM chat message (`CHAT_MSG_SYSTEM`) — never UIErrorsFrame, no sound.
/// enUS literals like every app-side chat line (`level_up_lines`' shape); the keys above are the
/// GlobalStrings homes. Entering rested also arms a one-shot tutorial popup (id 0x19) — the
/// tutorial system isn't built, a named cut.
fn rest_state_message(prev: u8, new: u8) -> Option<&'static str> {
    if prev == new {
        return None;
    }
    match new {
        1 => Some("You feel rested."),
        2 => Some("You feel normal."),
        _ => None,
    }
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
        // The shipped `CreatureType.dbc` is **1..11 dense**, not 1..9 — this table stopped two
        // rows early. 11 is reachable and wow-re's own nameplate filter tests for it
        // (`0x605570 == 0xb`).
        11 => "Totem",
        // 10 is "Not specified" in the DBC. Deliberately still None: this word is the tooltip's
        // level-line class slot and a literal "Not specified" there is noise. The cost is named
        // rather than hidden — `UnitCreatureType` shares this field, so it answers nil for a
        // type-10 unit where the reference answers the DBC word.
        _ => return None,
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
    // The guid -> entity map the `"targettarget"` hop lands in, the same index `/assist` resolves
    // its basis' `UNIT_FIELD_TARGET` through (`target::by_name`). `Option` because it belongs to
    // `NetPlugin` (net.rs's `init_resource`) and this feed does not: a UI-only harness runs this
    // system with no net stack at all, and a bare `Res` turns that into a system-validation panic
    // rather than a token that names nobody. Same shape as `factions` below, same reason.
    index: Option<Res<crate::net::GuidIndex>>,
    changed_stores: Query<(), Changed<ObjectStore>>,
    mut removed_stores: RemovedComponents<ObjectStore>,
    mut feed: ResMut<UnitFeedState>,
    mut names: ResMut<NameCache>,
    commands: Res<NetCommands>,
    factions: Option<Res<Factions>>,
    reputations: Res<Reputations>,
    group: Res<crate::ui_party::GroupState>,
    mut chat: ResMut<ChatLog>,
    // The guild-identity cache `GetGuildInfo(unit)` reads — `ResMut` because it is a LAZY cache
    // (decision 1257): a lookup that misses is what sends the `CMSG_GUILD_QUERY`, exactly as a
    // `NameCache::resolve` miss sends the name query above.
    mut guild: ResMut<crate::ui_guild::GuildState>,
) {
    let Some(mut script) = script else {
        return;
    };
    // One reborrow so the memo (`feed.vm`) and `feed.warned_sideless` can be borrowed as the
    // disjoint fields they are — through the `ResMut` deref they would alias.
    let feed = &mut *feed;
    let (memo, vm_reset) = feed.vm.get_reset(&script);

    // The gate (1439): every input the two snapshots and the edge diffs below read — any
    // descriptor change or DESPAWN (a removed store is invisible to `Changed`), the selection,
    // the group/reputation/faction state, and the two lazy caches by their landed counters.
    let names_moved = memo.names_generation.moved(names.generation());
    let guild_moved = memo.guild_generation.moved(guild.identity_generation());
    let selection_changed = selection.is_changed();
    let stores_changed = !changed_stores.is_empty();
    let stores_removed = !removed_stores.is_empty();
    let group_changed = group.is_changed();
    let reps_changed = reputations.is_changed();
    let factions_changed = factions.as_ref().is_some_and(|r| r.is_changed());
    gate::trace(
        "feed_units",
        &[
            ("vm_reset", vm_reset),
            ("names", names_moved),
            ("guild", guild_moved),
            ("selection", selection_changed),
            ("stores", stores_changed),
            ("removed", stores_removed),
            ("group", group_changed),
            ("reputations", reps_changed),
            ("factions", factions_changed),
        ],
    );
    let gate = gate::Gate::new(
        vm_reset
            || names_moved
            || guild_moved
            || selection_changed
            || stores_changed
            || stores_removed
            || group_changed
            || reps_changed
            || factions_changed,
    );
    removed_stores.clear();
    if gate.skip() {
        return;
    }

    // "player" = our own avatar's descriptor; "target" = the selected entity's. Absent → None, which
    // set_unit clears (UnitExists false), exactly as the real client reports a missing unit. Names
    // resolve through the cache — a miss queries the server once and lands on a later frame.
    let self_pair = self_q.iter().next();
    let player = self_pair.map(|(store, guid)| {
        let name = names.resolve(guid.0, &commands).map(str::to_string);
        let mut s = snapshot(store, name, 0);
        s.is_player = true;
        // The stated `is_connected` gap, closed for every token this feed pushes — the field's own
        // doc names the feed as what must set it ("mirroring `exists`"), and a token we push at all
        // is one whose descriptor is streamed. It was reachable, not academic: `GREY_ROW["WHISPER"]`
        // (UnitPopup.xml) greys on `not UnitIsConnected(unit)`, so Whisper sat greyed in the
        // right-click menu of every player you targeted. What stays out of reach is real link-death
        // — the only wire that carries it is the group roster's status byte, which the party feed
        // already reads for its own tokens; a non-group player logging out lingers here as
        // connected until that lands.
        s.is_connected = true;
        // Identity + the raid-target board mark (decision 0434 §5's popup gating; §6's board).
        s.guid = guid.0;
        s.raid_target = group.raid_target_index(guid.0);
        s.faction_group = faction_group(store, factions.as_deref());
        // `GetGuildInfo("player")` — the unit's own PUBLIC descriptor fields (191/192) joined
        // against the app's guild-identity cache, which the miss also asks for (decision 1257).
        // Filled here rather than in `snapshot` for the reason `faction_group` and `can_attack`
        // are: it needs a resource, and `snapshot`'s other six call sites hold none.
        s.guild = crate::ui_guild::unit_guild(&store.0, &mut guild, &commands);
        s
    });
    // The GM-mode confound, made audible (decision 0657). A self player whose faction template
    // names no side is almost always GM mode: vmangos swaps a GM to faction template 35, whose
    // `FactionTemplate.dbc` group mask is 0, and every faction-derived surface then has no side to
    // draw — the PvP flag icon most visibly, which simply hides. That is faithful (the only side
    // art that ships is Alliance/Horde/FFA), and it is indistinguishable on screen from the icon
    // being broken, which has now cost two separate sessions an investigation. So it is a BENCH
    // diagnostic, not UI: nothing appears on screen, exactly as in the reference.
    //
    // The honor arc (1512) put a SECOND surface behind this same side: a rank's title is the
    // GlobalString `PVP_RANK_<rank>_<team>`, and with no side there is no team digit, so
    // `GetPVPRankInfo` answers nil and the Honor tab renders `NONE` at every rank — for a Grand
    // Marshal. That reads exactly like an unbuilt pane, which is why it is named in the warning
    // rather than left for the next investigation to rediscover.
    if let Some(p) = &player {
        let sideless = p.faction_group.is_none();
        if sideless && !feed.warned_sideless {
            warn!(
                "faction: our own template names no side (usually GM mode — vmangos forces \
                 template 35, group mask 0). Faction-derived UI cannot resolve a side while this \
                 holds: the PvP flag icon stays hidden however flagged you are, and the Honor tab's rank \
                 title reads NONE at every rank (PVP_RANK_<rank>_<team> has no team digit). \
                 `.gm off` restores both."
            );
        }
        feed.warned_sideless = sideless;
    }
    // (The VM-half memo was taken at the top — the gate needs its reset flag. `warned_sideless`
    // stays server memory outside it, which the disjoint field borrows above preserve.)
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
        s.is_connected = true; // see the player leg
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
        // `GetGuildInfo("target")` — see the player leg. PLAYER_GUILDID/RANK are PUBLIC, which is
        // the whole reason the binding is per-unit rather than per-player.
        s.guild = crate::ui_guild::unit_guild(&store.0, &mut guild, &commands);
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

    // `"targettarget"` — the target's own target, one hop off its `UNIT_FIELD_TARGET`. It is the
    // same read `/assist` runs (`target::by_name`), and the same shape 1.12's own token resolver
    // walks for the `target` SUFFIX: resolve the base unit, then follow the guid its target field
    // carries. Only a STREAMED guid resolves — an unstreamed one leaves `UnitExists("targettarget")`
    // false, exactly as an out-of-range party member is false.
    //
    // No `guild` leg, deliberately: `unit_guild` is the LAZY cache whose miss SENDS a
    // `CMSG_GUILD_QUERY` (1257), and nothing asks a target-of-target for its guild — the party and
    // raid tokens leave it unfilled for the same reason. Everything else here is a pure read of a
    // descriptor we already hold.
    let tot = selection
        .target
        .and_then(|e| stores.get(e).ok())
        .and_then(|s| s.0.unit_target())
        .filter(|guid| *guid != 0)
        .and_then(|guid| Some((*index.as_ref()?.0.get(&guid)?, guid)))
        .and_then(|(entity, guid)| {
            let store = stores.get(entity).ok()?;
            let name = names.resolve(guid, &commands).map(str::to_string);
            let reaction = ring_reaction(
                factions.as_deref(),
                &reputations,
                Some(store),
                self_pair.map(|(s, _)| s),
            ) + 1;
            let mut s = snapshot(store, name, reaction);
            s.guid = guid;
            s.is_connected = true; // see the player leg
            s.raid_target = group.raid_target_index(guid);
            s.faction_group = faction_group(store, factions.as_deref());
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

    // `"player"` is pushed only while the descriptor EXISTS: its absence is "no data source",
    // never "the player stopped existing". The two absent windows are pre-arrival at login —
    // where a `None` push would erase the roster seat (`seat_from_roster`) that addon file scopes
    // and the loading-screen UI read — and the logout despawn frames, where `PLAYER_LOGOUT`
    // handlers still key their saved state by `UnitName("player")` (the reference keeps the unit
    // valid through its shutdown; a fresh VM starts with no `"player"` anyway, so nothing needs
    // the clear). `"target"` keeps the unconditional push: a selection's absence IS data — the
    // deselect/despawn transition the real client also reports.
    // Both pushes diff against the SAME memo the event loop below uses (1439): an identical
    // snapshot re-pushed is invisible to the VM, so only a real change pays the clone.
    if let Some(cur) = &player {
        if memo.last.get("player") != Some(cur) {
            gate.audit("feed_units", "the player snapshot");
            script.set_unit("player", player.clone());
        }
    }
    let target_dirty = match (&target, memo.last.get("target")) {
        (Some(cur), Some(prev)) => cur != prev,
        (None, None) => false,
        _ => true,
    };
    if target_dirty {
        gate.audit("feed_units", "the target snapshot");
        script.set_unit("target", target.clone());
    }
    // The same "absence IS data" push as the target's above: the target dropping ITS target is a
    // transition the frame has to hear about, and clearing the token is how it hears it.
    let tot_dirty = match (&tot, memo.last.get("targettarget")) {
        (Some(cur), Some(prev)) => cur != prev,
        (None, None) => false,
        _ => true,
    };
    if tot_dirty {
        gate.audit("feed_units", "the target-of-target snapshot");
        script.set_unit("targettarget", tot.clone());
    }

    // The XP bar's feed: push our own avatar's PLAYER_XP / PLAYER_NEXT_LEVEL_XP (both PRIVATE, only
    // ever streamed for self) and fire PLAYER_XP_UPDATE when either changes — the coinage feed's
    // shape. Absent fields read 0 (a fresh descriptor's zero default; the bar shows empty until XP
    // streams in). BEFORE the PLAYER_ENTERING_WORLD fire below, so the first paint reads real
    // values (1087 — the tick's handler divides by UnitXPMax).
    if let Some((store, _)) = self_q.iter().next() {
        let xp = store.0.player_xp().unwrap_or(0);
        let next = store.0.player_next_level_xp().unwrap_or(0);
        if memo.last_xp != Some((xp, next)) {
            gate.audit("feed_units", "the XP pair");
            memo.last_xp = Some((xp, next));
            script.set_player_xp(xp, next);
            script.fire_event("PLAYER_XP_UPDATE", vec![]);
        }
    }

    // The rest feed (decisions 1082/1087): the `PLAYER_BYTES_2` rest-state byte, the
    // `PLAYER_REST_STATE_EXPERIENCE` pool and PLAYER_FLAGS, pushed as one snapshot. Two watches,
    // the byte-verified grain (wow-re rested-xp-bindings.md §5): `UPDATE_EXHAUSTION` on a
    // state-or-pool change (the client installs a watcher on each — `0x5de4e0` on the byte,
    // `0x5de4b0` on the pool field), `PLAYER_UPDATE_RESTING` on **any PLAYER_FLAGS delta**
    // (`0x5ee990` fires it beside PLAYER_FLAGS_CHANGED without testing which bit moved — the
    // resting bit is just its loudest consumer). Runs before the PLAYER_ENTERING_WORLD fire
    // below, like the XP push: in the real client the descriptor always lands before that
    // event, so the first paint reads real state — the model's byte-2 default (its doc) is the
    // backstop, this ordering is the guarantee itself.
    if let Some((store, _)) = self_q.iter().next() {
        let rest = (
            store.0.player_rest_state().unwrap_or(0),
            store.0.player_rest_state_experience().unwrap_or(0),
            store.0.player_flags(),
        );
        if memo.last_rest != Some(rest) {
            gate.audit("feed_units", "the rest snapshot");
            let prev = memo.last_rest;
            memo.last_rest = Some(rest);
            script.set_rest_state(rest.0, rest.1, rest.2 & PLAYER_FLAGS_RESTING != 0);
            if prev.map(|p| (p.0, p.1)) != Some((rest.0, rest.1)) {
                script.fire_event("UPDATE_EXHAUSTION", vec![]);
            }
            // "You feel rested." / "You feel normal." (decision 1098): the BYTE watcher alone
            // messages, and only on a real transition — `prev` None is the login descriptor,
            // which the real client's fresh-CREATE path never runs through the notify pass
            // (byte-verified: login is structurally silent). The pool watcher never messages.
            if let Some(p) = prev {
                if let Some(text) = rest_state_message(p.0, rest.0) {
                    chat.push_event(ChatEvent::text_only(
                        ChatEventKind::System,
                        text.to_string(),
                    ));
                }
            }
            if prev.map(|p| p.2) != Some(rest.2) {
                script.fire_event("PLAYER_UPDATE_RESTING", vec![]);
            }
        }
    }

    // The ding feed: `PLAYER_LEVEL_UP` (arg1 = the new level) when our avatar's
    // `UNIT_FIELD_LEVEL` CHANGES — the event the exhaustion tick (1082) and the max-level rail
    // (1094) register; it was a dead registration until 1094. Any change, not only a rise:
    // vmangos `GiveLevel` runs for demotions too (a GM `.character level` down) and sends
    // `SMSG_LEVELUP_INFO` unconditionally, so the real client hears every change — 1094's
    // rise-only guard left the rail latched shown after a 60→1 demote (the 1106 live repro).
    // Trigger PROVISIONAL (0578's pattern): fired off the descriptor diff, which lands in the
    // same update batch as the ding's XP fields, so consumers read a coherent picture. The
    // real client plausibly fires it from its `SMSG_LEVELUP_INFO` handler instead, with the
    // packet's gain tuple as arg2+ — unpinned, and no 1.12 FrameXML consumer reads past arg1
    // (`ReputationWatchBar_Update` takes arg1; the tick's handler takes none), so the extra
    // args wait for a consumer.
    if let Some((store, _)) = self_q.iter().next() {
        if let Some(level) = store.0.unit_level() {
            let prev = memo.last_level.replace(level);
            if prev.is_some_and(|p| level != p) {
                gate.audit("feed_units", "the level edge");
                script.fire_event("PLAYER_LEVEL_UP", vec![ScriptValue::Int(i64::from(level))]);
            }
        }
    }

    // The action-bar toggle feed: `PLAYER_FIELD_BYTES` byte 2 — which of the four extra bars the
    // player has switched on (wow-re `system/ui/scratch/action-bar-toggles.md`). PRIVATE, like the
    // combo byte one address down (`+0x1029` vs `+0x102a`), and pushed on the EDGE.
    //
    // **This push is the ONLY thing that moves the VM's copy**, and that is the mechanism rather
    // than our simplification: no instruction in the real client writes this cell (§4.1 — the one
    // `+0x102a` access image-wide is `GetActionBarToggles`' read), so `SetActionBarToggles` posts
    // the byte and leaves the descriptor alone until the server's UPDATE_OBJECT echoes it. Nothing
    // is notified when it lands either (§4.2: all 49 field-change registrations at `0x468070` were
    // enumerated; none sits at an offset ≥ `0x1000`), so there is **no event to fire here** — the
    // reference reads the binding exactly once, in `UIParent.lua`'s `PLAYER_ENTERING_WORLD`
    // handler, and keeps `SHOW_MULTI_ACTIONBAR_1..4` as its optimistic copy in between.
    //
    // Which is why this sits ABOVE the fire, on 1087's precedent for the XP/rest pushes: the
    // handler that reads `GetActionBarToggles()` runs synchronously inside `fire_event`, so a push
    // below it would hand the first-paint the previous frame's value — four nils.
    //
    // `unwrap_or(0)` is faithful, not a shrug: with no local player the reference's chain fails
    // soft and the getter returns four `nil`s, which is exactly what a zero byte returns — "not in
    // world" and "byte == 0" share the branch and are indistinguishable to Lua (§5).
    if let Some((store, _)) = self_q.iter().next() {
        let toggles = store.0.player_action_bar_toggles().unwrap_or(0);
        if memo.action_bar_toggles != Some(toggles) {
            gate.audit("feed_units", "the action-bar toggle byte");
            memo.action_bar_toggles = Some(toggles);
            script.set_action_bar_toggles(toggles);
        }
    }

    // Initial pull: fire PLAYER_ENTERING_WORLD once PER WORLD ENTRY so frames do their first
    // paint on their own — gated on our avatar's descriptor EXISTING. 1087 stated the real
    // client's guarantee (the player object lands before this event) and moved the XP/rest
    // pushes above the fire, but the fire itself still went out on frame 1, seconds before
    // login: every one-shot first-paint read empty state, and only consumers with their own
    // diff events recovered. The 1094 live probe caught the one that couldn't — a level-60
    // login read UnitLevel()=0 at the fire and kept the XP strip. With the gate the guarantee
    // is structural for every consumer.
    //
    // The absent arm is the world-EXIT edge (logout / char switch — the self entity despawns
    // with the streamed world): the real client fires this event on *every* world entry, and
    // since 1290 so do we — the frame tree is torn down and rebuilt across that edge, the way
    // the reference does it. So re-arm the fire and forget the player-global diff
    // memories. Forgetting them makes every next-login first sighting re-seed SILENTLY — the
    // byte-verified fresh-CREATE notify silence (1098 §4), now holding per entry: without it a
    // 60→1 char switch latched the max-level rail shown over a level-1 body (1106's live
    // repro), and a normal→rested char switch would misfire "You feel rested." at login.
    if self_pair.is_some() {
        if !memo.entered_world {
            gate.audit("feed_units", "the PLAYER_ENTERING_WORLD arm");
            script.fire_event("PLAYER_ENTERING_WORLD", vec![]);
            memo.entered_world = true;
        }
    } else if memo.entered_world {
        gate.audit("feed_units", "the world-exit disarm");
        memo.entered_world = false;
        memo.last_xp = None;
        memo.last_rest = None;
        memo.last_level = None;
        memo.last_combo = None;
        memo.in_combat = None;
        memo.pvp_desired = None;
        // The worn-display pair is a player-global like the rest, and forgetting it is what makes
        // the next character's preference reach the VM at all: the push is an EDGE, so a memo
        // carrying the last body's bits would silently skip a new body that happens to disagree
        // with the VM's fresh "both shown" default (decision 1472).
        memo.worn_hidden = None;
        // Same reason as the worn-display pair: the push is an EDGE, so a memo carrying the last
        // body's byte would skip a new character whose own toggles happen to match it — and this
        // one has no optimistic default to fall back on, only four nils.
        memo.action_bar_toggles = None;
    }

    for (token, snap) in [
        ("player", &player),
        ("target", &target),
        ("targettarget", &tot),
    ] {
        match snap {
            Some(cur) => {
                let prev = memo.last.get(token);
                if prev != Some(cur) {
                    gate.audit("feed_units", "a unit-token transition");
                    fire_transitions(&mut script, token, prev, cur);
                    memo.last.insert(token.to_string(), cur.clone());
                }
            }
            None => {
                // Clearing a token isn't a UNIT_* event; the target frame reacts to
                // PLAYER_TARGET_CHANGED below.
                if memo.last.remove(token).is_some() {
                    gate.audit("feed_units", "a unit-token clear");
                }
            }
        }
    }

    // PLAYER_TARGET_CHANGED (no args, real WoW's shape) when the selection changes.
    if selection.guid != memo.target_guid {
        gate.audit("feed_units", "the PLAYER_TARGET_CHANGED edge");
        memo.target_guid = selection.guid;
        script.fire_event("PLAYER_TARGET_CHANGED", vec![]);
    }

    // PLAYER_REGEN_DISABLED/ENABLED: the self in-combat flag transition (`UNIT_FIELD_FLAGS`
    // bit `UNIT_FLAG_IN_COMBAT 0x00080000`, vmangos `UnitDefines.h:564`) — the center combat
    // text's ENTERING/LEAVING_COMBAT feed (decision 0578; the trigger is PROVISIONAL pending
    // the COMBAT_TEXT_UPDATE emission pin).
    if let Some((store, _)) = self_pair {
        let in_combat = store.0.unit_flags() & 0x0008_0000 != 0;
        if memo.in_combat != Some(in_combat) {
            gate.audit("feed_units", "the combat-flag edge");
            let first_sight = memo.in_combat.is_none();
            memo.in_combat = Some(in_combat);
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
        if let Some((toast, verbose)) = pvp_announcement(memo.pvp_desired, desired) {
            gate.audit("feed_units", "the PvP-desired edge");
            script.fire_event("UI_INFO_MESSAGE", vec![ScriptValue::Str(toast.to_string())]);
            chat.push_event(ChatEvent::text_only(
                ChatEventKind::System,
                verbose.to_string(),
            ));
        }
        memo.pvp_desired = Some(desired);
    }

    // The worn-display pair (decision 1472): `PLAYER_FLAGS`' two hide bits, mirrored into the VM
    // so `ShowingHelm()`/`ShowingCloak()` — the Options rows' getters — read the server's truth.
    // On the EDGE, not per frame: the setter flips the VM's belief the instant the box is clicked,
    // and the descriptor only catches up a round trip later. Re-pushing the stale pair in between
    // would un-click the box and make a second click compute the wrong flip.
    if let Some((store, _)) = self_pair {
        let hidden = (store.0.player_hides_helm(), store.0.player_hides_cloak());
        if memo.worn_hidden != Some(hidden) {
            gate.audit("feed_units", "the worn-display pair");
            memo.worn_hidden = Some(hidden);
            script.set_worn_display(!hidden.0, !hidden.1);
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
        if let Some(fire) = combo_edge(memo.last_combo, banked) {
            gate.audit("feed_units", "the combo-point edge");
            memo.last_combo = Some(banked);
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

    /// The rest-state chat law (decision 1098, wow-re §§6-10): a message needs a real byte
    /// TRANSITION, and only states 1/2 speak — state 0 is the pair table's no-message sentinel,
    /// the beta tiers (≥3) are gated off before the table, and a re-send of the same byte is
    /// swallowed by the dispatcher's mirror diff.
    #[test]
    fn rest_state_message_speaks_only_on_a_real_transition() {
        assert_eq!(rest_state_message(2, 1), Some("You feel rested."));
        assert_eq!(rest_state_message(1, 2), Some("You feel normal."));
        assert_eq!(
            rest_state_message(0, 1),
            Some("You feel rested."),
            "0→1 IS a transition"
        );
        assert_eq!(
            rest_state_message(1, 1),
            None,
            "same byte re-sent — the mirror diff eats it"
        );
        assert_eq!(rest_state_message(2, 2), None);
        assert_eq!(
            rest_state_message(1, 0),
            None,
            "state 0 is the 0x1d1 sentinel: no message"
        );
        assert_eq!(
            rest_state_message(2, 3),
            None,
            "beta tiers are gated off (cmp esi,3; jae)"
        );
        assert_eq!(rest_state_message(1, 5), None);
    }
}
