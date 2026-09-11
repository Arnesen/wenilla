//! **The control for the one-VM-per-addon bound** — the whole folder in ONE VM, the way a real
//! client runs it.
//!
//! The survey next door loads each addon into a VM of its own, with only its declared dependencies
//! underneath, and says so in its own header: *the headline is a floor, not an estimate*. That
//! bound is real and it is the largest single category left in the corpus's failures — a package
//! that does not ship `Tablet-2.0` and simply expects the library to be there, because in a folder
//! of 219 addons seventy of its neighbours ship a copy and Ace2's registry is one global table.
//!
//! **A bound that is stated and never measured is an excuse.** This is the measurement: one Lua
//! state, every addon in the folder walked in load order, dependencies first, each addon's
//! `ADDON_LOADED` at its own position — then the session start, once, for everybody. What comes
//! back is *which addons raise inside their own files when their neighbours are present*, and the
//! difference between that list and the survey's is exactly the size of the bound.
//!
//! ## What this is NOT
//!
//! Not a replacement for the survey and not a headline. It cannot attribute a *render* or a *use*
//! result (every addon's frames are in one world), it cannot tell "clean" from "never reached",
//! and one addon's runaway is everybody's. Attribution here is by the RAISING CHUNK and nothing
//! else — the same rule the survey applies to session errors, and the only one available when 219
//! addons share a state.
//!
//! ## It is run N times, and the reason is the whole finding
//!
//! **A big shared VM is not reproducible, and that is a property of Lua rather than a defect of
//! ours.** Lua 5.1 hashes a **table key by its POINTER** (`hashpointer`), so `pairs()` over a
//! registry keyed by objects — which is what every Ace2 library's registry is — walks in a
//! different order on every process, because ASLR moves the addresses. Measured directly rather
//! than deduced: twelve fresh tables as keys in one table, iterated, gave `3,7,11,1,…` then
//! `12,5,9,2,…` then `11,1,4,8,…` on three consecutive runs of the same binary.
//!
//! The reference's Lua does exactly the same thing, so this is a fact about the sessions we are
//! modelling and not something to fix. What it means for an INSTRUMENT is that a single run's
//! number cannot be quoted: three consecutive runs here disagreed by one addon and swapped three
//! rows. So the walk runs [`DEFAULT_RUNS`] times in fresh VMs and the verdict is per row:
//!
//!   - **raised in EVERY run** — a real failure with the neighbours present;
//!   - **raised in SOME** — order-sensitive, named as such and never folded into either count;
//!   - **clean in every run** — clean.
//!
//! The per-addon survey next door has been stable run to run (three rosters, byte-identical), and
//! the reason is scale rather than luck: one addon and its declared dependencies rarely build a
//! pointer-keyed registry big enough for the order to change an outcome. "Rarely" is not "never",
//! which is worth knowing about every `--diff` this arc has read as exact.
//!
//! **LoadOnDemand is ignored on purpose.** 62 of the corpus's manifests carry `## LoadOnDemand: 1`
//! and the boot walk skips every one (`0x51f600` loads only records whose LoadOnDemand byte is 0),
//! but the corpus's own addons then demand-load them: `FuBar.lua:1034`'s `LoadLoadOnDemandPlugins`
//! pulls in every installed `FuBar_*` the moment any one of them seats, and Auctioneer's stub does
//! the same for its family. A run that honoured the flag would answer for a session five minutes
//! shorter than the one anybody plays.

use std::collections::BTreeSet;
use std::path::Path;

use benilla_ui::script::UiScript;

use super::{
    corpus, load_addon_files, load_dependencies, manifest_path, LoadedDep, ADDON_INSTRUCTION_BUDGET,
};
use benilla_ui::toc::Toc;

/// How many times [`survey_together`] repeats the walk before it reports — see the module doc on
/// why one run is not quotable. Three is the smallest number that can tell "always" from
/// "sometimes"; each run is about twenty-five seconds.
pub const DEFAULT_RUNS: usize = 3;

/// One addon's verdict in the shared VM.
pub struct TogetherRow {
    pub name: String,
    /// Raises whose first stack frame is inside **this** addon's own folder, load-time and
    /// session-time together — the shared VM cannot separate the two windows per addon. From the
    /// FIRST run that produced any, so the text is a real traceback and not a merge of several.
    pub errors: Vec<String>,
    /// How many of the [`runs`](Self::runs) raised at all.
    pub raised_in: usize,
    pub runs: usize,
}

