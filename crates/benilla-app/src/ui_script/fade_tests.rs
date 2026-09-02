//! The fade kit's driver (`UIFrameFade` / `BenillaFadeDriver`, UiPanels.xml) — the park/wake
//! lifecycle that keeps the per-frame fade walk off the tick while nothing is fading. Split from
//! `panel_tests.rs` (1507's round): the slot manager and the fade kit share a source file, not a
//! concern.

use benilla_ui::script::UiScript;

use super::test_ui::load_ui as load_xml;

/// **The idle fade driver parks itself** (decision 1396's class, the audit's driver-hide item):
/// with `FADEFRAMES` empty there is nothing to walk, so the driver's OnUpdate hides its own frame
/// — and the engine dispatches OnUpdate only to visible frames (`tick.rs`), which stops the
/// per-frame walk cold. The probe wraps `UIFrameFadeUpdate` in a counter BEFORE the first tick:
/// ten idle ticks later it has never run, and the driver is hidden.
#[test]
fn an_idle_fade_driver_parks_itself_off_the_tick() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, r"Interface\FrameXML\UIPanelTemplates.lua");
    load_xml(&s, r"Interface\FrameXML\UIPanelTemplates.xml");
    s.resolve();
    s.run(
        "BENILLA_TEST_FADE_TICKS = 0\n\
         local real = UIFrameFadeUpdate\n\
         function UIFrameFadeUpdate(elapsed)\n\
             BENILLA_TEST_FADE_TICKS = BENILLA_TEST_FADE_TICKS + 1\n\
             real(elapsed)\n\
         end",
    )
    .unwrap();
    for _ in 0..10 {
        s.tick(0.016);
        s.resolve();
    }
    assert!(s.errors().is_empty(), "{:?}", s.errors());
    assert!(
        !s.eval::<bool>("return BenillaFadeDriver:IsShown()")
            .unwrap(),
        "an empty FADEFRAMES parks the driver"
    );
    assert_eq!(
        s.eval::<i64>("return BENILLA_TEST_FADE_TICKS").unwrap(),
        0,
        "a parked driver never runs the fade walk"
    );
}

/// The control for the guard above: the gate must not cost the fades it gates. `UIFrameFade` (the
/// kit's single insertion point — `UIFrameFadeIn`/`Out` both land there) wakes the driver beside
/// its `table.insert`; the fade then ramps on real ticks, completes, and the driver re-parks on
/// the first empty tick after.
#[test]
fn a_started_fade_still_ramps_and_the_driver_reparks_after() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, r"Interface\FrameXML\UIPanelTemplates.lua");
    load_xml(&s, r"Interface\FrameXML\UIPanelTemplates.xml");
    s.resolve();
    s.run(r#"CreateFrame("Frame", "BenillaFadeProbe")"#)
        .unwrap();
    for _ in 0..2 {
        s.tick(0.016); // settle: the driver parks
        s.resolve();
    }
    assert!(!s
        .eval::<bool>("return BenillaFadeDriver:IsShown()")
        .unwrap());

    s.run("UIFrameFadeIn(BenillaFadeProbe, 1.0)").unwrap();
    assert!(
        s.eval::<bool>("return BenillaFadeDriver:IsShown()")
            .unwrap(),
        "starting a fade wakes the driver"
    );
    assert_eq!(
        s.eval::<f64>("return BenillaFadeProbe:GetAlpha()").unwrap(),
        0.0,
        "the fade armed at its IN startAlpha"
    );

    s.tick(0.5);
    s.resolve();
    let mid = s.eval::<f64>("return BenillaFadeProbe:GetAlpha()").unwrap();
    assert!(
        (mid - 0.5).abs() < 1e-3,
        "half the timeToFade in, half the ramp: {mid}"
    );

    s.tick(0.6); // past timeToFade: the fade completes and leaves the list
    s.resolve();
    assert_eq!(
        s.eval::<f64>("return BenillaFadeProbe:GetAlpha()").unwrap(),
        1.0,
        "the fade completes at its endAlpha"
    );
    s.tick(0.016); // the first empty tick re-parks the driver
    s.resolve();
    assert!(
        !s.eval::<bool>("return BenillaFadeDriver:IsShown()")
            .unwrap(),
        "the driver re-parks once the list is empty"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
