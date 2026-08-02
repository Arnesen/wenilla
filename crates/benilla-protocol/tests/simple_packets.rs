//! Simple bare-scalar server packets — bodies too trivial to need golden hex from a validated
//! implementation: auth challenge/new-world/login-timespeed, the server-pushed sound trio, and
//! weather. Split out of the former `tests/messages.rs` — see `tests/common` for the shared
//! fixtures and methodology note.

mod common;

use benilla_protocol::events::{decode, SessionEvent};
use benilla_protocol::messages;
use benilla_protocol::{MoveMode, ServerPacket};
use common::hx;

#[test]
fn simple_server_bodies_parse() {
    // SMSG_AUTH_CHALLENGE: just a u32 seed.
    match messages::parse_server(messages::opcode::SMSG_AUTH_CHALLENGE, &hx("efbeadde")).unwrap() {
        ServerPacket::AuthChallenge { server_seed } => assert_eq!(server_seed, 0xDEAD_BEEF),
        _ => panic!("auth challenge"),
    }
    // SMSG_NEW_WORLD: map u32=1, position (1,2,3), orientation 0.5.
    let nw = hx("010000000000803f0000004000004040000000bf");
    match messages::parse_server(messages::opcode::SMSG_NEW_WORLD, &nw).unwrap() {
        ServerPacket::NewWorld {
            map,
            position,
            orientation,
        } => {
            assert_eq!(map, 1);
            assert_eq!((position.x, position.y, position.z), (1.0, 2.0, 3.0));
            assert_eq!(orientation, -0.5);
        }
        _ => panic!("new world"),
    }
    // SMSG_LOGIN_SETTIMESPEED: packed DateTime (LSB up: minute:6, hour:5, weekday:3, day:6,
    // month:4, year:5) with 14:30 on year 26 / month 6 / day 17; timescale. The day serial
    // flattens the date with the packed convention's 31-day months: 26·372 + 6·31 + 17 = 9875.
    let datetime: u32 = (26 << 24) | (6 << 20) | (17 << 14) | (14 << 6) | 30;
    let mut ts = datetime.to_le_bytes().to_vec();
    ts.extend_from_slice(&0.0166_6667f32.to_le_bytes());
    match messages::parse_server(messages::opcode::SMSG_LOGIN_SETTIMESPEED, &ts).unwrap() {
        ServerPacket::TimeSpeed {
            hours,
            minutes,
            day_serial,
            timescale,
        } => {
            assert_eq!((hours, minutes), (14, 30));
            assert_eq!(day_serial, 26 * 372 + 6 * 31 + 17);
            assert_eq!(timescale, 0.0166_6667);
        }
        _ => panic!("settimespeed"),
    }
}

/// The cinematic trigger (opcode 250, body a bare `CinematicSequences.dbc` id u32 per the vmangos
/// `SendCinematicStart` sender) parses and decodes to the event the Net drain must ack — and the
/// char-delete result (opcode 60, one result byte) parses.
#[test]
fn cinematic_and_char_delete_parse_and_decode() {
    // SMSG_TRIGGER_CINEMATIC: u32 cinematic id (41 = the dwarf intro, live-captured 2026-07-07).
    let body = 41u32.to_le_bytes();
    let p = messages::parse_server(messages::opcode::SMSG_TRIGGER_CINEMATIC, &body).unwrap();
    assert!(matches!(
        p,
        ServerPacket::TriggerCinematic { cinematic_id: 41 }
    ));
    assert!(matches!(
        decode(p)[..],
        [SessionEvent::CinematicTriggered { cinematic_id: 41 }]
    ));

    // SMSG_CHAR_DELETE: one result byte (0x39 = success, live-verified).
    let p = messages::parse_server(messages::opcode::SMSG_CHAR_DELETE, &[0x39]).unwrap();
    assert!(matches!(p, ServerPacket::CharDelete { result: 0x39 }));
}

