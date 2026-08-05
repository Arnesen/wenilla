//! The shipped pet frame, driven end to end (decision 0990) — `UnitFrames.xml`'s
//! `BenillaPetFrame` over synthetic `"pet"` snapshots and the events the app's feed fires.
//!
//! The frame's whole job is to be right about **which unit an event names**, and that is what most
//! of these test: `UNIT_PET` names the OWNER (`arg1 == "player"`, wow-re §9), every other `UNIT_*`
//! names the pet itself, and a frame that mixes the two repaints off the player's health.

use benilla_ui::script::{AuraState, QuadContent, ScriptValue, UiScript, UnitState};

/// Load one shipped `assets/ui/<file>`, panicking on any loader error (the unit-frame tests'
/// loader).
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

/// The pet frame's production load prefix — it is parented to `BenillaPlayerFrame`, so the whole
/// unit-frame file comes with it, and the tooltip/dropdown kit its hover and its neighbours need.
fn load_pet_frame() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UIParent.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "UIDropDownMenu.xml");
    load_xml(&s, "UnitPopup.xml");
    load_xml(&s, "UnitFrames.xml");
    s
}

/// A pet snapshot. `max_power == 0` is the powerless pet (a skeleton) — the leg that swaps the art.
fn pet(name: &str, health: u32, power: u32, max_power: u32, power_type: u8) -> UnitState {
    UnitState {
        exists: true,
        name: Some(name.into()),
        health,
        max_health: 100,
        level: 60,
        power_type,
        power,
        max_power,
        ..UnitState::default()
    }
}

/// Every texture path the UI actually **draws** this frame, and its vertex tint.
///
/// The engine ships `SetTexture`/`SetVertexColor` and no getters (the reference's `GetTexture`
/// exists but nothing in the shipped XML reads it), so these assertions go through the render path
/// the unit-frame tests already use — which is the honest question anyway: not what the region was
/// told, but what comes out the other end.
fn drawn(s: &mut UiScript) -> Vec<(String, Option<[f32; 4]>)> {
    s.resolve();
    s.extract()
        .into_iter()
        .filter_map(|q| match q.content {
            QuadContent::Texture {
                path: Some(p),
                color,
                ..
            } => Some((p, color)),
            _ => None,
        })
        .collect()
}

/// Is exactly this texture drawn? Exact, not `contains` — `UI-SmallTargetingFrame` is a prefix of
/// `UI-SmallTargetingFrame-NoMana`, and the whole point of the art-swap test is telling them apart.
fn draws(s: &mut UiScript, path: &str) -> bool {
    drawn(s).iter().any(|(p, _)| p == path)
}

fn debuff(spell_id: u32, name: &str, count: u8) -> AuraState {
    AuraState {
        spell_id,
        name: Some(name.into()),
        icon: Some(format!("Interface\\Icons\\Spell_{spell_id}")),
        count,
        debuff_type: None,
        // No unit but yourself carries a duration on the 1.12 wire (decision 0257 B6).
        duration: 0.0,
        expiration_time: 0.0,
        helpful: false,
        cancelable: false,
    }
}

/// Summon → the frame appears with the pet's name, health and power; dismiss → it hides again.
/// `UNIT_PET` is the only wire either edge has, which is why it carries them both.
#[test]
fn the_pet_frame_appears_on_a_summon_and_leaves_on_a_dismiss() {
    let mut s = load_pet_frame();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        !s.eval::<bool>("return BenillaPetFrame:IsVisible()")
            .unwrap(),
        "no pet, no frame"
    );

    s.set_unit("pet", Some(pet("Grimjaw", 72, 45, 80, 0)));
    s.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);

    let ok: bool = s
        .eval(
            r#"
            local hb, mb = BenillaPetFrameHealthBar, BenillaPetFrameManaBar
            local _, hmax = hb:GetMinMaxValues()
            local _, mmax = mb:GetMinMaxValues()
            return BenillaPetFrame:IsVisible()
               and BenillaPetFrameTextureFrameName:GetText() == "Grimjaw"
               and hb:GetValue() == 72 and hmax == 100
               and mb:GetValue() == 45 and mmax == 80 and mb:IsVisible()
            "#,
        )
        .unwrap();
    assert!(ok, "the summoned pet's name, health and power all draw");

    // The dismiss: the token clears and UNIT_PET fires, exactly as `feed_pet_unit` does it.
    s.set_unit("pet", None);
    s.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);
    assert!(!s
        .eval::<bool>("return BenillaPetFrame:IsVisible()")
        .unwrap());
}

