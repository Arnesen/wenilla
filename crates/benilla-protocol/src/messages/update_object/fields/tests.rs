use super::*;

/// The 0208 combat/stat indices re-derived from vmangos's own enum arithmetic (`OBJECT_END` =
/// 6, `UNIT_END` = 188) and chain-locked to the file's two live-tested anchors:
/// `FIELD_PLAYER_INV_SLOT_HEAD` (486, descriptor-dump verified) and
/// `FIELD_PLAYER_BUYBACK_PRICE_1` (1226, wow-re byte anchor, decision 0138). If any constant
/// drifts, this is the tripwire.
#[test]
fn combat_stat_indices_chain_to_the_tested_anchors() {
    const OBJECT_END: u16 = 6;
    const UNIT_END: u16 = OBJECT_END + 0xB6; // UpdateFields_1_12_1.h
    assert_eq!(UNIT_END, 188);
    // The two tested anchors re-derive from the same arithmetic — the chain is sound.
    assert_eq!(FIELD_PLAYER_INV_SLOT_HEAD, UNIT_END + 0x12A);
    assert_eq!(FIELD_PLAYER_BUYBACK_PRICE_1, UNIT_END + 0x40E);
    // UNIT combat block.
    assert_eq!(FIELD_UNIT_BASEATTACKTIME, OBJECT_END + 0x78);
    assert_eq!(FIELD_UNIT_RANGEDATTACKTIME, OBJECT_END + 0x7A);
    assert_eq!(FIELD_UNIT_MINDAMAGE, OBJECT_END + 0x80);
    assert_eq!(FIELD_UNIT_MAXDAMAGE, OBJECT_END + 0x81);
    assert_eq!(FIELD_UNIT_MINOFFHANDDAMAGE, OBJECT_END + 0x82);
    assert_eq!(FIELD_UNIT_MAXOFFHANDDAMAGE, OBJECT_END + 0x83);
    assert_eq!(FIELD_UNIT_STAT0, OBJECT_END + 0x90);
    assert_eq!(FIELD_UNIT_RESISTANCES, OBJECT_END + 0x95);
    assert_eq!(FIELD_UNIT_ATTACK_POWER, OBJECT_END + 0x9F);
    assert_eq!(FIELD_UNIT_ATTACK_POWER_MODS, OBJECT_END + 0xA0);
    assert_eq!(FIELD_UNIT_ATTACK_POWER_MULTIPLIER, OBJECT_END + 0xA1);
    assert_eq!(FIELD_UNIT_RANGED_ATTACK_POWER, OBJECT_END + 0xA2);
    assert_eq!(FIELD_UNIT_RANGED_ATTACK_POWER_MODS, OBJECT_END + 0xA3);
    assert_eq!(FIELD_UNIT_RANGED_ATTACK_POWER_MULTIPLIER, OBJECT_END + 0xA4);
    assert_eq!(FIELD_UNIT_MINRANGEDDAMAGE, OBJECT_END + 0xA5);
    assert_eq!(FIELD_UNIT_MAXRANGEDDAMAGE, OBJECT_END + 0xA6);
    // The rested-XP pool sits exactly one below the tested COINAGE anchor, and exactly one past
    // the explored-zones bitset's 64 slots (decision 1082).
    assert_eq!(FIELD_PLAYER_REST_STATE_EXPERIENCE, UNIT_END + 0x3DB);
    assert_eq!(
        FIELD_PLAYER_REST_STATE_EXPERIENCE + 1,
        FIELD_PLAYER_FIELD_COINAGE
    );
    assert_eq!(
        FIELD_PLAYER_EXPLORED_ZONES_1 + PLAYER_EXPLORED_ZONES_SLOTS,
        FIELD_PLAYER_REST_STATE_EXPERIENCE
    );
    // PLAYER stat block: POSSTAT0 sits one past the tested COINAGE, and the run through
    // AMMO_ID lands three shy of the tested BUYBACK_PRICE_1 (BYTES 1222, SELF_RES 1224,
    // PVP_MEDALS 1225 between).
    assert_eq!(FIELD_PLAYER_POSSTAT0, FIELD_PLAYER_FIELD_COINAGE + 1);
    assert_eq!(FIELD_PLAYER_POSSTAT0, UNIT_END + 0x3DD);
    assert_eq!(FIELD_PLAYER_NEGSTAT0, UNIT_END + 0x3E2);
    assert_eq!(FIELD_PLAYER_RESISTANCEBUFFMODSPOSITIVE, UNIT_END + 0x3E7);
    assert_eq!(FIELD_PLAYER_RESISTANCEBUFFMODSNEGATIVE, UNIT_END + 0x3EE);
    assert_eq!(FIELD_PLAYER_MOD_DAMAGE_DONE_POS, UNIT_END + 0x3F5);
    assert_eq!(FIELD_PLAYER_MOD_DAMAGE_DONE_NEG, UNIT_END + 0x3FC);
    assert_eq!(FIELD_PLAYER_MOD_DAMAGE_DONE_PCT, UNIT_END + 0x403);
    assert_eq!(FIELD_PLAYER_AMMO_ID, UNIT_END + 0x40B);
    assert_eq!(FIELD_PLAYER_AMMO_ID + 3, FIELD_PLAYER_BUYBACK_PRICE_1);
    assert_eq!(FIELD_PLAYER_FIELD_BYTES, UNIT_END + 0x40A);
    assert_eq!(FIELD_PLAYER_FIELD_BYTES + 1, FIELD_PLAYER_AMMO_ID);
    // The combo-target GUID is the two dwords immediately BEFORE the live-tested XP pair, so it
    // needs no anchor of its own. The binary agrees from the other side: `GetComboPoints 0x51a190`
    // reads it at `[player+0xe68]+0x838`, and 0x838/4 = 0x20E (decision 0875).
    assert_eq!(FIELD_PLAYER_FIELD_COMBO_TARGET, UNIT_END + 0x20E);
    assert_eq!(FIELD_PLAYER_FIELD_COMBO_TARGET + 2, FIELD_PLAYER_XP);
    // The skill array: one past the tested XP pair, 384 dwords ending at CHARACTER_POINTS1.
    assert_eq!(FIELD_PLAYER_SKILL_INFO_1_1, FIELD_PLAYER_NEXT_LEVEL_XP + 1);
    assert_eq!(FIELD_PLAYER_SKILL_INFO_1_1, UNIT_END + 0x212);
    assert_eq!(FIELD_PLAYER_SKILL_INFO_1_1 + 384, UNIT_END + 0x392);
    // The character-points pair sits exactly there (UNIT_END + 0x392/0x393; decision 0304).
    assert_eq!(
        FIELD_PLAYER_CHARACTER_POINTS1,
        FIELD_PLAYER_SKILL_INFO_1_1 + 384
    );
    assert_eq!(FIELD_PLAYER_CHARACTER_POINTS2, UNIT_END + 0x393);
    // The tracking-mask pair sits contiguously past it (UNIT_END + 0x394/0x395).
    assert_eq!(
        FIELD_PLAYER_TRACK_CREATURES,
        FIELD_PLAYER_CHARACTER_POINTS2 + 1
    );
    assert_eq!(FIELD_PLAYER_TRACK_RESOURCES, UNIT_END + 0x395);
}

