//! ScrollFrame (decision 0112 — the ScrollFrame mechanism): the scroll-child layout override, the
//! live vertical-scroll range, the ScrollFrame clip on `extract`, and the clip-gated `hit_test`.

use super::common::script;
use crate::layout::Rect;
use crate::script::QuadContent;

/// The rect of the first Texture quad whose path equals `marker` (a bare texture with no anchors of
/// its own draws at exactly its owner frame's resolved rect — the cheapest way to read a frame's
/// resolved rect back out through the public `extract` surface).
fn marker_rect(quads: &[crate::script::ExtractedQuad], marker: &str) -> Option<Rect> {
    quads.iter().find_map(|q| match &q.content {
        QuadContent::Texture { path: Some(p), .. } if p == marker => q.rect,
        _ => None,
    })
}

/// `Some(clip)` when a marker quad was found (`clip` itself `Option<Rect>`); `None` if no such
/// marker quad exists at all (a test-authoring mistake, not a legitimate "unclipped").
fn marker_clip(quads: &[crate::script::ExtractedQuad], marker: &str) -> Option<Option<Rect>> {
    quads.iter().find_map(|q| match &q.content {
        QuadContent::Texture { path: Some(p), .. } if p == marker => Some(q.clip),
        _ => None,
    })
}

#[test]
fn scrollframe_methods_exist_only_on_scrollframes() {
    let s = script();
    let ok: bool = s
        .eval(
            r#"
        local sf = CreateFrame("ScrollFrame", "SfDuck")
        local plain = CreateFrame("Frame")
        -- Duck-typing: addons branch on `if frame.SetScrollChild then` — a plain frame says nil.
        return (type(sf.SetScrollChild) == "function") and (plain.SetScrollChild == nil)
            and (type(sf.SetVerticalScroll) == "function") and (plain.SetVerticalScroll == nil)
            and (type(sf.GetVerticalScrollRange) == "function")
            and (type(sf.Show) == "function") -- base methods still reachable through the fallback
    "#,
        )
        .unwrap();
    assert!(ok);
}

#[test]
fn get_scroll_child_roundtrips_wrapper_and_name_and_clears_on_nil() {
    let s = script();
    s.run(
        r#"
        local sf = CreateFrame("ScrollFrame", "SF")
        assert(sf:GetScrollChild() == nil, "no child yet")

        local child = CreateFrame("Frame", "Child")
        sf:SetScrollChild(child)
        assert(sf:GetScrollChild() == child, "stable wrapper identity round-trips")

        sf:SetScrollChild(nil)
        assert(sf:GetScrollChild() == nil, "nil clears")

        -- A name string resolves like every other frame-target arg (SetParent, SetPoint's relativeTo).
        local byName = CreateFrame("Frame", "ByName")
        sf:SetScrollChild("ByName")
        assert(sf:GetScrollChild() == byName)
    "#,
    )
    .unwrap();
}

/// The sign convention (verified against the design's own worked example): frame top 500, vertical
/// 40 ⇒ child top 540. Also covers the 0 / clamped-to-range cases and that `SetScrollChild(nil)`
/// restores the child's own authored anchor (never mutated — the override is a local map).
#[test]
fn scroll_child_top_tracks_vertical_scroll_and_clamps_then_restores_on_clear() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0); // screen rect: bottom 0, left 0, top 600, right 800

    s.run(
        r#"
        local frame = CreateFrame("ScrollFrame", "SF")
        frame:SetPoint("TOPLEFT", 0, -100)  -- screen top 600 -> frame top 500
        frame:SetSize(300, 200)             -- frame bottom 300

        local child = CreateFrame("Frame", "Child")
        -- Authored anchor: relative to the screen (child has no parent) — overridden while scrolled,
        -- and must be exactly what re-applies once the scroll child is cleared.
        child:SetPoint("TOPLEFT", 20, -20)
        child:SetSize(300, 600)
        local marker = child:CreateTexture(nil, "ARTWORK")
        marker:SetTexture("marker:child")
        marker:SetAllPoints()  -- templateless Lua regions carry no implicit anchor (decision 1310)

        frame:SetScrollChild(child)
    "#,
    )
    .unwrap();
    s.resolve();

    let quads = s.extract();
    assert_eq!(
        marker_rect(&quads, "marker:child").map(|r| r.top),
        Some(500.0),
        "vertical=0 -> child top = frame top"
    );

    s.run("SF:SetVerticalScroll(40)").unwrap();
    s.resolve();
    let quads = s.extract();
    assert_eq!(
        marker_rect(&quads, "marker:child").map(|r| r.top),
        Some(540.0),
        "vertical=40 -> child top = frame top + 40 (a positive offset lifts the child)"
    );

    // range = child_h(600) - frame_h(200) = 400; asking for 9999 clamps to it.
    s.run("SF:SetVerticalScroll(9999)").unwrap();
    s.resolve();
    assert_eq!(
        s.eval::<f32>("return SF:GetVerticalScroll()").unwrap(),
        400.0
    );
    let quads = s.extract();
    assert_eq!(
        marker_rect(&quads, "marker:child").map(|r| r.top),
        Some(900.0),
        "clamped: frame top 500 + range 400"
    );

    // SetScrollChild(nil): the override stops being computed — the child's own authored anchor
    // (TOPLEFT, screen, +20,-20) re-applies with no restore step needed (it was never touched).
    s.run("SF:SetScrollChild(nil)").unwrap();
    s.resolve();
    let quads = s.extract();
    assert_eq!(
        marker_rect(&quads, "marker:child").map(|r| r.top),
        Some(580.0),
        "authored anchor survives: screen top 600 - 20"
    );
}

