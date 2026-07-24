//! Golden tests for the group/party opcode family — invite/accept/decline/kick/leader/disband, the
//! loot method, the roster push (`SMSG_GROUP_LIST`), party command feedback, live member stats for
//! the party/raid frame, minimap pings, raid subgroup management, raid-target icons, and ready
//! checks. CMSG bodies are asserted byte-exact against the builder output; SMSG bodies are hand-built
//! per the vmangos layouts (cited inline) and round-tripped through `parse_server` + `decode`. See
//! `tests/common` for the shared `hx()` fixture helper and methodology note.

mod common;

use benilla_protocol::events::{decode, SessionEvent};
use benilla_protocol::messages::{
    self, member_status, opcode, party_member_mask, party_operation, party_result, GroupLootInfo,
    GroupMemberEntry, PartyMemberStatsInfo, GROUP_MEMBER_ASSISTANT,
};
use benilla_protocol::ServerPacket;
use common::hx;

/// Every CMSG builder in the family, byte-exact — including the empty-body ones (asserting an
/// empty `Vec`) and the `raid_target_set` vs `raid_target_request` distinction.
#[test]
fn cmsg_bodies_golden() {
    let guid = 0x1234_5678_9abc_def0u64;
    let guid_hex = "f0debc9a78563412";

    // CMSG_GROUP_INVITE / CMSG_GROUP_UNINVITE (vmangos Group.cpp:4-7 / 9-12): one cstring.
    assert_eq!(
        messages::group_invite("Bob"),
        hx("426f6200"),
        "CMSG_GROUP_INVITE body"
    );
    assert_eq!(
        messages::group_uninvite("Bob"),
        hx("426f6200"),
        "CMSG_GROUP_UNINVITE body"
    );

    // CMSG_GROUP_ACCEPT / DECLINE / DISBAND / RAID_CONVERT (vmangos Opcodes.cpp: NullClientPacket):
    // all empty.
    assert_eq!(
        messages::group_accept(),
        Vec::<u8>::new(),
        "CMSG_GROUP_ACCEPT body"
    );
    assert_eq!(
        messages::group_decline(),
        Vec::<u8>::new(),
        "CMSG_GROUP_DECLINE body"
    );
    assert_eq!(
        messages::group_disband(),
        Vec::<u8>::new(),
        "CMSG_GROUP_DISBAND body"
    );
    assert_eq!(
        messages::group_raid_convert(),
        Vec::<u8>::new(),
        "CMSG_GROUP_RAID_CONVERT body"
    );

    // CMSG_GROUP_UNINVITE_GUID / CMSG_GROUP_SET_LEADER / CMSG_REQUEST_PARTY_MEMBER_STATS: a full guid.
    assert_eq!(
        messages::group_uninvite_guid(guid),
        hx(guid_hex),
        "CMSG_GROUP_UNINVITE_GUID body"
    );
    assert_eq!(
        messages::group_set_leader(guid),
        hx(guid_hex),
        "CMSG_GROUP_SET_LEADER body"
    );
    assert_eq!(
        messages::request_party_member_stats(guid),
        hx(guid_hex),
        "CMSG_REQUEST_PARTY_MEMBER_STATS body"
    );

    // CMSG_LOOT_METHOD (Group.cpp:26-31): u32 method, full guid lootMaster, u32 threshold.
    assert_eq!(
        messages::loot_method(2, guid, 3),
        hx(concat!("02000000", "f0debc9a78563412", "03000000")),
        "CMSG_LOOT_METHOD body"
    );

    // CMSG_GROUP_CHANGE_SUB_GROUP (Group.cpp:45-49): cstring name + u8 groupNr.
    assert_eq!(
        messages::group_change_sub_group("Bar", 5),
        hx("4261720005"),
        "CMSG_GROUP_CHANGE_SUB_GROUP body"
    );

    // CMSG_GROUP_SWAP_SUB_GROUP (Group.cpp:51-55): cstring name + cstring nameSwapWith.
    assert_eq!(
        messages::group_swap_sub_group("A", "B"),
        hx("41004200"),
        "CMSG_GROUP_SWAP_SUB_GROUP body"
    );

    // CMSG_GROUP_ASSISTANT_LEADER (Group.cpp:66-74, 1.12 guid branch): full guid + u8 flag.
    assert_eq!(
        messages::group_assistant_leader(guid, true),
        hx(concat!("f0debc9a78563412", "01")),
        "CMSG_GROUP_ASSISTANT_LEADER body, grant"
    );
    assert_eq!(
        messages::group_assistant_leader(guid, false),
        hx(concat!("f0debc9a78563412", "00")),
        "CMSG_GROUP_ASSISTANT_LEADER body, revoke"
    );

    // MSG_MINIMAP_PING, outbound (Group.cpp:33-37): f32 x, f32 y — no guid.
    assert_eq!(
        messages::minimap_ping(1.0, 2.0),
        hx("0000803f00000040"),
        "MSG_MINIMAP_PING outbound body"
    );

    // MSG_RAID_TARGET_UPDATE, client bodies (Group.cpp:77-82): icon+guid to set/clear one icon, a
    // lone 0xFF to ask for the current set — the two builders are never interchangeable byte-shapes.
    assert_eq!(
        messages::raid_target_set(3, guid),
        hx(concat!("03", "f0debc9a78563412")),
        "raid_target_set body"
    );
    assert_eq!(
        messages::raid_target_set(1, 0),
        hx(concat!("01", "0000000000000000")),
        "raid_target_set clear (guid 0) body"
    );
    assert_eq!(
        messages::raid_target_request(),
        hx("ff"),
        "raid_target_request body"
    );

    // MSG_RAID_READY_CHECK, client bodies (Group.cpp:84-92): empty to start, one byte to answer.
    assert_eq!(
        messages::ready_check_start(),
        Vec::<u8>::new(),
        "ready_check_start body"
    );
    assert_eq!(
        messages::ready_check_answer(true),
        hx("01"),
        "ready_check_answer(true) body"
    );
    assert_eq!(
        messages::ready_check_answer(false),
        hx("00"),
        "ready_check_answer(false) body"
    );
}

