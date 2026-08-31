//! The server-authored path walk ([`Spline`], `SMSG_MONSTER_MOVE`) and the terrain re-ground that
//! goes with it ([`ground_clamp_creatures`]) — the creature half of [`super`]'s motion model
//! (decisions 0052/0059/0097).

use std::sync::OnceLock;
use std::time::Duration;
use bevy::platform::time::Instant;

use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use benilla_protocol::{CreateSpline, EntityKind};
use bevy::prelude::*;

use crate::entities::CollisionHeight;
use crate::player::swim_enter_depth;

use super::super::{NetEntity, ObjectStore};

/// A server-dictated movement path (`SMSG_MONSTER_MOVE`): the unit traverses `points` at constant
/// speed over `duration` from `start`. All points are raw WoW coords; [`sample_splines`] interpolates
/// each frame into the entity's [`Transform`]. Present only while the unit is path-walking.
#[derive(Component, Debug, Clone)]
pub(crate) struct Spline {
    pub(crate) points: Vec<[f32; 3]>,
    pub(crate) start: Instant,
    pub(crate) duration: Duration,
    /// The server's spline id (`SMSG_MONSTER_MOVE`). The controller echoes it in
    /// `CMSG_MOVE_SPLINE_DONE` when this spline drove our OWN player (Charge/knockback/taxi) — the
    /// server validates the ack against the newest spline id. Irrelevant for a creature's walk.
    pub(crate) id: u32,
    /// A **ground walk** (the spline's `FLYING` bit was clear): the real client discards the path's Z
    /// and re-derives it from the terrain under the unit (byte-verified — decision 0059), so
    /// [`ground_clamp_creatures`] snaps this unit onto benilla's terrain. `false` for a flying path,
    /// which keeps the spline's own Z.
    pub(crate) grounded: bool,
    /// The path's `SPLINEFLAG_RUNMODE` bit — **a run when set, a walk when clear**, and the clear
    /// case is the one that does something: the real client's `SMSG_MONSTER_MOVE` commit
    /// `0x7c6a50` feeds this bit to `CMovement::SetRunMode 0x7c71c0` (`0x7c6ac2 and edi,0x100`;
    /// `0x7c6acb call`), whose argument is *run*, so **a spline without RUNMODE sets
    /// `MOVEFLAG_WALK_MODE` on the unit it moves.** Every incoming spline re-authors the bit
    /// (wow-re `collision/scratch/walk-mode-law.md` §5.2; decision 1758).
    ///
    /// Only the body **we drive** reads it ([`crate::player::server_ride`]) — a creature's own
    /// gait is derived from [`Spline::speed`], the path's arc length over its duration, so its
    /// walk bit would select nothing.
    pub(crate) run_mode: bool,
}

/// **A spline the server ended by decree**, carrying the id the acknowledgement owes (decision
/// 1281). `SMSG_MONSTER_MOVE`'s stop form has no path to walk, so it leaves no [`Spline`] behind —
/// but it is a spline launch like any other on the server side, with its own fresh id, and vmangos
/// arms `HasPendingSplineDone` for it whenever the unit is a player or a player's possessed
/// creature (`MoveSplineInit::Launch`, and `Unit::StopMoving` says so in as many words: *"Will
/// trigger CMSG_MOVE_SPLINE_DONE from client"*).
///
/// Until that acknowledgement lands — carrying **this** id, not the interrupted path's —
/// `HandleMovementOpcodes` drops every movement packet the client sends for that unit. So an
/// interrupted flee, charge or knockback needs the id kept, not discarded with the path.
/// [`crate::player::server_ride`] consumes it for the body in our hands; on anything else it is
/// inert bookkeeping, replaced by the next stop and cleared by the next real path.
#[derive(Component, Debug, Clone, Copy)]
pub(crate) struct SplineStopped(pub(crate) u32);

impl Spline {
    /// Average ground speed of the path in yards/second (total length ÷ duration) — what the creature
    /// animation selector ([`crate::creature_anim`]) reads to choose Walk vs Run.
    pub(crate) fn speed(&self) -> f32 {
        let length: f32 = self
            .points
            .windows(2)
            .map(|w| {
                let (dx, dy, dz) = (w[1][0] - w[0][0], w[1][1] - w[0][1], w[1][2] - w[0][2]);
                (dx * dx + dy * dy + dz * dz).sqrt()
            })
            .sum();
        length / self.duration.as_secs_f32().max(1e-3)
    }

    /// How far through the ride this path is, `0..=1` — elapsed over duration, clamped. The
    /// inspector's motion line reads it; [`sample`](Self::sample) computes the same fraction to
    /// locate the unit, and a path at `1` is one frame from being dropped.
    pub(crate) fn elapsed_frac(&self) -> f32 {
        (Instant::now()
            .saturating_duration_since(self.start)
            .as_secs_f32()
            / self.duration.as_secs_f32().max(1e-3))
        .clamp(0.0, 1.0)
    }

