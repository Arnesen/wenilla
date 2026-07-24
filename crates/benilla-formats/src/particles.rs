//! Vanilla (build 5875 / MD20 v256) M2 **particle emitter** parsing — read straight from raw bytes.
//!
//! `benilla-m2` parses only the render path and deliberately skips the cosmetic chunks, so the
//! particle emitters are read here. The post-vanilla `M2ParticleEmitter` (with `texture_file_data_ids`,
//! `enable_encryption`, WoD flag bits) does not match 1.12; the vanilla layout is a fixed
//! **0x1f8-byte** record. The byte layout is from `wow-5875-re` (`FUN_0070ebd0` @ `0x70ebd0`,
//! `m2-decomp.c`) and **difftested** here against `ElwynnCampfire.m2` (see `tests/`):
//!
//! ```text
//! header array : count @ MD20+0x13c, ptr @ MD20+0x140         record stride 0x1f8
//! +0x04 flags(u32)  +0x08 position(C3Vector)  +0x14 bone(u16)  +0x16 texture(u16, index→M2 textures)
//! +0x28 blendingType(u8)  +0x2a emitterType(u16:1=Plane,2=Sphere,3=Spline)
//! +0x2c particleType(u16:0=head,1=tail,2=both)  +0x30 tileRows(u16)  +0x32 tileCols(u16)
//! +0x34.. ten emission M2Tracks (vanilla 28-byte track; the runtime uses value[0] of each):
//!   +0x34 emissionSpeed  +0x50 speedVariation  +0x6c verticalRange  +0x88 horizontalRange
//!   +0xa4 gravity  +0xc0 lifespan  +0xdc emissionRate  +0xf8 areaLength  +0x114 areaWidth
//!   +0x130 zSource
//! +0x14c.. baked over-life color/opacity/scale/cell ramps (see `OverLife`).
//! +0x17c tailTime(f32, SECONDS — the tail streak's length is velocity·tailTime)
//! +0x180 twinkleSpeed  +0x184 twinklePercent  +0x188/+0x18c twinkleScale{min,max}
//! +0x190 inheritScale(f32)  +0x194 drag(f32, plain scalar — the velocity decay)
//! +0x198 spin(f32, rad/s quad rotation)
//! +0x1c4..+0x1d0 followSpeed1/followScale1/followSpeed2/followScale2 (the follow-delta line)
//! +0x1dc enabled(M2Track<u8>, step) — the emission ON/OFF gate, closing the record at 0x1f8.
//! ```
//!
//! The keyed tracks here (emission rate, enabled) parse through the shared [`crate::value_track`]
//! kernel: **rebased to the first sequence's band** at parse (see its module doc for the
//! global-timeline law), so the runtime emitter clock (seconds since the effect spawned) samples
//! them directly.
//!
//! The record tail (+0x180..) is the wow-re `part-simspace-fields.md` §5-verified map (their
//! `ac915a7d`): the loader `0x70ebd0` remaps file+0x188/+0x18c → runtime twinkleScale min/max
//! (delta = max − min at rt+0x1c0), NOT a size multiplier — see [`ParticleEmitterDef::twinkle`].

use std::io::Cursor;

use anyhow::Result;
use benilla_m2::parse_m2;

use crate::value_track::{seq0_band, track_enabled, track_keys_with, OnOffTrack, ValueTrack};

/// Emitter spawn shape (file `emitterType` @ +0x2a). Only `Plane` is needed for campfires/torches;
/// `Sphere` (e.g. dust clouds) and `Spline` follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticleShape {
    Plane,
    Sphere,
    Spline,
}

/// How a particle batch blends (file `blendingType` @ +0x28 = the M2/EGxBlend enum). Additive is the
/// flame/glow case (the campfire is `4`); the rest fold to alpha for now (mod/mod2x are rare for the
/// props we render and will get their own path when a model needs them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticleBlend {
    /// `3` NoAlphaAdd / `4` Add — `(SRC_ALPHA, ONE)`. Flames, glows, embers. No depth write/sort.
    Add,
    /// `2` Alpha — `(SRC_ALPHA, ONE_MINUS_SRC_ALPHA)`. Smoke.
    Alpha,
    /// `0` Opaque / `1` AlphaKey — rare for particles.
    Opaque,
}

/// The baked **over-life** ramps (record tail, +0x14c..) sampled by a particle's normalized age `u =
/// age/lifespan`. Each of color, size and cell is a **3-key ramp** evaluated as two linear segments
/// split at [`Self::mid`]: key0→key1 over `u∈[0,mid]`, then key1→key2 over `u∈[mid,1]`. Byte layout +
/// math verified in `wow-5875-re` (`FUN_0070ebd0` builder, `FUN_007b9b10` evaluator) and difftested
/// against `ElwynnCampfire.m2`. These drive the believable flame fade / grow / flicker.
#[derive(Debug, Clone, Copy)]
pub struct OverLife {
    /// Normalized age (`+0x14c`) splitting the two ramp segments.
    pub mid: f32,
    /// RGBA keys k0/k1/k2 (`+0x150/+0x154/+0x158`, packed BGRA u32 → linear 0..1). The **A** channel
    /// is the particle's additive weight (its over-life opacity); there is no separate alpha array.
    pub color: [[f32; 4]; 3],
    /// Uniform particle size keys k0/k1/k2 (`+0x15c/+0x160/+0x164`, yards).
    pub scale: [f32; 3],
    /// Head texture-cell index range `[begin, end]` for segment A (`u≤mid`, `+0x168/+0x16a`) and
    /// segment B (`u>mid`, `+0x16e/+0x170`) — the cell animated across the `rows×cols` atlas.
    pub cell_a: [u16; 2],
    pub cell_b: [u16; 2],
}

