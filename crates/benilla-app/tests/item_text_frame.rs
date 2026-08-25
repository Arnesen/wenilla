//! Drives the REAL `assets/ui/ItemTextFrame.xml` through the engine — the reader window for bag
//! letters (mail-made permanent copies) and, later, books. Loads the same file chain the app does
//! (cut to the reader's dependency prefix), pushes an `ItemTextState`, fires the reference event
//! flow `ITEM_TEXT_BEGIN` → `ITEM_TEXT_READY` → `ITEM_TEXT_CLOSED`, and asserts the transcribed
//! Lua actually paints (title, the "From," creator tail, page-button visibility, the close intent).

use benilla_ui::script::{ItemTextState, UiScript};

const UI_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/ui");

/// The reader's dependency prefix, in the manifest's own order. `ScrollTemplates.xml` and
/// `UIPanelTemplates.xml` joined it with decisions 1337/1338: the page sits in a real ScrollFrame
/// now, whose template is in the second and whose `ScrollFrame_OnLoad` is in the first.
const FILES: [&str; 6] = [
    "Fonts.xml",
    "MoneyFrame.xml",
    "UiPanels.xml",
    "ScrollTemplates.xml",
    "UIPanelTemplates.xml",
    "ItemTextFrame.xml",
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

/// The page body as the reader actually DRAWS it, one string per block.
///
/// `ItemTextPageText` is a `SimpleHTML` since decisions 1337/1338, and 5875's SimpleHTML has no
/// `GetText` — its Lua table is 19 entries and none of them is a text getter (wow-re
/// `simplehtml-markup-engine.md` §5.1; later clients grew one, this one has not). So the page is
/// read the way it is seen: off the render list. A plain body is one block through the engine's
/// raw-text fallback, which is what every letter here is.
fn page_blocks(s: &UiScript) -> Vec<String> {
    use benilla_ui::script::QuadContent;
    s.extract()
        .into_iter()
        .filter_map(|q| match q.content {
            QuadContent::Text { text, .. } => text,
            _ => None,
        })
        .collect()
}

fn letter() -> ItemTextState {
    ItemTextState {
        item: "Plain Letter".into(),
        creator: Some("One".into()),
        text: "asd".into(),
        page: 1,
        has_next: false,
        material: None,
    }
}

/// The letter flow: BEGIN paints the title, READY paints the body with the reference "From," tail
/// and shows the window; a single page shows no page number and no page-turn buttons.
#[test]
fn a_letter_reads_with_the_creator_tail() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    assert!(!s.eval::<bool>("return ItemTextFrame:IsShown()").unwrap());

    s.set_item_text(Some(letter()));
    s.fire_event("ITEM_TEXT_BEGIN", vec![]);
    assert_eq!(
        s.eval::<String>("return ItemTextTitleText:GetText()")
            .unwrap(),
        "Plain Letter"
    );
    s.fire_event("ITEM_TEXT_READY", vec![]);
    assert!(s.eval::<bool>("return ItemTextFrame:IsShown()").unwrap());
    assert!(
        page_blocks(&s).contains(&"\nasd\n\nFrom,\nOne\n\n".to_string()),
        "the reference creator tail (ITEM_TEXT_FROM really is comma'd), drawn as one raw block: {:?}",
        page_blocks(&s)
    );
    for hidden in [
        "ItemTextCurrentPage",
        "ItemTextPrevPageButton",
        "ItemTextNextPageButton",
        "ItemTextStatusBar",
    ] {
        assert!(
            !s.eval::<bool>(&format!("return {hidden}:IsShown()"))
                .unwrap(),
            "{hidden} must stay hidden on a single-page letter"
        );
    }
    assert!(s.take_errors().is_empty());
}

/// The scrollbar track (the ref's black `$parentMiddle` strip) belongs in the scrollbar column,
/// right of the page — regression for the black bar over the parchment: the ref declares the
/// ARTWORK layer before BACKGROUND because `Middle` anchors to `Top` by name and anchors resolve
/// at SetPoint time; a reordered transcription silently fell back to the parent. The warning
/// check pins the tripwire that now catches any such unresolved named anchor at load.
#[test]
fn the_scrollbar_track_sits_right_of_the_page() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    let unresolved: Vec<String> = s
        .warnings()
        .into_iter()
        .filter(|w| w.contains("does not resolve"))
        .collect();
    assert!(unresolved.is_empty(), "unresolved anchors: {unresolved:#?}");

    s.set_screen_size(1024.0, 768.0);
    s.set_item_text(Some(letter()));
    s.fire_event("ITEM_TEXT_BEGIN", vec![]);
    s.fire_event("ITEM_TEXT_READY", vec![]);
    s.resolve();
    let (track_left, page_right): (f32, f32) = s
        .eval(
            "return ItemTextScrollFrameMiddle:GetLeft(), \
             ItemTextScrollFrame:GetRight()",
        )
        .unwrap();
    assert!(
        track_left >= page_right - 0.5,
        "the track ({track_left}) must not overlap the page (right edge {page_right})"
    );
    assert!(s.take_errors().is_empty());
}

