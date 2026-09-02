//! The app-side **action seam** (decision 0068 slice 1, extended by decision 0216 §7/slice 4) —
//! the three directions around the engine-free bindings ([`benilla_ui::script`]'s `action`/
//! `cursor::bar` modules). This module owns the seam's *shared state* — the action table, the
//! spell catalogs, the plugin wiring — and each direction lives in its own file:
//!
//! - **Inward — identity** ([`feed`]): the net bridge fills [`PlayerActions`] from
//!   `SMSG_INITIAL_SPELLS` + `SMSG_ACTION_BUTTONS` (and [`drain::drain_action_sets`] writes it
//!   directly, client-side — the bar is client-authoritative, decision 0218 §4); the feed resolves
//!   each occupied slot's icon and count, pushes the 120-slot snapshot into the VM, and fires
//!   `ACTIONBAR_SLOT_CHANGED` per changed slot. What is gated on what is the design there.
//! - **Inward — dynamic state** ([`state`]): cooldown swirl, usability tint, range colour,
//!   checked/flash — the per-frame half, fed after the identity half so a fresh slot's first state
//!   push lands the same frame.
//! - **Outward — use** ([`drain::drain_action_uses`]): a queued `UseAction(n)` becomes wire — a
//!   spell through the one cast-send path ([`cast_send`]), the auto-attack through
//!   `CMSG_ATTACKSWING`, an item through the two-stage equip-vs-use law ([`drain::item_action_route`],
//!   decision 0666).
//! - **Outward — set** ([`drain::drain_action_sets`]): the cursor seam's `PickupAction`/
//!   `PlaceAction` mutations become `CMSG_SET_ACTION_BUTTON` sends, one per queued entry.
//!
//! The supporting law sits alongside: [`cast_target`] (which unit a cast binds), [`cast_fail`] +
//! [`errors`] (the red error line's two layers), [`usable`] (the castability walk the stance bar
//! shares), [`weapon_icon`] (the auto-attack's borrowed weapon icon).

use std::collections::{BTreeSet, HashMap};

use bevy::prelude::*;

use benilla_formats::SpellCatalog;
use benilla_protocol::messages::ActionButton;

use crate::ui_script::UiInput;
use crate::ui_unit::UnitFeed;
use benilla_assets::{AssetSet, LockRecover, WorldAssets};

mod cast_fail;
mod cast_send;
pub(crate) mod cast_target;
mod drain;
#[cfg(test)]
mod drain_tests;
pub(crate) mod drop_item;
mod errors;
mod feed;
#[cfg(test)]
mod feed_tests;
mod ranks;
mod state;
pub(crate) mod targeting;
pub(crate) mod toggle;
mod weapon_icon;

/// The cooldown-event cut: [`state::feed_action_state`] fires the store-change flush trio
/// (`ACTIONBAR_UPDATE_COOLDOWN`/`SPELL_UPDATE_COOLDOWN`/`BAG_UPDATE_COOLDOWN`) **synchronously**
/// (`UiScript::fire_event` walks the handlers inline), so every feed that pushes cooldown
/// triples the handlers re-read (the container feed's slot cooldowns, the spellbook feed's) must
/// run `.before(CooldownEvents)` — or a handler reads last frame's triples and the pie stays
/// missing until the next store change. The action states themselves are safe by construction
/// (pushed by the same system, before it fires).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CooldownEvents;

// The one cast path, and the only way in: every caster surface takes [`CastLadder`] as a single
// SystemParam and commits through [`CastCommit`] — `send_spell_cast` itself is private to
// `cast_send`, so a second send path cannot be written by accident (decision 0914).
pub(crate) use cast_send::{CastCommit, CastLadder};
pub(crate) use cast_target::AutoSelfCast;
pub(crate) use errors::{
    attack_actor_blocked, attack_actor_refusal, keyed_line, reagent_totem_refusal, show_messages,
    ui_error_text, CastErrors, CastFail, MessageSink, MountErrors, Shown, UiError, UiErrorKeys,
    UiErrorTexts,
};
// `pub(crate)`: the requirement validator's mounted block is ONE gate in the reference
// (`0x6094f0` @ `0x609c6c`) sitting under the ONE cast entry `TryCast 0x6e4b60` — but benilla still
// has cast sends outside [`CastLadder`]: the world click's skin cast, the corpse insignia cast and
// the GameObject opener (decisions 0552/0914). Each of them reaches TryCast in the reference
// (`0x5f05e0 → 0x6e5a90 → 0x6e4b60`; `0x5f35c0 → 0x6e5a90 → 0x6e4b60`), so each of them must ask
// this question — the same reason `reagent_totem_refusal` above is already exported. Exporting the
// predicate is the honest interim; the ladder refactor that would retire it is 0914's, not 1851's.
pub(crate) use state::cast_mounted_refusal;
// `pub(crate)`: the target chain registers the cursor pre-empt + the click commit, and the
// spellbook/stance/craft drains thread the mode through the one cast-send path (decision 0792).
// `TargetingWants` travels with it because the chain also holds a *seam-specific* consumer — the
// ground reticle, which draws for the location word alone (decision 0943).
pub(crate) use targeting::{ground_cast_radius, SpellTargeting, TargetingWants};
// `pub(crate)`: the spellbook shows the same borrowed weapon icon its bar buttons do, pre-resolved
// once per page (decisions 0230/0231).
pub(crate) use weapon_icon::{melee_auto_attack_icon, ranged_weapon_icon};
// `pub(crate)`: the stance bar's `isCastable` IS this walk (`GetShapeshiftFormInfo`'s fourth
// return runs `0x6e3d60`, wow-re shapeshift-bar-api.md) — `crate::ui_shapeshift` calls it with
// the same ctx shape the state feed builds.
pub(crate) mod usable;

