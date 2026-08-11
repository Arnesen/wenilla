//! The shipped **Skills tab** (`assets/ui/SkillFrame.xml`) driven end-to-end, engine-only (no
//! Bevy) — the per-window test module the spellbook/trainer/bank files already establish, split
//! out of `character_tests.rs` (which owns the paperdoll + the tab round-trip) so the skills-pane
//! paint law has a home of its own.
//!
//! What it pins is the **proficiency row**: `SkillFrame.lua`'s own `skillMaxRank == 1` gate draws
//! a full GRAY bar with NO rank text, and two different kinds of line land in it — the armor
//! proficiencies, which the server really does report as `1/1`, and the **single-rank** lines
//! (class skills, Dual Wield, racials, the per-mount riding lines), whose `skillMaxRank` the
//! engine overrides to `1` off `SkillRaceClassInfo.flags & 0x400` however high the server's own
//! descriptor is (`benilla-ui`'s `SkillEntry::mono`; wow-re `0x4d3610`'s `4d38b1` branch). A
//! hunter's `Beast Mastery` on vmangos arrives as `300/300` and must still read gray and
//! numberless, exactly as it does in the real client.

use benilla_ui::script::{QuadContent, SkillEntry, SkillsState, UiScript};

/// Load one shipped `assets/ui/<file>` into `s`, panicking on any loader error (the
/// character/spellbook tests' loader, duplicated so this file is self-contained).
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

fn skill(
    skill_id: u32,
    name: &str,
    value: u32,
    max: u32,
    mono: bool,
    (category_id, category_name, category_order): (u32, &str, u32),
) -> SkillEntry {
    SkillEntry {
        skill_id,
        name: name.into(),
        value,
        max,
        temp_bonus: 0,
        perm_bonus: 0,
        min_level: 0,
        cost_index: 0,
        category_id,
        category_name: category_name.into(),
        category_order,
        description: String::new(),
        abandonable: false,
        mono,
    }
}

/// The Skills page, shown, with one line of each shape — a hunter's real vmangos numbers:
/// `Beast Mastery 300/300` (single-rank, `SkillRaceClassInfo` 0x410), `Defense 12/60` (a normal
/// weapon line), `Cloth 1/1` (an armor proficiency the server itself caps).
fn shown_skills_page() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    for f in [
        "Fonts.xml",
        "UiPanels.xml",
        "GameTooltip.xml",
        "ScrollTemplates.xml",
        "CharacterFrame.xml",
        "SkillFrame.xml",
    ] {
        load_xml(&s, f);
    }
    s.set_skills(SkillsState {
        entries: vec![
            skill(50, "Beast Mastery", 300, 300, true, (7, "Class Skills", 2)),
            skill(95, "Defense", 12, 60, false, (6, "Weapon Skills", 5)),
            skill(415, "Cloth", 1, 1, false, (8, "Armor Proficiencies", 6)),
        ],
    });
    s.run(r#"ToggleCharacter("BenillaSkillFrame")"#).unwrap();
    s.resolve();
    s
}

/// Row slot `i`'s rank text and bar color — the two things the proficiency gate decides.
fn row(s: &mut UiScript, i: u32) -> (String, [f32; 4]) {
    let text = s
        .eval::<String>(&format!(
            "return BenillaSkillRankFrame{i}SkillRank:GetText() or \"\""
        ))
        .unwrap();
    let c = s
        .eval::<(f32, f32, f32, f32)>(&format!(
            "return BenillaSkillRankFrame{i}:GetStatusBarColor()"
        ))
        .unwrap();
    (text, [c.0, c.1, c.2, c.3])
}

/// Row slot `i`'s **trough** colour — the `$parentBackground` texture behind the fill, which the
/// ref recolours per branch alongside the fill (`SkillFrame.lua:158` normal / `:167` proficiency).
fn row_bg(s: &mut UiScript, i: u32) -> [f32; 4] {
    let c = s
        .eval::<(f32, f32, f32, f32)>(&format!(
            "return BenillaSkillRankFrame{i}Background:GetVertexColor()"
        ))
        .unwrap();
    [c.0, c.1, c.2, c.3]
}

