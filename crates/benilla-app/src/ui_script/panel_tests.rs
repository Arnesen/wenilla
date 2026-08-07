use benilla_ui::script::{
    ExtractedQuad, GossipMenu, GossipOptionView, GossipQuestRow, MerchantItem, MerchantState,
    QuadContent, SoundRequest, UiScript,
};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error and returning the
/// frame count it materialized — the panel tests all load `UiPanels.xml` (decision 0084 §2's slot
/// manager) before the panel frame(s) under test, exactly as `ui_script.rs`'s own shipped-list
/// order does, so `ShowUIPanel`/`HideUIPanel` already exist when a frame's OnLoad/OnEvent
/// references them.
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

/// Find a bare frame's own rect via its `QuadContent::Frame` entry (every frame emits one, at its
/// resolved rect, whether or not it paints anything itself — `UiScript::extract`'s doc). Used for
/// the synthetic pushable=7 loot marker, which has no visual layers of its own.
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

/// Load the real `assets/ui/GossipFrame.xml` (the shipped gossip window) behind `UiPanels.xml`
/// into a bare engine and drive it with a synthetic gossip menu — the whole phase-3 chain minus
/// Bevy (decision 0081), now over the UIPanel slot manager (decision 0084): the hidden→shown
/// lifecycle on GOSSIP_SHOW goes through ShowUIPanel (landing the window at the left slot,
/// TOPLEFT UIParent 0,-104 — pin §4's rect assertion), the greeting + option rows rendering, a
/// coded row disabled, a row click queuing the right select intent, and a close through
/// HideUIPanel vacating the left slot. Quest-row rendering (decision 0088 §3, the shared row pool)
/// gets its own dedicated test below (`shipped_gossip_frame_renders_quest_rows_above_options`).
#[test]
fn shipped_gossip_frame_drives_end_to_end() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "ScrollTemplates.xml"); // the shared scroll kit the window rides
                                         // The window + its scroll frame (bar + child) + the 32-row shared pool (quest rows and option
                                         // rows both draw from it, decision 0088 §3 — the reference's own NUMGOSSIPBUTTONS) + the close
                                         // button + the GOODBYE button. The greeting and the NPC-name banner are FontString layers (the
                                         // real GossipGreetingText ref l.241 / GossipFrameNpcNameText ref l.170) — not their own frames.
    assert_eq!(
        load_xml(&s, "GossipFrame.xml"),
        40,
        "window + scroll + bar (+2 arrows) + child + 32 rows + close + goodbye"
    );

    // Hidden by default: no gossip icon on screen.
    s.resolve();
    let vendor_icon = |quads: &[ExtractedQuad]| {
        quads.iter().any(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                    if p.contains("VendorGossipIcon"))
        })
    };
    assert!(!vendor_icon(&s.extract()), "gossip window starts hidden");
    assert!(
        s.eval::<bool>("return GetLeftFrame() == nil").unwrap(),
        "left slot empty before any panel opens"
    );

    // The app's feed: a two-option menu (a vendor option + a coded petition option).
    s.set_gossip(Some(GossipMenu {
        greeting: Some("Greetings, traveler. How may I help you?".into()),
        quests: Vec::new(),
        options: vec![
            GossipOptionView {
                label: "Let me browse your goods.".into(),
                icon_type: "vendor".into(),
                coded: false,
            },
            GossipOptionView {
                label: "I wish to sign the petition.".into(),
                icon_type: "gossip".into(),
                coded: true,
            },
        ],
    }));
    s.fire_event("GOSSIP_SHOW", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // The window is shown (ShowUIPanel put it on the left slot), the greeting painted, the vendor
    // row's icon rendered.
    assert!(s
        .eval::<bool>("return BenillaGossipFrame:IsVisible()")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return BenillaGossipGreetingText:GetText()")
            .unwrap(),
        "Greetings, traveler. How may I help you?"
    );
    // Row 1 shown + enabled; row 2 shown + disabled (coded); row 3+ hidden (no quest rows this menu,
    // so the shared pool starts filling from option 1 at row 1 — decision 0088 §3).
    let states: (bool, bool, bool, bool) = s
        .eval(
            "return BenillaGossipRow1:IsVisible(), BenillaGossipRow1:IsEnabled(),\n\
                        BenillaGossipRow2:IsEnabled(), BenillaGossipRow3:IsVisible()",
        )
        .unwrap();
    assert_eq!(
        states,
        (true, true, false, false),
        "coded row disabled, extras hidden"
    );

    s.resolve();
    let quads = s.extract();

    // The slot anchor actually applied: the window's rect top-left sits at (0, 664) — screen
    // height 768 minus the left slot's 104px drop (pin §4's extract-rect assertion). The re-skinned
    // window has no solid-colour fill (the parchment quadrants are opaque), so it's found by its own
    // 384×512 frame quad rather than a background texture.
    let win = frame_rect(&quads, 384.0, 512.0);
    assert_eq!(
        (win.left, win.top),
        (0.0, 664.0),
        "gossip window landed at the left slot (TOPLEFT UIParent, 0, -104)"
    );

    // The four QuestGreeting quadrant slabs ARE the parchment art (ref-GossipFrame.xml l.13-44):
    // 256-wide left halves, 128-wide right halves, each 256 tall, pinned to their corner, each
    // sampling its whole texture (no TexCoords).
    let quad_rect = |needle: &str, w: f32, h: f32| {
        let q = quads
            .iter()
            .find(|q| {
                matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                        if p.contains(needle))
            })
            .unwrap_or_else(|| panic!("no texture quad for {needle}"));
        let r = q.rect.unwrap_or_else(|| panic!("no rect for {needle}"));
        assert!(
            (r.width() - w).abs() < 0.5 && (r.height() - h).abs() < 0.5,
            "{needle} is {w}×{h}, got {}×{}",
            r.width(),
            r.height()
        );
        r
    };
    let tl = quad_rect("UI-QuestGreeting-TopLeft", 256.0, 256.0);
    assert_eq!(
        (tl.left, tl.top),
        (win.left, win.top),
        "TopLeft quadrant pinned to the window's TOPLEFT"
    );
    let tr = quad_rect("UI-QuestGreeting-TopRight", 128.0, 256.0);
    assert_eq!(
        (tr.right, tr.top),
        (win.right, win.top),
        "TopRight quadrant pinned to the window's TOPRIGHT"
    );
    let bl = quad_rect("UI-QuestGreeting-BotLeft", 256.0, 256.0);
    assert_eq!(
        (bl.left, bl.bottom),
        (win.left, win.bottom),
        "BotLeft quadrant pinned to the window's BOTTOMLEFT"
    );
    let br = quad_rect("UI-QuestGreeting-BotRight", 128.0, 256.0);
    assert_eq!(
        (br.right, br.bottom),
        (win.right, win.bottom),
        "BotRight quadrant pinned to the window's BOTTOMRIGHT"
    );

    let icon_rect = quads
        .iter()
        .find(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                    if p.contains("VendorGossipIcon"))
        })
        .and_then(|q| q.rect)
        .expect("vendor option icon visible after GOSSIP_SHOW");
    // The label text renders too.
    assert!(
        quads.iter().any(|q| {
            matches!(&q.content, QuadContent::Text { text: Some(t), .. }
                    if t == "Let me browse your goods.")
        }),
        "option label shows"
    );

    // Click option 1 (the icon center lies inside its row button) → SelectGossipOption(1) queues.
    let (cx, cy) = (
        (icon_rect.left + icon_rect.right) * 0.5,
        (icon_rect.bottom + icon_rect.top) * 0.5,
    );
    s.mouse_button(cx, cy, "LeftButton", true);
    s.mouse_button(cx, cy, "LeftButton", false);
    assert_eq!(s.take_gossip_selects(), vec![1]);
    assert!(!s.take_gossip_close());

    // The close button queues a close intent (the app clears state; here we drive the hide too)
    // through HideUIPanel, which vacates the left slot.
    s.run("BenillaGossipCloseButton_OnClick()").unwrap();
    assert!(s.take_gossip_close());
    assert!(!s
        .eval::<bool>("return BenillaGossipFrame:IsVisible()")
        .unwrap());
    assert!(
        s.eval::<bool>("return GetLeftFrame() == nil").unwrap(),
        "HideUIPanel vacated the left slot"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// Decision 0088 §3's deferred item, now drawn: `GossipMenu.quests` render as rows on the SAME
/// shared pool the option rows already used, quest rows filling first (ref-GossipFrame.lua
/// l.24-29/63-128 — `GossipFrameAvailableQuestsUpdate`/`ActiveQuestsUpdate` before
/// `OptionsUpdate`), option rows landing directly below them via the one static anchor chain
/// (no runtime repositioning). One active quest + one available quest + one option: rows 1/2 carry
/// the quest titles, row 3 the option, row 4+ stay hidden, and clicking a quest row queues the
/// right 1-based position on `take_gossip_quest_selects` (`benilla-ui` `script/gossip.rs`).
#[test]
fn shipped_gossip_frame_renders_quest_rows_above_options() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "ScrollTemplates.xml"); // the shared scroll kit the window rides
    load_xml(&s, "GossipFrame.xml");

    s.set_gossip(Some(GossipMenu {
        greeting: Some("A word, traveler.".into()),
        quests: vec![
            GossipQuestRow {
                title: "Report to Goldshire".into(),
                active: true,
            },
            GossipQuestRow {
                title: "A Threat Within".into(),
                active: false,
            },
        ],
        options: vec![GossipOptionView {
            label: "Let me browse your goods.".into(),
            icon_type: "vendor".into(),
            coded: false,
        }],
    }));
    s.fire_event("GOSSIP_SHOW", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // Rows 1-2 carry the quest titles (active first, matching the order the menu supplied them —
    // benilla's seam already flattens available/active into one ordered list, ui_gossip.rs), row 3
    // the option, row 4+ hidden.
    let (r1_text, r1_vis, r2_text, r2_vis, r3_text, r3_vis, r4_vis): (
        String,
        bool,
        String,
        bool,
        String,
        bool,
        bool,
    ) = s
        .eval(
            "return BenillaGossipRow1Label:GetText(), BenillaGossipRow1:IsVisible(),\n\
                        BenillaGossipRow2Label:GetText(), BenillaGossipRow2:IsVisible(),\n\
                        BenillaGossipRow3Label:GetText(), BenillaGossipRow3:IsVisible(),\n\
                        BenillaGossipRow4:IsVisible()",
        )
        .unwrap();
    assert_eq!((r1_text.as_str(), r1_vis), ("Report to Goldshire", true));
    assert_eq!((r2_text.as_str(), r2_vis), ("A Threat Within", true));
    assert_eq!(
        (r3_text.as_str(), r3_vis),
        ("Let me browse your goods.", true),
        "the option row sits below both quest rows"
    );
    assert!(!r4_vis, "no fourth row: 2 quests + 1 option");

    // The per-row icon matches active vs available (ref l.75/99), verified via the resolved quads
    // rather than the Lua-side texture string, so the assertion also proves the rows actually paint.
    s.resolve();
    let quads = s.extract();
    let has_icon = |needle: &str| {
        quads.iter().any(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p.contains(needle))
        })
    };
    assert!(has_icon("ActiveQuestIcon"), "row 1 (active) icon renders");
    assert!(
        has_icon("AvailableQuestIcon"),
        "row 2 (available) icon renders"
    );

    // The window's rect places row 3 (the option) strictly below row 2 (the last quest row) — the
    // static anchor chain, not a runtime SetPoint, closes the gap between the two sections. Found
    // by each row's own label text (rects are y-up, per `ExtractedQuad::rect`'s doc — "below" means
    // a smaller `top`), the same technique the existing test already uses for the vendor icon/label.
    let label_top = |needle: &str| {
        quads
            .iter()
            .find_map(|q| match &q.content {
                QuadContent::Text { text: Some(t), .. } if t == needle => q.rect,
                _ => None,
            })
            .unwrap_or_else(|| panic!("no text quad for {needle:?}"))
            .top
    };
    let row2_top = label_top("A Threat Within");
    let row3_top = label_top("Let me browse your goods.");
    assert!(
        row3_top < row2_top,
        "row 3 (option) sits below row 2 (last quest row): row2 top {row2_top}, row3 top {row3_top}"
    );

    // Click quest row 2 (available, "A Threat Within") → SelectGossipQuest(2) queues the seam's
    // 1-based quest-row position; the app maps it to the quest id + guid and sends
    // CMSG_QUESTGIVER_QUERY_QUEST (decision 0088 §3).
    s.run("BenillaGossipRow_OnClick(BenillaGossipRow2)")
        .unwrap();
    assert_eq!(s.take_gossip_quest_selects(), vec![2]);
    assert!(
        s.take_gossip_selects().is_empty(),
        "a quest-row click never queues SelectGossipOption"
    );
}