/// An authorless multi-page text (a book): no "From," tail, the page number and the page-turn
/// buttons show per the reference visibility rules (page 1 with a next → Next only).
#[test]
fn a_book_page_shows_the_paging_chrome() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_item_text(Some(ItemTextState {
        item: "Lament of the Highborne".into(),
        creator: None,
        text: "page one".into(),
        page: 1,
        has_next: true,
        material: None,
    }));
    s.fire_event("ITEM_TEXT_BEGIN", vec![]);
    s.fire_event("ITEM_TEXT_READY", vec![]);
    assert!(
        page_blocks(&s).contains(&"\npage one\n".to_string()),
        "the page body drawn: {:?}",
        page_blocks(&s)
    );
    assert_eq!(
        s.eval::<String>("return ItemTextCurrentPage:GetText()")
            .unwrap(),
        "1"
    );
    assert!(!s
        .eval::<bool>("return ItemTextPrevPageButton:IsShown()")
        .unwrap());
    assert!(s
        .eval::<bool>("return ItemTextNextPageButton:IsShown()")
        .unwrap());
    s.run("ItemTextNextPageButton:Click()").unwrap();
    assert_eq!(s.take_item_text_page_turns(), vec![1]);
    assert!(s.take_errors().is_empty());
}

/// Closing: the X hides the panel, the OnHide queues the `CloseItemText` intent (the app then
/// clears the session and fires `ITEM_TEXT_CLOSED` — which hides again, harmlessly).
#[test]
fn closing_queues_the_close_intent() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_item_text(Some(letter()));
    s.fire_event("ITEM_TEXT_BEGIN", vec![]);
    s.fire_event("ITEM_TEXT_READY", vec![]);
    let _ = s.take_item_text_close();

    s.run("ItemTextCloseButton:Click()").unwrap();
    assert!(!s.eval::<bool>("return ItemTextFrame:IsShown()").unwrap());
    assert!(s.take_item_text_close(), "OnHide queued the close intent");

    // The app's answer: session cleared + ITEM_TEXT_CLOSED (stays hidden, no errors).
    s.set_item_text(None);
    s.fire_event("ITEM_TEXT_CLOSED", vec![]);
    assert!(!s.eval::<bool>("return ItemTextFrame:IsShown()").unwrap());
    assert!(s.take_errors().is_empty());
}

/// **B240's render half, on the reported page.** Goudy's plaque body (`page_text` 2676, the
/// *Alliance Military Ranks* wall plaque in Stormwind's Old Town) went through the reader and came
/// out as its own source — `<HTML><BODY><H1 align="center">…` drawn literally, and cut off with
/// "..." partway down. Both were the page being a plain FontString where the reference has a
/// `SimpleHTML` (decisions 1337/1338).
///
/// What this pins is the whole path the report exercises: the app's page feed → the reader's
/// `"\n" .. body .. "\n"` padding → the markup parse → the drawn blocks. The falsification is
/// stated at the bottom: if the parse ever regresses to the raw-text fallback, the page draws as
/// one block that still has angle brackets in it, which is exactly what was reported.
#[test]
fn the_reported_html_page_draws_as_blocks_not_as_its_own_markup() {
    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    s.set_item_text(Some(ItemTextState {
        item: "Alliance Military Ranks".into(),
        creator: None,
        text: "<HTML>\n<BODY>\n\
               <H1 align=\"center\">ALLIANCE MILITARY RANKS</H1><BR/>\n\
               <P align=\"center\">OFFICERS</P><BR/>\n\
               <P align=\"center\">Grand Marshal</P>\n\
               <P align=\"center\">Knight</P><BR/>\n\
               <P align=\"center\">ENLISTED</P><BR/>\n\
               <P align=\"center\">Private</P>\n\
               </BODY>\n</HTML>"
            .into(),
        page: 1,
        has_next: false,
        material: None,
    }));
    s.fire_event("ITEM_TEXT_BEGIN", vec![]);
    s.fire_event("ITEM_TEXT_READY", vec![]);
    assert!(s.eval::<bool>("return ItemTextFrame:IsShown()").unwrap());

    let drawn = page_blocks(&s);
    for line in [
        "ALLIANCE MILITARY RANKS",
        "OFFICERS",
        "Grand Marshal",
        "Knight",
        "ENLISTED",
        "Private",
    ] {
        assert!(
            drawn.contains(&line.to_string()),
            "{line:?} should draw as its own block; drawn: {drawn:?}"
        );
    }
    // The reported symptom, stated as the thing that must not come back.
    assert!(
        !drawn
            .iter()
            .any(|b| b.contains("<HTML>") || b.contains("<P align")),
        "the markup itself must never reach the page — that is what was photographed: {drawn:?}"
    );
    // And the second half of the same symptom: the body is no longer a height-pinned FontString,
    // so decision 1332's ellipsis seam cannot cut a long page short. No block ends in the
    // truncation marker.
    assert!(
        !drawn.iter().any(|b| b.ends_with("...")),
        "a block was truncated — the page is height-pinned again: {drawn:?}"
    );
}

