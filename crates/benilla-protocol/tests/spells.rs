//! Spell/action/attack + spell-visual wire tests (mirrors `src/messages/spells.rs`): initial spells,
//! action buttons, cast/attack swing, cast result, attacker state, plus the spell-visual pipeline
//! (`SMSG_SPELL_START`/`GO`, `SMSG_SPELL_FAILED_OTHER`, `SMSG_CANCEL_AUTO_REPEAT`,
//! `SMSG_PLAY_SPELL_VISUAL`, decision 0099 phase 1). Split out of the former `tests/messages.rs` —
//! see `tests/common` for the shared fixtures and methodology note.

mod common;

use benilla_protocol::events::{decode, SessionEvent};
use benilla_protocol::messages;
use benilla_protocol::ServerPacket;
use common::hx;

#[test]
fn spell_and_action_wire() {
    use benilla_protocol::messages::{ActionButton, CastOutcome, ACTION_KIND_ITEM};

    // SMSG_INITIAL_SPELLS (vmangos Player::SendInitialSpells): u8 0; u16 n; n x (u16 id, u16 0);
    // u16 m; m x (u16 spell, u16 item, u16 category, u32 spellCdMs, u32 catCdMs).
    let body = hx(concat!(
        "00", "0300", "4e000000", // 78 Heroic Strike
        "cb190000", // 6603 Attack
        "111a0000", // 6673 Battle Shout
        "0100", "4e00", "0000", "2301", "88130000",
        "dc050000" // 78 on 5000ms cd, category 0x123 1500ms
    ));
    match messages::parse_server(messages::opcode::SMSG_INITIAL_SPELLS, &body).unwrap() {
        ServerPacket::InitialSpells {
            spell_ids,
            cooldowns,
        } => {
            assert_eq!(spell_ids, vec![78, 6603, 6673]);
            assert_eq!(cooldowns.len(), 1);
            assert_eq!((cooldowns[0].spell_id, cooldowns[0].category), (78, 0x123));
            assert_eq!(
                (cooldowns[0].spell_cd_ms, cooldowns[0].category_cd_ms),
                (5000, 1500)
            );
        }
        _ => panic!("initial spells"),
    }

    // SMSG_ACTION_BUTTONS: 120 packed u32s — action bits 0-23, kind bits 24-31, 0 = empty.
    let mut body = vec![0u8; 120 * 4];
    body[0..4].copy_from_slice(&0x0000_1A11u32.to_le_bytes()); // slot 0: spell 6673
    body[4..8].copy_from_slice(&0x8000_0075u32.to_le_bytes()); // slot 1: item 117
    body[44..48].copy_from_slice(&0x4000_0003u32.to_le_bytes()); // slot 11: macro 3
    let packet = messages::parse_server(messages::opcode::SMSG_ACTION_BUTTONS, &body).unwrap();
    let buttons = match &packet {
        ServerPacket::ActionButtons { buttons } => buttons.clone(),
        _ => panic!("action buttons"),
    };
    assert_eq!(
        buttons,
        vec![
            ActionButton {
                slot: 0,
                action: 6673,
                kind: 0
            },
            ActionButton {
                slot: 1,
                action: 117,
                kind: ACTION_KIND_ITEM
            },
            ActionButton {
                slot: 11,
                action: 3,
                kind: 0x40
            },
        ]
    );
    // ...and the decode passes them through as the ActionButtons event.
    match decode(packet).pop().unwrap() {
        SessionEvent::ActionButtons { buttons: b } => assert_eq!(b, buttons),
        _ => panic!("action buttons event"),
    }

    // CMSG_CAST_SPELL golden bytes (vmangos SpellCastTargets::read): self cast = mask 0, nothing
    // follows; unit cast = mask 2 + PACKED guid (build > 1.8.4 reads targets packed).
    assert_eq!(
        messages::cast_spell(6673, None),
        hx("111a00000000"),
        "self cast: u32 id + u16 mask 0"
    );
    let victim = 0xF130_0000_4500_002Au64; // creature: high 0xF130, entry 69, counter 0x2A
    assert_eq!(
        messages::cast_spell(5176, Some(victim)),
        hx("381400000200c92a4530f1"),
        "unit cast: u32 id + u16 mask 2 + packed guid (mask 0xC9)"
    );

    // CMSG_CAST_SPELL at an ITEM (0437 phase 3 — the enchant pick): mask 0x0010 + packed guid.
    // Item guid 0x4700_0000_0000_0951: present bytes 0 (0x51), 1 (0x09), 7 (0x47) → mask 0x83.
    let enchant_target = 0x4700_0000_0000_0951u64;
    assert_eq!(
        messages::cast_spell_item(7418, enchant_target),
        hx("fa1c0000100083510947"),
        "item cast: u32 id + u16 mask 0x10 + packed item guid"
    );

    // CMSG_ATTACKSWING: one full 8-byte guid.
    assert_eq!(messages::attack_swing(victim), hx("2a000045000030f1"));

    // CMSG_SET_ACTION_BUTTON opcode (296 / 0x0128 — vmangos Opcodes_1_12_1.h) + body: button u8 +
    // packed u32 (WorldPackets::Misc::SetActionButton::ReadFromWorldPacket, Misc.cpp:87-90).
    assert_eq!(
        messages::opcode::CMSG_SET_ACTION_BUTTON,
        0x0128,
        "CMSG_SET_ACTION_BUTTON opcode"
    );
    assert_eq!(
        messages::set_action_button(11, 0x8000_0075),
        hx("0b75000080"),
        "button 11 (lua slot 12), item 117 (kind 0x80<<24 | action 0x75)"
    );
    assert_eq!(
        messages::set_action_button(0, 0),
        hx("0000000000"),
        "packed 0 clears the slot"
    );

    // CMSG_SETSHEATHED opcode (480 / 0x01E0 — vmangos Opcodes_1_12_1.h) + body: one u32 state.
    assert_eq!(
        messages::opcode::CMSG_SETSHEATHED,
        0x01E0,
        "CMSG_SETSHEATHED opcode"
    );
    assert_eq!(messages::set_sheathed(1), hx("01000000"));

    // SMSG_CAST_RESULT: ok = status 0 ends the body; fail = status 2 + u8 reason.
    match messages::parse_server(messages::opcode::SMSG_CAST_RESULT, &hx("111a000000")).unwrap() {
        ServerPacket::CastResult { spell_id, outcome } => {
            assert_eq!((spell_id, outcome), (6673, CastOutcome::Ok));
        }
        _ => panic!("cast ok"),
    }
    let fail =
        messages::parse_server(messages::opcode::SMSG_CAST_RESULT, &hx("111a00000255")).unwrap();
    match decode(fail).pop().unwrap() {
        SessionEvent::CastResult {
            spell_id,
            success,
            reason,
        } => assert_eq!((spell_id, success, reason), (6673, false, Some(0x55))),
        _ => panic!("cast fail event"),
    }

    // SMSG_ATTACKSTART: two full guids. SMSG_ATTACKSTOP: two packed guids + u32 isDead.
    let body = hx("01000000000000002a000045000030f1");
    match messages::parse_server(messages::opcode::SMSG_ATTACKSTART, &body).unwrap() {
        ServerPacket::AttackStart {
            attacker,
            victim: v,
        } => {
            assert_eq!((attacker, v), (1, victim));
        }
        _ => panic!("attack start"),
    }
    let body = hx(concat!("0101", "c92a4530f1", "00000000"));
    match messages::parse_server(messages::opcode::SMSG_ATTACKSTOP, &body).unwrap() {
        ServerPacket::AttackStop {
            attacker,
            victim: v,
        } => {
            assert_eq!((attacker, v), (1, victim));
        }
        _ => panic!("attack stop"),
    }

    // SMSG_ATTACKERSTATEUPDATE (vmangos Unit::SendAttackStateUpdate, Unit.cpp:4572-4605 — decision
    // 0073): HitInfo, attacker PackGUID **first**, victim PackGUID, TotalDamage, *two* sub-damage
    // blocks (absorb summed across both — decision 0137 phase 2's floating combat text feed),
    // VictimState, two u32s (zero + spell id), blocked. The offhand bit (0x4) rides HitInfo.
    let mut body = 0x6u32.to_le_bytes().to_vec(); // HitInfo: 0x2 | 0x4 (offhand)
    body.extend_from_slice(&hx("0101")); // attacker packed guid = 1
    body.extend_from_slice(&hx("c92a4530f1")); // victim packed guid
    body.extend_from_slice(&42u32.to_le_bytes()); // TotalDamage
    body.push(2); // SubDamageCount
    body.extend_from_slice(&0u32.to_le_bytes()); // sub 1: school (physical)
    body.extend_from_slice(&42f32.to_le_bytes()); // sub 1: damage f32
    body.extend_from_slice(&42u32.to_le_bytes()); // sub 1: damage u32
    body.extend_from_slice(&5u32.to_le_bytes()); // sub 1: absorb
    body.extend_from_slice(&0u32.to_le_bytes()); // sub 1: resist
    body.extend_from_slice(&3u32.to_le_bytes()); // sub 2: school (frost)
    body.extend_from_slice(&0f32.to_le_bytes()); // sub 2: damage f32
    body.extend_from_slice(&0u32.to_le_bytes()); // sub 2: damage u32
    body.extend_from_slice(&7u32.to_le_bytes()); // sub 2: absorb
    body.extend_from_slice(&0u32.to_le_bytes()); // sub 2: resist
    body.extend_from_slice(&1u32.to_le_bytes()); // VictimState: hit
    body.extend_from_slice(&0u32.to_le_bytes()); // zero
    body.extend_from_slice(&0u32.to_le_bytes()); // spell id
    body.extend_from_slice(&15u32.to_le_bytes()); // blocked
    match messages::parse_server(messages::opcode::SMSG_ATTACKERSTATEUPDATE, &body).unwrap() {
        ServerPacket::AttackerState(s) => {
            assert_eq!(s.attacker, 1);
            assert_eq!(s.victim, victim);
            assert_eq!(s.hit_info, 0x6);
            assert_eq!(s.damage, 42);
            assert_eq!(s.victim_state, 1);
            assert_eq!(
                s.absorb, 12,
                "absorb summed across both sub-damage blocks (5 + 7)"
            );
            assert_eq!(s.blocked, 15);
        }
        _ => panic!("attacker state"),
    }

    // SMSG_AI_REACTION (vmangos Creature::SendAIReaction → Misc.cpp:445-449): raw (unpacked)
    // guid + reaction u32 — 2 HOSTILE (aggro), 0 ALERT (stealth pre-aggro).
    let body = hx(concat!("3412000000000000", "02000000"));
    let packet = messages::parse_server(messages::opcode::SMSG_AI_REACTION, &body).unwrap();
    match &packet {
        ServerPacket::AiReaction { unit, reaction } => {
            assert_eq!((*unit, *reaction), (0x1234, 2));
        }
        other => panic!("ai reaction, got {}", other.name()),
    }
    match decode(packet).pop().unwrap() {
        SessionEvent::AiReaction { unit, reaction } => assert_eq!((unit, reaction), (0x1234, 2)),
        other => panic!("ai reaction event, got {other:?}"),
    }
}

