//! Golden tests for the petition opcode family — the guild-charter flow that founds a guild
//! (decision 1672). CMSG bodies are asserted byte-exact against the builder output; SMSG bodies are
//! hand-built per the vmangos layouts (cited inline) and round-tripped through `parse_server` +
//! `decode`. See `tests/common` for the shared `hx()` helper and methodology note.
//!
//! Four of the tests exist for a specific failure mode rather than for coverage, and each was
//! mutation-checked — the code was broken that exact way and the test observed to fail:
//!
//! - `petition_buy_body_is_the_full_seventy_two_byte_frame` — vmangos's reader walks 72 fixed bytes
//!   field by field and never checks the remainder, so a body one field short leaves it reading
//!   garbage and the buy is dropped **silently**. There is no error packet for a malformed buy.
//! - `petition_query_response_gender_is_sixteen_bit` — the one odd width in the family. Read as a
//!   `u32`, every field after it shifts by two bytes and the choice count becomes a huge number.
//! - `petition_show_signatures_stride_is_twelve_bytes` — the trailing zero dword per signature is
//!   easy to omit; a nine-signature charter then reads eight signers and a truncation error, and a
//!   two-signature fixture would still pass.
//! - `msg_petition_opcodes_read_a_different_body_than_they_write` — the two `MSG_` opcodes are
//!   genuinely asymmetric (decline sends an item guid and receives a player guid), which is exactly
//!   the shape a shared body type would paper over.

mod common;

use benilla_protocol::events::{decode, SessionEvent};
use benilla_protocol::messages::{
    self, opcode, petition_result, CHARTER_DISPLAY_ID, CHARTER_ITEM_ENTRY, MAX_PETITION_SIGNATURES,
};
use benilla_protocol::ServerPacket;
use common::hx;

/// Every CMSG builder in the family, byte-exact.
#[test]
fn cmsg_bodies_golden() {
    // CMSG_PETITION_SHOWLIST (vmangos Server/Packets/Petition.cpp:3-6): one u64, the NPC.
    assert_eq!(
        messages::petition_show_list(0x0000_0000_0000_2a1f),
        hx("1f2a000000000000"),
        "CMSG_PETITION_SHOWLIST body"
    );

    // CMSG_PETITION_SHOW_SIGNATURES (Petition.cpp:8-11): one u64, the charter ITEM.
    assert_eq!(
        messages::petition_show_signatures(0x0000_0000_0001_0203),
        hx("0302010000000000"),
        "CMSG_PETITION_SHOW_SIGNATURES body"
    );

    // CMSG_TURN_IN_PETITION (Petition.cpp:24-27): one u64, the item.
    assert_eq!(
        messages::turn_in_petition(0x0000_0000_0001_0203),
        hx("0302010000000000"),
        "CMSG_TURN_IN_PETITION body"
    );

    // MSG_PETITION_DECLINE outbound (Petition.cpp:19-22): one u64 — the ITEM, not the player.
    assert_eq!(
        messages::petition_decline(0x0000_0000_0001_0203),
        hx("0302010000000000"),
        "MSG_PETITION_DECLINE body"
    );

    // CMSG_PETITION_SIGN (Petition.cpp:35-39): u64 item, then an i8 the server reads and skips.
    // The client's own default for that byte is 1, not 0 (`0x4f46d9`) — and because the server
    // discards it, this golden is the ONLY thing that can tell the two apart.
    assert_eq!(
        messages::petition_sign(0x0000_0000_0001_0203, 1),
        hx("030201000000000001"),
        "CMSG_PETITION_SIGN body, the client's default byte"
    );
    assert_eq!(
        messages::petition_sign(0x0000_0000_0001_0203, -1),
        hx("0302010000000000ff"),
        "…and a negative argument rides as its two's complement"
    );

    // CMSG_OFFER_PETITION (Petition.cpp:41-45): u64 item, u64 target player.
    assert_eq!(
        messages::offer_petition(0x0000_0000_0001_0203, 0x0000_0000_0000_00aa),
        hx("0302010000000000aa00000000000000"),
        "CMSG_OFFER_PETITION body"
    );

    // CMSG_PETITION_QUERY (Petition.cpp:13-17): u32 petitionId, u64 itemGuid.
    assert_eq!(
        messages::petition_query(7, 0x0000_0000_0001_0203),
        hx("070000000302010000000000"),
        "CMSG_PETITION_QUERY body"
    );

    // MSG_PETITION_RENAME outbound (Petition.cpp:29-33): u64 item, cstring newName.
    assert_eq!(
        messages::petition_rename(0x0000_0000_0001_0203, "Legacy"),
        hx("03020100000000004c656761637900"),
        "MSG_PETITION_RENAME body"
    );
}

