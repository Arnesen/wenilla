//! Drives the REAL `assets/ui/TradeFrame.xml` through the engine (decision 0592 P1) — the trade twin
//! of `mail_frame.rs`: it loads the same file chain the app does (cut to the trade window's dependency
//! prefix), pushes a synthetic two-sided offer, opens the window with the app's own `TRADE_SHOW`
//! event, and asserts the transcribed Lua actually paints — the named regions exist, both columns
//! populate from a fed `TradeState`, the money coin trios render, and the accept glow tracks the
//! `TRADE_ACCEPT_UPDATE(my, his)` args. This is the machine gate for the XML (a Lua error / missing
//! global / wrong region name fails here); the director's eye judges only the *look*.

use benilla_ui::script::{ScriptValue, TradeSideState, TradeSlotItem, TradeState, UiScript};

const UI_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/ui");

/// The trade window's load prefix — the app's own order (`ui_script/mod.rs`), members only.
/// MerchantFrame.xml rides along because TradeFrame.xml reuses its global `BenillaMoney_*` coin
/// helpers (the two gold displays), so a load error in either fails here.
const FILES: [&str; 5] = [
    "Fonts.xml",
    "MoneyFrame.xml",
    "UiPanels.xml",
    "GameTooltip.xml",
    "TradeFrame.xml",
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

fn item(item_id: u32, name: &str, count: u32, quality: u32) -> TradeSlotItem {
    TradeSlotItem {
        item_id,
        name: Some(name.to_string()),
        texture: Some("Interface\\Icons\\INV_Fabric_Linen_01".to_string()),
        count,
        quality: Some(quality),
        enchantment: None,
        link: Some(format!("|cffffffff|Hitem:{item_id}:0:0:0|h[{name}]|h|r")),
    }
}

/// A two-sided offer: we offer Linen Cloth ×5 + 1g23s45c; the partner offers Silk Cloth + 5s.
fn state() -> TradeState {
    let mut player = TradeSideState {
        gold: 12_345,
        ..Default::default()
    };
    player.slots[0] = Some(item(2589, "Linen Cloth", 5, 1));
    let mut target = TradeSideState {
        gold: 500,
        ..Default::default()
    };
    target.slots[0] = Some(item(4306, "Silk Cloth", 1, 2));
    TradeState {
        player,
        target,
        partner_name: Some("Thrall".into()),
    }
}

#[test]
fn trade_frame_loads_and_key_regions_exist() {
    let s = UiScript::new().unwrap();
    load_ui(&s);
    for name in [
        "TradeFrame",
        "TradePlayerItem1",
        "TradePlayerItem1ItemButton",
        "TradePlayerItem7", // the enchant slot
        "TradeRecipientItem1",
        "TradeRecipientItem7",
        "TradeFrameTradeButton",
        "TradeFrameCancelButton",
        "TradePlayerInputMoneyGold", // our gold is now the editable input (P2)
        "TradeRecipientMoneyFrameCoin1",
        "TradeHighlightPlayer",
        "TradeHighlightRecipientEnchant",
    ] {
        assert!(
            s.eval::<bool>(&format!("return getglobal('{name}') ~= nil"))
                .unwrap(),
            "region {name} should exist"
        );
    }
    // The window is hidden until TRADE_SHOW.
    assert!(!s.eval::<bool>("return TradeFrame:IsShown()").unwrap());
}

#[test]
fn trade_show_opens_and_both_columns_populate() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_trade(Some(state()));

    s.fire_event("TRADE_SHOW", vec![]);
    assert!(
        s.eval::<bool>("return TradeFrame:IsShown()").unwrap(),
        "the window opens on TRADE_SHOW"
    );
    // ...and it is SLOTTED into the left panel, not merely Show()n unpositioned. Without the
    // UIPanelWindows["TradeFrame"] row, ShowUIPanel takes its unregistered branch — a bare
    // frame:Show() with no SetLeftFrame placement — which still flips IsShown() but leaves the
    // window off its panel slot, so it never lands on screen (the live "OPEN_WINDOW arrived on both
    // clients, yet no window" bug). This is the assertion that catches that missing registration.
    assert_eq!(
        s.eval::<String>("return GetLeftFrame() and GetLeftFrame():GetName() or ''")
            .unwrap(),
        "TradeFrame",
        "TRADE_SHOW slots the window into the left panel"
    );

    // Our slot 1 and the partner's slot 1 painted their names + icons from the fed state.
    assert_eq!(
        s.eval::<String>("return TradePlayerItem1Name:GetText()")
            .unwrap(),
        "Linen Cloth"
    );
    assert_eq!(
        s.eval::<String>("return TradeRecipientItem1Name:GetText()")
            .unwrap(),
        "Silk Cloth"
    );
    assert!(
        s.eval::<bool>("return TradePlayerItem1ItemButtonIcon:IsShown()")
            .unwrap(),
        "a filled slot shows its icon"
    );
    // An empty slot clears its name + hides its icon.
    assert_eq!(
        s.eval::<String>("return TradePlayerItem2Name:GetText()")
            .unwrap(),
        ""
    );
    assert!(!s
        .eval::<bool>("return TradePlayerItem2ItemButtonIcon:IsShown()")
        .unwrap());

    // The partner's name paints from GetTradePartnerName().
    assert_eq!(
        s.eval::<String>("return TradeFrameRecipientNameText:GetText()")
            .unwrap(),
        "Thrall"
    );

    // The partner's read-only gold rendered a coin (5s → 1 coin); our own gold is the editable input,
    // exercised in `player_money_input_reflects_then_offers`.
    assert!(s
        .eval::<bool>("return TradeRecipientMoneyFrameCoin1:IsShown()")
        .unwrap());

    assert!(s.take_errors().is_empty(), "clean repaint");
}