/// The name/notice trio (`SMSG_GROUP_INVITE`/`_DECLINE`/`_SET_LEADER`, one cstring each) plus the
/// two empty-body notices (`SMSG_GROUP_UNINVITE`/`_DESTROYED`).
#[test]
fn group_notification_smsg_wire() {
    // SMSG_GROUP_INVITE (Group.cpp:107-110): one cstring, the inviter's name.
    let p = messages::parse_server(opcode::SMSG_GROUP_INVITE, &hx("426f6200")).unwrap();
    match &p {
        ServerPacket::GroupInvite { inviter } => assert_eq!(inviter, "Bob"),
        other => panic!("expected GroupInvite, got {}", other.name()),
    }
    match decode(p).as_slice() {
        [SessionEvent::GroupInvite { inviter }] => assert_eq!(inviter, "Bob"),
        other => panic!("group invite decode: {other:?}"),
    }

    // SMSG_GROUP_DECLINE (Group.cpp:112-115): one cstring, the declining player's name.
    let p = messages::parse_server(opcode::SMSG_GROUP_DECLINE, &hx("416c69636500")).unwrap();
    match &p {
        ServerPacket::GroupDecline { name } => assert_eq!(name, "Alice"),
        other => panic!("expected GroupDecline, got {}", other.name()),
    }
    match decode(p).as_slice() {
        [SessionEvent::GroupDecline { name }] => assert_eq!(name, "Alice"),
        other => panic!("group decline decode: {other:?}"),
    }

    // SMSG_GROUP_SET_LEADER (Group.cpp:150-153): one cstring, the new leader's name.
    let p = messages::parse_server(opcode::SMSG_GROUP_SET_LEADER, &hx("426f6200")).unwrap();
    match &p {
        ServerPacket::GroupLeaderChanged { name } => assert_eq!(name, "Bob"),
        other => panic!("expected GroupLeaderChanged, got {}", other.name()),
    }
    match decode(p).as_slice() {
        [SessionEvent::GroupLeaderChanged { name }] => assert_eq!(name, "Bob"),
        other => panic!("group leader changed decode: {other:?}"),
    }

    // SMSG_GROUP_UNINVITE (Group.cpp:117-119): empty body — we were kicked/left.
    let p = messages::parse_server(opcode::SMSG_GROUP_UNINVITE, &[]).unwrap();
    assert!(matches!(p, ServerPacket::GroupUninvited));
    assert!(matches!(
        decode(p).as_slice(),
        [SessionEvent::GroupUninvited]
    ));

    // SMSG_GROUP_DESTROYED (Group.cpp:121-123): empty body — the group disbanded outright.
    let p = messages::parse_server(opcode::SMSG_GROUP_DESTROYED, &[]).unwrap();
    assert!(matches!(p, ServerPacket::GroupDestroyed));
    assert!(matches!(
        decode(p).as_slice(),
        [SessionEvent::GroupDestroyed]
    ));
}

