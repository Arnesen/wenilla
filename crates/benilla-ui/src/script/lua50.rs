//! **The Lua 5.0 dialect** — the layer that makes an mlua 5.1 VM answer the questions a 1.12 addon
//! asks about its interpreter (decision 1194).
//!
//! The 1.12.1 client embeds **Lua 5.0**; we embed mlua's `lua51`. Every prior decision in this
//! crate treated that as a detail — 0068 called the target "stock Lua 5.1", and the WoW stdlib
//! layer beside this one is written as aliases *onto* 5.1. It is not a detail. It is the single
//! largest measured blocker in the whole addon arc:
//!
//! ```text
//! what stopped them (addons whose FIRST load error was each):
//!    61  error: 'X' is obsolete          <- table.setn, raised by Lua 5.1
//! ```
//!
//! Sixty-one of 218 real vanilla addons, plus ~23 more that merely depend on one of them, stopped
//! on their first chunk. The chain is worth spelling out because it is not obvious and because
//! **we caused the last link ourselves**: `AceLibrary.lua` opens with
//!
//! ```lua
//! local version = GetBuildInfo()
//! if string.find(version, "^2%.") then
//!     table_setn = function() end   -- TBC: Lua 5.1, setn is gone
//! else
//!     table_setn = table.setn       -- 1.12: Lua 5.0, setn is real
//! end
//! ```
//!
//! Ace asks the client which client it is and picks the right dialect. We started answering
//! `"1.12.1"` truthfully (1192 phase 5, `GetBuildInfo`), Ace correctly took the 5.0 branch, and our
//! 5.1 VM raised. The addon was right, the API was right, and the interpreter underneath was
//! lying. That is what this module fixes.
//!
//! ## What it does, in three parts
//!
//! 1. **`table.setn`/`table.getn` get their 5.0 meaning back.** 5.0 remembers a table's size —
//!    in a numeric `t.n` field if one already exists, otherwise in a private weak-keyed table
//!    where nothing walking the table can see it. 5.1 removed the concept and made `setn` raise.
//!    The exact mechanism matters more than it looks: see [`LUA_5_0_TABLE_SIZE`] for the draft
//!    that got it wrong and the 39 addons that noticed.
//! 2. **The 5.1-only members are removed.** `string.gmatch`/`match`/`reverse`, `table.maxn`,
//!    `math.fmod`/`modf`/`huge`/`cosh`/`sinh`/`tanh`, and the whole `coroutine` table — none of
//!    which 1.12 has. This is 1189's *"a superset is not free"* one level below `_G`: an addon
//!    that writes `string.gmatch or string.gfind` (Ace does, on the very next line) picks the
//!    branch we leave standing, so leaving both standing chooses the wrong one for it.
//! 3. **The 5.0 compat globals the reference actually has get added** — `sort`, `foreach`,
//!    `foreachi`. `reference/1.12-globals.tsv` lists all three; we had the other seventeen of that
//!    family and not these.
//!
//! ## What this module does NOT have to do: the vararg `arg` table
//!
//! Lua 5.0 gives every vararg function an implicit `arg` table with an `n` field, and 1.12's
//! FrameXML uses it directly — `QuestTimerFrame_Update(...)` walks `for i=1, arg.n`, and so do
//! `FCFDropDown_LoadChannels`, `TradeSkill_OnEvent` and the GM-ticket and CVar families. Three of
//! our own transcriptions carry a note saying they rewrote it "the 5.1-native way" with
//! `select("#", ...)`, which reads as though the dialect demanded it.
//!
//! **It does not.** mlua's vendored 5.1 is built with `LUA_COMPAT_VARARG`, so `arg` and `arg.n`
//! are already there — measured, not assumed: `local function f(...) return arg.n end; f('a','b')`
//! answers 2 in this VM. That matters for 1751: a stock file using 5.0 varargs needs nothing from
//! this module, and the rewrite was a precaution against a problem we did not have.
//!
//! ## The one known divergence, stated rather than hidden
//!
//! `table.insert`/`table.remove` stay on 5.1's `#t` border rather than consulting `getn`. **This
//! is byte-confirmed as a real divergence, not a suspicion**: the RE dispatch found that 1.12's
//! whole table library is `n`-based — `luaL_getn` is called by `insert`/`remove`/`concat`/`sort`/
//! `foreachi` *and* base `unpack`, and `insert`/`remove` **update** the stored size. So
//! `setn(t, 0)` on a non-empty table makes the next `insert` land at index 1 there and at
//! `#t + 1` here.
//!
//! It stays, and the trade was measured before it was made. Over the 218-addon corpus there are
//! 1,720 `setn` call sites; **1,471 pass 0** — always immediately after a
//! `for k in pairs(t) do t[k] = nil end` wipe, where `#t` is already 0 and the two agree — and 290
//! more pass `table.getn(source)` straight after copying that source, where they agree too. Fewer
//! than 20 sites could observe the difference, and none of them is reached at load. Replacing
//! `table.insert` with a Lua-level reimplementation to serve those would put an interpreted
//! function on the hottest path in every addon in existence. Revisit it if a *runtime* failure is
//! ever traced here — not before.
//!
//! ## Ground truth (decision 1196 — verified, no longer derived)
//!
//! This module first shipped with its member lists taken from Lua 5.0's published library
//! registrations and cross-checked against two artifacts we hold. An RE dispatch into
//! wow-5875-re then read the binary (`system/ui/scratch/lua-dialect.md`), and every list here is
//! now the array in the image:
//!
//! - **Lua 5.0**, on five independent discriminators — not just the `$Lua: Lua 5.0 …` blob at
//!   `0x811b30` but `LUA_REGISTRYINDEX = -10000` (5.1 uses −10002), a `luaT_eventname[]` pool with
//!   no `__mod`/`__len` (so **no `%` and no `#` operator**), and live `luaL_getn`/`luaL_setn`.
//! - **Five libraries are opened** by `InitLua 0x7039e0` and no more: base (a 36-entry array at
//!   `0x811e28` looped straight into `_G`), `string` (12, array `0x822d88`), `table` (8,
//!   `0x822d40`), `math` (24, `0x822c60`), and **`bit`** (8, `0x822c18`). `luaL_openlib` has four
//!   call sites image-wide, so `os`/`io`/`debug`/`coroutine` are absent by construction.
//! - **The compat globals are a Lua chunk, not C macros** — a 1310-byte `compat.lua` compiled into
//!   `.data` at `0x8722e8`, run last at init. That is why the set is a specific subset (`getn`
//!   aliased, `setn` deliberately not) rather than "whatever `LUA_COMPAT_*` gives you", and it is
//!   also where the **degree-based** bare `sin`/`cos`/`tan` come from — a convention this crate's
//!   stdlib layer had already reached independently.

