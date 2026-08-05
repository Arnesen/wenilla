//! The pet action bar's feed + drain — the app side of `benilla_ui::script::pet`'s seam
//! (decision 0982), and the home of [`PetBar`], the server-authoritative state
//! `net/apply/pet.rs` writes.
//!
//! It also feeds the **`"pet"` unit token** and the pet frame's events ([`feed_pet_unit`],
//! decision 0990). That lives here rather than beside the `"player"`/`"target"` feed for one
//! reason: the token's identity is [`PetBar`]'s cached pet guid — the client's `[0xb714a0]`, which
//! is also what `UNIT_PET` fires off (wow-re §9) — so the token and its repaint wire read the same
//! word, from the file that owns it.
//!
//! **The shape of this system is the stance bar's mirror image, and that is the thing to hold on
//! to.** `crate::ui_shapeshift` derives its whole bar locally, from the known-spell set × the
//! `Spell.dbc` catalog, and the server never has an opinion about it. Here the server owns the
//! bar's CONTENTS — which slots exist and what is in them — and hands them over whole in
//! `SMSG_PET_SPELLS`. So the feed does no admission and no ordering: it *renders* the ten words
//! the last packet delivered.
//!
//! **But the bar's STATE — the lit command, the lit reaction, the autocast bit — is applied
//! LOCALLY, and that is forced rather than chosen.** The server answers none of those three
//! presses: `HandlePetAction`'s command and reaction arms send nothing back,
//! `HandlePetSpellAutocastOpcode` sets its bits and returns, and `SMSG_PET_MODE` is emitted from
//! exactly one place in the whole server (`Pet::SetEnabled` — the enabled flag, nothing else). A
//! client that waited for confirmation would show a bar whose reaction buttons never light and
//! whose autocast ring never appears. Verified live against vmangos, 2026-08-05: a
//! `TogglePetAutocast` and a Follow press drew no reply packet at all. [`drain_pet_actions`]
//! carries that half, and the next `SMSG_PET_SPELLS` re-authorities it.
//!
//! **The slot law** (decode and constants in `benilla_protocol::messages::pet`, VERIFIED against
//! the client itself in decision 0988): each slot is one packed word whose type — bits 24–29,
//! **masked** — decides its whole reading:
//!
//! - `PET_ACT_COMMAND 7` — `action` is a `CommandStates`: Stay 0 / Follow 1 / Attack 2 / Dismiss 3.
//!   A token: `GetPetActionInfo` returns the NAMES of two globals rather than values (the
//!   reference's own `PetActionBarFrame.lua:98-104` fork), which `PetActionBar.xml` resolves —
//!   `PET_ACTION_*` out of the shipped `GlobalStrings.lua`, `PET_*_TEXTURE` out of the constants
//!   that file declares and ours quotes.
//! - `PET_ACT_REACTION 6` — `action` is a `ReactStates`: Passive 0 / Defensive 1 / Aggressive 2.
//!   Also a token.
//! - types **1–5** — `action` is a spell id, and the autocast pair rides in bits 31/30 rather than
//!   in the type. That split is why the server's `ACT_PASSIVE`/`ACT_DISABLED`/`ACT_ENABLED` are
//!   one branch here and not three.
//!
//! The two halves of `GetPetActionInfo`'s signature never overlap: `isActive` is a **token**
//! concept and `autoCast*` a **spell** one, each nil on the other's branch.
//!
//! The **3 commands / 4 spells / 3 reactions** arrangement everyone recognises is therefore
//! *server data*, not a layout: it is what `CharmInfo::InitPetActionBar` happens to write, and a
//! possessed minion or a charmed creature arrives with a different mix through the same ten words.
//! Nothing in this file or in `PetActionBar.xml` assumes it.

use std::time::Instant;

use bevy::prelude::*;

use benilla_protocol::messages::{
    PetActionEntry, PetSpells, PET_COMMAND_ATTACK, PET_COMMAND_DISMISS, PET_COMMAND_FOLLOW,
    PET_COMMAND_STAY, PET_REACT_AGGRESSIVE, PET_REACT_DEFENSIVE, PET_REACT_PASSIVE,
};
use benilla_ui::script::{PetActionView, ScriptValue, UiScript, UnitState};

use crate::cooldowns::Cooldowns;
use crate::names::NameCache;
use crate::net::{ClientCommand, GuidIndex, NetCommands, ObjectStore};
use crate::target::Selection;
use crate::ui_action::Spells;
use crate::ui_script::UiInput;
use crate::ui_unit::{fire_transitions, snapshot, UnitFeed};

/// The pet action bar's mirror of the server's state — written only by `net/apply/pet.rs`, read
/// only by [`feed_pet_bar`] and [`drain_pet_actions`].
///
/// `spells.pet_guid == 0` is "there is no pet bar", and it is the single test: the teardown packet
/// carries exactly that and nothing else.
#[derive(Resource, Default)]
pub(crate) struct PetBar {
    /// The last `SMSG_PET_SPELLS` in full, with `SMSG_PET_MODE`'s state edits folded in.
    pub(crate) spells: PetSpells,
    /// The **pet's own** cooldown store — a second [`Cooldowns`] instance, not the player's. The
    /// two must not share: a hunter and their pet both know Growl-shaped spells on independent
    /// timers, and the reference keeps a `SPELLHISTORY` list per unit for exactly this reason.
    /// Seeded from the `SMSG_PET_SPELLS` tail and topped up by `SMSG_SPELL_COOLDOWN` addressed to
    /// the pet's guid.
    pub(crate) cooldowns: Cooldowns,
    /// **"The pet is attacking"** — the client's own `[0xb714b0]`, and a purely local latch with
    /// no field behind it anywhere (wow-re §1).
    ///
    /// It has exactly three writers, and reproducing them is the whole of `IsPetAttackActive`:
    /// set by an ATTACK command press (`0x4bd42e`), cleared by `PetStopAttack` (`0x4bd6ae`), and
    /// cleared on **any** pet-guid change (`0x4bc8ce`) — a new pet is not attacking.
    ///
    /// benilla read this off the pet's streamed `UNIT_FIELD_TARGET` until the RE landed. That is a
    /// different question with a different answer: a defensive pet that retaliates on its own has
    /// a target the player never ordered, and the reference does not light the Attack button for
    /// it.
    pub(crate) attacking: bool,
}

