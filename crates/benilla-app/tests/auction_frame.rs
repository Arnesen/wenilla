//! Drives the REAL `assets/ui/AuctionFrame.xml` through the engine (decision 1511) — the auction
//! twin of `mail_frame.rs`: it loads the same file chain the app does (cut to the auction window's
//! dependency prefix), pushes a synthetic `AuctionState`, opens the window with the app's own
//! `AUCTION_HOUSE_SHOW`/`AUCTION_ITEM_LIST_UPDATE` events, and asserts the transcribed Lua actually
//! paints — the named regions exist, the Browse rows populate from the fed snapshot, a row past the
//! batch is hidden, and the bid/buyout gates open for an affordable row and stay shut for one the
//! player cannot pay for.

use benilla_ui::script::{
    AuctionCategory, AuctionItemRow, AuctionListState, AuctionState, AuctionSubCategory, UiScript,
    UnitState, BIDDER, LIST, OWNER,
};

const UI_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/ui");

/// The auction window's load prefix — the app's own order (`assets/ui/benilla.toc`), members only.
/// Every one of these is a real dependency: UiPanels for the panel manager + tab kit + StaticPopup
/// engine, ScrollTemplates for the faux lists, UIPanelTemplates for the button/input/checkbox
/// templates, UIDropDownMenu for the rarity capsule, MoneyFrame for `SmallMoneyFrameTemplate` +
/// the `MoneyTypeInfo` table this window registers `AUCTION_DEPOSIT` into, MerchantFrame for the
/// `BenillaMoneyInput_*` money-entry helpers.
const FILES: [&str; 9] = [
    "Fonts.xml",
    "UiPanels.xml",
    "GameTooltip.xml",
    "UIDropDownMenu.xml",
    "ScrollTemplates.xml",
    "UIPanelTemplates.xml",
    "MoneyFrame.xml",
    "MerchantFrame.xml",
    "AuctionFrame.xml",
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
        let text = std::fs::read_to_string(dir.join(file))
            .unwrap_or_else(|e| panic!("reading {file}: {e}"));
        let doc =
            benilla_ui::framexml::parse(&text).unwrap_or_else(|e| panic!("parsing {file}: {e}"));
        let report = benilla_ui::loader::load(script, &doc, &provider);
        assert!(
            report.errors.is_empty(),
            "{file} loaded with errors: {:#?}",
            report.errors
        );
    }
}

/// One browse row with the fields the window paints.
fn row(name: &str, min_bid: u32, buyout: u32, bid: u32, owner: &str) -> AuctionItemRow {
    AuctionItemRow {
        auction_id: 1,
        item_id: 2589,
        name: Some(name.to_string()),
        texture: Some("Interface\\Icons\\INV_Fabric_Linen_01".into()),
        count: 5,
        quality: Some(1),
        level: 10,
        min_bid,
        min_increment: if bid > 0 { 100 } else { 0 },
        buyout_price: buyout,
        bid_amount: bid,
        high_bidder: false,
        owner: Some(owner.to_string()),
        time_left: 4,
        link: Some("|cffffffff|Hitem:2589:0:0:0|h[Linen Cloth]|h|r".into()),
        random_property_id: 0,
    }
}

/// A session snapshot: `rows` on the Browse list, nothing on the other two, one category.
///
/// Each row gets a DISTINCT `auction_id` here, standing in for the app's own resolve: the engine
/// remembers a selection by wire id rather than by row position (auction.rs), so rows sharing an id
/// would all resolve back to the first of them.
fn state(mut rows: Vec<AuctionItemRow>) -> AuctionState {
    for (i, r) in rows.iter_mut().enumerate() {
        r.auction_id = i as u32 + 1;
    }
    let mut lists: [AuctionListState; 3] = Default::default();
    let total = rows.len() as u32;
    lists[LIST] = AuctionListState {
        rows,
        total,
        sort: vec![("bid".into(), true)],
    };
    lists[BIDDER] = AuctionListState::default();
    lists[OWNER] = AuctionListState::default();
    AuctionState {
        lists,
        categories: vec![AuctionCategory {
            class_id: 4,
            name: "Armor".into(),
            subclasses: vec![AuctionSubCategory {
                sub_id: 1,
                name: "Cloth".into(),
                has_inv_types: true,
            }],
        }],
        deposit_percent: 5,
    }
}

