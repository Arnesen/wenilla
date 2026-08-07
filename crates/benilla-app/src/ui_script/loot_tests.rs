use benilla_ui::script::{
    DressUpIntent, ExtractedQuad, LootRow, LootState, MerchantItem, MerchantState, QuadContent,
    SoundRequest, UiScript,
};

/// Load one shipped `assets/ui/<file>` into `s` (the panel tests' loader, duplicated here so this
/// file is self-contained), panicking on any loader error and returning the frame count.
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

/// A bare frame's own rect via its `QuadContent::Frame` entry (every frame emits one at its resolved
/// rect). The re-skinned loot window has no solid-colour fill (the UI-LootPanel slab is opaque), so
/// it's found by its own frame quad rather than a background texture.
fn frame_rect(quads: &[ExtractedQuad], w: f32, h: f32) -> benilla_ui::layout::Rect {
    quads
        .iter()
        .find_map(|q| match &q.content {
            QuadContent::Frame => q
                .rect
                .filter(|r| (r.width() - w).abs() < 0.5 && (r.height() - h).abs() < 0.5),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no bare-frame quad sized {w}x{h}"))
}

/// The centre of the first texture quad whose path contains `needle` (a row's icon), for clicking it.
fn icon_center(quads: &[ExtractedQuad], needle: &str) -> (f32, f32) {
    let r = quads
        .iter()
        .find(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p.contains(needle))
        })
        .and_then(|q| q.rect)
        .unwrap_or_else(|| panic!("no icon quad for {needle}"));
    ((r.left + r.right) * 0.5, (r.bottom + r.top) * 0.5)
}

/// The colour of the first text quad whose text equals `t`.
fn text_color(quads: &[ExtractedQuad], t: &str) -> Option<[f32; 4]> {
    quads.iter().find_map(|q| match &q.content {
        QuadContent::Text {
            text: Some(x),
            color,
            ..
        } if x == t => Some(*color),
        _ => None,
    })?
}

fn coin_and_two_items() -> LootState {
    LootState {
        rows: vec![
            LootRow {
                item_id: 0,
                name: Some("1g 23s 45c".into()),
                texture: Some("Interface\\Icons\\INV_Misc_Coin_01".into()),
                quantity: 1,
                quality: Some(1),
                is_coin: true,
                link: None,
            },
            LootRow {
                item_id: 0,
                name: Some("Wool Cloth".into()),
                texture: Some("Interface\\Icons\\INV_Fabric_Wool_01".into()),
                quantity: 3,
                quality: Some(2), // uncommon → green text
                is_coin: false,
                link: None,
            },
            LootRow {
                item_id: 0,
                name: Some("Linen Cloth".into()),
                texture: Some("Interface\\Icons\\INV_Fabric_Linen_01".into()),
                quantity: 1,
                quality: Some(1), // common → white text
                is_coin: false,
                link: None,
            },
        ],
    }
}

