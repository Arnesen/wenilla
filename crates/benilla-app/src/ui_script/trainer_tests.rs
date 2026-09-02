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

use super::test_ui::load_ui as load_xml;

/// Load the trainer window + all its deps into a fresh script, screen sized, with every state filter
/// ON (the XML defaults "Already Known" off — the tests want the full tree, deterministic indices).
///
/// The filter's source of truth is the three **saved globals** (decision 1128), which the window
/// pushes into the engine on every show — so a test that wants the full tree sets those, not the
/// engine's own `SetTrainerServiceTypeFilter`, which the next `TRAINER_SHOW` would overwrite.
fn trainer_script() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml"); // TOOLTIP_DEFAULT_* (the kit's MenuBackdrop), app order
    load_xml(&s, "UIDropDownMenu.xml"); // the filter dropdown's kit
    load_xml(&s, "ScrollTemplates.xml"); // the faux-scroll bar kit
    load_xml(&s, "MerchantFrame.xml"); // BenillaMoney_Set/_Clear/_SetColor live here
    load_xml(&s, "TrainerFrame.xml");
    s.run(
        "TRAINER_FILTER_AVAILABLE = 1 TRAINER_FILTER_UNAVAILABLE = 1 TRAINER_FILTER_USED = 1 \
         BenillaTrainerFrame_ApplyFilter()",
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
        tooltip: benilla_ui::script::TrainerTooltip::Spell {
            spell_id,
            alt_caster: false,
        },
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
        group_key: skill_line,
        group_name: line_name.into(),
    }
}

