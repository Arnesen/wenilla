//! `SMSG_UPDATE_OBJECT` / `SMSG_DESTROY_OBJECT` wire tests (mirrors `src/messages/update_object.rs`):
//! object creates (unit/gameobject/item/container), out-of-range removal, destroy, and the sparse
//! `ObjectFields` Values-update decode (unit state/power/customization, virtual items + sheath,
//! inventory slots). Split out of the former `tests/messages.rs` — see `tests/common` for the shared
//! fixtures and methodology note.

mod common;

use benilla_protocol::events::{decode, EntityKind, SessionEvent};
use benilla_protocol::messages;
use benilla_protocol::wire::write_packed_guid;
use common::hx;

#[test]
fn update_object_fixture_decodes() {
    // A real serialized SMSG_UPDATE_OBJECT body: Unit create (guid 0xAA, displayid 50, scale 1.5, a
    // living position) + GameObject create (guid 0xBB, displayid 99, scale 2.0, pos 100/200/300) +
    // out-of-range [0x10, 0x20].
    let body = hx("03000000000301aa03200000000000000000cdd70bc6357e04c3f90fa74200000040000000000000803f0000e040000090400000000000000000db0f4940051700000000000000000000000000000008000000aa00000000000000090000000000c03f320000000301bb05000117810700bb000000000000002100000000000040630000000000c84200004843000096430000803f040200000001100120");
    let packet = messages::parse_server(messages::opcode::SMSG_UPDATE_OBJECT, &body).unwrap();
    let events = decode(packet);

    let unit = events
        .iter()
        .find_map(|e| match e {
            SessionEvent::ObjectCreate {
                guid: 0xAA,
                kind,
                display_id,
                position,
                scale,
                speeds,
                ..
            } => Some((*kind, *display_id, *position, *scale, *speeds)),
            _ => None,
        })
        .expect("unit create");
    assert_eq!(unit.0, EntityKind::Unit);
    assert_eq!(unit.1, Some(50));
    assert_eq!(unit.2, [-8949.95, -132.493, 83.5312]);
    assert_eq!(unit.3, 1.5);
    assert_eq!(unit.4.map(|s| s.walk), Some(1.0)); // speeds[0] = walk, from the LIVING block

    let go = events
        .iter()
        .find_map(|e| match e {
            SessionEvent::ObjectCreate {
                guid: 0xBB,
                kind,
                display_id,
                position,
                scale,
                speeds,
                ..
            } => Some((*kind, *display_id, *position, *scale, *speeds)),
            _ => None,
        })
        .expect("gameobject create");
    assert_eq!(go.0, EntityKind::GameObject);
    assert_eq!(go.1, Some(99));
    assert_eq!(go.2, [100.0, 200.0, 300.0]);
    assert_eq!(go.3, 2.0);
    assert_eq!(go.4, None); // GameObject HAS_POSITION block carries no speeds

    let removed = events
        .iter()
        .find_map(|e| match e {
            SessionEvent::ObjectsRemoved(g) => Some(g.clone()),
            _ => None,
        })
        .expect("out-of-range");
    assert_eq!(removed, vec![0x10, 0x20]);
}

#[test]
fn destroy_object_decodes_to_object_destroyed() {
    // SMSG_DESTROY_OBJECT: a plain u64 guid (vmangos `DestroyObject::AppendBodyTo` streams the raw
    // ObjectGuid) — the corpse-decay / despawn teardown. Its own event, distinct from the OutOfRange
    // update block: destroyed objects pop instantly, out-of-range ones fade.
    let body = hx("efbeadde01000000");
    let packet = messages::parse_server(messages::opcode::SMSG_DESTROY_OBJECT, &body).unwrap();
    assert_eq!(packet.name(), "SMSG_DESTROY_OBJECT");
    let destroyed = decode(packet)
        .into_iter()
        .find_map(|e| match e {
            SessionEvent::ObjectDestroyed(guid) => Some(guid),
            _ => None,
        })
        .expect("destroy");
    assert_eq!(destroyed, 0x1_DEAD_BEEF);
}