use mlua::{Lua, Table, Value};

/// Members that exist in Lua 5.1 and **not** in Lua 5.0, by library.
///
/// Removed rather than left standing, per 1189: a feature-detecting addon branches on presence,
/// and the branch it should take is the one 1.12 gives it.
const REMOVED: &[(&str, &[&str])] = &[
    // 5.1 added the new pattern-matching spellings; 5.0 has only `gfind` and no `reverse`.
    ("string", &["gmatch", "match", "reverse"]),
    // 5.1 added `maxn`; 5.0's table library is concat/foreach/foreachi/getn/setn/sort/insert/remove.
    ("table", &["maxn"]),
    // 5.1 renamed `mod`→`fmod` (we keep `mod`, which 1.12 has as a bare global too) and added
    // `modf`, `huge`, and the hyperbolics.
    ("math", &["fmod", "modf", "huge", "cosh", "sinh", "tanh"]),
];

/// Lua 5.0's `luaL_getn`/`luaL_setn`, transcribed — **including the private `sizes` table**, which
/// is the part that matters and the part a plain reading of the manual leaves out.
///
/// 5.0 does **not** simply write `t.n`. `luaL_setn` writes `t.n` *only if a numeric `t.n` already
/// exists*; otherwise it stores the size in a weak-keyed table kept in the registry, so the size
/// is invisible to anything walking the table. `luaL_getn` reads `t.n`, then `sizes[t]`, then
/// falls back to counting.
///
/// **The first draft of this module wrote `t.n` unconditionally, and the corpus caught it inside
/// an hour.** `AceOO-2.0`'s `_Embed` walks its mixin's exports with `next(state.export, field)`
/// and errors on any field the target already has; a spurious `n` made that
/// `Method conflict in attempt to mixin. Field "n"` — **39 addons**, a *bigger* wall than the
/// `setn` gap it was meant to fix. Writing "faithful to Lua 5.0" in a doc comment does not make a
/// derivation verified, and an addon iterating a table is a sharper instrument than a memory of
/// `lauxlib.c`.
const LUA_5_0_TABLE_SIZE: &str = r#"
do
    -- The registry-side weak table. Weak KEYS: a table nobody else holds must still be
    -- collectable, and 5.0's own `getsizes` uses exactly this metatable.
    local sizes = setmetatable({}, { __mode = "k" })

    function table.setn(t, n)
        if type(rawget(t, "n")) == "number" then
            rawset(t, "n", n)   -- an existing numeric `n` field IS the size; keep using it
        else
            sizes[t] = n        -- otherwise the size lives out of sight
        end
    end

    function table.getn(t)
        local n = rawget(t, "n")
        if type(n) == "number" then return n end
        n = sizes[t]
        if type(n) == "number" then return n end
        return #t               -- neither: the array border, which is 5.1's whole answer
    end