/// `CMSG_PETITION_BUY` is 72 fixed bytes plus the name's, laid out exactly as vmangos's reader
/// walks them (`Server/Packets/Petition.cpp:47-67`): `u64 npc`, `u32 0`, `u64 0`, cstring name,
/// `10 × u32 0`, `u16 0`, `u8 0`, `u32 index`, `u32 0`.
///
/// **Mutation-checked.** Dropping any one of the skipped fields still compiles, still sends, and is
/// answered by nothing at all — the server reads past the end, `IsValidCharterName` sees garbage or
/// the handler returns, and no error packet exists for a malformed buy. This test is the only thing
/// between that and a "the Purchase button does nothing" report.
#[test]
fn petition_buy_body_is_the_full_seventy_two_byte_frame() {
    let body = messages::petition_buy(0x0000_0000_0000_2a1f, "Legacy");
    assert_eq!(
        body,
        hx(concat!(
            "1f2a000000000000", // u64 npcGuid
            "00000000",         // u32 skipped
            "0000000000000000", // u64 skipped
            "4c656761637900",   // cstring "Legacy"
            // 10 × u32 skipped
            "00000000000000000000000000000000000000000000000000000000000000000000000000000000",
            "0000",     // u16 skipped
            "00",       // u8 skipped
            "00000000", // u32 index — the server's own "unused"
            "00000000", // u32 skipped
        )),
        "CMSG_PETITION_BUY body"
    );
    assert_eq!(body.len(), 72 + "Legacy".len(), "72 fixed bytes + the name");

    // The name is the only variable-length part, and it sits at offset 20 — where the server's
    // reader arrives after skipping 8 + 4 + 8 bytes.
    assert_eq!(
        &body[20..27],
        b"Legacy\0",
        "the name starts at offset 20, NUL-terminated"
    );
}

/// `SMSG_PETITION_SHOWLIST` — the packet that opens the guild registrar. Layout from
/// `Server/Packets/Petition.cpp:115-127`, values from `Handlers/PetitionsHandler.cpp:482-507`.
///
/// The list is **counted**, and parsed as one even though vmangos's own header says the reference
/// client supports only one row: a reader that assumed one would desynchronise rather than degrade
/// if a server ever sent two. The fixture sends two for exactly that reason.
#[test]
fn petition_show_list_parses_a_counted_list() {
    let body = hx(concat!(
        "1f2a000000000000", // u64 npcGuid
        "02",               // u8 count — deliberately not vmangos's 1
        // row 1: index 1, entry 5863, display 16161, cost 1000, flags 1
        "01000000",
        "e7160000",
        "213f0000",
        "e8030000",
        "01000000",
        // row 2: index 2, entry 5863, display 16161, cost 2000, flags 0 (the "hidden" case)
        "02000000",
        "e7160000",
        "213f0000",
        "d0070000",
        "00000000",
    ));
    let packet = messages::parse_server(opcode::SMSG_PETITION_SHOWLIST, &body).unwrap();
    let ServerPacket::PetitionShowList(list) = &packet else {
        panic!("expected PetitionShowList, got {}", packet.name());
    };
    assert_eq!(list.npc, 0x2a1f);
    assert_eq!(
        list.entries.len(),
        2,
        "both rows parsed, not just the first"
    );
    assert_eq!(list.entries[0].index, 1);
    assert_eq!(list.entries[0].charter_entry, CHARTER_ITEM_ENTRY);
    assert_eq!(list.entries[0].charter_display_id, CHARTER_DISPLAY_ID);
    assert_eq!(list.entries[0].charter_cost, 1000, "10 silver, in copper");
    assert_eq!(list.entries[0].entry_flags, 1);
    assert_eq!(list.entries[1].charter_cost, 2000);
    assert_eq!(
        list.entries[1].entry_flags, 0,
        "the flag is kept raw, not folded into a bool"
    );

    let events = decode(packet);
    assert!(
        matches!(events.as_slice(), [SessionEvent::PetitionShowList(l)] if l.npc == 0x2a1f),
        "one PetitionShowList event, got {events:?}"
    );
}

