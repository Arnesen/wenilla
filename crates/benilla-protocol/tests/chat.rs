//! Chat + channel wire decode tests (mirrors `src/messages/chat.rs` + `src/messages/channel.rs`,
//! decision 0288 phase 1): one golden per `SMSG_MESSAGECHAT` per-type wire shape (the reader was
//! byte-correct before this decision but had zero golden coverage), `SMSG_TEXT_EMOTE`/`SMSG_EMOTE`,
//! every `SMSG_CHANNEL_NOTIFY` tail shape, `SMSG_CHANNEL_LIST`, `SMSG_CHAT_PLAYER_NOT_FOUND`,
//! `SMSG_CHAT_WRONG_FACTION`, `SMSG_NOTIFICATION`, `SMSG_PLAYED_TIME`, and `MSG_RANDOM_ROLL`'s
//! broadcast shape. Bytes
//! hand-computed from the vmangos layout (citations inline), independent of the Rust decoder — see
//! `tests/common` for the shared `hx` fixture helper.

mod common;

use benilla_protocol::events::{decode, SessionEvent};
use benilla_protocol::messages::{self, ChannelNoticeTail};
use benilla_protocol::ServerPacket;
use common::hx;

/// `SMSG_MESSAGECHAT`'s five distinct wire shapes (VERIFIED vmangos `ChatHandler::BuildChatPacket`,
/// `Chat/Chat.cpp:2542-2599`): SAY/PARTY/YELL (sender guid **twice**), MONSTER_SAY/MONSTER_YELL
/// (guid + length-prefixed name + target guid), CHANNEL (cstring channel + `u32` rank + guid),
/// MONSTER_WHISPER/RAID_BOSS_WHISPER/RAID_BOSS_EMOTE/MONSTER_EMOTE (length-prefixed name + target
/// guid, no leading sender guid), and the `default:` shape (one sender guid) — SYSTEM, WHISPER,
/// EMOTE, RAID, GUILD, OFFICER, AFK, DND, IGNORED, RAID_LEADER/WARNING, BATTLEGROUND(+LEADER) all
/// fall here. Every shape ends `u32 len (incl NUL) + text + NUL`, then the trailing `u8 chatTag`
/// (`Chat/Chat.h:86-92`) this decision surfaces on [`messages::ChatMessage::chat_tag`].
#[test]
fn message_chat_decodes_every_wire_shape() {
    // SAY: guid twice, chat_tag = AFK (1) — proves the tag rides through on the "two guid" shape.
    let body = hx("0007000000887766554433221188776655443322110900000068692074686572650001");
    match messages::parse_server(messages::opcode::SMSG_MESSAGECHAT, &body).unwrap() {
        ServerPacket::MessageChat(m) => {
            assert_eq!(m.chat_type, messages::CHAT_MSG_SAY);
            assert_eq!(m.language, 7);
            assert_eq!(m.sender_guid, 0x1122_3344_5566_7788);
            assert_eq!(m.sender_name, None);
            assert_eq!(m.channel, None);
            assert_eq!(m.text, "hi there");
            assert_eq!(m.chat_tag, messages::chat_tag::AFK);
        }
        _ => panic!("expected MessageChat"),
    }

    // MONSTER_SAY: guid + length-prefixed name + target guid, chat_tag = none. Body: type(0b) +
    // language(00000000) + sender guid(ddccbbaa…) + namelen(0f000000) + "Timmy the Wolf\0" +
    // target guid(66778899…) + msglen(05000000) + "Grr!\0" + chatTag(00).
    let body = hx("0b00000000ddccbbaa000000000f00000054696d6d792074686520576f6c6600667788990000000005000000477272210000");
    match messages::parse_server(messages::opcode::SMSG_MESSAGECHAT, &body).unwrap() {
        ServerPacket::MessageChat(m) => {
            assert_eq!(m.chat_type, messages::CHAT_MSG_MONSTER_SAY);
            assert_eq!(m.sender_guid, 0xAABB_CCDD);
            // The addressee — the guid slot between the name and the message. It is the subject
            // the real client's `$`-macro expander resolves this line against (decision 0754), so
            // it has to survive the parse instead of being read and dropped.
            assert_eq!(m.target_guid, 0x9988_7766);
            assert_eq!(m.sender_name.as_deref(), Some("Timmy the Wolf"));
            assert_eq!(m.channel, None);
            assert_eq!(m.text, "Grr!");
            assert_eq!(m.chat_tag, messages::chat_tag::NONE);
        }
        _ => panic!("expected MessageChat"),
    }

    // CHANNEL: cstring channel + u32 rank + guid. Body: type(0e) + language(00000000) +
    // "General\0" + playerRank(00000000) + sender guid(3412…) + msglen(10000000) +
    // "wtb boar livers\0" + chatTag(00).
    let body = hx("0e0000000047656e6572616c000000000034120000000000001000000077746220626f6172206c69766572730000");
    match messages::parse_server(messages::opcode::SMSG_MESSAGECHAT, &body).unwrap() {
        ServerPacket::MessageChat(m) => {
            assert_eq!(m.chat_type, messages::CHAT_MSG_CHANNEL);
            assert_eq!(m.channel.as_deref(), Some("General"));
            assert_eq!(m.sender_guid, 0x1234);
            assert_eq!(m.sender_name, None);
            assert_eq!(m.text, "wtb boar livers");
        }
        _ => panic!("expected MessageChat"),
    }

    // MONSTER_WHISPER: length-prefixed name + target guid (no leading sender guid at all). Body:
    // type(1a) + language(00000000) + namelen(0e000000) + "Innkeeper Bob\0" + target guid(55…) +
    // msglen(19000000) + "Welcome, weary traveler!\0".
    let body = hx("1a000000000e000000496e6e6b656570657220426f620055000000000000001900000057656c636f6d652c2077656172792074726176656c6572210000");
    match messages::parse_server(messages::opcode::SMSG_MESSAGECHAT, &body).unwrap() {
        ServerPacket::MessageChat(m) => {
            assert_eq!(m.chat_type, messages::CHAT_MSG_MONSTER_WHISPER);
            assert_eq!(m.sender_guid, 0, "no leading guid on this shape");
            assert_eq!(m.target_guid, 0x55, "the whispered-at player");
            assert_eq!(m.sender_name.as_deref(), Some("Innkeeper Bob"));
            assert_eq!(m.text, "Welcome, weary traveler!");
        }
        _ => panic!("expected MessageChat"),
    }

    // default shape (SYSTEM): one sender guid (0), chat_tag = GM (3). Body: type(0a) +
    // language(00000000) + sender guid(0000000000000000) + msglen(1a000000) +
    // "This is a system message.\0" + chatTag(03).
    let body = hx(
        "0a0000000000000000000000001a0000005468697320697320612073797374656d206d6573736167652e0003",
    );
    match messages::parse_server(messages::opcode::SMSG_MESSAGECHAT, &body).unwrap() {
        ServerPacket::MessageChat(m) => {
            assert_eq!(m.chat_type, messages::CHAT_MSG_SYSTEM);
            assert_eq!(m.sender_guid, 0);
            assert_eq!(m.text, "This is a system message.");
            assert_eq!(m.chat_tag, messages::chat_tag::GM);
        }
        _ => panic!("expected MessageChat"),
    }
}

