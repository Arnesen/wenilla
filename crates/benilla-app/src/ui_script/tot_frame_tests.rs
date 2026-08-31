//! The shipped target-of-target frame, driven end to end (decision 1576) — `UnitFrames.xml`'s
//! `TargetofTargetFrame` over synthetic `"targettarget"` snapshots and the events the app's feed
//! fires for that token.
//!
//! Two things here are not ordinary unit-frame plumbing and are what most of these test:
//!
//! * **The visibility law is six gates deep** — a switch, five display modes, and the four unit
//!   tests the reference wraps them in — and a frame that gets any of them wrong is either always
//!   there or never there. The mode leg is also where we knowingly leave the reference: its solo
//!   arm calls neither `Show` nor `Hide` when you are in a raid, so the frame keeps whatever state
//!   it had; ours answers the question the option asks.
//! * **A token going away is silent.** The feed clears `"targettarget"` without an event, by the
//!   same convention `"target"` uses, so the frame's own events cannot take it down — that is what
//!   `TargetFrame_OnUpdate`'s one-compare reconcile is for, and it has a test.

use benilla_ui::script::{
    AuraState, PartyMemberInfo, PartyState, QuadContent, RaidMemberInfo, ScriptValue,
    SelectionRequest, UiScript, UnitState,
};

/// Load one shipped `assets/ui/<file>`, panicking on any loader error (the unit-frame tests').
fn load_xml(s: &UiScript, file: &str) {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/ui")
            .join(file),
    )
    .unwrap();
    let doc = benilla_ui::framexml::parse(&text).unwrap();
    let report = benilla_ui::loader::load(s, &doc, &|_| None);
    assert!(
        report.errors.is_empty(),
        "{file}: loader errors: {:?}",
        report.errors
    );
}

/// A unit snapshot with a guid — the identity `UnitIsUnit` compares, which the "your target is
/// you" gate reads.
fn unit(name: &str, guid: u64, health: u32) -> UnitState {
    UnitState {
        exists: true,
        is_connected: true,
        name: Some(name.into()),
        guid,
        health,
        max_health: 100,
        level: 60,
        power_type: 0,
        power: 50,
        max_power: 100,
        ..UnitState::default()
    }
}

/// The frame's production load prefix, plus a player, a target and a target's target — the state
/// in which everything but the switch itself already allows the frame.
///
/// The tail three are the target-aura tests' set and are here for the same reasons: `BuffFrame`
/// defines `DebuffTypeColor`, which the debuff row's dispel tint indexes, and it will not TICK
/// without `ActionBar`'s `TOOLTIP_UPDATE_TIME` — which this file's reconcile test needs, since it
/// is the only one here that runs the clock.
fn load_tot() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UIParent.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "UIDropDownMenu.xml");
    load_xml(&s, "UnitPopup.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "ActionBar.xml");
    load_xml(&s, "UnitFrames.xml");
    load_xml(&s, "BuffFrame.xml");
    s.set_unit("player", Some(unit("Tri", 0x100, 100)));
    s.set_unit("target", Some(unit("Kobold Miner", 0x200, 80)));
    s.set_unit("targettarget", Some(unit("Tri", 0x100, 100)));
    s.fire_event("PLAYER_TARGET_CHANGED", vec![]);
    s
}