impl PetBar {
    /// `PetHasActionBar()` — is there a bar at all. The client's own gate is exactly this, a
    /// nonzero cached pet guid: no alive check, no control check (wow-re §3).
    pub(crate) fn has_bar(&self) -> bool {
        self.spells.pet_guid != 0
    }
}

/// `GetPetActionsUsable()` — may the bar be used at all (wow-re §4, the predicate `0x4bcf70`).
///
/// benilla's earlier reading — "the enabled-flags byte's `0x8`" — was REFUTED by the RE as the
/// *whole* answer, but it survives as one of the seven steps: the client tests bit 27 of the state
/// dword, which is that same byte's `0x8` (see `PET_STATE_BAR_DISABLED`). The step it was missing
/// is the pet's own crowd-control state — a **stunned, confused or feared** pet cannot be ordered,
/// and its bar greys until it recovers.
///
/// The remaining four steps are ownership identity (the player resolves, the pet resolves, the pet
/// is ours, we are not ourselves charmed). Holding a bar at all already means the server named us
/// this pet's controller, so they are structurally true here; the two that can actually change
/// while a bar is on screen are the two we test.
///
/// This is the same predicate that gates whether a press does anything, so a false answer greys
/// the icons *and* is the honest reason a click would be ignored.
fn actions_usable(bar: &PetBar, pet_flags: Option<u32>) -> bool {
    !bar.spells.bar_disabled()
        && pet_flags.is_none_or(|f| f & benilla_protocol::messages::PET_UNUSABLE_UNIT_FLAGS == 0)
}

/// What the feed last pushed, so `PET_BAR_UPDATE` fires on a real change rather than every frame
/// (the [`crate::ui_shapeshift`] memory pattern).
#[derive(Default)]
struct PetBarMemory {
    pushed: Option<(bool, bool, Vec<PetActionView>)>,
}

pub(crate) struct UiPetPlugin;

impl Plugin for UiPetPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PetBar>().add_systems(
            Update,
            (
                // Both feeds ride with the unit feed (before the VM ticks, like the stance bar's);
                // the drain runs after the input pass so a click goes out the same frame.
                feed_pet_bar.in_set(UnitFeed).before(UiInput),
                feed_pet_unit.in_set(UnitFeed).before(UiInput),
                drain_pet_actions.after(UiInput),
            ),
        );
    }
}

/// The command tokens' `(GlobalStrings key, texture-global name)` pair.
///
/// Both are returned as **the names of globals**, not values — the reference's token convention.
/// The name keys ship in `GlobalStrings.lua` (`PET_ACTION_ATTACK` = "Attack" at l.3029-3032); the
/// texture globals are declared by `PetActionBar.xml`, quoting the reference's
/// `PetActionBarFrame.lua:6-12` verbatim.
fn command_token(action: u32) -> Option<(&'static str, &'static str)> {
    Some(match action {
        PET_COMMAND_STAY => ("PET_ACTION_WAIT", "PET_WAIT_TEXTURE"),
        PET_COMMAND_FOLLOW => ("PET_ACTION_FOLLOW", "PET_FOLLOW_TEXTURE"),
        PET_COMMAND_ATTACK => ("PET_ACTION_ATTACK", "PET_ATTACK_TEXTURE"),
        PET_COMMAND_DISMISS => ("PET_ACTION_DISMISS", "PET_DISMISS_TEXTURE"),
        _ => return None,
    })
}

/// The reaction tokens' pair, same convention. `PET_MODE_*` ship in `GlobalStrings.lua`
/// (l.3045-3047) — deliberately the `PET_MODE_*` keys, which name the state the pet is IN, not the
/// `PET_AGGRESSIVE`/`PET_DEFENSIVE`/`PET_PASSIVE` keys, which are the right-click menu's
/// imperatives. Both read identically in enUS ("Aggressive"), so only a localized client can tell
/// them apart — which is exactly why the key matters rather than the string.
fn reaction_token(action: u32) -> Option<(&'static str, &'static str)> {
    Some(match action {
        PET_REACT_PASSIVE => ("PET_MODE_PASSIVE", "PET_PASSIVE_TEXTURE"),
        PET_REACT_DEFENSIVE => ("PET_MODE_DEFENSIVE", "PET_DEFENSIVE_TEXTURE"),
        PET_REACT_AGGRESSIVE => ("PET_MODE_AGGRESSIVE", "PET_AGGRESSIVE_TEXTURE"),
        _ => return None,
    })
}