/// **The two argument gates.** `UNIT_PET` names the OWNER, so `arg1 == "pet"` on it is not our
/// event; every other `UNIT_*` names the pet, so `arg1 == "player"` on those is not ours either.
/// A frame that has these backwards paints the player's health onto the pet's bar — which is why
/// this drives each event with the WRONG token and asserts nothing moved.
#[test]
fn the_frame_answers_only_the_events_that_name_its_own_unit() {
    let mut s = load_pet_frame();
    s.set_unit("pet", Some(pet("Grimjaw", 72, 45, 80, 0)));
    s.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);
    assert!(s
        .eval::<bool>("return BenillaPetFrame:IsVisible()")
        .unwrap());

    // A health change on the PLAYER must not touch the pet's bar.
    s.set_unit("pet", Some(pet("Grimjaw", 5, 45, 80, 0)));
    s.fire_event("UNIT_HEALTH", vec![ScriptValue::Str("player".into())]);
    assert_eq!(
        s.eval::<f64>("return BenillaPetFrameHealthBar:GetValue()")
            .unwrap(),
        72.0,
        "a UNIT_HEALTH for \"player\" is not the pet's event"
    );
    // …and the same event naming the pet does.
    s.fire_event("UNIT_HEALTH", vec![ScriptValue::Str("pet".into())]);
    assert_eq!(
        s.eval::<f64>("return BenillaPetFrameHealthBar:GetValue()")
            .unwrap(),
        5.0
    );

    // UNIT_PET is the mirror image: it names the owner, so a "pet" arg1 is not ours.
    s.set_unit("pet", None);
    s.fire_event("UNIT_PET", vec![ScriptValue::Str("pet".into())]);
    assert!(
        s.eval::<bool>("return BenillaPetFrame:IsVisible()")
            .unwrap(),
        "UNIT_PET names the OWNER — a \"pet\" arg1 is somebody else's event"
    );
    s.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);
    assert!(!s
        .eval::<bool>("return BenillaPetFrame:IsVisible()")
        .unwrap());
}

/// The reference's art swap (`PetFrame.lua`'s Update): a pet with no power wears the `-NoMana`
/// plate, which has no mana rail to leave empty — and the bar hides with it. A hunter's FOCUS pet
/// takes the plain plate and the focus colour, which is the case the mana-coloured default would
/// get wrong.
#[test]
fn a_powerless_pet_wears_the_no_mana_plate() {
    let mut s = load_pet_frame();

    const PLAIN: &str = "Interface\\TargetingFrame\\UI-SmallTargetingFrame";
    const NO_MANA: &str = "Interface\\TargetingFrame\\UI-SmallTargetingFrame-NoMana";

    // Focus (power type 2) — a hunter's pet: plain art, orange bar.
    s.set_unit("pet", Some(pet("Boar", 100, 60, 100, 2)));
    s.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);
    let (visible, r, g, b) = s
        .eval::<(bool, f64, f64, f64)>(
            "local r, g, b = BenillaPetFrameManaBar:GetStatusBarColor() \
             return BenillaPetFrameManaBar:IsVisible(), r, g, b",
        )
        .unwrap();
    assert!(visible);
    assert_eq!((r, g, b), (1.0, 0.5, 0.25), "FOCUS, not the mana default");
    assert!(draws(&mut s, PLAIN), "the plate with a mana rail");
    assert!(!draws(&mut s, NO_MANA));

    // A skeleton: no power at all.
    s.set_unit("pet", Some(pet("Skeleton", 100, 0, 0, 0)));
    s.fire_event("UNIT_MAXPOWER", vec![ScriptValue::Str("pet".into())]);
    assert!(
        !s.eval::<bool>("return BenillaPetFrameManaBar:IsVisible()")
            .unwrap(),
        "no power bar on the plate that has no rail for it"
    );
    assert!(draws(&mut s, NO_MANA), "…and the plate swaps with it");
    assert!(!draws(&mut s, PLAIN));
}

