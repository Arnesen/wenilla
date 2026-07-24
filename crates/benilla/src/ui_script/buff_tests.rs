//! The player buff bar (`assets/ui/BuffFrame.xml`, decisions 0255/0257) against its reference
//! behaviour. The XML/Lua is the unit under test; the app-side feed (`crate::ui_aura`) is stubbed by
//! pushing an [`AuraState`] list straight through [`UiScript::set_auras`] and firing `UNIT_AURA`, so
//! these exercise the *button* handlers — the row/filter wiring, the dispel-tinted border, the
//! stack count, the GetTime()-based countdown, the warning flash, and the right-click cancel — the
//! way the reference's own `BuffButton_*` did, but over our Era `UnitAura` bindings.
//!
//! State is read back through the widget (`IsShown`/`GetText`/`GetAlpha`) and the paint through the
//! [`draw list`](UiScript::extract) (icon path + border tint), the same two-lens split the cast-bar
//! tests use — Lua-visible state is blind to what actually renders.

use benilla_ui::script::{AuraState, ExtractedQuad, QuadContent, ScriptValue, UiScript};

/// Load one shipped `assets/ui/<file>`, panicking on any loader error.
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
    load_xml(&s, "Fonts.xml"); // NORMAL/HIGHLIGHT_FONT_COLOR + the FontStrings' faces
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "ActionBar.xml"); // BENILLA_FALLBACK_ICON (the unknown-icon fallback)
    load_xml(&s, "BuffFrame.xml");
    s
}

/// Build an [`AuraState`] as the feed would push it. `expiration_time` 0 = permanent (no wire
/// duration — the reference's "until cancelled"); the `GetTime()` clock starts at 0 in the harness.
#[allow(clippy::too_many_arguments)]
fn aura(
    spell_id: u32,
    name: &str,
    icon: &str,
    helpful: bool,
    count: u8,
    debuff_type: Option<&str>,
    expiration_time: f64,
    cancelable: bool,
) -> AuraState {
    AuraState {
        spell_id,
        name: Some(name.into()),
        icon: Some(icon.into()),
        count,
        debuff_type: debuff_type.map(Into::into),
        duration: expiration_time,
        expiration_time,
        helpful,
        cancelable,
    }
}

/// Push the list, then fire the discrete-change event the app's feed fires — the buttons repaint.
fn push(s: &mut UiScript, auras: Vec<AuraState>) {
    s.set_auras("player", Some(auras));
    s.fire_event("UNIT_AURA", vec![ScriptValue::Str("player".into())]);
}

/// One tick of the app's real order (OnUpdate → resolve → draw list), returning the quads.
fn frame(s: &mut UiScript, dt: f32) -> Vec<ExtractedQuad> {
    s.tick(dt);
    s.resolve();
    s.extract()
}

fn shown(s: &UiScript, name: &str) -> bool {
    s.eval::<bool>(&format!("return {name}:IsShown()")).unwrap()
}

fn text(s: &UiScript, name: &str) -> String {
    s.eval::<String>(&format!("return tostring(({name}:GetText()) or \"\")"))
        .unwrap()
}

fn alpha(s: &UiScript, name: &str) -> f64 {
    s.eval::<f64>(&format!("return {name}:GetAlpha()")).unwrap()
}

/// The first quad drawn from `Interface\...\<leaf>` — texture regions keyed by their art.
fn tex_quad<'a>(quads: &'a [ExtractedQuad], leaf: &str) -> Option<&'a ExtractedQuad> {
    quads.iter().find(|q| match &q.content {
        QuadContent::Texture { path: Some(p), .. } => p.ends_with(leaf),
        _ => false,
    })
}

