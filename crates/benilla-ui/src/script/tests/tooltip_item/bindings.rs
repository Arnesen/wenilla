//! The Set* entry points: the shift-compare seats and their CURRENTLY_EQUIPPED shape,
//! SetInventoryItem outside compare, and SetHyperlink's item-link filter.

use std::collections::HashMap;

use super::script;
use crate::script::*;

/// The shopping-compare pipeline end-to-end (0274 P4): a bag-ring hover on the main GameTooltip
/// with shift held fires `SHOW_COMPARE_TOOLTIP` once per finger slot; a ref-shaped listener
/// (PaperDollFrame.lua:621-640) seats ShoppingTooltip1/2, whose ARMED `SetInventoryItem` renders
/// the byte law's compare shape — gray "Currently Equipped", WHITE name, the compact cut (the
/// description never prints). Releasing shift hides the pair; a shift-up hover fires nothing
/// until the rising edge.
#[test]
fn shift_compare_fires_seats_and_renders_the_compare_shape() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    let mut inv: InventorySlots = Default::default();
    inv[11] = Some(InvSlotView {
        durability: None,
        item_id: 7000,
        name: Some("Old Loop".into()),
        quality: 1,
        ..Default::default()
    });
    inv[12] = Some(InvSlotView {
        durability: None,
        item_id: 7001,
        name: Some("Older Loop".into()),
        quality: 1,
        ..Default::default()
    });
    s.set_inventory_slots(inv);
    s.set_item_template(
        7000,
        ItemTemplateView {
            name: "Old Loop".into(),
            quality: 1,
            inventory_type: 11,
            description: "Round.".into(),
            ..Default::default()
        },
    );
    s.set_item_template(
        7001,
        ItemTemplateView {
            name: "Older Loop".into(),
            quality: 1,
            inventory_type: 11,
            ..Default::default()
        },
    );
    s.set_item_template(
        7002,
        ItemTemplateView {
            name: "New Loop".into(),
            quality: 2,
            inventory_type: 11,
            ..Default::default()
        },
    );
    let mut slots = HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            bar_placeable: true,
            durability: None,
            texture: None,
            count: 1,
            quality: Some(2),
            item_id: 7002,
            link: Some("|cff1eff00|Hitem:7002:0:0:0|h[New Loop]|h|r".into()),
            locked: false,
            equip_slots: vec![11, 12],
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
            num_slots: 4,
            slots,
        }),
    );
    s.run(
        r#"
        local a = CreateFrame("Button", "Slot"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        -- CreateFrame'd frames start SHOWN; the shipped XML instances are hidden="true".
        CreateFrame("GameTooltip", "GameTooltip"):Hide()
        CreateFrame("GameTooltip", "ShoppingTooltip1"):Hide()
        CreateFrame("GameTooltip", "ShoppingTooltip2"):Hide()
        compare_calls = {}
        for slot = 11, 12 do
            local f = CreateFrame("Button", "Doll" .. slot)
            f:SetPoint("CENTER", 100, 0); f:SetSize(8, 8)
            f.invSlotId = slot
            f:RegisterEvent("SHOW_COMPARE_TOOLTIP")
            f:SetScript("OnEvent", function()
                if arg1 ~= this.invSlotId or arg2 > 2 then return end
                table.insert(compare_calls, arg1 .. ":" .. arg2)
                local tooltip = getglobal("ShoppingTooltip" .. arg2)
                local anchor = "ANCHOR_RIGHT"
                if arg2 > 1 then anchor = "ANCHOR_BOTTOMRIGHT" end
                tooltip:SetOwner(this, anchor)
                local hasItem = tooltip:SetInventoryItem("player", this.invSlotId)
                if not hasItem then tooltip:Hide() end
            end)
        end
    "#,
    )
    .unwrap();
    // Shift up: the hover renders the main tooltip, no compare fires.
    s.run(
        r#"
        GameTooltip:SetOwner(Slot, "ANCHOR_RIGHT")
        GameTooltip:SetBagItem(0, 1)
        assert(table.getn(compare_calls) == 0, "no compare while shift is up")
        assert(not ShoppingTooltip1:IsShown())
    "#,
    )
    .unwrap();
    // The rising edge fires both ring slots in order.
    s.set_modifiers(true, false, false);
    s.run(
        r#"
        assert(table.getn(compare_calls) == 2, "two ring compares, got " .. table.getn(compare_calls))
        assert(compare_calls[1] == "11:1" and compare_calls[2] == "12:2", "slot:index order")
        assert(ShoppingTooltip1:IsShown() and ShoppingTooltip2:IsShown())
        assert(ShoppingTooltip1TextLeft1:GetText() == "Currently Equipped")
        assert(ShoppingTooltip1TextLeft2:GetText() == "Old Loop")
        assert(ShoppingTooltip2TextLeft2:GetText() == "Older Loop")
        -- The compact cut at 0x52e14c: the description never prints on a compare.
        for i = 1, ShoppingTooltip1:NumLines() do
            assert(getglobal("ShoppingTooltip1TextLeft" .. i):GetText() ~= "\"Round.\"",
                   "compact cut dropped the description")
        end
        -- The pair anchors to the DOLL slots (ref: SetOwner(this, ...)), index 2 below-right.
        local p1, r1 = ShoppingTooltip1:GetPoint()
        local p2, r2 = ShoppingTooltip2:GetPoint()
        assert(r1:GetName() == "Doll11" and p1 == "BOTTOMLEFT", "1 rides ANCHOR_RIGHT")
        assert(r2:GetName() == "Doll12" and p2 == "TOPLEFT", "2 rides ANCHOR_BOTTOMRIGHT")
    "#,
    )
    .unwrap();
    // The compare colors: gray header, WHITE name (never the quality color) — the byte law.
    s.resolve();
    let quads = s.extract();
    let color_of = |txt: &str| {
        quads
            .iter()
            .find_map(|q| match &q.content {
                QuadContent::Text {
                    text: Some(t),
                    color,
                    ..
                } if t == txt => *color,
                _ => None,
            })
            .unwrap_or([0.0; 4])
    };
    let gray = color_of("Currently Equipped");
    assert!(
        (gray[0] - 128.0 / 255.0).abs() < 0.01 && (gray[1] - 128.0 / 255.0).abs() < 0.01,
        "Currently Equipped is gray, got {gray:?}"
    );
    let name = color_of("Old Loop");
    assert_eq!(name, [1.0, 1.0, 1.0, 1.0], "compare name is WHITE");
    // Releasing shift hides the pair; the main tooltip stays.
    s.set_modifiers(false, false, false);
    s.run(
        r#"
        assert(not ShoppingTooltip1:IsShown() and not ShoppingTooltip2:IsShown(), "release hides")
        assert(GameTooltip:IsShown(), "the item hover itself stays")
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}

/// `SetInventoryItem` outside a compare: the FULL line law (quality name, description — no cut),
/// return 1 on an occupied slot, nil on empty/foreign units.
#[test]
fn set_inventory_item_renders_full_outside_compare() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    let mut inv: InventorySlots = Default::default();
    inv[16] = Some(InvSlotView {
        durability: None,
        item_id: 8000,
        name: Some("Worn Axe".into()),
        quality: 2,
        ..Default::default()
    });
    s.set_inventory_slots(inv);
    s.set_item_template(
        8000,
        ItemTemplateView {
            name: "Worn Axe".into(),
            quality: 2,
            inventory_type: 21,
            description: "Chipped.".into(),
            ..Default::default()
        },
    );
    s.run(
        r#"
        local a = CreateFrame("Button", "Slot9"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        assert(tt:SetInventoryItem("player", 16) == 1, "occupied slot answers 1")
        assert(TTTextLeft1:GetText() == "Worn Axe")
        local quoted = false
        for i = 1, tt:NumLines() do
            if getglobal("TTTextLeft" .. i):GetText() == "\"Chipped.\"" then quoted = true end
        end
        assert(quoted, "full render keeps the description")
        assert(tt:SetInventoryItem("player", 5) == nil, "empty slot answers nil")
        assert(tt:SetInventoryItem("target", 16) == nil, "self-only feed")
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}

/// `SetHyperlink`: the full escaped chat link and the bare `item:` form both render through the
/// shared law; non-item links no-op without touching the current content.
#[test]
fn set_hyperlink_renders_items_and_ignores_other_links() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_item_template(
        7002,
        ItemTemplateView {
            name: "New Loop".into(),
            quality: 2,
            inventory_type: 11,
            ..Default::default()
        },
    );
    s.run(
        r#"
        local a = CreateFrame("Button", "Slot10"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetHyperlink("|cff1eff00|Hitem:7002:0:0:0|h[New Loop]|h|r")
        assert(TTTextLeft1:GetText() == "New Loop", "escaped link renders")
        tt:SetHyperlink("player:Bob")
        assert(TTTextLeft1:GetText() == "New Loop", "non-item link leaves the content alone")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetHyperlink("item:7002")
        assert(TTTextLeft1:GetText() == "New Loop", "bare item: form renders")
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}
