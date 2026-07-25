//! Phase 4: world server (`mangosd`) connection.
//!
//! After the realmd logon ([`crate::logon`]) hands us the SRP6 session key, we connect to the world
//! server, prove we know that key, then stream object updates. The handshake:
//!
//! 1. Read `SMSG_AUTH_CHALLENGE` (unencrypted) → server seed.
//! 2. Send `CMSG_AUTH_SESSION` (unencrypted): build, uppercased account name, a client seed, and a
//!    SHA1 digest binding the session key to both seeds (computed by `benilla-srp`).
//! 3. **1.12 header obfuscation turns on right after** `CMSG_AUTH_SESSION` — every subsequent packet
//!    header (4-byte server / 6-byte client) is encrypted; bodies stay plaintext. The first encrypted
//!    packet we read is `SMSG_AUTH_RESPONSE`.
//! 4. `CMSG_CHAR_ENUM` → `SMSG_CHAR_ENUM`, then `CMSG_PLAYER_LOGIN(guid)`, then the world streams
//!    `SMSG_UPDATE_OBJECT`.
//!
//! Headers are (de)crypted by [`benilla_srp::vanilla_header`]; bodies by [`crate::messages`].

use std::io::{Read, Write};
use std::net::TcpStream;

use anyhow::{anyhow, Result};
use benilla_srp::vanilla_header::{DecrypterHalf, EncrypterHalf};

use crate::messages::{self, ServerPacket};

mod movement;
mod reader;
mod session;
mod writer;

pub use reader::WorldReader;
pub use session::{WardenRequired, WorldSession};
pub use writer::WorldWriter;

/// Default `mangosd` world-server port. Our vmangos deploy maps mangosd to host port 18085 —
/// moved off the classic 8085, which the WOA dev server owns on this machine. Normally the realm
/// list reply carries the port (the deploy's `VMANGOS_REALMLIST_PORT=18085` matches); this
/// constant is the fallback for probes/examples that dial the world server directly.
pub const WORLD_PORT: u16 = 18085;

/// Read one server packet from `stream`: decrypt the 4-byte header, read the body, parse by opcode.
/// `decrypter` is `None` for the (single) unencrypted `SMSG_AUTH_CHALLENGE`.
pub(super) fn recv_packet(
    stream: &mut TcpStream,
    decrypter: Option<&mut DecrypterHalf>,
) -> Result<ServerPacket> {
    let mut header = [0u8; 4];
    stream
        .read_exact(&mut header)
        .map_err(|e| anyhow!("reading world header: {e}"))?;
    if let Some(d) = decrypter {
        d.decrypt(&mut header);
    }
    // size is big-endian and counts the opcode (2) but not the size field; opcode is little-endian.
    let size = u16::from_be_bytes([header[0], header[1]]);
    let opcode = u16::from_le_bytes([header[2], header[3]]);
    let body_len = size.saturating_sub(2) as usize;
    let mut body = vec![0u8; body_len];
    stream
        .read_exact(&mut body)
        .map_err(|e| anyhow!("reading world body (opcode {opcode:#x}, {body_len} bytes): {e}"))?;
    messages::parse_server(opcode, &body).map_err(|e| anyhow!("parsing opcode {opcode:#x}: {e}"))
}

/// Write one client packet: an (optionally encrypted) 6-byte header + plaintext body. The header size
/// field counts the 4-byte opcode plus the body, but not the size field itself.
pub(super) fn send_packet(
    stream: &mut TcpStream,
    encrypter: Option<&mut EncrypterHalf>,
    opcode: u16,
    body: &[u8],
) -> Result<()> {
    let size = (body.len() + 4) as u16;
    let mut header = {
        let s = size.to_be_bytes();
        // The client header's opcode field is 4 bytes wide; widen the 16-bit opcode to fill it.
        let o = u32::from(opcode).to_le_bytes();
        [s[0], s[1], o[0], o[1], o[2], o[3]]
    };
    if let Some(e) = encrypter {
        e.encrypt(&mut header);
    }
    // **One write, not two** (decision 0617). Header-then-body was a self-inflicted Nagle stall: the
    // 6-byte header goes out as its own segment, and the body — small, and now behind unacknowledged
    // data — is held by the kernel until the server ACKs it. With Nagle now off (see
    // [`WorldSession::connect`]) that stall is gone either way, but two segments per packet is still
    // two syscalls and two headers for a ≤40-byte message, and the server cannot parse the packet
    // until both land. Joined here so one movement report is one segment.
    let mut packet = Vec::with_capacity(header.len() + body.len());
    packet.extend_from_slice(&header);
    packet.extend_from_slice(body);
    stream
        .write_all(&packet)
        .map_err(|e| anyhow!("sending opcode {opcode:#x}: {e}"))
}
