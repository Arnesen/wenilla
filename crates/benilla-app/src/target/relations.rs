//! **Who may I attack, who may I help** — the two unit-relationship predicates the reference
//! shares across systems, kept together because they are one concern and neither belongs to the
//! system that happens to call them first.
//!
//! Both are byte-derived and both key off [`ring_reaction`], on opposite sides of the same bar:
//! `CanAttack 0x606980` wants reaction ≤ 3 (not friendly), `CanAssist 0x6066f0` wants ≥ 4
//! (friendly) plus two flag clauses. They lived in `scan.rs` while `can_attack` had exactly one
//! caller; `can_assist` (the `UnitBuff` gate, decision 1035) made that a second concern in a file
//! about TAB targeting.

use benilla_protocol::messages::OwnerFallback;

use crate::net::{ObjectStore, Reputations};

use super::{ring_reaction, Factions};

/// The five `UNIT_FIELD_FLAGS` disqualifiers `CanAttack 0x606980` tests (bit positions
/// byte-verified; names vmangos-corroborated): NON_ATTACKABLE(1), NOT_ATTACKABLE_1(7),
/// NON_ATTACKABLE_2(16), TAXI_FLIGHT(20), NOT_SELECTABLE(25).
pub(super) const FLAG_DISQUALIFIERS: u32 = (1 << 1) | (1 << 7) | (1 << 16) | (1 << 20) | (1 << 25);

/// `CanAttack 0x606980` — the shared attackability predicate (the combat flash's gate and the
/// scan's filter 3): the flag disqualifiers clear AND an attackable reaction. A unit without a
/// store passes the flag leg (nothing known to disqualify). The duel/PVP legs are deferred with
/// duels/PvP.
///
/// The reaction leg is **≤ neutral (rank ≤ 3)** — attackable means *not friendly*. First
/// director-observed (0170), then **byte-confirmed** by the `0x606980` re-pin: the function has
/// three reaction legs selected by `UNIT_FLAG_PVP_ATTACKABLE` (bit 3) on both parties, and the
/// player-vs-creature case is Leg B — `UnitReaction(player→target) < 4` (`cmp eax,4; setl`),
/// single direction, friendly-only blocked. The old "≤ 1 hostile" gloss was Leg A (both parties
/// UN-flagged: NPC-vs-NPC, bidirectional), never the player's check. Leg C (both flagged: the
/// PvP/duel/group machinery) is deferred with PvP.
pub(crate) fn can_attack(
    store: Option<&ObjectStore>,
    factions: Option<&Factions>,
    reputations: &Reputations,
    self_store: Option<&ObjectStore>,
) -> bool {
    if store.is_some_and(|s| s.0.unit_flags() & FLAG_DISQUALIFIERS != 0) {
        return false;
    }
    ring_reaction(factions, reputations, store, self_store) <= 3
}

/// `UNIT_FLAG_NOT_SELECTABLE` (bit 25) — `CanAssist`'s own first disqualifier, and one of
/// [`FLAG_DISQUALIFIERS`]' five (vmangos `UnitDefines.h`).
const UNIT_FLAG_NOT_SELECTABLE: u32 = 1 << 25;
/// `UNIT_FLAG_PVP` (bit 12) — what `IsPvP 0x605ff0` tests, after resolving the unit's owner
/// (vmangos `UnitDefines.h:UNIT_FLAG_PVP = 0x1000`).
const UNIT_FLAG_PVP: u32 = 0x1000;
/// `UNIT_FLAG_PVP_ATTACKABLE` (bit 3) — behaviourally "player-controlled", the same bit
/// [`ring_reaction`]'s duel leg selects on.
const UNIT_FLAG_PLAYER_CONTROLLED: u32 = 0x8;

/// `CanAssist 0x6066f0` — "may I help this unit?", the predicate `UnitBuff`'s unit-level gate runs
/// (see [`crate::ui_aura::buffs_visible_on`]). Byte-derived in wow-re's
/// `ui/scratch/aura-display-pipeline.md` §9b; named by its own Lua registrar pair
/// (`.data 0x8504c8 = {"UnitCanAssist", 0x516bb0}`), not by resemblance.
///
/// Three clauses: `UNIT_FLAG_NOT_SELECTABLE` clear, `UnitReaction ≥ 4`, and — for a unit that is
/// **not** player-controlled — `IsPvP 0x605ff0` on the unit (owner-chased). The reaction bar is the
/// **internal** scale, not the Lua one: `UnitReaction 0x5167e0` does `inc eax` at `0x51683e` before
/// pushing, so Lua's FRIENDLY(5) is internal 4. [`ring_reaction`] already returns the raw internal
/// rank, so the comparison is against 4 directly — **neutral fails**.
///
/// The player-controlled arm (`0x60673e`–`0x60679f`) keys on an `[obj+0xe68]` record whose fields
/// wow-re did **not** name, so it is deliberately NOT modelled: a player-controlled unit takes the
/// permissive answer here. That is conservative in the only direction that matters (it shows what
/// we already showed) and it keeps the un-derived arm out of the one place it could silently blank
/// a player's or a pet's buffs.
pub(crate) fn can_assist(
    store: Option<&ObjectStore>,
    factions: Option<&Factions>,
    reputations: &Reputations,
    self_store: Option<&ObjectStore>,
    owner_store: impl FnOnce(u64) -> Option<ObjectStore>,
) -> bool {
    let Some(store) = store else {
        return false;
    };
    let flags = store.0.unit_flags();
    if flags & UNIT_FLAG_NOT_SELECTABLE != 0 {
        return false;
    }
    if ring_reaction(factions, reputations, Some(store), self_store) < 4 {
        return false;
    }
    if flags & UNIT_FLAG_PLAYER_CONTROLLED != 0 {
        // The un-derived arm — see the doc comment.
        return true;
    }
    // `IsPvP 0x605ff0`: the flag on the unit's OWNER when it has one (charmedBy, else createdBy —
    // the `0x5ee5a0` fallback pair), otherwise on the unit itself.
    let owned = store
        .0
        .unit_owner(OwnerFallback::CreatedBy)
        .and_then(owner_store);
    let pvp_flags = owned.as_ref().map_or(flags, |o| o.0.unit_flags());
    pvp_flags & UNIT_FLAG_PVP != 0
}
