//! The login screen (decision 0539) — the faithful `AccountLogin` glue, functional core only:
//! the `UI_MainMenu` scene with its authored fog/fires, the account/password boxes, Remember
//! Account Name, Login/Quit, the version block, and the connecting/error dialogs. The Credits/
//! Cinematics/TOS side of the reference screen is deliberately cut (the director's call).
//!
//! This module owns the **credential policy** — the 0193 §3 mirror for the IO thread's pre-logon
//! park: the env fast path (any of `WOW_USER`/`WOW_PASS`/`WOW_CHAR` explicitly set auto-submits
//! with the old `one`/`pone` defaults, so every probe/smoke invocation keeps working), the
//! pending-credentials resubmit (paced at the flat 3 s, app-side — the IO thread never sleeps),
//! and the director's typed submit. A *refused* code (bad password) clears the intent and shows
//! the authored `AUTH_*` dialog — never an auto-retry against a refusal.
//!
//! **A session that is lost is over** (decision 1262): the reference's `GlueParent.lua` answers
//! `DISCONNECTED_FROM_SERVER` with `SetGlueScreen("login")` + `GlueDialog_Show("DISCONNECTED")`,
//! and so does this. 0065's seamless reconnect survives only where nobody is here to type — an
//! unattended run ([`crate::run_mode::unattended_login`]) — because a client that
//! re-authenticates on its own takes the account back off whoever just displaced it.
//!
//! Module split: this file (state, policy, input, dialogs, the saved-account persistence),
//! [`screen`] (the authored layout, transcribed from `AccountLogin.xml`), [`smoke`] (the
//! `WOW_LOGIN_SMOKE` headless prover).

mod queue;
mod screen;
mod smoke;

use std::sync::atomic::Ordering;

use benilla_ui::widget::EditBoxState;
use bevy::input::keyboard::KeyboardInput;

use crate::textinput::{self, HostClipboard};
use bevy::input::ButtonState;
use bevy::prelude::*;

use benilla_protocol::{DialFailure, LoginRefusal, LoginStage};

use crate::char_select::ClientState;
use crate::glue_strings::GlueStrings;
use crate::net::{
    CharListMessage, DisconnectedMessage, LoginAbandon, LoginFailedMessage, LoginQueuedMessage,
    LoginRequest, LoginStageMessage, LoginSubmit,
};
use crate::portrait::{GluePreview, GlueScene};
use crate::sound::GlueSound;

pub(crate) use screen::LoginAction;
pub(crate) use smoke::smoke_character;

/// The flat resubmit pacing after a transport failure with pending credentials (decision 0065's
/// reconnect cadence, moved app-side by 0539 — the IO thread never sleeps).
const RETRY_DELAY_SECS: f32 = 3.0;
/// The quit grace: `gsTitleQuit` gets this long to be audible before `AppExit` drops the mixer.
const QUIT_GRACE_SECS: f32 = 0.4;
/// The ref's `letters="16"` on both edit boxes.
const MAX_LETTERS: usize = 16;

pub(crate) struct LoginPlugin;

impl Plugin for LoginPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LoginIntent>()
            .init_resource::<LoginForm>()
            .init_resource::<LoginDialog>()
            .add_systems(OnEnter(ClientState::Login), enter_login)
            .add_systems(OnExit(ClientState::Login), screen::exit_login)
            .add_systems(
                Update,
                (
                    // Policy + transitions run in EVERY state: the reconnect resubmit fires while
                    // `InWorld`, and the roster edge lands wherever it lands.
                    (drive_policy, to_select_on_roster, drive_quit).chain(),
                    (
                        screen::materialize_screen,
                        login_input,
                        tick_login_caret,
                        screen::refresh_boxes,
                        screen::refresh_checkbox,
                        drive_dialog,
                        // Both after `drive_dialog`: it is what spawns the dialog's edit box, and
                        // what a realmlist Okay changes the address in.
                        (screen::refresh_dialog_box, screen::refresh_realmlist),
                        crate::glue::art_swaps,
                        crate::glue::glue_button_visuals,
                        crate::glue::sync_outlines,
                    )
                        .chain()
                        .run_if(in_state(ClientState::Login)),
                    (smoke::debug_login_smoke, screen::debug_login_shot),
                )
                    .chain()
                    .after(benilla_world::schedule::WorldStage::Net),
            );
    }
}

// ── The credential policy ────────────────────────────────────────────────────────────────────────

/// Where the IO thread's read loop currently is, as far as the app can tell — the policy submits
/// credentials only while it's parked pre-logon.
#[derive(Default, PartialEq, Eq, Clone, Copy)]
enum IoPark {
    /// Parked at the pre-logon park (boot, a failure, a disconnect, a Back).
    #[default]
    AtLogin,
    /// Past logon — parked at select or streaming the world.
    Active,
}

/// The credential policy's memory (the 0193 §3 mirror): the last credentials this session
/// authenticated (or asked) with, the in-flight/park bookkeeping, and the resubmit timer.
#[derive(Resource, Default)]
pub(crate) struct LoginIntent {
    /// The session's credentials — kept while in-world so the logout relist and an unattended
    /// run's reconnect re-authenticate silently (0065); cleared by select's Back, a refusal code,
    /// a Cancel, and by a lost session (1262 — they are the session's, and the session is over).
    creds: Option<(String, String)>,
    /// A submit is in flight (between our send and its LoginFailed/CharacterList answer).
    in_flight: bool,
    /// Whether the in-flight submit came from the screen (it announced a connecting dialog and
    /// wants its failure surfaced) or from the silent auto path.
    announced: bool,
    park: IoPark,
    /// `Time::elapsed_secs` deadline for the next silent resubmit (`None` = no retry scheduled).
    retry_at: Option<f32>,
    /// Env fast path read latch (checked once, on the first policy run).
    env_read: bool,
}

impl LoginIntent {
    /// Forget the session's credentials and any scheduled retry (select's Back, a refusal).
    pub(crate) fn clear(&mut self) {
        self.creds = None;
        self.retry_at = None;
    }

    /// The account this session authenticated as, however it got there — the env fast path or the
    /// login screen. The one honest answer to "whose body is this?", which is what decides whether
    /// the probe shield has any business touching it (decision 0677).
    pub(crate) fn account(&self) -> Option<&str> {
        self.creds.as_ref().map(|(user, _)| user.as_str())
    }
}

/// **Everything one login attempt is made of**, as a single [`SystemParam`]: the policy's memory,
/// the channel to the parked IO thread, the abandon generation a Cancel bumps, and — since
/// decision 1667 — the realmlist it dials.
///
/// A bundle rather than four parameters, for `cvars::KnobParams`' reason: adding the realmlist put
/// [`login_input`] at **seventeen** parameters, one past Bevy's ceiling, and the three systems
/// that submit were already re-typing the same four names. Now a submit is one call on one param,
/// and the next thing an attempt needs is one field here instead of a fourth signature to widen.
#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct Attempt<'w> {
    pub(super) intent: ResMut<'w, LoginIntent>,
    submit: Res<'w, LoginSubmit>,
    abandon: Res<'w, LoginAbandon>,
    realmlist: Res<'w, crate::realmlist::Realmlist>,
}

impl Attempt<'_> {
    /// Send one login attempt to the parked IO thread, stamped with the current abandon
    /// generation.
    ///
    /// The realmlist is read **at submit time** (decision 1667) rather than by the IO thread, so a
    /// resubmit fired after the player repointed the client dials the new server while an attempt
    /// already on the wire keeps the one it started with.
    fn send(&mut self, user: &str, pass: &str, announced: bool) {
        self.intent.creds = Some((user.to_string(), pass.to_string()));
        self.intent.in_flight = true;
        self.intent.announced = announced;
        self.intent.retry_at = None;
        let _ = self.submit.0.send(LoginRequest {
            user: user.to_string(),
            pass: pass.to_string(),
            host: self.realmlist.address().to_string(),
            generation: self.abandon.0.load(Ordering::SeqCst),
        });
    }
}

/// The millisecond clock the queue ring is stamped with — the app's own elapsed time, which is
/// what the reference's `GetTickCount` is here. Only differences matter to the estimate, so an
/// arbitrary epoch is fine; `u32` keeps the wrapping arithmetic the same width the reference used.
fn queue_now_ms(time: &Time) -> u32 {
    time.elapsed().as_millis() as u32
}

/// What a Login press should do — the reference's two guards, as a verdict.
///
/// Extracted from [`login_input`] so the validation can be tested without standing a screen up,
/// and so the click sound has somewhere to live that is not inside one of its branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginPress {
    NeedAccount,
    NeedPassword,
    Submit,
}

/// The account box is checked first, so a wholly empty form asks for the account name — which is
/// the order the reference's dialogs come in.
fn login_press(form: &LoginForm) -> LoginPress {
    if form.account.text.is_empty() {
        LoginPress::NeedAccount
    } else if form.password.text.is_empty() {
        LoginPress::NeedPassword
    } else {
        LoginPress::Submit
    }
}

/// The `LOGIN_STATE_*` glue string for a stage (the connecting dialog's text).
fn stage_text(strings: &GlueStrings, stage: LoginStage) -> &str {
    match stage {
        LoginStage::Connecting => strings.text("LOGIN_STATE_CONNECTING", "Connecting"),
        LoginStage::Authenticating => strings.text("LOGIN_STATE_AUTHENTICATING", "Authenticating"),
        LoginStage::Handshaking => strings.text("LOGIN_STATE_HANDSHAKING", "Handshaking"),
    }
}

/// The authored failure string for an auth result byte (the vmangos-verified map, decision 0539
/// §6): each row is the client's own `GlueStrings` text; a transport failure (`None`) reads
/// `LOGIN_FAILED` ("Unable to connect").
///
/// **A dial that never opened a socket answers ahead of all of them**, because it is the only
/// failure the player can act on and — since 1667 made the address editable — the only one they can
/// misread. "Unable to connect" cannot distinguish *that name does not exist* from *the address is
/// fine and nothing is listening*, so a player whose server is simply down edits a correct address
/// until they give up. That is what the editor's first live use produced. Both replacement strings
/// are the reference's own (`AUTH_LOGIN_SERVER_NOT_FOUND` / `LOGIN_SERVER_DOWN`, so they localize);
/// the address line under them is ours, because the reference never had an address to get wrong.
fn fail_text(
    strings: &GlueStrings,
    refusal: Option<LoginRefusal>,
    dial: Option<&DialFailure>,
) -> String {
    if let Some(dial) = dial {
        let headline = if dial.unresolved {
            strings.text("AUTH_LOGIN_SERVER_NOT_FOUND", "Invalid Login Server")
        } else {
            strings.text("LOGIN_SERVER_DOWN", "Login Server Down")
        };
        return format!("{headline}\n{}", dial.address);
    }
    let authored = match refusal {
        Some(LoginRefusal::World(code)) => world_refusal_text(strings, code),
        Some(LoginRefusal::Logon(code)) => logon_refusal_text(strings, Some(code)),
        None => logon_refusal_text(strings, None),
    };
    without_dead_url(authored).into_owned()
}

