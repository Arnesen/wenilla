//! The breath / fatigue bars against the transcribed reference behavior (decision 0874): the
//! extracted 1.12 `MirrorTimer.lua` is the spec — a bar per running timer, the timer's own colour
//! and caption, the value integrating `scale * elapsed` between packets, and the frame released
//! back to the pool on STOP.
//!
//! Two halves, and the second is the load-bearing one (the cast bar's lesson, `cast_tests`): the
//! state tests read the Lua back, which is blind to what actually *paints*. The
//! [`draw list`](UiScript::extract) tests below are the ones that would catch a bar that is shown,
//! correctly valued, and invisible.

use benilla_ui::script::{ExtractedQuad, QuadContent, ScriptValue, UiScript};

/// Load one shipped `assets/ui/<file>`, panicking on any loader error.
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

fn harness() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MirrorTimer.xml");
    s
}

/// The app's own `MIRROR_TIMER_START` arg list (`ui_mirror::feed_mirror_timers`): the client's
/// timer name, milliseconds, milliseconds, the signed rate, the paused flag, the caption.
fn start(
    s: &mut UiScript,
    name: &str,
    remaining_ms: i64,
    duration_ms: i64,
    scale: i64,
    label: &str,
) {
    s.fire_event(
        "MIRROR_TIMER_START",
        vec![
            ScriptValue::Str(name.into()),
            ScriptValue::Int(remaining_ms),
            ScriptValue::Int(duration_ms),
            ScriptValue::Int(scale),
            ScriptValue::Int(0),
            ScriptValue::Str(label.into()),
        ],
    );
}

fn shown(s: &UiScript, frame: &str) -> bool {
    s.eval::<bool>(&format!("return {frame}:IsShown()"))
        .unwrap()
}

fn bar_value(s: &UiScript, frame: &str) -> f64 {
    s.eval::<f64>(&format!("return {frame}StatusBar:GetValue()"))
        .unwrap()
}

fn bar_max(s: &UiScript, frame: &str) -> f64 {
    s.eval::<f64>(&format!(
        "local lo, hi = {frame}StatusBar:GetMinMaxValues(); return hi"
    ))
    .unwrap()
}

fn bar_color(s: &UiScript, frame: &str) -> (f64, f64, f64) {
    s.eval::<(f64, f64, f64)>(&format!(
        "local r, g, b = {frame}StatusBar:GetStatusBarColor(); return r, g, b"
    ))
    .unwrap()
}

fn caption(s: &UiScript, frame: &str) -> String {
    s.eval::<String>(&format!("return {frame}Text:GetText()"))
        .unwrap()
}

/// One tick of the app's real order (`drive_script`): OnUpdate, resolve, then the draw list.
fn frame(s: &mut UiScript, dt: f32) -> Vec<ExtractedQuad> {
    s.tick(dt);
    s.resolve();
    s.extract()
}

/// The quad drawn from `Interface\...\<leaf>`, if any.
fn tex_quad<'a>(quads: &'a [ExtractedQuad], leaf: &str) -> Option<&'a ExtractedQuad> {
    quads.iter().find(|q| match &q.content {
        QuadContent::Texture { path: Some(p), .. } => p.ends_with(leaf),
        _ => false,
    })
}

/// Every bar starts hidden — no timer, no chrome on screen.
#[test]
fn the_three_timers_load_hidden() {
    let mut s = harness();
    for i in 1..=3 {
        assert!(!shown(&s, &format!("MirrorTimer{i}")), "MirrorTimer{i}");
    }
    assert!(
        tex_quad(&frame(&mut s, 0.016), "UI-CastingBar-Border").is_none(),
        "no border chrome before a timer starts"
    );
}

/// A breath timer: the first free frame takes it, blue, captioned, scaled to the full span, and
/// seated at the value the server named.
#[test]
fn breath_takes_the_first_bar_in_the_reference_blue() {
    let mut s = harness();
    start(&mut s, "BREATH", 45_000, 60_000, -1, "Breath");

    assert!(shown(&s, "MirrorTimer1"));
    assert!(!shown(&s, "MirrorTimer2"), "only one timer is running");
    assert_eq!(caption(&s, "MirrorTimer1"), "Breath");
    // Milliseconds on the wire, seconds in the bar — the reference's `/1000` on both.
    assert_eq!(bar_value(&s, "MirrorTimer1"), 45.0);
    assert_eq!(bar_max(&s, "MirrorTimer1"), 60.0);
    // MirrorTimerColors["BREATH"]
    let (r, g, b) = bar_color(&s, "MirrorTimer1");
    assert!(
        (r - 0.0).abs() < 1e-6 && (g - 0.5).abs() < 1e-6 && (b - 1.0).abs() < 1e-6,
        "reference blue, got ({r}, {g}, {b})"
    );
}

