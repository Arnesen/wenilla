//! The shipped `MinimapCluster.xml` driven engine-only: the +/- zoom buttons must re-sync their
//! enabled state when the active zoom index switches (stepping inside/outside a WMO flips to the
//! other, independent level). Regression guard for the director-caught stale-button bug (2026-07-09):
//! `ZoomIn` greyed from an outdoor max-zoom stayed greyed indoors at the middle default level.

use benilla_ui::script::UiScript;

use super::test_ui::load_ui as load_xml;

fn enabled(s: &UiScript, button: &str) -> bool {
    s.eval::<bool>(&format!("return {button}:IsEnabled() ~= 0"))
        .unwrap()
}

#[test]
fn minimap_zoom_buttons_resync_when_switching_inside_and_outside() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    // Host globals the cluster's OnLoad/clicks lean on that a bare engine doesn't install.
    s.run("function GetMinimapZoneText() return '' end")
        .unwrap();
    s.run("function PlaySound() end").unwrap();
    load_xml(&s, "Fonts.xml");
    // The shipped load order provides GameTooltip before the cluster; Minimap_Update's tooltip
    // half (the PVP tint slice, decision 0287) touches it from OnLoad on.
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "MinimapCluster.xml");

    // OnLoad seeds the +/- state from the live zoom. The CVar default is 3 — a middle level, so
    // BOTH buttons start enabled (the old hardcoded `MinimapZoomOut:Disable()` assumed zoom 0).
    assert_eq!(s.eval::<u8>("return Minimap:GetZoom()").unwrap(), 3);
    assert!(enabled(&s, "MinimapZoomIn"));
    assert!(enabled(&s, "MinimapZoomOut"));

    // Outdoors, zoom fully in (index 5 = max): the click handler greys ZoomIn.
    s.run("Minimap:SetZoom(5)").unwrap();
    s.fire_event("MINIMAP_UPDATE_ZOOM", vec![]);
    assert!(!enabled(&s, "MinimapZoomIn"), "at max zoom ZoomIn disables");
    assert!(enabled(&s, "MinimapZoomOut"));

    // Step INSIDE: the API now routes to the indoor index, still at its own untouched default 3.
    // The app fires MINIMAP_UPDATE_ZOOM on the transition; the cluster must re-sync to the new level.
    s.set_minimap_inside(true);
    s.fire_event("MINIMAP_UPDATE_ZOOM", vec![]);
    assert_eq!(
        s.eval::<u8>("return Minimap:GetZoom()").unwrap(),
        3,
        "indoors reads the separate indoor index"
    );
    assert!(
        enabled(&s, "MinimapZoomIn"),
        "the stale outdoor max-zoom greying must clear — this is the reported bug"
    );
    assert!(enabled(&s, "MinimapZoomOut"));

    // Indoors, zoom fully out (index 0 = widest): ZoomOut greys.
    s.run("Minimap:SetZoom(0)").unwrap();
    s.fire_event("MINIMAP_UPDATE_ZOOM", vec![]);
    assert!(
        !enabled(&s, "MinimapZoomOut"),
        "at min zoom ZoomOut disables"
    );
    assert!(enabled(&s, "MinimapZoomIn"));

    // Step back OUTSIDE: the outdoor index is exactly where we left it (5 = max), so ZoomIn is
    // greyed again and the indoor min-zoom greying of ZoomOut is gone.
    s.set_minimap_inside(false);
    s.fire_event("MINIMAP_UPDATE_ZOOM", vec![]);
    assert_eq!(s.eval::<u8>("return Minimap:GetZoom()").unwrap(), 5);
    assert!(!enabled(&s, "MinimapZoomIn"), "outdoor max-zoom survived");
    assert!(enabled(&s, "MinimapZoomOut"));
}

/// The tracking icon (ref-Minimap.xml l.109-174, transcribed verbatim): hidden at load, and the
/// verbatim OnEvent follows `GetTrackingTexture()` across `PLAYER_AURAS_CHANGED` — the event the
/// aura feed fires beside `UNIT_AURA` on every display-state change (`ui_aura`). Show with a
/// tracking state pushed, hide when it clears.
#[test]
fn tracking_frame_follows_get_tracking_texture_across_player_auras_changed() {
    let _data = benilla_formats::wow_data_or_skip!();
    use benilla_ui::script::TrackingState;

    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    s.run("function GetMinimapZoneText() return '' end")
        .unwrap();
    s.run("function PlaySound() end").unwrap();
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "MinimapCluster.xml");

    let vis = |s: &UiScript| {
        s.eval::<bool>("return MiniMapTrackingFrame:IsVisible()")
            .unwrap()
    };
    assert!(!vis(&s), "no tracking at load — the frame starts hidden");

    // The feed pushes a tracking aura (a miner's Find Minerals) and fires the rebuild event.
    s.set_tracking(Some(TrackingState {
        spell_id: 2580,
        name: Some("Find Minerals".into()),
        icon: Some("Interface\\Icons\\Trade_Mining".into()),
        cancelable: true,
    }));
    s.fire_event("PLAYER_AURAS_CHANGED", vec![]);
    assert!(vis(&s), "a live tracking texture shows the icon");

    // Tracking cancelled/expired: the same event path hides it.
    s.set_tracking(None);
    s.fire_event("PLAYER_AURAS_CHANGED", vec![]);
    assert!(!vis(&s), "no tracking texture hides the frame again");
}