/// Resolve one packed slot word into what the bar draws.
///
/// `cooldown` is passed in rather than looked up here so the whole function stays a pure
/// (state, word) → view mapping, which is what the tests below exercise.
fn slot_view(
    entry: PetActionEntry,
    bar: &PetSpells,
    spell: Option<&benilla_formats::SpellDisplay>,
    cooldown: Option<(i64, u32, bool)>,
    pet_attacking: bool,
) -> PetActionView {
    let kind = entry.kind();
    let action = entry.action();

    if let Some((name, texture)) = (kind == benilla_protocol::messages::PET_ACT_COMMAND)
        .then(|| command_token(action))
        .flatten()
    {
        // A command token lights on `(state >> 8) == action` **or** on the attack latch
        // (wow-re §2.3, `0x4bdf03`-`0x4bdf22`). Two things live in that `or`:
        //
        // - the compare is against the UNMASKED `state >> 8` (`PetSpells::command_state`'s own
        //   note), so a disabled bar puts every command button out;
        // - ATTACK gets the extra clause, which is why the pet can be *told* to Stay and still
        //   show Attack lit while it is on something.
        //
        // `attack_active` — `IsPetAttackActive`, the click fork — is the same latch narrowed to
        // this slot (wow-re §5: type 7, action 2, and the flag). Same input, so the button that
        // lights is exactly the button whose next press calls the pet off.
        let attacking = pet_attacking && action == PET_COMMAND_ATTACK;
        return PetActionView {
            name: Some(name.to_string()),
            texture: Some(texture.to_string()),
            is_token: true,
            active: bar.command_state() == action || attacking,
            attack_active: attacking,
            ..Default::default()
        };
    }

    if let Some((name, texture)) = (kind == benilla_protocol::messages::PET_ACT_REACTION)
        .then(|| reaction_token(action))
        .flatten()
    {
        // The reaction compare's left side is forced to Passive when the bar is disabled
        // (wow-re §2.2, `0x4bde3c`): a pet that cannot be ordered reads as Passive rather than
        // keeping the mode light it had, which is the honest thing for it to say.
        let showing = if bar.bar_disabled() {
            benilla_protocol::messages::PET_REACT_PASSIVE
        } else {
            bar.react_state()
        };
        return PetActionView {
            name: Some(name.to_string()),
            texture: Some(texture.to_string()),
            is_token: true,
            active: showing == action,
            ..Default::default()
        };
    }

    // A spell slot. `is_empty` is the zero WORD (the client tests the dword); vmangos's own unused
    // middle slots are not zero and arrive here instead, where their spell id 0 misses the catalog
    // and takes the same exit — the client's own route to the same empty button.
    if !entry.is_spell() || entry.is_empty() {
        return PetActionView::default();
    }
    let Some(spell) = spell else {
        // The catalog failed to load, or the server named a spell 5875's DBC does not have. Draw
        // the slot as occupied but nameless rather than inventing a name: the button then hides,
        // which is honest, and the alternative (a "?" with no tooltip) has fooled nobody.
        return PetActionView::default();
    };
    PetActionView {
        name: Some(spell.name.clone()),
        subtext: spell.rank.clone(),
        texture: spell.icon.clone(),
        is_token: false,
        spell_id: Some(action),
        // A spell slot NEVER reports isActive — VERIFIED nil on every path (wow-re §2.1, pushed
        // at `0x4bdd5e`), which retires 0982's INTERIM. `isActive` is exclusively a token
        // concept and `autoCast*` exclusively a spell one; the two halves of the signature never
        // overlap (§2.5 quirk 3).
        //
        // The client does express "the pet is running this spell", but as an ICON swap, not a
        // ring: `GetPetActionInfo` substitutes the record's `ActiveIconID` for its `SpellIconID`
        // when the spell is a live aura on the pet with its aura-flag bit set (`0x4bcea0`). That
        // needs an aura-indexed icon we do not carry yet — named in decision 0988, not faked here.
        active: false,
        // Autocast is bits 31/30 of the word, not the type byte (wow-re §2.1) — and both are
        // additionally gated on the spell resolving in `Spell.dbc`, which the early return above
        // has already enforced by the time we get here.
        autocast_allowed: entry.autocast_allowed(),
        autocast_enabled: entry.autocast_on(),
        attack_active: false,
        cooldown,
    }
}

/// Rebuild the ten slot views each frame and diff-push them, firing `PET_BAR_UPDATE` on a change.
///
/// One event where the reference has four (`PET_BAR_UPDATE`, `PET_BAR_UPDATE_COOLDOWN`,
/// `UNIT_PET`, and the `UNIT_FLAGS`/`UNIT_AURA` pair its bar filters for `arg1 == "pet"`) — the
/// deliberate collapse `crate::ui_shapeshift` already makes for the stance bar, and for the same
/// reason: we diff the whole pushed state, so one event carries every change there can be.
#[allow(clippy::too_many_arguments)]
fn feed_pet_bar(
    script: Option<NonSendMut<UiScript>>,
    bar: Res<PetBar>,
    spells: Option<Res<Spells>>,
    clock: Res<crate::ui_script::UiClock>,
    index: Res<GuidIndex>,
    stores: Query<&ObjectStore>,
    mut memory: Local<PetBarMemory>,
) {
    let Some(mut script) = script else {
        return;
    };
    let now = Instant::now();
    let (anchor, ui_now) = (clock.anchor, clock.ui_now);
    let has_bar = bar.has_bar();
    // The pet's own `UNIT_FIELD_FLAGS`, for the crowd-control half of the usability predicate.
    // `None` = we hold a bar for a unit whose descriptor has not arrived (or has left); the
    // predicate then rests on bit 27 alone rather than greying a bar on missing data.
    let pet_flags = index
        .0
        .get(&bar.spells.pet_guid)
        .and_then(|&e| stores.get(e).ok())
        .map(|s| s.0.unit_flags());
    let usable = actions_usable(&bar, pet_flags);
    let pet_attacking = bar.attacking;

    let fresh: Vec<PetActionView> = if has_bar {
        bar.spells
            .bar
            .iter()
            .map(|&entry| {
                let display = entry
                    .is_spell()
                    .then(|| spells.as_ref().and_then(|s| s.catalog.get(entry.action())))
                    .flatten();
                let cooldown = display.and_then(|d| {
                    bar.cooldowns
                        .info(entry.action(), 0, Some(d), now)
                        .ui_triple(anchor, ui_now)
                });
                slot_view(entry, &bar.spells, display, cooldown, pet_attacking)
            })
            .collect()
    } else {
        Vec::new()
    };

    if memory.pushed.as_ref() != Some(&(has_bar, usable, fresh.clone())) {
        debug!(
            "ui_pet: bar {} ({} occupied slot(s), {})",
            if has_bar { "shown" } else { "hidden" },
            fresh.iter().filter(|s| s.name.is_some()).count(),
            if usable { "usable" } else { "disabled" },
        );
        memory.pushed = Some((has_bar, usable, fresh.clone()));
        script.set_pet_actions(has_bar, usable, fresh);
        script.fire_event("PET_BAR_UPDATE", vec![]);
    }
}

