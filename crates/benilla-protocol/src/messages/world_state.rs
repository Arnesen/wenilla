//! World states — the server's key/value scoreboard table (`SMSG_INIT_WORLD_STATES` `0x2C2`,
//! `SMSG_UPDATE_WORLD_STATE` `0x2C3`). It is what the NPC-text `$<n>w`/`$<n>e` tokens read, and it
//! is what a battleground scoreboard is built from; benilla uses the first today.
//!
//! Both wires are settled from two independent sources that agree:
//!
//! - **The reference binary** (wow-re `system/ui/scratch/questtext-macro-expander.md` §3): the one
//!   handler `0x48f690` holds the only two callers of the table setter `0x4c5870`, registered
//!   against exactly these two opcodes at `0x48f515-0x48f52f`. `0x2C2` is *two leading dwords*
//!   (passed to the unexamined `0x4c5650`), a `u16` count, then `count × (id:u32, value:u32)`;
//!   `0x2C3` is a single such pair.
//! - **vmangos's writers** (`Player::SendInitWorldStates`, `Player.cpp:8219-8293`, and
//!   `WorldPackets::Misc::UpdateWorldState::AppendBodyTo`, `Misc.cpp:685-694`): `u32 mapId`,
//!   `u32 zoneId` (the zone dword is `#if SUPPORTED_CLIENT_BUILD > CLIENT_BUILD_1_11_2`, so 1.12.1
//!   gets it), the `u16` count back-patched at the end, then the pairs; and `u32 field, u32 value`
//!   respectively (also build-gated — a pre-1.8.4 client would read `u16`s).
//!
//! **The count includes a `(0, 0)` terminator.** vmangos starts `count = 1` and appends
//! `uint32(0) << uint32(0)` last, *"to prevent repeating audio bug"* — so the pair run is read
//! verbatim, terminator and all, exactly as the reference's count loop does. There is no
//! terminator special case on either side and we add none: id 0 simply lands in the table as 0.
//!
//! Ids and values are **raw dwords** on the wire and stay raw here. That matters: the reference's
//! `$<n>e` token reads the table at the *negated* key, so a text asking for `$2077e` looks up
//! `0xFFFFF7E3` — a key only reachable if nothing has re-interpreted the field on the way in.

use std::io;

use crate::wire::{read_u16_le, read_u32_le};

/// `SMSG_INIT_WORLD_STATES` — the whole table for a zone, sent on login and on every zone change
/// (vmangos `Player::UpdateZone` → `SendInitWorldStates`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InitWorldStates {
    /// The map the states are scoped to. The reference hands this and `zone` to `0x4c5650` and
    /// nothing else; what that call does with them is **not recorded** (see [`read_init_world_states`]).
    pub map: u32,
    pub zone: u32,
    /// `(id, value)` verbatim, including the trailing `(0, 0)` terminator.
    pub states: Vec<(u32, u32)>,
}

/// Read `SMSG_INIT_WORLD_STATES`: `u32 map, u32 zone, u16 count`, then `count` `(id, value)` pairs.
///
/// The count is trusted only as far as the body allows — a truncated run stops at the short read
/// like every other reader here, rather than pre-allocating on a hostile length.
pub(in crate::messages) fn read_init_world_states(r: &mut &[u8]) -> io::Result<InitWorldStates> {
    let map = read_u32_le(r)?;
    let zone = read_u32_le(r)?;
    let count = read_u16_le(r)?;
    let mut states = Vec::new();
    for _ in 0..count {
        states.push((read_u32_le(r)?, read_u32_le(r)?));
    }
    Ok(InitWorldStates { map, zone, states })
}

/// Read `SMSG_UPDATE_WORLD_STATE`: one `(id, value)` dword pair.
pub(in crate::messages) fn read_update_world_state(r: &mut &[u8]) -> io::Result<(u32, u32)> {
    Ok((read_u32_le(r)?, read_u32_le(r)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A vmangos-shaped `SMSG_INIT_WORLD_STATES`: map, zone, a count that *includes* the trailing
    /// `(0, 0)` terminator, then the pairs. The terminator is data, not a sentinel we strip.
    #[test]
    fn init_reads_every_pair_including_the_terminator() {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_le_bytes()); // map 0 (Eastern Kingdoms)
        body.extend_from_slice(&1519u32.to_le_bytes()); // zone (Stormwind)
        body.extend_from_slice(&3u16.to_le_bytes()); // 2 real pairs + the terminator
        for (id, value) in [(2264u32, 7u32), (2263u32, 0u32), (0, 0)] {
            body.extend_from_slice(&id.to_le_bytes());
            body.extend_from_slice(&value.to_le_bytes());
        }
        let got = read_init_world_states(&mut body.as_slice()).unwrap();
        assert_eq!(got.map, 0);
        assert_eq!(got.zone, 1519);
        assert_eq!(got.states, vec![(2264, 7), (2263, 0), (0, 0)]);
    }

    /// A count longer than the body is a short read, not a panic or a giant allocation.
    #[test]
    fn init_truncated_run_errors() {
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&u16::MAX.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());
        assert!(read_init_world_states(&mut body.as_slice()).is_err());
    }

    /// The update wire is the bare pair, and both halves stay raw dwords — a value with the top bit
    /// set survives as-is, so the `%d` render downstream is the only place a sign appears.
    #[test]
    fn update_reads_a_raw_dword_pair() {
        let mut body = Vec::new();
        body.extend_from_slice(&2264u32.to_le_bytes());
        body.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        assert_eq!(
            read_update_world_state(&mut body.as_slice()).unwrap(),
            (2264, 0xFFFF_FFFF)
        );
    }
}
