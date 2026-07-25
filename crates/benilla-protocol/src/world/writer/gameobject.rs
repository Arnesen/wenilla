//! The GameObject family's `WorldWriter` sends — the single player-facing USE verb and the
//! ask-once template head. Bodies in [`crate::messages::gameobject`], whose scope this mirrors.
//! Split out of `writer/mod.rs` (decision 0636).
//!
//! Two verbs is the whole family by design (decision 0236): the client does not know or care what
//! kind of GameObject it just clicked — one `CMSG_GAMEOBJ_USE` goes out and the *server* fans it
//! out by GO type. Everything the client needs to draw the thing and pick its cursor comes from the
//! template query.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Use a world GameObject (`CMSG_GAMEOBJ_USE`, layout in [`messages::gameobj_use`]) — the single
    /// player-facing verb for any usable GO (decision 0236): a full guid naming the chest/door/quest
    /// object/lever. The server fans it out by GO type — a chest answers with `SMSG_LOOT_RESPONSE`
    /// (the loot window), a questgiver GO with the gossip/quest packets, a door with a
    /// `GAMEOBJECT_STATE` flip via `UPDATE_OBJECT` — or refuses silently (out of range, locked, no
    /// quest). There is no dedicated success reply.
    pub fn gameobj_use(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_GAMEOBJ_USE, &messages::gameobj_use(guid))
    }

    /// Ask for a GameObject template's type/display/name/`data[24]` head (`CMSG_GAMEOBJECT_QUERY`:
    /// entry + guid — the ask-once template lookup, decision 0236). The `entry` is the one embedded
    /// in the GameObject's guid bits 24–47 ([`crate::guid::entry`]), the same convention as
    /// [`WorldWriter::creature_query`]. Answered by `SMSG_GAMEOBJECT_QUERY_RESPONSE`.
    pub fn gameobject_query(&mut self, entry: u32, guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_GAMEOBJECT_QUERY,
            &messages::gameobject_query(entry, guid),
        )
    }
}
