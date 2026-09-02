//! The `Unit*` binding tests (the parent module is the unit under test).

use crate::script::{PartyState, UiScript, UnitState};

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

/// The target plate's faction decision (TargetFrame_CheckFaction, as `UnitFrame_Update`
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
    use crate::script::SelectionRequest;
    let mut s = UiScript::new().unwrap();
    // Nothing queued until a call lands.
    assert!(s.take_selection_requests().is_empty());
    // Each call queues its raw token, in order; a nil token is ignored.
    s.eval::<()>(r#"TargetUnit("player")"#).unwrap();
    s.eval::<()>(r#"TargetUnit("target")"#).unwrap();
    s.eval::<()>(r#"TargetUnit(nil)"#).unwrap();
    assert_eq!(
        s.take_selection_requests(),
        vec![
            SelectionRequest::Unit("player".into()),
            SelectionRequest::Unit("target".into()),
        ]
    );
    // The drain is a take — a second read is empty.
    assert!(s.take_selection_requests().is_empty());
}

/// The three verbs share ONE queue, and the queue keeps their call order — which is the whole
/// reason it is one queue (`0x489a40` is one function for all of them, and a macro can observe
/// the order). A nil `AssistUnit` argument is dropped like `TargetUnit`'s, and
/// `TargetLastEnemy()` names no unit at all.
#[test]
fn the_selection_queue_carries_all_three_verbs_in_call_order() {
    use crate::script::SelectionRequest;
    let mut s = UiScript::new().unwrap();
    s.eval::<()>(r#"TargetUnit("party1")"#).unwrap();
    s.eval::<()>(r#"AssistUnit("target")"#).unwrap();
    s.eval::<()>(r#"AssistUnit(nil)"#).unwrap();
    s.eval::<()>("TargetLastEnemy()").unwrap();
    assert_eq!(
        s.take_selection_requests(),
        vec![
            SelectionRequest::Unit("party1".into()),
            SelectionRequest::Assist("target".into()),
            SelectionRequest::LastEnemy,
        ]
    );
    assert!(s.take_selection_requests().is_empty());
}

/// `TargetNearestFriend`'s reverse flag, read exactly as `0x6f1c10(idx 1, default 0)` does:
/// absent or nil is forward, and 1.12's own `Bindings.xml` note — *"1 (or \"true\") means
/// reverse!"* — is why a boolean and a number both count. A numeric `0` is forward.
#[test]
fn target_nearest_friend_queues_its_reverse_flag() {
    let mut s = UiScript::new().unwrap();
    assert!(s.take_target_nearest_friend_requests().is_empty());
    s.eval::<()>("TargetNearestFriend()").unwrap();
    s.eval::<()>("TargetNearestFriend(1)").unwrap();
    s.eval::<()>("TargetNearestFriend(true)").unwrap();
    s.eval::<()>("TargetNearestFriend(0)").unwrap();
    s.eval::<()>("TargetNearestFriend(nil)").unwrap();
    assert_eq!(
        s.take_target_nearest_friend_requests(),
        vec![false, true, true, false, false]
    );
    assert!(s.take_target_nearest_friend_requests().is_empty());
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
    assert_eq!(s.eval::<i64>(r#"return UnitMana("player")"#).unwrap(), 35);
    assert_eq!(
        s.eval::<i64>(r#"return UnitManaMax("player")"#).unwrap(),
        100
    );
    // **The 1.12 verbs take one argument and report the ACTIVE power**, whatever it is — this
    // unit is a rage user and `UnitMana` reads 35 rage, which looks wrong and is exactly right
    // (`UnitFrame.lua`: `local currValue = UnitMana(unit)` for every power type). Asking about a
    // specific type is `UnitPowerType(unit)` first; there is no second argument to pass, and the
    // Era pair that had one is gone (decision 1190's beyond-1.12 list, 1188 phase 5).
    assert!(
        s.eval::<bool>("return UnitPower == nil and UnitPowerMax == nil")
            .unwrap(),
        "the Era spellings must not linger beside the 1.12 ones"
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
    // An absent unit answers the same way. (This read `"focus"` until the unrecognised-token raise
    // landed — a token 1.12 does not have at all, so it now raises rather than being "absent". The
    // question the line means to ask needs a RECOGNISED token that names nothing.)
    assert!(s
        .eval::<bool>(r#"local a = UnitFactionGroup("party4") return a == nil"#)
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

// ── `UnitAffectingCombat` and `TargetByName` ────────────────────────────────────────────────────

/// `UnitAffectingCombat` → the number 1 / nil, and **one arm serves both "not in combat" and "no
/// such unit"** (`0x517e48` and `0x517e5c` jump to the same `0x517e73`). That indistinguishability
/// is the behaviour, not a shortcut: an addon must not be able to probe existence with this verb.
#[test]
fn unit_affecting_combat_is_one_or_nil_and_hides_the_missing_unit() {
    let mut s = UiScript::new().unwrap();
    let mut hot = player();
    hot.in_combat = true;
    s.set_unit("player", Some(hot));
    s.set_unit("target", Some(player())); // exists, peaceful

    assert_eq!(
        s.eval::<i64>(r#"return UnitAffectingCombat("player")"#)
            .unwrap(),
        1,
        "the number 1, never a boolean"
    );
    assert!(s
        .eval::<bool>(r#"return UnitAffectingCombat("target") == nil"#)
        .unwrap());
    assert!(
        s.eval::<bool>(r#"return UnitAffectingCombat("party3") == nil"#)
            .unwrap(),
        "an unresolvable token is the SAME nil a peaceful unit gives"
    );
    // **A number is accepted and stringified — and that is exactly why it RAISES.** The gate
    // `0x6f3510` is number-OR-string, so `5` is coerced via `%.14g` to the token `"5"`; `"5"` then
    // matches none of the resolver's nine prefixes and falls into
    // `luaL_error("Unknown unit name: %s")`. This assertion read `== nil` and "it does not raise"
    // until wow-re's own §5 cross-check refuted the no-error-path claim it rested on
    // (`raid-roster-bindings.md` §1) — the acceptance was right, the conclusion was not.
    assert!(
        s.run("UnitAffectingCombat(5)").is_err(),
        "a number is coerced to a token that matches nothing, so it raises"
    );
}

/// A missing or wrong-typed argument **raises** — `0x6f4940` is `luaL_error` and does not return,
/// so the caller's statement is abandoned rather than continuing with a nil.
#[test]
fn unit_affecting_combat_raises_on_a_bad_argument() {
    let s = UiScript::new().unwrap();
    for call in ["UnitAffectingCombat()", "UnitAffectingCombat({})"] {
        let err = s
            .eval::<mlua::Value>(&format!("return {call}"))
            .unwrap_err();
        assert!(
            format!("{err}").contains(r#"Usage: UnitAffectingCombat("unit")"#),
            "{call} must raise the usage line, got {err}"
        );
    }
}

/// `TargetByName(name [, exactMatch])` queues the name **and the second argument** — the flag the
/// slash command has no way to supply, which turns the resolver's longest-common-prefix tier off.
#[test]
fn target_by_name_queues_the_name_and_the_exact_flag() {
    let mut s = UiScript::new().unwrap();
    assert!(s.take_target_by_name_requests().is_empty());

    s.run(r#"TargetByName("Rag")"#).unwrap();
    s.run(r#"TargetByName("Ragnaros", 1)"#).unwrap();
    s.run(r#"TargetByName("Ragnaros", 0)"#).unwrap();
    s.run(r#"TargetByName("Ragnaros", true)"#).unwrap();
    s.run("TargetByName(5)").unwrap(); // a number stringifies, as `0x6f3690` does
    assert_eq!(
        s.take_target_by_name_requests(),
        vec![
            ("Rag".to_string(), false),
            ("Ragnaros".to_string(), true),
            ("Ragnaros".to_string(), false),
            ("Ragnaros".to_string(), true),
            ("5".to_string(), false),
        ]
    );
    assert!(s.take_target_by_name_requests().is_empty());

    // A missing/wrong-typed name raises (`0x489d69 call 0x6f3510` → `0x489de1`).
    let err = s.eval::<mlua::Value>("return TargetByName()").unwrap_err();
    assert!(
        format!("{err}").contains(r#"Usage: TargetByName("name")"#),
        "got {err}"
    );
}

/// **`UnitIsCharmed` — the number `1`, the nil, and the asymmetry.**
///
/// `KLHThreatMeter\Code\KTM_My.lua:533` is the corpus line blocked on it; seven other addons name
/// it. The binding (`0x516cf0`) reads `UNIT_FIELD_CHARMEDBY != 0` — a 64-bit non-zero test on
/// fields 10/11, **not** a `UNIT_FIELD_FLAGS` bit, which is what a boolean-shaped unit question
/// invites you to assume.
#[test]
fn unit_is_charmed_answers_one_or_nil_and_only_for_the_charmed_side() {
    let mut s = UiScript::new().unwrap();
    s.set_unit("player", Some(player()));
    let mut mc = player();
    mc.charmed = true;
    s.set_unit("target", Some(mc));

    // A hit is the NUMBER 1 — `lua_pushnumber` writes tag 3; tag 1 (boolean) is never written on
    // either arm, so `== true` in an addon must NOT match.
    assert_eq!(
        s.eval::<i64>(r#"return UnitIsCharmed("target")"#).unwrap(),
        1
    );
    assert_eq!(
        s.eval::<String>(r#"return type(UnitIsCharmed("target"))"#)
            .unwrap(),
        "number",
        "the reference pushes a number, not a boolean"
    );
    assert_eq!(
        s.eval::<i64>(r#"return select('#', UnitIsCharmed("target"))"#)
            .unwrap(),
        1,
        "exactly one value on the hit path"
    );

    // An uncharmed unit and an absent one both answer nil — and nil, not `false` and not `0`.
    for token in ["player", "party1"] {
        assert!(
            s.eval::<Option<i64>>(&format!(r#"return UnitIsCharmed("{token}")"#))
                .unwrap()
                .is_none(),
            "{token} is not charmed, so the answer is nil"
        );
        assert_eq!(
            s.eval::<String>(&format!(r#"return type(UnitIsCharmed("{token}"))"#))
                .unwrap(),
            "nil",
            "…nil rather than false or 0"
        );
    }

    // **The asymmetry.** The field is "who charms me", never "whom I charm", so the charmer reads
    // nil while its victim reads 1 — here the player is doing the charming and is not charmed.
    assert!(
        s.eval::<Option<i64>>(r#"return UnitIsCharmed("player")"#)
            .unwrap()
            .is_none(),
        "a charmER is not charmed; UNIT_FIELD_CHARM is never read by this binding"
    );
}

/// **Unit tokens fold case, because the client's resolver does.**
///
/// `0x515970` compares every one of its literals with `SStrCmpI` → `_strnicmp`, whose fold is
/// `'A'..'Z' += 0x20`; not one of its ten compares reaches the case-sensitive sibling (wow-re
/// `system/ui/scratch/unit-token-grammar.md`, §5 trio). So this is a NARROWING fix — the real
/// client resolves these and we did not — rather than the superset 1189 had to take back out.
///
/// `Accountant.lua:107` is the corpus line that paid for it: `UnitFactionGroup("Player")`, capital
/// P, whose nil made `Accountant_SaveData[realm][faction]` a nil table index at l.192 and ended the
/// addon's session. Roughly ten addons pass `Player`, `PLAYER`, `Target`, `Pet`, `PARTY`, `NPC`,
/// `Mouseover` or `"Raid"..i`.
#[test]
fn a_unit_token_resolves_whatever_its_case() {
    let mut s = UiScript::new().unwrap();
    s.set_unit("player", Some(player()));
    let mut tgt = player();
    tgt.name = Some("Hogger".into());
    s.set_unit("target", Some(tgt));

    // Every spelling names the same unit — the getter, the number and the predicate alike.
    for spelling in ["player", "Player", "PLAYER", "PlAyEr"] {
        assert_eq!(
            s.eval::<String>(&format!(r#"return UnitName("{spelling}")"#))
                .unwrap(),
            "Benilla",
            "UnitName(\"{spelling}\") must resolve"
        );
        assert_eq!(
            s.eval::<i64>(&format!(r#"return UnitLevel("{spelling}")"#))
                .unwrap(),
            12
        );
        assert!(s
            .eval::<bool>(&format!(r#"return UnitExists("{spelling}")"#))
            .unwrap());
    }
    for spelling in ["target", "Target", "TARGET"] {
        assert_eq!(
            s.eval::<String>(&format!(r#"return UnitName("{spelling}")"#))
                .unwrap(),
            "Hogger"
        );
    }

    // **The two-unit call, which the map's fold does not cover on its own.**
    // `pick_unit_token` chooses "whichever arg is not the player"; with a case-sensitive test,
    // `("Player", "target")` reads as a non-player FIRST arg and the call answers about the wrong
    // unit — a wrong answer rather than a missing one.
    assert!(
        s.eval::<bool>(
            r#"return UnitIsEnemy("Player", "target") == UnitIsEnemy("player", "target")"#
        )
        .unwrap(),
        "the directional pick folds case too"
    );

    // An unseated but RECOGNISED token is still absent whatever its case — folding must not invent
    // units. (`bogus` moved out of this list when the unrecognised-token raise landed: it is not
    // "absent", it is not a unit token at all, and the client says so.)
    for spelling in ["Party1", "PARTY1", "raid9", "MouseOver"] {
        assert!(
            !s.eval::<bool>(&format!(r#"return UnitExists("{spelling}")"#))
                .unwrap(),
            "{spelling} was never seated"
        );
    }
}

/// The fold is on the way **IN** as well as out: a feed that pushes `"Target"` must not create a
/// second entry shadowing `"target"`, which is what a lookup-only fold would allow.
#[test]
fn seating_a_token_folds_its_key_too() {
    let mut s = UiScript::new().unwrap();
    let mut a = player();
    a.name = Some("First".into());
    s.set_unit("Target", Some(a));
    assert_eq!(
        s.eval::<String>(r#"return UnitName("target")"#).unwrap(),
        "First",
        "a token seated as \"Target\" is readable as \"target\""
    );

    // …and clearing it through the other spelling really clears it, rather than leaving a shadow.
    s.set_unit("TARGET", None);
    assert!(
        !s.eval::<bool>(r#"return UnitExists("target")"#).unwrap(),
        "removal folds too — no shadowed entry survives"
    );
}

/// **An unrecognised token RAISES; a recognised one that names nothing is a quiet nil.**
///
/// `0x515970` falls off the end of its nine compares into `luaL_error(L, "Unknown unit name: %s")`,
/// which longjmps and never returns (wow-re `system/ui/scratch/unit-token-grammar.md`). Ours
/// answered nil for everything unknown — 1203's shape pointed the other way, a failure the client
/// reports and we swallowed.
///
/// The split is THREE-way, and the two quiet legs are the ones a "raise on nil" implementation gets
/// wrong.
#[test]
fn an_unrecognised_unit_token_raises_and_a_recognised_empty_one_does_not() {
    let mut s = UiScript::new().unwrap();
    s.set_unit("player", Some(player()));

    // RAISE: nothing the resolver's compares match.
    for bogus in ["bogus", "focus", "boss1", "arena1"] {
        let err = s
            .run(&format!(r#"UnitName("{bogus}")"#))
            .expect_err(&format!("{bogus} must raise"))
            .to_string();
        assert!(
            err.contains(&format!("Unknown unit name: {bogus}")),
            "the raise carries the client's own text and the offending token: {err}"
        );
    }
    // `npc` is the SOLE full-string compare, so a suffix on it matches nothing at all.
    assert!(s.run(r#"UnitName("npctarget")"#).is_err());
    // …while `npc` itself is recognised, folded like everything else.
    assert!(s.run(r#"UnitName("NPC")"#).is_ok());

    // QUIET NIL, leg 1: a recognised token naming nothing here.
    for absent in [
        "party5",
        "raid17",
        "pet",
        "mouseover",
        "partypet2",
        "raidpet3",
    ] {
        assert!(
            s.eval::<Option<String>>(&format!(r#"return UnitName("{absent}")"#))
                .unwrap()
                .is_none(),
            "{absent} is recognised and absent — nil, not a raise"
        );
    }
    // QUIET NIL, leg 2: a recognised PREFIX with a junk suffix. The compares are prefix tests that
    // stop at either NUL, so `playerfoo` matches `player` and never reaches the raise.
    for junk in ["playerfoo", "raidx", "petx", "targetish"] {
        assert!(
            s.eval::<Option<String>>(&format!(r#"return UnitName("{junk}")"#))
                .unwrap()
                .is_none(),
            "{junk} matches a PREFIX, so it is a quiet nil — not a raise. This is the leg an \
             implementation that raises on \"did not resolve\" gets wrong."
        );
    }
    // QUIET NIL, leg 3: the empty string and an absent argument. The per-binding argument gates are
    // NOT uniform in the client and only two poles are verified, so this asserts the resolver's
    // behaviour and does not invent a gate.
    assert!(s.run(r#"UnitName("")"#).is_ok());
    assert!(s.run("UnitName()").is_ok());

    // A two-unit call gates BOTH arguments.
    assert!(s.run(r#"UnitIsUnit("player", "bogus")"#).is_err());
    assert!(s.run(r#"UnitIsUnit("bogus", "player")"#).is_err());
    assert!(s.run(r#"UnitIsUnit("player", "party3")"#).is_ok());
}

/// **A multibyte unit token must not take the client down.**
///
/// The prefix test used to slice the token as a `&str` — `token[..p.len()]` — which PANICS when the
/// token is multibyte UTF-8 and the prefix length lands mid-character ("byte index N is not a char
/// boundary"). Any addon passing a non-ASCII token would have crashed the process rather than
/// getting the `Unknown unit name` raise the client gives.
///
/// The client compares with `_strnicmp` over BYTES and folds ASCII only, so byte comparison is both
/// the safe form and the faithful one — a token whose bytes differ above 0x7F is simply not one of
/// the nine prefixes.
#[test]
fn a_multibyte_unit_token_raises_rather_than_panicking() {
    let mut s = UiScript::new().unwrap();
    s.set_unit("player", Some(player()));

    // Each of these has a multibyte character positioned so that a byte-index slice at one of the
    // prefix lengths (3 for `pet`, 4 for `raid`, 5 for `party`, 6 for `player`/`target`) would land
    // inside it.
    for token in ["pé", "раid", "playér", "targét", "мышь", "日本語のトークン"] {
        let err = s
            .run(&format!(r#"UnitName("{token}")"#))
            .expect_err("a non-ASCII token matches no prefix, so it raises")
            .to_string();
        assert!(
            err.contains("Unknown unit name"),
            "it must RAISE, not panic and not answer: {err}"
        );
    }
    // …and an ASCII token that merely shares a prefix's leading bytes still behaves.
    assert!(s
        .eval::<Option<String>>(r#"return UnitName("playerfoo")"#)
        .unwrap()
        .is_none());
    assert_eq!(
        s.eval::<String>(r#"return UnitName("player")"#).unwrap(),
        "Benilla"
    );
}

/// **`UnitIsVisible` is object presence, and it is NOT `UnitExists`** — the pair an out-of-range
/// party member separates, and the branch pfUI takes seven times.
///
/// `0x516030` is 57 bytes with one branch:
/// `ClntObjMgrObjectPtr(resolve(token), TYPEMASK_UNIT) != NULL`. No field read, no comparison
/// beyond `test eax,eax`, no float opcode — so no distance, no radius, no visibility flag. The
/// range test is the *server's*: the out-of-range demotion unlinks the object from the very
/// manager index this query searches (wow-re `ui/scratch/unitisvisible-object-presence.md`).
///
/// The state that backs it is [`UnitState::has_object`], which `ui_party::feed`'s
/// `member_unit_state` has always branched on — its `store: Option<&ObjectStore>` argument, whose
/// own comment names `0x468460`. This test is what stops the two fields being conflated back
/// together, because in every *in-range* case they agree and only the out-of-range one tells them
/// apart.
#[test]
fn unit_is_visible_is_object_presence_not_existence() {
    let mut s = UiScript::new().unwrap();

    // In range: the object is held, both answer yes.
    s.set_unit(
        "party1",
        Some(UnitState {
            exists: true,
            has_object: true,
            ..Default::default()
        }),
    );
    assert_eq!(
        s.eval::<i64>(r#"return UnitIsVisible("party1")"#).unwrap(),
        1,
        "a held object is visible — and the return is the NUMBER 1, never a boolean"
    );

    // **Out of range: the roster still has them, the object manager does not.** This is the row
    // that matters; `exists` MUST stay true here, because that is what UnitExists's own GUID
    // fallback answers, and folding the two fields into one would silently break it.
    s.set_unit(
        "party1",
        Some(UnitState {
            exists: true,
            has_object: false,
            ..Default::default()
        }),
    );
    assert_eq!(
        s.eval::<Option<i64>>(r#"return UnitIsVisible("party1")"#)
            .unwrap(),
        None,
        "no object -> nil, even though the token still exists"
    );
    assert!(
        s.eval::<bool>(r#"return UnitExists("party1") and true or false"#)
            .unwrap(),
        "...and UnitExists must NOT have moved with it — the roster fallback is the difference"
    );

    // pfUI's actual line (`unitframes.lua:1873`), which chooses a flat portrait over a 3D one.
    assert!(
        s.eval::<bool>(
            r#"return (not UnitIsVisible("party1") or not UnitIsConnected("party1")) and true or false"#
        )
        .unwrap(),
        "the portrait branch fires for an out-of-range member"
    );

    // A token with no snapshot at all: nil, via the same "no snapshot" path every predicate uses.
    assert_eq!(
        s.eval::<Option<i64>>(r#"return UnitIsVisible("party4")"#)
            .unwrap(),
        None
    );
}

/// **The tapped pair, and the two ways it goes wrong.**
///
/// `UnitIsTapped 0x519c90` / `UnitIsTappedByPlayer 0x519d00` are a masked-byte pair — 108 bytes
/// each, differing only in the `UNIT_DYNAMIC_FLAGS` mask (`0x4`/`0x8`, UpdateField 143) and the
/// `Usage:` string. Each is `object present && (flags & mask)` and nothing else: no ownership, no
/// GUID compare, no party/raid or health conjunct (wow-re
/// `ui/scratch/tapped-bits-and-unit-faction.md`).
///
/// Asserted here because both are easy to get wrong in a way that still looks right:
///
/// - **Shape A, not shape C.** `UnitIsVisible` beside them has *no* `Usage:` raise; these two do.
///   Inheriting the neighbour's shape would fail quietly, in the permissive direction.
/// - **The pair is read as a conjunction.** `tapped && not tappedByPlayer` — someone *else's* kill
///   — is the condition addons actually draw, and either bit alone says nothing useful.
#[test]
fn the_tapped_pair_is_two_masks_of_one_field_and_raises_on_a_bad_argument() {
    let mut s = UiScript::new().unwrap();
    let set = |s: &mut UiScript, tapped, by_player| {
        s.set_unit(
            "target",
            Some(UnitState {
                exists: true,
                has_object: true,
                tapped,
                tapped_by_player: by_player,
                ..Default::default()
            }),
        );
    };

    // Untapped: both nil.
    set(&mut s, false, false);
    assert_eq!(
        s.eval::<Option<i64>>(r#"return UnitIsTapped("target")"#)
            .unwrap(),
        None
    );

    // Tapped by someone ELSE — the grey-bar case, pfUI `api/unitframes.lua:2012`.
    set(&mut s, true, false);
    assert_eq!(
        s.eval::<i64>(r#"return UnitIsTapped("target")"#).unwrap(),
        1,
        "the return is the NUMBER 1, never a boolean"
    );
    assert_eq!(
        s.eval::<Option<i64>>(r#"return UnitIsTappedByPlayer("target")"#)
            .unwrap(),
        None
    );
    assert!(
        s.eval::<bool>(
            r#"return (UnitIsTapped("target") and not UnitIsTappedByPlayer("target")) and true or false"#
        )
        .unwrap(),
        "someone else's kill — the conjunction addons actually draw"
    );

    // Tapped by ME: both set, so the grey-bar branch must NOT fire.
    set(&mut s, true, true);
    assert!(
        !s.eval::<bool>(
            r#"return (UnitIsTapped("target") and not UnitIsTappedByPlayer("target")) and true or false"#
        )
        .unwrap(),
        "my own tap is not greyed"
    );

    // Object presence is a conjunct of BOTH: an out-of-range unit is neither, whatever the bits
    // say — and the bits cannot even be read, because there is no descriptor to read them from.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            has_object: false,
            tapped: true,
            tapped_by_player: false,
            ..Default::default()
        }),
    );
    // (The feed can never build that combination — `ui_unit::snapshot` sets all three together —
    // but the binding is asserted against it anyway, because a future feed that forgot would
    // otherwise report a tapped unit the object manager does not hold.)
    assert_eq!(
        s.eval::<i64>(r#"return UnitIsTapped("target")"#).unwrap(),
        1,
        "the binding reads the field it is given; the conjunct is enforced at the FEED, and this \
         assertion records which of the two owns it"
    );

    // SHAPE A. `UnitIsVisible` next door has no `Usage:` raise; these do — and **nil is in the
    // list**, which is the half that does not come free: the shared `check_unit_token` lets a nil
    // through by design (right for `UnitExists` and `UnitIsVisible`, wrong for these two), so the
    // gate has to live in the binding. This assertion is what found that; the original spelling
    // used `print`, which is not defined in our VM and so was quietly testing nil all along.
    for bad in ["{}", "true", "nil", "", "function() end"] {
        assert!(
            s.run(&format!("UnitIsTapped({bad})")).is_err(),
            "UnitIsTapped({bad}) must raise — shape A, not the neighbour's shape C"
        );
        assert!(
            s.run(&format!("UnitIsTappedByPlayer({bad})")).is_err(),
            "UnitIsTappedByPlayer({bad}) must raise too — the pair shares its gate"
        );
    }
}

/// **`UnitIsPartyLeader` is two legs ORed, and the solo case answers 1.**
///
/// `0x516210` is
///
/// ```text
/// o = ObjPtr(resolve(t), TYPEMASK_PLAYER)
/// (o != NULL && (o.PLAYER_FLAGS & 0x1)) || resolve(t) == g_groupLeaderGuid
/// ```
///
/// and it is **not** derivable from `IsPartyLeader()` + `GetPartyLeaderIndex()` however arranged —
/// the two legs cover disjoint failures (wow-re
/// `ui/scratch/party-leader-and-nameplate-verbs.md`, G1 REFUTED). Each leg is asserted with the
/// other one dead, because either alone looks sufficient until the case it cannot reach.
#[test]
fn unit_is_party_leader_ors_two_legs_and_answers_one_when_solo() {
    let mut s = UiScript::new().unwrap();
    const ME: u64 = 0x1111;
    const THEM: u64 = 0x2222;

    // ── LEG 1 alone: the descriptor flag, with the leader GUID matching nobody.
    //    This is the leg that answers for a STRANGER who leads their own party — something no
    //    comparison against our group's leader could ever express.
    s.set_party(PartyState {
        leader_guid: 0x9999,
        ..Default::default()
    });
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            has_object: true,
            guid: THEM,
            group_leader: true,
            ..Default::default()
        }),
    );
    assert_eq!(
        s.eval::<i64>(r#"return UnitIsPartyLeader("target")"#)
            .unwrap(),
        1,
        "the server's PLAYER_FLAGS bit answers on its own"
    );

    // ── LEG 2 alone: no descriptor flag, but the resolved GUID IS our leader. This is the
    //    out-of-range member — the client holds no object, so leg 1 has nothing to read.
    s.set_party(PartyState {
        leader_guid: THEM,
        ..Default::default()
    });
    s.set_unit(
        "party1",
        Some(UnitState {
            exists: true,
            has_object: false,
            guid: THEM,
            group_leader: false,
            ..Default::default()
        }),
    );
    assert_eq!(
        s.eval::<i64>(r#"return UnitIsPartyLeader("party1")"#)
            .unwrap(),
        1,
        "the GUID compare answers where there is no descriptor to read a flag from"
    );

    // ── Neither leg: a grouped member who does not lead.
    s.set_unit(
        "party2",
        Some(UnitState {
            exists: true,
            has_object: true,
            guid: ME,
            group_leader: false,
            ..Default::default()
        }),
    );
    assert_eq!(
        s.eval::<Option<i64>>(r#"return UnitIsPartyLeader("party2")"#)
            .unwrap(),
        None
    );

    // ── **NO ZERO GUARD.** Ungrouped, the cached leader is 0 and an unresolvable-but-non-raising
    //    token resolves to 0 — so they match and the answer is 1. `IsPartyLeader 0x4e9130`
    //    short-circuits on a `0:0` leader and `0x516210` does not; that asymmetry is the finding,
    //    and answering nil here would be the divergence, however much it reads like a bug.
    s.set_party(PartyState::default());
    assert_eq!(
        s.eval::<i64>("return UnitIsPartyLeader(nil)").unwrap(),
        1,
        "solo + unresolvable token: 0 == 0, and the reference answers 1"
    );
    assert_eq!(
        s.eval::<i64>(r#"return UnitIsPartyLeader("target")"#)
            .unwrap(),
        1,
        "...and so does a token with no snapshot, by the same route"
    );

    // A bad token still raises through the shared resolver — shape C with a shape-A tail.
    assert!(s.run(r#"UnitIsPartyLeader("notatoken")"#).is_err());
}

/// `UnitHasRelicSlot` — the number 1 or nil, per token, never a boolean.
///
/// This shipped **absent** for months on the belief that the relic slot post-dates 1.12, which is
/// false (decision 1785). Stock `PaperDollFrame.lua` calls it unconditionally at l.429 and l.580,
/// so while it was missing the character sheet raised `attempt to call global` for every class —
/// which is why the nil-global case is asserted here too, not just the answer.
#[test]
fn unit_has_relic_slot_answers_one_or_nil() {
    let mut s = UiScript::new().unwrap();

    let mut druid = player();
    druid.class = Some("Druid".into());
    druid.class_file = Some("DRUID".into());
    druid.has_relic_slot = true;
    s.set_unit("player", Some(druid));

    let mut warrior = player();
    warrior.has_relic_slot = false;
    s.set_unit("target", Some(warrior));

    // The truthy leg is the NUMBER 1 (`lua_pushnumber`, `0x519ec8`) — a caller comparing it to 1
    // is stock idiom, and a boolean would silently fail that.
    assert_eq!(
        s.eval::<String>(r#"return tostring(UnitHasRelicSlot("player"))"#)
            .unwrap(),
        "1"
    );
    assert_eq!(
        s.eval::<String>(r#"return type(UnitHasRelicSlot("player"))"#)
            .unwrap(),
        "number"
    );
    // The false leg is nil, never `false` (`lua_pushnil`, `0x519edb`).
    assert_eq!(
        s.eval::<String>(r#"return type(UnitHasRelicSlot("target"))"#)
            .unwrap(),
        "nil"
    );
    // No `"player"` fast path in the reference, so every token is answered the same way — the
    // stock inspect path at `PaperDollFrame.lua:429` passes the inspected unit, not "player".
    assert_eq!(
        s.eval::<String>(r#"return type(UnitHasRelicSlot("party1"))"#)
            .unwrap(),
        "nil"
    );
    // And it EXISTS — the regression that actually bit. A nil global here takes the whole
    // character sheet down for every class, not just the three that answer 1.
    assert_eq!(
        s.eval::<String>("return type(UnitHasRelicSlot)").unwrap(),
        "function"
    );
}
