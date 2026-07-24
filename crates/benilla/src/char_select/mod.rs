//! Character select — the faithful glue screen (decision 0465, superseding 0193 §4's v1 overlay).
//!
//! Owns [`ClientState`], the app's lifecycle state machine: `CharSelect` is the pre-world "glue"
//! layer (the real client's GlueXML universe — login/realm/select screens), `InWorld` is the game.
//! The IO thread parks after the world handshake and emits the account roster
//! ([`CharListMessage`]); the **pick policy** here decides what answers it — the pending pick
//! (seamless reconnect, decision 0065), the `WOW_CHAR` env fast path, or the director's choice on
//! the screen. The pick travels the [`CharPick`] channel; `Connected` flips us `InWorld`;
//! a `/logout` round-trip ([`LoggedOutMessage`]) flips back with the pending pick cleared.
//!
//! The screen is the reference's own arrangement (`CharacterSelect.xml/.lua`, extracted off the
//! patch chain — decision 0465): the fullscreen `UI_<Race>` glue scene with the **selected
//! character standing in it, geared from its enum record** (the glue booth), the right-column
//! character list (realm banner, ten row buttons, Create New Character), Enter World / Back /
//! Delete Character along the bottom, the rotate pair, drag-to-rotate, arrow-key cycling,
//! double-click-to-enter, and the typed-`DELETE` confirm dialog. Art/strings/sounds come off the
//! player's own client data ([`crate::glue`]) — never embedded.
//!
//! Module split: this file (state machine + roster policy + shared display constants),
//! [`screen`] (the authored layout), [`refresh`] (list/banner/booth-feed refresh), [`input`]
//! (clicks, keys, rotation, the flows), [`dialog`] (the delete confirm).

mod dialog;
mod input;
mod refresh;
mod screen;

use benilla_protocol::{CharAction, Character};
use bevy::prelude::*;

use crate::net::{
    CharActionResultMessage, CharListMessage, CharPick, CharRequest, EnteredWorldMessage,
    LoggedOutMessage,
};

/// The app's lifecycle: which screen owns the session (decision 0193). Grows glue variants
/// (`RealmList`, …) as the glue arc fills in.
#[derive(States, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum ClientState {
    /// Parked pre-logon at the login screen (decision 0539): the IO thread waits for credentials;
    /// [`crate::login`]'s policy decides what answers it (the env fast path, the reconnect
    /// resubmit, or the director's typed submit).
    #[default]
    Login,
    /// Parked at character select: the select screen is up, the IO thread waits for a pick, and
    /// the in-world input surfaces (player controller, FrameXML keyboard) are gated off.
    CharSelect,
    /// The character-creation screen (decision 0423): still parked at select (the IO thread
    /// services create/delete in place), a sibling glue screen. Entered from the select screen's
    /// "Create New Character" button; Back returns to `CharSelect`. In-world input stays gated off
    /// (any `in_state(InWorld)` system is off here, mechanically).
    CharCreate,
    /// A character is in (or entering) the world.
    InWorld,
}

/// The character-select subsystem: the state machine + the select screen.
pub(crate) struct CharSelectPlugin {
    /// Capture mode boots straight `InWorld` (no net thread, no picker) so the deterministic
    /// scene harness is untouched by the glue layer.
    pub(crate) start_in_world: bool,
}

impl Plugin for CharSelectPlugin {
    fn build(&self, app: &mut App) {
        app.insert_state(if self.start_in_world {
            ClientState::InWorld
        } else {
            // Connected boots start at the login screen (decision 0539); the roster's arrival
            // flips to CharSelect (`crate::login::to_select_on_roster`).
            ClientState::Login
        })
        .init_resource::<Roster>()
        .init_resource::<dialog::DeleteDialog>()
        .add_systems(OnEnter(ClientState::CharSelect), screen::enter_select)
        .add_systems(OnExit(ClientState::CharSelect), screen::exit_select)
        .add_systems(Update, (debug_glue_roundtrip, debug_logout_smoke))
        .add_systems(
            Update,
            (
                // The policy + transitions run in BOTH states: the roster auto-answer (reconnect
                // relogin) happens while `InWorld`, and the logout edge arrives there too.
                (apply_roster_policy, enter_on_connected, back_on_logout).chain(),
                (
                    screen::materialize_screen,
                    input::select_input,
                    input::rotate_model,
                    debug_select_dialog,
                    dialog::drive_delete_dialog,
                    refresh::refresh_list,
                    refresh::refresh_banner_and_buttons,
                    refresh::feed_glue_preview,
                    crate::glue::art_swaps,
                    crate::glue::glue_button_visuals,
                    delete_result,
                    debug_select_shot,
                    crate::glue::sync_outlines,
                )
                    .chain()
                    .run_if(in_state(ClientState::CharSelect)),
            )
                .chain()
                .after(crate::schedule::WorldStage::Net),
        );
    }
}