#[test]
fn values_update_decodes_unit_state() {
    // A hand-built Values-only update (update_type 0) for guid 0xAA carrying the unit descriptor
    // fields HEALTH=22 (=100), MAXHEALTH=28 (=120), LEVEL=34 (=5), BYTES_0=36 (race 1/class 1/gender 0)
    // — bits in 2 mask blocks, values in ascending field order. Verifies the `Object::Values` path
    // (previously dropped) emits an ObjectValues delta whose fields decode via the named accessors.
    let body = hx("01000000000001aa02000040101400000064000000780000000500000001010001");
    let packet = messages::parse_server(messages::opcode::SMSG_UPDATE_OBJECT, &body).unwrap();
    let fields = decode(packet)
        .into_iter()
        .find_map(|e| match e {
            SessionEvent::ObjectValues { guid: 0xAA, fields } => Some(fields),
            _ => None,
        })
        .expect("object values");
    assert_eq!(
        (
            fields.unit_health(),
            fields.unit_max_health(),
            fields.unit_level(),
            fields.unit_race(),
            fields.unit_class(),
            fields.unit_gender(),
        ),
        (Some(100), Some(120), Some(5), Some(1), Some(1), Some(0))
    );
    // A non-player unit carries no PLAYER block, so the customization accessors stay `None`.
    assert_eq!(fields.player_skin(), None);
    assert_eq!(fields.player_facial_hair(), None);
}

#[test]
fn values_update_decodes_player_customization() {
    // A Values-only update for player guid 0xBB carrying PLAYER_BYTES=193 (skin 0x10 / face 0x11 /
    // hairStyle 0x12 / hairColor 0x13, packed b0..b3) and PLAYER_BYTES_2=194 (facialHair 0x14, b0).
    // Field 193 lives in mask block 6 (bit 1), 194 in block 6 (bit 2) → 7 blocks, the first six empty.
    // Pins the RF-0056 field indices + byte order against the decode.
    let body = hx(
        "01000000000001bb07000000000000000000000000000000000000000000000000060000001011121314000000",
    );
    let packet = messages::parse_server(messages::opcode::SMSG_UPDATE_OBJECT, &body).unwrap();
    let fields = decode(packet)
        .into_iter()
        .find_map(|e| match e {
            SessionEvent::ObjectValues { guid: 0xBB, fields } => Some(fields),
            _ => None,
        })
        .expect("player object values");
    assert_eq!(
        (
            fields.player_skin(),
            fields.player_face(),
            fields.player_hair_style(),
            fields.player_hair_color(),
            fields.player_facial_hair(),
        ),
        (Some(0x10), Some(0x11), Some(0x12), Some(0x13), Some(0x14))
    );
}

#[test]
fn values_update_decodes_unit_power() {
    // A Values-only update for guid 0xAA carrying POWER1=23 (40), MAXPOWER1=29 (60), BYTES_0=36
    // (race 1 / class 1 / gender 0 / power type 0 = mana). Pins the 1.12.1 power field indices
    // (vmangos UpdateFields_1_12_1.h) + the bytes_0 power-type byte against the decode.
    let body = hx("01000000000001aa020000802010000000280000003c00000001010000");
    let packet = messages::parse_server(messages::opcode::SMSG_UPDATE_OBJECT, &body).unwrap();
    let fields = decode(packet)
        .into_iter()
        .find_map(|e| match e {
            SessionEvent::ObjectValues { guid: 0xAA, fields } => Some(fields),
            _ => None,
        })
        .expect("object values");
    assert_eq!(fields.unit_power_type(), 0);
    assert_eq!(fields.unit_power(0), Some(40));
    assert_eq!(fields.unit_max_power(0), Some(60));
    // Other slots are absent; out-of-range slots are None by contract.
    assert_eq!(fields.unit_power(1), None);
    assert_eq!(fields.unit_max_power(5), None);
}