impl OverLife {
    /// Sample the ramps at normalized age `u` (0..1): `(rgba, size_yards, cell_index)`.
    pub fn sample(&self, u: f32) -> ([f32; 4], f32, u16) {
        let u = u.clamp(0.0, 1.0);
        let mid = self.mid.clamp(1e-3, 1.0);
        let (a, b, t, cell) = if u <= mid {
            (0, 1, u / mid, self.cell_a)
        } else {
            (1, 2, (u - mid) / (1.0 - mid).max(1e-3), self.cell_b)
        };
        let t = t.clamp(0.0, 1.0);
        let mut color = [0.0; 4];
        for (c, slot) in color.iter_mut().enumerate() {
            *slot = self.color[a][c] + (self.color[b][c] - self.color[a][c]) * t;
        }
        let size = self.scale[a] + (self.scale[b] - self.scale[a]) * t;
        // cell = floor(begin + (end-begin+1)·t), clamped to [begin, end].
        let span = (i32::from(cell[1]) - i32::from(cell[0]) + 1).max(1) as f32;
        let idx = (f32::from(cell[0]) + span * t).floor() as i32;
        let cell_index = idx.clamp(i32::from(cell[0]), i32::from(cell[1])) as u16;
        (color, size, cell_index)
    }
}

/// A SPLINE (type-3) emitter's authored curve: a flattened **cubic Bézier chain** (K segments,
/// `3K+1` control points: `P, out-tangent, in-tangent, P, …`), **arc-length parameterized** — the
/// reference computes per-segment arc lengths into a knot array at load (`0x7b9a80` →
/// `0x4532e0`; the knots are NOT on disk) and the spawn kernel's `t ∈ [0,1]` walks the chain by
/// normalized arc length (wow-re `part-spline-file-layout.md` + `part-shape-kernels.md` §3). Our
/// per-segment length is a 16-chord subdivision (the reference's `0x453e50` method is untraced —
/// any smooth-curve length approximation lands within a fraction of a percent).
#[derive(Debug, Clone)]
pub struct SplineData {
    /// Control points, model-local WoW axes (`3K+1`, from file+0x1d4/+0x1d8).
    pub points: Vec<[f32; 3]>,
    /// Cumulative normalized arc length at each segment boundary (`K+1` entries, 0 → 1).
    knots: Vec<f32>,
}

impl SplineData {
    /// Build from the raw control points (must be `3K+1`; returns `None` below one segment).
    pub fn new(points: Vec<[f32; 3]>) -> Option<Self> {
        let k = points.len().checked_sub(1)? / 3;
        if k == 0 || points.len() != 3 * k + 1 {
            return None;
        }
        let mut knots = vec![0.0f32];
        for seg in 0..k {
            let mut len = 0.0;
            let mut prev = Self::bezier(&points[3 * seg..3 * seg + 4], 0.0);
            for i in 1..=16 {
                let p = Self::bezier(&points[3 * seg..3 * seg + 4], i as f32 / 16.0);
                len += ((p[0] - prev[0]).powi(2)
                    + (p[1] - prev[1]).powi(2)
                    + (p[2] - prev[2]).powi(2))
                .sqrt();
                prev = p;
            }
            knots.push(knots[seg] + len);
        }
        let total = *knots.last().unwrap();
        if total > 0.0 {
            for kn in &mut knots {
                *kn /= total;
            }
        }
        Some(Self { points, knots })
    }

    fn bezier(p: &[[f32; 3]], u: f32) -> [f32; 3] {
        let w = [
            (1.0 - u).powi(3),
            3.0 * u * (1.0 - u).powi(2),
            3.0 * u * u * (1.0 - u),
            u.powi(3),
        ];
        std::array::from_fn(|c| (0..4).map(|i| w[i] * p[i][c]).sum())
    }

    fn bezier_deriv(p: &[[f32; 3]], u: f32) -> [f32; 3] {
        let w = [
            -3.0 * (1.0 - u).powi(2),
            3.0 * (1.0 - u) * (1.0 - 3.0 * u),
            3.0 * u * (2.0 - 3.0 * u),
            3.0 * u * u,
        ];
        std::array::from_fn(|c| (0..4).map(|i| w[i] * p[i][c]).sum())
    }

    /// Locate `(segment, local u)` for a normalized arc fraction `t`.
    fn locate(&self, t: f32) -> (usize, f32) {
        let k = self.knots.len() - 1;
        let seg = self.knots[1..k]
            .iter()
            .position(|&kn| t < kn)
            .unwrap_or(k - 1);
        let (a, b) = (self.knots[seg], self.knots[seg + 1]);
        (seg, ((t - a) / (b - a).max(1e-6)).clamp(0.0, 1.0))
    }

    /// The curve point at arc fraction `t` — first/last control point outside `[0,1]` (the
    /// reference's `0x453390` clamp legs).
    pub fn eval(&self, t: f32) -> [f32; 3] {
        if t <= 0.0 {
            return self.points[0];
        }
        if t >= 1.0 {
            return *self.points.last().unwrap();
        }
        let (seg, u) = self.locate(t);
        Self::bezier(&self.points[3 * seg..3 * seg + 4], u)
    }

    /// The (unnormalized) curve tangent at arc fraction `t` (the reference's `0x453420` leg —
    /// its caller renormalizes).
    pub fn tangent(&self, t: f32) -> [f32; 3] {
        let (seg, u) = self.locate(t.clamp(0.0, 1.0));
        Self::bezier_deriv(&self.points[3 * seg..3 * seg + 4], u)
    }
}

