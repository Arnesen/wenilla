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
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
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
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
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
    assert_eq!(attack.effect_1, 78, "6603 Effect[0] == SPELL_EFFECT_ATTACK");
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
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
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
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
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
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
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
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
    let mut chain = crate::open_chain(&data).expect("open chain");
    let cat = load_spell_catalog(&mut chain).expect("load Spell/SpellIcon");

    // The gathering/lockpick openers carry SPELL_EFFECT_OPEN_LOCK; `open_lock_type` is the
    // LockType they open — the same indices Lock.dbc's skill slots name (mining vein → 3).
    assert_eq!(
        cat.get(2575).unwrap().open_lock_type,
        Some(3),
        "Mining opens LockType 3"
    );
    assert_eq!(
        cat.get(2366).unwrap().open_lock_type,
        Some(2),
        "Herb Gathering opens LockType 2"
    );
    assert_eq!(
        cat.get(1804).unwrap().open_lock_type,
        Some(1),
        "Pick Lock opens LockType 1"
    );
    // A plain damage spell opens no lock (Effect[0] is not OPEN_LOCK).
    assert_eq!(
        cat.get(133).unwrap().open_lock_type,
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

/// The cooldown/cost/range columns on the real build-5875 `Spell.dbc`, pinned 2026-07-10
/// against the vmangos `spell_template` rows (MAX(build) ≤ 5875 per entry — the module's
/// established cross-check). A slip on any of columns 2/19/20/31/32/36/156/157/158 fails
/// loudly. Skips without client data.
#[test]
fn real_spell_catalog_reads_cooldown_cost_and_range_columns() {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
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
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
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
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
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
}

/// The tooltip-arc columns (decision 0274 P2) on the real build-5875 `Spell.dbc`, pinned
/// 2026-07-10: description/aura-description text, DurationIndex/CastingTimeIndex/ProcChance,
/// and the per-effect arrays — end-to-end through the new [`load_spell_cast_times`]/
/// [`load_spell_durations`] catalogs. Skips without client data.
#[test]
fn real_spell_catalog_reads_tooltip_columns() {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
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
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
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
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
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
            d.effect_1, SPELL_EFFECT_CREATE_ITEM,
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
        assert_eq!(d.effect_1, SPELL_EFFECT_TRADE_SKILL, "opener {opener}");
        assert_eq!(
            d.effect_item_type, [0; 3],
            "opener {opener} creates nothing"
        );
    }
}
