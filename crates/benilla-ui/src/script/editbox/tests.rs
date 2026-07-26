//! Rust-driven tests of the EditBox runtime (RF-0082): focus acquisition + routing, text/cursor/
//! selection editing, the specialized script fires, caps, numeric/password, and text-region sync.
//! Frames are built programmatically via `CreateFrame("EditBox", …)` and driven through the public
//! keyboard API (`char_input`/`key_input`/`editbox_action`/`has_keyboard_focus`) and the Lua
//! method surface.

use crate::script::{EditAction, EditUnit, QuadContent, UiScript};

fn script() -> UiScript {
    UiScript::new().expect("construct UiScript")
}

/// The display text of the (single) text quad an extract produces, if any.
fn text_quad(s: &UiScript) -> Option<String> {
    s.extract().into_iter().find_map(|q| match q.content {
        QuadContent::Text { text, .. } => text,
        _ => None,
    })
}

// ── §1/§2 focus acquisition + routing ───────────────────────────────────────────────────────

#[test]
fn autofocus_does_not_focus_on_show_but_self_acquires_first_event() {
    let mut s = script();
    s.run(r#"E = CreateFrame("EditBox", "E"); E:SetAutoFocus(true)"#)
        .unwrap();
    // autoFocus does NOT focus on show — nothing owns the keyboard yet.
    assert!(!s.has_keyboard_focus());
    assert!(!s.eval::<bool>("return E:HasFocus()").unwrap());

    // The first char self-acquires focus AND processes that same event.
    assert!(
        s.char_input("a"),
        "an autoFocus box consumes the acquiring event"
    );
    assert!(s.has_keyboard_focus());
    assert!(s.eval::<bool>("return E:HasFocus()").unwrap());
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "a");
}

#[test]
fn non_autofocus_unfocused_box_ignores_input() {
    let mut s = script();
    s.run(r#"E = CreateFrame("EditBox", "E")"#).unwrap();
    assert!(!s.char_input("a"), "no focus, no autoFocus → not consumed");
    assert!(!s.editbox_action(EditAction::Move {
        unit: EditUnit::Char,
        back: true,
        extend: false
    }));
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "");
    assert!(!s.has_keyboard_focus());
}

#[test]
fn focused_box_consumes_everything() {
    let mut s = script();
    s.run(r#"E = CreateFrame("EditBox", "E"); E:SetFocus()"#)
        .unwrap();
    // Empty text: these do nothing, but a focused box still consumes them.
    assert!(s.editbox_action(EditAction::Move {
        unit: EditUnit::Char,
        back: true,
        extend: false
    }));
    assert!(s.editbox_action(EditAction::Move {
        unit: EditUnit::Edge,
        back: true,
        extend: false
    }));
    assert!(s.editbox_action(EditAction::Delete {
        unit: EditUnit::Char,
        back: false
    }));
    assert!(s.char_input("x"));
}

#[test]
fn set_focus_on_hidden_box_is_a_noop() {
    let s = script();
    s.run(
        r#"
        gained = 0
        E = CreateFrame("EditBox", "E")
        E:SetScript("OnEditFocusGained", function() gained = gained + 1 end)
        E:Hide()
        E:SetFocus()
    "#,
    )
    .unwrap();
    assert!(!s.eval::<bool>("return E:HasFocus()").unwrap());
    assert!(!s.has_keyboard_focus());
    assert_eq!(
        s.eval::<i64>("return gained").unwrap(),
        0,
        "no focus gained"
    );
}

#[test]
fn click_focuses_regardless_of_autofocus_and_transition_order_is_lost_then_gained() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        log = {}
        local function wire(name, y)
            local f = CreateFrame("EditBox", name)
            f:SetPoint("BOTTOMLEFT", nil, "BOTTOMLEFT", 0, y)
            f:SetSize(100, 20)
            f:SetScript("OnEditFocusGained", function() table.insert(log, "gained"..name) end)
            f:SetScript("OnEditFocusLost", function() table.insert(log, "lost"..name) end)
        end
        wire("A", 0)      -- rect bottom 0..20
        wire("B", 100)    -- rect bottom 100..120
    "#,
    )
    .unwrap();
    s.resolve();

    // Neither box has autoFocus, yet a click focuses each.
    s.mouse_button(50.0, 10.0, "LeftButton", true);
    s.mouse_button(50.0, 10.0, "LeftButton", false);
    assert!(s.eval::<bool>("return A:HasFocus()").unwrap());

    s.mouse_button(50.0, 110.0, "LeftButton", true);
    s.mouse_button(50.0, 110.0, "LeftButton", false);
    assert!(s.eval::<bool>("return B:HasFocus()").unwrap());

    let log: Vec<String> = s.eval("return log").unwrap();
    assert_eq!(log, vec!["gainedA", "lostA", "gainedB"]);
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

