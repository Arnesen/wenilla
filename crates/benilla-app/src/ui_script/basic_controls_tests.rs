//! The shipped `assets/ui/BasicControls.xml` — the reference's second file, driven the way an
//! addon drives it.
//!
//! Nothing benilla ships calls `message`, `TEXT` or `_ERRORMESSAGE`; their only consumers are
//! third-party addons (26 call `TEXT`, ~10 genuinely call `message`, two replace `_ERRORMESSAGE`).
//! So every test here enters from Lua the way those addons do.

use benilla_ui::script::UiScript;

use super::test_ui::load_ui as load_xml;

/// Fonts then BasicControls — the manifest's own order, and the whole dependency this file has.
fn basic_controls() -> UiScript {
    let mut s = UiScript::new().unwrap();
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "BasicControls.xml");
    s.set_screen_size(1024.0, 768.0);
    s.resolve();
    s
}

/// **`message("...")` puts its text in the dialog and shows it** — the whole contract, and the
/// reason the frame has to exist rather than the function being a stub that prints somewhere.
#[test]
fn message_shows_the_script_errors_dialog_carrying_its_text() {
    let s = basic_controls();
    assert!(
        !s.eval::<bool>("return ScriptErrors:IsVisible()").unwrap(),
        "the dialog starts hidden (DialogBoxFrame's `hidden=true`)"
    );

    s.run(r#"message("Validation error: [nil]")"#).unwrap();
    assert!(s.errors().is_empty(), "{:?}", s.errors());
    assert!(s.eval::<bool>("return ScriptErrors:IsVisible()").unwrap());
    assert_eq!(
        s.eval::<String>("return ScriptErrors_Message:GetText()")
            .unwrap(),
        "Validation error: [nil]"
    );

    // The ref guard: a second message while the dialog is up does NOT overwrite the first.
    s.run(r#"message("second")"#).unwrap();
    assert_eq!(
        s.eval::<String>("return ScriptErrors_Message:GetText()")
            .unwrap(),
        "Validation error: [nil]",
        "`if not ScriptErrors:IsVisible()` — the first error is the one you keep"
    );

    // And OKAY hides it, which is the only way it closes.
    s.run("ScriptErrorsButton:Click()").unwrap();
    assert!(!s.eval::<bool>("return ScriptErrors:IsVisible()").unwrap());
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// `_ERRORMESSAGE` is the reference's own name for the body, it **returns its argument**, and it
/// is replaceable — which is the entire point for the two corpus addons that replace it.
#[test]
fn error_message_returns_its_argument_and_can_be_replaced() {
    let s = basic_controls();
    assert_eq!(
        s.eval::<String>(r#"return _ERRORMESSAGE("boom")"#).unwrap(),
        "boom",
        "the ref returns `message` — a caller may chain on it"
    );

    // ImprovedErrorFrame's shape: swap the global, then `message` must route through the swap.
    s.run(
        r#"Captured = nil
           _ERRORMESSAGE = function(m) Captured = m return m end
           message("routed")"#,
    )
    .unwrap();
    assert_eq!(s.eval::<String>("return Captured").unwrap(), "routed");
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// The reference's closing line is transcribed since decision 1305: BasicControls installs
/// `seterrorhandler(_ERRORMESSAGE)`, so a script error pops the dialog like the real client's —
/// and the host channel stays sighted because the ENGINE records every caught error before the
/// dispatch ever runs (that dual channel is what dissolved the old divergence, whose guard this
/// test used to be).
#[test]
fn error_message_is_the_installed_handler_and_the_host_channel_stays_sighted() {
    let mut s = basic_controls();
    assert!(
        s.eval::<bool>("return geterrorhandler() == _ERRORMESSAGE")
            .unwrap(),
        "the reference's own default handler is installed (1305)"
    );
    // An addon's pcall wrapper calls geterrorhandler()(msg) — with _ERRORMESSAGE installed that
    // pops the dialog, exactly as it does in the real client.
    s.run(r#"geterrorhandler()("reported")"#).unwrap();
    assert!(
        s.eval::<bool>("return ScriptErrors:IsVisible()").unwrap(),
        "the dialog is the report now"
    );
    assert_eq!(
        s.eval::<String>("return ScriptErrors_Message:GetText()")
            .unwrap(),
        "reported"
    );
    // And an ENGINE-caught error still reaches `UiScript::errors()` — the instruments' channel —
    // with the dispatch adding the dialog on top, not instead.
    s.run("BrokenProbe = CreateFrame('Frame') BrokenProbe:RegisterEvent('B271_PROBE') BrokenProbe:SetScript('OnEvent', function() error('fault') end)")
        .unwrap();
    s.fire_event("B271_PROBE", vec![]);
    assert!(
        s.errors().iter().any(|e| e.contains("fault")),
        "engine-caught errors stay on the host channel: {:?}",
        s.errors()
    );
    s.dispatch_script_errors_to_handler();
    assert!(
        s.eval::<bool>("return ScriptErrors:IsVisible()").unwrap(),
        "and the dispatch keeps the dialog up for the player"
    );
}

/// `TEXT` is the identity function, and 26 corpus addons call it because shipped FrameXML does.
#[test]
fn text_is_the_identity_function_the_corpus_expects() {
    let s = basic_controls();
    assert_eq!(
        s.eval::<String>(r#"return TEXT("Level")"#).unwrap(),
        "Level"
    );
    assert_eq!(s.eval::<i64>("return TEXT(7)").unwrap(), 7);
}

/// The dialog kit an addon inherits from directly: `DialogBoxFrame` plus the three named textures.
/// The OKAY button must hide *the caller's* frame, and `$parent` must resolve against the caller's
/// name — `MyDialogButton`, never `DialogBoxFrameButton`.
#[test]
fn a_dialog_box_from_the_template_names_and_closes_the_callers_frame() {
    let s = basic_controls();
    s.run(
        r#"MyDialog = CreateFrame("Frame", "MyDialog", UIParent, "DialogBoxFrame")
           MyDialog:SetWidth(384) MyDialog:SetHeight(128)
           MyDialog:Show()"#,
    )
    .unwrap();
    assert!(s.errors().is_empty(), "{:?}", s.errors());
    assert!(
        s.eval::<bool>(r#"return getglobal("MyDialogButton") ~= nil"#)
            .unwrap(),
        "the caller's name won through $parent"
    );
    assert!(
        s.eval::<bool>(r#"return getglobal("DialogBoxFrameButton") == nil"#)
            .unwrap(),
        "and the template's own name published nothing"
    );
    assert_eq!(
        s.eval::<String>(r#"return getglobal("MyDialogButton"):GetText()"#)
            .unwrap(),
        "OKAY"
    );

    s.run("MyDialogButton:Click()").unwrap();
    assert!(
        !s.eval::<bool>("return MyDialog:IsVisible()").unwrap(),
        "`this:GetParent():Hide()` closed the caller's frame, not the template's"
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}
