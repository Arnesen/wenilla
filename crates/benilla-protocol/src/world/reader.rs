use std::io::Read;
use std::net::TcpStream;

use anyhow::{anyhow, Result};
use benilla_srp::vanilla_header::DecrypterHalf;

use crate::messages::{self, ServerPacket};

use super::recv_packet;

/// Read half of a split [`WorldSession`](super::WorldSession) — owns a cloned socket + the decrypter. Lives on the network
/// thread, streaming decoded [`crate::SessionEvent`]s (via [`Self::poll`]).
pub struct WorldReader {
    pub(super) stream: TcpStream,
    pub(super) decrypter: DecrypterHalf,
}

impl WorldReader {
    /// Read + decrypt one server packet (blocking).
    pub fn recv(&mut self) -> Result<ServerPacket> {
        recv_packet(&mut self.stream, Some(&mut self.decrypter))
    }

    /// Read one packet and decode it into a [`crate::Poll`]: the typed [`crate::SessionEvent`]s it
    /// produced, or [`crate::Poll::Skipped`] for an unparseable one. Errors only on a real socket
    /// failure (disconnect). The running world model lives in the ECS — the reader is stateless,
    /// turning bytes into events and nothing more.
    ///
    /// `recv_packet` reads the whole body into a buffer before parsing, so a parse error leaves the
    /// stream aligned — we skip that packet rather than tear down the session.
    pub fn poll(&mut self) -> Result<crate::Poll> {
        let mut header = [0u8; 4];
        if let Err(e) = self.stream.read_exact(&mut header) {
            return Err(anyhow!("world stream closed: {e}"));
        }
        self.decrypter.decrypt(&mut header);
        let size = u16::from_be_bytes([header[0], header[1]]);
        let opcode = u16::from_le_bytes([header[2], header[3]]);
        let body_len = size.saturating_sub(2) as usize;
        let mut body = vec![0u8; body_len];
        if let Err(e) = self.stream.read_exact(&mut body) {
            return Err(anyhow!("world stream closed: {e}"));
        }
        match messages::parse_server(opcode, &body) {
            Ok(packet) => Ok(crate::Poll::Events {
                opcode,
                events: crate::decode(packet),
            }),
            // Include the raw body (capped) so an unparseable packet can be decoded by hand — a parse
            // bug is otherwise invisible past "failed to fill whole buffer". The opcode rides
            // separately so the net thread can feed the app's dropped-packet tally.
            Err(e) => Ok(crate::Poll::Skipped {
                opcode,
                reason: format!(
                    "opcode {opcode:#06x} ({}): {e} [{body_len}B: {}]",
                    messages::opcode_name(opcode).unwrap_or("?"),
                    hex_preview(&body, 64)
                ),
            }),
        }
    }
}

/// Space-separated hex of the first `max` bytes of `body` (with a `…` when truncated) — the diagnostic
/// tail on a [`crate::Poll::Skipped`] reason so an unparseable packet's layout can be decoded by hand.
fn hex_preview(body: &[u8], max: usize) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    for b in body.iter().take(max) {
        let _ = write!(s, "{b:02x} ");
    }
    if body.len() > max {
        s.push('…');
    }
    s.trim_end().to_string()
}
