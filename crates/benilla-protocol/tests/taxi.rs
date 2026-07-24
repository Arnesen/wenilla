//! Oracle-free golden tests for the taxi/flight-master arc's protocol layer (decision 0484 phase
//! 1): the four `CMSG_TAXI*` send verbs, the `SMSG_SHOWTAXINODES` map + `TaxiMask` bit law, the
//! `SMSG_TAXINODE_STATUS` status/learn packet, `SMSG_ACTIVATETAXIREPLY`'s verdict codes, and the
//! empty `SMSG_NEW_TAXI_PATH` ack. Same idioms as `trainer.rs`/`gossip_vendor.rs` — `hx(...)`
//! golden CMSG bodies, hand-built SMSG bodies round-tripped through `parse_server`, each SMSG's
//! `decode()` bridge exercised too.

use benilla_protocol::events::{decode, SessionEvent};
use benilla_protocol::messages::{self, taxi_reply, TaxiMask};
use benilla_protocol::ServerPacket;

fn hx(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn taxi_send_bodies_golden() {
    // CMSG_TAXINODE_STATUS_QUERY / CMSG_TAXIQUERYAVAILABLENODES (vmangos Packets/Taxi.cpp): both
    // a single full 8-byte guid — same shape as CMSG_GOSSIP_HELLO / CMSG_TRAINER_LIST.
    assert_eq!(
        messages::taxi_node_status_query(0x1234_5678_9abc_def0),
        hx("f0debc9a78563412"),
        "CMSG_TAXINODE_STATUS_QUERY body"
    );
    assert_eq!(
        messages::taxi_query_available_nodes(0x1234_5678_9abc_def0),
        hx("f0debc9a78563412"),
        "CMSG_TAXIQUERYAVAILABLENODES body"
    );

    // CMSG_ACTIVATETAXI (vmangos ActivateTaxi::ReadFromWorldPacket): guid, node1, node2 — a
    // single-hop flight, Stormwind (2) -> Sentinel Hill (4).
    assert_eq!(
        messages::activate_taxi(0x1234_5678_9abc_def0, 2, 4),
        hx(concat!("f0debc9a78563412", "02000000", "04000000")),
        "CMSG_ACTIVATETAXI body"
    );

    // CMSG_ACTIVATETAXIEXPRESS (vmangos ActivateTaxiExpress::ReadFromWorldPacket): guid,
    // totalcost, nodeCount, nodeCount x node — a 3-node chain at a combined 110-copper fare.
    assert_eq!(
        messages::activate_taxi_express(0x1234_5678_9abc_def0, 110, &[2, 3, 4]),
        hx(concat!(
            "f0debc9a78563412",
            "6e000000",
            "03000000",
            "02000000",
            "03000000",
            "04000000"
        )),
        "CMSG_ACTIVATETAXIEXPRESS body"
    );
    // An empty node list still writes a zero count, not a truncated body.
    assert_eq!(
        messages::activate_taxi_express(0x1234_5678_9abc_def0, 0, &[]),
        hx(concat!("f0debc9a78563412", "00000000", "00000000")),
        "CMSG_ACTIVATETAXIEXPRESS body, empty node list"
    );
}

#[test]
fn taxi_mask_bit_law() {
    // Bit law (vmangos PlayerTaxi.h IsTaximaskNodeKnown/SetTaximaskNode): 1-based node id,
    // field = (id-1)/32, bit = (id-1)%32. Node 2 -> field 0 bit 1 (0x2); node 4 -> field 0 bit 3
    // (0x8); node 33 -> field 1 bit 0 (0x1) — the first id that crosses into the second word.
    let mask = TaxiMask([0x0000_000A, 0x0000_0001, 0, 0, 0, 0, 0, 0]);
    assert!(mask.is_known(2));
    assert!(mask.is_known(4));
    assert!(mask.is_known(33));
    assert!(!mask.is_known(3), "bit not set for node 3");
    assert!(!mask.is_known(65), "field 2 is all-zero");
    assert!(!mask.is_known(0), "node id 0 is never valid");
}

#[test]
fn show_taxi_nodes_wire() {
    // SMSG_SHOWTAXINODES (vmangos SendTaxiMenu, TaxiHandler.cpp:82-96): u32 windowConstant
    // (always 1), u64 flightmasterGuid (PLAIN, not packed), u32 nearestNode, u32[8] knownMask.
    // 48 bytes fixed. Mask carries node 2 (Stormwind) and node 33 (crosses into the second word).
    let mut body = 1u32.to_le_bytes().to_vec(); // window constant
    body.extend_from_slice(&0xCCu64.to_le_bytes()); // flightmaster guid
    body.extend_from_slice(&2u32.to_le_bytes()); // nearest node
    let mask_words = [0x0000_0002u32, 0x0000_0001, 0, 0, 0, 0, 0, 0];
    for w in mask_words {
        body.extend_from_slice(&w.to_le_bytes());
    }
    assert_eq!(body.len(), 4 + 8 + 4 + 8 * 4, "48-byte fixed body");

    match messages::parse_server(messages::opcode::SMSG_SHOWTAXINODES, &body).unwrap() {
        ServerPacket::ShowTaxiNodes {
            window,
            flightmaster,
            nearest_node,
            known,
        } => {
            assert_eq!((window, flightmaster, nearest_node), (1, 0xCC, 2));
            assert_eq!(known, TaxiMask(mask_words));
            assert!(known.is_known(2));
            assert!(known.is_known(33));
            assert!(!known.is_known(3));
        }
        other => panic!("show taxi nodes, got {}", other.name()),
    }

    // The decode() bridge drops the gate word and carries the rest straight through.
    let packet = messages::parse_server(messages::opcode::SMSG_SHOWTAXINODES, &body).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::TaxiNodesShown {
            flightmaster,
            nearest_node,
            known_mask,
        } => {
            assert_eq!((flightmaster, nearest_node), (0xCC, 2));
            assert_eq!(known_mask, TaxiMask(mask_words));
        }
        other => panic!("taxi nodes shown event, got {other:?}"),
    }

    // The leading word is the client parser's own conditional gate (0496 §claim-1): a zero gate
    // ends the body right there — 4 bytes total, and no event out of decode. vmangos never sends
    // it; the parse is faithful anyway.
    let gated = 0u32.to_le_bytes().to_vec();
    let packet = messages::parse_server(messages::opcode::SMSG_SHOWTAXINODES, &gated).unwrap();
    assert!(
        decode(packet).is_empty(),
        "a zero-gated SHOWTAXINODES carries no menu and yields no event"
    );
}

