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
    TradeSkillDifficulty, TradeSkillReagent, TradeSkillRecipe, TradeSkillState, UiScript,
};

const UI_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/ui");

/// The tradeskill window's load prefix — the app's own order (`ui_script/mod.rs`), members only.
/// CraftFrame.xml rides along (it loads right after TradeSkillFrame.xml in the app and shares
/// its guarded-global utilities) so a load error in EITHER window fails here.
const FILES: [&str; 7] = [
    "Fonts.xml",
    "UiPanels.xml",
    "GameTooltip.xml",
    "UIDropDownMenu.xml",
    "ScrollTemplates.xml",
    "TradeSkillFrame.xml",
    "CraftFrame.xml",
];

fn load_ui(script: &UiScript) {
    let dir = std::path::Path::new(UI_DIR);
    let provider = |req: &str| -> Option<String> {
        let norm = req.replace('\\', "/");
        let base = norm.rsplit('/').next().unwrap_or(&norm);
        std::fs::read_to_string(dir.join(&norm))
            .or_else(|_| std::fs::read_to_string(dir.join(base)))
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
