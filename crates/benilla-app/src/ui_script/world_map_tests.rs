//! The world map's **POI layer** — the icons `GetNumMapLandmarks`/`GetMapLandmarkInfo` feed into
//! `WorldMapFrame_Update`'s frame pool. Today the only landmark is the guard's directions marker
//! (`SMSG_GOSSIP_POI` → [`crate::poi_marker`]); the `AreaPOI.dbc` rows are 0203's deferred slice
//! and arrive through the same list, so these tests pin the *pool*, not the marker.
//!
//! Driven through the shipped `assets/ui/WorldMapFrame.xml` in a bare engine (no Bevy) — the
//! panel-test idiom: push host state, run the repaint, read the frames back.

use benilla_ui::script::{UiScript, WorldMapLandmarkView};

/// The map plus everything its OnLoad touches, in the shipped list's own order.
fn harness() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for file in [
        "Fonts.xml",
        "MoneyFrame.xml",
        "UiPanels.xml",
        r"Interface\FrameXML\UIPanelTemplates.lua",
        r"Interface\FrameXML\UIPanelTemplates.xml",
        "GameTooltip.xml",
        "Interface\\FrameXML\\UIDropDownMenu.xml", // the map's continent/zone pickers initialize into it at OnLoad
        "ScrollTemplates.xml",
        // The blip templates, which WorldMapFrame.xml instantiates with inherits=. Not
        // optional: an unknown template is a loader WARNING, not an error, so leaving this
        // out loads clean and then hands every blip a sizeless, anchorless frame.
        "WorldMapFrameTemplates.xml",
        "WorldMapFrame.xml",
    ] {
        // `test_ui::load_ui`, not a local read: a manifest entry carrying a path separator is the
        // REFERENCE's own file and must come off the player's chain, which
        // `std::fs::read_to_string` under `assets/ui` cannot do — it goes looking for
        // `assets/ui/Interface/FrameXML/...` and fails. The shared loader resolves both shapes, and
        // its own doc already records this consolidation happening once before. Hand-rolling it
        // here is what made this kit break the moment a file it loads migrated (1751).
        super::test_ui::load_ui(&s, file);
    }
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    s
}

fn landmark(name: &str, icon: u32, uv: (f32, f32)) -> WorldMapLandmarkView {
    WorldMapLandmarkView {
        name: name.into(),
        description: String::new(),
        texture_index: icon,
        uv,
    }
}

