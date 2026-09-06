//! **No user-facing sentence is written in Rust when the reference ships one.**
//!
//! The real client never composes display text: every sentence is a key into `GlobalStrings.lua`
//! (in-game) or `GlueStrings.lua` (the login/character screens), resolved at runtime from the
//! player's own install. Writing the English in Rust throws away three things at once —
//! localization, and, for anything that goes through the message catalog, the *surface* the
//! message is shown on and the *voice line* it speaks with (decisions 1770, 1815, 2035).
//!
//! **Why a tripwire and not a review rule.** Decision 2035 found a loot-refusal table that had
//! composed its own eight sentences, six of which said something 1.12 never says, under a comment
//! asserting they were quoted from the reference. The comment was the only check there had ever
//! been, and a comment cannot fail. What makes that class *findable* is that a re-typed string is
//! byte-identical to the shipped one — so a walk that normalises both sides and compares them finds
//! every instance mechanically, which is what this does.
//!
//! **What it cannot find, stated plainly.** Text that matches nothing is invisible here: an
//! *invented* sentence (2035's "Those pockets are already empty.", for a code 1.12 has no string
//! for) has no shipped counterpart to match against. This tripwire catches the re-typing class,
//! which is the large one; the invention class needs a reader who checks the reference.
//!
//! **A literal beside a key lookup is correct and is not flagged** — a fallback for an install
//! whose chain lacks the key is the reference's own `UNKNOWNOBJECT` shape. The walk only counts a
//! literal whose enclosing function resolves nothing.
//!
//! [`ALLOWED`] is a **ratchet**: it records what was already drifted when the tripwire was built,
//! per file, and the counts may only ever go **down**. Converting a file means lowering its number
//! (or deleting the row); a file not listed may carry none at all. Skips without client data.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The drift still standing, per file: **252 literals over 32 files**, each a sentence the
/// reference ships and we re-typed.
///
/// **These numbers may only decrease.** They are not a budget to spend; every one is a string that
/// will not translate, and some carry a surface and a voice line with them. Lower the count when
/// you convert a file, and delete the row at zero — a row left stale fails this test too, so a
/// conversion cannot quietly re-permit the drift it retired. `char_create` (2045) and `ui_trade`
/// (2042, a neighbouring session) are both converted and have no row: that is what retiring one
/// looks like, and the second was caught by this test rather than by remembering.
///
/// **Regenerate these against a clean tree, never mid-edit.** The walk is deterministic on the
/// sources plus the shipped tables (two consecutive runs agree exactly), but a baseline captured
/// while other files are half-converted records counts that no later tree will reproduce — which
/// is exactly how the first cut of this list carried a row the gates then failed on.
const ALLOWED: &[(&str, usize)] = &[
    ("benilla-app/src/ui_chat/frames.rs", 48),
    ("benilla-app/src/ui_guild/lines.rs", 34),
    ("benilla-app/src/ui_tooltip/mod.rs", 28),
    ("benilla-app/src/ui_party/mod.rs", 12),
    ("benilla-app/src/ui_social/mod.rs", 18),
    ("benilla-app/src/ui_petition/lines.rs", 15),
    ("benilla-app/src/ui_chat/feed.rs", 13),
    ("benilla-ui/src/script/tooltip_item/render.rs", 9),
    ("benilla-app/src/ui_unit.rs", 6),
    ("benilla-formats/src/spells/tokens.rs", 6),
    ("benilla-app/src/ui_duel.rs", 5),
    ("benilla-app/src/ui_social/feed.rs", 4),
    ("benilla-app/src/ui_talent.rs", 4),
    ("benilla-ui/src/script/tooltip_unit.rs", 4),
    ("benilla-app/src/ui_quest.rs", 3),
    ("benilla-app/src/ui_quest_log.rs", 3),
    ("benilla-app/src/ui_script/resolve_bench.rs", 3),
    ("benilla-ui/src/script/tooltip_item/names.rs", 3),
    ("benilla-ui/src/script/tradeskill/view.rs", 3),
    ("benilla-app/src/net/apply/chat.rs", 2),
    ("benilla-app/src/ui_binder.rs", 2),
    ("benilla-app/src/ui_char.rs", 2),
    ("benilla-app/src/ui_trainer/law.rs", 2),
    ("benilla-app/src/ui_trainer/mod.rs", 2),
    ("benilla-ui/src/script/tooltip_item/mod.rs", 2),
    ("benilla-ui/src/script/tooltip_spell.rs", 2),
    ("benilla-ui/src/script/unit/mod.rs", 2),
    ("benilla-app/src/death.rs", 1),
    ("benilla-app/src/net/apply/quests.rs", 1),
    ("benilla-app/src/ui_chat/input/mod.rs", 1),
    ("benilla-app/src/ui_items/feed.rs", 1),
    ("benilla-ui/src/script/talent.rs", 1),
];

