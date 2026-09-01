//! Guard for the one line that ships the pages' own scripts: `scripts/web-build.sh`'s `cp`.
//!
//! The two hosting pages (`web/index.html`, `crates/wenilla-realm/templates/play.html`) import
//! plain ES modules that live beside them in `web/` — `boot.js`, `wasi_stubs.js`, `platform.js`.
//! Nothing builds those: they are copied into `web/dist/` by hand-written filename, and
//! `docker/realm.Dockerfile` then ships that directory as `/app/www`. Add an import, forget the
//! `cp`, and the failure lands in production rather than in a build: the file 404s, and a
//! *static* import that fails to resolve stops the whole module — no credential fetch, no
//! `boot()`, a black page. (Both pages import `platform.js` dynamically for exactly that reason,
//! which softens the blow to "no full-screen button" — but the trap is one static import away.)
//!
//! So: every `./name.js` the pages ask for that exists in `web/` must appear in that `cp`.
//! Build outputs (`wenilla.js`, from wasm-bindgen) have no source file in `web/` and are skipped.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    let path = root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Every `./something.js` in a page: `import … from './x.js'`, `import('./x.js')`, and the
/// import map's `"./wasi_stubs.js"`. One scan for all three — the quoting is what they share.
fn referenced_scripts(page: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for quote in ['\'', '"'] {
        let needle = format!("{quote}./");
        let mut rest = page;
        while let Some(start) = rest.find(&needle) {
            rest = &rest[start + needle.len()..];
            if let Some(end) = rest.find(quote) {
                let name = &rest[..end];
                if name.ends_with(".js") && !name.contains('/') {
                    found.insert(name.to_string());
                }
            }
        }
    }
    found
}

#[test]
fn every_page_script_that_lives_in_web_is_copied_into_dist() {
    let build = read("scripts/web-build.sh");
    let cp = build
        .lines()
        .find(|l| l.trim_start().starts_with("cp web/"))
        .expect("web-build.sh still has the `cp web/…` line that populates web/dist");

    for page in ["web/index.html", "crates/wenilla-realm/templates/play.html"] {
        for name in referenced_scripts(&read(page)) {
            // Not every import has a source file: wenilla.js is wasm-bindgen's output, written
            // straight into dist by the build itself.
            if !root().join("web").join(&name).exists() {
                continue;
            }
            assert!(
                cp.contains(&format!("web/{name}")),
                "{page} imports ./{name} and web/{name} exists, but scripts/web-build.sh never \
                 copies it into web/dist — it would 404 in the built image.\n  cp line: {cp}"
            );
        }
    }
}

#[test]
fn the_pages_agree_on_the_platform_module() {
    // platform.js is best-effort by contract (see its header): a page must import it
    // dynamically, so a missing or broken module costs the button and never the boot.
    for page in ["web/index.html", "crates/wenilla-realm/templates/play.html"] {
        let text = read(page);
        assert!(
            text.contains("import('./platform.js')"),
            "{page} should import ./platform.js dynamically (not statically): it must not be \
             able to break the boot"
        );
        assert!(
            text.contains("id=\"fs\""),
            "{page} imports platform.js but has no #fs button for installFullscreenToggle"
        );
    }
}