#[test]
fn values_update_decodes_unit_virtual_items_and_sheath() {
    // A Values-only update for creature guid 0xCC carrying UNIT_VIRTUAL_ITEM_SLOT_DISPLAY slot 0
    // (field 37) = 1234, UNIT_VIRTUAL_ITEM_INFO slot 0's pair (fields 40/41): dword0 packs
    // class=2/subclass=7/material=1/invType=21, dword1 byte0 = sheath 3; and UNIT_FIELD_BYTES_2
    // (field 164) byte0 = sheath state 1 (melee drawn). Field -> (block, bit): 37 -> (1,5),
    // 40 -> (1,8), 41 -> (1,9), 164 -> (5,4) -- 6 mask blocks.
    let mut body = hx("01000000000001cc06"); // count 1, no transport, VALUES, packed guid 0xCC, 6 blocks
    let mut masks = [0u32; 6];
    masks[1] = (1 << 5) | (1 << 8) | (1 << 9);
    masks[5] = 1 << 4;
    for m in masks {
        body.extend_from_slice(&m.to_le_bytes());
    }
    let info_dword0 = 2u32 | (7 << 8) | (1 << 16) | (21 << 24); // class/subclass/material/invType
    for v in [1234u32, info_dword0, 3u32, 1u32] {
        body.extend_from_slice(&v.to_le_bytes());
    }
    let packet = messages::parse_server(messages::opcode::SMSG_UPDATE_OBJECT, &body).unwrap();
    let fields = decode(packet)
        .into_iter()
        .find_map(|e| match e {
            SessionEvent::ObjectValues { guid: 0xCC, fields } => Some(fields),
            _ => None,
        })
        .expect("object values");

    assert_eq!(fields.unit_virtual_item_display(0), Some(1234));
    assert_eq!(fields.unit_virtual_item_display(1), None, "slot 1 unsent");
    assert_eq!(
        fields.unit_virtual_item_display(3),
        None,
        "slot out of range"
    );
    assert_eq!(fields.unit_virtual_item_info(0), Some((2, 7, 1, 21)));
    assert_eq!(fields.unit_virtual_item_info(3), None, "slot out of range");
    assert_eq!(fields.unit_virtual_item_sheath(0), Some(3));
    assert_eq!(
        fields.unit_virtual_item_sheath(3),
        None,
        "slot out of range"
    );
    assert_eq!(fields.unit_sheath_state(), Some(1));
}

#[test]
fn values_update_decodes_inventory_slots() {
    // A Values-only update for guid 1 carrying PLAYER_FIELD_INV_SLOT 16 (the offhand, fields
    // 518/519 — live-verified: One's shield guid landed exactly there) = guid 0xAB and
    // PLAYER_FIELD_PACK_SLOT 0 (fields 532/533, live-verified) = guid 0xCD. Pins the CORRECTED
    // 1.12.1 bases (vmangos enum arithmetic, NOT its stale hex comments: INV_SLOT_HEAD 486,
    // PACK 532). 17 mask blocks; block 16 = bits 6,7 (518,519) + 20,21 (532,533).
    let mut body = hx("010000000000010111"); // count 1, no transport, VALUES, packed guid 1, 17 blocks
    for block in 0..17u32 {
        let mask: u32 = if block == 16 { 0x0030_00C0 } else { 0 };
        body.extend_from_slice(&mask.to_le_bytes());
    }
    for v in [0xABu32, 0, 0xCD, 0] {
        body.extend_from_slice(&v.to_le_bytes());
    }
    let packet = messages::parse_server(messages::opcode::SMSG_UPDATE_OBJECT, &body).unwrap();
    let fields = decode(packet)
        .into_iter()
        .find_map(|e| match e {
            SessionEvent::ObjectValues { guid: 1, fields } => Some(fields),
            _ => None,
        })
        .expect("values event");
    assert_eq!(fields.player_inv_slot(16), Some(0xAB));
    assert_eq!(fields.player_pack_slot(0), Some(0xCD));
    assert_eq!(fields.player_pack_slot(3), None, "unsent slot reads None");
    assert_eq!(fields.player_bank_slot(0), None);
    assert_eq!(fields.container_slot(36), None, "out of range");
}