/// The server-pushed sound trio (opcodes vmangos `Opcodes_1_12_1.h` 631/632/722; bodies are bare
/// LE scalars per the vmangos senders): each parses to its packet and decodes to one event.
#[test]
fn server_sound_trio_parse_and_decode() {
    // SMSG_PLAY_SOUND: u32 soundId.
    let body = 8595u32.to_le_bytes();
    let p = messages::parse_server(messages::opcode::SMSG_PLAY_SOUND, &body).unwrap();
    assert!(matches!(p, ServerPacket::PlaySound { sound_id: 8595 }));
    assert!(matches!(
        decode(p)[..],
        [SessionEvent::PlaySound { sound_id: 8595 }]
    ));

    // SMSG_PLAY_MUSIC: u32 musicId.
    let body = 2523u32.to_le_bytes();
    let p = messages::parse_server(messages::opcode::SMSG_PLAY_MUSIC, &body).unwrap();
    assert!(matches!(
        decode(p)[..],
        [SessionEvent::PlayMusic { music_id: 2523 }]
    ));

    // SMSG_PLAY_OBJECT_SOUND: u32 soundId + u64 guid (the fishing-bobber splash, vmangos
    // GameObject.cpp:373 sends exactly soundId 3355).
    let mut body = 3355u32.to_le_bytes().to_vec();
    body.extend_from_slice(&0xF110_0000_0000_002Au64.to_le_bytes());
    let p = messages::parse_server(messages::opcode::SMSG_PLAY_OBJECT_SOUND, &body).unwrap();
    assert!(matches!(
        decode(p)[..],
        [SessionEvent::PlayObjectSound {
            sound_id: 3355,
            guid: 0xF110_0000_0000_002A,
        }]
    ));
}

/// SMSG_WEATHER (opcode vmangos 756; body `u32 type, f32 grade, u32 soundId, u8 instant` per
/// `Weather::SendWeatherForPlayersInZone`, 1.12 branch): parses and decodes to one event, with
/// the soundId being a real SoundEntries loop kit (8534 = RainMedium).
#[test]
fn weather_parses_and_decodes() {
    let mut body = 1u32.to_le_bytes().to_vec(); // WEATHER_TYPE_RAIN
    body.extend_from_slice(&0.7f32.to_le_bytes());
    body.extend_from_slice(&8534u32.to_le_bytes());
    body.push(0);
    let p = messages::parse_server(messages::opcode::SMSG_WEATHER, &body).unwrap();
    match decode(p).as_slice() {
        [SessionEvent::Weather {
            weather_type,
            grade,
            sound_id,
            instant,
        }] => {
            assert_eq!((*weather_type, *sound_id, *instant), (1, 8534, false));
            assert!((grade - 0.7).abs() < 1e-6);
        }
        other => panic!("weather decode: {} events", other.len()),
    }
}

/// `SMSG_SET_PROFICIENCY` (295): u8 itemClass + u32 subclass bitmask (vmangos `Skill.h` /
/// `Skill.cpp AppendBodyTo`) — parses and decodes to the proficiency event the item tooltip's
/// slot-line red reads.
#[test]
fn set_proficiency_parses_and_decodes() {
    assert_eq!(messages::opcode::SMSG_SET_PROFICIENCY, 295);
    // Weapons (class 2), mask 0x2408F: the fresh-warrior shape.
    let body = hx("028f400200");
    let p = messages::parse_server(messages::opcode::SMSG_SET_PROFICIENCY, &body).unwrap();
    match p {
        ServerPacket::SetProficiency {
            item_class,
            subclass_mask,
        } => {
            assert_eq!(item_class, 2);
            assert_eq!(subclass_mask, 0x0002_408f);
        }
        _ => panic!("set proficiency"),
    }
    let p = messages::parse_server(messages::opcode::SMSG_SET_PROFICIENCY, &body).unwrap();
    assert!(matches!(
        decode(p)[..],
        [SessionEvent::Proficiency {
            item_class: 2,
            subclass_mask: 0x0002_408f
        }]
    ));
}

/// `SMSG_SET_FACTION_STANDING` (292): u32 count + count x (u32 reputationListId, i32 standing)
/// (vmangos `ReputationMgr::SendState` / `SetFactionStanding::AppendBodyTo`) — the mid-session
/// standing delta the reputation red re-ranks from.
#[test]
fn set_faction_standing_parses_and_decodes() {
    assert_eq!(messages::opcode::SMSG_SET_FACTION_STANDING, 292);
    // Two slots: list 46 -> 3000, list 89 -> -6000 (a standing can go down).
    let body = hx("020000002e000000b80b00005900000090e8ffff");
    let p = messages::parse_server(messages::opcode::SMSG_SET_FACTION_STANDING, &body).unwrap();
    match p {
        ServerPacket::SetFactionStanding { ref standings } => {
            assert_eq!(standings[..], [(46, 3000), (89, -6000)]);
        }
        _ => panic!("set faction standing"),
    }
    assert!(matches!(
        decode(p)[..],
        [SessionEvent::ReputationDelta { ref standings }]
            if standings[..] == [(46, 3000), (89, -6000)]
    ));
}

