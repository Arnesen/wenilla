//! Character select — the faithful glue screen (decision 0465, superseding 0193 §4's v1 overlay).
//!
//! Owns [`ClientState`], the app's lifecycle state machine: `CharSelect` is the pre-world "glue"
//! layer (the real client's GlueXML universe — login/realm/select screens), `InWorld` is the game.
//! The IO thread parks after the world handshake and emits the account roster
//! ([`CharListMessage`]); the **pick policy** here decides what answers it — the pending pick
//! (seamless reconnect, decision 0065), the `WOW_CHAR` env fast path, or the director's choice on
//! the screen. The pick travels the [`CharPick`] channel; `Connected` flips us `InWorld`;
//! a `/logout` round-trip ([`LoggedOutMessage`]) flips back to select with the pending pick
//! cleared, and a **lost** session flips all the way back to the login screen (decision 1262 —
//! the reference's `DISCONNECTED_FROM_SERVER`).
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

mod addons;
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
    /// The screen this session opens on.
    ///
    /// A connected boot starts at [`ClientState::Login`] (decision 0539) and the roster's arrival
    /// flips it. A world capture starts straight at [`ClientState::InWorld`] — no net thread, no
    /// picker — so the deterministic scene harness is untouched by the glue layer. A **glue**
    /// capture starts on the glue screen it photographs, which is the reason this is a state and
    /// not the bool it used to be: there turned out to be three answers, not two.
    pub(crate) start: ClientState,
}

/// Mirror the session's screen onto the engine's one-bit "is there a world" fact.
///
/// One writer, not a line at each of the dozen sites that set [`ClientState`] — a mirror a future
/// transition can forget is worse than the coupling it replaced.
fn publish_world_live(
    state: Res<State<ClientState>>,
    mut live: ResMut<benilla_world::schedule::WorldLive>,
) {
    let now = benilla_world::schedule::WorldLive(*state.get() == ClientState::InWorld);
    if *live != now {
        *live = now;
    }
}

