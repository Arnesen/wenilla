//! The player-summon family — the server's question and the client's one-word answer
//! (opcodes 683 and 684; decision 1747).
//!
//! Two packets, and the asymmetry between them is the whole story. A warlock's Ritual of
//! Summoning, a meeting stone, or a GM's `.summon` all end in the *server* asking the summoned
//! player `SMSG_SUMMON_REQUEST`; the client parks the three fields and puts the `CONFIRM_SUMMON`
//! dialog up. Accepting sends `CMSG_SUMMON_RESPONSE` carrying **the summoner's guid and nothing
//! else** — there is no accept/decline byte in 1.12, because **declining sends no packet at all**
//! and the server auto-declines on its own timer.
//!
//! | opcode | direction | body |
//! |---|---|---|
//! | `SMSG_SUMMON_REQUEST` 0x2ab | in | `u64` summoner guid + `u32` zone id + `u32` delay ms |
//! | `CMSG_SUMMON_RESPONSE` 0x2ac | out | `u64` summoner guid |
//!
//! **Byte-pinned client-side**, not inferred from the server: the 1.12.1 handler `0x5e6140` (the
//! `0x5ab650` registration site maps `0x2ab` to it) reads exactly three fields in this order —
//! `CDataStore::GetGuid 0x4190b0`, `CDataStore::GetInt32 0x418e30`, `CDataStore::GetUInt32_2
//! 0x418fb0` — and the accept binding `ConfirmSummon 0x48b770` writes opcode `0x2ac` followed by
//! one `CDataStore::PutGuid 0x418370` and sends (`ClientServices::Send 0x5ab630`). Nothing else
//! goes into either body.
//!
//! vmangos agrees on both, independently: `Server/Packets/Misc.cpp`'s
//! `SummonRequest::AppendBodyTo` writes `summonerGuid; zoneId; autoDeclineDelay`, and
//! `SummonResponse::ReadFromWorldPacket` reads **only** `summonerGuid` — then ignores it, calling
//! `Player::SummonIfPossible(true)` off its own `m_summon_expire` window
//! (`Handlers/MovementHandler.cpp`). So the guid is an echo the server does not check; what makes
//! a stale accept harmless is the server's timer, not the guid.

use std::io;

use crate::wire::{read_u32_le, read_u64_le};

/// `SMSG_SUMMON_REQUEST` — someone is asking to pull us to them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SummonRequest {
    /// Who is summoning. Echoed back in [`summon_response`], and it is the guid
    /// `GetSummonConfirmSummoner()` resolves through the name cache.
    pub summoner: u64,
    /// The **summoner's** zone — an `AreaTable.dbc` id, which is the whole of what
    /// `GetSummonConfirmAreaName()` looks up. Not our own zone, and not a map id.
    pub zone: u32,
    /// Milliseconds until the server auto-declines (vmangos `MAX_PLAYER_SUMMON_DELAY * 1000`, two
    /// minutes). The client turns it into a deadline the moment the packet lands and counts down
    /// against it; the dialog takes itself off screen when it runs out.
    pub delay_ms: u32,
}

/// Read `SMSG_SUMMON_REQUEST`: guid, zone, delay — see the module header for the byte pin.
pub(super) fn read_summon_request(r: &mut &[u8]) -> io::Result<SummonRequest> {
    Ok(SummonRequest {
        summoner: read_u64_le(r)?,
        zone: read_u32_le(r)?,
        delay_ms: read_u32_le(r)?,
    })
}

/// Body of `CMSG_SUMMON_RESPONSE`: the summoner's full 8-byte guid, and nothing after it.
///
/// **Decorative on vmangos, and still the right thing to send**: its handler reads the guid and
/// never looks at it again. The client's own `ConfirmSummon 0x48b770` puts the latched guid here,
/// so a server that *does* check it (or a future one that must distinguish two pending summons)
/// gets what the reference would have sent.
pub fn summon_response(summoner_guid: u64) -> Vec<u8> {
    summoner_guid.to_le_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The request body is the three fields in wire order, and the reader must not reorder the
    /// two `u32`s — a swapped pair would name the zone by a millisecond count and count down two
    /// minutes' worth of area ids, both of which read as "the dialog is just wrong".
    #[test]
    fn the_request_body_is_guid_then_zone_then_delay() {
        let mut bytes = 0xF130_0000_0001_2345u64.to_le_bytes().to_vec();
        bytes.extend_from_slice(&1519u32.to_le_bytes()); // Stormwind City
        bytes.extend_from_slice(&120_000u32.to_le_bytes()); // two minutes
        assert_eq!(
            read_summon_request(&mut &bytes[..]).unwrap(),
            SummonRequest {
                summoner: 0xF130_0000_0001_2345,
                zone: 1519,
                delay_ms: 120_000,
            }
        );
    }

    /// A truncated body is an error, not a zero-filled request: a half-read packet that latched
    /// would put a dialog up naming nobody, for no time.
    #[test]
    fn a_short_request_body_is_an_error() {
        let bytes = [0u8; 15];
        assert!(read_summon_request(&mut &bytes[..]).is_err());
    }

    /// The answer is one little-endian guid — the shape vmangos's `SummonResponse` reads back,
    /// and byte-for-byte what `ConfirmSummon 0x48b770` puts on the wire.
    #[test]
    fn the_response_body_is_one_little_endian_guid_and_nothing_else() {
        assert_eq!(
            summon_response(0x0000_0000_0000_2a01),
            vec![0x01, 0x2a, 0, 0, 0, 0, 0, 0]
        );
    }
}
