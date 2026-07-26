//! Liquid (water) rendering: the animated lake/river/ocean surfaces the reference draws over MCLQ
//! geometry. The parse + flat mesh live in `benilla_formats::liquid` (built into each `ChunkMesh.liquid`
//! by the terrain loader); this is the Bevy render glue — one shared [`LiquidMaterial`] per
//! [`LiquidKind`], a `texture_2d_array` of its animated frames, and a 24 fps frame-index cycler.
//!
//! Faithful model (RE'd from `WoW.exe` + `ocean0_s.bls` + apitrace WoW.17 program 159, all agree).
//! `ocean0_s.bls`: `rgb = primary·colorTex.rgb + detailTex.rgb + (secondary+0.25)·detailTex.a`,
//! `alpha = colorTex.a`. The body colour is **`primary · waterTint`**, where:
//! - **`waterTint`** is a plain **2-endpoint linear lerp** of the zone's dedicated `Light.dbc` water
//!   rows, RAW (no ×0.711): IntBand rows 16/17 (river/lake) or 14/15 (ocean), shallow→deep, by the
//!   per-vertex depth `V` (river/lake `V = clamp(byte/42)`, VERIFIED `c81768`/`FUN_0068d790`; saturates
//!   ~5 yd so the channel middle reaches the deep/teal row). Swatch builder VERIFIED: WoW.exe
//!   `FUN_0068a830`, golden-vector-matched to the apitrace swatch ≤1/255 over all 64 rows. (The earlier
//!   "reflected sky × 0.711 via `FUN_0068c250`" model fingered the WRONG builder — a separate grey edge
//!   texture never bound on the water unit; and `byte/255` was the wrong LUT → river never went teal.)
//! - **`primary`** is the lit vertex colour `clamp(ambient + N·L·sun)`.
//! - the animated `lake_a`/`ocean_h` frame is the **`detailTex`** (near-black RGB + ripple alpha): a
//!   faint flat lift + an achromatic shimmer on crests — NOT the body colour. Mipped + 16× aniso so the
//!   ripple averages out at distance (near-field samples mip 0, so near sparkle is the term itself).
//! - **opacity** = the SAME `V` indexes both colour and alpha (one swatch row → RGB + A): a ramp between
//!   the LightParams shallow/deep alphas — river 0.5→1.0, ocean 0.75→1.0 (VERIFIED WoW.exe `FUN_0068a830`
//!   α = `127+2·row`). The river channel reaches α=1.0 (opaque, deep teal) by byte 42 ≈ 5 yd; the shore
//!   stays see-through (the pale edge band, faithful — the bottom shows).
//!
//! River/lake `V = clamp(byte/42)` (steep `c81768` LUT, `FUN_0068d790`) — NOT `byte/255` (the `c7fcd8`
//! LUT, a different draw list the from-above river path doesn't use; it left the river middle stuck on
//! the shallow green row). Ocean uses a non-LUT UV path → placeholder `/255` pending its own RE+A/B.
//! (Earlier cuts: ripple-as-colour → black; `×8` → "deep too early"; FLAT colour → "completely gone";
//! sky × 0.711 → wrong builder; `byte/255` → wrong LUT, no teal centre. Faithful = rows 14–17 raw lerp
//! + the /42 V. 2026-05-31.)
//!
//! Two-sided, alpha-blended, depth-write off (Bevy's transparent pass = the verified MCLQ water render
//! state).
//!
//! The frame-flip is the client's first render animation — a deliberate **one-off** (a frame-index
//! uniform off Bevy real `Time`), NOT a general animation system. Two clocks: animation =
//! wall-clock; day/night = server game-time.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::{ExtendedMaterial, MaterialPlugin};
use bevy::prelude::*;

use crate::assets::LockRecover;
use crate::assets::{liquid_frame_array, AssetSet, RenderConfig, WorldAssets};
use crate::lighting::{
    WowLighting, OCEAN_SHALLOW_ALPHA, RIVER_SHALLOW_ALPHA, WATER_DEEP_ALPHA, WATER_SHININESS,
};
use crate::player::WorldCamera;
use crate::terrain::{LiquidExt, LiquidMaterial};
use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use benilla_formats::{read_texture_mip_chain, BlpMipChain, LiquidKind, LiquidMesh};

/// Frame-flip rate — 30 frames over 1.25 s (VERIFIED `FUN_0068aac0`), i.e. 24 fps, real wall-clock.
const ANIM_FPS: f32 = 24.0;

/// The water subsystem: load the per-kind frame arrays + shared materials at startup, then cycle the
/// animation frame each update. Spawning the per-chunk surfaces happens in the terrain streamer (via
/// [`spawn_liquids`], water lives *with* its tile), reading [`LiquidAssets`].
pub(crate) struct LiquidPlugin;

impl Plugin for LiquidPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<LiquidMaterial>::default())
            .init_resource::<Underwater>()
            .add_systems(Startup, setup_liquid.after(AssetSet::Open))
            .add_systems(Update, (animate_liquid, detect_submersion));
    }
}

/// Whether the camera eye is currently below a water surface. Set by [`detect_submersion`]; read by
/// `lighting::update_time_lighting`, which (when true) samples the **underwater** Light param so the
/// whole scene gets the dense teal fog + cool tint + teal clear colour (VERIFIED apitrace WoW.18 —
/// the murk is fog + light-tint, no overlay quad). Two clocks aside, this is the one cross-feed from
/// the water subsystem into lighting.
#[derive(Resource, Default)]
pub(crate) struct Underwater(pub(crate) bool);

/// Which file the liquid came from — the **delegation key** for [`liquid_at`].
///
/// The reference's liquid query is context-aware: terrain's `0x69b6d0` **delegates the WMO case out**
/// via `0x69b520` (wow-re `terrain/scratch/class-batch3.md`), and the per-frame camera probe runs the
/// ADT query `0x6b9f10` *or* the WMO fallback `0x6723d0` (`sound/scratch/benilla-pins.md`). Without
/// the distinction a tunnel bored under a lake inherits the lake: an ADT footprint is a flat XY
/// rectangle with no floor, so every position beneath it reads as submerged — the "swim in air"
/// family (decision 0634).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LiquidSource {
    /// An ADT map-chunk surface (MCLQ) — the outdoor world's lakes, rivers, coast.
    AdtChunk,
    /// A WMO group's embedded surface (MLIQ) — canals, fountains, the Great Forge lava,
    /// Undercity's slime.
    WmoGroup,
}

/// One liquid surface as the submersion/swim/foam/sound queries see it: its **grid**, in world WoW
/// space with the placement transform already baked in, plus the XY bounds, which file it came
/// from, and which liquid it is. Attached to each [`LiquidSurface`]; despawns with its tile, so no
/// manual lifecycle.
///
/// Named `Water*` from when only water carried one. It now rides **every** kind — magma and slime
/// included, which is what makes Blackrock's lava and Undercity's slime swimmable at all (decision
/// 0634). Consumers that are specifically about *water* (the teal murk, foam, the splash) filter on
/// [`Self::kind`]; the swim mode does not, because you swim in lava too.
///
/// **A liquid is a grid — not a plane, not a triangle soup.** Both of its questions, *is this XY
/// wet* and *how high is the surface here*, are answered by locating the containing cell
/// ([`LiquidGrid::wet_cell_at`]) and reading it: the cell's own flag for the first, a bilinear over
/// its four corner heights for the second — both O(1). The bounding box is only a cheap reject, and
/// the triangles are only what the renderer draws.
#[derive(Component)]
pub(crate) struct WaterChunkInfo {
    min_x: f32,
    max_x: f32,
    min_y: f32,
    max_y: f32,
    source: LiquidSource,
    kind: LiquidKind,
    grid: LiquidGrid,
}

