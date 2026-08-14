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
pub(crate) mod commands;
mod edit;
mod event;
mod feed;
mod frames;
mod input;
#[cfg(test)]
mod tests;

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
            // `ChatChannels.dbc` — six rows, read once; the auto-join walk and every chat event's
            // arg7 both come out of it. **`.after(AssetSet::Open)` is load-bearing**: without it
            // this runs before the patch chain exists, takes its `assets: Option<Res<_>>` `None`
            // arm, and silently loads nothing — no zone channels, ever, with no error. A live
            // probe is what caught that; no unit test could, because the tests hand the catalog in.
            // `crate::area`'s `AreaTable.dbc` load carries the same ordering for the same reason.
            .add_systems(
                Startup,
                channels::load_chat_channels.after(benilla_assets::AssetSet::Open),
            )
            // The slash-command table (decision 0881), built from the reference's own alias strings
            // once the VM's globals and the emote catalog exist — both are `Startup`, so this runs
            // at the next schedule rather than chasing an ordering constraint into two modules.
            .add_systems(PostStartup, commands::build_slash_commands)
            // Push before the input pass so a line is on screen the same frame it decodes (mirrors
            // the loot/merchant feeds).
            .add_systems(Update, feed::feed_chat.before(UiInput))
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
            .add_systems(
                Update,
                channels::auto_join_zone_channels
                    .run_if(in_state(crate::char_select::ClientState::InWorld)),
            );
    }
}
