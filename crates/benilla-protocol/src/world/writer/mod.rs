use std::net::TcpStream;

use anyhow::Result;
use benilla_srp::vanilla_header::EncrypterHalf;

use crate::messages::{self, opcode, JumpInfo, TransportPose};

use super::movement::{client_uptime_ms, movement_info};
use super::send_packet;

mod channel;
mod group;
mod mail;
mod trade;

/// Write half of a split [`WorldSession`](super::WorldSession) — owns a cloned socket + the encrypter. Used to send our own
/// movement (`MSG_MOVE_*`). The active player must already be the confirmed mover (see
/// [`WorldSession::set_active_mover`](super::WorldSession::set_active_mover)). Coordinates are raw WoW yards; `orientation` is radians.
pub struct WorldWriter {
    pub(super) stream: TcpStream,
    pub(super) encrypter: EncrypterHalf,
    /// The language every chat send carries — the logged-in character's faction tongue,
    /// inherited from the session at split (set by `WorldSession::player_login` off the roster's
    /// race). Load-bearing: vmangos drops the whole message (dot-commands included) when the
    /// character doesn't know the language — a Horde character sending Common gets only an
    /// `SMSG_NOTIFICATION` back, never the say / the command.
    pub(super) chat_language: u32,
}

impl WorldWriter {
    fn send(&mut self, opcode: u16, body: &[u8]) -> Result<()> {
        send_packet(&mut self.stream, Some(&mut self.encrypter), opcode, body)
    }

    /// Send one self-movement packet: a `MSG_MOVE_*` `opcode` carrying a `MovementInfo` with the given
    /// `flags` + pose. The caller (the controller) chooses the opcode per movement-axis transition
    /// (start/stop forward/back/strafe/turn), the periodic heartbeat, and the facing update — exactly as
    /// the real client does — and the `flags` it passes are the live CMovement bits the server relays to
    /// nearby players. `flags` must only set bits whose `MovementInfo` tail we serialize — the base
    /// directional/turn/walk bits, `JUMPING` (with its `jump` tail), `SWIMMING` (with its `pitch`
    /// tail), and `ON_TRANSPORT` (with its `transport` local-frame tail — decision 0438 phase 2).
    #[allow(clippy::too_many_arguments)]
    pub fn send_movement(
        &mut self,
        opcode: u16,
        flags: u32,
        pos: [f32; 3],
        orientation: f32,
        pitch: f32,
        fall_time: u32,
        jump: Option<JumpInfo>,
        transport: Option<TransportPose>,
    ) -> Result<()> {
        let mut info = movement_info(pos, orientation, flags);
        // Each conditional tail is gated on its flag by the serializer, so the flag and the value must
        // agree (they share `flags`): `SWIMMING` ⇒ the swim pitch, `JUMPING` ⇒ the ballistic launch
        // tail, `ON_TRANSPORT` ⇒ the rider's local pose.
        info.pitch = pitch;
        info.fall_time = fall_time;
        info.jump = jump;
        info.transport = transport;
        self.send(opcode, &messages::movement(&info))
    }

    /// Acknowledge that a server-authored spline (Charge/knockback/taxi — an `SMSG_MONSTER_MOVE`
    /// addressed to our own guid) finished: `CMSG_MOVE_SPLINE_DONE` with a `MovementInfo` at the
    /// ride's endpoint and the `spline_id` we were driven by. The server sets `SplineDonePending` for
    /// a player mover and validates this against its newest spline id, then relocates us and
    /// re-broadcasts a stop/heartbeat to observers — so it must be sent, at rest, once the ride ends.
    pub fn move_spline_done(
        &mut self,
        flags: u32,
        pos: [f32; 3],
        orientation: f32,
        spline_id: u32,
    ) -> Result<()> {
        let info = movement_info(pos, orientation, flags);
        self.send(
            opcode::CMSG_MOVE_SPLINE_DONE,
            &messages::move_spline_done(&info, spline_id),
        )
    }

    /// Echo a cross-map worldport ack: confirms `SMSG_NEW_WORLD` so the server resumes its object
    /// stream on the new continent (`MSG_MOVE_WORLDPORT_ACK` has an empty body).
    pub fn worldport_ack(&mut self) -> Result<()> {
        self.send(opcode::MSG_MOVE_WORLDPORT_ACK, &[])
    }

    /// Answer a `SMSG_FORCE_*_SPEED_CHANGE` (`CMSG_FORCE_*_SPEED_CHANGE_ACK`, picked by `kind`):
    /// echo the mover `guid` + `counter` + the exact `speed` the server sent, carrying our live
    /// `MovementInfo` (same field set as [`Self::send_movement`] — the server relocates us to it).
    /// Mandatory: unacked, the server force-resolves the change after ~4 s and flags its anticheat.
    #[allow(clippy::too_many_arguments)]
    pub fn force_speed_change_ack(
        &mut self,
        kind: messages::SpeedKind,
        guid: u64,
        counter: u32,
        speed: f32,
        flags: u32,
        pos: [f32; 3],
        orientation: f32,
        pitch: f32,
        fall_time: u32,
        jump: Option<JumpInfo>,
        transport: Option<TransportPose>,
    ) -> Result<()> {
        let mut info = movement_info(pos, orientation, flags);
        info.pitch = pitch;
        info.fall_time = fall_time;
        info.jump = jump;
        info.transport = transport;
        self.send(
            kind.ack_opcode(),
            &messages::force_speed_ack(guid, counter, &info, speed),
        )
    }