/// `PET_ATTACK_START`/`PET_ATTACK_STOP` show and hide the flashing attack overlay, and neither
/// repaints the rest of the frame (they are the ref's own early returns).
#[test]
fn the_attack_overlay_follows_its_own_two_events() {
    let mut s = load_pet_frame();
    s.set_unit("pet", Some(pet("Grimjaw", 72, 45, 80, 0)));
    s.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);
    assert!(!s
        .eval::<bool>("return BenillaPetFrameAttackMode:IsVisible()")
        .unwrap());

    s.fire_event("PET_ATTACK_START", vec![]);
    assert!(s
        .eval::<bool>("return BenillaPetFrameAttackMode:IsVisible()")
        .unwrap());

    // The ref's OnUpdate ramps the overlay's tint alpha down from 1 on the opening (sign −1) leg.
    // Two ticks so the ramp has visibly moved, and the range guard catches the classic transcription
    // slip: the ref's constants are 0..255 and the API takes 0..1.
    s.tick(0.1);
    s.tick(0.1);
    let tint = drawn(&mut s)
        .into_iter()
        .find(|(p, _)| p.contains("UI-Player-AttackStatus"))
        .and_then(|(_, c)| c)
        .expect("the attack overlay draws while shown");
    assert!(
        (0.0..=1.0).contains(&tint[3]),
        "the pulse stays inside the alpha range (got {})",
        tint[3]
    );
    assert!(tint[3] < 1.0, "…and it has actually ramped off full");

    s.fire_event("PET_ATTACK_STOP", vec![]);
    assert!(!s
        .eval::<bool>("return BenillaPetFrameAttackMode:IsVisible()")
        .unwrap());
}

/// The four debuff buttons fill from `UnitDebuff("pet", i)` in order, show a stack count only above
/// 1, and the unused ones hide. `UNIT_AURA("pet")` is the wire.
#[test]
fn the_debuff_row_fills_from_the_pets_own_auras() {
    let mut s = load_pet_frame();
    s.set_unit("pet", Some(pet("Grimjaw", 72, 45, 80, 0)));
    s.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);

    s.set_auras(
        "pet",
        Some(vec![debuff(1000, "Rend", 1), debuff(1001, "Sunder", 3)]),
    );
    s.fire_event("UNIT_AURA", vec![ScriptValue::Str("pet".into())]);

    let ok: bool = s
        .eval(
            r#"
            return BenillaPetFrameDebuff1:IsVisible()
               and BenillaPetFrameDebuff2:IsVisible()
               and not BenillaPetFrameDebuff3:IsVisible()
               and not BenillaPetFrameDebuff4:IsVisible()
               and not BenillaPetFrameDebuff1Count:IsVisible()
               and BenillaPetFrameDebuff2Count:IsVisible()
               and BenillaPetFrameDebuff2Count:GetText() == "3"
            "#,
        )
        .unwrap();
    assert!(ok, "two debuffs draw, a stack count shows only above 1");
    // …and the icons are the pet's own auras, in the pet's own order.
    assert!(draws(&mut s, "Interface\\Icons\\Spell_1000"));
    assert!(draws(&mut s, "Interface\\Icons\\Spell_1001"));

    // They clear when the auras do.
    s.set_auras("pet", Some(vec![]));
    s.fire_event("UNIT_AURA", vec![ScriptValue::Str("pet".into())]);
    assert!(!s
        .eval::<bool>("return BenillaPetFrameDebuff1:IsVisible()")
        .unwrap());
}

/// Left-clicking the frame targets the pet — the ref's `PetFrame_OnClick` bottom leg, and the one
/// the app answers in `target::click::target_unit_requests`' `"pet"` arm.
#[test]
fn left_clicking_the_pet_frame_targets_it() {
    let mut s = load_pet_frame();
    s.set_unit("pet", Some(pet("Grimjaw", 72, 45, 80, 0)));
    s.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);

    s.run("BenillaPetFrame_OnClick(\"LeftButton\")").unwrap();
    assert_eq!(s.take_target_requests(), vec!["pet".to_string()]);

    // The right button is the deferred PET menu — it must do nothing at all, not target.
    s.run("BenillaPetFrame_OnClick(\"RightButton\")").unwrap();
    assert!(s.take_target_requests().is_empty());
}

/// The layering law (decision 0884), the pet frame's copy of the player/target/party tests: the
/// frame art must draw OVER the bar fills, which only the frame LEVEL can hold — the draw layer is
/// bucket-wide. A "simplification" back to declaration order puts the bars on top as pasted slabs.
#[test]
fn the_pet_art_paints_over_the_bars() {
    let s = load_pet_frame();
    let level: (i64, i64, i64) = s
        .eval(
            "return BenillaPetFrameTextureFrame:GetFrameLevel(), \
                    BenillaPetFrameHealthBar:GetFrameLevel(), \
                    BenillaPetFrameManaBar:GetFrameLevel()",
        )
        .unwrap();
    let (art, health, mana) = level;
    assert_eq!(health, art - 1, "the health bar sits under the art");
    assert_eq!(mana, art - 1, "and so does the mana bar");
}