/// Turn the frame on the way the option row does — the write plus its `applyFunc`.
fn switch_on(s: &mut UiScript) {
    s.run(r#"SHOW_TARGET_OF_TARGET = "1" TargetofTarget_Update()"#)
        .unwrap();
}

fn shown(s: &mut UiScript) -> bool {
    s.eval::<bool>("return TargetofTargetFrame:IsShown() and true or false")
        .unwrap()
}

/// Every texture path the UI actually draws this frame (the pet-frame tests' render probe).
fn drawn(s: &mut UiScript) -> Vec<String> {
    s.resolve();
    s.extract()
        .into_iter()
        .filter_map(|q| match q.content {
            QuadContent::Texture { path: Some(p), .. } => Some(p),
            _ => None,
        })
        .collect()
}

fn debuff(spell_id: u32, name: &str, debuff_type: Option<&str>) -> AuraState {
    AuraState {
        spell_id,
        name: Some(name.into()),
        icon: Some(format!("Interface\\Icons\\Spell_{spell_id}")),
        count: 1,
        debuff_type: debuff_type.map(str::to_string),
        // No unit but yourself carries a duration on the 1.12 wire (decision 0257 B6).
        duration: 0.0,
        expiration_time: 0.0,
        helpful: false,
        cancelable: false,
        until_cancelled: false,
        channeled: false,
    }
}

/// A party of `n` others; `GetNumPartyMembers` is that list's length.
fn party(n: usize) -> PartyState {
    PartyState {
        members: (0..n)
            .map(|i| PartyMemberInfo {
                name: format!("Mate{i}"),
                guid: 0x300 + i as u64,
            })
            .collect(),
        ..PartyState::default()
    }
}

/// A raid of `n` (the roster INCLUDES the player, per `GetRaidRosterInfo`'s array).
fn raid(n: usize) -> PartyState {
    PartyState {
        raid: (0..n)
            .map(|i| RaidMemberInfo {
                name: format!("Raider{i}"),
                guid: 0x400 + i as u64,
                ..RaidMemberInfo::default()
            })
            .collect(),
        ..PartyState::default()
    }
}

/// The switch is the outer gate, and it ships OFF — 1.12's own default (`SHOW_TARGET_OF_TARGET`
/// = `"0"`, ref UIOptionsFrame.lua l.116). Everything else about the state already allows the
/// frame, so this is the switch alone.
#[test]
fn the_frame_ships_off_and_the_switch_is_what_shows_it() {
    let mut s = load_tot();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert_eq!(
        s.eval::<String>("return SHOW_TARGET_OF_TARGET").unwrap(),
        "0",
        "the shipped default is the reference's: off"
    );
    assert!(
        !shown(&mut s),
        "off means hidden with everything else ready"
    );

    switch_on(&mut s);
    assert!(shown(&mut s), "the switch alone brings it up");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The four unit gates the reference wraps the switch in (ref TargetofTarget_Update l.504): a
/// target, a target OF it, a target that is not you, and a target that is alive.
#[test]
fn the_four_unit_gates_each_take_the_frame_down() {
    let mut s = load_tot();
    switch_on(&mut s);
    assert!(shown(&mut s));

    // No target of target.
    s.set_unit("targettarget", None);
    s.run("TargetofTarget_Update()").unwrap();
    assert!(!shown(&mut s), "nothing to show");
    s.set_unit("targettarget", Some(unit("Tri", 0x100, 100)));

    // No target at all.
    s.set_unit("target", None);
    s.run("TargetofTarget_Update()").unwrap();
    assert!(!shown(&mut s), "no target, no target's target");
    s.set_unit("target", Some(unit("Kobold Miner", 0x200, 80)));

    // The target is YOU: its target is your own target, and the frame would restate the frame it
    // hangs off.
    s.set_unit("target", Some(unit("Tri", 0x100, 100)));
    s.run("TargetofTarget_Update()").unwrap();
    assert!(!shown(&mut s), "self-target: nothing to add");
    s.set_unit("target", Some(unit("Kobold Miner", 0x200, 80)));

    // A dead target.
    s.set_unit("target", Some(unit("Kobold Miner", 0x200, 0)));
    s.run("TargetofTarget_Update()").unwrap();
    assert!(!shown(&mut s), "a corpse is fighting nobody");

    s.set_unit("target", Some(unit("Kobold Miner", 0x200, 80)));
    s.run("TargetofTarget_Update()").unwrap();
    assert!(shown(&mut s), "and back");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The five display modes, each against solo / party / raid. Mode 3 in a raid is the one that
/// diverges from the reference on purpose: its ladder calls neither Show nor Hide there and the
/// frame keeps its last state (ref l.513-520), which is a leak rather than a rule.
#[test]
fn the_five_modes_answer_solo_party_and_raid() {
    let mut s = load_tot();
    switch_on(&mut s);

    let states = [
        ("solo", PartyState::default()),
        ("party", party(2)),
        ("raid", raid(10)),
    ];
    // mode -> (solo, party, raid)
    let expected = [
        ("1", [false, false, true]), // raid only
        ("2", [false, true, false]), // party, and not while raiding
        ("3", [true, false, false]), // solo only
        ("4", [false, true, true]),  // grouped at all
        ("5", [true, true, true]),   // always
    ];
    for (mode, want) in expected {
        for (i, (label, state)) in states.iter().enumerate() {
            s.set_party(state.clone());
            s.run(&format!(
                r#"SHOW_TARGET_OF_TARGET_STATE = "{mode}" TargetofTarget_Update()"#
            ))
            .unwrap();
            assert_eq!(
                shown(&mut s),
                want[i],
                "mode {mode} while {label}: expected shown={}",
                want[i]
            );
        }
    }
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Mode 3 in a raid, specifically as a STATE question rather than a fresh evaluation: the frame
/// must come down when the raid forms, not keep whatever it had. This is the reference's leaked
/// branch, and it can only be seen by showing the frame first.
#[test]
fn the_solo_mode_comes_down_when_a_raid_forms() {
    let mut s = load_tot();
    switch_on(&mut s);
    s.run(r#"SHOW_TARGET_OF_TARGET_STATE = "3" TargetofTarget_Update()"#)
        .unwrap();
    assert!(shown(&mut s), "solo, in solo mode");

    s.set_party(raid(10));
    s.fire_event("RAID_ROSTER_UPDATE", vec![]);
    assert!(!shown(&mut s), "the raid forms and the frame goes away");
}

/// The paint: name, health, and the powerless unit's hidden mana bar (this file's convention on
/// every frame — the reference leaves an empty 0/0 rail drawn).
#[test]
fn the_frame_paints_its_unit() {
    let mut s = load_tot();
    switch_on(&mut s);

    assert_eq!(
        s.eval::<String>("return TargetofTargetName:GetText()")
            .unwrap(),
        "Tri"
    );
    let (value, max) = s
        .eval::<(f64, f64)>(
            "local _, m = TargetofTargetHealthBar:GetMinMaxValues() \
             return TargetofTargetHealthBar:GetValue(), m",
        )
        .unwrap();
    assert_eq!((value, max), (100.0, 100.0));
    assert!(
        s.eval::<bool>("return TargetofTargetManaBar:IsShown() and true or false")
            .unwrap(),
        "a unit with mana shows its rail"
    );

    let mut powerless = unit("Skeleton", 0x100, 100);
    powerless.max_power = 0;
    s.set_unit("targettarget", Some(powerless));
    s.fire_event(
        "UNIT_MAXPOWER",
        vec![ScriptValue::Str("targettarget".into())],
    );
    assert!(
        !s.eval::<bool>("return TargetofTargetManaBar:IsShown() and true or false")
            .unwrap(),
        "no power, no rail"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// DEAD over a dimmed trough — and the reference's connected test, which is what tells a corpse
/// from a linkdead player (both read zero health).
#[test]
fn the_dead_word_needs_a_connected_corpse() {
    let mut s = load_tot();
    switch_on(&mut s);
    assert!(
        !s.eval::<bool>("return TargetofTargetDeadText:IsShown() and true or false")
            .unwrap(),
        "alive: no word"
    );

    let mut corpse = unit("Tri", 0x100, 0);
    corpse.dead = true;
    s.set_unit("targettarget", Some(corpse.clone()));
    s.fire_event("UNIT_HEALTH", vec![ScriptValue::Str("targettarget".into())]);
    assert!(
        s.eval::<bool>("return TargetofTargetDeadText:IsShown() and true or false")
            .unwrap(),
        "dead: the word"
    );

    let mut linkdead = corpse;
    linkdead.is_connected = false;
    s.set_unit("targettarget", Some(linkdead));
    s.fire_event("UNIT_HEALTH", vec![ScriptValue::Str("targettarget".into())]);
    assert!(
        !s.eval::<bool>("return TargetofTargetDeadText:IsShown() and true or false")
            .unwrap(),
        "disconnected, not dead"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The portrait tint (ref TargetofTargetHealthCheck), and its PLAYERS-ONLY gate: a creature's
/// portrait is never tinted, which is why the check has to run off the bar's own value rather
/// than off the snapshot.
#[test]
fn the_portrait_tints_with_a_players_state_and_never_a_creatures() {
    let mut s = load_tot();
    switch_on(&mut s);

    let mut hurt = unit("Tri", 0x100, 15);
    hurt.is_player = true;
    s.set_unit("targettarget", Some(hurt));
    s.fire_event("UNIT_HEALTH", vec![ScriptValue::Str("targettarget".into())]);
    let (r, g, b) = s
        .eval::<(f64, f64, f64)>("return TargetofTargetPortrait:GetVertexColor()")
        .unwrap();
    assert_eq!((r, g, b), (1.0, 0.0, 0.0), "a player under a fifth: red");

    let mut healthy = unit("Tri", 0x100, 90);
    healthy.is_player = true;
    s.set_unit("targettarget", Some(healthy));
    s.fire_event("UNIT_HEALTH", vec![ScriptValue::Str("targettarget".into())]);
    let (r, g, b) = s
        .eval::<(f64, f64, f64)>("return TargetofTargetPortrait:GetVertexColor()")
        .unwrap();
    assert_eq!((r, g, b), (1.0, 1.0, 1.0), "and white again");

    // A creature at the same 15%: the tint must not move (the check returns before touching it).
    s.set_unit("targettarget", Some(unit("Kobold", 0x500, 15)));
    s.fire_event("UNIT_HEALTH", vec![ScriptValue::Str("targettarget".into())]);
    let (r, g, b) = s
        .eval::<(f64, f64, f64)>("return TargetofTargetPortrait:GetVertexColor()")
        .unwrap();
    assert_eq!((r, g, b), (1.0, 1.0, 1.0), "creatures carry no reading");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The left click selects — the one leg the target frame's own click does not have, and the whole
/// reason the frame is clickable.
#[test]
fn the_left_click_targets_the_unit() {
    let mut s = load_tot();
    switch_on(&mut s);
    s.run(r#"TargetofTarget_OnClick("LeftButton")"#).unwrap();
    assert_eq!(
        s.take_selection_requests(),
        vec![SelectionRequest::Unit("targettarget".into())]
    );

    // A right click is not a menu here: the reference gives this frame none.
    s.run(r#"TargetofTarget_OnClick("RightButton")"#).unwrap();
    assert!(s.take_selection_requests().is_empty());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The four debuff buttons, dispel-tinted (ref RefreshBuffs over MAX_PARTY_DEBUFFS).
#[test]
fn the_debuff_row_draws_what_the_unit_carries() {
    let mut s = load_tot();
    switch_on(&mut s);
    s.set_auras(
        "targettarget",
        Some(vec![
            debuff(1000, "Rend", None),
            debuff(1001, "Curse of Agony", Some("Curse")),
        ]),
    );
    s.fire_event("UNIT_AURA", vec![ScriptValue::Str("targettarget".into())]);

    let paths = drawn(&mut s);
    assert!(
        paths.iter().any(|p| p == "Interface\\Icons\\Spell_1000"),
        "the first debuff's icon draws: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p == "Interface\\Icons\\Spell_1001"),
        "and the second"
    );
    assert!(
        !s.eval::<bool>("return TargetofTargetFrameDebuff3:IsShown() and true or false")
            .unwrap(),
        "the empty slots stay down"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The target's OWN aura rows re-wrap around this frame: 5 to a row instead of 6, and the buffs
/// open under the second debuff row instead of the seventh icon (ref TargetDebuffButton_Update
/// l.310-334). Both directions — the reference only re-runs the rows on the way in.
#[test]
fn the_target_rows_wrap_short_while_the_frame_stands_beside_them() {
    let mut s = load_tot();
    // A hostile target, so the debuffs lead and the buffs hang off them.
    s.set_auras("target", Some(vec![debuff(2000, "Sunder", None)]));
    s.fire_event("UNIT_AURA", vec![ScriptValue::Str("target".into())]);

    let anchor = |s: &mut UiScript, frame: &str| -> String {
        s.eval::<String>(&format!(
            "local _, rel = {frame}:GetPoint(1) return rel:GetName()"
        ))
        .unwrap()
    };
    assert_eq!(
        anchor(&mut s, "TargetFrameBuff1"),
        "TargetFrameDebuff7",
        "hidden: the sixth icon closes the first row"
    );

    switch_on(&mut s);
    assert!(shown(&mut s));
    assert_eq!(
        anchor(&mut s, "TargetFrameBuff1"),
        "TargetFrameDebuff6",
        "shown: the row wraps a slot early"
    );
    assert_eq!(
        anchor(&mut s, "TargetFrameDebuff6"),
        "TargetFrameDebuff1",
        "and the sixth icon starts the second row"
    );

    // Back off — the reference never re-lays the rows here, so they would stay wrapped short.
    s.run(r#"SHOW_TARGET_OF_TARGET = "0" TargetofTarget_Update()"#)
        .unwrap();
    assert_eq!(
        anchor(&mut s, "TargetFrameBuff1"),
        "TargetFrameDebuff7",
        "and back to six"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The reconcile. A token going away is CLEARED, not announced — no UNIT_* event carries it — so
/// the frame's own registrations cannot take it down. `TargetFrame_OnUpdate` is what does, and it
/// runs only while you have a target.
#[test]
fn the_reconcile_takes_the_frame_down_when_the_token_goes_silent() {
    let mut s = load_tot();
    switch_on(&mut s);
    assert!(shown(&mut s));

    // Exactly what the feed does: clear the token, fire nothing.
    s.set_unit("targettarget", None);
    assert!(shown(&mut s), "no event, so nothing has told the frame yet");

    s.tick(0.016);
    assert!(!shown(&mut s), "the reconcile notices within a frame");

    // And the other direction: a token appearing DOES fire, but the reconcile covers it anyway.
    s.set_unit("targettarget", Some(unit("Tri", 0x100, 100)));
    s.tick(0.016);
    assert!(shown(&mut s), "and brings it back");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
