//! The movable-frame family — `SetMovable`/`StartMoving`/`StopMovingOrSizing`/`SetUserPlaced`/
//! `SetResizable` (the mechanism and its wow-re addresses are in `script::object::movable`).
//!
//! Every test here drives the PRODUCTION path: the Lua bindings, and the real
//! `mouse_button`/`mouse_move` entry points, so the drag gesture that fires `OnDragStart` is the
//! same one an addon gets.

use super::common::script;
use crate::script::UiScript;

/// A frame wired the canonical way, laid out at (100, 100) 200×80 on an 800×600 screen.
fn movable_panel() -> UiScript {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        starts, stops = 0, 0
        Panel = CreateFrame("Frame", "MovePanel")
        Panel:SetPoint("BOTTOMLEFT", 100, 100)
        Panel:SetSize(200, 80)
        Panel:EnableMouse(true)
        Panel:SetMovable(true)
        Panel:RegisterForDrag("LeftButton")
        Panel:SetScript("OnDragStart", function() starts = starts + 1; this:StartMoving() end)
        Panel:SetScript("OnDragStop",  function() stops = stops + 1; this:StopMovingOrSizing() end)
        "#,
    )
    .unwrap();
    s.resolve();
    s
}

/// The frame's resolved bottom-left corner, after a resolve.
fn corner(s: &mut UiScript) -> (f32, f32) {
    s.resolve();
    let left = s.eval::<f64>("return MovePanel:GetLeft()").unwrap() as f32;
    let bottom = s.eval::<f64>("return MovePanel:GetBottom()").unwrap() as f32;
    (left, bottom)
}

/// The whole point of the family: the four-line addon idiom actually moves the frame, and the
/// position it was dragged to SURVIVES the stop — nothing snaps it back to the `SetPoint` it was
/// born with, and nothing keeps moving it after the button is up.
#[test]
fn the_canonical_addon_idiom_moves_a_frame_and_the_position_survives_the_stop() {
    let mut s = movable_panel();
    assert_eq!(corner(&mut s), (100.0, 100.0), "born where SetPoint put it");

    s.mouse_button(150.0, 140.0, "LeftButton", true); // press inside the panel
    s.mouse_move(152.0, 140.0); // 2 px — under the drag threshold, nothing yet
    assert_eq!(s.eval::<i64>("return starts").unwrap(), 0);
    assert_eq!(corner(&mut s), (100.0, 100.0));

    s.mouse_move(160.0, 140.0); // crosses the threshold ⇒ OnDragStart ⇒ StartMoving
    assert_eq!(s.eval::<i64>("return starts").unwrap(), 1);
    s.mouse_move(260.0, 190.0); // +100, +50 from the grab
    assert_eq!(
        corner(&mut s),
        (200.0, 150.0),
        "the frame follows the cursor delta since StartMoving"
    );

    s.mouse_button(260.0, 190.0, "LeftButton", false); // OnDragStop ⇒ StopMovingOrSizing
    assert_eq!(s.eval::<i64>("return stops").unwrap(), 1);
    assert_eq!(corner(&mut s), (200.0, 150.0), "the position survives");

    // And the move is really over: further cursor motion moves nothing.
    s.mouse_move(500.0, 400.0);
    assert_eq!(corner(&mut s), (200.0, 150.0), "no longer following");
    assert!(s.errors().is_empty(), "{:?}", s.errors());

    // GetPoint reads back the frame's OWN point with the dragged offsets — the reference writes
    // the anchors in place rather than rewriting them, so what an addon saves is what it set.
    let (point, x, y) = s
        .eval::<(String, f64, f64)>("local p, _, _, x, y = MovePanel:GetPoint() return p, x, y")
        .unwrap();
    assert_eq!((point.as_str(), x, y), ("BOTTOMLEFT", 200.0, 150.0));
}

