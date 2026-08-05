//! Whole-directory guards over the shipped `assets/ui/*.xml` — the coverage the per-window test
//! modules structurally cannot give, since each of those loads only the file it is about.

/// EVERY shipped `assets/ui/*.xml` parses — not just the ones a window test happens to load.
///
/// The per-window tests each load their own file, which left real holes: `CraftFrame.xml` and
/// `TradeSkillFrame.xml` ship in [`super::load_default_ui`]'s manifest and had NO parse coverage at
/// all, because the manifest is an inline array no test walks. A malformed comment in either (XML
/// forbids `--` inside `<!-- -->`, which a prose edit hits easily) would have reached a real run
/// untouched by a green suite.
///
/// Deliberately parse-only, not `loader::load`: loading one file out of manifest order reports
/// legitimate errors for templates its predecessors define, so a load-sweep would have to duplicate
/// the load order to say anything. Parsing is order-free and catches the whole well-formedness
/// class on its own.
#[test]
fn every_shipped_ui_xml_parses() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("assets/ui").flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "xml") {
            let text = std::fs::read_to_string(&path).expect("read");
            if let Err(e) = benilla_ui::framexml::parse(&text) {
                panic!("{}: {e}", path.display());
            }
            checked += 1;
        }
    }
    // Never let the sweep pass by finding nothing — a moved assets dir would otherwise turn this
    // into a test that guards zero files while staying green.
    assert!(
        checked >= 40,
        "only {checked} xml files swept — sweep broke"
    );
}

/// The WHOLE manifest LOADS, in its real order, with zero loader errors — the check the parse
/// sweep above structurally cannot give, and the one the app itself never made.
///
/// [`super::load_default_ui`] only ever *logged* its errors, so a broken manifest entry reached a
/// real run behind a log line nobody greps: a mistyped file name, a frame name colliding with a
/// later window's, a template used before its definer, an `<Include>` that resolves to nothing.
/// Nor could a capture run have caught it — captures skip the manifest entirely unless
/// `WOW_CAPTURE_UI=1`. This is that assertion, over the array the app really walks, so a new
/// window's entry is covered the moment it is added rather than when someone remembers to test it.
#[test]
fn the_whole_shipped_manifest_loads_without_errors() {
    let mut s = benilla_ui::script::UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    let failures = super::load_default_ui(&s);
    assert!(failures.is_empty(), "manifest load errors: {failures:#?}");
    s.resolve();
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// **Every `text=` in the shipped UI is answerable against the REAL `GlobalStrings.lua`** — the
/// tripwire for the defect that put "CREATE_MACROS" across the macro window's title bar (0991).
///
/// `text=` is a global-string LOOKUP, not a literal (wow-re rf28 l.36/l.115 → `FrameScript_GetText
/// 0x703bf0`). The loader didn't do the lookup at all, so **23 key-shaped values across six
/// windows** were rendering as their own key names — and nothing anywhere said so. Two halves, both
/// needed:
///
/// - a **key-shaped** value (`SCREAMING_SNAKE`) must resolve to a non-empty string, or it reaches a
///   real screen as its own key;
/// - a **literal** value ("Send Mail") must NOT collide with a real global, because the loader's
///   deliberate literal-fallback divergence means a collision would silently swap the words on
///   screen for a localized string nobody asked for.
///
/// Skips without client data (the real string table is the whole point — a hand-kept copy of the
/// keys would rot into agreeing with itself).
#[test]
fn every_shipped_text_attribute_answers_against_the_real_global_strings() {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    if !data.is_dir() {
        eprintln!("skipping: vanilla client not present at {}", data.display());
        return;
    }
    let mut chain = benilla_formats::open_chain(&data).expect("open chain");
    let src = chain
        .read_file("Interface\\FrameXML\\GlobalStrings.lua")
        .expect("GlobalStrings.lua in the chain");
    let s = benilla_ui::script::UiScript::new().expect("VM");
    s.run(&String::from_utf8_lossy(&src)).expect("runs clean");

    // The loader's own shape test, restated here so the two can't drift apart silently.
    let key_shaped = |v: &str| {
        v.len() >= 2
            && v.chars().any(|c| c.is_ascii_uppercase())
            && v.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    };

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui");
    let mut keys = 0;
    for entry in std::fs::read_dir(&dir).expect("assets/ui").flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "xml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read");
        let file = path.file_name().unwrap_or_default().to_string_lossy();
        for value in text
            .match_indices("text=\"")
            .filter_map(|(i, m)| text[i + m.len()..].split('"').next())
        {
            let resolved = s.lua().globals().get::<String>(value).ok();
            if key_shaped(value) {
                assert!(
                    resolved.is_some_and(|t| !t.is_empty()),
                    "{file}: text=\"{value}\" is shaped like a GlobalStrings key but the real \
                     GlobalStrings.lua has no such string — it would render as its own key name"
                );
                keys += 1;
            } else {
                assert!(
                    resolved.is_none(),
                    "{file}: the literal text=\"{value}\" collides with a real GlobalStrings key — \
                     the loader would silently show that string's value instead of these words"
                );
            }
        }
    }
    // Never let the sweep pass by matching nothing: 23 key-shaped values across six windows is the
    // floor as of 0991, and a regex that stops matching is exactly how this guard would retire.
    assert!(keys >= 23, "only {keys} key-shaped text= values swept");
}

