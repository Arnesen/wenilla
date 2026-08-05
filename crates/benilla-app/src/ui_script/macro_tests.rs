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

/// **The two tabs fit inside the 384-wide window** — the director's own report (2026-08-05: "tab is
/// overflowing"), and the check 0983 never had.
///
/// 0983 dropped the reference's `PanelTemplates_TabResize(-15, …)` tuning as redundant with the
/// template's own +0 fit. It isn't: at +0 a tab is `text + 40`, and two of those plus the 65-unit
/// left inset run off a 384-wide plate — the more so on tab 2, whose label carries a player name and
/// so has no width anyone can know when the window is written. The reference's answer is the −15
/// padding on both and a 150 cap on tab 2; this pins both arms, including the cap actually engaging.
///
/// Widths are fed the way the app's font atlas feeds them ([`UiScript::set_measured_text`]) — the
/// template's fit runs from OnUpdate once the measure lands, so the `tick` is load-bearing.
#[test]
fn the_two_tabs_fit_inside_the_window() {
    /// `sideWidths` — 2 × the tab's 16-unit end slice (TabButtonTemplate, UiPanels.xml).
    const SIDES: f64 = 32.0;
    /// The reference's own padding for this window's tabs (Blizzard_MacroUI.xml l.488/511).
    const PAD: f64 = -15.0;
    /// …and its cap on tab 2 (l.511).
    const CAP: f64 = 150.0;

    for (label_width, expect_tab2) in [
        // A short name: under the cap, so the tab is text + PAD + SIDES.
        (121.0, 121.0 + PAD + SIDES),
        // A long one: the reference's cap would give CAP + PAD + SIDES = 167, but the structural
        // clamp is tighter here and wins at 164 — the room left between tab 1's right edge and the
        // window's DRAWN edge (decision 1002). Pinned as the smaller of the two on purpose: the
        // whole point of the clamp is that it, not the hand-tuned cap, is what holds the line.
        (240.0, 164.0),
    ] {
        assert!(
            expect_tab2 <= CAP + PAD + SIDES,
            "the clamp may tighten the reference's cap, never loosen it"
        );
        let mut s = harness();
        s.run("ShowMacroFrame()").unwrap();
        s.resolve();
        // Answer the measure round-trip for the two tab labels (the app's font-atlas job in-game).
        let measures: Vec<_> = s
            .fontstrings_needing_measure()
            .into_iter()
            .filter(|r| r.text.contains("Macros"))
            .map(|r| {
                let w = if r.text == "General Macros" {
                    78.0
                } else {
                    label_width
                };
                (r.id, w as f32, 10.0, r.key)
            })
            .collect();
        assert!(measures.len() >= 2, "both tab labels request a measure");
        s.set_measured_text_unwrapped(&measures);
        s.tick(0.016);
        s.resolve();

        let (w1, w2, left1): (f64, f64, f64) = s
            .eval(
                "return BenillaMacroFrameTab1:GetWidth(), BenillaMacroFrameTab2:GetWidth(), \
                 BenillaMacroFrameTab1:GetLeft() - BenillaMacroFrame:GetLeft()",
            )
            .unwrap();
        assert_eq!(
            w1,
            78.0 + PAD + SIDES,
            "tab 1 is text − 15 + the end slices"
        );
        assert_eq!(w2, expect_tab2, "tab 2, label width {label_width}");
        assert!(
            left1 + w1 + w2 <= 384.0,
            "the tab row ({left1} + {w1} + {w2}) runs off the 384-wide window"
        );
        no_errors(&s, "tab fit");
    }
}

