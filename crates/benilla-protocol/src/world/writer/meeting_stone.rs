//! The meeting-stone queue's sends (decision 1963).

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Leave the meeting-stone queue (`CMSG 0x293`, empty) — `CancelMeetingStoneRequest()`'s
    /// packet, sent by the party leader (or a player in no party).
    pub fn meeting_stone_leave(&mut self) -> Result<()> {
        self.send(
            opcode::CMSG_MEETINGSTONE_LEAVE,
            &messages::meeting_stone_leave(),
        )
    }

    /// Ask for the meeting-stone status (`CMSG 0x296`, empty) — the enter-world query the
    /// reference sends once per world session (decision 1974).
    pub fn meeting_stone_status_query(&mut self) -> Result<()> {
        self.send(opcode::CMSG_MEETINGSTONE_STATUS_QUERY, &[])
    }
}
