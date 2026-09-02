//! The shipped taxi-map window driven end-to-end, engine-only (no Bevy): the real
//! `assets/ui/TaxiFrame.xml` fed a synthetic two-node snapshot (the [`crate::ui_taxi`] feed's exact
//! output shape). Covers what only a runtime load exercises: the Lua parses and every referenced
//! global resolves (including the static node-button pool and the runtime-created route-line
//! textures — TaxiFrame.xml's own header note on both), `TAXIMAP_OPENED` shows the window with the
//! flight master's name and paints the node buttons at their pushed positions, a click on a node
//! drains through `TakeTaxiNode`, and `TAXIMAP_CLOSED` hides it.

use benilla_ui::script::{ScriptValue, TaxiNodeType, TaxiUiNode, TaxiUiState, UiScript};

use super::test_ui::load_ui as load_xml;

/// Load the taxi window + its deps into a fresh script, screen sized.
fn taxi_script() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml"); // TaxiNodeOnButtonEnter's tooltip + SetTooltipMoney
    load_xml(&s, "Interface\\FrameXML\\UIErrorsFrame.xml"); // BenillaErrorsFrame_AddMessage — DrawOneHopLines' refusal
                                                            // Three the reference's own TaxiFrame leans on that our transcription did not:
                                                            //   · GlobalStrings — `ERR_TAXINOPATHS` is a GlobalString, and `AddMessage(nil)` draws an
                                                            //     empty line rather than raising, so its absence is silent.
                                                            //   · UIPanelTemplates (.lua then .xml) — `TaxiCloseButton` inherits `UIPanelCloseButton`,
                                                            //     which lives there and NOT in our UiPanels.xml. Without it the close button loads as a
                                                            //     bare Button with no handler and a click does nothing at all.
    load_xml(&s, "Interface\\FrameXML\\GlobalStrings.lua");
    load_xml(&s, "Interface\\FrameXML\\UIPanelTemplates.lua");
    load_xml(&s, "Interface\\FrameXML\\UIPanelTemplates.xml");
    load_xml(&s, "Interface\\FrameXML\\TaxiFrame.xml");
    s
}

/// Seat the flight master as the `"npc"` unit. Stock `TaxiFrame_OnEvent` fills its label from
/// `UnitName("npc")` (TaxiFrame.lua:27), where our deleted transcription read the `TAXIMAP_OPENED`
/// argument — so a fixture that only fires the event leaves the reference's label blank.
fn seat_flight_master(s: &mut UiScript, name: &str) {
    let npc = benilla_ui::script::UnitState {
        name: Some(name.into()),
        ..Default::default()
    };
    s.set_unit("npc", Some(npc));
}

/// A two-node snapshot: Stormwind (Current) and the verified Sentinel Hill hop (Reachable, 110
/// copper, one route segment) — [`crate::ui_taxi::build_nodes`]'s exact real-data shape.
fn menu() -> TaxiUiState {
    TaxiUiState {
        art: "Interface\\TaxiFrame\\TAXIMAP0".into(),
        nodes: vec![
            TaxiUiNode {
                name: "Stormwind, Elwynn".into(),
                node_type: TaxiNodeType::Current,
                pos: (0.43, 0.33),
                cost: 0,
                routes: vec![],
            },
            TaxiUiNode {
                name: "Sentinel Hill, Westfall".into(),
                node_type: TaxiNodeType::Reachable,
                pos: (0.41, 0.25),
                cost: 110,
                routes: vec![[0.43, 0.33, 0.41, 0.25]],
            },
        ],
    }
}

