//! **benilla's own interface** — its manifest, and the boot split that loads it in two phases.
//!
//! The *how* of loading any interface lives in [`super::addons`]: an [`Addon`] is a name, a parsed
//! `.toc`, and a source its files come from, and ours is simply the one whose source is the
//! compiled-in tree (decision 1184). What is specific to us, and therefore still here, is the
//! **seam at index 0** — see [`load_default_ui`].
//!
//! The manifest itself is not here either. It is [`MANIFEST`] — `assets/ui/benilla.toc`, an
//! ordinary addon manifest read by the ordinary `.toc` parser ([`benilla_ui::toc`]), exactly as a
//! third-party addon's is (decision 1178). Until then it was a hand-ordered `&[&str]` in this file,
//! which meant our own interface loaded by a private door and the addon path was untested by
//! construction.

use bevy::prelude::*;

use benilla_ui::script::UiScript;

use super::addons::Addon;

/// The built-in interface's manifest, relative to `assets/ui`. Its `## Interface:`/`## Title:`
/// directives are what `GetAddOnInfo` will read once the AddOn API lands (1178 step 4); nothing
/// consumes them yet.
pub(super) const MANIFEST: &str = "benilla.toc";

/// The built-in manifest's file list, in load order — a convenience over [`Addon::builtin`] for the
/// tests and the content sweep, which want the names rather than a loader.
#[cfg(test)]
pub(super) fn manifest_files() -> Vec<String> {
    Addon::builtin().toc.files
}

/// Run decision 0272's load-time `UIParent_ManageFramePositions()` pass.
///
/// Only meaningful once the frames that table names exist, so the font-registry-only load
/// ([`load_font_registry`]) skips it. It is defined in `UIParent.xml`, which is in the deferred
/// half; calling it after `Fonts.xml` alone is a nil-global error, not a no-op.
///
/// The ref applies `UIPARENT_MANAGED_FRAME_POSITIONS` once at load, then re-fires from the bottom
/// bars' OnShow/OnHide. Every frame the table names exists by the time this runs, so this is that
/// load-time application; the stance bar's show/hide handles the rest at runtime.
fn bootstrap_positions(script: &UiScript) -> Vec<String> {
    if let Err(e) = script.run("UIParent_ManageFramePositions()") {
        error!("ui_script: managed-positions bootstrap: {e}");
        return vec![format!("managed-positions bootstrap: {e}")];
    }
    Vec::new()
}

/// Load benilla's own default UI — every file [`MANIFEST`] names — through the engine-free loader.
/// This is our content (MIT/Apache), committed and **compiled into the binary**
/// ([`super::content`], decision 1175); a dev build still prefers the copy on disk, so editing a
/// FrameXML file costs no recompile. Textures (`Interface\…`) still resolve at render through the
/// MPQ `sprite_texture` path; the loader only needs the XML/Lua text.
///
/// Returns every loader error, tagged `"<Addon>/<file>: <error>"` — the app ignores the value (each
/// is already logged as it happens) and [`shipped_xml_tests`] asserts it empty. Before that
/// assertion a broken entry — a bad file name, a frame that collides with a later window's, a
/// template referenced before its definer — reached a real run with nothing but a log line. Capture
/// runs cannot cover it either: they skip this function entirely unless `WOW_CAPTURE_UI=1`.
///
/// **Split across the boot boundary (1051).** `Fonts.xml` — the manifest's first entry, zero frames
/// materialized — is the font-object registry the glyph atlas bakes its plan from, and our native
/// glue screens share that one atlas, so it must exist before the login screen. Everything after it
/// is in-game UI and loads at world entry ([`load_ingame_ui`]). This whole-manifest entry point
/// stays for the tests, which assert over the complete shipped set — production now loads in two
/// phases, so production has no caller for the whole-manifest form — the tests do, and so does
/// the addon harness ([`crate::addon_harness`]), which needs our entire interface under each
/// surveyed addon.
pub(crate) fn load_default_ui(script: &UiScript) -> Vec<String> {
    super::reference_ui::load_sourced(script);
    let builtin = Addon::builtin();
    let mut failures = builtin.load_files(script, &builtin.toc.files);
    failures.extend(bootstrap_positions(script));
    failures
}

