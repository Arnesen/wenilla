//! The shipped `MinimapCluster.xml` driven engine-only: the +/- zoom buttons must re-sync their
//! enabled state when the active zoom index switches (stepping inside/outside a WMO flips to the
//! other, independent level). Regression guard for the director-caught stale-button bug (2026-07-09):
//! `ZoomIn` greyed from an outdoor max-zoom stayed greyed indoors at the middle default level.

use benilla_ui::script::UiScript;

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error (the other tests'
/// loader, duplicated so this file is self-contained).
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

fn enabled(s: &UiScript, button: &str) -> bool {
    s.eval::<bool>(&format!("return {button}:IsEnabled()"))
        .unwrap()
}

#[test]
fn minimap_zoom_buttons_resync_when_switching_inside_and_outside() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    // Host globals the cluster's OnLoad/clicks lean on that a bare engine doesn't install.
    s.run("function GetMinimapZoneText() return '' end")
        .unwrap();
    s.run("function PlaySound() end").unwrap();
    load_xml(&s, "Fonts.xml");
    // The shipped load order provides GameTooltip before the cluster; Minimap_Update's verbatim
    // ref block (the PVP tint slice, decision 0287) touches it from OnLoad on.
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