    /// Interpolated `(raw-WoW position, facing, travel pitch)` at `now`, constant speed along the
    /// path (clamped at the ends). `facing` is the WoW orientation of the travel direction in
    /// progress (`atan2` of its horizontal component), `None` for a degenerate/vertical direction so
    /// the caller keeps prior facing. The **travel pitch** (radians, +up) is the direction's
    /// `asin(dz/len)` — the observed-mover pitch rule `asin(dir.z)` — `0` at rest/level; read by the
    /// swimming-creature body pitch (its consumer gates on [`CreatureSwimming`], so a ground
    /// walker's slope never tilts it).
    ///
    /// A **ground** path evaluates each segment as a straight lerp (the byte-verified
    /// `linear_pos_diff` follow — wow-re curvemath RF-0048). A **flying** path (`!grounded` — the
    /// wire's `Flying`/`Mask_CatmullRom` bit) evaluates a uniform **Catmull-Rom** through the same
    /// waypoints, neighbours phantom-duplicated at the ends per the client's CCurve commit
    /// (RF-0052) — smooth arcs, no corners on a taxi flight. **Byte-VERIFIED** (decision 0496
    /// folds back 0484 I3): the flying commit writes curve mode 1 (`0x7c6a50` →
    /// `[movInfo+0x54]`), and the point-at-t evaluator `0x4541b0` is itself the mode dispatcher —
    /// mode 1 calls the Horner-basis Catmull-Rom cubic (`0x453580`), mode 0 the 2-point lerp
    /// (wow-re `system/curvemath/scratch/taxi-flying-curve-mode.md`). Segment *location* stays
    /// chord-length parameterised in both modes (RF-0052 fills per-segment chord + total
    /// arc-length).
    pub(crate) fn sample(&self, now: Instant) -> ([f32; 3], Option<f32>, f32) {
        let pts = self.points.as_slice();
        if pts.len() < 2 {
            return (pts.first().copied().unwrap_or([0.0; 3]), None, 0.0);
        }
        let seg = |a: [f32; 3], b: [f32; 3]| {
            let (dx, dy, dz) = (b[0] - a[0], b[1] - a[1], b[2] - a[2]);
            (dx * dx + dy * dy + dz * dz).sqrt()
        };
        let lengths: Vec<f32> = pts.windows(2).map(|w| seg(w[0], w[1])).collect();
        let total: f32 = lengths.iter().sum();
        if total <= f32::EPSILON {
            return (pts[0], None, 0.0);
        }
        let frac = (now.saturating_duration_since(self.start).as_secs_f32()
            / self.duration.as_secs_f32().max(1e-3))
        .clamp(0.0, 1.0);
        let mut want = frac * total;
        for (i, &len) in lengths.iter().enumerate() {
            if want <= len || i + 1 == lengths.len() {
                let t = if len > f32::EPSILON {
                    (want / len).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (pos, dir) = if self.grounded {
                    let (a, b) = (pts[i], pts[i + 1]);
                    let pos = [
                        a[0] + (b[0] - a[0]) * t,
                        a[1] + (b[1] - a[1]) * t,
                        a[2] + (b[2] - a[2]) * t,
                    ];
                    let dir = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                    (pos, dir)
                } else {
                    catmull_rom(pts, i, t)
                };
                let (dx, dy, dz) = (dir[0], dir[1], dir[2]);
                let facing = (dx * dx + dy * dy > 1e-6).then(|| dy.atan2(dx));
                let dlen = (dx * dx + dy * dy + dz * dz).sqrt();
                let pitch = if dlen > f32::EPSILON {
                    (dz / dlen).clamp(-1.0, 1.0).asin()
                } else {
                    0.0
                };
                return (pos, facing, pitch);
            }
            want -= len;
        }
        (*pts.last().unwrap(), None, 0.0)
    }

    /// The **flying attitude** `(pitch, bank)` in radians — the byte law (wow-re `taxi-system.md`
    /// §TU-4, `0x7c5490`'s flying branch decomposed at §5 grade; decision 0516, correcting 0501's
    /// INTERIM look-ahead *pitch*):
    ///
    /// - **Pitch** is the instantaneous tangent's own climb — [`Spline::sample`]'s travel pitch,
    ///   `asin(t̂z)` — not a look-ahead difference: the client orients the mover's forward along
    ///   the spline tangent (`mover+0x5c`), so the climb angle IS that tangent's vertical
    ///   component. (The `mover+0x20` asin scalar is the SWIM branch's — flight never reaches it.)
    /// - **Bank**: `θ = 2·sign(cross)·acos(clamp(t̂ₓᵧ·d̂ₓᵧ, −1, 1))` between the XY-normalized
    ///   tangent and the XY direction to the **1000 world look-ahead** point (`0x3e8` @ `0x7c5623`;
    ///   a degenerate XY length — the client's eps `2.384e-7` — banks 0), then the **snap** — not
    ///   a soft clamp: `θ < −π/2 → −π`, `θ ≥ +π/2 → +π` (the sharp-corner/antipodal guard: a
    ///   hairpin reads as a momentary full roll, visible on the reference at switchbacks). No
    ///   ease, no damping — recomputed and applied per frame, exactly like the client. The lean
    ///   DIRECTION (into the turn: `cross > 0` = a left turn = roll left) is INFERRED — the
    ///   stored row's downstream matrix plumbing wasn't traced; magnitude, ×2, and snap are
    ///   byte-pinned.
    pub(crate) fn flight_attitude(&self, now: Instant) -> (f32, f32) {
        let (pos, facing, pitch) = self.sample(now);
        let Some(f) = facing else {
            return (pitch, 0.0);
        };
        let (look, ..) = self.sample(now + Duration::from_millis(1000));
        let (dx, dy) = (look[0] - pos[0], look[1] - pos[1]);
        let dlen = (dx * dx + dy * dy).sqrt();
        if dlen <= 2.384e-7 {
            return (pitch, 0.0);
        }
        let (tx, ty) = (f.cos(), f.sin());
        let (ux, uy) = (dx / dlen, dy / dlen);
        let dot = (tx * ux + ty * uy).clamp(-1.0, 1.0);
        let cross = tx * uy - ty * ux;
        let theta = if cross < 0.0 { -dot.acos() } else { dot.acos() } * 2.0;
        let bank = if theta < -std::f32::consts::FRAC_PI_2 {
            -std::f32::consts::PI
        } else if theta >= std::f32::consts::FRAC_PI_2 {
            std::f32::consts::PI
        } else {
            theta
        };
        (pitch, bank)
    }
}

/// Uniform Catmull-Rom position + travel direction on the polyline segment `pts[i] → pts[i+1]` at
/// local parameter `u ∈ [0,1]`, with the neighbour control points phantom-duplicated at the path
/// ends — the real client's CCurve commit stores `[first, first, …, last, last]` (wow-re curvemath
/// RF-0052), which makes the curve pass through every waypoint and start/end tangent to the end
/// segments. Returns `(position, d/du tangent)`; the tangent's scale is meaningless to callers
/// (they normalize or `atan2` it), only its direction matters.
fn catmull_rom(pts: &[[f32; 3]], i: usize, u: f32) -> ([f32; 3], [f32; 3]) {
    let p0 = pts[i.saturating_sub(1)];
    let p1 = pts[i];
    let p2 = pts[i + 1];
    let p3 = pts[(i + 2).min(pts.len() - 1)];
    let (u2, u3) = (u * u, u * u * u);
    let mut pos = [0.0f32; 3];
    let mut dir = [0.0f32; 3];
    for a in 0..3 {
        // The standard uniform C-R basis: p(u) = ½·(2P₁ + (−P₀+P₂)u + (2P₀−5P₁+4P₂−P₃)u² +
        // (−P₀+3P₁−3P₂+P₃)u³); dir is its analytic d/du.
        let c1 = p2[a] - p0[a];
        let c2 = 2.0 * p0[a] - 5.0 * p1[a] + 4.0 * p2[a] - p3[a];
        let c3 = -p0[a] + 3.0 * p1[a] - 3.0 * p2[a] + p3[a];
        pos[a] = 0.5 * (2.0 * p1[a] + c1 * u + c2 * u2 + c3 * u3);
        dir[a] = 0.5 * (c1 + 2.0 * c2 * u + 3.0 * c3 * u2);
    }
    (pos, dir)
}

/// Build the [`Spline`] implied by one `SMSG_MONSTER_MOVE`: the unit rides `path` — the full travel-order
/// polyline `[start, …waypoints…, endpoint]` the protocol decoded — at constant (arc-length) speed over
/// `duration_ms`. [`Spline::sample`] interpolates a ground path piecewise-linearly, which is faithful: the
/// real client's ground creature-follow evaluates the path with `linear_pos_diff` (a segment lerp),
/// arc-length parameterised, through every waypoint (wow-re curvemath RF-0048/RF-0052); a **flying** path
/// takes the Catmull-Rom family instead (the taxi/flight look — see [`Spline::sample`]'s INTERIM note).
/// Returns `None` — "stationary, clear any path" — for a `Stop`, a zero duration, or a path with fewer
/// than two points (nothing to travel along).
pub(in crate::net) fn monster_move_spline(
    path: Vec<[f32; 3]>,
    spline_id: u32,
    stop: bool,
    duration_ms: u32,
    flying: bool,
    run_mode: bool,
) -> Option<Spline> {
    if stop || duration_ms == 0 || path.len() < 2 {
        return None;
    }
    Some(Spline {
        points: path,
        start: Instant::now(),
        duration: Duration::from_millis(u64::from(duration_ms)),
        id: spline_id,
        grounded: !flying,
        run_mode,
    })
}

/// Build the [`Spline`] a unit is **already riding** at the moment it streams into view — its create
/// block's `MOVEFLAG_SPLINE_ENABLED` tail (decision 0708). Same path, same sampler as
/// [`monster_move_spline`]; the one difference is *where the ride starts*: the server tells us how much
/// of the path it has already covered ([`CreateSpline::time_passed_ms`]), so the spline's clock is
/// **back-dated** by that much and [`Spline::sample`] picks the walk up exactly where the server has it,
/// rather than restarting it from the top.
///
/// Returns `None` — "this unit is not walking, leave it at its create pose" — for a degenerate path, a
/// zero duration, or a ride the server has already finished (`time_passed ≥ duration`: the create pose
/// *is* the endpoint). `WOW_CREATE_SPLINE=off` also returns `None` for everything, restoring the
/// pre-0708 behaviour for an A/B.
pub(in crate::net) fn create_spline(spline: CreateSpline) -> Option<Spline> {
    if !create_spline_enabled()
        || spline.duration_ms == 0
        || spline.path.len() < 2
        || spline.time_passed_ms >= spline.duration_ms
    {
        return None;
    }
    let passed = Duration::from_millis(u64::from(spline.time_passed_ms));
    Some(Spline {
        points: spline.path,
        // `checked_sub` because `Instant` has no epoch to spare: a client started seconds after boot
        // can be handed a spline older than its own monotonic clock, and plain `-` panics there.
        start: Instant::now()
            .checked_sub(passed)
            .unwrap_or_else(Instant::now),
        duration: Duration::from_millis(u64::from(spline.duration_ms)),
        id: spline.id,
        grounded: !spline.flying,
        run_mode: spline.run_mode,
    })
}

/// One `csp` line per create block that carried a live spline — the **supply** half of the
/// spawn-freeze instrument (decision 0708), written to the shared `WOW_MOVE_TRACE` sink so it
/// interleaves with everything else on one clock. Logged from the wire, *before* the ride is
/// interpreted, so the `WOW_CREATE_SPLINE=off` leg records the same lines as the fixed one and the two
/// are directly diffable.
///
/// `left` is the number that matters: the yards of path still ahead of the server at create time — i.e.
/// exactly how far this unit will drift from us while we hold it still, and therefore how big a jump
/// its next `SMSG_MONSTER_MOVE` has to make up. Read it against the `mmv` lines' realized snaps.
pub(in crate::net) fn trace_create_spline(guid: u64, spline: Option<&CreateSpline>) {
    if !benilla_assets::trace::enabled() {
        return;
    }
    let Some(s) = spline else { return };
    let length: f32 = s
        .path
        .windows(2)
        .map(|w| {
            let (dx, dy, dz) = (w[1][0] - w[0][0], w[1][1] - w[0][1], w[1][2] - w[0][2]);
            (dx * dx + dy * dy + dz * dz).sqrt()
        })
        .sum();
    let ridden = s.time_passed_ms as f32 / s.duration_ms.max(1) as f32;
    let left = length * (1.0 - ridden).clamp(0.0, 1.0);
    benilla_assets::trace::line(
        "csp",
        &format!(
            "{guid:#x} nodes={} len={length:.2} left={left:.2} t={}/{} world{}{}{}",
            s.path.len(),
            s.time_passed_ms,
            s.duration_ms,
            if s.flying { " flying" } else { "" },
            if s.cyclic { " cyclic" } else { "" },
            if create_spline_enabled() {
                ""
            } else {
                " DROPPED(WOW_CREATE_SPLINE=off)"
            },
        ),
    );
}

/// One `mmv` line per `SMSG_MONSTER_MOVE` — the **realized** half of the same instrument: the distance
/// from where we were drawing the unit to where the server says its new path starts, which is the
/// teleport the director sees. `?` when the unit has no transform yet (its spawn command hasn't
/// flushed), which is not a snap at all.
///
/// Split into `xy` and `z` on purpose: only the **horizontal** part is a desync. A grounded creature's
/// Z is deliberately ours, not the server's — [`ground_clamp_creatures`] re-derives it from our terrain
/// (decision 0059), so on a slope a correctly-followed creature still reads a Z difference of a yard or
/// two against the wire. Read `xy`; `z` is the terrain disagreement, and reading the 3-D total instead
/// would bury a clean follow in hill noise.
pub(in crate::net) fn trace_move_snap(
    guid: u64,
    from: Option<[f32; 3]>,
    start: [f32; 3],
    stop: bool,
    duration_ms: u32,
) {
    if !benilla_assets::trace::enabled() {
        return;
    }
    let snap = from.map_or("xy=? z=?".to_string(), |f| {
        let (dx, dy, dz) = (start[0] - f[0], start[1] - f[1], start[2] - f[2]);
        format!("xy={:.2} z={dz:+.2}", (dx * dx + dy * dy).sqrt())
    });
    benilla_assets::trace::line(
        "mmv",
        &format!(
            "{guid:#x} {snap} start=[{:.2},{:.2},{:.2}] dur={duration_ms}{}",
            start[0],
            start[1],
            start[2],
            if stop { " stop" } else { "" },
        ),
    );
}

/// The A/B switch behind [`create_spline`]: `WOW_CREATE_SPLINE=off` drops every create-block spline,
/// which is exactly what the client did before decision 0708 (creatures frozen at first sight until
/// their next `SMSG_MONSTER_MOVE` snapped them forward).
fn create_spline_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        !std::env::var("WOW_CREATE_SPLINE").is_ok_and(|v| matches!(v.as_str(), "off" | "0"))
    })
}