/// What the `"pet"` token feed last pushed — the three edges it fires on (decision 0990).
#[derive(Default)]
struct PetUnitMemory {
    /// The last snapshot pushed under `"pet"`, for [`fire_transitions`]' per-field diff.
    pushed: Option<UnitState>,
    /// The last pet guid — `UNIT_PET`'s trigger. `None` until the first feed, so a login with a
    /// pet already out still announces it once.
    guid: Option<u64>,
    /// The last attack-latch state — the `PET_ATTACK_START`/`PET_ATTACK_STOP` pair's trigger.
    attacking: bool,
}

/// Feed the **`"pet"` unit token** and the pet frame's three events (decision 0990).
///
/// **The token resolves off the bar's cached pet guid, not off our own `UNIT_FIELD_SUMMON`**, and
/// that is the client's own choice rather than a convenience. wow-re §9 carves `UNIT_PET` as firing
/// from inside `SetPet 0x4bc7e0` (`0x4bc84f`: `SignalEvent(2, "%s", "player")`) — the single writer
/// of `[0xb714a0]`, the same cached guid the whole pet bar reads — and **only when that guid
/// actually changed**. Since `UNIT_PET` is the pet frame's only repaint wire, a token sourced from
/// anywhere else would repaint on the wrong edges. Reading both off one guid is what keeps them in
/// step, and it is also what makes a **possessed or charmed** unit — which has a bar but is nobody's
/// `UNIT_FIELD_SUMMON` — carry a frame at all.
///
/// The reaction argument is `0`: the pet frame reads no reaction (only `"target"` resolves one —
/// [`crate::ui_unit::feed_units`]' own note), and the party feed passes the same for the same
/// reason.
fn feed_pet_unit(
    script: Option<NonSendMut<UiScript>>,
    bar: Res<PetBar>,
    index: Res<GuidIndex>,
    stores: Query<&ObjectStore>,
    mut names: ResMut<NameCache>,
    commands: Res<NetCommands>,
    mut memory: Local<PetUnitMemory>,
) {
    let Some(mut script) = script else {
        return;
    };
    let pet_guid = bar.spells.pet_guid;
    // A bar whose unit has not streamed yet (or has left) pushes nothing: `UnitExists("pet")` then
    // reads false and the frame hides, which is honest — we have the guid but none of the fields
    // the frame draws.
    let fresh = (pet_guid != 0)
        .then(|| index.0.get(&pet_guid))
        .flatten()
        .and_then(|&e| stores.get(e).ok())
        .map(|store| {
            let name = names.resolve(pet_guid, &commands).map(str::to_string);
            let mut s = snapshot(store, name, 0);
            s.guid = pet_guid;
            s
        });

    script.set_unit("pet", fresh.clone());
    match &fresh {
        Some(cur) => {
            if memory.pushed.as_ref() != Some(cur) {
                // The one line worth having here, and it is about the failure that is otherwise
                // SILENT: holding a bar for a guid whose descriptor never streamed leaves
                // `UnitExists("pet")` false, so the frame simply never appears and nothing says
                // why. Naming the resolved unit on each change makes "the bar is up but the frame
                // is not" a one-grep question.
                if memory.pushed.is_none() {
                    debug!(
                        "ui_pet: \"pet\" resolved — {} ({}/{} hp)",
                        cur.name.as_deref().unwrap_or("<name pending>"),
                        cur.health,
                        cur.max_health,
                    );
                }
                fire_transitions(&mut script, "pet", memory.pushed.as_ref(), cur);
                memory.pushed = Some(cur.clone());
            }
        }
        // Clearing a token is not a UNIT_* event — the frame reacts to UNIT_PET below, exactly as
        // the target frame reacts to PLAYER_TARGET_CHANGED rather than to a cleared snapshot.
        None => memory.pushed = None,
    }

    // UNIT_PET(arg1 = "player") — VERIFIED wow-re §9, including the `arg1` and the changed-guid
    // gate. Summon, stable swap and dismiss are the three edges; a re-sent `SMSG_PET_SPELLS` for
    // the same pet (a learned spell, a mode change) is not one, which is why this diffs the guid
    // rather than riding `feed_pet_bar`'s whole-state diff.
    if memory.guid != Some(pet_guid) {
        memory.guid = Some(pet_guid);
        debug!("ui_pet: UNIT_PET — pet is now {pet_guid:#x}");
        script.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);
    }

    // PET_ATTACK_START / PET_ATTACK_STOP — the pet frame's flashing `UI-Player-AttackStatus`
    // overlay. DERIVED, not carved: the two events' own fire sites are outside the pet TU and
    // wow-re has not pinned them, but the attack latch `[0xb714b0]` IS "the pet is attacking" and
    // its three writers are carved exhaustively (§1, and [`PetBar::attacking`]), so its edges are
    // the honest source. Named as derived in decision 0990 rather than claimed verified.
    if memory.attacking != bar.attacking {
        memory.attacking = bar.attacking;
        script.fire_event(
            if bar.attacking {
                "PET_ATTACK_START"
            } else {
                "PET_ATTACK_STOP"
            },
            vec![],
        );
    }
}