impl TogetherRow {
    /// Raised in every run — a failure the neighbours do not fix.
    pub fn always_raises(&self) -> bool {
        self.raised_in == self.runs
    }
    /// Raised in some runs and not others — the pointer-hash order showing through (module doc).
    pub fn order_sensitive(&self) -> bool {
        self.raised_in > 0 && self.raised_in < self.runs
    }
}

/// Load every addon under `root` into one VM and drive the session start — [`DEFAULT_RUNS`] times,
/// in a fresh VM each time, because one run's answer is not reproducible (module doc).
pub fn survey_together(root: &Path) -> Vec<TogetherRow> {
    let mut merged: Vec<TogetherRow> = Vec::new();
    for run in 0..DEFAULT_RUNS {
        let rows = survey_together_once(root);
        if run == 0 {
            merged = rows
                .into_iter()
                .map(|(name, errors)| TogetherRow {
                    name,
                    raised_in: usize::from(!errors.is_empty()),
                    errors,
                    runs: DEFAULT_RUNS,
                })
                .collect();
            continue;
        }
        for (name, errors) in rows {
            let Some(row) = merged.iter_mut().find(|r| r.name == name) else {
                continue;
            };
            if errors.is_empty() {
                continue;
            }
            row.raised_in += 1;
            if row.errors.is_empty() {
                row.errors = errors;
            }
        }
    }
    merged
}

/// One walk, one VM — the measurement [`survey_together`] repeats.
fn survey_together_once(root: &Path) -> Vec<(String, Vec<String>)> {
    let (names, installed, registry) = corpus(root);
    let Ok(mut script) = UiScript::new() else {
        return Vec::new();
    };
    script.set_screen_size(1024.0, 768.0);
    script.register_addons(registry, Some(root.to_path_buf()), None, None);
    super::seat_a_session(&mut script);
    let _ = crate::ui_script::load_default_ui(&script);

    // `seen` spans the WHOLE walk, not one addon: a library folder declared by eighty dependents
    // loads once, which is the property that makes this a session rather than eighty of them.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    // `(folder, its own load failures)` — kept beside the walk rather than read back out of the
    // VM, because the diagnostics log caps at 256 distinct rows and evicts, and 219 addons
    // overflow it. The survey has the same rule for the same reason.
    let mut load_errors: Vec<(String, Vec<String>)> = Vec::new();
    for name in &names {
        let Some(toc) = manifest_path(root, name)
            .and_then(|p| std::fs::read(p).ok())
            .map(|b| Toc::parse(&benilla_ui::source::decode(&b)))
        else {
            continue;
        };
        if !seen.insert(name.to_ascii_lowercase()) {
            continue; // already pulled in as somebody's dependency
        }
        // Re-armed PER ADDON, exactly as the live walk re-arms it (1306): without the reset one
        // runaway spends the whole allowance and every addon after it fails for somebody else's
        // loop.
        script.set_instruction_budget(ADDON_INSTRUCTION_BUDGET);
        let mut pulled: Vec<LoadedDep> = Vec::new();
        load_dependencies(&mut script, root, &toc, &installed, &mut seen, &mut pulled);
        // **A dependency's load failures are collected HERE and nowhere else.** In this VM a
        // library folder loads exactly once, under whichever dependent reached it first, so it
        // never gets a turn of its own in the loop below — and its failures would simply vanish.
        for dep in pulled {
            load_errors.push((dep.name, dep.files.errors));
        }
        let files = load_addon_files(&script, root, name, &toc);
        load_errors.push((name.clone(), files.errors));
        script.mark_addon_loaded(name);
        script.fire_event(
            "ADDON_LOADED",
            vec![benilla_ui::script::ScriptValue::Str(name.clone())],
        );
    }
    script.set_instruction_budget(ADDON_INSTRUCTION_BUDGET);
    for event in ["VARIABLES_LOADED", "PLAYER_LOGIN", "PLAYER_ENTERING_WORLD"] {
        script.fire_event(event, Vec::new());
    }
    for _ in 0..10 {
        script.tick(0.1);
    }

    // Attribution by the raising chunk. `\<Folder>\` rather than a prefix match, because Lua
    // truncates a long chunk name from the LEFT (`...Ons\FuBar_DakSmak\Libs\…`) and the prefix is
    // the half it eats.
    let mut rows: Vec<(String, Vec<String>)> =
        names.iter().map(|n| (n.clone(), Vec::new())).collect();
    // The LOAD half first, already attributed by whose manifest was being walked — the survey's
    // own `loaded` rule applies unchanged: a named file the package does not contain is not a
    // raise (2155), so it is not counted here either.
    for (folder, errs) in load_errors {
        let Some(row) = rows.iter_mut().find(|r| r.0 == folder) else {
            continue;
        };
        row.1
            .extend(errs.into_iter().filter(|e| !is_absent_file(e)));
    }
    for err in script.errors() {
        let Some(first) = err.lines().next().map(str::to_ascii_lowercase) else {
            continue;
        };
        // Longest folder name first, so `FuBar` never claims a raise inside `FuBar_AtlasFu`.
        let mut owner: Option<usize> = None;
        for (i, row) in rows.iter().enumerate() {
            let folder = row.0.to_ascii_lowercase();
            let hit = first.contains(&format!("\\{folder}\\"))
                || first.contains(&format!("{folder}/"))
                || first.starts_with(&format!("{folder}\\"));
            if hit && owner.is_none_or(|o| rows[o].0.len() < row.0.len()) {
                owner = Some(i);
            }
        }
        if let Some(i) = owner {
            rows[i].1.push(err);
        }
    }
    rows
}

