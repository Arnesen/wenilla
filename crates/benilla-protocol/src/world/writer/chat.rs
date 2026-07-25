//! The chat frame's `WorldWriter` sends — every `CMSG_MESSAGECHAT` flavour the slash commands
//! reach (say/yell/emote/whisper/party/raid/guild/officer/the two raid-leader forms/the two
//! battleground forms/AFK/DND/channel), the ignore-list notify, and the three non-`MESSAGECHAT`
//! slash commands whose *answer* is a chat line (`/played`, `/random`, the DBC-indexed `/wave`).
//! Bodies in [`crate::messages`]'s `messagechat*`/`played_time`/`random_roll`/`text_emote`
//! builders; the inbound half is [`crate::messages::chat`]. Split out of `writer/mod.rs`
//! (decision 0636), mirroring [`super::channel`], which owns the channel *administration* verbs
//! (join/leave/moderation) — only the channel *send* lives here.
//!
//! One fact governs the whole family: **every send must speak [`WorldWriter::chat_language`]**, the
//! logged-in character's own tongue. vmangos rejects `Universal` from clients and rejects a tongue
//! the character doesn't know, dropping the whole message — which silently ate every Horde
//! character's dot-commands while this was hardcoded Common.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Send a chat line as `/say`. Used to issue server **dot commands** (`.tele Westfall`, …) —
    /// vmangos parses anything beginning with `.` as a GM command on the way in, but only *after*
    /// the language gate: the send must speak [`Self::chat_language`], the character's own tongue
    /// (**vmangos rejects `Universal` from clients**, and rejects a tongue the character doesn't
    /// know — which silently ate every Horde character's commands while this was hardcoded Common).
    pub fn send_chat(&mut self, message: &str) -> Result<()> {
        self.send(
            opcode::CMSG_MESSAGECHAT,
            &messages::messagechat(messages::CHAT_TYPE_SAY, self.chat_language, message),
        )
    }

    /// Send a `/yell` line (`CHAT_MSG_YELL`) — same body shape as [`Self::send_chat`], a different
    /// wire chat type (VERIFIED vmangos `SharedDefines.h:1199`).
    pub fn send_yell(&mut self, message: &str) -> Result<()> {
        self.send(
            opcode::CMSG_MESSAGECHAT,
            &messages::messagechat(messages::CHAT_TYPE_YELL, self.chat_language, message),
        )
    }

    /// Send a custom `/emote <text>` line (`CHAT_MSG_EMOTE`, VERIFIED vmangos
    /// `SharedDefines.h:1202`) — distinct from [`Self::text_emote`]'s DBC-indexed `/wave`: the
    /// server renders it verbatim as `"PlayerName <text>"`.
    pub fn send_emote_chat(&mut self, message: &str) -> Result<()> {
        self.send(
            opcode::CMSG_MESSAGECHAT,
            &messages::messagechat(messages::CHAT_TYPE_EMOTE, self.chat_language, message),
        )
    }

    /// Send a `/whisper <target> <text>` line (`CHAT_MSG_WHISPER`): body in
    /// [`messages::messagechat_whisper`], the one `CMSG_MESSAGECHAT` shape that carries a name
    /// ahead of the message (VERIFIED vmangos `Server/Packets/Chat.cpp:3-12`). A bad `target`
    /// answers `SMSG_CHAT_PLAYER_NOT_FOUND`, unmodelled here — silent from the client's own POV.
    pub fn send_whisper(&mut self, target: &str, message: &str) -> Result<()> {
        self.send(
            opcode::CMSG_MESSAGECHAT,
            &messages::messagechat_whisper(self.chat_language, target, message),
        )
    }

    /// Send a `/p` party line (`CHAT_MSG_PARTY`) — same body shape as [`Self::send_chat`] (VERIFIED
    /// vmangos `Handlers/ChatHandler.cpp:472-493`: the server rebroadcasts to the group, no group
    /// membership needed on the wire — the server enforces it and silently drops the send if we're
    /// not grouped).
    pub fn send_party(&mut self, message: &str) -> Result<()> {
        self.send(
            opcode::CMSG_MESSAGECHAT,
            &messages::messagechat(messages::CHAT_TYPE_PARTY, self.chat_language, message),
        )
    }

    /// Send a `/ra` raid line (`CHAT_MSG_RAID`) — requires a raid group server-side
    /// (`Handlers/ChatHandler.cpp:514-536`).
    pub fn send_raid(&mut self, message: &str) -> Result<()> {
        self.send(
            opcode::CMSG_MESSAGECHAT,
            &messages::messagechat(messages::CHAT_TYPE_RAID, self.chat_language, message),
        )
    }

    /// Send a `/g` guild line (`CHAT_MSG_GUILD`) — requires guild membership server-side
    /// (`Handlers/ChatHandler.cpp:494-503`).
    pub fn send_guild(&mut self, message: &str) -> Result<()> {
        self.send(
            opcode::CMSG_MESSAGECHAT,
            &messages::messagechat(messages::CHAT_TYPE_GUILD, self.chat_language, message),
        )
    }

    /// Send a `/o` guild-officer line (`CHAT_MSG_OFFICER`) — requires guild membership server-side
    /// (`Handlers/ChatHandler.cpp:504-513`).
    pub fn send_officer(&mut self, message: &str) -> Result<()> {
        self.send(
            opcode::CMSG_MESSAGECHAT,
            &messages::messagechat(messages::CHAT_TYPE_OFFICER, self.chat_language, message),
        )
    }

    /// Send a `/rl` raid-leader line (`CHAT_MSG_RAID_LEADER`) — leader-only server-side
    /// (`Handlers/ChatHandler.cpp:538-559`; VERIFIED active for 5875, `> CLIENT_BUILD_1_10_2`).
    pub fn send_raid_leader(&mut self, message: &str) -> Result<()> {
        self.send(
            opcode::CMSG_MESSAGECHAT,
            &messages::messagechat(messages::CHAT_TYPE_RAID_LEADER, self.chat_language, message),
        )
    }

    /// Send a `/rw` raid-warning line (`CHAT_MSG_RAID_WARNING`) — leader/assistant-only server-side
    /// (`Handlers/ChatHandler.cpp:561-576`).
    pub fn send_raid_warning(&mut self, message: &str) -> Result<()> {
        self.send(
            opcode::CMSG_MESSAGECHAT,
            &messages::messagechat(
                messages::CHAT_TYPE_RAID_WARNING,
                self.chat_language,
                message,
            ),
        )
    }

    /// Send a battleground raid line (`CHAT_MSG_BATTLEGROUND`) — requires a BG group server-side
    /// (`Handlers/ChatHandler.cpp:579-593`; VERIFIED active for 5875, `> CLIENT_BUILD_1_11_2`).
    pub fn send_battleground(&mut self, message: &str) -> Result<()> {
        self.send(
            opcode::CMSG_MESSAGECHAT,
            &messages::messagechat(
                messages::CHAT_TYPE_BATTLEGROUND,
                self.chat_language,
                message,
            ),
        )
    }

    /// Send a battleground-leader line (`CHAT_MSG_BATTLEGROUND_LEADER`) — BG-group-leader-only
    /// server-side (`Handlers/ChatHandler.cpp:595-609`).
    pub fn send_battleground_leader(&mut self, message: &str) -> Result<()> {
        self.send(
            opcode::CMSG_MESSAGECHAT,
            &messages::messagechat(
                messages::CHAT_TYPE_BATTLEGROUND_LEADER,
                self.chat_language,
                message,
            ),
        )
    }

    /// Toggle AFK (`CHAT_MSG_AFK`, `/afk [message]`) — `message` may be empty (a bare toggle); when
    /// non-empty it becomes the AFK auto-reply text (vmangos `Handlers/ChatHandler.cpp:611-630`:
    /// `masterPlr->afkMsg`). Setting AFK clears DND server-side (mutually exclusive).
    pub fn send_afk(&mut self, message: &str) -> Result<()> {
        self.send(
            opcode::CMSG_MESSAGECHAT,
            &messages::messagechat(messages::CHAT_TYPE_AFK, self.chat_language, message),
        )
    }

    /// Toggle DND (`CHAT_MSG_DND`, `/dnd [message]`) — same shape as [`Self::send_afk`]
    /// (`Handlers/ChatHandler.cpp:632-648`); mutually exclusive with AFK server-side.
    pub fn send_dnd(&mut self, message: &str) -> Result<()> {
        self.send(
            opcode::CMSG_MESSAGECHAT,
            &messages::messagechat(messages::CHAT_TYPE_DND, self.chat_language, message),
        )
    }

    /// Send a `/1`-style channel line (`CHAT_MSG_CHANNEL`) — `channel` is the channel **name**
    /// (`Handlers/ChatHandler.cpp:255-327`; body in [`messages::messagechat_channel`]). Requires
    /// membership; the server silently drops it if we're not on the channel.
    pub fn send_channel(&mut self, channel: &str, message: &str) -> Result<()> {
        self.send(
            opcode::CMSG_MESSAGECHAT,
            &messages::messagechat_channel(self.chat_language, channel, message),
        )
    }

    /// Tell the server we've added `guid` to our ignore list (`CMSG_CHAT_IGNORED`, a raw 8-byte
    /// guid — VERIFIED vmangos `WorldPackets::Misc::ChatIgnored::ReadFromWorldPacket`,
    /// `Server/Packets/Misc.cpp:127-130`). The server whispers that player a `CHAT_MSG_IGNORED`
    /// self-notice ("So-and-so is now ignoring you") — `Handlers/ChatHandler.cpp:755-763`.
    pub fn chat_ignored(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_CHAT_IGNORED, &messages::full_guid(guid))
    }

    /// Ask our played time (`CMSG_PLAYED_TIME`, empty body, layout in [`messages::played_time`]) —
    /// the `/played` command. Answered by `SMSG_PLAYED_TIME` (total + since-last-level-up seconds).
    pub fn played_time(&mut self) -> Result<()> {
        self.send(opcode::CMSG_PLAYED_TIME, &messages::played_time())
    }

    /// Roll `/random [min] [max]` (`MSG_RANDOM_ROLL`, layout in [`messages::random_roll`]): the
    /// server validates `min <= max <= 10000` and broadcasts the result (to our group if we're in
    /// one, else just to us) as the same opcode's server→client shape — `min, max, roll, guid`
    /// (decoded by the codec into a `RandomRoll` event).
    pub fn random_roll(&mut self, min: u32, max: u32) -> Result<()> {
        self.send(opcode::MSG_RANDOM_ROLL, &messages::random_roll(min, max))
    }

    /// Perform a chat emote (`CMSG_TEXT_EMOTE`: EmotesText id + target guid, 0 = untargeted).
    /// The server echoes `SMSG_TEXT_EMOTE` to everyone in range **including us**, so our own
    /// emote's sound/anim arrive through the same receive path as everyone else's.
    pub fn text_emote(&mut self, text_id: u32, target: u64) -> Result<()> {
        self.send(
            opcode::CMSG_TEXT_EMOTE,
            &messages::text_emote(text_id, target),
        )
    }
}