/// The dropped-packet tally feed (the wire-coverage instrument): an opcode with no parse arm falls
/// through to `ServerPacket::Other` and MUST decode to a `PacketDropped` event (not silence) so the
/// app can tally the gap — and the generated 1.12.1 name table resolves both known and unassigned
/// opcode numbers.
#[test]
fn unknown_opcode_decodes_to_packet_dropped() {
    // 0x0319 = MSG_MOVE_TIME_SKIPPED — assigned in 1.12.1 but deliberately unparsed by benilla.
    let packet = messages::parse_server(0x0319, &hx("0102030405")).unwrap();
    assert!(matches!(packet, ServerPacket::Other { opcode: 0x0319 }));
    match decode(packet).as_slice() {
        [SessionEvent::PacketDropped {
            opcode: 0x0319,
            unparseable: false,
        }] => {}
        other => panic!("expected one PacketDropped event, got {other:?}"),
    }
    // The generated name table: a known opcode resolves, an unassigned number doesn't.
    assert_eq!(messages::opcode_name(0x0319), Some("MSG_MOVE_TIME_SKIPPED"));
    assert_eq!(
        messages::opcode_name(messages::opcode::SMSG_UPDATE_OBJECT),
        Some("SMSG_UPDATE_OBJECT")
    );
    assert_eq!(messages::opcode_name(0xFFFF), None);
}

