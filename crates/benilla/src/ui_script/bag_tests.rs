use benilla_ui::script::{
    ContainerMove, ContainerSlot, ContainerState, ItemTemplateView, QuadContent, SoundRequest,
    UiScript,
};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error and returning the
/// frame count (the panel/loot tests' loader, duplicated so this file is self-contained).
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

/// The equipped-bag BAR icons must draw ABOVE the action-bar art, not under it. The bar buttons are
/// relocated onto `BenillaActionBarArtFrame` but are top-level frames, so they default to a lower
/// frame level than the bar's own child-hierarchy art (the ExpBar dwarf notches + metal/well art) —
/// which would then paint over the centered icons, leaving the ring but no bag icon. The OnLoad
/// `BenillaActionBarArt_SeatAbove` seats them one level above the art (the action buttons' level).
/// This locks that: no bag-slot icon quad may be covered by a higher-z art texture at its center.
#[test]
fn bag_bar_icons_draw_above_the_action_bar_art() {
    let mut s = UiScript::new().unwrap();
    // The screen the client defaults to; the action bar centers here and the bag bar lands over its
    // right end, where the dwarf-notch strip overlaps — the exact geometry that reproduced the bug.
    s.set_screen_size(1600.0, 900.0);
    // ActionBar.xml carries both the anchor target (BenillaActionBarArtFrame) and the occluder (the
    // ExpBar dwarf art); MerchantFrame.xml is BagFrame's documented purse-helper dep.
    for file in [
        "Fonts.xml",
        "UiPanels.xml",
        "MerchantFrame.xml",
        "Cooldown.xml",
        "ActionBar.xml",
        "BagFrame.xml",
    ] {
        load_xml(&s, file);
    }
    s.resolve();
    let quads = s.extract();

    // A bag-slot icon is occluded when any HIGHER-z textured quad (other than the button's own ring)
    // covers its center — i.e. the bar art draws on top of it.
    let occluded = quads
        .iter()
        .filter(|q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p.contains("UI-PaperDoll-Slot-Bag")))
        .filter(|icon| {
            let r = icon.rect.expect("a resolved icon rect");
            let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
            quads.iter().any(|q| {
                q.z > icon.z
                    && matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if !p.contains("UI-Quickslot2"))
                    && q.rect.is_some_and(|qr| qr.left <= cx && cx <= qr.right && qr.bottom <= cy && cy <= qr.top)
            })
        })
        .count();
    assert_eq!(
        occluded, 0,
        "a bag-slot icon is painted over by the action-bar art (the seat-above-the-bar fix regressed)"
    );
}

/// The backpack open/close kits (ContainerFrame.lua ContainerFrame_OnShow/OnHide, l.140 / l.120):
/// showing the window queues igBackPackOpen, hiding it queues igBackPackClose — and nothing queues
/// at load (the frame is authored hidden="true", so it never transitions on startup). Driven through
/// `BenillaBagToggle_OnClick`, the exact path both the bag button and the 'B' binding use.
#[test]
fn backpack_toggle_plays_open_and_close_kits() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    // BenillaMoney_Set (the bag's purse helper) lives in MerchantFrame.xml — the bag's documented
    // isolation dep; Fonts.xml first so both files' `inherits=` FontStrings resolve.
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "BagFrame.xml");
    s.set_money(0);

    // Hidden at load: no sound queued, the frame starts hidden.
    assert!(
        s.take_sounds().is_empty(),
        "no sound at load (never transitions)"
    );
    assert!(!s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap());

    // Toggle open → OnShow → igBackPackOpen.
    s.run("BenillaBagToggle_OnClick()").unwrap();
    assert!(s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap());
    assert_eq!(
        s.take_sounds(),
        vec![SoundRequest::KitName("igBackPackOpen".into())],
        "opening the backpack plays igBackPackOpen"
    );

    // Toggle closed → OnHide → igBackPackClose.
    s.run("BenillaBagToggle_OnClick()").unwrap();
    assert!(!s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap());
    assert_eq!(
        s.take_sounds(),
        vec![SoundRequest::KitName("igBackPackClose".into())],
        "closing the backpack plays igBackPackClose"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The 'B' binding / backpack button opens or closes EVERY bag at once (ref ToggleBackpack), not
/// just the backpack: an equipped bag (here bag 2) opens alongside it, while empty bag slots (1,3,4
/// — no container, GetContainerNumSlots == 0) stay shut. A second toggle closes them all. And ESC's
/// CloseAllWindows must sweep all of them, not only the backpack.
#[test]
fn b_toggles_all_bags_open_and_closed() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "BagFrame.xml");
    s.set_money(0);

    // Backpack (16) + one equipped bag in slot 2 (6). Bags 1/3/4 are left unset → 0 slots.
    s.set_container(
        0,
        Some(ContainerState {
            name: Some("Backpack".into()),
            num_slots: 16,
            slots: std::collections::HashMap::new(),
        }),
    );
    s.set_container(
        2,
        Some(ContainerState {
            name: Some("Small Pouch".into()),
            num_slots: 6,
            slots: std::collections::HashMap::new(),
        }),
    );

    let shown =
        |s: &mut UiScript, name: &str| s.eval::<bool>(&format!("return {name}:IsShown()")).unwrap();

    // Toggle open: backpack + bag 2 open; the empty slots do not.
    s.run("BenillaBagToggle_OnClick()").unwrap();
    let _ = s.take_sounds();
    assert!(shown(&mut s, "BenillaBagFrame"), "backpack opens");
    assert!(
        shown(&mut s, "BenillaBagFrame2"),
        "the equipped bag opens too"
    );
    assert!(
        !shown(&mut s, "BenillaBagFrame1")
            && !shown(&mut s, "BenillaBagFrame3")
            && !shown(&mut s, "BenillaBagFrame4"),
        "empty bag slots (no container) have no window to show"
    );

    // Toggle again: every open bag closes.
    s.run("BenillaBagToggle_OnClick()").unwrap();
    let _ = s.take_sounds();
    assert!(
        !shown(&mut s, "BenillaBagFrame") && !shown(&mut s, "BenillaBagFrame2"),
        "the second toggle closes them all"
    );

    // ESC's CloseAllWindows sweeps every open bag, not just the backpack.
    s.run("BenillaBagToggle_OnClick()").unwrap();
    let _ = s.take_sounds();
    assert!(
        shown(&mut s, "BenillaBagFrame2"),
        "reopened for the ESC check"
    );
    s.run("CloseAllWindows()").unwrap();
    assert!(
        !shown(&mut s, "BenillaBagFrame") && !shown(&mut s, "BenillaBagFrame2"),
        "CloseAllWindows hides every bag window"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// An open bag LIGHTS its bar button (the CheckButton ring — ref ContainerFrame_OnShow/OnHide
/// SetChecked, l.124-131/84-95), and any close clears it: the windows are the source of truth,
/// so the ring tracks opens from every path (the all-toggle, a bar click, ESC's sweep).
#[test]
fn bag_bar_buttons_light_while_their_bag_is_open() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "BagFrame.xml");
    s.set_money(0);
    s.set_container(
        0,
        Some(ContainerState {
            name: Some("Backpack".into()),
            num_slots: 16,
            slots: std::collections::HashMap::new(),
        }),
    );
    s.set_container(
        2,
        Some(ContainerState {
            name: Some("Small Pouch".into()),
            num_slots: 6,
            slots: std::collections::HashMap::new(),
        }),
    );
    let checked = |s: &mut UiScript, name: &str| {
        s.eval::<bool>(&format!("return {name}:GetChecked() and true or false"))
            .unwrap()
    };

    // Open-all: the backpack button and the equipped bag's slot light; the empty slots don't.
    s.run("BenillaBagToggle_OnClick()").unwrap();
    assert!(checked(&mut s, "BenillaBagToggle"), "backpack ring lights");
    assert!(checked(&mut s, "BenillaBagBarSlot2"), "bag 2's ring lights");
    assert!(
        !checked(&mut s, "BenillaBagBarSlot1"),
        "empty slot stays dark"
    );

    // ...and the rings actually EMIT (extract-level): exactly two CheckButtonHilight quads, the
    // toggle's owner-sized on the 37px button at the art frame's BOTTOMRIGHT −6,2 (art right
    // edge = 1024 at this screen ⇒ x[981,1018] y[2,39]).
    s.resolve();
    let rings: Vec<_> = s
        .extract()
        .into_iter()
        .filter(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                    if p.contains("CheckButtonHilight"))
        })
        .collect();
    assert_eq!(rings.len(), 2, "toggle + bag 2 rings emit, nothing else");
    let toggle_ring = rings
        .iter()
        .find_map(|q| q.rect.filter(|r| r.left == 981.0))
        .expect("the toggle's ring at the art frame's corner");
    assert_eq!(
        (
            toggle_ring.left,
            toggle_ring.bottom,
            toggle_ring.right,
            toggle_ring.top
        ),
        (981.0, 2.0, 1018.0, 39.0)
    );

    // Closing ONE window (its close button / any Hide path) clears just its ring.
    s.run("BenillaBagFrame2:Hide()").unwrap();
    assert!(
        !checked(&mut s, "BenillaBagBarSlot2"),
        "closing bag 2 clears its ring"
    );
    assert!(
        checked(&mut s, "BenillaBagToggle"),
        "the backpack ring stays"
    );

    // A bar-slot click reopens bag 2 and relights it (the click auto-toggle + the ref's
    // re-derive tail agree here). Driven as a RIGHT-click through the real input path: the ref's
    // BagSlotButtonTemplate inherits PaperDollItemSlotButtonTemplate, whose OnLoad registers
    // ("LeftButtonUp", "RightButtonUp") — PaperDollFrame.lua:86 — and BagSlotButton_OnClick reads
    // no button, so either one opens the bag. Ours registered LeftButtonUp only until 0908.
    let (cx, cy): (f64, f64) = s
        .eval(
            "return (BenillaBagBarSlot2:GetLeft() + BenillaBagBarSlot2:GetRight()) / 2, \
                    (BenillaBagBarSlot2:GetTop() + BenillaBagBarSlot2:GetBottom()) / 2",
        )
        .unwrap();
    s.mouse_button(cx as f32, cy as f32, "RightButton", true);
    s.mouse_button(cx as f32, cy as f32, "RightButton", false);
    assert!(
        checked(&mut s, "BenillaBagBarSlot2"),
        "a RIGHT-click on the bar slot reopens bag 2, exactly as a left one does"
    );

    // ESC's sweep closes everything → every ring dark.
    s.run("CloseAllWindows()").unwrap();
    assert!(
        !checked(&mut s, "BenillaBagToggle") && !checked(&mut s, "BenillaBagBarSlot2"),
        "close-all clears every ring"
    );
    let _ = s.take_sounds();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// A slot on the RIGHT half of the screen hangs its tooltip LEFT — the ref's own screen-edge
