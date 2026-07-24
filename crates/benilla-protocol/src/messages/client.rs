//! Client packet **body builders** — the outbound half of the message layer (the inbound parse +
//! the `ServerPacket` vocabulary stay in [`super`]; spell/attack builders live with their family
//! in [`super::spells`]). Framing/encryption is `world.rs`'s job; these produce plain body bytes.

use super::{CharCreateReq, MovementInfo};

/// `CMSG_AUTH_SESSION` body: build, server id, account name, client seed, proof, and an empty
/// zlib-wrapped addon block (`decompressed_size = 0` + zlib of no bytes — what the client sends and
/// vmangos accepts).
pub fn auth_session(
    build: u32,
    username: &str,
    client_seed: u32,
    client_proof: &[u8; 20],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(40 + username.len());
    body.extend_from_slice(&build.to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes()); // server_id
    body.extend_from_slice(username.as_bytes());
    body.push(0);
    body.extend_from_slice(&client_seed.to_le_bytes());
    body.extend_from_slice(client_proof);
    body.extend_from_slice(&0u32.to_le_bytes()); // addon decompressed_size
    let encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    let empty_addon_zlib = encoder.finish().expect("zlib finish on empty input");
    body.extend_from_slice(&empty_addon_zlib);
    body
}

/// `CMSG_CHAR_CREATE` body: name (CString), then race/class/gender + the 5 appearance bytes
/// (skin/face/hair_style/hair_color/facial_hair) + `outfit_id`. Matches vmangos's read exactly
/// (`Packets/Character.cpp:4-19`): name + 9 bytes. `outfit_id` is always 0 — the server reads and
/// ignores it (`CharacterHandler.cpp:310`) and recomputes start gear (decision 0423).
pub fn char_create(req: &CharCreateReq) -> Vec<u8> {
    let mut body = Vec::with_capacity(req.name.len() + 10);
    body.extend_from_slice(req.name.as_bytes());
    body.push(0);
    // race, class, gender, skin, face, hair_style, hair_color, facial_hair, outfit_id(0).
    body.extend_from_slice(&[
        req.race,
        req.class,
        req.gender,
        req.skin,
        req.face,
        req.hair_style,
        req.hair_color,
        req.facial_hair,
        0,
    ]);
    body
}

/// `CMSG_MESSAGECHAT` body for every sendable `ChatMsg` type (VERIFIED vmangos
/// `WorldPackets::Chat::ChatMessage::ReadFromWorldPacket`, `Server/Packets/Chat.cpp:3-12`):
/// `recv_data >> type >> lang`, then `>> whisperTargetOrChannel` **only** for
/// `CHAT_MSG_WHISPER`/`CHAT_MSG_CHANNEL` (the target name or channel name rides *before* the
/// message, not after — vmangos's own union of the two), then `>> message`. `target` is `None` for
/// every other sendable type. The general form [`messagechat`]/[`messagechat_whisper`]/
/// [`messagechat_channel`] build on.
pub fn messagechat_kind(
    chat_type: u32,
    language: u32,
    target: Option<&str>,
    message: &str,
) -> Vec<u8> {
    let mut body = Vec::with_capacity(message.len() + target.map_or(0, str::len) + 10);
    body.extend_from_slice(&chat_type.to_le_bytes());
    body.extend_from_slice(&language.to_le_bytes());
    if let Some(target) = target {
        body.extend_from_slice(target.as_bytes());
        body.push(0);
    }
    body.extend_from_slice(message.as_bytes());
    body.push(0);
    body
}

/// `CMSG_MESSAGECHAT` body (chat type + language as `u32`, then the message) — the shape shared by
/// every type that carries no target/channel name (SAY, YELL, EMOTE, PARTY, RAID, GUILD, OFFICER,
/// RAID_LEADER, RAID_WARNING, BATTLEGROUND(+LEADER), AFK, DND). Thin wrapper over
/// [`messagechat_kind`] with `target: None`.
pub fn messagechat(chat_type: u32, language: u32, message: &str) -> Vec<u8> {
    messagechat_kind(chat_type, language, None, message)
}

/// `CMSG_MESSAGECHAT` body for `CHAT_MSG_WHISPER` (`0x6`): chat type + language, then the whisper
/// **target's player name**, then the message. Thin wrapper over [`messagechat_kind`].
pub fn messagechat_whisper(language: u32, target: &str, message: &str) -> Vec<u8> {
    messagechat_kind(super::CHAT_TYPE_WHISPER, language, Some(target), message)
}

