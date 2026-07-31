//! The shipped **spellbook window** driven end-to-end, engine-only (no Bevy): the real
//! `assets/ui/SpellBookFrame.xml` loaded behind `Fonts.xml`/`UiPanels.xml`/`GameTooltip.xml`
//! (plus `ActionBar.xml` for the cross-window place test) and fed a small synthetic book —
//! mirroring `character_tests.rs`'s/`action_bar_tests.rs`'s harness (decision 0216 §8, slice 5).

use benilla_ui::script::{SpellBookState, SpellSlotView, SpellTabView, UiScript};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error (the
/// character/questlog tests' loader, duplicated so this file is self-contained).
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

/// Two tabs' worth of a small book: "Fire" (Fireball, Fire Blast — slots 0-1) and "Frost" (Frost
/// Armor — slot 2), the flat `slots` in tab order. `offset` is each tab's 0-based start index into
/// `slots` (`benilla-ui`'s own book-id seam doc) — tab 1's is 0, tab 2's is 2 (right after tab 1's
/// two spells).
fn book() -> SpellBookState {
    SpellBookState {
        tabs: vec![
            SpellTabView {
                name: "Fire".into(),
                texture: Some("Interface\\Icons\\Spell_Fire_FlameBolt".into()),
                offset: 0,
                num_spells: 2,
            },
            SpellTabView {
                name: "Frost".into(),
                texture: Some("Interface\\Icons\\Spell_Frost_FrostBolt02".into()),
                offset: 2,
                num_spells: 1,
            },
        ],
        slots: vec![
            SpellSlotView {
                spell_id: 133,
                name: "Fireball".into(),
                rank: Some("Rank 1".into()),
                texture: Some("Interface\\Icons\\Spell_Fire_FlameBolt".into()),
                passive: false,
                current: false,
                cooldown: None,
            },
            SpellSlotView {
                spell_id: 2136,
                name: "Fire Blast".into(),
                rank: Some("Rank 1".into()),
                texture: Some("Interface\\Icons\\Spell_Fire_FireBolt02".into()),
                passive: false,
                current: false,
                cooldown: None,
            },
            SpellSlotView {
                spell_id: 168,
                name: "Frost Armor".into(),
                rank: Some("Rank 1".into()),
                texture: Some("Interface\\Icons\\Spell_Frost_FrostArmor02".into()),
                passive: false,
                current: false,
                cooldown: None,
            },
        ],
    }
}

/// The loader itself: every file the window depends on parses and materializes with no errors —
/// the window + close + prev/next page buttons + 12 spell buttons (each with a Cooldown child)
/// + 8 skill-line tabs = 36 frames.
#[test]
fn shipped_spellbook_loads_clean() {
    let s = UiScript::new().unwrap();
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui/SpellBookFrame.xml"),
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
        report.frames, 36,
        "window + close + prev/next + 12 spell buttons (each with a Cooldown child) + 8 \
         skill-line tabs"
    );
}