/// A liquid surface's vertex grid in world WoW space: `cols × rows` positions row-major
/// (`j·cols + i`), one wet flag per `(cols−1) × (rows−1)` cell, and the lattice's affine basis.
///
/// The grid stays a regular lattice under any placement — a WMO's MODF transform is affine, so the
/// world positions are exactly `origin + i·u + j·v` in XY. That is *measured*, not assumed:
/// Blackrock's 55×82 magma grid (a ~7° yaw placement, `u = (−4.136, −0.508)`) and Felwood's
/// axis-aligned 9×9 MCLQ both reproduce from the span-derived basis to **0.0005 yd** — one f32 ulp
/// at world magnitude. So a world XY inverts to grid coordinates with one 2×2 solve, and neither
/// the rotated case nor the axis-aligned one needs a search.
struct LiquidGrid {
    cols: usize,
    rows: usize,
    /// Vertex positions, world WoW, row-major `j·cols + i`.
    positions: Vec<[f32; 3]>,
    /// Per-cell liquid coverage, row-major over `(cols−1) × (rows−1)`.
    wet: Vec<bool>,
    /// Grid vertex `(0, 0)`, XY.
    origin: [f32; 2],
    /// World XY step per `+1` in `i` / in `j` — derived over the **full span** (`(last − first)/n`)
    /// rather than from one adjacent pair. At world magnitude a single f32 difference of two ~7600
    /// yd coordinates carries ~1e-4 relative error, which over Blackrock's 54 cells drifts 0.02 yd;
    /// dividing the same error by the span lands it at one ulp instead (both measured).
    u: [f32; 2],
    v: [f32; 2],
    /// `1/det` of the `[u v]` basis — `None` when the lattice is degenerate in XY (a placement that
    /// stood the liquid plane on edge, or a malformed grid). Degenerate ⇒ queries fall back to the
    /// bounds.
    inv_det: Option<f32>,
    /// The highest wet vertex — the **degenerate fallback only**. Never the answer for a grid we can
    /// sample: taking the chunk maximum as "the surface" is precisely the bug this type was rebuilt
    /// to kill (decision 0642).
    fallback_z: f32,
}

/// How far outside the grid, in cells, a query may land and still be snapped back in. The lattice
/// reproduces to ~1e-4 cells, so this is pure edge hygiene (≈4 mm): a player standing exactly on
/// the outer rim of a lake must not fall through it on an f32 tie.
const GRID_EDGE_TOLERANCE: f32 = 1e-3;

impl LiquidGrid {
    /// The cell containing this world XY plus the in-cell fractions — `(i, j, fx, fy)` with
    /// `0 ≤ fx, fy ≤ 1` — or `None` if the XY is off the grid, over a dry cell, or the grid is
    /// unusable.
    ///
    /// The dry-cell rejection is the whole of decision 0635: a liquid grid is sparse (MLIQ per-tile
    /// nibble `0xf` = hole, MCLQ likewise), so its bounding box routinely spans ground the liquid
    /// never covers — canal banks, the tunnel under a canal, the dirt beside a river. One MLIQ
    /// grid's box in Stormwind is **95 × 80 yards** and covers the canal *and* the dry mage-district
    /// tunnel beside it; `[min,max]` alone can never tell them apart.
    fn wet_cell_at(&self, x: f32, y: f32) -> Option<(usize, usize, f32, f32)> {
        let (cells_x, cells_y) = (self.cols.checked_sub(1)?, self.rows.checked_sub(1)?);
        let inv_det = self.inv_det?;
        // Invert the lattice basis: p − origin = a·u + b·v, solved in cell units.
        let (dx, dy) = (x - self.origin[0], y - self.origin[1]);
        let a = (dx * self.v[1] - dy * self.v[0]) * inv_det;
        let b = (self.u[0] * dy - self.u[1] * dx) * inv_det;
        let snap = |t: f32, cells: usize| -> Option<(usize, f32)> {
            if t < -GRID_EDGE_TOLERANCE || t > cells as f32 + GRID_EDGE_TOLERANCE {
                return None;
            }
            // The last cell owns its far edge, so `t == cells` lands in cell `cells−1` at f = 1.
            let idx = (t.floor().max(0.0) as usize).min(cells - 1);
            Some((idx, (t - idx as f32).clamp(0.0, 1.0)))
        };
        let (i, fx) = snap(a, cells_x)?;
        let (j, fy) = snap(b, cells_y)?;
        self.wet.get(j * cells_x + i)?.then_some((i, j, fx, fy))
    }

    /// The liquid surface height (WoW Z) at an in-cell position — the **bilinear** over the cell's
    /// four corner heights.
    ///
    /// This is the reference's own rule: `0x6b7500` `liquid_height_sample` locates the cell, then
    /// lerps along one axis and then the other over exactly these four heights (wow-re
    /// `system/terrain/terrain.md` — transcribed there and difftested bit-exact against `WoW.exe`).
    /// Same shape here, over the same corners.
    fn height_in_cell(&self, i: usize, j: usize, fx: f32, fy: f32) -> f32 {
        let z = |i: usize, j: usize| self.positions[j * self.cols + i][2];
        let t1 = z(i, j) + (z(i + 1, j) - z(i, j)) * fx;
        let t2 = z(i, j + 1) + (z(i + 1, j + 1) - z(i, j + 1)) * fx;
        t1 + (t2 - t1) * fy
    }

    /// The `(lowest, highest)` wet vertex — how much relief this one surface carries. Walks the wet
    /// cells; for the `/liquid` instrument only, which runs once per invocation.
    fn wet_z_range(&self) -> (f32, f32) {
        let Some(cells_x) = self.cols.checked_sub(1) else {
            return (self.fallback_z, self.fallback_z);
        };
        let mut lo = f32::MAX;
        for cell in (0..self.wet.len()).filter(|&c| self.wet[c]) {
            let (i, j) = (cell % cells_x, cell / cells_x);
            for (di, dj) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                lo = lo.min(self.positions[(j + dj) * self.cols + i + di][2]);
            }
        }
        (lo.min(self.fallback_z), self.fallback_z)
    }
}