    /// Send the ~30 s keepalive (`CMSG_PING`): `sequence` is the ++counter the server echoes back
    /// as `SMSG_PONG`, `last_rtt_ms` the previous round-trip measurement (the real client's
    /// lastRtt; the server stores it as our reported latency). Cadence discipline is the caller's:
    /// vmangos kicks a socket whose pings repeat faster than 27 s apart (`_HandlePing`'s
    /// overspeed count), so this is a timer send, never a retry.
    pub fn ping(&mut self, sequence: u32, last_rtt_ms: u32) -> Result<()> {
        self.send(opcode::CMSG_PING, &messages::ping(sequence, last_rtt_ms))
    }

    /// Ask to leave the world back to character select (`CMSG_LOGOUT_REQUEST`, empty body). The
    /// server answers `SMSG_LOGOUT_RESPONSE` (a refusal while in combat) and, once the logout
    /// completes (instant for a resting/GM character), `SMSG_LOGOUT_COMPLETE` — which the stream
    /// surfaces as [`SessionEvent::LoggedOut`](crate::SessionEvent::LoggedOut).
    pub fn logout_request(&mut self) -> Result<()> {
        self.send(opcode::CMSG_LOGOUT_REQUEST, &[])
    }

    /// Acknowledge a triggered cinematic as finished (`CMSG_COMPLETE_CINEMATIC`, empty body) — the
    /// packet the real client sends when the cinematic ends or the player ESCs out. Must answer
    /// every `SMSG_TRIGGER_CINEMATIC` ([`SessionEvent::CinematicTriggered`]
    /// (crate::SessionEvent::CinematicTriggered)): while one runs unacked, vmangos anchors object
    /// visibility to the flying cinematic camera and the world around the body despawns.
    pub fn complete_cinematic(&mut self) -> Result<()> {
        self.send(opcode::CMSG_COMPLETE_CINEMATIC, &[])
    }

    /// Echo a same-map teleport ack so the server completes the near-teleport **immediately**
    /// (relocates us + streams the surrounding objects). Without a valid ack the teleport only
    /// finishes on a ~20s server-side fallback, so the destination's objects appear ~20s late.
    ///
    /// vanilla 1.12 / vmangos expect a **full 8-byte guid** for this opcode (the lone movement opcode
    /// that does — the `MovementInfo` ones are correctly packed). With a packed guid the server's
    /// `ByteBuffer` overruns and drops the packet (confirmed by a vmangos capture: opcode `0xC7` →
    /// `ByteBufferException`, then the teleport completing 20s later).
    pub fn teleport_ack(&mut self, guid: u64, counter: u32) -> Result<()> {
        self.send(
            opcode::MSG_MOVE_TELEPORT_ACK,
            &messages::teleport_ack(guid, counter, client_uptime_ms()),
        )
    }

    /// Acknowledge a server root/unroot on our mover (`SMSG_FORCE_MOVE_[UN]ROOT` → the matching
    /// `CMSG_FORCE_MOVE_[UN]ROOT_ACK`): full guid + the echoed counter + our current
    /// `MovementInfo`. Un-acked, the change never reaches observers; a wrong/zero counter trips
    /// the server's cheat log (`HandleMoveRootAck`). While rooted, `flags` must not carry moving
    /// bits (vmangos `MovementInfo.h`: moving flags with `MOVEFLAG_ROOT` freeze the real client).
    pub fn move_root_ack(
        &mut self,
        guid: u64,
        counter: u32,
        rooted: bool,
        flags: u32,
        pos: [f32; 3],
        orientation: f32,
    ) -> Result<()> {
        let info = movement_info(pos, orientation, flags);
        self.send(
            if rooted {
                opcode::CMSG_FORCE_MOVE_ROOT_ACK
            } else {
                opcode::CMSG_FORCE_MOVE_UNROOT_ACK
            },
            &messages::move_flag_ack(guid, counter, &info, None),
        )
    }

    /// Acknowledge a water-walk grant/removal on our mover (`SMSG_MOVE_WATER_WALK`/`LAND_WALK` →
    /// `CMSG_MOVE_WATER_WALK_ACK`, one ack opcode for both directions): the root ack's shape plus
    /// the trailing `u32 apply` the server's `MoveFlagChangeAck` reader expects.
    pub fn water_walk_ack(
        &mut self,
        guid: u64,
        counter: u32,
        on: bool,
        flags: u32,
        pos: [f32; 3],
        orientation: f32,
    ) -> Result<()> {
        let info = movement_info(pos, orientation, flags);
        self.send(
            opcode::CMSG_MOVE_WATER_WALK_ACK,
            &messages::move_flag_ack(guid, counter, &info, Some(on)),
        )
    }

    /// Release the spirit (`CMSG_REPOP_REQUEST`, empty body — decision 0308 slice 1): valid only
    /// while dead and unreleased (the server refuses it alive or already-ghost). The server
    /// answers with the ghost form (aura 8326 → the ghost flags), the corpse object, unroot,
    /// water-walk, `SMSG_CORPSE_RECLAIM_DELAY`, and the graveyard teleport.
    pub fn repop_request(&mut self) -> Result<()> {
        self.send(opcode::CMSG_REPOP_REQUEST, &[])
    }

    /// Ask where our corpse is (`MSG_CORPSE_QUERY`, empty request): answered by the same opcode
    /// (the [`SessionEvent::CorpseQuery`](crate::SessionEvent::CorpseQuery) feed for the map
    /// markers + the corpse-run range gate).
    pub fn corpse_query(&mut self) -> Result<()> {
        self.send(opcode::MSG_CORPSE_QUERY, &[])
    }