#[test]
fn unit_stats_and_resistances_read_indexed_and_gate_range() {
    let f = ObjectFields::from_pairs(&[
        (150, 25),                  // STAT0 strength
        (154, 31),                  // STAT4 spirit
        (155, 120),                 // RESISTANCES[0] = armor
        (157, 15),                  // fire
        (161, 8u32.wrapping_neg()), // arcane, cursed to −8
    ]);
    assert_eq!(f.unit_stat(0), Some(25));
    assert_eq!(f.unit_stat(4), Some(31));
    assert_eq!(f.unit_stat(1), None, "never streamed");
    assert_eq!(f.unit_stat(5), None, "out of range");
    assert_eq!(f.unit_resistance(0), Some(120));
    assert_eq!(f.unit_resistance(2), Some(15));
    assert_eq!(f.unit_resistance(6), Some(-8), "resistances are signed");
    assert_eq!(f.unit_resistance(7), None, "out of range");
}

#[test]
fn attack_power_mods_split_signed_halves() {
    // vmangos SetInt16Value(index_mod, 0, pos) / (index_mod, 1, neg): lo = positive,
    // hi = negative (already negative-or-zero). pos 30, neg −10.
    let packed = u32::from(30u16) | (u32::from((-10i16) as u16) << 16);
    let f = ObjectFields::from_pairs(&[
        (165, 78),                              // ATTACK_POWER
        (166, packed),                          // ATTACK_POWER_MODS
        (167, 0.1f32.to_bits()),                // MULTIPLIER (stores multiplier − 1.0)
        (168, 52),                              // RANGED_ATTACK_POWER
        (169, u32::from((-3i16) as u16) << 16), // ranged: pos 0, neg −3
        (170, 0.0f32.to_bits()),
    ]);
    assert_eq!(f.unit_attack_power(), Some(78));
    assert_eq!(f.unit_attack_power_mods(), (30, -10));
    assert_eq!(f.unit_attack_power_multiplier(), Some(0.1));
    assert_eq!(f.unit_ranged_attack_power(), Some(52));
    assert_eq!(f.unit_ranged_attack_power_mods(), (0, -3));
    assert_eq!(f.unit_ranged_attack_power_multiplier(), Some(0.0));
    // Absent mods read (0, 0) — the descriptor's zero-initialized default.
    assert_eq!(ObjectFields::default().unit_attack_power_mods(), (0, 0));
}