/// `SMSG_GROUP_LIST`, party shape: 2 other members + the full loot tail (master loot, so
/// `looterGuid` carries the master's guid).
#[test]
fn group_list_party_two_members_master_loot() {
    // Group.cpp:155-180 (GroupList::AppendBodyTo): groupType, ownGroupAndAssistantFlag,
    // memberCount, memberCount x member rows, leaderGuid, then (memberCount > 0) the loot tail.
    let mut body = vec![0u8]; // groupType: party
    body.push(0x00); // ownGroupAndAssistantFlag: subgroup 0, not assistant
    body.extend_from_slice(&2u32.to_le_bytes()); // memberCount

    body.extend_from_slice(b"Alice\0");
    body.extend_from_slice(&0x1111u64.to_le_bytes());
    body.push(member_status::ONLINE);
    body.push(0x00);

    body.extend_from_slice(b"Carol\0");
    body.extend_from_slice(&0x3333u64.to_le_bytes());
    body.push(member_status::ONLINE | member_status::DEAD);
    body.push(0x00);

    body.extend_from_slice(&0x1111u64.to_le_bytes()); // leaderGuid: Alice

    body.push(2); // lootMethod: master loot
    body.extend_from_slice(&0x3333u64.to_le_bytes()); // looterGuid: Carol is master looter
    body.push(3); // lootThreshold
    body.push(0); // dungeonDifficulty (always 0 at 5875)

    let p = messages::parse_server(opcode::SMSG_GROUP_LIST, &body).unwrap();
    match &p {
        ServerPacket::GroupList {
            group_type,
            own_flags,
            members,
            leader,
            loot,
        } => {
            assert_eq!(*group_type, 0);
            assert_eq!(*own_flags, 0x00);
            assert_eq!(
                members,
                &vec![
                    GroupMemberEntry {
                        name: "Alice".into(),
                        guid: 0x1111,
                        status: member_status::ONLINE,
                        flags: 0x00,
                    },
                    GroupMemberEntry {
                        name: "Carol".into(),
                        guid: 0x3333,
                        status: member_status::ONLINE | member_status::DEAD,
                        flags: 0x00,
                    },
                ]
            );
            assert_eq!(*leader, 0x1111);
            assert_eq!(
                *loot,
                Some(GroupLootInfo {
                    method: 2,
                    master: 0x3333,
                    threshold: 3,
                })
            );
        }
        other => panic!("expected GroupList, got {}", other.name()),
    }

    match decode(p).as_slice() {
        [SessionEvent::GroupList {
            group_type: 0,
            members,
            leader: 0x1111,
            loot: Some(loot),
            ..
        }] => {
            assert_eq!(members.len(), 2);
            assert_eq!(loot.master, 0x3333);
        }
        other => panic!("group list decode: {other:?}"),
    }
}

