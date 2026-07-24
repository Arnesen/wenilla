//! The group/party arm helpers (decision 0434): the composed-system-line push. The composition
//! policy itself lives in [`crate::ui_party::GroupState`] — these are the drain-side shims.

use crate::ui_chat::{ChatEvent, ChatEventKind, ChatLog};

/// Push the lines a `GroupState::apply_*` composed as CHAT_MSG_SYSTEM — the same seam the other
/// client-composed feeds use (the reference's engine formats these via its errorId→GlobalStrings
/// display and fires them as system chat; benilla's composer hands us the finished strings).
pub(super) fn push_group_lines(chat_log: &mut ChatLog, lines: Vec<String>) {
    for line in lines {
        chat_log.push_event(ChatEvent::text_only(ChatEventKind::System, line));
    }
}
