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
                ..Default::default()
            },
            SpellSlotView {
                spell_id: 2136,
                name: "Fire Blast".into(),
                rank: Some("Rank 1".into()),
                texture: Some("Interface\\Icons\\Spell_Fire_FireBolt02".into()),
                passive: false,
                current: false,
                cooldown: None,
                ..Default::default()
            },
            SpellSlotView {
                spell_id: 168,
                name: "Frost Armor".into(),
                rank: Some("Rank 1".into()),
                texture: Some("Interface\\Icons\\Spell_Frost_FrostArmor02".into()),
                passive: false,
                current: false,
                cooldown: None,
                ..Default::default()
            },
        ],
    }
}

/// The centre of a laid-out frame, for a real mouse click through the hit-test — the file's own
/// idiom, lifted out of `shipped_spellbook_drives_end_to_end` so the pet test shares it.
fn center(s: &UiScript, name: &str) -> (f32, f32) {
    let l: f32 = s.eval(&format!("return {name}:GetLeft()")).unwrap();
    let r: f32 = s.eval(&format!("return {name}:GetRight()")).unwrap();
    let t: f32 = s.eval(&format!("return {name}:GetTop()")).unwrap();
    let b: f32 = s.eval(&format!("return {name}:GetBottom()")).unwrap();
    ((l + r) * 0.5, (t + b) * 0.5)
}

/// One full press/release of `button` on the named frame.
fn click(s: &mut UiScript, name: &str, button: &str) {
    s.resolve();
    let (x, y) = center(s, name);
    s.mouse_button(x, y, button, true);
    s.mouse_button(x, y, button, false);
}

/// The loader itself: every file the window depends on parses and materializes with no errors —
/// the window + close + prev/next page buttons + 12 spell buttons (each with a Cooldown child)
/// + 8 skill-line tabs + the 3 Spell/Pet toggle tabs.
#[test]
fn shipped_spellbook_loads_clean() {
    let s = UiScript::new().unwrap();
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
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
        report.frames, 39,
        "window + close + prev/next + 12 spell buttons (each with a Cooldown child) + 8 \
         skill-line tabs + the 3 Spell/Pet toggle tabs (decision 1032)"
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
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "ActionBar.xml");
    load_xml(&s, "SpellBookFrame.xml");
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);

    s.set_spellbook(book());

    assert!(!s.eval::<bool>("return SpellBookFrame:IsVisible()").unwrap());
    s.run("ToggleSpellBook(BOOKTYPE_SPELL)").unwrap();
    assert!(s.errors().is_empty(), "open errors: {:?}", s.errors());
    assert!(s.eval::<bool>("return SpellBookFrame:IsVisible()").unwrap());

    // Tab 1 ("Fire", selected by default — SpellBookFrame_OnLoad's own SkillLineTab_OnClick(1)):
    // book id 1 (SpellButton1, id="1") shows Fireball; book id 2 (SpellButton3,
    // id="2" — the SECOND row of column 1, not the second on-screen button) shows Fire Blast —
    // the ref's own column-major id assignment (`id="1"`/`"7"`/`"2"`/`"8"`/…, this file's grid
    // comment), not left-to-right on-screen order. SpellButton2 (id="7", book id 7) is
    // past this 2-spell tab's end and stays disabled/hidden.
    assert_eq!(
        s.eval::<String>("return SpellButton1SpellName:GetText()")
            .unwrap(),
        "Fireball"
    );
    assert_eq!(
        s.eval::<String>("return SpellButton1SubSpellName:GetText()")
            .unwrap(),
        "Rank 1"
    );
    assert_eq!(
        s.eval::<String>("return SpellButton3SpellName:GetText()")
            .unwrap(),
        "Fire Blast"
    );
    assert!(
        !s.eval::<bool>("return SpellButton2:IsEnabled() ~= 0")
            .unwrap(),
        "book id 7 is past the 2-spell Fire tab — disabled"
    );

    s.resolve();
    let (x1, y1) = center(&s, "SpellButton1");

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
    let (ax, ay) = center(&s, "ActionButton1");
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
        "MoneyFrame.xml",
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
        if s.quad_owner_name(q.target).as_deref() != Some("SpellButton1Cooldown") {
            return None;
        }
        match q.content {
            QuadContent::Cooldown { fraction, flash } => Some((fraction, flash)),
            _ => None,
        }
    });
    let (fraction, flash) = sweep.expect("SpellButton1's Cooldown widget is showing");
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
        !quads
            .iter()
            .any(|q| { s.quad_owner_name(q.target).as_deref() == Some("SpellButton1Cooldown") }),
        "an on-hold cooldown draws no sweep"
    );
    let icon_color = quads.iter().find_map(|q| {
        if s.quad_owner_name(q.target).as_deref() != Some("SpellButton1") {
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
    load_xml(&s, "MoneyFrame.xml");
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
        if s.quad_owner_name(eq.target).as_deref() == Some("SpellButton5") {
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
        .eval::<bool>("return SpellButton5:GetChecked() and true or false")
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
                "SpellButton1:SetChecked({arg}) \
                 return SpellButton1:GetChecked() and true or false"
            ))
            .unwrap();
        assert_eq!(got, want, "SetChecked({arg})");
    }
}

