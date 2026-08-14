//! **Leaving** — the app half of the game menu's Logout and Exit Game buttons (decision 0674):
//! the outbound request, the server's answer, the countdown dialog that narrates it, and the
//! process exit.
//!
//! The whole arc is three packets and one rule: **the server owns the clock, the client only
//! narrates it.** `Logout()` sends `CMSG_LOGOUT_REQUEST`; vmangos
//! (`WorldSession::HandleLogoutRequestOpcode`, read at MiscHandler.cpp:284) answers
//! `SMSG_LOGOUT_RESPONSE {u32 reason, u8 instant}` and that one packet decides everything:
//!
//! - `reason != 0` — **refused**, no logout starts (1 in combat, 3 jumping/falling, 2 GM-frozen).
//! - `reason == 0, instant == 1` — logged out on the spot (resting in an inn or a city, on a taxi,
//!   or a GM-level account — vmangos's `CONFIG_UINT32_INSTANT_LOGOUT`); `SMSG_LOGOUT_COMPLETE`
//!   follows immediately and there is no dialog at all.
//! - `reason == 0, instant == 0` — a **20-second server-side timer** now runs, during which the
//!   character sits and is rooted. This is the case the CAMP dialog counts down, and the case
//!   `CancelLogout()` (`CMSG_LOGOUT_CANCEL` → `SMSG_LOGOUT_CANCEL_ACK`) calls off.
//!
//! Quit is Logout plus one bit of local intent: end the *process* on completion instead of falling
//! back to character select. It is the same wire, the same clock, the same dialog under a different
//! name (QUIT_TIMER "%d %s until exit"), which is why the reference gives QUIT an "Exit now" button
//! and CAMP none — a force-quit needs no server round trip, a force-logout does.
//!
//! **Pinned vs inferred.** The wire shape is verified twice over: vmangos's packet
//! (`WorldPackets::Misc::LogoutResponse` — `u32 reason, u8 instant`) and the real client's own
//! handler (wow-re `system/net/ledger.tsv` 0x5b4630, `Handle(0x4c) — {u32, u8}`). The 1.12
//! FrameXML fixes the UI half exactly: the CAMP/QUIT dialogs, their 20 s timeouts, and their
//! `PLAYER_CAMPING` / `PLAYER_QUITING` / `LOGOUT_CANCEL` drivers (UIParent.lua l.304-315, event
//! ids 276/277/278 in wow-re's `re/events/event-catalog.tsv`). What is **INFERRED** is the join
//! between them — that the client fires PLAYER_CAMPING off exactly this packet's `instant == 0` —
//! because the 0x4c handler's body isn't RE'd. It is the only wiring consistent with what the
//! reference client observably does (no dialog when you camp in an inn, a 20 s dialog in the
//! field, matching the server's own timer), and the falsification is cheap: a dialog that appears
//! on an inn logout, or none in the field, means the trigger is elsewhere. The refusal string is
//! inferred the same way (see [`ERR_LOGOUT_FAILED`]).

use benilla_ui::script::{ScriptValue, SessionRequest, UiScript};
use bevy::prelude::*;

use crate::net::{ClientCommand, LoggedOutMessage, NetCommands, SelfGuid};
use crate::ui_script::UiInput;

/// The reference's own refusal line (GlobalStrings.lua `ERR_LOGOUT_FAILED`), shown when the server
/// answers a non-zero reason. **INFERRED**: which of 1.12's several logout-failure strings the real
/// client picks per reason code isn't RE'd, and vmangos's three reasons (combat / falling / frozen)
/// have no distinct strings in the ERR_ table — this generic one covers all three, and is the line
/// vanilla is remembered for. `PLAYER_LOGOUT_FAILED_ERROR` ("…because you can't sit down right
/// now") is the other candidate and would be the correction if a capture ever shows it.
const ERR_LOGOUT_FAILED: &str = "You can't logout now.";