/// Interpolate every path-walking entity along its [`Spline`] into its [`Transform`] each frame — so
/// motion stays smooth between the sparse server `MSG_MOVE` packets. Writes translation + rotation
/// only (scale, set by the renderer when it attaches the model, is preserved).
#[allow(clippy::type_complexity)] // one Bevy system's full input set
pub(in crate::net) fn sample_splines(
    mut commands: Commands,
    mut q: Query<(
        Entity,
        &Spline,
        &mut Transform,
        Has<CreatureSwimming>,
        // The rider's server identity, for the `spl` trace alone — `Option` because the sampler
        // must never gate on it (a spline entity with no guid still has to be flown).
        Option<&super::super::Guid>,
    )>,
    mut trace_next: Local<f32>,
    time: Res<Time>,
) {
    let now = Instant::now();
    // The `spl` sampler's own clock (see [`trace_ride`]): one tick per second for the WHOLE
    // population, not per entity — a shared deadline keeps every rider's line on the same instant,
    // which is what makes two units' progress directly comparable in the file.
    let tracing = benilla_assets::trace::enabled_for("spl");
    let tick = tracing && time.elapsed_secs() >= *trace_next;
    if tick {
        *trace_next = time.elapsed_secs() + SPL_TRACE_SECS;
    }
    for (entity, spline, mut t, swimming, guid) in &mut q {
        let (wow_pos, facing, pitch) = spline.sample(now);
        // What the transform held *coming in* — i.e. whatever the previous frame left there after
        // every other writer had its turn. Traced beside the fresh sample as `was=`: a rider whose
        // `was` is not last frame's sample is being stomped by another system, which no amount of
        // staring at the sampler would ever show.
        let was = tick.then(|| bevy_to_wow(t.translation));
        t.translation = wow_to_bevy(wow_pos);
        if let Some(f) = facing {
            // The swim body pitch (TU-A's render law, applied to the spline movers): a swimming
            // creature moving along its path renders its root pitched to the segment's travel
            // pitch, nose-up positive about the body's local X; ground walkers render level. A
            // FLYING spline (the taxi) takes the full attitude — the tangent's climb plus the
            // look-ahead BANK ([`Spline::flight_attitude`], decision 0516) — on the unit
            // transform, so mount and rider tilt and lean as one composite, any model (0501's
            // placement law). Roll composes innermost: about the body's travel axis.
            let (pitch, bank) = if !spline.grounded {
                spline.flight_attitude(now)
            } else if swimming {
                (pitch, 0.0)
            } else {
                (0.0, 0.0)
            };
            t.rotation = if pitch != 0.0 || bank != 0.0 {
                Quat::from_rotation_y(f)
                    * Quat::from_rotation_x(pitch)
                    * Quat::from_rotation_z(bank)
            } else {
                Quat::from_rotation_y(f)
            };
        }
        if let Some(was) = was {
            trace_ride(guid.map_or(0, |g| g.0), spline, wow_pos, was, now);
        }
        // Path finished: the final pose is written above (the sample clamps to the last point), so drop
        // the spline. It's what makes a `Spline` mean "actively moving" — otherwise a completed path
        // lingers until the next packet and a creature reads as walking forever after one move
        // (`creature_anim` keys Walk/Run on the spline's presence).
        if now.saturating_duration_since(spline.start) >= spline.duration {
            if tracing {
                benilla_assets::trace::line(
                    "spl",
                    &format!("{:#x} DONE — spline dropped", guid.map_or(0, |g| g.0)),
                );
            }
            commands.entity(entity).remove::<Spline>();
        }
    }
}

/// How often the `spl` tag samples a live ride. One second is the resolution a "does this thing
/// actually move?" question needs, and it keeps a zone full of walkers off the shared trace mutex
/// (see [`benilla_assets::trace`] on why a busy tag distorts the run it measures).
const SPL_TRACE_SECS: f32 = 1.0;

/// One `spl` line per live [`Spline`] per [`SPL_TRACE_SECS`] — the **ride** half of the movement
/// instrument, beside `csp`'s supply and `mmv`'s realized snaps (decision 0708). Those two log at
/// the wire; this one logs what the client is actually *drawing*, which is the only thing that
/// answers "the server says it is flying and it looks frozen to me".
///
/// `t=` is the ride's own progress (elapsed/duration, seconds), `pos` the sampled raw-WoW point,
/// `spd` the path's constant speed (its full length over its full duration), and `was=` the
/// distance from what the transform held *coming into this frame* to the fresh sample. The three
/// separate the three faults that all look identical on screen: a ride whose `pos` does not change
/// while `t` advances is a **sampler** fault; a ride whose `spd` is wrong is a **wire** fault; and
/// a `was=` far larger than one frame of travel is a **stomp** — some other system writing this
/// transform after us.
fn trace_ride(guid: u64, spline: &Spline, pos: [f32; 3], was: [f32; 3], now: Instant) {
    let elapsed = now.saturating_duration_since(spline.start).as_secs_f32();
    let (dx, dy, dz) = (pos[0] - was[0], pos[1] - was[1], pos[2] - was[2]);
    let drift = (dx * dx + dy * dy + dz * dz).sqrt();
    benilla_assets::trace::line(
        "spl",
        &format!(
            "{guid:#x} t={elapsed:.1}/{:.1} pos=[{:.2},{:.2},{:.2}] was={drift:.3} spd={:.2} nodes={} {}",
            spline.duration.as_secs_f32(),
            pos[0],
            pos[1],
            pos[2],
            spline.speed(),
            spline.points.len(),
            if spline.grounded { "ground" } else { "flying" },
        ),
    );
}