#[test]
fn a_single_rank_line_paints_gray_with_no_rank_text() {
    let mut s = shown_skills_page();
    assert!(
        s.eval::<bool>("return BenillaSkillFrame:IsVisible()")
            .unwrap(),
        "the Skills page is up"
    );

    // Visible rows: 1 header "Class Skills", 2 Beast Mastery, 3 header "Weapon Skills",
    // 4 Defense, 5 header "Armor Proficiencies", 6 Cloth — one list slot each.
    let (text, color) = row(&mut s, 2);
    assert_eq!(
        text, "",
        "Beast Mastery is single-rank: no rank text, though the server said 300/300"
    );
    assert_eq!(color, [0.5, 0.5, 0.5, 1.0], "and a gray bar");
    assert_eq!(
        row_bg(&mut s, 2),
        [1.0, 1.0, 1.0, 0.5],
        "over the proficiency branch's WHITE trough (ref SkillFrame.lua:167)"
    );

    // The armor proficiency reaches the same branch by the server's own 1/1.
    let (text, color) = row(&mut s, 6);
    assert_eq!(text, "", "Cloth is 1/1: no rank text");
    assert_eq!(color, [0.5, 0.5, 0.5, 1.0], "gray too");
    assert_eq!(row_bg(&mut s, 6), [1.0, 1.0, 1.0, 0.5], "white trough too");

    // The control: a normal weapon line still reads its numbers, in blue.
    let (text, color) = row(&mut s, 4);
    assert_eq!(text, "12/60", "Defense keeps its rank text");
    assert_eq!(color, [0.0, 0.0, 1.0, 0.5], "and the blue fill");
    assert_eq!(
        row_bg(&mut s, 4),
        [0.0, 0.0, 0.75, 0.5],
        "over the normal branch's DARK BLUE trough (ref SkillFrame.lua:158)"
    );

    // And what those troughs actually DRAW at. The template declares the texture `<Color 1,1,1,0.2>`
    // — a real texel — and the vertex colour MULTIPLIES it, alpha included (the composition law,
    // `benilla-ui` `script::tests::regions`): the proficiency's white reaches the screen at
    // `0.2 x 0.5 = 0.1`, not 0.5. Every solid-colour quad this page emits is one of these troughs.
    let solids: Vec<[f32; 4]> = s
        .extract()
        .iter()
        .filter_map(|q| match &q.content {
            QuadContent::Texture {
                path: None,
                color: Some(c),
                ..
            } => Some(*c),
            _ => None,
        })
        .collect();
    assert!(
        solids.contains(&[1.0, 1.0, 1.0, 0.1]),
        "a proficiency trough draws white at 0.1; got {solids:?}"
    );
    assert!(
        solids.contains(&[0.0, 0.0, 0.75, 0.1]),
        "a normal row's trough draws dark blue at 0.1; got {solids:?}"
    );
    assert!(
        !solids.iter().any(|c| c[3] == 0.5),
        "nothing draws at the raw vertex alpha — that was the replace bug; got {solids:?}"
    );

    // Selecting the single-rank row paints the detail pane the same way (the shared PaintBar).
    s.run("SetSelectedSkill(2) BenillaSkillFrame_Update()")
        .unwrap();
    assert_eq!(
        s.eval::<String>("return BenillaSkillDetailBarSkillRank:GetText() or \"\"")
            .unwrap(),
        "",
        "the detail pane's own bar is a proficiency too"
    );
    assert_eq!(
        s.eval::<(f32, f32, f32, f32)>("return BenillaSkillDetailBarBackground:GetVertexColor()")
            .unwrap(),
        (1.0, 1.0, 1.0, 0.5),
        "including its trough (the shared PaintBar; ref SkillFrame.lua:376)"
    );
    assert!(s.errors().is_empty(), "errors: {:?}", s.errors());
}

