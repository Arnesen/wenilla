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
/// The autocast **corner brackets** (`UI-AutoCastableOverlay`), per template — the sibling pin to
/// the shine's, and the same invariant: not a magic size, but the **bracket square against its
/// button**, which is the thing an eye actually reads.
///
/// The art fills only the middle `33/64` of the texture, so the drawn size decides where the
/// brackets land. The reference gets this right on the pet button (58 x 0.5156 = 29.9 on 30) and
/// loose on the spell book (60 x 0.5156 = 30.9 on 37 — three units inside each edge). Decision
/// 1393 carries the pet button's ratio across; this test is what keeps it carried, and what will
/// catch a third template added with a borrowed number.
#[test]
fn the_autocast_brackets_reach_each_buttons_corners() {
    use benilla_ui::framexml::{Element, TopLevel};

    /// The fraction of `UI-AutoCastableOverlay.blp` the bracket art actually covers, measured off
    /// the shipped BLP (`benilla-extract blp`): art bbox 15..47 of 64.
    const ART: f32 = 33.0 / 64.0;

    fn dim(el: &Element, tag: &str, axis: &str) -> Option<f32> {
        el.children
            .iter()
            .find(|c| c.tag.eq_ignore_ascii_case(tag))
            .and_then(|n| n.children.first())
            .and_then(|d| {
                d.attrs()
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(axis))
                    .map(|(_, v)| v.clone())
            })
            .and_then(|v| v.parse().ok())
    }

    /// Walk a template, carrying the nearest enclosing button size down to the overlay.
    fn walk(el: &Element, button: Option<f32>, out: &mut Vec<(String, f32, f32)>) {
        let button = dim(el, "Size", "x")
            .filter(|_| el.tag.ends_with("Button"))
            .or(button);
        let name = el
            .attrs()
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("name"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        if name.ends_with("AutoCastable") {
            if let (Some(size), Some(btn)) = (dim(el, "Size", "x"), button) {
                out.push((name.clone(), size * ART, btn));
            }
        }
        for child in &el.children {
            walk(child, button, out);
        }
    }

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("assets/ui") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|e| e != "xml") {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("read");
        for item in benilla_ui::framexml::parse(&src).expect("parse").items {
            match item {
                TopLevel::Font(el) | TopLevel::Template(el) | TopLevel::Instance(el) => {
                    walk(&el, None, &mut found)
                }
                TopLevel::Include(_) | TopLevel::Script(_) => {}
            }
        }
    }
    assert_eq!(found.len(), 2, "expected two autocast overlays: {found:?}");
    for (name, brackets, button) in &found {
        assert!(
            (brackets / button - 0.997).abs() < 0.01,
            "{name}: a {brackets:.1}-unit bracket square on a {button}-unit button is {:.3}x — the \
             pet button's is 0.997x, which is what puts brackets IN the corners (decision 1393)",
            brackets / button
        );
    }
}