// ── §3 text buffer + editing + OnTextChanged/OnTextSet ───────────────────────────────────────

#[test]
fn typing_builds_text_and_fires_ontextchanged_per_mutation() {
    let mut s = script();
    s.run(
        r#"
        changed = 0
        E = CreateFrame("EditBox", "E")
        E:SetScript("OnTextChanged", function() changed = changed + 1 end)
        E:SetFocus()
    "#,
    )
    .unwrap();
    assert!(s.char_input("a"));
    assert!(s.char_input("b"));
    assert!(s.char_input("c"));
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "abc");
    assert_eq!(s.eval::<i64>("return changed").unwrap(), 3);
}

#[test]
fn set_text_short_circuits_on_same_value_else_fires_textset_then_textchanged() {
    let s = script();
    s.run(
        r#"
        log = {}
        E = CreateFrame("EditBox", "E")
        E:SetScript("OnTextSet", function() table.insert(log, "set") end)
        E:SetScript("OnTextChanged", function() table.insert(log, "changed") end)
        E:SetText("hi")     -- fires set then changed
        E:SetText("hi")     -- unchanged → short-circuit, no events
        E:SetText("bye")    -- fires set then changed
    "#,
    )
    .unwrap();
    let log: Vec<String> = s.eval("return log").unwrap();
    assert_eq!(log, vec!["set", "changed", "set", "changed"]);
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "bye");
}

#[test]
fn numeric_aborts_a_mixed_insert_wholesale() {
    let s = script();
    s.run(
        r#"
        E = CreateFrame("EditBox", "E")
        E:SetNumeric(true)
        E:Insert("12")     -- all digits: accepted
        E:Insert("3a")     -- one non-digit: the WHOLE insert aborts
    "#,
    )
    .unwrap();
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "12");
}

#[test]
fn max_letters_trims_from_the_end() {
    let s = script();
    s.run(
        r#"
        E = CreateFrame("EditBox", "E")
        E:SetMaxLetters(3)
        E:Insert("abcde")
    "#,
    )
    .unwrap();
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "abc");
    assert_eq!(s.eval::<i64>("return E:GetNumLetters()").unwrap(), 3);
}

// ── §4 selection / highlight / keys ──────────────────────────────────────────────────────────

#[test]
fn highlight_all_then_typing_replaces_the_selection() {
    let mut s = script();
    s.run(
        r#"
        E = CreateFrame("EditBox", "E")
        E:SetText("hello")
        E:HighlightText(0, -1)   -- select all
        E:SetFocus()
    "#,
    )
    .unwrap();
    assert!(s.char_input("X"), "focused: consumed");
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "X");
}

#[test]
fn paste_inserts_at_the_cursor_and_replaces_the_selection() {
    let mut s = script();
    s.run(
        r#"
        changed = 0
        E = CreateFrame("EditBox", "E")
        E:SetScript("OnTextChanged", function() changed = changed + 1 end)
        E:SetText("ab")
        E:SetFocus()
        changed = 0   -- ignore the setup SetText's fire; count only the pastes below
    "#,
    )
    .unwrap();
    // Cursor sits at end after SetText → paste appends.
    assert!(s.paste("CD"), "focused box consumes the paste");
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "abCD");
    // Select-all then paste replaces the whole selection.
    s.run("E:HighlightText(0, -1)").unwrap();
    assert!(s.paste("xyz"));
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "xyz");
    // Each paste that changed the text fired OnTextChanged once.
    assert_eq!(s.eval::<i64>("return changed").unwrap(), 2);
}

#[test]
fn paste_into_an_unfocused_box_is_not_consumed() {
    let mut s = script();
    s.run(r#"E = CreateFrame("EditBox", "E")"#).unwrap();
    assert!(!s.paste("hi"), "no focus, no autoFocus → not consumed");
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "");
}

