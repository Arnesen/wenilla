//! The stable window's `WorldWriter` sends — the list refresh and the four mutations. Bodies in
//! [`crate::messages::stable`], whose scope this mirrors.
//!
//! **Every verb names the stable master's guid**, because the server re-checks the interaction on
//! each one (`WorldSession::CheckStableMaster`, VERIFIED vmangos `NPCHandler.cpp:584-607`): the guid
//! must be an NPC with `UNIT_NPC_FLAG_STABLEMASTER` that the player can still interact with. A
//! window left open while the player walks away therefore fails every button with
//! [`messages::stable_result::ERR_STABLE`] rather than acting at a distance — which is why the app
//! side range-guards the session rather than trusting the open window.
//!
//! **None of the four mutations is answered with a fresh list** — only a one-byte
//! `SMSG_STABLE_RESULT` (VERIFIED: every `HandleStable*` path ends in `SendStableResult` and
//! nothing else). So a success is a cue to re-ask with [`WorldWriter::list_stabled_pets`], the same
//! shape as the trainer's post-purchase re-request.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Ask (or re-ask) a stable master's pet list (`MSG_LIST_STABLED_PETS`, layout in
    /// [`messages::list_stabled_pets`]) — one 8-byte NPC guid.
    ///
    /// This is the *refresh* verb. The window first *opens* off the server's own unprompted send of
    /// the same opcode, which the gossip stable option produces (`GOSSIP_OPTION_STABLEPET` →
    /// `SendStablePet`, VERIFIED vmangos `Player.cpp:12400-12402`) — exactly the trainer's and the
    /// vendor's arrangement. Answered by `MSG_LIST_STABLED_PETS` inbound.
    pub fn list_stabled_pets(&mut self, npc_guid: u64) -> Result<()> {
        self.send(
            opcode::MSG_LIST_STABLED_PETS,
            &messages::list_stabled_pets(npc_guid),
        )
    }

    /// Put the current pet into a stable slot (`CMSG_STABLE_PET`, layout in
    /// [`messages::stable_pet`]): the NPC guid alone.
    ///
    /// **There is no slot to name** — the server takes the first free one and refuses if that index
    /// exceeds the slots the player has bought (`HandleStablePet`, VERIFIED `NPCHandler.cpp:609-655`).
    /// Answers [`messages::stable_result::SUCCESS_STABLE`], or `ERR_STABLE` when the player is dead,
    /// has no live hunter pet, or has no free bought slot.
    pub fn stable_pet(&mut self, npc_guid: u64) -> Result<()> {
        self.send(opcode::CMSG_STABLE_PET, &messages::stable_pet(npc_guid))
    }

    /// Summon a stabled pet as the current pet (`CMSG_UNSTABLE_PET`, layout in
    /// [`messages::unstable_pet`]): the NPC guid + the pet's own
    /// [`messages::StabledPet::pet_number`], never its slot.
    ///
    /// Only valid with **no** current pet — vmangos refuses even when the existing pet is merely
    /// unsummoned and out of range (`HandleUnstablePet`, VERIFIED `NPCHandler.cpp:657-702`). With a
    /// pet already out, the verb is [`Self::stable_swap_pet`]. Answers
    /// [`messages::stable_result::SUCCESS_UNSTABLE`] or `ERR_STABLE`.
    pub fn unstable_pet(&mut self, npc_guid: u64, pet_number: u32) -> Result<()> {
        self.send(
            opcode::CMSG_UNSTABLE_PET,
            &messages::unstable_pet(npc_guid, pet_number),
        )
    }

    /// Trade the current pet for a stabled one in a single step (`CMSG_STABLE_SWAP_PET`, layout in
    /// [`messages::stable_swap_pet`]): the NPC guid + the stabled pet's
    /// [`messages::StabledPet::pet_number`].
    ///
    /// The current pet goes into the slot the named pet vacates (`HandleStableSwapPet`, VERIFIED
    /// `NPCHandler.cpp:735-789`). Requires a live hunter pet to be out. Answers
    /// [`messages::stable_result::SUCCESS_UNSTABLE`] — the *same* code a plain unstable returns, so
    /// the reply cannot tell the two verbs apart.
    pub fn stable_swap_pet(&mut self, npc_guid: u64, pet_number: u32) -> Result<()> {
        self.send(
            opcode::CMSG_STABLE_SWAP_PET,
            &messages::stable_swap_pet(npc_guid, pet_number),
        )
    }

    /// Buy the next stable slot (`CMSG_BUY_STABLE_SLOT`, layout in
    /// [`messages::buy_stable_slot`]): the NPC guid alone.
    ///
    /// The *which* is implicit, exactly as it is for the bank's bag slots (decision 0604): the
    /// server buys `m_stableSlots + 1` and prices it from `StableSlotPrices.dbc` at that row
    /// (`HandleBuyStableSlot`, VERIFIED `NPCHandler.cpp:704-729`). Answers
    /// [`messages::stable_result::SUCCESS_BUY_SLOT`], `ERR_MONEY`, or — past the two slots 5875
    /// ships — `ERR_STABLE`. No packet reports the new count: the next list's `num_stable_slots`
    /// does.
    pub fn buy_stable_slot(&mut self, npc_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_BUY_STABLE_SLOT,
            &messages::buy_stable_slot(npc_guid),
        )
    }
}
