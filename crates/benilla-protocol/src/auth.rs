//! The realmd (login) wire protocol — 1.12.1 uses **login protocol version 3**. In-repo replacement
//! for `wow_login_messages` (decision 0021), scoped to the three exchanges [`crate::logon`] performs:
//! logon challenge, logon proof, and realm list. Login packets are *not* header-encrypted (unlike the
//! world protocol) and each is self-delimiting, so we read them in request/response order.
//!
//! Opcodes: `CMD_AUTH_LOGON_CHALLENGE = 0x00`, `CMD_AUTH_LOGON_PROOF = 0x01`, `CMD_REALM_LIST = 0x10`.

use std::io::Write;
use std::net::Ipv4Addr;

use anyhow::{bail, Result};
use sha1::{Digest, Sha1};

use crate::transport::ReadExactAsync;
use crate::RealmInfo;

// ── The awaited wire readers ──────────────────────────────────────────────────────────────────────
//
// [`crate::wire`]'s readers take `impl Read`, which the browser cannot supply: a WebSocket frame
// arrives on the very event loop a blocking read would be sitting on. These are the same four
// encodings this protocol needs, byte for byte, over [`ReadExactAsync`] instead — every login field
// is fixed-width or length-prefixed, so each one is a `read_exact` and the parsing below is
// unchanged. Private: the world protocol reads its bodies out of a buffer, so this is the only
// place that decodes straight off the wire.

/// Read a fixed-length byte array.
async fn read_array_async<const N: usize>(r: &mut impl ReadExactAsync) -> std::io::Result<[u8; N]> {
    let mut b = [0u8; N];
    r.read_exact_async(&mut b).await?;
    Ok(b)
}

async fn read_u8_async(r: &mut impl ReadExactAsync) -> std::io::Result<u8> {
    Ok(read_array_async::<1>(r).await?[0])
}

async fn read_u16_le_async(r: &mut impl ReadExactAsync) -> std::io::Result<u16> {
    Ok(u16::from_le_bytes(read_array_async::<2>(r).await?))
}

async fn read_u32_le_async(r: &mut impl ReadExactAsync) -> std::io::Result<u32> {
    Ok(u32::from_le_bytes(read_array_async::<4>(r).await?))
}

async fn read_f32_le_async(r: &mut impl ReadExactAsync) -> std::io::Result<f32> {
    Ok(f32::from_bits(read_u32_le_async(r).await?))
}

/// Read a NUL-terminated string (the realm list's names and addresses).
async fn read_cstring_async(r: &mut impl ReadExactAsync) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    loop {
        let b = read_u8_async(r).await?;
        if b == 0 {
            break;
        }
        bytes.push(b);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

const CMD_AUTH_LOGON_CHALLENGE: u8 = 0x00;
const CMD_AUTH_LOGON_PROOF: u8 = 0x01;
const CMD_REALM_LIST: u8 = 0x10;

const PROTOCOL_VERSION_THREE: u8 = 3;
const GAME_NAME_WOW: u32 = 0x0057_6f57; // "WoW\0" little-endian
const PLATFORM_X86: u32 = 0x0078_3836; // "x86\0"
                                       // The OS/platform tags ride the wire *reversed*: the real client stores them as little-endian u32s,
                                       // so `0x0057696e` writes the bytes `n i W \0`, and realmd reverses them back (`AuthSocket.cpp`:
                                       // `std::reverse(m_os.begin(), m_os.end())`). vmangos accepts exactly "Win" and "OSX"
                                       // (`WorldSocket::HandleAuthSession`) and fails the session on anything else.
const OS_WINDOWS: u32 = 0x0057_696e; // "Win\0"
const OS_MACOS: u32 = 0x004F_5358; // "OSX\0"

/// The OS tag for the host we are actually running on. We hardcoded Windows on every platform,
/// which is plainly false on a Mac — and servers act on this field: vmangos picks `WardenWin` vs
/// `WardenMac` from it, and realmd validates the build against it (`FindBuildInfo(build, os,
/// platform)`). There was never a 1.12 Linux client, so every non-Mac host keeps saying Windows —
/// the closest true thing we can say.
const fn client_os() -> u32 {
    if cfg!(target_os = "macos") {
        OS_MACOS
    } else {
        OS_WINDOWS
    }
}
const LOCALE_EN_US: u32 = 0x656e_5553; // "enUS"

/// A non-success auth result byte from the server (challenge or proof stage), typed so the app can
/// map the code to the client's own `AUTH_*` glue string (decision 0539). Rides [`crate::logon`]'s
/// `anyhow` chain — `err.downcast_ref::<AuthReject>()` recovers the code through the contexts.
/// vmangos note (its `AuthCodes.h`, verified): unknown account AND wrong password both arrive as
/// 0x04 (`WOW_FAIL_UNKNOWN_ACCOUNT`) — the client locks out after an 0x05, so the server avoids it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthReject {
    /// The grunt result byte (`WOW_FAIL_*`).
    pub code: u8,
}

