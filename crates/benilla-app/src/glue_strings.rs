//! The 1.12 glue string table (`Interface\GlueXML\GlueStrings.lua`), read off the MPQ chain at
//! startup — the localized text the glue screens quote: faction/race/class description paragraphs
//! (`FACTION_INFO_*`, `RACE_INFO_*`, `ABILITY_INFO_*`, `CLASS_*`), the per-race customization dial
//! labels (`HAIR_<tok>_STYLE`, `FACIAL_HAIR_<tok>`), and the button captions.
//!
//! Runtime-read, never embedded: the paragraphs are Blizzard content, so they load from the
//! player's own client data like every other asset (the repo's never-commit rule). The file is
//! plain `KEY = "value";` Lua assignments, one per line — a full Lua VM would be theater here;
//! [`parse_glue_strings`] handles exactly that shape (and skips everything else).

use std::collections::HashMap;

use bevy::prelude::*;

use benilla_assets::{LockRecover, WorldAssets};

const GLUE_STRINGS: &str = "Interface\\GlueXML\\GlueStrings.lua";

/// The glue string table. Present but possibly empty (missing client data — the graceful-absence
/// posture; callers fall back to their built-in captions).
#[derive(Resource, Default)]
pub(crate) struct GlueStrings(HashMap<String, String>);

impl GlueStrings {
    /// The string for a key, or `None` (unknown key / no data).
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// The string for a key, falling back to a built-in caption.
    pub(crate) fn text<'a>(&'a self, key: &str, fallback: &'a str) -> &'a str {
        self.get(key).unwrap_or(fallback)
    }
}

/// Startup: read + parse the glue string table off the chain (after the chain exists).
pub(crate) fn load_glue_strings(mut commands: Commands, assets: Option<Res<WorldAssets>>) {
    let mut table = GlueStrings::default();
    if let Some(assets) = assets {
        match assets.chain.lock_recover().read_file(GLUE_STRINGS) {
            Ok(bytes) => {
                table.0 = parse_glue_strings(&String::from_utf8_lossy(&bytes));
                info!("glue strings: {} entries", table.0.len());
            }
            Err(e) => warn!("glue strings unavailable ({e:#}) — built-in captions only"),
        }
    }
    commands.insert_resource(table);
}

/// Parse the `KEY = "value";` assignments (one per line; `\n`/`\t`/`\"`/`\\` escapes unfolded).
/// Anything else — comments, code, multi-line constructs — is skipped.
fn parse_glue_strings(src: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in src.lines() {
        let Some((key, rest)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('"') else {
            continue;
        };
        // Unescape up to the closing quote; a line whose quote never closes is skipped.
        let mut value = String::new();
        let mut chars = rest.chars();
        let mut closed = false;
        while let Some(c) = chars.next() {
            match c {
                '"' => {
                    closed = true;
                    break;
                }
                '\\' => match chars.next() {
                    Some('n') => value.push('\n'),
                    Some('t') => value.push('\t'),
                    Some('r') => {}
                    Some(other) => value.push(other),
                    None => break,
                },
                other => value.push(other),
            }
        }
        if closed {
            out.insert(key.to_string(), value);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_assignments_and_skips_the_rest() {
        let src = r#"
MALE = "Male";
FACTION_INFO_HORDE = "Four races\ncomprise the Horde";
QUOTED = "a \"b\" c";
-- a comment = "not a string";
CODE = getglobal("nope");
BROKEN = "no close
"#;
        let t = parse_glue_strings(src);
        assert_eq!(t.get("MALE").map(String::as_str), Some("Male"));
        assert_eq!(
            t.get("FACTION_INFO_HORDE").map(String::as_str),
            Some("Four races\ncomprise the Horde")
        );
        assert_eq!(t.get("QUOTED").map(String::as_str), Some(r#"a "b" c"#));
        assert!(!t.contains_key("CODE"));
        assert!(!t.contains_key("BROKEN"));
        assert_eq!(t.len(), 3);
    }
}
