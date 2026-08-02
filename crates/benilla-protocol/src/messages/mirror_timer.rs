//! The mirror timers — the breath / fatigue / feign-death bars (opcodes 473-475; decision 0874).
//!
//! "Mirror" is the client's own word for a timer whose authority is the **server**: the client
//! never computes breath or fatigue, it mirrors a countdown the server ships and integrates it
//! locally between packets. Three opcodes carry the whole system:
//!
//! | opcode | what it means |
//! |---|---|
//! | `SMSG_START_MIRROR_TIMER` 0x1D9 | start **or fully re-state** one timer |
//! | `SMSG_PAUSE_MIRROR_TIMER` 0x1DA | freeze/unfreeze one running timer |
//! | `SMSG_STOP_MIRROR_TIMER` 0x1DB | that timer is over — hide its bar |
//!
//! The bodies are VERIFIED against vmangos's own packet builds
//! (`Server/Packets/Misc.h:718-751` + `Misc.cpp:472-491`), which are the exact bytes benilla
//! receives, and the type numbering against `Objects/MirrorTimer.h`
//! (`FATIGUE=0, BREATH=1, FEIGNDEATH=2, NUM_CLIENT_TIMERS=3`) — the server's `SendMirrorTimers`
//! gate means a **fourth** type (its internal `ENVIRONMENTAL`, lava/slime damage) is never sent
//! to a client at all; it drives damage server-side with no bar.
//!
//! Two things about this family are worth knowing before reading the app side:
//!
//! - **There is no separate "update" opcode.** A running timer that changes anything —
//!   direction, remaining time, frozen state — is re-sent as a whole `START`
//!   (`Player::SendMirrorTimers`, the `FULL_UPDATE` arm). So the consumer must treat `START` as
//!   idempotent *re-statement*, not only as a first appearance.
//! - **`PAUSE` is effectively dead on a vanilla wire.** vmangos deliberately never sends it —
//!   `SendMirrorTimers` replaces the pause with a full `START` and says why in the source
//!   ("Gotta do a full update with SMSG_START_MIRROR_TIMER to avoid lua errors"), because the
//!   shipped 1.12 `MirrorTimer.lua` handler tests the same `arg1` both as a name and as a number.
//!   We still decode it — a server that sends one should not desync us (see
//!   `crate::ui_mirror` for what the app does with it).

use std::io::{self, Read};

use crate::wire::{read_i32_le, read_u32_le, read_u8};

/// Which mirror timer a packet is about — the wire's `timerType` word.
///
/// The numbering is the server's `MirrorTimer::Type` (vmangos `Objects/MirrorTimer.h`), and it is
/// also the index the reference client uses into its own 3-entry name table, so a value outside
/// this set has no meaning on either end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorTimerKind {
    /// `0` — swimming in "high sea" (deep, uncharted water): the **Fatigue** bar.
    Fatigue,
    /// `1` — head under the surface: the **Breath** bar.
    Breath,
    /// `2` — feigning death. The server starts this one from a duration that is zero unless a
    /// script set it, so vanilla play essentially never shows it; it is a client timer type all
    /// the same.
    FeignDeath,
}

impl MirrorTimerKind {
    /// Map the wire's `timerType` word, or `None` for a type this client has no bar for
    /// (the server's own `NUM_CLIENT_TIMERS` gate means vanilla never sends one).
    pub fn from_wire(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Fatigue),
            1 => Some(Self::Breath),
            2 => Some(Self::FeignDeath),
            _ => None,
        }
    }

    /// The `timerType` word this kind is sent as — the inverse of [`Self::from_wire`].
    pub fn to_wire(self) -> u32 {
        match self {
            Self::Fatigue => 0,
            Self::Breath => 1,
            Self::FeignDeath => 2,
        }
    }
}

/// `SMSG_START_MIRROR_TIMER` — start, or wholly re-state, one mirror timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MirrorTimerStart {
    /// The wire's raw `timerType`. Kept raw rather than pre-mapped so an unknown type survives
    /// the decode as data instead of failing the whole packet; [`MirrorTimerKind::from_wire`]
    /// is the mapping, applied at the UI seam.
    pub kind: u32,
    /// Time left, in **milliseconds**. Counts toward `0` while draining and toward `duration_ms`
    /// while refilling; the bar is `remaining_ms / 1000` in bar units.
    pub remaining_ms: u32,
    /// The timer's full span, in **milliseconds** — the bar's maximum (60 s of breath by
    /// default: vmangos `CONFIG_UINT32_MIRRORTIMER_BREATH_MAX`).
    pub duration_ms: u32,
    /// Rate of change, in bar-units per second, and **signed**: `-1` while the timer drains
    /// (underwater, in high sea), `+10` while it refills (surfaced — a full breath bar refills
    /// ten times faster than it drained, which is why surfacing snaps it back). The consumer
    /// integrates `scale * elapsed` between packets; this is the whole of the client-side motion.
    pub scale: i32,
    /// Frozen: the bar holds its value and stops integrating. Sent as one byte.
    pub paused: bool,
    /// The spell driving this timer, if any (a water-breathing aura owns the breath timer for its
    /// duration). `0` = no spell. The reference client's Lua never sees this field.
    pub spell_id: u32,
}

/// Read `SMSG_START_MIRROR_TIMER`: `u32 type, u32 remaining, u32 duration, i32 scale, u8 paused,
/// u32 spellId` — VERIFIED against vmangos `StartMirrorTimer::AppendBodyTo` (`Misc.cpp:472`),
/// field for field in that order. `scale` is the one signed field.
pub fn read_start_mirror_timer(r: &mut impl Read) -> io::Result<MirrorTimerStart> {
    Ok(MirrorTimerStart {
        kind: read_u32_le(r)?,
        remaining_ms: read_u32_le(r)?,
        duration_ms: read_u32_le(r)?,
        scale: read_i32_le(r)?,
        paused: read_u8(r)? != 0,
        spell_id: read_u32_le(r)?,
    })
}