/// Every shipped autocast-shine token, and the rim/viewport ratio each one asks for — the one
/// place the widget's geometry is decided, so the one place to pin it.
///
/// The pet button is the REFERENCE's own numbers (`setAllPoints` on 30x30 at `scale="1.2"`), and
/// its ratio is why that button reads as a rim: the rim square is 1.024x its viewport, so it runs
/// ON the edge and the widget's scissor halves every star (1387/1391).
///
/// The spell book is **one deliberate deviation** (decision 1392). The reference writes
/// `scale="1.22"` into a 36-unit viewport — a 0.87x rim that floats clear of the edge, is never
/// clipped, and washes the icon; the real 1.12 client looks the same way (director-checked), so
/// this is taste, not fidelity. We write 1.44 to borrow the pet button's ratio. This test is what
/// stops that drifting, or being "corrected" back to 1.22 by someone who only read the ref.
#[test]
fn the_shine_tokens_ask_for_the_rims_we_meant() {
    use benilla_ui::framexml::{Element, TopLevel};

    fn walk(el: &Element, out: &mut Vec<(String, f32, f32)>) {
        for (key, value) in el.attrs() {
            let scale = crate::autocast_shine::token_model_scale(value);
            let is_token = key.eq_ignore_ascii_case("file")
                && value.starts_with(crate::autocast_shine::SHINE_TOKEN);
            if let (true, Some(scale)) = (is_token, scale) {
                let view = el
                    .children
                    .iter()
                    .find(|c| c.tag.eq_ignore_ascii_case("Size"))
                    .and_then(|sz| sz.children.first())
                    .and_then(|d| {
                        d.attrs()
                            .iter()
                            .find(|(k, _)| k.eq_ignore_ascii_case("x"))
                            .map(|(_, v)| v.to_string())
                    })
                    .and_then(|v| v.parse::<f32>().ok())
                    // No <Size> means setAllPoints on the pet button, which is 30x30.
                    .unwrap_or(30.0);
                out.push((
                    el.attrs()
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("name"))
                        .map(|(_, v)| v.to_string())
                        .unwrap_or_default(),
                    0.02 * 1280.0 * scale,
                    view,
                ));
            }
        }
        for child in &el.children {
            walk(child, out);
        }
    }

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("assets/ui") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|e| e != "xml") {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("read");
        for item in benilla_ui::framexml::parse(&src).expect("parse").items {
            match item {
                TopLevel::Font(el) | TopLevel::Template(el) | TopLevel::Instance(el) => {
                    walk(&el, &mut found)
                }
                TopLevel::Include(_) | TopLevel::Script(_) => {}
            }
        }
    }
    assert_eq!(
        found.len(),
        2,
        "expected exactly two shine tokens: {found:?}"
    );
    for (name, rim, view) in &found {
        assert!(
            (rim / view - 1.024).abs() < 1e-3,
            "{name}: a {rim}-unit rim in a {view}-unit viewport is {:.3}x, not the pet button's \
             1.024x — a rim that does not reach its own viewport edge is never clipped, and reads \
             as a wash rather than a rim (decisions 1387/1392)",
            rim / view
        );
    }
}