/// `SMSG_GROUP_LIST`, raid shape: `groupType == 1`, one member carrying the raid-assistant bit
/// (`0x80`), round-robin loot (so `looterGuid` is `0` — vmangos only fills it for master loot).
#[test]
fn group_list_raid_with_assistant_flag() {
    let mut body = vec![1u8]; // groupType: raid
    body.push(0x02); // ownGroupAndAssistantFlag: subgroup 2, not assistant
    body.extend_from_slice(&1u32.to_le_bytes()); // memberCount

    body.extend_from_slice(b"Dave\0");
    body.extend_from_slice(&0x4444u64.to_le_bytes());
    body.push(member_status::ONLINE);
    body.push(GROUP_MEMBER_ASSISTANT | 0x01); // subgroup 1, assistant

    body.extend_from_slice(&0x4444u64.to_le_bytes()); // leaderGuid: Dave

    body.push(1); // lootMethod: round-robin
    body.extend_from_slice(&0u64.to_le_bytes()); // looterGuid: 0 (not master loot)
    body.push(2); // lootThreshold
    body.push(0); // dungeonDifficulty

    let p = messages::parse_server(opcode::SMSG_GROUP_LIST, &body).unwrap();
    match &p {
        ServerPacket::GroupList {
            group_type,
            own_flags,
            members,
            leader,
            loot,
        } => {
            assert_eq!(*group_type, 1);
            assert_eq!(*own_flags, 0x02);
            assert_eq!(
                members,
                &vec![GroupMemberEntry {
                    name: "Dave".into(),
                    guid: 0x4444,
                    status: member_status::ONLINE,
                    flags: GROUP_MEMBER_ASSISTANT | 0x01,
                }]
            );
            assert!(members[0].flags & GROUP_MEMBER_ASSISTANT != 0);
            assert_eq!(*leader, 0x4444);
            assert_eq!(
                *loot,
                Some(GroupLootInfo {
                    method: 1,
                    master: 0,
                    threshold: 2,
                })
            );
        }
        other => panic!("expected GroupList, got {}", other.name()),
    }
}

/// The degenerate "you left the group" shape (`Group.cpp:155-180` with an empty member list):
/// exactly 14 bytes (`groupType, ownFlags, u32 memberCount=0, u64 leaderGuid=0`), no loot tail.
#[test]
fn group_list_empty_you_left_shape_is_14_bytes() {
    let body = vec![0u8; 14];
    assert_eq!(body.len(), 14);

    let p = messages::parse_server(opcode::SMSG_GROUP_LIST, &body).unwrap();
    match &p {
        ServerPacket::GroupList {
            group_type,
            own_flags,
            members,
            leader,
            loot,
        } => {
            assert_eq!(*group_type, 0);
            assert_eq!(*own_flags, 0);
            assert!(members.is_empty());
            assert_eq!(*leader, 0);
            assert!(loot.is_none(), "empty member list carries no loot tail");
        }
        other => panic!("expected GroupList, got {}", other.name()),
    }

    match decode(p).as_slice() {
        [SessionEvent::GroupList {
            members,
            loot: None,
            ..
        }] => assert!(members.is_empty()),
        other => panic!("empty group list decode: {other:?}"),
    }
}

/// `SMSG_PARTY_COMMAND_RESULT` (Group.cpp:100-105): a named refusal, and the ignoring-you refusal
/// which names no one (`Handlers/GroupHandler.cpp:466`: `SendPartyResult(PARTY_OP_INVITE, "", ...)`).
#[test]
fn party_command_result_wire() {
    let mut body = party_operation::INVITE.to_le_bytes().to_vec();
    body.extend_from_slice(b"Bob\0");
    body.extend_from_slice(&party_result::ALREADY_IN_GROUP.to_le_bytes());
    let p = messages::parse_server(opcode::SMSG_PARTY_COMMAND_RESULT, &body).unwrap();
    match &p {
        ServerPacket::PartyCommandResult {
            operation,
            member,
            result,
        } => {
            assert_eq!(*operation, party_operation::INVITE);
            assert_eq!(member, "Bob");
            assert_eq!(*result, party_result::ALREADY_IN_GROUP);
        }
        other => panic!("expected PartyCommandResult, got {}", other.name()),
    }
    match decode(p).as_slice() {
        [SessionEvent::PartyCommandResult { member, .. }] => assert_eq!(member, "Bob"),
        other => panic!("party command result decode: {other:?}"),
    }

    let mut body = party_operation::LEAVE.to_le_bytes().to_vec();
    body.push(0); // empty cstring: no member named
    body.extend_from_slice(&party_result::IGNORING_YOU.to_le_bytes());
    let p = messages::parse_server(opcode::SMSG_PARTY_COMMAND_RESULT, &body).unwrap();
    match &p {
        ServerPacket::PartyCommandResult {
            operation,
            member,
            result,
        } => {
            assert_eq!(*operation, party_operation::LEAVE);
            assert!(member.is_empty());
            assert_eq!(*result, party_result::IGNORING_YOU);
        }
        other => panic!("expected PartyCommandResult, got {}", other.name()),
    }
}

