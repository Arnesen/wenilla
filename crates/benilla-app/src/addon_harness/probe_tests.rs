//! **The probe's can-it-fail proof**, plus the one property that makes its answers worth
//! anything: the VM it reads is the corpus's, not the selection's.
//!
//! A debugger that quietly answers a different question from the report it exists to explain is
//! worse than no debugger, because every conclusion drawn from it looks sourced.

use std::path::{Path, PathBuf};

use super::probe::probe;

/// One throwaway AddOns root, cleaned up on drop even if a test panics.
struct Fixtures(PathBuf);

impl Fixtures {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "benilla-probe-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn addon(&self, name: &str, body: &str) -> &Self {
        let dir = self.0.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{name}.toc")),
            "## Interface: 11200\nbody.lua\n",
        )
        .unwrap();
        std::fs::write(dir.join("body.lua"), body).unwrap();
        self
    }

    fn root(&self) -> &Path {
        &self.0
    }
}

impl Drop for Fixtures {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// **The probe reads AFTER the session start, and a raise is a value, not a stop.**
///
/// Three claims in one addon. A global written at file scope is readable (the load ran); a global
/// written only from a `PLAYER_LOGIN` handler is readable too (**the session start ran** — this is
/// the claim that separates this from a bare load, and the moment the session column's own errors
/// come from); and an eval that raises comes back tagged `ERROR:` **without stopping the eval after
/// it**, because a debugging session that dies on its own first typo is a debugging session spent
/// re-running the command.
#[test]
fn the_probe_reads_the_vm_after_the_session_start() {
    let fx = Fixtures::new("after");
    fx.addon(
        "Talker",
        r#"
        TalkerFileScope = "loaded"
        local f = CreateFrame("Frame")
        f:RegisterEvent("PLAYER_LOGIN")
        f:SetScript("OnEvent", function() TalkerAtLogin = "logged in" end)
    "#,
    );

    let out = probe(
        fx.root(),
        "Talker",
        &[
            "return TalkerFileScope".to_string(),
            "return TalkerAtLogin".to_string(),
            "return nil + 1".to_string(),
            "return TalkerFileScope".to_string(),
        ],
    )
    .expect("the fixture has a manifest");

    assert!(out.load_errors.is_empty(), "{:?}", out.load_errors);
    assert!(out.session_errors.is_empty(), "{:?}", out.session_errors);
    let answers: Vec<&str> = out.answers.iter().map(|(_, a)| a.as_str()).collect();
    assert_eq!(answers[0], "= loaded", "the file scope ran");
    assert_eq!(
        answers[1], "= logged in",
        "the PLAYER_LOGIN handler ran BEFORE the read — a load-only probe would answer nil"
    );
    assert!(
        answers[2].starts_with("ERROR:"),
        "a raise is reported, not propagated: {}",
        answers[2]
    );
    assert_eq!(
        answers[3], "= loaded",
        "...and the eval after a raise still ran"
    );
}

/// **The environment is the whole folder's, never the probed addon's.**
///
/// `GetAddOnInfo` is how AceAddon and AceLibrary — the two most replicated files in the corpus —
/// find their dependencies, so a probe built against a registry of one would send half the
/// ecosystem down its "nothing is installed" path and answer a different question from the survey
/// row it was opened to explain. The sibling here is never surveyed and never loaded; it must
/// still be *installed*.
#[test]
fn the_probe_sees_the_whole_folder_installed() {
    let fx = Fixtures::new("registry");
    fx.addon("Asker", "AskerSaw = GetAddOnInfo(\"Sibling\")\n");
    fx.addon("Sibling", "SiblingRan = 1\n");

    let out = probe(
        fx.root(),
        "Asker",
        &[
            "return AskerSaw".to_string(),
            "return GetNumAddOns()".to_string(),
            "return SiblingRan".to_string(),
        ],
    )
    .expect("the fixture has a manifest");

    assert!(out.load_errors.is_empty(), "{:?}", out.load_errors);
    assert_eq!(
        out.answers[0].1, "= Sibling",
        "the sibling must be INSTALLED in the probed VM"
    );
    assert_eq!(
        out.answers[1].1, "= 2",
        "the registry is the folder's, not the selection's"
    );
    // ...and installed is not loaded: one addon per VM is the survey's own isolation rule, and a
    // probe that silently ran the neighbours would attribute their globals to the addon in hand.
    assert_eq!(
        out.answers[2].1, "= nil",
        "an installed sibling must NOT have been run"
    );
}

/// A folder with no manifest is `None`, not an empty outcome — the same refusal the survey makes
/// by filtering, said out loud so the caller can tell "no such addon" from "that addon is silent".
#[test]
fn a_folder_without_a_manifest_is_refused() {
    let fx = Fixtures::new("nomanifest");
    std::fs::create_dir_all(fx.root().join("Backup")).unwrap();
    assert!(probe(fx.root(), "Backup", &[]).is_none());
    assert!(probe(fx.root(), "NotThereAtAll", &[]).is_none());
}