#[test]
fn enchant_slot_shows_the_not_traded_note() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    let mut st = state();
    // Park an item in our enchant slot (index 6 = slot 7).
    st.player.slots[6] = Some(item(6217, "Copper Rod", 1, 1));
    s.set_trade(Some(st));
    s.fire_event("TRADE_SHOW", vec![]);
    // Slot 7 with an item but no enchant shows the coloured "will not be traded" note.
    assert_eq!(
        s.eval::<String>("return TradePlayerItem7Name:GetText()")
            .unwrap(),
        "|cffffffffWill Not Be Traded|r"
    );
    assert!(s.take_errors().is_empty());
}

#[test]
fn accept_update_drives_the_column_glows() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_trade(Some(state()));
    s.fire_event("TRADE_SHOW", vec![]);
    // Highlights start hidden (TradeFrame_Update hides all four).
    assert!(!s
        .eval::<bool>("return TradeHighlightRecipient:IsShown()")
        .unwrap());

    // The partner accepts: their column + enchant glow show, ours stay hidden.
    s.fire_event(
        "TRADE_ACCEPT_UPDATE",
        vec![ScriptValue::Int(0), ScriptValue::Int(1)],
    );
    assert!(s
        .eval::<bool>("return TradeHighlightRecipient:IsShown()")
        .unwrap());
    assert!(s
        .eval::<bool>("return TradeHighlightRecipientEnchant:IsShown()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return TradeHighlightPlayer:IsShown()")
        .unwrap());

    // We accept: our glow shows and the Trade button disables (the reference's own-accept lock).
    s.fire_event(
        "TRADE_ACCEPT_UPDATE",
        vec![ScriptValue::Int(1), ScriptValue::Int(0)],
    );
    assert!(s
        .eval::<bool>("return TradeHighlightPlayer:IsShown()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return TradeFrameTradeButton:IsEnabled() ~= 0")
        .unwrap());
    assert!(s.take_errors().is_empty());
}

#[test]
fn closing_the_window_queues_the_cancel() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_trade(Some(state()));
    s.fire_event("TRADE_SHOW", vec![]);
    let _ = s.take_trade_close();

    // The X button hides the window → OnHide → CloseTrade queues the local close/cancel verb.
    s.run("TradeFrameCloseButton:Click()").unwrap();
    assert!(!s.eval::<bool>("return TradeFrame:IsShown()").unwrap());
    assert!(s.take_trade_close(), "closing queued the CloseTrade intent");
    assert!(s.take_errors().is_empty());

    // TRADE_CLOSED from the server-driven path also hides it.
    s.fire_event("TRADE_SHOW", vec![]);
    assert!(s.eval::<bool>("return TradeFrame:IsShown()").unwrap());
    s.fire_event("TRADE_CLOSED", vec![]);
    assert!(!s.eval::<bool>("return TradeFrame:IsShown()").unwrap());
}

