//! The always-on multibars (`MultiBars.xml`) + the stance bar (`StanceBar.xml`) driven end to
//! end through the REAL shipped XML — split out of `action_bar_tests.rs` (one file per bar
//! family; self-contained loader per the `bag_tests` precedent).

use benilla_ui::script::{ActionSlot, QuadContent, SpellTooltipView, UiScript};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error.
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
        "loader errors in {file}: {:?}",
        report.errors
    );
}

/// Load the shipped `UIParent.xml` (UIParent_ManageFramePositions — the stance bar's
/// OnShow/OnHide calls it, decision 0272; the runtime loads it before every bar) +
/// `Cooldown.xml` (CooldownFrame_SetTimer — same before-every-consumer posture) +
/// `ActionBar.xml` (the anchor target + shared globals both new bars need).
fn load_action_bar(s: &UiScript) {
    for file in ["UIParent.xml", "Cooldown.xml", "ActionBar.xml"] {
        load_xml(s, file);
    }
}

/// The two always-on bottom multibars (MultiBars.xml) through the REAL shipped XML: the fixed
/// page bases (BottomLeft = actions 61..72, BottomRight = 49..60 — ref ActionButton_GetPagedID's
/// parent-name fork), the vanilla anchor chain (BottomLeft's BOTTOMLEFT on ActionButton1's
/// TOPLEFT +17, BottomRight 10 to its right), empty wells HIDDEN except while a payload is held
/// (the ref's own multibar default, unlike the main bar's always-visible wells), a click queuing
/// the multibar id, and the bonus-bar page flip leaving multibar ids untouched.
#[test]
fn shipped_multibars_drive_end_to_end() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_action_bar(&s);
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui/MultiBars.xml"),
    )
    .unwrap();
    let doc = benilla_ui::framexml::parse(&text).unwrap();
    let report = benilla_ui::loader::load(&s, &doc, &|_| None);
    assert!(
        report.errors.is_empty(),
        "loader errors: {:?}",
        report.errors
    );
    assert_eq!(
        report.frames, 50,
        "2 bar frames + 24 buttons, each with a Cooldown child"
    );

    // Occupy main slot 1, BottomLeft slot 1 (action 61), BottomRight slot 1 (action 49).
    s.set_action(
        1,
        Some(ActionSlot {
            texture: Some("Interface\\Icons\\Spell_Main".into()),
            kind: 0x00,
            action: 100,
            count: 0,
        }),
    );
    s.set_action(
        61,
        Some(ActionSlot {
            texture: Some("Interface\\Icons\\Spell_BL".into()),
            kind: 0x00,
            action: 200,
            count: 0,
        }),
    );
    s.set_action(
        49,
        Some(ActionSlot {
            texture: Some("Interface\\Icons\\Spell_BR".into()),
            kind: 0x00,
            action: 300,
            count: 0,
        }),
    );
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    s.resolve();
    let quads = s.extract();
    let icon = |path: &str| {
        quads
            .iter()
            .find(|q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p == path))
            .and_then(|q| q.rect)
    };

    // Geometry: the 1024-wide bar's left edge sits at x=0 on the 1024-wide screen; main button 1
    // spans x[8,44] y[4,40] (the end-to-end test above). BottomLeft's BOTTOMLEFT = button 1's
    // TOPLEFT +(0,17) = (8,57), its button 1 at the frame's BOTTOMLEFT ⇒ x[8,44] y[57,93].
    // BottomRight's LEFT = BottomLeft's (500-wide) RIGHT +(10,0) ⇒ frame left 518, same y band.
    let bl = icon("Interface\\Icons\\Spell_BL").expect("BottomLeft button 1 icon");
    assert_eq!(
        (bl.left, bl.bottom, bl.right, bl.top),
        (8.0, 57.0, 44.0, 93.0)
    );
    let br = icon("Interface\\Icons\\Spell_BR").expect("BottomRight button 1 icon");
    assert_eq!(
        (br.left, br.bottom, br.right, br.top),
        (518.0, 57.0, 554.0, 93.0)
    );

    // Empty multibar wells are HIDDEN (the ref default): rings = 12 main wells (always drawn)
    // + the 2 occupied multibar buttons only.
    let rings = |quads: &[benilla_ui::script::ExtractedQuad], path: &str| {
        quads
            .iter()
            .filter(
                |q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p == path),
            )
            .count()
    };
    assert_eq!(
        rings(&quads, "Interface\\Buttons\\UI-Quickslot2"),
        14,
        "12 main wells + 2 occupied multibar buttons; 22 empty multibar wells hidden"
    );

    // A click on BottomLeft button 1 (center (26,75)) queues the FIXED id 61 — and stays 61 when
    // a bonus page is active (the ref's fork: only the main bar re-pages).
    s.mouse_button(26.0, 75.0, "LeftButton", true);
    s.mouse_button(26.0, 75.0, "LeftButton", false);
    assert_eq!(s.take_action_uses(), vec![61]);
    s.set_bonus_bar_offset(1);
    s.fire_event("UPDATE_BONUS_ACTIONBAR", vec![]);
    s.mouse_button(26.0, 75.0, "LeftButton", true);
    s.mouse_button(26.0, 75.0, "LeftButton", false);
    assert_eq!(
        s.take_action_uses(),
        vec![61],
        "a bonus page never re-pages a multibar"
    );
    s.set_bonus_bar_offset(0);
    s.fire_event("UPDATE_BONUS_ACTIONBAR", vec![]);

    // While a payload is held (SHOWGRID), the hidden empty wells appear as drop-target rings
    // (UI-Quickslot, the "no action" ring): 11 empty main wells swap texture + 22 multibar wells
    // show ⇒ 33; HIDEGRID hides the multibar ones again.
    s.fire_event("ACTIONBAR_SHOWGRID", vec![]);
    s.resolve();
    assert_eq!(
        rings(&s.extract(), "Interface\\Buttons\\UI-Quickslot"),
        33,
        "grid shows every empty well as a drop target"
    );
    s.fire_event("ACTIONBAR_HIDEGRID", vec![]);
    s.resolve();
    assert_eq!(rings(&s.extract(), "Interface\\Buttons\\UI-Quickslot"), 0);
    assert_eq!(rings(&s.extract(), "Interface\\Buttons\\UI-Quickslot2"), 14);

    // The unbound multibar hotkey corner carries the ref's RANGE_INDICATOR dot: out of range paints
    // the red dot, back in range clears it (the main bar's labels tint instead).
    use benilla_ui::script::ActionState;
    s.set_action_state(
        61,
        Some(ActionState {
            usable: true,
            in_range: Some(false),
            has_range: true,
            ..Default::default()
        }),
    );
    s.tick(0.5); // past the 0.2 s range recheck
    s.resolve();
    let dot_shown = |quads: &[benilla_ui::script::ExtractedQuad]| {
        quads
            .iter()
            .any(|q| matches!(&q.content, QuadContent::Text { text: Some(t), .. } if t == "●"))
    };
    assert!(dot_shown(&s.extract()), "out of range shows the red dot");
    s.set_action_state(
        61,
        Some(ActionState {
            usable: true,
            in_range: Some(true),
            has_range: true,
            ..Default::default()
        }),
    );
    s.tick(0.5);
    s.resolve();
    assert!(!dot_shown(&s.extract()), "back in range clears the dot");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The stance bar (StanceBar.xml) through the REAL shipped XML: hidden at zero forms, sized to
