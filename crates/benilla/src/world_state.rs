//! The world-state table — the server's key/value scoreboard, fed by `SMSG_INIT_WORLD_STATES` and
//! `SMSG_UPDATE_WORLD_STATE` ([`benilla_protocol::messages::world_state`] for both wires).
//!
//! Its one reader today is the NPC-text expander's `$<n>w` / `$<n>e` tokens
//! ([`crate::npc_text`]): the reference renders a hit as `%d` and a **miss as `"0"`**, so every
//! such token printed `"0"` until this landed — faithful while the table was empty, wrong the
//! moment a zone sent real states. A battleground/PvP scoreboard is the other consumer, later.
//!
//! Shape is the reference's, not a convenience: it keeps an open hash keyed by the **raw wire
//! dword** (`[0xb71ec8]`, bucket `mask & key`, value at node `+0x18`, miss → `0` at `0x4c5867`).
//! Raw matters because `$<n>e` reads the table at the *negated* key — `$2077e` looks up
//! `0xFFFFF7E3`, which only resolves if nothing re-interpreted the id on the way in.
//!
//! **One thing is deliberately not modelled, because it is not known.** The reference hands
//! `SMSG_INIT_WORLD_STATES`' two leading dwords (map, zone) to `0x4c5650` before the pair loop
//! runs, and wow-re has not carved that function — so whether an init *clears* the table first is
//! unrecorded. We do not clear: with no evidence either way, inventing a reset is the speculative
//! act, and vmangos re-sends its full default set on every zone change (`Player.cpp:8267-8270`),
//! which makes a stale key from a previous zone hard to reach in practice. If `0x4c5650` turns out
//! to reset, this is a one-line change in [`WorldStates::write`]'s caller.

use std::collections::HashMap;

use bevy::prelude::*;

/// The live world-state table. Keys and values are raw wire dwords; see the module doc.
#[derive(Resource, Default)]
pub(crate) struct WorldStates {
    values: HashMap<u32, u32>,
}

impl WorldStates {
    /// The reference's getter `0x4c5810`, including its miss leg: an absent key reads **0**, which
    /// is why an un-received world state renders `"0"` rather than blank or the literal token.
    ///
    /// Returned as `i32` because that is the width the one caller formats — the reference prints
    /// the dword through `"%d"`, so a value with the top bit set shows as negative.
    pub(crate) fn get(&self, key: u32) -> i32 {
        self.values.get(&key).copied().unwrap_or(0) as i32
    }

    /// Apply a run of `(id, value)` writes — the reference's setter `0x4c5870`, which both opcodes
    /// funnel into. Init's trailing `(0, 0)` terminator is written like any other pair, exactly as
    /// the reference's count loop does; it is data, not a sentinel.
    pub(crate) fn write(&mut self, states: &[(u32, u32)]) {
        self.values.extend(states.iter().copied());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The getter's miss leg and the `%d` width: an unknown key is 0 (not blank), a known one is
    /// its value, and the top bit reads through as a negative — the three things `$<n>w` depends on.
    #[test]
    fn a_miss_reads_zero_and_a_value_keeps_its_sign() {
        let mut table = WorldStates::default();
        assert_eq!(table.get(2077), 0, "empty table — the reference's `\"0\"`");
        table.write(&[(2077, 7), (2264, 0xFFFF_FFFF)]);
        assert_eq!(table.get(2077), 7);
        assert_eq!(table.get(2264), -1, "printed through `%d`");
        assert_eq!(table.get(9999), 0, "still a miss");
    }

    /// `$<n>e`'s negated key is a *different* entry, not the same one — the whole reason ids stay
    /// raw dwords. Writing id 2077 must not make `$2077e` (key `0xFFFFF7E3`) resolve.
    #[test]
    fn the_negated_key_is_a_separate_entry() {
        let mut table = WorldStates::default();
        table.write(&[(2077, 7)]);
        assert_eq!(table.get(2077u32.wrapping_neg()), 0);
        table.write(&[(2077u32.wrapping_neg(), 42)]);
        assert_eq!(table.get(2077u32.wrapping_neg()), 42);
        assert_eq!(table.get(2077), 7, "the positive key is untouched");
    }

    /// A later write to a live key replaces it (`SMSG_UPDATE_WORLD_STATE`'s whole job), and an
    /// init's terminator lands as the ordinary entry it is.
    #[test]
    fn a_write_replaces_and_the_terminator_is_just_data() {
        let mut table = WorldStates::default();
        table.write(&[(2264, 1), (0, 0)]);
        table.write(&[(2264, 5)]);
        assert_eq!(table.get(2264), 5);
        assert_eq!(table.get(0), 0);
    }
}
