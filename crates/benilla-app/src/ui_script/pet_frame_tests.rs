//! The shipped pet frame, driven end to end (decision 0990) — `UnitFrames.xml`'s
//! `PetFrame` over synthetic `"pet"` snapshots and the events the app's feed fires.
//!
//! The frame's whole job is to be right about **which unit an event names**, and that is what most
//! of these test: `UNIT_PET` names the OWNER (`arg1 == "player"`, wow-re §9), every other `UNIT_*`
//! names the pet itself, and a frame that mixes the two repaints off the player's health.

use benilla_ui::script::{
    AuraState, QuadContent, ScriptValue, SelectionRequest, UiScript, UnitState,
};

use super::test_ui::load_ui as load_xml;

/// The pet frame's production load prefix — it is parented to `PlayerFrame`, so the whole
/// unit-frame file comes with it, and the tooltip/dropdown kit its hover and its neighbours need.
///
/// **A player is seated, and that is load-bearing rather than tidy.** `PetFrame` really is a CHILD
/// of `PlayerFrame` — our `UnitFrames.xml:1792` says `parent="PlayerFrame"` and the reference's
/// `PetFrame.xml:4` says the same — and `UnitFrame_Update` hides a frame whose unit does not exist.
/// So a fixture with a pet and no player gives `PlayerFrame:Hide()`, and a hidden parent means an
/// invisible pet frame. That is the reference's own structure working correctly; a pet without a
/// player is a state the game cannot be in.
///
/// (It did not matter until the loader learned the `parent=` attribute, which it had been ignoring:
/// before that `PetFrame` was a top-level frame and nothing could hide it from above.)
fn load_pet_frame() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UIParent.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "UIDropDownMenu.xml");
    load_xml(&s, "UnitPopup.xml");
    load_xml(&s, "UnitFrames.xml");
    s.set_unit(
        "player",
        Some(UnitState {
            exists: true,
            name: Some("Tri".into()),
            health: 100,
            max_health: 100,
            level: 60,
            power_type: 0,
            power: 100,
            max_power: 100,
            ..UnitState::default()
        }),
    );
    s.fire_event("UNIT_HEALTH", vec![ScriptValue::Str("player".into())]);
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
        until_cancelled: false,
        channeled: false,
    }
}