end
"#;

/// Install the dialect. Runs **before** the WoW stdlib layer, so its aliases bind the 5.0-shaped
/// functions rather than the 5.1 ones they replace.
pub(super) fn install(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    // ── 1 · `setn`/`getn` get their 5.0 meaning back ──────────────────────────────────────────
    // Written in Lua because the weak-keyed side table below is the whole mechanism and a Lua
    // `setmetatable({}, {__mode="k"})` says that in one line.
    lua.load(LUA_5_0_TABLE_SIZE)
        .set_name("=[benilla lua5.0 dialect]")
        .set_mode(mlua::ChunkMode::Text)
        .exec()?;

    // ── 2 · the 5.1-only members go ───────────────────────────────────────────────────────────
    for (lib, names) in REMOVED {
        let Ok(t) = g.get::<Table>(*lib) else {
            continue;
        };
        for name in *names {
            t.set(*name, Value::Nil)?;
        }
    }
    // `coroutine` is not in the 1.12 client's `_G` at all — not a member gap, a whole library the
    // client does not open. It was on `reference_surface`'s beyond-1.12 exception list as
    // "inherited rather than chosen"; it is now neither.
    g.set("coroutine", Value::Nil)?;

    // ── 3 · the 5.0 compat globals the reference has and we lacked ────────────────────────────
    // `sort`/`foreach`/`foreachi` are in `reference/1.12-globals.tsv`; `setn` deliberately is not,
    // so no bare `setn` is installed even though `table.setn` now works. The client's compat set
    // is a specific subset, not "all of them".
    let table: Table = g.get("table")?;
    for name in ["sort", "foreach", "foreachi"] {
        if let Ok(f) = table.get::<Value>(name) {
            if !matches!(f, Value::Nil) {
                g.set(name, f)?;
            }
        }
    }
    // `getn` is re-bound here rather than left to the stdlib layer's `table.getn or …` fallback,
    // so the bare global and the member are the same function and cannot drift apart.
    g.set("getn", table.get::<Value>("getn")?)?;

    // ── 4 · what the binary said and nobody had asked (decision 1196) ─────────────────────────
    // `print` and `_VERSION` are **not in 1.12's `_G`** — the captured table says so and the RE
    // dispatch found why: the base library is a 36-entry array looped into `_G`, and neither is in
    // it (`_VERSION`'s literal is not even in the image). Both were on `reference_surface`'s
    // exception list as "inherited rather than chosen"; nothing of ours uses either.
    g.set("print", Value::Nil)?;
    g.set("_VERSION", Value::Nil)?;

    // `__pow` is a **global** in 1.12 (`function engine` in the captured table) — Lua 5.0
    // implements `^` by calling it, and 5.1 made the operator native, so the name simply
    // disappeared. Present here because an addon can see it, not because anything calls it.
    g.set(
        "__pow",
        lua.create_function(|_, (a, b): (f64, f64)| Ok(a.powf(b)))?,
    )?;

    install_bit(lua)?;

    Ok(())
}

