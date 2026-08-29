//! **Where the WoW install is** — THE ONE ANSWER (decision 1175).
//!
//! 0954 made every path benilla *writes* resolve through one module, on the grounds that a
//! hand-built path is a place the rule can be got wrong. Its *input* path never got the same
//! treatment: the string
//! `PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")` was hand-copied to 215+
//! sites, alongside 11 bare `var("WOW_DATA")` reads and 271 copies of the same four-line "is it
//! there? if not, skip" guard whose message had drifted into five variants. Every one of those is
//! a place the release rule would have to be written again — and the first Windows build finds
//! every place someone forgot. This module is the rule, written once.
//!
//! **The rule, in the director's words:** *everything should always assume that the wow/data
//! folder is in the project folder or the folder the binary is run from.*
//!
//! It lives in `benilla-formats` because every consumer already depends on it — `benilla-app`,
//! `benilla-assets`, `benilla-world`, and the two detached probes — and because this crate already
//! owns [`crate::Chain`], the thing you open *with* the answer.
//!
//! ## Resolution order
//!
//! 1. **`$WOW_DATA`** — the explicit override, for a second install or a non-standard layout. Kept
//!    because the director's rule is about what the client *assumes*, not about removing the
//!    escape hatch; the sprawl was 11 reads of it, not the variable itself. **Set and empty**
//!    (`WOW_DATA=`) means *there is no install*: the ladder stops there and returns nothing, which
//!    is how a machine that has an install can still run the no-install path (decision 1451).
//! 2. **`<project folder>/WoW/Data`** — `#[cfg(feature = "dev")]` only. Computed from **this
//!    crate's** `CARGO_MANIFEST_DIR`, which lands on the same workspace root whichever crate is
//!    asking, and is what the repo-root `WoW` symlink points at. Gated because a shipped binary
//!    must not carry the build machine's source tree: that is the whole point of the record.
//! 3. **`<exe dir>/Data`, then `<exe dir>/WoW/Data`** — the release convention. Drop benilla into
//!    your WoW folder, or drop a `WoW/` folder beside benilla, and double-click. Both spellings
//!    are cheap to accept and a player will try both.
//!
//! Every candidate must **exist** to be chosen, so a stale `$WOW_DATA` falls through to a real
//! install rather than poisoning the run. [`wow_data`] returns `None` when none of them do — the
//! honest answer, and the one [`wow_data_or_skip`] turns into a uniform test skip.
//!
//! **The feature-unification trap** (called out in the record, and the reason
//! `scripts/gates.sh`'s `--no-default-features` line exists): `dev` is default-on here, so every
//! dependent must declare `default-features = false` and re-export it, or a player build pulls
//! candidate 2 back in through unification and the binary carries `/Users/…` after all. The gate
//! is what catches getting this wrong; [`tests::a_player_build_carries_no_source_tree_path`] is
//! what catches it here.

use std::path::{Path, PathBuf};

/// The vanilla `Data` directory, or `None` when there is no install to be found.
///
/// See the module header for the resolution order and why each step is there. Cheap enough to call
/// at a use site (three `is_dir` stats at worst) — there is deliberately no cache, because a
/// `OnceLock` would freeze the answer across a `$WOW_DATA` change.
#[cfg(not(target_arch = "wasm32"))]
pub fn wow_data() -> Option<PathBuf> {
    candidates().into_iter().find(|c| c.is_dir())
}

/// The web target's answer is not a ladder at all: there is no filesystem to probe candidates
/// against, and every chain file is fetched over HTTP against the Data URL scheme a companion web
/// host answers (Lane A ↔ Lane H of the wasm plan), always rooted at `/data` on this page's own
/// origin. [`crate::Chain::open`] on wasm ignores whatever path it's handed and asks
/// [`crate::web::data_base`] directly — this still returns `Some` so call sites written against
/// the native "is there an install?" contract (`wow_data().is_some()`) get the right answer on web
/// too, without needing their own `cfg`.
#[cfg(target_arch = "wasm32")]
pub fn wow_data() -> Option<PathBuf> {
    Some(PathBuf::from("/data"))
}

