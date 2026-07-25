//! The layout change gate: a resolve whose inputs are byte-identical to the last converged one is
//! skipped outright (`script::layout::InputFingerprint`).
//!
//! These assert on `Model::layout_solves` — the count of times the fixpoint actually ran — rather
//! than on the rects, because rect equality alone cannot distinguish "the gate skipped" from "the
//! gate re-solved and got the same answer". The value of the gate is precisely the difference.

use super::common::script;
use crate::script::{Model, UiScript};

/// How many times the fixpoint has run.
fn solves(s: &UiScript) -> u64 {
    s.lua()
        .app_data_ref::<Model>()
        .expect("model app_data")
        .layout_solves
}

/// A frame anchored to the screen, plus a child hanging off it — enough that a move has to
/// propagate, so a wrongly-skipped resolve would be visible in the child too.
fn setup(s: &UiScript) {
    s.run(
        r#"
        parent = CreateFrame("Frame", "Parent", nil)
        parent:SetWidth(100); parent:SetHeight(40)
        parent:SetPoint("TOPLEFT", nil, "TOPLEFT", 10, -10)
        child = CreateFrame("Frame", "Child", parent)
        child:SetWidth(20); child:SetHeight(20)
        child:SetPoint("TOPLEFT", parent, "BOTTOMRIGHT", 0, 0)
        "#,
    )
    .expect("setup");
}

#[test]
fn an_unchanged_resolve_does_not_run_the_fixpoint() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    setup(&s);

    s.resolve();
    let after_first = solves(&s);
    assert_eq!(after_first, 1, "the first resolve must run");

    // Nothing touched in between: every further resolve is a no-op.
    for _ in 0..5 {
        s.resolve();
    }
    assert_eq!(
        solves(&s),
        after_first,
        "resolves with identical inputs must be skipped, not re-run"
    );
}

#[test]
fn moving_a_frame_reopens_the_gate_and_propagates() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    setup(&s);
    s.resolve();
    s.resolve(); // settle: the gate is now closed
    let before = solves(&s);

    let child_left = |s: &UiScript| -> f32 { s.eval::<f32>("return Child:GetLeft()").unwrap() };
    let first = child_left(&s);

    s.run("parent:SetPoint(\"TOPLEFT\", nil, \"TOPLEFT\", 60, -10)")
        .expect("move");
    s.resolve();
    assert_eq!(solves(&s), before + 1, "a SetPoint must reopen the gate");
    assert!(
        (child_left(&s) - (first + 50.0)).abs() < 0.001,
        "the move must propagate to the child: {} -> {}",
        first,
        child_left(&s)
    );

    // And it closes again once the move has settled.
    let after = solves(&s);
    s.resolve();
    assert_eq!(
        solves(&s),
        after,
        "the gate must close again after the move"
    );
}

/// Resizing the window moves every top-level frame — the gate must never swallow it.
#[test]
fn a_screen_resize_reopens_the_gate() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    setup(&s);
    s.resolve();
    s.resolve();
    let before = solves(&s);

    s.set_screen_size(1024.0, 768.0);
    s.resolve();
    assert_eq!(
        solves(&s),
        before + 1,
        "a screen-size change must reopen the gate"
    );
}

/// A region's own `SetPoint` writes `region_data`, not `layout_inputs` — the half of the read set
/// that is easiest to leave out of a change gate, and the one the region sweep consumes.
#[test]
fn moving_a_region_reopens_the_gate() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    setup(&s);
    s.run(
        r#"
        tex = parent:CreateTexture(nil, "ARTWORK")
        tex:SetWidth(10); tex:SetHeight(10)
        tex:SetPoint("TOPLEFT", parent, "TOPLEFT", 0, 0)
        "#,
    )
    .expect("region");
    s.resolve();
    s.resolve();
    let before = solves(&s);

    s.run("tex:SetPoint(\"TOPLEFT\", parent, \"TOPLEFT\", 25, 0)")
        .expect("move region");
    s.resolve();
    assert_eq!(
        solves(&s),
        before + 1,
        "a region SetPoint must reopen the gate"
    );
}

/// Hiding a frame does NOT move any rect (the client resolves hidden frames too — visibility is an
/// extract-time filter), so the gate is right to stay closed. Pinned deliberately: it is the one
/// place where "nothing to re-solve" is surprising, and a future reader tempted to dirty on
/// show/hide should see that the current model does not need it.
#[test]
fn hiding_a_frame_does_not_reopen_the_gate() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    setup(&s);
    s.resolve();
    s.resolve();
    let before = solves(&s);

    s.run("parent:Hide()").expect("hide");
    s.resolve();
    assert_eq!(
        solves(&s),
        before,
        "visibility does not move rects, so the gate stays closed"
    );
}