/// The whole contract in one end-to-end drive: `ToggleSpellBook` (the 'P' binding's entry point)
/// opens through `ShowUIPanel`, tab 1's page renders names + ranks, a plain click casts (drains
/// `take_spell_casts`, no cursor payload), a shift-click picks it up instead (the modifier
/// mirror), and the held spell places on an action button through the SAME slice-4 machinery a
/// bar-to-bar drag uses — packing kind 0x00 (SPELL) with the spell id.
#[test]
fn shipped_spellbook_drives_end_to_end() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "ActionBar.xml");
    load_xml(&s, "SpellBookFrame.xml");
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);

    s.set_spellbook(book());

    assert!(!s
        .eval::<bool>("return BenillaSpellBookFrame:IsVisible()")
        .unwrap());
    s.run("ToggleSpellBook(BOOKTYPE_SPELL)").unwrap();
    assert!(s.errors().is_empty(), "open errors: {:?}", s.errors());
    assert!(s
        .eval::<bool>("return BenillaSpellBookFrame:IsVisible()")
        .unwrap());

    // Tab 1 ("Fire", selected by default — SpellBookFrame_OnLoad's own SkillLineTab_OnClick(1)):
    // book id 1 (BenillaSpellButton1, id="1") shows Fireball; book id 2 (BenillaSpellButton3,
    // id="2" — the SECOND row of column 1, not the second on-screen button) shows Fire Blast —
    // the ref's own column-major id assignment (`id="1"`/`"7"`/`"2"`/`"8"`/…, this file's grid
    // comment), not left-to-right on-screen order. BenillaSpellButton2 (id="7", book id 7) is
    // past this 2-spell tab's end and stays disabled/hidden.
    assert_eq!(
        s.eval::<String>("return BenillaSpellButton1SpellName:GetText()")
            .unwrap(),
        "Fireball"
    );
    assert_eq!(
        s.eval::<String>("return BenillaSpellButton1SubSpellName:GetText()")
            .unwrap(),
        "Rank 1"
    );
    assert_eq!(
        s.eval::<String>("return BenillaSpellButton3SpellName:GetText()")
            .unwrap(),
        "Fire Blast"
    );
    assert!(
        !s.eval::<bool>("return BenillaSpellButton2:IsEnabled()")
            .unwrap(),
        "book id 7 is past the 2-spell Fire tab — disabled"
    );

    let center = |s: &UiScript, name: &str| -> (f32, f32) {
        let l: f32 = s.eval(&format!("return {name}:GetLeft()")).unwrap();
        let r: f32 = s.eval(&format!("return {name}:GetRight()")).unwrap();
        let t: f32 = s.eval(&format!("return {name}:GetTop()")).unwrap();
        let b: f32 = s.eval(&format!("return {name}:GetBottom()")).unwrap();
        ((l + r) * 0.5, (t + b) * 0.5)
    };
    s.resolve();
    let (x1, y1) = center(&s, "BenillaSpellButton1");

    // A plain click CASTS (drains the intent) — never picks up.
    s.mouse_button(x1, y1, "LeftButton", true);
    s.mouse_button(x1, y1, "LeftButton", false);
    assert!(s.errors().is_empty(), "click errors: {:?}", s.errors());
    assert_eq!(s.take_spell_casts(), vec![133]);
    assert!(s.cursor_payload().is_none(), "a cast never picks up");

    // A shift-click PICKS UP instead (the modifier mirror) — never casts.
    s.set_modifiers(true, false, false);
    s.mouse_button(x1, y1, "LeftButton", true);
    s.mouse_button(x1, y1, "LeftButton", false);
    s.set_modifiers(false, false, false);
    assert!(s.take_spell_casts().is_empty(), "shift-click never casts");
    let (kind, _book_id, book_type, spell_id) = s
        .eval::<(String, i64, String, i64)>(
            "local k, slot, book, id = GetCursorInfo() return k, slot, book, id",
        )
        .unwrap();
    assert_eq!(
        (kind.as_str(), book_type.as_str(), spell_id),
        ("spell", "spell", 133)
    );

    // Place the held spell onto action button 1 — the SAME slice-4 machinery
    // (`cursor::bar::place_action`) a bar-to-bar drag uses: a plain click on an action button
    // routes through UseAction's checkCursor=1 fork to a place. Packs kind 0x00 (SPELL, decision
    // 0216 §1) with the spell id — `action_sets` is the app's own CMSG_SET_ACTION_BUTTON queue.
    let (ax, ay) = center(&s, "BenillaActionButton1");
    s.mouse_button(ax, ay, "LeftButton", true);
    s.mouse_button(ax, ay, "LeftButton", false);
    assert!(s.errors().is_empty(), "place errors: {:?}", s.errors());
    assert!(s.cursor_payload().is_none(), "placed — cursor clears");
    assert!(s.eval::<bool>("return HasAction(1)").unwrap());
    assert_eq!(s.take_action_sets(), vec![(1, 133)]); // 0x00<<24 | 133
}

