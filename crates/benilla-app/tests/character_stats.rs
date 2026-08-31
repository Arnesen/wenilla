//! Drives the REAL `assets/ui/CharacterFrame.xml` stat rows through the engine — the first test
//! that executes the paper doll's own `PaperDollFrame_Set*` Lua at all.
//!
//! That gap is why this file exists. The character sheet's whole buff-decomposition surface — the
//! green number, the red number, the `(base +x -y)` tooltip — sat broken from the day it shipped
//! (B165, B251) because the four `POSSTAT`/`NEGSTAT`/`RESISTANCEBUFFMODS*` descriptor arrays were
//! decoded as f32 when the wire carries INT, so every real buff arrived as a rounded `0` and every
//! row took its "nothing is modifying this" leg. Both gates were green throughout: the bindings'
//! own unit tests fed the snapshot directly, and nothing ever ran the Lua that colours the row.
//!
//! So the assertions here are deliberately at the *rendered string*: what `CharacterStatFrameNStatText`
//! and `MagicResTextN` actually carry after a repaint, colour escapes included. Three shapes per
//! family — untouched (plain), buffed (green), debuffed (red) — plus the tooltip's base arithmetic,
//! which is the guard for `UnitStat`'s first return being the RAW field rather than a pre-subtracted
//! one (decision 1397: the ref Lua subtracts `posBuff`/`negBuff` itself, so a pre-subtracted first
//! return deducts the buff twice — a defect the f32 bug was hiding).

use benilla_ui::script::{UiScript, UnitCombatStats, UnitState};

const UI_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/ui");

/// The character window's load prefix, in `assets/ui/benilla.toc` order. The .toc's own entry says
/// it "depends only on Fonts/UiPanels/GameTooltip"; `ItemButtonTemplate.xml` joins them because the
/// doll's equipment slots are `SetItemButton*` buttons.
const FILES: [&str; 6] = [
    "Fonts.xml",
    "ItemButtonTemplate.xml",
    "MoneyFrame.xml",
    "UiPanels.xml",
    "GameTooltip.xml",
    "CharacterFrame.xml",
];

fn load_ui(script: &UiScript) {
    let dir = std::path::Path::new(UI_DIR);
    // The app's provider shape: backslash paths, dir-relative, basename fallback.
    let provider = |req: &str| -> Option<Vec<u8>> {
        let norm = req.replace('\\', "/");
        let base = norm.rsplit('/').next().unwrap_or(&norm);
        std::fs::read(dir.join(&norm))
            .or_else(|_| std::fs::read(dir.join(base)))
            .ok()
    };
    for file in FILES {
        let text = std::fs::read_to_string(dir.join(file))
            .unwrap_or_else(|e| panic!("reading {file}: {e}"));
        let doc =
            benilla_ui::framexml::parse(&text).unwrap_or_else(|e| panic!("parsing {file}: {e}"));
        let report = benilla_ui::loader::load(script, &doc, &provider);
        assert!(
            report.errors.is_empty(),
            "{file} loaded with errors: {:#?}",
            report.errors
        );
    }
}

/// The two colour escapes the sheet writes, as `Fonts.xml` defines them — asserted against the
/// rendered string rather than re-derived, so a palette change fails here loudly instead of
/// silently agreeing with itself.
const GREEN: &str = "|cff20ff20";
const RED: &str = "|cffff2020";

/// A level-60 body carrying one of each case: strength heavily geared (+105), agility cursed
/// (−12), stamina untouched, and the same three shapes across the resistance schools. The negative
/// halves are the wire words an **x86**-hosted server sends; an arm64 host saturates a debuff to a
/// flat `0` on the way out (decision 1397), which is exactly why the red leg has to be exercised
/// here and cannot be exercised against the local deploy.
fn stats() -> UnitCombatStats {
    UnitCombatStats {
        stats: [225, 68, 178, 34, 51],
        stat_pos: [105, 0, 0, 4, 0],
        stat_neg: [0, -12, 0, 0, 0],
        // schools: [0] armor, then holy/fire/nature/frost/shadow/arcane.
        resistances: [2965, 0, 65, 20, 0, 0, 0],
        resistance_pos: [0, 0, 65, 0, 0, 0, 0],
        resistance_neg: [0, 0, 0, -10, 0, 0, 0],
        ..Default::default()
    }
}

fn player() -> UnitState {
    UnitState {
        exists: true,
        name: Some("Probefour".into()),
        level: 60,
        race: Some("Human".into()),
        class: Some("Warrior".into()),
        ..Default::default()
    }
}

/// Repaint the sheet the way an in-world `UNIT_STATS` does and read a row's rendered text back.
fn painted(script: &mut UiScript) {
    script
        .run("PaperDollFrame_SetStats() PaperDollFrame_SetResistances() PaperDollFrame_SetArmor()")
        .expect("the paper doll's stat rows repaint");
}