#[test]
fn damage_and_attack_time_fields_read_their_slots() {
    let f = ObjectFields::from_pairs(&[
        (126, 2900),              // BASEATTACKTIME main
        (127, 1500),              // BASEATTACKTIME offhand
        (128, 2800),              // RANGEDATTACKTIME
        (134, 12.5f32.to_bits()), // MINDAMAGE
        (135, 19.5f32.to_bits()), // MAXDAMAGE
        (136, 5.0f32.to_bits()),  // MINOFFHANDDAMAGE
        (137, 9.0f32.to_bits()),  // MAXOFFHANDDAMAGE
        (171, 31.0f32.to_bits()), // MINRANGEDDAMAGE
        (172, 47.0f32.to_bits()), // MAXRANGEDDAMAGE
    ]);
    assert_eq!(f.unit_base_attack_time(0), Some(2900));
    assert_eq!(f.unit_base_attack_time(1), Some(1500));
    assert_eq!(f.unit_base_attack_time(2), None, "out of range");
    assert_eq!(f.unit_ranged_attack_time(), Some(2800));
    assert_eq!(f.unit_min_damage(), Some(12.5));
    assert_eq!(f.unit_max_damage(), Some(19.5));
    assert_eq!(f.unit_min_offhand_damage(), Some(5.0));
    assert_eq!(f.unit_max_offhand_damage(), Some(9.0));
    assert_eq!(f.unit_min_ranged_damage(), Some(31.0));
    assert_eq!(f.unit_max_ranged_damage(), Some(47.0));
}

#[test]
fn player_stat_buff_arrays_read_floats_with_signed_negatives() {
    let f = ObjectFields::from_pairs(&[
        (1177, 10.0f32.to_bits()),    // POSSTAT0
        (1182, (-4.0f32).to_bits()),  // NEGSTAT0 (stored negative — ApplyStatBuffMod)
        (1187, 30.0f32.to_bits()),    // RESISTANCEBUFFMODSPOSITIVE[0]
        (1196, (-20.0f32).to_bits()), // RESISTANCEBUFFMODSNEGATIVE[2]
    ]);
    assert_eq!(f.player_posstat(0), Some(10.0));
    assert_eq!(f.player_negstat(0), Some(-4.0));
    assert_eq!(f.player_posstat(5), None, "out of range");
    assert_eq!(f.player_negstat(5), None, "out of range");
    assert_eq!(f.player_resistance_buff_pos(0), Some(30.0));
    assert_eq!(f.player_resistance_buff_neg(2), Some(-20.0));
    assert_eq!(f.player_resistance_buff_pos(7), None, "out of range");
    assert_eq!(f.player_resistance_buff_neg(7), None, "out of range");
}

#[test]
fn player_mod_damage_done_reads_int_pos_neg_and_float_pct() {
    let f = ObjectFields::from_pairs(&[
        (1201, 25),                   // MOD_DAMAGE_DONE_POS[0] physical
        (1203, 40),                   // MOD_DAMAGE_DONE_POS[2] fire
        (1208, 15u32.wrapping_neg()), // MOD_DAMAGE_DONE_NEG[0] = −15
        (1215, 1.1f32.to_bits()),     // MOD_DAMAGE_DONE_PCT[0] — a true float
    ]);
    assert_eq!(f.player_mod_damage_done_pos(0), Some(25));
    assert_eq!(f.player_mod_damage_done_pos(2), Some(40));
    assert_eq!(f.player_mod_damage_done_neg(0), Some(-15));
    assert_eq!(f.player_mod_damage_done_pct(0), Some(1.1));
    assert_eq!(f.player_mod_damage_done_pct(1), None, "never streamed");
    assert_eq!(f.player_mod_damage_done_pos(7), None, "out of range");
    assert_eq!(f.player_mod_damage_done_neg(7), None, "out of range");
    assert_eq!(f.player_mod_damage_done_pct(7), None, "out of range");
}

