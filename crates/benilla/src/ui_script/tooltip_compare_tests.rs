//! The shopping-compare + chat-link tooltips over the REAL shipped XMLs (decision 0274 P4):
//! shift over a bag item fires `SHOW_COMPARE_TOOLTIP`, CharacterFrame's doll-slot listener
//! (ref PaperDollFrame.lua:621-640) seats `ShoppingTooltip1` on the matching slot, the armed
//! `SetInventoryItem` renders the byte law's compare shape over the template's own adopted
//! small-font ladder, and `SetItemRef` fills the parked `ItemRefTooltip`.

use benilla_ui::script::{
    ContainerSlot, ContainerState, InvSlotView, InventorySlots, ItemTemplateView, UiScript,
    UnitState,
};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error (the bag/panel
/// tests' loader, duplicated so this file is self-contained).
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

/// The full window set the compare flow crosses: fonts, UIParent, panels, both tooltip files,
/// the bag (hover source) and the character window (the listener's doll slots).
fn harness() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for f in [
        "Fonts.xml",
        "UiPanels.xml",
        "UIParent.xml",
        "GameTooltip.xml",
        "ItemRef.xml",
        "MerchantFrame.xml",
        "Cooldown.xml",
        "BagFrame.xml",
        "CharacterFrame.xml",
    ] {
        load_xml(&s, f);
    }
    s.set_money(0);
    s
}

/// An equipped helm in the head slot + a better helm in the backpack, both with templates —
/// the compare pair.
fn seed_items(s: &mut UiScript) {
    let mut inv: InventorySlots = Default::default();
    inv[1] = Some(InvSlotView {
        bar_placeable: true,
        durability: None,
        flags: 0,
        item_id: 1234,
        icon: Some("Interface\\Icons\\INV_Helmet_01".into()),
        count: 1,
        quality: 2,
        name: Some("Test Helm".into()),
        link: Some("|cff1eff00|Hitem:1234:0:0:0|h[Test Helm]|h|r".into()),
        locked: false,
        equip_slots: vec![1],
        creator: None,
        enchants: Vec::new(),
    });
    s.set_inventory_slots(inv);
    s.set_item_template(
        1234,
        ItemTemplateView {
            name: "Test Helm".into(),
            quality: 2,
            class: 4,
            subclass: 1,
            inventory_type: 1,
            armor: 40,
            description: "Snug.".into(),
            ..Default::default()
        },
    );
    s.set_item_template(
        2000,
        ItemTemplateView {
            name: "Another Helm".into(),
            quality: 3,
            class: 4,
            subclass: 1,
            inventory_type: 1,
            armor: 55,
            ..Default::default()
        },
    );
    let mut slots = std::collections::HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            bar_placeable: true,
            durability: None,
            texture: Some("Interface\\Icons\\INV_Helmet_02".into()),
            count: 1,
            quality: Some(3),
            item_id: 2000,
            link: Some("|cff0070dd|Hitem:2000:0:0:0|h[Another Helm]|h|r".into()),
            locked: false,
            equip_slots: vec![1],
            cooldown: None,
            readable: false,
            creator: None,
            flags: 0,
            enchants: Vec::new(),
        },
    );
    s.set_container(
        0,
        Some(ContainerState {
            name: Some("Backpack".into()),
            num_slots: 16,
            slots,
        }),
    );
}