/// The player: a name (the Browse bid gate compares it against the row's owner), a level (the row
/// level colours off it) and a purse.
fn seat_player(s: &mut UiScript, money: u64) {
    s.set_unit(
        "player",
        Some(UnitState {
            exists: true,
            name: Some("Buyer".into()),
            level: 60,
            ..UnitState::default()
        }),
    );
    s.set_money(money);
}

#[test]
fn auction_frame_loads_and_key_regions_exist() {
    let s = UiScript::new().unwrap();
    load_ui(&s);
    for name in [
        // The window, its three panes and its three tabs.
        "AuctionFrame",
        "AuctionFrameBrowse",
        "AuctionFrameBid",
        "AuctionFrameAuctions",
        "AuctionFrameTab1",
        "AuctionFrameTab2",
        "AuctionFrameTab3",
        // One row of each list, and the parts the repaint addresses by name.
        "BrowseButton1",
        "BrowseButton1Name",
        "BrowseButton1MoneyFrameGoldButton",
        "BrowseButton1MoneyFrameCopperButton",
        "BidButton1",
        "AuctionsButton1",
        // The create form: the sell slot and both money inputs.
        "AuctionsItemButton",
        "StartPrice",
        "StartPriceGold",
        "BuyoutPrice",
        "BuyoutPriceGold",
        // The filter column and the action buttons the gates drive.
        "AuctionFilterButton1",
        "AuctionFilterButton15",
        "BrowseBidButton",
        "BrowseBuyoutButton",
        "AuctionsCreateAuctionButton",
    ] {
        assert!(
            s.eval::<bool>(&format!("return getglobal('{name}') ~= nil"))
                .unwrap(),
            "region {name} should exist"
        );
    }
    // The window is hidden until its show event.
    assert!(!s.eval::<bool>("return AuctionFrame:IsShown()").unwrap());
    assert!(s.errors().is_empty(), "load errors: {:?}", s.errors());
}

/// The window is registered `doublewide`, which is the one row of its kind in all of 1.12 — without
/// it ShowUIPanel takes its unregistered branch and the window flips `IsShown()` without landing.
#[test]
fn the_window_is_registered_doublewide() {
    let s = UiScript::new().unwrap();
    load_ui(&s);
    assert_eq!(
        s.eval::<String>("return UIPanelWindows['AuctionFrame'].area")
            .unwrap(),
        "doublewide"
    );
}

#[test]
fn auction_house_show_opens_the_window_on_the_browse_tab() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    seat_player(&mut s, 500_000);
    s.set_auction(Some(state(vec![row(
        "Linen Cloth",
        1000,
        5000,
        0,
        "Seller",
    )])));

    s.fire_event("AUCTION_HOUSE_SHOW", vec![]);
    assert!(
        s.eval::<bool>("return AuctionFrame:IsShown()").unwrap(),
        "the window opens on AUCTION_HOUSE_SHOW"
    );
    assert!(
        s.eval::<bool>("return AuctionFrameBrowse:IsShown()")
            .unwrap(),
        "and lands on the Browse tab"
    );
    assert!(!s.eval::<bool>("return AuctionFrameBid:IsShown()").unwrap());
    assert!(!s
        .eval::<bool>("return AuctionFrameAuctions:IsShown()")
        .unwrap());
    // The skin followed the tab.
    assert_eq!(
        s.eval::<String>("return AuctionFrameTopLeft:GetTexture()")
            .unwrap()
            .to_ascii_lowercase(),
        "interface\\auctionframe\\ui-auctionframe-browse-topleft"
    );

    // Tab 3 re-skins and swaps the pane.
    s.run("AuctionFrameTab_OnClick(3)").unwrap();
    assert!(s
        .eval::<bool>("return AuctionFrameAuctions:IsShown()")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return AuctionFrameTopLeft:GetTexture()")
            .unwrap()
            .to_ascii_lowercase(),
        "interface\\auctionframe\\ui-auctionframe-auction-topleft"
    );
    // Opening the Auctions tab asks the server for the owned list, once per window session.
    assert_eq!(
        s.take_auction_owner_query(),
        Some(0),
        "the Auctions pane fetches the owned list on its first show"
    );

    assert!(s.errors().is_empty(), "clean open: {:?}", s.errors());
}

