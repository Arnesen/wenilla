//! **The cover-input wall** — the loading screen's input rule, counted from source on every
//! `cargo test`, so the swallow list can never quietly fall behind what the client actually reads.
//!
//! `loading_screen/input.rs` takes the whole input plane while the cover is up, at the source, in
//! `PreUpdate`. That cut is what makes the rule survive a consumer nobody remembered — the world
//! pick chain, which hovered units and changed the hardware cursor *through* the loading screen
//! until the director noticed. But a source cut only covers the channels it names, and bevy has
//! more channels than we currently read: touch, IME, gamepads, the trackpad gestures. The day
//! someone reads one of those, the cover has to grow a line — and nothing in the compiler will say
//! so.
//!
//! This is that "nothing". It scans every workspace source for the input channels the client
//! reaches for and requires each to carry a **verdict** in [`VERDICTS`] below:
//!
//! - **`Swallowed`** — the cover empties it. Cross-checked: the name must literally appear in the
//!   gate's own source, so the table cannot claim coverage the gate does not implement.
//! - **`Open`** — deliberately *not* swallowed, with the reason written down. Window lifecycle is
//!   the whole of this class today: a close request, a focus change and an occlusion change must
//!   still land while a load is running.
//! - **`Plumbing`** — a type, enum, plugin or system-set name that carries no player input at all.
//!
//! An unclassified name fails the test with the question it wants answered: *does the cover
//! swallow this?* Answering it is one line here and, when the answer is yes, one line in the gate.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    /// The cover empties this channel — see `loading_screen/input.rs`.
    Swallowed,
    /// Deliberately left open under the cover. The `&str` is the reason.
    Open(&'static str),
    /// Not a player-input channel at all.
    Plumbing,
}
use Verdict::{Open, Plumbing, Swallowed};

/// Every `bevy::input::…` / `bevy::window::…` leaf the workspace names, plus the prelude-sourced
/// input items, each with its verdict. **The wall itself.**
const VERDICTS: &[(&str, Verdict)] = &[
    // ── The channels the cover takes ────────────────────────────────────────────────────────
    ("ButtonInput", Swallowed),
    ("KeyboardInput", Swallowed),
    ("MouseButtonInput", Swallowed),
    ("MouseMotion", Swallowed),
    ("MouseWheel", Swallowed),
    ("CursorMoved", Swallowed),
    ("AccumulatedMouseMotion", Swallowed),
    ("AccumulatedMouseScroll", Swallowed),
    // The pointer position is a *field*, not a message; the cover blanks it, which is why every
    // hit-test in the client goes quiet without knowing the cover exists.
    ("cursor_position", Swallowed),
    ("physical_cursor_position", Swallowed),
    // ── Deliberately open under the cover ───────────────────────────────────────────────────
    (
        "WindowCloseRequested",
        Open("closing the window during a load must work — it is the OS asking, not the player"),
    ),
    (
        "WindowFocused",
        Open("alt-tab bookkeeping; `cursor.rs` re-asserts the hardware cursor on the focus edge"),
    ),
    (
        "WindowOccluded",
        Open("the present-mode throttle: a load behind another window must still throttle"),
    ),
    (
        "CursorOptions",
        Open("an OUTPUT (grab/visible), not a channel — the cover has nothing to take from it"),
    ),
    // ── Not input at all ────────────────────────────────────────────────────────────────────
    ("ButtonState", Plumbing),
    // The cursor OUTPUTS — the art and the grab mode benilla *writes*. Nothing arrives through
    // them, so there is nothing for the cover to take.
    ("CursorGrabMode", Plumbing),
    ("CursorIcon", Plumbing),
    ("CustomCursor", Plumbing),
    ("CustomCursorImage", Plumbing),
    ("InputPlugin", Plumbing),
    ("InputSystems", Plumbing),
    ("Key", Plumbing),
    ("KeyCode", Plumbing),
    ("MouseButton", Plumbing),
    ("MouseScrollUnit", Plumbing),
    ("NativeKey", Plumbing),
    ("ExitCondition", Plumbing),
    ("Monitor", Plumbing),
    ("MonitorSelection", Plumbing),
    ("PresentMode", Plumbing),
    ("PrimaryWindow", Plumbing),
    ("RawHandleWrapper", Plumbing),
    ("Window", Plumbing),
    ("WindowLevel", Plumbing),
    ("WindowMode", Plumbing),
    ("WindowPlugin", Plumbing),
    ("WindowPosition", Plumbing),
    ("WindowResolution", Plumbing),
];

/// The gate's own source — the cross-check target for every `Swallowed` row.
const GATE: &str = "crates/benilla-app/src/loading_screen/input.rs";

