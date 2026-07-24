//! Drives the REAL `assets/ui/ItemTextFrame.xml` through the engine — the reader window for bag
//! letters (mail-made permanent copies) and, later, books. Loads the same file chain the app does
//! (cut to the reader's dependency prefix), pushes an `ItemTextState`, fires the reference event
//! flow `ITEM_TEXT_BEGIN` → `ITEM_TEXT_READY` → `ITEM_TEXT_CLOSED`, and asserts the transcribed
//! Lua actually paints (title, the "From," creator tail, page-button visibility, the close intent).

use benilla_ui::script::{ItemTextState, UiScript};

const UI_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/ui");

const FILES: [&str; 3] = ["Fonts.xml", "UiPanels.xml", "ItemTextFrame.xml"];

fn load_ui(script: &UiScript) {
    let dir = std::path::Path::new(UI_DIR);
    let provider = |req: &str| -> Option<String> {
        let norm = req.replace('\\', "/");
        let base = norm.rsplit('/').next().unwrap_or(&norm);
        std::fs::read_to_string(dir.join(&norm))
            .or_else(|_| std::fs::read_to_string(dir.join(base)))
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
    assert!(!s
        .eval::<bool>("return BenillaItemTextFrame:IsShown()")
        .unwrap());

    s.set_item_text(Some(letter()));
    s.fire_event("ITEM_TEXT_BEGIN", vec![]);
    assert_eq!(
        s.eval::<String>("return BenillaItemTextTitleText:GetText()")
            .unwrap(),
        "Plain Letter"
    );
    s.fire_event("ITEM_TEXT_READY", vec![]);
    assert!(s
        .eval::<bool>("return BenillaItemTextFrame:IsShown()")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return BenillaItemTextPageText:GetText()")
            .unwrap(),
        "\nasd\n\nFrom,\nOne\n\n",
        "the reference creator tail (ITEM_TEXT_FROM really is comma'd)"
    );
    for hidden in [
        "BenillaItemTextCurrentPage",
        "BenillaItemTextPrevPageButton",
        "BenillaItemTextNextPageButton",
        "BenillaItemTextStatusBar",
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
            "return BenillaItemTextScrollFrameMiddle:GetLeft(), \
             BenillaItemTextScrollFrame:GetRight()",
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
    assert_eq!(
        s.eval::<String>("return BenillaItemTextPageText:GetText()")
            .unwrap(),
        "\npage one\n"
    );
    assert_eq!(
        s.eval::<String>("return BenillaItemTextCurrentPage:GetText()")
            .unwrap(),
        "1"
    );
    assert!(!s
        .eval::<bool>("return BenillaItemTextPrevPageButton:IsShown()")
        .unwrap());
    assert!(s
        .eval::<bool>("return BenillaItemTextNextPageButton:IsShown()")
        .unwrap());
    s.run("BenillaItemTextNextPageButton:Click()").unwrap();
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

    s.run("BenillaItemTextCloseButton:Click()").unwrap();
    assert!(!s
        .eval::<bool>("return BenillaItemTextFrame:IsShown()")
        .unwrap());
    assert!(s.take_item_text_close(), "OnHide queued the close intent");

    // The app's answer: session cleared + ITEM_TEXT_CLOSED (stays hidden, no errors).
    s.set_item_text(None);
    s.fire_event("ITEM_TEXT_CLOSED", vec![]);
    assert!(!s
        .eval::<bool>("return BenillaItemTextFrame:IsShown()")
        .unwrap());
    assert!(s.take_errors().is_empty());
}