#[test]
fn player_skill_unpacks_the_three_dword_triplet() {
    // Slot 1 (base 718 + 3 = 721): Swords (43) step 0, 25/300, temp −3 / perm +5 —
    // MAKE_PAIR32 lo|hi packing throughout, bonuses signed (Player.cpp:94-100).
    let f = ObjectFields::from_pairs(&[
        (721, 43),
        (722, 25 | (300 << 16)),
        (723, u32::from((-3i16) as u16) | (5 << 16)),
    ]);
    assert_eq!(
        f.player_skill(1),
        Some(PlayerSkillSlot {
            skill_id: 43,
            step: 0,
            value: 25,
            max: 300,
            temp_bonus: -3,
            perm_bonus: 5,
        })
    );
    // A never-streamed slot → None; a zeroed (server-cleared) slot → Some with id 0;
    // out of range → None.
    assert_eq!(f.player_skill(0), None);
    assert_eq!(
        ObjectFields::from_pairs(&[(718, 0)])
            .player_skill(0)
            .map(|s| s.skill_id),
        Some(0)
    );
    assert_eq!(f.player_skill(PLAYER_SKILL_SLOTS), None);
}

#[test]
fn player_ammo_id_reads_the_field() {
    let f = ObjectFields::from_pairs(&[(1223, 2512)]);
    assert_eq!(f.player_ammo_id(), Some(2512));
    assert_eq!(ObjectFields::default().player_ammo_id(), None);
}

#[test]
fn player_field_bytes_splits_into_combo_points_and_honor_rank() {
    // flags=0x01 / combo=0x02 / actionBars=0x03 / highestHonorRank=5 packed little-endian: the
    // dword's two live readers take byte 1 and byte 3 without bleeding into each other.
    let f = ObjectFields::from_pairs(&[(1222, 0x05_03_02_01)]);
    assert_eq!(f.player_honor_rank(), Some(5));
    assert_eq!(f.player_combo_points(), Some(2));
    // A full five points with everything else clear.
    let capped = ObjectFields::from_pairs(&[(1222, 0x00_00_05_00)]);
    assert_eq!(capped.player_combo_points(), Some(5));
    assert_eq!(capped.player_honor_rank(), Some(0));
    assert_eq!(ObjectFields::default().player_honor_rank(), None);
    assert_eq!(ObjectFields::default().player_combo_points(), None);
}

/// `PLAYER_FIELD_COMBO_TARGET` reads as one GUID out of its two dwords — the unit the points are
/// banked on, which the display gate compares against the current target (decision 0875).
#[test]
fn player_combo_target_reads_the_guid_pair() {
    let f = ObjectFields::from_pairs(&[(714, 0x1234_5678), (715, 0xF000_0001)]);
    assert_eq!(f.player_combo_target(), 0xF000_0001_1234_5678);
    // Nothing banked reads as 0 — the same value the current-target global holds with no target,
    // which is exactly why the binary's plain equality compare needs no null special case.
    assert_eq!(ObjectFields::default().player_combo_target(), 0);
}

/// The four aura arrays tile exactly, and the block lands on `BASEATTACKTIME` — an index this
/// file already chain-locks to two live-verified anchors. So this pins all four aura indices
/// without a new anchor of its own: 48 spell-id dwords, then 48 nibbles (6 dwords), then 48
/// bytes (12 dwords) twice, then `AURASTATE` (+0x77), then `BASEATTACKTIME` (+0x78).
#[test]
fn aura_arrays_tile_from_object_end_onto_the_tested_anchor() {
    const OBJECT_END: u16 = 6;
    assert_eq!(FIELD_UNIT_AURA, OBJECT_END + 0x29);
    assert_eq!(FIELD_UNIT_AURAFLAGS, OBJECT_END + 0x59);
    assert_eq!(FIELD_UNIT_AURALEVELS, OBJECT_END + 0x5F);
    assert_eq!(FIELD_UNIT_AURAAPPLICATIONS, OBJECT_END + 0x6B);

    assert_eq!(
        FIELD_UNIT_AURA + u16::from(UNIT_AURA_SLOTS),
        FIELD_UNIT_AURAFLAGS,
        "48 dwords of spell id"
    );
    assert_eq!(
        FIELD_UNIT_AURAFLAGS + 6,
        FIELD_UNIT_AURALEVELS,
        "48 nibbles"
    );
    assert_eq!(
        FIELD_UNIT_AURALEVELS + 12,
        FIELD_UNIT_AURAAPPLICATIONS,
        "48 bytes"
    );
    assert_eq!(
        FIELD_UNIT_AURAAPPLICATIONS + 12 + 1,
        FIELD_UNIT_BASEATTACKTIME,
        "48 bytes, then the 1-dword AURASTATE, then the tested anchor"
    );
}

