//! The battleground queue's wire (decision 1963; wow-re `staticpopup-dialog-bindings.md` §7):
//! the server's per-slot status and the port answer `AcceptBattlefieldPort` sends. Three queue
//! slots exist in the client (`0xb6e9d0`, stride `0x20`), addressed 1-based from Lua.

use std::io::{self, Read};

use crate::wire::{read_u32_le, read_u8};

/// `SMSG_BATTLEFIELD_STATUS`, one slot's update (VERIFIED at the bytes, handler `0x4aa850`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BattlefieldStatus {
    /// Which of the three slots, 0-based on the wire.
    pub slot: u32,
    /// The battleground's Map.dbc row id; zero clears the slot.
    pub map_id: u32,
    pub bracket: u8,
    /// `+0x10` — the instance id, `GetBattlefieldStatus`'s third value (1963 parked this dword
    /// as reader-less; `battlefield-verb-family.md` §2.2 found the reader, 1974).
    pub instance_id: u32,
    pub status: u32,
    /// Status 2: the port deadline's delta, in ms (`+0x14 = now + Δ`, `GetBattlefieldPortExpiration`).
    pub time_ms: Option<u32>,
    /// Status 3 (`battlefield-verb-family.md` §4.2, 1972): `(Δ₁, Δ₂)` — the instance's expiration
    /// delta (`[0xb6ebb8] = now + Δ₁`, `GetBattlefieldInstanceExpiration`) and its elapsed run time
    /// (`[0xb6ebbc] = now − Δ₂`, `GetBattlefieldInstanceRunTime`).
    pub in_progress: Option<(u32, u32)>,
    /// Status 1 (§4.2): `(estimated wait ms, raw; Δ waited)` — `[slot+0x18]` and
    /// `[slot+0x1c] = now − Δ`, the `GetBattlefieldEstimatedWaitTime`/`GetBattlefieldTimeWaited` pair.
    /// 1963's reader dropped this tail; the reference reads it (1972).
    pub queued: Option<(u32, u32)>,
}

/// Parse `SMSG_BATTLEFIELD_STATUS`: `u32 slot`, `u32 mapId`, and — only for a non-zero map —
/// `u8 bracket`, `u32 instanceId`, `u32 status`, then the status-conditional tail: two `u32` for
/// status 1, one for status 2, two for status 3.
pub(super) fn read_battlefield_status(r: &mut impl Read) -> io::Result<BattlefieldStatus> {
    let slot = read_u32_le(r)?;
    let map_id = read_u32_le(r)?;
    if map_id == 0 {
        return Ok(BattlefieldStatus {
            slot,
            map_id,
            bracket: 0,
            instance_id: 0,
            status: 0,
            time_ms: None,
            in_progress: None,
            queued: None,
        });
    }
    let bracket = read_u8(r)?;
    let instance_id = read_u32_le(r)?;
    let status = read_u32_le(r)?;
    let time_ms = if status == 2 {
        Some(read_u32_le(r)?)
    } else {
        None
    };
    let in_progress = if status == 3 {
        Some((read_u32_le(r)?, read_u32_le(r)?))
    } else {
        None
    };
    let queued = if status == 1 {
        Some((read_u32_le(r)?, read_u32_le(r)?))
    } else {
        None
    };
    Ok(BattlefieldStatus {
        slot,
        map_id,
        bracket,
        instance_id,
        status,
        time_ms,
        in_progress,
        queued,
    })
}

/// One scoreboard row of `MSG_PVP_LOG_DATA` (VERIFIED at the bytes, handler `0x4aab30`; wow-re
/// `battlefield-verb-family.md` §2.3/§4.3, 1972). The client's 0x40-byte block, wire order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PvpLogRow {
    pub guid: u64,
    /// Wire field 2 — read before the kills, stored at `+0x1c`.
    pub rank: u32,
    pub killing_blows: u32,
    pub honorable_kills: u32,
    pub deaths: u32,
    pub honor_gained: u32,
    /// The extra-stat dwords, at most eight stored — the client consumes and discards the rest.
    pub stats: Vec<u32>,
}

/// `MSG_PVP_LOG_DATA` inbound: the whole scoreboard, rows in wire order (nothing here sorts).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PvpLogData {
    /// `u8 != 0` — the battleground has ended; `LeaveBattlefield` sends nothing until this is set.
    pub ended: bool,
    /// Read only when `ended`: `0` = Horde, `1` = Alliance (`GetBattlefieldWinner`).
    pub winner: Option<u8>,
    pub rows: Vec<PvpLogRow>,
}

