//! The XP bar's own tests — the hover plate, the rested/exhaustion-tick system and the
//! on-bar numerals (decision 1082) — split from `action_bar_tests.rs`, which keeps the
//! action-button machinery. Same file, two concerns: the strip along the bar's top is the
//! player-progress readout; the buttons are the input surface.

use benilla_ui::script::UiScript;

/// The XP bar's load set: the manifest prefix it actually needs — fonts, UIParent, the
/// tooltip, `TextStatusBar.xml` (the numerals machinery `BenillaExpBar_OnLoad` wires in),
/// then Cooldown + the bar itself (manifest order).
fn exp_bar_harness() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for file in [
        "Fonts.xml",
        "UIParent.xml",
        "GameTooltip.xml",
        "TextStatusBar.xml",
        "Cooldown.xml",
        "ActionBar.xml",
    ] {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets/ui")
                .join(file),
        )
        .unwrap();
        let doc = benilla_ui::framexml::parse(&text).unwrap();
        let report = benilla_ui::loader::load(&s, &doc, &|_| None);
        assert!(
            report.errors.is_empty(),
            "{file}: loader errors: {:?}",
            report.errors
        );
    }
    s
}

/// The XP bar's hover (ref-MainMenuBar.xml l.136-147): the strip takes the mouse, and the plate is
/// the ref's two-line `GameTooltip_AddNewbieTip` — "XP Bar" over the wrapped explanation.
///
/// `enableMouse` is the load-bearing half and the easy thing to lose: without it the strip is
/// transparent and the hover silently never fires, which no tooltip assertion alone would catch.
#[test]
fn the_xp_bar_takes_the_mouse_and_explains_itself() {
    let mut s = exp_bar_harness();
    s.resolve();

    // Mid-height of the strip (the bar's top 13 px), a quarter of the way along — NOT its
    // horizontal center, where the page arrows straddle the strip's lower edge and rightly take
    // the mouse ahead of it.
    let (x, y) = s
        .eval::<(f64, f64)>(
            "return BenillaExpBar:GetLeft() + BenillaExpBar:GetWidth() / 4, \
                    (BenillaExpBar:GetBottom() + BenillaExpBar:GetTop()) / 2",
        )
        .unwrap();
    assert_eq!(
        s.hit_test_name(x as f32, y as f32).as_deref(),
        Some("BenillaExpBar"),
        "the XP strip must be mouse-enabled or the hover never fires"
    );

    s.run("BenillaExpBar_OnEnter(BenillaExpBar)").unwrap();
    assert_eq!(
        s.eval::<String>("return GameTooltipTextLeft1:GetText()")
            .unwrap(),
        "XP Bar"
    );
    assert_eq!(
        s.eval::<String>("return GameTooltipTextLeft2:GetText()")
            .unwrap(),
        s.eval::<String>("return NEWBIE_TOOLTIP_XPBAR").unwrap(),
        "line 2 is the ref's NEWBIE_TOOLTIP_XPBAR, verbatim"
    );
    assert_eq!(
        s.eval::<i64>("return GameTooltip.default").unwrap(),
        1,
        "the default-corner anchor"
    );

    s.run("BenillaExpBar_OnLeave()").unwrap();
    assert!(
        !s.eval::<bool>("return GameTooltip:IsVisible()").unwrap(),
        "leaving hides the plate"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The exhaustion tick (decision 1082, ref `ExhaustionTick_Update`): with 1000/10000 XP and a
/// rested pool of 700 base-XP, the doubled span is 1400 bar-XP, so the tick parks at
/// (1000+1400)/10000 of the strip's width and the pale fill stretches exactly to it; the bar
/// paints rested blue. Draining the pool (normal state) hides both and returns the purple; a
/// pool whose span runs past the level's end hides the tick but keeps the rested blue.
#[test]
fn the_exhaustion_tick_marks_where_rested_runs_out() {
    let mut s = exp_bar_harness();

    s.set_player_xp(1000, 10000);
    s.set_rest_state(1, 700, true);
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
    s.resolve();

    // The binding trio underneath (the app-feed shape the XML consumes).
    let (id, name, mult) = s
        .eval::<(i64, String, f64)>("return GetRestState()")
        .unwrap();
    assert_eq!((id, name.as_str(), mult), (1, "Rested", 2.0));
    assert_eq!(
        s.eval::<Option<i64>>("return GetXPExhaustion()").unwrap(),
        Some(1400),
        "the pool is served ×2 — bar-XP, not wire units"
    );
    assert_eq!(
        s.eval::<Option<i64>>("return IsResting()").unwrap(),
        Some(1)
    );

    let ok: bool = s
        .eval(
            r#"
            local tick, fill = BenillaExhaustionTick, BenillaExhaustionLevelFillBar
            local expected = (1000 + 1400) / 10000 * BenillaExpBar:GetWidth()
            local x = tick:GetCenter()
            local r, g, b = BenillaExpBar:GetStatusBarColor()
            -- fill's width via its RESOLVED edges: the authored size is 0 x 13, and the engine
            -- derives the span from the TOPLEFT + runtime-TOPRIGHT anchor pair (layout.rs's
            -- zero-size law), exactly as the real client does.
            return tick:IsVisible() and fill:IsShown()
               and math.abs((x - BenillaExpBar:GetLeft()) - expected) < 0.5
               and math.abs((fill:GetRight() - fill:GetLeft()) - expected) < 0.5
               and r == 0.0 and math.abs(g - 0.39) < 0.001 and math.abs(b - 0.88) < 0.001
        "#,
        )
        .unwrap();
    assert!(ok, "tick at the rested boundary, fill up to it, bar blue");

    // The pool drains dry: state normal, no threshold — tick and fill gone, purple back.
    s.set_rest_state(2, 0, false);
    s.fire_event("UPDATE_EXHAUSTION", vec![]);
    let ok: bool = s
        .eval(
            r#"
            local r, g, b = BenillaExpBar:GetStatusBarColor()
            return not BenillaExhaustionTick:IsShown()
               and not BenillaExhaustionLevelFillBar:IsShown()
               and GetXPExhaustion() == nil
               and math.abs(r - 0.58) < 0.001 and g == 0.0 and math.abs(b - 0.55) < 0.001
        "#,
        )
        .unwrap();
    assert!(ok, "a dry pool hides the tick and returns the purple bar");

    // A pool past the level's end (6000×2 span from 1000/10000): off the strip → hidden tick,
    // still rested blue (the ref's exhaustionTickSet > width branch).
    s.set_rest_state(1, 6000, false);
    s.fire_event("UPDATE_EXHAUSTION", vec![]);
    let ok: bool = s
        .eval(
            r#"
            local r, g, b = BenillaExpBar:GetStatusBarColor()
            return not BenillaExhaustionTick:IsShown()
               and not BenillaExhaustionLevelFillBar:IsShown()
               and math.abs(g - 0.39) < 0.001
        "#,
        )
        .unwrap();
    assert!(
        ok,
        "a span past the level end hides the tick but keeps the blue"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The hover numerals (decision 1082, ref TextStatusBar.lua over `MainMenuBarExpText`): entering
/// the strip shows "curr / max" centered on the bar, leaving hides it (the lockShow refcount's
/// 0↔1 edge — the `statusBarText` cvar that pins it on permanently reads OFF here).
#[test]
fn the_xp_bar_numerals_show_on_hover() {
    let mut s = exp_bar_harness();
    s.set_player_xp(1234, 5678);
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);

    s.run("BenillaExpBar_OnEnter(BenillaExpBar)").unwrap();
    assert_eq!(
        s.eval::<String>("return BenillaExpBarText:GetText()")
            .unwrap(),
        "1234 / 5678"
    );
    assert!(s
        .eval::<bool>("return BenillaExpBarText:IsShown()")
        .unwrap());

    s.run("BenillaExpBar_OnLeave(BenillaExpBar)").unwrap();
    assert!(
        !s.eval::<bool>("return BenillaExpBarText:IsShown()")
            .unwrap(),
        "leaving drops the lockShow refcount to 0 and hides the numerals"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The rest-state tooltip line (decision 1082, ref `ExhaustionToolTipText`'s newbie-tips branch):
/// hovering the BAR arms a 1 s timer and the canAddRestStateLine handshake; when the timer runs
/// out on the bar's OnUpdate, the "Rested / 200% of normal experience" line is APPENDED to the
/// held-open newbie tip, once (the handshake is consumed).
#[test]
fn the_rest_state_line_joins_the_held_open_tooltip() {
    let mut s = exp_bar_harness();
    s.set_player_xp(1000, 10000);
    s.set_rest_state(1, 700, true);
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);

    // 1.12's default posture (0661): detailed newbie tips ON.
    s.run("SHOW_NEWBIE_TIPS = \"1\"").unwrap();
    s.run("BenillaExpBar_OnEnter(BenillaExpBar)").unwrap();
    assert_eq!(
        s.eval::<f64>("return BenillaExhaustionTick.timer").unwrap(),
        1.0,
        "the bar hover arms the tick's timer"
    );

    // Three OnUpdate ticks walk the timer 1 → 0.4 → −0.2 → fire (the ref's `< 0` edge).
    for _ in 0..3 {
        s.run("BenillaExhaustionTick_OnUpdate(0.6)").unwrap();
    }
    let appended: String = s.eval("return GameTooltipTextLeft3:GetText()").unwrap();
    assert!(
        appended.contains("Rested") && appended.contains("200%"),
        "the rest-state line rides the open plate: {appended:?}"
    );
    assert!(
        s.eval::<bool>("return GameTooltip.canAddRestStateLine == nil")
            .unwrap(),
        "the handshake is consumed — the line never doubles"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
