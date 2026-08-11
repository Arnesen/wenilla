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

/// The rest of the bare globals Blizzard's own restricted scope enumerates (decision 1187).
///
/// The list is not from memory: `Blizzard_RestrictedAddOnEnvironment/RestrictedEnvironment.lua`
/// copies exactly these out of the engine, which is what distinguishes them from the table helpers
/// (`tContains`, `tInvert`, …) that FrameXML defines in Lua and we must therefore NOT supply.
///
/// The measured cost of their absence: Bagnon + BagBrother produced 94 load failures, 81 of them
/// `Cannot find a library instance of "…"`, because `strmatch` was nil so every LibStub-registering
/// library aborted before it could register.
#[test]
fn the_rest_of_the_bare_globals() {
    let s = script();
    s.run(
        r#"
        -- string family
        assert(strbyte("A") == 65 and strchar(65) == "A")

        -- and the Era-only names 1187 added are GONE (decision 1189): the 5.0 client has no
        -- string.match/gmatch, and claiming otherwise misleads an addon that feature-detects.
        assert(strmatch == nil and gmatch == nil and strrev == nil)
        assert(strlenutf8 == nil and strcmputf8i == nil)
        assert(securecall == nil and hooksecurefunc == nil and issecure == nil)

        -- math family
        assert(exp(0) == 1 and log(1) == 0 and log10(100) == 2)
        assert(frexp(8) == 0.5 and ldexp(0.5, 4) == 8)

        -- the trig globals are DEGREE-based, inverses included — same family as the verified
        -- sin/cos, and what every addon rotation helper assumes.
        assert(math.abs(tan(45) - 1) < 1e-9)
        assert(math.abs(asin(1) - 90) < 1e-9)
        assert(math.abs(acos(1)) < 1e-9)
        assert(math.abs(atan(1) - 45) < 1e-9)
        assert(math.abs(atan2(1, 0) - 90) < 1e-9)
    "#,
    )
    .unwrap();
}

/// **The Lua 5.0 dialect a vanilla addon is written in runs on our 5.1 VM** — measured, because
/// this question has been answered three different ways from memory.
///
/// 1.12 runs Lua 5.0 (byte-confirmed in wow-5875-re: `0x811b30 = "Lua: Lua 5.0 Copyright..."`);
/// we run 5.1 via mlua's `lua51`. Decision 1188 called that "the deepest divergence and it is
/// unresolved" and told the next session to test it; 1189 replied that 0068 had already closed it.
/// Meanwhile five of our own transcribed FrameXML files carried the opposite claim in a comment —
/// *"`LUA_COMPAT_VARARG` isn't shipped"* — each citing the others as precedent, so a false fact
/// propagated by citation without anyone re-running it.
///
/// It is shipped. This asserts the five idioms vanilla addon code actually uses, so the next
/// session reads a result instead of a recollection. **The implicit `arg` table is the one that
/// matters**: it is the difference between a vanilla addon's vararg functions working and every
/// one of them erroring at runtime, which is 1188's stated redirect-the-arc risk.
#[test]
fn the_lua_5_0_dialect_vanilla_addons_are_written_in_runs_here() {
    let s = script();
    s.run(
        r##"
        -- The implicit vararg table, 5.0's spelling of what 5.1 does with `...`.
        local function varargs(...) return arg.n, arg[1], arg[2] end
        local n, first, second = varargs("a", "b")
        assert(n == 2 and first == "a" and second == "b")

        -- **The one real edge**, and it is compat mode's rule rather than a gap: `arg` is
        -- synthesized only for a vararg function that does NOT also mention `...` in its body.
        -- Use both spellings in one function and `arg` is nil. Vanilla addon code is uniformly
        -- 5.0 and never mixes them, so this costs an addon nothing — it only means one of OUR
        -- transcriptions must pick a spelling per function and stay with it.
        local function mixed(...) return arg == nil and select("#", ...) or -1 end
        assert(mixed(1, 2, 3) == 3)

        -- `...` alone is the 5.1 spelling and is unaffected.
        local function modern(...) return select("#", ...) end
        assert(modern("x", "y") == 2)

        -- 5.0's table/string/math spellings, all of which 5.1 renamed.
        assert(table.getn({ 1, 2, 3 }) == 3)
        assert(string.gfind ~= nil)          -- 5.1 renamed this to string.gmatch
        assert(math.mod(7, 3) == 1)
        assert(7 % 3 == 1)                   -- and the 5.1 operator 5.0 lacks also works
    "##,
    )
    .unwrap();
}