/// A gossip option whose label WRAPS gets a row as tall as its wrapped text (the reference's
/// `GossipResize` — `SetHeight(GetTextHeight() + 2)`, ref-GossipFrame.lua l.130-132), so the static
/// row chain (each row on the previous row's BOTTOMLEFT) still stacks them clear of one another.
/// Without the resize every row stayed the template's 16 px while its label drew 2-3 wrapped lines,
/// and the labels printed on top of each other — the director's screenshot of a four-option
/// judgement menu, every option overlapping the next.
///
/// The engine-only harness has no font atlas, so the measure round-trip is answered with a
/// deterministic 6 px/char × 14 px/line fake (the app answers from the real atlas in-game). The
/// assertions pin the LAW — row height = its label's measured height + 2, and consecutive rows share
/// an edge — not any glyph metric.
#[test]
fn shipped_gossip_rows_grow_to_their_wrapped_labels() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "ScrollTemplates.xml"); // the shared scroll kit the window rides
    load_xml(&s, "GossipFrame.xml");

    // Three long options — the shape of a real judgement/roleplay menu, every one of them wrapping
    // at the row label's 275 px width.
    let long = |t: &str| GossipOptionView {
        label: t.into(),
        icon_type: "gossip".into(),
        coded: false,
    };
    s.set_gossip(Some(GossipMenu {
        greeting: Some("Make your choice!".into()),
        quests: Vec::new(),
        options: vec![
            long(
                "I slay the man on the spot as my liege would expect me to, as he has broken the \
                  law of the land and it is my sworn duty to enforce it.",
            ),
            long(
                "I turn over the man to my liege for punishment, as the man has stolen, and I am \
                  not the arbiter of his fate.",
            ),
            long(
                "I allow the man to take enough corn to feed his family for a couple of days, \
                  encouraging him to leave the land.",
            ),
        ],
    }));
    s.fire_event("GOSSIP_SHOW", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // The host's job: measure every FontString the frame asks about. 6 px/char, wrapped at the
    // request's own wrap width, 14 px per resulting line.
    let answer_measures = |s: &mut UiScript| {
        let answers: Vec<(u32, f32, f32, u64)> = s
            .fontstrings_needing_measure()
            .into_iter()
            .map(|r| {
                let ink = r.text.chars().count() as f32 * 6.0;
                match r.wrap_width {
                    Some(w) => {
                        let lines = (ink / w).ceil().max(1.0);
                        (r.id, ink.min(w), lines * 14.0, r.key)
                    }
                    None => (r.id, ink, 14.0, r.key),
                }
            })
            .collect();
        s.set_measured_text_unwrapped(&answers);
    };
    // Frame 1: the labels are measured (their heights land for the NEXT tick — the round-trip is a
    // frame late, exactly as the tab-fit and quest-panel resizes already live with).
    answer_measures(&mut s);
    s.resolve();
    // Frame 2: the frame's own settle pass reads those measures and sizes each row.
    s.tick(0.016);
    answer_measures(&mut s);
    s.resolve();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    let row = |i: u32| -> (f32, f32, f32) {
        s.eval::<(f32, f32, f32)>(&format!(
            "return BenillaGossipRow{i}:GetTop(), BenillaGossipRow{i}:GetBottom(), \
             BenillaGossipRow{i}Label:GetStringHeight()"
        ))
        .unwrap()
    };
    let (t1, b1, h1) = row(1);
    let (t2, b2, h2) = row(2);
    let (t3, b3, h3) = row(3);
    // Every label wrapped (the fake measure gives ≥ 2 lines of 14) …
    for (i, h) in [(1, h1), (2, h2), (3, h3)] {
        assert!(h >= 28.0, "row {i}'s label wraps: measured height {h}");
    }
    // … and each row is exactly its label + the reference's 2 px.
    for (i, (top, bottom, h)) in [(1, (t1, b1, h1)), (2, (t2, b2, h2)), (3, (t3, b3, h3))] {
        assert!(
            (top - bottom - (h + 2.0)).abs() < 0.5,
            "row {i} height is its wrapped label + 2: got {}, label {h}",
            top - bottom
        );
    }
    // The chain stacks them edge to edge — no row overprints the next (rects are y-up).
    assert!(
        (t2 - b1).abs() < 0.5,
        "row 2 starts where row 1 ends: row1 bottom {b1}, row2 top {t2}"
    );
    assert!(
        (t3 - b2).abs() < 0.5,
        "row 3 starts where row 2 ends: row2 bottom {b2}, row3 top {t3}"
    );
}