/// The whole loot chain minus Bevy (decision 0084): LOOT_OPENED lands the window at the left slot,
/// the coin row (first) + two item rows render with quality-coloured text + a stack count, a coin
/// click and an item click each queue the right 1-based row pick, a LOOT_UPDATE with a row removed
/// repaints to two rows, and the close button releases through OnHide → CloseLoot.
#[test]
fn shipped_loot_frame_drives_end_to_end() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml"); // ITEM_QUALITY_COLORS (LootFrame's palette), app load order
    load_xml(&s, "UiPanels.xml");
    // window + 4 rows + up + down + close.
    assert_eq!(
        load_xml(&s, "LootFrame.xml"),
        8,
        "window + 4 rows + up + down + close"
    );

    // Hidden by default: no coin icon on screen, left slot empty.
    s.resolve();
    let has_icon = |quads: &[ExtractedQuad], needle: &str| {
        quads.iter().any(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p.contains(needle))
        })
    };
    assert!(
        !has_icon(&s.extract(), "INV_Misc_Coin_01"),
        "loot window starts hidden"
    );
    assert!(s.eval::<bool>("return GetLeftFrame() == nil").unwrap());

    // The app's feed: a coin pile + two items.
    s.set_loot(Some(coin_and_two_items()));
    s.fire_event("LOOT_OPENED", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // Shown at the left slot; three rows visible (coin + 2 items), row 4 hidden.
    assert!(s
        .eval::<bool>("return BenillaLootFrame:IsVisible()")
        .unwrap());
    let vis: (bool, bool, bool, bool) = s
        .eval(
            "return BenillaLootButton1:IsVisible(), BenillaLootButton2:IsVisible(),\n\
                    BenillaLootButton3:IsVisible(), BenillaLootButton4:IsVisible()",
        )
        .unwrap();
    assert_eq!(vis, (true, true, true, false), "coin + 2 items, 4th hidden");
    // Only 3 items ⇒ no pager.
    assert!(!s
        .eval::<bool>("return BenillaLootDownButton:IsVisible()")
        .unwrap());

    s.resolve();
    let quads = s.extract();

    // The slot anchor applied: top-left at (0, 664) — screen height 768 minus the left slot's 104.
    let win = frame_rect(&quads, 256.0, 256.0);
    assert_eq!(
        (win.left, win.top),
        (0.0, 664.0),
        "loot window landed at the left slot (TOPLEFT UIParent, 0, -104)"
    );

    // The single UI-LootPanel slab IS the window art (ref LootFrame.xml l.88): no size/anchors → it
    // fills the 256×256 window, sampling its whole texture (no TexCoords).
    let panel = quads
        .iter()
        .find(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                    if p.contains("UI-LootPanel"))
        })
        .and_then(|q| q.rect)
        .expect("no UI-LootPanel art quad");
    assert!(
        (panel.width() - 256.0).abs() < 0.5 && (panel.height() - 256.0).abs() < 0.5,
        "loot panel fills the 256×256 window, got {}×{}",
        panel.width(),
        panel.height()
    );
    assert_eq!(
        (panel.left, panel.top),
        (win.left, win.top),
        "loot panel pinned to the window's TOPLEFT"
    );

    // The coin row is FIRST: its money text renders, and the uncommon item's text is green while the
    // common item's is white (ITEM_QUALITY_COLORS).
    let has_text = |t: &str| {
        quads
            .iter()
            .any(|q| matches!(&q.content, QuadContent::Text { text: Some(x), .. } if x == t))
    };
    assert!(has_text("1g 23s 45c"), "coin row shows the money amount");
    assert!(has_text("Wool Cloth"), "item row shows the name");
    let green = text_color(&quads, "Wool Cloth").expect("Wool Cloth colour");
    assert!(
        (green[0] - 0.12).abs() < 0.02 && (green[1] - 1.0).abs() < 0.02 && green[2].abs() < 0.02,
        "uncommon item text is green, got {green:?}"
    );
    let white = text_color(&quads, "Linen Cloth").expect("Linen Cloth colour");
    assert!(
        (white[0] - 1.0).abs() < 0.02
            && (white[1] - 1.0).abs() < 0.02
            && (white[2] - 1.0).abs() < 0.02,
        "common item text is white, got {white:?}"
    );
    // The stack count "3" overlays the Wool Cloth row.
    assert!(has_text("3"), "the x3 stack count renders");

    // Click the coin row's icon → LootSlot(1); click the Wool Cloth row's icon → LootSlot(2).
    let (cx, cy) = icon_center(&quads, "INV_Misc_Coin_01");
    s.mouse_button(cx, cy, "LeftButton", true);
    s.mouse_button(cx, cy, "LeftButton", false);
    let (ix, iy) = icon_center(&quads, "INV_Fabric_Wool_01");
    s.mouse_button(ix, iy, "LeftButton", true);
    s.mouse_button(ix, iy, "LeftButton", false);
    assert_eq!(
        s.take_loot_picks(),
        vec![1, 2],
        "coin row is pick 1, the first item row is pick 2"
    );
    assert!(!s.take_loot_close());

    // LOOT_UPDATE with the coin row gone (looted) repaints to two rows, keeping the window open.
    s.set_loot(Some(LootState {
        rows: coin_and_two_items().rows[1..].to_vec(),
    }));
    s.fire_event("LOOT_UPDATE", vec![]);
    let vis2: (bool, bool, bool) = s
        .eval(
            "return BenillaLootButton1:IsVisible(), BenillaLootButton2:IsVisible(),\n\
                    BenillaLootButton3:IsVisible()",
        )
        .unwrap();
    assert_eq!(vis2, (true, true, false), "the coin row cleared → two rows");

    // The close button hides the window → OnHide → CloseLoot() queues the release intent.
    s.run("BenillaLootCloseButton_OnClick()").unwrap();
    assert!(
        s.take_loot_close(),
        "closing the window releases the loot (OnHide → CloseLoot)"
    );
    assert!(!s
        .eval::<bool>("return BenillaLootFrame:IsVisible()")
        .unwrap());
    assert!(
        s.eval::<bool>("return GetLeftFrame() == nil").unwrap(),
        "HideUIPanel vacated the left slot"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The LOOTWINDOWOPENEMPTY branch (LootFrame.lua LootFrame_OnShow, l.135-136): an empty loot roll
/// queues the empty-open kit on show; a normal (non-empty) loot open queues NO sound (that kit is
/// C-side, server-driven). DR 0086 masks the lootable flag for empty rolls so live empty windows are
/// rare — but the branch must exist, and this proves it fires the right kit and only then.
#[test]
fn loot_empty_roll_plays_the_empty_open_kit() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml"); // ITEM_QUALITY_COLORS (LootFrame's palette), app load order
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "LootFrame.xml");

    // A normal, non-empty loot open queues no sound (the normal open kit is C-side).
    s.set_loot(Some(coin_and_two_items()));
    s.fire_event("LOOT_OPENED", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        s.take_sounds().is_empty(),
        "a non-empty loot open is silent (its open kit is C-side)"
    );

    // Close it (also silent — the loot-close kit is C-side).
    s.fire_event("LOOT_CLOSED", vec![]);
    assert!(s.take_sounds().is_empty(), "loot close is silent (C-side)");

    // Re-open with an EMPTY roll: OnShow's numItems==0 fork queues exactly LOOTWINDOWOPENEMPTY.
    s.set_loot(Some(LootState { rows: vec![] }));
    s.fire_event("LOOT_OPENED", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert_eq!(
        s.take_sounds(),
        vec![SoundRequest::KitName("LOOTWINDOWOPENEMPTY".into())],
        "an empty loot roll plays the empty-open kit"
    );
}