/// Summon → the frame appears with the pet's name, health and power; dismiss → it hides again.
/// `UNIT_PET` is the only wire either edge has, which is why it carries them both.
#[test]
fn the_pet_frame_appears_on_a_summon_and_leaves_on_a_dismiss() {
    let mut s = load_pet_frame();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    assert!(
        !s.eval::<bool>("return PetFrame:IsVisible()").unwrap(),
        "no pet, no frame"
    );

    s.set_unit("pet", Some(pet("Grimjaw", 72, 45, 80, 0)));
    s.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);

    let ok: bool = s
        .eval(
            r#"
            local hb, mb = PetFrameHealthBar, PetFrameManaBar
            local _, hmax = hb:GetMinMaxValues()
            local _, mmax = mb:GetMinMaxValues()
            return PetFrame:IsVisible()
               and PetName:GetText() == "Grimjaw"
               and hb:GetValue() == 72 and hmax == 100
               and mb:GetValue() == 45 and mmax == 80 and mb:IsVisible()
            "#,
        )
        .unwrap();
    assert!(ok, "the summoned pet's name, health and power all draw");

    // The dismiss: the token clears and UNIT_PET fires, exactly as `feed_pet_unit` does it.
    s.set_unit("pet", None);
    s.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);
    assert!(!s.eval::<bool>("return PetFrame:IsVisible()").unwrap());
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
    assert!(s.eval::<bool>("return PetFrame:IsVisible()").unwrap());

    // A health change on the PLAYER must not touch the pet's bar.
    s.set_unit("pet", Some(pet("Grimjaw", 5, 45, 80, 0)));
    s.fire_event("UNIT_HEALTH", vec![ScriptValue::Str("player".into())]);
    assert_eq!(
        s.eval::<f64>("return PetFrameHealthBar:GetValue()")
            .unwrap(),
        72.0,
        "a UNIT_HEALTH for \"player\" is not the pet's event"
    );
    // …and the same event naming the pet does.
    s.fire_event("UNIT_HEALTH", vec![ScriptValue::Str("pet".into())]);
    assert_eq!(
        s.eval::<f64>("return PetFrameHealthBar:GetValue()")
            .unwrap(),
        5.0
    );

    // UNIT_PET is the mirror image: it names the owner, so a "pet" arg1 is not ours.
    s.set_unit("pet", None);
    s.fire_event("UNIT_PET", vec![ScriptValue::Str("pet".into())]);
    assert!(
        s.eval::<bool>("return PetFrame:IsVisible()").unwrap(),
        "UNIT_PET names the OWNER — a \"pet\" arg1 is somebody else's event"
    );
    s.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);
    assert!(!s.eval::<bool>("return PetFrame:IsVisible()").unwrap());
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
            "local r, g, b = PetFrameManaBar:GetStatusBarColor() \
             return PetFrameManaBar:IsVisible(), r, g, b",
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
        !s.eval::<bool>("return PetFrameManaBar:IsVisible()")
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
        .eval::<bool>("return PetAttackModeTexture:IsVisible()")
        .unwrap());

    s.fire_event("PET_ATTACK_START", vec![]);
    assert!(s
        .eval::<bool>("return PetAttackModeTexture:IsVisible()")
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
        .eval::<bool>("return PetAttackModeTexture:IsVisible()")
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
            return PetFrameDebuff1:IsVisible()
               and PetFrameDebuff2:IsVisible()
               and not PetFrameDebuff3:IsVisible()
               and not PetFrameDebuff4:IsVisible()
               and not PetFrameDebuff1Count:IsVisible()
               and PetFrameDebuff2Count:IsVisible()
               and PetFrameDebuff2Count:GetText() == "3"
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
        .eval::<bool>("return PetFrameDebuff1:IsVisible()")
        .unwrap());
}

/// Left-clicking the frame targets the pet — the ref's `PetFrame_OnClick` bottom leg, and the one
/// the app answers in `target::click::target_unit_requests`' `"pet"` arm.
#[test]
fn left_clicking_the_pet_frame_targets_it() {
    let mut s = load_pet_frame();
    s.set_unit("pet", Some(pet("Grimjaw", 72, 45, 80, 0)));
    s.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);

    s.run("PetFrame_OnClick(\"LeftButton\")").unwrap();
    assert_eq!(
        s.take_selection_requests(),
        vec![SelectionRequest::Unit("pet".into())]
    );

    // The right button is the deferred PET menu — it must do nothing at all, not target.
    s.run("PetFrame_OnClick(\"RightButton\")").unwrap();
    assert!(s.take_selection_requests().is_empty());
}

