//! The shipped `assets/ui/ActionBar.xml`'s performance ("ping") meter — the tinted bar in the
//! main bar's last empty recess — over the real files, never a stub.
//!
//! What these guard, in order: the ref geometry that puts the bar IN the recess (the whole point of
//! the slice); the LOW-strata draw order that makes it show *through* the bar art instead of over
//! it; the latency→color law and its 10 s poll; and the hover tooltip's live number.

use benilla_ui::script::{QuadContent, UiScript};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error.
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

/// The bar on a 1024×768 screen. 1024 wide is deliberate: the 1024-wide bar then spans x 0..1024,
/// so every ref offset below is also an absolute screen coordinate.
fn harness(extra: &[&str]) -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "ActionBar.xml");
    for f in extra {
        load_xml(&s, f);
    }
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    s
}

/// The meter's own paint quad (its `MainMenuBarPerformanceBar` texture) and where it sits in the
/// painter's order.
fn bar_quad(s: &mut UiScript) -> (usize, [f32; 4], Option<[f32; 4]>) {
    s.resolve();
    let quads = s.extract();
    let i = quads
        .iter()
        .position(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                if p.contains("UI-MainMenuBar-PerformanceBar"))
        })
        .expect("the performance bar's texture quad");
    let rect = quads[i].rect.expect("a resolved rect");
    let color = match &quads[i].content {
        QuadContent::Texture { color, .. } => *color,
        _ => unreachable!(),
    };
    (i, [rect.left, rect.bottom, rect.right, rect.top], color)
}

