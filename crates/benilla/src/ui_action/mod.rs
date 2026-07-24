//! The app-side **action-bar feed + use/set-drain** (decision 0068 slice 1, extended by decision
//! 0216 §7/slice 4) — the three directions of the action seam around the engine-free bindings
//! ([`benilla_ui::script`]'s `action`/`cursor::bar` modules):
//!
//! - **Inward**: the net bridge fills [`PlayerActions`] from `SMSG_INITIAL_SPELLS` +
//!   `SMSG_ACTION_BUTTONS` (and slice 4's [`drain_action_sets`] writes it directly, client-side —
//!   the bar is client-authoritative, decision 0218 §4); [`feed_actions`] resolves each occupied
//!   slot's icon (spell: Spell.dbc × SpellIcon.dbc; item: the item template chain, same ask-once
//!   store the bags use) and count (item: the bag walk, `ui_items::count_of`) and pushes the
//!   120-slot snapshot into the VM, firing `ACTIONBAR_SLOT_CHANGED` per changed slot. The identity
//!   resolve (icon/kind/action) is gated on `dirty` (a real, if occasional, event: login or a
//!   local pickup/place); the ITEM-kind count refresh AND the auto-attack's weapon icon run every
//!   frame regardless — a bag count drifts (eating a stack down) and the auto-attack icon tracks the
//!   equipped main-hand weapon (decision 0230), both without ever touching the action table itself,
//!   so gating them on the SAME flag would let the Count fontstring / the Attack icon go stale.
//!   The stance page (`GetBonusBarOffset`)
//!   is the client's own data path (wow-re byte-verified): our descriptor's shapeshift-form byte
//!   indexed into `SpellShapeshiftForm.dbc`'s BonusActionBar column — druid forms, warrior
//!   stances, and stealth all fall out of the data — firing `UPDATE_BONUS_ACTIONBAR` on change.
//! - **Outward (use)**: [`drain_action_uses`] turns queued `UseAction(n)` intents into the wire —
//!   a spell action resolves its wire target through the cast-arm law ([`cast_target`]: self
//!   spells commit targetless, friendly-required casts bind the selection or fall back to the
//!   player, an unbindable cast refuses locally), the auto-attack action (6603)
//!   sends `CMSG_ATTACKSWING` at the selection, an item action resolves to its first bag position
//!   and routes through the equip-vs-use fork (mirroring `ui_items::drain_container_uses`'s own
//!   fork); macro actions are a stated gap (no macro window yet).
//! - **Outward (set)**: [`drain_action_sets`] turns the cursor seam's queued `PickupAction`/
//!   `PlaceAction` mutations into `CMSG_SET_ACTION_BUTTON` sends, one per queued entry (0218 §4: a
//!   drag-swap is two sends, never atomic) — no server round-trip to lock against, since the app
//!   already wrote its own `PlayerActions.buttons` optimistically before sending.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use bevy::prelude::*;

use benilla_formats::{SpellCatalog, SpellDisplay};
use benilla_protocol::messages::{ActionButton, ACTION_KIND_ITEM, ACTION_KIND_SPELL};
use benilla_ui::script::{ActionSlot, ScriptValue, UiScript};

use crate::assets::{AssetSet, LockRecover, WorldAssets};
use crate::entities::ItemDisplays;
use crate::items::Items;
use crate::net::{ClientCommand, NetCommands, ObjectStore, SelfPlayer};
use crate::ui_script::UiInput;
use crate::ui_unit::UnitFeed;

mod cast_fail;
pub(crate) mod cast_target;
mod errors;
mod state;

pub(crate) use errors::{
    attack_mounted_refusal, reagent_totem_refusal, CastErrors, MountErrors, UiError, UiErrorKeys,
};
use errors::{first_missing_totem, first_short_reagent, mount_result_key, ui_error_text};
// `pub(crate)`: the stance bar's `isCastable` IS this walk (`GetShapeshiftFormInfo`'s fourth
// return runs `0x6e3d60`, wow-re shapeshift-bar-api.md) — `crate::ui_shapeshift` calls it with
// the same ctx shape the state feed builds.
pub(crate) mod usable;

/// The auto-attack pseudo-spell (`Attack`, every character's slot-1 default): not a cast — it
/// toggles melee via the attack-swing pair. The USE path (`CMSG_ATTACKSWING`) keys on this id; the
/// ICON substitution keys on the effect type instead ([`SpellDisplay::is_melee_auto_attack`],
/// decision 0231 — 6603 is simply the only spell carrying `SPELL_EFFECT_ATTACK`).
pub(crate) const SPELL_ATTACK: u32 = 6603;

/// Equipment slot 15 = `EQUIPMENT_SLOT_MAINHAND` (vmangos `EquipmentSlots`).
const EQUIPMENT_SLOT_MAINHAND: u8 = 15;

/// Equipment slot 17 = `EQUIPMENT_SLOT_RANGED` — the ranged helper `0x4e6990`'s read
/// (`[ecx+0x88]`, `0x88 = 17×8`; wow-re `attack-icon-substitution.md` §5).
const EQUIPMENT_SLOT_RANGED: u8 = 17;

/// Weapon subclass 16 = thrown — the ranged icon helper's skip (`0x4e6990`'s `0x5d9f90 == 0x10`
/// test): a thrown weapon never substitutes its icon, so Throw keeps the spell's own face.
const ITEM_SUBCLASS_THROWN: u32 = 16;

/// The client's unarmed/disarmed auto-attack icon (wow-re `attack-icon-substitution.md`, the
/// hardcoded string at `0x84bf58`) — what the melee auto-attack shows when there is no main-hand
/// weapon to borrow from, instead of spell 6603's `Temp` placeholder (decision 0231).
const SPELL_RESET_ICON: &str = "Interface\\Buttons\\Spell-Reset";

/// The equipped main-hand weapon's inventory icon (slot 15 → the item's `ItemDisplayInfo` icon,
/// the chain the bags/paper doll use). `None` when unarmed or the item hasn't streamed yet.
pub(crate) fn main_hand_weapon_icon(
    store: &ObjectStore,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
) -> Option<String> {
    let guid = store.0.player_inv_slot(EQUIPMENT_SLOT_MAINHAND)?;
    let entry = items.object(guid)?.object_entry()?;
    let display = items.template(entry, guid, commands)?.display_info_id;
    icons?.catalog.get(display)?.icon.clone()
}