/// One parsed vanilla M2 particle emitter, reduced to what the renderer needs. Positions/vectors are in
/// **model-local** space (WoW axes, Z up) — compose with the instance world transform at spawn.
#[derive(Debug, Clone)]
pub struct ParticleEmitterDef {
    pub flags: u32,
    /// Emitter origin, model-local. (Bone-follow @ +0x14 is deferred — static props sit on the root.)
    pub position: [f32; 3],
    pub bone: u16,
    pub shape: ParticleShape,
    pub blend: ParticleBlend,
    /// **Geometry model** (file+0x18 count / +0x1c offset, an `M2Array<char>` path — wow-re
    /// `part-model-particles.md`): when it names a resolvable `.m2`, this emitter spawns tiny
    /// 3-D MODEL instances instead of billboard quads (Whirlwind's blades, Cone of Cold's
    /// shards, the cyclones). `None` when unauthored.
    pub geometry_model: Option<String>,
    /// **Recursion model** (file+0x20 / +0x24 — wow-re `part-child-recursion.md`): the named
    /// model's OWN particle emitters (capped at 4) become CHILD emitters driven once per live
    /// parent particle per frame at the particle's position (Fire Blast's impact sparks, bomb
    /// explosions). `None` when unauthored.
    pub recursion_model: Option<String>,
    /// Particle texture (`.blp` path, as embedded in the M2 textures table). `None` if unresolved.
    pub texture: Option<String>,
    /// Texture atlas size for cell animation (`1×1` = a single static cell).
    pub tile_rows: u16,
    pub tile_cols: u16,
    /// 0 = head (camera quad), 1 = tail (speed-stretched), 2 = both. We render head first.
    pub head_tail: u8,
    // --- emission params: value[0] of each emission M2Track (the runtime's own load-time bake) ---
    /// Initial particle speed (yards/sec).
    pub emission_speed: f32,
    /// Fractional random speed spread (`speed·(1 ± var·noise)`).
    pub speed_variation: f32,
    /// Half-angle of the emission cone, radians (small = a tight upward jet).
    pub vertical_range: f32,
    /// Azimuthal spread, radians (`2π` = full circle around the up axis).
    pub horizontal_range: f32,
    /// Downward acceleration (yards/sec²); small/zero for a buoyant flame.
    pub gravity: f32,
    /// Particle lifetime (seconds) — killed at this age.
    pub lifespan: f32,
    /// Particles spawned per second — the one emission parameter parsed as its **full keyed track**
    /// ([`ValueTrack`]), because the one-shot combat effects author their bursts on it (see
    /// [`ValueTrack`]'s doc). The other nine stay `value[0]`-baked: constant in every model this
    /// renderer draws (verified across the spurt family + the ambient props).
    pub emission_rate: ValueTrack,
    /// The emission ON/OFF gate ([`OnOffTrack`], file `+0x1dc`) — the one-shot effects'
    /// choreography track. Sampled on the same clip clock as `emission_rate`.
    pub enabled: OnOffTrack,
    /// The clip clock's WRAP: `Some(first sequence's span ms)` when that sequence LOOPS — the
    /// client samples the two tracks above on `m2_animate`'s sequence time, which wraps there,
    /// re-firing windowed tracks every pass (a precast hold's pulsing hand flash). A CLAMPED
    /// sequence holds its end instead — and a discrete kit instance is reaped at exactly one
    /// pass (`spell_fx`), so its boundary never renders; `None` keeps the end-clamp hold.
    /// (wow-re `part-emission-rate-animated.md`: the per-frame `m2_animate` sampler.)
    pub clip_wrap_ms: Option<f32>,
    /// Plane emitter full extents (yards); spawn points are drawn from the `±½·extent` rectangle.
    pub area_length: f32,
    pub area_width: f32,
    /// zSource / gravity2 — a pull toward a point above the emitter (0 = unused).
    pub z_source: f32,
    /// Velocity **drag** (file +0x194; a plain scalar, not a track — the builder copies it straight to
    /// the runtime emitter's `+0x1e0` at `m2-decomp.c:8950`, *outside* the ten track setters). Each
    /// frame the runtime applies `vel −= min(dt·drag, 1)·vel` (verified `particle_integrate` @
    /// `0x7b2680`, step 4) — exponential
    /// velocity decay that caps a particle's total travel near `speed/drag`. Load-bearing for props
    /// that author a *fast, long-lived, zero-gravity* jet and rely on drag to contain it: e.g.
    /// `CandelabraTallWall01` (speed 0.56, life 6, gravity 0, **drag 10** → a ~0.06 yd flicker; with
    /// no drag the same particle coasts 3.3 yd to the ceiling). 0 = no drag.
    pub drag: f32,
    /// **Tail time** (file +0x17c, seconds — wow-re `part-quad-tail-twinkle.md`, their `65b8305b`):
    /// a tail-mode particle (`head_tail` 1/2) renders a velocity-projected streak of world length
    /// `|velocity| · tail_time`, trailing behind the motion; file flag `0x400` additionally clamps
    /// the time to the particle's age (the streak grows from zero at birth).
    pub tail_time: f32,
    /// **Tumble** — the model particles' angular-velocity range (file +0x19c min / +0x1a8 max,
    /// C3Vectors, rad/s — wow-re `part-model-particles.md` §b): each birth rolls a body-frame
    /// spin vector. The reference's roll carries a VERIFIED original-client asymmetry — only X
    /// honors `min + u·range`; Y and Z multiply a raw `[1,2)` mantissa by their range alone
    /// (their min is loaded but never read) — which a fidelity consumer must replicate.
    pub angular_velocity_min: [f32; 3],
    pub angular_velocity_max: [f32; 3],
    /// **Emitter-motion inherit scale** (file +0x190 → runtime +0x1c4 — wow-re
    /// `part-emitter-motion.md` §1, byte-verified `0x70ff71/0x70ff77`; closes the field the old
    /// tail map left "?"): scales the inherited velocity a flag-0x40 emitter feeds its births
    /// (see [`Self::inherits_emitter_motion`]).
    pub inherit_scale: f32,
    /// **Follow pairs** (file +0x1c4..+0x1d0 — wow-re `part-emitter-motion.md` §2, the
    /// `0x7b5d30` setter): two authored (emitter speed → follow fraction) samples defining the
    /// follow-delta response line; see [`Self::follow_line`].
    pub follow_speed1: f32,
    pub follow_scale1: f32,
    pub follow_speed2: f32,
    pub follow_scale2: f32,
    /// **Twinkle** — the per-frame flicker modulation (file +0x180/+0x184/+0x188/+0x18c; wow-re
    /// `part-simspace-fields.md`, byte-verified in the quad writer `0x7b2a50`). The rendered half-
    /// size is the over-life scale ramp × a **gated** twinkle multiplier:
    /// `min ≠ max ⇒ noise(speed·age)·(max − min) + min`, **skipped entirely when `min == max`**
    /// (a degenerate range — `{0,0}` and `{1,1}` alike burn steady at ramp size; the kobold candle's
    /// `{0,0}` is NOT size zero). This corrects the old `scale_base + rand·scale_variation` reading,
    /// which inflated every `{1,1}` flame 1–2× and collapsed `{0,0}` to invisible.
    pub twinkle_speed: f32,
    /// Fraction of frames the particle draws at all while twinkling (the reference's separate
    /// draw-gate; exact byte condition thin — consumers may treat `≥ 1` / `0` as always-draw).
    pub twinkle_percent: f32,
    pub twinkle_min: f32,
    pub twinkle_max: f32,
    /// SPLINE (type-3) curve data — `Some` only when the record authors a valid Bézier chain
    /// (file+0x1d4 count / +0x1d8 offset, `N = 3·⌊count/3⌋+1` C3Vectors — wow-re
    /// `part-spline-file-layout.md`, VERIFIED). For splines the generic emission fields are
    /// **repurposed** (each setter reads its track's `values[0]`): `area_length`/`area_width`
    /// = **tMin/tMax** (the spawned arc-fraction window, clamped [0,1]), `vertical_range` =
    /// the **tangent-spin range ψ** (birth velocity = +Z rotated about the local tangent by
    /// S11·ψ), `horizontal_range` = the **scatter** (a `U01·scatter` position jitter along the
    /// velocity), and the rate track's `values[0]` doubles as the load-time arcScale (a
    /// one-frame transient — the per-frame rate sample overwrites it; we always sample the
    /// track, so it needs no separate field).
    pub spline: Option<SplineData>,
    /// Quad **spin** (file +0x198 → runtime +0x18c): the billboard rotates in-plane by
    /// `angle = spin · age` (rad; fcos/fsin @ `0x7b2ddc` in the quad writer). 0 = no rotation.
    /// A **negative** spin is the author's alternating-direction switch: the writer negates the
    /// (negative) angle on half the particles (pointer-bit-5 hash, `0x7b2dda`), counter-rotating
    /// the cloud — vanilla's only rotation randomizer (the record has no phase/variance field).
    pub spin: f32,
    /// Baked over-life ramps (color/opacity/size/cell) — see [`OverLife`].
    pub over_life: OverLife,
}

