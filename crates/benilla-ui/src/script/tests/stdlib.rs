//! The WoW stdlib: positional `format`, the `getglobal`/`strsplit`/`wipe` alias layer, and the
//! sandbox holes (`loadstring` text-only, dangerous globals removed).

use super::common::script;

// ── The positional format wrapper ───────────────────────────────────────────────────────────────

#[test]
fn positional_format_reorders_and_mix_is_an_error() {
    let s = script();
    assert_eq!(
        s.eval::<String>(r#"return format("%2$s %1$s", "a", "b")"#)
            .unwrap(),
        "b a"
    );
    // width/precision travel with the positional spec
    assert_eq!(
        s.eval::<String>(r#"return format("%1$05d", 42)"#).unwrap(),
        "00042"
    );
    // sequential still works (and via string.format too, which we patched)
    assert_eq!(
        s.eval::<String>(r#"return string.format("%d-%s", 1, "x")"#)
            .unwrap(),
        "1-x"
    );
    // %% is preserved
    assert_eq!(
        s.eval::<String>(r#"return format("%1$d%%", 50)"#).unwrap(),
        "50%"
    );
    // mixing positional and sequential is an error (matches Blizzard erroring)
    let mixed_ok: bool = s
        .eval(r#"return pcall(format, "%1$s %s", "a", "b")"#)
        .unwrap();
    assert!(!mixed_ok, "mixed positional+sequential must error");
}

// ── getglobal / strsplit / wipe / the alias layer ───────────────────────────────────────────────

#[test]
fn stdlib_aliases_and_helpers() {
    let s = script();
    s.run(
        r#"
        -- getglobal on a named frame
        local f = CreateFrame("Frame", "GG")
        assert(getglobal("GG") == f)

        -- strsplit returns pieces (empty fields preserved)
        local a, b, c = strsplit(",", "x,y,z")
        assert(a == "x" and b == "y" and c == "z")
        local e1, e2 = strsplit(",", ",tail")
        assert(e1 == "" and e2 == "tail")

        -- strjoin / strconcat / strtrim
        assert(strjoin("-", "a", "b", "c") == "a-b-c")
        assert(strconcat("a", "b", "c") == "abc")
        assert(strtrim("  hi \t") == "hi")

        -- the bare-global aliases
        assert(strupper("ab") == "AB" and strlower("AB") == "ab")
        assert(strsub("hello", 2, 3) == "el")
        assert(strlen("hello") == 5)
        local t = {}
        tinsert(t, 10); tinsert(t, 20)
        assert(getn(t) == 2)
        tremove(t, 1)
        assert(t[1] == 20)

        -- wipe empties a table in place
        local w = { 1, 2, x = 3 }
        assert(wipe(w) == w and next(w) == nil)

        -- tostringall
        local s1, s2 = tostringall(1, true)
        assert(s1 == "1" and s2 == "true")
    "#,
    )
    .unwrap();
}

// ── Sandbox holes ───────────────────────────────────────────────────────────────────────────────

#[test]
fn sandbox_removes_dangerous_globals() {
    let s = script();
    let all_nil: bool = s
        .eval(
            r#"return io == nil and os == nil and package == nil and require == nil
               and dofile == nil and loadfile == nil and debug == nil"#,
        )
        .unwrap();
    assert!(all_nil);
    // debugstack stub survives and returns "".
    assert_eq!(s.eval::<String>("return debugstack()").unwrap(), "");
}

#[test]
fn loadstring_is_text_only_bytecode_rejected() {
    let s = script();
    let ok: bool = s
        .eval(
            r#"
        -- valid source compiles
        local f = loadstring("return 1 + 1")
        assert(type(f) == "function" and f() == 2)
        -- bytecode is rejected: returns nil + error message
        local bc = string.dump(function() return 7 end)
        local g, err = loadstring(bc)
        return (g == nil) and (type(err) == "string")
    "#,
        )
        .unwrap();
    assert!(ok, "loadstring must reject bytecode");
}

// ── GetTime: the session clock (decision 0137 — the reference cast bar anchors on it) ───────────

#[test]
fn gettime_starts_at_zero_and_tracks_tick() {
    let mut s = script();
    assert_eq!(s.eval::<f64>("return GetTime()").unwrap(), 0.0);
    s.tick(0.25);
    s.tick(0.25);
    let t = s.eval::<f64>("return GetTime()").unwrap();
    assert!((t - 0.5).abs() < 1e-6, "two 0.25s ticks = 0.5 (got {t})");
}