/// A two-line warrior menu. Groups sort by name (Arms < Fury); within a group by level then name. The
/// full-filter tree is:
///   1 H:Arms · 2 Heroic Strike(avail,10c,l1) · 3 Cleave(unavail,l20,skill+ability) ·
///   4 H:Fury · 5 Rend(used,30c) · 6 Thunder Clap(avail,500c)
fn menu() -> TrainerState {
    TrainerState {
        greeting: "Well met. Let me show you the way of the warrior.".into(),
        trainer_type: 0,
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
        s.eval::<String>("return BenillaTrainerService1Text:GetText()")
            .unwrap(),
        "Arms"
    );
    assert_eq!(
        s.eval::<String>("return BenillaTrainerService2Text:GetText()")
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
        .eval::<bool>("return BenillaTrainerTrainButton:IsEnabled() ~= 0")
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
        .eval::<bool>("return BenillaTrainerTrainButton:IsEnabled() ~= 0")
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
        .eval::<bool>("return BenillaTrainerTrainButton:IsEnabled() ~= 0")
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
        s.eval::<String>("return BenillaTrainerService2Text:GetText()")
            .unwrap(),
        "  Heroic Strike"
    );

    // Click the Arms header (row 1): its two services fold → 4 rows (H:Arms, H:Fury, Rend, Thunder
    // Clap). Row 2 is now the Fury header.
    s.run("BenillaTrainerSkillButton_OnClick(BenillaTrainerService1, 'LeftButton')")
        .unwrap();
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 4);
    assert_eq!(
        s.eval::<String>("return BenillaTrainerService2Text:GetText()")
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

/// The filter rows toggle **through the real dropdown kit** — the path a mouse takes, which the test
/// above deliberately shortcuts by faking `this`. `UIDropDownMenuButton_OnClick` runs the row's func
/// and only THEN flips the check for a `keepShownOnClick` row, so the func must not repaint the row
/// itself: an in-func `UIDropDownMenu_Initialize` re-derived the check from the fresh engine state and
/// the kit's flip then inverted it straight back — the check never moved on screen, and `this.checked`
/// stuck true, so a filter turned off could never be turned back on. Click, re-click, and re-open all
/// have to agree.
#[test]
fn filter_rows_toggle_through_the_dropdown_kit() {
    let mut s = trainer_script();
    s.set_money(50);
    s.set_trainer(Some(menu()));
    s.fire_event("TRAINER_SHOW", vec![ScriptValue::Str("Sana".into())]);

    // Open the menu the way the capsule's arrow does. Row 2 is "Unavailable" (Initialize's order),
    // checked because trainer_script() turned every state on.
    s.run("ToggleDropDownMenu(1, nil, BenillaTrainerFilterDropDown)")
        .unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    let row_checked = |s: &mut UiScript| {
        s.eval::<bool>("return DropDownList1Button2Check:IsVisible() and true or false")
            .unwrap()
    };
    assert!(row_checked(&mut s), "Unavailable starts checked");
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 6);

    // Click it: the check clears, the engine filter clears, and Cleave (the unavailable service)
    // drops out of the tree. Its Arms header stays.
    s.run("this = DropDownList1Button2; UIDropDownMenuButton_OnClick()")
        .unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(!row_checked(&mut s), "the click clears the row's check");
    assert!(
        !s.eval::<bool>("return GetTrainerServiceTypeFilter('unavailable') == 1")
            .unwrap(),
        "and the engine's unavailable filter with it"
    );
    assert_eq!(
        s.eval::<i64>("return GetNumTrainerServices()").unwrap(),
        5,
        "Cleave is hidden"
    );

    // Click it again: back on, both on screen and in the engine — the case the old code could never
    // reach, because `this.checked` never went false.
    s.run("this = DropDownList1Button2; UIDropDownMenuButton_OnClick()")
        .unwrap();
    assert!(row_checked(&mut s), "the re-click restores the check");
    assert!(
        s.eval::<bool>("return GetTrainerServiceTypeFilter('unavailable') == 1")
            .unwrap(),
        "and re-enables the filter"
    );
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 6);

    // Close and re-open: Initialize re-derives every row from the engine, so the menu agrees with
    // what the clicks left behind (the two states left on, "used" still on from trainer_script()).
    s.run("this = DropDownList1Button2; UIDropDownMenuButton_OnClick()")
        .unwrap();
    s.run("ToggleDropDownMenu(1, nil, BenillaTrainerFilterDropDown)")
        .unwrap(); // same owner → closes
    s.run("ToggleDropDownMenu(1, nil, BenillaTrainerFilterDropDown)")
        .unwrap(); // re-open → re-Initialize
    assert!(
        !row_checked(&mut s),
        "a re-opened menu shows the filter the clicks actually left off"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// A menu bigger than the 11 visible rows (one skill line, 15 services → a 16-row tree), so the list
/// scrolls. The wheel spins the list even when the cursor is over a row — the engine bubbles the spin
/// up the parent chain to the window, which drives the faux-scroll bar.
fn long_menu() -> TrainerState {
    TrainerState {
        greeting: "Much to learn.".into(),
        trainer_type: 0,
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

    let row1 = "return BenillaTrainerService1Text:GetText()";
    // At the top: row 1 is the "Arms" header, row 2 the first service.
    assert_eq!(s.eval::<String>(row1).unwrap(), "Arms");
    assert_eq!(
        s.eval::<String>("return BenillaTrainerService2Text:GetText()")
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

/// The selected service row's name renders white (HIGHLIGHT), legible against its colour glow — and
/// so does a HOVERED one. Both are the reference's own behaviour, through one mechanism: the row's
/// name is the button's `<ButtonText>` under a `<HighlightFont inherits="GameFontHighlight">`, the
/// engine swaps that instance in while the cursor is on the row, and `LockHighlight()` pins it for
/// the selection (Blizzard_TrainerUI.lua l.183). `SetTextColor` writes the NORMAL instance only, so
/// the state colour cannot follow the label into either state — which is exactly why it goes white.
///
/// An earlier revision of this test called the white name "a deliberate divergence from the ref
/// (which whitens only the subtext)". The ref whitens the subtext *by hand* precisely BECAUSE the
/// subtext is a child FontString the lock cannot reach; the name it leaves to the lock. Ours could
/// not, because the name was a child FontString too and the engine's highlighted label fell back to
/// the normal state's colour — so the white was hand-painted here and absent on hover entirely
/// (decision 1605).
#[test]
fn a_selected_or_hovered_service_row_paints_its_name_white() {
    let mut s = trainer_script();
    s.set_money(50);
    s.set_trainer(Some(menu()));
    s.fire_event("TRAINER_SHOW", vec![ScriptValue::Str("Sana".into())]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // The name really is the button's own label now — a child FontString would leave
    // GetFontString() nil and no per-state font could reach it.
    assert!(
        s.eval::<bool>("return BenillaTrainerService1:GetFontString() ~= nil")
            .unwrap(),
        "the row name is the Button's ButtonText, the only region per-state fonts reach"
    );

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

    // Now hover it, selecting nothing: the HighlightFont instance takes over and the row lights up.
    let (x, y) = text_center(&s.extract(), "Cleave");
    s.mouse_move(x, y);
    s.resolve();
    assert!(
        text_has_color(&s.extract(), "Cleave", [1.0, 1.0, 1.0]),
        "a hovered row's name renders white, with no script doing it"
    );
    assert!(
        text_has_color(&s.extract(), "Heroic Strike", [1.0, 1.0, 1.0]),
        "and the SELECTED row stays white while another is hovered"
    );

    // Cursor off the list: the hovered row falls back to its state colour, the selected one holds.
    s.mouse_move(1000.0, 20.0);
    s.resolve();
    assert!(
        text_has_color(&s.extract(), "Cleave", [0.9, 0.0, 0.0]),
        "cursor away: red again"
    );
    assert!(
        text_has_color(&s.extract(), "Heroic Strike", [1.0, 1.0, 1.0]),
        "the selection's white is the LOCK, not the hover"
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

/// **The filter is remembered across a restart** (decision 1128) — the whole persistence path, in
/// one test: the shipped XML declares its three globals for saving at file scope, a dropdown toggle
/// writes the *global* (not only the engine mask), the serialized file carries it, and a fresh VM
/// that loads the same XML and then executes that file comes up with the toggled filter applied.
///
/// It also pins the two halves that are easy to "simplify" apart and silently break: writing only
/// the engine mask in the click handler would lose the choice at the next list packet (the reference
/// rebuilds that mask on every one), and applying the globals only in `<OnShow>` would leave the
/// first painted frame under the reset.
#[test]
fn the_state_filter_survives_a_restart_through_the_saved_variables_file() {
    let mut s = trainer_script();
    s.set_money(50);
    s.set_trainer(Some(menu()));
    s.fire_event("TRAINER_SHOW", vec![ScriptValue::Str("Sana".into())]);
    assert_eq!(s.eval::<i64>("return GetNumTrainerServices()").unwrap(), 6);

    // This XML declared exactly the reference's three globals, in its order. Filtered to the
    // trainer's own prefix because the registry belongs to the whole UI: every file the harness
    // loads adds its own (GameTooltip.xml's SHOW_NEWBIE_TIPS since 1136), and what the OTHER files
    // register is not this test's business.
    let declared: Vec<String> = s
        .saved_variable_names()
        .into_iter()
        .filter(|n| n.starts_with("TRAINER_"))
        .collect();
    assert_eq!(
        declared,
        vec![
            "TRAINER_FILTER_AVAILABLE",
            "TRAINER_FILTER_UNAVAILABLE",
            "TRAINER_FILTER_USED",
        ]
    );

    // Toggle "Unavailable" off through the dropdown row's own handler (pre-click state on `this`).
    s.run("this = { value = 'unavailable', checked = 1 }; BenillaTrainerFilterDropDown_OnClick()")
        .unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert_eq!(
        s.eval::<i64>("return TRAINER_FILTER_UNAVAILABLE").unwrap(),
        0,
        "the click must write the SAVED global, not just the engine mask"
    );
    let text = s.saved_variables_text();
    assert!(
        text.contains("TRAINER_FILTER_UNAVAILABLE = 0"),
        "the file carries the toggle: {text}"
    );

    // The restart: a fresh VM with the same XML (file-scope defaults stand), then the saved file
    // executed over them, then a trainer opens.
    let mut fresh = trainer_script();
    fresh.run(&text).unwrap();
    fresh.set_money(50);
    fresh.set_trainer(Some(menu()));
    fresh.fire_event("TRAINER_SHOW", vec![ScriptValue::Str("Sana".into())]);
    assert!(
        !fresh
            .eval::<bool>("return GetTrainerServiceTypeFilter('unavailable') == 1")
            .unwrap(),
        "the remembered filter reached the engine"
    );
    assert_eq!(
        fresh.eval::<i64>("return GetNumTrainerServices()").unwrap(),
        5,
        "Cleave (the unavailable service) is hidden on the very first paint after the restart"
    );
    assert!(
        fresh.errors().is_empty(),
        "script errors: {:?}",
        fresh.errors()
    );
}

/// A **new list packet resets the engine's filter mask** to the builder's own default — mask 3 at a
/// class/tradeskill/pet trainer, mask 5 (available|used) at a mount trainer — and clears the collapse
/// set, byte-verified (decision 1128). This is the engine half of the pair above: the reset is why
/// the window re-pushes its saved globals on every show.
#[test]
fn a_new_list_packet_resets_the_filter_mask_and_the_collapse_set() {
    let mut s = trainer_script();
    s.set_trainer(Some(menu()));
    s.run("BenillaTrainerFrame_ApplyFilter()").unwrap();
    // Collapse a group, and turn a state off directly (no global) — both are engine-side state.
    s.run("CollapseTrainerSkillLine(1) SetTrainerServiceTypeFilter('used', 1)")
        .unwrap();
    assert!(s
        .eval::<bool>("return GetTrainerServiceTypeFilter('used') == 1")
        .unwrap());
    assert!(s.eval::<i64>("return GetNumTrainerServices()").unwrap() < 6);

    s.reset_trainer_filter(0);
    assert!(
        !s.eval::<bool>("return GetTrainerServiceTypeFilter('used') == 1")
            .unwrap(),
        "mask 3: available|unavailable, already-known OFF"
    );
    assert!(s
        .eval::<bool>("return GetTrainerServiceTypeFilter('available') == 1")
        .unwrap());
    assert!(s
        .eval::<bool>("return GetTrainerServiceTypeFilter('unavailable') == 1")
        .unwrap());
    assert_eq!(
        s.eval::<i64>("return GetNumTrainerServices()").unwrap(),
        5,
        "nothing collapsed any more (6 rows less the already-known service the mask now hides)"
    );

    // A mount trainer wants available|used instead — what makes a known mount visible at all.
    s.reset_trainer_filter(1);
    assert!(s
        .eval::<bool>("return GetTrainerServiceTypeFilter('used') == 1")
        .unwrap());
    assert!(!s
        .eval::<bool>("return GetTrainerServiceTypeFilter('unavailable') == 1")
        .unwrap());
}

/// A profession trainer's list: one skill line, a long recipe name and a short one, each with a rank
/// subtext — the shape of the B253 report (a Leatherworking trainer at "Handstitched Leather Pants").
fn recipe_menu() -> TrainerState {
    let mut long = service(
        3756,
        "Handstitched Leather Pants",
        TrainerServiceCategory::Available,
        50,
        1,
        165,
        "Leatherworking",
        None,
        vec![],
    );
    long.subtext = Some("Rank 1".into());
    let mut short = service(
        2149,
        "Belt",
        TrainerServiceCategory::Available,
        50,
        1,
        165,
        "Leatherworking",
        None,
        vec![],
    );
    short.subtext = Some("Rank 1".into());
    TrainerState {
        greeting: "Can I teach you how to turn beast hides into armor?".into(),
        trainer_type: 2,
        groups: Vec::new(),
        services: vec![long, short],
    }
}

/// **B253 — a long row name drew two lines over the row beneath it.** The row is a FLOW, not two
/// fixed columns: the name carries no width (a FontString given one wraps at it, and a 16 px row
/// cannot grow), and the rank follows the name's right edge rather than sitting at an invented
/// x=188. Both halves are the reference's own row
/// (`ClassTrainerFrameTemplates.xml`'s `<ButtonText>` at width 0 + the per-row
/// `SetPoint("LEFT", <row>Text, "RIGHT", 10, 0)`), read off the player's chain.
///
/// The mutation check is the first assertion: put a `<Size>` back on the row's `<ButtonText>` and
/// the request
/// carries a wrap width, the long name measures two lines, and its height passes the row height.
#[test]
fn a_long_row_name_stays_on_one_line_and_carries_its_rank_along() {
    let mut s = trainer_script();
    s.set_money(5000);
    s.set_trainer(Some(recipe_menu()));
    s.fire_event("TRAINER_SHOW", vec![ScriptValue::Str("Nadyia".into())]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // The host measure: 6 px per character, 14 px per line, wrapped at whatever width was asked for.
    // Capture the row names' requests on the way past — what they ASK is the structural fact.
    let mut name_wraps: Vec<Option<f32>> = Vec::new();
    let mut answer = |s: &mut UiScript, collect: bool| {
        let reqs = s.fontstrings_needing_measure();
        if collect {
            name_wraps.extend(
                reqs.iter()
                    .filter(|r| r.text.contains("Handstitched"))
                    .map(|r| r.wrap_width),
            );
        }
        let answers: Vec<(u32, f32, f32, u64)> = reqs
            .into_iter()
            .map(|r| {
                let ink = r.text.chars().count() as f32 * 6.0;
                match r.wrap_width {
                    Some(w) => (r.id, ink.min(w), (ink / w).ceil().max(1.0) * 14.0, r.key),
                    None => (r.id, ink, 14.0, r.key),
                }
            })
            .collect();
        s.set_measured_text_unwrapped(&answers);
    };
    answer(&mut s, true);
    s.resolve();
    s.tick(0.016);
    answer(&mut s, false);
    s.resolve();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    assert!(
        !name_wraps.is_empty() && name_wraps.iter().all(|w| w.is_none()),
        "the row name must ask for NO wrap width — it is a single line whatever the name is; \
         got {name_wraps:?}"
    );

    // Find each name's row by what it painted (the group comparator owns the order, not this test).
    let row_of = |s: &mut UiScript, needle: &str| -> i64 {
        s.eval::<i64>(&format!(
            "for i = 1, 11 do local b = getglobal('BenillaTrainerService' .. i) \
             local t = b:GetText() if t and strfind(t, '{needle}', 1, 1) then return i end \
             end return 0"
        ))
        .unwrap()
    };
    let long_row = row_of(&mut s, "Handstitched");
    let short_row = row_of(&mut s, "Belt");
    assert!(long_row > 0 && short_row > 0, "both services painted");

    let geom = |s: &mut UiScript, row: i64| -> (f32, f32, f32, f32) {
        s.eval::<(f32, f32, f32, f32)>(&format!(
            "local b = getglobal('BenillaTrainerService{row}') \
             local n = b:GetFontString() \
             return n:GetHeight(), n:GetRight(), b.subtext:GetLeft(), b:GetHeight()"
        ))
        .unwrap()
    };
    let (long_h, long_name_right, long_sub_left, row_h) = geom(&mut s, long_row);
    let (short_h, short_name_right, short_sub_left, _) = geom(&mut s, short_row);

    assert!(
        long_h <= row_h + 0.5,
        "the long name is one line inside its own row: name {long_h} px, row {row_h} px"
    );
    assert!((long_h - short_h).abs() < 0.5, "and so is the short one");
    for (name, right, left) in [
        ("long", long_name_right, long_sub_left),
        ("short", short_name_right, short_sub_left),
    ] {
        assert!(
            (left - right - 10.0).abs() < 0.5,
            "the {name} row's rank sits 10 px past its NAME's right edge (the reference's own \
             offset), not at a fixed column: name right {right}, subtext left {left}"
        );
    }
    assert!(
        long_sub_left > short_sub_left + 50.0,
        "so a longer name pushes its rank along instead of running under it: {long_sub_left} vs \
         {short_sub_left}"
    );
}

/// **B256 — "filter to Available, learn a spell, the filter comes back partly reset."** The window
/// is driven here through the real [`crate::ui_trainer::TrainerOpen`], so the packet→reset decision
/// under test is the app's own and not this test's: the closure below is the trainer feed's three
/// lines (`if open.fresh_list { reset } ; set_trainer ; fire`), and everything else is the shipped
/// Lua.
///
/// The reference cannot produce this bug because it never gets a list packet with the window open —
/// it repaints a purchase from a client-side state re-derivation (`0x4d7d40`, decision 1128 §4.2).
/// benilla re-asks the server instead, so the reference's per-packet mask reset (`0x4d75d9`) was
/// riding in on a packet the reference never sends, and taking the player's choice — and their
/// collapsed groups, which 1128 recorded as "a collapse does not survive a purchase" — with it.
#[test]
fn learning_a_spell_keeps_the_filter_and_the_collapse_a_re_open_still_resets() {
    use crate::ui_trainer::TrainerOpen;
    const DAZALAR: u64 = 0xabc;

    let mut s = trainer_script();
    // The reference's own file-scope defaults (1128), then the player's choice below.
    s.run(
        "TRAINER_FILTER_AVAILABLE = 1 TRAINER_FILTER_UNAVAILABLE = 1 TRAINER_FILTER_USED = 0 \
         BenillaTrainerFrame_ApplyFilter()",
    )
    .unwrap();
    s.set_money(5000);

    // The trainer feed (`ui_trainer::feed_trainer`), reduced to the part this bug lives in.
    fn feed(s: &mut UiScript, open: &mut TrainerOpen, state: TrainerState, event: &str) {
        if open.fresh_list {
            s.reset_trainer_filter(open.trainer_type);
            open.fresh_list = false;
        }
        s.set_trainer(Some(state));
        s.fire_event(event, vec![ScriptValue::Str("Dazalar".into())]);
    }
    let filter_on = |s: &mut UiScript, kind: &str| {
        s.eval::<bool>(&format!(
            "return GetTrainerServiceTypeFilter('{kind}') == 1"
        ))
        .unwrap()
    };
    let rows = |s: &mut UiScript| s.eval::<i64>("return GetNumTrainerServices()").unwrap();

    // He opens the trainer and filters the list down to what he can actually learn.
    let mut open = TrainerOpen::default();
    open.open(DAZALAR, 0, vec![], "Hello, hunter!".into());
    feed(&mut s, &mut open, menu(), "TRAINER_SHOW");
    s.run("ToggleDropDownMenu(1, nil, BenillaTrainerFilterDropDown)")
        .unwrap();
    s.run("this = DropDownList1Button2; UIDropDownMenuButton_OnClick()")
        .unwrap();
    s.run("ToggleDropDownMenu(1, nil, BenillaTrainerFilterDropDown)")
        .unwrap();
    assert!(
        !filter_on(&mut s, "unavailable"),
        "his choice reached the engine"
    );
    assert_eq!(
        rows(&mut s),
        4,
        "two headers over the two learnable services"
    );
    // …and folds the first group away while he is at it.
    s.run("CollapseTrainerSkillLine(1)").unwrap();
    assert_eq!(rows(&mut s), 3, "the folded group keeps its header only");

    // He trains. The app re-asks for the list (`trainer_buy_succeeded`) and marks its own answer as
    // the repaint it is; the bought service comes back gray.
    let mut learned = menu();
    learned.services[0].category = TrainerServiceCategory::Used;
    open.refresh_pending = true;
    open.open(DAZALAR, 0, vec![], "Hello, hunter!".into());
    feed(&mut s, &mut open, learned, "TRAINER_UPDATE");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        !filter_on(&mut s, "unavailable"),
        "learning a spell is a repaint, not a new window: his filter stands"
    );
    assert_eq!(
        rows(&mut s),
        2,
        "and the list is still his — the folded Arms group is empty of learnables now, so it goes \
         with its header, leaving Fury over Thunder Clap"
    );
    s.run("ToggleDropDownMenu(1, nil, BenillaTrainerFilterDropDown)")
        .unwrap();
    assert!(
        !s.eval::<bool>("return DropDownList1Button2Check:IsVisible() and true or false")
            .unwrap(),
        "the dropdown agrees with the list, instead of claiming a filter the list ignores"
    );
    s.run("ToggleDropDownMenu(1, nil, BenillaTrainerFilterDropDown)")
        .unwrap();

    // He walks away and comes back: THIS packet opens a window, so the reference's reset lands —
    // and the window's own OnShow/TRAINER_SHOW puts the saved globals straight back over it. The
    // filter survives (it is saved), the collapse does not (it is engine-side, and the reset clears
    // it) — the reference's own split.
    open.clear();
    open.open(DAZALAR, 0, vec![], "Hello, hunter!".into());
    let mut learned = menu();
    learned.services[0].category = TrainerServiceCategory::Used;
    feed(&mut s, &mut open, learned, "TRAINER_SHOW");
    assert!(
        !filter_on(&mut s, "unavailable"),
        "the saved filter is pushed back over the packet reset"
    );
    assert!(
        filter_on(&mut s, "available"),
        "and the states he never turned off are still on"
    );
    assert_eq!(
        rows(&mut s),
        2,
        "nothing collapsed now — but still filtered"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