/// **All three legs of `PetFrame_OnClick` survive the click** — B208's "dropping food from the bag
/// onto the pet doesn't feed" (decision 1055).
///
/// The handler transcribed the reference's three legs correctly from the day it shipped, but two of
/// the globals it calls — `DropItemOnUnit` and `SpellTargetUnit` — were never registered, so the
/// middle leg called a nil value and the whole handler errored out. Nothing fed, and nothing said
/// why. The bottom-leg test above could not catch it: it only ever exercises the `else`.
///
/// So this walks the legs the reference's own order picks between them (`SpellIsTargeting()` first,
/// then `CursorHasItem()`, then plain target — `PetFrame.lua:114-129`), and asserts each *runs*.
/// The feed itself is the app's half, gated and sent in `ui_action::drop_item`; what the VM owes is
/// a queued token and no error.
#[test]
fn every_leg_of_the_pet_frame_click_reaches_a_live_binding() {
    let mut s = load_pet_frame();
    s.set_unit("pet", Some(pet("Grimjaw", 72, 45, 80, 0)));
    s.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);

    // The three globals the handler reaches for must all exist. Named individually so a failure
    // says which one went missing rather than just "attempt to call a nil value".
    for global in [
        "SpellIsTargeting",
        "CursorHasItem",
        "DropItemOnUnit",
        "SpellTargetUnit",
    ] {
        assert!(
            s.eval::<bool>(&format!("return _G[\"{global}\"] ~= nil"))
                .unwrap_or(false),
            "{global} is not registered — the pet frame's click calls it"
        );
    }

    // The middle leg, for real: put food in the backpack, pick it up the way a player does, then
    // click the pet. Before the fix this click errored instead of queueing anything.
    s.set_container(
        0,
        Some(benilla_ui::script::ContainerState {
            name: Some("Backpack".into()),
            num_slots: 16,
            slots: std::collections::HashMap::from([(
                1,
                benilla_ui::script::ContainerSlot {
                    item_id: 2287, // Haunch of Meat — pet food
                    count: 1,
                    ..Default::default()
                },
            )]),
        }),
    );
    s.fire_event("BAG_UPDATE", vec![ScriptValue::Int(0)]);
    s.run("PickupContainerItem(0, 1)").unwrap();
    assert!(
        s.eval::<bool>("return CursorHasItem()").unwrap(),
        "the food is on the cursor"
    );

    s.run("PetFrame_OnClick(\"LeftButton\")")
        .expect("the cursor-holds-an-item leg must not error");
    assert_eq!(
        s.take_drop_item_on_unit(),
        vec!["pet".to_string()],
        "a held item + a pet click queues the drop"
    );
    // …and it is the DROP leg, not the target leg — the reference's if/elseif is exclusive.
    assert!(s.take_selection_requests().is_empty());
}

/// The GlobalStrings the happiness tooltip resolves by key. Loaded from the MPQ in production; the
/// test declares the ones it reads, the pet-bar tests' convention. (That the real keys exist in the
/// shipped file is asserted separately, by name, in `ui_pet_stats`.)
fn declare_happiness_strings(s: &UiScript) {
    s.run(
        "PET_HAPPINESS1 = 'Unhappy' PET_HAPPINESS2 = 'Content' PET_HAPPINESS3 = 'Happy' \
         PET_DAMAGE_PERCENTAGE = 'Damage: %d%%' \
         LOSING_LOYALTY = 'Losing Loyalty' GAINING_LOYALTY = 'Gaining Loyalty'",
    )
    .unwrap();
}

fn stats(
    hunter: bool,
    happiness: Option<u32>,
    damage: f32,
    rate: f32,
) -> benilla_ui::script::PetStats {
    benilla_ui::script::PetStats {
        hunter_pet: hunter,
        happiness,
        damage_percentage: damage,
        loyalty_rate: rate,
        loyalty: Some("(Loyalty Level 6) Best Friend".into()),
        training_points: (170, 130),
        experience: (4200, 8000),
        // The unit frame draws neither the family word (the paper doll's level line) nor the diet
        // (its tooltip) — decision 1062. Left at their defaults on purpose: the happiness icon
        // these tests drive must not start depending on them.
        ..benilla_ui::script::PetStats::default()
    }
}