// ── State: the roster + pick policy ──────────────────────────────────────────────────────────────

/// The account roster + the pick policy's memory. `pending_pick` is the character we asked to log
/// in as — kept while in-world so a reconnect's fresh roster is auto-answered with it (decision
/// 0065's seamless relogin); cleared by a deliberate logout so the roster is *shown* instead.
#[derive(Resource, Default)]
pub(crate) struct Roster {
    pub(super) chars: Vec<Character>,
    /// The selected row (the ref's `CharacterSelect.selectedIndex`, 0-based; `None` = empty list).
    /// A fresh roster clamps it into range and defaults to the first row (the ref's law).
    pub(super) selected: Option<usize>,
    /// The guid we answered the IO thread with; `Some` = a login is requested/live.
    pub(super) pending_pick: Option<u64>,
    /// `WOW_CHAR`, when explicitly set: auto-pick this name on the FIRST roster (the dev fast
    /// path past the screen). `take()`n once — a later `/logout` shows the screen normally.
    env_char: Option<String>,
    /// A just-created character's name (decision 0423): the next roster update selects its row
    /// (the ref's `SELECT_LAST_CHARACTER`, keyed by name so it survives the create/enum race).
    just_created: Option<String>,
    /// The auth realm-list entry this session connected to (the screen's realm banner, 0465);
    /// refreshed with each roster.
    pub(super) realm: Option<benilla_protocol::RealmInfo>,
    /// `WOW_CHAR` read latch (env read once at first policy run).
    env_read: bool,
}

impl Roster {
    /// Note a character the create screen just made, so the next roster update selects its row.
    pub(crate) fn note_created(&mut self, name: String) {
        self.just_created = Some(name);
    }

    /// The selected character, if any.
    pub(super) fn selected_char(&self) -> Option<&Character> {
        self.selected.and_then(|i| self.chars.get(i))
    }
}

/// Ask the parked IO thread to log in as `guid` (the pick channel) and remember it as pending.
fn send_pick(roster: &mut Roster, pick: &CharPick, guid: u64) {
    roster.pending_pick = Some(guid);
    let _ = pick.0.send(CharRequest::Enter(guid));
}

/// Drain each [`CharListMessage`] into the roster, then decide what answers it: the pending pick
/// (reconnect), the `WOW_CHAR` fast path (first roster only), or nothing — the screen waits for
/// the director. A shown roster clamps the selection into range and defaults it to the first row
/// (the ref's `UpdateCharacterList`).
fn apply_roster_policy(
    mut msgs: MessageReader<CharListMessage>,
    mut roster: ResMut<Roster>,
    pick: Res<CharPick>,
) {
    if !roster.env_read {
        roster.env_read = true;
        roster.env_char = std::env::var("WOW_CHAR").ok();
    }
    for msg in msgs.read() {
        roster.chars = msg.characters.clone();
        roster.realm = msg.realm.clone();
        // Select the just-created character's row (the ref's SELECT_LAST_CHARACTER — ours keys by
        // name so it survives the create → enum race), else clamp into range, first row default.
        if let Some(name) = roster.just_created.take() {
            roster.selected = roster.chars.iter().position(|c| c.name == name);
        }
        if roster.chars.is_empty() {
            roster.selected = None;
        } else {
            let sel = roster.selected.unwrap_or(0).min(roster.chars.len() - 1);
            roster.selected = Some(sel);
        }
        if let Some(guid) = roster.pending_pick {
            // We already chose this session (in-world reconnect, or a pick raced a dying socket):
            // re-answer without showing the screen.
            send_pick(&mut roster, &pick, guid);
        } else if let Some(name) = roster.env_char.take() {
            match roster
                .chars
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(&name))
            {
                Some(c) => {
                    let guid = c.guid;
                    info!("char select: WOW_CHAR={name} — fast path");
                    send_pick(&mut roster, &pick, guid);
                }
                None => warn!("char select: WOW_CHAR={name} not on this account — showing roster"),
            }
        }
    }
}