/// The spell-visual pipeline wire (decision 0099 phase 1): `SMSG_SPELL_START`/`GO`,
/// `SMSG_SPELL_FAILED_OTHER`, `SMSG_CANCEL_AUTO_REPEAT`, `SMSG_PLAY_SPELL_VISUAL`. Golden bytes are
/// hand-built from the vmangos writers (`Spell.cpp:4468-4659`, `SpellCastTargetsInfo.cpp:180-234`,
/// `Spell.cpp:4780-4789`, `Server/Packets/Misc.cpp:548-550`, `Server/Packets/Spell.cpp:54-58`) —
/// there is no existing oracle for these opcodes.
#[test]
fn spell_visual_wire_golden() {
    use benilla_protocol::messages::{SpellCastTargets, SpellGo, SpellStart};

    let creature = 0xF130_0000_4500_002Au64; // packs mask 0xC9 (same fixture guid as elsewhere)

    // SMSG_SPELL_START, a Fireball-style cast: item_or_caster pguid (no cast item, guid 1) + caster
    // pguid (the creature) + spellId + castFlags (CAST_FLAG_UNKNOWN2 only) + remaining cast-time ms
    // + a unit-target SpellCastTargets block. No ammo (castFlags doesn't carry CAST_FLAG_AMMO).
    let body = hx(concat!(
        "0101",       // item_or_caster pguid: mask 1, guid 1
        "c92a4530f1", // caster pguid: the creature guid, mask 0xC9
        "85000000",   // spellId 133
        "0200",       // castFlags 0x2 (CAST_FLAG_UNKNOWN2)
        "b80b0000",   // remaining cast time: 3000ms
        "0200",       // SpellCastTargets mask: TARGET_FLAG_UNIT (0x2)
        "0155",       // unit target pguid: mask 1, guid 0x55
    ));
    let packet = messages::parse_server(messages::opcode::SMSG_SPELL_START, &body).unwrap();
    match &packet {
        ServerPacket::SpellStart(s) => {
            assert_eq!(
                s,
                &SpellStart {
                    item_or_caster: 1,
                    caster: creature,
                    spell_id: 133,
                    cast_flags: 0x2,
                    cast_time_ms: 3000,
                    targets: SpellCastTargets {
                        mask: 0x2,
                        unit_target: Some(0x55),
                        go_target: None,
                        dest: None,
                    },
                    ammo_display_id: None,
                }
            );
        }
        other => panic!("spell start, got {}", other.name()),
    }
    match decode(packet).pop().unwrap() {
        SessionEvent::SpellStart {
            caster,
            spell_id,
            cast_flags,
            cast_time_ms,
            target,
            ammo_display_id,
        } => assert_eq!(
            (
                caster,
                spell_id,
                cast_flags,
                cast_time_ms,
                target,
                ammo_display_id,
            ),
            (creature, 133, 0x2, 3000, Some(0x55), None)
        ),
        other => panic!("spell start event, got {other:?}"),
    }

    // SMSG_SPELL_START, an instant self-cast: cast time 0 (never gets a `Casting` component — the
    // net bridge's insert gate is `cast_time_ms > 0`), target mask 0 (TARGET_FLAG_SELF) — nothing
    // follows the mask.
    let body = hx(concat!(
        "0109",     // item_or_caster pguid: mask 1, guid 9 (no cast item — this is the caster itself)
        "0109",     // caster pguid: same object, guid 9
        "11000000", // spellId 17
        "0200",     // castFlags 0x2
        "00000000", // remaining cast time: 0 (instant)
        "0000",     // SpellCastTargets mask: 0 (SELF) — nothing follows
    ));
    let packet = messages::parse_server(messages::opcode::SMSG_SPELL_START, &body).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::SpellStart {
            caster,
            spell_id,
            cast_time_ms,
            target,
            ..
        } => assert_eq!((caster, spell_id, cast_time_ms, target), (9, 17, 0, None)),
        other => panic!("instant spell start event, got {other:?}"),
    }

    // SMSG_SPELL_GO, a ranged Auto Shot: the same guid pair + spellId + castFlags (UNKNOWN9 |
    // AMMO) + a 2-hit/2-miss target list (one plain miss, one SPELL_MISS_REFLECT with its trailing
    // reflectResult byte) + a unit-target SpellCastTargets block + the ammo block (displayId
    // 5996, inventoryType dropped).
    let body = hx(concat!(
        "0101",             // item_or_caster pguid: guid 1
        "c92a4530f1",       // caster pguid: the creature
        "4b000000",         // spellId 75 (Auto Shot)
        "2001",             // castFlags 0x120 (CAST_FLAG_UNKNOWN9 0x100 | CAST_FLAG_AMMO 0x20)
        "02",               // hit count 2
        "aa00000000000000", // hit guid 0xAA, raw (unpacked)
        "bb00000000000000", // hit guid 0xBB, raw
        "02",               // miss count 2
        "cc00000000000000",
        "02", // miss guid 0xCC, SPELL_MISS_RESIST (2)
        "dd00000000000000",
        "0b",
        "02",       // miss guid 0xDD, SPELL_MISS_REFLECT (11) + reflectResult 2
        "0200",     // SpellCastTargets mask: TARGET_FLAG_UNIT
        "01ee",     // unit target pguid: guid 0xEE
        "6c170000", // ammo displayId 5996
        "12000000", // ammo inventoryType 18 (dropped)
    ));
    let packet = messages::parse_server(messages::opcode::SMSG_SPELL_GO, &body).unwrap();
    match &packet {
        ServerPacket::SpellGo(s) => {
            assert_eq!(
                s,
                &SpellGo {
                    item_or_caster: 1,
                    caster: creature,
                    spell_id: 75,
                    cast_flags: 0x120,
                    hits: vec![0xAA, 0xBB],
                    misses: vec![(0xCC, 2), (0xDD, 11)],
                    targets: SpellCastTargets {
                        mask: 0x2,
                        unit_target: Some(0xEE),
                        go_target: None,
                        dest: None,
                    },
                    ammo_display_id: Some(5996),
                }
            );
        }
        other => panic!("spell go, got {}", other.name()),
    }
    match decode(packet).pop().unwrap() {
        SessionEvent::SpellGo {
            caster,
            spell_id,
            cast_flags,
            hits,
            misses,
            target,
            go_target,
            dest,
            ammo_display_id,
            item_caster,
        } => {
            assert_eq!(caster, creature);
            assert_eq!(spell_id, 75);
            assert_eq!(cast_flags, 0x120);
            assert_eq!(hits, vec![0xAA, 0xBB]);
            assert_eq!(misses, vec![(0xCC, 2), (0xDD, 11)]);
            assert_eq!(target, Some(0xEE));
            assert_eq!(go_target, None); // a unit-targeted cast leaves the GO target empty
            assert_eq!(dest, None); // …and the ground point empty (mask 0x2, no 0x40)
            assert_eq!(ammo_display_id, Some(5996));
            // The packet's first guid (1) differs from the caster — an item cast, surfaced for
            // the item-use cooldown key.
            assert_eq!(item_caster, Some(1));
        }
        other => panic!("spell go event, got {other:?}"),
    }

    // SMSG_SPELL_GO for a GROUND cast (the B132 follow-up): empty hit/miss lists, mask 0x0040 +
    // the dest Vector3d — the exact shape the live vmangos Blizzard capture produced
    // (2026-07-30; the f32 bit patterns below are the captured DYNAMICOBJECT_POS values). The
    // event layer must surface the point — it is the only launch-side record of where the
    // spell went.
    let body = hx(concat!(
        "011a",     // item_or_caster pguid: guid 0x1a (== caster: a plain spell, no item)
        "011a",     // caster pguid
        "0a000000", // spellId 10 (Blizzard)
        "0001",     // castFlags 0x100 (CAST_FLAG_UNKNOWN9)
        "00",       // hit count 0
        "00",       // miss count 0
        "4000",     // SpellCastTargets mask: TARGET_FLAG_DEST_LOCATION
        "8f300ac6", // dest x −8844.14 (0xc60a308f)
        "1f352744", // dest y 668.83 (0x4427351f)
        "b89ec342", // dest z 97.81 (0x42c39eb8)
    ));
    let packet = messages::parse_server(messages::opcode::SMSG_SPELL_GO, &body).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::SpellGo {
            caster,
            spell_id,
            hits,
            misses,
            target,
            dest,
            item_caster,
            ..
        } => {
            assert_eq!((caster, spell_id), (0x1a, 10));
            assert!(hits.is_empty() && misses.is_empty());
            assert_eq!(target, None);
            assert_eq!(
                dest,
                Some([
                    f32::from_bits(0xc60a308f),
                    f32::from_bits(0x4427351f),
                    f32::from_bits(0x42c39eb8),
                ])
            );
            assert_eq!(item_caster, None, "same guid twice — not an item cast");
        }
        other => panic!("ground spell go event, got {other:?}"),
    }

    // SMSG_SPELL_FAILED_OTHER: raw (unpacked) guid + spellId (vmangos `Spell::SendInterrupted`).
    let body = hx(concat!("4200000000000000", "85000000"));
    let packet = messages::parse_server(messages::opcode::SMSG_SPELL_FAILED_OTHER, &body).unwrap();
    match &packet {
        ServerPacket::SpellFailedOther { caster, spell_id } => {
            assert_eq!((*caster, *spell_id), (0x42, 133));
        }
        other => panic!("spell failed other, got {}", other.name()),
    }
    match decode(packet).pop().unwrap() {
        SessionEvent::SpellFailedOther { caster, spell_id } => {
            assert_eq!((caster, spell_id), (0x42, 133));
        }
        other => panic!("spell failed other event, got {other:?}"),
    }

    // SMSG_SPELL_DELAYED: raw (unpacked) guid + pushback ms (vmangos `Spell::Delayed` — `data <<
    // ObjectGuid` is a raw u64, then `u32` delaytime). 500ms = 0x1F4.
    let body = hx(concat!("4200000000000000", "f4010000"));
    let packet = messages::parse_server(messages::opcode::SMSG_SPELL_DELAYED, &body).unwrap();
    match &packet {
        ServerPacket::SpellDelayed { caster, delay_ms } => {
            assert_eq!((*caster, *delay_ms), (0x42, 500));
        }
        other => panic!("spell delayed, got {}", other.name()),
    }
    match decode(packet).pop().unwrap() {
        SessionEvent::SpellDelayed { caster, delay_ms } => {
            assert_eq!((caster, delay_ms), (0x42, 500));
        }
        other => panic!("spell delayed event, got {other:?}"),
    }

    // SMSG_CANCEL_AUTO_REPEAT: empty body (vmangos `CancelAutoRepeat::AppendBodyTo` writes nothing).
    let packet = messages::parse_server(messages::opcode::SMSG_CANCEL_AUTO_REPEAT, &[]).unwrap();
    assert!(matches!(packet, ServerPacket::CancelAutoRepeat));
    assert!(matches!(
        decode(packet).pop().unwrap(),
        SessionEvent::CancelAutoRepeat
    ));

    // SMSG_PLAY_SPELL_VISUAL: raw (unpacked) guid + kit id.
    let body = hx(concat!("3412000000000000", "39000000"));
    let packet = messages::parse_server(messages::opcode::SMSG_PLAY_SPELL_VISUAL, &body).unwrap();
    match &packet {
        ServerPacket::PlaySpellVisual { unit, kit_id } => {
            assert_eq!((*unit, *kit_id), (0x1234, 57));
        }
        other => panic!("play spell visual, got {}", other.name()),
    }
    match decode(packet).pop().unwrap() {
        SessionEvent::PlaySpellVisual { unit, kit_id } => assert_eq!((unit, kit_id), (0x1234, 57)),
        other => panic!("play spell visual event, got {other:?}"),
    }

    // MSG_CHANNEL_START: spellId + duration, both u32 — self-only, no guid on the wire (vmangos
    // `Spell::SendChannelStart`, `Spell.cpp:4951-4954`; decision 0137). 10797 Starshards, 6s.
    let body = hx(concat!("2d2a0000", "70170000"));
    let packet = messages::parse_server(messages::opcode::MSG_CHANNEL_START, &body).unwrap();
    match &packet {
        ServerPacket::ChannelStart {
            spell_id,
            duration_ms,
        } => assert_eq!((*spell_id, *duration_ms), (10797, 6000)),
        other => panic!("channel start, got {}", other.name()),
    }
    match decode(packet).pop().unwrap() {
        SessionEvent::ChannelStart {
            spell_id,
            duration_ms,
        } => assert_eq!((spell_id, duration_ms), (10797, 6000)),
        other => panic!("channel start event, got {other:?}"),
    }

    // MSG_CHANNEL_UPDATE: one u32 time-left (vmangos `Player::SendChannelUpdate`,
    // `Player.cpp:21106-21110`); 0 = the channel is over.
    let body = hx("b80b0000");
    let packet = messages::parse_server(messages::opcode::MSG_CHANNEL_UPDATE, &body).unwrap();
    match &packet {
        ServerPacket::ChannelUpdate { remaining_ms } => assert_eq!(*remaining_ms, 3000),
        other => panic!("channel update, got {}", other.name()),
    }
    match decode(packet).pop().unwrap() {
        SessionEvent::ChannelUpdate { remaining_ms } => assert_eq!(remaining_ms, 3000),
        other => panic!("channel update event, got {other:?}"),
    }

    // Truncated buffers fail cleanly rather than silently short-reading (the codec's usual
    // contract — every reader is `Read::read_exact`-backed, which errors on a short buffer).
    let body = hx(concat!(
        "0101",
        "c92a4530f1",
        "85000000",
        "0200", // truncated before the u32 cast-time
    ));
    assert!(messages::parse_server(messages::opcode::SMSG_SPELL_START, &body).is_err());
    let body = hx(concat!(
        "0101",
        "c92a4530f1",
        "4b000000",
        "2001",
        "02",
        "aa00000000000000", // hit count says 2, only 1 hit guid present
    ));
    assert!(messages::parse_server(messages::opcode::SMSG_SPELL_GO, &body).is_err());
}

