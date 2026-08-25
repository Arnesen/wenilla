//! The shipped **pet paper doll** driven end-to-end, engine-only (decision 1057): the real
//! `assets/ui/PetPaperDollFrame.xml` behind `CharacterFrame.xml`, fed synthetic pet snapshots.
//!
//! The page's rows are the character page's own code with a different unit token, and
//! `character_tests.rs` already pins those. What is genuinely new — and what these test — is the
//! **conditional tab**: it goes up and down with the pet, the tab beside it slides to close the
//! gap, the page refuses to open without a pet, and it closes itself when the pet leaves. That
//! machinery is the half a green build cannot see.

use benilla_ui::script::{
    PetStats, QuadContent, ScriptValue, UiScript, UnitCombatStats, UnitState,
};

/// Load one shipped `assets/ui/<file>`, panicking on any loader error (the character tests' loader).
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

/// The page's production load prefix — it leans on `CharacterFrame.xml` for the shared setters,
/// the row templates and the tab row itself, exactly as the manifest orders them.
fn load_pet_page() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    load_xml(&s, "Fonts.xml");
    load_xml(&s, "MoneyFrame.xml");
    load_xml(&s, "UiPanels.xml");
    // `UIParent.xml` is the manifest's third file and this page needs one thing out of it:
    // `BuildListString`, the reference's own `UIParent.lua:1051` joiner behind the diet tooltip
    // (decision 1062). Loaded in manifest order rather than bolted on at the end.
    load_xml(&s, "UIParent.xml");
    load_xml(&s, "GameTooltip.xml");
    load_xml(&s, "CharacterFrame.xml");
    load_xml(&s, "PetPaperDollFrame.xml");
    s.set_unit("player", Some(player_unit()));
    s
}

fn player_unit() -> UnitState {
    UnitState {
        exists: true,
        name: Some("Benilla".into()),
        health: 100,
        max_health: 100,
        level: 60,
        race: Some("Night Elf".into()),
        race_file: Some("NightElf".into()),
        class: Some("Hunter".into()),
        class_file: Some("HUNTER".into()),
        sex: 2,
        is_player: true,
        ..UnitState::default()
    }
}

fn pet_unit() -> UnitState {
    UnitState {
        exists: true,
        name: Some("Snarl".into()),
        health: 900,
        max_health: 1000,
        level: 58,
        ..UnitState::default()
    }
}

/// A hunter pet's stat block — `hunter_pet` is `HasPetUI`'s SECOND return, the one that gates the
/// training-point line and the diet icon. The family pair is the shipped `CreatureFamily.dbc` row
/// for a Boar (id 5, food mask 63) resolved through `ItemPetFood.dbc` (decision 1062).
fn hunter_pet_stats() -> PetStats {
    PetStats {
        hunter_pet: true,
        happiness: Some(3),
        damage_percentage: 125.0,
        loyalty_rate: 20.0,
        loyalty: Some("(Loyalty Level 6) Best Friend".into()),
        training_points: (170, 130),
        experience: (4200, 8000),
        family: Some("Boar".into()),
        food_types: ["Meat", "Fish", "Cheese", "Bread", "Fungus", "Fruit"]
            .map(String::from)
            .to_vec(),
    }
}

/// Numbers a pet's descriptor really carries: stats and resistances, no PLAYER-block buff split
/// (a creature has none — decision 1057), so every pos/neg stays zero.
fn pet_combat_stats() -> UnitCombatStats {
    UnitCombatStats {
        stats: [123, 88, 210, 20, 45],
        resistances: [2400, 0, 55, 0, 0, 0, 30],
        min_damage: 90.0,
        max_damage: 130.0,
        main_attack_time_ms: 2000,
        attack_power: 640,
        ..UnitCombatStats::default()
    }
}

/// Every string the UI actually draws this frame.
fn texts(s: &mut UiScript) -> Vec<String> {
    s.resolve();
    s.extract()
        .into_iter()
        .filter_map(|q| match q.content {
            QuadContent::Text { text: Some(t), .. } => Some(t),
            _ => None,
        })
        .collect()
}