#[test]
fn paste_strips_newlines_in_a_single_line_box_but_keeps_them_when_multiline() {
    let mut s = script();
    s.run(r#"E = CreateFrame("EditBox", "E"); E:SetFocus()"#)
        .unwrap();
    // Single-line: newlines/tabs and other control chars are dropped; spaces survive.
    assert!(s.paste("one\ntwo\tthree\r"));
    assert_eq!(
        s.eval::<String>("return E:GetText()").unwrap(),
        "onetwothree"
    );

    // Multi-line box: the newline survives (a tab is still a dropped control char).
    let mut s = script();
    s.run(r#"E = CreateFrame("EditBox", "E"); E:SetMultiLine(true); E:SetFocus()"#)
        .unwrap();
    assert!(s.paste("one\ntwo"));
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "one\ntwo");
}

#[test]
fn paste_honors_max_letters_and_numeric() {
    // maxLetters trims the paste from the end.
    let mut s = script();
    s.run(r#"E = CreateFrame("EditBox", "E"); E:SetMaxLetters(3); E:SetFocus()"#)
        .unwrap();
    assert!(s.paste("abcdef"));
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "abc");

    // numeric aborts a paste that carries any non-digit (matching the typed-insert rule).
    let mut s = script();
    s.run(r#"E = CreateFrame("EditBox", "E"); E:SetNumeric(true); E:SetFocus()"#)
        .unwrap();
    assert!(s.paste("12a3"), "consumed even though the insert aborts");
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "");
    assert!(s.paste("456"));
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "456");
}

#[test]
fn paste_does_not_fire_onspacepressed() {
    let mut s = script();
    s.run(
        r#"
        spaces = 0
        E = CreateFrame("EditBox", "E")
        E:SetScript("OnSpacePressed", function() spaces = spaces + 1 end)
        E:SetFocus()
    "#,
    )
    .unwrap();
    assert!(s.paste("a b c"));
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "a b c");
    assert_eq!(
        s.eval::<i64>("return spaces").unwrap(),
        0,
        "a paste is not a typed space"
    );
}

#[test]
fn backspace_deletes_the_selection() {
    let mut s = script();
    s.run(
        r#"
        E = CreateFrame("EditBox", "E")
        E:SetText("hello")
        E:HighlightText(0, -1)
        E:SetFocus()
    "#,
    )
    .unwrap();
    assert!(s.editbox_action(EditAction::Delete {
        unit: EditUnit::Char,
        back: true
    }));
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "");
}

#[test]
fn enter_escape_tab_space_fire_their_slots() {
    let mut s = script();
    s.run(
        r#"
        log = {}
        E = CreateFrame("EditBox", "E")
        E:SetScript("OnEnterPressed", function() table.insert(log, "enter") end)
        E:SetScript("OnEscapePressed", function() table.insert(log, "escape") end)
        E:SetScript("OnTabPressed", function() table.insert(log, "tab") end)
        E:SetScript("OnSpacePressed", function() table.insert(log, "space") end)
        E:SetFocus()
    "#,
    )
    .unwrap();
    assert!(s.key_input("ENTER")); // single-line → OnEnterPressed
    assert!(s.key_input("ESCAPE")); // fires, does NOT release focus
    assert!(
        s.eval::<bool>("return E:HasFocus()").unwrap(),
        "ESCAPE keeps focus"
    );
    assert!(s.key_input("TAB"));
    assert!(s.char_input(" ")); // a literal space fires OnSpacePressed

    let log: Vec<String> = s.eval("return log").unwrap();
    assert_eq!(log, vec!["enter", "escape", "tab", "space"]);
    // ENTER on a single-line box must NOT insert; only the space landed.
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), " ");
}

#[test]
fn multiline_enter_inserts_a_newline_without_onspacepressed() {
    let mut s = script();
    s.run(
        r#"
        spaces = 0
        E = CreateFrame("EditBox", "E")
        E:SetMultiLine(true)
        E:SetScript("OnSpacePressed", function() spaces = spaces + 1 end)
        E:SetFocus()
    "#,
    )
    .unwrap();
    assert!(s.key_input("ENTER"));
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "\n");
    assert_eq!(s.eval::<i64>("return spaces").unwrap(), 0);
}