/// Distance (yd) above a creature's current feet that the terrain probe starts. Generous enough to
/// clear the small float (a slightly-high server Z) and the "little hill" a straight-line spline Z
/// leaves a unit inside, low enough not to grab an overhang/bridge a unit walks *under*. It also
/// **self-gates a genuinely-airborne unit**: a creature hovering/flying farther than this above the
/// ground leaves the surface out of the probe's reach, so it is never clamped. (A full swept
/// controller — the real client's mechanism, which can't tunnel into rising ground at all — is the
/// follow-up for large deviations; decision 0059.)
const GROUND_CLAMP_UP: f32 = 2.5;
/// Distance (yd) below the origin the probe additionally reaches, so a unit follows a small step/slope
/// *down* onto the surface. Total cast length is `GROUND_CLAMP_UP + GROUND_CLAMP_DOWN`.
const GROUND_CLAMP_DOWN: f32 = 4.0;

/// Snap every **grounded creature** onto benilla's own terrain — the path-walkers (a ground spline)
/// **and the idle ones standing at their raw spawn Z** (the "NPCs floating a bit"). The real client
/// doesn't trust the wire Z for a ground unit: a walker re-derives Z from the surface (byte-verified —
/// the grounded fork zeroes the spline Z-delta and the WALK resolver reads Z off the world trace), and
/// an idle unit reads grounded against the reference too (the exact idle path isn't byte-pinned yet —
/// decision 0059). We mirror the *behaviour*: cast a ray straight down against the terrain/WMO
/// **walking** colliders — the same set the player stands on ([`benilla_world::collision::WorldCollision::body_filter`]) — and set the
/// unit's Y to the hit.
///
/// Scope: **`Unit` creatures only** (a player owns its Z via the controller / `RemoteMotion`; a
/// GameObject sits at its authored Z — a lamp on a table, a chest on a ledge). A **flying** spline
/// keeps its own Z, and so does a **swimming** creature ([`CreatureSwimming`]): its wire Z *is* its
/// swim depth — vmangos paths a water creature in 3D through the volume with a plain non-FLYING
/// spline (verified `MoveSplineInit`/`WaypointMovementGenerator.cpp`: only `CanFly()` sets the flag),
/// so ground-clamping it dragged murlocs to the lakebed. Runs right after [`sample_splines`] so a
/// walker's freshly-sampled XY+Z is what we re-ground.
///
/// **The probe measures from the unit's SEAT, never from the clamp's own last answer** (decision
/// 1384). The seat is the Z whoever owns this unit's position last wrote — the create block, an
/// `SMSG_UPDATE_OBJECT` move, its spline, a transport deck composing its rider — and it is what the
/// server means by "where this unit is". Feeding the previous *output* back in as the next input is
/// what made a single wrong answer permanent: a unit created inside a building at world entry, in
/// the window before that building's own floor collider had attached, found the terrain under the
/// building instead, dropped onto it, and from down there the floor was above the probe's reach
/// forever after. Deriving from the seat makes every frame's answer a pure function of (the
/// server's pose, the colliders) — so the moment the floor lands, the unit is back on it, with no
/// movement needed to shake it loose. A probe **miss** — genuinely airborne, or the ground here
/// hasn't streamed in — puts the unit at its seat, which is exactly where an unclamped unit belongs.
#[allow(clippy::type_complexity)] // one Bevy system's full input set
pub(in crate::net) fn ground_clamp_creatures(
    world: benilla_world::collision::WorldCollision,
    epoch: Res<benilla_world::collision::ColliderEpoch>,
    mut commands: Commands,
    mut q: Query<(
        Entity,
        &NetEntity,
        Option<&Spline>,
        &mut Transform,
        Option<&mut GroundClamped>,
        Has<CreatureSwimming>,
    )>,
) {
    let cost = clamp_cost_enabled();
    let legacy = clamp_seat_disabled();
    let t0 = cost.then(bevy::platform::time::Instant::now);
    let (mut visited, mut skipped, mut held, mut cast, mut hit_n, mut moved) =
        (0u32, 0u32, 0u32, 0u32, 0u32, 0u32);
    // What re-armed each cast (1384): the unit's own seat moved, or the world's colliders changed
    // under a unit that didn't. The second must be ~0 in a settled scene — a nonzero steady-state
    // `armed` means the collider set is churning and the gate is holding nothing.
    let (mut reseat, mut armed) = (0u32, 0u32);

    for (entity, net, spline, mut t, mut clamped, swimming) in &mut q {
        visited += 1;
        if net.kind != EntityKind::Unit {
            skipped += 1;
            continue; // players + GameObjects own their Z
        }
        if spline.is_some_and(|s| !s.grounded) {
            skipped += 1;
            continue; // a flying path is authoritative on Z
        }
        if swimming {
            skipped += 1;
            continue; // in-liquid: the wire Z is the creature's swim depth
        }
        let xz = [t.translation.x, t.translation.z];
        // **Re-seat on any write that wasn't ours.** The Y standing here differs from the one this
        // unit's last clamp left behind exactly when somebody who owns this unit's position moved
        // it — the wire, its spline, a transport deck — and *that* Y is the authority the probe
        // measures from. With no memo at all (a unit's first frame), the pose it spawned at is the
        // seat, which is the create block's own Z.
        let seat_y = match clamped.as_deref() {
            _ if legacy => t.translation.y, // the pre-1384 leg: measure from our own last answer
            Some(c) if c.y_written == t.translation.y => c.seat_y,
            _ => t.translation.y,
        };
        // The cast gate (decision 1357): a unit whose seat and XZ are bit-identical to the cast
        // that produced its cached hit, in a world whose collider set has not changed since, cannot
        // get a different answer — those three ARE the ray's inputs. A moving floor (a lift, a
        // transport deck) reaches its rider through the wire's own position writes, which move the
        // seat and re-arm the cast; a building whose floor collider attaches a few frames after the
        // unit was created moves the epoch and re-arms it (decision 1384 — before the epoch was in
        // the gate, that unit's wrong answer was cached for the session). A MISS never caches, so a
        // unit standing on a tile whose collider hasn't streamed in keeps asking until it lands.
        if let Some(c) = clamped.as_deref() {
            let same_question = if legacy {
                c.y_written == t.translation.y
            } else {
                c.seat_y == seat_y && c.epoch == epoch.get()
            };
            if c.hit && c.xz == xz && same_question {
                held += 1;
                continue;
            }
            if cost && !legacy {
                if c.xz == xz && c.seat_y == seat_y {
                    armed += 1; // the world changed under a unit that didn't move
                } else {
                    reseat += 1; // the unit moved, or was moved
                }
            }
        }
        let origin = Vec3::new(t.translation.x, seat_y + GROUND_CLAMP_UP, t.translation.z);
        let reach = GROUND_CLAMP_UP + GROUND_CLAMP_DOWN;
        cast += 1;
        // The one-sided down-ray (decision 0970): a creature grounds like the player grounds — a
        // face whose winding points away is no floor, or an idle NPC would stand mid-air on the
        // very shell face the player mover now falls through.
        let hit = world.ray_body(origin, Dir3::NEG_Y, reach);
        // The down-ray's hit point Y = feet on the surface; no surface in reach = the seat, i.e.
        // where the server put it.
        let y = match &hit {
            Some(h) => {
                hit_n += 1;
                origin.y - h.distance
            }
            None => seat_y,
        };
        // Exact bit equality is deliberate, not a sloppy float compare: the question is "would the
        // write change anything" — Bevy's change detection fires on the DerefMut regardless of
        // value, so writing an equal Y every frame marked every standing creature's transform
        // subtree dirty. An epsilon would answer a different question: a real sub-epsilon ground
        // shift must still land.
        if y != t.translation.y {
            moved += 1;
            t.translation.y = y;
        }
        let state = GroundClamped {
            xz,
            seat_y,
            y_written: t.translation.y,
            hit: hit.is_some(),
            epoch: epoch.get(),
        };
        match clamped.as_deref_mut() {
            Some(c) => *c = state,
            None => {
                commands.entity(entity).insert(state);
            }
        }
    }

    if let Some(t0) = t0 {
        // `WOW_CLAMP_COST=1` — the premise check for gating this sweep (0732 slice S), which
        // 0732 sized at 0.42 traced against avian's `SpatialQuery::cast_ray`. **0970 replaced
        // that ray** with a broadphase-plus-BVH gather, so the recorded price measures a
        // mechanism that no longer exists and the lane is honestly unsized until this prints.
        //
        // The two fields that decide the two possible gates, and their kill conditions:
        //   · `cast` vs `visited`  — how much of the walk even reaches a ray. A movement gate can
        //     only ever save the `cast` share; if `ms` is small this whole item is dead.
        //   · `moved` vs `hit`     — how often the write actually CHANGES Y. Every hit writes
        //     `Transform` unconditionally today, and a write dirties the transform subtree whether
        //     or not the value differs. If `moved` is a small fraction of `hit`, an equality gate
        //     on the write is the cheap half and needs no movement tracking at all.
        //
        // Per 0734's law (~10.5 ns per row visit), the walk itself is never the cost here: at ~800
        // units it is ~8 µs. Only `ms` justifies the slice — quote it, not the counts.
        eprintln!(
            "[clamp-cost] visited={visited} skipped={skipped} held={held} cast={cast} reseat={reseat} armed={armed} hit={hit_n} moved={moved} ms={:.3}",
            t0.elapsed().as_secs_f32() * 1000.0
        );
    }
}