impl WaterChunkInfo {
    /// Build a footprint from a **world-space** liquid grid: `cols × rows` positions row-major and
    /// one wet flag per cell. Bounds and the degenerate fallback height come from the wet cells'
    /// own corners, so a sparse grid's box stays as tight as its liquid.
    pub(crate) fn new(
        source: LiquidSource,
        kind: LiquidKind,
        grid: [usize; 2],
        positions: Vec<[f32; 3]>,
        wet: Vec<bool>,
    ) -> Self {
        // A grid whose dimensions don't match its arrays is normalized away to an EMPTY one here,
        // in the one place that can judge it — so every method below indexes a grid it has already
        // been told is self-consistent, instead of each re-deriving that judgement (and one of them
        // getting it wrong). An empty grid has no bounds, so it simply claims nothing.
        let [cols, rows] = grid;
        let sane = cols >= 2
            && rows >= 2
            && positions.len() == cols * rows
            && wet.len() == (cols - 1) * (rows - 1);
        if !sane {
            return WaterChunkInfo {
                min_x: f32::MAX,
                max_x: f32::MIN,
                min_y: f32::MAX,
                max_y: f32::MIN,
                source,
                kind,
                grid: LiquidGrid {
                    cols: 0,
                    rows: 0,
                    positions: Vec::new(),
                    wet: Vec::new(),
                    origin: [0.0; 2],
                    u: [0.0; 2],
                    v: [0.0; 2],
                    inv_det: None,
                    fallback_z: f32::MIN,
                },
            };
        }
        let (mut min_x, mut max_x) = (f32::MAX, f32::MIN);
        let (mut min_y, mut max_y) = (f32::MAX, f32::MIN);
        let mut fallback_z = f32::MIN;
        for cell in (0..wet.len()).filter(|&c| wet[c]) {
            let (i, j) = (cell % (cols - 1), cell / (cols - 1));
            for (di, dj) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = positions[(j + dj) * cols + i + di];
                min_x = min_x.min(p[0]);
                max_x = max_x.max(p[0]);
                min_y = min_y.min(p[1]);
                max_y = max_y.max(p[1]);
                fallback_z = fallback_z.max(p[2]);
            }
        }
        // Span-derived basis (see `LiquidGrid::u`) and its 2×2 determinant. A plane stood on edge
        // projects to a line in XY: no cell lookup is possible there, so leave `inv_det` `None` and
        // let the query fall back to the bounds rather than answer a wrong cell.
        let origin = [positions[0][0], positions[0][1]];
        let step = |far: [f32; 3], n: usize| {
            [
                (far[0] - origin[0]) / n as f32,
                (far[1] - origin[1]) / n as f32,
            ]
        };
        let u = step(positions[cols - 1], cols - 1);
        let v = step(positions[(rows - 1) * cols], rows - 1);
        let det = u[0] * v[1] - u[1] * v[0];
        WaterChunkInfo {
            min_x,
            max_x,
            min_y,
            max_y,
            source,
            kind,
            grid: LiquidGrid {
                cols,
                rows,
                positions,
                wet,
                origin,
                u,
                v,
                inv_det: (det.abs() > 1e-9).then(|| 1.0 / det),
                fallback_z,
            },
        }
    }

    /// The liquid surface height (WoW Z) at this WoW-space XY, or `None` where this surface isn't
    /// there — the **one** question the swim, submersion, wade and foam queries ask. A `None` is
    /// exactly "dry here"; there is deliberately no second predicate that answers wet/dry on its
    /// own, because two spellings of one question are how the box test and the cell test were able
    /// to disagree for as long as they did.
    ///
    /// The answer is the bilinear sample of the containing cell — **not the chunk's highest
    /// vertex**. A liquid grid is a heightfield, not a plane: Blackrock's magma runs 167.29 → 175.00
    /// across one group, and Felwood's river drops ~2 yd across a single MCNK. The maximum is the
    /// *whole surface's* ceiling, which near the low end sits metres above the liquid actually under
    /// your feet — and that read as "swim in air" over both (decision 0642).
    pub(crate) fn surface_z_at(&self, x: f32, y: f32) -> Option<f32> {
        if !self.contains(x, y) {
            return None; // the bounding box is the cheap reject
        }
        match self.grid.wet_cell_at(x, y) {
            Some((i, j, fx, fy)) => Some(self.grid.height_in_cell(i, j, fx, fy)),
            // A grid we can't invert must not silently swallow its whole box — fall back to the
            // bounds and the highest wet vertex rather than report a surface we failed to lay out
            // as dry. A wrong "dry" is a player falling through a lake; a wrong "wet" is milder.
            None if self.grid.inv_det.is_none() => Some(self.grid.fallback_z),
            None => None,
        }
    }

    /// Is this WoW-space XY inside the chunk's wet footprint?
    pub(crate) fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.min_x && x <= self.max_x && y >= self.min_y && y <= self.max_y
    }

    /// Does this WoW-space XY box overlap the chunk's wet footprint?
    pub(crate) fn overlaps(&self, lo_x: f32, hi_x: f32, lo_y: f32, hi_y: f32) -> bool {
        hi_x >= self.min_x && lo_x <= self.max_x && hi_y >= self.min_y && lo_y <= self.max_y
    }

    /// Call `f` with every wet cell's four world-WoW corners, `[tl, tr, bl, br]` — the foam
    /// builder's view of the surface, which clips each decal to the wet cells overlapping its box.
    pub(crate) fn for_each_wet_cell(&self, mut f: impl FnMut([[f32; 3]; 4])) {
        let g = &self.grid;
        let Some(cells_x) = g.cols.checked_sub(1) else {
            return;
        };
        for cell in (0..g.wet.len()).filter(|&c| g.wet[c]) {
            let (i, j) = (cell % cells_x, cell / cells_x);
            let p = |di: usize, dj: usize| g.positions[(j + dj) * g.cols + i + di];
            f([p(0, 0), p(1, 0), p(0, 1), p(1, 1)]);
        }
    }

    /// The wet footprint's nearest point to a WoW-space XY, ON the surface — the liquid ambient
    /// loop's emitter slew target (the ref positions the channel at the nearest liquid cell; the
    /// AABB clamp is our cell-level approximation, noted in 0506). Its height is the surface's at
    /// that clamped XY, falling back to the highest wet vertex when the clamp lands over a hole.
    pub(crate) fn nearest_point_wow(&self, x: f32, y: f32) -> [f32; 3] {
        let cx = x.clamp(self.min_x, self.max_x);
        let cy = y.clamp(self.min_y, self.max_y);
        [
            cx,
            cy,
            self.surface_z_at(cx, cy).unwrap_or(self.grid.fallback_z),
        ]
    }
}

/// Marks a liquid surface that grows **foam** — water kinds only, never magma or slime.
///
/// A marker, not data: the wet cells foam clips against live on [`WaterChunkInfo`]
/// ([`WaterChunkInfo::for_each_wet_cell`]), because the swim query needs the very same cells to
/// answer "is this XY actually wet". They used to be duplicated here, which is how the two could
/// disagree — foam clipped to the wet cells while swimming only ever tested the bounding box.
#[derive(Component)]
pub(crate) struct FoamPatch;

/// One liquid the query landed in: its surface height (WoW Z) and which liquid it is.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct LiquidHit {
    pub(crate) surface_z: f32,
    pub(crate) kind: LiquidKind,
}

/// The liquid over a **WoW-space** position — the shared query under swim mode, submersion, wading
/// and the enter-water sounds.
///
/// **`indoors` is the whole delegation** ([`LiquidSource`]): `Some(true)` (inside a WMO interior)
/// answers from that building's own MLIQ surfaces only, `Some(false)` from the ADT's MCLQ ones only.
/// This mirrors the reference, whose terrain query delegates the WMO case out rather than unioning
/// the two, and it is the fix for "swim in air": a footprint is a flat XY rectangle with **no
/// floor**, so before the split the Stormwind canal claimed the mage-district tunnel beneath it and
/// Undercity's water claimed the rooms below it.
///
/// `None` = **the caller has no interior state for this subject** — today only the remote-unit wade
/// probe (`net::motion::spline`), since `CurrentWmoInterior` is a down-ray published for the local
/// player alone. Both sources answer, i.e. the pre-0634 behaviour, so another player wading inside a
/// building can still read the outdoor water beneath it. Cosmetic (their splash sound and wade
/// pose); a named gap, not a silent guess — fixing it wants a per-unit interior claim.
///
/// Each candidate answers with its height **at this XY** ([`WaterChunkInfo::surface_z_at`]), and
/// among them the **lowest wins**. Overlapping footprints used to resolve by iteration order
/// (`.next()`) — an arbitrary pick that made the answer depend on spawn order. The lowest is the one
/// whose volume you are actually in when standing between two stacked surfaces.
///
/// Still ignores Z **below** the surface — see the module TODO: bounding a liquid from underneath
/// needs the reference's rule, which is not yet carved. The `indoors` split removes the reported
/// symptom; a multi-floor WMO whose upper storey holds water would still claim the storey below.
pub(crate) fn liquid_at<'a>(
    liquids: impl Iterator<Item = &'a WaterChunkInfo>,
    wow: [f32; 3],
    indoors: Option<bool>,
) -> Option<LiquidHit> {
    let want = indoors.map(|inside| {
        if inside {
            LiquidSource::WmoGroup
        } else {
            LiquidSource::AdtChunk
        }
    });
    liquids
        .filter(|w| want.is_none_or(|s| w.source == s))
        .filter_map(|w| {
            w.surface_z_at(wow[0], wow[1]).map(|surface_z| LiquidHit {
                surface_z,
                kind: w.kind,
            })
        })
        .min_by(|a, b| a.surface_z.total_cmp(&b.surface_z))
}

