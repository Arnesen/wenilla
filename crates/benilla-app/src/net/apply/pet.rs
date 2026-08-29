//! The pet bar's wire arms (decisions 0982, 0988) — `SMSG_PET_SPELLS`, `SMSG_PET_MODE`,
//! `SMSG_PET_ACTION_FEEDBACK`, `SMSG_PET_CAST_FAILED` folded into [`crate::ui_pet::PetBar`].
//!
//! The whole file is short because `SMSG_PET_SPELLS` is not a delta — it **is** the bar, so
//! applying it is a replace. These four arms own the bar's CONTENTS; its lit state is
//! [`crate::ui_pet`]'s, latched on the press because the server never answers one (that file's
//! module doc has the why). The only state this file writes is what actually arrives on the wire.
//!
//! The two refusal arms deliberately reuse the *player's* red-line queues rather than growing pet
//! copies: both resolve their text through the VM's own `GlobalStrings.lua` the same way, and the
//! cast-fail resolver keys its power family on the failing **spell's** record ([`cast_fail`]'s
//! `power_keys(spell.power_type)`), so a pet's focus ability already reads "Not enough focus"
//! with no pet-awareness anywhere in the display layer.
//!
//! [`cast_fail`]: crate::ui_action

// `bevy::platform::time::Instant`, not `bevy::platform::time::Instant`: this flows into `crate::cooldowns`/`crate::ui_script::UiClock`, which on wasm32 (the default `web` Bevy feature) is a genuinely different type from `bevy::platform::time::Instant` — a plain alias for it everywhere else.
use bevy::platform::time::Instant;

use bevy::prelude::*;

use benilla_protocol::messages::{PetMode, PetSpells};

use crate::ui_action::{CastErrors, Spells, UiError, UiErrorKeys};
use crate::ui_pet::PetBar;

/// `SMSG_PET_SPELLS` — replace the whole bar, and reseed the pet's own cooldown store from the
/// packet's tail.
///
/// A **zero guid** is the teardown (`Player::RemovePetActionBar`): the bar goes away and the
/// cooldown store goes with it, because the next pet is a different unit with different timers.
/// Everything else is a wholesale replace — including a re-send from the same pet, which is how a
/// learned spell, a mode change or an autocast toggle actually reaches the bar.
pub(super) fn pet_spells(spells: PetSpells, catalog: Option<&Spells>, bar: &mut PetBar) {
    if spells.pet_guid == 0 {
        if bar.spells.pet_guid != 0 {
            debug!("net: pet bar torn down");
        }
        *bar = PetBar::default();
        return;
    }
    // A pet-GUID CHANGE clears the attack latch (`0x4bc8ce`, the client's own single writer of the
    // pet guid does it unconditionally) — a freshly summoned pet is not attacking, whatever the
    // last one was doing. A re-send from the SAME pet leaves it alone: a learned spell must not
    // silently call the pet off.
    if bar.spells.pet_guid != spells.pet_guid {
        bar.attacking = false;
    }
    debug!(
        "net: pet bar for {:#x} — react {} command {} {}, {} known spell(s), {} cooldown(s)",
        spells.pet_guid,
        spells.react_state(),
        spells.command_state(),
        if spells.bar_disabled() {
            "DISABLED"
        } else {
            "usable"
        },
        spells.spells.len(),
        spells.cooldowns.len(),
    );
    // The ten words, spelled out. Worth its own line rather than a raw hex dump: every question
    // this packet raises ("why is that slot empty", "why is nothing lit", "is that spell in the
    // catalog") is answered by seeing the type/action pair beside the name we resolved for it, and
    // reading that off `0xc1000bc2` by hand is exactly the step nobody does.
    debug!(
        "net: pet bar slots — {}",
        spells
            .bar
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let what = match e.kind() {
                    benilla_protocol::messages::PET_ACT_COMMAND => format!("cmd {}", e.action()),
                    benilla_protocol::messages::PET_ACT_REACTION => format!("react {}", e.action()),
                    _ if e.is_empty() => "empty".to_string(),
                    _ => match catalog.and_then(|c| c.catalog.get(e.action())) {
                        Some(d) => format!("{} ({})", d.name, e.action()),
                        None => format!("spell {} NOT IN CATALOG", e.action()),
                    },
                };
                format!("{}:{what}/{:#04x}", i + 1, e.kind())
            })
            .collect::<Vec<_>>()
            .join(" ")
    );
    // The spell LIST, spelled out the same way and for the same reason (decision 1032). It is a
    // different question from the bar's — "why is the pet book shorter than the packet said" — and
    // the answer is always one of three things this line names: the id resolves to no `Spell.dbc`
    // record, it carries `DO_NOT_DISPLAY` (the book's whole add-gate, `0x4b2f90`), or it is in.
    // vmangos sends the pet's runtime passives here alongside its real spells, so the gap between
    // the two counts is usually large and entirely correct.
    debug!(
        "net: pet spellbook — {}",
        spells
            .spells
            .iter()
            .map(|e| {
                match catalog.and_then(|c| c.catalog.get(e.action())) {
                    Some(d) if d.in_pet_book() => format!("{} ({})", d.name, e.action()),
                    Some(d) => format!("{} ({}) DO_NOT_DISPLAY", d.name, e.action()),
                    None => format!("{} NOT IN CATALOG", e.action()),
                }
            })
            .collect::<Vec<_>>()
            .join(" · ")
    );
    let now = Instant::now();
    bar.cooldowns = crate::cooldowns::Cooldowns::default();
    for cd in &spells.cooldowns {
        let display = catalog.and_then(|c| c.catalog.get(cd.spell_id));
        bar.cooldowns.seed_pet(cd, display, now);
    }
    bar.spells = spells;
}

