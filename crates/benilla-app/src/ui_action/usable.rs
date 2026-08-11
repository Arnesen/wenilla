//! The plain-spell **usable walk** — `Spell_C::IsSpellUsableNow 0x6e3d60` (wow-re
//! `action-button-state-api.md` §2a, byte-verified 2026-07-10), the compute behind
//! `IsUsableAction`'s grey tint beyond the power gate. The §2a ordered gate table, transcribed;
//! any tripped gate answers `(usable=false, oom=false)`, and ONLY the power leg (the last) sets
//! `notEnoughMana` — the §5's B2, re-confirmed.
//!
//! Modeled legs: the TRADE_SKILL early-out · dead (leg 1) · reagents/totems (leg 3) · required
//! equipped item (leg 4) · the combo-point gate (leg 5) · the shapeshift-form gate (leg 6,
//! [`SpellDisplay::usable_in_form`]) · only-stealthed (leg 7) · not-in-combat (leg 8) ·
//! CasterAuraState (leg 9) · TargetAuraState + its CanAttack/CanAssist fork (legs 10/10b — the
//! ONE target-dependent pair, §5-proven: the current-target GUID globals `0xb4e2d8/dc` appear
//! nowhere earlier in the whole function, 0879; our per-frame diff-push recomputes it on target
//! change for free, where the ref re-runs its cache on events) · the bit-25 cooldown fold
//! (leg 11) · power (leg 12).
//!
//! Deferred, named (each answers usable=true until modeled): leg 2's caster aura-immunity
//! helpers (`0x6e9f20/40/60` — silence/pacify vs the spell's school/mechanic; needs an aura-type
//! model), leg 4's `AttributesEx3` sub-conditions and the broken-durability exclusion (no
//! durability model), and the ghost state beyond plain death. CanAssist inside 10b is the
//! reaction-rank stand-in the ring/`can_attack` share, pending the true `0x6066f0` walk.

use crate::ui_items::{count_of, InventoryScope};
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
    if d.effects[0] == SPELL_EFFECT_TRADE_SKILL {
        return (true, false);
    }
    // Leg 1: dead casters use nothing without the castable-while-dead attribute.
    if ctx.store.0.unit_health() == Some(0) && d.attributes & ATTR_CASTABLE_WHILE_DEAD == 0 {
        return (false, false);
    }
    // Leg 3 (`0x6e4000`): every reagent pair in bag counts; every totem tool present.
    for &(entry, count) in &d.reagents {
        if entry != 0 && count_of(&ctx.store.0, items, entry, InventoryScope::CARRIED) < count {
            return (false, false);
        }
    }
    for &totem in &d.totems {
        if totem != 0 && count_of(&ctx.store.0, items, totem, InventoryScope::CARRIED) == 0 {
            return (false, false);
        }
    }
    // Leg 4 (`0x6e40e0`): some worn item must match the class + subclass mask.
    if !equipped_item_fits(d, ctx.store, items, commands) {
        return (false, false);
    }
    // Leg 5 (`0x6e3e7a`–`0x6e3eb2`): the combo-point gate, §5-VERIFIED end to end (0879). A
    // `test [SpellRec+0x1c], 0x500000` selects finishing moves — ONE any-of test with one jcc, so
    // `AttributesEx` b20 and b22 never fork — and then `mov al,[ecx+0x1029]; test al,al` fails the
    // leg when the caster's combo-point byte is 0. That is `PLAYER_FIELD_BYTES` byte 1 off the
    // player-block base, one byte below the honor rank the item-usable gate reads at `+0x102b`.
    // The leg's second clause (`0x6e3e83`–`0x6e3e9e`) re-checks the caster's own GUID against the
    // active player's — structurally tautological, since `esi` was found by that same GUID.
    //
    // CASTER-only, now proven rather than assumed: the current-target GUID globals `0xb4e2d8/dc`
    // appear nowhere before leg 10 in the whole of `0x6e3d60`. So a point banked on mob A with mob
    // B selected really does leave this button lit, and the server really does refuse it with
    // `SPELL_FAILED_BAD_TARGETS` — the client's own divergence, not ours (0869, 0879). Overpower
    // is this leg's whole reason to exist: it has no aura state, so every other leg passes and the
    // button stayed lit forever without it.
    //
    // NO class gate here, and that asymmetry is the whole point: the Lua `GetComboPoints 0x51a190`
    // reads this same byte behind `cmp al,4 / cmp al,0xb`, so a warrior's Overpower point greys
    // the button through this leg while lighting no combo dot (decision 0875). Anything the
    // *client* gates on reads the wire, never `GetComboPoints`.
    if d.needs_combo_points() && ctx.store.0.player_combo_points().unwrap_or(0) == 0 {
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
    // Leg 11: ONLY a cooldown-on-event spell folds its cooldown into usable (B3) — and the
    // predicate is the corrected `0x6e1690` (an on-hold-record test, wow-re `gcd-power-gate.md`
    // §3): Stealth greys while its record is PARKED; once the event starts the clocks — and for
    // every ordinary cooldown — the button never greys from here.
    if d.cooldown_on_event() && ctx.cooldowns.has_on_hold_record(spell_id, Some(d)) {
        return (false, false);
    }
    // Leg 12 (`0x6e3fba`–`0x6e3feb`): the power gate — the SOLE notEnoughMana writer (B2).
    if !can_afford(d, ctx.store) {
        return (false, true);
    }
    (true, false)
}