/// The font-object registry alone (`Fonts.xml`), loaded at `Startup` — see [`load_default_ui`].
///
/// Verified lossless for the atlas: the full manifest and this file alone both yield the **same 19
/// distinct `(font, height, outline)` combinations**. The three font objects defined outside it
/// (`GameFontNormalMed1` 13, `OptionsFontHighlightMedium` 14, `OptionsFontHighlightHuge` 20) are
/// un-outlined and their heights are already declared here, so they add nothing to the bake plan.
pub(crate) fn load_font_registry(script: &UiScript) -> Vec<String> {
    let builtin = Addon::builtin();
    builtin.load_files(script, builtin.toc.files.get(..1).unwrap_or_default())
}

/// The in-game UI — everything after the font registry — loaded on entering the world, and then
/// **every third-party addon** ([`super::addons`], decision 1184).
///
/// The reference does the same at `CGGameUI::Initialize 0x48fbf0`, reached only from world entry
/// (`0x401570` ← `0x46c236`), and loads its addons from that same function (`0x4900a3` →
/// `0x51f600`); its glue screens run GlueXML with their own `GlueFonts.xml` registry, which is why
/// the reference has no equivalent of our shared-atlas coupling (wow-5875-re, 1051).
///
/// Addons load **after** the built-in interface, not interleaved with it: an addon may reference
/// our templates and globals (that is the point of 1178's seam), and nothing of ours may depend on
/// an addon.
///
/// `identity` is `(realm, character)`, which names this character's AddOn enable-state file — the
/// reference keys `AddOns.txt` per character too. `None` (no pick yet, a capture) means every
/// discovered addon is enabled, the same answer an absent file gives.
///
/// `version_check` is the persisted `checkAddonVersion` — the *Load out of date AddOns* toggle,
/// inverted — resolved by the caller because at load time this VM's own CVar table does not
/// exist yet (registration is a per-VM `Update` seed, decision 1291); the persisted value is the
/// truth the reference's live read would land on, since the session edge folds the dying VM's
/// table into it before any rebuild reads it.
pub(crate) fn load_ingame_ui(
    script: &mut UiScript,
    identity: Option<&(String, String)>,
    version_check: bool,
) -> Vec<String> {
    // The reference FrameXML this client SOURCES off the patch chain rather than transcribing
    // ([`super::reference_ui`], whose header is the rule). It runs FIRST, before our own files,
    // precisely so that every global we define ourselves overwrites the reference's — its
    // `ToggleBackpack` walks twelve `ContainerFrame`s we do not build; ours walks our windows.
    super::reference_ui::load_sourced(script);
    let builtin = Addon::builtin();
    let mut failures = builtin.load_files(script, builtin.toc.files.get(1..).unwrap_or_default());
    failures.extend(bootstrap_positions(script));
    // `&mut` from here down: each addon's `ADDON_LOADED` fires as that addon finishes, which is
    // the reference's own interleaving (`0x51f5ad`, per addon) rather than a batch at the end.
    failures.extend(super::addons::load_third_party(
        script,
        identity,
        version_check,
    ));
    failures
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
        let toc = Addon::builtin().toc;
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
    /// An entry naming a file we do not ship is a log line per entry and an empty screen (what
    /// `content::tests::every_manifest_entry_is_compiled_in` catches). The other direction is the
    /// one nothing caught before: a FrameXML file added to `assets/ui` and never listed here is
    /// simply never loaded, and the symptom is a window that does not exist rather than an error.
    #[test]
    fn the_manifest_lists_every_shipped_file_and_nothing_else() {
        let mut listed = manifest_files();
        let mut shipped: Vec<String> = super::super::content::shipped_files()
            .filter(|f| f.ends_with(".xml"))
            .map(str::to_owned)
            .collect();
        listed.sort();
        shipped.sort();
        assert_eq!(listed, shipped);
    }
}