impl std::fmt::Display for AuthReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "server rejected logon: result {:#04x}", self.code)
    }
}

impl std::error::Error for AuthReject {}

/// The SRP6 inputs from a successful `CMD_AUTH_LOGON_CHALLENGE_Server`.
pub struct ChallengeReply {
    pub server_public_key: [u8; 32],
    pub generator: u8,
    pub large_safe_prime: [u8; 32],
    pub salt: [u8; 32],
    /// The 16-byte version challenge the client answers with `crc_hash` — see [`version_proof`].
    /// Not an SRP6 input; it seeds the client's binary-integrity digest.
    pub crc_salt: [u8; 16],
}

/// Send `CMD_AUTH_LOGON_CHALLENGE_Client`. `account_name` must already be uppercased (it is what the
/// SRP6 hashes use). `build` is the client build (5875).
pub fn write_logon_challenge(
    w: &mut impl Write,
    account_name: &str,
    build: u16,
) -> std::io::Result<()> {
    // Everything after the 2-byte size field, assembled first so we can prefix its length.
    let mut body = Vec::with_capacity(34 + account_name.len());
    body.extend_from_slice(&GAME_NAME_WOW.to_le_bytes());
    body.push(1); // version major
    body.push(12); // version minor
    body.push(1); // version patch
    body.extend_from_slice(&build.to_le_bytes());
    body.extend_from_slice(&PLATFORM_X86.to_le_bytes());
    body.extend_from_slice(&client_os().to_le_bytes());
    body.extend_from_slice(&LOCALE_EN_US.to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes()); // utc_timezone_offset
    body.extend_from_slice(&Ipv4Addr::LOCALHOST.octets()); // client_ip_address (network order)
    body.push(account_name.len() as u8);
    body.extend_from_slice(account_name.as_bytes());

    let mut packet = Vec::with_capacity(4 + body.len());
    packet.push(CMD_AUTH_LOGON_CHALLENGE);
    packet.push(PROTOCOL_VERSION_THREE);
    packet.extend_from_slice(&(body.len() as u16).to_le_bytes());
    packet.extend_from_slice(&body);
    w.write_all(&packet)
}

/// Read the challenge reply, returning the SRP6 inputs (or erroring on a non-success result).
pub async fn read_challenge_reply(r: &mut impl ReadExactAsync) -> Result<ChallengeReply> {
    let opcode = read_u8_async(r).await?;
    if opcode != CMD_AUTH_LOGON_CHALLENGE {
        bail!("expected CMD_AUTH_LOGON_CHALLENGE (0x00), got {opcode:#x}");
    }
    let _protocol_version = read_u8_async(r).await?;
    let result = read_u8_async(r).await?;
    if result != 0 {
        return Err(AuthReject { code: result }.into());
    }
    let server_public_key = read_array_async::<32>(r).await?;
    let generator_len = read_u8_async(r).await?;
    let mut generator = vec![0u8; generator_len as usize];
    r.read_exact_async(&mut generator).await?;
    let generator = match generator.first() {
        Some(&g) => g,
        None => bail!("server sent an empty generator"),
    };
    let prime_len = read_u8_async(r).await? as usize;
    let mut prime = vec![0u8; prime_len];
    r.read_exact_async(&mut prime).await?;
    let large_safe_prime: [u8; 32] = prime
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("large safe prime was {prime_len} bytes, expected 32"))?;
    let salt = read_array_async::<32>(r).await?;
    // Consume the rest of the packet so the stream stays aligned for the next message — login packets
    // are NOT length-framed, so leftover bytes desync the very next read. crc_salt[16] feeds the
    // integrity proof (not SRP6); security_flag (+ the PIN block if set) is unused but must still be
    // read off.
    let crc_salt = read_array_async::<16>(r).await?;
    let security_flag = read_u8_async(r).await?;
    if security_flag & 0x01 != 0 {
        // PIN: pin_grid_seed (u32) + pin_salt[16]. (vmangos sends security_flag = 0; handled anyway.)
        let _pin_grid_seed = read_u32_le_async(r).await?;
        let _pin_salt = read_array_async::<16>(r).await?;
    }
    Ok(ChallengeReply {
        server_public_key,
        generator,
        large_safe_prime,
        salt,
        crc_salt,
    })
}