/// Pin §4's headline scenario, and §2's whole justification: with both windows loaded, opening
/// gossip then merchant closes gossip purely through panel replacement (both register
/// pushable=0, so the second `ShowUIPanel` replaces the left occupant — UIParent.lua l.729-732) —
/// never a server-side CloseGossip. Merchant's own close then vacates the slot.
/// The gossip window's open/close kits — the window-sound convention (decision 0090). The real
/// GossipFrame.xml frame Scripts play igQuestListOpen on OnShow (l.445) and igQuestListClose on OnHide
/// (l.454); GOSSIP_SHOW → ShowUIPanel → Show() fires OnShow, GOSSIP_CLOSED → HideUIPanel → Hide() fires
/// OnHide. Nothing queues at load (the frame is authored hidden="true").
#[test]
fn gossip_show_hide_plays_open_and_close_kits() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "ScrollTemplates.xml"); // the shared scroll kit the window rides
    load_xml(&s, "GossipFrame.xml");

    // Hidden at load: no open sound (never transitions on startup).
    assert!(
        s.take_sounds().is_empty(),
        "no sound at load (never transitions)"
    );

    s.set_gossip(Some(GossipMenu {
        greeting: Some("Well met.".into()),
        quests: Vec::new(),
        options: Vec::new(),
    }));
    s.fire_event("GOSSIP_SHOW", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert_eq!(
        s.take_sounds(),
        vec![SoundRequest::KitName("igQuestListOpen".into())],
        "opening the gossip window plays igQuestListOpen"
    );

    s.fire_event("GOSSIP_CLOSED", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert_eq!(
        s.take_sounds(),
        vec![SoundRequest::KitName("igQuestListClose".into())],
        "closing the gossip window plays igQuestListClose"
    );
}

#[test]
fn shipped_panel_slot_replaces_gossip_with_merchant() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "ScrollTemplates.xml"); // the shared scroll kit the window rides
    load_xml(&s, "GossipFrame.xml");
    load_xml(&s, "GameTooltip.xml"); // app load order: tooltip before merchant
    load_xml(&s, "MerchantFrame.xml");

    s.set_gossip(Some(GossipMenu {
        greeting: Some("Well met.".into()),
        quests: Vec::new(),
        options: vec![GossipOptionView {
            label: "Let me browse your goods.".into(),
            icon_type: "vendor".into(),
            coded: false,
        }],
    }));
    s.fire_event("GOSSIP_SHOW", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        s.eval::<bool>("return BenillaGossipFrame:IsVisible()")
            .unwrap(),
        "gossip opened onto the (empty) left slot"
    );
    s.resolve();
    let win = frame_rect(&s.extract(), 384.0, 512.0);
    assert_eq!((win.left, win.top), (0.0, 664.0));

    // The vendor gossip option is picked; the app's SMSG_LIST_INVENTORY handler opens the
    // merchant window with no server-side gossip close (pin §2) — purely a second ShowUIPanel.
    s.set_merchant(Some(MerchantState {
        items: vec![MerchantItem {
            name: Some("Refreshing Spring Water".into()),
            texture: Some("Interface\\Icons\\INV_Drink_18".into()),
            price: 25,
            quantity: 1,
            num_available: -1,
            item_id: 159,
            stats: None,
            // Not this test's subject (the slot manager is) — a row with no template answer yet
            // carries no link (decision 1059).
            link: None,
        }],
        ..Default::default()
    }));
    s.fire_event("MERCHANT_SHOW", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // Gossip is hidden (both pushable=0 ⇒ SetLeftFrame replaced it); merchant now holds the left
    // slot at the same anchor.
    assert!(
        !s.eval::<bool>("return BenillaGossipFrame:IsVisible()")
            .unwrap(),
        "opening merchant replaced gossip at the left slot"
    );
    assert!(s
        .eval::<bool>("return BenillaMerchantFrame:IsVisible()")
        .unwrap());
    s.resolve();
    let win = frame_rect(&s.extract(), 384.0, 512.0);
    assert_eq!(
        (win.left, win.top),
        (0.0, 664.0),
        "merchant took the left slot gossip vacated"
    );
    assert!(s
        .eval::<bool>("return GetLeftFrame():GetName() == \"BenillaMerchantFrame\"")
        .unwrap());

    // Merchant's own close vacates the slot entirely.
    s.fire_event("MERCHANT_CLOSED", vec![]);
    assert!(
        s.eval::<bool>("return GetLeftFrame() == nil").unwrap(),
        "the slot is empty once the replacing window also closes"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The cross-window session-clear (decision 0095): when one NPC window displaces another at the left
/// panel slot, the displaced window's OnHide fires its `CloseX()`, queuing the client-side clear
/// intent the app drains to end that session's resource. Without it the displaced session stayed
/// "open" in its resource, and the window would not reopen until the range-guard reset it — the
/// director's gossip↔vendor lockup. This drives the exact displacement and asserts the intents.
#[test]
fn displacing_an_npc_window_ends_the_displaced_session() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "ScrollTemplates.xml"); // the shared scroll kit the window rides
    load_xml(&s, "GossipFrame.xml");
    load_xml(&s, "GameTooltip.xml"); // app load order: tooltip before merchant
    load_xml(&s, "MerchantFrame.xml");

    // Vendor open at the left slot; clear any startup/open intents.
    s.set_merchant(Some(MerchantState::default()));
    s.fire_event("MERCHANT_SHOW", vec![]);
    let _ = s.take_merchant_close();
    let _ = s.take_gossip_close();

    // Gossip opens over it → SetLeftFrame hides the merchant → merchant OnHide → CloseMerchant().
    s.set_gossip(Some(GossipMenu {
        greeting: Some("Well met.".into()),
        quests: Vec::new(),
        options: Vec::new(),
    }));
    s.fire_event("GOSSIP_SHOW", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        s.take_merchant_close(),
        "gossip displacing the vendor ends the vendor session (OnHide → CloseMerchant)"
    );

    // The reverse: the vendor back over gossip → gossip OnHide → CloseGossip().
    s.set_merchant(Some(MerchantState::default()));
    s.fire_event("MERCHANT_SHOW", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        s.take_gossip_close(),
        "the vendor displacing gossip ends the gossip session (OnHide → CloseGossip)"
    );
}