/// The character's melee auto-attack icon (decision 0231; the client's melee helper `0x4e6870`):
/// the equipped main-hand weapon's icon, or [`SPELL_RESET_ICON`] when unarmed. Character-level —
/// independent of WHICH auto-attack spell (they all show this), so the spellbook can pre-resolve it
/// once for its whole page. (The verdict's ranged auto-repeat / shapeshift-form / disarmed cases
/// are deferred — see decision 0231; this covers the common melee, armed-or-unarmed case.)
pub(crate) fn melee_auto_attack_icon(
    store: &ObjectStore,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
) -> String {
    main_hand_weapon_icon(store, items, icons, commands)
        .unwrap_or_else(|| SPELL_RESET_ICON.to_string())
}

/// The equipped ranged weapon's inventory icon (slot 17 → `ItemDisplayInfo`), for the ranged
/// icon substitution (`0x4e6990`, decision 0231's deferred case — wow-re
/// `attack-icon-substitution.md` §5): a **thrown** weapon is skipped (the helper's
/// `0x5d9f90 == 0x10` test), and `None` — missing weapon, thrown, or an unstreamed item — falls
/// back to the spell's OWN icon at the caller, never `Spell-Reset` (the helper's `0x4e6a44` null
/// return hands over to the normal SpellIconID path).
pub(crate) fn ranged_weapon_icon(
    store: &ObjectStore,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
) -> Option<String> {
    let guid = store.0.player_inv_slot(EQUIPMENT_SLOT_RANGED)?;
    let entry = items.object(guid)?.object_entry()?;
    let template = items.template(entry, guid, commands)?;
    if template.subclass == ITEM_SUBCLASS_THROWN {
        return None;
    }
    let display = template.display_info_id;
    icons?.catalog.get(display)?.icon.clone()
}

/// Whether `spell` substitutes an equipped weapon's icon at all — the two resolvers' shared
/// pre-test (melee: the effect trigger; ranged: the paired attribute bits). The per-frame icon
/// refresh keys on this, so a ranged-weapon swap re-feeds Auto Shot like a main-hand swap
/// re-feeds Attack.
pub(crate) fn substitutes_weapon_icon(spell: &SpellDisplay) -> bool {
    spell.is_melee_auto_attack() || spell.ranged_icon_substitution()
}

/// The icon `spell` shows on the action bar when it substitutes an equipped weapon's
/// ([`substitutes_weapon_icon`]): the melee auto-attack shows [`melee_auto_attack_icon`]
/// (weapon or `Spell-Reset`); a ranged auto-repeat shot ([`SpellDisplay::ranged_icon_substitution`])
/// shows [`ranged_weapon_icon`]. `None` for any other spell, for a ranged shot with no
/// substitutable weapon, or when there is no character to read the weapon from — the caller uses
/// the spell's own icon.
pub(crate) fn auto_attack_icon(
    spell: &SpellDisplay,
    store: Option<&ObjectStore>,
    items: &mut Items,
    icons: Option<&ItemDisplays>,
    commands: &NetCommands,
) -> Option<String> {
    let store = store?;
    if spell.is_melee_auto_attack() {
        return Some(melee_auto_attack_icon(store, items, icons, commands));
    }
    if spell.ranged_icon_substitution() {
        return ranged_weapon_icon(store, items, icons, commands);
    }
    None
}

/// The player's action store: the occupied wire slots (0..119) + the known-spell set. Written by
/// the net bridge (`SMSG_INITIAL_SPELLS`/`SMSG_ACTION_BUTTONS`) AND, since decision 0216 §7,
/// directly by [`drain_action_sets`] (the bar is client-authoritative — a local pickup/place is
/// never echoed back by the server, vmangos `MasterPlayer::addActionButton`/`removeActionButton`
/// send nothing). Read by [`feed_actions`]/[`drain_action_uses`].
#[derive(Resource, Default)]
pub(crate) struct PlayerActions {
    /// Wire slot (0-based) → the slot's packed action. Lua action id = slot + 1.
    pub buttons: HashMap<u8, ActionButton>,
    /// The spell book (`SMSG_INITIAL_SPELLS`), for gating/consumers to come.
    pub spells: HashSet<u32>,
    /// Set on every book/bar arrival AND every local `action_sets` drain; cleared by the feed
    /// after re-resolving each slot's identity (icon/kind/action) and pushing. Gates ONLY the
    /// identity resolve — an ITEM slot's bag COUNT is refreshed unconditionally every frame
    /// instead (see the module doc): it drifts independently of this flag.
    pub dirty: bool,
}

/// The live auto-repeat spell — the client's autorepeat key `0xceac30` (wow-re `wave-cast.md`:
/// written at the local cast-send for `AttributesEx2 & 0x20` spells, cleared by
/// `SMSG_CANCEL_AUTO_REPEAT`'s `0x6ea080` and by a matching cast-fail). Distinct from the sticky
/// `creature_anim::AutoRepeatArmed` (the Load/Hold idle gate, never cleared): THIS one is what
/// `IsAutoRepeatAction` and the button flash read, and it goes out when the shooting stops.
#[derive(Resource, Default)]
pub(crate) struct AutoRepeatActive(pub Option<u32>);

/// The spell display catalog + the shapeshift bonus-bar map (absent when the client data isn't —
/// every consumer tolerates that). `pub(crate)`: the cast-visual router
/// (`crate::creature_anim::spell_visual`) resolves spell → visual through the same catalog — one
/// `Spell.dbc` load serves both faces (decision 0107).
#[derive(Resource)]
pub(crate) struct Spells {
    pub(crate) catalog: SpellCatalog,
    /// Form id → the `SpellShapeshiftForm.dbc` row: **BonusActionBar** (the client's own paging
    /// map, wow-re byte-verified: `GetBonusBarOffset` reads a cached copy of exactly this
    /// lookup) + **flags1** (the form gate's stance bit, [`state`]'s usable walk; the
    /// toggle-cancel block bit, `crate::ui_shapeshift`'s drain).
    pub(crate) forms: std::collections::HashMap<u32, benilla_formats::ShapeshiftForm>,
    /// `SpellRange.dbc` — the byte-verified `GetMinMaxRange 0x6e3480` inputs the range indicator
    /// reads ([`state`], decision 0137 phase 4). Empty when the DBC failed (range reads `None`).
    pub(crate) ranges: benilla_formats::SpellRangeCatalog,
    /// `SpellCastTimes.dbc` — the tooltip's cast-time cell (byte-verified `GetCastTime 0x6e3340`
    /// reads `CastingTimeIndex` against it; decision 0274 P2). Empty on a failed load.
    pub(crate) cast_times: benilla_formats::SpellCastTimeCatalog,
    /// `SpellDuration.dbc` — the `$d`/`$o` tokens' source (`GetDuration 0x6ea000`).
    pub(crate) durations: benilla_formats::SpellDurationCatalog,
    /// `SpellRadius.dbc` — the `$a` token's yards.
    pub(crate) radii: benilla_formats::SpellRadiusCatalog,
}