/// `SMSG_PET_MODE` — the react/command state alone. Applied only when it names the pet whose bar
/// we actually hold: a mode packet for a unit we have no bar for has nothing to write into, and
/// taking its state anyway would light a reaction button on the wrong pet's bar.
pub(super) fn pet_mode(mode: PetMode, bar: &mut PetBar) {
    if bar.spells.pet_guid == 0 || bar.spells.pet_guid != mode.pet_guid {
        return;
    }
    debug!("net: pet mode — state {:#010x}", mode.state);
    // The whole dword, verbatim — the client stores this packet and `SMSG_PET_SPELLS`' state field
    // through the same writer (`0x4bc930`), and bit 27 can only ever arrive this way.
    bar.spells.state = mode.state;
}

/// `SMSG_PET_ACTION_FEEDBACK` — one reason byte for a refused order, queued onto the red line by
/// GlobalStrings key ([`UiErrorKeys`], the `DisplayError` route). An unrecognised code queues
/// nothing, exactly as an absent key shows nothing.
pub(super) fn pet_action_feedback(reason: u8, errors: &mut UiErrorKeys) {
    debug!("net: pet action feedback {reason}");
    if let Some(key) = pet_feedback_key(reason) {
        errors.0.push(UiError::key(key));
    }
}

/// The `SMSG_PET_ACTION_FEEDBACK` reason → its `GlobalStrings.lua` key.
///
/// vmangos sends exactly two (`Unit::SendPetActionFeedback`'s call sites): `1` when the pet cannot
/// path to the ordered spot, `2` when the ordered target is out of its reach. Both keys ship in
/// 1.12's `GlobalStrings.lua` (`PET_SPELL_NOPATH` at l.3054, `SPELL_FAILED_OUT_OF_RANGE`), so the
/// text — and its localization — comes from the VM like every other error line.
fn pet_feedback_key(reason: u8) -> Option<&'static str> {
    Some(match reason {
        1 => "PET_SPELL_NOPATH",
        2 => "SPELL_FAILED_OUT_OF_RANGE",
        _ => return None,
    })
}