/// Every place [`wow_data`] looks, in order, whether or not it exists — the resolver's own
/// explanation of itself.
///
/// Public because "no install found" is a message a human has to act on: the skip in
/// [`wow_data_or_skip`] and any future first-run screen both want to say *where we looked*, not
/// just that we failed.
pub fn candidates() -> Vec<PathBuf> {
    candidates_from(
        std::env::var_os("WOW_DATA").map(PathBuf::from),
        std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_path_buf)),
    )
}

/// [`candidates`] with the two environment facts passed in, so the ladder can be tested without
/// touching the process environment.
///
/// That is not tidiness — it is the fix for a real flake. Once every test in the workspace resolves
/// its install through this module, a test that *sets* `$WOW_DATA` poisons every other test running
/// concurrently in the same process, and the failure moves around depending on scheduling. Before
/// 1175 each test baked its own `CARGO_MANIFEST_DIR` path and was immune. The env read now happens
/// in exactly one place that no test mutates; the wiring of that one read is covered out-of-process
/// by `tests/wow_data_env.rs`.
fn candidates_from(override_dir: Option<PathBuf>, exe_dir: Option<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::with_capacity(4);

    // 1 · the explicit override — and its EMPTY spelling. `WOW_DATA=` (set, no value) is the
    // answer *there is no install*: it returns no candidates at all, so [`wow_data`] is `None`
    // even in a dev build with the project folder sitting right there. It exists because the
    // no-install boot path — which every player who unzips benilla into the wrong folder takes —
    // was unreachable in any build we run on this machine, and rotted until it panicked on frame
    // one (decision 1451). `scripts/gates.sh` runs the enforcer under it on every commit; a
    // session can see what a player without data sees with `WOW_DATA= cargo play`.
    if let Some(over) = override_dir {
        if over.as_os_str().is_empty() {
            return Vec::new();
        }
        out.push(over);
    }

    // 2 · the project folder — dev builds only. `CARGO_MANIFEST_DIR` is THIS crate's, so it is the
    // same workspace root for every caller, including the two detached probes. Walked with
    // `ancestors` rather than joined with `../..` so the path prints readably in the "looked in"
    // message a human has to act on.
    #[cfg(feature = "dev")]
    if let Some(root) = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2) {
        out.push(root.join("WoW/Data"));
    }

    // 3 · beside the binary. `current_exe` fails only on exotic platforms and on a deleted exe;
    // there is nothing to fall back to, so an error is simply "no candidate here".
    if let Some(dir) = exe_dir {
        out.push(dir.join("Data"));
        out.push(dir.join("WoW/Data"));
    }

    out
}