/// A hunter's pet book: Growl (autocast ON, on cooldown), Claw (autocast OFF), Avoidance (a
/// passive — not autocastable). Three spells so the tab row raises and the page has content.
fn pet_book() -> benilla_ui::script::PetBookState {
    benilla_ui::script::PetBookState {
        token: Some("PET".into()),
        slots: vec![
            SpellSlotView {
                spell_id: 2649,
                name: "Growl".into(),
                rank: Some("Rank 1".into()),
                texture: Some("Interface\\Icons\\Ability_Physical_Taunt".into()),
                cooldown: Some((9400, 5000, true)),
                autocast: Some((true, true)),
                packed: 0xC100_0000 | 2649,
                ..Default::default()
            },
            SpellSlotView {
                spell_id: 16827,
                name: "Claw".into(),
                rank: Some("Rank 1".into()),
                texture: Some("Interface\\Icons\\Ability_Druid_Rake".into()),
                autocast: Some((true, false)),
                packed: 0x8100_0000 | 16827,
                ..Default::default()
            },
            SpellSlotView {
                spell_id: 3025,
                name: "Avoidance".into(),
                texture: Some("Interface\\Icons\\Spell_Nature_SpiritArmor".into()),
                passive: true,
                autocast: Some((false, false)),
                packed: 0x0100_0000 | 3025,
                ..Default::default()
            },
        ],
    }
}

