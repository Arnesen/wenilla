//! Drives the REAL `assets/ui/TradeSkillFrame.xml` through the engine — the crafting-book twin
//! of `talent_frame.rs`, and the first test that executes the transcribed tradeskill Lua at all
//! (the polish pass's own discovery: no suite loaded this file, so a runtime bug in the
//! Show/Update/dropdown code would only ever surface in a live session).
//!
//! The harness loads the same file chain the app does (`ui_script/mod.rs`'s list, cut to the
//! tradeskill window's dependency prefix), pushes a synthetic two-group Blacksmithing book,
//! opens the window with the app's own `TRADE_SKILL_SHOW`, and exercises the polish-pass
//! surface end-to-end: the CollapseAll tab (text, fold-all round trip through the engine's
//! touched-flag → `TRADE_SKILL_UPDATE` contract), and both filter dropdowns (capsule default
//! text, a REAL menu-row click driving the exclusive filter, the "All" row restoring it).

use benilla_ui::script::{
    CraftRecipe, CraftState, CraftTooltip, TradeSkillDifficulty, TradeSkillReagent,
    TradeSkillRecipe, TradeSkillState, UiScript,
};

const UI_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/ui");

/// The tradeskill window's load prefix — the app's own order (`ui_script/mod.rs`), members only.
/// CraftFrame.xml rides along (it loads right after TradeSkillFrame.xml in the app and shares
/// its guarded-global utilities) so a load error in EITHER window fails here.
const FILES: [&str; 8] = [
    "Fonts.xml",
    "MoneyFrame.xml",
    "UiPanels.xml",
    "GameTooltip.xml",
    "UIDropDownMenu.xml",
    "ScrollTemplates.xml",
    "TradeSkillFrame.xml",
    "CraftFrame.xml",
];

fn load_ui(script: &UiScript) {
    let dir = std::path::Path::new(UI_DIR);
    let provider = |req: &str| -> Option<Vec<u8>> {
        let norm = req.replace('\\', "/");
        let base = norm.rsplit('/').next().unwrap_or(&norm);
        std::fs::read(dir.join(&norm))
            .or_else(|_| std::fs::read(dir.join(base)))
            .ok()
    };
    for file in FILES {
        let text = std::fs::read_to_string(dir.join(file)).unwrap_or_else(|e| {
            panic!("reading {file}: {e}");
        });
        let doc = benilla_ui::framexml::parse(&text).unwrap_or_else(|e| {
            panic!("parsing {file}: {e}");
        });
        let report = benilla_ui::loader::load(script, &doc, &provider);
        assert!(
            report.errors.is_empty(),
            "{file} loaded with errors: {:#?}",
            report.errors
        );
    }
}

fn recipe(
    spell_id: u32,
    name: &str,
    group: (u32, u32, &str),
    product_inv_type: u32,
) -> TradeSkillRecipe {
    TradeSkillRecipe {
        group: Some((group.0, group.1, group.2.to_string())),
        spell_id,
        name: name.into(),
        difficulty: TradeSkillDifficulty::Medium,
        num_available: 2,
        icon: Some("Interface\\Icons\\INV_Misc_ArmorKit_04".into()),
        min_made: 1,
        max_made: 1,
        cooldown_secs: None,
        product_item: spell_id + 10_000,
        product_inv_type,
        product_item_level: 0, // neutral — this file's order pins fall through to the name
        reagents: vec![TradeSkillReagent {
            item: 2840,
            name: Some("Copper Bar".into()),
            icon: Some("Interface\\Icons\\INV_Ingot_02".into()),
            need: 2,
            have: 5,
        }],
        tools: vec![("Anvil".into(), true)],
    }
}

/// A two-group Blacksmithing book: Mail (a chest + a legs product) and Trade Goods (a non-equip
/// stone → the 0x800000 catch-all slot) — the director's own reference-screenshot shape.
fn state() -> TradeSkillState {
    TradeSkillState {
        line: 164,
        line_name: "Blacksmithing".into(),
        rank: 1,
        max_rank: 75,
        recipes: vec![
            recipe(2661, "Copper Chain Vest", (4, 3, "Mail"), 5),
            recipe(2662, "Copper Chain Pants", (4, 3, "Mail"), 7),
            recipe(3320, "Rough Sharpening Stone", (7, 0, "Trade Goods"), 0),
        ],
        repeat_count: 0,
    }
}

