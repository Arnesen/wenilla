//! The channel subsystem — join/leave/list/moderation (opcodes 151-168, VERIFIED vmangos
//! `Server/Protocol/Opcodes_1_12_1.h:154-171`) plus the two server notification packets:
//! `SMSG_CHANNEL_NOTIFY` (a notice byte + channel name + a per-notice tail — [`read_channel_notify`],
//! VERIFIED vmangos `Chat/Channel.cpp:804-1008` against the enum in `Chat/Channel.h:35-71`) and
//! `SMSG_CHANNEL_LIST` (the member roster — [`read_channel_list`], VERIFIED
//! `Chat/Channel.cpp:497-557`). CMSG bodies are VERIFIED against `Server/Packets/Channel.h`/`.cpp`
//! (every `ReadFromWorldPacket` there): every moderation command reads `cstring channelName [+
//! cstring playerName]` — the client body builders in [`super::client`] all funnel through
//! [`super::client::cstrings_body`] for that shared shape.

use std::io;

use crate::wire::{read_cstring, read_u32_le, read_u64_le, read_u8};

/// `SMSG_CHANNEL_NOTIFY`'s notice byte (`ChatNotify`, VERIFIED vmangos `Chat/Channel.h:35-71`) —
/// selects which tail follows the channel name (see [`ChannelNoticeTail`]/[`read_channel_notify`]).
pub mod channel_notice {
    pub const JOINED: u8 = 0x00;
    pub const LEFT: u8 = 0x01;
    pub const YOU_JOINED: u8 = 0x02;
    pub const YOU_LEFT: u8 = 0x03;
    pub const WRONG_PASSWORD: u8 = 0x04;
    pub const NOT_MEMBER: u8 = 0x05;
    pub const NOT_MODERATOR: u8 = 0x06;
    pub const PASSWORD_CHANGED: u8 = 0x07;
    pub const OWNER_CHANGED: u8 = 0x08;
    pub const PLAYER_NOT_FOUND: u8 = 0x09;
    pub const NOT_OWNER: u8 = 0x0A;
    pub const CHANNEL_OWNER: u8 = 0x0B;
    pub const MODE_CHANGE: u8 = 0x0C;
    pub const ANNOUNCEMENTS_ON: u8 = 0x0D;
    pub const ANNOUNCEMENTS_OFF: u8 = 0x0E;
    pub const MODERATION_ON: u8 = 0x0F;
    pub const MODERATION_OFF: u8 = 0x10;
    pub const MUTED: u8 = 0x11;
    pub const PLAYER_KICKED: u8 = 0x12;
    pub const BANNED: u8 = 0x13;
    pub const PLAYER_BANNED: u8 = 0x14;
    pub const PLAYER_UNBANNED: u8 = 0x15;
    pub const PLAYER_NOT_BANNED: u8 = 0x16;
    pub const PLAYER_ALREADY_MEMBER: u8 = 0x17;
    pub const INVITE: u8 = 0x18;
    pub const INVITE_WRONG_FACTION: u8 = 0x19;
    pub const WRONG_FACTION: u8 = 0x1A;
    pub const INVALID_NAME: u8 = 0x1B;
    pub const NOT_MODERATED: u8 = 0x1C;
    pub const PLAYER_INVITED: u8 = 0x1D;
    pub const PLAYER_INVITE_BANNED: u8 = 0x1E;
    pub const THROTTLED: u8 = 0x1F;
}

/// The per-notice tail `SMSG_CHANNEL_NOTIFY` carries after `u8 notice + cstring channel` — which
/// shape follows depends on `notice` ([`channel_notice`]); VERIFIED against vmangos's one
/// `Channel::Make*` producer per notice (`Chat/Channel.cpp:804-1008`). `NOT_MEMBER` (0x05) has a
/// second, **static** producer (`Channel::MakeNotOnPacket`, `Channel.cpp:847-851`, used when there's
/// no live `Channel` object to query — e.g. `/list` on a channel name that doesn't exist) that writes
/// the byte-**identical** shape (`u8 notice, cstring channel`, nothing more): both call sites decode
/// the same way, an empty tail — there is no ambiguity to special-case here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelNoticeTail {
    /// 0x00 JOINED / 0x01 LEFT: the guid that joined/left.
    Guid(u64),
    /// 0x02 YOU_JOINED: our own new channel flags. The trailing `u32` vmangos writes after it is a
    /// hardcoded `0` ("the non-zero number will be appended to the channel name" — never actually
    /// sent nonzero) and carries no information, so it isn't surfaced.
    YouJoined { flags: u32 },
    /// 0x03 YOU_LEFT / 0x04 WRONG_PASSWORD / 0x05 NOT_MEMBER / 0x06 NOT_MODERATOR / 0x0A NOT_OWNER /
    /// 0x11 MUTED / 0x13 BANNED / 0x19 INVITE_WRONG_FACTION / 0x1A WRONG_FACTION / 0x1B INVALID_NAME /
    /// 0x1C NOT_MODERATED / 0x1F THROTTLED: no tail at all — the notice + channel name is the whole
    /// packet.
    Empty,
    /// 0x07 PASSWORD_CHANGED / 0x08 OWNER_CHANGED / 0x0D ANNOUNCEMENTS_ON / 0x0E ANNOUNCEMENTS_OFF /
    /// 0x0F MODERATION_ON / 0x10 MODERATION_OFF / 0x17 PLAYER_ALREADY_MEMBER / 0x18 INVITE: the guid
    /// that made the change / is doing the inviting.
    Actor(u64),
    /// 0x09 PLAYER_NOT_FOUND / 0x0B CHANNEL_OWNER / 0x16 PLAYER_NOT_BANNED / 0x1D PLAYER_INVITED /
    /// 0x1E PLAYER_INVITE_BANNED: a bare player name (CHANNEL_OWNER sends the literal string
    /// `"Nobody"` for a non-constant channel with no owner, or `"PLAYER_NOT_FOUND"` if the owner's
    /// name can't be resolved — both ride this same string field).
    Name(String),
    /// 0x0C MODE_CHANGE: the affected guid + its `ChannelMemberFlags` byte before/after the change.
    ModeChange {
        guid: u64,
        old_flags: u8,
        new_flags: u8,
    },
    /// 0x12 PLAYER_KICKED / 0x14 PLAYER_BANNED / 0x15 PLAYER_UNBANNED: the affected player, then who
    /// did it.
    Actors { target: u64, source: u64 },
}

