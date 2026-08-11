//! The session-lifecycle `WorldWriter` sends — the three the client makes about *being logged in*
//! rather than about anything in the world: the keepalive, the leave-the-world request, and the
//! cinematic ack that gates the world stream on entering it. Split out of `writer/mod.rs`
//! (decision 0636).
//!
//! All three are effectively bodyless (`ping` carries only its two counters), and all three are
//! **cadence/obligation** sends rather than player intents: skip the ping and the socket dies,
//! skip the cinematic ack and the world around the body despawns.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Send the ~30 s keepalive (`CMSG_PING`): `sequence` is the ++counter the server echoes back
    /// as `SMSG_PONG`, `last_rtt_ms` the previous round-trip measurement (the real client's
    /// lastRtt; the server stores it as our reported latency). Cadence discipline is the caller's:
    /// vmangos kicks a socket whose pings repeat faster than 27 s apart (`_HandlePing`'s
    /// overspeed count), so this is a timer send, never a retry.
    pub fn ping(&mut self, sequence: u32, last_rtt_ms: u32) -> Result<()> {
        self.send(opcode::CMSG_PING, &messages::ping(sequence, last_rtt_ms))
    }

    /// Ask to leave the world back to character select (`CMSG_LOGOUT_REQUEST`, empty body). The
    /// server answers `SMSG_LOGOUT_RESPONSE` (a refusal while in combat) and, once the logout
    /// completes (instant for a resting/GM character), `SMSG_LOGOUT_COMPLETE` — which the stream
    /// surfaces as [`SessionEvent::LoggedOut`](crate::SessionEvent::LoggedOut).
    pub fn logout_request(&mut self) -> Result<()> {
        self.send(opcode::CMSG_LOGOUT_REQUEST, &[])
    }

    /// Call off a pending logout (`CMSG_LOGOUT_CANCEL`, empty body) — the CAMP/QUIT dialog's Cancel
    /// (decision 0674). Only meaningful while the server's 20-second timer is running (a non-instant
    /// [`logout_request`](Self::logout_request)); the server drops the timer, unroots the character
    /// and answers `SMSG_LOGOUT_CANCEL_ACK`.
    pub fn logout_cancel(&mut self) -> Result<()> {
        self.send(opcode::CMSG_LOGOUT_CANCEL, &[])
    }

    /// Ask the server for its wall clock (`CMSG_QUERY_TIME`, empty body); answered with
    /// `SMSG_QUERY_TIME_RESPONSE`, one `u32` of unix-epoch seconds.
    ///
    /// A cadence/obligation send like [`ping`](Self::ping), not a player intent: the server writes
    /// *absolute* stamps in its own epoch into descriptor fields — a timed quest's deadline is
    /// `time(nullptr) + limitTime` — and nothing on the wire ever restates them as a duration. So
    /// every countdown the client draws is only as right as its last sample of this clock; benilla
    /// takes one on entering the world (decision 1150).
    pub fn query_time(&mut self) -> Result<()> {
        self.send(opcode::CMSG_QUERY_TIME, &messages::query_time())
    }

    /// Acknowledge a triggered cinematic as finished (`CMSG_COMPLETE_CINEMATIC`, empty body) — the
    /// packet the real client sends when the cinematic ends or the player ESCs out. Must answer
    /// every `SMSG_TRIGGER_CINEMATIC` ([`SessionEvent::CinematicTriggered`]
    /// (crate::SessionEvent::CinematicTriggered)): while one runs unacked, vmangos anchors object
    /// visibility to the flying cinematic camera and the world around the body despawns.
    pub fn complete_cinematic(&mut self) -> Result<()> {
        self.send(opcode::CMSG_COMPLETE_CINEMATIC, &[])
    }
}