/// A moving frame follows SUCCESSIVE moves, each one applying only that step's delta (the pump
/// re-centers its sample — a bug that integrated from the grab instead would double the second
/// step, and one that forgot to re-center would freeze after the first).
#[test]
fn a_moving_frame_follows_successive_mouse_moves() {
    let mut s = movable_panel();
    s.mouse_button(150.0, 140.0, "LeftButton", true);
    s.mouse_move(160.0, 140.0); // the move that STARTS the drag
    assert_eq!(
        corner(&mut s),
        (100.0, 100.0),
        "the frame follows from where StartMoving was called, so the starting move moves nothing"
    );
    for (step, want) in [
        ((170.0, 140.0), (110.0, 100.0)),
        ((170.0, 160.0), (110.0, 120.0)),
        ((120.0, 160.0), (60.0, 120.0)), // back left of where it started
        ((120.0, 160.0), (60.0, 120.0)), // a zero-delta move changes nothing
    ] {
        s.mouse_move(step.0, step.1);
        assert_eq!(corner(&mut s), want, "after moving to {step:?}");
    }
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// `StartMoving` on a frame that is not `SetMovable(true)` RAISES and moves nothing — the
/// reference's own guard (`0x776700`'s movable-bit test). The error is a real Lua error, so an
/// addon's `pcall` sees it.
#[test]
fn start_moving_on_a_frame_that_is_not_movable_raises_and_moves_nothing() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        Fixed = CreateFrame("Frame", "FixedPanel")
        Fixed:SetPoint("BOTTOMLEFT", 100, 100)
        Fixed:SetSize(200, 80)
        ok, err = pcall(function() Fixed:StartMoving() end)
        "#,
    )
    .unwrap();
    s.resolve();
    assert!(!s.eval::<bool>("return FixedPanel:IsMovable()").unwrap());
    assert!(!s.eval::<bool>("return ok").unwrap(), "StartMoving raised");
    assert!(
        s.eval::<String>("return tostring(err)")
            .unwrap()
            .contains("not movable"),
        "the refusal names why"
    );

    // Nothing is in flight, so the cursor moves nothing.
    s.mouse_move(400.0, 400.0);
    s.resolve();
    let left = s.eval::<f64>("return FixedPanel:GetLeft()").unwrap();
    assert_eq!(left, 100.0, "a refused StartMoving started no move");

    // The same frame, once made movable, moves — proving the guard was the only thing stopping it.
    s.run("FixedPanel:SetMovable(true) FixedPanel:StartMoving()")
        .unwrap();
    s.mouse_move(450.0, 400.0);
    s.resolve();
    assert_eq!(s.eval::<f64>("return FixedPanel:GetLeft()").unwrap(), 150.0);
}