/// The prelude re-exports these, so a source can name them with no `bevy::input::` path in sight.
/// Scanned for by bare identifier; anything matched still has to carry a verdict above.
const PRELUDE_INPUT_NAMES: &[&str] = &[
    "ButtonInput",
    "AccumulatedMouseMotion",
    "AccumulatedMouseScroll",
    "cursor_position",
    "physical_cursor_position",
];

#[test]
fn every_input_channel_the_client_reads_has_a_verdict_under_the_cover() {
    let root = workspace_root();
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for file in workspace_sources(&root) {
        // The wall names every channel in its own prose; scanning it would only find itself.
        if file.ends_with("tests/cover_input_wall.rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .to_string_lossy()
            .to_string();
        for name in input_names(&text) {
            found.entry(name).or_default().insert(rel.clone());
        }
    }

    let table: BTreeMap<&str, Verdict> = VERDICTS.iter().copied().collect();
    let unclassified: Vec<_> = found
        .iter()
        .filter(|(name, _)| !table.contains_key(name.as_str()))
        .collect();
    assert!(
        unclassified.is_empty(),
        "the client reads input channels the cover has never been asked about.\n\
         For each, decide: does the loading cover swallow it? If yes, add the line to {GATE} \
         and a `Swallowed` row to VERDICTS; if not, an `Open` row saying why.\n{}",
        unclassified
            .iter()
            .map(|(name, files)| format!(
                "  {name} — {}",
                files.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // A `Swallowed` verdict is a claim about the gate's source; hold it to it. (The reverse is not
    // asserted: the gate may name a channel nothing reads yet — that is the cover being ahead.)
    let gate = std::fs::read_to_string(root.join(GATE)).expect("the gate's source");
    let unbacked: Vec<&str> = VERDICTS
        .iter()
        .filter(|(name, v)| *v == Swallowed && !gate.contains(name))
        .map(|(name, _)| *name)
        .collect();
    assert!(
        unbacked.is_empty(),
        "VERDICTS claims the cover swallows these, but {GATE} never names them: {unbacked:?}"
    );

    // And the table itself must not rot: a row nothing in the workspace names any more is a row to
    // delete, not to keep as decoration. (`Plumbing` rows are exempt — they exist to absorb noise.)
    let stale: Vec<&str> = VERDICTS
        .iter()
        .filter(|(name, v)| *v != Plumbing && !found.contains_key(*name))
        .map(|(name, _)| *name)
        .collect();
    assert!(
        stale.is_empty(),
        "VERDICTS rows nothing reads any more — delete them: {stale:?}"
    );
}

/// Every input-ish identifier `text` names: the leaf of any `bevy::input::…`/`bevy::window::…`
/// path (grouped `use` braces expanded), plus the prelude-sourced names.
fn input_names(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for prefix in ["bevy::input::", "bevy::window::"] {
        let mut rest = text;
        while let Some(at) = rest.find(prefix) {
            rest = &rest[at + prefix.len()..];
            // `use bevy::input::mouse::{A, B};` — take the braced group whole.
            let path: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
                .collect();
            let leaf_source = if rest[path.len()..].starts_with('{') {
                let end = rest[path.len()..].find('}').unwrap_or(0);
                rest[path.len() + 1..path.len() + end].to_string()
            } else {
                path.clone()
            };
            for item in leaf_source.split(',') {
                // `keyboard::KeyboardInput`, `KeyCode::Space`, `self` → the first segment that is
                // a type-shaped name (upper-case initial) wins; a module segment is skipped.
                if let Some(name) = item
                    .split("::")
                    .map(str::trim)
                    .find(|seg| seg.starts_with(|c: char| c.is_ascii_uppercase()))
                {
                    out.insert(name.to_string());
                }
            }
            rest = &rest[path.len()..];
        }
    }
    for name in PRELUDE_INPUT_NAMES {
        if text.contains(name) {
            out.insert((*name).to_string());
        }
    }
    out
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/benilla-app → the workspace root")
        .to_path_buf()
}

/// Every `.rs` under each crate's `src/` and `examples/` — the client's own code. Test files are
/// included on purpose: a test that reaches for a new input channel is still a signal that the
/// channel exists, and the verdict costs one line.
fn workspace_sources(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for crate_dir in std::fs::read_dir(root.join("crates"))
        .into_iter()
        .flatten()
        .flatten()
    {
        for sub in ["src", "examples", "tests"] {
            let dir = crate_dir.path().join(sub);
            if dir.is_dir() {
                collect_rs(&dir, &mut out);
            }
        }
    }
    out.sort();
    out
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
}