/// The death arc's wire family (decision 0308): the corpse query answer (both shapes + the
/// dungeon-entrance map split), the reclaim delay, a resurrect request, the spirit-healer
/// confirm, and the ack'd movement-flag family with its PACKED mover guid — all through
/// `parse_server` and the event decode.
#[test]
fn death_arc_family_parses_and_decodes() {
    use benilla_protocol::messages::opcode;

    // MSG_CORPSE_QUERY, found: u8(1), i32 map 0, xyz, u32 corpsemap 36 (a dungeon corpse whose
    // display coords were entrance-rewritten — the two maps deliberately differ).
    let mut body = vec![1u8];
    body.extend_from_slice(&0i32.to_le_bytes());
    body.extend_from_slice(&(-11209.6f32).to_le_bytes());
    body.extend_from_slice(&1666.54f32.to_le_bytes());
    body.extend_from_slice(&25.0f32.to_le_bytes());
    body.extend_from_slice(&36u32.to_le_bytes());
    let pkt = messages::parse_server(opcode::MSG_CORPSE_QUERY, &body).unwrap();
    match decode(pkt).as_slice() {
        [SessionEvent::CorpseQuery {
            found,
            display_map,
            position,
            corpse_map,
        }] => {
            assert!(found);
            assert_eq!(*display_map, 0);
            assert_eq!(*corpse_map, 36);
            assert_eq!(position[2], 25.0);
        }
        other => panic!("corpse query decode: {other:?}"),
    }
    // The unprompted not-found shape (bones conversion): the lone u8(0).
    let pkt = messages::parse_server(opcode::MSG_CORPSE_QUERY, &[0u8]).unwrap();
    match decode(pkt).as_slice() {
        [SessionEvent::CorpseQuery { found: false, .. }] => {}
        other => panic!("corpse query not-found decode: {other:?}"),
    }

    // SMSG_CORPSE_RECLAIM_DELAY: u32 ms.
    let pkt = messages::parse_server(opcode::SMSG_CORPSE_RECLAIM_DELAY, &30_000u32.to_le_bytes())
        .unwrap();
    match decode(pkt).as_slice() {
        [SessionEvent::CorpseReclaimDelay { delay_ms: 30_000 }] => {}
        other => panic!("reclaim delay decode: {other:?}"),
    }

    // SMSG_RESURRECT_REQUEST, player caster: guid, u32 len 1, empty cstring, sickness 0, timer 1.
    let mut body = 0x2Au64.to_le_bytes().to_vec();
    body.extend_from_slice(&1u32.to_le_bytes());
    body.extend_from_slice(&[0, 0, 1]);
    let pkt = messages::parse_server(opcode::SMSG_RESURRECT_REQUEST, &body).unwrap();
    match decode(pkt).as_slice() {
        [SessionEvent::ResurrectRequest {
            caster: 0x2A,
            name,
            sickness: false,
            has_timer: true,
        }] => assert!(name.is_empty(), "player caster ⇒ empty wire name"),
        other => panic!("resurrect request decode: {other:?}"),
    }

    // SMSG_SPIRIT_HEALER_CONFIRM: one full guid.
    let pkt = messages::parse_server(
        opcode::SMSG_SPIRIT_HEALER_CONFIRM,
        &0xF130_0000_0000_2AB3u64.to_le_bytes(),
    )
    .unwrap();
    match decode(pkt).as_slice() {
        [SessionEvent::SpiritHealerConfirm {
            npc: 0xF130_0000_0000_2AB3,
        }] => {}
        other => panic!("spirit healer confirm decode: {other:?}"),
    }

    // **The whole ack'd movement-mode family, all eight opcodes** (decision 0866): one wire shape,
    // PACKED guid + u32 counter. Guid 0x2A packs as mask 0x01 + one byte. Every pair is exercised,
    // because the family's whole point is that adding a mode must not need a new lane — and because
    // the four that were missing (feather-fall, hover) were missing *silently*.
    let body = hx("012a07000000"); // packed(0x2A) + counter 7
    for (op, mode, apply) in [
        (opcode::SMSG_FORCE_MOVE_ROOT, MoveMode::Root, true),
        (opcode::SMSG_FORCE_MOVE_UNROOT, MoveMode::Root, false),
        (opcode::SMSG_MOVE_WATER_WALK, MoveMode::WaterWalk, true),
        (opcode::SMSG_MOVE_LAND_WALK, MoveMode::WaterWalk, false),
        (opcode::SMSG_MOVE_FEATHER_FALL, MoveMode::FeatherFall, true),
        (opcode::SMSG_MOVE_NORMAL_FALL, MoveMode::FeatherFall, false),
        (opcode::SMSG_MOVE_SET_HOVER, MoveMode::Hover, true),
        (opcode::SMSG_MOVE_UNSET_HOVER, MoveMode::Hover, false),
    ] {
        let pkt = messages::parse_server(op, &body).unwrap();
        match decode(pkt).as_slice() {
            [SessionEvent::MoveMode {
                guid: 0x2A,
                counter: 7,
                mode: got_mode,
                apply: got_apply,
            }] if *got_mode == mode && *got_apply == apply => {}
            other => panic!("move mode decode for opcode {op:#06x}: {other:?}"),
        }
    }

    // The mode→bit map, VERIFIED vmangos `Objects/MovementInfo.h:25-62`. These are the bits the
    // mover reads and the ack echoes, so a transcription slip here is silent everywhere else.
    assert_eq!(MoveMode::Root.flag(), 0x0000_1000);
    assert_eq!(MoveMode::WaterWalk.flag(), 0x1000_0000);
    assert_eq!(MoveMode::FeatherFall.flag(), 0x2000_0000);
    assert_eq!(MoveMode::Hover.flag(), 0x4000_0000);

    // Root is the ONLY mode whose ack opcode differs by direction, and the only one whose ack body
    // carries no trailing apply dword — vmangos routes it to `HandleMoveRootAck`, the other three to
    // `HandleMovementFlagChangeToggleAck` (`Server/Protocol/Opcodes.cpp:314-816`).
    assert_eq!(
        MoveMode::Root.ack_opcode(true),
        opcode::CMSG_FORCE_MOVE_ROOT_ACK
    );
    assert_eq!(
        MoveMode::Root.ack_opcode(false),
        opcode::CMSG_FORCE_MOVE_UNROOT_ACK
    );
    assert!(!MoveMode::Root.ack_carries_apply());
    for mode in [MoveMode::WaterWalk, MoveMode::FeatherFall, MoveMode::Hover] {
        assert_eq!(
            mode.ack_opcode(true),
            mode.ack_opcode(false),
            "{mode:?}: one ack opcode both ways"
        );
        assert!(mode.ack_carries_apply(), "{mode:?}: ack carries apply");
    }
}

