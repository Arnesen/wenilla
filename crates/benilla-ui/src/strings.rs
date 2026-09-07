//! **One faithful substitution for reference string templates** — the client's `SStrPrintf` face.
//!
//! Every user-visible sentence benilla shows comes from `GlobalStrings.lua` or `GlueStrings.lua`
//! (decision 2045), and most of those carry `%s`/`%d` holes the caller fills. Doing that filling
//! correctly is not `format!`'s job and never was: the template is *data read at runtime from the
//! player's install*, so the holes have to be walked, not compiled.
//!
//! **Why this module exists.** Before it, the substitution had been reinvented at least eight
//! times across the workspace, with semantics that did not agree:
//!
//! - `ui_instance::fill_template` — ordered, `%%`-aware, starvation-safe, and tested. The best of
//!   them, and the one this is derived from.
//! - `ui_duel::winner_line` — a hand-rolled `%1$s`/`%2$s` positional pass, because the duel's
//!   retreat wording swaps its two names and no ordered filler can express that.
//! - `ui_action::errors::ui_error_text` — `str::replace`, which fills **every** `%s` with the
//!   *same* argument. Latent rather than live (no message it currently carries has two), but it
//!   was a real trap waiting for the first two-argument template to reach that queue.
//! - `ui_guild::fill`, `ui_petition::fill`, and one-off `replacen` calls in `ui_binder`,
//!   `ui_trade`, `ui_items::feed` and `equip_error`.
//!
//! Eight copies of one primitive is how the `ui_error_text` bug survived: there was no single
//! place where "how do we fill a reference template" could be got right once.
//!
//! **The semantics, and why each is what it is.**
//!
//! - `%s` and `%d` consume the next argument, **left to right**. This is `SStrPrintf`'s own order.
//! - `%N$s` / `%N$d` take argument `N` (1-based) and do **not** move the sequential cursor. 1.12
//!   uses these exactly where a translation needs to reorder the holes —
//!   `DUEL_WINNER_RETREAT = "%2$s has fled from %1$s in a duel"` is the canonical case, and it is
//!   also why hardcoding an English sentence is a localization bug and not merely untidy.
//! - `%%` collapses to one `%`.
//! - **A specifier whose argument is missing is copied through literally.** A template we
//!   mis-modelled should look wrong, not look plausible — `ui_instance` established this and it is
//!   the right call: a visibly broken line gets reported, a quietly wrong one does not.
//! - Any other specifier is copied through untouched.

use std::fmt::Write as _;

/// One argument to [`fill`]. `%s` renders either variant as text; `%d` renders either as an
/// integer, so a caller that groups its arguments differently from the template still fills in the
/// order the template asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg<'a> {
    /// A string argument — a player name, an item link, a zone.
    S(&'a str),
    /// A numeric argument.
    D(i64),
}

impl<'a> From<&'a str> for Arg<'a> {
    fn from(s: &'a str) -> Self {
        Arg::S(s)
    }
}

impl<'a> From<&'a String> for Arg<'a> {
    fn from(s: &'a String) -> Self {
        Arg::S(s.as_str())
    }
}

macro_rules! arg_from_int {
    ($($t:ty),*) => {$(
        impl From<$t> for Arg<'_> {
            fn from(n: $t) -> Self {
                Arg::D(i64::from(n))
            }
        }
    )*};
}
arg_from_int!(u8, u16, u32, i8, i16, i32);

impl Arg<'_> {
    fn as_s(&self, out: &mut String) {
        match self {
            Arg::S(s) => out.push_str(s),
            Arg::D(n) => {
                let _ = write!(out, "{n}");
            }
        }
    }

    fn as_d(&self, out: &mut String) {
        match self {
            Arg::D(n) => {
                let _ = write!(out, "{n}");
            }
            // A `%d` handed a string is a caller bug, not a display decision; showing the string
            // is strictly more useful than showing nothing and matches what varargs would do with
            // a pointer-sized value far better than a zero would.
            Arg::S(s) => out.push_str(s),
        }
    }
}