/// The whole taxi window minus Bevy: hidden by default, `TAXIMAP_OPENED` shows it with the flight
/// master's name (the event-arg deviation TaxiFrame.xml's header note flags), paints exactly the
/// pushed node buttons (the rest of the 50-slot static pool stays hidden — the CreateFrame/template
/// deviation), a click on the reachable node drains through `TakeTaxiNode`, and `TAXIMAP_CLOSED`
/// hides it again.
#[test]
fn shipped_taxi_frame_drives_end_to_end() {
    let mut s = taxi_script();

    assert!(!s.eval::<bool>("return TaxiFrame:IsVisible()").unwrap());

    s.set_taxi(Some(menu()));
    seat_flight_master(&mut s, "Dungar Longdrink");
    s.fire_event(
        "TAXIMAP_OPENED",
        vec![ScriptValue::Str("Dungar Longdrink".into())],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    assert!(s.eval::<bool>("return TaxiFrame:IsVisible()").unwrap());
    assert_eq!(
        // The reference names the flight-master label `TaxiMerchant` and fills it from
        // `UnitName("npc")` (TaxiFrame.lua:27) — NOT from the event's argument, which is what our
        // transcription's `TaxiNameText` read. So the fixture seats the npc unit above.
        s.eval::<String>("return TaxiMerchant:GetText()").unwrap(),
        "Dungar Longdrink"
    );

    // The two pushed nodes show, and there is no third button AT ALL — the reference builds its
    // node buttons on demand, one `CreateFrame("Button", "TaxiButton"..i, TaxiRouteMap,
    // "TaxiButtonTemplate")` per node (TaxiFrame.lua:39), where our transcription pre-made a fixed
    // pool and hid the tail. Same thing on screen, a different mechanism behind it — so the
    // expectation moves from "hidden" to "absent".
    assert!(s.eval::<bool>("return TaxiButton1:IsVisible()").unwrap());
    assert!(s.eval::<bool>("return TaxiButton2:IsVisible()").unwrap());
    assert!(s.eval::<bool>("return TaxiButton3 == nil").unwrap());
    assert!(s.eval::<bool>("return TaxiButton50 == nil").unwrap());

    // A click on node 2 (Sentinel Hill) drains through TakeTaxiNode.
    s.resolve();
    let (cx, cy) = s
        .eval::<(f32, f32)>("return TaxiButton2:GetCenter()")
        .unwrap();
    s.mouse_button(cx, cy, "LeftButton", true);
    s.mouse_button(cx, cy, "LeftButton", false);
    assert_eq!(s.take_taxi_node(), vec![2]);
    assert!(s.take_taxi_node().is_empty(), "drained");

    // TAXIMAP_CLOSED hides the window.
    s.set_taxi(None);
    s.fire_event("TAXIMAP_CLOSED", vec![]);
    assert!(!s.eval::<bool>("return TaxiFrame:IsVisible()").unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The "no single-hop destination" refusal (ref `DrawOneHopLines`, fired from `OnShow`): a map
/// with only the `Current` node (no `Reachable` neighbor at all) hits `numSingleHops == 0` and
/// calls `BenillaErrorsFrame_AddMessage` (TaxiFrame.xml's deviation 3 from the reference's
/// `UIErrorsFrame:AddMessage`) then hides the window — verified end-to-end since this is the one
/// call this engine has no precedent for outside this window (`ErrorsFrame.xml`'s own Lua seam,
/// not yet exercised from another shipped window's script).
#[test]
fn no_single_hop_destination_posts_the_error_and_closes() {
    let mut s = taxi_script();
    s.set_taxi(Some(TaxiUiState {
        art: "Interface\\TaxiFrame\\TAXIMAP0".into(),
        nodes: vec![TaxiUiNode {
            name: "Stormwind, Elwynn".into(),
            node_type: TaxiNodeType::Current,
            pos: (0.43, 0.33),
            cost: 0,
            routes: vec![],
        }],
    }));
    seat_flight_master(&mut s, "Dungar Longdrink");
    s.fire_event(
        "TAXIMAP_OPENED",
        vec![ScriptValue::Str("Dungar Longdrink".into())],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // OnShow ran DrawOneHopLines, which found zero single-hop nodes: the red error line posted
    // and the window hid itself again — the reference's own reaction to a dead-end flight point.
    assert!(!s.eval::<bool>("return TaxiFrame:IsVisible()").unwrap());
    s.resolve();
    let quads = s.extract();
    let has_refusal = quads.iter().any(|q| match &q.content {
        benilla_ui::script::QuadContent::Text { text: Some(t), .. } => {
            t.contains("don\u{2019}t know any flight locations")
        }
        _ => false,
    });
    assert!(
        has_refusal,
        "ERR_TAXINOPATHS posted to UIErrorsFrame: {quads:?}"
    );
}

/// The close button's own click path: `CloseTaxiMap()` queues the intent this engine drains, and
/// the window hides immediately (no one-frame lag) — the Gossip/Trainer close-button precedent.
#[test]
fn close_button_queues_the_intent_and_hides() {
    let mut s = taxi_script();
    s.set_taxi(Some(menu()));
    seat_flight_master(&mut s, "Dungar Longdrink");
    s.fire_event(
        "TAXIMAP_OPENED",
        vec![ScriptValue::Str("Dungar Longdrink".into())],
    );
    assert!(s.eval::<bool>("return TaxiFrame:IsVisible()").unwrap());

    // The reference's close button is a plain `UIPanelCloseButton` (TaxiFrame.xml:133) — it has no
    // handler of its own, so the click goes through the template's, which hides the parent panel.
    // Ours carried a named `BenillaTaxiCloseButton_OnClick`; that went with the file.
    s.run("TaxiCloseButton:Click()").unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(s.take_taxi_close());
    assert!(!s.eval::<bool>("return TaxiFrame:IsVisible()").unwrap());
}
