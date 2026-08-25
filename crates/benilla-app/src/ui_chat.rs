//! The chat system (decision 0288), the reference's own three-stage shape: [`event`] is the
//! internal currency (`CHAT_MSG_*` typed — kinds, groups, the shipped color table); [`feed`] turns
//! every source (wire lines, channel notices, rolls, client-composed loot/system lines) into
//! events with names resolved ask-once; [`frames`] is the router + the `ChatFrame_OnEvent`
//! composer fanning lines across the docked windows; [`input`] is the outbound side (the chat
//! EditBox, the slash grammar, the send-side emote posture gate) over [`commands`]'s table of every
//! `/command` the client answers (decision 0881, built from the reference's own alias strings).
//! This face wires them into [`UiChatPlugin`] and re-exports the crate-facing API (`ChatLog`,
//! `ChatEvent`, `ChatEventKind`).

use bevy::prelude::*;

use crate::ui_script::UiInput;

#[cfg(test)]
mod ace_gate_tests;
mod channels;
/// The combat log's chat lines (B297) — classification, chat type, and the GlobalString key each
/// combat packet's sentence is built from.
pub(crate) mod combat;
pub(crate) mod commands;
mod edit;
mod event;
mod feed;
mod frames;
mod input;
/// The language gate — the exemptions and the fluency lookup behind the chat garble (B262).
mod language;
#[cfg(test)]
mod tests;

/// The joined-channel roster + the `ChatChannels.dbc` catalog. Read outside this module by the
/// world-state readout ([`crate::world_state_ui`]), whose `Type == 1` gate is "has the player
/// joined a zone-dependent defense channel".
pub(crate) use edit::ChannelState;
/// Test-only: `ui_script::chat_tests` checks every name we fire against the live `ChatTypeInfo`
/// table, which lives on that side of the tree. The app itself calls it through `event::` — the
/// router is the only production caller and it is inside this module.
#[cfg(test)]
pub(crate) use event::event_name;
pub(crate) use event::{default_color, ChatEvent, ChatEventKind};
pub(crate) use feed::ChatLog;

pub(crate) struct UiChatPlugin;

impl Plugin for UiChatPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ChatLog>()
            .init_resource::<frames::ChatWindows>()
            .init_resource::<edit::ChatEditState>()
            .init_resource::<edit::ChannelState>()
            .init_resource::<channels::ZoneChannelWalk>()
            .init_resource::<language::ChatLanguages>()
            // `ChatChannels.dbc` — six rows, read once; the auto-join walk and every chat event's
            // arg7 both come out of it. **`.after(AssetSet::Open)` is load-bearing**: without it
            // this runs before the patch chain exists, takes its `assets: Option<Res<_>>` `None`
            // arm, and silently loads nothing — no zone channels, ever, with no error. A live
            // probe is what caught that; no unit test could, because the tests hand the catalog in.
            // `crate::area`'s `AreaTable.dbc` load carries the same ordering for the same reason.
            // `EmotesText.dbc` × `EmotesTextData.dbc` (decision 1274) rides the same ordering for
            // the same reason: the sentence tables are read once, off the open patch chain.
            .add_systems(
                Startup,
                (channels::load_chat_channels, feed::load_emote_texts)
                    .after(benilla_assets::AssetSet::Open),
            )
            // The slash-command table (decision 0881), built from the reference's own alias strings
            // once the VM's globals and the emote catalog exist — both are `Startup`, so this runs
            // at the next schedule rather than chasing an ordering constraint into two modules.
            .add_systems(PostStartup, commands::build_slash_commands)
            // Push before the input pass so a line is on screen the same frame it decodes (mirrors
            // the loot/merchant feeds).
            // The language gate's two feeds, both upstream of the chat drain that reads them: the
            // word pool loads once (it retries until the chain is up), and the fluency map
            // rebuilds off the spell book + the self descriptors.
            .add_systems(
                Update,
                (
                    language::load_language_words,
                    language::feed_language_skills,
                    language::feed_default_language,
                )
                    .before(feed::feed_chat),
            )
            .add_systems(Update, feed::feed_chat.before(UiInput))
            // A fresh VM gets the joined-channel mirror re-pushed once (decision 1291) — before
            // the feed, so the reload frame's first routed line already renders numbered.
            .add_systems(Update, channels::seed_channels.before(feed::feed_chat))
            // RequestTimePlayed() -> CMSG_PLAYED_TIME, and SMSG_PLAYED_TIME -> TIME_PLAYED_MSG.
            // Beside the chat feed because /played is a chat command and the answer prints there
            // too; before the input pass for the same reason feed_chat is.
            .add_systems(Update, feed::played_time_bridge.before(UiInput))
            // The input: open on ENTER (after the UI input pass has set UiKeyboardCapture, so we
            // don't reopen the box that's already eating keys), then drain any submitted line. Both
            // touch the single NonSend VM, so they chain. In-world only (decision 0193): at the
            // character-select glue screen ENTER must not open a chat box behind the overlay.
            .add_systems(
                Update,
                (
                    edit::open_chat_keys,
                    edit::open_tell_requests,
                    // An addon's `ChatFrame_OpenChat` — before the live parse in the same chain,
                    // so a box opened prefilled `/w Bob ` has its type switched on the very next
                    // frame rather than showing the raw slash to the user first.
                    edit::open_chat_requests,
                    edit::chat_edit_live,
                    edit::chat_tab_cycle,
                    input::drain_chat_input,
                    // An addon's own line into the wire (decision 1199). AFTER the box's drain
                    // and in the same chain, so a `SendChatMessage` fired from a slash handler
                    // that the box's drain just ran goes out on the same frame.
                    input::drain_addon_chat_sends,
                    // …and its addon-lane twin (decision 1235). Same position in the chain, for
                    // the same reason: a `SendAddonMessage` fired from a handler that ran earlier
                    // in this chain goes out on this frame rather than the next.
                    input::drain_addon_message_sends,
                )
                    .chain()
                    .after(UiInput)
                    .run_if(in_state(crate::char_select::ClientState::InWorld)),
            )
            // The zone-channel auto-join (0288 P6): the client half of a handshake vmangos
            // deliberately leaves to us. In-world only, and it early-outs on an unchanged zone.
            //
            // Both session-end edges clear its state (1284): leaving the world at all, and a socket
            // drop that stays in-world for the reconnect. The disconnect twin is chained BEFORE the
            // walk so a drop and a walk landing on the same frame cannot re-diff against membership
            // the drop just invalidated.
            .add_systems(
                Update,
                (
                    channels::end_session_channels_on_disconnect,
                    channels::auto_join_zone_channels
                        .run_if(in_state(crate::char_select::ClientState::InWorld)),
                )
                    .chain(),
            )
            .add_systems(
                OnExit(crate::char_select::ClientState::InWorld),
                (channels::end_session_channels, end_session_chat),
            );
    }
}

