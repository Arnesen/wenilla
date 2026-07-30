//! The `Unit*` binding tests (the parent module is the unit under test).

use crate::script::{UiScript, UnitState};

fn player() -> UnitState {
    UnitState {
        exists: true,
        name: Some("Benilla".into()),
        health: 72,
        max_health: 100,
        level: 12,
        power_type: 1, // rage
        power: 35,
        max_power: 100,
        dead: false,
        reaction: 0,
        race: Some("Night Elf".into()),
        race_file: Some("NightElf".into()),
        class: Some("Warrior".into()),
        class_file: Some("WARRIOR".into()),
        sex: 3, // female
        ..Default::default()
    }
}

#[test]
fn unit_reaction_reports_the_scale_value_or_nil() {
    let mut s = UiScript::new().unwrap();
    // Neutral target (UnitReaction 4) → the integer; the name-plate palette indexes it.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            reaction: 4,
            ..Default::default()
        }),
    );
    assert_eq!(
        s.eval::<i64>(r#"return UnitReaction("target", "player")"#)
            .unwrap(),
        4
    );
    // reaction 0 (unresolved) reports as nil — the API's "can't tell".
    s.set_unit(
        "player",
        Some(UnitState {
            exists: true,
            reaction: 0,
            ..Default::default()
        }),
    );
    assert!(s
        .eval::<bool>(r#"return UnitReaction("player", "target") == nil"#)
        .unwrap());
}

/// The target plate's faction decision (TargetFrame_CheckFaction, as `BenillaUnitFrame_Update`
/// composes it): a player-controlled target picks red (mutually hostile) or blue (else) and
/// NEVER the reaction green; only an NPC reads the reaction swatch. This pins the predicate
/// composition the plate branch depends on — a friendly *player* must resolve to blue, which is
/// exactly the green-vs-blue bug the branch fixes.
#[test]
fn the_name_plate_faction_branch_picks_player_red_blue_over_the_reaction_swatch() {
    let mut s = UiScript::new().unwrap();
    // The plate's decision, verbatim from the XML Update (a chunk whose if-branches each return).
    let plate = r#"
            local u = "target"
            if UnitIsPlayer(u) then
                if UnitIsEnemy(u, "player") then return "red" else return "blue" end
            else
                return UnitReaction(u, "player") and "reaction" or "reaction-blue"
            end
        "#;
    let decide = |s: &mut UiScript| s.eval::<String>(plate).unwrap();

    // A friendly PLAYER (reaction 5) → blue, not the reaction green.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            is_player: true,
            reaction: 5,
            ..Default::default()
        }),
    );
    assert_eq!(decide(&mut s), "blue");
    // A hostile PLAYER (reaction 2) → red.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            is_player: true,
            reaction: 2,
            ..Default::default()
        }),
    );
    assert_eq!(decide(&mut s), "red");
    // A friendly NPC (same reaction 5, not a player) → the reaction swatch (green), unchanged.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            is_player: false,
            reaction: 5,
            ..Default::default()
        }),
    );
    assert_eq!(decide(&mut s), "reaction");
    // UnitIsPlayer itself: 1 for a player, nil otherwise.
    assert!(s
        .eval::<bool>(r#"return UnitIsPlayer("target") == nil"#)
        .unwrap());
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            is_player: true,
            ..Default::default()
        }),
    );
    assert_eq!(
        s.eval::<i64>(r#"return UnitIsPlayer("target")"#).unwrap(),
        1
    );
}