/// Drain the bar's three intents onto the wire.
///
/// Every one carries the pet's guid from [`PetBar`] — never a guid the VM supplied — so an intent
/// queued in the frame a pet was dismissed dies here rather than naming a unit that is gone.
///
/// `CMSG_PET_ACTION` echoes the slot's **own packed word**: the server re-splits it and dispatches
/// on the type byte, so command, reaction and cast all leave through this one send. The target is
/// our current selection, which is what makes "select a mob, press Attack" work; a slot that wants
/// no target simply has the server ignore it (`HandlePetAction`'s `explicitlySelectedTarget` fork
/// drops a target the spell does not want).
///
/// **This drain is also where the bar's STATE moves, and that is not an optimisation — it is
/// forced.** The server answers none of these three: `HandlePetAction`'s command and reaction arms
/// send nothing back, and `HandlePetSpellAutocastOpcode` sets its bits and returns
/// (`PetHandler.cpp:451-478`). `SMSG_PET_MODE` is emitted from exactly ONE place in the whole
/// server — `Pet::SetEnabled`, i.e. only when the bar is enabled or disabled
/// (`Pet.cpp:2362-2377`). So a client that waited for confirmation would show a pet bar whose
/// reaction buttons never light and whose autocast ring never appears; the reference plainly does
/// not, so it must apply all three locally, and so do we. Verified live, 2026-08-05: a
/// `TogglePetAutocast` + a Follow press against vmangos drew no packet whatsoever in reply.
///
/// The optimism is bounded by the same packet that owns everything else — the next
/// `SMSG_PET_SPELLS` (a re-summon, a learn, a stable swap) replaces state and contents together.
fn drain_pet_actions(
    script: Option<NonSendMut<UiScript>>,
    mut bar: ResMut<PetBar>,
    selection: Res<Selection>,
    commands: Res<NetCommands>,
) {
    let Some(mut script) = script else {
        return;
    };
    let pressed = script.take_pet_actions();
    let toggles = script.take_pet_autocast_toggles();
    let stops = script.take_pet_stop_attacks();
    if pressed.is_empty() && toggles.is_empty() && stops == 0 {
        return;
    }
    let pet_guid = bar.spells.pet_guid;
    if pet_guid == 0 {
        debug!("ui_pet: dropping queued pet intents — the bar is gone");
        return;
    }
    let target_guid = selection.guid.unwrap_or(0);

    for slot in pressed {
        let Some(entry) = slot_entry(&bar, slot) else {
            continue;
        };
        debug!(
            "ui_pet: press slot {slot} (action {} kind {:#04x}) at {target_guid:#x}",
            entry.action(),
            entry.kind()
        );
        let _ = commands.0.send(ClientCommand::PetAction {
            pet_guid,
            packed: entry.packed,
            target_guid,
        });
        latch_press(&mut bar, entry);
    }
    for slot in toggles {
        let Some(entry) = slot_entry(&bar, slot).filter(|e| e.autocast_allowed()) else {
            continue;
        };
        // The client flips bit 30 in the slot word IN PLACE and sends the whole new word — it is
        // not a "set autocast to X for spell Y" verb (wow-re §10.2, `0x4bcbff`/`0x4bcc17`). The
        // server reads the direction back out of the type byte it arrives in.
        let flipped = entry.with_autocast(!entry.autocast_on());
        debug!(
            "ui_pet: autocast {} for spell {} (slot {slot})",
            flipped.autocast_on(),
            entry.action()
        );
        let _ = commands.0.send(ClientCommand::PetSetAction {
            pet_guid,
            // The wire's slot positions are 0-based (vmangos bounds them `< 10`); the Lua's are
            // 1-based. This subtraction is the whole of the conversion and cannot underflow —
            // `slot_entry` already rejected 0.
            entries: vec![(slot - 1, flipped.packed)],
        });
        if let Some(e) = slot_entry_mut(&mut bar, slot) {
            *e = flipped;
        }
    }
    for _ in 0..stops {
        // `PetStopAttack` no-ops entirely when the pet is not attacking (`0x4bd65e`) — no packet,
        // no repaint. That gate lives here rather than in the VM because the latch does.
        if !bar.attacking {
            continue;
        }
        debug!("ui_pet: stop attack");
        let _ = commands.0.send(ClientCommand::PetStopAttack { pet_guid });
        bar.attacking = false;
    }
}