/// The movement-mode ack bodies (client side): full guid + echoed counter + MovementInfo, with the
/// trailing `u32 apply` on every mode EXCEPT root (vmangos `Movement.cpp:38-59` — `MoveRootAck` has
/// no apply dword). The root ack must be exactly 4 bytes shorter than the others for the same
/// inputs.
#[test]
fn move_flag_ack_bodies_differ_by_the_apply_tail() {
    use benilla_protocol::messages::{move_flag_ack, MovementInfo};
    use benilla_protocol::wire::Vector3d;

    let info = MovementInfo {
        flags: 0,
        timestamp: 0x1122_3344,
        position: Vector3d {
            x: 1.0,
            y: 2.0,
            z: 3.0,
        },
        orientation: 0.5,
        transport: None,
        pitch: 0.0,
        fall_time: 0,
        jump: None,
    };
    let root = move_flag_ack(0x2A, 7, &info, None);
    let walk = move_flag_ack(0x2A, 7, &info, Some(true));
    assert_eq!(root.len() + 4, walk.len());
    assert_eq!(&walk[..root.len()], root.as_slice());
    assert_eq!(&walk[root.len()..], 1u32.to_le_bytes());
    // The full (unpacked) guid + counter head.
    assert_eq!(&root[..8], &0x2Au64.to_le_bytes());
    assert_eq!(&root[8..12], &7u32.to_le_bytes());
}

/// `SMSG_DURABILITY_DAMAGE_DEATH` (0x2BD): an EMPTY body (vmangos `DurabilityDamageDeath::
/// AppendBodyTo` writes nothing) parsing to its cue event — the red 10%-loss error line.
#[test]
fn durability_damage_death_is_an_empty_body_cue() {
    let pkt = messages::parse_server(messages::opcode::SMSG_DURABILITY_DAMAGE_DEATH, &[]).unwrap();
    assert!(matches!(pkt, ServerPacket::DurabilityDamageDeath));
    assert!(matches!(
        decode(pkt).as_slice(),
        [SessionEvent::DurabilityDamageDeath]
    ));
}

/// `SMSG_TRANSFER_PENDING` (0x3F): `u32 mapId` alone for an ordinary far teleport; `+ u32
/// transportEntry + u32 oldMapId` when the player rides a transport through the transfer
/// (VERIFIED vmangos `Misc.cpp:493-501`). The block's presence routes how NEW_WORLD's
/// coordinates are read (decision 0455), so both shapes are pinned — and the abort
/// (`SMSG_TRANSFER_ABORTED`, one reason byte) clears the latch.
#[test]
fn transfer_pending_both_shapes_and_abort_parse_and_decode() {
    // Plain: mapId 1 (Kalimdor), no transport block.
    let plain =
        messages::parse_server(messages::opcode::SMSG_TRANSFER_PENDING, &hx("01000000")).unwrap();
    assert!(matches!(
        plain,
        ServerPacket::TransferPending {
            map: 1,
            transport: None
        }
    ));
    assert!(matches!(
        decode(plain).as_slice(),
        [SessionEvent::TransferPending {
            map_id: 1,
            transport_entry: None
        }]
    ));
    // Riding: mapId 0 (EK), transportEntry 176310 (0x2B0B6 — the Menethil boat), oldMapId 1.
    let mut riding = 0u32.to_le_bytes().to_vec();
    riding.extend_from_slice(&176310u32.to_le_bytes());
    riding.extend_from_slice(&1u32.to_le_bytes());
    let riding = messages::parse_server(messages::opcode::SMSG_TRANSFER_PENDING, &riding).unwrap();
    assert!(matches!(
        riding,
        ServerPacket::TransferPending {
            map: 0,
            transport: Some((176310, 1))
        }
    ));
    assert!(matches!(
        decode(riding).as_slice(),
        [SessionEvent::TransferPending {
            map_id: 0,
            transport_entry: Some(176310)
        }]
    ));
    // Abort: one reason byte.
    let abort = messages::parse_server(messages::opcode::SMSG_TRANSFER_ABORTED, &[2]).unwrap();
    assert!(matches!(abort, ServerPacket::TransferAborted { reason: 2 }));
    assert!(matches!(
        decode(abort).as_slice(),
        [SessionEvent::TransferAborted { reason: 2 }]
    ));
}
