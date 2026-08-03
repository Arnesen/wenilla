use benilla_ui::script::{ContainerSlot, ContainerState, LootRow, LootState, UiScript};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error (the panel/loot
/// tests' loader, duplicated so this file is self-contained).
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

/// A one-item backpack (slot 1 holds a resolved item, so it is pickable onto the cursor).
fn one_item_backpack() -> ContainerState {
    let mut slots = std::collections::HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            bar_placeable: true,
            durability: None,
            texture: Some("Interface\\Icons\\INV_Misc_Food_16".into()),
            count: 1,
            quality: Some(1),
            item_id: 117,
            link: Some("|cffffffff|Hitem:117|h[Tough Jerky]|h|r".into()),
            locked: false,
            equip_slots: Vec::new(),
            cooldown: None,
            readable: false,
            creator: None,
            flags: 0,
            enchants: Vec::new(),
        },
    );
    ContainerState {
        name: Some("Backpack".into()),
        num_slots: 16,
        slots,
    }
}

/// Load every window and drive the whole ESC-close path (UIParent.lua ToggleGameMenu → l.1491
/// CloseAllWindows): with the bag open, the loot window on
/// the left slot, and an item on the cursor, ESC closes the bag AND the panel slot, releases the
/// loot (OnHide → CloseLoot), and drops the held cursor. Also asserts the host-glue precedence: no
/// EditBox focused ⇒ `key_input("ESCAPE")` does not consume, so the app runs the binding.
#[test]
fn escape_closes_bag_and_panel_releases_loot_and_clears_cursor() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml"); // BenillaMoney_Set, used by the bag's Update
    load_xml(&s, "LootFrame.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "BagFrame.xml");
    s.set_money(0);
    s.set_container(0, Some(one_item_backpack()));

    // Open the bag and the loot window; drain the open kits (not under test here).
    s.run("BenillaBagToggle_OnClick()").unwrap();
    s.set_loot(Some(LootState {
        rows: vec![LootRow {
            item_id: 0,
            name: Some("Wool Cloth".into()),
            texture: Some("Interface\\Icons\\INV_Fabric_Wool_01".into()),
            quantity: 1,
            quality: Some(1),
            is_coin: false,
        }],
    }));
    s.fire_event("LOOT_OPENED", vec![]);
    let _ = s.take_sounds();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // Pick up slot 1 onto the cursor.
    s.run("C_Container.PickupContainerItem(0, 1)").unwrap();
    assert!(s.cursor_item().is_some(), "cursor holds the picked item");
    assert!(
        s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap(),
        "bag is open before ESC"
    );
    assert!(
        s.eval::<bool>("return GetLeftFrame():GetName() == \"BenillaLootFrame\"")
            .unwrap(),
        "loot holds the left panel slot before ESC"
    );

    // Precedence: no EditBox focused ⇒ ESCAPE is NOT consumed, so the host runs the binding.
    assert!(
        !s.key_input("ESCAPE"),
        "no EditBox focused ⇒ ESC is not consumed by the box layer"
    );

    // The escape binding (what the host runs on the unconsumed ESC).
    s.run("ToggleGameMenu()").unwrap();

    assert!(
        !s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap(),
        "ESC closed the bag"
    );
    assert!(
        s.eval::<bool>("return GetLeftFrame() == nil").unwrap(),
        "ESC vacated the panel slot"
    );
    assert!(
        !s.eval::<bool>("return BenillaLootFrame:IsVisible()")
            .unwrap(),
        "ESC hid the loot window"
    );
    assert!(
        s.take_loot_close(),
        "closing the loot fired the release (OnHide → CloseLoot)"
    );
    assert!(s.cursor_item().is_none(), "ESC dropped the held cursor");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The other precedence side: a focused EditBox consumes ESCAPE (`key_input` returns true), so the
/// host's `!consumed` gate never runs the escape binding while typing — the bag stays open. Drives
/// the shipped chat box (the app's ENTER-to-open path is `focus_editbox`); its own OnEscapePressed
/// clears the focus (chat_tests covers the box's submit/close contract itself).
#[test]
fn escape_is_consumed_by_a_focused_editbox_and_leaves_windows_open() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "BagFrame.xml");
    load_xml(&s, "ChatFrame.xml");
    s.set_money(0);

    s.run("BenillaBagToggle_OnClick()").unwrap();
    assert!(
        s.focus_editbox("ChatFrameEditBox"),
        "the chat edit box focuses"
    );
    assert!(s.has_keyboard_focus());

    // ESCAPE is consumed by the focused box — the host would NOT run the escape binding.
    assert!(s.key_input("ESCAPE"), "a focused EditBox consumes ESCAPE");
    // The box's OnEscapePressed cleared its own focus, but the bag window is untouched.
    assert!(!s.has_keyboard_focus(), "the box cleared its focus");
    assert!(
        s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap(),
        "the bag stays open (the escape binding never ran)"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// ESC closes an open stack-split spinner (decision 0216 §6/slice 2, StackSplit.xml): this engine
/// has no plain-frame keyboard capture (the real client's own `StackSplitFrame` OnKeyDown ESCAPE
/// arm can't be driven), so the hook rides `ToggleGameMenu`'s shared chain instead — checked
/// right after the confirm popup and before the world map, both similarly transient overlays.
#[test]
fn escape_closes_an_open_stack_split_frame() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "BagFrame.xml");
    load_xml(&s, "StackSplit.xml");
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
    s.run("BenillaBagToggle_OnClick()").unwrap();
    s.resolve();

    // Open the spinner the same way bag_tests.rs's split tests do: SHIFT + left-click on slot 1
    // (the reference fork, driven through the modifier mirror).
    s.set_modifiers(true, false, false);
    s.run(
        "BENILLA_TEST_BTN = nil\n\
         for i = 1, 16 do local b = getglobal(\"BenillaBagSlot\" .. i)\n\
           if b and b.slot == 1 then BENILLA_TEST_BTN = b end\n\
         end\n\
         BenillaBagSlot_OnClick(BENILLA_TEST_BTN, \"LeftButton\")",
    )
    .unwrap();
    s.set_modifiers(false, false, false);
    assert!(s
        .eval::<bool>("return BenillaStackSplitFrame:IsShown()")
        .unwrap());

    s.run("ToggleGameMenu()").unwrap();
    assert!(
        !s.eval::<bool>("return BenillaStackSplitFrame:IsShown()")
            .unwrap(),
        "ESC closed the split frame"
    );
    // ESC's unconditional cursor-clear (ToggleGameMenu's own first line) ran too — nothing was
    // held anyway (the split branch already released it), so this just confirms no error.
    assert!(s.cursor_item().is_none());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The ESC ladder end-to-end (the ref's `ToggleGameMenu` order, `UIParent.lua:1482-1496`, one
/// eater per press — the director's two-press report, decision 0449): mid-cast with a menu
/// open, a bag open and a target, the press closes the MENU first (`CloseMenus`, `l.1488` —
/// the cast survives), the next only cancels the cast (`SpellStopCasting`, `l.1489`), the next
/// only closes the windows (`CloseAllWindows`, `l.1491`), and only then — nothing left to
/// eat — does one drop the target (`ClearTarget`, `l.1492`). The 1/nil returns are load-bearing
/// at every rung (an unconditional true anywhere would wedge the ladder; the artifact's
/// `elseif` chain is the proof each real binding is conditional).
#[test]
fn escape_ladder_cast_then_windows_then_target_one_eater_per_press() {
    use benilla_ui::script::UnitState;

    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "BagFrame.xml");
    load_xml(&s, "GameTooltip.xml"); // TOOLTIP_DEFAULT_COLOR — the menu backdrop's OnLoad reads it
    load_xml(&s, "UIDropDownMenu.xml");
    s.set_money(0);
    s.set_container(0, Some(one_item_backpack()));
    s.run("BenillaBagToggle_OnClick()").unwrap();
    let _ = s.take_sounds();
    assert!(s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap());
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            ..Default::default()
        }),
    );

    // Press 0 — mid-cast with a dropdown open (the ref's CloseMenus rung, l.1488): the menu
    // closes and eats the press — the cast, the windows, and the target all survive.
    s.set_casting(true);
    s.run("DropDownList1:Show()").unwrap();
    s.run("ToggleGameMenu()").unwrap();
    assert!(
        !s.eval::<bool>("return DropDownList1:IsShown()").unwrap(),
        "ESC closed the open dropdown menu"
    );
    assert!(
        !s.take_spell_stop(),
        "the menu press must NOT reach SpellStopCasting — the cast runs on (ref order: \
         CloseMenus sits before the cast rung)"
    );
    assert!(
        s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap(),
        "the menu press must not close windows either"
    );
    assert!(!s.take_target_clear());

    // Press 1 — mid-cast (the app's per-frame IsCasting mirror): the cast dies, NOTHING else.
    s.set_casting(true);
    s.run("ToggleGameMenu()").unwrap();
    assert!(
        s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap(),
        "ESC mid-cast is eaten by SpellStopCasting — the bag stays open"
    );
    assert!(
        s.take_spell_stop(),
        "the stop request queued for the app's local cancel"
    );
    assert!(
        !s.take_target_clear(),
        "the same press must NOT also drop the target (the raw-key double-fire 0449 retires)"
    );

    // Press 2 — the cancel resolved (next frame's mirror push): the windows close, target stays.
    s.set_casting(false);
    s.run("ToggleGameMenu()").unwrap();
    assert!(
        !s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap(),
        "idle ESC falls through SpellStopCasting's nil to CloseAllWindows"
    );
    assert!(!s.take_spell_stop(), "no stray stop request when idle");
    assert!(
        !s.take_target_clear(),
        "a press CloseAllWindows ate must not reach ClearTarget"
    );

    // Press 3 — nothing left to eat: the target drops.
    s.run("ToggleGameMenu()").unwrap();
    assert!(
        s.take_target_clear(),
        "the bare press reaches ClearTarget — the ladder's last rung"
    );

    // Press 4 — no target either: the chain runs out. In a full UI this press opens the game menu
    // (decision 0674, `game_menu_tests`); this harness deliberately loads no GameMenuFrame.xml, so
    // what it pins is the rung BELOW it — ClearTarget answering nil rather than eating the press.
    s.set_unit("target", None);
    s.run("ToggleGameMenu()").unwrap();
    assert!(
        !s.take_target_clear(),
        "ClearTarget answers nil with no target — nothing queued"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The SpellStopTargeting rung (`UIParent.lua:1490`, decision 0792): ESC with the ground-target
/// cursor up cancels the targeting and ONLY the targeting — after the cast rung (the artifact's
/// order), before the window close. The 1/nil returns are load-bearing exactly like the cast
/// rung's: an idle press must fall straight through both.
#[test]
fn escape_ladder_targeting_rung_after_cast_before_windows() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "MerchantFrame.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "BagFrame.xml");
    s.set_money(0);
    s.set_container(0, Some(one_item_backpack()));
    s.run("BenillaBagToggle_OnClick()").unwrap();
    let _ = s.take_sounds();
    assert!(s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap());

    // SpellIsTargeting() mirrors the pushed state — the PetFrame-family readers' predicate.
    assert!(!s.eval::<bool>("return SpellIsTargeting()").unwrap_or(false));
    s.set_spell_targeting(true);
    assert!(s.eval::<bool>("return SpellIsTargeting()").unwrap());

    // Press 1 — targeting, no cast in flight: SpellStopTargeting eats the press; the bag stays.
    s.run("ToggleGameMenu()").unwrap();
    assert!(
        s.take_stop_targeting(),
        "the stop-targeting request queued for the app's cancel"
    );
    assert!(
        !s.take_spell_stop(),
        "the cast rung answered nil — nothing casting"
    );
    assert!(
        s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap(),
        "ESC while targeting must NOT also close windows"
    );

    // Press 2 — BOTH casting and targeting pushed (unreachable app state, but the chain's order
    // is the artifact's law): the cast rung sits first (l.1489 before l.1490) and eats alone.
    s.set_casting(true);
    s.run("ToggleGameMenu()").unwrap();
    assert!(s.take_spell_stop(), "the cast rung eats first");
    assert!(
        !s.take_stop_targeting(),
        "the same press must not also cancel the targeting"
    );

    // Press 3 — idle (the app cleared the mode and the cast resolved): both rungs answer nil
    // and the press falls through to CloseAllWindows.
    s.set_casting(false);
    s.set_spell_targeting(false);
    s.run("ToggleGameMenu()").unwrap();
    assert!(!s.take_stop_targeting(), "no stray trigger when idle");
    assert!(
        !s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap(),
        "the idle press falls through both spell rungs to CloseAllWindows"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