#[cfg(test)]
impl Spells {
    /// An empty catalog set — the usable walk's unit tests need only the `forms` map.
    pub(crate) fn empty_for_tests() -> Self {
        Spells {
            catalog: SpellCatalog::from_displays(HashMap::new()),
            forms: HashMap::new(),
            ranges: benilla_formats::SpellRangeCatalog::default(),
            cast_times: Default::default(),
            durations: Default::default(),
            radii: Default::default(),
        }
    }
}

/// The feed's memory of what it last pushed, for per-slot change events.
#[derive(Default)]
struct FeedMemory {
    pushed: HashMap<u32, ActionSlot>,
    bonus_offset: u8,
}

pub(crate) struct UiActionPlugin;

impl Plugin for UiActionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlayerActions>()
            .init_resource::<CastErrors>()
            .init_resource::<MountErrors>()
            .init_resource::<UiErrorKeys>()
            .init_resource::<crate::cooldowns::Cooldowns>()
            .init_resource::<AutoRepeatActive>()
            .init_resource::<cast_target::AutoSelfCast>()
            .add_systems(Startup, load_spells.after(AssetSet::Open))
            .add_systems(
                Update,
                (
                    // Feed rides with the unit feed, before the VM ticks; both drains run after
                    // the input pass so a click's UseAction/PickupAction/PlaceAction goes out the
                    // same frame. The two queues are disjoint per gesture (a checkCursor place
                    // routes entirely to `action_sets`, never also queuing a use), so the drains'
                    // relative order doesn't matter. The dynamic-state feed follows the identity
                    // feed so a fresh slot's first state push lands the same frame.
                    feed_actions.in_set(UnitFeed).before(UiInput),
                    state::feed_action_state
                        .in_set(UnitFeed)
                        .after(feed_actions)
                        .before(UiInput),
                    drain_action_sets.after(UiInput),
                    drain_action_uses.after(UiInput),
                ),
            );
    }
}

fn load_spells(mut commands: Commands, assets: Option<Res<WorldAssets>>) {
    let Some(assets) = assets else { return };
    let loaded = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_spell_catalog(&mut chain)
    };
    match loaded {
        Ok(catalog) => {
            let forms = {
                let mut chain = assets.chain.lock_recover();
                benilla_formats::load_shapeshift_forms(&mut chain).unwrap_or_else(|e| {
                    warn!("ui_action: SpellShapeshiftForm.dbc failed — stance paging off: {e:#}");
                    Default::default()
                })
            };
            let ranges = {
                let mut chain = assets.chain.lock_recover();
                benilla_formats::load_spell_ranges(&mut chain).unwrap_or_else(|e| {
                    warn!("ui_action: SpellRange.dbc failed — range indicator off: {e:#}");
                    benilla_formats::SpellRangeCatalog::default()
                })
            };
            let cast_times = {
                let mut chain = assets.chain.lock_recover();
                benilla_formats::load_spell_cast_times(&mut chain).unwrap_or_else(|e| {
                    warn!("ui_action: SpellCastTimes.dbc failed — cast-time cell off: {e:#}");
                    Default::default()
                })
            };
            let durations = {
                let mut chain = assets.chain.lock_recover();
                benilla_formats::load_spell_durations(&mut chain).unwrap_or_else(|e| {
                    warn!("ui_action: SpellDuration.dbc failed — $d/$o tokens off: {e:#}");
                    Default::default()
                })
            };
            let radii = {
                let mut chain = assets.chain.lock_recover();
                benilla_formats::load_spell_radii(&mut chain).unwrap_or_else(|e| {
                    warn!("ui_action: SpellRadius.dbc failed — $a token off: {e:#}");
                    Default::default()
                })
            };
            info!(
                "ui_action: {} spells in the display catalog, {} shapeshift forms, {} range rows, \
                 {} cast times, {} durations, {} radii",
                catalog.len(),
                forms.len(),
                ranges.len(),
                cast_times.len(),
                durations.len(),
                radii.len()
            );
            commands.insert_resource(Spells {
                catalog,
                forms,
                ranges,
                cast_times,
                durations,
                radii,
            });
        }
        Err(e) => warn!("ui_action: Spell.dbc failed to load — bar icons disabled: {e:#}"),
    }
}