/// Latch a pressed slot's state locally — the half the server never confirms (the module doc's
/// "forced rather than chosen"), now with the client's own masks rather than byte assignments.
///
/// Both writes are exact transcriptions, and the masks are the point:
///
/// - **reaction** (`0x4bc94c`): `state = state & 0xFFFFFF00 | action` — byte 0 only.
/// - **command** (`0x4bc96f`): `state = state & 0x080000FF | action << 8` — bits 8–15, keeping
///   byte 0 and **bit 27**. The client deliberately preserves the disabled bit while rewriting the
///   command, which is also the proof that bit 27 is server-owned: no client path writes it.
///
/// **Only STAY and FOLLOW reach the command write.** The binary's type-7 arm gates it on
/// `action <= 1` (§10.1): DISMISS (3) falls straight through, and ATTACK (2) leaves down the
/// validation chain that ends at `[0xb714b0] = 1` (`0x4bd42e`) — the attack latch, never the
/// command byte. The distinction is the difference between an order and a mode: Stay and Follow
/// are what the pet is *doing until told otherwise*; Attack is a thing you *tell it once*, and its
/// light comes from the latch, which `PetStopAttack` and a new pet are the only things that clear.
///
/// Getting this wrong is visible within one click: latching ATTACK into the command byte leaves
/// the Attack button lit forever (`isActive`'s state compare keeps matching), blanks Follow and
/// Stay, and survives the `PetStopAttack` that should have put it out.
///
/// The latch is raised unconditionally here, where the client first clears two gates —
/// `0x612df0` (which also picks the target, and vetoes the whole press when it refuses) and
/// `0x5ee5a0`. Both are UNRESOLVED in wow-re's own note, so this is the all-gates-pass case and
/// nothing else; a refused attack currently lights the button where the real client would not.
fn latch_press(bar: &mut PetBar, entry: PetActionEntry) {
    let action = entry.action();
    match entry.kind() {
        benilla_protocol::messages::PET_ACT_COMMAND
            if action == PET_COMMAND_STAY || action == PET_COMMAND_FOLLOW =>
        {
            bar.spells.state = (bar.spells.state & 0x0800_00FF) | (action << 8);
        }
        benilla_protocol::messages::PET_ACT_COMMAND if action == PET_COMMAND_ATTACK => {
            bar.attacking = true;
        }
        benilla_protocol::messages::PET_ACT_REACTION => {
            bar.spells.state = (bar.spells.state & 0xFFFF_FF00) | action;
        }
        _ => {}
    }
}

/// The 1-based Lua slot index → the packed word we still hold for it.
fn slot_entry(bar: &PetBar, slot: u32) -> Option<PetActionEntry> {
    let index = usize::try_from(slot.checked_sub(1)?).ok()?;
    bar.spells.bar.get(index).copied()
}