#[test]
fn item_and_container_creates_decode() {
    // Two position-less creates in one packet — an item and a bag — exactly as vmangos streams our
    // inventory at login: CREATE_OBJECT, packed guid, TypeId 1/2, movement block = UPDATEFLAG_ALL
    // (0x10) + its constant u32 1 (wire-verified `Object::BuildMovementUpdate`), then the mask.
    // These used to be dropped whole by the create placement gate; now they're ItemCreate events.
    // Container fields pin the CORRECTED bases (enum arithmetic, not the stale hex comments —
    // the same 6-low drift as the PLAYER block): NUM_SLOTS = ITEM_END = 48, SLOT_1 = 50.
    let mut body = hx("0200000000");
    // Item guid 0x42, entry 4660 (field 3), stack count 5 (field 14): 1 mask block, bits 3+14.
    body.extend_from_slice(&hx("0201420110010000000108400000"));
    body.extend_from_slice(&4660u32.to_le_bytes());
    body.extend_from_slice(&5u32.to_le_bytes());
    // Container guid 0x43, entry 828 (field 3), num-slots 6 (field 48), slot-0 guid 0x42
    // (fields 50/51): 2 mask blocks — block0 bit 3, block1 bits 16/18/19.
    body.extend_from_slice(&hx("020143021001000000020800000000000d00"));
    for v in [828u32, 6, 0x42, 0] {
        body.extend_from_slice(&v.to_le_bytes());
    }
    let packet = messages::parse_server(messages::opcode::SMSG_UPDATE_OBJECT, &body).unwrap();
    let events = decode(packet);
    assert_eq!(events.len(), 2, "both creates decode, nothing spatial");
    match &events[0] {
        SessionEvent::ItemCreate {
            guid: 0x42,
            container: false,
            fields,
        } => {
            assert_eq!(fields.object_entry(), Some(4660));
            assert_eq!(fields.item_stack_count(), Some(5));
        }
        other => panic!("item create, got {other:?}"),
    }
    match &events[1] {
        SessionEvent::ItemCreate {
            guid: 0x43,
            container: true,
            fields,
        } => {
            assert_eq!(fields.object_entry(), Some(828));
            assert_eq!(fields.container_num_slots(), Some(6));
            assert_eq!(fields.container_slot(0), Some(0x42));
            assert_eq!(fields.container_slot(1), None, "unsent slot reads None");
        }
        other => panic!("container create, got {other:?}"),
    }
}

