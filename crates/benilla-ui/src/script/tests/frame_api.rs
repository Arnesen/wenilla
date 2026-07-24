//! Small frame-API wins: GetLocale, SetAllPoints, SetFormattedText, SetShown, and GetPoint readback.

use super::common::script;

#[test]
fn consensus_api_small_wins() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    // GetLocale — benilla is enUS-data-only.
    assert_eq!(s.eval::<String>("return GetLocale()").unwrap(), "enUS");

    s.run(
        r#"
        p = CreateFrame("Frame", "SAP_Parent")
        p:SetPoint("BOTTOMLEFT", 100, 100); p:SetSize(200, 50)
        c = CreateFrame("Frame", "SAP_Child", p)
        c:SetAllPoints()                          -- default: the parent
        f = CreateFrame("Frame", "SAP_Free")
        f:SetAllPoints("SAP_Parent")              -- by name
        fs = p:CreateFontString(nil, "OVERLAY")
        fs:SetFormattedText("%2$s %1$s (%3$d)", "world", "hello", 7)
    "#,
    )
    .unwrap();
    s.resolve();

    // SetAllPoints pins the full target rect, parent-default and by-name forms alike.
    let ok: bool = s
        .eval(
            r#"
        return c:GetWidth() == 200 and c:GetHeight() == 50
           and f:GetWidth() == 200 and f:GetHeight() == 50
    "#,
        )
        .unwrap();
    assert!(ok, "SetAllPoints matched the target rect");

    // SetFormattedText routes through the positional-aware format.
    assert_eq!(
        s.eval::<String>("return fs:GetText()").unwrap(),
        "hello world (7)"
    );

    // SetShown: truthy shows, nil/false hides.
    s.run("p:SetShown(false)").unwrap();
    assert!(!s.eval::<bool>("return p:IsShown()").unwrap());
    s.run("p:SetShown(1)").unwrap();
    assert!(s.eval::<bool>("return p:IsShown()").unwrap());
    s.run("p:SetShown(nil)").unwrap();
    assert!(!s.eval::<bool>("return p:IsShown()").unwrap());
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn getpoint_reads_back_anchors() {
    let s = script();
    s.run(
        r#"
        a = CreateFrame("Frame", "GP_A")
        a:SetPoint("BOTTOMLEFT", 100, 100)
        b = CreateFrame("Frame", "GP_B")
        b:SetPoint("TOPLEFT", a, "BOTTOMRIGHT", 4, -8)
    "#,
    )
    .unwrap();
    // Screen-anchored: relativeTo is nil (no UIParent wrapper yet — stated).
    let ok: bool = s
        .eval(
            r#"
        local p, rel, rp, x, y = GP_A:GetPoint()
        return p == "BOTTOMLEFT" and rel == nil and rp == "BOTTOMLEFT" and x == 100 and y == 100
    "#,
        )
        .unwrap();
    assert!(ok, "screen anchor reads back");
    // Frame-anchored: relativeTo is the same wrapper table (stable identity).
    let ok: bool = s
        .eval(
            r#"
        local p, rel, rp, x, y = GP_B:GetPoint(1)
        return p == "TOPLEFT" and rel == a and rp == "BOTTOMRIGHT" and x == 4 and y == -8
    "#,
        )
        .unwrap();
    assert!(ok, "frame anchor reads back with wrapper identity");
    // Out-of-range n: all nils.
    assert!(s.eval::<bool>("return GP_B:GetPoint(9) == nil").unwrap());
}