/// Whether the ground-clamp meter is armed (`WOW_CLAMP_COST`). Read once, then a relaxed bool.
fn clamp_cost_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WOW_CLAMP_COST").is_some())
}

/// The A/B switch behind decision 1384: `WOW_CLAMP_SEAT=off` restores the pre-1384 clamp, which
/// measured from its own previous answer and cached a hit without dating it against the collider
/// set. That is the leg where a unit created inside a building before the building's floor collider
/// attached stays under the floor for the session (B197) — kept as the lever that reproduces the bug
/// on the fixed binary, so the fix's evidence never depends on two different builds.
fn clamp_seat_disabled() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| {
        std::env::var("WOW_CLAMP_SEAT").is_ok_and(|v| matches!(v.as_str(), "off" | "0"))
    })
}

/// What [`mark_swimming_creatures`] reads per unit: identity, kind, pose, its own collision height
/// (`None` only on a unit's first frame, before the stamp), its `UNIT_FIELD_FLAGS` (the enter gate
/// — [`crate::player::may_swim`]), its own WMO-room claim (whose liquid may answer for it —
/// decision 0696), and whether it is already marked.
/// The gate: a stamped unit re-enters the query only when its `Transform` moved (spline ticks,
/// teleports, clamp writes) or its descriptors changed (a `UNIT_FIELD_FLAGS` delta — a pet losing
/// PLAYER_CONTROLLED, a GM `.modify unitflag` — must re-ask); an unstamped one is retried every
/// frame until it evaluates. `Changed<ObjectStore>` also covers the *insert*, which is what lets a
/// unit whose descriptor block lands a frame after its transform still evaluate.
type SwimMarkGate = Or<(
    Changed<Transform>,
    Changed<ObjectStore>,
    Without<SwimEvaluated>,
)>;

type SwimMarkQuery = (
    Entity,
    &'static NetEntity,
    &'static Transform,
    Option<&'static CollisionHeight>,
    Option<&'static ObjectStore>,
    Has<CreatureSwimming>,
    Has<SwimEvaluated>,
);

/// This unit has been through [`mark_swimming_creatures`] with its room claim settled — the
/// change-gate's stamp (decision 1445). A stationary unit's swim state cannot change while the
/// world stands still (1.12 water is static; the boundary depends only on the unit's own
/// position and height), so a stamped unit re-evaluates only on `Changed<Transform>` — which
/// every spline tick, teleport and clamp correction raises. The world NOT standing still is the
/// [`ColliderEpoch`] (1384's law, liquid-flavoured): liquid arrives with the same streamed
/// tiles/WMOs that attach colliders, so an epoch bump drops every stamp and the population
/// re-evaluates once. Before the gate, `water_surface_at` ran for every streamed unit every
/// frame — ~0.13 traced ms/frame at the Goldshire pin, almost all of it standing idlers.
#[derive(Component)]
pub(in crate::net) struct SwimEvaluated;

/// The exit band (yd) below the swim boundary where a marked swimmer stays marked — the player
/// boundary's **VERIFIED** 1/36-yd hysteresis (`0x7ff9d0`), an absolute distance independent of
/// collision height, so it is subtracted from the boundary rather than scaled with it.
const CREATURE_SWIM_EXIT_BAND: f32 = 1.0 / 36.0;

/// A **creature** currently past the swim boundary — the client-side derivation of the swim state
/// the wire never carries for creatures: vmangos sets `MOVEFLAG_SWIMMING` only from player packets,
/// and a water creature's create block + splines carry no swim marker at all (verified
/// `vmangos-src`, 2026-07-17). The real client runs its own depth decision `0x6030c0` on **remote
/// units and creatures too**, not only on the body it steers — VERIFIED, wow-re
/// `collision/scratch/remote-swim-decision.md` §1. `0x6030c0` has exactly one caller and it reaches
/// per-unit three ways: a **per-frame registry walk** (`0x616800`, a registered frame callback →
/// `0x615b10` over the movement manager's intrusive CMovement list → `0x616620` per node, whose own
/// active-mover GUID compare gates only the `0xee` heartbeat), the inbound move-message apply
/// (`0x618c30`), and `CGUnit_C::Initialize` for a unit that streams in already dead. A
/// spline-driven creature is spliced into that list by the `SMSG_MONSTER_MOVE` apply itself
/// (`0x6018f0 → 0x6187a0 → 0x619ca0`), and `0x60df70`'s not-the-mover arm sets SWIMMING
/// **synchronously** (`0x61a130 → 0x61a230 → 0x7c6e50`) — the GUID compare only chooses whether the
/// transition is additionally *reported* to the server. So deriving it here is faithful, and always
/// was. Evaluating **every streamed unit**, as this does, is a safe superset of the reference's
/// population: the walk covers a unit only while its CMovement is linked and `+0x40 & 0x8000000`
/// is clear.
///
/// **What was missing is the gate** (B311, decisions 1568 + 1572). `0x6030c0` reads the unit's
/// `UNIT_FIELD_FLAGS` on **both** legs of its decision ([`crate::player::may_swim`] —
/// PLAYER_CONTROLLED, PET_IN_COMBAT, USE_SWIM_ANIMATION): a set bit permits entry, and on the exit
/// leg a set bit *prevents* the stop, so a unit with none of them **walks the lakebed at any
/// depth** and is driven out of swim if anything else ever put the flag on it. That is what
/// vmangos means by *"Giant type creatures walk underwater"*: the Shore Strider off the Forgotten
/// Coast is a sea giant with no `CREATURE_STATIC_FLAG_CAN_SWIM`, so the reference wades it
/// chest-deep on its legs while benilla, gating on depth alone, slid it along on a swim gait.
///
/// Depth is measured on the creature's **own** collision height: marked once its feet sit deeper
/// than `0.75·h` ([`swim_enter_depth`]), unmarked once they rise a [`CREATURE_SWIM_EXIT_BAND`]
/// above it so the state can't flicker. The flat 2.0-yd stand-in it used before decision 0645 is
/// retired (0464's `collisionHeight` plumb was what it was waiting on); what stays open is the
/// *reference point* the real client measures a creature's depth from.
///
/// `0x6030c0`'s other guard — bail entirely while `MOVEFLAG_LEVITATING` (`0x400`) is set, so
/// neither leg runs — has no creature-side expression here, for two independent reasons: a
/// spline-walked creature carries no live move-flag word at all (`creature_anim::select::unify`'s
/// creature leg synthesises one), and vmangos never puts the bit on a creature anyway
/// (`Unit::SetLevitate` has **zero callers**; the only writer is the GM-fly path on a *player*).
/// The bail is real and ungated in the reference — its bit arrives through `0x618c30`'s wire
/// flag-merge mask, which has no active-mover compare — so this is a "nothing to read", not a
/// "doesn't apply". The local avatar's copy of the same guard is `update_swimming`'s first branch.
///
/// Consumers: the swim-gait leg of the animation selector (`creature_anim::select::unify`), the
/// swimming body pitch ([`sample_splines`]), and the [`ground_clamp_creatures`] exemption — which
/// is why the gate reaches further than the gait. A creature the server did NOT flag is now
/// re-grounded like any other walker, and that is right by the same authority: vmangos paths an
/// unflagged creature along the **bottom** of the water (`Object.cpp`'s *"Giant type creatures
/// walk underwater"* early-return, no upward randomisation), so its wire Z is the seabed and
/// clamping it to our terrain is the same answer, not a different one.
///
/// **Not** a consumer, deliberately: the enter-water splash (`sound::water`) reads `0.4·h` against
/// its own depth, exactly like the reference's `0x60314a` splash compare — which sits *outside*
/// the flag gate. A giant wading in still splashes.
#[derive(Component)]
pub(crate) struct CreatureSwimming;

