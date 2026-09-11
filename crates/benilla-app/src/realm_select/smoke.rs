//! **The realm-list boundary smoke** (`WOW_REALM_SMOKE=1`, run by `scripts/smoke.sh`) — the
//! scripted walk that reproduces the failure this subsystem shipped with, so it can never come
//! back silently.
//!
//! What broke, and why nothing caught it: the realm list was a `ClientState`, and the roster that
//! arrives after Okay only moved the app off `Login`. So Okay published the pick, the IO thread
//! entered the realm and walked on to the character park — and the app sat on the realm screen,
//! looking exactly as if the button had done nothing. The next Cancel then sent its answer to a
//! park two ahead of the thread, where nobody was listening, and every later login queued behind
//! it: the client hung on "Connecting" with **nothing in the log**. Three screens' worth of
//! symptoms, one missing transition, and no test in the workspace could see any of it, because
//! every one of them models the app or the thread and the bug lived in the seam.
//!
//! So this leg drives the seam, against a real server:
//!
//! 1. from character select, **Change Realm** — the dialog goes up *over* the screen, which stays;
//! 2. **Okay** on the realm we are already on — a real world re-dial, and the roster has to come
//!    back with the dialog down and the screen still character select (the leg that was broken);
//! 3. **Change Realm again, then Cancel** — the dialog goes down and the session underneath is
//!    untouched, which is the whole of `RealmList_OnCancel`;
//! 4. one more round trip through the park to prove the thread is still where the app thinks it
//!    is: a second Okay that has to land.
//!
//! Inert without the env.

use bevy::prelude::*;

use crate::char_select::{ClientState, Roster};
use crate::net::{RealmChoice, RealmRequest};

use super::Realms;

/// How long each leg may take before the walk gives up and says which one stalled.
const LEG_TIMEOUT: f32 = 25.0;
/// How long the walk waits to REACH character select before it says why it never started.
///
/// The whole login is inside this — dial, auth, realm list, roster — which is why it is not
/// [`LEG_TIMEOUT`], and why it exists at all: leg 0 was the one leg with no predecessor to name
/// it, so a walk that never reached the roster screen said nothing and ran out the caller's own
/// timeout instead. `scripts/smoke.sh` then reported "the realm walk did not finish within the
/// timeout", which is true of every possible failure and a cause for none of them.
const ENTRY_TIMEOUT: f32 = 60.0;
/// A beat with the dialog up, so a human watching sees it and a stuck frame has time to show.
const DWELL: f32 = 1.0;

#[derive(Default)]
pub(super) struct Walk {
    phase: u8,
    mark: f32,
    /// The roster generation we are waiting to move past — a realm entered is a roster replaced.
    rosters: u32,
    seen: u32,
    /// The one-shot startup refusal has run.
    armed: bool,
}