/// The cooldown pie through the REAL shipped XML (the once-deferred half of slice 5): a pushed
/// per-slot triple + `SPELL_UPDATE_COOLDOWN` arms the button's Cooldown widget mid-sweep (ref
/// SpellButton_UpdateButton l.359-360), and an on-hold triple (enable 0 — Stealth/Feign Death
/// parked until SMSG_COOLDOWN_EVENT) keeps the widget hidden and dims the icon to 40% (l.361-365).
#[test]
fn shipped_spellbook_shows_the_cooldown_pie() {
    use benilla_ui::script::QuadContent;

    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for f in [
        "Fonts.xml",
        "UiPanels.xml",
        "GameTooltip.xml",
        "Cooldown.xml",
        "SpellBookFrame.xml",
    ] {
        load_xml(&s, f);
    }
    s.set_spellbook(book());
    s.run("ToggleSpellBook(BOOKTYPE_SPELL)").unwrap();
    s.tick(10.0); // GetTime = 10

    // A running 10 s cooldown started at t=6 (6 s left): the event re-read arms the sweep.
    let mut b = book();
    b.slots[0].cooldown = Some((6_000, 10_000, true));
    s.set_spellbook(b);
    s.fire_event("SPELL_UPDATE_COOLDOWN", vec![]);
    s.resolve();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
    let sweep = s.extract().into_iter().find_map(|q| {
        if s.quad_owner_name(q.target).as_deref() != Some("BenillaSpellButton1Cooldown") {
            return None;
        }
        match q.content {
            QuadContent::Cooldown { fraction, flash } => Some((fraction, flash)),
            _ => None,
        }
    });
    let (fraction, flash) = sweep.expect("BenillaSpellButton1's Cooldown widget is showing");
    assert!(
        (fraction - 0.4).abs() < 1e-3,
        "4 s elapsed of 10 ⇒ the sweep sits at 40%, got {fraction}"
    );
    assert_eq!(flash, None);

    // An on-hold triple: no sweep (CooldownFrame_SetTimer's enable gate), the icon dims to 40%.
    let mut b = book();
    b.slots[0].cooldown = Some((6_000, 10_000, false));
    s.set_spellbook(b);
    s.fire_event("SPELL_UPDATE_COOLDOWN", vec![]);
    s.resolve();
    let quads = s.extract();
    assert!(
        !quads.iter().any(|q| {
            s.quad_owner_name(q.target).as_deref() == Some("BenillaSpellButton1Cooldown")
        }),
        "an on-hold cooldown draws no sweep"
    );
    let icon_color = quads.iter().find_map(|q| {
        if s.quad_owner_name(q.target).as_deref() != Some("BenillaSpellButton1") {
            return None;
        }
        match &q.content {
            QuadContent::Texture {
                path: Some(p),
                color,
                ..
            } if p.contains("Spell_Fire_FlameBolt") => Some(*color),
            _ => None,
        }
    });
    let c = icon_color.expect("icon quad").expect("vertex color set");
    assert_eq!((c[0], c[1], c[2]), (0.4, 0.4, 0.4), "the on-hold 40% dim");
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The empty-slot LOOK, pinned at the quad level (the regression that shipped with slice 5): a
/// slot past the book's length is a **disabled** SpellButton with no DisabledTexture and an
/// unchecked ring — so it draws its UI-Spellbook-SpellBackground square and **nothing else**. The
/// slice-5 build leaked both the UI-Quickslot2 NormalTexture (disabled wrongly fell back to
/// Normal) and the CheckButtonHilight ring (the reference's `SetChecked(0)` was read Lua-truthy),
/// putting a gold ring on all 12 slots — the director's "spellbook looks very wrong".
#[test]
fn shipped_spellbook_empty_slot_draws_only_the_background() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(640.0, 700.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "SpellBookFrame.xml");
    // An EMPTY book: every slot takes the `id > offset + numSpells` disable path.
    s.run("ToggleSpellBook(BOOKTYPE_SPELL)").unwrap();
    s.tick(0.05);
    s.resolve();
    assert!(s.errors().is_empty(), "errors: {:?}", s.errors());

    let mut slot5_paths: Vec<String> = Vec::new();
    for eq in s.extract() {
        if s.quad_owner_name(eq.target).as_deref() == Some("BenillaSpellButton5") {
            if let benilla_ui::script::QuadContent::Texture { path: Some(p), .. } = &eq.content {
                slot5_paths.push(p.clone());
            }
        }
    }
    assert_eq!(
        slot5_paths,
        vec!["Interface\\Spellbook\\UI-Spellbook-SpellBackground".to_string()],
        "an empty slot draws its background square and nothing else (no ring, no checked glow)"
    );
    // The reference passes SetChecked(0) — numeric coercion, not Lua truthiness.
    assert!(!s
        .eval::<bool>("return BenillaSpellButton5:GetChecked() and true or false")
        .unwrap());
}

/// The SetChecked coercion table, pinned from the reference's own call sites (SpellBookFrame.lua
/// l.132/134/268/296-303/336): 1/"true"/true check; 0/"false"/nil/non-numeric strings uncheck.
#[test]
fn set_checked_uses_blizzard_bool_coercion() {
    let s = UiScript::new().unwrap();
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "SpellBookFrame.xml");
    for (arg, want) in [
        ("1", true),
        ("0", false),
        ("nil", false),
        ("true", true),
        ("false", false),
        ("\"true\"", true),
        ("\"false\"", false),
        ("\"1\"", true),
        ("\"0\"", false),
        ("\"junk\"", false),
    ] {
        let got = s
            .eval::<bool>(&format!(
                "BenillaSpellButton1:SetChecked({arg}) \
                 return BenillaSpellButton1:GetChecked() and true or false"
            ))
            .unwrap();
        assert_eq!(got, want, "SetChecked({arg})");
    }
}
