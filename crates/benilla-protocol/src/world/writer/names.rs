//! The name-lookup `WorldWriter` sends — the three ask-once queries that turn a guid into
//! something displayable: a player's name/race/gender/class, a creature template's name/subname,
//! and a pet's given name. Bodies in [`crate::messages`]'s `full_guid`/`creature_query`/
//! `pet_name_query` builders. Split out of `writer/mod.rs` (decision 0636).
//!
//! Three verbs rather than one because the guid's own bits decide which can answer: a creature's
//! guid embeds its template entry ([`crate::guid::entry`]) while a pet's embeds a pet number
//! ([`crate::guid::pet_number`]), so [`WorldWriter::creature_query`] cannot name a pet and
//! [`WorldWriter::pet_name_query`] is the only thing that can. Every answer is cacheable forever
//! (a name never changes under a guid), which is what makes these the ask-once family.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Ask for a player character's name/race/gender/class (`CMSG_NAME_QUERY`, a full 8-byte guid —
    /// vmangos `QueryPlayerName::ReadFromWorldPacket`). Answered by `SMSG_NAME_QUERY_RESPONSE`.
    pub fn name_query(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_NAME_QUERY, &messages::full_guid(guid))
    }

    /// Ask for a creature template's name/subname (`CMSG_CREATURE_QUERY`: entry + guid). The `entry`
    /// is the one embedded in the creature's guid bits 24–47 ([`crate::guid::entry`]). Answered by
    /// `SMSG_CREATURE_QUERY_RESPONSE`.
    pub fn creature_query(&mut self, entry: u32, guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_CREATURE_QUERY,
            &messages::creature_query(entry, guid),
        )
    }

    /// Ask for a pet's name (`CMSG_PET_NAME_QUERY`: pet number + guid). A pet's guid holds a pet
    /// number where a creature's holds its template entry ([`crate::guid::pet_number`]), so
    /// [`Self::creature_query`] cannot name one — this is the only query that can. Answered by
    /// `SMSG_PET_NAME_QUERY_RESPONSE`, or by silence if the pet is gone.
    pub fn pet_name_query(&mut self, pet_number: u32, guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_PET_NAME_QUERY,
            &messages::pet_name_query(pet_number, guid),
        )
    }
}
