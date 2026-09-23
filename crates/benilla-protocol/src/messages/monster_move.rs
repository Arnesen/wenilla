//! `SMSG_MONSTER_MOVE` — the server-dictated creature movement path — and its transport twin.
//!
//! One packet: a mover, its `start`, a spline id, a `moveType`-switched final facing, then (unless it is
//! a `Stop`) the spline block — a flag word, a duration, and the waypoints. Decodes into the full
//! travel-order polyline [`ServerPacket::MonsterMove::path`] the app rides at constant (arc-length) speed
//! (decision 0097). The wire packs the waypoints two ways, keyed by the `Flying`/`Mask_CatmullRom` flag —
//! see [`read_monster_move_spline`].
//!
//! **`SMSG_MONSTER_MOVE_TRANSPORT` is the same packet with one field inserted**: a second packed guid,
//! the transport's, immediately after the mover's — and every coordinate in the body is then an
//! **offset in that transport's frame**, not world space (decision 1936). vmangos writes exactly that
//! (`Movement::MoveSplineInit::Launch`, `spline/MoveSplineInit.cpp:138-170`: the same builder, the
//! opcode swapped and the transport packed guid appended, after `CalculatePassengerOffset` has moved
//! the start into deck space), and it computes such a path against the transport's **model** navmesh
//! rather than the map's (`PathFinder::calculate`, `Maps/PathFinder.cpp:76-84`), so the waypoints are
//! deck-local too. One reader serves both, keyed by [`read_monster_move`]'s `on_transport`.

use std::io;

use crate::messages::{MonsterMoveFacing, ServerPacket};
use crate::wire::{
    capacity_hint, packed_to_vector3d, read_f32_le, read_i32_le, read_packed_guid, read_u32_le,
    read_u64_le, read_u8, Vector3d,
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
/// `SPLINEFLAG_RUNMODE` — the path is travelled at run speed. Its **absence** is what matters:
/// the real client feeds this bit straight into `CMovement::SetRunMode 0x7c71c0`
/// (`0x7c6ac2 and edi,0x100` → `0x7c6acb call 0x7c71c0`, inside the `SMSG_MONSTER_MOVE` commit
/// `0x7c6a50`), and that setter's argument is *run* — so a spline **without** this bit **sets**
/// `MOVEFLAG_WALK_MODE` on the unit it moves. The two `0x100`s are inverses of each other
/// (wow-re `collision/scratch/walk-mode-law.md` §5.2; benilla decision 1758).
const SPLINE_FLAG_RUNMODE: u32 = 0x100;

/// Parse an `SMSG_MONSTER_MOVE` body into [`ServerPacket::MonsterMove`]. The head (mover, `start`, spline
/// id, `moveType` + its final facing) is always present; a **stop** (`moveType 1`) ends there (the client
/// reads no flags/duration/points and implies `flags=0x100, count=1, duration=0` — wow-re RF-0049;
/// reading a tail anyway over-ran the body, the `0x00dd: failed to fill whole buffer` skip that dropped
/// every creature stop). Otherwise the spline block yields the full polyline `[start, …waypoints…,
/// endpoint]`.
pub(super) fn read_monster_move(r: &mut &[u8], on_transport: bool) -> io::Result<ServerPacket> {
    let guid = read_packed_guid(r)?;
    // `SMSG_MONSTER_MOVE_TRANSPORT` only: the deck the whole body is expressed on, packed like the
    // mover's guid and read immediately after it. Everything below is unchanged — the two opcodes
    // share the builder server-side and differ by this one field.
    let transport = on_transport.then(|| read_packed_guid(r)).transpose()?;
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
            transport,
            start,
            spline_id,
            path: Vec::new(),
            facing,
            stop: true,
            duration_ms: 0,
            flying: false,
            // The stop form carries no spline-flags dword at all, so there is no run/walk verdict
            // in it — and it builds no path, so nothing downstream reads this.
            run_mode: true,
        }
    } else {
        let spline_flags = read_u32_le(r)?;
        let duration_ms = read_u32_le(r)?;
        let flying = spline_flags & SPLINE_FLAG_FLYING != 0;
        let run_mode = spline_flags & SPLINE_FLAG_RUNMODE != 0;
        // The decoded waypoints *after* the start (see [`read_monster_move_spline`]) — both layouts
        // ship only post-start points, so the wire `start` anchors the full travel-order polyline
        // `[start, …waypoints…, endpoint]`.
        let tail = read_monster_move_spline(r, flying)?;
        let path = if tail.is_empty() {
            Vec::new()
        } else {
            std::iter::once(start).chain(tail).collect()
        };
        ServerPacket::MonsterMove {
            guid,
            transport,
            start,
            spline_id,
            path,
            facing,
            stop: false,
            duration_ms,
            flying,
            run_mode,
        }
    })
}

