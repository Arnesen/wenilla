//! The shipped errors/info frame (`assets/ui/ErrorsFrame.xml` — the ref UIErrorsFrame, a real
//! `<MessageFrame>`) driven engine-only: the yellow `UI_INFO_MESSAGE` toast (the quest
//! objective-progress popup's surface), the red `UI_ERROR_MESSAGE` line, insertMode-TOP stacking,
//! and the hold+fade expiry.
//!
//! Every assertion here reads the **drawn quads**, not Lua state. It used to read
//! `UIErrorsFrameLine1:GetText()` — the hand-rolled version's three stacked FontStrings — and those
//! are gone with it: the widget draws its own message bands now, so what a player sees is the only
//! thing left to test, which is also the thing worth testing.

use benilla_ui::script::{ExtractedQuad, QuadContent, ScriptValue, UiScript};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error (the panel/quest
/// tests' loader, duplicated so this file is self-contained).
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

/// The toast lines as drawn, **top row first**, each with its colour+alpha and its band bottom.
fn toast_lines(s: &mut UiScript) -> Vec<(String, [f32; 4], f32)> {
    s.resolve();
    let mut v: Vec<(String, [f32; 4], f32)> = s
        .extract()
        .iter()
        .filter_map(|q| match (&q.content, q.rect) {
            (
                QuadContent::Text {
                    text: Some(t),
                    color: Some(c),
                    ..
                },
                Some(r),
            ) if !t.is_empty() => Some((t.clone(), *c, r.bottom)),
            _ => None,
        })
        .collect();
    v.sort_by(|a, b| b.2.total_cmp(&a.2));
    v
}

#[test]
fn info_and_error_messages_stack_hold_and_expire() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "ErrorsFrame.xml");

    // Empty at load. The frame itself is shown — a MessageFrame with nothing to say simply draws
    // nothing, where the hand-rolled version had to `Hide()` itself.
    assert!(toast_lines(&mut s).is_empty());

    // A quest progress toast: yellow (ref UIErrorsFrame.lua:12), on the top line.
    s.fire_event(
        "UI_INFO_MESSAGE",
        vec![ScriptValue::Str("Tough Wolf Meat: 2/8".into())],
    );
    assert!(s.errors().is_empty(), "info errors: {:?}", s.errors());
    let lines = toast_lines(&mut s);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].0, "Tough Wolf Meat: 2/8");
    assert_eq!(lines[0].1, [1.0, 1.0, 0.0, 1.0], "UI_INFO_MESSAGE yellow");

    // A second message lands ON TOP (insertMode="TOP"), red for UI_ERROR_MESSAGE (lua:14), pushing
    // the toast to the row below — the whole point of the attribute.
    s.fire_event(
        "UI_ERROR_MESSAGE",
        vec![ScriptValue::Str("You are too far away!".into())],
    );
    let lines = toast_lines(&mut s);
    assert_eq!(
        lines.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(),
        ["You are too far away!", "Tough Wolf Meat: 2/8"]
    );
    // 0.1 comes back as 26/255: `AddMessage` byte-quantizes every channel round-half-up
    // (`ftol(v*255 + 0.5)`), so the colour a message draws with is never quite the float handed in.
    assert_eq!(
        lines[0].1,
        [1.0, 26.0 / 255.0, 26.0 / 255.0, 1.0],
        "UI_ERROR_MESSAGE red, byte-quantized"
    );

    // Hold 5 s (the ref's displayDuration="5"), then the class ctor's 3 s ramp, then gone. The
    // phase check is per-tick, so the tick that spends the last of the hold still draws full.
    s.tick(4.0);
    assert_eq!(toast_lines(&mut s).len(), 2);
    s.tick(2.0); // the hold is spent on this tick; the ramp starts on the next
    assert_eq!(toast_lines(&mut s).len(), 2);
    s.tick(1.5); // half the ramp — still drawing, now dimmer
    let mid = toast_lines(&mut s);
    assert_eq!(mid.len(), 2);
    assert!(
        mid[0].1[3] < 1.0 && mid[0].1[3] > 0.0,
        "mid-ramp alpha: {:?}",
        mid[0].1
    );
    s.tick(2.0); // ramp done — retired, and this class frees the line rather than blanking it
    assert!(toast_lines(&mut s).is_empty());
    assert!(s.errors().is_empty(), "expiry errors: {:?}", s.errors());
}

