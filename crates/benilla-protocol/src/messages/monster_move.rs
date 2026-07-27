//! `SMSG_MONSTER_MOVE` — the server-dictated creature movement path.
//!
//! One packet: a mover, its `start`, a spline id, a `moveType`-switched final facing, then (unless it is
//! a `Stop`) the spline block — a flag word, a duration, and the waypoints. Decodes into the full
//! travel-order polyline [`ServerPacket::MonsterMove::path`] the app rides at constant (arc-length) speed
//! (decision 0097). The wire packs the waypoints two ways, keyed by the `Flying`/`Mask_CatmullRom` flag —
//! see [`read_monster_move_spline`].

use std::io;

use crate::messages::{MonsterMoveFacing, ServerPacket};
use crate::wire::{
    packed_to_vector3d, read_f32_le, read_i32_le, read_packed_guid, read_u32_le, read_u64_le,
    read_u8, Vector3d,
};

/// `MonsterMoveType::Stop` (`SMSG_MONSTER_MOVE`).
const MONSTER_MOVE_STOP: u8 = 0x1;
/// `SplineFlags::Flying` in the `SMSG_MONSTER_MOVE` spline-flags word. When set, the unit follows the
/// path's **own Z** (a 3-D flight path, points sent explicit); when clear, the path is a *ground* walk
/// and the real client **discards the spline Z and re-derives it from the terrain** under the unit (the
/// swept-collision down-probe — byte-verified in wow-5875-re: the grounded fork zeroes the spline's
/// Z-delta at `0x616cb0` and the WALK resolver `0x634040 → 0x6367b0` reads Z off the world trace
/// `0x6721b0`). benilla mirrors that split: a non-flying spline is terrain-clamped, a flying one keeps
/// its Z. The client gates on this same bit (`0x6018f0: test ah,0x2`). It is also `Mask_CatmullRom` —
/// flying paths interpolate (and wire-encode) as a curve, ground paths as straight segments.
const SPLINE_FLAG_FLYING: u32 = 0x200;

/// Parse an `SMSG_MONSTER_MOVE` body into [`ServerPacket::MonsterMove`]. The head (mover, `start`, spline
/// id, `moveType` + its final facing) is always present; a **stop** (`moveType 1`) ends there (the client
/// reads no flags/duration/points and implies `flags=0x100, count=1, duration=0` — wow-re RF-0049;
/// reading a tail anyway over-ran the body, the `0x00dd: failed to fill whole buffer` skip that dropped
/// every creature stop). Otherwise the spline block yields the full polyline `[start, …waypoints…,
/// endpoint]`.
pub(super) fn read_monster_move(r: &mut &[u8]) -> io::Result<ServerPacket> {
    let guid = read_packed_guid(r)?;
    let start = Vector3d::read(r)?;
    // The server's per-move spline counter. Discarded for a creature (nothing acks its walk), but
    // the client echoes it back in `CMSG_MOVE_SPLINE_DONE` when a spline drives its OWN player
    // (Charge/knockback/taxi): the server validates the ack against the newest spline id.
    let spline_id = read_u32_le(r)?;
    let move_type = read_u8(r)?;
    // The `moveType`-switched final facing (jumptable `0x602114`): 2 = spot, 3 = target guid, 4 = angle.
    // The real client snaps the unit to it (`0x7c6f30`); benilla applies it in the net bridge. A plain
    // move (0) / stop (1) dictates no facing.
    let facing = match move_type {
        2 => {
            let spot = Vector3d::read(r)?;
            MonsterMoveFacing::Spot([spot.x, spot.y, spot.z])
        }
        3 => MonsterMoveFacing::Target(read_u64_le(r)?),
        4 => MonsterMoveFacing::Angle(read_f32_le(r)?),
        _ => MonsterMoveFacing::None,
    };
    Ok(if move_type == MONSTER_MOVE_STOP {
        ServerPacket::MonsterMove {
            guid,
            start,
            spline_id,
            path: Vec::new(),
            facing,
            stop: true,
            duration_ms: 0,
            flying: false,
        }
    } else {
        let spline_flags = read_u32_le(r)?;
        let duration_ms = read_u32_le(r)?;
        let flying = spline_flags & SPLINE_FLAG_FLYING != 0;
        // The decoded waypoints *after* the start (see [`read_monster_move_spline`]): a ground path's
        // first packed point re-encodes `start` (quantized), so drop it and anchor the path with the
        // exact wire `start`; a flying path sends only post-start points, kept whole. The result is the
        // full travel-order polyline `[start, …waypoints…, endpoint]`.
        let tail = read_monster_move_spline(r, flying)?;
        let path = if tail.is_empty() {
            Vec::new()
        } else {
            let skip = usize::from(!flying && tail.len() >= 2);
            std::iter::once(start)
                .chain(tail.into_iter().skip(skip))
                .collect()
        };
        ServerPacket::MonsterMove {
            guid,
            start,
            spline_id,
            path,
            facing,
            stop: false,
            duration_ms,
            flying,
        }
    })
}

