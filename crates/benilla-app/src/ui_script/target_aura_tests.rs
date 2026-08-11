//! The target frame's aura rows (`assets/ui/UnitFrames.xml`, decision 0255's deferred slice)
//! against the reference's `TargetDebuffButton_Update` law (ref-TargetFrame.lua l.263-387). The
//! XML/Lua is the unit under test; the app-side feed (`crate::ui_aura`'s target half) is stubbed
//! by pushing an [`AuraState`] list through [`UiScript::set_auras`] and firing the events the feed
//! fires — `PLAYER_TARGET_CHANGED` on a switch, `UNIT_AURA "target"` on a list change.
//!
//! Under test: the friend/hostile row swap (buffs first vs debuffs first), the 21→17 shrink when
//! the debuff count reaches the wrap (6, no target-of-target frame), the dispel-tinted border, the
//! stack count, and the hide-on-empty lifecycle. Files load in the app's own order (UnitFrames
//! before BuffFrame), which also pins the `DebuffTypeColor` late-definition contract.

use benilla_ui::script::{AuraState, QuadContent, ScriptValue, UiScript, UnitState};

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

fn harness() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "GameTooltip.xml"); // TOOLTIP_DEFAULT_* (the dropdown kit's MenuBackdrop)
    load_xml(&s, "UIDropDownMenu.xml"); // the unit popups' kit (UnitFrames' DropDown children)
    load_xml(&s, "UnitPopup.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "ActionBar.xml"); // BENILLA_FALLBACK_ICON
    load_xml(&s, "UnitFrames.xml"); // loads BEFORE BuffFrame, as in the app
    load_xml(&s, "BuffFrame.xml"); // DebuffTypeColor
    s
}

/// Target a unit of the given reaction (2 = hostile, 5 = friendly) carrying `auras`.
fn target(s: &mut UiScript, reaction: u8, auras: Vec<AuraState>) {
    s.set_unit(
        "target",
        Some(UnitState {
            exists: true,
            name: Some("Subject".into()),
            health: 40,
            max_health: 40,
            level: 5,
            reaction,
            ..UnitState::default()
        }),
    );
    s.set_auras("target", Some(auras));
    s.fire_event("PLAYER_TARGET_CHANGED", vec![]);
}

fn debuff(spell_id: u32, name: &str, count: u8, debuff_type: Option<&str>) -> AuraState {
    AuraState {
        spell_id,
        name: Some(name.into()),
        icon: Some(format!("Interface\\Icons\\Spell_{spell_id}")),
        count,
        debuff_type: debuff_type.map(Into::into),
        // The 1.12 wire carries no duration for another unit (decision 0257 B6).
        duration: 0.0,
        expiration_time: 0.0,
        helpful: false,
        cancelable: false,
        // Only the PLAYER cache carries `untilCancelled`, and only `GetPlayerBuff` reads it
        // (decision 0257 / `benilla::ui_aura`); a target's rows have no such record.
        until_cancelled: false,
        channeled: false,
    }
}

fn buff(spell_id: u32, name: &str) -> AuraState {
    AuraState {
        spell_id,
        name: Some(name.into()),
        icon: Some(format!("Interface\\Icons\\Spell_{spell_id}")),
        count: 1,
        debuff_type: None,
        duration: 0.0,
        expiration_time: 0.0,
        helpful: true,
        cancelable: false,
        until_cancelled: false,
        channeled: false,
    }
}

fn shown(s: &UiScript, name: &str) -> bool {
    s.eval::<bool>(&format!("return {name}:IsVisible()"))
        .unwrap()
}

/// The first anchor of `name`: (point, relative frame's name, relativePoint, x, y).
fn anchor(s: &UiScript, name: &str) -> (String, String, String, f64, f64) {
    s.eval(&format!(
        r#"local p, rel, rp, x, y = {name}:GetPoint()
           return p, (rel and rel:GetName()) or "<screen>", rp, x, y"#
    ))
    .unwrap()
}

fn size(s: &UiScript, name: &str) -> (f64, f64) {
    s.eval(&format!("return {name}:GetWidth(), {name}:GetHeight()"))
        .unwrap()
}