/// The app-side contract the window's event-driven repaints ride on: after any engine-side list
/// mutator, drain the touched flag and fire `TRADE_SKILL_UPDATE` (drain_trade_skill's own shape).
fn pump(script: &mut UiScript) {
    if script.take_trade_skill_touched() {
        script.fire_event("TRADE_SKILL_UPDATE", vec![]);
    }
}

#[test]
fn collapse_all_tab_and_filter_dropdowns_work_end_to_end() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);

    s.set_trade_skill(Some(state()));
    s.fire_event("TRADE_SKILL_SHOW", vec![]);
    assert!(
        s.eval::<bool>("return BenillaTradeSkillFrame:IsShown()")
            .unwrap(),
        "the window opens on TRADE_SKILL_SHOW"
    );
    // 2 headers + 3 recipes.
    assert_eq!(s.eval::<i64>("return GetNumTradeSkills()").unwrap(), 5);

    // The CollapseAll tab carries its GlobalString text (a cross-chunk local would render nil).
    assert_eq!(
        s.eval::<String>("return BenillaTradeSkillCollapseAllButton:GetText()")
            .unwrap(),
        "All"
    );

    // Fold everything through the tab: the click never calls Update() itself — the engine's
    // touched flag + TRADE_SKILL_UPDATE (pump) is the whole repaint path, the ref's own contract.
    s.run("BenillaTradeSkillCollapseAllButton:Click()").unwrap();
    pump(&mut s);
    assert_eq!(
        s.eval::<i64>("return GetNumTradeSkills()").unwrap(),
        2,
        "collapse-all leaves only the two headers"
    );
    s.run("BenillaTradeSkillCollapseAllButton:Click()").unwrap();
    pump(&mut s);
    assert_eq!(s.eval::<i64>("return GetNumTradeSkills()").unwrap(), 5);

    // The dropdown capsules default to the "All …" texts (the OnShow/Initialize dance).
    assert_eq!(
        s.eval::<String>("return BenillaTradeSkillSubClassDropDownText:GetText()")
            .unwrap(),
        "All Subclasses"
    );
    assert_eq!(
        s.eval::<String>("return BenillaTradeSkillInvSlotDropDownText:GetText()")
            .unwrap(),
        "All Slots"
    );
    // The InvSlot vocabulary: Chest(5) → bit 4, Legs(7) → bit 6, stone(0) → the catch-all.
    assert_eq!(
        s.eval::<(String, String, String)>("return GetTradeSkillInvSlots()")
            .unwrap(),
        (
            "Chest".to_string(),
            "Legs".to_string(),
            "Not equippable.".to_string()
        )
    );

    // A REAL menu-row click: open the SubClass menu, click "Trade Goods" (row 3: All + 2 groups).
    s.run("ToggleDropDownMenu(1, nil, BenillaTradeSkillSubClassDropDown)")
        .unwrap();
    s.run("DropDownList1Button3:Click()").unwrap();
    pump(&mut s);
    assert_eq!(
        s.eval::<i64>("return GetNumTradeSkills()").unwrap(),
        2,
        "exclusive Trade Goods: its header + one recipe"
    );
    assert_eq!(
        s.eval::<String>("local n = GetTradeSkillInfo(1) return n")
            .unwrap(),
        "Trade Goods"
    );
    // The capsule follows the picked row on the next initialize (OnShow re-runs it).
    s.run("BenillaTradeSkillSubClassDropDown:Hide() BenillaTradeSkillSubClassDropDown:Show()")
        .unwrap();
    assert_eq!(
        s.eval::<String>("return BenillaTradeSkillSubClassDropDownText:GetText()")
            .unwrap(),
        "Trade Goods"
    );

    // The "All Subclasses" row (row 1) restores the full list.
    s.run("ToggleDropDownMenu(1, nil, BenillaTradeSkillSubClassDropDown)")
        .unwrap();
    s.run("DropDownList1Button1:Click()").unwrap();
    pump(&mut s);
    assert_eq!(s.eval::<i64>("return GetNumTradeSkills()").unwrap(), 5);
}