/// `SMSG_TEXT_EMOTE` (VERIFIED vmangos `MaNGOS::EmoteChatBuilder::operator()`,
/// `Handlers/ChatHandler.cpp:681-702`): `u64` performer guid (an `ObjectGuid` write, unpacked) + `u32`
/// textEmote + `u32` emoteNum + `u32 namelen` + `namelen` bytes (the target's name + NUL, or a lone
/// NUL when untargeted) — and `SMSG_EMOTE` (VERIFIED vmangos `EmoteNotify::AppendBodyTo`,
/// `Server/Packets/Misc.cpp:670-674`): `u32` emoteId + `u64` guid.
#[test]
fn text_emote_and_emote_decode() {
    let body = hx("7700000000000000650000000000000004000000426f6200");
    let p = messages::parse_server(messages::opcode::SMSG_TEXT_EMOTE, &body).unwrap();
    assert!(matches!(
        p,
        ServerPacket::TextEmote {
            guid: 0x77,
            text_emote: 101,
        }
    ));
    assert!(matches!(
        decode(p)[..],
        [SessionEvent::TextEmote {
            guid: 0x77,
            text_emote: 101,
        }]
    ));

    let body = hx("060000008800000000000000");
    let p = messages::parse_server(messages::opcode::SMSG_EMOTE, &body).unwrap();
    assert!(matches!(
        p,
        ServerPacket::Emote {
            guid: 0x88,
            emote_id: 6,
        }
    ));
    assert!(matches!(
        decode(p)[..],
        [SessionEvent::Emote {
            guid: 0x88,
            emote_id: 6,
        }]
    ));
}

