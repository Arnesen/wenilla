//! The shipped **bank window** driven end-to-end, engine-only (no Bevy): the real
//! `assets/ui/BankFrame.xml` loaded behind `UiPanels.xml`/`BagFrame.xml`/`MerchantFrame.xml` (the
//! coin rig + the container-slot family it reuses) — decision 0604 phase 4's machine checks. Split
//! from `merchant_tests`/`bag_tests` along the folder's one-file-per-window convention.

use benilla_ui::script::{
    BankState, ContainerSlot, ContainerState, ExtractedQuad, QuadContent, ScriptValue,
    SoundRequest, UiScript,
};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error (the folder's own
/// per-file copy of the same helper — merchant_tests/bag_tests keep one each too).
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

/// The bank's own dependency chain (the app's real load order — `ui_script/mod.rs`): Fonts before
/// anything with a font `inherits`, UiPanels before any UIPanel/StaticPopup use, Cooldown+BagFrame
/// before BankFrame reuses their slot/window templates and Lua family, GameTooltip before any
/// slot's hover, MerchantFrame before BankFrame's `BenillaMoney_*` coin-rig calls.
fn setup() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "BagFrame.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "BankFrame.xml");
    s
}

fn has_icon(quads: &[ExtractedQuad], needle: &str) -> bool {
    quads.iter().any(
        |q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p.contains(needle)),
    )
}