/// Parse `MSG_PVP_LOG_DATA` (§4.3): `u8 ended`, `u8 winner` iff ended, `u32 count`, `count` rows of
/// `u64 guid, u32 rank, u32 kb, u32 hk, u32 deaths, u32 honor, u32 statCount, statCount × u32`.
/// The client stores no more than eight stats per row and clamps nothing else — a count past its
/// 80 blocks is its own anomaly (§10); ours keeps every row the wire carries.
pub(super) fn read_pvp_log_data(r: &mut impl Read) -> io::Result<PvpLogData> {
    let ended = read_u8(r)? != 0;
    let winner = if ended { Some(read_u8(r)?) } else { None };
    let count = read_u32_le(r)?;
    let mut rows = Vec::with_capacity(count.min(80) as usize);
    for _ in 0..count {
        let guid = crate::wire::read_u64_le(r)?;
        let rank = read_u32_le(r)?;
        let killing_blows = read_u32_le(r)?;
        let honorable_kills = read_u32_le(r)?;
        let deaths = read_u32_le(r)?;
        let honor_gained = read_u32_le(r)?;
        let stat_count = read_u32_le(r)?;
        let mut stats = Vec::with_capacity(stat_count.min(8) as usize);
        for i in 0..stat_count {
            let v = read_u32_le(r)?;
            if i < 8 {
                stats.push(v);
            }
        }
        rows.push(PvpLogRow {
            guid,
            rank,
            killing_blows,
            honorable_kills,
            deaths,
            honor_gained,
            stats,
        });
    }
    Ok(PvpLogData {
        ended,
        winner,
        rows,
    })
}

/// Body of `CMSG_LEAVE_BATTLEFIELD` (VERIFIED, `0x4abe60`): `u32 mapId` — the active slot's map,
/// or the literal 0 when no slot is active.
pub fn leave_battlefield(map_id: u32) -> Vec<u8> {
    map_id.to_le_bytes().to_vec()
}

/// `SMSG_BATTLEFIELD_LIST` (VERIFIED at the bytes, handler `0x4aa6c0`; wow-re
/// `battlefield-verb-family.md` §4.1, 1974): the instance list a battlemaster (or
/// `ShowBattlefieldList`) opens.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct BattlefieldList {
    /// The battlemaster's guid — `0` when the list was opened without an NPC. Cached by the
    /// client and read by `JoinBattlefield` to choose between the two join opcodes.
    pub battlemaster: u64,
    /// The battleground's Map.dbc row id.
    pub map_id: u32,
    /// The level-bracket index; the client derives the bracket's min/max from it and the map row.
    pub bracket: u8,
    /// The instance ids, wire order — nothing sorts them; index 0 on the wire is instance 1 in Lua.
    pub instances: Vec<u32>,
}

/// Parse `SMSG_BATTLEFIELD_LIST` (§4.1): `u64 battlemaster`, `u32 mapId`, `u8 bracket`,
/// `u32 count`, `count × u32 instanceId`.
pub(super) fn read_battlefield_list(r: &mut impl Read) -> io::Result<BattlefieldList> {
    let battlemaster = crate::wire::read_u64_le(r)?;
    let map_id = read_u32_le(r)?;
    let bracket = read_u8(r)?;
    let count = read_u32_le(r)?;
    let mut instances = Vec::with_capacity(count.min(64) as usize);
    for _ in 0..count {
        instances.push(read_u32_le(r)?);
    }
    Ok(BattlefieldList {
        battlemaster,
        map_id,
        bracket,
        instances,
    })
}

/// One teammate's map position off `MSG_BATTLEGROUND_PLAYER_POSITIONS` (1980): raw world
/// floats — the client prefers the live object's position when it has one, and normalizes
/// either through the world-map projection under the active battleground's map.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BattlefieldPosition {
    pub guid: u64,
    pub x: f32,
    pub y: f32,
}

/// `MSG_BATTLEGROUND_PLAYER_POSITIONS` inbound (VERIFIED at the bytes, handler `0x4aad40`; wow-re
/// `worldmap-arrow-and-positions.md` §3.1): the teammates not in the requester's group, then the
/// friendly flag carrier when there is one.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct BattlefieldPositions {
    pub players: Vec<BattlefieldPosition>,
    pub carrier: Option<BattlefieldPosition>,
}

/// The client's store holds 40 entries and its handler writes past them for a larger count
/// (§3.2, an overrun the reference has); ours keeps the first 40.
pub const BATTLEFIELD_POSITIONS_MAX: usize = 40;

