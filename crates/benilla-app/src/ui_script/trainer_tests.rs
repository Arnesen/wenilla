//! The shipped **trainer window** driven end-to-end, engine-only (no Bevy): the real
//! `assets/ui/TrainerFrame.xml` — a client-sorted, collapsible **skill-line tree** with a **dropdown**
//! state filter and a draggable **scroll bar** (decisions 0247/0251) — loaded behind its deps
//! (`UiPanels.xml` + `UIDropDownMenu.xml` + `ScrollTemplates.xml` + `MerchantFrame.xml` for the
//! `BenillaMoney_*` coin helpers) and fed a synthetic service list. Covers what only a runtime load
//! exercises: the Lua parses and every referenced global resolves, the tree renders interleaved
//! header/service rows, a header click folds its group, the dropdown filter hides a state, the wheel
//! scrolls the list, the NPC name rides `arg1` into the title, the byte-exact GlobalStrings render, the
//! Train button gates on available-and-affordable, and the buy queues the row's spell id.

use benilla_ui::script::{
    ExtractedQuad, QuadContent, ScriptValue, SoundRequest, TrainerAbilityReq, TrainerService,
    TrainerServiceCategory, TrainerSkillReq, TrainerState, UiScript,
};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error and returning the
/// frame count it materialized.
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

/// Load the trainer window + all its deps into a fresh script, screen sized, with every state filter
/// ON (the XML defaults "Already Known" off — the tests want the full tree, deterministic indices).
fn trainer_script() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml"); // TOOLTIP_DEFAULT_* (the kit's MenuBackdrop), app order
    load_xml(&s, "UIDropDownMenu.xml"); // the filter dropdown's kit
    load_xml(&s, "ScrollTemplates.xml"); // the faux-scroll bar kit
    load_xml(&s, "MerchantFrame.xml"); // BenillaMoney_Set/_Clear/_SetColor live here
    load_xml(&s, "TrainerFrame.xml");
    s.run(
        "SetTrainerServiceTypeFilter('available',1) \
         SetTrainerServiceTypeFilter('unavailable',1) \
         SetTrainerServiceTypeFilter('used',1)",
    )
    .unwrap();
    s
}

/// Whether any rendered text quad carries `color` (within a small tolerance) — used to spot the
/// reddened cost coins, whose `(1.0, 0.1, 0.1)` is distinct from the unavailable row's `(0.9, 0, 0)`.
fn has_text_color(quads: &[ExtractedQuad], color: [f32; 3]) -> bool {
    quads.iter().any(|q| match &q.content {
        QuadContent::Text { color: Some(c), .. } => (0..3).all(|i| (c[i] - color[i]).abs() < 0.02),
        _ => false,
    })
}

/// Whether the first text quad containing `needle` renders in `color` (small tolerance) — used to
/// assert the selected row's white name vs. a state colour.
fn text_has_color(quads: &[ExtractedQuad], needle: &str, color: [f32; 3]) -> bool {
    quads.iter().any(|q| match &q.content {
        QuadContent::Text {
            text: Some(t),
            color: Some(c),
            ..
        } => t.contains(needle) && (0..3).all(|i| (c[i] - color[i]).abs() < 0.02),
        _ => false,
    })
}

/// The centre of the first text quad whose text contains `needle` — a point to aim the wheel at.
fn text_center(quads: &[ExtractedQuad], needle: &str) -> (f32, f32) {
    let r = quads
        .iter()
        .find_map(|q| match &q.content {
            QuadContent::Text { text: Some(t), .. } if t.contains(needle) => q.rect,
            _ => None,
        })
        .unwrap_or_else(|| panic!("no text quad containing {needle:?}"));
    ((r.left + r.right) * 0.5, (r.bottom + r.top) * 0.5)
}

