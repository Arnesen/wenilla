//! The dressing room + the chat-link insert, driven through the real click paths (decisions
//! 1059/1060).
//!
//! What these pin is the **fork**, at the sites a player actually clicks: CTRL opens the room
//! wearing the item, SHIFT posts its link into an open chat edit box — and, at a bag slot, SHIFT
//! with chat *closed* still opens the stack splitter it always did (the reference's own `else`,
//! and the regression these two features could most easily cause).

use benilla_ui::script::{
    ContainerSlot, ContainerState, DressUpIntent, InvSlotView, InventorySlots, SoundRequest,
    UiScript,
};

/// The jerky stack every bag fixture here uses — a real 1.12 link, quality white.
const JERKY_LINK: &str = "|cffffffff|Hitem:117|h[Tough Jerky]|h|r";

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

/// The shipped files the dressing room's click sites need, in manifest order.
fn load_room(s: &UiScript) {
    for file in [
        "Fonts.xml",
        "MoneyFrame.xml",
        "UiPanels.xml",
        "UIParent.xml",
        "GameTooltip.xml",
        "UIDropDownMenu.xml",
        "UnitPopup.xml",
        "ItemRef.xml",
        "Cooldown.xml",
        "MerchantFrame.xml",
        "BagFrame.xml",
        "StackSplit.xml",
        "DressUpFrame.xml",
        "ChatFrame.xml",
    ] {
        load_xml(s, file);
    }
}