/// Fatigue is a different timer type, a different colour and a different caption — and the
/// client's key for it is `EXHAUSTION`, not the server's `FATIGUE`.
#[test]
fn fatigue_is_the_reference_yellow_under_its_own_caption() {
    let mut s = harness();
    start(&mut s, "EXHAUSTION", 60_000, 60_000, -1, "Fatigue");

    assert!(shown(&s, "MirrorTimer1"));
    assert_eq!(caption(&s, "MirrorTimer1"), "Fatigue");
    let (r, g, b) = bar_color(&s, "MirrorTimer1");
    assert!(
        (r - 1.0).abs() < 1e-6 && (g - 0.9).abs() < 1e-6 && (b - 0.0).abs() < 1e-6,
        "reference yellow, got ({r}, {g}, {b})"
    );
}

/// The whole client-side motion: the frame integrates `scale * elapsed` per OnUpdate. At the
/// reference's draining rate that is one bar-second per real second.
#[test]
fn the_bar_drains_at_the_servers_signed_rate() {
    let mut s = harness();
    start(&mut s, "BREATH", 45_000, 60_000, -1, "Breath");

    for _ in 0..100 {
        frame(&mut s, 0.05); // 5 s of client time, in reference-sized ticks
    }
    let after = bar_value(&s, "MirrorTimer1");
    assert!(
        (after - 40.0).abs() < 0.05,
        "5 s at scale -1 should land near 40, got {after}"
    );

    // Surfacing: the server re-sends the same timer with the +10 refill rate (there is no update
    // opcode). The bar must reverse, ten times faster than it drained.
    start(&mut s, "BREATH", 40_000, 60_000, 10, "Breath");
    for _ in 0..20 {
        frame(&mut s, 0.05); // 1 s
    }
    let refilled = bar_value(&s, "MirrorTimer1");
    assert!(
        (refilled - 50.0).abs() < 0.5,
        "1 s at scale +10 should climb ~10 bar-seconds to ~50, got {refilled}"
    );
}

/// A paused timer holds its value — the frozen state the server sends when it stops the clock.
#[test]
fn a_paused_timer_holds_its_value() {
    let mut s = harness();
    s.fire_event(
        "MIRROR_TIMER_START",
        vec![
            ScriptValue::Str("BREATH".into()),
            ScriptValue::Int(45_000),
            ScriptValue::Int(60_000),
            ScriptValue::Int(-1),
            ScriptValue::Int(1), // paused
            ScriptValue::Str("Breath".into()),
        ],
    );
    for _ in 0..40 {
        frame(&mut s, 0.05);
    }
    assert_eq!(
        bar_value(&s, "MirrorTimer1"),
        45.0,
        "frozen: no integration at all"
    );

    // MIRROR_TIMER_PAUSE(name, 0) releases it (our arg2 reading — the reference's own branch is
    // bugged and the packet is never sent by vmangos; see MirrorTimer.xml departure 3).
    s.fire_event(
        "MIRROR_TIMER_PAUSE",
        vec![ScriptValue::Str("BREATH".into()), ScriptValue::Int(0)],
    );
    for _ in 0..20 {
        frame(&mut s, 0.05);
    }
    assert!(
        bar_value(&s, "MirrorTimer1") < 44.5,
        "unfrozen: draining again"
    );
}

/// Two timers at once (deep water: fatigue *and* breath) stack onto separate frames, each keeping
/// its own colour — and a re-STATE of one lands back on the frame already holding it rather than
/// consuming a third.
#[test]
fn two_timers_stack_and_restate_in_place() {
    let mut s = harness();
    start(&mut s, "EXHAUSTION", 60_000, 60_000, -1, "Fatigue");
    start(&mut s, "BREATH", 45_000, 60_000, -1, "Breath");

    assert!(shown(&s, "MirrorTimer1") && shown(&s, "MirrorTimer2"));
    assert!(!shown(&s, "MirrorTimer3"));
    assert_eq!(caption(&s, "MirrorTimer1"), "Fatigue");
    assert_eq!(caption(&s, "MirrorTimer2"), "Breath");

    // The server re-sends breath on every change; it must reuse frame 2, not take frame 3.
    start(&mut s, "BREATH", 30_000, 60_000, -1, "Breath");
    assert!(!shown(&s, "MirrorTimer3"), "re-state must not take a frame");
    assert_eq!(bar_value(&s, "MirrorTimer2"), 30.0);
    assert_eq!(bar_value(&s, "MirrorTimer1"), 60.0, "fatigue untouched");
}