/// **The world server's** `SMSG_AUTH_RESPONSE` refusal, in the client's own words.
///
/// A straight transcription of the client's own dispatch over this enum, decompiled in wow-re
/// `system/net/scratch/w2b-pack.c` — each case loads exactly the `GlueStrings` key named below,
/// and the numbering is `AuthResponseCodes` in cmangos `SharedDefines.h:1721+`. Nothing here is a
/// judgement call; where the client picks a string, so do we.
///
/// Every one of these used to arrive as a generic "Unable to connect", because the world result
/// was formatted into an error string in `WorldSession::connect` and the byte thrown away.
fn world_refusal_text(strings: &GlueStrings, code: u8) -> &str {
    use benilla_protocol::messages as m;
    let (key, fallback): (&str, &str) = match code {
        m::AUTH_FAILED => ("AUTH_FAILED", "Authentication failed"),
        m::AUTH_REJECT => ("AUTH_REJECT", "Login unavailable"),
        m::AUTH_BAD_SERVER_PROOF => ("AUTH_BAD_SERVER_PROOF", "Server is not valid"),
        m::AUTH_UNAVAILABLE => (
            "AUTH_UNAVAILABLE",
            "System unavailable - Please try again later",
        ),
        m::AUTH_SYSTEM_ERROR => ("AUTH_SYSTEM_ERROR", "System Error"),
        m::AUTH_BILLING_ERROR => ("AUTH_BILLING_ERROR", "Billing system error"),
        m::AUTH_BILLING_EXPIRED => ("AUTH_BILLING_EXPIRED", "Account billing has expired"),
        m::AUTH_VERSION_MISMATCH => ("AUTH_VERSION_MISMATCH", "Wrong client version"),
        m::AUTH_UNKNOWN_ACCOUNT => ("AUTH_UNKNOWN_ACCOUNT", "Unknown account"),
        m::AUTH_INCORRECT_PASSWORD => ("AUTH_INCORRECT_PASSWORD", "Incorrect Password"),
        m::AUTH_SESSION_EXPIRED => ("AUTH_SESSION_EXPIRED", "Session Expired"),
        m::AUTH_SERVER_SHUTTING_DOWN => ("AUTH_SERVER_SHUTTING_DOWN", "Server Shutting Down"),
        m::AUTH_ALREADY_LOGGING_IN => ("AUTH_ALREADY_LOGGING_IN", "Already Logging In"),
        m::AUTH_LOGIN_SERVER_NOT_FOUND => ("AUTH_LOGIN_SERVER_NOT_FOUND", "Invalid Login Server"),
        // The tail past the queue. These five are exactly the rows of the client's `OKAY_WITH_URL`
        // table (`0x803740`), which is why the URL dialog is reachable only from THIS enum: the
        // lookup is a linear search over a ClientServices code and realmd's results never enter it.
        // benilla shows the text without the URL button — `LaunchURL` is a shell-out we do not do,
        // and its whitelist (`0x85cc34`) is ten worldofwarcraft.com/blizzard.com domains that have
        // not served these pages in fifteen years.
        m::AUTH_BANNED => (
            "AUTH_BANNED",
            "This account has been banned for violating the Terms of Use Agreement",
        ),
        m::AUTH_ALREADY_ONLINE => ("AUTH_ALREADY_ONLINE", "This character is still logged on"),
        m::AUTH_NO_TIME => (
            "AUTH_NO_TIME",
            "Your World of Warcraft subscription has expired",
        ),
        m::AUTH_DB_BUSY => ("AUTH_DB_BUSY", "This session has timed out"),
        m::AUTH_SUSPENDED => (
            "AUTH_SUSPENDED",
            "This account has been temporarily suspended for violating the Terms of Use Agreement",
        ),
        m::AUTH_PARENTAL_CONTROL => (
            "AUTH_PARENTAL_CONTROL",
            "Access to this account has been blocked by parental controls.",
        ),
        // `AUTH_OK` never reaches here (it is the success path) and `AUTH_WAIT_QUEUE` is not a
        // refusal at all; anything else is a code this client does not know.
        _ => ("AUTH_FAILED", "Authentication failed"),
    };
    strings.text(key, fallback)
}

/// Cut the **dead web address** off the tail of an authored failure string (director's call,
/// 2026-08-28).
///
/// Every long `LOGIN_*`/`AUTH_*` string ends by pointing the player at a Blizzard page —
/// `www.worldofwarcraft.com`, `worldofwarcraft.com/misc/banned.html`. None has served anything in
/// fifteen years, and none was ever about *this* server: benilla's player is on somebody's private
/// realm, where "see www.worldofwarcraft.com for more information" is not merely stale but
/// misdirection. The rest of the sentence is still worth reading, so the address is cut at its
/// **clause**, not its sentence — `"…a lost or stolen password and account, see
/// www.worldofwarcraft.com for more information."` becomes `"…a lost or stolen password and
/// account."`
///
/// Find the first web address, walk back to the clause boundary before it (a comma or a full
/// stop), end there. **With no boundary to walk back to the string is left exactly as it is** —
/// that guard is what stops `AUTH_BANNED` ("…violating the Terms of Use Agreement -
/// www.worldofwarcraft.com/termsofuse.shtml…"), whose address sits behind a dash with no clause
/// break before it, from being trimmed away to nothing.
fn without_dead_url(text: &str) -> std::borrow::Cow<'_, str> {
    let Some(url) = ["www.", "http://", "https://"]
        .iter()
        .filter_map(|p| text.find(p))
        .min()
    else {
        return std::borrow::Cow::Borrowed(text);
    };
    let head = &text[..url];
    let Some(cut) = head.rfind([',', '.']) else {
        return std::borrow::Cow::Borrowed(text);
    };
    let kept = head[..cut].trim_end();
    if kept.is_empty() {
        return std::borrow::Cow::Borrowed(text);
    }
    std::borrow::Cow::Owned(format!("{kept}."))
}

/// The **realmd** logon-proof refusal, in the client's own words — the LONG `LOGIN_*` family.
///
/// **This table was wrong in every row until decision 1679**, and wrong in an instructive way: the
/// codes were right and the *string family* was not. Decision 0539 §6 built it by reading vmangos'
/// `AuthCodes.h`, which names the wire bytes but says nothing about what the client displays, and
/// `GlueStrings.lua` happens to define two plausible families whose keys read alike. So a bad
/// password showed the terse `AUTH_UNKNOWN_ACCOUNT` ("Unknown account") — a real reference string,
/// in the wrong slot.
///
/// VERIFIED (wow-re `system/glue/scratch/login-failure-dialogs.md`, §5 cross-checked): the client
/// keeps **two** login-status enums with **two** key tables, and they are conflated precisely
/// because both raise the dialog through `OPEN_STATUS_DIALOG`. realmd's results resolve against
/// table `0x836b78` — the long `LOGIN_*` family; the world server's resolve against `0x85cae8` —
/// the short `AUTH_*` family ([`world_refusal_text`]). The chain is grunt opcode table `0x85e278`
/// → `Logon::OnAuthResult 0x5b2c90` (byte-index table `0x5b2ea4` + jump table `0x5b2e78`) →
/// `CGlueMgr::OnLoginState 0x46b0f0` → `CGlueMgr::UpdateLoginDialog 0x46b140`.
///
/// Two consequences worth stating outright:
///
/// **0x04 and 0x05 share one jump-table arm** — byte-identical, both `LOGIN_UNKNOWN_ACCOUNT`. The
/// reference client structurally *cannot* say "wrong password" on a logon refusal, and
/// `LOGIN_INCORRECT_PASSWORD` is dead surface it never reaches. That also retires the emulator lore
/// that the client locks out after 0x05 (vmangos sends 0x04 for both to avoid it): there is no
/// lockout — no disabled button, no delay, nothing persisted. Its one counter is gated on a
/// `securityFlags` byte that is zero on vmangos and cmangos, and only escalates the same string
/// toward its `_PIN`/`_CALL` variants.
///
/// **Anything past 0x0F is clamped**, not passed through: the byte-index table saturates at 0xFF,
/// which lands on the arm that shows `DISCONNECTED`. So 0x10/0x11/0x12 and every unknown future
/// code read as a disconnect rather than as an authentication message.
fn logon_refusal_text(strings: &GlueStrings, code: Option<u8>) -> &str {
    let (key, fallback): (&str, &str) = match code {
        // A transport failure never had a code; the client's own answer for the codes that mean
        // "no usable connection" is the same string, so they share this row.
        // 0x01/0x02 (unknown0/1), 0x0B (invalid server), 0x0D (no access).
        None | Some(0x01) | Some(0x02) | Some(0x0B) | Some(0x0D) => {
            ("LOGIN_FAILED", "Unable to connect")
        }
        Some(0x03) => (
            "LOGIN_BANNED",
            "This World of Warcraft account has been closed and is no longer available for use.  \
             Please go to http://www.worldofwarcraft.com/misc/banned.html for further information. ",
        ),
        // BOTH the unknown account and the wrong password — see the doc above.
        Some(0x04) | Some(0x05) => (
            "LOGIN_UNKNOWN_ACCOUNT",
            "The information you have entered is not valid.  Please check the spelling of the \
             account name and password.  If you need help in retrieving a lost or stolen password \
             and account, see www.worldofwarcraft.com for more information.",
        ),
        Some(0x06) => (
            "LOGIN_ALREADYONLINE",
            "This account is already logged into World of Warcraft.  Please check the spelling and \
             try again.",
        ),
        Some(0x07) => (
            "LOGIN_NOTIME",
            "You have used up your prepaid time for this account. Please purchase more to continue \
             playing",
        ),
        Some(0x08) => (
            "LOGIN_DBBUSY",
            "Could not log in to World of Warcraft at this time.  Please try again later.",
        ),
        Some(0x09) => (
            "LOGIN_BADVERSION",
            "Unable to validate game version.  This may be caused by file corruption or the \
             interference of another program.  Please visit www.blizzard.com/support/wow/ for more \
             information and possible solutions to this issue.",
        ),
        Some(0x0C) => (
            "LOGIN_SUSPENDED",
            "This World of Warcraft account has been temporarily suspended.  Please go to \
             http://www.worldofwarcraft.com/misc/banned.html for further information.",
        ),
        Some(0x0F) => (
            "LOGIN_PARENTALCONTROL",
            "Access to this account has been blocked by parental controls.  Your settings may be \
             changed in your account preferences at http://www.worldofwarcraft.com.",
        ),
        // 0x0A (version update) is IGNORED on the proof (`0x5bada2`) and never reaches a dialog;
        // it only means anything on the challenge, where it is the patch-download state. Every
        // remaining code clamps to the disconnect arm.
        Some(_) => ("DISCONNECTED", "Disconnected from server"),
    };
    strings.text(key, fallback)
}

