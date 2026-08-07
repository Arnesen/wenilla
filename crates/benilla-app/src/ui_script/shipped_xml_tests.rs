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

/// **Loading the UI makes no sound.** Materializing the shipped manifest is bookkeeping — nothing
/// has opened, so nothing may be heard (decision 1033).
///
/// The defect this pins: a dropdown's `OnLoad` calls `UIDropDownMenu_Initialize`, which —
/// faithfully, ref `UIDropDownMenu.lua` l.49-52 — *calls the init function immediately*. For the
/// unit popups that init function reaches `UnitPopup_ShowMenu`, whose last line is
/// `PlaySound("igMainMenuOpen")`. The ref never gets there at load: `UnitPopup_HideButtons` leaves
/// nothing but CANCEL shown for a unit that does not exist, tripping the "only one item, don't show
/// the menu" early-out. Ours was missing that hide for FOLLOW and INSPECT (ref l.304-307/316-319),
/// so all four party dropdowns rang on startup — four copies of the menu-open tack stacked in one
/// frame, on the login screen.
///
/// Deliberately asserted over the WHOLE manifest rather than the popup: any future window that
/// plays a sound from a load-time handler is the same bug, and this is where it gets caught. Sound
/// is on by default now (decision 1026), so a load-time sound is something the director hears on
/// every single launch.
#[test]
fn loading_the_shipped_ui_queues_no_sounds() {
    let mut s = benilla_ui::script::UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    let failures = super::load_default_ui(&s);
    assert!(failures.is_empty(), "manifest load errors: {failures:#?}");
    s.resolve();
    assert_eq!(
        s.take_sounds(),
        vec![],
        "loading the UI played a sound — a load-time handler is ringing; see \
         UnitPopup_HideButtons / UIDropDownMenu_Initialize (decision 1033)"
    );
}

/// **Every texture path the shipped UI names RESOLVES in the real archives** — the tripwire for a
/// mis-typed `file=`, which the renderer reports by drawing a plain WHITE QUAD and nothing else.
///
/// A sprite that fails to resolve is `None` all the way to `ui_pass`, where a texture-less quad
/// samples the shared 1×1 white image (that fallback is what makes flat-shaded quads batch, so it
/// cannot itself be made loud). The bug it hid (1046): six `file=` attributes in
/// `SpellBookFrame.xml` were written with **doubled** separators — `Interface\\SpellBook\\…`, the
/// Lua escaping, in an XML attribute where a backslash is already literal. `normalize_path` only
/// folds case and slashes, so the doubled key missed the archive hash and the pet book shipped with
/// a white slab over its autocast ring and another under its tab row. Every gate was green.
///
/// The shape half runs everywhere; the resolve half needs client data and skips without it (a
/// hand-kept list of real file names would rot into agreeing with itself — the `text=` sweep's
/// argument below). Resolution goes through the renderer's own `sprite_candidates`, not a copy of it —
/// including its `.blp`/`.tga` fallback, so the sweep accepts exactly what the renderer accepts.
#[test]
fn every_shipped_texture_path_resolves_in_the_client_archives() {
    use benilla_ui::framexml::{Element, TopLevel};

    // `file=` on a texture element is an ARCHIVE path; `<Script file=>`/`<Include file=>` name our
    // own source files and are top-level items, not elements, so walking elements can't see them.
    fn walk(el: &Element, file: &str, out: &mut Vec<(String, String, String)>) {
        for (key, value) in el.attrs() {
            let archive_path = ["file", "bgfile", "edgefile"]
                .contains(&key.to_ascii_lowercase().as_str())
                && !value.is_empty();
            if archive_path {
                out.push((file.to_string(), el.tag.clone(), value.clone()));
            }
        }
        for child in &el.children {
            walk(child, file, out);
        }
    }

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui");
    let mut refs = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("assets/ui").flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "xml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read");
        let file = path.file_name().unwrap_or_default().to_string_lossy();
        let doc = benilla_ui::framexml::parse(&text).expect("parses");
        for item in &doc.items {
            match item {
                TopLevel::Font(el) | TopLevel::Template(el) | TopLevel::Instance(el) => {
                    walk(el, &file, &mut refs);
                }
                TopLevel::Include(_) | TopLevel::Script(_) => {}
            }
        }
    }
    // Never let the sweep pass by matching nothing — the shipped UI names hundreds of textures.
    assert!(refs.len() >= 200, "only {} texture paths swept", refs.len());

    // The shape half: a doubled separator is the Lua escaping written into XML, and it resolves to
    // nothing. Checked without the client so a data-less machine still catches this exact class.
    for (file, tag, path) in &refs {
        assert!(
            !path.contains("\\\\"),
            "{file}: <{tag} file=\"{path}\"> has DOUBLED separators — XML attributes are not Lua \
             strings, so the backslashes stay doubled, the archive lookup misses, and the widget \
             draws as a white quad"
        );
    }

    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
    if !data.is_dir() {
        eprintln!(
            "skipping the resolve half: no client data at {}",
            data.display()
        );
        return;
    }
    let chain = benilla_formats::open_chain(&data).expect("open chain");
    let missing: Vec<String> = refs
        .iter()
        .filter(|(_, _, path)| {
            !crate::assets::sprite_candidates(path)
                .iter()
                .any(|c| chain.contains(c))
        })
        .map(|(file, tag, path)| format!("{file}: <{tag} file=\"{path}\">"))
        .collect();
    assert!(
        missing.is_empty(),
        "texture paths that resolve to nothing (each draws as a white quad): {missing:#?}"
    );
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