/// `CMSG_MESSAGECHAT` body for `CHAT_MSG_CHANNEL` (`0xE`): chat type + language, then the
/// **channel name**, then the message — the same wire shape as [`messagechat_whisper`], vmangos's
/// `whisperTargetOrChannel` field doing double duty. Thin wrapper over [`messagechat_kind`].
pub fn messagechat_channel(language: u32, channel: &str, message: &str) -> Vec<u8> {
    messagechat_kind(super::CHAT_TYPE_CHANNEL, language, Some(channel), message)
}

/// Concatenate `strings` as back-to-back NUL-terminated C-strings — the shape shared by the whole
/// `CMSG_CHANNEL_*` family (VERIFIED vmangos `Server/Packets/Channel.cpp`, every `ReadFromWorldPacket`
/// there: a channel name alone, or a channel name + a second field — a password or a target player
/// name).
fn cstrings_body(strings: &[&str]) -> Vec<u8> {
    let mut body = Vec::with_capacity(strings.iter().map(|s| s.len() + 1).sum());
    for s in strings {
        body.extend_from_slice(s.as_bytes());
        body.push(0);
    }
    body
}

/// `CMSG_JOIN_CHANNEL` body (VERIFIED vmangos `WorldPackets::Channel::JoinChannel::
/// ReadFromWorldPacket`, `Server/Packets/Channel.cpp:3-7`): cstring channel name + cstring password
/// (empty string for none) — **no channel id** on the 1.12 wire (a TBC+ addition).
pub fn join_channel(name: &str, password: &str) -> Vec<u8> {
    cstrings_body(&[name, password])
}

/// `CMSG_LEAVE_CHANNEL` body: cstring channel name (VERIFIED vmangos
/// `WorldPackets::Channel::LeaveChannel::ReadFromWorldPacket`, `Channel.cpp:9-12`).
pub fn leave_channel(name: &str) -> Vec<u8> {
    cstrings_body(&[name])
}

/// `CMSG_CHANNEL_LIST` body — asks for the member roster (VERIFIED vmangos
/// `WorldPackets::Channel::ChannelList::ReadFromWorldPacket`, `Channel.cpp:14-17`): cstring channel
/// name. Answered by `SMSG_CHANNEL_LIST` ([`super::channel::read_channel_list`]).
pub fn channel_list(name: &str) -> Vec<u8> {
    cstrings_body(&[name])
}

/// `CMSG_CHANNEL_PASSWORD` body (VERIFIED vmangos `ChannelPassword::ReadFromWorldPacket`,
/// `Channel.cpp:19-23`): cstring channel name + cstring new password.
pub fn channel_password(name: &str, password: &str) -> Vec<u8> {
    cstrings_body(&[name, password])
}

/// `CMSG_CHANNEL_SET_OWNER` body (VERIFIED vmangos `ChannelSetOwner::ReadFromWorldPacket`,
/// `Channel.cpp:25-29`): cstring channel name + cstring new owner's player name.
pub fn channel_set_owner(name: &str, player: &str) -> Vec<u8> {
    cstrings_body(&[name, player])
}

/// `CMSG_CHANNEL_OWNER` body — asks who owns the channel (VERIFIED vmangos
/// `ChannelOwner::ReadFromWorldPacket`, `Channel.cpp:31-34`): cstring channel name only (the answer
/// rides `SMSG_CHANNEL_NOTIFY`'s [`super::channel::channel_notice::CHANNEL_OWNER`] notice).
pub fn channel_owner(name: &str) -> Vec<u8> {
    cstrings_body(&[name])
}

/// `CMSG_CHANNEL_MODERATOR` body (VERIFIED vmangos `ChannelModerator::ReadFromWorldPacket`,
/// `Channel.cpp:36-40`): cstring channel name + cstring target player name.
pub fn channel_moderator(name: &str, player: &str) -> Vec<u8> {
    cstrings_body(&[name, player])
}

/// `CMSG_CHANNEL_UNMODERATOR` body (VERIFIED vmangos `ChannelUnmoderator::ReadFromWorldPacket`,
/// `Channel.cpp:42-46`): cstring channel name + cstring target player name.
pub fn channel_unmoderator(name: &str, player: &str) -> Vec<u8> {
    cstrings_body(&[name, player])
}

