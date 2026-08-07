//! Chat-window arm bodies for [`super::apply_net_updates`]'s dispatch match — the spoken line
//! itself, plus the server notices and query answers that render as chat lines (the channel
//! roster, whisper refusals, `SMSG_NOTIFICATION`, `/played`). Each `pub(super)` fn here is exactly
//! one arm's body; the match at the call site stays the dispatcher, one call per arm.

use benilla_protocol::messages::{ChatMessage, CHAT_MSG_WHISPER};
use bevy::prelude::*;

use crate::ui_chat::{ChatEvent, ChatEventKind, ChatLog};
use crate::ui_social::SocialState;

use super::super::{ClientCommand, NetCommands, ServerSaidMessage};

/// A spoken line (`SMSG_MESSAGECHAT`) — the chat window's own feed (decision 0084):
/// [`crate::ui_chat`] formats + colors per type, resolves the sender name ask-once, and
/// AddMessages it into ChatFrame1.
///
/// System lines (`CHAT_MSG_SYSTEM` 0x0A, vmangos `SharedDefines.h`) are the SERVER'S ANSWER to a GM
/// dot-command — "Premade gear template N applied", "No matching premade player template found",
/// "There is no such command". For a headless probe that is the only channel the server has to say
/// *why* something did not happen, so it rides at `info!`: at `debug!` it was in the log but
/// invisible at the default level, and a refused command read exactly like an applied one
/// (decision 0651 — the rig's whole batch silently no-op'd on a too-low GM level and nothing said
/// so). Ordinary chat stays at `debug!`: conversation, not diagnosis, and high volume.
pub(super) fn chat(
    m: ChatMessage,
    chat_log: &mut ChatLog,
    social: &SocialState,
    net_commands: &NetCommands,
    server_said: &mut MessageWriter<ServerSaidMessage>,
) {
    // The ADDON gate (decision 1029, B215): a line whose `language` is `LANG_ADDON` is not speech
    // at all — it is one addon talking to another over the party/raid/guild/channel lane, because
    // 1.12.1 has no addon opcode and no addon `ChatMsg` type. The real client never renders it; it
    // fires `CHAT_MSG_ADDON` instead. Rendering it printed a partied real-client player's
    // `[Party] [Soreen]: Quiver VERSION:3.1.4` version ping as ordinary party chat, and a
    // mixed-client party makes that constant background noise.
    //
    // VERIFIED in `WoW.exe` (5875) — wow-re `system/ui/scratch/addon-chat-law.md`: the test is
    // `0x49a89b`/`0x49a89e cmp edi,-0x1`, in the DISPLAY function `0x49a870`, and every branch of
    // the addon arm `[0x49a8a7, 0x49a96d)` jumps below the normal render path at `0x49a970` — the
    // chat frame is unreachable for such a line by construction. The **language field is the whole
    // discriminator**: the type byte takes no part in the comparison, and the wire parser
    // `0x49d560` never inspects language at all.
    //
    // **Ignore beats addon**, so that check comes first even though it costs a duplicated call:
    // the reference drops an ignored sender's line at `0x49d72d`, *upstream* of the language test,
    // so an ignored player's addon traffic fires nothing at all. Unobservable while both paths
    // merely drop, but it is the precedence that has to already be right the day there is
    // something to fire. (`CMSG_CHAT_IGNORED` is not owed here — the server refuses `LANG_ADDON`
    // on WHISPER, the only type that answers on the wire.)
    //
    // **One divergence, named:** the reference suppresses *after* name resolution — an uncached
    // sender parks the line in `__AUPENDINGCHAT__` (`0x49d7ba`) behind `CMSG_NAME_QUERY`, and
    // `CHAT_MSG_ADDON` fires late from the callback `0x49ccc0`. We drop here instead, ahead of the
    // name query [`ChatLog::push_wire`] would queue. Invisible today (nothing listens for
    // `CHAT_MSG_ADDON` — benilla runs FrameXML, not third-party addons) and it saves a name query
    // per addon sender; when an addon runtime lands, the *fire* must move downstream of the
    // resolve so its `sender` arg is a name rather than a guid — the drop can stay here.
    //
    // The payload rides `debug!` and its own `addon` trace tag rather than vanishing, so
    // mixed-client traffic stays diagnosable: invisible in chat, never invisible to us.
    if m.is_addon() {
        if !social.is_ignored(m.sender_guid) {
            debug!(
                "net: addon chat [{:#04x}] from {:#x}: {:?} (suppressed — not a chat line)",
                m.chat_type, m.sender_guid, m.text
            );
            if crate::dbg_trace::enabled() {
                crate::dbg_trace::line("addon", &format!("[{:#04x}] {:?}", m.chat_type, m.text));
            }
        }
        return;
    }
    if m.chat_type == 0x0A {
        info!("net: server says — {}", m.text);
        // …and as a message, for the senders that must know whether their command landed. Server
        // state with no descriptor field (god mode) has no other tell.
        server_said.write(ServerSaidMessage {
            text: m.text.clone(),
        });
    } else {
        debug!("net: chat [{:#04x}] {}", m.chat_type, m.text);
    }
    // …and on the trace clock too (decision 0624). A GM dot-command is the only way to ask the
    // SERVER what it believes — `.gps` reads back the server-side position of a mover whose packets
    // may or may not be reaching it — and its answer is only usable if it lands on the same
    // timeline as the `snd`/`rly`/`run` lines it must be read against. `debug!` timestamps are
    // wall-clock in a different format; this is one clock, one file.
    if crate::dbg_trace::enabled() {
        crate::dbg_trace::line(
            "sys",
            &format!("[{:#04x}] {}", m.chat_type, m.text.replace('\n', " ⏎ ")),
        );
    }
    // The ignore gate (decision 0668): an ignored speaker is dropped SILENTLY — no line at all —
    // which is the client's own `FriendList::IsIgnored 0x5ae5a0` check, VERIFIED for the sibling
    // text-emote path (wow-re `system/ui/scratch/text-emote-composition.md`). A dropped WHISPER
    // additionally tells the server, so the sender gets the "is ignoring you" answer: that is what
    // `CMSG_CHAT_IGNORED` is for, and only the client can send it.
    if social.is_ignored(m.sender_guid) {
        if m.chat_type == CHAT_MSG_WHISPER {
            let _ = net_commands.0.send(ClientCommand::ChatIgnored {
                guid: m.sender_guid,
            });
        }
        return;
    }
    chat_log.push_wire(m);
}

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

