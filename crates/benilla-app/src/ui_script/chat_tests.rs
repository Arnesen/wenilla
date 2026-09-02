//! Shipped end-to-end tests for the chat window + input (decision 0084, the chat arc): the real
//! `assets/ui/ChatFrame.xml` loaded through the engine loader, driven exactly as the app drives it —
//! `add_chat_message` → `resolve`/`extract` (lines render in the pinned colors), the wheel → the Lua
//! `OnMouseWheel` handler → `ScrollUp` (which freezes the fade), and the input EditBox's ENTER →
//! `OnEnterPressed` → `SubmitChatInput` → `take_chat_input` (the app then parses it — see
//! `crate::ui_chat`'s `parse_line` tests for the `/say`//`/yell`//`/w` mapping).

use benilla_ui::script::{ExtractedQuad, QuadContent, UiScript};

use super::test_ui::load_ui as load_xml;

/// The full ChatFrame stack (fonts first, so the FontString's `inherits="ChatFontNormal"` resolves).
fn chat_frame() -> UiScript {
    let mut s = UiScript::new().unwrap();
    load_xml(&s, "Fonts.xml");
    // GameTooltip.xml + UIDropDownMenu.xml are real RUNTIME dependencies of the chat tabs since
    // decision 1589: a left click closes any open menu (`CloseDropDownMenus`, the reference's own
    // first move in `FCF_Tab_OnClick`) and a right click opens the window's options menu.
    // `benilla.toc` already orders both ahead of ChatFrame.xml (l.60/64 vs l.399); the harness
    // says so too, rather than a guard that would hide a real ordering fault. (The tooltip file is
    // the dropdown kit's own dependency — its MenuBackdrop reads `TOOLTIP_DEFAULT_COLOR`.)
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "UIDropDownMenu.xml");
    load_xml(&s, "Interface\\FrameXML\\UIMenu.xml"); // the kit the chat menus build from
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

