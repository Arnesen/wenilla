//! Input / hit-testing (decision 0068; spec-faithful, not byte-pinned).

use super::common::script;

#[test]
fn enable_mouse_gates_hit_testing() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        who = nil
        -- Two full-screen frames. `b` is created later (drawn on top) but mouse-disabled, so it is
        -- transparent to hits; the enabled frame behind it (`a`) must capture.
        local a = CreateFrame("Frame", "A")
        a:SetPoint("BOTTOMLEFT", 0, 0); a:SetSize(800, 600); a:EnableMouse(true)
        a:SetScript("OnEnter", function(self) who = self:GetName() end)
        local b = CreateFrame("Frame", "B")
        b:SetPoint("BOTTOMLEFT", 0, 0); b:SetSize(800, 600); b:EnableMouse(false)
        b:SetScript("OnEnter", function(self) who = self:GetName() end)
        assert(a:IsMouseEnabled() == true and b:IsMouseEnabled() == false)
    "#,
    )
    .unwrap();
    s.resolve();

    let hit = s.mouse_move(400.0, 300.0);
    assert!(
        hit.is_some(),
        "a mouse-enabled frame under the cursor captures"
    );
    assert_eq!(
        s.eval::<String>("return who").unwrap(),
        "A",
        "the top frame is mouse-disabled ⇒ the enabled frame behind it captures"
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn topmost_by_draw_order_captures_among_overlapping_enabled_frames() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        who = nil
        local a = CreateFrame("Frame", "A")
        a:SetPoint("BOTTOMLEFT", 0, 0); a:SetSize(800, 600); a:EnableMouse(true)
        a:SetScript("OnEnter", function(self) who = self:GetName() end)
        local b = CreateFrame("Frame", "B")
        b:SetPoint("BOTTOMLEFT", 0, 0); b:SetSize(800, 600); b:EnableMouse(true)
        b:SetScript("OnEnter", function(self) who = self:GetName() end)
    "#,
    )
    .unwrap();
    s.resolve();

    // Same strata/level: B has the later insertion ⇒ drawn on top ⇒ captures.
    s.mouse_move(400.0, 300.0);
    assert_eq!(s.eval::<String>("return who").unwrap(), "B");

    // Move off everything to clear the mouseover (fires OnLeave, resets focus) before re-testing.
    s.mouse_move(-10.0, -10.0);

    // Raise A above B by strata (no rect change ⇒ no re-resolve needed): A now captures.
    s.run("A:SetFrameStrata('DIALOG')").unwrap();
    s.mouse_move(400.0, 300.0);
    assert_eq!(s.eval::<String>("return who").unwrap(), "A");

    s.mouse_move(-10.0, -10.0);

    // Put B in the same (DIALOG) strata but a higher frame level: B captures again.
    s.run("B:SetFrameStrata('DIALOG'); B:SetFrameLevel(10)")
        .unwrap();
    s.mouse_move(400.0, 300.0);
    assert_eq!(s.eval::<String>("return who").unwrap(), "B");
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn mouse_move_fires_enter_then_leave_across_a_boundary_with_correct_self() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        enters, leaves = 0, 0
        enter_self_ok, leave_self_ok = false, false
        local a = CreateFrame("Frame", "A")   -- left half only
        a:SetPoint("BOTTOMLEFT", 0, 0); a:SetSize(400, 600); a:EnableMouse(true)
        a:SetScript("OnEnter", function(self) enters = enters + 1; enter_self_ok = (self == a) end)
        a:SetScript("OnLeave", function(self) leaves = leaves + 1; leave_self_ok = (self == a) end)
    "#,
    )
    .unwrap();
    s.resolve();

    s.mouse_move(200.0, 300.0); // inside A ⇒ OnEnter
    s.mouse_move(600.0, 300.0); // outside A ⇒ OnLeave
    let (enters, leaves): (i64, i64) = s.eval("return enters, leaves").unwrap();
    assert_eq!((enters, leaves), (1, 1));
    assert!(
        s.eval::<bool>("return enter_self_ok").unwrap(),
        "OnEnter self is the frame"
    );
    assert!(
        s.eval::<bool>("return leave_self_ok").unwrap(),
        "OnLeave self is the frame"
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn onclick_fires_on_press_release_same_frame_not_when_release_lands_elsewhere() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        clicks_a, clicks_b, click_btn = 0, 0, nil
        local a = CreateFrame("Frame", "A")   -- left half
        a:SetPoint("BOTTOMLEFT", 0, 0); a:SetSize(400, 600); a:EnableMouse(true)
        a:SetScript("OnClick", function(self, button, down) clicks_a = clicks_a + 1; click_btn = button end)
        local b = CreateFrame("Frame", "B")   -- right half
        b:SetPoint("BOTTOMLEFT", 400, 0); b:SetSize(400, 600); b:EnableMouse(true)
        b:SetScript("OnClick", function(self, button, down) clicks_b = clicks_b + 1 end)
    "#,
    )
    .unwrap();
    s.resolve();

    // Press + release both on A ⇒ OnClick on A (button arg == "LeftButton").
    s.mouse_button(200.0, 300.0, "LeftButton", true);
    s.mouse_button(200.0, 300.0, "LeftButton", false);
    assert_eq!(s.eval::<i64>("return clicks_a").unwrap(), 1);
    assert_eq!(s.eval::<String>("return click_btn").unwrap(), "LeftButton");

    // Press on A, release on B ⇒ no OnClick on either frame.
    s.mouse_button(200.0, 300.0, "LeftButton", true);
    s.mouse_button(600.0, 300.0, "LeftButton", false);
    assert_eq!(
        s.eval::<i64>("return clicks_a").unwrap(),
        1,
        "release landed off A ⇒ no click on A"
    );
    assert_eq!(
        s.eval::<i64>("return clicks_b").unwrap(),
        0,
        "release on B was not preceded by a press on B"
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn hidden_or_effective_hidden_frame_never_captures() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        entered = false
        -- own shown = false
        local own = CreateFrame("Frame", "OwnHidden")
        own:SetPoint("BOTTOMLEFT", 0, 0); own:SetSize(800, 600); own:EnableMouse(true)
        own:SetScript("OnEnter", function() entered = true end)
        own:Hide()
        -- effective-hidden: child is shown but its parent is hidden
        local parent = CreateFrame("Frame", "Par")
        parent:SetPoint("BOTTOMLEFT", 0, 0); parent:SetSize(800, 600)
        local child = CreateFrame("Frame", "Ch", parent)
        child:SetPoint("BOTTOMLEFT", 0, 0); child:SetSize(800, 600); child:EnableMouse(true)
        child:SetScript("OnEnter", function() entered = true end)
        parent:Hide()
    "#,
    )
    .unwrap();
    s.resolve();

    assert!(
        s.hit_test(400.0, 300.0).is_none(),
        "no visible mouse-enabled frame ⇒ no capture"
    );
    assert!(s.mouse_move(400.0, 300.0).is_none());
    assert!(
        !s.eval::<bool>("return entered").unwrap(),
        "hidden / effective-hidden frames fire no OnEnter"
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn mouse_wheel_passes_delta_to_the_captured_frame() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        wheel = nil
        local a = CreateFrame("Frame", "A")
        a:SetPoint("BOTTOMLEFT", 0, 0); a:SetSize(800, 600); a:EnableMouse(true)
        a:SetScript("OnMouseWheel", function(self, delta) wheel = delta end)
    "#,
    )
    .unwrap();
    s.resolve();

    s.mouse_wheel(400.0, 300.0, 1.0);
    assert!((s.eval::<f64>("return wheel").unwrap() - 1.0).abs() < 1e-6);
    s.mouse_wheel(400.0, 300.0, -1.0);
    assert!((s.eval::<f64>("return wheel").unwrap() + 1.0).abs() < 1e-6);
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn hit_rect_insets_shrink_the_mouse_rect_only() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    // A 100×100 frame at the origin with the micro-button shape: an 18-unit dead header at the
    // top, and 5 off each other side.
    s.run(
        r#"
        entered = false
        local a = CreateFrame("Frame", "A")
        a:SetPoint("BOTTOMLEFT", 0, 0); a:SetSize(100, 100); a:EnableMouse(true)
        a:SetScript("OnEnter", function() entered = true end)
        a:SetHitRectInsets(5, 5, 18, 5)
        local l, r, t, b = a:GetHitRectInsets()
        assert(l == 5 and r == 5 and t == 18 and b == 5)
    "#,
    )
    .unwrap();
    s.resolve();

    // Geometry is untouched — insets move the MOUSE rect, never the frame.
    assert_eq!(s.eval::<f64>("return A:GetHeight()").unwrap(), 100.0);
    assert_eq!(s.eval::<f64>("return A:GetTop()").unwrap(), 100.0);

    // Inside the frame but inside the dead header ⇒ no capture (the case the ref's top=18 buys:
    // the empty band above a micro button's art must not eat the click).
    assert!(
        s.hit_test(50.0, 90.0).is_none(),
        "a point in the inset header is outside the hit rect"
    );
    assert!(s.mouse_move(50.0, 90.0).is_none());
    assert!(!s.eval::<bool>("return entered").unwrap());
    // …and each of the other three insets bites too.
    assert!(s.hit_test(2.0, 50.0).is_none(), "left inset");
    assert!(s.hit_test(98.0, 50.0).is_none(), "right inset");
    assert!(s.hit_test(50.0, 2.0).is_none(), "bottom inset");

    // Just below the header, still inside the shrunken rect ⇒ captures.
    assert!(s.hit_test(50.0, 80.0).is_some());
    assert!(s.mouse_move(50.0, 80.0).is_some());
    assert!(s.eval::<bool>("return entered").unwrap());
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn hit_rect_insets_default_to_zero() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(r#"local a = CreateFrame("Frame", "Plain")"#).unwrap();
    let (l, r, t, b) = s
        .eval::<(f64, f64, f64, f64)>("return Plain:GetHitRectInsets()")
        .unwrap();
    assert_eq!((l, r, t, b), (0.0, 0.0, 0.0, 0.0), "no inset by default");
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// `EnableMouseWheel`/`IsMouseWheelEnabled` round-trip, and the two kinds born wheel-enabled
/// (decision 1198).
///
/// The flag is real and settable; **the dispatch is deliberately not gated on it yet** — see
/// `object::frame_state`'s note for the 44 shipped `OnMouseWheel` sites that declare no
/// `enableMouseWheel` and the concrete condition for flipping it. This test pins the flag's own
/// behaviour so that flip is a one-line change with a test already standing behind it.
#[test]
fn the_mouse_wheel_flag_round_trips_and_the_scrolling_kinds_are_born_enabled() {
    let s = script();
    s.run(
        r#"
        Plain  = CreateFrame("Frame", "WheelPlain")
        Scroll = CreateFrame("ScrollFrame", "WheelScroll")
        Msg    = CreateFrame("ScrollingMessageFrame", "WheelMsg")
    "#,
    )
    .unwrap();

    // A plain frame is born wheel-deaf, like WoW's own default.
    assert!(!s
        .eval::<bool>("return Plain:IsMouseWheelEnabled()")
        .unwrap());
    // ...and the two kinds whose ctor takes the wheel are born enabled, the same by-construction
    // argument `mouse_enabled` already makes for a Button.
    assert!(s
        .eval::<bool>("return Scroll:IsMouseWheelEnabled()")
        .unwrap());
    assert!(s.eval::<bool>("return Msg:IsMouseWheelEnabled()").unwrap());

    s.run("Plain:EnableMouseWheel(true)").unwrap();
    assert!(s
        .eval::<bool>("return Plain:IsMouseWheelEnabled()")
        .unwrap());
    s.run("Plain:EnableMouseWheel(false)").unwrap();
    assert!(!s
        .eval::<bool>("return Plain:IsMouseWheelEnabled()")
        .unwrap());
    // Lua truthiness, like every other flag binding: `EnableMouseWheel(1)` is on.
    s.run("Plain:EnableMouseWheel(1)").unwrap();
    assert!(s
        .eval::<bool>("return Plain:IsMouseWheelEnabled()")
        .unwrap());
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// A closed-vocabulary attribute survives stray whitespace (decision 1204).
///
/// The corpus case: `zBar.xml:146` — a shipped, working 1.12 addon — declares
/// `frameStrata="BACKGROUND "` with a trailing space. The real client took it; we refused, and the
/// refusal took the frame's whole `<Frames>` subtree with it, so the addon never loaded at all.
///
/// The three parsers are covered together because the next one to meet a stray space should not
/// need its own bug report.
#[test]
fn an_enum_attribute_tolerates_the_whitespace_a_real_addon_ships() {
    let s = script();
    s.run(
        r#"
        Padded = CreateFrame(" Frame ", "PaddedKind")
        Padded:SetFrameStrata("BACKGROUND ")
        Trailing = Padded:CreateTexture("PaddedTex", " OVERLAY")
    "#,
    )
    .unwrap();
    assert!(s.errors().is_empty(), "{:?}", s.errors());
    assert_eq!(
        s.eval::<String>("return Padded:GetFrameStrata()").unwrap(),
        "BACKGROUND",
        "the frame took the strata, and reports it in the canonical spelling"
    );
    // ...and the padded frame KIND resolved too — an unknown kind raises from CreateFrame, so
    // reaching this line at all is the assertion.
    assert!(s.eval::<bool>("return PaddedKind ~= nil").unwrap());
    assert!(s.eval::<bool>("return PaddedTex ~= nil").unwrap());
}

// ── OnDoubleClick — the corpus's biggest script gap (250 sites / 85 addons) ───────────────────
//
// Every rule below is byte-verified, from the §5 cross-check this work dispatched into wow-re
// (`system/ui/scratch/button-doubleclick-law.md`): the interval is a hardcoded **300 ms**
// (`0x77937b cmp ecx, 0x12c`), the fire site is the mouse-**UP** dispatcher `0x7792d0` alone, the
// double leg **replaces** the second `OnClick` (`0x77939d jmp` past `call [edx+0x94]`), a completed
// double **zeroes** the stamp so clicks pair up, the detector is armed only when the frame carries
// an `OnDoubleClick` script (`[+0x4d4] != 0`), and it carries **no button identity** — what
// normally confines it to the left button is `RegisterForClicks`, not the detector.

/// Two fast clicks fire `OnDoubleClick(self, button)` — and the second `OnClick` does **not** fire,
/// because the two legs are exclusive. (This was the interim implementation's mistake: it fired
/// both, on the press edge, at 500 ms. All three were corrected at the bytes.)
#[test]
fn a_second_fast_click_fires_on_double_click_instead_of_the_second_on_click() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        clicks, doubles, dblbtn = 0, 0, nil
        local b = CreateFrame("Button", "DblB")
        b:SetPoint("BOTTOMLEFT", 0, 0); b:SetSize(200, 200); b:EnableMouse(true)
        b:SetScript("OnClick", function(self, button) clicks = clicks + 1 end)
        b:SetScript("OnDoubleClick", function(self, button) doubles = doubles + 1 dblbtn = button end)
    "#,
    )
    .unwrap();
    s.resolve();

    // Click one. Note the press alone does nothing: the fire site is the mouse-UP dispatcher.
    s.mouse_button(50.0, 50.0, "LeftButton", true);
    assert_eq!(
        s.eval::<i64>("return clicks").unwrap(),
        0,
        "the default RegisterForClicks is LeftButtonUp — a press fires no click at all"
    );
    s.mouse_button(50.0, 50.0, "LeftButton", false);
    assert_eq!(s.eval::<i64>("return clicks").unwrap(), 1);
    assert_eq!(
        s.eval::<i64>("return doubles").unwrap(),
        0,
        "one click is not a double click"
    );

    // Click two, inside the 300 ms.
    s.tick(0.1);
    s.mouse_button(50.0, 50.0, "LeftButton", true);
    s.mouse_button(50.0, 50.0, "LeftButton", false);
    assert_eq!(
        s.eval::<i64>("return doubles").unwrap(),
        1,
        "the second click inside the interval is a double click"
    );
    assert_eq!(
        s.eval::<String>("return dblbtn").unwrap(),
        "LeftButton",
        "OnDoubleClick(self, button) — the same single button-name arg OnClick gets"
    );
    assert_eq!(
        s.eval::<i64>("return clicks").unwrap(),
        1,
        "…and it REPLACED the second OnClick — the two legs are exclusive"
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// **The gate that keeps rapid clicking from breaking every button in the UI.** The binary's chain
/// tests `[+0x4d4] != 0`, so a widget with no `OnDoubleClick` script takes the single leg every
/// time. Without that test, the exclusivity above would silently swallow every second `OnClick` on
/// every frame in the game.
#[test]
fn a_frame_with_no_double_click_handler_keeps_every_rapid_on_click() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        clicks = 0
        local b = CreateFrame("Button", "NoDbl")
        b:SetPoint("BOTTOMLEFT", 0, 0); b:SetSize(200, 200); b:EnableMouse(true)
        b:SetScript("OnClick", function() clicks = clicks + 1 end)
    "#,
    )
    .unwrap();
    s.resolve();
    for _ in 0..4 {
        s.mouse_button(50.0, 50.0, "LeftButton", true);
        s.mouse_button(50.0, 50.0, "LeftButton", false);
    }
    assert_eq!(
        s.eval::<i64>("return clicks").unwrap(),
        4,
        "no OnDoubleClick script ⇒ four rapid clicks are four clicks"
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// The boundary: too slow, a different frame, and the PAIRING rule (a completed double zeroes the
/// stamp, so four rapid clicks read Click · Double · Click · Double — there is no triple-click and
/// no run of doubles).
#[test]
fn the_double_click_boundary_is_time_and_frame_and_clicks_pair_up() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        clicks, doubles = 0, 0
        local function watch(f)
            f:SetScript("OnClick", function() clicks = clicks + 1 end)
            f:SetScript("OnDoubleClick", function() doubles = doubles + 1 end)
        end
        local a = CreateFrame("Button", "DblA")
        a:SetPoint("BOTTOMLEFT", 0, 0); a:SetSize(100, 100); a:EnableMouse(true); watch(a)
        local b = CreateFrame("Button", "DblBB")
        b:SetPoint("BOTTOMLEFT", 300, 0); b:SetSize(100, 100); b:EnableMouse(true); watch(b)
    "#,
    )
    .unwrap();
    s.resolve();
    let click = |s: &mut crate::script::UiScript, x: f32| {
        s.mouse_button(x, 50.0, "LeftButton", true);
        s.mouse_button(x, 50.0, "LeftButton", false);
    };

    // (1) TOO SLOW — 0.35 s > 300 ms, so the second click is a fresh first half.
    click(&mut s, 50.0);
    s.tick(0.35);
    click(&mut s, 50.0);
    assert_eq!(s.eval::<i64>("return doubles").unwrap(), 0);
    assert_eq!(s.eval::<i64>("return clicks").unwrap(), 2);

    // (2) A DIFFERENT FRAME — fast, but the timestamp lives on the WIDGET, so B has its own.
    s.run("clicks, doubles = 0, 0").unwrap();
    s.tick(1.0);
    click(&mut s, 50.0);
    click(&mut s, 350.0);
    assert_eq!(s.eval::<i64>("return doubles").unwrap(), 0);
    assert_eq!(s.eval::<i64>("return clicks").unwrap(), 2);

    // (3) PAIRING — four rapid clicks on one frame are Click · Double · Click · Double.
    s.run("clicks, doubles = 0, 0").unwrap();
    s.tick(1.0);
    for _ in 0..4 {
        click(&mut s, 50.0);
    }
    assert_eq!(
        (
            s.eval::<i64>("return clicks").unwrap(),
            s.eval::<i64>("return doubles").unwrap()
        ),
        (2, 2),
        "clicks pair up — the completed double zeroes the stamp, so there is no triple-click"
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// **The detector carries no button identity** — the surprising half of the law, and the one an
/// implementation keyed by mouse button would get wrong. `[CButton+0x334]` is a bare timestamp, so
/// a widget registered for two `…Up` types completes a double click across them, with `arg1` = the
/// button of the SECOND click. A stock button never sees this only because `RegisterForClicks`
/// defaults to `{"LeftButtonUp"}`.
#[test]
fn a_multi_registered_button_pairs_a_left_click_with_a_right_one() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        doubles, dblbtn = 0, nil
        local b = CreateFrame("Button", "MixDbl")
        b:SetPoint("BOTTOMLEFT", 0, 0); b:SetSize(200, 200); b:EnableMouse(true)
        b:RegisterForClicks("LeftButtonUp", "RightButtonUp")
        b:SetScript("OnDoubleClick", function(self, button) doubles = doubles + 1 dblbtn = button end)
    "#,
    )
    .unwrap();
    s.resolve();
    s.mouse_button(50.0, 50.0, "LeftButton", true);
    s.mouse_button(50.0, 50.0, "LeftButton", false);
    s.tick(0.1);
    s.mouse_button(50.0, 50.0, "RightButton", true);
    s.mouse_button(50.0, 50.0, "RightButton", false);
    assert_eq!(s.eval::<i64>("return doubles").unwrap(), 1);
    assert_eq!(
        s.eval::<String>("return dblbtn").unwrap(),
        "RightButton",
        "arg1 is the button of the completing click, not of the one that armed it"
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// **Nothing clears a half-finished double click** — not hide, not disable, not the cursor leaving
/// the window. `[+0x334]` has three writers image-wide and none of them is any of those, so this
/// pins the *absence* of the tidy-up that looks like it belongs in
/// [`crate::script::UiScript::pointer_left_window`].
#[test]
fn a_half_finished_double_click_survives_the_cursor_leaving_the_window() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        doubles = 0
        local b = CreateFrame("Button", "LeaveDbl")
        b:SetPoint("BOTTOMLEFT", 0, 0); b:SetSize(200, 200); b:EnableMouse(true)
        b:SetScript("OnDoubleClick", function() doubles = doubles + 1 end)
    "#,
    )
    .unwrap();
    s.resolve();
    s.mouse_button(50.0, 50.0, "LeftButton", true);
    s.mouse_button(50.0, 50.0, "LeftButton", false);
    s.pointer_left_window();
    s.tick(0.1);
    s.mouse_button(50.0, 50.0, "LeftButton", true);
    s.mouse_button(50.0, 50.0, "LeftButton", false);
    assert_eq!(
        s.eval::<i64>("return doubles").unwrap(),
        1,
        "the pointer left and came back inside 300 ms — the reference still pairs them"
    );
}

