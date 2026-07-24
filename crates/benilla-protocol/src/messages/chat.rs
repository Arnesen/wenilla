//! Inbound chat — `SMSG_MESSAGECHAT` (VERIFIED vmangos `ChatHandler::BuildChatPacket`,
//! `Chat/Chat.cpp:2542-2599`, all `SUPPORTED_CLIENT_BUILD` bands included for 5875): `u8 type`, `u32
//! language`, a **per-type sender prefix**, then `u32 len` (includes the NUL) + the text + `u8
//! chatTag`. This is how everything textual reaches the client — player say/yell/whisper, NPC
//! lines, channels, and the **system messages** GM dot-commands answer with (type `0x0A`), which
//! is what makes command feedback visible to the probe and the chat UI.
//!
//! Also owns the handful of small chat-adjacent SMSG bodies that aren't their own subsystem:
//! `SMSG_CHAT_PLAYER_NOT_FOUND`, `SMSG_CHAT_WRONG_FACTION`, `SMSG_PLAYED_TIME`, and `MSG_RANDOM_ROLL`'s
//! server→client broadcast shape. The channel subsystem (join/leave/list/moderation +
//! `SMSG_CHANNEL_NOTIFY`/`_LIST`) is big enough to own its own module: [`super::channel`].

use std::io;

use crate::wire::{read_cstring, read_u32_le, read_u64_le, read_u8};

// ChatMsg values (VERIFIED vmangos `SharedDefines.h:1191-1301`, the 5875 band — every
// `#if SUPPORTED_CLIENT_BUILD > CLIENT_BUILD_1_11_2`-or-earlier guard is compile-time true for build
// 5875). vmangos's enum runs the full `0x00..=0x5D`; the combat-log/spell/skill/loot/money block
// (0x09, 0x0F-0x13, 0x17-0x50, 0x52-0x56, 0x5B) is **never** `SMSG_MESSAGECHAT` wire in 1.12 — canned
// emotes ride `SMSG_TEXT_EMOTE`, channel feedback rides `SMSG_CHANNEL_NOTIFY`/`_LIST`, and
// skill/loot/money/xp lines are client-composed from their own opcodes (decision 0288) — so those
// values have no named constant here. What's below is every type benilla either receives (as a
// distinct wire shape or a renderable line) or sends.
pub const CHAT_MSG_SAY: u8 = 0x00;
pub const CHAT_MSG_PARTY: u8 = 0x01;
pub const CHAT_MSG_RAID: u8 = 0x02;
pub const CHAT_MSG_GUILD: u8 = 0x03;
pub const CHAT_MSG_OFFICER: u8 = 0x04;
pub const CHAT_MSG_YELL: u8 = 0x05;
pub const CHAT_MSG_WHISPER: u8 = 0x06;
/// Server→client only ("You whisper to Name: …" self-echo, vmangos `MasterPlayerChat.cpp:54`) —
/// never a `CMSG_MESSAGECHAT` type a client sends.
pub const CHAT_MSG_WHISPER_INFORM: u8 = 0x07;
pub const CHAT_MSG_EMOTE: u8 = 0x08;
/// GM dot-command feedback and other server-authored lines; always the single-sender-guid shape
/// (the guid is usually 0).
pub const CHAT_MSG_SYSTEM: u8 = 0x0A;
pub const CHAT_MSG_MONSTER_SAY: u8 = 0x0B;
pub const CHAT_MSG_MONSTER_YELL: u8 = 0x0C;
pub const CHAT_MSG_MONSTER_EMOTE: u8 = 0x0D;
pub const CHAT_MSG_CHANNEL: u8 = 0x0E;
pub const CHAT_MSG_AFK: u8 = 0x14;
pub const CHAT_MSG_DND: u8 = 0x15;
/// The "so-and-so is ignoring you" self-notice (`WorldSession::HandleChatIgnoredOpcode`,
/// `Handlers/ChatHandler.cpp:755-763`) — single-sender-guid shape, sender is the ignoring player.
pub const CHAT_MSG_IGNORED: u8 = 0x16;
pub const CHAT_MSG_MONSTER_WHISPER: u8 = 0x1A;
/// `#if SUPPORTED_CLIENT_BUILD > CLIENT_BUILD_1_10_2` — active for 5875.
pub const CHAT_MSG_RAID_LEADER: u8 = 0x57;
pub const CHAT_MSG_RAID_WARNING: u8 = 0x58;
/// `#if SUPPORTED_CLIENT_BUILD > CLIENT_BUILD_1_11_2` — active for 5875 (pre-1.11.3 this value
/// aliased `CHAT_MSG_MONSTER_WHISPER` instead; irrelevant here since 5875 always takes this branch).
pub const CHAT_MSG_RAID_BOSS_WHISPER: u8 = 0x59;
pub const CHAT_MSG_RAID_BOSS_EMOTE: u8 = 0x5A;
pub const CHAT_MSG_BATTLEGROUND: u8 = 0x5C;
pub const CHAT_MSG_BATTLEGROUND_LEADER: u8 = 0x5D;