#[test]
fn unit_level_reads_minus_one_when_the_level_cant_be_told() {
    use crate::script::PlayerReqState;
    let mut s = UiScript::new().unwrap();
    s.set_player_req_state(PlayerReqState {
        level: 3,
        ..Default::default()
    });
    // A hostile 10+ levels up (reaction 2, level 13 vs player 3) → −1: the target frame's
    // skull branch (TargetFrame_CheckLevel's `level <= 0`).
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            level: 13,
            reaction: 2,
            ..Default::default()
        }),
    );
    assert_eq!(s.eval::<i64>(r#"return UnitLevel("target")"#).unwrap(), -1);
    // One level short of the band (12 vs 3) → the raw number.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            level: 12,
            reaction: 2,
            ..Default::default()
        }),
    );
    assert_eq!(s.eval::<i64>(r#"return UnitLevel("target")"#).unwrap(), 12);
    // The same 10-up gap on a NEUTRAL (reaction 4) tells its level — the gate is hostile-only.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            level: 13,
            reaction: 4,
            ..Default::default()
        }),
    );
    assert_eq!(s.eval::<i64>(r#"return UnitLevel("target")"#).unwrap(), 13);
    // A world boss (creature rank 3) hides its level regardless of the gap.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            level: 5,
            reaction: 4,
            rank: 3,
            ..Default::default()
        }),
    );
    assert_eq!(s.eval::<i64>(r#"return UnitLevel("target")"#).unwrap(), -1);
    // A PLAYER never reads −1 (rank/reaction notwithstanding).
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            level: 60,
            reaction: 2,
            is_player: true,
            ..Default::default()
        }),
    );
    assert_eq!(s.eval::<i64>(r#"return UnitLevel("target")"#).unwrap(), 60);
    // A raw level 0 (unstreamed) returns VERBATIM — never −1 (§5: `0x517fc0` pushes the
    // raw field; only the boss/hostile legs substitute). Same skull outcome via `<= 0`.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            level: 0,
            reaction: 2,
            ..Default::default()
        }),
    );
    assert_eq!(s.eval::<i64>(r#"return UnitLevel("target")"#).unwrap(), 0);
}

#[test]
fn corpse_can_attack_and_green_range_bindings() {
    use crate::script::PlayerReqState;
    let mut s = UiScript::new().unwrap();
    // UnitIsCorpse: a pure TYPEID_CORPSE object check (§5) — a DEAD unit is NOT a corpse…
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            dead: true,
            ..Default::default()
        }),
    );
    assert!(s
        .eval::<bool>(r#"return UnitIsCorpse("target") == nil"#)
        .unwrap());
    // …only a resolved corpse world object is.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            corpse_object: true,
            ..Default::default()
        }),
    );
    assert_eq!(
        s.eval::<i64>(r#"return UnitIsCorpse("target")"#).unwrap(),
        1
    );
    // UnitCanAttack reads the app-fed 0x606980 verdict off the non-player token, both
    // argument orders; false → nil.
    assert!(s
        .eval::<bool>(r#"return UnitCanAttack("player", "target") == nil"#)
        .unwrap());
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            can_attack: true,
            ..Default::default()
        }),
    );
    assert_eq!(
        s.eval::<i64>(r#"return UnitCanAttack("player", "target")"#)
            .unwrap(),
        1
    );
    assert_eq!(
        s.eval::<i64>(r#"return UnitCanAttack("target", "player")"#)
            .unwrap(),
        1
    );
    assert!(s
        .eval::<bool>(r#"return UnitIsCorpse("target") == nil"#)
        .unwrap());
    // GetQuestGreenRange: the grey band for the player's level — 4 at level 3, 9 at 46,
    // 12 clamped past the table (the byte-verified GREY_BAND).
    for (pl, want) in [(3u32, 4i64), (46, 9), (120, 12)] {
        s.set_player_req_state(PlayerReqState {
            level: pl,
            ..Default::default()
        });
        assert_eq!(
            s.eval::<i64>("return GetQuestGreenRange()").unwrap(),
            want,
            "green range at level {pl}"
        );
    }
}

#[test]
fn target_unit_queues_the_token_for_the_app_to_resolve() {
    let mut s = UiScript::new().unwrap();
    // Nothing queued until a call lands.
    assert!(s.take_target_requests().is_empty());
    // Each call queues its raw token, in order; a nil token is ignored.
    s.eval::<()>(r#"TargetUnit("player")"#).unwrap();
    s.eval::<()>(r#"TargetUnit("target")"#).unwrap();
    s.eval::<()>(r#"TargetUnit(nil)"#).unwrap();
    assert_eq!(s.take_target_requests(), vec!["player", "target"]);
    // The drain is a take — a second read is empty.
    assert!(s.take_target_requests().is_empty());
}

#[test]
fn set_unit_is_read_by_the_bindings() {
    let mut s = UiScript::new().unwrap();
    s.set_unit("player", Some(player()));

    assert!(s.eval::<bool>(r#"return UnitExists("player")"#).unwrap());
    assert_eq!(
        s.eval::<String>(r#"return UnitName("player")"#).unwrap(),
        "Benilla"
    );
    assert_eq!(s.eval::<i64>(r#"return UnitHealth("player")"#).unwrap(), 72);
    assert_eq!(
        s.eval::<i64>(r#"return UnitHealthMax("player")"#).unwrap(),
        100
    );
    assert_eq!(s.eval::<i64>(r#"return UnitLevel("player")"#).unwrap(), 12);
    assert!(!s.eval::<bool>(r#"return UnitIsDead("player")"#).unwrap());
}

#[test]
fn absent_token_reports_not_existing_with_zero_numbers() {
    let s = UiScript::new().unwrap();
    assert!(!s.eval::<bool>(r#"return UnitExists("target")"#).unwrap());
    assert_eq!(s.eval::<i64>(r#"return UnitHealth("target")"#).unwrap(), 0);
    assert_eq!(
        s.eval::<i64>(r#"return UnitHealthMax("target")"#).unwrap(),
        0
    );
    assert_eq!(s.eval::<i64>(r#"return UnitLevel("target")"#).unwrap(), 0);
    assert!(s
        .eval::<bool>(r#"return UnitName("target") == nil"#)
        .unwrap());
    assert!(!s.eval::<bool>(r#"return UnitIsDead("target")"#).unwrap());
}

#[test]
fn a_dead_unit_reports_dead_and_zero_health() {
    let mut s = UiScript::new().unwrap();
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            name: None,
            health: 0,
            max_health: 3200,
            level: 30,
            dead: true,
            ..Default::default()
        }),
    );
    assert!(s.eval::<bool>(r#"return UnitExists("target")"#).unwrap());
    assert!(s.eval::<bool>(r#"return UnitIsDead("target")"#).unwrap());
    assert_eq!(s.eval::<i64>(r#"return UnitHealth("target")"#).unwrap(), 0);
    // Name unknown (no name-query yet) → nil, the absent-name shape.
    assert!(s
        .eval::<bool>(r#"return UnitName("target") == nil"#)
        .unwrap());
}

#[test]
fn power_bindings_read_the_active_type() {
    let mut s = UiScript::new().unwrap();
    s.set_unit("player", Some(player())); // rage 35/100
    assert_eq!(
        s.eval::<(i64, String)>(r#"return UnitPowerType("player")"#)
            .unwrap(),
        (1, "RAGE".into())
    );
    assert_eq!(s.eval::<i64>(r#"return UnitPower("player")"#).unwrap(), 35);
    assert_eq!(
        s.eval::<i64>(r#"return UnitPowerMax("player")"#).unwrap(),
        100
    );
    // The active type asked for explicitly serves the value; another type serves 0 (v1 shape).
    assert_eq!(
        s.eval::<i64>(r#"return UnitPower("player", 1)"#).unwrap(),
        35
    );
    assert_eq!(
        s.eval::<i64>(r#"return UnitPower("player", 0)"#).unwrap(),
        0
    );
    // An absent unit: type reads as mana, values 0.
    assert_eq!(
        s.eval::<(i64, String)>(r#"return UnitPowerType("target")"#)
            .unwrap(),
        (0, "MANA".into())
    );
}

#[test]
fn get_money_reads_the_pushed_purse() {
    let mut s = UiScript::new().unwrap();
    // No feed yet: 0 copper.
    assert_eq!(s.eval::<i64>("return GetMoney()").unwrap(), 0);
    s.set_money(123_456);
    assert_eq!(s.eval::<i64>("return GetMoney()").unwrap(), 123_456);
}

#[test]
fn unit_xp_reads_the_player_globals_only_for_the_player_token() {
    let mut s = UiScript::new().unwrap();
    // No feed yet: 0/0.
    assert_eq!(s.eval::<i64>(r#"return UnitXP("player")"#).unwrap(), 0);
    assert_eq!(s.eval::<i64>(r#"return UnitXPMax("player")"#).unwrap(), 0);
    s.set_player_xp(4200, 6000);
    assert_eq!(s.eval::<i64>(r#"return UnitXP("player")"#).unwrap(), 4200);
    assert_eq!(
        s.eval::<i64>(r#"return UnitXPMax("player")"#).unwrap(),
        6000
    );
    // Any non-player token reports 0 (no creature/other unit exposes XP — the live shape).
    assert_eq!(s.eval::<i64>(r#"return UnitXP("target")"#).unwrap(), 0);
    assert_eq!(s.eval::<i64>(r#"return UnitXPMax("target")"#).unwrap(), 0);
}

#[test]
fn set_unit_none_removes_the_token() {
    let mut s = UiScript::new().unwrap();
    s.set_unit("player", Some(player()));
    assert!(s.eval::<bool>(r#"return UnitExists("player")"#).unwrap());
    s.set_unit("player", None);
    assert!(!s.eval::<bool>(r#"return UnitExists("player")"#).unwrap());
    assert_eq!(s.eval::<i64>(r#"return UnitHealth("player")"#).unwrap(), 0);
}

#[test]
fn party_frame_predicates_report_1_or_nil() {
    let mut s = UiScript::new().unwrap();
    s.set_unit(
        "party1",
        Some(UnitState {
            exists: true,
            is_connected: true,
            is_afk: true,
            is_dnd: false,
            pvp: true,
            is_pvp_ffa: false,
            ..Default::default()
        }),
    );
    assert_eq!(
        s.eval::<i64>(r#"return UnitIsConnected("party1")"#)
            .unwrap(),
        1
    );
    assert_eq!(s.eval::<i64>(r#"return UnitIsAFK("party1")"#).unwrap(), 1);
    assert!(s
        .eval::<bool>(r#"return UnitIsDND("party1") == nil"#)
        .unwrap());
    assert_eq!(s.eval::<i64>(r#"return UnitIsPVP("party1")"#).unwrap(), 1);
    assert!(s
        .eval::<bool>(r#"return UnitIsPVPFreeForAll("party1") == nil"#)
        .unwrap());

    // Flip every flag: the false/true cases aren't just "always true".
    s.set_unit(
        "party1",
        Some(UnitState {
            exists: true,
            is_connected: false,
            is_afk: false,
            is_dnd: true,
            pvp: false,
            is_pvp_ffa: true,
            ..Default::default()
        }),
    );
    assert!(s
        .eval::<bool>(r#"return UnitIsConnected("party1") == nil"#)
        .unwrap());
    assert!(s
        .eval::<bool>(r#"return UnitIsAFK("party1") == nil"#)
        .unwrap());
    assert_eq!(s.eval::<i64>(r#"return UnitIsDND("party1")"#).unwrap(), 1);
    assert!(s
        .eval::<bool>(r#"return UnitIsPVP("party1") == nil"#)
        .unwrap());
    assert_eq!(
        s.eval::<i64>(r#"return UnitIsPVPFreeForAll("party1")"#)
            .unwrap(),
        1
    );

    // An absent token reports nil regardless of any flag's zero value.
    assert!(s
        .eval::<bool>(r#"return UnitIsConnected("party2") == nil"#)
        .unwrap());
}

#[test]
fn unit_race_class_sex_report_the_snapshot_or_the_absent_shape() {
    let mut s = UiScript::new().unwrap();
    s.set_unit("player", Some(player()));
    assert_eq!(
        s.eval::<(String, String)>(r#"return UnitRace("player")"#)
            .unwrap(),
        ("Night Elf".into(), "NightElf".into())
    );
    assert_eq!(
        s.eval::<(String, String)>(r#"return UnitClass("player")"#)
            .unwrap(),
        ("Warrior".into(), "WARRIOR".into())
    );
    assert_eq!(s.eval::<i64>(r#"return UnitSex("player")"#).unwrap(), 3);
    // An absent token: nil, nil / nil — the live API's absent-unit shape.
    assert!(s
        .eval::<bool>(r#"local a, b = UnitRace("target") return a == nil and b == nil"#)
        .unwrap());
    assert!(s
        .eval::<bool>(r#"local a, b = UnitClass("target") return a == nil and b == nil"#)
        .unwrap());
    assert!(s
        .eval::<bool>(r#"return UnitSex("target") == nil"#)
        .unwrap());
    // A snapshot whose race/class haven't resolved yet (feed pending): same nils.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            ..Default::default()
        }),
    );
    assert!(s
        .eval::<bool>(r#"local a, b = UnitRace("target") return a == nil and b == nil"#)
        .unwrap());
    assert!(s
        .eval::<bool>(r#"return UnitSex("target") == nil"#)
        .unwrap());
}

/// `UnitFactionGroup` returns the (english, localized) pair the PvP-icon law reads, and `nil, nil`
/// for a unit with no side — the state the reference's `if ( factionGroup and … )` gate exists
/// for (decision 0646 §1). `TogglePVP` queues one toggle per call.
#[test]
fn faction_group_pair_and_the_pvp_toggle() {
    let mut s = UiScript::new().unwrap();
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            faction_group: Some("Horde".into()),
            ..Default::default()
        }),
    );
    let (english, localized) = s
        .eval::<(String, String)>(r#"return UnitFactionGroup("target")"#)
        .unwrap();
    assert_eq!((english.as_str(), localized.as_str()), ("Horde", "Horde"));

    // No side (a Monster/neutral template, or a unit whose template hasn't streamed).
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            faction_group: None,
            ..Default::default()
        }),
    );
    assert!(s
        .eval::<bool>(r#"local a, b = UnitFactionGroup("target") return a == nil and b == nil"#)
        .unwrap());
    // An absent unit answers the same way.
    assert!(s
        .eval::<bool>(r#"local a = UnitFactionGroup("focus") return a == nil"#)
        .unwrap());

    assert_eq!(s.take_pvp_toggles(), 0, "nothing queued yet");
    s.run("TogglePVP() TogglePVP()").unwrap();
    assert_eq!(
        s.take_pvp_toggles(),
        2,
        "one send per call, never collapsed"
    );
    assert_eq!(s.take_pvp_toggles(), 0, "the drain empties the queue");
}

/// `UnitClassification` (decision 0782, byte-verified table `0x850424`): five words indexed by the
/// gated rank, a STRING for every input — including an absent token, which the binary answers
/// `"normal"` for (its unresolved path loads index 0), never nil.
#[test]
fn unit_classification_reads_the_five_word_table() {
    let mut s = UiScript::new().unwrap();

    for (rank, word) in [
        (0, "normal"),
        (1, "elite"),
        (2, "rareelite"),
        (3, "worldboss"),
        (4, "rare"),
    ] {
        s.set_unit(
            "target",
            Some(UnitState {
                exists: true,
                rank,
                ..UnitState::default()
            }),
        );
        assert_eq!(
            s.eval::<String>(r#"return UnitClassification("target")"#)
                .unwrap(),
            word,
            "rank {rank}"
        );
    }

    // No snapshot at all, and a missing argument: still "normal", never nil. The target frame's
    // border branch compares this against string literals, so a nil here would be a Lua error the
    // reference cannot produce.
    s.set_unit("target", None);
    assert_eq!(
        s.eval::<String>(r#"return UnitClassification("target")"#)
            .unwrap(),
        "normal",
        "an absent unit is normal, not nil"
    );
    assert_eq!(
        s.eval::<String>(r#"return UnitClassification()"#).unwrap(),
        "normal",
        "a missing token is normal, not nil"
    );
}
