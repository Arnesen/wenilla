//! What the ADT liquid actually **is** under a pin — the shoreline instrument:
//! `cargo run -p benilla-formats --example water_here -- <map> <x> <y> [radius_yd]`
//! e.g. `water_here Azeroth -12512.69 -180.21 40`.
//!
//! A water-at-the-shore report ("there's a step in it", "the foam runs past the waterline") is a
//! question about three surfaces that only ever agree in the middle of a lake: the MCLQ **wet-cell
//! lattice** (4.167-yd cells, the granularity everything on the water path clips to), the liquid
//! **surface height** on those cells, and the **terrain** underneath. This prints all three over one
//! neighbourhood so the geometry can be read instead of guessed:
//!
//! - the **block census** — one line per MCLQ block: kind, wet cells, surface-height span, depth-byte
//!   span. Where a step would be *authored*, this is where it shows: two blocks over one chunk, or a
//!   block whose own plane has relief;
//! - the **overlap** scan — cells claimed by two blocks at once, which would composite the
//!   translucent water twice;
//! - the **seam** scan — every pair of adjacent wet cells whose planes disagree, so an authored step
//!   can be told apart from a rendered one before anyone opens the renderer;
//! - the **lattice-vs-waterline** scan — dry cells the ground still runs under. Whole wet cells are
//!   the only thing the renderer draws, so wherever the lattice runs out before the ground climbs
//!   through the plane, the water ends on a 4.167-yd straight edge in open water: a shape no
//!   shoreline has;
//! - the **shoreline slope** at the pin, and what a coplanarity *lift* costs there. A decal held `l`
//!   yards above the water plane keeps painting for `l / slope` yards past the waterline, on dry
//!   ground — the arithmetic behind B348's foam-on-the-sand, and the reason that settle is a depth
//!   bias now and not a lift.
//!
//! Output is Blizzard data — pipe it to the scratchpad, never into the repo.

use std::collections::BTreeMap;

use benilla_formats::{terrain_height_at, ChunkMesh, CHUNK_SIZE};

/// One MCLQ cell edge, in yards — the granularity of `wet`, and of every clip on the water path.
const CELL: f32 = CHUNK_SIZE / 8.0;
/// How far past the reported radius the block census still enumerates, so that a cell at the edge
/// of the answer has its real neighbours to be judged against (one chunk clears it).
const MARGIN: f32 = CHUNK_SIZE;

/// One seam between two adjacent wet cells whose surface planes disagree.
struct Seam {
    /// `to.surface − from.surface`, in yards.
    dz: f32,
    from: (i32, i32),
    to: (i32, i32),
}

/// One MCLQ cell of one block, resolved into world terms.
struct Cell {
    /// Cell-centre world XY.
    x: f32,
    y: f32,
    /// The block's surface height over the cell (bilinear at the centre, as the queries sample it).
    surface: f32,
    /// Terrain height at the same point, `None` where the tile has a hole.
    ground: Option<f32>,
    /// Which chunk/block this cell came from — `(chunk index, block index)`.
    owner: (usize, usize),
    kind: benilla_formats::LiquidKind,
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let usage = || anyhow::anyhow!("usage: water_here <map> <x> <y> [radius_yd]");
    let map = args.next().ok_or_else(usage)?;
    let x: f32 = args.next().ok_or_else(usage)?.parse()?;
    let y: f32 = args.next().ok_or_else(usage)?.parse()?;
    let radius: f32 = args.next().map_or(Ok(30.0), |r| r.parse())?;

    let data = benilla_formats::wow_data().expect("no WoW install found (set $WOW_DATA)");
    let mut chain = benilla_formats::open_chain(&data)?;
    // One tile of slack: a shoreline pin sits on a tile seam as often as not.
    let tiles = benilla_formats::load_tiles_around(&mut chain, &map, x, y, 1)?;
    let chunks: Vec<ChunkMesh> = tiles.into_iter().flat_map(|(_, t)| t.chunks).collect();
    println!(
        "{map}: pin ({x}, {y}) r={radius} yd — {} chunks loaded",
        chunks.len()
    );

