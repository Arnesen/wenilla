use benilla_ui::script::{QuadContent, ScriptValue, SoundRequest, UiScript, UnitState};

/// Load one shipped `assets/ui/<file>` (the panel tests' loader, duplicated to stay
/// self-contained), panicking on any loader error.
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

/// The unit frames' production load prefix (ui_script/mod.rs order): fonts + UIParent +
/// tooltip, then the dropdown kit + unit popups the frames' DropDown children initialize into.
fn load_unit_frames(s: &UiScript) {
    load_xml(s, "Fonts.xml");
    load_xml(s, "UIParent.xml");
    load_xml(s, "GameTooltip.xml");
    load_xml(s, "UIDropDownMenu.xml");
    load_xml(s, "UnitPopup.xml");
    load_xml(s, "UnitFrames.xml");
}

/// Load the real `assets/ui/UnitFrames.xml` (the shipped default UI) into a bare engine and
/// drive it with synthetic snapshots — the whole slice-1 chain minus Bevy: template expansion,
/// StatusBar fill, the Era event set, the target frame's hide/show lifecycle, and the async
/// name arriving via UNIT_NAME_UPDATE.
#[test]
fn shipped_unit_frames_drive_end_to_end() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_unit_frames(&s);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // No units yet: both frames hid themselves on their OnLoad's first update.
    let hidden: bool = s
        .eval("return not BenillaPlayerFrame:IsVisible() and not BenillaTargetFrame:IsVisible()")
        .unwrap();
    assert!(hidden, "frames hide while their units don't exist");

    // The player appears (name still unresolved), at 72/100 health, 45/80 mana.
    s.set_unit(
        "player",
        Some(UnitState {
            exists: true,
            name: None,
            health: 72,
            max_health: 100,
            level: 12,
            power_type: 0,
            power: 45,
            max_power: 80,
            dead: false,
            reaction: 0,
            ..UnitState::default()
        }),
    );
    s.fire_event("UNIT_HEALTH", vec![ScriptValue::Str("player".into())]);
    assert!(s
        .eval::<bool>("return BenillaPlayerFrame:IsVisible()")
        .unwrap());
    let ok: bool = s
        .eval(
            r#"
            local hb, pb = BenillaPlayerFrameHealthBar, BenillaPlayerFramePowerBar
            local _, hmax = hb:GetMinMaxValues()
            local _, pmax = pb:GetMinMaxValues()
            local r, g, b = pb:GetStatusBarColor()
            return hb:GetValue() == 72 and hmax == 100
               and pb:GetValue() == 45 and pmax == 80 and pb:IsVisible()
               and b == 1 and r == 0 -- mana blue
               and BenillaPlayerFrameTextureFrameNameText:GetText() == "Unknown"
        "#,
        )
        .unwrap();
    assert!(ok, "player frame painted from the snapshot");

    // The name-query answer lands: UNIT_NAME_UPDATE repaints the name.
    s.set_unit(
        "player",
        Some(UnitState {
            exists: true,
            name: Some("Benilla".into()),
            health: 72,
            max_health: 100,
            level: 12,
            power_type: 0,
            power: 45,
            max_power: 80,
            dead: false,
            reaction: 0,
            ..UnitState::default()
        }),
    );
    s.fire_event("UNIT_NAME_UPDATE", vec![ScriptValue::Str("player".into())]);
    assert_eq!(
        s.eval::<String>("return BenillaPlayerFrameTextureFrameNameText:GetText()")
            .unwrap(),
        "Benilla"
    );

    // A powerless wolf gets targeted: frame shows, power bar hides, health fills 30/50.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            name: Some("Young Wolf".into()),
            health: 30,
            max_health: 50,
            level: 3,
            power_type: 0,
            power: 0,
            max_power: 0,
            dead: false,
            reaction: 4, // neutral
            ..UnitState::default()
        }),
    );
    s.take_sounds(); // drain anything earlier; the target select/deselect pair is under test below
    s.fire_event("PLAYER_TARGET_CHANGED", vec![]);
    let ok: bool = s
        .eval(
            r#"
            return BenillaTargetFrame:IsVisible()
               and not BenillaTargetFramePowerBar:IsVisible()
               and BenillaTargetFrameTextureFrameNameText:GetText() == "Young Wolf"
               and BenillaTargetFrameTextureFrameLevelText:GetText() == "3"
               and BenillaTargetFrameTextureFrameDeadText:GetText() == "" -- living target: no DEAD text
        "#,
        )
        .unwrap();
    assert!(
        ok,
        "target frame painted; powerless unit hides its power bar"
    );
    // The select sound rides the frame's OnShow (ref TargetFrame_OnShow): a neutral (4) wolf is
    // neither UnitIsEnemy (≤2) nor UnitIsFriend (≥5) → the neutral kit.
    assert_eq!(
        s.take_sounds(),
        vec![SoundRequest::KitName("igCreatureNeutralSelect".into())],
        "neutral target select kit"
    );

    // Neutral reaction (4) tints the name plate yellow — UnitReactionColor[4] = (1,1,0), the
    // faithful TargetFrame_CheckFaction path (the plate was untinted before). Assert on the extracted
    // quad's vertex color (same style as the bar-fill check below).
    s.resolve();
    let plate = s
        .extract()
        .into_iter()
        .find_map(|q| match q.content {
            QuadContent::Texture {
                path: Some(p),
                color: Some(c),
                ..
            } if p.contains("LevelBackground") => Some(c),
            _ => None,
        })
        .expect("target name-plate quad present");
    assert!(
        (plate[0] - 1.0).abs() < 1e-6 && (plate[1] - 1.0).abs() < 1e-6 && plate[2].abs() < 1e-6,
        "neutral name plate is yellow, got {plate:?}"
    );

    // The target's health-bar fill quad is 60% of the bar width (30/50 of the real 119px bar,
    // ref-TargetFrame.xml l.253-255).
    s.resolve();
    let quads = s.extract();
    let bar_rect = quads
        .iter()
        .filter(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                    if p.contains("UI-StatusBar"))
        })
        .filter_map(|q| q.rect)
        .find(|r| (r.width() - 119.0 * 0.6).abs() < 0.01)
        .expect("target health fill at 60% of 119px");
    assert!((bar_rect.width() - 71.4).abs() < 0.01);

    // Deselect: the target frame hides again, playing the lost-target kit (ref TargetFrame_OnHide).
    s.set_unit("target", None);
    s.fire_event("PLAYER_TARGET_CHANGED", vec![]);
    assert!(!s
        .eval::<bool>("return BenillaTargetFrame:IsVisible()")
        .unwrap());
    assert_eq!(
        s.take_sounds(),
        vec![SoundRequest::KitName(
            "INTERFACESOUND_LOSTTARGETUNIT".into()
        )],
        "deselect plays the lost-target kit"
    );

    // A hostile (reaction 2) target takes the UnitIsEnemy branch — the aggro kit.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            name: Some("Kobold Vermin".into()),
            health: 40,
            max_health: 40,
            level: 1,
            reaction: 2,
            ..UnitState::default()
        }),
    );
    s.fire_event("PLAYER_TARGET_CHANGED", vec![]);
    assert!(s
        .eval::<bool>("return UnitIsEnemy(\"target\", \"player\") == 1")
        .unwrap());
    assert_eq!(
        s.take_sounds(),
        vec![SoundRequest::KitName("igCreatureAggroSelect".into())],
        "hostile target select kit"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The hit indicator's drawn height via the engine extract: the `SetTextHeight` override on the
/// Text quad whose text matches (the two-regime split, decision 0582 — GetFont keeps reporting
/// the font object's own 30, like the real API).
fn extracted_text_height(s: &mut UiScript, text: &str) -> Option<f32> {
    s.resolve();
    s.extract().into_iter().find_map(|q| match q.content {
        QuadContent::Text {
            text: Some(t),
            text_height,
            ..
        } if t == text => Some(text_height),
        _ => None,
    })?
}

/// The portrait hit indicator (decision 0576): `UNIT_COMBAT` over `"player"` drives the
/// transcribed CombatFeedback — a physical wound paints the amount white at the base height 30,
/// a spell crit paints yellow at ×1.5, a full absorb paints the word at ×0.75 — and the fade
/// envelope (0.2 s in, 0.7 s hold, 0.3 s out) ends in a Hide. A `"target"`-token event never
/// touches it (only the player frame registers UNIT_COMBAT in 1.12).
#[test]
fn unit_combat_drives_the_player_hit_indicator() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_unit_frames(&s);
    s.set_unit(
        "player",
        Some(UnitState {
            exists: true,
            health: 72,
            max_health: 100,
            ..UnitState::default()
        }),
    );
    s.fire_event("UNIT_HEALTH", vec![ScriptValue::Str("player".into())]);

    let ev = |unit: &str, action: &str, flags: &str, amount: i64, school: i64| {
        vec![
            ScriptValue::Str(unit.into()),
            ScriptValue::Str(action.into()),
            ScriptValue::Str(flags.into()),
            ScriptValue::Int(amount),
            ScriptValue::Int(school),
        ]
    };

    // A physical wound: the amount, white, base height.
    s.fire_event("UNIT_COMBAT", ev("player", "WOUND", "", 17, 0));
    let ok: bool = s
        .eval(
            r#"
            local ind = BenillaPlayerFrameTextureFrameHitIndicator
            return ind:IsShown() ~= nil and tostring(ind:GetText()) == "17"
        "#,
        )
        .unwrap();
    assert!(ok, "physical wound paints the amount ({:?})", s.errors());
    assert_eq!(
        extracted_text_height(&mut s, "17"),
        Some(30.0),
        "base height 30 (the SetTextHeight regime)"
    );

    // A spell crit: ×1.5 height (the CRITICAL arm), and the type>0 yellow.
    s.fire_event("UNIT_COMBAT", ev("player", "WOUND", "CRITICAL", 64, 4));
    let ok: bool = s
        .eval("return tostring(BenillaPlayerFrameTextureFrameHitIndicator:GetText()) == \"64\"")
        .unwrap();
    assert!(ok, "spell crit paints ({:?})", s.errors());
    assert_eq!(
        extracted_text_height(&mut s, "64"),
        Some(45.0),
        "×1.5 crit height, UNCAPPED past 32 (decision 0582's regime split)"
    );

    // A full absorb: the word at ×0.75.
    s.fire_event("UNIT_COMBAT", ev("player", "WOUND", "ABSORB", 0, 0));
    let ok: bool = s
        .eval("return BenillaPlayerFrameTextureFrameHitIndicator:GetText() == \"Absorb\"")
        .unwrap();
    assert!(ok, "full absorb paints the word ({:?})", s.errors());
    assert_eq!(
        extracted_text_height(&mut s, "Absorb"),
        Some(22.5),
        "the word at ×0.75"
    );

    // The envelope: mid-hold the text is fully opaque; past 1.2 s it hides.
    s.tick(0.5); // 0.2 fade-in + into the hold
    let ok: bool = s
        .eval(
            r#"
            local ind = BenillaPlayerFrameTextureFrameHitIndicator
            return ind:IsShown() ~= nil and ind:GetAlpha() == 1.0
        "#,
        )
        .unwrap();
    assert!(ok, "mid-hold: opaque ({:?})", s.errors());
    s.tick(0.8); // past fade-in + hold + fade-out (1.2 s total)
    assert!(
        s.eval::<bool>("return BenillaPlayerFrameTextureFrameHitIndicator:IsShown() == nil")
            .unwrap(),
        "the envelope ends in a Hide ({:?})",
        s.errors()
    );

    // A target-token event leaves the (hidden) indicator alone.
    s.fire_event("UNIT_COMBAT", ev("target", "WOUND", "", 99, 0));
    assert!(
        s.eval::<bool>("return BenillaPlayerFrameTextureFrameHitIndicator:IsShown() == nil")
            .unwrap(),
        "a target event never touches the player indicator"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Left-clicking the player unit frame targets yourself — the faithful self-target path (ref
/// `PlayerFrame_OnClick` → `TargetUnit("player")`). Drives the real hit-test path (a press + release
/// on the frame's centre fires its `OnClick`) against the shipped `UnitFrames.xml`, and asserts the
/// `TargetUnit` request the app drains and commits. A right-click opens the SELF unit popup
/// (decision 0434 phase 5) — nothing solo (only CANCEL survives the gates, the ref shows no
/// menu), the full leader set once a party is pushed.
#[test]
fn left_clicking_the_player_frame_targets_self() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    // The SELF-menu strings the popup rows bake at UnitPopup.xml load (no GlobalStrings in a
    // bare engine — production reads the real file).
    s.run(
        r#"
        LOOT_METHOD = "Loot Method"
        LOOT_THRESHOLD = "Loot Threshold"
        LOOT_GROUP_LOOT = "Group Loot"
        PARTY_LEAVE = "Leave Party"
        RAID_TARGET_ICON = "Raid Target Icon"
        CANCEL = "Cancel"
        ITEM_QUALITY2_DESC = "Uncommon"
    "#,
    )
    .unwrap();
    load_unit_frames(&s);

    // The player must exist for the frame to be shown and mouse-hittable.
    s.set_unit(
        "player",
        Some(UnitState {
            exists: true,
            health: 72,
            max_health: 100,
            ..UnitState::default()
        }),
    );
    s.fire_event("UNIT_HEALTH", vec![ScriptValue::Str("player".into())]);
    s.resolve();
    assert!(
        s.take_target_requests().is_empty(),
        "no request before any click"
    );

    // Click the frame's centre through the real hit-test path (press + release on the same frame).
    let (cx, cy) = s
        .eval::<(f64, f64)>("return BenillaPlayerFrame:GetCenter()")
        .unwrap();
    s.mouse_button(cx as f32, cy as f32, "RightButton", true);
    s.mouse_button(cx as f32, cy as f32, "RightButton", false);
    assert!(
        s.take_target_requests().is_empty(),
        "right-click queues no target"
    );
    // Solo, every SELF row is gated off (only CANCEL survives) — the ref opens no menu.
    assert!(
        s.eval::<bool>("return not DropDownList1:IsVisible()")
            .unwrap(),
        "no SELF menu while solo"
    );

    s.mouse_button(cx as f32, cy as f32, "LeftButton", true);
    s.mouse_button(cx as f32, cy as f32, "LeftButton", false);
    assert_eq!(
        s.take_target_requests(),
        vec!["player"],
        "left-click queues a self-target"
    );

    // Grouped and leading: the same right-click opens the SELF popup — title (our name),
    // Loot Method + Loot Threshold (nested), Leave Party, Raid Target Icon (nested), Cancel.
    s.set_party(benilla_ui::script::PartyState {
        members: vec![benilla_ui::script::PartyMemberInfo {
            name: "Alice".into(),
            guid: 0xA11CE,
        }],
        leader_index: 0, // we lead
        raid_members: 0,
        loot_method: "group".into(),
        master_looter: None,
        loot_threshold: 2,
    });
    s.mouse_button(cx as f32, cy as f32, "RightButton", true);
    s.mouse_button(cx as f32, cy as f32, "RightButton", false);
    assert!(
        s.eval::<bool>("return DropDownList1:IsVisible()").unwrap(),
        "the SELF menu opens for a party leader"
    );
    assert_eq!(
        s.eval::<i64>("return DropDownList1.numButtons").unwrap(),
        6,
        "title + Loot Method + Loot Threshold + Leave Party + Raid Target Icon + Cancel"
    );
    assert_eq!(
        s.eval::<String>("return DropDownList1Button4:GetText()")
            .unwrap(),
        "Leave Party"
    );
    // The nested rows carry the expand arrow (the level-2 gate for a leader).
    assert!(
        s.eval::<bool>("return DropDownList1Button2ExpandArrow:IsVisible()")
            .unwrap(),
        "Loot Method is nested for the leader"
    );
    // Clicking Leave Party through the real hit path fires UnitPopup_OnClick → LeaveParty()
    // and closes the list (not keepShownOnClick).
    s.resolve();
    let (rx, ry) = s
        .eval::<(f64, f64)>("return DropDownList1Button4:GetCenter()")
        .unwrap();
    s.mouse_button(rx as f32, ry as f32, "LeftButton", true);
    s.mouse_button(rx as f32, ry as f32, "LeftButton", false);
    assert_eq!(
        s.take_party_requests(),
        vec![benilla_ui::script::PartyRequest::Leave],
        "Leave Party queues the leave intent"
    );
    assert!(
        s.eval::<bool>("return not DropDownList1:IsVisible()")
            .unwrap(),
        "the click closes the list"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The nested level-2 list end-to-end (the 6a suspect path): a leader right-clicks the player
/// frame, hovers Raid Target Icon — a `hasArrow` row's OnEnter is what opens `DropDownList2` —
/// and clicks Skull through the real hit path. The mark intent must queue against the menu's
/// unit. The level-1 click was pinned above; this pins the level the marks actually live on.
#[test]
fn raid_mark_clicks_through_the_nested_level() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    s.run(
        r#"
        LOOT_METHOD = "Loot Method"
        LOOT_THRESHOLD = "Loot Threshold"
        LOOT_GROUP_LOOT = "Group Loot"
        PARTY_LEAVE = "Leave Party"
        RAID_TARGET_ICON = "Raid Target Icon"
        CANCEL = "Cancel"
        ITEM_QUALITY2_DESC = "Uncommon"
        RAID_TARGET_1 = "Star"; RAID_TARGET_2 = "Circle"; RAID_TARGET_3 = "Diamond";
        RAID_TARGET_4 = "Triangle"; RAID_TARGET_5 = "Moon"; RAID_TARGET_6 = "Square";
        RAID_TARGET_7 = "Cross"; RAID_TARGET_8 = "Skull"; NONE = "None"
    "#,
    )
    .unwrap();
    load_unit_frames(&s);

    s.set_unit(
        "player",
        Some(UnitState {
            exists: true,
            health: 72,
            max_health: 100,
            ..UnitState::default()
        }),
    );
    s.fire_event("UNIT_HEALTH", vec![ScriptValue::Str("player".into())]);
    s.set_party(benilla_ui::script::PartyState {
        members: vec![benilla_ui::script::PartyMemberInfo {
            name: "Alice".into(),
            guid: 0xA11CE,
        }],
        leader_index: 0, // we lead — the mark rows are leader-gated
        raid_members: 0,
        loot_method: "group".into(),
        master_looter: None,
        loot_threshold: 2,
    });
    s.resolve();

    let (cx, cy) = s
        .eval::<(f64, f64)>("return BenillaPlayerFrame:GetCenter()")
        .unwrap();
    s.mouse_button(cx as f32, cy as f32, "RightButton", true);
    s.mouse_button(cx as f32, cy as f32, "RightButton", false);
    assert!(
        s.eval::<bool>("return DropDownList1:IsVisible()").unwrap(),
        "the SELF menu opens"
    );
    s.resolve();

    // Hover the nested row through the real pointer path.
    assert_eq!(
        s.eval::<String>("return DropDownList1Button5:GetText()")
            .unwrap(),
        "Raid Target Icon"
    );
    let (rx, ry) = s
        .eval::<(f64, f64)>("return DropDownList1Button5:GetCenter()")
        .unwrap();
    s.mouse_move(rx as f32, ry as f32);
    assert!(
        s.eval::<bool>("return DropDownList2:IsVisible()").unwrap(),
        "hovering the nested row opens level 2"
    );
    assert_eq!(
        s.eval::<i64>("return DropDownList2.numButtons").unwrap(),
        9,
        "the eight marks + None"
    );
    s.resolve();

    // Click Skull (row 8) through the real hit path.
    assert_eq!(
        s.eval::<String>("return DropDownList2Button8:GetText()")
            .unwrap(),
        "Skull"
    );
    let (sx, sy) = s
        .eval::<(f64, f64)>("return DropDownList2Button8:GetCenter()")
        .unwrap();
    s.mouse_button(sx as f32, sy as f32, "LeftButton", true);
    s.mouse_button(sx as f32, sy as f32, "LeftButton", false);
    assert_eq!(
        s.take_party_requests(),
        vec![benilla_ui::script::PartyRequest::SetRaidTarget {
            unit: "player".into(),
            index: 8
        }],
        "Skull queues the mark intent for the menu's unit"
    );
    // Ref law: the click hides only the row's OWN list (UIDropDownMenuButton_OnClick's
    // `this:GetParent():Hide()`; DropDownList1's OnHide closes level 2, never the reverse).
    // Level 1 lingers and dies by the 2s show-timer once the pointer leaves the chain.
    assert!(
        s.eval::<bool>("return not DropDownList2:IsVisible() and DropDownList1:IsVisible()")
            .unwrap(),
        "the click closes its own level; level 1 lingers (ref)"
    );
    // Two ticks: the ref's OnUpdate hides only on the frame AFTER the timer crosses zero.
    s.mouse_move(5.0, 5.0);
    s.tick(2.1);
    s.tick(0.1);
    assert!(
        s.eval::<bool>("return not DropDownList1:IsVisible()")
            .unwrap(),
        "level 1 times out 2s after the pointer leaves the chain"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The level slot's transcribed CheckLevel law (ref-TargetFrame.lua l.119-142) end to end over
/// the REAL `GetDifficultyColor` (ref-QuestLogFrame.lua l.14-20 + l.584-599, loaded from the
/// shipped QuestLogFrame.xml — its ref home) and `UnitLevel`'s −1 return: an attackable target
/// difficulty-colors its number; a hostile 10+ levels up (UnitLevel −1) or a corpse swaps the
/// number for the HighLevelTexture skull; the green→grey boundary rides the real
/// GetQuestGreenRange binding.
#[test]
fn shipped_target_frame_runs_the_level_law() {
    use benilla_ui::script::PlayerReqState;
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_unit_frames(&s);
    // GetDifficultyColor's own load chain (the quest log window, its ref home).
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "QuestLogFrame.xml");
    // The player at level 3, both feeds (the snapshot UnitLevel("player") reads; the req state
    // the −1 gate and GetQuestGreenRange read) — the app keeps the two in step.
    s.set_player_req_state(PlayerReqState {
        level: 3,
        ..Default::default()
    });
    s.set_unit(
        "player",
        Some(UnitState {
            exists: true,
            level: 3,
            is_player: true,
            health: 50,
            max_health: 50,
            ..UnitState::default()
        }),
    );

    // An attackable boar 5 up (the screenshot's case at the director's level): the number shows,
    // impossible-red via the real table; no skull.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            name: Some("Elder Mottled Boar".into()),
            health: 40,
            max_health: 40,
            level: 8,
            reaction: 4,
            can_attack: true,
            ..UnitState::default()
        }),
    );
    s.fire_event("PLAYER_TARGET_CHANGED", vec![]);
    let ok: bool = s
        .eval(
            r#"
            local lvl = getglobal("BenillaTargetFrameTextureFrameLevelText")
            local skull = getglobal("BenillaTargetFrameTextureFrameHighLevelTexture")
            local c = GetDifficultyColor(8)
            return lvl:IsShown() ~= nil and skull:IsShown() == nil
               and tostring(lvl:GetText()) == "8"
               and c.r == 1.00 and c.g == 0.10 and c.b == 0.10
        "#,
        )
        .unwrap();
    assert!(ok, "attackable +5: red number, no skull ({:?})", s.errors());

    // A hostile 10+ levels up: UnitLevel reads −1 → the skull replaces the number.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            name: Some("Guard".into()),
            health: 400,
            max_health: 400,
            level: 13,
            reaction: 2,
            can_attack: true,
            ..UnitState::default()
        }),
    );
    s.fire_event("PLAYER_TARGET_CHANGED", vec![]);
    let ok: bool = s
        .eval(
            r#"
            local lvl = getglobal("BenillaTargetFrameTextureFrameLevelText")
            local skull = getglobal("BenillaTargetFrameTextureFrameHighLevelTexture")
            return UnitLevel("target") == -1
               and skull:IsShown() ~= nil and lvl:IsShown() == nil
        "#,
        )
        .unwrap();
    assert!(ok, "hostile +10: the skull shows ({:?})", s.errors());

    // A DEAD mob is NOT a corpse (§5: UnitIsCorpse is a pure TYPEID_CORPSE object check, and
    // UnitLevel has no health test) — the ref shows a dead mob's NUMBER, not the skull.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            name: Some("Elder Mottled Boar".into()),
            health: 0,
            max_health: 40,
            level: 8,
            reaction: 4,
            dead: true,
            ..UnitState::default()
        }),
    );
    s.fire_event("PLAYER_TARGET_CHANGED", vec![]);
    let ok: bool = s
        .eval(
            r#"
            local lvl = getglobal("BenillaTargetFrameTextureFrameLevelText")
            local skull = getglobal("BenillaTargetFrameTextureFrameHighLevelTexture")
            return UnitIsCorpse("target") == nil
               and lvl:IsShown() ~= nil and skull:IsShown() == nil
               and tostring(lvl:GetText()) == "8"
        "#,
        )
        .unwrap();
    assert!(
        ok,
        "dead mob: the number shows, no skull ({:?})",
        s.errors()
    );

    // A resolved CORPSE world object: the ref's first branch — the skull, whatever the level.
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            name: Some("Corpse of Somebody".into()),
            level: 8,
            corpse_object: true,
            ..UnitState::default()
        }),
    );
    s.fire_event("PLAYER_TARGET_CHANGED", vec![]);
    let ok: bool = s
        .eval(
            r#"
            local skull = getglobal("BenillaTargetFrameTextureFrameHighLevelTexture")
            return UnitIsCorpse("target") == 1 and skull:IsShown() ~= nil
        "#,
        )
        .unwrap();
    assert!(ok, "corpse object: the skull shows ({:?})", s.errors());

    // The green→grey boundary rides GetQuestGreenRange: at player 30 the band is 7 —
    // 7 below (23) still standard-green, 8 below (22) trivial-grey.
    s.set_player_req_state(PlayerReqState {
        level: 30,
        ..Default::default()
    });
    s.set_unit(
        "player",
        Some(UnitState {
            exists: true,
            level: 30,
            is_player: true,
            health: 50,
            max_health: 50,
            ..UnitState::default()
        }),
    );
    let ok: bool = s
        .eval(
            r#"
            local g, t = GetDifficultyColor(23), GetDifficultyColor(22)
            return GetQuestGreenRange() == 7
               and g.r == 0.25 and g.g == 0.75 and g.b == 0.25
               and t.r == 0.50 and t.g == 0.50 and t.b == 0.50
        "#,
        )
        .unwrap();
    assert!(ok, "green range boundary at level 30 ({:?})", s.errors());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