/// `SMSG_PARTY_MEMBER_STATS` (delta form): mask `0x000000FF` — every bit from `STATUS` through
/// `ZONE` — hand-computed body, the plain (non-`_FULL`) opcode.
#[test]
fn party_member_stats_delta_status_through_zone() {
    let body = hx(concat!(
        "012a",     // packed guid 0x2A (mask 0x01, one nonzero byte)
        "ff000000", // mask: STATUS|CUR_HP|MAX_HP|POWER_TYPE|CUR_POWER|MAX_POWER|LEVEL|ZONE
        "01",       // status: ONLINE
        "6400",     // cur_hp 100
        "9600",     // max_hp 150
        "00",       // power_type: mana
        "c800",     // cur_power 200
        "fa00",     // max_power 250
        "3c00",     // level 60
        "ef05",     // zone 1519
    ));
    let p = messages::parse_server(opcode::SMSG_PARTY_MEMBER_STATS, &body).unwrap();
    match &p {
        ServerPacket::PartyMemberStats { guid, full, info } => {
            assert_eq!(*guid, 0x2A);
            assert!(!*full);
            assert_eq!(
                **info,
                PartyMemberStatsInfo {
                    status: Some(member_status::ONLINE),
                    cur_hp: Some(100),
                    max_hp: Some(150),
                    power_type: Some(0),
                    cur_power: Some(200),
                    max_power: Some(250),
                    level: Some(60),
                    zone: Some(1519),
                    ..Default::default()
                }
            );
        }
        other => panic!("expected PartyMemberStats, got {}", other.name()),
    }
    match decode(p).as_slice() {
        [SessionEvent::PartyMemberStats {
            guid: 0x2A,
            full: false,
            info,
        }] => assert_eq!(info.level, Some(60)),
        other => panic!("party member stats delta decode: {other:?}"),
    }
}

