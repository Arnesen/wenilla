//! Button / CheckButton: method sets, state textures, click registration, additive highlight.

use super::common::script;
use crate::script::*;

#[test]
fn button_methods_exist_only_on_buttons() {
    let s = script();
    s.run(
        r#"
        f = CreateFrame("Frame", "PlainF")
        b = CreateFrame("Button", "Btn")
        cb = CreateFrame("CheckButton", "CBtn")
    "#,
    )
    .unwrap();
    // Duck-typing honesty across the class chain (RF-28 method sets).
    assert!(s.eval::<bool>("return f.SetText == nil").unwrap());
    assert!(s.eval::<bool>("return f.SetChecked == nil").unwrap());
    assert!(s.eval::<bool>("return b.SetText ~= nil").unwrap());
    assert!(s.eval::<bool>("return b.SetChecked == nil").unwrap());
    assert!(s.eval::<bool>("return b.SetValue == nil").unwrap());
    assert!(s.eval::<bool>("return cb.SetChecked ~= nil").unwrap());
    assert!(s.eval::<bool>("return cb.SetNormalTexture ~= nil").unwrap());
    // Buttons are mouse-enabled by construction (the client ctor enables input).
    assert!(s.eval::<bool>("return b:IsMouseEnabled()").unwrap());
    assert!(!s.eval::<bool>("return f:IsMouseEnabled()").unwrap());
}

