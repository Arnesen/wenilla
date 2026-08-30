//! The Help window's five sends — the whole client half of the GM trouble-ticket flow
//! (decision 1673).
//!
//! Three of the five have empty bodies, which is the shape of the feature: the client holds no
//! ticket state of its own, so "show me my ticket", "abandon it" and "is the queue up?" are each a
//! bare opcode and the server's answer is the only truth. The two that carry a body — file and
//! edit — are byte-verified against the real client's own senders (`0x5ef740` / `0x5efac0`); see
//! [`crate::messages::gm_ticket`] for why the category is a single byte in both.
//!
//! **Nothing here may be retried on a timeout.** vmangos answers several refusals with silence
//! rather than a response code (queue off, under `GMTickets.MinLevel`, category >= 11 —
//! `GMTicketHandler.cpp:91,106-113`), and it treats more than two `CMSG_GMTICKET_UPDATETEXT` in a
//! single world tick as flooding, whose default sanction is a **kick**
//! (`WorldSession.cpp:1316-1342`, `Antiflood.Sanction = 8`). So a client that re-sent an edit
//! because no answer came would disconnect itself. The Help window asks once per click, exactly as
//! the reference does.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// File a new ticket (`CMSG_GMTICKET_CREATE`) — the Help window's Submit.
    ///
    /// `category` is a `GMTicketCategory.dbc` id (1..10, the clicked row); `map`/`pos` are where
    /// the player is standing, which is what lets a GM `.tele` to the reported spot. Answered by
    /// `SMSG_GMTICKET_CREATE` — **or by nothing at all**, on the several vmangos paths that refuse
    /// silently (module doc).
    pub fn gm_ticket_create(
        &mut self,
        category: u8,
        map: u32,
        pos: [f32; 3],
        text: &str,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_GMTICKET_CREATE,
            &messages::gm_ticket_create(category, map, pos, text),
        )
    }

    /// Edit the open ticket's text (`CMSG_GMTICKET_UPDATETEXT`) — the Help window's "Save Changes".
    ///
    /// Carries the category byte the client carries even though vmangos reads and discards it; the
    /// category of an existing ticket cannot actually be changed this way. Answered by
    /// `SMSG_GMTICKET_UPDATETEXT`. **Rate-limited server-side** — see the module doc.
    pub fn gm_ticket_updatetext(&mut self, category: u8, text: &str) -> Result<()> {
        self.send(
            opcode::CMSG_GMTICKET_UPDATETEXT,
            &messages::gm_ticket_updatetext(category, text),
        )
    }

    /// Ask for the open ticket (`CMSG_GMTICKET_GETTICKET`, empty body) — `GetGMTicket()`.
    ///
    /// The client's own poll: once on world entry and every 10 minutes while the ticket status
    /// toast is up. Answered by `SMSG_GMTICKET_GETTICKET`, and on vmangos **also** by an
    /// unsolicited `SMSG_QUERY_TIME_RESPONSE` — its handler calls `SendQueryTimeResponse()` as its
    /// first statement (`GMTicketHandler.cpp:34`), so two packets come back for this one request.
    pub fn gm_ticket_get(&mut self) -> Result<()> {
        self.send(opcode::CMSG_GMTICKET_GETTICKET, &[])
    }

    /// Abandon the open ticket (`CMSG_GMTICKET_DELETETICKET`, empty body) — the
    /// `HELP_TICKET_ABANDON_CONFIRM` dialog's Yes. Answered by `SMSG_GMTICKET_DELETETICKET`
    /// carrying 9, or — with no ticket to delete — by nothing at all on vmangos.
    pub fn gm_ticket_delete(&mut self) -> Result<()> {
        self.send(opcode::CMSG_GMTICKET_DELETETICKET, &[])
    }

    /// Ask whether the petition queue is taking tickets (`CMSG_GMTICKET_SYSTEMSTATUS`, empty body)
    /// — `GetGMStatus()`, which the Help window calls from its own OnShow. Answered by
    /// `SMSG_GMTICKETSYSTEMSTATUS`.
    pub fn gm_ticket_system_status(&mut self) -> Result<()> {
        self.send(opcode::CMSG_GMTICKET_SYSTEMSTATUS, &[])
    }
}