#[test]
fn ignore_arrows_swallows_char_moves_but_not_word_or_edge() {
    let mut s = script();
    s.run(
        r#"
        E = CreateFrame("EditBox", "E")
        E:SetIgnoreArrows(true)
        E:SetText("abc")
        E:SetFocus()
    "#,
    )
    .unwrap();
    // ignoreArrows: a Char move (a plain arrow) is consumed but inert; the cursor stays put — a
    // subsequent BACKSPACE removes the char before end-of-text ('c'), proving it didn't move.
    assert!(s.editbox_action(EditAction::Move {
        unit: EditUnit::Char,
        back: true,
        extend: false
    }));
    assert!(s.editbox_action(EditAction::Delete {
        unit: EditUnit::Char,
        back: true
    }));
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "ab");
    // The ref's Ctrl bypass (guard 0x77b18e) maps to Word — which passes the gate; Edge too.
    s.editbox_action(EditAction::Move {
        unit: EditUnit::Word,
        back: true,
        extend: false,
    });
    assert!(s.editbox_action(EditAction::Delete {
        unit: EditUnit::Char,
        back: false
    }));
    assert_eq!(
        s.eval::<String>("return E:GetText()").unwrap(),
        "b",
        "the word move must have reached text start"
    );
}

// ── password + GetNumber ─────────────────────────────────────────────────────────────────────

#[test]
fn password_masks_the_display_but_gettext_returns_the_real_text() {
    let s = script();
    s.run(
        r#"
        E = CreateFrame("EditBox", "E")
        E:SetPassword(true)
        E:SetText("secret")
    "#,
    )
    .unwrap();
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "secret");
    assert_eq!(
        text_quad(&s).as_deref(),
        Some("******"),
        "the text region shows one '*' per character"
    );
}

#[test]
fn get_number_parses_the_text() {
    let s = script();
    s.run(r#"E = CreateFrame("EditBox", "E"); E:SetText("12.5")"#)
        .unwrap();
    assert_eq!(s.eval::<f64>("return E:GetNumber()").unwrap(), 12.5);
    // Non-numeric text → 0.
    s.run(r#"E:SetText("hello")"#).unwrap();
    assert_eq!(s.eval::<f64>("return E:GetNumber()").unwrap(), 0.0);
}

// ── the EditBox override never fires generic OnChar/OnKeyDown (§2) ────────────────────────────

#[test]
fn key_and_char_paths_never_fire_generic_handlers() {
    // OnKeyDown/OnChar aren't even in SCRIPT_KINDS, so SetScript rejects them — proving the runtime
    // has no generic keyboard slots to fire (the C++ override replaced them).
    let s = script();
    let err = s
        .run(r#"CreateFrame("EditBox", "E"):SetScript("OnChar", function() end)"#)
        .unwrap_err();
    assert!(err.to_string().contains("OnChar"), "{err}");
}

// ── history recall (`historyLines` / AddHistoryLine — decision 0288 P2) ──────────────────────

#[test]
fn history_recall_walks_up_and_down_and_restores_the_draft() {
    let mut s = script();
    s.run(
        r#"
        E = CreateFrame("EditBox", "E")
        E:SetHistoryLines(32)
        E:AddHistoryLine("/say one")
        E:AddHistoryLine("/g two")
        E:SetText("draft")
        E:SetFocus()
    "#,
    )
    .unwrap();
    // UP recalls newest-first; a second UP walks older; at the oldest it holds.
    assert!(s.editbox_action(EditAction::HistoryPrev));
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "/g two");
    s.editbox_action(EditAction::HistoryPrev);
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "/say one");
    s.editbox_action(EditAction::HistoryPrev);
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "/say one");
    // DOWN walks newer; past the newest the stashed live draft comes back.
    s.editbox_action(EditAction::HistoryNext);
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "/g two");
    s.editbox_action(EditAction::HistoryNext);
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "draft");
    // Not browsing: DOWN does nothing.
    s.editbox_action(EditAction::HistoryNext);
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "draft");
}

#[test]
fn typing_ends_the_history_browse() {
    let mut s = script();
    s.run(
        r#"
        E = CreateFrame("EditBox", "E")
        E:SetHistoryLines(32)
        E:AddHistoryLine("older")
        E:AddHistoryLine("newer")
        E:SetFocus()
    "#,
    )
    .unwrap();
    s.editbox_action(EditAction::HistoryPrev);
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "newer");
    // A typed char turns the recalled line into an ordinary draft: the next UP starts a FRESH
    // browse from the newest entry (stashing the edited line as the new draft).
    s.char_input("!");
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "newer!");
    s.editbox_action(EditAction::HistoryPrev);
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "newer");
    s.editbox_action(EditAction::HistoryNext);
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "newer!");
}

