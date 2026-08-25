//! **The reference file is the test oracle** (decision 0675) — the shared half of it.
//!
//! A transcribed window's geometry errors are invisible to every behavioural test: the frames
//! load, the buttons click, the lists populate, and only the *look* is wrong. 0675 built the
//! detector for `FriendsFrame` after two rounds of the director's eye caught what a one-number
//! diff would have caught on its first run, and closed with "for the next transcribed window,
//! write the diff test first". This is that scraper, lifted out of `friends_tests` so the next
//! window costs a call rather than a copy (`QuestTimerFrame` is the first to pay the lower price).
//!
//! Deliberately narrow: it does not try to understand the XML, only to notice that a number moved.
//! Two properties make it a guard rather than a comfort, and both are the caller's to keep —
//! **known-benign differences are an explicit list, never a pattern**, so a new difference cannot
//! hide inside a tolerance; and **it must be verified to fail** (perturb one number by 1 px and
//! watch it name the element).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The extracted reference FrameXML file of that name, or `None` when the install isn't there
/// (these tests skip cleanly without it — it is a gitignored Blizzard asset).
pub(super) fn reference(file: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../WoW/_extracted_framexml")
        .join(file);
    path.is_file().then_some(path)
}

/// Named element → the `<AbsDimension>` pairs inside it, up to the next named element.
///
/// Hand-rolled rather than a regex: this crate has no regex dependency, and adding one for a scan
/// of two files is a worse trade than twenty lines of `find`.
///
/// `$parent*` names repeat across templates (every row template has a `$parentText`), so a bare
/// name would compare one template's column against another's. Each is qualified by the template
/// it lives in — the nearest preceding real name.
fn scrape(text: &str) -> Vec<(String, Vec<(f32, f32)>)> {
    const TAGS: &[&str] = &[
        "<Button name=\"",
        "<Frame name=\"",
        "<EditBox name=\"",
        "<ScrollFrame name=\"",
        "<FontString name=\"",
        "<Texture name=\"",
        "<CheckButton name=\"",
    ];
    // Every (offset, name) where a named element starts, in document order.
    let mut marks: Vec<(usize, String)> = Vec::new();
    for tag in TAGS {
        let mut from = 0;
        while let Some(hit) = text[from..].find(tag) {
            let at = from + hit;
            let name_start = at + tag.len();
            let Some(len) = text[name_start..].find('"') else {
                break;
            };
            marks.push((at, text[name_start..name_start + len].to_string()));
            from = name_start + len;
        }
    }
    marks.sort_by_key(|(at, _)| *at);

    let mut owner = String::new();
    marks
        .iter()
        .enumerate()
        .map(|(i, (at, name))| {
            let end = marks.get(i + 1).map_or(text.len(), |(next, _)| *next);
            let key = if let Some(child) = name.strip_prefix("$parent") {
                format!("{owner}/{child}")
            } else {
                owner = name.clone();
                name.clone()
            };
            (key, dimensions(&text[*at..end]))
        })
        .collect()
}

/// Every `<AbsDimension x=".." y=".."/>` in `chunk`, in order.
fn dimensions(chunk: &str) -> Vec<(f32, f32)> {
    const OPEN: &str = "<AbsDimension x=\"";
    let mut out = Vec::new();
    let mut rest = chunk;
    while let Some(hit) = rest.find(OPEN) {
        rest = &rest[hit + OPEN.len()..];
        let Some((x, tail)) = rest.split_once('"') else {
            break;
        };
        let Some(y_at) = tail.find("y=\"") else { break };
        let Some((y, tail)) = tail[y_at + 3..].split_once('"') else {
            break;
        };
        if let (Ok(x), Ok(y)) = (x.parse(), y.parse()) {
            out.push((x, y));
        }
        rest = tail;
    }
    out
}

/// Assert every element benilla's `ours` file shares with the reference `reference` file carries
/// the reference's `<AbsDimension>` numbers.
///
/// - `ours` is a path under `assets/ui/`; names are matched to the reference's after stripping the
///   house `Benilla` prefix (a window whose frames keep the bare reference names — decision 0591
///   §3 — matches unchanged).
/// - `expected` lists the *deliberate* deviations by their REFERENCE name. Every entry earns a
///   comment at the call site naming its reason; that is what keeps this a guard.
/// - `min_compared` fails the test if fewer elements paired up than that, so a scrape or a naming
///   change that quietly stops comparing anything is caught rather than reported as a pass.
///
/// Elements ours has and the reference does not (our own template faces) are skipped, not failed.
pub(super) fn assert_geometry_matches(
    ours: &str,
    reference: &Path,
    expected: &[&str],
    min_compared: usize,
) {
    assert_geometry_matches_text(
        ours,
        &std::fs::read_to_string(reference).unwrap(),
        expected,
        min_compared,
    );
}

/// [`assert_geometry_matches`] against reference TEXT rather than a file on disk.
///
/// The split exists because not every reference file is *on* disk: `_extracted_framexml/` holds
/// FrameXML only, and a window whose specification is partly a `Blizzard_*` **addon** — the raid
/// grid is the first, decision 1549 — has to read its half straight out of the patch chain
/// (`benilla_formats::open_chain`, the same door `mpqx` uses). Reading it rather than requiring
/// someone to have extracted it first is what keeps that half of the diff ARMED instead of
/// skipped, and it writes nothing into the install.
pub(super) fn assert_geometry_matches_text(
    ours: &str,
    reference_text: &str,
    expected: &[&str],
    min_compared: usize,
) {
    let ours_text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/ui")
            .join(ours),
    )
    .unwrap();
    let theirs: HashMap<String, Vec<(f32, f32)>> = scrape(reference_text).into_iter().collect();

    let mut compared = 0;
    let mut drifted = Vec::new();
    for (name, dims) in scrape(&ours_text) {
        let key = name.replace("Benilla", "");
        let Some(ref_dims) = theirs.get(&key) else {
            continue; // ours alone (the local template faces) — nothing to compare against
        };
        compared += 1;
        if dims != *ref_dims && !expected.contains(&key.as_str()) {
            drifted.push(format!("{key}: ours {dims:?} != ref {ref_dims:?}"));
        }
    }
    assert!(
        compared >= min_compared,
        "only {compared} elements of {ours} matched by name (expected >= {min_compared}) — the \
         scrape or the naming broke"
    );
    assert!(
        drifted.is_empty(),
        "{ours} geometry differs from the reference FrameXML:\n  {}",
        drifted.join("\n  ")
    );
}