/// `CMSG_CHANNEL_MUTE` body (VERIFIED vmangos `ChannelMute::ReadFromWorldPacket`,
/// `Channel.cpp:48-52`): cstring channel name + cstring target player name.
pub fn channel_mute(name: &str, player: &str) -> Vec<u8> {
    cstrings_body(&[name, player])
}

/// `CMSG_CHANNEL_UNMUTE` body (VERIFIED vmangos `ChannelUnmute::ReadFromWorldPacket`,
/// `Channel.cpp:54-58`): cstring channel name + cstring target player name.
pub fn channel_unmute(name: &str, player: &str) -> Vec<u8> {
    cstrings_body(&[name, player])
}

/// `CMSG_CHANNEL_INVITE` body (VERIFIED vmangos `ChannelInvite::ReadFromWorldPacket`,
/// `Channel.cpp:60-64`): cstring channel name + cstring invitee player name.
pub fn channel_invite(name: &str, player: &str) -> Vec<u8> {
    cstrings_body(&[name, player])
}

/// `CMSG_CHANNEL_KICK` body (VERIFIED vmangos `ChannelKick::ReadFromWorldPacket`,
/// `Channel.cpp:66-70`): cstring channel name + cstring target player name.
pub fn channel_kick(name: &str, player: &str) -> Vec<u8> {
    cstrings_body(&[name, player])
}

/// `CMSG_CHANNEL_BAN` body (VERIFIED vmangos `ChannelBan::ReadFromWorldPacket`,
/// `Channel.cpp:72-76`): cstring channel name + cstring target player name.
pub fn channel_ban(name: &str, player: &str) -> Vec<u8> {
    cstrings_body(&[name, player])
}

/// `CMSG_CHANNEL_UNBAN` body (VERIFIED vmangos `ChannelUnban::ReadFromWorldPacket`,
/// `Channel.cpp:78-82`): cstring channel name + cstring target player name.
pub fn channel_unban(name: &str, player: &str) -> Vec<u8> {
    cstrings_body(&[name, player])
}

/// `CMSG_CHANNEL_ANNOUNCEMENTS` body — toggles the channel's join/leave announcements (VERIFIED
/// vmangos `ChannelAnnouncements::ReadFromWorldPacket`, `Channel.cpp:84-87`): cstring channel name
/// only.
pub fn channel_announcements(name: &str) -> Vec<u8> {
    cstrings_body(&[name])
}

/// `CMSG_CHANNEL_MODERATE` body — toggles channel moderation (VERIFIED vmangos
/// `ChannelModerate::ReadFromWorldPacket`, `Channel.cpp:89-92`): cstring channel name only.
pub fn channel_moderate(name: &str) -> Vec<u8> {
    cstrings_body(&[name])
}

/// `CMSG_PLAYED_TIME` body — empty (VERIFIED vmangos: the handler takes a `NullClientPacket`,
/// `Handlers/MiscHandler.cpp:935`). The `/played` request.
pub fn played_time() -> Vec<u8> {
    Vec::new()
}

/// `MSG_RANDOM_ROLL` client→server request body (VERIFIED vmangos
/// `WorldPackets::Group::RandomRoll::ReadFromWorldPacket`, `Server/Packets/Group.cpp:39-43`): `u32
/// minimum + u32 maximum`. The server validates `minimum <= maximum <= 10000` and broadcasts the
/// roll (layout in [`super::chat::read_random_roll`]); an out-of-range request is silently dropped.
pub fn random_roll(min: u32, max: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(8);
    body.extend_from_slice(&min.to_le_bytes());
    body.extend_from_slice(&max.to_le_bytes());
    body
}

/// `CMSG_TEXT_EMOTE` body: `u32 textEmote (EmotesText.dbc id), u32 emoteNum, u64 target guid`
/// (0 = no target) — VERIFIED vmangos `TextEmote::ReadFromWorldPacket` (`Misc.cpp:60-65`), three
/// fields. `emoteNum` is only relayed to observers inside `SMSG_TEXT_EMOTE` (the emote-text
/// variation index; we send 0 = the first variation). The 12-byte two-field body this originally
/// sent made the server's guid read run off the packet's end — the emote was silently discarded,
/// on every send, which read as "/wave does nothing" (director-caught against the ref client).
/// The server echoes `SMSG_TEXT_EMOTE` to everyone in range **including us**, so our own emote
/// sound/anim plays through the same receive path as everyone else's.
pub fn text_emote(text_id: u32, target: u64) -> Vec<u8> {
    let mut b = Vec::with_capacity(16);
    b.extend_from_slice(&text_id.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes()); // emoteNum: text-variation 0
    b.extend_from_slice(&target.to_le_bytes());
    b
}