#[test]
fn programmatic_set_text_keeps_the_browse_and_focus_gain_resets_it() {
    let mut s = script();
    s.run(
        r#"
        E = CreateFrame("EditBox", "E")
        E:SetHistoryLines(32)
        E:AddHistoryLine("/say one")
        E:AddHistoryLine("/g two")
        E:SetFocus()
    "#,
    )
    .unwrap();
    // Recall the newest, then rewrite the box the way the chat live parse does on a slash
    // recall ("/g two" → Guild + "two"). The browse walk must survive: the next UP lands on
    // the OLDER entry, not back on the newest (the "history only goes back 1" bug).
    s.editbox_action(EditAction::HistoryPrev);
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "/g two");
    s.run(r#"E:SetText("two")"#).unwrap();
    s.editbox_action(EditAction::HistoryPrev);
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "/say one");
    // Refocusing starts a fresh session: the stale walk drops, UP recalls the newest again.
    s.run(r#"E:ClearFocus() E:SetFocus()"#).unwrap();
    s.editbox_action(EditAction::HistoryPrev);
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "/g two");
}

#[test]
fn history_caps_at_history_lines_drop_oldest() {
    let mut s = script();
    s.run(
        r#"
        E = CreateFrame("EditBox", "E")
        E:SetHistoryLines(2)
        E:AddHistoryLine("a")
        E:AddHistoryLine("b")
        E:AddHistoryLine("c")
        E:SetFocus()
    "#,
    )
    .unwrap();
    assert_eq!(s.eval::<i64>("return E:GetHistoryLines()").unwrap(), 2);
    // Only b/c survive: two UPs land on 'b', a third holds there.
    s.editbox_action(EditAction::HistoryPrev);
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "c");
    s.editbox_action(EditAction::HistoryPrev);
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "b");
    s.editbox_action(EditAction::HistoryPrev);
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "b");
}

#[test]
fn history_off_by_default_and_up_is_still_consumed() {
    let mut s = script();
    s.run(r#"E = CreateFrame("EditBox", "E"); E:SetText("t"); E:SetFocus()"#)
        .unwrap();
    // No historyLines: AddHistoryLine is a no-op, UP consumed but inert (a focused box eats every
    // key — RF-0082 §2).
    s.run(r#"E:AddHistoryLine("x")"#).unwrap();
    assert!(s.editbox_action(EditAction::HistoryPrev));
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "t");
}

// ── SetTextInsets (0288 P2 — the header-driven text-rect shrink) ─────────────────────────────

#[test]
fn text_insets_shrink_the_text_region_rect() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        E = CreateFrame("EditBox", "E")
        E:SetPoint("BOTTOMLEFT", 100, 50)
        E:SetWidth(400)
        E:SetHeight(32)
        E:SetTextInsets(15, 13, 2, 3)
        E:SetText("hello")
    "#,
    )
    .unwrap();
    s.resolve();
    let quads = s.extract();
    let rect = quads
        .iter()
        .find_map(|q| match (&q.content, q.rect) {
            (crate::script::QuadContent::Text { text: Some(t), .. }, Some(r)) if t == "hello" => {
                Some(r)
            }
            _ => None,
        })
        .expect("the edit box text renders");
    assert!((rect.left - 115.0).abs() < 0.01, "left inset 15: {rect:?}");
    assert!(
        (rect.right - 487.0).abs() < 0.01,
        "right inset 13: {rect:?}"
    );
    assert!((rect.top - 80.0).abs() < 0.01, "top inset 2: {rect:?}");
    assert!(
        (rect.bottom - 53.0).abs() < 0.01,
        "bottom inset 3: {rect:?}"
    );
    // The getter echoes.
    let (l, r_, t, b) = s
        .eval::<(f32, f32, f32, f32)>("return E:GetTextInsets()")
        .unwrap();
    assert_eq!((l, r_, t, b), (15.0, 13.0, 2.0, 3.0));
}