/// An open-lock cast on a chest / locked door: `SMSG_SPELL_GO` whose `SpellCastTargets` names a
/// GameObject (`TARGET_FLAG_GAMEOBJECT`, no unit target). The decoder must surface the GO guid so the
/// lid/door animation can open it (decision 0250) — the guid the older decode read for alignment and
/// dropped.
#[test]
fn spell_go_surfaces_the_gameobject_target() {
    let body = hx(concat!(
        "0101",     // item_or_caster pguid: guid 1
        "0107",     // caster pguid: guid 7
        "250d0000", // spellId 3365 ("Opening")
        "0001",     // castFlags 0x100 (CAST_FLAG_UNKNOWN9, no ammo)
        "00",       // hit count 0
        "00",       // miss count 0
        "0008",     // SpellCastTargets mask: TARGET_FLAG_GAMEOBJECT (0x0800)
        "033412",   // GO target pguid: guid 0x1234
    ));
    let packet = messages::parse_server(messages::opcode::SMSG_SPELL_GO, &body).unwrap();
    match &packet {
        ServerPacket::SpellGo(s) => {
            assert_eq!(s.targets.go_target, Some(0x1234));
            assert_eq!(s.targets.unit_target, None);
        }
        other => panic!("spell go, got {}", other.name()),
    }
    match decode(packet).pop().unwrap() {
        SessionEvent::SpellGo {
            spell_id,
            go_target,
            target,
            ..
        } => {
            assert_eq!(spell_id, 3365);
            assert_eq!(go_target, Some(0x1234)); // the chest the open-lock cast will pop
            assert_eq!(target, None);
        }
        other => panic!("spell go event, got {other:?}"),
    }
}