/// The **`bit` library** — 1.12 opens one, and it is not stock Lua (decision 1196).
///
/// `InitLua 0x7039e0` opens exactly five libraries: base, `string`, `table`, `math`, and **`bit`**
/// (`0x7fadc0`, array `0x822c18`). The captured `_G` agrees — `bit` is there, attributed `engine`.
/// We had assumed the opposite: `attempt to index global 'bit'` showed up in the corpus survey and
/// was written off as an addon reaching for something 1.12 lacks. It was reaching for something
/// **we** lacked.
///
/// Eight functions, the array's own order and its own names. Semantics are 32-bit two's
/// complement, which is what every caller assumes (`band(flags, 0xFF)`); `arshift` is the
/// sign-propagating shift `rshift` is not, which is the only pair anyone gets wrong.
fn install_bit(lua: &Lua) -> mlua::Result<()> {
    let bit = lua.create_table()?;
    // Lua numbers are doubles; the client's bit ops truncate to a 32-bit int and return a signed
    // one. `as i64 as u32` is the C cast chain (`(unsigned)(int)x`) rather than a saturating one,
    // so `bnot(0)` answers -1 exactly as it does there.
    fn u32_of(v: f64) -> u32 {
        v as i64 as u32
    }
    fn out(v: u32) -> i64 {
        v as i32 as i64
    }
    bit.set(
        "bnot",
        lua.create_function(|_, a: f64| Ok(out(!u32_of(a))))?,
    )?;
    bit.set(
        "band",
        lua.create_function(|_, (a, b): (f64, f64)| Ok(out(u32_of(a) & u32_of(b))))?,
    )?;
    bit.set(
        "bor",
        lua.create_function(|_, (a, b): (f64, f64)| Ok(out(u32_of(a) | u32_of(b))))?,
    )?;
    bit.set(
        "bxor",
        lua.create_function(|_, (a, b): (f64, f64)| Ok(out(u32_of(a) ^ u32_of(b))))?,
    )?;
    // The shifts mask their count to 5 bits, like the x86 shift instructions the client compiles
    // to — `lshift(1, 32)` is `1`, not `0`, and an addon that shifts by a computed width relies on
    // it not trapping.
    bit.set(
        "lshift",
        lua.create_function(|_, (a, n): (f64, f64)| Ok(out(u32_of(a) << (u32_of(n) & 31))))?,
    )?;
    bit.set(
        "rshift",
        lua.create_function(|_, (a, n): (f64, f64)| Ok(out(u32_of(a) >> (u32_of(n) & 31))))?,
    )?;
    bit.set(
        "arshift",
        lua.create_function(|_, (a, n): (f64, f64)| {
            Ok(out(((u32_of(a) as i32) >> (u32_of(n) & 31)) as u32))
        })?,
    )?;
    // `bit.mod` — integer remainder, not `math.mod`'s float one. The library's own eighth entry.
    bit.set(
        "mod",
        lua.create_function(|_, (a, b): (f64, f64)| {
            let b = u32_of(b) as i32;
            if b == 0 {
                return Err(mlua::Error::runtime("bit.mod: division by zero"));
            }
            Ok(((u32_of(a) as i32) % b) as i64)
        })?,
    )?;
    lua.globals().set("bit", bit)
}

#[cfg(test)]
mod tests {
    use crate::script::UiScript;

