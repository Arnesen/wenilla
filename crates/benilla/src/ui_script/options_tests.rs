//! The shipped `assets/ui/OptionsFrame.xml` — the era-skinned options window (decision 0950),
//! slice A: the shell.
//!
//! What these guard: the file loads clean inside the real neighbourhood (Fonts + UiPanels +
//! GameMenuFrame); the menu's Options button is the door in (menu down, options up, on the ref's
//! own kit); Controls is the default category and the page title follows the selection; both
//! close spellings put the window away; and — against a hand-pushed atlas table, the
//! era_atlas_tests idiom — the selected row actually wears `options_list_active` and drops it
//! when the selection moves. The harness pushes NO atlas table by default: `SetAtlas` is a
//! warn-once no-op there by design (a missing extraction must not kill the UI), which is exactly
//! why the one atlas-backed test pushes its own two members.

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