/// **The `$parentTextureFrame` idiom's contract, over the whole shipped UI**: a frame whose art is
/// meant to cap a unit frame must sit at a strictly HIGHER frame level than that frame's status
/// bars — because frame level is the only key term above the draw layer, and the layer would
/// otherwise lift an ARTWORK bar fill straight over BACKGROUND art.
///
/// Decision 0884 pinned the layer as bucket-wide and above the frame. Two files had been getting
/// this right for the wrong reason — `UnitFrames.xml` and `PartyFrame.xml` both *declared* the
/// TextureFrame after the bars and leaned on the retired key's insertion-order tie-break — so both
/// inverted the instant the key was corrected, and both reached the director's screen. The
/// reference spends a real frame level on this in both of its own spellings (`TargetFrame.lua`
/// l.32-34's explicit `SetFrameLevel(textureFrame-1)`, `PlayerFrame.xml` l.50-52's two anonymous
/// nesting frames); benilla now uses the first, everywhere.
///
/// Name-driven on purpose, so a *new* copy of the idiom is covered the day it is written rather
/// than when someone remembers to test it. A geometric sweep was run once over this same UI —
/// every substantially-overlapping cross-frame quad pair at one `(strata, level)`, i.e. every pair
/// the draw layer alone orders — and found no third instance; the surviving 45 pairs were all
/// benign (action-bar and bag-slot chrome adjacency, text over art, and a bar fill over its own
/// frame's trough, which is the intended order). It is not kept as a gate: frozen, that list would
/// churn on every action-bar edit, and a noisy gate is a gate nobody reads.
#[test]
fn every_texture_frame_outranks_its_status_bars() {
    use benilla_ui::order::unpack;

    let mut s = benilla_ui::script::UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    let failures = super::load_default_ui(&s);
    assert!(failures.is_empty(), "manifest load errors: {failures:#?}");
    // Every unit-frame family painting at once: the frames hide themselves without a unit, and a
    // hidden frame emits no quads to read a level from.
    for unit in ["player", "target", "party1", "party2", "party3", "party4"] {
        s.set_unit(
            unit,
            Some(benilla_ui::script::UnitState {
                exists: true,
                name: Some("Someone".into()),
                health: 60,
                max_health: 100,
                level: 60,
                power_type: 0,
                power: 60,
                max_power: 100,
                ..benilla_ui::script::UnitState::default()
            }),
        );
    }
    s.fire_event("PLAYER_ENTERING_WORLD", vec![]);
    s.fire_event("PARTY_MEMBERS_CHANGED", vec![]);
    s.resolve();

    // owner frame name → its (strata, level), read off the packed draw key the renderer sorts by.
    let mut level_of: std::collections::BTreeMap<String, (u8, u16)> =
        std::collections::BTreeMap::new();
    for q in s.extract() {
        if let Some(name) = s.quad_owner_name(q.target) {
            let p = unpack(q.z);
            level_of.insert(name, (p.strata, p.level));
        }
    }

    let mut checked = 0;
    for (texture_frame, &(tf_strata, tf_level)) in &level_of {
        let Some(base) = texture_frame.strip_suffix("TextureFrame") else {
            continue;
        };
        for suffix in ["HealthBar", "PowerBar", "ManaBar"] {
            let bar = format!("{base}{suffix}");
            let Some(&(bar_strata, bar_level)) = level_of.get(&bar) else {
                continue;
            };
            assert_eq!(
                bar_strata, tf_strata,
                "{bar} and {texture_frame} must share a strata for the level to decide"
            );
            assert!(
                tf_level > bar_level,
                "{texture_frame} (level {tf_level}) must outrank {bar} (level {bar_level}): \
                 tied, the draw layer lifts the bar's ARTWORK fill over the frame's BACKGROUND art"
            );
            checked += 1;
        }
    }
    // Player + target (health/power) and four party members (health/mana) — never let this pass by
    // matching nothing, which is exactly how a renamed frame would silently retire the check.
    assert!(
        checked >= 12,
        "only {checked} texture-frame/bar pairs checked — the name sweep found nothing"
    );
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}