    /// **The 61-addon fix, as the addon actually writes it.**
    ///
    /// This is `AceLibrary.lua`'s own opening, verbatim in shape: ask the client which client it
    /// is, take the 5.0 branch, use `table.setn`. Under mlua's stock 5.1 the last line raises
    /// `'setn' is obsolete` and the addon's first chunk dies — taking ~84 of 218 corpus addons
    /// with it, because the Ace/FuBar half of the ecosystem embeds this file.
    #[test]
    fn acelibrarys_own_dialect_probe_takes_the_5_0_branch_and_works() {
        let s = UiScript::new().unwrap();
        s.run(
            r#"
            local table_setn
            local version = GetBuildInfo()
            if string.find(version, "^2%.") then
                table_setn = function() end
            else
                table_setn = table.setn
            end
            local t = { "a", "b", "c" }
            for k in pairs(t) do t[k] = nil end
            table_setn(t, 0)
            AceProbe = table.getn(t)
            "#,
        )
        .expect("Ace's dialect probe must not raise");
        assert_eq!(s.eval::<i64>("return AceProbe").unwrap(), 0);
    }

    /// `setn`/`getn` are 5.0's real pair — a remembered size that is **not** a no-op and **not** a
    /// field the table's owner can see.
    ///
    /// Both halves have already been got wrong once each. A no-op passes the Ace probe above and
    /// fails the round-trip here; an unconditional `rawset(t, "n", …)` passes both and breaks 39
    /// addons that walk the table (see [`LUA_5_0_TABLE_SIZE`]).
    #[test]
    fn getn_and_setn_remember_a_size_without_polluting_the_table() {
        let s = UiScript::new().unwrap();
        // No remembered size: the border, exactly as 5.1 answers.
        assert_eq!(s.eval::<i64>("return table.getn({1,2,3})").unwrap(), 3);
        // The size round-trips...
        assert_eq!(
            s.eval::<i64>("local t = {1,2,3} table.setn(t, 7) return table.getn(t)")
                .unwrap(),
            7
        );
        // ...and is INVISIBLE to anything walking the table. This is the assertion that would have
        // caught the first draft: AceOO-2.0's `_Embed` walks a mixin's fields with `next` and
        // errors on any the target already has.
        assert_eq!(
            s.eval::<i64>(
                "local t = {1,2,3} table.setn(t, 0) \
                 local c = 0 for k in pairs(t) do c = c + 1 end return c"
            )
            .unwrap(),
            3,
            "a remembered size must not appear as a key — 39 corpus addons died on a stray 'n'"
        );
        // A table that ALREADY carries a numeric `n` keeps using it, which is 5.0's own branch and
        // why `arg.n` behaves the way 5.0-era code expects.
        assert_eq!(
            s.eval::<i64>("local t = {1,2,3, n=3} table.setn(t, 7) return t.n")
                .unwrap(),
            7
        );
        // ...and the bare global is the same function, not a second implementation.
        assert!(
            s.eval::<bool>("return getn == table.getn").unwrap(),
            "a divergent `getn` global is how the two answers start disagreeing"
        );
    }

    /// The 5.1-only members are gone, so an addon's feature detection picks 1.12's branch.
    ///
    /// `string.gmatch or string.gfind` is the line **directly below** Ace's `setn` probe: with
    /// both present it takes `gmatch`, a function 1.12 does not have. That is 1189's superset
    /// argument one level below `_G`, and this is the test that keeps it true.
    #[test]
    fn the_5_1_only_members_are_not_offered() {
        let s = UiScript::new().unwrap();
        for expr in [
            "string.gmatch",
            "string.match",
            "string.reverse",
            "table.maxn",
            "math.fmod",
            "math.modf",
            "math.huge",
            "math.cosh",
            "coroutine",
        ] {
            assert!(
                s.eval::<bool>(&format!("return {expr} == nil")).unwrap(),
                "{expr} is a Lua 5.1 addition — 1.12 does not have it"
            );
        }
        // ...and the 5.0 spellings that replace them are present.
        for expr in ["string.gfind", "math.mod", "table.setn", "table.foreach"] {
            assert!(
                s.eval::<bool>(&format!("return {expr} ~= nil")).unwrap(),
                "{expr} is Lua 5.0's own spelling and 1.12 has it"
            );
        }
    }

