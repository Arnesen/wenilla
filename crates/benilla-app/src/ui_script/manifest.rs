//! The shipped FrameXML manifest and the three ways it is loaded.
//!
//! Split out of `ui_script/mod.rs` as its own concern: the loader that walks an ordered manifest.
//! The interesting thing here is the **seam at index 0** — see [`load_default_ui`].
//!
//! The manifest itself is not here. It is [`MANIFEST`] — `assets/ui/benilla.toc`, an ordinary
//! addon manifest read by the ordinary `.toc` parser ([`benilla_ui::toc`]), exactly as a
//! third-party addon's will be (decision 1178). Until then it was a hand-ordered `&[&str]` in
//! this file, which meant our own interface loaded by a private door and the addon path was
//! untested by construction.

use bevy::prelude::*;

use benilla_ui::script::UiScript;
use benilla_ui::toc::Toc;

use super::content;

/// The built-in interface's manifest, relative to `assets/ui` — parsed for its ordered file list
/// by [`manifest_files`]. Its `## Interface:`/`## Title:` directives are what `GetAddOnInfo` will
/// read once the AddOn API lands (1178 step 4); nothing consumes them yet.
pub(super) const MANIFEST: &str = "benilla.toc";

/// The manifest's file list, in load order.
///
/// Re-read and re-parsed per call rather than cached: it is two calls per process (the font
/// registry at startup, the rest at world entry) over a 3 kB file, and a dev build prefers the
/// copy on disk ([`content`]) — so editing `benilla.toc` costs no recompile, same as editing any
/// FrameXML file it names.
///
/// A missing or empty manifest is an interface-less client, which is why it is `error!` and not a
/// silent empty list; [`tests::the_manifest_lists_every_shipped_file_and_nothing_else`] is what
/// stops it ever reaching a run.
pub(super) fn manifest_files() -> Vec<String> {
    let Some(text) = content::read(MANIFEST) else {
        error!("ui_script: {MANIFEST} is not in the shipped UI — no interface will load");
        return Vec::new();
    };
    let files = Toc::parse(&text).files;
    if files.is_empty() {
        error!("ui_script: {MANIFEST} lists no files — no interface will load");
    }
    files
}

/// Load a slice of the manifest into the VM, in the order given. Returns per-file errors.
///
/// `bootstrap_positions` runs decision 0272's load-time `UIParent_ManageFramePositions()` pass —
/// only meaningful once the frames that table names exist, so the font-registry-only load
/// ([`load_font_registry`]) skips it. It is defined in `UIParent.xml`, which is in the deferred
/// half; calling it after `Fonts.xml` alone is a nil-global error, not a no-op.
fn load_ui_files(script: &UiScript, files: &[String], bootstrap_positions: bool) -> Vec<String> {
    let mut failures = Vec::new();
    // Provider for FrameXML/Lua references: try the path as given and by basename (Blizzard-style
    // backslash paths, dir-relative). The *content* comes from `super::content` — the shipped tree
    // is compiled into the binary (1175), so this resolves on a machine that has never seen our
    // source; the two-try shape is a property of FrameXML references and stays here.
    let provider = |req: &str| -> Option<String> {
        let norm = req.replace('\\', "/");
        let base = norm.rsplit('/').next().unwrap_or(&norm);
        content::read(&norm).or_else(|| content::read(base))
    };
    for file in files {
        let text = match content::read(file) {
            Some(t) => t,
            None => {
                error!("ui_script: {file} is not in the shipped UI");
                continue;
            }
        };
        let doc = match benilla_ui::framexml::parse(&text) {
            Ok(d) => d,
            Err(e) => {
                error!("ui_script: parsing {file}: {e}");
                continue;
            }
        };
        let report = benilla_ui::loader::load(script, &doc, &provider);
        for w in &report.warnings {
            warn!("ui_script({file}): {w}");
        }
        for e in &report.errors {
            error!("ui_script({file}): {e}");
            failures.push(format!("{file}: {e}"));
        }
        info!(
            "ui_script: {file} loaded ({} frames materialized)",
            report.frames
        );
    }

    // The managed positions' startup pass (decision 0272): the ref applies
    // UIPARENT_MANAGED_FRAME_POSITIONS once at load, then re-fires from the bottom bars'
    // OnShow/OnHide. Every frame the table names now exists, so this is that load-time
    // application; the stance bar's show/hide handles the rest at runtime.
    if bootstrap_positions {
        if let Err(e) = script.run("UIParent_ManageFramePositions()") {
            error!("ui_script: managed-positions bootstrap: {e}");
            failures.push(format!("managed-positions bootstrap: {e}"));
        }
    }
    failures
}