/// The RESOLVED power cost — `GetPowerCost 0x6e31b0`'s data terms: the flat `manaCost`, the
/// per-level term `(level − spellLevel) · manaCostPerlevel` (signed delta, the vmangos
/// `CalculatePowerCost` shape, clamped at 0 on the way out; every nonzero row is a creature
/// spell, so the clamp choice is dormant for players), and `ManaCostPercentage` of the pool
/// 0948 chose per type (base mana for mana spells — the vmangos basis — max pool otherwise,
/// health included for negative types). One law, every consumer: the usable walk's leg 12, the
/// press-path power gate (0948), and the tooltip's cost cell (1074) — mirroring the byte fn's
/// own caller set (`0x609657`/`0x60968d`/`0x4e5201`/`0x6e3fdb`/`0x52e8ad`/`0x507de3`). The
/// reference's per-school unit mods and spell-mods (talent cost cuts) are not modeled — 0948's
/// standing gap, now stated once here.
pub(crate) fn power_cost(d: &SpellDisplay, store: &ObjectStore) -> u32 {
    let power_type = d.power_type as i32;
    let base = if d.mana_cost_pct == 0 {
        0
    } else if d.power_type == 0 {
        store.0.unit_base_mana().unwrap_or(0)
    } else if power_type < 0 {
        store.0.unit_max_health().unwrap_or(0)
    } else {
        store.0.unit_max_power(power_type as u8).unwrap_or(0)
    };
    let level_delta = i64::from(store.0.unit_level().unwrap_or(0)) - i64::from(d.spell_level);
    let cost = i64::from(d.mana_cost)
        + level_delta * i64::from(d.mana_cost_per_level)
        + i64::from(base) * i64::from(d.mana_cost_pct) / 100;
    u32::try_from(cost).unwrap_or(0)
}

/// Whether the caster can afford `d`'s power cost — the shared availability-vs-cost compare
/// behind BOTH the usable walk's leg 12 (`0x6e3fba`) and the press-path power gate
/// (`0x6094f0` @ `0x60962c`, decision 0948): raw `UNIT_FIELD_POWER[type]` — ANY negative
/// PowerType reads `UNIT_FIELD_HEALTH` instead (the ref's `jl` at `0x609631`; Bloodrage's −2) —
/// signed-compared against [`power_cost`]'s number.
pub(crate) fn can_afford(d: &SpellDisplay, store: &ObjectStore) -> bool {
    let power_type = d.power_type as i32;
    let avail = if power_type < 0 {
        store.0.unit_health().unwrap_or(0)
    } else {
        store.0.unit_power(power_type as u8).unwrap_or(0)
    };
    avail >= power_cost(d, store)
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

        // Leg 5: a finishing move with no combo points banked (field 1222 byte 1). Overpower's
        // own shape — no aura state anywhere, so this leg is the only thing that greys it.
        let overpower = SpellDisplay {
            attributes_ex: 0x4810_0200,
            ..Default::default()
        };
        assert!(overpower.needs_combo_points());
        assert_eq!(walk(&overpower, &alive), (false, false));
        let dodged = player(&[(1222, 0x05_03_01_01)]);
        assert_eq!(walk(&overpower, &dodged), (true, false));
        // The neighbouring bytes of that dword must not read as combo points.
        let ranked = player(&[(1222, 0x05_03_00_01)]);
        assert_eq!(walk(&overpower, &ranked), (false, false));

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
            effects: [SPELL_EFFECT_TRADE_SKILL, 0, 0],
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
            };
            assert_eq!(
                spell_usable(5308, &execute, &spells, &ctx, &mut items, &commands),
                (expect, false)
            );
        }
    }
}
