//! The shipped `assets/ui/MacroFrame.xml` — the macro editor + its name/icon popup (decision
//! 0983), driven against the engine's own macro table.
//!
//! What these guard, end to end through the real file: the window loads clean in its real
//! neighbourhood; NEW → pick an icon → OKAY actually creates a macro on the right tab; typing in
//! the body and closing the window commits it (the `MacroFrame_SaveMacro` line that makes the
//! whole editor work); the two tabs address the two index ranges; DELETE removes and re-selects;
//! and a macro button is a drag source that loads the cursor with the macro payload.
//!
//! The **saving** half is deliberately exercised through the window rather than the bindings: the
//! bindings' own round trip is `benilla_ui::script::macros`' unit tests, and what can only break
//! here is the wiring — a tab that forgets to save, an OKAY that creates on the wrong tab.

use benilla_ui::script::{CursorPayload, UiScript};

/// The window's real neighbourhood, in the manifest's own order.
fn harness() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    // The global strings the file reads by name (the app runs the real `GlobalStrings.lua`; here
    // only the handful this window formats need to exist).
    s.run(
        r#"
        CREATE_MACROS = "Create Macros"
        GENERAL_MACROS = "General Macros"
        CHARACTER_SPECIFIC_MACROS = "%s Specific Macros"
        ENTER_MACRO_LABEL = "Enter Macro Commands:"
        MACROFRAME_CHAR_LIMIT = "%d/255 Characters Used"
        MACRO_POPUP_TEXT = "Enter Macro Name (Max 16 Characters):"
        MACRO_POPUP_CHOOSE_ICON = "Choose an Icon:"
        CHANGE_MACRO_NAME_ICON = "Change Name/Icon"
        DELETE = "Delete"
        NEW = "New"
        EXIT = "Exit"
        CANCEL = "Cancel"
        OKAY = "Okay"
        MACROS = "Macros"
        -- The tooltip plate colours the body's backdrop reads (the app gets these from
        -- UIParent.lua's own globals; the window only needs them to exist).
        TOOLTIP_DEFAULT_COLOR = { r = 1.0, g = 1.0, b = 1.0 }
        TOOLTIP_DEFAULT_BACKGROUND_COLOR = { r = 0.09, g = 0.09, b = 0.19 }
        "#,
    )
    .unwrap();
    for file in [
        "Fonts.xml",
        "UiPanels.xml",
        "ScrollTemplates.xml",
        "MicroMenu.xml",
        "MacroFrame.xml",
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
    }
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    // The icon chooser's list is the app's push; three entries is enough to index into.
    s.set_macro_icons(vec![
        "Interface\\Icons\\Ability_Ambush".into(),
        "Interface\\Icons\\Ability_BackStab".into(),
        "Interface\\Icons\\Spell_Fire_FlameBolt".into(),
    ]);
    s
}

/// Assert no script error has been collected, naming the step that produced it.
fn no_errors(s: &UiScript, step: &str) {
    assert!(s.errors().is_empty(), "{step}: {:?}", s.errors());
}