/// The last ground clamp this unit took (decisions 1357 + 1384) — [`ground_clamp_creatures`]' seat
/// and its cast gate. Every field compares by bit: the gate's question is "are the ray's inputs
/// identical to the cast that produced this", never "close enough".
#[derive(Component, Clone, Copy)]
pub(crate) struct GroundClamped {
    /// Where the last cast stood.
    xz: [f32; 2],
    /// **The authoritative Y the answer was derived from** — the pose whoever owns this unit's
    /// position last wrote, never the clamp's own output. This is what makes the clamp a pure
    /// function of (server pose, colliders) instead of a ratchet that can only ever fall (1384).
    pub(crate) seat_y: f32,
    /// The Y standing after that cast (written or left). It differs from [`Self::seat_y`] by
    /// exactly the clamp's own correction, so an *external* write is detectable as "the Y here is
    /// not the one I left", which re-seats.
    y_written: f32,
    /// Only a HIT caches: a miss keeps casting, so a tile whose collider streams in late still
    /// catches its standing units.
    hit: bool,
    /// The collider-set stamp the answer was computed against — a cached answer outlives neither
    /// the unit's own pose nor the world it described.
    epoch: u64,
}

/// The reference's `0x6030c0` decision for one creature, as a pure function of everything it
/// reads — [`CreatureSwimming`]'s law in one place, so both asymmetries are assertable without a
/// world. Byte-VERIFIED whole (wow-re `collision/scratch/remote-swim-decision.md` §2/§3):
///
/// ```text
/// enter iff  flags ∧ depth >  0.75·h              (0x603106 test ah,0x41 + jne — a ZF test, STRICT)
/// stay  iff  flags ∧ depth >= 0.75·h − 1/36       (0x6031c5 test ah,5 + jnp — parity, INCLUSIVE at ==)
/// ```
///
/// **The flag term is on both legs**, which is the part decision 1568 shipped wrong and 1572
/// corrects: [`crate::player::may_swim`] is not an entry permit but the predicate *"may this unit
/// be locally SWIMMING at all"*, and its false value forces a stop **at any depth**
/// (`0x6031eb → 0x60dff0`), not merely a refusal to start. So a creature that loses its bit
/// mid-water leaves the gait immediately, exactly as the reference drives it out on the next tick.
///
/// The two depth boundaries are deliberately different comparisons, not a rounding accident — the
/// enter compare is strict and the stay compare is inclusive at equality, which is what makes the
/// 1/36-yd band a band rather than a knife edge.
fn creature_swim_state(marked: bool, depth: f32, boundary: f32, unit_flags: u32) -> bool {
    if !crate::player::may_swim(unit_flags) {
        return false; // both legs — a permit-less unit cannot start AND cannot stay
    }
    if marked {
        depth >= boundary - CREATURE_SWIM_EXIT_BAND
    } else {
        depth > boundary
    }
}

/// Maintain [`CreatureSwimming`] on `Unit` creatures from the water over their feet (module docs on
/// the INTERIM boundary). Runs chained before [`ground_clamp_creatures`] so a fresh mark exempts the
/// clamp the same frame (Bevy inserts the deferred-command sync point for the chain).
pub(in crate::net) fn mark_swimming_creatures(
    mut commands: Commands,
    units: Query<SwimMarkQuery, SwimMarkGate>,
    stamped: Query<Entity, With<SwimEvaluated>>,
    world: benilla_world::world_point::WorldPoint,
    epoch: Res<benilla_world::collision::ColliderEpoch>,
    mut last_epoch: Local<Option<u64>>,
) {
    // A world edge (a streamed tile/WMO attached or dropped colliders — and, with them, their
    // liquid) invalidates every standing answer: drop the stamps, the whole population
    // re-evaluates next frame ([`SwimEvaluated`]'s doc). Rare, so the steady state stays gated.
    let now = epoch.get();
    if last_epoch.replace(now) != Some(now) {
        for e in &stamped {
            commands.entity(e).remove::<SwimEvaluated>();
        }
    }
    for (e, net, t, collision, store, marked, evaluated) in &units {
        if net.kind != EntityKind::Unit {
            continue; // players carry the real flag on the wire; GameObjects don't swim
        }
        // The unit's OWN room decides whose liquid answers (0696). Before it, both sources
        // answered: Undercity's NPCs read Tirisfal's ADT water 95 yd over their heads and swam on
        // dry stone in rooms the player walked.
        //
        // A unit the room tracker hasn't reached yet cannot ENTER swim — an unsettled claim admits
        // both sources, the very false positive that fix removes, so a freshly streamed NPC
        // standing in an interior would flash the swim gait for the frame before its room lands.
        // It can still LEAVE: a stale mark must always be able to clear.
        let who = benilla_world::world_point::Subject::Unit(e);
        if !marked && !world.room_settled(who) {
            continue; // …and stays unstamped, so it is retried until the claim lands
        }
        if !evaluated {
            commands.entity(e).insert(SwimEvaluated);
        }
        let wow = bevy_to_wow(t.translation);
        let depth = world
            .water_surface_at(who, wow)
            .map_or(f32::MIN, |s| s - wow[2]);
        let boundary = swim_enter_depth(collision.copied().unwrap_or_default().0);
        // A unit whose descriptor block has not landed yet reads flags 0 and simply cannot enter
        // this frame; `Changed<ObjectStore>` brings it straight back when the block arrives.
        let flags = store.map_or(0, |s| s.0.unit_flags());
        let swimming = creature_swim_state(marked, depth, boundary, flags);
        if swimming != marked {
            if swimming {
                commands.entity(e).insert(CreatureSwimming);
            } else {
                commands.entity(e).remove::<CreatureSwimming>();
            }
        }
    }
}

/// B197's mechanism, in a world small enough to assert on: a unit created inside a building whose
/// floor collider has not attached yet, and what happens to it when the floor lands.
///
/// The real thing is a race between two async collider builds during a loading screen; here it is
/// two `spawn`s and a `bump`, which is the same three facts — the terrain is under the unit, the
/// floor is not there yet, and it arrives later. Both halves of decision 1384 are load-bearing in
/// the second assertion: the seat is what lets the probe reach back up to the floor at all, and the
/// epoch is what makes anything re-ask after the world changed under a unit standing still.
#[cfg(test)]
mod under_floor {
    use avian3d::prelude::*;
    use benilla_protocol::EntityKind;
    use bevy::ecs::system::RunSystemOnce;
    use bevy::prelude::*;

    use super::{ground_clamp_creatures, GroundClamped};
    use crate::net::NetEntity;
    use benilla_world::collision::ColliderEpoch;

    /// Auberdine's geometry, to the yard: terrain at 6.98, the building's floor 2.08 above it, and
    /// the server's Z for the NPCs inside 0.08 above *that* (the small float every wire Z carries).
    const TERRAIN_Y: f32 = 6.98;
    const FLOOR_Y: f32 = 9.06;
    const WIRE_Y: f32 = 9.14;

    /// A 10×10 up-wound quad at `y` — a floor the one-sided down-ray will stand on.
    fn floor_at(app: &mut App, y: f32) -> Entity {
        let verts = vec![
            Vec3::new(-5.0, y, -5.0),
            Vec3::new(5.0, y, -5.0),
            Vec3::new(5.0, y, 5.0),
            Vec3::new(-5.0, y, 5.0),
        ];
        app.world_mut()
            .spawn((
                RigidBody::Static,
                Collider::trimesh(verts, vec![[0u32, 2, 1], [0, 3, 2]]),
                Transform::default(),
            ))
            .id()
    }