/// The policy tick + the net-message reactions. Runs in every state (the reconnect path fires
/// while `InWorld`); the screen's own submit comes through [`login_input`], which calls
/// [`send_login`] with `announced = true`.
#[allow(clippy::too_many_arguments)]
fn drive_policy(
    mut attempt: Attempt,
    mut dialog: ResMut<LoginDialog>,
    strings: Option<Res<GlueStrings>>,
    time: Res<Time>,
    mut stages: MessageReader<LoginStageMessage>,
    mut queued: MessageReader<LoginQueuedMessage>,
    mut failures: MessageReader<LoginFailedMessage>,
    mut disconnects: MessageReader<DisconnectedMessage>,
    mut exit: MessageWriter<AppExit>,
) {
    let now = time.elapsed_secs();
    // A harness run (env creds, and not the smoke — the smoke owns its own verdict) may have
    // nobody at the keyboard: a login failure no resubmit can change would leave it parked on a
    // dialog for its whole wall-clock, and every retry a runner grants it is spent the same way.
    // Those failures exit non-zero instead, on one greppable marker — "login: FATAL" — that
    // leg.sh keys on (decision 1371).
    //
    // **"May" is the whole correction.** This flag is derived from the environment
    // ([`crate::run_mode::unattended_login`] — any of `$WOW_USER`/`$WOW_PASS`/`$WOW_CHAR`), and
    // the environment cannot see who is in the room. The director keeps `$WOW_CHAR` set to skip
    // character select, which made every one of their sessions "a harness" — so a **typed**
    // password with a typo killed the process instead of showing the dialog the reference shows.
    // The env fact is the right answer to "should the client log in without waiting for someone
    // to type?" and the wrong answer to "is there anybody here?"; it was answering both. Whether
    // an *attempt* was typed is not an inference at all — see `announced` at the use sites.
    let harness_env =
        crate::run_mode::unattended_login() && std::env::var_os("WOW_LOGIN_SMOKE").is_none();
    let empty = GlueStrings::default();
    let strings = strings.as_deref().unwrap_or(&empty);

    // The env fast path, once (decision 0539 §3): any of WOW_USER/WOW_PASS/WOW_CHAR explicitly
    // set → auto-submit env-with-defaults, so every probe/smoke/harness invocation keeps working.
    // The login smoke drives its own credentials instead.
    if !attempt.intent.env_read {
        attempt.intent.env_read = true;
        // The same fact a lost session asks about (decision 1262) — read from the one place that
        // owns it, so "the harness logs in for us" and "the harness logs back in for us" can never
        // be two different answers.
        if crate::run_mode::unattended_login() && std::env::var_os("WOW_LOGIN_SMOKE").is_none() {
            let user = std::env::var("WOW_USER").unwrap_or_else(|_| "one".into());
            let pass = std::env::var("WOW_PASS").unwrap_or_else(|_| "pone".into());
            // The account guard (decision 0649): a vmangos login KICKS whoever holds the account,
            // so an unattended run from a pool slot must not authenticate as the director's `one`
            // or a neighbouring slot's probe. Only the *automated* path is gated — a typed login
            // is the director's own and is never second-guessed.
            match crate::run_mode::account_guard(&user) {
                Ok(()) => {
                    info!("login: env fast path — auto-submitting as {user}");
                    attempt.intent.creds = Some((user, pass));
                    attempt.intent.retry_at = Some(now);
                }
                Err(why) if std::env::var_os("WOW_ALLOW_ACCOUNT").is_some() => {
                    warn!("login: {why} — WOW_ALLOW_ACCOUNT is set, going ahead anyway");
                    attempt.intent.creds = Some((user, pass));
                    attempt.intent.retry_at = Some(now);
                }
                Err(why) => {
                    error!("login: REFUSING the env fast path — {why} Set WOW_ALLOW_ACCOUNT=1 if the cross-account login is deliberate.");
                    dialog.open_error(&why);
                    // The refusal is deterministic — the slot is baked into the binary — so the
                    // run can never get past this screen (the 1371 legs burned 3 × timeout on it).
                    error!("login: FATAL — account guard refused the only credentials this run has; exiting");
                    exit.write(AppExit::error());
                }
            }
        }
    }

    for msg in stages.read() {
        if matches!(dialog.kind, Some(DialogKind::Status)) {
            dialog.set_text(stage_text(strings, msg.stage));
        }
    }
    // **The queue** (decision 1681): each packet is one sample, and the first one turns the
    // connecting dialog into the queue dialog. A queue is not a failure — the attempt is still in
    // flight and `in_flight` deliberately stays set, so nothing resubmits underneath it.
    for msg in queued.read() {
        if !matches!(dialog.kind, Some(DialogKind::Queued)) {
            dialog.open_queued(msg.realm.clone());
        }
        if let Some(position) = msg.position {
            dialog.queue.sample(position, queue_now_ms(&time));
        }
        info!(
            "login: queued for {} at position {}",
            msg.realm.as_deref().unwrap_or("the realm"),
            msg.position
                .map_or_else(|| "unknown".to_string(), |p| p.to_string()),
        );
    }
    // The countdown is live between packets, so the text is recomputed every frame it is up.
    if matches!(dialog.kind, Some(DialogKind::Queued)) {
        let text = dialog.queue.text(
            queue_now_ms(&time),
            dialog.queue_realm.clone().as_deref(),
            strings,
        );
        dialog.set_text(&text);
    }
    for msg in failures.read() {
        attempt.intent.in_flight = false;
        attempt.intent.park = IoPark::AtLogin;
        // A terminal failure names something no resubmit can change (the server requires Warden,
        // say) — show the server's own words and drop the credentials so nothing retries.
        // **Did a person submit the attempt that just failed?** `announced` is set by the login
        // screen's own submit and by nothing else — the env fast path and every paced resubmit
        // leave it false — so it is direct evidence rather than an inference about the room, and
        // `LoginIntent::clear` below deliberately does not reset it. A typed attempt is attended
        // by definition, and an attended failure gets the reference's answer: the dialog, and
        // another go. Only an attempt nobody typed may kill the process.
        let typed = attempt.intent.announced;
        let fatal = harness_env && !typed;
        if msg.terminal {
            warn!("login: {}", msg.reason);
            attempt.intent.clear();
            dialog.open_error(&msg.reason);
            if fatal {
                error!("login: FATAL — terminal login failure with nobody at the keyboard ({}); exiting", msg.reason);
                exit.write(AppExit::error());
            }
            continue;
        }
        match msg.refusal {
            Some(refusal) => {
                // A refusal — from EITHER server: surface it (even on the silent path, since the
                // credentials went stale) and never auto-retry against it.
                let byte = refusal.byte();
                warn!("login: refused ({refusal:?}, {byte:#04x}) — {}", msg.reason);
                attempt.intent.clear();
                dialog.open_error(&fail_text(strings, Some(refusal), None));
                if fatal {
                    error!("login: FATAL — refused ({refusal:?}, {byte:#04x}) and no resubmit can change it; exiting");
                    exit.write(AppExit::error());
                }
            }
            None if attempt.intent.announced => {
                warn!("login: {}", msg.reason);
                dialog.open_error(&fail_text(strings, None, msg.dial.as_ref()));
            }
            None => {
                // Silent transport failure with pending intent: schedule the paced resubmit.
                debug!("login: transport failure ({}) — retrying", msg.reason);
                if attempt.intent.creds.is_some() {
                    attempt.intent.retry_at = Some(now + RETRY_DELAY_SECS);
                }
            }
        }
    }
    for msg in disconnects.read() {
        // The IO thread is heading back to its pre-logon park.
        attempt.intent.park = IoPark::AtLogin;
        attempt.intent.in_flight = false;
        if msg.session_over {
            // The reference's `DISCONNECTED_FROM_SERVER` (decision 1262): `GlueParent.lua` answers
            // it with `SetGlueScreen("login")` + `GlueDialog_Show("DISCONNECTED")` — the account
            // screen and one Okay button. Nothing retries, and the credentials go with the session:
            // a client that re-authenticates on its own steals the account back from whoever just
            // displaced it, which is the ping-pong the report described.
            warn!(
                "login: {} — session over, back to the login screen",
                msg.reason
            );
            attempt.intent.clear();
            dialog.open_error(strings.text("DISCONNECTED", "Disconnected from server"));
            continue;
        }
        // Otherwise the session continues through the park and the re-auth is silent: immediate
        // after a clean logout (the roster IS the select screen the app now shows), paced after a
        // stream death an unattended run must recover from on its own (0065, paced app-side).
        if attempt.intent.creds.is_some() {
            let delay = if msg.end == benilla_protocol::SessionEnd::LoggedOut {
                0.0
            } else {
                RETRY_DELAY_SECS
            };
            attempt.intent.retry_at = Some(now + delay);
        }
    }

    // The silent (re)submit tick.
    if attempt.intent.park == IoPark::AtLogin
        && !attempt.intent.in_flight
        && attempt.intent.retry_at.is_some_and(|t| now >= t)
    {
        if let Some((user, pass)) = attempt.intent.creds.clone() {
            attempt.send(&user, &pass, false);
        } else {
            attempt.intent.retry_at = None;
        }
    }
}

/// The roster's arrival is the login flow's success edge: the attempt settled, the IO thread is
/// parked at select — leave the login screen for CharSelect (only from `Login`; a reconnect's
/// roster lands while `InWorld` and must not flip the screen).
fn to_select_on_roster(
    mut msgs: MessageReader<CharListMessage>,
    mut intent: ResMut<LoginIntent>,
    mut dialog: ResMut<LoginDialog>,
    state: Res<State<ClientState>>,
    mut next: ResMut<NextState<ClientState>>,
) {
    if msgs.read().next().is_none() {
        return;
    }
    intent.in_flight = false;
    intent.park = IoPark::Active;
    intent.retry_at = None;
    dialog.close();
    if *state.get() == ClientState::Login {
        next.set(ClientState::CharSelect);
    }
}

// ── The screen's form state + input ──────────────────────────────────────────────────────────────

/// Which edit box has the focus.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Field {
    #[default]
    Account,
    Password,
}

/// The typed form: both boxes, the focus, and the Remember checkbox. Each box is a real
/// [`EditBoxState`] — the same byte-verified model the chat box uses — so the login fields get
/// caret movement, selection, Ctrl+A and the clipboard from the shared law rather than the
/// three-case imitation they used to carry (decision 0704). The caret clock lives in the box too
/// (`blink_accum`/`caret_shown`), so it blinks on the client's own 0.5 s period.
#[derive(Resource)]
pub(crate) struct LoginForm {
    pub(super) account: EditBoxState,
    pub(super) password: EditBoxState,
    pub(super) focus: Field,
    pub(super) save: bool,
}

