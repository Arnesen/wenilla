//! The innkeeper bind answer — the client's one send in the bind family (decision 1331).
//!
//! There is no "bind me here" verb: the flow starts with the *server's* `SMSG_BINDER_CONFIRM`
//! (raised by selecting the innkeeper's gossip line), and this is the Yes. The guid is the one
//! that arrived in the confirm; vmangos resolves it back to a live innkeeper in interact range
//! (`HandleBinderActivateOpcode`), so it is load-bearing rather than an echo.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Accept an innkeeper's bind offer (`CMSG_BINDER_ACTIVATE`) — the `CONFIRM_BINDER` dialog's
    /// Accept. The server answers by casting spell 3286 on us, which lands as
    /// `SMSG_BINDPOINTUPDATE` + `SMSG_PLAYERBOUND`; declining sends nothing at all.
    pub fn binder_activate(&mut self, binder_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_BINDER_ACTIVATE,
            &messages::binder_activate(binder_guid),
        )
    }
}
