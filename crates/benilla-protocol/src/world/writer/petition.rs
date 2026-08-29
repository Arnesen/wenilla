//! The petition family's `WorldWriter` sends — buying a guild charter, showing it, signing it,
//! offering it, renaming it and turning it in (decision 1672).
//!
//! [`super::guild`] is *being* in a guild; this is *founding* one, and at 1.12 the two share only
//! their error channel. Three shapes bind every caller here:
//!
//! - **A charter is addressed by its ITEM guid**, not by a petition id — sign, offer, rename,
//!   decline and turn-in all take the item. The petition *id* appears in exactly one place,
//!   [`WorldWriter::petition_query`], and it is not derivable from the item without either a
//!   preceding `SMSG_PETITION_SHOW_SIGNATURES` or a read of the item's enchantment slot 0
//!   (see [`crate::messages::petition`]'s module doc).
//! - **Success is mostly silent.** Buying answers with nothing but the new item; offering answers
//!   the *target*, not us; renaming echoes only if it took. What comes back on a refusal is an
//!   `SMSG_GUILD_COMMAND_RESULT`, borrowed from the guild family — so a caller that only listens
//!   for petition opcodes will see a failed verb as a hang.
//! - **Every one of these can be refused silently** by a range or ownership check the client
//!   cannot see (`GetNPCIfCanInteractWith`, "do we hold that item", "are we the owner"). None of
//!   them is safe to treat as applied at the send.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Ask a petitioner NPC what charters it sells (`CMSG_PETITION_SHOWLIST`), answered by
    /// `SMSG_PETITION_SHOWLIST`.
    ///
    /// This is the **second** way that answer arrives: selecting the NPC's
    /// `GOSSIP_OPTION_PETITIONER` row makes the server close the gossip menu and push the same
    /// packet unasked (`Player.cpp:12428-12431`). So a client that only ever talks through gossip
    /// never needs this send — it is here for the direct re-open, and because the opcode exists.
    pub fn petition_show_list(&mut self, npc: u64) -> Result<()> {
        self.send(
            opcode::CMSG_PETITION_SHOWLIST,
            &messages::petition_show_list(npc),
        )
    }

    /// Buy a guild charter from a petitioner NPC (`CMSG_PETITION_BUY`) under the given name.
    ///
    /// **Nothing acknowledges a successful buy.** The server takes the money, stores item
    /// [`messages::CHARTER_ITEM_ENTRY`] and sends only the ordinary `SMSG_ITEM_PUSH_RESULT`
    /// (`Handlers/PetitionsHandler.cpp:130`). A refusal, by contrast, is loud and arrives on three
    /// *different* opcodes depending on which check failed: `SMSG_GUILD_COMMAND_RESULT` for a
    /// taken or invalid name, `SMSG_BUY_FAILED` for too little money, `SMSG_INVENTORY_CHANGE_FAILURE`
    /// for a full bag.
    ///
    /// The name is capped at [`messages::CHARTER_NAME_MAX_LENGTH`] characters by the server's own
    /// validator; the caller enforces that at the edit box, as the reference does, rather than
    /// truncating here.
    pub fn petition_buy(&mut self, npc: u64, name: &str) -> Result<()> {
        self.send(
            opcode::CMSG_PETITION_BUY,
            &messages::petition_buy(npc, name),
        )
    }

    /// Open a charter and see who has signed it (`CMSG_PETITION_SHOW_SIGNATURES`), answered by
    /// `SMSG_PETITION_SHOW_SIGNATURES`.
    ///
    /// Dropped **silently** if we are already in a guild or do not hold that item
    /// (`Handlers/PetitionsHandler.cpp:140`, `:143`), so a caller must not wait on the answer as
    /// though it were guaranteed.
    pub fn petition_show_signatures(&mut self, item: u64) -> Result<()> {
        self.send(
            opcode::CMSG_PETITION_SHOW_SIGNATURES,
            &messages::petition_show_signatures(item),
        )
    }

    /// Sign somebody's charter (`CMSG_PETITION_SIGN`), answered by `SMSG_PETITION_SIGN_RESULTS` —
    /// which the server sends to the **owner too**, identical, with the signer's guid in both
    /// copies.
    ///
    /// Not every refusal comes back on that opcode: an already-guilded or cross-faction signer is
    /// answered with an `SMSG_GUILD_COMMAND_RESULT` instead
    /// (`Handlers/PetitionsHandler.cpp:252`, `:258`).
    /// `arg` is the byte the optional Lua argument rides on; the client's own default is `1`.
    pub fn petition_sign(&mut self, item: u64, arg: i8) -> Result<()> {
        self.send(
            opcode::CMSG_PETITION_SIGN,
            &messages::petition_sign(item, arg),
        )
    }

    /// Show our charter to another player so they can sign it (`CMSG_OFFER_PETITION`).
    ///
    /// The success path answers **them**, not us: the server sends *the target* an
    /// `SMSG_PETITION_SHOW_SIGNATURES` (`Handlers/PetitionsHandler.cpp:390-397`). We hear only
    /// about refusals, as `SMSG_GUILD_COMMAND_RESULT`. So there is nothing local to update on the
    /// send, and no answer to wait for.
    pub fn offer_petition(&mut self, item: u64, player: u64) -> Result<()> {
        self.send(
            opcode::CMSG_OFFER_PETITION,
            &messages::offer_petition(item, player),
        )
    }

    /// Turn a completed charter in to a guild registrar (`CMSG_TURN_IN_PETITION`), answered by
    /// `SMSG_TURN_IN_PETITION_RESULTS`.
    ///
    /// Only the charter's **owner** may do this; anyone else is refused silently
    /// (`Handlers/PetitionsHandler.cpp:432`). And a guild-name collision answers with an
    /// `SMSG_GUILD_COMMAND_RESULT` and **no results packet at all** (`:445`) — so the absence of a
    /// result is itself an outcome, not a dropped packet.
    ///
    /// On success the guild is created with every signer as a member, and the charter item is
    /// destroyed — which arrives as an ordinary inventory update, plus the guild family's own
    /// roster push.
    pub fn turn_in_petition(&mut self, item: u64) -> Result<()> {
        self.send(
            opcode::CMSG_TURN_IN_PETITION,
            &messages::turn_in_petition(item),
        )
    }

    /// Ask for a petition's record by id (`CMSG_PETITION_QUERY`), answered by
    /// `SMSG_PETITION_QUERY_RESPONSE` — the **only** packet that carries the proposed guild's name
    /// and the signature requirement.
    ///
    /// The `item` field is read and then ignored by vmangos, which looks the petition up by id
    /// alone (`Handlers/PetitionsHandler.cpp:171-185`); it is sent at its true value anyway.
    pub fn petition_query(&mut self, petition_id: u32, item: u64) -> Result<()> {
        self.send(
            opcode::CMSG_PETITION_QUERY,
            &messages::petition_query(petition_id, item),
        )
    }

    /// Rename a charter (`MSG_PETITION_RENAME`), echoed back **only if it took**; a rejected name
    /// comes back as an `SMSG_GUILD_COMMAND_RESULT` and no echo.
    ///
    /// vmangos does **no ownership check** here — anyone holding the item may rename it
    /// (`Handlers/PetitionsHandler.cpp:187-216`).
    pub fn petition_rename(&mut self, item: u64, name: &str) -> Result<()> {
        self.send(
            opcode::MSG_PETITION_RENAME,
            &messages::petition_rename(item, name),
        )
    }

    /// Decline a charter someone offered us (`MSG_PETITION_DECLINE`). We send the charter's item
    /// guid; the server forwards **our** guid to its owner, which is why one opcode has two
    /// bodies.
    pub fn petition_decline(&mut self, item: u64) -> Result<()> {
        self.send(
            opcode::MSG_PETITION_DECLINE,
            &messages::petition_decline(item),
        )
    }
}