impl Default for LoginForm {
    fn default() -> Self {
        LoginForm {
            account: textinput::field(MAX_LETTERS, false),
            // `password` masks the *display* only; the real text is never rendered or copied.
            password: textinput::field(MAX_LETTERS, true),
            focus: Field::default(),
            save: false,
        }
    }
}

impl LoginForm {
    /// Give `field` the keyboard **and select everything already in it** — dropping the selection
    /// on the box being left.
    ///
    /// **A knowing divergence** (director's call, 2026-08-28), and the reference half of it is now
    /// byte-settled rather than inferred (wow-re `editbox-selection-focus-law.md` §4/§5, §5-VERIFIED,
    /// dispatched from this work). The reference does the *opposite* on a click: `OnMouseDown`
    /// (`0x77b800`) hit-tests the click to a byte index, **collapses** the selection onto it
    /// (`0x77b86f call 0x77ccf0`) and only then calls `SetFocus` — so a fresh click-focus leaves an
    /// EMPTY selection at the character you clicked. And `SetFocus` itself writes no selection field
    /// at all; `0x77e3f6` being the only instruction image-wide that grants focus makes *every* focus
    /// gain selection-neutral, TAB included. Losing focus likewise touches nothing (`0x77af50` raises
    /// only the cursor dirty bit).
    ///
    /// So we diverge in both directions, deliberately: the reference collapses where we select, and
    /// leaves stale where we collapse. The reason is the same one for both — the thing a player does
    /// after clicking into a login field is *replace* what is there, a remembered account name or a
    /// mistyped password, and having to select-all or backspace it out first is friction the
    /// reference only avoids by being what everyone was used to in 2006.
    ///
    /// **A divergence owes both halves.** 1682 shipped only the select; nothing ever unselected,
    /// so the box you left kept a grey highlight behind you and the screen showed two selections at
    /// once (the director's report, 2026-08-29). Selecting on focus is only coherent if the
    /// selection *means* "this is the box the keyboard is in" — which makes collapsing the outgoing
    /// box not a second feature but the other half of this one. It runs the client's own
    /// collapse-to-cursor (`0x77ccf0`), the same primitive every delete path uses.
    ///
    /// The selection uses the client's own `HighlightText(0, -1)` (`0x77cca0`), which resets the
    /// blink on its way, so the box still opens on a solid caret — the property the old
    /// `reset_blink()` calls at these sites existed to preserve.
    fn focus(&mut self, field: Field) {
        self.focused().collapse();
        self.focus = field;
        self.focused().highlight_text(0, -1);
    }

    /// The box that currently owns the keyboard.
    fn focused(&mut self) -> &mut EditBoxState {
        match self.focus {
            Field::Account => &mut self.account,
            Field::Password => &mut self.password,
        }
    }
}

/// The armed quit (`gsTitleQuit` needs [`QUIT_GRACE_SECS`] to be audible before `AppExit`).
#[derive(Resource, Default)]
struct QuitArm(Option<f32>);

/// Entering the login screen: the ref's `AccountLogin_OnShow` — prefill the saved account name,
/// clear the password, focus account when empty / password otherwise, checkbox = saved-name
/// exists — and stand the `UI_MainMenu` scene up.
fn enter_login(mut form: ResMut<LoginForm>, mut preview: ResMut<GluePreview>) {
    let saved = load_saved_account();
    form.save = !saved.is_empty();
    form.focus = if saved.is_empty() {
        Field::Account
    } else {
        Field::Password
    };
    form.account.set_text(&saved);
    form.password.set_text("");
    // `SetFocus` starts the caret solid — the screen never opens mid-blink-off (`set_text` alone
    // wouldn't do it: it no-ops when the saved name is already in the box) — and selects, so a
    // remembered account name is typed over rather than appended to.
    let focus = form.focus;
    form.focus(focus);
    preview.scene = Some(GlueScene::MainMenu);
    preview.look = None;
    preview.yaw = 0.0;
}

/// The screen's input: typing into the focused box (the ref's 16-letter cap), Tab cycling, Enter
/// submits, Esc quits (dialog-first — an open dialog's Esc is its Cancel/Okay), clicks focus the
/// boxes / press the buttons / toggle the checkbox.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn login_input(
    presses: Query<(Entity, &LoginAction, Ref<Interaction>)>,
    clicks: Res<crate::glue::GlueClicks>,
    mut keyboard: MessageReader<KeyboardInput>,
    keys: Res<ButtonInput<KeyCode>>,
    // The host pasteboard + the window handle the Wayland backend needs (decision 0702).
    mut clipboard: NonSendMut<HostClipboard>,
    raw_handle: Query<&bevy::window::RawHandleWrapper, With<bevy::window::PrimaryWindow>>,
    mut form: ResMut<LoginForm>,
    mut attempt: Attempt,
    mut dialog: ResMut<LoginDialog>,
    strings: Option<Res<GlueStrings>>,
    mut sounds: MessageWriter<GlueSound>,
    mut quit: Local<bool>,
    mut commands: Commands,
    time: Res<Time>,
) {
    let empty = GlueStrings::default();
    let strings = strings.as_deref().unwrap_or(&empty);

    let mut do_login = false;
    let mut do_quit = false;

    // While a dialog is up it owns the input; the box/button surface underneath is inert.
    let dialog_open = dialog.kind.is_some();

    // **The edit boxes focus on the PRESS**, and only they. The reference's `CEditBox` takes focus
    // from its own OnMouseDown handler (`0x77b800`), unconditionally and autoFocus-independent
    // (wow-re `ui.md`) — an edit box is not a Button and does not wait for the release. Every
    // *button* on this screen fires from the release loop below (1533).
    for (entity, action, interaction) in &presses {
        if dialog_open {
            continue;
        }
        // **The edit boxes focus on the PRESS**, and only they: the reference's `CEditBox` takes
        // focus from its own OnMouseDown handler (`0x77b800`), unconditionally and
        // autoFocus-independent (wow-re `ui.md`) — an edit box is not a Button and does not wait
        // for the release. `Ref` supplies the press *edge* the old `Changed<Interaction>` filter
        // gave, without costing this system a second query.
        if interaction.is_changed() && *interaction == Interaction::Pressed {
            match action {
                // A click takes the focus the same way TAB does — the solid caret, and the
                // select-all that makes the next keystroke replace what is in the box.
                LoginAction::FocusAccount => form.focus(Field::Account),
                LoginAction::FocusPassword => form.focus(Field::Password),
                _ => {}
            }
        }
        // Everything else on this screen is a Button, and a Button fires on the RELEASE (1533).
        if !clicks.hit(entity) {
            continue;
        }
        match action {
            LoginAction::FocusAccount | LoginAction::FocusPassword => {} // focused on the press
            LoginAction::Login => do_login = true,
            LoginAction::Quit => do_quit = true,
            // The realmlist control (1667) — the button and the address readout under it are
            // the same action, so clicking either opens the editor.
            LoginAction::Realmlist => {
                // `gsLoginNewAccount` — what the reference plays for the other buttons in this
                // corner (`AccountLogin_ManageAccount`, `AccountLogin_LaunchCommunitySite`), which
                // is the closest authored answer for a button it does not have. It replaces a
                // `gsClick` I invented in 1667: no such SoundEntries kit exists, so that press was
                // silently playing nothing at all.
                sounds.write(GlueSound("gsLoginNewAccount"));
                if attempt.realmlist.pinned_by_env() {
                    // A harness/dev run owns the address for the session (`cvars`' env-override
                    // law). Say so rather than opening an editor whose Okay would be a silent
                    // no-op — the trap that shape would set.
                    dialog.open_error(&format!(
                        "$WOW_HOST is set for this session, so the realmlist is fixed at {}.",
                        attempt.realmlist.address(),
                    ));
                } else {
                    dialog.open_realmlist(
                        // The reference's own registered help text for `realmList`, byte-verified
                        // in `WoW.exe` beside the CVar's name and default.
                        "Address of realm list server",
                        attempt.realmlist.address(),
                    );
                }
            }
            LoginAction::ToggleSave => {
                form.save = !form.save;
                // Verbatim ref quirk (`AccountLoginSaveAccountName` OnClick): checked plays the
                // "Off" kit, unchecked the "On" kit.
                sounds.write(GlueSound(if form.save {
                    "igMainMenuOptionCheckBoxOff"
                } else {
                    "igMainMenuOptionCheckBoxOn"
                }));
                if !form.save {
                    save_account("");
                }
            }
            // The dialog's own buttons are [`drive_dialog`]'s, and this loop is skipped
            // entirely while one is open.
            LoginAction::Dialog | LoginAction::Dialog2 => {}
        }
    }

    let mods = textinput::mods_now(&keys);
    let wl = textinput::wayland_display(raw_handle.iter().next());
    for ev in keyboard.read() {
        // A dialog with an edit box owns the keyboard while it is up — the ref's `GlueDialog` is
        // `toplevel` with `enableKeyboard="true"`, so the boxes behind it hear nothing. ENTER and
        // ESCAPE still come back unclaimed; [`drive_dialog`] reads them as its two buttons.
        if dialog_open {
            if dialog.kind.is_some_and(DialogKind::has_edit_box) {
                textinput::feed_key(
                    &mut dialog.edit,
                    ev,
                    mods,
                    &mut clipboard,
                    wl,
                    textinput::CharFilter::Any,
                );
            }
            continue;
        }
        // The shared law first (editing, caret, selection, the clipboard trio); only what it
        // hands back unclaimed is the screen's own — TAB cycles the two boxes, ENTER/ESCAPE are
        // handled below off `just_pressed`.
        if textinput::feed_key(
            form.focused(),
            ev,
            mods,
            &mut clipboard,
            wl,
            textinput::CharFilter::Any,
        ) == textinput::FieldKey::Consumed
        {
            if form.focus == Field::Account {
                on_account_edited(&mut form);
            }
            continue;
        }
        if ev.state == ButtonState::Pressed && ev.key_code == KeyCode::Tab {
            let next = match form.focus {
                Field::Account => Field::Password,
                Field::Password => Field::Account,
            };
            form.focus(next);
        }
    }

    if !dialog_open
        && (keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter))
    {
        do_login = true;
    }
    if !dialog_open && keys.just_pressed(KeyCode::Escape) {
        do_quit = true;
    }

    if do_login && !attempt.intent.in_flight {
        // **`gsLogin` first, always** — `AccountLogin_Login` plays it before it calls
        // `DefaultServerLogin`, so the reference clicks even when the attempt is about to be
        // refused for an empty box. Ours played it only on the path that reached the wire, so a
        // Login press with a field still blank was silent and the dialog appeared out of nowhere
        // (the director's "the sound of the popup is sometimes missing"). It sits OUTSIDE the
        // match on purpose: a sound that is structurally unconditional cannot go missing on some
        // branch again.
        sounds.write(GlueSound("gsLogin"));
        match login_press(&form) {
            // The ref's own guards: empty account / empty password get their dialog, no wire.
            LoginPress::NeedAccount => {
                dialog.open_error(
                    strings.text("LOGIN_ENTER_NAME", "Please enter your account name."),
                );
            }
            LoginPress::NeedPassword => {
                dialog.open_error(
                    strings.text("LOGIN_ENTER_PASSWORD", "Please enter your password."),
                );
            }
            LoginPress::Submit => {
                // `AccountLogin_Login`: save or clear the account name per the checkbox, clear the
                // password box after grabbing it.
                if form.save {
                    save_account(&form.account.text);
                } else {
                    save_account("");
                }
                let (user, pass) = (form.account.text.clone(), form.password.text.clone());
                form.password.set_text("");
                dialog.open_status(strings.text("LOGIN_STATE_CONNECTING", "Connecting"));
                attempt.send(&user, &pass, true);
            }
        }
    }
    if do_quit && !*quit {
        *quit = true;
        sounds.write(GlueSound("gsTitleQuit"));
        commands.insert_resource(QuitArm(Some(time.elapsed_secs() + QUIT_GRACE_SECS)));
    }
}

