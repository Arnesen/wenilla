//! `assets/ui/UIParent.xml`'s addon-facing helpers, driven from Lua the way an addon drives them.
//!
//! The panel/ESC machinery in that file is covered by `panel_tests` and `escape_tests`; this is for
//! the loose functions the reference's `UIParent.lua` also defines, which benilla itself may never
//! call and an addon calls constantly.

use benilla_ui::script::UiScript;

/// Fonts (for any `inherits=`), then UIParent — the manifest's order.
fn ui_parent() -> UiScript {
    let mut s = UiScript::new().unwrap();
    for file in ["Fonts.xml", "UIParent.xml"] {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets/ui")
                .join(file),
        )
        .unwrap();
        let doc = benilla_ui::framexml::parse(&text).unwrap();
        let report = benilla_ui::loader::load(&s, &doc, &|_| None);
        assert!(report.errors.is_empty(), "{file}: {:?}", report.errors);
    }
    s.set_screen_size(1024.0, 768.0);
    s.run(
        r#"Box = CreateFrame("Frame", "Box", UIParent)
           Box:SetWidth(100) Box:SetHeight(50)
           Box:SetPoint("BOTTOMLEFT", UIParent, "BOTTOMLEFT", 200, 300)"#,
    )
    .unwrap();
    s.resolve();
    s
}