pub fn full_guid(guid: u64) -> Vec<u8> {
    guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_STANDSTATECHANGE` (vmangos `StandStateChange::ReadFromWorldPacket`:
/// `recv_data >> animState`): one `u32` stand state. The server accepts only
/// {0 STAND, 1 SIT, 3 SLEEP, 8 KNEEL} (`HandleStandStateChangeOpcode`,
/// `MiscHandler.cpp:437`) and applies it via `Unit::SetStandState` → `UNIT_FIELD_BYTES_1`
/// byte 0, which relays to every observer — the same client-volunteers → server-echoes →
/// fields-drive-everyone pattern as sheath (decision 0080c).
pub fn stand_state_change(state: u32) -> Vec<u8> {
    state.to_le_bytes().to_vec()
}

/// Body of a `CMSG_CREATURE_QUERY`: the template `entry` + the asking guid (VERIFIED vmangos
/// `QueryCreature::ReadFromWorldPacket` — `entry` then a full 8-byte guid).
pub fn creature_query(entry: u32, guid: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&entry.to_le_bytes());
    body.extend_from_slice(&guid.to_le_bytes());
    body
}

/// Body of a movement message (`MSG_MOVE_*`) — a `MovementInfo`.
pub fn movement(info: &MovementInfo) -> Vec<u8> {
    let mut body = Vec::with_capacity(28);
    info.write(&mut body);
    body
}

/// Body of `CMSG_MOVE_SPLINE_DONE` (client): the `MovementInfo` at the ride's endpoint, the
/// `spline_id` being acknowledged (matched against the server's newest spline for our mover), and a
/// trailing float the server reads-and-discards (`MoveSplineDone::ReadFromWorldPacket` does an
/// unconditional `read_skip<float>()`, so the four bytes MUST be present or the server's parse
/// under-runs). The real client writes its **completion fraction** `clamp(elapsed/duration, 0, 1)`
/// there (byte-verified — `0x600b10`, decision 0496 §claim-6); we only ever send at ride
/// completion, so the faithful value is `1.0`. Emitted once when a server spline that drove our own
/// player (Charge/knockback/taxi) finishes; the server then relocates us and re-broadcasts a
/// stop/heartbeat to observers.
pub fn move_spline_done(info: &MovementInfo, spline_id: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(36);
    info.write(&mut body);
    body.extend_from_slice(&spline_id.to_le_bytes());
    body.extend_from_slice(&1.0f32.to_le_bytes()); // completion fraction (server-skipped)
    body
}

/// Body of `MSG_MOVE_TELEPORT_ACK` (client): a **full** 8-byte GUID (not packed — vmangos requires
/// it for this opcode), the movement counter, and the client time.
pub fn teleport_ack(guid: u64, counter: u32, time_ms: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(&guid.to_le_bytes());
    body.extend_from_slice(&counter.to_le_bytes());
    body.extend_from_slice(&time_ms.to_le_bytes());
    body
}

/// Body of a `CMSG_FORCE_*_SPEED_CHANGE_ACK` (all six kinds share it): a **full** 8-byte guid (the
/// mover the SMSG named — vmangos `MoveSpeedAck::ReadFromWorldPacket` extracts a plain `ObjectGuid`,
/// which reads a raw `uint64`, `ObjectGuid.cpp:180` — NOT packed, unlike the SMSG's guid), the
/// echoed movement counter, our live `MovementInfo`, and the echoed new speed. The server accepts
/// only an exact counter match and a speed within ±0.01 of what it sent
/// (`Unit::FindPendingMovementSpeedChange`), then relocates us to the carried pose — so the info
/// must be the mover's honest live state, same as any `MSG_MOVE_*`.
pub fn force_speed_ack(guid: u64, counter: u32, info: &MovementInfo, speed: f32) -> Vec<u8> {
    let mut body = Vec::with_capacity(44);
    body.extend_from_slice(&guid.to_le_bytes());
    body.extend_from_slice(&counter.to_le_bytes());
    info.write(&mut body);
    body.extend_from_slice(&speed.to_le_bytes());
    body
}

/// Body of `CMSG_PING` (the ~30 s keepalive): `{u32 sequence, u32 lastRtt}` — VERIFIED twice, wow-re
/// net W1 (`SendPing 0x537e10`: seq = ++counter, then the last measured round-trip ms) and vmangos
/// `WorldSocket::_HandlePing` (`recvPacket >> ping >> latency`). The server echoes `sequence` back
/// as `SMSG_PONG` and stores `lastRtt` as the session's reported latency.
pub fn ping(sequence: u32, last_rtt_ms: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(8);
    body.extend_from_slice(&sequence.to_le_bytes());
    body.extend_from_slice(&last_rtt_ms.to_le_bytes());
    body
}

/// Body of a movement-flag-change ack (`CMSG_FORCE_MOVE_[UN]ROOT_ACK` / `CMSG_MOVE_WATER_WALK_ACK`
/// — the death arc's root/water-walk family, decision 0308): a **full** `u64` guid, the echoed
/// `u32` movement counter, our current `MovementInfo`, and — for the water-walk ack ONLY — a
/// trailing `u32 apply` (VERIFIED vmangos `Server/Packets/Movement.cpp:38-59`:
/// `MoveFlagChangeAck` reads the apply dword, `MoveRootAck` does not). The counter MUST be the one
/// the server sent — a zero/greater counter trips its cheat log (`HandleMoveRootAck`).
pub fn move_flag_ack(guid: u64, counter: u32, info: &MovementInfo, apply: Option<bool>) -> Vec<u8> {
    let mut body = Vec::with_capacity(48);
    body.extend_from_slice(&guid.to_le_bytes());
    body.extend_from_slice(&counter.to_le_bytes());
    info.write(&mut body);
    if let Some(apply) = apply {
        body.extend_from_slice(&u32::from(apply).to_le_bytes());
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Vector3d;

    /// `CMSG_MOVE_SPLINE_DONE` body, byte-exact: a `MovementInfo` (here with no swim/jump tail),
    /// then the acked `splineId`, then the trailing float the server `read_skip`s. The order and the
    /// mandatory-but-ignored trailing four bytes are what vmangos `MoveSplineDone::ReadFromWorldPacket`
    /// requires (`>> movementInfo; >> splineId; read_skip<float>()`); omitting the float under-runs
    /// its parse. A new wire body never lands without a golden (method).
    #[test]
    fn move_spline_done_body_golden() {
        let info = MovementInfo {
            flags: 0, // no SWIMMING / JUMPING ⇒ no conditional MovementInfo tails
            timestamp: 0x1122_3344,
            position: Vector3d {
                x: 1.0,
                y: 2.0,
                z: 3.0,
            },
            orientation: 0.5,
            transport: None,
            pitch: 0.0,
            fall_time: 0x5566_7788,
            jump: None,
        };
        let body = move_spline_done(&info, 0xAABB_CCDD);
        #[rustfmt::skip]
        let want: &[u8] = &[
            0x00, 0x00, 0x00, 0x00, // flags
            0x44, 0x33, 0x22, 0x11, // timestamp
            0x00, 0x00, 0x80, 0x3F, // pos.x = 1.0
            0x00, 0x00, 0x00, 0x40, // pos.y = 2.0
            0x00, 0x00, 0x40, 0x40, // pos.z = 3.0
            0x00, 0x00, 0x00, 0x3F, // orientation = 0.5
            0x88, 0x77, 0x66, 0x55, // fall_time
            0xDD, 0xCC, 0xBB, 0xAA, // splineId
            0x00, 0x00, 0x80, 0x3F, // completion fraction 1.0 (server-skipped)
        ];
        assert_eq!(body, want, "CMSG_MOVE_SPLINE_DONE body");
        // The leading bytes are exactly a MovementInfo (the shape the server reads first).
        let mi = movement(&info);
        assert_eq!(
            &body[..mi.len()],
            &mi[..],
            "MovementInfo comes first, verbatim"
        );
        assert_eq!(
            body.len(),
            mi.len() + 8,
            "…then splineId + the skipped float"
        );
    }
}