/// `SMSG_PARTY_MEMBER_STATS_FULL`: `POSITION` + `AURAS` (2 bits set) + `AURAS_NEGATIVE` (1 bit) +
/// the whole pet block (`PET_GUID`..`PET_AURAS_NEGATIVE`, including `PET_AURAS`) — every remaining
/// field family the delta test above didn't touch.
#[test]
fn party_member_stats_full_position_auras_and_pet_block() {
    let mask = party_member_mask::POSITION
        | party_member_mask::AURAS
        | party_member_mask::AURAS_NEGATIVE
        | party_member_mask::PET_GUID
        | party_member_mask::PET_NAME
        | party_member_mask::PET_MODEL_ID
        | party_member_mask::PET_CUR_HP
        | party_member_mask::PET_MAX_HP
        | party_member_mask::PET_POWER_TYPE
        | party_member_mask::PET_CUR_POWER
        | party_member_mask::PET_MAX_POWER
        | party_member_mask::PET_AURAS
        | party_member_mask::PET_AURAS_NEGATIVE;
    assert_eq!(mask, 0x001F_FF00, "sanity: the hand-picked bit set");

    let mut body = vec![0x01, 0x7F]; // packed guid 0x7F (mask 0x01, one nonzero byte)
    body.extend_from_slice(&mask.to_le_bytes());

    // POSITION: i16 x, i16 y (negative, to prove the signed cast).
    body.extend_from_slice(&1234i16.to_le_bytes());
    body.extend_from_slice(&(-5678i16).to_le_bytes());
    // AURAS: u32 posMask, bits 0 and 5 set -> spell ids 133 (Fireball), 116 (Frostbolt).
    body.extend_from_slice(&0x0000_0021u32.to_le_bytes());
    body.extend_from_slice(&133u16.to_le_bytes());
    body.extend_from_slice(&116u16.to_le_bytes());
    // AURAS_NEGATIVE: u16 negMask, bit 2 set -> spell id 8050.
    body.extend_from_slice(&0x0004u16.to_le_bytes());
    body.extend_from_slice(&8050u16.to_le_bytes());
    // PET_GUID.
    body.extend_from_slice(&0x1122_3344_5566_7788u64.to_le_bytes());
    // PET_NAME.
    body.extend_from_slice(b"Fido\0");
    // PET_MODEL_ID, PET_CUR_HP, PET_MAX_HP.
    body.extend_from_slice(&618u16.to_le_bytes());
    body.extend_from_slice(&40u16.to_le_bytes());
    body.extend_from_slice(&50u16.to_le_bytes());
    // PET_POWER_TYPE: mana.
    body.push(0);
    // PET_CUR_POWER, PET_MAX_POWER.
    body.extend_from_slice(&80u16.to_le_bytes());
    body.extend_from_slice(&100u16.to_le_bytes());
    // PET_AURAS: u32 mask, bit 3 set -> spell id 1126 (Mark of the Wild).
    body.extend_from_slice(&0x0000_0008u32.to_le_bytes());
    body.extend_from_slice(&1126u16.to_le_bytes());
    // PET_AURAS_NEGATIVE: u16 mask, bit 1 set -> spell id 770 (Faerie Fire).
    body.extend_from_slice(&0x0002u16.to_le_bytes());
    body.extend_from_slice(&770u16.to_le_bytes());

    let p = messages::parse_server(opcode::SMSG_PARTY_MEMBER_STATS_FULL, &body).unwrap();
    match &p {
        ServerPacket::PartyMemberStats { guid, full, info } => {
            assert_eq!(*guid, 0x7F);
            assert!(*full);
            assert_eq!(info.position, Some((1234, -5678)));
            assert_eq!(info.auras, Some(vec![133, 116]));
            assert_eq!(info.auras_negative, Some(vec![8050]));
            assert_eq!(info.pet_guid, Some(0x1122_3344_5566_7788));
            assert_eq!(info.pet_name.as_deref(), Some("Fido"));
            assert_eq!(info.pet_model_id, Some(618));
            assert_eq!(info.pet_cur_hp, Some(40));
            assert_eq!(info.pet_max_hp, Some(50));
            assert_eq!(info.pet_power_type, Some(0));
            assert_eq!(info.pet_cur_power, Some(80));
            assert_eq!(info.pet_max_power, Some(100));
            assert_eq!(info.pet_auras, Some(vec![1126]));
            assert_eq!(info.pet_auras_negative, Some(vec![770]));
            // Bits not in the mask stay None.
            assert_eq!(info.status, None);
            assert_eq!(info.level, None);
        }
        other => panic!("expected PartyMemberStats, got {}", other.name()),
    }

    match decode(p).as_slice() {
        [SessionEvent::PartyMemberStats {
            guid: 0x7F,
            full: true,
            info,
        }] => assert_eq!(info.pet_name.as_deref(), Some("Fido")),
        other => panic!("party member stats full decode: {other:?}"),
    }
}

/// The offline-miss reply (`Handlers/GroupHandler.cpp:763-774`, the not-in-raid-with-us branch):
/// `SMSG_PARTY_MEMBER_STATS_FULL`, mask `STATUS` only, status `MEMBER_STATUS_OFFLINE` (`0`).
#[test]
fn party_member_stats_offline_miss_is_status_only() {
    let body = hx(concat!(
        "015c",     // packed guid 0x5C
        "01000000", // mask: STATUS only
        "00",       // status: offline
    ));
    let p = messages::parse_server(opcode::SMSG_PARTY_MEMBER_STATS_FULL, &body).unwrap();
    match &p {
        ServerPacket::PartyMemberStats { guid, full, info } => {
            assert_eq!(*guid, 0x5C);
            assert!(*full);
            assert_eq!(info.status, Some(member_status::OFFLINE));
            assert_eq!(info.cur_hp, None);
        }
        other => panic!("expected PartyMemberStats, got {}", other.name()),
    }
}