/// The packing goldens: nibble-per-slot flags (8/dword), byte-per-slot levels and applications
/// (4/dword, holding `stack - 1`). Slot 32 deliberately omits its applications word — an absent
/// field must read as the descriptor's zero, i.e. a stack of 1, not a stack of 0.
#[test]
fn aura_slots_unpack_nibbles_bytes_and_the_stack_bias() {
    let f = ObjectFields::from_pairs(&[
        // Spell ids: slot 0 and 2 (buffs), slot 32 and 47 (debuffs).
        (47, 1126),
        (49, 25),
        (79, 589),
        (94, 11),
        // Flags: slot 0 nibble 0x9 (eff0 + cancelable), slot 2 nibble 0x8 (eff0, not cancelable).
        (95, 0x0000_0809),
        (99, 0x0000_0002),  // slot 32, nibble 0x2 (eff2)
        (100, 0xE000_0000), // slot 47, nibble 0xE in the top nibble
        // Levels: slot 0 → 60, slot 2 → 5.
        (101, 0x0005_003C),
        (109, 0x0000_003C), // slot 32 → 60
        (112, 0x0100_0000), // slot 47 → 1
        // Applications: slot 0 → byte 0 (stack 1), slot 2 → byte 2 (stack 3).
        (113, 0x0002_0000),
        (124, 0xFE00_0000), // slot 47 → byte 254 (stack 255)
    ]);

    let buff = f.unit_aura(0).expect("slot 0 occupied");
    assert_eq!(
        buff,
        UnitAuraSlot {
            slot: 0,
            spell_id: 1126,
            flags: 0x9,
            level: 60,
            stacks: 1
        }
    );
    assert!(buff.is_helpful() && buff.is_cancelable());
    assert!(
        buff.flags & AURA_FLAG_EFF_INDEX_MASK != 0,
        "an occupied slot always has an effect"
    );

    assert_eq!(f.unit_aura(1), None, "no spell id ⇒ empty slot");

    let stacked = f.unit_aura(2).expect("slot 2 occupied");
    assert_eq!(
        (stacked.spell_id, stacked.level, stacked.stacks),
        (25, 5, 3)
    );
    assert!(!stacked.is_cancelable(), "nibble 0x8 has no cancelable bit");

    let debuff = f.unit_aura(32).expect("slot 32 occupied");
    assert_eq!(
        debuff,
        UnitAuraSlot {
            slot: 32,
            spell_id: 589,
            flags: 0x2,
            level: 60,
            stacks: 1
        }
    );
    assert!(!debuff.is_helpful() && !debuff.is_cancelable());

    let last = f.unit_aura(47).expect("slot 47 occupied");
    assert_eq!((last.flags, last.level, last.stacks), (0xE, 1, 255));

    assert_eq!(f.unit_aura(UNIT_AURA_SLOTS), None, "out of range");
    assert_eq!(
        f.unit_auras().map(|a| a.slot).collect::<Vec<_>>(),
        [0, 2, 32, 47],
        "occupied slots, ascending"
    );
    assert_eq!(ObjectFields::default().unit_auras().count(), 0);
}

/// A slot whose flags nibble the server cleared (`& 0x0E == 0`) is **empty**, even if a stale
/// spell id lingers in `UNIT_FIELD_AURA` — the client's own `flags & 0x0E` liveness test. Gating
/// on the spell id alone (as an earlier cut did) surfaced the husk as a phantom buff the real
/// client hides. The cancelable bit `0x1` on its own does not resurrect it (it is not an effect
/// bit), so this covers both a fully-zeroed nibble and a `0x1`-only one.
#[test]
fn a_stale_spell_id_with_a_cleared_flags_nibble_is_not_a_live_aura() {
    let f = ObjectFields::from_pairs(&[
        // slot 0: a live buff — spell id + effect bit 0x8.
        (47, 1126),
        (95, 0x0000_0018), // slot 0 nibble 0x8 (eff0), slot 1 nibble 0x1 (cancelable only)
        // slot 1: a HUSK — the server left the spell id but cleared every effect bit, keeping
        // only the cancelable bit. `flags & 0x0E == 0`, so it is not live.
        (48, 5000),
        // slot 2: a HUSK — spell id present, flags nibble fully zero.
        (49, 6000),
    ]);

    assert!(f.unit_aura(0).is_some(), "slot 0 is a live buff");
    assert_eq!(
        f.unit_aura(1),
        None,
        "a spell id with only the cancelable bit (no effect) is a husk, not a live aura"
    );
    assert_eq!(
        f.unit_aura(2),
        None,
        "a spell id with a zero flags nibble is a husk, not a live aura"
    );
    assert_eq!(
        f.unit_auras().map(|a| a.slot).collect::<Vec<_>>(),
        [0],
        "only the live slot enumerates; the two husks are hidden"
    );
}