/// Paths that are never player-facing: dev instruments, probes, capture harnesses and benches.
/// A `.learn <spell>` GM command is not a UI string even when it collides with one.
fn is_instrument(rel: &str) -> bool {
    [
        "/capture/",
        "/bin/",
        "/probes/",
        "debug_panel",
        "shape_gate",
        "framexml_diff",
    ]
    .iter()
    .any(|p| rel.contains(p))
}

/// Collapse a shipped value and a Rust literal onto the same shape: every placeholder — the
/// reference's `%s`/`%d`/`%1$s` and Rust's `{}`/`{name}` alike — becomes one marker, `\32` is the
/// escaped space it stands for, runs of whitespace collapse, and case is dropped.
fn normalize(s: &str) -> String {
    let s = s.replace("\\32", " ");
    let mut out = String::with_capacity(s.len());
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            '{' => {
                // a Rust format hole
                while i < b.len() && b[i] != '}' {
                    i += 1;
                }
                i += 1;
                out.push('\u{1}');
            }
            '%' => {
                let mut j = i + 1;
                while j < b.len()
                    && (b[j].is_ascii_digit() || b[j] == '$' || b[j] == '.' || b[j] == '-')
                {
                    j += 1;
                }
                if j < b.len() && matches!(b[j], 's' | 'd' | 'f' | 'c' | 'g') {
                    out.push('\u{1}');
                    i = j + 1;
                } else {
                    out.push('%');
                    i += 1;
                }
            }
            c => {
                out.push(c.to_ascii_lowercase());
                i += 1;
            }
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse `KEY = "value";` out of a Lua string table.
fn lua_table(src: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in src.lines() {
        let Some((k, rest)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        if k.is_empty()
            || !k
                .bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
        {
            continue;
        }
        let rest = rest.trim();
        let Some(rest) = rest.strip_prefix('"') else {
            continue;
        };
        if let Some(end) = rest.rfind("\";") {
            out.insert(k.to_string(), rest[..end].to_string());
        }
    }
    out
}

fn walk(dir: &Path, into: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !matches!(name, "target" | ".git" | "tests" | "benches" | "examples") {
                walk(&path, into);
            }
        } else if name.ends_with(".rs") && !name.contains("test") {
            into.push(path);
        }
    }
}

/// Does this function resolve a key at all? If so, a literal inside it is a fallback.
const RESOLVERS: &[&str] = &[
    "strings.get(",
    ".text(",
    "keyed_line",
    "globals().get::<String>",
    "GlueStrings",
    "by_key(",
    "UiError::key",
    "glue_strings",
];

#[test]
fn no_user_facing_sentence_is_written_in_rust_when_the_reference_ships_one() {
    let data = match benilla_formats::wow_data() {
        Some(d) => d,
        None => return,
    };
    let mut chain = benilla_formats::open_chain(&data).expect("open chain");
    let mut shipped = HashMap::new();
    for file in [
        "Interface\\FrameXML\\GlobalStrings.lua",
        "Interface\\GlueXML\\GlueStrings.lua",
    ] {
        let src = chain
            .read_file(file)
            .unwrap_or_else(|e| panic!("{file}: {e}"));
        for (k, v) in lua_table(&String::from_utf8_lossy(&src)) {
            let n = normalize(&v);
            // A one-word or punctuation-only value ("Locked", "%s") is too weak a signal: it
            // collides with ordinary program text. Sentences are what this is after.
            if n.split(' ').count() >= 2 && n.chars().any(|c| c.is_ascii_lowercase()) {
                shipped.entry(n).or_insert(k);
            }
        }
    }

    let mut sources = Vec::new();
    walk(Path::new("src"), &mut sources);
    walk(Path::new("../benilla-ui/src"), &mut sources);
    walk(Path::new("../benilla-formats/src"), &mut sources);

    let allowed: HashMap<&str, usize> = ALLOWED.iter().copied().collect();
    let mut found: HashMap<String, Vec<String>> = HashMap::new();

    for path in &sources {
        // The walk starts at this crate's `src` and at the two sibling crates by relative path;
        // name every hit `<crate>/src/...` so the ratchet rows read the same wherever it ran.
        let raw = path.to_string_lossy().to_string();
        let rel = match raw.strip_prefix("../") {
            Some(sibling) => sibling.to_string(),
            None => format!("benilla-app/{raw}"),
        };
        if is_instrument(&rel) {
            continue;
        }
        let src = strip_test_modules(&std::fs::read_to_string(path).expect("read source"));
        for (fname, body) in split_fns(&src) {
            if RESOLVERS.iter().any(|r| body.contains(r)) {
                continue; // a literal here is a fallback beside a real lookup
            }
            for lit in string_literals(body) {
                let n = normalize(&lit);
                if n.split(' ').count() < 2 {
                    continue;
                }
                if let Some(key) = shipped.get(&n) {
                    found
                        .entry(rel.clone())
                        .or_default()
                        .push(format!("{key} — {fname}: {lit:?}"));
                }
            }
        }
    }

    let mut over = Vec::new();
    for (file, hits) in &found {
        let budget = allowed.get(file.as_str()).copied().unwrap_or(0);
        if hits.len() > budget {
            over.push(format!(
                "\n  {file}: {} literals, ratchet allows {budget}\n{}",
                hits.len(),
                hits.iter()
                    .map(|h| format!("      {h}\n"))
                    .collect::<String>()
            ));
        }
    }
    // The ratchet only bites downward if a *stale* row is an error too — otherwise a converted
    // file's row lingers and silently re-permits the drift it was meant to retire.
    for (file, budget) in ALLOWED {
        let actual = found.get(*file).map_or(0, |h| h.len());
        assert!(
            actual >= *budget,
            "\nRATCHET STALE: {file} now has {actual} re-typed literals but its row still allows \
             {budget}.\nLower it to {actual} (or delete the row at 0) — the count may only go down."
        );
    }
    assert!(
        over.is_empty(),
        "\nA user-facing sentence is written in Rust where the reference ships the string.\n\
         Resolve it by key instead — `keyed_line`/`keyed_line_s` where a `UiScript` is in hand, \
         `UiError::key` into a queue at the net bridge, `GlueStrings::text` on the glue screens.\n\
         (decision 2035){}",
        over.join("")
    );
}

