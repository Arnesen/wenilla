//! WDT (map tile table) reader + tile↔world coords for **WoW 1.12.1 (build 5875)** — in-repo,
//! replacing `wow-wdt` (decision 0021).
//!
//! A vanilla `.wdt` is a tiny chunked file (`MVER`, `MPHD`, then `MAIN`); the only part the client
//! streamer needs is **`MAIN`** — a 64×64 table of 8-byte entries whose flag bit 0 (`0x1`) marks
//! "this tile has an `.adt`". We read that grid and ignore the rest. The coord helpers map a world
//! `(x, y)` to its `(tile_x, tile_y)` and back, on the 64-tile, 533⅓-yd grid.
//!
//! Proven against `wow-wdt` over a real map's WDT during the decision-0021 migration (oracle test in
//! git history); the `benilla-formats` terrain tests pin tile selection + world coords end-to-end.

use std::io::{Read, Seek, SeekFrom};

/// WoW client generation. We only target Classic (1.12.1); kept as an enum so the reader signature
/// matches the call sites.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WowVersion {
    Classic,
}

/// Re-export so `benilla_wdt::version::WowVersion` resolves (mirrors the original crate's module path).
pub mod version {
    pub use super::WowVersion;
}

/// Tiles per map edge (64×64).
const MAP_SIZE: usize = 64;
/// Yards per ADT tile (1/64 of the map edge).
const TILE_YARDS: f32 = 533.333_3;

/// World `(x, y)` → ADT tile `(tile_x, tile_y)`. WoW's axes are swapped vs the tile grid: `tile_x`
/// comes from world **y**, `tile_y` from world **x**. Clamped to `0..=63`.
pub fn world_to_tile(world_x: f32, world_y: f32) -> (u32, u32) {
    let offset = 32.0 * TILE_YARDS;
    let tile_x = ((offset - world_y) / TILE_YARDS) as u32;
    let tile_y = ((offset - world_x) / TILE_YARDS) as u32;
    (tile_x.min(63), tile_y.min(63))
}

/// ADT tile `(tile_x, tile_y)` → the world `(x, y)` of its max-corner origin (inverse of
/// [`world_to_tile`]).
pub fn tile_to_world(tile_x: u32, tile_y: u32) -> (f32, f32) {
    let offset = 32.0 * TILE_YARDS;
    (
        offset - tile_y as f32 * TILE_YARDS,
        offset - tile_x as f32 * TILE_YARDS,
    )
}

/// Info about one map tile (the subset the streamer reads).
#[derive(Debug, Clone, Copy)]
pub struct TileInfo {
    pub x: usize,
    pub y: usize,
    /// Whether this tile has an `.adt` (MAIN flag bit 0).
    pub has_adt: bool,
}

/// A parsed WDT: the 64×64 tile-existence grid.
#[derive(Debug)]
pub struct WdtFile {
    /// `has_adt` per tile, indexed `y * 64 + x` (the on-disk MAIN order).
    has_adt: Vec<bool>,
}

impl WdtFile {
    /// Tile info at `(x, y)`, or `None` out of range.
    pub fn get_tile(&self, x: usize, y: usize) -> Option<TileInfo> {
        if x >= MAP_SIZE || y >= MAP_SIZE {
            return None;
        }
        Some(TileInfo {
            x,
            y,
            has_adt: self.has_adt[y * MAP_SIZE + x],
        })
    }
}

/// Reads a [`WdtFile`] from a chunked WDT stream.
pub struct WdtReader<R> {
    reader: R,
    _version: WowVersion,
}

impl<R: Read + Seek> WdtReader<R> {
    pub fn new(reader: R, version: WowVersion) -> Self {
        Self {
            reader,
            _version: version,
        }
    }

    /// Parse the WDT, returning the tile grid. Walks chunks until EOF, reading only `MAIN`.
    pub fn read(&mut self) -> std::io::Result<WdtFile> {
        let mut has_adt: Option<Vec<bool>> = None;
        loop {
            let mut hdr = [0u8; 8];
            if !read_full(&mut self.reader, &mut hdr)? {
                break; // clean EOF
            }
            // Chunk magics are stored reversed on disk; MAIN ⇒ "NIAM".
            let size = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as u64;
            if &hdr[0..4] == b"NIAM" {
                // 64×64 entries × 8 bytes (flags u32, area_id u32); bit 0 of flags = has_adt.
                let count = MAP_SIZE * MAP_SIZE;
                let mut buf = vec![0u8; count * 8];
                self.reader.read_exact(&mut buf)?;
                let grid = (0..count)
                    .map(|i| {
                        let flags = u32::from_le_bytes([
                            buf[i * 8],
                            buf[i * 8 + 1],
                            buf[i * 8 + 2],
                            buf[i * 8 + 3],
                        ]);
                        flags & 0x1 != 0
                    })
                    .collect();
                has_adt = Some(grid);
            } else {
                self.reader.seek(SeekFrom::Current(size as i64))?;
            }
        }
        let has_adt = has_adt.ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "WDT missing MAIN chunk")
        })?;
        Ok(WdtFile { has_adt })
    }
}