/// Every loaded liquid footprint containing this WoW XY, one human-readable line each — the body of
/// the `/liquid` chat instrument.
///
/// Built because the "swim in air" family cannot be reasoned about from the outside: the answer
/// depends on which surfaces cover a spot, which FILE each came from, and the player's live interior
/// claim — three things no offline dump can see together. Prints every candidate, not just the
/// winner, so a surface that should not be claiming is visible next to the one that should.
///
/// Each line also carries the **cell** the height came from and the surface's full Z range, because
/// the two failures this instrument exists to separate look identical without them: claiming a spot
/// it shouldn't (wrong cell) versus claiming the right spot at the wrong height (wrong height rule).
/// 0635 read a footprint's *size* off this instrument to find the first; a `grid z` span far from
/// the sampled height is the second (decision 0642).
pub(crate) fn describe_at<'a>(
    liquids: impl Iterator<Item = &'a WaterChunkInfo>,
    wow: [f32; 3],
) -> Vec<String> {
    let mut out: Vec<(f32, String)> = liquids
        .filter(|w| w.contains(wow[0], wow[1]))
        .map(|w| {
            let z = w.surface_z_at(wow[0], wow[1]);
            let (lo, hi) = w.grid.wet_z_range();
            let here = match (z, w.grid.wet_cell_at(wow[0], wow[1])) {
                (Some(z), Some((i, j, fx, fy))) => format!(
                    "WET-CELL surface z {z:.2} ({:+.2} over feet)  cell [{i},{j}] +({fx:.2},{fy:.2})",
                    z - wow[2]
                ),
                (Some(z), None) => format!(
                    "no-grid (bounds fallback) surface z {z:.2} ({:+.2} over feet)",
                    z - wow[2]
                ),
                (None, _) => "box-only (dry here)".to_string(),
            };
            (
                z.unwrap_or(hi),
                format!(
                    "{:?} {:?} {here}  grid z [{lo:.2}..{hi:.2}]  xy [{:.0}..{:.0}, {:.0}..{:.0}]",
                    w.source, w.kind, w.min_x, w.max_x, w.min_y, w.max_y,
                ),
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out.into_iter().map(|(_, line)| line).collect()
}

/// [`liquid_at`] restricted to **water** kinds — the query for the consumers that are about water
/// specifically (the teal murk, foam, the wade splash), which must not fire in the Great Forge's
/// lava or Undercity's slime. Swim mode deliberately does NOT use this one.
pub(crate) fn water_surface_at<'a>(
    water: impl Iterator<Item = &'a WaterChunkInfo>,
    wow: [f32; 3],
    indoors: Option<bool>,
) -> Option<f32> {
    liquid_at(water.filter(|w| !w.kind.is_fullbright()), wow, indoors).map(|h| h.surface_z)
}

// The **wade ceiling** used to live here, as `WADE_MAX = 2.0` — a flat proxy for a boundary B7
// (decision 0226) had already shown to be `0.75·collisionHeight`, kept because the per-unit height
// it needed was 0464's un-plumbed `CreatureModelData.collisionHeight`. Decision 0645 plumbed it, so
// the proxy is gone and there is no wade constant to re-import: wading is *the complement of
// swimming*, one number, and its one spelling is `player::swim_enter_depth(h)`. A human's line
// moved 2.0 → 1.52 yd, a murloc's far shallower.

/// Eye-submersion accept margin (VERIFIED `FUN_0069b6d0`: `eye.z < surface + 0.01`).
const SUBMERSION_EPS: f32 = 0.01;

/// Set [`Underwater`] from the camera vs the water surfaces: the eye is submerged if it's over a wet
/// cell and below that cell's surface (`FUN_0069b6d0` — its 9×9 bilinear sample is now what
/// [`WaterChunkInfo::surface_z_at`] does, so this is the binary's own rule and no longer a per-chunk
/// flat approximation of it). One pass over the loaded water surfaces (a few hundred, cheap).
fn detect_submersion(
    mut underwater: ResMut<Underwater>,
    camera: Query<&Transform, With<WorldCamera>>,
    water: Query<&WaterChunkInfo>,
) {
    let Ok(cam) = camera.single() else {
        return;
    };
    let eye = bevy_to_wow(cam.translation); // [x, y, z] WoW yards
                                            // WATER only: the murk this drives is the teal underwater fog/tint. Magma and slime now carry a
                                            // footprint too (they became swimmable in 0634), and dunking the camera in the Great Forge would
                                            // otherwise turn the lava teal.
    underwater.0 = water.iter().filter(|w| !w.kind.is_fullbright()).any(|w| {
        w.surface_z_at(eye[0], eye[1])
            .is_some_and(|z| eye[2] < z + SUBMERSION_EPS)
    });
}

/// The shared liquid materials, one per [`LiquidKind`], plus each one's animated frame count (for
/// the modulo in `animate_liquid`). Read by the terrain streamer (via [`spawn_liquids`]) to material
/// the per-chunk water meshes. Absent when the client has no data (no `WorldAssets`).
#[derive(Resource, Default)]
pub(crate) struct LiquidAssets {
    materials: HashMap<LiquidKind, LiquidEntry>,
}

struct LiquidEntry {
    material: Handle<LiquidMaterial>,
    frame_count: u32,
}

impl LiquidAssets {
    /// The shared material for a liquid kind, if its frames loaded.
    pub(crate) fn material(&self, kind: LiquidKind) -> Option<Handle<LiquidMaterial>> {
        self.materials.get(&kind).map(|e| e.material.clone())
    }

    /// `(kind, material handle)` for each loaded kind — so `lighting::apply_wow_lighting` can push the
    /// per-kind water colours + alpha onto the right shared material each light change.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (LiquidKind, &Handle<LiquidMaterial>)> {
        self.materials.iter().map(|(k, e)| (*k, &e.material))
    }
}

/// Marks a spawned water surface (one per liquid MCNK chunk), so it can be queried/culled as a group.
#[derive(Component)]
pub(crate) struct LiquidSurface;

/// What the **above-water ambient-loop system** needs beyond the surface's geometry (wow-re
/// `liquid-ambience-loop.md`, decision 0506): the sound-class nibble the driver resolves through
/// `SoundWaterType.dbc`. Attached to **every** liquid surface, the fullbright kinds included (the
/// Ironforge lava rumble, Undercity slime).
///
/// It used to carry its own copy of the footprint — bounds + a surface height — because when 0506
/// wrote it, magma and slime carried no [`WaterChunkInfo`] to read them from. 0634 gave every kind
/// one (that is what made lava swimmable), so the copy became a third set of numbers describing the
/// same surface, and its height stayed the chunk maximum after the grid sample landed. The driver
/// queries `(&LiquidSoundSource, &WaterChunkInfo)` instead — the pairing both spawn paths already
/// guarantee — and reads the geometry from the one component that owns it.
#[derive(Component)]
pub(crate) struct LiquidSoundSource {
    /// The surface's sound-class nibble (`class = n & 3`, `FluidSpeed = n & 0xc`).
    pub(crate) nibble: u8,
}

/// A liquid surface's [`WaterChunkInfo`] — its grid lifted into **world** WoW space, with
/// `transform` mapping the mesh's local space into the world.
///
/// For MCLQ water `lq.positions` are already absolute WoW and `transform` is `IDENTITY` —
/// `bevy_to_wow(wow_to_bevy(p))` is an exact round-trip (a pure axis permutation with sign flips),
/// so the grid comes through bit-for-bit. For WMO liquid the positions are model-local and
/// `transform` is the building's MODF placement, so each vertex is carried local-WoW → local-Bevy →
/// world-Bevy → world-WoW. That transform is affine, so the grid is still a regular lattice on the
/// far side — which is what lets [`WaterChunkInfo`] invert a world XY straight to a cell.
fn wet_footprint(lq: &LiquidMesh, transform: &Transform, source: LiquidSource) -> WaterChunkInfo {
    // The grid is carried in WORLD WoW space (placement baked in) so every consumer — the swim
    // query's cell lookup, the height sample, the foam clip, the ambient loop — reads one set of
    // vertices in one frame of reference.
    let positions: Vec<[f32; 3]> = lq
        .positions
        .iter()
        .map(|&p| world_wow(transform, p))
        .collect();
    WaterChunkInfo::new(
        source,
        lq.kind,
        [lq.grid[0] as usize, lq.grid[1] as usize],
        positions,
        lq.wet.clone(),
    )
}

/// A liquid vertex's world-space WoW position: **local-WoW → local-Bevy → world-Bevy → world-WoW**.
/// The one place the placement transform is baked into raw liquid coords. For MCLQ water the
/// transform is `IDENTITY`, so this is `bevy_to_wow(wow_to_bevy(p))` = `p` exactly.
fn world_wow(transform: &Transform, local: [f32; 3]) -> [f32; 3] {
    bevy_to_wow(transform.transform_point(wow_to_bevy(local)))
}

/// Spawn a set of water surfaces — one flat mesh per [`LiquidMesh`], on its [`LiquidKind`]'s shared
/// animated material. Used by the `AdtTile` pipeline (`terrain_stream`). No-op when the client has no
/// data (`liquid_assets` absent) or a kind's frames didn't load. Spawned entities are pushed onto
/// `entities` so they despawn with their tile.
pub(crate) fn spawn_liquids<'a>(
    commands: &mut Commands,
    liquids: impl Iterator<Item = &'a LiquidMesh>,
    liquid_assets: Option<&LiquidAssets>,
    meshes: &mut Assets<Mesh>,
    entities: &mut Vec<Entity>,
) {
    let Some(liquid) = liquid_assets else {
        return;
    };
    for lq in liquids {
        let Some(material) = liquid.material(lq.kind) else {
            continue; // this kind's frames failed to load (warned at setup)
        };
        // The world-space liquid grid (MCLQ positions are already absolute WoW, so the IDENTITY
        // transform is a no-op round-trip).
        let info = wet_footprint(lq, &Transform::IDENTITY, LiquidSource::AdtChunk);
        let foam = !lq.kind.is_fullbright(); // white surf is a water thing
        entities.push(
            commands
                .spawn((
                    Mesh3d(meshes.add(liquid_bevy_mesh(lq))),
                    MeshMaterial3d(material),
                    Transform::IDENTITY,
                    LiquidSurface,
                    info,
                    LiquidSoundSource {
                        nibble: lq.sound_nibble,
                    },
                ))
                .id(),
        );
        // Foam is water-only; the cells it clips against ride `info`, not the marker.
        if foam {
            commands
                .entity(*entities.last().expect("just pushed"))
                .insert(FoamPatch);
        }
    }
}

