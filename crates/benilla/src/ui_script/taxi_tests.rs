//! The shipped taxi-map window driven end-to-end, engine-only (no Bevy): the real
//! `assets/ui/TaxiFrame.xml` fed a synthetic two-node snapshot (the [`crate::ui_taxi`] feed's exact
//! output shape). Covers what only a runtime load exercises: the Lua parses and every referenced
//! global resolves (including the static node-button pool and the runtime-created route-line
//! textures — TaxiFrame.xml's own header note on both), `TAXIMAP_OPENED` shows the window with the
//! flight master's name and paints the node buttons at their pushed positions, a click on a node
//! drains through `TakeTaxiNode`, and `TAXIMAP_CLOSED` hides it.

use benilla_ui::script::{ScriptValue, TaxiNodeType, TaxiUiNode, TaxiUiState, UiScript};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error.
fn load_xml(s: &UiScript, file: &str) -> usize {
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
    report.frames
}

/// Load the taxi window + its deps into a fresh script, screen sized.
fn taxi_script() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml"); // TaxiNodeOnButtonEnter's tooltip + SetTooltipMoney
    load_xml(&s, "ErrorsFrame.xml"); // BenillaErrorsFrame_AddMessage — DrawOneHopLines' refusal
    load_xml(&s, "TaxiFrame.xml");
    s
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

    assert!(!s
        .eval::<bool>("return BenillaTaxiFrame:IsVisible()")
        .unwrap());

    s.set_taxi(Some(menu()));
    s.fire_event(
        "TAXIMAP_OPENED",
        vec![ScriptValue::Str("Dungar Longdrink".into())],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    assert!(s
        .eval::<bool>("return BenillaTaxiFrame:IsVisible()")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return BenillaTaxiNameText:GetText()")
            .unwrap(),
        "Dungar Longdrink"
    );

    // The two pushed nodes show; the pool's remainder (slot 3 onward) stays hidden.
    assert!(s
        .eval::<bool>("return BenillaTaxiButton1:IsVisible()")
        .unwrap());
    assert!(s
        .eval::<bool>("return BenillaTaxiButton2:IsVisible()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return BenillaTaxiButton3:IsVisible()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return BenillaTaxiButton50:IsVisible()")
        .unwrap());

    // A click on node 2 (Sentinel Hill) drains through TakeTaxiNode.
    s.resolve();
    let (cx, cy) = s
        .eval::<(f32, f32)>("return BenillaTaxiButton2:GetCenter()")
        .unwrap();
    s.mouse_button(cx, cy, "LeftButton", true);
    s.mouse_button(cx, cy, "LeftButton", false);
    assert_eq!(s.take_taxi_node(), vec![2]);
    assert!(s.take_taxi_node().is_empty(), "drained");

    // TAXIMAP_CLOSED hides the window.
    s.set_taxi(None);
    s.fire_event("TAXIMAP_CLOSED", vec![]);
    assert!(!s
        .eval::<bool>("return BenillaTaxiFrame:IsVisible()")
        .unwrap());
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
    s.fire_event(
        "TAXIMAP_OPENED",
        vec![ScriptValue::Str("Dungar Longdrink".into())],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // OnShow ran DrawOneHopLines, which found zero single-hop nodes: the red error line posted
    // and the window hid itself again — the reference's own reaction to a dead-end flight point.
    assert!(!s
        .eval::<bool>("return BenillaTaxiFrame:IsVisible()")
        .unwrap());
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
        "ERR_TAXINOPATHS posted to BenillaErrorsFrame: {quads:?}"
    );
}

/// The close button's own click path: `CloseTaxiMap()` queues the intent this engine drains, and
/// the window hides immediately (no one-frame lag) — the Gossip/Trainer close-button precedent.
#[test]
fn close_button_queues_the_intent_and_hides() {
    let mut s = taxi_script();
    s.set_taxi(Some(menu()));
    s.fire_event(
        "TAXIMAP_OPENED",
        vec![ScriptValue::Str("Dungar Longdrink".into())],
    );
    assert!(s
        .eval::<bool>("return BenillaTaxiFrame:IsVisible()")
        .unwrap());

    s.run("BenillaTaxiCloseButton_OnClick()").unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(s.take_taxi_close());
    assert!(!s
        .eval::<bool>("return BenillaTaxiFrame:IsVisible()")
        .unwrap());
}