#[test]
fn button_state_textures_switch_with_interaction() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local b = CreateFrame("Button", "StateBtn")
        b:SetPoint("BOTTOMLEFT", 0, 0); b:SetSize(100, 100)
        b:SetNormalTexture("Interface\\N.blp")
        b:SetPushedTexture("Interface\\P.blp")
        b:SetDisabledTexture("Interface\\D.blp")
        b:SetHighlightTexture("Interface\\H.blp")
        b:SetText("Go")
    "#,
    )
    .unwrap();
    s.resolve();

    let visible = |s: &UiScript| -> Vec<String> {
        s.extract()
            .iter()
            .filter_map(|q| match &q.content {
                QuadContent::Texture { path: Some(p), .. } => Some(p.clone()),
                _ => None,
            })
            .collect()
    };

    // Idle: normal only (pushed/disabled suppressed; not hovered ⇒ no highlight).
    assert_eq!(visible(&s), vec!["Interface\\N.blp".to_string()]);
    // The ButtonText always draws.
    assert!(s
        .extract()
        .iter()
        .any(|q| matches!(&q.content, QuadContent::Text { text: Some(t), .. } if t == "Go")));

    // Hover: normal + additive highlight.
    s.mouse_move(50.0, 50.0);
    let v = visible(&s);
    assert!(
        v.contains(&"Interface\\N.blp".to_string()) && v.contains(&"Interface\\H.blp".to_string())
    );

    // Held down over it: pushed replaces normal (highlight still hovering).
    s.mouse_button(50.0, 50.0, "LeftButton", true);
    let v = visible(&s);
    assert!(
        v.contains(&"Interface\\P.blp".to_string()) && !v.contains(&"Interface\\N.blp".to_string())
    );
    s.mouse_button(50.0, 50.0, "LeftButton", false);

    // Disabled: disabled texture only, and no highlight even under the cursor.
    s.run("StateBtn:Disable()").unwrap();
    assert_eq!(visible(&s), vec!["Interface\\D.blp".to_string()]);
    assert!(!s.eval::<bool>("return StateBtn:IsEnabled()").unwrap());
    s.run("StateBtn:Enable()").unwrap();
    assert_eq!(s.eval::<String>("return StateBtn:GetText()").unwrap(), "Go");
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// **A right-click lights a button up too** — the director's report, and `0x77924b`'s law.
///
/// `CButton::OnMouseDown` gates the PushedTexture on `[this+0x330] & (m | m << 8)`: the button
/// registered for that mouse button in **either** variant. Not on the click firing, not on the
/// handler doing anything — which is why right-clicking an action, spellbook or pet slot flashes,
/// while right-clicking a default `LeftButtonUp` button does not.
#[test]
fn any_registered_mouse_button_shows_the_pushed_texture() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local function slot(name, x)
            local b = CreateFrame("Button", name)
            b:SetPoint("BOTTOMLEFT", x, 0); b:SetSize(100, 100)
            b:SetNormalTexture("Interface\\" .. name .. "N.blp")
            b:SetPushedTexture("Interface\\" .. name .. "P.blp")
            return b
        end
        -- A bar slot: both buttons registered, exactly as ActionButton/PetActionButton do.
        slot("Bar", 0):RegisterForClicks("LeftButtonUp", "RightButtonUp")
        -- A plain button: the default set, {LeftButtonUp}.
        slot("Plain", 200)
    "#,
    )
    .unwrap();
    s.resolve();
    let shows = |s: &UiScript, path: &str| {
        s.extract()
            .iter()
            .any(|q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p == path))
    };

    // The bar slot lights under EITHER button.
    for button in ["LeftButton", "RightButton"] {
        s.mouse_move(50.0, 50.0);
        s.mouse_button(50.0, 50.0, button, true);
        assert!(
            shows(&s, "Interface\\BarP.blp") && !shows(&s, "Interface\\BarN.blp"),
            "{button} down must show the pushed art"
        );
        assert_eq!(
            s.eval::<String>("return Bar:GetButtonState()").unwrap(),
            "PUSHED",
            "and the state variable the engine writes is one variable ({button})"
        );
        s.mouse_button(50.0, 50.0, button, false);
        assert!(shows(&s, "Interface\\BarN.blp"), "the release restores it");
    }

    // The plain button lights under the left only — `0x77924b` is a real gate, not a formality.
    s.mouse_move(250.0, 50.0);
    s.mouse_button(250.0, 50.0, "RightButton", true);
    assert!(
        shows(&s, "Interface\\PlainN.blp") && !shows(&s, "Interface\\PlainP.blp"),
        "an unregistered button must not light"
    );
    s.mouse_button(250.0, 50.0, "RightButton", false);
    s.mouse_button(250.0, 50.0, "LeftButton", true);
    assert!(shows(&s, "Interface\\PlainP.blp"));
    s.mouse_button(250.0, 50.0, "LeftButton", false);
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// `SetButtonState`/`GetButtonState` (`0x780270`/`0x780180`) — the scripted press state the ref's
/// `ActionButtonDown/Up` keybind pair drives: PUSHED shows the pushed texture with no mouse
/// involved, NORMAL restores, unknown states error, and a disabled button answers DISABLED.
#[test]
fn set_button_state_drives_the_pushed_visual() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local b = CreateFrame("Button", "PushBtn")
        b:SetPoint("BOTTOMLEFT", 0, 0); b:SetSize(100, 100)
        b:SetNormalTexture("Interface\\N.blp")
        b:SetPushedTexture("Interface\\P.blp")
    "#,
    )
    .unwrap();
    s.resolve();
    let shows = |s: &UiScript, path: &str| {
        s.extract()
            .iter()
            .any(|q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p == path))
    };

    assert_eq!(
        s.eval::<String>("return PushBtn:GetButtonState()").unwrap(),
        "NORMAL"
    );
    s.run(r#"PushBtn:SetButtonState("PUSHED")"#).unwrap();
    assert_eq!(
        s.eval::<String>("return PushBtn:GetButtonState()").unwrap(),
        "PUSHED"
    );
    assert!(shows(&s, "Interface\\P.blp") && !shows(&s, "Interface\\N.blp"));
    s.run(r#"PushBtn:SetButtonState("NORMAL")"#).unwrap();
    assert!(shows(&s, "Interface\\N.blp") && !shows(&s, "Interface\\P.blp"));
    // Unknown state: a runtime error, not a silent no-op.
    assert!(s.run(r#"PushBtn:SetButtonState("SIDEWAYS")"#).is_err());
    // Disabled wins the read (Enable/Disable owns that state, not SetButtonState).
    s.run("PushBtn:Disable()").unwrap();
    assert_eq!(
        s.eval::<String>("return PushBtn:GetButtonState()").unwrap(),
        "DISABLED"
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn disabled_button_swallows_clicks_checkbutton_toggles_before_onclick() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        clicks, seen_checked = 0, nil
        local cb = CreateFrame("CheckButton", "Toggler")
        cb:SetPoint("BOTTOMLEFT", 0, 0); cb:SetSize(100, 100)
        cb:SetScript("OnClick", function(self, button, down)
            clicks = clicks + 1
            seen_checked = self:GetChecked()
        end)
    "#,
    )
    .unwrap();
    s.resolve();

    // Physical click: the checked state flips BEFORE OnClick (the documented contract).
    s.mouse_button(50.0, 50.0, "LeftButton", true);
    s.mouse_button(50.0, 50.0, "LeftButton", false);
    assert_eq!(s.eval::<i64>("return clicks").unwrap(), 1);
    assert!(s.eval::<bool>("return seen_checked == true").unwrap());
    assert!(s.eval::<bool>("return Toggler:GetChecked()").unwrap());

    // Programmatic Click() rides the same path: toggles back off.
    s.run("Toggler:Click()").unwrap();
    assert_eq!(s.eval::<i64>("return clicks").unwrap(), 2);
    assert!(!s.eval::<bool>("return Toggler:GetChecked()").unwrap());

    // Disabled: the click is swallowed — no OnClick, no toggle.
    s.run("Toggler:Disable()").unwrap();
    s.mouse_button(50.0, 50.0, "LeftButton", true);
    s.mouse_button(50.0, 50.0, "LeftButton", false);
    s.run("Toggler:Click()").unwrap();
    assert_eq!(s.eval::<i64>("return clicks").unwrap(), 2);
    assert!(!s.eval::<bool>("return Toggler:GetChecked()").unwrap());

    // SetChecked takes 1/true/nil (1.12 and Era forms).
    s.run("Toggler:SetChecked(1)").unwrap();
    assert!(s.eval::<bool>("return Toggler:GetChecked()").unwrap());
    s.run("Toggler:SetChecked(nil)").unwrap();
    assert!(!s.eval::<bool>("return Toggler:GetChecked()").unwrap());
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn default_registration_is_left_click_only_right_click_reaches_nothing() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        clicks = 0
        local btn = CreateFrame("Button", "Vendor")
        btn:SetPoint("BOTTOMLEFT", 0, 0); btn:SetSize(100, 100)
        btn:SetScript("OnClick", function(self, button, down) clicks = clicks + 1 end)
    "#,
    )
    .unwrap();
    s.resolve();

    // A right-click (press+release, same frame) reaches OnMouseDown/OnMouseUp but never OnClick —
    // the client's own default registered-click set is {"LeftButtonUp"} only.
    s.mouse_button(50.0, 50.0, "RightButton", true);
    s.mouse_button(50.0, 50.0, "RightButton", false);
    assert_eq!(s.eval::<i64>("return clicks").unwrap(), 0);
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn register_for_clicks_grows_right_click_and_carries_the_button_name() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        clicks, click_btn, arg1_btn = 0, nil, nil
        local btn = CreateFrame("Button", "Vendor")
        btn:SetPoint("BOTTOMLEFT", 0, 0); btn:SetSize(100, 100)
        btn:RegisterForClicks("LeftButtonUp", "RightButtonUp")
        btn:SetScript("OnClick", function(self, button, down)
            clicks = clicks + 1
            click_btn = button
            arg1_btn = arg1   -- the 1.12 legacy-global convention, same value
        end)
    "#,
    )
    .unwrap();
    s.resolve();

    s.mouse_button(50.0, 50.0, "RightButton", true);
    s.mouse_button(50.0, 50.0, "RightButton", false);
    assert_eq!(s.eval::<i64>("return clicks").unwrap(), 1);
    assert_eq!(s.eval::<String>("return click_btn").unwrap(), "RightButton");
    assert_eq!(s.eval::<String>("return arg1_btn").unwrap(), "RightButton");

    // RegisterForClicks replaces the set with its whole vararg list — LeftButtonUp still fires.
    s.mouse_button(50.0, 50.0, "LeftButton", true);
    s.mouse_button(50.0, 50.0, "LeftButton", false);
    assert_eq!(s.eval::<i64>("return clicks").unwrap(), 2);
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn down_registration_fires_on_press_and_toggles_checked_once() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        clicks, seen_down = 0, nil
        local cb = CreateFrame("CheckButton", "QuickSell")
        cb:SetPoint("BOTTOMLEFT", 0, 0); cb:SetSize(100, 100)
        cb:RegisterForClicks("LeftButtonDown")
        cb:SetScript("OnClick", function(self, button, down)
            clicks = clicks + 1
            seen_down = down
        end)
    "#,
    )
    .unwrap();
    s.resolve();

    // The press alone fires OnClick (down=true) and toggles Checked exactly once.
    s.mouse_button(50.0, 50.0, "LeftButton", true);
    assert_eq!(s.eval::<i64>("return clicks").unwrap(), 1);
    assert!(s.eval::<bool>("return seen_down == true").unwrap());
    assert!(s.eval::<bool>("return QuickSell:GetChecked()").unwrap());

    // The release does not fire again — "LeftButtonUp" is no longer in the registered set
    // (RegisterForClicks replaced it, it did not add to it).
    s.mouse_button(50.0, 50.0, "LeftButton", false);
    assert_eq!(s.eval::<i64>("return clicks").unwrap(), 1);
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

#[test]
fn highlight_is_additive_and_state_textures_fill_then_anchor() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        local b = CreateFrame("Button", "AddBtn")
        b:SetPoint("BOTTOMLEFT", 100, 100); b:SetSize(36, 36)
        b:SetNormalTexture("Interface\\Ring.blp")
        b:GetNormalTexture():SetSize(64, 64)
        b:SetHighlightTexture("Interface\\Hi.blp")
    "#,
    )
    .unwrap();
    s.resolve();
    s.mouse_move(118.0, 118.0); // hover so the highlight draws

    let quads = s.extract();
    let find = |path: &str| {
        quads
            .iter()
            .find(|q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p == path))
            .expect(path)
    };
    // The highlight carries the ADD contract (the client's SetHighlightTexture default).
    assert!(matches!(
        &find("Interface\\Hi.blp").content,
        QuadContent::Texture { additive: true, .. }
    ));
    // A fresh state texture gets the creation-path implicit SetAllPoints (decision 1310 — the
    // reference's string setters anchor a freshly built texture to the button outright), whose
    // two corners pin all four edges: the later SetSize(64) is structurally unread and the ring
    // FILLS the 36px button.
    let r = find("Interface\\Ring.blp").rect.unwrap();
    assert_eq!(
        (r.left, r.right, r.bottom, r.top),
        (100.0, 136.0, 100.0, 136.0)
    );
    // The real quickslot-overhang idiom is an ANCHOR, not an anchorless size (ActionButton's
    // 66×66 UI-Quickslot2 authors `<Anchor point="CENTER">`): one CENTER point replaces only its
    // own slot, but clearing first leaves the single anchor + the 64px size → the centered
    // overhang, 100..136 → 86..150.
    s.run(
        r#"
        local n = AddBtn:GetNormalTexture()
        n:ClearAllPoints()
        n:SetPoint("CENTER", AddBtn, "CENTER", 0, 0)
    "#,
    )
    .unwrap();
    s.resolve();
    let quads = s.extract();
    let ring = quads
        .iter()
        .find(|q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p == "Interface\\Ring.blp"))
        .unwrap();
    let r = ring.rect.unwrap();
    assert_eq!(
        (r.left, r.right, r.bottom, r.top),
        (86.0, 150.0, 86.0, 150.0)
    );
    // SetBlendMode overrides back to straight alpha.
    s.run(r#"AddBtn:GetHighlightTexture():SetBlendMode("BLEND")"#)
        .unwrap();
    let quads = s.extract();
    let hi = quads
        .iter()
        .find(|q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p == "Interface\\Hi.blp"))
        .unwrap();
    assert!(matches!(
        &hi.content,
        QuadContent::Texture {
            additive: false,
            ..
        }
    ));
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// The per-state label fonts (`SetTextFontObject`/`SetDisabledFontObject`, XML `<NormalFont>` etc.):
/// the ButtonText re-points to the CURRENT state's font object at extract — gold while enabled,
/// gray after Disable() — with no Lua repaint (the client's UIPanelButtonTemplate label behavior).
/// An explicit SetTextColor survives the re-point (the client's explicitly-set mask).
#[test]
fn button_label_repaints_by_state_font_object() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.register_font_object(
        "GoldFont",
        FontObject {
            color: Some([1.0, 0.82, 0.0, 1.0]),
            height: Some(12.0),
            ..Default::default()
        },
    );
    s.register_font_object(
        "GrayFont",
        FontObject {
            color: Some([0.5, 0.5, 0.5, 1.0]),
            height: Some(12.0),
            ..Default::default()
        },
    );
    s.run(
        r#"
        b = CreateFrame("Button", "FontBtn")
        b:SetPoint("CENTER", 0, 0); b:SetSize(100, 20)
        b:SetText("Label")
        b:SetTextFontObject("GoldFont")
        b:SetDisabledFontObject("GrayFont")
    "#,
    )
    .unwrap();

    let label_color = |s: &mut crate::script::UiScript| {
        s.resolve();
        s.extract()
            .into_iter()
            .find_map(|q| match q.content {
                QuadContent::Text {
                    text: Some(t),
                    color,
                    ..
                } if t == "Label" => Some(color),
                _ => None,
            })
            .expect("label text quad")
    };

    assert_eq!(
        label_color(&mut s),
        Some([1.0, 0.82, 0.0, 1.0]),
        "enabled: gold"
    );
    s.run("b:Disable()").unwrap();
    assert_eq!(
        label_color(&mut s),
        Some([0.5, 0.5, 0.5, 1.0]),
        "disabled: gray"
    );
    s.run("b:Enable()").unwrap();
    assert_eq!(
        label_color(&mut s),
        Some([1.0, 0.82, 0.0, 1.0]),
        "re-enabled: gold again"
    );

    // An explicit SetTextColor wins over the state font's color (explicitly-set mask).
    s.run("b:GetFontString():SetTextColor(0.1, 0.2, 0.3)")
        .unwrap();
    let c = label_color(&mut s).expect("colored");
    assert!((c[0] - 0.1).abs() < 1e-6 && (c[1] - 0.2).abs() < 1e-6);
}

