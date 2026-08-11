//! Data-gated regression tests for [`super::load_spell_catalog`] — every column pin documented in
//! `spells/mod.rs`'s module doc, exercised end-to-end against the real build-5875 `Spell.dbc` (and,
//! for the tooltip-arc columns, the new `SpellCastTimes.dbc`/`SpellDuration.dbc` catalogs). Split
//! out of `mod.rs` purely for file size — this is still `crate::spells`'s own test suite, not a
//! separate concern. Every test skips (passes) without `<repo>/WoW/Data`.

use super::*;

/// The learn-spell hop (decision 0247): a class trainer offers a LEARN *wrapper* spell, not the
/// ability — the wire id is never in `SkillLineAbility`, so the tree must hop through the taught
/// spell to group it. Probed on real 5875 data: the warrior wrappers resolve to their abilities
/// (Heroic Strike 78 via 1605, Charge 100 via 1738, Rend 772 via 1423, Battle Shout 6673 via
/// 6674), the wrappers themselves carry no skill line, and the taught abilities do. This is the
/// exact failure that emptied the trainer tree until the hop landed. Skips without client data.
#[test]
fn real_learn_spell_hop_resolves_the_taught_ability() {
    let data = crate::wow_data_or_skip!();
    let mut chain = crate::open_chain(&data).expect("open chain");
    let spells = load_spell_catalog(&mut chain).expect("load Spell/SpellIcon");
    let skills = crate::skill_lines::load_skill_line_catalog(&mut chain).expect("load skill lines");

    for (wrapper, ability, line) in [(1605u32, 78u32, 26u32), (1738, 100, 26), (6674, 6673, 256)] {
        assert_eq!(
            spells.learned_spell(wrapper),
            Some(ability),
            "learn wrapper {wrapper} teaches ability {ability}"
        );
        assert_eq!(
            skills.spell_to_line(wrapper),
            None,
            "the wrapper {wrapper} is not itself in SkillLineAbility (the bug's root)"
        );
        assert_eq!(
            skills.spell_to_line(ability),
            Some(line),
            "the taught ability {ability} groups under skill line {line}"
        );
        assert!(
            spells.get(ability).is_some_and(|d| !d.name.is_empty()),
            "the taught ability carries the display name"
        );
    }
}

/// The attribute columns + the ranged gate on the real build-5875 `Spell.dbc` — a column slip
/// fails loudly. Values are the vmangos `spell_template` rows the module doc's pin used.
/// Skips without client data.
#[test]
fn real_spell_catalog_reads_ranged_attributes() {
    let data = crate::wow_data_or_skip!();
    let mut chain = crate::open_chain(&data).expect("open chain");
    let cat = load_spell_catalog(&mut chain).expect("load Spell/SpellIcon");

    // Auto Shot: SPELL_ATTR_RANGED (0x2, in 0x50012) + auto-repeat (0x20). No visual — the
    // missile is the wire ammo.
    let auto_shot = cat.get(75).expect("Auto Shot");
    assert_eq!(auto_shot.attributes, 0x50012);
    assert_eq!(auto_shot.attributes_ex2, 0x20);
    assert!(auto_shot.ranged_attack());
    assert_eq!(auto_shot.visual, 0, "the shot spells have no SpellVisual");
    assert_eq!(auto_shot.speed, 40.0);

    // Wand Shoot: the 0x18&0x2 side of the client gate, plus auto-repeat.
    let shoot = cat.get(5019).expect("Shoot (wand)");
    assert_eq!(shoot.attributes, 0x12);
    assert_eq!(shoot.attributes_ex2, 0x20);
    assert!(shoot.ranged_attack());

    // Throw: ranged-attribute but not auto-repeat — still arms the ranged stance.
    let throw = cat.get(2764).expect("Throw");
    assert_eq!(throw.attributes, 0x410012);
    assert_eq!(throw.attributes_ex2, 0);
    assert!(throw.ranged_attack());

    // Fireball: neither bit — a plain cast never arms ranged.
    let fireball = cat.get(133).expect("Fireball");
    assert_eq!(fireball.attributes, 0x10000);
    assert_eq!(fireball.attributes_ex2, 0);
    assert!(!fireball.ranged_attack());

    // Effect[0] (column 61): the auto-attack 6603 "Attack" carries SPELL_EFFECT_ATTACK (78) —
    // the client's own melee-substitution trigger (decision 0231); an ordinary spell doesn't.
    // A column slip on Effect[0] fails here.
    let attack = cat.get(6603).expect("Attack");
    assert_eq!(
        attack.effects[0], 78,
        "6603 Effect[0] == SPELL_EFFECT_ATTACK"
    );
    assert!(attack.is_melee_auto_attack());
    assert!(!fireball.is_melee_auto_attack());
    assert!(
        !auto_shot.is_melee_auto_attack(),
        "Auto Shot is ranged (Effect 58), not melee"
    );
}

/// The aura-bar display filter on the real build-5875 `Spell.dbc` (decisions 0268 + 0385): the
/// warrior stances carry `SPELL_ATTR_EX_NO_AURA_ICON` and the internal proc auras (Defensive
/// State 5301/5302) carry `SPELL_ATTR_DO_NOT_DISPLAY` (`0x80`) — the two bits the reference's
/// cache builder (`PlayerAuras_Update 0x4e4170`) refuses a slot for (its `Attributes` read is
/// byte-width) — while the everyday warrior buff (Battle Shout), an ordinary long buff
/// (Power Word: Fortitude), and the uncancelable world buff Echoes of Lordaeron (`Attributes`
/// dword sign bit, which is NOT a display filter) stay visible. The exact attribute values pin
/// columns 6/7 — a column slip fails loudly. Skips without client data.
#[test]
fn real_spell_catalog_hides_stances_from_the_aura_bar() {
    let data = crate::wow_data_or_skip!();
    let mut chain = crate::open_chain(&data).expect("open chain");
    let cat = load_spell_catalog(&mut chain).expect("load Spell/SpellIcon");

    // The stances: Battle carries NO_AURA_ICON | CAST_WHEN_LEARNED, the other two just
    // NO_AURA_ICON (extracted Spell.dbc, cross-checked against vmangos spell_template).
    let battle = cat.get(2457).expect("Battle Stance");
    assert_eq!(battle.attributes_ex, 0x9000_0000);
    assert!(battle.hidden_from_aura_bar());
    let defensive = cat.get(71).expect("Defensive Stance");
    assert_eq!(defensive.attributes_ex, 0x1000_0000);
    assert!(defensive.hidden_from_aura_bar());
    let berserker = cat.get(2458).expect("Berserker Stance");
    assert_eq!(berserker.attributes_ex, 0x1000_0000);
    assert!(berserker.hidden_from_aura_bar());

    // The internal proc auras: Defensive State 5302 rides a visible wire slot (not passive)
    // but carries `SPELL_ATTR_DO_NOT_DISPLAY`, so the reference never shows it (director's
    // report, 2026-07-14: it showed on our bar, sometimes with its timer).
    let def_state = cat.get(5302).expect("Defensive State");
    assert_eq!(def_state.attributes, 0x2000_0190);
    assert!(def_state.hidden_from_aura_bar());
    let def_state_dnd = cat.get(5301).expect("Defensive State (DND)");
    assert_eq!(def_state_dnd.attributes, 0x1d0);
    assert!(def_state_dnd.hidden_from_aura_bar());

    // The auras a warrior actually watches stay on the bar.
    let shout = cat.get(6673).expect("Battle Shout");
    assert!(!shout.hidden_from_aura_bar());
    let fortitude = cat.get(1243).expect("Power Word: Fortitude");
    assert!(!fortitude.hidden_from_aura_bar());

    // The dword sign bit (`SPELL_ATTR_NO_AURA_CANCEL`) is NOT a display filter — the cache
    // builder's `Attributes` read is byte-width (decision 0385). Echoes of Lordaeron is
    // uncancelable yet displays on the reference; the sign-bit transcription would hide it.
    let echoes = cat.get(1386).expect("Echoes of Lordaeron");
    assert_eq!(echoes.attributes, 0x8800_0100);
    assert!(!echoes.hidden_from_aura_bar());
}

