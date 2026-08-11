//! **Reference FrameXML this client EXECUTES off the patch chain, rather than transcribing.**
//!
//! ## The rule, and why it is a rule
//!
//! Some of `Interface\FrameXML` is not *our* logic to re-derive — it is a library of globals that
//! every 1.12 addon calls, or replaces, by name. `ContainerFrameItemButton_OnEnter` is one: Bagnon
//! builds its own item buttons and then hands them to the client's own handler, and four other
//! corpus addons overwrite that global outright. Those bodies do not encode anything benilla wants
//! to decide differently; they encode *the contract addons were written against*.
//!
//! Two ways to have them, and only one is available to us:
//!
//! | | |
//! |---|---|
//! | transcribe the file into `assets/ui` | **forbidden.** Blizzard's Lua, committed to a repo that mirrors public under MIT/Apache. the contract: never commit Blizzard assets. |
//! | read it out of the player's own installed patch chain at runtime | what we do. Nothing is committed; a player runs the file they already own. |
//!
//! This is not a new mechanism — it is [`super::load_global_strings`]'s, generalised. That already
//! executes the real `GlobalStrings.lua` off the chain at boot for exactly the same reason
//! (~5,000 localized globals nobody should retype), and this module is that pattern with a list.
//!
//! ## Where the reference file and our own UI collide, and which one wins
//!
//! `ContainerFrame.lua` is not only handlers. Two thirds of it drives the reference's OWN twelve
//! `ContainerFrame1..12` windows, which benilla does not build — our bag UI is `BagFrame.xml`
//! (decision 0068 T2). So sourcing the file wholesale would put reference bodies on top of working
//! benilla ones. **The rule, applied consistently and stated here once:**
//!
//! 1. **Order decides.** The sourced reference file runs **before** our own `assets/ui`, so every
//!    global we define ourselves — `ToggleBag`, `ToggleBackpack`, `OpenAllBags`, `OpenBag`,
//!    `CloseBag`, `IsBagOpen`, `CloseAllBags`, `OpenBackpack`, `CloseBackpack`, `ToggleKeyRing`,
//!    `PutKeyInKeyRing`, `GetKeyRingSize` — **overwrites** the reference's. Ours drive our windows.
//! 2. **What only the reference defines, we keep, unmodified.** The item-button family
//!    (`ContainerFrameItemButton_OnLoad`/`_OnClick`/`_OnEnter`/`_OnUpdate`, `KeyRingItemButton_OnClick`)
//!    is frame-agnostic: every one of them works on whatever button the caller passes, through the
//!    contract "the button's own ID is the slot, its parent's ID is the bag". Bagnon's buttons obey
//!    it. So does anything else written for the real client. It also brings the file's constants
//!    (`NUM_BAG_FRAMES`, read by 8 corpus addons; `NUM_CONTAINER_FRAMES` by 5) for free.
//! 3. **Where a reference body needs something benilla genuinely does not have, we ADAPT it in our
//!    own file** — `assets/ui/ContainerFrameAdapters.xml`, which loads after this and wraps the
//!    global. Exactly one function needs that today; the file names it and says why.
//! 4. **Nothing is stubbed silently.** The reference's frame-driven functions
//!    (`ContainerFrame_GetOpenFrame`, `updateContainerFrameAnchors`, `ContainerFrame_OnLoad` and
//!    kin) are left as the reference wrote them. Driven against frames that do not exist they raise
//!    — naming the frame — which is loud, correct, and strictly better than a no-op that pretends
//!    (1203, 1205, 1211, 1230 are four records of exactly that pretending). `ContainerFrame_Update`
//!    and `ContainerFrame_GenerateFrame` are NOT in that set: both take the frame as an argument
//!    and touch nothing global, so an addon that passes its own conforming frame gets the real
//!    behaviour.
//!
//! ## No install, no file
//!
//! A machine with no client data (CI, a bare checkout) simply does not get these globals, and says
//! so once, loudly. Every consumer already had to survive that: it is the same condition under
//! which `GlobalStrings` is absent, and the addon survey prints which mode it ran in for that
//! reason. Tests that need the file gate on the install the way every other client-data test does.

use benilla_ui::script::UiScript;
use bevy::prelude::*;