/// A backpack holding a 5-stack of Tough Jerky in slot 1, opened; returns that button's centre.
fn backpack_with_jerky(s: &mut UiScript) -> (f32, f32) {
    s.set_money(0);
    let mut slots = std::collections::HashMap::new();
    slots.insert(
        1,
        ContainerSlot {
            bar_placeable: true,
            texture: Some("Interface\\Icons\\INV_Misc_Food_16".into()),
            count: 5,
            quality: Some(1),
            item_id: 117,
            link: Some(JERKY_LINK.into()),
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

/// CTRL + left-click on a bag item opens the dressing room and puts that item on — the ref's
/// `DressUpItemLink(GetContainerItemLink(...))` (ContainerFrame.lua:565-566). The two intents are
/// ordered: `Dress` (the window was closed, so it re-dresses in the player's own gear first) then
/// `TryOn` — reversed, the room would show the player's own gear and not the clicked item.
#[test]
fn ctrl_click_on_a_bag_item_opens_the_room_wearing_it() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_room(&s);
    let (x, y) = backpack_with_jerky(&mut s);
    assert!(!s.eval::<bool>("return DressUpFrame:IsVisible()").unwrap());

    s.set_modifiers(false, true, false);
    s.mouse_button(x, y, "LeftButton", true);
    s.mouse_button(x, y, "LeftButton", false);
    s.set_modifiers(false, false, false);

    assert!(
        s.eval::<bool>("return DressUpFrame:IsVisible()").unwrap(),
        "ctrl-click opened the dressing room"
    );
    assert_eq!(
        s.take_dressup_intents(),
        vec![DressUpIntent::Dress, DressUpIntent::TryOn(117)],
        "re-dress first, then try the clicked item on"
    );
    assert!(
        s.cursor_item().is_none(),
        "the ctrl fork never picks the item up"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// SHIFT + left-click posts the item's link into the chat edit box **when it is open**, and still
/// opens the stack splitter when it is not — the reference's own if/else (ContainerFrame.lua:
/// 567-577). Both halves in one test because it is the *fork* that matters: the split was the
/// behaviour that already shipped, and the insert must not have eaten it.
#[test]
fn shift_click_posts_the_link_with_chat_open_and_splits_with_it_closed() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_room(&s);
    let (x, y) = backpack_with_jerky(&mut s);

    // Chat closed → the splitter, exactly as before.
    s.set_modifiers(true, false, false);
    s.mouse_button(x, y, "LeftButton", true);
    s.mouse_button(x, y, "LeftButton", false);
    s.set_modifiers(false, false, false);
    assert!(
        s.eval::<bool>("return StackSplitFrame:IsShown()").unwrap(),
        "with chat closed, shift-click still opens the stack splitter"
    );
    s.run("StackSplitFrame:Hide()").unwrap();

    // Chat open → the link, and no splitter.
    assert!(s.focus_editbox("ChatFrameEditBox"));
    s.set_modifiers(true, false, false);
    s.mouse_button(x, y, "LeftButton", true);
    s.mouse_button(x, y, "LeftButton", false);
    s.set_modifiers(false, false, false);
    assert_eq!(
        s.eval::<String>("return ChatFrameEditBox:GetText()")
            .unwrap(),
        JERKY_LINK,
        "the item's full escaped link landed in the chat box"
    );
    assert!(
        !s.eval::<bool>("return StackSplitFrame:IsShown()").unwrap(),
        "with chat open the splitter must NOT open"
    );
    assert!(
        s.cursor_item().is_none(),
        "neither shift fork picks the stack up"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// A link already IN chat: ctrl-clicking it previews that item (ref ItemRef.lua:49-50), shift
/// -clicking it re-inserts it (the branch that already shipped). The router is `SetItemRef`, which
/// is what the message frame's `OnHyperlinkClick` calls with the full markup.
#[test]
fn ctrl_clicking_a_chat_link_previews_it() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_room(&s);

    s.set_modifiers(false, true, false);
    s.run(&format!(
        "SetItemRef(\"item:117\", \"{JERKY_LINK}\", \"LeftButton\")"
    ))
    .unwrap();
    s.set_modifiers(false, false, false);

    assert_eq!(
        s.take_dressup_intents(),
        vec![DressUpIntent::Dress, DressUpIntent::TryOn(117)],
        "a chat link's id reached TryOn through DressUpItemLink's own gsub"
    );
    assert!(
        s.eval::<bool>("return DressUpFrame:IsVisible()").unwrap(),
        "and the room opened"
    );
    // The plain click still shows the link tooltip rather than the room (no modifier).
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The paper doll's own slots (ref PaperDollFrame.lua:647-655): ctrl previews what you are
/// wearing, shift posts its link. Both read the unit-keyed `GetInventoryItemLink`, the binding this
/// arc added — so this also pins that the getter answers for `"player"`.
#[test]
fn the_paper_doll_slots_preview_and_post_what_you_wear() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for file in [
        "Fonts.xml",
        "MoneyFrame.xml",
        "UiPanels.xml",
        "UIParent.xml",
        "GameTooltip.xml",
        "CharacterFrame.xml",
        "DressUpFrame.xml",
        "ChatFrame.xml",
    ] {
        load_xml(&s, file);
    }
    let mut slots = InventorySlots::default();
    slots[1] = Some(InvSlotView {
        item_id: 1234,
        icon: Some("Interface\\Icons\\INV_Helmet_01".into()),
        count: 1,
        quality: 2,
        name: Some("Test Helm".into()),
        link: Some("|cff1eff00|Hitem:1234:0:0:0|h[Test Helm]|h|r".into()),
        equip_slots: vec![1],
        ..Default::default()
    });
    s.set_inventory_slots(slots);
    assert_eq!(
        s.eval::<String>("return GetInventoryItemLink(\"player\", 1)")
            .unwrap(),
        "|cff1eff00|Hitem:1234:0:0:0|h[Test Helm]|h|r"
    );

    // CTRL on the head slot → the room, wearing the helm.
    s.set_modifiers(false, true, false);
    s.run("BenillaPaperDollSlot_OnClick(CharacterHeadSlot, \"LeftButton\")")
        .unwrap();
    s.set_modifiers(false, false, false);
    assert_eq!(
        s.take_dressup_intents(),
        vec![DressUpIntent::Dress, DressUpIntent::TryOn(1234)]
    );

    // SHIFT with chat open → the link; the item is never picked up either way.
    assert!(s.focus_editbox("ChatFrameEditBox"));
    s.set_modifiers(true, false, false);
    s.run("BenillaPaperDollSlot_OnClick(CharacterHeadSlot, \"LeftButton\")")
        .unwrap();
    s.set_modifiers(false, false, false);
    assert_eq!(
        s.eval::<String>("return ChatFrameEditBox:GetText()")
            .unwrap(),
        "|cff1eff00|Hitem:1234:0:0:0|h[Test Helm]|h|r"
    );
    assert!(
        s.cursor_item().is_none(),
        "a modified doll click never picks the worn item up"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// A slot whose item template has not answered yet has **no link** — a state the real client never
/// has (its item cache is synchronous), so the reference carries no guard and `EditBox:Insert`
/// takes a string. Shift-clicking such a slot must post nothing and raise nothing; the click after
/// the answer lands works normally.
#[test]
fn shift_clicking_an_unresolved_slot_posts_nothing_and_never_raises() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for file in [
        "Fonts.xml",
        "MoneyFrame.xml",
        "UiPanels.xml",
        "UIParent.xml",
        "GameTooltip.xml",
        "CharacterFrame.xml",
        "DressUpFrame.xml",
        "ChatFrame.xml",
    ] {
        load_xml(&s, file);
    }
    // The slot is occupied but unresolved: an item id with no name/quality yet, so no link.
    let mut slots = InventorySlots::default();
    slots[1] = Some(InvSlotView {
        item_id: 1234,
        count: 1,
        ..Default::default()
    });
    s.set_inventory_slots(slots);
    assert!(s.focus_editbox("ChatFrameEditBox"));

    s.set_modifiers(true, false, false);
    s.run("BenillaPaperDollSlot_OnClick(CharacterHeadSlot, \"LeftButton\")")
        .unwrap();
    s.set_modifiers(false, false, false);
    assert_eq!(
        s.eval::<String>("return ChatFrameEditBox:GetText()")
            .unwrap(),
        "",
        "nothing posts while the template is in flight"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // And ctrl on the same unresolved slot is inert too (DressUpItemLink's own nil guard).
    s.set_modifiers(false, true, false);
    s.run("BenillaPaperDollSlot_OnClick(CharacterHeadSlot, \"LeftButton\")")
        .unwrap();
    s.set_modifiers(false, false, false);
    assert!(
        s.take_dressup_intents().is_empty(),
        "no try-on for an item we cannot even name yet"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The window's own controls: Reset re-dresses (and clicks), closing it empties the booth, and the
/// rotate buttons move the pane's yaw by the reference's own ±0.03 per OnClick — which fires on
/// BOTH mouse edges, so one tap is 0.06 (decision 0638 §3).
#[test]
fn reset_re_dresses_close_empties_and_the_arrows_spin_the_pane() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_room(&s);
    s.run("ShowUIPanel(DressUpFrame)").unwrap();
    let _ = s.take_sounds();
    let _ = s.take_dressup_intents();
    s.resolve();

    // The pane's OnLoad seeded the ref's default facing.
    assert!(
        (s.dressup_yaw() - 0.61).abs() < 1e-6,
        "ref UIParent.lua:1422"
    );

    s.run("DressUpFrameResetButton:Click()").unwrap();
    assert_eq!(s.take_dressup_intents(), vec![DressUpIntent::Dress]);
    assert_eq!(
        s.take_sounds(),
        vec![SoundRequest::KitName("gsTitleOptionOK".into())],
        "Reset plays the ref's own kit"
    );

    // One tap of rotate-left: OnClick on press AND release, −0.03 each.
    let before = s.dressup_yaw();
    let (x, y) = s
        .eval::<(f32, f32)>(
            "return (DressUpModelFrameRotateLeftButton:GetLeft() \
                     + DressUpModelFrameRotateLeftButton:GetRight()) / 2, \
                    (DressUpModelFrameRotateLeftButton:GetTop() \
                     + DressUpModelFrameRotateLeftButton:GetBottom()) / 2",
        )
        .unwrap();
    s.mouse_button(x, y, "LeftButton", true);
    s.mouse_button(x, y, "LeftButton", false);
    assert!(
        (s.dressup_yaw() - (before - 0.06)).abs() < 1e-5,
        "a tap fires OnClick twice: {} → {}",
        before,
        s.dressup_yaw()
    );

    s.run("HideUIPanel(DressUpFrame)").unwrap();
    assert_eq!(
        s.take_dressup_intents(),
        vec![DressUpIntent::Close],
        "closing the window empties the booth"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
