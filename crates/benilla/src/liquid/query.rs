//! **Where the liquid is, and whether you are in it** — the geometry side of the liquid subsystem,
//! with no render glue at all.
//!
//! Every spawned surface publishes its grid in world WoW space ([`WaterChunkInfo`], built by
//! `super::surface::wet_footprint`); everything that asks a *position* question — swimming, the
//! wade splash and footstep depth, the foam lattice, the ambient loops, and the submerged
//! atmosphere — reads it through [`liquid_at`] and its filtered siblings. [`detect_submersion`] is
//! the one cross-feed out of the subsystem: it publishes WHICH liquid the camera eye stands in
//! ([`Underwater`]), and `lighting::update_time_lighting` selects the whole submerged atmosphere
//! from that.

use bevy::prelude::*;

use crate::player::WorldCamera;
use crate::wmo_portal::{CameraInteriorClaim, PlayerWmoRoom, UnitWmoRoom, WmoRoom};
use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use benilla_formats::{LiquidKind, LiquidMesh};

/// The **camera eye's** liquid claim — the reference's `[0xc7b748]` branch in the environment probe
/// `0x6809c0`: a containing map-object selects its MLIQ, otherwise the ADT liquid answers.
pub(crate) fn camera_claim(claim: &CameraInteriorClaim) -> LiquidClaim {
    match claim.0 {
        Some(c) => LiquidClaim::Inside(c.room),
        None => LiquidClaim::Outdoors,
    }
}

/// The **player's** liquid claim, from the interior down-ray `wmo_portal` already runs each frame.
pub(crate) fn player_claim(room: &PlayerWmoRoom) -> LiquidClaim {
    match room.0 {
        Some(room) => LiquidClaim::Inside(room),
        None => LiquidClaim::Outdoors,
    }
}

/// A **remote unit's** liquid claim, from its own per-unit room. The component is absent only on a
/// unit's first frame, before `wmo_portal::track_unit_interiors` has reached it — that, and only
/// that, is [`LiquidClaim::Unknown`].
pub(crate) fn unit_claim(room: Option<&UnitWmoRoom>) -> LiquidClaim {
    match room.map(UnitWmoRoom::room) {
        Some(Some(room)) => LiquidClaim::Inside(room),
        Some(None) => LiquidClaim::Outdoors,
        None => LiquidClaim::Unknown,
    }
}

/// What the camera eye is currently submerged in — **which liquid, not merely whether**. Set by
/// [`detect_submersion`]; read by `lighting::update_time_lighting`, which selects the atmosphere from
/// it so the whole scene (fog colour + distances, ambient, diffuse, clear colour) becomes the
/// submerged one (VERIFIED apitrace WoW.18 — the murk is fog + light-tint, no overlay quad). Two
/// clocks aside, this is the one cross-feed from the liquid subsystem into lighting.
///
/// **The kind is load-bearing, not decoration.** Water and ocean read the *zone's* underwater
/// `LightParams` slot; magma and slime read fixed global rows instead, zone-independent
/// (byte-VERIFIED `0x6d2371` — see `benilla_formats::Submersion`). Carrying a bare `bool` here is
/// what left lava and slime with no submerged view at all: they were excluded from the flag outright,
/// because one bool could only mean "the water murk", and turning the Great Forge teal was worse than
/// showing nothing.
#[derive(Resource, Default)]
pub(crate) struct Underwater(pub(crate) benilla_formats::Submersion);