    /// `mod` still works after `math.fmod` is removed — the stdlib layer aliased the 5.1 name.
    #[test]
    fn the_bare_math_family_survives_the_removals() {
        let s = UiScript::new().unwrap();
        assert_eq!(s.eval::<f64>("return mod(7, 3)").unwrap(), 1.0);
        assert_eq!(s.eval::<f64>("return floor(2.7)").unwrap(), 2.0);
    }

    /// The three compat globals the reference has and we did not.
    #[test]
    fn sort_foreach_and_foreachi_are_bare_globals_like_the_reference() {
        let s = UiScript::new().unwrap();
        assert_eq!(
            s.eval::<String>("local t = {'c','a','b'} sort(t) return table.concat(t)")
                .unwrap(),
            "abc"
        );
        assert_eq!(
            s.eval::<i64>("local n = 0 foreach({1,2,3}, function() n = n + 1 end) return n")
                .unwrap(),
            3
        );
        assert_eq!(
            s.eval::<i64>("local n = 0 foreachi({1,2,3}, function() n = n + 1 end) return n")
                .unwrap(),
            3
        );
    }

    /// The `bit` library exists in 1.12 and we did not have it (decision 1196).
    ///
    /// `InitLua 0x7039e0` opens exactly five libraries and `bit` is the fifth; the captured `_G`
    /// lists it as `engine`. The corpus's `attempt to index global 'bit'` had been written off as
    /// an addon reaching past 1.12 — it was reaching past *us*.
    #[test]
    fn the_bit_library_is_present_with_the_references_eight_functions() {
        let s = UiScript::new().unwrap();
        for name in [
            "bnot", "band", "bor", "bxor", "lshift", "rshift", "arshift", "mod",
        ] {
            assert!(
                s.eval::<bool>(&format!("return type(bit.{name}) == 'function'"))
                    .unwrap(),
                "bit.{name} is one of the array's eight entries"
            );
        }
        // Two's complement, signed out — `bnot(0)` is -1, not 4294967295.
        assert_eq!(s.eval::<i64>("return bit.bnot(0)").unwrap(), -1);
        assert_eq!(
            s.eval::<i64>("return bit.band(0x1234, 0xFF)").unwrap(),
            0x34
        );
        assert_eq!(s.eval::<i64>("return bit.bor(0xF0, 0x0F)").unwrap(), 0xFF);
        assert_eq!(s.eval::<i64>("return bit.bxor(0xFF, 0x0F)").unwrap(), 0xF0);
        assert_eq!(s.eval::<i64>("return bit.lshift(1, 4)").unwrap(), 16);
        // The pair everyone gets wrong: rshift is logical, arshift propagates the sign.
        assert_eq!(s.eval::<i64>("return bit.rshift(-1, 28)").unwrap(), 15);
        assert_eq!(s.eval::<i64>("return bit.arshift(-1, 28)").unwrap(), -1);
        // The shift count masks to 5 bits, like the x86 instruction it compiles to.
        assert_eq!(s.eval::<i64>("return bit.lshift(1, 32)").unwrap(), 1);
        assert_eq!(s.eval::<i64>("return bit.mod(17, 5)").unwrap(), 2);
    }

    /// `print` and `_VERSION` are **not** in 1.12's `_G`, and `__pow` is.
    ///
    /// All three are things a `_G`-shaped instrument could always have caught and nobody had
    /// looked: the first two sat on the beyond-1.12 exception list as "inherited rather than
    /// chosen", and the third was simply missing.
    #[test]
    fn the_base_library_matches_the_captured_globals() {
        let s = UiScript::new().unwrap();
        assert!(s.eval::<bool>("return print == nil").unwrap());
        assert!(s.eval::<bool>("return _VERSION == nil").unwrap());
        // 5.0 implements `^` by calling `__pow`; 5.1 made it native and the global vanished.
        assert_eq!(s.eval::<f64>("return __pow(2, 10)").unwrap(), 1024.0);
    }

