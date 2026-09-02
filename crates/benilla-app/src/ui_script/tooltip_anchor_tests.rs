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

use benilla_ui::script::{AuraState, UiScript, UnitState};

use super::test_ui::load_ui as load_xml;

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
/// GameTooltip_SetDefaultAnchor, not an owner anchor).
///
/// Leave drops the plate AT ONCE, not on the fade ramp: `UnitFrame_OnLeave` l.84-88 branches on
/// `SHOW_NEWBIE_TIPS`, and 1.12's default is on (0661/0663) — `FadeOut` is the tips-off arm. The
/// world mouseover keeps the ramp; this is the unit *frame*.
#[test]
fn unit_frame_hover_takes_the_default_corner_and_drops_on_leave() {
    // The kit + popups precede the unit frames (their DropDown children's OnLoad), app order.
    let mut s = harness(&["UIDropDownMenu.xml", "UnitPopup.xml", "UnitFrames.xml"]);
    s.set_unit("target", Some(wolf()));
    s.run("BenillaUnitFrame_OnEnter(TargetFrame)").unwrap();
    assert!(s.errors().is_empty(), "hover errors: {:?}", s.errors());
    s.resolve();
    let ok: bool = s
        .eval(
            "return GameTooltip:IsVisible() \
               and GameTooltip:GetRight() == 1011 and GameTooltip:GetBottom() == 70 \
               and GameTooltip:IsOwned(TargetFrame)",
        )
        .unwrap();
    assert!(
        ok,
        "unit-frame tooltip sits at the default corner, owned by the frame"
    );
    // A wolf is no player, so the hover is the ordinary unit readout — the fork below never fires.
    assert!(
        s.eval::<String>("return GameTooltipTextLeft1:GetText()")
            .unwrap()
            .contains("Wolf"),
        "a non-player target still gets the unit lines"
    );
    // Leave: gone on the spot, no ramp to wait out.
    s.run("BenillaUnitFrame_OnLeave(TargetFrame)").unwrap();
    let hidden: bool = s.eval("return not GameTooltip:IsShown()").unwrap();
    assert!(
        hidden,
        "unit-frame tooltip hides on leave, it does not fade"
    );
    assert!(s.errors().is_empty(), "errors: {:?}", s.errors());
}

/// The detailed-tooltip fork (ref UnitFrame_OnEnter l.58-67, director-approved 0663): with tips on
/// — the 1.12 default — the frame explains its RIGHT-CLICK MENU and returns BEFORE `SetUnit`, so
/// the unit lines never render. Your own portrait always; another player's whenever they're your
/// target.
///
/// This is the most behaviour-changing thing in the newbie-tip arc, so it is pinned from both
/// sides: the explanation replaces the readout, and a non-player target still gets the readout
/// (the sibling test above).
#[test]
fn your_own_portrait_explains_the_menu_instead_of_showing_your_health() {
    let mut s = harness(&["UIDropDownMenu.xml", "UnitPopup.xml", "UnitFrames.xml"]);
    s.set_unit("player", Some(wolf()));

    s.run("BenillaUnitFrame_OnEnter(PlayerFrame)").unwrap();
    assert_eq!(
        s.eval::<String>("return GameTooltipTextLeft1:GetText()")
            .unwrap(),
        "Party Options",
        "your own frame explains the party menu"
    );
    assert_eq!(
        s.eval::<String>("return GameTooltipTextLeft2:GetText()")
            .unwrap(),
        s.eval::<String>("return NEWBIE_TOOLTIP_PARTYOPTIONS")
            .unwrap()
    );
    assert_eq!(
        s.eval::<i64>("return GameTooltip:NumLines()").unwrap(),
        2,
        "it RETURNS before SetUnit — no health/level lines underneath"
    );

    // A player target takes the other arm. `player` stays a wolf here on purpose: the ref reads
    // UnitIsPlayer on the "target" token alone, never on the hovered frame's own unit.
    s.set_unit(
        "target",
        Some(UnitState {
            is_player: true,
            name: Some("Someone".into()),
            ..wolf()
        }),
    );
    s.run("BenillaUnitFrame_OnEnter(TargetFrame)").unwrap();
    assert_eq!(
        s.eval::<String>("return GameTooltipTextLeft1:GetText()")
            .unwrap(),
        "Player Options",
        "another player's frame explains the player menu"
    );
    assert!(s.errors().is_empty(), "errors: {:?}", s.errors());
}