impl ParticleEmitterDef {
    /// File flag `0x10` (→ runtime `0x100` — the loader's flag word is a **non-identity remap**,
    /// wow-re `part-simspace-fields.md` corrections `1f40db0b`, byte block `0x70faf8–0x70fc44`):
    /// the whole cloud renders through the emitter's **live bone matrix** each frame (rotation and
    /// all — the chandelier's candle flames rigidly ride the swing). Clear ⇒ bone/model rotation is
    /// baked at birth instead. Either way the cloud is **re-anchored to the emitter's current
    /// position every frame** (the draw matrix rebuilds with `translate(−emitterPos)` per frame) —
    /// a moving model carries its flame; there is NO world-frozen trail mode.
    pub fn model_space(&self) -> bool {
        self.flags & 0x10 != 0
    }
    /// File flag `0x4000` (→ runtime `0x40000` — wow-re `part-emitter-motion.md` §2/§2b,
    /// §5-resolved): live particles keep exactly **[`Self::follow_line`]'s fraction (≤ 1) of
    /// the emitter's per-frame world motion** — at saturation the trail rides the emitter
    /// rigidly, below it lags toward a world-frozen trail; it never leads. (The reference's
    /// baseline for this content class is world-frozen — its emitter motion folds into the
    /// emitter matrix — and its `+fraction·Δ` add recovers the ride; over an anchor-riding
    /// store the same observable is a `(fraction−1)·Δ` move.) The hunter missiles author it
    /// (19 emitters / 15 spell models).
    pub fn follow_emitter(&self) -> bool {
        self.flags & 0x4000 != 0
    }
    /// The follow-delta response line (the reference's load-time `0x7b5d30`): `(slope,
    /// intercept)` of the line through the two authored `(speed, fraction)` samples, so the
    /// per-frame fraction is `clamp(slope·|Δpos|/dt + intercept, 0, 1)`. `None` when the two
    /// speeds coincide (the reference zeroes both — no follow response).
    pub fn follow_line(&self) -> Option<(f32, f32)> {
        ((self.follow_speed2 - self.follow_speed1).abs() >= 1e-6).then(|| {
            let slope = (self.follow_scale2 - self.follow_scale1)
                / (self.follow_speed2 - self.follow_speed1);
            (slope, self.follow_scale1 - slope * self.follow_speed1)
        })
    }
    /// File flag `0x40` (→ runtime `0x400` — wow-re `part-emitter-motion.md` §1, VERIFIED):
    /// births inherit the emitter's recent motion. The emitter keeps a ~30 Hz inherit-velocity
    /// vector — at each trigger (accumulated dt > 1/30 s), `oneFrameΔ · ((1/30)/accum) ·
    /// inherit_scale`, zeroed while no particles are live — and each birth adds
    /// `(1 + S11·speed_variation) · inherit` to its velocity (the shape kernels' closing
    /// block). The enchant hands / Bloodlust / Death Wish family authors it (70 emitters / 33
    /// spell models).
    pub fn inherits_emitter_motion(&self) -> bool {
        self.flags & 0x40 != 0
    }
    /// File flag `0x20` (→ runtime `0x200`): the rendered particle size is multiplied by the
    /// emitter transform's scale magnitude. Torches/campfires author it; without it an
    /// instance-scaled prop scales its particle *positions* only.
    pub fn scale_size_by_instance(&self) -> bool {
        self.flags & 0x20 != 0
    }
    /// File flag `0x100` on a **sphere** emitter (→ runtime `0x4000`, mapped only when the type
    /// word is 2): birth velocity is straight `+Z` instead of radial through the shell point.
    pub fn sphere_up(&self) -> bool {
        self.flags & 0x100 != 0
    }
    /// File flag `0x80` on a **sphere** emitter (→ runtime `0x800` — like [`Self::sphere_up`]
    /// the loader maps it only when the type word is 2, so a plane's authored `0x80` is dead
    /// data): kill the particle the frame its motion turns AWAY from the emitter origin — the
    /// integrator's tail tests `dot(stepVelocity, updatedPos) > 0` and returns dead (`0x7b2680`,
    /// the `rt 0x800` branch). The suction containment: every corpus author pairs it with a
    /// NEGATIVE emission speed (Blink/Conjure/Detect Magic converge inward), and the kill is
    /// what stops the stream at the centre instead of letting it spray out the far side.
    pub fn kill_outbound(&self) -> bool {
        self.shape == ParticleShape::Sphere && self.flags & 0x80 != 0
    }
    /// File flag `0x400` (→ runtime `0x10000`): a tail streak's `tail_time` is clamped to the
    /// particle's age — the streak grows from zero at birth instead of popping in full-length.
    pub fn tail_clamps_to_age(&self) -> bool {
        self.flags & 0x400 != 0
    }
    /// File flag `0x200` (→ runtime `0x8000` — wow-re `part-model-particles.md` §b): each
    /// tumble axis's rolled angular velocity is independently sign-flipped with probability ½
    /// (three extra draws) — the model particles' spin-direction randomizer.
    pub fn tumble_random_sign(&self) -> bool {
        self.flags & 0x200 != 0
    }
    /// File flag `0x2000` (→ runtime `0x20000` — wow-re `part-groundsnap-zhook.md`, VERIFIED):
    /// the **at-spawn ground snap**. Once, at each birth (anchored mode only — the shape
    /// kernels call the hook `0x7b2140` inside their bit-0x100-CLEAR branch), the client
    /// probes **20 yd straight down** for terrain OR WMO/doodad geometry (`0x672b60` →
    /// `0x6aa160`, flags 0x100111); on a hit the particle's up-coordinate becomes `surfaceZ +
    /// its birth over-life SIZE` (the sampler's float leg is the scale ramp — `0x7b9e20`
    /// writes bank+0x24/+0x28 from file+0x15c/0x160/0x164 — so the quad stands ON the surface
    /// by its half-extent). No surface within 20 yd leaves the spawn position untouched. NOT a
    /// per-frame clamp, NOT terrain-only. Fire/Frost Nova, the ground fogs (37 emitters / 17
    /// spell models).
    pub fn ground_snap(&self) -> bool {
        self.flags & 0x2000 != 0
    }
    /// File flag `0x8000` (bit 15): the emission model is a **one-shot BURST**, not continuous.
    /// The reference's per-frame emitter pass branches on this bit (`0x718ec8`): a burst emitter
    /// gets a rising-edge trigger — the frame `(enabled && rate > 0)` first turns true it requests
    /// ONE spawn of `ftol(rate · density · LOD)` particles (`0x7b5c50` → the spawn driver's
    /// self-clearing burst branch `0x7b55ae`/`0x7b563d`) — and the continuous-enable bit is
    /// skipped, so the `rate·dt` pour can never run for it. Re-arms only when the gate falls
    /// (a looping clip's next pass). This flag is why the Feint/Eviscerate impact's plume and
    /// crescents are over ~0.5 s while the same-shaped `Eviscerate_Cast_Hands` flame (bit clear)
    /// pours for its whole 1.6 s clip (wow-re `part-emission-burst-flag.md`, byte-arbitrated).
    pub fn burst(&self) -> bool {
        self.flags & 0x8000 != 0
    }
    /// File flag `0x1000` (→ runtime `0x2000`): the head quad does NOT billboard — it lies flat
    /// in the emitter's model-space XY plane, carried by the live model→world matrix (the
    /// community "XYQuad"; the impact crescents, state rings, fish-school splash rings).
    /// Byte-pinned end to end in wow-re: corners = the ±1 XY unit square (z = 0) × the draw
    /// matrix — camera-independent, unit half-extent exactly like the billboard corner table —
    /// and quad spin is Rodrigues about the quad-plane normal (consumption
    /// `part-quad-tail-twinkle.md` §3, builder semantics `part-tiled-corner-builder.md`, both
    /// VERIFIED).
    pub fn xy_quad(&self) -> bool {
        self.flags & 0x1000 != 0
    }
    /// The gated twinkle size multiplier, given a noise sample `noise ∈ [0,1)` from the caller's
    /// LUT (indexed off `twinkle_speed · age`): identity when the range is degenerate.
    pub fn twinkle(&self, noise: f32) -> f32 {
        if (self.twinkle_max - self.twinkle_min).abs() < 1e-6 {
            1.0
        } else {
            noise * (self.twinkle_max - self.twinkle_min) + self.twinkle_min
        }
    }
}