/// The caret's clock. Exactly one box owns the keyboard, so the focused one is the one that blinks
/// — on the shared law's 0.5 s period, so the login caret keeps time with the create-name box, the
/// delete dialog and the chat box (decision 0704). It keeps blinking under an open dialog: a dialog
/// eats the keys, not the clock.
///
/// Its own system, not a line inside [`login_input`]: a blink is a clock, not input, and as a
/// system it can be run on its own in a test — which is the only thing that can catch this tick
/// going missing again. It went missing once already, and nothing but an eye noticed: the box's
/// `caret_shown` simply never left its `true` default, so the login caret was the one glue caret
/// that sat solid.
///
/// **A dialog with an edit box takes the focus with the keys** (1667): its box blinks and the
/// form's stops, so the screen never shows two live carets at once. Every other dialog leaves the
/// form's caret running — a dialog eats the keys, not the clock.
fn tick_login_caret(mut form: ResMut<LoginForm>, mut dialog: ResMut<LoginDialog>, time: Res<Time>) {
    let dt = time.delta_secs();
    let in_dialog = dialog.kind.is_some_and(DialogKind::has_edit_box);
    textinput::tick_caret(form.focused(), !in_dialog, dt);
    if in_dialog {
        textinput::tick_caret(&mut dialog.edit, true, dt);
    }
}

/// Editing the account box away from the saved name clears the save + unchecks (the ref's
/// `OnTextChanged`).
fn on_account_edited(form: &mut LoginForm) {
    if form.save {
        let saved = load_saved_account();
        if !saved.is_empty() && saved != form.account.text {
            save_account("");
            form.save = false;
        }
    }
}

/// Fire the armed quit once its grace elapsed (so `gsTitleQuit` is heard).
fn drive_quit(arm: Option<Res<QuitArm>>, time: Res<Time>, mut exit: MessageWriter<AppExit>) {
    if let Some(arm) = arm {
        if arm.0.is_some_and(|t| time.elapsed_secs() >= t) {
            exit.write(AppExit::Success);
        }
    }
}

// ── The GlueDialog (connecting / error) ──────────────────────────────────────────────────────────

/// Which dialog is up: the connecting status (Cancel button, text driven by the stages), an
/// error (Okay button), or the realmlist editor (Okay + Cancel over an edit box).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DialogKind {
    Status,
    Error,
    /// **Queued for a full realm** (decision 1681). The reference has no queue dialog *type*: it
    /// opens the ordinary `CANCEL` status dialog and re-texts it every frame, relabelling the one
    /// button to `CHANGE_REALM`. This is that, as a kind — the relabel is the only thing that
    /// distinguishes it from [`Self::Status`], and a kind is how this screen spells "different
    /// button caption".
    Queued,
    /// The realmlist editor (decision 1667) — the reference's `GlueDialog` `hasEditBox` shape,
    /// which is how the shipped dialog asks for a typed value (`GlueDialog.lua`: it shows
    /// `GlueDialogEditBox` and re-heights the box to
    /// `16 + text + 8 + editbox + 8 + button + 16`). The reference never opens this particular
    /// dialog — it has no realmlist UI at all — but the widget is its own.
    Realmlist,
}

impl DialogKind {
    /// Whether this dialog carries the ref's `GlueDialogEditBox` (its `hasEditBox` flag).
    pub(super) fn has_edit_box(self) -> bool {
        matches!(self, DialogKind::Realmlist)
    }

    /// `(button1, button2)` captions — `GlueDialogTypes`' own two fields. A `None` second button
    /// is the ref's centred single-button layout; `Some` is its BOTTOMRIGHT/LEFT pair.
    pub(super) fn buttons(self, strings: &GlueStrings) -> (&str, Option<&str>) {
        match self {
            DialogKind::Status => (strings.text("CANCEL", "Cancel"), None),
            // The reference's own relabel: leaving a queue is "Change Realm", not "Cancel".
            DialogKind::Queued => (strings.text("CHANGE_REALM", "Change Realm"), None),
            DialogKind::Error => (strings.text("OKAY", "Okay"), None),
            DialogKind::Realmlist => (
                strings.text("OKAY", "Okay"),
                Some(strings.text("CANCEL", "Cancel")),
            ),
        }
    }
}

/// What the realmlist dialog says when the box holds something that is not an address. It replaces
/// the prompt in place and **leaves the typed text alone**, so the fix is an edit rather than a
/// retype — the reason a bad value does not close the dialog or become an error dialog of its own.
const REALMLIST_BAD: &str =
    "That is not a server address.\nTry  logon.example.org  or  127.0.0.1:3724";

/// Which of the dialog's two buttons a key press answers, as `(button1, button2)` — the keyboard
/// half of [`drive_dialog`]'s buttons; the mouse half is OR-ed in beside it. Button 1 is the
/// affirmative one on every kind (Cancel on the status dialog, Okay on the others); button 2
/// exists only where the kind declares it. ENTER confirms, ESCAPE dismisses; on a one-button
/// dialog they are the same button.
///
/// **`on_screen` is the whole of the second bug fixed on 2026-08-29.** Pressing ENTER on an empty
/// login form played the sound and showed nothing, while *clicking* Login showed the dialog
/// (director's report). The two systems poll the same [`ButtonInput`]: [`login_input`] opened the
/// error dialog on the ENTER edge, and [`drive_dialog`] — later in the same chained frame, with
/// `keys` untouched — read *the same* `just_pressed(Enter)` as that dialog's own Okay and closed
/// it before it had ever been drawn. Both sounds played, which is exactly what was heard.
///
/// The reference cannot have this bug because it is event-driven: the ENTER that fires
/// `AccountLogin_Login` is *dispatched* to the focused edit box (`OnEnterPressed`), and a dialog
/// that does not exist yet is not in the dispatch. Polling is our seam, so the gate has to be
/// ours too, and it says the same thing the dispatch does — **a dialog answers only keys pressed
/// while it was already on screen**. Not "not this frame": on screen. It is passed the same
/// `fresh` flag that drives the (re)spawn, so the two cannot drift apart.
fn dialog_keys(kind: DialogKind, on_screen: bool, enter: bool, escape: bool) -> (bool, bool) {
    if !on_screen {
        return (false, false);
    }
    let button1 = match kind {
        // The status dialog's one button IS Cancel, so ESCAPE is it. The queue's button is the
        // same act under another name, so it answers to ESCAPE the same way.
        DialogKind::Status | DialogKind::Queued => escape,
        DialogKind::Error => escape || enter,
        DialogKind::Realmlist => enter,
    };
    (button1, kind == DialogKind::Realmlist && escape)
}

/// The login screen's one dialog (the ref's shared `GlueDialog`): kind + text; the driver spawns/
/// despawns the tree (respawning on a kind change — the button caption differs) and updates the
/// text in place.
#[derive(Resource, Default)]
pub(crate) struct LoginDialog {
    pub(super) kind: Option<DialogKind>,
    pub(super) text: String,
    pub(super) dirty: bool,
    pub(super) root: Option<Entity>,
    /// The ref's `GlueDialogEditBox` — a real [`EditBoxState`] like the two on the screen behind
    /// it, so the realmlist box gets the same caret, selection, Ctrl+A and clipboard law
    /// (decision 0704). Only meaningful while a [`DialogKind::has_edit_box`] dialog is up; it is
    /// rebuilt from the current value on every open, so a cancelled edit leaves nothing behind.
    pub(super) edit: EditBoxState,
    /// The queue's sample ring and the realm it is for — live only while [`DialogKind::Queued`]
    /// is up, and rebuilt from empty on every fresh queue so a second login never inherits the
    /// first one's estimate.
    pub(super) queue: queue::QueueEstimate,
    queue_realm: Option<String>,
    /// The kind the spawned tree was built for.
    spawned: Option<DialogKind>,
    /// The glue scale the spawned tree was built at — a resize rebuilds it.
    spawned_s: f32,
}

impl LoginDialog {
    fn open_status(&mut self, text: &str) {
        self.kind = Some(DialogKind::Status);
        self.set_text(text);
    }
    fn open_error(&mut self, text: &str) {
        self.kind = Some(DialogKind::Error);
        self.set_text(text);
    }
    /// Enter the queue: a fresh ring (a second login must not inherit the first one's estimate)
    /// and the realm's name for the `_NAME` text variants.
    fn open_queued(&mut self, realm: Option<String>) {
        self.kind = Some(DialogKind::Queued);
        self.queue = queue::QueueEstimate::default();
        self.queue_realm = realm;
        self.set_text("");
    }
    /// Open the realmlist editor over `current`, with the caret at the end of it and the whole
    /// value selected — the reference's `hasEditBox` dialogs open ready to be typed over, and a
    /// player changing servers is replacing the address far more often than editing it.
    fn open_realmlist(&mut self, prompt: &str, current: &str) {
        self.kind = Some(DialogKind::Realmlist);
        self.edit = textinput::field(crate::realmlist::MAX_LETTERS, false);
        self.edit.set_text(current);
        // `HighlightText(0, -1)` — the client's own select-all (`0x77cca0`), which resets the
        // blink on its way so the box opens on a solid caret.
        self.edit.highlight_text(0, -1);
        self.set_text(prompt);
    }
    fn set_text(&mut self, text: &str) {
        if self.text != text {
            self.text = text.to_string();
            self.dirty = true;
        }
    }
    fn close(&mut self) {
        self.kind = None;
        self.text.clear();
        self.dirty = false;
    }
}