/// Every `SMSG_CHANNEL_NOTIFY` tail shape (VERIFIED vmangos `Channel::Make*`,
/// `Chat/Channel.cpp:804-1008`), one representative per [`ChannelNoticeTail`] variant, plus the
/// unrecognized-notice error path.
#[test]
fn channel_notify_decodes_each_tail_shape() {
    // 0x00 JOINED: guid.
    let body = hx("0047656e6572616c00aa00000000000000");
    match messages::parse_server(messages::opcode::SMSG_CHANNEL_NOTIFY, &body).unwrap() {
        ServerPacket::ChannelNotify(n) => {
            assert_eq!(n.notice, messages::channel_notice::JOINED);
            assert_eq!(n.channel, "General");
            assert_eq!(n.tail, ChannelNoticeTail::Guid(0xAA));
        }
        _ => panic!("expected ChannelNotify"),
    }

    // 0x02 YOU_JOINED: u32 flags + u32 0 (reserved).
    let body = hx("0247656e6572616c001800000000000000");
    match messages::parse_server(messages::opcode::SMSG_CHANNEL_NOTIFY, &body).unwrap() {
        ServerPacket::ChannelNotify(n) => {
            assert_eq!(n.notice, messages::channel_notice::YOU_JOINED);
            assert_eq!(n.tail, ChannelNoticeTail::YouJoined { flags: 0x18 });
        }
        _ => panic!("expected ChannelNotify"),
    }

    // 0x05 NOT_MEMBER: empty tail — both the instance (`MakeNotMember`) and static
    // (`MakeNotOnPacket`) vmangos producers write this exact shape; no special-casing needed.
    let body = hx("0553656372657400");
    match messages::parse_server(messages::opcode::SMSG_CHANNEL_NOTIFY, &body).unwrap() {
        ServerPacket::ChannelNotify(n) => {
            assert_eq!(n.notice, messages::channel_notice::NOT_MEMBER);
            assert_eq!(n.channel, "Secret");
            assert_eq!(n.tail, ChannelNoticeTail::Empty);
        }
        _ => panic!("expected ChannelNotify"),
    }

    // 0x18 INVITE: a lone actor guid.
    let body = hx("1847656e6572616c00bb00000000000000");
    match messages::parse_server(messages::opcode::SMSG_CHANNEL_NOTIFY, &body).unwrap() {
        ServerPacket::ChannelNotify(n) => {
            assert_eq!(n.notice, messages::channel_notice::INVITE);
            assert_eq!(n.tail, ChannelNoticeTail::Actor(0xBB));
        }
        _ => panic!("expected ChannelNotify"),
    }

    // 0x09 PLAYER_NOT_FOUND: a bare name.
    let body = hx("0947656e6572616c0047686f737400");
    match messages::parse_server(messages::opcode::SMSG_CHANNEL_NOTIFY, &body).unwrap() {
        ServerPacket::ChannelNotify(n) => {
            assert_eq!(n.notice, messages::channel_notice::PLAYER_NOT_FOUND);
            assert_eq!(n.tail, ChannelNoticeTail::Name("Ghost".into()));
        }
        _ => panic!("expected ChannelNotify"),
    }

    // 0x0B CHANNEL_OWNER: the "Nobody" literal vmangos sends for an ownerless non-constant channel.
    let body = hx("0b47656e6572616c004e6f626f647900");
    match messages::parse_server(messages::opcode::SMSG_CHANNEL_NOTIFY, &body).unwrap() {
        ServerPacket::ChannelNotify(n) => {
            assert_eq!(n.notice, messages::channel_notice::CHANNEL_OWNER);
            assert_eq!(n.tail, ChannelNoticeTail::Name("Nobody".into()));
        }
        _ => panic!("expected ChannelNotify"),
    }

    // 0x0C MODE_CHANGE: guid + old flags + new flags.
    let body = hx("0c47656e6572616c00cc000000000000000002");
    match messages::parse_server(messages::opcode::SMSG_CHANNEL_NOTIFY, &body).unwrap() {
        ServerPacket::ChannelNotify(n) => {
            assert_eq!(n.notice, messages::channel_notice::MODE_CHANGE);
            assert_eq!(
                n.tail,
                ChannelNoticeTail::ModeChange {
                    guid: 0xCC,
                    old_flags: 0x00,
                    new_flags: 0x02,
                }
            );
        }
        _ => panic!("expected ChannelNotify"),
    }

    // 0x12 PLAYER_KICKED: target guid + source (actor) guid.
    let body = hx("1247656e6572616c00dd00000000000000ee00000000000000");
    match messages::parse_server(messages::opcode::SMSG_CHANNEL_NOTIFY, &body).unwrap() {
        ServerPacket::ChannelNotify(n) => {
            assert_eq!(n.notice, messages::channel_notice::PLAYER_KICKED);
            assert_eq!(
                n.tail,
                ChannelNoticeTail::Actors {
                    target: 0xDD,
                    source: 0xEE,
                }
            );
        }
        _ => panic!("expected ChannelNotify"),
    }

    // Round-trip the decoded ChannelNotify through the event layer once (the mapping is a flat
    // passthrough, but it must actually be wired).
    let body = hx("0047656e6572616c00aa00000000000000");
    let p = messages::parse_server(messages::opcode::SMSG_CHANNEL_NOTIFY, &body).unwrap();
    match &decode(p)[..] {
        [SessionEvent::ChannelNotify {
            notice,
            channel,
            tail,
        }] => {
            assert_eq!(*notice, messages::channel_notice::JOINED);
            assert_eq!(channel, "General");
            assert_eq!(*tail, ChannelNoticeTail::Guid(0xAA));
        }
        other => panic!("channel notify decode: {} events", other.len()),
    }

    // An unrecognized notice byte (outside vmangos's defined 0x00..=0x1F) errors rather than
    // guessing a tail shape.
    let body = hx("2047656e6572616c00");
    assert!(messages::parse_server(messages::opcode::SMSG_CHANNEL_NOTIFY, &body).is_err());
}

