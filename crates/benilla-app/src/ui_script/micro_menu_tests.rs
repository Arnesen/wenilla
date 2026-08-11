//! The shipped `assets/ui/MicroMenu.xml` — the eight micro buttons in the main bar's right-hand
//! recess — loaded behind `ActionBar.xml` (their anchor target `MainMenuBarArtFrame` and the
//! `BenillaActionBarArt_SeatAbove` seating helper) into a bare engine.
//!
//! What these guard, in order: the row's ref geometry (29×58 at art-frame BOTTOMLEFT +(552,2), a
//! 26 px stride) — which is the whole point of the slice, since the gap the row fills is exactly
//! that panel of empty bar art; the `HitRectInsets` header that keeps a button's transparent top
//! from eating the mouse; `UpdateMicroButtons` tracking a panel's visibility; and
//! `UpdateTalentButton`'s level-10 gate closing the row up behind the hidden talent button.

use benilla_ui::script::{QuadContent, ScriptValue, TexCoords, UiScript};

/// Load `ActionBar.xml` then `MicroMenu.xml` into a 1024×768 engine, asserting both load clean.
/// 1024 wide is deliberate: the 1024-wide bar then centers at x=0, so every ref offset below is
/// also an absolute screen coordinate.
fn harness() -> UiScript {
    harness_with(&[])
}

/// …with more shipped files behind the row — the tooltip test needs the real plate.
fn harness_with(extra: &[&str]) -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    let files: Vec<&str> = [
        "Fonts.xml",
        "Cooldown.xml",
        "ActionBar.xml",
        "MicroMenu.xml",
    ]
    .into_iter()
    .chain(extra.iter().copied())
    .collect();
    for file in files {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets/ui")
                .join(file),
        )
        .unwrap();
        let doc = benilla_ui::framexml::parse(&text).unwrap();
        let report = benilla_ui::loader::load(&s, &doc, &|_| None);
        assert!(
            report.errors.is_empty(),
            "{file}: loader errors: {:?}",
            report.errors
        );
    }
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    s
}

