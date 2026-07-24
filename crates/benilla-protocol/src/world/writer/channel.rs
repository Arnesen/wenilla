//! The channel family's `WorldWriter` sends — join/leave/list/moderation (bodies in
//! [`crate::messages`]'s `join_channel`/`leave_channel`/`channel_*` builders, all cstring-only
//! shapes; layout VERIFIED against vmangos `Server/Packets/Channel.h`/`.cpp`). Split out of
//! `writer/mod.rs` (decision 0288 phase 1) — the channel administration commands are one clearly
//! separable concern among the writer's many domains.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Join a channel (`CMSG_JOIN_CHANNEL`, layout in [`messages::join_channel`]): `password` is an
    /// empty string for a channel that has none. Zone channels (General/Trade/LocalDefense) are
    /// joined by the CLIENT too — in 1.12 the client walks ChatChannels.dbc on login/zone-in and
    /// sends this same packet with the composed name ("General - Elwynn Forest"); the server only
    /// tracks membership by name (decision 0288 phase 6 drives that walk). Answered by
    /// `SMSG_CHANNEL_NOTIFY`'s `YOU_JOINED`/`WRONG_PASSWORD`/`BANNED`/… notice.
    pub fn join_channel(&mut self, name: &str, password: &str) -> Result<()> {
        self.send(
            opcode::CMSG_JOIN_CHANNEL,
            &messages::join_channel(name, password),
        )
    }

    /// Leave a channel (`CMSG_LEAVE_CHANNEL`, layout in [`messages::leave_channel`]).
    pub fn leave_channel(&mut self, name: &str) -> Result<()> {
        self.send(opcode::CMSG_LEAVE_CHANNEL, &messages::leave_channel(name))
    }

    /// Ask a channel's member roster (`CMSG_CHANNEL_LIST`, layout in [`messages::channel_list`]),
    /// the `/chatlist` / `/list` command. Answered by `SMSG_CHANNEL_LIST`.
    pub fn channel_list(&mut self, name: &str) -> Result<()> {
        self.send(opcode::CMSG_CHANNEL_LIST, &messages::channel_list(name))
    }

    /// Set a channel's password (`CMSG_CHANNEL_PASSWORD`, layout in
    /// [`messages::channel_password`]) — owner-only server-side.
    pub fn channel_password(&mut self, name: &str, password: &str) -> Result<()> {
        self.send(
            opcode::CMSG_CHANNEL_PASSWORD,
            &messages::channel_password(name, password),
        )
    }

    /// Transfer channel ownership (`CMSG_CHANNEL_SET_OWNER`, layout in
    /// [`messages::channel_set_owner`]) — owner-only server-side.
    pub fn channel_set_owner(&mut self, name: &str, player: &str) -> Result<()> {
        self.send(
            opcode::CMSG_CHANNEL_SET_OWNER,
            &messages::channel_set_owner(name, player),
        )
    }

    /// Ask who owns a channel (`CMSG_CHANNEL_OWNER`, layout in [`messages::channel_owner`]) — the
    /// `/console` "who owns" query. Answered by `SMSG_CHANNEL_NOTIFY`'s `CHANNEL_OWNER` notice.
    pub fn channel_owner(&mut self, name: &str) -> Result<()> {
        self.send(opcode::CMSG_CHANNEL_OWNER, &messages::channel_owner(name))
    }

    /// Grant moderator (`CMSG_CHANNEL_MODERATOR`, layout in [`messages::channel_moderator`]) —
    /// owner-only server-side.
    pub fn channel_moderator(&mut self, name: &str, player: &str) -> Result<()> {
        self.send(
            opcode::CMSG_CHANNEL_MODERATOR,
            &messages::channel_moderator(name, player),
        )
    }

    /// Revoke moderator (`CMSG_CHANNEL_UNMODERATOR`, layout in [`messages::channel_unmoderator`]).
    pub fn channel_unmoderator(&mut self, name: &str, player: &str) -> Result<()> {
        self.send(
            opcode::CMSG_CHANNEL_UNMODERATOR,
            &messages::channel_unmoderator(name, player),
        )
    }

    /// Mute a player on the channel (`CMSG_CHANNEL_MUTE`, layout in [`messages::channel_mute`]) —
    /// moderator-only server-side.
    pub fn channel_mute(&mut self, name: &str, player: &str) -> Result<()> {
        self.send(
            opcode::CMSG_CHANNEL_MUTE,
            &messages::channel_mute(name, player),
        )
    }

    /// Unmute a player (`CMSG_CHANNEL_UNMUTE`, layout in [`messages::channel_unmute`]).
    pub fn channel_unmute(&mut self, name: &str, player: &str) -> Result<()> {
        self.send(
            opcode::CMSG_CHANNEL_UNMUTE,
            &messages::channel_unmute(name, player),
        )
    }

    /// Invite a player to a moderated/private channel (`CMSG_CHANNEL_INVITE`, layout in
    /// [`messages::channel_invite`]); level-gated server-side
    /// (`CONFIG_UINT32_CHANNEL_INVITE_MIN_LEVEL`).
    pub fn channel_invite(&mut self, name: &str, player: &str) -> Result<()> {
        self.send(
            opcode::CMSG_CHANNEL_INVITE,
            &messages::channel_invite(name, player),
        )
    }

    /// Kick a player off the channel (`CMSG_CHANNEL_KICK`, layout in [`messages::channel_kick`]) —
    /// moderator-only server-side.
    pub fn channel_kick(&mut self, name: &str, player: &str) -> Result<()> {
        self.send(
            opcode::CMSG_CHANNEL_KICK,
            &messages::channel_kick(name, player),
        )
    }

    /// Ban a player from the channel (`CMSG_CHANNEL_BAN`, layout in [`messages::channel_ban`]) —
    /// moderator-only server-side.
    pub fn channel_ban(&mut self, name: &str, player: &str) -> Result<()> {
        self.send(
            opcode::CMSG_CHANNEL_BAN,
            &messages::channel_ban(name, player),
        )
    }

    /// Unban a player (`CMSG_CHANNEL_UNBAN`, layout in [`messages::channel_unban`]).
    pub fn channel_unban(&mut self, name: &str, player: &str) -> Result<()> {
        self.send(
            opcode::CMSG_CHANNEL_UNBAN,
            &messages::channel_unban(name, player),
        )
    }

    /// Toggle join/leave announcements (`CMSG_CHANNEL_ANNOUNCEMENTS`, layout in
    /// [`messages::channel_announcements`]) — owner/moderator-only server-side.
    pub fn channel_announcements(&mut self, name: &str) -> Result<()> {
        self.send(
            opcode::CMSG_CHANNEL_ANNOUNCEMENTS,
            &messages::channel_announcements(name),
        )
    }

    /// Toggle moderation (speak-restricted-to-moderators) mode (`CMSG_CHANNEL_MODERATE`, layout in
    /// [`messages::channel_moderate`]) — owner-only server-side.
    pub fn channel_moderate(&mut self, name: &str) -> Result<()> {
        self.send(
            opcode::CMSG_CHANNEL_MODERATE,
            &messages::channel_moderate(name),
        )
    }
}