/// Load benilla's own default UI — every file [`MANIFEST`] names — through the engine-free loader,
/// resolving any `<Include>`/`<Script file=>` references against the same content store. This is
/// our content (MIT/Apache), committed and **compiled into the binary** ([`super::content`],
/// decision 1175) — a dev build still prefers the copy on disk, so editing a FrameXML file costs no
/// recompile. Textures (`Interface\…`) still resolve at render through the MPQ `sprite_texture`
/// path; the loader only needs the XML/Lua text.
///
/// Returns every loader error, tagged `"<file>: <error>"` — the app ignores the value (each is
/// already logged as it happens) and [`shipped_xml_tests`] asserts it empty. Before that assertion
/// a broken entry — a bad file name, a frame that collides with a later window's, a template
/// referenced before its definer — reached a real run with nothing but a log line. Capture runs
/// cannot cover it either: they skip this function entirely unless `WOW_CAPTURE_UI=1`.
///
/// **Split across the boot boundary (1051).** `Fonts.xml` — the manifest's first entry, zero frames
/// materialized — is the font-object registry the glyph atlas bakes its plan from, and our native
/// glue screens share that one atlas, so it must exist before the login screen. Everything after it
/// is in-game UI and loads at world entry ([`load_ingame_ui`]). This whole-manifest entry point
/// stays for the tests, which assert over the complete shipped set — production now loads in two
/// phases, so this has no non-test caller.
#[cfg(test)]
pub(crate) fn load_default_ui(script: &UiScript) -> Vec<String> {
    load_ui_files(script, &manifest_files(), true)
}

/// The font-object registry alone (`Fonts.xml`), loaded at `Startup` — see [`load_default_ui`].
///
/// Verified lossless for the atlas: the full manifest and this file alone both yield the **same 19
/// distinct `(font, height, outline)` combinations**. The three font objects defined outside it
/// (`GameFontNormalMed1` 13, `OptionsFontHighlightMedium` 14, `OptionsFontHighlightHuge` 20) are
/// un-outlined and their heights are already declared here, so they add nothing to the bake plan.
pub(crate) fn load_font_registry(script: &UiScript) -> Vec<String> {
    let files = manifest_files();
    load_ui_files(script, files.get(..1).unwrap_or_default(), false)
}

/// The in-game UI — everything after the font registry — loaded on entering the world.
///
/// The reference does the same at `CGGameUI::Initialize 0x48fbf0`, reached only from world entry
/// (`0x401570` ← `0x46c236`); its glue screens run GlueXML with their own `GlueFonts.xml` registry,
/// which is why the reference has no equivalent of our shared-atlas coupling (wow-5875-re, 1051).
pub(crate) fn load_ingame_ui(script: &UiScript) -> Vec<String> {
    let files = manifest_files();
    load_ui_files(script, files.get(1..).unwrap_or_default(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The manifest parses as a `.toc`, declares the build it targets, and splits where the loader
    /// splits it. `Fonts.xml` first is not tidiness: [`load_font_registry`] takes entry 0 and
    /// [`load_ingame_ui`] takes the rest, so a reordering here silently moves a real file across
    /// the boot boundary (1051) — into the glue screens' phase, or out of the atlas bake plan.
    #[test]
    fn the_manifest_is_a_toc_that_starts_with_the_font_registry() {
        let toc = Toc::parse(&content::read(MANIFEST).expect("benilla.toc is shipped"));
        assert_eq!(toc.interface_versions(), vec![11200]);
        assert_eq!(toc.directive("Title"), Some("benilla"));
        assert_eq!(
            toc.files.first().map(String::as_str),
            Some("Fonts.xml"),
            "the font registry is the manifest's first entry — the loader splits there"
        );
    }

    /// The manifest and `assets/ui` describe the same interface, both ways.
    ///
    /// An entry naming a file we do not ship is 61 log lines and an empty screen (what
    /// `content::tests::every_manifest_entry_is_compiled_in` catches). The other direction is the
    /// one nothing caught before: a FrameXML file added to `assets/ui` and never listed here is
    /// simply never loaded, and the symptom is a window that does not exist rather than an error.
    #[test]
    fn the_manifest_lists_every_shipped_file_and_nothing_else() {
        let mut listed = manifest_files();
        let mut shipped: Vec<String> = content::shipped_files()
            .filter(|f| f.ends_with(".xml"))
            .map(str::to_owned)
            .collect();
        listed.sort();
        shipped.sort();
        assert_eq!(listed, shipped);
    }
}