    /// A world with the terrain in and the building's floor still building, plus one idle NPC
    /// standing at the Z the server sent for it.
    fn half_arrived_world() -> (App, Entity) {
        let mut app = App::new();
        // `WorldCollision` takes the mover's trace exclusions (the ghost/DOOR set, 1767); the
        // real one is initialised by the world plugins, which a headless harness does not run.
        app.init_resource::<benilla_world::collision::MoverTraceExclusions>();
        app.add_plugins((
            MinimalPlugins,
            bevy::transform::TransformPlugin,
            bevy::asset::AssetPlugin::default(),
            bevy::scene::ScenePlugin,
            PhysicsPlugins::new(bevy::app::PostUpdate),
        ));
        app.init_asset::<Mesh>().init_resource::<ColliderEpoch>();
        // `update()` never runs plugin `finish()`, where avian seats its diagnostics resources —
        // and the second `update()` below (the one that lands the late floor) does step physics.
        app.finish();
        app.cleanup();
        floor_at(&mut app, TERRAIN_Y);
        let npc = app
            .world_mut()
            .spawn((
                NetEntity {
                    kind: EntityKind::Unit,
                    display_id: None,
                    scale: 1.0,
                },
                Transform::from_xyz(0.0, WIRE_Y, 0.0),
            ))
            .id();
        app.update(); // seats Position/Rotation and the collider trees
        (app, npc)
    }

    fn clamp(app: &mut App) {
        app.world_mut()
            .run_system_once(ground_clamp_creatures)
            .expect("run the clamp");
    }

    fn y_of(app: &App, e: Entity) -> f32 {
        app.world().get::<Transform>(e).unwrap().translation.y
    }

    #[test]
    fn a_unit_sunk_by_a_late_floor_stands_back_up_when_it_lands() {
        let (mut app, npc) = half_arrived_world();

        // The window B197 lives in: the only surface under this NPC is the terrain the building
        // sits on, so that is what the clamp finds. This drop is CORRECT given what exists.
        clamp(&mut app);
        assert_eq!(
            y_of(&app, npc),
            TERRAIN_Y,
            "with only terrain built, the clamp grounds onto terrain"
        );
        assert_eq!(
            app.world().get::<GroundClamped>(npc).unwrap().seat_y,
            WIRE_Y,
            "the seat is the server's Z, not the answer the clamp just wrote"
        );

        // Nothing moves the NPC. The building's floor collider attaches, and stamps the world.
        floor_at(&mut app, FLOOR_Y);
        app.world_mut().resource_mut::<ColliderEpoch>().bump();
        app.update();

        // The fix: it comes back up on its own. Before 1384 it stayed on the terrain for the rest
        // of the session — the cached hit was never re-asked, and even re-asked it would have
        // measured from down there.
        clamp(&mut app);
        assert_eq!(
            y_of(&app, npc),
            FLOOR_Y,
            "the floor arrived under an NPC that never moved; it belongs on it"
        );
    }

    #[test]
    fn a_settled_unit_is_not_re_cast_while_the_world_holds_still() {
        // 1357's saving, still intact: the gate holds an answer whose three inputs are unchanged.
        let (mut app, npc) = half_arrived_world();
        clamp(&mut app);
        let before = *app.world().get::<GroundClamped>(npc).unwrap();
        clamp(&mut app);
        let after = *app.world().get::<GroundClamped>(npc).unwrap();
        assert_eq!(
            (before.seat_y, before.y_written, before.epoch),
            (after.seat_y, after.y_written, after.epoch),
            "a held unit's memo is untouched — nothing re-asked the ground"
        );
    }

    #[test]
    fn a_unit_with_no_ground_in_reach_sits_where_the_server_put_it() {
        // The miss branch: an airborne/unstreamed unit is left at its seat, never at whatever the
        // clamp last wrote. `GROUND_CLAMP_UP + GROUND_CLAMP_DOWN` below the seat is empty air here.
        let (mut app, npc) = half_arrived_world();
        app.world_mut()
            .get_mut::<Transform>(npc)
            .unwrap()
            .translation
            .y = TERRAIN_Y + 40.0;
        clamp(&mut app);
        assert_eq!(
            y_of(&app, npc),
            TERRAIN_Y + 40.0,
            "no surface in reach ⇒ the wire's Z stands"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A spline `frac` of the way through its ride: 10 s duration, started `frac·10 s` ago.
    fn spline_at(points: Vec<[f32; 3]>, grounded: bool, frac: f32) -> (Spline, Instant) {
        let spline = Spline {
            points,
            start: Instant::now() - Duration::from_secs_f32(10.0 * frac),
            duration: Duration::from_secs(10),
            id: 7,
            grounded,
            run_mode: true,
        };
        let now = Instant::now();
        (spline, now)
    }

    /// A ground path is a straight segment lerp: midway through the first leg of an L-shaped path
    /// sits exactly on the chord, facing along it.
    #[test]
    fn ground_path_samples_linearly() {
        let pts = vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [10.0, 10.0, 0.0]];
        let (s, now) = spline_at(pts, true, 0.25); // half of segment 0 (20 yd total)
        let (pos, facing, pitch) = s.sample(now);
        assert!(
            (pos[0] - 5.0).abs() < 0.05 && pos[1].abs() < 1e-3,
            "{pos:?}"
        );
        assert!(facing.unwrap().abs() < 1e-3);
        assert_eq!(pitch, 0.0);
    }

    /// A flying path still passes through every waypoint (the C-R curve interpolates, it doesn't
    /// approximate): at the chord-length boundary the sample is the corner point exactly.
    #[test]
    fn flying_path_passes_through_waypoints() {
        let pts = vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [10.0, 10.0, 0.0]];
        let (s, now) = spline_at(pts, false, 0.5); // exactly the corner (10 of 20 yd)
        let (pos, _, _) = s.sample(now);
        assert!(
            (pos[0] - 10.0).abs() < 0.05 && pos[1].abs() < 0.05,
            "corner waypoint expected, got {pos:?}"
        );
    }