/// Cut every `#[cfg(test)]` block out of a source file before scanning it.
///
/// Without this the walk reads test fixtures as production text — `ui_action::cast_fail`'s unit
/// tests build a *fake* GlobalStrings map whose 48 entries are, by construction, byte-identical to
/// the shipped strings. Those are the test doing its job, not drift.
fn strip_test_modules(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(at) = rest.find("#[cfg(test)]") {
        out.push_str(&rest[..at]);
        // skip to the opening brace of the item that attribute guards, then brace-match it
        let after = &rest[at..];
        let Some(open) = after.find('{') else { break };
        let mut depth = 0usize;
        let mut end = None;
        for (i, c) in after[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(open + i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        match end {
            Some(e) => rest = &after[e..],
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// Split a source file into `(fn name, body)` pairs — crude but enough to ask "does the enclosing
/// function resolve a key?", which is the only question here.
fn split_fns(src: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut starts: Vec<(usize, &str)> = Vec::new();
    for (i, _) in src.match_indices("fn ") {
        // a top-levelish `fn`: preceded only by whitespace/visibility on its line
        let line_start = src[..i].rfind('\n').map_or(0, |n| n + 1);
        let prefix = &src[line_start..i];
        if !prefix.trim_start().is_empty()
            && !matches!(
                prefix.trim(),
                "pub" | "pub(crate)" | "pub(super)" | "const" | "async"
            )
            && !prefix.trim().starts_with("pub(")
        {
            continue;
        }
        let rest = &src[i + 3..];
        let name_end = rest
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        starts.push((i, &rest[..name_end]));
    }
    for (n, (i, name)) in starts.iter().enumerate() {
        let end = starts.get(n + 1).map_or(bytes.len(), |(j, _)| *j);
        out.push((*name, &src[*i..end]));
    }
    if out.is_empty() {
        out.push(("<file>", src));
    } else if let Some((first, _)) = starts.first() {
        out.push(("<consts>", &src[..*first])); // module-level `const … : &str = "…"`
    }
    out
}

/// Every double-quoted literal in a chunk, skipping comment lines and developer-facing macros.
fn string_literals(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in body.lines() {
        let t = line.trim_start();
        if t.starts_with("//") {
            continue;
        }
        if [
            "debug!",
            "info!",
            "warn!",
            "error!",
            "trace!",
            "println!",
            "eprintln!",
            "panic!",
            "assert",
            "unreachable!",
            "todo!",
            ".expect(",
            "unwrap_or_else",
        ]
        .iter()
        .any(|m| line.contains(m))
        {
            continue;
        }
        let mut rest = line;
        while let Some(start) = rest.find('"') {
            let after = &rest[start + 1..];
            let Some(end) = after.find('"') else { break };
            let lit = &after[..end];
            if lit.len() >= 6 && lit.contains(' ') && lit.chars().any(|c| c.is_ascii_lowercase()) {
                out.push(lit.to_string());
            }
            rest = &after[end + 1..];
        }
    }
    out
}