// The FontInstance half an EditBox inherits in the client: SetTextColor tints the box's text
// region (`ChatEdit_UpdateHeader` colors the typed text this way — its absence killed the chat
// header chunk mid-run and left the /w insets stale, the caret-in-the-header bug).
#[test]
fn set_text_color_tints_the_text_region() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        E = CreateFrame("EditBox", "E")
        E:SetPoint("BOTTOMLEFT", 100, 50)
        E:SetWidth(400)
        E:SetHeight(32)
        E:SetText("hello")
        E:SetTextColor(1.0, 0.5, 0.25)
    "#,
    )
    .unwrap();
    s.resolve();
    let color = s
        .extract()
        .iter()
        .find_map(|q| match &q.content {
            crate::script::QuadContent::Text {
                text: Some(t),
                color,
                ..
            } if t == "hello" => Some(*color),
            _ => None,
        })
        .expect("the edit box text renders");
    assert_eq!(color, Some([1.0, 0.5, 0.25, 1.0]));
}

// ── the host text-UI seam: advances, caret/selection geometry, mouse, clipboard ─────────────

/// Build a focused 200×32 box at BOTTOMLEFT(100,50), resolve it, type `text`, and answer its
/// advance table with a synthetic monospace 7 px/byte — the standard rig for the geometry tests.
fn seam_rig(s: &mut UiScript, text: &str) {
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        E = CreateFrame("EditBox", "E")
        E:SetWidth(200); E:SetHeight(32)
        E:SetPoint("BOTTOMLEFT", 100, 50)
        E:SetFocus()
    "#,
    )
    .unwrap();
    for ch in text.chars() {
        s.char_input(&ch.to_string());
    }
    s.resolve();
    answer_advances(s);
}

/// Answer any pending advance request at 7 px per byte.
fn answer_advances(s: &mut UiScript) {
    if let Some(req) = s.editbox_advances_request() {
        let cum: Vec<f32> = (0..=req.text.len()).map(|i| i as f32 * 7.0).collect();
        s.set_editbox_advances(req.id, req.key, cum, vec![0], 0.0);
    }
}

/// The seam reports caret geometry from the advance table, and vanishes with focus or
/// visibility — the old prefix-string seam's laws, now in pixels.
#[test]
fn text_ui_reports_caret_geometry_and_focus() {
    let mut s = script();
    seam_rig(&mut s, "");
    // Empty box: the advance table settles engine-side; the caret stands at the origin.
    let ui = s
        .focused_editbox_text_ui()
        .expect("focused box has text-UI");
    assert_eq!(ui.caret_x, 0.0);
    assert_eq!(ui.display_from, 0);
    assert!(ui.selection.is_empty());
    // The empty box still emits its Text quad (Some("")) — the caret's ride-along.
    assert!(
        s.extract().iter().any(|q| matches!(
            (&q.target, &q.content),
            (t, QuadContent::Text { text: Some(x), .. }) if *t == ui.target && x.is_empty()
        )),
        "an empty focused box must still emit its text-region quad"
    );

    s.char_input("a");
    s.char_input("b");
    answer_advances(&mut s);
    assert_eq!(s.focused_editbox_text_ui().unwrap().caret_x, 14.0);
    s.editbox_action(EditAction::Move {
        unit: EditUnit::Char,
        back: true,
        extend: false,
    });
    assert_eq!(s.focused_editbox_text_ui().unwrap().caret_x, 7.0);

    s.run("E:ClearFocus()").unwrap();
    assert!(s.focused_editbox_text_ui().is_none(), "no focus, no caret");
    s.run("E:SetFocus(); E:Hide()").unwrap();
    assert!(s.focused_editbox_text_ui().is_none(), "hidden box: none");
}