/// The whole fade round trip as it reaches a real chat window: a line ramps down, a scroll brings
/// it back to full, the scrolled-up view then holds it there, and returning to the bottom lets it
/// ramp again. The re-arm half is `msgframe-fade-rearm-law.md` — every scroll entry reaches
/// `0x788b80` or the relayout's `0x788af0`; before it, a faded-out chat could not be recovered by
/// any input the client offers (director-reported, 2026-08-29).
#[test]
fn wheel_scroll_re_arms_the_fade_then_freezes_it() {
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
    // Lua OnMouseWheel handler calls ScrollUp, which re-arms every displayed line to full alpha —
    // and, no longer AtBottom, the tick then holds it there.
    s.mouse_wheel(100.0, 150.0, 1.0);
    s.resolve();
    let a2 = text_color(&s.extract(), "L1").expect("L1 still visible")[3];
    assert!(close(a2, 1.0), "the scroll brought the line back: {a2}");
    s.tick(2.0);
    s.resolve();
    let a3 = text_color(&s.extract(), "L1").expect("L1 still visible")[3];
    assert!(close(a3, 1.0), "frozen while scrolled up: {a3}");

    // Wheel back down to the bottom — that scroll re-arms too, so the ramp restarts from full.
    s.mouse_wheel(100.0, 150.0, -1.0);
    s.tick(1.0);
    s.resolve();
    let a4 = text_color(&s.extract(), "L1").expect("L1 visible")[3];
    assert!(a4 < 1.0 && a4 > 0.0, "the fade resumed at the bottom: {a4}");
    assert!(close(a4, a1), "and from a full re-arm: {a4} vs {a1}");
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
        "MoneyFrame.xml",
        "UiPanels.xml",
        "GameTooltip.xml",
        "Cooldown.xml",
        "Interface\\FrameXML\\UIMenu.xml", // the kit ChatMenu/EmoteMenu/VoiceMacroMenu build from
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
        s.eval::<f32>("return SpellButton1:GetLeft()").unwrap(),
        s.eval::<f32>("return SpellButton1:GetRight()").unwrap(),
        s.eval::<f32>("return SpellButton1:GetTop()").unwrap(),
        s.eval::<f32>("return SpellButton1:GetBottom()").unwrap(),
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
            petition: None,
            already_bound: false,
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
    s.run("PickupContainerItem(0, 1)").unwrap();
    assert!(s.cursor_item().is_some(), "fixture: the item is held");
    s.mouse_button(cx, cy, "LeftButton", true);
    s.mouse_button(cx, cy, "LeftButton", false);
    assert!(
        s.cursor_item().is_some(),
        "a chat click never touches an item payload"
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

// ── ChatTypeInfo: the addon-facing color table ────────────────────────────────────────────────

/// `ChatTypeInfo` carries the shipped default chat colors twice: once in `assets/ui/ChatFrame.xml`
/// for addons to read, and once in [`crate::ui_chat::default_color`] for our own feed to render.
/// Both are the same wow-re byte table (`chat-color-table.md`, the static registry at
/// `.rdata 0x804710`) — so this is the gate that makes the duplication safe: every kind we model
/// must agree to the byte, and the table's shape (`sticky`/`id`) must be the reference's.
#[test]
fn chat_type_info_matches_the_host_color_table() {
    use crate::ui_chat::{default_color, ChatEventKind as K};

    /// Each modeled kind and its `ChatTypeInfo` key — the reference's own spellings.
    const PAIRS: &[(&str, K, i64)] = &[
        ("SAY", K::Say, 1),
        ("PARTY", K::Party, 2),
        ("RAID", K::Raid, 3),
        ("GUILD", K::Guild, 4),
        ("OFFICER", K::Officer, 5),
        ("YELL", K::Yell, 6),
        ("WHISPER", K::Whisper, 7),
        ("WHISPER_INFORM", K::WhisperInform, 8),
        ("EMOTE", K::Emote, 9),
        ("TEXT_EMOTE", K::TextEmote, 10),
        ("SYSTEM", K::System, 11),
        ("MONSTER_SAY", K::MonsterSay, 12),
        ("MONSTER_YELL", K::MonsterYell, 13),
        ("MONSTER_EMOTE", K::MonsterEmote, 14),
        ("MONSTER_WHISPER", K::MonsterWhisper, 27),
        ("CHANNEL", K::Channel, 15),
        ("CHANNEL_JOIN", K::ChannelJoin, 16),
        ("CHANNEL_LEAVE", K::ChannelLeave, 17),
        ("CHANNEL_NOTICE", K::ChannelNotice, 19),
        ("CHANNEL_NOTICE_USER", K::ChannelNoticeUser, 20),
        ("CHANNEL_LIST", K::ChannelList, 18),
        ("AFK", K::Afk, 21),
        ("DND", K::Dnd, 22),
        ("IGNORED", K::Ignored, 23),
        ("SKILL", K::Skill, 24),
        ("LOOT", K::Loot, 25),
        ("MONEY", K::Money, 87),
        ("COMBAT_XP_GAIN", K::CombatXpGain, 46),
        ("RAID_LEADER", K::RaidLeader, 88),
        ("RAID_WARNING", K::RaidWarning, 89),
        ("RAID_BOSS_EMOTE", K::RaidBossEmote, 91),
        ("BATTLEGROUND", K::Battleground, 93),
        ("BATTLEGROUND_LEADER", K::BattlegroundLeader, 94),
        ("BG_SYSTEM_NEUTRAL", K::BgSystemNeutral, 83),
        ("BG_SYSTEM_ALLIANCE", K::BgSystemAlliance, 84),
        ("BG_SYSTEM_HORDE", K::BgSystemHorde, 85),
    ];

    let s = chat_frame();
    for (name, kind, want_id) in PAIRS {
        let (r, g, b, id): (f64, f64, f64, i64) = s
            .eval(&format!(
                r#"local i = ChatTypeInfo["{name}"] return i.r, i.g, i.b, i.id"#
            ))
            .unwrap_or_else(|e| panic!(r#"ChatTypeInfo["{name}"]: {e}"#));
        let got = [
            (r * 255.0).round() as u8,
            (g * 255.0).round() as u8,
            (b * 255.0).round() as u8,
        ];
        assert_eq!(got, default_color(*kind), r#"ChatTypeInfo["{name}"] color"#);
        // `id` is GetChatTypeIndex's 1-based registry slot, asserted EXACTLY rather than as a
        // range: four of these ship the same FFDBB7 and a range check would let a transposition
        // inside that family through with both colors still matching.
        assert_eq!(id, *want_id, r#"ChatTypeInfo["{name}"].id"#);
    }
}

/// The table's *shape*, transcribed rather than derived: the reference's 105 keys, its five sticky
/// types (the set `ui_chat::edit`'s `SendType::sticky` already quotes), the two keys FrameXML
/// declares that the engine's color registry does not contain, and the ten boot-seeded channel
/// extras. An addon reads `.sticky` and `.id` as often as it reads the color.
#[test]
fn chat_type_info_has_the_references_shape() {
    let s = chat_frame();

    let count: i64 = s
        .eval("local n = 0 for _ in pairs(ChatTypeInfo) do n = n + 1 end return n")
        .unwrap();
    assert_eq!(count, 105, "the reference declares 105 keys");

    let sticky: String = s
        .eval(
            "local t = {} for k, v in pairs(ChatTypeInfo) do if v.sticky == 1 then \
             table.insert(t, k) end end table.sort(t) return table.concat(t, \" \")",
        )
        .unwrap();
    assert_eq!(sticky, "BATTLEGROUND GUILD PARTY RAID SAY");

    // REPLY and COMBAT_ERROR are declared by FrameXML and absent from the engine's 94-entry
    // registry, so GetChatTypeIndex answers 0 for both — but their COLORS differ, which is the
    // trap this asserts. Nothing ever writes COMBAT_ERROR. REPLY is written by hand inside the
    // UPDATE_CHAT_COLOR handler, which mirrors WHISPER into it (ChatFrame.lua l.1357-1365), so
    // its end state is WHISPER's FF80FF. Seeding both white reads as symmetric and is wrong.
    for (name, want) in [
        ("REPLY", [255u8, 128, 255]),
        ("COMBAT_ERROR", [255, 255, 255]),
    ] {
        let (id, r, g, b): (i64, f64, f64, f64) = s
            .eval(&format!(
                r#"local i = ChatTypeInfo["{name}"] return i.id, i.r, i.g, i.b"#
            ))
            .unwrap();
        assert_eq!(id, 0, r#""{name}".id — not in the engine's registry"#);
        let got = [
            (r * 255.0).round() as u8,
            (g * 255.0).round() as u8,
            (b * 255.0).round() as u8,
        ];
        assert_eq!(got, want, r#""{name}" color"#);
    }

    // The extras: CHANNEL1..CHANNEL10, indices 95..104, each the live CHANNEL entry's FFC0C0.
    for n in 1..=10 {
        let (id, r, g, b): (i64, f64, f64, f64) = s
            .eval(&format!(
                r#"local i = ChatTypeInfo["CHANNEL{n}"] return i.id, i.r, i.g, i.b"#
            ))
            .unwrap();
        assert_eq!(id, 94 + n, "CHANNEL{n}.id");
        let rgb = [
            (r * 255.0).round() as u8,
            (g * 255.0).round() as u8,
            (b * 255.0).round() as u8,
        ];
        assert_eq!(rgb, [255, 192, 192], "CHANNEL{n} color");
    }
}

/// **Every event name we fire is a key the reference's own `ChatTypeInfo` carries.**
/// `ChatFrame_OnEvent` recovers the type with `strsub(event, 10)` and indexes
/// `ChatTypeInfo[type]` with it — a name that misses that table is a name whose colour, id and
/// sticky flag an addon cannot look up, so this is the check that our `CHAT_MSG_*` spellings are
/// the reference's and not ours.
///
/// Swept over `ChatEventKind::ALL` rather than a second hand-written list, so a kind added without
/// a matching table key fails here instead of shipping a name nothing can resolve.
#[test]
fn fired_event_names_are_all_chat_type_info_keys() {
    use crate::ui_chat::{event_name, ChatEventKind as K};

    let s = chat_frame();
    for &kind in K::ALL {
        let name = event_name(kind);
        let key = name
            .strip_prefix("CHAT_MSG_")
            .unwrap_or_else(|| panic!("{name}: every fired chat event is CHAT_MSG_-prefixed"));
        let present: bool = s
            .eval(&format!(r#"return ChatTypeInfo["{key}"] ~= nil"#))
            .unwrap();
        assert!(present, r#"{name} → ChatTypeInfo["{key}"] is missing"#);
    }
}

// ── The seven chat windows (NUM_CHAT_WINDOWS) ────────────────────────────────────────────────
//
// benilla shipped two windows against a constant the reference sets to seven. Everything below is
// a claim about the five that were missing, checked against the shipped `ChatFrame.xml` itself
// rather than against the loader's idea of it.

/// `NUM_CHAT_WINDOWS = 7` (ref ChatFrame.lua l.5) and every index it promises resolves to a real
/// `ScrollingMessageFrame`. _LazyPig walks exactly this loop and indexes the result with no nil
/// guard (`LazyPig.lua:1992`), so the constant without the frames is worse than neither.
#[test]
fn every_window_num_chat_windows_promises_is_a_real_frame() {
    let s = chat_frame();
    assert_eq!(s.eval::<i64>("return NUM_CHAT_WINDOWS").unwrap(), 7);
    for i in 1..=7 {
        let ok: bool = s
            .eval(&format!(
                "local f = getglobal('ChatFrame{i}') \
                 return f ~= nil and f.AddMessage ~= nil and f:GetID() == {i}"
            ))
            .unwrap();
        assert!(ok, "ChatFrame{i} is a real message frame carrying its id");
    }
}

/// The corpus walk itself, verbatim from `_LazyPig/LazyPig.lua:1992` down to the unguarded
/// `getglobal(...):IsVisible()` that used to die at i=3.
#[test]
fn the_lazypig_window_walk_survives_all_seven_indices() {
    let s = chat_frame();
    let visible: i64 = s
        .eval(
            "local n = 0\n\
             for i = 1, NUM_CHAT_WINDOWS do\n\
               local ChatFrame = getglobal('ChatFrame'..i)\n\
               if ChatFrame:IsVisible() then n = n + 1 end\n\
             end\n\
             return n",
        )
        .unwrap();
    assert_eq!(visible, 1, "only the selected dock window is visible");
}

/// ChatFrame3..7 ship hidden and with no `isDocked`, the reference's own chat-cache state
/// (`DOCKED 0 / SHOWN 0`). This is not cosmetic: `Outfitter.lua:3099` reaches an UNGUARDED
/// `getglobal('ChatFrame'..i..'Tab'):GetText()` for any window that is visible *or* docked, and we
/// build no tabs past ChatFrame2Tab.
///
/// **ChatFrame1 and ChatFrame2 DO carry it**, and that is the half this test gained with the
/// move/resize arc: the reference's own gates read `chatFrame.isDocked` — `isDocked and chatFrame
/// ~= DEFAULT_CHAT_FRAME` is what stops the Combat Log being dragged out of the dock it is
/// anchored into — so the flag has a consumer now rather than being an advertisement. Both have
/// tabs (1575), so the Outfitter walk below is still safe.
#[test]
fn the_undocked_windows_are_hidden_and_carry_no_is_docked() {
    let s = chat_frame();
    for i in 1..=2 {
        let docked: bool = s
            .eval(&format!("return ChatFrame{i}.isDocked ~= nil"))
            .unwrap();
        assert!(docked, "ChatFrame{i} is docked and says so");
    }
    for i in 2..=7 {
        let shown: bool = s.eval(&format!("return ChatFrame{i}:IsShown()")).unwrap();
        assert!(!shown, "ChatFrame{i} ships hidden");
    }
    for i in 3..=7 {
        let docked: bool = s
            .eval(&format!("return ChatFrame{i}.isDocked ~= nil"))
            .unwrap();
        assert!(!docked, "ChatFrame{i} carries no isDocked");
    }
    // And therefore the Outfitter walk never reaches a tab that does not exist.
    let ok: bool = s
        .eval(
            "for i = 1, NUM_CHAT_WINDOWS do\n\
               local f = getglobal('ChatFrame'..i)\n\
               if f and (f:IsVisible() or f.isDocked) then\n\
                 local tab = getglobal('ChatFrame'..i..'Tab')\n\
                 if not tab then return false end\n\
                 local _ = tab:GetText()\n\
               end\n\
             end\n\
             return true",
        )
        .unwrap();
    assert!(ok, "the Outfitter tab walk never touches a missing tab");
}

/// `GetChatWindowInfo`'s `shown` is not an independent opinion — it must agree with the frame the
/// shipped XML actually built, or an addon that trusts the getter and an addon that trusts the
/// frame will disagree about the same window. The drift guard between the Rust table and the XML.
#[test]
fn get_chat_window_info_shown_matches_the_shipped_frames() {
    let s = chat_frame();
    for i in 1..=7 {
        let agrees: bool = s
            .eval(&format!(
                // Both sides are normalised to a boolean before comparing: `IsShown` answers the
                // NUMBER 1 or nil (1830), so comparing it straight against `shown ~= nil` compares
                // a number with a boolean and is false for every window. This test is about the two
                // AGREEING, not about either one's shape.
                "local _, _, _, _, _, _, shown = GetChatWindowInfo({i})\n\
                 return (shown ~= nil) == (ChatFrame{i}:IsShown() ~= nil)"
            ))
            .unwrap();
        assert!(
            agrees,
            "window {i}: GetChatWindowInfo disagrees with the frame"
        );
    }
}

/// A named debug sink is a real ring of its own: `ChatFrame3:AddMessage` (IgniteStatus does this
/// seven times, TipBuddy once, Radar guarded, and AceDebug's `debugFrame` stores the frame) lands
/// in ChatFrame3 and nowhere near the window the player is reading.
#[test]
fn a_line_added_to_chat_frame3_lands_in_chat_frame3_only() {
    let mut s = chat_frame();
    s.add_chat_message("ChatFrame1", "a real line", 1.0, 1.0, 1.0);
    s.run("ChatFrame3:AddMessage('Radar: debug', 1, 1, 0)")
        .unwrap();
    assert_eq!(
        s.eval::<i64>("return ChatFrame3:GetNumMessages()").unwrap(),
        1
    );
    assert_eq!(
        s.eval::<i64>("return ChatFrame1:GetNumMessages()").unwrap(),
        1
    );
    // Hidden, so nothing of it reaches the screen.
    s.resolve();
    assert!(
        text_color(&s.extract(), "Radar: debug").is_none(),
        "a hidden window renders nothing"
    );
}

/// `FCF_SelectDockFrame(frame)` — the corpus idiom (CustomNameplates.lua:69, Roid-Macros
/// Utility.lua:40) is "un-hide the default chat frame before printing into it". It takes a FRAME,
/// where `BenillaFCF_SelectDock` takes a dock id.
///
/// **It used to RAISE on an undocked window and no longer does** (1714). The raise was right for a
/// client whose dock was the literal `for i = 1, 2`: "select ChatFrame5" had no meaning, and
/// answering with silence would have left the caller's next `AddMessage` in a still-hidden window.
/// With a real `DOCKED_CHAT_FRAMES` the reference's own answer applies — the assignment, then
/// `FCF_DockUpdate` hiding every docked window (none of them is the selection) and leaving the
/// undocked one exactly as it was. Not a nice answer, but the client's; and neither corpus caller
/// reaches it, since both pass `DEFAULT_CHAT_FRAME`.
#[test]
fn fcf_select_dock_frame_selects_by_frame_and_leaves_an_undocked_one_alone() {
    let s = chat_frame();
    s.run("FCF_SelectDockFrame(ChatFrame2)").unwrap();
    let (one, two): (bool, bool) = (
        s.eval("return ChatFrame1:IsShown()").unwrap(),
        s.eval("return ChatFrame2:IsShown()").unwrap(),
    );
    assert!(!one && two, "selecting the Combat Log swaps the dock");

    s.run("if not DEFAULT_CHAT_FRAME:IsVisible() then FCF_SelectDockFrame(DEFAULT_CHAT_FRAME) end")
        .unwrap();
    assert!(
        s.eval::<bool>("return ChatFrame1:IsShown()").unwrap(),
        "the corpus guard brings the default frame back"
    );

    // An undocked window: no raise, and the reference's consequence — the dock's members all
    // hide, because none of them is the selection, and ChatFrame5 is untouched (still hidden,
    // since nothing showed it).
    let shown5_before: bool = s
        .eval("return ChatFrame5:IsShown() and true or false")
        .unwrap();
    s.run("FCF_SelectDockFrame(ChatFrame5)").unwrap();
    assert_eq!(
        s.eval::<String>("return SELECTED_DOCK_FRAME:GetName()")
            .unwrap(),
        "ChatFrame5"
    );
    assert!(
        !s.eval::<bool>("return ChatFrame1:IsShown()").unwrap()
            && !s.eval::<bool>("return ChatFrame2:IsShown()").unwrap(),
        "FCF_DockUpdate hides every docked window when the selection is not one of them"
    );
    assert_eq!(
        s.eval::<bool>("return ChatFrame5:IsShown() and true or false")
            .unwrap(),
        shown5_before,
        "an undocked window is not in the loop, so nothing shows or hides it"
    );
}

/// **The idle dock writes nothing** (decision 1396). `FCF_OnUpdate`'s apply block used to run
/// unconditionally — ~24 `getglobal`s, two table builds and ~20 `SetAlpha` calls every frame of
/// every session, writing the values that were already there, at 36 µs/frame (1395 measured it as
/// the largest single handler in the client).
///
/// The probe is a sentinel the applier can never write: it only ever writes `{0, 0.5, 1.0} × reveal`,
/// and `reveal` is pinned at 0 with the cursor away from the dock. If the gate is removed, the very
/// next tick overwrites 0.42 with 0 and this goes red.
#[test]
fn an_idle_dock_stops_rewriting_the_tab_alpha_every_frame() {
    let mut s = chat_frame();
    s.mouse_move(1500.0, 850.0); // far from the dock: no hover, so `reveal` stays 0
    for _ in 0..8 {
        s.tick(0.016); // let the label measurements land and the applier reach its resting write
        s.resolve();
    }

    s.run("ChatFrame1Tab:SetAlpha(0.42)").unwrap();
    for _ in 0..10 {
        s.tick(0.016);
        s.resolve();
    }
    assert!(s.errors().is_empty(), "{:?}", s.errors());

    let alpha: f64 = s.eval("return ChatFrame1Tab:GetAlpha()").unwrap();
    assert!(
        (alpha - 0.42).abs() < 1e-6,
        "a settled dock must not rewrite its tab alpha — got {alpha}"
    );
}

/// The control for the guard above: the gate must not cost the reveal it is gating. A stationary
/// cursor over the dock for longer than `CHAT_TAB_SHOW_DELAY` fades the selected tab to full.
#[test]
fn hovering_the_dock_still_reveals_the_tabs() {
    let mut s = chat_frame();
    s.mouse_move(1500.0, 850.0);
    for _ in 0..4 {
        s.tick(0.016);
        s.resolve();
    }
    assert!(
        s.eval::<f64>("return ChatFrame1Tab:GetAlpha()").unwrap() < 1e-6,
        "the dock starts concealed"
    );

    // The dock's own centre, asked of the layout rather than hardcoded.
    let (x, y): (f32, f32) = s
        .eval(
            "return (ChatFrame1:GetLeft() + ChatFrame1:GetRight()) / 2, \
             (ChatFrame1:GetBottom() + ChatFrame1:GetTop()) / 2",
        )
        .unwrap();
    s.mouse_move(x, y);
    // 0.2 s stationary to arm the reveal, then the 0.15 s ramp — 25 frames covers both with room.
    for _ in 0..25 {
        s.tick(0.016);
        s.resolve();
    }
    assert!(s.errors().is_empty(), "{:?}", s.errors());

    let alpha: f64 = s.eval("return ChatFrame1Tab:GetAlpha()").unwrap();
    assert!(
        (alpha - 1.0).abs() < 1e-6,
        "a stationary hover reveals the selected tab at full alpha — got {alpha}"
    );
}

/// **A view at the bottom writes nothing** — `ChatFrame_OnUpdate`'s at-bottom branch used to call
/// `flash:Hide()` and zero `flashTimer` every frame; the 1.12 reference gates both behind
/// `flash:IsVisible()` (1396's residual item 3, now restored — our `flashTimer = 0` rides inside
/// the reference's gate, so a return-to-bottom during a hidden blink phase keeps the residual
/// phase, a divergence that costs nothing at rest).
///
/// The probe is a sentinel the gated branch can only zero: `flashTimer = 0.42` with the flash
/// hidden survives ten ticks. The controls: scrolled up, the bottom-button blink still runs its
/// 0.5s toggle; and scrolling back while the flash is LIT still hides it and zeroes the timer.
#[test]
fn a_chat_view_at_the_bottom_stops_rewriting_the_flash() {
    let mut s = chat_frame();
    for t in ["L0", "L1", "L2"] {
        s.add_chat_message("ChatFrame1", t, 1.0, 1.0, 1.0);
    }
    for _ in 0..3 {
        s.tick(0.016);
        s.resolve();
    }

    // At the bottom, flash hidden: the sentinel must survive.
    s.run("ChatFrame1.flashTimer = 0.42").unwrap();
    for _ in 0..10 {
        s.tick(0.016);
        s.resolve();
    }
    assert!(s.errors().is_empty(), "{:?}", s.errors());
    let timer: f64 = s.eval("return ChatFrame1.flashTimer").unwrap();
    assert!(
        (timer - 0.42).abs() < 1e-6,
        "a settled view must not rewrite flashTimer every frame — got {timer}"
    );
    assert!(
        !s.eval::<bool>("return ChatFrame1BottomButtonFlash:IsVisible()")
            .unwrap(),
        "the flash stays hidden at rest"
    );

    // Control 1: scrolled up, the blink still runs — the timer accumulates past 0.5 and shows
    // the flash (a point inside the docked frame, as the wheel test uses).
    s.mouse_wheel(100.0, 150.0, 1.0);
    s.tick(0.3); // 0.42 + 0.3 = 0.72 >= 0.5 -> toggle on
    s.resolve();
    assert!(
        s.eval::<bool>("return ChatFrame1BottomButtonFlash:IsVisible()")
            .unwrap(),
        "scrolled up, the bottom-button blink still lights"
    );

    // Control 2: back to the bottom while the flash is LIT — the transition still writes.
    s.mouse_wheel(100.0, 150.0, -1.0);
    s.tick(0.016);
    s.resolve();
    assert!(
        !s.eval::<bool>("return ChatFrame1BottomButtonFlash:IsVisible()")
            .unwrap(),
        "returning to the bottom hides a lit flash"
    );
    assert_eq!(
        s.eval::<f64>("return ChatFrame1.flashTimer").unwrap(),
        0.0,
        "and zeroes the timer with it (our reset rides inside the reference's gate)"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **Selecting the Combat Log must not stop the dock.** The falsification for decision 1588.
///
/// `FCF_OnUpdate` is the dock's whole driver — the hover reveal, both tabs' alphas, the label
/// settle, the whisper blink — and it used to ride `ChatFrame1`'s own `OnUpdate`.
/// `BenillaFCF_SelectDock(2)` hides ChatFrame1, and a hidden frame gets no `OnUpdate`
/// ([`UiScript::tick`] runs only effectively-visible frames, which is the reference's rule), so the
/// driver died at the exact moment the feature it drives was first used: the tabs froze mid-write,
/// still showing General as the selected one. That is what the director saw and reported, and every
/// test we had passed straight through it — including this file's two dock tests, because both only
/// ever exercise window 1.
///
/// The trap this walks around: calling `FCF_OnUpdate()` by hand — which the first live probe did,
/// and which every earlier test does — bypasses the visibility gate and reports healthy alphas on a
/// dead driver. So this drives the clock and nothing else.
#[test]
fn selecting_the_combat_log_keeps_the_dock_driver_running() {
    let mut s = chat_frame();
    // Hover the dock and let the reveal settle: General at full, Combat Log at half.
    let (x, y): (f32, f32) = s
        .eval(
            "return (ChatFrame1:GetLeft() + ChatFrame1:GetRight()) / 2, \
             (ChatFrame1:GetBottom() + ChatFrame1:GetTop()) / 2",
        )
        .unwrap();
    s.mouse_move(x, y);
    for _ in 0..25 {
        s.tick(0.016);
        s.resolve();
    }
    assert!(
        close(
            s.eval::<f32>("return ChatFrame1Tab:GetAlpha()").unwrap(),
            1.0
        ) && close(
            s.eval::<f32>("return ChatFrame2Tab:GetAlpha()").unwrap(),
            0.5
        ),
        "the hovered dock starts with General selected — this test must not pass vacuously"
    );

    // Click the Combat Log tab, then advance the clock WITHOUT touching FCF_OnUpdate.
    s.run("BenillaFCF_TabClick(2)").unwrap();
    for _ in 0..10 {
        s.tick(0.016);
        s.resolve();
    }
    assert!(s.errors().is_empty(), "{:?}", s.errors());

    assert!(
        s.eval::<bool>("return ChatFrame2:IsShown() and not ChatFrame1:IsShown()")
            .unwrap(),
        "the click swapped the windows"
    );
    let (a1, a2): (f32, f32) = s
        .eval("return ChatFrame1Tab:GetAlpha(), ChatFrame2Tab:GetAlpha()")
        .unwrap();
    assert!(
        close(a1, 0.5) && close(a2, 1.0),
        "the selected tab is the one at full alpha — got General {a1}, Combat Log {a2}"
    );
}

/// The other half of the same freeze, and the one a screenshot cannot show: with the driver dead,
/// `reveal` never decayed either, so the chat box and both tabs stayed lit after the cursor left.
#[test]
fn the_dock_still_fades_out_with_the_combat_log_selected() {
    let mut s = chat_frame();
    let (x, y): (f32, f32) = s
        .eval(
            "return (ChatFrame1:GetLeft() + ChatFrame1:GetRight()) / 2, \
             (ChatFrame1:GetBottom() + ChatFrame1:GetTop()) / 2",
        )
        .unwrap();
    s.mouse_move(x, y);
    for _ in 0..25 {
        s.tick(0.016);
        s.resolve();
    }
    s.run("BenillaFCF_TabClick(2)").unwrap();
    for _ in 0..10 {
        s.tick(0.016);
        s.resolve();
    }
    assert!(
        close(
            s.eval::<f32>("return ChatFrame2Tab:GetAlpha()").unwrap(),
            1.0
        ),
        "revealed before the cursor leaves"
    );

    s.mouse_move(1500.0, 850.0); // away from the dock
    for _ in 0..20 {
        s.tick(0.016);
        s.resolve();
    }
    assert!(s.errors().is_empty(), "{:?}", s.errors());
    let a2: f32 = s.eval("return ChatFrame2Tab:GetAlpha()").unwrap();
    assert!(
        a2 < 1e-6,
        "the dock conceals itself again on leave — got Combat Log tab alpha {a2}"
    );
}

/// The Combat Log window has the scroll column too — the third part of window 2 that was simply
/// never authored (after the tab, 1575, and the border art, 1579). It is a behavioural check, not
/// a name check: scroll up and the bottom button's flash must start blinking on window 2's own
/// button, which only happens if window 2 both HAS the button and runs `ChatFrame_OnUpdate`.
#[test]
fn the_combat_log_window_runs_its_own_bottom_button_blink() {
    let mut s = chat_frame();
    s.run("BenillaFCF_TabClick(2)").unwrap();
    for i in 0..40 {
        s.add_chat_message("ChatFrame2", &format!("line {i}"), 1.0, 1.0, 1.0);
    }
    s.run("ChatFrame2:ScrollUp()").unwrap();
    assert!(
        !s.eval::<bool>("return ChatFrame2:AtBottom()").unwrap(),
        "scrolled off the bottom — this test must not pass vacuously"
    );

    // CHAT_BUTTON_FLASH_TIME is 0.5 s; 40 frames of 16 ms crosses it.
    for _ in 0..40 {
        s.tick(0.016);
        s.resolve();
    }
    assert!(s.errors().is_empty(), "{:?}", s.errors());
    assert!(
        s.eval::<bool>("return ChatFrame2BottomButtonFlash:IsVisible()")
            .unwrap(),
        "window 2's bottom-button flash blinks while it is scrolled up"
    );
}