/// **The pet tab, end to end through the shipped XML** (decision 1032): the toggle row appears
/// only once there are pet spells, clicking the pet tab switches the book, the page renders the
/// pet's own spells with their autocast overlay, the skill-line strip goes away, the title becomes
/// the class token's label, and a right-click flips autocast instead of casting.
#[test]
fn the_pet_tab_switches_books_and_renders_the_pets_spells() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "Cooldown.xml");
    load_xml(&s, "SpellBookFrame.xml");
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
    s.set_spellbook(book());

    // ── No pet: no toggle row, and the pet book cannot be opened at all (ref l.11-14) ──────────
    s.run("ToggleSpellBook(BOOKTYPE_SPELL)").unwrap();
    assert!(s.errors().is_empty(), "open errors: {:?}", s.errors());
    assert!(
        !s.eval::<bool>("return SpellBookFrameTabButton1:IsVisible()")
            .unwrap(),
        "with no pet spells the ref hides the whole toggle row"
    );
    s.run("ToggleSpellBook(BOOKTYPE_PET)").unwrap();
    assert!(
        s.eval::<bool>("return SpellBookFrame.bookType == BOOKTYPE_SPELL")
            .unwrap(),
        "asking for a pet book you have not got does nothing — not even close the window"
    );
    assert!(s.eval::<bool>("return SpellBookFrame:IsVisible()").unwrap());

    // ── A pet arrives: the row appears on the next repaint ────────────────────────────────────
    s.set_pet_book(pet_book());
    s.fire_event("SPELLS_CHANGED", vec![]);
    assert!(s.errors().is_empty(), "repaint errors: {:?}", s.errors());
    assert!(s
        .eval::<bool>("return SpellBookFrameTabButton1:IsVisible()")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return SpellBookFrameTabButton2:GetText()")
            .unwrap(),
        "Pet",
        "the label is PET_TYPE_<token>, not the token"
    );

    // ── Click the pet tab ─────────────────────────────────────────────────────────────────────
    click(&mut s, "SpellBookFrameTabButton2", "LeftButton");
    assert!(s.errors().is_empty(), "tab errors: {:?}", s.errors());
    assert!(s
        .eval::<bool>("return SpellBookFrame.bookType == BOOKTYPE_PET")
        .unwrap());
    assert_eq!(
        s.eval::<String>("return SpellBookTitleText:GetText()")
            .unwrap(),
        "Pet",
        "the window title becomes the pet's, not SPELLBOOK"
    );
    assert!(
        !s.eval::<bool>("return SpellBookSkillLineTab1:IsVisible()")
            .unwrap(),
        "the pet book has no skill lines — the whole strip hides (ref l.124)"
    );

    // The page: book ids are the button ids themselves on this book, so button 1 is Growl and
    // button 3 (id="2") is Claw — the same column-major id map the spell book uses.
    assert_eq!(
        s.eval::<String>("return SpellButton1SpellName:GetText()")
            .unwrap(),
        "Growl"
    );
    assert_eq!(
        s.eval::<String>("return SpellButton3SpellName:GetText()")
            .unwrap(),
        "Claw"
    );
    // The autocast overlay follows GetSpellAutocast's FIRST return (can it), not the second.
    assert!(s
        .eval::<bool>("return SpellButton1AutoCastable:IsVisible()")
        .unwrap());
    assert!(
        !s.eval::<bool>("return SpellButton5AutoCastable:IsVisible()")
            .unwrap(),
        "a passive is not autocastable"
    );
    // …and the shine marker follows the SECOND (is it on) — the native lane (decision 1383)
    // draws the sparkle wherever a shown marker sits, so shown-ness IS the enable.
    assert!(s
        .eval::<bool>("return SpellButton1Shine:IsVisible()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return SpellButton3Shine:IsVisible()")
        .unwrap());
    // The corner brackets' RECT, through the live widget. Asserted on the resolved rect rather
    // than the XML, so an anchor bug between the two is still caught — and CENTERED with no
    // offset, which is what makes them concentric with the shine below (1393).
    let br: Vec<f32> = [
        "GetWidth()",
        "GetHeight()",
        "GetLeft() + SpellButton1AutoCastable:GetWidth() / 2 - (SpellButton1:GetLeft() + SpellButton1:GetWidth() / 2)",
        "GetBottom() + SpellButton1AutoCastable:GetHeight() / 2 - (SpellButton1:GetBottom() + SpellButton1:GetHeight() / 2)",
    ]
    .iter()
    .map(|e| {
        s.eval::<f32>(&format!("return SpellButton1AutoCastable:{e}"))
            .unwrap()
    })
    .collect();
    assert!(
        (br[0] - 71.53).abs() < 0.01 && (br[1] - 71.53).abs() < 0.01,
        "brackets are {}x{}, 1393 draws them at 71.53 so the art reaches this button's corners",
        br[0],
        br[1]
    );
    assert!(
        br[2].abs() < 0.01 && br[3].abs() < 0.01,
        "brackets sit ({}, {}) off the button's centre; the ref centres them exactly",
        br[2],
        br[3]
    );

    // The marker's RECT: 1391 gave it the ref's own 36x36 at CENTER (1,1); 1393 squares it on the
    // button instead, so the glow and the brackets share a centre. Checked here rather than
    // trusted to the XML, because the whole spell-book thread turns on where this viewport sits.
    let geom: Vec<f32> = ["GetWidth", "GetHeight"]
        .iter()
        .map(|m| {
            s.eval::<f32>(&format!("return SpellButton1Shine:{m}()"))
                .unwrap()
        })
        .collect();
    assert!(
        (geom[0] - 37.0).abs() < 0.01 && (geom[1] - 37.0).abs() < 0.01,
        "shine marker is {geom:?}, expected 37x37 (1393 squares it on the button)"
    );
    let dx = s
        .eval::<f32>("return SpellButton1Shine:GetLeft() - SpellButton1:GetLeft()")
        .unwrap();
    let dy = s
        .eval::<f32>("return SpellButton1Shine:GetBottom() - SpellButton1:GetBottom()")
        .unwrap();
    assert!(
        dx.abs() < 0.01 && dy.abs() < 0.01,
        "shine marker sits at ({dx}, {dy}) inside the button; 1393 squares it on the button so it \
         is concentric with the brackets — the ref's +1,+1 is what read as a top/right bias"
    );

    // ── The clicks ────────────────────────────────────────────────────────────────────────────
    // Left: a pet cast, on the pet queue and NOT the player's.
    click(&mut s, "SpellButton1", "LeftButton");
    assert!(s.errors().is_empty(), "click errors: {:?}", s.errors());
    assert_eq!(s.take_pet_spell_casts(), vec![2649]);
    assert!(s.take_spell_casts().is_empty());
    assert!(s.take_pet_spell_autocasts().is_empty());

    // Right: autocast, never a cast (ref l.284-285).
    click(&mut s, "SpellButton3", "RightButton");
    assert!(
        s.errors().is_empty(),
        "right-click errors: {:?}",
        s.errors()
    );
    assert_eq!(s.take_pet_spell_autocasts(), vec![16827]);
    assert!(
        s.take_pet_spell_casts().is_empty(),
        "a right-click on the pet page must not also cast"
    );

    // ── The two reference QUIRKS, asserted on purpose (decisions 1030/1032) ───────────────────
    // The pet page ignores its page number: `SpellBook_GetSpellID`'s pet arm is a bare `return id`.
    assert_eq!(s.eval::<i64>("return SpellBook_GetSpellID(1)").unwrap(), 1);
    s.run(r#"SPELLBOOK_PAGENUMBERS["pet"] = 2"#).unwrap();
    assert_eq!(
        s.eval::<i64>("return SpellBook_GetSpellID(1)").unwrap(),
        1,
        "DO NOT FIX: the ref's pet arm has no page term (decision 1032)"
    );
    // …and the Next arrow writes the SPELL book's counter, leaving the pet page where it was.
    s.run(r#"SPELLBOOK_PAGENUMBERS["pet"] = 1"#).unwrap();
    click(&mut s, "SpellBookNextPageButton", "LeftButton");
    assert!(s.errors().is_empty(), "page errors: {:?}", s.errors());
    assert_eq!(
        s.eval::<i64>(r#"return SPELLBOOK_PAGENUMBERS["pet"]"#)
            .unwrap(),
        1,
        "DO NOT FIX: the ref writes SPELLBOOK_PAGENUMBERS[selectedSkillLine] on both books"
    );
    assert_eq!(
        s.eval::<String>("return SpellButton1SpellName:GetText()")
            .unwrap(),
        "Growl",
        "so the page does not turn"
    );

    // ── The pet leaves: the window closes and reverts (ref l.147-151) ─────────────────────────
    s.set_pet_book(benilla_ui::script::PetBookState::default());
    s.fire_event("SPELLS_CHANGED", vec![]);
    assert!(s.errors().is_empty(), "teardown errors: {:?}", s.errors());
    assert!(
        !s.eval::<bool>("return SpellBookFrame:IsVisible()").unwrap(),
        "a pet book with no pet closes rather than showing an empty page"
    );
    assert!(s
        .eval::<bool>("return SpellBookFrame.bookType == BOOKTYPE_SPELL")
        .unwrap());
}

/// The spellbook's **macro-editor fork**, pinned in both directions (report B248).
///
/// The reference's contract, read off the 1.12.1 install's own `SpellBookFrame.lua:271-283` and
/// `MacroFrame.lua:93-97`, is narrow and easy to "fix" into a later expansion's:
///
///   * only a **shift**-click reaches the editor at all — a plain left OR **right** click still
///     casts, exactly as it does with no macro window on screen;
///   * what lands is a whole **`/cast <name>[(<rank>)]` line**, not the bare name (the bare-name
///     insert is 2.x's `ChatEdit_InsertLink`, a different client — and `MACRO_HELP_TEXT_LINE5`,
///     which advertises this feature, says "*Shift* click");
///   * it is **appended with no separator** — the reference's own `GetText()..line` — so two
///     shift-clicks run together on one line;
///   * a **passive** contributes nothing, and neither does a shift-click while the name/icon
///     popup has the body box hidden;
///   * with the editor hidden the shift-click is a **pickup** again.
///
/// DO NOT "FIX" the `/cast` prefix away (the standing rule, decision 1030): B248 reported it as
/// wrong, and the install says it is what 1.12.1 does.
#[test]
fn the_macro_editor_takes_a_shift_click_and_only_a_shift_click() {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    // The macro window's own strings (the app runs the real `GlobalStrings.lua`; `macro_tests`'
    // harness is the source of this list).
    s.run(
        r#"
        CREATE_MACROS = "Create Macros"
        GENERAL_MACROS = "General Macros"
        CHARACTER_SPECIFIC_MACROS = "%s Specific Macros"
        ENTER_MACRO_LABEL = "Enter Macro Commands:"
        MACROFRAME_CHAR_LIMIT = "%d/255 Characters Used"
        MACRO_POPUP_TEXT = "Enter Macro Name (Max 16 Characters):"
        MACRO_POPUP_CHOOSE_ICON = "Choose an Icon:"
        CHANGE_MACRO_NAME_ICON = "Change Name/Icon"
        DELETE = "Delete" NEW = "New" EXIT = "Exit" CANCEL = "Cancel" OKAY = "Okay"
        MACROS = "Macros"
        TOOLTIP_DEFAULT_COLOR = { r = 1.0, g = 1.0, b = 1.0 }
        TOOLTIP_DEFAULT_BACKGROUND_COLOR = { r = 0.09, g = 0.09, b = 0.19 }
        "#,
    )
    .unwrap();
    for file in [
        "Fonts.xml",
        "MoneyFrame.xml",
        "UiPanels.xml",
        "GameTooltip.xml",
        "Cooldown.xml",
        "ScrollTemplates.xml",
        "MicroMenu.xml",
        "ActionBar.xml",
        "MacroFrame.xml",
        "SpellBookFrame.xml",
    ] {
        load_xml(&s, file);
    }
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);

    // The file's own book plus a third Fire spell that is PASSIVE — book id 3, which the ref's
    // column-major grid puts on SpellButton5.
    let mut b = book();
    b.tabs[0].num_spells = 3;
    b.tabs[1].offset = 3;
    b.slots.insert(
        2,
        SpellSlotView {
            spell_id: 168,
            name: "Frost Warding".into(),
            texture: Some("Interface\\Icons\\Spell_Frost_FrostWard".into()),
            passive: true,
            ..Default::default()
        },
    );
    s.set_spellbook(b);

    s.run(r#"CreateMacro("Ambush", 1, "")"#).unwrap();
    s.run("ShowMacroFrame()").unwrap();
    s.run("ToggleSpellBook(BOOKTYPE_SPELL)").unwrap();
    assert!(s.errors().is_empty(), "open errors: {:?}", s.errors());
    let body = |s: &UiScript| -> String {
        s.eval::<String>("return BenillaMacroFrameText:GetText()")
            .unwrap()
    };
    assert!(
        s.eval::<bool>("return BenillaMacroFrameText:IsVisible()")
            .unwrap(),
        "the editor's body box is up — the ref's own `MacroFrame_AddMacroLine` gate"
    );
    assert_eq!(body(&s), "");

    // ── B248's reported gesture: a plain RIGHT click still CASTS, and writes nothing ──────────
    click(&mut s, "SpellButton1", "RightButton");
    assert!(
        s.errors().is_empty(),
        "right-click errors: {:?}",
        s.errors()
    );
    assert_eq!(s.take_spell_casts(), vec![133], "a right-click casts");
    assert_eq!(
        body(&s),
        "",
        "B248: an UNSHIFTED click must never reach the macro editor (ref l.271 tests shift first)"
    );
    assert!(s.cursor_payload().is_none());

    // …and so does a plain left click, the other half of the same arm.
    click(&mut s, "SpellButton1", "LeftButton");
    assert_eq!(s.take_spell_casts(), vec![133]);
    assert_eq!(body(&s), "");

    // ── The reference gesture: shift-click APPENDS a whole `/cast` line, and never casts ──────
    s.set_modifiers(true, false, false);
    click(&mut s, "SpellButton1", "LeftButton");
    assert!(
        s.errors().is_empty(),
        "shift-click errors: {:?}",
        s.errors()
    );
    assert_eq!(
        body(&s),
        "/cast Fireball(Rank 1)",
        "ref l.276: SLASH_CAST1 + name + the rank in parens"
    );
    assert!(
        s.take_spell_casts().is_empty(),
        "the macro arm replaces the cast, it does not add to it"
    );
    assert!(
        s.cursor_payload().is_none(),
        "with the editor open a shift-click writes instead of picking up"
    );
    assert!(
        s.eval::<bool>("return BenillaMacroFrame.textChanged == 1")
            .unwrap(),
        "the write goes through the box's own OnTextChanged, so the window is dirty"
    );

    // A second one runs straight on: the reference appends with NO separator (`GetText()..line`),
    // which is why a two-spell macro built this way needs the player to break the line himself.
    // DO NOT "fix" this into a newline join.
    click(&mut s, "SpellButton3", "RightButton"); // shift outranks the button — ref l.271 vs l.284
    assert_eq!(
        body(&s),
        "/cast Fireball(Rank 1)/cast Fire Blast(Rank 1)",
        "ref MacroFrame.lua:95 — appended, unseparated"
    );
    assert!(s.take_spell_casts().is_empty());

    // A PASSIVE writes nothing at all — and does not fall through to a pickup either (ref l.274's
    // guard sits INSIDE the macro-frame arm).
    let before = body(&s);
    click(&mut s, "SpellButton5", "LeftButton");
    assert_eq!(body(&s), before, "a passive is not a castable line");
    assert!(s.cursor_payload().is_none());

    // The name/icon popup hides the body box; the ref's AddMacroLine gates on exactly that.
    s.run("BenillaMacroNewButton_OnClick()").unwrap();
    assert!(!s
        .eval::<bool>("return BenillaMacroFrameText:IsVisible()")
        .unwrap());
    click(&mut s, "SpellButton1", "LeftButton");
    assert!(s.errors().is_empty(), "popup-open errors: {:?}", s.errors());
    assert!(s.cursor_payload().is_none(), "still not a pickup");
    s.run("BenillaMacroPopupFrame:Hide()").unwrap();
    s.run("BenillaMacroFrame_Update()").unwrap();
    assert_eq!(body(&s), before, "nothing landed while the popup was up");

    // ── Editor hidden: the shift-click is a PICKUP again (ref l.281-282's else) ───────────────
    s.run("HideUIPanel(BenillaMacroFrame)").unwrap();
    click(&mut s, "SpellButton1", "LeftButton");
    s.set_modifiers(false, false, false);
    assert!(s.errors().is_empty(), "pickup errors: {:?}", s.errors());
    let (kind, spell_id) = s
        .eval::<(String, i64)>("local k, _, _, id = GetCursorInfo() return k, id")
        .unwrap();
    assert_eq!((kind.as_str(), spell_id), ("spell", 133));
    assert!(s.take_spell_casts().is_empty());
}