/// The Craft window's own CollapseAll tab is faithful-but-inert (a 1.12 craft list is a single
/// skill-line group, so the header scan always finds zero and hides the tab — the file's own
/// deviation note); this pins the load + the text attribute + the hidden-in-practice state.
#[test]
fn craft_collapse_tab_loads_with_text_and_stays_hidden_for_a_flat_list() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);

    assert_eq!(
        s.eval::<String>("return BenillaCraftCollapseAllButton:GetText()")
            .unwrap(),
        "All"
    );
    s.fire_event("CRAFT_SHOW", vec![]);
    assert!(
        s.eval::<bool>("return BenillaCraftFrame:IsShown()")
            .unwrap(),
        "the craft window opens on CRAFT_SHOW"
    );
    assert!(
        !s.eval::<bool>("return BenillaCraftExpandButtonFrame:IsShown()")
            .unwrap(),
        "zero headers → the tab hides (ref l.269-282's own scan)"
    );
}

/// **The reagent slot IS `QuestItemTemplate`** — B250's pin, in both windows.
///
/// The ref's `TradeSkillItemTemplate` (Blizzard_TradeSkillUI.xml l.11-35) and `CraftItemTemplate`
/// (Blizzard_CraftUI.xml l.29-53) each inherit `QuestItemTemplate` and override **only scripts**, so
/// the slot's whole visual is that template's: 147×41, a 39×39 icon, the `UI-QuestItemNameFrame`
/// plate on the icon's right edge, the name centred ON the plate, and the count on the icon's own
/// BOTTOMRIGHT.
///
/// Every assertion here is one the shape that shipped before B250 would fail — a 140×32 slot with a
/// 28×28 icon, no plate at all, the name top-anchored right of the icon and the count *below* the
/// name. It is written that way on purpose (decision 1107's "so the sibling law would fail it"): the
/// numbers alone would let a future session drift the slot back toward a hand-rolled shape with no
/// test going red.
#[test]
fn reagent_slots_carry_the_questitemtemplate_shape_in_both_windows() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);

    // Both windows open (a slot's anchors resolve against a laid-out parent), and the slots this
    // test measures are shown — the ref's own SetSelection shows one per reagent.
    s.set_trade_skill(Some(state()));
    s.fire_event("TRADE_SKILL_SHOW", vec![]);
    s.fire_event("CRAFT_SHOW", vec![]);
    for w in ["BenillaTradeSkillReagent", "BenillaCraftReagent"] {
        for i in 1..=3 {
            s.run(&format!("{w}{i}:Show()")).unwrap();
        }
    }

    for w in ["BenillaTradeSkillReagent", "BenillaCraftReagent"] {
        let num = |expr: &str| {
            s.eval::<f64>(&format!("return {expr}"))
                .unwrap_or_else(|e| panic!("{expr}: {e}"))
        };

        // The row box: 147×41, the ref's QuestItemTemplate <Size>.
        assert_eq!(
            (
                num(&format!("{w}1:GetWidth()")),
                num(&format!("{w}1:GetHeight()"))
            ),
            (147.0, 41.0),
            "{w}1 row box"
        );

        // The 2-column grid: column 2 opens exactly one row-width right (Reagent2 anchors
        // LEFT→Reagent1's RIGHT with a zero offset, so the pitch IS the row width) …
        assert_eq!(
            num(&format!("{w}2:GetLeft()")) - num(&format!("{w}1:GetLeft()")),
            147.0,
            "{w} column pitch"
        );
        // … and row 2 drops one row height plus the ref's own 2px gutter.
        assert_eq!(
            num(&format!("{w}3:GetTop()")) - num(&format!("{w}1:GetTop()")),
            -43.0,
            "{w} row step"
        );

        // The icon: 39×39 flush in the row's TOPLEFT corner.
        assert_eq!(
            (
                num(&format!("{w}1Icon:GetWidth()")),
                num(&format!("{w}1Icon:GetHeight()"))
            ),
            (39.0, 39.0),
            "{w}1 icon"
        );
        assert_eq!(
            num(&format!("{w}1Icon:GetLeft()")),
            num(&format!("{w}1:GetLeft()")),
            "{w}1 icon flush left"
        );
        assert_eq!(
            num(&format!("{w}1Icon:GetTop()")),
            num(&format!("{w}1:GetTop()")),
            "{w}1 icon flush top"
        );

        // The name plate — the piece that was missing entirely. Its 128×64 texture starts 10px
        // inside the icon's right edge and is centred on the icon's own middle.
        assert_eq!(
            s.eval::<String>(&format!("return {w}1NameFrame:GetTexture()"))
                .unwrap(),
            "Interface\\QuestFrame\\UI-QuestItemNameFrame",
            "{w}1 name plate art"
        );
        assert_eq!(
            (
                num(&format!("{w}1NameFrame:GetWidth()")),
                num(&format!("{w}1NameFrame:GetHeight()"))
            ),
            (128.0, 64.0),
            "{w}1 plate size"
        );
        assert_eq!(
            num(&format!("{w}1NameFrame:GetLeft()")) - num(&format!("{w}1Icon:GetRight()")),
            -10.0,
            "{w}1 plate rides the icon's right edge"
        );

        // The name sits ON the plate (+15 from its left), vertically centred — not above-right of
        // the icon, where the pre-B250 shape put it.
        assert_eq!(
            num(&format!("{w}1Name:GetLeft()")) - num(&format!("{w}1NameFrame:GetLeft()")),
            15.0,
            "{w}1 name inset"
        );
        let (nc, pc) = (
            num(&format!("({w}1Name:GetTop() + {w}1Name:GetBottom()) / 2")),
            num(&format!(
                "({w}1NameFrame:GetTop() + {w}1NameFrame:GetBottom()) / 2"
            )),
        );
        assert!(
            (nc - pc).abs() < 0.01,
            "{w}1 name is centred on the plate ({nc} vs {pc})"
        );

        // The count rides the ICON's bottom-right corner (-4, +1) — not a line below the name.
        assert_eq!(
            num(&format!("{w}1Count:GetRight()")) - num(&format!("{w}1Icon:GetRight()")),
            -4.0,
            "{w}1 count x"
        );
        assert_eq!(
            num(&format!("{w}1Count:GetBottom()")) - num(&format!("{w}1Icon:GetBottom()")),
            1.0,
            "{w}1 count y"
        );
        assert!(
            num(&format!("{w}1Count:GetBottom()")) >= num(&format!("{w}1Icon:GetBottom()")),
            "{w}1 count sits ON the icon, not below the row"
        );
    }
}