/// One service in a named skill line, spelling out every field so the intent is legible at the call
/// site.
#[allow(clippy::too_many_arguments)]
fn service(
    spell_id: u32,
    name: &str,
    category: TrainerServiceCategory,
    cost: u32,
    level_req: u32,
    skill_line: u32,
    line_name: &str,
    skill_req: Option<TrainerSkillReq>,
    ability_reqs: Vec<TrainerAbilityReq>,
) -> TrainerService {
    TrainerService {
        spell_id,
        name: Some(name.into()),
        subtext: None,
        texture: Some("Interface\\Icons\\INV_Sword_04".into()),
        description: String::new(),
        cost,
        prof_first_rank: false,
        category,
        level_req,
        skill_req,
        ability_reqs,
        is_trade_skill: false,
        skill_line,
        skill_line_name: line_name.into(),
    }
}

/// A two-line warrior menu. Groups sort by name (Arms < Fury); within a group by level then name. The
/// full-filter tree is:
///   1 H:Arms · 2 Heroic Strike(avail,10c,l1) · 3 Cleave(unavail,l20,skill+ability) ·
///   4 H:Fury · 5 Rend(used,30c) · 6 Thunder Clap(avail,500c)
fn menu() -> TrainerState {
    TrainerState {
        greeting: "Well met. Let me show you the way of the warrior.".into(),
        is_tradeskill: false,
        groups: Vec::new(),
        services: vec![
            service(
                78,
                "Heroic Strike",
                TrainerServiceCategory::Available,
                10,
                1,
                26,
                "Arms",
                None,
                vec![],
            ),
            service(
                845,
                "Cleave",
                TrainerServiceCategory::Unavailable,
                100,
                20,
                26,
                "Arms",
                Some(TrainerSkillReq {
                    name: "Swords".into(),
                    rank: 50,
                    met: false,
                }),
                // The director's case: Cleave is gated (level/skill), but its prerequisite ability is
                // already learned — so it reads MET (white) with its rank, decoupled from the
                // service's unavailable state.
                vec![TrainerAbilityReq {
                    name: "Charge (Rank 1)".into(),
                    met: true,
                }],
            ),
            service(
                6343,
                "Thunder Clap",
                TrainerServiceCategory::Available,
                500,
                1,
                256,
                "Fury",
                None,
                vec![],
            ),
            service(
                772,
                "Rend",
                TrainerServiceCategory::Used,
                30,
                1,
                256,
                "Fury",
                None,
                vec![],
            ),
        ],
    }
}