#[test]
fn trade_button_click_queues_accept() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_trade(Some(state()));
    s.fire_event("TRADE_SHOW", vec![]);
    let _ = s.take_trade_accept();

    s.run("TradeFrameTradeButton:Click()").unwrap();
    assert!(s.take_trade_accept(), "the Trade button queues AcceptTrade");
    assert!(s.take_errors().is_empty());
}

/// The editable player money input (decision 0592 P2): the server's PLAYER_TRADE_MONEY echo reflects
/// the accepted gold into the three boxes without re-offering (the diff-guarded SetCopper), and a
/// keystroke offers the running copper total through SetTradeMoney → the app's SET_TRADE_GOLD.
#[test]
fn player_money_input_reflects_then_offers() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_trade(Some(state())); // state().player.gold == 12345 (1g 23s 45c)
    s.fire_event("TRADE_SHOW", vec![]);

    // The three numeric boxes exist and start empty.
    for box_ in ["Gold", "Silver", "Copper"] {
        assert!(
            s.eval::<bool>(&format!(
                "return getglobal('TradePlayerInputMoney{box_}') ~= nil"
            ))
            .unwrap(),
            "money box {box_} exists"
        );
    }

    // Reflection: PLAYER_TRADE_MONEY fills the boxes from GetPlayerTradeMoney(), guarded so the
    // programmatic fill does NOT re-offer (no spurious SET_TRADE_GOLD).
    let _ = s.take_trade_money();
    s.fire_event("PLAYER_TRADE_MONEY", vec![]);
    assert_eq!(
        s.eval::<(String, String, String)>(
            "return TradePlayerInputMoneyGold:GetText(), \
             TradePlayerInputMoneySilver:GetText(), \
             TradePlayerInputMoneyCopper:GetText()"
        )
        .unwrap(),
        ("1".into(), "23".into(), "45".into()),
        "the echo reflects 1g 23s 45c into the boxes"
    );
    assert_eq!(
        s.take_trade_money(),
        None,
        "the guarded reflect does not re-offer"
    );

    // A genuine keystroke offers the running total (bypass the affordability clamp — a bare harness
    // has no purse).
    s.run("GetMoney = function() return 100000000 end").unwrap();
    s.run("TradePlayerInputMoneyGold:SetText('2')").unwrap();
    s.tick(0.0); // the deferred OnTextChanged drains here (decision 1831)
    assert_eq!(
        s.take_trade_money(),
        Some(2 * 10000 + 23 * 100 + 45),
        "typing offers the copper total via SetTradeMoney"
    );
    assert!(s.take_errors().is_empty());
}

/// The trade slot buttons route through the shared drop handler (decision 0592 P2): clicking OUR
/// filled slot clears it (ClickTradeButton → CLEAR_TRADE_ITEM), while the partner's column is inert
/// (ClickTargetTradeButton). Exercises the real button OnClick → BenillaTradeSlot_OnClick →
/// player/recipient name-dispatch, without needing a cursor payload (the empty-cursor clear path).
#[test]
fn slot_click_routes_player_to_clear_and_recipient_to_inert() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_trade(Some(state())); // player slot 1 + recipient slot 1 are both filled
    s.fire_event("TRADE_SHOW", vec![]);
    let _ = s.take_trade_clear_items();

    // Our filled slot 1 clicked with an empty cursor → a clear routed through ClickTradeButton.
    s.run("TradePlayerItem1ItemButton:Click()").unwrap();
    assert_eq!(
        s.take_trade_clear_items(),
        vec![1],
        "clicking our filled slot clears it (ClickTradeButton)"
    );

    // The partner's filled slot is read-only — the click hits ClickTargetTradeButton, which queues
    // nothing on either channel.
    s.run("TradeRecipientItem1ItemButton:Click()").unwrap();
    assert!(
        s.take_trade_clear_items().is_empty() && s.take_trade_set_items().is_empty(),
        "the partner slot is inert (ClickTargetTradeButton)"
    );
    assert!(s.take_errors().is_empty());
}