/// `Connected` (bridged as [`EnteredWorldMessage`]) → the world owns the session.
fn enter_on_connected(
    mut msgs: MessageReader<EnteredWorldMessage>,
    mut next: ResMut<NextState<ClientState>>,
) {
    if msgs.read().next().is_some() {
        next.set(ClientState::InWorld);
    }
}

/// A confirmed `/logout` → back to the glue layer, pick cleared (the follow-up roster must be
/// shown, not auto-answered). Also releases the in-world UI's input latches — `feed_ui_input`
/// stops running outside `InWorld`, so whatever it last wrote would otherwise stick.
fn back_on_logout(
    mut msgs: MessageReader<LoggedOutMessage>,
    mut roster: ResMut<Roster>,
    mut next: ResMut<NextState<ClientState>>,
    mut ui_hover: ResMut<crate::ui_script::PlayerUiHover>,
    mut ui_keys: ResMut<crate::ui_script::UiKeyboardCapture>,
) {
    if msgs.read().next().is_some() {
        roster.pending_pick = None;
        ui_hover.0 = None;
        ui_keys.0 = false;
        next.set(ClientState::CharSelect);
    }
}

/// Surface a refused delete (the roster refresh already reflects a success — the row vanishes).
/// A refusal is realistically unreachable on vmangos (any enumerated character deletes), so a log
/// line honest-flags it rather than growing an error dialog nothing can trigger.
fn delete_result(mut msgs: MessageReader<CharActionResultMessage>) {
    for msg in msgs.read() {
        if msg.action == CharAction::Delete
            && msg.code != benilla_protocol::messages::CHAR_DELETE_SUCCESS
        {
            warn!("char select: delete refused (code {:#04x})", msg.code);
        }
    }
}

/// Glue-flow smoke (`WOW_GLUE_ROUNDTRIP=1`, decision 0423): once a real roster is up, bounce
/// CharSelect → CharCreate → **Back** → CharSelect and exit — so the return-to-select rebuild is
/// provable headlessly from the logs. Runs ungated (it crosses states); inert without the env.
fn debug_glue_roundtrip(
    roster: Res<Roster>,
    state: Res<State<ClientState>>,
    mut next: ResMut<NextState<ClientState>>,
    time: Res<Time>,
    mut exit: MessageWriter<AppExit>,
    mut phase: Local<u8>,
    mut mark: Local<f32>,
) {
    if std::env::var("WOW_GLUE_ROUNDTRIP").is_err() {
        return;
    }
    let now = time.elapsed_secs();
    match *phase {
        0 if !roster.chars.is_empty() && *state.get() == ClientState::CharSelect => {
            info!(
                "glue-roundtrip: initial roster = {} char(s) → entering CharCreate",
                roster.chars.len()
            );
            next.set(ClientState::CharCreate);
            (*phase, *mark) = (1, now);
        }
        1 if *state.get() == ClientState::CharCreate && now - *mark > 1.5 => {
            info!("glue-roundtrip: in CharCreate → Back to CharSelect");
            next.set(ClientState::CharSelect);
            (*phase, *mark) = (2, now);
        }
        2 if *state.get() == ClientState::CharSelect && now - *mark > 1.5 => {
            info!(
                "glue-roundtrip: back at CharSelect, roster = {} char(s) — done",
                roster.chars.len()
            );
            exit.write(AppExit::Success);
            *phase = 3;
        }
        _ => {}
    }
}