const STRIDE: usize = 0x1f8;
const HDR_COUNT: usize = 0x13c;
const HDR_PTR: usize = 0x140;

fn le_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn le_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn le_f32(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn le_vec3(b: &[u8], o: usize) -> [f32; 3] {
    [le_f32(b, o), le_f32(b, o + 4), le_f32(b, o + 8)]
}

/// Read value[0] of a vanilla M2Track (28 bytes) whose record begins at `track`. Track tail:
/// `values{count @ +0x14, offset @ +0x18}`, values are `f32`. Returns `default` if the track is empty
/// or points out of range (the runtime's load-time bake reads exactly this first value).
fn track_first(b: &[u8], track: usize, default: f32) -> f32 {
    if track + 0x1c > b.len() {
        return default;
    }
    let n = le_u32(b, track + 0x14);
    let ofs = le_u32(b, track + 0x18) as usize;
    if n == 0 || ofs + 4 > b.len() {
        return default;
    }
    le_f32(b, ofs)
}

/// Read a vanilla M2Track's full `(timestamp ms, f32)` key list, band-rebased — the shared
/// kernel's reader with the scalar decode ([`track_keys_with`]).
fn track_keys(b: &[u8], track: usize, default: f32, band: (u32, u32)) -> ValueTrack {
    track_keys_with(b, track, default, band, 4, le_f32)
}

fn blend_of(v: u8) -> ParticleBlend {
    match v {
        3 | 4 => ParticleBlend::Add,
        2 | 5 | 6 => ParticleBlend::Alpha, // 5/6 (mod/mod2x) fold to alpha until a model needs them
        _ => ParticleBlend::Opaque,
    }
}

fn shape_of(v: u16) -> ParticleShape {
    match v {
        2 => ParticleShape::Sphere,
        3 => ParticleShape::Spline,
        _ => ParticleShape::Plane,
    }
}

/// Parse the vanilla M2's particle emitters from its file bytes. Texture indices are resolved against
/// the M2 textures table (parsed by `benilla-m2`; the particle *records* are read here). Returns an
/// empty vec if the model has no emitters or isn't a parseable M2.
pub fn parse_m2_particle_emitters(bytes: &[u8]) -> Result<Vec<ParticleEmitterDef>> {
    // Texture names come from `benilla-m2`'s textures table.
    let textures: Vec<Option<String>> = match parse_m2(&mut Cursor::new(bytes)) {
        Ok(fmt) => fmt
            .model()
            .textures
            .iter()
            .map(|t| {
                let f = t
                    .filename
                    .string
                    .to_string_lossy()
                    .trim_end_matches('\0')
                    .to_string();
                (!f.is_empty()).then_some(f)
            })
            .collect(),
        Err(_) => Vec::new(),
    };

    if bytes.len() < HDR_PTR + 4 || &bytes[..4] != b"MD20" {
        return Ok(Vec::new());
    }
    let count = le_u32(bytes, HDR_COUNT) as usize;
    let base = le_u32(bytes, HDR_PTR) as usize;
    if count == 0 || count > 256 || base + count * STRIDE > bytes.len() {
        return Ok(Vec::new());
    }

    // The first sequence's absolute time band — the window the keyed tracks below are rebased
    // onto (`value_track::seq0_band`).
    let band = seq0_band(bytes);
    let clip_wrap_ms = crate::value_track::seq0_wrap_ms(bytes);

    // Decode a packed BGRA `CImVector` color key → linear RGBA 0..1 (A = over-life opacity).
    let color_key = |o: usize| -> [f32; 4] {
        let v = le_u32(bytes, o);
        [
            ((v >> 16) & 0xff) as f32 / 255.0, // R
            ((v >> 8) & 0xff) as f32 / 255.0,  // G
            (v & 0xff) as f32 / 255.0,         // B
            ((v >> 24) & 0xff) as f32 / 255.0, // A
        ]
    };

    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let e = base + i * STRIDE;
        let tex_index = le_u16(bytes, e + 0x16) as usize;
        let texture = textures.get(tex_index).cloned().flatten();
        let shape = shape_of(le_u16(bytes, e + 0x2a));
        // The two per-emitter model paths (`M2Array<char>`, offset pre-fixup = file-relative):
        // geometry (model particles) and recursion (child emitters).
        let model_path = |cnt_off: usize| -> Option<String> {
            let n = le_u32(bytes, e + cnt_off) as usize;
            let ofs = le_u32(bytes, e + cnt_off + 4) as usize;
            if n < 2 || ofs + n > bytes.len() {
                return None;
            }
            let s = String::from_utf8_lossy(&bytes[ofs..ofs + n])
                .trim_end_matches('\0')
                .to_string();
            (!s.is_empty()).then_some(s)
        };
        let geometry_model = model_path(0x18);
        let recursion_model = model_path(0x20);
        // SPLINE control points (file+0x1d4/+0x1d8, raw M2Array dwords): the reference copies
        // N = 3·⌊count/3⌋+1 C3Vectors — the flattened cubic Bézier chain.
        let spline = (shape == ParticleShape::Spline)
            .then(|| {
                let q = le_u32(bytes, e + 0x1d4) as usize;
                let ofs = le_u32(bytes, e + 0x1d8) as usize;
                let n = 3 * (q / 3) + 1;
                if q < 3 || ofs + n * 12 > bytes.len() {
                    return None;
                }
                SplineData::new((0..n).map(|p| le_vec3(bytes, ofs + p * 12)).collect())
            })
            .flatten();
        let over_life = OverLife {
            mid: le_f32(bytes, e + 0x14c),
            color: [
                color_key(e + 0x150),
                color_key(e + 0x154),
                color_key(e + 0x158),
            ],
            scale: [
                le_f32(bytes, e + 0x15c),
                le_f32(bytes, e + 0x160),
                le_f32(bytes, e + 0x164),
            ],
            cell_a: [le_u16(bytes, e + 0x168), le_u16(bytes, e + 0x16a)],
            cell_b: [le_u16(bytes, e + 0x16e), le_u16(bytes, e + 0x170)],
        };
        out.push(ParticleEmitterDef {
            flags: le_u32(bytes, e + 0x04),
            position: le_vec3(bytes, e + 0x08),
            bone: le_u16(bytes, e + 0x14),
            shape,
            spline,
            geometry_model,
            recursion_model,
            clip_wrap_ms,
            blend: blend_of(bytes[e + 0x28]),
            texture,
            tile_rows: le_u16(bytes, e + 0x30).max(1),
            tile_cols: le_u16(bytes, e + 0x32).max(1),
            head_tail: bytes[e + 0x2c],
            emission_speed: track_first(bytes, e + 0x34, 0.0),
            speed_variation: track_first(bytes, e + 0x50, 0.0),
            vertical_range: track_first(bytes, e + 0x6c, 0.0),
            horizontal_range: track_first(bytes, e + 0x88, 0.0),
            gravity: track_first(bytes, e + 0xa4, 0.0),
            lifespan: track_first(bytes, e + 0xc0, 1.0),
            emission_rate: track_keys(bytes, e + 0xdc, 0.0, band),
            enabled: track_enabled(bytes, e + 0x1dc, band),
            area_length: track_first(bytes, e + 0xf8, 0.0),
            area_width: track_first(bytes, e + 0x114, 0.0),
            z_source: track_first(bytes, e + 0x130, 0.0),
            drag: le_f32(bytes, e + 0x194),
            tail_time: le_f32(bytes, e + 0x17c),
            angular_velocity_min: le_vec3(bytes, e + 0x19c),
            angular_velocity_max: le_vec3(bytes, e + 0x1a8),
            inherit_scale: le_f32(bytes, e + 0x190),
            follow_speed1: le_f32(bytes, e + 0x1c4),
            follow_scale1: le_f32(bytes, e + 0x1c8),
            follow_speed2: le_f32(bytes, e + 0x1cc),
            follow_scale2: le_f32(bytes, e + 0x1d0),
            twinkle_speed: le_f32(bytes, e + 0x180),
            twinkle_percent: le_f32(bytes, e + 0x184),
            twinkle_min: le_f32(bytes, e + 0x188),
            twinkle_max: le_f32(bytes, e + 0x18c),
            spin: le_f32(bytes, e + 0x198),
            over_life,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repo root's `WoW/Data` (gitignored; the real-data tests skip when absent).
    fn vanilla_data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
    }

    /// The spline chain is **arc-length** parameterized: two straight segments of length 1 and
    /// 3 (control points at exact thirds, so the cubic degenerates to linear) put the segment
    /// boundary at t = 0.25, not 0.5 — plus the reference's outside-[0,1] clamp legs.
    #[test]
    fn spline_chain_is_arc_length_parameterized() {
        let x = |v: f32| [v, 0.0, 0.0];
        let s = SplineData::new(vec![
            x(0.0),
            x(1.0 / 3.0),
            x(2.0 / 3.0),
            x(1.0), // segment 0: 1 yd
            x(2.0),
            x(3.0),
            x(4.0), // segment 1: 3 yd
        ])
        .expect("3K+1 chain");
        assert!((s.eval(0.25)[0] - 1.0).abs() < 1e-4, "boundary at arc 1/4");
        assert!((s.eval(0.625)[0] - 2.5).abs() < 1e-4, "mid of segment 1");
        assert_eq!(s.eval(-0.5), [0.0, 0.0, 0.0], "clamp to first point");
        assert_eq!(s.eval(1.5), [4.0, 0.0, 0.0], "clamp to last point");
        let tan = s.tangent(0.1);
        assert!(tan[0] > 0.0 && tan[1] == 0.0 && tan[2] == 0.0, "+X tangent");
        assert!(SplineData::new(vec![x(0.0)]).is_none(), "below one segment");
    }

    /// The real `BloodSpurt.m2` (decision 0140 fold-back — the melee impact flash): 4 emitters,
    /// and the three the old `value[0]` bake silently killed are **keyed bursts** — the starflash
    /// (STARFLASH_GREY, additive) rates 0→20→0 over the first 133 ms. Pins the burst keys and
    /// that the spray (emitter 0) still reads its constant-equivalent first key of 100/s.
    #[test]
    fn real_blood_spurt_emitters_are_keyed_bursts() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let bytes = chain
            .read_file("Particles\\BloodSpurts\\BloodSpurt.m2")
            .expect("read BloodSpurt.m2");
        let defs = parse_m2_particle_emitters(&bytes).expect("parse emitters");
        assert_eq!(defs.len(), 4);
        // Emitter 0 — the red spray: starts hot (100/s at t=0), the old bake's one working case.
        assert_eq!(defs[0].emission_rate.first(), 100.0);
        // Emitter 3 — the starflash: additive, STARFLASH texture, burst-keyed (first key 0,
        // peak 20/s inside the 67–100 ms window, dead again by 133 ms).
        let flash = &defs[3];
        assert!(flash
            .texture
            .as_deref()
            .is_some_and(|t| t.to_ascii_uppercase().contains("STARFLASH")));
        assert_eq!(flash.blend, ParticleBlend::Add);
        assert_eq!(flash.emission_rate.first(), 0.0, "value[0] bake = silence");
        assert_eq!(flash.emission_rate.peak(), 20.0);
        assert_eq!(flash.emission_rate.step_ms(80.0), 20.0);
        assert_eq!(flash.emission_rate.step_ms(200.0), 0.0);
        // Emitters 1/2 — the glowball droplets, same burst shape, peaks 200.
        assert_eq!(defs[1].emission_rate.peak(), 200.0);
        assert_eq!(defs[2].emission_rate.peak(), 200.0);
        // The spray's enabled track cuts emission at 500 ms — exactly where its rate track goes
        // negative (the authored tail the old always-on read let the floor-at-0 hide).
        assert!(defs[0].enabled.on_at(400.0));
        assert!(!defs[0].enabled.on_at(600.0));
    }

    /// The real `Feint_Impact_Chest.m2` (the Eviscerate/Feint impact) — the BURST-flag split
    /// that bounds its visible burst at ~0.5 s on the reference: plume (e0) and crescents (e3)
    /// author file flag 0x8000 (one-shot burst — a single `ftol(rate)` puff at their 67 ms key),
    /// lava (e1) and dust (e2) are continuous, choreographed by their enabled windows. The
    /// cast-side `Eviscerate_Cast_Hands.m2` is the counter-case: the same 1.6 s asset shape with
    /// bit 15 clear — a continuous pour for the whole clip.
    #[test]
    fn real_feint_impact_authors_burst_emitters() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let impact = parse_m2_particle_emitters(
            &chain
                .read_file("Spells\\Feint_Impact_Chest.m2")
                .expect("read Feint_Impact_Chest.m2"),
        )
        .expect("parse emitters");
        assert_eq!(impact.len(), 4);
        assert!(impact[0].burst(), "plume is a one-shot burst");
        assert!(!impact[1].burst(), "lava pours (enabled window 0–333 ms)");
        assert!(!impact[2].burst(), "dust pours (enabled window 67–200 ms)");
        assert!(impact[3].burst(), "crescents are a one-shot burst");
        // The STEP rate law that arms the burst count: silent before the 67 ms key, the full
        // value at it, held past it (where the burst latch keeps it from ever mattering).
        assert_eq!(impact[0].emission_rate.step_ms(50.0), 0.0);
        assert_eq!(impact[0].emission_rate.step_ms(67.0), 30.0);
        assert_eq!(impact[0].emission_rate.step_ms(1500.0), 30.0);
        let cast = parse_m2_particle_emitters(
            &chain
                .read_file("Spells\\Eviscerate_Cast_Hands.m2")
                .expect("read Eviscerate_Cast_Hands.m2"),
        )
        .expect("parse emitters");
        assert!(
            cast.iter().all(|e| !e.burst()),
            "the cast-hands flame is continuous — same asset shape, opposite flag"
        );
    }

    /// The real `FlameStrike_Area.m2` — the spline-record law on real content (the
    /// `part-spline-file-layout.md` corpus caveat, resolved): e2..e5 are type-3 emitters whose
    /// control-point counts land exactly on the `3K+1` flattened-Bézier law (16 and 19), with
    /// the repurposed fields reading sanely (t window [0,1], no spin/scatter — the fire
    /// columns sit ON their authored descent curves with zero birth velocity).
    #[test]
    fn real_flamestrike_authors_spline_chains() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let defs = parse_m2_particle_emitters(
            &chain
                .read_file("Spells\\FlameStrike_Area.m2")
                .expect("read FlameStrike_Area.m2"),
        )
        .expect("parse emitters");
        let splines: Vec<_> = defs
            .iter()
            .filter(|d| d.shape == ParticleShape::Spline)
            .collect();
        assert_eq!(splines.len(), 4, "the four fire-column emitters");
        for d in &splines {
            let s = d.spline.as_ref().expect("chain parses");
            assert_eq!(s.points.len() % 3, 1, "3K+1 control points");
            assert!(s.points.len() >= 16);
            assert_eq!(d.area_length, 0.0, "tMin");
            assert_eq!(d.area_width, 1.0, "tMax");
            assert_eq!(d.vertical_range, 0.0, "no tangent spin");
            assert!(d.burst(), "one puff of standing flames");
            // The chain starts high (the authored descent) — a sane, in-model coordinate.
            assert!(
                (5.0..15.0).contains(&s.points[0][2]),
                "z {}",
                s.points[0][2]
            );
            assert_eq!(s.eval(0.0), s.points[0], "t=0 is the first point");
        }
    }

    /// The real `Fire_Cast_Hand.m2` (spell 133's "go" flash) and `MoltenBlast_Impact_Chest.m2`
    /// (its impact): the authored choreography this parse exists to carry. Cast hand: constant
    /// rates, enabled for exactly the first 200 ms of the clip. Impact emitter 0 (the plume
    /// burst): rate ramps 0→60 across the first 133 ms — a full second late under the old raw
    /// global-timeline read (seq 0 spans [1000, 2600]).
    #[test]
    fn real_fireball_effects_rebase_to_clip_time() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cast = parse_m2_particle_emitters(
            &chain
                .read_file("Spells\\Fire_Cast_Hand.m2")
                .expect("read Fire_Cast_Hand.m2"),
        )
        .expect("parse emitters");
        assert_eq!(cast.len(), 3);
        for em in &cast {
            assert!(em.enabled.on_at(100.0), "the flash is ON at its start");
            assert!(
                !em.enabled.on_at(250.0),
                "and OFF from 200 ms — the 1.0 s clip does not burn through"
            );
        }
        let impact = parse_m2_particle_emitters(
            &chain
                .read_file("Spells\\MoltenBlast_Impact_Chest.m2")
                .expect("read MoltenBlast_Impact_Chest.m2"),
        )
        .expect("parse emitters");
        assert_eq!(impact.len(), 6);
        assert_eq!(impact[0].emission_rate.step_ms(0.0), 0.0);
        assert_eq!(
            impact[0].emission_rate.step_ms(133.0),
            60.0,
            "the plume bursts at impact"
        );
        assert!(impact[1].enabled.on_at(100.0));
        assert!(
            !impact[1].enabled.on_at(400.0),
            "shockwave window is 300 ms"
        );
        assert!(!impact[4].enabled.on_at(50.0), "smoke starts staggered…");
        assert!(impact[4].enabled.on_at(200.0));
        assert!(!impact[4].enabled.on_at(700.0), "…and ends by 567 ms");
    }
}