/// `rank`/`passive` on the real build-5875 `Spell.dbc` — the module doc's own probe spells
/// (every rank of Fireball, plus Frost Armor/Corruption/Fire Blast) all carry their literal
/// "Rank N" subtext, and a representative passive (a weapon-skill spell, `SPELL_ATTR_PASSIVE`
/// module doc) reads `passive == true` while an ordinary active spell reads `false`. Skips
/// without client data.
#[test]
fn real_spell_catalog_reads_rank_and_passive() {
    let data = crate::wow_data_or_skip!();
    let mut chain = crate::open_chain(&data).expect("open chain");
    let cat = load_spell_catalog(&mut chain).expect("load Spell/SpellIcon");

    // Fireball's first three ranks — even rank 1 carries the literal "Rank 1" (module doc).
    assert_eq!(cat.get(133).unwrap().rank.as_deref(), Some("Rank 1"));
    assert_eq!(cat.get(143).unwrap().rank.as_deref(), Some("Rank 2"));
    assert_eq!(cat.get(145).unwrap().rank.as_deref(), Some("Rank 3"));
    assert_eq!(cat.get(168).unwrap().rank.as_deref(), Some("Rank 1")); // Frost Armor
    assert_eq!(cat.get(172).unwrap().rank.as_deref(), Some("Rank 1")); // Corruption
    assert_eq!(cat.get(2136).unwrap().rank.as_deref(), Some("Rank 1")); // Fire Blast

    // None of the above are passive; a weapon-skill spell (One-Handed Swords, id 201 —
    // `SPELL_ATTR_PASSIVE`'s own probe set) is.
    assert!(!cat.get(133).unwrap().passive);
    assert!(
        cat.get(201).unwrap().passive,
        "One-Handed Swords is passive"
    );
}

/// The spellbook add-gate on the real build-5875 `Spell.dbc` (decision 0227; the wow-re §5's
/// own concrete probe spells): displayable player spells pass, and the three hidden classes —
/// a language, an armor proficiency, a weapon proficiency (all `Attributes 0xC0`) — fail. A
/// column slip on castUI (3) or the gate bits fails loudly. Skips without client data.
#[test]
fn real_spell_catalog_gates_the_spellbook() {
    let data = crate::wow_data_or_skip!();
    let mut chain = crate::open_chain(&data).expect("open chain");
    let cat = load_spell_catalog(&mut chain).expect("load Spell/SpellIcon");

    // Shown: Fireball's ranks, Frostbolt, Polymorph — ordinary cast spells (bit 0x80 clear,
    // castUI 0).
    for id in [133, 143, 145, 116, 118] {
        let d = cat.get(id).unwrap_or_else(|| panic!("spell {id}"));
        assert!(
            d.in_spellbook(),
            "spell {id} should show ({:#x})",
            d.attributes
        );
        assert_eq!(d.attributes & 0x80, 0, "spell {id} is not DO_NOT_DISPLAY");
        assert_eq!(d.cast_ui, 0, "an ordinary spell reads castUI 0");
    }

    // Hidden: a language, cloth/leather armor proficiency, a weapon proficiency — each
    // `0xC0 = PASSIVE | DO_NOT_DISPLAY`, so `in_spellbook()` is false.
    for (id, what) in [
        (668u32, "Language: Common"),
        (9078, "Cloth"),
        (9077, "Leather"),
        (196, "One-Handed Axes"),
    ] {
        let d = cat.get(id).unwrap_or_else(|| panic!("spell {id} {what}"));
        assert_eq!(
            d.attributes & 0xC0,
            0xC0,
            "{what} ({id}) is PASSIVE|DO_NOT_DISPLAY"
        );
        assert!(!d.in_spellbook(), "{what} ({id}) is hidden from the book");
    }
}

/// `open_lock_type` on the real Spell.dbc — the OPEN_LOCK effect (col 61 == 0x21) and its
/// `LockType` (EffectMiscValue, col 106). Cross-verifies with `Lock.dbc`: a Copper Vein's skill
/// slot names LockType index 3, and spell 2575 "Mining" opens exactly that. A column slip on
/// either 61 or 106 breaks the match. Skips without client data.
#[test]
fn real_spell_catalog_reads_open_lock_types() {
    let data = crate::wow_data_or_skip!();
    let mut chain = crate::open_chain(&data).expect("open chain");
    let cat = load_spell_catalog(&mut chain).expect("load Spell/SpellIcon");

    // The gathering/lockpick openers carry SPELL_EFFECT_OPEN_LOCK; `open_lock_type` is the
    // LockType they open — the same indices Lock.dbc's skill slots name (mining vein → 3).
    assert_eq!(
        cat.get(2575).unwrap().open_lock_type(),
        Some(3),
        "Mining opens LockType 3"
    );
    assert_eq!(
        cat.get(2366).unwrap().open_lock_type(),
        Some(2),
        "Herb Gathering opens LockType 2"
    );
    assert_eq!(
        cat.get(1804).unwrap().open_lock_type(),
        Some(1),
        "Pick Lock opens LockType 1"
    );
    // A plain damage spell opens no lock (Effect[0] is not OPEN_LOCK).
    assert_eq!(
        cat.get(133).unwrap().open_lock_type(),
        None,
        "Fireball opens no lock"
    );

    // The totem (tool) and reagent columns the pre-send possession check reads (decision 0552;
    // the ref's `0x6e4000` at SpellRec+0xA0/+0xA8 = cols 40-41 / 42-49+50-57). A column slip
    // here silently breaks "Requires Mining Pick" / "Missing reagent: …".
    assert_eq!(
        cat.get(2575).unwrap().totems,
        [2901, 0],
        "Mining requires the Mining Pick (2901)"
    );
    assert_eq!(
        cat.get(8613).unwrap().totems,
        [7005, 0],
        "Skinning requires the Skinning Knife (7005)"
    );
    let slow_fall = cat.get(130).unwrap();
    assert_eq!(
        slow_fall.reagents[0],
        (17056, 1),
        "Slow Fall consumes one Light Feather (17056)"
    );
    assert_eq!(cat.get(133).unwrap().totems, [0, 0]);
}

