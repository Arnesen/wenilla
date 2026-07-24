//! Backdrop: Lua verbs + extract emission (backdrop-mechanism.md).

use super::common::script;
use crate::script::*;

// SetBackdrop installs the plate; SetBackdropColor tints the bg only, SetBackdropBorderColor all 8
// border pieces; extract emits bg-then-border at the frame's own slot with those colors.
#[test]
fn backdrop_installs_and_extracts_pieces_with_colors() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local f = CreateFrame("Frame", "Plate")
        f:SetPoint("TOPLEFT", nil, "TOPLEFT", 100, -100)
        f:SetSize(200, 100)
        f:SetBackdrop({
            bgFile = "bg", edgeFile = "edge", tile = true,
            tileSize = 16, edgeSize = 16,
            insets = { left = 5, right = 5, top = 5, bottom = 5 },
        })
        f:SetBackdropColor(0.09, 0.09, 0.19)
        f:SetBackdropBorderColor(1, 1, 1)
    "#,
    )
    .unwrap();
    s.resolve();
    let pieces: Vec<(String, [f32; 4])> = s
        .extract()
        .into_iter()
        .filter_map(|q| match q.content {
            QuadContent::Backdrop { path, color, .. } => Some((path, color)),
            _ => None,
        })
        .collect();
    // bg (1) + 8 border pieces.
    assert_eq!(pieces.len(), 9);
    // First is the bg, tinted the tooltip background color.
    assert_eq!(pieces[0].0, "bg");
    assert_eq!(pieces[0].1, [0.09, 0.09, 0.19, 1.0]);
    // The remaining 8 are the border, white, from the edge file.
    assert!(pieces[1..]
        .iter()
        .all(|(p, c)| p == "edge" && *c == [1.0, 1.0, 1.0, 1.0]));
}

// SetBackdrop(nil) tears the plate down (no pieces after).
#[test]
fn set_backdrop_nil_tears_down() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local f = CreateFrame("Frame", "Plate2")
        f:SetPoint("CENTER")
        f:SetSize(100, 100)
        f:SetBackdrop({ bgFile = "bg", edgeFile = "edge" })
        f:SetBackdrop(nil)
    "#,
    )
    .unwrap();
    s.resolve();
    assert!(s
        .extract()
        .iter()
        .all(|q| !matches!(q.content, QuadContent::Backdrop { .. })));
}
