//! The pet bar's `WorldWriter` sends — mirroring [`crate::messages::pet`] (decisions 0982, 0988).
//!
//! Four verbs, and the thing to know about them is that **no reply packet answers any of them**.
//! The first three are intents the client has already applied to its own state before sending
//! (wow-re §10.1/§10.2: the reaction, command and autocast paths all write `[0xb71468]` or the
//! slot word and fire `PET_BAR_UPDATE` *before* the packet leaves). The bar's contents change only
//! when a fresh `SMSG_PET_SPELLS` arrives — a summon, a swap, a learned spell — never as a reply
//! to one of these.
//!
//! [`WorldWriter::pet_cancel_aura`] is the exception worth naming: it too gets no reply, but it is
//! the one verb the client does *not* pre-apply, because what it changes is the pet's own
//! descriptor. The aura leaves in a `UNIT_FIELD_AURA` delta, and the slot's icon follows from
//! that — so the round trip is visible, just not as an answer.
//!
//! Every one carries the pet's guid: the server re-checks ownership on each
//! (`PetHandler.cpp`'s `GetCharmerOrOwnerGuid` gate) and silently drops anything naming a unit we
//! don't control.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Press one pet bar slot (`CMSG_PET_ACTION`, layout in [`messages::pet_action`]) — the one
    /// verb behind all three slot classes, because the server dispatches on the **type byte inside
    /// the word we echo**: a command token runs `HandlePetCommand`, a reaction token sets the react
    /// state, a spell casts. `target` is the unit the action should aim at, or `0`.
    pub fn pet_action(&mut self, pet_guid: u64, packed: u32, target_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_PET_ACTION,
            &messages::pet_action(pet_guid, packed, target_guid),
        )
    }

    /// Call the pet off its target (`CMSG_PET_STOP_ATTACK`, layout in
    /// [`messages::pet_stop_attack`]) — the Attack button's second press.
    pub fn pet_stop_attack(&mut self, pet_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_PET_STOP_ATTACK,
            &messages::pet_stop_attack(pet_guid),
        )
    }

    /// Cancel one of the **pet's** auras (`CMSG_PET_CANCEL_AURA`, layout in
    /// [`messages::pet_cancel_aura`]) — a pet bar spell click that lands on a spell already running
    /// on the pet (wow-re §10.1, `0x4bd25f`; the predicate is `0x4bcea0`).
    ///
    /// The click that sends this sends **nothing else**: the reference returns straight after,
    /// so the ordinary `CMSG_PET_ACTION` never leaves. That "and return" is the whole behaviour —
    /// clicking a lit pet buff takes it off rather than re-casting it.
    pub fn pet_cancel_aura(&mut self, pet_guid: u64, spell_id: u32) -> Result<()> {
        self.send(
            opcode::CMSG_PET_CANCEL_AURA,
            &messages::pet_cancel_aura(pet_guid, spell_id),
        )
    }

    /// Write one or two pet bar slots (`CMSG_PET_SET_ACTION`, layout — and the reason this one
    /// opcode carries two different verbs — in [`messages::pet_set_action`]).
    ///
    /// **The autocast toggle is this send**, with one entry: the pressed slot and its word with
    /// bit 30 flipped. The drag is the same send with one or two. Unlike the player bar's
    /// [`WorldWriter::set_action_button`], a swap here IS atomic — the server tells the forms
    /// apart by body size and applies both entries together.
    pub fn pet_set_action(&mut self, pet_guid: u64, entries: &[(u32, u32)]) -> Result<()> {
        self.send(
            opcode::CMSG_PET_SET_ACTION,
            &messages::pet_set_action(pet_guid, entries),
        )
    }

    /// Flip one pet **spellbook** entry's autocast (`CMSG_PET_SPELL_AUTOCAST`, layout in
    /// [`messages::pet_spell_autocast`]) — `ToggleSpellAutocast`'s send (decision 1032).
    ///
    /// The fifth verb, and the one easy to mistake for the fourth: the pet BAR's autocast right
    /// click is [`WorldWriter::pet_set_action`] with one entry (a slot and its word), while this
    /// names a **spell id**, because the book it comes from has no slots. Same bit, same
    /// server-side effect; different opcode, different body, different handler. Like the rest of
    /// this file: no reply.
    pub fn pet_spell_autocast(
        &mut self,
        pet_guid: u64,
        spell_id: u32,
        enabled: bool,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_PET_SPELL_AUTOCAST,
            &messages::pet_spell_autocast(pet_guid, spell_id, enabled),
        )
    }
}