/// Build the Bevy render mesh for one [`LiquidMesh`]: positions mapped WoW→Bevy (`lq.positions` are
/// raw WoW coords — absolute for MCLQ, WMO-model-local for WMO liquid), a flat up normal, the tiling
/// UVs, and the per-vertex swatch `V` packed into UV1.x for the shader's colour/opacity ramp. The
/// caller decides the surface's world placement via the spawned entity's `Transform` (`IDENTITY` for
/// absolute MCLQ water; the WMO placement transform for WMO liquid).
fn liquid_bevy_mesh(lq: &LiquidMesh) -> Mesh {
    let positions: Vec<[f32; 3]> = lq
        .positions
        .iter()
        .map(|p| wow_to_bevy(*p).to_array())
        .collect();
    let n = positions.len();
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    // Flat surface: WoW up (0,0,1) → Bevy up (0,1,0). The shader lights against this (rotated into
    // world by the entity transform) + the sun.
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; n]);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, lq.uvs.clone());
    // UV1.x carries the per-vertex swatch depth (0..1) for the shader's opacity ramp.
    let uv1: Vec<[f32; 2]> = lq.depths.iter().map(|&d| [d, 0.0]).collect();
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_1, uv1);
    mesh.insert_indices(Indices::U32(lq.indices.clone()));
    mesh
}

/// Spawn a WMO group's embedded liquid surfaces (Stormwind's canals + fountains, the Ironforge lava,
/// dungeon pools) at the building's placement `transform`, on the shared per-kind liquid material —
/// the same animated water render as MCLQ, but its geometry is WMO-model-local (built by
/// `benilla_formats::wmo_group_liquid_mesh`) so the placement transform lifts it into the world.
///
/// No-op when the client has no data (`liquid_assets` absent) or a kind's frames didn't load. Each
/// WATER surface also carries a world-space [`WaterChunkInfo`] + [`FoamPatch`] (both built by baking the
/// placement transform into the raw liquid coords, [`world_wow`]) so the whole water-interaction stack
/// sees WMO liquid exactly like MCLQ: swimming ([`crate::player::swim`]), the underwater murk
/// ([`detect_submersion`]), the wading splash/footstep sounds, AND the `CWater0Ripple` wade wake /
/// standing ring ([`crate::water_fx`], which builds each foam decal from the wet-cell lattice). The
/// foam's world-axis texgen + per-triangle overlap consume the transformed cells fine, so a rotated
/// canal's ring is still correctly world-oriented. Spawned entities are pushed onto `entities` so they
/// despawn with the placement.
pub(crate) fn spawn_wmo_liquids<'a>(
    commands: &mut Commands,
    liquids: impl Iterator<Item = &'a LiquidMesh>,
    liquid_assets: Option<&LiquidAssets>,
    meshes: &mut Assets<Mesh>,
    transform: Transform,
    entities: &mut Vec<Entity>,
) {
    let Some(liquid) = liquid_assets else {
        return;
    };
    for lq in liquids {
        let Some(material) = liquid.material(lq.kind) else {
            continue; // this kind's frames failed to load (warned at setup)
        };
        let surface = commands
            .spawn((
                Mesh3d(meshes.add(liquid_bevy_mesh(lq))),
                MeshMaterial3d(material),
                transform,
                LiquidSurface,
                // The ambient-loop source rides EVERY kind — the fullbright lava/slime hum too
                // (0506). It reads its geometry off the `WaterChunkInfo` inserted below.
                LiquidSoundSource {
                    nibble: lq.sound_nibble,
                },
            ))
            .id();
        // The swim/submersion grid rides EVERY kind, magma and slime included — that is what
        // makes Blackrock's lava and Undercity's slime swimmable instead of something you fall
        // through (decision 0634, bugs B24/B25). It used to be gated on `!is_fullbright()` because
        // `WaterChunkInfo` carried no kind, so tagging lava would have swum the player under a teal
        // *water* murk with white foam. The component carries [`LiquidKind`] now and the
        // water-flavoured consumers filter on it (`water_surface_at`, `detect_submersion`), so the
        // exclusion is no longer what keeps lava from looking like a lake.
        //
        // Lava/slime **damage** is still not modelled — a named gap, not a reason to keep the
        // geometry non-solid.
        commands
            .entity(surface)
            .insert(wet_footprint(lq, &transform, LiquidSource::WmoGroup));
        // Foam stays water-only: it is white surf, and there is no such thing on magma.
        if !lq.kind.is_fullbright() {
            commands.entity(surface).insert(FoamPatch);
        }
        entities.push(surface);
    }
}

/// Each kind's animated frame set: `(kind, XTextures subdir, file stem, frame count on disk)`.
/// Frames are `XTextures\<dir>\<stem>.<1..=count>.blp` (256² RGBA, RGB dark + alpha ripple).
const FRAME_SETS: &[(LiquidKind, &str, &str, u32)] = &[
    (LiquidKind::Still, "river", "lake_a", 30),
    (LiquidKind::Rapids, "river", "fast_a", 16),
    (LiquidKind::Ocean, "ocean", "ocean_h", 30),
    // WMO-liquid-only kinds (magma/slime carry no MCLQ data). Opaque + fullbright: the animated
    // texture IS the body colour (VERIFIED wow-re — magma vert-fill = constant 1.0, no depth LUT).
    (LiquidKind::Magma, "lava", "lava", 30),
    (LiquidKind::Slime, "slime", "slime", 30),
];