// --- the version (client-integrity) proof ---------------------------------------------------------
//
// The proof packet's `crc_hash` answers the challenge's `crc_salt`: the real client scans its own
// binaries under that salt into a 20-byte digest `H`, and sends `SHA1(A ‖ H)`. realmd recomputes the
// same expression from a stored `H` and rejects a mismatch as a *modified client*
// (vmangos `AuthSocket::VerifyVersion`, cmangos ditto — byte-identical implementations). We sent
// twenty zeros here for the project's whole life, which is invisible until a server turns the check
// on: `StrictVersionCheck = 1` then answers a correct password with `WOW_FAIL_VERSION_INVALID` (0x09,
// our login screen's "Wrong client version") — B241. Note vmangos's own shipped
// `realmd.conf.example` sets 1, so this is a *default* on a fresh repack, not exotic hardening.

/// The 16-byte `crc_salt` every mangos-family realmd sends — a **constant, not a nonce** (vmangos
/// `realmd/AuthSocket.cpp:65` and cmangos `realmd/AuthSocket.cpp:187`, two independent copies of the
/// same bytes). The whole scheme rests on that: because every server issues the same challenge, the
/// client's answer is a per-build constant a server can store without owning the client.
const MANGOS_VERSION_CHALLENGE: [u8; 16] = [
    0xba, 0xa3, 0x1e, 0x99, 0xa0, 0x0b, 0x21, 0x57, 0xfc, 0x37, 0x3f, 0xb3, 0x69, 0xcd, 0xd2, 0xf1,
];

/// `H` for build 5875 under [`MANGOS_VERSION_CHALLENGE`], Windows client. Cross-checked between two
/// independent emulator lineages: vmangos ships it as DB data (`sql/migrations/20221117065844_logon.sql`,
/// `allowed_clients` row `1,12,1,'',5875,'Win','x86'`), cmangos as a hardcoded table
/// (`realmd/RealmList.cpp`, `ExpectedRealmdClientBuilds`) — byte-identical.
const INTEGRITY_HASH_5875_WINDOWS: [u8; 20] = [
    0x95, 0xed, 0xb2, 0x7c, 0x78, 0x23, 0xb3, 0x63, 0xcb, 0xdd, 0xab, 0x56, 0xa3, 0x92, 0xe7, 0xcb,
    0x73, 0xfc, 0xca, 0x20,
];

/// `H` for build 5875 under [`MANGOS_VERSION_CHALLENGE`], Mac client — same two sources, and the one
/// we must answer with on macOS, where [`client_os`] says `OSX` (vmangos keys its lookup on the os
/// field, cmangos picks `MacHash` off the same field).
const INTEGRITY_HASH_5875_MACOS: [u8; 20] = [
    0x8d, 0x17, 0x3c, 0xc3, 0x81, 0x96, 0x1e, 0xeb, 0xab, 0xf3, 0x36, 0xf5, 0xe6, 0x67, 0x5b, 0x10,
    0x1b, 0xb5, 0x13, 0xe5,
];