/// What the wire told us to say next — drained by [`feed_logout`] into script events. A queue
/// rather than a flag: a cancel can land in the same frame as the response it cancels, and the UI
/// must see both, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogoutSignal {
    /// A non-instant logout started — `PLAYER_CAMPING` (the CAMP dialog).
    Camping,
    /// The same, for a pending Quit — `PLAYER_QUITING` (the QUIT dialog).
    Quiting,
    /// The server dropped the pending logout — `LOGOUT_CANCEL` (both dialogs hide).
    Cancelled,
    /// The server refused outright — a red error line, no dialog.
    Refused,
}

/// The pending session-exit, and the signals owed to the UI.
///
/// `quitting` is the ONE piece of local state the whole arc needs: the wire cannot tell a logout
/// from a quit (both are `CMSG_LOGOUT_REQUEST`), so which dialog to show and whether to exit the
/// process at the end is remembered here. It is cleared by every terminal edge — a refusal, a
/// cancel, or the exit itself — so a cancelled quit can never leave a client that dies on the next
/// unrelated logout.
#[derive(Resource, Default)]
pub(crate) struct LogoutState {
    quitting: bool,
    signals: Vec<LogoutSignal>,
}

impl LogoutState {
    /// `SMSG_LOGOUT_RESPONSE` — the whole decision table (module docs). Returns nothing; the
    /// instant case is silent on purpose (the completion is already on its way).
    pub(crate) fn apply_response(&mut self, reason: u32, instant: bool) {
        // Announced: this one packet decides the entire user-visible outcome (dialog, no dialog, or
        // a red line), and the three inputs behind it — combat, resting, account security — are all
        // invisible from this side. A logout that "did nothing" is diagnosed from this line.
        info!("logout: server answered reason={reason} instant={instant}");
        if reason != 0 {
            self.quitting = false;
            self.signals.push(LogoutSignal::Refused);
            return;
        }
        if instant {
            return;
        }
        self.signals.push(if self.quitting {
            LogoutSignal::Quiting
        } else {
            LogoutSignal::Camping
        });
    }

    /// `SMSG_LOGOUT_CANCEL_ACK` — the countdown is off.
    pub(crate) fn apply_cancelled(&mut self) {
        self.quitting = false;
        self.signals.push(LogoutSignal::Cancelled);
    }
}

/// Fire the camp/quit events the dialogs are driven by (GameMenuFrame.xml's `BenillaLogoutDriver`).
fn feed_logout(script: Option<NonSendMut<UiScript>>, mut logout: ResMut<LogoutState>) {
    let Some(mut script) = script else {
        return;
    };
    for signal in std::mem::take(&mut logout.signals) {
        match signal {
            LogoutSignal::Camping => script.fire_event("PLAYER_CAMPING", Vec::new()),
            LogoutSignal::Quiting => script.fire_event("PLAYER_QUITING", Vec::new()),
            LogoutSignal::Cancelled => script.fire_event("LOGOUT_CANCEL", Vec::new()),
            LogoutSignal::Refused => script.fire_event(
                "UI_ERROR_MESSAGE",
                vec![ScriptValue::Str(ERR_LOGOUT_FAILED.into())],
            ),
        }
    }
}