/// A load failure that is only *a file the package does not contain* — the reference logs
/// `Couldn't open %s` and carries on (2155), so it is not a raise here either.
fn is_absent_file(err: &str) -> bool {
    err.ends_with(": not found")
        || err.contains("no provider hit for")
        || err.contains("; the whole document it names is missing")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The control answers the question it exists for**: an addon whose package is short a
    /// library its neighbour ships fails ALONE and is clean TOGETHER.
    ///
    /// Both halves are asserted, because either one alone would pass for the wrong reason — a
    /// control that finds everything clean is worthless, and so is one that finds nothing.
    #[test]
    fn a_library_a_neighbour_ships_is_there_in_the_shared_vm() {
        let tmp =
            std::env::temp_dir().join(format!("benilla-harness-together-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let write = |name: &str, toc: &str, file: &str, body: &str| {
            let dir = tmp.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{name}.toc")), toc).unwrap();
            std::fs::write(dir.join(file), body).unwrap();
        };
        // Loads FIRST (the walk is alphabetical) and puts the library in the one global state,
        // exactly as any of the seventy corpus packages that ship a copy of `Tablet-2.0` does.
        write(
            "AlphaShipsIt",
            "## Interface: 11200\nlib.lua\n",
            "lib.lua",
            "SharedLibrary = { greet = function() return 1 end }\n",
        );
        // Declares no dependency on it and does not ship it — the corpus's own shape.
        write(
            "ZuluWantsIt",
            "## Interface: 11200\nuse.lua\n",
            "use.lua",
            "ZuluSaw = SharedLibrary.greet()\n",
        );

        let alone = crate::addon_harness::survey(&tmp);
        let zulu = alone.iter().find(|r| r.name == "ZuluWantsIt").unwrap();
        assert!(
            !zulu.loaded,
            "ALONE it must fail — otherwise this control is measuring nothing: {:?}",
            zulu.errors
        );

        let rows = survey_together(&tmp);
        let of = |n: &str| rows.iter().find(|r| r.name == n).unwrap();
        assert_eq!(
            of("ZuluWantsIt").raised_in,
            0,
            "TOGETHER the neighbour's library is simply there, in every run: {:?}",
            of("ZuluWantsIt").errors
        );
        assert_eq!(
            of("AlphaShipsIt").raised_in,
            0,
            "and the provider is unaffected: {:?}",
            of("AlphaShipsIt").errors
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// ...and a raise is still ATTRIBUTED, rather than the shared VM turning everything green.
    #[test]
    fn a_raise_in_the_shared_vm_lands_on_the_folder_that_raised() {
        let tmp = std::env::temp_dir().join(format!(
            "benilla-harness-together-attrib-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        for (name, body) in [("Quiet", "QuietGlobal = 1\n"), ("Loud", "error('boom')\n")] {
            let dir = tmp.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(format!("{name}.toc")),
                "## Interface: 11200\nf.lua\n",
            )
            .unwrap();
            std::fs::write(dir.join("f.lua"), body).unwrap();
        }
        let rows = survey_together(&tmp);
        let of = |n: &str| rows.iter().find(|r| r.name == n).unwrap();
        assert!(
            of("Loud").always_raises() && of("Loud").errors.iter().any(|e| e.contains("boom")),
            "the raise is the loud one's, in every run: {:?}",
            of("Loud").errors
        );
        assert_eq!(
            of("Quiet").raised_in,
            0,
            "and not its neighbour's: {:?}",
            of("Quiet").errors
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