/// `SMSG_MONSTER_MOVE` spline points → the reconstructed **absolute** waypoints the block carries, in
/// travel order (the point *after* the start … the endpoint). Two wire layouts, keyed by `catmull_rom`
/// (the `Mask_CatmullRom` = `Flying` spline flag), both from vmangos `PacketBuilder`:
///
/// - **Ground (linear, `WriteLinearPath`):** a `u32` count, then the endpoint as an absolute `Vector3d`,
///   then `count − 1` packed `i32` offsets — `endpoint − waypoint` per point (see [`packed_to_vector3d`]),
///   in travel order. We invert each (`waypoint = endpoint − offset`) and append the endpoint, so the
///   returned list is `[waypoint₀ ≈ start, waypoint₁, …, endpoint]`. `waypoint₀` re-encodes the packet's
///   `start` (¼-yd quantized); the caller drops it in favour of the exact `start`.
///   **`count == 2` carries no offsets at all**: vmangos guards the offset loop with `last_idx > 1`
///   (`packet_builder.cpp:92`) while still writing `last_idx + 1` as the count, so a plain two-point
///   `MoveTo` announces 2 points and ships only the destination. Reading the phantom offset anyway
///   over-runs the body and the whole packet is skipped — i.e. that creature stops dead until its next
///   move, the same freeze decision 0708 is about. Never observed on this deploy (its pathfinder emits
///   3+ point paths, verified over four live runs: no `nodes=2` in any `csp`/`mmv` trace), but a map
///   without mmaps would hit it on every step.
/// - **Flying (Catmull-Rom, `WriteCatmullRomPath`):** a `u32` count, then `count` **absolute** `Vector3d`s
///   (the control points from `getPoint(2)` on — the post-start waypoints through the endpoint). Returned
///   verbatim. (Reading these as packed offsets — the old single-layout path — mis-sized the body and
///   dropped every flight move.)
///
/// The client interpolates a ground path **piecewise-linearly, arc-length-parameterised** through all of
/// these (wow-re curvemath RF-0048/RF-0052: the creature follow's point-at-t is `linear_pos_diff`, not the
/// Catmull-Rom blend — only flying units take the curved family).
fn read_monster_move_spline(r: &mut &[u8], catmull_rom: bool) -> io::Result<Vec<Vector3d>> {
    let count = read_u32_le(r)?;
    // Cap the *pre-allocation* (not the read) at a sane bound — a corrupt `count` must not reserve GBs;
    // the read itself still errors the instant the (bounded) body underruns.
    let cap = count.min(0xFFFF) as usize;
    if catmull_rom {
        // Absolute control points, verbatim.
        let mut points = Vec::with_capacity(cap);
        for _ in 0..count {
            points.push(Vector3d::read(r)?);
        }
        return Ok(points);
    }
    // Endpoint (absolute) + packed offsets from it; invert to absolute, endpoint last.
    if count == 0 {
        return Ok(Vec::new());
    }
    let endpoint = Vector3d::read(r)?;
    let mut points = Vec::with_capacity(cap);
    // `count == 2` ⇒ the producer skipped its offset loop entirely (`last_idx > 1`); the destination is
    // the whole payload, and the caller pairs it with the packet's exact `start`.
    let offsets = if count > 2 { count - 1 } else { 0 };
    for _ in 0..offsets {
        let off = packed_to_vector3d(read_i32_le(r)?);
        points.push(Vector3d {
            x: endpoint.x - off.x,
            y: endpoint.y - off.y,
            z: endpoint.z - off.z,
        });
    }
    points.push(endpoint);
    Ok(points)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{opcode, parse_server};
    use crate::wire::write_packed_guid;

    /// The fixed head of a `SMSG_MONSTER_MOVE`: packed guid, start pos, splineId, moveType.
    fn head(guid: u64, start: [f32; 3], move_type: u8) -> Vec<u8> {
        let mut b = Vec::new();
        write_packed_guid(guid, &mut b).unwrap();
        for f in start {
            b.extend_from_slice(&f.to_le_bytes());
        }
        b.extend_from_slice(&7u32.to_le_bytes()); // splineId
        b.push(move_type);
        b
    }

    /// The producer's linear encoder, transcribed from vmangos `PacketBuilder::WriteLinearPath`:
    /// count = number of points, the **endpoint** written absolute, then each earlier point as a packed
    /// `endpoint − point` offset in travel order. `path` is the full `[start, …mids…, endpoint]`.
    /// The `last_idx > 1` guard is transcribed too: a **two**-point path writes the count and the
    /// destination and stops — no offsets at all, not even for the start.
    fn append_linear_path(body: &mut Vec<u8>, path: &[[f32; 3]]) {
        let (&endpoint, leading) = path.split_last().expect("a path has an endpoint");
        body.extend_from_slice(&(path.len() as u32).to_le_bytes());
        for f in endpoint {
            body.extend_from_slice(&f.to_le_bytes());
        }
        if path.len() <= 2 {
            return; // vmangos `packet_builder.cpp:92` — `if (last_idx > 1)`
        }
        // `leading` = [start, …mids…]; each packed as endpoint − point (¼-yd quantized).
        for &p in leading {
            let pack = |v: f32, shift: u32, mask: i32| ((v * 4.0).round() as i32 & mask) << shift;
            let off = [endpoint[0] - p[0], endpoint[1] - p[1], endpoint[2] - p[2]];
            let packed = pack(off[0], 0, 0x7FF) | pack(off[1], 11, 0x7FF) | pack(off[2], 22, 0x3FF);
            body.extend_from_slice(&packed.to_le_bytes());
        }
    }

    #[test]
    fn monster_move_stop_has_no_tail() {
        // A STOP (moveType 1) is head-only: no flags/duration/points. Parsing the tail anyway
        // over-ran the body and skipped the packet (`0x00dd: failed to fill whole buffer`).
        let body = head(0x1234, [1.0, 2.0, 3.0], MONSTER_MOVE_STOP);
        let p = parse_server(opcode::SMSG_MONSTER_MOVE, &body).expect("a stop parses head-only");
        match p {
            ServerPacket::MonsterMove { stop, path, .. } => {
                assert!(stop, "moveType 1 is a stop");
                assert!(path.is_empty(), "a stop carries no path");
            }
            _ => panic!("expected MonsterMove"),
        }
    }

    #[test]
    fn monster_move_facing_angle_is_captured() {
        // A facing-angle move (moveType 4): head, angle, then flags/duration/count + one point.
        let mut body = head(0x55, [0.0, 0.0, 0.0], 4);
        body.extend_from_slice(&1.25f32.to_le_bytes()); // facing angle
        body.extend_from_slice(&0u32.to_le_bytes()); // spline flags (ground)
        body.extend_from_slice(&500u32.to_le_bytes()); // duration
        body.extend_from_slice(&1u32.to_le_bytes()); // count = 1 → endpoint only, no packed
        for f in [10.0f32, 0.0, 0.0] {
            body.extend_from_slice(&f.to_le_bytes()); // the single (absolute) endpoint
        }
        let p = parse_server(opcode::SMSG_MONSTER_MOVE, &body).expect("a facing move parses");
        match p {
            ServerPacket::MonsterMove {
                facing,
                path,
                duration_ms,
                stop,
                ..
            } => {
                assert!(!stop);
                assert_eq!(duration_ms, 500);
                // A count-1 (endpoint-only) move is the two-point straight hop `[start, endpoint]`.
                assert_eq!(path.len(), 2, "start + endpoint");
                assert!((path[0].x - 0.0).abs() < 1e-6, "anchored at the wire start");
                assert!((path[1].x - 10.0).abs() < 1e-6, "endpoint verbatim");
                match facing {
                    MonsterMoveFacing::Angle(a) => assert!((a - 1.25).abs() < 1e-6),
                    other => panic!("expected an Angle facing, got {other:?}"),
                }
            }
            _ => panic!("expected MonsterMove"),
        }
    }

    /// The plain two-point hop — a `MoveTo` with no intermediate waypoints. vmangos announces `count = 2`
    /// but ships **only** the destination (its offset loop is guarded by `last_idx > 1`), so a decoder
    /// that trusts the count and reads `count − 1` offsets over-runs the body and the packet is skipped
    /// entirely — the creature freezes where it stands. Encoded here by the transcribed producer above.
    #[test]
    fn monster_move_two_point_path_carries_no_offsets() {
        let mut body = head(0x77, [4.0, 8.0, 0.0], 0);
        body.extend_from_slice(&0u32.to_le_bytes()); // spline flags: ground
        body.extend_from_slice(&1_000u32.to_le_bytes()); // duration
        append_linear_path(&mut body, &[[4.0, 8.0, 0.0], [12.0, 8.0, 0.0]]);
        let p = parse_server(opcode::SMSG_MONSTER_MOVE, &body).expect("a two-point hop parses");
        match p {
            ServerPacket::MonsterMove { path, .. } => {
                assert_eq!(path.len(), 2, "start + destination, got {path:?}");
                assert!((path[0].x - 4.0).abs() < 1e-6 && (path[1].x - 12.0).abs() < 1e-6);
            }
            _ => panic!("expected MonsterMove"),
        }
    }

    #[test]
    fn monster_move_ground_path_decodes_every_waypoint() {
        // A four-waypoint ground patrol, encoded exactly as vmangos `WriteLinearPath` ships it, must
        // round-trip to the full travel-order polyline — not collapse to `start → endpoint`. Points are
        // ¼-yd multiples so the packed quantization is exact. The start is anchored from the wire `start`
        // (the redundant quantized first packed point is dropped).
        let want = [
            [0.0f32, 0.0, 0.0], // start
            [10.0, 0.0, 0.0],   // corner east
            [10.0, 10.0, 0.0],  // corner north
            [10.0, 10.0, 5.0],  // endpoint, up a step
        ];
        let mut body = head(0xABCD, want[0], 0); // moveType 0 (normal)
        body.extend_from_slice(&0u32.to_le_bytes()); // spline flags (ground/linear)
        body.extend_from_slice(&4000u32.to_le_bytes()); // duration
        append_linear_path(&mut body, &want);
        let p = parse_server(opcode::SMSG_MONSTER_MOVE, &body).expect("a ground path parses");
        match p {
            ServerPacket::MonsterMove {
                path, flying, stop, ..
            } => {
                assert!(!stop && !flying, "a normal ground move");
                assert_eq!(
                    path.len(),
                    4,
                    "all four waypoints survive, not just the endpoint"
                );
                for (got, exp) in path.iter().zip(want.iter()) {
                    assert!(
                        (got.x - exp[0]).abs() < 1e-4
                            && (got.y - exp[1]).abs() < 1e-4
                            && (got.z - exp[2]).abs() < 1e-4,
                        "waypoint {got:?} != {exp:?}"
                    );
                }
            }
            _ => panic!("expected MonsterMove"),
        }
    }

    #[test]
    fn monster_move_flying_path_reads_absolute_points() {
        // A flying (`Mask_CatmullRom`) move ships its waypoints ABSOLUTE (vmangos `WriteCatmullRomPath`),
        // not as packed offsets. The parser must switch layouts on the flag — the old single-layout read
        // mis-sized the body and dropped every flight. `count` = post-start points; the start prepends.
        let mids = [[3.0f32, 4.0, 50.0], [6.0, 8.0, 55.0]];
        let mut body = head(0x77, [0.0, 0.0, 40.0], 0);
        body.extend_from_slice(&SPLINE_FLAG_FLYING.to_le_bytes()); // flying ⇒ catmull-rom layout
        body.extend_from_slice(&3000u32.to_le_bytes()); // duration
        body.extend_from_slice(&(mids.len() as u32).to_le_bytes()); // count = absolute points
        for pt in mids {
            for f in pt {
                body.extend_from_slice(&f.to_le_bytes());
            }
        }
        let p = parse_server(opcode::SMSG_MONSTER_MOVE, &body).expect("a flying path parses");
        match p {
            ServerPacket::MonsterMove { path, flying, .. } => {
                assert!(flying, "the FLYING flag drives the catmull-rom layout");
                assert_eq!(path.len(), 3, "start + two absolute waypoints");
                assert!(
                    (path[0].z - 40.0).abs() < 1e-6,
                    "anchored at the wire start (Z kept — flying)"
                );
                assert!((path[2].x - 6.0).abs() < 1e-6 && (path[2].z - 55.0).abs() < 1e-6);
            }
            _ => panic!("expected MonsterMove"),
        }
    }
}
