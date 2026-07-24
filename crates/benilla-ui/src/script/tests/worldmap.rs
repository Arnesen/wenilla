//! The world-map bindings (decision 0203 phase 2): catalog/feed pushes, the engine-owned
//! selection, and the deferred WORLD_MAP_UPDATE queue.

use super::common::script;
use crate::script::*;

/// A two-continent catalog shaped like the real 5875 one (Kalimdor first — the app's push order
/// defines every Lua-visible index; the sheet rects are the real disjoint kernel outputs). The
/// EK zone grid is sparse: exactly the real Goldshire cell (12735, verified against the shipped
/// Azeroth.zmp) marked as zone 1.
fn push_catalog(s: &mut UiScript) {
    let mut ek_grid = vec![0u16; 128 * 128];
    ek_grid[12735] = 1;
    // Cell 8801 = the child (0.5, 0.5) resolves to through Elwynn's OWN loc rect — a "Stormwind
    // City" peer child. Clicking it from the Elwynn zone map drills into the city (the §3 path).
    ek_grid[8801] = 2;
    s.set_world_map_catalog(vec![
        WorldMapContinentView {
            name: "Kalimdor".into(),
            map_file: "Kalimdor".into(),
            world_rect: (0.08882, 0.07910, 0.40020, 0.86952),
            loc_rect: (17066.6, -19733.2, 12799.9, -11733.3),
            zone_grid: Vec::new(),
            zones: vec![
                WorldMapZoneView {
                    name: "Durotar".into(),
                    area_id: 14,
                    map_file: "Durotar".into(),
                    loc_rect: (-1800.0, -6800.0, -3800.0, -8500.0),
                    overlays: Vec::new(),
                },
                WorldMapZoneView {
                    name: "The Barrens".into(),
                    area_id: 17,
                    map_file: "Barrens".into(),
                    loc_rect: (-1000.0, -11000.0, -5500.0, -17000.0),
                    overlays: Vec::new(),
                },
            ],
        },
        WorldMapContinentView {
            name: "Eastern Kingdoms".into(),
            map_file: "Azeroth".into(),
            world_rect: (0.62375, 0.02695, 0.92315, 0.87126),
            loc_rect: (16000.0, -19199.9, 7466.6, -16000.0),
            zone_grid: ek_grid,
            zones: vec![
                WorldMapZoneView {
                    name: "Elwynn Forest".into(),
                    area_id: 12,
                    map_file: "Elwynn".into(),
                    // A clean synthetic loc rect inside the EK continent rect: w=2000, h=1500 → the
                    // continent-highlight formula lands on round-ish expected coords (asserted below).
                    loc_rect: (-8000.0, -10000.0, -400.0, -1900.0),
                    // Two overlays shaped like real Elwynn rows: Northshire reveals on explore bit
                    // 125, Goldshire on bit 124 (the real AreaTable exploreFlags).
                    overlays: vec![
                        WorldMapOverlayView {
                            texture: "Interface\\WorldMap\\Elwynn\\NORTHSHIREVALLEY".into(),
                            width: 200,
                            height: 176,
                            offset_x: 490,
                            offset_y: 200,
                            explore_bits: vec![125],
                        },
                        WorldMapOverlayView {
                            texture: "Interface\\WorldMap\\Elwynn\\GOLDSHIRE".into(),
                            width: 172,
                            height: 128,
                            offset_x: 380,
                            offset_y: 320,
                            explore_bits: vec![124],
                        },
                    ],
                },
                // A city peer of Elwynn — a WorldMapArea child of the continent with its own art
                // folder, indistinguishable from a zone in code (wow-re 15b2a8ea Part 2a).
                WorldMapZoneView {
                    name: "Stormwind City".into(),
                    area_id: 1519,
                    map_file: "StormwindCity".into(),
                    loc_rect: (-8200.0, -8900.0, -400.0, -1100.0),
                    overlays: Vec::new(),
                },
            ],
        },
    ]);
}

