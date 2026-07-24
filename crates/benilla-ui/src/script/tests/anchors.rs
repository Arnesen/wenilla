//! SetPoint + resolve → GetWidth/GetHeight + rect.

use super::common::script;
use crate::layout::Rect;

// Regression: an *explicit* `nil` relativeTo must still consume its argument slot, so the
// relativePoint + offsets that follow line up. The `SetPoint("P", nil, "P", x, y)` form is the
// common FrameXML idiom for "anchor to the screen at an offset"; a leading nil that failed to
// advance the cursor silently dropped the offsets (screen-anchored frames pinned to the corner).
#[test]
fn setpoint_explicit_nil_relative_to_keeps_offsets() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local f = CreateFrame("Frame", "Nil")
        f:SetPoint("TOPLEFT", nil, "TOPLEFT", 40, -40)
        f:SetSize(300, 200)
    "#,
    )
    .unwrap();
    s.resolve();
    let rect = s
        .extract()
        .iter()
        .find_map(|q| match q.target {
            crate::order::ZTarget::Frame(_) => q.rect,
            _ => None,
        })
        .expect("resolved frame rect");
    // screen [0,0,600,800], TOPLEFT+(40,-40): left 40, top 560, size 300×200.
    assert_eq!(rect, Rect::new(360.0, 40.0, 560.0, 340.0));
}

#[test]
fn setpoint_resolve_size_and_rect() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0); // screen rect [bottom 0, left 0, top 600, right 800]
    s.run(
        r#"
        local f = CreateFrame("Frame", "Sized")
        f:SetPoint("TOPLEFT", 10, -5)   -- relativeTo = screen (default), relativePoint = TOPLEFT
        f:SetSize(200, 50)
    "#,
    )
    .unwrap();
    s.resolve();

    let (w, h): (f32, f32) = s
        .eval("return Sized:GetWidth(), Sized:GetHeight()")
        .unwrap();
    assert_eq!(w, 200.0);
    assert_eq!(h, 50.0);

    // Hand-computed (layout.md oracle): TOPLEFT anchored to screen [0,0,600,800] at (10,-5), size
    // 200×50 → Rect(bottom 545, left 10, top 595, right 210).
    let quads = s.extract();
    let frame_rect = quads
        .iter()
        .find_map(|q| match q.target {
            crate::order::ZTarget::Frame(_) => q.rect,
            _ => None,
        })
        .expect("resolved frame rect");
    assert_eq!(frame_rect, Rect::new(545.0, 10.0, 595.0, 210.0));
}

#[test]
fn getwidth_falls_back_to_explicit_size_before_resolve() {
    let s = script();
    // No SetPoint ⇒ unresolvable; GetWidth returns the explicit SetSize value.
    let w: f32 = s
        .eval(r#"local f = CreateFrame("Frame"); f:SetWidth(123); return f:GetWidth()"#)
        .unwrap();
    assert_eq!(w, 123.0);
}

// A *named* relativeTo that doesn't resolve falls back to the parent/owner — the client's
// behavior — but must SAY so: the silent version misdirected ItemTextFrame's scrollbar track
// onto the parchment (an XML forward reference; anchors resolve at SetPoint time). Both the
// frame and the region SetPoint paths warn.
#[test]
fn setpoint_unresolved_name_warns() {
    let mut s = script();
    s.run(
        r#"
        local f = CreateFrame("Frame", "Orphan")
        f:SetPoint("TOPLEFT", "NoSuchFrame", "TOPLEFT", 0, 0)
        local t = f:CreateTexture(nil, "ARTWORK")
        t:SetPoint("TOPRIGHT", "NoSuchRegion")
    "#,
    )
    .unwrap();
    let w = s.take_warnings();
    assert!(
        w.iter()
            .any(|w| w.contains("Orphan") && w.contains("NoSuchFrame")),
        "frame path: {w:#?}"
    );
    assert!(
        w.iter()
            .any(|w| w.contains("Orphan") && w.contains("NoSuchRegion")),
        "region path: {w:#?}"
    );
    // A resolvable name stays silent.
    s.run(r#"CreateFrame("Frame", "Target"); Orphan:SetPoint("BOTTOMLEFT", "Target", "TOPLEFT")"#)
        .unwrap();
    assert!(s.take_warnings().is_empty());
}
