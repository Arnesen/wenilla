//! **Running** a macro, and the one derivation the action bar needs from a macro body: its
//! **bound spell** (decision 0983).
//!
//! ## Running: a macro line is a typed chat line
//!
//! The reference reaches Lua's `SlashCmdList` for `/target`, `/script`, the chat types and the 225
//! emotes — every one of those handlers *is* FrameXML, so a macro that drives them must arrive
//! where a typed line arrives. benilla has exactly one such door already, and it is not a chat
//! window: [`UiScript::push_chat_input`], the seam `WOW_PROBE_CHAT` drives, whose drain
//! (`crate::ui_chat::input`) is the client's whole slash grammar in one place. So a macro run is
//! **its lines pushed onto that queue, in order** — every command a player can type, a macro can
//! run, by construction, and a command added later works in macros the day it lands.
//!
//! That is the *mechanism* implemented once rather than the engine's plumbing aped: the reference
//! does this inside `UIMacros.cpp` (`0x4f1460`'s callees), and how those callees hand a line to the
//! Lua VM is **not recorded in wow-re** — see decision 0983's open question. What is settled is the
//! observable behaviour this reproduces: a `/`-line runs its command, a plain line is sent as chat
//! (the director's own 1.12 `macros-cache.txt` drives `.cheat fly on` that way), and every line in
//! the body runs.
//!
//! ## The bound spell (`[rec+0x564]`, VERIFIED consumer)
//!
//! A macro action-bar slot shows **the macro's own icon** but reports the **cooldown, usability,
//! range and checked state of the spell it casts** — byte-verified: `0x4e5a50`'s macro arm
//! resolves the macro record through `0x4f0f40` and returns `[rec+0x564]` as the slot's spell id
//! (wow-re `action-spell-icon-apis.md` §2), and every `Is*Action`/`GetActionCooldown` binding reads
//! through that resolver. [`bound_spell`] is how that field is filled: the body's first `/cast`
//! line, or a `CastSpellByName("…")` call in a `/script` line.
//!
//! Both forms come from the reference's own `UIMacros.cpp` string block, which holds exactly two
//! things it could match a body against: **`SLASH_CAST%d`** (`0x44cac4` — the global-string prefix
//! whose aliases are `/cast`) and the literal **`CastSpellByName(`** (`0x44cab0`, no format
//! specifier, so it is a *compare*, not a build). Nothing else in that TU parses a body. The
//! derivation is therefore INFERRED from a verified string set plus a verified consumer — named as
//! such in decision 0983.

use benilla_ui::script::{resolve_spell_by_name, SpellBookState};

use crate::ui_chat::commands::{Command, SlashCommands, SlashIndex};

/// The literal the reference matches a `/script`-style body line against (`0x44cab0`).
const CAST_BY_NAME_CALL: &str = "CastSpellByName(";

/// A macro body's runnable lines: split on newlines, trimmed, blanks dropped. Carriage returns go
/// too — a `macros-cache.txt` hand-copied off a Windows install is a real input.
pub(crate) fn macro_lines(body: &str) -> impl Iterator<Item = &str> {
    body.lines().map(str::trim).filter(|l| !l.is_empty())
}

/// The spell name a macro body casts, if any — [`bound_spell`]'s parse half, split out because it
/// is the whole INFERRED part and deserves its own test.
///
/// Walks the body in order and returns the FIRST match of either form:
/// - a line whose command resolves to [`SlashIndex::Cast`] through the boot-built alias table
///   (never a hardcoded `"/cast"` — decision 0881's law), argument taken whole; or
/// - a line containing `CastSpellByName(` with a quoted first argument.
pub(crate) fn cast_name(table: &SlashCommands, body: &str) -> Option<String> {
    for line in macro_lines(body) {
        if let Some(rest) = line.strip_prefix('/') {
            let (cmd, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            if table.lookup(cmd) == Some(Command::Slash(SlashIndex::Cast)) {
                let args = args.trim();
                if !args.is_empty() {
                    return Some(args.to_string());
                }
                continue;
            }
        }
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
