//! Taxi/flight-master messages — the "right-click a flight master → pick a destination → fly"
//! wire (opcodes 425-431 + 786, vmangos `Opcodes_1_12_1.h`, VERIFIED; decision 0484). Bodies from
//! vmangos `Handlers/TaxiHandler.cpp` (the SMSG writers) and the hand-serialized
//! `Server/Packets/Taxi.{h,cpp}` (the CMSG readers + the two hand-serialized SMSGs,
//! `TaxiNodeStatus`/`ActivateTaxiReply`/`NewTaxiPath`). Every guid on this wire, both directions,
//! is a **plain** u64 — `ObjectGuid`'s own `ByteBuffer` stream operators (`ObjectGuid.cpp:174-186`)
//! read/write the raw value; `PackedGuid` is a distinct type these packets never reach for. The
//! flight itself (`SMSG_MONSTER_MOVE`, the mount rails) rides existing wire families
//! (`messages::monster_move`, decision 0441) — this module is only the menu + activate send.

use std::io;

use crate::wire::{read_u32_le, read_u64_le, read_u8};

/// The known-node bitmask (`SHOWTAXINODES`'s tail; vmangos `PlayerTaxi::m_taximask`, a `TaxiMask`
/// = `uint32[8]`, `DBCStructure.h:842-844` `TaxiMaskSize`/`TaxiMask`, written by
/// `PlayerTaxi::AppendTaximaskTo`, `PlayerTaxi.cpp:53-65`). Bit law (`PlayerTaxi.h:37-49`,
/// `IsTaximaskNodeKnown`/`SetTaximaskNode`): a node id is 1-based, `field = (id-1)/32`, `bit =
/// (id-1)%32` within that word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TaxiMask(pub [u32; 8]);

impl TaxiMask {
    /// Whether `node_id` is known. Node id `0` is never valid (ids are 1-based) and reads
    /// unknown; an id past the mask's 256-node range reads unknown too (the shipped 1.12 catalog
    /// tops out at 85 nodes, so real data never reaches that branch).
    pub fn is_known(&self, node_id: u32) -> bool {
        node_id != 0
            && self
                .0
                .get(((node_id - 1) / 32) as usize)
                .is_some_and(|word| word & (1 << ((node_id - 1) % 32)) != 0)
    }
}

/// `TaxiError` (vmangos `Objects/Player.h:404-416`) — `SMSG_ACTIVATETAXIREPLY`'s `replyCode`.
/// Codes actually assigned by `Player::ActivateTaxiPathTo` (`Player.cpp:18008-18252`, this
/// session's grep): OK, UNSPECIFIED_SERVER_ERROR, NO_SUCH_PATH, NOT_ENOUGH_MONEY, TOO_FAR, BUSY,
/// ALREADY_MOUNTED, SHAPESHIFTED. `NO_VENDOR_NEARBY`/`NOT_VISITED`/`PLAYER_MOVING`/`SAME_NODE`/
/// `NOT_STANDING` are defined in the enum but no send site ever assigns them — kept for
/// completeness (a private-server fork could still emit one) but never observed on the wire.
pub mod taxi_reply {
    pub const OK: u32 = 0;
    pub const UNSPECIFIED_SERVER_ERROR: u32 = 1;
    pub const NO_SUCH_PATH: u32 = 2;
    pub const NOT_ENOUGH_MONEY: u32 = 3;
    pub const TOO_FAR: u32 = 4;
    pub const NO_VENDOR_NEARBY: u32 = 5;
    pub const NOT_VISITED: u32 = 6;
    pub const BUSY: u32 = 7;
    pub const ALREADY_MOUNTED: u32 = 8;
    pub const SHAPESHIFTED: u32 = 9;
    pub const PLAYER_MOVING: u32 = 10;
    pub const SAME_NODE: u32 = 11;
    pub const NOT_STANDING: u32 = 12;
}

