//! The one area-trigger send. Body in [`crate::messages::area_trigger`], whose scope this mirrors.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Report that we just walked into an `AreaTrigger.dbc` volume (`CMSG_AREATRIGGER`, layout in
    /// [`messages::area_trigger`]) — the client's whole part in the trigger system: it owns the
    /// geometry and names the id, and the **server** decides what that trigger means (a teleport,
    /// a quest's explore objective, the inn's rested state, a battleground's entrance list).
    ///
    /// There is no dedicated success reply: a teleport arrives as the ordinary
    /// `SMSG_TRANSFER_PENDING` + `SMSG_NEW_WORLD` pair or a same-map `MSG_MOVE_TELEPORT_ACK`, a
    /// refusal as `SMSG_AREA_TRIGGER_MESSAGE`, and most triggers answer with nothing at all.
    pub fn area_trigger(&mut self, trigger_id: u32) -> Result<()> {
        self.send(
            opcode::CMSG_AREATRIGGER,
            &messages::area_trigger(trigger_id),
        )
    }
}