/// `SMSG_PETITION_SHOW_SIGNATURES` — `u64 item`, `u64 owner`, `u32 petitionId`, `u8 count`, then
/// **12 bytes per signature**: the signer's guid and a dword vmangos writes as a literal zero
/// (`Handlers/PetitionsHandler.cpp:160-168`, `Guild/GuildMgr.cpp:358-366`).
///
/// **Mutation-checked** against omitting that dword. The fixture carries a full nine signatures
/// because that is the only size at which the drift is unmistakable — with two, an 8-byte stride
/// reads one signer and then a truncation error, which is easy to misread as a short packet.
#[test]
fn petition_show_signatures_stride_is_twelve_bytes() {
    let mut body = hx(concat!(
        "0302010000000000", // u64 itemGuid
        "aa00000000000000", // u64 ownerGuid
        "07000000",         // u32 petitionId
        "09",               // u8 signatureCount
    ));
    for signer in 1..=MAX_PETITION_SIGNATURES as u64 {
        body.extend_from_slice(&signer.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
    }

    let packet = messages::parse_server(opcode::SMSG_PETITION_SHOW_SIGNATURES, &body).unwrap();
    let ServerPacket::PetitionShowSignatures(sigs) = &packet else {
        panic!("expected PetitionShowSignatures, got {}", packet.name());
    };
    assert_eq!(sigs.item, 0x0001_0203);
    assert_eq!(sigs.owner, 0xaa);
    assert_eq!(sigs.petition_id, 7);
    assert_eq!(sigs.signatures.len(), MAX_PETITION_SIGNATURES);
    // The LAST signer is the one that proves the stride: an 8-byte stride would have walked into
    // the padding long before here.
    assert_eq!(
        sigs.signatures[MAX_PETITION_SIGNATURES - 1].signer,
        MAX_PETITION_SIGNATURES as u64,
        "the ninth signer, at stride 12"
    );
    assert!(sigs.signatures.iter().all(|s| s.unknown == 0));

    let events = decode(packet);
    assert!(
        matches!(events.as_slice(), [SessionEvent::PetitionShowSignatures(s)] if s.petition_id == 7),
        "one PetitionShowSignatures event, got {events:?}"
    );
}

/// `SMSG_PETITION_QUERY_RESPONSE` — sixteen fields, one of which is sixteen bits.
///
/// **Mutation-checked** against reading `allowedGender` as a `u32`. That shifts everything after it
/// by two bytes: `allowedMinLevel` picks up half of `allowedMaxLevel`, and the choice count reads a
/// large number, so the parse fails on a truncated cstring — far from the cause. The fixture gives
/// every trailing field a distinct value so the shift cannot land on a lucky zero.
#[test]
fn petition_query_response_gender_is_sixteen_bit() {
    let body = hx(concat!(
        "07000000",         // u32 petitionId
        "aa00000000000000", // u64 ownerGuid
        "4c656761637900",   // cstring name "Legacy"
        "00",               // cstring bodyText ""
        "01000000",         // u32 flags
        "09000000",         // u32 minSignatures
        "09000000",         // u32 maxSignatures
        "11111111",         // u32 deadline
        "22222222",         // u32 creation
        "33333333",         // u32 allowedGuildID
        "44444444",         // u32 allowedClasses
        "55555555",         // u32 allowedRaces
        "6666",             // u16 allowedGender  <-- sixteen bits
        "77777777",         // u32 allowedMinLevel
        "88888888",         // u32 allowedMaxLevel
        "01000000",         // u32 choiceCount
        "59657300",         // cstring "Yes"
        "99999999",         // u32 defaultChoice
    ));
    let packet = messages::parse_server(opcode::SMSG_PETITION_QUERY_RESPONSE, &body).unwrap();
    let ServerPacket::PetitionQueryResponse(r) = &packet else {
        panic!("expected PetitionQueryResponse, got {}", packet.name());
    };
    assert_eq!(r.petition_id, 7);
    assert_eq!(r.owner, 0xaa);
    assert_eq!(r.name, "Legacy");
    assert_eq!(r.body_text, "", "vmangos always sends this empty");
    assert_eq!(r.min_signatures, 9);
    assert_eq!(r.max_signatures, 9);
    assert_eq!(r.allowed_gender, 0x6666, "sixteen bits, not thirty-two");
    // These two are what a 32-bit gender read would corrupt first.
    assert_eq!(r.allowed_min_level, 0x7777_7777);
    assert_eq!(r.allowed_max_level, 0x8888_8888);
    assert_eq!(r.choices, vec!["Yes".to_string()], "the counted tail");
    assert_eq!(r.default_choice, 0x9999_9999);

    let events = decode(packet);
    assert!(
        matches!(events.as_slice(), [SessionEvent::PetitionQueryResponse(q)] if q.name == "Legacy"),
        "one PetitionQueryResponse event, got {events:?}"
    );
}

/// `SMSG_PETITION_SIGN_RESULTS` (`Petition.cpp:69-74`) and `SMSG_TURN_IN_PETITION_RESULTS`
/// (`:76-79`) share the `PetitionSigns` enum but not a layout: the first carries two guids and a
/// code, the second carries **only** the code.
#[test]
fn the_two_results_packets_share_an_enum_but_not_a_layout() {
    let body = hx(concat!(
        "0302010000000000", // u64 itemGuid
        "bb00000000000000", // u64 playerGuid — the SIGNER, in both copies of this packet
        "03000000",         // u32 result = CANT_SIGN_OWN
    ));
    let packet = messages::parse_server(opcode::SMSG_PETITION_SIGN_RESULTS, &body).unwrap();
    let ServerPacket::PetitionSignResults(r) = &packet else {
        panic!("expected PetitionSignResults, got {}", packet.name());
    };
    assert_eq!(r.item, 0x0001_0203);
    assert_eq!(r.player, 0xbb);
    assert_eq!(r.result, petition_result::CANT_SIGN_OWN);

    // The turn-in answer is a bare u32 with no charter named at all.
    let packet = messages::parse_server(
        opcode::SMSG_TURN_IN_PETITION_RESULTS,
        &hx("04000000"), // NEED_MORE
    )
    .unwrap();
    assert!(
        matches!(
            packet,
            ServerPacket::TurnInPetitionResults { result } if result == petition_result::NEED_MORE
        ),
        "expected TurnInPetitionResults, got {}",
        packet.name()
    );
    let events = decode(packet);
    assert!(
        matches!(
            events.as_slice(),
            [SessionEvent::TurnInPetitionResults { result: 4 }]
        ),
        "one TurnInPetitionResults event, got {events:?}"
    );
}

/// The two `MSG_` opcodes are genuinely bidirectional, and **decline reads a different body than it
/// writes**: we send the charter's item guid, the owner receives the declining player's guid. Both
/// are eight bytes, so nothing but this test distinguishes a correct implementation from one that
/// reuses a single body type and is wrong half the time.
///
/// Rename, by contrast, is symmetric — the same `u64 item` + cstring both ways — which is why the
/// two are asserted together: the family is not uniformly one or the other.
#[test]
fn msg_petition_opcodes_read_a_different_body_than_they_write() {
    // Decline: outbound is the ITEM, inbound is the PLAYER. Same width, opposite meaning.
    assert_eq!(
        messages::petition_decline(0x0001_0203),
        hx("0302010000000000"),
        "outbound MSG_PETITION_DECLINE carries the item guid"
    );
    let packet =
        messages::parse_server(opcode::MSG_PETITION_DECLINE, &hx("bb00000000000000")).unwrap();
    assert!(
        matches!(packet, ServerPacket::PetitionDeclined { player } if player == 0xbb),
        "inbound MSG_PETITION_DECLINE carries the player guid, got {}",
        packet.name()
    );

    // Rename: symmetric, and the echo arrives only on success.
    let body = hx("03020100000000004c656761637900");
    assert_eq!(
        messages::petition_rename(0x0001_0203, "Legacy"),
        body,
        "outbound MSG_PETITION_RENAME"
    );
    let packet = messages::parse_server(opcode::MSG_PETITION_RENAME, &body).unwrap();
    let ServerPacket::PetitionRenamed(r) = &packet else {
        panic!("expected PetitionRenamed, got {}", packet.name());
    };
    assert_eq!(r.item, 0x0001_0203);
    assert_eq!(r.name, "Legacy");

    let events = decode(packet);
    assert!(
        matches!(events.as_slice(), [SessionEvent::PetitionRenamed(p)] if p.name == "Legacy"),
        "one PetitionRenamed event, got {events:?}"
    );
}

/// An empty charter — no signatures yet, which is what the owner sees the moment they buy one.
/// The count byte is zero and the packet simply ends; nothing may read a signature record that is
/// not there.
#[test]
fn a_freshly_bought_charter_has_no_signatures() {
    let body = hx(concat!(
        "0302010000000000", // item
        "aa00000000000000", // owner
        "07000000",         // petitionId
        "00",               // no signatures
    ));
    let packet = messages::parse_server(opcode::SMSG_PETITION_SHOW_SIGNATURES, &body).unwrap();
    let ServerPacket::PetitionShowSignatures(sigs) = &packet else {
        panic!("expected PetitionShowSignatures, got {}", packet.name());
    };
    assert!(sigs.signatures.is_empty());
    assert_eq!(sigs.owner, 0xaa, "the owner is still named");
}