/// The logout-boundary smoke (`WOW_LOGOUT_SMOKE=1`, meant with the `WOW_CHAR` fast path): once
/// seated in the world, linger, request the `/logout` round-trip, confirm the return to
/// CharSelect, linger again (the glue theme should be the only thing audible over the logs'
/// world-teardown lines), and exit — the world-audio boundary is provable end-to-end without a
/// hand on the keyboard. Inert without the env.
fn debug_logout_smoke(
    state: Res<State<ClientState>>,
    player: Res<crate::player::Player>,
    commands: Res<crate::net::NetCommands>,
    time: Res<Time>,
    mut exit: MessageWriter<AppExit>,
    mut phase: Local<u8>,
    mut mark: Local<f32>,
) {
    if std::env::var("WOW_LOGOUT_SMOKE").is_err() {
        return;
    }
    let now = time.elapsed_secs();
    match *phase {
        0 if *state.get() == ClientState::InWorld && player.active => {
            info!("logout-smoke: seated in world — lingering");
            (*phase, *mark) = (1, now);
        }
        1 if now - *mark > 3.0 => {
            info!("logout-smoke: requesting logout");
            let _ = commands.0.send(crate::net::ClientCommand::Logout);
            *phase = 2;
        }
        2 if *state.get() == ClientState::CharSelect => {
            info!("logout-smoke: back at character select — lingering");
            (*phase, *mark) = (3, now);
        }
        3 if now - *mark > 4.0 => {
            info!("logout-smoke: done");
            exit.write(AppExit::Success);
            *phase = 4;
        }
        _ => {}
    }
}

/// The shot instrument's delete-dialog dial (`WOW_CHARSELECT_DIALOG=<typed>`): open the
/// typed-confirm dialog for the selected character a few seconds after the screen is up, with
/// `<typed>` pre-typed (may be empty) — so the dialog's geometry (the ChatInputBorder edit box,
/// the caret bar) is capturable headlessly. Pair with `WOW_CHARSELECT_SHOT_OUT`; inert without
/// the env; fires once.
fn debug_select_dialog(
    roster: Res<Roster>,
    mut dialog: ResMut<dialog::DeleteDialog>,
    time: Res<Time>,
    mut entered_at: Local<Option<f32>>,
    mut done: Local<bool>,
) {
    if *done {
        return;
    }
    let Ok(typed) = std::env::var("WOW_CHARSELECT_DIALOG") else {
        *done = true;
        return;
    };
    let start = *entered_at.get_or_insert(time.elapsed_secs());
    if time.elapsed_secs() - start < 4.0 {
        return;
    }
    let Some(c) = roster.selected_char() else {
        return; // roster not in yet — keep waiting
    };
    let (guid, name, level, class) = (c.guid, c.name.clone(), c.level, class_name(c.class));
    dialog.open_for(guid, name, level, class);
    dialog.typed = typed;
    info!("char select: dialog instrument opened the delete confirm");
    *done = true;
}

/// The select-screen shot instrument (`WOW_CHARSELECT_SHOT_OUT=<path>`, decision 0465): once the
/// screen has been up a few seconds (art + scene + model settled), write one PNG of the window via
/// Bevy's own framebuffer readback — machine-checkable geometry without macOS screen-recording
/// permission. Inert without the env.
fn debug_select_shot(
    mut commands: Commands,
    time: Res<Time>,
    mut entered_at: Local<Option<f32>>,
    mut done: Local<bool>,
) {
    if *done {
        return;
    }
    let Ok(out) = std::env::var("WOW_CHARSELECT_SHOT_OUT") else {
        *done = true;
        return;
    };
    let start = *entered_at.get_or_insert(time.elapsed_secs());
    if time.elapsed_secs() - start < 8.0 {
        return;
    }
    use bevy::render::view::screenshot::{save_to_disk, Screenshot};
    commands
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(out.clone()));
    info!("char select: shot instrument writing {out}");
    *done = true;
}

/// The WoW UI font, straight off the patch chain (Bevy's TTF loader over the `mpq://` source).
pub(crate) fn wow_font(assets: &AssetServer) -> Handle<Font> {
    assets.load("mpq://Fonts/FRIZQT__.ttf")
}

// ── 1.12 display constants (frozen facts of the build; display only) ────────────────────────────

pub(crate) fn race_name(race: u8) -> &'static str {
    match race {
        1 => "Human",
        2 => "Orc",
        3 => "Dwarf",
        4 => "Night Elf",
        5 => "Undead",
        6 => "Tauren",
        7 => "Gnome",
        8 => "Troll",
        _ => "?",
    }
}

pub(crate) fn class_name(class: u8) -> &'static str {
    match class {
        1 => "Warrior",
        2 => "Paladin",
        3 => "Hunter",
        4 => "Rogue",
        5 => "Priest",
        7 => "Shaman",
        8 => "Mage",
        9 => "Warlock",
        11 => "Druid",
        _ => "?",
    }
}
