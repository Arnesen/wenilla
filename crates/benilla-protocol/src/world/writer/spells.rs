//! The cast family's `WorldWriter` sends — the three `CMSG_CAST_SPELL` shapes (a unit or self, a
//! GameObject, an item), plus the three cancels a cast can need: the in-flight cast, the channel,
//! and the aura it left behind. Bodies in [`crate::messages::spells`], whose scope this mirrors;
//! the two spell-id cancels are a bare `u32` written inline. Split out of `writer/mod.rs`
//! (decision 0636), narrowed to the cast itself by decision 0640.
//!
//! 0636 kept auto-attack, the action bar and the talent spend here on the grounds that all of them
//! are "the ability system's wire". That was the weakest grouping in it, and once 0640 split
//! `messages/spells.rs` the mirror both allowed and recommended the finer cut: they are now
//! [`super::attack`], [`super::action_bar`] and [`super::progression`].
//!
//! The aura cancel stays because the wire says it belongs to the spell, not to the aura:
//! `CMSG_CANCEL_AURA` is addressed **by spell id, not by aura slot**.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Cast a spell (`CMSG_CAST_SPELL`): `target: None` = a self/implicit cast, `Some(guid)` = an
    /// explicit unit target (body in [`messages::cast_spell`]). The server answers
    /// `SMSG_CAST_RESULT` (and `SMSG_SPELL_START`/`GO` on success, unmodelled yet).
    pub fn cast_spell(&mut self, spell_id: u32, target: Option<u64>) -> Result<()> {
        self.send(
            opcode::CMSG_CAST_SPELL,
            &messages::cast_spell(spell_id, target),
        )
    }

    /// Cancel one of our own auras (`CMSG_CANCEL_AURA`, body in [`messages::cancel_aura`]) — the
    /// right-click-a-buff wire. Carries the **spell id**, not a slot; the server refuses passives,
    /// no-cancel spells and debuffs (decision 0257). No answer packet — the removal arrives as a
    /// `UNIT_FIELD_AURA` delta zeroing the slot.
    pub fn cancel_aura(&mut self, spell_id: u32) -> Result<()> {
        self.send(opcode::CMSG_CANCEL_AURA, &messages::cancel_aura(spell_id))
    }

    /// Cast an OPEN_LOCK spell at a **GameObject** (`CMSG_CAST_SPELL`, body in
    /// [`messages::cast_spell_gameobject`]) — the right-click on a locked chest / mining vein / herb
    /// node (decision 0239). The server runs `EffectOpenLock` → for a chest, opens the loot
    /// (`SMSG_LOOT_RESPONSE`); the profession/skill gate is the server's. Answered by
    /// `SMSG_CAST_RESULT` on refusal.
    pub fn cast_spell_gameobject(&mut self, spell_id: u32, go_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_CAST_SPELL,
            &messages::cast_spell_gameobject(spell_id, go_guid),
        )
    }

    /// Cast an item-targeted spell (`CMSG_CAST_SPELL` with `TARGET_FLAG_ITEM` + the item's packed
    /// guid — [`messages::cast_spell_item`]): the enchant cast the CraftFrame's item pick
    /// completes (decision 0437 phase 3). The server resolves the item, checks reagents, applies
    /// the enchant; refusal answers `SMSG_CAST_RESULT`.
    pub fn cast_spell_item(&mut self, spell_id: u32, item_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_CAST_SPELL,
            &messages::cast_spell_item(spell_id, item_guid),
        )
    }

    /// Cancel a named in-flight cast (`CMSG_CANCEL_CAST`: one `u32` spell id — vmangos
    /// `HandleCancelCastOpcode`). Sent by the wand-only auto-repeat handoff (`0x6095b8`) and by
    /// the cast bar's local self-cancel (movement/Esc mid-cast, `benilla::ui_cast`).
    pub fn cancel_cast(&mut self, spell_id: u32) -> Result<()> {
        self.send(opcode::CMSG_CANCEL_CAST, &spell_id.to_le_bytes())
    }

    /// End our own running channel (`CMSG_CANCEL_CHANNELLING`: one `u32` spell id, which vmangos
    /// reads and ignores — the interrupt is unconditional; the real client still writes it). The
    /// channel half of the local self-cancel (`benilla::ui_cast`).
    pub fn cancel_channelling(&mut self, spell_id: u32) -> Result<()> {
        self.send(opcode::CMSG_CANCEL_CHANNELLING, &spell_id.to_le_bytes())
    }
}
