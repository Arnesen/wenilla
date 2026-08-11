//! The shipped errors/info frame (`assets/ui/ErrorsFrame.xml` — the ref UIErrorsFrame) driven
//! engine-only: the yellow `UI_INFO_MESSAGE` toast (the quest objective-progress popup's surface),
//! the red `UI_ERROR_MESSAGE` line, insertMode-TOP stacking, and the hold+fade expiry.

use benilla_ui::script::{ScriptValue, UiScript};

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

#[test]
fn info_and_error_messages_stack_hold_and_expire() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "ErrorsFrame.xml");

    // Empty at load.
    assert!(!s.eval::<bool>("return UIErrorsFrame:IsVisible()").unwrap());

    // A quest progress toast: yellow (ref UIErrorsFrame.lua:12), on the top line.
    s.fire_event(
        "UI_INFO_MESSAGE",
        vec![ScriptValue::Str("Tough Wolf Meat: 2/8".into())],
    );
    assert!(s.errors().is_empty(), "info errors: {:?}", s.errors());
    assert!(s.eval::<bool>("return UIErrorsFrame:IsVisible()").unwrap());
    assert_eq!(
        s.eval::<String>("return UIErrorsFrameLine1:GetText()")
            .unwrap(),
        "Tough Wolf Meat: 2/8"
    );
    assert!(s
        .eval::<bool>(
            "local m = UIErrorsFrame.messages[1] return m.r == 1.0 and m.g == 1.0 and m.b == 0.0"
        )
        .unwrap());

    // A second message lands ON TOP (insertMode TOP), red for UI_ERROR_MESSAGE (lua:14),
    // pushing the toast to line 2.
    s.fire_event(
        "UI_ERROR_MESSAGE",
        vec![ScriptValue::Str("You are too far away!".into())],
    );
    assert_eq!(
        s.eval::<String>("return UIErrorsFrameLine1:GetText()")
            .unwrap(),
        "You are too far away!"
    );
    assert_eq!(
        s.eval::<String>("return UIErrorsFrameLine2:GetText()")
            .unwrap(),
        "Tough Wolf Meat: 2/8"
    );
    assert!(s
        .eval::<bool>(
            "local m = UIErrorsFrame.messages[1] return m.r == 1.0 and m.g == 0.1 and m.b == 0.1"
        )
        .unwrap());

    // Hold 5 s (ref displayDuration), then the fade window, then gone — the frame hides once the
    // last message expires.
    s.tick(4.0);
    assert!(s.eval::<bool>("return UIErrorsFrame:IsVisible()").unwrap());
    s.tick(2.5); // both past 5 s hold + 1 s fade
    assert!(!s.eval::<bool>("return UIErrorsFrame:IsVisible()").unwrap());
    assert!(s.errors().is_empty(), "expiry errors: {:?}", s.errors());
}

/// **The toast must outrank an open panel window.** `UIErrorsFrame` is 512x60 at TOP (0,-122)
/// and every left-slot panel is 384-wide at TOPLEFT (0,-104), so they overlap. The reference puts
/// this frame in HIGH (`UIErrorsFrame.xml` l.4) and benilla had dropped it, leaving it in the
/// default MEDIUM alongside the panels.
///
/// That is not a tie: the toast's content is flat FontString *regions* on the frame itself (level
/// 0), while a panel carries nested child frames (level 1+), and level outranks insertion in the
/// draw key. So the toast lost to any open panel — invisible in exactly the state you most want to
/// read it. Same defect as the party frame's in decision 0597, one stratum up.
#[test]
fn an_error_toast_draws_over_an_open_panel_window() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "ErrorsFrame.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "MerchantFrame.xml"); // BenillaMoney_* — QuestLogDetail's reward money row
    load_xml(&s, "QuestLogFrame.xml");

    // A left-slot panel open, and the toast raised after it — the order that must not decide.
    s.eval::<()>("ShowUIPanel(QuestLogFrame)").unwrap();
    s.fire_event(
        "UI_ERROR_MESSAGE",
        vec![ScriptValue::Str("Out of range.".into())],
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(s.eval::<bool>("return UIErrorsFrame:IsVisible()").unwrap());
    assert!(s.eval::<bool>("return QuestLogFrame:IsVisible()").unwrap());

    s.resolve();
    let quads = s.extract();
    const STRATUM_SHIFT: u32 = 60;
    let toast = quads
        .iter()
        .filter_map(|q| match &q.content {
            benilla_ui::script::QuadContent::Text { text: Some(t), .. } if t == "Out of range." => {
                Some(q.z)
            }
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
    let above_toast = quads.iter().filter(|q| q.z > toast).count();
    assert_eq!(
        above_toast, 0,
        "nothing may draw over the error toast while a panel is open"
    );
    assert_eq!(
        toast >> STRATUM_SHIFT,
        4,
        "HIGH is stratum 4 — the ref's own for UIErrorsFrame"
    );
}