/// Where a liquid surface came from, and — for WMO liquid — **whose room it is**: the
/// **delegation + scope key** for [`liquid_at`].
///
/// The reference's liquid query is context-aware: terrain's `0x69b6d0` **delegates the WMO case out**
/// via `0x69b520` (wow-re `terrain/scratch/class-batch3.md`), which transforms the query point into
/// each placed map-object's own space before any MLIQ is sampled; and the per-frame camera
/// environment probe `0x6809c0` samples the **current WMO group's** MLIQ (`0x6b9f10`) when
/// `[0xc7b748]` names a containing map-object, else the ADT query `0x6723d0 → 0x69b6d0`
/// (wow-re `terrain/scratch/fog-env-state.md` §1, `models/scratch/wmo-lit-selector.md` §3.4).
///
/// Without the source split, a tunnel bored under a lake inherits the lake: an ADT footprint is a
/// flat XY rectangle with no floor, so every position beneath it reads as submerged — the "swim in
/// air" family (decision 0634). Without the **owner**, the same holds one level up: a WMO pool
/// claims every position under its XY in *every other building on the map*, at any depth. That is
/// decision 0696 — the Uldaman entrance read as submerged under a mushroom cave's pool 186 yd
/// overhead, in a building the player was 191 yd below and had never entered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LiquidSource {
    /// An ADT map-chunk surface (MCLQ) — the outdoor world's lakes, rivers, coast.
    AdtChunk,
    /// A WMO group's embedded surface (MLIQ) — canals, fountains, the Great Forge lava,
    /// Undercity's slime — tagged with the [`WmoRoom`] that owns it.
    ///
    /// `None` only for a placement that spawned no [`WmoPortalInstance`](crate::wmo_portal::WmoPortalInstance)
    /// — a portal-less building with no `WMOAreaTable` identity. Nothing can claim to be *inside*
    /// such a placement (the interior trackers skip it), so its pool answers no one. That is the
    /// pre-existing behaviour made explicit rather than a new gap: an unowned pool was already
    /// unreachable, it just used to be unreachable by accident.
    WmoGroup(Option<WmoRoom>),
}

/// Whose liquid answers for one subject at one position — the query's context, and the whole of the
/// "swim in air" family's fix.
///
/// Every subject that asks the liquid a question carries one: the player (from
/// [`PlayerWmoRoom`](crate::wmo_portal::PlayerWmoRoom)), the camera eye (from
/// [`CameraInteriorClaim`](crate::wmo_portal::CameraInteriorClaim) — the reference's `[0xc7b748]`),
/// and every remote unit (its own per-unit claim). Before 0696 the parameter was a bare
/// `Option<bool>`, which could say *that* a subject was indoors but never *where* — so "indoors"
/// admitted every MLIQ surface in the world.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LiquidClaim {
    /// The subject stands in the open world: the ADT's MCLQ answers, nothing else.
    Outdoors,
    /// The subject stands in this placed building: **that placement's** MLIQ answers, nothing else.
    Inside(WmoRoom),
    /// No interior claim has been computed for this subject yet — its first frame, before the
    /// tracker has run. Both sources answer (the pre-0634 behaviour), which is the wrong answer for
    /// exactly one frame rather than a silent guess at the right one.
    Unknown,
}