/// The load-bearing regression guard for the whole job: accepting `OnDoubleClick` in
/// `SCRIPT_KINDS` without wiring the detector would make this test's `SetScript` succeed and its
/// handler never run — the silent-capability trap. Asserting the *fire* is what makes the row real.
#[test]
fn set_script_on_double_click_is_accepted_because_something_fires_it() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        ran = false
        Btn = CreateFrame("Button", "DblAccepted")
        Btn:SetPoint("BOTTOMLEFT", 0, 0); Btn:SetSize(100, 100); Btn:EnableMouse(true)
        Btn:SetScript("OnDoubleClick", function() ran = true end)
    "#,
    )
    .unwrap();
    assert!(
        s.eval::<bool>(r#"return Btn:GetScript("OnDoubleClick") ~= nil"#)
            .unwrap(),
        "SetScript stored it"
    );
    s.resolve();
    for _ in 0..2 {
        s.mouse_button(50.0, 50.0, "LeftButton", true);
        s.mouse_button(50.0, 50.0, "LeftButton", false);
    }
    assert!(s.eval::<bool>("return ran").unwrap(), "…and it FIRED");
}

/// The script names that deliberately still raise, and why each is out (the reasons live at
/// `object::events_regions::set_script`). This is the other half of the rule "a name is accepted
/// only once something fires it": a future widening has to delete a line here, which is exactly
/// the moment to check something fires it.
#[test]
fn the_unfired_script_kinds_still_raise_rather_than_silently_accepting() {
    let s = script();
    s.run(r#"Raiser = CreateFrame("EditBox", "RaiserBox")"#)
        .unwrap();
    for name in [
        // The FLAG exists (`EnableKeyboard`/`IsKeyboardEnabled`, 1252); what is still absent is the
        // keyboard INDEX and the strata walk that would deliver a key to a frame carrying it, so
        // these three stay unfired (wow-re `scripts-auto-enable.md` kinds 0/1). Being enabled is
        // not being a handler — the carve is explicit that the two are separable.
        "OnKeyDown",
        "OnKeyUp",
        "OnChar",
        // Caret geometry is host-side; its four float args would all be zero.
        "OnCursorChanged",
        // 2.0's secure-frame system — no such slot exists in any 1.12 resolver.
        "OnAttributeChanged",
        // Real 1.12 slots we do not fire, and zero corpus call sites.
        "OnHorizontalScroll",
        "OnHyperlinkEnter",
        "OnMessageScrollChanged",
        "OnUpdateModel",
        "OnAnimFinished",
        "OnInputLanguageChanged",
    ] {
        let err = s
            .run(&format!(r#"Raiser:SetScript("{name}", function() end)"#))
            .unwrap_err();
        assert!(
            err.to_string().contains(name),
            "{name} must raise by name, got {err}"
        );
    }
}
