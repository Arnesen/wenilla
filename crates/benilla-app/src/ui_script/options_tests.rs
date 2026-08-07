//! The shipped `assets/ui/OptionsFrame.xml` — the era-shaped, 1.12-skinned options window
//! (0950: the shell; 0957: the Audio page; 0959: the Graphics page; 0978: the 1.12-native
//! skin — no era extraction, every texture from the MPQ chain; 0981: the 1.14 System-window
//! dialog chrome — translucent dark ground, outline boxes, hairline dividers; 0984: the 1.14
//! select/hover wash mechanism, the working era search; 0985: the provenance split those
//! cite; 0989: the directed cuts — steppers and the corner X gone, the whole bar live via
//! the engine's track-press law, the search box at the era's verbatim seat; 0992: the
//! dropdown row shape on the 1.12 kit — Environment Detail — and the Nameplates page's
//! three UnitName* rows).
//!
//! What these guard: the file loads clean inside the real neighbourhood (Fonts + UiPanels +
//! GameMenuFrame); the menu's Options button is the door in (menu down, options up, on the ref's
//! own kit); Controls is the default category and the page title follows the selection; both
//! close spellings put the window away; the selected row wears the LOCKED GOLD additive wash
//! and hover runs the steel-blue one (the 1.14 pair); and the search reflows the live rows
//! under category heads and restores the authored page exactly.
//!
//! The page tests (0957 Audio, 0959 Graphics) run against the REAL registered CVar set
//! (`crate::cvars`): rows read the table on select, writes land on the change queue the host
//! drains, the snap grids and readouts hold (the era 5% volumes; the 1.12 uiscale 0.01 and
//! farclip min-anchored 60), the 1.12 master→ambience dependency greys, and Defaults walks the
//! visible page back to the registered defaults.

use benilla_ui::script::{QuadContent, SoundRequest, UiScript};

/// The window's real neighbourhood, in the manifest's own order (options before the menu — the
/// game_menu_tests::harness_with idiom, minus the extras this file never needs).
fn harness() -> UiScript {
    harness_on(UiScript::new().unwrap())
}

/// Load the manifest slice onto a prepared script — split out so page tests can seed CVars
/// BEFORE the XML loads, the way the app does (ui_script::setup_script). The dropdown kit
/// rides along since 0992 (the dropdown rows inherit its capsule template), and GameTooltip
/// before it for the kit's TOOLTIP_DEFAULT_COLOR — the app's own order.
fn harness_on(mut s: UiScript) -> UiScript {
    s.set_screen_size(1024.0, 768.0);
    for file in [
        "Fonts.xml",
        "UiPanels.xml",
        "GameTooltip.xml",
        "UIDropDownMenu.xml",
        "ScrollTemplates.xml", // the Keybindings page's faux-scroll kit
        "KeyBindingsPage.xml", // the Keybindings body's templates + script (1008)
        "OptionsFrame.xml",
        "GameMenuFrame.xml",
    ] {
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
        // The dialect announces DROPPED subtrees as warnings, not errors — for the new file,
        // a warning is a silently-missing piece of chrome, so it fails here.
        if file == "OptionsFrame.xml" {
            assert!(
                report.warnings.is_empty(),
                "{file}: loader warnings (dropped subtrees?): {:?}",
                report.warnings
            );
        }
    }
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    s
}