    // ── The block census, and the cells ──────────────────────────────────────────────────────
    let mut cells: Vec<Cell> = Vec::new();
    for (ci, chunk) in chunks.iter().enumerate() {
        let Some(nw) = chunk.positions.first() else {
            continue;
        };
        // The chunk's own footprint against the pin box (chunk spans south/east of its NW corner),
        // widened by MARGIN so every cell inside the radius has its real neighbours loaded — a cell
        // at the box edge would otherwise read as "the lattice stops here" when it is only the
        // enumeration that stopped.
        if nw[0] - CHUNK_SIZE > x + radius + MARGIN
            || nw[0] < x - radius - MARGIN
            || nw[1] - CHUNK_SIZE > y + radius + MARGIN
            || nw[1] < y - radius - MARGIN
        {
            continue;
        }
        for (bi, liq) in chunk.liquids.iter().enumerate() {
            let [cols, rows] = liq.grid;
            let (cols, rows) = (cols as usize, rows as usize);
            let wet_n = liq.wet.iter().filter(|w| **w).count();
            let (mut zlo, mut zhi) = (f32::MAX, f32::MIN);
            let (mut dlo, mut dhi) = (f32::MAX, f32::MIN);
            for (n, p) in liq.positions.iter().enumerate() {
                let cell_touched = touching_cells(n, cols, rows)
                    .into_iter()
                    .any(|c| liq.wet.get(c).copied().unwrap_or(false));
                if !cell_touched {
                    continue;
                }
                zlo = zlo.min(p[2]);
                zhi = zhi.max(p[2]);
                dlo = dlo.min(liq.depths[n]);
                dhi = dhi.max(liq.depths[n]);
            }
            println!(
                "  chunk[{ci:3}] {:>2}_{:<2} block[{bi}] {:?} nibble {} — {wet_n:2}/{} wet, \
                 z {zlo:.3}..{zhi:.3} (span {:.3}), depthV {dlo:.3}..{dhi:.3}",
                chunk.index_x,
                chunk.index_y,
                liq.kind,
                liq.sound_nibble,
                (cols - 1) * (rows - 1),
                zhi - zlo,
            );

            for j in 0..rows - 1 {
                for i in 0..cols - 1 {
                    if !liq.wet[j * (cols - 1) + i] {
                        continue;
                    }
                    let corners = [
                        liq.positions[j * cols + i],
                        liq.positions[j * cols + i + 1],
                        liq.positions[(j + 1) * cols + i],
                        liq.positions[(j + 1) * cols + i + 1],
                    ];
                    let cx = corners.iter().map(|p| p[0]).sum::<f32>() / 4.0;
                    let cy = corners.iter().map(|p| p[1]).sum::<f32>() / 4.0;
                    let surface = corners.iter().map(|p| p[2]).sum::<f32>() / 4.0;
                    cells.push(Cell {
                        x: cx,
                        y: cy,
                        surface,
                        ground: terrain_height_at(&chunks, [cx, cy, 0.0]),
                        owner: (ci, bi),
                        kind: liq.kind,
                    });
                }
            }
        }
    }
    if cells.is_empty() {
        println!("  (no MCLQ liquid within the box)");
        return Ok(());
    }

    // ── Overlap: two blocks claiming one patch of world, at two heights ──────────────────────
    // Key a cell by its lattice index, floored — two DIFFERENT cells of one block must never
    // collide, or the overlap report cries wolf on its own rounding.
    let key = |c: &Cell| ((c.x / CELL).floor() as i32, (c.y / CELL).floor() as i32);
    let mut by_key: BTreeMap<(i32, i32), Vec<usize>> = BTreeMap::new();
    for (n, c) in cells.iter().enumerate() {
        by_key.entry(key(c)).or_default().push(n);
    }
    let mut overlaps = 0;
    for (k, ns) in &by_key {
        if ns.len() < 2 {
            continue;
        }
        overlaps += 1;
        if overlaps <= 12 {
            let list: Vec<String> = ns
                .iter()
                .map(|&n| {
                    format!(
                        "chunk[{}]blk[{}] {:?} z {:.3}",
                        cells[n].owner.0, cells[n].owner.1, cells[n].kind, cells[n].surface
                    )
                })
                .collect();
            println!("  OVERLAP at cell {k:?}: {}", list.join("  |  "));
        }
    }
    println!("  overlapping cells: {overlaps}");

    // ── The seam scan: adjacent wet cells whose planes disagree ──────────────────────────────
    let mut steps: Vec<Seam> = Vec::new();
    for (k, ns) in &by_key {
        let z = cells[ns[0]].surface;
        for nb in [(k.0 + 1, k.1), (k.0, k.1 + 1)] {
            if let Some(other) = by_key.get(&nb) {
                let dz = cells[other[0]].surface - z;
                if dz.abs() > 0.01 {
                    steps.push(Seam {
                        dz,
                        from: *k,
                        to: nb,
                    });
                }
            }
        }
    }
    steps.sort_by(|a, b| b.dz.abs().partial_cmp(&a.dz.abs()).expect("finite"));
    println!(
        "  wet-cell seams with a height step > 0.01 yd: {} (largest first)",
        steps.len()
    );
    for s in steps.iter().take(10) {
        println!("    {:?} -> {:?}  dz {:+.3} yd", s.from, s.to, s.dz);
    }