/// A tab's resolved left edge. Only meaningful with the **window open** — a frame nothing lays out
/// has no rect at all, and `GetLeft()` answers nil rather than a stale number.
fn tab_left(s: &mut UiScript, tab: u32) -> f64 {
    s.resolve();
    s.eval::<f64>(&format!("return CharacterFrameTab{tab}:GetLeft()"))
        .unwrap_or_else(|e| panic!("tab {tab} has no resolved rect: {e}"))
}

/// Put a pet in the world and tell the page about it, the way the app's feeds do.
fn give_pet(s: &mut UiScript) {
    s.set_unit("pet", Some(pet_unit()));
    s.set_pet_stats(true, hunter_pet_stats());
    s.set_pet_combat_stats(Some(pet_combat_stats()));
    s.fire_event("PET_BAR_UPDATE", vec![]);
}

/// Take it away again — the dismiss/death path: `UNIT_PET` names the OWNER, not the pet (0990).
fn take_pet(s: &mut UiScript) {
    s.set_unit("pet", None);
    s.set_pet_stats(false, PetStats::default());
    s.set_pet_combat_stats(None);
    s.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);
}

#[test]
fn shipped_pet_page_loads_clean() {
    let s = load_pet_page();
    assert!(s.errors().is_empty(), "load errors: {:?}", s.errors());
    // The page itself exists and starts down, like every other subframe.
    assert!(!s
        .eval::<bool>("return PetPaperDollFrame:IsVisible()")
        .unwrap());
}

