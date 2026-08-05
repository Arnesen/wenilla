//! The shipped `assets/ui/KeyBindingsPage.xml` + OptionsFrame.xml's Keybindings body — the
//! Options window's Keybindings category over the engine's binding table (decision 1008,
//! superseding 0997's standalone window; the provenance block in the XML).
//!
//! What these guard: the module + the options window load clean together; the page is an
//! ordinary category (body-swapped, Defaults live, the Unbind button only here); the section
//! tree is the honest tree's non-empty categories in 1.12 `Bindings.xml` order, COLLAPSED by
//! default (the era's own state) and toggling like the era's expandable sections; the capture
//! flow — select a capsule → the host arm arms → the canonical chord lands through
//! `KeyBindings_OnHostKey` — binds LIVE-COMMIT (every mutation queues the host persist,
//! 1008's law; closing the window keeps everything), steals with the red 1.12 message only
//! when the victim goes bare, refuses the wheel on press+release commands and restores the
//! old key; the character-specific checkbox runs 1.12's set model with the era's
//! confirm-on-uncheck-only; search surfaces binding matches as LIVE rows under the
//! Keybindings redirect head; and the action bar's abbreviation law (`GetBindingText`
//! transcribed in UIParent.xml) reads `s-2`, the ref's own Lua.
//!
//! Labels here are the RAW tokens (`BINDING_HEADER_MOVEMENT`, `BUTTON3`): the harness loads no
//! GlobalStrings, exercising the page's `getglobal(...) or raw` fallback — the app's VM
//! executes the real 1.12 GlobalStrings.lua at boot, which turns them into "Movement Keys" /
//! "Middle Mouse". One test seeds a handful of the real strings to pin that path too.

use benilla_ui::script::keybind::{KeybindCommand, KeybindRequest};
use benilla_ui::script::{QuadContent, UiScript};

use crate::bindings::commands::SPECS;