/// `PlayerChatTag` (VERIFIED vmangos `Chat/Chat.h:86-92`) — [`ChatMessage::chat_tag`]'s values.
/// `Player::GetChatTag` (`Objects/Player.cpp:1757-1767`) picks GM over DND over AFK over none.
pub mod chat_tag {
    pub const NONE: u8 = 0;
    pub const AFK: u8 = 1;
    pub const DND: u8 = 2;
    pub const GM: u8 = 3;
}

/// One decoded chat line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    /// The `ChatMsg` type byte (0x00 say, 0x0A system, 0x0B monster say, …).
    pub chat_type: u8,
    /// The `Language` id (0 universal, 7 common, …).
    pub language: u32,
    /// The speaker, when the type carries a guid (0 for system messages / name-only shapes).
    pub sender_guid: u64,
    /// The speaker's name, for the monster shapes that carry it inline (`None` elsewhere —
    /// player names resolve via the name query, exactly like the real client).
    pub sender_name: Option<String>,
    /// The channel name, on `CHAT_MSG_CHANNEL` only.
    pub channel: Option<String>,
    pub text: String,
    /// The trailing tag byte ([`chat_tag`]) — 0 none, 1 AFK, 2 DND, 3 GM. Drives the
    /// `<AFK>`/`<DND>`/`<GM>` prefix the real chat frame prepends to the sender's name.
    pub chat_tag: u8,
}

/// Read the length-prefixed string shape (`u32 len` including NUL + bytes).
fn read_len_string(r: &mut &[u8]) -> io::Result<String> {
    let len = read_u32_le(r)? as usize;
    if len == 0 {
        return Ok(String::new());
    }
    // The NUL is inside `len`; read through it via the cstring reader and sanity-cap first.
    if len > r.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("chat text length {len} exceeds remaining {}", r.len()),
        ));
    }
    read_cstring(r)
}

