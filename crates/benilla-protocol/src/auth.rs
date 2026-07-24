//! The realmd (login) wire protocol — 1.12.1 uses **login protocol version 3**. In-repo replacement
//! for `wow_login_messages` (decision 0021), scoped to the three exchanges [`crate::logon`] performs:
//! logon challenge, logon proof, and realm list. Login packets are *not* header-encrypted (unlike the
//! world protocol) and each is self-delimiting, so we read them in request/response order.
//!
//! Opcodes: `CMD_AUTH_LOGON_CHALLENGE = 0x00`, `CMD_AUTH_LOGON_PROOF = 0x01`, `CMD_REALM_LIST = 0x10`.

use std::io::{Read, Write};
use std::net::Ipv4Addr;

use anyhow::{bail, Result};

use crate::wire::{read_array, read_cstring, read_f32_le, read_u16_le, read_u32_le, read_u8};
use crate::RealmInfo;

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
pub fn read_challenge_reply(r: &mut impl Read) -> Result<ChallengeReply> {
    let opcode = read_u8(r)?;
    if opcode != CMD_AUTH_LOGON_CHALLENGE {
        bail!("expected CMD_AUTH_LOGON_CHALLENGE (0x00), got {opcode:#x}");
    }
    let _protocol_version = read_u8(r)?;
    let result = read_u8(r)?;
    if result != 0 {
        return Err(AuthReject { code: result }.into());
    }
    let server_public_key = read_array::<32>(r)?;
    let generator_len = read_u8(r)?;
    let mut generator = vec![0u8; generator_len as usize];
    r.read_exact(&mut generator)?;
    let generator = match generator.first() {
        Some(&g) => g,
        None => bail!("server sent an empty generator"),
    };
    let prime_len = read_u8(r)? as usize;
    let mut prime = vec![0u8; prime_len];
    r.read_exact(&mut prime)?;
    let large_safe_prime: [u8; 32] = prime
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("large safe prime was {prime_len} bytes, expected 32"))?;
    let salt = read_array::<32>(r)?;
    // Consume the rest of the packet so the stream stays aligned for the next message — login packets
    // are NOT length-framed, so leftover bytes desync the very next read. crc_salt[16] + security_flag
    // (+ the PIN block if set) are unused for the SRP6 calculation but must still be read off.
    let _crc_salt = read_array::<16>(r)?;
    let security_flag = read_u8(r)?;
    if security_flag & 0x01 != 0 {
        // PIN: pin_grid_seed (u32) + pin_salt[16]. (vmangos sends security_flag = 0; handled anyway.)
        let _pin_grid_seed = read_u32_le(r)?;
        let _pin_salt = read_array::<16>(r)?;
    }
    Ok(ChallengeReply {
        server_public_key,
        generator,
        large_safe_prime,
        salt,
    })
}

/// Send `CMD_AUTH_LOGON_PROOF_Client`. `crc_hash` is the client-binary checksum (vmangos accepts
/// zeros); no telemetry keys, no security flag.
pub fn write_logon_proof(
    w: &mut impl Write,
    client_public_key: &[u8; 32],
    client_proof: &[u8; 20],
) -> std::io::Result<()> {
    let mut packet = Vec::with_capacity(1 + 32 + 20 + 20 + 1 + 1);
    packet.push(CMD_AUTH_LOGON_PROOF);
    packet.extend_from_slice(client_public_key);
    packet.extend_from_slice(client_proof);
    packet.extend_from_slice(&[0u8; 20]); // crc_hash
    packet.push(0); // number_of_telemetry_keys
    packet.push(0); // security_flag = None
    w.write_all(&packet)
}

/// Read the proof reply, returning the server's proof `M2` (or erroring on a non-success result).
pub fn read_proof_reply(r: &mut impl Read) -> Result<[u8; 20]> {
    let opcode = read_u8(r)?;
    if opcode != CMD_AUTH_LOGON_PROOF {
        bail!("expected CMD_AUTH_LOGON_PROOF (0x01), got {opcode:#x}");
    }
    let result = read_u8(r)?;
    if result != 0 {
        // A wrong password fails HERE (the server can't verify M1): vmangos answers 0x04.
        return Err(AuthReject { code: result }.into());
    }
    let server_proof = read_array::<20>(r)?;
    let _hardware_survey_id = read_u32_le(r)?;
    Ok(server_proof)
}

/// Send `CMD_REALM_LIST_Client` (opcode + a `u32` padding of 0).
pub fn write_realm_list_request(w: &mut impl Write) -> std::io::Result<()> {
    let mut packet = [0u8; 5];
    packet[0] = CMD_REALM_LIST;
    w.write_all(&packet)
}

/// Read `CMD_REALM_LIST_Server` into the advertised realms.
pub fn read_realm_list(r: &mut impl Read) -> Result<Vec<RealmInfo>> {
    let opcode = read_u8(r)?;
    if opcode != CMD_REALM_LIST {
        bail!("expected CMD_REALM_LIST (0x10), got {opcode:#x}");
    }
    let _size = read_u16_le(r)?;
    let _header_padding = read_u32_le(r)?;
    let number_of_realms = read_u8(r)?;
    let mut realms = Vec::with_capacity(number_of_realms as usize);
    for _ in 0..number_of_realms {
        let realm_type = read_u32_le(r)?;
        let _flag = read_u8(r)?;
        let name = read_cstring(r)?;
        let address = read_cstring(r)?;
        let population = read_f32_le(r)?;
        let characters = read_u8(r)?;
        let _category = read_u8(r)?;
        let _realm_id = read_u8(r)?;
        realms.push(RealmInfo {
            name,
            address,
            population: format!("{population}"),
            characters,
            realm_type,
        });
    }
    let _footer_padding = read_u16_le(r)?; // consume so the stream stays aligned (see challenge reply)
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
}
