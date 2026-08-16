//! The **column index**: which of a face set's triangles can own a vertical (x, y) column.
//!
//! Every position question this client asks a WMO is a column query — which room is the camera in
//! (`wmo_portal::seed`), which room is a unit in (`track_unit_interiors`), which floor's baked MOCV
//! lights an entity (`interior::classify_entity_interior`), what the zone text should say. All of
//! them cast straight down and want "the triangles whose XY projection contains this point".
//!
//! Until now the narrow phase for that was **a linear scan of every triangle in every group whose
//! AABB contains the column** (decisions 0330/0364 built the per-group bounds; the group is where
//! the culling stopped). That is fine for a cottage and catastrophic for a dungeon: Blackrock
//! Spire's groups hold ~11–16k faces each, and a vertically stacked spire puts several of them in
//! any given column. Measured in LBRS on 2026-07-27: **~1.2 M triangle tests per frame** to light
//! **37 moving NPCs** — ~32k per unit per frame, 11 ms of a 29 ms frame, the second-largest term in
//! the NPC-population collapse (B31/B06 + the BWL/LBRS reports).
//!
//! So each group's faces get a uniform XY grid, built once at load: a column tests one cell's
//! worth of triangles instead of the group's whole face list.
//!
//! **The index never changes a verdict.** It is a pure accelerator with two properties the callers
//! depend on, both pinned by tests:
//!
//! - **Superset** — every triangle whose XY projection contains the column is a candidate. A
//!   triangle is inserted into every cell its XY AABB touches (inclusive both ends), a query point
//!   outside the grid clamps into the edge cell rather than missing, and a triangle whose AABB
//!   spans more cells than [`MAX_SPAN_CELLS`] goes in [`ColumnGrid::spanning`], tested always.
//! - **Order** — candidates come back in **ascending triangle index**, the order a linear scan
//!   visits them, so every tie-break downstream (`down_ray_claim`'s first-wins, `footprint_sample`'s
//!   later-wins) resolves exactly as it did before the index existed.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// A triangle whose XY AABB touches more cells than this is not binned at the level being built —
/// it is handed to the next level down (the coarse grid), and only a face too large for *that*
/// becomes a [`ColumnGrid::spanning`] always-tested residue. Big floor slabs are the case: at a
/// ~1-triangle-per-cell sizing a 100-yd slab would otherwise be copied into hundreds of cells.
const MAX_SPAN_CELLS: u32 = 32;

/// Below this many oversized faces, a second grid is not worth its own allocation pair and lookup —
/// they stay an always-tested list. Well under the per-query counts 1351 measured (~480), so the
/// pin's groups all take the coarse level.
const MIN_COARSE_TRIS: usize = 24;

/// Never build cells finer than this (yd) — a degenerate face set (all triangles stacked in one
/// spot) would otherwise ask for an unbounded grid.
const MIN_CELL: f32 = 0.25;

/// Cell budget per group: the grid is at most this many cells regardless of face count, so a
/// 500-yd dungeon group costs tens of KB of index, not tens of MB.
const MAX_CELLS: usize = 1 << 16;

/// One uniform XY grid over some set of triangles — CSR (`starts`/`items`), so one allocation
/// pair per level rather than a `Vec` per cell.
#[derive(Debug, Clone)]
struct Level {
    /// Grid origin (the indexed set's XY minimum).
    min: [f32; 2],
    /// 1 / cell size, in cells per yard.
    inv_cell: f32,
    nx: u32,
    ny: u32,
    /// CSR row offsets into [`Self::items`], length `nx * ny + 1`.
    starts: Vec<u32>,
    /// **Global** triangle indices, ascending within each cell.
    items: Vec<u32>,
}