fn text_of(script: &mut UiScript, region: &str) -> String {
    script
        .eval::<String>(&format!(r#"return getglobal("{region}"):GetText()"#))
        .unwrap_or_else(|e| panic!("reading {region}: {e}"))
}

fn tooltip_of(script: &mut UiScript, frame: &str) -> String {
    script
        .eval::<String>(&format!(r#"return getglobal("{frame}").tooltip"#))
        .unwrap_or_else(|e| panic!("reading {frame}.tooltip: {e}"))
}

fn seated() -> UiScript {
    let mut script = UiScript::new().expect("a UI VM");
    load_ui(&script);
    script.set_unit("player", Some(player()));
    script.set_player_combat_stats(Some(stats()));
    script
}

#[test]
fn a_geared_stat_renders_green_and_its_tooltip_names_the_unbuffed_base() {
    let mut s = seated();
    painted(&mut s);

    // Strength: 225 total off a 120 base, +105 from gear. Green number, and the tooltip's
    // parenthesised base is the ONE subtraction the ref Lua performs (225 − 105 − 0 = 120).
    let text = text_of(&mut s, "CharacterStatFrame1StatText");
    assert!(
        text.starts_with(GREEN) && text.contains("225"),
        "a gear-boosted strength must render green, got {text:?}"
    );
    let tip = tooltip_of(&mut s, "CharacterStatFrame1");
    assert!(
        tip.contains("Strength 225") && tip.contains("(120") && tip.contains("+105"),
        "the tooltip must read the total, the unbuffed base and the delta, got {tip:?}"
    );
    assert!(
        !tip.contains("(15"),
        "base 15 = 225 − 105 − 105: the buff was deducted twice, so UnitStat's first return is \
         pre-subtracted. It must be the raw UNIT_FIELD_STAT — {tip:?}"
    );
}

#[test]
fn a_debuffed_stat_renders_red_and_an_untouched_one_stays_plain() {
    let mut s = seated();
    painted(&mut s);

    let agility = text_of(&mut s, "CharacterStatFrame2StatText");
    assert!(
        agility.starts_with(RED) && agility.contains("68"),
        "a cursed agility must render red, got {agility:?}"
    );
    // Stamina has neither half — the ref's `posBuff == 0 and negBuff == 0` leg: a bare number with
    // no escape at all. This is the assertion the shipped bug would have PASSED, and it is here so
    // a future over-correction that colours everything is caught.
    let stamina = text_of(&mut s, "CharacterStatFrame3StatText");
    assert_eq!(
        stamina, "178",
        "an unmodified stat carries no colour escape at all"
    );
    // Red wins over green when both halves are in play — the ref colours on `negBuff < 0` first.
    s.set_player_combat_stats(Some(UnitCombatStats {
        stat_pos: [105, 30, 0, 4, 0],
        ..stats()
    }));
    painted(&mut s);
    let mixed = text_of(&mut s, "CharacterStatFrame2StatText");
    assert!(
        mixed.starts_with(RED),
        "a stat with both a buff and a debuff renders RED, got {mixed:?}"
    );
}

#[test]
fn resistance_rows_colour_by_which_half_is_bigger() {
    let mut s = seated();
    painted(&mut s);

    // The five magic rows are the sheet's own school order (BENILLA_MAGICRES_IDS); resolve each
    // row's school from the frame rather than assuming, so a row-order change cannot silently
    // re-point these assertions.
    let row_of = |s: &mut UiScript, school: i64| -> usize {
        s.eval::<i64>(&format!(
            "for i = 1, NUM_RESISTANCE_TYPES do \
               if BENILLA_MAGICRES_IDS[i] == {school} then return i end \
             end return 0"
        ))
        .expect("the school→row map") as usize
    };

    let fire = row_of(&mut s, 2);
    let nature = row_of(&mut s, 3);
    assert!(fire > 0 && nature > 0, "fire and nature both have rows");

    let fire_text = text_of(&mut s, &format!("MagicResText{fire}"));
    assert!(
        fire_text.starts_with(GREEN) && fire_text.contains("65"),
        "a +65 fire resistance renders green, got {fire_text:?}"
    );
    let nature_text = text_of(&mut s, &format!("MagicResText{nature}"));
    assert!(
        nature_text.starts_with(RED),
        "a nature school whose debuff outweighs its buff renders red, got {nature_text:?}"
    );
}

/// Armor is the one row of this family the local server never decomposes — item armor lands in
/// `UNIT_FIELD_RESISTANCES[0]` itself, not in the buff split — so its plain-number leg is the live
/// shape and worth pinning as such.
#[test]
fn armor_with_no_buff_split_renders_plain() {
    let mut s = seated();
    painted(&mut s);
    assert_eq!(text_of(&mut s, "CharacterArmorFrameStatText"), "2965");
}