/// **The toast must outrank an open panel window.** `UIErrorsFrame` is 512x60 at TOP (0,-122)
/// and every left-slot panel is 384-wide at TOPLEFT (0,-104), so they overlap. The reference puts
/// this frame in HIGH (`UIErrorsFrame.xml` l.4) and benilla had dropped it, leaving it in the
/// default MEDIUM alongside the panels.
///
/// That is not a tie: a panel carries nested child frames (level 1+) while the toast's content
/// belongs to the errors frame itself (level 0), and level outranks insertion in the draw key. So
/// the toast lost to any open panel — invisible in exactly the state you most want to read it. Same
/// defect as the party frame's in decision 0597, one stratum up.
#[test]
fn an_error_toast_draws_over_an_open_panel_window() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "ErrorsFrame.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "MerchantFrame.xml"); // BenillaMoney_* — QuestLogDetail's reward money row
                                       // ScrollTemplates.xml (the faux kit the list rides) + UIPanelTemplates.xml (the detail
                                       // pane's UIPanelScrollFrameTemplate). A MISSING template is a loader *warning*, so an
                                       // under-loaded list passes and then dies on the first FauxScrollFrame_Update.
    load_xml(&s, "ScrollTemplates.xml");
    load_xml(&s, "UIPanelTemplates.xml");
    load_xml(&s, "QuestLogFrame.xml");

    // A left-slot panel open, and the toast raised after it — the order that must not decide.
    s.eval::<()>("ShowUIPanel(QuestLogFrame)").unwrap();
    s.fire_event(
        "UI_ERROR_MESSAGE",
        vec![ScriptValue::Str("Out of range.".into())],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(s.eval::<bool>("return QuestLogFrame:IsVisible()").unwrap());

    s.resolve();
    let quads = s.extract();
    const STRATUM_SHIFT: u32 = 60;
    let toast = quads
        .iter()
        .filter_map(|q| match &q.content {
            QuadContent::Text { text: Some(t), .. } if t == "Out of range." => Some(q.z),
            _ => None,
        })
        .min()
        .expect("the toast text must draw");
    let panel_ceiling = quads.iter().map(|q| q.z).filter(|z| *z < toast).count();
    assert!(
        panel_ceiling > 0,
        "sanity: the panel must be drawing something at all"
    );
    // Everything the quest log draws is below the toast, and by STRATUM — not by luck within one.
    //
    // "Above" is measured against **other frames**: UIErrorsFrame's own declared `<FontString>` —
    // the child that supplies the message font and the CENTER justification — is a real region of
    // the frame and sorts above its message bands, exactly as a chat frame's does. It carries no
    // text and draws nothing, so excluding the frame's own quads is what keeps this test about the
    // thing it is named for.
    let above: Vec<&ExtractedQuad> = quads
        .iter()
        .filter(|q| q.z > toast)
        // `RaidWarningFrame` is excluded on exactly the same grounds, and it is the reference's
        // own arrangement rather than ours: it is HIGH + toplevel too (ref RaidWarning.xml:4), so
        // it raises above the toast in z — but it sits BELOW UIErrorsFrame on screen (anchored to
        // its BOTTOM at -10) and its declared `<FontString>` carries no text, so nothing of it is
        // ever painted over the toast. Excluding it keeps this test about a PANEL WINDOW drawing
        // over the toast, which is what it is named for.
        .filter(|q| {
            !matches!(
                s.quad_owner_name(q.target).as_deref(),
                Some("UIErrorsFrame") | Some("RaidWarningFrame")
            )
        })
        .collect();
    assert!(
        above.is_empty(),
        "nothing may draw over the error toast while a panel is open: {above:?}"
    );
    assert_eq!(
        toast >> STRATUM_SHIFT,
        4,
        "HIGH is stratum 4 — the ref's own for UIErrorsFrame"
    );
}