/// The row's geometry, quoted from ref-MainMenuBarMicroButtons.xml: CharacterMicroButton at the
/// art frame's BOTTOMLEFT +(552,2), each button 29×58, the rest chained BOTTOMLEFT-to-previous-
/// BOTTOMRIGHT +(-3,0) ⇒ a 26 px stride, so the eight run x 552..757 — inside the 1024-wide bar,
/// between the page arrows (x≈522) and the bag cluster at the far right.
#[test]
fn the_micro_row_sits_where_the_reference_puts_it() {
    let mut s = harness();
    // Past level 10 so all eight are shown and the row is unbroken.
    set_player_level(&mut s, 10);
    s.resolve();

    let names = [
        "CharacterMicroButton",
        "SpellbookMicroButton",
        "TalentMicroButton",
        "QuestLogMicroButton",
        "SocialsMicroButton",
        "WorldMapMicroButton",
        "MainMenuMicroButton",
        "HelpMicroButton",
    ];
    for (i, name) in names.iter().enumerate() {
        let (left, bottom, w, h) = s
            .eval::<(f64, f64, f64, f64)>(&format!(
                "return {name}:GetLeft(), {name}:GetBottom(), {name}:GetWidth(), {name}:GetHeight()"
            ))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((w, h), (29.0, 58.0), "{name} size");
        assert_eq!(left, 552.0 + 26.0 * i as f64, "{name} left edge");
        assert_eq!(bottom, 2.0, "{name} sits 2 above the bar's bottom");
    }
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The `HitRectInsets top="18"` header: the micro-button art fills only the lower ~40 of the 58,
/// and the empty top must stay transparent to the mouse — otherwise the row eats hover over the
/// bar's XP strip and the sliver of world above it.
#[test]
fn the_transparent_top_of_a_micro_button_does_not_capture_the_mouse() {
    let mut s = harness();
    set_player_level(&mut s, 10);
    s.resolve();

    // Mid-button horizontally (552 + 14), inside the art band (y 2..42) ⇒ captures.
    assert_eq!(
        s.hit_test_name(566.0, 20.0).as_deref(),
        Some("CharacterMicroButton"),
        "the art band takes the mouse"
    );
    // Same column, in the dead 18-unit header (y 42..60) ⇒ nothing of ours captures it.
    assert_ne!(
        s.hit_test_name(566.0, 50.0).as_deref(),
        Some("CharacterMicroButton"),
        "the inset header must be transparent to the mouse"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The character button's face: the region binds the SAME `"player"` portrait slot the unit frame
/// samples, and carries the ref's crop window out to the renderer. Both halves matter — the crop is
/// what makes an 18×25 rectangle show a face instead of the whole square bake squashed into it, and
/// the extract path used to pin portrait quads to full UVs and drop it (fixed with this slice).
/// Pushing the button swaps the window and dims it (ref CharacterMicroButton_SetPushed).
#[test]
fn the_character_button_carries_the_player_portrait_through_the_reference_crop() {
    let mut s = harness();
    s.resolve();

    let window = |s: &mut UiScript| {
        s.extract()
            .into_iter()
            .find_map(|q| match q.content {
                QuadContent::Texture {
                    portrait_unit: Some(unit),
                    tex_coords,
                    circular,
                    ..
                } => Some((unit, tex_coords, circular)),
                _ => None,
            })
            .expect("the micro button's portrait quad")
    };

    let (unit, coords, circular) = window(&mut s);
    assert_eq!(unit, "player");
    assert!(
        circular,
        "the ref's round stencil lives in the bake's own UV space, so the crop below yields a \
         rectangular slice OF a masked face — not an ellipse fitted to this 18x25 region"
    );
    let round4 = |c: TexCoords| match c {
        TexCoords::Rect(e) => e.map(|v| (v * 10_000.0).round() / 10_000.0),
        TexCoords::Corners(_) => panic!("the 4-edge form"),
    };
    assert_eq!(
        round4(coords.expect("a crop window")),
        [0.2, 0.8, 0.0666, 0.9],
        "the normal window (ref CharacterMicroButton_SetNormal)"
    );

    s.run("CharacterMicroButton_SetPushed()").unwrap();
    s.resolve();
    let (_, coords, _) = window(&mut s);
    assert_eq!(
        round4(coords.expect("a crop window")),
        [0.2666, 0.8666, 0.0, 0.8333],
        "the held-down window (ref CharacterMicroButton_SetPushed)"
    );
    assert_eq!(
        s.eval::<f64>("return MicroButtonPortrait:GetAlpha()")
            .unwrap(),
        0.5,
        "…and the face dims while the button is down"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// `UpdateMicroButtons` (ref MainMenuBarMicroButtons.lua l.20-84): a button is PUSHED exactly
/// while its panel is up. Driven here with a stand-in frame under the real panel's name rather
/// than the whole character window — this is the visibility contract, not that window's.
#[test]
fn a_micro_button_pushes_while_its_panel_is_open() {
    let s = harness();
    s.run(
        r#"
        local f = CreateFrame("Frame", "CharacterFrame")
        f:SetPoint("CENTER", 0, 0); f:SetSize(100, 100); f:Hide()
    "#,
    )
    .unwrap();

    let state = |s: &UiScript| {
        s.eval::<String>("return CharacterMicroButton:GetButtonState()")
            .unwrap()
    };
    s.run("UpdateMicroButtons()").unwrap();
    assert_eq!(state(&s), "NORMAL", "closed panel ⇒ button up");

    s.run("CharacterFrame:Show(); UpdateMicroButtons()")
        .unwrap();
    assert_eq!(state(&s), "PUSHED", "open panel ⇒ button held down");

    s.run("CharacterFrame:Hide(); UpdateMicroButtons()")
        .unwrap();
    assert_eq!(state(&s), "NORMAL", "closed again ⇒ button pops back");

    // A button whose panel doesn't exist at all (the three unwired ones, and any panel a test
    // harness didn't load) never pushes — and never errors.
    assert_eq!(
        s.eval::<String>("return SocialsMicroButton:GetButtonState()")
            .unwrap(),
        "NORMAL"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// `UpdateTalentButton` (ref l.133-141): under level 10 the talent button is hidden and the quest
/// log button slides onto its seat, so the row has no hole in it; at 10 the button returns and the
/// tail shifts back out by one stride.
#[test]
fn the_talent_button_appears_at_level_ten_and_the_row_closes_up_below_it() {
    let mut s = harness();

    set_player_level(&mut s, 9);
    s.resolve();
    assert!(
        !s.eval::<bool>("return TalentMicroButton:IsVisible()")
            .unwrap(),
        "no talents before 10"
    );
    // The quest log button takes the talent button's own seat — slot 3, not slot 4.
    assert_eq!(
        s.eval::<f64>("return QuestLogMicroButton:GetLeft()")
            .unwrap(),
        552.0 + 26.0 * 2.0
    );
    assert_eq!(
        s.eval::<f64>("return HelpMicroButton:GetLeft()").unwrap(),
        552.0 + 26.0 * 6.0,
        "the whole tail moves up one slot with it"
    );

    set_player_level(&mut s, 10);
    s.resolve();
    assert!(
        s.eval::<bool>("return TalentMicroButton:IsVisible()")
            .unwrap(),
        "the button returns at 10"
    );
    assert_eq!(
        s.eval::<f64>("return QuestLogMicroButton:GetLeft()")
            .unwrap(),
        552.0 + 26.0 * 3.0
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The hover (ref MainMenuBarMicroButton's OnEnter, l.13): every button's plate is the ref's
/// TWO-line `GameTooltip_AddNewbieTip` — the label, then that button's own `NEWBIE_TOOLTIP_*`
/// explanation in gold. 1.12 ships detailed tips ON (`SHOW_NEWBIE_TIPS = "1"`, ref
/// UIOptionsFrame.lua l.100), so the paragraph is the DEFAULT hover; this file used to open-code
/// the tips-off branch and show a bare label. Decision 0661.
///
/// Checked across all eight, because the explanation is threaded per button through OnLoad and a
/// single missing argument is invisible until someone hovers that one button.
#[test]
fn every_micro_button_hovers_with_its_reference_explanation() {
    let mut s = harness_with(&["UIParent.xml", "GameTooltip.xml"]);
    set_player_level(&mut s, 10);
    s.resolve();

    for (button, newbie) in [
        ("CharacterMicroButton", "NEWBIE_TOOLTIP_CHARACTER"),
        ("SpellbookMicroButton", "NEWBIE_TOOLTIP_SPELLBOOK"),
        ("TalentMicroButton", "NEWBIE_TOOLTIP_TALENTS"),
        ("QuestLogMicroButton", "NEWBIE_TOOLTIP_QUESTLOG"),
        ("SocialsMicroButton", "NEWBIE_TOOLTIP_SOCIAL"),
        ("WorldMapMicroButton", "NEWBIE_TOOLTIP_WORLDMAP"),
        ("MainMenuMicroButton", "NEWBIE_TOOLTIP_MAINMENU"),
        ("HelpMicroButton", "NEWBIE_TOOLTIP_HELP"),
    ] {
        s.run(&format!("BenillaMicroButton_OnEnter({button})"))
            .unwrap();
        assert_eq!(
            s.eval::<i64>("return GameTooltip:NumLines()").unwrap(),
            2,
            "{button}: the label, then the explanation"
        );
        // Line 1 is the button's own label (plus a `|cff…(key)|r` suffix on the five bound ones).
        let label = s.eval::<String>(&format!("return {button}.label")).unwrap();
        let line1 = s
            .eval::<String>("return GameTooltipTextLeft1:GetText()")
            .unwrap();
        assert!(
            line1.starts_with(&label),
            "{button}: line 1 is {line1:?}, expected it to open with {label:?}"
        );
        assert_eq!(
            s.eval::<String>("return GameTooltipTextLeft2:GetText()")
                .unwrap(),
            s.eval::<String>(&format!("return {newbie}")).unwrap(),
            "{button}: line 2 is the ref's {newbie}, verbatim"
        );
        assert_eq!(
            s.eval::<i64>("return GameTooltip.default").unwrap(),
            1,
            "{button}: the default-corner anchor, not ANCHOR_RIGHT off the button"
        );
    }
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Set the player's level and fire the `UNIT_LEVEL` the talent gate listens for.
fn set_player_level(s: &mut UiScript, level: u32) {
    s.set_unit(
        "player",
        Some(benilla_ui::script::UnitState {
            exists: true,
            level,
            ..Default::default()
        }),
    );
    s.fire_event("UNIT_LEVEL", vec![ScriptValue::Str("player".into())]);
}
