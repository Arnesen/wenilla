//! The tooltip ANCHOR law over the real shipped XMLs — where each hover SEATS the plate.
//!
//! The world/unit-frame/action-bar rows of the law all route through the ref's
//! `GameTooltip_SetDefaultAnchor` (ref GameTooltip.lua l.73-77): the screen's bottom-right
//! corner, `-CONTAINER_OFFSET_X - 13` in from the right, `CONTAINER_OFFSET_Y` up from the
//! bottom. These tests exist because the wiring had two silent holes only the live game showed
//! (the world tooltip parked ON the character): `GameTooltip.xml` never wired
//! `<OnTooltipSetDefaultAnchor>`, and the `UIParent` GLOBAL the ref handler passes didn't exist
//! — engine tests stubbed the handler, so nothing asserted the real files' geometry. Everything
//! here loads the shipped XMLs and asserts resolved rects / anchors, never a stub.

use benilla_ui::script::{AuraState, ScriptValue, UiScript, UnitState};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error (the bag/panel
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

/// A 1024×768 screen with the anchor law's three fixed files (fonts, the real UIParent, the
/// real GameTooltip) plus `extra`. `CONTAINER_OFFSET_X/Y` hold their UIParent.xml load values
/// (0 / 70) — the manage pass only runs from the app's post-load bootstrap, so the expected
/// default corner in every test here is x = 1024−13 = 1011, y = 70.
fn harness(extra: &[&str]) -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UIParent.xml");
    load_xml(&s, "GameTooltip.xml");
    for f in extra {
        load_xml(&s, f);
    }
    s
}

fn wolf() -> UnitState {
    UnitState {
        exists: true,
        name: Some("Timber Wolf".into()),
        health: 30,
        max_health: 50,
        level: 10,
        reaction: 2,
        creature_type_name: Some("Beast".into()),
        ..Default::default()
    }
}

/// UIParent is a real, named, full-screen frame (ref UIParent.xml l.5) — the Lua global
/// resolves, and its rect IS the screen.
#[test]
fn uiparent_is_a_real_full_screen_frame() {
    let mut s = harness(&[]);
    s.resolve();
    let ok: bool = s
        .eval(
            "return UIParent ~= nil and UIParent:GetName() == \"UIParent\" \
               and UIParent:GetLeft() == 0 and UIParent:GetBottom() == 0 \
               and UIParent:GetRight() == 1024 and UIParent:GetTop() == 768",
        )
        .unwrap();
    assert!(ok, "UIParent exists and fills the screen");
    assert!(s.errors().is_empty(), "errors: {:?}", s.errors());
}

/// The world mouseover seats the plate at the DEFAULT corner — the engine fires
/// `OnTooltipSetDefaultAnchor`, the shipped handler (ref GameTooltip.lua l.73-77 via
/// ref GameTooltipTemplate.xml l.617-619) anchors BOTTOMRIGHT to UIParent at (−13, 70).
/// THE regression test for the "tooltip on my character" bug: without the wiring the plate
/// kept its load-time position instead.
#[test]
fn world_hover_seats_the_default_corner() {
    let mut s = harness(&[]);
    s.set_unit("mouseover", Some(wolf()));
    assert!(s.world_tooltip_unit("mouseover"), "the hover shows");
    assert!(s.errors().is_empty(), "hover errors: {:?}", s.errors());
    s.resolve();
    let ok: bool = s
        .eval(
            "return GameTooltip:IsVisible() \
               and GameTooltip:GetRight() == 1011 and GameTooltip:GetBottom() == 70",
        )
        .unwrap();
    assert!(ok, "world tooltip sits at the screen's bottom-right corner");
}