/// **`MouseIsOver` is the hover idiom, and its edge cases are the reference's, not the obvious
/// ones** — 9 corpus addons call it.
///
/// Three behaviours that a re-implementation would get wrong and a transcription gets right: the
/// offsets are *added* to every edge (so a positive `bottomOffset` raises the bottom rather than
/// lowering it), a nil `topOffset` zeroes **all four** even if the others were passed, and the
/// bounds are strict, so a cursor exactly on an edge is outside.
#[test]
fn mouse_is_over_is_the_references_own_box_test() {
    let mut s = ui_parent();
    // The box is x 200..300, y 300..350.
    s.mouse_move(250.0, 325.0);
    assert_eq!(
        s.eval::<Option<i64>>("return MouseIsOver(Box)").unwrap(),
        Some(1),
        "dead centre"
    );

    s.mouse_move(150.0, 325.0);
    assert_eq!(
        s.eval::<Option<i64>>("return MouseIsOver(Box)").unwrap(),
        None,
        "100px to the left is out, and the miss is nil rather than false"
    );
    // ...but a leftOffset of -60 pushes the left edge out to 140 and takes it in.
    assert_eq!(
        s.eval::<Option<i64>>("return MouseIsOver(Box, 0, 0, -60, 0)")
            .unwrap(),
        Some(1),
        "offsets are ADDED to each edge: leftOffset -60 grows the box leftward"
    );

    // A positive bottomOffset SHRINKS from below — the same addition, read the other way.
    s.mouse_move(250.0, 305.0);
    assert_eq!(
        s.eval::<Option<i64>>("return MouseIsOver(Box)").unwrap(),
        Some(1)
    );
    assert_eq!(
        s.eval::<Option<i64>>("return MouseIsOver(Box, 0, 10, 0, 0)")
            .unwrap(),
        None,
        "bottom + 10 = 310, and the cursor is at 305"
    );

    // The reference's `if ( not topOffset )` zeroes ALL FOUR — so this 10 is ignored entirely.
    assert_eq!(
        s.eval::<Option<i64>>("return MouseIsOver(Box, nil, 10, 0, 0)")
            .unwrap(),
        Some(1),
        "a nil topOffset discards the other three, exactly as 1.12 does"
    );

    // Strict bounds: exactly on an edge is outside.
    s.mouse_move(200.0, 325.0);
    assert_eq!(
        s.eval::<Option<i64>>("return MouseIsOver(Box)").unwrap(),
        None,
        "`x > left`, not `>=`"
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// **`RaiseFrameLevel` / `LowerFrameLevel` are FrameXML, not engine** — an addon calling them is
/// calling a function the client's own UI defines, and `BetterCharacterStats` dies at load without
/// them (`attempt to call global 'RaiseFrameLevel'`).
///
/// Two lines each in the reference and worth a test only because the direction is easy to swap:
/// Raise is `+1`, Lower is `-1`, and both read the frame's CURRENT level rather than a stored one,
/// so repeated calls accumulate.
#[test]
fn raise_and_lower_frame_level_step_the_frames_own_level() {
    let s = ui_parent();
    let base: i64 = s.eval("return Box:GetFrameLevel()").unwrap();

    s.run("RaiseFrameLevel(Box) RaiseFrameLevel(Box)").unwrap();
    assert_eq!(
        s.eval::<i64>("return Box:GetFrameLevel()").unwrap(),
        base + 2,
        "each call reads the level back, so they accumulate"
    );
    s.run("LowerFrameLevel(Box)").unwrap();
    assert_eq!(
        s.eval::<i64>("return Box:GetFrameLevel()").unwrap(),
        base + 1
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// `randomseed` is an engine global in 1.12 beside `random`, and it was the missing half —
/// `IgniteStatus` calls it at file scope and dies on `attempt to call global`.
#[test]
fn randomseed_is_a_bare_global_like_random() {
    let s = ui_parent();
    // Seeding twice with the same value must produce the same first draw; that is the whole
    // contract an addon wants from it.
    let a: i64 = s
        .eval("randomseed(12345) return random(1, 1000000)")
        .unwrap();
    let b: i64 = s
        .eval("randomseed(12345) return random(1, 1000000)")
        .unwrap();
    assert_eq!(a, b, "the same seed gives the same sequence");
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

/// The reference's own "hack to fix a symptom not the real issue": a frame that has never been
/// laid out answers nil from `GetLeft()`, and `MouseIsOver` must return nil rather than raise.
#[test]
fn mouse_is_over_survives_a_frame_with_no_resolved_rect() {
    let mut s = ui_parent();
    s.mouse_move(250.0, 325.0);
    s.run(r#"Floating = CreateFrame("Frame", "Floating", UIParent)"#)
        .unwrap();
    assert_eq!(
        s.eval::<Option<i64>>("return MouseIsOver(Floating)")
            .unwrap(),
        None,
        "an unanchored frame is a nil answer, not an error"
    );
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}

// ── Fonts.xml's shared colour globals, as an addon reaches them ───────────────────────────────

/// **`RAID_CLASS_COLORS` is the table every raid addon paints names with** — 22 corpus addons, and
/// its absence was the whole of the harness's remaining `bad argument #1 to 'pairs' (table
/// expected, got nil)` row.
///
/// The shape matters as much as the values: addons walk it with `pairs` and index it by the
/// UPPERCASE class file token that `UnitClass`'s second return gives them, so the test does exactly
/// that rather than reading one key.
#[test]
fn raid_class_colors_is_the_references_own_nine() {
    let mut s = UiScript::new().unwrap();
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui/Fonts.xml"),
    )
    .unwrap();
    let doc = benilla_ui::framexml::parse(&text).unwrap();
    let report = benilla_ui::loader::load(&s, &doc, &|_| None);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    s.set_screen_size(1024.0, 768.0);

    // PaintChips-2.0's own line, verbatim in shape — the one that was raising.
    let n: i64 = s
        .eval("local n = 0 for i, v in pairs(RAID_CLASS_COLORS) do n = n + 1 end return n")
        .unwrap();
    assert_eq!(n, 9, "1.12 has nine playable classes and one row each");

    let keys: String = s
        .eval(
            "local t = {} for k in pairs(RAID_CLASS_COLORS) do table.insert(t, k) end \
             table.sort(t) return table.concat(t, \" \")",
        )
        .unwrap();
    assert_eq!(
        keys, "DRUID HUNTER MAGE PALADIN PRIEST ROGUE SHAMAN WARLOCK WARRIOR",
        "keyed by UnitClass's uppercase second return, which is how addons index it"
    );

    // Spot the two the reference makes identical — a "fix" here would be a divergence.
    assert_eq!(
        s.eval::<(f64, f64, f64)>(
            "local p, h = RAID_CLASS_COLORS.PALADIN, RAID_CLASS_COLORS.SHAMAN \
             return p.r - h.r, p.g - h.g, p.b - h.b"
        )
        .unwrap(),
        (0.0, 0.0, 0.0),
        "SHAMAN shares PALADIN's pink in 1.12 — transcribed, not corrected"
    );
    assert_eq!(
        s.eval::<(f64, f64, f64)>("local c = RAID_CLASS_COLORS.HUNTER return c.r, c.g, c.b")
            .unwrap(),
        (0.67, 0.83, 0.45)
    );
}

/// **The four font-PATH globals** (ref `Fonts.xml` l.4-7), which this file quoted around for a long
/// time — it took the colour block at l.8-19 and skipped the four lines above it.
///
/// `STANDARD_TEXT_FONT` is why they matter: `Dewdrop-2.0` calls
/// `button.text:SetFont(STANDARD_TEXT_FONT, height)` **unguarded** at two of its three sites, so a
/// nil global is a failed `SetFont` on every menu button the Ace2 ecosystem draws.
#[test]
fn the_font_path_globals_are_the_references_own_four() {
    let mut s = UiScript::new().unwrap();
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui/Fonts.xml"),
    )
    .unwrap();
    let doc = benilla_ui::framexml::parse(&text).unwrap();
    let report = benilla_ui::loader::load(&s, &doc, &|_| None);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    s.set_screen_size(1024.0, 768.0);

    for name in [
        "STANDARD_TEXT_FONT",
        "UNIT_NAME_FONT",
        "DAMAGE_TEXT_FONT",
        "NAMEPLATE_FONT",
    ] {
        assert_eq!(
            s.eval::<String>(&format!("return {name}")).unwrap(),
            "Fonts\\FRIZQT__.TTF",
            "{name} — 1.12 assigns the same face to all four, which is the reference's own \
             identity and not a simplification"
        );
    }

    // Dewdrop's own unguarded line, in shape: a nil global here is a failed SetFont on every
    // Ace2 menu button.
    s.run(
        r#"F = CreateFrame("Frame", "F", UIParent)
           T = F:CreateFontString("T", "ARTWORK")
           T:SetFont(STANDARD_TEXT_FONT, 10)"#,
    )
    .unwrap();
    assert!(s.errors().is_empty(), "{:?}", s.errors());
}