/// `MSG_MINIMAP_PING` inbound (`Handlers/GroupHandler.cpp:382-391`): full guid + f32 x + f32 y —
/// the server-stamped rebroadcast shape (outbound, guid-less, is covered in [`cmsg_bodies_golden`]).
#[test]
fn minimap_ping_inbound_wire() {
    let mut body = 0x77u64.to_le_bytes().to_vec();
    body.extend_from_slice(&3.5f32.to_le_bytes());
    body.extend_from_slice(&(-4.25f32).to_le_bytes());
    let p = messages::parse_server(opcode::MSG_MINIMAP_PING, &body).unwrap();
    match &p {
        ServerPacket::MinimapPing { guid, x, y } => {
            assert_eq!(*guid, 0x77);
            assert_eq!(*x, 3.5);
            assert_eq!(*y, -4.25);
        }
        other => panic!("expected MinimapPing, got {}", other.name()),
    }
    match decode(p).as_slice() {
        [SessionEvent::MinimapPing { guid: 0x77, .. }] => {}
        other => panic!("minimap ping decode: {other:?}"),
    }
}

/// `MSG_RAID_TARGET_UPDATE`, all three server shapes (Group.cpp:132-147): mode 0 (delta), mode 1
/// with 2 entries, and mode 1 empty (no icons currently set — a raid with a clean target board).
#[test]
fn raid_target_update_smsg_shapes() {
    let mut body = vec![0u8, 3]; // mode 0 (delta), icon 3
    body.extend_from_slice(&0x99u64.to_le_bytes());
    let p = messages::parse_server(opcode::MSG_RAID_TARGET_UPDATE, &body).unwrap();
    match &p {
        ServerPacket::RaidTargetSet { icon, guid } => {
            assert_eq!(*icon, 3);
            assert_eq!(*guid, 0x99);
        }
        other => panic!("expected RaidTargetSet, got {}", other.name()),
    }
    match decode(p).as_slice() {
        [SessionEvent::RaidTargetSet {
            icon: 3,
            guid: 0x99,
        }] => {}
        other => panic!("raid target set decode: {other:?}"),
    }

    let mut body = vec![1u8]; // mode 1 (full list), 2 entries
    body.push(0);
    body.extend_from_slice(&0x11u64.to_le_bytes());
    body.push(7);
    body.extend_from_slice(&0x22u64.to_le_bytes());
    let p = messages::parse_server(opcode::MSG_RAID_TARGET_UPDATE, &body).unwrap();
    match &p {
        ServerPacket::RaidTargetList { entries } => {
            assert_eq!(entries, &vec![(0u8, 0x11u64), (7u8, 0x22u64)]);
        }
        other => panic!("expected RaidTargetList, got {}", other.name()),
    }
    match decode(p).as_slice() {
        [SessionEvent::RaidTargetList { entries }] => assert_eq!(entries.len(), 2),
        other => panic!("raid target list decode: {other:?}"),
    }

    // mode 1, empty: no icons currently marked.
    let p = messages::parse_server(opcode::MSG_RAID_TARGET_UPDATE, &[1u8]).unwrap();
    match &p {
        ServerPacket::RaidTargetList { entries } => assert!(entries.is_empty()),
        other => panic!("expected empty RaidTargetList, got {}", other.name()),
    }
}

/// `MSG_RAID_READY_CHECK`, both server shapes (Group.cpp:94-96 empty / 126-130 answer): the empty
/// "a check just started" body, and a member's forwarded answer (full guid + state).
#[test]
fn ready_check_smsg_shapes() {
    let p = messages::parse_server(opcode::MSG_RAID_READY_CHECK, &[]).unwrap();
    assert!(matches!(p, ServerPacket::ReadyCheckRequest));
    assert!(matches!(
        decode(p).as_slice(),
        [SessionEvent::ReadyCheckRequest]
    ));

    let mut body = 0x88u64.to_le_bytes().to_vec();
    body.push(1);
    let p = messages::parse_server(opcode::MSG_RAID_READY_CHECK, &body).unwrap();
    match &p {
        ServerPacket::ReadyCheckAnswer { guid, ready } => {
            assert_eq!(*guid, 0x88);
            assert_eq!(*ready, 1);
        }
        other => panic!("expected ReadyCheckAnswer, got {}", other.name()),
    }
    match decode(p).as_slice() {
        [SessionEvent::ReadyCheckAnswer {
            guid: 0x88,
            ready: 1,
        }] => {}
        other => panic!("ready check answer decode: {other:?}"),
    }
}