/// Pin §4's pushable path: a higher-`pushable` occupant (loot's future pushable=7 row, already
/// registered in `UiPanels.xml`) gets pushed to the center slot rather than replaced when a
/// pushable=0 frame (merchant) wants the left spot (UIParent.lua l.734-741) — the synthetic
/// registrant the pin calls for, since no loot window ships yet. A bare `CreateFrame` with a
/// distinctive 50×50 size stands in for it: it has no visual layers, but `extract` still emits
/// its own `QuadContent::Frame` quad at its resolved rect (`UiScript::extract`'s doc), which is
/// enough to prove the slot math without a real loot window.
#[test]
fn shipped_panel_slot_pushable_promotes_to_center() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml"); // app load order: tooltip before merchant
    load_xml(&s, "MerchantFrame.xml");

    // The synthetic pushable=7 loot stand-in opens first onto the empty left slot. `CreateFrame`
    // frames start shown by default (matching the real client) — every shipped panel frame is
    // authored `hidden="true"` for exactly this reason (ShowUIPanel no-ops on an already-visible
    // frame), so the stand-in hides itself first too.
    s.run(
        r#"
            local loot = CreateFrame("Frame", "BenillaLootFrame")
            loot:Hide()
            loot:SetSize(50, 50)
            ShowUIPanel(loot)
        "#,
    )
    .unwrap();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        s.eval::<bool>("return GetLeftFrame():GetName() == \"BenillaLootFrame\"")
            .unwrap(),
        "loot took the empty left slot"
    );
    s.resolve();
    let loot_left = frame_rect(&s.extract(), 50.0, 50.0);
    assert_eq!((loot_left.left, loot_left.top), (0.0, 664.0));

    // Merchant (pushable=0) then opens: loot's pushable=7 outranks it, so loot is pushed to the
    // center slot (384, -104) and merchant takes the left spot loot vacated.
    s.set_merchant(Some(MerchantState {
        items: vec![MerchantItem {
            name: Some("Refreshing Spring Water".into()),
            texture: Some("Interface\\Icons\\INV_Drink_18".into()),
            price: 25,
            quantity: 1,
            num_available: -1,
            item_id: 159,
            stats: None,
            // Not this test's subject (the slot manager is) — a row with no template answer yet
            // carries no link (decision 1059).
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
    s.resolve();
    let quads = s.extract();
    let loot_center = frame_rect(&quads, 50.0, 50.0);
    assert_eq!(
        (loot_center.left, loot_center.top),
        (384.0, 664.0),
        "loot moved to the center slot (TOPLEFT UIParent, 384, -104)"
    );
    let merchant_left = frame_rect(&quads, 384.0, 512.0);
    assert_eq!(
        (merchant_left.left, merchant_left.top),
        (0.0, 664.0),
        "merchant landed on the left slot loot vacated"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The gossip → bank handoff (decision 0604 follow-up, director-observed): vmangos sends no
/// `SMSG_GOSSIP_COMPLETE` for the gossip menu's bank option, and the slot logic alone can't close
/// the menu — the bank's `pushable = 6` promotes it to the *center* slot beside a pushable-0
/// gossip instead of replacing it. The app's `show_bank` therefore ends the gossip session itself,
/// so BANKFRAME_OPENED and GOSSIP_CLOSED fire in the *same frame* — in either order, since the
/// two feeds aren't ordered against each other. Both orders must converge on the same end state:
/// gossip hidden, the bank holding the LEFT slot (HideUIPanel's left-vacate branch slides a
/// center-parked "left-area" occupant back — UIParent.lua l.777-782).
#[test]
fn gossip_bank_option_hands_the_left_slot_to_the_bank() {
    use benilla_ui::script::{BankState, ScriptValue};

    // The bank window's own dependency chain (bank_tests::setup), plus the gossip window.
    let order_first = |bank_first: bool| {
        let mut s = UiScript::new().unwrap();
        s.set_screen_size(1024.0, 768.0);
        load_xml(&s, "Fonts.xml");
        load_xml(&s, "UiPanels.xml");
        load_xml(&s, "Cooldown.xml");
        load_xml(&s, "BagFrame.xml");
        load_xml(&s, "GameTooltip.xml");
        load_xml(&s, "MerchantFrame.xml");
        load_xml(&s, "BankFrame.xml");
        load_xml(&s, "ScrollTemplates.xml"); // the shared scroll kit the window rides
        load_xml(&s, "GossipFrame.xml");

        // The gossip menu is open on the banker (its bank option showing).
        s.set_gossip(Some(GossipMenu {
            greeting: Some("Welcome to the bank of Ironforge!".into()),
            quests: Vec::new(),
            options: vec![GossipOptionView {
                label: "I would like to check my deposit box.".into(),
                icon_type: "money".into(),
                coded: false,
            }],
        }));
        s.fire_event("GOSSIP_SHOW", vec![]);
        assert!(s
            .eval::<bool>("return GetLeftFrame():GetName() == \"BenillaGossipFrame\"")
            .unwrap());

        // The option is picked; SMSG_SHOW_BANK lands and the app both opens the bank AND clears
        // the gossip session in the same apply pass — the two events fire the same frame, in
        // whichever order the feeds run.
        s.set_money(0);
        s.set_bank(Some(BankState::default()));
        s.set_gossip(None);
        if bank_first {
            s.fire_event("BANKFRAME_OPENED", vec![ScriptValue::Str("Banker".into())]);
            s.fire_event("GOSSIP_CLOSED", vec![]);
        } else {
            s.fire_event("GOSSIP_CLOSED", vec![]);
            s.fire_event("BANKFRAME_OPENED", vec![ScriptValue::Str("Banker".into())]);
        }
        assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

        assert!(
            !s.eval::<bool>("return BenillaGossipFrame:IsVisible()")
                .unwrap(),
            "the gossip menu is gone once the vault is up (bank_first={bank_first})"
        );
        assert!(s
            .eval::<bool>("return BenillaBankFrame:IsVisible()")
            .unwrap());
        assert!(
            s.eval::<bool>("return GetLeftFrame():GetName() == \"BenillaBankFrame\"")
                .unwrap(),
            "the bank ends at the LEFT slot, not parked at center (bank_first={bank_first})"
        );
        assert!(
            s.eval::<bool>("return GetCenterFrame() == nil").unwrap(),
            "the center slot is empty again (bank_first={bank_first})"
        );
    };
    order_first(true);
    order_first(false);
}

/// A menu too tall for the parchment SCROLLS instead of spilling out of the window — the reference's
/// own answer (`GossipGreetingScrollFrame`, ref-GossipFrame.xml l.223-436), which benilla had omitted
/// on the grounds that "our short greeting needs no scrolling". Once the rows grew to their wrapped
/// text, a menu of long options ran straight off the bottom of the window and over the world (the
/// director's second screenshot).
///
/// Pins the whole mechanism: the content is measured into the scroll child, the range is the overflow,
/// the bar appears only when there IS overflow, every row is CLIPPED to the frame's rect, and
/// scrolling moves the content up under that clip. Same deterministic measure fake as the row test.
#[test]
fn an_overflowing_gossip_menu_scrolls_instead_of_spilling() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "ScrollTemplates.xml");
    load_xml(&s, "GossipFrame.xml");

    // Eight wrapping options: ~4 lines each, far past the 334 px scroll frame.
    let long = |n: usize| GossipOptionView {
        label: format!(
            "Option {n}: I slay the man on the spot as my liege would expect me to, as he has \
             broken the law of the land and it is my sworn duty to enforce it, whatever the cost."
        ),
        icon_type: "gossip".into(),
        coded: false,
    };
    s.set_gossip(Some(GossipMenu {
        greeting: Some("Make your choice!".into()),
        quests: Vec::new(),
        options: (1..=8).map(long).collect(),
    }));
    s.fire_event("GOSSIP_SHOW", vec![]);

    let answer_measures = |s: &mut UiScript| {
        let answers: Vec<(u32, f32, f32, u64)> = s
            .fontstrings_needing_measure()
            .into_iter()
            .map(|r| {
                let ink = r.text.chars().count() as f32 * 6.0;
                match r.wrap_width {
                    Some(w) => (r.id, ink.min(w), (ink / w).ceil().max(1.0) * 14.0, r.key),
                    None => (r.id, ink, 14.0, r.key),
                }
            })
            .collect();
        s.set_measured_text_unwrapped(&answers);
    };
    // Settle: measures land, rows fit, the child is sized to them, the bar re-ranges.
    for _ in 0..3 {
        answer_measures(&mut s);
        s.resolve();
        s.tick(0.016);
    }
    s.resolve();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    // The child grew past the 334 px frame, so there is a range to scroll …
    let (child_h, range): (f32, f32) = s
        .eval(
            "return BenillaGossipGreetingScrollChild:GetHeight(), \
             BenillaGossipGreetingScroll:GetVerticalScrollRange()",
        )
        .unwrap();
    assert!(
        child_h > 334.0 && range > 0.0,
        "content overflows the 334px frame: child {child_h}, range {range}"
    );
    // … and the bar is up for it (it stays hidden when everything fits — the fit case is covered by
    // `shipped_gossip_frame_drives_end_to_end`'s two-option menu).
    assert!(
        s.eval::<bool>("return BenillaGossipGreetingScrollBar:IsVisible()")
            .unwrap(),
        "the scrollbar shows once the menu overflows"
    );

    // Every row is CLIPPED to the scroll frame — the mechanism that replaces "spilling out of the
    // window". The engine clips by carrying a clip rect on the quad (decision 0112 §4/§5, applied in
    // `ui_pass`), not by shrinking its rect, so the check is on the clip each row's text quad rides
    // out with: it must be the scroll frame's own rect, and rows past the bottom must be entirely
    // outside it (nothing of them survives the clip).
    let (frame_top, frame_bottom): (f32, f32) = s
        .eval(
            "return BenillaGossipGreetingScroll:GetTop(), BenillaGossipGreetingScroll:GetBottom()",
        )
        .unwrap();
    let painted_rows =
        |s: &mut UiScript| -> Vec<(benilla_ui::layout::Rect, benilla_ui::layout::Rect)> {
            s.extract()
                .into_iter()
                .filter_map(|q| match &q.content {
                    QuadContent::Text { text: Some(t), .. } if t.starts_with("Option ") => {
                        Some((q.rect?, q.clip?))
                    }
                    _ => None,
                })
                .collect()
        };
    let rows = painted_rows(&mut s);
    assert_eq!(rows.len(), 8, "all eight option labels extract");
    for (rect, clip) in &rows {
        assert!(
            (clip.top - frame_top).abs() < 0.5 && (clip.bottom - frame_bottom).abs() < 0.5,
            "row clipped to the scroll frame [{frame_bottom}, {frame_top}], got {clip:?}"
        );
        let _ = rect;
    }
    // And the menu really is longer than the window: at least one row sits entirely below the
    // frame's bottom edge (drawn nowhere, because the clip discards it) rather than over the world.
    assert!(
        rows.iter().any(|(rect, _)| rect.top < frame_bottom),
        "the menu overflows: some row is entirely below the frame"
    );

    // Scrolling pans the content up under that clip by exactly the scroll amount. Rects are y-up
    // (`ExtractedQuad::rect`), so "up" means row 1's top EDGE VALUE grows as it slides off the top.
    let first_top =
        |s: &mut UiScript| -> f32 { s.eval::<f32>("return BenillaGossipRow1:GetTop()").unwrap() };
    let before = first_top(&mut s);
    s.run("BenillaScroll_Step(BenillaGossipGreetingScroll, 100)")
        .unwrap();
    s.resolve();
    let after = first_top(&mut s);
    assert!(
        (after - before - 100.0).abs() < 0.5,
        "scrolling 100px lifts the content 100px: {before} → {after}"
    );
    // The bar followed the scroll (SyncBar off OnVerticalScroll), and the rows are still clipped.
    assert_eq!(
        s.eval::<f32>("return BenillaGossipGreetingScrollBar:GetValue()")
            .unwrap(),
        100.0,
        "the bar seats at the scroll offset"
    );
    for (_, clip) in painted_rows(&mut s) {
        assert!(
            (clip.top - frame_top).abs() < 0.5 && (clip.bottom - frame_bottom).abs() < 0.5,
            "a scrolled row still clips to the frame, got {clip:?}"
        );
    }
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
