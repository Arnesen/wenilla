use anyhow::{anyhow, Result};
use benilla_srp::vanilla_header::DecrypterHalf;

use crate::messages::{self, ServerPacket};
use crate::transport::{ConnReader, ReadExactAsync};

use super::recv_packet;

/// Read half of a split [`WorldSession`](super::WorldSession) — owns the read half of the
/// connection + the decrypter. Lives on the network thread (native) or in the sequencer task
/// (web), streaming decoded [`crate::SessionEvent`]s (via [`Self::poll_async`]).
pub struct WorldReader {
    pub(super) reader: ConnReader,
    pub(super) decrypter: DecrypterHalf,
}

impl WorldReader {
    /// Read + decrypt one server packet (blocking) — the native twin of [`Self::recv_async`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn recv(&mut self) -> Result<ServerPacket> {
        futures_lite::future::block_on(self.recv_async())
    }

    /// Read + decrypt one server packet.
    pub async fn recv_async(&mut self) -> Result<ServerPacket> {
        recv_packet(&mut self.reader, Some(&mut self.decrypter)).await
    }

    /// Read one packet and decode it into a [`crate::Poll`] (blocking) — the native twin of
    /// [`Self::poll_async`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn poll(&mut self) -> Result<crate::Poll> {
        futures_lite::future::block_on(self.poll_async())
    }

    /// Read one packet and decode it into a [`crate::Poll`]: the typed [`crate::SessionEvent`]s it
    /// produced, or [`crate::Poll::Skipped`] for an unparseable one. Errors only on a real socket
    /// failure (disconnect). The running world model lives in the ECS — the reader is stateless,
    /// turning bytes into events and nothing more.
    ///
    /// `recv_packet` reads the whole body into a buffer before parsing, so a parse error leaves the
    /// stream aligned — we skip that packet rather than tear down the session.
    pub async fn poll_async(&mut self) -> Result<crate::Poll> {
        let mut header = [0u8; 4];
        if let Err(e) = self.reader.read_exact_async(&mut header).await {
            return Err(anyhow!("world stream closed: {e}"));
        }
        self.decrypter.decrypt(&mut header);
        let size = u16::from_be_bytes([header[0], header[1]]);
        let opcode = u16::from_le_bytes([header[2], header[3]]);
        let body_len = size.saturating_sub(2) as usize;
        let mut body = vec![0u8; body_len];
        if let Err(e) = self.reader.read_exact_async(&mut body).await {
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