/// `SMSG_CHANNEL_LIST` (VERIFIED vmangos `Channel::List`, `Chat/Channel.cpp:513-556`): cstring
/// channel + `u8` flags + `u32` count + that many `(u64 guid, u8 memberFlags)` rows.
#[test]
fn channel_list_decodes_roster() {
    let body = hx("47656e6572616c001802000000011000000000000001021000000000000000");
    match messages::parse_server(messages::opcode::SMSG_CHANNEL_LIST, &body).unwrap() {
        ServerPacket::ChannelList {
            channel,
            flags,
            members,
        } => {
            assert_eq!(channel, "General");
            assert_eq!(flags, 0x18);
            assert_eq!(members, vec![(0x1001, 0x01), (0x1002, 0x00)]);
        }
        _ => panic!("expected ChannelList"),
    }
}

/// `SMSG_CHAT_PLAYER_NOT_FOUND` (cstring name, VERIFIED vmangos `Server/Packets/Chat.cpp:26-29`) and
/// `SMSG_CHAT_WRONG_FACTION` (empty body, `Server/Packets/Chat.cpp:16-18`).
#[test]
fn chat_player_not_found_and_wrong_faction_decode() {
    let body = hx("47686f73746e616d6500");
    match messages::parse_server(messages::opcode::SMSG_CHAT_PLAYER_NOT_FOUND, &body).unwrap() {
        ServerPacket::ChatPlayerNotFound { name } => assert_eq!(name, "Ghostname"),
        _ => panic!("expected ChatPlayerNotFound"),
    }
    assert!(matches!(
        decode(
            messages::parse_server(messages::opcode::SMSG_CHAT_PLAYER_NOT_FOUND, &body).unwrap()
        )[..],
        [SessionEvent::ChatPlayerNotFound { .. }]
    ));

    let p = messages::parse_server(messages::opcode::SMSG_CHAT_WRONG_FACTION, &[]).unwrap();
    assert!(matches!(p, ServerPacket::ChatWrongFaction));
    assert!(matches!(decode(p)[..], [SessionEvent::ChatWrongFaction]));
}

