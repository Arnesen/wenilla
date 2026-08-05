//! **Running** a macro, and the one derivation the action bar needs from a macro body: its
//! **bound spell** (decision 0983).
//!
//! ## Running: each line is fired as `EXECUTE_CHAT_LINE` (VERIFIED, 0996)
//!
//! 0983 shipped this as "push the line onto the chat-input queue", with the engine's route to Lua
//! recorded as an open question. The wow-re §5 settled it, and the answer is better than the guess:
//! the runner `0x4f14e0` fires **`FrameScript_SignalEvent(EXECUTE_CHAT_LINE, "%s", line)`** per
//! non-empty line and does nothing else. It names no Lua function and holds no command table — which
//! is precisely why a scan of `WoW.exe` finds no FrameXML function name but `GetText`, no
//! `SLASH_%s%d` walk, and no chat-frame name. (`0x188` → the name is pinned inside the binary: the
//! registry slot `0xbe17b8` has exactly one writer, `0x51b4ff`, storing `0x852470`.)
//!
//! So [`super::run_macro`] fires the event and `ChatFrame1` handles it — the reference's own
//! division, with benilla's Rust drain (`crate::ui_chat::input`, the whole slash grammar in one
//! place) standing in for `ChatEdit_SendText`. Two things follow that the old shape did not give:
//! **an addon registering `EXECUTE_CHAT_LINE` sees macro lines**, and the dependency is the
//! reference's real one — ChatFrame1's registration is the *only* one in the default UI, so a macro
//! line runs through that frame's box whichever chat frame has focus.
//!
//! The chat type of a plain line is decided nowhere in the engine (§6): the runner has no
//! `SendChatMessage` call and no type constant. It is `ChatEdit_SendText`'s
//! `editBox.chatType`, which the reference's trailing `ChatEdit_OnEscapePressed` resets to
//! `stickyType` — benilla's drain sends as the box's current type, the same observable.
//!
//! ## The bound spell (`[rec+0x564]`, VERIFIED)
//!
//! A macro action-bar slot shows **the macro's own icon** but reports the **cooldown, usability,
//! range and checked state of the spell it casts** — byte-verified: `0x4e5a50`'s macro arm
//! resolves the macro record through `0x4f0f40` and returns `[rec+0x564]` as the slot's spell id
//! (wow-re `action-spell-icon-apis.md` §2), and every `Is*Action`/`GetActionCooldown` binding reads
//! through that resolver. [`bound_spell`] is how that field is filled: the body's first `/cast`
//! line, or a `CastSpellByName("…")` call in a `/script` line.
//!
//! 0983 called this derivation INFERRED (it was reasoned from the two string literals in the
//! reference's `UIMacros.cpp` block). The wow-re §5 **read `0x4efe00` at the bytes and promoted it
//! to VERIFIED**, with three refinements now implemented here:
//!
//! - **Arm A** builds `"SLASH_CAST%d"` for n = 1, 2, … and reads each as a **Lua global's value**
//!   (`FrameScript_GetText 0x703bf0`), stopping at the first nil/empty — never a hardcoded
//!   `"/cast"`, which is 0881's law arrived at independently. The compare is case-insensitive and
//!   the separator must be a literal `' '`.
//! - **Arm B** is a case-SENSITIVE substring search for `CastSpellByName(`, tried on the same line
//!   after the alias loop ends. So `/CAST x` matches; `castspellbyname("x")` does not.
//! - The first line matching **either** arm wins and the walk stops there.
//!
//! **The shared resolver is faithful — do not split it.** `CastSpellByName`'s own binding
//! (`0x4b4ab0`) calls the *identical* name resolver this derive does (`0x4b3950` → `0x4b3a10`); the
//! two differ only downstream, in what they do with the `(index, bookFlag)` pair. And a bare name
//! binds the **highest** rank: `0x4b3a10` walks its list strictly descending (`edi` from count,
//! post-decremented) over a list `0x4b2fd0` qsorts rank-ascending immediately before every re-derive.
//! So [`bound_spell`] and the press going through one [`resolve_spell_by_name`] is the reference's
//! shape, not a convenience.
//!
//! One thing the reference distinguishes and benilla does not: a `/cast` line whose name did not
//! resolve stores **-1**, a `CastSpellByName(` line whose name did not resolve stores **0**, and the
//! incremental re-derive retries only negatives — so a failed `CastSpellByName` is never retried
//! while a failed `/cast` is. benilla recomputes the whole table off two change signals instead
//! (`rebind_macro_spells`), which retries both; strictly more forgiving, and the reason the
//! asymmetry has no observable to reproduce here.
//!
//! **`[rec+0x568]` is NOT what 0983 said it was.** It is the SPELLBOOK the bound spell resolved
//! from — 0 = the player's list, 1 = the PET's (Lua's `BOOKTYPE_SPELL`/`BOOKTYPE_PET`) — not "the
//! cast is not self-targeted". benilla does not model it: `bound_spell` resolves against the player
//! book only, so a macro that casts a pet spell binds nothing. See decision 0996 for what that
//! costs (pet autocast state and pet-aura checked state on a macro slot).