impl Plugin for CharSelectPlugin {
    fn build(&self, app: &mut App) {
        app.insert_state(self.start)
            .init_resource::<Roster>()
            // **The engine's world-existence bit** (1160's wire (b)): the session owner is this
            // module, so this module tells the world whether there is one. Ordered ahead of every
            // world stage so the fact and its falling edge are this frame's, not last frame's — which
            // is the whole reason `WorldLive` is a resource and not a mirrored state.
            .add_systems(
                Update,
                publish_world_live.before(benilla_world::schedule::WorldStage::Net),
            )
            .init_resource::<dialog::DeleteDialog>()
            .init_resource::<addons::AddonsPanel>()
            .add_systems(OnEnter(ClientState::CharSelect), screen::enter_select)
            .add_systems(OnExit(ClientState::CharSelect), screen::exit_select)
            .add_systems(Update, (debug_glue_roundtrip, debug_logout_smoke))
            .add_systems(
                Update,
                (
                    // The policy + transitions run in BOTH states: the roster auto-answer (reconnect
                    // relogin) happens while `InWorld`, and the logout edge arrives there too.
                    // `back_on_disconnect` LAST: a session that died during entry queues both
                    // edges into one frame, and the dead one has to win (decision 1262).
                    (
                        apply_roster_policy,
                        enter_on_connected,
                        back_on_logout,
                        back_on_disconnect,
                    )
                        .chain(),
                    (
                        screen::materialize_screen,
                        input::select_input,
                        input::rotate_model,
                        debug_select_dialog,
                        dialog::drive_delete_dialog,
                        // Before the list refresh, and before `select_input` reads a click that
                        // landed on the panel rather than the screen (decision 1196).
                        addons::drive_addons_panel,
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
                    .after(benilla_world::schedule::WorldStage::Net),
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
    /// A just-created character's name (decision 0423): its row gets selected (the ref's
    /// `SELECT_LAST_CHARACTER`, keyed by name so it survives the create/enum race). Armed by
    /// [`Roster::note_created`], which consumes it against the roster **already in hand** —
    /// [`apply_roster_policy`] only has to answer the other arrival order.
    just_created: Option<String>,
    /// The auth realm-list entry this session connected to (the screen's realm banner, 0465);
    /// refreshed with each roster.
    pub(super) realm: Option<benilla_protocol::RealmInfo>,
    /// `WOW_CHAR` read latch (env read once at first policy run).
    env_read: bool,
}

impl Roster {
    /// A roster with a pick already in flight — the state world entry runs in.
    ///
    /// `#[cfg(test)]` and `pub(crate)` because the fields are `pub(super)`: a test outside this
    /// module cannot build one, and `ui_script`'s does exactly that to drive
    /// `seat_from_roster` over a real row rather than a copy of its logic.
    #[cfg(test)]
    pub(crate) fn with_pending_pick(chars: Vec<Character>, guid: u64) -> Self {
        Self {
            chars,
            pending_pick: Some(guid),
            ..Self::default()
        }
    }

    /// Note a character the create screen just made, and select its row.
    ///
    /// **The consume happens here, not on the next roster message** (B119): the IO thread
    /// re-enumerates and emits the fresh roster *before* the create result (`net::io`), so by the
    /// time the result reaches [`crate::char_create`] the new row is normally already in
    /// [`Self::chars`] and [`apply_roster_policy`] has already run for that list. Arming a flag for
    /// "the next roster update" then waited for a message that never arrives again — the select
    /// screen came back on the previously-selected row (reproduced: create a tauren on a
    /// human-first account and the human is still standing there).
    ///
    /// The arm survives when the name isn't in hand yet, so the reverse arrival order (result
    /// first) is still answered by [`apply_roster_policy`].
    pub(crate) fn note_created(&mut self, name: String) {
        self.just_created = Some(name);
        self.select_created_by_name();
    }

    /// Select the armed just-created row **by name**, disarming only on a hit. Case-insensitive:
    /// vmangos normalizes a created name (`normalizePlayerName` — first letter upper, rest lower),
    /// so a typed `ZZBULL` enumerates back as `Zzbull` and an exact compare would miss.
    fn select_created_by_name(&mut self) {
        let Some(name) = self.just_created.as_deref() else {
            return;
        };
        if let Some(row) = self
            .chars
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
        {
            self.selected = Some(row);
            self.just_created = None;
        }
    }

    /// The selected character, if any.
    pub(super) fn selected_char(&self) -> Option<&Character> {
        self.selected.and_then(|i| self.chars.get(i))
    }

    /// The pending pick's map — the loading screen resolves the *destination's* art from the
    /// roster at the entry edge, before the server's `SMSG_LOGIN_VERIFY_WORLD` snap lands
    /// (decision 0737).
    pub(crate) fn pending_map(&self) -> Option<u32> {
        self.pending_row().map(|c| c.map)
    }

    /// The picked character's `(map, wow xyz)` — **where the world we are about to load actually
    /// is**, known from the roster row a whole server round-trip before `SMSG_LOGIN_VERIFY_WORLD`
    /// says so. The streamers aim at this during world entry (decision 0777); without it the only
    /// answer available before the snap is the hardcoded Northshire anchor
    /// ([`crate::SPAWN_XY`]), which is a guess that is wrong for every character who isn't a fresh
    /// human — and a wrong guess here does not merely idle, it spends the entry's IO budget
    /// decoding tiles that are dropped a moment later.
    pub(crate) fn pending_entry(&self) -> Option<(u32, [f32; 3])> {
        self.pending_row()
            .map(|c| (c.map, [c.position.x, c.position.y, c.position.z]))
    }

    /// The roster row for the pick in flight.
    ///
    /// `pub(crate)` because this row is the ONLY description of the character that exists during
    /// world entry: `Connected` flips us `InWorld` a whole server round-trip before the self
    /// descriptor streams in, and the in-game UI — plus every addon's file scope — materializes
    /// inside that window. `ui_script::load_ingame_ui_on_world_entry` seats
    /// `UnitName("player")` from here for exactly that reason.
    pub(crate) fn pending_row(&self) -> Option<&Character> {
        self.pending_pick
            .and_then(|g| self.chars.iter().find(|c| c.guid == g))
    }
}

/// Ask the parked IO thread to log in as `guid` (the pick channel) and remember it as pending.
/// `pub(crate)` for the probe rig (decision 0651), which picks a character it may have had to
/// *create* first — going through here is what keeps `pending_pick` truthful, so 0065's reconnect
/// re-answers with the rigged body rather than showing the roster.
pub(crate) fn send_pick(roster: &mut Roster, pick: &CharPick, guid: u64) {
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
    // Is the character pick already spoken for? Present only when a rig is driving this run
    // (decision 1174's always-present run fact) — absent in every ordinary run, which is the
    // player answer and the one this screen was written for.
    rig: Option<Res<crate::run_mode::RigCharacter>>,
) {
    if !roster.env_read {
        roster.env_read = true;
        // `WOW_RIG` with a body outranks `WOW_CHAR`: the rig picks its own derived character, and
        // it may have to CREATE it first, which this fast path (a one-shot `take()` that gives up
        // on a name it can't find) structurally cannot wait for.
        roster.env_char = match rig.as_deref() {
            Some(crate::run_mode::RigCharacter(name)) => {
                if let Ok(ignored) = std::env::var("WOW_CHAR") {
                    warn!("char select: WOW_RIG names {name} — ignoring WOW_CHAR={ignored}");
                }
                None
            }
            // `WOW_CHAR`, or the login smoke's optional third field (decision 1262): the smoke's
            // seat exists because `WOW_CHAR` is also an *unattended* marker
            // ([`crate::run_mode::unattended_login`]), so using it to reach the world would switch
            // the very branch a session test is trying to exercise. Same fast path, no marker.
            None => std::env::var("WOW_CHAR").ok().or_else(|| {
                std::env::var("WOW_LOGIN_SMOKE")
                    .ok()
                    .and_then(|s| crate::login::smoke_character(&s))
            }),
        };
    }
    for msg in msgs.read() {
        roster.chars = msg.characters.clone();
        roster.realm = msg.realm.clone();
        // A still-armed create (the result hasn't landed yet — the reverse of the order `net::io`
        // actually sends in) selects its row here: by name first, else the LAST row, which is the
        // ref's literal `SELECT_LAST_CHARACTER` → `SelectCharacter(numChars)` and lands on the same
        // character regardless — vmangos enumerates `ORDER BY create_time, guid`, so the new one is
        // last. Otherwise clamp into range, first row default.
        if roster.just_created.is_some() {
            roster.select_created_by_name();
            if let Some(name) = roster.just_created.take() {
                warn!(
                    "char select: created {name:?} is not on the fresh roster — selecting the last row"
                );
                roster.selected = roster.chars.len().checked_sub(1);
            }
        }
        if roster.chars.is_empty() {
            roster.selected = None;
        } else {
            let sel = roster.selected.unwrap_or(0).min(roster.chars.len() - 1);
            roster.selected = Some(sel);
        }
        // `WOW_CHARSELECT_PICK=<name>` — **select** that row and stay on the screen, the
        // deliberate opposite of `WOW_CHAR`'s enter-the-world fast path. Without it the screen is
        // only ever reachable on its default first row, which makes every "what does this
        // character look like at select?" check hostage to the account's create order (it cost
        // this session's item-glow verification a run). An unknown name leaves the default.
        if let Ok(name) = std::env::var("WOW_CHARSELECT_PICK") {
            match roster
                .chars
                .iter()
                .position(|c| c.name.eq_ignore_ascii_case(&name))
            {
                Some(row) => {
                    info!("char select: WOW_CHARSELECT_PICK={name} — selecting row {row}");
                    roster.selected = Some(row);
                }
                None => warn!("char select: WOW_CHARSELECT_PICK={name} not on this account"),
            }
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

/// **The session died** → all the way back to the login screen (decision 1262), which is where the
/// reference's `GlueParent.lua` puts it: `SetGlueScreen("login")` on `DISCONNECTED_FROM_SERVER`,
/// with [`crate::login`] raising the "Disconnected from server" dialog over it.
///
/// Chained **after** [`enter_on_connected`] on purpose. Both edges can land in the same frame — the
/// IO thread emits `Connected` before its read loop starts, so a stream that dies during entry
/// queues `Connected` and `Disconnected` back to back and the app drains both at once. That is the
/// race the report caught: the entry half won, and the client walked into a world whose session had
/// already been torn down — a camera with no avatar, `SelfGuid` set, an entry awaiting a snap that
/// could never arrive. Ordering it last makes the dead session the last word by construction.
fn back_on_disconnect(
    mut msgs: MessageReader<crate::net::DisconnectedMessage>,
    mut roster: ResMut<Roster>,
    mut next: ResMut<NextState<ClientState>>,
) {
    if !msgs.read().any(|m| m.session_over) {
        return;
    }
    // The pending pick is the reconnect's memory (0065). With no reconnect coming it would only
    // auto-answer the *next* roster — sending the player straight back into the world they were
    // just thrown out of, without ever seeing the screen.
    roster.pending_pick = None;
    next.set(ClientState::Login);
}

/// A confirmed `/logout` → back to the glue layer, pick cleared (the follow-up roster must be
/// shown, not auto-answered).
///
/// The in-world UI's input latches are released by [`crate::ui_script::end_ui_session`], on the
/// `OnExit(InWorld)` edge this transition causes: they stick because `feed_ui_input` stops running
/// outside `InWorld`, which is a fact about the edge and not about *why* we left it — so both this
/// path and the disconnect above get it from one place (1290).
fn back_on_logout(
    mut msgs: MessageReader<LoggedOutMessage>,
    mut roster: ResMut<Roster>,
    mut next: ResMut<NextState<ClientState>>,
) {
    if msgs.read().next().is_some() {
        roster.pending_pick = None;
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
/// world-teardown lines), **enter the world a second time**, and exit. Inert without the env.
///
/// The re-entry leg is not decoration. Since decision 0777 the world is *released* on the way out
/// (`terrain_stream::release_world`), so the second entry is a materially different code path from
/// the first — it streams a map the streamer has already torn down once — and a teardown that
/// forgets to reset its own bookkeeping fails exactly here and nowhere else. The world-audio
/// boundary this smoke was written for is still checked on the way through.
#[allow(clippy::too_many_arguments)] // a smoke test that drives the whole round trip
fn debug_logout_smoke(
    state: Res<State<ClientState>>,
    player: Res<crate::player::Player>,
    commands: Res<crate::net::NetCommands>,
    mut roster: ResMut<Roster>,
    pick: Res<CharPick>,
    streamer: Res<benilla_world::terrain_stream::TerrainStreamer>,
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
        // **The residency reading belongs HERE, not on arrival.** It used to print on the first
        // frame at CharSelect and report "25 tiles resident (must be 0)" every single run — a
        // permanent, alarming, wrong number. `release_world` runs ~150 ms later (it hangs off the
        // world-live falling edge, not the state edge), so the probe was reading the count before
        // the thing it is checking had happened, and reporting the world it had just left.
        //
        // It cost more than a wrong line: 1291 wrote the reading up as a live contradiction of
        // 0777's release-world claim, on the strength of it reproducing identically on an older
        // commit — which it did, because an instrument that measures too early does that reliably.
        // A number that is always wrong teaches everyone to skip the line (0777's own lesson about
        // tripwires nobody trips, from the other end).
        3 if now - *mark > 4.0 => match roster.chars.first().map(|c| (c.guid, c.name.clone())) {
            Some((guid, name)) => {
                info!(
                    "logout-smoke: {} tiles resident after the release (must be 0)",
                    streamer.residency().1
                );
                info!("logout-smoke: re-entering the world as {name}");
                send_pick(&mut roster, &pick, guid);
                (*phase, *mark) = (4, now);
            }
            None => {
                warn!("logout-smoke: empty roster — cannot test re-entry");
                *phase = 5;
            }
        },
        4 if *state.get() == ClientState::InWorld && player.active && now - *mark > 3.0 => {
            info!(
                "logout-smoke: re-entered — {} tiles resident, done",
                streamer.residency().1
            );
            *phase = 5;
        }
        5 => {
            info!("logout-smoke: done");
            exit.write(AppExit::Success);
            *phase = 6;
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
    dialog.typed.set_text(&typed);
    info!("char select: dialog instrument opened the delete confirm");
    *done = true;
}

/// Default delay before the select-screen shot fires (seconds from the screen coming up) — long
/// enough for the art, the glue scene and the geared model to settle.
const SELECT_SHOT_AT: f32 = 8.0;

/// The select-screen shot instrument (`WOW_CHARSELECT_SHOT_OUT=<path>`, decision 0465): once the
/// screen has been up a few seconds (art + scene + model settled), write one PNG of the window via
/// Bevy's own framebuffer readback — machine-checkable geometry without macOS screen-recording
/// permission. Inert without the env.
///
/// `WOW_CHARSELECT_SHOT_AT=<secs>` moves the shot (default [`SELECT_SHOT_AT`], measured from the
/// first frame the screen is up). The knob exists because the fixed 8 s lands *inside* the
/// create round-trip when the run is `WOW_CHARCREATE_SHOT` + `WOW_CHARCREATE_NAME`: the B119 retest
/// wants the screen a few seconds AFTER the new character lands, and it captured the frame 19 ms
/// after the create result — before the booth had re-baked the new selection.
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
    let at = std::env::var("WOW_CHARSELECT_SHOT_AT")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(SELECT_SHOT_AT);
    let start = *entered_at.get_or_insert(time.elapsed_secs());
    if time.elapsed_secs() - start < at {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn character(guid: u64, name: &str) -> Character {
        Character {
            guid,
            name: name.to_string(),
            race: 1,
            class: 1,
            gender: 0,
            skin: 0,
            face: 0,
            hair_style: 0,
            hair_color: 0,
            facial_hair: 0,
            level: 1,
            zone: 0,
            map: 0,
            position: benilla_protocol::wire::Vector3d {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            flags: 0,
            equipment: [benilla_protocol::CharEnumItem::default(); 19],
        }
    }

    /// **A session that dies during world entry does not leave the client in the world**
    /// (decision 1262).
    ///
    /// The IO thread emits `Connected` *before* its read loop starts, so a socket that dies mid-
    /// entry — which is exactly what a displacement kick looks like, a bare EOF and nothing else —
    /// queues `Connected` and `Disconnected` back to back, and the drain (`try_iter`) hands the app
    /// both in one frame. Both edges then fire in the same `Update`. Whichever writes `NextState`
    /// last wins, and before this the entry did: the client flipped `InWorld` against a world the
    /// same frame's teardown had already emptied, and stood there in a camera with no avatar.
    ///
    /// The guard is the *ordering*, which nothing else can catch: each system is correct alone, the
    /// build is green either way, and the failure needs two logins racing on one account to
    /// reproduce by hand.
    #[test]
    fn a_session_lost_during_entry_beats_the_entry() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::state::app::StatesPlugin))
            .insert_state(ClientState::CharSelect)
            .init_resource::<Roster>()
            .init_resource::<crate::ui_script::PlayerUiHover>()
            .init_resource::<crate::ui_script::UiKeyboardCapture>()
            .add_message::<EnteredWorldMessage>()
            .add_message::<crate::net::DisconnectedMessage>()
            .add_systems(Update, (enter_on_connected, back_on_disconnect).chain());
        app.world_mut().resource_mut::<Roster>().pending_pick = Some(7);

        // One frame carrying both halves of the race, in the order the drain produces them.
        app.world_mut().write_message(EnteredWorldMessage);
        app.world_mut()
            .write_message(crate::net::DisconnectedMessage {
                reason: "disconnected: world stream closed: failed to fill whole buffer".into(),
                end: benilla_protocol::SessionEnd::Lost,
                session_over: true,
            });
        app.update();
        app.update(); // `StateTransition` applies the pending state at the next frame

        assert_eq!(
            *app.world().resource::<State<ClientState>>().get(),
            ClientState::Login,
            "the dead session must be the last word — the reference's DISCONNECTED_FROM_SERVER \
             puts the client back on the account screen, not into a world with no session",
        );
        assert_eq!(
            app.world().resource::<Roster>().pending_pick,
            None,
            "and the reconnect's pending pick goes with it, or the next roster walks straight \
             back into the world the player was just thrown out of",
        );
    }

    /// The same frame with the session *not* over (a clean logout's teardown, or an unattended run
    /// keeping 0065's seamless reconnect) still enters the world — the guard above must not have
    /// turned every disconnect into a bounce to the login screen.
    #[test]
    fn a_teardown_that_is_not_the_end_leaves_the_entry_alone() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::state::app::StatesPlugin))
            .insert_state(ClientState::CharSelect)
            .init_resource::<Roster>()
            .init_resource::<crate::ui_script::PlayerUiHover>()
            .init_resource::<crate::ui_script::UiKeyboardCapture>()
            .add_message::<EnteredWorldMessage>()
            .add_message::<crate::net::DisconnectedMessage>()
            .add_systems(Update, (enter_on_connected, back_on_disconnect).chain());

        app.world_mut().write_message(EnteredWorldMessage);
        app.world_mut()
            .write_message(crate::net::DisconnectedMessage {
                reason: "logged out".into(),
                end: benilla_protocol::SessionEnd::LoggedOut,
                session_over: false,
            });
        app.update();
        app.update();

        assert_eq!(
            *app.world().resource::<State<ClientState>>().get(),
            ClientState::InWorld,
        );
    }

    /// B119 — a created character is selected against the roster **already in hand**. `net::io`
    /// re-enumerates and emits the fresh roster BEFORE the create result, so by the time
    /// `note_created` runs the row exists and the roster message is spent; a version that only armed
    /// a flag for "the next roster update" left the previously-selected row standing (reproduced
    /// live: create a tauren on a human-first account, the human stays on the stage).
    #[test]
    fn a_created_character_is_selected_from_the_roster_in_hand() {
        let mut roster = Roster {
            chars: vec![
                character(1, "Kerwind"),
                character(2, "Xero"),
                character(3, "Zzbullone"), // the fresh row, appended by the re-enum
            ],
            selected: Some(0),
            ..default()
        };
        roster.note_created("Zzbullone".to_string());
        assert_eq!(roster.selected, Some(2), "the new row must be selected");
        assert!(roster.just_created.is_none(), "and the arm consumed");
    }

    /// The name key is case-insensitive: vmangos normalizes a created name
    /// (`normalizePlayerName` — first letter upper, rest lower), so the string the create screen
    /// typed is NOT what the enum sends back.
    #[test]
    fn the_created_name_matches_the_servers_normalized_spelling() {
        let mut roster = Roster {
            chars: vec![character(1, "Kerwind"), character(2, "Zzbullone")],
            selected: Some(0),
            ..default()
        };
        roster.note_created("ZZBULLONE".to_string()); // as typed
        assert_eq!(roster.selected, Some(1));
    }

    /// The reverse arrival order (result first, roster after) stays armed and is answered by the
    /// roster policy — and a name that still can't be found falls back to the LAST row, which is the
    /// ref's literal `SELECT_LAST_CHARACTER` → `SelectCharacter(numChars)` and the same character
    /// anyway (vmangos enumerates `ORDER BY create_time, guid`).
    #[test]
    fn an_unanswerable_create_stays_armed() {
        let mut roster = Roster {
            chars: vec![character(1, "Kerwind")],
            selected: Some(0),
            ..default()
        };
        roster.note_created("Zzbulltwo".to_string());
        assert_eq!(
            roster.just_created.as_deref(),
            Some("Zzbulltwo"),
            "no row to select yet — the arm must survive for the roster policy"
        );
        assert_eq!(roster.selected, Some(0), "and nothing is mis-selected");
    }
}