/// The two `Effect[0]` values the client latches at spell-learn time (`0x4b25e0` → `[0xb700e4]` /
/// `[0xb700e8]`, decision 0752), pinned against the shipped file: the cursor's skin leg refuses to
/// show the knife unless one of them is present in the book, so a wrong constant would silently
/// re-open the "everyone sees the skinning cursor" report — or, worse, hide it from skinners.
/// Skips without client data.
#[test]
fn real_spell_catalog_pins_the_skin_latch_effects() {
    let data = crate::wow_data_or_skip!();
    let mut chain = crate::open_chain(&data).expect("open chain");
    let cat = load_spell_catalog(&mut chain).expect("load Spell/SpellIcon");

    // Skinning (8613) — `0x4b2623: cmp [esi+0xf4], 0x5f`.
    assert_eq!(
        cat.get(8613).unwrap().effects[0],
        crate::SPELL_EFFECT_SKINNING,
        "Skinning carries SPELL_EFFECT_SKINNING (95 == 0x5f)"
    );
    // Remove Insignia (22027) — the `[0xb700e8]` half, `0x4b2632: cmp [esi+0xf4], 0x74`.
    assert_eq!(
        cat.get(22027).unwrap().effects[0],
        0x74,
        "Remove Insignia carries SPELL_EFFECT_SKIN_PLAYER_CORPSE (116 == 0x74)"
    );
    // Nothing an ordinary caster starts with does — the latch stays empty for a non-skinner.
    assert_ne!(
        cat.get(133).unwrap().effects[0],
        crate::SPELL_EFFECT_SKINNING
    );
    assert_ne!(
        cat.get(6247).unwrap().effects[0],
        crate::SPELL_EFFECT_SKINNING
    );
}

/// The **skill an opener provides** on the real Spell.dbc — the left-hand side of the client's lock
/// satisfaction test (`0x5f850f`; decision 0752). This walk decides whether a right-click opens a
/// lock at all, so its inputs (spellLevel 28 · EffectDieSides 64 · EffectBaseDice 67 ·
/// EffectDicePerLevel 70 · EffectRealPointsPerLevel 73 · EffectBasePoints 76) are pinned by
/// *result*, against anchors whose right answers are known from the game rather than the file.
/// Skips without client data.
#[test]
fn real_spell_catalog_computes_the_lock_skill_an_opener_provides() {
    let data = crate::wow_data_or_skip!();
    let mut chain = crate::open_chain(&data).expect("open chain");
    let cat = load_spell_catalog(&mut chain).expect("load Spell/SpellIcon");

    // Pick Lock (1804): `4 + 1 + 5.0×(level − 1)` — a rogue's Lockpicking cap, 5×level, exactly.
    assert_eq!(cat.get(1804).unwrap().open_lock_skill(60), Some(300));
    assert_eq!(cat.get(1804).unwrap().open_lock_skill(45), Some(225));
    // Mining (2575) / Herb Gathering (2366): `−1 + 1 + 5.0×level` — the same profession cap, but
    // quoted at spellLevel 0, so they do not lose the first level the way Pick Lock does.
    assert_eq!(cat.get(2575).unwrap().open_lock_skill(60), Some(300));
    assert_eq!(cat.get(2366).unwrap().open_lock_skill(60), Some(300));
    // Small / Large Seaforium Charge (4056 / 4075): flat `149 + 1` = 150 and `249 + 1` = 250 — and
    // lock 92 asks for `Blasting 150`. The charge exists to open exactly that door, so the equality
    // is the cross-check: a column slip anywhere in the walk breaks it.
    assert_eq!(cat.get(4056).unwrap().open_lock_skill(1), Some(150));
    assert_eq!(cat.get(4075).unwrap().open_lock_skill(1), Some(250));
    // The universally-known "Opening"/"Closing" family is flat 100 at every level — which is why
    // the Action gate, not the value test, is what keeps them off a padlocked door (decision 0752).
    for id in [3365, 6233, 6246, 6247, 6477, 6478, 21651, 21652] {
        assert_eq!(
            cat.get(id).unwrap().open_lock_skill(1),
            Some(100),
            "spell {id} is a flat-100 opener"
        );
        assert_eq!(cat.get(id).unwrap().open_lock_skill(60), Some(100));
    }
    // A spell with no OPEN_LOCK effect provides nothing.
    assert_eq!(cat.get(133).unwrap().open_lock_skill(60), None);
}

