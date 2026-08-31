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
        pet_display_id: 0,
        pet_level: 0,
        pet_family: 0,
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
    world.init_resource::<super::ReloadUiPending>();
    super::setup_script(&mut world);
    world
}

/// Queue and run a `ReloadUI()` the way the app does: the pending flag, then
/// [`super::run_pending_reload`] — which checks the client state itself, so the test states it.
fn reload(world: &mut World, state: crate::char_select::ClientState) {
    world.insert_resource(State::new(state));
    world.resource_mut::<super::ReloadUiPending>().0 = true;
    super::run_pending_reload(world);
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

// ───────────────────────────────── ReloadUI (decision 1291) ─────────────────────────────────

/// **`ReloadUI()` is a real login run in place** — the reference's teardown/rebuild pair
/// (`0x495664`/`0x495669`), which for us is the same two edge functions the logout/login cycle
/// runs. A fresh VM, a fresh file scope, the same character, and the UI back up — without leaving
/// the world.
#[test]
fn reload_ui_is_a_fresh_login_in_place() {
    let _l = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (tmp, _c, _h) = hermetic_probe("reload");
    let mut world = booted_world();

    log_in_as(&mut world, "Onehunter", 1);
    let first_session = world
        .get_non_send_resource::<benilla_ui::script::UiScript>()
        .expect("in-world VM")
        .session();

    reload(&mut world, crate::char_select::ClientState::InWorld);

    assert_eq!(
        probe_saw(&world).as_deref(),
        Some("Onehunter"),
        "the reloaded session ran the addon's file scope again, under the same character"
    );
    assert_eq!(
        probe_loads(&world),
        1,
        "…in a FRESH VM — a reload stacked onto the live state would count 2"
    );
    assert_ne!(
        world
            .get_non_send_resource::<benilla_ui::script::UiScript>()
            .expect("in-world VM")
            .session(),
        first_session,
        "the VM identity changed, so every VmMemo about the old session expires (1290)"
    );
    assert!(
        frame_exists(&world, "PlayerFrame"),
        "and the in-game UI is back up"
    );
    assert!(
        !world.resource::<super::ReloadUiPending>().0,
        "the request was consumed"
    );

    drop(world);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// **A toggle staged through the API takes effect at the reload** — the whole point of the verb.
/// `DisableAddOn` only marks the live registry; the reload's shutdown tail writes `AddOns.txt`
/// (the reference's own last write before the state dies), and the rebuild reads it back — so the
/// addon is genuinely not loaded, not hidden.
#[test]
fn a_disable_staged_in_the_session_applies_at_the_reload() {
    let _l = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (tmp, _c, _h) = hermetic_probe("disable");
    let mut world = booted_world();

    log_in_as(&mut world, "Onehunter", 1);
    assert!(
        probe_saw(&world).is_some(),
        "the probe loaded to begin with"
    );
    world
        .get_non_send_resource::<benilla_ui::script::UiScript>()
        .expect("in-world VM")
        .run("DisableAddOn('SwitchProbe')")
        .expect("DisableAddOn");
    assert!(
        probe_saw(&world).is_some(),
        "disabling alone changes nothing in the live session — there is no unload (1197)"
    );

    reload(&mut world, crate::char_select::ClientState::InWorld);

    assert_eq!(
        probe_saw(&world),
        None,
        "after the reload the disabled addon's file scope never ran"
    );
    let enable_file = super::addons::enable_state_path(Some(&("Realm".into(), "Onehunter".into())))
        .expect("hermetic enable path");
    let text = std::fs::read_to_string(&enable_file).expect("the teardown wrote AddOns.txt");
    assert!(
        text.contains("SwitchProbe: disabled"),
        "…because the choice reached disk on the way down: {text}"
    );

    drop(world);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// **Saved variables survive the reload** — written by the teardown (after `PLAYER_LOGOUT`),
/// restored by the rebuild after file scope, so the saved value wins over the file-scope default
/// (the byte-verified `AddOn_Load` order, 1128).
#[test]
fn saved_variables_round_trip_through_a_reload() {
    let _l = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (tmp, _c, _h) = hermetic_probe("saved");
    let mut world = booted_world();

    log_in_as(&mut world, "Onehunter", 1);
    world
        .get_non_send_resource::<benilla_ui::script::UiScript>()
        .expect("in-world VM")
        .run("SwitchProbeDB.mark = 41")
        .expect("mutate the saved table");

    reload(&mut world, crate::char_select::ClientState::InWorld);

    let mark = world
        .get_non_send_resource::<benilla_ui::script::UiScript>()
        .expect("in-world VM")
        .eval::<Option<u32>>("return SwitchProbeDB and SwitchProbeDB.mark")
        .expect("read back")
        .unwrap_or(0);
    assert_eq!(
        mark, 41,
        "the reload wrote the table down and the rebuild restored it OVER the file-scope default"
    );

    drop(world);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A `ReloadUI()` that fires outside the world is dropped, not deferred: at the glue there is no
/// in-game UI to rebuild and no identity to load addons under (the reference's own gate,
/// `0x494a50(0xa)`, refuses there too).
#[test]
fn reload_outside_the_world_is_dropped() {
    let _l = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (tmp, _c, _h) = hermetic_probe("glue-reload");
    let mut world = booted_world();

    log_in_as(&mut world, "Onehunter", 1);
    super::end_ui_session(&mut world);

    reload(&mut world, crate::char_select::ClientState::CharSelect);

    assert_eq!(
        probe_saw(&world),
        None,
        "no addon loaded — the request was dropped, not run against the glue"
    );
    assert!(
        !frame_exists(&world, "PlayerFrame"),
        "and no in-game UI appeared behind the character screen"
    );
    assert!(
        !world.resource::<super::ReloadUiPending>().0,
        "the stale request is consumed, so it cannot fire on the NEXT login's first frame"
    );

    drop(world);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// **B271, the error half.** An addon that raises at file scope while entering world must not
/// take the client with it: the walk reports it, the sibling addon still loads, and the player
/// sees the reference's red ScriptErrors dialog (decision 1305) — the report was debugged
/// entirely off terminal WARN lines because the client showed nothing.
#[test]
fn an_addon_error_while_entering_world_reports_on_screen_and_the_sibling_loads() {
    let _l = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (tmp, _c, _h) = hermetic_probe("addon-error");
    // A second addon, alphabetically FIRST, that dies at file scope — so the probe behind it
    // proves a broken neighbour drops only itself.
    let dir = tmp.join("benilla-config/AddOns/AaBroken");
    std::fs::create_dir_all(&dir).expect("broken addon dir");
    std::fs::write(dir.join("AaBroken.toc"), "## Interface: 11200\nboom.lua\n").expect("toc");
    std::fs::write(dir.join("boom.lua"), "error('B271: file-scope boom')\n").expect("lua");
    let mut world = booted_world();

    log_in_as(&mut world, "Onehunter", 1);
    assert_eq!(
        probe_saw(&world).as_deref(),
        Some("Onehunter"),
        "the addon AFTER the broken one still loads — a neighbour's error drops only itself"
    );

    // The app's per-frame drain runs the dispatch; the test runs the same call.
    let mut script = world
        .remove_non_send_resource::<benilla_ui::script::UiScript>()
        .expect("VM");
    script.dispatch_script_errors_to_handler();
    assert!(
        script
            .eval::<bool>("return ScriptErrors:IsVisible()")
            .expect("ScriptErrors exists — BasicControls loaded"),
        "the ScriptErrors dialog is on screen — `seterrorhandler(_ERRORMESSAGE)` is installed \
         and the engine dispatched the caught error to it"
    );
    let shown: String = script
        .eval::<Option<String>>("return ScriptErrors_Message:GetText()")
        .expect("eval")
        .unwrap_or_default();
    assert!(
        shown.contains("B271: file-scope boom"),
        "the dialog names the actual error, got: {shown:?}"
    );
    drop(script);
    drop(world);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// **B271, the freeze half.** An addon that never returns cannot freeze world entry: the load
/// bound (decision 1306) fails it with the distinctive budget message, the sibling addon still
/// loads, and this test FINISHING is the claim — before 1306 it would hang here forever.
#[test]
fn a_looping_addon_cannot_freeze_world_entry() {
    let _l = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (tmp, _c, _h) = hermetic_probe("addon-loop");
    let dir = tmp.join("benilla-config/AddOns/AaSpin");
    std::fs::create_dir_all(&dir).expect("spin addon dir");
    std::fs::write(dir.join("AaSpin.toc"), "## Interface: 11200\nspin.lua\n").expect("toc");
    std::fs::write(dir.join("spin.lua"), "while true do end\n").expect("lua");
    let mut world = booted_world();

    log_in_as(&mut world, "Onehunter", 1);

    assert_eq!(
        probe_saw(&world).as_deref(),
        Some("Onehunter"),
        "the addon after the spinner still loads — the budget failed one addon, not the entry"
    );
    // The budget raise travelled the load walk's failure arm into the handler queue (1305), so
    // the player-facing dialog is where it lands — the frozen loading screen becomes a dialog
    // that NAMES the loop.
    let mut script = world
        .remove_non_send_resource::<benilla_ui::script::UiScript>()
        .expect("VM");
    script.dispatch_script_errors_to_handler();
    let shown: String = script
        .eval::<Option<String>>("return ScriptErrors_Message:GetText()")
        .expect("eval")
        .unwrap_or_default();
    assert!(
        shown.contains("instruction budget exhausted"),
        "the dialog names the runaway loop with the budget's distinctive message, got: {shown:?}"
    );

    drop(script);
    drop(world);
    let _ = std::fs::remove_dir_all(&tmp);
}

// ─────────────────── The deferred entry load (0962's frame accounting) ───────────────────

/// **The director's "frozen char for 1 sec" report, from the other side.** With the cover up,
/// the armed entry load waits [`super::lifecycle::run_pending_entry_load`]'s covered-frame
/// count — the frames whose renders put the cover on the glass — and only then pays the burst.
/// Before the deferral the load ran inside `OnEnter(InWorld)`, which is exactly the frame whose
/// render would first present the cover, so the ~0.5 s of FrameXML + addons + `PLAYER_LOGIN`
/// held the previous present: the frozen character screen.
#[test]
fn the_entry_load_waits_for_the_cover_to_present() {
    let _l = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (tmp, _c, _h) = hermetic_probe("defer");
    let mut world = booted_world();
    world.insert_resource(State::new(crate::char_select::ClientState::InWorld));
    world.insert_resource(crate::loading_screen::LoadingScreen::test_covering());
    world.insert_resource(crate::loading_screen::EntryCover::default());
    world.insert_resource(roster_named("Onehunter", 1));
    world.insert_resource(super::PendingEntryUiLoad::default());

    // Covered frames 1 and 2: the cover has not provably presented yet — no load.
    for frame in 1..=2 {
        world
            .resource_mut::<crate::loading_screen::EntryCover>()
            .tick(true);
        super::lifecycle::run_pending_entry_load(&mut world);
        assert_eq!(
            probe_saw(&world),
            None,
            "covered frame {frame}: the burst must wait for the cover to reach the glass"
        );
    }
    // Covered frame 3 onward: two cover renders have committed — the load starts, behind a
    // presented cover, and (since the slicing) spreads over as many frames as its budget needs;
    // the latch stays up the whole way so the clear condition keeps waiting on it.
    let mut frames = 0;
    while world.get_resource::<super::PendingEntryUiLoad>().is_some() {
        world
            .resource_mut::<crate::loading_screen::EntryCover>()
            .tick(true);
        super::lifecycle::run_pending_entry_load(&mut world);
        frames += 1;
        assert!(frames < 10_000, "the sliced entry load never finished");
    }
    assert_eq!(
        probe_saw(&world).as_deref(),
        Some("Onehunter"),
        "the load ran behind a presented cover, over {frames} frame(s)"
    );
    assert!(
        world.get_resource::<super::PendingEntryUiLoad>().is_none(),
        "the latch is consumed — the loading screen's clear condition reads its absence"
    );

    drop(world);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// No cover — a capture booting straight `InWorld`, or the screen's assets missing — means no
/// glass to protect: the armed load runs on the first frame. Without this arm a coverless run
/// would count covered frames that never come and the UI would never load.
#[test]
fn no_cover_means_the_entry_load_runs_at_once() {
    let _l = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (tmp, _c, _h) = hermetic_probe("nocover");
    let mut world = booted_world();
    world.insert_resource(State::new(crate::char_select::ClientState::InWorld));
    world.insert_resource(crate::loading_screen::LoadingScreen::default());
    world.insert_resource(roster_named("Onehunter", 1));
    world.insert_resource(super::PendingEntryUiLoad::default());

    super::lifecycle::run_pending_entry_load(&mut world);
    // "At once" = the first frame takes real steps instead of counting cover frames that never
    // come. Since the slicing, finishing takes as many frames as the budget needs.
    assert!(
        world
            .get_resource::<super::PendingEntryUiLoad>()
            .is_none_or(|p| p.fraction() > 0.0),
        "uncovered: the load starts on the first frame"
    );
    let mut frames = 1;
    while world.get_resource::<super::PendingEntryUiLoad>().is_some() {
        super::lifecycle::run_pending_entry_load(&mut world);
        frames += 1;
        assert!(frames < 10_000, "the sliced entry load never finished");
    }
    assert_eq!(
        probe_saw(&world).as_deref(),
        Some("Onehunter"),
        "uncovered: the load ran, over {frames} frame(s)"
    );

    drop(world);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// An exit inside the deferral window (an instant disconnect at entry) drops the armed load and
/// **writes nothing**: the session never built a UI, so the shutdown tail running against the
/// boot VM would compose every saved file from emptiness — the wipe
/// [`quitting_from_the_character_screen_does_not_blank_the_session_it_wrote`] guards at the
/// other edge.
#[test]
fn leaving_inside_the_deferral_window_drops_the_load_and_writes_nothing() {
    let _l = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (tmp, _c, _h) = hermetic_probe("dropped");
    let mut world = booted_world();
    world.insert_resource(State::new(crate::char_select::ClientState::InWorld));
    world.insert_resource(crate::loading_screen::LoadingScreen::test_covering());
    world.insert_resource(roster_named("Onehunter", 1));
    world.insert_resource(crate::loading_screen::EntryCover::default());
    world.insert_resource(super::PendingEntryUiLoad::default());

    world
        .resource_mut::<crate::loading_screen::EntryCover>()
        .tick(true);
    super::lifecycle::run_pending_entry_load(&mut world); // covered frame 1 — still pending
    super::end_ui_session(&mut world);

    assert_eq!(probe_saw(&world), None, "no UI ever loaded");
    assert!(
        world.get_resource::<super::PendingEntryUiLoad>().is_none(),
        "the latch died with the session — it must not fire on the glue"
    );
    let flat = crate::local_state::saved_variables_path()
        .expect("hermetic home resolves the flat saved path");
    assert!(
        !flat.exists(),
        "the shutdown tail was skipped — a UI-less VM must not write saved variables"
    );

    drop(world);
    let _ = std::fs::remove_dir_all(&tmp);
}

// ───────────── The login one-shots wait for the in-game UI (decision 1348) ─────────────

/// **The director's white XP bar, from the side that causes it.**
///
/// The world-entry UI load is deferred a few covered frames (0962/1051), and the unit feed is not:
/// it fires `PLAYER_ENTERING_WORLD`, the first `PLAYER_XP_UPDATE` and the first `UPDATE_EXHAUSTION`
/// the moment our own descriptor lands. When that lands *inside* the deferral window the events go
/// to a VM with no frames, and because every one of them is latched by a [`super::VmMemo`] keyed on
/// the VM's session — which the entry load does not change — they never fire again. The frames
/// built moments later do their first paint with no first paint, which is why
/// `ExhaustionTick_Update` had never run and `ExhaustionLevelFillBar` was still wearing its
/// authored opaque white across the whole XP strip, with the tick parked at the strip's centre.
///
/// It is a RACE against the wire, so it took some logins and not others.
///
/// The probe is a plain frame in the boot VM registering the event the way FrameXML does — if the
/// feed runs at all in the window, it sees it.
#[test]
fn the_login_one_shots_wait_for_the_in_game_ui() {
    let _l = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (tmp, _c, _h) = hermetic_probe("oneshot");

    let mut app = App::new();
    app.add_plugins(crate::ui_unit::UiUnitPlugin);
    app.init_resource::<super::AddOnIdentity>();
    app.init_resource::<crate::minimap::MinimapZoom>();
    app.init_resource::<super::ReloadUiPending>();
    app.init_resource::<crate::target::Selection>();
    app.init_resource::<crate::names::NameCache>();
    let (tx, _rx) = crossbeam_channel::unbounded();
    app.insert_resource(crate::net::NetCommands(tx));
    app.init_resource::<crate::net::Reputations>();
    app.init_resource::<crate::ui_party::GroupState>();
    app.init_resource::<crate::ui_chat::ChatLog>();
    app.init_resource::<crate::ui_guild::GuildState>();
    app.add_message::<crate::creature_anim::SwingImpact>();
    super::setup_script(app.world_mut());

    // The probe frame — FrameXML's own shape, in the VM that exists before the entry load.
    app.world()
        .non_send_resource::<benilla_ui::script::UiScript>()
        .run(
            "EnteringWorldSeen = 0 \
             local f = CreateFrame(\"Frame\") \
             f:RegisterEvent(\"PLAYER_ENTERING_WORLD\") \
             f:SetScript(\"OnEvent\", function() \
                 EnteringWorldSeen = EnteringWorldSeen + 1 end)",
        )
        .expect("probe frame");

    // Our own descriptor has landed — the condition the feed fires the one-shots on.
    app.world_mut().spawn((
        crate::net::SelfPlayer,
        crate::net::Guid(1),
        crate::net::ObjectStore(
            benilla_protocol::messages::ObjectFields::from_pairs(&[])
                .into_created(benilla_protocol::messages::ObjectType::Player),
        ),
    ));

    let seen = |app: &App| -> i64 {
        app.world()
            .non_send_resource::<benilla_ui::script::UiScript>()
            .eval::<i64>("return EnteringWorldSeen")
            .expect("probe global")
    };

    // …but the in-game UI is still owed. Three frames inside the deferral window.
    app.insert_resource(super::PendingEntryUiLoad::default());
    for frame in 1..=3 {
        app.update();
        assert_eq!(
            seen(&app),
            0,
            "frame {frame}: the feed must not spend PLAYER_ENTERING_WORLD on a UI-less VM"
        );
    }

    // The entry load has run (its own tests cover the timing) — the latch is gone, and the very
    // next feed delivers the full set to the frames that now exist.
    app.world_mut()
        .remove_resource::<super::PendingEntryUiLoad>();
    app.update();
    assert_eq!(
        seen(&app),
        1,
        "with the UI up the one-shot fires — once, on the first feed after the load"
    );
    app.update();
    assert_eq!(seen(&app), 1, "and exactly once per world entry");

    drop(app);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// **B293's headline, at the edge that produces it** (decision 1495). An addon that fails to load
/// *without raising* — the commonest shape by far, a `.toc` naming a file the package does not
/// ship — used to `warn!` to the terminal and vanish. Nothing raised, so 1305's dialog could not
/// fire; the walk's failure list was dropped on the floor at `load_ingame_ui_on_world_entry`; and
/// the per-frame drain kept no history. From the player's chair the addon simply was not there and
/// the client said nothing, which is the literal content of *"there are a lot of addons that still
/// doesn't work"*.
///
/// Three claims, and the third is the one that makes the first two reachable: the failure is
/// **retained**, it is **readable from Lua** (so the window is a view of it, not a second copy),
/// and the player is **told to look**.
#[test]
fn an_addon_that_fails_to_load_without_raising_is_readable_in_the_error_log() {
    let _l = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (tmp, _c, _h) = hermetic_probe("addon-missing-file");
    // Alphabetically first, so the probe behind it also proves this class drops only itself.
    let dir = tmp.join("benilla-config/AddOns/AaMissing");
    std::fs::create_dir_all(&dir).expect("addon dir");
    std::fs::write(
        dir.join("AaMissing.toc"),
        "## Interface: 11200\nBossnames\\BossNames.xml\n",
    )
    .expect("toc");
    // …and no such file is written. This is the director's own AtlasLoot copy, reduced.
    let mut world = booted_world();
    world.init_resource::<crate::ui_chat::ChatLog>();

    log_in_as(&mut world, "Onehunter", 1);

    assert_eq!(
        probe_saw(&world).as_deref(),
        Some("Onehunter"),
        "the addon after the broken one still loads"
    );

    let script = world
        .get_non_send_resource::<benilla_ui::script::UiScript>()
        .expect("VM");

    // 1 · retained, and tagged as a LOAD failure — the kind that means "this addon is not running".
    let rows = script.diagnostics();
    let row = rows
        .iter()
        .find(|d| d.message.contains("AaMissing"))
        .unwrap_or_else(|| panic!("the missing file is in the log; got {rows:#?}"));
    assert_eq!(
        row.kind,
        benilla_ui::script::diagnostics::DiagnosticKind::Load
    );
    assert!(
        row.message.contains("not found"),
        "the row says what went wrong, verbatim as the terminal line: {:?}",
        row.message
    );

    // 2 · readable from Lua by the same three reads the window uses.
    let count: i64 = script
        .eval("local shown = BenillaGetNumScriptErrors() return shown")
        .expect("BenillaGetNumScriptErrors is installed");
    assert!(count >= 1, "the window's own read sees it");
    let seen: String = script
        .eval(
            "local text = '' \
             for i = 1, BenillaGetNumScriptErrors() do \
                local seq, kind, message = BenillaGetScriptErrorInfo(i) \
                if kind == 'load' then text = message end \
             end \
             return text",
        )
        .expect("BenillaGetScriptErrorInfo is installed");
    assert!(
        seen.contains("AaMissing"),
        "the window walks the log and finds it: {seen:?}"
    );

    // …and the window itself materialized, so `/errors` has something to toggle.
    assert!(
        script
            .eval::<bool>("return BenillaScriptLogFrame ~= nil")
            .expect("eval"),
        "ScriptLogFrame.xml loaded and built the window"
    );

    // **The repaint actually runs, over a log that has a row in it.** Asserting the globals exist
    // would prove nothing: this drives the real path — `FauxScrollFrame_Update`, the row rebind,
    // `strfind`/`strsub`/`strlen`/`format`, `SetTextColor`, the highlight seat and the detail
    // pane — and a `nil` global anywhere in it raises here instead of on the player's first
    // `/errors` (which is exactly the shape of failure this whole record exists to stop shipping).
    script
        .eval::<()>("BenillaScriptLog_Update() return nil")
        .expect("the window repaints over a real log without raising");
    let row_label: String = script
        .eval::<Option<String>>("return BenillaScriptLogRow1Label:GetText()")
        .expect("eval")
        .unwrap_or_default();
    assert!(
        row_label.contains("AaMissing"),
        "row 1 shows the failure, trimmed to the row's width: {row_label:?}"
    );
    let summary: String = script
        .eval::<Option<String>>("return BenillaScriptLogSummary:GetText()")
        .expect("eval")
        .unwrap_or_default();
    assert!(
        summary.contains("error"),
        "the summary line counted them: {summary:?}"
    );

    // 3 · **this class deliberately does NOT seize the screen.** Nothing raised; the reference's
    // answer to an unparseable/absent document is a log line and silence, and 1495 keeps that.
    // What it changes is that the silence is no longer total — hence the chat notice below.
    assert!(
        !script
            .eval::<bool>("return ScriptErrors:IsVisible()")
            .expect("ScriptErrors exists"),
        "a non-raising load failure must not pop the red dialog — that would put non-errors \
         through `_ERRORMESSAGE` and through every addon handler that replaces it"
    );

    // 4 · the player is told to look. Without this the log is a room nobody knows about, and
    // silence — not the missing list — is the actual defect B293 reports.
    assert_eq!(
        world.resource::<crate::ui_chat::ChatLog>().pending_len(),
        1,
        "world entry queued the 'N addon load failures — type /errors' line"
    );

    drop(world);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// **The burst 1305 measured, as the log sees it.** An `OnUpdate` that raises every frame produced
/// 470–1113 collected errors in 1305's own runs, of which `_ERRORMESSAGE` shows the **first** and
/// the per-frame drain keeps none. Deduplication is what makes a log survive that: one row with a
/// count, not 1,113 rows and not a truncated window of the last few.
#[test]
fn a_repeating_error_is_one_row_with_a_count_not_a_flood() {
    let _l = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (tmp, _c, _h) = hermetic_probe("addon-repeat");
    let mut world = booted_world();
    log_in_as(&mut world, "Onehunter", 1);

    let mut script = world
        .remove_non_send_resource::<benilla_ui::script::UiScript>()
        .expect("VM");
    let before = script.diagnostics().len();
    // The same failure, over and over, through the engine's own catch path — a slash command whose
    // body raises is the cheapest real one to drive from a test.
    script
        .run("SlashCmdList = SlashCmdList or {} SLASH_B293BOOM1 = '/b293boom' SlashCmdList['B293BOOM'] = function() error('every frame') end")
        .expect("register");
    for _ in 0..500 {
        script.run_slash_command("b293boom", "");
    }

    let rows = script.diagnostics();
    assert_eq!(
        rows.len(),
        before + 1,
        "500 identical raises are ONE new row: {rows:#?}"
    );
    let row = rows.last().expect("a row");
    assert_eq!(row.count, 500, "the count is where the 500 went");
    assert_eq!(
        row.kind,
        benilla_ui::script::diagnostics::DiagnosticKind::Error,
        "code ran and raised — the addon is loaded, unlike a Load row"
    );

    drop(script);
    drop(world);
    let _ = std::fs::remove_dir_all(&tmp);
}
