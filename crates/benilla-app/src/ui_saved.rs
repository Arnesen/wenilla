//! The **saved-variables** host side (decision 1128) — the file, the load seam, the write
//! triggers. The engine half (the `RegisterForSave` declaration set and the serializer) is
//! [`benilla_ui::script`]'s `saved` module; this is the part that touches the disk and the clock.
//!
//! **Load** — one chunk, executed straight into the VM immediately after the in-game UI's XML has
//! loaded and before anything runs against it, then `VARIABLES_LOADED`. That ordering is the whole
//! mechanism, byte-verified in wow-re (`system/ui/scratch/savedvariables-protocol.md`): the
//! reference's `AddOn_Load 0x51f240` runs the addon's own files (step 2 — where the file-scope
//! `TRAINER_FILTER_* = 1` defaults are assigned), *then* executes the saved file over the top
//! (step 4), *then* fires the load event (step 6). Defaults first, saved values second, consumers
//! third — reverse any two and the saved value can never win.
//!
//! **Write** — `OnExit(InWorld)` (a `/logout` or a disconnect) and `AppExit`, the two edges our
//! session has. The reference writes from exactly one place, the UI shutdown `0x490bd0`, reached
//! from five roots (logout to character select, quit, disconnect, application exit, `/reload`),
//! with `PLAYER_LOGOUT` fired just before; it has no autosave, no dirty bit, and no Lua binding
//! that can force a write. We keep that shape rather than debouncing like [`crate::cvars`] does:
//! these are a handful of scalars a player toggles a few times a session, and the file is written
//! whole from the live globals, so there is nothing an intermediate write would preserve.
//!
//! Divergence, disclosed (1128): the reference rotates the old file to `.bak` and then truncates
//! in place — a one-shot, not crash safety (`MoveFileW` without `MOVEFILE_REPLACE_EXISTING`, its
//! result discarded, so after the first rotation it overwrites forever, and nothing ever reads a
//! `.bak`). We write through [`crate::local_state::write_atomic`] instead, which is what that dance
//! was reaching for.

use bevy::prelude::*;

use benilla_ui::script::UiScript;

use crate::char_select::ClientState;

pub(crate) struct UiSavedPlugin;

impl Plugin for UiSavedPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnExit(ClientState::InWorld), save_on_session_end)
            .add_systems(Update, save_on_exit);
    }
}

/// Execute the saved-variables file into the VM and fire `VARIABLES_LOADED`.
///
/// Called from the in-game UI load ([`crate::ui_script`]) at the reference's own seam — after the
/// XML, before anything consumes it. A missing file is the normal first-run case (defaults stand);
/// a malformed one warns and is left on disk untouched, so a hand edit that fails to parse costs
/// this session's settings and not the file.
pub(crate) fn load_saved_variables(script: &mut UiScript) {
    // `None` = hermetic capture, or no install — session-only state, and the event below still fires.
    if let Some(path) = crate::local_state::saved_variables_path() {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                if let Err(e) = script.run(&text) {
                    warn!(
                        "saved variables: {} did not load ({e}) — running on defaults",
                        path.display()
                    );
                } else {
                    info!("saved variables: loaded {}", path.display());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => warn!("saved variables: cannot read {}: {e}", path.display()),
        }
    }
    // `VARIABLES_LOADED` fires whether or not there was a file: it means "the settings are now what
    // they are going to be", which is as true on a first run or a hermetic capture as after a real
    // restore — the reference fires it as a step of the load sequence, not conditionally. A window
    // that waits on it (GameTime.xml, BagFrame.xml) must not be left uninitialized in a capture.
    script.fire_event("VARIABLES_LOADED", vec![]);
}

/// Write the file from the live globals. No-op when nothing is registered — the UI never loaded
/// (a glue-only run, a capture), and an empty write would be a wipe rather than a save. The
/// reference *does* delete the file when an addon declares nothing, which is a different case:
/// there, the addon loaded and its declaration list is genuinely empty.
fn save(script: &mut UiScript) {
    let names = script.saved_variable_names();
    if names.is_empty() {
        return;
    }
    let Some(path) = crate::local_state::saved_variables_path() else {
        return;
    };
    let body = script.saved_variables_text();
    for w in script.take_warnings() {
        warn!("saved variables: {w}");
    }
    match crate::local_state::write_atomic(&path, &format!("{HEADER}{body}")) {
        Ok(()) => info!(
            "saved variables: wrote {} ({} names)",
            path.display(),
            names.len()
        ),
        Err(e) => warn!("saved variables: cannot write {}: {e}", path.display()),
    }
}