/// Paging (LootFrame.lua l.70-73,105-118): 5 items ⇒ 3 rows + a Down pager on page 1, 2 rows + an Up
/// pager on page 2 — the real shipped XML driving the render, not just the arithmetic.
#[test]
fn shipped_loot_frame_pages_five_items() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml"); // ITEM_QUALITY_COLORS (LootFrame's palette), app load order
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "LootFrame.xml");

    let rows: Vec<LootRow> = (0..5)
        .map(|i| LootRow {
            item_id: 0,
            name: Some(format!("Item {i}")),
            texture: Some(format!("Interface\\Icons\\Item_{i}")),
            quantity: 1,
            quality: Some(1),
            is_coin: false,
            link: None,
        })
        .collect();
    s.set_loot(Some(LootState { rows }));
    s.fire_event("LOOT_OPENED", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // Page 1: 3 rows (the pager spends the 4th slot), Down shown, Up hidden.
    let page1: (bool, bool, bool, bool) = s
        .eval(
            "return BenillaLootButton1:IsVisible(), BenillaLootButton2:IsVisible(),\n\
                    BenillaLootButton3:IsVisible(), BenillaLootButton4:IsVisible()",
        )
        .unwrap();
    assert_eq!(page1, (true, true, true, false), "page 1 shows 3 rows");
    let pager1: (bool, bool) = s
        .eval("return BenillaLootUpButton:IsVisible(), BenillaLootDownButton:IsVisible()")
        .unwrap();
    assert_eq!(pager1, (false, true), "page 1: Up hidden, Down shown");

    // Page down → page 2: 2 rows, Up shown, Down hidden.
    s.run("BenillaLootFrame_PageDown()").unwrap();
    let page2: (bool, bool, bool) = s
        .eval(
            "return BenillaLootButton1:IsVisible(), BenillaLootButton2:IsVisible(),\n\
                    BenillaLootButton3:IsVisible()",
        )
        .unwrap();
    assert_eq!(page2, (true, true, false), "page 2 shows the last 2 rows");
    let pager2: (bool, bool) = s
        .eval("return BenillaLootUpButton:IsVisible(), BenillaLootDownButton:IsVisible()")
        .unwrap();
    assert_eq!(pager2, (true, false), "page 2: Up shown, Down hidden");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Pin §4/§5: loot (pushable=7) open, then merchant (pushable=0) open → loot is pushed to the CENTER
/// slot rather than replaced, and merchant takes the left slot loot vacated — the real windows, not
/// the synthetic stand-in the merchant panel test uses.
#[test]
fn shipped_loot_pushed_to_center_by_merchant() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml"); // ITEM_QUALITY_COLORS (LootFrame's palette), app load order
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "LootFrame.xml");
    load_xml(&s, "GameTooltip.xml"); // app load order: tooltip before merchant
    load_xml(&s, "MerchantFrame.xml");

    // Loot opens onto the empty left slot.
    s.set_loot(Some(coin_and_two_items()));
    s.fire_event("LOOT_OPENED", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        s.eval::<bool>("return GetLeftFrame():GetName() == \"BenillaLootFrame\"")
            .unwrap(),
        "loot took the empty left slot"
    );

    // Merchant (pushable=0) opens: loot's pushable=7 outranks it, so loot is promoted to center and
    // merchant takes the left spot.
    s.set_merchant(Some(MerchantState {
        items: vec![MerchantItem {
            name: Some("Refreshing Spring Water".into()),
            texture: Some("Interface\\Icons\\INV_Drink_18".into()),
            price: 25,
            quantity: 1,
            num_available: -1,
            item_id: 159,
            stats: None,
            link: None,
        }],
        ..Default::default()
    }));
    s.fire_event("MERCHANT_SHOW", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    assert!(
        s.eval::<bool>("return GetCenterFrame():GetName() == \"BenillaLootFrame\"")
            .unwrap(),
        "loot was pushed to center, not replaced"
    );
    assert!(
        s.eval::<bool>("return GetLeftFrame():GetName() == \"BenillaMerchantFrame\"")
            .unwrap(),
        "merchant took the left slot loot vacated"
    );
    // Both windows still visible, at their slots.
    s.resolve();
    let quads = s.extract();
    let loot_center = frame_rect(&quads, 256.0, 256.0);
    assert_eq!(
        (loot_center.left, loot_center.top),
        (384.0, 664.0),
        "loot moved to the center slot (TOPLEFT UIParent, 384, -104)"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **The party frame drew through the loot window** — the director's report, and the reason
/// decision 0597 exists.
///
/// Both own the top-left corner: `ShowUIPanel` plants the loot window at `UIParent TOPLEFT
/// (0,-104)` and `PartyMemberFrame1` sits just above it. Benilla had transcribed the party template
/// without the reference's `frameStrata="LOW"` (`PartyFrameTemplates.xml` l.194), so it inherited
/// the default MEDIUM — the panels' own stratum. Within one stratum the draw key is
/// `level`-then-insertion, and the party frame's art rides a nested `$parentTextureFrame` CHILD
/// (level 1) while the loot window's `UI-LootPanel` slab is a REGION of the window itself (level
/// 0). Level outranks insertion, so being shown later could never have lifted the window above it.
///
/// Asserted on the packed draw key itself, and it pins both halves of the fix: that the template's
/// strata reaches an `inherits=` instance at all, and that it cascades down to the nested child
/// frames (`SetFrameStrata 0x76a470` is a whole-subtree cascade).
#[test]
fn the_loot_window_draws_over_the_party_frames() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    // PartyFrame's per-member dropdown OnLoad walks the whole popup kit (app manifest order).
    load_xml(&s, "UIDropDownMenu.xml");
    load_xml(&s, "UnitPopup.xml");
    load_xml(&s, "PartyFrame.xml");
    load_xml(&s, "LootFrame.xml");

    // The party frame up FIRST, the window second — the order that cannot be what saves it.
    s.eval::<()>("PartyMemberFrame1:Show()").unwrap();
    s.set_loot(Some(coin_and_two_items()));
    s.fire_event("LOOT_OPENED", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    s.resolve();
    let quads = s.extract();
    let z_of = |needle: &str| {
        quads
            .iter()
            .find(|q| {
                matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                    if p.contains(needle))
            })
            .unwrap_or_else(|| panic!("no quad for {needle}"))
            .z
    };
    // The window's own background slab (a level-0 region) against the party art (a level-1 child
    // frame's region) — the exact pair that inverted.
    let loot_panel = z_of("UI-LootPanel");
    let party_art = z_of("UI-PartyFrame");
    assert!(
        loot_panel > party_art,
        "the loot window's background must draw OVER the party frame art: \
         panel {loot_panel:#x} vs party {party_art:#x}"
    );
    // And the whole party frame is below the whole window, not just that one pair.
    const STRATUM_SHIFT: u32 = 60;
    assert!(
        (party_art >> STRATUM_SHIFT) < (loot_panel >> STRATUM_SHIFT),
        "they must differ by STRATUM, not by luck within one"
    );
}

/// The row click's modifier fork (ref `LootFrameItem_OnClick`, LootFrame.lua l.147-154): CTRL
/// previews the row's item in the dressing room (decision 1060), SHIFT posts its link into an open
/// chat edit box (decision 1059) — and **neither loots**.
///
/// That last clause is the whole point of the test, and it is ours to get right rather than the
/// reference's: in the real client the loot itself is the C `LootButton` widget's click behaviour
/// (l.94's `button:SetSlot(slot)`), so its Lua arms fall through harmlessly; ours owns the
/// `LootSlot` call, so the arms had to grow a `return` the reference does not have. The unmodified
/// click still loots — the regression that would otherwise ship silently.
#[test]
fn ctrl_and_shift_on_a_loot_row_preview_and_post_without_looting() {
    const WOOL_LINK: &str = "|cffffffff|Hitem:2589:0:0:0|h[Wool Cloth]|h|r";
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for file in [
        "Fonts.xml",
        "UiPanels.xml",
        "UIParent.xml", // BenillaChatEdit_InsertLink, the shared shift-insert helper
        "LootFrame.xml",
        "DressUpFrame.xml",
        "ChatFrame.xml",
    ] {
        load_xml(&s, file);
    }

    // A coin row + one resolved item: the item's link is fed exactly as `ui_loot.rs` builds it.
    s.set_loot(Some(LootState {
        rows: vec![
            LootRow {
                item_id: 0,
                name: Some("1g 23s 45c".into()),
                texture: Some("Interface\\Icons\\INV_Misc_Coin_01".into()),
                quantity: 1,
                quality: Some(1),
                is_coin: true,
                link: None, // synthesized row, no item behind it
            },
            LootRow {
                item_id: 2589,
                name: Some("Wool Cloth".into()),
                texture: Some("Interface\\Icons\\INV_Fabric_Wool_01".into()),
                quantity: 3,
                quality: Some(1),
                is_coin: false,
                link: Some(WOOL_LINK.into()),
            },
        ],
    }));
    s.fire_event("LOOT_OPENED", vec![]);
    s.resolve();
    let quads = s.extract();
    let (x, y) = icon_center(&quads, "INV_Fabric_Wool_01");
    let (coin_x, coin_y) = icon_center(&quads, "INV_Misc_Coin_01");

    // The control first, while nothing else has been opened: a plain click still loots row 2.
    s.mouse_button(x, y, "LeftButton", true);
    s.mouse_button(x, y, "LeftButton", false);
    assert_eq!(
        s.take_loot_picks(),
        vec![2],
        "an unmodified click still loots the row"
    );

    // SHIFT with the chat edit box open → the link, and no loot.
    assert!(s.focus_editbox("ChatFrameEditBox"));
    s.set_modifiers(true, false, false);
    s.mouse_button(x, y, "LeftButton", true);
    s.mouse_button(x, y, "LeftButton", false);
    s.set_modifiers(false, false, false);
    assert_eq!(
        s.eval::<String>("return ChatFrameEditBox:GetText()")
            .unwrap(),
        WOOL_LINK,
        "the row's full escaped link landed in the chat box"
    );
    assert!(
        s.take_loot_picks().is_empty(),
        "a shift-click must not also loot the row"
    );

    // The COIN row has no link at all, and a modified click on it must be inert rather than an
    // error: our `EditBox:Insert` binding is typed `String` and raises on a nil, so the handler
    // guards it (the reference's C Insert tolerates the nil its own callers hand it).
    s.set_modifiers(true, false, false);
    s.mouse_button(coin_x, coin_y, "LeftButton", true);
    s.mouse_button(coin_x, coin_y, "LeftButton", false);
    s.set_modifiers(false, false, false);
    assert!(
        s.errors().is_empty(),
        "shift-clicking the linkless coin row must not raise: {:?}",
        s.errors()
    );
    assert_eq!(
        s.eval::<String>("return ChatFrameEditBox:GetText()")
            .unwrap(),
        WOOL_LINK,
        "and it must not disturb what is already typed"
    );
    assert!(
        s.take_loot_picks().is_empty(),
        "a shift-click on the coin row must not loot the money either"
    );

    // ALT → nothing at all, and in particular NOT a loot (decision 1067). No FrameXML file binds
    // alt on a loot row; the suppression is the C `CLootButton::OnClick`'s own third gate
    // (`0x41f8f0(2)` @ `0x4c1841`, VERIFIED at the bytes), which our Lua-side take has to
    // reproduce. This is the arm nothing else in the suite would catch: before 1067, alt looted.
    s.set_modifiers(false, false, true);
    s.mouse_button(x, y, "LeftButton", true);
    s.mouse_button(x, y, "LeftButton", false);
    s.set_modifiers(false, false, false);
    assert!(
        s.take_loot_picks().is_empty(),
        "an alt-click must not loot the row (the C gate's third modifier)"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // CTRL → the dressing room wearing it, still no loot. Last, because opening the room takes the
    // left UIPanel slot and moves the loot window (pushable 2 vs 7).
    s.set_modifiers(false, true, false);
    s.mouse_button(x, y, "LeftButton", true);
    s.mouse_button(x, y, "LeftButton", false);
    s.set_modifiers(false, false, false);
    assert_eq!(
        s.take_dressup_intents(),
        vec![DressUpIntent::Dress, DressUpIntent::TryOn(2589)],
        "re-dress first, then try the looted item on"
    );
    assert!(
        s.take_loot_picks().is_empty(),
        "a ctrl-click must not also loot the row"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
