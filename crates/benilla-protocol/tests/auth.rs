//! Oracle-free regression tests for the realmd login protocol — specifically **stream alignment**.
//! Login packets are not length-framed, so each reader must consume its packet *exactly*; a reader
//! that stops early leaves trailing bytes that desync the next read. These build a back-to-back
//! challenge → proof → realm-list byte stream (the real logon sequence) and read all three from one
//! cursor, which fails loudly if any reader under- or over-consumes.

use benilla_protocol::auth;
use benilla_protocol::transport::ReadExactAsync;
use futures_lite::future::block_on;

/// The login readers are awaited now — a browser reads its WebSocket off the event loop, never out
/// of a blocking call — so the fixture's byte cursor takes the same shape. It reads off the front
/// of the slice and leaves the remainder behind, which is what the alignment assertion at the end
/// of the test inspects.
struct Cursor<'a>(&'a [u8]);

impl ReadExactAsync for Cursor<'_> {
    async fn read_exact_async(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
        std::io::Read::read_exact(&mut self.0, buf)
    }
}

const N: [u8; 32] = benilla_srp::LARGE_SAFE_PRIME_LITTLE_ENDIAN;

/// The `crc_salt` a real mangos-family realmd sends (its fixed `VersionChallenge`). The fixture uses
/// the live bytes rather than zeros, so the reader meets what it actually meets — and so the `got
/// 0xba` in the proof-read comment below names a real byte of this array.
const CRC_SALT: [u8; 16] = [
    0xba, 0xa3, 0x1e, 0x99, 0xa0, 0x0b, 0x21, 0x57, 0xfc, 0x37, 0x3f, 0xb3, 0x69, 0xcd, 0xd2, 0xf1,
];

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
    p.extend_from_slice(&CRC_SALT);
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

    let mut stream = Cursor(&buf);

    // 1. Challenge — must consume crc_salt + security_flag so the proof read lands on opcode 0x01.
    let reply = block_on(auth::read_challenge_reply(&mut stream)).expect("challenge reply");
    assert_eq!(reply.server_public_key, server_public_key);
    assert_eq!(reply.generator, 7);
    assert_eq!(reply.large_safe_prime, N);
    assert_eq!(reply.salt, salt);
    assert_eq!(reply.crc_salt, CRC_SALT); // the proof's `crc_hash` is computed from this

    // 2. Proof — this is exactly where a challenge-reply under-read used to surface (`got 0xba`).
    let proof = block_on(auth::read_proof_reply(&mut stream))
        .expect("proof reply (stream desynced if this fails)");
    assert_eq!(proof, server_proof);

    // 3. Realm list — confirms the proof reader also consumed its packet exactly.
    let realms = block_on(auth::read_realm_list(&mut stream))
        .expect("realm list (stream desynced if this fails)");
    assert_eq!(realms.len(), 1);
    assert_eq!(realms[0].name, "Benilla");
    assert_eq!(realms[0].address, "127.0.0.1:8085");
    assert_eq!(realms[0].characters, 3);

    // The whole stream should be consumed (no leftover bytes).
    assert!(
        stream.0.is_empty(),
        "{} trailing bytes — a reader over/under-consumed",
        stream.0.len()
    );
}