/// The combat-log wire (decision 0137 phase 2's floating-combat-text data feed):
/// `SMSG_SPELLNONMELEEDAMAGELOG`, `SMSG_PERIODICAURALOG`, `SMSG_SPELLDAMAGESHIELD`,
/// `SMSG_SPELLLOGMISS`, `SMSG_LOG_XPGAIN`, `SMSG_LEVELUP_INFO`. Golden bytes are hand-built from
/// the vmangos writers (`Server/Packets/Spell.cpp:68-86,124-140`, `Unit.cpp:4395-4443`,
/// `Server/Packets/Combat.cpp:73-79`, `Server/Packets/Misc.cpp:512-532`) — there is no existing
/// oracle for these opcodes.
#[test]
fn combat_log_wire_golden() {
    use benilla_protocol::messages::{
        DamageShield, EnvironmentalDamageLog, ExplorationXp, LevelUpInfo, PeriodicAuraLog,
        PeriodicTick, SpellDamageLog, SpellLogMiss, XpGain,
    };

    let creature = 0xF130_0000_4500_002Au64; // packs mask 0xC9 (same fixture guid as elsewhere)

    // SMSG_SPELLNONMELEEDAMAGELOG: target PackedGuid, attacker PackedGuid, spellId, damage, school
    // u8, absorbed, resist i32, periodicLog u8 (bool), unused u8, blocked, hitInfo, extendedData u8
    // (always 0, dropped). hitInfo 0x2 = SPELL_HIT_TYPE_CRIT.
    let body = hx(concat!(
        "c92a4530f1", // target pguid: the creature
        "0101",       // attacker pguid: guid 1
        "85000000",   // spellId 133
        "f4010000",   // damage 500
        "03",         // school 3 (frost)
        "32000000",   // absorbed 50
        "ecffffff",   // resist -20
        "00",         // periodicLog: false (direct, not a DoT tick)
        "00",         // unused
        "0a000000",   // blocked 10
        "02000000",   // hitInfo 0x2 (crit)
        "00",         // extendedData (always 0, dropped)
    ));
    let packet =
        messages::parse_server(messages::opcode::SMSG_SPELLNONMELEEDAMAGELOG, &body).unwrap();
    match &packet {
        ServerPacket::SpellDamageLog(s) => {
            assert_eq!(
                s,
                &SpellDamageLog {
                    target: creature,
                    attacker: 1,
                    spell_id: 133,
                    damage: 500,
                    school: 3,
                    absorb: 50,
                    resist: -20,
                    periodic: false,
                    blocked: 10,
                    hit_info: 0x2,
                }
            );
        }
        other => panic!("spell damage log, got {}", other.name()),
    }
    match decode(packet).pop().unwrap() {
        SessionEvent::SpellDamageLog(s) => assert_eq!(s.hit_info & 0x2, 0x2, "crit bit"),
        other => panic!("spell damage log event, got {other:?}"),
    }

    // SMSG_PERIODICAURALOG: target PackedGuid, caster PackedGuid, spellId, count, then `count` x
    // {auraType u32, payload}. First vector: one PERIODIC_DAMAGE (3) tick.
    let body = hx(concat!(
        "c92a4530f1", // target pguid: the creature
        "0101",       // caster pguid: guid 1
        "ac000000",   // spellId 172 (Corruption)
        "01000000",   // count 1
        "03000000",   // auraType 3 (PERIODIC_DAMAGE)
        "58000000",   // damage 88
        "06000000",   // school 6 (shadow)
        "0c000000",   // absorb 12
        "fbffffff",   // resist -5
    ));
    let packet = messages::parse_server(messages::opcode::SMSG_PERIODICAURALOG, &body).unwrap();
    match &packet {
        ServerPacket::PeriodicAuraLog(l) => {
            assert_eq!(
                l,
                &PeriodicAuraLog {
                    target: creature,
                    caster: 1,
                    spell_id: 172,
                    ticks: vec![PeriodicTick::Damage {
                        amount: 88,
                        school: 6,
                        absorb: 12,
                        resist: -5,
                    }],
                }
            );
        }
        other => panic!("periodic aura log (damage), got {}", other.name()),
    }
    match decode(packet).pop().unwrap() {
        SessionEvent::PeriodicAuraLog(l) => assert_eq!(l.ticks.len(), 1),
        other => panic!("periodic aura log event, got {other:?}"),
    }

    // Second vector: one PERIODIC_HEAL (8) tick — a self-heal (target == caster).
    let body = hx(concat!(
        "0101",     // target pguid: guid 1
        "0101",     // caster pguid: guid 1 (self)
        "59000000", // spellId 89 (Renew)
        "01000000", // count 1
        "08000000", // auraType 8 (PERIODIC_HEAL)
        "2d000000", // amount 45
    ));
    let packet = messages::parse_server(messages::opcode::SMSG_PERIODICAURALOG, &body).unwrap();
    match &packet {
        ServerPacket::PeriodicAuraLog(l) => {
            assert_eq!(
                l,
                &PeriodicAuraLog {
                    target: 1,
                    caster: 1,
                    spell_id: 89,
                    ticks: vec![PeriodicTick::Heal { amount: 45 }],
                }
            );
        }
        other => panic!("periodic aura log (heal), got {}", other.name()),
    }

    // Third vector: an unrecognized auraType (4) cannot be skipped without desyncing the stream —
    // decode must fail rather than guess a payload width.
    let body = hx(concat!(
        "0101",     // target
        "0101",     // caster
        "01000000", // spellId 1
        "01000000", // count 1
        "04000000", // auraType 4 (unhandled)
    ));
    assert!(messages::parse_server(messages::opcode::SMSG_PERIODICAURALOG, &body).is_err());

    // SMSG_SPELLDAMAGESHIELD: victim raw guid, attacker raw guid, damage, school — all four fields
    // distinct. `victim` is the shield's bearer; `attacker` receives the damage back.
    let body = hx(concat!(
        "1111000000000000", // victim raw guid 0x1111
        "2222000000000000", // attacker raw guid 0x2222
        "21000000",         // damage 33
        "04000000",         // school 4 (nature)
    ));
    let packet = messages::parse_server(messages::opcode::SMSG_SPELLDAMAGESHIELD, &body).unwrap();
    match &packet {
        ServerPacket::DamageShield(s) => {
            assert_eq!(
                s,
                &DamageShield {
                    victim: 0x1111,
                    attacker: 0x2222,
                    damage: 33,
                    school: 4,
                }
            );
        }
        other => panic!("damage shield, got {}", other.name()),
    }
    match decode(packet).pop().unwrap() {
        SessionEvent::DamageShield(s) => assert_eq!((s.victim, s.attacker), (0x1111, 0x2222)),
        other => panic!("damage shield event, got {other:?}"),
    }

    // SMSG_ENVIRONMENTALDAMAGELOG: victim raw guid (vmangos `ObjectGuid.cpp:174` streams the raw
    // u64, NOT a PackedGuid), damageType u8, damage, absorbed, resist i32 (the > 1.6.1 tail,
    // `Server/Packets/Combat.cpp:58-67`). Type 2 = fall.
    let body = hx(concat!(
        "0300000000000000", // victim raw guid 3
        "02",               // damageType 2 (DAMAGE_FALL)
        "17000000",         // damage 23
        "05000000",         // absorbed 5
        "ffffffff",         // resist -1
    ));
    let packet =
        messages::parse_server(messages::opcode::SMSG_ENVIRONMENTALDAMAGELOG, &body).unwrap();
    match &packet {
        ServerPacket::EnvironmentalDamageLog(e) => {
            assert_eq!(
                e,
                &EnvironmentalDamageLog {
                    victim: 3,
                    damage_type: 2,
                    damage: 23,
                    absorb: 5,
                    resist: -1,
                }
            );
        }
        other => panic!("environmental damage log, got {}", other.name()),
    }
    match decode(packet).pop().unwrap() {
        SessionEvent::EnvironmentalDamageLog(e) => {
            assert_eq!((e.victim, e.damage_type, e.damage), (3, 2, 23));
        }
        other => panic!("environmental damage log event, got {other:?}"),
    }

    // SMSG_SPELLLOGMISS: spellId, caster raw guid, useExtended u8 (0 here — no trailing 2xf32 per
    // entry), count, then `count` x {target raw guid, missInfo u8}. Two entries: DODGE (3) and
    // REFLECT (11) — the reflect trailing byte only rides SMSG_SPELL_GO's miss list, not this one.
    let body = hx(concat!(
        "85000000",         // spellId 133
        "4200000000000000", // caster raw guid 0x42
        "00",               // useExtended: 0
        "02000000",         // count 2
        "aa00000000000000", // target 0xAA
        "03",               // missInfo 3 (DODGE)
        "bb00000000000000", // target 0xBB
        "0b",               // missInfo 11 (REFLECT)
    ));
    let packet = messages::parse_server(messages::opcode::SMSG_SPELLLOGMISS, &body).unwrap();
    match &packet {
        ServerPacket::SpellLogMiss(s) => {
            assert_eq!(
                s,
                &SpellLogMiss {
                    spell_id: 133,
                    caster: 0x42,
                    misses: vec![(0xAA, 3), (0xBB, 11)],
                }
            );
        }
        other => panic!("spell log miss, got {}", other.name()),
    }
    match decode(packet).pop().unwrap() {
        SessionEvent::SpellLogMiss(s) => assert_eq!(s.misses, vec![(0xAA, 3), (0xBB, 11)]),
        other => panic!("spell log miss event, got {other:?}"),
    }

    // SMSG_LOG_XPGAIN: victim raw guid (0 for non-kill) + total + xpType (0 kill, carries trailing
    // base+bonus; 1 non-kill, nothing trailing). Both vectors consume exactly their own body — a
    // kill's trailing base/bonus is present, a non-kill's is absent.
    let mut body = 0xCCu64.to_le_bytes().to_vec(); // victim
    body.extend_from_slice(&250u32.to_le_bytes()); // total
    body.push(0); // xpType: kill
    body.extend_from_slice(&200u32.to_le_bytes()); // base xp (kept: rested bonus = total − base)
    body.extend_from_slice(&1.25f32.to_le_bytes()); // group bonus (dropped)
    let packet = messages::parse_server(messages::opcode::SMSG_LOG_XPGAIN, &body).unwrap();
    match &packet {
        ServerPacket::XpGain(x) => {
            assert_eq!(
                x,
                &XpGain {
                    victim: 0xCC,
                    total: 250,
                    base: 200,
                    kill: true,
                }
            );
        }
        other => panic!("xp gain (kill), got {}", other.name()),
    }
    match decode(packet).pop().unwrap() {
        SessionEvent::XpGain(x) => assert_eq!((x.victim, x.total, x.kill), (0xCC, 250, true)),
        other => panic!("xp gain event, got {other:?}"),
    }

    let mut body = 0u64.to_le_bytes().to_vec(); // victim: 0 (non-kill xp has no victim)
    body.extend_from_slice(&50u32.to_le_bytes()); // total
    body.push(1); // xpType: non-kill — nothing trails
    let packet = messages::parse_server(messages::opcode::SMSG_LOG_XPGAIN, &body).unwrap();
    match &packet {
        ServerPacket::XpGain(x) => {
            assert_eq!(
                x,
                &XpGain {
                    victim: 0,
                    total: 50,
                    base: 50, // non-kill carries no base on the wire; base = total
                    kill: false,
                }
            );
        }
        other => panic!("xp gain (non-kill), got {}", other.name()),
    }
    match decode(packet).pop().unwrap() {
        SessionEvent::XpGain(x) => assert_eq!((x.victim, x.total, x.kill), (0, 50, false)),
        other => panic!("xp gain event, got {other:?}"),
    }

    // SMSG_EXPLORATION_EXPERIENCE: areaId u32 (an AreaTable.dbc row id) + xp u32 — exactly
    // 8 bytes (VERIFIED vmangos Misc.cpp:552-556; sent even when xp is 0). The vector is
    // Westfall (area 40) worth 85 xp.
    let mut body = 40u32.to_le_bytes().to_vec();
    body.extend_from_slice(&85u32.to_le_bytes());
    assert_eq!(body.len(), 8);
    let packet =
        messages::parse_server(messages::opcode::SMSG_EXPLORATION_EXPERIENCE, &body).unwrap();
    match &packet {
        ServerPacket::ExplorationXp(x) => {
            assert_eq!(
                x,
                &ExplorationXp {
                    area_id: 40,
                    xp: 85,
                }
            );
        }
        other => panic!("exploration xp, got {}", other.name()),
    }
    match decode(packet).pop().unwrap() {
        SessionEvent::ExplorationXp(x) => assert_eq!((x.area_id, x.xp), (40, 85)),
        other => panic!("exploration xp event, got {other:?}"),
    }

    // SMSG_LEVELUP_INFO: twelve u32 — level, healthGain, powerGains[5] (mana..happiness),
    // statGains[5] (str..spirit). VERIFIED vmangos Misc.cpp:524-532; exactly 48 bytes, no guid
    // (self-addressed). The vector is a caster-flavored ding: mana + int/spirit heavy.
    let mut body = Vec::new();
    for v in [7u32, 22, 15, 0, 0, 0, 0, 1, 0, 1, 2, 1] {
        body.extend_from_slice(&v.to_le_bytes());
    }
    assert_eq!(body.len(), 48);
    let packet = messages::parse_server(messages::opcode::SMSG_LEVELUP_INFO, &body).unwrap();
    match &packet {
        ServerPacket::LevelUp(l) => {
            assert_eq!(
                l,
                &LevelUpInfo {
                    level: 7,
                    health: 22,
                    powers: [15, 0, 0, 0, 0],
                    stats: [1, 0, 1, 2, 1],
                }
            );
        }
        other => panic!("level up, got {}", other.name()),
    }
    match decode(packet).pop().unwrap() {
        SessionEvent::LevelUp(l) => assert_eq!((l.level, l.health, l.powers[0]), (7, 22, 15)),
        other => panic!("level up event, got {other:?}"),
    }
}