/// The part that proves it works: a fed snapshot paints the Browse rows.
#[test]
fn the_browse_list_populates_from_the_fed_snapshot() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    seat_player(&mut s, 500_000);
    s.set_auction(Some(state(vec![
        // No bids yet: the row shows the seller's opening price as the current bid.
        row("Linen Cloth", 1000, 5000, 0, "Seller"),
        // Bid on: the row shows the live bid instead.
        row("Wool Cloth", 1000, 0, 2500, "Someone"),
    ])));

    s.fire_event("AUCTION_HOUSE_SHOW", vec![]);
    s.fire_event("AUCTION_ITEM_LIST_UPDATE", vec![]);

    assert_eq!(
        s.eval::<(i64, i64)>("return GetNumAuctionItems('list')")
            .unwrap(),
        (2, 2)
    );
    assert_eq!(
        s.eval::<String>("return BrowseButton1Name:GetText()")
            .unwrap(),
        "Linen Cloth"
    );
    assert_eq!(
        s.eval::<String>("return BrowseButton2Name:GetText()")
            .unwrap(),
        "Wool Cloth"
    );
    // The seller column shows the OWNER (the reference's `HighBidder` region name notwithstanding).
    assert_eq!(
        s.eval::<String>("return BrowseButton1HighBidder:GetText()")
            .unwrap(),
        "Seller"
    );
    // Row 1: no bids, so the money line reads the 10s opening price.
    assert_eq!(
        s.eval::<String>("return tostring(BrowseButton1MoneyFrameSilverButton:GetText())")
            .unwrap(),
        "10",
        "1000c with no bids paints the minimum bid"
    );
    // Row 2: bid on, so the money line reads the live 25s bid instead.
    assert_eq!(
        s.eval::<String>("return tostring(BrowseButton2MoneyFrameSilverButton:GetText())")
            .unwrap(),
        "25",
        "a bid-on row paints the live bid, not the minimum"
    );
    // …and the ZERO coins below it stay on. This is the `MoneyTypeInfo["AUCTION"]` collapse rule
    // — `showSmallerCoins`, which drops only the LEADING zeros — and it is the whole reason this
    // window is laid out on `SmallMoneyFrameTemplate` rather than the three-slot kit the rest of
    // benilla uses: that kit drops every zero denomination, so 10s 0c read as a lone "10 🥈" and a
    // 1g buyout as a lone "1 🥇" (director's report, 2026-08-22).
    assert!(
        !s.eval::<bool>("return BrowseButton1MoneyFrameGoldButton:IsShown()")
            .unwrap(),
        "no gold in 1000c, and a LEADING zero is the one thing that does collapse"
    );
    assert!(
        s.eval::<bool>("return BrowseButton1MoneyFrameCopperButton:IsShown()")
            .unwrap(),
        "the trailing zero copper stays on under showSmallerCoins"
    );
    assert_eq!(
        s.eval::<String>("return tostring(BrowseButton1MoneyFrameCopperButton:GetText())")
            .unwrap(),
        "0"
    );
    // Row 1 has a buyout, so its buyout line shows; row 2 has none, so it hides.
    assert!(s
        .eval::<bool>("return BrowseButton1BuyoutMoneyFrame:IsShown()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return BrowseButton2BuyoutMoneyFrame:IsShown()")
        .unwrap());
    // The stack count shows for a stack of 5.
    assert!(s
        .eval::<bool>("return BrowseButton1ItemCount:IsShown()")
        .unwrap());
    // The closing-time bucket resolved to a row word plus a hover detail (both GlobalStrings, so
    // both are blank without the string table — what matters is that the tooltip field is set).
    assert!(s
        .eval::<bool>("return BrowseButton1ClosingTime.tooltip ~= nil")
        .unwrap());

    // A row past the batch is hidden.
    assert!(s.eval::<bool>("return BrowseButton2:IsShown()").unwrap());
    assert!(
        !s.eval::<bool>("return BrowseButton3:IsShown()").unwrap(),
        "row 3 is past the 2-row batch and must be hidden"
    );

    assert!(s.errors().is_empty(), "clean repaint: {:?}", s.errors());
}