/// The navigation surface: lists come from the catalog, SetMapZoom's selection reads back
/// synchronously in the same Lua breath (the reference's dropdown-click contract), zone/continent
/// arguments clamp, and GetMapInfo names the displayed art folder (nil at the world level — the
/// reference Lua's own "World" fallback).
#[test]
fn worldmap_navigation_and_map_info() {
    let mut s = script();
    push_catalog(&mut s);

    s.run(
        r#"
        conts = { GetMapContinents() }
        kalimdor_zones = { GetMapZones(1) }
    "#,
    )
    .unwrap();
    assert_eq!(
        s.eval::<(String, String)>("return conts[1], conts[2]")
            .unwrap(),
        ("Kalimdor".into(), "Eastern Kingdoms".into()),
        "display names are the Map.dbc localized ones, not the art folders"
    );
    assert_eq!(
        s.eval::<(String, String)>("return kalimdor_zones[1], kalimdor_zones[2]")
            .unwrap(),
        ("Durotar".into(), "The Barrens".into())
    );

    // World level by default: continent 0, nil map info.
    assert_eq!(
        s.eval::<(i64, i64)>("return GetCurrentMapContinent(), GetCurrentMapZone()")
            .unwrap(),
        (0, 0)
    );
    assert!(s.eval::<bool>("return GetMapInfo() == nil").unwrap());

    // The synchronous read-back the reference relies on, and the art-folder name per level.
    s.run("SetMapZoom(2)").unwrap();
    assert_eq!(
        s.eval::<(i64, i64, String)>(
            "return GetCurrentMapContinent(), GetCurrentMapZone(), GetMapInfo()"
        )
        .unwrap(),
        (2, 0, "Azeroth".into())
    );
    s.run("SetMapZoom(1, 2)").unwrap();
    assert_eq!(
        s.eval::<String>("return GetMapInfo()").unwrap(),
        "Barrens",
        "a zone selection names the ZONE's art folder"
    );

    // Out-of-range arguments clamp (never a Lua error — addons pass garbage).
    s.run("SetMapZoom(9, 9)").unwrap();
    assert_eq!(
        s.eval::<(i64, i64)>("return GetCurrentMapContinent(), GetCurrentMapZone()")
            .unwrap(),
        (2, 2),
        "continent clamps to the catalog, zone to that continent's list (EK has 2 zones)"
    );
}