/// Parse it (§3.1): `u32 count`, `count × (u64, f32, f32)`, `u8 hasCarrier`, and the carrier
/// triple only when that byte is non-zero.
pub(super) fn read_battlefield_positions(r: &mut impl Read) -> io::Result<BattlefieldPositions> {
    let count = read_u32_le(r)?;
    let mut players = Vec::with_capacity((count as usize).min(BATTLEFIELD_POSITIONS_MAX));
    for i in 0..count {
        let guid = crate::wire::read_u64_le(r)?;
        let x = crate::wire::read_f32_le(r)?;
        let y = crate::wire::read_f32_le(r)?;
        if (i as usize) < BATTLEFIELD_POSITIONS_MAX {
            players.push(BattlefieldPosition { guid, x, y });
        }
    }
    let carrier = if read_u8(r)? != 0 {
        Some(BattlefieldPosition {
            guid: crate::wire::read_u64_le(r)?,
            x: crate::wire::read_f32_le(r)?,
            y: crate::wire::read_f32_le(r)?,
        })
    } else {
        None
    };
    Ok(BattlefieldPositions { players, carrier })
}

/// Body of `CMSG_BATTLEFIELD_LIST` (VERIFIED, `0x4ab8c0`): `u32 mapId` of the queued slot.
pub fn battlefield_list(map_id: u32) -> Vec<u8> {
    map_id.to_le_bytes().to_vec()
}

/// Body of `CMSG_BATTLEMASTER_JOIN` (VERIFIED, `0x4a9f60`'s GUID arm): `u64 battlemaster`,
/// `u32 mapId`, `u32 instanceId` (`0` = first available), `u8 asGroup`.
pub fn battlemaster_join(
    battlemaster: u64,
    map_id: u32,
    instance_id: u32,
    as_group: bool,
) -> Vec<u8> {
    let mut body = battlemaster.to_le_bytes().to_vec();
    body.extend_from_slice(&map_id.to_le_bytes());
    body.extend_from_slice(&instance_id.to_le_bytes());
    body.push(u8::from(as_group));
    body
}

/// Body of `CMSG_BATTLEFIELD_JOIN` (VERIFIED, `0x4a9f60`'s no-GUID arm): `u32 mapId`,
/// `u32 instanceId` (`0` = first available), `u8 asGroup`.
pub fn battlefield_join(map_id: u32, instance_id: u32, as_group: bool) -> Vec<u8> {
    let mut body = map_id.to_le_bytes().to_vec();
    body.extend_from_slice(&instance_id.to_le_bytes());
    body.push(u8::from(as_group));
    body
}

/// Body of `CMSG_BATTLEFIELD_PORT` (VERIFIED, `0x4ab3b0`): `u32 mapId` then a genuinely
/// one-byte `accept`, normalised to 0/1 before it reaches the wire.
pub fn battlefield_port(map_id: u32, accept: bool) -> Vec<u8> {
    let mut body = map_id.to_le_bytes().to_vec();
    body.push(u8::from(accept));
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reads_the_conditional_tails() {
        let mut body = vec![
            1u8, 0, 0, 0, 30, 0, 0, 0, 5, 7, 0, 0, 0, 2, 0, 0, 0, 100, 0, 0, 0,
        ];
        let s = read_battlefield_status(&mut body.as_slice()).unwrap();
        assert_eq!(
            (s.slot, s.map_id, s.bracket, s.instance_id, s.status),
            (1, 30, 5, 7, 2)
        );
        assert_eq!(s.time_ms, Some(100));
        body = vec![0u8, 0, 0, 0, 0, 0, 0, 0];
        let s = read_battlefield_status(&mut body.as_slice()).unwrap();
        assert_eq!(
            s.map_id, 0,
            "a zero map clears the slot and ends the packet"
        );
    }

    #[test]
    fn port_is_a_map_id_and_one_byte() {
        assert_eq!(battlefield_port(489, true), vec![0xE9, 1, 0, 0, 1]);
        assert_eq!(battlefield_port(489, false), vec![0xE9, 1, 0, 0, 0]);
    }
}

#[cfg(test)]
mod pvp_log_tests {
    use super::*;

    /// The scoreboard reader: the winner byte only when ended, the rank BEFORE the kills, and no
    /// more than eight stats kept while every one is consumed.
    #[test]
    fn the_scoreboard_reads_its_conditional_winner_and_keeps_eight_stats() {
        let mut body = vec![1u8, 0u8, 1, 0, 0, 0];
        body.extend_from_slice(&7u64.to_le_bytes());
        for v in [5u32, 3, 9, 2, 120, 9] {
            body.extend_from_slice(&v.to_le_bytes());
        }
        for v in 1u32..=9 {
            body.extend_from_slice(&v.to_le_bytes());
        }
        let d = read_pvp_log_data(&mut body.as_slice()).unwrap();
        assert!(d.ended);
        assert_eq!(d.winner, Some(0));
        assert_eq!(d.rows.len(), 1);
        let r = &d.rows[0];
        assert_eq!(
            (
                r.guid,
                r.rank,
                r.killing_blows,
                r.honorable_kills,
                r.deaths,
                r.honor_gained
            ),
            (7, 5, 3, 9, 2, 120)
        );
        assert_eq!(r.stats, vec![1, 2, 3, 4, 5, 6, 7, 8]);

        let body = vec![0u8, 0, 0, 0, 0];
        let d = read_pvp_log_data(&mut body.as_slice()).unwrap();
        assert!(!d.ended && d.winner.is_none() && d.rows.is_empty());
        assert_eq!(leave_battlefield(489), vec![0xE9, 1, 0, 0]);
    }