#[allow(clippy::too_many_arguments)] // a Bevy system's full input set
fn feed_actions(
    script: Option<NonSendMut<UiScript>>,
    mut actions: ResMut<PlayerActions>,
    mut cast_errors: ResMut<CastErrors>,
    mut mount_errors: ResMut<MountErrors>,
    mut ui_error_keys: ResMut<UiErrorKeys>,
    spells: Option<Res<Spells>>,
    self_q: Query<&ObjectStore, With<SelfPlayer>>,
    mut items: ResMut<Items>,
    icons: Option<Res<ItemDisplays>>,
    commands: Res<NetCommands>,
    mut memory: Local<FeedMemory>,
) {
    let Some(mut script) = script else {
        return;
    };

    // Rejected casts surface as the client's red error line (UI_ERROR_MESSAGE → the errors
    // frame), resolved through the byte-verified two-layer display ([`cast_fail`]) against the
    // VM's own GlobalStrings — resolve first (immutable script), then fire (mutable).
    // 0x78 TOTEMS / 0x5c REAGENTS are the argument-formatted reasons whose `%s` fill benilla
    // models (decisions 0545 + 0552, the ref's shared fill arm `0x6e1e7f`): "Requires %s" /
    // "Missing reagent: %s" + the FAILING slot's item name — re-derived here exactly as the
    // check derived it (first missing totem / first short reagent against our bags). On an
    // item-cache miss the ref queries and shows nothing that frame, then its DBCACHECALLBACK
    // `0x6e29b0` REDISPLAYS when the answer lands — modeled by keeping the entry queued: the
    // ask-once query is away, and the frame the template answers, the fill succeeds and fires.
    let self_store = self_q.iter().next();
    let mut await_template: Vec<(u32, u8)> = Vec::new();
    let texts: Vec<String> = cast_errors
        .0
        .drain(..)
        .filter_map(|(spell_id, reason)| {
            let d = spells.as_ref().and_then(|s| s.catalog.get(spell_id));
            let get = |key: &str| script.lua().globals().get::<String>(key).ok();
            if reason == 0x78 || reason == 0x5c {
                let d = d?;
                let failing = if reason == 0x78 {
                    self_store
                        .and_then(|s| first_missing_totem(d, s, &items))
                        // No store to test against (a race): name the first tool at all.
                        .or_else(|| d.totems.iter().copied().find(|&t| t != 0))
                } else {
                    self_store
                        .and_then(|s| first_short_reagent(d, s, &items))
                        .or_else(|| d.reagents.iter().map(|&(id, _)| id).find(|&id| id != 0))
                }?;
                let cached = items
                    .template(failing, 0, &commands)
                    .map(|i| i.name.clone());
                let name = match cached {
                    Some(name) => name,
                    // Answered-unknown → the ref's callback fallback literal (`0x838044`);
                    // still pending → keep the entry queued for the redisplay.
                    None if items.template_answered_unknown(failing) => "UNKNOWN".to_string(),
                    None => {
                        await_template.push((spell_id, reason));
                        return None;
                    }
                };
                let key = if reason == 0x78 {
                    "SPELL_FAILED_TOTEMS"
                } else {
                    "SPELL_FAILED_REAGENTS"
                };
                return get(key)
                    .filter(|s| !s.is_empty())
                    .map(|t| t.replace("%s", &name));
            }
            cast_fail::cast_fail_text(reason, d, &get)
        })
        .collect();
    cast_errors.0.extend(await_template);
    for text in texts {
        script.fire_event("UI_ERROR_MESSAGE", vec![ScriptValue::Str(text)]);
    }

    // (Dis)mount refusals ride the same red line, keyed straight into GlobalStrings
    // ([`mount_result_key`] — no format arguments in any of these strings).
    let mount_texts: Vec<String> = mount_errors
        .0
        .drain(..)
        .filter_map(|(mount, code)| {
            let key = mount_result_key(mount, code)?;
            script.lua().globals().get::<String>(key).ok()
        })
        .collect();
    for text in mount_texts {
        script.fire_event("UI_ERROR_MESSAGE", vec![ScriptValue::Str(text)]);
    }

    // Client-local by-key refusals (the `DisplayError` route — [`UiErrorKeys`]) ride the same
    // red line; the key IS the GlobalStrings lookup, no code table between.
    let key_texts: Vec<String> = ui_error_keys
        .0
        .drain(..)
        .filter_map(|e| ui_error_text(&e, &|key| script.lua().globals().get::<String>(key).ok()))
        .collect();
    for text in key_texts {
        script.fire_event("UI_ERROR_MESSAGE", vec![ScriptValue::Str(text)]);
    }

    let store = self_q.iter().next();

    // Stance page: our own descriptor's form byte, pushed on change (UPDATE_BONUS_ACTIONBAR is
    // the client's event for exactly this transition — the bar re-picks its page on it).
    let form = store.map(|s| s.0.unit_shapeshift_form()).unwrap_or(0);
    let offset = spells
        .as_ref()
        .and_then(|s| s.forms.get(&u32::from(form)))
        .map(|f| f.bonus_bar)
        .unwrap_or(0) as u8;
    if offset != memory.bonus_offset {
        debug!("ui_action: bonus bar offset {} (form {form})", offset);
        memory.bonus_offset = offset;
        script.set_bonus_bar_offset(offset);
        script.fire_event("UPDATE_BONUS_ACTIONBAR", vec![]);
    }

    if actions.dirty {
        actions.dirty = false;

        // Resolve every occupied wire slot to its display, diff against what the VM holds, push +
        // fire ACTIONBAR_SLOT_CHANGED (arg1 = the Lua action id) per transition. Item icons/counts
        // resolve via the same ask-once template chain + bag walk the bags use
        // (`ui_items::count_of`) — an in-flight template shows the fallback (no texture) and
        // re-feeds once the answer lands, same as a bag slot.
        let mut fresh: HashMap<u32, ActionSlot> = HashMap::new();
        for (slot, button) in &actions.buttons {
            let (texture, count) = match button.kind {
                ACTION_KIND_SPELL => {
                    // The melee auto-attack shows the equipped weapon icon, not spell 6603's `Temp`
                    // placeholder (decision 0230); every other spell uses its own icon.
                    let d = spells.as_ref().and_then(|s| s.catalog.get(button.action));
                    let icon = d
                        .and_then(|d| {
                            auto_attack_icon(d, store, &mut items, icons.as_deref(), &commands)
                        })
                        .or_else(|| d.and_then(|d| d.icon.clone()));
                    (icon, 0)
                }
                ACTION_KIND_ITEM => {
                    let texture = items
                        .template(button.action, 0, &commands)
                        .cloned()
                        .and_then(|t| icons.as_ref()?.catalog.get(t.display_info_id)?.icon.clone());
                    let count = store
                        .map(|s| crate::ui_items::count_of(&s.0, &items, button.action))
                        .unwrap_or(0);
                    (texture, count)
                }
                // Macro actions: no macro catalog yet (no macro window ships) — the engine-side
                // fallback icon shows, matching the pre-slice-4 gap.
                _ => (None, 0),
            };
            fresh.insert(
                u32::from(*slot) + 1,
                ActionSlot {
                    texture,
                    kind: button.kind,
                    action: button.action,
                    count,
                },
            );
        }
        let changed: Vec<u32> = fresh
            .keys()
            .chain(memory.pushed.keys())
            .copied()
            .collect::<HashSet<_>>()
            .into_iter()
            .filter(|a| fresh.get(a) != memory.pushed.get(a))
            .collect();
        for &action in &changed {
            script.set_action(action, fresh.get(&action).cloned());
        }
        memory.pushed = fresh;
        debug!(
            "ui_action: fed {} changed slot(s) ({} occupied)",
            changed.len(),
            memory.pushed.len()
        );
        for action in changed {
            script.fire_event(
                "ACTIONBAR_SLOT_CHANGED",
                vec![ScriptValue::Int(i64::from(action))],
            );
        }
    }

    // Two things drift independently of `dirty` (decision 0216 §7's module-doc note) and so must
    // refresh every frame, not just on an action-table edit: an ITEM slot's COUNT (eating down a
    // stack never touches SMSG_ACTION_BUTTONS) and the auto-attack's ICON (it tracks the equipped
    // main-hand weapon, which a weapon swap changes without touching the action table — decision
    // 0230). Gating either on `dirty` leaves it stale until the next unrelated action-bar edit.
    // Bounded to the already-pushed slots (normally a handful) — the same per-frame bag walk /
    // template lookup `count_of` already pays for the quest log's item objectives.
    if let Some(store) = store {
        for (&action, slot) in memory.pushed.iter_mut() {
            let changed = match slot.kind {
                ACTION_KIND_ITEM => {
                    let fresh = crate::ui_items::count_of(&store.0, &items, slot.action);
                    let changed = fresh != slot.count;
                    if changed {
                        slot.count = fresh;
                    }
                    changed
                }
                // A weapon-substituting icon tracks the equipped weapon — main-hand for Attack
                // (decision 0230), the ranged slot for Auto Shot / wand Shoot (decision 0231's
                // ranged case) — which a swap changes without touching the action table, so
                // refresh it too. A normal spell's icon is stable, so it's skipped.
                ACTION_KIND_SPELL => {
                    let d = spells.as_ref().and_then(|s| s.catalog.get(slot.action));
                    match d.filter(|d| substitutes_weapon_icon(d)) {
                        Some(d) => {
                            let fresh = auto_attack_icon(
                                d,
                                Some(store),
                                &mut items,
                                icons.as_deref(),
                                &commands,
                            )
                            .or_else(|| d.icon.clone());
                            let changed = fresh != slot.texture;
                            if changed {
                                slot.texture = fresh;
                            }
                            changed
                        }
                        None => false,
                    }
                }
                _ => false,
            };
            if changed {
                script.set_action(action, Some(slot.clone()));
                script.fire_event(
                    "ACTIONBAR_SLOT_CHANGED",
                    vec![ScriptValue::Int(i64::from(action))],
                );
            }
        }
    }
}