#[test]
fn a_hostile_target_draws_debuffs_first_with_tint_and_count() {
    let mut s = harness();
    target(
        &mut s,
        2,
        vec![
            debuff(589, "Shadow Word: Pain", 1, Some("Magic")),
            debuff(772, "Rend", 3, None),
            buff(1126, "Mark of the Wild"),
        ],
    );

    assert!(shown(&s, "TargetFrameDebuff1"), "first debuff shows");
    assert!(shown(&s, "TargetFrameDebuff2"), "second debuff shows");
    assert!(
        !shown(&s, "TargetFrameDebuff3"),
        "no third debuff — button hides"
    );
    assert!(shown(&s, "TargetFrameBuff1"), "the buff shows");
    assert!(!shown(&s, "TargetFrameBuff2"), "no second buff");

    // Hostile: the debuff row opens at the frame's BOTTOMLEFT (5,32); buffs seat under Debuff7
    // (the not-shown target-of-target leg, ref l.329).
    let (p, rel, rp, x, y) = anchor(&s, "TargetFrameDebuff1");
    assert_eq!(
        (p.as_str(), rel.as_str(), rp.as_str(), x, y),
        ("TOPLEFT", "TargetFrame", "BOTTOMLEFT", 5.0, 32.0),
        "hostile: debuffs first"
    );
    let (_, rel, _, _, _) = anchor(&s, "TargetFrameBuff1");
    assert_eq!(rel, "TargetFrameDebuff7", "hostile: buffs below row 2");

    // Under the wrap (2 < 6): full 21px icons, 23px borders.
    assert_eq!(size(&s, "TargetFrameDebuff1"), (21.0, 21.0));
    assert_eq!(size(&s, "TargetFrameDebuff1Border"), (23.0, 23.0));
    assert_eq!(size(&s, "TargetFrameBuff1"), (21.0, 21.0));

    // Stack count shows only above 1.
    assert_eq!(
        s.eval::<String>(r#"return tostring((TargetFrameDebuff2Count:GetText()) or "")"#)
            .unwrap(),
        "3"
    );
    assert_eq!(
        s.eval::<String>(r#"return tostring((TargetFrameDebuff1Count:GetText()) or "")"#)
            .unwrap(),
        ""
    );

    // The Magic-tinted border drew (DebuffTypeColor["Magic"] = 0.20, 0.60, 1.00); Rend's untyped
    // border wears the "none" red (0.80, 0, 0).
    s.resolve();
    let tints: Vec<[f32; 4]> = s
        .extract()
        .into_iter()
        .filter_map(|q| match q.content {
            QuadContent::Texture {
                path: Some(p),
                color: Some(c),
                ..
            } if p.contains("UI-Debuff-Overlays") => Some(c),
            _ => None,
        })
        .collect();
    assert!(
        tints
            .iter()
            .any(|c| (c[0] - 0.20).abs() < 1e-3 && (c[1] - 0.60).abs() < 1e-3),
        "a Magic-tinted border drew, got {tints:?}"
    );
    assert!(
        tints
            .iter()
            .any(|c| (c[0] - 0.80).abs() < 1e-3 && c[1].abs() < 1e-3),
        "an untyped border wears the none-red, got {tints:?}"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

#[test]
fn a_friendly_target_puts_the_buff_row_first() {
    let mut s = harness();
    target(
        &mut s,
        5,
        vec![
            buff(1126, "Mark of the Wild"),
            debuff(589, "Shadow Word: Pain", 1, Some("Magic")),
        ],
    );

    let (p, rel, rp, x, y) = anchor(&s, "TargetFrameBuff1");
    assert_eq!(
        (p.as_str(), rel.as_str(), rp.as_str(), x, y),
        ("TOPLEFT", "TargetFrame", "BOTTOMLEFT", 5.0, 32.0),
        "friendly: buffs first"
    );
    let (p, rel, rp, x, y) = anchor(&s, "TargetFrameDebuff1");
    assert_eq!(
        (p.as_str(), rel.as_str(), rp.as_str(), x, y),
        ("TOPLEFT", "TargetFrameBuff1", "BOTTOMLEFT", 0.0, -2.0),
        "friendly: debuffs under the buff row"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

#[test]
fn reaching_the_wrap_shrinks_the_first_row_to_17px() {
    let mut s = harness();
    let debuffs: Vec<AuraState> = (0..6)
        .map(|i| debuff(1000 + i, &format!("D{i}"), 1, None))
        .collect();
    target(&mut s, 2, debuffs);

    // 6 debuffs ≥ wrap (6): 17px icons, 19px borders — the reference resizes only the FIRST row.
    assert_eq!(size(&s, "TargetFrameDebuff1"), (17.0, 17.0));
    assert_eq!(size(&s, "TargetFrameDebuff1Border"), (19.0, 19.0));
    assert_eq!(size(&s, "TargetFrameBuff1"), (17.0, 17.0));

    // Dropping below the wrap grows them back — the feed re-fires UNIT_AURA on the change.
    s.set_auras("target", Some(vec![debuff(1000, "D0", 1, None)]));
    s.fire_event("UNIT_AURA", vec![ScriptValue::Str("target".into())]);
    assert_eq!(size(&s, "TargetFrameDebuff1"), (21.0, 21.0));
    assert_eq!(size(&s, "TargetFrameDebuff1Border"), (23.0, 23.0));
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

#[test]
fn clearing_the_list_or_the_target_hides_the_buttons() {
    let mut s = harness();
    target(&mut s, 2, vec![debuff(589, "Pain", 1, Some("Magic"))]);
    assert!(shown(&s, "TargetFrameDebuff1"));

    // The last debuff expires: the feed pushes the emptied list and re-fires.
    s.set_auras("target", Some(vec![]));
    s.fire_event("UNIT_AURA", vec![ScriptValue::Str("target".into())]);
    assert!(
        !shown(&s, "TargetFrameDebuff1"),
        "an emptied list hides the button"
    );

    // Deselect: the frame (and every child button) hides; the token clears without a UNIT_AURA.
    target(&mut s, 2, vec![debuff(589, "Pain", 1, Some("Magic"))]);
    assert!(shown(&s, "TargetFrameDebuff1"));
    s.set_unit("target", None);
    s.set_auras("target", None);
    s.fire_event("PLAYER_TARGET_CHANGED", vec![]);
    assert!(!shown(&s, "TargetFrameDebuff1"), "no target, no buttons");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