/// **B288, closed at the reported symptom** (CarlG, decision 1507): the Verdant Note open from
/// the bag, then a quest giver's gossip — both frames drew at the same TOPLEFT 0,-104 anchor,
/// page text and greeting interleaved. The cause was the reader's missing `UIPanelWindows` row:
/// registered (the ref's own `{ area = "left", pushable = 0 }`, UIParent.lua l.20), the two are
/// left-slot rivals and showing one replaces the other — in BOTH orders, each displaced window's
/// OnHide ending its own session exactly as the merchant/gossip pair already does.
#[test]
fn a_quest_givers_gossip_displaces_the_open_reader() {
    use benilla_ui::script::GossipMenu;

    let mut s = UiScript::new().unwrap();
    load_ui(&s);
    // The other rival, loaded exactly as the app's manifest does (after the reader's prefix).
    {
        let dir = std::path::Path::new(UI_DIR);
        let text = std::fs::read_to_string(dir.join("GossipFrame.xml")).unwrap();
        let doc = benilla_ui::framexml::parse(&text).unwrap();
        let report = benilla_ui::loader::load(&s, &doc, &|_| None);
        assert!(
            report.errors.is_empty(),
            "GossipFrame.xml: {:?}",
            report.errors
        );
    }

    // The note is open and holding the left slot.
    s.set_item_text(Some(letter()));
    s.fire_event("ITEM_TEXT_BEGIN", vec![]);
    s.fire_event("ITEM_TEXT_READY", vec![]);
    assert!(s.eval::<bool>("return ItemTextFrame:IsShown()").unwrap());
    assert!(
        s.eval::<bool>("return GetLeftFrame():GetName() == 'ItemTextFrame'")
            .unwrap(),
        "the registered reader seats at the left slot, not a bare Show"
    );
    let _ = s.take_item_text_close();

    // The quest giver's gossip opens: one window, not two — the reported stack cannot form.
    s.set_gossip(Some(GossipMenu {
        greeting: "What can I do for you?".into(),
        quests: Vec::new(),
        options: Vec::new(),
    }));
    s.fire_event("GOSSIP_SHOW", vec![]);
    assert!(s.take_errors().is_empty());
    assert!(
        !s.eval::<bool>("return ItemTextFrame:IsShown()").unwrap(),
        "gossip replaced the note — the B288 stack (both drawn at 0,-104) cannot form"
    );
    assert!(s
        .eval::<bool>("return GossipFrame:IsShown() and GetLeftFrame():GetName() == 'GossipFrame'")
        .unwrap());
    assert!(
        s.take_item_text_close(),
        "the displaced reader's OnHide ended the read session (CloseItemText)"
    );
    // The app's answer to that close must not disturb the gossip that displaced it.
    s.set_item_text(None);
    s.fire_event("ITEM_TEXT_CLOSED", vec![]);
    assert!(s.eval::<bool>("return GossipFrame:IsShown()").unwrap());

    // The reverse order: reading the note over an open gossip ends the gossip session.
    let _ = s.take_gossip_close();
    s.set_item_text(Some(letter()));
    s.fire_event("ITEM_TEXT_BEGIN", vec![]);
    s.fire_event("ITEM_TEXT_READY", vec![]);
    assert!(s.take_errors().is_empty());
    assert!(
        s.eval::<bool>("return ItemTextFrame:IsShown() and not GossipFrame:IsShown()")
            .unwrap(),
        "the reader replaces gossip the same way"
    );
    assert!(
        s.take_gossip_close(),
        "the displaced gossip's OnHide ended the gossip session (CloseGossip)"
    );
}
