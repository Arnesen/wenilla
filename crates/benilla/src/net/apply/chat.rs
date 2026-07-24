//! Chat-window system-line arm bodies for [`super::apply_net_updates`]'s dispatch match — the
//! server notices and query answers that render as chat lines (the channel roster, whisper
//! refusals, `SMSG_NOTIFICATION`, `/played`). Each `pub(super)` fn here is exactly one arm's
//! body; the match at the call site stays the dispatcher, one call per arm.

use crate::ui_chat::{ChatEvent, ChatEventKind, ChatLog};

/// The `/chatlist` roster (`SMSG_CHANNEL_LIST`) — CHAT_CHANNEL_LIST_GET "[%s] " + the roster.
/// Names arrive as guids; v1 renders the count (the per-member resolve fan-out lands with the
/// P6 channel wiring — /chatlist is rare enough that a count is honest, never wrong).
pub(super) fn channel_list(channel: String, members: &[(u64, u8)], chat_log: &mut ChatLog) {
    let mut ev = ChatEvent::text_only(
        ChatEventKind::ChannelList,
        format!("{} member(s)", members.len()),
    );
    ev.channel = channel;
    chat_log.push_event(ev);
}

/// A whisper target wasn't online — ERR_CHAT_PLAYER_NOT_FOUND_S (GlobalStrings:1534).
pub(super) fn chat_player_not_found(name: &str, chat_log: &mut ChatLog) {
    chat_log.push_event(ChatEvent::text_only(
        ChatEventKind::System,
        format!("No player named '{name}' is currently playing."),
    ));
}

/// A cross-faction whisper was refused — ERR_CHAT_WRONG_FACTION (GlobalStrings:1537).
pub(super) fn chat_wrong_faction(chat_log: &mut ChatLog) {
    chat_log.push_event(ChatEvent::text_only(
        ChatEventKind::System,
        "You can only whisper to members of your alliance.".to_string(),
    ));
}

/// A server notice (`SMSG_NOTIFICATION`). The ref flashes it in the red UIErrorsFrame
/// (center-screen); benilla has no error frame yet, so the chat feed carries it — an honest
/// divergence, chosen over silence (a dropped notice hid the language-gate bug for weeks:
/// a rejected send looked like nothing at all).
pub(super) fn notification(text: String, chat_log: &mut ChatLog) {
    chat_log.push_event(ChatEvent::text_only(ChatEventKind::System, text));
}

/// The `/played` answer (`SMSG_PLAYED_TIME`) — TIME_PLAYED_TOTAL/LEVEL over
/// TIME_DAYHOURMINUTESECOND (GlobalStrings:4243-4247; the ref's
/// ChatFrame_DisplayTimePlayed breakdown).
pub(super) fn played_time(total: u32, level: u32, chat_log: &mut ChatLog) {
    for (label, secs) in [
        ("Total time played", total),
        ("Time played this level", level),
    ] {
        let (d, rem) = (secs / 86_400, secs % 86_400);
        let (h, rem) = (rem / 3_600, rem % 3_600);
        let (m, sec) = (rem / 60, rem % 60);
        chat_log.push_event(ChatEvent::text_only(
            ChatEventKind::System,
            format!("{label}: {d} days, {h} hours, {m} minutes, {sec} seconds"),
        ));
    }
}