/// A transport GameObject's create (decision 0438 "the wire"): `UPDATE_FLAG_HAS_POSITION` (0x40) +
/// `UPDATE_FLAG_TRANSPORT` (0x02), the latter's tail being the server's pathProgress ms anchor
/// (vmangos `Object.cpp:590-605`). The mask carries `GAMEOBJECT_DISPLAYID` only — **no**
/// `GAMEOBJECT_POS_*` fields, exactly what vmangos sends for a boat/zeppelin (it puts the stationary
/// spawn point in the movement block's `HAS_POSITION` slot instead) — pinning both the new
/// `transport_progress` surfacing AND the create-placement fallback to the movement block's position
/// (`create_placement`, `events/decode.rs`) in one fixture.
#[test]
fn gameobject_create_surfaces_transport_progress_and_position_fallback() {
    // HIGH_MO_TRANSPORT (0x1FC0) with the Menethil-Theramore ferry's real template entry (176495)
    // riding the full low 32 bits (guid.rs `entry()` — MO_TRANSPORT has no 24-bit entry slot).
    let guid: u64 = 176_495 | (0x1FC0u64 << 48);

    let mut body = 1u32.to_le_bytes().to_vec(); // amount_of_objects
    body.push(0); // has_transport (unused by the parser)
    body.push(2); // update_type: CREATE_OBJECT
    write_packed_guid(guid, &mut body).unwrap();
    body.push(5); // TypeId::GameObject

    // Movement block: HAS_POSITION | TRANSPORT.
    body.push(0x40 | 0x02);
    body.extend_from_slice(&5000.0f32.to_le_bytes()); // stationary pos.x
    body.extend_from_slice(&6000.0f32.to_le_bytes()); // stationary pos.y
    body.extend_from_slice(&15.0f32.to_le_bytes()); // stationary pos.z
    body.extend_from_slice(&2.0f32.to_le_bytes()); // orientation
    body.extend_from_slice(&123_456u32.to_le_bytes()); // UPDATE_FLAG_TRANSPORT tail: pathProgress ms

    // Mask: 1 block, GAMEOBJECT_DISPLAYID (field 8) only.
    body.push(1);
    body.extend_from_slice(&(1u32 << 8).to_le_bytes());
    body.extend_from_slice(&3015u32.to_le_bytes()); // transportship.wmo's displayId

    let packet = messages::parse_server(messages::opcode::SMSG_UPDATE_OBJECT, &body).unwrap();
    let events = decode(packet);
    match events.as_slice() {
        [SessionEvent::ObjectCreate {
            guid: g,
            kind,
            display_id,
            position,
            orientation,
            speeds,
            transport_progress,
            transport,
            ..
        }] => {
            assert_eq!(*g, guid);
            assert_eq!(*kind, EntityKind::GameObject);
            assert_eq!(*display_id, Some(3015));
            assert_eq!(
                *position,
                [5000.0, 6000.0, 15.0],
                "falls back to the movement block's HAS_POSITION pose — GAMEOBJECT_POS_* is absent"
            );
            assert_eq!(*orientation, 2.0);
            assert_eq!(
                *speeds, None,
                "a GameObject's HAS_POSITION block carries no speeds"
            );
            assert_eq!(
                *transport_progress,
                Some(123_456),
                "UPDATE_FLAG_TRANSPORT's u32 tail is the pathProgress ms anchor"
            );
            assert_eq!(
                *transport, None,
                "a HAS_POSITION block (not LIVING) carries no ON_TRANSPORT rider tail"
            );
        }
        other => panic!("expected one ObjectCreate, got {other:?}"),
    }
}