/// **The director's reference screenshot, reproduced row for row** (decision 1091). A real
/// level-60 tauren hunter's `PLAYER_SKILL_INFO` block — read straight out of the live vmangos
/// `character_skills` rows the A/B was taken on — fed through the app's own display predicate
/// ([`crate::ui_char::skills_row`]) and the engine's grouping, against the REAL shipped DBCs.
///
/// What the reference client shows for exactly this block is the expectation below: four headers,
/// fourteen rows — and **no** `Dual Wield`, `Tauren Racial` or `GENERIC (DND)`, and no `Secondary
/// Skills` header, though the server sends all three lines at 300/300 like any other. Skips
/// without client data.
#[test]
fn a_real_hunters_block_lists_exactly_what_the_reference_client_lists() {
    let data = benilla_formats::wow_data_or_skip!();
    let mut chain = benilla_formats::open_chain(&data).expect("open chain");
    let catalog = benilla_formats::load_skill_line_catalog(&mut chain).expect("skill lines");

    // (skill line, value, max) — Twohunter's rows, verbatim. Tauren (race 6) hunter (class 3), 60.
    const BLOCK: &[(u16, u16, u16)] = &[
        (44, 300, 300),  // Axes
        (46, 300, 300),  // Guns
        (50, 300, 300),  // Beast Mastery       — single-rank
        (51, 300, 300),  // Survival            — single-rank
        (95, 300, 300),  // Defense
        (109, 300, 300), // Language: Orcish
        (115, 300, 300), // Language: Taurahe
        (118, 300, 300), // Dual Wield          — HIDDEN
        (124, 300, 300), // Tauren Racial       — HIDDEN
        (162, 300, 300), // Unarmed
        (163, 300, 300), // Marksmanship        — single-rank
        (173, 300, 300), // Daggers
        (183, 300, 300), // GENERIC (DND)       — HIDDEN
        (226, 300, 300), // Crossbows
        (413, 1, 1),     // Mail
        (414, 1, 1),     // Leather
        (415, 1, 1),     // Cloth
    ];
    let entries: Vec<SkillEntry> = BLOCK
        .iter()
        .filter_map(|&(skill_id, value, max)| {
            let slot = benilla_protocol::messages::PlayerSkillSlot {
                skill_id,
                step: 0,
                value,
                max,
                temp_bonus: 0,
                perm_bonus: 0,
            };
            crate::ui_char::skills_row(&slot, &catalog, 6, 3, 60)
        })
        .collect();

    let mut s = UiScript::new().unwrap();
    s.set_skills(SkillsState { entries });

    let n = s.eval::<i64>("return GetNumSkillLines()").unwrap();
    let rows: Vec<(String, bool)> = (1..=n)
        .map(|i| {
            s.eval::<(String, Option<i64>)>(&format!(
                "local n,h = GetSkillLineInfo({i}) return n,h"
            ))
            .map(|(name, header)| (name, header.is_some()))
            .unwrap()
        })
        .collect();

    let expected: Vec<(&str, bool)> = vec![
        ("Class Skills", true),
        ("Beast Mastery", false),
        ("Marksmanship", false),
        ("Survival", false),
        ("Weapon Skills", true),
        ("Axes", false),
        ("Crossbows", false),
        ("Daggers", false),
        ("Defense", false),
        ("Guns", false),
        ("Unarmed", false),
        ("Armor Proficiencies", true),
        ("Cloth", false),
        ("Leather", false),
        ("Mail", false),
        ("Languages", true),
        ("Language: Orcish", false),
        ("Language: Taurahe", false),
    ];
    let got: Vec<(&str, bool)> = rows.iter().map(|(n, h)| (n.as_str(), *h)).collect();
    assert_eq!(got, expected, "the pane, row for row");

    // The three the server sends and the client never lists.
    for hidden in ["Dual Wield", "Tauren Racial", "GENERIC (DND)"] {
        assert!(
            !got.iter().any(|(n, _)| *n == hidden),
            "{hidden} must not appear"
        );
    }
    // And the class lines still read as proficiencies despite their 300/300 descriptor.
    assert_eq!(
        s.eval::<i64>("return (select(7, GetSkillLineInfo(2)))")
            .unwrap(),
        1,
        "Beast Mastery's skillMaxRank"
    );
}