/// [`slot_entry`]'s mutable twin, for the autocast flip's write-back.
fn slot_entry_mut(bar: &mut PetBar, slot: u32) -> Option<&mut PetActionEntry> {
    let index = usize::try_from(slot.checked_sub(1)?).ok()?;
    bar.spells.bar.get_mut(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_protocol::messages::{
        PET_ACT_COMMAND, PET_ACT_DISABLED, PET_ACT_ENABLED, PET_ACT_PASSIVE, PET_ACT_REACTION,
        PET_STATE_BAR_DISABLED,
    };

    fn packed(action: u32, kind: u8) -> PetActionEntry {
        PetActionEntry::from(action | (u32::from(kind) << 24))
    }

    fn spell(name: &str, rank: Option<&str>) -> benilla_formats::SpellDisplay {
        benilla_formats::SpellDisplay {
            name: name.to_string(),
            rank: rank.map(str::to_string),
            icon: Some("Interface\\Icons\\Ability_Druid_Rake".into()),
            ..Default::default()
        }
    }

    /// The state dword as the server packs it: react in byte 0, command in byte 1.
    fn state(command: u32, react: u32) -> PetSpells {
        PetSpells {
            pet_guid: 0x2A,
            state: react | (command << 8),
            ..Default::default()
        }
    }

    /// A command token returns GLOBAL NAMES for both name and texture, and is lit exactly when it
    /// IS the pet's current command — the whole reason three command buttons can share one state.
    #[test]
    fn command_tokens_light_on_the_current_command() {
        let bar = state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE);

        let follow = slot_view(
            packed(PET_COMMAND_FOLLOW, PET_ACT_COMMAND),
            &bar,
            None,
            None,
            false,
        );
        assert_eq!(follow.name.as_deref(), Some("PET_ACTION_FOLLOW"));
        assert_eq!(follow.texture.as_deref(), Some("PET_FOLLOW_TEXTURE"));
        assert!(follow.is_token && follow.active);
        assert!(!follow.attack_active, "Follow is not the attack fork");

        let stay = slot_view(
            packed(PET_COMMAND_STAY, PET_ACT_COMMAND),
            &bar,
            None,
            None,
            false,
        );
        assert!(!stay.active, "only the CURRENT command is lit");
    }

    /// The Attack button's second clause: the attack latch lights it and arms the call-off fork
    /// **independently of the command state**, which is how a pet told to Follow can still show
    /// Attack lit while it is on something.
    #[test]
    fn the_attack_latch_lights_attack_whatever_the_command_state_says() {
        let following = state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE);
        let attack = packed(PET_COMMAND_ATTACK, PET_ACT_COMMAND);

        let idle = slot_view(attack, &following, None, None, false);
        assert!(!idle.active && !idle.attack_active);

        let on = slot_view(attack, &following, None, None, true);
        assert!(on.active, "the latch lights it even on a FOLLOW command");
        assert!(on.attack_active, "and the next press calls the pet off");

        // The latch never reaches another command's button, however busy the pet is.
        let follow = slot_view(
            packed(PET_COMMAND_FOLLOW, PET_ACT_COMMAND),
            &following,
            None,
            None,
            true,
        );
        assert!(!follow.attack_active);
        assert!(follow.active, "…but Follow is still the standing command");
    }

    /// Reaction tokens use the `PET_MODE_*` keys — the state words — not the right-click menu's
    /// `PET_AGGRESSIVE` imperatives. Identical in enUS, distinguishable only by key.
    #[test]
    fn reaction_tokens_use_the_mode_keys_and_light_on_the_current_react() {
        let bar = state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE);
        let def = slot_view(
            packed(PET_REACT_DEFENSIVE, PET_ACT_REACTION),
            &bar,
            None,
            None,
            false,
        );
        assert_eq!(def.name.as_deref(), Some("PET_MODE_DEFENSIVE"));
        assert_eq!(def.texture.as_deref(), Some("PET_DEFENSIVE_TEXTURE"));
        assert!(def.is_token && def.active);
        assert!(
            !slot_view(
                packed(PET_REACT_AGGRESSIVE, PET_ACT_REACTION),
                &bar,
                None,
                None,
                false
            )
            .active
        );
    }

    /// A DISABLED bar (bit 27) changes what both token classes report, and neither is a special
    /// case we wrote: the reaction compare's left side is forced to Passive, and the command
    /// compare — unmasked `state >> 8` — is put out of range of every command at once.
    #[test]
    fn a_disabled_bar_reads_passive_and_lights_no_command() {
        let mut bar = state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE);
        bar.state |= PET_STATE_BAR_DISABLED;

        let passive = slot_view(
            packed(PET_REACT_PASSIVE, PET_ACT_REACTION),
            &bar,
            None,
            None,
            false,
        );
        assert!(passive.active, "a bar that cannot be ordered reads Passive");
        assert!(
            !slot_view(
                packed(PET_REACT_DEFENSIVE, PET_ACT_REACTION),
                &bar,
                None,
                None,
                false
            )
            .active
        );

        assert!(
            !slot_view(
                packed(PET_COMMAND_FOLLOW, PET_ACT_COMMAND),
                &bar,
                None,
                None,
                false
            )
            .active,
            "the command it IS on goes dark too"
        );
    }

    /// `GetPetActionsUsable` — bit 27 OR the pet's own crowd-control flags. The second half is the
    /// leg benilla was missing: a feared/stunned/confused pet's bar greys.
    #[test]
    fn usability_is_the_disabled_bit_and_the_pets_crowd_control() {
        let mut bar = PetBar {
            spells: state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE),
            ..Default::default()
        };
        assert!(actions_usable(&bar, Some(0)));
        assert!(
            actions_usable(&bar, None),
            "a missing descriptor is not a no"
        );

        for flag in [0x0004_0000, 0x0040_0000, 0x0080_0000] {
            assert!(!actions_usable(&bar, Some(flag)), "flag {flag:#x} disables");
        }
        // POSSESSED is deliberately not one of them — that IS the pet-bar case.
        assert!(actions_usable(&bar, Some(0x0100_0000)));

        bar.spells.state |= PET_STATE_BAR_DISABLED;
        assert!(!actions_usable(&bar, Some(0)));
    }

    /// A spell slot: real name, rank subtext, icon PATH — and the autocast pair read off **bits
    /// 31/30**, not the type byte. `isActive` is nil on every spell path.
    #[test]
    fn spell_slots_read_their_autocast_off_bits_31_and_30() {
        let bar = state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE);
        let claw = spell("Claw", Some("Rank 3"));

        let on = slot_view(
            packed(3010, PET_ACT_ENABLED),
            &bar,
            Some(&claw),
            None,
            false,
        );
        assert_eq!(on.name.as_deref(), Some("Claw"));
        assert_eq!(on.subtext.as_deref(), Some("Rank 3"));
        assert_eq!(on.spell_id, Some(3010));
        assert!(!on.is_token);
        assert!(on.autocast_allowed && on.autocast_enabled);
        assert!(!on.active, "a SPELL slot never reports isActive");

        let off = slot_view(
            packed(3010, PET_ACT_DISABLED),
            &bar,
            Some(&claw),
            None,
            false,
        );
        assert!(off.autocast_allowed && !off.autocast_enabled);

        // A passive pet spell shows, but can never autocast — no ring, no sparkle.
        let passive = slot_view(
            packed(3010, PET_ACT_PASSIVE),
            &bar,
            Some(&claw),
            None,
            false,
        );
        assert!(!passive.autocast_allowed && !passive.autocast_enabled);
    }

    /// The two routes to an empty-looking button: the client's zero WORD, and vmangos's own
    /// `(0, ACT_DISABLED)` filler, which takes the spell branch and misses the catalog. Both must
    /// draw nothing — reading either one wrong puts four "?" buttons mid-bar.
    #[test]
    fn both_kinds_of_empty_slot_draw_nothing() {
        let bar = state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE);

        let zero = PetActionEntry::default();
        assert!(zero.is_empty());
        assert_eq!(
            slot_view(zero, &bar, None, None, false),
            PetActionView::default()
        );

        let filler = packed(0, PET_ACT_DISABLED);
        assert!(!filler.is_empty(), "the WORD is not zero");
        assert_eq!(
            slot_view(filler, &bar, None, None, false),
            PetActionView::default(),
            "…but spell id 0 resolves to nothing, so the button is still empty"
        );
    }

    /// A spell the catalog cannot name draws nothing rather than a nameless icon — and with it
    /// goes the autocast pair, which the client also gates on the record resolving.
    #[test]
    fn an_unresolvable_spell_draws_nothing() {
        let bar = state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE);
        let v = slot_view(packed(999, PET_ACT_ENABLED), &bar, None, None, false);
        assert_eq!(v, PetActionView::default());
        assert!(!v.autocast_allowed && !v.autocast_enabled);
    }

    /// A type outside 1–7 is inert. The client's own default arm under-pushes here (wow-re §2.5);
    /// we answer the empty slot instead, and in particular never reach the spell catalog with an
    /// arbitrary number.
    #[test]
    fn an_unknown_type_byte_is_inert() {
        let bar = state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE);
        let claw = spell("Claw", None);
        assert_eq!(
            slot_view(packed(3010, 0x33), &bar, Some(&claw), None, false),
            PetActionView::default()
        );
    }

    /// The slot index map is 1-based and bounded — a stale index from the VM cannot read past the
    /// ten words or wrap into slot 10.
    #[test]
    fn slot_lookup_is_one_based_and_bounded() {
        let mut bar = PetBar {
            spells: state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE),
            ..Default::default()
        };
        bar.spells.bar[0] = packed(PET_COMMAND_ATTACK, PET_ACT_COMMAND);
        bar.spells.bar[9] = packed(PET_REACT_PASSIVE, PET_ACT_REACTION);

        assert_eq!(slot_entry(&bar, 1).unwrap().action(), PET_COMMAND_ATTACK);
        assert_eq!(slot_entry(&bar, 10).unwrap().action(), PET_REACT_PASSIVE);
        assert!(slot_entry(&bar, 0).is_none());
        assert!(slot_entry(&bar, 11).is_none());
        assert!(slot_entry_mut(&mut bar, 0).is_none());
        assert!(slot_entry_mut(&mut bar, 11).is_none());
    }

    /// The local latch, which exists because the server confirms none of it (module doc), written
    /// with the client's own masks: a command press moves bits 8–15 and keeps byte 0 **and bit
    /// 27**, a reaction press moves byte 0 only, a spell press moves neither — and DISMISS moves
    /// nothing, because it ends the pet rather than becoming its standing order.
    #[test]
    fn a_press_latches_the_state_the_server_never_echoes() {
        let mut bar = PetBar {
            spells: state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE),
            ..Default::default()
        };
        bar.spells.state |= PET_STATE_BAR_DISABLED;

        latch_press(&mut bar, packed(PET_COMMAND_STAY, PET_ACT_COMMAND));
        assert_eq!(bar.spells.command_state() & 0xFF, PET_COMMAND_STAY);
        assert_eq!(bar.spells.react_state(), PET_REACT_DEFENSIVE, "untouched");
        assert!(
            bar.spells.bar_disabled(),
            "the disabled bit is the SERVER's — a command press must preserve it"
        );

        latch_press(&mut bar, packed(PET_REACT_AGGRESSIVE, PET_ACT_REACTION));
        assert_eq!(bar.spells.react_state(), PET_REACT_AGGRESSIVE);
        assert_eq!(bar.spells.command_state() & 0xFF, PET_COMMAND_STAY);
        assert!(bar.spells.bar_disabled());

        latch_press(&mut bar, packed(3010, PET_ACT_DISABLED));
        assert_eq!(bar.spells.command_state() & 0xFF, PET_COMMAND_STAY);
        assert_eq!(bar.spells.react_state(), PET_REACT_AGGRESSIVE);

        latch_press(&mut bar, packed(PET_COMMAND_DISMISS, PET_ACT_COMMAND));
        assert_eq!(
            bar.spells.command_state() & 0xFF,
            PET_COMMAND_STAY,
            "Dismiss ends the pet; it never becomes its standing command"
        );
    }

    /// ATTACK is the one command press that also raises the attack latch — the flag that lights
    /// the button and turns its next press into a call-off.
    #[test]
    fn an_attack_press_raises_the_latch_and_dismiss_does_not() {
        let mut bar = PetBar {
            spells: state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE),
            ..Default::default()
        };
        assert!(!bar.attacking);

        latch_press(&mut bar, packed(PET_COMMAND_ATTACK, PET_ACT_COMMAND));
        assert!(bar.attacking);

        bar.attacking = false;
        latch_press(&mut bar, packed(PET_COMMAND_DISMISS, PET_ACT_COMMAND));
        latch_press(&mut bar, packed(PET_REACT_PASSIVE, PET_ACT_REACTION));
        assert!(!bar.attacking, "only ATTACK raises it");
    }

    /// The regression the director caught by eye: the Attack button behaving as a sticky toggle.
    ///
    /// ATTACK is an ORDER, not a mode. Latching it into the command byte — which the binary's
    /// `action <= 1` gate forbids — lit it permanently, put Follow and Stay out, and left it lit
    /// through the very `PetStopAttack` whose whole job is to put it out. The press-then-call-off
    /// round trip is driven here end to end, because each half alone looks fine.
    #[test]
    fn an_attack_order_never_becomes_the_standing_command() {
        let mut bar = PetBar {
            spells: state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE),
            ..Default::default()
        };
        let attack = packed(PET_COMMAND_ATTACK, PET_ACT_COMMAND);
        let follow = packed(PET_COMMAND_FOLLOW, PET_ACT_COMMAND);

        latch_press(&mut bar, attack);
        assert_eq!(
            bar.spells.command_state(),
            PET_COMMAND_FOLLOW,
            "an attack order leaves the standing command alone"
        );
        assert!(
            slot_view(follow, &bar.spells, None, None, bar.attacking).active,
            "so Follow keeps its light while the pet is on something"
        );
        assert!(slot_view(attack, &bar.spells, None, None, bar.attacking).attack_active);

        // The call-off: `PetStopAttack` clears the latch, and the latch was the ONLY thing lighting
        // the button — so it goes out, which is what a stuck command byte used to prevent.
        bar.attacking = false;
        let called_off = slot_view(attack, &bar.spells, None, None, bar.attacking);
        assert!(
            !called_off.active,
            "the Attack button goes out with the latch"
        );
        assert!(
            !called_off.attack_active,
            "and its next press orders, not calls off"
        );

        // Stay is still reachable as a mode — the command byte was never hijacked.
        latch_press(&mut bar, packed(PET_COMMAND_STAY, PET_ACT_COMMAND));
        assert_eq!(bar.spells.command_state(), PET_COMMAND_STAY);
    }
}