/// The walk. Every leg names itself on the way in, so a timeout says which boundary stalled
/// instead of "the client did not exit".
#[allow(clippy::too_many_arguments)]
pub(super) fn debug_realm_smoke(
    mut msgs: MessageReader<crate::net::CharListMessage>,
    realms: ResMut<Realms>,
    roster: Res<Roster>,
    state: Res<State<ClientState>>,
    choice: Res<RealmChoice>,
    time: Res<Time>,
    mut walk: Local<Walk>,
    mut exit: MessageWriter<AppExit>,
) {
    walk.seen += msgs.read().count() as u32;
    if std::env::var_os("WOW_REALM_SMOKE").is_none() || walk.phase == u8::MAX {
        return;
    }
    let now = time.elapsed_secs();
    let at_select = *state.get() == ClientState::CharSelect && !roster.chars.is_empty();
    let mut fail = |why: &str| {
        error!("realm-smoke: FAILED — {why}");
        exit.write(AppExit::error());
    };
    // **The walk needs the roster SCREEN, and two envs structurally deny it** — so refuse here,
    // on the first frame, instead of spending a login and a timeout discovering it. `WOW_CHAR` is
    // the enter-the-world fast path and `WOW_RIG` seats its own derived body; either one answers
    // the very roster this walk stands on, and neither can be waited out.
    //
    // Inherited rather than typed is the case worth naming. `scripts/smoke.sh` runs this leg out
    // of the same shell as everything else and now scrubs `WOW_*` before it starts — but this
    // check is the guard for the leg itself, not for that one caller: an agent reproducing a
    // realm-list report by hand types `WOW_REALM_SMOKE=1 cargo run` into a shell that has been
    // exporting probe credentials all session, and gets told why in the first second.
    if !walk.armed {
        walk.armed = true;
        if let Some((key, value)) = ["WOW_CHAR", "WOW_RIG"]
            .into_iter()
            .find_map(|k| std::env::var_os(k).map(|v| (k, v)))
        {
            fail(&format!(
                "{key}={} seats a body — this walk drives CHARACTER SELECT and there is no roster \
                 screen to drive once one is seated. Unset it (`scripts/smoke.sh` does).",
                value.to_string_lossy(),
            ));
            walk.phase = u8::MAX;
            return;
        }
    }
    if walk.phase == 0 && now - walk.mark > ENTRY_TIMEOUT {
        fail(&format!(
            "never reached character select in {ENTRY_TIMEOUT:.0}s (state {:?}, {} characters, \
             {} realms) — nothing was driven, so the fault is upstream of the walk",
            state.get(),
            roster.chars.len(),
            realms.realms.len(),
        ));
        walk.phase = u8::MAX;
        return;
    }
    if walk.phase > 0 && now - walk.mark > LEG_TIMEOUT {
        fail(&format!(
            "leg {} stalled for {LEG_TIMEOUT:.0}s (state {:?}, dialog {}, rosters {}/{})",
            walk.phase,
            state.get(),
            if realms.shown { "up" } else { "down" },
            walk.seen,
            walk.rosters,
        ));
        walk.phase = u8::MAX;
        return;
    }

    match walk.phase {
        // ── 1 · Change Realm: the dialog goes up over the screen. ────────────────────────────
        0 if at_select && !realms.realms.is_empty() => {
            info!("realm-smoke: at character select → Change Realm");
            open(realms, &choice);
            (walk.phase, walk.mark) = (1, now);
        }
        1 if realms.shown && now - walk.mark > DWELL => {
            if *state.get() != ClientState::CharSelect {
                fail("the dialog replaced the character screen instead of standing over it");
                walk.phase = u8::MAX;
                return;
            }
            info!("realm-smoke: dialog up, character select still underneath → Okay");
            okay(realms, &choice);
            (walk.phase, walk.mark, walk.rosters) = (2, now, walk.seen + 1);
        }
        // ── 2 · Okay has to land: a fresh roster, dialog down, still at select. ──────────────
        2 if walk.seen >= walk.rosters && !realms.shown && at_select => {
            info!("realm-smoke: Okay landed — fresh roster at character select → Change Realm");
            open(realms, &choice);
            (walk.phase, walk.mark) = (3, now);
        }
        // ── 3 · Cancel changes nothing. ──────────────────────────────────────────────────────
        3 if realms.shown && now - walk.mark > DWELL => {
            info!("realm-smoke: dialog up → Cancel");
            cancel(realms, &choice);
            (walk.phase, walk.mark) = (4, now);
        }
        4 if !realms.shown && now - walk.mark > DWELL => {
            if !at_select {
                fail("Cancel left character select instead of just hiding the dialog");
                walk.phase = u8::MAX;
                return;
            }
            // ── 4 · And the park is still where the app thinks it is. ────────────────────────
            info!("realm-smoke: Cancel kept the session → one more Change Realm + Okay");
            open(realms, &choice);
            (walk.phase, walk.mark) = (5, now);
        }
        5 if realms.shown && now - walk.mark > DWELL => {
            okay(realms, &choice);
            (walk.phase, walk.mark, walk.rosters) = (6, now, walk.seen + 1);
        }
        6 if walk.seen >= walk.rosters && !realms.shown && at_select => {
            info!(
                "realm-smoke: done — {} roster(s), {} char(s) at character select",
                walk.seen,
                roster.chars.len()
            );
            exit.write(AppExit::Success);
            walk.phase = u8::MAX;
        }
        _ => {}
    }
}

/// The character screen's Change Realm button, as the walk presses it.
fn open(mut realms: ResMut<Realms>, choice: &RealmChoice) {
    super::open_over_char_select(&mut realms, choice);
}

/// `RealmList_OnOk` on whatever the dialog opened with highlighted, or the first row.
fn okay(mut realms: ResMut<Realms>, choice: &RealmChoice) {
    let name = realms.selected().map(|r| r.name.clone()).or_else(|| {
        realms
            .rows()
            .first()
            .map(|&i| realms.realms[i].name.clone())
    });
    let Some(name) = name else {
        return;
    };
    realms.hide();
    let _ = choice.0.try_send(RealmRequest::Enter(name));
}

/// `RealmList_OnCancel`.
fn cancel(mut realms: ResMut<Realms>, choice: &RealmChoice) {
    realms.hide();
    let _ = choice.0.try_send(RealmRequest::Abandon);
}
