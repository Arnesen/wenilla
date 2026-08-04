//! The shipped `assets/ui/OptionsFrame.xml` — the era-skinned options window (decision 0950:
//! the shell; 0957: the Audio page; 0959: the Graphics page).
//!
//! What these guard: the file loads clean inside the real neighbourhood (Fonts + UiPanels +
//! GameMenuFrame); the menu's Options button is the door in (menu down, options up, on the ref's
//! own kit); Controls is the default category and the page title follows the selection; both
//! close spellings put the window away; and — against a hand-pushed atlas table, the
//! era_atlas_tests idiom — the selected row actually wears `options_list_active` and drops it
//! when the selection moves. The harness pushes NO atlas table by default: `SetAtlas` is a
//! warn-once no-op there by design (a missing extraction must not kill the UI), which is exactly
//! why the one atlas-backed test pushes its own two members.
//!
//! The page tests (0957 Audio, 0959 Graphics) run against the REAL registered CVar set
//! (`crate::cvars`): rows read the table on select, writes land on the change queue the host
//! drains, the snap grids and readouts hold (the era 5% volumes; the 1.12 uiscale 0.01 and
//! farclip min-anchored 60), the 1.12 master→ambience dependency greys, and Defaults walks the
//! visible page back to the registered defaults.

use benilla_ui::script::{EraAtlasEntry, SoundRequest, UiScript};

/// The window's real neighbourhood, in the manifest's own order (options before the menu — the
/// game_menu_tests::harness_with idiom, minus the extras this file never needs).
fn harness() -> UiScript {
    harness_on(UiScript::new().unwrap())
}