#[test]
fn vertical_scroll_range_is_live_and_zero_when_unresolved_or_childless_or_shorter() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local frame = CreateFrame("ScrollFrame", "SF")
        frame:SetPoint("TOPLEFT", 0, -100)
        frame:SetSize(300, 200)
        local child = CreateFrame("Frame", "Child")
        child:SetSize(300, 600)
        frame:SetScrollChild(child)
    "#,
    )
    .unwrap();
    // Before resolve: no resolved rects yet -> 0.
    assert_eq!(
        s.eval::<f32>("return SF:GetVerticalScrollRange()").unwrap(),
        0.0,
        "unresolved -> 0"
    );

    s.resolve();
    assert_eq!(
        s.eval::<f32>("return SF:GetVerticalScrollRange()").unwrap(),
        400.0,
        "600 - 200"
    );

    // A child shorter than the frame: 0, not negative.
    s.run("Child:SetSize(300, 150)").unwrap();
    s.resolve();
    assert_eq!(
        s.eval::<f32>("return SF:GetVerticalScrollRange()").unwrap(),
        0.0,
        "child shorter than frame -> 0, not negative"
    );

    // No child at all: 0.
    s.run("SF:SetScrollChild(nil)").unwrap();
    assert_eq!(
        s.eval::<f32>("return SF:GetVerticalScrollRange()").unwrap(),
        0.0,
        "no child -> 0"
    );
}