/// Read an `SMSG_MESSAGECHAT` body.
pub(super) fn read_message_chat(r: &mut &[u8]) -> io::Result<ChatMessage> {
    let chat_type = read_u8(r)?;
    let language = read_u32_le(r)?;
    let mut sender_guid = 0u64;
    let mut sender_name = None;
    let mut channel = None;
    match chat_type {
        CHAT_MSG_MONSTER_WHISPER
        | CHAT_MSG_RAID_BOSS_WHISPER
        | CHAT_MSG_RAID_BOSS_EMOTE
        | CHAT_MSG_MONSTER_EMOTE => {
            sender_name = Some(read_len_string(r)?);
            let _target = read_u64_le(r)?;
        }
        CHAT_MSG_SAY | CHAT_MSG_PARTY | CHAT_MSG_YELL => {
            sender_guid = read_u64_le(r)?;
            let _sender_again = read_u64_le(r)?;
        }
        CHAT_MSG_MONSTER_SAY | CHAT_MSG_MONSTER_YELL => {
            sender_guid = read_u64_le(r)?;
            sender_name = Some(read_len_string(r)?);
            let _target = read_u64_le(r)?;
        }
        CHAT_MSG_CHANNEL => {
            channel = Some(read_cstring(r)?);
            let _player_rank = read_u32_le(r)?;
            sender_guid = read_u64_le(r)?;
        }
        // Everything else (RAID/GUILD/OFFICER/WHISPER/WHISPER_INFORM/EMOTE/SYSTEM/AFK/DND/IGNORED/
        // RAID_LEADER/RAID_WARNING/BATTLEGROUND(+LEADER)/BG_SYSTEM_*/…): one sender guid
        // (vmangos `BuildChatPacket`'s own `default:` branch).
        _ => {
            sender_guid = read_u64_le(r)?;
        }
    }
    let text = read_len_string(r)?;
    let chat_tag = read_u8(r)?;
    Ok(ChatMessage {
        chat_type,
        language,
        sender_guid,
        sender_name,
        channel,
        text,
        chat_tag,
    })
}

/// Read `SMSG_CHAT_PLAYER_NOT_FOUND` (VERIFIED vmangos `WorldPackets::Chat::ChatPlayerNotFound::
/// AppendBodyTo`, `Server/Packets/Chat.cpp:26-29`): one cstring, the (un-normalized) name the
/// player tried to whisper. Sent by `WorldSession::SendPlayerNotFoundNotice`
/// (`Handlers/ChatHandler.cpp:766-771`) — a bad whisper target, offline or misspelled.
pub(super) fn read_chat_player_not_found(r: &mut &[u8]) -> io::Result<String> {
    read_cstring(r)
}

/// Read `SMSG_NOTIFICATION` (VERIFIED vmangos `WorldSession::SendNotification`,
/// `Server/WorldSession.cpp:900-915`: `data << szStr`): one cstring — the notice text, already
/// formatted server-side ("You do not know that language", trade refusals, and kin). The real
/// client flashes it in the red UIErrorsFrame.
pub(super) fn read_notification(r: &mut &[u8]) -> io::Result<String> {
    read_cstring(r)
}

/// Read `SMSG_PLAYED_TIME` (VERIFIED vmangos `WorldPackets::Misc::PlayedTime::AppendBodyTo`,
/// `Server/Packets/Misc.cpp:278-282`; handler `WorldSession::HandlePlayedTime`,
/// `Handlers/MiscHandler.cpp:935-941`): `u32 totalPlayedTime` (seconds since character creation) +
/// `u32 levelPlayedTime` (seconds since the last level-up) — answers `CMSG_PLAYED_TIME` (empty
/// body, the `/played` command).
pub(super) fn read_played_time(r: &mut &[u8]) -> io::Result<(u32, u32)> {
    let total = read_u32_le(r)?;
    let level = read_u32_le(r)?;
    Ok((total, level))
}

/// Read `MSG_RANDOM_ROLL`'s server→client **broadcast** shape (VERIFIED vmangos
/// `WorldSession::HandleRandomRollOpcode`, `Handlers/GroupHandler.cpp:394-422`): `u32 minimum, u32
/// maximum, u32 roll` (the server's own `urand(minimum, maximum)`), then a **raw** `u64` guid — the
/// roller (`data << GetPlayer()->GetObjectGuid()`, an `ObjectGuid` stream write, so unpacked).
/// Same opcode as the client→server request shape (`u32 minimum, u32 maximum`,
/// [`super::client::random_roll`]) — a client only ever *receives* this broadcast shape, never its
/// own request echoed back.
pub(super) fn read_random_roll(r: &mut &[u8]) -> io::Result<(u32, u32, u32, u64)> {
    let min = read_u32_le(r)?;
    let max = read_u32_le(r)?;
    let roll = read_u32_le(r)?;
    let guid = read_u64_le(r)?;
    Ok((min, max, roll, guid))
}
