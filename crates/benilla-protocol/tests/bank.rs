//! Oracle-free golden tests for the bank arc's protocol layer (Phase 1 of decision 0604): the
//! six-opcode wire (`CMSG_BANKER_ACTIVATE`/`SMSG_SHOW_BANK`/`CMSG_BUY_BANK_SLOT`/
//! `SMSG_BUY_BANK_SLOT_RESULT`/`CMSG_AUTOSTORE_BANK_ITEM`/`CMSG_AUTOBANK_ITEM`) plus the two field
//! accessors the vault window needs (`player_bank_bag_slot`, `player_bank_bag_slots_purchased`).
//! Same idioms as `tests/gossip_vendor.rs` — `hx(...)` golden CMSG bodies, hand-built SMSG bodies
//! round-tripped through `parse_server`.

use benilla_protocol::events::{decode, SessionEvent};
use benilla_protocol::messages::{self, ObjectFields};
use benilla_protocol::ServerPacket;

fn hx(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn bank_bodies_golden() {
    // CMSG_BANKER_ACTIVATE (vmangos Npc.h:58-66): a full guid.
    assert_eq!(
        messages::banker_activate(0x1234_5678_9abc_def0),
        hx("f0debc9a78563412"),
        "CMSG_BANKER_ACTIVATE body"
    );

    // CMSG_BUY_BANK_SLOT (vmangos Item.h:157-163): a full guid — no slot index, the server buys
    // `purchased_count + 1` itself.
    assert_eq!(
        messages::buy_bank_slot(0x1234_5678_9abc_def0),
        hx("f0debc9a78563412"),
        "CMSG_BUY_BANK_SLOT body"
    );

    // CMSG_AUTOBANK_ITEM (vmangos Item.h:108-116): srcbag, srcslot.
    assert_eq!(
        messages::autobank_item(255, 23),
        vec![255u8, 23],
        "CMSG_AUTOBANK_ITEM body"
    );

    // CMSG_AUTOSTORE_BANK_ITEM (vmangos Item.h:118-126): srcbag, srcslot — same shape, direction
    // is decided server-side by whether the position names a bank slot.
    assert_eq!(
        messages::autostore_bank_item(255, 39),
        vec![255u8, 39],
        "CMSG_AUTOSTORE_BANK_ITEM body"
    );
}

#[test]
fn show_bank_wire() {
    // SMSG_SHOW_BANK (vmangos Npc.cpp:94): one full guid.
    let body = hx("f0debc9a78563412");
    match messages::parse_server(messages::opcode::SMSG_SHOW_BANK, &body).unwrap() {
        ServerPacket::ShowBank { banker } => {
            assert_eq!(banker, 0x1234_5678_9abc_def0);
        }
        other => panic!("show bank, got {}", other.name()),
    }

    let packet = messages::parse_server(messages::opcode::SMSG_SHOW_BANK, &body).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::ShowBank { banker } => {
            assert_eq!(banker, 0x1234_5678_9abc_def0);
        }
        other => panic!("show bank event, got {other:?}"),
    }
}

#[test]
fn buy_bank_slot_result_wire() {
    use messages::bank_slot_result;

    // Result enum values (vmangos Player.h:91-94) — pinned so a future edit can't drift them.
    assert_eq!(
        (
            bank_slot_result::FAILED_TOO_MANY,
            bank_slot_result::INSUFFICIENT_FUNDS,
            bank_slot_result::NOTBANKER,
            bank_slot_result::OK,
        ),
        (0, 1, 2, 3)
    );

    // SMSG_BUY_BANK_SLOT_RESULT (vmangos Item.cpp:137-140): one u32 result. Round-trip every code.
    for &code in &[
        bank_slot_result::FAILED_TOO_MANY,
        bank_slot_result::INSUFFICIENT_FUNDS,
        bank_slot_result::NOTBANKER,
        bank_slot_result::OK,
    ] {
        let body = code.to_le_bytes().to_vec();
        match messages::parse_server(messages::opcode::SMSG_BUY_BANK_SLOT_RESULT, &body).unwrap() {
            ServerPacket::BuyBankSlotResult { result } => {
                assert_eq!(result, code, "result code {code}");
            }
            other => panic!("buy bank slot result, got {}", other.name()),
        }

        let packet =
            messages::parse_server(messages::opcode::SMSG_BUY_BANK_SLOT_RESULT, &body).unwrap();
        match decode(packet).pop().unwrap() {
            SessionEvent::BuyBankSlotResult { result } => {
                assert_eq!(result, code, "event result code {code}");
            }
            other => panic!("buy bank slot result event, got {other:?}"),
        }
    }
}

#[test]
fn player_bank_bag_slot_field() {
    // FIELD_PLAYER_BANK_BAG_SLOT_1 = 612 (564 + 24×2): bank bag 0's guid lives at fields 612/613.
    let f = ObjectFields::from_pairs(&[(612, 0xEF), (613, 0)]);
    assert_eq!(f.player_bank_bag_slot(0), Some(0xEF));
    assert_eq!(f.player_bank_bag_slot(1), None, "unsent slot reads None");
    assert_eq!(
        f.player_bank_bag_slot(6),
        None,
        "out of range (only 6 bag slots)"
    );

    // Bag slot 5 (the 6th, last purchasable slot): fields 622/623.
    let f5 = ObjectFields::from_pairs(&[(622, 0x1234), (623, 0)]);
    assert_eq!(f5.player_bank_bag_slot(5), Some(0x1234));
}

#[test]
fn player_bank_bag_slots_purchased_field() {
    // PLAYER_BYTES_2 (field 194): byte 0 facialHair, byte 1 unk, byte 2 bankBagSlots, byte 3
    // restState (vmangos Player.h:347, PLAYER_BYTES_2_OFFSET_BANK_BAG_SLOTS = 2). Packed value
    // 0x04_03_02_01 = restState 4, bankBagSlots 3, unk 2, facialHair 1.
    let f = ObjectFields::from_pairs(&[(194, 0x0403_0201)]);
    assert_eq!(f.player_bank_bag_slots_purchased(), Some(3));
    assert_eq!(f.player_facial_hair(), Some(1), "byte 0 unaffected");

    // Unsent field: None, not 0 — the caller must not treat "unstreamed" as "zero slots bought".
    let empty = ObjectFields::from_pairs(&[]);
    assert_eq!(empty.player_bank_bag_slots_purchased(), None);

    // All 6 slots purchased.
    let full = ObjectFields::from_pairs(&[(194, 6 << 16)]);
    assert_eq!(full.player_bank_bag_slots_purchased(), Some(6));
}