/// **A row click paints the selection glow, and a row hover paints nothing** — the two things the
/// director saw wrong in a live window, pinned in both directions (decision 1598).
///
/// Neither was subtle. Both survived because this suite drove the window's tabs, dropdowns and
/// reagent slots without ever clicking or hovering a LIST ROW:
///
///   * `BenillaTradeSkillFrame_Update` addressed `BenillaTradeSkillHighlight` — the *texture* — for
///     the Hide/SetPoint/Show that the reference does on `TradeSkillHighlightFrame`, the *frame*
///     (Blizzard_TradeSkillUI.lua l.99/142-143; only l.200's `SetVertexColor` is the texture's).
///     The frame is declared `hidden="true"`, so it never once became visible and no selection ever
///     highlighted. `CraftFrame.xml`/`TrainerFrame.xml` both split the two correctly already.
///   * The row template carried a `GameTooltip:SetTradeSkillItem` OnEnter that the reference has no
///     trace of: `TradeSkillSkillButtonTemplate` overrides `OnClick` alone, and its base
///     `ClassTrainerSkillButtonTemplate` (`Interface\FrameXML\ClassTrainerFrameTemplates.xml`) only
///     recolours `$parentSubText` on hover. The real window's list is bare on hover.
///
/// The hover half asserts through the row's own script table rather than a synthetic mouse-over, so
/// it fails on the *existence* of a hover handler — re-adding one "harmlessly" goes red here even if
/// the tooltip it opens happens to be empty in a headless VM.
#[test]
fn a_row_click_shows_the_selection_glow_and_a_row_hover_shows_nothing() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_trade_skill(Some(state()));
    s.fire_event("TRADE_SKILL_SHOW", vec![]);

    // Row 1 is the "Mail" header, row 2 the first recipe (the two-group book `state()` builds).
    assert_eq!(
        s.eval::<String>("local _, t = GetTradeSkillInfo(2) return t")
            .unwrap(),
        "medium",
        "row 2 is a recipe, not a header (state()'s own difficulty)"
    );

    // Opening already selects `GetFirstTradeSkill()` (the OnEvent path), so the glow is up before
    // any click — exactly what the director's screenshot should have shown and didn't.
    let glow_on_row = |s: &UiScript, n: i64| {
        let (glow, row) = (
            s.eval::<f64>("return BenillaTradeSkillHighlightFrame:GetTop()")
                .unwrap(),
            s.eval::<f64>(&format!("return BenillaTradeSkillSkill{n}:GetTop()"))
                .unwrap(),
        );
        (glow - row).abs() < 0.01
    };
    assert!(
        s.eval::<bool>("return BenillaTradeSkillHighlightFrame:IsShown()")
            .unwrap(),
        "the show-time auto-selection glows — the FRAME, not just its texture"
    );
    assert!(glow_on_row(&s, 2), "and it is parked on the first recipe");

    // A click on the OTHER Mail recipe moves it, rather than leaving it at the window's TOPLEFT.
    s.run("BenillaTradeSkillSkill3:Click()").unwrap();
    pump(&mut s);
    assert_eq!(
        s.eval::<i64>("return GetTradeSkillSelectionIndex()")
            .unwrap(),
        3,
        "the click selected row 3"
    );
    assert!(
        s.eval::<bool>("return BenillaTradeSkillHighlightFrame:IsShown()")
            .unwrap()
            && glow_on_row(&s, 3),
        "the glow followed the click to row 3"
    );

    // Fold every group away and no recipe row is visible to carry it.
    s.run("BenillaTradeSkillCollapseAllButton:Click()").unwrap();
    pump(&mut s);
    assert!(
        !s.eval::<bool>("return BenillaTradeSkillHighlightFrame:IsShown()")
            .unwrap(),
        "no recipe row on screen → no glow (headers never take the selection)"
    );

    // The hover half. Positive control first, so a `GetScript` that answered nil for everything
    // could not quietly pass the real assertions below: the reagent slot DOES tooltip on hover.
    assert!(
        s.eval::<bool>("return BenillaTradeSkillReagent1:GetScript(\"OnEnter\") ~= nil")
            .unwrap(),
        "control: a reagent slot has an OnEnter, so GetScript reports real handlers"
    );
    // The list row has no hover script at all, in the template or as a global.
    for handler in ["OnEnter", "OnLeave"] {
        assert!(
            s.eval::<bool>(&format!(
                "return BenillaTradeSkillSkill2:GetScript(\"{handler}\") == nil"
            ))
            .unwrap(),
            "a list row must carry no {handler} — the reference's rows never tooltip"
        );
    }
    for f in [
        "BenillaTradeSkillSkillButton_OnEnter",
        "BenillaTradeSkillSkillButton_OnLeave",
    ] {
        assert!(
            s.eval::<bool>(&format!("return getglobal(\"{f}\") == nil"))
                .unwrap(),
            "{f} must not exist"
        );
    }
}