/// hand-kept list of real file names would rot into agreeing with itself — the `text=` sweep's
/// argument below). Resolution goes through the renderer's own `sprite_candidates`, not a copy of it —
/// including its `.blp`/`.tga` fallback, so the sweep accepts exactly what the renderer accepts.
#[test]
fn every_shipped_texture_path_resolves_in_the_client_archives() {
    use benilla_ui::framexml::{Element, TopLevel};

    // `file=` on a texture element is an ARCHIVE path; `<Script file=>`/`<Include file=>` name our
    // own source files and are top-level items, not elements, so walking elements can't see them.
    fn walk(el: &Element, file: &str, out: &mut Vec<(String, String, String)>) {
        // `<Model file=>` names a **model** (`.mdx`), not a texture: it never reaches
        // `sprite_candidates` and it cannot draw a white quad, because this engine renders no
        // FrameXML models at all. The one shipped case is the reference's `CooldownFrameTemplate`,
        // transcribed with its own attributes so an addon's `inherits=` resolves, while the sweep
        // it drives lives in our native `<Cooldown>` widget instead (decision 0263).
        if el.tag.eq_ignore_ascii_case("Model") {
            return;
        }
        for (key, value) in el.attrs() {
            let archive_path = ["file", "bgfile", "edgefile"]
                .contains(&key.to_ascii_lowercase().as_str())
                && !value.is_empty();
            // The autocast-shine token (decision 1383) is a REGISTRATION, not an archive path:
            // conversion intercepts it before the resolver and it draws nothing, so it is
            // exempt — through the token's OWN parser, so a typo'd token (or an unparseable
            // `scale=` suffix) still fails this sweep as the white quad it would actually be.
            if archive_path && crate::autocast_shine::token_model_scale(value).is_none() {
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

    let data = benilla_formats::wow_data_or_skip!();
    let chain = benilla_formats::open_chain(&data).expect("open chain");
    let missing: Vec<String> = refs
        .iter()
        .filter(|(_, _, path)| {
            !benilla_assets::sprite_candidates(path)
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

/// **Every archive path a shipped LUA chunk names survives its own escaping** — the mirror image
/// of the doubled-separator check above, and the tripwire for the defect that left the raid tab
/// with no window art at all (the `FriendsFrame_Update` arm was written
/// `"Interface\PaperDollInfoFrame\…"`, one backslash, so Lua ate both separators and
/// `SetTexture` was handed `InterfacePaperDollInfoFrameUI-Character-General-TopLeft`).
///
/// The two halves of the same trap, and they pull in OPPOSITE directions, which is exactly why
/// neither check can stand in for the other:
///
/// - an **XML attribute** is not a Lua string, so `file="Interface\Buttons\X"` is right and a
///   doubled separator there is the bug (the sweep above);
/// - a **Lua literal** is, so `SetTexture("Interface\\Buttons\\X")` is right and a single
///   separator here is the bug — and it fails SILENTLY: `\P` is not an escape Lua rejects, it is
///   one Lua drops the backslash from, so the string is well-formed, the call succeeds, and the
///   only symptom is a texture that never appears.
///
/// The shape half runs without client data; the resolution half needs the archives and skips
/// without them, the same posture as its sibling.
#[test]
fn every_archive_path_a_shipped_lua_chunk_names_survives_its_own_escaping() {
    use benilla_ui::framexml::{Element, ScriptRef, TopLevel};

    // The archive roots a path literal can start with. A prefix list rather than "has a
    // backslash", because Lua strings legitimately carry `\n`/`\124` and those are not paths.
    const ROOTS: [&str; 7] = [
        "interface",
        "textures",
        "world",
        "sound",
        "character",
        "item",
        "spells",
    ];

    /// Every `"…"`/`'…'` literal in one Lua chunk, as its RAW source text (escapes uninterpreted —
    /// what this sweep is looking for is an escape Lua would have eaten, so interpreting them
    /// first would destroy the evidence).
    ///
    /// **Comments and long strings are skipped, not scanned.** A `--` line quoting a path (this
    /// house writes plenty: "the file is `Interface\\Foo\\Bar`") is prose, and a prose backslash
    /// is nobody's bug — flagging one would train the next session to weaken this test rather
    /// than read it.
    fn literals(chunk: &str) -> Vec<(String, bool)> {
        let src: Vec<char> = chunk.chars().collect();
        let at = |i: usize, s: &str| src[i..].starts_with(&s.chars().collect::<Vec<_>>()[..]);
        let mut out = Vec::new();
        let mut i = 0;
        while i < src.len() {
            if at(i, "--") {
                i = if at(i + 2, "[[") {
                    src[i..]
                        .windows(2)
                        .position(|w| w == [']', ']'])
                        .map_or(src.len(), |k| i + k + 2)
                } else {
                    src[i..]
                        .iter()
                        .position(|&c| c == '\n')
                        .map_or(src.len(), |k| i + k + 1)
                };
                continue;
            }
            if at(i, "[[") {
                i = src[i + 2..]
                    .windows(2)
                    .position(|w| w == [']', ']'])
                    .map_or(src.len(), |k| i + 2 + k + 2);
                continue;
            }
            let quote = src[i];
            if quote != '"' && quote != '\'' {
                i += 1;
                continue;
            }
            let (mut raw, mut j, mut closed) = (String::new(), i + 1, false);
            while j < src.len() {
                let c = src[j];
                if c == '\\' && j + 1 < src.len() {
                    raw.push(c);
                    raw.push(src[j + 1]);
                    j += 2;
                    continue;
                }
                // A newline before the closing quote means this was never a literal (an
                // apostrophe in a comment, most often) — drop it and resume at the next char.
                if c == '\n' {
                    break;
                }
                if c == quote {
                    closed = true;
                    break;
                }
                raw.push(c);
                j += 1;
            }
            if closed {
                // Is this the WHOLE path, or a fragment? A literal with `..` against either side
                // is being concatenated onto, and one carrying a `%` spec is a `format` template —
                // in both cases the string in the source is a prefix and resolving it would fail
                // by construction. Read off the SITE rather than guessed from the text (a
                // "ends with a dash" rule would be a list to feed forever), so a fragment that
                // later becomes whole starts being resolved on its own.
                let before = src[..i].iter().rposition(|c| !c.is_whitespace());
                let after = src[j + 1..].iter().position(|c| !c.is_whitespace());
                let joined = before.is_some_and(|k| k >= 1 && src[k] == '.' && src[k - 1] == '.')
                    || after.is_some_and(|k| {
                        src.get(j + 1 + k) == Some(&'.') && src.get(j + 2 + k) == Some(&'.')
                    });
                // …and the two fragments a `..` cannot see, because they are assigned to a
                // variable first and joined somewhere else: a literal ending in the SEPARATOR is a
                // directory, and one ending in a DASH is a family prefix (`"…\\MageFire-"` .. rank).
                // Neither is a file name — no texture in the chain ends in either character — so
                // this excludes fragments without excluding anything real.
                let fragment = raw.ends_with('\\') || raw.ends_with('-');
                let whole = !joined && !fragment && !raw.contains('%');
                out.push((raw, whole));
                i = j + 1;
            } else {
                i += 1;
            }
        }
        out
    }

    // Every Lua chunk in the file: the top-level `<Script>` blocks and every element body (the
    // `<OnLoad>`-family handlers). Through the PARSER, not a text scan — an attribute value is
    // also inside double quotes, and a text scan cannot tell the two apart, which is the whole
    // distinction this test exists to make.
    fn bodies(el: &Element, out: &mut Vec<String>) {
        if !el.body.trim().is_empty() {
            out.push(el.body.clone());
        }
        for child in &el.children {
            bodies(child, out);
        }
    }

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui");
    let mut paths: Vec<(String, String, bool)> = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("assets/ui").flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "xml") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("read");
        let file = path.file_name().unwrap_or_default().to_string_lossy();
        let mut chunks = Vec::new();
        for item in &benilla_ui::framexml::parse(&text).expect("parses").items {
            match item {
                TopLevel::Script(ScriptRef::Inline { body, .. }) => chunks.push(body.clone()),
                TopLevel::Font(el) | TopLevel::Template(el) | TopLevel::Instance(el) => {
                    bodies(el, &mut chunks);
                }
                TopLevel::Script(ScriptRef::File(_)) | TopLevel::Include(_) => {}
            }
        }
        for (raw, whole) in chunks.iter().flat_map(|c| literals(c)) {
            let lower = raw.to_ascii_lowercase();
            if ROOTS.iter().any(|r| lower.starts_with(r)) && raw.contains('\\') {
                paths.push((file.to_string(), raw, whole));
            }
        }
    }
    // Never let the sweep pass by matching nothing.
    assert!(
        paths.len() >= 20,
        "only {} archive paths swept out of the shipped Lua",
        paths.len()
    );

    // The shape half: after collapsing every `\\` pair, no backslash may remain — one that does
    // is a separator Lua is about to eat.
    for (file, raw, _) in &paths {
        assert!(
            !raw.replace("\\\\", "").contains('\\'),
            "{file}: the Lua literal \"{raw}\" has SINGLE separators — Lua drops the backslash \
             from every one of them, so the path arrives with its folders run together, the \
             archive lookup misses, and the texture silently never appears. Double them."
        );
    }

    let data = benilla_formats::wow_data_or_skip!();
    let chain = benilla_formats::open_chain(&data).expect("open chain");
    let missing: Vec<String> = paths
        .iter()
        // `sprite_candidates` answers for TEXTURES; a sound or a model path is a different
        // resolver's business, so only the two texture roots take the resolution half — and only
        // a WHOLE path, never a concatenation fragment.
        .filter(|(_, raw, whole)| {
            let lower = raw.to_ascii_lowercase();
            *whole && (lower.starts_with("interface") || lower.starts_with("textures"))
        })
        .filter(|(_, raw, _)| {
            let real = raw.replace("\\\\", "\\");
            !benilla_assets::sprite_candidates(&real)
                .iter()
                .any(|c| chain.contains(c))
        })
        .map(|(file, raw, _)| format!("{file}: \"{raw}\""))
        .collect();
    assert!(
        missing.is_empty(),
        "archive paths a shipped Lua chunk names that resolve to nothing: {missing:#?}"
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
    let data = benilla_formats::wow_data_or_skip!();
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
    let provider = |_: &str| -> Option<Vec<u8>> { None };
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

/// The whole shipped UI survives `VARIABLES_LOADED` (decision 1128) — the event the saved-variables
/// load fires at every launch, and which nothing fired before this arc existed.
///
/// Today no shipped file registers it, so this asserts a clean no-op; the moment one does (the
/// combat-text option family and `TwentyFourHourTime` are next on 1128's list) this is what catches a
/// handler that errors on the one event that runs before any window has been shown.
#[test]
fn the_shipped_ui_takes_variables_loaded_without_a_script_error() {
    let mut s = benilla_ui::script::UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    let failures = super::load_default_ui(&s);
    assert!(failures.is_empty(), "loader errors: {failures:?}");
    let _ = s.errors(); // drain anything the load itself logged; this test is about the event
    s.fire_event("VARIABLES_LOADED", vec![]);
    assert!(
        s.errors().is_empty(),
        "VARIABLES_LOADED script errors: {:?}",
        s.errors()
    );
}

/// **Every `<Font name=…>` the shipped manifest declares is a real Lua global** — a `Font` object
/// answering the FontInstance getters, not a bare style record with no name.
///
/// The per-fragment test in `benilla-ui` proves the mechanism on a two-font document; this proves
/// it over the 54 fonts our real `Fonts.xml` (and the windows after it) actually declare, in
/// manifest order, which is the only place a name collision with a *frame* of the same name — the
/// one way publication can silently not happen, since `publish_global` never overwrites — could
/// show up.
///
/// The named spot-checks are the corpus's four most-wanted font objects: `GameFontNormal` (98
/// addons), `GameTooltipText` (89), `GameFontHighlightSmall` (69), `GameTooltipHeaderText` (the
/// `Tablet-2.0.lua:289` header-size probe, 268 read sites).
#[test]
fn every_shipped_font_object_is_published_as_a_lua_global() {
    let mut s = benilla_ui::script::UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    let failures = super::load_default_ui(&s);
    assert!(failures.is_empty(), "loader errors: {failures:?}");

    // Collect the declared names straight out of the shipped XML, so the sweep cannot go stale.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui");
    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("assets/ui").flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "xml") {
            let text = std::fs::read_to_string(&path).expect("read");
            for chunk in text.split("<Font ").skip(1) {
                if let Some(rest) = chunk.split_once("name=\"") {
                    if let Some((name, _)) = rest.1.split_once('"') {
                        names.push(name.to_string());
                    }
                }
            }
        }
    }
    names.sort();
    names.dedup();
    assert!(
        names.len() >= 50,
        "only {} <Font name=> declarations found — the sweep broke",
        names.len()
    );

    let mut unpublished: Vec<&str> = Vec::new();
    for name in &names {
        match s.eval::<String>(&format!("return {name}:GetObjectType()")) {
            Ok(t) if t == "Font" => {}
            _ => unpublished.push(name),
        }
    }
    assert!(
        unpublished.is_empty(),
        "declared <Font name=> that is not a Font global: {unpublished:?}"
    );

    // The four the corpus wants most actually carry a face and a size, not just a name.
    for name in [
        "GameFontNormal",
        "GameTooltipText",
        "GameFontHighlightSmall",
        "GameTooltipHeaderText",
    ] {
        let (face, height) = s
            .eval::<(String, f32)>(&format!("return {name}:GetFont()"))
            .unwrap_or_else(|e| panic!("{name}:GetFont() — {e}"));
        assert!(
            face.to_ascii_uppercase().ends_with(".TTF"),
            "{name}: {face}"
        );
        assert!(height > 0.0, "{name}: height {height}");
    }
}

/// **Two reference templates addons INHERIT, and the silence that hid them.**
///
/// `TargetBuffButtonTemplate` (ref `TargetFrame.xml`) and `MainMenuBarMicroButton` (ref
/// `MainMenuBarMicroButtons.xml`) are declared by the reference and were not by us. An unresolved
/// `inherits=` is **silent** — no load error, no session error, nothing in any harness column — so
/// a consumer just gets a button with no size, no hit-rect and no scripts, and nothing anywhere
/// says so. They surfaced only in the report's missing-TEMPLATES ranking.
///
/// Asserted through what a consumer actually gets: inherit the template, then read back the
/// geometry and the script the reference confers.
#[test]
fn the_inheritable_reference_templates_confer_their_shape() {
    let mut s = benilla_ui::script::UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    assert!(super::load_default_ui(&s).is_empty());

    // `CT_UnitFrames/CT_TargetFrame.xml:4` builds its own virtual template on top of this one, so
    // the inherit has to work through a second hop as well as directly.
    s.run(
        r#"
        BuffProbe = CreateFrame("Button", "BuffProbe", UIParent, "TargetBuffButtonTemplate")
        MicroProbe = CreateFrame("Button", "MicroProbe", UIParent, "MainMenuBarMicroButton")
        "#,
    )
    .unwrap();
    s.resolve();

    assert_eq!(
        s.eval::<(f64, f64)>("return BuffProbe:GetWidth(), BuffProbe:GetHeight()")
            .unwrap(),
        (21.0, 21.0),
        "the buff button carries the reference's 21x21"
    );
    assert!(
        s.eval::<bool>("return BuffProbeIcon ~= nil").unwrap(),
        "$parentIcon is the region a consumer getglobals to set the texture"
    );
    assert!(
        s.eval::<bool>(r#"return BuffProbe:GetScript("OnEnter") ~= nil"#)
            .unwrap(),
        "the tooltip script comes with the template"
    );

    assert_eq!(
        s.eval::<(f64, f64)>("return MicroProbe:GetWidth(), MicroProbe:GetHeight()")
            .unwrap(),
        (29.0, 58.0)
    );
    assert!(s
        .eval::<bool>(r#"return MicroProbe:GetScript("OnEnter") ~= nil"#)
        .unwrap());
    assert!(s.errors().is_empty(), "no script errors: {:?}", s.errors());
}

/// **`CursorUpdate` / `CursorOnUpdate` — the inspect-cursor pair, transcribed from FrameXML.**
///
/// Both are `framexml` origin in `reference/1.12-globals.tsv` (while `ResetCursor` and
/// `ShowInspectCursor` beside them are `engine`), so they are ours to write rather than to bind.
/// Two corpus addons call `CursorUpdate` and one calls `CursorOnUpdate`; it is also one of the
/// blockers on two reference templates addons inherit, and the sourced `ContainerFrame.lua` calls
/// it from its keyring fork.
#[test]
fn the_inspect_cursor_pair_takes_both_arms() {
    let mut s = benilla_ui::script::UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    assert!(super::load_default_ui(&s).is_empty());

    // Both exist as functions — the shape a caller checks before hooking.
    assert_eq!(
        s.eval::<(String, String)>("return type(CursorUpdate), type(CursorOnUpdate)")
            .unwrap(),
        ("function".into(), "function".into())
    );

    // **The no-ctrl arm, which is the one that runs in practice.** `this.hasItem` is read as a
    // plain truthy field, so a frame that never sets it simply takes `ResetCursor` — this is why
    // the pair is safe to ship before anything sets that field.
    s.run(
        r#"
        CursorProbe = CreateFrame("Frame", "CursorProbe", UIParent)
        this = CursorProbe
        CursorUpdate()
        this = nil
        "#,
    )
    .unwrap();
    assert!(
        s.errors().is_empty(),
        "the ResetCursor arm runs clean: {:?}",
        s.errors()
    );

    // `CursorOnUpdate` gates on tooltip ownership, so with the tooltip unowned it must do nothing
    // at all rather than reach through to the cursor.
    s.run("this = CursorProbe CursorOnUpdate() this = nil")
        .unwrap();
    assert!(
        s.errors().is_empty(),
        "the unowned-tooltip gate short-circuits: {:?}",
        s.errors()
    );
}