/// the pushed list (buttons past numForms hide), the checked ring on the active form, the 0.4
/// grey on a not-castable one, a click queuing the form's spell id, and an emptied push hiding
/// the whole frame again — the wow-re shapeshift-bar-api mechanism driven end to end.
#[test]
fn shipped_stance_bar_drives_end_to_end() {
    use benilla_ui::script::ShapeshiftFormView;

    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    // Cooldown.xml + ActionBar.xml first: StanceBar.xml anchors to BenillaActionBar and calls
    // CooldownFrame_SetTimer / BENILLA_FALLBACK_ICON (the runtime load order).
    load_action_bar(&s);
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui/StanceBar.xml"),
    )
    .unwrap();
    let doc = benilla_ui::framexml::parse(&text).unwrap();
    let report = benilla_ui::loader::load(&s, &doc, &|_| None);
    assert!(
        report.errors.is_empty(),
        "loader errors: {:?}",
        report.errors
    );
    assert_eq!(
        report.frames, 21,
        "the bar frame + 10 buttons, each with a Cooldown child"
    );

    // No forms pushed (a mage): the frame is hidden.
    assert!(!s
        .eval::<bool>("return BenillaShapeshiftBarFrame:IsShown()")
        .unwrap());

    // A warrior's two stances: battle active, defensive known but not castable.
    s.set_shapeshift_forms(vec![
        ShapeshiftFormView {
            spell_id: 2457,
            texture: Some("Interface\\Icons\\Stance_A".into()),
            name: "Battle Stance".into(),
            active: true,
            castable: true,
            cooldown: None,
        },
        ShapeshiftFormView {
            spell_id: 71,
            texture: Some("Interface\\Icons\\Stance_B".into()),
            name: "Defensive Stance".into(),
            active: false,
            castable: false,
            cooldown: None,
        },
    ]);
    s.fire_event("UPDATE_SHAPESHIFT_FORMS", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    s.resolve();
    let quads = s.extract();

    // Geometry: the stance frame's BOTTOMLEFT = BenillaActionBar (1024×53, screen-bottom
    // centered ⇒ left edge 0) TOPLEFT +(30,45) = (30,98); button 1 at frame BOTTOMLEFT +(11,3),
    // 30×30 ⇒ x[41,71] y[101,131]; button 2 chains +7 ⇒ left 78.
    let icon = |quads: &[benilla_ui::script::ExtractedQuad], path: &str| {
        quads
            .iter()
            .find(|q| matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p == path))
            .and_then(|q| q.rect)
    };
    let a = icon(&quads, "Interface\\Icons\\Stance_A").expect("stance button 1 icon");
    assert_eq!(
        (a.left, a.bottom, a.right, a.top),
        (41.0, 101.0, 71.0, 131.0)
    );
    let b = icon(&quads, "Interface\\Icons\\Stance_B").expect("stance button 2 icon");
    assert_eq!(b.left, 78.0);

    // The active form's checked ring; the not-castable grey on button 2's icon.
    assert!(s
        .eval::<bool>("return BenillaShapeshiftButton1:GetChecked()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return BenillaShapeshiftButton2:GetChecked()")
        .unwrap());
    let grey = quads.iter().find_map(|q| match &q.content {
        QuadContent::Texture {
            path: Some(p),
            color: Some(c),
            ..
        } if p.contains("Stance_B") => Some(*c),
        _ => None,
    });
    assert_eq!(
        grey,
        Some([0.4, 0.4, 0.4, 1.0]),
        "not-castable greys the icon"
    );
    // Buttons past numForms stay hidden: exactly two stance icons drew.
    let stance_icons = quads
        .iter()
        .filter(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p.contains("Stance_"))
        })
        .count();
    assert_eq!(stance_icons, 2);

    // A click on button 2 (center (93,116)) queues the form's SPELL id — cast-vs-cancel is the
    // app drain's call, not the XML's.
    s.mouse_button(93.0, 116.0, "LeftButton", true);
    s.mouse_button(93.0, 116.0, "LeftButton", false);
    assert_eq!(s.take_shapeshift_casts(), vec![71]);

    // The checked ring never follows the click — the OnClick reverts the CheckButton's own
    // pre-OnClick toggle (ref ShapeshiftButtonTemplate), leaving checked to the isActive
    // repaint: button 2 stays unchecked until the server actually shifts us…
    assert!(
        !s.eval::<bool>("return BenillaShapeshiftButton2:GetChecked()")
            .unwrap(),
        "a clicked non-active form must stay unchecked until the form byte confirms"
    );
    assert!(s
        .eval::<bool>("return BenillaShapeshiftButton1:GetChecked()")
        .unwrap());

    // …and clicking the ACTIVE form (the director's warrior bug: Battle Stance must not
    // untoggle) still queues the spell — the app drain decides it is a silent no-op — while the
    // ring stays lit with no repaint needed.
    s.mouse_button(56.0, 116.0, "LeftButton", true);
    s.mouse_button(56.0, 116.0, "LeftButton", false);
    assert_eq!(s.take_shapeshift_casts(), vec![2457]);
    assert!(
        s.eval::<bool>("return BenillaShapeshiftButton1:GetChecked()")
            .unwrap(),
        "clicking the active stance must not untoggle its checked ring"
    );

    // RIGHT-CLICK IS DEAD ON THIS BAR — no flash, no cast — and that is the reference (decisions
    // 1023 + 1030). `ShapeshiftButtonTemplate` inherits `ActionButtonTemplate` and then overrides
    // <OnLoad> with a body that only scales the cooldown, dropping the
    // `RegisterForClicks("LeftButtonUp","RightButtonUp")` that `ActionButton_OnLoad` gives every
    // other action-style button (ref ActionButton.lua:109). The default {LeftButtonUp} stands, and
    // `0x77924b` gates the PushedTexture on that same mask. 1027 diverged and 1030 reverted it, so
    // this asserts the quirk on purpose: the left press below proves the flash works at all here.
    let depressed = |s: &UiScript| {
        s.extract().iter().any(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. } if p.contains("Quickslot-Depress"))
        })
    };
    s.mouse_move(56.0, 116.0);
    s.mouse_button(56.0, 116.0, "RightButton", true);
    assert!(
        !depressed(&s),
        "a right-press must NOT flash — unregistered"
    );
    s.mouse_button(56.0, 116.0, "RightButton", false);
    assert!(
        s.take_shapeshift_casts().is_empty(),
        "and must not cast — the ref never routes a right-click here at all"
    );
    s.mouse_button(56.0, 116.0, "LeftButton", true);
    assert!(depressed(&s), "the LEFT press does flash — the mask has it");
    s.mouse_button(56.0, 116.0, "LeftButton", false);
    let _ = s.take_shapeshift_casts();
    assert!(
        s.eval::<bool>("return BenillaShapeshiftButton1:GetChecked()")
            .unwrap(),
        "and the active form stays lit through either"
    );

    // An emptied push hides the whole frame (the formless class path, live: shapeshift unlearned).
    s.set_shapeshift_forms(vec![]);
    s.fire_event("UPDATE_SHAPESHIFT_FORMS", vec![]);
    assert!(!s
        .eval::<bool>("return BenillaShapeshiftBarFrame:IsShown()")
        .unwrap());
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The hover reads the button's OWN paged action, not the main bar's slot of the same index.
///
/// The director's report (2026-07-21): hovering a multibar spell showed nothing, or the tooltip
/// of the spell *below* it on the main bar. `BenillaActionButton_OnEnter` called
/// `BenillaActionBar_ActionFor(button.index)` — the MAIN bar's page formula — instead of
/// `BenillaActionButton_Action(button)`, which carries the ref's parent-name fork as the
/// per-button `base`. Index 1 on BottomLeft therefore rendered action 1, not 61: the button
/// directly below it. Every other button path already used the fork; only the tooltip reached
/// past it, so nothing else showed the bug.
#[test]
fn multibar_hover_renders_the_buttons_own_action() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for file in [
        "Fonts.xml",
        "UIParent.xml",
        "GameTooltip.xml",
        "Cooldown.xml",
        "ActionBar.xml",
        "MultiBars.xml",
    ] {
        load_xml(&s, file);
    }

    // Main slot 1 → spell 100, BottomLeft slot 1 (action 61) → 200, BottomRight slot 1 (49) → 300.
    // Main slot 2 is left EMPTY while BottomLeft slot 2 (62) → 400: the "no tooltip at all" half
    // of the report — the wrong id resolved to an empty slot, and SetAction renders nothing.
    for (slot, spell) in [(1u32, 100u32), (61, 200), (49, 300), (62, 400)] {
        s.set_action(
            slot,
            Some(ActionSlot {
                texture: Some(format!("Interface\\Icons\\Spell_{spell}")),
                kind: 0x00,
                action: spell,
                count: 0,
            }),
        );
        s.set_spell_tooltip(
            spell,
            SpellTooltipView {
                name: format!("Spell {spell}"),
                description: "does a thing".into(),
                ..Default::default()
            },
        );
    }
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());

    let hover_name = |s: &UiScript, button: &str| -> Option<String> {
        s.run(&format!(
            "GameTooltip:Hide() BenillaActionButton_OnEnter({button})"
        ))
        .unwrap();
        s.eval::<Option<String>>(
            "if not GameTooltip:IsShown() then return nil end return GameTooltipTextLeft1:GetText()",
        )
        .unwrap()
    };

    assert_eq!(
        hover_name(&s, "BenillaActionButton1").as_deref(),
        Some("Spell 100"),
        "the main bar still reads its own slot"
    );
    assert_eq!(
        hover_name(&s, "BenillaMultiBarBottomLeftButton1").as_deref(),
        Some("Spell 200"),
        "BottomLeft button 1 is action 61 — not main slot 1 (the spell below it)"
    );
    assert_eq!(
        hover_name(&s, "BenillaMultiBarBottomRightButton1").as_deref(),
        Some("Spell 300"),
        "BottomRight button 1 is action 49"
    );
    assert_eq!(
        hover_name(&s, "BenillaMultiBarBottomLeftButton2").as_deref(),
        Some("Spell 400"),
        "an occupied multibar slot over an EMPTY main slot still renders (the no-tooltip half)"
    );

    // A bonus page re-pages ONLY the main bar — the multibar hover is untouched by it.
    s.set_bonus_bar_offset(1);
    s.fire_event("UPDATE_BONUS_ACTIONBAR", vec![]);
    assert_eq!(
        hover_name(&s, "BenillaMultiBarBottomLeftButton1").as_deref(),
        Some("Spell 200"),
        "a bonus page never re-pages a multibar hover"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