/// The install, or **skip this test** — the one replacement for 271 hand-copied existence guards
/// and the five skip messages they had drifted into.
///
/// Expands to an expression, so it reads as an ordinary binding:
///
/// ```ignore
/// let data = wow_data_or_skip!();          // in a `-> ()` test
/// let data = wow_data_or_skip!(None);      // in a helper returning Option
/// ```
///
/// These tests read the **real** 1.12 install, which is gitignored and not on every machine
/// (the contract: never commit Blizzard assets), so "no install" has always meant *pass without
/// asserting* rather than *fail*. That is deliberately unchanged; what changes is that the message
/// now names every path that was tried, so a machine where the tests silently do nothing says why.
#[macro_export]
macro_rules! wow_data_or_skip {
    () => {
        $crate::wow_data_or_skip!(())
    };
    ($ret:expr) => {
        match $crate::wow_data() {
            Some(data) => data,
            None => {
                eprintln!(
                    "skipping: no WoW install found — looked in {:?}",
                    $crate::candidates()
                );
                return $ret;
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No test in here touches the process environment — see [`candidates_from`] for why.
    fn probe(over: Option<&str>, exe: Option<&str>) -> Vec<PathBuf> {
        candidates_from(over.map(PathBuf::from), exe.map(PathBuf::from))
    }

    /// The override leads, and beside-the-binary always follows in both spellings a player will
    /// try — the release convention, and the only candidates a shipped binary has.
    #[test]
    fn the_ladder_is_override_then_project_folder_then_beside_the_binary() {
        let c = probe(Some("/opt/wow/Data"), Some("/games/benilla"));
        assert_eq!(c.first(), Some(&PathBuf::from("/opt/wow/Data")), "{c:?}");
        assert!(c.contains(&PathBuf::from("/games/benilla/Data")), "{c:?}");
        assert!(
            c.contains(&PathBuf::from("/games/benilla/WoW/Data")),
            "{c:?}"
        );

        // Both halves are optional and their absence must not shift the rest.
        assert_eq!(
            probe(None, Some("/games/benilla")).last(),
            Some(&PathBuf::from("/games/benilla/WoW/Data"))
        );
        assert!(!probe(Some("/opt/wow/Data"), None)
            .iter()
            .any(|p| p.starts_with("/games")));
    }

    /// A candidate only wins if it EXISTS, so a stale `$WOW_DATA` falls through to a real install
    /// rather than poisoning the run. Exercised through [`wow_data`]'s own filter.
    #[test]
    fn a_candidate_that_does_not_exist_is_never_chosen() {
        let tmp = std::env::temp_dir().join(format!("benilla-wd-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("Data")).unwrap();
        let ghost = tmp.join("nope");

        // Which real candidate wins depends on the build (a dev build has the project folder
        // ahead of the exe dir), so the assertion is the property, not the winner: never the
        // ghost, always something that is actually there.
        let chosen = candidates_from(Some(ghost.clone()), Some(tmp.clone()))
            .into_iter()
            .find(|c| c.is_dir());
        assert_ne!(
            chosen,
            Some(ghost),
            "a $WOW_DATA that does not exist must never be chosen"
        );
        assert!(
            chosen.is_some_and(|c| c.is_dir()),
            "a stale override must fall through to a real install"
        );

        std::fs::remove_dir_all(&tmp).ok();
    }

    /// `WOW_DATA=` — set and empty — is *there is no install*, and it outranks every other rung
    /// including the dev build's project folder. Without it the no-install path is unreachable on
    /// any machine that has an install, which is every machine this is developed on (1451).
    #[test]
    fn an_empty_override_means_there_is_no_install() {
        assert_eq!(
            probe(Some(""), Some("/games/benilla")),
            Vec::<PathBuf>::new(),
            "`WOW_DATA=` must leave the ladder with no rungs at all"
        );
    }

    /// **The falsifier for the whole record, as a unit test.** A player build — `dev` off — must
    /// not look anywhere inside the source tree that compiled it. If this fails, the project-folder
    /// candidate came back through feature unification and `strings <binary> | grep /Users` will
    /// find it too.
    #[cfg(not(feature = "dev"))]
    #[test]
    fn a_player_build_carries_no_source_tree_path() {
        let root = env!("CARGO_MANIFEST_DIR");
        for c in probe(None, Some("/games/benilla")) {
            assert!(
                !c.starts_with(root),
                "a player build looked inside the source tree at {}",
                c.display()
            );
        }
    }

    /// The dev twin: with the feature on, the project folder IS a candidate — otherwise every
    /// real-data test in the workspace silently turns into a skip and nobody notices.
    #[cfg(feature = "dev")]
    #[test]
    fn a_dev_build_looks_in_the_project_folder() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap();
        let c = probe(None, Some("/games/benilla"));
        assert!(
            c.contains(&root.join("WoW/Data")),
            "the dev build lost its project-folder candidate ({}): {c:?}",
            root.display()
        );
    }
}
