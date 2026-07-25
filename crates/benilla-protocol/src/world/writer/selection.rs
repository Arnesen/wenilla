//! The two `WorldWriter` sends that set our **selection** — picking a unit, and inspecting a
//! player. Both carry a raw 8-byte guid ([`crate::messages::full_guid`]), and both land in the
//! server's `UNIT_FIELD_TARGET` for us: `CMSG_INSPECT` sets the selection as a side effect
//! (vmangos `MiscHandler.cpp:945`), which is why the real client sends it even though the inspect
//! window paints from already-streamed PUBLIC fields. Split out of `writer/mod.rs`
//! (decision 0636).

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Set (or clear) our current target: `CMSG_SET_SELECTION` carrying a **full 8-byte GUID** (verified
    /// vmangos `SetSelection::ReadFromWorldPacket` — `recv_data >> guid` reads a raw `uint64`, not a
    /// packed guid). `guid == 0` clears the selection. The real client sends this the moment the local
    /// player picks a unit; the server records it in the player's `UNIT_FIELD_TARGET` and relays it to
    /// nearby observers.
    pub fn set_selection(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_SET_SELECTION, &messages::full_guid(guid))
    }

    /// Ask to inspect a player (`CMSG_INSPECT`, a full 8-byte guid — vmangos
    /// `WorldPackets::Misc::Inspect`). Server-side this *also* sets our selection to the target
    /// (`MiscHandler.cpp:945`), which is why the real client sends it even though the inspect
    /// window paints from already-streamed PUBLIC fields. The `SMSG_INSPECT` reply echoes the guid
    /// and nothing else — we neither wait for it nor parse it (decision 0631).
    pub fn inspect(&mut self, guid: u64) -> Result<()> {
        self.send(opcode::CMSG_INSPECT, &messages::full_guid(guid))
    }
}
