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
    /// **"The possessed unit is attacking"** — the client's own `[0xb714b0]`, a purely local latch
    /// with no field behind it anywhere (wow-re §1), and **the possess bar's, not the pet bar's**.
    ///
    /// It is `IsPetAttackActive` entire, and `GetPetActionInfo`'s COMMAND branch ORs it into
    /// `isActive` for action 2 (`0x4bdf16`–`0x4bdf22`), so it is also the only thing that can ever
    /// light an Attack button. What gates the one write that raises it is the whole story
    /// ([`possessing`]): `0x4bd420` demands `0x5ee5a0(player) == the bar's unit`, and `0x5ee5a0`
    /// answers the **possessed** unit or nothing. A hunter's pet is not possessed, so on an
    /// ordinary pet bar this latch **can never rise** — which is exactly the director's report:
    /// Attack is a button you press, never a mode that stays lit.
    ///
    /// The three ways down are transcribed anyway, because under possession they are live: the lit
    /// button's own second press (Lua `PetStopAttack` → `0x4bd650`), a new pet (`0x4bc8ce`), and
    /// the **old-target clear `0x493910` at `0x493a18`** ([`pet_stop_on_old_target_clear`]), which
    /// every selection writer runs.
    ///
    /// benilla read this off the pet's streamed `UNIT_FIELD_TARGET` until the RE landed. That is a
    /// different question with a different answer: a defensive pet that retaliates on its own has
    /// a target the player never ordered, and the reference does not light the Attack button for
    /// it either.
    ///
    /// **It is not the source of `PET_ATTACK_START`/`PET_ATTACK_STOP`**, however much it reads like
    /// one — decision 0990 made exactly that inference and it was wrong. Those ride the pet's
    /// server-owned `UNIT_FIELD_FLAGS` bit `0x800` instead ([`feed_pet_unit`]). This latch answers
    /// "did the player order the unit they are *driving* to attack"; the flag answers "is the pet
    /// fighting", and it is the flag that moves for a hunter.
    pub(crate) attacking: bool,
    /// **`SignalEvent(0x161 PET_BAR_UPDATE)`, counted** — the half of a press that is not the state
    /// write and matters just as much.
    ///
    /// Both state writers end on an **unconditional** signal, with no compare of the old value
    /// against the new one:
    ///
    /// ```text
    /// 0x4bc940  [0xb71468] = state & 0xFFFFFF00 | action ; ecx = 0x161 ; jmp SignalEvent  (reaction)
    /// 0x4bc960  [0xb71468] = state & 0x080000FF | a << 8 ; ecx = 0x161 ; jmp SignalEvent  (command)
    /// ```
    ///
    /// That is not redundancy, it is load-bearing, and missing it is visible on the first click:
    /// the CheckButton widget flips itself before `OnClick` runs and the reference's first line is
    /// `this:SetChecked(0)`, so **every** press starts by taking the light off. What puts it back
    /// is `PetActionBar_Update` running on this signal and re-deriving `isActive`. Press the mode
    /// the pet is *already* in and nothing about the state changes — but the signal still fires and
    /// the light still comes back.
    ///
    /// benilla's feed dedups on what it last pushed ([`PetBarMemory`]), which is right for a
    /// per-frame feed and wrong here: a no-change press pushed nothing, fired nothing, and left the
    /// button visually un-toggled with no way back until something else moved. So the counter is
    /// folded into the dedup key — a bump is a forced repaint, which is exactly what the signal
    /// buys the reference. Wrapping, because only its *changes* are ever read.
    pub(crate) bar_signals: u32,
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
///
/// The leading `u32` is [`PetBar::bar_signals`], and it is what keeps the dedup from swallowing a
/// press that changes nothing — see that field for why the reference cannot afford to skip one.
#[derive(Default)]
struct PetBarMemory {
    pushed: Option<(u32, bool, bool, bool, Vec<PetActionView>)>,
}

pub(crate) struct UiPetPlugin;

