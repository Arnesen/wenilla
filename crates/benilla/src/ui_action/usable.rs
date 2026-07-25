//! The plain-spell **usable walk** — `Spell_C::IsSpellUsableNow 0x6e3d60` (wow-re
//! `action-button-state-api.md` §2a, byte-verified 2026-07-10), the compute behind
//! `IsUsableAction`'s grey tint beyond the power gate. The §2a ordered gate table, transcribed;
//! any tripped gate answers `(usable=false, oom=false)`, and ONLY the power leg (the last) sets
//! `notEnoughMana` — the §5's B2, re-confirmed.
//!
//! Modeled legs: the TRADE_SKILL early-out · dead (leg 1) · reagents/totems (leg 3) · required
//! equipped item (leg 4) · the shapeshift-form gate (leg 6, [`SpellDisplay::usable_in_form`]) ·
//! only-stealthed (leg 7) · not-in-combat (leg 8) · CasterAuraState (leg 9) · TargetAuraState +
//! its CanAttack/CanAssist fork (legs 10/10b — the ONE target-dependent pair; our per-frame
//! diff-push recomputes it on target change for free, where the ref re-runs its cache on events)
//! · the bit-25 cooldown fold (leg 11) · power (leg 12).
//!
//! Deferred, named (each answers usable=true until modeled): leg 2's caster aura-immunity
//! helpers (`0x6e9f20/40/60` — silence/pacify vs the spell's school/mechanic; needs an aura-type
//! model), leg 5's self-only identity (`AttributesEx` b20/b22 + `caster+0xe68+0x1029`), leg 4's
//! `AttributesEx3` sub-conditions and the broken-durability exclusion (no durability model), and
//! the ghost state beyond plain death. CanAssist inside 10b is the reaction-rank stand-in the
//! ring/`can_attack` share, pending the true `0x6066f0` walk.

use std::time::Instant;

use benilla_formats::{
    SpellDisplay, ATTR_CASTABLE_WHILE_DEAD, ATTR_NOT_IN_COMBAT, ATTR_ONLY_STEALTHED,
    SPELL_EFFECT_TRADE_SKILL,
};

use crate::cooldowns::Cooldowns;
use crate::items::Items;
use crate::net::{NetCommands, ObjectStore, Reputations};
use crate::target::{can_attack, ring_reaction, Factions};

use super::Spells;

/// `UNIT_FLAG_IN_COMBAT` (vmangos `UnitDefines.h`, bit 19) — leg 8's caster unit-flag test.
const UNIT_FLAG_IN_COMBAT: u32 = 0x0008_0000;

/// The implicit-target enums leg 10b forks on (`0x6e3f8a`/`0x6e3fa2`): 6 = single enemy →
/// `CanAttack 0x606980`, 21 = single friend → `CanAssist 0x6066f0`.
const IMPLICIT_TARGET_ENEMY: u32 = 6;
const IMPLICIT_TARGET_FRIEND: u32 = 21;

/// Everything the walk reads besides the spell itself. `target_store` is the CURRENT TARGET's
/// (leg 10 resolves the current-target global `0xb4e2d8`, not an explicit cast target).
pub(crate) struct UsableCtx<'a> {
    pub(crate) store: &'a ObjectStore,
    pub(crate) target_store: Option<&'a ObjectStore>,
    pub(crate) factions: Option<&'a Factions>,
    pub(crate) reputations: &'a Reputations,
    pub(crate) cooldowns: &'a Cooldowns,
    pub(crate) now: Instant,
}

/// Leg 4's own test (`0x6e40e0`), shared with the spell tooltip's requirement line: does some
/// WORN item match `EquippedItemClass` + `EquippedItemSubClassMask`? `true` when the spell asks
/// for nothing (`class < 0`). An equipped item whose template hasn't streamed yet counts as a
/// match — never grey (and never red) on missing data, the catalog-absent convention.
pub(crate) fn equipped_item_fits(
    d: &SpellDisplay,
    store: &ObjectStore,
    items: &mut Items,
    commands: &NetCommands,
) -> bool {
    if d.equipped_item_class < 0 {
        return true;
    }
    let class = d.equipped_item_class as u32;
    (0..19).any(|slot| {
        let Some(guid) = store.0.player_inv_slot(slot).filter(|&g| g != 0) else {
            return false;
        };
        let Some(entry) = items.object(guid).and_then(|o| o.object_entry()) else {
            return false;
        };
        let Some(t) = items.template(entry, guid, commands) else {
            return true; // unresolved template: benefit of the doubt
        };
        t.class == class
            && (d.equipped_item_subclass_mask == 0
                || d.equipped_item_subclass_mask & (1 << t.subclass) != 0)
    })
}