/// A pushed landmark becomes a shown POI frame at its UV, wearing its own cell of the 8×8
/// `POIIcons` atlas. Icon **6** is `ICON_POI_REDFLAG` — the red flag every 5875-era
/// `points_of_interest` row ships, so this is the guard-directions case exactly.
#[test]
fn a_landmark_draws_its_poi_icon_at_its_map_position() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = harness();
    s.set_world_map_landmarks(vec![landmark("Stormwind Warrior Trainer", 6, (0.25, 0.5))]);
    s.run("WorldMapFrame_Update()").unwrap();

    assert_eq!(
        s.eval::<i64>("return GetNumMapLandmarks()").unwrap(),
        1,
        "the host reports the one landmark"
    );
    assert!(
        s.eval::<bool>("return WorldMapFramePOI1:IsShown()")
            .unwrap(),
        "the pool grew a frame for it and showed it"
    );

    // Cell 6 of the 8×8 grid = column 6, row 0.
    // `GetTexCoord` answers EIGHT (UL, LL, UR, LR as x,y pairs) since 1840; the old
    // `(l, r, t, b)` rect is `ULx, URx, ULy, LLy` — positions 1, 5, 2, 4.
    let (l, t, _, b, r, ..): (f32, f32, f32, f32, f32, f32, f32, f32) = s
        .eval("return WorldMapFramePOI1Texture:GetTexCoord()")
        .unwrap();
    assert_eq!(
        (l, r, t, b),
        (0.75, 0.875, 0.0, 0.125),
        "POIIcons cell 6 — the red flag"
    );

    // Seated at UV × the 1002×668 detail frame, from its TOPLEFT (y down).
    let (x, y): (f32, f32) = s
        .eval(
            "local _, _, _, ox, oy = WorldMapFramePOI1:GetPoint(1) \
             return ox, oy",
        )
        .unwrap();
    assert_eq!((x, y), (0.25 * 1002.0, -0.5 * 668.0));
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The pool grows and is reused, never shrunk: a busier map's tail parks hidden rather than being
/// destroyed, and the same frame is re-seated when the list shrinks back.
#[test]
fn the_poi_pool_grows_and_parks_its_tail() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = harness();
    s.set_world_map_landmarks(vec![
        landmark("The Bank", 6, (0.1, 0.1)),
        landmark("The Inn", 6, (0.2, 0.2)),
        landmark("The Auction House", 6, (0.3, 0.3)),
    ]);
    s.run("WorldMapFrame_Update()").unwrap();
    assert_eq!(s.eval::<i64>("return NUM_WORLDMAP_POIS").unwrap(), 3);
    assert!(s
        .eval::<bool>("return WorldMapFramePOI3:IsShown()")
        .unwrap());

    s.set_world_map_landmarks(vec![landmark("The Inn", 6, (0.2, 0.2))]);
    s.run("WorldMapFrame_Update()").unwrap();
    assert_eq!(
        s.eval::<i64>("return NUM_WORLDMAP_POIS").unwrap(),
        3,
        "the pool never shrinks"
    );
    assert!(s
        .eval::<bool>("return WorldMapFramePOI1:IsShown()")
        .unwrap());
    assert!(
        !s.eval::<bool>("return WorldMapFramePOI2:IsShown()")
            .unwrap(),
        "slot 2 parked hidden"
    );
    assert!(!s
        .eval::<bool>("return WorldMapFramePOI3:IsShown()")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return WorldMapFramePOI1.name").unwrap(),
        "The Inn",
        "slot 1 was re-seated, not left on the stale landmark"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Hovering a POI names it. The description line only appears on landmarks that carry one — the
/// guard's directions never do, a battleground node's "In Conflict" would.
#[test]
fn hovering_a_poi_names_it_and_adds_a_status_line_only_when_there_is_one() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = harness();
    s.set_world_map_landmarks(vec![landmark("Lion's Pride Inn", 6, (0.5, 0.5))]);
    s.run("WorldMapFrame_Update()").unwrap();
    s.run("this = WorldMapFramePOI1 this:GetScript(\"OnEnter\")() this = nil")
        .unwrap();
    assert_eq!(
        s.eval::<String>("return GameTooltipTextLeft1:GetText()")
            .unwrap(),
        "Lion's Pride Inn"
    );
    assert!(
        s.eval::<bool>("return GameTooltipTextLeft2:GetText() == nil or GameTooltipTextLeft2:GetText() == \"\"")
            .unwrap(),
        "no description → no second line"
    );

    let mut with_status = landmark("Stables", 6, (0.5, 0.5));
    with_status.description = "In Conflict".into();
    s.set_world_map_landmarks(vec![with_status]);
    s.run("WorldMapFrame_Update()").unwrap();
    s.run("this = WorldMapFramePOI1 this:GetScript(\"OnEnter\")() this = nil")
        .unwrap();
    assert_eq!(
        s.eval::<String>("return GameTooltipTextLeft2:GetText()")
            .unwrap(),
        "In Conflict"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// `GetMapLandmarkInfo` hands back the reference's own five values, and a landmark with no
/// description answers **nil** there rather than an empty string (VERIFIED, `0x4a8740`) — the
/// guard's marker never carries one.
#[test]
fn the_landmark_getter_returns_the_references_five_values() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = harness();
    let mut with_status = landmark("Stables", 9, (0.25, 0.75));
    with_status.description = "In Conflict".into();
    s.set_world_map_landmarks(vec![landmark("Woo Ping", 6, (0.5, 0.5)), with_status]);

    let (name, desc_is_nil, icon, x, y): (String, bool, i64, f32, f32) = s
        .eval("local n, d, t, x, y = GetMapLandmarkInfo(1) return n, d == nil, t, x, y")
        .unwrap();
    assert_eq!(
        (name.as_str(), desc_is_nil, icon, x, y),
        ("Woo Ping", true, 6, 0.5, 0.5)
    );

    let desc: String = s
        .eval("local _, d = GetMapLandmarkInfo(2) return d")
        .unwrap();
    assert_eq!(
        desc, "In Conflict",
        "a landmark that HAS one still answers it"
    );
    assert!(
        s.eval::<bool>("return GetMapLandmarkInfo(3) == nil")
            .unwrap(),
        "past the end is nil"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// No landmarks → no frames, and the repaint stays quiet. This is the common case by far: the map
/// is open far more often than a guard has just given directions.
#[test]
fn no_landmarks_draws_nothing() {
    let _data = benilla_formats::wow_data_or_skip!();
    let s = harness();
    s.run("WorldMapFrame_Update()").unwrap();
    assert_eq!(s.eval::<i64>("return GetNumMapLandmarks()").unwrap(), 0);
    assert_eq!(s.eval::<i64>("return NUM_WORLDMAP_POIS").unwrap(), 0);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **B320 — party members are on the map.** One frame per `party1..4` slot, placed from
/// `GetPlayerMapPosition` through the same `(0,0)`-means-hide law the player and corpse blips
/// already obey, and hidden again the moment a slot stops answering.
///
/// The seating is the reference's: `CENTER` against `WorldMapDetailFrame`'s `TOPLEFT`, x scaled by
/// its width and y by **minus** its height — UV v runs down the sheet where frame y runs up, and
/// getting that sign wrong mirrors every blip about the top edge without failing anything else.
#[test]
fn party_blips_sit_at_their_map_positions_and_hide_when_absent() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = harness();
    // `IsShown`, not `IsVisible`: the map itself is closed in this harness (the POI test's own
    // idiom), so every child would read invisible through its hidden ancestor.
    let shown = |s: &UiScript, i: u32| -> bool {
        s.eval::<bool>(&format!("return WorldMapParty{i}:IsShown()"))
            .unwrap()
    };

    // Nobody in the party: every slot answers the hide sentinel.
    s.run("WorldMapParty_Update()").unwrap();
    for i in 1..=4 {
        assert!(!shown(&s, i), "slot {i} hides while the party is empty");
    }

    // Two members on the displayed map, one of them off it (the None the app pushes for a member
    // whose position projects outside the rect, or whom we hold no position for at all).
    s.set_world_map_feed(
        None,
        Some((0.5, 0.5)),
        0.0,
        None,
        vec![Some((0.25, 0.75)), None, Some((0.5, 0.125))],
    );
    s.run("WorldMapParty_Update()").unwrap();
    let diag = s
        .eval::<String>(
            r#"return "p1="..tostring(GetPlayerMapPosition("party1")).." p3="..tostring(GetPlayerMapPosition("party3"))"#,
        )
        .unwrap();
    assert!(
        shown(&s, 1) && shown(&s, 3),
        "the two placed members show ({diag})"
    );
    assert!(!shown(&s, 2), "a member with no position stays hidden");
    assert!(!shown(&s, 4), "a slot past the roster's end stays hidden");

    // The seating, read back against the detail frame's own box.
    let (w, h) = s
        .eval::<(f64, f64)>(
            "return WorldMapDetailFrame:GetWidth(), WorldMapDetailFrame:GetHeight()",
        )
        .unwrap();
    let (blip_x, blip_y) = s
        .eval::<(f64, f64)>("return WorldMapParty1:GetCenter()")
        .unwrap();
    let (sheet_left, sheet_top) = s
        .eval::<(f64, f64)>("return WorldMapDetailFrame:GetLeft(), WorldMapDetailFrame:GetTop()")
        .unwrap();
    assert!(
        (blip_x - (sheet_left + 0.25 * w)).abs() < 0.5,
        "u scales by the sheet's width"
    );
    assert!(
        (blip_y - (sheet_top - 0.75 * h)).abs() < 0.5,
        "v runs DOWN from the sheet's top"
    );

    // The party breaks up: the blips go with it.
    s.set_world_map_feed(None, Some((0.5, 0.5)), 0.0, None, Vec::new());
    s.run("WorldMapParty_Update()").unwrap();
    for i in 1..=4 {
        assert!(!shown(&s, i), "slot {i} hides when the party is gone");
    }
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **Opening the map must not hide the map's own furniture.** (Director report: the world showed
/// through at the left and right of the sheet, where black bars had always been.)
///
/// `WorldMapFrame` is an `area = "full"` panel, so `ShowUIPanel` routes it to `SetFullScreenFrame`
/// — which hides `UIParent` (decision 1734 restored that line). Anything the map needs *while it
/// is up* therefore must not hang off `UIParent`, and the reference is emphatic about it in three
/// places: `BlackoutWorld` is a texture inside `WorldMapFrame` itself, `WorldMapTooltip` is
/// `parent="WorldMapFrame"` where the shared `GameTooltip` is `parent="UIParent"`, and the
/// dropdown lists carry `toplevel="true"` with no parent at all.
#[test]
fn the_maps_own_furniture_survives_the_hide_that_showing_it_performs() {
    let _data = benilla_formats::wow_data_or_skip!();
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1600.0, 900.0);
    let failures = super::load_default_ui(&s);
    assert!(failures.is_empty(), "manifest load errors: {failures:#?}");
    s.resolve();

    let visible = |s: &UiScript, f: &str| {
        s.eval::<i64>(&format!(
            "local x = getglobal('{f}') if not x then return -1 end return x:IsVisible() and 1 or 0"
        ))
        .unwrap()
    };

    s.run("ShowUIPanel(WorldMapFrame)").unwrap();
    s.resolve();

    assert_eq!(visible(&s, "WorldMapFrame"), 1, "the map itself is up");
    assert_eq!(
        visible(&s, "UIParent"),
        0,
        "and it took the screen from the HUD"
    );
    assert_eq!(
        visible(&s, "BlackoutWorld"),
        1,
        "the blackout is up WITH it — it is what stops the 3D world showing through the margins \
         beside the 4:3 sheet, and a blackout that hides when the map opens is no blackout"
    );

    // The map's own header carries two `UIDropDownMenu` pickers, and every menu in the game is
    // seated on the shared `DropDownList1`. `ToggleDropDownMenu` ends in `listFrame:Show()`, so
    // the property that decides whether a continent list can ever appear over the map is whether
    // that Show survives — which is what a `parent="UIParent"` took away. Asserted at the Show
    // rather than through `ToggleDropDownMenu` on purpose: the toggle bails early on an empty
    // list (`numButtons == 0`), so driving it here would pass on a host with no continents
    // pushed and prove nothing about the seat.
    s.run("DropDownList1:Show()").unwrap();
    s.resolve();
    assert_eq!(
        visible(&s, "DropDownList1"),
        1,
        "the shared dropdown list can open over a full-screen panel — the continent and zone \
         pickers in the map's own header have no other seat"
    );

    s.run("CloseDropDownMenus()").unwrap();
    s.run("HideUIPanel(WorldMapFrame)").unwrap();
    s.resolve();
    assert_eq!(visible(&s, "UIParent"), 1, "and the HUD comes back after");
    assert_eq!(visible(&s, "BlackoutWorld"), 0, "with the blackout down");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