/// Load the four files onto a prepared script — split out so the atlas test can push its table
/// BEFORE the XML loads, the way the app does (ui_script::setup_script).
fn harness_on(mut s: UiScript) -> UiScript {
    s.set_screen_size(1024.0, 768.0);
    for file in [
        "Fonts.xml",
        "UiPanels.xml",
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

/// Both close spellings — the red Close and the corner X — hide the window on the shared
/// igMainMenuClose kit, and the Defaults button (nothing to default this slice) reads disabled.
#[test]
fn both_close_buttons_hide_the_window_and_defaults_is_disabled() {
    let mut s = harness();

    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    assert!(
        !s.eval::<bool>("return OptionsFrameContainerDefaults:IsEnabled()")
            .unwrap(),
        "Defaults is dead until the pages slice brings rows to default"
    );
    let _ = s.take_sounds();
    s.run("OptionsFrameCloseButton:Click()").unwrap();
    assert!(!s.eval::<bool>("return OptionsFrame:IsVisible()").unwrap());
    assert!(s
        .take_sounds()
        .contains(&SoundRequest::KitName("igMainMenuClose".into())));

    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    let _ = s.take_sounds();
    s.run("OptionsFrameClosePanelButton:Click()").unwrap();
    assert!(!s.eval::<bool>("return OptionsFrame:IsVisible()").unwrap());
    assert!(s
        .take_sounds()
        .contains(&SoundRequest::KitName("igMainMenuClose".into())));
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The row art, against a hand-pushed two-member atlas table (real manifest numbers, the
/// era_atlas_tests idiom): the selected row's bg wears `options_list_active` at atlas size, and
/// a moved selection strips the old row bare (`SetTexture(nil)` → GetAtlas nil). The window's
/// OTHER atlas members (nine-slice, divider, inner panel) stay unserved here on purpose — they
/// drain as warn-once misses, never errors.
#[test]
fn the_selected_row_wears_the_active_atlas_and_yields_it_on_reselect() {
    let mut s = UiScript::new().unwrap();
    s.set_era_atlases([
        (
            "options_list_active".to_string(),
            EraAtlasEntry {
                file: "era:textures/1318750.blp".to_string(),
                uv: [604.0 / 1024.0, 791.0 / 1024.0, 1.0 / 1024.0, 22.0 / 1024.0],
                size: [187.0, 21.0],
            },
        ),
        (
            "options_list_hover".to_string(),
            EraAtlasEntry {
                file: "era:textures/1318750.blp".to_string(),
                uv: [793.0 / 1024.0, 980.0 / 1024.0, 1.0 / 1024.0, 22.0 / 1024.0],
                size: [187.0, 21.0],
            },
        ),
    ]);
    let s = harness_on(s);

    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    assert_eq!(
        s.eval::<String>("return OptionsFrameCategoryListRowControlsBg:GetAtlas()")
            .unwrap(),
        "options_list_active",
        "the default selection wears the active art"
    );
    // useAtlasSize rode along: the bg is the member's nominal 187×21, wider than the 175 row —
    // the era's own look.
    assert_eq!(
        s.eval::<f64>("return OptionsFrameCategoryListRowControlsBg:GetWidth()")
            .unwrap(),
        187.0
    );

    s.run("OptionsFrameCategoryListRowAudio:Click()").unwrap();
    assert!(
        s.eval::<bool>("return OptionsFrameCategoryListRowControlsBg:GetAtlas() == nil")
            .unwrap(),
        "the old row stripped bare when the selection moved"
    );
    assert_eq!(
        s.eval::<String>("return OptionsFrameCategoryListRowAudioBg:GetAtlas()")
            .unwrap(),
        "options_list_active"
    );
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
    s.run("OptionsFrameCategoryListRowControls:Click()")
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

    // The steppers move one step on the era's own kit sound.
    let _ = s.take_sounds();
    s.run("OptionsFrameContainerBodyAudioRowMasterControlForward:Click()")
        .unwrap();
    assert_eq!(
        s.take_cvar_changes(),
        vec![("MasterVolume".to_string(), "0.5".to_string())]
    );
    assert!(s
        .take_sounds()
        .contains(&SoundRequest::KitName("igMainMenuOptionCheckBoxOn".into())));
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

/// Selecting Graphics shows ITS page body (0959) with both 1.12 sliders reading the table:
/// uiScale on the 0.64..1.0 panel range with the percent readout, farclip on 177..777 with the
/// raw-yards readout. The swap works both ways — Audio's body takes over when clicked.
#[test]
fn the_graphics_page_reads_the_cvar_table_on_select() {
    let mut s = audio_harness();
    s.set_cvar_host("uiScale", "0.8");
    s.set_cvar_host("farclip", "297");
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
    assert!(s
        .eval::<bool>(
            "return math.abs(OptionsFrameContainerBodyGraphicsRowFarclipControlSlider:GetValue() - 297) < 0.001"
        )
        .unwrap());
    assert_eq!(
        s.eval::<String>(
            "return OptionsFrameContainerBodyGraphicsRowFarclipControlValue:GetText()"
        )
        .unwrap(),
        "297"
    );
    // The labels are the 1.12 GlobalStrings' own.
    assert_eq!(
        s.eval::<String>("return OptionsFrameContainerBodyGraphicsRowFarclipLabel:GetText()")
            .unwrap(),
        "Terrain Distance"
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

/// The Graphics sliders snap to the 1.12 panel grids and write clean short strings: farclip's
/// grid is 177+n*60 — ANCHORED AT THE MINIMUM, so 300 lands on 297, not the multiple-of-60
/// 300 — and uiscale's is the 0.01 percent grid. Selecting the page queues nothing.
#[test]
fn the_graphics_sliders_snap_to_the_1_12_grids_and_write_clean_strings() {
    let mut s = harness_on(audio_harness());
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.run("OptionsFrameCategoryListRowGraphics:Click()")
        .unwrap();
    assert!(
        s.take_cvar_changes().is_empty(),
        "reading the table on select must not write it back"
    );

    // farclip: a drag to 300 snaps DOWN the min-anchored grid to 297.
    s.run("OptionsFrameContainerBodyGraphicsRowFarclipControlSlider:SetValue(300)")
        .unwrap();
    assert!(s
        .eval::<bool>(
            "return math.abs(OptionsFrameContainerBodyGraphicsRowFarclipControlSlider:GetValue() - 297) < 0.001"
        )
        .unwrap());
    assert_eq!(
        s.eval::<String>(
            "return OptionsFrameContainerBodyGraphicsRowFarclipControlValue:GetText()"
        )
        .unwrap(),
        "297"
    );
    assert_eq!(
        s.take_cvar_changes(),
        vec![("farclip".to_string(), "297".to_string())]
    );

    // The stepper walks one 60-yard step up the same grid, on the era kit.
    let _ = s.take_sounds();
    s.run("OptionsFrameContainerBodyGraphicsRowFarclipControlForward:Click()")
        .unwrap();
    assert_eq!(
        s.take_cvar_changes(),
        vec![("farclip".to_string(), "357".to_string())]
    );
    assert!(s
        .take_sounds()
        .contains(&SoundRequest::KitName("igMainMenuOptionCheckBoxOn".into())));

    // uiScale: off-grid 0.787 snaps to 0.79, percent readout, two-decimal clean string.
    s.run("OptionsFrameContainerBodyGraphicsRowUiScaleControlSlider:SetValue(0.787)")
        .unwrap();
    assert_eq!(
        s.eval::<String>(
            "return OptionsFrameContainerBodyGraphicsRowUiScaleControlValue:GetText()"
        )
        .unwrap(),
        "79%"
    );
    assert_eq!(
        s.take_cvar_changes(),
        vec![("uiScale".to_string(), "0.79".to_string())]
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Defaults on the Graphics page: both CVars back to their registered defaults (uiScale 0.9,
/// farclip 777), rows following, and ONLY the moved values queue.
#[test]
fn defaults_resets_the_graphics_page_to_registered_defaults() {
    let mut s = audio_harness();
    s.set_cvar_host("uiScale", "0.8");
    s.set_cvar_host("farclip", "297");
    let mut s = harness_on(s);
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.run("OptionsFrameCategoryListRowGraphics:Click()")
        .unwrap();
    let _ = s.take_cvar_changes();

    s.run("OptionsFrameContainerDefaults:Click()").unwrap();
    let changes = s.take_cvar_changes();
    assert!(
        changes.contains(&("uiScale".to_string(), "0.9".to_string())),
        "{changes:?}"
    );
    assert!(
        changes.contains(&("farclip".to_string(), "777".to_string())),
        "{changes:?}"
    );
    assert_eq!(changes.len(), 2, "only the moved values queue: {changes:?}");
    assert_eq!(
        s.eval::<String>(
            "return OptionsFrameContainerBodyGraphicsRowUiScaleControlValue:GetText()"
        )
        .unwrap(),
        "90%"
    );
    assert_eq!(
        s.eval::<String>(
            "return OptionsFrameContainerBodyGraphicsRowFarclipControlValue:GetText()"
        )
        .unwrap(),
        "777"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