/// **The boot phase is inert.** `Fonts.xml` — the only file loaded at `Startup` since 1051 — is a
/// pure registry: it materializes no frames, so nothing of the in-game UI can draw, tick or ring
/// before a character is in the world. (1033's tack was a load-time handler firing on the login
/// screen; this is the structural half of that fix.)
#[test]
fn the_boot_phase_materializes_no_frames() {
    let mut s = benilla_ui::script::UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui/Fonts.xml");
    let text = std::fs::read_to_string(&dir).expect("Fonts.xml");
    let doc = benilla_ui::framexml::parse(&text).expect("parses");
    let provider = |_: &str| -> Option<String> { None };
    let report = benilla_ui::loader::load(&s, &doc, &provider);
    assert_eq!(
        report.frames, 0,
        "the boot-phase load materialized {} frame(s) — the login screen is meant to carry none",
        report.frames
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
}

/// **The font registry alone covers the WHOLE glyph-atlas bake plan** — the property that makes
/// 1051's split safe, asserted rather than reasoned.
///
/// Our native glue screens share the one atlas, and that atlas bakes **once**, on the first
/// `Update`, from `script.font_objects()` (`ui_text/atlas.rs` — the face size-list extension *and*
/// the outlined-cell census). So the registry must be complete at boot even though the other 55
/// manifest files now load at world entry. It is: three font objects live outside `Fonts.xml`
/// (`GameFontNormalMed1` 13, `OptionsFontHighlightMedium` 14, `OptionsFontHighlightHuge` 20), all
/// un-outlined, all at heights `Fonts.xml` already declares.
///
/// If this fails, someone added a font object to a non-`Fonts.xml` file with a new height or an
/// outline. In-game text would silently lose that variant for the whole session — the bake has
/// already happened by the time the file loads. Either move it into `Fonts.xml` or make the atlas
/// rebakeable.
#[test]
fn the_font_registry_alone_covers_the_whole_bake_plan() {
    let plan = |whole: bool| -> std::collections::BTreeSet<(String, String, String)> {
        let mut s = benilla_ui::script::UiScript::new().unwrap();
        s.set_screen_size(1024.0, 768.0);
        if whole {
            let _ = super::load_default_ui(&s);
        } else {
            let _ = super::load_font_registry(&s);
        }
        s.font_objects()
            .iter()
            .map(|f| {
                (
                    f.font.clone().unwrap_or_default().to_ascii_lowercase(),
                    format!("{:?}", f.height),
                    format!("{:?}", f.outline),
                )
            })
            .collect()
    };
    let whole = plan(true);
    let registry_only = plan(false);
    let missing: Vec<_> = whole.difference(&registry_only).collect();
    assert!(
        missing.is_empty(),
        "these (font, height, outline) combinations exist in the full manifest but NOT in the \
         boot-time font registry, so the atlas would never bake them: {missing:#?}"
    );
    // Never let this pass by finding nothing on both sides.
    assert!(
        registry_only.len() >= 19,
        "only {} combinations swept — the registry sweep broke",
        registry_only.len()
    );
}