/// `SMSG_NOTIFICATION` (VERIFIED vmangos `WorldSession::SendNotification`,
/// `Server/WorldSession.cpp:900-915`: `data << szStr`): one cstring, pre-formatted server-side —
/// the "You do not know that language" reply is how a wrong-tongue send fails (silently, until
/// this packet was rendered).
#[test]
fn notification_decodes() {
    let body = hx("596f7520646f206e6f74206b6e6f772074686174206c616e677561676500");
    match messages::parse_server(messages::opcode::SMSG_NOTIFICATION, &body).unwrap() {
        ServerPacket::Notification { ref text } => {
            assert_eq!(text, "You do not know that language")
        }
        other => panic!("expected Notification, got {}", other.name()),
    }
    match &decode(messages::parse_server(messages::opcode::SMSG_NOTIFICATION, &body).unwrap())[..] {
        [SessionEvent::Notification { text }] => assert_eq!(text, "You do not know that language"),
        other => panic!("notification decode: {} events", other.len()),
    }
}

/// `SMSG_PLAYED_TIME` (VERIFIED vmangos `WorldPackets::Misc::PlayedTime::AppendBodyTo`,
/// `Server/Packets/Misc.cpp:278-282`): `u32` total + `u32` level, both seconds.
#[test]
fn played_time_decodes() {
    let body = hx("40e20100100e0000"); // total = 123456, level = 3600
    let p = messages::parse_server(messages::opcode::SMSG_PLAYED_TIME, &body).unwrap();
    assert!(matches!(
        p,
        ServerPacket::PlayedTime {
            total: 123_456,
            level: 3600,
        }
    ));
    assert!(matches!(
        decode(p)[..],
        [SessionEvent::PlayedTime {
            total: 123_456,
            level: 3600,
        }]
    ));
}

/// `MSG_RANDOM_ROLL`'s server→client broadcast shape (VERIFIED vmangos
/// `WorldSession::HandleRandomRollOpcode`, `Handlers/GroupHandler.cpp:394-422`): `u32` min + `u32`
/// max + `u32` roll + a **raw** `u64` guid.
#[test]
fn random_roll_broadcast_decodes() {
    let body = hx("01000000640000002a0000009999000000000000");
    let p = messages::parse_server(messages::opcode::MSG_RANDOM_ROLL, &body).unwrap();
    assert!(matches!(
        p,
        ServerPacket::RandomRoll {
            min: 1,
            max: 100,
            roll: 42,
            guid: 0x9999,
        }
    ));
    assert!(matches!(
        decode(p)[..],
        [SessionEvent::RandomRoll {
            min: 1,
            max: 100,
            roll: 42,
            guid: 0x9999,
        }]
    ));
}

/// The `$`-macro gate is a byte-level fact, not a design choice — pin it.
///
/// VERIFIED in `WoW.exe` (build 5875) at `0x49cf36-0x49cf5d`: the `cmp`-chain guarding the
/// `call 0x506f70` at `0x49cf70` tests exactly `0x0b 0x0c 0x0d 0x1a 0x52 0x53 0x54`. Two things
/// this guards against, both of which read as plausible and are wrong: adding the RAID_BOSS pair
/// (0x59/0x5A) because they look like boss text, and dropping the BG_SYSTEM trio (0x52-0x54)
/// because they look like plain system lines. Player chat is never expanded.
#[test]
fn the_macro_expanded_chat_types_are_the_reference_seven() {
    assert_eq!(
        messages::MACRO_EXPANDED_TYPES,
        [0x0B, 0x0C, 0x0D, 0x1A, 0x52, 0x53, 0x54]
    );
    for t in [
        messages::CHAT_MSG_SAY,
        messages::CHAT_MSG_YELL,
        messages::CHAT_MSG_WHISPER,
        messages::CHAT_MSG_GUILD,
        messages::CHAT_MSG_CHANNEL,
        messages::CHAT_MSG_SYSTEM,
        messages::CHAT_MSG_RAID_BOSS_WHISPER,
        messages::CHAT_MSG_RAID_BOSS_EMOTE,
    ] {
        assert!(
            !messages::MACRO_EXPANDED_TYPES.contains(&t),
            "type {t:#04x} must reach the frame verbatim"
        );
    }
}