    /// Reclaim our corpse (`CMSG_RECLAIM_CORPSE` — the RECOVER_CORPSE popup's Accept): the corpse's
    /// guid. Server gates: ghost, the reclaim delay elapsed, within 39 yd. Success comes back as
    /// ordinary descriptor deltas (alive, ghost flags clear) + the corpse-to-bones swap.
    pub fn reclaim_corpse(&mut self, corpse_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_RECLAIM_CORPSE,
            &messages::reclaim_corpse(corpse_guid),
        )
    }

    /// Take the spirit healer's resurrection (`CMSG_SPIRIT_HEALER_ACTIVATE` — the XP_LOSS
    /// confirm's final Accept): res at 50%, 25% durability loss, resurrection sickness at
    /// level ≥ 11. `npc` is the spirit healer's guid (from `SMSG_SPIRIT_HEALER_CONFIRM`).
    pub fn spirit_healer_activate(&mut self, npc: u64) -> Result<()> {
        self.send(
            opcode::CMSG_SPIRIT_HEALER_ACTIVATE,
            &messages::spirit_healer_activate(npc),
        )
    }

    /// Answer a resurrection offer (`CMSG_RESURRECT_RESPONSE` — the RESURRECT popup's
    /// Accept/Decline): the offerer's guid + the accept byte.
    pub fn resurrect_response(&mut self, caster: u64, accept: bool) -> Result<()> {
        self.send(
            opcode::CMSG_RESURRECT_RESPONSE,
            &messages::resurrect_response(caster, accept),
        )
    }

    /// Set (or clear) our current target: `CMSG_SET_SELECTION` carrying a **full 8-byte GUID** (verified
    /// vmangos `SetSelection::ReadFromWorldPacket` — `recv_data >> guid` reads a raw `uint64`, not a
    /// packed guid). `guid == 0` clears the selection. The real client sends this the moment the local
    /// player picks a unit; the server records it in the player's `UNIT_FIELD_TARGET` and relays it to
    /// nearby observers.
    pub fn set_selection(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_SET_SELECTION, &messages::full_guid(guid))
    }

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

    /// Ask for a player character's name/race/gender/class (`CMSG_NAME_QUERY`, a full 8-byte guid —
    /// vmangos `QueryPlayerName::ReadFromWorldPacket`). Answered by `SMSG_NAME_QUERY_RESPONSE`.
    pub fn name_query(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_NAME_QUERY, &messages::full_guid(guid))
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

    /// Ask for a creature template's name/subname (`CMSG_CREATURE_QUERY`: entry + guid). The `entry`
    /// is the one embedded in the creature's guid bits 24–47 ([`crate::guid::entry`]). Answered by
    /// `SMSG_CREATURE_QUERY_RESPONSE`.
    pub fn creature_query(&mut self, entry: u32, guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_CREATURE_QUERY,
            &messages::creature_query(entry, guid),
        )
    }

    /// Ask for a pet's name (`CMSG_PET_NAME_QUERY`: pet number + guid). A pet's guid holds a pet
    /// number where a creature's holds its template entry ([`crate::guid::pet_number`]), so
    /// [`Self::creature_query`] cannot name one — this is the only query that can. Answered by
    /// `SMSG_PET_NAME_QUERY_RESPONSE`, or by silence if the pet is gone.
    pub fn pet_name_query(&mut self, pet_number: u32, guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_PET_NAME_QUERY,
            &messages::pet_name_query(pet_number, guid),
        )
    }

    /// Cast a spell (`CMSG_CAST_SPELL`): `target: None` = a self/implicit cast, `Some(guid)` = an
    /// explicit unit target (body in [`messages::cast_spell`]). The server answers
    /// `SMSG_CAST_RESULT` (and `SMSG_SPELL_START`/`GO` on success, unmodelled yet).
    pub fn cast_spell(&mut self, spell_id: u32, target: Option<u64>) -> Result<()> {
        self.send(
            opcode::CMSG_CAST_SPELL,
            &messages::cast_spell(spell_id, target),
        )
    }

    /// Cancel one of our own auras (`CMSG_CANCEL_AURA`, body in [`messages::cancel_aura`]) — the
    /// right-click-a-buff wire. Carries the **spell id**, not a slot; the server refuses passives,
    /// no-cancel spells and debuffs (decision 0257). No answer packet — the removal arrives as a
    /// `UNIT_FIELD_AURA` delta zeroing the slot.
    pub fn cancel_aura(&mut self, spell_id: u32) -> Result<()> {
        self.send(opcode::CMSG_CANCEL_AURA, &messages::cancel_aura(spell_id))
    }

    /// Cast an OPEN_LOCK spell at a **GameObject** (`CMSG_CAST_SPELL`, body in
    /// [`messages::cast_spell_gameobject`]) — the right-click on a locked chest / mining vein / herb
    /// node (decision 0239). The server runs `EffectOpenLock` → for a chest, opens the loot
    /// (`SMSG_LOOT_RESPONSE`); the profession/skill gate is the server's. Answered by
    /// `SMSG_CAST_RESULT` on refusal.
    pub fn cast_spell_gameobject(&mut self, spell_id: u32, go_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_CAST_SPELL,
            &messages::cast_spell_gameobject(spell_id, go_guid),
        )
    }

    /// Cast an item-targeted spell (`CMSG_CAST_SPELL` with `TARGET_FLAG_ITEM` + the item's packed
    /// guid — [`messages::cast_spell_item`]): the enchant cast the CraftFrame's item pick
    /// completes (decision 0437 phase 3). The server resolves the item, checks reagents, applies
    /// the enchant; refusal answers `SMSG_CAST_RESULT`.
    pub fn cast_spell_item(&mut self, spell_id: u32, item_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_CAST_SPELL,
            &messages::cast_spell_item(spell_id, item_guid),
        )
    }

    /// Start melee auto-attack on `guid` (`CMSG_ATTACKSWING`, a full 8-byte guid — vmangos
    /// `AttackSwing::ReadFromWorldPacket`). Echoed back as `SMSG_ATTACKSTART`.
    pub fn attack_swing(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_ATTACKSWING, &messages::attack_swing(guid))
    }

    /// Stop melee auto-attack (`CMSG_ATTACKSTOP`, empty body). Echoed as `SMSG_ATTACKSTOP`.
    pub fn attack_stop(&mut self) -> Result<()> {
        self.send(opcode::CMSG_ATTACKSTOP, &[])
    }

    /// Stop our ranged auto-repeat (`CMSG_CANCEL_AUTO_REPEAT_SPELL`, empty body) — the ack every
    /// local cancel sends (the client's one send site `0x6ea0c6`, inside the cancel `0x6ea080`).
    pub fn cancel_auto_repeat(&mut self) -> Result<()> {
        self.send(opcode::CMSG_CANCEL_AUTO_REPEAT_SPELL, &[])
    }

    /// Cancel a named in-flight cast (`CMSG_CANCEL_CAST`: one `u32` spell id — vmangos
    /// `HandleCancelCastOpcode`). Sent by the wand-only auto-repeat handoff (`0x6095b8`) and by
    /// the cast bar's local self-cancel (movement/Esc mid-cast, `benilla::ui_cast`).
    pub fn cancel_cast(&mut self, spell_id: u32) -> Result<()> {
        self.send(opcode::CMSG_CANCEL_CAST, &spell_id.to_le_bytes())
    }

    /// End our own running channel (`CMSG_CANCEL_CHANNELLING`: one `u32` spell id, which vmangos
    /// reads and ignores — the interrupt is unconditional; the real client still writes it). The
    /// channel half of the local self-cancel (`benilla::ui_cast`).
    pub fn cancel_channelling(&mut self, spell_id: u32) -> Result<()> {
        self.send(opcode::CMSG_CANCEL_CHANNELLING, &spell_id.to_le_bytes())
    }

    /// Set (or clear, `packed == 0`) one action-bar slot (`CMSG_SET_ACTION_BUTTON`, layout in
    /// [`messages::set_action_button`]) — decision 0216 §7/0218 §4: the bar is
    /// client-authoritative, so this is the ONLY wire traffic a local pickup/place/hop generates,
    /// one send per slot mutation (a drag-swap is two sends, never atomic). No dedicated answer
    /// packet — `SMSG_ACTION_BUTTONS` only ever re-arrives on a server-side edit (a GM command, a
    /// macro-menu save), never as our own edit's echo.
    pub fn set_action_button(&mut self, button: u8, packed: u32) -> Result<()> {
        self.send(
            opcode::CMSG_SET_ACTION_BUTTON,
            &messages::set_action_button(button, packed),
        )
    }

    /// Tell the server our sheath state (`CMSG_SETSHEATHED`): `state` 0 unarmed/stowed, 1 melee
    /// drawn, 2 ranged drawn. Purely client-volunteered (body in [`messages::set_sheathed`]) — it
    /// becomes our own `UNIT_FIELD_BYTES_2` sheath byte, which other clients read. Sent by the
    /// manual toggle, the attack-start auto-draw, and every reconcile force (byte-verified: the
    /// setter `0x611cf0` sends it whenever `bFireEvent` — wow-re `sheath-policy.md`,
    /// decision 0080).
    pub fn set_sheathed(&mut self, state: u32) -> Result<()> {
        self.send(opcode::CMSG_SETSHEATHED, &messages::set_sheathed(state))
    }

    /// Tell the server our stand state (`CMSG_STANDSTATECHANGE`): 0 stand · 1 sit · 3 sleep ·
    /// 8 kneel (the only values vmangos accepts). Client-volunteered like sheath — the echo into
    /// `UNIT_FIELD_BYTES_1` byte 0 drives every observer's sit/stand pose (decision 0080c).
    pub fn stand_state_change(&mut self, state: u32) -> Result<()> {
        self.send(
            opcode::CMSG_STANDSTATECHANGE,
            &messages::stand_state_change(state),
        )
    }

    /// The mounted space-bar flourish (`CMSG_MOUNTSPECIAL_ANIM`, EMPTY body — VERIFIED vmangos
    /// `HandleMountSpecialAnimOpcode`). The sender plays its own MountSpecial(94) locally at
    /// send time and self-suppresses the broadcast echo (decision 0441 P2 — whether the echo
    /// arrives is a server-config detail).
    pub fn mount_special(&mut self) -> Result<()> {
        self.send(opcode::CMSG_MOUNTSPECIAL_ANIM, &[])
    }

    /// Ask an item template's display head (`CMSG_ITEM_QUERY_SINGLE`: entry + item guid, 0 for
    /// template-only asks). Answered by `SMSG_ITEM_QUERY_SINGLE_RESPONSE` (an `ItemTemplate`
    /// event) — the T2 container groundwork.
    pub fn item_query(&mut self, entry: u32, guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_ITEM_QUERY_SINGLE,
            &messages::item_query(entry, guid),
        )
    }

    /// Use an item by bag position (`CMSG_USE_ITEM`, layout in [`messages::use_item`]) — eat the
    /// food, drink the potion, hearthstone home. The server answers with the effect (values
    /// deltas, a stack decrement/destroy) or `SMSG_CAST_RESULT` on refusal.
    pub fn use_item(&mut self, bag_index: u8, slot: u8, spell_slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_USE_ITEM,
            &messages::use_item(bag_index, slot, spell_slot),
        )
    }

    /// Equip a bag item (`CMSG_AUTOEQUIP_ITEM`, layout in [`messages::auto_equip_item`]) — the
    /// server picks the destination slot. Success arrives as inventory-slot values deltas (and the
    /// visible-item change everyone renders); refusal as `SMSG_INVENTORY_CHANGE_FAILURE`.
    pub fn auto_equip_item(&mut self, bag_index: u8, slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_AUTOEQUIP_ITEM,
            &messages::auto_equip_item(bag_index, slot),
        )
    }

    /// Load ammo into the ammo slot (`CMSG_SET_AMMO`, layout in [`messages::set_ammo`]) — the
    /// client's own auto-equip fork for ammo-class items (wow-re `cursor-dragdrop-slots.md`).
    /// Addressed by item *entry*, not a bag slot; the stack stays in the bag and `PLAYER_AMMO_ID`
    /// starts referencing it. A wrong/absent ranged weapon refuses via
    /// `SMSG_INVENTORY_CHANGE_FAILURE`. Decision 0526.
    pub fn set_ammo(&mut self, entry: u32) -> Result<()> {
        self.send(opcode::CMSG_SET_AMMO, &messages::set_ammo(entry))
    }

    /// Swap two of the player's own inventory slots (`CMSG_SWAP_INV_ITEM`, layout in
    /// [`messages::swap_inv_item`]) — the wire for a backpack-internal pick/place/swap (both slots
    /// are `INVENTORY_SLOT_ITEM_START`+i). An empty destination is a move; the server settles both
    /// slots with values deltas, or refuses via `SMSG_INVENTORY_CHANGE_FAILURE`.
    pub fn swap_inv_item(&mut self, src_slot: u8, dst_slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_SWAP_INV_ITEM,
            &messages::swap_inv_item(src_slot, dst_slot),
        )
    }

    /// The general bag↔bag move (`CMSG_SWAP_ITEM`, layout in [`messages::swap_item`]): either
    /// endpoint may be an equipped bag (unlike [`Self::swap_inv_item`], which only ever addresses
    /// the player's own grid) — the wire for a whole-space bag-window pick/place/swap (decision
    /// 0216 §6, slice 2). An empty destination is a move, same as `swap_inv_item`; refusal answers
    /// `SMSG_INVENTORY_CHANGE_FAILURE`.
    pub fn swap_item(
        &mut self,
        dst_bag: u8,
        dst_slot: u8,
        src_bag: u8,
        src_slot: u8,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_SWAP_ITEM,
            &messages::swap_item(dst_bag, dst_slot, src_bag, src_slot),
        )
    }

    /// Split a stack (`CMSG_SPLIT_ITEM`, layout in [`messages::split_item`]): carry `count` off
    /// `(src_bag, src_slot)` onto `(dst_bag, dst_slot)` — either endpoint may be an equipped bag
    /// (unlike [`Self::swap_inv_item`]). Success settles both slots via values deltas; refusal
    /// answers `SMSG_INVENTORY_CHANGE_FAILURE`.
    pub fn split_item(
        &mut self,
        src_bag: u8,
        src_slot: u8,
        dst_bag: u8,
        dst_slot: u8,
        count: u8,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_SPLIT_ITEM,
            &messages::split_item(src_bag, src_slot, dst_bag, dst_slot, count),
        )
    }

    /// Destroy a bag item (`CMSG_DESTROYITEM`, layout in [`messages::destroy_item`]): `count` 0 =
    /// the whole stack. The delete-confirm popup's `OnAccept` (decision 0216 §3) — no dedicated
    /// answer packet; the item's disappearance is the ordinary field-update stream.
    pub fn destroy_item(&mut self, bag: u8, slot: u8, count: u8) -> Result<()> {
        self.send(
            opcode::CMSG_DESTROYITEM,
            &messages::destroy_item(bag, slot, count),
        )
    }

    /// Open a gossip menu on an NPC (`CMSG_GOSSIP_HELLO`, layout in [`messages::gossip_hello`]) —
    /// works on any interactable creature, not only gossip-flagged ones (vmangos
    /// `CanInteractWithNPC`, `Player.cpp:347`, passes `UNIT_NPC_FLAG_NONE` for this opcode).
    /// Answered by `SMSG_GOSSIP_MESSAGE` (a `GossipMenu` event).
    pub fn gossip_hello(&mut self, npc_guid: u64) -> Result<()> {
        self.send(opcode::CMSG_GOSSIP_HELLO, &messages::gossip_hello(npc_guid))
    }

    /// Choose a gossip option (`CMSG_GOSSIP_SELECT_OPTION`, layout in
    /// [`messages::gossip_select_option`]): `gossip_list_id` is the option's echoed `index`; `code`
    /// carries a password only for a `coded` option, omitted entirely otherwise. The server answers
    /// either a fresh `SMSG_GOSSIP_MESSAGE` (a sub-menu) or `SMSG_GOSSIP_COMPLETE`.
    pub fn gossip_select_option(
        &mut self,
        npc_guid: u64,
        gossip_list_id: u32,
        code: Option<&str>,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_GOSSIP_SELECT_OPTION,
            &messages::gossip_select_option(npc_guid, gossip_list_id, code),
        )
    }

    /// Ask for a gossip menu's greeting text (`CMSG_NPC_TEXT_QUERY`, layout in
    /// [`messages::npc_text_query`]) — sent on receiving a gossip menu's `text_id`. Answered by
    /// `SMSG_NPC_TEXT_UPDATE` (an `NpcGreeting` event); ask-once cacheable like an item template.
    pub fn npc_text_query(&mut self, text_id: u32, guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_NPC_TEXT_QUERY,
            &messages::npc_text_query(text_id, guid),
        )
    }

    /// Ask a vendor's stock (`CMSG_LIST_INVENTORY`, layout in [`messages::list_inventory`]) — the
    /// server requires `UNIT_NPC_FLAG_VENDOR` + the player alive (`ItemHandler.cpp:693`). Answered
    /// by `SMSG_LIST_INVENTORY` (a `VendorInventory` event).
    pub fn list_inventory(&mut self, vendor_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_LIST_INVENTORY,
            &messages::list_inventory(vendor_guid),
        )
    }

    /// Buy from a vendor (`CMSG_BUY_ITEM`, layout in [`messages::buy_item`]): `entry` is the item
    /// **template** id (not the vendor row's `muid`), `count` the number of stacks. Auto-places
    /// into the first free bag slot. Success updates the vendor stock (`SMSG_BUY_ITEM`) and
    /// delivers the item via the normal item-create path; refusal answers `SMSG_BUY_FAILED`.
    pub fn buy_item(&mut self, vendor_guid: u64, entry: u32, count: u8) -> Result<()> {
        self.send(
            opcode::CMSG_BUY_ITEM,
            &messages::buy_item(vendor_guid, entry, count),
        )
    }

    /// Sell an item to a vendor (`CMSG_SELL_ITEM`, layout in [`messages::sell_item`]): `count` 0 =
    /// sell the whole stack. Success is silent (the item vanishes + coinage rises via
    /// `UPDATE_OBJECT`); refusal answers `SMSG_SELL_ITEM`'s error shape (a `VendorSellFailed`
    /// event).
    pub fn sell_item(&mut self, vendor_guid: u64, item_guid: u64, count: u8) -> Result<()> {
        self.send(
            opcode::CMSG_SELL_ITEM,
            &messages::sell_item(vendor_guid, item_guid, count),
        )
    }

    /// Buy a sold item back (`CMSG_BUYBACK_ITEM`, layout in [`messages::buyback_item`]): `slot` is
    /// the absolute player-array buyback slot 69–80. Success is the item re-creating + coinage
    /// falling via `UPDATE_OBJECT`; refusal answers `SMSG_BUY_FAILED`.
    pub fn buyback_item(&mut self, vendor_guid: u64, slot: u32) -> Result<()> {
        self.send(
            opcode::CMSG_BUYBACK_ITEM,
            &messages::buyback_item(vendor_guid, slot),
        )
    }

    /// Repair at a repair-capable vendor (`CMSG_REPAIR_ITEM`, layout in
    /// [`messages::repair_item`]): `item_guid` 0 = repair everything. No dedicated answer packet —
    /// durability rises + coinage falls via `UPDATE_OBJECT`.
    pub fn repair_item(&mut self, vendor_guid: u64, item_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_REPAIR_ITEM,
            &messages::repair_item(vendor_guid, item_guid),
        )
    }

    /// Open the bank (`CMSG_BANKER_ACTIVATE`, layout in [`messages::banker_activate`]) — one
    /// 8-byte banker guid. Answered by `SMSG_SHOW_BANK` (decision 0604).
    pub fn banker_activate(&mut self, banker_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_BANKER_ACTIVATE,
            &messages::banker_activate(banker_guid),
        )
    }

    /// Buy the next bank-bag slot (`CMSG_BUY_BANK_SLOT`, layout in [`messages::buy_bank_slot`]).
    /// No packet on success (the PLAYER_BYTES_2 count + coinage deltas are the confirmation);
    /// refusal answers `SMSG_BUY_BANK_SLOT_RESULT`.
    pub fn buy_bank_slot(&mut self, banker_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_BUY_BANK_SLOT,
            &messages::buy_bank_slot(banker_guid),
        )
    }

    /// Deposit an item into the bank (`CMSG_AUTOBANK_ITEM`, layout in
    /// [`messages::autobank_item`]): the wire `(bag, slot)` of the source item.
    pub fn autobank_item(&mut self, bag: u8, slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_AUTOBANK_ITEM,
            &messages::autobank_item(bag, slot),
        )
    }

    /// Withdraw a bank item into the bags (`CMSG_AUTOSTORE_BANK_ITEM`, layout in
    /// [`messages::autostore_bank_item`]): the wire `(bag, slot)` of the bank item (vmangos
    /// routes by whether the source is a bank position, so it tolerates either direction).
    pub fn autostore_bank_item(&mut self, bag: u8, slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_AUTOSTORE_BANK_ITEM,
            &messages::autostore_bank_item(bag, slot),
        )
    }

    /// Ask (or re-ask) a trainer's service list (`CMSG_TRAINER_LIST`, layout in
    /// [`messages::trainer_list`]) — one 8-byte trainer guid. The window first *opens* off the
    /// gossip trainer option's `SMSG_TRAINER_LIST`; this is the *refresh* verb, re-requested after a
    /// purchase to repaint the bought row green→gray (vmangos `HandleTrainerListOpcode` honors a
    /// standalone re-request while the player can still interact — VERIFIED `NPCHandler.cpp:92-95`,
    /// the server does not auto-resend on a buy). Answered by `SMSG_TRAINER_LIST` (a `TrainerList`
    /// event).
    pub fn trainer_list(&mut self, trainer_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_TRAINER_LIST,
            &messages::trainer_list(trainer_guid),
        )
    }

    /// Buy (learn) a trainer service (`CMSG_TRAINER_BUY_SPELL`, layout in
    /// [`messages::trainer_buy_spell`]): the trainer guid + the service's spell id. Success answers
    /// `SMSG_TRAINER_BUY_SUCCEEDED` and delivers the spell via the learn effect's `SMSG_LEARNED_SPELL`
    /// (the green→gray repaint then needs a `CMSG_TRAINER_LIST` re-request); refusal answers
    /// `SMSG_TRAINER_BUY_FAILED` with a [`messages::train_fail`] code.
    pub fn trainer_buy_spell(&mut self, trainer_guid: u64, spell_id: u32) -> Result<()> {
        self.send(
            opcode::CMSG_TRAINER_BUY_SPELL,
            &messages::trainer_buy_spell(trainer_guid, spell_id),
        )
    }

    /// Ask a nearby flight master's known status (`CMSG_TAXINODE_STATUS_QUERY`, layout in
    /// [`messages::taxi_node_status_query`]): the flight master's guid, not ours. Answered by
    /// `SMSG_TAXINODE_STATUS` (a `TaxiNodeStatus` event, decision 0484).
    pub fn taxi_node_status_query(&mut self, flightmaster_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_TAXINODE_STATUS_QUERY,
            &messages::taxi_node_status_query(flightmaster_guid),
        )
    }

    /// Open a flight master's taxi map (`CMSG_TAXIQUERYAVAILABLENODES`, layout in
    /// [`messages::taxi_query_available_nodes`], decision 0496 I4 — CONFIRMED as built: the
    /// interact ladder is first-match-wins low→high over `UNIT_NPC_FLAGS`, so only a pure
    /// flightmaster reaches here). A known node answers `SMSG_SHOWTAXINODES` (a `TaxiNodesShown`
    /// event); a never-visited node instead answers the first-visit learn pair (`NewTaxiPath` +
    /// `TaxiNodeStatus`) and opens no menu on this click.
    pub fn taxi_query_available_nodes(&mut self, flightmaster_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_TAXIQUERYAVAILABLENODES,
            &messages::taxi_query_available_nodes(flightmaster_guid),
        )
    }

    /// Fly a single hop (`CMSG_ACTIVATETAXI`, layout in [`messages::activate_taxi`]): the
    /// flight-master guid, the source node, the destination node. Answered by
    /// `SMSG_ACTIVATETAXIREPLY`; success continues into the mount + `SMSG_MONSTER_MOVE` flight.
    pub fn activate_taxi(
        &mut self,
        flightmaster_guid: u64,
        source_node: u32,
        dest_node: u32,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_ACTIVATETAXI,
            &messages::activate_taxi(flightmaster_guid, source_node, dest_node),
        )
    }

    /// Fly a multi-hop chain in one send (`CMSG_ACTIVATETAXIEXPRESS`, layout in
    /// [`messages::activate_taxi_express`], decision 0496 §TU-3 — sent when no direct `TaxiPath`
    /// edge exists current→target, the real discriminator the verdict corrected from a hop-count
    /// guess): the flight-master guid, the route's combined fare, and the full node chain in
    /// order. Answered by `SMSG_ACTIVATETAXIREPLY`, same as [`Self::activate_taxi`].
    pub fn activate_taxi_express(
        &mut self,
        flightmaster_guid: u64,
        total_cost: u32,
        nodes: &[u32],
    ) -> Result<()> {
        self.send(
            opcode::CMSG_ACTIVATETAXIEXPRESS,
            &messages::activate_taxi_express(flightmaster_guid, total_cost, nodes),
        )
    }

    /// Unlearn a whole skill line (`CMSG_UNLEARN_SKILL`, layout in [`messages::unlearn_skill`]) —
    /// the skills pane's abandon. No ack: the server's `SetSkill(id, 0, 0)` comes back as a
    /// `PLAYER_SKILL_INFO` field update.
    pub fn unlearn_skill(&mut self, skill_id: u32) -> Result<()> {
        self.send(
            opcode::CMSG_UNLEARN_SKILL,
            &messages::unlearn_skill(skill_id),
        )
    }

    /// Spend talent points (`CMSG_LEARN_TALENT`, layout in [`messages::learn_talent`]): the
    /// `Talent.dbc` row id + the requested rank (0-based, learn-up-to). No dedicated reply — the
    /// server validates silently; success arrives as the rank spell's learn effects plus the
    /// refreshed `PLAYER_CHARACTER_POINTS1` (decision 0304).
    pub fn learn_talent(&mut self, talent_id: u32, requested_rank: u32) -> Result<()> {
        self.send(
            opcode::CMSG_LEARN_TALENT,
            &messages::learn_talent(talent_id, requested_rank),
        )
    }

    /// Open a loot window (`CMSG_LOOT`, layout in [`messages::loot`]) — a full guid naming the
    /// corpse/creature/player to loot. Answered by `SMSG_LOOT_RESPONSE` (a `LootResponse` event
    /// on success, `LootError` on refusal).
    pub fn loot(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_LOOT, &messages::loot(guid))
    }

    /// Use a world GameObject (`CMSG_GAMEOBJ_USE`, layout in [`messages::gameobj_use`]) — the single
    /// player-facing verb for any usable GO (decision 0236): a full guid naming the chest/door/quest
    /// object/lever. The server fans it out by GO type — a chest answers with `SMSG_LOOT_RESPONSE`
    /// (the loot window), a questgiver GO with the gossip/quest packets, a door with a
    /// `GAMEOBJECT_STATE` flip via `UPDATE_OBJECT` — or refuses silently (out of range, locked, no
    /// quest). There is no dedicated success reply.
    pub fn gameobj_use(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_GAMEOBJ_USE, &messages::gameobj_use(guid))
    }

    /// Ask for a GameObject template's type/display/name/`data[24]` head (`CMSG_GAMEOBJECT_QUERY`:
    /// entry + guid — the ask-once template lookup, decision 0236). The `entry` is the one embedded
    /// in the GameObject's guid bits 24–47 ([`crate::guid::entry`]), the same convention as
    /// [`WorldWriter::creature_query`]. Answered by `SMSG_GAMEOBJECT_QUERY_RESPONSE`.
    pub fn gameobject_query(&mut self, entry: u32, guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_GAMEOBJECT_QUERY,
            &messages::gameobject_query(entry, guid),
        )
    }

    /// Take one loot-window row (`CMSG_AUTOSTORE_LOOT_ITEM`, layout in
    /// [`messages::autostore_loot_item`]): `loot_slot` is the wire's 0-based row index. The server
    /// auto-places the item into the first free bag slot; success arrives via the normal
    /// item-create/values path plus `SMSG_LOOT_REMOVED` clearing the row and
    /// `SMSG_ITEM_PUSH_RESULT` naming what landed.
    pub fn autostore_loot_item(&mut self, loot_slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_AUTOSTORE_LOOT_ITEM,
            &messages::autostore_loot_item(loot_slot),
        )
    }

    /// Take the loot's coin pile (`CMSG_LOOT_MONEY`, empty body). Answered by
    /// `SMSG_LOOT_MONEY_NOTIFY` (our share) then `SMSG_LOOT_CLEAR_MONEY` (the coin line clears for
    /// every looter) plus the coinage rising via `UPDATE_OBJECT`.
    pub fn loot_money(&mut self) -> Result<()> {
        self.send(opcode::CMSG_LOOT_MONEY, &messages::loot_money())
    }

    /// Close the loot window (`CMSG_LOOT_RELEASE`, layout in [`messages::loot_release`]); the
    /// server ignores `guid` and releases whatever loot guid it has stored for us instead.
    /// Answered by `SMSG_LOOT_RELEASE_RESPONSE` (a `LootReleaseResponse` event).
    pub fn loot_release(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_LOOT_RELEASE, &messages::loot_release(guid))
    }

    /// Cast a group-loot vote (`CMSG_LOOT_ROLL`, layout in [`messages::loot_roll`]) — the roll is
    /// addressed by the `(looted_target, item_slot)` pair `SMSG_LOOT_START_ROLL` opened it with,
    /// never by the client-internal `rollID`. `roll_type` is a [`messages::roll_vote`] value; the
    /// server drops anything `>= 3` without a reply. Answered by an `SMSG_LOOT_ROLL` broadcast of
    /// our vote, then the resolution (`SMSG_LOOT_ROLL_WON` / `SMSG_LOOT_ALL_PASSED`).
    pub fn loot_roll(&mut self, looted_target: u64, item_slot: u32, roll_type: u8) -> Result<()> {
        self.send(
            opcode::CMSG_LOOT_ROLL,
            &messages::loot_roll(looted_target, item_slot, roll_type),
        )
    }

    /// Ask a quest's detail panel (`CMSG_QUESTGIVER_QUERY_QUEST`, layout in
    /// [`messages::questgiver_query_quest`]) — the click a greeting/gossip quest row makes.
    /// Answered by `SMSG_QUESTGIVER_QUEST_DETAILS` (a `QuestDetail` event).
    pub fn questgiver_query_quest(&mut self, npc: u64, quest: u32) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_QUERY_QUEST,
            &messages::questgiver_query_quest(npc, quest),
        )
    }

    /// Accept a quest (`CMSG_QUESTGIVER_ACCEPT_QUEST`, layout in
    /// [`messages::questgiver_accept_quest`]) — the detail panel's Accept button. Adds it to the log;
    /// the server closes the gossip window (`SMSG_GOSSIP_COMPLETE`).
    pub fn questgiver_accept_quest(&mut self, npc: u64, quest: u32) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_ACCEPT_QUEST,
            &messages::questgiver_accept_quest(npc, quest),
        )
    }

    /// Ask a quest's turn-in progress panel (`CMSG_QUESTGIVER_COMPLETE_QUEST`, layout in
    /// [`messages::questgiver_complete_quest`]) — answered by `SMSG_QUESTGIVER_REQUEST_ITEMS`
    /// (a `QuestProgress` event), or OFFER_REWARD when there are no required items.
    pub fn questgiver_complete_quest(&mut self, npc: u64, quest: u32) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_COMPLETE_QUEST,
            &messages::questgiver_complete_quest(npc, quest),
        )
    }

    /// Advance from the progress panel to the reward panel (`CMSG_QUESTGIVER_REQUEST_REWARD`, layout
    /// in [`messages::questgiver_request_reward`]) — the progress panel's Continue button. Answered
    /// by `SMSG_QUESTGIVER_OFFER_REWARD` (a `QuestOffer` event).
    pub fn questgiver_request_reward(&mut self, npc: u64, quest: u32) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_REQUEST_REWARD,
            &messages::questgiver_request_reward(npc, quest),
        )
    }

    /// Choose a reward and finish the quest (`CMSG_QUESTGIVER_CHOOSE_REWARD`, layout in
    /// [`messages::questgiver_choose_reward`]; `reward` = choice index) — the reward panel's Complete
    /// button. Answered by `SMSG_QUESTGIVER_QUEST_COMPLETE` (a `QuestComplete` event) + the
    /// XP/money/item grants via `UPDATE_OBJECT`.
    pub fn questgiver_choose_reward(&mut self, npc: u64, quest: u32, reward: u32) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_CHOOSE_REWARD,
            &messages::questgiver_choose_reward(npc, quest, reward),
        )
    }

    /// Ask a quest's full template (`CMSG_QUEST_QUERY`, layout in [`messages::quest_query`]) — the
    /// quest-log detail pane's ask-once source, distinct from [`Self::questgiver_query_quest`]
    /// (which needs an NPC guid, not just the quest id). Answered by `SMSG_QUEST_QUERY_RESPONSE`
    /// (a `QuestTemplate` event).
    pub fn quest_query(&mut self, quest_id: u32) -> Result<()> {
        self.send(opcode::CMSG_QUEST_QUERY, &messages::quest_query(quest_id))
    }

    /// Ask an NPC's questgiver dialog status (`CMSG_QUESTGIVER_STATUS_QUERY`, layout in
    /// [`messages::questgiver_status_query`]) — the overhead `!`/`?` marker's value, answered by
    /// `SMSG_QUESTGIVER_STATUS`.
    pub fn questgiver_status_query(&mut self, npc: u64) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTGIVER_STATUS_QUERY,
            &messages::questgiver_status_query(npc),
        )
    }

    /// Abandon a quest-log slot (`CMSG_QUESTLOG_REMOVE_QUEST`, layout in
    /// [`messages::questlog_remove_quest`]) — no ack SMSG; the server clears the `PLAYER_QUEST_LOG`
    /// slot fields directly.
    pub fn questlog_remove_quest(&mut self, slot: u8) -> Result<()> {
        self.send(
            opcode::CMSG_QUESTLOG_REMOVE_QUEST,
            &messages::questlog_remove_quest(slot),
        )
    }
}
