//! The shipped `assets/ui/ScrollTemplates.xml` faux-scroll kit, under the **reference's own
//! names** — `FauxScrollFrame_Update` / `_GetOffset` / `_SetOffset` / `_OnVerticalScroll` and
//! `ScrollFrame_OnLoad` (decision 1190: a name the shipped 1.12 UI defines is FrameXML's).
//!
//! These drive the kit the way an addon does — a bare instance of `FauxScrollFrameTemplate` with
//! rows of its own — rather than through one of our six owner windows, because the API is now a
//! public surface and its edges (a list that overflows, one that exactly fits, one that shrinks
//! under a scrolled offset, the shrink/widen tail, the return value) belong to it and not to any
//! window. The windows' own tests still cover their wiring.
//!
//! The last test is the one worth reading: it drives the reference's *whole* path — bar drag →
//! `SetVerticalScroll` → the frame's `<OnVerticalScroll>` → `FauxScrollFrame_OnVerticalScroll` —
//! on a real `<ScrollFrame>` with a scroll child, which is exactly the shape 27 corpus addons
//! write. It has to set the scroll child by hand (`SetScrollChild`) because our XML loader has no
//! `<ScrollChild>` element yet; that one missing element is the entire distance between this test
//! and an addon's list scrolling in-game, which is why it is pinned here rather than described.

use benilla_ui::script::UiScript;

/// One shipped file into `s`, asserting it loaded clean.
fn load_xml(s: &UiScript, file: &str) {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/ui")
            .join(file),
    )
    .unwrap();
    let doc = benilla_ui::framexml::parse(&text).unwrap();
    let report = benilla_ui::loader::load(s, &doc, &|_| None);
    assert!(
        report.errors.is_empty(),
        "{file}: loader errors: {:?}",
        report.errors
    );
}

/// A document written here in the test, loaded against the templates already registered — the
/// cross-file template registry is on the `Model`, so an inline doc reaches `ScrollTemplates.xml`'s
/// `FauxScrollFrameTemplate` exactly as `TrainerFrame.xml` does.
fn load_inline(s: &UiScript, xml: &str) {
    let doc = benilla_ui::framexml::parse(xml).unwrap();
    let report = benilla_ui::loader::load(s, &doc, &|_| None);
    assert!(
        report.errors.is_empty(),
        "loader errors: {:?}",
        report.errors
    );
}

/// The kit plus a list an addon might own: five visible rows, a moving highlight, and a faux
/// scroll frame over them.
fn harness() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "ScrollTemplates.xml");
    load_inline(
        &s,
        r#"<Ui>
            <Frame name="TestList">
                <Size><AbsDimension x="300" y="120"/></Size>
                <Anchors><Anchor point="TOPLEFT"/></Anchors>
                <Frames>
                    <Button name="TestRow1"><Size><AbsDimension x="300" y="16"/></Size></Button>
                    <Button name="TestRow2"><Size><AbsDimension x="300" y="16"/></Size></Button>
                    <Button name="TestRow3"><Size><AbsDimension x="300" y="16"/></Size></Button>
                    <Button name="TestRow4"><Size><AbsDimension x="300" y="16"/></Size></Button>
                    <Button name="TestRow5"><Size><AbsDimension x="300" y="16"/></Size></Button>
                    <Frame name="TestHighlight"><Size><AbsDimension x="300" y="16"/></Size></Frame>
                    <Frame name="TestScroll" inherits="FauxScrollFrameTemplate">
                        <Size><AbsDimension x="290" y="80"/></Size>
                        <Anchors><Anchor point="TOPLEFT"/></Anchors>
                    </Frame>
                </Frames>
            </Frame>
        </Ui>"#,
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    s
}

/// `ScrollFrame_OnLoad` ran off the template: a fresh frame is at the top, with an empty range and
/// both arrows greyed. (The reference's own `UIPanelScrollFrameTemplate` OnLoad — the name we now
/// wear instead of a `Benilla*` one.)
#[test]
fn a_fresh_faux_frame_loads_at_the_top_with_no_range() {
    let s = harness();
    assert_eq!(s.eval::<i64>("return TestScroll.offset").unwrap(), 0);
    let (lo, hi) = s
        .eval::<(f64, f64)>("return TestScrollScrollBar:GetMinMaxValues()")
        .unwrap();
    assert_eq!((lo, hi), (0.0, 0.0));
    assert!(
        !s.eval::<bool>("return TestScrollScrollBarScrollUpButton:IsEnabled() ~= 0")
            .unwrap(),
        "up arrow greyed at load (ref UIPanelTemplates.lua l.245-246)"
    );
    assert!(!s
        .eval::<bool>("return TestScrollScrollBarScrollDownButton:IsEnabled() ~= 0")
        .unwrap());
}