/// The reference FrameXML files benilla sources instead of transcribing, in load order.
///
/// **Adding to this list is a decision, not a convenience.** A file earns a place when its globals
/// are a *contract addons call by name* AND benilla has no reason to compute them differently.
/// Anything we want to own — because our own windows are the consumer — belongs in `assets/ui` as
/// our own code, under the reference's names (`BagFrame.xml`'s `ToggleBackpack` is the pattern).
pub(crate) const SOURCED: &[&str] = &["Interface\\FrameXML\\ContainerFrame.lua"];

/// Execute every [`SOURCED`] file into `script`, read off the install's patch chain.
///
/// Returns whether the chain was available at all. Read **once per process** and cached: the
/// addon survey stands up 218 VMs, and this is per-VM work otherwise.
pub(crate) fn load_sourced(script: &UiScript) -> bool {
    let Some(files) = sources() else {
        warn!(
            "ui_script: no client data — the reference FrameXML this client sources ({}) is \
             absent, so its globals are nil; addons that call them will raise",
            SOURCED.join(", ")
        );
        return false;
    };
    for (path, src) in files {
        if let Err(e) = script.run_chunk_named(src.as_bytes(), path) {
            error!("ui_script: sourced reference file {path} failed to run: {e}");
        }
    }
    true
}

/// The [`SOURCED`] files' text, or `None` with no install to read them from.
///
/// A file listed here and missing from the chain is an `error!`, not a silent skip: the list is
/// short and deliberate, so a miss means the chain is not what we think it is.
fn sources() -> Option<&'static [(&'static str, String)]> {
    use std::sync::OnceLock;
    static SRC: OnceLock<Option<Vec<(&'static str, String)>>> = OnceLock::new();
    SRC.get_or_init(|| {
        let data = benilla_formats::wow_data()?;
        let mut chain = benilla_formats::open_chain(&data).ok()?;
        let mut out = Vec::new();
        for &path in SOURCED {
            match chain.read_file(path) {
                Ok(bytes) => out.push((path, benilla_ui::source::decode(&bytes).into_owned())),
                Err(e) => {
                    error!("ui_script: sourced reference file {path} not in the chain: {e:#}")
                }
            }
        }
        Some(out)
    })
    .as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The item-button family arrives, and the bag verbs stay OURS** — rule 1 and rule 2 of this
    /// module's header, asserted rather than described, because the order between the two is the
    /// whole design and nothing else would notice if it flipped.
    ///
    /// Skips without client data, like every other test that reads the install.
    #[test]
    fn sourcing_the_reference_file_adds_its_handlers_without_taking_our_bag_verbs() {
        let _data = benilla_formats::wow_data_or_skip!();
        let mut s = UiScript::new().expect("VM");
        s.set_screen_size(1024.0, 768.0);

        assert!(load_sourced(&s), "the chain opened");
        // Rule 2: what only the reference defines is now here.
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
        // …including its constants, which the corpus reads directly.
        assert_eq!(s.eval::<i64>("return NUM_BAG_FRAMES").unwrap(), 4);
        assert_eq!(s.eval::<i64>("return NUM_CONTAINER_FRAMES").unwrap(), 12);

        // Rule 1: the reference's own bag verbs are here for the moment…
        assert!(s
            .eval::<bool>("return type(ToggleBackpack) == \"function\"")
            .unwrap());
        // …and our interface, loading after, takes them over. `BENILLA_BAG_WINDOWS` is the tell:
        // it is the table OUR ToggleBackpack walks, and the reference's body cannot mention it.
        let failures = super::super::load_default_ui(&s);
        assert!(failures.is_empty(), "our own FrameXML: {failures:#?}");
        assert!(
            s.eval::<bool>(
                "local f = ToggleBackpack \
                 for _, name in ipairs(BENILLA_BAG_WINDOWS) do end \
                 return BenillaBagFrame ~= nil"
            )
            .unwrap(),
            "our bag windows exist"
        );
        s.run("ToggleBackpack()")
            .expect("ours runs — the reference's body would die on a nil ContainerFrame1");
        assert!(
            s.eval::<bool>("return BenillaBagFrame:IsShown()").unwrap(),
            "ToggleBackpack must be OURS after our files load, not the reference's"
        );
        assert!(s.errors().is_empty(), "{:#?}", s.errors());
    }
}