/// SetMapToCurrentZone lands on the app-fed player zone; the feed's projection + facing surface
/// through GetPlayerMapPosition/GetPlayerFacing (and only for "player" — party is a later arc).
#[test]
fn worldmap_current_zone_and_player_feed() {
    let mut s = script();
    push_catalog(&mut s);

    s.set_world_map_feed(Some((1, 2)), Some((0.25, 0.75)), 1.5, None);
    s.run("SetMapToCurrentZone()").unwrap();
    assert_eq!(
        s.eval::<(i64, i64)>("return GetCurrentMapContinent(), GetCurrentMapZone()")
            .unwrap(),
        (1, 2)
    );
    let (x, y) = s
        .eval::<(f64, f64)>(r#"return GetPlayerMapPosition("player")"#)
        .unwrap();
    assert!((x - 0.25).abs() < 1e-6 && (y - 0.75).abs() < 1e-6);
    assert!((s.eval::<f64>("return GetPlayerFacing()").unwrap() - 1.5).abs() < 1e-6);
    assert_eq!(
        s.eval::<(f64, f64)>(r#"return GetPlayerMapPosition("party1")"#)
            .unwrap(),
        (0.0, 0.0),
        "non-player units are off-map until the party arc"
    );

    // No feed → the world sheet (never an error).
    s.set_world_map_feed(None, None, 0.0, None);
    s.run("SetMapToCurrentZone()").unwrap();
    assert_eq!(s.eval::<i64>("return GetCurrentMapContinent()").unwrap(), 0);
}

/// World-level ProcessMapClick picks the continent whose sheet-rect contains the click (the
/// 0x4a7100 AABB walk — the real kernel rects are disjoint); continent-level clicks resolve
/// through the 0x4a6ec0 zone grid; hover names ride the same cell law.
#[test]
fn worldmap_click_containment_and_zone_grid() {
    let mut s = script();
    push_catalog(&mut s);

    // Mid-Kalimdor on the world sheet.
    s.run("ProcessMapClick(0.25, 0.45)").unwrap();
    assert_eq!(s.eval::<i64>("return GetCurrentMapContinent()").unwrap(), 1);

    // Kalimdor ships no grid in this fixture: continent-level clicks are inert.
    s.run("ProcessMapClick(0.5, 0.5)").unwrap();
    assert_eq!(
        s.eval::<(i64, i64)>("return GetCurrentMapContinent(), GetCurrentMapZone()")
            .unwrap(),
        (1, 0)
    );

    // Back at the world level, EK land picks continent 2.
    s.run("SetMapZoom(0)").unwrap();
    s.run("ProcessMapClick(0.72, 0.63)").unwrap();
    assert_eq!(s.eval::<i64>("return GetCurrentMapContinent()").unwrap(), 2);

    // Continent level: Goldshire's UV inside the EK rect lands on grid cell 12735 (the real
    // Azeroth.zmp index for that world position) → zone 1. The zone lights up: name + fileName +
    // the six geometry values (wow-re 15b2a8ea continent branch), all against the zone's loc rect
    // within the continent. cont_w=35199.9, cont_h=23466.6; w=2000, h=1500.
    let (name, file, tpx, tpy, tx, ty, sx, sy) = s
        .eval::<(String, String, f64, f64, f64, f64, f64, f64)>(
            "return UpdateMapHighlight(0.452843, 0.720880)",
        )
        .unwrap();
    assert_eq!(name, "Elwynn Forest");
    assert_eq!(
        file, "Elwynn",
        "fileName = the zone art folder → <file>Highlight"
    );
    assert_eq!(
        tpx, 1.0,
        "continent-branch texPercentageX is the constant 1.0"
    );
    assert!(
        (tpy - 0.75).abs() < 1e-6,
        "texPercentageY = dim/potdim = 96/128 = 0.75, got {tpy}"
    );
    assert!(
        (tx - 2000.0 / 35199.9).abs() < 1e-4,
        "textureX = w/contW, got {tx}"
    );
    assert!(
        (ty - 1500.0 / 23466.6).abs() < 1e-4,
        "textureY = h/contH, got {ty}"
    );
    assert!(
        (sx - 24000.0 / 35199.9).abs() < 1e-4,
        "scrollChildX = (contX0-zoneX0)/contW, got {sx}"
    );
    assert!(
        (sy - 7866.6 / 23466.6).abs() < 1e-4,
        "scrollChildY = (contY0-zoneY0)/contH, got {sy}"
    );
    // Ocean at continent level: off the grid → the nil/zero tail (frame hides the quad).
    assert!(s
        .eval::<Option<String>>("return UpdateMapHighlight(0.99, 0.01)")
        .unwrap()
        .is_none());
    s.run("ProcessMapClick(0.452843, 0.720880)").unwrap();
    assert_eq!(
        s.eval::<(i64, i64)>("return GetCurrentMapContinent(), GetCurrentMapZone()")
            .unwrap(),
        (2, 1),
        "the grid click drills into the zone"
    );

    // Now at the ZONE level (Elwynn): a click re-expressed through ELWYNN's own rect lands on
    // cell 8801 = the Stormwind City peer child → drills into the city map. This is the path that
    // was a no-op before (wow-re 15b2a8ea Part 2: continent- and zone-level clicks run identical
    // code, only the windowing rect differs).
    s.run("ProcessMapClick(0.5, 0.5)").unwrap();
    assert_eq!(
        s.eval::<(i64, i64)>("return GetCurrentMapContinent(), GetCurrentMapZone()")
            .unwrap(),
        (2, 2),
        "a zone-level click on a city footprint drills into the city"
    );

    // Open ocean at the world level: outside both rects, no selection change.
    s.run("SetMapZoom(0)").unwrap();
    s.run("ProcessMapClick(0.01, 0.99)").unwrap();
    assert_eq!(s.eval::<i64>("return GetCurrentMapContinent()").unwrap(), 0);
}

/// SetMapZoom queues WORLD_MAP_UPDATE through the pending-event queue: nothing fires inside the
/// call (a binding can't re-enter dispatch), the registered frame hears it on the next tick.
#[test]
fn worldmap_update_event_fires_on_next_tick() {
    let mut s = script();
    push_catalog(&mut s);
    // Drain the catalog push's own queued event first.
    s.tick(0.01);
    s.run(
        r#"
        heard = 0
        f = CreateFrame("Frame", "MapListener")
        f:RegisterEvent("WORLD_MAP_UPDATE")
        f:SetScript("OnEvent", function() heard = heard + 1 end)
        SetMapZoom(1)
    "#,
    )
    .unwrap();
    assert_eq!(
        s.eval::<i64>("return heard").unwrap(),
        0,
        "nothing fires inside the SetMapZoom call itself"
    );
    s.tick(0.01);
    assert_eq!(s.eval::<i64>("return heard").unwrap(), 1);
    s.tick(0.01);
    assert_eq!(
        s.eval::<i64>("return heard").unwrap(),
        1,
        "the queue drains — no re-fire"
    );
}

/// The exploration fog: overlays reveal per the pushed bitset — none before any push, the
/// matching subset after (bit 125 = Northshire in the fixture), the full info tuple comes back,
/// and re-pushing the same bitset queues no extra repaint.
#[test]
fn worldmap_overlays_reveal_by_explored_bits() {
    let mut s = script();
    push_catalog(&mut s);
    s.run("SetMapZoom(2, 1)").unwrap(); // the Elwynn zone map (EK's only fixture zone)

    // Nothing explored: the zone shows no overlays (all parchment).
    assert_eq!(s.eval::<i64>("return GetNumMapOverlays()").unwrap(), 0);

    // Bit 125 set (word 3, bit 29): Northshire reveals, Goldshire stays fogged.
    let mut explored = vec![0u32; 64];
    explored[3] = 1 << 29;
    s.set_world_map_explored(explored);
    assert_eq!(s.eval::<i64>("return GetNumMapOverlays()").unwrap(), 1);
    let (tex, w, h, ox, oy) = s
        .eval::<(String, i64, i64, i64, i64)>("return GetMapOverlayInfo(1)")
        .unwrap();
    assert_eq!(tex, "Interface\\WorldMap\\Elwynn\\NORTHSHIREVALLEY");
    assert_eq!((w, h, ox, oy), (200, 176, 490, 200));

    // Both bits: both overlays, catalog order; out-of-range asks read nil.
    let mut explored = vec![0u32; 64];
    explored[3] = (1 << 29) | (1 << 28);
    s.set_world_map_explored(explored);
    assert_eq!(s.eval::<i64>("return GetNumMapOverlays()").unwrap(), 2);
    assert!(s
        .eval::<bool>("return GetMapOverlayInfo(3) == nil")
        .unwrap());

    // At the continent level the overlay family reads empty (fog is a zone-map thing).
    s.run("SetMapZoom(2)").unwrap();
    assert_eq!(s.eval::<i64>("return GetNumMapOverlays()").unwrap(), 0);
}