/// `StopMovingOrSizing` with nothing moving is harmless — the double call, the OnDragStop that
/// never had a StartMoving, the addon that also wires it to OnMouseUp. And it only stops the
/// frame that is actually in the drag slot (the reference's `[root+0xcfc] == self` compare), so
/// one frame's stop cannot end another's move.
#[test]
fn stop_moving_or_sizing_is_harmless_with_nothing_moving_and_stops_only_its_own_frame() {
    let mut s = movable_panel();
    s.run(
        r#"
        Other = CreateFrame("Frame", "OtherPanel")
        Other:SetPoint("BOTTOMLEFT", 400, 400)
        Other:SetSize(50, 50)
        MovePanel:StopMovingOrSizing()      -- nothing is moving
        MovePanel:StopMovingOrSizing()      -- twice
        OtherPanel:StopMovingOrSizing()
        "#,
    )
    .unwrap();
    assert!(s.errors().is_empty(), "{:?}", s.errors());
    assert_eq!(corner(&mut s), (100.0, 100.0), "nothing moved");

    // Now start a real move and have the OTHER frame try to stop it.
    s.run("MovePanel:StartMoving()").unwrap();
    s.mouse_move(50.0, 50.0); // cursor_pos was (0,0) ⇒ +50, +50
    assert_eq!(corner(&mut s), (150.0, 150.0));
    s.run("OtherPanel:StopMovingOrSizing()").unwrap();
    s.mouse_move(60.0, 50.0);
    assert_eq!(
        corner(&mut s),
        (160.0, 150.0),
        "another frame's stop does not end this move"
    );
    s.run("MovePanel:StopMovingOrSizing()").unwrap();
    s.mouse_move(200.0, 200.0);
    assert_eq!(corner(&mut s), (160.0, 150.0), "its own stop does");
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// A frame stretched between two anchors — no explicit size at all — moves RIGIDLY and keeps its
/// derived size. This is the case a single-point rewrite gets wrong (it would drop the opposing
/// anchor and leave the frame under-constrained, i.e. gone), and the case that decides between
/// translating one anchor and translating the set.
#[test]
fn a_frame_stretched_between_two_anchors_moves_rigidly() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        Back = CreateFrame("Frame", "StretchBack")
        Back:SetPoint("BOTTOMLEFT", 0, 0); Back:SetSize(800, 600)
        Stretch = CreateFrame("Frame", "StretchPanel", Back)
        Stretch:SetPoint("BOTTOMLEFT", Back, "BOTTOMLEFT", 100, 100)
        Stretch:SetPoint("TOPRIGHT",   Back, "BOTTOMLEFT", 300, 200)
        Stretch:SetMovable(true)
        "#,
    )
    .unwrap();
    s.resolve();
    let size = s
        .eval::<(f64, f64)>("return StretchPanel:GetWidth(), StretchPanel:GetHeight()")
        .unwrap();
    assert_eq!(size, (200.0, 100.0), "size derived from the two anchors");

    s.run("StretchPanel:StartMoving()").unwrap();
    s.mouse_move(30.0, 40.0); // cursor_pos starts at (0,0)
    s.run("StretchPanel:StopMovingOrSizing()").unwrap();
    s.resolve();
    let left = s.eval::<f64>("return StretchPanel:GetLeft()").unwrap();
    let bottom = s.eval::<f64>("return StretchPanel:GetBottom()").unwrap();
    let size_after = s
        .eval::<(f64, f64)>("return StretchPanel:GetWidth(), StretchPanel:GetHeight()")
        .unwrap();
    assert_eq!((left, bottom), (130.0, 140.0), "both anchors translated");
    assert_eq!(size_after, (200.0, 100.0), "and the frame kept its size");
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// A SCALED frame moves with the cursor 1:1 on screen — the anchor offsets it writes are in local
/// units, so they carry the inverse of the frame's own scale (`geo_768710`'s `dx/scale`).
#[test]
fn a_scaled_frame_tracks_the_cursor_one_to_one_on_screen() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        Scaled = CreateFrame("Frame", "ScaledPanel")
        Scaled:SetPoint("BOTTOMLEFT", 100, 100)
        Scaled:SetSize(100, 100)
        Scaled:SetScale(2)
        Scaled:SetMovable(true)
        Scaled:StartMoving()
        "#,
    )
    .unwrap();
    s.resolve();
    // Local 100 at scale 2 ⇒ 200 screen px.
    assert_eq!(
        s.eval::<f64>("return ScaledPanel:GetLeft()").unwrap(),
        100.0
    );
    s.mouse_move(40.0, 0.0);
    s.resolve();
    // GetLeft reports LOCAL units: 40 screen px is 20 local, on top of the local 100.
    assert_eq!(
        s.eval::<f64>("return ScaledPanel:GetLeft()").unwrap(),
        120.0
    );
    let x = s
        .eval::<f64>("local _, _, _, x = ScaledPanel:GetPoint() return x")
        .unwrap();
    assert_eq!(x, 120.0, "the anchor offset is local too");
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// The flags: default off, round-trip through their setters, and `SetUserPlaced`'s guard — the
/// reference refuses it unless the frame is movable OR resizable (`776adb: test ah,0x3`).
/// `StartMoving` sets the bit itself, which is the reference's drag-start doing it, not us.
#[test]
fn the_three_flags_default_off_round_trip_and_user_placed_is_guarded() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        F = CreateFrame("Frame", "FlagPanel")
        F:SetPoint("BOTTOMLEFT", 10, 10); F:SetSize(50, 50)
        "#,
    )
    .unwrap();
    let all = |s: &mut UiScript| {
        s.eval::<(bool, bool, bool)>(
            "return FlagPanel:IsMovable(), FlagPanel:IsResizable(), FlagPanel:IsUserPlaced()",
        )
        .unwrap()
    };
    assert_eq!(
        all(&mut s),
        (false, false, false),
        "no frame is born flagged"
    );

    // SetUserPlaced before either flag: refused, exactly like the reference.
    assert!(
        !s.eval::<bool>("return (pcall(function() FlagPanel:SetUserPlaced(true) end))")
            .unwrap(),
        "SetUserPlaced on a frame that is neither movable nor resizable raises"
    );
    assert!(!s.eval::<bool>("return FlagPanel:IsUserPlaced()").unwrap());

    s.run("FlagPanel:SetResizable(true) FlagPanel:SetUserPlaced(true)")
        .unwrap();
    assert_eq!(
        all(&mut s),
        (false, true, true),
        "resizable satisfies it too"
    );
    s.run("FlagPanel:SetUserPlaced(false) FlagPanel:SetResizable(false) FlagPanel:SetMovable(1)")
        .unwrap();
    assert_eq!(
        all(&mut s),
        (true, false, false),
        "truthy 1 sets, false clears"
    );

    // The drag start sets userPlaced itself (`0x7652b0`).
    s.resolve();
    s.run("FlagPanel:StartMoving()").unwrap();
    assert!(
        s.eval::<bool>("return FlagPanel:IsUserPlaced()").unwrap(),
        "StartMoving sets the userPlaced bit"
    );
    s.run("FlagPanel:StopMovingOrSizing()").unwrap();
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// `movable="true"` / `resizable="true"` in XML land on the real methods — they were a warn-once
/// gap in the loader until this family existed, which left every reference window authored
/// movable undraggable while `SetMovable` worked fine from Lua.
#[test]
fn the_xml_movable_and_resizable_attributes_reach_the_methods() {
    let s = script();
    let doc = crate::framexml::parse(
        r#"<Ui>
             <Frame name="XmlMovable" movable="true" resizable="true">
               <Size><AbsDimension x="100" y="50"/></Size>
               <Anchors>
                 <Anchor point="BOTTOMLEFT"><Offset><AbsDimension x="10" y="10"/></Offset></Anchor>
               </Anchors>
             </Frame>
           </Ui>"#,
    )
    .expect("valid FrameXML");
    let report = crate::loader::load(&s, &doc, &|_| None);
    assert!(
        !report
            .warnings
            .iter()
            .any(|w| w.contains("SetMovable") || w.contains("SetResizable")),
        "no gap warning any more: {:?}",
        report.warnings
    );
    assert_eq!(
        s.eval::<(bool, bool)>("return XmlMovable:IsMovable(), XmlMovable:IsResizable()")
            .unwrap(),
        (true, true)
    );
}