/// A face set's column index: **two grids and a residue**.
///
/// One grid cannot serve a dungeon group. Sizing cells at ~1 triangle each is right for the ~11–16k
/// small faces, and catastrophic for the handful of 40–100 yd floor slabs sharing the group: a slab
/// would be copied into hundreds of cells, so [`MAX_SPAN_CELLS`] sets it aside instead — and
/// "aside" used to mean *tested on every query*.
///
/// 1351 measured what that cost at the LBRS pin: **99.26 %** of all triangles tested per frame were
/// those set-aside slabs — 93,624 of 94,322, against 698 that came from a cell list. The index was
/// accelerating the 0.7 %.
///
/// So the oversized set gets its own grid, sized from its own extent and its own count (0711's
/// named fix, deferred there and confirmed by 1351). Cells at that level are ~an order of magnitude
/// wider, which is exactly what a slab needs to bin. Whatever is still too big for the coarse level
/// — a face spanning the whole group — falls to [`Self::spanning`] and keeps the old always-tested
/// behaviour, which is now a residue rather than the main term.
///
/// **Two levels, not recursion.** A recursive grid would need a boxed iterator per query on the
/// hottest path in the interior lane; two explicit levels keep [`ColumnGrid::candidates`] a
/// three-way merge of three ascending *slices* — no allocation, no indirection, and the
/// superset+ascending invariants hold at each level independently.
#[derive(Debug, Clone)]
pub struct ColumnGrid {
    /// The ~1-triangle-per-cell grid over the ordinary faces.
    fine: Level,
    /// The coarse grid over the faces `fine` set aside. `None` when there were too few of them to
    /// be worth a second index — then they are all in [`Self::spanning`], as before.
    coarse: Option<Level>,
    /// Faces too large to bin at **either** level, ascending — candidates for every query.
    spanning: Vec<u32>,
}

/// `WOW_COLUMN_COST=1`: how many triangles a column query actually tests, split by **source** —
/// `binned` (the fine level's cell), `coarse` (the coarse level's cell), and `spanning` (the
/// residue too large for either, tested on every query).
///
/// This is 0711's residual, and the counter that confirmed it. That hunt cut the lane from ~1.2 M
/// triangle tests/frame to an 88 k remainder, wrote down what the remainder was — big floor slabs,
/// too large to bin, merged into every column query — named the fix, and left it. 1351 measured the
/// remainder three weeks later: **99.26 %** of tested triangles were that set, and `resolve_ms` was
/// still 1.14, byte for byte the number 0711 recorded, because nothing had touched the file.
///
/// The counter stays as the **regression watch** on the two-level index: `spanning_pct` is now the
/// residue's share and should read near zero. If it climbs, some face set has a slab too big for
/// even the coarse level and the lane is quietly reverting to 0711's shape.
static COLUMN_QUERIES: AtomicU64 = AtomicU64::new(0);
static COLUMN_BINNED: AtomicU64 = AtomicU64::new(0);
static COLUMN_COARSE: AtomicU64 = AtomicU64::new(0);
static COLUMN_SPANNING: AtomicU64 = AtomicU64::new(0);

/// Whether the column-query meter is armed (`WOW_COLUMN_COST`). Read once, then a relaxed bool.
pub fn column_cost_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WOW_COLUMN_COST").is_some())
}

/// Take and zero the column-query counters: `(queries, binned, coarse, spanning)` triangles tested.
///
/// One caller per frame — the meter that prints them — so the numbers are always per-frame.
pub fn take_column_query_stats() -> (u64, u64, u64, u64) {
    (
        COLUMN_QUERIES.swap(0, Ordering::Relaxed),
        COLUMN_BINNED.swap(0, Ordering::Relaxed),
        COLUMN_COARSE.swap(0, Ordering::Relaxed),
        COLUMN_SPANNING.swap(0, Ordering::Relaxed),
    )
}