/// `SMSG_PET_CAST_FAILED` — the pet's cast refusal, queued onto the SAME red line as our own
/// through [`CastErrors`], because the resolver is spell-keyed and needs no pet-awareness.
///
/// What it deliberately does NOT do is touch our cast state: the caster is the pet, so there is no
/// pending cast of ours to revert, no GCD of ours to clear and no button of ours to unflash — the
/// three things `cast_result`'s own failure path does. Reusing that path would have made a pet's
/// refused Growl cancel the player's cast bar.
pub(super) fn pet_cast_failed(spell_id: u32, reason: Option<u8>, errors: &mut CastErrors) {
    debug!("net: pet cast failed — spell {spell_id} reason {reason:?}");
    if let Some(reason) = reason {
        errors.push_local(spell_id, reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_protocol::messages::{PetActionEntry, PET_ACT_COMMAND, PET_COMMAND_ATTACK};

    fn a_bar(guid: u64) -> PetSpells {
        let mut s = PetSpells {
            pet_guid: guid,
            state: 0x0101, // react Defensive, command Follow
            ..Default::default()
        };
        s.bar[0] = PetActionEntry::from(PET_COMMAND_ATTACK | (u32::from(PET_ACT_COMMAND) << 24));
        s
    }

    /// The teardown clears the bar AND its cooldown store — the next pet is a different unit, so
    /// carrying its predecessor's timers would sweep the new pet's buttons for no reason.
    #[test]
    fn a_zero_guid_tears_the_whole_bar_down() {
        let mut bar = PetBar::default();
        pet_spells(a_bar(0x2A), None, &mut bar);
        assert_eq!(bar.spells.pet_guid, 0x2A);

        pet_spells(PetSpells::default(), None, &mut bar);
        assert_eq!(bar.spells, PetSpells::default());
        assert!(!bar.has_bar());
    }

    /// A mode packet for a DIFFERENT unit is dropped — it would otherwise light a reaction button
    /// on the bar we are holding for somebody else's pet.
    #[test]
    fn pet_mode_only_writes_its_own_bar() {
        let mut bar = PetBar::default();
        pet_spells(a_bar(0x2A), None, &mut bar);

        pet_mode(
            PetMode {
                pet_guid: 0x99,
                state: 0x0202,
            },
            &mut bar,
        );
        assert_eq!(bar.spells.react_state(), 1, "a stranger's mode is ignored");

        pet_mode(
            PetMode {
                pet_guid: 0x2A,
                state: 0x0202,
            },
            &mut bar,
        );
        assert_eq!(bar.spells.react_state(), 2);
        assert_eq!(bar.spells.command_state(), 2);
        // The bar itself is untouched — PET_MODE carries no slots.
        assert_eq!(bar.spells.bar[0].action(), PET_COMMAND_ATTACK);
    }

    /// The attack latch is cleared by a pet CHANGE and survives a re-send from the same pet — the
    /// client's own single pet-guid writer does exactly this (`0x4bc8ce`). A learned spell must
    /// not silently call the pet off; a new pet must not inherit the last one's attack.
    #[test]
    fn the_attack_latch_survives_a_resend_and_dies_on_a_new_pet() {
        let mut bar = PetBar::default();
        pet_spells(a_bar(0x2A), None, &mut bar);
        bar.attacking = true;

        pet_spells(a_bar(0x2A), None, &mut bar);
        assert!(bar.attacking, "the same pet re-sending keeps its attack");

        pet_spells(a_bar(0x99), None, &mut bar);
        assert!(!bar.attacking, "a different pet is not attacking");
    }

    /// Only the two codes vmangos actually sends resolve; anything else draws nothing rather than
    /// putting a bare number on the red line.
    #[test]
    fn only_the_shipped_feedback_codes_resolve() {
        let mut errors = UiErrorKeys::default();
        for reason in [0u8, 1, 2, 3, 200] {
            pet_action_feedback(reason, &mut errors);
        }
        assert_eq!(
            errors.0.iter().map(|e| e.key).collect::<Vec<_>>(),
            ["PET_SPELL_NOPATH", "SPELL_FAILED_OUT_OF_RANGE"]
        );
    }
}
