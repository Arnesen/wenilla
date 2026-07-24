//! `SMSG_CHAR_ENUM` (the character-select roster) + the logout round-trip. The enum body is
//! hand-built byte-exact to the vmangos serializer (`Player::BuildEnumData`, `Player.cpp:1629`:
//! guid u64 · name cstring · race/class/gender/skin/face/hairStyle/hairColor/facialHair u8 ·
//! level u8 · zone u32 · map u32 · x/y/z f32 · guild u32 · flags u32 · firstLogin u8 ·
//! petDisplay/petLevel/petFamily u32 · 19×(display u32 + invType u8) equipment · first-bag
//! display u32 + invType u8); two characters in one body exercise the alignment of everything
//! the parser skips.

use benilla_protocol::events::{decode, SessionEvent};
use benilla_protocol::messages;
use benilla_protocol::ServerPacket;

/// One serialized enum entry, vmangos field order. Non-roster fields get distinct junk values so a
/// misaligned parse can't accidentally pass.
#[allow(clippy::too_many_arguments)]
fn enum_entry(
    guid: u64,
    name: &str,
    race: u8,
    class: u8,
    gender: u8,
    level: u8,
    map: u32,
    pos: [f32; 3],
) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&guid.to_le_bytes());
    b.extend_from_slice(name.as_bytes());
    b.push(0);
    b.extend_from_slice(&[race, class, gender]);
    b.extend_from_slice(&[3, 4, 5, 6, 7]); // skin/face/hairStyle/hairColor/facialHair
    b.push(level);
    b.extend_from_slice(&12u32.to_le_bytes()); // zone
    b.extend_from_slice(&map.to_le_bytes());
    for c in pos {
        b.extend_from_slice(&c.to_le_bytes());
    }
    b.extend_from_slice(&0u32.to_le_bytes()); // guild id
    b.extend_from_slice(&0x0200_0000u32.to_le_bytes()); // character flags (DECLINED — junk)
    b.push(1); // first login
    b.extend_from_slice(&99u32.to_le_bytes()); // pet display id
    b.extend_from_slice(&7u32.to_le_bytes()); // pet level
    b.extend_from_slice(&1u32.to_le_bytes()); // pet family
    for slot in 0..19u32 {
        b.extend_from_slice(&(1000 + slot).to_le_bytes()); // equipment display id
        b.push(slot as u8); // inventory type
    }
    b.extend_from_slice(&2000u32.to_le_bytes()); // first bag display id
    b.push(18); // first bag inventory type
    b
}

#[test]
fn char_enum_roster_parses_and_decodes() {
    let mut body = vec![2u8]; // count
    body.extend(enum_entry(
        0x0000_0000_0000_0007,
        "Two",
        1, // Human
        1, // Warrior
        0,
        1,
        0,
        [-8949.95, -132.493, 83.5312],
    ));
    body.extend(enum_entry(
        0x0000_0000_0000_0009,
        "Twomage",
        1, // Human
        8, // Mage
        1,
        60,
        1,
        [1.5, -2.5, 3.5],
    ));

    let p = messages::parse_server(messages::opcode::SMSG_CHAR_ENUM, &body).unwrap();
    let ServerPacket::CharEnum { ref characters } = p else {
        panic!("char enum")
    };
    assert_eq!(characters.len(), 2);
    let c = &characters[0];
    assert_eq!(
        (c.guid, c.name.as_str(), c.race, c.class, c.gender),
        (7, "Two", 1, 1, 0)
    );
    // The appearance bytes + zone + flags land verbatim (decision 0465 — the select screen
    // renders them; they used to be alignment-skips).
    assert_eq!(
        (c.skin, c.face, c.hair_style, c.hair_color, c.facial_hair),
        (3, 4, 5, 6, 7)
    );
    assert_eq!((c.zone, c.flags), (12, 0x0200_0000));
    assert_eq!((c.level, c.map), (1, 0));
    assert_eq!((c.position.x, c.position.y), (-8949.95, -132.493));
    // All 19 equipment pairs, in slot order: display 1000+slot, invType = slot.
    for (slot, item) in c.equipment.iter().enumerate() {
        assert_eq!(
            (item.display_id, item.inventory_type),
            (1000 + slot as u32, slot as u8)
        );
    }
    let c = &characters[1];
    assert_eq!(
        (c.guid, c.name.as_str(), c.race, c.class, c.gender),
        (9, "Twomage", 1, 8, 1)
    );
    assert_eq!((c.level, c.map, c.position.z), (60, 1, 3.5));
    assert_eq!(c.equipment[18].display_id, 1018);

    // Decodes to one CharacterList event carrying the roster through (no realm context on the raw
    // decode path — the IO thread's own emit is what carries the auth realm entry).
    match &decode(p)[..] {
        [SessionEvent::CharacterList { characters, realm }] => {
            assert_eq!(characters.len(), 2);
            assert!(realm.is_none());
        }
        other => panic!("expected CharacterList, got {other:?}"),
    }
}

#[test]
fn logout_complete_decodes_to_logged_out() {
    // SMSG_LOGOUT_COMPLETE: empty body.
    let p = messages::parse_server(messages::opcode::SMSG_LOGOUT_COMPLETE, &[]).unwrap();
    assert!(matches!(decode(p)[..], [SessionEvent::LoggedOut]));
}