/// The whole creation flow through the window's own buttons: NEW opens the popup, an icon click
/// selects, OKAY enables and creates. This is the path a player takes, and every step of it is
/// wiring this file owns.
#[test]
fn new_then_pick_an_icon_then_okay_creates_the_macro() {
    let s = harness();
    s.run("ShowMacroFrame()").unwrap();
    no_errors(&s, "show");

    // OKAY starts disabled: no name, no icon.
    s.run("BenillaMacroNewButton_OnClick()").unwrap();
    assert!(s
        .eval::<bool>("return BenillaMacroPopupFrame:IsVisible()")
        .unwrap());
    assert!(
        !s.eval::<bool>("return BenillaMacroPopupOkayButton:IsEnabled()")
            .unwrap(),
        "a nameless, iconless macro cannot be created"
    );

    s.run(r#"BenillaMacroPopupEditBox:SetText("Ambush")"#)
        .unwrap();
    s.run("BenillaMacroPopupButton_OnClick(BenillaMacroPopupButton1)")
        .unwrap();
    assert!(
        s.eval::<bool>("return BenillaMacroPopupOkayButton:IsEnabled()")
            .unwrap(),
        "a name and an icon enable OKAY"
    );

    s.run("BenillaMacroPopupOkayButton_OnClick()").unwrap();
    no_errors(&s, "okay");
    assert_eq!(
        s.eval::<(i64, i64)>("local a, c = GetNumMacros() return a, c")
            .unwrap(),
        (1, 0),
        "created on the ACCOUNT tab (macroBase 0)"
    );
    let (name, tex) = s
        .eval::<(String, String)>("local n, t = GetMacroInfo(1) return n, t")
        .unwrap();
    assert_eq!(name, "Ambush");
    assert_eq!(tex, "Interface\\Icons\\Ability_Ambush");
    assert!(
        !s.eval::<bool>("return BenillaMacroPopupFrame:IsVisible()")
            .unwrap(),
        "OKAY closes the popup"
    );
    // …and the new macro is selected and shown in the detail pane.
    assert_eq!(
        s.eval::<String>("return BenillaMacroFrameSelectedMacroName:GetText()")
            .unwrap(),
        "Ambush"
    );
}

/// The body editor's commit path — the single line that makes the window an editor at all
/// (`MacroFrame_SaveMacro`, called from the tab switch, the list click, and the window's OnHide).
/// A regression here silently discards everything the player typed.
#[test]
fn typing_a_body_and_closing_the_window_commits_it() {
    let s = harness();
    s.run(r#"CreateMacro("Ambush", 1, "")"#).unwrap();
    s.run("ShowMacroFrame()").unwrap();
    s.run(r#"BenillaMacroFrameText:SetText("/cast Ambush\n/say pew")"#)
        .unwrap();
    no_errors(&s, "type");
    assert!(
        s.eval::<bool>("return BenillaMacroFrame.textChanged == 1")
            .unwrap(),
        "OnTextChanged marks the window dirty"
    );
    // The character counter is the ref's own MACROFRAME_CHAR_LIMIT fill.
    assert_eq!(
        s.eval::<String>("return BenillaMacroFrameCharLimitText:GetText()")
            .unwrap(),
        "21/255 Characters Used"
    );

    s.run(r#"HideUIPanel(BenillaMacroFrame)"#).unwrap();
    no_errors(&s, "hide");
    assert_eq!(
        s.eval::<String>("local _, _, b = GetMacroInfo(1) return b")
            .unwrap(),
        "/cast Ambush\n/say pew",
        "closing the window commits the body"
    );
}

/// The two tabs address the two index ranges, and switching tabs saves first. The 19 is the whole
/// point: `MacroFrame.macroBase` is 0 or MAX_MACROS, and every binding takes `macroBase + i`.
#[test]
fn the_character_tab_creates_in_the_second_index_range_and_switching_saves() {
    let s = harness();
    s.run(r#"CreateMacro("Acct", 1, "")"#).unwrap();
    s.run("ShowMacroFrame()").unwrap();

    // Type into the account macro, then switch tabs WITHOUT any explicit save.
    s.run(r#"BenillaMacroFrameText:SetText("/say account")"#)
        .unwrap();
    s.run("BenillaMacroFrameTab2:Click()").unwrap();
    no_errors(&s, "tab 2");
    assert_eq!(
        s.eval::<String>("local _, _, b = GetMacroInfo(1) return b")
            .unwrap(),
        "/say account",
        "the tab switch saved the body first"
    );
    assert_eq!(
        s.eval::<i64>("return BenillaMacroFrame.macroBase").unwrap(),
        18
    );

    // Creating on this tab lands at 19 (the character range's base + 1).
    s.run("BenillaMacroNewButton_OnClick()").unwrap();
    s.run(r#"BenillaMacroPopupEditBox:SetText("Char")"#)
        .unwrap();
    s.run("BenillaMacroPopupButton_OnClick(BenillaMacroPopupButton2)")
        .unwrap();
    s.run("BenillaMacroPopupOkayButton_OnClick()").unwrap();
    no_errors(&s, "create on tab 2");
    assert_eq!(
        s.eval::<(i64, i64)>("local a, c = GetNumMacros() return a, c")
            .unwrap(),
        (1, 1)
    );
    assert_eq!(
        s.eval::<String>("return GetMacroInfo(19)").unwrap(),
        "Char",
        "the character tab's first slot is index 19"
    );

    // …and back to tab 1 shows the account macro again.
    s.run("BenillaMacroFrameTab1:Click()").unwrap();
    assert_eq!(
        s.eval::<i64>("return BenillaMacroFrame.macroBase").unwrap(),
        0
    );
    assert_eq!(
        s.eval::<String>("return BenillaMacroFrameSelectedMacroName:GetText()")
            .unwrap(),
        "Acct"
    );
}

/// DELETE removes the macro and leaves the window on a sane selection — the ref re-runs its own
/// OnLoad, which re-selects the first macro (or clears the detail pane when none is left).
#[test]
fn delete_removes_the_macro_and_re_selects() {
    let s = harness();
    s.run(r#"CreateMacro("One", 1, "/say one")"#).unwrap();
    s.run(r#"CreateMacro("Two", 2, "/say two")"#).unwrap();
    s.run("ShowMacroFrame()").unwrap();

    s.run("BenillaMacroDeleteButton:Click()").unwrap();
    no_errors(&s, "delete");
    assert_eq!(
        s.eval::<(i64, i64)>("local a, c = GetNumMacros() return a, c")
            .unwrap(),
        (1, 0)
    );
    // The list closed its gap, so slot 1 is now the survivor and it is what's selected.
    assert_eq!(s.eval::<String>("return GetMacroInfo(1)").unwrap(), "Two");
    assert_eq!(
        s.eval::<String>("return BenillaMacroFrameSelectedMacroName:GetText()")
            .unwrap(),
        "Two"
    );

    // Deleting the last one clears the detail pane rather than leaving a stale selection.
    s.run("BenillaMacroDeleteButton:Click()").unwrap();
    no_errors(&s, "delete last");
    assert_eq!(
        s.eval::<(i64, i64)>("local a, c = GetNumMacros() return a, c")
            .unwrap(),
        (0, 0)
    );
    assert!(
        !s.eval::<bool>("return BenillaMacroFrameSelectedMacroButton:IsVisible()")
            .unwrap(),
        "no selection, no detail pane"
    );
}

/// A macro button is a DRAG SOURCE: `OnDragStart` loads the cursor with the macro payload, which
/// `PlaceAction` then packs onto a bar slot under the MACRO tag. This is the only route a macro
/// reaches the action bar.
#[test]
fn dragging_a_macro_button_loads_the_cursor_with_the_macro_payload() {
    let s = harness();
    s.run(r#"CreateMacro("Ambush", 1, "/cast Ambush")"#)
        .unwrap();
    s.run("ShowMacroFrame()").unwrap();

    s.run("BenillaMacroButton1:GetScript(\"OnDragStart\")(BenillaMacroButton1)")
        .unwrap();
    no_errors(&s, "drag");
    let payload = s.cursor_payload();
    assert!(
        matches!(&payload, Some(CursorPayload::Macro(m)) if m.index == 1),
        "the macro payload, carrying its index: {payload:?}"
    );
    assert_eq!(
        s.eval::<(String, i64)>("local k, i = GetCursorInfo() return k, i")
            .unwrap(),
        ("macro".to_string(), 1)
    );

    // Placing it on a bar slot packs the MACRO tag (0x40 << 24) with the macro index.
    let mut s = s;
    s.run("PlaceAction(1)").unwrap();
    assert_eq!(s.take_action_sets(), vec![(1, 0x4000_0000 | 1)]);
}

/// The icon chooser's grid: 20 buttons over the app-pushed list, the tail hidden rather than
/// blank, and a click marking the selection.
#[test]
fn the_icon_chooser_shows_the_pushed_list_and_hides_its_tail() {
    let s = harness();
    s.run("ShowMacroFrame()").unwrap();
    s.run("BenillaMacroNewButton_OnClick()").unwrap();
    no_errors(&s, "popup");

    assert_eq!(s.eval::<i64>("return GetNumMacroIcons()").unwrap(), 3);
    for i in 1..=3 {
        assert!(
            s.eval::<bool>(&format!("return BenillaMacroPopupButton{i}:IsVisible()"))
                .unwrap(),
            "button {i} shows an icon"
        );
    }
    assert!(
        !s.eval::<bool>("return BenillaMacroPopupButton4:IsVisible()")
            .unwrap(),
        "past the end of the list the button hides — not a blank square"
    );

    s.run("BenillaMacroPopupButton_OnClick(BenillaMacroPopupButton3)")
        .unwrap();
    assert_eq!(
        s.eval::<i64>("return BenillaMacroPopupFrame.selectedIcon")
            .unwrap(),
        3
    );
    assert!(
        s.eval::<bool>("return BenillaMacroPopupButton3:GetChecked()")
            .unwrap(),
        "the picked icon is checked"
    );
}