    /// **The corpus's other dialect probe, and we answer it wrong** (decision 1208).
    ///
    /// Ace2 asks "am I on Lua 5.1?" by compiling a 5.1-only construct — the vararg *expression*,
    /// which 5.0 has no grammar for (5.0 collects varargs into `arg`; the shipped 1.12 FrameXML
    /// reads `arg`/`arg.n` 64 times across 177 files and never once writes `...` as a value):
    ///
    /// ```lua
    /// if loadstring("return function(...) return ... end") and AceLibrary:HasInstance(MAJOR)
    ///     then return end -- lua51 check
    /// ```
    ///
    /// On the real 1.12 client that `loadstring` returns nil, the guard never fires, and normal
    /// revision comparison decides which copy of a library wins. On ours it compiles, so the
    /// **newer** copy silently declines to register and whichever copy loaded first is pinned
    /// forever. **92 library files across 24 corpus addon folders carry it.**
    ///
    /// Asserted rather than fixed: unlike `table.setn` or `bit`, this is not reachable from inside
    /// Lua — it is the parser. 1208 records the measurement; 1202 is where the fix lives.
    #[test]
    fn the_vararg_expression_compiles_and_that_is_the_1208_divergence() {
        let s = UiScript::new().unwrap();
        assert!(
            s.eval::<bool>(r#"return loadstring("return function(...) return ... end") ~= nil"#)
                .unwrap(),
            "if this ever fails, the VM became 5.0-shaped and 1208 closed with 1202"
        );
        // The 5.0 half of the same question, which we answer correctly (1194): `arg` still exists.
        assert_eq!(
            s.eval::<i64>("local f = function(...) return arg.n end return f(1, 2, 3)")
                .unwrap(),
            3,
            "`arg` is 5.1's compat-vararg table, which is the form all of 1.12 FrameXML uses"
        );
    }

    /// **The six `debug*` stubs are the REFERENCE's no-ops, not ours** — and that distinction is
    /// the whole reason they are allowed to exist under real 1.12 names.
    ///
    /// wow-re carved all eight (2026-08-11): `debuginfo`, `debugload`, `debugprint`, `debugdump`,
    /// `debugbreak` and `debugtimestamp` are byte-identical `xor eax,eax; ret` — three bytes, no
    /// call, no memory write, **zero Lua return values**. Only `debugprofilestart`/`stop` are real.
    /// A no-op we invented would be the "capability absent without a failure" class 1203 named;
    /// a no-op the client itself ships is a transcription.
    #[test]
    fn the_debug_family_is_six_stubs_and_two_real_ones() {
        let s = UiScript::new().unwrap();
        // Zero values, not nil — `select('#')` is the only check that can tell them apart.
        for name in [
            "debuginfo",
            "debugload",
            "debugprint",
            "debugdump",
            "debugbreak",
            "debugtimestamp",
        ] {
            assert_eq!(
                s.eval::<i64>(&format!("return select('#', {name}())"))
                    .unwrap(),
                0,
                "{name} returns nothing at all"
            );
        }
        // ...and the two that are real answer a number of milliseconds.
        let ms: f64 = s
            .eval("debugprofilestart() return debugprofilestop()")
            .unwrap();
        assert!(ms >= 0.0, "elapsed milliseconds, not nil: {ms}");
    }

    /// The known divergence, pinned so it is a decision rather than a surprise.
    ///
    /// In Lua 5.0 `table.insert` consults `getn`, so this would land at index 1. Here it lands at
    /// `#t + 1`. Recorded in the module doc with the corpus measurement that justifies it; if this
    /// test ever needs to change, the trade has been revisited on purpose.
    #[test]
    fn table_insert_still_uses_the_border_not_setn() {
        let s = UiScript::new().unwrap();
        assert_eq!(
            s.eval::<i64>("local t = {1,2,3} table.setn(t, 0) table.insert(t, 9) return #t")
                .unwrap(),
            4,
            "5.1's insert appends at the border; 5.0's would have written index 1"
        );
    }
}