/// `SMSG_MONSTER_MOVE` spline points → the reconstructed **absolute** waypoints the block carries, in
/// travel order (the point *after* the start … the endpoint). Two wire layouts, keyed by `catmull_rom`
/// (the `Mask_CatmullRom` = `Flying` spline flag), both from vmangos `PacketBuilder`:
///
/// - **Ground (linear, `WriteLinearPath`):** a `u32` count of the points **after** the start, then the
///   endpoint as an absolute `Vector3d`, then `count − 1` packed `i32` offsets — `endpoint − waypoint`
///   for each intermediate waypoint (see [`packed_to_vector3d`]), in travel order. We invert each
///   (`waypoint = endpoint − offset`) and append the endpoint, so the returned list is
///   `[waypoint₁, …, endpoint]`; the start is the packet head's.
///   **The start is not among the offsets.** vmangos calls the builder with `firstPoint = 1`
///   (`packet_builder.h:34`'s default, `MoveSplineInit.cpp:169`) over a spline laid out
///   `[phantom, c₀ … cₙ₋₁, cₙ₋₁]` — the Catmull-Rom initializer even for a linear path
///   (`spline.cpp:52`) — so `last_idx = n − 1`, the count is `n − 1`, and the offset loop runs
///   `c₁ … cₙ₋₂`; the head's start is `getPoint(first())` = `c₀`. The reference decoder
///   (`0x6018f0`, wow-re `net/scratch/rf49-inbound-wire-parses.md` rows 9–11) reads the same
///   `count − 1` intermediates, and its curve is `[start, points…]` (`curvemath/…/rf52-curve-construction.md`).
///   Until the sweep that found it, this read assumed `firstPoint = 0` (0097, 0708): it treated the
///   first offset as a re-encoded start and dropped it — every ground path lost its first corner —
///   and read none at `count == 2`, leaving the one real waypoint unread (the decode-length
///   instrument's `SMSG_MONSTER_MOVE … left 4 trailing byte(s)` on a live run).
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
    if catmull_rom {
        // Absolute control points, verbatim.
        let mut points = Vec::with_capacity(capacity_hint(count, 0xFFFF));
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
    let mut points = Vec::with_capacity(capacity_hint(count, 0xFFFF));
    // `count == 1` ⇒ a straight hop: the producer's `last_idx > 1` guard skips the loop, and
    // `count − 1` is already zero.
    for _ in 1..count {
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
    use crate::messages::{opcode, parse_server, parse_server_with_tail};
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

    /// The producer's linear encoder, transcribed from vmangos `PacketBuilder::WriteLinearPath` as
    /// `WriteMonsterMove` calls it (`firstPoint = 1`). `path` is the full `[c₀ = start, …, cₙ₋₁]`
    /// (the head carries `c₀` separately); over the spline's `[phantom, c₀ … cₙ₋₁, cₙ₋₁]` layout
    /// `last_idx = n − 1`, so it writes count `n − 1`, the **endpoint** absolute, then — under
    /// `if (last_idx > 1)` — `c₁ … cₙ₋₂` as packed `endpoint − point` offsets. The start is never
    /// among them.
    fn append_linear_path(body: &mut Vec<u8>, path: &[[f32; 3]]) {
        let (&endpoint, leading) = path.split_last().expect("a path has an endpoint");
        let last_idx = path.len() - 1;
        body.extend_from_slice(&(last_idx as u32).to_le_bytes()); // last_idx − start + 1
        for f in endpoint {
            body.extend_from_slice(&f.to_le_bytes());
        }
        if last_idx <= 1 {
            return; // vmangos `packet_builder.cpp:92` — `if (last_idx > 1)`
        }
        // `for (i = start; i < last_idx; ++i)` with `start = 1`: the intermediates, packed as
        // endpoint − point (¼-yd quantized).
        for &p in &leading[1..] {
            let pack = |v: f32, shift: u32, mask: i32| ((v * 4.0).round() as i32 & mask) << shift;
            let off = [endpoint[0] - p[0], endpoint[1] - p[1], endpoint[2] - p[2]];
            let packed = pack(off[0], 0, 0x7FF) | pack(off[1], 11, 0x7FF) | pack(off[2], 22, 0x3FF);
            body.extend_from_slice(&packed.to_le_bytes());
        }
    }

    /// `SMSG_MONSTER_MOVE_TRANSPORT` is `SMSG_MONSTER_MOVE` with the transport's packed guid
    /// inserted between the mover's guid and the start position — everything after it parses
    /// identically, and every coordinate is then a deck-local offset. Reading the *plain* opcode's
    /// layout for it would consume the transport guid as the start's X and Y and land the creature
    /// somewhere near the map origin; reading the transport layout for a plain packet is the same
    /// mistake in reverse. Both directions are pinned here (decision 1936).
    #[test]
    fn monster_move_transport_reads_the_deck_guid_first() {
        let mut body = Vec::new();
        write_packed_guid(0x55, &mut body).unwrap(); // the mover
        write_packed_guid(0x2000_0000_0000_0007, &mut body).unwrap(); // the transport
        for f in [1.5f32, -2.5, 0.75] {
            body.extend_from_slice(&f.to_le_bytes()); // deck-local start
        }
        body.extend_from_slice(&7u32.to_le_bytes()); // splineId
        body.push(0); // moveType: a plain move
        body.extend_from_slice(&0u32.to_le_bytes()); // spline flags: ground, walk
        body.extend_from_slice(&2_000u32.to_le_bytes()); // duration
        append_linear_path(&mut body, &[[1.5, -2.5, 0.75], [4.0, -2.5, 0.75]]);

        match parse_server(opcode::SMSG_MONSTER_MOVE_TRANSPORT, &body).expect("parses") {
            ServerPacket::MonsterMove {
                guid,
                transport,
                start,
                path,
                ..
            } => {
                assert_eq!(guid, 0x55);
                assert_eq!(transport, Some(0x2000_0000_0000_0007));
                assert_eq!((start.x, start.y, start.z), (1.5, -2.5, 0.75));
                assert_eq!(path.len(), 2, "start + endpoint, both deck-local");
                assert_eq!((path[1].x, path[1].y, path[1].z), (4.0, -2.5, 0.75));
            }
            _ => panic!("expected MonsterMove"),
        }

        // The plain opcode over the SAME bytes must NOT come back with a transport — and it
        // mis-reads the head, which is exactly why the two layouts cannot share one arm.
        match parse_server(opcode::SMSG_MONSTER_MOVE, &body) {
            Ok(ServerPacket::MonsterMove { transport, .. }) => assert_eq!(transport, None),
            Ok(_) => panic!("expected MonsterMove"),
            Err(_) => {} // an under-run is equally fine: the point is that it is not the same read
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

    /// The plain two-point hop — a `MoveTo` with no intermediate waypoints: `count = 1`, the
    /// destination, no offsets. Encoded here by the transcribed producer above.
    #[test]
    fn monster_move_two_point_path_carries_no_offsets() {
        let mut body = head(0x77, [4.0, 8.0, 0.0], 0);
        body.extend_from_slice(&0u32.to_le_bytes()); // spline flags: ground
        body.extend_from_slice(&1_000u32.to_le_bytes()); // duration
        append_linear_path(&mut body, &[[4.0, 8.0, 0.0], [12.0, 8.0, 0.0]]);
        let (p, tail) = parse_server_with_tail(opcode::SMSG_MONSTER_MOVE, &body)
            .expect("a two-point hop parses");
        assert_eq!(tail, 0, "the body is exactly the count and the destination");
        match p {
            ServerPacket::MonsterMove { path, .. } => {
                assert_eq!(path.len(), 2, "start + destination, got {path:?}");
                assert!((path[0].x - 4.0).abs() < 1e-6 && (path[1].x - 12.0).abs() < 1e-6);
            }
            _ => panic!("expected MonsterMove"),
        }
    }

    /// **The one-corner path, byte for byte as vmangos writes it** — hand-assembled, not through the
    /// transcribed encoder, so the two cannot share a misreading. A pathfinder route `c₀ → c₁ → c₂`
    /// is `count = 2`, the endpoint `c₂`, and ONE packed offset `c₂ − c₁`. The read this replaced
    /// took `count == 2` for an offset-free hop: it read nothing, cut the corner (`[c₀, c₂]`) and
    /// left four bytes unread — the live run's `SMSG_MONSTER_MOVE … left 4 trailing byte(s)`.
    #[test]
    fn monster_move_one_corner_path_keeps_its_corner() {
        let mut body = head(0x77, [0.0, 0.0, 0.0], 0); // c₀ in the head
        body.extend_from_slice(&0u32.to_le_bytes()); // spline flags: ground
        body.extend_from_slice(&2_000u32.to_le_bytes()); // duration
        body.extend_from_slice(&2u32.to_le_bytes()); // count = last_idx − start + 1 = 2
        for f in [10.0f32, 10.0, 0.0] {
            body.extend_from_slice(&f.to_le_bytes()); // endpoint c₂
        }
        // c₂ − c₁ = (0, 10, 0): x 0, y 10·4 = 40 at bit 11, z 0 (`appendPackXYZ`'s ¼-yd fields).
        body.extend_from_slice(&(40i32 << 11).to_le_bytes());
        let (p, tail) =
            parse_server_with_tail(opcode::SMSG_MONSTER_MOVE, &body).expect("a corner path parses");
        assert_eq!(tail, 0, "the one offset is read, not left behind");
        match p {
            ServerPacket::MonsterMove { path, .. } => {
                let got: Vec<[f32; 3]> = path.iter().map(|v| [v.x, v.y, v.z]).collect();
                assert_eq!(
                    got,
                    vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [10.0, 10.0, 0.0]]
                );
            }
            _ => panic!("expected MonsterMove"),
        }
    }

    #[test]
    fn monster_move_ground_path_decodes_every_waypoint() {
        // A four-waypoint ground patrol, encoded exactly as vmangos `WriteLinearPath` ships it, must
        // round-trip to the full travel-order polyline — not collapse to `start → endpoint`, and not
        // lose its first corner. Points are ¼-yd multiples so the packed quantization is exact.
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
