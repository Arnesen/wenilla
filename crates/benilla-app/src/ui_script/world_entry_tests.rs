//! **"The selected one is always Onewarrior no matter what char I log into."** — the director's
//! character-switch report, reproduced at the edge that causes it.
//!
//! ## Why this file exists
//!
//! The symptom arrived as a dropdown bug: Bagnon's character menu never moved its checkmark off
//! whoever the session started as. It is not a dropdown bug and it is not Bagnon's. Every addon on
//! the machine reads the live character **once, at file scope** — `local currentPlayer =
//! UnitName("player")` is the corpus idiom, not an idiosyncrasy — and until this landed the file
//! scope ran exactly once per **process**. Logging out to the character screen and back in kept the
//! same VM, so nothing re-read it.
//!
//! The tests here drive the two real edges — [`super::load_ingame_ui_on_world_entry`] and
//! [`super::end_ui_session`] — over a planted addon that captures the name the same way, and assert
//! the second login sees the second character. Reverting the rebuild makes
//! [`the_second_login_runs_addon_file_scope_under_the_second_character`] report `Onehunter`, which
//! is the director's screenshot in one string.
//!
//! Nothing here needs the client install or the addon corpus: the probe addon is written into a
//! hermetic `BENILLA_HOME` by the test itself.

use bevy::prelude::*;

use crate::char_select::Roster;
use crate::local_state::test_env::{EnvGuard, ENV_LOCK};

/// **Bagnon's own idiom**, reduced to the one line that carries the bug: the live character's name,
/// read once while the file runs, and parked where the test can see it.
///
/// `SwitchProbeLoads` counts file-scope runs *within one VM*, so a rebuild resets it to 1 — that
/// number is what tells a re-entry apart from a second load stacked onto the same state.
const PROBE_LUA: &str = "\
local currentPlayer = UnitName(\"player\")
SwitchProbeFileScope = currentPlayer
SwitchProbeLoads = (SwitchProbeLoads or 0) + 1
SwitchProbeDB = { who = currentPlayer }
";

/// …and it declares that table as a per-character saved variable, so the shutdown writes a real
/// file — which is what [`quitting_from_the_character_screen_does_not_blank_the_session_it_wrote`]
/// watches.
const PROBE_TOC: &str = "\
## Interface: 11200
## SavedVariablesPerCharacter: SwitchProbeDB
SwitchProbe.lua
";