/// The file's own header. The reference writes no header at all (its files open with a bare blank
/// line); ours says what the file is, because a visible folder invites a look.
const HEADER: &str = "\
-- benilla saved variables (decision 1128) — the UI's own remembered settings.
-- Written at logout/exit from the live values; executed as a Lua chunk at UI load.
";

/// `OnExit(InWorld)`: a `/logout` back to the glue, or a disconnect — the reference's
/// logout-to-character-select and disconnect roots.
fn save_on_session_end(script: Option<NonSendMut<UiScript>>) {
    if let Some(mut script) = script {
        save(&mut script);
    }
}

/// `AppExit`: quitting the client — the reference's quit / application-exit roots. Reads the
/// message rather than a state edge because a quit from in-world never leaves `InWorld`.
fn save_on_exit(script: Option<NonSendMut<UiScript>>, mut exits: MessageReader<AppExit>) {
    if exits.read().next().is_none() {
        return;
    }
    if let Some(mut script) = script {
        save(&mut script);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_state::test_env::{EnvGuard, ENV_LOCK};

    /// A VM with one declared global and a `VARIABLES_LOADED` witness.
    fn script(value: &str) -> UiScript {
        let s = UiScript::new().unwrap();
        s.run(&format!(
            "KEPT = {value} RegisterForSave(\"KEPT\") \
             VL_SEEN = 0 \
             local f = CreateFrame(\"Frame\") \
             f:RegisterEvent(\"VARIABLES_LOADED\") \
             f:SetScript(\"OnEvent\", function() VL_SEEN = VL_SEEN + 1 end)"
        ))
        .unwrap();
        s
    }

    /// The disk half, end to end: the file lands in the folder, carries its header, and a *different*
    /// VM's value is replaced by the saved one — then `VARIABLES_LOADED` fires, once, after it.
    #[test]
    fn the_file_round_trips_through_the_folder_and_then_fires_variables_loaded() {
        let _l = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("benilla-sv-{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        let _c = EnvGuard::unset("WOW_CAPTURE");
        let _h = EnvGuard::set("BENILLA_HOME", tmp.to_str().unwrap());

        let mut s = script("7");
        save(&mut s);
        let path = tmp.join("saved-variables.lua");
        let text = std::fs::read_to_string(&path).expect("the file was written");
        assert!(text.starts_with("-- benilla saved variables"), "{text}");
        assert!(text.contains("KEPT = 7"), "{text}");

        // The restart: this VM's own default is 1, the file says 7, and the file wins.
        let mut fresh = script("1");
        load_saved_variables(&mut fresh);
        assert_eq!(fresh.eval::<i64>("return KEPT").unwrap(), 7);
        assert_eq!(
            fresh.eval::<i64>("return VL_SEEN").unwrap(),
            1,
            "VARIABLES_LOADED fires exactly once, after the chunk"
        );

        // A malformed file is left alone, and this session simply runs on defaults.
        crate::local_state::write_atomic(&path, "KEPT = = 3\n").unwrap();
        let mut broken = script("1");
        load_saved_variables(&mut broken);
        assert_eq!(broken.eval::<i64>("return KEPT").unwrap(), 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "KEPT = = 3\n");
        std::fs::remove_dir_all(&tmp).ok();
    }

    /// Hermetic: a capture run neither reads a machine's settings nor writes any back, even with
    /// `BENILLA_HOME` pointing somewhere writable (0954's law, and the reason captures are stable).
    #[test]
    fn a_capture_run_neither_reads_nor_writes() {
        let _l = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("benilla-svcap-{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        let _h = EnvGuard::set("BENILLA_HOME", tmp.to_str().unwrap());
        let _c = EnvGuard::set("WOW_CAPTURE", "ui-saved");

        let mut s = script("7");
        save(&mut s);
        assert!(!tmp.exists(), "a capture must not plant a settings file");
        // The load is a no-op too — but the event still fires, so a window waiting on it is not stuck.
        let mut fresh = script("1");
        load_saved_variables(&mut fresh);
        assert_eq!(fresh.eval::<i64>("return KEPT").unwrap(), 1);
        assert_eq!(fresh.eval::<i64>("return VL_SEEN").unwrap(), 1);
    }

    /// Nothing registered = the UI never loaded: writing would be a wipe, so it is skipped.
    #[test]
    fn an_empty_declaration_set_writes_nothing() {
        let _l = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("benilla-svempty-{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        let _c = EnvGuard::unset("WOW_CAPTURE");
        let _h = EnvGuard::set("BENILLA_HOME", tmp.to_str().unwrap());

        let mut bare = UiScript::new().unwrap();
        save(&mut bare);
        assert!(!tmp.exists());
    }
}