/// ref-MainMenuBar.xml l.344-364: a 16×64 frame at the bar's BOTTOMRIGHT +(-227,-10), carrying a
/// 20×66 texture off its TOPRIGHT. On a 1024-wide bar that is x 781..797 for the frame and
/// x 777..797 for the texture — the empty recess between the last micro button (which ends at 763)
/// and the bag cluster. Both hang below the bar's own bottom, which is what drops the texture's
/// grey column into the slot rather than floating it above.
#[test]
fn the_meter_sits_in_the_bar_recess_the_reference_leaves_for_it() {
    let mut s = harness(&[]);
    s.resolve();

    let (left, bottom, w, h) = s
        .eval::<(f64, f64, f64, f64)>(
            "return MainMenuBarPerformanceBarFrame:GetLeft(), MainMenuBarPerformanceBarFrame:GetBottom(), \
             MainMenuBarPerformanceBarFrame:GetWidth(), MainMenuBarPerformanceBarFrame:GetHeight()",
        )
        .unwrap();
    assert_eq!((w, h), (16.0, 64.0), "frame size");
    assert_eq!(left, 781.0, "1024 − 227 − 16");
    assert_eq!(bottom, -10.0, "hangs 10 below the bar's bottom");

    // The texture overspills the frame: 20 wide off the TOPRIGHT ⇒ 4 further left, and 66 tall from
    // the frame's top (54) ⇒ 2 further down.
    let (_, rect, _) = bar_quad(&mut s);
    assert_eq!(
        rect,
        [777.0, -12.0, 797.0, 54.0],
        "[left, bottom, right, top]"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// `frameStrata="LOW"` is the mechanism, not decoration: the bar art is a transparent WINDOW over
/// this slot, so the meter has to paint UNDERNEATH it. At the default MEDIUM the 20-wide texture
/// would paint over the metal surround that frames the 10-wide slot.
#[test]
fn the_meter_paints_under_the_bar_art_it_shows_through() {
    let mut s = harness(&[]);
    let (meter, _, _) = bar_quad(&mut s);

    let quads = s.extract();
    let art = quads
        .iter()
        .position(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                if p.contains("UI-MainMenuBar-Dwarf"))
        })
        .expect("the bar's own dwarf art");
    assert!(
        meter < art,
        "the meter must draw before (under) the bar art — it is seen through the art's \
         transparent slot, not over it"
    );

    // And the hover button is above everything, the ref's own HIGH-strata split: the LOW frame
    // beneath the art can't take the mouse itself.
    assert_eq!(
        s.eval::<String>("return MainMenuBarPerformanceBarFrameButton:GetFrameStrata()")
            .unwrap(),
        "HIGH"
    );
    assert_eq!(
        s.eval::<String>("return MainMenuBarPerformanceBarFrame:GetFrameStrata()")
            .unwrap(),
        "LOW",
        "the parent's LOW must not have been dragged up by its HIGH child"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// ref-MainMenuBar.xml l.375-392: the poll runs every `PERFORMANCEBAR_UPDATE_INTERVAL` seconds and
/// tints the bar green under 300 ms, yellow to 600, red beyond. 1.12 never scales the bar's HEIGHT
/// — color is the entire readout, which is why this test asserts the tint and the rect separately.
#[test]
fn the_meter_tints_by_latency_on_the_reference_thresholds() {
    let mut s = harness(&[]);

    // The first tick polls immediately (updateInterval starts at 0), so an unmeasured connection
    // reads 0 ms and shows green rather than sitting untinted grey.
    s.tick(0.016);
    let (_, _, color) = bar_quad(&mut s);
    assert_eq!(color, Some([0.0, 1.0, 0.0, 1.0]), "0 ms ⇒ green");

    for (latency, want, band) in [
        (299, [0.0, 1.0, 0.0, 1.0], "under LOW ⇒ green"),
        (301, [1.0, 1.0, 0.0, 1.0], "past LOW ⇒ yellow"),
        (601, [1.0, 0.0, 0.0, 1.0], "past MEDIUM ⇒ red"),
    ] {
        s.set_latency_ms(Some(latency));
        poll_beat(&mut s);
        let (_, _, color) = bar_quad(&mut s);
        assert_eq!(color, Some(want), "{latency} ms: {band}");
    }

    // A latency that crosses a threshold does NOT repaint before the next poll beat — the ref's
    // own 10 s cadence, and the reason a 30 s ping's jitter can't strobe the bar.
    s.set_latency_ms(Some(0));
    s.tick(1.0);
    let (_, _, color) = bar_quad(&mut s);
    assert_eq!(
        color,
        Some([1.0, 0.0, 0.0, 1.0]),
        "still red until the beat"
    );
    poll_beat(&mut s);
    let (_, _, color) = bar_quad(&mut s);
    assert_eq!(
        color,
        Some([0.0, 1.0, 0.0, 1.0]),
        "the beat repaints it green"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The drawn colour of the tooltip's explanation line — found by its TEXT, so it can't be confused
/// with the label above it. `None` if that line isn't being painted at all.
fn newbie_line_color(s: &mut UiScript) -> Option<[f32; 4]> {
    let want = s
        .eval::<String>("return NEWBIE_TOOLTIP_LATENCY")
        .expect("the string is declared with the meter");
    s.resolve();
    s.extract().iter().find_map(|q| match &q.content {
        QuadContent::Text {
            text: Some(t),
            color,
            ..
        } if *t == want => Some(*color),
        _ => None,
    })?
}

/// Advance exactly one poll beat. The ref's OnUpdate spends the whole remaining interval on one
/// tick and only polls on the tick that finds it non-positive, so a beat is always two ticks longer
/// than the interval: the first drains it, the second crosses zero and re-arms it at 10.
fn poll_beat(s: &mut UiScript) {
    s.tick(11.0);
    s.tick(11.0);
}

/// `GetNetStats()` is the whole seam: the app pushes the averaged RTT, the meter reads it. The two
/// bandwidth returns are the named gap (benilla tallies no throughput) and must stay NUMBERS, so
/// arithmetic on them can't error.
#[test]
fn get_net_stats_reports_the_pushed_latency() {
    let mut s = harness(&[]);
    assert_eq!(
        s.eval::<(f64, f64, f64)>("return GetNetStats()").unwrap(),
        (0.0, 0.0, 0.0),
        "nothing measured yet"
    );
    s.set_latency_ms(Some(42));
    assert_eq!(
        s.eval::<(f64, f64, f64)>("return GetNetStats()").unwrap(),
        (0.0, 0.0, 42.0)
    );
    // Back to unmeasured (a disconnect clears the app's ring) ⇒ 0, the reference's own reading for
    // a connection it has no sample for.
    s.set_latency_ms(None);
    assert_eq!(
        s.eval::<f64>("local _, _, ms = GetNetStats() return ms")
            .unwrap(),
        0.0
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The hover tooltip (ref l.395-406): the HIGH-strata button owns the mouse over the meter, and its
/// tooltip reads the live number. The held-open plate re-reads it on the same 10 s beat.
///
/// The plate is the ref's TWO-line `GameTooltip_AddNewbieTip` — detailed tips ship ON in 1.12
/// (`SHOW_NEWBIE_TIPS = "1"`, ref UIOptionsFrame.lua l.100), so the explanation under the number is
/// the DEFAULT hover, not an opt-in. Decision 0661; this test is the guard on that, because a
/// regression here (the helper's branch flipping, the string going missing) shows up only as a
/// tooltip that is quietly one line short.
#[test]
fn hovering_the_meter_shows_the_live_latency() {
    let mut s = harness(&["UIParent.xml", "GameTooltip.xml"]);
    s.set_latency_ms(Some(42));
    s.resolve();

    assert_eq!(
        s.hit_test_name(789.0, 20.0).as_deref(),
        Some("MainMenuBarPerformanceBarFrameButton"),
        "the button over the meter takes the mouse"
    );

    s.run("BenillaPerformanceBar_OnEnter(MainMenuBarPerformanceBarFrameButton)")
        .unwrap();
    assert_eq!(
        s.eval::<String>("return GameTooltipTextLeft1:GetText()")
            .unwrap(),
        "Latency: 42ms"
    );
    assert_eq!(
        s.eval::<i64>("return GameTooltip:NumLines()").unwrap(),
        2,
        "the number, then the explanation — the ref's newbie branch"
    );
    assert_eq!(
        s.eval::<String>("return GameTooltipTextLeft2:GetText()")
            .unwrap(),
        s.eval::<String>("return NEWBIE_TOOLTIP_LATENCY").unwrap(),
        "line 2 is the ref's own NEWBIE_TOOLTIP_LATENCY, verbatim"
    );
    // The newbie branch seats the plate at the default screen corner, NOT beside the meter — the
    // `default` flag GameTooltip_SetDefaultAnchor stamps is the observable half of that.
    assert_eq!(
        s.eval::<i64>("return GameTooltip.default").unwrap(),
        1,
        "the default-corner anchor, not ANCHOR_RIGHT off the button"
    );
    // …and the explanation draws in NORMAL_FONT_COLOR, the gold that separates it from the white
    // label above it.
    assert_eq!(
        newbie_line_color(&mut s),
        Some([1.0, 0.82, 0.0, 1.0]),
        "NORMAL_FONT_COLOR (Fonts.xml l.37)"
    );

    // Still hovering, and the latency moved: the next poll beat rewrites the plate in place.
    s.set_latency_ms(Some(7));
    poll_beat(&mut s);
    assert_eq!(
        s.eval::<String>("return GameTooltipTextLeft1:GetText()")
            .unwrap(),
        "Latency: 7ms"
    );
    assert_eq!(
        s.eval::<i64>("return GameTooltip:NumLines()").unwrap(),
        2,
        "the refresh rebuilds BOTH lines — a held-open plate never decays to one"
    );

    s.run("BenillaPerformanceBar_OnLeave()").unwrap();
    assert!(
        !s.eval::<bool>("return GameTooltip:IsVisible()").unwrap(),
        "leaving hides the plate"
    );
    // …and the poll no longer touches it.
    s.set_latency_ms(Some(999));
    poll_beat(&mut s);
    assert!(
        !s.eval::<bool>("return GameTooltip:IsVisible()").unwrap(),
        "an unhovered meter never re-opens the tooltip"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The meter is a frame slot in the bar's own child list — a regression guard on the frame count
/// the bar's end-to-end test pins, kept here so the two move together.
#[test]
fn the_meter_adds_its_two_frames_to_the_bar() {
    let s = harness(&[]);
    for name in [
        "MainMenuBarPerformanceBarFrame",
        "MainMenuBarPerformanceBarFrameButton",
    ] {
        assert!(
            s.eval::<bool>(&format!("return {name} ~= nil")).unwrap(),
            "{name} must exist"
        );
    }
    // The button is the frame's child (the ref's `parent=` attribute, expressed as nesting).
    assert_eq!(
        s.eval::<String>("return MainMenuBarPerformanceBarFrameButton:GetParent():GetName()")
            .unwrap(),
        "MainMenuBarPerformanceBarFrame"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