/// The whole trainer window minus Bevy: it loads clean, opens on TRAINER_SHOW with the NPC name in the
/// title, renders the interleaved tree (headers + services), picks the first available service, renders
/// the exact `Cost:` label, gates Train on available-and-affordable, queues the selected row's spell id
/// on a buy, reddens the cost of an unaffordable service, builds the `Requires:` line for a gated one,
/// and hides on TRAINER_CLOSED.
#[test]
fn shipped_trainer_frame_drives_end_to_end() {
    let mut s = trainer_script();

    // Hidden by default.
    assert!(!s
        .eval::<bool>("return BenillaTrainerFrame:IsVisible()")
        .unwrap());

    // The app's feed: 50 copper in the purse + the warrior menu, then the open event with the name.
    s.set_money(50);
    s.set_trainer(Some(menu()));
    s.fire_event(
        "TRAINER_SHOW",
        vec![ScriptValue::Str("Sana Winterhoof".into())],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // Shown, title took the NPC name off arg1.
    assert!(s
        .eval::<bool>("return BenillaTrainerFrame:IsVisible()")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return BenillaTrainerTitleText:GetText()")
            .unwrap(),
        "Sana Winterhoof"
    );

    // Row 1 renders the "Arms" header (its name, no indent); row 2 the first service.
    assert_eq!(
        s.eval::<String>("return BenillaTrainerService1Name:GetText()")
            .unwrap(),
        "Arms"
    );
    assert_eq!(
        s.eval::<String>("return BenillaTrainerService2Name:GetText()")
            .unwrap(),
        "  Heroic Strike"
    );

    // selectFirstService picked the first available row (Heroic Strike, index 2) → the detail pane
    // shows the exact "Cost:" label and Train is enabled (available + 10c affordable at 50c).
    assert_eq!(
        s.eval::<i64>("return GetTrainerSelectionIndex()").unwrap(),
        2
    );
    assert_eq!(
        s.eval::<String>("return BenillaTrainerCostLabel:GetText()")
            .unwrap(),
        "Cost:"
    );
    assert!(s
        .eval::<bool>("return BenillaTrainerTrainButton:IsEnabled()")
        .unwrap());

    // Train buys the selected service — the row's spell id reaches the app's drain.
    s.run("BuyTrainerService(GetTrainerSelectionIndex())")
        .unwrap();
    assert_eq!(s.take_trainer_buys(), vec![78]);
    assert!(s.take_trainer_buys().is_empty(), "drained");

    // Select the available-but-unaffordable service (Thunder Clap, index 6, 500c > 50c): Train disables
    // and the cost coins redden (BenillaMoney_SetColor 1.0, 0.1, 0.1).
    s.run("SelectTrainerService(6); BenillaTrainerFrame_Update()")
        .unwrap();
    assert!(!s
        .eval::<bool>("return BenillaTrainerTrainButton:IsEnabled()")
        .unwrap());
    s.resolve();
    assert!(
        has_text_color(&s.extract(), [1.0, 0.1, 0.1]),
        "unaffordable cost coins render red"
    );

    // Select the gated service (Cleave, index 3): the Requires: line is built from the level/skill/
    // ability gates (byte-exact REQUIRES_LABEL), and Train stays disabled (unavailable).
    s.run("SelectTrainerService(3); BenillaTrainerFrame_Update()")
        .unwrap();
    let reqs = s
        .eval::<String>("return BenillaTrainerSkillRequirements:GetText()")
        .unwrap();
    assert!(reqs.starts_with("Requires: "), "reqs: {reqs}");
    for term in ["Level", "Swords", "Charge"] {
        assert!(reqs.contains(term), "reqs missing {term}: {reqs}");
    }
    // The met prerequisite renders WHITE (|cffffffff…|r) with its rank, while the unmet level/skill
    // gates redden — the mixed line the director asked for (a learned prev-rank isn't reddened just
    // because the spell itself is unavailable).
    assert!(
        reqs.contains("|cffffffffCharge (Rank 1)|r"),
        "a known prerequisite shows white with its rank: {reqs}"
    );
    assert!(!s
        .eval::<bool>("return BenillaTrainerTrainButton:IsEnabled()")
        .unwrap());

    // The app's client-side close: clear the snapshot + fire TRAINER_CLOSED → the window hides.
    s.set_trainer(None);
    s.fire_event("TRAINER_CLOSED", vec![]);
    assert!(!s
        .eval::<bool>("return BenillaTrainerFrame:IsVisible()")
        .unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Clicking a header row folds its group (and expands it back) — the tree collapse, end-to-end through
/// the row button's OnClick → Collapse/ExpandTrainerSkillLine(headerIndex).
#[test]
fn clicking_a_header_row_collapses_its_group() {
    let mut s = trainer_script();
    s.set_money(50);
    s.set_trainer(Some(menu()));
    s.fire_event("TRAINER_SHOW", vec![ScriptValue::Str("Sana".into())]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // Full tree: 2 headers + 4 services = 6 rows; row 2 is Heroic Strike.
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 6);
    assert_eq!(
        s.eval::<String>("return BenillaTrainerService2Name:GetText()")
            .unwrap(),
        "  Heroic Strike"
    );

    // Click the Arms header (row 1): its two services fold → 4 rows (H:Arms, H:Fury, Rend, Thunder
    // Clap). Row 2 is now the Fury header.
    s.run("BenillaTrainerSkillButton_OnClick(BenillaTrainerService1, 'LeftButton')")
        .unwrap();
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 4);
    assert_eq!(
        s.eval::<String>("return BenillaTrainerService2Name:GetText()")
            .unwrap(),
        "Fury",
        "Arms folded; its header (row 1) now abuts the Fury header (row 2)"
    );

    // Click it again → expands back to 6 rows.
    s.run("BenillaTrainerSkillButton_OnClick(BenillaTrainerService1, 'LeftButton')")
        .unwrap();
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 6);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The dropdown filter hides a state client-side: toggling "used" off drops the already-known service,
/// its header stays, and the row count falls.
#[test]
fn filter_hides_a_state_keeping_headers() {
    let mut s = trainer_script();
    s.set_money(50);
    s.set_trainer(Some(menu()));
    s.fire_event("TRAINER_SHOW", vec![ScriptValue::Str("Sana".into())]);

    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 6);

    // Drive the dropdown's own click handler for the "used" row: the row button rides as `this` with
    // its pre-click state (checked=on, value="used"), exactly as UIDropDownMenuButton_OnClick invokes
    // the row func. It flips the engine filter off and repaints. Rend drops (6 → 5); headers remain.
    s.run("this = { value = 'used', checked = 1 }; BenillaTrainerFilterDropDown_OnClick()")
        .unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert_eq!(
        s.eval::<i64>("return GetNumTrainerServices()").unwrap(),
        5,
        "the used service (Rend) is hidden; both headers stay"
    );
    assert!(
        !s.eval::<bool>("return GetTrainerServiceTypeFilter('used') == 1")
            .unwrap(),
        "the engine's used filter is now off"
    );
}

/// A menu bigger than the 11 visible rows (one skill line, 15 services → a 16-row tree), so the list
/// scrolls. The wheel spins the list even when the cursor is over a row — the engine bubbles the spin
/// up the parent chain to the window, which drives the faux-scroll bar.
fn long_menu() -> TrainerState {
    TrainerState {
        greeting: "Much to learn.".into(),
        is_tradeskill: false,
        groups: Vec::new(),
        services: (1..=15)
            .map(|i| {
                service(
                    1000 + i,
                    &format!("Service {i:02}"),
                    TrainerServiceCategory::Available,
                    10,
                    10,
                    26,
                    "Arms",
                    None,
                    vec![],
                )
            })
            .collect(),
    }
}

#[test]
fn wheel_over_a_row_scrolls_the_list() {
    let mut s = trainer_script();
    s.set_money(50);
    s.set_trainer(Some(long_menu()));
    s.fire_event("TRAINER_SHOW", vec![ScriptValue::Str("Sana".into())]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    let row1 = "return BenillaTrainerService1Name:GetText()";
    // At the top: row 1 is the "Arms" header, row 2 the first service.
    assert_eq!(s.eval::<String>(row1).unwrap(), "Arms");
    assert_eq!(
        s.eval::<String>("return BenillaTrainerService2Name:GetText()")
            .unwrap(),
        "  Service 01"
    );

    // Aim the wheel at a LIST ROW's text (service 3 — visible at the top): the spot over a row.
    s.resolve();
    let (x, y) = text_center(&s.extract(), "Service 03");

    // Spin down: the list slides one row so row 1 now shows Service 01 (WoW convention: negative = down).
    s.mouse_wheel(x, y, -1.0);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert_eq!(
        s.eval::<String>(row1).unwrap(),
        "  Service 01",
        "wheel-over-row scrolled the list down one row"
    );

    // Spin down again: row 1 advances to Service 02 — the scroll bar really moves the offset.
    s.mouse_wheel(x, y, -1.0);
    assert_eq!(s.eval::<String>(row1).unwrap(), "  Service 02");

    // Spin back up: Service 01 returns to the top.
    s.mouse_wheel(x, y, 1.0);
    assert_eq!(s.eval::<String>(row1).unwrap(), "  Service 01");
}

/// The selected service row's text renders white (HIGHLIGHT), legible against its colour glow — a
/// deliberate divergence from the ref (which whitens only the subtext, keeping the name state-coloured).
/// Selecting the unavailable Cleave (red when unselected) whitens its name; selecting away restores it.
#[test]
fn selected_service_row_text_is_white() {
    let mut s = trainer_script();
    s.set_money(50);
    s.set_trainer(Some(menu()));
    s.fire_event("TRAINER_SHOW", vec![ScriptValue::Str("Sana".into())]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // Cleave (index 3) is unavailable → red when not selected. Select it → its name goes white.
    s.run("SelectTrainerService(3); BenillaTrainerFrame_Update()")
        .unwrap();
    s.resolve();
    assert!(
        text_has_color(&s.extract(), "Cleave", [1.0, 1.0, 1.0]),
        "the selected row's name renders white"
    );

    // Select Heroic Strike (index 2): Cleave is no longer selected → back to unavailable red.
    s.run("SelectTrainerService(2); BenillaTrainerFrame_Update()")
        .unwrap();
    s.resolve();
    assert!(
        text_has_color(&s.extract(), "Cleave", [0.9, 0.0, 0.0]),
        "an unselected unavailable row is red again"
    );
}

/// Scrolling is silent, faithfully: the mouse WHEEL plays no sound (the ref's
/// `ScrollFrameTemplate_OnMouseWheel` is soundless — the director asked for no sound when scrolling),
/// while the arrow BUTTONS keep the ref's `UChatScrollButton` click.
#[test]
fn wheel_scroll_is_silent_but_the_arrows_click() {
    let mut s = trainer_script();
    s.set_money(50);
    s.set_trainer(Some(long_menu())); // 16 rows > 11 visible → the bar + arrows show
    s.fire_event("TRAINER_SHOW", vec![ScriptValue::Str("Sana".into())]);
    let _ = s.take_sounds(); // drain the window's OnShow open sound

    let click = SoundRequest::KitName("UChatScrollButton".into());

    // A wheel spin over the list scrolls it (proven by the wheel test) but plays NOTHING.
    s.resolve();
    let (x, y) = text_center(&s.extract(), "Service 03");
    s.mouse_wheel(x, y, -1.0);
    assert!(
        !s.take_sounds().contains(&click),
        "the wheel scroll is silent"
    );

    // The down arrow (enabled at the top), though — clicking it plays the ref's arrow click.
    s.run("BenillaTrainerListScrollFrameScrollBarScrollDownButton:Click()")
        .unwrap();
    assert!(
        s.take_sounds().contains(&click),
        "the arrow button clicks (UChatScrollButton)"
    );
}

/// The scrollbar ARROWS move the list the way they point — the direction half the sound test
/// above never checked. They were inverted in the shared kit (ScrollTemplates.xml): at the top the
/// up arrow correctly greys out, but the *enabled* down arrow called `Step(bar, -1)`, decrementing
/// a value already clamped at its minimum — so both arrows were dead in every faux-scroll window
/// (the wheel and the thumb drag masked it, and the sound test passed because OnClick still fired).
#[test]
fn the_scrollbar_arrows_step_the_list_the_way_they_point() {
    let mut s = trainer_script();
    s.set_money(50);
    s.set_trainer(Some(long_menu())); // 16 rows > 11 visible → the bar shows
    s.fire_event("TRAINER_SHOW", vec![ScriptValue::Str("Sana".into())]);
    let offset = |s: &mut UiScript| {
        s.eval::<i64>("return BenillaTrainerListScrollFrame.offset or -1")
            .unwrap()
    };
    let click = |s: &mut UiScript, which: &str| {
        s.run(&format!(
            "BenillaTrainerListScrollFrameScrollBarScroll{which}Button:Click()"
        ))
        .unwrap();
    };
    assert_eq!(offset(&mut s), 0, "opens at the top");
    click(&mut s, "Down");
    assert_eq!(
        offset(&mut s),
        1,
        "the down arrow advances the list one row"
    );
    click(&mut s, "Down");
    assert_eq!(offset(&mut s), 2);
    click(&mut s, "Up");
    assert_eq!(offset(&mut s), 1, "the up arrow walks it back");
    click(&mut s, "Up");
    click(&mut s, "Up");
    assert_eq!(offset(&mut s), 0, "and stops at the top, never past it");
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}