/// The create-block absent-field semantics (director-caught: fully broken gear read as 100%). A
/// CREATE omits zero-valued fields (vmangos `_SetCreateBits`: mask bit iff `value != 0`), so a
/// create-seeded store must answer `Some(0)` for an absent field — the real client's
/// zero-initialized descriptor — while a bare `Values` delta keeps absent = `None` ("untouched").
#[test]
fn created_store_reads_absent_fields_as_zero_a_delta_as_none() {
    // A broken item's create: MAXDURABILITY present (40), DURABILITY omitted (it is 0).
    let create = ObjectFields::from_pairs(&[(3, 2264), (47, 40)]).into_created(ObjectType::Item);
    assert_eq!(create.item_durability(), Some(0), "created absent = 0");
    assert_eq!(create.item_max_durability(), Some(40));

    // The same mask as a bare delta: absent means "not changed", never 0.
    let delta = ObjectFields::from_pairs(&[(47, 40)]);
    assert_eq!(delta.item_durability(), None, "delta absent = untouched");

    // A guid slot keeps present-iff-low-half semantics even on a created store (consumers
    // already treat a sent-empty `Some(0)` as no-guid; the fallback would only blur the two).
    assert_eq!(create.corpse_owner(), None);
}

/// …and "absent = 0" stops at the end of the object's OWN descriptor (decision 1081). A creature
/// has no PLAYER block to be absent from, so a PLAYER-block read off one is `None` — a question it
/// cannot answer — not a confident `0`.
///
/// The live bug: `unit_combat_stats` reads `PLAYER_FIELD_MOD_DAMAGE_DONE_PCT` for any unit and
/// defaults an absent answer to the divide-safe `1.0`. With a created pet answering `Some(0.0)`,
/// the default never fired and the ref's `damage / percent` produced the pet sheet's
/// `inf - inf` / `nan` tooltip and its red damage line (director, 2026-08-07).
#[test]
fn a_creature_has_no_player_block_to_be_absent_from() {
    // PLAYER_FIELD_MOD_DAMAGE_DONE_PCT[0] = UNIT_END(188) + 0x403 — past a UNIT's descriptor end.
    const MOD_DAMAGE_DONE_PCT: u16 = 1215;
    // UNIT_FIELD_BASEATTACKTIME — well inside it.
    const BASEATTACKTIME: u16 = 126;

    let pet = ObjectFields::from_pairs(&[(2, 0x09), (150, 33)]).into_created(ObjectType::Unit);
    assert_eq!(
        pet.player_mod_damage_done_pct(0),
        None,
        "a creature cannot answer a PLAYER-block field"
    );
    assert_eq!(
        pet.unit_base_attack_time(0),
        Some(0),
        "…while a field it DOES have still reads the create's zero"
    );
    assert_eq!(pet.unit_stat(0), Some(33), "and a present field is itself");

    // The same index on a real player: inside the descriptor, so absent is the honest 0 the
    // client's own zero-initialized buffer holds.
    let player = ObjectFields::from_pairs(&[(2, 0x19), (BASEATTACKTIME, 1800)])
        .into_created(ObjectType::Player);
    assert_eq!(player.player_mod_damage_done_pct(0), Some(0.0));
    assert_eq!(player.unit_base_attack_time(0), Some(1800));

    // A bare delta answers nothing either way — it never claimed to be complete.
    let delta = ObjectFields::from_pairs(&[(MOD_DAMAGE_DONE_PCT, 1.0f32.to_bits())]);
    assert_eq!(delta.player_mod_damage_done_pct(0), Some(1.0));
    assert_eq!(delta.unit_base_attack_time(0), None);
}

/// Merging keeps the semantics straight: a delta overlays a created store without unsetting the
/// created mark, and a re-CREATE *replaces* the store — stale non-zero values for fields the
/// fresh snapshot omits (they dropped to zero out of view) must die with it.
#[test]
fn merge_overlays_deltas_and_replaces_on_recreate() {
    let mut store =
        ObjectFields::from_pairs(&[(3, 2264), (46, 40), (47, 40)]).into_created(ObjectType::Item);

    // A durability-damage delta overlays; the store stays a complete (created) descriptor.
    store.merge(ObjectFields::from_pairs(&[(46, 30)]));
    assert_eq!(store.item_durability(), Some(30));
    assert_eq!(
        store.item_max_durability(),
        Some(40),
        "untouched field kept"
    );

    // Re-create after the item broke out of view: the fresh snapshot omits DURABILITY (0).
    store.merge(ObjectFields::from_pairs(&[(3, 2264), (47, 40)]).into_created(ObjectType::Item));
    assert_eq!(
        store.item_durability(),
        Some(0),
        "a re-create replaces — the stale 30 must not survive an overlay"
    );
}

