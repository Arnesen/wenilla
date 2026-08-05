//! The shipped `assets/ui/KeyBindingsFrame.xml` — the era-shaped, 1.12-skinned Key Bindings
//! window over the engine's binding table (decision 0997; the provenance block in the XML).
//!
//! What these guard: the file loads clean inside its real neighbourhood; the game menu's Key
//! Bindings button is the door in and the window's OnHide brings the menu back (both
//! references' law); the sidebar shows exactly the honest tree's non-empty categories in 1.12
//! `Bindings.xml` order; the rows read the live table (defaults byte-real from the client's own
//! `bindings-cache.wtf`); the capture flow — select a capsule → the host arm arms → the
//! canonical chord lands through `KeyBindingFrame_OnHostKey` — binds live, steals with the red
//! 1.12 message only when the victim goes bare, refuses the wheel on press+release commands,
//! and restores the old key on refusal; Unbind/Okay/Cancel/Reset and the character-specific
//! checkbox run the 1.12 set model (Save queues the host persist; Cancel/ESC reverts through
//! `LoadBindings(GetCurrentBindingSet())`; the ESC ladder rung is that exact Cancel).
//!
//! Labels here are the RAW tokens (`BINDING_HEADER_MOVEMENT`, `BUTTON3`): the harness loads no
//! GlobalStrings, exercising the window's `getglobal(...) or raw` fallback — the app's VM
//! executes the real 1.12 GlobalStrings.lua at boot, which turns them into "Movement Keys" /
//! "Middle Mouse". One test seeds a handful of the real strings to pin that path too.

use benilla_ui::script::keybind::{KeybindCommand, KeybindRequest};
use benilla_ui::script::UiScript;

use crate::bindings::commands::SPECS;

/// The window's real neighbourhood, in the manifest's own order, with the registry seeded the
/// way the app seeds it (`crate::bindings::seed_bindings` — registration before any show).
fn harness() -> UiScript {
    let mut s = UiScript::new().unwrap();
    let cmds: Vec<KeybindCommand> = SPECS
        .iter()
        .map(|spec| KeybindCommand {
            name: spec.name,
            category: spec.category,
            run_on_up: spec.run_on_up(),
            default1: spec.d1,
            default2: spec.d2,
        })
        .collect();
    s.register_bindings(&cmds);
    s.set_screen_size(1024.0, 768.0);
    for file in [
        "Fonts.xml",
        "UiPanels.xml",
        "GameTooltip.xml",
        "ScrollTemplates.xml",
        "KeyBindingsFrame.xml",
        "GameMenuFrame.xml",
    ] {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets/ui")
                .join(file),
        )
        .unwrap();
        let doc = benilla_ui::framexml::parse(&text).unwrap();
        let report = benilla_ui::loader::load(&s, &doc, &|_| None);
        assert!(
            report.errors.is_empty(),
            "{file}: loader errors: {:?}",
            report.errors
        );
        if file == "KeyBindingsFrame.xml" {
            assert!(
                report.warnings.is_empty(),
                "{file}: loader warnings (dropped subtrees?): {:?}",
                report.warnings
            );
        }
    }
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    s
}

fn shown(s: &mut UiScript) {
    s.run("ShowUIPanel(KeyBindingFrame)").unwrap();
    assert!(s.errors().is_empty(), "on show: {:?}", s.errors());
}

#[test]
fn the_menu_button_is_the_door_in_and_the_window_hands_the_menu_back() {
    let s = harness();
    s.run("ShowUIPanel(GameMenuFrame)").unwrap();
    s.run("GameMenuButtonKeybindings:Click()").unwrap();
    assert!(
        s.eval::<bool>("return KeyBindingFrame:IsVisible()")
            .unwrap(),
        "the window opened"
    );
    assert!(
        !s.eval::<bool>("return GameMenuFrame:IsVisible()").unwrap(),
        "…and the menu went away first"
    );
    // Cancel closes AND reopens the menu — both references' OnHide.
    s.run("KeyBindingFrameCancelButton:Click()").unwrap();
    assert!(!s
        .eval::<bool>("return KeyBindingFrame:IsVisible()")
        .unwrap());
    assert!(
        s.eval::<bool>("return GameMenuFrame:IsVisible()").unwrap(),
        "closing the window brings the game menu back"
    );
}

