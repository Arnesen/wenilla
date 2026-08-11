//! The **registered-binding argument/error ABI** — the marshalling contract every one of build
//! 5875's ~700 registered Lua C-bindings opens with, in one place because getting it wrong is
//! wrong 700 times (wow-re `ui/scratch/binding-arg-error-contract.md`, §5-cross-checked with
//! orchestrator byte-arbitration).
//!
//! ## The three helpers the reference uses, and what they accept
//!
//! | addr | contract |
//! |---|---|
//! | `0x6f34d0` | `is-number(L, idx)` — tag 3, **or** a string coerced through `0x6f7c20` |
//! | `0x6f3510` | `is-number-**or**-string(L, idx)` — strictly wider: tag 3 or tag 4 |
//! | `0x6f3620` → `0x40a2b0` | `tonumber` then `double → int32`, **truncating toward zero** |
//!
//! `0x40a2b0` sets the x87 rounding control to chop (`or ah,0xc`) before `fistp`, so it is a C
//! cast and **not** `floor`: `−2.9 → −2`. [`number_arg`] reproduces that (Rust's `as` on a float
//! truncates toward zero as well), and hands back the low dword the bindings actually consume.
//!
//! ## The headline: the `Usage:` arm RAISES
//!
//! `0x6f4940` is `luaL_error`, and it **does not return**: `luaL_where` + `lua_pushvfstring` +
//! `lua_concat` + `lua_error`, whose own chain (`0x6f4440` → `0x6fc780` → `0x6f5d80`) ends in a
//! CRT `longjmp` or `exit(1)` on both legs. The `xor eax,eax; ret` that MSVC emits after every
//! such call inside a binding is **unreachable boilerplate**, not a "returns zero values" arm — a
//! reading five committed wow-re notes had inverted, corrected at the bytes on 2026-08-11.
//!
//! So a binding's bad-argument path **abandons the caller's statement** and unwinds to the
//! enclosing protected call. It returns neither `nil` nor zero values, and a client that answers
//! `nil` there keeps executing a statement the real client never finishes.
//!
//! Stated as the negative, because it is as load-bearing as the positive: the **zero Lua values**
//! outcome is real only where a binding reaches `xor eax,eax; ret` *without* passing through
//! `0x6f4940` — `GetDefaultLanguage`'s four object/bounds failure edges are this repo's one
//! example ([`super::session`]). Those arms genuinely return nothing.
//!
//! ## The three shapes an "empty answer" can take
//!
//! Settled per binding, never generalised:
//!
//! 1. **push nil**, one Lua value (`UnitAffectingCombat` on a false/unresolved unit,
//!    `GetActionText` on a non-macro slot, `UnitInRaid` on a miss).
//! 2. **zero Lua values** — distinct from `nil` for `select('#', …)` and for a multiple
//!    assignment, identical to it for a single-value caller (`GetDefaultLanguage`'s failure edges).
//! 3. **raise** — the statement is abandoned.

use mlua::{Lua, Value};

/// `is-number` (`0x6f34d0`) → `tonumber` (`0x6f3620`) → `double → int32` truncating toward zero
/// (`0x40a2b0`), with the binding's own `Usage:` string on the failure edge.
///
/// A **missing** argument fails the same test a wrong-typed one does — `0x6f3410` returns NULL
/// past `L->top`, which `0x6f34d0` reports as "not a number" — so `f()` and `f({})` take the same
/// raise. A numeric *string* passes: Lua 5.1's `lua_isnumber` coerces, and so does
/// [`Lua::coerce_number`].
///
/// The `i32` is deliberate: `0x40a2b0` stores a qword but every binding here consumes only `eax`,
/// the low dword. Rust's `i64 as i32` truncates the same way. (Past ±2^63 the two diverge — the
/// x87 stores the "integer indefinite" pattern where Rust saturates — which no caller can reach
/// and no consumer could tell apart, since both answers are garbage.)
pub(crate) fn number_arg(lua: &Lua, v: Value, usage: &'static str) -> mlua::Result<i32> {
    match lua.coerce_number(v)? {
        Some(n) => Ok(n as i64 as i32),
        None => Err(mlua::Error::RuntimeError(usage.into())),
    }
}

/// `is-number-or-string` (`0x6f3510`) → `arg-as-C-string` (`0x6f3690`), with the binding's own
/// `Usage:` string on the failure edge.
///
/// Strictly wider than [`number_arg`]'s guard: a *number* is accepted and stringified, which is
/// why `UnitAffectingCombat(5)` does not raise in the reference — it resolves the token `"5"`,
/// finds nothing, and answers `nil`. Everything else (nil, boolean, table, function) raises.
pub(crate) fn string_arg(lua: &Lua, v: Value, usage: &'static str) -> mlua::Result<String> {
    match lua.coerce_string(v)? {
        Some(s) => Ok(s.to_str()?.to_owned()),
        None => Err(mlua::Error::RuntimeError(usage.into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lua() -> Lua {
        Lua::new()
    }

    #[test]
    fn number_arg_truncates_toward_zero_not_floor() {
        let lua = lua();
        // `0x40a2b0` sets RC = chop before `fistp` — a C cast, not `floor`.
        assert_eq!(number_arg(&lua, Value::Number(2.9), "u").unwrap(), 2);
        assert_eq!(number_arg(&lua, Value::Number(-2.9), "u").unwrap(), -2);
        assert_eq!(number_arg(&lua, Value::Number(-0.5), "u").unwrap(), 0);
    }

    #[test]
    fn number_arg_accepts_a_numeric_string_and_raises_on_everything_else() {
        let lua = lua();
        let s = lua.create_string("7").unwrap();
        assert_eq!(number_arg(&lua, Value::String(s), "u").unwrap(), 7);
        // Missing and wrong-typed take the SAME arm: `0x6f3410` hands `0x6f34d0` a NULL for an
        // absent argument, which reports "not a number" exactly as a table does.
        for v in [Value::Nil, Value::Boolean(true)] {
            let err = number_arg(&lua, v, "Usage: Thing(n)").unwrap_err();
            assert!(format!("{err}").contains("Usage: Thing(n)"));
        }
    }

    #[test]
    fn string_arg_accepts_a_number_and_raises_on_nil() {
        let lua = lua();
        assert_eq!(string_arg(&lua, Value::Integer(5), "u").unwrap(), "5");
        let err = string_arg(&lua, Value::Nil, "Usage: Thing(\"s\")").unwrap_err();
        assert!(format!("{err}").contains("Usage: Thing(\"s\")"));
    }
}