/// Test 1 — BANKFRAME_OPENED shows the frame (through ShowUIPanel, landing at the left slot — the same
/// NPC-session chain Merchant/Trainer use), sets the title to the event arg (falling back to
/// "Banker" before any event ever fires), and opens the backpack windows (the 0561 assertion
/// pattern — `vendor_opens_and_closes_all_equipped_bags` in merchant_tests).
#[test]
fn bankframe_opened_shows_sets_title_and_opens_backpack() {
    let mut s = setup();

    // Pre-open default: the XML-authored placeholder.
    assert_eq!(
        s.eval::<String>("return BankFrameTitleText:GetText()")
            .unwrap(),
        "Banker"
    );
    assert!(!s.eval::<bool>("return BankFrame:IsVisible()").unwrap());

    let _ = s.take_sounds(); // ignore anything from load (every frame is hidden; nothing should fire)

    s.set_money(0);
    s.set_bank(Some(BankState::default()));
    s.fire_event(
        "BANKFRAME_OPENED",
        vec![ScriptValue::Str("Grumnus Steelshaper".into())],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    assert!(s.eval::<bool>("return BankFrame:IsVisible()").unwrap());
    assert_eq!(
        s.eval::<String>("return BankFrameTitleText:GetText()")
            .unwrap(),
        "Grumnus Steelshaper"
    );
    assert!(
        s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap(),
        "opening the bank opens the backpack (the 0561 OpenBackpack contract)"
    );
    assert!(
        s.eval::<bool>("return GetLeftFrame() == BankFrame")
            .unwrap(),
        "ShowUIPanel landed the bank at the left slot"
    );
    assert_eq!(
        s.take_sounds(),
        vec![
            SoundRequest::KitName("igBackPackOpen".into()),
            SoundRequest::KitName("igMainMenuOpen".into()),
        ],
        "the backpack kit leads (OpenBackpack shows it before the panel sound), then the bank's own OnShow kit"
    );
}

/// Test 2 — Hiding the frame (BANKFRAME_CLOSED, routed through HideUIPanel) queues the CloseBankFrame
/// intent (`take_bank_close()`), closes any open bank-bag popout with it, and plays the close
/// sound — the reference's own OnHide order (CloseBankBagFrames(); CloseBankFrame(); PlaySound).
#[test]
fn bankframe_closed_queues_close_closes_open_popouts_and_plays_the_close_kit() {
    let mut s = setup();
    s.set_money(0);
    s.set_bank(Some(BankState::default()));
    s.fire_event("BANKFRAME_OPENED", vec![ScriptValue::Str("Banker".into())]);
    let _ = s.take_sounds();
    let _ = s.take_bank_close(); // nothing queued yet — the drain would otherwise see a stale flag

    // A bank-bag popout open when the main window closes (ref CloseBankBagFrames' own reason to
    // exist) — Show() directly, exercising the SAME BenillaBagFrame_OnShow the equipped-bag
    // windows use (BagFrame.xml, reused verbatim).
    s.run("BenillaBankBagFrame1:Show()").unwrap();
    assert!(s
        .eval::<bool>("return BenillaBankBagFrame1:IsShown()")
        .unwrap());
    let _ = s.take_sounds();

    s.fire_event("BANKFRAME_CLOSED", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    assert!(
        !s.eval::<bool>("return BankFrame:IsVisible()").unwrap(),
        "BANKFRAME_CLOSED hides the window"
    );
    assert!(
        !s.eval::<bool>("return BenillaBankBagFrame1:IsShown()")
            .unwrap(),
        "closing the bank closes the popout it left open (CloseBankBagFrames)"
    );
    assert!(
        s.take_bank_close(),
        "OnHide queued CloseBankFrame() — the client-side close intent (no wire opcode, decision 0604)"
    );
    assert!(
        s.eval::<bool>("return GetLeftFrame() == nil").unwrap(),
        "HideUIPanel vacated the left slot"
    );
    let sounds = s.take_sounds();
    assert!(
        sounds.contains(&SoundRequest::KitName("igMainMenuClose".into())),
        "the close kit plays: {sounds:?}"
    );
}

/// Test 3 — A pushed container −1 with an item in slot 3 paints that slot button's icon;
/// PLAYERBANKSLOTS_CHANGED(3) repaints it after a later change.
#[test]
fn item_slot_paints_from_container_minus_one_and_repaints_on_playerbankslots_changed() {
    let mut s = setup();
    s.set_money(0);
    s.set_bank(Some(BankState::default()));
    s.fire_event("BANKFRAME_OPENED", vec![]);
    s.resolve();
    assert!(
        !has_icon(&s.extract(), "INV_Misc_Gem_01"),
        "nothing painted before any container push"
    );

    let mut slots = std::collections::HashMap::new();
    slots.insert(
        3,
        ContainerSlot {
            texture: Some("Interface\\Icons\\INV_Misc_Gem_01".into()),
            count: 1,
            ..Default::default()
        },
    );
    s.set_container(
        -1,
        Some(ContainerState {
            name: Some("Bank".into()),
            num_slots: 24,
            slots,
        }),
    );
    s.fire_event("PLAYERBANKSLOTS_CHANGED", vec![ScriptValue::Int(3)]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    s.resolve();
    assert!(
        has_icon(&s.extract(), "INV_Misc_Gem_01"),
        "slot 3's icon painted after PLAYERBANKSLOTS_CHANGED(3)"
    );

    // A later change to the same slot (a stack count bump) repaints again.
    let mut slots2 = std::collections::HashMap::new();
    slots2.insert(
        3,
        ContainerSlot {
            texture: Some("Interface\\Icons\\INV_Misc_Gem_01".into()),
            count: 5,
            ..Default::default()
        },
    );
    s.set_container(
        -1,
        Some(ContainerState {
            name: Some("Bank".into()),
            num_slots: 24,
            slots: slots2,
        }),
    );
    s.fire_event("PLAYERBANKSLOTS_CHANGED", vec![ScriptValue::Int(3)]);
    s.resolve();
    let quads = s.extract();
    assert!(
        quads
            .iter()
            .any(|q| matches!(&q.content, QuadContent::Text { text: Some(t), .. } if t == "5")),
        "the repaint picks up the new stack count"
    );
}

/// Test 4 — Bag buttons: with `set_bank` num_purchased=2, buttons 1-2 read normal (white) and 3-6 read
/// the red tint (1.0, 0.1, 0.1 — the reference's exact tint); `BenillaGetBankBagTexture` drives
/// button 1's icon.
#[test]
fn bag_buttons_tint_by_purchase_count_and_texture_from_the_bank_bag_feed() {
    let mut s = setup();
    s.set_money(0);
    let mut state = BankState {
        num_purchased: 2,
        next_cost: 100_000,
        ..Default::default()
    };
    state.bag_textures[0] = Some("Interface\\Icons\\INV_Misc_Bag_08".into());
    s.set_bank(Some(state));
    s.fire_event("BANKFRAME_OPENED", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    s.resolve();
    let quads = s.extract();

    // BagFrame.xml's own bag-bar slots (CharacterBag0Slot..4) fall back to this SAME empty-slot
    // texture, so the search must be scoped to each named bank button's own rect, not the path
    // alone — get each button's center via eval, then read the color off the quad sitting there.
    let center = |name: &str| -> (f32, f32) {
        let (x, y): (f64, f64) = s
            .eval(&format!(
                "return ({name}:GetLeft() + {name}:GetRight()) / 2, \
                        ({name}:GetTop() + {name}:GetBottom()) / 2"
            ))
            .unwrap();
        (x as f32, y as f32)
    };
    let color_at = |cx: f32, cy: f32, path_needle: &str| -> Option<[f32; 4]> {
        quads.iter().find_map(|q| match &q.content {
            QuadContent::Texture {
                path: Some(p),
                color,
                ..
            } if p.contains(path_needle) => q
                .rect
                .filter(|r| r.left <= cx && cx <= r.right && r.bottom <= cy && cy <= r.top)
                .map(|_| color.unwrap_or([1.0; 4])),
            _ => None,
        })
    };
    let near = |c: [f32; 4], r: f32, g: f32, b: f32| {
        (c[0] - r).abs() < 0.01 && (c[1] - g).abs() < 0.01 && (c[2] - b).abs() < 0.15
    };

    // Button 1: its own bag icon (BenillaGetBankBagTexture(1)), purchased -> white.
    let (x1, y1) = center("BankBagButton1");
    let c1 = color_at(x1, y1, "INV_Misc_Bag_08").expect("button 1 shows its fed bag icon");
    assert!(
        near(c1, 1.0, 1.0, 1.0),
        "purchased slot 1 stays white, got {c1:?}"
    );

    // Button 2 (purchased, no bag fed): the fallback texture, still white.
    let (x2, y2) = center("BankBagButton2");
    let c2 =
        color_at(x2, y2, "PaperDoll-Slot-Bag").expect("button 2 shows the empty-slot fallback");
    assert!(
        near(c2, 1.0, 1.0, 1.0),
        "purchased slot 2 stays white, got {c2:?}"
    );

    // Buttons 3..6 (unpurchased): the fallback texture, tinted the reference's exact red.
    for name in [
        "BankBagButton3",
        "BankBagButton4",
        "BankBagButton5",
        "BankBagButton6",
    ] {
        let (x, y) = center(name);
        let c = color_at(x, y, "PaperDoll-Slot-Bag")
            .unwrap_or_else(|| panic!("{name} shows the empty-slot fallback"));
        assert!(
            near(c, 1.0, 0.1, 0.1),
            "{name} (unpurchased) tints red (1.0, 0.1, 0.1), got {c:?}"
        );
    }
}

/// Test 5 — The purchase flow: the Purchase button's click shows the `CONFIRM_BUY_BANK_SLOT` popup; the
/// popup's accept queues `take_bank_purchase()`; the whole row hides once `num_purchased` reaches
/// 6 (`GetNumBankSlots()`'s `full`).
#[test]
fn purchase_flow_shows_popup_queues_the_intent_and_the_row_hides_when_full() {
    let mut s = setup();
    s.set_money(1_000_000);
    s.set_bank(Some(BankState {
        num_purchased: 2,
        next_cost: 100_000,
        ..Default::default()
    }));
    s.fire_event("BANKFRAME_OPENED", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        s.eval::<bool>("return BankFramePurchaseInfo:IsShown()")
            .unwrap(),
        "the purchase row shows while unfilled"
    );
    assert!(
        !s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "no popup before the click"
    );

    s.run("BenillaBankFramePurchaseButton_OnClick()").unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "the click shows the confirm popup"
    );
    assert!(!s.take_bank_purchase(), "nothing queued until accept");

    s.run("StaticPopup1Button1:Click()").unwrap(); // Yes
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        s.take_bank_purchase(),
        "accepting the popup queued PurchaseSlot()'s intent"
    );
    assert!(!s.take_bank_purchase(), "drained");

    // Six purchased: full -> the whole row hides (PLAYERBANKBAGSLOTS_CHANGED is the no-packet
    // buy's own repaint trigger, decision 0604).
    s.set_bank(Some(BankState {
        num_purchased: 6,
        next_cost: 999_999_999,
        ..Default::default()
    }));
    s.fire_event("PLAYERBANKBAGSLOTS_CHANGED", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        !s.eval::<bool>("return BankFramePurchaseInfo:IsShown()")
            .unwrap(),
        "the purchase row hides once full (6 purchased)"
    );
}

/// The bank's bag buttons take BOTH mouse buttons (decision 0908): the ref's
/// `BankItemButtonBagTemplate` OnLoad runs `BankFrameBagButton_OnLoad` →
/// `BankFrameBaseButton_OnLoad`, which registers `("LeftButtonUp","RightButtonUp")`
/// (BankFrame.lua:12), and `BankFrameItemButtonBag_OnClick` reads no button. Ours registered
/// nothing, so the widget default (`{"LeftButtonUp"}`) swallowed every right-click. Asserted on
/// the click sound the handler plays before any of its own forks — the one observable that does
/// not need a bag object fed into the slot.
#[test]
fn a_bank_bag_button_answers_the_right_button_too() {
    let mut s = setup();
    s.set_bank(Some(BankState::default()));
    s.fire_event("BANKFRAME_OPENED", vec![]);
    s.resolve();
    let _ = s.take_sounds();

    let (cx, cy): (f64, f64) = s
        .eval(
            "return (BankBagButton1:GetLeft() + BankBagButton1:GetRight()) / 2, \
                    (BankBagButton1:GetTop() + BankBagButton1:GetBottom()) / 2",
        )
        .unwrap();
    s.mouse_button(cx as f32, cy as f32, "RightButton", true);
    let consumed = s.mouse_button(cx as f32, cy as f32, "RightButton", false);
    assert!(consumed, "the right-click lands on the button");
    assert_eq!(
        s.take_sounds(),
        vec![benilla_ui::script::SoundRequest::KitName(
            "BAGMENUBUTTONPRESS".into()
        )],
        "a right-click runs the same OnClick a left-click does"
    );
    assert!(s.errors().is_empty(), "click errors: {:?}", s.errors());
}

/// Test 6 — Clicking item slot 1 with an empty cursor queues a `PickupContainerItem(-1, 1)` — read off
/// the cursor's own resulting payload (`bag_tests`' pattern: drive the real click through
/// `mouse_button`, not a bare `run()`, so the XML's `OnClick`/`RegisterForClicks` wiring is
/// actually under test).
#[test]
fn clicking_item_slot_one_with_an_empty_cursor_queues_the_pickup() {
    let mut s = setup();
    s.set_money(0);

    let mut slots = std::collections::HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            texture: Some("Interface\\Icons\\INV_Misc_Gem_01".into()),
            count: 1,
            item_id: 774,
            ..Default::default()
        },
    );
    s.set_container(
        -1,
        Some(ContainerState {
            name: Some("Bank".into()),
            num_slots: 24,
            slots,
        }),
    );
    s.set_bank(Some(BankState::default()));
    s.fire_event("BANKFRAME_OPENED", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    s.resolve();

    assert!(s.cursor_item().is_none(), "nothing on the cursor yet");

    let (cx, cy): (f64, f64) = s
        .eval(
            "return (BankItem1:GetLeft() + BankItem1:GetRight()) / 2, \
                    (BankItem1:GetTop() + BankItem1:GetBottom()) / 2",
        )
        .unwrap();
    s.mouse_button(cx as f32, cy as f32, "LeftButton", true);
    let consumed = s.mouse_button(cx as f32, cy as f32, "LeftButton", false);
    assert!(consumed, "the click lands on a mouse-enabled frame");
    assert!(s.errors().is_empty(), "click errors: {:?}", s.errors());

    let held = s
        .cursor_item()
        .expect("the click picked the item up onto the cursor");
    assert_eq!(
        (held.bag, held.slot),
        (-1, 1),
        "PickupContainerItem(-1, 1) — bank container, slot 1"
    );
    assert!(
        s.take_container_moves().is_empty(),
        "a bare pickup (nothing to swap into) queues no move yet"
    );
}
