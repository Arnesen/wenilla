//! The chat system (decision 0288), the reference's own three-stage shape: [`event`] is the
//! internal currency (`CHAT_MSG_*` typed — kinds, groups, the shipped color table); [`feed`] turns
//! every source (wire lines, channel notices, rolls, client-composed loot/system lines) into
//! events with names resolved ask-once; [`frames`] is the router + the `ChatFrame_OnEvent`
//! composer fanning lines across the docked windows; [`input`] is the outbound side (the chat
//! EditBox, the slash grammar, the send-side emote posture gate). This face wires them into
//! [`UiChatPlugin`] and re-exports the crate-facing API (`ChatLog`, `ChatEvent`, `ChatEventKind`).

use bevy::prelude::*;

use crate::ui_script::UiInput;

mod edit;
mod event;
mod feed;
mod frames;
mod input;
#[cfg(test)]
mod tests;

pub(crate) use event::{default_color, ChatEvent, ChatEventKind};
pub(crate) use feed::ChatLog;

pub(crate) struct UiChatPlugin;

impl Plugin for UiChatPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ChatLog>()
            .init_resource::<frames::ChatWindows>()
            .init_resource::<edit::ChatEditState>()
            .init_resource::<edit::ChannelState>()
            // Push before the input pass so a line is on screen the same frame it decodes (mirrors
            // the loot/merchant feeds).
            .add_systems(Update, feed::feed_chat.before(UiInput))
            // The input: open on ENTER (after the UI input pass has set UiKeyboardCapture, so we
            // don't reopen the box that's already eating keys), then drain any submitted line. Both
            // touch the single NonSend VM, so they chain. In-world only (decision 0193): at the
            // character-select glue screen ENTER must not open a chat box behind the overlay.
            .add_systems(
                Update,
                (
                    edit::open_chat_keys,
                    edit::open_tell_requests,
                    edit::chat_edit_live,
                    edit::chat_tab_cycle,
                    input::drain_chat_input,
                )
                    .chain()
                    .after(UiInput)
                    .run_if(in_state(crate::char_select::ClientState::InWorld)),
            );
    }
}