/// The 20-byte integrity digest to answer `crc_salt` with, or `None` when we have no answer for that
/// salt.
///
/// We hold `H` only for the one salt above, so a server that issues a *different* one gets zeros
/// rather than a stale constant. That is not defensive padding: replaying a constant computed for
/// another challenge is exactly the tell a randomised challenge would be looking for, and it cannot
/// pass anyway. (No server in the mangos family randomises it — a per-connection salt would make
/// their own stored hashes useless — so this branch is the honest answer to a hypothetical, and free.)
fn integrity_hash(crc_salt: &[u8; 16]) -> Option<[u8; 20]> {
    if *crc_salt != MANGOS_VERSION_CHALLENGE {
        return None;
    }
    Some(if client_os() == OS_MACOS {
        INTEGRITY_HASH_5875_MACOS
    } else {
        INTEGRITY_HASH_5875_WINDOWS
    })
}

/// The proof packet's `crc_hash`: `SHA1(A ‖ H)` over the wire bytes of `A` (realmd hashes `lp->A`
/// as received, so this is the same 32 bytes we put in the packet). Twenty zeros when we have no
/// `H` for this salt — the pre-B241 behaviour, which a non-strict server ignores.
pub fn version_proof(crc_salt: &[u8; 16], client_public_key: &[u8; 32]) -> [u8; 20] {
    match integrity_hash(crc_salt) {
        Some(h) => {
            let mut sha = Sha1::new();
            sha.update(client_public_key);
            sha.update(h);
            sha.finalize().into()
        }
        None => [0u8; 20],
    }
}

/// Send `CMD_AUTH_LOGON_PROOF_Client`. `crc_salt` is the challenge's — the packet's `crc_hash` is
/// computed here ([`version_proof`]) rather than passed in, so no caller can forget it. No telemetry
/// keys, no security flag.
pub fn write_logon_proof(
    w: &mut impl Write,
    client_public_key: &[u8; 32],
    client_proof: &[u8; 20],
    crc_salt: &[u8; 16],
) -> std::io::Result<()> {
    let mut packet = Vec::with_capacity(1 + 32 + 20 + 20 + 1 + 1);
    packet.push(CMD_AUTH_LOGON_PROOF);
    packet.extend_from_slice(client_public_key);
    packet.extend_from_slice(client_proof);
    packet.extend_from_slice(&version_proof(crc_salt, client_public_key));
    packet.push(0); // number_of_telemetry_keys
    packet.push(0); // security_flag = None
    w.write_all(&packet)
}

/// Read the proof reply, returning the server's proof `M2` (or erroring on a non-success result).
pub async fn read_proof_reply(r: &mut impl ReadExactAsync) -> Result<[u8; 20]> {
    let opcode = read_u8_async(r).await?;
    if opcode != CMD_AUTH_LOGON_PROOF {
        bail!("expected CMD_AUTH_LOGON_PROOF (0x01), got {opcode:#x}");
    }
    let result = read_u8_async(r).await?;
    if result != 0 {
        // A wrong password fails HERE (the server can't verify M1): vmangos answers 0x04.
        return Err(AuthReject { code: result }.into());
    }
    let server_proof = read_array_async::<20>(r).await?;
    let _hardware_survey_id = read_u32_le_async(r).await?;
    Ok(server_proof)
}

/// Send `CMD_REALM_LIST_Client` (opcode + a `u32` padding of 0).
pub fn write_realm_list_request(w: &mut impl Write) -> std::io::Result<()> {
    let mut packet = [0u8; 5];
    packet[0] = CMD_REALM_LIST;
    w.write_all(&packet)
}