/// Body of `CMSG_TAXINODE_STATUS_QUERY` (vmangos `Packets/Taxi.cpp`,
/// `TaxiNodeStatusQuery::ReadFromWorldPacket`): one full 8-byte guid — "the GUID of the
/// flightmaster" per vmangos's own comment (a normal client sends the NPC it's standing near, not
/// itself). Answered by `SMSG_TAXINODE_STATUS`.
pub fn taxi_node_status_query(flightmaster_guid: u64) -> Vec<u8> {
    flightmaster_guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_TAXIQUERYAVAILABLENODES` (vmangos `Packets/Taxi.cpp`,
/// `TaxiQueryAvailableNodes::ReadFromWorldPacket`): one full 8-byte flight-master guid — the
/// direct taxi-map opener (decision 0496 I4: CONFIRMED as built — the client's interact ladder is
/// first-match-wins low→high over `UNIT_NPC_FLAGS`, so a gossip+taxi NPC pre-empts to gossip and
/// only a pure flightmaster sends this; the gossip taxi option reaches the same menu server-side
/// through `SendTaxiMenu`). Answered by `SMSG_SHOWTAXINODES` on a known node; a
/// never-visited node instead answers `SMSG_NEW_TAXI_PATH` + `SMSG_TAXINODE_STATUS(known=1)` —
/// the server "learns, never opens" on first contact (vmangos `SendLearnNewTaxiNode`).
pub fn taxi_query_available_nodes(flightmaster_guid: u64) -> Vec<u8> {
    flightmaster_guid.to_le_bytes().to_vec()
}

/// Body of `CMSG_ACTIVATETAXI` (vmangos `Packets/Taxi.cpp`, `ActivateTaxi::ReadFromWorldPacket`):
/// `u64 flightmasterGuid, u32 node1 (source), u32 node2 (dest)` — a single-hop flight. Answered
/// by `SMSG_ACTIVATETAXIREPLY`; success continues into the mount + `SMSG_MONSTER_MOVE` flight.
pub fn activate_taxi(flightmaster_guid: u64, node1: u32, node2: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(&flightmaster_guid.to_le_bytes());
    body.extend_from_slice(&node1.to_le_bytes());
    body.extend_from_slice(&node2.to_le_bytes());
    body
}

/// Body of `CMSG_ACTIVATETAXIEXPRESS` (vmangos `Packets/Taxi.cpp`,
/// `ActivateTaxiExpress::ReadFromWorldPacket`, `> CLIENT_BUILD_1_9_4` — active for 5875): `u64
/// flightmasterGuid, u32 totalcost, u32 nodeCount, nodeCount x u32 node` — the full multi-hop
/// chain in one send when **no direct `TaxiPath` edge exists current→target** (decision 0496
/// §TU-3, byte-verified discriminator at `0x4dbad0`: it is edge presence, not hop count — a
/// direct edge sends the single-hop `ActivateTaxi` below even when the drawn route detours
/// through intermediate stops).
pub fn activate_taxi_express(flightmaster_guid: u64, total_cost: u32, nodes: &[u32]) -> Vec<u8> {
    let mut body = Vec::with_capacity(16 + nodes.len() * 4);
    body.extend_from_slice(&flightmaster_guid.to_le_bytes());
    body.extend_from_slice(&total_cost.to_le_bytes());
    body.extend_from_slice(&(nodes.len() as u32).to_le_bytes());
    for &n in nodes {
        body.extend_from_slice(&n.to_le_bytes());
    }
    body
}

/// Read `SMSG_SHOWTAXINODES` (vmangos `WorldSession::SendTaxiMenu`, `TaxiHandler.cpp:82-96`):
/// `u32 gate`, then **iff `gate != 0`** `u64 flightmasterGuid` (plain, not packed), `u32
/// nearestNode` (the node closest to the flight master — the map's "you are here"), `u32[8]
/// knownMask` ([`TaxiMask`], `AppendTaximaskTo`). The leading word is a real conditional gate in
/// the client's own parser (`0x5ece60` reads the rest only when it is nonzero — byte-verified,
/// decision 0496 §claim-1), not a constant to skip; vmangos always sends `1` (48 bytes), so the
/// gated-off 4-byte body never occurs live — parsed faithfully anyway. Returns `(gate,
/// flightmaster, nearest_node, mask)`, all-zero past a zero gate (decode drops it — no event).
pub(super) fn read_show_taxi_nodes(r: &mut &[u8]) -> io::Result<(u32, u64, u32, TaxiMask)> {
    let window = read_u32_le(r)?;
    if window == 0 {
        return Ok((0, 0, 0, TaxiMask::default()));
    }
    let flightmaster = read_u64_le(r)?;
    let nearest_node = read_u32_le(r)?;
    let mut mask = [0u32; 8];
    for word in &mut mask {
        *word = read_u32_le(r)?;
    }
    Ok((window, flightmaster, nearest_node, TaxiMask(mask)))
}

/// Read `SMSG_TAXINODE_STATUS` (vmangos `TaxiNodeStatus::AppendBodyTo`, `Packets/Taxi.cpp:36-40`):
/// `u64 guid` (plain), `u8 known` (a C++ `bool` — `ByteBuffer::operator<<(bool const&)` appends
/// exactly one byte, `ByteBuffer.h:194-197`). Returns `(guid, known)`.
pub(super) fn read_taxi_node_status(r: &mut &[u8]) -> io::Result<(u64, u8)> {
    Ok((read_u64_le(r)?, read_u8(r)?))
}

/// Read `SMSG_ACTIVATETAXIREPLY` (vmangos `ActivateTaxiReply::AppendBodyTo`,
/// `Packets/Taxi.cpp:46-49`): one [`taxi_reply`] `u32` code.
pub(super) fn read_activate_taxi_reply(r: &mut &[u8]) -> io::Result<u32> {
    read_u32_le(r)
}