/// A session with the minimap cluster + the time-of-day indicator (`GameTime.xml`) loaded, the
/// game clock parked at `hour:minute` — the shape `crate::minimap::feed_game_time` pushes.
fn game_time_session(hour: u32, minute: u32) -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    s.run("function GetMinimapZoneText() return '' end")
        .unwrap();
    s.run("function PlaySound() end").unwrap();
    s.run(&format!(
        "__benilla_game_hour = {hour}; __benilla_game_minute = {minute}"
    ))
    .unwrap();
    // The player's own strings and locale rather than a hand-typed copy of three of them: the
    // window is the reference's file since 1751 window 5, and `TIME_TWENTYFOURHOURS` and
    // `TwentyFourHourTime` are exactly what it reads. `Localization.xml` only DEFINES
    // `LocalizeFrames`; the reference calls it from `UIParent_OnEvent`'s VARIABLES_LOADED arm, and
    // this session has no UIParent, so it calls it directly.
    load_xml(&s, "Interface\\FrameXML\\GlobalStrings.lua");
    load_xml(&s, "Interface\\FrameXML\\Localization.xml");
    s.run("LocalizeFrames()").unwrap();
    load_xml(&s, "Fonts.xml");
    // `TEXT()`, which the reference's own tooltip and label formatting passes every string through.
    load_xml(&s, "BasicControls.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "MinimapCluster.xml");
    load_xml(&s, "Interface\\FrameXML\\GameTime.xml");
    s
}

/// `GameTimeTexture`'s current texcoord window, as the `(left, right, top, bottom)` rect this
/// file reasons in.
///
/// `GetTexCoord` answers EIGHT values since decision 1840 — `ULx, ULy, LLx, LLy, URx, URy, LRx,
/// LRy` — so the rect is positions 1, 5, 2, 4. Folded here rather than at each call site because
/// the window really is axis-aligned and every assertion below is about its edges.
fn tod_window(s: &UiScript) -> (f64, f64, f64, f64) {
    let (ulx, uly, _, lly, urx, ..): (f64, f64, f64, f64, f64, f64, f64, f64) =
        s.eval("return GameTimeTexture:GetTexCoord()").unwrap();
    (ulx, urx, uly, lly)
}

/// `GameTimeFrame_Update`'s law, exactly: the 50-px window over the 128×64 UI-TOD-Indicator
/// sits on the LEFT half (the sun) through the game day and slides +0.5 to the RIGHT half (the
/// moon) outside it — night is before 5:30 AM or from 9:00 PM, boundaries included exactly as
/// the ref compares (`< DAWN or >= DUSK`).
#[test]
fn game_time_frame_slides_the_sun_moon_window_on_the_game_clock() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = game_time_session(10, 30);
    // OnLoad seeded `timeOfDay = 0` and 10:30 ≠ 0, so the very first update already seated the
    // window — no OnUpdate tick needed for the initial state.
    let day = (0.0, 50.0 / 128.0, 0.0, 50.0 / 64.0);
    assert_eq!(tod_window(&s), day, "mid-morning shows the sun half");

    // 21:00 exactly is night (`>= DUSK`): the OnUpdate re-read slides the window +0.5.
    s.run("__benilla_game_hour = 21; __benilla_game_minute = 0")
        .unwrap();
    s.tick(0.016);
    assert_eq!(
        tod_window(&s),
        (0.5, 0.5 + 50.0 / 128.0, 0.0, 50.0 / 64.0),
        "9:00 PM sharp is the moon half"
    );

    // 5:29 is still night; 5:30 exactly is day (`< DAWN`).
    s.run("__benilla_game_hour = 5; __benilla_game_minute = 29")
        .unwrap();
    s.tick(0.016);
    assert_eq!(tod_window(&s).0, 0.5, "5:29 AM is still the moon");
    s.run("__benilla_game_minute = 30").unwrap();
    s.tick(0.016);
    assert_eq!(tod_window(&s), day, "5:30 AM sharp flips to the sun");
}