/// **The icon chooser's scroll bar sits ON the popup's plate**, not off its right edge over the
/// world. 0983 sized the faux scroll frame to the icon grid (252×152) instead of transcribing the
/// reference's rect, which pushed the bar — anchored 6 units right of the frame — to popup x
/// 282..298, i.e. straddling the 297-wide plate's edge.
///
/// The frame is invisible, so its rect has exactly one observable consequence and this is it.
#[test]
fn the_icon_choosers_scroll_bar_sits_on_the_popup_plate() {
    let mut s = harness();
    s.run("ShowMacroFrame() BenillaMacroNewButton_OnClick()")
        .unwrap();
    s.resolve();
    let (bar_left, bar_right, plate_left, plate_right): (f64, f64, f64, f64) = s
        .eval(
            "local b = BenillaMacroPopupScrollFrameScrollBar local p = BenillaMacroPopupFrame \
             return b:GetLeft(), b:GetRight(), p:GetLeft(), p:GetRight()",
        )
        .unwrap();
    assert!(
        bar_left >= plate_left && bar_right <= plate_right,
        "scroll bar {bar_left}..{bar_right} is outside the popup plate {plate_left}..{plate_right}"
    );
    // The reference's own seat: 6 units right of a scroll frame whose right edge is 39 in from the
    // popup's — so 264..280 of 297. Pinned as a number so a future re-anchor has to mean it.
    assert_eq!(
        (bar_left - plate_left, bar_right - plate_left),
        (264.0, 280.0)
    );
    no_errors(&s, "popup scroll bar");
}

/// Answer the measure round-trip the way the APP does — every pending request, every frame, with a
/// stand-in font metric (6 units/char, wrapping greedily at the requested width). The point is the
/// LOOP, not the metric: a hand-fed one-shot `set_measured_text` cannot show a fit that oscillates,
/// because oscillation needs the engine to re-request after the fit changes the label's box.
fn pump_measures(s: &mut UiScript) {
    const PER_CHAR: f32 = 8.0;
    let measures: Vec<_> = s
        .fontstrings_needing_measure()
        .into_iter()
        .map(|r| {
            let natural = r.text.chars().count() as f32 * PER_CHAR;
            let w = r.wrap_width.map_or(natural, |cap| natural.min(cap));
            let lines = if w > 0.0 { (natural / w).ceil() } else { 1.0 };
            // The laid-out extent AND the natural one — the distinction this whole test is about.
            (r.id, w, 13.0 * lines, natural, r.key)
        })
        .collect();
    s.set_measured_text(&measures);
}