/// answer (ContainerFrameItemButton_OnEnter, ContainerFrame.lua:602-612 side-pick), which is what
/// keeps a bag tooltip from running off the right edge (the bag lives at the bottom-right).
#[test]
fn bag_tooltip_hangs_left_when_the_slot_sits_in_the_right_half() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "BagFrame.xml");
    s.set_money(0);

    let mut slots = std::collections::HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            bar_placeable: true,
            durability: None,
            texture: Some("Interface\\Icons\\INV_ThrowingKnife_02".into()),
            count: 200,
            quality: Some(1),
            item_id: 2947,
            link: Some("|cffffffff|Hitem:2947|h[Small Throwing Knife]|h|r".into()),
            locked: false,
            equip_slots: Vec::new(),
            cooldown: None,
            readable: false,
            creator: None,
            flags: 0,
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
    s.run("BenillaBagToggle_OnClick()").unwrap();
    s.take_sounds();
    s.resolve();

    // The engine speaks 1.12: GetScreenWidth serves the host-set root width.
    assert_eq!(s.eval::<f64>("return GetScreenWidth()").unwrap(), 1024.0);
    // The bag window anchors bottom-right, so every slot button is in the right half. The bag
    // numbers its buttons visually (reversed from container slots) — find the button showing
    // container slot 1, where the fixture item lives.
    s.run(
        "for i = 1, 16 do local b = getglobal(\"BenillaBagSlot\" .. i) \
           if b and b.slot == 1 then BENILLA_TEST_BTN = b end end",
    )
    .unwrap();
    let ok: bool = s
        .eval("return BENILLA_TEST_BTN:GetRight() >= GetScreenWidth() / 2")
        .unwrap();
    assert!(ok, "fixture: the slot must sit in the right half");

    s.run("BenillaBagSlot_OnEnter(BENILLA_TEST_BTN)").unwrap();
    assert!(s.errors().is_empty(), "hover errors: {:?}", s.errors());

    assert!(s.eval::<bool>("return GameTooltip:IsVisible()").unwrap());
    s.resolve();
    // ANCHOR_LEFT seats the tooltip's BOTTOMRIGHT on the slot's TOPLEFT: the whole tooltip stays
    // left of the slot, i.e. on-screen — never past the right edge.
    let ok: bool = s
        .eval(
            "return GameTooltip:GetRight() <= BENILLA_TEST_BTN:GetLeft() \
               and GameTooltip:GetRight() <= GetScreenWidth()",
        )
        .unwrap();
    assert!(ok, "tooltip hangs LEFT of a right-half slot");
}