/// Action-bar hovers take the default corner too — ref ActionButton_SetTooltip l.366-372
/// branches on the UberTooltips CVar, whose stock default is "1" (byte-read from WoW.exe
/// 0x48fdd9 / default string 0x82e748; see `cvars::REGISTERED`). An empty slot renders nothing,
/// but the anchor must already be seated — asserted through GetPoint, resolved rect or not.
///
/// Registered, not merely absent: before B230 the CVar was not in the table at all, so `GetCVar`
/// answered nil and the "1" leg was reached by accident rather than by value. This seeds the real
/// table so the pass means what it says.
#[test]
fn action_button_hover_takes_the_default_corner() {
    let mut s = harness(&["Cooldown.xml", "ActionBar.xml"]);
    s.register_cvars(crate::cvars::REGISTERED.iter().copied());
    s.run("BenillaActionButton_OnEnter(ActionButton3)").unwrap();
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

/// The other leg of that same branch, live since B230 registered the CVar: with `UberTooltips`
/// off, an action button's plate leaves the screen corner and seats BESIDE the button — LEFT for
/// the three bars the reference lists (`MultiBarBottomRight`, `MultiBarRight`, `MultiBarLeft`: the
/// ones at or against the right edge, whose plates have to open toward the centre), RIGHT for
/// everything else, including the main bar. All three exist here since 1219/1500; the set used to
/// be the first alone.
///
/// The two anchors are read off the resolved SetPoint pair, which is what `SetOwner` actually
/// writes: ANCHOR_RIGHT = the plate's BOTTOMLEFT on the button's TOPRIGHT, ANCHOR_LEFT its mirror
/// (`script/tooltip/verbs.rs`).
#[test]
fn ubertooltips_off_seats_action_bar_plates_beside_the_button() {
    let mut s = harness(&["Cooldown.xml", "ActionBar.xml", "MultiBars.xml"]);
    s.register_cvars(crate::cvars::REGISTERED.iter().copied());
    s.set_cvar_engine("UberTooltips", "0");

    let seat = |s: &UiScript| {
        s.eval::<String>(
            "local p, rel, rp = GameTooltip:GetPoint() \
             return p .. \"/\" .. (rel and rel:GetName() or \"?\") .. \"/\" .. rp",
        )
        .unwrap()
    };

    s.run("BenillaActionButton_OnEnter(ActionButton3)").unwrap();
    assert!(
        s.eval::<bool>("return GameTooltip.default == nil").unwrap(),
        "off: the main bar's plate is owner-anchored, not the default corner"
    );
    assert!(
        s.eval::<bool>("return GameTooltip:IsOwned(ActionButton3)")
            .unwrap(),
        "…owned by the button it opened from"
    );
    assert_eq!(
        seat(&s),
        "BOTTOMLEFT/ActionButton3/TOPRIGHT",
        "ANCHOR_RIGHT — the main bar is not in the ref's LEFT set"
    );

    // All three members of the ref's LEFT set, by frame — membership is not gated on visibility,
    // and the two vertical bars are hidden until their option is ticked.
    for bar in ["MultiBarBottomRight", "MultiBarRight", "MultiBarLeft"] {
        s.run(&format!("BenillaActionButton_OnEnter({bar}Button1)"))
            .unwrap();
        assert_eq!(
            seat(&s),
            format!("BOTTOMRIGHT/{bar}Button1/TOPLEFT"),
            "ANCHOR_LEFT — {bar} opens toward screen centre"
        );
    }

    // And the CVar back on restores the corner — the fork is a fork, not a one-way door.
    s.set_cvar_engine("UberTooltips", "1");
    s.run("BenillaActionButton_OnEnter(MultiBarBottomRightButton1)")
        .unwrap();
    assert!(
        s.eval::<bool>("return GameTooltip.default ~= nil").unwrap(),
        "on: back to the screen corner"
    );
    assert!(s.errors().is_empty(), "hover errors: {:?}", s.errors());
}

/// The stance bar's own leg of the same branch (ref BonusActionBarFrame.xml l.40-45 — ANCHOR_RIGHT
/// with no bar fork of its own), pinned separately because it is a different file's handler and a
/// different tooltip verb; it was collapsed to the "1" leg alongside the action bar's and
/// un-collapsed with it.
#[test]
fn ubertooltips_off_seats_stance_plates_beside_the_button() {
    let mut s = harness(&["Cooldown.xml", "ActionBar.xml", "StanceBar.xml"]);
    s.register_cvars(crate::cvars::REGISTERED.iter().copied());
    s.set_shapeshift_forms(vec![benilla_ui::script::ShapeshiftFormView {
        spell_id: 5487,
        name: "Bear Form".into(),
        texture: Some("Interface\\Icons\\Ability_Racial_BearForm".into()),
        active: false,
        castable: true,
        cooldown: None,
    }]);
    s.fire_event("UPDATE_SHAPESHIFT_FORMS", vec![]);
    s.resolve();

    s.run("BenillaShapeshiftButton_OnEnter(ShapeshiftButton1)")
        .unwrap();
    assert!(
        s.eval::<bool>("return GameTooltip.default ~= nil").unwrap(),
        "on (the stock default): the screen corner"
    );

    s.set_cvar_engine("UberTooltips", "0");
    s.run("BenillaShapeshiftButton_OnEnter(ShapeshiftButton1)")
        .unwrap();
    assert_eq!(
        s.eval::<String>(
            "local p, rel, rp = GameTooltip:GetPoint() \
             return p .. \"/\" .. (rel and rel:GetName() or \"?\") .. \"/\" .. rp",
        )
        .unwrap(),
        "BOTTOMLEFT/ShapeshiftButton1/TOPRIGHT",
        "off: ANCHOR_RIGHT, beside the button"
    );
    assert!(s.errors().is_empty(), "hover errors: {:?}", s.errors());
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
            until_cancelled: false,
            channeled: false,
        }]),
    );
    // The reference's own event, which the buff buttons register for (`ui_aura` fires it beside
    // the Era-shaped UNIT_AURA on the same rebuild).
    s.fire_event("PLAYER_AURAS_CHANGED", vec![]);
    s.resolve();
    // Through the template's real `<OnEnter>` — the reference keeps the SetOwner/SetPlayerBuff pair
    // inline there rather than in a named function, so the handler body itself is what this drives.
    // `this` is set by hand because the engine sets it only when it *fires* a handler (RF-0025);
    // calling the compiled function directly does not, and the body reads `this`, not its argument.
    s.run("this = BuffButton0; BuffButton0:GetScript(\"OnEnter\")(BuffButton0)")
        .unwrap();
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