#[test]
fn taxi_node_status_wire() {
    // SMSG_TAXINODE_STATUS (vmangos TaxiNodeStatus::AppendBodyTo): u64 guid (PLAIN), u8 known
    // (a C++ bool — one byte). Exercise both known and unknown.
    let mut known_body = 0xCCu64.to_le_bytes().to_vec();
    known_body.push(1);
    match messages::parse_server(messages::opcode::SMSG_TAXINODE_STATUS, &known_body).unwrap() {
        ServerPacket::TaxiNodeStatus { guid, known } => assert_eq!((guid, known), (0xCC, true)),
        other => panic!("taxi node status, got {}", other.name()),
    }
    let packet =
        messages::parse_server(messages::opcode::SMSG_TAXINODE_STATUS, &known_body).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::TaxiNodeStatus { guid, known } => assert_eq!((guid, known), (0xCC, true)),
        other => panic!("taxi node status event, got {other:?}"),
    }

    let mut unknown_body = 0xDDu64.to_le_bytes().to_vec();
    unknown_body.push(0);
    match messages::parse_server(messages::opcode::SMSG_TAXINODE_STATUS, &unknown_body).unwrap() {
        ServerPacket::TaxiNodeStatus { guid, known } => assert_eq!((guid, known), (0xDD, false)),
        other => panic!("taxi node status (unknown), got {}", other.name()),
    }
}

#[test]
fn activate_taxi_reply_and_new_taxi_path_wire() {
    // SMSG_ACTIVATETAXIREPLY (vmangos ActivateTaxiReply::AppendBodyTo): one u32 TaxiError code.
    let body = taxi_reply::NOT_ENOUGH_MONEY.to_le_bytes().to_vec();
    match messages::parse_server(messages::opcode::SMSG_ACTIVATETAXIREPLY, &body).unwrap() {
        ServerPacket::ActivateTaxiReply { code } => {
            assert_eq!(code, taxi_reply::NOT_ENOUGH_MONEY);
        }
        other => panic!("activate taxi reply, got {}", other.name()),
    }
    let packet = messages::parse_server(messages::opcode::SMSG_ACTIVATETAXIREPLY, &body).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::ActivateTaxiReply { code } => {
            assert_eq!(code, taxi_reply::NOT_ENOUGH_MONEY);
        }
        other => panic!("activate taxi reply event, got {other:?}"),
    }

    // TaxiError values (vmangos Objects/Player.h:404-416) — pinned so a future edit can't drift
    // them. Codes actually emitted by Player::ActivateTaxiPathTo (this session's grep of
    // Player.cpp:18008-18252): OK, UNSPECIFIED_SERVER_ERROR, NO_SUCH_PATH, NOT_ENOUGH_MONEY,
    // TOO_FAR, BUSY, ALREADY_MOUNTED, SHAPESHIFTED. The rest are defined but never sent.
    // (13-wide, so an array rather than a tuple — Rust's tuple trait impls stop at 12.)
    assert_eq!(
        [
            taxi_reply::OK,
            taxi_reply::UNSPECIFIED_SERVER_ERROR,
            taxi_reply::NO_SUCH_PATH,
            taxi_reply::NOT_ENOUGH_MONEY,
            taxi_reply::TOO_FAR,
            taxi_reply::NO_VENDOR_NEARBY,
            taxi_reply::NOT_VISITED,
            taxi_reply::BUSY,
            taxi_reply::ALREADY_MOUNTED,
            taxi_reply::SHAPESHIFTED,
            taxi_reply::PLAYER_MOVING,
            taxi_reply::SAME_NODE,
            taxi_reply::NOT_STANDING,
        ],
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
    );

    // SMSG_NEW_TAXI_PATH — empty body (vmangos NewTaxiPath::AppendBodyTo writes nothing).
    match messages::parse_server(messages::opcode::SMSG_NEW_TAXI_PATH, &[]).unwrap() {
        ServerPacket::NewTaxiPath => {}
        other => panic!("new taxi path, got {}", other.name()),
    }
    let packet = messages::parse_server(messages::opcode::SMSG_NEW_TAXI_PATH, &[]).unwrap();
    match decode(packet).pop().unwrap() {
        SessionEvent::NewTaxiPath => {}
        other => panic!("new taxi path event, got {other:?}"),
    }
}