/// The `DYNAMICOBJECT_*` accessors against the exact live capture (vmangos, 2026-07-30, the
/// B132 follow-up's `--groundfx 10` run): Blizzard's dynobj create — caster guid 26, BYTES 1
/// (area spell), SPELLID 10, RADIUS 8.0, the cast point in POS, FACING never sent.
#[test]
fn dynamicobject_fields_read_the_live_blizzard_capture() {
    let f = ObjectFields::from_pairs(&[
        (0, 6),                        // OBJECT_FIELD_GUID lo
        (1, 0xf100_0000),              // guid hi — HIGHGUID dynobj
        (2, 0x41),                     // TYPE: OBJECT | DYNAMICOBJECT
        (3, 10),                       // ENTRY = the spell id
        (4, 1.0f32.to_bits()),         // SCALE_X
        (6, 26),                       // DYNAMICOBJECT_CASTER lo
        (8, 1),                        // BYTES: area spell
        (9, 10),                       // SPELLID
        (10, 8.0f32.to_bits()),        // RADIUS
        (11, (-8844.14f32).to_bits()), // POS_X
        (12, 668.83f32.to_bits()),     // POS_Y
        (13, 97.81f32.to_bits()),      // POS_Z
    ])
    .into_created(ObjectType::DynamicObject);
    assert_eq!(f.dynamicobject_caster(), Some(26));
    assert_eq!(f.dynamicobject_bytes(), Some(1));
    assert_eq!(f.dynamicobject_spell_id(), Some(10));
    assert_eq!(f.dynamicobject_radius(), Some(8.0));
    let (pos, facing) = f.dynamicobject_position().unwrap();
    assert_eq!(pos, [-8844.14, 668.83, 97.81]);
    assert_eq!(facing, 0.0, "FACING absent on a created store reads 0");
    // Not a dynobj (a corpse never streams SPELLID): the id gate reads None, not Some(0).
    assert_eq!(
        ObjectFields::from_pairs(&[(6, 26)]).dynamicobject_spell_id(),
        None
    );
}

/// **Feign death is one bit, and the client reads it everywhere it reads health** (decision 1022).
/// `UNIT_DYNFLAG_DEAD` leaves `UNIT_FIELD_HEALTH` alone — vmangos `Unit::SetFeignDeath` only sets
/// the flag — so every predicate that keys on the raw field must stay FALSE while every one that
/// keys on how the unit *reads* must flip. The byte law being transcribed: `UnitHealth 0x5174d0`
/// and `UnitMana 0x517670` answer 0 under the flag, their `*Max` siblings (`0x5175b0`/`0x5177e0`)
/// have no such gate, and `0x605f90` is the shared reads-dead triple.
#[test]
fn feign_death_reads_dead_without_touching_the_raw_health_field() {
    const MANA: u8 = 0;
    let unit = |pairs: &[(u16, u32)]| ObjectFields::from_pairs(pairs);
    let alive = unit(&[
        (FIELD_UNIT_HEALTH, 1200),
        (FIELD_UNIT_MAXHEALTH, 1500),
        (FIELD_UNIT_POWER1, 300),
        (FIELD_UNIT_MAXPOWER1, 900),
    ]);
    let feigning = unit(&[
        (FIELD_UNIT_HEALTH, 1200),
        (FIELD_UNIT_MAXHEALTH, 1500),
        (FIELD_UNIT_POWER1, 300),
        (FIELD_UNIT_MAXPOWER1, 900),
        (FIELD_UNIT_DYNAMIC_FLAGS, 0x20),
    ]);
    let corpse = unit(&[(FIELD_UNIT_HEALTH, 0), (FIELD_UNIT_MAXHEALTH, 1500)]);

    // The raw field predicate is untouched by the flag — the death arc, loot and the corpse never
    // fire for a feign.
    assert!(!alive.unit_is_dead());
    assert!(!feigning.unit_is_dead(), "feign leaves HEALTH alone");
    assert!(corpse.unit_is_dead());
    assert_eq!(feigning.unit_health(), Some(1200), "the raw field survives");

    // …and the reads-dead predicate flips for both, plus the stand-state leg.
    assert!(!alive.unit_reads_dead());
    assert!(feigning.unit_reads_dead(), "0x605f9d — the dynflag leg");
    assert!(corpse.unit_reads_dead());
    assert!(
        unit(&[
            (FIELD_UNIT_HEALTH, 1200),
            (FIELD_UNIT_MAXHEALTH, 1500),
            (FIELD_UNIT_BYTES_1, 7),
        ])
        .unit_reads_dead(),
        "0x605faa — stand state 7 (inert against vmangos, kept as the reference has it)"
    );

    // The UI getters: current goes to zero, the maxima do not — that is what makes the bar render
    // 0/max (empty) instead of vanishing.
    assert_eq!(alive.unit_shown_health(), Some(1200));
    assert_eq!(feigning.unit_shown_health(), Some(0));
    assert_eq!(feigning.unit_max_health(), Some(1500));
    assert_eq!(alive.unit_shown_power(MANA), Some(300));
    assert_eq!(feigning.unit_shown_power(MANA), Some(0));
    assert_eq!(feigning.unit_max_power(MANA), Some(900));
    // Out-of-range power slots stay nil under the flag, exactly as the ungated reader has them.
    assert_eq!(feigning.unit_shown_power(5), None);
    // An empty store is not a feigning one: no flag, no zeroing, and no invented health.
    assert!(!ObjectFields::default().unit_reads_dead());
    assert_eq!(ObjectFields::default().unit_shown_health(), None);
}