/// The bid/buyout gates: both start shut on every repaint and open only for the SELECTED row, and
/// only when the purse can actually cover it.
#[test]
fn the_bid_and_buyout_gates_read_the_purse() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    // 2 gold. Row 1 costs 10s to bid and 50s to buy out — affordable. Row 2 wants 5 gold.
    seat_player(&mut s, 20_000);
    s.set_auction(Some(state(vec![
        row("Linen Cloth", 1000, 5000, 0, "Seller"),
        row("Arcanite Bar", 50_000, 60_000, 0, "Seller"),
    ])));
    s.fire_event("AUCTION_HOUSE_SHOW", vec![]);
    s.fire_event("AUCTION_ITEM_LIST_UPDATE", vec![]);

    // Nothing selected: both shut.
    assert!(!s
        .eval::<bool>("return BrowseBidButton:IsEnabled()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return BrowseBuyoutButton:IsEnabled()")
        .unwrap());

    // Select the affordable row: both open, and the bid box is seated at the required bid.
    s.run("BrowseButton1:Click()").unwrap();
    assert_eq!(
        s.eval::<i64>("return GetSelectedAuctionItem('list')")
            .unwrap(),
        1
    );
    assert!(
        s.eval::<bool>("return BrowseBidButton:IsEnabled()")
            .unwrap(),
        "10s is affordable on 2g"
    );
    assert!(
        s.eval::<bool>("return BrowseBuyoutButton:IsEnabled()")
            .unwrap(),
        "50s is affordable on 2g"
    );
    assert_eq!(
        s.eval::<i64>("return BenillaMoneyInput_GetCopper('BrowseBidPrice')")
            .unwrap(),
        1000,
        "with no bids the required bid IS the minimum bid"
    );

    // Select the unaffordable row: both shut again.
    s.run("BrowseButton2:Click()").unwrap();
    assert_eq!(
        s.eval::<i64>("return GetSelectedAuctionItem('list')")
            .unwrap(),
        2
    );
    assert!(
        !s.eval::<bool>("return BrowseBidButton:IsEnabled()")
            .unwrap(),
        "5g is not affordable on 2g"
    );
    assert!(
        !s.eval::<bool>("return BrowseBuyoutButton:IsEnabled()")
            .unwrap(),
        "6g is not affordable on 2g"
    );

    // Back to row 1 and press Bid: the intent reaches the app with the seated amount.
    s.run("BrowseButton1:Click()").unwrap();
    let _ = s.take_auction_bids();
    s.run("BrowseBidButton:Click()").unwrap();
    let bids = s.take_auction_bids();
    assert_eq!(bids.len(), 1, "one bid queued, got {bids:?}");
    assert_eq!(bids[0].list, LIST);
    assert_eq!(bids[0].index, 1);
    assert_eq!(bids[0].amount, 1000);
    assert!(
        !s.eval::<bool>("return BrowseBidButton:IsEnabled()")
            .unwrap(),
        "the button disables itself so a second click cannot outrun the answer"
    );

    assert!(s.errors().is_empty(), "clean gating: {:?}", s.errors());
}

/// You cannot bid on your own auction, however much money you have — the one leg of the Browse gate
/// that is not about affordability.
#[test]
fn you_cannot_bid_on_your_own_auction() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    seat_player(&mut s, 10_000_000);
    s.set_auction(Some(state(vec![row(
        "Linen Cloth",
        1000,
        5000,
        0,
        "Buyer",
    )])));
    s.fire_event("AUCTION_HOUSE_SHOW", vec![]);
    s.fire_event("AUCTION_ITEM_LIST_UPDATE", vec![]);

    s.run("BrowseButton1:Click()").unwrap();
    assert!(
        !s.eval::<bool>("return BrowseBidButton:IsEnabled()")
            .unwrap(),
        "the row's owner is the player"
    );
    // The buyout gate has no such leg — the reference lets you buy out your own listing.
    assert!(s
        .eval::<bool>("return BrowseBuyoutButton:IsEnabled()")
        .unwrap());
    assert!(s.errors().is_empty());
}