/// The auto-attack pseudo-spell (`Attack`, every character's slot-1 default): not a cast — it
/// toggles melee via the attack-swing pair. The USE path (`CMSG_ATTACKSWING`) keys on this id; the
/// ICON substitution keys on the effect type instead ([`benilla_formats::SpellDisplay::is_melee_auto_attack`],
/// decision 0231 — 6603 is simply the only spell carrying `SPELL_EFFECT_ATTACK`).
pub(crate) const SPELL_ATTACK: u32 = 6603;

/// The player's action store: the occupied wire slots (0..119) + the known-spell set. Written by
/// the net bridge (`SMSG_INITIAL_SPELLS`/`SMSG_ACTION_BUTTONS`) AND, since decision 0216 §7,
/// directly by [`drain::drain_action_sets`] (the bar is client-authoritative — a local
/// pickup/place is never echoed back by the server, vmangos `MasterPlayer::addActionButton`/
/// `removeActionButton` send nothing). Read by [`feed::feed_actions`]/[`drain::drain_action_uses`].
#[derive(Resource, Default)]
pub(crate) struct PlayerActions {
    /// Wire slot (0-based) → the slot's packed action. Lua action id = slot + 1.
    pub buttons: HashMap<u8, ActionButton>,
    /// The spell book (`SMSG_INITIAL_SPELLS`).
    ///
    /// **Ordered, and that is load-bearing** (decision 1312). The reference keeps its known spells
    /// in an ARRAY and the scans that hunt it — the GameObject lock resolver `0x5f83d0` above all —
    /// stop at the first hit, so the visit order picks *which* of several equally-qualified spells
    /// wins. A `HashSet` made that pick nondeterministic: every character knows both 6478
    /// "Opening" and 22810 "Opening - No Text" (both `LockType 13`, both trivially sufficient), and
    /// whichever the hash happened to reach first went on the cast bar (B247). Ascending spell id
    /// is the array's own order after login — the server sends the initial batch out of a
    /// `std::map`, so the wire arrives sorted.
    pub spells: BTreeSet<u32>,
    /// Set on every book/bar arrival AND every local `action_sets` drain; cleared by the feed
    /// after re-resolving each slot's identity (icon/kind/action) and pushing. It is only ONE of
    /// the identity resolve's two triggers — a landed item template is the other (decision 0660,
    /// [`Items::template_epoch`]) — and it gates ONLY that resolve: an ITEM slot's bag COUNT is
    /// refreshed unconditionally every frame instead (see [`feed`]'s module doc), since it drifts
    /// independently of both.
    ///
    /// [`Items::template_epoch`]: crate::items::Items::template_epoch
    pub dirty: bool,
}

/// The live auto-repeat spell — the client's autorepeat key `0xceac30` (wow-re `wave-cast.md`:
/// written at the local cast-send for `AttributesEx2 & 0x20` spells, cleared by
/// `SMSG_CANCEL_AUTO_REPEAT`'s `0x6ea080` and by a matching cast-fail). Distinct from the sticky
/// `creature_anim::AutoRepeatArmed` (the Load/Hold idle gate, never cleared): THIS one is what
/// `IsAutoRepeatAction` and the button flash read, and it goes out when the shooting stops.
#[derive(Resource, Default)]
pub(crate) struct AutoRepeatActive(pub Option<u32>);

