//! The app half of the macro system (decision 0983): persistence under `benilla/macros/`, the
//! runner's route into the chat drain, and the seed/dirty contract the plugin's systems rely on.
//!
//! The file FORMAT has its own tests in [`super::store`] (including the director's real 1.12
//! `macros-cache.txt`); the API's own round trip is `benilla_ui::script::macros`'; the window's is
//! `crate::ui_script::macro_tests`. What is only testable here is the wiring.

use benilla_ui::script::{MacroState, MacroView, UiScript};

use crate::local_state::test_env::{EnvGuard, ENV_LOCK};

fn macro_view(name: &str, body: &str) -> MacroView {
    MacroView {
        name: name.into(),
        texture: Some("Interface\\Icons\\Ability_Ambush".into()),
        body: body.into(),
        local_only: false,
    }
}

/// A save writes the reference's own format under `benilla/macros/`, and a load brings the same
/// macros back — the whole persistence loop over the real `local_state` law.
#[test]
fn a_saved_macro_table_round_trips_through_benilla_macros() {
    let _l = ENV_LOCK.lock().unwrap();
    let tmp = std::env::temp_dir().join(format!("benilla-macros-{}", std::process::id()));
    std::fs::remove_dir_all(&tmp).ok();
    let _c = EnvGuard::unset("WOW_CAPTURE");
    let _h = EnvGuard::set("BENILLA_HOME", tmp.to_str().unwrap());

    let account = crate::local_state::macros_account_path().unwrap();
    let character = crate::local_state::macros_character_path("Test Realm", "Probeone").unwrap();

    let state = MacroState {
        account: vec![macro_view("Ambush", "/cast Ambush\n/say pew")],
        character: vec![macro_view("Charge", "/cast Charge")],
    };
    crate::local_state::write_atomic(&account, &super::store::write(&state.account)).unwrap();
    crate::local_state::write_atomic(&character, &super::store::write(&state.character)).unwrap();

    // The file on disk is the reference's own shape — readable, hand-editable, and the exact
    // format a vanilla `macros-cache.txt` already has.
    assert_eq!(
        std::fs::read_to_string(&account).unwrap(),
        "MACRO 1 \"Ambush\" Ability_Ambush\n/cast Ambush\n/say pew\nEND\n"
    );

    let back = MacroState {
        account: super::store::parse(&std::fs::read_to_string(&account).unwrap()),
        character: super::store::parse(&std::fs::read_to_string(&character).unwrap()),
    };
    assert_eq!(back, state);
    std::fs::remove_dir_all(&tmp).ok();
}

/// A capture run is hermetic (decision 0954): both paths resolve to `None`, so a macro edit during
/// a capture is session-only and nothing is written under anyone's install.
#[test]
fn a_capture_run_persists_nothing() {
    let _l = ENV_LOCK.lock().unwrap();
    let _h = EnvGuard::set("BENILLA_HOME", "/tmp/benilla-should-not-exist");
    let _c = EnvGuard::set("WOW_CAPTURE", "ui-macro");
    assert_eq!(crate::local_state::macros_account_path(), None);
    assert_eq!(crate::local_state::macros_character_path("R", "C"), None);
}

/// The real UI, so the WHOLE route is under test: `run_macro` fires `EXECUTE_CHAT_LINE`, the
/// shipped ChatFrame1 is registered for it, and its handler calls `SubmitChatInput`. A bare VM
/// would pass this by accident under the old direct-push shape and silently prove nothing under
/// this one.
fn ui() -> UiScript {
    let mut s = UiScript::new().unwrap();
    s.set_screen_size(1024.0, 768.0);
    let failures = crate::ui_script::load_default_ui(&s);
    assert!(failures.is_empty(), "manifest load errors: {failures:#?}");
    s
}

/// The runner delivers every body line through the reference's own door — one
/// `EXECUTE_CHAT_LINE` event per non-empty line, in order, landing in the chat-input queue via
/// ChatFrame1's handler (0996, wow-re `macro-execution-law.md` §4). That is what makes `/cast`,
/// `/target`, `/script`, the chat types and the 225 emotes all work in a macro without the runner
/// knowing any of them.
#[test]
fn running_a_macro_queues_its_lines_as_chat_input() {
    let mut s = ui();
    s.set_macros(MacroState {
        account: vec![macro_view(
            "Ambush",
            "/cast Ambush\n\n  /say pew  \n/target Bob",
        )],
        character: Vec::new(),
    });

    assert!(super::run_macro(&mut s, 1));
    assert_eq!(
        s.take_chat_input(),
        vec!["/cast Ambush", "/say pew", "/target Bob"],
        "blank lines dropped, each line trimmed, order kept"
    );

    // An empty macro and an empty slot both run nothing and queue nothing.
    s.set_macros(MacroState {
        account: vec![macro_view("Blank", "   \n\n")],
        character: Vec::new(),
    });
    assert!(!super::run_macro(&mut s, 1));
    assert!(!super::run_macro(&mut s, 7));
    assert!(s.take_chat_input().is_empty());
}