/// The range is in LOCAL units, not screen px — `child:GetHeight() - self:GetHeight()`, the
/// contract the method states. It has to be: `SetVerticalScroll` becomes the child's anchor
/// y-offset, which the solver multiplies by the child's scale, so a screen-px range against a
/// local-unit offset makes a SCALED frame stop short of its own end and strand the tail of its
/// content. (Found on the era-scaled options window's page scroll, which lost 22% of its travel.)
#[test]
fn vertical_scroll_range_is_local_units_on_a_scaled_frame() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local frame = CreateFrame("ScrollFrame", "SF")
        frame:SetPoint("TOPLEFT", 0, -100)
        frame:SetSize(300, 200)
        frame:SetScale(0.5)
        local child = CreateFrame("Frame", "Child", frame)
        child:SetSize(300, 600)
        frame:SetScrollChild(child)
    "#,
    )
    .unwrap();
    s.resolve();
    assert_eq!(
        s.eval::<f32>("return SF:GetVerticalScrollRange()").unwrap(),
        400.0,
        "600 - 200 in the frame's own units, NOT (600 - 200) * 0.5"
    );
    assert_eq!(
        s.eval::<f32>("return Child:GetHeight() - SF:GetHeight()")
            .unwrap(),
        400.0,
        "…which is exactly what the widget contract says it is"
    );
    // And the clamp agrees, so scrolling to the range really reaches the end: the child's bottom
    // lands on the frame's.
    s.run("SF:SetVerticalScroll(400)").unwrap();
    s.resolve();
    assert_eq!(
        s.eval::<f32>("return SF:GetVerticalScroll()").unwrap(),
        400.0,
        "the full range is reachable, not clamped away"
    );
    assert_eq!(
        s.eval::<f32>("return Child:GetBottom() - SF:GetBottom()")
            .unwrap(),
        0.0,
        "at the end, the child's bottom sits on the frame's"
    );
}

#[test]
fn vertical_scroll_fires_clamped_and_update_rect_fires_range_changed() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local frame = CreateFrame("ScrollFrame", "SF")
        frame:SetPoint("TOPLEFT", 0, -100)
        frame:SetSize(300, 200)
        local child = CreateFrame("Frame", "Child")
        child:SetSize(300, 600)
        frame:SetScrollChild(child)

        seen_v = nil
        frame:SetScript("OnVerticalScroll", function(self, offset)
            seen_v = offset
            assert(self == frame and arg1 == offset, "RF-0025 conventions carry the value")
        end)
        seen_lo, seen_hi = nil, nil
        frame:SetScript("OnScrollRangeChanged", function(self, lo, hi)
            seen_lo, seen_hi = lo, hi
        end)
    "#,
    )
    .unwrap();
    s.resolve();

    s.run("SF:SetVerticalScroll(9999)").unwrap();
    assert_eq!(
        s.eval::<f32>("return seen_v").unwrap(),
        400.0,
        "fires with the CLAMPED value"
    );

    s.run("SF:UpdateScrollChildRect()").unwrap();
    let (lo, hi): (f32, f32) = s.eval("return seen_lo, seen_hi").unwrap();
    assert_eq!((lo, hi), (0.0, 400.0));
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// The Era idiom `UpdateScrollChildRect` exists to serve — resize the content, then immediately ask
/// for the new range, in ONE script tick with no intervening [`UiScript::resolve`] call in between
/// (exactly how the app's own drive loop runs Lua: `ui_script/mod.rs` resolves once per FRAME, not
/// once per script statement) — must see the JUST-set size, not the last frame's stale `resolved`
/// rect. Before `UpdateScrollChildRect` forced its own fresh resolve, this fired the OLD (here,
/// zero) range on the very call whose whole job is reporting the new one.
#[test]
fn update_scroll_child_rect_sees_a_same_tick_resize_with_no_intervening_resolve() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local frame = CreateFrame("ScrollFrame", "SF")
        frame:SetPoint("TOPLEFT", 0, -100)
        frame:SetSize(300, 200)
        local child = CreateFrame("Frame", "Child")
        child:SetSize(300, 200) -- starts exactly frame-height: range 0
        frame:SetScrollChild(child)
    "#,
    )
    .unwrap();
    s.resolve(); // the "last frame" resolve, before this tick's resize

    s.run(
        r#"
        seen_hi = nil
        SF:SetScript("OnScrollRangeChanged", function(self, lo, hi) seen_hi = hi end)
        Child:SetSize(300, 600) -- grows well past the frame — all in this ONE tick
        SF:UpdateScrollChildRect()
    "#,
    )
    .unwrap();
    assert!(s.errors().is_empty(), "{:?}", s.errors());
    assert_eq!(
        s.eval::<f32>("return seen_hi").unwrap(),
        400.0,
        "UpdateScrollChildRect must resolve the JUST-set height itself, not report the stale \
         (pre-resize) range from the last full UiScript::resolve"
    );
    // GetVerticalScrollRange() also benefits (same fresh `resolved`), and a same-tick
    // SetVerticalScroll now clamps against the true, current range instead of the stale one.
    assert_eq!(
        s.eval::<f32>("return SF:GetVerticalScrollRange()").unwrap(),
        400.0
    );
}

