//! The reputation pane's three send verbs, and the one server push that is not a standing.
//!
//! Every faction the client can *change* is addressed by its **reputation-list id** — `Faction.dbc`'s
//! `reputationIndex`, the same slot the `SMSG_INITIALIZE_FACTIONS` array is positional in — never by
//! the `Faction.dbc` id and never by the panel's display row. The three bodies are byte-exact against
//! the vmangos reader side (`Server/Packets/Misc.cpp` + `Misc.h`, the `SetFactionAtWar` /
//! `SetFactionInactive` / `SetWatchedFaction` `ClientPacket`s), which is what a real server will
//! actually parse:
//!
//! | verb | opcode | body |
//! |---|---|---|
//! | at-war toggle | `CMSG_SET_FACTION_ATWAR` 0x125 | `u32 repListId`, `u8 flag` |
//! | inactive toggle | `CMSG_SET_FACTION_INACTIVE` 0x317 | `u32 repListId`, `u8 inactive` |
//! | watched faction | `CMSG_SET_WATCHED_FACTION` 0x318 | `i32 repListId` |
//!
//! **The watched verb is signed, and that is load-bearing.** Slot `0` is a real faction (the Bloodsail
//! Buccaneers hold `reputationIndex` 0 in 1.12's `Faction.dbc`), so "watch nothing" cannot be 0 on the
//! wire — vmangos writes the value straight into `PLAYER_FIELD_WATCHED_FACTION_INDEX` with
//! `SetInt32Value` (`Handlers/CharacterHandler.cpp` `HandleSetWatchedFactionOpcode`), and the descriptor
//! field's "none" is `-1`. FrameXML's own `SetWatchedFactionIndex(0)` passes a *display row* of 0
//! meaning "no row", so the binding — not this layer — is where 0 becomes [`WATCHED_FACTION_NONE`].
//!
//! None of the three is acked. The at-war and inactive flags come back as the flag byte of a fresh
//! `SMSG_INITIALIZE_FACTIONS`-shaped state only at the next login; within a session the client owns
//! its own optimistic flag copy, exactly as it owns the collapse state. The watched index comes back
//! as a `PLAYER_FIELD_WATCHED_FACTION_INDEX` descriptor update.

/// The `PLAYER_FIELD_WATCHED_FACTION_INDEX` / `CMSG_SET_WATCHED_FACTION` sentinel for "watch no
/// faction" — see the module header on why it cannot be `0`.
pub const WATCHED_FACTION_NONE: i32 = -1;

/// Body of `CMSG_SET_FACTION_ATWAR`: the reputation-list slot, then the desired at-war state.
///
/// vmangos drops the request outright while the player is in combat
/// (`HandleSetFactionAtWarOpcode`), so a declare/withdraw mid-fight is silently nothing.
pub fn set_faction_at_war(rep_list_id: u32, at_war: bool) -> Vec<u8> {
    let mut out = rep_list_id.to_le_bytes().to_vec();
    out.push(u8::from(at_war));
    out
}

/// Body of `CMSG_SET_FACTION_INACTIVE`: the reputation-list slot, then the desired inactive state.
pub fn set_faction_inactive(rep_list_id: u32, inactive: bool) -> Vec<u8> {
    let mut out = rep_list_id.to_le_bytes().to_vec();
    out.push(u8::from(inactive));
    out
}

/// Body of `CMSG_SET_WATCHED_FACTION`: one **signed** reputation-list slot, or
/// [`WATCHED_FACTION_NONE`] to stop watching.
pub fn set_watched_faction(rep_list_id: i32) -> Vec<u8> {
    rep_list_id.to_le_bytes().to_vec()
}
