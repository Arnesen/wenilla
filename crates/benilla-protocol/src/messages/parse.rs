//! The opcode→[`ServerPacket`] dispatch: [`parse_server`] decodes one server packet body (already
//! decrypted + sized) into its variant, delegating per-domain payloads to the domain children
//! (`spells`, `quest`, `loot`, …). One match arm per opcode — which opcode carries what stays
//! readable in one place.

use std::io::{self, Read};

use crate::wire::{
    read_cstring, read_f32_le, read_packed_guid, read_u16_le, read_u32_le, read_u64_le, read_u8,
    Vector3d,
};

use super::{
    action_bar, area_trigger, attack, auction, bank, binder, channel, chat, combat_log, death,
    duel, gameobject, gm_ticket, gossip, group, guild, items, loot, mail, mirror_timer,
    monster_move, movement, opcode, page_text, pet, petition, progression, pvp, quest, social,
    spellbook, spells, stable, taxi, trade, trainer, update_object, vendor, world_state, Character,
    CreatureQueryInfo, MoveMode, ServerPacket, SpeedKind,
};

/// Read one `SMSG_FORCE_*_SPEED_CHANGE` body — `[packed mover guid][u32 counter][f32 speed]`,
/// identical across all six kinds (VERIFIED vmangos `SendSpeedChangeToController`, the 5875
/// `> 1_9_4` branch). The kind is the opcode's; the caller's arm names it.
fn read_force_speed(kind: SpeedKind, r: &mut impl Read) -> io::Result<ServerPacket> {
    Ok(ServerPacket::ForceSpeedChange {
        guid: read_packed_guid(r)?,
        kind,
        counter: read_u32_le(r)?,
        speed: read_f32_le(r)?,
    })
}

/// Read an `SMSG_COMPRESSED_MOVES` body: `[u32 uncompressed size][deflate stream]`, the stream a
/// run of `[u8 size][u16 opcode][body]` records where `size` counts the opcode's own two bytes
/// (VERIFIED vmangos `MovementData::AddPacket`/`BuildPacket`). Each record is a whole movement
/// packet, so it goes straight back through [`parse_server`] — the batch is a *transport*, not a
/// message shape, and nothing downstream needs to know a move arrived batched.
///
/// A record that fails to parse fails the **whole** batch (surfacing as one `Poll::Skipped` naming
/// the inner opcode) rather than being dropped quietly. Losing a batch is worse than losing a
/// packet, but silence here is what cost days: an unhandled inner opcode has to be loud. Only the
/// `MSG_MOVE_*` relays vmangos routes through `ObjectViewersMovementDeliverer` can appear, and we
/// model all of them (`the_batch_carries_every_relayed_move_opcode`).
fn read_compressed_moves(r: &mut impl Read) -> io::Result<Vec<ServerPacket>> {
    let uncompressed = read_u32_le(r)? as usize;
    let mut buf = Vec::with_capacity(uncompressed.min(64 * 1024));
    flate2::read::ZlibDecoder::new(r).read_to_end(&mut buf)?;
    let mut rest = buf.as_slice();
    let mut packets = Vec::new();
    while !rest.is_empty() {
        let size = read_u8(&mut rest)? as usize;
        let opcode = read_u16_le(&mut rest)?;
        // `size` spans opcode + body; anything under two bytes cannot even hold the opcode we
        // just read, so the stream is misframed and every later record would be garbage.
        let body_len = size.checked_sub(2).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("compressed-moves record size {size} < 2"),
            )
        })?;
        if rest.len() < body_len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "compressed-moves record {opcode:#06x} wants {body_len}B, {}B left",
                    rest.len()
                ),
            ));
        }
        let (body, tail) = rest.split_at(body_len);
        rest = tail;
        // A batch inside a batch would recurse without bound; vmangos never nests one.
        if opcode == opcode::SMSG_COMPRESSED_MOVES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "compressed-moves nested inside compressed-moves",
            ));
        }
        packets.push(parse_server(opcode, body).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "compressed-moves record {opcode:#06x} ({}): {e}",
                    super::opcode_name(opcode).unwrap_or("?")
                ),
            )
        })?);
    }
    Ok(packets)
}

/// Read one `SMSG_SPLINE_SET_*_SPEED` body — `[packed guid][f32 speed]` (VERIFIED vmangos
/// `MovementPacketSender::SendSpeedChangeToAll` + the mid-spline observer branch, the 5875
/// `> 1_8_4` layout). Observer-only: no counter, no ack (decision 0441).
fn read_spline_speed(kind: SpeedKind, r: &mut impl Read) -> io::Result<ServerPacket> {
    Ok(ServerPacket::SplineSpeedChange {
        guid: read_packed_guid(r)?,
        kind,
        speed: read_f32_le(r)?,
    })
}

/// Read one `MSG_MOVE_SET_*_SPEED` body — `[packed guid][MovementInfo][f32 speed]` (VERIFIED
/// vmangos `SendSpeedChangeToObservers`, the finalized-spline branch): a freely-moving player's
/// speed change, carrying a fresh pose alongside (decision 0441). No ack.
fn read_move_set_speed(kind: SpeedKind, r: &mut impl Read) -> io::Result<ServerPacket> {
    let guid = read_packed_guid(r)?;
    let info = movement::read_movement_info(r)?;
    Ok(ServerPacket::MoveSetSpeed {
        guid,
        kind,
        flags: info.flags,
        position: info.position,
        orientation: info.orientation,
        pitch: info.pitch,
        time: info.timestamp,
        fall_time: info.fall_time,
        jump: info.jump,
        transport: info.transport,
        speed: read_f32_le(r)?,
    })
}

/// True for a relayed player-movement opcode — one the server rebroadcasts as
/// `[packed guid][MovementInfo]` (every opcode bound to vmangos `HandleMovementOpcodes`, VERIFIED
/// `Opcodes.cpp`). Excludes `MSG_MOVE_TELEPORT_ACK` / `MSG_MOVE_WORLDPORT_ACK`, which share the family
/// but carry different bodies and are decoded by their own arms.
const fn is_movement_relay(o: u16) -> bool {
    matches!(
        o,
        opcode::MSG_MOVE_START_FORWARD
            | opcode::MSG_MOVE_START_BACKWARD
            | opcode::MSG_MOVE_STOP
            | opcode::MSG_MOVE_START_STRAFE_LEFT
            | opcode::MSG_MOVE_START_STRAFE_RIGHT
            | opcode::MSG_MOVE_STOP_STRAFE
            | opcode::MSG_MOVE_JUMP
            | opcode::MSG_MOVE_START_TURN_LEFT
            | opcode::MSG_MOVE_START_TURN_RIGHT
            | opcode::MSG_MOVE_STOP_TURN
            | opcode::MSG_MOVE_START_PITCH_UP
            | opcode::MSG_MOVE_START_PITCH_DOWN
            | opcode::MSG_MOVE_STOP_PITCH
            | opcode::MSG_MOVE_SET_RUN_MODE
            | opcode::MSG_MOVE_SET_WALK_MODE
            | opcode::MSG_MOVE_FALL_LAND
            | opcode::MSG_MOVE_START_SWIM
            | opcode::MSG_MOVE_STOP_SWIM
            | opcode::MSG_MOVE_SET_FACING
            | opcode::MSG_MOVE_SET_PITCH
            | opcode::MSG_MOVE_HEARTBEAT
    )
}