/// The clip on `extract`: the scroll child's own quads, and a grandchild region nested arbitrarily
/// deep under it, carry `clip = Some(the scrollframe's resolved rect)`; a sibling outside the
/// ScrollFrame entirely carries `None`.
#[test]
fn extract_clips_the_scroll_childs_whole_subtree_and_leaves_a_sibling_unclipped() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local frame = CreateFrame("ScrollFrame", "SF")
        frame:SetPoint("TOPLEFT", 0, -100)  -- top 500
        frame:SetSize(300, 200)             -- bottom 300, right 300

        local child = CreateFrame("Frame", "Child")
        child:SetSize(300, 600)
        frame:SetScrollChild(child)
        local cm = child:CreateTexture(nil, "ARTWORK")
        cm:SetTexture("marker:child")

        -- a grandchild FRAME (a real arena descendant of the scroll child) with its own region
        local grand = CreateFrame("Frame", "Grand", child)
        grand:SetPoint("TOPLEFT", child, "TOPLEFT", 0, 0)
        grand:SetSize(50, 50)
        local gm = grand:CreateTexture(nil, "ARTWORK")
        gm:SetTexture("marker:grand")

        -- a sibling entirely outside the ScrollFrame
        local sib = CreateFrame("Frame", "Sib")
        sib:SetPoint("TOPLEFT", 0, 0)
        sib:SetSize(50, 50)
        local sm = sib:CreateTexture(nil, "ARTWORK")
        sm:SetTexture("marker:sib")
    "#,
    )
    .unwrap();
    s.resolve();
    let quads = s.extract();

    let sf_rect = Rect::new(300.0, 0.0, 500.0, 300.0);
    assert_eq!(marker_clip(&quads, "marker:child"), Some(Some(sf_rect)));
    assert_eq!(
        marker_clip(&quads, "marker:grand"),
        Some(Some(sf_rect)),
        "a descendant of the scroll child clips too, not just the child itself"
    );
    assert_eq!(
        marker_clip(&quads, "marker:sib"),
        Some(None),
        "a sibling outside the ScrollFrame is unclipped"
    );
}