/// Send one spell cast at the current selection — the client's local cast-send follow-through
/// (the client's `0x6e54f0` tail): a ranged-attribute spell arms the **ranged stance** now
/// (`0x6e5930`'s `SetSheatheState(2,1,1)` — the echo START re-requests it, idempotent), an
/// auto-repeat spell sets the sticky armed state (`0x6e593b`'s `|= 0x200`, the standing Load/Hold
/// idle's gate — decision 0099 phase 5), and the resolved `CMSG_CAST_SPELL` goes out. Shared by
/// [`drain_action_uses`] (a SPELL-kind action button) and `ui_spellbook::drain_spell_casts` (a
/// spellbook cast, decision 0216 §8) — ONE cast-send path, so the follow-through can't drift
/// between the two spell sources (the root-cause rule: never duplicate a send path).
///
/// The `pending` guard is the client's optimistic in-flight refusal (wow-re `wave-cast.md`
/// `TryCast` IsCasting gate; see [`crate::ui_cast::PendingCast`]): a normal cast is dropped at the
/// source while one is already in flight, so mashing a key can no longer fire a duplicate
/// `CMSG_CAST_SPELL` the server bounces back as a spurious cast-bar cancel. Ranged/auto-repeat
/// shots keep their own lifecycle — they never arm the guard and are never blocked by it.
#[allow(clippy::too_many_arguments)] // every input the follow-through + the send itself need
pub(crate) fn send_spell_cast(
    spell_id: u32,
    ctx: &cast_target::CastContext,
    commands: &NetCommands,
    self_player: &Query<(Entity, Has<crate::creature_anim::Engaged>), With<SelfPlayer>>,
    spells: Option<&Spells>,
    items: &Items,
    sheath: &mut MessageWriter<crate::creature_anim::SheathRequest>,
    ecs: &mut Commands,
    pending: &mut crate::ui_cast::PendingCast,
    queued_melee: &mut crate::ui_cast::QueuedMeleeSpell,
    cooldowns: &mut crate::cooldowns::Cooldowns,
    cast_errors: &mut CastErrors,
    auto_repeat: &mut AutoRepeatActive,
    trade_skill_opens: &mut crate::ui_tradeskill::TradeSkillOpens,
) {
    let now = Instant::now();
    let def = spells.and_then(|s| s.catalog.get(spell_id));
    // The profession-window intercept (decision 0437): `Spell_C::TryCast 0x6e4b60`'s own first
    // special branch (wow-re `wave-cast.md`, VERIFIED) — an `Effect[0] == SPELL_EFFECT_TRADE_SKILL`
    // cast NEVER reaches the wire; the crafting book opens client-side instead. Before the
    // cooldown ladder, exactly where the client dispatches it (`6e4bce`, ahead of every gate).
    if def.is_some_and(|d| d.effect_1 == benilla_formats::SPELL_EFFECT_TRADE_SKILL) {
        debug!("ui_action: cast {spell_id} is a profession opener — the crafting book opens, no packet");
        trade_skill_opens.0.push(spell_id);
        return;
    }
    // The button re-press toggle (`0x4e60da`, wow-re `nocked-ammo-cancel.md` §Q-B-2):
    // re-invoking the spell that IS the running auto-repeat cancels it instead of re-casting —
    // the classic press-again-to-stop. Checked before the cooldown ladder, like the client's
    // action-button handler (which never reaches TryCast for the toggle-off).
    if def.is_some_and(|d| d.auto_repeat()) && auto_repeat.0 == Some(spell_id) {
        debug!("ui_action: cast {spell_id} re-pressed — auto-repeat toggles off");
        let self_e = self_player.single().ok().map(|(e, _)| e);
        crate::creature_anim::cancel_auto_repeat_local(self_e, auto_repeat, ecs, commands);
        return;
    }
    // The local not-ready refusal (the client's `IsSpellOnCooldown 0x6e1690` gate in the cast
    // path): a spell/category cooldown refuses at the source with the client's own reason 0x3c
    // ("Spell is not ready yet.") — never sent, exactly like the real client. The GCD is
    // faithfully NOT part of this test (0x6e1690 skips the GCD fields).
    if cooldowns.is_on_cooldown(spell_id, def, now) {
        debug!("ui_action: cast {spell_id} refused locally — on cooldown");
        cast_errors.0.push((spell_id, 0x3c));
        return;
    }
    // The GCD leg of the local not-ready ladder ([`Cooldowns::gcd_locked`], decision 0379): a
    // GCD-carrying spell pressed while its startRecoveryCategory still runs refuses at the
    // source — same reason 0x3c, NEVER sent. Sending would draw the server's NOT_READY fail
    // (vmangos enforces the GCD), whose faithful revert clears the RUNNING GCD — the spam-press
    // vanished-pie bug. GCD-free presses (Heroic Strike's queue, Attack, Shoot) pass.
    if def.is_some_and(|d| cooldowns.gcd_locked(d, now)) {
        debug!("ui_action: cast {spell_id} refused locally — the GCD is running");
        cast_errors.0.push((spell_id, 0x3c));
        return;
    }
    // The cast classes at this seam. A ranged/auto-repeat shot (Auto Shot, wand Shoot, Throw) is
    // not a cast-bar cast — it runs the ranged-stance / `AutoRepeatArmed` path, outside the
    // in-flight guard. An on-next-swing spell (`Attributes & 0x404` — Heroic Strike, Cleave)
    // queues on the server's melee slot: it arms [`crate::ui_cast::QueuedMeleeSpell`], never the
    // in-flight guard, so a queued strike cannot block the next cast (the ref's `6e4d97`
    // exemption on the inflight rec's 0x404 bits — wow-re `wave-cast.md`).
    let on_next_swing = def.is_some_and(|d| d.on_next_swing());
    let normal_cast = !def.is_some_and(|d| d.ranged_attack()) && !on_next_swing;
    // Re-pressing the queued strike is the ref's silent same-spell bail (`6e4d43`) — no cancel,
    // no error: 1.12 has no re-press-to-unqueue.
    if on_next_swing && queued_melee.current() == Some(spell_id) {
        debug!("ui_action: cast {spell_id} suppressed — already queued on next swing");
        return;
    }
    if (normal_cast || on_next_swing) && pending.in_flight(now) {
        // The ref's already-casting refusal: the same spell bails silently (`6e4d43`); a
        // different one errors reason 0x61 "Another action is in progress" (`6e4d97` →
        // `HandleCastFailed`) — the inflight rec here is always an ordinary cast, so even an
        // on-next-swing press is refused while it holds.
        if pending.current(now) != Some(spell_id) {
            cast_errors.0.push((spell_id, 0x61));
        }
        debug!("ui_action: cast {spell_id} suppressed — a cast is already in flight");
        return;
    }
    // The client-side mounted gate (decision 0481; wow-re `mounted-action-gate.md` §5:
    // TryCast's requirement validator `0x6094f0`, mounted block `0x609c6c` — a live
    // `UNIT_FIELD_MOUNTDISPLAYID` refuses a non-exempt cast with reason 0x39 "You are
    // mounted" BEFORE the cast-arm's target binding, which is why a targetless mounted click
    // never reads "You have no target"). Exemption: Attributes bit 24 (`0x01000000`,
    // castable-while-mounted). The gate must be LOCAL: vmangos silently dismounts a mounted
    // caster instead of erroring, so without this check the message can never appear. Named
    // micro-divergence: the ref range-tests a RESOLVED target (its step 6) before this gate;
    // ours range-tests after binding, so the mounted∧out-of-range double fault shows "You are
    // mounted" where the ref shows "Out of range." — unobservable outside that corner.
    if state::cast_mounted_refusal(
        ctx.rel
            .self_store
            .is_some_and(|s| s.0.unit_mount_display_id() > 0),
        def,
    ) {
        debug!("ui_action: cast {spell_id} refused locally — mounted (0x39)");
        cast_errors.0.push((spell_id, 0x39));
        return;
    }
    // The pre-send totem/reagent possession check (`CheckReagentsAndTotems 0x6e4000`, TryCast's
    // `0x6e4ded` — decision 0552): a missing tool (Mining Pick) or a short reagent refuses HERE
    // with the client's own 0x78/0x5c red line and NEVER sends. The gate must be local: vmangos
    // answers a sent pickless cast with the wrong code (`ITEM_GONE` "Item is gone"), so without
    // it the real message can't appear. Placement after the mounted gate mirrors the ref only
    // approximately (the `0x6094f0`-vs-`0x6e4ded` call order isn't pinned — double-fault
    // corners only).
    if reagent_totem_refusal(spell_id, def, ctx.rel.self_store, items, cast_errors) {
        return;
    }
    // ArmCast (`0x6e5250`): resolve the wire target from the spell's targeting constraints —
    // never the raw selection ([`cast_target`] module docs). A refusal is local and pre-commit,
    // like the ref's residual flag_word: no send, no GCD, no pending arm, no autorepeat key.
    let target = match cast_target::resolve_cast_target(
        def,
        ctx.selection_guid,
        ctx.self_guid,
        ctx.auto_self_cast,
        &ctx.rel,
    ) {
        cast_target::CastWireTarget::SelfImplicit => None,
        cast_target::CastWireTarget::Unit(guid) => Some(guid),
        cast_target::CastWireTarget::Refused(reason) => {
            debug!("ui_action: cast {spell_id} refused locally — unbindable target ({reason:#x})");
            cast_errors.0.push((spell_id, reason));
            return;
        }
    };
    // The local range gate (the client's TryCast runs `CanTargetUnit 0x6e4440` →
    // `IsTargetInRange 0x6e47b0` BEFORE the commit `0x6e54f0`): an out-of-range / too-close
    // press on a bound unit target refuses here — before the ranged-stance arm below, so a
    // too-close Throw/Auto Shot never draws the bow and never stows the melee weapons (the
    // sheath snap `0x6e5930` lives in the commit tail this refusal never reaches). The bound
    // target is only ever the selection or ourselves; a self-bind (autoSelfCast) is distance 0
    // with a min-0 range in practice, so only the selection leg is tested.
    if let Some(d) = def {
        if target.is_some() && target == ctx.selection_guid && target != ctx.self_guid {
            let row = spells.and_then(|s| s.ranges.get(d.range_index));
            let dist_sq = ctx
                .range
                .self_pos
                .zip(ctx.range.target_pos)
                .map(|(a, b)| a.distance_squared(b));
            if let Some(reason) = state::cast_range_refusal(
                d,
                row,
                ctx.range.self_reach,
                ctx.range.target_reach,
                dist_sq,
            ) {
                debug!("ui_action: cast {spell_id} refused locally — range ({reason:#x})");
                cast_errors.0.push((spell_id, reason));
                return;
            }
        }
    }
    // The wand-only auto-repeat handoff (the client's `0x60959e` inside TryCast's `0x6094f0`
    // step, wow-re `nocked-ammo-cancel.md` §Q-B-5): a NEW cast cancels the running auto-repeat
    // iff the CACHED spell carries `AttributesEx3 & 0x400000` — wand Shoot 5019 alone in the
    // 1.12 data. Auto Shot survives by construction: hunter shot-weaving. The client first
    // sends `CMSG_CANCEL_CAST` naming the cached wand spell (`0x6095b8`), then runs the local
    // cancel (whose own `CMSG_CANCEL_AUTO_REPEAT` ack follows). The same-spell re-press never
    // reaches here — the toggle above returned.
    if let Some(cached) = auto_repeat.0 {
        if spells
            .and_then(|s| s.catalog.get(cached))
            .is_some_and(|d| d.casting_cancels_autorepeat())
        {
            debug!("ui_action: cast {spell_id} cancels the running wand repeat {cached}");
            let _ = commands
                .0
                .send(ClientCommand::CancelCast { spell_id: cached });
            let self_e = self_player.single().ok().map(|(e, _)| e);
            crate::creature_anim::cancel_auto_repeat_local(self_e, auto_repeat, ecs, commands);
        }
    }
    if let Some(d) = def {
        if let Ok((e, _)) = self_player.single() {
            if d.ranged_attack() {
                sheath.write(crate::creature_anim::SheathRequest {
                    entity: e,
                    state: 2,
                    ceremony: false,
                });
            }
            if d.auto_repeat() {
                ecs.entity(e).insert(crate::creature_anim::AutoRepeatArmed);
                // The live autorepeat key (`0xceac30 = SpellRec+0x00` at `0x6e5947`) — what the
                // button's flash/checked state reads until CANCEL_AUTO_REPEAT clears it.
                auto_repeat.0 = Some(spell_id);
            }
        }
    }
    let _ = commands
        .0
        .send(ClientCommand::CastSpell { spell_id, target });
    if normal_cast {
        pending.arm(spell_id, now);
    } else if on_next_swing {
        queued_melee.arm(spell_id);
    }
    // TryCast's post-send tail (`6e51b5`) — byte-verified whole by the 2026-07-14 wow-re §5
    // (`combat-feel-law.md` @ c445713b): a committed send whose rec passes
    // [`SpellDisplay::initiates_auto_attack`] (on-next-swing `0x404` or `AttributesEx & 0x200`,
    // and not the GO-deferred Ex2-bit20; Charge carries none) starts the melee auto-attack at
    // the cast's bound unit target, unless one is already running (`0x60ecb0` over
    // `[player+0xc48]`; our mirror is the wire-echoed `Engaged`). The start is the attack path's
    // own pair (`0x6131a0` → `0x5ecb70`): melee-sheath SNAP + `CMSG_ATTACKSWING` — the same two
    // edges as the Attack button's arm in [`drain_action_uses`]. Path-independent in the ref
    // (button/spellbook/CastSpellByName share the one tail) — matching our one send seam.
    if let (Some(d), Some(guid)) = (def, target) {
        if d.initiates_auto_attack() {
            if let Ok((e, engaged)) = self_player.single() {
                if !engaged {
                    debug!("ui_action: cast {spell_id} initiates auto-attack at {guid:#x}");
                    sheath.write(crate::creature_anim::SheathRequest {
                        entity: e,
                        state: 1,
                        ceremony: false,
                    });
                    // Melee attack-start cancels a running auto-repeat UNCONDITIONALLY — the
                    // client's `0x5ecd8c` tail right after its melee snap (wow-re
                    // `nocked-ammo-cancel.md` §Q-B-5): you can't melee and auto-shoot at once.
                    crate::creature_anim::cancel_auto_repeat_local(
                        Some(e),
                        auto_repeat,
                        ecs,
                        commands,
                    );
                    let _ = commands.0.send(ClientCommand::AttackSwing { guid });
                }
            }
        }
    }
    // Arm the GCD at send (`StartGlobalCooldown 0x6e2de0` ← the cast-send arm `0x6e58fb`,
    // byte-verified) — a later `SMSG_CAST_RESULT` failure clears it again (`0x6e1630`).
    if let Some(d) = def {
        cooldowns.start_gcd(spell_id, d, now);
    }
}