/// Click places the cursor at the nearest char boundary (`0x77b800` → `0x77d0d0`), collapsing
/// any selection; dragging extends it (`0x77a860`); release ends the drag.
#[test]
fn click_places_cursor_and_drag_selects() {
    let mut s = script();
    seam_rig(&mut s, "hello world");
    // Box left = 100; the text region fills it (no insets). Byte i sits at x = 100 + 7i.
    // Click at x=121 → advance-space 21 → boundary 3.
    assert!(s.mouse_button(121.0, 60.0, "LeftButton", true));
    let ui = s.focused_editbox_text_ui().unwrap();
    assert_eq!(ui.caret_x, 21.0, "cursor at byte 3");
    assert!(ui.selection.is_empty(), "click collapses");
    // Drag right to x=149 (byte 7): selection 3..7, cursor at 7.
    s.mouse_move(149.0, 60.0);
    let ui = s.focused_editbox_text_ui().unwrap();
    assert_eq!(ui.selection, vec![(0, 21.0, 49.0)]);
    assert_eq!(ui.caret_x, 49.0);
    // Drag back LEFT past the anchor to x=107 (byte 1): the selection flips to 1..3.
    s.mouse_move(107.0, 60.0);
    let ui = s.focused_editbox_text_ui().unwrap();
    assert_eq!(ui.selection, vec![(0, 7.0, 21.0)]);
    // Release ends the drag: further moves change nothing.
    s.mouse_button(107.0, 60.0, "LeftButton", false);
    s.mouse_move(170.0, 60.0);
    assert_eq!(
        s.focused_editbox_text_ui().unwrap().selection,
        vec![(0, 7.0, 21.0)]
    );
    // Nearest-boundary rounding: x=124.9 (advance 24.9): |24.9−21|=3.9 vs |28−24.9|=3.1 → byte 4.
    s.mouse_button(124.9, 60.0, "LeftButton", true);
    assert_eq!(s.focused_editbox_text_ui().unwrap().caret_x, 28.0);
    s.mouse_button(124.9, 60.0, "LeftButton", false);
}

/// Ctrl+arrows jump word boundaries (alnum runs); Ctrl+Shift extends the selection there.
#[test]
fn ctrl_arrows_jump_words() {
    let mut s = script();
    seam_rig(&mut s, "abc def");
    s.editbox_action(EditAction::Move {
        unit: EditUnit::Edge,
        back: true,
        extend: false,
    });
    s.editbox_action(EditAction::Move {
        unit: EditUnit::Word,
        back: false,
        extend: false,
    });
    assert_eq!(
        s.focused_editbox_text_ui().unwrap().caret_x,
        21.0,
        "end of 'abc'"
    );
    s.editbox_action(EditAction::Move {
        unit: EditUnit::Word,
        back: false,
        extend: false,
    });
    assert_eq!(
        s.focused_editbox_text_ui().unwrap().caret_x,
        49.0,
        "end of 'def'"
    );
    s.editbox_action(EditAction::Move {
        unit: EditUnit::Word,
        back: true,
        extend: false,
    });
    assert_eq!(
        s.focused_editbox_text_ui().unwrap().caret_x,
        28.0,
        "start of 'def'"
    );
    // Ctrl+Shift+LEFT from here selects back across 'abc'.
    s.editbox_action(EditAction::Move {
        unit: EditUnit::Word,
        back: true,
        extend: true,
    });
    assert_eq!(
        s.focused_editbox_text_ui().unwrap().selection,
        vec![(0, 0.0, 28.0)]
    );
}

/// Copy needs a selection; cut copies then deletes; the password box yields the mask run, never
/// the real text (RF-0082 §4's placeholder law, mask stand-in).
#[test]
fn copy_cut_and_the_password_placeholder() {
    let mut s = script();
    seam_rig(&mut s, "secret");
    assert_eq!(s.editbox_copy(), None, "no selection, no copy");
    s.run("E:HighlightText(0, 3)").unwrap();
    assert_eq!(s.editbox_copy().as_deref(), Some("sec"));
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "secret");
    assert_eq!(s.editbox_cut().as_deref(), Some("sec"));
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "ret");

    s.run(r#"P = CreateFrame("EditBox", "P"); P:SetPassword(true); P:SetFocus()"#)
        .unwrap();
    s.char_input("h");
    s.char_input("i");
    s.run("P:HighlightText(0, -1)").unwrap();
    assert_eq!(
        s.editbox_copy().as_deref(),
        Some("**"),
        "password copy must never yield the real text"
    );
}