/// A roster with a pick in flight, named — the state a world entry actually runs in
/// ([`super::seat_from_roster`] reads exactly this).
fn roster_named(name: &str, guid: u64) -> Roster {
    let row = benilla_protocol::Character {
        guid,
        name: name.into(),
        race: 1,  // Human → Alliance
        class: 1, // Warrior
        gender: 0,
        level: 60,
        skin: 0,
        face: 0,
        hair_style: 0,
        hair_color: 0,
        facial_hair: 0,
        zone: 0,
        map: 0,
        position: benilla_protocol::wire::Vector3d {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
        flags: 0,
        equipment: [benilla_protocol::CharEnumItem::default(); 19],
    };
    Roster::with_pending_pick(vec![row], guid)
}

/// A hermetic state folder holding one addon — the probe — and the guards that point the whole
/// client at it. Every guard must outlive the world.
fn hermetic_probe(tag: &str) -> (std::path::PathBuf, EnvGuard, EnvGuard) {
    // The pid keeps two concurrent `benilla_app` test binaries out of each other's tree, the same
    // reason `addons::tests::hermetic_root` carries one.
    let tmp =
        std::env::temp_dir().join(format!("benilla-world-entry-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let home = tmp.join("benilla-config");
    let dir = home.join("AddOns").join("SwitchProbe");
    std::fs::create_dir_all(&dir).expect("probe addon dir");
    std::fs::write(dir.join("SwitchProbe.toc"), PROBE_TOC).expect("probe toc");
    std::fs::write(dir.join("SwitchProbe.lua"), PROBE_LUA).expect("probe lua");
    let capture = EnvGuard::unset("WOW_CAPTURE");
    let benilla_home = EnvGuard::set("BENILLA_HOME", home.to_str().expect("utf-8 temp path"));
    (tmp, capture, benilla_home)
}

/// The world a session boots into: `Startup` has run ([`super::setup_script`]), so there is a VM
/// carrying the font registry and nothing else.
fn booted_world() -> World {
    let mut world = World::new();
    world.init_resource::<super::AddOnIdentity>();
    world.init_resource::<crate::minimap::MinimapZoom>();
    super::setup_script(&mut world);
    world
}

/// One login, driven exactly as the app drives it: the roster carries the pick, then the world-entry
/// edge runs.
fn log_in_as(world: &mut World, name: &str, guid: u64) {
    world.insert_resource(roster_named(name, guid));
    super::load_ingame_ui_on_world_entry(world);
}

/// What the probe addon captured at file scope this session — `None` if it never ran.
fn probe_saw(world: &World) -> Option<String> {
    world
        .get_non_send_resource::<benilla_ui::script::UiScript>()
        .and_then(|s| s.eval::<Option<String>>("SwitchProbeFileScope").ok())
        .flatten()
}

/// Is a named frame our own FrameXML creates present in the live VM?
fn frame_exists(world: &World, name: &str) -> bool {
    world
        .get_non_send_resource::<benilla_ui::script::UiScript>()
        .and_then(|s| s.eval::<bool>(&format!("return {name} ~= nil")).ok())
        .unwrap_or(false)
}

/// How many times the probe's file scope ran **in the VM that is live now**.
fn probe_loads(world: &World) -> u32 {
    world
        .get_non_send_resource::<benilla_ui::script::UiScript>()
        .and_then(|s| s.eval::<Option<u32>>("SwitchProbeLoads").ok())
        .flatten()
        .unwrap_or(0)
}

/// **The director's report.** Log in as one character, log out to the character screen, log in as
/// another: the second character's addons must see the second character.
///
/// Pre-fix this asserts `Onehunter` on the second login — the whole bug, in one string.
#[test]
fn the_second_login_runs_addon_file_scope_under_the_second_character() {
    let _l = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (tmp, _c, _h) = hermetic_probe("switch");
    let mut world = booted_world();

    log_in_as(&mut world, "Onehunter", 1);
    assert_eq!(
        probe_saw(&world).as_deref(),
        Some("Onehunter"),
        "the first login's addon file scope reads the first character"
    );

    super::end_ui_session(&mut world);
    log_in_as(&mut world, "Onewarrior", 2);

    assert_eq!(
        probe_saw(&world).as_deref(),
        Some("Onewarrior"),
        "the second login's addon file scope must read the SECOND character — this is the \
         director's \"always Onewarrior\" report, from the other side"
    );
    assert_eq!(
        probe_loads(&world),
        1,
        "the second session is a FRESH VM, not the first one loaded twice — a second load stacked \
         onto the live state would count 2 and would have two of every frame"
    );

    drop(world);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The identity the shutdown writes under follows the character, so a logout does not file the
/// second character's saved variables under the first one's name.
///
/// This is the data-corruption half of the same bug: with the load latched, `AddOnIdentity` was
/// only ever written on the first entry, so every later session's `SavedVariables` went into the
/// first character's folder.
#[test]
fn the_addon_identity_follows_the_character() {
    let _l = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (tmp, _c, _h) = hermetic_probe("identity");
    let mut world = booted_world();

    log_in_as(&mut world, "Onehunter", 1);
    let first = world.resource::<super::AddOnIdentity>().0.clone();
    super::end_ui_session(&mut world);
    log_in_as(&mut world, "Onewarrior", 2);
    let second = world.resource::<super::AddOnIdentity>().0.clone();

    assert_ne!(
        first, second,
        "the enable-state / saved-variables identity is re-resolved per login"
    );
    assert_eq!(
        second.as_ref().map(|(_, c)| c.as_str()),
        Some("Onewarrior"),
        "and it names the character actually logged in"
    );

    drop(world);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// **Quitting from the character screen must not blank what the session already wrote.**
///
/// The client's shutdown runs from five roots, and two of them can fire in sequence: a `/logout`
/// ends the session, and then the player quits from the character screen — where `AppExit` runs
/// [`super::shutdown_ui_state`] again, now against a boot VM with no addon in it. Writing the saved
/// variables *from* that VM would compose every file from nothing.
///
/// It does not, and this pins why: the three write paths each refuse an empty source
/// (`ui_saved::save` on `names.is_empty()`, `save_enable_state` on `states.is_empty()` — its own
/// comment already called an empty write a wipe — and `save_addon_variables` because a boot VM
/// declares no variable sets to iterate). The reference reaches the same place with an explicit
/// guard (`0x401ee0`'s `ds:0x882734` test: "logout then quit writes once, not twice"); ours falls
/// out of the writers having nothing to say, which is only a *safe* answer for as long as those
/// guards hold. Hence a test rather than a comment.
#[test]
fn quitting_from_the_character_screen_does_not_blank_the_session_it_wrote() {
    let _l = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (tmp, _c, _h) = hermetic_probe("quit");
    let mut world = booted_world();

    log_in_as(&mut world, "Onehunter", 1);
    super::end_ui_session(&mut world);

    let saved = crate::local_state::addon_saved_character_dir("Realm", "Onehunter")
        .expect("a hermetic home resolves the per-character saved dir")
        .join("SwitchProbe.lua");
    let after_logout = std::fs::read_to_string(&saved).expect("the logout wrote the addon's file");
    assert!(
        after_logout.contains("Onehunter"),
        "…and wrote the character it belonged to: {after_logout}"
    );

    // Now quit — `shutdown_on_exit`'s body, against the boot VM the logout left behind.
    let identity = world.resource::<super::AddOnIdentity>().0.clone();
    let mut script = world
        .remove_non_send_resource::<benilla_ui::script::UiScript>()
        .expect("a boot VM is live at the character screen");
    super::shutdown_ui_state(&mut script, identity.as_ref());

    assert_eq!(
        std::fs::read_to_string(&saved).ok().as_deref(),
        Some(after_logout.as_str()),
        "the quit pass wrote nothing — the session's file is byte-identical"
    );

    drop(world);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Between a logout and the next login there is **no in-game UI at all** — the character screen is
/// native, and the previous session's frame tree must not survive behind it.
///
/// 1051 measured what that costs when it does: probed under login-screen conditions the in-game
/// tree emits 193 quads, invisible only because the glue screen's opaque node covers them.
#[test]
fn logging_out_leaves_no_in_game_frames_behind() {
    let _l = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (tmp, _c, _h) = hermetic_probe("teardown");
    let mut world = booted_world();

    log_in_as(&mut world, "Onehunter", 1);
    assert!(
        probe_saw(&world).is_some(),
        "the session under test actually loaded"
    );

    super::end_ui_session(&mut world);

    assert_eq!(
        probe_saw(&world),
        None,
        "the session's Lua state is gone at the character screen"
    );
    assert!(
        !frame_exists(&world, "PlayerFrame"),
        "and so is the in-game frame tree — 1051 measured 193 quads' worth of it surviving \
         behind the glue screen's opaque node"
    );
    assert!(
        world
            .get_non_send_resource::<benilla_ui::script::UiScript>()
            .is_some(),
        "a boot VM stays: the character screen's text still bakes off the shared font registry"
    );

    drop(world);
    let _ = std::fs::remove_dir_all(&tmp);
}