fn setup_liquid(
    mut commands: Commands,
    config: Option<Res<RenderConfig>>,
    world_assets: Option<ResMut<WorldAssets>>,
    lighting: Option<Res<WowLighting>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<LiquidMaterial>>,
) {
    let (Some(_config), Some(mut world_assets)) = (config, world_assets) else {
        return; // no client data → no terrain, so no water either
    };
    // Seed light/fog + water colours from the current light (or a sane default) so a surface renders
    // correctly on its first frame; `apply_wow_lighting` keeps these in sync afterward (same path as
    // terrain/WDL). Water colour = the per-kind Light.dbc close→far depth gradient (seeded below).
    let light = lighting
        .as_ref()
        .map(|l| l.terrain_uniforms(false))
        .unwrap_or_default();

    let mut assets = LiquidAssets::default();
    for &(kind, dir, stem, count) in FRAME_SETS {
        let Some((frames, frame_count)) =
            load_frame_array(&mut world_assets, &mut images, dir, stem, count)
        else {
            warn!("liquid: no frames for {stem} — {kind:?} water will not render");
            continue;
        };
        // Per-kind water-swatch SEED (frame 0): the Light.dbc water-row shallow→deep endpoints (river/lake
        // = IntBand 16/17, ocean = 14/15, RAW) + shallow alpha (river 0.5 / ocean 0.75; both reach deep =
        // 1.0). Daytime fallback mirroring `Atmosphere::DEFAULT`; `apply_wow_lighting` replaces it with the
        // live per-zone values on frame 1. The shader lerps both colour AND alpha by the same depth V.
        let (shallow, deep, shallow_a) = match kind {
            LiquidKind::Ocean => (
                [0.063, 0.294, 0.349],
                [0.0, 0.114, 0.161],
                OCEAN_SHALLOW_ALPHA,
            ),
            LiquidKind::Still | LiquidKind::Rapids => (
                [0.310, 0.365, 0.078],
                [0.200, 0.322, 0.333],
                RIVER_SHALLOW_ALPHA,
            ),
            // Magma/slime are fullbright (the shader takes the animated texture as the opaque body,
            // ignoring the swatch); these endpoints are unread but kept white/opaque for clarity.
            LiquidKind::Magma | LiquidKind::Slime => ([1.0, 1.0, 1.0], [1.0, 1.0, 1.0], 1.0),
        };
        let material = materials.add(ExtendedMaterial {
            base: StandardMaterial {
                // We do our own (WoW) lighting in the shader; blend + two-sided + depth-write-off
                // (Bevy's transparent pass) is exactly the verified MCLQ water render state.
                unlit: true,
                alpha_mode: AlphaMode::Blend,
                cull_mode: None,
                double_sided: true,
                ..default()
            },
            extension: LiquidExt {
                frames,
                light_ambient: light.light_ambient,
                light_diffuse: light.light_diffuse,
                light_sun: light.light_sun,
                light_spec: Vec4::new(
                    light.light_spec.x,
                    light.light_spec.y,
                    light.light_spec.z,
                    WATER_SHININESS,
                ),
                water_shallow: Vec4::new(shallow[0], shallow[1], shallow[2], shallow_a),
                water_deep: Vec4::new(deep[0], deep[1], deep[2], WATER_DEEP_ALPHA),
                fog_color: light.fog_color,
                fog_params: light.fog_params,
                // x = frame 0 (index driven by `animate_liquid`); y = frame count; z = the fullbright
                // flag (>0.5 ⇒ magma/slime: output the animated texture opaque, skip the swatch/lighting
                // — VERIFIED wow-re magma path); w unused.
                anim: Vec4::new(
                    0.0,
                    frame_count as f32,
                    if kind.is_fullbright() { 1.0 } else { 0.0 },
                    0.0,
                ),
            },
        });
        assets.materials.insert(
            kind,
            LiquidEntry {
                material,
                frame_count,
            },
        );
    }
    info!(
        "liquid: loaded {} water frame set(s)",
        assets.materials.len()
    );
    commands.insert_resource(assets);
}

/// Decode frames `1..=count` for a kind — each with its BLP **authored mip chain** — into one
/// repeating, mipmapped + anisotropic `texture_2d_array` (`assets::liquid_frame_array`; mips are what
/// stop the ripple aliasing into sparkle at distance). Stops at the first missing/non-square/
/// size-mismatched frame (the on-disk sets are contiguous 256² runs). Returns the image handle + the
/// number of frames actually loaded, or `None` if none decoded.
fn load_frame_array(
    world_assets: &mut WorldAssets,
    images: &mut Assets<Image>,
    dir: &str,
    stem: &str,
    count: u32,
) -> Option<(Handle<Image>, u32)> {
    let mut frames: Vec<BlpMipChain> = Vec::new();
    let mut size = 0u32;
    for i in 1..=count {
        let path = format!("XTextures\\{dir}\\{stem}.{i}.blp");
        let Ok(chain) = read_texture_mip_chain(&mut world_assets.chain.lock_recover(), &path)
        else {
            break;
        };
        if chain.width != chain.height {
            break; // water frames are square; bail rather than build a ragged array
        }
        if size == 0 {
            size = chain.width;
        } else if chain.width != size {
            break; // a frame at a different resolution can't share the array
        }
        frames.push(chain);
    }
    if frames.is_empty() {
        return None;
    }
    let loaded = frames.len() as u32;
    Some((images.add(liquid_frame_array(frames)), loaded))
}