/// Spawn/despawn the dialog tree with the resource, update its text, and run its one button:
/// Status's Cancel bumps the abandon generation (the in-flight attempt discards at its next
/// stage boundary) and forgets the intent; Error's Okay just closes. Esc = the button; Enter
/// confirms an error.
#[allow(clippy::too_many_arguments)]
fn drive_dialog(
    mut commands: Commands,
    mut dialog: ResMut<LoginDialog>,
    mut intent: ResMut<LoginIntent>,
    mut realmlist: ResMut<crate::realmlist::Realmlist>,
    mut script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    abandon: Res<LoginAbandon>,
    art: Res<crate::glue::art::GlueArt>,
    assets: Res<AssetServer>,
    strings: Option<Res<GlueStrings>>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Query<(Entity, &LoginAction)>,
    clicks: Res<crate::glue::GlueClicks>,
    mut texts: Query<&mut Text, With<screen::DialogText>>,
    mut sounds: MessageWriter<GlueSound>,
    window: Query<&Window, With<bevy::window::PrimaryWindow>>,
) {
    let empty = GlueStrings::default();
    let strings = strings.as_deref().unwrap_or(&empty);

    let Some(kind) = dialog.kind else {
        if let Some(root) = dialog.root.take() {
            commands.entity(root).despawn();
        }
        dialog.spawned = None;
        return;
    };

    // (Re)spawn on the open edge, a kind change (the button caption differs), or a window resize
    // (the tree bakes the glue scale); a text-only change updates the message line in place.
    //
    // `fresh` is *this dialog appearing*, as opposed to a resize rebuilding a dialog already up —
    // and it is the same flag [`dialog_keys`] takes as `!on_screen`, so the frame the dialog is
    // drawn on and the frame it starts answering keys on are one decision, not two.
    let s = crate::glue::screen_scale(window.single().ok());
    let fresh = dialog.root.is_none() || dialog.spawned != Some(kind);
    if fresh || dialog.spawned_s != s {
        if let Some(root) = dialog.root.take() {
            commands.entity(root).despawn();
        }
        dialog.root = Some(screen::spawn_dialog(
            &mut commands,
            &art,
            &assets,
            strings,
            kind,
            &dialog.text,
            s,
        ));
        dialog.edit.reset_blink();
        dialog.spawned = Some(kind);
        dialog.spawned_s = s;
        dialog.dirty = false;
    } else if dialog.dirty {
        for mut t in &mut texts {
            if t.0 != dialog.text {
                t.0 = dialog.text.clone();
            }
        }
        dialog.dirty = false;
    }

    // The buttons, from the mouse or the keys. A click needs no `fresh` guard of its own: the
    // button it would hit did not exist to be clicked on the frame the tree spawned.
    let hit = |want: LoginAction| buttons.iter().any(|(e, a)| *a == want && clicks.hit(e));
    let (key1, key2) = dialog_keys(
        kind,
        !fresh,
        keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter),
        keys.just_pressed(KeyCode::Escape),
    );
    let button1 = hit(LoginAction::Dialog) || key1;
    let button2 = hit(LoginAction::Dialog2) || key2;

    // **Every glue dialog button clicks** — `GlueDialog_OnClick` ends with
    // `PlaySound("gsTitleOptionOK")` for button 1 and button 2 alike, whatever the dialog's type.
    // Ours played nothing at all. Emitted here, once, before the kinds diverge, so no arm can
    // forget it (and so the realmlist editor's Okay-that-refuses still clicks: the reference plays
    // the sound on the press, not on the outcome).
    if button1 || button2 {
        sounds.write(GlueSound("gsTitleOptionOK"));
    }
    if kind == DialogKind::Realmlist && button1 {
        // Okay: take the typed address, or say why it cannot be taken and stay open with the text
        // as typed — the fix is then an edit, not a retype.
        let typed = dialog.edit.text.clone();
        if accept_realmlist(&typed, &mut realmlist, script.as_deref_mut()) {
            dialog.close();
            if let Some(root) = dialog.root.take() {
                commands.entity(root).despawn();
            }
        } else {
            dialog.set_text(REALMLIST_BAD);
        }
        return;
    }
    if button1 || button2 {
        if matches!(kind, DialogKind::Status | DialogKind::Queued) {
            // Cancel: the next stage boundary discards the attempt; a canceled manual attempt
            // must not silently resubmit later.
            abandon.0.fetch_add(1, Ordering::SeqCst);
            intent.in_flight = false;
            intent.clear();
        }
        dialog.close();
        if let Some(root) = dialog.root.take() {
            commands.entity(root).despawn();
        }
    }
}

/// Take the realmlist dialog's Okay: normalize what was typed, point the session at it, and mirror
/// it into the `realmList` CVar so it is still there next launch. `false` = the box holds nothing
/// usable and the caller should keep the dialog open.
///
/// The persistence leg is `char_select`'s `lastCharacterIndex` pattern exactly (1131/1293): an
/// **engine-side** write rides the change queue like a Lua `SetCVar`, so `cvars::sync_cvars` folds
/// it into the knob and marks the file dirty, and `save_config` writes `config.toml`. A write to a
/// name the VM has not registered yet is a deliberate silent no-op there, so this reports that
/// case rather than claiming a save that did not happen — the **session** value stands either way,
/// which is the half that matters for the login about to be attempted.
fn accept_realmlist(
    typed: &str,
    realmlist: &mut crate::realmlist::Realmlist,
    script: Option<&mut benilla_ui::script::UiScript>,
) -> bool {
    let Some(address) = crate::realmlist::normalize(typed) else {
        return false;
    };
    realmlist.set(&address);
    let name = crate::realmlist::CVAR_REALMLIST;
    match script {
        Some(script) => {
            script.set_cvar_engine(name, &address);
            if script.cvar(name).is_some() {
                info!("login: realmlist -> {address}");
            } else {
                warn!(
                    "login: realmlist -> {address} for this session, but the VM has not registered \
                     {name} yet, so it was not saved"
                );
            }
        }
        None => {
            warn!("login: realmlist -> {address} for this session only — no UI VM to persist it")
        }
    }
    true
}

// ── The saved account name (decision 0539 §4) ────────────────────────────────────────────────────

/// Read the saved account name from `base` (missing file/dir = empty). Takes the *file* rather
/// than resolving one, so the round-trip is testable from a tempdir.
fn load_saved_account_from(path: &std::path::Path) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Write (or, for an empty name, remove) the saved account name at `path`.
fn save_account_to(path: &std::path::Path, name: &str) {
    if name.is_empty() {
        let _ = std::fs::remove_file(path);
        return;
    }
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    if let Err(e) = std::fs::write(path, name) {
        warn!("login: saving account name failed: {e}");
    }
}

/// The ref's `GetSavedAccountName`. The path is [`crate::local_state`]'s — this module computed its
/// own until decision 1181, which is how the account name ended up in a different folder from every
/// other setting, and how a capture came to read one off the host machine.
fn load_saved_account() -> String {
    crate::local_state::saved_account_path()
        .map(|p| load_saved_account_from(&p))
        .unwrap_or_default()
}