/// **The recipe list lights its rows white — under the cursor, and on the selected one.** The
/// director put the real client's Blacksmithing window next to ours: there, the hovered row and the
/// selected row are both white, while every other recipe wears its difficulty colour. Ours wore the
/// difficulty colour everywhere, hover and selection included.
///
/// One mechanism does both, and it is the row BUTTON's, not a script's.
/// `ClassTrainerSkillButtonTemplate` — the base under this window's rows, Craft's and the class
/// trainer's — declares `<HighlightFont inherits="GameFontHighlight">` (white) beside its
/// `<NormalFont>`, and `TradeSkillFrame_Update` paints each row with
/// `skillButton:SetTextColor(difficulty)` and then calls `LockHighlight()` on the selected one
/// (Blizzard_TradeSkillUI.lua l.113/144). A `SetTextColor` writes the NORMAL font instance only, so
/// it cannot reach a highlighted label — which is exactly why the reference's rows still turn white.
///
/// Ours could not, for two reasons that had to be fixed together (decision 1605): the row's label
/// was a child `$parentName` FontString rather than the button's own `<ButtonText>`, so no per-state
/// font could reach it; and the engine's highlighted label fell back to the normal state's colour,
/// so even a ButtonText would have stayed orange. This test is the end-to-end pin — it reads the
/// colour off the extracted text quad, not off any Lua state, so it fails if either half regresses.
#[test]
fn a_hovered_or_selected_recipe_row_paints_its_label_white() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_screen_size(1024.0, 768.0);
    s.set_trade_skill(Some(state()));
    s.fire_event("TRADE_SKILL_SHOW", vec![]);

    // Row 1 is the "Mail" header; rows 2 and 3 are its two Medium recipes, and opening the window
    // auto-selects the first of them.
    assert_eq!(
        s.eval::<i64>("return GetTradeSkillSelectionIndex()")
            .unwrap(),
        2,
        "the show-time auto-selection landed on the first recipe"
    );

    // The label really is the button's own ButtonText now — a child FontString would leave
    // GetFontString() nil and take every per-state font with it.
    assert!(
        s.eval::<bool>("return BenillaTradeSkillSkill2:GetFontString() ~= nil")
            .unwrap(),
        "the row label is the Button's ButtonText, the only region per-state fonts reach"
    );

    let row_color = |s: &mut UiScript, n: i64| -> [f32; 4] {
        let text = s
            .eval::<String>(&format!("return BenillaTradeSkillSkill{n}:GetText()"))
            .unwrap();
        s.resolve();
        s.extract()
            .into_iter()
            .find_map(|q| match q.content {
                benilla_ui::script::QuadContent::Text {
                    text: Some(t),
                    color,
                    ..
                } if t == text => color,
                _ => None,
            })
            .unwrap_or_else(|| panic!("no text quad for row {n} (\"{text}\")"))
    };
    let park_cursor_off_the_list = |s: &mut UiScript| {
        s.resolve();
        s.mouse_move(1000.0, 20.0);
    };
    let hover_row = |s: &mut UiScript, n: i64| {
        s.resolve();
        let (x, y) = (
            s.eval::<f64>(&format!(
                "return (BenillaTradeSkillSkill{n}:GetLeft() + BenillaTradeSkillSkill{n}:GetRight()) / 2"
            ))
            .unwrap(),
            s.eval::<f64>(&format!(
                "return (BenillaTradeSkillSkill{n}:GetTop() + BenillaTradeSkillSkill{n}:GetBottom()) / 2"
            ))
            .unwrap(),
        );
        s.mouse_move(x as f32, y as f32);
    };

    const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
    const MEDIUM: [f32; 4] = [1.0, 1.0, 0.0, 1.0]; // TradeSkillTypeColor["medium"], the book's own

    park_cursor_off_the_list(&mut s);
    assert_eq!(
        row_color(&mut s, 2),
        WHITE,
        "the SELECTED row is white with the cursor nowhere near it — LockHighlight, not a repaint"
    );
    assert_eq!(
        row_color(&mut s, 3),
        MEDIUM,
        "an unselected, unhovered recipe wears its difficulty colour"
    );

    hover_row(&mut s, 3);
    assert_eq!(
        row_color(&mut s, 3),
        WHITE,
        "hovered: the HighlightFont instance is in force over SetTextColor's difficulty paint"
    );
    assert_eq!(
        row_color(&mut s, 2),
        WHITE,
        "and the selected row stays lit while another row is hovered"
    );

    // Click row 3: the white follows the selection, and row 2 falls back to its difficulty colour.
    s.run("BenillaTradeSkillSkill3:Click()").unwrap();
    pump(&mut s);
    park_cursor_off_the_list(&mut s);
    assert_eq!(
        s.eval::<i64>("return GetTradeSkillSelectionIndex()")
            .unwrap(),
        3
    );
    assert_eq!(row_color(&mut s, 3), WHITE, "the new selection is white");
    assert_eq!(
        row_color(&mut s, 2),
        MEDIUM,
        "the old selection is UNLOCKED back to its difficulty colour, not left lit"
    );
}