/// A search reads every filter at once, and nothing before the button is pressed: typing a name,
/// setting a level band and clicking a class row queue no query at all until Search fires.
#[test]
fn search_reads_the_filters_and_nothing_queries_before_it() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    seat_player(&mut s, 0);
    s.set_auction(Some(state(vec![])));
    s.set_auction_can_query(true);
    s.fire_event("AUCTION_HOUSE_SHOW", vec![]);
    let _ = s.take_auction_query();

    // The class tree paints from the pushed categories.
    s.run("AuctionFrameFilters_Update()").unwrap();
    assert_eq!(
        s.eval::<String>("return AuctionFilterButton1:GetText()")
            .unwrap(),
        "Armor"
    );
    // Clicking it expands the subclass beneath it and queries NOTHING.
    s.run("AuctionFilterButton1:Click()").unwrap();
    assert!(
        s.eval::<String>("return AuctionFilterButton2:GetText()")
            .unwrap()
            .contains("Cloth"),
        "the selected class expands its subclasses in place"
    );
    assert!(
        s.take_auction_query().is_none(),
        "a filter click must not query — the selection is read when Search is pressed"
    );

    s.run("BrowseName:SetText('linen')").unwrap();
    s.run("BrowseMinLevel:SetText('10')").unwrap();
    s.run("BrowseMaxLevel:SetText('20')").unwrap();
    s.run("IsUsableCheckButton:SetChecked(1)").unwrap();
    assert!(s.take_auction_query().is_none(), "still nothing queued");

    s.run("AuctionFrameBrowse_Search()").unwrap();
    let query = s.take_auction_query().expect("Search queues one query");
    assert_eq!(query.name, "linen");
    assert_eq!(query.min_level, 10);
    assert_eq!(query.max_level, 20);
    assert_eq!(query.class, Some(1), "the selected class row, 1-based");
    assert!(query.usable_only);
    assert_eq!(query.page, 0);
    assert!(s.errors().is_empty(), "clean search: {:?}", s.errors());
}