/// One decoded `SMSG_CHANNEL_NOTIFY`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelNotify {
    pub notice: u8,
    pub channel: String,
    pub tail: ChannelNoticeTail,
}

/// Read `SMSG_CHANNEL_NOTIFY` (see [`ChannelNoticeTail`] for the per-notice VERIFIED citations).
/// An unrecognized notice byte (outside vmangos's defined `0x00..=0x1F`) errors rather than guessing
/// a shape — there is no way to skip an unknown tail without risking misreading the rest of this
/// packet's bytes as something else.
pub(super) fn read_channel_notify(r: &mut &[u8]) -> io::Result<ChannelNotify> {
    use channel_notice as n;

    let notice = read_u8(r)?;
    let channel = read_cstring(r)?;
    let tail = match notice {
        n::JOINED | n::LEFT => ChannelNoticeTail::Guid(read_u64_le(r)?),
        n::YOU_JOINED => {
            let flags = read_u32_le(r)?;
            let _reserved = read_u32_le(r)?; // always 0 on the wire (see the variant doc)
            ChannelNoticeTail::YouJoined { flags }
        }
        n::YOU_LEFT
        | n::WRONG_PASSWORD
        | n::NOT_MEMBER
        | n::NOT_MODERATOR
        | n::NOT_OWNER
        | n::MUTED
        | n::BANNED
        | n::INVITE_WRONG_FACTION
        | n::WRONG_FACTION
        | n::INVALID_NAME
        | n::NOT_MODERATED
        | n::THROTTLED => ChannelNoticeTail::Empty,
        n::PASSWORD_CHANGED
        | n::OWNER_CHANGED
        | n::ANNOUNCEMENTS_ON
        | n::ANNOUNCEMENTS_OFF
        | n::MODERATION_ON
        | n::MODERATION_OFF
        | n::PLAYER_ALREADY_MEMBER
        | n::INVITE => ChannelNoticeTail::Actor(read_u64_le(r)?),
        n::PLAYER_NOT_FOUND
        | n::CHANNEL_OWNER
        | n::PLAYER_NOT_BANNED
        | n::PLAYER_INVITED
        | n::PLAYER_INVITE_BANNED => ChannelNoticeTail::Name(read_cstring(r)?),
        n::MODE_CHANGE => ChannelNoticeTail::ModeChange {
            guid: read_u64_le(r)?,
            old_flags: read_u8(r)?,
            new_flags: read_u8(r)?,
        },
        n::PLAYER_KICKED | n::PLAYER_BANNED | n::PLAYER_UNBANNED => ChannelNoticeTail::Actors {
            target: read_u64_le(r)?,
            source: read_u64_le(r)?,
        },
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("SMSG_CHANNEL_NOTIFY: unknown notice type {other:#04x}"),
            ))
        }
    };
    Ok(ChannelNotify {
        notice,
        channel,
        tail,
    })
}

/// Read `SMSG_CHANNEL_LIST` (`Channel::List`, VERIFIED vmangos `Chat/Channel.cpp:513-556`): cstring
/// channel name + `u8` channel flags + `u32` member count + that many `(u64 guid, u8 memberFlags)`
/// rows (`ChannelMemberFlags` — owner/moderator/voiced/muted/custom/mic-muted, `Chat/Channel.h:
/// 119-130`). Returns `(channel, flags, members)`.
#[allow(clippy::type_complexity)]
pub(super) fn read_channel_list(r: &mut &[u8]) -> io::Result<(String, u8, Vec<(u64, u8)>)> {
    let channel = read_cstring(r)?;
    let flags = read_u8(r)?;
    let count = read_u32_le(r)?;
    // The count is wire-controlled — never let it drive allocation (a corrupt 0xFFFFFFFF must fail
    // at the bounds-checked reads below, not abort in the allocator). Legit rosters exceed 256
    // rarely; push simply grows past the hint.
    let mut members = Vec::with_capacity((count as usize).min(256));
    for _ in 0..count {
        let guid = read_u64_le(r)?;
        let member_flags = read_u8(r)?;
        members.push((guid, member_flags));
    }
    Ok((channel, flags, members))
}
