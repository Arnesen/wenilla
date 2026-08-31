//! **The in-game interface** — its manifest, and the boot split that loads it in two phases.
//!
//! The *how* of loading any interface lives in [`super::addons`]: an [`Addon`] is a name, a parsed
//! `.toc`, and a source its files come from (decision 1184). What is specific to the default UI,
//! and therefore still here, is the **seam at index 0** — see [`load_default_ui`] — and the
//! **two sources one manifest names**, see [`load_manifest`].
//!
//! The manifest itself is not here either. It is [`MANIFEST`] — `assets/ui/benilla.toc`, an
//! ordinary addon manifest read by the ordinary `.toc` parser ([`benilla_ui::toc`]), exactly as a
//! third-party addon's is (decision 1178). Until then it was a hand-ordered `&[&str]` in this file,
//! which meant our own interface loaded by a private door and the addon path was untested by
//! construction.
//!
//! ## One ordered list, two stores (decision 1751)
//!
//! The end state for this interface is the stock 1.12 FrameXML run off the player's own install;
//! `assets/ui` is scaffolding that retires file by file ([`super::reference_ui`], whose header is
//! the rule). A manifest entry carrying a **path** is sourced off the chain, a **bare filename** is
//! one we ship — so the migration of a window is one line changing in `benilla.toc` plus the
//! deletion of our copy, and the manifest stays the single ordered truth of what loads when. That
//! ordering is the point: stock `ContainerFrame.xml` inherits four templates our earlier files
//! declare, so "source the reference first" — the only order a Lua-only mechanism could express —
//! is not a position it can load at.

use bevy::prelude::*;

use benilla_ui::script::UiScript;

use super::addons::Addon;
use super::reference_ui;

/// The built-in interface's manifest, relative to `assets/ui`. Its `## Interface:`/`## Title:`
/// directives are what `GetAddOnInfo` will read once the AddOn API lands (1178 step 4); nothing
/// consumes them yet.
pub(super) const MANIFEST: &str = "benilla.toc";

/// The manifest's file list, in load order — a convenience over [`Addon::builtin`] for the tests
/// and the content sweep, which want the names rather than a loader. **Both stores**: filter with
/// [`reference_ui::is_chain_entry`] for one or the other.
#[cfg(test)]
pub(super) fn manifest_files() -> Vec<String> {
    Addon::builtin().toc.files
}

