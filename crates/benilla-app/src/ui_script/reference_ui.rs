//! **The reference FrameXML this client EXECUTES off the player's own patch chain**, rather than
//! shipping a copy of it — the mechanism half of decision 1751.
//!
//! ## The rule
//!
//! The end state for the in-game interface is the stock 1.12 FrameXML, run off the file the player
//! already owns. `assets/ui` is scaffolding: it retires file by file, and a migrated window means
//! *its stock XML + Lua run off the chain and our counterpart file is deleted* (1751 §2). Fidelity
//! by construction — the reference's text cannot drift from itself, and every frame name, id,
//! template and stratum an addon reaches for is right because it **is** the reference's.
//!
//! What stays ours permanently: the glue screens (0068 §8 — GlueXML is a separate engine surface
//! even in the real client), dev-only frames, and adapter shims only while a genuine engine
//! difference forces one.
//!
//! ## Where the list lives — the manifest, not a second list in Rust
//!
//! Until 1751 this module carried its own `SOURCED` array and ran it *before* `assets/ui`, which
//! was the only ordering a Lua-only mechanism could express. Sourcing **XML** needs a real
//! position in the load order instead: stock `ContainerFrame.xml` inherits `ItemButtonTemplate`,
//! `CooldownFrameTemplate`, `SmallMoneyFrameTemplate` and `UIPanelCloseButton`, so it has to load
//! *after* the files that declare them.
//!
//! So there is exactly one ordered list of what loads and when, and it is `assets/ui/benilla.toc`
//! — the manifest that already had that job. **A manifest entry carrying a path separator is
//! sourced off the chain; a bare filename is a file we ship** ([`is_chain_entry`]). Our tree is
//! flat, so the two can never be confused, and the migration reads as what it is: the line
//! `BagFrame.xml` becomes `Interface\FrameXML\ContainerFrame.xml`, and `BagFrame.xml` is deleted.
//!
//! Everything else — the XML parse, `<Include>` / `<Script file=>` resolution against the
//! document's own directory, chunk naming, the error reporting that reaches the player — is
//! [`super::addons::Addon`]'s, unchanged. This module is only a third [`super::addons::Source`]:
//! *the player's install*.
//!
//! ## Where a reference file and our own UI still collide, and which one wins
//!
//! **Order decides, and the manifest is the order.** A name defined by both goes to whichever line
//! is later. That is the whole rule; there is no precedence machinery. A file sourced *before* our
//! own (`PaperDollFrame.lua`, which is there for one frame-agnostic button family and collides on
//! eighteen names we drive ourselves) has its collisions overwritten by ours; a file sourced at the
//! position of the window it replaces owns its names outright, which is what migrating a window
//! means.
//!
//! **Nothing is stubbed silently.** A reference body that reaches for something this client does
//! not have raises, naming it — which is loud, correct, and strictly better than a no-op that
//! pretends (1203, 1205, 1211, 1230). The answer is to build the verb, or to adapt the body in one
//! of our own files and say why at the site.
//!
//! ## No install, no file
//!
//! A machine with no client data (CI, a bare checkout) simply does not get these files, and says so
//! once, loudly. It is the same condition under which `GlobalStrings` is absent, and the addon
//! survey already prints which mode it ran in for that reason. An install-less checkout cannot run
//! most meaningful UI tests anyway — the art, fonts and MPQs come from the install too — so tests
//! that need these files gate on the install like every other client-data test.

use std::sync::OnceLock;

use benilla_formats::Chain;
use benilla_ui::toc::Toc;
use bevy::prelude::*;

use super::addons::{Addon, Source};

/// The addon name the reference's own files load under — the reference's word for its interface.
///
/// It is not `Interface\AddOns\…` anything: FrameXML is not an addon, gets no `ADDON_LOADED`, and
/// an addon that derives its folder from a `debugstack` pattern (`"\\AddOns\\(.*)\\"` —
/// `benilla_ui::script::addon_chunk_name`'s reason for existing) must not match a FrameXML frame.
/// [`Addon::chunk_name`] is what keeps that true: a chain file's chunk is named after its own
/// chain path, which is exactly what the real client names it.
pub(super) const NAME: &str = "FrameXML";

/// Is this manifest entry **sourced off the player's chain**, rather than shipped by us?
///
/// The test is a path separator, and it is decidable because our own shipped tree is *flat*: every
/// `assets/ui` entry is a bare filename, and every chain entry is a full internal path
/// (`Interface\FrameXML\ContainerFrame.xml`). `manifest::tests` pins both halves so the day
/// somebody adds a subdirectory to `assets/ui` is a failing test rather than a file that silently
/// stops loading.
pub(super) fn is_chain_entry(entry: &str) -> bool {
    entry.contains('\\') || entry.contains('/')
}