/// The happiness icon end to end through the real XML (decision 1005): shown for a hunter pet,
/// re-cut per bucket, hidden for anything that is not one.
///
/// The tooltip key is the assertion rather than the texcoords because it is what proves **which
/// branch ran** — `PET_HAPPINESS2` can only come from the `happiness == 2` arm.
#[test]
fn the_happiness_icon_shows_per_bucket_and_hides_for_a_non_hunter_pet() {
    let mut s = load_pet_frame();
    declare_happiness_strings(&s);
    s.set_unit("pet", Some(pet("Grimjaw", 72, 45, 80, 2)));

    for (bucket, tip, rate, loyalty_line) in [
        (3u32, "Happy", 20.0f32, Some("Gaining Loyalty")),
        (2, "Content", 0.0, None),
        (1, "Unhappy", -10.0, Some("Losing Loyalty")),
    ] {
        s.set_pet_stats(true, stats(true, Some(bucket), 125.0, rate));
        s.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);
        assert!(
            s.eval::<bool>("return PetFrameHappiness:IsVisible()")
                .unwrap(),
            "bucket {bucket} must show the icon"
        );
        assert_eq!(
            s.eval::<String>("return PetFrameHappiness.tooltip")
                .unwrap(),
            tip,
            "bucket {bucket} took the wrong texcoord branch"
        );
        // The loyalty line is chosen by the SIGN of the rate — and is absent at exactly zero.
        let line = s
            .eval::<Option<String>>("return PetFrameHappiness.tooltipLoyalty")
            .unwrap();
        assert_eq!(
            line.as_deref(),
            loyalty_line,
            "bucket {bucket} loyalty line"
        );
    }

    // A warlock's imp: `HasPetUI`'s second return is nil, so the icon hides however happy the
    // first return looks.
    s.set_pet_stats(true, stats(false, Some(3), 125.0, 20.0));
    s.fire_event("UNIT_HAPPINESS", vec![]);
    assert!(!s
        .eval::<bool>("return PetFrameHappiness:IsVisible()")
        .unwrap());
}

/// **Bucket 0 keeps the icon up.** The reference hides on `not happiness`, and `0` is truthy in
/// Lua — so a client that folded bucket 0 into nil would hide a frame the reference shows. This is
/// the trap wow-re calls out by name, tested where it would actually bite: in the frame.
#[test]
fn happiness_bucket_zero_keeps_the_icon_showing() {
    let mut s = load_pet_frame();
    declare_happiness_strings(&s);
    s.set_unit("pet", Some(pet("Grimjaw", 72, 45, 80, 2)));

    s.set_pet_stats(true, stats(true, Some(0), 100.0, 0.0));
    s.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);
    assert!(
        s.eval::<bool>("return PetFrameHappiness:IsVisible()")
            .unwrap(),
        "bucket 0 is a number, not the hide case"
    );

    // …whereas a genuine nil — the gate failure — does hide it.
    s.set_pet_stats(true, stats(true, None, 100.0, 0.0));
    s.fire_event("UNIT_HAPPINESS", vec![]);
    assert!(!s
        .eval::<bool>("return PetFrameHappiness:IsVisible()")
        .unwrap());
}

/// `UNIT_HAPPINESS` repaints the icon **alone** — the reference's own arm returns before the
/// frame-wide update, so a happiness tick must not be a full repaint.
#[test]
fn unit_happiness_repaints_only_the_icon() {
    let mut s = load_pet_frame();
    declare_happiness_strings(&s);
    s.set_unit("pet", Some(pet("Grimjaw", 72, 45, 80, 2)));
    s.set_pet_stats(true, stats(true, Some(1), 75.0, -10.0));
    s.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);
    assert_eq!(
        s.eval::<String>("return PetFrameHappiness.tooltip")
            .unwrap(),
        "Unhappy"
    );

    // Feed it up a bucket and fire ONLY the happiness event.
    s.set_pet_stats(true, stats(true, Some(3), 125.0, 20.0));
    s.fire_event("UNIT_HAPPINESS", vec![]);
    assert_eq!(
        s.eval::<String>("return PetFrameHappiness.tooltip")
            .unwrap(),
        "Happy",
        "UNIT_HAPPINESS must re-cut the icon on its own"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The layering law (decision 0884), the pet frame's copy of the player/target/party tests: the
/// frame art must draw OVER the bar fills, which only the frame LEVEL can hold — the draw layer is
/// bucket-wide. A "simplification" back to declaration order puts the bars on top as pasted slabs.
#[test]
fn the_pet_art_paints_over_the_bars() {
    let s = load_pet_frame();
    let level: (i64, i64, i64) = s
        .eval(
            "return PetFrameTextureFrame:GetFrameLevel(), \
                    PetFrameHealthBar:GetFrameLevel(), \
                    PetFrameManaBar:GetFrameLevel()",
        )
        .unwrap();
    let (art, health, mana) = level;
    assert_eq!(health, art - 1, "the health bar sits under the art");
    assert_eq!(mana, art - 1, "and so does the mana bar");
}