#[test]
fn buffs_and_debuffs_fill_their_own_rows_with_counts_and_the_dispel_tint() {
    let mut s = harness();
    push(
        &mut s,
        vec![
            aura(
                1126,
                "Mark of the Wild",
                "Interface\\Icons\\Spell_Nature_Regeneration",
                true,
                1,
                None,
                120.0,
                true,
            ),
            aura(
                589,
                "Shadow Word: Pain",
                "Interface\\Icons\\Spell_Shadow_ShadowWordPain",
                false,
                3,
                Some("Magic"),
                18.0,
                false,
            ),
        ],
    );
    let quads = frame(&mut s, 0.1);

    // The buff takes the first helpful button, the debuff the first harmful one; the rest hide.
    assert!(shown(&s, "BuffButton0"), "first buff -> BuffButton0");
    assert!(!shown(&s, "BuffButton1"), "no second buff");
    assert!(shown(&s, "BuffButton16"), "first debuff -> BuffButton16");
    assert!(!shown(&s, "BuffButton17"), "no second debuff");

    // Both icons drew.
    assert!(
        tex_quad(&quads, "Spell_Nature_Regeneration").is_some(),
        "buff icon drew"
    );
    assert!(
        tex_quad(&quads, "Spell_Shadow_ShadowWordPain").is_some(),
        "debuff icon drew"
    );

    // The debuff border wears the Magic tint (DebuffTypeColor["Magic"] = 0.20, 0.60, 1.00).
    let border = tex_quad(&quads, "UI-Debuff-Overlays").expect("debuff border drew");
    match &border.content {
        QuadContent::Texture {
            color: Some(c),
            tex_coords: Some(uv),
            ..
        } => {
            assert!(
                (c[0] - 0.20).abs() < 1e-3
                    && (c[1] - 0.60).abs() < 1e-3
                    && (c[2] - 1.00).abs() < 1e-3,
                "Magic tint, got {c:?}"
            );
            // The reference's overlay-quadrant crop rode through unchanged.
            let uv = uv.edges();
            assert!(
                (uv[0] - 0.296875).abs() < 1e-6 && (uv[3] - 0.515625).abs() < 1e-6,
                "debuff overlay tex-coords, got {uv:?}"
            );
        }
        other => panic!("border is not a tinted, cropped texture: {other:?}"),
    }

    // Stack count shows only above 1: the 3-stack debuff shows "3", the single buff shows nothing.
    assert_eq!(text(&s, "BuffButton16Count"), "3");
    assert_eq!(text(&s, "BuffButton0Count"), "");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

#[test]
fn a_timed_aura_counts_down_and_a_permanent_one_shows_no_timer() {
    let mut s = harness();
    push(
        &mut s,
        vec![
            // Permanent (a stance): no wire duration -> expiration 0 -> no timer.
            aura(
                2457,
                "Battle Stance",
                "Interface\\Icons\\Ability_Warrior_OffensiveStance",
                true,
                1,
                None,
                0.0,
                false,
            ),
            // Timed: 120s out. After a 0.1s tick GetTime()=0.1, so 119.9s -> SecondsToTimeAbbrev -> "2 m".
            aura(
                1126,
                "Mark of the Wild",
                "Interface\\Icons\\Spell_Nature_Regeneration",
                true,
                1,
                None,
                120.0,
                true,
            ),
        ],
    );
    frame(&mut s, 0.1);

    assert_eq!(
        text(&s, "BuffButton0Duration"),
        "",
        "a permanent aura shows no timer (our 'until cancelled')"
    );
    assert_eq!(
        text(&s, "BuffButton1Duration"),
        "2 m",
        "120s remaining abbreviates to minutes"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

#[test]
fn the_warning_flash_pulses_a_low_aura_but_leaves_a_long_one_solid() {
    let mut s = harness();
    push(
        &mut s,
        vec![
            // 5 min out — never inside the 31s warning window, so it stays at full alpha.
            aura(
                1,
                "Long",
                "Interface\\Icons\\INV_Misc_QuestionMark",
                true,
                1,
                None,
                300.0,
                true,
            ),
            // 20s out — inside the window the whole run, so its button pulses.
            aura(
                2,
                "Short",
                "Interface\\Icons\\INV_Misc_QuestionMark",
                true,
                1,
                None,
                20.0,
                true,
            ),
        ],
    );

    let (mut short_min, mut long_min) = (2.0_f64, 2.0_f64);
    for _ in 0..20 {
        s.tick(0.1);
        short_min = short_min.min(alpha(&s, "BuffButton1"));
        long_min = long_min.min(alpha(&s, "BuffButton0"));
    }
    assert!(
        short_min < 0.9,
        "the 20s aura's icon pulses toward BUFF_MIN_ALPHA (min seen {short_min})"
    );
    assert!(
        (long_min - 1.0).abs() < 1e-9,
        "the 5min aura's icon stays solid (min seen {long_min})"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

#[test]
fn right_clicking_a_cancelable_buff_queues_its_spell_cancel() {
    let mut s = harness();
    push(
        &mut s,
        vec![aura(
            1126,
            "Mark of the Wild",
            "Interface\\Icons\\Spell_Nature_Regeneration",
            true,
            1,
            None,
            120.0,
            true,
        )],
    );
    frame(&mut s, 0.1);

    assert!(
        s.take_cancel_aura_requests().is_empty(),
        "nothing queued yet"
    );
    // The RightButtonUp handler -> CancelUnitBuff -> the app drains one CMSG_CANCEL_AURA by spell id.
    s.eval::<()>("BenillaBuffButton_OnClick(BuffButton0)")
        .unwrap();
    assert_eq!(s.take_cancel_aura_requests(), vec![1126]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

#[test]
fn an_emptied_bar_hides_every_button() {
    let mut s = harness();
    push(
        &mut s,
        vec![aura(
            1126,
            "Mark of the Wild",
            "Interface\\Icons\\Spell_Nature_Regeneration",
            true,
            1,
            None,
            120.0,
            true,
        )],
    );
    frame(&mut s, 0.1);
    assert!(shown(&s, "BuffButton0"), "shown while the aura is up");

    // The aura drops: the feed pushes an empty list and re-fires. The button hides, timer blanks.
    push(&mut s, vec![]);
    frame(&mut s, 0.1);
    assert!(!shown(&s, "BuffButton0"), "hidden once the aura is gone");
    assert_eq!(text(&s, "BuffButton0Duration"), "");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