/// Read exactly `buf.len()` bytes; `Ok(false)` on a clean EOF before any byte, `Ok(true)` on success.
fn read_full<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => {
                return if filled == 0 {
                    Ok(false)
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "truncated WDT chunk header",
                    ))
                }
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // Mirror of the crate-private grid constants, so the tests read the geometry independently of
    // the values the code under test happens to use.
    const T: f32 = 533.333_3;
    const OFFSET: f32 = 32.0 * T;

    /// Build a minimal on-disk WDT: an ignored `MVER`, an ignored `MPHD`, then a `MAIN` whose flag
    /// bit 0 is set exactly for the `set_tiles` `(x, y)` list. A trailing unknown chunk exercises
    /// the seek-past-unknown path *after* `MAIN` too.
    fn synth_wdt(set_tiles: &[(usize, usize)], trailing: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let chunk = |magic: &[u8; 4], payload: &[u8], out: &mut Vec<u8>| {
            out.extend_from_slice(magic);
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(payload);
        };
        // MVER (magic reversed on disk) — version 18, ignored by the reader.
        chunk(b"REVM", &18u32.to_le_bytes(), &mut out);
        // MPHD — 32 bytes of header we don't read.
        chunk(b"DHPM", &[0u8; 32], &mut out);
        // MAIN — 64×64 × 8 bytes; set bit 0 of `flags` for the requested tiles.
        let count = MAP_SIZE * MAP_SIZE;
        let mut main = vec![0u8; count * 8];
        for &(x, y) in set_tiles {
            let flags_off = (y * MAP_SIZE + x) * 8;
            main[flags_off] = 0x1; // low byte of the u32 flags carries bit 0
        }
        chunk(b"NIAM", &main, &mut out);
        if trailing {
            chunk(b"FOOB", &[0xAB; 16], &mut out);
        }
        out
    }

    fn parse(bytes: Vec<u8>) -> std::io::Result<WdtFile> {
        WdtReader::new(Cursor::new(bytes), WowVersion::Classic).read()
    }

    #[test]
    fn world_origin_is_map_center_tile() {
        // World (0, 0) sits at the 32,32 tile boundary — the documented map center.
        assert_eq!(world_to_tile(0.0, 0.0), (32, 32));
    }

    #[test]
    fn tile_center_round_trips_exactly() {
        // Sample the *center* of each tile (corner minus half a tile on both axes); a half-tile
        // margin absorbs f32 rounding so truncation can't spill into a neighbour.
        for &(tx, ty) in &[(0, 0), (1, 10), (20, 31), (33, 33), (50, 62), (63, 63)] {
            let (cx, cy) = tile_to_world(tx, ty);
            let center = (cx - 0.5 * T, cy - 0.5 * T);
            assert_eq!(
                world_to_tile(center.0, center.1),
                (tx, ty),
                "tile ({tx},{ty}) center did not round-trip"
            );
        }
    }

    #[test]
    fn tile_to_world_matches_grid_formula() {
        // Max-corner origin: world_x from tile_y, world_y from tile_x (the axis swap).
        assert_eq!(tile_to_world(0, 0), (OFFSET, OFFSET));
        let (wx, wy) = tile_to_world(1, 2);
        assert!((wx - (OFFSET - 2.0 * T)).abs() < 0.01);
        assert!((wy - (OFFSET - 1.0 * T)).abs() < 0.01);
    }

    #[test]
    fn world_to_tile_clamps_both_extremes() {
        // Far past the max corner saturates low; far past the min corner clamps to the last tile.
        assert_eq!(world_to_tile(1.0e9, 1.0e9), (0, 0));
        assert_eq!(world_to_tile(-1.0e9, -1.0e9), (63, 63));
    }

    #[test]
    fn reads_main_grid_and_flags() {
        let wdt = parse(synth_wdt(&[(3, 5), (63, 0), (0, 63)], false)).unwrap();
        assert!(wdt.get_tile(3, 5).unwrap().has_adt);
        assert!(wdt.get_tile(63, 0).unwrap().has_adt);
        assert!(wdt.get_tile(0, 63).unwrap().has_adt);
        // An untouched tile reads empty, and the returned coords echo the query.
        let empty = wdt.get_tile(10, 10).unwrap();
        assert!(!empty.has_adt);
        assert_eq!((empty.x, empty.y), (10, 10));
    }

    #[test]
    fn skips_unknown_chunks_before_and_after_main() {
        // MVER/MPHD precede MAIN and a bogus chunk follows it; all must be seek-skipped cleanly.
        let wdt = parse(synth_wdt(&[(1, 1)], true)).unwrap();
        assert!(wdt.get_tile(1, 1).unwrap().has_adt);
    }

    #[test]
    fn get_tile_out_of_range_is_none() {
        let wdt = parse(synth_wdt(&[], false)).unwrap();
        assert!(wdt.get_tile(64, 0).is_none());
        assert!(wdt.get_tile(0, 64).is_none());
        assert!(wdt.get_tile(usize::MAX, usize::MAX).is_none());
    }

    #[test]
    fn missing_main_chunk_is_an_error() {
        // Only MVER present — no MAIN ⇒ InvalidData.
        let mut only_mver = Vec::new();
        only_mver.extend_from_slice(b"REVM");
        only_mver.extend_from_slice(&4u32.to_le_bytes());
        only_mver.extend_from_slice(&18u32.to_le_bytes());
        let err = parse(only_mver).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn empty_stream_has_no_main() {
        // A clean EOF at the very first header is not a truncation error, but still no MAIN.
        let err = parse(Vec::new()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn truncated_chunk_header_is_unexpected_eof() {
        // A stray 3-byte tail cannot form an 8-byte chunk header.
        let err = parse(vec![b'N', b'I', b'A']).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }
}
