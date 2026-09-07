//! The battleground queue's sends (decision 1963).

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Answer a ready battleground (`CMSG_BATTLEFIELD_PORT`): the slot's map id and whether to
    /// enter — `AcceptBattlefieldPort(index, accept)`'s packet.
    pub fn battlefield_port(&mut self, map_id: u32, accept: bool) -> Result<()> {
        self.send(
            opcode::CMSG_BATTLEFIELD_PORT,
            &messages::battlefield_port(map_id, accept),
        )
    }

    /// Ask for the scoreboard (`MSG_PVP_LOG_DATA`, empty) — `RequestBattlefieldScoreData()`'s
    /// packet; the 5000 ms throttle is the caller's (decision 1972).
    pub fn request_battlefield_score_data(&mut self) -> Result<()> {
        self.send(opcode::MSG_PVP_LOG_DATA, &[])
    }

    /// Leave the battleground (`CMSG_LEAVE_BATTLEFIELD`): `LeaveBattlefield()`'s packet, sent
    /// only once the scoreboard's "ended" byte has arrived (decision 1972).
    pub fn leave_battlefield(&mut self, map_id: u32) -> Result<()> {
        self.send(
            opcode::CMSG_LEAVE_BATTLEFIELD,
            &messages::leave_battlefield(map_id),
        )
    }

    /// Reopen a queued battleground's instance list (`CMSG_BATTLEFIELD_LIST`):
    /// `ShowBattlefieldList(index)`'s packet, the queued slot's map (decision 1974).
    pub fn battlefield_list(&mut self, map_id: u32) -> Result<()> {
        self.send(
            opcode::CMSG_BATTLEFIELD_LIST,
            &messages::battlefield_list(map_id),
        )
    }

    /// Join through the battlemaster the list came from (`CMSG_BATTLEMASTER_JOIN`) —
    /// `JoinBattlefield`'s packet when the cached guid is non-zero (decision 1974).
    pub fn battlemaster_join(
        &mut self,
        battlemaster: u64,
        map_id: u32,
        instance_id: u32,
        as_group: bool,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_BATTLEMASTER_JOIN,
            &messages::battlemaster_join(battlemaster, map_id, instance_id, as_group),
        )
    }

    /// Join without a battlemaster (`CMSG_BATTLEFIELD_JOIN`) — `JoinBattlefield`'s packet when
    /// the list arrived with a zero guid (decision 1974).
    pub fn battlefield_join(
        &mut self,
        map_id: u32,
        instance_id: u32,
        as_group: bool,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_BATTLEFIELD_JOIN,
            &messages::battlefield_join(map_id, instance_id, as_group),
        )
    }

    /// Ask for every queue slot's state (`CMSG_BATTLEFIELD_STATUS`, empty) — the reference's
    /// world-enter reset sends it once per entry (decision 1974).
    pub fn battlefield_status(&mut self) -> Result<()> {
        self.send(opcode::CMSG_BATTLEFIELD_STATUS, &[])
    }

    /// Ask for the teammates' positions (`MSG_BATTLEGROUND_PLAYER_POSITIONS`, empty) —
    /// `RequestBattlefieldPositions()`'s packet; the 5000 ms throttle is the caller's (1980).
    pub fn request_battlefield_positions(&mut self) -> Result<()> {
        self.send(opcode::MSG_BATTLEGROUND_PLAYER_POSITIONS, &[])
    }
}