/// **The chat module's session end** — what the reference gets for free by destroying its Lua
/// state, and we have to do by hand until that teardown lands (1288).
///
/// `shutdown_ui_state`'s own doc carves the reference's logout tail: `PLAYER_LEAVING_WORLD` →
/// `PLAYER_LOGOUT` → the saved files → **destroy the Lua state**. That last step is the one
/// [`crate::ui_script::IngameUiLoaded`] exists to stand in for; while it does, every window in the
/// VM keeps its contents across a character switch. The director saw it as the previous
/// character's `Joined Channel:` lines still sitting under the new character's — chat scrollback
/// is simply the most visible tenant of a VM that should have been rebuilt.
///
/// So this clears what a fresh VM (and a fresh `ui_chat`) would have: both windows' lines, the
/// edit box's whole cross-open memory — sticky type, the `lastTell` ring, `toldTarget` — and the
/// undrained feed, whose queued lines were addressed to a character that is gone. It is the same
/// shape, and the same reasoning, as [`crate::ui_aura`]'s `end_session_aura_state` (0900).
///
/// **Not on a reconnect**, deliberately, which is why this is not in [`channels`]'s disconnect
/// twin: channel membership is *server* state and dies with the server's `Player` object however
/// the socket ended, but a seamless same-character reconnect (0065) is benilla's own affordance —
/// the reference has no such thing, it goes to the login screen — and inside it the window keeping
/// its scrollback is the whole point.
fn end_session_chat(
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    mut windows: ResMut<frames::ChatWindows>,
    mut edit: ResMut<edit::ChatEditState>,
    mut log: ResMut<ChatLog>,
) {
    end_chat_session(
        script.map(NonSendMut::into_inner),
        &mut windows,
        &mut edit,
        &mut log,
    );
}

/// [`end_session_chat`]'s body, callable without a `World` — the clear is the law, the system is
/// the wiring.
pub(crate) fn end_chat_session(
    script: Option<&mut benilla_ui::script::UiScript>,
    windows: &mut frames::ChatWindows,
    edit: &mut edit::ChatEditState,
    log: &mut ChatLog,
) {
    *log = ChatLog::default();
    windows.tell_alert_left = 0.0;
    // The channel target/number are [`channels::end_session_channels`]'s (1284); everything else
    // the box remembers across an open is this session's too.
    *edit = edit::ChatEditState::default();
    if let Some(script) = script {
        for frame in ["ChatFrame1", "ChatFrame2"] {
            crate::ui_script::run_or_warn(script, &format!("{frame}:Clear()"));
        }
    }
}