/// The cooldown/cost/range columns on the real build-5875 `Spell.dbc`, pinned 2026-07-10
/// against the vmangos `spell_template` rows (MAX(build) ≤ 5875 per entry — the module's
/// established cross-check). A slip on any of columns 2/19/20/31/32/36/156/157/158 fails
/// loudly. Skips without client data.
#[test]
fn real_spell_catalog_reads_cooldown_cost_and_range_columns() {
    let data = crate::wow_data_or_skip!();
    let mut chain = crate::open_chain(&data).expect("open chain");
    let cat = load_spell_catalog(&mut chain).expect("load Spell/SpellIcon");

    // Fireball r1: no cooldown, the ordinary GCD (133/1500), 30 mana, 35yd range row.
    let fireball = cat.get(133).unwrap();
    assert_eq!(
        (
            fireball.category,
            fireball.recovery_ms,
            fireball.category_recovery_ms
        ),
        (0, 0, 0)
    );
    assert_eq!(
        (fireball.start_recovery_category, fireball.start_recovery_ms),
        (133, 1500)
    );
    assert_eq!((fireball.power_type, fireball.mana_cost), (0, 30));
    assert_eq!(fireball.range_index, 35);
    assert!(!fireball.cooldown_on_event());

    // Charge: category 44 with a 15 s category cooldown, rage (1), NO GCD pair, range row 95.
    let charge = cat.get(100).unwrap();
    assert_eq!(
        (
            charge.category,
            charge.recovery_ms,
            charge.category_recovery_ms
        ),
        (44, 0, 15000)
    );
    assert_eq!(
        (charge.start_recovery_category, charge.start_recovery_ms),
        (0, 0)
    );
    assert_eq!(charge.power_type, 1, "Charge costs rage");
    assert_eq!(charge.range_index, 95);

    // Feign Death: a 30 s own-spell RecoveryTime and SPELL_ATTR_COOLDOWN_ON_EVENT (bit 25 of
    // attributes 0x2151400 — the on-hold family).
    let feign = cat.get(5384).unwrap();
    assert_eq!(feign.recovery_ms, 30_000);
    assert!(
        feign.cooldown_on_event(),
        "Feign Death is cooldown-on-event"
    );

    // Lay on Hands: the hour-long category cooldown (56 / 3_600_000).
    let loh = cat.get(633).unwrap();
    assert_eq!((loh.category, loh.category_recovery_ms), (56, 3_600_000));

    // ManaCostPercentage's own nonzero probe rows (the flat sample was all-zero): 370
    // Purge r1 = 10, 527 Dispel Magic r1 = 18.
    assert_eq!(cat.get(370).unwrap().mana_cost_pct, 10);
    assert_eq!(cat.get(527).unwrap().mana_cost_pct, 18);
}

/// The cast-arm targeting columns ([`COL_TARGETS`] 13 / [`COL_IMPLICIT_TARGET_A1`] 82) on the
/// real build-5875 `Spell.dbc` — each row chosen to pin a distinct switch arm or `Targets`
/// bit (values cross-checked against the `0x6e5250` arm map, wow-re `wave-cast.md`). Skips
/// without client data.
#[test]
fn real_spell_catalog_reads_cast_targeting_columns() {
    let data = crate::wow_data_or_skip!();
    let mut chain = crate::open_chain(&data).expect("open chain");
    let cat = load_spell_catalog(&mut chain).expect("load Spell/SpellIcon");

    // The implicit-target enum: 6 = single enemy (hostile bit), 1 = self, 21 = single
    // friend (assist bit), 20 = party-around-caster (a no-op arm → mask stays 0).
    assert_eq!(cat.get(133).unwrap().implicit_target_a1, 6, "Fireball");
    assert_eq!(cat.get(7302).unwrap().implicit_target_a1, 1, "Ice Armor");
    assert_eq!(cat.get(5384).unwrap().implicit_target_a1, 1, "Feign Death");
    assert_eq!(
        cat.get(1459).unwrap().implicit_target_a1,
        21,
        "Arcane Intellect"
    );
    assert_eq!(
        cat.get(6673).unwrap().implicit_target_a1,
        20,
        "Battle Shout"
    );

    // The `Targets` seed mask: 0 for ordinary casts; Resurrection carries the corpse-ally
    // bit 15, Skinning unit bit 1 + the requires-explicit-selection gate bit 10.
    assert_eq!(cat.get(133).unwrap().targets, 0);
    assert_eq!(cat.get(6673).unwrap().targets, 0);
    assert_eq!(cat.get(2006).unwrap().targets, 0x8000, "Resurrection");
    assert_eq!(cat.get(8613).unwrap().targets, 0x402, "Skinning");
}