/// Hovering the indicator through the REAL pointer path (hit-test → OnEnter) shows the game-time
/// tooltip, live-updates it while owned (the `IsOwned` refresh branch), and hides it on leave.
/// This also pins the two loader-side pieces this frame leans on: the `<Scripts>` walker's
/// mouse auto-enable (the frame declares no enableMouse, like the reference — without the law the
/// hit-test never captures) and the `<HitRectInsets>` hull.
#[test]
fn hovering_the_indicator_shows_and_live_updates_the_game_time_tooltip() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = game_time_session(21, 7);
    s.resolve();

    // Hover the middle of the frame's hit rect: the resolved rect inset by (l=6, r=0, t=5, b=10).
    let (l, r, t, b) = (
        s.eval::<f32>("return GameTimeFrame:GetLeft()").unwrap(),
        s.eval::<f32>("return GameTimeFrame:GetRight()").unwrap(),
        s.eval::<f32>("return GameTimeFrame:GetTop()").unwrap(),
        s.eval::<f32>("return GameTimeFrame:GetBottom()").unwrap(),
    );
    let (x, y) = ((l + 6.0 + r) * 0.5, (b + 10.0 + t - 5.0) * 0.5);
    s.mouse_move(x, y);
    assert!(
        s.eval::<bool>("return GameTooltip:IsVisible()").unwrap(),
        "OnEnter owns the tooltip (the Scripts-walker auto-enable capturing at {x},{y})"
    );
    // TwentyFourHourTime = 1 (enGB LocalizeFrames, per GameTime.xml's header): 21:07, not 9:07 PM.
    let text = |s: &UiScript| {
        s.eval::<String>("return GameTooltipTextLeft1:GetText()")
            .unwrap()
    };
    assert_eq!(text(&s), "21:07");

    // The minute ticks while hovered: GameTimeFrame_Update's IsOwned branch refreshes in place.
    s.run("__benilla_game_minute = 8").unwrap();
    s.tick(0.016);
    assert_eq!(text(&s), "21:08", "the owned tooltip follows the clock");

    // Leave: a point past the left inset — outside the hit hull but still inside the raw 50×50
    // rect — must ALSO leave; the insets are part of the reference geometry.
    s.mouse_move(l + 2.0, y);
    assert!(
        !s.eval::<bool>("return GameTooltip:IsVisible()").unwrap(),
        "the 6-px left inset band is not hoverable"
    );
}

/// **The ping's click path, driven by a real mouse event** (decision 1596).
///
/// The point of the test is the *path*, not the arithmetic: `Minimap_OnClick` is only reached if
/// the widget is mouse-enabled, hit-tests, and its `OnMouseUp` fires — and what it parks has to be
/// centre-relative UI units, because that is the contract the app's conversion is written against.
/// Calling `Minimap_OnClick()` from `s.run` would prove none of it (1234 §2).
#[test]
fn a_click_on_the_minimap_parks_a_centre_relative_ping_request() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    s.run("function GetMinimapZoneText() return '' end")
        .unwrap();
    s.run("function PlaySound() end").unwrap();
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "MinimapCluster.xml");
    s.resolve();

    let cx = s
        .eval::<f32>("local x = Minimap:GetCenter(); return x")
        .unwrap();
    let cy = s
        .eval::<f32>("local _, y = Minimap:GetCenter(); return y")
        .unwrap();
    assert!(cx > 0.0 && cy > 0.0, "the widget resolved: ({cx}, {cy})");

    // Nothing is parked until somebody clicks.
    assert_eq!(s.take_minimap_ping_request(), None);

    // A click 20 UI units right and 12 up of the centre — inside the 70-unit disc.
    s.mouse_button(cx + 20.0, cy + 12.0, "LeftButton", true);
    s.mouse_button(cx + 20.0, cy + 12.0, "LeftButton", false);
    let (dx, dy) = s
        .take_minimap_ping_request()
        .expect("OnMouseUp → Minimap_OnClick → PingLocation");
    assert!((dx - 20.0).abs() < 0.01, "x right of centre: {dx}");
    assert!((dy - 12.0).abs() < 0.01, "y UP from centre: {dy}");
    // Draining is a drain: the app must not see the same click twice.
    assert_eq!(s.take_minimap_ping_request(), None);

    // A click that misses the widget entirely never reaches the handler.
    s.mouse_button(10.0, 10.0, "LeftButton", true);
    s.mouse_button(10.0, 10.0, "LeftButton", false);
    assert_eq!(s.take_minimap_ping_request(), None, "off-widget is no ping");
}

/// `Minimap:GetPingPosition()` answers **nil** with no ping, and the live pair once the app
/// publishes one — never `(0, 0)` for "there isn't one", which a caller cannot tell from a ping
/// under its own feet.
#[test]
fn get_ping_position_is_nil_until_there_is_a_ping() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    s.run("function GetMinimapZoneText() return '' end")
        .unwrap();
    s.run("function PlaySound() end").unwrap();
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "MinimapCluster.xml");

    assert_eq!(
        s.eval::<Option<f32>>("return (Minimap:GetPingPosition())")
            .unwrap(),
        None
    );
    s.set_minimap_ping(Some((0.25, -0.125)));
    let x = s
        .eval::<f32>("local x = Minimap:GetPingPosition(); return x")
        .unwrap();
    let y = s
        .eval::<f32>("local _, y = Minimap:GetPingPosition(); return y")
        .unwrap();
    assert!(
        (x - 0.25).abs() < 1e-6 && (y + 0.125).abs() < 1e-6,
        "{x} {y}"
    );
    s.set_minimap_ping(None);
    assert_eq!(
        s.eval::<Option<f32>>("return (Minimap:GetPingPosition())")
            .unwrap(),
        None
    );
}
