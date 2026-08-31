//! Every `ERR_*` key this workspace names must be a real row of the client's message catalog.
//!
//! benilla writes these keys as string literals at ~160 call sites, and each one used to arrive by
//! hand-tracing a `push <id>; call CGGameUI::DisplayError` in the binary. A typo, a key from a
//! later expansion, or a plausible-looking invention would all have been invisible: the string
//! simply would not resolve in `GlobalStrings.lua`, and the reference's own data-suppression rule
//! means benilla shows *nothing* for an unresolvable key. Silence is the failure mode, which is the
//! worst kind — nobody files a bug about a toast they never saw.
//!
//! Since decision 1770 the surface those keys are shown on is read from
//! [`benilla_ui::messages`] rather than carried beside them, so a key that is not in the catalog
//! also silently takes the fallback. This walk is what keeps that fallback unreachable: it is the
//! tripwire that turns "we believe these are message ids" into a checked claim.
//!
//! It reads source text rather than any registry the code keeps, deliberately — a registry could
//! only contain what somebody remembered to register.

use std::collections::BTreeSet;
use std::path::Path;

/// Not message ids, and not meant to be — both are fixtures for the unknown-key *fallback*, which
/// needs a key the client does not have in order to be tested at all: `ui_petition::lines`'
/// degrade-to-generic arm, and `benilla_ui::messages`' own proof that `kind_of` answers for a key
/// it has never heard of. Anything else appearing here is a defect, not an entry to add.
const NOT_A_MESSAGE: &[&str] = &["ERR_SOMETHING_UNCARVED", "ERR_NOT_A_REAL_MESSAGE"];

/// The generated table itself, which is the answer and not a question — walking it would make
/// every key trivially present and the check vacuous.
const GENERATED: &str = "catalog.rs";

fn walk(dir: &Path, into: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            walk(&path, into);
        } else if path.extension().is_some_and(|e| e == "rs")
            && path.file_name().is_some_and(|n| n != GENERATED)
        {
            into.push(std::fs::read_to_string(&path).expect("read source"));
        }
    }
}

/// Pull every `"ERR_…"` string literal out of a source file, the way the call sites write them.
fn err_keys(src: &str) -> impl Iterator<Item = String> + '_ {
    src.match_indices("\"ERR_").filter_map(|(i, _)| {
        let rest = &src[i + 1..];
        let end = rest.find('"')?;
        let key = &rest[..end];
        key.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            .then(|| key.to_string())
    })
}

#[test]
fn every_error_key_in_the_source_is_a_catalog_row() {
    let mut sources = Vec::new();
    walk(Path::new("src"), &mut sources);
    walk(Path::new("../benilla-ui/src"), &mut sources);
    assert!(sources.len() > 100, "the walk found almost nothing to read");

    let keys: BTreeSet<String> = sources.iter().flat_map(|s| err_keys(s)).collect();
    // 163 at the time of writing — the point is only that the walk found the vocabulary and not
    // a handful of files, so this is a floor and not a ledger to keep updated.
    assert!(
        keys.len() > 150,
        "expected the hand-written `ERR_*` vocabulary, found {}",
        keys.len()
    );

    let strays: Vec<&String> = keys
        .iter()
        .filter(|k| !NOT_A_MESSAGE.contains(&k.as_str()))
        .filter(|k| benilla_ui::messages::by_key(k).is_none())
        .collect();
    assert!(
        strays.is_empty(),
        "these keys are not rows of the 5875 message catalog, so the client would show nothing \
         for them: {strays:?}"
    );
}
