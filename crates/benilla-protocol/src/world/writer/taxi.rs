//! The flight-master family's `WorldWriter` sends — the known-status probe, the map open, and the
//! two flight verbs. Bodies in [`crate::messages::taxi`], whose scope this mirrors. Split out of
//! `writer/mod.rs` (decision 0636).
//!
//! Two flight opcodes, one discriminator (decision 0496 §TU-3): send `CMSG_ACTIVATETAXI` when a
//! direct `TaxiPath` edge exists current→target, and `CMSG_ACTIVATETAXIEXPRESS` with the whole node
//! chain when it doesn't — the real rule the verdict corrected from a hop-count guess. Both answer
//! `SMSG_ACTIVATETAXIREPLY`.

use anyhow::Result;

use crate::messages::{self, opcode};

use super::WorldWriter;

impl WorldWriter {
    /// Ask a nearby flight master's known status (`CMSG_TAXINODE_STATUS_QUERY`, layout in
    /// [`messages::taxi_node_status_query`]): the flight master's guid, not ours. Answered by
    /// `SMSG_TAXINODE_STATUS` (a `TaxiNodeStatus` event, decision 0484).
    pub fn taxi_node_status_query(&mut self, flightmaster_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_TAXINODE_STATUS_QUERY,
            &messages::taxi_node_status_query(flightmaster_guid),
        )
    }

    /// Open a flight master's taxi map (`CMSG_TAXIQUERYAVAILABLENODES`, layout in
    /// [`messages::taxi_query_available_nodes`], decision 0496 I4 — CONFIRMED as built: the
    /// interact ladder is first-match-wins low→high over `UNIT_NPC_FLAGS`, so only a pure
    /// flightmaster reaches here). A known node answers `SMSG_SHOWTAXINODES` (a `TaxiNodesShown`
    /// event); a never-visited node instead answers the first-visit learn pair (`NewTaxiPath` +
    /// `TaxiNodeStatus`) and opens no menu on this click.
    pub fn taxi_query_available_nodes(&mut self, flightmaster_guid: u64) -> Result<()> {
        self.send(
            opcode::CMSG_TAXIQUERYAVAILABLENODES,
            &messages::taxi_query_available_nodes(flightmaster_guid),
        )
    }

    /// Fly a single hop (`CMSG_ACTIVATETAXI`, layout in [`messages::activate_taxi`]): the
    /// flight-master guid, the source node, the destination node. Answered by
    /// `SMSG_ACTIVATETAXIREPLY`; success continues into the mount + `SMSG_MONSTER_MOVE` flight.
    pub fn activate_taxi(
        &mut self,
        flightmaster_guid: u64,
        source_node: u32,
        dest_node: u32,
    ) -> Result<()> {
        self.send(
            opcode::CMSG_ACTIVATETAXI,
            &messages::activate_taxi(flightmaster_guid, source_node, dest_node),
        )
    }

    /// Fly a multi-hop chain in one send (`CMSG_ACTIVATETAXIEXPRESS`, layout in
    /// [`messages::activate_taxi_express`], decision 0496 §TU-3 — sent when no direct `TaxiPath`
    /// edge exists current→target, the real discriminator the verdict corrected from a hop-count
    /// guess): the flight-master guid, the route's combined fare, and the full node chain in
    /// order. Answered by `SMSG_ACTIVATETAXIREPLY`, same as [`Self::activate_taxi`].
    pub fn activate_taxi_express(
        &mut self,
        flightmaster_guid: u64,
        total_cost: u32,
        nodes: &[u32],
    ) -> Result<()> {
        self.send(
            opcode::CMSG_ACTIVATETAXIEXPRESS,
            &messages::activate_taxi_express(flightmaster_guid, total_cost, nodes),
        )
    }
}