/// STOP hides only the named timer and hands its frame back to the pool.
#[test]
fn stop_hides_that_timer_and_frees_its_frame() {
    let mut s = harness();
    start(&mut s, "EXHAUSTION", 60_000, 60_000, -1, "Fatigue");
    start(&mut s, "BREATH", 45_000, 60_000, -1, "Breath");

    s.fire_event(
        "MIRROR_TIMER_STOP",
        vec![ScriptValue::Str("EXHAUSTION".into())],
    );
    assert!(!shown(&s, "MirrorTimer1"), "fatigue's frame released");
    assert!(shown(&s, "MirrorTimer2"), "breath untouched");

    // The freed frame is reusable: a fresh timer takes it back.
    start(&mut s, "FEIGNDEATH", 10_000, 10_000, -1, "");
    assert!(shown(&s, "MirrorTimer1"));
    assert_eq!(
        caption(&s, "MirrorTimer1"),
        "",
        "no FEIGNDEATH_LABEL exists in the 1.12 GlobalStrings"
    );
}

/// Entering the world clears every bar — a worldport must not leave a stale breath bar behind
/// (the reference's `PLAYER_ENTERING_WORLD` arm).
#[test]
fn entering_the_world_clears_every_bar() {
    let mut s = harness();
    start(&mut s, "EXHAUSTION", 60_000, 60_000, -1, "Fatigue");
    start(&mut s, "BREATH", 45_000, 60_000, -1, "Breath");

    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
    for i in 1..=3 {
        assert!(!shown(&s, &format!("MirrorTimer{i}")), "MirrorTimer{i}");
    }
}

/// A STOP for a timer that is not running must be inert — not an error, and not a frame hidden
/// out from under a different timer.
#[test]
fn a_stop_for_an_idle_timer_touches_nothing() {
    let mut s = harness();
    start(&mut s, "BREATH", 45_000, 60_000, -1, "Breath");
    s.fire_event(
        "MIRROR_TIMER_STOP",
        vec![ScriptValue::Str("EXHAUSTION".into())],
    );
    assert!(shown(&s, "MirrorTimer1"), "breath's bar survives");
    assert_eq!(bar_value(&s, "MirrorTimer1"), 45.0);
}

/// The paint half. A running timer must actually put its chrome and its fill on screen — the
/// state tests above all pass on a bar that draws nothing.
#[test]
fn a_running_timer_paints_its_chrome_and_fill() {
    let mut s = harness();
    start(&mut s, "BREATH", 45_000, 60_000, -1, "Breath");
    let quads = frame(&mut s, 0.016);

    let border = tex_quad(&quads, "UI-CastingBar-Border").expect("the bar's border chrome");
    let border_w = border.rect.expect("the border resolves a rect").width();
    assert!(border_w > 100.0, "the 256-wide border, got {border_w}");

    let fill = tex_quad(&quads, "UI-StatusBar").expect("the status-bar fill");
    let fill_w = fill.rect.expect("the fill resolves a rect").width();
    // 45 of 60 seconds over the reference's 195 px bar.
    let expected = 195.0 * 45.0 / 60.0;
    assert!(
        (fill_w - expected).abs() < 2.0,
        "fill should be ~{expected} px at 45/60, got {fill_w}"
    );

    assert!(
        quads.iter().any(|q| matches!(
            &q.content,
            QuadContent::Text { text: Some(t), .. } if t == "Breath"
        )),
        "the caption is drawn"
    );
}

/// …and a stopped timer takes all of it off screen again.
#[test]
fn a_stopped_timer_paints_nothing() {
    let mut s = harness();
    start(&mut s, "BREATH", 45_000, 60_000, -1, "Breath");
    frame(&mut s, 0.016);
    s.fire_event("MIRROR_TIMER_STOP", vec![ScriptValue::Str("BREATH".into())]);

    let quads = frame(&mut s, 0.016);
    assert!(tex_quad(&quads, "UI-CastingBar-Border").is_none());
    assert!(tex_quad(&quads, "UI-StatusBar").is_none());
}