/// One liquid surface as the submersion/swim/foam/sound queries see it: its **grid**, in world WoW
/// space with the placement transform already baked in, plus the XY bounds, which file it came
/// from, and which liquid it is. Attached to each [`LiquidSurface`]; despawns with its tile, so no
/// manual lifecycle.
///
/// Named `Water*` from when only water carried one. It now rides **every** kind — magma and slime
/// included, which is what makes Blackrock's lava and Undercity's slime swimmable at all (decision
/// 0634). Consumers that are specifically about *water* (the surface swatch, foam, the wade splash and
/// footstep depth) filter on [`Self::kind`]; the swim mode does not, because you swim in lava too —
/// and neither does the **submerged atmosphere**, which is per-kind rather than water-only
/// ([`Underwater`]).
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
    /// The **chunk-maximum** height — the single highest wet vertex of the whole grid.
    ///
    /// This is the rule the per-cell sample REPLACED (decision 0634): it answered "swimming" from
    /// anywhere under a surface's XY box, which is how Blackrock's staircase read as submerged with
    /// its lava metres below. Nothing live reads it; `super::real_data` does, to assert the number
    /// the fix moved away from — a regression test whose baseline would otherwise be a comment.
    #[cfg(test)]
    pub(super) fn chunk_max_z(&self) -> f32 {
        self.grid.fallback_z
    }

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

    /// Does this surface answer for a subject holding `claim`? — the delegation, in one place.
    ///
    /// An [`Inside`](LiquidClaim::Inside) subject reads its **own placement's** MLIQ and nothing
    /// else: not the ADT liquid overhead (the 0634 delegation), and not another building's pool
    /// (the 0696 owner scoping). An [`Outdoors`](LiquidClaim::Outdoors) subject reads only the ADT.
    /// [`Unknown`](LiquidClaim::Unknown) is the un-classified first frame and admits both.
    fn answers(&self, claim: LiquidClaim) -> bool {
        match (claim, self.source) {
            (LiquidClaim::Unknown, _) => true,
            (LiquidClaim::Outdoors, LiquidSource::AdtChunk) => true,
            (LiquidClaim::Outdoors, LiquidSource::WmoGroup(_)) => false,
            (LiquidClaim::Inside(_), LiquidSource::AdtChunk) => false,
            (LiquidClaim::Inside(room), LiquidSource::WmoGroup(owner)) => {
                owner.is_some_and(|o| o.instance == room.instance)
            }
        }
    }

    /// The room this surface belongs to (`None` for ADT liquid and for an unowned WMO placement) —
    /// the `/liquid` instrument's attribution column, which is what turns "some pool is claiming
    /// me" into "*that* building's pool is claiming me".
    fn owner(&self) -> Option<WmoRoom> {
        match self.source {
            LiquidSource::AdtChunk => None,
            LiquidSource::WmoGroup(owner) => owner,
        }
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

/// A liquid surface's [`WaterChunkInfo`] — its grid lifted into **world** WoW space, with
/// `transform` mapping the mesh's local space into the world.
///
/// For MCLQ water `lq.positions` are already absolute WoW and `transform` is `IDENTITY` —
/// `bevy_to_wow(wow_to_bevy(p))` is an exact round-trip (a pure axis permutation with sign flips),
/// so the grid comes through bit-for-bit. For WMO liquid the positions are model-local and
/// `transform` is the building's MODF placement, so each vertex is carried local-WoW → local-Bevy →
/// world-Bevy → world-WoW. That transform is affine, so the grid is still a regular lattice on the
/// far side — which is what lets [`WaterChunkInfo`] invert a world XY straight to a cell.
pub(super) fn wet_footprint(
    lq: &LiquidMesh,
    transform: &Transform,
    source: LiquidSource,
) -> WaterChunkInfo {
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
/// **`claim` is the whole delegation** ([`LiquidClaim`], [`LiquidSource`]):
/// [`Inside`](LiquidClaim::Inside) answers from **that placement's own** MLIQ surfaces only,
/// [`Outdoors`](LiquidClaim::Outdoors) from the ADT's MCLQ ones only. This mirrors the reference,
/// whose terrain query delegates the WMO case out rather than unioning the two — and whose WMO leg
/// transforms the point into each map-object's own space before sampling anything, so a building the
/// subject is not in cannot answer for it.
///
/// It is the fix for "swim in air" at both levels. A footprint is a flat XY rectangle with **no
/// floor**: before the source split the Stormwind canal claimed the mage-district tunnel beneath it
/// (0634); before the *owner* scoping a mushroom cave's pool claimed the Uldaman entrance 186 yd
/// below it, in a building the player had never entered (0696).
///
/// Each candidate answers with its height **at this XY** ([`WaterChunkInfo::surface_z_at`]), and
/// among them the **lowest wins**. Overlapping footprints used to resolve by iteration order
/// (`.next()`) — an arbitrary pick that made the answer depend on spawn order. The lowest is the one
/// whose volume you are actually in when standing between two stacked surfaces.
///
/// **Still open — the bound BELOW a surface within one room.** Owner scoping bounds a pool to its
/// building; it does not bound it to its *storey*. A multi-floor WMO whose upper room holds water
/// would still claim the room beneath it, and whether the reference scopes to the containing GROUP
/// (and whether a WMO hit with no liquid suppresses the ADT leg) is a live wow-re dispatch — 0634's
/// deferral, re-asked with the group question attached in 0696. Do not invent the rule here.
pub(crate) fn liquid_at<'a>(
    liquids: impl Iterator<Item = &'a WaterChunkInfo>,
    wow: [f32; 3],
    claim: LiquidClaim,
) -> Option<LiquidHit> {
    liquids
        .filter(|w| w.answers(claim))
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
///
/// And each line names its **owner** — which placement + group a WMO pool belongs to, and whether
/// the subject's own claim matches it (`◀ YOURS` / `other room`). Without that column the Uldaman
/// report ("VERDICT Still, +185.91 over feet, WmoGroup WET-CELL") was indistinguishable from a
/// legitimate pool sampled at the wrong height: nothing on the line said the surface belonged to a
/// *different building* — which was the entire bug (0696).
pub(crate) fn describe_at<'a>(
    liquids: impl Iterator<Item = &'a WaterChunkInfo>,
    wow: [f32; 3],
    claim: LiquidClaim,
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
            let owner = match (w.source, w.owner()) {
                (LiquidSource::AdtChunk, _) => "AdtChunk".to_string(),
                (LiquidSource::WmoGroup(_), Some(o)) => {
                    format!("WmoGroup {:?} g{}", o.instance, o.group)
                }
                (LiquidSource::WmoGroup(_), None) => "WmoGroup (unowned)".to_string(),
            };
            (
                z.unwrap_or(hi),
                format!(
                    "{owner} {} {:?} {here}  grid z [{lo:.2}..{hi:.2}]  xy [{:.0}..{:.0}, {:.0}..{:.0}]",
                    if w.answers(claim) { "◀ YOURS" } else { "· other room" },
                    w.kind,
                    w.min_x,
                    w.max_x,
                    w.min_y,
                    w.max_y,
                ),
            )
        })
        .collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out.into_iter().map(|(_, line)| line).collect()
}