/// A unit's `LIVING` block with `MOVEFLAG_ON_TRANSPORT` (0x0200_0000): the rider pose tail
/// `[u64 transport guid][local x,y,z][local o]` (`update_object/movement.rs`'s `MovementBlock::read`,
/// mirroring `read_movement_info`'s relay parse). VERIFIES the tail lands in `ObjectCreate::transport`
/// and the 6 movement speeds that follow still parse from the right offset (the alignment the old
/// discard-to-stay-aligned code depended on, now proven by consuming the values instead of dropping
/// them).
#[test]
fn unit_living_block_surfaces_on_transport_rider_pose() {
    // The Org-UC zeppelin's template entry (176244), same MO_TRANSPORT composition as the ferry above.
    let transport_guid: u64 = 176_244 | (0x1FC0u64 << 48);

    let mut body = 1u32.to_le_bytes().to_vec();
    body.push(0); // has_transport
    body.push(2); // update_type: CREATE_OBJECT
    write_packed_guid(0xAA, &mut body).unwrap();
    body.push(3); // TypeId::Unit

    body.push(0x20); // movement block: LIVING only
    let flags: u32 = 0x0200_0000; // MOVEFLAG_ON_TRANSPORT (1.12 bit 25, vmangos MovementInfo.h)
    body.extend_from_slice(&flags.to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes()); // timestamp
    body.extend_from_slice(&100.0f32.to_le_bytes()); // living pos.x (world, NOT local)
    body.extend_from_slice(&200.0f32.to_le_bytes()); // living pos.y
    body.extend_from_slice(&5.0f32.to_le_bytes()); // living pos.z
    body.extend_from_slice(&1.0f32.to_le_bytes()); // living orientation
    body.extend_from_slice(&transport_guid.to_le_bytes()); // ON_TRANSPORT tail: FULL u64 guid
    body.extend_from_slice(&2.0f32.to_le_bytes()); // local x
    body.extend_from_slice(&3.0f32.to_le_bytes()); // local y
    body.extend_from_slice(&0.5f32.to_le_bytes()); // local z
    body.extend_from_slice(&0.25f32.to_le_bytes()); // local o
    body.extend_from_slice(&0.0f32.to_le_bytes()); // fall_time (not swimming/jumping, no other tails)
    for v in [2.5f32, 7.0, 4.5, 4.722_222_3, 2.5, std::f32::consts::PI] {
        body.extend_from_slice(&v.to_le_bytes()); // the 6 speeds — must still land right after the tail
    }

    body.push(0); // mask: 0 blocks (nothing to interpret beyond the pose)

    let packet = messages::parse_server(messages::opcode::SMSG_UPDATE_OBJECT, &body).unwrap();
    let events = decode(packet);
    match events.as_slice() {
        [SessionEvent::ObjectCreate {
            guid: 0xAA,
            kind: EntityKind::Unit,
            position,
            orientation,
            speeds,
            transport_progress,
            transport,
            ..
        }] => {
            assert_eq!(*position, [100.0, 200.0, 5.0], "the unit's own world pose");
            assert_eq!(*orientation, 1.0);
            assert_eq!(
                transport,
                &Some(benilla_protocol::messages::TransportPose {
                    guid: transport_guid,
                    pos: benilla_protocol::wire::Vector3d {
                        x: 2.0,
                        y: 3.0,
                        z: 0.5,
                    },
                    orientation: 0.25,
                }),
                "the ON_TRANSPORT tail surfaces as the rider's local pose"
            );
            assert_eq!(
                transport_progress, &None,
                "no UPDATE_FLAG_TRANSPORT on a rider create"
            );
            assert_eq!(
                speeds.map(|s| s.walk),
                Some(2.5),
                "the 6 speeds still parse right after the transport tail"
            );
        }
        other => panic!("expected one ObjectCreate, got {other:?}"),
    }
}