/// An area trigger's refusal (`SMSG_AREA_TRIGGER_MESSAGE`) — "You must be at least level 58 to
/// enter…", "You cannot enter … while in ghost form." The reference sends it to the **same** sink
/// as [`notification`] (its `0x2b8` arm and the system-message table's kinds 1/2 both end at
/// `0x4945b0`), so it goes wherever that one goes. Without it, a refused portal is silent, which
/// reads exactly like a portal that is still broken.
pub(super) fn area_trigger_message(text: String, chat_log: &mut ChatLog) {
    // Logged for the same reason 0651 logs the server's dot-command answers: a trigger that
    // refused and a trigger the client never noticed look identical from outside.
    info!("net: area-trigger message — {text}");
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

#[cfg(test)]
mod tests {
    use bevy::ecs::system::RunSystemOnce;

    use super::*;
    use benilla_protocol::messages::{CHAT_MSG_PARTY, LANGUAGE_ADDON};

    fn a_party_line(language: u32, text: &str) -> ChatMessage {
        ChatMessage {
            chat_type: CHAT_MSG_PARTY,
            language,
            sender_guid: 0x21,
            target_guid: 0x21,
            sender_name: None,
            channel: None,
            text: text.to_string(),
            chat_tag: 0,
        }
    }

    /// Run one inbound line through the real arm body and report whether it reached the chat log.
    fn reaches_the_chat_window(m: ChatMessage) -> bool {
        let mut world = World::new();
        world.init_resource::<Messages<ServerSaidMessage>>();
        let (tx, _rx) = crossbeam_channel::unbounded();
        world.insert_resource(NetCommands(tx));
        world.insert_resource(SocialState::default());
        world.insert_resource(ChatLog::default());
        world
            .run_system_once(
                move |mut chat_log: ResMut<ChatLog>,
                      social: Res<SocialState>,
                      net_commands: Res<NetCommands>,
                      mut server_said: MessageWriter<ServerSaidMessage>| {
                    chat(
                        m.clone(),
                        &mut chat_log,
                        &social,
                        &net_commands,
                        &mut server_said,
                    );
                },
            )
            .unwrap();
        world.resource::<ChatLog>().pending_len() > 0
    }

    /// B215 / decision 1029: an addon broadcast never reaches the chat window. It arrives as an
    /// ordinary `CHAT_MSG_PARTY` — the lane a mixed-client party's hunter addon talks over — so the
    /// `language` sentinel is the only thing standing between it and a rendered `[Party]` line.
    #[test]
    fn addon_chat_never_reaches_the_chat_window() {
        assert!(
            !reaches_the_chat_window(a_party_line(LANGUAGE_ADDON, "Quiver\tVERSION:3.1.4")),
            "an addon broadcast must be dropped before the chat log"
        );
        // A payload with no tab is still addon traffic — the gate reads the language field, never
        // the shape of the text.
        assert!(!reaches_the_chat_window(a_party_line(
            LANGUAGE_ADDON,
            "nopayloadseparator"
        )));
        // An empty addon payload likewise: nothing about the text can un-addon a line.
        assert!(!reaches_the_chat_window(a_party_line(LANGUAGE_ADDON, "")));
    }

    /// The control the gate must not swallow: real speech on the same lane, in every tongue a
    /// character can speak. A gate that keyed on "unknown language" instead of the sentinel would
    /// silently eat Demonic and Gutterspeak.
    #[test]
    fn speech_on_the_addon_lane_still_renders() {
        for language in [0, 1, 2, 3, 6, 7, 8, 9, 10, 11, 12, 13, 14, 33] {
            assert!(
                reaches_the_chat_window(a_party_line(language, "party control line")),
                "language {language} is a tongue — the line must render"
            );
        }
    }
}