/// The usable-walk columns (§2a) on the real build-5875 data — one pinned row per gate
/// family — plus the form-gate law over the real form flags. Skips without client data.
#[test]
fn real_spell_catalog_reads_usable_walk_columns() {
    let data = crate::wow_data_or_skip!();
    let mut chain = crate::open_chain(&data).expect("open chain");
    let cat = load_spell_catalog(&mut chain).expect("load Spell/SpellIcon");
    let forms = load_shapeshift_forms(&mut chain).expect("load SpellShapeshiftForm");

    // Claw: cat form (1) required. Ambush: stealth form (30) + a dagger equipped + the
    // only-stealthed attribute. Execute: battle/berserker stances + a melee weapon +
    // the target's healthless-20% aura state. Revenge: the caster's defense state.
    let claw = cat.get(1082).unwrap();
    assert_eq!(claw.stances, 0x1);
    let ambush = cat.get(8676).unwrap();
    assert_eq!(ambush.stances, 0x2000_0000);
    assert_ne!(ambush.attributes & ATTR_ONLY_STEALTHED, 0);
    assert_eq!(
        (
            ambush.equipped_item_class,
            ambush.equipped_item_subclass_mask
        ),
        (2, 0x8000)
    );
    let execute = cat.get(5308).unwrap();
    assert_eq!((execute.stances, execute.target_aura_state), (0x50000, 2));
    assert_eq!(cat.get(6572).unwrap().caster_aura_state, 1, "Revenge");

    // Leg 5, the combo-point gate (0869): Overpower carries NO aura state — its window rides
    // `AttributesEx` b20 (`FINISHING_MOVE_DAMAGE`) exactly like the rogue/druid finishers, which
    // is why the aura-state legs alone left it permanently lit. Every rank, the finishers with
    // it, and the neighbouring warrior abilities as the control.
    for rank in [7384, 7887, 11584, 11585] {
        let op = cat.get(rank).unwrap();
        assert!(op.needs_combo_points(), "Overpower {rank}");
        assert_eq!(
            (op.caster_aura_state, op.target_aura_state),
            (0, 0),
            "Overpower {rank} has no aura-state gate — leg 5 is all that holds it"
        );
    }
    for finisher in [
        2098, /* Eviscerate */
        1943, /* Rupture */
        5171, /* Slice and Dice */
    ] {
        assert!(
            cat.get(finisher).unwrap().needs_combo_points(),
            "{finisher}"
        );
    }
    for plain in [
        78,   /* Heroic Strike */
        5308, /* Execute */
        6572, /* Revenge */
    ] {
        assert!(!cat.get(plain).unwrap().needs_combo_points(), "{plain}");
    }

    // Auto Shot: bows/guns/crossbows. Slow Fall: one Light Feather.
    let auto_shot = cat.get(75).unwrap();
    assert_eq!(
        (
            auto_shot.equipped_item_class,
            auto_shot.equipped_item_subclass_mask
        ),
        (2, 0x4000c)
    );
    assert_eq!(cat.get(130).unwrap().reagents[0], (17056, 1), "Slow Fall");

    // The form flags: warrior Battle Stance (17) is a *stance* (flags1 bit 0), druid Cat
    // Form (1) is a true shapeshift — the actAsShifted fork's data.
    assert!(forms.get(&17).unwrap().is_stance());
    assert!(!forms.get(&1).unwrap().is_stance());
    // The bonus-bar column still reads through the richer row (Cat → page 1).
    assert_eq!(forms.get(&1).unwrap().bonus_bar, 1);

    // The form-gate law on the real rows: Claw usable in cat, not unshifted; Fireball
    // usable unshifted AND in Battle Stance (a stance), not in Cat Form (a shapeshift);
    // Execute usable in Battle (17), not in Defensive (18).
    assert!(claw.usable_in_form(1, false));
    assert!(!claw.usable_in_form(0, false));
    let fireball = cat.get(133).unwrap();
    assert!(fireball.usable_in_form(0, false));
    assert!(fireball.usable_in_form(17, true));
    assert!(!fireball.usable_in_form(1, false));
    assert!(execute.usable_in_form(17, true));
    assert!(!execute.usable_in_form(18, true));

    // Ghost Wolf on the real rows (the shaman lockout, verified 2026-07-31): form 16 is a true
    // shapeshift and cancelable; the spell carries NOT_SHAPESHIFT (bit 16) AND the stance-bar
    // exclusion (ex2 0x2 — the shipped carrier: a shaman gets no stance bar). In the form,
    // an ordinary spell refuses 0x3d; a form-requiring spell out of its form refuses 0x56.
    let ghost_wolf = cat.get(2645).unwrap();
    assert_eq!(ghost_wolf.shapeshift_form, Some(16));
    assert_ne!(ghost_wolf.attributes & 0x1_0000, 0);
    assert_ne!(ghost_wolf.attributes_ex2 & 0x2, 0);
    let wolf_row = forms.get(&16).unwrap();
    assert!(!wolf_row.is_stance());
    assert!(wolf_row.cancelable());
    use crate::FormRefusal;
    let bolt = cat.get(403).unwrap();
    assert_eq!(
        bolt.form_refusal(16, false),
        Some(FormRefusal::NotShapeshift)
    );
    assert_eq!(bolt.form_refusal(0, false), None);
    assert_eq!(
        claw.form_refusal(0, false),
        Some(FormRefusal::OnlyShapeshift)
    );
    assert_eq!(
        ghost_wolf.form_refusal(16, false),
        Some(FormRefusal::NotShapeshift),
        "re-pressing Ghost Wolf in the form draws the gate too (the cancel is a separate branch)"
    );
    assert_eq!(FormRefusal::NotShapeshift.reason(), 0x3d);
    assert_eq!(FormRefusal::OnlyShapeshift.reason(), 0x56);

    // The active-action toggle's raw-column gate (wow-re shapeshift-plaincast-toggle.md):
    // Ghost Wolf carries a nonzero ActiveIconID, so its button press-again cancels; Battle
    // Stance carries 0, which is what keeps a stance un-cancelable on the plain paths.
    assert_ne!(ghost_wolf.active_icon_id, 0);
    assert_eq!(cat.get(2457).unwrap().active_icon_id, 0, "Battle Stance");

    // The form's AttackIconID column (field 13, `0x4e6870`'s `+0x34` read — wow-re
    // `action-spell-icon-apis.md` §3.3), resolved through SpellIcon.dbc at load: Cat Form
    // carries its own attack face, Ghost Wolf's column is 0 → the weapon fall-through.
    assert_eq!(
        forms.get(&1).unwrap().attack_icon.as_deref(),
        Some("Interface\\Icons\\Ability_Druid_CatFormAttack")
    );
    assert_eq!(wolf_row.attack_icon, None);
}

/// The tooltip-arc columns (decision 0274 P2) on the real build-5875 `Spell.dbc`, pinned
/// 2026-07-10: description/aura-description text, DurationIndex/CastingTimeIndex/ProcChance,
/// and the per-effect arrays — end-to-end through the new [`load_spell_cast_times`]/
/// [`load_spell_durations`] catalogs. Skips without client data.
#[test]
fn real_spell_catalog_reads_tooltip_columns() {
    let data = crate::wow_data_or_skip!();
    let mut chain = crate::open_chain(&data).expect("open chain");
    let cat = load_spell_catalog(&mut chain).expect("load Spell/SpellIcon");
    let cast_times = load_spell_cast_times(&mut chain).expect("load SpellCastTimes");
    let durations = load_spell_durations(&mut chain).expect("load SpellDuration");

    // Fireball r1: the description's own opening line, and its real DoT-tail duration — it is
    // NOT an "instant, no duration" spell (the description literally says "over $d": a 2 s
    // apply-aura tick, effect slot 1, running the full 4 s tail).
    let fireball = cat.get(133).unwrap();
    assert!(
        fireball
            .description
            .as_deref()
            .unwrap()
            .starts_with("Hurls a fiery ball that causes"),
        "Fireball description: {:?}",
        fireball.description
    );
    assert_eq!(fireball.casting_time_index, 16);
    assert_eq!(
        cast_times.get(16).unwrap().base_ms,
        1500,
        "Fireball's real cast time"
    );
    assert_eq!(fireball.duration_index, 35);
    assert_eq!(
        durations.get(35).unwrap().base_ms,
        4000,
        "Fireball's DoT-tail duration, not zero — it genuinely has one"
    );
    assert_eq!(
        fireball.proc_chance, 101,
        "vmangos's always-triggers sentinel"
    );
    assert_eq!(
        (fireball.effect_base_points[0], fireball.effect_die_sides[0]),
        (13, 9),
        "Fireball r1's direct-damage roll: 14-22"
    );
    assert_eq!(
        (fireball.effect_apply_aura[1], fireball.effect_amplitude[1]),
        (3, 2000),
        "Fireball's periodic-damage tail: SPELL_AURA_PERIODIC_DAMAGE ticking every 2s"
    );

    // Frost Armor: a real 30-minute buff, an instant cast, a nonempty short aura blurb, and its
    // own description names the exact chill-proc spell (6136) its EffectTriggerSpell[1] holds.
    let frost_armor = cat.get(168).unwrap();
    assert!(frost_armor
        .aura_description
        .as_deref()
        .is_some_and(|s| !s.is_empty()));
    assert_eq!(frost_armor.duration_index, 30);
    assert_eq!(durations.get(30).unwrap().base_ms, 1_800_000, "30 minutes");
    assert_eq!(frost_armor.casting_time_index, 1);
    assert_eq!(cast_times.get(1).unwrap().base_ms, 0, "instant");
    assert_eq!(
        frost_armor.effect_apply_aura[0], 22,
        "SPELL_AURA_MOD_RESISTANCE"
    );
    assert_eq!(
        frost_armor.effect_trigger_spell[1], 6136,
        "the chill proc the description text itself names"
    );

    // Fire Blast: a direct-damage spell with no aura component at all — empty aura text, no
    // duration row.
    let fire_blast = cat.get(2136).unwrap();
    assert_eq!(fire_blast.aura_description, None);
    assert_eq!(fire_blast.duration_index, 0);
    assert!(durations.get(0).is_none(), "no row 0 in SpellDuration.dbc");

    // Auto Shot / Feign Death: the signed EffectBasePoints sentinel (-1, weapon-damage/no
    // fixed roll) actually round-trips through i32 — a column slip to unsigned would read
    // 4294967295 here instead.
    assert_eq!(cat.get(75).unwrap().effect_base_points[0], -1, "Auto Shot");
    assert_eq!(
        cat.get(5384).unwrap().effect_base_points[0],
        -1,
        "Feign Death"
    );
}