/// A unit that is **already walking** when it streams in: the `LIVING` block's
/// `MOVEFLAG_SPLINE_ENABLED` (0x0040_0000) tail, byte-shaped exactly as vmangos writes it
/// (`PacketBuilder::WriteCreate`, `packet_builder.cpp:152`). Two things are verified, and both are
/// what decision 0708 turns on:
///
/// 1. The node array is the server's **internal** control array — `[phantom, p₀…pₙ, tail]`, since
///    vmangos builds even a linear spline through `InitCatmullRom` (`spline.cpp:52`) — so the decoded
///    `path` must be that array with its two virtual points trimmed, not the raw nodes.
/// 2. The descriptor mask *after* the spline still parses: a mis-sized spline read (a missed node, a
///    forgotten final destination) silently shifts every field that follows, which is precisely how a
///    parse-and-discard block hides its own bugs.
#[test]
fn unit_living_block_surfaces_the_walk_it_is_already_on() {
    // The real path: an L, 10 yd east then 20 yd north. What the server actually serializes is that
    // path wrapped in its two virtual points: a phantom head at `p₀.lerp(p₁, -1)` = 2p₀ − p₁ (one
    // segment *behind* the start) and a tail duplicating the destination.
    let path = [
        [10.0f32, 20.0, 30.0],
        [20.0, 20.0, 30.0],
        [20.0, 40.0, 30.0],
    ];
    let nodes = [
        [0.0f32, 20.0, 30.0], // phantom: 2·p₀ − p₁
        path[0],
        path[1],
        path[2],
        path[2], // tail: the destination again
    ];

    let mut body = 1u32.to_le_bytes().to_vec();
    body.push(0); // has_transport
    body.push(2); // update_type: CREATE_OBJECT
    write_packed_guid(0xAA, &mut body).unwrap();
    body.push(3); // TypeId::Unit

    body.push(0x20); // movement block: LIVING only
    let flags: u32 = 0x0040_0000; // MOVEFLAG_SPLINE_ENABLED (vmangos MovementInfo.h:53)
    body.extend_from_slice(&flags.to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes()); // timestamp
                                                 // The unit's stored pose — vmangos re-syncs it to the spline only every 400 ms, so it trails the
                                                 // true position; the spline below is what actually places the unit.
    for v in [14.0f32, 20.0, 30.0, 0.0] {
        body.extend_from_slice(&v.to_le_bytes()); // living pos + orientation
    }
    body.extend_from_slice(&0.0f32.to_le_bytes()); // fall_time
    for v in [2.5f32, 7.0, 4.5, 4.722_222_3, 2.5, std::f32::consts::PI] {
        body.extend_from_slice(&v.to_le_bytes()); // the 6 speeds
    }
    // The spline tail: flags, time_passed, duration, id, node count, the nodes, final destination.
    body.extend_from_slice(&0u32.to_le_bytes()); // spline flags: ground walk, no dictated facing
    body.extend_from_slice(&3_000u32.to_le_bytes()); // time_passed: a quarter of the ride is done
    body.extend_from_slice(&12_000u32.to_le_bytes()); // duration
    body.extend_from_slice(&42u32.to_le_bytes()); // spline id
    body.extend_from_slice(&(nodes.len() as u32).to_le_bytes());
    for n in nodes {
        for f in n {
            body.extend_from_slice(&f.to_le_bytes());
        }
    }
    for f in path[2] {
        body.extend_from_slice(&f.to_le_bytes()); // FinalDestination
    }

    // A descriptor field behind the spline: UNIT_FIELD_DISPLAYID (131) = block 4, bit 3.
    body.push(5); // 5 mask blocks
    for m in [0u32, 0, 0, 0, 1 << 3] {
        body.extend_from_slice(&m.to_le_bytes());
    }
    body.extend_from_slice(&1234u32.to_le_bytes());

    let packet = messages::parse_server(messages::opcode::SMSG_UPDATE_OBJECT, &body).unwrap();
    let events = decode(packet);
    match events.as_slice() {
        [SessionEvent::ObjectCreate {
            guid: 0xAA,
            kind: EntityKind::Unit,
            display_id,
            position,
            spline,
            ..
        }] => {
            let s = spline.as_ref().expect("the create block's live spline");
            assert_eq!(
                s.path, path,
                "the two virtual control points are the server's, not waypoints"
            );
            assert_eq!(s.time_passed_ms, 3_000);
            assert_eq!(s.duration_ms, 12_000);
            assert_eq!(s.id, 42);
            assert!(!s.flying, "no Flying bit ⇒ a ground walk");
            assert!(!s.cyclic);
            assert_eq!(
                *position,
                [14.0, 20.0, 30.0],
                "the block's own pose is the server's last 400 ms sync, kept as the spawn pose"
            );
            assert_eq!(
                *display_id,
                Some(1234),
                "the descriptor mask behind the spline still parses — the block was sized right"
            );
        }
        other => panic!("expected one ObjectCreate, got {other:?}"),
    }
}