/// The manifest's entries that name a file **we ship** — everything [`reference_ui`] does not
/// source off the player's install. This is what a check about `assets/ui` wants.
#[cfg(test)]
pub(super) fn shipped_manifest_files() -> Vec<String> {
    manifest_files()
        .into_iter()
        .filter(|f| !reference_ui::is_chain_entry(f))
        .collect()
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
pub(super) fn bootstrap_positions(script: &UiScript) -> Vec<String> {
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
    let mut failures = load_manifest(script, &Addon::builtin().toc.files);
    failures.extend(bootstrap_positions(script));
    failures
}

/// Load a slice of [`MANIFEST`] entries, **each from its own store** (decision 1751).
///
/// A bare filename is a file we ship and comes from [`Addon::builtin`]; a path is the reference's
/// own file and comes off the player's installed chain ([`reference_ui`]). The dispatch is
/// per-entry rather than per-run so the manifest's order is the load order verbatim — the whole
/// reason the list is a manifest and not two lists.
///
/// Both stores answer `<Include>` / `<Script file=>` the same way: the loader resolves a reference
/// against the *including document's own* directory in its source's path space (1186), which for a
/// chain document means `Interface\FrameXML\ContainerFrame.xml`'s `<Script
/// file="ContainerFrame.lua"/>` reaches `Interface\FrameXML\ContainerFrame.lua` without anything
/// here knowing about it.
pub(super) fn load_manifest(script: &UiScript, files: &[String]) -> Vec<String> {
    let builtin = Addon::builtin();
    let reference = reference_ui::addon(
        files
            .iter()
            .filter(|f| reference_ui::is_chain_entry(f))
            .cloned()
            .collect(),
    );
    let mut failures = Vec::new();
    for file in files {
        let from = if reference_ui::is_chain_entry(file) {
            &reference
        } else {
            &builtin
        };
        failures.extend(from.load_files(script, std::slice::from_ref(file)));
    }
    failures
}

/// The font-object registry alone (`Fonts.xml`), loaded at `Startup` — see [`load_default_ui`].
///
/// Verified lossless for the atlas: the full manifest and this file alone both yield the **same 19
/// distinct `(font, height, outline)` combinations**. The three font objects defined outside it
/// (`GameFontNormalMed1` 13, `OptionsFontHighlightMedium` 14, `OptionsFontHighlightHuge` 20) are
/// un-outlined and their heights are already declared here, so they add nothing to the bake plan.
pub(crate) fn load_font_registry(script: &UiScript) -> Vec<String> {
    load_manifest(
        script,
        Addon::builtin().toc.files.get(..1).unwrap_or_default(),
    )
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
    // The whole load edge runs bounded (decision 1306): the reference files sourced off the
    // player's own chain, our builtin, and every addon (which re-arms per addon in
    // `load_third_party`). A chunk that never returns fails as a load error instead of freezing
    // the client on the loading screen; the caller disarms once the edge is done
    // (`lifecycle::load_ingame_ui_on_world_entry`), so the session's steady state runs unhooked.
    script.set_instruction_budget(super::addons::LOAD_INSTRUCTION_BUDGET);
    let mut failures = load_manifest(
        script,
        Addon::builtin().toc.files.get(1..).unwrap_or_default(),
    );
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
        let mut listed = shipped_manifest_files();
        let mut shipped: Vec<String> = super::super::content::shipped_files()
            .filter(|f| f.ends_with(".xml"))
            .map(str::to_owned)
            .collect();
        listed.sort();
        shipped.sort();
        assert_eq!(listed, shipped);
    }

    /// **The shipped tree is FLAT, and that is what makes the two stores tellable apart.**
    ///
    /// [`reference_ui::is_chain_entry`] decides where a manifest entry comes from by asking
    /// whether it carries a path separator (1751). That is only decidable while every file we
    /// ship is a bare name — the day somebody adds `assets/ui/templates/Foo.xml`, its manifest
    /// entry would be read as a chain path, and the symptom would be a window that silently does
    /// not exist rather than an error. This is that day's failing test.
    #[test]
    fn every_file_we_ship_is_a_bare_name_so_a_path_can_only_mean_the_chain() {
        for name in super::super::content::shipped_files() {
            assert!(
                !reference_ui::is_chain_entry(name),
                "assets/ui is flat by construction, but ships {name} — a manifest entry with a                  separator is read as a file to source off the player's install"
            );
        }
    }

    /// Every `Interface\…` entry the manifest names is really in the 1.12 chain, and is really
    /// **not** something we also ship under that basename.
    ///
    /// Skips without client data, like every other test that reads the install.
    #[test]
    fn every_chain_entry_resolves_off_the_players_install() {
        let _data = benilla_formats::wow_data_or_skip!();
        let chain: Vec<String> = manifest_files()
            .into_iter()
            .filter(|f| reference_ui::is_chain_entry(f))
            .collect();
        for entry in &chain {
            assert!(
                reference_ui::read(entry).is_some(),
                "benilla.toc sources {entry} off the patch chain, which does not hold it"
            );
        }
    }

    /// **Nothing may declare `parent="UIParent"` before `UIParent.xml` has loaded.**
    ///
    /// A parent name is resolved at LOAD, and a name that does not exist yet is not an error: the
    /// loader warns and silently falls back to the enclosing frame. So this ordering mistake does
    /// not fail, it *half-works* — the frame keeps drawing, keeps answering `IsShown`, and simply
    /// never joins the cascade `UIParent:Hide()` walks. That is precisely how it would be missed.
    ///
    /// It nearly was: `UIParent.xml` sat below `UiPanels.xml` until decision 1734, so restoring
    /// `StaticPopup1`/`StaticPopup2`'s parents there would have written two declarations that did
    /// nothing at all. The reference's own order is the fix (FrameXML.toc: BasicControls.xml l.6,
    /// UIParent.xml l.8), and this keeps it.
    #[test]
    fn nothing_declares_a_uiparent_child_before_uiparent_itself_loads() {
        let files = manifest_files();
        let at = files
            .iter()
            .position(|f| f == "UIParent.xml")
            .expect("the manifest lists UIParent.xml");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ui");
        for early in &files[..at] {
            // A chain entry is the player's file, not one on this machine — and the reference's
            // own load order is what this test exists to preserve, so it cannot be the thing that
            // violates it. `every_chain_entry_resolves_off_the_players_install` covers those.
            if reference_ui::is_chain_entry(early) {
                continue;
            }
            let text = std::fs::read_to_string(dir.join(early)).unwrap();
            assert!(
                !text.contains(r#"parent="UIParent""#),
                "{early} loads before UIParent.xml (position {at}) but declares a \
                 UIParent child — the loader would warn and silently drop it. Move \
                 UIParent.xml up, or the declaration down."
            );
        }
    }
}