    // ── Where the LATTICE stops vs where the WATERLINE is ────────────────────────────────────
    // The renderer draws whole wet cells and nothing else, so the water's visible boundary is the
    // *lattice* edge wherever the lattice runs out before the ground climbs through the plane. That
    // edge is a 4.167-yd straight line in open water — the shape a shoreline never has. Sample every
    // dry cell that touches a wet one on a 0.25-yd grid and report how deep the water would have
    // been there.
    let mut short_cells: Vec<(f32, (i32, i32))> = Vec::new();
    for k in by_key.keys().copied().collect::<Vec<_>>() {
        for nb in [
            (k.0 + 1, k.1),
            (k.0 - 1, k.1),
            (k.0, k.1 + 1),
            (k.0, k.1 - 1),
        ] {
            if by_key.contains_key(&nb) {
                continue;
            }
            let surface = cells[by_key[&k][0]].surface;
            let (bx, by) = ((nb.0 as f32 + 0.5) * CELL, (nb.1 as f32 + 0.5) * CELL);
            let mut deepest = f32::MIN;
            for si in 0..=16 {
                for sj in 0..=16 {
                    let px = bx - CELL / 2.0 + CELL * si as f32 / 16.0;
                    let py = by - CELL / 2.0 + CELL * sj as f32 / 16.0;
                    if let Some(g) = terrain_height_at(&chunks, [px, py, 0.0]) {
                        deepest = deepest.max(surface - g);
                    }
                }
            }
            if deepest > 0.02 && (bx - x).hypot(by - y) <= radius {
                short_cells.push((deepest, nb));
            }
        }
    }
    short_cells.sort_by(|a, b| b.0.partial_cmp(&a.0).expect("finite"));
    short_cells.dedup_by_key(|(_, k)| *k);
    println!(
        "\n  DRY cells the ground still runs under (the lattice stops before the waterline does): {}",
        short_cells.len()
    );
    for (d, k) in short_cells.iter().take(10) {
        println!(
            "    cell {k:?} at ({:.1}, {:.1}) — up to {d:.3} yd of water missing",
            (k.0 as f32 + 0.5) * CELL,
            (k.1 as f32 + 0.5) * CELL
        );
    }

    // ── The waterline's slope, and what a coplanarity lift costs horizontally ────────────────
    // A decal on the water plane, lifted `l` yards to win the coplanar tie, keeps painting for
    // `l / slope` yards past the waterline — up the beach, on dry ground. Measure the slope where
    // the plane actually meets the ground nearest the pin.
    let mut best: Option<(f32, f32, f32, f32)> = None; // (dist to pin, x, y, slope)
    for c in &cells {
        let Some(g) = c.ground else { continue };
        if (c.surface - g).abs() > 0.5 {
            continue;
        }
        let h = 1.0_f32;
        let sample = |dx: f32, dy: f32| terrain_height_at(&chunks, [c.x + dx, c.y + dy, 0.0]);
        let (Some(px), Some(mx), Some(py), Some(my)) = (
            sample(h, 0.0),
            sample(-h, 0.0),
            sample(0.0, h),
            sample(0.0, -h),
        ) else {
            continue;
        };
        let slope = (((px - mx) / (2.0 * h)).powi(2) + ((py - my) / (2.0 * h)).powi(2)).sqrt();
        let d = (c.x - x).hypot(c.y - y);
        if best.as_ref().is_none_or(|b| d < b.0) {
            best = Some((d, c.x, c.y, slope));
        }
    }
    if let Some((d, wx, wy, slope)) = best {
        println!(
            "\n  waterline nearest the pin: ({wx:.2}, {wy:.2}), {d:.1} yd away — ground slope {slope:.4} \
             ({:.1}%)",
            slope * 100.0
        );
        for lift in [0.03_f32, 0.01, 0.0] {
            println!(
                "    a {lift:.2}-yd lift paints {:.2} yd of dry beach",
                if slope > 0.0 {
                    lift / slope
                } else {
                    f32::INFINITY
                }
            );
        }
    }

    Ok(())
}

/// The (up to four) cell indices a grid vertex `n` is a corner of, row-major over `(cols−1)` cells.
fn touching_cells(n: usize, cols: usize, rows: usize) -> Vec<usize> {
    let (i, j) = (n % cols, n / cols);
    let mut out = Vec::new();
    for dj in [0isize, -1] {
        for di in [0isize, -1] {
            let (ci, cj) = (i as isize + di, j as isize + dj);
            if ci >= 0 && cj >= 0 && (ci as usize) < cols - 1 && (cj as usize) < rows - 1 {
                out.push(cj as usize * (cols - 1) + ci as usize);
            }
        }
    }
    out
}