/// A tooltip opened while the item's template is still in flight repaints itself the moment the
/// stats land — no re-hover. The refresh loop is the ref's own (ContainerFrameItemButton_OnUpdate,
/// ContainerFrame.lua:645-660: re-run OnEnter every frame while `GameTooltip:IsOwned(this)`), and
/// hiding the tooltip drops ownership so the loop can never resurrect it.
#[test]
fn hovered_bag_tooltip_fills_itself_when_the_stats_land() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "BagFrame.xml");
    s.set_money(0);

    let mut slots = std::collections::HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            bar_placeable: true,
            durability: None,
            texture: Some("Interface\\Icons\\INV_Sword_04".into()),
            count: 1,
            quality: Some(1),
            item_id: 25,
            link: Some("|cffffffff|Hitem:25|h[Worn Shortsword]|h|r".into()),
            locked: false,
            equip_slots: Vec::new(),
            cooldown: None,
            readable: false,
            creator: None,
            flags: 0,
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
    s.run("BenillaBagToggle_OnClick()").unwrap();
    s.resolve();
    s.run(
        "for i = 1, 16 do local b = getglobal(\"BenillaBagSlot\" .. i) \
           if b and b.slot == 1 then BENILLA_TEST_BTN = b end end",
    )
    .unwrap();

    // Hover with the stats store empty: the fallback one-line tooltip, and the miss recorded.
    s.run("BenillaBagSlot_OnEnter(BENILLA_TEST_BTN)").unwrap();
    assert!(s.eval::<bool>("return GameTooltip:IsVisible()").unwrap());
    assert_eq!(
        s.eval::<i64>("return GameTooltip:NumLines()").unwrap(),
        1,
        "in-flight template: the name-only fallback line"
    );
    assert_eq!(s.take_item_stat_asks(), vec![25], "the miss asks the app");

    // The template lands (the app's arrival-driven push) → the very next frame's OnUpdate
    // re-enter repaints the OPEN tooltip with the full stat head.
    s.set_item_template(
        25,
        ItemTemplateView {
            name: "Worn Shortsword".into(),
            quality: 1,
            inventory_type: 21,
            class: 2,
            subclass: 7,
            damages: vec![(1.0, 3.0, 0)],
            delay_ms: 1900,
            ..Default::default()
        },
    );
    s.tick(0.016);
    assert!(
        s.eval::<i64>("return GameTooltip:NumLines()").unwrap() > 1,
        "the stats landing repainted the open tooltip"
    );
    let has_damage: bool = s
        .eval(
            "for i = 1, GameTooltip:NumLines() do \
               local fs = getglobal(\"GameTooltipTextLeft\" .. i) \
               if fs and string.find(fs:GetText() or \"\", \"Damage\") then return true end \
             end return false",
        )
        .unwrap();
    assert!(
        has_damage,
        "the repaint carries the stat head's damage line"
    );

    // Leaving drops ownership: the loop must not resurrect the hidden tooltip.
    s.run("BenillaBagSlot_OnLeave(BENILLA_TEST_BTN)").unwrap();
    s.tick(0.016);
    assert!(
        !s.eval::<bool>("return GameTooltip:IsVisible()").unwrap(),
        "hide drops ownership; OnUpdate never resurrects"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// At a vendor, a bag hover shows the engine-truth sell-price money row (SellPrice × stack, wow-re
/// tooltip-money.md 0x52b650@0x52e376) — or the ITEM_UNSELLABLE "No sell price" line — and arms
/// the pouch cursor (ShowContainerSellCursor → Buy over a Point base, cursor-system.md §7);
/// leaving resets it.
#[test]
fn vendor_bag_hover_shows_sell_price_and_arms_the_pouch_cursor() {
    use benilla_ui::script::{MerchantState, ScriptValue, UiCursorMode};

    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "BagFrame.xml");
    s.set_money(0);

    let mut slots = std::collections::HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            bar_placeable: true,
            durability: None,
            texture: Some("Interface\\Icons\\INV_Misc_Pelt_Wolf_01".into()),
            count: 4,
            quality: Some(1),
            item_id: 2318,
            link: Some("|cffffffff|Hitem:2318|h[Light Leather]|h|r".into()),
            locked: false,
            equip_slots: Vec::new(),
            cooldown: None,
            readable: false,
            creator: None,
            flags: 0,
        },
    );
    slots.insert(
        2,
        ContainerSlot {
            bar_placeable: true,
            durability: None,
            texture: Some("Interface\\Icons\\INV_Misc_Key_03".into()),
            count: 1,
            quality: Some(1),
            item_id: 9999,
            link: Some("|cffffffff|Hitem:9999|h[Shadowforge Key]|h|r".into()),
            locked: false,
            equip_slots: Vec::new(),
            cooldown: None,
            readable: false,
            creator: None,
            flags: 0,
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
    // Sellable stack: 13c each × 4 = 52c. Unsellable: SellPrice 0.
    s.set_item_template(
        2318,
        ItemTemplateView {
            name: "Light Leather".into(),
            quality: 1,
            sell_price: 13,
            ..Default::default()
        },
    );
    s.set_item_template(
        9999,
        ItemTemplateView {
            name: "Shadowforge Key".into(),
            quality: 1,
            sell_price: 0,
            ..Default::default()
        },
    );
    s.set_merchant(Some(MerchantState::default()));
    s.fire_event("MERCHANT_SHOW", vec![ScriptValue::Str("Vendor".into())]);
    s.run("BenillaBagToggle_OnClick()").unwrap();
    s.take_sounds();
    s.resolve();
    s.run(
        "BENILLA_TEST_B1, BENILLA_TEST_B2 = nil, nil\n\
         for i = 1, 16 do local b = getglobal(\"BenillaBagSlot\" .. i)\n\
           if b and b.slot == 1 then BENILLA_TEST_B1 = b end\n\
           if b and b.slot == 2 then BENILLA_TEST_B2 = b end\n\
         end",
    )
    .unwrap();

    // The sellable stack: a money row (52c → the copper coin slot shows "52") + the pouch armed.
    s.run("BenillaBagSlot_OnEnter(BENILLA_TEST_B1)").unwrap();
    assert!(s.errors().is_empty(), "hover errors: {:?}", s.errors());
    assert!(s
        .eval::<bool>(
            "return GameTooltipMoneyCoin1:IsShown() \
             and GameTooltipMoneyCoin1Num:GetText() == '52'",
        )
        .unwrap());
    assert_eq!(
        s.ui_cursor(),
        Some(UiCursorMode::Buy),
        "the pouch cursor is armed over a sellable item"
    );

    // Leaving resets the cursor and the money row dies with the tooltip.
    s.run("BenillaBagSlot_OnLeave(BENILLA_TEST_B1)").unwrap();
    assert_eq!(s.ui_cursor(), None, "ResetCursor on leave");
    assert!(s
        .eval::<bool>("return not GameTooltipMoneyCoin1:IsShown()")
        .unwrap());

    // The unsellable item: the ITEM_UNSELLABLE line, no coins.
    s.run("BenillaBagSlot_OnEnter(BENILLA_TEST_B2)").unwrap();
    let has_line: bool = s
        .eval(
            "for i = 1, GameTooltip:NumLines() do \
               if (getglobal('GameTooltipTextLeft' .. i):GetText() or '') == 'No sell price' \
                 then return true end \
             end return false",
        )
        .unwrap();
    assert!(has_line, "SellPrice 0 shows the ITEM_UNSELLABLE line");
    assert!(s
        .eval::<bool>("return not GameTooltipMoneyCoin1:IsShown()")
        .unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// A readable bag item (a mail permanent copy — the instance carries item text) shows the
/// Inspect magnifier on hover (ref ContainerFrameItemButton_OnEnter, ContainerFrame.lua l.638:
/// `this.readable → ShowInspectCursor()`); a plain item leaves the base cursor; leaving resets.
#[test]
fn readable_letter_hover_shows_the_inspect_magnifier() {
    use benilla_ui::script::UiCursorMode;

    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "MerchantFrame.xml"); // BenillaMoney_Set — the bag window's money strip
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "BagFrame.xml");
    s.set_money(0);

    let mut slots = std::collections::HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            texture: Some("Interface\\Icons\\INV_Misc_Note_01".into()),
            count: 1,
            quality: Some(1),
            item_id: 8383,
            link: Some("|cffffffff|Hitem:8383|h[Plain Letter]|h|r".into()),
            readable: true,
            ..Default::default()
        },
    );
    slots.insert(
        2,
        ContainerSlot {
            texture: Some("Interface\\Icons\\INV_Misc_Food_16".into()),
            count: 5,
            quality: Some(1),
            item_id: 117,
            link: Some("|cffffffff|Hitem:117|h[Tough Jerky]|h|r".into()),
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
    s.run("BenillaBagToggle_OnClick()").unwrap();
    s.take_sounds();
    s.resolve();
    s.run(
        "BENILLA_TEST_B1, BENILLA_TEST_B2 = nil, nil\n\
         for i = 1, 16 do local b = getglobal(\"BenillaBagSlot\" .. i)\n\
           if b and b.slot == 1 then BENILLA_TEST_B1 = b end\n\
           if b and b.slot == 2 then BENILLA_TEST_B2 = b end\n\
         end",
    )
    .unwrap();

    s.run("BenillaBagSlot_OnEnter(BENILLA_TEST_B1)").unwrap();
    assert!(s.errors().is_empty(), "hover errors: {:?}", s.errors());
    assert_eq!(
        s.ui_cursor(),
        Some(UiCursorMode::Inspect),
        "the magnifier over the letter"
    );
    s.run("BenillaBagSlot_OnLeave(BENILLA_TEST_B1)").unwrap();
    assert_eq!(s.ui_cursor(), None, "ResetCursor on leave");

    s.run("BenillaBagSlot_OnEnter(BENILLA_TEST_B2)").unwrap();
    assert_eq!(s.ui_cursor(), None, "no magnifier over the jerky");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The drag trio (decision 0216 §3): a real press-drag-release across two slot buttons routes
/// through the SAME `BenillaBagSlot_OnClick("LeftButton")` path a two-click pickup/place does —
/// unlike every other bag test here, which calls the Lua click handler directly, this one drives
/// actual `mouse_button`/`mouse_move` so the `RegisterForDrag`/`OnDragStart`/`OnReceiveDrag` XML
/// wiring itself is under test, not just the handler body.
#[test]
fn drag_across_two_slots_queues_the_same_move_a_click_pickup_would() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "GameTooltip.xml"); // BenillaBagSlot_OnClick's :Hide() dep
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "BagFrame.xml");
    s.set_money(0);

    let mut slots = std::collections::HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            bar_placeable: true,
            durability: None,
            texture: Some("Interface\\Icons\\INV_Misc_Food_16".into()),
            count: 5,
            quality: Some(3),
            item_id: 117,
            link: Some("|cffffffff|Hitem:117|h[Tough Jerky]|h|r".into()),
            locked: false,
            equip_slots: Vec::new(),
            cooldown: None,
            readable: false,
            creator: None,
            flags: 0,
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
    s.run("BenillaBagToggle_OnClick()").unwrap();
    s.take_sounds();
    s.resolve();

    s.run(
        "BENILLA_TEST_B1, BENILLA_TEST_B5 = nil, nil\n\
         for i = 1, 16 do local b = getglobal(\"BenillaBagSlot\" .. i)\n\
           if b and b.slot == 1 then BENILLA_TEST_B1 = b end\n\
           if b and b.slot == 5 then BENILLA_TEST_B5 = b end\n\
         end",
    )
    .unwrap();
    let (x1, y1): (f64, f64) = s
        .eval(
            "return (BENILLA_TEST_B1:GetLeft() + BENILLA_TEST_B1:GetRight()) / 2, \
                    (BENILLA_TEST_B1:GetTop() + BENILLA_TEST_B1:GetBottom()) / 2",
        )
        .unwrap();
    let (x5, y5): (f64, f64) = s
        .eval(
            "return (BENILLA_TEST_B5:GetLeft() + BENILLA_TEST_B5:GetRight()) / 2, \
                    (BENILLA_TEST_B5:GetTop() + BENILLA_TEST_B5:GetBottom()) / 2",
        )
        .unwrap();

    // Press on slot 1 (picks up), drag past the threshold onto slot 5, release there.
    s.mouse_button(x1 as f32, y1 as f32, "LeftButton", true);
    s.mouse_move(x5 as f32, y5 as f32);
    let consumed = s.mouse_button(x5 as f32, y5 as f32, "LeftButton", false);
    assert!(consumed, "the drag release lands on a mouse-enabled frame");

    assert!(s.cursor_item().is_none(), "placed onto the empty slot 5");
    assert_eq!(
        s.take_container_moves(),
        vec![ContainerMove {
            src_bag: 0,
            src_slot: 1,
            dst_bag: 0,
            dst_slot: 5,
            count: None,
        }]
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// A second bag window (decision 0216 slice 2): bag 1's snapshot feeds through the SAME
/// C_Container/BenillaBagWindow_Update plumbing the backpack uses, opened via the bag-bar path
/// (`BenillaBagBarSlot_OnClick`, not the backpack toggle) and painting its own slot 1 icon.
#[test]
fn a_second_bag_window_feeds_and_paints_via_the_bag_bar() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "BagFrame.xml");
    s.set_money(0);

    let mut slots = std::collections::HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            bar_placeable: true,
            durability: None,
            texture: Some("Interface\\Icons\\INV_Misc_Gem_01".into()),
            count: 1,
            quality: Some(2),
            item_id: 200,
            link: Some("|cffffffff|Hitem:200|h[Shiny Gem]|h|r".into()),
            locked: false,
            equip_slots: Vec::new(),
            cooldown: None,
            readable: false,
            creator: None,
            flags: 0,
        },
    );
    s.set_container(
        1,
        Some(ContainerState {
            name: Some("Small Pouch".into()),
            num_slots: 6,
            slots,
        }),
    );

    assert!(
        !s.eval::<bool>("return BenillaBagFrame1:IsShown()").unwrap(),
        "hidden by default"
    );
    // BenillaBagBarSlot1 == bag id 1 (BenillaBagBarSlot_OnLoad(self, 1) in BagFrame.xml).
    s.run("BenillaBagBarSlot_OnClick(BenillaBagBarSlot1)")
        .unwrap();
    let _ = s.take_sounds();
    assert!(
        s.eval::<bool>("return BenillaBagFrame1:IsShown()").unwrap(),
        "the bag-bar click opened bag 1's window"
    );
    assert_eq!(
        s.eval::<String>("return BenillaBagFrame1Name:GetText()")
            .unwrap(),
        "Small Pouch",
        "the title reads the live GetBagName"
    );

    s.resolve();
    let painted = s.extract().iter().any(|q| {
        matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                if p.contains("INV_Misc_Gem_01"))
    });
    assert!(painted, "bag 1's slot 1 icon is on screen");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// An equipped bag's window SNUG-FITS its row count (BenillaBagWindow_FitBackground) instead of the