/// [`liquid_at`] restricted to **water** kinds — the query for the consumers that are about water
/// specifically (the wade splash, footstep depth, the remote-motion spline's depth), which must not
/// fire in the Great Forge's lava or Undercity's slime. Swim mode deliberately does NOT use this one.
///
/// The **submerged atmosphere no longer routes through here either**: it is per-kind (magma and slime
/// have their own fixed `LightParams` rows), so it reads [`Underwater`] instead. This wrapper's docs
/// used to name "the teal murk" as its headline consumer, and that was exactly the assumption that
/// left lava and slime with no submerged view at all.
pub(crate) fn water_surface_at<'a>(
    water: impl Iterator<Item = &'a WaterChunkInfo>,
    wow: [f32; 3],
    claim: LiquidClaim,
) -> Option<f32> {
    liquid_at(water.filter(|w| !w.kind.is_fullbright()), wow, claim).map(|h| h.surface_z)
}

// The **wade ceiling** used to live here, as `WADE_MAX = 2.0` — a flat proxy for a boundary B7
// (decision 0226) had already shown to be `0.75·collisionHeight`, kept because the per-unit height
// it needed was 0464's un-plumbed `CreatureModelData.collisionHeight`. Decision 0645 plumbed it, so
// the proxy is gone and there is no wade constant to re-import: wading is *the complement of
// swimming*, one number, and its one spelling is `player::swim_enter_depth(h)`. A human's line
// moved 2.0 → 1.52 yd, a murloc's far shallower.

/// Eye-submersion accept margin for the **water** kinds (VERIFIED `FUN_0069b6d0`: `eye.z < surface +
/// 0.01`, the f32 `0x3c23d70a` at `0x8029d0`, strict `<`). The WMO magma/slime compare carries no
/// epsilon at all, so [`detect_submersion`] applies this per kind.
const SUBMERSION_EPS: f32 = 0.01;

/// Which submerged atmosphere a liquid kind selects. Water/ocean/rapids take the zone's own
/// underwater `LightParams` slot; magma and slime take fixed global rows instead, zone-independent
/// (see [`benilla_formats::Submersion`]).
fn submersion_of(kind: LiquidKind) -> benilla_formats::Submersion {
    use benilla_formats::Submersion;
    match kind {
        LiquidKind::Still | LiquidKind::Rapids | LiquidKind::Ocean => Submersion::Water,
        LiquidKind::Magma => Submersion::Magma,
        LiquidKind::Slime => Submersion::Slime,
    }
}