/// Read `SMSG_PAUSE_MIRROR_TIMER`: `u32 type, u8 paused` (vmangos `PauseMirrorTimer::AppendBodyTo`,
/// `Misc.cpp:487`). Returns the raw type and the flag.
pub fn read_pause_mirror_timer(r: &mut impl Read) -> io::Result<(u32, bool)> {
    Ok((read_u32_le(r)?, read_u8(r)? != 0))
}

/// Read `SMSG_STOP_MIRROR_TIMER`: the bare `u32 type` and nothing else
/// (vmangos `StopMirrorTimer::AppendBodyTo`, `Misc.cpp:482`).
pub fn read_stop_mirror_timer(r: &mut impl Read) -> io::Result<u32> {
    read_u32_le(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A golden `SMSG_START_MIRROR_TIMER` body, byte-for-byte as vmangos writes it: the breath
    /// timer draining with 45 s left of a 60 s span, unfrozen, no spell.
    #[test]
    fn start_reads_every_field_in_the_servers_order() {
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes()); // type: BREATH
        body.extend_from_slice(&45_000u32.to_le_bytes()); // remaining ms
        body.extend_from_slice(&60_000u32.to_le_bytes()); // duration ms
        body.extend_from_slice(&(-1i32).to_le_bytes()); // scale: draining
        body.push(0); // paused
        body.extend_from_slice(&0u32.to_le_bytes()); // spellId
        assert_eq!(body.len(), 21, "the server's body is 4+4+4+4+1+4 bytes");
        assert_eq!(
            read_start_mirror_timer(&mut &body[..]).unwrap(),
            MirrorTimerStart {
                kind: 1,
                remaining_ms: 45_000,
                duration_ms: 60_000,
                scale: -1,
                paused: false,
                spell_id: 0,
            }
        );
    }

    /// The refill direction: surfacing re-sends the same timer with `scale = +10`, frozen flag
    /// clear, and a water-breathing aura's id when one owns it. `scale` must survive as a
    /// *signed* field — a `u32` read would still pass on `+10` and silently break on `-1`.
    #[test]
    fn start_carries_a_signed_scale_and_the_owning_spell() {
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&30_000u32.to_le_bytes());
        body.extend_from_slice(&60_000u32.to_le_bytes());
        body.extend_from_slice(&10i32.to_le_bytes());
        body.push(1);
        body.extend_from_slice(&5697u32.to_le_bytes()); // Water Breathing
        let start = read_start_mirror_timer(&mut &body[..]).unwrap();
        assert_eq!(
            start.scale, 10,
            "refilling ten times faster than it drained"
        );
        assert!(start.paused);
        assert_eq!(start.spell_id, 5697);
    }

    /// The three client timer types, and the fact that nothing else maps — the server's
    /// `NUM_CLIENT_TIMERS` gate keeps its internal `ENVIRONMENTAL` (3) off the wire entirely.
    #[test]
    fn only_the_three_client_timer_types_map() {
        assert_eq!(
            MirrorTimerKind::from_wire(0),
            Some(MirrorTimerKind::Fatigue)
        );
        assert_eq!(MirrorTimerKind::from_wire(1), Some(MirrorTimerKind::Breath));
        assert_eq!(
            MirrorTimerKind::from_wire(2),
            Some(MirrorTimerKind::FeignDeath)
        );
        assert_eq!(
            MirrorTimerKind::from_wire(3),
            None,
            "ENVIRONMENTAL: server-only"
        );
        assert_eq!(MirrorTimerKind::from_wire(u32::MAX), None);
        for kind in [
            MirrorTimerKind::Fatigue,
            MirrorTimerKind::Breath,
            MirrorTimerKind::FeignDeath,
        ] {
            assert_eq!(MirrorTimerKind::from_wire(kind.to_wire()), Some(kind));
        }
    }

    /// `SMSG_PAUSE_MIRROR_TIMER`: type then one flag byte.
    #[test]
    fn pause_is_a_type_and_a_flag_byte() {
        let mut body = 0u32.to_le_bytes().to_vec();
        body.push(1);
        assert_eq!(read_pause_mirror_timer(&mut &body[..]).unwrap(), (0, true));
        let mut body = 1u32.to_le_bytes().to_vec();
        body.push(0);
        assert_eq!(read_pause_mirror_timer(&mut &body[..]).unwrap(), (1, false));
    }

    /// `SMSG_STOP_MIRROR_TIMER`: the bare type word.
    #[test]
    fn stop_is_the_bare_type_word() {
        assert_eq!(
            read_stop_mirror_timer(&mut &1u32.to_le_bytes()[..]).unwrap(),
            1
        );
    }

    /// A truncated body is an error, never a silently zeroed field — the `paused` byte and the
    /// trailing `spellId` are the two the server appends last and the two a sloppy reader drops.
    #[test]
    fn a_truncated_start_body_is_an_error() {
        let mut full = Vec::new();
        full.extend_from_slice(&1u32.to_le_bytes());
        full.extend_from_slice(&45_000u32.to_le_bytes());
        full.extend_from_slice(&60_000u32.to_le_bytes());
        full.extend_from_slice(&(-1i32).to_le_bytes());
        full.push(0);
        full.extend_from_slice(&0u32.to_le_bytes());
        for cut in [12, 16, 17, 20] {
            assert!(
                read_start_mirror_timer(&mut &full[..cut]).is_err(),
                "a {cut}-byte body must not decode"
            );
        }
        assert!(read_start_mirror_timer(&mut &full[..]).is_ok());
    }
}