impl Level {
    /// Index `ids` (ascending, global triangle indices) into one uniform grid.
    ///
    /// Returns the level plus the **overflow**: the ids whose XY AABB spans more than
    /// [`MAX_SPAN_CELLS`] cells at this level's sizing, ascending, for the next level down.
    /// `None` when the set has no finite extent to grid.
    fn build(
        ids: &[u32],
        xy_aabb: &impl Fn(usize) -> ([f32; 2], [f32; 2]),
    ) -> Option<(Self, Vec<u32>)> {
        let (mut min, mut max) = ([f32::MAX; 2], [f32::MIN; 2]);
        for &i in ids {
            let (lo, hi) = xy_aabb(i as usize);
            for a in 0..2 {
                min[a] = min[a].min(lo[a]);
                max[a] = max[a].max(hi[a]);
            }
        }
        let (w, h) = ((max[0] - min[0]).max(0.0), (max[1] - min[1]).max(0.0));
        if !w.is_finite() || !h.is_finite() || (w <= 0.0 && h <= 0.0) {
            return None;
        }
        // Aim for ~one triangle per cell, then clamp both ways: never finer than MIN_CELL, never
        // more than MAX_CELLS cells. Sizing from THIS set's own count and extent is what makes the
        // coarse level coarse: a few hundred slabs over the same group span give cells ~an order of
        // magnitude wider than the fine level's, which is what lets a slab bin at all.
        let target_cells = ids.len().clamp(1, MAX_CELLS) as f32;
        let area = (w * h).max(f32::MIN_POSITIVE);
        let mut cell = (area / target_cells).sqrt().max(MIN_CELL);
        let (mut nx, mut ny) = dims(w, h, cell);
        while nx as usize * ny as usize > MAX_CELLS {
            cell *= 2.0;
            (nx, ny) = dims(w, h, cell);
        }
        let inv_cell = 1.0 / cell;
        let cells = nx as usize * ny as usize;

        // Pass 1 — count per cell, and set the oversized triangles aside for the next level.
        let mut counts = vec![0u32; cells + 1];
        let mut overflow = Vec::new();
        let span_of = |i: u32| -> Option<(u32, u32, u32, u32)> {
            let (lo, hi) = xy_aabb(i as usize);
            let x0 = cell_of(lo[0], min[0], inv_cell, nx);
            let x1 = cell_of(hi[0], min[0], inv_cell, nx);
            let y0 = cell_of(lo[1], min[1], inv_cell, ny);
            let y1 = cell_of(hi[1], min[1], inv_cell, ny);
            let touched = (x1 - x0 + 1) * (y1 - y0 + 1);
            (touched <= MAX_SPAN_CELLS).then_some((x0, x1, y0, y1))
        };
        for &i in ids {
            match span_of(i) {
                Some((x0, x1, y0, y1)) => {
                    for y in y0..=y1 {
                        for x in x0..=x1 {
                            counts[(y * nx + x) as usize] += 1;
                        }
                    }
                }
                None => overflow.push(i),
            }
        }
        // Prefix sum → CSR starts.
        let mut starts = vec![0u32; cells + 1];
        let mut acc = 0u32;
        for c in 0..cells {
            starts[c] = acc;
            acc += counts[c];
        }
        starts[cells] = acc;

        // Pass 2 — fill. `ids` is ascending, so each cell fills in ascending triangle order, which
        // is the order guarantee the tie-breaks downstream rest on.
        let mut items = vec![0u32; acc as usize];
        let mut cursor = starts.clone();
        for &i in ids {
            if let Some((x0, x1, y0, y1)) = span_of(i) {
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let c = (y * nx + x) as usize;
                        items[cursor[c] as usize] = i;
                        cursor[c] += 1;
                    }
                }
            }
        }
        Some((
            Self {
                min,
                inv_cell,
                nx,
                ny,
                starts,
                items,
            },
            overflow,
        ))
    }

    /// This level's own cell list for the column at `(x, y)` — ascending, possibly empty.
    fn cell(&self, x: f32, y: f32) -> &[u32] {
        let cx = cell_of(x, self.min[0], self.inv_cell, self.nx);
        let cy = cell_of(y, self.min[1], self.inv_cell, self.ny);
        let c = (cy * self.nx + cx) as usize;
        &self.items[self.starts[c] as usize..self.starts[c + 1] as usize]
    }
}

impl ColumnGrid {
    /// Build the index for `count` triangles, given each one's XY AABB by index.
    ///
    /// Returns `None` when there is nothing to accelerate — an empty face set, or one small enough
    /// that a linear scan beats the indirection (the caller then keeps its plain scan).
    pub fn build(count: usize, xy_aabb: impl Fn(usize) -> ([f32; 2], [f32; 2])) -> Option<Self> {
        // Below this a linear scan is the cheaper answer and the grid is pure overhead.
        const MIN_TRIS: usize = 64;
        if count < MIN_TRIS {
            return None;
        }
        let all: Vec<u32> = (0..count as u32).collect();
        let (fine, oversized) = Level::build(&all, &xy_aabb)?;

        // The oversized set gets its own grid when there are enough of them to pay for one. What
        // *that* level cannot bin stays an always-tested residue — the old behaviour, now applied
        // to a set 1351 predicts is tiny rather than to 99 % of the traffic.
        let (coarse, spanning) = if oversized.len() >= MIN_COARSE_TRIS {
            match Level::build(&oversized, &xy_aabb) {
                Some((level, residue)) => (Some(level), residue),
                None => (None, oversized),
            }
        } else {
            (None, oversized)
        };

        Some(Self {
            fine,
            coarse,
            spanning,
        })
    }