/// The combat-initiation classes on the real build-5875 `Spell.dbc` — the two accessor masks
/// the cast seam's queue/attack-start logic keys on ([`SpellDisplay::on_next_swing`] `0x404`,
/// [`SpellDisplay::initiates_auto_attack`] adding `AttributesEx & 0x200`), pinned against the
/// vmangos `spell_template` rows read at decision time (2026-07-14). A column slip or a mask
/// slip fails loudly. Skips without client data.
#[test]
fn real_spell_catalog_classifies_combat_initiation() {
    let data = crate::wow_data_or_skip!();
    let mut chain = crate::open_chain(&data).expect("open chain");
    let cat = load_spell_catalog(&mut chain).expect("load Spell/SpellIcon");

    // (spell, on_next_swing, initiates_auto_attack)
    for (id, name, next_swing, initiates) in [
        (78u32, "Heroic Strike", true, true), // Attributes 0x50014
        (845, "Cleave", true, true),          // Attributes 0x50014, Ex 0x200
        (2973, "Raptor Strike", true, true),  // Attributes 0x50404
        (772, "Rend", false, true),           // Ex 0x8000200
        (7386, "Sunder Armor", false, true),  // Ex 0x8000200
        (1464, "Slam", false, true),          // Ex 0x8000200
        (100, "Charge", false, false),        // Ex 0x400 — neither bit
        (6673, "Battle Shout", false, false), // Ex 0x0
        (6603, "Attack", false, false),       // the auto-attack pseudo-spell itself
        (133, "Fireball", false, false),      // an ordinary cast
    ] {
        let d = cat
            .get(id)
            .unwrap_or_else(|| panic!("{name} ({id}) in the catalog"));
        assert_eq!(
            d.on_next_swing(),
            next_swing,
            "{name} ({id}) on_next_swing (Attributes {:#x})",
            d.attributes
        );
        assert_eq!(
            d.initiates_auto_attack(),
            initiates,
            "{name} ({id}) initiates_auto_attack (Attributes {:#x}, Ex {:#x})",
            d.attributes,
            d.attributes_ex
        );
        // The §5's one DBC-owed bit (wow-re `combat-feel-law.md` @ c445713b): `AttributesEx2 &
        // 0x100000` (INITIATE_COMBAT_POST_CAST) defers a spell's attack-start to SMSG_SPELL_GO —
        // a client path benilla leaves unbuilt because no spell here carries the bit. This pins
        // that from the real client DBC: in particular Charge (100) is bit20-CLEAR, so vanilla
        // Charge starts no auto-attack through ANY client channel.
        assert_eq!(
            d.attributes_ex2 & 0x0010_0000,
            0,
            "{name} ({id}) must not carry INITIATE_COMBAT_POST_CAST (Ex2 {:#x}) — the deferred \
             GO-time attack-start is unbuilt",
            d.attributes_ex2
        );
    }
}

/// The crafting columns (decision 0437) on the real build-5875 `Spell.dbc`: `EffectItemType`
/// (103-105) and `RequiresSpellFocus` (15), cross-checked against the live vmangos
/// `spell_template` rows queried at pin time (2963 → creates 2996, 2738 → 2845, 3920 → 8067 with
/// BasePoints[0]=199; 2538 Charred Wolf Meat → focus 4 Cooking Fire; 2738 Copper Axe → focus 1 Anvil).
/// A column slip fails loudly. Skips without client data.
#[test]
fn real_crafting_columns_read_created_item_and_focus() {
    let data = crate::wow_data_or_skip!();
    let mut chain = crate::open_chain(&data).expect("open chain");
    let cat = load_spell_catalog(&mut chain).expect("load Spell/SpellIcon");

    // (recipe spell, created item, focus): Bolt of Linen Cloth, Minor Healing Potion,
    // Copper Axe, Crafted Light Shot, Charred Wolf Meat.
    for (spell, item, focus) in [
        (2963u32, 2996u32, 0u32),
        (2330, 118, 0),
        (2738, 2845, 1), // Blacksmithing needs the Anvil (focus 1)
        (3920, 8067, 0),
        (2538, 2679, 4),
    ] {
        let d = cat.get(spell).expect("recipe in the catalog");
        assert_eq!(
            d.effects[0], SPELL_EFFECT_CREATE_ITEM,
            "spell {spell} creates"
        );
        assert_eq!(d.effect_item_type[0], item, "spell {spell} created item");
        assert_eq!(d.requires_spell_focus, focus, "spell {spell} focus");
    }

    // Crafted Light Shot's 200-per-craft: BasePoints[0]=199, DieSides[0]=1 → made = 199+1.
    let shots = cat.get(3920).expect("Crafted Light Shot");
    assert_eq!(shots.effect_base_points[0], 199);
    assert_eq!(shots.effect_die_sides[0], 1);

    // The openers carry effect 47 and no product: Tailoring 3908, Enchanting 7411.
    for opener in [3908u32, 7411] {
        let d = cat.get(opener).expect("opener in the catalog");
        assert_eq!(d.effects[0], SPELL_EFFECT_TRADE_SKILL, "opener {opener}");
        assert_eq!(
            d.effect_item_type, [0; 3],
            "opener {opener} creates nothing"
        );
    }
}