/// The walk. Returns `(usable, not_enough_mana)` — the `IsUsableAction` pair.
pub(crate) fn spell_usable(
    spell_id: u32,
    d: &SpellDisplay,
    spells: &Spells,
    ctx: &UsableCtx,
    items: &mut Items,
    commands: &NetCommands,
) -> (bool, bool) {
    // Early-out (`0x6e3d99`): a tradeskill "spell" is always usable.
    if d.effect_1 == SPELL_EFFECT_TRADE_SKILL {
        return (true, false);
    }
    // Leg 1: dead casters use nothing without the castable-while-dead attribute.
    if ctx.store.0.unit_health() == Some(0) && d.attributes & ATTR_CASTABLE_WHILE_DEAD == 0 {
        return (false, false);
    }
    // Leg 3 (`0x6e4000`): every reagent pair in bag counts; every totem tool present.
    for &(entry, count) in &d.reagents {
        if entry != 0 && crate::ui_items::count_of(&ctx.store.0, items, entry) < count {
            return (false, false);
        }
    }
    for &totem in &d.totems {
        if totem != 0 && crate::ui_items::count_of(&ctx.store.0, items, totem) == 0 {
            return (false, false);
        }
    }
    // Leg 4 (`0x6e40e0`): some worn item must match the class + subclass mask.
    if !equipped_item_fits(d, ctx.store, items, commands) {
        return (false, false);
    }
    // Leg 6 (`0x612480`): the shapeshift-form gate — the form's stance flag from
    // SpellShapeshiftForm.dbc decides whether it counts as "shapeshifted".
    let form = ctx.store.0.unit_shapeshift_form();
    let form_is_stance = spells
        .forms
        .get(&u32::from(form))
        .is_some_and(|f| f.is_stance());
    if !d.usable_in_form(form, form_is_stance) {
        return (false, false);
    }
    // Leg 7: only-stealthed spells need the CREEP vis flag (the stealth aura's byte).
    if d.attributes & ATTR_ONLY_STEALTHED != 0 && !ctx.store.0.unit_is_stealthed() {
        return (false, false);
    }
    // Leg 8: only-out-of-combat spells grey while UNIT_FLAG_IN_COMBAT is up.
    if d.attributes & ATTR_NOT_IN_COMBAT != 0 && ctx.store.0.unit_flags() & UNIT_FLAG_IN_COMBAT != 0
    {
        return (false, false);
    }
    // Leg 9: the caster's own aura state.
    if d.caster_aura_state != 0
        && ctx.store.0.unit_aura_state() & (1 << (d.caster_aura_state - 1)) == 0
    {
        return (false, false);
    }
    // Legs 10/10b: the target's aura state — the walk's ONE target-dependent pair (§2a B1).
    // No current target ⇒ unusable; then the aura-state bit; then the relation fork.
    if d.target_aura_state != 0 {
        let Some(target) = ctx.target_store else {
            return (false, false);
        };
        if target.0.unit_aura_state() & (1 << (d.target_aura_state - 1)) == 0 {
            return (false, false);
        }
        match d.implicit_target_a1 {
            // The shared `CanAttack 0x606980` the ring/scan transcribe.
            IMPLICIT_TARGET_ENEMY
                if !can_attack(Some(target), ctx.factions, ctx.reputations, Some(ctx.store)) =>
            {
                return (false, false);
            }
            // CanAssist stand-in: reaction rank >= friendly (module docs).
            IMPLICIT_TARGET_FRIEND
                if ring_reaction(ctx.factions, ctx.reputations, Some(target), Some(ctx.store))
                    < 4 =>
            {
                return (false, false);
            }
            _ => {}
        }
    }
    // Leg 11: ONLY a cooldown-on-event spell folds its cooldown into usable (B3) — Stealth
    // greys while its effect runs; Fireball never greys mid-cooldown.
    if d.cooldown_on_event() && ctx.cooldowns.is_on_cooldown(spell_id, Some(d), ctx.now) {
        return (false, false);
    }
    // Leg 12 (`0x6e3fba`–`0x6e3feb`): the power gate — the SOLE notEnoughMana writer (B2).
    // Percent costs scale from base mana (mana spells) / max power (the vmangos
    // CalculatePowerCost basis).
    let ty = d.power_type as u8;
    let base = if d.mana_cost_pct == 0 {
        0
    } else if d.power_type == 0 {
        ctx.store.0.unit_base_mana().unwrap_or(0)
    } else {
        ctx.store.0.unit_max_power(ty).unwrap_or(0)
    };
    let cost = d.mana_cost + base * d.mana_cost_pct / 100;
    if ctx.store.0.unit_power(ty).unwrap_or(0) < cost {
        return (false, true);
    }
    (true, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_protocol::ObjectFields;

    // Field indices mirrored from the protocol crate's own consts (private there): health 22,
    // power1 23, flags 46, aurastate 125, bytes_1 138.
    fn player(pairs: &[(u16, u32)]) -> ObjectStore {
        let mut base = vec![(22u16, 100u32), (23, 500)];
        base.extend_from_slice(pairs);
        ObjectStore(ObjectFields::from_pairs(&base))
    }

    fn ctx<'a>(
        store: &'a ObjectStore,
        cooldowns: &'a Cooldowns,
        reputations: &'a Reputations,
    ) -> UsableCtx<'a> {
        UsableCtx {
            store,
            target_store: None,
            factions: None,
            reputations,
            cooldowns,
            now: Instant::now(),
        }
    }

    fn walk(d: &SpellDisplay, store: &ObjectStore) -> (bool, bool) {
        let cooldowns = Cooldowns::default();
        let reputations = Reputations(Vec::new());
        let spells = Spells::empty_for_tests();
        let mut items = Items::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let commands = NetCommands(tx);
        spell_usable(
            1,
            d,
            &spells,
            &ctx(store, &cooldowns, &reputations),
            &mut items,
            &commands,
        )
    }

    /// Each modeled gate trips alone, and only the power leg raises notEnoughMana (B2).
    #[test]
    fn gates_trip_independently_and_only_power_sets_oom() {
        let alive = player(&[]);
        let d = SpellDisplay::default();
        assert_eq!(walk(&d, &alive), (true, false));

        // Leg 1: dead — unusable, not oom; the attribute waives it.
        let dead = player(&[(22, 0)]);
        assert_eq!(walk(&d, &dead), (false, false));
        let while_dead = SpellDisplay {
            attributes: ATTR_CASTABLE_WHILE_DEAD,
            ..Default::default()
        };
        assert_eq!(walk(&while_dead, &dead), (true, false));

        // Leg 3: a missing reagent.
        let reagent = SpellDisplay {
            reagents: [
                (17056, 1),
                (0, 0),
                (0, 0),
                (0, 0),
                (0, 0),
                (0, 0),
                (0, 0),
                (0, 0),
            ],
            ..Default::default()
        };
        assert_eq!(walk(&reagent, &alive), (false, false));

        // Leg 6: a cat-form spell out of form.
        let claw = SpellDisplay {
            stances: 0x1,
            ..Default::default()
        };
        assert_eq!(walk(&claw, &alive), (false, false));
        let in_cat = player(&[(138, 1 << 16)]);
        assert_eq!(walk(&claw, &in_cat), (true, false));

        // Leg 7: only-stealthed vs the CREEP byte.
        let ambush = SpellDisplay {
            attributes: ATTR_ONLY_STEALTHED,
            ..Default::default()
        };
        assert_eq!(walk(&ambush, &alive), (false, false));
        let sneaking = player(&[(138, 0x2 << 24)]);
        assert_eq!(walk(&ambush, &sneaking), (true, false));

        // Leg 8: not-in-combat vs UNIT_FLAG_IN_COMBAT.
        let mount = SpellDisplay {
            attributes: ATTR_NOT_IN_COMBAT,
            ..Default::default()
        };
        assert_eq!(walk(&mount, &alive), (true, false));
        let fighting = player(&[(46, UNIT_FLAG_IN_COMBAT)]);
        assert_eq!(walk(&mount, &fighting), (false, false));

        // Leg 9: CasterAuraState (defense = 1 → bit 0).
        let revenge = SpellDisplay {
            caster_aura_state: 1,
            ..Default::default()
        };
        assert_eq!(walk(&revenge, &alive), (false, false));
        let defended = player(&[(125, 0x1)]);
        assert_eq!(walk(&revenge, &defended), (true, false));

        // Leg 10: TargetAuraState with no current target.
        let execute = SpellDisplay {
            target_aura_state: 2,
            implicit_target_a1: 6,
            ..Default::default()
        };
        assert_eq!(walk(&execute, &alive), (false, false));

        // Leg 12: power — the only oom.
        let costly = SpellDisplay {
            mana_cost: 501,
            ..Default::default()
        };
        assert_eq!(walk(&costly, &alive), (false, true));

        // The early-out beats every gate.
        let tradeskill = SpellDisplay {
            effect_1: SPELL_EFFECT_TRADE_SKILL,
            mana_cost: 9999,
            stances: 0x1,
            ..Default::default()
        };
        assert_eq!(walk(&tradeskill, &dead), (true, false));
    }

    /// Leg 10 against a target store: the aura-state bit gates, and the enemy fork's CanAttack
    /// passes on the default-neutral reaction.
    #[test]
    fn target_aura_state_reads_the_current_target() {
        let me = player(&[]);
        let cooldowns = Cooldowns::default();
        let reputations = Reputations(Vec::new());
        let spells = Spells::empty_for_tests();
        let mut items = Items::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let commands = NetCommands(tx);
        let execute = SpellDisplay {
            target_aura_state: 2,
            implicit_target_a1: 6,
            ..Default::default()
        };

        let healthy = ObjectStore(ObjectFields::from_pairs(&[(22, 100), (125, 0)]));
        let low = ObjectStore(ObjectFields::from_pairs(&[(22, 10), (125, 0x2)]));
        for (target, expect) in [(&healthy, false), (&low, true)] {
            let ctx = UsableCtx {
                store: &me,
                target_store: Some(target),
                factions: None,
                reputations: &reputations,
                cooldowns: &cooldowns,
                now: Instant::now(),
            };
            assert_eq!(
                spell_usable(5308, &execute, &spells, &ctx, &mut items, &commands),
                (expect, false)
            );
        }
    }
}