    /// The triangles that can own the column at `(x, y)` — a superset, in ascending index order.
    pub fn candidates(&self, x: f32, y: f32) -> ColumnCandidates<'_> {
        let binned = self.fine.cell(x, y);
        let coarse = self.coarse.as_ref().map_or(&[][..], |c| c.cell(x, y));
        if column_cost_enabled() {
            COLUMN_QUERIES.fetch_add(1, Ordering::Relaxed);
            COLUMN_BINNED.fetch_add(binned.len() as u64, Ordering::Relaxed);
            COLUMN_COARSE.fetch_add(coarse.len() as u64, Ordering::Relaxed);
            COLUMN_SPANNING.fetch_add(self.spanning.len() as u64, Ordering::Relaxed);
        }
        ColumnCandidates {
            binned,
            coarse,
            spanning: &self.spanning,
        }
    }

    /// Index size in triangle slots — the memory the acceleration costs, for the load-time log.
    pub fn slots(&self) -> usize {
        self.fine.items.len()
            + self.coarse.as_ref().map_or(0, |c| c.items.len())
            + self.spanning.len()
    }
}

/// Ascending merge of the three sources a column can draw from: the fine level's cell, the coarse
/// level's cell, and the always-tested residue. All three are ascending, so this is a three-pointer
/// walk and the output order matches a linear scan exactly — which is the guarantee every downstream
/// tie-break (`down_ray_claim`'s first-wins, `footprint_sample`'s later-wins) rests on.
///
/// A triangle appears in exactly one source by construction: a face is binned at the fine level, or
/// handed down to the coarse level, or in the residue — never two. So the merge cannot duplicate.
pub struct ColumnCandidates<'a> {
    binned: &'a [u32],
    coarse: &'a [u32],
    spanning: &'a [u32],
}

impl Iterator for ColumnCandidates<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        // Pick the smallest head across the three streams; `None` sorts last.
        let heads = [
            self.binned.first().copied(),
            self.coarse.first().copied(),
            self.spanning.first().copied(),
        ];
        let pick = heads
            .iter()
            .enumerate()
            .filter_map(|(n, h)| h.map(|v| (n, v)))
            .min_by_key(|&(_, v)| v)?
            .0;
        let src = match pick {
            0 => &mut self.binned,
            1 => &mut self.coarse,
            _ => &mut self.spanning,
        };
        let (first, rest) = src.split_first()?;
        *src = rest;
        Some(*first as usize)
    }
}

/// Grid dimensions for an extent at a cell size (at least one cell each way).
fn dims(w: f32, h: f32, cell: f32) -> (u32, u32) {
    let n = |extent: f32| ((extent / cell).ceil() as u32).max(1);
    (n(w), n(h))
}