/// Set [`Underwater`] from the camera vs the liquid surfaces: the eye is submerged if it's over a wet
/// cell and below that cell's surface (`FUN_0069b6d0` — its 9×9 bilinear sample is now what
/// [`WaterChunkInfo::surface_z_at`] does, so this is the binary's own rule and no longer a per-chunk
/// flat approximation of it). One pass over the loaded surfaces (a few hundred, cheap).
///
/// **The candidate set is the camera's own room, not the world.** The reference's per-frame
/// environment probe `0x6809c0` reads the render eye `[0xc7cf20/24/28]` and then queries **one**
/// source: the current WMO group's MLIQ (`0x6b9f10`) when `[0xc7b748]` names a containing
/// map-object, otherwise the ADT liquid (`0x6723d0 → 0x69b6d0`) — VERIFIED, wow-re
/// `terrain/scratch/fog-env-state.md` §1 (the two `[0xc7f288]` writers) and
/// `ui/scratch/camera-arm-liquid-blind.md` §2's band census, which names both call sites inside
/// `[0x6809c0, 0x680b90)`. We take the eye's claim from [`CameraInteriorClaim`], which
/// `wmo_portal::compute_wmo_pvs` publishes off the very down-ray that writes `[0xc7b748]`.
///
/// This probe used to consult **every** loaded surface with no delegation at all, while the player's
/// query had delegated since 0634 — so the two disagreed by construction, and the screen took the
/// underwater filter in rooms the player was demonstrably dry in. Standing in Undercity's Rogues'
/// Quarter at `(1414.08, 53.00, -62.26)`, 95 yd of rock below Tirisfal's ADT water at z 32.93, the
/// player read "not in liquid" and the whole scene still rendered green (decision 0696).
///
/// **Every** liquid counts, including magma and slime — they are not the water murk, they are their
/// own atmospheres (see [`Underwater`]). The eye's own **`+0.01` accept margin is water's alone**:
/// byte-VERIFIED (`0x8029d0` = `0x3c23d70a` = 0.01, strict `<`, at `0x69ba23`) that the WMO
/// magma/slime compare is a bare `z < h` with **no epsilon**, so the margin is applied per kind
/// rather than uniformly.
///
/// Where surfaces stack, the **deepest submerging** one wins: standing in the Great Forge's lava
/// under an unrelated water footprint should read as lava, and a `.any()` over an unordered query
/// would otherwise answer with whichever entity the ECS happened to yield first.
pub(super) fn detect_submersion(
    mut underwater: ResMut<Underwater>,
    camera: Query<&Transform, With<WorldCamera>>,
    water: Query<&WaterChunkInfo>,
    eye_claim: Res<crate::wmo_portal::CameraInteriorClaim>,
    time: Res<Time>,
    mut last_dump: Local<Option<u32>>,
) {
    let Ok(cam) = camera.single() else {
        return;
    };
    let claim = camera_claim(&eye_claim);
    let eye = bevy_to_wow(cam.translation); // [x, y, z] WoW yards
    underwater.0 = water
        .iter()
        .filter(|w| w.answers(claim))
        .filter_map(|w| {
            let z = w.surface_z_at(eye[0], eye[1])?;
            let eps = if w.kind.is_fullbright() {
                0.0
            } else {
                SUBMERSION_EPS
            };
            (eye[2] < z + eps).then_some((z, submersion_of(w.kind)))
        })
        // Lowest surface first: `total_cmp` on the surface z, so a tie is still deterministic.
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, s)| s)
        .unwrap_or_default();
    // `WOW_FOG_DUMP` also explains *this* decision (`frame` drops the 1 Hz throttle, as there). The
    // committed fog is whichever submerged atmosphere the verdict names, so a fog line alone cannot
    // say whether an atmosphere that reads wrong is the wrong record or the right record never
    // selected. Reports the eye, the verdict, and every candidate surface over the eye's XY.
    if std::env::var_os("WOW_FOG_DUMP").is_some() {
        let sec = time.elapsed_secs() as u32;
        if last_dump.replace(sec) != Some(sec) {
            // Every surface over the eye's XY, whether or not the eye's claim admits it — a
            // candidate the delegation REJECTED is exactly what the Undercity green-screen report
            // needed to see, and a list filtered to the winners can never show it.
            let mut cands: Vec<String> = water
                .iter()
                .filter_map(|w| {
                    w.surface_z_at(eye[0], eye[1]).map(|z| {
                        format!(
                            "{:?} z {z:.2}{}",
                            w.kind,
                            if w.answers(claim) { "" } else { " (not yours)" }
                        )
                    })
                })
                .collect();
            cands.sort();
            eprintln!(
                "[submerged] {:?} claim {claim:?} eye [{:.1} {:.1} {:.2}] over-xy {}",
                underwater.0,
                eye[0],
                eye[1],
                eye[2],
                if cands.is_empty() {
                    "(no surface covers the eye's XY)".to_string()
                } else {
                    cands.join(", ")
                }
            );
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

    /// A stand-in placement identity — two distinct buildings, so "whose pool is this" is testable
    /// without a `World`. `Entity::from_raw_u32` is the only way to mint one outside an ECS.
    fn placement(n: u32) -> Entity {
        Entity::from_raw_u32(n).expect("valid entity id")
    }

    /// A WMO pool owned by placement `n`, group 0.
    fn wmo_info(owner: u32, kind: LiquidKind, z: f32) -> WaterChunkInfo {
        flat_info(
            LiquidSource::WmoGroup(Some(WmoRoom {
                instance: placement(owner),
                group: 0,
            })),
            kind,
            z,
        )
    }

    /// The claim of a subject standing inside placement `n`.
    fn inside(owner: u32) -> LiquidClaim {
        LiquidClaim::Inside(WmoRoom {
            instance: placement(owner),
            group: 0,
        })
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
            let info = wet_footprint(&flat_quad(5.0), &transform, LiquidSource::WmoGroup(None));
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
        let canal = wmo_info(1, LiquidKind::Still, 8.0);
        let all = [&lake, &canal];
        let deep_under = [5.0, 5.0, 0.0];

        // Standing in the tunnel: the lake 50 yd overhead must NOT answer.
        let hit = liquid_at(all.into_iter(), deep_under, inside(1)).unwrap();
        assert_eq!(hit.surface_z, 8.0, "indoors must read the WMO's own liquid");
        // Out on the surface: the ADT lake answers and the building's canal does not.
        let outside = liquid_at(all.into_iter(), deep_under, LiquidClaim::Outdoors).unwrap();
        assert_eq!(outside.surface_z, 50.0);
        // Un-classified (a unit's first frame): both sources answer — the pre-0634 behaviour, and
        // the ONLY arm that still has it.
        assert!(liquid_at(all.into_iter(), deep_under, LiquidClaim::Unknown).is_some());
        // Outside the XY footprint nothing answers, either way.
        assert!(liquid_at(all.into_iter(), [99.0, 99.0, 0.0], LiquidClaim::Outdoors).is_none());
    }

    /// **The Uldaman bug** (director repro at `-6152.73, -2969.59, 213.73`): the claiming surface
    /// belonged to a DIFFERENT BUILDING. Offline against the real client files, the player stands in
    /// `kz_uldaman_a.wmo` (id 1218) group 22 — which carries no liquid over that XY at all — while
    /// the pool answering `+185.91` over their feet is group 1 of a `md_mushroomcave.wmo` placement
    /// whose every group bbox EXCLUDES the player, 186 yd overhead.
    ///
    /// A footprint has no floor, so "indoors" as a bare bool admitted every MLIQ surface on the map.
    /// Being inside building A must never read building B's water.
    #[test]
    fn another_buildings_pool_never_claims_you() {
        let mine = wmo_info(1, LiquidKind::Still, 8.0);
        let theirs = wmo_info(2, LiquidKind::Still, 190.0);
        let all = [&mine, &theirs];
        let feet = [5.0, 5.0, 0.0];

        assert_eq!(
            liquid_at(all.into_iter(), feet, inside(1))
                .unwrap()
                .surface_z,
            8.0,
            "in building 1: only building 1's pool"
        );
        // In building 2 the tall pool IS yours — the scoping is an attribution, not a height cap.
        assert_eq!(
            liquid_at(all.into_iter(), feet, inside(2))
                .unwrap()
                .surface_z,
            190.0
        );
        // The Uldaman shape exactly: standing in a building with no pool of its own, under someone
        // else's. Pre-0696 the lowest of the two answered and the player swam in air.
        assert!(
            liquid_at([&theirs].into_iter(), feet, inside(1)).is_none(),
            "building 1 has no liquid; building 2's must not stand in for it"
        );
    }

    /// A pool on a placement that spawned no instance entity is claimable by nobody — the honest
    /// consequence of having no owner, rather than a silent fallback to "everybody".
    #[test]
    fn an_unowned_pool_answers_no_one() {
        let orphan = flat_info(LiquidSource::WmoGroup(None), LiquidKind::Still, 8.0);
        let feet = [5.0, 5.0, 0.0];
        assert!(liquid_at([&orphan].into_iter(), feet, inside(1)).is_none());
        assert!(liquid_at([&orphan].into_iter(), feet, LiquidClaim::Outdoors).is_none());
        // …but an un-classified subject still sees it, like every other surface.
        assert!(liquid_at([&orphan].into_iter(), feet, LiquidClaim::Unknown).is_some());
    }

    /// **The Undercity camera bug** (director repro at `1414.08, 53.00, -62.26`): the player's query
    /// said "not in liquid" while the camera-eye probe still saw Tirisfal's ADT water at z 32.93,
    /// 95 yd overhead, and the whole scene took the underwater filter. Both subjects now run the
    /// same delegation, so an indoor claim excludes the ADT surface for the eye exactly as it does
    /// for the feet.
    #[test]
    fn an_indoor_eye_does_not_see_the_adt_water_overhead() {
        let tirisfal = flat_info(LiquidSource::AdtChunk, LiquidKind::Still, 32.93);
        let eye = [5.0, 5.0, -62.26];
        assert!(
            liquid_at([&tirisfal].into_iter(), eye, LiquidClaim::Outdoors).is_some(),
            "outdoors under the same water: still submerged (the control)"
        );
        assert!(
            liquid_at([&tirisfal].into_iter(), eye, inside(1)).is_none(),
            "inside a building, the ADT water overhead is not the eye's liquid"
        );
    }

    /// Stacked surfaces resolve to the LOWEST, not to whichever the iterator yields first — the old
    /// `.next()` made the answer depend on spawn order.
    #[test]
    fn stacked_surfaces_take_the_lowest() {
        let upper = wmo_info(1, LiquidKind::Still, 40.0);
        let lower = wmo_info(1, LiquidKind::Still, 4.0);
        for order in [[&upper, &lower], [&lower, &upper]] {
            let hit = liquid_at(order.into_iter(), [5.0, 5.0, 0.0], inside(1)).unwrap();
            assert_eq!(hit.surface_z, 4.0);
        }
    }

    /// Lava and slime ARE swimmable (`liquid_at`) but must never drive the water-flavoured
    /// consumers (`water_surface_at` → the wade splash, footstep depth, the spline depth). B24/B25 vs
    /// the teal-lava regression the old fullbright exclusion was guarding against — both, at once.
    /// The submerged atmosphere is deliberately NOT in that list any more: it is per-kind.
    #[test]
    fn fullbright_kinds_swim_but_are_not_water() {
        let lava = wmo_info(1, LiquidKind::Magma, 6.0);
        let here = [5.0, 5.0, 0.0];
        let hit = liquid_at([&lava].into_iter(), here, inside(1)).expect("lava is a swim volume");
        assert_eq!(hit.kind, LiquidKind::Magma);
        assert!(
            water_surface_at([&lava].into_iter(), here, inside(1)).is_none(),
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
            LiquidSource::WmoGroup(Some(WmoRoom {
                instance: placement(1),
                group: 0,
            })),
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
        assert!(liquid_at([&info].into_iter(), [5.0, 5.0, 0.0], inside(1)).is_some());
        assert!(liquid_at([&info].into_iter(), [15.0, 5.0, 0.0], inside(1)).is_none());
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
            describe_at([&info].into_iter(), [0.0, 0.0, 0.0], LiquidClaim::Outdoors).is_empty(),
            "and `/liquid` lists no candidate for it"
        );
    }
}