use benilla_ui::script::{resolve_spell_by_name, SpellBookState};

use crate::ui_chat::commands::{Command, SlashCommands, SlashIndex};

/// The literal the reference matches a `/script`-style body line against — `0x84cab0`, searched
/// case-SENSITIVELY anywhere in the line (`0x4efed5` → `0x64b4f0` → a `rep cmpsb` `strncmp`).
const CAST_BY_NAME_CALL: &str = "CastSpellByName(";

/// A macro body's runnable lines. The reference's tokenizer (`0x64ae50`) takes `"\r\n"` as a
/// **delimiter SET** — either character splits — and skips empty tokens, which is why an interior
/// blank line never even produces one (wow-re `macro-execution-law.md` §3/§6). So do we; a
/// `macros-cache.txt` hand-copied off a Windows install is a real input, and so is a lone `\r`.
///
/// One friendly divergence: we **trim** each line. The reference hands the line over verbatim, so
/// its `" /say hi"` is not a slash command and its `"/cast  Fireball"` (two spaces) binds a name
/// with a leading space that resolves to nothing. Both are ways a player's macro silently does
/// nothing, and neither is a behaviour worth reproducing. (The reference does fire the event for a
/// whitespace-only line and drop it a layer up in `ChatEdit_SendText`'s length gate; dropping it
/// here is the same observable.)
pub(crate) fn macro_lines(body: &str) -> impl Iterator<Item = &str> {
    body.split(['\r', '\n'])
        .map(str::trim)
        .filter(|l| !l.is_empty())
}

/// The spell name a macro body casts, if any — [`bound_spell`]'s parse half, split out because it
/// carries the whole of the reference's `0x4efe00` and deserves its own test.
///
/// Walks the body in order and returns the FIRST match of either form:
/// - a line whose command resolves to [`SlashIndex::Cast`] through the boot-built alias table
///   (never a hardcoded `"/cast"` — decision 0881's law, and the reference's own: it reads
///   `SLASH_CAST1`, `SLASH_CAST2`, … out of the Lua globals), argument taken whole; or
/// - a line containing `CastSpellByName(` with a quoted first argument.
pub(crate) fn cast_name(table: &SlashCommands, body: &str) -> Option<String> {
    for line in macro_lines(body) {
        // Arm A. The separator after the alias must be a literal `' '` — the reference compares
        // `[ebp+ebx-0x108]` against `0x20` exactly (`0x4efe96`), so a TAB does not match and a
        // bare `/cast` at end-of-line does not either. The alias itself is compared with an
        // ASCII-folding `_strnicmp` (`0x414310`), which `SlashCommands::lookup` already is.
        if let Some(rest) = line.strip_prefix('/') {
            let (cmd, args) = rest.split_once(' ').unwrap_or((rest, ""));
            if table.lookup(cmd) == Some(Command::Slash(SlashIndex::Cast)) {
                let args = args.trim();
                if !args.is_empty() {
                    return Some(args.to_string());
                }
                // The ref falls through to arm B on this same line, then to the next line.
                continue;
            }
        }
        // Arm B, second and on the same line (`0x4efed5`).
        if let Some(name) = quoted_call_argument(line) {
            return Some(name);
        }
    }
    None
}

/// `… CastSpellByName("Fireball" …) …` → `Fireball`. Only a double-quoted first argument is read:
/// a computed one (`CastSpellByName(spell)`) has no name to bind at parse time, and the reference
/// — matching a bare literal with no format specifier — cannot read one either.
fn quoted_call_argument(line: &str) -> Option<String> {
    let after = line.find(CAST_BY_NAME_CALL)? + CAST_BY_NAME_CALL.len();
    let rest = line.get(after..)?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let close = rest.find('"')?;
    let name = &rest[..close];
    (!name.is_empty()).then(|| name.to_string())
}