    /// A flying path *curves*: mid-segment on the same L-path the C-R sample leaves the straight
    /// chord (the taxi look — decision 0484 I3), where the ground sampler would sit on it.
    #[test]
    fn flying_path_bends_off_the_chord() {
        let pts = vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [10.0, 10.0, 0.0]];
        let (s, now) = spline_at(pts, false, 0.25);
        let (pos, _, _) = s.sample(now);
        assert!(
            pos[1].abs() > 0.1,
            "expected a curved deviation off the y=0 chord, got {pos:?}"
        );
    }

    /// The flying attitude on a straight climb (decision 0516 — `0x7c5490`'s flying branch): the
    /// pitch is the TANGENT's own climb (45° here), and a straight path never banks — the
    /// look-ahead direction coincides with the tangent, so θ = 2·acos(1) = 0.
    #[test]
    fn a_straight_climb_pitches_by_the_tangent_and_never_banks() {
        let pts = vec![[0.0, 0.0, 0.0], [10.0, 0.0, 10.0]];
        let (s, now) = spline_at(pts, false, 0.3);
        let (pitch, bank) = s.flight_attitude(now);
        assert!(
            (pitch - std::f32::consts::FRAC_PI_4).abs() < 0.05,
            "45° climb tangent, got pitch {pitch}"
        );
        assert_eq!(bank, 0.0, "no bank on a straight path");
    }

    /// The bank's sign law (0516): the θ between the XY tangent and the 1 s look-ahead direction,
    /// ×2 — a LEFT turn (WoW CCW, `cross > 0`) leans left (+), the mirrored right turn leans
    /// right (−), and a gentle divergence stays inside the ±π/2 snap threshold.
    #[test]
    fn a_turn_banks_into_itself() {
        let left = vec![[0.0, 0.0, 0.0], [30.0, 0.0, 0.0], [30.0, 30.0, 0.0]];
        let (s, now) = spline_at(left, false, 0.42);
        let (_, bank) = s.flight_attitude(now);
        assert!(
            bank > 0.1 && bank < std::f32::consts::FRAC_PI_2,
            "left turn leans left, unsnapped — got {bank}"
        );
        let right = vec![[0.0, 0.0, 0.0], [30.0, 0.0, 0.0], [30.0, -30.0, 0.0]];
        let (s, now) = spline_at(right, false, 0.42);
        let (_, bank) = s.flight_attitude(now);
        assert!(
            bank < -0.1 && bank > -std::f32::consts::FRAC_PI_2,
            "right turn leans right, unsnapped — got {bank}"
        );
    }

    /// A create-block spline is joined **where the server already is**, not restarted from the top
    /// (decision 0708): a 20-yd path 12 s long, 9 s of it already ridden, samples three quarters
    /// along — 15 yd in. Restarting it (the naive read of the same packet) would sample at 0 and put
    /// the creature back at the start.
    #[test]
    fn a_create_spline_joins_the_walk_in_progress() {
        let s = create_spline(CreateSpline {
            path: vec![[0.0, 0.0, 0.0], [20.0, 0.0, 0.0]],
            id: 7,
            time_passed_ms: 9_000,
            duration_ms: 12_000,
            flying: false,
            cyclic: false,
            run_mode: true,
        })
        .expect("a live walk");
        let (pos, facing, _) = s.sample(Instant::now());
        assert!(
            (pos[0] - 15.0).abs() < 0.05 && pos[1].abs() < 1e-3,
            "expected three quarters along, got {pos:?}"
        );
        assert!(facing.unwrap().abs() < 1e-3, "facing down the path");
        assert!(s.grounded, "no Flying bit ⇒ terrain-clamped");
    }

    /// A ride the server has already finished — and a degenerate one — leave the unit at its create
    /// pose rather than replaying a walk that is over.
    #[test]
    fn a_finished_or_degenerate_create_spline_is_no_walk() {
        let spline = |time_passed_ms, duration_ms, path: Vec<[f32; 3]>| CreateSpline {
            path,
            id: 1,
            time_passed_ms,
            duration_ms,
            flying: false,
            cyclic: false,
            run_mode: true,
        };
        let straight = vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0]];
        assert!(create_spline(spline(5_000, 5_000, straight.clone())).is_none());
        assert!(create_spline(spline(0, 0, straight)).is_none());
        assert!(create_spline(spline(0, 5_000, vec![[1.0, 2.0, 3.0]])).is_none());
    }

    /// The SNAP past ±π/2 (0516 — `0x7c5573..`: a snap to ±π, not a soft clamp): approaching a
    /// hairpin, the look-ahead lands on the return leg, the divergence doubles past π/2, and the
    /// bank pins to a full ±π roll — the reference's momentary switchback barrel-roll.
    #[test]
    fn a_hairpin_snaps_the_bank_to_a_full_roll() {
        let pts = vec![[0.0, 0.0, 0.0], [80.0, 0.0, 0.0], [0.0, 4.0, 0.0]];
        let (s, now) = spline_at(pts, false, 0.47);
        let (_, bank) = s.flight_attitude(now);
        assert_eq!(
            bank.abs(),
            std::f32::consts::PI,
            "the antipodal guard writes the ±π constant, got {bank}"
        );
    }
}

/// **B311, in a world small enough to assert on** — [`creature_swim_state`] at the reported
/// giant's own numbers. The Shore Strider is a sea giant with no `CREATURE_STATIC_FLAG_CAN_SWIM`,
/// so no `UNIT_FLAG_USE_SWIM_ANIMATION` reaches the client: the reference wades it on its legs
/// however deep the water. The same depth with any one of the three gate bits set must still
/// swim — before this gate benilla swam both, which is the "gliding in deep water" the report
/// saw, and a fix that stopped every creature swimming would be the same bug wearing a hat.
#[cfg(test)]
mod swim_gate {
    use super::{creature_swim_state, CREATURE_SWIM_EXIT_BAND};
    use crate::player::swim_enter_depth;

    /// The Shore Strider's real numbers, from the shipped DBCs: display 4945 → CreatureModelData 35
    /// (`Creature\SeaGiant\SeaGiant.mdx`), `collisionHeight` 2.083, `CreatureDisplayInfo.scale`
    /// 1.75 over `modelScale` 1.0 — so `h = 2.083 × 1.75` and the boundary is `0.75·h`.
    const SEA_GIANT_H: f32 = 2.083 * 1.75;

    const NO_FLAGS: u32 = 0;
    const USE_SWIM_ANIMATION: u32 = 0x8000;
    const PLAYER_CONTROLLED: u32 = 0x8;
    const PET_IN_COMBAT: u32 = 0x800;

    #[test]
    fn a_sea_giant_walks_the_lakebed_however_deep_the_water() {
        let boundary = swim_enter_depth(SEA_GIANT_H);
        // Chest-deep — past the boundary that used to be the whole test — and then absurdly deep.
        for depth in [boundary + 0.01, boundary * 2.0, 100.0] {
            assert!(
                !creature_swim_state(false, depth, boundary, NO_FLAGS),
                "a unit with no swim flag must never enter swim (depth {depth})"
            );
        }
    }

    /// The control that must not change: the gate is a gate, not a switch-off. Any ONE of the three
    /// bits still admits the unit, and the depth law under it is untouched.
    #[test]
    fn a_flagged_unit_still_enters_on_the_same_depth_law() {
        let boundary = swim_enter_depth(SEA_GIANT_H);
        for flags in [
            USE_SWIM_ANIMATION,
            PLAYER_CONTROLLED,
            PET_IN_COMBAT,
            USE_SWIM_ANIMATION | PLAYER_CONTROLLED | PET_IN_COMBAT,
        ] {
            assert!(
                creature_swim_state(false, boundary + 0.01, boundary, flags),
                "flags {flags:#x} deep enough → swims"
            );
            assert!(
                !creature_swim_state(false, boundary, boundary, flags),
                "flags {flags:#x} exactly at the boundary → not yet (the compare is STRICT)"
            );
        }
    }

    /// Leaving keeps the 1/36-yd hysteresis, and its compare is **inclusive at equality** while
    /// enter's is strict — `0x6031c5 test ah,5` + `jnp` is a parity test on C0|C2, so `depth ==
    /// thr` does not stop, whereas `0x603106 test ah,0x41` + `jne` is a ZF test and `depth ==
    /// boundary` does not start. Deliberately asymmetric; that asymmetry is the band.
    #[test]
    fn leaving_keeps_its_hysteresis_and_is_inclusive_where_entering_is_strict() {
        let boundary = swim_enter_depth(SEA_GIANT_H);
        let flags = USE_SWIM_ANIMATION;
        assert!(
            creature_swim_state(true, boundary - CREATURE_SWIM_EXIT_BAND, boundary, flags),
            "exactly at the lower edge it holds (the stay compare is inclusive)"
        );
        assert!(
            !creature_swim_state(
                true,
                boundary - CREATURE_SWIM_EXIT_BAND - 1e-4,
                boundary,
                flags
            ),
            "below the band it leaves"
        );
        assert!(
            !creature_swim_state(true, f32::MIN, boundary, flags),
            "no liquid at all leaves"
        );
    }

    /// **The correction 1572 makes to 1568.** The three bits are one predicate — *may this unit be
    /// locally SWIMMING at all* — and `0x6030c0` tests it on the **exit** leg too, inverted: with
    /// all three clear the exit leg falls straight through to `StopSwim` (`0x6031eb → 0x60dff0`)
    /// however deep the water. A charm ending over deep water is the case: the creature loses
    /// PLAYER_CONTROLLED and must drop to the seabed on the next tick, not keep swimming until it
    /// finds a shallow.
    #[test]
    fn losing_the_flag_mid_water_stops_the_swim_at_any_depth() {
        let boundary = swim_enter_depth(SEA_GIANT_H);
        assert!(
            creature_swim_state(true, 100.0, boundary, PLAYER_CONTROLLED),
            "charmed and deep: swimming"
        );
        assert!(
            !creature_swim_state(true, 100.0, boundary, NO_FLAGS),
            "charm ends, still 100 yd down: the reference stops it anyway"
        );
    }

    /// A unit whose descriptor block has not landed reads flags 0 — it must not enter on that,
    /// and it must not be *stuck*: the system's `Changed<ObjectStore>` gate re-asks on the insert.
    #[test]
    fn an_unresolved_descriptor_cannot_enter_but_is_not_stuck() {
        let boundary = swim_enter_depth(SEA_GIANT_H);
        assert!(!creature_swim_state(false, 100.0, boundary, NO_FLAGS));
        assert!(creature_swim_state(
            false,
            100.0,
            boundary,
            USE_SWIM_ANIMATION
        ));
    }
}
