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

mod screen;
mod smoke;

use std::sync::atomic::Ordering;

use benilla_ui::widget::EditBoxState;
use bevy::input::keyboard::KeyboardInput;

use crate::textinput::{self, HostClipboard};
use bevy::input::ButtonState;
use bevy::prelude::*;

use benilla_protocol::LoginStage;

use crate::char_select::ClientState;
use crate::glue_strings::GlueStrings;
use crate::net::{
    CharListMessage, DisconnectedMessage, LoginAbandon, LoginFailedMessage, LoginRequest,
    LoginStageMessage, LoginSubmit,
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

/// Send one login attempt to the parked IO thread, stamped with the current abandon generation.
fn send_login(
    intent: &mut LoginIntent,
    submit: &LoginSubmit,
    abandon: &LoginAbandon,
    user: &str,
    pass: &str,
    announced: bool,
) {
    intent.creds = Some((user.to_string(), pass.to_string()));
    intent.in_flight = true;
    intent.announced = announced;
    intent.retry_at = None;
    let _ = submit.0.try_send(LoginRequest {
        user: user.to_string(),
        pass: pass.to_string(),
        generation: abandon.0.load(Ordering::SeqCst),
    });
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
fn fail_text(strings: &GlueStrings, code: Option<u8>) -> &str {
    let (key, fallback): (&str, &str) = match code {
        None => ("LOGIN_FAILED", "Unable to connect"),
        Some(0x03) => ("AUTH_BANNED", "This account has been banned"),
        // vmangos sends 0x04 for unknown account AND wrong password (its AuthCodes.h comment:
        // the client locks out after an 0x05).
        Some(0x04) => ("AUTH_UNKNOWN_ACCOUNT", "Unknown account"),
        Some(0x05) => ("AUTH_INCORRECT_PASSWORD", "Incorrect Password"),
        Some(0x06) => ("AUTH_ALREADY_ONLINE", "This account is already logged in"),
        Some(0x07) => ("AUTH_NO_TIME", "Your subscription has expired"),
        Some(0x08) => ("AUTH_DB_BUSY", "This session has timed out"),
        Some(0x09) => ("AUTH_VERSION_MISMATCH", "Wrong client version"),
        Some(0x0B) => ("LOGIN_FAILED", "Unable to connect"),
        Some(0x0C) => (
            "AUTH_SUSPENDED",
            "This account has been temporarily suspended",
        ),
        Some(0x0D) => ("AUTH_REJECT", "Login unavailable"),
        Some(0x0F) => ("AUTH_PARENTAL_CONTROL", "Blocked by parental controls"),
        Some(_) => ("AUTH_FAILED", "Authentication failed"),
    };
    strings.text(key, fallback)
}

/// The policy tick + the net-message reactions. Runs in every state (the reconnect path fires
/// while `InWorld`); the screen's own submit comes through [`login_input`], which calls
/// [`send_login`] with `announced = true`.
#[allow(clippy::too_many_arguments)]
fn drive_policy(
    mut intent: ResMut<LoginIntent>,
    mut dialog: ResMut<LoginDialog>,
    submit: Res<LoginSubmit>,
    abandon: Res<LoginAbandon>,
    strings: Option<Res<GlueStrings>>,
    time: Res<Time>,
    mut stages: MessageReader<LoginStageMessage>,
    mut failures: MessageReader<LoginFailedMessage>,
    mut disconnects: MessageReader<DisconnectedMessage>,
    mut exit: MessageWriter<AppExit>,
) {
    let now = time.elapsed_secs();
    // A harness run (env creds, and not the smoke — the smoke owns its own verdict) has nobody at
    // the keyboard: a login failure no resubmit can change leaves it parked on a dialog for its
    // whole wall-clock, and every retry a runner grants it is spent the same way. Those failures
    // exit non-zero instead, on one greppable marker — "login: FATAL" — that leg.sh keys on.
    let harness =
        crate::run_mode::unattended_login() && std::env::var_os("WOW_LOGIN_SMOKE").is_none();
    let empty = GlueStrings::default();
    let strings = strings.as_deref().unwrap_or(&empty);

    // The env fast path, once (decision 0539 §3): any of WOW_USER/WOW_PASS/WOW_CHAR explicitly
    // set → auto-submit env-with-defaults, so every probe/smoke/harness invocation keeps working.
    // The login smoke drives its own credentials instead.
    if !intent.env_read {
        intent.env_read = true;
        // The same fact a lost session asks about (decision 1262) — read from the one place that
        // owns it, so "the harness logs in for us" and "the harness logs back in for us" can never
        // be two different answers.
        if crate::run_mode::unattended_login() && std::env::var_os("WOW_LOGIN_SMOKE").is_none() {
            let user = crate::webenv::var("WOW_USER").unwrap_or_else(|| "one".into());
            let pass = crate::webenv::var("WOW_PASS").unwrap_or_else(|| "pone".into());
            // The account guard (decision 0649): a vmangos login KICKS whoever holds the account,
            // so an unattended run from a pool slot must not authenticate as the director's `one`
            // or a neighbouring slot's probe. Only the *automated* path is gated — a typed login
            // is the director's own and is never second-guessed.
            match crate::run_mode::account_guard(&user) {
                Ok(()) => {
                    info!("login: env fast path — auto-submitting as {user}");
                    intent.creds = Some((user, pass));
                    intent.retry_at = Some(now);
                }
                Err(why) if std::env::var_os("WOW_ALLOW_ACCOUNT").is_some() => {
                    warn!("login: {why} — WOW_ALLOW_ACCOUNT is set, going ahead anyway");
                    intent.creds = Some((user, pass));
                    intent.retry_at = Some(now);
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
    for msg in failures.read() {
        intent.in_flight = false;
        intent.park = IoPark::AtLogin;
        // A terminal failure names something no resubmit can change (the server requires Warden,
        // say) — show the server's own words and drop the credentials so nothing retries.
        if msg.terminal {
            warn!("login: {}", msg.reason);
            intent.clear();
            dialog.open_error(&msg.reason);
            if harness {
                error!("login: FATAL — terminal login failure with nobody at the keyboard ({}); exiting", msg.reason);
                exit.write(AppExit::error());
            }
            continue;
        }
        match msg.code {
            Some(code) => {
                // A refusal: surface it (even on the silent path — the credentials went stale)
                // and never auto-retry against it.
                warn!("login: refused (code {code:#04x}) — {}", msg.reason);
                intent.clear();
                dialog.open_error(fail_text(strings, Some(code)));
                if harness {
                    error!("login: FATAL — refused (code {code:#04x}) and no resubmit can change it; exiting");
                    exit.write(AppExit::error());
                }
            }
            None if intent.announced => {
                warn!("login: {}", msg.reason);
                dialog.open_error(fail_text(strings, None));
            }
            None => {
                // Silent transport failure with pending intent: schedule the paced resubmit.
                debug!("login: transport failure ({}) — retrying", msg.reason);
                if intent.creds.is_some() {
                    intent.retry_at = Some(now + RETRY_DELAY_SECS);
                }
            }
        }
    }
    for msg in disconnects.read() {
        // The IO thread is heading back to its pre-logon park.
        intent.park = IoPark::AtLogin;
        intent.in_flight = false;
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
            intent.clear();
            dialog.open_error(strings.text("DISCONNECTED", "Disconnected from server"));
            continue;
        }
        // Otherwise the session continues through the park and the re-auth is silent: immediate
        // after a clean logout (the roster IS the select screen the app now shows), paced after a
        // stream death an unattended run must recover from on its own (0065, paced app-side).
        if intent.creds.is_some() {
            let delay = if msg.end == benilla_protocol::SessionEnd::LoggedOut {
                0.0
            } else {
                RETRY_DELAY_SECS
            };
            intent.retry_at = Some(now + delay);
        }
    }

    // The silent (re)submit tick.
    if intent.park == IoPark::AtLogin
        && !intent.in_flight
        && intent.retry_at.is_some_and(|t| now >= t)
    {
        if let Some((user, pass)) = intent.creds.clone() {
            send_login(&mut intent, &submit, &abandon, &user, &pass, false);
        } else {
            intent.retry_at = None;
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
#[derive(Default, Clone, Copy, PartialEq, Eq)]
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
    // wouldn't do it: it no-ops when the saved name is already in the box).
    form.focused().reset_blink();
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
    mut intent: ResMut<LoginIntent>,
    mut dialog: ResMut<LoginDialog>,
    submit: Res<LoginSubmit>,
    abandon: Res<LoginAbandon>,
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
                // A click takes the focus the same way TAB does — including the solid caret the
                // fresh focus starts on, so the box you just clicked never answers with a blank
                // half-period.
                LoginAction::FocusAccount => {
                    form.focus = Field::Account;
                    form.focused().reset_blink();
                }
                LoginAction::FocusPassword => {
                    form.focus = Field::Password;
                    form.focused().reset_blink();
                }
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
            LoginAction::Dialog => {} // the dialog driver's
        }
    }

    let mods = textinput::mods_now(&keys);
    let wl = textinput::wayland_display(raw_handle.iter().next());
    for ev in keyboard.read() {
        if dialog_open {
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
            form.focus = match form.focus {
                Field::Account => Field::Password,
                Field::Password => Field::Account,
            };
            form.focused().reset_blink();
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

    if do_login && !intent.in_flight {
        // The ref's own guards: empty account / empty password get their dialog, no wire.
        if form.account.text.is_empty() {
            dialog.open_error(strings.text("LOGIN_ENTER_NAME", "Please enter your account name."));
        } else if form.password.text.is_empty() {
            dialog.open_error(strings.text("LOGIN_ENTER_PASSWORD", "Please enter your password."));
        } else {
            sounds.write(GlueSound("gsLogin"));
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
            send_login(&mut intent, &submit, &abandon, &user, &pass, true);
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
fn tick_login_caret(mut form: ResMut<LoginForm>, time: Res<Time>) {
    let dt = time.delta_secs();
    textinput::tick_caret(form.focused(), true, dt);
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

/// Which dialog is up: the connecting status (Cancel button, text driven by the stages) or an
/// error (Okay button).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DialogKind {
    Status,
    Error,
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
    abandon: Res<LoginAbandon>,
    art: Res<crate::glue::art::GlueArt>,
    assets: Res<AssetServer>,
    strings: Option<Res<GlueStrings>>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Query<(Entity, &LoginAction)>,
    clicks: Res<crate::glue::GlueClicks>,
    mut texts: Query<&mut Text, With<screen::DialogText>>,
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
    let s = crate::glue::screen_scale(window.single().ok());
    if dialog.root.is_none() || dialog.spawned != Some(kind) || dialog.spawned_s != s {
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

    // The one button (or its keys).
    let mut pressed = buttons
        .iter()
        .any(|(e, a)| matches!(a, LoginAction::Dialog) && clicks.hit(e));
    if keys.just_pressed(KeyCode::Escape) {
        pressed = true;
    }
    if kind == DialogKind::Error
        && (keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::NumpadEnter))
    {
        pressed = true;
    }
    if pressed {
        if kind == DialogKind::Status {
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
    fn policy_app() -> App {
        let (tx, rx) = async_channel::unbounded();
        // Held for the App's life: a dropped receiver would turn every submit into an `Err` and
        // hide a policy that sent one.
        std::mem::forget(rx);
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<LoginIntent>()
            .init_resource::<LoginDialog>()
            .insert_resource(LoginSubmit(tx))
            .insert_resource(LoginAbandon(std::sync::Arc::new(
                std::sync::atomic::AtomicU64::new(0),
            )))
            .add_message::<LoginStageMessage>()
            .add_message::<LoginFailedMessage>()
            .add_message::<DisconnectedMessage>()
            .add_systems(Update, drive_policy);
        app.world_mut().resource_mut::<LoginIntent>().env_read = true;
        app
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
        let mut app = policy_app();
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
        let mut app = policy_app();
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
        let mut app = policy_app();
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

    /// The code→string map quotes the client's own strings for the vmangos-verified rows.
    #[test]
    fn fail_text_maps_the_verified_codes() {
        let strings = GlueStrings::default(); // empty table → the fallback literals
        assert_eq!(fail_text(&strings, Some(0x04)), "Unknown account");
        assert_eq!(fail_text(&strings, Some(0x05)), "Incorrect Password");
        assert_eq!(fail_text(&strings, None), "Unable to connect");
        assert_eq!(fail_text(&strings, Some(0x09)), "Wrong client version");
        assert_eq!(fail_text(&strings, Some(0xEE)), "Authentication failed");
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