/// A CHARACTER-range macro runs by its own index — the second half of the space is not a special
/// case anywhere in the runner.
#[test]
fn a_character_macro_runs_by_its_own_index() {
    let mut s = ui();
    s.set_macros(MacroState {
        account: Vec::new(),
        character: vec![macro_view("Charge", "/cast Charge")],
    });
    assert!(super::run_macro(&mut s, 19));
    assert_eq!(s.take_chat_input(), vec!["/cast Charge"]);
}

/// The seed→dirty→save contract the plugin's two systems rest on: the app's own load must not look
/// like a change (or every login would rewrite the file), and every script mutation must.
#[test]
fn the_dirty_edge_distinguishes_a_load_from_an_edit() {
    let mut s = UiScript::new().unwrap();
    s.set_macros(MacroState {
        account: vec![macro_view("Ambush", "/cast Ambush")],
        character: Vec::new(),
    });
    assert!(
        !s.take_macros_dirty(),
        "loading from disk is not an edit — a save here would be a write-back loop"
    );

    s.run(r#"EditMacro(1, nil, nil, "/cast Backstab")"#)
        .unwrap();
    assert!(s.take_macros_dirty());
    assert_eq!(s.macros().account[0].body, "/cast Backstab");
}

/// The generation counter is the per-frame consumers' gate (the action bar's identity feed): it
/// moves on a seed AND on every mutation, and is never consumed by reading it.
#[test]
fn the_generation_moves_on_every_write_and_is_not_drained() {
    let mut s = UiScript::new().unwrap();
    let at_start = s.macros_generation();
    assert_eq!(s.macros_generation(), at_start, "reading never drains it");

    s.set_macros(MacroState {
        account: vec![macro_view("Ambush", "/cast Ambush")],
        character: Vec::new(),
    });
    let after_seed = s.macros_generation();
    assert_ne!(after_seed, at_start, "a seed changes the bar's icons too");

    s.run(r#"EditMacro(1, "Renamed", 1)"#).unwrap();
    assert_ne!(s.macros_generation(), after_seed);
}

/// **An addon that registers `EXECUTE_CHAT_LINE` sees macro lines** — the behaviour benilla gained
/// by firing the reference's event instead of calling its own drain (0996). In 1.12 this is not a
/// courtesy: the event is the entire mechanism, and ChatFrame1's registration is just the default
/// UI's use of it.
#[test]
fn a_registered_frame_sees_every_macro_line_as_an_event() {
    let mut s = ui();
    s.run(
        r#"MacroSpy = CreateFrame("Frame")
           MacroSpy.seen = {}
           MacroSpy:RegisterEvent("EXECUTE_CHAT_LINE")
           MacroSpy:SetScript("OnEvent", function()
               table.insert(MacroSpy.seen, arg1)
           end)"#,
    )
    .unwrap();
    s.set_macros(MacroState {
        account: vec![macro_view("Pull", "/cast Charge\n/say Incoming!")],
        character: Vec::new(),
    });

    assert!(super::run_macro(&mut s, 1));
    assert_eq!(
        s.eval::<(String, String, i64)>(
            "return MacroSpy.seen[1], MacroSpy.seen[2], table.getn(MacroSpy.seen)"
        )
        .unwrap(),
        ("/cast Charge".into(), "/say Incoming!".into(), 2),
        "each line arrives as arg1 of its own event, in body order"
    );
    // …and the same lines still reach the chat grammar: the spy is an observer, not a diversion.
    assert_eq!(s.take_chat_input(), vec!["/cast Charge", "/say Incoming!"]);
    assert!(s.errors().is_empty(), "script errors: {:?}", s.errors());
}

/// The reference's tokenizer takes `"\r\n"` as a delimiter SET — either character splits a line
/// (wow-re `macro-execution-law.md` §3). A body carrying lone `\r`s (an old-Mac hand edit, or a
/// file round-tripped through one) is therefore three lines, not one long one.
#[test]
fn either_line_ending_splits_a_body() {
    let mut s = ui();
    s.set_macros(MacroState {
        account: vec![macro_view("Mixed", "/cast Charge\r/say a\r\n/say b")],
        character: Vec::new(),
    });
    assert!(super::run_macro(&mut s, 1));
    assert_eq!(
        s.take_chat_input(),
        vec!["/cast Charge", "/say a", "/say b"],
        "\\r alone splits; \\r\\n does not leave an empty token"
    );
}