#[allow(clippy::too_many_arguments)] // a Bevy system's full input set
fn drain_action_uses(
    script: Option<NonSendMut<UiScript>>,
    actions: Res<PlayerActions>,
    targeting: cast_target::CastTargeting,
    commands: Res<NetCommands>,
    self_player: Query<(Entity, Has<crate::creature_anim::Engaged>), With<SelfPlayer>>,
    mut items: ResMut<Items>,
    mut sheath: MessageWriter<crate::creature_anim::SheathRequest>,
    mut acquire: MessageWriter<crate::target::AttackNearestRequest>,
    spells: Option<Res<Spells>>,
    mut pending: ResMut<crate::ui_cast::PendingCast>,
    mut queued_melee: ResMut<crate::ui_cast::QueuedMeleeSpell>,
    mut cooldowns: ResMut<crate::cooldowns::Cooldowns>,
    // The error sinks, one tuple param (Bevy's 16-param ceiling): the reason-coded cast line
    // + the by-key local line.
    mut errors: (ResMut<CastErrors>, ResMut<UiErrorKeys>),
    mut auto_repeat: ResMut<AutoRepeatActive>,
    mut trade_skill_opens: ResMut<crate::ui_tradeskill::TradeSkillOpens>,
    mut ecs: Commands,
) {
    let selection = &targeting.selection;
    let Some(mut script) = script else {
        return;
    };
    for action in script.take_action_uses() {
        let slot = match u8::try_from(action.saturating_sub(1)) {
            Ok(s) => s,
            Err(_) => continue,
        };
        match actions.buttons.get(&slot) {
            Some(b) if b.kind == ACTION_KIND_SPELL && b.action == SPELL_ATTACK => {
                // The mounted attack block ([`attack_mounted_refusal`]): the ref's validator
                // refuses BEFORE the with-target swing and before the nearest-enemy scan
                // (`0x613039` precedes `0x6130b5`), so both arms gate here.
                if attack_mounted_refusal(targeting.self_store.iter().next(), &mut errors.1) {
                    continue;
                }
                match selection.guid {
                    Some(guid) => {
                        debug!("ui_action: attack swing at {guid:#x}");
                        // Auto-draw: initiating melee requests melee sheath state through the
                        // anim layer's ONE setter (decision 0080) — a SNAP, no ceremony, no
                        // sound: the attack path passes `(newState=1, bInstant=1, bFireEvent=1)`
                        // at `0x5ecd80` (wow-re `sheath-policy.md`). The setter's idempotency
                        // is the client's own "no-op if already melee".
                        if let Ok((e, _)) = self_player.single() {
                            sheath.write(crate::creature_anim::SheathRequest {
                                entity: e,
                                state: 1,
                                ceremony: false,
                            });
                        }
                        // Melee attack-start cancels a running auto-repeat UNCONDITIONALLY —
                        // the client's `0x5ecd8c` (wow-re `nocked-ammo-cancel.md` §Q-B-5).
                        let self_e = self_player.single().ok().map(|(e, _)| e);
                        crate::creature_anim::cancel_auto_repeat_local(
                            self_e,
                            &mut auto_repeat,
                            &mut ecs,
                            &commands,
                        );
                        let _ = commands.0.send(ClientCommand::AttackSwing { guid });
                    }
                    // No target: the client's attack resolver runs the nearest-enemy core and
                    // swings at the winner (`0x612df0` @ `6130b5`) — `target::scan` answers.
                    None => {
                        debug!("ui_action: attack with no target — acquiring nearest");
                        acquire.write(crate::target::AttackNearestRequest);
                    }
                }
            }
            Some(b) if b.kind == ACTION_KIND_SPELL => {
                debug!("ui_action: cast {} (target {:?})", b.action, selection.guid);
                send_spell_cast(
                    b.action,
                    &targeting.context(),
                    &commands,
                    &self_player,
                    spells.as_deref(),
                    &items,
                    &mut sheath,
                    &mut ecs,
                    &mut pending,
                    &mut queued_melee,
                    &mut cooldowns,
                    &mut errors.0,
                    &mut auto_repeat,
                    &mut trade_skill_opens,
                );
            }
            // An item action names an item id, not a bag position — resolve to its FIRST bag
            // slot (`ui_items::first_bag_slot`'s own doc: an unverified-but-necessary resolve
            // order, decision 0216 §7) and route through the same equip-vs-use fork
            // `ui_items::drain_container_uses` uses. A miss (the item left the bag between the
            // click and this drain, or was never there — e.g. a stale action left over from a
            // previous session) is a plain debug-log-and-skip, NOT the red error line: nothing
            // was actually attempted against the server, so "Item is not ready" would be a lie.
            Some(b) if b.kind == ACTION_KIND_ITEM => {
                let Some(store) = targeting.self_store.iter().next() else {
                    continue;
                };
                let Some((bag_index, slot0)) =
                    crate::ui_items::first_bag_slot(&store.0, &items, b.action)
                else {
                    debug!(
                        "ui_action: item action {action} (entry {}) not in any bag — skipped",
                        b.action
                    );
                    continue;
                };
                // The equip-vs-use fork: an equippable item (inventoryType != 0) auto-equips,
                // same as a bag-slot click; the template is all but always cached by click time
                // (the icon resolve needed it), unresolved falls back to USE like the bag drain.
                let equippable = items
                    .template(b.action, 0, &commands)
                    .is_some_and(|t| t.inventory_type != 0);
                if equippable {
                    debug!("ui_action: item action {action} auto-equip (wire {bag_index}/{slot0})");
                    let _ = commands.0.send(ClientCommand::AutoEquipItem {
                        bag_index,
                        slot: slot0,
                    });
                } else {
                    // The item twin of the cast path's local not-ready refusal
                    // (`IsItemOnCooldown 0x6e2fc0`: the on-use spell against the cooldown list)
                    // — reason 0x28 is the client's "Item is not ready yet.".
                    let use_spell = items
                        .template(b.action, 0, &commands)
                        .and_then(|t| t.use_spell);
                    let on_cooldown = use_spell.filter(|u| {
                        cooldowns.is_on_cooldown(
                            u.spell_id,
                            spells.as_ref().and_then(|s| s.catalog.get(u.spell_id)),
                            Instant::now(),
                        )
                    });
                    if let Some(u) = on_cooldown {
                        debug!("ui_action: item action {action} refused locally — on cooldown");
                        errors.0 .0.push((u.spell_id, 0x28));
                        continue;
                    }
                    debug!("ui_action: item action {action} use (wire {bag_index}/{slot0})");
                    let _ = commands.0.send(ClientCommand::UseItem {
                        bag_index,
                        slot: slot0,
                    });
                }
            }
            Some(b) => {
                debug!(
                    "ui_action: action {action} kind {:#04x} not castable yet (macro)",
                    b.kind
                );
            }
            None => debug!("ui_action: UseAction({action}) on an empty slot"),
        }
    }
}