/// The create form's gate, and the two triggers that move the deposit. The deposit is the client's
/// own arithmetic over the stack's vendor value and the RUN TIME — never over what you ask for.
#[test]
fn the_create_gate_and_the_deposit() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    seat_player(&mut s, 100_000);
    s.set_auction(Some(state(vec![])));
    s.fire_event("AUCTION_HOUSE_SHOW", vec![]);
    s.run("AuctionFrameTab_OnClick(3)").unwrap();

    // Empty slot: shut, whatever the prices say.
    assert!(
        !s.eval::<bool>("return AuctionsCreateAuctionButton:IsEnabled()")
            .unwrap(),
        "no item in the sell slot"
    );
    // The default run time is the middle one, 8 hours.
    assert_eq!(
        s.eval::<i64>("return AuctionFrameAuctions.duration")
            .unwrap(),
        480
    );
    s.run("AuctionsShortAuctionButton:Click()").unwrap();
    assert_eq!(
        s.eval::<i64>("return AuctionFrameAuctions.duration")
            .unwrap(),
        120
    );
    s.run("AuctionsLongAuctionButton:Click()").unwrap();
    assert_eq!(
        s.eval::<i64>("return AuctionFrameAuctions.duration")
            .unwrap(),
        1440
    );

    // With no item, the form does not even reach the price checks — the buyout error stays hidden.
    s.run("BenillaMoneyInput_SetCopper('StartPrice', 10000)")
        .unwrap();
    s.run("BenillaMoneyInput_SetCopper('BuyoutPrice', 5000)")
        .unwrap();
    s.run("AuctionsFrameAuctions_ValidateAuction()").unwrap();
    assert!(!s
        .eval::<bool>("return AuctionsCreateAuctionButton:IsEnabled()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return AuctionsBuyoutErrorText:IsShown()")
        .unwrap());

    // Now put something in the slot, through the real cursor path: pick a bag item up and click
    // the slot, which is `ClickAuctionSellItemButton` exactly as a drag or a drop would be.
    s.set_container(
        0,
        Some(benilla_ui::script::ContainerState {
            name: Some("Backpack".into()),
            num_slots: 16,
            slots: std::collections::HashMap::from([(
                1,
                benilla_ui::script::ContainerSlot {
                    texture: Some("Interface\\Icons\\INV_Fabric_Linen_01".into()),
                    count: 4,
                    quality: Some(1),
                    item_id: 2589,
                    link: Some("|cffffffff|Hitem:2589:0:0:0|h[Linen Cloth]|h|r".into()),
                    ..Default::default()
                },
            )]),
        }),
    );
    s.set_item_template(
        2589,
        benilla_ui::script::ItemTemplateView {
            name: "Linen Cloth".into(),
            sell_price: 25,
            ..Default::default()
        },
    );
    // A SPLIT pickup, deliberately. A whole-stack `PickupContainerItem` records `count: None`
    // ("None = the whole stack", cursor.rs), and `GetAuctionSellItemInfo` then answers a count of
    // ONE for it — so the slot would show no stack count and the deposit would be computed on a
    // single unit. That is an ENGINE gap in `script/auction.rs`'s `sell_item_info`, not this
    // window's: the window shows the count whenever the API reports more than one, which is what
    // this exercises.
    s.run("SplitContainerItem(0, 1, 2)").unwrap();
    s.run("AuctionsItemButton:Click()").unwrap();
    assert_eq!(
        s.eval::<String>("return AuctionsItemButtonName:GetText()")
            .unwrap(),
        "Linen Cloth",
        "the slot paints the item it took from the cursor"
    );
    assert_eq!(
        s.eval::<String>("return AuctionsItemButtonCount:GetText()")
            .unwrap(),
        "2"
    );
    assert!(s
        .eval::<bool>("return AuctionsItemButtonCount:IsShown()")
        .unwrap());

    // With an item in the slot, the buyout-under-start error is the one the form explains out loud.
    s.run("AuctionsFrameAuctions_ValidateAuction()").unwrap();
    assert!(
        s.eval::<bool>("return AuctionsBuyoutErrorText:IsShown()")
            .unwrap(),
        "a 50s buyout under a 1g start price is an error, and it is shown"
    );
    assert!(!s
        .eval::<bool>("return AuctionsCreateAuctionButton:IsEnabled()")
        .unwrap());

    // Clear the buyout and the form opens; pressing Create sends exactly what is on screen.
    s.run("BenillaMoneyInput_SetCopper('BuyoutPrice', 0)")
        .unwrap();
    s.run("AuctionsFrameAuctions_ValidateAuction()").unwrap();
    assert!(
        s.eval::<bool>("return AuctionsCreateAuctionButton:IsEnabled()")
            .unwrap(),
        "an item, a 1g start price and no buyout is a valid auction"
    );
    // The deposit is the CLIENT's own arithmetic (auction.rs §7): 5% of the carried stack's vendor
    // value (2 x 25c = 50c) floors to 2c, and 24h is twelve two-hour units, so 24c. Two hours would
    // be 2c — the duration is what moves this, never the asking price.
    assert_eq!(
        s.eval::<i64>("return CalculateAuctionDeposit(1440)")
            .unwrap(),
        24
    );
    assert_eq!(
        s.eval::<i64>("return CalculateAuctionDeposit(120)")
            .unwrap(),
        2
    );
    let _ = s.take_auction_start();
    s.run("AuctionsCreateAuctionButton:Click()").unwrap();
    let start = s.take_auction_start().expect("Create queues one auction");
    assert_eq!(start.min_bid, 10000);
    assert_eq!(start.buyout, 0);
    assert_eq!(start.duration, 1440, "the long run time is still selected");

    assert!(s.errors().is_empty(), "clean create form: {:?}", s.errors());
}

/// Closing the window ends the session client-side and takes both confirmations with it.
#[test]
fn hiding_the_window_closes_the_session() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    seat_player(&mut s, 0);
    s.set_auction(Some(state(vec![])));
    s.fire_event("AUCTION_HOUSE_SHOW", vec![]);
    let _ = s.take_auction_close();

    s.run("HideUIPanel(AuctionFrame)").unwrap();
    assert!(!s.eval::<bool>("return AuctionFrame:IsShown()").unwrap());
    assert!(
        s.take_auction_close(),
        "OnHide queues CloseAuctionHouse (the session is client-side only)"
    );
    assert!(s.errors().is_empty());

    // And the server-side close hides it again from the other direction.
    s.fire_event("AUCTION_HOUSE_SHOW", vec![]);
    assert!(s.eval::<bool>("return AuctionFrame:IsShown()").unwrap());
    s.fire_event("AUCTION_HOUSE_CLOSED", vec![]);
    assert!(!s.eval::<bool>("return AuctionFrame:IsShown()").unwrap());
    assert!(s.errors().is_empty(), "clean close: {:?}", s.errors());
}