/// backpack's fixed 5-row/260 slab. The heights are the real client's, from
/// `ContainerFrame_GenerateFrame` in the shipped `Interface\FrameXML\ContainerFrame.lua`:
/// `height = topH + ((rows-1)*41 - 9) + 10`, with `topH` = 72 for a size%4==2 bag (its own plus-two
/// top band), 86 for a single full row, else 94. The `-9` is the reference's `firstRowPixelOffset`
/// and the `10` its fixed bottom rim; both are load-bearing — dropping the offset slides the rim a
/// row-fraction low and bleeds the next row's wells in above it. This locks that arithmetic AND the
/// core fix: a small bag is far shorter than the old fixed height.
#[test]
fn equipped_bag_window_snug_fits_its_row_count() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "BagFrame.xml");
    s.set_money(0);

    // (bag id, slot count, expected window height). 6 → 2 rows plus-two-top (72+32+10); 8 → 2 rows
    // full-top (94+32+10); 10 → 3 rows plus-two-top (72+73+10); 20 → 5 rows full-top (94+155+10).
    // The last two exercise the no-middle fork: 4 → one full row (86+0+10); 2 → one plus-two row
    // (72+0+10). Bag 1 stays at 6 so the h6 assertion below still reads the pouch.
    for (bag, size, expected) in [
        (1, 6, 114.0),
        (2, 8, 136.0),
        (3, 10, 155.0),
        (4, 20, 259.0),
        (2, 4, 96.0),
        (3, 2, 82.0),
    ] {
        s.set_container(
            bag,
            Some(ContainerState {
                name: Some(format!("Bag {bag}")),
                num_slots: size,
                slots: std::collections::HashMap::new(),
            }),
        );
        let frame = format!("BenillaBagFrame{bag}");
        s.run(&format!("BenillaBagWindow_Update({frame})")).unwrap();
        let h = s
            .eval::<f64>(&format!("return {frame}:GetHeight()"))
            .unwrap();
        assert!(
            (h - expected).abs() < 0.5,
            "bag {bag} ({size} slots): height {h}, expected {expected}"
        );
    }
    // The core regression: a 6-slot bag is much shorter than the old fixed 260-tall slab.
    let h6 = s
        .eval::<f64>("return BenillaBagFrame1:GetHeight()")
        .unwrap();
    assert!(
        h6 < 200.0,
        "a 6-slot bag must not fill the old 260 slab, got {h6}"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// A fixture backpack with a 5-stack of Tough Jerky in slot 1, the bag opened — shared setup for
/// the shift-click/split tests below. Returns the slot-1 button's screen center.
fn open_backpack_with_a_five_stack(s: &mut UiScript) -> (f32, f32) {
    load_xml(s, "Fonts.xml");
    load_xml(s, "UiPanels.xml");
    load_xml(s, "MerchantFrame.xml");
    load_xml(s, "GameTooltip.xml");
    load_xml(s, "Cooldown.xml");
    load_xml(s, "BagFrame.xml");
    load_xml(s, "StackSplit.xml");
    s.set_money(0);

    let mut slots = std::collections::HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            bar_placeable: true,
            durability: None,
            texture: Some("Interface\\Icons\\INV_Misc_Food_16".into()),
            count: 5,
            quality: Some(1),
            item_id: 117,
            link: Some("|cffffffff|Hitem:117|h[Tough Jerky]|h|r".into()),
            locked: false,
            equip_slots: Vec::new(),
            cooldown: None,
            readable: false,
            creator: None,
            flags: 0,
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
    s.run("BenillaBagToggle_OnClick()").unwrap();
    let _ = s.take_sounds();
    s.resolve();

    s.run(
        "BENILLA_TEST_BTN = nil\n\
         for i = 1, 16 do local b = getglobal(\"BenillaBagSlot\" .. i)\n\
           if b and b.slot == 1 then BENILLA_TEST_BTN = b end\n\
         end",
    )
    .unwrap();
    s.eval(
        "return (BENILLA_TEST_BTN:GetLeft() + BENILLA_TEST_BTN:GetRight()) / 2, \
                (BENILLA_TEST_BTN:GetTop() + BENILLA_TEST_BTN:GetBottom()) / 2",
    )
    .unwrap()
}

/// The stack-split trigger — SHIFT + left-click on an unlocked stack of ≥2, the reference fork
/// verbatim (ContainerFrame.lua:567-577), driven through the `set_modifiers` mirror the cursor
/// arc landed. Nothing is picked up: the spinner opens against the still-seated stack.
#[test]
fn shift_click_on_a_stack_opens_the_split_frame() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    let (x, y) = open_backpack_with_a_five_stack(&mut s);

    assert!(!s
        .eval::<bool>("return BenillaStackSplitFrame:IsShown()")
        .unwrap());

    s.set_modifiers(true, false, false);
    s.mouse_button(x, y, "LeftButton", true);
    s.mouse_button(x, y, "LeftButton", false);
    s.set_modifiers(false, false, false);
    assert!(
        s.eval::<bool>("return BenillaStackSplitFrame:IsShown()")
            .unwrap(),
        "shift-click opened the split frame"
    );
    assert!(
        s.cursor_item().is_none(),
        "the shift fork never picks the stack up"
    );
    assert_eq!(
        s.eval::<i64>("return BenillaStackSplitFrame.maxStack")
            .unwrap(),
        5
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Okay in the split spinner only picks the split carry up (ref/cursor.rs `SplitContainerItem` —
/// a pickup, not a self-contained move); a SUBSEQUENT placement is what actually queues the
/// `ContainerMove` with `count: Some(n)`, drained the same way any other container move is.
#[test]
fn split_okay_then_a_placement_queues_the_split_move() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    let (x, y) = open_backpack_with_a_five_stack(&mut s);
    s.set_modifiers(true, false, false);
    s.mouse_button(x, y, "LeftButton", true);
    s.mouse_button(x, y, "LeftButton", false);
    s.set_modifiers(false, false, false);
    assert!(s
        .eval::<bool>("return BenillaStackSplitFrame:IsShown()")
        .unwrap());

    // Bump the spinner from 1 to 3, then Okay — the carry lands on the cursor.
    s.run("BenillaStackSplitRight_Click()").unwrap();
    s.run("BenillaStackSplitRight_Click()").unwrap();
    assert_eq!(
        s.eval::<i64>("return BenillaStackSplitFrame.split")
            .unwrap(),
        3
    );
    s.run("BenillaStackSplitOkay_Click()").unwrap();
    assert!(
        !s.eval::<bool>("return BenillaStackSplitFrame:IsShown()")
            .unwrap(),
        "Okay hides the spinner"
    );
    let held = s.cursor_item().expect("Okay picked up the split carry");
    assert_eq!((held.bag, held.slot, held.count), (0, 1, Some(3)));
    assert!(
        s.take_container_moves().is_empty(),
        "no move yet — only a pickup"
    );

    // Place the carry on slot 5 (empty) — NOW the move queues, carrying the split count.
    s.run(
        "BENILLA_TEST_B5 = nil\n\
         for i = 1, 16 do local b = getglobal(\"BenillaBagSlot\" .. i)\n\
           if b and b.slot == 5 then BENILLA_TEST_B5 = b end\n\
         end",
    )
    .unwrap();
    s.run("BenillaBagSlot_OnClick(BENILLA_TEST_B5, \"LeftButton\")")
        .unwrap();
    assert!(s.cursor_item().is_none());
    assert_eq!(
        s.take_container_moves(),
        vec![ContainerMove {
            src_bag: 0,
            src_slot: 1,
            dst_bag: 0,
            dst_slot: 5,
            count: Some(3),
        }]
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// A plain click hides any open split frame (ref ContainerFrame.lua:581) — even a click on an
/// unrelated, empty slot.
#[test]
fn a_plain_click_hides_an_open_split_frame() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    let (x, y) = open_backpack_with_a_five_stack(&mut s);
    s.set_modifiers(true, false, false);
    s.mouse_button(x, y, "LeftButton", true);
    s.mouse_button(x, y, "LeftButton", false);
    s.set_modifiers(false, false, false);
    assert!(s
        .eval::<bool>("return BenillaStackSplitFrame:IsShown()")
        .unwrap());

    s.run(
        "BENILLA_TEST_B9 = nil\n\
         for i = 1, 16 do local b = getglobal(\"BenillaBagSlot\" .. i)\n\
           if b and b.slot == 9 then BENILLA_TEST_B9 = b end\n\
         end",
    )
    .unwrap();
    s.run("BenillaBagSlot_OnClick(BENILLA_TEST_B9, \"LeftButton\")")
        .unwrap();
    assert!(
        !s.eval::<bool>("return BenillaStackSplitFrame:IsShown()")
            .unwrap(),
        "the plain click on an unrelated slot hid the spinner"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Bag-slot item cooldowns through the shipped XML (decision 0263's deferral): a potion mid-
/// cooldown pushes its triple with the container snapshot; opening the bag runs the ref's
/// occupied-slot fork (`BenillaBagSlot_UpdateCooldown` → `GetContainerItemCooldown` →
/// `CooldownFrame_SetTimer`) and the slot grows a live sweep; a `BAG_UPDATE_COOLDOWN` refresh
/// with the cooldown gone hides it again.
#[test]
fn bag_slot_cooldown_sweeps_through_the_xml() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "Cooldown.xml"); // the shared CooldownFrame_SetTimer
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "BagFrame.xml");
    s.set_money(0);
    s.tick(100.0); // a nonzero clock epoch, like the engine cooldown tests

    let potion = |cooldown| ContainerSlot {
        durability: None,
        texture: Some("Interface\\Icons\\INV_Potion_49".into()),
        count: 3,
        quality: Some(1),
        item_id: 118,
        cooldown,
        ..Default::default()
    };
    let backpack = |cooldown| {
        let mut slots = std::collections::HashMap::new();
        slots.insert(1, potion(cooldown));
        ContainerState {
            name: Some("Backpack".into()),
            num_slots: 16,
            slots,
        }
    };
    // 12 s remain of the potion category's 60 s: started at GetTime 52 (absolute-start triple).
    s.set_container(0, Some(backpack(Some((52_000, 60_000, true)))));
    s.run("BenillaOpenAllBags()").unwrap();
    s.fire_event("BAG_UPDATE", vec![benilla_ui::script::ScriptValue::Int(0)]);
    s.resolve();

    let sweep = |s: &UiScript| {
        s.extract().iter().find_map(|q| match q.content {
            QuadContent::Cooldown { fraction, .. } => Some(fraction),
            _ => None,
        })
    };
    let fraction = sweep(&s).expect("the bag slot sweeps");
    assert!(
        (fraction - 0.8).abs() < 1e-3,
        "48 of 60 s elapsed: fraction {fraction}"
    );

    // The cooldown clears (a CLEAR_COOLDOWN, or it simply ran out before the re-push): the
    // refresh event re-reads the now-cold triple and hides the widget.
    s.set_container(0, Some(backpack(None)));
    s.fire_event("BAG_UPDATE_COOLDOWN", vec![]);
    assert_eq!(sweep(&s), None, "cold again after the refresh");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The bar's five bag buttons had NO hover at all — the director's "the bags are missing their
/// simple tooltips". These are the ref's plain `SetText` plates (ref-MainMenuBarBagButtons.xml
/// l.91-99 for the backpack, ref-MainMenuBarBagButtons.lua l.86-96 for the four slots), NOT the
/// two-line newbie kind the micro buttons next to them use — so what's pinned here is the label,
/// the empty-slot fallback, and that they seat BESIDE the button rather than at the screen corner.
#[test]
fn the_bar_bag_buttons_name_themselves_on_hover() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for file in [
        "Fonts.xml",
        "UIParent.xml",
        "GameTooltip.xml",
        "UiPanels.xml",
        "MerchantFrame.xml",
        "Cooldown.xml",
        "ActionBar.xml",
        "BagFrame.xml",
    ] {
        load_xml(&s, file);
    }
    s.resolve();

    // The backpack: its label plus the one bare-key binding benilla actually ships ('B').
    s.run("BenillaBagToggle_OnEnter(BenillaBagToggle)").unwrap();
    let line = s
        .eval::<String>("return GameTooltipTextLeft1:GetText()")
        .unwrap();
    assert!(
        line.starts_with("Backpack") && line.contains("(B)"),
        "the backpack names itself and its key: {line:?}"
    );
    // Beside the button, not the default corner — the ref's ANCHOR_LEFT.
    assert!(
        s.eval::<bool>("return GameTooltip.default == nil").unwrap(),
        "a bag button's plate is owner-anchored, never the default corner"
    );
    assert!(
        s.eval::<bool>("return GameTooltip:IsOwned(BenillaBagToggle)")
            .unwrap(),
        "…owned by the button it opened from"
    );

    // An empty bag slot falls back to the ref's EQUIP_CONTAINER rather than showing nothing.
    s.run("BenillaBagBarSlot_OnEnter(BenillaBagBarSlot1)")
        .unwrap();
    assert_eq!(
        s.eval::<String>("return GameTooltipTextLeft1:GetText()")
            .unwrap(),
        "Equip Container",
        "an empty slot says what belongs in it"
    );

    // With a bag actually equipped there, the ref shows that BAG's own item tooltip instead — the
    // SetInventoryItem arm. Bar slot 1 is inventory slot 20 (Bag0Slot).
    let mut inv: benilla_ui::script::InventorySlots = Default::default();
    inv[20] = Some(benilla_ui::script::InvSlotView {
        bar_placeable: true,
        durability: None,
        flags: 0,
        item_id: 4496,
        icon: Some("Interface\\Icons\\INV_Misc_Bag_08".into()),
        count: 1,
        quality: 1,
        name: Some("Small Brown Pouch".into()),
        link: Some("|cffffffff|Hitem:4496:0:0:0|h[Small Brown Pouch]|h|r".into()),
        locked: false,
        equip_slots: vec![20],
        creator: None,
    });
    s.set_inventory_slots(inv);
    s.run("BenillaBagBarSlot_OnEnter(BenillaBagBarSlot1)")
        .unwrap();
    assert_eq!(
        s.eval::<String>("return GameTooltipTextLeft1:GetText()")
            .unwrap(),
        "Small Brown Pouch",
        "an equipped slot shows that bag, not the empty-slot fallback"
    );

    s.run("BenillaBagBarButton_OnLeave()").unwrap();
    assert!(
        !s.eval::<bool>("return GameTooltip:IsVisible()").unwrap(),
        "leaving hides the plate"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// One keyring container fed as the app feeds it: container −2 sized by the level ladder, with a
/// key in the first slot. `size` is the count `keyring_size(level)` would give.
fn keyring(size: u32, occupied: bool) -> ContainerState {
    let mut slots = std::collections::HashMap::new();
    if occupied {
        slots.insert(
            1,
            ContainerSlot {
                // The Scarlet Key — the item the director's own character carries in keyring slot 1.
                item_id: 7146,
                count: 1,
                quality: Some(1),
                texture: Some("Interface\\Icons\\INV_Misc_Key_07".into()),
                link: Some("|cffffffff|Hitem:7146:0:0:0|h[The Scarlet Key]|h|r".into()),
                ..Default::default()
            },
        );
    }
    ContainerState {
        name: Some("Keyring".into()),
        num_slots: size,
        slots,
    }
}

/// How many drawn quads carry a texture path containing `needle`. The engine exposes no
/// `GetTexture`, so "which art is this frame wearing" is asked of the resolved draw list — which is
/// the stronger question anyway (it answers what would actually be on screen).
fn drawn_with(s: &mut UiScript, needle: &str) -> usize {
    s.resolve();
    s.extract()
        .iter()
        .filter(|q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p.contains(needle)))
        .count()
}

/// Load the whole bar+bag surface the keyring spans (its button seats on the action bar and its
/// window is a BagFrame container, so both files are load-bearing).
fn keyring_surface(s: &UiScript) {
    for file in [
        "Fonts.xml",
        "UiPanels.xml",
        "MerchantFrame.xml",
        "Cooldown.xml",
        "ErrorsFrame.xml",
        "ActionBar.xml",
        "BagFrame.xml",
    ] {
        load_xml(s, file);
    }
}

/// **The gate** (decision 0765): no key ⇒ no keyring anywhere on the bar — the button is hidden and
/// the bar's two right-hand strips wear the ordinary dwarf plate. The first key flips all of it:
/// the button appears, both strips swap to the keyring plate with the reference's own TexCoords,
/// and the performance meter slides from −227 to −235 to clear the new socket
/// (ref MainMenuBar_UpdateKeyRing, MainMenuBar.lua l.174-183).
///
/// This is the director-reported symptom itself: a character holding a key saw no keyring at all.
#[test]
fn the_first_key_puts_the_keyring_on_the_bar() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    keyring_surface(&s);
    s.set_money(0);

    // Keyless: the button is hidden and the plate is the dwarf one.
    s.set_has_key(false);
    s.run("BenillaMainMenuBar_UpdateKeyRing()").unwrap();
    assert!(
        !s.eval::<bool>("return BenillaKeyRingButton:IsShown()")
            .unwrap(),
        "no key ⇒ no keyring button"
    );
    assert_eq!(
        drawn_with(&mut s, "UI-MainMenuBar-KeyRing"),
        0,
        "no key ⇒ every bar strip wears the ordinary dwarf plate"
    );

    // A key lands. The app pushes HasKey and the wire's BAG_UPDATE reaches the button's OnEvent —
    // the exact runtime path, not a direct call.
    s.set_has_key(true);
    s.set_container(-2, Some(keyring(8, true)));
    s.fire_event("BAG_UPDATE", vec![benilla_ui::script::ScriptValue::Int(-2)]);
    assert!(
        s.eval::<bool>("return BenillaKeyRingButton:IsShown()")
            .unwrap(),
        "the first key reveals the keyring button"
    );
    assert_eq!(
        drawn_with(&mut s, "UI-MainMenuBar-KeyRing"),
        2,
        "exactly the two RIGHT-hand strips swap to the keyring plate"
    );
    for (strip, top, bottom) in [
        ("BenillaActionBarTexture2", 0.6640625, 1.0),
        ("BenillaActionBarTexture3", 0.1640625, 0.5),
    ] {
        let (t, b) = s
            .eval::<(f64, f64)>(&format!(
                "local _, _, top, bottom = {strip}:GetTexCoord() return top, bottom"
            ))
            .unwrap();
        assert!(
            (t - top).abs() < 1e-9 && (b - bottom).abs() < 1e-9,
            "{strip} keeps the reference's own band — got {t}..{b}, want {top}..{bottom}"
        );
    }
    // Geometry cross-check against the reference's own chain. The ref seats its backpack button at
    // BOTTOMRIGHT (-6, 2) and steps 37px buttons left with -5 gaps, putting KeyRingButton's left
    // edge at -234 from the bar's right; ours steps 36px buttons with -6 gaps and lands at -235.
    // Within a pixel of the real bar — which is the check that matters, because the socket the
    // button sits in is PAINTED INTO the keyring plate and cannot be nudged to meet it.
    let bar_right = s.eval::<f64>("return BenillaActionBar:GetRight()").unwrap();
    let button_left = s
        .eval::<f64>("return BenillaKeyRingButton:GetLeft()")
        .unwrap();
    assert!(
        (bar_right - button_left - 235.0).abs() < 1.5,
        "the keyring button must land in the plate's painted socket — the ref's own -234, got {}",
        bar_right - button_left
    );

    // The performance meter clears the new socket (ref l.180: -227 → -235).
    let perf_right = s
        .eval::<f64>("return BenillaPerformanceBarFrame:GetRight()")
        .unwrap();
    let bar_right = s.eval::<f64>("return BenillaActionBar:GetRight()").unwrap();
    assert!(
        (bar_right - perf_right - 235.0).abs() < 0.01,
        "the meter slid to -235 with the keyring up (got {})",
        bar_right - perf_right
    );

    // And it reverts: destroying the last key takes the keyring back off the bar (our divergence
    // from the ref's one-way saved-variable latch — see BenillaMainMenuBar_UpdateKeyRing).
    s.set_has_key(false);
    s.fire_event("BAG_UPDATE", vec![benilla_ui::script::ScriptValue::Int(-2)]);
    assert!(
        !s.eval::<bool>("return BenillaKeyRingButton:IsShown()")
            .unwrap(),
        "losing the last key takes the button away again"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The window itself: clicking the button opens a container titled "Keyring", stitched from the
/// `-Keyring` plate, showing exactly the level-gated slot count — and it jingles rather than
/// rustling (KeyRingOpen/KeyRingClose, ref ContainerFrame.lua l.116-138).
#[test]
fn the_keyring_button_opens_a_keyring_window() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    keyring_surface(&s);
    s.set_money(0);
    s.set_has_key(true);
    // A level-44 character: 8 usable slots (the ladder's 40..49 rung).
    s.set_container(-2, Some(keyring(8, true)));
    s.fire_event("BAG_UPDATE", vec![benilla_ui::script::ScriptValue::Int(-2)]);
    let _ = s.take_sounds();

    assert!(!s
        .eval::<bool>("return BenillaKeyRingFrame:IsShown()")
        .unwrap());
    s.run("BenillaKeyRingButton_OnClick(BenillaKeyRingButton)")
        .unwrap();
    assert!(
        s.eval::<bool>("return BenillaKeyRingFrame:IsShown()")
            .unwrap(),
        "the button opens the keyring"
    );
    assert_eq!(
        s.take_sounds(),
        vec![SoundRequest::KitName("KeyRingOpen".into())],
        "the keyring has its own open kit, not the backpack's"
    );
    assert_eq!(
        s.eval::<String>("return BenillaKeyRingFrameName:GetText()")
            .unwrap(),
        "Keyring",
        "titled from the KEYRING string, not a bag item's name"
    );
    assert!(
        drawn_with(&mut s, "UI-Bag-Components-Keyring") > 0,
        "stitched from the keyring plate, not the ordinary bag sheet"
    );
    // 8 usable slots: buttons 1..8 shown, the template's remaining 12 hidden — the ordinary
    // physIndex-past-size branch, no keyring-specific code.
    let shown = s
        .eval::<i64>(
            "local n = 0 for i = 1, 20 do \
             if getglobal('BenillaKeyRingFrameSlot' .. i):IsShown() then n = n + 1 end end return n",
        )
        .unwrap();
    assert_eq!(shown, 8, "only the level-unlocked keyring slots are drawn");
    // The key is in the window (slot 1 of 8 is the LAST chain button — size - physIndex + 1).
    assert_eq!(
        s.eval::<i64>("return C_Container.GetContainerItemID(KEYRING_CONTAINER, 1)")
            .unwrap(),
        7146
    );

    s.run("BenillaKeyRingButton_OnClick(BenillaKeyRingButton)")
        .unwrap();
    assert!(!s
        .eval::<bool>("return BenillaKeyRingFrame:IsShown()")
        .unwrap());
    assert_eq!(
        s.take_sounds(),
        vec![SoundRequest::KitName("KeyRingClose".into())],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Dropping a held key on the button files it in the first free slot (ref PutKeyInKeyRing), and a
/// full keyring refuses out loud — where the reference prints nothing at all, having passed a
/// GlobalStrings name that was never defined.
#[test]
fn a_key_dropped_on_the_button_files_itself() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    keyring_surface(&s);
    s.set_money(0);
    s.set_has_key(true);

    // A 4-slot keyring with slot 1 taken, and the key on the cursor (picked out of the backpack).
    s.set_container(-2, Some(keyring(4, true)));
    s.set_container(
        0,
        Some(ContainerState {
            name: Some("Backpack".into()),
            num_slots: 16,
            slots: std::collections::HashMap::from([(
                3,
                ContainerSlot {
                    item_id: 7146,
                    count: 1,
                    ..Default::default()
                },
            )]),
        }),
    );
    s.fire_event("BAG_UPDATE", vec![benilla_ui::script::ScriptValue::Int(-2)]);
    s.run("C_Container.PickupContainerItem(0, 3)").unwrap();
    assert!(s.eval::<bool>("return CursorHasItem()").unwrap());

    s.run("BenillaKeyRingButton_OnClick(BenillaKeyRingButton)")
        .unwrap();
    assert_eq!(
        s.take_container_moves(),
        vec![ContainerMove {
            src_bag: 0,
            src_slot: 3,
            dst_bag: -2,
            dst_slot: 2,
            count: None,
        }],
        "filed into the FIRST FREE keyring slot (1 is taken), not the backpack"
    );

    // Now full: the click refuses with the error line instead of queueing anything.
    let mut full = keyring(4, true);
    for slot in 2..=4 {
        full.slots.insert(
            slot,
            ContainerSlot {
                item_id: 7146,
                count: 1,
                ..Default::default()
            },
        );
    }
    s.set_container(-2, Some(full));
    s.run("C_Container.PickupContainerItem(0, 3)").unwrap();
    s.run("BenillaKeyRingButton_OnClick(BenillaKeyRingButton)")
        .unwrap();
    assert!(
        s.take_container_moves().is_empty(),
        "a full keyring queues no move"
    );
    assert_eq!(
        s.eval::<String>("return BenillaErrorsFrameLine1:GetText()")
            .unwrap(),
        "Your keyring is full.",
        "and says so — the reference's own dangling string name prints nothing"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The item-push drop animation (decision 0887): `ITEM_PUSH(container, icon)` runs the pushed item's
/// icon down into **that** container's bag-bar button and nobody else's, along the curves read out of
/// `ForcedBackpackItem.m2` — pop in, hang, fall, shrink away — and hides itself at the end.
///
/// This is the whole observable contract of `BenillaItemPushAnim_*`: the routing (which button), the
/// motion (starts a fall above the button, lands centred on it), the fade/scale shape (the file's
/// keys), and the CLAMP end (one play, then gone). It is also the regression net for the OnUpdate
/// gate — an anim frame left shown would keep ticking forever.
#[test]
fn an_item_push_drops_its_icon_into_the_bag_that_took_it() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1600.0, 900.0);
    for file in [
        "Fonts.xml",
        "UiPanels.xml",
        "MerchantFrame.xml",
        "Cooldown.xml",
        "ActionBar.xml",
        "BagFrame.xml",
    ] {
        load_xml(&s, file);
    }
    s.resolve();

    let shown = |s: &UiScript, name: &str| {
        s.eval::<bool>(&format!("return {name}ItemAnim:IsShown()"))
            .unwrap()
    };
    // The card's centre relative to its button's centre, in screen px (y-up): (dx, dy).
    let offset = |s: &UiScript, name: &str| -> (f32, f32) {
        s.eval::<(f32, f32)>(&format!(
            "local a, b = {name}ItemAnim, {name} \
             local ax, ay = a:GetLeft() + a:GetWidth() / 2, a:GetBottom() + a:GetHeight() / 2 \
             local bx, by = b:GetLeft() + b:GetWidth() / 2, b:GetBottom() + b:GetHeight() / 2 \
             return ax - bx, ay - by"
        ))
        .unwrap()
    };
    let size = |s: &UiScript, name: &str| {
        s.eval::<f32>(&format!("return {name}ItemAnim:GetWidth()"))
            .unwrap()
    };
    let alpha = |s: &UiScript, name: &str| {
        s.eval::<f32>(&format!("return {name}ItemAnim:GetAlpha()"))
            .unwrap()
    };

    // Nothing is animating until a push arrives.
    for b in [
        "BenillaBagToggle",
        "BenillaBagBarSlot2",
        "BenillaKeyRingButton",
    ] {
        assert!(!shown(&s, b), "{b}'s card starts hidden");
    }

    // A push into equipped bag 2.
    s.fire_event(
        "ITEM_PUSH",
        vec![
            benilla_ui::script::ScriptValue::Int(2),
            benilla_ui::script::ScriptValue::Str("Interface\\Icons\\INV_Misc_Bag_08".into()),
        ],
    );
    s.resolve();
    assert!(shown(&s, "BenillaBagBarSlot2"), "bag 2's card plays");
    for b in [
        "BenillaBagToggle",
        "BenillaBagBarSlot1",
        "BenillaKeyRingButton",
    ] {
        assert!(!shown(&s, b), "{b} took nothing, so {b} animates nothing");
    }
    // The pushed icon reaches the RENDERER, not just the Lua state — exactly one quad carries it.
    let drawn = |s: &UiScript, icon: &str| {
        s.extract()
            .iter()
            .filter(
                |q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p == icon),
            )
            .count()
    };
    assert_eq!(
        drawn(&s, "Interface\\Icons\\INV_Misc_Bag_08"),
        1,
        "the card wears the pushed item's icon (ITEM_PUSH's arg2)"
    );

    // t=0: invisible, full size, held a whole fall above the button (and a drift to its left) —
    // the .m2's alpha key (0.000, 0.0), scale key (0.000, 1.0), translation (0.000, (0,0)).
    let (dx, dy) = offset(&s, "BenillaBagBarSlot2");
    assert!(
        (dy - 48.0).abs() < 0.5 && (dx + 12.0).abs() < 0.5,
        "starts one fall (48px) above and one drift (12px) left of the button: got ({dx}, {dy})"
    );
    assert!(
        alpha(&s, "BenillaBagBarSlot2") < 0.01,
        "fades in from nothing"
    );

    // t=0.133: fully faded in, at the 1.2x swell — the pop the director sees above the bar.
    s.tick(0.133);
    s.resolve();
    assert!(
        (alpha(&s, "BenillaBagBarSlot2") - 1.0).abs() < 0.02,
        "opaque by the alpha track's second key"
    );
    assert!(
        (size(&s, "BenillaBagBarSlot2") - 36.0 * 1.2).abs() < 0.5,
        "swollen to 1.2x the 36px icon at the scale track's peak"
    );

    // t=0.267: back to the icon's own size, still opaque, still parked.
    s.tick(0.134);
    s.resolve();
    assert!(
        (size(&s, "BenillaBagBarSlot2") - 36.0).abs() < 0.5,
        "settled back to the 36px icon at the scale track's third key"
    );
    let (_, dy) = offset(&s, "BenillaBagBarSlot2");
    assert!((dy - 48.0).abs() < 0.5, "has not started falling: got {dy}");

    // t=0.5: STILL PARKED — the translation track's second key is (0,0), so the whole first half
    // second is a hang above the bar. The other two tracks are already running down, though: the
    // three curves keep different schedules, which is the thing a "hold everything then drop"
    // reading would get wrong (scale/alpha ≈ 0.686/0.682 here, the 0.267→1.000 ramps at t=0.5).
    s.tick(0.233);
    s.resolve();
    let (_, dy) = offset(&s, "BenillaBagBarSlot2");
    assert!(
        (dy - 48.0).abs() < 0.5,
        "has not moved yet at the half second: got {dy}"
    );
    assert!(
        (size(&s, "BenillaBagBarSlot2") - 36.0 * 0.686).abs() < 0.5,
        "but is already dwindling: got {}",
        size(&s, "BenillaBagBarSlot2")
    );
    assert!(
        (alpha(&s, "BenillaBagBarSlot2") - 0.682).abs() < 0.02,
        "and already fading: got {}",
        alpha(&s, "BenillaBagBarSlot2")
    );

    // t=0.75: halfway down.
    s.tick(0.25);
    s.resolve();
    let (_, dy) = offset(&s, "BenillaBagBarSlot2");
    assert!(
        (dy - 24.0).abs() < 0.5,
        "half the fall travelled by t=0.75: got {dy}"
    );

    // Just short of the end: landed on the button, shrunk to nothing, faded out.
    s.tick(0.24);
    s.resolve();
    let (dx, dy) = offset(&s, "BenillaBagBarSlot2");
    assert!(
        dx.abs() < 1.0 && dy.abs() < 1.5,
        "lands centred on the button it went into: got ({dx}, {dy})"
    );
    assert!(
        size(&s, "BenillaBagBarSlot2") < 2.0,
        "collapsed to the scale track's 0.014"
    );
    assert!(alpha(&s, "BenillaBagBarSlot2") < 0.05, "faded out");

    // Past 1.000s the CLAMP sequence is over — the ref's OnAnimFinished → Hide.
    s.tick(0.05);
    assert!(
        !shown(&s, "BenillaBagBarSlot2"),
        "one play, then gone (and no OnUpdate left running)"
    );

    // The keyring is a real destination, not a rounding of the backpack.
    s.fire_event(
        "ITEM_PUSH",
        vec![
            benilla_ui::script::ScriptValue::Int(-2),
            benilla_ui::script::ScriptValue::Str("Interface\\Icons\\INV_Misc_Key_03".into()),
        ],
    );
    assert!(shown(&s, "BenillaKeyRingButton"), "the keyring card plays");
    assert!(
        !shown(&s, "BenillaBagToggle"),
        "and the backpack's does not"
    );

    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