/// **The `modalNextSpell` chain's queue** — spells the *client* casts on its own, one per
/// `SMSG_CAST_RESULT` that names a spell with a non-zero `Spell.dbc` column 38
/// ([`benilla_formats::SpellDisplay::modal_next_spell`]). Written by the net drain's
/// `cast_result`, drained through the one cast path by [`drain::drain_chain_casts`].
///
/// It exists because the reference's chain runs *inside* `HandleCastResult 0x6e7330`
/// (`0x6e74aa call 0x6e5a90` → `TryCast`) and ours cannot: the net-apply drain and
/// [`CastLadder`] want the same half-dozen resources, so a direct call is a Bevy param conflict.
/// A one-frame queue is the seam — and it keeps the rule that nothing sends a cast except the
/// ladder.
#[derive(Resource, Default)]
pub(crate) struct ChainCasts(pub(crate) Vec<u32>);

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

impl Spells {
    /// The resolved cast time, ms — `GetCastTime 0x6e3340`'s level-scaled walk (wow-re
    /// `wave-cooldown.md`/`moving-cast-gate.md`, byte-verified): `CastingTimeIndex` resolves the
    /// [`Self::cast_times`] row, `base + perLevel·(casterLevel − baseLevel)` floors to the
    /// row's minimum (row 1, the all-zero instant sentinel, resolves 0). The level term keys on
    /// the `SpellRec+0x70` column ([`SpellDisplay::base_level`]); spellmod op `0xa`
    /// (SPELLMOD_CASTING_TIME) is unmodeled — benilla has no spellmod system — a named
    /// micro-divergence (a talent-shortened 0-second cast doesn't exist in the 1.12 data).
    /// A missing row reads 0 (instant), like a failed catalog load everywhere else.
    pub(crate) fn cast_time_ms(
        &self,
        def: &benilla_formats::SpellDisplay,
        caster_level: u32,
    ) -> u32 {
        self.cast_times
            .get(def.casting_time_index)
            .map_or(0, |row| row.resolved_ms(caster_level, def.base_level))
    }
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

/// The client's **learned-ability latches** — `[0xb700e4]` and `[0xb700e8]`, mirrored (decision
/// 0752). The reference does not scan the spell book to answer "can this player skin?": it caches
/// the answer at *learn* time. `0x4b25e0`, right after setting the known-spell bit, tests the
/// freshly-learned spell's `Effect[0]` and stores the spell id into a dedicated global —
/// `0x5f` (`SPELL_EFFECT_SKINNING`) → `[0xb700e4]`, `0x74` (`SPELL_EFFECT_SKIN_PLAYER_CORPSE`) →
/// `[0xb700e8]` — and the unlearn path `0x4b2c50` zeroes whichever global named that spell.
///
/// The world cursor's skin leg then reads `[0xb700e4 + 4×isPlayerTarget]` as a hard precondition
/// (wow-re `cursor-system.md` §3, the skin/insignia row): **a corpse flagged `UNIT_FLAG_SKINNABLE`
/// shows no skin cursor at all to a player who never learned Skinning.** Without it the ladder
/// offers the knife to everyone, which is what the channel reported.
#[derive(Resource, Default)]
pub(crate) struct LearnedAbilities {
    /// `[0xb700e4]` — our known `SPELL_EFFECT_SKINNING` spell (creature skinning), `None` if we
    /// never learned one. It is also the spell the skin click casts, so there is one lookup here,
    /// not a second scan at the click.
    pub(crate) skinning: Option<u32>,
    /// `[0xb700e8]` — our known `SPELL_EFFECT_SKIN_PLAYER_CORPSE` spell (the PvP insignia). Kept
    /// for symmetry with the reference's pair; the insignia arm of the cursor isn't modelled yet.
    pub(crate) skin_player_corpse: Option<u32>,
    /// `[0xcecad8]` — our known `SPELL_EFFECT_FEED_PET` spell (Feed Pet 6991), `None` if we never
    /// learned one. The reference latches it the same way, at learn time: `0x6ea1d0` (←
    /// `0x5e9e49`) stores the spell whose `Effect[0] == 0x65`. It is the **third gate** on
    /// [`targeting::drop_item_on_unit`]'s pet leg, and the spell that leg casts — so, like
    /// `skinning`, one lookup here rather than a second scan at the drop.
    pub(crate) feed_pet: Option<u32>,
}

/// `SpellEffects` value `0x74` — `SPELL_EFFECT_SKIN_PLAYER_CORPSE` (the "Remove Insignia" family),
/// the second of the two effects `0x4b25e0` latches.
const SPELL_EFFECT_SKIN_PLAYER_CORPSE: u32 = 0x74;

/// `SpellEffects` value `0x65` (101) — `SPELL_EFFECT_FEED_PET`, the effect the reference tests at
/// learn time to latch `[0xcecad8]` (wow-re `ui/scratch/item-target-cursor-and-dropitemonunit.md`).
/// Feed Pet 6991 is the only shipped row carrying it.
const SPELL_EFFECT_FEED_PET: u32 = 0x65;

/// Re-derive [`LearnedAbilities`] whenever the spell book changes — our stand-in for the
/// reference's learn/unlearn write sites. Change-detected, so it is a no-op on almost every frame;
/// rescanning the book beats threading a hook through every spell-arrival path, and it cannot
/// drift from the book the way an incrementally-maintained latch could.
fn track_learned_abilities(
    actions: Res<PlayerActions>,
    spells: Option<Res<Spells>>,
    mut learned: ResMut<LearnedAbilities>,
) {
    let Some(spells) = spells else { return };
    if !actions.is_changed() && !spells.is_changed() {
        return;
    }
    // The **last** matching spell in ascending-id order, not the first: the reference latches at
    // learn time and each new rank overwrites the global, so what it holds is the newest rank
    // learned — and a rank chain is ascending by id (Skinning 8613 → 8617 → 8618 → 10768). Which
    // end we take is only visible when a chain's ranks are known together, but "arbitrary" was
    // never an answer: the set was a `HashSet` until 1312 and this picked at the hash's whim.
    let last_with = |effect: u32| {
        actions.spells.iter().copied().rfind(|&id| {
            spells
                .catalog
                .get(id)
                .is_some_and(|d| d.effects[0] == effect)
        })
    };
    let (skinning, skin_player_corpse, feed_pet) = (
        last_with(benilla_formats::SPELL_EFFECT_SKINNING),
        last_with(SPELL_EFFECT_SKIN_PLAYER_CORPSE),
        last_with(SPELL_EFFECT_FEED_PET),
    );
    if (skinning, skin_player_corpse, feed_pet)
        != (
            learned.skinning,
            learned.skin_player_corpse,
            learned.feed_pet,
        )
    {
        *learned = LearnedAbilities {
            skinning,
            skin_player_corpse,
            feed_pet,
        };
    }
}

pub(crate) struct UiActionPlugin;

impl Plugin for UiActionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PlayerActions>()
            .init_resource::<LearnedAbilities>()
            .init_resource::<CastErrors>()
            .init_resource::<MountErrors>()
            .init_resource::<UiErrorKeys>()
            .init_resource::<UiErrorTexts>()
            .init_resource::<crate::cooldowns::Cooldowns>()
            .init_resource::<AutoRepeatActive>()
            .init_resource::<ChainCasts>()
            .init_resource::<cast_target::AutoSelfCast>()
            .init_resource::<targeting::SpellTargeting>()
            .init_resource::<targeting::EnchantConfirmItem>()
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
                    // The rank pass runs on the same `dirty` flag the identity feed consumes,
                    // and strictly before it: a slot corrected here is resolved and pushed with
                    // its right rank the same frame, so a stale rank never reaches a pixel
                    // (decision 0883).
                    ranks::normalize_action_ranks
                        .in_set(UnitFeed)
                        .before(feed::feed_actions),
                    feed::feed_actions.in_set(UnitFeed).before(UiInput),
                    state::feed_action_state
                        .in_set(UnitFeed)
                        .in_set(CooldownEvents)
                        .after(feed::feed_actions)
                        .before(UiInput),
                    drain::drain_action_sets.after(UiInput),
                    drain::drain_action_uses.after(UiInput),
                    // The T binding (0997): the attack arm's twin door, after the dispatch wrote
                    // this frame's fires.
                    drain::attack_target_binding.after(UiInput),
                    // The `modalNextSpell` chain (1597): a hunter shot's CAST_RESULT queues
                    // Auto Shot, and it goes out through the same ladder every other caster
                    // takes. After the input pass like the other drains — the queue is filled by
                    // the net drain, which runs earlier in the frame.
                    drain::drain_chain_casts.after(UiInput),
                    // The learned-ability latches must be current before the target chain's
                    // cursor classifier reads them; the book feed runs in `UnitFeed`, so sitting
                    // right after it is enough.
                    track_learned_abilities
                        .in_set(UnitFeed)
                        .after(feed::feed_actions),
                    // The targeting mode's ESC-chain halves (decision 0792): the state push
                    // rides the feeds (before the input pass runs `ToggleGameMenu`), the
                    // trigger drain follows it — same frame, so an ESC's cancel lands before
                    // the next frame's cursor drive reads the mode. The cursor pre-empt, the
                    // right-press cancel, and the click commit register in the TARGET chain
                    // (ordering against the classifier and the select click is theirs to own).
                    targeting::feed_targeting_to_vm
                        .in_set(UnitFeed)
                        .before(UiInput),
                    targeting::drain_stop_targeting.after(UiInput),
                    // The item half's commit (decision 0923) — the bag / paper-doll click seam's
                    // `0x495d60`. A UI drain like the others: after the input pass, so a click
                    // this frame binds this frame. It is deliberately NOT in the target chain —
                    // the clicks it consumes never reach the world.
                    targeting::commit_item_cast_on_pick.after(UiInput),
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