/// A unit standing still carries no spline tail at all — the flag gates it, so `spline` is `None` and
/// nothing downstream tries to walk it.
#[test]
fn unit_living_block_without_the_spline_flag_carries_no_walk() {
    let mut body = 1u32.to_le_bytes().to_vec();
    body.push(0); // has_transport
    body.push(2); // CREATE_OBJECT
    write_packed_guid(0xAB, &mut body).unwrap();
    body.push(3); // TypeId::Unit
    body.push(0x20); // LIVING
    body.extend_from_slice(&0u32.to_le_bytes()); // no move flags
    body.extend_from_slice(&0u32.to_le_bytes()); // timestamp
    for v in [1.0f32, 2.0, 3.0, 0.5] {
        body.extend_from_slice(&v.to_le_bytes());
    }
    body.extend_from_slice(&0.0f32.to_le_bytes()); // fall_time
    for v in [2.5f32, 7.0, 4.5, 4.722_222_3, 2.5, std::f32::consts::PI] {
        body.extend_from_slice(&v.to_le_bytes());
    }
    body.push(0); // no descriptor fields

    let packet = messages::parse_server(messages::opcode::SMSG_UPDATE_OBJECT, &body).unwrap();
    match decode(packet).as_slice() {
        [SessionEvent::ObjectCreate { spline, .. }] => assert!(spline.is_none()),
        other => panic!("expected one ObjectCreate, got {other:?}"),
    }
}

/// `PLAYER_VISIBLE_ITEM_<n>_0` across the slot range — the PUBLIC entry the inspect window's whole
/// paper doll is built from (decision 0631) and the same field equipment rendering resolves other
/// players' gear through. Untested until now, and an off-by-one in its `258 + 2 + 12i` stride would
/// put an inspected player's rings on their feet: the stride is exercised at both ends and in the
/// middle, not just at slot 0 (which any wrong-but-anchored formula would also get right).
///
/// Field index = `PLAYER_VISIBLE_ITEM_1_CREATOR` (258, a 2-field guid) + 2 + 12·slot, so
/// slot 0 → 260 (block 8 bit 4), slot 1 → 272 (block 8 bit 16), slot 14 → 428 (block 13 bit 12),
/// slot 18 → 476 (block 14 bit 28). 15 mask blocks cover it.
#[test]
fn values_update_decodes_player_visible_item_entries() {
    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(&1u32.to_le_bytes()); // count 1
    body.push(0); // no transport
    body.push(0); // UPDATETYPE_VALUES
    body.extend_from_slice(&hx("01dd")); // packed guid 0xDD
    body.push(15); // 15 mask blocks
    let mut masks = [0u32; 15];
    masks[8] = (1 << 4) | (1 << 16);
    masks[13] = 1 << 12;
    masks[14] = 1 << 28;
    for m in masks {
        body.extend_from_slice(&m.to_le_bytes());
    }
    // Ascending field order: 260, 272, 428, 476.
    for v in [7365u32, 2196u32, 15406u32, 2504u32] {
        body.extend_from_slice(&v.to_le_bytes());
    }
    let packet = messages::parse_server(messages::opcode::SMSG_UPDATE_OBJECT, &body).unwrap();
    let fields = decode(packet)
        .into_iter()
        .find_map(|e| match e {
            SessionEvent::ObjectValues { guid: 0xDD, fields } => Some(fields),
            _ => None,
        })
        .expect("object values");

    assert_eq!(fields.player_visible_item_entry(0), Some(7365), "HEAD");
    assert_eq!(fields.player_visible_item_entry(1), Some(2196), "NECK");
    assert_eq!(fields.player_visible_item_entry(14), Some(15406), "BACK");
    assert_eq!(fields.player_visible_item_entry(18), Some(2504), "TABARD");
    // An unsent slot and the sentinel are both None — an empty equipment slot must not read as
    // "item 0" (the feed keys "is there gear here" on exactly this).
    assert_eq!(fields.player_visible_item_entry(2), None, "slot 2 unsent");
    assert_eq!(
        fields.player_visible_item_entry(19),
        None,
        "slot out of range (19 slots, 0..=18)"
    );
}