/// **The raw→display power divide** (decision 1034, wow-re `feign-death-dyndead.md` §3): the
/// reference's `UnitMana`/`UnitManaMax` divide by `0x6e7130`'s table at `0x86f978` —
/// `{1, 10, 1, 1, 1000}`. vmangos ships rage max **1000** and pet happiness max **1050000**
/// (`GetCreatePowers`), which through that divide are the 100 and 1050 a real client shows. The
/// RAW accessors must stay raw: the pet happiness bucket thresholds compare on the wire scale.
#[test]
fn rage_and_happiness_divide_for_display_but_not_for_the_raw_readers() {
    const RAGE: u8 = 1;
    const HAPPINESS: u8 = 4;
    const MANA: u8 = 0;
    let warrior = ObjectFields::from_pairs(&[
        (FIELD_UNIT_POWER1 + u16::from(RAGE), 350),
        (FIELD_UNIT_MAXPOWER1 + u16::from(RAGE), 1000),
    ]);
    assert_eq!(
        warrior.unit_power(RAGE),
        Some(350),
        "the wire value, untouched"
    );
    assert_eq!(warrior.unit_shown_power(RAGE), Some(35));
    assert_eq!(warrior.unit_shown_max_power(RAGE), Some(100), "not 1000");

    let pet = ObjectFields::from_pairs(&[
        (FIELD_UNIT_POWER1 + u16::from(HAPPINESS), 1_020_000),
        (FIELD_UNIT_MAXPOWER1 + u16::from(HAPPINESS), 1_050_000),
    ]);
    assert_eq!(
        pet.unit_power(HAPPINESS),
        Some(1_020_000),
        "raw — the happiness bucket thresholds are on this scale"
    );
    assert_eq!(pet.unit_shown_power(HAPPINESS), Some(1020));
    assert_eq!(pet.unit_shown_max_power(HAPPINESS), Some(1050));

    // Mana/focus/energy divide by 1 — the shown and raw readers agree.
    let caster = ObjectFields::from_pairs(&[
        (FIELD_UNIT_POWER1 + u16::from(MANA), 4200),
        (FIELD_UNIT_MAXPOWER1 + u16::from(MANA), 8000),
    ]);
    assert_eq!(caster.unit_shown_power(MANA), caster.unit_power(MANA));
    assert_eq!(
        caster.unit_shown_max_power(MANA),
        caster.unit_max_power(MANA)
    );
    // Absent stays nil rather than becoming a divided zero.
    assert_eq!(caster.unit_shown_power(RAGE), None);
    assert_eq!(caster.unit_shown_max_power(RAGE), None);
    // The dead gate still wins over the divide, and only for the current value.
    let feigning_warrior = ObjectFields::from_pairs(&[
        (FIELD_UNIT_POWER1 + u16::from(RAGE), 350),
        (FIELD_UNIT_MAXPOWER1 + u16::from(RAGE), 1000),
        (FIELD_UNIT_DYNAMIC_FLAGS, 0x20),
    ]);
    assert_eq!(feigning_warrior.unit_shown_power(RAGE), Some(0));
    assert_eq!(feigning_warrior.unit_shown_max_power(RAGE), Some(100));
}
