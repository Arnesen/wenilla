//! The death/corpse-run family's `WorldWriter` sends — release the spirit, find the corpse,
//! reclaim it, or take the spirit healer's res; plus the answer to someone else's res offer.
//! Bodies in [`crate::messages`]'s `reclaim_corpse`/`spirit_healer_activate`/`resurrect_response`
//! builders (the first two verbs are bodyless). Split out of `writer/mod.rs` (decision 0636),
//! mirroring [`crate::messages::death`].
//!
//! Every one of these is server-gated on a death state the client only *believes* it is in
//! (ghost/unreleased/delay-elapsed/in-range), so a refusal is normal and is not always a packet —
//! the confirmations arrive as ordinary descriptor deltas.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Release the spirit (`CMSG_REPOP_REQUEST`, empty body — decision 0308 slice 1): valid only
    /// while dead and unreleased (the server refuses it alive or already-ghost). The server
    /// answers with the ghost form (aura 8326 → the ghost flags), the corpse object, unroot,
    /// water-walk, `SMSG_CORPSE_RECLAIM_DELAY`, and the graveyard teleport.
    pub fn repop_request(&mut self) -> Result<()> {
        self.send(opcode::CMSG_REPOP_REQUEST, &[])
    }

    /// Ask where our corpse is (`MSG_CORPSE_QUERY`, empty request): answered by the same opcode
    /// (the [`SessionEvent::CorpseQuery`](crate::SessionEvent::CorpseQuery) feed for the map
    /// markers + the corpse-run range gate).
    pub fn corpse_query(&mut self) -> Result<()> {
        self.send(opcode::MSG_CORPSE_QUERY, &[])
    }

    /// Reclaim our corpse (`CMSG_RECLAIM_CORPSE` — the RECOVER_CORPSE popup's Accept): the corpse's
    /// guid. Server gates: ghost, the reclaim delay elapsed, within 39 yd. Success comes back as
    /// ordinary descriptor deltas (alive, ghost flags clear) + the corpse-to-bones swap.
    pub fn reclaim_corpse(&mut self, corpse_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_RECLAIM_CORPSE,
            &messages::reclaim_corpse(corpse_guid),
        )
    }

    /// Take the spirit healer's resurrection (`CMSG_SPIRIT_HEALER_ACTIVATE` — the XP_LOSS
    /// confirm's final Accept): res at 50%, 25% durability loss, resurrection sickness at
    /// level ≥ 11. `npc` is the spirit healer's guid (from `SMSG_SPIRIT_HEALER_CONFIRM`).
    pub fn spirit_healer_activate(&mut self, npc: u64) -> Result<()> {
        self.send(
            opcode::CMSG_SPIRIT_HEALER_ACTIVATE,
            &messages::spirit_healer_activate(npc),
        )
    }

    /// Answer a resurrection offer (`CMSG_RESURRECT_RESPONSE` — the RESURRECT popup's
    /// Accept/Decline): the offerer's guid + the accept byte.
    pub fn resurrect_response(&mut self, caster: u64, accept: bool) -> Result<()> {
        self.send(
            opcode::CMSG_RESURRECT_RESPONSE,
            &messages::resurrect_response(caster, accept),
        )
    }
}