/// Unit-frame hovers take the SAME default corner (ref UnitFrame_OnEnter l.56 calls
/// GameTooltip_SetDefaultAnchor, not an owner anchor), and the plate FADES on leave
/// (ref UnitFrame_OnLeave l.88-93: FadeOut, not Hide).
#[test]
fn unit_frame_hover_takes_the_default_corner_and_fades_on_leave() {
    // The kit + popups precede the unit frames (their DropDown children's OnLoad), app order.
    let mut s = harness(&["UIDropDownMenu.xml", "UnitPopup.xml", "UnitFrames.xml"]);
    s.set_unit("target", Some(wolf()));
    s.run("BenillaUnitFrame_OnEnter(BenillaTargetFrame)")
        .unwrap();
    assert!(s.errors().is_empty(), "hover errors: {:?}", s.errors());
    s.resolve();
    let ok: bool = s
        .eval(
            "return GameTooltip:IsVisible() \
               and GameTooltip:GetRight() == 1011 and GameTooltip:GetBottom() == 70 \
               and GameTooltip:IsOwned(BenillaTargetFrame)",
        )
        .unwrap();
    assert!(
        ok,
        "unit-frame tooltip sits at the default corner, owned by the frame"
    );
    // Leave: the fade ramp (TOOLTIP_FADE_SECS), then hidden.
    s.run("BenillaUnitFrame_OnLeave(BenillaTargetFrame)")
        .unwrap();
    s.tick(0.6);
    let hidden: bool = s.eval("return not GameTooltip:IsShown()").unwrap();
    assert!(hidden, "unit-frame tooltip fades out on leave");
    assert!(s.errors().is_empty(), "errors: {:?}", s.errors());
}

/// Action-bar hovers take the default corner too — ref ActionButton_SetTooltip l.366-372
/// branches on the UberTooltips CVar, whose stock default is "1" (byte-read from WoW.exe
/// 0x48fdd9 / default string 0x82e748; see ActionBar.xml). An empty slot renders nothing,
/// but the anchor must already be seated — asserted through GetPoint, resolved rect or not.
#[test]
fn action_button_hover_takes_the_default_corner() {
    let s = harness(&["Cooldown.xml", "ActionBar.xml"]);
    s.run("BenillaActionButton_OnEnter(BenillaActionButton3)")
        .unwrap();
    assert!(s.errors().is_empty(), "hover errors: {:?}", s.errors());
    let ok: bool = s
        .eval(
            "local p, rel, rp, x, y = GameTooltip:GetPoint() \
             return p == \"BOTTOMRIGHT\" and rel ~= nil and rel:GetName() == \"UIParent\" \
               and rp == \"BOTTOMRIGHT\" and x == -13 and y == 70",
        )
        .unwrap();
    assert!(
        ok,
        "action-button hover anchors the plate to UIParent's bottom-right"
    );
}

/// Buff hovers hang BELOW the button — ref BuffFrame.xml l.37 is ANCHOR_BOTTOMLEFT (the buff
/// row lives at the screen's top-right): the tooltip's TOPRIGHT seats on the button's
/// BOTTOMLEFT.
#[test]
fn buff_hover_hangs_below_left_of_the_button() {
    let mut s = harness(&["Cooldown.xml", "ActionBar.xml", "BuffFrame.xml"]);
    s.set_auras(
        "player",
        Some(vec![AuraState {
            spell_id: 1459,
            name: Some("Arcane Intellect".into()),
            icon: Some("Interface\\Icons\\Spell_Holy_MagicalSentry".into()),
            count: 1,
            debuff_type: None,
            duration: 1800.0,
            expiration_time: 1800.0,
            helpful: true,
            cancelable: true,
        }]),
    );
    s.fire_event("UNIT_AURA", vec![ScriptValue::Str("player".into())]);
    s.resolve();
    s.run("BenillaBuffButton_OnEnter(BuffButton0)").unwrap();
    assert!(s.errors().is_empty(), "hover errors: {:?}", s.errors());
    let ok: bool = s
        .eval(
            "local p, rel, rp = GameTooltip:GetPoint() \
             return p == \"TOPRIGHT\" and rel ~= nil and rel:GetName() == \"BuffButton0\" \
               and rp == \"BOTTOMLEFT\"",
        )
        .unwrap();
    assert!(
        ok,
        "buff tooltip hangs its TOPRIGHT on the button's BOTTOMLEFT"
    );
}