/// Fill a reference template. See the module doc for the rules.
pub fn fill(template: &str, args: &[Arg<'_>]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let chars: Vec<char> = template.chars().collect();
    let mut i = 0;
    let mut next = 0usize; // the sequential cursor
    while i < chars.len() {
        if chars[i] != '%' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // `%%`
        if chars.get(i + 1) == Some(&'%') {
            out.push('%');
            i += 2;
            continue;
        }
        // `%N$s` / `%N$d` — positional, does not move the cursor
        let mut j = i + 1;
        let mut digits = String::new();
        while j < chars.len() && chars[j].is_ascii_digit() {
            digits.push(chars[j]);
            j += 1;
        }
        if !digits.is_empty() && chars.get(j) == Some(&'$') {
            let spec = chars.get(j + 1).copied();
            let idx = digits.parse::<usize>().unwrap_or(0);
            if matches!(spec, Some('s') | Some('d')) {
                match idx.checked_sub(1).and_then(|k| args.get(k)) {
                    Some(a) => {
                        if spec == Some('s') {
                            a.as_s(&mut out);
                        } else {
                            a.as_d(&mut out);
                        }
                        i = j + 2;
                        continue;
                    }
                    // starved: copy the whole specifier through
                    None => {
                        out.extend(&chars[i..j + 2]);
                        i = j + 2;
                        continue;
                    }
                }
            }
        }
        // plain `%s` / `%d`
        match chars.get(i + 1) {
            Some('s') | Some('d') => {
                let spec = chars[i + 1];
                match args.get(next) {
                    Some(a) => {
                        if spec == 's' {
                            a.as_s(&mut out);
                        } else {
                            a.as_d(&mut out);
                        }
                        next += 1;
                        i += 2;
                    }
                    None => {
                        // starved — copy it through so a mis-modelled template looks wrong
                        out.push('%');
                        i += 1;
                    }
                }
            }
            _ => {
                out.push('%');
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ui_instance::fill_template`'s own contract, which this subsumes — ordered, starvation-safe,
    /// `%%`-aware. These are its test's cases verbatim, so the migration cannot change behaviour.
    #[test]
    fn ordered_fills_stop_at_the_arguments_they_have() {
        assert_eq!(
            fill("%s: %d/%d", &[Arg::S("MC"), Arg::D(2), Arg::D(5)]),
            "MC: 2/5"
        );
        assert_eq!(fill("%s: %d/%d", &[Arg::S("MC"), Arg::D(2)]), "MC: 2/%d");
        assert_eq!(fill("100%% sure", &[]), "100% sure");
        assert_eq!(fill("no fills", &[Arg::D(7)]), "no fills");
    }

    /// **The case an ordered filler cannot express**, and the reason positional specifiers exist:
    /// 1.12's duel pair reorders the same two names, so filling them left-to-right names the wrong
    /// winner. Both templates are quoted from GlobalStrings (958/959).
    #[test]
    fn positional_specifiers_reorder_rather_than_consume() {
        let (a, b) = (Arg::S("Alice"), Arg::S("Bob"));
        assert_eq!(
            fill("%1$s has defeated %2$s in a duel", &[a, b]),
            "Alice has defeated Bob in a duel"
        );
        assert_eq!(
            fill("%2$s has fled from %1$s in a duel", &[a, b]),
            "Bob has fled from Alice in a duel"
        );
        // A positional may repeat an argument, which no cursor-based fill can do.
        assert_eq!(fill("%1$s vs %1$s", &[a]), "Alice vs Alice");
    }

    /// The bug that eight copies of this hid: `str::replace` fills every `%s` with the *same*
    /// argument. Two holes must take two arguments.
    #[test]
    fn each_hole_takes_its_own_argument() {
        assert_eq!(
            fill(
                "%s has promoted %s to %s.",
                &[Arg::S("A"), Arg::S("B"), Arg::S("Knight")]
            ),
            "A has promoted B to Knight."
        );
    }

    /// A starved positional is copied through too, and an unknown specifier is left alone.
    #[test]
    fn starved_and_unknown_specifiers_survive_visibly() {
        assert_eq!(fill("%1$s and %2$s", &[Arg::S("only")]), "only and %2$s");
        assert_eq!(fill("50% off", &[]), "50% off");
        assert_eq!(fill("%q", &[Arg::S("x")]), "%q");
    }

    /// Numbers reach `%s` as digits and strings reach `%d` as themselves — a caller whose argument
    /// grouping differs from the template's still fills in the template's order.
    #[test]
    fn arguments_render_for_whichever_hole_they_meet() {
        assert_eq!(fill("%s/%d", &[Arg::D(3), Arg::D(5)]), "3/5");
        assert_eq!(fill("%d", &[Arg::S("many")]), "many");
    }
}