/// **The craft list lights the same way, off the same base template.** `CraftButtonTemplate` and
/// `TradeSkillSkillButtonTemplate` both inherit `ClassTrainerSkillButtonTemplate`, so its
/// `<HighlightFont inherits="GameFontHighlight">` is one mechanism serving both windows — and
/// `Craft_Update` locks its selected row exactly as the tradeskill one does (Blizzard_CraftUI.lua
/// l.234). Ours had the same `$parentName`-FontString workaround in both files, so fixing only the
/// window the director was looking at would have left the enchanting book wrong beside it
/// (decision 1605).
#[test]
fn a_hovered_or_selected_craft_row_paints_its_label_white() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_screen_size(1024.0, 768.0);

    let recipe = |spell_id: u32, name: &str| CraftRecipe {
        spell_id,
        name: name.into(),
        sub_name: String::new(),
        difficulty: TradeSkillDifficulty::Medium,
        num_available: 1,
        icon: Some("Interface\\Icons\\Spell_Holy_Heal".into()),
        description: None,
        needs_item_target: false,
        reagents: vec![],
        tools: vec![],
        tooltip: CraftTooltip::Spell(spell_id),
        spell_level: 0,
    };
    s.set_craft(Some(CraftState {
        name: "Enchanting".into(),
        rank: 100,
        max_rank: 150,
        craft_type: 3,
        recipes: vec![
            recipe(7420, "Enchant Bracer - Minor Health"),
            recipe(7426, "Enchant Chest - Minor Absorption"),
        ],
    }));
    s.fire_event("CRAFT_SHOW", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    assert!(
        s.eval::<bool>("return BenillaCraft1:GetFontString() ~= nil")
            .unwrap(),
        "the craft row name is the Button's ButtonText"
    );

    let row_color = |s: &mut UiScript, n: i64| -> [f32; 4] {
        let text = s
            .eval::<String>(&format!("return BenillaCraft{n}:GetText()"))
            .unwrap();
        s.resolve();
        s.extract()
            .into_iter()
            .find_map(|q| match q.content {
                benilla_ui::script::QuadContent::Text {
                    text: Some(t),
                    color,
                    ..
                } if t == text => color,
                _ => None,
            })
            .unwrap_or_else(|| panic!("no text quad for craft row {n} (\"{text}\")"))
    };

    const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
    const MEDIUM: [f32; 4] = [1.0, 1.0, 0.0, 1.0]; // CraftTypeColor["medium"]

    // Row 1 is the show-time selection; row 2 is not.
    s.run("SelectCraft(1); BenillaCraftFrame_Update()").unwrap();
    s.resolve();
    s.mouse_move(1000.0, 20.0);
    assert_eq!(
        row_color(&mut s, 1),
        WHITE,
        "the selected craft row is white"
    );
    assert_eq!(
        row_color(&mut s, 2),
        MEDIUM,
        "an unselected one wears its difficulty colour"
    );

    // Hover row 2.
    s.resolve();
    let (x, y) = (
        s.eval::<f64>("return (BenillaCraft2:GetLeft() + BenillaCraft2:GetRight()) / 2")
            .unwrap(),
        s.eval::<f64>("return (BenillaCraft2:GetTop() + BenillaCraft2:GetBottom()) / 2")
            .unwrap(),
    );
    s.mouse_move(x as f32, y as f32);
    assert_eq!(row_color(&mut s, 2), WHITE, "hovered: white");

    // And the selection follows a click, releasing the old row's lock.
    s.run("SelectCraft(2); BenillaCraftFrame_Update()").unwrap();
    s.resolve();
    s.mouse_move(1000.0, 20.0);
    assert_eq!(row_color(&mut s, 2), WHITE);
    assert_eq!(
        row_color(&mut s, 1),
        MEDIUM,
        "the old selection is UNLOCKED, not left lit"
    );
}
