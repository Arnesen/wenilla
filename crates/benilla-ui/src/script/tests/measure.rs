//! The measure round-trip + frame→region anchors, end to end.

use super::common::script;
use crate::script::*;

/// The measure round-trip + frame→region anchors, end to end: a height-less FontString reports a
/// [`MeasureRequest`]; the host answer becomes its implicit size; and a FRAME anchored to that
/// FontString by name binds to its measured bottom in resolve's second round — the real gossip
/// structure (option rows hang off the greeting's laid-out height, ref-GossipFrame.xml l.258-261).
#[test]
fn measured_fontstring_height_feeds_frame_anchors() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local w = CreateFrame("Frame", "Win")
        w:SetPoint("TOPLEFT", 0, -100); w:SetSize(384, 512)
        local g = w:CreateFontString("Greeting", "ARTWORK")
        g:SetText("a long greeting that wraps")
        g:SetWidth(270)
        g:SetPoint("TOPLEFT", 33, -91)
        local row = CreateFrame("Button", "Row1")
        row:SetSize(300, 16)
        row:SetPoint("TOPLEFT", "Greeting", "BOTTOMLEFT", -10, -20)
    "#,
    )
    .unwrap();
    s.resolve();
    let reqs = s.fontstrings_needing_measure();
    assert_eq!(reqs.len(), 1, "one height-less FontString wants measuring");
    let r = &reqs[0];
    assert_eq!(r.wrap_width, Some(270.0));
    assert_eq!(r.text, "a long greeting that wraps");
    // Host answers: 3 wrapped lines of 16px ⇒ 48 tall.
    s.set_measured_text(&[(r.id, 250.0, 48.0, r.key)]);
    s.resolve();
    assert!(
        s.fontstrings_needing_measure().is_empty(),
        "cache key satisfied — no re-measure on a quiet frame"
    );
    let quads = s.extract();
    // Greeting: TOPLEFT of Win +(33,-91) ⇒ top 409 (win top 500), measured height 48 ⇒ bottom 361.
    let g = quads
        .iter()
        .find_map(|q| match &q.content {
            QuadContent::Text { text: Some(t), .. } if t.starts_with("a long") => q.rect,
            _ => None,
        })
        .expect("greeting rect");
    assert_eq!((g.top, g.bottom, g.left), (409.0, 361.0, 33.0));
    // Row1: TOPLEFT → Greeting BOTTOMLEFT +(-10,-20) ⇒ top 341, left 23 — bound in round 2.
    let row = quads
        .iter()
        .find_map(|q| match q.target {
            crate::order::ZTarget::Frame(_) => q.rect.filter(|r| (r.width() - 300.0).abs() < 0.1),
            _ => None,
        })
        .expect("row rect");
    assert_eq!((row.top, row.left), (341.0, 23.0));
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// A stored measure serves ONLY the current text: after `SetText` to a DIFFERENT string, the old
/// string's width must not leak through `GetWidth`/`GetStringWidth` — the whisper-header cursor
/// bug: the chat edit box ran `SetTextInsets(15 + header:GetWidth(), …)` on a type switch
/// (Say → "Tell Alice:") and its `w > 1` settle gate passed with the PREVIOUS header's measure,
/// latching the caret inside the new header. The metric read is key-checked
/// ([`crate::script::RegionData`]'s measure key): a changed string reads 0 until its own measure
/// lands, so poll-until-nonzero callers converge on the RIGHT width.
#[test]
fn a_changed_text_reads_zero_until_its_own_measure_lands() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local w = CreateFrame("Frame", "Win")
        w:SetPoint("TOPLEFT", 0, -100); w:SetSize(384, 512)
        local h = w:CreateFontString("Header", "ARTWORK")
        h:SetText("Say: ")
        h:SetPoint("LEFT", 13, 0)
    "#,
    )
    .unwrap();
    s.resolve();
    let reqs = s.fontstrings_needing_measure();
    assert_eq!(reqs.len(), 1);
    let r = reqs[0].clone();
    assert_eq!(r.text, "Say: ");
    s.set_measured_text(&[(r.id, 30.0, 16.0, r.key)]);
    assert_eq!(
        s.eval::<f64>(r#"return getglobal("Header"):GetWidth()"#)
            .unwrap(),
        30.0
    );
    // The type switch: same region, new text. The old measure must NOT serve.
    s.run(r#"getglobal("Header"):SetText("Tell Alice: ")"#)
        .unwrap();
    assert_eq!(
        s.eval::<f64>(r#"return getglobal("Header"):GetWidth()"#)
            .unwrap(),
        0.0,
        "a stale measure must not serve for changed text"
    );
    // The round-trip re-measures the new string; the true width serves.
    s.resolve();
    let reqs = s.fontstrings_needing_measure();
    assert_eq!(reqs.len(), 1, "the changed text wants re-measuring");
    let r2 = reqs[0].clone();
    assert_eq!(r2.text, "Tell Alice: ");
    assert_ne!(r2.key, r.key, "the key tracks the text");
    s.set_measured_text(&[(r2.id, 72.0, 16.0, r2.key)]);
    assert_eq!(
        s.eval::<f64>(r#"return getglobal("Header"):GetWidth()"#)
            .unwrap(),
        72.0
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// A zero-WIDTH FontString with an explicit height auto-sizes its width to the measured line —
/// the reference label idiom (`<Size x="0" y="16"/>` anchored TOPRIGHT→TOPLEFT: MailFrame's
/// "From:"/"Subject:" labels end at the anchor and grow leftward, and the value string anchored
/// LEFT→label RIGHT starts past them, never overlapping). Gating the measure on height alone
/// left these rects zero-width — "From" and the sender name painted on top of each other.
#[test]
fn zero_width_fontstring_autosizes_to_its_line() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local w = CreateFrame("Frame", "Win")
        w:SetPoint("TOPLEFT", 0, -100); w:SetSize(384, 512)
        local label = w:CreateFontString("FromLabel", "ARTWORK")
        label:SetText("From:")
        label:SetSize(0, 16)
        label:SetPoint("TOPRIGHT", "Win", "TOPLEFT", 114, -45)
        local value = w:CreateFontString("FromValue", "ARTWORK")
        value:SetText("Thrall")
        value:SetSize(110, 0)
        value:SetPoint("LEFT", "FromLabel", "RIGHT", 5, 0)
    "#,
    )
    .unwrap();
    s.resolve();
    let reqs = s.fontstrings_needing_measure();
    let label_req = reqs
        .iter()
        .find(|r| r.text == "From:")
        .expect("the zero-width label asks for a measure");
    assert_eq!(
        label_req.wrap_width, None,
        "width 0 = unwrapped single line"
    );
    let value_req = reqs.iter().find(|r| r.text == "Thrall").expect("value");
    let answers = [
        (label_req.id, 40.0, 16.0, label_req.key),
        (value_req.id, 45.0, 16.0, value_req.key),
    ];
    s.set_measured_text(&answers);
    s.resolve();
    // Label: right edge pinned at Win left +114, measured width 40 ⇒ [74, 114].
    let (l_left, l_right, l_w): (f32, f32, f32) = s
        .eval("return FromLabel:GetLeft(), FromLabel:GetRight(), FromLabel:GetStringWidth()")
        .unwrap();
    assert_eq!((l_left, l_right, l_w), (74.0, 114.0, 40.0));
    // Value: starts 5 past the label's real right edge — no overlap.
    let v_left: f32 = s.eval("return FromValue:GetLeft()").unwrap();
    assert_eq!(v_left, 119.0);
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}