/// **`Button:GetTextWidth` / `GetTextHeight`** — the reference's own Button text-extent readers
/// (`0x782290` / `0x782390`; wow-re `widget-api-batch-benilla.md` Q8 carves them present on Button
/// and `GetStringWidth` **absent**). Both forward to the label FontString's own extent slots, which
/// is what this asserts: answer a host measure for the label, and the BUTTON must report it.
///
/// `Bagnon_Forever/database/ui.lua:61` sizes its character-switch dropdown from
/// `button:GetTextWidth() + 40`, so a nil method took the whole dropdown down — the director could
/// not switch characters in Bagnon at all.
#[test]
fn a_button_reports_its_labels_extent() {
    let mut s = script();
    s.set_screen_size(800.0, 600.0);
    s.run(
        r#"
        b = CreateFrame("Button", "WidthBtn", UIParent)
        b:SetPoint("CENTER", UIParent, "CENTER", 0, 0)
        b:SetWidth(120) b:SetHeight(24)
        b:SetText("A Label")
        plain = CreateFrame("Button", "LabellessBtn", UIParent)
    "#,
    )
    .unwrap();
    s.resolve();

    // Unmeasured is 0 — the same "converges next frame" contract every other metric read has.
    assert_eq!(
        s.eval::<f64>("return WidthBtn:GetTextWidth()").unwrap(),
        0.0
    );

    let req = s
        .fontstrings_needing_measure()
        .into_iter()
        .find(|r| r.text == "A Label")
        .expect("the label asks the host for its extent");
    s.set_measured_text_unwrapped(&[(req.id, 47.0, 14.0, req.key)]);

    assert_eq!(
        s.eval::<f64>("return WidthBtn:GetTextWidth()").unwrap(),
        47.0,
        "the Button forwards to its own label"
    );
    assert_eq!(
        s.eval::<f64>("return WidthBtn:GetTextHeight()").unwrap(),
        14.0
    );

    // A Button with no label at all answers 0 rather than raising: the reference dereferences a
    // FontString pointer (`+0x338`) here that a bare CreateFrame("Button") leaves null, and what it
    // does then is not byte-read — so this takes the harmless number instead of guessing a crash.
    assert_eq!(
        s.eval::<f64>("return LabellessBtn:GetTextWidth()").unwrap(),
        0.0
    );

    // …and it is a BUTTON method: a plain Frame must not have grown one.
    assert!(
        s.eval::<bool>(r#"return CreateFrame("Frame").GetTextWidth == nil"#)
            .unwrap(),
        "GetTextWidth is Button's, not Region's (wow-re Q8's own split)"
    );
}