impl Plugin for UiPetPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PetBar>().add_systems(
            Update,
            (
                // Both feeds ride with the unit feed (before the VM ticks, like the stance bar's);
                // the drain runs after the input pass so a click goes out the same frame.
                // The old-target clear runs BEFORE the feed so the Attack button goes out in the
                // same frame the selection moved, not one behind it.
                pet_stop_on_old_target_clear
                    .in_set(UnitFeed)
                    .before(feed_pet_bar),
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
/// `cooldown` and `showing_active` are passed in rather than looked up here so the whole function
/// stays a pure (state, word) → view mapping, which is what the tests below exercise.
/// `showing_active` is [`active_aura_press`]'s predicate — the *same* answer that decides whether
/// a click cancels, because in the reference it is literally the same call (`0x4bcea0`, reached
/// from `GetPetActionInfo` at `0x4bdd2f` and from `CastPetAction` at `0x4bd24a`). Computing it
/// once and handing it to both is what keeps the icon honest: the button that shows the active art
/// is exactly the button whose next press takes the aura off.
fn slot_view(
    entry: PetActionEntry,
    bar: &PetSpells,
    spell: Option<&benilla_formats::SpellDisplay>,
    cooldown: Option<(i64, u32, bool)>,
    pet_attacking: bool,
    showing_active: bool,
) -> PetActionView {
    PetActionView {
        // The raw word rides EVERY slot, including the ones that draw as empty — decision 1010's
        // drag is word arithmetic and reads it. Zeroing an "empty" slot here would be wrong on the
        // wire *and* wrong in the drop core: vmangos fills its unused slots with `ACT_DISABLED` +
        // spell id 0, and that shape (type 1, low 16 zero) is precisely the relocation candidate
        // the core hunts for.
        packed: entry.packed,
        // Only a resolved spell can be passive; a token has no record and an unresolvable id has
        // no answer, and `false` is the honest reading of both.
        passive: spell.is_some_and(|s| s.passive),
        ..slot_paint(entry, bar, spell, cooldown, pet_attacking, showing_active)
    }
}

/// [`slot_view`]'s painted half — everything the button draws, with no wire word in it. Split so
/// the drag's two raw fields are stamped in exactly one place rather than on each of four returns.
fn slot_paint(
    entry: PetActionEntry,
    bar: &PetSpells,
    spell: Option<&benilla_formats::SpellDisplay>,
    cooldown: Option<(i64, u32, bool)>,
    pet_attacking: bool,
    showing_active: bool,
) -> PetActionView {
    let kind = entry.kind();
    let action = entry.action();

    if let Some((name, texture)) = (kind == benilla_protocol::messages::PET_ACT_COMMAND)
        .then(|| command_token(action))
        .flatten()
    {
        // A command token lights on `(state >> 8) == action` **or** on the attack latch — read at
        // the bytes at `0x4bdf01`-`0x4bdf22`, and both halves matter:
        //
        // - the compare is against the UNMASKED `state >> 8` (`PetSpells::command_state`'s own
        //   note), so a disabled bar puts every command button out;
        // - ATTACK gets the extra clause — and it is the *only* thing that can light ATTACK,
        //   because the command byte is never written for it (§10.1).
        //
        // So whether Attack ever appears lit is entirely a question about [`PetBar::attacking`],
        // and the answer for a pet bar is **never**: that latch is the possess bar's, gated on
        // `0x5ee5a0` ([`possessing`]). This expression stays faithful rather than hard-coding the
        // `false`, because under Mind Control or Eyes of the Beast the same button does light.
        //
        // `attack_active` — `IsPetAttackActive`, the click fork — is the same latch narrowed to
        // this slot (`0x4be138`-`0x4be153`: type 7, action 2, and the flag). Same input, so the
        // button that lights is exactly the button whose next press calls the unit off.
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
        // THE ICON SWAP (decision 1007, wow-re §2.1 `0x4bdd2f`/`0x4bdd38`/`0x4bdd77`): a spell the
        // pet is currently running draws its record's `ActiveIconID` instead of its `SpellIconID`.
        // Falling back to `icon` here would be wrong — the reference looks up whichever id the
        // predicate chose and pushes **nil** if that lookup fails (`0x4bdd50`), so an unresolvable
        // active icon hides the button rather than showing the inactive art on an active spell.
        // `active_icon` is `None` on exactly that failure, so `.clone()` already says it.
        texture: if showing_active {
            spell.active_icon.clone()
        } else {
            spell.icon.clone()
        },
        is_token: false,
        spell_id: Some(action),
        // A spell slot NEVER reports isActive — VERIFIED nil on every path (wow-re §2.1, pushed
        // at `0x4bdd5e`), which retires 0982's INTERIM. `isActive` is exclusively a token
        // concept and `autoCast*` exclusively a spell one; the two halves of the signature never
        // overlap (§2.5 quirk 3).
        //
        // "The pet is running this spell" is expressed by the icon above, not by this flag — which
        // is why 0988's hole closes without this line changing.
        active: false,
        // Autocast is bits 31/30 of the word, not the type byte (wow-re §2.1) — and both are
        // additionally gated on the spell resolving in `Spell.dbc`, which the early return above
        // has already enforced by the time we get here.
        autocast_allowed: entry.autocast_allowed(),
        autocast_enabled: entry.autocast_on(),
        attack_active: false,
        cooldown,
        // `packed`/`passive` are [`slot_view`]'s to stamp — this half paints, it does not encode.
        ..Default::default()
    }
}

/// The pet spell slot that is **showing active** — the reference's `0x4bcea0` (wow-re
/// `ui/scratch/pet-action-bar-api.md` §2.1), returning the spell id when it holds so the one
/// answer can drive both of its consumers. Decision 1007.
///
/// It is not a new predicate: `0x4bcea0` is the *pet-side compiled twin* of the player's
/// `0x4e55f0`, which we already carry as [`crate::ui_action::toggle::active_action_toggle`] — same
/// three tests (nonzero raw `ActiveIconID`, the spell's own id in a live `UNIT_FIELD_AURA` slot,
/// that slot's `AURAFLAGS` nibble bit 0), different unit. So this reaches for it rather than
/// restating it, and the pet's store goes in where the player's does.
///
/// **The `ActiveIconID != 0` gate is load-bearing on the send, not just the icon.** Because the
/// binary tests it first (`0x4bcefd`) and `CastPetAction` takes its cancel arm on the whole
/// predicate, a pet spell whose record carries no active icon can never be clicked off — it
/// re-casts instead. That reads like an oversight and isn't ours to fix: the same `0` is what
/// tells the bar there is no "active" art to show, so the two halves are consistent.
fn active_aura_press(
    entry: PetActionEntry,
    pet: Option<&ObjectStore>,
    spell: Option<&benilla_formats::SpellDisplay>,
) -> Option<u32> {
    if !entry.is_spell() || entry.is_empty() {
        return None;
    }
    let spell_id = entry.action();
    crate::ui_action::toggle::active_action_toggle(spell_id, spell?, pet?).then_some(spell_id)
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
    // The pet's own descriptor. `None` = we hold a bar for a unit whose descriptor has not arrived
    // (or has left); the usability predicate then rests on bit 27 alone rather than greying a bar
    // on missing data, and no slot can read as showing-active.
    let pet_store = index
        .0
        .get(&bar.spells.pet_guid)
        .and_then(|&e| stores.get(e).ok());
    let pet_flags = pet_store.map(|s| s.0.unit_flags());
    let usable = actions_usable(&bar, pet_flags);
    // `PickupPetAction`'s own gate, and nobody else's (`0x4be1c1`): a POSSESSED unit's bar cannot
    // be rearranged. Deliberately not folded into `usable` — the reference keeps possession out of
    // the flags that grey the bar, because a possessed unit is exactly when the buttons must work.
    // Absent flags read as not-possessed, matching `usable`'s own missing-data posture.
    let pickup_allowed = pet_flags.unwrap_or(0) & UNIT_FLAG_POSSESSED == 0;
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
                slot_view(
                    entry,
                    &bar.spells,
                    display,
                    cooldown,
                    pet_attacking,
                    active_aura_press(entry, pet_store, display).is_some(),
                )
            })
            .collect()
    } else {
        Vec::new()
    };

    // `bar.bar_signals` rides the key so a press the state does not move still repaints — the
    // `0x4bc940`/`0x4bc960` signal, which the button's own `SetChecked(0)` makes mandatory.
    let key = (bar.bar_signals, has_bar, usable, pickup_allowed, fresh);
    if memory.pushed.as_ref() != Some(&key) {
        debug!(
            "ui_pet: bar {} ({} occupied slot(s), {}{})",
            if key.1 { "shown" } else { "hidden" },
            key.4.iter().filter(|s| s.name.is_some()).count(),
            if key.2 { "usable" } else { "disabled" },
            if key.3 { "" } else { ", possessed" },
        );
        memory.pushed = Some(key.clone());
        script.set_pet_actions(key.1, key.2, key.3, key.4);
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
    /// The pet's last `UNIT_FIELD_FLAGS & 0x800` — the `PET_ATTACK_START`/`PET_ATTACK_STOP` pair's
    /// trigger. `None` until the first resolved pet, so a login with a pet already fighting
    /// announces it once instead of reading as a transition from "calm".
    in_combat: Option<bool>,
}

/// `UNIT_FIELD_FLAGS` bit `0x800` — the **pet-in-combat** flag, and the whole trigger for
/// `PET_ATTACK_START`/`PET_ATTACK_STOP` (`0x5ff75e test ah,8`, wow-re
/// `object-layer/scratch/pet-command-validators.md` §4).
///
/// Server-owned and server-written: nothing client-side sets it, which is exactly why it — and not
/// the local click latch — is what the reference watches.
const UNIT_FLAG_PET_IN_COMBAT: u32 = 0x0000_0800;

/// `UNIT_FIELD_FLAGS` bit 24 — `UNIT_FLAG_POSSESSED` (vmangos `UnitDefines.h:515`), read by the
/// reference as the descriptor byte `[[pet+0x110]+0xA3] & 1`. Gates the pet bar's **drag** and
/// nothing else (decision 1010): a possessed unit's buttons still work, its layout is just not
/// yours to rearrange.
const UNIT_FLAG_POSSESSED: u32 = 0x0100_0000;

/// **The unit behind [`PetBar`]'s cached guid, plus our own identity** — the one lookup both pet
/// systems need and neither owns.
///
/// It exists because the client's two newest pet answers are both *about the pet's own
/// descriptor* rather than about the bar: the `PET_ATTACK_*` edge reads the pet's
/// `UNIT_FIELD_FLAGS`, and the ATTACK order's validator reads the pet's whole eligibility. Both
/// also need the active player's guid, because both test **ownership** before they trust what they
/// read (`0x5ff780` and `0x612e33`).
#[derive(bevy::ecs::system::SystemParam)]
pub(crate) struct PetUnit<'w, 's> {
    index: Res<'w, GuidIndex>,
    stores: Query<'w, 's, &'static ObjectStore>,
    self_guid: Res<'w, crate::net::SelfGuid>,
}