/// The two tooltip-law reads added for the 2026-07-25 spellbook reports (decision 0620), on pure
/// data — no client install needed.
#[test]
fn the_tooltip_gates_read_effect_and_mask() {
    // §3.4: the cast|cooldown line goes on the ATTRIBUTE bit or on Effect[0] ∈ {47, 78}.
    let plain = SpellDisplay::default();
    assert!(!plain.tooltip_omits_cast_line());
    let attribute_passive = SpellDisplay {
        passive: true,
        ..Default::default()
    };
    assert!(attribute_passive.tooltip_omits_cast_line());
    // 6603 "Attack"'s shape: Effect[0] = 78, attributes 0x10 (NOT the passive bit).
    let auto_attack = SpellDisplay {
        effects: [78, 0, 0],
        attributes: 0x10,
        ..Default::default()
    };
    assert!(!auto_attack.passive, "the attribute bit is clear");
    assert!(auto_attack.tooltip_omits_cast_line(), "the Effect[0] leg");
    let trade_skill = SpellDisplay {
        effects: [47, 0, 0],
        ..Default::default()
    };
    assert!(trade_skill.tooltip_omits_cast_line());

    // §3-EQUIPITEM's naming rule moved to `ItemSubClassCatalog::requirement_name`, where the
    // vocabulary it reads lives — see `itemsubclass::tests` for its coverage against the real DBCs.
}

/// The item-target family and its gate columns (decision 0923), against the real 5875 file. The
/// reference's `TargetingWantsItem 0x6e6330` is `flag_word & 0x4010`, and on shipped data those
/// two bits are **never** mixed with a unit bit — the whole family is `Targets` exactly `0x10`
/// (the enchant/poison/stone/scope arm, this slice) or exactly `0x4000` (the OPEN_LOCK arm, whose
/// lock machinery is a separate one). That disjointness is what lets the resolver fork on the
/// bare word instead of running the reference's bind walk to exhaustion.
///
/// The gate columns are the three `0x495d60` reads, pinned on rows whose answer is checkable by
/// eye: an armor enchant names its `InventoryType` slot (bracer → 9, chest → 5 | robe 20) while a
/// weapon enchant/poison names a class+subclass instead and leaves the type mask 0.
#[test]
fn real_item_target_family_and_its_gate_columns() {
    let data = crate::wow_data_or_skip!();
    let mut chain = crate::open_chain(&data).expect("open chain");
    let cat = load_spell_catalog(&mut chain).expect("load Spell/SpellIcon");

    // The unit-shaped bits of the flag_word (`cast_target`'s `UNIT_BITS`).
    const UNIT_BITS: u32 = 0x0002 | 0x0004 | 0x0008 | 0x0080 | 0x0100 | 0x0200 | 0x0400 | 0x8000;
    let mut item_only = 0usize;
    let mut locked_only = 0usize;
    for (id, d) in cat.iter() {
        if d.targets & 0x4010 == 0 {
            continue;
        }
        assert_eq!(
            d.targets & UNIT_BITS,
            0,
            "spell {id} mixes an item bit with a unit bit — the resolver's fork would be wrong"
        );
        match d.targets {
            0x0010 => item_only += 1,
            0x4000 => locked_only += 1,
            other => panic!("spell {id}: unexpected item-family word {other:#x}"),
        }
    }
    assert_eq!(
        item_only, 363,
        "Targets == 0x10 — the enchant/poison family"
    );
    assert_eq!(locked_only, 103, "Targets == 0x4000 — the OPEN_LOCK family");

    // The OPEN_LOCK family's **implicit arm**, which is what turns its bare `0x4000` into the word
    // both click seams read (decision 0939). The app's `cast_target_mask` ORs `0x800` for arm 23
    // and `TF_UNIT` for arm 25, so this census is the data behind "a lock word is `0x4000` or
    // `0x4800`, and either way `& 0x4010` and `& 0x4800` are *both* nonzero" — the overlap that
    // lets one armed cursor answer the bag click and the world click. Pick Lock 1804 is one of the
    // 100 (live-probed: it arms `0x4000` and both seams take it). If this distribution moves, the
    // seam that stops being reachable fails here first.
    let mut arms = std::collections::BTreeMap::<u32, usize>::new();
    for (_, d) in cat.iter().filter(|(_, d)| d.targets == 0x4000) {
        *arms.entry(d.implicit_target_a1).or_default() += 1;
    }
    assert_eq!(
        arms.values().sum::<usize>(),
        103,
        "every OPEN_LOCK row is counted"
    );
    assert_eq!(
        arms.get(&25),
        Some(&1),
        "one row arms 25 (its overlay is TF_UNIT, not the GameObject bit)"
    );
    assert!(
        arms.get(&23).copied().unwrap_or(0) >= 100,
        "the family is overwhelmingly arm 23, whose overlay is TARGET_FLAG_GAMEOBJECT: {arms:?}"
    );
    // And no row in the whole file reaches the GameObject seam by `Targets` alone — bit 11 is
    // something the implicit arm puts on the word, never a column value.
    assert_eq!(
        cat.iter().filter(|(_, d)| d.targets & 0x800 != 0).count(),
        0,
        "TARGET_FLAG_GAMEOBJECT never appears in the Targets column"
    );

    // Bracer enchant: armor (4), any subclass, InventoryType WRIST(9) only.
    let bracer = cat.get(7418).unwrap();
    assert_eq!(bracer.targets, 0x10);
    assert_eq!(bracer.equipped_item_class, 4);
    assert_eq!(bracer.equipped_item_inventory_type_mask, 1 << 9);
    // Chest enchant: CHEST(5) or ROBE(20) — the mask that makes a cloth robe legal.
    let chest = cat.get(7443).unwrap();
    assert_eq!(
        chest.equipped_item_inventory_type_mask,
        (1 << 5) | (1 << 20)
    );
    // A weapon-side row gates on class+subclass and leaves the type mask alone.
    let poison = cat.get(8679).unwrap();
    assert_eq!(
        (
            poison.equipped_item_class,
            poison.equipped_item_subclass_mask,
            poison.equipped_item_inventory_type_mask
        ),
        (2, 0x2a5f3, 0)
    );

    // The reference's gate walks all THREE effect slots looking for an enchant effect
    // (`0x495d60`'s loop, `495de4`–`496050`); [`SpellDisplay`] carries only slot 0. That is
    // byte-equivalent on shipped data and this is why: across the whole item-target family, not
    // one row puts its enchant effect anywhere but slot 0. Read raw, since the catalog itself
    // only keeps `effects[0]`.
    let raw = chain.read_file(SPELL).expect("Spell.dbc");
    let set = parse(&raw, spell_schema(), "Spell.dbc").expect("parse Spell.dbc");
    let is_enchant = |e: u32| {
        e == crate::SPELL_EFFECT_ENCHANT_ITEM || e == crate::SPELL_EFFECT_ENCHANT_ITEM_TEMPORARY
    };
    for r in set.records() {
        if u32_at(r, COL_TARGETS).unwrap_or(0) != 0x10 {
            continue;
        }
        let effects: Vec<u32> = (0..3)
            .map(|i| u32_at(r, COL_EFFECT_1 + i).unwrap_or(0))
            .collect();
        assert!(
            is_enchant(effects[0]) || !(is_enchant(effects[1]) || is_enchant(effects[2])),
            "spell {} hides its enchant effect outside slot 0 ({effects:?}) — the one-slot read \
             in `ui_action::targeting::item_target_refusal` would miss it",
            u32_at(r, 0).unwrap_or(0)
        );
    }
}