#[test]
fn the_sidebar_is_the_honest_tree_in_bindings_xml_order() {
    let mut s = harness();
    shown(&mut s);
    // The registry's category tokens, first-appearance order — exactly 1.12's file order.
    let mut expected: Vec<&str> = Vec::new();
    for spec in SPECS {
        if !expected.contains(&spec.category) {
            expected.push(spec.category);
        }
    }
    for (i, token) in expected.iter().enumerate() {
        let text = s
            .eval::<String>(&format!(
                "return KeyBindingFrameCategory{}:GetText()",
                i + 1
            ))
            .unwrap();
        assert_eq!(&text, token, "category {} (raw-token fallback)", i + 1);
    }
    assert!(
        !s.eval::<bool>(&format!(
            "return KeyBindingFrameCategory{}:IsVisible()",
            expected.len() + 1
        ))
        .unwrap(),
        "no spare category rows show"
    );
    // Movement is selected by default and wears the locked-gold wash.
    assert!(s
        .eval::<bool>("return KeyBindingFrameCategory1Bg:IsVisible()")
        .unwrap());
    // Row 1 is MOVEANDSTEER with its byte-real default (raw token without GlobalStrings).
    assert_eq!(
        s.eval::<String>("return KeyBindingFrameBinding1Description:GetText()")
            .unwrap(),
        "MOVEANDSTEER"
    );
    assert_eq!(
        s.eval::<String>("return KeyBindingFrameBinding1Key1ButtonText:GetText()")
            .unwrap(),
        "BUTTON3"
    );
    // With the real strings present (the app's GlobalStrings), the same row reads 1.12's text.
    s.run(
        r#"BINDING_HEADER_MOVEMENT = "Movement Keys"
             BINDING_NAME_MOVEANDSTEER = "Move and Steer"
             KEY_BUTTON3 = "Middle Mouse""#,
    )
    .unwrap();
    s.run("KeyBindings_LoadCategories(); KeyBindingFrame_Update()")
        .unwrap();
    assert_eq!(
        s.eval::<String>("return KeyBindingFrameCategory1:GetText()")
            .unwrap(),
        "Movement Keys"
    );
    assert_eq!(
        s.eval::<String>("return KeyBindingFrameBinding1Description:GetText()")
            .unwrap(),
        "Move and Steer"
    );
    assert_eq!(
        s.eval::<String>("return KeyBindingFrameBinding1Key1ButtonText:GetText()")
            .unwrap(),
        "Middle Mouse"
    );
}