/// Shift over a bag helm with the character window OPEN: `ShoppingTooltip1` seats on the head
/// doll slot (ANCHOR_RIGHT), renders gray "Currently Equipped" + the equipped helm through the
/// template's ADOPTED small-font ladder (line 1 = GameFontNormalSmall's 10px face — the
/// engine-created lines of the MAIN tooltip stay on its own faces), the compact cut drops the
/// description, and releasing shift hides the pair. With the window CLOSED nothing shows —
/// the 1.12 behavior.
#[test]
fn shift_compare_over_a_bag_item_seats_on_the_doll_slot() {
    let mut s = harness();
    s.set_unit("player", Some(UnitState::default()));
    seed_items(&mut s);

    // Open the bag and hover the helm slot.
    s.run("BenillaBagToggle_OnClick()").unwrap();
    s.take_sounds();
    s.run(
        "for i = 1, 16 do local b = getglobal(\"BenillaBagSlot\" .. i) \
           if b and b.slot == 1 then BENILLA_TEST_BTN = b end end \
         BenillaBagSlot_OnEnter(BENILLA_TEST_BTN)",
    )
    .unwrap();
    assert!(s.errors().is_empty(), "hover errors: {:?}", s.errors());

    // Character window CLOSED: the shift edge fires, no listener answers, nothing shows.
    s.set_modifiers(true, false, false);
    assert!(
        !s.eval::<bool>("return ShoppingTooltip1:IsShown()").unwrap(),
        "no compare with the character window closed"
    );
    s.set_modifiers(false, false, false);

    // Open the window; the shift edge now seats the compare on the head slot.
    s.run(r#"ToggleCharacter("BenillaPaperDollFrame")"#)
        .unwrap();
    s.take_sounds();
    s.run("BenillaBagSlot_OnEnter(BENILLA_TEST_BTN)").unwrap();
    s.set_modifiers(true, false, false);
    assert!(s.errors().is_empty(), "compare errors: {:?}", s.errors());
    let ok: bool = s
        .eval(
            "local p, rel = ShoppingTooltip1:GetPoint() \
             return ShoppingTooltip1:IsShown() \
               and ShoppingTooltip1TextLeft1:GetText() == \"Currently Equipped\" \
               and ShoppingTooltip1TextLeft2:GetText() == \"Test Helm\" \
               and rel:GetName() == \"BenillaCharacterHeadSlot\" and p == \"BOTTOMLEFT\"",
        )
        .unwrap();
    assert!(ok, "the compare plate seats on the head doll slot");
    // The adopted ladder: the shopping plate's line 1 wears GameFontNormalSmall (10px); the
    // MAIN tooltip's engine-created line 1 keeps the header face — different sizes.
    let ok: bool = s
        .eval(
            "local _, sh = ShoppingTooltip1TextLeft1:GetFont() \
             local _, mh = GameTooltipTextLeft1:GetFont() \
             return sh == 10 and mh > sh",
        )
        .unwrap();
    assert!(
        ok,
        "the template's small-font ladder rides the compare plate"
    );
    // The compact cut: the equipped helm's description never prints on the compare plate.
    let ok: bool = s
        .eval(
            "for i = 1, ShoppingTooltip1:NumLines() do \
               if getglobal(\"ShoppingTooltip1TextLeft\" .. i):GetText() == \"\\\"Snug.\\\"\" then \
                 return false \
               end \
             end \
             return true",
        )
        .unwrap();
    assert!(ok, "compact cut drops the description");
    // One helm slot → exactly one shopping tooltip.
    assert!(
        !s.eval::<bool>("return ShoppingTooltip2:IsShown()").unwrap(),
        "a single-slot item fires one compare"
    );
    // Release hides the pair; the bag hover itself stays.
    s.set_modifiers(false, false, false);
    let ok: bool = s
        .eval("return not ShoppingTooltip1:IsShown() and GameTooltip:IsShown()")
        .unwrap();
    assert!(ok, "release hides the compare, keeps the hover");
    assert!(s.errors().is_empty(), "errors: {:?}", s.errors());
}

/// A chat item link through the ref router: `SetItemRef` shows the parked `ItemRefTooltip`
/// (BOTTOM +80, ANCHOR_PRESERVE keeps the XML seat) with the linked item's law; the corner
/// close button hides it.
#[test]
fn item_ref_tooltip_renders_a_chat_link() {
    let mut s = harness();
    seed_items(&mut s);
    s.run(
        r#"SetItemRef("item:2000", "|cff0070dd|Hitem:2000:0:0:0|h[Another Helm]|h|r", "LeftButton")"#,
    )
    .unwrap();
    assert!(s.errors().is_empty(), "link errors: {:?}", s.errors());
    let ok: bool = s
        .eval(
            "local p, rel, rp, x, y = ItemRefTooltip:GetPoint() \
             return ItemRefTooltip:IsShown() \
               and ItemRefTooltipTextLeft1:GetText() == \"Another Helm\" \
               and p == \"BOTTOM\" and y == 80",
        )
        .unwrap();
    assert!(ok, "the link tooltip shows at its parked seat");
    // The close button (ref ItemRefCloseButton): HideUIPanel drops it.
    s.run("HideUIPanel(ItemRefTooltip)").unwrap();
    assert!(
        !s.eval::<bool>("return ItemRefTooltip:IsShown()").unwrap(),
        "the close path hides the link tooltip"
    );
    assert!(s.errors().is_empty(), "errors: {:?}", s.errors());
}

/// Hovering the EQUIPPED item itself (a paper-doll slot) renders the INSTANCE — the live
/// durability pair off the slot view, never the template's authored max/max (ref
/// PaperDollItemSlotButton_OnEnter l.741: `SetInventoryItem`, not an id/template render;
/// director-caught: broken gear read 100% in the char window while the bag read it right).
/// And shift over the doll slot compares NOTHING — a worn item compared with itself is no
/// comparison: `SetInventoryItem` never arms, and its content clear drops any stale arm left
/// by an earlier bag hover.
#[test]
fn doll_hover_renders_the_live_instance_and_never_self_compares() {
    let mut s = harness();
    s.set_unit("player", Some(UnitState::default()));
    seed_items(&mut s);
    // Break the equipped helm: instance pair (0, 40); the template stays authored-full.
    let mut inv: InventorySlots = Default::default();
    inv[1] = Some(InvSlotView {
        bar_placeable: true,
        durability: Some((0, 40)),
        flags: 0,
        item_id: 1234,
        icon: Some("Interface\\Icons\\INV_Helmet_01".into()),
        count: 1,
        quality: 2,
        name: Some("Test Helm".into()),
        link: Some("|cff1eff00|Hitem:1234:0:0:0|h[Test Helm]|h|r".into()),
        locked: false,
        equip_slots: vec![1],
        creator: None,
        enchants: Vec::new(),
    });
    s.set_inventory_slots(inv);
    s.set_item_template(
        1234,
        ItemTemplateView {
            name: "Test Helm".into(),
            quality: 2,
            class: 4,
            subclass: 1,
            inventory_type: 1,
            armor: 40,
            max_durability: 40,
            ..Default::default()
        },
    );

    // Arm a compare first through a BAG hover (the stale-arm hazard the doll hover must clear).
    s.run("BenillaBagToggle_OnClick()").unwrap();
    s.take_sounds();
    s.run(
        "for i = 1, 16 do local b = getglobal(\"BenillaBagSlot\" .. i) \
           if b and b.slot == 1 then BENILLA_TEST_BTN = b end end \
         BenillaBagSlot_OnEnter(BENILLA_TEST_BTN)",
    )
    .unwrap();
    s.run(r#"ToggleCharacter("BenillaPaperDollFrame")"#)
        .unwrap();
    s.take_sounds();

    // The doll hover: the live pair, not the template's 40/40.
    s.run("BenillaPaperDollSlot_OnEnter(BenillaCharacterHeadSlot)")
        .unwrap();
    assert!(s.errors().is_empty(), "hover errors: {:?}", s.errors());
    let found: String = s
        .eval(
            "for i = 1, GameTooltip:NumLines() do \
               local t = getglobal(\"GameTooltipTextLeft\" .. i):GetText() \
               if t and string.find(t, \"Durability\") then return t end \
             end \
             return \"<none>\"",
        )
        .unwrap();
    assert_eq!(
        found, "Durability 0 / 40",
        "the doll hover carries the instance's live pair"
    );

    // Shift over the worn item: no compare — not even off the bag hover's stale arm.
    s.set_modifiers(true, false, false);
    assert!(
        !s.eval::<bool>("return ShoppingTooltip1:IsShown()").unwrap(),
        "a worn item never compares with itself"
    );
    s.set_modifiers(false, false, false);
    assert!(s.errors().is_empty(), "errors: {:?}", s.errors());
}