/// Cell coordinate of a world value, clamped into the grid — a column outside the face set's
/// bounds lands in the edge cell rather than missing, which keeps the candidate set a superset
/// under float error at the boundary.
fn cell_of(v: f32, min: f32, inv_cell: f32, n: u32) -> u32 {
    let i = ((v - min) * inv_cell).floor();
    if i < 0.0 {
        0
    } else {
        (i as u32).min(n - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A triangle's XY AABB, the shape the callers hand `build`.
    fn aabb(tri: [[f32; 3]; 3]) -> ([f32; 2], [f32; 2]) {
        let xs = [tri[0][0], tri[1][0], tri[2][0]];
        let ys = [tri[0][1], tri[1][1], tri[2][1]];
        (
            [
                xs.iter().copied().fold(f32::MAX, f32::min),
                ys.iter().copied().fold(f32::MAX, f32::min),
            ],
            [
                xs.iter().copied().fold(f32::MIN, f32::max),
                ys.iter().copied().fold(f32::MIN, f32::max),
            ],
        )
    }

    /// A deterministic pseudo-random field of small triangles plus a few slabs — the dungeon shape
    /// (many small faces, a handful of huge floors) the index has to survive.
    fn field(n: usize) -> Vec<[[f32; 3]; 3]> {
        let mut out = Vec::with_capacity(n);
        let mut s = 12345u32;
        let mut rnd = || {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            (s >> 8) as f32 / (1 << 24) as f32
        };
        for i in 0..n {
            let (x, y) = (rnd() * 200.0 - 100.0, rnd() * 200.0 - 100.0);
            if i % 97 == 0 {
                // A slab spanning a large area — the MAX_SPAN_CELLS path.
                out.push([[x, y, 0.0], [x + 80.0, y, 0.0], [x, y + 80.0, 0.0]]);
            } else {
                let (dx, dy) = (rnd() * 3.0, rnd() * 3.0);
                out.push([[x, y, 0.0], [x + dx, y, 0.0], [x, y + dy, 0.0]]);
            }
        }
        out
    }

    /// The load-bearing property: for any column, the index's candidates are a SUPERSET of the
    /// triangles a linear scan would find containing it — an index that can miss a face silently
    /// moves a unit's room, its light, and the zone name.
    #[test]
    fn candidates_are_a_superset_of_the_linear_scan() {
        let tris = field(4000);
        let grid = ColumnGrid::build(tris.len(), |i| aabb(tris[i])).expect("field indexes");
        let inside = |t: &[[f32; 3]; 3], x: f32, y: f32| {
            let (lo, hi) = aabb(*t);
            x >= lo[0] && x <= hi[0] && y >= lo[1] && y <= hi[1]
        };
        let mut probes = 0;
        for gx in -12..=12 {
            for gy in -12..=12 {
                let (x, y) = (gx as f32 * 9.7, gy as f32 * 9.3);
                let got: Vec<usize> = grid.candidates(x, y).collect();
                for (i, t) in tris.iter().enumerate() {
                    if inside(t, x, y) {
                        assert!(
                            got.contains(&i),
                            "column ({x}, {y}) missed triangle {i} — the index is not a superset"
                        );
                    }
                }
                probes += 1;
            }
        }
        assert!(probes > 500, "the sweep must actually probe");
    }

    /// The other half of "never changes a verdict": candidates arrive in the order a linear scan
    /// visits them, so first-wins / later-wins tie-breaks downstream are unaffected.
    #[test]
    fn candidates_are_ascending() {
        let tris = field(2000);
        let grid = ColumnGrid::build(tris.len(), |i| aabb(tris[i])).expect("field indexes");
        for gx in -8..=8 {
            for gy in -8..=8 {
                let got: Vec<usize> = grid
                    .candidates(gx as f32 * 12.0, gy as f32 * 12.0)
                    .collect();
                assert!(
                    got.windows(2).all(|w| w[0] < w[1]),
                    "candidates must be strictly ascending, got {got:?}"
                );
            }
        }
    }

    /// A column far outside the face set still returns a valid (edge-cell) candidate list rather
    /// than panicking or indexing out of range.
    #[test]
    fn columns_outside_the_bounds_are_safe() {
        let tris = field(200);
        let grid = ColumnGrid::build(tris.len(), |i| aabb(tris[i])).expect("field indexes");
        for (x, y) in [(-1e6, -1e6), (1e6, 1e6), (0.0, 1e6), (f32::MIN, f32::MAX)] {
            let _: Vec<usize> = grid.candidates(x, y).collect();
        }
    }

    /// The two-level structure actually engages on the dungeon shape, and the residue it leaves is
    /// small. Without this, a regression that quietly stopped building the coarse level would still
    /// pass every invariant test above — the index would just be slow again, which is exactly how
    /// 0711's residual survived ~600 decision records unnoticed.
    #[test]
    fn the_coarse_level_takes_the_slabs() {
        let tris = field(4000);
        let grid = ColumnGrid::build(tris.len(), |i| aabb(tris[i])).expect("field indexes");

        // `field` plants a slab every 97th triangle; every one of them is far too wide to bin at
        // the fine level's ~1-triangle-per-cell sizing.
        let slabs = tris.len().div_ceil(97);
        let coarse = grid
            .coarse
            .as_ref()
            .expect("the oversized set must get its own grid");
        assert!(
            coarse.items.len() >= slabs / 2,
            "the coarse level binned {} of ~{slabs} slabs — it is not taking them",
            coarse.items.len()
        );
        assert!(
            grid.spanning.len() * 4 < slabs,
            "residue {} is not small against ~{slabs} slabs — the coarse cells are too fine",
            grid.spanning.len()
        );
    }

    /// The merge draws from three sources; a face must appear in exactly one of them, or a column
    /// would test it twice and `later-wins` tie-breaks would read the same triangle as two hits.
    #[test]
    fn no_triangle_is_in_two_sources() {
        let tris = field(3000);
        let grid = ColumnGrid::build(tris.len(), |i| aabb(tris[i])).expect("field indexes");
        for gx in -10..=10 {
            for gy in -10..=10 {
                let (x, y) = (gx as f32 * 11.3, gy as f32 * 10.7);
                let got: Vec<usize> = grid.candidates(x, y).collect();
                let mut sorted = got.clone();
                sorted.sort_unstable();
                sorted.dedup();
                assert_eq!(
                    sorted.len(),
                    got.len(),
                    "column ({x}, {y}) yielded a duplicate"
                );
            }
        }
    }

    /// Small face sets opt out — the caller keeps its linear scan rather than paying indirection.
    #[test]
    fn tiny_face_sets_are_not_indexed() {
        let tris = field(8);
        assert!(ColumnGrid::build(tris.len(), |i| aabb(tris[i])).is_none());
    }
}