/// **More rows than fit**: the bar comes up, its range is the overflow in pixels, its step is one
/// row, and `FauxScrollFrame_Update` returns the reference's `showScrollBar`.
#[test]
fn more_rows_than_fit_raise_the_bar_over_the_overflow_range() {
    let s = harness();
    let shown = s
        .eval::<i64>("return FauxScrollFrame_Update(TestScroll, 12, 5, 16)")
        .unwrap();
    assert_eq!(shown, 1, "the ref returns 1 when the bar is up");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    let (lo, hi) = s
        .eval::<(f64, f64)>("return TestScrollScrollBar:GetMinMaxValues()")
        .unwrap();
    assert_eq!(
        (lo, hi),
        (0.0, 112.0),
        "(12 - 5) rows * 16px — the value model is PIXELS (ScrollTemplates.xml's header)"
    );
    assert_eq!(
        s.eval::<f64>("return TestScrollScrollBar:GetValueStep()")
            .unwrap(),
        16.0
    );
    assert!(s
        .eval::<bool>("return TestScrollScrollBar:IsVisible()")
        .unwrap());
    assert!(
        s.eval::<bool>("return TestScroll:IsVisible()").unwrap(),
        "the frame itself too — the ref's own frame:Show(), which is what an addon reads back"
    );
    assert!(
        !s.eval::<bool>("return TestScrollScrollBarScrollUpButton:IsEnabled() ~= 0")
            .unwrap(),
        "at the top: up greyed"
    );
    assert!(s
        .eval::<bool>("return TestScrollScrollBarScrollDownButton:IsEnabled() ~= 0")
        .unwrap());
}

/// **Exactly fitting** is the boundary the reference draws at `numItems > numToDisplay`: five rows
/// in five slots shows nothing, and there is no off-by-one that leaves a dead bar up.
#[test]
fn a_list_that_exactly_fits_shows_no_bar() {
    let s = harness();
    let shown = s
        .eval::<Option<i64>>("return FauxScrollFrame_Update(TestScroll, 5, 5, 16)")
        .unwrap();
    assert_eq!(shown, None, "the ref returns nil when the bar stays down");
    assert!(!s
        .eval::<bool>("return TestScrollScrollBar:IsVisible()")
        .unwrap());
    assert!(!s.eval::<bool>("return TestScroll:IsVisible()").unwrap());
    let (_, hi) = s
        .eval::<(f64, f64)>("return TestScrollScrollBar:GetMinMaxValues()")
        .unwrap();
    assert_eq!(hi, 0.0, "no overflow, no range");

    // One more row and it appears — the same call, one item along.
    assert_eq!(
        s.eval::<i64>("return FauxScrollFrame_Update(TestScroll, 6, 5, 16)")
            .unwrap(),
        1
    );
    assert!(s
        .eval::<bool>("return TestScrollScrollBar:IsVisible()")
        .unwrap());
}