impl PetUnit<'_, '_> {
    /// The pet's streamed descriptor, or `None` while the bar names a guid whose object has not
    /// arrived (or has left) — the case the reference treats as "no pet", never as "a bad pet".
    pub(crate) fn store(&self, pet_guid: u64) -> Option<&ObjectStore> {
        let e = *self.index.0.get(&pet_guid)?;
        self.stores.get(e).ok()
    }
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
    pet: PetUnit,
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
        .then(|| pet.store(pet_guid))
        .flatten()
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

    // PET_ATTACK_START (334) / PET_ATTACK_STOP (335) — the pet frame's flashing
    // `UI-Player-AttackStatus` overlay.
    //
    // **CORRECTED.** Decision 0990 derived these from the attack latch `[0xb714b0]`'s edges and
    // said so honestly; the derivation was wrong. wow-re later carved the real fire site —
    // `0x5ff793`/`0x5ff79a` inside `0x5ff580`, a per-field change callback registered *by field
    // byte offset* (`0x6042e2 mov edx,0xa0`), which is why walking the call graph out of the pet
    // TU never reached it. The trigger is the unit's own server-supplied
    // `UNIT_FIELD_FLAGS & 0x800` **transition**, gated on the unit's owner guid being ours.
    //
    // The two are not the same question and they visibly diverge: the latch is a local click
    // record with exactly three writers, so a pet that disengages on its own — its target dies, it
    // runs out of range, it is feared off — clears the flag with no client-side call at all, and
    // the latch-driven version would hold the frame's combat glow lit until the player pressed
    // something. Conversely a defensive pet that retaliates unbidden raises the flag without any
    // press, and the latch never moves.
    //
    // (`PetActionBarFrame`'s Attack *button* is the other mechanism and keeps the latch — it is
    // driven by `PET_BAR_UPDATE` + `IsPetAttackActive`. Two frames, two sources, genuinely
    // independent.)
    let in_combat = fresh
        .as_ref()
        .and_then(|_| pet_combat_flag(pet.store(pet_guid)?, pet.self_guid.0));
    if let Some(now) = in_combat {
        if memory.in_combat != Some(now) {
            memory.in_combat = Some(now);
            debug!("ui_pet: pet in-combat flag → {now}");
            script.fire_event(
                if now {
                    "PET_ATTACK_START"
                } else {
                    "PET_ATTACK_STOP"
                },
                vec![],
            );
        }
    } else {
        // No pet, or not ours: forget the edge rather than firing a STOP the reference never
        // sends. `0x5ff580` is a *change* callback — a unit going away does not call it.
        memory.in_combat = None;
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
#[allow(clippy::too_many_arguments)] // one Bevy system's full input set
fn drain_pet_actions(
    script: Option<NonSendMut<UiScript>>,
    mut bar: ResMut<PetBar>,
    mut selection: ResMut<Selection>,
    commands: Res<NetCommands>,
    pet: PetUnit,
    spells: Option<Res<Spells>>,
    mut ui_errors: ResMut<crate::ui_action::UiErrorKeys>,
    scan: crate::target::EnemyScan,
    mut seam: crate::creature_anim::AttackSeam,
) {
    let Some(mut script) = script else {
        return;
    };
    let pressed = script.take_pet_actions();
    let toggles = script.take_pet_autocast_toggles();
    let stops = script.take_pet_stop_attacks();
    let writes = script.take_pet_set_actions();
    if pressed.is_empty() && toggles.is_empty() && stops == 0 && writes.is_empty() {
        return;
    }
    let pet_guid = bar.spells.pet_guid;
    if pet_guid == 0 {
        debug!("ui_pet: dropping queued pet intents — the bar is gone");
        return;
    }
    // NOTE, so it is not "fixed" again (decision 1030 reverting 1027): the repaint is
    // [`latch_press`]'s alone, and it is deliberately NOT unconditional on the click.
    // `PetActionButton_OnClick`'s `this:SetChecked(0)` runs for every click, but the reference only
    // signals `PET_BAR_UPDATE` from the writes that changed something — so a **refused**
    // `TogglePetAutocast` (`0x4bcbf7`: a token is not autocastable, which is every right-click on
    // Follow, Stay or a reaction) leaves that button's ring off until the next repaint from
    // anywhere. That is the reference's own behaviour, checked there by the director, and
    // reproduced.
    //
    // The pet's own descriptor — the ATTACK arm's validator reads it, and so does the spell arm's
    // showing-active test. Neither is about the bar; both are about the pet.
    let pet_store = pet.store(pet_guid);
    // `0x4bd420`'s compare, hoisted once: is the bar's unit the one we are *driving*? Only then may
    // an ATTACK press latch (see [`possessing`]) — which for an ordinary pet is never.
    let possessing = possessing(pet_store, pet.self_guid.0);

    for slot in pressed {
        let Some(entry) = slot_entry(&bar, slot) else {
            continue;
        };
        // The spell arm's early exit (wow-re §10.1, `0x4bd240`–`0x4bd2ad`): a press on a spell the
        // pet is already running takes the aura OFF and **returns** — `CMSG_PET_ACTION` never
        // leaves, so it is a cancel, not a re-cast. Nothing is latched locally either: the icon
        // goes back when the pet's `UNIT_FIELD_AURA` says the aura is gone, which is the honest
        // order (the server can refuse — a dead pet gets `FEEDBACK_PET_DEAD` instead).
        let display = entry
            .is_spell()
            .then(|| spells.as_ref().and_then(|s| s.catalog.get(entry.action())))
            .flatten();
        if let Some(spell_id) = active_aura_press(entry, pet_store, display) {
            debug!("ui_pet: slot {slot} cancels its own aura (spell {spell_id}) — no PetAction");
            let _ = commands
                .0
                .send(ClientCommand::PetCancelAura { pet_guid, spell_id });
            continue;
        }
        // `0x612df0`, the ATTACK order's validator, and its actor is the **pet**. Phase A is the
        // pet's own eligibility; Phase B is the target, and it can move your selection — see
        // [`crate::target::attack_order_target`], which is the whole of "press Attack at nothing
        // and the pet goes for whatever is in front of you".
        //
        // Every other arm sends the selection untouched: `CastPetAction` substitutes it for a nil
        // Lua argument at `0x4bd212` and only the ATTACK arm ever calls the validator, so a Follow
        // or a spell carries whatever you happened to have.
        let mut target_guid = selection.guid.unwrap_or(0);
        let refused = if is_attack_order(entry) {
            crate::ui_action::attack_actor_refusal(pet_store, pet.self_guid.0, &mut ui_errors)
                || match crate::target::attack_order_target(
                    &scan,
                    &mut selection,
                    &mut seam,
                    &mut ui_errors,
                ) {
                    Some(guid) => {
                        target_guid = guid;
                        false
                    }
                    None => true,
                }
        } else {
            false
        };
        if !commit_press(&mut bar, entry, refused, possessing) {
            debug!("ui_pet: slot {slot} refused by the attack validator — no packet");
            continue;
        }
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
        if stop_pet_attack(&mut bar, &commands) {
            debug!("ui_pet: stop attack");
        }
    }
    // The drag's writes (decision 1010). The engine ran the assign core against its own mirror and
    // handed back the `(0-based position, word)` pairs; the authoritative ten words live *here*, so
    // the app's whole job is to mirror each pair and put the batch on the wire **whole** — the
    // server tells the one-pair form from the two-pair form by body size, so a relocation and its
    // write must not be split into two sends.
    for entries in writes {
        for &(position, packed) in &entries {
            if let Some(e) = bar.spells.bar.get_mut(position as usize) {
                *e = PetActionEntry::from(packed);
            }
        }
        debug!("ui_pet: bar write {entries:?}");
        let _ = commands
            .0
            .send(ClientCommand::PetSetAction { pet_guid, entries });
    }
}

/// `PetStopAttack`'s **core**, `0x4bd650` — call the pet off, and the only thing besides a new pet
/// that puts the Attack button out.
///
/// It **no-ops entirely when the latch is down** (`0x4bd65e`): no packet, no repaint. That gate
/// lives here rather than in the VM because the latch does. Returns whether it actually fired,
/// which is what its two callers log.
///
/// Split out of the drain because the drain is *not* its only caller: [`pet_stop_on_old_target_clear`]
/// is the second, exactly as `0x4bd650` has a second call site of its own.
fn stop_pet_attack(bar: &mut PetBar, commands: &NetCommands) -> bool {
    let pet_guid = bar.spells.pet_guid;
    if !bar.attacking || pet_guid == 0 {
        return false;
    }
    let _ = commands.0.send(ClientCommand::PetStopAttack { pet_guid });
    bar.attacking = false;
    true
}

/// `0x493910`'s entry gate, as a predicate on the selection transition.
///
/// The old-target clear returns immediately unless there **is** a current selection
/// (`0x493937 or edx,ecx; je epilogue`) and the guid it was handed is either zero or that same
/// selection (`0x493949`/`0x493951`) — and its two callers only ever hand it one of those two.
/// So it runs on exactly the transitions below: a selection that existed is being replaced or
/// dropped. `None → Some` is *not* one of them.
fn old_target_cleared(previous: Option<u64>, now: Option<u64>) -> bool {
    previous.is_some() && previous != now
}

/// **Clearing the old target calls `PetStopAttack`'s core** — the third and last way the attack
/// latch goes down, and live only on a **possess** bar (see [`possessing`]; a hunter's pet never
/// raises the latch in the first place, so [`stop_pet_attack`]'s own gate makes this a no-op for
/// it, exactly as `0x4bd65e` does in the reference).
///
/// `0x493910` — the old-target clear, run by *both* selection writers (`0x493540`'s switch calls
/// it with the outgoing guid at `ecx = 0`, the explicit clear with `{0,0}` at `ecx = 1`) — does
/// three things past its entry gate, of which benilla previously carried only the first:
///
/// ```text
/// 0x493a08  0x5ecac0(player)      ; StopAttack   -> CMSG_ATTACKSTOP   (target/scan.rs `commit`)
/// 0x493a0f  0x5ee5a0(player)      ; the unit I am POSSESSING, or null
/// 0x493a18  0x4bd650()            ; PetStopAttack's core, iff there is one   <- THIS
/// 0x493a1d  if (notifyServer) ... ; CMSG_SET_SELECTION
/// ```
///
/// The `0x5ee5a0` at `0x493a0f` is the same possession resolver `0x4bd420` uses to gate the latch,
/// which is the tidy proof that the whole latch arm belongs to one bar: the thing that raises it
/// and the thing that clears it ask the identical question. The call also sits **above** the
/// notify-server branch, so it runs on the silent switch-clear too, not only on the explicit one.
///
/// Modelled as a transition on [`Selection`] rather than a hook in the selection writers because
/// that is what the reference gets for free by routing every writer through one clear: a `Local`
/// mirror sees `/target`, a click, TAB, ESC, the death-teardown clear and the acquire alike. The
/// dedup is the reference's own (`0x493540` bails when the guid is already current, so no clear
/// runs) and falls out of comparing against the mirror.
fn pet_stop_on_old_target_clear(
    selection: Res<Selection>,
    mut previous: Local<Option<u64>>,
    mut bar: ResMut<PetBar>,
    commands: Res<NetCommands>,
) {
    let now = selection.guid;
    if *previous == now {
        return;
    }
    let cleared = old_target_cleared(*previous, now);
    *previous = now;
    if cleared && stop_pet_attack(&mut bar, &commands) {
        debug!("ui_pet: the old-target clear called the pet off (0x493a18)");
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
/// command byte. The distinction is the difference between a mode and an order: Stay and Follow
/// are what the pet is *doing until told otherwise*, and they stay lit because `isActive`'s state
/// compare keeps matching the command byte. Attack is a thing you tell it once.
///
/// Getting this wrong is visible within one click: latching ATTACK into the command byte leaves
/// the Attack button lit forever and blanks Follow and Stay.
///
/// **The ATTACK arm's latch is gated on possession** — [`possessing`], the `0x5ee5a0` compare at
/// `0x4bd420`, which no ordinary pet passes. So on a hunter's bar this match arm never fires and
/// the Attack button never checks, which is what the reference does and what the director sees.
/// The arm is still written out rather than deleted because the possess bar is the same bar: Mind
/// Control and Eyes of the Beast reach it, and everything downstream (`IsPetAttackActive`,
/// `PetStopAttack`, the old-target clear) is live for them.
///
/// The other gate on this press, `0x612df0`, ran back in [`drain_pet_actions`] and is *not*
/// symmetric with it: a validator veto costs the whole press (`0x4bd414` jumps to the epilogue,
/// no packet), while failing the possession compare costs only the latch (`0x4bd427` jumps to the
/// send). [`commit_press`] holds that asymmetry.
/// What the `PET_ATTACK_*` field-change callback would see for this unit: `Some(fighting)` when
/// the unit is **ours**, `None` when it is nobody's business of ours and no event may fire.
///
/// Both halves are `0x5ff580`'s, in its order: the owner test at `0x5ff780` first — CHARMEDBY when
/// set, else SUMMONEDBY, which is *not* the fallback `0x5ee5a0` uses for the same-shaped read — and
/// only then the flag at `0x5ff78d`. Reading the flag without the owner test would let any unit in
/// the world flash our pet frame.
fn pet_combat_flag(store: &ObjectStore, self_guid: Option<u64>) -> Option<bool> {
    (store
        .0
        .unit_owner(benilla_protocol::OwnerFallback::SummonedBy)
        == self_guid)
        .then(|| store.0.unit_flags() & UNIT_FLAG_PET_IN_COMBAT != 0)
}

/// `0x5ee5a0` — **"the unit I am directly possessing"**, and the gate that decides whether an
/// ATTACK press latches anything at all.
///
/// This is the fact the pet bar was missing, and it is the answer to "why can the reference's
/// Attack button never stay lit". `0x4bd420` calls this on the *player* and compares the result
/// with the bar's own unit; only on a match does `0x4bd42e` raise `[0xb714b0]`. And the function
/// is not "resolve my pet" despite the shape — read at the bytes it is four tests in a row:
///
/// ```text
/// 0x5ee5bc  [player + 0x1c70] & 1        ; "I am driving something that is not me" — else null
/// 0x5ee5d5  the passed unit IS the local player                                    ; else guid 0
/// 0x5ee5e9  [[player + 0xe68] + 0x830]   ; the guid I am driving -> object lookup (type 8)
/// 0x5ee626  [fields + 0xa3] & 1          ; UNIT_FLAG_POSSESSED on that unit — else null
/// 0x5ee62f  charmedBy (else createdBy) == me                                       ; else null
/// ```
///
/// `[player + 0x1c70]`'s bit 0 is set and cleared by the control-transfer handler `0x5ee2xx`
/// against the active-mover guid, firing event `0x27a` with a bool — the client's own
/// "control lost / control gained". So the whole thing means *possession*: Mind Control, Eye of
/// Kilrogg, Eyes of the Beast. vmangos agrees from the other side — `UNIT_FLAG_POSSESSED` is
/// written in exactly three places (`Player::SummonPossessedMinion`, `SPELL_AURA_MOD_POSSESS`,
/// `Player::ModPossessPet`), each of them paired with `SetMover` + `UpdateControl`, and **never**
/// for an ordinary hunter or warlock pet.
///
/// Which is why `PetActionButton_OnClick`'s `IsPetAttackActive` fork and `PetStopAttack` are dead
/// code for a hunter, the way `PetActionButton_StartFlash` is dead code for everyone: the shipped
/// `PetActionBarFrame.lua` is written for both bars and a hunter only ever takes one branch.
///
/// **What benilla checks, and the gap:** the flag and the ownership, which are the two halves that
/// live in the descriptor we already stream. The active-mover halves (`+0x1c70`, the driven guid)
/// need client-control state benilla does not model yet; they only ever *narrow* this further, and
/// the server sets the flag and the mover together, so the flag standing in for both is faithful
/// until possession itself is built.
fn possessing(store: Option<&ObjectStore>, self_guid: Option<u64>) -> bool {
    store.is_some_and(|s| {
        s.0.unit_flags() & UNIT_FLAG_POSSESSED != 0
            && s.0.unit_owner(benilla_protocol::OwnerFallback::CreatedBy) == self_guid
    })
}

/// Is this press the ATTACK **order** — the one slot word that runs a validator before it sends?
///
/// Type 7 action 2 and nothing else. The reference's type-7 arm branches exactly twice
/// (`cmp ecx,1; jle` then `cmp ecx,2; jne`), so DISMISS, every `action >= 4`, and both mode
/// commands leave down paths that send unconditionally.
fn is_attack_order(entry: PetActionEntry) -> bool {
    entry.kind() == benilla_protocol::messages::PET_ACT_COMMAND
        && entry.action() == PET_COMMAND_ATTACK
}

/// Commit one press against the bar and answer **whether it goes on the wire** — the composition
/// of the gate and the latch, in one place because that composition is where the last bug lived.
///
/// `refused` is the shared attack-start validator's verdict on the **pet** as actor (`0x4bd40d`
/// passes `ecx = edi`, the pet object resolved four instructions earlier). A veto costs
/// everything: `0x4bd414 je 0x4bd4c6` jumps to the **function epilogue, not the send**, so there
/// is no packet, no `[0xb714b0] = 1`, and no `PET_BAR_UPDATE`. The click is as if it never
/// happened, except for the red line the validator itself raised.
///
/// This closes the residual decision 0998 named in its own text — *"a refused attack currently
/// lights the button where the real client would not"* — and it is deliberately the one function
/// that both decides and records, because 0998's bug was invisible to thirteen tests that checked
/// the read side and the write side separately and never their join.
///
/// `possessing` is [`possessing`]'s answer, threaded rather than re-derived because it is the
/// *second* gate on the same press and it belongs beside the first: `0x4bd420` runs immediately
/// after `0x4bd40d`'s validator, and unlike it, failing costs only the latch — `0x4bd427 jne
/// 0x4bd444` lands on the **send**, so the order still goes out. That asymmetry is the whole shape
/// of the Attack button: the pet is always dispatched, the button never lights for it.
fn commit_press(bar: &mut PetBar, entry: PetActionEntry, refused: bool, possessing: bool) -> bool {
    if refused {
        return false;
    }
    latch_press(bar, entry, possessing);
    true
}

fn latch_press(bar: &mut PetBar, entry: PetActionEntry, possessing: bool) {
    let action = entry.action();
    match entry.kind() {
        benilla_protocol::messages::PET_ACT_COMMAND
            if action == PET_COMMAND_STAY || action == PET_COMMAND_FOLLOW =>
        {
            // `0x4bc960` — the write, then `ecx = 0x161; jmp SignalEvent`. Both halves, and the
            // signal is unconditional: writing the SAME command still repaints the bar, which is
            // the only thing that puts back the light `OnClick`'s `SetChecked(0)` just took off.
            bar.spells.state = (bar.spells.state & 0x0800_00FF) | (action << 8);
            bar.bar_signals = bar.bar_signals.wrapping_add(1);
        }
        benilla_protocol::messages::PET_ACT_COMMAND
            if action == PET_COMMAND_ATTACK && possessing =>
        {
            // `0x4bd42e` signals too (`mov ecx,0x161` immediately before the write), and it is the
            // one signal a pet bar never gets: the possession gate above it is what skips both.
            bar.attacking = true;
            bar.bar_signals = bar.bar_signals.wrapping_add(1);
        }
        benilla_protocol::messages::PET_ACT_REACTION => {
            // `0x4bc940`, the same shape one byte-mask over.
            bar.spells.state = (bar.spells.state & 0xFFFF_FF00) | action;
            bar.bar_signals = bar.bar_signals.wrapping_add(1);
        }
        // DISMISS, spells and every unknown type reach `0x4bd444` without signalling. A spell's
        // button can be checked (its active-aura ring), and it comes back the honest way: the
        // pet's aura field moves and the feed sees a real change.
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

    /// [`slot_view`] for a slot that is **not** showing active — the ordinary case, and the only
    /// one the token/spell/cooldown tests care about. The showing-active leg has its own tests,
    /// which call `slot_view` directly so the flag is visible at the call.
    fn view(
        entry: PetActionEntry,
        bar: &PetSpells,
        spell: Option<&benilla_formats::SpellDisplay>,
        cooldown: Option<(i64, u32, bool)>,
        pet_attacking: bool,
    ) -> PetActionView {
        slot_view(entry, bar, spell, cooldown, pet_attacking, false)
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

        let follow = view(
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

        let stay = view(
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

        let idle = view(attack, &following, None, None, false);
        assert!(!idle.active && !idle.attack_active);

        let on = view(attack, &following, None, None, true);
        assert!(on.active, "the latch lights it even on a FOLLOW command");
        assert!(on.attack_active, "and the next press calls the pet off");

        // The latch never reaches another command's button, however busy the pet is.
        let follow = view(
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
        let def = view(
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
            !view(
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

        let passive = view(
            packed(PET_REACT_PASSIVE, PET_ACT_REACTION),
            &bar,
            None,
            None,
            false,
        );
        assert!(passive.active, "a bar that cannot be ordered reads Passive");
        assert!(
            !view(
                packed(PET_REACT_DEFENSIVE, PET_ACT_REACTION),
                &bar,
                None,
                None,
                false
            )
            .active
        );

        assert!(
            !view(
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

        let on = view(
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

        let off = view(
            packed(3010, PET_ACT_DISABLED),
            &bar,
            Some(&claw),
            None,
            false,
        );
        assert!(off.autocast_allowed && !off.autocast_enabled);

        // A passive pet spell shows, but can never autocast — no ring, no sparkle.
        let passive = view(
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
            view(zero, &bar, None, None, false),
            PetActionView::default()
        );

        let filler = packed(0, PET_ACT_DISABLED);
        assert!(!filler.is_empty(), "the WORD is not zero");
        assert_eq!(
            view(filler, &bar, None, None, false),
            PetActionView {
                // Draws nothing and still CARRIES its word (decision 1010) — this is the exact
                // slot the drop core hunts for as a relocation candidate (type 1, low 16 zero),
                // so zeroing it here would both send the wrong word and lose the candidate.
                packed: filler.packed,
                ..Default::default()
            },
            "…but spell id 0 resolves to nothing, so the button is still empty"
        );
    }

    /// A spell the catalog cannot name draws nothing rather than a nameless icon — and with it
    /// goes the autocast pair, which the client also gates on the record resolving.
    #[test]
    fn an_unresolvable_spell_draws_nothing() {
        let bar = state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE);
        let entry = packed(999, PET_ACT_ENABLED);
        let v = view(entry, &bar, None, None, false);
        assert_eq!(
            v,
            PetActionView {
                packed: entry.packed,
                ..Default::default()
            }
        );
        assert!(!v.autocast_allowed && !v.autocast_enabled);
    }

    /// A type outside 1–7 is inert. The client's own default arm under-pushes here (wow-re §2.5);
    /// we answer the empty slot instead, and in particular never reach the spell catalog with an
    /// arbitrary number.
    #[test]
    fn an_unknown_type_byte_is_inert() {
        let bar = state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE);
        let claw = spell("Claw", None);
        let entry = packed(3010, 0x33);
        assert_eq!(
            view(entry, &bar, Some(&claw), None, false),
            PetActionView {
                packed: entry.packed,
                ..Default::default()
            }
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

        latch_press(&mut bar, packed(PET_COMMAND_STAY, PET_ACT_COMMAND), false);
        assert_eq!(bar.spells.command_state() & 0xFF, PET_COMMAND_STAY);
        assert_eq!(bar.spells.react_state(), PET_REACT_DEFENSIVE, "untouched");
        assert!(
            bar.spells.bar_disabled(),
            "the disabled bit is the SERVER's — a command press must preserve it"
        );

        latch_press(
            &mut bar,
            packed(PET_REACT_AGGRESSIVE, PET_ACT_REACTION),
            false,
        );
        assert_eq!(bar.spells.react_state(), PET_REACT_AGGRESSIVE);
        assert_eq!(bar.spells.command_state() & 0xFF, PET_COMMAND_STAY);
        assert!(bar.spells.bar_disabled());

        latch_press(&mut bar, packed(3010, PET_ACT_DISABLED), false);
        assert_eq!(bar.spells.command_state() & 0xFF, PET_COMMAND_STAY);
        assert_eq!(bar.spells.react_state(), PET_REACT_AGGRESSIVE);

        latch_press(
            &mut bar,
            packed(PET_COMMAND_DISMISS, PET_ACT_COMMAND),
            false,
        );
        assert_eq!(
            bar.spells.command_state() & 0xFF,
            PET_COMMAND_STAY,
            "Dismiss ends the pet; it never becomes its standing command"
        );
    }

    /// **The director's report: clicking a lit Follow or Passive un-toggles it.**
    ///
    /// The CheckButton flips itself before `OnClick` runs and the reference's first line is
    /// `this:SetChecked(0)`, so every press starts by taking the light off. `0x4bc940`/`0x4bc960`
    /// put it back by signalling `PET_BAR_UPDATE` **unconditionally** — no old-vs-new compare — so
    /// pressing the mode the pet is already in still repaints. Our feed dedups on what it pushed,
    /// so without a counter in that key the light had no way back.
    ///
    /// A press that the reference does NOT signal must not bump: DISMISS and a spell reach
    /// `0x4bd444` straight, and a hunter's ATTACK skips `0x4bd429` with the latch it never raises.
    #[test]
    fn every_press_the_reference_signals_forces_a_repaint() {
        let mut bar = PetBar {
            spells: state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE),
            ..Default::default()
        };
        let signals = |b: &PetBar| b.bar_signals;

        // Pressing the mode the pet is ALREADY in: nothing about the state moves, and the signal
        // still fires. This is the whole bug.
        let before = signals(&bar);
        latch_press(&mut bar, packed(PET_COMMAND_FOLLOW, PET_ACT_COMMAND), false);
        assert_eq!(bar.spells.command_state(), PET_COMMAND_FOLLOW, "unmoved");
        assert_ne!(signals(&bar), before, "and it repaints anyway");

        let before = signals(&bar);
        latch_press(
            &mut bar,
            packed(PET_REACT_DEFENSIVE, PET_ACT_REACTION),
            false,
        );
        assert_eq!(bar.spells.react_state(), PET_REACT_DEFENSIVE, "unmoved");
        assert_ne!(signals(&bar), before, "same for the reaction side");

        // The presses the reference leaves silent.
        let before = signals(&bar);
        latch_press(
            &mut bar,
            packed(PET_COMMAND_DISMISS, PET_ACT_COMMAND),
            false,
        );
        latch_press(&mut bar, packed(3010, PET_ACT_ENABLED), false);
        latch_press(&mut bar, packed(PET_COMMAND_ATTACK, PET_ACT_COMMAND), false);
        assert_eq!(
            signals(&bar),
            before,
            "Dismiss, a spell and a pet's Attack all reach 0x4bd444 without signalling"
        );

        // A POSSESSED unit's Attack does signal — `0x4bd429`, one instruction before the latch.
        latch_press(&mut bar, packed(PET_COMMAND_ATTACK, PET_ACT_COMMAND), true);
        assert_ne!(signals(&bar), before);
    }

    /// ATTACK is the one command press that can raise the attack latch — **and only while we are
    /// possessing the unit** (`0x4bd420`). On an ordinary pet bar the arm is unreachable, which is
    /// the whole reason its button never checks.
    #[test]
    fn only_a_possessed_units_attack_press_raises_the_latch() {
        let mut bar = PetBar {
            spells: state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE),
            ..Default::default()
        };
        let attack = packed(PET_COMMAND_ATTACK, PET_ACT_COMMAND);
        assert!(!bar.attacking);

        // A hunter's pet: the press goes out (the caller sends regardless) and latches nothing.
        latch_press(&mut bar, attack, false);
        assert!(
            !bar.attacking,
            "an ordinary pet can never raise the latch, so its button can never light"
        );

        latch_press(&mut bar, attack, true);
        assert!(bar.attacking, "a possessed unit can");

        bar.attacking = false;
        latch_press(&mut bar, packed(PET_COMMAND_DISMISS, PET_ACT_COMMAND), true);
        latch_press(&mut bar, packed(PET_REACT_PASSIVE, PET_ACT_REACTION), true);
        assert!(!bar.attacking, "only ATTACK raises it");
    }

    /// `0x5ee5a0` itself — the compare that decides all of the above.
    ///
    /// This is the fact the pet bar was missing: the reference is not asking "is this my pet", it
    /// is asking "is this the unit I am *driving*". A hunter pet answers no on the flag, so no
    /// hunter press ever reaches the latch.
    #[test]
    fn the_latch_gate_is_possession_not_ownership() {
        const FLAGS: u16 = 46;
        const CHARMEDBY: u16 = 10;
        const CREATEDBY: u16 = 14;
        let unit =
            |pairs: &[(u16, u32)]| ObjectStore(benilla_protocol::ObjectFields::from_pairs(pairs));
        let me = Some(0x77u64);

        // An ordinary hunter pet: ours, in combat, not possessed. The answer is no.
        let hunter_pet = unit(&[
            (CREATEDBY, 0x77),
            (CREATEDBY + 1, 0),
            (FLAGS, UNIT_FLAG_PET_IN_COMBAT),
        ]);
        assert!(!possessing(Some(&hunter_pet), me));

        // The same pet under Eyes of the Beast — possessed, and now the latch arm is live.
        let driven = unit(&[
            (CREATEDBY, 0x77),
            (CREATEDBY + 1, 0),
            (FLAGS, UNIT_FLAG_POSSESSED),
        ]);
        assert!(possessing(Some(&driven), me));

        // A mind-controlled mob: CHARMEDBY is the primary leg, so it needs no CREATEDBY at all.
        let charmed = unit(&[(CHARMEDBY, 0x77), (CHARMEDBY + 1, 0), (FLAGS, 0x0100_0000)]);
        assert!(possessing(Some(&charmed), me));

        // Somebody ELSE's possessed unit is not ours to latch for.
        let theirs = unit(&[
            (CHARMEDBY, 0x99),
            (CHARMEDBY + 1, 0),
            (FLAGS, UNIT_FLAG_POSSESSED),
        ]);
        assert!(!possessing(Some(&theirs), me));

        // No descriptor is no possession — the honest reading of missing data, and the same
        // posture the rest of the bar takes.
        assert!(!possessing(None, me));
    }

    /// **The director's report, twice over: the Attack button is never a toggle.**
    ///
    /// Reported by eye against the reference client — *"there is no way in the ref client for the
    /// attack button to stay lit up/toggled like the follow or stay… it's simply a button to send
    /// the pet"* — and the first fix was wrong because it took the latch's *lifetime* to be the
    /// question. It is not: the latch never rises at all on a pet bar, because `0x4bd420` gates it
    /// on possession. This drives a whole pet's worth of ATTACK presses and asserts the button
    /// stays dark through every one.
    #[test]
    fn a_pets_attack_button_never_lights() {
        let mut bar = PetBar {
            spells: state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE),
            ..Default::default()
        };
        let attack = packed(PET_COMMAND_ATTACK, PET_ACT_COMMAND);
        let follow = packed(PET_COMMAND_FOLLOW, PET_ACT_COMMAND);

        for _ in 0..3 {
            assert!(
                commit_press(&mut bar, attack, false, false),
                "the order goes"
            );
            let lit = view(attack, &bar.spells, None, None, bar.attacking);
            assert!(!lit.active, "and the button does not light — ever");
            assert!(
                !lit.attack_active,
                "so the next press orders again, never calls off"
            );
        }
        assert_eq!(
            bar.spells.command_state(),
            PET_COMMAND_FOLLOW,
            "an attack order leaves the standing command alone"
        );
        assert!(
            view(follow, &bar.spells, None, None, bar.attacking).active,
            "and Follow keeps the light that is actually a mode's"
        );

        // Stay is still reachable as a mode — the command byte was never hijacked.
        latch_press(&mut bar, packed(PET_COMMAND_STAY, PET_ACT_COMMAND), false);
        assert_eq!(bar.spells.command_state(), PET_COMMAND_STAY);
    }

    /// The **possess** bar is the other half of the same button, and it does light — which is why
    /// the `isActive` clause, `IsPetAttackActive` and `PetStopAttack` are all still modelled rather
    /// than deleted as dead weight.
    #[test]
    fn a_possessed_units_attack_button_does_light() {
        let mut bar = PetBar {
            spells: state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE),
            ..Default::default()
        };
        let attack = packed(PET_COMMAND_ATTACK, PET_ACT_COMMAND);
        let follow = packed(PET_COMMAND_FOLLOW, PET_ACT_COMMAND);

        assert!(commit_press(&mut bar, attack, false, true));
        assert!(view(attack, &bar.spells, None, None, bar.attacking).active);
        assert!(view(attack, &bar.spells, None, None, bar.attacking).attack_active);
        assert!(
            view(follow, &bar.spells, None, None, bar.attacking).active,
            "the latch is not the command byte — Follow keeps its own light"
        );

        // The call-off: `PetStopAttack` clears the latch, and the latch was the ONLY thing lighting
        // the button — so it goes out.
        bar.attacking = false;
        let called_off = view(attack, &bar.spells, None, None, bar.attacking);
        assert!(!called_off.active);
        assert!(!called_off.attack_active);
    }

    /// **A refused ATTACK costs everything** — no packet, no latch, no light. The reference's veto
    /// jumps to the function epilogue rather than the shared send (`0x4bd414 je 0x4bd4c6`), so a
    /// pet that is dead, stunned, feared, confused, pacified or charmed away simply does not take
    /// the order, and the button must not report that it did.
    ///
    /// This is the join decision 0998 could not test: the gate and the latch, composed.
    #[test]
    fn a_refused_attack_neither_sends_nor_lights_the_button() {
        let mut bar = PetBar {
            spells: state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE),
            ..Default::default()
        };
        let attack = packed(PET_COMMAND_ATTACK, PET_ACT_COMMAND);

        assert!(
            !commit_press(&mut bar, attack, true, true),
            "a vetoed order never reaches the wire"
        );
        assert!(!bar.attacking, "and never raises the latch");
        assert!(
            !view(attack, &bar.spells, None, None, bar.attacking).active,
            "so the button stays dark, even on the one bar where it could light"
        );
        assert_eq!(
            bar.spells.command_state(),
            PET_COMMAND_FOLLOW,
            "a refusal cannot move the standing command either"
        );

        // The same press with the gate clear is the ordinary attack, unchanged. The two gates are
        // NOT symmetric: this one costs the packet, possession costs only the latch.
        assert!(commit_press(&mut bar, attack, false, true));
        assert!(bar.attacking);
        assert!(view(attack, &bar.spells, None, None, bar.attacking).active);
    }

    /// **Touching your target calls the pet off** — `0x493910`'s `0x493a18`, the second call site
    /// of `PetStopAttack`'s core and the one benilla was missing.
    ///
    /// This is the mechanism behind the director's report that Attack is not a toggle. Without it
    /// the latch had only two ways down (the lit button's own second press and a new pet), so an
    /// order lit the button for the rest of the pet's life and read exactly like the Stay/Follow
    /// modes it is supposed to contrast with.
    #[test]
    fn touching_your_target_calls_the_pet_off() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let commands = NetCommands(tx);
        let mut bar = PetBar {
            spells: PetSpells {
                pet_guid: 0xF14,
                ..state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE)
            },
            attacking: true,
            ..Default::default()
        };

        // Selecting when nothing was selected is NOT a clear — `0x493937` returns before anything.
        assert!(!old_target_cleared(None, Some(7)));
        // Replacing one target with another IS, and so is dropping it: the guard is on the OLD
        // selection existing, never on the new one being empty.
        assert!(old_target_cleared(Some(7), Some(9)));
        assert!(old_target_cleared(Some(7), None));
        // A no-op re-select never reaches the clear at all (`0x493540`'s own dedup).
        assert!(!old_target_cleared(Some(7), Some(7)));

        assert!(stop_pet_attack(&mut bar, &commands));
        assert!(!bar.attacking, "the latch is down");
        assert!(
            matches!(rx.try_recv(), Ok(ClientCommand::PetStopAttack { pet_guid }) if pet_guid == 0xF14),
            "and the server is told, with the BAR's guid"
        );

        // The button is dark now, because the latch was the only thing lighting it.
        let attack = packed(PET_COMMAND_ATTACK, PET_ACT_COMMAND);
        assert!(!view(attack, &bar.spells, None, None, bar.attacking).active);

        // Re-clearing is a no-op: `0x4bd65e` returns before the send when the latch is already
        // down, so a target change per second does not spray packets.
        assert!(!stop_pet_attack(&mut bar, &commands));
        assert!(rx.try_recv().is_err());
    }

    /// `PET_ATTACK_START`/`STOP` read the pet's **server-owned** in-combat flag, and only for a
    /// unit we own — the correction to decision 0990, which derived them from the local click
    /// latch. The owner test uses SUMMONEDBY as its fallback, which is the callback's own choice
    /// and not the one `0x5ee5a0` makes for the same-shaped read.
    #[test]
    fn the_attack_events_read_the_pets_combat_flag_not_the_click_latch() {
        const FLAGS: u16 = 46;
        const CHARMEDBY: u16 = 10;
        const SUMMONEDBY: u16 = 12;
        let unit =
            |pairs: &[(u16, u32)]| ObjectStore(benilla_protocol::ObjectFields::from_pairs(pairs));
        let me = Some(0x77u64);

        // Ours by SUMMONEDBY, fighting / not fighting.
        let mine = |flags: u32| unit(&[(SUMMONEDBY, 0x77), (SUMMONEDBY + 1, 0), (FLAGS, flags)]);
        assert_eq!(
            pet_combat_flag(&mine(UNIT_FLAG_PET_IN_COMBAT), me),
            Some(true)
        );
        assert_eq!(pet_combat_flag(&mine(0), me), Some(false));
        // An unrelated flag bit is not combat — the callback tests one bit.
        assert_eq!(pet_combat_flag(&mine(0x1000), me), Some(false));

        // Somebody else's minion never fires, however hard it is fighting.
        let theirs = unit(&[
            (SUMMONEDBY, 0x99),
            (SUMMONEDBY + 1, 0),
            (FLAGS, UNIT_FLAG_PET_IN_COMBAT),
        ]);
        assert_eq!(pet_combat_flag(&theirs, me), None);

        // CHARMEDBY wins over SUMMONEDBY: a mob WE mind-controlled is ours even though it was
        // summoned by nobody, and a minion charmed AWAY from us stops being ours.
        let charmed_by_me = unit(&[
            (CHARMEDBY, 0x77),
            (CHARMEDBY + 1, 0),
            (FLAGS, UNIT_FLAG_PET_IN_COMBAT),
        ]);
        assert_eq!(pet_combat_flag(&charmed_by_me, me), Some(true));
        let stolen = unit(&[
            (CHARMEDBY, 0x99),
            (CHARMEDBY + 1, 0),
            (SUMMONEDBY, 0x77),
            (SUMMONEDBY + 1, 0),
            (FLAGS, UNIT_FLAG_PET_IN_COMBAT),
        ]);
        assert_eq!(pet_combat_flag(&stolen, me), None);
    }

    /// Only the ATTACK order consults the validator. The reference's type-7 arm branches exactly
    /// twice, and every other action — the two modes, DISMISS, and anything `>= 4` — leaves down a
    /// path that sends unconditionally. Gating them too would make a stunned pet impossible to
    /// dismiss or to put back on Follow, which is not what the binary does.
    #[test]
    fn only_the_attack_order_is_gated() {
        assert!(is_attack_order(packed(PET_COMMAND_ATTACK, PET_ACT_COMMAND)));
        for action in [
            PET_COMMAND_STAY,
            PET_COMMAND_FOLLOW,
            PET_COMMAND_DISMISS,
            4,
            9,
        ] {
            assert!(
                !is_attack_order(packed(action, PET_ACT_COMMAND)),
                "command {action} sends unconditionally"
            );
        }
        // A REACTION slot whose action happens to equal ATTACK's 2 (that is Aggressive) is a
        // different word entirely — the type byte decides, never the action alone.
        assert!(!is_attack_order(packed(
            PET_REACT_AGGRESSIVE,
            PET_ACT_REACTION
        )));
    }

    /// A pet with one aura in slot 0. `AURAFLAGS` is nibble-packed 8 slots to the u32, so slot 0's
    /// nibble is the low four bits: `0x2` is an effect-index bit (what makes the slot *live*) and
    /// `0x1` is `AFLAG_CANCELABLE` — the bit `0x4bcea0` actually tests.
    fn pet_running(spell_id: u32, nibble: u32) -> ObjectStore {
        const AURA: u16 = 47;
        const AURAFLAGS: u16 = 95;
        ObjectStore(benilla_protocol::ObjectFields::from_pairs(&[
            (AURA, spell_id),
            (AURAFLAGS, nibble),
        ]))
    }

    /// A spell with an active icon — the shape the predicate needs before it can ever fire.
    fn toggle_spell() -> benilla_formats::SpellDisplay {
        benilla_formats::SpellDisplay {
            active_icon_id: 122,
            active_icon: Some("Interface\\Icons\\Ability_Druid_Cower".into()),
            ..spell("Cower", Some("Rank 1"))
        }
    }

    /// The showing-active predicate, all four of its ways to say no. The pet-side twin of
    /// `0x4e55f0` is the *same function* we already had, so what these assert is the wiring: the
    /// pet's store goes in, and the three tests are the reference's three.
    #[test]
    fn a_pet_spell_shows_active_only_while_it_is_a_live_cancelable_aura_on_the_pet() {
        let slot = packed(2645, benilla_protocol::messages::PET_TYPE_SPELL_FIRST);
        let running = pet_running(2645, 0x3);
        let d = toggle_spell();

        assert_eq!(
            active_aura_press(slot, Some(&running), Some(&d)),
            Some(2645)
        );

        // No ActiveIconID: never a toggle — and that gate is on the SEND too, so this spell
        // re-casts rather than cancelling however live its aura is (the reference's own quirk).
        let plain = spell("Growl", None);
        assert_eq!(active_aura_press(slot, Some(&running), Some(&plain)), None);
        // Live but NOT cancelable (effect-index bit only).
        let uncancelable = pet_running(2645, 0x2);
        assert_eq!(active_aura_press(slot, Some(&uncancelable), Some(&d)), None);
        // A different spell's aura, and no pet descriptor at all.
        assert_eq!(
            active_aura_press(slot, Some(&pet_running(768, 0x3)), Some(&d)),
            None
        );
        assert_eq!(active_aura_press(slot, None, Some(&d)), None);
        // A COMMAND slot whose action equals the spell id is a different word entirely — the type
        // byte decides here exactly as it does for the ATTACK gate.
        assert_eq!(
            active_aura_press(packed(2645, PET_ACT_COMMAND), Some(&running), Some(&d)),
            None
        );
    }

    /// The icon swap itself: `ActiveIconID`'s texture replaces `SpellIconID`'s while the spell is
    /// running, and the button keeps everything else it had.
    #[test]
    fn an_active_pet_spell_draws_its_active_icon() {
        let slot = packed(2645, benilla_protocol::messages::PET_TYPE_SPELL_FIRST);
        let bar = state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE);
        let d = toggle_spell();

        let idle = slot_view(slot, &bar, Some(&d), None, false, false);
        assert_eq!(idle.texture, d.icon);
        let active = slot_view(slot, &bar, Some(&d), None, false, true);
        assert_eq!(active.texture, d.active_icon);
        assert_eq!(
            active.name, idle.name,
            "only the icon swaps — the name, rank and autocast flags are untouched"
        );
        assert!(
            !active.active,
            "and it is still not `isActive`: a spell slot pushes nil there on every path"
        );
    }

    /// An active spell whose `ActiveIconID` does not resolve in `SpellIcon.dbc` pushes **nil**,
    /// not the inactive art. The reference looks up whichever id the predicate chose and gives up
    /// if that lookup fails (`0x4bdd50`) — falling back would draw "not running" on a running
    /// spell, which is worse than drawing nothing.
    #[test]
    fn an_unresolvable_active_icon_hides_rather_than_falling_back() {
        let slot = packed(2645, benilla_protocol::messages::PET_TYPE_SPELL_FIRST);
        let bar = state(PET_COMMAND_FOLLOW, PET_REACT_DEFENSIVE);
        let d = benilla_formats::SpellDisplay {
            active_icon_id: 9999,
            active_icon: None,
            ..spell("Cower", None)
        };
        assert!(slot_view(slot, &bar, Some(&d), None, false, true)
            .texture
            .is_none());
    }
}
