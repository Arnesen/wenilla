//! The gossip family's `WorldWriter` sends — open a menu, choose an option, and fetch a menu's
//! greeting text. Bodies in [`crate::messages::gossip`], whose scope this mirrors. Split out of
//! `writer/mod.rs` (decision 0636).
//!
//! `CMSG_GOSSIP_HELLO` is the front door to every other NPC service window — vendor, trainer,
//! bank, taxi, quest — because the server passes `UNIT_NPC_FLAG_NONE` for this opcode (vmangos
//! `CanInteractWithNPC`, `Player.cpp:347`), so it works on any interactable creature, not only
//! gossip-flagged ones.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Open a gossip menu on an NPC (`CMSG_GOSSIP_HELLO`, layout in [`messages::gossip_hello`]) —
    /// works on any interactable creature, not only gossip-flagged ones (vmangos
    /// `CanInteractWithNPC`, `Player.cpp:347`, passes `UNIT_NPC_FLAG_NONE` for this opcode).
    /// Answered by `SMSG_GOSSIP_MESSAGE` (a `GossipMenu` event).
    pub fn gossip_hello(&mut self, npc_guid: u64) -> Result<()> {
        self.send(opcode::CMSG_GOSSIP_HELLO, &messages::gossip_hello(npc_guid))
    }

    /// Choose a gossip option (`CMSG_GOSSIP_SELECT_OPTION`, layout in
    /// [`messages::gossip_select_option`]): `gossip_list_id` is the option's echoed `index`; `code`
    /// carries a password only for a `coded` option, omitted entirely otherwise. The server answers
    /// either a fresh `SMSG_GOSSIP_MESSAGE` (a sub-menu) or `SMSG_GOSSIP_COMPLETE`.
    pub fn gossip_select_option(
        &mut self,
        npc_guid: u64,
        gossip_list_id: u32,
        code: Option<&str>,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_GOSSIP_SELECT_OPTION,
            &messages::gossip_select_option(npc_guid, gossip_list_id, code),
        )
    }

    /// Ask for a gossip menu's greeting text (`CMSG_NPC_TEXT_QUERY`, layout in
    /// [`messages::npc_text_query`]) — sent on receiving a gossip menu's `text_id`. Answered by
    /// `SMSG_NPC_TEXT_UPDATE` (an `NpcGreeting` event); ask-once cacheable like an item template.
    pub fn npc_text_query(&mut self, text_id: u32, guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_NPC_TEXT_QUERY,
            &messages::npc_text_query(text_id, guid),
        )
    }
}
