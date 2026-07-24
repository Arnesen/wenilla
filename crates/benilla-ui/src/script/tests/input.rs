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