/// **Fewer rows than fit**, arrived at by the list SHRINKING under a scrolled offset — the case
/// that leaves a window painting rows that are not there if the clamp is missing. The reference
/// clamps by setting the bar to 0; we clamp the offset itself, which is what the owner's repaint
/// reads back through `FauxScrollFrame_GetOffset`.
#[test]
fn a_shrinking_list_clamps_the_offset_back_into_range() {
    let s = harness();
    s.run("FauxScrollFrame_Update(TestScroll, 20, 5, 16)")
        .unwrap();
    s.run("FauxScrollFrame_SetOffset(TestScroll, 12)").unwrap();
    assert_eq!(
        s.eval::<i64>("return FauxScrollFrame_GetOffset(TestScroll)")
            .unwrap(),
        12
    );

    // The list drops to 8 rows: the deepest legal offset is now 3.
    s.run("FauxScrollFrame_Update(TestScroll, 8, 5, 16)")
        .unwrap();
    assert_eq!(
        s.eval::<i64>("return FauxScrollFrame_GetOffset(TestScroll)")
            .unwrap(),
        3,
        "clamped to numItems - numToDisplay, not left at 12"
    );
    assert_eq!(
        s.eval::<f64>("return TestScrollScrollBar:GetValue()")
            .unwrap(),
        48.0,
        "and the thumb followed the clamp"
    );

    // Down to fewer than fit: offset 0, bar gone, and nothing left pointing past the end.
    s.run("FauxScrollFrame_Update(TestScroll, 3, 5, 16)")
        .unwrap();
    assert_eq!(
        s.eval::<i64>("return FauxScrollFrame_GetOffset(TestScroll)")
            .unwrap(),
        0
    );
    assert!(!s
        .eval::<bool>("return TestScrollScrollBar:IsVisible()")
        .unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The reference's **shrink/widen tail** — the six trailing arguments 28 corpus addons pass and
/// benilla's own windows never do. Rows narrow when the bar takes the gutter and widen back when
/// it leaves (ref UIPanelTemplates.lua l.205-223).
#[test]
fn the_shrink_widen_tail_resizes_the_rows_and_the_highlight() {
    let s = harness();
    s.run(
        "FauxScrollFrame_Update(TestScroll, 12, 5, 16, \"TestRow\", 280, 300, TestHighlight, 276, 296)",
    )
    .unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    for i in 1..=5 {
        assert_eq!(
            s.eval::<f64>(&format!("return TestRow{i}:GetWidth()"))
                .unwrap(),
            280.0,
            "row {i} narrowed for the bar"
        );
    }
    assert_eq!(
        s.eval::<f64>("return TestHighlight:GetWidth()").unwrap(),
        276.0
    );

    // The list fits again: every row goes back to the wide measurement.
    s.run(
        "FauxScrollFrame_Update(TestScroll, 4, 5, 16, \"TestRow\", 280, 300, TestHighlight, 276, 296)",
    )
    .unwrap();
    for i in 1..=5 {
        assert_eq!(
            s.eval::<f64>(&format!("return TestRow{i}:GetWidth()"))
                .unwrap(),
            300.0,
            "row {i} widened back"
        );
    }
    assert_eq!(
        s.eval::<f64>("return TestHighlight:GetWidth()").unwrap(),
        296.0
    );
}

/// Dragging the bar moves the offset a whole row at a time and repaints the owner's list — the
/// value model in one test (pixels on the bar, rows in the offset, `floor(v/step + 0.5)` between).
#[test]
fn dragging_the_bar_steps_the_offset_by_rows_and_repaints() {
    let s = harness();
    s.run(
        "TestRepaints = 0 TestScroll.updateFunc = function() TestRepaints = TestRepaints + 1 end",
    )
    .unwrap();
    s.run("FauxScrollFrame_Update(TestScroll, 12, 5, 16)")
        .unwrap();
    let before = s.eval::<i64>("return TestRepaints").unwrap();

    s.run("TestScrollScrollBar:SetValue(48)").unwrap();
    assert_eq!(
        s.eval::<i64>("return FauxScrollFrame_GetOffset(TestScroll)")
            .unwrap(),
        3
    );
    assert_eq!(
        s.eval::<i64>("return TestRepaints").unwrap(),
        before + 1,
        "exactly one repaint per row the drag crossed"
    );

    // A sub-row nudge rounds to the nearest row and, landing on the same one, repaints nothing.
    s.run("TestScrollScrollBar:SetValue(51)").unwrap();
    assert_eq!(
        s.eval::<i64>("return FauxScrollFrame_GetOffset(TestScroll)")
            .unwrap(),
        3
    );
    assert_eq!(s.eval::<i64>("return TestRepaints").unwrap(), before + 1);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **The addon path, end to end** — the reference's wiring rather than ours: a real `<ScrollFrame>`
/// on `FauxScrollFrameTemplate` whose `<OnVerticalScroll>` calls `FauxScrollFrame_OnVerticalScroll`,
/// driven by a drag of the shared bar.
///
/// The `SetScrollChild` line is the gap, made visible: the reference's template declares that child
/// as `<ScrollChild><Frame name="$parentScrollChildFrame">`, our XML loader has no `<ScrollChild>`
/// element, and `SetVerticalScroll` clamps into `[0, GetVerticalScrollRange()]` — which is computed
/// from that child. Without it the range is 0, the handler fires with `arg1 = 0` and an addon's list
/// never leaves the top. With it, the whole reference path runs.
#[test]
fn the_reference_on_vertical_scroll_path_runs_once_a_scroll_child_exists() {
    let mut s = harness();
    load_inline(
        &s,
        r#"<Ui>
            <ScrollFrame name="AddonScroll" inherits="FauxScrollFrameTemplate">
                <Size><AbsDimension x="290" y="80"/></Size>
                <Anchors><Anchor point="TOPLEFT"/></Anchors>
                <Scripts>
                    <OnVerticalScroll>FauxScrollFrame_OnVerticalScroll(16, AddonScroll_Update)</OnVerticalScroll>
                </Scripts>
            </ScrollFrame>
            <Frame name="AddonScrollChildFrame">
                <Size><AbsDimension x="290" y="192"/></Size>
            </Frame>
        </Ui>"#,
    );
    s.run("AddonRepaints = 0 function AddonScroll_Update() AddonRepaints = AddonRepaints + 1 end")
        .unwrap();

    // The instance's own tag wins over the template's on inherit, so this really is a ScrollFrame.
    assert!(
        s.eval::<bool>("return type(AddonScroll.SetVerticalScroll) == 'function'")
            .unwrap(),
        "<ScrollFrame inherits=\"FauxScrollFrameTemplate\"> is a ScrollFrame"
    );

    s.run("AddonScroll:SetScrollChild(AddonScrollChildFrame)")
        .unwrap();
    s.resolve();
    s.run("AddonScroll:UpdateScrollChildRect()").unwrap();
    assert_eq!(
        s.eval::<f64>("return AddonScroll:GetVerticalScrollRange()")
            .unwrap(),
        112.0,
        "192px of rows in an 80px window — the same overflow the bar's range carries"
    );

    s.run("FauxScrollFrame_Update(AddonScroll, 12, 5, 16)")
        .unwrap();
    s.run("AddonScrollScrollBar:SetValue(48)").unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert_eq!(
        s.eval::<i64>("return FauxScrollFrame_GetOffset(AddonScroll)")
            .unwrap(),
        3,
        "the bar's value reached the frame's OnVerticalScroll and became a row offset"
    );
    assert!(
        s.eval::<i64>("return AddonRepaints").unwrap() >= 1,
        "and the addon's own update function ran"
    );
}

/// **The ScrollingEdit trio answers to the REFERENCE's names, and to a bare call.**
///
/// `InviteOMatic` wires `ScrollingEdit_OnTextChanged` straight into an EditBox's `OnTextChanged`
/// and raised `attempt to call global` the moment anyone typed — found by the use-probe, since it
/// only fires on input. Both helpers take an OPTIONAL scroll frame and fall back to
/// `this:GetParent()` (ref `UIPanelTemplates.lua` l.307-310); an addon wiring the bare name depends
/// on that fallback, so the no-argument path is what this test drives.
#[test]
fn scrolling_edit_helpers_answer_bare_calls_from_a_handler() {
    let s = UiScript::new().unwrap();
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "ScrollTemplates.xml");
    load_inline(
        &s,
        r#"<Ui>
            <ScrollFrame name="SeScroll">
                <Size><AbsDimension x="200" y="100"/></Size>
                <Anchors><Anchor point="CENTER"/></Anchors>
                <ScrollChild>
                    <EditBox name="SeEdit" multiLine="true">
                        <Size><AbsDimension x="200" y="300"/></Size>
                    </EditBox>
                </ScrollChild>
            </ScrollFrame>
        </Ui>"#,
    );

    // Bare call with `this` set to the edit box — the addon's exact wiring.
    s.run(
        "this = SeEdit; ScrollingEdit_OnTextChanged(); ScrollingEdit_OnCursorChanged(0, 42, 0, 14); this = nil",
    )
    .unwrap();
    assert!(
        s.errors().is_empty(),
        "bare calls must not raise: {:?}",
        s.errors()
    );

    // OnCursorChanged records where the caret is, which is what an OnUpdate would follow.
    assert_eq!(s.eval::<f32>("return SeEdit.cursorOffset").unwrap(), 42.0);
    assert_eq!(s.eval::<f32>("return SeEdit.cursorHeight").unwrap(), 14.0);

    // ...and the explicit-frame form works too (the reference's first branch).
    s.run("ScrollingEdit_OnTextChanged(SeScroll)").unwrap();
    assert!(s.errors().is_empty(), "explicit form: {:?}", s.errors());
}