/// The macro's bound spell id — [`cast_name`] resolved against the player's book by the same law
/// `CastSpellByName` itself uses ([`resolve_spell_by_name`]), so the icon's cooldown swirl and the
/// press always agree about which rank is meant. `None` for a macro that casts nothing, or names a
/// spell this character does not know: the slot then reports no cooldown and no range, which is
/// what the reference's `0x4e5a50` produces for a `[rec+0x564]` of 0.
pub(crate) fn bound_spell(table: &SlashCommands, body: &str, book: &SpellBookState) -> Option<u32> {
    let name = cast_name(table, body)?;
    resolve_spell_by_name(book, &name).map(|s| s.spell_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_chat::commands::SlashCommands;

    /// A table with just the aliases these tests need, built the way boot builds it (through the
    /// global-string reader), so the parse is exercised against a real lookup and never a literal.
    fn table() -> SlashCommands {
        SlashCommands::build(
            |name| match name {
                "SLASH_CAST1" => Some("/cast".into()),
                "SLASH_CAST2" => Some("/spell".into()),
                "SLASH_SCRIPT1" => Some("/script".into()),
                "SLASH_TARGET1" => Some("/target".into()),
                _ => None,
            },
            |_| None,
        )
    }

    #[test]
    fn macro_lines_trims_blanks_and_windows_line_endings() {
        let body = "/cast Fireball\r\n\r\n  /say pew  \n";
        let lines: Vec<&str> = macro_lines(body).collect();
        assert_eq!(lines, ["/cast Fireball", "/say pew"]);
    }

    /// The `/cast` form, through the ALIAS table — `/spell` is `SLASH_CAST2` in the shipped
    /// strings, so it must bind exactly as `/cast` does.
    #[test]
    fn cast_name_reads_the_first_cast_line_through_the_alias_table() {
        let t = table();
        assert_eq!(
            cast_name(&t, "/target Bob\n/cast Fireball\n/say pew"),
            Some("Fireball".into())
        );
        assert_eq!(cast_name(&t, "/spell Frostbolt"), Some("Frostbolt".into()));
        // The whole argument, subtext included — `resolve_spell_by_name` owns that grammar.
        assert_eq!(
            cast_name(&t, "/cast Fireball(Rank 1)"),
            Some("Fireball(Rank 1)".into())
        );
        // First match wins.
        assert_eq!(
            cast_name(&t, "/cast Fireball\n/cast Frostbolt"),
            Some("Fireball".into())
        );
        // A bare `/cast` binds nothing and does not stop the walk.
        assert_eq!(
            cast_name(&t, "/cast\n/cast Frostbolt"),
            Some("Frostbolt".into())
        );
        assert_eq!(cast_name(&t, "/say hello\n/target Bob"), None);
    }

    /// The separator after the alias is a **literal space** — the reference compares that one byte
    /// against `0x20` (`0x4efe96`), so a tab is not a match and the line is simply not a cast line
    /// (wow-re `macro-execution-law.md` §7). The alias itself folds case, which is the same
    /// `_strnicmp` the ref uses.
    #[test]
    fn the_alias_separator_is_a_literal_space_and_the_alias_folds_case() {
        let t = table();
        assert_eq!(cast_name(&t, "/CAST Fireball"), Some("Fireball".into()));
        assert_eq!(cast_name(&t, "/Cast Fireball"), Some("Fireball".into()));
        // A tab does not separate — the ref reads it as "not a /cast line" and walks on.
        assert_eq!(
            cast_name(&t, "/cast\tFireball\n/cast Frostbolt"),
            Some("Frostbolt".into())
        );
        // Arm B is case-SENSITIVE (a `rep cmpsb` strncmp), so the lowercased spelling misses.
        assert_eq!(
            cast_name(&t, r#"/script castspellbyname("Fireball")"#),
            None
        );
    }

    /// The `CastSpellByName(` form — the other half of the reference's own two-literal parse.
    #[test]
    fn cast_name_reads_a_quoted_cast_spell_by_name_call() {
        let t = table();
        assert_eq!(
            cast_name(&t, r#"/script CastSpellByName("Shadow Bolt")"#),
            Some("Shadow Bolt".into())
        );
        // Spacing and a second argument don't matter; the first quoted argument is the name.
        assert_eq!(
            cast_name(&t, r#"/script CastSpellByName( "Healing Touch", 1 )"#),
            Some("Healing Touch".into())
        );
        // A computed argument has no name to bind — neither here nor in the reference.
        assert_eq!(cast_name(&t, "/script CastSpellByName(spell)"), None);
        assert_eq!(cast_name(&t, r#"/script CastSpellByName("")"#), None);
    }

    /// The bound spell is resolved by the SAME law the press uses, so the swirl and the cast can
    /// never disagree about the rank.
    #[test]
    fn bound_spell_resolves_through_the_book() {
        use benilla_ui::script::SpellSlotView;

        let book = SpellBookState {
            tabs: Vec::new(),
            slots: vec![
                SpellSlotView {
                    spell_id: 133,
                    name: "Fireball".into(),
                    rank: Some("Rank 1".into()),
                    ..Default::default()
                },
                SpellSlotView {
                    spell_id: 145,
                    name: "Fireball".into(),
                    rank: Some("Rank 2".into()),
                    ..Default::default()
                },
            ],
        };
        let t = table();
        // No subtext -> the highest known rank.
        assert_eq!(bound_spell(&t, "/cast Fireball", &book), Some(145));
        // A pinned subtext -> that rank, both spacings.
        assert_eq!(bound_spell(&t, "/cast Fireball(Rank 1)", &book), Some(133));
        assert_eq!(bound_spell(&t, "/cast Fireball (Rank 1)", &book), Some(133));
        // Unknown spell / no cast line -> nothing bound (the slot reports no cooldown).
        assert_eq!(bound_spell(&t, "/cast Pyroblast", &book), None);
        assert_eq!(bound_spell(&t, "/say hi", &book), None);
    }
}
