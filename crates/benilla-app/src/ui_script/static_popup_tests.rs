//! The shared StaticPopup engine (decision 0308 §3 — the ref's registry + Show + OnUpdate
//! machinery, transcribed in UiPanels.xml): the countdown/StartDelay/cancels/ESC laws the death
//! arc's dialogs ride. Entries here are inline test dialogs — the real entries (DELETE_ITEM,
//! ABANDON_QUEST, the death family) are covered by their features' own tests.

use benilla_ui::script::UiScript;

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

fn setup() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    s
}

/// timeout: the dialog counts down and expires into OnCancel(data, "timeout") + hide
/// (ref StaticPopup_OnUpdate l.1713-1726).
#[test]
fn timeout_expires_into_a_timeout_cancel() {
    let mut s = setup();
    s.run(
        r#"reason = "unset"
           StaticPopupDialogs["TEST_TIMEOUT"] = {
               text = "expiring", button1 = "OK", timeout = 2,
               OnCancel = function(data, r) reason = r or "none" end,
           }
           StaticPopup_Show("TEST_TIMEOUT")"#,
    )
    .unwrap();
    assert!(s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap());
    s.tick(1.0);
    assert!(
        s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "still counting"
    );
    s.tick(1.5);
    assert!(
        !s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "expiry hides the dialog"
    );
    assert_eq!(
        s.eval::<String>("return reason").unwrap(),
        "timeout",
        "expiry runs OnCancel with reason 'timeout'"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// StartDelay: button1 is disabled while the delay counts down through delayText, then the real
/// text swaps in (with the stashed text_arg1) and the button enables (ref l.1684-1690 +
/// l.1764-1776). RECOVER_CORPSE is a real which on the delay-text list, so the countdown text
/// itself is exercised too.
#[test]
fn start_delay_gates_button1_then_swaps_the_text_in() {
    let mut s = setup();
    s.run(
        r#"StaticPopupDialogs["RECOVER_CORPSE"] = {
               StartDelay = function() return 2 end,
               delayText = "%d %s until resurrection",
               text = "Resurrect now?",
               button1 = "Accept", timeout = 0, whileDead = 1,
           }
           StaticPopup_Show("RECOVER_CORPSE")"#,
    )
    .unwrap();
    assert!(s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap());
    assert!(
        !s.eval::<bool>("return StaticPopup1Button1:IsEnabled() ~= 0")
            .unwrap(),
        "button1 starts disabled under StartDelay"
    );
    s.tick(0.5);
    assert_eq!(
        s.eval::<String>("return StaticPopup1Text:GetText()")
            .unwrap(),
        "2 seconds until resurrection",
        "the delayText countdown renders (ceil of 1.5s)"
    );
    s.tick(1.6);
    assert_eq!(
        s.eval::<String>("return StaticPopup1Text:GetText()")
            .unwrap(),
        "Resurrect now?",
        "delay expiry swaps the real text in"
    );
    assert!(
        s.eval::<bool>("return StaticPopup1Button1:IsEnabled() ~= 0")
            .unwrap(),
        "delay expiry enables button1"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// cancels: showing X hides a visible X.cancels with an "override" cancel (ref l.1470-1485) —
/// the death arc's RESURRECT-cancels-DEATH chain.
#[test]
fn showing_a_dialog_cancels_its_named_victim_with_override() {
    let s = setup();
    s.run(
        r#"victim_reason = "unset"
           StaticPopupDialogs["TEST_VICTIM"] = {
               text = "victim", button1 = "OK", timeout = 0,
               OnCancel = function(data, r) victim_reason = r or "none" end,
           }
           StaticPopupDialogs["TEST_CANCELLER"] = {
               text = "canceller", button1 = "OK", timeout = 0,
               cancels = "TEST_VICTIM",
           }
           StaticPopup_Show("TEST_VICTIM")
           StaticPopup_Show("TEST_CANCELLER")"#,
    )
    .unwrap();
    assert_eq!(
        s.eval::<String>("return victim_reason").unwrap(),
        "override",
        "the victim's OnCancel ran with reason 'override'"
    );
    assert_eq!(
        s.eval::<String>("return StaticPopup1.which").unwrap(),
        "TEST_CANCELLER",
        "the canceller owns the (single) dialog instance now"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// ESC only closes hideOnEscape dialogs (ref StaticPopup_EscapePressed l.1879-1893): a
/// non-escapable entry — the DEATH release popup's law — survives ToggleGameMenu.
#[test]
fn escape_skips_dialogs_without_hide_on_escape() {
    let s = setup();
    s.run(
        r#"StaticPopupDialogs["TEST_STICKY"] = {
               text = "cannot escape me", button1 = "OK", timeout = 0, whileDead = 1,
           }
           StaticPopup_Show("TEST_STICKY")"#,
    )
    .unwrap();
    s.run("ToggleGameMenu()").unwrap();
    assert!(
        s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "a non-hideOnEscape dialog ignores ESC (the DEATH popup law)"
    );
    // The same entry marked escapable closes.
    s.run(
        r#"StaticPopup_Hide("TEST_STICKY")
           StaticPopupDialogs["TEST_STICKY"].hideOnEscape = 1
           StaticPopup_Show("TEST_STICKY")"#,
    )
    .unwrap();
    s.run("ToggleGameMenu()").unwrap();
    assert!(
        !s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "the escapable variant closes"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The DEATH-family per-tick countdown text (ref l.1729-1762): a DEATH entry whose OnShow seeds
/// `timeleft` re-renders "%d %s until release" every tick, minutes above 60 s.
#[test]
fn the_death_countdown_text_rerenders_each_tick() {
    let mut s = setup();
    s.run(
        r#"StaticPopupDialogs["DEATH"] = {
               text = "%d %s until release",
               button1 = "Release Spirit",
               OnShow = function()
                   this.timeleft = 90
               end,
               timeout = 0, whileDead = 1,
           }
           StaticPopup_Show("DEATH")"#,
    )
    .unwrap();
    s.tick(0.1);
    assert_eq!(
        s.eval::<String>("return StaticPopup1Text:GetText()")
            .unwrap(),
        "2 minutes until release",
        "above 60s renders ceil-minutes"
    );
    s.tick(31.0);
    assert_eq!(
        s.eval::<String>("return StaticPopup1Text:GetText()")
            .unwrap(),
        "59 seconds until release",
        "below 60s renders seconds"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