/// Read `CMD_REALM_LIST_Server` into the advertised realms.
pub async fn read_realm_list(r: &mut impl ReadExactAsync) -> Result<Vec<RealmInfo>> {
    let opcode = read_u8_async(r).await?;
    if opcode != CMD_REALM_LIST {
        bail!("expected CMD_REALM_LIST (0x10), got {opcode:#x}");
    }
    let _size = read_u16_le_async(r).await?;
    let _header_padding = read_u32_le_async(r).await?;
    let number_of_realms = read_u8_async(r).await?;
    let mut realms = Vec::with_capacity(number_of_realms as usize);
    for _ in 0..number_of_realms {
        let realm_type = read_u32_le_async(r).await?;
        let _flag = read_u8_async(r).await?;
        let name = read_cstring_async(r).await?;
        let address = read_cstring_async(r).await?;
        let population = read_f32_le_async(r).await?;
        let characters = read_u8_async(r).await?;
        let _category = read_u8_async(r).await?;
        let _realm_id = read_u8_async(r).await?;
        realms.push(RealmInfo {
            name,
            address,
            population: format!("{population}"),
            characters,
            realm_type,
        });
    }
    let _footer_padding = read_u16_le_async(r).await?; // consume so the stream stays aligned (see challenge reply)
    Ok(realms)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The OS/platform tags are reversed on the wire (the real client's little-endian u32s), and a
    /// server that reads anything but "Win"/"OSX" fails the session — so pin the byte order, not
    /// just the constant. Reversing the wire bytes must spell the tag.
    #[test]
    fn os_tags_reverse_to_the_names_servers_match_on() {
        let spelled = |tag: u32| {
            let mut b = tag.to_le_bytes();
            b.reverse();
            String::from_utf8(b.iter().copied().filter(|&c| c != 0).collect()).unwrap()
        };
        assert_eq!(spelled(OS_WINDOWS), "Win");
        assert_eq!(spelled(OS_MACOS), "OSX");
        assert_eq!(spelled(PLATFORM_X86), "x86");
    }

    /// We advertise the host we actually run on — Windows everywhere except macOS.
    #[test]
    fn client_os_follows_the_host() {
        if cfg!(target_os = "macos") {
            assert_eq!(client_os(), OS_MACOS);
        } else {
            assert_eq!(client_os(), OS_WINDOWS);
        }
    }

    /// An arbitrary but fixed `A` for the version-proof vectors below.
    fn test_public_key() -> [u8; 32] {
        std::array::from_fn(|i| (i as u8).wrapping_mul(7).wrapping_add(9))
    }

    /// Known answers, computed *outside* this crate (Python `hashlib`) from realmd's own expression
    /// `SHA1(A ‖ H)` and the emulator-sourced `H`. A regression here means either constant moved or
    /// the hash grew an extra input — both of which read as "modified client" to a strict server and
    /// are otherwise invisible until someone tries to log in.
    #[test]
    fn version_proof_matches_the_realmd_expression() {
        let expected = if cfg!(target_os = "macos") {
            "dccd0e67946fae221451513b1a59724090ff6096"
        } else {
            "9b95cd41edd719fddf237294b8aca17010e71703"
        };
        let got = version_proof(&MANGOS_VERSION_CHALLENGE, &test_public_key());
        assert_eq!(
            got.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            expected
        );
    }

    /// A salt we hold no `H` for is answered with zeros, not with the constant for another salt.
    #[test]
    fn an_unknown_version_challenge_is_answered_with_zeros() {
        let mut salt = MANGOS_VERSION_CHALLENGE;
        salt[0] ^= 0xff;
        assert_eq!(version_proof(&salt, &test_public_key()), [0u8; 20]);
    }

    /// The proof packet carries the answer at the offset realmd reads it from: `opcode · A[32] ·
    /// M1[20] · crc_hash[20] · num_keys · security_flag`, 75 bytes total.
    #[test]
    fn the_proof_packet_carries_the_version_proof() {
        let a = test_public_key();
        let m1: [u8; 20] = std::array::from_fn(|i| (i as u8).wrapping_mul(13).wrapping_add(4));
        let mut packet = Vec::new();
        write_logon_proof(&mut packet, &a, &m1, &MANGOS_VERSION_CHALLENGE).unwrap();

        assert_eq!(packet.len(), 1 + 32 + 20 + 20 + 1 + 1);
        assert_eq!(packet[0], CMD_AUTH_LOGON_PROOF);
        assert_eq!(&packet[1..33], &a);
        assert_eq!(&packet[33..53], &m1);
        assert_eq!(
            &packet[53..73],
            &version_proof(&MANGOS_VERSION_CHALLENGE, &a)
        );
        assert_eq!(&packet[73..], &[0, 0]);
    }
}
