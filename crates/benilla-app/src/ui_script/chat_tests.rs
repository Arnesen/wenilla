//! Shipped end-to-end tests for the chat window + input (decision 0084, the chat arc): the real
//! `assets/ui/ChatFrame.xml` loaded through the engine loader, driven exactly as the app drives it —
//! `add_chat_message` → `resolve`/`extract` (lines render in the pinned colors), the wheel → the Lua
//! `OnMouseWheel` handler → `ScrollUp` (which freezes the fade), and the input EditBox's ENTER →
//! `OnEnterPressed` → `SubmitChatInput` → `take_chat_input` (the app then parses it — see
//! `crate::ui_chat`'s `parse_line` tests for the `/say`//`/yell`//`/w` mapping).

use benilla_ui::script::{ExtractedQuad, QuadContent, UiScript};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error.
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

/// The full ChatFrame stack (fonts first, so the FontString's `inherits="ChatFontNormal"` resolves).
fn chat_frame() -> UiScript {
    let mut s = UiScript::new().unwrap();
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "ChatFrame.xml");
    s.set_screen_size(1600.0, 900.0);
    s.resolve();
    s
}

/// The color+alpha of the first Text quad whose text equals `t`.
fn text_color(quads: &[ExtractedQuad], t: &str) -> Option<[f32; 4]> {
    quads.iter().find_map(|q| match &q.content {
        QuadContent::Text {
            text: Some(x),
            color: Some(c),
            ..
        } if x == t => Some(*c),
        _ => None,
    })
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

#[test]
fn injected_lines_render_in_the_pinned_colors() {
    let mut s = chat_frame();
    // The app's feed passes 0..1 floats from the pinned type→color table; the seam quantizes them.
    // SAY white (FFFFFF), SYSTEM yellow (FFFF00 — the GM-feedback color), LOOT green (00AA00).
    s.add_chat_message("ChatFrame1", "[Tri] says: hi", 1.0, 1.0, 1.0);
    s.add_chat_message("ChatFrame1", "You give 500 copper.", 1.0, 1.0, 0.0);
    s.add_chat_message(
        "ChatFrame1",
        "You receive loot: [Tough Jerky].",
        0.0,
        170.0 / 255.0,
        0.0,
    );
    s.resolve();
    let quads = s.extract();

    let say = text_color(&quads, "[Tri] says: hi").expect("say line rendered");
    assert!(
        close(say[0], 1.0) && close(say[1], 1.0) && close(say[2], 1.0),
        "say white: {say:?}"
    );
    assert!(close(say[3], 1.0), "a fresh line is fully opaque");

    let sys = text_color(&quads, "You give 500 copper.").expect("system line rendered");
    assert!(
        close(sys[0], 1.0) && close(sys[1], 1.0) && close(sys[2], 0.0),
        "system yellow: {sys:?}"
    );

    let loot = text_color(&quads, "You receive loot: [Tough Jerky].").expect("loot line rendered");
    assert!(
        close(loot[0], 0.0) && close(loot[1], 170.0 / 255.0) && close(loot[2], 0.0),
        "loot green: {loot:?}"
    );
}

#[test]
fn newest_line_sits_at_the_bottom() {
    let mut s = chat_frame();
    s.add_chat_message("ChatFrame1", "older", 1.0, 1.0, 1.0);
    s.add_chat_message("ChatFrame1", "newer", 1.0, 1.0, 1.0);
    s.resolve();
    let quads = s.extract();
    let y = |t: &str| {
        quads
            .iter()
            .find_map(|q| match &q.content {
                QuadContent::Text { text: Some(x), .. } if x == t => q.rect.map(|r| r.bottom),
                _ => None,
            })
            .unwrap()
    };
    // y-up: the newest line's band is lower than the older one's.
    assert!(y("newer") < y("older"), "newest renders at the bottom");
}

#[test]
fn wheel_scroll_freezes_the_fade_then_resumes() {
    let mut s = chat_frame();
    // Short fade so a tick visibly ramps: straight into phase 2 (timeVisible 0), 4s ramp.
    s.run("ChatFrame1:SetTimeVisible(0); ChatFrame1:SetFadeDuration(4)")
        .unwrap();
    for t in ["L0", "L1", "L2"] {
        s.add_chat_message("ChatFrame1", t, 1.0, 1.0, 1.0);
    }
    s.resolve();

    // One tick at the bottom ramps every line's alpha down.
    s.tick(1.0);
    s.resolve();
    let a1 = text_color(&s.extract(), "L1").expect("L1 visible")[3];
    assert!(a1 < 1.0 && a1 > 0.0, "the line faded partway: {a1}");

    // Scroll up via the wheel (a point inside the docked frame: BOTTOMLEFT (32,85), 430x160). The
    // Lua OnMouseWheel handler calls ScrollUp → no longer AtBottom → the fade must freeze.
    s.mouse_wheel(100.0, 150.0, 1.0);
    s.tick(2.0);
    s.resolve();
    let a2 = text_color(&s.extract(), "L1").expect("L1 still visible");
    assert!(
        close(a2[3], a1),
        "frozen while scrolled up: {} vs {a1}",
        a2[3]
    );

    // Wheel back down to the bottom → the fade resumes.
    s.mouse_wheel(100.0, 150.0, -1.0);
    s.tick(1.0);
    s.resolve();
    let a3 = text_color(&s.extract(), "L1").expect("L1 visible")[3];
    assert!(a3 < a1, "the fade resumed at the bottom: {a3} < {a1}");
}

#[test]
fn input_editbox_enter_drains_the_typed_line() {
    let mut s = chat_frame();
    // ENTER opens (the app calls focus_editbox); typing goes to the focused box.
    assert!(s.focus_editbox("ChatFrameEditBox"), "the edit box focuses");
    assert!(s.has_keyboard_focus(), "focus gates the world's keys");
    s.char_input("/yell hi");
    // ENTER → OnEnterPressed → SubmitChatInput(GetText()) + SetText('') + ClearFocus + Hide.
    assert!(s.key_input("ENTER"), "the box consumes ENTER");
    assert_eq!(s.take_chat_input(), vec!["/yell hi".to_string()]);
    assert!(
        !s.has_keyboard_focus(),
        "submit closes the box (ClearFocus + Hide)"
    );
    assert!(s.take_chat_input().is_empty(), "drained");
}

#[test]
fn input_escape_closes_without_submitting() {
    let mut s = chat_frame();
    assert!(s.focus_editbox("ChatFrameEditBox"));
    s.char_input("hello");
    assert!(s.key_input("ESCAPE"), "the box consumes ESCAPE");
    assert!(s.take_chat_input().is_empty(), "escape submits nothing");
    assert!(!s.has_keyboard_focus(), "escape closes the box");
}

/// The shipped chat box takes plain arrows and history recall — end-to-end over the real XML.
/// Guards the exact in-game gap the director hit: the ref template's `ignoreArrows="true"` (which
/// benilla deliberately drops — plain arrows edit; the divergence note in ChatFrame.xml) would
/// leave LEFT/RIGHT consumed-but-dead here, and an unpushed history would leave Up/Down empty.
#[test]
fn chat_box_arrows_edit_and_history_recalls() {
    use benilla_ui::script::{EditAction, EditUnit};
    let mut s = chat_frame();
    assert!(s.focus_editbox("ChatFrameEditBox"));
    s.char_input("ab");
    // Plain LEFT must MOVE the caret (not be swallowed): a backspace after it deletes 'a', not 'b'.
    s.editbox_action(EditAction::Move {
        unit: EditUnit::Char,
        back: true,
        extend: false,
    });
    s.editbox_action(EditAction::Delete {
        unit: EditUnit::Char,
        back: true,
    });
    assert_eq!(
        s.eval::<String>("return ChatFrameEditBox:GetText()")
            .unwrap(),
        "b",
        "plain arrows move the caret in the shipped chat box"
    );

    // Submit, then push the canonical line the router would (ui_chat::input::history_line) — the
    // asynchronous ChatEdit_AddHistory slot.
    s.run("ChatFrameEditBox:SetText('')").unwrap();
    s.char_input("/yell hi");
    assert!(s.key_input("ENTER"));
    assert_eq!(s.take_chat_input(), vec!["/yell hi".to_string()]);
    assert!(
        s.editbox_add_history("ChatFrameEditBox", "/yell hi"),
        "the seam finds the shipped box by name after it closed"
    );

    // Reopen (the app refocuses on the next Enter): Up recalls, Down restores the draft.
    assert!(s.focus_editbox("ChatFrameEditBox"));
    s.char_input("dra");
    s.editbox_action(EditAction::HistoryPrev);
    assert_eq!(
        s.eval::<String>("return ChatFrameEditBox:GetText()")
            .unwrap(),
        "/yell hi",
        "Up recalls the submitted line"
    );
    s.editbox_action(EditAction::HistoryNext);
    assert_eq!(
        s.eval::<String>("return ChatFrameEditBox:GetText()")
            .unwrap(),
        "dra",
        "Down past the newest restores the in-progress draft"
    );
}

/// Decision 0843 — the chat body as a dismiss surface, the director's stuck-spell gesture end to
/// end through the shipped XML: a spell dragged out of the spellbook and RELEASED over the chat
/// keeps carrying (a drag release is never a click — 0218's byte-verified trigger), the follow-up
/// completed LEFT CLICK on the chat body dismisses it, and the same click leaves an ITEM payload
/// untouched (a silent item dismissal would be a destroy — only the world-drop popup offers that).
#[test]
fn chat_click_dismisses_a_stuck_spell_but_not_an_item() {
    use benilla_ui::script::{
        ContainerSlot, ContainerState, SpellBookState, SpellSlotView, SpellTabView,
    };
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for f in [
        "Fonts.xml",
        "UiPanels.xml",
        "GameTooltip.xml",
        "Cooldown.xml",
        "ChatFrame.xml",
        "SpellBookFrame.xml",
    ] {
        load_xml(&s, f);
    }
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
    s.set_spellbook(SpellBookState {
        tabs: vec![SpellTabView {
            name: "Fire".into(),
            texture: Some("Interface\\Icons\\Spell_Fire_FlameBolt".into()),
            offset: 0,
            num_spells: 1,
        }],
        slots: vec![SpellSlotView {
            spell_id: 133,
            name: "Fireball".into(),
            rank: Some("Rank 1".into()),
            texture: Some("Interface\\Icons\\Spell_Fire_FlameBolt".into()),
            passive: false,
            current: false,
            cooldown: None,
            ..Default::default()
        }],
    });
    s.run("ToggleSpellBook(BOOKTYPE_SPELL)").unwrap();
    s.resolve();

    // Drag Fireball off its book button (press → past the 4px threshold → the payload is up).
    let (l, r, t, b) = (
        s.eval::<f32>("return BenillaSpellButton1:GetLeft()")
            .unwrap(),
        s.eval::<f32>("return BenillaSpellButton1:GetRight()")
            .unwrap(),
        s.eval::<f32>("return BenillaSpellButton1:GetTop()")
            .unwrap(),
        s.eval::<f32>("return BenillaSpellButton1:GetBottom()")
            .unwrap(),
    );
    let (x1, y1) = ((l + r) * 0.5, (t + b) * 0.5);
    s.mouse_button(x1, y1, "LeftButton", true);
    s.mouse_move(x1 + 20.0, y1);
    assert!(
        s.cursor_payload().is_some(),
        "OnDragStart picked the spell up"
    );

    // Release the DRAG over the chat body: keeps carrying (OnClick never fires on a drag).
    let (cx, cy) = (200.0, 150.0); // inside ChatFrame1 (BOTTOMLEFT 32,85 + 430×120)
    s.mouse_move(cx, cy);
    s.mouse_button(cx, cy, "LeftButton", false);
    assert!(
        s.cursor_payload().is_some(),
        "a drag release over the chat keeps carrying"
    );

    // The completed click on the chat body dismisses the stuck spell.
    s.mouse_button(cx, cy, "LeftButton", true);
    s.mouse_button(cx, cy, "LeftButton", false);
    assert!(s.errors().is_empty(), "{:?}", s.errors());
    assert!(
        s.cursor_payload().is_none(),
        "a chat click dismisses a spell payload"
    );

    // An ITEM payload survives the same click untouched.
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
            link: None,
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
    s.run("C_Container.PickupContainerItem(0, 1)").unwrap();
    assert!(s.cursor_item().is_some(), "fixture: the item is held");
    s.mouse_button(cx, cy, "LeftButton", true);
    s.mouse_button(cx, cy, "LeftButton", false);
    assert!(
        s.cursor_item().is_some(),
        "a chat click never touches an item payload"
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}