/// The reference interface as an [`Addon`] whose files come off the chain — the peer of
/// [`Addon::builtin`], and the thing [`super::manifest`] hands a manifest's chain entries to.
///
/// `files` are full chain paths, so the addon's prefix is empty and each entry is already in its
/// source's path space.
pub(super) fn addon(files: Vec<String>) -> Addon {
    Addon::new(
        NAME.to_string(),
        Toc {
            directives: Vec::new(),
            files,
        },
        Source::Chain,
    )
}

/// One file's bytes, read off the player's installed patch chain by internal path.
///
/// **Bytes, not text** (1193): a `.lua` chunk goes to Lua as it sits in the archive, and only an
/// XML parse decodes — a `read_to_string` here would not make a cp1252 file lose a glyph, it would
/// make the file *not exist*.
pub(super) fn read(req: &str) -> Option<Vec<u8>> {
    let chain = chain()?;
    match chain.read(req) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            debug!("ui_script: {req} is not in the patch chain: {e:#}");
            None
        }
    }
}

/// The player's patch chain, opened once per process and cached.
///
/// Cached because the addon survey stands up 218 VMs and this would otherwise be per-VM work. A
/// process-local chain rather than the one [`benilla_assets`] holds: the interface loads from
/// places that have no Bevy world to ask (the tests, the addon harness, a bare `UiScript`), and
/// `Chain`'s reads are `&self` and lock-free, so a second handle costs the mount and nothing else.
fn chain() -> Option<&'static Chain> {
    static CHAIN: OnceLock<Option<Chain>> = OnceLock::new();
    CHAIN
        .get_or_init(|| {
            let Some(data) = benilla_formats::wow_data() else {
                warn!(
                    "ui_script: no client data — every interface file this client SOURCES off the \
                     player's install (benilla.toc's `Interface\\…` entries) is absent, so the \
                     windows they build do not exist and addons that call their globals will raise"
                );
                return None;
            };
            match benilla_formats::open_chain(&data) {
                Ok(chain) => Some(chain),
                Err(e) => {
                    error!("ui_script: opening the patch chain to source the reference UI: {e:#}");
                    None
                }
            }
        })
        .as_ref()
}

#[cfg(test)]
mod tests {
    use benilla_ui::script::UiScript;

    /// **A chain entry really loads off the player's install, and order really decides.**
    ///
    /// The two halves of this module's rule, asserted rather than described, because nothing else
    /// would notice if either flipped: a chain entry that silently resolved to nothing would leave
    /// its globals nil (the failure mode is a window that does not exist, not an error), and the
    /// collision direction is invisible until an addon calls the wrong body.
    ///
    /// Skips without client data, like every other test that reads the install.
    #[test]
    fn a_chain_entry_loads_and_the_later_line_owns_the_collision() {
        let _data = benilla_formats::wow_data_or_skip!();
        let mut s = UiScript::new().expect("VM");
        s.set_screen_size(1024.0, 768.0);

        let failures = super::super::manifest::load_default_ui(&s);
        assert!(failures.is_empty(), "the default UI: {failures:#?}");

        // The item-button family the `ContainerFrame.lua` line is there for — nothing but the
        // sourced file defines these.
        for name in [
            "ContainerFrameItemButton_OnEnter",
            "ContainerFrameItemButton_OnClick",
            "ContainerFrameItemButton_OnLoad",
            "ContainerFrameItemButton_OnUpdate",
            "KeyRingItemButton_OnClick",
        ] {
            assert!(
                s.eval::<bool>(&format!("return type({name}) == \"function\""))
                    .unwrap(),
                "{name} must come from the sourced reference file"
            );
        }
        // …and its constants, which the corpus reads directly.
        assert_eq!(s.eval::<i64>("return NUM_BAG_FRAMES").unwrap(), 4);
        assert_eq!(s.eval::<i64>("return NUM_CONTAINER_FRAMES").unwrap(), 12);
        // The `PaperDollFrame.lua` line's own reason to exist.
        assert!(s
            .eval::<bool>("return type(PaperDollItemSlotButton_OnLoad) == \"function\"")
            .unwrap());

        // Order decides: `PaperDollFrame.lua` is sourced ABOVE CharacterFrame.xml, so OUR body of
        // a colliding name is the live one. `PaperDollFrame_SetLevel` is in the 18-name overlap.
        assert!(
            s.eval::<bool>(
                "return type(PaperDollFrame_SetLevel) == \"function\" \
                 and BenillaPaperDollSlot_OnLoad ~= nil"
            )
            .unwrap(),
            "our character sheet's own bodies must still be the live ones"
        );
        assert!(s.errors().is_empty(), "{:#?}", s.errors());
    }

    /// A path is a chain entry; a bare name is ours. The one-line rule the manifest rests on.
    #[test]
    fn a_separator_is_what_makes_an_entry_the_players_own_file() {
        assert!(super::is_chain_entry(
            "Interface\\FrameXML\\ContainerFrame.xml"
        ));
        assert!(super::is_chain_entry(
            "Interface/FrameXML/ContainerFrame.xml"
        ));
        assert!(!super::is_chain_entry("BagFrame.xml"));
        assert!(!super::is_chain_entry("Fonts.xml"));
    }
}