/// The ref's `SetSavedAccountName` (empty clears).
fn save_account(name: &str) {
    if let Some(path) = crate::local_state::saved_account_path() {
        save_account_to(&path, name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand `drive_policy` up on its own with the env fast path already spent, so the policy
    /// under test is the disconnect arm and nothing else — and so an ambient `WOW_USER` in
    /// whatever shell runs the suite cannot seed credentials behind the assertions.
    fn policy_app() -> (App, crossbeam_channel::Receiver<LoginRequest>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<LoginIntent>()
            .init_resource::<LoginDialog>()
            // Literal, not Default: `Realmlist::default()` reads `$WOW_HOST`, and every probe
            // recipe in this repo exports it — a suite run from such a shell would otherwise
            // assert against whatever that shell happened to be pointing at.
            .insert_resource(crate::realmlist::Realmlist::unpinned(
                crate::realmlist::DEFAULT_REALMLIST,
            ))
            .insert_resource(LoginSubmit(tx))
            .insert_resource(LoginAbandon(std::sync::Arc::new(
                std::sync::atomic::AtomicU64::new(0),
            )))
            .add_message::<LoginStageMessage>()
            .add_message::<LoginQueuedMessage>()
            .add_message::<LoginFailedMessage>()
            .add_message::<DisconnectedMessage>()
            .add_systems(Update, drive_policy);
        app.world_mut().resource_mut::<LoginIntent>().env_read = true;
        // The receiver is RETURNED rather than leaked: a dropped one turns every submit into an
        // `Err` and hides a policy that sent one, and holding it is also what lets a test read
        // back the request that went out.
        (app, rx)
    }

    /// **A lost session does not log itself back in** (decision 1262).
    ///
    /// This is the whole of the displacement report: log into the same account from the reference
    /// client, vmangos kicks us with a bare socket close, and 0065's paced resubmit — which cannot
    /// see *why* the socket died, because nothing on the wire says — re-authenticated three seconds
    /// later and kicked the client that had just displaced us. The account ping-ponged. The
    /// reference's `GlueParent.lua` answers `DISCONNECTED_FROM_SERVER` with the login screen and a
    /// one-button dialog, and retries nothing.
    #[test]
    fn a_lost_session_clears_the_credentials_and_shows_the_dialog() {
        let (mut app, _requests) = policy_app();
        app.world_mut().resource_mut::<LoginIntent>().creds = Some(("one".into(), "pone".into()));
        app.world_mut().write_message(DisconnectedMessage {
            reason: "disconnected: world stream closed: failed to fill whole buffer".into(),
            end: benilla_protocol::SessionEnd::Lost,
            session_over: true,
        });
        app.update();

        let intent = app.world().resource::<LoginIntent>();
        assert!(
            intent.creds.is_none(),
            "the session's credentials die with the session — keeping them is what won the \
             account back off the client that displaced us",
        );
        assert!(intent.retry_at.is_none(), "and nothing is scheduled");
        let dialog = app.world().resource::<LoginDialog>();
        assert_eq!(dialog.kind, Some(DialogKind::Error));
        // The fallback literal: this App has no GlueStrings, and the table's own row is
        // `DISCONNECTED = "Disconnected from server";` (GlueStrings.lua) — the same words.
        assert_eq!(dialog.text, "Disconnected from server");
    }

    /// A **clean logout's** teardown rides the same message and must keep its silent relist: the
    /// IO thread returns to the pre-logon park, and the roster it comes back with IS the character
    /// select the player asked for. Breaking this would strand `/logout` on the login screen.
    #[test]
    fn a_logout_teardown_still_relists_at_once() {
        let (mut app, _requests) = policy_app();
        app.world_mut().resource_mut::<LoginIntent>().creds = Some(("one".into(), "pone".into()));
        app.world_mut().write_message(DisconnectedMessage {
            reason: "logged out".into(),
            end: benilla_protocol::SessionEnd::LoggedOut,
            session_over: false,
        });
        app.update();

        let intent = app.world().resource::<LoginIntent>();
        assert!(
            intent.creds.is_some(),
            "a logout keeps the account signed in"
        );
        assert!(
            intent.in_flight,
            "and the same tick resubmits — the delay for a logout is 0, so the roster comes \
             straight back",
        );
        assert!(
            app.world().resource::<LoginDialog>().kind.is_none(),
            "with no dialog: nothing went wrong",
        );
    }

    /// An **unattended** run keeps 0065's paced reconnect on a lost session — the verdict rides
    /// the message, so the policy honours it without re-reading the environment.
    #[test]
    fn an_unattended_run_still_reconnects_on_its_own() {
        let (mut app, _requests) = policy_app();
        app.world_mut().resource_mut::<LoginIntent>().creds =
            Some(("probe1".into(), "pprobe1".into()));
        app.world_mut().write_message(DisconnectedMessage {
            reason: "disconnected: connection reset".into(),
            end: benilla_protocol::SessionEnd::Lost,
            session_over: false,
        });
        app.update();

        let intent = app.world().resource::<LoginIntent>();
        assert!(intent.creds.is_some());
        assert_eq!(
            intent.retry_at,
            Some(RETRY_DELAY_SECS),
            "paced by the flat 3 s off a zeroed clock, not fired on the spot",
        );
        assert!(app.world().resource::<LoginDialog>().kind.is_none());
    }

    /// **The submitted attempt dials the configured realmlist** (decision 1667) — the whole
    /// point of the setting. Before this, the address was latched out of `$WOW_HOST` once at
    /// process start and the request had no say in it; now the request carries it, so a change
    /// made between attempts is the one the next attempt uses.
    #[test]
    fn a_submitted_attempt_carries_the_configured_realmlist() {
        let (mut app, requests) = policy_app();
        app.world_mut()
            .insert_resource(crate::realmlist::Realmlist::unpinned(
                "logon.example.org:3725",
            ));
        // Credentials pending with the retry due: the policy's silent submit tick.
        {
            let mut intent = app.world_mut().resource_mut::<LoginIntent>();
            intent.creds = Some(("one".into(), "pone".into()));
            intent.retry_at = Some(0.0);
        }
        app.update();

        let sent = requests.try_recv().expect("the policy submitted");
        assert_eq!(sent.user, "one");
        assert_eq!(
            sent.host, "logon.example.org:3725",
            "the attempt dials what the realmlist says, not a value latched at spawn",
        );

        // And a change between attempts is picked up by the next one, with no relaunch.
        app.world_mut()
            .insert_resource(crate::realmlist::Realmlist::unpinned(
                "elsewhere.example.org",
            ));
        {
            let mut intent = app.world_mut().resource_mut::<LoginIntent>();
            intent.in_flight = false;
            intent.retry_at = Some(0.0);
        }
        app.update();
        assert_eq!(
            requests.try_recv().expect("resubmitted").host,
            "elsewhere.example.org",
        );
    }

    /// The dialog's Okay: what the box holds becomes the session's address, including the
    /// `realmlist.wtf` line a player pastes off a server's setup page. No VM here, so the
    /// persistence leg is the `None` arm — the session value is what this asserts, and it is the
    /// half the next login attempt reads.
    #[test]
    fn the_realmlist_dialog_takes_what_was_typed() {
        let mut realmlist = crate::realmlist::Realmlist::unpinned("localhost");
        assert!(accept_realmlist(
            r#"  SET realmlist "logon.example.org"  "#,
            &mut realmlist,
            None,
        ));
        assert_eq!(realmlist.address(), "logon.example.org");
    }

    /// …and a box holding something that is not an address changes nothing and reports it, so the
    /// caller keeps the dialog open over the text as typed rather than closing on a silent no-op.
    #[test]
    fn a_bad_address_leaves_the_realmlist_alone() {
        let mut realmlist = crate::realmlist::Realmlist::unpinned("localhost");
        for typed in ["", "   ", "logon.example.org and more", "host:notaport"] {
            assert!(
                !accept_realmlist(typed, &mut realmlist, None),
                "{typed:?} is not an address",
            );
            assert_eq!(realmlist.address(), "localhost");
        }
    }

    /// The dialog kinds' own shape: only the realmlist editor carries the ref's `hasEditBox`, and
    /// only it declares a second button. The caret clock and the keyboard routing both branch on
    /// the first of those, and `spawn_dialog` lays out from the second.
    #[test]
    fn only_the_realmlist_dialog_has_a_box_and_two_buttons() {
        let strings = GlueStrings::default();
        assert!(!DialogKind::Status.has_edit_box());
        assert!(!DialogKind::Error.has_edit_box());
        assert!(DialogKind::Realmlist.has_edit_box());
        assert_eq!(DialogKind::Status.buttons(&strings), ("Cancel", None));
        assert_eq!(DialogKind::Error.buttons(&strings), ("Okay", None));
        assert_eq!(
            DialogKind::Realmlist.buttons(&strings),
            ("Okay", Some("Cancel")),
        );
    }

    /// **A dialog never answers the key press that opened it.** ENTER on an empty login form
    /// opened the error dialog in `login_input` and then, later in the *same* frame with the same
    /// `ButtonInput`, `drive_dialog` read that identical `just_pressed(Enter)` as the dialog's own
    /// Okay — so the popup appeared and vanished inside one frame and only the two sounds were
    /// heard (director's report, 2026-08-29). The gate is being on screen, not a frame count.
    #[test]
    fn a_dialog_does_not_answer_the_key_that_opened_it() {
        // The frame it appears on: the key that opened it is not its answer.
        assert_eq!(
            dialog_keys(DialogKind::Error, false, true, false),
            (false, false),
        );
        assert_eq!(
            dialog_keys(DialogKind::Error, false, false, true),
            (false, false),
        );
        // Once it is up, the same press is.
        assert_eq!(
            dialog_keys(DialogKind::Error, true, true, false),
            (true, false),
        );
    }

    /// Which key answers which button, per kind: ENTER and ESCAPE are the same (single) button on
    /// an error dialog; ESCAPE alone works the Cancel-shaped ones; the two-button editor splits
    /// them, ENTER to Okay and ESCAPE to Cancel.
    #[test]
    fn each_dialog_kind_maps_its_own_keys() {
        for (kind, enter, escape, want) in [
            (DialogKind::Error, true, false, (true, false)),
            (DialogKind::Error, false, true, (true, false)),
            (DialogKind::Status, true, false, (false, false)),
            (DialogKind::Status, false, true, (true, false)),
            (DialogKind::Queued, false, true, (true, false)),
            (DialogKind::Queued, true, false, (false, false)),
            (DialogKind::Realmlist, true, false, (true, false)),
            (DialogKind::Realmlist, false, true, (false, true)),
        ] {
            assert_eq!(
                dialog_keys(kind, true, enter, escape),
                want,
                "{kind:?} enter={enter} escape={escape}",
            );
        }
    }

    /// Opening the editor seats the current address in the box, selected whole — so typing a new
    /// server replaces the old one instead of appending to it.
    #[test]
    fn opening_the_editor_preselects_the_current_address() {
        let mut dialog = LoginDialog::default();
        dialog.open_realmlist("Address of realm list server", "logon.example.org");
        assert_eq!(dialog.kind, Some(DialogKind::Realmlist));
        assert_eq!(dialog.edit.text, "logon.example.org");
        assert_eq!(
            dialog.edit.selected_text().as_deref(),
            Some("logon.example.org")
        );
        assert_eq!(dialog.edit.max_letters, crate::realmlist::MAX_LETTERS);
        assert!(!dialog.edit.password, "an address is not a secret");
    }

    /// Did this update ask the app to quit?
    fn exited(app: &mut App, cursor: &mut bevy::ecs::message::MessageCursor<AppExit>) -> bool {
        let msgs = app.world().resource::<Messages<AppExit>>();
        cursor.read(msgs).next().is_some()
    }

    /// Drive one refusal through the policy and report whether it killed the process.
    fn refusal_exits(typed: bool) -> bool {
        let (mut app, _requests) = policy_app();
        let mut cursor = bevy::ecs::message::MessageCursor::<AppExit>::default();
        // Drain whatever the first update writes before the message under test.
        app.update();
        let _ = exited(&mut app, &mut cursor);
        app.world_mut().resource_mut::<LoginIntent>().announced = typed;
        app.world_mut().write_message(LoginFailedMessage {
            refusal: Some(LoginRefusal::Logon(0x05)),
            reason: "server rejected logon: result 0x05".into(),
            terminal: false,
            dial: None,
        });
        app.update();
        assert_eq!(
            app.world().resource::<LoginDialog>().kind,
            Some(DialogKind::Error),
            "every refusal shows the dialog, exit or not",
        );
        exited(&mut app, &mut cursor)
    }

    /// **A typo must not kill the client.**
    ///
    /// The reported crash: `login: FATAL — refused (code 0x04) … exiting`, on a password typed at
    /// the screen. `$WOW_CHAR` was set — the director keeps it to skip character select — which
    /// makes [`crate::run_mode::unattended_login`] true and therefore made *every* session "a
    /// harness", so a refusal took `AppExit::error()` instead of the reference's dialog. The
    /// environment cannot see who is in the room; `announced` can, because only the screen's own
    /// submit sets it.
    #[test]
    fn a_typed_password_refusal_never_exits() {
        let _lock = crate::local_state::test_env::ENV_LOCK.lock();
        // The harness environment the director actually runs with.
        let _char = crate::local_state::test_env::EnvGuard::set("WOW_CHAR", "Somebody");
        let _smoke = crate::local_state::test_env::EnvGuard::unset("WOW_LOGIN_SMOKE");
        assert!(
            crate::run_mode::unattended_login(),
            "the fixture must reproduce the env that made this fatal",
        );
        assert!(
            !refusal_exits(true),
            "a password typed at the screen is attended by definition — dialog, then another go",
        );
        assert!(
            refusal_exits(false),
            "an attempt nobody typed still exits non-zero: decision 1371's leg guarantee",
        );
    }

    /// **The dead web address comes off** (director's call) — at the clause, so the sentence in
    /// front of it survives.
    #[test]
    fn a_dead_url_is_cut_at_its_clause() {
        // The reported one, verbatim from `GlueStrings.lua`.
        assert_eq!(
            without_dead_url(
                "The information you have entered is not valid.  Please check the spelling of the \
                 account name and password.  If you need help in retrieving a lost or stolen \
                 password and account, see www.worldofwarcraft.com for more information.",
            ),
            "The information you have entered is not valid.  Please check the spelling of the \
             account name and password.  If you need help in retrieving a lost or stolen password \
             and account.",
        );
        // A whole trailing sentence goes when the address is what the sentence is for.
        assert_eq!(
            without_dead_url(
                "This World of Warcraft account has been closed and is no longer available for \
                 use.  Please go to http://www.worldofwarcraft.com/misc/banned.html for further \
                 information. ",
            ),
            "This World of Warcraft account has been closed and is no longer available for use.",
        );
        // Nothing to cut: left exactly alone, and borrowed rather than rebuilt.
        let clean = "You have used up your prepaid time for this account.";
        assert!(matches!(
            without_dead_url(clean),
            std::borrow::Cow::Borrowed(_)
        ));
        assert_eq!(without_dead_url(clean), clean);
    }

    /// The guard that stops the trim eating a string whose address has no clause in front of it —
    /// `AUTH_BANNED`, where the URL sits behind a dash. Better untrimmed than truncated to nothing.
    #[test]
    fn an_address_with_no_clause_boundary_is_left_alone() {
        let banned = "This account has been banned for violating the Terms of Use Agreement - \
                      www.worldofwarcraft.com/termsofuse.shtml. Please contact our GM department.";
        assert_eq!(without_dead_url(banned), banned);
        // …and the degenerate case cannot produce an empty dialog.
        assert_eq!(without_dead_url("www.example.com"), "www.example.com");
        assert_eq!(without_dead_url(". www.example.com"), ". www.example.com");
    }

    /// The Login press's two guards, in the reference's order: a wholly empty form asks for the
    /// account name first.
    #[test]
    fn the_login_press_asks_for_the_account_before_the_password() {
        let mut form = LoginForm::default();
        assert_eq!(login_press(&form), LoginPress::NeedAccount);
        form.account.set_text("one");
        assert_eq!(login_press(&form), LoginPress::NeedPassword);
        form.password.set_text("pone");
        assert_eq!(login_press(&form), LoginPress::Submit);
        // And a password-only form still asks for the account, not the password.
        let mut only_pass = LoginForm::default();
        only_pass.password.set_text("pone");
        assert_eq!(login_press(&only_pass), LoginPress::NeedAccount);
    }

    /// **Focusing a box selects it** (director's call, diverging from the reference) — and leaves
    /// the caret solid, which is what the `reset_blink` calls this replaced were for.
    #[test]
    fn focusing_a_box_selects_all_of_it() {
        let mut form = LoginForm::default();
        form.account.set_text("remembered");
        form.password.set_text("secret");

        form.focus(Field::Account);
        assert_eq!(form.focus, Field::Account);
        assert_eq!(form.account.selected_text().as_deref(), Some("remembered"));
        assert!(form.account.caret_shown, "a fresh focus starts solid");

        // TAB to the other box selects that one whole too. Checked as a RANGE, not as text: a
        // password box's `selected_text` hands back the `*` mask, because the real characters are
        // never rendered or copied (decision 0704's box law) — which is the correct answer and
        // exactly why the assertion cannot ask for them.
        form.focus(Field::Password);
        assert_eq!(
            (form.password.sel_start, form.password.sel_end),
            (0, form.password.text.len()),
            "the whole password is selected even though it cannot be read back",
        );

        // **And the box we left is no longer selected.** The other half of the divergence: with
        // the selection standing for "the keyboard is here", exactly one box can carry one.
        assert_eq!(
            form.account.selected_text(),
            None,
            "the box that lost the keyboard keeps no highlight",
        );

        // Typing then replaces rather than appends — the point of the divergence.
        form.focused().insert("x");
        assert_eq!(form.password.text, "x");

        form.focus(Field::Account);
        assert_eq!(
            form.password.selected_text(),
            None,
            "and back the other way"
        );
    }

    /// The code→string map quotes the client's own strings for the vmangos-verified rows.
    /// The realmd map, against the byte-verified table (wow-re `login-failure-dialogs.md`).
    ///
    /// Every row of this changed in decision 1679: the codes were always right and the string
    /// FAMILY was always wrong, so each of these used to answer with the terse `AUTH_*` twin of
    /// the string it now gives.
    #[test]
    fn fail_text_maps_the_verified_codes() {
        let strings = GlueStrings::default(); // empty table → the fallback literals
        let logon = |b| fail_text(&strings, Some(LoginRefusal::Logon(b)), None);

        // 0x04 and 0x05 share one jump-table arm — the client cannot say "wrong password", and
        // this is the string a player actually gets for a typo.
        assert!(logon(0x04).starts_with("The information you have entered is not valid."));
        assert_eq!(logon(0x05), logon(0x04), "one arm, byte-identical");
        assert!(logon(0x04).contains("account name and password"));

        // The four "no usable connection" codes share `LOGIN_FAILED` with a bare transport failure.
        assert_eq!(fail_text(&strings, None, None), "Unable to connect");
        for code in [0x01, 0x02, 0x0B, 0x0D] {
            assert_eq!(logon(code), "Unable to connect", "code {code:#04x}");
        }

        assert!(logon(0x03).contains("has been closed"));
        assert!(logon(0x06).contains("already logged into"));
        assert!(logon(0x09).contains("Unable to validate game version"));
        assert!(logon(0x0C).contains("temporarily suspended"));
        assert!(logon(0x0F).contains("parental controls"));

        // Past 0x0F the byte-index table saturates onto the disconnect arm — so an unknown code
        // reads as a disconnect, not as an authentication message.
        for code in [0x10, 0x11, 0x12, 0xEE] {
            assert_eq!(logon(code), "Disconnected from server", "code {code:#04x}");
        }
    }

    /// **The two result enums are different enums**, and the same byte must not mean the same
    /// thing in both. 0x0C is `AUTH_LOGON_FAILED_SUSPENDED` to realmd and `AUTH_OK` to the world
    /// server; 0x05 is a bad password to realmd and nothing at all to the world server. A single
    /// `Option<u8>` could not tell them apart, which is why `LoginRefusal` exists.
    #[test]
    fn the_two_auth_enums_do_not_share_a_byte() {
        use benilla_protocol::messages as m;
        let strings = GlueStrings::default();
        let logon = |b| fail_text(&strings, Some(LoginRefusal::Logon(b)), None);
        let world = |b| fail_text(&strings, Some(LoginRefusal::World(b)), None);

        // 0x0C: "suspended" to realmd, and to the world server it is AUTH_OK — success, which
        // never reaches a refusal string at all.
        assert!(logon(0x0C).contains("temporarily suspended"));
        assert_eq!(world(m::AUTH_BILLING_ERROR), "Billing system error");
        assert_ne!(logon(0x0C), world(0x0C));

        // Every world row is the client's own dispatch (`w2b-pack.c`), transcribed.
        assert_eq!(world(m::AUTH_INCORRECT_PASSWORD), "Incorrect Password");
        assert_eq!(world(m::AUTH_SESSION_EXPIRED), "Session Expired");
        assert_eq!(world(m::AUTH_SERVER_SHUTTING_DOWN), "Server Shutting Down");
        assert_eq!(world(m::AUTH_ALREADY_LOGGING_IN), "Already Logging In");
        assert_eq!(
            world(m::AUTH_UNAVAILABLE),
            "System unavailable - Please try again later"
        );
        // The five codes the reference's URL dialog keys on — reachable only from this enum.
        assert!(world(m::AUTH_BANNED).contains("banned"));
        assert!(world(m::AUTH_SUSPENDED).contains("temporarily suspended"));
        assert!(world(m::AUTH_PARENTAL_CONTROL).contains("parental controls"));
        // A code this client does not know still lands on the authored catch-all.
        assert_eq!(world(0xEE), "Authentication failed");
    }

    /// **The report that prompted this** — a dead local server read as a bad address, twice, because
    /// "Unable to connect" cannot tell the two apart. Each dial verdict now gets the reference's own
    /// string for that exact condition, with the address under it so there is nothing left to guess.
    #[test]
    fn a_dial_failure_says_which_failure_it_was() {
        let strings = GlueStrings::default();
        let down = DialFailure {
            address: "127.0.0.1:3724".into(),
            unresolved: false,
        };
        assert_eq!(
            fail_text(&strings, None, Some(&down)),
            "Login Server Down\n127.0.0.1:3724",
            "the address resolved and nothing answered — editing the address will not help",
        );
        let missing = DialFailure {
            address: "logon.nonesuch.example:3724".into(),
            unresolved: true,
        };
        assert_eq!(
            fail_text(&strings, None, Some(&missing)),
            "Invalid Login Server\nlogon.nonesuch.example:3724",
            "the name resolved to nothing — this one IS the address",
        );
        // A refusal still wins its own authored string; the dial verdict only speaks for the dial.
        assert!(fail_text(&strings, Some(LoginRefusal::Logon(0x05)), None)
            .starts_with("The information you have entered is not valid."));
    }

    /// Save → load → clear round-trips through the dot-file (the ref's Get/SetSavedAccountName).
    #[test]
    fn saved_account_round_trips() {
        // A FILE, not a folder: since decision 1181 these two take the resolved path
        // (`local_state::saved_account_path`) rather than a base to join `account` onto.
        let dir = std::env::temp_dir().join(format!(
            "benilla-login-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let path = dir.join("account");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load_saved_account_from(&path), "");
        // The write creates the folder on its way, exactly as a first run must.
        save_account_to(&path, "ONE");
        assert_eq!(load_saved_account_from(&path), "ONE");
        save_account_to(&path, "");
        assert_eq!(load_saved_account_from(&path), "");
        assert!(!path.exists(), "clearing the name removes the file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The clock system toggles the FOCUSED box on the shared 0.5 s period and leaves the other
    /// one alone. It guards the system's behaviour, not its registration (that is the one
    /// `tick_login_caret` line in [`LoginPlugin`], and a login screen with no clock is
    /// indistinguishable from one whose caret is in its ON half forever — `caret_shown` defaults
    /// to `true`, which is exactly how this went unnoticed).
    #[test]
    fn the_focused_box_caret_blinks() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<LoginForm>()
            // The clock now asks which box owns the focus (1667): a dialog with an edit box takes
            // it. No dialog is open here, so the form's box keeps it — which is the case this
            // asserts.
            .init_resource::<LoginDialog>()
            .add_systems(Update, tick_login_caret);

        let past_the_period = |app: &mut App| {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_millis(600));
            app.update();
            app.world().resource::<LoginForm>().account.caret_shown
        };

        // Account has the focus by default; one period each way is on → off → on.
        assert!(!past_the_period(&mut app), "the first period turns it off");
        assert!(past_the_period(&mut app), "the second turns it back on");
        // The box that doesn't own the keyboard never accumulates, so switching focus to it lands
        // on a solid caret rather than wherever its own clock would have drifted to.
        assert_eq!(
            app.world().resource::<LoginForm>().password.blink_accum,
            0.0
        );
    }
}
