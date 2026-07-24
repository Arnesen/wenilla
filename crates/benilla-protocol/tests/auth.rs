//! Oracle-free regression tests for the realmd login protocol — specifically **stream alignment**.
//! Login packets are not length-framed, so each reader must consume its packet *exactly*; a reader
//! that stops early leaves trailing bytes that desync the next read. These build a back-to-back
//! challenge → proof → realm-list byte stream (the real logon sequence) and read all three from one
//! cursor, which fails loudly if any reader under- or over-consumes.

use benilla_protocol::auth;

const N: [u8; 32] = benilla_srp::LARGE_SAFE_PRIME_LITTLE_ENDIAN;

/// A successful `CMD_AUTH_LOGON_CHALLENGE_Server` body, including the `crc_salt` + `security_flag`
/// tail that must be consumed.
fn challenge_packet(server_public_key: &[u8; 32], salt: &[u8; 32]) -> Vec<u8> {
    let mut p = vec![0x00, 0x00, 0x00]; // opcode, protocol_version, result=success
    p.extend_from_slice(server_public_key);
    p.push(1); // generator length
    p.push(7); // generator
    p.push(32); // large-safe-prime length
    p.extend_from_slice(&N);
    p.extend_from_slice(salt);
    p.extend_from_slice(&[0u8; 16]); // crc_salt
    p.push(0); // security_flag = None
    p
}

fn proof_packet(server_proof: &[u8; 20]) -> Vec<u8> {
    let mut p = vec![0x01, 0x00]; // opcode, result=success
    p.extend_from_slice(server_proof);
    p.extend_from_slice(&0u32.to_le_bytes()); // hardware_survey_id
    p
}

fn realm_list_packet() -> Vec<u8> {
    let mut realms = Vec::new();
    realms.extend_from_slice(&0u32.to_le_bytes()); // realm_type
    realms.push(0); // flag
    realms.extend_from_slice(b"Benilla\0");
    realms.extend_from_slice(b"127.0.0.1:8085\0");
    realms.extend_from_slice(&0.0f32.to_le_bytes()); // population
    realms.push(3); // number_of_characters
    realms.push(0); // category
    realms.push(1); // realm_id

    let mut p = vec![0x10]; // opcode
    let size = (4 + 1 + realms.len() + 2) as u16; // header_padding + count + realms + footer
    p.extend_from_slice(&size.to_le_bytes());
    p.extend_from_slice(&0u32.to_le_bytes()); // header_padding
    p.push(1); // number_of_realms
    p.extend_from_slice(&realms);
    p.extend_from_slice(&0u16.to_le_bytes()); // footer_padding
    p
}

#[test]
fn logon_read_sequence_stays_aligned() {
    let server_public_key: [u8; 32] =
        std::array::from_fn(|i| (i as u8).wrapping_mul(7).wrapping_add(9));
    let salt: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(11).wrapping_add(2));
    let server_proof: [u8; 20] =
        std::array::from_fn(|i| (i as u8).wrapping_mul(13).wrapping_add(4));

    // Concatenate the three replies as the server would stream them back-to-back.
    let mut buf = challenge_packet(&server_public_key, &salt);
    buf.extend_from_slice(&proof_packet(&server_proof));
    buf.extend_from_slice(&realm_list_packet());

    let mut stream: &[u8] = &buf;

    // 1. Challenge — must consume crc_salt + security_flag so the proof read lands on opcode 0x01.
    let reply = auth::read_challenge_reply(&mut stream).expect("challenge reply");
    assert_eq!(reply.server_public_key, server_public_key);
    assert_eq!(reply.generator, 7);
    assert_eq!(reply.large_safe_prime, N);
    assert_eq!(reply.salt, salt);

    // 2. Proof — this is exactly where a challenge-reply under-read used to surface (`got 0xba`).
    let proof =
        auth::read_proof_reply(&mut stream).expect("proof reply (stream desynced if this fails)");
    assert_eq!(proof, server_proof);

    // 3. Realm list — confirms the proof reader also consumed its packet exactly.
    let realms =
        auth::read_realm_list(&mut stream).expect("realm list (stream desynced if this fails)");
    assert_eq!(realms.len(), 1);
    assert_eq!(realms[0].name, "Benilla");
    assert_eq!(realms[0].address, "127.0.0.1:8085");
    assert_eq!(realms[0].characters, 3);

    // The whole stream should be consumed (no leftover bytes).
    assert!(
        stream.is_empty(),
        "{} trailing bytes — a reader over/under-consumed",
        stream.len()
    );
}