/// The page's real neighbourhood, in the manifest's own order, with the registry seeded the
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
        "UIDropDownMenu.xml",
        "ScrollTemplates.xml",
        "UIParent.xml",
        "KeyBindingsPage.xml",
        "OptionsFrame.xml",
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
        if file == "KeyBindingsPage.xml" || file == "OptionsFrame.xml" {
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

/// Open the options window on the Keybindings page.
fn on_page(s: &mut UiScript) {
    s.run(r#"ShowUIPanel(OptionsFrame); OptionsFrame_SelectCategory("Keybindings")"#)
        .unwrap();
    assert!(s.errors().is_empty(), "on page: {:?}", s.errors());
}

const ROW: &str = "OptionsFrameContainerBodyKeybindingsRow";

#[test]
fn the_page_is_an_options_category_with_the_collapsed_honest_tree() {
    let mut s = harness();
    on_page(&mut s);
    assert!(s
        .eval::<bool>("return OptionsFrameContainerBodyKeybindings:IsVisible()")
        .unwrap());
    assert!(
        s.eval::<bool>("return OptionsFrameContainerUnbind:IsVisible()")
            .unwrap(),
        "Unbind Key exists on this page"
    );
    assert!(
        !s.eval::<bool>("return OptionsFrameContainerUnbind:IsEnabled()")
            .unwrap(),
        "…disabled until a capsule is selected"
    );
    assert!(s
        .eval::<bool>("return OptionsFrameContainerDefaults:IsEnabled()")
        .unwrap());
    // The section tree: the registry's category tokens, first-appearance order — exactly
    // 1.12's file order — every section a COLLAPSED header row (the era default).
    let mut expected: Vec<&str> = Vec::new();
    for spec in SPECS {
        if !expected.contains(&spec.category) {
            expected.push(spec.category);
        }
    }
    for (i, token) in expected.iter().enumerate() {
        let text = s
            .eval::<String>(&format!("return {ROW}{}HeaderText:GetText()", i + 1))
            .unwrap();
        assert_eq!(&text, token, "section {} (raw-token fallback)", i + 1);
    }
    assert!(
        !s.eval::<bool>(&format!("return {ROW}{}:IsVisible()", expected.len() + 1))
            .unwrap(),
        "all sections collapsed: nothing past the headers"
    );
    // The multibar section is real (1008): its header token sits in the tree.
    assert!(expected.contains(&"BINDING_HEADER_MULTIACTIONBAR"));
    // Expanding Movement puts its rows under the header, byte-real defaults on the capsules.
    s.run(&format!("{ROW}1Header:Click()")).unwrap();
    assert_eq!(
        s.eval::<String>(&format!("return {ROW}2Description:GetText()"))
            .unwrap(),
        "MOVEANDSTEER"
    );
    assert_eq!(
        s.eval::<String>(&format!("return {ROW}2Key1ButtonText:GetText()"))
            .unwrap(),
        "BUTTON3"
    );
    // With the real strings present (the app's GlobalStrings), the same rows read 1.12 text.
    s.run(
        r#"BINDING_HEADER_MOVEMENT = "Movement Keys"
             BINDING_NAME_MOVEANDSTEER = "Move and Steer"
             KEY_BUTTON3 = "Middle Mouse"
             KeyBindingsPage_Update()"#,
    )
    .unwrap();
    assert_eq!(
        s.eval::<String>(&format!("return {ROW}1HeaderText:GetText()"))
            .unwrap(),
        "Movement Keys"
    );
    assert_eq!(
        s.eval::<String>(&format!("return {ROW}2Description:GetText()"))
            .unwrap(),
        "Move and Steer"
    );
    assert_eq!(
        s.eval::<String>(&format!("return {ROW}2Key1ButtonText:GetText()"))
            .unwrap(),
        "Middle Mouse"
    );
    // Collapse again: the next header slides back up under the first.
    s.run(&format!("{ROW}1Header:Click()")).unwrap();
    assert_eq!(
        s.eval::<String>(&format!("return {ROW}2HeaderText:GetText()"))
            .unwrap(),
        "BINDING_HEADER_CHAT"
    );
    // Leaving the page hides its body and the Unbind button.
    s.run(r#"OptionsFrame_SelectCategory("Controls")"#).unwrap();
    assert!(!s
        .eval::<bool>("return OptionsFrameContainerBodyKeybindings:IsVisible()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return OptionsFrameContainerUnbind:IsVisible()")
        .unwrap());
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn the_capture_flow_binds_steals_and_refuses_like_112() {
    let mut s = harness();
    on_page(&mut s);
    s.run(&format!("{ROW}1Header:Click()")).unwrap(); // expand Movement
    s.take_keybind_requests(); // drop any noise before the flow under test
                               // Row 3 is MOVEFORWARD (W, UP). Selecting its Key 1 capsule arms the host seam.
    assert!(!s.bind_capture_armed());
    s.run(&format!("{ROW}3Key1Button:Click()")).unwrap();
    assert!(
        s.bind_capture_armed(),
        "a selected capsule arms the capture"
    );
    assert!(
        s.eval::<bool>("return OptionsFrameContainerUnbind:IsEnabled()")
            .unwrap(),
        "Unbind arms with the selection"
    );
    // The host hands back a canonical chord: F binds into slot 1, W's old seat; UP survives
    // in slot 2; the capture disarms; the table is LIVE and the bind COMMITTED (Save queued —
    // 1008's live-commit law, where 0997 waited for Okay).
    s.run(r#"KeyBindings_OnHostKey("F")"#).unwrap();
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
    assert_eq!(s.take_keybind_requests(), vec![KeybindRequest::Save(1)]);
    assert_eq!(
        s.eval::<String>("return OptionsFrameContainerBodyKeybindingsOutput:GetText()")
            .unwrap(),
        "Key Bound Successfully"
    );
    // Stealing the LAST key of another command names the victim in red (1.12's
    // KEY_UNBOUND_ERROR). T is ATTACKTARGET's only key.
    s.run(&format!("{ROW}3Key2Button:Click()")).unwrap();
    s.run(r#"KeyBindings_OnHostKey("T")"#).unwrap();
    assert_eq!(
        s.eval::<String>(r#"return GetBindingAction("T")"#).unwrap(),
        "MOVEFORWARD"
    );
    assert!(
        s.eval::<String>("return OptionsFrameContainerBodyKeybindingsOutput:GetText()")
            .unwrap()
            .contains("ATTACKTARGET"),
        "the newly-bare victim is named"
    );
    // The wheel refusal: MOVEFORWARD has press+release state — SetBinding refuses the wheel
    // and the slot's old key is restored (1.12's KeyBindingFrame_SetBinding).
    s.run(&format!("{ROW}3Key1Button:Click()")).unwrap();
    s.run(r#"KeyBindings_OnHostKey("MOUSEWHEELUP")"#).unwrap();
    assert!(
        s.eval::<bool>(r#"local k1 = GetBindingKey("MOVEFORWARD"); return k1 == "F""#)
            .unwrap(),
        "the refused slot restored its key"
    );
    assert_eq!(
        s.eval::<String>("return OptionsFrameContainerBodyKeybindingsOutput:GetText()")
            .unwrap(),
        "Can't bind mousewheel to actions with up and down states"
    );
    // A right-click on the armed capsule deselects without binding.
    s.run(&format!("{ROW}3Key1Button:Click()")).unwrap();
    assert!(s.bind_capture_armed());
    s.run(&format!(r#"{ROW}3Key1Button:Click("RightButton")"#))
        .unwrap();
    assert!(!s.bind_capture_armed(), "right-click deselects");
    // Hiding the window disarms a straggling capture (the OnHide hook — a locked-out client
    // otherwise: the armed seam swallows all input with no window on screen).
    s.run(&format!("{ROW}3Key1Button:Click()")).unwrap();
    assert!(s.bind_capture_armed());
    s.run("HideUIPanel(OptionsFrame)").unwrap();
    assert!(!s.bind_capture_armed(), "OnHide disarms");
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn unbind_reset_and_the_live_commit_replace_okay_cancel() {
    let mut s = harness();
    on_page(&mut s);
    s.run(&format!("{ROW}1Header:Click()")).unwrap(); // expand Movement
    s.take_keybind_requests();
    // JUMP is Movement's 8th command → row 9 under the header. Unbind its Key 1: SPACE goes,
    // NUMPAD0 slides into slot 1 (the 1.12 slot dance), and the change commits at once.
    assert_eq!(
        s.eval::<String>(&format!("return {ROW}9Description:GetText()"))
            .unwrap(),
        "JUMP"
    );
    s.run(&format!("{ROW}9Key1Button:Click()")).unwrap();
    s.run("OptionsFrameContainerUnbind:Click()").unwrap();
    assert!(s
        .eval::<bool>(
            r#"local k1, k2 = GetBindingKey("JUMP"); return k1 == "NUMPAD0" and k2 == nil"#
        )
        .unwrap());
    assert_eq!(s.take_keybind_requests(), vec![KeybindRequest::Save(1)]);
    // LIVE-COMMIT is the law now (1008): bind G, close the window — the bind KEEPS (0997's
    // Cancel/ESC revert died with the standalone window).
    s.run(&format!("{ROW}9Key1Button:Click()")).unwrap();
    s.run(r#"KeyBindings_OnHostKey("G")"#).unwrap();
    assert_eq!(s.take_keybind_requests(), vec![KeybindRequest::Save(1)]);
    s.run("OptionsFrameCloseButton:Click()").unwrap();
    assert_eq!(
        s.eval::<String>(r#"return GetBindingAction("G")"#).unwrap(),
        "JUMP",
        "closing keeps the live-committed bind"
    );
    // Reset To Default: the page's Defaults button behind the era confirm — JUMP's real
    // defaults return and the reset itself commits.
    on_page(&mut s);
    s.run("OptionsFrameContainerDefaults:Click()").unwrap();
    assert!(s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap());
    s.run("StaticPopup1Button1:Click()").unwrap();
    assert!(s
        .eval::<bool>(
            r#"local k1, k2 = GetBindingKey("JUMP"); return k1 == "SPACE" and k2 == "NUMPAD0""#
        )
        .unwrap());
    assert_eq!(s.take_keybind_requests(), vec![KeybindRequest::Save(1)]);
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn the_esc_ladder_closes_the_window_and_the_checkbox_switches_sets() {
    let mut s = harness();
    on_page(&mut s);
    // The ladder (the ESC binding's own body): the options rung hides the window — since 1008
    // that IS the whole gesture for keybinds too (live-commit; nothing to revert).
    s.run("ToggleGameMenu()").unwrap();
    assert!(!s.eval::<bool>("return OptionsFrame:IsVisible()").unwrap());

    // The character-specific checkbox (1.12's set model, era confirm-on-uncheck-only law):
    // CHECK switches to the character set and saves it into existence at once.
    on_page(&mut s);
    s.take_keybind_requests();
    s.run("OptionsFrameContainerBodyKeybindingsCharacterRowCheck:Click()")
        .unwrap();
    assert_eq!(s.current_binding_set(), 2);
    assert!(s.character_bindings_exist());
    assert_eq!(s.take_keybind_requests(), vec![KeybindRequest::Save(2)]);
    // UNCHECK is destructive: the box springs back and the 1.12 confirm decides. Cancel
    // first — still on the character set.
    s.run("OptionsFrameContainerBodyKeybindingsCharacterRowCheck:Click()")
        .unwrap();
    assert!(
        s.eval::<bool>("return StaticPopup1:IsVisible()").unwrap(),
        "leaving the character set confirms the permanent delete"
    );
    assert!(
        s.eval::<bool>(
            "return OptionsFrameContainerBodyKeybindingsCharacterRowCheck:GetChecked() ~= nil"
        )
        .unwrap(),
        "the box springs back until the popup decides"
    );
    s.run("StaticPopup1Button2:Click()").unwrap();
    assert_eq!(s.current_binding_set(), 2);
    assert!(s.character_bindings_exist());
    // Accept: back to the account set, the character set dropped (load-then-save order — the
    // account file must not inherit the character binds).
    s.run("OptionsFrameContainerBodyKeybindingsCharacterRowCheck:Click()")
        .unwrap();
    s.run("StaticPopup1Button1:Click()").unwrap();
    assert_eq!(s.current_binding_set(), 1);
    assert!(
        !s.character_bindings_exist(),
        "the confirmed delete dropped set 2"
    );
    assert_eq!(s.take_keybind_requests(), vec![KeybindRequest::Save(1)]);
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn search_surfaces_bindings_as_live_rows_under_the_redirect_head() {
    let mut s = harness();
    s.run(r#"BINDING_NAME_JUMP = "Jump""#).unwrap();
    s.run("ShowUIPanel(OptionsFrame)").unwrap();
    s.take_keybind_requests();
    // A query that matches one binding and no CVar row: the Keybindings group head shows,
    // with the match painted LIVE on the search pool (the era reflows its real rows the
    // same way).
    s.run(r#"OptionsFrameSearchBox:SetText("jump")"#).unwrap();
    assert!(s
        .eval::<bool>("return OptionsFrameContainerBodySearchHeadKeybindings:IsVisible()")
        .unwrap());
    assert!(s
        .eval::<bool>("return OptionsFrameContainerBodyKeybindSearch1:IsVisible()")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return OptionsFrameContainerBodyKeybindSearch1Description:GetText()")
            .unwrap(),
        "Jump"
    );
    assert!(!s
        .eval::<bool>("return OptionsFrameContainerBodyKeybindSearch2:IsVisible()")
        .unwrap());
    // The result row is LIVE: its capsule arms, the bind lands and commits, the row relabels.
    s.run("OptionsFrameContainerBodyKeybindSearch1Key1Button:Click()")
        .unwrap();
    assert!(s.bind_capture_armed());
    s.run(r#"KeyBindings_OnHostKey("H")"#).unwrap();
    assert_eq!(
        s.eval::<String>(r#"return GetBindingAction("H")"#).unwrap(),
        "JUMP"
    );
    assert_eq!(s.take_keybind_requests(), vec![KeybindRequest::Save(1)]);
    assert_eq!(
        s.eval::<String>("return OptionsFrameContainerBodyKeybindSearch1Key1ButtonText:GetText()")
            .unwrap(),
        "H"
    );
    // The head is the era redirect: clicking it ends the search on the Keybindings page.
    s.run("OptionsFrameContainerBodySearchHeadKeybindings:Click()")
        .unwrap();
    assert_eq!(
        s.eval::<String>("return OptionsFrame.selectedCategory")
            .unwrap(),
        "Keybindings"
    );
    assert!(s
        .eval::<bool>("return OptionsFrameContainerBodyKeybindings:IsVisible()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return OptionsFrameContainerBodyKeybindSearch1:IsVisible()")
        .unwrap());
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn the_action_bar_abbreviation_is_the_refs_own_getbindingtext() {
    let s = harness();
    // ref UIParent.lua:1819 transcribed (UIParent.xml): one modifier abbreviates…
    assert_eq!(
        s.eval::<String>(r#"return GetBindingText("SHIFT-2", "KEY_", 1)"#)
            .unwrap(),
        "s-2",
        "the director's SHIF… truncation reads s-2 now"
    );
    assert_eq!(
        s.eval::<String>(r#"return GetBindingText("ALT-Z", "KEY_", 1)"#)
            .unwrap(),
        "a-Z"
    );
    assert_eq!(
        s.eval::<String>(r#"return GetBindingText("W", "KEY_", 1)"#)
            .unwrap(),
        "W"
    );
    // …two or more collapse to the ref's dot — including the CTRL-- oddity, whose second
    // dash the ref's dash-counting loop counts as a second modifier (their quirk, pinned).
    assert_eq!(
        s.eval::<String>(r#"return GetBindingText("CTRL-SHIFT-2", "KEY_", 1)"#)
            .unwrap(),
        "·"
    );
    assert_eq!(
        s.eval::<String>(r#"return GetBindingText("CTRL--", "KEY_", 1)"#)
            .unwrap(),
        "·"
    );
    // The full form keeps the prefixes and reads the KEY_* global when it exists.
    assert_eq!(
        s.eval::<String>(r#"return GetBindingText("SHIFT-2", "KEY_")"#)
            .unwrap(),
        "SHIFT-2"
    );
    s.run(r#"KEY_SPACE = "Spacebar""#).unwrap();
    assert_eq!(
        s.eval::<String>(r#"return GetBindingText("SPACE", "KEY_")"#)
            .unwrap(),
        "Spacebar"
    );
    assert!(s
        .eval::<bool>(r#"return GetBindingText(nil) == """#)
        .unwrap());
}

/// The wheel and the bar's seat (the director's scuff report, same day 1008 landed). The wheel
/// is the QuestLog/Trainer lesson relearned: a spin bubbles up the PARENT chain (pointer.rs),
/// so a handler on the sibling faux frame never sees it — it lives on the page body, which is
/// mouse-enabled so a spin over a row's NAME (a plain-Frame area no child claims) is caught
/// too, not just the bubbles from capsule/header Buttons. The bar rides the gutter INSIDE the
/// body: the kit hangs it 6px right of the faux frame's edge, so the frame ends at the rows'
/// own -32 — anchoring it to body-right hung the bar on the window border.
#[test]
fn the_wheel_bubbles_from_the_rows_and_the_bar_rides_the_gutter() {
    let mut s = harness();
    on_page(&mut s);
    // Every section open: 100+ flat rows — the list overflows its 19 slots and the bar shows.
    s.run(
        r#"for i = 1, table.getn(KeyBindingsPage.sections) do KeyBindings_ExpandSection(i, true) end
           KeyBindingsPage_Update()"#,
    )
    .unwrap();
    const SF: &str = "OptionsFrameContainerBodyKeybindingsScrollFrame";
    assert!(s
        .eval::<bool>(&format!("return {SF}ScrollBar:IsVisible()"))
        .unwrap());
    s.resolve();
    let body_right = s
        .eval::<f64>("return OptionsFrameContainerBodyKeybindings:GetRight()")
        .unwrap();
    let rows_right = s.eval::<f64>(&format!("return {ROW}1:GetRight()")).unwrap();
    let bar_left = s
        .eval::<f64>(&format!("return {SF}ScrollBar:GetLeft()"))
        .unwrap();
    let bar_right = s
        .eval::<f64>(&format!("return {SF}ScrollBar:GetRight()"))
        .unwrap();
    assert!(
        bar_left >= rows_right,
        "bar starts right of the rows ({bar_left} vs {rows_right})"
    );
    assert!(
        bar_right <= body_right - 8.0,
        "bar inset from the body edge ({bar_right} vs body {body_right})"
    );
    // A spin over a row NAME — the dead zone when the handler lived on the sibling.
    let quads = s.extract();
    let (wx, wy) = quads
        .iter()
        .find_map(|q| match &q.content {
            QuadContent::Text { text: Some(t), .. } if t == "MOVEANDSTEER" => q
                .rect
                .map(|r| ((r.left + r.right) * 0.5, (r.bottom + r.top) * 0.5)),
            _ => None,
        })
        .expect("a MOVEANDSTEER description quad");
    s.mouse_wheel(wx, wy, -1.0);
    assert!(s.errors().is_empty(), "wheel over a name: {:?}", s.errors());
    assert_eq!(
        s.eval::<f64>(&format!("return BenillaFauxScrollFrame_GetOffset({SF})"))
            .unwrap(),
        1.0
    );
    // …and over a CAPSULE (a Button: the bubble path, capsule → row → body). GetLeft-family
    // reads are scale-LOCAL (the 1.12 contract; this window wears ERA_WINDOW_SCALE) — the
    // pointer lives in screen px, so the aim converts through GetEffectiveScale.
    let (cx, cy) = {
        let k = s
            .eval::<f64>(&format!("return {ROW}2Key1Button:GetEffectiveScale()"))
            .unwrap();
        let l = s
            .eval::<f64>(&format!("return {ROW}2Key1Button:GetLeft()"))
            .unwrap();
        let r = s
            .eval::<f64>(&format!("return {ROW}2Key1Button:GetRight()"))
            .unwrap();
        let t = s
            .eval::<f64>(&format!("return {ROW}2Key1Button:GetTop()"))
            .unwrap();
        let b = s
            .eval::<f64>(&format!("return {ROW}2Key1Button:GetBottom()"))
            .unwrap();
        ((((l + r) * 0.5 * k) as f32), (((t + b) * 0.5 * k) as f32))
    };
    s.mouse_wheel(cx, cy, -1.0);
    assert_eq!(
        s.eval::<f64>(&format!("return BenillaFauxScrollFrame_GetOffset({SF})"))
            .unwrap(),
        2.0
    );
    // While a capsule is armed the wheel is a BIND, never a scroll (1.12's law) — in the app
    // the host seam swallows the spin before the UI sees one; the Lua guard is the belt for
    // a spin that reaches the page anyway.
    s.run(&format!("{ROW}2Key1Button:Click()")).unwrap();
    s.mouse_wheel(wx, wy, -1.0);
    assert_eq!(
        s.eval::<f64>(&format!("return BenillaFauxScrollFrame_GetOffset({SF})"))
            .unwrap(),
        2.0,
        "armed: the wheel must not scroll"
    );
    s.run("KeyBindings_SetSelected(nil)").unwrap();
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}