#[test]
fn the_capture_flow_binds_steals_and_refuses_like_112() {
    let mut s = harness();
    shown(&mut s);
    // Row 2 is MOVEFORWARD (W, UP). Selecting its Key 1 capsule arms the host seam.
    assert!(!s.bind_capture_armed());
    s.run("KeyBindingFrameBinding2Key1Button:Click()").unwrap();
    assert!(
        s.bind_capture_armed(),
        "a selected capsule arms the capture"
    );
    assert!(
        s.eval::<bool>("return KeyBindingFrameUnbindButton:IsEnabled() ~= nil and KeyBindingFrameUnbindButton:IsEnabled() ~= 0")
            .unwrap(),
        "Unbind arms with the selection"
    );
    // The host hands back a canonical chord: F binds into slot 1, W's old seat; UP survives
    // in slot 2; the capture disarms; the table is LIVE (GetBindingAction).
    s.run(r#"KeyBindingFrame_OnHostKey("F")"#).unwrap();
    assert!(!s.bind_capture_armed(), "a completed bind disarms");
    assert!(s
        .eval::<bool>(
            r#"local k1, k2 = GetBindingKey("MOVEFORWARD"); return k1 == "F" and k2 == "UP""#
        )
        .unwrap());
    assert_eq!(
        s.eval::<String>(r#"return GetBindingAction("W")"#).unwrap(),
        "",
        "the old key is free"
    );
    assert_eq!(
        s.eval::<String>("return KeyBindingFrameOutputText:GetText()")
            .unwrap(),
        "Key Bound Successfully"
    );
    // Stealing the LAST key of another command names the victim in red (1.12's
    // KEY_UNBOUND_ERROR). T is ATTACKTARGET's only key.
    s.run("KeyBindingFrameBinding2Key2Button:Click()").unwrap();
    s.run(r#"KeyBindingFrame_OnHostKey("T")"#).unwrap();
    assert_eq!(
        s.eval::<String>(r#"return GetBindingAction("T")"#).unwrap(),
        "MOVEFORWARD"
    );
    assert!(
        s.eval::<String>("return KeyBindingFrameOutputText:GetText()")
            .unwrap()
            .contains("ATTACKTARGET"),
        "the newly-bare victim is named"
    );
    // The wheel refusal: MOVEFORWARD has press+release state — SetBinding refuses the wheel
    // and the slot's old key is restored (1.12's KeyBindingFrame_SetBinding).
    s.run("KeyBindingFrameBinding2Key1Button:Click()").unwrap();
    s.run(r#"KeyBindingFrame_OnHostKey("MOUSEWHEELUP")"#)
        .unwrap();
    assert!(
        s.eval::<bool>(r#"local k1 = GetBindingKey("MOVEFORWARD"); return k1 == "F""#)
            .unwrap(),
        "the refused slot restored its key"
    );
    assert_eq!(
        s.eval::<String>("return KeyBindingFrameOutputText:GetText()")
            .unwrap(),
        "Can't bind mousewheel to actions with up and down states"
    );
    // A right-click on the armed capsule deselects without binding.
    s.run("KeyBindingFrameBinding2Key1Button:Click()").unwrap();
    assert!(s.bind_capture_armed());
    s.run(r#"KeyBindingFrameBinding2Key1Button:Click("RightButton")"#)
        .unwrap();
    assert!(!s.bind_capture_armed(), "right-click deselects");
}

#[test]
fn unbind_okay_cancel_and_reset_run_the_112_set_model() {
    let mut s = harness();
    shown(&mut s);
    // Unbind: select JUMP's Key 1 (row 8), unbind — both defaults gone from slot 1, slot 2
    // survives (the 1.12 slot dance).
    assert_eq!(
        s.eval::<String>("return KeyBindingFrameBinding8Description:GetText()")
            .unwrap(),
        "JUMP"
    );
    s.run("KeyBindingFrameBinding8Key1Button:Click()").unwrap();
    s.run("KeyBindingFrameUnbindButton:Click()").unwrap();
    assert!(s
        .eval::<bool>(
            r#"local k1, k2 = GetBindingKey("JUMP"); return k1 == "NUMPAD0" and k2 == nil"#
        )
        .unwrap());
    // Okay persists the account set: the host request queue carries Save(1), window closes.
    s.run("KeyBindingFrameOkayButton:Click()").unwrap();
    assert_eq!(s.take_keybind_requests(), vec![KeybindRequest::Save(1)]);
    assert!(!s
        .eval::<bool>("return KeyBindingFrame:IsVisible()")
        .unwrap());
    // Reopen; rebind something and CANCEL — the live table reverts to the saved set.
    shown(&mut s);
    s.run("KeyBindingFrameBinding8Key1Button:Click()").unwrap();
    s.run(r#"KeyBindingFrame_OnHostKey("G")"#).unwrap();
    assert_eq!(
        s.eval::<String>(r#"return GetBindingAction("G")"#).unwrap(),
        "JUMP"
    );
    s.run("KeyBindingFrameCancelButton:Click()").unwrap();
    assert_eq!(
        s.eval::<String>(r#"return GetBindingAction("G")"#).unwrap(),
        "",
        "Cancel reverted the unsaved bind"
    );
    assert!(
        s.take_keybind_requests().is_empty(),
        "Cancel persists nothing"
    );
    // Reset To Default: era confirm popup, then LoadBindings(0) — JUMP's defaults return
    // (Okay earlier saved it as NUMPAD0-only, so this really is the DEFAULT set, not the
    // saved one).
    shown(&mut s);
    s.run("KeyBindingFrameDefaultButton:Click()").unwrap();
    s.run("StaticPopup1Button1:Click()").unwrap();
    assert!(s
        .eval::<bool>(
            r#"local k1, k2 = GetBindingKey("JUMP"); return k1 == "SPACE" and k2 == "NUMPAD0""#
        )
        .unwrap());
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn the_esc_ladder_rung_cancels_and_the_character_checkbox_switches_sets() {
    let mut s = harness();
    shown(&mut s);
    // An unsaved bind, then the ladder (the ESC binding's own body): reverted + closed +
    // menu back.
    s.run("KeyBindingFrameBinding8Key1Button:Click()").unwrap();
    s.run(r#"KeyBindingFrame_OnHostKey("G")"#).unwrap();
    s.run("ToggleGameMenu()").unwrap();
    assert!(!s
        .eval::<bool>("return KeyBindingFrame:IsVisible()")
        .unwrap());
    assert_eq!(
        s.eval::<String>(r#"return GetBindingAction("G")"#).unwrap(),
        "",
        "the ladder's rung is the Cancel gesture"
    );
    assert!(s.eval::<bool>("return GameMenuFrame:IsVisible()").unwrap());
    s.run("HideUIPanel(GameMenuFrame)").unwrap();

    // The character-specific checkbox: no unsaved changes → straight profile switch; the
    // title takes 1.12's per-character form; Okay saves set 2.
    shown(&mut s);
    s.run("KeyBindingFrameCharacterButton:Click()").unwrap();
    assert_eq!(s.current_binding_set(), 2);
    assert!(s
        .eval::<String>("return KeyBindingFrameTitleText:GetText()")
        .unwrap()
        .starts_with("Key Bindings for"));
    s.run("KeyBindingFrameOkayButton:Click()").unwrap();
    assert_eq!(s.take_keybind_requests(), vec![KeybindRequest::Save(2)]);
    assert!(s.character_bindings_exist());
    // Back to general: Okay from the character set asks the 1.12 confirm, whose accept
    // saves set 1 and drops the character set.
    shown(&mut s);
    s.run("KeyBindingFrameCharacterButton:Click()").unwrap();
    assert_eq!(s.current_binding_set(), 1);
    s.run("KeyBindingFrameOkayButton:Click()").unwrap();
    assert!(
        s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "leaving the character set confirms the permanent delete"
    );
    s.run("StaticPopup1Button1:Click()").unwrap();
    assert_eq!(s.take_keybind_requests(), vec![KeybindRequest::Save(1)]);
    assert!(
        !s.character_bindings_exist(),
        "the confirmed delete dropped set 2"
    );
    assert!(!s
        .eval::<bool>("return KeyBindingFrame:IsVisible()")
        .unwrap());
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}