/// Turn the Lua intents into packets — and, for the two force paths, straight into an exit.
fn drain_logout(
    script: Option<NonSendMut<UiScript>>,
    mut logout: ResMut<LogoutState>,
    self_guid: Res<SelfGuid>,
    commands: Res<NetCommands>,
    mut exit: MessageWriter<AppExit>,
    mut reload: ResMut<crate::ui_script::ReloadUiPending>,
) {
    let Some(mut script) = script else {
        return;
    };
    for request in script.take_session_requests() {
        match request {
            SessionRequest::Logout | SessionRequest::Quit => {
                logout.quitting = request == SessionRequest::Quit;
                // Not in the world (or the socket is gone): there is nothing to log out OF, so a
                // quit is just an exit and a logout is a no-op. The reference does the same — its
                // Quit() at the glue screen closes the process outright.
                if self_guid.0.is_none() || commands.0.send(ClientCommand::Logout).is_err() {
                    if logout.quitting {
                        info!("logout: quit with no world session — exiting");
                        exit.write(AppExit::Success);
                    } else {
                        warn!("logout: no world session — logout dropped");
                        logout.quitting = false;
                    }
                } else {
                    // Announced because what happens next is entirely the server's call — instant
                    // or a 20 s countdown — and a silent request looks identical to a dropped one.
                    info!(
                        "logout: requested (quitting={}) — awaiting SMSG_LOGOUT_RESPONSE",
                        logout.quitting
                    );
                }
            }
            SessionRequest::CancelLogout => {
                // The dialog's own OnHide fires this on EVERY early close, including one caused by
                // a cancel that already landed — so a stray send with nothing pending is normal and
                // harmless (vmangos ignores it). `quitting` clears here rather than on the ack so a
                // cancel with a dead socket still can't leave a quit armed.
                logout.quitting = false;
                let _ = commands.0.send(ClientCommand::LogoutCancel);
            }
            SessionRequest::ForceQuit => {
                info!("logout: force quit");
                exit.write(AppExit::Success);
            }
            // Not this module's business beyond routing: the rebuild itself is
            // [`crate::ui_script::run_pending_reload`]'s, at the top of the next frame.
            SessionRequest::ReloadUi => reload.0 = true,
        }
    }
}

/// The completion edge: `SMSG_LOGOUT_COMPLETE` normally lands us at character select (decision
/// 0193), but a logout that began as Exit Game ends the process instead.
fn exit_on_logout_complete(
    mut logged_out: MessageReader<LoggedOutMessage>,
    mut logout: ResMut<LogoutState>,
    mut exit: MessageWriter<AppExit>,
) {
    if logged_out.read().count() == 0 {
        return;
    }
    if logout.quitting {
        logout.quitting = false;
        info!("logout: world session ended — exiting");
        exit.write(AppExit::Success);
    }
}

pub struct UiLogoutPlugin;

impl Plugin for UiLogoutPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LogoutState>().add_systems(
            Update,
            (
                feed_logout.before(UiInput),
                drain_logout.after(UiInput),
                exit_on_logout_complete.after(UiInput),
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The response table (module docs): refusal speaks and disarms, instant is silent, and the
    /// slow path picks its dialog from the pending intent.
    #[test]
    fn the_response_decides_which_dialog_if_any() {
        let mut s = LogoutState::default();
        s.apply_response(0, true);
        assert!(s.signals.is_empty(), "an instant logout shows no dialog");

        s.apply_response(0, false);
        assert_eq!(s.signals, vec![LogoutSignal::Camping]);

        let mut s = LogoutState {
            quitting: true,
            signals: Vec::new(),
        };
        s.apply_response(0, false);
        assert_eq!(s.signals, vec![LogoutSignal::Quiting], "a quit says so");

        // In combat: the red line, and the pending quit is disarmed — otherwise the NEXT logout,
        // minutes later and deliberate, would silently kill the process instead.
        let mut s = LogoutState {
            quitting: true,
            signals: Vec::new(),
        };
        s.apply_response(1, false);
        assert_eq!(s.signals, vec![LogoutSignal::Refused]);
        assert!(!s.quitting);
    }

    /// The cancel ack disarms a quit too — the same "no armed quit survives a terminal edge" rule.
    #[test]
    fn a_cancel_disarms_a_pending_quit() {
        let mut s = LogoutState {
            quitting: true,
            signals: Vec::new(),
        };
        s.apply_cancelled();
        assert_eq!(s.signals, vec![LogoutSignal::Cancelled]);
        assert!(!s.quitting);
    }
}