/// The OS-native delete family (no 1.12 counterpart — the host keymap's Option/Cmd/Ctrl
/// Backspace-Delete chords): a word delete takes the adjacent alnum run (+ the separators toward
/// it), an edge delete clears to the line end — and both collapse to "delete the selection" when
/// one exists, like every deletion gesture.
#[test]
fn word_and_edge_deletes() {
    let mut s = script();
    seam_rig(&mut s, "abc def ghi");
    // Word-delete back from the end: "ghi" goes (boundary at 8).
    s.editbox_action(EditAction::Delete {
        unit: EditUnit::Word,
        back: true,
    });
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "abc def ");
    // Again: the space + "def" go together (back-walk skips separators first).
    s.editbox_action(EditAction::Delete {
        unit: EditUnit::Word,
        back: true,
    });
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "abc ");
    // Word-delete forward from HOME: the "abc" run goes, its trailing space survives.
    s.editbox_action(EditAction::Move {
        unit: EditUnit::Edge,
        back: true,
        extend: false,
    });
    s.editbox_action(EditAction::Delete {
        unit: EditUnit::Word,
        back: false,
    });
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), " ");

    // Selection-first: with a live selection, a word delete removes exactly it.
    s.run(r#"E:SetText("hello world"); E:HighlightText(2, 5)"#)
        .unwrap();
    s.editbox_action(EditAction::Delete {
        unit: EditUnit::Word,
        back: true,
    });
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "he world");

    // Edge-delete back (macOS Cmd+Backspace): clears to the start — the whole line from the end.
    s.run(r#"E:SetText("clear me")"#).unwrap();
    s.editbox_action(EditAction::Delete {
        unit: EditUnit::Edge,
        back: true,
    });
    assert_eq!(s.eval::<String>("return E:GetText()").unwrap(), "");
    // Inert at the boundary: nothing to delete consumes without firing OnTextChanged.
    s.run("n = 0; E:SetScript('OnTextChanged', function() n = n + 1 end)")
        .unwrap();
    assert!(s.editbox_action(EditAction::Delete {
        unit: EditUnit::Edge,
        back: true
    }));
    assert_eq!(s.eval::<i64>("return n").unwrap(), 0);
}

/// SelectAll (the ref's Ctrl+A → `HighlightText(0, -1)` law): whole text selected, caret to the
/// end; typing replaces the lot.
#[test]
fn select_all_action() {
    let mut s = script();
    seam_rig(&mut s, "abc def");
    s.editbox_action(EditAction::SelectAll);
    let ui = s.focused_editbox_text_ui().unwrap();
    assert_eq!(ui.selection, vec![(0, 0.0, 49.0)], "all 7 bytes selected");
    assert_eq!(ui.caret_x, 49.0, "caret at the end");
    s.char_input("z");
    assert_eq!(
        s.eval::<String>("return E:GetText()").unwrap(),
        "z",
        "an insert replaces the whole selection"
    );
}

/// The scroll window follows the cursor: typing past the box edge advances `display_from`
/// (whole chars); HOME snaps it back (`0x77da80`'s stay-visible invariant, the char-granular
/// `E+0x348` window).
#[test]
fn scroll_window_keeps_the_cursor_visible() {
    let mut s = script();
    // 40 bytes × 7px = 280px in a 200px box (avail 198): the window must slide.
    seam_rig(&mut s, &"x".repeat(40));
    let ui = s.focused_editbox_text_ui().unwrap();
    assert!(ui.display_from > 0, "overlong text must scroll: {ui:?}");
    assert!(ui.caret_x <= 198.0, "caret inside the window: {ui:?}");
    assert_eq!(
        ui.caret_x,
        (40 - ui.display_from) as f32 * 7.0,
        "caret x measures from the window origin"
    );
    // HOME: the window snaps back to 0.
    s.editbox_action(EditAction::Move {
        unit: EditUnit::Edge,
        back: true,
        extend: false,
    });
    let ui = s.focused_editbox_text_ui().unwrap();
    assert_eq!(ui.display_from, 0);
    assert_eq!(ui.caret_x, 0.0);
}

/// The blink law (`0x77a790`): 0.5 s half-period toggle, reset shown by every edit.
#[test]
fn caret_blinks_on_tick_and_resets_on_edit() {
    let mut s = script();
    seam_rig(&mut s, "a");
    assert!(s.focused_editbox_text_ui().unwrap().caret_on);
    s.tick(0.6); // crosses the 0.5 default → hidden
    assert!(!s.focused_editbox_text_ui().unwrap().caret_on);
    s.tick(0.6); // crosses again → shown
    assert!(s.focused_editbox_text_ui().unwrap().caret_on);
    s.tick(0.6);
    assert!(!s.focused_editbox_text_ui().unwrap().caret_on);
    // An edit shows the caret immediately (blink reset).
    s.char_input("b");
    answer_advances(&mut s);
    assert!(s.focused_editbox_text_ui().unwrap().caret_on);
}
