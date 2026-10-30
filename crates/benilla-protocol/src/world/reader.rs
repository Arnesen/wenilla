use anyhow::{anyhow, Result};
use benilla_srp::vanilla_header::DecrypterHalf;

use crate::messages::{self, ServerPacket};
use crate::transport::ConnReader;

use super::{read_frame_async, recv_packet};

/// The read half of a split [`WorldSession`](super::WorldSession): the connection's read half and
/// decrypter, and what the connection learned before the split, for
/// [`WorldSession::rejoin`](super::WorldSession::rejoin). Lives on the network thread (native) or
/// in the sequencer task (web), streaming decoded [`crate::SessionEvent`]s (via
/// [`Self::poll_async`]).
pub struct WorldReader {
    pub(super) reader: ConnReader,
    pub(super) decrypter: DecrypterHalf,
    pub(super) link: super::session::LinkFacts,
    pub(super) traffic: std::sync::Arc<super::LinkTraffic>,
}

impl WorldReader {
    /// Read + decrypt one server packet (blocking) — the native twin of [`Self::recv_async`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn recv(&mut self) -> Result<ServerPacket> {
        futures_lite::future::block_on(self.recv_async())
    }

    /// Read + decrypt one server packet.
    pub async fn recv_async(&mut self) -> Result<ServerPacket> {
        recv_packet(&mut self.reader, Some(&mut self.decrypter), &self.traffic).await
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
        let (opcode, body) =
            read_frame_async(&mut self.reader, Some(&mut self.decrypter), &self.traffic)
                .await
                .map_err(|e| anyhow!("world stream closed: {e}"))?;
        let arrived = crate::Instant::now();
        match messages::parse_server_with_tail(opcode, &body) {
            Ok((packet, tail)) => {
                let mut events = crate::decode(packet);
                stamp_arrival(&mut events, arrived);
                Ok(crate::Poll::Events {
                    opcode,
                    events,
                    tail,
                    body,
                })
            }
            Err(e) => Ok(crate::Poll::Skipped {
                opcode,
                reason: format!(
                    "opcode {opcode:#06x} ({}): {e} [{}B: {}]",
                    messages::opcode_name(opcode).unwrap_or("?"),
                    body.len(),
                    hex_preview(&body, 64)
                ),
                body,
            }),
        }
    }
}

/// Stamp every transport create of one packet with the instant it was read, as the reference's
/// network thread stamps a packet's queue node (`0x538851`) and its create runs with that time.
fn stamp_arrival(events: &mut [crate::SessionEvent], at: crate::Instant) {
    for ev in events {
        if let crate::SessionEvent::ObjectCreate {
            transport_progress: Some(clock),
            ..
        } = ev
        {
            clock.arrived = Some(at);
        }
    }
}

/// Hex of the first `max` bytes of `body`, `…` when truncated, for decoding a packet by hand.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EntityKind, ObjectFields, SessionEvent, TransportClock};

    /// Every transport create of a packet takes the packet's read instant; nothing else changes.
    #[test]
    fn a_packets_transport_creates_take_its_arrival() {
        let create = |progress: Option<u32>| SessionEvent::ObjectCreate {
            guid: 1,
            kind: EntityKind::GameObject,
            display_id: None,
            position: [0.0; 3],
            orientation: 0.0,
            scale: 1.0,
            speeds: None,
            mover: None,
            transport_progress: progress.map(|progress_ms| TransportClock {
                progress_ms,
                arrived: None,
            }),
            transport: None,
            spline: None,
            fields: ObjectFields::default(),
        };
        let mut events = vec![create(Some(7)), create(None), SessionEvent::ObjectUpdateEnd];
        let at = crate::Instant::now();
        stamp_arrival(&mut events, at);
        let clocks: Vec<Option<TransportClock>> = events
            .iter()
            .map(|e| match e {
                SessionEvent::ObjectCreate {
                    transport_progress, ..
                } => *transport_progress,
                _ => None,
            })
            .collect();
        assert_eq!(
            clocks,
            [
                Some(TransportClock {
                    progress_ms: 7,
                    arrived: Some(at)
                }),
                None,
                None
            ]
        );
    }
}