/// **The tab row is stable and inside the window, frame after frame** — the director's repro
/// (2026-08-05: a 9-character character name, "tab is still slightly overlapping"), and the check
/// the first fix did not have.
///
/// `the_two_tabs_fit_inside_the_window` feeds one measure and ticks once, which cannot catch this:
/// the defect is a FEEDBACK LOOP. The fit sets a width on the label; that changed the label's box;
/// and `GetStringWidth` used to report the box rather than the string, so the next frame refit off
/// a smaller number, uncapped, un-set the box, re-measured wider, capped again — a tab that changed
/// width every single frame and could be photographed at any point in the cycle.
#[test]
fn the_tab_row_settles_and_stays_inside_the_window() {
    let mut s = harness();
    s.run(r#"UnitName = function() return "Onehunter" end"#)
        .unwrap();
    s.run("ShowMacroFrame()").unwrap();
    s.run(r#"BenillaMacroFrameTab2:SetText(BenillaMacroFrame_TabTwoLabel())"#)
        .unwrap();

    let mut widths = Vec::new();
    for _ in 0..12 {
        s.resolve();
        pump_measures(&mut s);
        s.tick(0.016);
        s.resolve();
        widths.push(
            s.eval::<(f64, f64, f64)>(
                "return BenillaMacroFrameTab1:GetWidth(), BenillaMacroFrameTab2:GetWidth(), \
                 BenillaMacroFrameTab2:GetRight() - BenillaMacroFrame:GetLeft()",
            )
            .unwrap(),
        );
    }

    let last = *widths.last().unwrap();
    let settled = &widths[widths.len() - 4..];
    assert!(
        settled.iter().all(|w| *w == last),
        "the tab fit never settles — it changes every frame: {widths:?}"
    );
    // 344, not 384: this window's art stops 40 units short of its frame rect (MacroFrame.xml's
    // `benillaTabRightInset`), and the frame rect is not what the player sees.
    assert!(
        last.2 <= 344.0,
        "tab 2 ends at {} of a window whose plate stops at 344: {widths:?}",
        last.2
    );
    no_errors(&s, "tab settle");
}

/// **The structural cap: no character name can push the tab row off the window** — the guarantee
/// the reference does not have (its 150 is a number hand-tuned to its own font) and the one the
/// director asked for by name. A tab clamps at its parent's right edge whatever it was asked for,
/// so this holds for a name no window author could have anticipated.
#[test]
fn no_character_name_can_push_the_tab_row_off_the_window() {
    // `capped = false` strips the window's own `benillaTabMaxWidth`, leaving ONLY the structural
    // clamp — which is the state every other tab in the client is in (no window but this one asks
    // for a cap), and the state this guarantee exists for.
    for (name, capped) in [
        ("Ai", true),
        ("Onehunter", true),
        ("Bartholomewthethird", false),
        (&"W".repeat(64), false),
    ] {
        let mut s = harness();
        s.run(&format!("UnitName = function() return \"{name}\" end"))
            .unwrap();
        s.run("ShowMacroFrame()").unwrap();
        if !capped {
            s.run("BenillaMacroFrameTab2.benillaTabMaxWidth = nil")
                .unwrap();
        }
        s.run("BenillaMacroFrameTab2:SetText(BenillaMacroFrame_TabTwoLabel())")
            .unwrap();
        for _ in 0..12 {
            s.resolve();
            pump_measures(&mut s);
            s.tick(0.016);
            s.resolve();
        }
        let (right, w2): (f64, f64) = s
            .eval(
                "return BenillaMacroFrameTab2:GetRight() - BenillaMacroFrame:GetLeft(), \
                 BenillaMacroFrameTab2:GetWidth()",
            )
            .unwrap();
        assert!(
            right <= 344.0,
            "{name:?}: the tab row ends at {right}, past the drawn plate's edge at 344 \
             (tab 2 is {w2} wide)"
        );
        no_errors(&s, name);
    }
}

/// **The hover highlight IS the tab, at every width and from the first frame** — the director's
/// report on the landed 1002 build: in one screenshot the glow stood proud of tab 1 and fell well
/// short of tab 2.
///
/// Both halves were the same defect, and it is 1002's own read-back trap in the one line 1002 did
/// not fix: the old kit sized the highlight from the label and then capped it at `tab:GetWidth()`,
/// which serves the last RESOLVED rect — inside a settle, the template's pre-fit 115. So every tab
/// in the client wore a 129-unit highlight regardless of its own width, and the settle's change-gate
/// latched it there. The fix is structural (the highlight anchors to the tab's two side edges), so
/// this test asserts the property that makes the whole class of bug impossible: **no frame, no
/// label, no clamp can put the glow at a width other than its tab's.**
#[test]
fn the_tab_highlight_is_exactly_its_tab() {
    // A name under the reference's cap, one over it, and one long enough that the structural
    // drawn-edge clamp (1002) is what sets the width — all three must hold the same property.
    for name in ["Ai", "Onehunter", &"W".repeat(40)] {
        let mut s = harness();
        s.run(&format!("UnitName = function() return \"{name}\" end"))
            .unwrap();
        s.run("ShowMacroFrame()").unwrap();
        s.run("BenillaMacroFrameTab2:SetText(BenillaMacroFrame_TabTwoLabel())")
            .unwrap();

        for frame in 0..12 {
            s.resolve();
            pump_measures(&mut s);
            s.tick(0.016);
            s.resolve();
            for tab in ["BenillaMacroFrameTab1", "BenillaMacroFrameTab2"] {
                let (tl, tr, hl, hr): (f64, f64, f64, f64) = s
                    .eval(&format!(
                        "return {tab}:GetLeft(), {tab}:GetRight(), \
                         {tab}HighlightTexture:GetLeft(), {tab}HighlightTexture:GetRight()"
                    ))
                    .unwrap();
                assert_eq!(
                    hr - hl,
                    tr - tl,
                    "{tab} f{frame} ({name}): highlight {} wide on a {} tab",
                    hr - hl,
                    tr - tl
                );
                // …and seated where the reference seats it: its own +2 nudge off the tab's edges
                // (TabButtonTemplate's highlight anchor offset), not centred on some other rect.
                assert_eq!(
                    hl,
                    tl + 2.0,
                    "{tab} f{frame} ({name}): highlight not on the tab"
                );
            }
        }
        no_errors(&s, "tab highlight");
    }
}
