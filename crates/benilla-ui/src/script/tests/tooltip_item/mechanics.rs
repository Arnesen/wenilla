//! Render mechanics around the line law: the sell-price money protocol (OnTooltipAddMoney +
//! ITEM_UNSELLABLE), wrapped-line measurement through the re-enter loop, and the unit health
//! bar hiding on an item render.

use std::collections::HashMap;

use super::script;
use crate::script::*;

/// The sell-price money protocol: SetBagItem at an open merchant fires OnTooltipAddMoney with
/// SellPrice × stack; price 0 prints the ITEM_UNSELLABLE line; no merchant → neither; the
/// in-flight template renders the link-name fallback and records the ask.
#[test]
fn bag_item_money_law_and_fallback() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    let mut slots = HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            count: 4,
            quality: Some(1),
            item_id: 2318,
            link: Some("|cffffffff|Hitem:2318|h[Light Leather]|h|r".into()),
            ..Default::default()
        },
    );
    slots.insert(
        2,
        ContainerSlot {
            count: 1,
            quality: Some(1),
            item_id: 9999,
            link: Some("|cffffffff|Hitem:9999|h[Shadowforge Key]|h|r".into()),
            ..Default::default()
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
    s.set_item_template(
        2318,
        ItemTemplateView {
            name: "Light Leather".into(),
            quality: 1,
            sell_price: 13,
            ..Default::default()
        },
    );
    s.run(
        r#"
        money_fired = nil
        local a = CreateFrame("Button", "Slot3"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetScript("OnTooltipAddMoney", function(self) money_fired = arg1 end)
        -- No merchant: no money handler fires.
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetBagItem(0, 1)
        assert(money_fired == nil, "no merchant, no money")
    "#,
    )
    .unwrap();
    s.set_merchant(Some(MerchantState::default()));
    s.run(
        r#"
        TT:SetOwner(Slot3, "ANCHOR_RIGHT")
        TT:SetBagItem(0, 1)
        assert(money_fired == 52, "SellPrice 13 × stack 4, got " .. tostring(money_fired))
        -- The unresolved key: the link-name fallback line + nothing else.
        TT:SetOwner(Slot3, "ANCHOR_RIGHT")
        TT:SetBagItem(0, 2)
        assert(TT:NumLines() == 1, "fallback one-liner")
        assert(TTTextLeft1:GetText() == "Shadowforge Key", "fallback carries the link name")
    "#,
    )
    .unwrap();
    assert_eq!(s.take_item_stat_asks(), vec![9999], "the miss asked");
    // Zero sell price at a merchant: the ITEM_UNSELLABLE line, no money.
    s.set_item_template(
        9999,
        ItemTemplateView {
            name: "Shadowforge Key".into(),
            quality: 1,
            sell_price: 0,
            ..Default::default()
        },
    );
    s.run(
        r#"
        money_fired = nil
        TT:SetOwner(Slot3, "ANCHOR_RIGHT")
        TT:SetBagItem(0, 2)
        assert(money_fired == nil, "unsellable fires no money")
        local last = getglobal("TTTextLeft" .. TT:NumLines())
        assert(last:GetText() == "No sell price", "ITEM_UNSELLABLE line")
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}

/// A wrap-flagged line carries its wrap column on the VERY FIRST measure ask (append_line pins
/// the region width at write time), so ONE round-trip answers the wrapped extent — and the
/// plate height/width converge even when the hover's re-enter loop clears + rebuilds the
/// content every frame. The old two-step (layout re-pinning after an overflowing unwrapped
/// measure) never converged under that loop: the wrapped re-measure was wiped before it could
/// land, wrap lines contributed no height, and everything after them spilled below the plate
/// (the live bread/hearthstone bug).
#[test]
fn wrap_lines_measure_wrapped_in_one_pass_and_survive_the_reenter_loop() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_item_template(
        7,
        ItemTemplateView {
            name: "Storybook".into(),
            quality: 1,
            charges: 1,
            description:
                "An exceedingly long tale of adventure and woe that would never fit on one line."
                    .into(),
            ..Default::default()
        },
    );
    s.run(
        r#"
        local a = CreateFrame("Button", "Slot4"); a:SetPoint("TOPLEFT", 10, -10); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(a, "ANCHOR_RIGHT")
        tt:SetItemById(7)
    "#,
    )
    .unwrap();
    s.resolve();
    // ONE measure pass, the frame loop's shape: the wrap-flagged description asks WITH the
    // wrap column; plain lines ask unconstrained.
    let answer = |s: &mut UiScript| {
        let reqs = s.fontstrings_needing_measure();
        let answers: Vec<(u32, f32, f32, u64)> = reqs
            .iter()
            .map(|r| {
                if r.text.starts_with('"') {
                    assert_eq!(
                        r.wrap_width,
                        Some(crate::widget::TOOLTIP_WRAP_WIDTH),
                        "a wrap line's FIRST ask carries the wrap column"
                    );
                    (r.id, 250.0, 36.0, r.key) // wrapped: 3 rows tall, inside the column
                } else {
                    (r.id, 60.0, 14.0, r.key)
                }
            })
            .collect();
        s.set_measured_text(&answers);
        s.resolve();
    };
    answer(&mut s);
    // name 14 + gap 2 + "1 Charge" 14 + gap 2 + description 36 + 2·pad 20 = 88.
    s.run(
        r#"
        assert(TT:GetWidth() == 270, "wrap column + padding, got " .. TT:GetWidth())
        assert(TT:GetHeight() == 88, "wrap rows counted, got " .. TT:GetHeight())
    "#,
    )
    .unwrap();
    // The re-enter loop: clear + rebuild + one measure pass per frame, three frames — the size
    // must HOLD (the oscillation regression).
    for _ in 0..3 {
        s.run(
            r#"
            TT:SetOwner(Slot4, "ANCHOR_RIGHT")
            TT:SetItemById(7)
        "#,
        )
        .unwrap();
        s.resolve();
        answer(&mut s);
        s.run(
            r#"
            assert(TT:GetWidth() == 270 and TT:GetHeight() == 88,
                   "re-enter holds the wrapped size, got " .. TT:GetWidth() .. "x" .. TT:GetHeight())
        "#,
        )
        .unwrap();
    }
    assert!(s.take_errors().is_empty());
}

/// The mouseover health bar is UNIT content: an item render on the same tooltip HIDES it (the
/// live bug: the bar from the last mob hover rode under every item tooltip).
#[test]
fn item_render_hides_the_unit_health_bar() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.set_unit(
        "mouseover",
        Some(UnitState {
            exists: true,
            name: Some("Timber Wolf".into()),
            health: 30,
            max_health: 50,
            level: 10,
            reaction: 2,
            ..Default::default()
        }),
    );
    s.set_item_template(
        9,
        ItemTemplateView {
            name: "Plain Rock".into(),
            quality: 1,
            ..Default::default()
        },
    );
    s.run(
        r#"
        local a = CreateFrame("Button", "Slot6"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "GameTooltip"); tt:Hide()
        local bar = CreateFrame("StatusBar", "GameTooltipStatusBar", tt)
        bar:SetPoint("TOPLEFT", tt, "BOTTOMLEFT", 2, -1); bar:SetSize(100, 8)
    "#,
    )
    .unwrap();
    assert!(s.world_tooltip_unit("mouseover"), "the unit hover shows");
    s.run(r#"assert(GameTooltipStatusBar:IsShown(), "unit hover shows the bar")"#)
        .unwrap();
    s.run(
        r#"
        GameTooltip:SetOwner(Slot6, "ANCHOR_RIGHT")
        GameTooltip:SetItemById(9)
        assert(not GameTooltipStatusBar:IsShown(), "item content hides the unit bar")
        assert(GameTooltip:IsShown(), "the item tooltip itself shows")
    "#,
    )
    .unwrap();
    assert!(s.take_errors().is_empty());
}

/// A REAL-INSTANCE hover renders the instance's LIVE durability pair, not the template's full
/// max/max — the wire updates `ITEM_FIELD_DURABILITY` on damage (death 10%, spirit healer 25%;
/// director-reported: the 25% loss showed nowhere). A plain template/link hover keeps max/max.
#[test]
fn real_instance_hover_renders_live_durability() {
    let mut s = script();
    let mut slots = std::collections::HashMap::new();
    slots.insert(
        2,
        ContainerSlot {
            count: 1,
            quality: Some(2),
            item_id: 2264,
            link: Some("|cff1eff00|Hitem:2264|h[Mantle of Doan]|h|r".into()),
            durability: Some((30, 40)), // the spirit healer's 25% off a 40-max piece
            ..Default::default()
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
    s.set_item_template(
        2264,
        ItemTemplateView {
            name: "Mantle of Doan".into(),
            quality: 2,
            max_durability: 40,
            ..Default::default()
        },
    );
    s.run(
        r#"
        local a = CreateFrame("Button", "Slot"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(Slot, "ANCHOR_RIGHT")
        tt:SetBagItem(0, 2)
        found = nil
        for i = 1, tt:NumLines() do
            local t = getglobal("TTTextLeft" .. i):GetText()
            if t and string.find(t, "Durability") then found = t end
        end
        assert(found == "Durability 30 / 40", "live pair on a bag hover, got " .. tostring(found))
        -- The template/link hover of the SAME item keeps the authored full pair.
        tt:SetItemById(2264)
        found = nil
        for i = 1, tt:NumLines() do
            local t = getglobal("TTTextLeft" .. i):GetText()
            if t and string.find(t, "Durability") then found = t end
        end
        assert(found == "Durability 40 / 40", "template hover stays full, got " .. tostring(found))
    "#,
    )
    .unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// FULLY BROKEN gear renders its true `0 / max` pair (director-caught: gear ground to durability
/// 0 by repeated spirit rezzes read as 100% — the create block omits the zero `DURABILITY` field,
/// and the sparse `None` fell back to the template's max/max; `ObjectFields`' created semantics
/// now feed the explicit 0 through).
#[test]
fn broken_instance_hover_renders_zero_durability() {
    let mut s = script();
    let mut slots = std::collections::HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            count: 1,
            quality: Some(1),
            item_id: 2264,
            durability: Some((0, 40)),
            ..Default::default()
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
    s.set_item_template(
        2264,
        ItemTemplateView {
            name: "Mantle of Doan".into(),
            quality: 2,
            max_durability: 40,
            ..Default::default()
        },
    );
    s.run(
        r#"
        local a = CreateFrame("Button", "Slot"); a:SetPoint("CENTER", 0, 0); a:SetSize(10, 10)
        local tt = CreateFrame("GameTooltip", "TT")
        tt:SetOwner(Slot, "ANCHOR_RIGHT")
        tt:SetBagItem(0, 1)
        found = nil
        for i = 1, tt:NumLines() do
            local t = getglobal("TTTextLeft" .. i):GetText()
            if t and string.find(t, "Durability") then found = t end
        end
        assert(found == "Durability 0 / 40", "broken gear shows its true 0, got " .. tostring(found))
    "#,
    )
    .unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    // Red iff broken — the byte law ("durability (red iff broken==0)", wow-re ui.md): the 0/40
    // line paints the requirement red, never white.
    let lines = super::lines_of(&mut s);
    let dur = lines
        .iter()
        .find(|(t, _)| t.starts_with("Durability"))
        .expect("the durability line renders");
    assert_eq!(
        dur.1,
        [1.0, 32.0 / 255.0, 32.0 / 255.0, 1.0],
        "broken (0) paints red"
    );
}
