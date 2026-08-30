//! Oracle-free golden tests for the stable-master arc's protocol layer (decision 1676): the five
//! send verbs, the `MSG_LIST_STABLED_PETS` list, and the `SMSG_STABLE_RESULT` byte. Same idioms as
//! `trainer.rs` — `hx(...)` golden CMSG bodies, hand-built SMSG bodies round-tripped through
//! `parse_server`.
//!
//! Every layout here is VERIFIED against vmangos: the send bodies from
//! `Server/Packets/Npc.cpp:51-76` (`ReadFromWorldPacket` for each `ClientPacket`), the list from
//! `WorldSession::SendStablePet` (`Handlers/NPCHandler.cpp:522-575`), and the result byte from
//! `StableResult::AppendBodyTo` (`Npc.cpp:99-102`) with the codes from that file's
//! `StableResultCode` enum (`NPCHandler.cpp:40-47`).

use benilla_protocol::messages::{self, stable_result, StabledPet};
use benilla_protocol::ServerPacket;

fn hx(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

const NPC: u64 = 0x1234_5678_9abc_def0;
const NPC_HEX: &str = "f0debc9a78563412";

#[test]
fn stable_send_bodies_golden() {
    // The three guid-only verbs. They are byte-identical bodies on three different opcodes — the
    // opcode IS the verb — so each is asserted separately rather than through a shared helper: a
    // writer that sent `stable_pet`'s body under `CMSG_BUY_STABLE_SLOT` would still match a shared
    // assertion.
    assert_eq!(
        messages::list_stabled_pets(NPC),
        hx(NPC_HEX),
        "MSG_LIST_STABLED_PETS body"
    );
    assert_eq!(
        messages::stable_pet(NPC),
        hx(NPC_HEX),
        "CMSG_STABLE_PET body"
    );
    assert_eq!(
        messages::buy_stable_slot(NPC),
        hx(NPC_HEX),
        "CMSG_BUY_STABLE_SLOT body"
    );

    // The two pet-number verbs: u64 npcGuid, u32 petNumber. The number is the pet's OWN id
    // (`character_pet.id`), never its slot — a body carrying a slot index would name the wrong pet
    // for every hunter whose stable is not in id order.
    assert_eq!(
        messages::unstable_pet(NPC, 42),
        hx(&format!("{NPC_HEX}2a000000")),
        "CMSG_UNSTABLE_PET body"
    );
    assert_eq!(
        messages::stable_swap_pet(NPC, 42),
        hx(&format!("{NPC_HEX}2a000000")),
        "CMSG_STABLE_SWAP_PET body"
    );
}

/// Append one variable-length pet record to a `MSG_LIST_STABLED_PETS` body, in wire order:
/// `u32 petNumber, u32 creatureEntry, u32 level, cstring name, u32 loyalty, u8 slot` — where
/// `slot` is the **wire** value (1-based), not the client index the decode produces.
fn push_pet(
    body: &mut Vec<u8>,
    pet_number: u32,
    entry: u32,
    level: u32,
    name: &str,
    loyalty: u32,
    wire_slot: u8,
) {
    body.extend_from_slice(&pet_number.to_le_bytes());
    body.extend_from_slice(&entry.to_le_bytes());
    body.extend_from_slice(&level.to_le_bytes());
    body.extend_from_slice(name.as_bytes());
    body.push(0);
    body.extend_from_slice(&loyalty.to_le_bytes());
    body.push(wire_slot);
}

fn list_body(num_stable_slots: u8, pets: &[(u32, u32, u32, &str, u32, u8)]) -> Vec<u8> {
    let mut body = NPC.to_le_bytes().to_vec();
    body.push(pets.len() as u8);
    body.push(num_stable_slots);
    for &(n, e, l, name, loy, slot) in pets {
        push_pet(&mut body, n, e, l, name, loy, slot);
    }
    body
}

fn parse_list(body: &[u8]) -> (u64, u8, Vec<StabledPet>) {
    match messages::parse_server(messages::opcode::MSG_LIST_STABLED_PETS, body).unwrap() {
        ServerPacket::ListStabledPets {
            npc,
            num_stable_slots,
            pets,
        } => (npc, num_stable_slots, pets),
        _ => panic!("expected ListStabledPets"),
    }
}

/// A full hunter: a pet at their side and both stable slots occupied. **The slot rebasing is the
/// assertion that matters** — the wire is 1-based (`SendStablePet` writes a literal `0x01` for the
/// current pet and `it->slot + 1` for a stabled one) and the client is 0-based, so a decode that
/// forwarded the wire byte would put the current pet in stable slot 1 and drop the last pet off the
/// end of a 3-slot window.
#[test]
fn a_full_stable_decodes_with_client_slot_indices() {
    let body = list_body(
        2,
        &[
            (7, 299, 41, "Rex", 6, 1),
            (8, 1126, 38, "Bruiser", 4, 2),
            (9, 883, 12, "Nibbles", 1, 3),
        ],
    );
    let (npc, slots, pets) = parse_list(&body);
    assert_eq!(npc, NPC);
    assert_eq!(slots, 2, "purchased slots, not occupied ones");
    assert_eq!(pets.len(), 3);

    // Wire 1 → client 0 (the current pet), wire 3 → client 2 (the second stable slot).
    assert_eq!(
        pets[0],
        StabledPet {
            pet_number: 7,
            creature_entry: 299,
            level: 41,
            name: "Rex".into(),
            loyalty: 6,
            slot: 0,
        }
    );
    assert_eq!(pets[1].slot, 1);
    assert_eq!(pets[2].slot, 2);
    // The name is a cstring mid-record, so every field after it depends on having consumed it
    // exactly: a mis-read name shifts loyalty and slot together.
    assert_eq!(pets[2].name, "Nibbles");
    assert_eq!((pets[2].loyalty, pets[2].creature_entry), (1, 883));
}

/// A hunter with **no current pet** — the row for slot 0 is simply absent (vmangos emits it only
/// for a live `HUNTER_PET` or a cached one). The rows must therefore be read BY SLOT: a consumer
/// that treated `pets[0]` as the current pet would show a stabled wolf standing at the player's
/// side.
#[test]
fn an_absent_current_pet_leaves_no_slot_zero_row() {
    let body = list_body(1, &[(8, 1126, 38, "Bruiser", 4, 2)]);
    let (_, slots, pets) = parse_list(&body);
    assert_eq!(slots, 1);
    assert_eq!(pets.len(), 1);
    assert_eq!(
        pets[0].slot, 1,
        "the lone row is stable slot 1, not current"
    );
    assert!(!pets.iter().any(|p| p.slot == 0));
}

/// A hunter who has bought a slot and stabled nothing: an empty list with a non-zero slot count.
/// This must decode to an empty vec — the count and the rows are independent numbers, and treating
/// `num_stable_slots` as a row count would over-read the body.
#[test]
fn an_empty_list_still_carries_the_purchased_slot_count() {
    let (_, slots, pets) = parse_list(&list_body(1, &[]));
    assert_eq!(slots, 1);
    assert!(pets.is_empty());

    // And the true zero state: nothing bought, nothing stabled.
    let (_, slots, pets) = parse_list(&list_body(0, &[]));
    assert_eq!(slots, 0);
    assert!(pets.is_empty());
}

/// Every `StableResultCode` vmangos can send, round-tripped. The success codes are distinct
/// numbers but `SUCCESS_UNSTABLE` answers BOTH a plain unstable and a swap, so the reply alone
/// never identifies which verb ran.
#[test]
fn stable_result_codes_round_trip() {
    for code in [
        stable_result::ERR_MONEY,
        stable_result::ERR_STABLE,
        stable_result::SUCCESS_STABLE,
        stable_result::SUCCESS_UNSTABLE,
        stable_result::SUCCESS_BUY_SLOT,
    ] {
        match messages::parse_server(messages::opcode::SMSG_STABLE_RESULT, &[code]).unwrap() {
            ServerPacket::StableResult { result } => assert_eq!(result, code),
            _ => panic!("expected StableResult"),
        }
    }

    // The values themselves, against vmangos's enum — a renumbering here would silently turn a
    // refusal into a success in the app's match.
    assert_eq!(
        [
            stable_result::ERR_MONEY,
            stable_result::ERR_STABLE,
            stable_result::SUCCESS_STABLE,
            stable_result::SUCCESS_UNSTABLE,
            stable_result::SUCCESS_BUY_SLOT,
        ],
        [0x01, 0x06, 0x08, 0x09, 0x0A]
    );
}