/// Parse a server packet body (already decrypted + sized) by opcode.
pub fn parse_server(opcode: u16, body: &[u8]) -> io::Result<ServerPacket> {
    let mut r = body;
    Ok(match opcode {
        opcode::SMSG_AUTH_CHALLENGE => ServerPacket::AuthChallenge {
            server_seed: read_u32_le(&mut r)?,
        },
        opcode::SMSG_AUTH_RESPONSE => {
            // **The client's own grammar** (VERIFIED, handler `0x5b41b0`; wow-re
            // `system/glue/scratch/login-failure-dialogs.md`):
            //
            //   u8 code
            //   if (code == AUTH_OK || code == AUTH_WAIT_QUEUE) && remaining >= 5:
            //       u32 billingTimeRemaining, u8 billingPlanFlags, u32 billingTimeRested
            //   if code == AUTH_WAIT_QUEUE:
            //       u32 position
            //
            // The client **branches on the remaining length**, which is what makes both shapes the
            // mangos family sends parse correctly: the short `{u8, u32}` leaves 4 bytes (`4 < 5`,
            // billing skipped, position at body offset 1) and the long
            // `{u8, u32, u8, u32, u32}` leaves 13 (billing read, position at offset 10).
            //
            // The threshold is **5 guarding a 9-byte group** — not a size check, just "is there
            // more here than a bare position?". Copied rather than tidied: a body of 6..=9 bytes
            // hits it and mis-parses, and matching the client's mistakes is the point of a
            // faithful parser.
            //
            // ONE DELIBERATE DIVERGENCE: where a truncated body leaves the client's position
            // *unwritten* — silently redisplaying the previous queue position from an
            // uninitialised process-lifetime global — ours reports `None`. Rendering a stale
            // position as current is a bug we decline to reproduce.
            let result = read_u8(&mut r)?;
            let queued = result == super::AUTH_WAIT_QUEUE;
            if (result == super::AUTH_OK || queued) && r.len() >= 5 {
                let _billing_time_remaining = read_u32_le(&mut r)?;
                let _billing_plan_flags = read_u8(&mut r)?;
                let _billing_time_rested = read_u32_le(&mut r)?;
            }
            ServerPacket::AuthResponse {
                result,
                queue_position: queued.then(|| read_u32_le(&mut r).ok()).flatten(),
            }
        }
        opcode::SMSG_CHAR_ENUM => {
            let count = read_u8(&mut r)?;
            let mut characters = Vec::with_capacity(count as usize);
            for _ in 0..count {
                characters.push(Character::read(&mut r)?);
            }
            ServerPacket::CharEnum { characters }
        }
        opcode::SMSG_CHAR_DELETE => ServerPacket::CharDelete {
            result: read_u8(&mut r)?,
        },
        opcode::SMSG_CHAR_CREATE => ServerPacket::CharCreate {
            result: read_u8(&mut r)?,
        },
        opcode::SMSG_UPDATE_OBJECT => ServerPacket::UpdateObject {
            objects: update_object::read_update_object(&mut r)?,
        },
        opcode::SMSG_DESTROY_OBJECT => ServerPacket::DestroyObject {
            guid: read_u64_le(&mut r)?,
        },
        opcode::SMSG_TRIGGER_CINEMATIC => ServerPacket::TriggerCinematic {
            cinematic_id: read_u32_le(&mut r)?,
        },
        opcode::SMSG_COMPRESSED_UPDATE_OBJECT => {
            let _decompressed_size = read_u32_le(&mut r)?;
            let mut decoder = flate2::read::ZlibDecoder::new(r);
            let mut decompressed = Vec::new();
            decoder.read_to_end(&mut decompressed)?;
            let mut dr = decompressed.as_slice();
            ServerPacket::UpdateObject {
                objects: update_object::read_update_object(&mut dr)?,
            }
        }
        opcode::SMSG_COMPRESSED_MOVES => ServerPacket::CompressedMoves {
            packets: read_compressed_moves(&mut r)?,
        },
        opcode::SMSG_MONSTER_MOVE => monster_move::read_monster_move(&mut r)?,
        opcode::MSG_MOVE_TELEPORT_ACK => {
            let guid = read_packed_guid(&mut r)?;
            let counter = read_u32_le(&mut r)?;
            let info = movement::read_movement_info(&mut r)?;
            ServerPacket::Teleport {
                guid,
                counter,
                position: info.position,
                orientation: info.orientation,
            }
        }
        opcode::SMSG_NEW_WORLD => {
            let map = read_u32_le(&mut r)?;
            let position = Vector3d::read(&mut r)?;
            let orientation = read_f32_le(&mut r)?;
            ServerPacket::NewWorld {
                map,
                position,
                orientation,
            }
        }
        opcode::SMSG_LOGIN_VERIFY_WORLD => {
            let map = read_u32_le(&mut r)?;
            let position = Vector3d::read(&mut r)?;
            let orientation = read_f32_le(&mut r)?;
            ServerPacket::LoginVerifyWorld {
                map,
                position,
                orientation,
            }
        }
        opcode::SMSG_TRANSFER_PENDING => {
            // `u32 newMapId` +, iff the player rides a transport through the transfer,
            // `{u32 transportEntry, u32 oldMapId}` (VERIFIED vmangos `Misc.cpp:493-501`). The
            // block's PRESENCE is load-bearing: it decides whether the follow-up NEW_WORLD's
            // coordinates are boat-local or world (decision 0455).
            let map = read_u32_le(&mut r)?;
            let transport = if r.is_empty() {
                None
            } else {
                let entry = read_u32_le(&mut r)?;
                let old_map = read_u32_le(&mut r)?;
                Some((entry, old_map))
            };
            ServerPacket::TransferPending { map, transport }
        }
        opcode::SMSG_TRANSFER_ABORTED => ServerPacket::TransferAborted {
            reason: read_u8(&mut r)?,
        },
        opcode::SMSG_LOGIN_SETTIMESPEED => {
            // The vanilla packed DateTime bit layout (LSB up): minute:6, hour:5, weekday:3,
            // day-of-month:6, month:4, year:5. The day serial flattens it with the packed
            // convention's fixed 31-day months / 372-day years — monotonic across the dates a
            // server actually serves, which is all the celestial moon-phase precession needs.
            let datetime = read_u32_le(&mut r)?;
            let timescale = read_f32_le(&mut r)?;
            let (day, month, year) = (
                (datetime >> 14) & 0x3F,
                (datetime >> 20) & 0x0F,
                (datetime >> 24) & 0x1F,
            );
            ServerPacket::TimeSpeed {
                hours: ((datetime >> 6) & 0x1F) as u8,
                minutes: (datetime & 0x3F) as u8,
                day_serial: year * 372 + month * 31 + day,
                timescale,
            }
        }
        // The OTHER clock (decision 1150) — kept beside SETTIMESPEED precisely because they are
        // easy to confuse: this one is wall-clock UNIX seconds (vmangos
        // `WorldSession::SendQueryTimeResponse`, `Handlers/QueryHandler.cpp:418-423` — a bare
        // `(uint32)time(nullptr)`), the epoch every absolute stamp the server writes into a
        // descriptor field is expressed in. SETTIMESPEED above is the in-game day/night clock and
        // says nothing about it.
        opcode::SMSG_QUERY_TIME_RESPONSE => ServerPacket::QueryTimeResponse {
            unix_time: read_u32_le(&mut r)?,
        },
        opcode::SMSG_BINDPOINTUPDATE => {
            // vmangos BindpointUpdate::AppendBodyTo (Packets/Misc.cpp): x, y, z, mapId, areaId.
            let position = Vector3d::read(&mut r)?;
            let map = read_u32_le(&mut r)?;
            let area = read_u32_le(&mut r)?;
            ServerPacket::BindPoint {
                position,
                map,
                area,
            }
        }
        // The GM trouble-ticket answers (decision 1673). The three response opcodes share one
        // 4-byte body and therefore one reader; only the opcode says which verb was answered.
        opcode::SMSG_GMTICKET_GETTICKET => ServerPacket::GmTicketAnswer {
            ticket: gm_ticket::read_gm_ticket(&mut r)?.map(Box::new),
        },
        opcode::SMSG_GMTICKET_CREATE => ServerPacket::GmTicketCreated {
            response: gm_ticket::read_gm_ticket_response(&mut r)?,
        },
        opcode::SMSG_GMTICKET_UPDATETEXT => ServerPacket::GmTicketUpdated {
            response: gm_ticket::read_gm_ticket_response(&mut r)?,
        },
        opcode::SMSG_GMTICKET_DELETETICKET => ServerPacket::GmTicketDeleted {
            response: gm_ticket::read_gm_ticket_response(&mut r)?,
        },
        opcode::SMSG_GMTICKET_SYSTEMSTATUS => ServerPacket::GmTicketSystemStatus {
            status: gm_ticket::read_gm_ticket_system_status(&mut r)?,
        },
        opcode::SMSG_GM_TICKET_STATUS_UPDATE => ServerPacket::GmTicketStatusUpdate {
            status: gm_ticket::read_gm_ticket_response(&mut r)?,
        },
        opcode::SMSG_BINDER_CONFIRM => ServerPacket::BinderConfirm {
            binder: binder::read_binder_confirm(&mut r)?,
        },
        // The talent twin of the confirm above, on a two-way `MSG_` opcode: this direction is the
        // question (guid + cost); the answer we send back carries the guid alone (decision 1580).
        opcode::MSG_TALENT_WIPE_CONFIRM => {
            let ask = progression::read_talent_wipe_confirm(&mut r)?;
            ServerPacket::TalentWipeConfirm {
                trainer: ask.trainer,
                cost: ask.cost,
            }
        }
        opcode::SMSG_PLAYERBOUND => {
            let bound = binder::read_player_bound(&mut r)?;
            ServerPacket::PlayerBound {
                binder: bound.binder,
                area: bound.area,
            }
        }
        opcode::SMSG_SET_PROFICIENCY => {
            // vmangos SetProficiency::AppendBodyTo (Packets/Skill.cpp): u8 itemClass + u32 mask.
            let item_class = read_u8(&mut r)?;
            let subclass_mask = read_u32_le(&mut r)?;
            ServerPacket::SetProficiency {
                item_class,
                subclass_mask,
            }
        }
        opcode::SMSG_PLAY_SOUND => ServerPacket::PlaySound {
            sound_id: read_u32_le(&mut r)?,
        },
        opcode::SMSG_PLAY_MUSIC => ServerPacket::PlayMusic {
            music_id: read_u32_le(&mut r)?,
        },
        opcode::SMSG_PLAY_OBJECT_SOUND => ServerPacket::PlayObjectSound {
            sound_id: read_u32_le(&mut r)?,
            guid: read_u64_le(&mut r)?,
        },
        opcode::SMSG_WEATHER => ServerPacket::Weather {
            weather_type: read_u32_le(&mut r)?,
            grade: read_f32_le(&mut r)?,
            sound_id: read_u32_le(&mut r)?,
            instant: read_u8(&mut r)? != 0,
        },
        opcode::SMSG_TEXT_EMOTE => {
            let guid = read_u64_le(&mut r)?;
            let text_emote = read_u32_le(&mut r)?;
            // Read and dropped on purpose — `emoteNum` selects neither the sentence nor the voice
            // kit on the receive side (wow-re `ui/scratch/text-emote-composition.md` §3).
            let _emote_num = read_u32_le(&mut r)?;
            // The target's name, length-prefixed **including its NUL** (vmangos writes a lone
            // `0x00` and `namelen == 1` when there was no target). Trimmed at the first NUL, so an
            // untargeted emote arrives as the empty string — which is precisely the "untargeted"
            // bit of the sentence-form selector, not a missing value.
            let namelen = read_u32_le(&mut r)? as usize;
            let mut name = Vec::with_capacity(namelen.min(64));
            for _ in 0..namelen {
                name.push(read_u8(&mut r)?);
            }
            let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
            let target_name = String::from_utf8_lossy(&name[..end]).into_owned();
            ServerPacket::TextEmote {
                guid,
                text_emote,
                target_name,
            }
        }
        opcode::SMSG_EMOTE => {
            let emote_id = read_u32_le(&mut r)?;
            let guid = read_u64_le(&mut r)?;
            ServerPacket::Emote { guid, emote_id }
        }
        opcode::SMSG_ITEM_QUERY_SINGLE_RESPONSE => {
            let (entry, info) = items::read_item_query_response(&mut r)?;
            ServerPacket::ItemQueryResponse {
                entry,
                info: info.map(Box::new),
            }
        }
        opcode::SMSG_MESSAGECHAT => ServerPacket::MessageChat(chat::read_message_chat(&mut r)?),
        opcode::SMSG_CHANNEL_NOTIFY => {
            ServerPacket::ChannelNotify(channel::read_channel_notify(&mut r)?)
        }
        opcode::SMSG_CHANNEL_LIST => {
            let (name, flags, members) = channel::read_channel_list(&mut r)?;
            ServerPacket::ChannelList {
                channel: name,
                flags,
                members,
            }
        }
        opcode::SMSG_CHAT_PLAYER_NOT_FOUND => ServerPacket::ChatPlayerNotFound {
            name: chat::read_chat_player_not_found(&mut r)?,
        },
        opcode::SMSG_CHAT_WRONG_FACTION => ServerPacket::ChatWrongFaction,
        opcode::SMSG_NOTIFICATION => ServerPacket::Notification {
            text: chat::read_notification(&mut r)?,
        },
        opcode::SMSG_AREA_TRIGGER_MESSAGE => ServerPacket::AreaTriggerMessage {
            text: area_trigger::read_area_trigger_message(&mut r)?,
        },
        opcode::SMSG_PLAYED_TIME => {
            let (total, level) = chat::read_played_time(&mut r)?;
            ServerPacket::PlayedTime { total, level }
        }
        opcode::MSG_RANDOM_ROLL => {
            let (min, max, roll, guid) = chat::read_random_roll(&mut r)?;
            ServerPacket::RandomRoll {
                min,
                max,
                roll,
                guid,
            }
        }
        opcode::SMSG_INVENTORY_CHANGE_FAILURE => {
            let (reason, required_level, item_guid, bag_slot) =
                items::read_inventory_change_failure(&mut r)?;
            ServerPacket::InventoryChangeFailure {
                reason,
                required_level,
                item_guid,
                bag_slot,
            }
        }
        opcode::SMSG_INITIAL_SPELLS => {
            let (spell_ids, cooldowns) = spellbook::read_initial_spells(&mut r)?;
            ServerPacket::InitialSpells {
                spell_ids,
                cooldowns,
            }
        }
        opcode::SMSG_ACTION_BUTTONS => ServerPacket::ActionButtons {
            buttons: action_bar::read_action_buttons(&mut r)?,
        },
        opcode::SMSG_LEARNED_SPELL => ServerPacket::LearnedSpell {
            spell_id: spellbook::read_learned_spell(&mut r)?,
        },
        opcode::SMSG_REMOVED_SPELL => ServerPacket::RemovedSpell {
            spell_id: spellbook::read_removed_spell(&mut r)?,
        },
        opcode::SMSG_SUPERCEDED_SPELL => {
            let (old_spell_id, new_spell_id) = spellbook::read_superceded_spell(&mut r)?;
            ServerPacket::SupercededSpell {
                old_spell_id,
                new_spell_id,
            }
        }
        opcode::SMSG_CAST_RESULT => {
            let (spell_id, outcome) = spells::read_cast_result(&mut r)?;
            ServerPacket::CastResult { spell_id, outcome }
        }
        opcode::SMSG_PET_SPELLS => ServerPacket::PetSpells(pet::read_pet_spells(&mut r)?),
        opcode::SMSG_PET_MODE => ServerPacket::PetMode(pet::read_pet_mode(&mut r)?),
        opcode::SMSG_PET_ACTION_FEEDBACK => ServerPacket::PetActionFeedback {
            reason: pet::read_pet_action_feedback(&mut r)?,
        },
        opcode::SMSG_PET_CAST_FAILED => {
            let (spell_id, outcome) = pet::read_pet_cast_failed(&mut r)?;
            ServerPacket::PetCastFailed { spell_id, outcome }
        }
        opcode::SMSG_ATTACKSTART => {
            let (attacker, victim) = attack::read_attack_start(&mut r)?;
            ServerPacket::AttackStart { attacker, victim }
        }
        opcode::SMSG_ATTACKSTOP => {
            let (attacker, victim) = attack::read_attack_stop(&mut r)?;
            ServerPacket::AttackStop { attacker, victim }
        }
        opcode::SMSG_ATTACKERSTATEUPDATE => {
            ServerPacket::AttackerState(attack::read_attacker_state(&mut r)?)
        }
        opcode::SMSG_AI_REACTION => {
            let (unit, reaction) = attack::read_ai_reaction(&mut r)?;
            ServerPacket::AiReaction { unit, reaction }
        }
        opcode::SMSG_SPELL_START => ServerPacket::SpellStart(spells::read_spell_start(&mut r)?),
        opcode::SMSG_SPELL_GO => ServerPacket::SpellGo(spells::read_spell_go(&mut r)?),
        opcode::SMSG_SPELL_UPDATE_CHAIN_TARGETS => {
            ServerPacket::SpellChainTargets(spells::read_spell_chain_targets(&mut r)?)
        }
        opcode::SMSG_SPELL_FAILED_OTHER => {
            let (caster, spell_id) = spells::read_spell_failed_other(&mut r)?;
            ServerPacket::SpellFailedOther { caster, spell_id }
        }
        opcode::SMSG_SPELL_DELAYED => {
            let (caster, delay_ms) = spells::read_spell_delayed(&mut r)?;
            ServerPacket::SpellDelayed { caster, delay_ms }
        }
        opcode::SMSG_CANCEL_AUTO_REPEAT => ServerPacket::CancelAutoRepeat,
        opcode::SMSG_SPELL_COOLDOWN => {
            let (caster, cooldowns) = spellbook::read_spell_cooldown(&mut r)?;
            ServerPacket::SpellCooldownList { caster, cooldowns }
        }
        opcode::SMSG_ITEM_COOLDOWN => {
            let (item_guid, spell_id) = spellbook::read_item_cooldown(&mut r)?;
            ServerPacket::ItemCooldown {
                item_guid,
                spell_id,
            }
        }
        opcode::SMSG_ITEM_ENCHANT_TIME_UPDATE => {
            let (item_guid, slot, seconds) = items::read_item_enchant_time(&mut r)?;
            ServerPacket::ItemEnchantTime {
                item_guid,
                slot,
                seconds,
            }
        }
        opcode::SMSG_COOLDOWN_EVENT => {
            let (spell_id, caster) = spellbook::read_cooldown_event(&mut r)?;
            ServerPacket::CooldownEvent { spell_id, caster }
        }
        opcode::SMSG_CLEAR_COOLDOWN => {
            let (spell_id, caster) = spellbook::read_cooldown_event(&mut r)?;
            ServerPacket::ClearCooldown { spell_id, caster }
        }
        opcode::SMSG_COOLDOWN_CHEAT => ServerPacket::CooldownCheat {
            caster: spellbook::read_cooldown_cheat(&mut r)?,
        },
        opcode::MSG_CHANNEL_START => {
            let (spell_id, duration_ms) = spells::read_channel_start(&mut r)?;
            ServerPacket::ChannelStart {
                spell_id,
                duration_ms,
            }
        }
        opcode::MSG_CHANNEL_UPDATE => ServerPacket::ChannelUpdate {
            remaining_ms: spells::read_channel_update(&mut r)?,
        },
        opcode::SMSG_UPDATE_AURA_DURATION => {
            let (slot, remaining_ms) = spells::read_update_aura_duration(&mut r)?;
            ServerPacket::UpdateAuraDuration { slot, remaining_ms }
        }
        opcode::SMSG_PLAY_SPELL_VISUAL => {
            let (unit, kit_id) = spells::read_play_spell_visual(&mut r)?;
            ServerPacket::PlaySpellVisual { unit, kit_id }
        }
        opcode::SMSG_SPELLNONMELEEDAMAGELOG => {
            ServerPacket::SpellDamageLog(combat_log::read_spell_damage_log(&mut r)?)
        }
        opcode::SMSG_PERIODICAURALOG => {
            ServerPacket::PeriodicAuraLog(combat_log::read_periodic_aura_log(&mut r)?)
        }
        opcode::SMSG_SPELLDAMAGESHIELD => {
            ServerPacket::DamageShield(combat_log::read_damage_shield(&mut r)?)
        }
        opcode::SMSG_SPELLHEALLOG => {
            ServerPacket::SpellHealLog(combat_log::read_spell_heal_log(&mut r)?)
        }
        opcode::SMSG_SPELLENERGIZELOG => {
            ServerPacket::SpellEnergizeLog(combat_log::read_spell_energize_log(&mut r)?)
        }
        opcode::SMSG_ENVIRONMENTALDAMAGELOG => {
            ServerPacket::EnvironmentalDamageLog(combat_log::read_environmental_damage_log(&mut r)?)
        }
        opcode::SMSG_SPELLLOGMISS => {
            ServerPacket::SpellLogMiss(combat_log::read_spell_log_miss(&mut r)?)
        }
        opcode::SMSG_LOG_XPGAIN => ServerPacket::XpGain(progression::read_xp_gain(&mut r)?),
        opcode::SMSG_EXPLORATION_EXPERIENCE => {
            ServerPacket::ExplorationXp(progression::read_exploration_xp(&mut r)?)
        }
        opcode::SMSG_LEVELUP_INFO => {
            ServerPacket::LevelUp(progression::read_level_up_info(&mut r)?)
        }
        opcode::SMSG_QUESTGIVER_STATUS => {
            let (npc, status) = quest::read_questgiver_status(&mut r)?;
            ServerPacket::QuestGiverStatus { npc, status }
        }
        opcode::SMSG_QUESTGIVER_QUEST_LIST => {
            ServerPacket::QuestGiverQuestList(quest::read_questgiver_quest_list(&mut r)?)
        }
        opcode::SMSG_QUESTGIVER_QUEST_DETAILS => {
            ServerPacket::QuestGiverDetails(quest::read_questgiver_quest_details(&mut r)?)
        }
        opcode::SMSG_QUESTGIVER_REQUEST_ITEMS => {
            ServerPacket::QuestGiverRequestItems(quest::read_questgiver_request_items(&mut r)?)
        }
        opcode::SMSG_QUESTGIVER_OFFER_REWARD => {
            ServerPacket::QuestGiverOfferReward(quest::read_questgiver_offer_reward(&mut r)?)
        }
        opcode::SMSG_QUESTGIVER_QUEST_COMPLETE => {
            ServerPacket::QuestGiverComplete(quest::read_questgiver_quest_complete(&mut r)?)
        }
        opcode::SMSG_QUESTGIVER_QUEST_INVALID => ServerPacket::QuestGiverInvalid {
            msg: quest::read_questgiver_quest_invalid(&mut r)?,
        },
        opcode::SMSG_QUESTGIVER_QUEST_FAILED => {
            let (quest_id, reason) = quest::read_questgiver_quest_failed(&mut r)?;
            ServerPacket::QuestGiverFailed { quest_id, reason }
        }
        opcode::SMSG_QUEST_QUERY_RESPONSE => {
            ServerPacket::QuestQueryResponse(Box::new(quest::read_quest_query_response(&mut r)?))
        }
        opcode::SMSG_QUESTLOG_FULL => ServerPacket::QuestLogFull,
        opcode::SMSG_QUESTUPDATE_COMPLETE => ServerPacket::QuestUpdateComplete {
            quest_id: quest::read_quest_update_complete(&mut r)?,
        },
        opcode::SMSG_QUESTUPDATE_FAILED => ServerPacket::QuestUpdateFailed {
            quest_id: quest::read_quest_update_failed(&mut r)?,
        },
        opcode::SMSG_QUESTUPDATE_FAILEDTIMER => ServerPacket::QuestUpdateFailedTimer {
            quest_id: quest::read_quest_update_failedtimer(&mut r)?,
        },
        opcode::SMSG_QUESTUPDATE_ADD_KILL => {
            let (quest_id, entry, count, required, guid) =
                quest::read_quest_update_add_kill(&mut r)?;
            ServerPacket::QuestUpdateAddKill {
                quest_id,
                entry,
                count,
                required,
                guid,
            }
        }
        opcode::SMSG_QUESTUPDATE_ADD_ITEM => {
            let (item_id, count) = quest::read_quest_update_add_item(&mut r)?;
            ServerPacket::QuestUpdateAddItem { item_id, count }
        }
        opcode::SMSG_GOSSIP_MESSAGE => {
            let (npc, text_id, options, quests) = gossip::read_gossip_message(&mut r)?;
            ServerPacket::GossipMessage {
                npc,
                text_id,
                options,
                quests,
            }
        }
        opcode::SMSG_GOSSIP_COMPLETE => ServerPacket::GossipComplete,
        opcode::SMSG_GOSSIP_POI => ServerPacket::GossipPoi(gossip::read_gossip_poi(&mut r)?),
        opcode::SMSG_NPC_TEXT_UPDATE => {
            let (text_id, blocks) = gossip::read_npc_text_update(&mut r)?;
            ServerPacket::NpcText { text_id, blocks }
        }
        opcode::SMSG_LIST_INVENTORY => {
            let (vendor, items) = vendor::read_list_inventory(&mut r)?;
            ServerPacket::VendorList { vendor, items }
        }
        opcode::SMSG_BUY_ITEM => {
            let (vendor, slot, new_count, purchase_count) = vendor::read_buy_item(&mut r)?;
            ServerPacket::BuyItem {
                vendor,
                slot,
                new_count,
                purchase_count,
            }
        }
        opcode::SMSG_SELL_ITEM => {
            let (vendor, item_guid, reason) = vendor::read_sell_item(&mut r)?;
            ServerPacket::SellItemResult {
                vendor,
                item_guid,
                reason,
            }
        }
        opcode::SMSG_BUY_FAILED => {
            let (vendor, item_entry, reason) = vendor::read_buy_failed(&mut r)?;
            ServerPacket::BuyFailed {
                vendor,
                item_entry,
                reason,
            }
        }
        opcode::SMSG_SHOW_BANK => {
            let banker = bank::read_show_bank(&mut r)?;
            ServerPacket::ShowBank { banker }
        }
        opcode::SMSG_BUY_BANK_SLOT_RESULT => {
            let result = bank::read_buy_bank_slot_result(&mut r)?;
            ServerPacket::BuyBankSlotResult { result }
        }
        opcode::SMSG_TRAINER_LIST => {
            let (trainer, trainer_type, services, title) = trainer::read_trainer_list(&mut r)?;
            ServerPacket::TrainerList {
                trainer,
                trainer_type,
                services,
                title,
            }
        }
        opcode::SMSG_TRAINER_BUY_SUCCEEDED => {
            let (trainer, spell_id) = trainer::read_trainer_buy_succeeded(&mut r)?;
            ServerPacket::TrainerBuySucceeded { trainer, spell_id }
        }
        opcode::SMSG_TRAINER_BUY_FAILED => {
            let (trainer, spell_id, error) = trainer::read_trainer_buy_failed(&mut r)?;
            ServerPacket::TrainerBuyFailed {
                trainer,
                spell_id,
                error,
            }
        }
        opcode::MSG_LIST_STABLED_PETS => {
            let (npc, num_stable_slots, pets) = stable::read_list_stabled_pets(&mut r)?;
            ServerPacket::ListStabledPets {
                npc,
                num_stable_slots,
                pets,
            }
        }
        opcode::SMSG_STABLE_RESULT => {
            let result = stable::read_stable_result(&mut r)?;
            ServerPacket::StableResult { result }
        }
        opcode::SMSG_LOOT_RESPONSE => {
            let (guid, body) = loot::read_loot_response(&mut r)?;
            match body {
                loot::LootResponseBody::Items {
                    loot_type,
                    gold,
                    items,
                } => ServerPacket::LootResponse {
                    guid,
                    loot_type,
                    gold,
                    items,
                },
                loot::LootResponseBody::Error { error } => ServerPacket::LootError { guid, error },
            }
        }
        opcode::SMSG_LOOT_RELEASE_RESPONSE => {
            let (guid, result) = loot::read_loot_release_response(&mut r)?;
            ServerPacket::LootReleaseResponse { guid, result }
        }
        opcode::SMSG_LOOT_REMOVED => ServerPacket::LootRemoved {
            slot: loot::read_loot_removed(&mut r)?,
        },
        opcode::SMSG_LOOT_MONEY_NOTIFY => ServerPacket::LootMoneyNotify {
            amount: loot::read_loot_money_notify(&mut r)?,
        },
        opcode::SMSG_LOOT_CLEAR_MONEY => ServerPacket::LootClearMoney,
        opcode::SMSG_LOOT_START_ROLL => {
            ServerPacket::LootStartRoll(loot::read_loot_start_roll(&mut r)?)
        }
        opcode::SMSG_LOOT_ROLL => ServerPacket::LootRoll(loot::read_loot_roll(&mut r)?),
        opcode::SMSG_LOOT_ROLL_WON => ServerPacket::LootRollWon(loot::read_loot_roll_won(&mut r)?),
        opcode::SMSG_LOOT_ALL_PASSED => {
            ServerPacket::LootAllPassed(loot::read_loot_all_passed(&mut r)?)
        }
        opcode::SMSG_LOOT_MASTER_LIST => ServerPacket::LootMasterList {
            candidates: loot::read_loot_master_list(&mut r)?,
        },
        opcode::SMSG_ITEM_PUSH_RESULT => {
            ServerPacket::ItemPushResult(loot::read_item_push_result(&mut r)?)
        }
        opcode::MSG_CORPSE_QUERY => {
            ServerPacket::CorpseQuery(death::read_corpse_query_response(&mut r)?)
        }
        opcode::SMSG_DURABILITY_DAMAGE_DEATH => ServerPacket::DurabilityDamageDeath,
        opcode::SMSG_CORPSE_RECLAIM_DELAY => ServerPacket::CorpseReclaimDelay {
            delay_ms: death::read_corpse_reclaim_delay(&mut r)?,
        },
        opcode::SMSG_RESURRECT_REQUEST => {
            ServerPacket::ResurrectRequest(death::read_resurrect_request(&mut r)?)
        }
        opcode::SMSG_SPIRIT_HEALER_CONFIRM => ServerPacket::SpiritHealerConfirm {
            npc: death::read_spirit_healer_confirm(&mut r)?,
        },
        // **The ack'd movement-mode family** (decision 0866) — root, water-walk, feather-fall and
        // hover, granted to the controlling client. ONE wire shape for all eight opcodes:
        // `packed guid + u32 counter` (VERIFIED vmangos `MovementPacketSender.cpp:342-366`, the
        // `> CLIENT_BUILD_1_9_4` branch). The app must ack with the echoed counter, or the server
        // never applies the change and observers never see it.
        opcode::SMSG_FORCE_MOVE_ROOT
        | opcode::SMSG_FORCE_MOVE_UNROOT
        | opcode::SMSG_MOVE_WATER_WALK
        | opcode::SMSG_MOVE_LAND_WALK
        | opcode::SMSG_MOVE_FEATHER_FALL
        | opcode::SMSG_MOVE_NORMAL_FALL
        | opcode::SMSG_MOVE_SET_HOVER
        | opcode::SMSG_MOVE_UNSET_HOVER => {
            let (mode, apply) = match opcode {
                opcode::SMSG_FORCE_MOVE_ROOT => (MoveMode::Root, true),
                opcode::SMSG_FORCE_MOVE_UNROOT => (MoveMode::Root, false),
                opcode::SMSG_MOVE_WATER_WALK => (MoveMode::WaterWalk, true),
                opcode::SMSG_MOVE_LAND_WALK => (MoveMode::WaterWalk, false),
                opcode::SMSG_MOVE_FEATHER_FALL => (MoveMode::FeatherFall, true),
                opcode::SMSG_MOVE_NORMAL_FALL => (MoveMode::FeatherFall, false),
                opcode::SMSG_MOVE_SET_HOVER => (MoveMode::Hover, true),
                _ => (MoveMode::Hover, false),
            };
            let guid = read_packed_guid(&mut r)?;
            let counter = read_u32_le(&mut r)?;
            ServerPacket::MoveMode {
                guid,
                counter,
                mode,
                apply,
            }
        }
        opcode::SMSG_INITIALIZE_FACTIONS => {
            let count = read_u32_le(&mut r)?;
            let mut standings = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let flags = read_u8(&mut r)?;
                let standing = read_u32_le(&mut r)? as i32;
                standings.push((flags, standing));
            }
            ServerPacket::InitializeFactions { standings }
        }
        opcode::SMSG_SET_FACTION_STANDING => {
            let count = read_u32_le(&mut r)?;
            let mut standings = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let list_id = read_u32_le(&mut r)?;
                let standing = read_u32_le(&mut r)? as i32;
                standings.push((list_id, standing));
            }
            ServerPacket::SetFactionStanding { standings }
        }
        opcode::SMSG_SET_FACTION_VISIBLE => ServerPacket::SetFactionVisible {
            list_id: read_u32_le(&mut r)?,
        },
        opcode::SMSG_NAME_QUERY_RESPONSE => {
            let guid = read_u64_le(&mut r)?;
            let name = read_cstring(&mut r)?;
            let _realm = read_cstring(&mut r)?; // cross-realm BG name; empty on a single realm
            ServerPacket::NameQueryResponse {
                guid,
                name,
                race: read_u32_le(&mut r)?,
                gender: read_u32_le(&mut r)?,
                class: read_u32_le(&mut r)?,
            }
        }
        opcode::SMSG_CREATURE_QUERY_RESPONSE => {
            let entry = read_u32_le(&mut r)?;
            // A miss is the lone entry echoed with its top bit set — nothing follows.
            if entry & 0x8000_0000 != 0 {
                ServerPacket::CreatureQueryResponse {
                    entry: entry & 0x7FFF_FFFF,
                    info: None,
                }
            } else {
                let name = read_cstring(&mut r)?;
                for _ in 0..3 {
                    let _ = read_cstring(&mut r)?; // name2..name4, always empty in 5875
                }
                let subname = read_cstring(&mut r)?;
                // The tail (VERIFIED vmangos `HandleCreatureQueryOpcode`, 5875 = every build
                // guard included): type_flags, type, pet_family, RANK, unk, pet_spell_list_id,
                // display_id (u32 ×7), then civilian + racial_leader (u8 ×2). We keep `type`
                // (the `CreatureType.dbc` id — the TAB-target filter's input), `pet_family` (the
                // `CreatureFamily.dbc` id behind `UnitCreatureFamily` and the diet tooltip,
                // decision 1062), `rank` (the unit tooltip's Elite/Boss word, decision 0276's
                // level-line law), `type_flags` (bit 0x10 hides its faction line), the
                // `civilian`/`racial_leader` pair (its green CIVILIAN / white LEADER lines), and
                // `display_id` — the template's model, which is the ONLY way to draw a creature
                // that has no world object to read `UNIT_FIELD_DISPLAYID` off: a stabled pet
                // (decision 1676). The `unk`/`pet_spell_list_id` pair stays alignment-only.
                let type_flags = read_u32_le(&mut r)?;
                let creature_type = read_u32_le(&mut r)?;
                let pet_family = read_u32_le(&mut r)?;
                let rank = read_u32_le(&mut r)?;
                let _unk = read_u32_le(&mut r)?;
                let _pet_spell_list = read_u32_le(&mut r)?;
                let display_id = read_u32_le(&mut r)?;
                let civilian = read_u8(&mut r)? != 0;
                let racial_leader = read_u8(&mut r)? != 0;
                ServerPacket::CreatureQueryResponse {
                    entry,
                    info: Some(CreatureQueryInfo {
                        name,
                        subname,
                        creature_type,
                        pet_family,
                        rank,
                        type_flags,
                        display_id,
                        civilian,
                        racial_leader,
                    }),
                }
            }
        }
        // `u32 petNumber`, cstring name, `u32 nameTimestamp` (VERIFIED vmangos
        // `PetNameQueryResponse::AppendBodyTo`, `Server/Packets/Pet.cpp:79-84`). The timestamp ages
        // out the reference's on-disk pet-name cache; we keep none, so it is alignment-only.
        opcode::SMSG_PET_NAME_QUERY_RESPONSE => {
            let pet_number = read_u32_le(&mut r)?;
            let name = read_cstring(&mut r)?;
            let _name_timestamp = read_u32_le(&mut r)?;
            ServerPacket::PetNameQueryResponse { pet_number, name }
        }
        opcode::SMSG_GAMEOBJECT_QUERY_RESPONSE => {
            let (entry, info) = gameobject::read_gameobject_query_response(&mut r)?;
            ServerPacket::GameObjectQueryResponse { entry, info }
        }
        opcode::SMSG_PAGE_TEXT_QUERY_RESPONSE => {
            let (page_id, text, next_page_id) = page_text::read_page_text_query_response(&mut r)?;
            ServerPacket::PageTextQueryResponse {
                page_id,
                text,
                next_page_id,
            }
        }
        opcode::SMSG_GAMEOBJECT_CUSTOM_ANIM => {
            let (guid, anim_id) = gameobject::read_gameobject_custom_anim(&mut r)?;
            ServerPacket::GameObjectCustomAnim { guid, anim_id }
        }
        opcode::SMSG_GAMEOBJECT_DESPAWN_ANIM => {
            let guid = gameobject::read_gameobject_despawn_anim(&mut r)?;
            ServerPacket::GameObjectDespawnAnim { guid }
        }
        // Both fishing verdicts are empty-bodied (the opcode IS the message).
        opcode::SMSG_FISH_NOT_HOOKED => ServerPacket::FishNotHooked,
        opcode::SMSG_FISH_ESCAPED => ServerPacket::FishEscaped,
        opcode::SMSG_LOGOUT_COMPLETE => ServerPacket::LogoutComplete,
        // `{u32 reason, u8 instant}` — the `instant` byte is what the CAMP/QUIT countdown hangs
        // on (see the variant's own doc); it used to be dropped on the floor here.
        opcode::SMSG_LOGOUT_RESPONSE => ServerPacket::LogoutResponse {
            reason: read_u32_le(&mut r)?,
            instant: read_u8(&mut r)? != 0,
        },
        opcode::SMSG_LOGOUT_CANCEL_ACK => ServerPacket::LogoutCancelAck,
        // The keepalive echo: our CMSG_PING's sequence number back (vmangos `_HandlePing` — the
        // body is the one u32 it read from us).
        opcode::SMSG_PONG => ServerPacket::Pong {
            sequence: read_u32_le(&mut r)?,
        },
        // The force-speed-change family — one arm per kind, one shared body reader (the wire shape
        // and the mandatory ack protocol are documented on the opcode block).
        opcode::SMSG_FORCE_WALK_SPEED_CHANGE => read_force_speed(SpeedKind::Walk, &mut r)?,
        opcode::SMSG_FORCE_RUN_SPEED_CHANGE => read_force_speed(SpeedKind::Run, &mut r)?,
        opcode::SMSG_FORCE_RUN_BACK_SPEED_CHANGE => read_force_speed(SpeedKind::RunBack, &mut r)?,
        opcode::SMSG_FORCE_SWIM_SPEED_CHANGE => read_force_speed(SpeedKind::Swim, &mut r)?,
        opcode::SMSG_FORCE_SWIM_BACK_SPEED_CHANGE => read_force_speed(SpeedKind::SwimBack, &mut r)?,
        opcode::SMSG_FORCE_TURN_RATE_CHANGE => read_force_speed(SpeedKind::TurnRate, &mut r)?,
        // The group/party family (see the opcode block's note; bodies in `group`).
        opcode::SMSG_GROUP_INVITE => ServerPacket::GroupInvite {
            inviter: group::read_group_invite(&mut r)?,
        },
        opcode::SMSG_GROUP_DECLINE => ServerPacket::GroupDecline {
            name: group::read_group_decline(&mut r)?,
        },
        opcode::SMSG_GROUP_UNINVITE => ServerPacket::GroupUninvited,
        opcode::SMSG_GROUP_SET_LEADER => ServerPacket::GroupLeaderChanged {
            name: group::read_group_set_leader(&mut r)?,
        },
        opcode::SMSG_GROUP_DESTROYED => ServerPacket::GroupDestroyed,
        opcode::SMSG_GROUP_LIST => {
            let (group_type, own_flags, members, leader, loot) = group::read_group_list(&mut r)?;
            ServerPacket::GroupList {
                group_type,
                own_flags,
                members,
                leader,
                loot,
            }
        }
        opcode::SMSG_PARTY_COMMAND_RESULT => {
            let (operation, member, result) = group::read_party_command_result(&mut r)?;
            ServerPacket::PartyCommandResult {
                operation,
                member,
                result,
            }
        }
        opcode::SMSG_PARTY_MEMBER_STATS | opcode::SMSG_PARTY_MEMBER_STATS_FULL => {
            let (guid, info) = group::read_party_member_stats(&mut r)?;
            ServerPacket::PartyMemberStats {
                guid,
                full: opcode == opcode::SMSG_PARTY_MEMBER_STATS_FULL,
                info: Box::new(info),
            }
        }
        opcode::MSG_MINIMAP_PING => {
            let (guid, x, y) = group::read_minimap_ping(&mut r)?;
            ServerPacket::MinimapPing { guid, x, y }
        }
        opcode::MSG_RAID_TARGET_UPDATE => match group::read_raid_target_update(&mut r)? {
            group::RaidTargetUpdate::Delta { icon, guid } => {
                ServerPacket::RaidTargetSet { icon, guid }
            }
            group::RaidTargetUpdate::List(entries) => ServerPacket::RaidTargetList { entries },
        },
        opcode::SMSG_RAID_INSTANCE_INFO => ServerPacket::RaidInstanceInfo {
            entries: group::read_raid_instance_info(&mut r)?,
        },
        opcode::MSG_RAID_READY_CHECK => match group::read_ready_check(&mut r)? {
            group::ReadyCheck::Started => ServerPacket::ReadyCheckRequest,
            group::ReadyCheck::Answer { guid, ready } => {
                ServerPacket::ReadyCheckAnswer { guid, ready }
            }
        },
        // The duel family (decision 0633; bodies in `duel`). Both empty-body arms carry their
        // whole meaning in the opcode.
        opcode::SMSG_DUEL_REQUESTED => {
            let req = duel::read_duel_requested(&mut r)?;
            ServerPacket::DuelRequested {
                arbiter: req.arbiter,
                challenger: req.challenger,
            }
        }
        opcode::SMSG_DUEL_OUTOFBOUNDS => ServerPacket::DuelOutOfBounds,
        opcode::SMSG_DUEL_INBOUNDS => ServerPacket::DuelInBounds,
        opcode::SMSG_DUEL_COMPLETE => ServerPacket::DuelComplete {
            started: duel::read_duel_complete(&mut r)?,
        },
        opcode::SMSG_DUEL_WINNER => {
            let w = duel::read_duel_winner(&mut r)?;
            ServerPacket::DuelWinner {
                fled: w.fled,
                winner: w.winner,
                loser: w.loser,
            }
        }
        opcode::SMSG_DUEL_COUNTDOWN => ServerPacket::DuelCountdown {
            seconds: duel::read_duel_countdown(&mut r)?,
        },
        // The honor pair (decision 1512; bodies in `pvp`). MSG_INSPECT_HONOR_STATS is an `MSG_`:
        // opcode 0x2D6 carries our 8-byte request AND the server's 50-byte reply. There is no
        // ambiguity to resolve here — `parse_server` is fed only by the world *reader*, so an
        // 0x2D6 body arriving at this function is always the reply. The request shape is built by
        // `pvp::inspect_honor_stats` and goes straight out through `world::writer::pvp`; it never
        // reaches this match.
        opcode::MSG_INSPECT_HONOR_STATS => {
            ServerPacket::InspectHonorStats(pvp::read_inspect_honor_stats(&mut r)?)
        }
        opcode::SMSG_PVP_CREDIT => ServerPacket::PvpCredit(pvp::read_pvp_credit(&mut r)?),
        // The mirror timers (decision 0874; bodies in `mirror_timer`). START carries the timer's
        // whole state and is re-sent on every change — there is no update opcode in the family.
        opcode::SMSG_START_MIRROR_TIMER => {
            ServerPacket::MirrorTimerStart(mirror_timer::read_start_mirror_timer(&mut r)?)
        }
        opcode::SMSG_PAUSE_MIRROR_TIMER => {
            let (kind, paused) = mirror_timer::read_pause_mirror_timer(&mut r)?;
            ServerPacket::MirrorTimerPause { kind, paused }
        }
        opcode::SMSG_STOP_MIRROR_TIMER => ServerPacket::MirrorTimerStop {
            kind: mirror_timer::read_stop_mirror_timer(&mut r)?,
        },
        // The social family (decision 0668; bodies in `social`). Both list packets are
        // replace-everything snapshots; the status packet is one result about one player.
        opcode::SMSG_FRIEND_LIST => ServerPacket::FriendList {
            friends: social::read_friend_list(&mut r)?,
        },
        opcode::SMSG_IGNORE_LIST => ServerPacket::IgnoreList {
            guids: social::read_ignore_list(&mut r)?,
        },
        opcode::SMSG_FRIEND_STATUS => {
            ServerPacket::FriendStatus(social::read_friend_status(&mut r)?)
        }
        opcode::SMSG_WHO => ServerPacket::WhoResults(social::read_who(&mut r)?),
        // The guild family (bodies in `guild`). The two big ones are caches — a query response
        // fills the guild-id→identity cache, the roster replaces the whole membership — and the
        // three small ones are notifications. `SMSG_GUILD_ROSTER`'s member loop carries the
        // family's one conditional field; see `guild::read_guild_roster`.
        opcode::SMSG_GUILD_QUERY_RESPONSE => {
            ServerPacket::GuildQueryResponse(guild::read_guild_query_response(&mut r)?)
        }
        opcode::SMSG_GUILD_ROSTER => ServerPacket::GuildRoster(guild::read_guild_roster(&mut r)?),
        opcode::SMSG_GUILD_EVENT => ServerPacket::GuildEvent(guild::read_guild_event(&mut r)?),
        opcode::SMSG_GUILD_COMMAND_RESULT => {
            ServerPacket::GuildCommandResult(guild::read_guild_command_result(&mut r)?)
        }
        opcode::SMSG_GUILD_INVITE => {
            let (inviter, guild) = guild::read_guild_invite(&mut r)?;
            ServerPacket::GuildInvite { inviter, guild }
        }
        opcode::SMSG_GUILD_DECLINE => ServerPacket::GuildDecline {
            name: guild::read_guild_decline(&mut r)?,
        },
        opcode::SMSG_GUILD_INFO => ServerPacket::GuildInfo(guild::read_guild_info(&mut r)?),
        // The petition family (bodies in `petition`) — founding a guild, which at 1.12 is an
        // entirely different wire from the guild family above. Note `SMSG_PETITION_SHOW_SIGNATURES`
        // answers two different asks, and the two `MSG_` opcodes read a *different* body from the
        // one they write (see `petition`'s own docs).
        opcode::SMSG_PETITION_SHOWLIST => {
            ServerPacket::PetitionShowList(petition::read_petition_show_list(&mut r)?)
        }
        opcode::SMSG_PETITION_SHOW_SIGNATURES => {
            ServerPacket::PetitionShowSignatures(petition::read_petition_show_signatures(&mut r)?)
        }
        opcode::SMSG_PETITION_SIGN_RESULTS => {
            ServerPacket::PetitionSignResults(petition::read_petition_sign_results(&mut r)?)
        }
        opcode::SMSG_PETITION_QUERY_RESPONSE => {
            ServerPacket::PetitionQueryResponse(petition::read_petition_query_response(&mut r)?)
        }
        opcode::SMSG_TURN_IN_PETITION_RESULTS => ServerPacket::TurnInPetitionResults {
            result: petition::read_turn_in_petition_results(&mut r)?,
        },
        opcode::MSG_PETITION_DECLINE => ServerPacket::PetitionDeclined {
            player: petition::read_petition_decline(&mut r)?,
        },
        opcode::MSG_PETITION_RENAME => {
            ServerPacket::PetitionRenamed(petition::read_petition_rename(&mut r)?)
        }
        // The observer speed legs (decision 0441): a unit we don't control changed speed — a
        // creature or mid-spline player (SPLINE family), or a freely-moving player (MOVE_SET
        // family, which carries a fresh pose too). No ack on either.
        opcode::SMSG_SPLINE_SET_WALK_SPEED => read_spline_speed(SpeedKind::Walk, &mut r)?,
        opcode::SMSG_SPLINE_SET_RUN_SPEED => read_spline_speed(SpeedKind::Run, &mut r)?,
        opcode::SMSG_SPLINE_SET_RUN_BACK_SPEED => read_spline_speed(SpeedKind::RunBack, &mut r)?,
        opcode::SMSG_SPLINE_SET_SWIM_SPEED => read_spline_speed(SpeedKind::Swim, &mut r)?,
        opcode::SMSG_SPLINE_SET_SWIM_BACK_SPEED => read_spline_speed(SpeedKind::SwimBack, &mut r)?,
        opcode::SMSG_SPLINE_SET_TURN_RATE => read_spline_speed(SpeedKind::TurnRate, &mut r)?,
        opcode::MSG_MOVE_SET_WALK_SPEED => read_move_set_speed(SpeedKind::Walk, &mut r)?,
        opcode::MSG_MOVE_SET_RUN_SPEED => read_move_set_speed(SpeedKind::Run, &mut r)?,
        opcode::MSG_MOVE_SET_RUN_BACK_SPEED => read_move_set_speed(SpeedKind::RunBack, &mut r)?,
        opcode::MSG_MOVE_SET_SWIM_SPEED => read_move_set_speed(SpeedKind::Swim, &mut r)?,
        opcode::MSG_MOVE_SET_SWIM_BACK_SPEED => read_move_set_speed(SpeedKind::SwimBack, &mut r)?,
        opcode::MSG_MOVE_SET_TURN_RATE => read_move_set_speed(SpeedKind::TurnRate, &mut r)?,
        // Mount feedback (decision 0441): one u32 result code per attempt; error lines are a P2
        // trimming — decoded so the wire is modelled, surfaced for the debug log.
        opcode::SMSG_MOUNTRESULT => ServerPacket::MountResult {
            mount: true,
            code: read_u32_le(&mut r)?,
        },
        opcode::SMSG_DISMOUNTRESULT => ServerPacket::MountResult {
            mount: false,
            code: read_u32_le(&mut r)?,
        },
        // A nearby rider's flourish: one raw u64 guid (VERIFIED vmangos
        // `HandleMountSpecialAnimOpcode` — `data << GetObjectGuid()`, full 8 bytes, not packed).
        opcode::SMSG_MOUNTSPECIAL_ANIM => ServerPacket::MountSpecialAnim {
            guid: read_u64_le(&mut r)?,
        },
        // The control handoff: PACKED guid (unlike the flourish above) + one byte (VERIFIED
        // vmangos `Server/Packets/Misc.cpp:677-682` — `WriteAsPacked()`, then `uint8 allowMove`).
        opcode::SMSG_CLIENT_CONTROL_UPDATE => ServerPacket::ClientControlUpdate {
            mover: read_packed_guid(&mut r)?,
            allow_move: read_u8(&mut r)? != 0,
        },
        // The taxi/flight-master family (decision 0484): the map (SHOWTAXINODES), a status
        // answer/first-visit learn (TAXINODE_STATUS), the activate verdict, and the multi-hop
        // path ack (NEW_TAXI_PATH, empty body).
        opcode::SMSG_SHOWTAXINODES => {
            let (window, flightmaster, nearest_node, known) = taxi::read_show_taxi_nodes(&mut r)?;
            ServerPacket::ShowTaxiNodes {
                window,
                flightmaster,
                nearest_node,
                known,
            }
        }
        opcode::SMSG_TAXINODE_STATUS => {
            let (guid, known) = taxi::read_taxi_node_status(&mut r)?;
            ServerPacket::TaxiNodeStatus {
                guid,
                known: known != 0,
            }
        }
        opcode::SMSG_ACTIVATETAXIREPLY => ServerPacket::ActivateTaxiReply {
            code: taxi::read_activate_taxi_reply(&mut r)?,
        },
        opcode::SMSG_NEW_TAXI_PATH => ServerPacket::NewTaxiPath,
        // The mail arc (decision 0544 P0): the inbox list, the send/take/return/delete verdict,
        // the letter-body ask-once fetch, and the arrival pair (RECEIVED_MAIL + the
        // QUERY_NEXT_MAIL_TIME reply — same opcode as our empty-body request).
        opcode::SMSG_MAIL_LIST_RESULT => ServerPacket::MailList {
            mails: mail::read_mail_list_result(&mut r)?,
        },
        opcode::SMSG_SEND_MAIL_RESULT => {
            let (mail_id, action, error, equip_error, item) = mail::read_send_mail_result(&mut r)?;
            ServerPacket::SendMailResult {
                mail_id,
                action,
                error,
                equip_error,
                item,
            }
        }
        opcode::SMSG_ITEM_TEXT_QUERY_RESPONSE => {
            let (text_id, text) = mail::read_item_text_query_response(&mut r)?;
            ServerPacket::ItemTextQueryResponse { text_id, text }
        }
        opcode::SMSG_RECEIVED_MAIL => ServerPacket::ReceivedMail {
            seconds: mail::read_received_mail(&mut r)?,
        },
        opcode::MSG_QUERY_NEXT_MAIL_TIME => ServerPacket::NextMailTime {
            seconds: mail::read_query_next_mail_time(&mut r)?,
        },
        // The auction house arc (decision 1511 P0). MSG_AUCTION_HELLO is two-way — this is the
        // REPLY (our request is the same opcode with just the guid), and it is what opens the
        // window. The three list results share one frame and one 64-byte record; their reader is
        // bounded by the buffer as well as by the wire `count`, because vmangos's browse fast path
        // can count a record it then fails to write.
        opcode::MSG_AUCTION_HELLO => {
            let (auctioneer, house_id) = auction::read_auction_hello(&mut r)?;
            ServerPacket::AuctionHello {
                auctioneer,
                house_id,
            }
        }
        opcode::SMSG_AUCTION_COMMAND_RESULT => {
            let (auction_id, action, error, tail) = auction::read_auction_command_result(&mut r)?;
            ServerPacket::AuctionCommandResult {
                auction_id,
                action,
                error,
                tail,
            }
        }
        opcode::SMSG_AUCTION_LIST_RESULT => {
            let (auctions, total_count) = auction::read_auction_list_result(&mut r)?;
            ServerPacket::AuctionListResult {
                auctions,
                total_count,
            }
        }
        opcode::SMSG_AUCTION_OWNER_LIST_RESULT => {
            let (auctions, total_count) = auction::read_auction_list_result(&mut r)?;
            ServerPacket::AuctionOwnerListResult {
                auctions,
                total_count,
            }
        }
        opcode::SMSG_AUCTION_BIDDER_LIST_RESULT => {
            let (auctions, total_count) = auction::read_auction_list_result(&mut r)?;
            ServerPacket::AuctionBidderListResult {
                auctions,
                total_count,
            }
        }
        // The two notifications carry different field orders and the owner one has no house id —
        // two readers, never one.
        opcode::SMSG_AUCTION_BIDDER_NOTIFICATION => ServerPacket::AuctionBidderNotification(
            auction::read_auction_bidder_notification(&mut r)?,
        ),
        opcode::SMSG_AUCTION_OWNER_NOTIFICATION => ServerPacket::AuctionOwnerNotification(
            auction::read_auction_owner_notification(&mut r)?,
        ),
        opcode::SMSG_AUCTION_REMOVED_NOTIFICATION => {
            let (auction_id, item_entry, random_property_id) =
                auction::read_auction_removed_notification(&mut r)?;
            ServerPacket::AuctionRemovedNotification {
                auction_id,
                item_entry,
                random_property_id,
            }
        }
        // The player-trade arc (decision 0592 P0): the state-machine status pulse and the
        // item/gold snapshot for one window side.
        opcode::SMSG_TRADE_STATUS => ServerPacket::TradeStatus {
            status: trade::read_trade_status(&mut r)?,
        },
        opcode::SMSG_TRADE_STATUS_EXTENDED => ServerPacket::TradeStatusExtended {
            state: Box::new(trade::read_trade_status_extended(&mut r)?),
        },
        // The world-state table — the `$<n>w`/`$<n>e` NPC-text tokens' source (and a battleground
        // scoreboard's, later). Both opcodes write the one table; see [`super::world_state`].
        opcode::SMSG_INIT_WORLD_STATES => {
            ServerPacket::InitWorldStates(world_state::read_init_world_states(&mut r)?)
        }
        opcode::SMSG_UPDATE_WORLD_STATE => {
            let (id, value) = world_state::read_update_world_state(&mut r)?;
            ServerPacket::UpdateWorldState { id, value }
        }
        // A relayed player movement packet: `[packed mover guid][MovementInfo]`, same opcode the mover
        // sent. This is how other players' walking/turning/strafing reaches us (creatures use
        // SMSG_MONSTER_MOVE instead). Surface the pose + live move-flags; the app extrapolates between.
        o if is_movement_relay(o) => {
            let guid = read_packed_guid(&mut r)?;
            let info = movement::read_movement_info(&mut r)?;
            ServerPacket::PlayerMove {
                guid,
                opcode: o,
                flags: info.flags,
                position: info.position,
                orientation: info.orientation,
                pitch: info.pitch,
                time: info.timestamp,
                fall_time: info.fall_time,
                jump: info.jump,
                transport: info.transport,
            }
        }
        other => ServerPacket::Other { opcode: other },
    })
}