/// Nested ScrollFrames intersect their rects (decision 0112 §4): an inner ScrollFrame living inside
/// an outer one's scroll child clips its own content to the intersection of both rects, not just
/// the innermost.
#[test]
fn nested_scrollframes_intersect_their_clip_rects() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local outer = CreateFrame("ScrollFrame", "Outer")
        outer:SetPoint("TOPLEFT", 0, -50)   -- top 550
        outer:SetSize(400, 300)             -- bottom 250, right 400
        local om = outer:CreateTexture(nil, "ARTWORK"); om:SetTexture("marker:outer")

        local c1 = CreateFrame("Frame", "C1")
        c1:SetSize(400, 1000)
        outer:SetScrollChild(c1)
        local c1m = c1:CreateTexture(nil, "ARTWORK"); c1m:SetTexture("marker:c1")

        -- The inner ScrollFrame is a real arena child of C1 (so walking its ancestors reaches C1,
        -- Outer's registered scroll child) and sits far enough right that its own rect pokes past
        -- Outer's right edge — an intersection distinct from either rect alone.
        local inner = CreateFrame("ScrollFrame", "Inner", c1)
        inner:SetPoint("TOPLEFT", "C1", "TOPLEFT", 350, -30)
        inner:SetSize(200, 150)
        local im = inner:CreateTexture(nil, "ARTWORK"); im:SetTexture("marker:inner")

        local c2 = CreateFrame("Frame", "C2", inner)
        c2:SetSize(200, 500)
        inner:SetScrollChild(c2)
        local leaf = c2:CreateTexture(nil, "ARTWORK"); leaf:SetTexture("marker:leaf")
    "#,
    )
    .unwrap();
    s.resolve();
    let quads = s.extract();

    let outer_rect = Rect::new(250.0, 0.0, 550.0, 400.0);
    // Inner's OWN rect (unclipped by itself): TOPLEFT anchored to C1 (top 550) at (350,-30) -> top
    // 520, left 350; size 200x150 -> bottom 370, right 550.
    let inner_rect = Rect::new(370.0, 350.0, 520.0, 550.0);
    let intersection = Rect::new(
        inner_rect.bottom.max(outer_rect.bottom),
        inner_rect.left.max(outer_rect.left),
        inner_rect.top.min(outer_rect.top),
        inner_rect.right.min(outer_rect.right),
    );
    // A genuine intersection, not just "pick the smaller rect": inner's right edge (550) is cut
    // back to outer's (400), while inner's tighter bottom/left/top edges win.
    assert_eq!(intersection, Rect::new(370.0, 350.0, 520.0, 400.0));

    assert_eq!(
        marker_clip(&quads, "marker:outer"),
        Some(None),
        "Outer itself is not clipped"
    );
    assert_eq!(
        marker_clip(&quads, "marker:c1"),
        Some(Some(outer_rect)),
        "Outer's own registered child clips to Outer's rect"
    );
    assert_eq!(
        marker_clip(&quads, "marker:inner"),
        Some(Some(outer_rect)),
        "Inner (the widget itself) lives inside C1's clipped content -> clipped by Outer too"
    );
    assert_eq!(
        marker_clip(&quads, "marker:leaf"),
        Some(Some(intersection)),
        "Inner's own scroll child intersects both ScrollFrames' rects"
    );
}

/// Hit-testing (decision 0112 §5): a button inside the scroll child, scrolled out of the frame's
/// rect, must not hit even though its own resolved rect contains the cursor; scrolled into view, it
/// hits normally.
#[test]
fn hit_test_denies_a_button_clipped_out_and_admits_it_once_scrolled_into_view() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local frame = CreateFrame("ScrollFrame", "SF")
        frame:SetPoint("TOPLEFT", 0, -100)  -- top 500
        frame:SetSize(300, 200)             -- bottom 300
        frame:EnableMouse(false)            -- isolate the button's own clip-gated hit (§5's subject)

        local child = CreateFrame("Frame", "Child")
        child:SetSize(300, 600)
        frame:SetScrollChild(child)

        local btn = CreateFrame("Button", "Btn", child)
        btn:SetPoint("TOPLEFT", "Child", "TOPLEFT", 0, -400)
        btn:SetSize(50, 20)
    "#,
    )
    .unwrap();
    s.resolve();

    // vertical=0: child top 500, button top 500-400=100, bottom 80 — its OWN rect contains (25,90),
    // but that's well outside the frame's clip [300,500].
    assert_eq!(
        s.hit_test(25.0, 90.0),
        None,
        "the button's own rect contains the point, but the ScrollFrame's clip excludes it"
    );

    // Scroll 250px: button top 500+250-400=350, bottom 330 — now inside both its own rect AND the
    // frame's clip window.
    s.run("SF:SetVerticalScroll(250)").unwrap();
    s.resolve();
    assert!(
        s.hit_test(25.0, 340.0).is_some(),
        "scrolled into view, the button hits again"
    );
}