/// **The conditional tab, both ways.** With no pet the Pet tab is down and Skills slides onto its
/// spot; with a pet the Pet tab comes up *at that very spot* and Skills moves out past it. Pinning
/// "Skills-when-closed sits exactly where Pet-when-open does" is what proves the re-anchor really
/// ran — a tab merely hidden, with the row left as-authored, would leave a gap and pass a weaker
/// check.
#[test]
fn the_pet_tab_rises_and_falls_with_the_pet_and_skills_closes_the_gap() {
    let mut s = load_pet_page();
    // The rects only exist while something lays the window out.
    s.run(r#"ToggleCharacter("PaperDollFrame")"#).unwrap();

    assert!(
        !s.eval::<bool>("return CharacterFrameTab2:IsVisible()")
            .unwrap(),
        "no pet: the Pet tab is down"
    );
    let closed3 = tab_left(&mut s, 3);

    give_pet(&mut s);
    assert!(
        s.eval::<bool>("return CharacterFrameTab2:IsVisible()")
            .unwrap(),
        "a pet raises the Pet tab"
    );
    let open2 = tab_left(&mut s, 2);
    let open3 = tab_left(&mut s, 3);
    assert_eq!(
        open2, closed3,
        "with no pet, Skills stood exactly where the Pet tab stands with one"
    );
    assert!(
        open3 > open2,
        "…and a pet pushes Skills out past it ({open3} vs {open2})"
    );

    take_pet(&mut s);
    assert!(
        !s.eval::<bool>("return CharacterFrameTab2:IsVisible()")
            .unwrap(),
        "the pet leaving lowers the tab again"
    );
    assert_eq!(tab_left(&mut s, 3), closed3, "…and the row closes back up");
    assert!(s.errors().is_empty(), "errors: {:?}", s.errors());
}

/// The reference's `ToggleCharacter` guard (ref `CharacterFrame.lua:4-6`): asking for the pet page
/// with no pet does **nothing at all** — it does not open the window on some other tab, and it does
/// not close an already-open one.
#[test]
fn asking_for_the_pet_page_without_a_pet_does_nothing() {
    let s = load_pet_page();
    s.run(r#"ToggleCharacter("PetPaperDollFrame")"#).unwrap();
    assert!(
        !s.eval::<bool>("return CharacterFrame:IsVisible()").unwrap(),
        "no pet: the window stays shut"
    );

    // …and with the window already open on the character page, the refusal leaves it open.
    s.run(r#"ToggleCharacter("PaperDollFrame")"#).unwrap();
    s.run(r#"ToggleCharacter("PetPaperDollFrame")"#).unwrap();
    assert!(
        s.eval::<bool>("return PaperDollFrame:IsVisible()").unwrap(),
        "the refusal must not switch pages or close the window"
    );
    assert!(s.errors().is_empty(), "errors: {:?}", s.errors());
}

/// The page paints the **pet's** numbers, not the player's — the whole point of un-gating the stat
/// bindings. The player's own snapshot is deliberately absent here: anything that leaked through
/// the `"player"` path would read zeros and this would fail.
#[test]
fn the_page_reads_the_pets_own_snapshot() {
    let mut s = load_pet_page();
    give_pet(&mut s);
    s.run(r#"ToggleCharacter("PetPaperDollFrame")"#).unwrap();
    assert!(s
        .eval::<bool>("return PetPaperDollFrame:IsVisible()")
        .unwrap());

    let drawn = texts(&mut s);
    for want in ["123", "88", "210", "Snarl", "(Loyalty Level 6) Best Friend"] {
        assert!(
            drawn.iter().any(|t| t.contains(want)),
            "expected {want:?} on the page; drew {drawn:?}"
        );
    }
    // Unspent training points = total - spent (ref l.85-86), not either number by itself.
    assert!(
        drawn.iter().any(|t| t == "40"),
        "unspent training points (170-130); drew {drawn:?}"
    );
    assert!(s.errors().is_empty(), "errors: {:?}", s.errors());
}

/// The window has ONE name line and the two pages take turns holding it (ref l.50-60) — the pet's
/// name replaces the player's on show and hands it back on hide. A page that forgot the hand-back
/// would leave the character sheet nameless.
#[test]
fn the_page_borrows_the_windows_name_line_and_gives_it_back() {
    let mut s = load_pet_page();
    give_pet(&mut s);

    s.run(r#"ToggleCharacter("PetPaperDollFrame")"#).unwrap();
    assert!(!s
        .eval::<bool>("return CharacterNameText:IsVisible()")
        .unwrap());
    assert!(s.eval::<bool>("return PetNameText:IsVisible()").unwrap());

    s.run(r#"ToggleCharacter("PaperDollFrame")"#).unwrap();
    assert!(
        s.eval::<bool>("return CharacterNameText:IsVisible()")
            .unwrap(),
        "the player's name line comes back"
    );
    assert!(!s.eval::<bool>("return PetNameText:IsVisible()").unwrap());
    assert!(s.errors().is_empty(), "errors: {:?}", s.errors());
}

/// A pet dismissed or killed while its own page is open **closes the window** (ref l.32-37). This
/// is the one arm that must run while the page is visible and the pet is already gone, so it is
/// also the arm that would blow up on a page that repainted before checking.
#[test]
fn the_pet_leaving_closes_the_page_under_it() {
    let mut s = load_pet_page();
    give_pet(&mut s);
    s.run(r#"ToggleCharacter("PetPaperDollFrame")"#).unwrap();
    assert!(s.eval::<bool>("return CharacterFrame:IsVisible()").unwrap());

    take_pet(&mut s);
    assert!(
        !s.eval::<bool>("return CharacterFrame:IsVisible()").unwrap(),
        "the pet's page cannot outlive the pet"
    );
    assert!(s.errors().is_empty(), "errors: {:?}", s.errors());
}

/// A warlock's minion: `HasPetUI` says yes, its second return says no. The page opens in full and
/// the three hunter-only pieces stay down (1005's class gate, seen from the page's side).
#[test]
fn a_minion_gets_the_page_without_the_hunter_furniture() {
    let mut s = load_pet_page();
    s.set_unit("pet", Some(pet_unit()));
    s.set_pet_stats(true, PetStats::default()); // has_ui, but hunter_pet == false
    s.set_pet_combat_stats(Some(pet_combat_stats()));
    s.fire_event("PET_BAR_UPDATE", vec![]);

    s.run(r#"ToggleCharacter("PetPaperDollFrame")"#).unwrap();
    assert!(
        s.eval::<bool>("return CharacterFrameTab2:IsVisible()")
            .unwrap(),
        "a minion still gets the tab"
    );
    assert!(!s
        .eval::<bool>("return PetPaperDollPetInfo:IsVisible()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return PetTrainingPointText:IsVisible()")
        .unwrap());
    assert!(!s
        .eval::<bool>("return PetTrainingPointLabel:IsVisible()")
        .unwrap());
    // …and the stat rows still read the minion's own numbers.
    assert!(
        texts(&mut s).iter().any(|t| t.contains("210")),
        "the shared rows are not hunter-gated"
    );
    assert!(s.errors().is_empty(), "errors: {:?}", s.errors());
}

/// **The level line, both ways** (decision 1062). The reference guards its whole `SetText` on
/// `UnitCreatureFamily("pet")` (ref `PetPaperDollFrame.lua:68-70`), so the family is not decoration
/// on an existing line — it is the *condition* for the line existing at all. With a family the row
/// reads "Level 58 Imp"; without one the page shows no level at all, which is the state 1057
/// shipped in and the hole this closes.
///
/// The minion is deliberate: a warlock's Imp fails `HasPetUI`'s second return, so this also pins
/// that the family word rides OUTSIDE 1005's hunter gate. Gating it would blank this line for every
/// warlock in the game.
///
/// **The two halves are two separate page loads on purpose.** The ref's guard skips the `SetText`;
/// it does not clear the FontString — so pushing a family and then taking it away leaves the old
/// word on screen (this test found that by asserting the opposite first). That is the reference's
/// own shape and it is unreachable in play: a family is a property of the pet's *template*, it
/// never goes away under a live pet, and the page closes with the pet anyway. The case that IS
/// reachable is "the page painted before the creature query answered", which is a page that never
/// had a family — the second half here.
#[test]
fn the_level_line_names_the_family_and_is_blank_without_one() {
    let with_family = |family: Option<String>| {
        let mut s = load_pet_page();
        s.set_unit("pet", Some(pet_unit()));
        s.set_pet_combat_stats(Some(pet_combat_stats()));
        s.set_pet_stats(
            true,
            PetStats {
                family,
                ..PetStats::default() // hunter_pet false — a warlock's minion
            },
        );
        s.fire_event("PET_BAR_UPDATE", vec![]);
        s.run(r#"ToggleCharacter("PetPaperDollFrame")"#).unwrap();
        let drawn = texts(&mut s);
        assert!(s.errors().is_empty(), "errors: {:?}", s.errors());
        drawn
    };

    let drawn = with_family(Some("Imp".into()));
    assert!(
        drawn.iter().any(|t| t == "Level 58 Imp"),
        "UNIT_LEVEL_TEMPLATE + the family word; drew {drawn:?}"
    );

    // No family — the creature query has not answered, or the template has none. The line is
    // absent, not "Level 58" and not "Level 58 " with a trailing space: the ref's guard skips the
    // whole SetText, which is why nil (never "") is the binding's contract.
    let drawn = with_family(None);
    assert!(
        !drawn.iter().any(|t| t.starts_with("Level")),
        "no family ⇒ no level line at all; drew {drawn:?}"
    );
}

/// **The diet tooltip** (decision 1062): the happiness-art icon under the rotate buttons is the
/// pet's DIET affordance, and its hover runs the reference's own
/// `format(PET_DIET_TEMPLATE, BuildListString(GetPetFoodTypes()))` (ref
/// `PetPaperDollFrame.xml:269`) — the vararg binding feeding the `UIParent.lua` joiner.
///
/// Driven through the frame's real `OnEnter`, not by re-typing the expression, so a guard left in
/// the XML or a mis-anchored `GetScript` slot fails here.
#[test]
fn hovering_the_diet_icon_lists_what_the_pet_eats() {
    let mut s = load_pet_page();
    give_pet(&mut s);
    s.run(r#"ToggleCharacter("PetPaperDollFrame")"#).unwrap();
    assert!(
        s.eval::<bool>("return PetPaperDollPetInfo:IsVisible()")
            .unwrap(),
        "the diet icon is shown for a hunter pet"
    );

    // …and it is actually PAINTED, not merely a mouse-enabled hole. The director's 2026-08-06
    // report was exactly that split — the tooltip answered while nothing was on screen — so the
    // hover assertion below is not sufficient on its own: pin the art too, with its size and its
    // reference TexCoords (the icon is the pet's DIET affordance, so its coords are static).
    s.resolve();
    let quads = s.extract();
    let icon = quads
        .iter()
        .find(|q| {
            matches!(&q.content, QuadContent::Texture { path: Some(p), .. }
                if p.contains("UI-PetHappiness"))
        })
        .expect("the diet icon's UI-PetHappiness quad is in the render list");
    let rect = icon.rect.expect("…with a resolved rect");
    assert!(
        (rect.right - rect.left - 24.0).abs() < 0.5 && (rect.top - rect.bottom - 23.0).abs() < 0.5,
        "24x23 (ref PetPaperDollPetInfo), got {}x{}",
        rect.right - rect.left,
        rect.top - rect.bottom
    );
    // …and it draws ON TOP of the model pane it sits inside — the actual regression. The pane's
    // booth quad is OPAQUE and the icon's rect is wholly within it, so being in the render list is
    // not the same as being seen. The pane rides BACKGROUND for exactly this reason (decision
    // 1070): at ARTWORK the draw layer, which is bucket-wide and outranks the frame (0884), buried
    // the icon no matter which frame was declared later.
    let pane = quads
        .iter()
        .find(|q| {
            matches!(&q.content, QuadContent::Texture { path, .. }
                if path.as_deref().unwrap_or_default().is_empty())
                && q.rect.is_some_and(|r| {
                    r.left <= rect.left
                        && r.right >= rect.right
                        && r.bottom <= rect.bottom
                        && r.top >= rect.top
                })
        })
        .expect("the pet model pane's booth quad covers the icon's rect");
    assert!(
        icon.z > pane.z,
        "the diet icon must paint over the pane it sits in (icon z={:#x}, pane z={:#x})",
        icon.z,
        pane.z
    );

    s.run(
        "local f = PetPaperDollPetInfo \
         f:GetScript(\"OnEnter\")(f)",
    )
    .unwrap();
    assert_eq!(
        s.eval::<String>("return GameTooltipTextLeft1:GetText()")
            .unwrap(),
        "Diet: Meat, Fish, Cheese, Bread, Fungus, Fruit",
        "PET_DIET_TEMPLATE around a plain \", \" join — 1.12's BuildListString has no \"and\""
    );
    assert!(s.errors().is_empty(), "errors: {:?}", s.errors());
}

/// `BuildListString`'s own edges, on the shipped `UIParent.xml` (decision 1062). The reference's is
/// a bare comma join with **no** "and" and **no** localization string, and it answers **nil** for
/// zero arguments — the shape `format("%s", …)` would error on, which is exactly why the diet icon
/// is hunter-gated. Quietly returning `""` here would hide that coupling.
#[test]
fn build_list_string_is_a_plain_comma_join_that_nils_on_nothing() {
    let s = load_pet_page();
    assert_eq!(
        s.eval::<String>("return BuildListString('Meat', 'Fish', 'Cheese')")
            .unwrap(),
        "Meat, Fish, Cheese"
    );
    assert_eq!(
        s.eval::<String>("return BuildListString('Meat')").unwrap(),
        "Meat",
        "one item is the item, with no separator anywhere"
    );
    assert!(
        s.eval::<bool>("return BuildListString() == nil").unwrap(),
        "zero arguments is nil, not an empty string"
    );
}