/// The 0948 §5's flagged data questions, pinned on the real 5875 data (`gcd-power-gate.md`
/// §2.1): the SpellCategory flags-bit-0x2 wildcard set is EXACTLY {351} — wand Shoot's category
/// (the whole-bar swing sweep the store's wildcard leg implements) — and the `{cat=0, time≠0}`
/// GCD-source shape (which would arm a category-0 GCD node matching every category-0 press) has
/// NO player-castable carrier: every such row is an NPC/internal spell. A data change here means
/// the store's predicates need re-judging, loudly.
#[test]
fn gcd_wildcard_and_shape_corners_hold_on_the_real_data() {
    let data = crate::wow_data_or_skip!();
    let mut chain = crate::open_chain(&data).expect("open chain");
    let cat = load_spell_catalog(&mut chain).expect("catalog");

    // The wildcard set: exactly wand Shoot's 351, resolved onto the display at load.
    let shoot = cat.get(5019).expect("wand Shoot");
    assert_eq!(shoot.category, 351);
    assert!(
        shoot.category_wildcard,
        "Shoot's category row carries flags&2"
    );
    for &(id, name) in &[
        (133u32, "Fireball"),
        (100, "Charge"),
        (6673, "Battle Shout"),
    ] {
        let d = cat.get(id).expect(name);
        assert!(
            !d.category_wildcard,
            "{name}'s category must not read wildcard"
        );
    }

    // Scroll of Armor's spell: the {cat≠0, time=0} shape the corrected refusal predicate now
    // locks during a GCD (the pressed spell's own time is never consulted).
    let scroll = cat.get(8091).expect("Scroll of Armor's spell");
    assert_eq!(
        (scroll.start_recovery_category, scroll.start_recovery_ms),
        (133, 0)
    );
}

/// The cost columns on the REAL 5875 data (decision 1074, B192): the health power type
/// (−2 as `0xFFFFFFFE`), Bloodrage's pct-only shape, Health Funnel's `manaPerSecond`, the cast
/// cell's attr rows, and the verified NEGATIVE that keeps columns 33/35 unparsed. Skips without
/// client data.
#[test]
fn real_spell_catalog_cost_columns() {
    let data = crate::wow_data_or_skip!();
    let mut chain = crate::open_chain(&data).expect("open chain");
    let cat = crate::load_spell_catalog(&mut chain).expect("Spell.dbc");

    // Life Tap, EVERY rank: health type and NO cost columns at all — the client's own file,
    // against the folk memory of a printed health cost (that is 2.x's change; 1.12 carries the
    // trade in the description text). The cost cell stays EMPTY, which is the reference render.
    for id in [1454u32, 1455, 1456, 11687, 11688, 11689] {
        let d = cat.get(id).unwrap();
        assert_eq!(
            (
                d.power_type,
                d.mana_cost,
                d.mana_cost_pct,
                d.mana_per_second
            ),
            (0xFFFF_FFFE, 0, 0, 0),
            "Life Tap {id}"
        );
    }
    // Bloodrage: pct-ONLY health cost — the resolved-cost law's health-pool lane (B192).
    let bloodrage = cat.get(2687).unwrap();
    assert_eq!(
        (
            bloodrage.power_type,
            bloodrage.mana_cost,
            bloodrage.mana_cost_pct
        ),
        (0xFFFF_FFFE, 0, 20)
    );
    // Health Funnel: flat health + per-second — the `_PER_TIME` composite's live customer — and
    // channeled, so its cast cell reads "Channeled".
    let funnel = cat.get(755).unwrap();
    assert_eq!(
        (funnel.power_type, funnel.mana_cost, funnel.mana_per_second),
        (0xFFFF_FFFE, 11, 5)
    );
    assert!(funnel.tooltip_channeled());
    // The cast cell's attr arms at their pinned rows: Heroic Strike (next melee, rage 150 wire =
    // "15 Rage" displayed), Auto Shot and Throw (the ranged bit ALONE — Throw is not
    // auto-repeat and still reads "Attack speed"), Mind Flay (channeled mana), Judgement
    // (pct-only mana — the resolved flat number, never a percentage line).
    let hs = cat.get(78).unwrap();
    assert_eq!((hs.power_type, hs.mana_cost), (1, 150));
    assert!(hs.on_next_swing());
    assert!(cat.get(75).unwrap().tooltip_on_next_ranged(), "Auto Shot");
    assert!(cat.get(2764).unwrap().tooltip_on_next_ranged(), "Throw");
    let mf = cat.get(15407).unwrap();
    assert!(mf.tooltip_channeled());
    assert_eq!((mf.power_type, mf.mana_cost), (0, 45));
    let judgement = cat.get(20271).unwrap();
    assert_eq!(
        (
            judgement.power_type,
            judgement.mana_cost,
            judgement.mana_cost_pct
        ),
        (0, 0, 6)
    );

    // The column scan, both verdicts pinned: `manaPerSecondPerLevel` (35) is all-zero across
    // the whole file — the verified negative that keeps it unparsed — while `manaCostPerlevel`
    // (33) is real: exactly 72 nonzero rows, all creature-cast spells (Dark Offering's is even
    // in the health lane), which is why the per-level term is modeled in `power_cost` but
    // dormant for player tooltips. (1074)
    let bytes = chain.read_file(super::SPELL).expect("reading Spell.dbc");
    let rs = super::parse(&bytes, super::spell_schema(), "Spell.dbc").expect("Spell.dbc");
    let mut per_level_rows = 0u32;
    for r in rs.records() {
        let id = super::u32_at(r, 0).unwrap_or(0);
        if super::u32_at(r, 33).unwrap_or(0) > 0 {
            per_level_rows += 1;
        }
        assert_eq!(
            super::u32_at(r, 35).unwrap_or(0),
            0,
            "manaPerSecondPerLevel on {id}"
        );
    }
    assert_eq!(per_level_rows, 72, "the manaCostPerlevel population");
    let dark_offering = cat.get(7154).unwrap();
    assert_eq!(
        (
            dark_offering.power_type,
            dark_offering.mana_cost,
            dark_offering.mana_cost_per_level,
            dark_offering.spell_level
        ),
        (0xFFFF_FFFE, 180, 9, 24),
        "Dark Offering: the per-level term's health-lane row"
    );
}
