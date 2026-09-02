//! The two shared dropdown lists' own scripts (`UIDropDownMenu.xml`'s `DropDownList1`/`2`).
//!
//! These exist because the instance `<Scripts>` block was absent entirely and nothing said so: the
//! lists were one-line self-closing elements, the loader has no opinion about a frame with no
//! scripts, and every dropdown still opened and closed. The reference hangs three separate jobs off
//! those two elements and we had none of them.
//!
//! Each test is named after the claim it defends (1212): the submenu catch-all, the open-menu
//! registry's clear, the fact that the default text height is DERIVED rather than a literal, and —
//! found by the third while it was failing — that a button's label answers for the font object its
//! button gave it, which it did not.

use benilla_ui::script::UiScript;

use super::test_ui::load_ui as load_xml;

/// The production prefix the lists need (ui_script/mod.rs order): the fonts (this OnLoad reads a
/// real font height off Button1's NormalText), UIParent (the lists declare `parent="UIParent"`),
/// and GameTooltip — the list's own `$parentMenuBackdrop` OnLoad indexes `TOOLTIP_DEFAULT_COLOR`,
/// which GameTooltip.xml declares.
fn load_dropdown_kit(s: &UiScript) {
    for file in [
        "Fonts.xml",
        "UIParent.xml",
        "GameTooltip.xml",
        "Interface\\FrameXML\\UIDropDownMenu.xml",
    ] {
        load_xml(s, file);
    }
}

/// **Hiding level 1 closes level 2.** The bug this replaces was reachable by hand: open a menu with
/// a `hasArrow` row, hover the submenu open, then toggle level 1 shut. `ToggleDropDownMenu`'s close
/// arm is a bare `listFrame:Hide()`, so DropDownList2 stayed on screen with its parent gone.
///
/// The test hides level 1 *directly* rather than through the toggle, which is the stronger claim
/// and the reason the reference puts this on `OnHide` instead of in the toggle: it must catch every
/// path that hides level 1, including a parent Hide, the ESC ladder, and an addon calling
/// `DropDownList1:Hide()` on its own.
#[test]
fn hiding_the_parent_list_closes_an_open_submenu() {
    let s = UiScript::new().unwrap();
    load_dropdown_kit(&s);

    s.run("DropDownList1:Show(); DropDownList2:Show();")
        .unwrap();
    assert!(
        s.eval::<bool>("return DropDownList2:IsVisible()").unwrap(),
        "fixture: the submenu must be open before level 1 hides"
    );

    s.run("DropDownList1:Hide();").unwrap();

    assert!(
        !s.eval::<bool>("return DropDownList2:IsVisible()").unwrap(),
        "level 2 was left on screen after its parent list hid — the orphaned submenu"
    );
}

/// **`OPEN_DROPDOWNMENUS` clears when its list hides.** `UnitPopup.xml` writes the level when a
/// unit menu opens and its every-frame driver walks the table; nothing ever removed the entry, so
/// it outlived its menu forever.
///
/// No player-visible symptom is reachable from the stale entry today — `UnitPopup_OnUpdate` returns
/// early unless `DropDownList1` is visible and a `UnitPopupFrame` owns the open menu, and
/// `UIDropDownMenu_AddButton` re-enables every button unconditionally. That is stated here rather
/// than left implied, because both of those guards belong to *other* code and either could move.
/// The divergence from the reference is in this file, so it is fixed and falsified in this file.
#[test]
fn hiding_a_list_clears_its_open_menu_registry_entry() {
    let s = UiScript::new().unwrap();
    load_dropdown_kit(&s);

    s.run(
        r#"
        DropDownList1:Show();
        DropDownList2:Show();
        OPEN_DROPDOWNMENUS[1] = { which = "SELF", unit = "player" };
        OPEN_DROPDOWNMENUS[2] = { which = "SELF", unit = "player" };
    "#,
    )
    .unwrap();

    s.run("DropDownList2:Hide();").unwrap();
    assert!(
        s.eval::<bool>("return OPEN_DROPDOWNMENUS[2] == nil")
            .unwrap(),
        "level 2's registry entry survived its list hiding"
    );

    s.run("DropDownList1:Hide();").unwrap();
    assert!(
        s.eval::<bool>("return OPEN_DROPDOWNMENUS[1] == nil")
            .unwrap(),
        "level 1's registry entry survived its list hiding"
    );
}

/// **The default text height is read off Button1's font, never written as a literal.**
///
/// This is the trap the whole change turned on. `UIDropDownMenu.lua:16` forward-declares
/// `UIDROPDOWNMENU_DEFAULT_TEXT_HEIGHT = nil`, and reading only that line says "the reference
/// leaves it nil, so defining it would be a divergence". The assignment is in the *XML*, in this
/// OnLoad, and it derives the value from the button's own NormalText — so the honest answer is that
/// the name is a real gap whose only correct value is whatever our font object reports.
///
/// The assertion is therefore that it is non-nil and EQUALS Button1's reported height, not that it
/// equals some number: a literal would pass a number check and still be wrong.
#[test]
fn the_default_text_height_is_derived_from_button1_not_a_literal() {
    let s = UiScript::new().unwrap();
    load_dropdown_kit(&s);

    let (height, from_button): (Option<f64>, Option<f64>) = s
        .eval(
            r#"
            local _, buttonHeight = DropDownList1Button1NormalText:GetFont();
            return UIDROPDOWNMENU_DEFAULT_TEXT_HEIGHT, buttonHeight
        "#,
        )
        .unwrap();

    let from_button = from_button.expect("fixture: Button1's NormalText must report a font height");
    assert_eq!(
        height,
        Some(from_button),
        "the constant must be the height Button1's own NormalText reports, not a literal"
    );
}

/// **A button label answers for the font object its button gave it.** The gap the test above
/// uncovered: `extract` overlays the per-state font object onto a *clone* of the region's data
/// every frame, so a `<NormalFont inherits="GameFontHighlightSmall"/>` label painted correctly and
/// still answered `nil` to both `GetFont` and `GetFontObject`. Nothing errored — a FontString with
/// no font of its own is a legal state — so it took the reference's own OnLoad, which reads exactly
/// that, to surface it.
///
/// Asserted on the dropdown row because that is the site that found it, and against
/// `GameFontHighlightSmall` by name because that is what `UIDropDownMenuButtonTemplate` declares.
#[test]
fn a_button_label_reports_the_font_object_its_button_set() {
    let s = UiScript::new().unwrap();
    load_dropdown_kit(&s);

    let (object_height, label_height, label_face): (Option<f64>, Option<f64>, Option<String>) = s
        .eval(
            r#"
            local _, objectHeight = GameFontHighlightSmall:GetFont();
            local face, height = DropDownList1Button1NormalText:GetFont();
            return objectHeight, height, face
        "#,
        )
        .unwrap();

    assert_eq!(
        label_height, object_height,
        "the row label must report GameFontHighlightSmall's height, not nil"
    );
    assert!(
        label_face.is_some_and(|f| f.ends_with(".TTF")),
        "the row label must report a real font face through its button's font object"
    );
    assert!(
        s.eval::<bool>(
            "return DropDownList1Button1NormalText:GetFontObject() == GameFontHighlightSmall"
        )
        .unwrap(),
        "the label must hand back the font OBJECT, which is what the corpus indexes immediately"
    );
}