/// Advance every liquid material's frame index at [`ANIM_FPS`] off Bevy **real** `Time` (wall-clock,
/// mirroring the reference's `GetTickCount`-driven cycler — NOT the day/night game clock). Writes
/// only on the [`ANIM_FPS`] tick edge: `Assets::get_mut` alone marks the asset Modified and feeds
/// the respecialization pipeline (the mark-changed scan + `Changed<Mesh3d>` sweeps) every frame —
/// the 0353 demand-price law; between ticks the frame index cannot have changed.
fn animate_liquid(
    time: Res<Time>,
    liquid: Option<Res<LiquidAssets>>,
    mut materials: ResMut<Assets<LiquidMaterial>>,
    mut last_ticks: Local<Option<u32>>,
) {
    let Some(liquid) = liquid else {
        return;
    };
    // Captures pin the cycler to frame 0: the wall-clock at screenshot time varies with load
    // times, so any framing with open water diffs differently run to run — the flake substrate's
    // baseline redesign caught (MAE 3.97 → 0.009 pinned; decision 0600). One clause, one frame.
    let ticks = if crate::capture::scenario_active() {
        0
    } else {
        (time.elapsed_secs() * ANIM_FPS) as u32
    };
    if *last_ticks == Some(ticks) {
        return;
    }
    *last_ticks = Some(ticks);
    for entry in liquid.materials.values() {
        if let Some(m) = materials.get_mut(&entry.material) {
            m.extension.anim.x = (ticks % entry.frame_count.max(1)) as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat 10×10 yd wet quad at WoW z = `z` — one cell, four corners.
    fn flat_quad(z: f32) -> LiquidMesh {
        LiquidMesh {
            grid: [2, 2],
            wet: vec![true],
            positions: vec![
                [0.0, 0.0, z],
                [10.0, 0.0, z],
                [0.0, 10.0, z],
                [10.0, 10.0, z],
            ],
            uvs: vec![[0.0, 0.0]; 4],
            depths: vec![1.0; 4],
            indices: vec![0, 1, 2, 1, 3, 2],
            sound_nibble: 0,
            kind: LiquidKind::Still,
        }
    }

    /// One `cols × rows` grid of `step`-yard cells with its corner at the origin, heights from
    /// `z(i, j)`, and the given per-cell wetness.
    fn grid_info(
        source: LiquidSource,
        kind: LiquidKind,
        cols: usize,
        rows: usize,
        step: f32,
        wet: Vec<bool>,
        z: impl Fn(usize, usize) -> f32,
    ) -> WaterChunkInfo {
        let mut positions = Vec::with_capacity(cols * rows);
        for j in 0..rows {
            for i in 0..cols {
                positions.push([i as f32 * step, j as f32 * step, z(i, j)]);
            }
        }
        WaterChunkInfo::new(source, kind, [cols, rows], positions, wet)
    }

    /// A flat one-cell surface at `z`, 10 yd square — the fixture for the tests that are about the
    /// delegation or the stacking rule, not about the height sample.
    fn flat_info(source: LiquidSource, kind: LiquidKind, z: f32) -> WaterChunkInfo {
        grid_info(source, kind, 2, 2, 10.0, vec![true], move |_, _| z)
    }

    /// MCLQ water passes `IDENTITY`: `bevy_to_wow(wow_to_bevy(p))` is a pure axis permutation with
    /// sign flips, so the footprint must equal the raw wet-vertex bounds exactly (bit-for-bit — the
    /// refactor that routed MCLQ through `wet_footprint` must not move a single lake edge).
    #[test]
    fn identity_footprint_is_the_raw_bounds() {
        let info = wet_footprint(
            &flat_quad(5.0),
            &Transform::IDENTITY,
            LiquidSource::AdtChunk,
        );
        assert_eq!((info.min_x, info.max_x), (0.0, 10.0));
        assert_eq!((info.min_y, info.max_y), (0.0, 10.0));
        assert_eq!(info.surface_z_at(5.0, 5.0), Some(5.0));
    }

    /// A WMO canal under a yaw-only building placement (spin about vertical + a world lift): the
    /// water plane stays LEVEL, so the sampled height must equal the local height plus the
    /// placement's vertical lift, for EVERY yaw — and the cell lookup must still find the quad's
    /// own centre after the spin, which is the property the world-space grid rests on. (Bevy +Y is
    /// up; a WoW z-lift is a Bevy +Y translate.)
    #[test]
    fn yaw_placement_keeps_the_surface_level() {
        let lift = 3.0_f32;
        for deg in [0.0_f32, 30.0, 90.0, 200.0, 355.0] {
            let transform = Transform {
                translation: Vec3::new(100.0, lift, -50.0), // Bevy +Y = WoW +Z lift
                rotation: Quat::from_rotation_y(deg.to_radians()), // yaw about vertical
                scale: Vec3::ONE,
            };
            let info = wet_footprint(&flat_quad(5.0), &transform, LiquidSource::WmoGroup);
            let centre = bevy_to_wow(transform.transform_point(wow_to_bevy([5.0, 5.0, 5.0])));
            let z = info
                .surface_z_at(centre[0], centre[1])
                .unwrap_or_else(|| panic!("yaw {deg}°: centre {centre:?} off the grid"));
            assert!(
                (z - (5.0 + lift)).abs() < 1e-3,
                "yaw {deg}°: surface not level (got {z})"
            );
        }
    }

    /// **The height rule** (director repro, Blackrock's lava and Felwood's river). A liquid grid is
    /// a heightfield: the surface at an XY is the BILINEAR of its cell's four corners, never the
    /// chunk's highest vertex. Over a cell rising 0 → 8 yd, the maximum is 8 everywhere while the
    /// true surface runs the full ramp — which is exactly how a spot metres under the lava read as
    /// metres over it.
    #[test]
    fn the_surface_is_the_bilinear_of_its_cell_not_the_chunk_maximum() {
        // One 10 yd cell; corner heights 0 / 4 (+x) / 2 (+y) / 8 (+x+y) — a genuine twist, so a
        // plane fit through any three corners cannot reproduce the fourth.
        let info = grid_info(
            LiquidSource::AdtChunk,
            LiquidKind::Still,
            2,
            2,
            10.0,
            vec![true],
            |i, j| match (i, j) {
                (0, 0) => 0.0,
                (1, 0) => 4.0,
                (0, 1) => 2.0,
                _ => 8.0,
            },
        );
        for (x, y, want) in [
            (0.0, 0.0, 0.0),  // the corners are exact
            (10.0, 0.0, 4.0), // …including the far edges, which the last cell owns
            (0.0, 10.0, 2.0),
            (10.0, 10.0, 8.0),
            (5.0, 0.0, 2.0), // midway along the near edge
            (5.0, 5.0, 3.5), // the centre: (0 + 4 + 2 + 8)/4
            // An interior sample: lerp(lerp(0,4,.25), lerp(2,8,.25), .75) = lerp(1.0, 3.5, .75).
            (2.5, 7.5, 2.875),
        ] {
            let got = info
                .surface_z_at(x, y)
                .expect("wet everywhere on this cell");
            assert!(
                (got - want).abs() < 1e-4,
                "bilinear at ({x}, {y}): got {got}, want {want}"
            );
        }
        // The old rule — the chunk's highest wet vertex — would answer 8.0 at every one of those.
        assert!(info.surface_z_at(0.0, 0.0).unwrap() < 8.0);
    }

    /// The delegation, which is the whole "swim in air" fix: a tunnel bored under a lake sits inside
    /// the lake's flat XY footprint (footprints have no floor), so before the source split it read as
    /// submerged. Inside a WMO only WMO liquid answers; outdoors only ADT liquid does.
    #[test]
    fn indoors_and_outdoors_see_different_liquid() {
        let lake = flat_info(LiquidSource::AdtChunk, LiquidKind::Still, 50.0);
        let canal = flat_info(LiquidSource::WmoGroup, LiquidKind::Still, 8.0);
        let all = [&lake, &canal];
        let deep_under = [5.0, 5.0, 0.0];

        // Standing in the tunnel: the lake 50 yd overhead must NOT answer.
        let inside = liquid_at(all.into_iter(), deep_under, Some(true)).unwrap();
        assert_eq!(
            inside.surface_z, 8.0,
            "indoors must read the WMO's own liquid"
        );
        // Out on the surface: the ADT lake answers and the building's canal does not.
        let outside = liquid_at(all.into_iter(), deep_under, Some(false)).unwrap();
        assert_eq!(outside.surface_z, 50.0);
        // No claim (remote units): both sources answer — the documented pre-0634 behaviour.
        assert!(liquid_at(all.into_iter(), deep_under, None).is_some());
        // Outside the XY footprint nothing answers, either way.
        assert!(liquid_at(all.into_iter(), [99.0, 99.0, 0.0], Some(false)).is_none());
    }

    /// Stacked surfaces resolve to the LOWEST, not to whichever the iterator yields first — the old
    /// `.next()` made the answer depend on spawn order.
    #[test]
    fn stacked_surfaces_take_the_lowest() {
        let upper = flat_info(LiquidSource::WmoGroup, LiquidKind::Still, 40.0);
        let lower = flat_info(LiquidSource::WmoGroup, LiquidKind::Still, 4.0);
        for order in [[&upper, &lower], [&lower, &upper]] {
            let hit = liquid_at(order.into_iter(), [5.0, 5.0, 0.0], Some(true)).unwrap();
            assert_eq!(hit.surface_z, 4.0);
        }
    }

    /// Lava and slime ARE swimmable (`liquid_at`) but must never drive the water-flavoured
    /// consumers (`water_surface_at` → the teal murk, foam, the splash). B24/B25 vs the teal-lava
    /// regression the old fullbright exclusion was guarding against — both, at once.
    #[test]
    fn fullbright_kinds_swim_but_are_not_water() {
        let lava = flat_info(LiquidSource::WmoGroup, LiquidKind::Magma, 6.0);
        let here = [5.0, 5.0, 0.0];
        let hit = liquid_at([&lava].into_iter(), here, Some(true)).expect("lava is a swim volume");
        assert_eq!(hit.kind, LiquidKind::Magma);
        assert!(
            water_surface_at([&lava].into_iter(), here, Some(true)).is_none(),
            "magma must not read as water"
        );
    }

    /// **The canal-tunnel bug** (director repro at `-8889.49, 765.26, 93.38`, `/liquid` output:
    /// one candidate, `xy [-8927..-8832, 688..768]`, surface +2.09 over the feet). A liquid grid is
    /// sparse — its bounding box spans dry ground the wet cells never cover — so containment must
    /// test the CELLS. Bounding-box containment is what kept the Stormwind canal claiming the dry
    /// mage-district tunnel through the whole of 0634.
    #[test]
    fn a_dry_spot_inside_the_bounding_box_is_not_liquid() {
        // Three cells in a row; the MIDDLE one is a hole, so the box spans dry ground between two
        // wet halves — the canal-either-side-of-a-tunnel shape.
        let info = grid_info(
            LiquidSource::WmoGroup,
            LiquidKind::Still,
            4,
            2,
            10.0,
            vec![true, false, true],
            |_, _| 5.0,
        );
        assert!(info.contains(15.0, 5.0), "the box does span the dry middle");
        assert!(
            info.surface_z_at(5.0, 5.0).is_some() && info.surface_z_at(25.0, 5.0).is_some(),
            "over the wet cells — must be liquid"
        );
        assert!(
            info.surface_z_at(15.0, 5.0).is_none(),
            "inside the box but over the HOLE — must NOT be liquid (the canal tunnel)"
        );
        // And the query agrees, which is what actually decides swimming.
        assert!(liquid_at([&info].into_iter(), [5.0, 5.0, 0.0], Some(true)).is_some());
        assert!(liquid_at([&info].into_iter(), [15.0, 5.0, 0.0], Some(true)).is_none());
    }

    /// A grid we cannot invert (here a plane stood on edge, so it projects to a line in XY) falls
    /// back to its bounds and its highest wet vertex, rather than reporting the whole box dry — a
    /// wrong "no liquid" is a player falling through a lake, the strictly worse failure.
    #[test]
    fn a_degenerate_grid_falls_back_to_the_bounds() {
        let info = WaterChunkInfo::new(
            LiquidSource::AdtChunk,
            LiquidKind::Still,
            [2, 2],
            vec![
                [0.0, 0.0, 0.0],
                [0.0, 0.0, 10.0], // zero XY extent along i ⇒ the basis has no area
                [0.0, 10.0, 0.0],
                [0.0, 10.0, 10.0],
            ],
            vec![true],
        );
        assert_eq!(info.surface_z_at(0.0, 5.0), Some(10.0));
        assert_eq!(info.surface_z_at(50.0, 5.0), None, "still bounded in XY");
    }

    /// A malformed grid (dimensions that don't match the arrays) claims nothing at all — no bounds,
    /// so no surface. Better a liquid that isn't there than one that swallows the map.
    ///
    /// It must also be *inert*, not merely unclaimed: every walker over the grid — the foam cell
    /// walk, the `/liquid` range — indexes `positions` from the declared dimensions, so a grid that
    /// kept 9×9 dimensions over a 4-vertex array would read off the end. The constructor normalizes
    /// it to empty instead of leaving each walker to re-check.
    #[test]
    fn a_malformed_grid_claims_nothing_and_walks_nothing() {
        let info = WaterChunkInfo::new(
            LiquidSource::AdtChunk,
            LiquidKind::Still,
            [9, 9],
            vec![[0.0, 0.0, 5.0]; 4], // 4 positions for an 81-vertex grid
            vec![true; 64],
        );
        assert_eq!(info.surface_z_at(0.0, 0.0), None);
        let mut cells = 0;
        info.for_each_wet_cell(|_| cells += 1);
        assert_eq!(cells, 0, "nothing to walk, and no panic walking it");
        assert!(
            describe_at([&info].into_iter(), [0.0, 0.0, 0.0]).is_empty(),
            "and `/liquid` lists no candidate for it"
        );
    }
}

/// The height rule against the **real client data** at the two positions the director reported —
/// Blackrock Mountain's lava (a WMO's 55×82 MLIQ under a rotated placement) and Felwood's Felfire
/// Hill river (one MCNK's 9×9 MCLQ). Both read as "swimming" while the liquid was visibly below the
/// feet; both are the chunk-maximum height rule, and nothing but real grids reproduces them.
///
/// Skips when the 1.12.1 client isn't present (the repo never carries Blizzard data).
#[cfg(test)]
mod real_data {
    use super::*;
    use benilla_assets::coords::placement_rotation;
    use benilla_formats::{parse_wmo_root, wmo_group_liquid_mesh};

    /// Every loaded liquid surface covering `wow`'s tile neighbourhood on `map`, world-placed —
    /// the ADT's own MCLQ chunks and every WMO placement's MLIQ groups, i.e. the same candidate set
    /// the running client's `WaterChunkInfo` query sees.
    fn surfaces_near(map: &str, wow: [f32; 3]) -> Vec<WaterChunkInfo> {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            return Vec::new();
        }
        let mut chain = benilla_formats::open_chain(&data).expect("open chain");
        let (cx, cy) = benilla_formats::world_to_tile(wow[0], wow[1]);
        let mut out = Vec::new();
        // A building as big as Blackrock is placed in every tile it straddles, so its MODF may sit
        // in a neighbour of the tile the position falls in.
        for dx in -1i32..=1 {
            for dy in -1i32..=1 {
                let (tx, ty) = ((cx as i32 + dx) as u32, (cy as i32 + dy) as u32);
                let Ok(tile) = benilla_formats::load_tile_mesh(&mut chain, map, tx, ty) else {
                    continue;
                };
                for lq in tile.chunks.iter().filter_map(|c| c.liquid.as_ref()) {
                    out.push(wet_footprint(
                        lq,
                        &Transform::IDENTITY,
                        LiquidSource::AdtChunk,
                    ));
                }
                for w in &tile.wmos {
                    let transform = Transform {
                        translation: wow_to_bevy(w.position),
                        rotation: placement_rotation(w.rotation),
                        scale: Vec3::ONE,
                    };
                    let root_path = w.model.to_ascii_lowercase();
                    let Ok(bytes) = chain.read_file(&root_path) else {
                        continue;
                    };
                    let Ok(root) = parse_wmo_root(&bytes) else {
                        continue;
                    };
                    let stem = root_path.strip_suffix(".wmo").unwrap_or(&root_path);
                    for gi in 0..root.group_count() {
                        let Ok(gb) = chain.read_file(&format!("{stem}_{gi:03}.wmo")) else {
                            continue;
                        };
                        if let Some(lq) = wmo_group_liquid_mesh(&gb) {
                            out.push(wet_footprint(
                                lq_ref(&lq),
                                &transform,
                                LiquidSource::WmoGroup,
                            ));
                        }
                    }
                }
            }
        }
        out
    }

    fn lq_ref(lq: &LiquidMesh) -> &LiquidMesh {
        lq
    }

    /// The verdict at a position, and the highest wet vertex among the surfaces that claim it —
    /// i.e. what the query answers now, beside what the chunk-maximum rule used to answer.
    fn verdict(map: &str, wow: [f32; 3], indoors: bool) -> Option<(f32, f32)> {
        let all = surfaces_near(map, wow);
        if all.is_empty() {
            return None; // no client data
        }
        let hit = liquid_at(all.iter(), wow, Some(indoors))?;
        let old = all
            .iter()
            .filter(|w| w.surface_z_at(wow[0], wow[1]).is_some())
            .map(|w| w.grid.fallback_z)
            .fold(f32::MIN, f32::max);
        Some((hit.surface_z, old))
    }

    /// **Blackrock Mountain's lava** at the director's `.go xyz -7531.21 -1123.64 172.58` (indoors,
    /// `blackrock.wmo` group 038 — a 55×82 magma grid running 167.29 → 175.00 under a ~7° yaw
    /// placement). The old chunk-maximum answered 175.00, i.e. 2.42 yd OVER the feet and well past
    /// the 1.52 yd swim line, on a staircase whose lava is metres below.
    #[test]
    fn blackrock_lava_is_below_the_feet_not_above_it() {
        let feet = [-7531.21_f32, -1123.64, 172.58];
        let Some((surface, old_max)) = verdict("Azeroth", feet, true) else {
            eprintln!("skipping: no WoW client data");
            return;
        };
        assert!(
            (old_max - 175.00).abs() < 0.05,
            "the chunk maximum this replaces (got {old_max})"
        );
        assert!(
            (surface - 168.45).abs() < 0.05,
            "the lava under the feet is the cell's own height (got {surface})"
        );
        assert!(
            surface < feet[2],
            "surface {surface} must be UNDER the feet {} — standing on the stairs, not swimming",
            feet[2]
        );
    }

    /// **Felfire Hill's river** at the director's `.go xyz 1983.97 -2875.84 98.00` (outdoors, one
    /// MCNK's 9×9 MCLQ falling 95.78 → 99.56 across the chunk). The old chunk-maximum answered
    /// 99.56 — 1.56 yd over the feet, just past the 1.52 yd swim line — while the player stands on
    /// the bank with the water at their soles.
    #[test]
    fn felfire_hill_river_does_not_swim_on_the_bank() {
        let feet = [1983.97_f32, -2875.84, 98.00];
        let Some((surface, old_max)) = verdict("Kalimdor", feet, false) else {
            eprintln!("skipping: no WoW client data");
            return;
        };
        assert!(
            (old_max - 99.56).abs() < 0.05,
            "the chunk maximum this replaces (got {old_max})"
        );
        assert!(
            surface < feet[2],
            "surface {surface} must be UNDER the feet {} — standing on the bank",
            feet[2]
        );
        assert!(
            (feet[2] - surface) < 1.0,
            "…but only just: the water is at the player's soles (got {surface})"
        );
    }

    /// **The gradient the swim law is sized against** (decision 0644): Felwood's Felfire Hill
    /// channel, along the run the live probe swam. A liquid surface is a heightfield, and *how far
    /// from flat* is exactly what decides whether the swim latch's 1/36-yd hysteresis band can
    /// absorb travelling along it — so the slope `player::swim`'s regression test drives is pinned
    /// here against the shipped ADT instead of living as a constant someone can only take on faith.
    #[test]
    fn the_felfire_channel_falls_about_a_tenth_of_a_yard_per_yard() {
        let (downstream, upstream) = ([1953.97_f32, -2866.84, 0.0], [2013.97_f32, -2866.84, 0.0]);
        let all = surfaces_near("Kalimdor", downstream);
        if all.is_empty() {
            eprintln!("skipping: no WoW client data");
            return;
        }
        let z = |at: [f32; 3]| {
            liquid_at(all.iter(), at, Some(false))
                .unwrap_or_else(|| panic!("no river at {at:?}"))
                .surface_z
        };
        let slope = (z(upstream) - z(downstream)) / (upstream[0] - downstream[0]);
        assert!(
            (slope - 0.099).abs() < 0.005,
            "the channel's gradient over 60 yd (got {slope})"
        );
    }
}