/// The door in: the game menu's Options button (its ref OnClick + the explicit hide) swaps the
/// two native-center frames — menu down, options up holding the center slot — on the ref's own
/// igMainMenuOption kit.
#[test]
fn the_menu_options_button_swaps_the_menu_for_the_options_window() {
    let mut s = harness();
    s.run("ShowUIPanel(GameMenuFrame)").unwrap();
    let _ = s.take_sounds();

    s.run("GameMenuButtonOptions:Click()").unwrap();
    assert!(
        s.eval::<bool>("return OptionsFrame:IsVisible()").unwrap(),
        "the options window opened"
    );
    assert!(
        !s.eval::<bool>("return GameMenuFrame:IsVisible()").unwrap(),
        "…and the menu went away first"
    );
    assert!(
        s.eval::<bool>("return GetCenterFrame():GetName() == \"OptionsFrame\"")
            .unwrap(),
        "the window holds the native-center slot the menu vacated"
    );
    assert!(s
        .take_sounds()
        .contains(&SoundRequest::KitName("igMainMenuOption".into())));
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Controls is the default page (the OnShow seat), and the page title is the selected row's own
/// label — including the one key whose label differs from it (ActionBars → "Action Bars").
#[test]
fn controls_is_the_default_category_and_the_title_reads_it() {
    let s = harness();
    s.run("ShowUIPanel(OptionsFrame)").unwrap();

    assert_eq!(
        s.eval::<String>("return OptionsFrame.selectedCategory")
            .unwrap(),
        "Controls"
    );
    assert_eq!(
        s.eval::<String>("return OptionsFrameContainerTitle:GetText()")
            .unwrap(),
        "Controls"
    );
    // The row labels are the era category tree's.
    assert_eq!(
        s.eval::<String>("return OptionsFrameCategoryListRowActionBars:GetText()")
            .unwrap(),
        "Action Bars"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Clicking a category row moves the selection and the page title with it — and the selection
/// survives a close/reopen (the OnShow re-applies the last seat, not the default).
#[test]
fn clicking_a_row_moves_the_selection_and_the_page_title() {
    let s = harness();
    s.run("ShowUIPanel(OptionsFrame)").unwrap();

    s.run("OptionsFrameCategoryListRowGraphics:Click()")
        .unwrap();
    assert_eq!(
        s.eval::<String>("return OptionsFrame.selectedCategory")
            .unwrap(),
        "Graphics"
    );
    assert_eq!(
        s.eval::<String>("return OptionsFrameContainerTitle:GetText()")
            .unwrap(),
        "Graphics"
    );

    // Close and reopen: still Graphics, not Controls.
    s.run("HideUIPanel(OptionsFrame)").unwrap();
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    assert_eq!(
        s.eval::<String>("return OptionsFrameContainerTitle:GetText()")
            .unwrap(),
        "Graphics"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The red Close hides the window on the igMainMenuClose kit, the corner X does not EXIST
/// (0989's directed cut), and a page WITHOUT rows keeps Defaults disabled (Controls has rows
/// since 0961, so the rowless check moved to Interface).
#[test]
fn the_close_button_hides_the_window_and_defaults_is_disabled() {
    let mut s = harness();

    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.run("OptionsFrameCategoryListRowInterface:Click()")
        .unwrap();
    assert!(
        !s.eval::<bool>("return OptionsFrameContainerDefaults:IsEnabled()")
            .unwrap(),
        "Defaults is dead on a page with no rows"
    );
    let _ = s.take_sounds();
    s.run("OptionsFrameCloseButton:Click()").unwrap();
    assert!(!s.eval::<bool>("return OptionsFrame:IsVisible()").unwrap());
    assert!(s
        .take_sounds()
        .contains(&SoundRequest::KitName("igMainMenuClose".into())));

    // No corner X (0989's directed cut — the era HAS one; the red button and ESC are the
    // window's exits).
    assert!(s
        .eval::<bool>("return OptionsFrameClosePanelButton == nil")
        .unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The row wash (0984: the 1.14 OptionsListButtonTemplate mechanism, verbatim — ONE
/// UI-QuestLogTitleHighlight quad in ADD blend, two tints): the selected row's wash draws
/// additive in the LOCKED GOLD (1,1,0) at the era plate seat (187x21 — sized once in
/// OptionsCategoryRow_OnLoad); a moved selection reseats the single gold quad lower; and
/// hovering an UNSELECTED row lights the same texture in the steel-blue hover tint
/// (.196,.388,.8) while the locked gold stands — 1.14's LockHighlight guard.
#[test]
fn the_selected_row_wears_the_gold_wash_and_hover_runs_blue() {
    let mut s = harness();
    // Pin the window at scale 1 so row rects and the pointer share coordinates (the fit
    // clamp stays out of the way at 1024x768: both ratios sit above 1).
    s.run("ERA_WINDOW_SCALE = 1").unwrap();
    s.run("ShowUIPanel(OptionsFrame)").unwrap();

    // The plate seat: the era's 187x21, authored in the row OnLoad now that no
    // SetAtlas(name, true) sizes it.
    assert_eq!(
        s.eval::<f64>("return OptionsFrameCategoryListRowControlsBg:GetWidth()")
            .unwrap(),
        187.0
    );
    assert_eq!(
        s.eval::<f64>("return OptionsFrameCategoryListRowControlsBg:GetHeight()")
            .unwrap(),
        21.0
    );

    // Every visible category wash: rect + tint + blend.
    let washes = |s: &mut UiScript| -> Vec<(benilla_ui::layout::Rect, [f32; 4], bool)> {
        s.resolve();
        s.extract()
            .into_iter()
            .filter_map(|q| match &q.content {
                QuadContent::Texture {
                    path: Some(p),
                    color,
                    additive,
                    ..
                } if p.contains("UI-QuestLogTitleHighlight") => {
                    q.rect.map(|r| (r, color.unwrap_or([1.0; 4]), *additive))
                }
                _ => None,
            })
            .collect()
    };
    let gold =
        |c: &[f32; 4]| (c[0] - 1.0).abs() < 1e-3 && (c[1] - 1.0).abs() < 1e-3 && c[2].abs() < 1e-3;
    let blue = |c: &[f32; 4]| {
        (c[0] - 0.196).abs() < 1e-3 && (c[1] - 0.388).abs() < 1e-3 && (c[2] - 0.8).abs() < 1e-3
    };

    let before = washes(&mut s);
    assert_eq!(
        before.len(),
        1,
        "exactly one wash shows: the locked selection"
    );
    assert!(before[0].2, "the wash draws ADD — 1.14's alphaMode");
    assert!(gold(&before[0].1), "…in the locked gold: {:?}", before[0].1);

    s.run("OptionsFrameCategoryListRowAudio:Click()").unwrap();
    let after = washes(&mut s);
    assert_eq!(after.len(), 1, "the wash moved, not multiplied");
    assert!(gold(&after[0].1), "still the gold tint: {:?}", after[0].1);
    assert!(
        after[0].0.top < before[0].0.top,
        "Audio sits below Controls, so the wash reseats lower (top {} -> {})",
        before[0].0.top,
        after[0].0.top
    );

    // Hover the now-unselected Controls row: its wash lights BLUE beside Audio's gold.
    let (cx, cy) = {
        let l: f32 = s
            .eval("return OptionsFrameCategoryListRowControls:GetLeft()")
            .unwrap();
        let r: f32 = s
            .eval("return OptionsFrameCategoryListRowControls:GetRight()")
            .unwrap();
        let t: f32 = s
            .eval("return OptionsFrameCategoryListRowControls:GetTop()")
            .unwrap();
        let b: f32 = s
            .eval("return OptionsFrameCategoryListRowControls:GetBottom()")
            .unwrap();
        ((l + r) * 0.5, (t + b) * 0.5)
    };
    s.mouse_move(cx, cy);
    let hovered = washes(&mut s);
    assert_eq!(
        hovered.len(),
        2,
        "hover adds its wash beside the locked gold"
    );
    assert_eq!(hovered.iter().filter(|w| gold(&w.1)).count(), 1);
    assert_eq!(
        hovered.iter().filter(|w| blue(&w.1)).count(),
        1,
        "the hover tint is 1.14's steel-blue: {:?}",
        hovered.iter().map(|w| w.1).collect::<Vec<_>>()
    );

    // Leaving puts the hover wash away; the lock stays.
    s.mouse_move(5.0, 5.0);
    let left = washes(&mut s);
    assert_eq!(left.len(), 1);
    assert!(gold(&left[0].1));
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Search (0984: the era SettingsPanel mechanism transcribed): typing reflows the LIVE
/// matching rows under a clickable category head — a matched CHILD pulls its parent volume
/// row in above it (the era parentInitializer rule) — the title reads "Search Results" and
/// Defaults hides; clearing the box lands every row back on its authored XML chain exactly
/// (the one-layout-law pin) with the page view restored.
#[test]
fn search_reflows_live_rows_and_restores_the_page() {
    let mut s = harness_on(audio_harness());
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.run("OptionsFrameCategoryListRowAudio:Click()").unwrap();
    s.resolve();
    let authored_top: f32 = s
        .eval("return OptionsFrameContainerBodyAudioRowMaster:GetTop()")
        .unwrap();

    s.run("OptionsFrameSearchBox:SetText(\"music\")").unwrap();
    assert_eq!(
        s.eval::<String>("return OptionsFrameContainerTitle:GetText()")
            .unwrap(),
        "Search Results"
    );
    assert!(
        !s.eval::<bool>("return OptionsFrameContainerDefaults:IsVisible()")
            .unwrap(),
        "Defaults hides while the search holds the page (the era SetShown(not hasText))"
    );
    // One head — Audio's; the matches plus their pulled-in parent; nothing else.
    for (frame, shown) in [
        ("OptionsFrameContainerBodySearchHeadAudio", true),
        ("OptionsFrameContainerBodySearchHeadControls", false),
        ("OptionsFrameContainerBodySearchHeadGraphics", false),
        ("OptionsFrameContainerBodyAudioRowMusic", true),
        ("OptionsFrameContainerBodyAudioRowEnableMusic", true),
        ("OptionsFrameContainerBodyAudioRowMaster", true), // the parent, pulled in unmatched
        ("OptionsFrameContainerBodyAudioRowSound", false),
        ("OptionsFrameContainerBodyAudioRowEnableAll", false),
        ("OptionsFrameContainerBodyControlsRowAutoLoot", false),
        ("OptionsFrameContainerBodyGraphicsRowUiScale", false),
        ("OptionsFrameContainerBodyNoResults", false),
    ] {
        assert_eq!(
            s.eval::<bool>(&format!("return {frame}:IsVisible()"))
                .unwrap(),
            shown,
            "{frame} shown={shown}"
        );
    }
    // The chain reads head → parent → the two matches, downward.
    s.resolve();
    let tops: Vec<f32> = [
        "OptionsFrameContainerBodySearchHeadAudio",
        "OptionsFrameContainerBodyAudioRowMaster",
        "OptionsFrameContainerBodyAudioRowMusic",
        "OptionsFrameContainerBodyAudioRowEnableMusic",
    ]
    .iter()
    .map(|f| s.eval::<f32>(&format!("return {f}:GetTop()")).unwrap())
    .collect();
    assert!(
        tops[0] > tops[1] && tops[1] > tops[2] && tops[2] > tops[3],
        "head, parent, then the matches chain downward: {tops:?}"
    );
    // The reflowed row is LIVE: moving the music slider writes its CVar mid-search.
    let _ = s.take_cvar_changes();
    s.run("OptionsFrameContainerBodyAudioRowMusicControlSlider:SetValue(0.25)")
        .unwrap();
    assert_eq!(
        s.take_cvar_changes(),
        vec![("MusicVolume".to_string(), "0.25".to_string())]
    );

    // Clearing restores the page view and the authored chain EXACTLY.
    s.run("OptionsFrameSearchBox:SetText(\"\")").unwrap();
    assert_eq!(
        s.eval::<String>("return OptionsFrameContainerTitle:GetText()")
            .unwrap(),
        "Audio"
    );
    assert!(s
        .eval::<bool>("return OptionsFrameContainerDefaults:IsVisible()")
        .unwrap());
    s.resolve();
    let restored_top: f32 = s
        .eval("return OptionsFrameContainerBodyAudioRowMaster:GetTop()")
        .unwrap();
    assert!(
        (restored_top - authored_top).abs() < 0.01,
        "the restore law equals the authored XML chain ({restored_top} vs {authored_top})"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The era's word scoring (Blizzard_Settings.lua MatchesSearchTags + the words list): the
/// WHOLE query is the first word, so a phrase match outscores its own tokens and chains
/// first within the group — "master volume" seats Master above the token-matched children.
#[test]
fn the_phrase_match_outranks_its_words() {
    let mut s = harness_on(audio_harness());
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.run("OptionsFrameSearchBox:SetText(\"master volume\")")
        .unwrap();
    s.resolve();
    let tops: Vec<f32> = [
        "OptionsFrameContainerBodyAudioRowMaster", // phrase hit, score 12
        "OptionsFrameContainerBodyAudioRowSound",  // "VOLUME" hits, score 5, page order
        "OptionsFrameContainerBodyAudioRowMusic",
        "OptionsFrameContainerBodyAudioRowAmbience",
    ]
    .iter()
    .map(|f| s.eval::<f32>(&format!("return {f}:GetTop()")).unwrap())
    .collect();
    assert!(
        tops[0] > tops[1] && tops[1] > tops[2] && tops[2] > tops[3],
        "phrase first, then the word matches in page order: {tops:?}"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The era's search exits: clicking a results head (the era's jump — SelectCategory clears
/// the box) and a no-match query (the section-header "no results" line, nothing chained).
#[test]
fn a_head_click_ends_the_search_and_a_miss_shows_no_results() {
    let s = harness();
    s.run("ShowUIPanel(OptionsFrame)").unwrap();

    s.run("OptionsFrameSearchBox:SetText(\"volume\")").unwrap();
    s.run("OptionsFrameContainerBodySearchHeadAudio:Click()")
        .unwrap();
    assert_eq!(
        s.eval::<String>("return OptionsFrameSearchBox:GetText()")
            .unwrap(),
        "",
        "the head click cleared the search (the era SelectCategory law)"
    );
    assert_eq!(
        s.eval::<String>("return OptionsFrame.selectedCategory")
            .unwrap(),
        "Audio"
    );
    assert_eq!(
        s.eval::<String>("return OptionsFrameContainerTitle:GetText()")
            .unwrap(),
        "Audio"
    );
    assert!(s
        .eval::<bool>("return OptionsFrameContainerBodyAudio:IsVisible()")
        .unwrap());

    s.run("OptionsFrameSearchBox:SetText(\"flibbertigibbet\")")
        .unwrap();
    assert!(s
        .eval::<bool>("return OptionsFrameContainerBodyNoResults:IsVisible()")
        .unwrap());
    for head in ["Controls", "Audio", "Graphics"] {
        assert!(
            !s.eval::<bool>(&format!(
                "return OptionsFrameContainerBodySearchHead{head}:IsVisible()"
            ))
            .unwrap(),
            "no head on a miss"
        );
    }
    s.run("OptionsFrameSearchBox:SetText(\"\")").unwrap();
    assert!(!s
        .eval::<bool>("return OptionsFrameContainerBodyNoResults:IsVisible()")
        .unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The bar IS the control (0989: steppers cut by the director's call; the engine's
/// track-press law): a LeftButton press on the slider's track — off the thumb — seats the
/// thumb under the cursor (the value jumps, the CVar write queues), the SAME press keeps
/// dragging (0250 §5's capture began), and the stepper buttons no longer exist.
#[test]
fn a_track_press_seats_the_thumb_and_keeps_dragging() {
    let mut s = harness_on(audio_harness());
    s.set_cvar_host("MasterVolume", "0.1");
    s.run("ERA_WINDOW_SCALE = 1").unwrap(); // pointer and rects share coordinates
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.run("OptionsFrameCategoryListRowAudio:Click()").unwrap();
    let _ = s.take_cvar_changes();

    // The steppers are gone.
    for btn in ["Back", "Forward"] {
        assert!(s
            .eval::<bool>(&format!(
                "return OptionsFrameContainerBodyAudioRowMasterControl{btn} == nil"
            ))
            .unwrap());
    }

    let slider = "OptionsFrameContainerBodyAudioRowMasterControlSlider";
    s.resolve(); // seat the rects before reading them (the wash test's idiom)
    let (l, r, t, b) = (
        s.eval::<f32>(&format!("return {slider}:GetLeft()"))
            .unwrap(),
        s.eval::<f32>(&format!("return {slider}:GetRight()"))
            .unwrap(),
        s.eval::<f32>(&format!("return {slider}:GetTop()")).unwrap(),
        s.eval::<f32>(&format!("return {slider}:GetBottom()"))
            .unwrap(),
    );
    let cy = (t + b) * 0.5;
    // Press 1 unit inside the RIGHT end of the track — far from the 0.1-seated thumb. The
    // center-grab seat clamps the fraction to 1.0 there (the cursor sits inside the thumb's
    // half-width end zone), so the value lands exactly at the max.
    s.mouse_button(r - 1.0, cy, "LeftButton", true);
    assert!(
        s.eval::<bool>(&format!(
            "return math.abs({slider}:GetValue() - 1.0) < 0.0001"
        ))
        .unwrap(),
        "the press itself seats the thumb"
    );
    assert_eq!(
        s.take_cvar_changes(),
        vec![("MasterVolume".to_string(), "1".to_string())]
    );
    // Still held: the same gesture drags on. The slider's midpoint is exactly fraction 0.5
    // ((mid − thumb/2 − left) / (width − thumb)), on the 0.05 grid, so no snap correction.
    s.mouse_move((l + r) * 0.5, cy);
    assert!(
        s.eval::<bool>(&format!(
            "return math.abs({slider}:GetValue() - 0.5) < 0.0001"
        ))
        .unwrap(),
        "the capture began at the press — the drag follows without re-grabbing"
    );
    assert_eq!(
        s.take_cvar_changes(),
        vec![("MasterVolume".to_string(), "0.5".to_string())]
    );
    s.mouse_button((l + r) * 0.5, cy, "LeftButton", false);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The Audio harness: the real registered CVar set on the table before the XML loads, exactly
/// the app's boot order (register → seed → load → select).
fn audio_harness() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.register_cvars(crate::cvars::REGISTERED.iter().copied());
    s
}

/// Selecting Audio shows the page body, arms Defaults, and every row reads the CVar table:
/// sliders take the stored value with the era rounded-percent readout, checkboxes take the flag.
/// Leaving the page hides it and puts Defaults back to sleep.
#[test]
fn the_audio_page_reads_the_cvar_table_on_select() {
    let mut s = audio_harness();
    s.set_cvar_host("MusicVolume", "0.7");
    s.set_cvar_host("EnableMusic", "0");
    let s = harness_on(s);
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.run("OptionsFrameCategoryListRowAudio:Click()").unwrap();

    assert!(s
        .eval::<bool>("return OptionsFrameContainerBodyAudio:IsVisible()")
        .unwrap());
    assert!(s
        .eval::<bool>("return OptionsFrameContainerDefaults:IsEnabled()")
        .unwrap());
    // The music slider holds the stored 0.7 (f32 wobble tolerated), readout "70%".
    assert!(s
        .eval::<bool>(
            "return math.abs(OptionsFrameContainerBodyAudioRowMusicControlSlider:GetValue() - 0.7) < 0.0001"
        )
        .unwrap());
    assert_eq!(
        s.eval::<String>("return OptionsFrameContainerBodyAudioRowMusicControlValue:GetText()")
            .unwrap(),
        "70%"
    );
    // Checkboxes: EnableMusic off, the master (default "1") on.
    assert!(!s
        .eval::<bool>("return OptionsFrameContainerBodyAudioRowEnableMusicCheck:GetChecked()")
        .unwrap());
    assert!(s
        .eval::<bool>("return OptionsFrameContainerBodyAudioRowEnableAllCheck:GetChecked()")
        .unwrap());

    // Off to a ROWLESS page: body hidden, Defaults disabled again.
    s.run("OptionsFrameCategoryListRowInterface:Click()")
        .unwrap();
    assert!(!s
        .eval::<bool>("return OptionsFrameContainerBodyAudio:IsVisible()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return OptionsFrameContainerDefaults:IsEnabled()")
        .unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// A user move snaps to the era 5% grid (obeyStepOnDrag transcribed) and writes the CVar as a
/// clean short string — the change queue carries what config.toml will store. A refresh write
/// (the page reading the table) queues nothing.
#[test]
fn a_slider_move_snaps_and_writes_the_cvar() {
    let mut s = harness_on(audio_harness());
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.run("OptionsFrameCategoryListRowAudio:Click()").unwrap();
    assert!(
        s.take_cvar_changes().is_empty(),
        "reading the table on select must not write it back"
    );

    // An off-grid move (what a drag delivers) snaps to 0.45 and queues exactly that.
    s.run("OptionsFrameContainerBodyAudioRowMasterControlSlider:SetValue(0.43)")
        .unwrap();
    assert!(s
        .eval::<bool>(
            "return math.abs(OptionsFrameContainerBodyAudioRowMasterControlSlider:GetValue() - 0.45) < 0.0001"
        )
        .unwrap());
    assert_eq!(
        s.eval::<String>("return OptionsFrameContainerBodyAudioRowMasterControlValue:GetText()")
            .unwrap(),
        "45%"
    );
    assert_eq!(
        s.take_cvar_changes(),
        vec![("MasterVolume".to_string(), "0.45".to_string())]
    );

    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The checkbox rows write their flag CVar on the 1.12 panel's own (quirky) click kits, and the
/// 1.12 dependency holds: Enable All Sound off greys exactly the Enable Ambience row.
#[test]
fn the_checkbox_rows_write_flags_and_the_master_greys_ambience() {
    let mut s = harness_on(audio_harness());
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.run("OptionsFrameCategoryListRowAudio:Click()").unwrap();
    let _ = s.take_cvar_changes();
    let _ = s.take_sounds();

    // Uncheck the master: flag "0" queued, ambience greyed, music left alive (the 1.12 quirk),
    // and the just-UNchecked box plays the CheckBoxOn kit (SoundOptionsFrame.lua verbatim).
    s.run("OptionsFrameContainerBodyAudioRowEnableAllCheck:Click()")
        .unwrap();
    assert_eq!(
        s.take_cvar_changes(),
        vec![("MasterSoundEffects".to_string(), "0".to_string())]
    );
    assert!(!s
        .eval::<bool>("return OptionsFrameContainerBodyAudioRowEnableAmbienceCheck:IsEnabled()")
        .unwrap());
    assert!(s
        .eval::<bool>("return OptionsFrameContainerBodyAudioRowEnableMusicCheck:IsEnabled()")
        .unwrap());
    assert!(s
        .take_sounds()
        .contains(&SoundRequest::KitName("igMainMenuOptionCheckBoxOn".into())));

    // Re-check: flag "1", ambience live again.
    s.run("OptionsFrameContainerBodyAudioRowEnableAllCheck:Click()")
        .unwrap();
    assert_eq!(
        s.take_cvar_changes(),
        vec![("MasterSoundEffects".to_string(), "1".to_string())]
    );
    assert!(s
        .eval::<bool>("return OptionsFrameContainerBodyAudioRowEnableAmbienceCheck:IsEnabled()")
        .unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Defaults walks every Audio row's CVar back to its registered default and the rows follow —
/// the era per-page reset, on the one page with rows.
#[test]
fn defaults_resets_the_audio_page_to_registered_defaults() {
    let mut s = audio_harness();
    s.set_cvar_host("MusicVolume", "0.9");
    s.set_cvar_host("EnableMusic", "0");
    let mut s = harness_on(s);
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.run("OptionsFrameCategoryListRowAudio:Click()").unwrap();
    let _ = s.take_cvar_changes();

    s.run("OptionsFrameContainerDefaults:Click()").unwrap();
    let changes = s.take_cvar_changes();
    assert!(
        changes.contains(&("MusicVolume".to_string(), "0.4".to_string())),
        "music back to its 1.12 registration default: {changes:?}"
    );
    assert!(
        changes.contains(&("EnableMusic".to_string(), "1".to_string())),
        "the flag back on: {changes:?}"
    );
    // Only the MOVED values queue — the rows already at default write nothing.
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert!(s
        .eval::<bool>("return OptionsFrameContainerBodyAudioRowEnableMusicCheck:GetChecked()")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return OptionsFrameContainerBodyAudioRowMusicControlValue:GetText()")
            .unwrap(),
        "40%"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Selecting Graphics shows ITS page body (0959; one row since the farclip retirement, 0961)
/// with the uiScale slider reading the table on the 0.64..1.0 panel range with the percent
/// readout. The swap works both ways — Audio's body takes over when clicked.
#[test]
fn the_graphics_page_reads_the_cvar_table_on_select() {
    let mut s = audio_harness();
    s.set_cvar_host("uiScale", "0.8");
    let s = harness_on(s);
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.run("OptionsFrameCategoryListRowGraphics:Click()")
        .unwrap();

    assert!(s
        .eval::<bool>("return OptionsFrameContainerBodyGraphics:IsVisible()")
        .unwrap());
    assert!(s
        .eval::<bool>("return OptionsFrameContainerDefaults:IsEnabled()")
        .unwrap());
    assert!(s
        .eval::<bool>(
            "return math.abs(OptionsFrameContainerBodyGraphicsRowUiScaleControlSlider:GetValue() - 0.8) < 0.0001"
        )
        .unwrap());
    assert_eq!(
        s.eval::<String>(
            "return OptionsFrameContainerBodyGraphicsRowUiScaleControlValue:GetText()"
        )
        .unwrap(),
        "80%"
    );
    // The label is the 1.12 GlobalStrings' own.
    assert_eq!(
        s.eval::<String>("return OptionsFrameContainerBodyGraphicsRowUiScaleLabel:GetText()")
            .unwrap(),
        "UI Scale"
    );

    // The swap, the other way: Audio in, Graphics out.
    s.run("OptionsFrameCategoryListRowAudio:Click()").unwrap();
    assert!(!s
        .eval::<bool>("return OptionsFrameContainerBodyGraphics:IsVisible()")
        .unwrap());
    assert!(s
        .eval::<bool>("return OptionsFrameContainerBodyAudio:IsVisible()")
        .unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The dropdown row (0992, the first): the capsule reads the CVar on select (text from the
/// row's own entries), the capsule button opens the shared list at the OWNER's effective scale
/// (the kit's 0992 uiScale correction — the window rides SetScale) with the current value's
/// entry checked, and an entry click writes the CVar, repaints the capsule, and closes the
/// list on the kit's own law.
#[test]
fn the_world_detail_dropdown_writes_the_cvar_and_the_capsule_follows() {
    let mut s = audio_harness();
    s.set_cvar_host("WorldDetail", "0");
    let mut s = harness_on(s);
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.run("OptionsFrameCategoryListRowGraphics:Click()")
        .unwrap();
    assert_eq!(
        s.eval::<String>(
            "return OptionsFrameContainerBodyGraphicsRowWorldDetailDropdownText:GetText()"
        )
        .unwrap(),
        "Low"
    );
    assert_eq!(
        s.eval::<String>("return OptionsFrameContainerBodyGraphicsRowWorldDetailLabel:GetText()")
            .unwrap(),
        "Environment Detail"
    );
    assert!(
        s.take_cvar_changes().is_empty(),
        "reading the table on select must not write it back"
    );

    // The capsule button opens the list: three entries, the stored value's row checked, and
    // the shared list wearing the window's effective scale (0.78 era scale, checkFit capped).
    s.run("OptionsFrameContainerBodyGraphicsRowWorldDetailDropdownButton:Click()")
        .unwrap();
    assert!(s.eval::<bool>("return DropDownList1:IsVisible()").unwrap());
    assert_eq!(
        s.eval::<f64>("return DropDownList1.numButtons").unwrap(),
        3.0
    );
    assert!(s
        .eval::<bool>("return DropDownList1Button1Check:IsVisible()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return DropDownList1Button3Check:IsVisible()")
        .unwrap());
    assert!(s
        .eval::<bool>(
            "return math.abs(DropDownList1:GetScale() - OptionsFrame:GetScale()) < 0.0001"
        )
        .unwrap());

    // Clicking High: the write queues, the capsule repaints, the list closes.
    s.run("DropDownList1Button3:Click()").unwrap();
    assert_eq!(
        s.take_cvar_changes(),
        vec![("WorldDetail".to_string(), "2".to_string())]
    );
    assert_eq!(
        s.eval::<String>(
            "return OptionsFrameContainerBodyGraphicsRowWorldDetailDropdownText:GetText()"
        )
        .unwrap(),
        "High"
    );
    assert!(!s.eval::<bool>("return DropDownList1:IsVisible()").unwrap());

    // An off-grid value (an env A/B: the hermetic capture's clutter-off session seeds "-1")
    // displays the NEAREST stop — 0959's out-of-range law — and writes nothing back.
    s.set_cvar_host("WorldDetail", "-1");
    s.run("OptionsFrameCategoryListRowAudio:Click(); OptionsFrameCategoryListRowGraphics:Click()")
        .unwrap();
    assert_eq!(
        s.eval::<String>(
            "return OptionsFrameContainerBodyGraphicsRowWorldDetailDropdownText:GetText()"
        )
        .unwrap(),
        "Low"
    );
    assert!(
        s.take_cvar_changes().is_empty(),
        "the nearest-stop display must not write back"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The Nameplates page (0992 — live at last): the three 1.12 UnitName* checkbox rows read the
/// table on select and write their flags on the interface panel's own click kit (checked →
/// CheckBoxOn; these rows carry no soundQuirk).
#[test]
fn the_nameplates_page_toggles_the_unit_name_cvars() {
    let mut s = harness_on(audio_harness());
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.run("OptionsFrameCategoryListRowNameplates:Click()")
        .unwrap();
    assert!(s
        .eval::<bool>("return OptionsFrameContainerBodyNameplates:IsVisible()")
        .unwrap());
    assert!(s
        .eval::<bool>("return OptionsFrameContainerDefaults:IsEnabled()")
        .unwrap());
    // All three ride their registered "1" defaults.
    for row in ["RowPlayerNames", "RowNpcNames", "RowOwnName"] {
        assert!(
            s.eval::<bool>(&format!(
                "return OptionsFrameContainerBodyNameplates{row}Check:GetChecked()"
            ))
            .unwrap(),
            "{row} defaults checked"
        );
    }
    let _ = s.take_sounds();

    // Unchecking NPC Names queues the flag off and plays the OFF kit (no quirk here — the
    // 1.12 interface panel's PlayClickSound mapping, not the sound panel's inverted one).
    s.run("OptionsFrameContainerBodyNameplatesRowNpcNamesCheck:Click()")
        .unwrap();
    assert_eq!(
        s.take_cvar_changes(),
        vec![("UnitNameNPC".to_string(), "0".to_string())]
    );
    assert!(s
        .take_sounds()
        .contains(&SoundRequest::KitName("igMainMenuOptionCheckBoxOff".into())));
    s.run("OptionsFrameContainerBodyNameplatesRowNpcNamesCheck:Click()")
        .unwrap();
    assert_eq!(
        s.take_cvar_changes(),
        vec![("UnitNameNPC".to_string(), "1".to_string())]
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The uiScale row DEFERS (0961, era CommitFlag.Apply transcribed): moves snap to the 1.12
/// 0.01 grid and update the readout, but the CVar does not move — the Apply button appears
/// instead, commits the pending value on click, and disappears. Dragging back onto the
/// committed value clears the pending without a commit (era's IsModified).
#[test]
fn the_ui_scale_slider_defers_to_the_apply_button() {
    let mut s = harness_on(audio_harness());
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.run("OptionsFrameCategoryListRowGraphics:Click()")
        .unwrap();
    assert!(
        s.take_cvar_changes().is_empty(),
        "reading the table on select must not write it back"
    );
    assert!(
        !s.eval::<bool>("return OptionsFrameApplyButton:IsVisible()")
            .unwrap(),
        "no pending edit, no Apply button"
    );

    // Off-grid 0.787 snaps to 0.79 and the readout follows — but NOTHING queues; the Apply
    // button exists now (era shows and enables it together).
    s.run("OptionsFrameContainerBodyGraphicsRowUiScaleControlSlider:SetValue(0.787)")
        .unwrap();
    assert_eq!(
        s.eval::<String>(
            "return OptionsFrameContainerBodyGraphicsRowUiScaleControlValue:GetText()"
        )
        .unwrap(),
        "79%"
    );
    assert!(
        s.take_cvar_changes().is_empty(),
        "a deferred row must not write the CVar on the move"
    );
    assert!(s
        .eval::<bool>("return OptionsFrameApplyButton:IsVisible()")
        .unwrap());
    assert!(s
        .eval::<bool>("return OptionsFrameApplyButton:IsEnabled()")
        .unwrap());

    // A second move re-stages the pending value — still nothing queues (0989: the steppers
    // are gone; a drag's SetValue is the remaining move).
    s.run("OptionsFrameContainerBodyGraphicsRowUiScaleControlSlider:SetValue(0.8)")
        .unwrap();
    assert!(s.take_cvar_changes().is_empty());

    // Apply commits the LAST pending value, once, and the button goes away.
    s.run("OptionsFrameApplyButton:Click()").unwrap();
    assert_eq!(
        s.take_cvar_changes(),
        vec![("uiScale".to_string(), "0.8".to_string())]
    );
    assert!(!s
        .eval::<bool>("return OptionsFrameApplyButton:IsVisible()")
        .unwrap());

    // A move away arms Apply; dragging back onto the committed value disarms it.
    s.run("OptionsFrameContainerBodyGraphicsRowUiScaleControlSlider:SetValue(0.79)")
        .unwrap();
    assert!(s
        .eval::<bool>("return OptionsFrameApplyButton:IsVisible()")
        .unwrap());
    s.run("OptionsFrameContainerBodyGraphicsRowUiScaleControlSlider:SetValue(0.8)")
        .unwrap();
    assert!(!s
        .eval::<bool>("return OptionsFrameApplyButton:IsVisible()")
        .unwrap());
    assert!(
        s.take_cvar_changes().is_empty(),
        "arming and disarming never touched the CVar"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Pending edits are PANEL-wide like era's modified table: they survive a category switch
/// (the row redisplays the pending value on return, era's GetValue-returns-pending) and die
/// only when the window hides — the reopened window reads the committed truth.
#[test]
fn a_pending_ui_scale_survives_the_page_switch_and_dies_on_hide() {
    let mut s = harness_on(audio_harness());
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.run("OptionsFrameCategoryListRowGraphics:Click()")
        .unwrap();
    s.run("OptionsFrameContainerBodyGraphicsRowUiScaleControlSlider:SetValue(0.7)")
        .unwrap();
    let _ = s.take_cvar_changes();

    // Off to Audio: the Apply button stays (the pending edit is not page-scoped)…
    s.run("OptionsFrameCategoryListRowAudio:Click()").unwrap();
    assert!(s
        .eval::<bool>("return OptionsFrameApplyButton:IsVisible()")
        .unwrap());
    // …and back: the slider shows the PENDING value, not the committed one.
    s.run("OptionsFrameCategoryListRowGraphics:Click()")
        .unwrap();
    assert_eq!(
        s.eval::<String>(
            "return OptionsFrameContainerBodyGraphicsRowUiScaleControlValue:GetText()"
        )
        .unwrap(),
        "70%"
    );

    // Hide discards (the era confirm dialog is cut): the reopened window reads the truth.
    s.run("HideUIPanel(OptionsFrame)").unwrap();
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.run("OptionsFrameCategoryListRowGraphics:Click()")
        .unwrap();
    assert_eq!(
        s.eval::<String>(
            "return OptionsFrameContainerBodyGraphicsRowUiScaleControlValue:GetText()"
        )
        .unwrap(),
        "90%"
    );
    assert!(!s
        .eval::<bool>("return OptionsFrameApplyButton:IsVisible()")
        .unwrap());
    assert!(
        s.take_cvar_changes().is_empty(),
        "the whole pending lifecycle never wrote the CVar"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Defaults on the Graphics page: uiScale back to its registered default (0.9), the row
/// following, ONLY the moved value queuing — and a pending edit dies with it (the default
/// write supersedes what Apply would have committed).
#[test]
fn defaults_resets_the_graphics_page_to_registered_defaults() {
    let mut s = audio_harness();
    s.set_cvar_host("uiScale", "0.8");
    let mut s = harness_on(s);
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.run("OptionsFrameCategoryListRowGraphics:Click()")
        .unwrap();
    // Stage a pending edit too — Defaults must kill it, not commit it.
    s.run("OptionsFrameContainerBodyGraphicsRowUiScaleControlSlider:SetValue(0.7)")
        .unwrap();
    let _ = s.take_cvar_changes();

    s.run("OptionsFrameContainerDefaults:Click()").unwrap();
    let changes = s.take_cvar_changes();
    assert_eq!(
        changes,
        vec![("uiScale".to_string(), "0.9".to_string())],
        "only the default write queues — never the dead pending"
    );
    assert_eq!(
        s.eval::<String>(
            "return OptionsFrameContainerBodyGraphicsRowUiScaleControlValue:GetText()"
        )
        .unwrap(),
        "90%"
    );
    assert!(!s
        .eval::<bool>("return OptionsFrameApplyButton:IsVisible()")
        .unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Controls is the DEFAULT page and has rows since 0961: opening the window lands on it with
/// Defaults armed, and the rows read the table — Sticky Targeting INVERTED (checked when
/// `deselectOnClick` is "0", the 1.12 interface panel's own arm), the plain flags direct.
#[test]
fn the_controls_page_reads_flags_with_the_sticky_inversion() {
    let mut s = audio_harness();
    s.set_cvar_host("deselectOnClick", "0");
    s.set_cvar_host("autoLootDefault", "1");
    let s = harness_on(s);
    s.run("ShowUIPanel(OptionsFrame)").unwrap();

    assert!(s
        .eval::<bool>("return OptionsFrameContainerBodyControls:IsVisible()")
        .unwrap());
    assert!(s
        .eval::<bool>("return OptionsFrameContainerDefaults:IsEnabled()")
        .unwrap());
    assert!(
        s.eval::<bool>("return OptionsFrameContainerBodyControlsRowStickyTargetCheck:GetChecked()")
            .unwrap(),
        "deselectOnClick '0' reads as Sticky Targeting CHECKED"
    );
    assert!(s
        .eval::<bool>("return OptionsFrameContainerBodyControlsRowAutoLootCheck:GetChecked()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return OptionsFrameContainerBodyControlsRowInvertMouseCheck:GetChecked()")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return OptionsFrameContainerBodyControlsRowStickyTargetLabel:GetText()")
            .unwrap(),
        "Sticky Targeting"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The Controls checkboxes write their flags on the INTERFACE panel's kit mapping (checked →
/// CheckBoxOn — OptionsFrame.lua's PlayClickSound, NOT the Audio page's inverted quirk), and
/// Sticky Targeting writes the CVar inverted both ways.
#[test]
fn the_controls_checkboxes_write_flags_with_the_interface_panel_kit() {
    let mut s = harness_on(audio_harness());
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    assert!(
        s.take_cvar_changes().is_empty(),
        "reading the table on open must not write it back"
    );
    let _ = s.take_sounds();

    // Invert Mouse on: flag "1", and the just-CHECKED box plays CheckBoxOn (normal mapping).
    s.run("OptionsFrameContainerBodyControlsRowInvertMouseCheck:Click()")
        .unwrap();
    assert_eq!(
        s.take_cvar_changes(),
        vec![("mouseInvertPitch".to_string(), "1".to_string())]
    );
    assert!(s
        .take_sounds()
        .contains(&SoundRequest::KitName("igMainMenuOptionCheckBoxOn".into())));

    // Sticky Targeting on: the write INVERTS — checking it writes deselectOnClick "0".
    s.run("OptionsFrameContainerBodyControlsRowStickyTargetCheck:Click()")
        .unwrap();
    assert_eq!(
        s.take_cvar_changes(),
        vec![("deselectOnClick".to_string(), "0".to_string())]
    );

    // …and off again: back to "1", the just-UNchecked box on the CheckBoxOff kit.
    s.run("OptionsFrameContainerBodyControlsRowStickyTargetCheck:Click()")
        .unwrap();
    assert_eq!(
        s.take_cvar_changes(),
        vec![("deselectOnClick".to_string(), "1".to_string())]
    );
    assert!(s
        .take_sounds()
        .contains(&SoundRequest::KitName("igMainMenuOptionCheckBoxOff".into())));
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Defaults on the Controls page: the moved flags come back (deselectOnClick "1",
/// autoLootDefault "0"), the rows follow, and only the moved values queue.
#[test]
fn defaults_resets_the_controls_page_to_registered_defaults() {
    let mut s = audio_harness();
    s.set_cvar_host("deselectOnClick", "0");
    s.set_cvar_host("autoLootDefault", "1");
    let mut s = harness_on(s);
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    let _ = s.take_cvar_changes();

    s.run("OptionsFrameContainerDefaults:Click()").unwrap();
    let changes = s.take_cvar_changes();
    assert!(
        changes.contains(&("deselectOnClick".to_string(), "1".to_string())),
        "{changes:?}"
    );
    assert!(
        changes.contains(&("autoLootDefault".to_string(), "0".to_string())),
        "{changes:?}"
    );
    assert_eq!(changes.len(), 2, "only the moved values queue: {changes:?}");
    assert!(
        !s.eval::<bool>(
            "return OptionsFrameContainerBodyControlsRowStickyTargetCheck:GetChecked()"
        )
        .unwrap(),
        "deselectOnClick back at '1' reads as Sticky Targeting unchecked"
    );
    assert!(!s
        .eval::<bool>("return OptionsFrameContainerBodyControlsRowAutoLootCheck:GetChecked()")
        .unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Drive the window a few frames so the frame-late fits (the scroll body, the tab widths)
/// converge: rects resolve after Lua runs, so OptionsScroll_Fit answers one frame behind.
fn settle(s: &mut UiScript) {
    for _ in 0..4 {
        s.resolve();
        s.tick(0.016);
    }
    s.resolve();
}

/// B217: a search that matches across categories chains five group heads and every matched row
/// into one column — ~150 units past the page area, which before the page scrolled drew straight
/// out through the dialog border onto the world. Now the body is the scroll child: it grows to the
/// content, the bar and its trough appear, and everything past the fold is CLIPPED to the page
/// rect until scrolled to.
#[test]
fn a_broad_search_scrolls_the_page_instead_of_overflowing_it() {
    let mut s = harness_on(audio_harness());
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    settle(&mut s);

    // Control first: a settled page fits, so there is no scroll and no bar at all — and the body
    // sits exactly on the page rect, the seat every page's XML anchors were authored against.
    assert_eq!(
        s.eval::<f32>("return OptionsFrameContainerScroll:GetVerticalScrollRange()")
            .unwrap(),
        0.0,
        "the Controls page fits its area"
    );
    for f in [
        "OptionsFrameContainerScrollBar",
        "OptionsFrameContainerScrollBarTrough",
    ] {
        assert!(
            !s.eval::<bool>(&format!("return {f}:IsVisible()")).unwrap(),
            "{f} stays away while the page fits"
        );
    }
    let page_h: f32 = s
        .eval("return OptionsFrameContainerScroll:GetHeight()")
        .unwrap();
    assert!(
        (s.eval::<f32>("return OptionsFrameContainerBody:GetHeight()")
            .unwrap()
            - page_h)
            .abs()
            < 0.5,
        "the body is the page rect when nothing overflows"
    );

    // Now the broad search: every category surfaces, and the column outruns the page.
    s.run("OptionsFrameSearchBox:SetText(\"e\")").unwrap();
    settle(&mut s);
    let range: f32 = s
        .eval("return OptionsFrameContainerScroll:GetVerticalScrollRange()")
        .unwrap();
    assert!(
        range > 100.0,
        "the results outrun the page, so there is something to scroll (range {range})"
    );
    for f in [
        "OptionsFrameContainerScrollBar",
        "OptionsFrameContainerScrollBarTrough",
    ] {
        assert!(
            s.eval::<bool>(&format!("return {f}:IsVisible()")).unwrap(),
            "{f} appears with the overflow"
        );
    }

    // THE SYMPTOM: nothing under the page draws past its bottom edge any more. Every quad the
    // page's own content emits carries the page rect as its clip (the engine's ScrollFrame
    // mechanism), so the deepest DRAWN pixel is the page's own bottom — while the content's own
    // rects still reach far below it, which is exactly the spill that used to be on screen.
    let quads = s.extract();
    let clip = quads
        .iter()
        .find_map(|q| q.clip)
        .expect("the page clips its content");
    assert!(
        quads.iter().all(|q| q.clip.is_none_or(|c| c == clip)),
        "one clip in play: the page area"
    );
    let deepest_rect = quads
        .iter()
        .filter(|q| q.clip.is_some())
        .filter_map(|q| q.rect)
        .map(|r| r.bottom)
        .fold(f32::INFINITY, f32::min);
    assert!(
        deepest_rect < clip.bottom - 100.0,
        "there is a real fold to make: the results reach {deepest_rect} against a page bottom of {}",
        clip.bottom
    );
    let deepest_drawn = quads
        .iter()
        .filter_map(|q| Some((q.rect?, q.clip?)))
        .map(|(r, c)| r.bottom.max(c.bottom))
        .fold(f32::INFINITY, f32::min);
    assert!(
        deepest_drawn >= clip.bottom - 0.01,
        "…and it is folded AT the page bottom, not spilling ({deepest_drawn} vs {})",
        clip.bottom
    );

    // The last result is reachable: scrolling to the end brings it inside the page rect. (Widget
    // coordinates here, not the extract's screen px — the window carries ERA_WINDOW_SCALE.)
    let sf_bottom: f32 = s
        .eval("return OptionsFrameContainerScroll:GetBottom()")
        .unwrap();
    let tail = |s: &UiScript| -> f32 { s.eval("return OptionsScroll_ContentBottom()").unwrap() };
    assert!(
        tail(&s) < sf_bottom,
        "the tail starts below the fold ({} vs {sf_bottom})",
        tail(&s)
    );
    s.run(&format!("OptionsFrameContainerScrollBar:SetValue({range})"))
        .unwrap();
    settle(&mut s);
    assert!(
        tail(&s) >= sf_bottom - 0.5,
        "scrolling to the end brings the tail into the page ({} vs {sf_bottom})",
        tail(&s)
    );

    // Clearing the search puts the page — and the bar with its trough — back.
    s.run("OptionsFrameSearchBox:SetText(\"\")").unwrap();
    settle(&mut s);
    assert_eq!(
        s.eval::<f32>("return OptionsFrameContainerScroll:GetVerticalScrollRange()")
            .unwrap(),
        0.0
    );
    for f in [
        "OptionsFrameContainerScrollBar",
        "OptionsFrameContainerScrollBarTrough",
    ] {
        assert!(
            !s.eval::<bool>(&format!("return {f}:IsVisible()")).unwrap(),
            "{f} goes away with the results"
        );
    }
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The bar's TROUGH — the recessed channel it rides in, the "background and border" a bare
/// arrows-and-knob bar was missing. It is the shared kit's template on 1.12's own
/// UI-Character-ScrollBar channel art over the ref's black backing, and it seats ITSELF on its bar
/// at the ref's hang: 31 wide against the bar's 16, 8 units left, and — the part B224 caught —
/// overhanging the bar by 21 above and 20 below, which is what drops each arrow button into the
/// 16-tall SOCKET the art carries for it. (The Keybindings page's own bar wears the same one —
/// keybindings_tests, where the harness has a real binding registry to overflow the list with.)
#[test]
fn the_page_scroll_bar_wears_the_trough_with_its_arrows_in_the_sockets() {
    let mut s = harness_on(audio_harness());
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.run("OptionsFrameSearchBox:SetText(\"e\")").unwrap();
    settle(&mut s);

    let bar = "OptionsFrameContainerScrollBar";
    let trough = "OptionsFrameContainerScrollBarTrough";
    let g = |f: &str, m: &str| s.eval::<f32>(&format!("return {f}:{m}()")).unwrap();
    assert!(
        (g(trough, "GetWidth") - 31.0).abs() < 0.01,
        "the trough is the 31-wide channel"
    );
    assert!(
        (g(bar, "GetWidth") - 16.0).abs() < 0.01,
        "against a 16-wide bar"
    );
    assert!(
        (g(bar, "GetLeft") - g(trough, "GetLeft") - 8.0).abs() < 0.01,
        "the trough hangs the ref's 8 units left of the bar, so the bar rides it centred"
    );
    // THE SOCKET LAW (BenillaScrollTrough_Seat): the trough overhangs the bar by 21/20 — not the
    // arrow-to-arrow 16/16 that B224 reported, which sat both arrows 4 units OUT of their sockets,
    // riding the caps with bare socket showing beside them. Ref: ReputationFrame's trough at the
    // scroll frame's +5/-4 against a UIPanelScrollBarTemplate bar at -16/+16.
    assert!(
        (g(trough, "GetTop") - g(bar, "GetTop") - 21.0).abs() < 0.01,
        "trough top 21 above the bar"
    );
    assert!(
        (g(bar, "GetBottom") - g(trough, "GetBottom") - 20.0).abs() < 0.01,
        "trough bottom 20 below the bar"
    );
    // Read the same law off the ARROWS, which is what the eye actually judges: 5 units of top cap
    // above the up arrow, 4 of bottom cap below the down arrow, and the 16-tall sockets between.
    let up = format!("{bar}ScrollUpButton");
    let down = format!("{bar}ScrollDownButton");
    assert!((g(&up, "GetHeight") - 16.0).abs() < 0.01, "16-tall arrows");
    assert!((g(&down, "GetHeight") - 16.0).abs() < 0.01);
    assert!(
        (g(trough, "GetTop") - g(&up, "GetTop") - 5.0).abs() < 0.01,
        "the cap shows 5 above the up arrow"
    );
    assert!(
        (g(&down, "GetBottom") - g(trough, "GetBottom") - 4.0).abs() < 0.01,
        "and 4 below the down arrow"
    );
    // …and on THIS page the whole channel lands on the page rect: the scroll frame is the page
    // area, so the bar takes the 21/20 inset rather than the trough poking out through the header
    // divider above and the container's bottom below.
    let sf = "OptionsFrameContainerScroll";
    assert!((g(trough, "GetTop") - g(sf, "GetTop")).abs() < 0.01);
    assert!((g(trough, "GetBottom") - g(sf, "GetBottom")).abs() < 0.01);

    // The art: three channel slices from the one 1.12 file — top cap, stretched run, bottom cap —
    // each spanning the channel's full width, stacked with no gap and no overlap.
    let mut slices: Vec<_> = s
        .extract()
        .into_iter()
        .filter_map(|q| match &q.content {
            QuadContent::Texture { path: Some(p), .. } if p.contains("UI-Character-ScrollBar") => {
                q.rect
            }
            _ => None,
        })
        .collect();
    assert_eq!(slices.len(), 3, "top cap, stretched run, bottom cap");
    slices.sort_by(|a, b| b.top.total_cmp(&a.top));
    assert!(
        slices
            .windows(2)
            .all(|w| (w[0].bottom - w[1].top).abs() < 0.01),
        "the three stack flush into one channel: {slices:?}"
    );
    let width = slices[0].right - slices[0].left;
    assert!(
        slices
            .iter()
            .all(|r| (r.right - r.left - width).abs() < 0.01),
        "…all at the channel's own width: {slices:?}"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

// ── B223 · the row tooltips (decision 1054) ─────────────────────────────────────────────────────

/// Hover the middle of a named row's LABEL half (left of the control column) — the surface the
/// reporter's cursor was on when the row lit and said nothing. Callers pin `ERA_WINDOW_SCALE = 1`
/// first (the wash test's own idiom): frame rects are in the frame's scale space, the pointer is
/// in the screen's, and at scale 1 the two are the same numbers.
fn hover_label(s: &mut UiScript, frame: &str) {
    s.resolve();
    let g = |s: &mut UiScript, verb: &str| -> f32 {
        s.eval(&format!("return {frame}:{verb}()")).unwrap()
    };
    let (l, t, b) = (g(s, "GetLeft"), g(s, "GetTop"), g(s, "GetBottom"));
    s.mouse_move(l + 60.0, (t + b) * 0.5);
}

/// A hovered row raises its 1.12 description, on the era's seat, and drops it on leave — the row
/// itself, its checkbox, and (B223's report) the label the cursor actually crosses. The string
/// resolves by KEY at hover, so seeding it AFTER the window loaded still paints: that is the
/// property the 1.12 panel's own `getglobal("OPTION_TOOLTIP_"..key)` lookup buys.
#[test]
fn a_hovered_row_raises_its_1_12_description_on_the_era_seat() {
    let mut s = harness_on(audio_harness());
    s.run("OPTION_TOOLTIP_GAMEFIELD_DESELECT = \"Checking this will prevent the deselection.\"")
        .unwrap();
    s.run("ERA_WINDOW_SCALE = 1").unwrap();
    s.run("ShowUIPanel(OptionsFrame)").unwrap();

    let row = "OptionsFrameContainerBodyControlsRowStickyTarget";
    hover_label(&mut s, row);
    s.resolve();
    assert!(
        s.eval::<bool>("return GameTooltip:IsVisible()").unwrap(),
        "the label half raises the plate — the reported gap"
    );
    assert_eq!(
        s.eval::<String>("return GameTooltipTextLeft1:GetText()")
            .unwrap(),
        "Checking this will prevent the deselection."
    );
    assert_eq!(
        s.eval::<i64>("return GameTooltip:NumLines()").unwrap(),
        1,
        "the description ALONE — the era's white name line is cut (1054)"
    );
    // The era seat: BOTTOMLEFT on the label region's TOPRIGHT, hung 10 back over the control
    // column (DefaultTooltipMixin's ANCHOR_RIGHT / x -10).
    let owned: bool = s
        .eval(&format!("return GameTooltip:IsOwned({row}Tip)"))
        .unwrap();
    assert!(owned, "owned by the row's $parentTip region");
    let (tip_right, tip_top): (f32, f32) = (
        s.eval(&format!("return {row}Tip:GetRight()")).unwrap(),
        s.eval(&format!("return {row}Tip:GetTop()")).unwrap(),
    );
    let (left, bottom): (f32, f32) = (
        s.eval("return GameTooltip:GetLeft()").unwrap(),
        s.eval("return GameTooltip:GetBottom()").unwrap(),
    );
    assert!(
        (left - (tip_right - 10.0)).abs() < 0.01 && (bottom - tip_top).abs() < 0.01,
        "plate at ({left}, {bottom}); the era seat is ({}, {tip_top})",
        tip_right - 10.0
    );

    // Crossing onto the checkbox keeps it up (one seam lights the wash and raises the plate, so
    // the row's OnLeave and the box's OnEnter cancel out inside the one move).
    let (bl, br, bt, bb): (f32, f32, f32, f32) = (
        s.eval(&format!("return {row}Check:GetLeft()")).unwrap(),
        s.eval(&format!("return {row}Check:GetRight()")).unwrap(),
        s.eval(&format!("return {row}Check:GetTop()")).unwrap(),
        s.eval(&format!("return {row}Check:GetBottom()")).unwrap(),
    );
    s.mouse_move((bl + br) * 0.5, (bt + bb) * 0.5);
    assert!(
        s.eval::<bool>("return GameTooltip:IsVisible()").unwrap(),
        "the plate survives the crossing onto the control"
    );

    // Leaving the row puts it away.
    s.mouse_move(5.0, 5.0);
    assert!(
        !s.eval::<bool>("return GameTooltip:IsVisible()").unwrap(),
        "OnLeave drops the plate"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The row with no 1.12 string raises NOTHING — not a plate echoing the label under the cursor,
/// and not the LAST row's description left standing over it. Auto Loot is that row: 1.12 has no
/// Auto Loot setting, so no `OPTION_TOOLTIP_*` for it. The second leg is the live probe's find: a
/// hover driven without the outgoing row's OnLeave (1054) must still put the neighbour away.
#[test]
fn a_row_with_no_1_12_string_raises_no_plate() {
    let mut s = harness_on(audio_harness());
    s.run("OPTION_TOOLTIP_GAMEFIELD_DESELECT = \"Sticky's own description.\"")
        .unwrap();
    s.run("ERA_WINDOW_SCALE = 1").unwrap();
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    hover_label(&mut s, "OptionsFrameContainerBodyControlsRowAutoLoot");
    s.resolve();
    assert!(
        !s.eval::<bool>("return GameTooltip:IsVisible()").unwrap(),
        "no 1.12 description, no plate"
    );
    assert!(
        s.eval::<bool>("return OptionsFrameContainerBodyControlsRowAutoLootHover:IsVisible()")
            .unwrap(),
        "…but the row still lights: the wash is not gated on the string"
    );

    // Sticky's plate up, then straight into the mute row with no OnLeave in between.
    s.run("OptionsRow_Hover(OptionsFrameContainerBodyControlsRowStickyTarget, 1)")
        .unwrap();
    assert!(s.eval::<bool>("return GameTooltip:IsVisible()").unwrap());
    s.run("OptionsRow_Hover(OptionsFrameContainerBodyControlsRowAutoLoot, 1)")
        .unwrap();
    assert!(
        !s.eval::<bool>("return GameTooltip:IsVisible()").unwrap(),
        "the mute row puts the neighbour's description away — it never describes THIS row"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The RUNTIME leg on the real client data (`ui_quest`'s pattern): every key the rows carry
/// resolves to a non-empty string in the shipped 1.12 `GlobalStrings.lua` — the guard against a
/// typo'd key degrading a description to silence — and the ONE row without a key is Auto Loot.
/// Pins the reporter's own row end to end (B223's screenshot text). Skips without client data.
#[test]
fn every_row_tooltip_key_resolves_in_the_real_global_strings() {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
    let mut chain = benilla_formats::open_chain(&data).expect("open chain");
    let src = chain
        .read_file("Interface\\FrameXML\\GlobalStrings.lua")
        .expect("GlobalStrings.lua in the chain");
    let strings = UiScript::new().expect("VM");
    strings
        .run(&String::from_utf8_lossy(&src))
        .expect("GlobalStrings runs clean");

    // The mapping, read off the LIVE rows so a page added later can't slip the check.
    let s = harness();
    let listing: String = s
        .eval(
            "local out = {} \
             for page, rows in pairs(OPTIONS_PAGE_ROWS) do \
               for _, rkey in ipairs(rows) do \
                 local row = getglobal(\"OptionsFrameContainerBody\" .. page .. rkey) \
                 table.insert(out, page .. rkey .. \"=\" .. (row.tip or \"\")) \
               end \
             end \
             return table.concat(out, \",\")",
        )
        .unwrap();

    let mut untipped = vec![];
    let mut checked = 0;
    for entry in listing.split(',') {
        let (row, key) = entry.split_once('=').expect("row=key");
        if key.is_empty() {
            untipped.push(row.to_string());
            continue;
        }
        assert!(
            key.starts_with("OPTION_TOOLTIP_"),
            "{row}: {key} is not a 1.12 option-tooltip key"
        );
        let text: String = strings
            .lua()
            .globals()
            .get::<String>(key)
            .unwrap_or_default();
        assert!(
            !text.is_empty() && text != "PLACE_HOLDER",
            "{row}: {key} resolves to nothing in the real GlobalStrings"
        );
        checked += 1;
    }
    assert_eq!(checked, 14, "every row but one carries a live 1.12 key");
    assert_eq!(
        untipped,
        vec!["ControlsRowAutoLoot".to_string()],
        "Auto Loot is the only row 1.12 never had"
    );

    // The reporter's row, byte for byte off the MPQ chain.
    assert_eq!(
        strings
            .lua()
            .globals()
            .get::<String>("OPTION_TOOLTIP_GAMEFIELD_DESELECT")
            .unwrap(),
        "Checking this will prevent the deselection of targets by clicking on the gamefield.  \
         Targets can only be cleared by pressing escape or clicking another target."
    );
}

/// EVERY row on every page, all three control flavors: hovering it raises a plate exactly when
/// the row carries a 1.12 key, and no row's hover errors. The teeth are the per-template
/// `$parentTip` seat — a flavor missing that region would take `SetOwner(nil, …)` and print a red
/// line instead of a plate, which no key-mapping check would ever see.
#[test]
fn every_flavor_of_row_raises_its_plate_from_the_page_it_lives_on() {
    let mut s = harness_on(audio_harness());
    // Stand-in strings for every key the rows name (the real texts are the data test's job).
    s.run(
        "for page, rows in pairs(OPTIONS_PAGE_ROWS) do \
           for _, rkey in ipairs(rows) do \
             local row = getglobal(\"OptionsFrameContainerBody\" .. page .. rkey) \
             if row.tip then setglobal(row.tip, \"described: \" .. rkey) end \
           end \
         end",
    )
    .unwrap();
    s.run("ERA_WINDOW_SCALE = 1").unwrap();
    s.run("ShowUIPanel(OptionsFrame)").unwrap();

    let mut raised = 0;
    for page in ["Controls", "Audio", "Graphics", "Nameplates"] {
        s.run(&format!("OptionsFrameCategoryListRow{page}:Click()"))
            .unwrap();
        let rows: String = s
            .eval(&format!(
                "return table.concat(OPTIONS_PAGE_ROWS.{page}, \",\")"
            ))
            .unwrap();
        for rkey in rows.split(',') {
            let row = format!("OptionsFrameContainerBody{page}{rkey}");
            hover_label(&mut s, &row);
            s.resolve();
            let tipped: bool = s.eval(&format!("return {row}.tip ~= nil")).unwrap();
            let shown: bool = s.eval("return GameTooltip:IsVisible()").unwrap();
            assert_eq!(
                shown, tipped,
                "{row}: plate shown {shown}, has key {tipped}"
            );
            if tipped {
                assert_eq!(
                    s.eval::<String>("return GameTooltipTextLeft1:GetText()")
                        .unwrap(),
                    format!("described: {rkey}"),
                    "{row}: the plate reads ITS OWN row's description"
                );
                assert!(
                    s.eval::<bool>(&format!("return GameTooltip:IsOwned({row}Tip)"))
                        .unwrap(),
                    "{row}: seated on its own $parentTip region"
                );
                raised += 1;
            }
            s.mouse_move(5.0, 5.0);
        }
        assert!(
            s.errors().is_empty(),
            "{page}: script errors: {:?}",
            s.errors()
        );
    }
    assert_eq!(raised, 14, "14 of the 15 rows have a 1.12 description");
}