    /// The instance list: the guid, the map, the bracket byte and the ids in wire order.
    #[test]
    fn the_list_reads_its_guid_bracket_and_instances() {
        let mut body = 0x1234_5678_9abc_def0u64.to_le_bytes().to_vec();
        body.extend_from_slice(&489u32.to_le_bytes());
        body.push(2);
        body.extend_from_slice(&3u32.to_le_bytes());
        for id in [7u32, 3, 11] {
            body.extend_from_slice(&id.to_le_bytes());
        }
        let l = read_battlefield_list(&mut body.as_slice()).unwrap();
        assert_eq!(l.battlemaster, 0x1234_5678_9abc_def0);
        assert_eq!((l.map_id, l.bracket), (489, 2));
        assert_eq!(l.instances, vec![7, 3, 11], "wire order, nothing sorts it");
        let body = [0u8; 8]
            .iter()
            .chain(&30u32.to_le_bytes())
            .chain(&[0u8])
            .chain(&0u32.to_le_bytes())
            .copied()
            .collect::<Vec<_>>();
        let l = read_battlefield_list(&mut body.as_slice()).unwrap();
        assert_eq!(l.battlemaster, 0, "a list opened without an NPC");
        assert!(l.instances.is_empty());
    }

    /// The positions reply: the count-led list, the carrier byte, and the 40-entry cap.
    #[test]
    fn the_positions_reply_reads_the_list_and_the_carrier() {
        let mut body = 2u32.to_le_bytes().to_vec();
        for (g, x, y) in [(0x10u64, 1.5f32, -2.0f32), (0x11, 3.0, 4.0)] {
            body.extend_from_slice(&g.to_le_bytes());
            body.extend_from_slice(&x.to_le_bytes());
            body.extend_from_slice(&y.to_le_bytes());
        }
        body.push(1);
        body.extend_from_slice(&0x20u64.to_le_bytes());
        body.extend_from_slice(&9.0f32.to_le_bytes());
        body.extend_from_slice(&8.0f32.to_le_bytes());
        let p = read_battlefield_positions(&mut body.as_slice()).unwrap();
        assert_eq!(p.players.len(), 2);
        assert_eq!(
            p.players[1],
            BattlefieldPosition {
                guid: 0x11,
                x: 3.0,
                y: 4.0
            }
        );
        assert_eq!(
            p.carrier,
            Some(BattlefieldPosition {
                guid: 0x20,
                x: 9.0,
                y: 8.0
            })
        );
        let body = [0u8, 0, 0, 0, 0];
        let p = read_battlefield_positions(&mut body.as_slice()).unwrap();
        assert!(p.players.is_empty() && p.carrier.is_none());
        let mut body = 41u32.to_le_bytes().to_vec();
        for i in 0..41u64 {
            body.extend_from_slice(&i.to_le_bytes());
            body.extend_from_slice(&[0u8; 8]);
        }
        body.push(0);
        let p = read_battlefield_positions(&mut body.as_slice()).unwrap();
        assert_eq!(
            p.players.len(),
            40,
            "the client's store, not the wire's count"
        );
    }

    /// The two join bodies differ only by the leading guid; the list request is the map alone.
    #[test]
    fn the_join_bodies_and_the_list_request() {
        assert_eq!(
            battlefield_join(489, 0, true),
            vec![0xE9, 1, 0, 0, 0, 0, 0, 0, 1],
            "first available, as a group"
        );
        let mut want = 0x42u64.to_le_bytes().to_vec();
        want.extend_from_slice(&[0xE9, 1, 0, 0, 5, 0, 0, 0, 0]);
        assert_eq!(battlemaster_join(0x42, 489, 5, false), want);
        assert_eq!(battlefield_list(529), vec![0x11, 2, 0, 0]);
    }

    /// Status 1 carries the two wait dwords 1963's reader dropped.
    #[test]
    fn a_queued_status_carries_its_wait_pair() {
        let mut body = vec![0u8, 0, 0, 0];
        body.extend_from_slice(&489u32.to_le_bytes());
        body.push(3);
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes());
        body.extend_from_slice(&30000u32.to_le_bytes());
        body.extend_from_slice(&5000u32.to_le_bytes());
        let s = read_battlefield_status(&mut body.as_slice()).unwrap();
        assert_eq!(s.status, 1);
        assert_eq!(s.queued, Some((30000, 5000)));
        assert!(s.time_ms.is_none() && s.in_progress.is_none());
    }
}