/// Paging: with more matches than the server's 50-row page, the turners appear only when the bar is
/// at the very bottom, and the list is fed one extra row so the bar can travel there. A 50-row
/// batch out of 120 fills all 8 slots, so the turners stay hidden until the list is scrolled down.
#[test]
fn paging_shows_the_turners_only_at_the_end_of_the_list() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    seat_player(&mut s, 0);
    let mut rows = Vec::new();
    for i in 0..50 {
        rows.push(row(&format!("Item {i}"), 100, 0, 0, "Seller"));
    }
    let mut st = state(rows);
    st.lists[LIST].total = 120;
    s.set_auction(Some(st));
    s.fire_event("AUCTION_HOUSE_SHOW", vec![]);
    s.fire_event("AUCTION_ITEM_LIST_UPDATE", vec![]);

    assert!(
        !s.eval::<bool>("return BrowseNextPageButton:IsShown()")
            .unwrap(),
        "at the top of a full page the turners stay out of the way"
    );

    // Scroll to the very bottom. The list was fed 51 rows for 8 slots, so the last offset is 43 —
    // which leaves row slot 8 empty and reveals the turners.
    s.run("FauxScrollFrame_SetOffset(BrowseScrollFrame, 43)")
        .unwrap();
    s.run("AuctionFrameBrowse_Update()").unwrap();
    assert!(
        !s.eval::<bool>("return BrowseButton8:IsShown()").unwrap(),
        "the extra row slot is empty at the bottom — that is what reveals the turners"
    );
    assert!(
        s.eval::<bool>("return BrowseNextPageButton:IsShown()")
            .unwrap(),
        "120 matches over a 50-row page, scrolled to the end: the turners appear"
    );
    assert!(s
        .eval::<bool>("return BrowseSearchCountText:IsShown()")
        .unwrap());

    // Turning the page re-searches at the next offset rather than scrolling.
    s.set_auction_can_query(true);
    let _ = s.take_auction_query();
    s.run("BrowseNextPageButton:Click()").unwrap();
    let query = s.take_auction_query().expect("the turner re-queries");
    assert_eq!(query.page, 1, "the next page, 0-based");

    assert!(s.errors().is_empty(), "clean paging: {:?}", s.errors());
}

/// The row and sell-slot hovers go through the reference's own tooltip verbs (decision 1511).
///
/// Both used `SetHyperlink` while `GameTooltip:SetAuctionItem` / `SetAuctionSellItem` had no
/// bindings. The rendered tooltip was the same either way — what changes is that an addon hooking
/// either verb now sees the call it expects, which is the whole point of shipping the reference's
/// API surface rather than an equivalent one.
#[test]
fn a_row_hover_goes_through_the_reference_tooltip_verb() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_auction(Some(state(vec![row("Copper Bar", 100, 500, 0, "Someone")])));
    s.fire_event("AUCTION_HOUSE_SHOW", vec![]);
    s.fire_event("AUCTION_ITEM_LIST_UPDATE", vec![]);

    // The verbs exist as tooltip methods at all — the thing that was missing.
    assert!(
        s.eval::<bool>("return type(GameTooltip.SetAuctionItem) == 'function'")
            .unwrap(),
        "SetAuctionItem is bound"
    );
    assert!(
        s.eval::<bool>("return type(GameTooltip.SetAuctionSellItem) == 'function'")
            .unwrap(),
        "SetAuctionSellItem is bound"
    );

    // And the row hover drives one without erroring. The tooltip body itself depends on the item
    // template store, which a bare harness has not been fed — the assertion here is the call path,
    // not the rendered text.
    // `this` is what the event dispatcher binds before it calls a handler; a direct call from a
    // chunk has to bind it the same way or `SetOwner(this, ...)` gets a nil frame.
    s.run("this = BrowseButton1Item AuctionFrameItem_OnEnter('list', 1)")
        .unwrap();
    assert!(
        s.take_errors().is_empty(),
        "the row hover raised no script error"
    );

    // An empty sell slot is a no-op rather than a throw, which is how the window's own OnEnter
    // gate calls it.
    s.run("GameTooltip:SetAuctionSellItem()").unwrap();
    assert!(s.take_errors().is_empty());
}