/// Drain the `(lua action id, packed)` pairs the cursor seam's `PickupAction`/`PlaceAction`
/// queued (decision 0216 §7) — the engine's own local mutation already agrees with what lands
/// here (it wrote the same value into its optimistic `model.actions` mirror before queuing this).
/// Each entry: write `PlayerActions.buttons` (`packed == 0` removes the slot, else inserts),
/// mark `dirty` so [`feed_actions`] re-resolves + re-pushes + fires `ACTIONBAR_SLOT_CHANGED` (the
/// existing diff machinery — no bespoke event here), and send ONE `CMSG_SET_ACTION_BUTTON`
/// (0218 §4: client-authoritative, no answer packet, a drag-swap is two independent sends).
fn drain_action_sets(
    script: Option<NonSendMut<UiScript>>,
    mut actions: ResMut<PlayerActions>,
    commands: Res<NetCommands>,
) {
    let Some(mut script) = script else {
        return;
    };
    for (lua_id, packed) in script.take_action_sets() {
        let Ok(slot) = u8::try_from(lua_id.saturating_sub(1)) else {
            debug!("ui_action: set_action_button lua id {lua_id} out of range — ignored");
            continue;
        };
        if packed == 0 {
            actions.buttons.remove(&slot);
        } else {
            actions.buttons.insert(
                slot,
                ActionButton {
                    slot,
                    action: packed & 0x00FF_FFFF,
                    kind: (packed >> 24) as u8,
                },
            );
        }
        actions.dirty = true;
        debug!(
            "ui_action: set_action_button lua {lua_id} (wire slot {slot}) packed {packed:#010x}"
        );
        let _ = commands.0.send(ClientCommand::SetActionButton {
            button: slot,
            packed,
        });
    }
}
