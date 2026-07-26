//! The server-authored path walk ([`Spline`], `SMSG_MONSTER_MOVE`) and the terrain re-ground that
//! goes with it ([`ground_clamp_creatures`]) — the creature half of [`super`]'s motion model
//! (decisions 0052/0059/0097).

use std::time::{Duration, Instant};

use avian3d::prelude::SpatialQuery;
use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use benilla_protocol::EntityKind;
use bevy::prelude::*;

use crate::collision::player_query_filter;
use crate::entities::CollisionHeight;
use crate::liquid::{water_surface_at, WaterChunkInfo};
use crate::player::swim_enter_depth;

use super::super::NetEntity;

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
}

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
    ///   tangent and the XY direction to the **1000 ms look-ahead** point (`0x3e8` @ `0x7c5623`;
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
    })
}

/// Interpolate every path-walking entity along its [`Spline`] into its [`Transform`] each frame — so
/// motion stays smooth between the sparse server `MSG_MOVE` packets. Writes translation + rotation
/// only (scale, set by the renderer when it attaches the model, is preserved).
pub(in crate::net) fn sample_splines(
    mut commands: Commands,
    mut q: Query<(Entity, &Spline, &mut Transform, Has<CreatureSwimming>)>,
) {
    let now = Instant::now();
    for (entity, spline, mut t, swimming) in &mut q {
        let (wow_pos, facing, pitch) = spline.sample(now);
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
        // Path finished: the final pose is written above (the sample clamps to the last point), so drop
        // the spline. It's what makes a `Spline` mean "actively moving" — otherwise a completed path
        // lingers until the next packet and a creature reads as walking forever after one move
        // (`creature_anim` keys Walk/Run on the spline's presence).
        if now.saturating_duration_since(spline.start) >= spline.duration {
            commands.entity(entity).remove::<Spline>();
        }
    }
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
/// **walking** colliders — the same set the player stands on ([`player_query_filter`]) — and set the
/// unit's Y to the hit.
///
/// Scope: **`Unit` creatures only** (a player owns its Z via the controller / `RemoteMotion`; a
/// GameObject sits at its authored Z — a lamp on a table, a chest on a ledge). A **flying** spline
/// keeps its own Z, and so does a **swimming** creature ([`CreatureSwimming`]): its wire Z *is* its
/// swim depth — vmangos paths a water creature in 3D through the volume with a plain non-FLYING
/// spline (verified `MoveSplineInit`/`WaypointMovementGenerator.cpp`: only `CanFly()` sets the flag),
/// so ground-clamping it dragged murlocs to the lakebed. A probe **miss** — the unit is genuinely
/// airborne (ground out of reach) or the tile's collider hasn't streamed in yet — leaves the current
/// Z untouched (no pop). Runs right after [`sample_splines`] so a walker's freshly-sampled XY+Z is
/// what we re-ground; an idle unit (no spline) is re-grounded in place each frame, which also catches
/// it once its terrain collider loads.
pub(in crate::net) fn ground_clamp_creatures(
    spatial: SpatialQuery,
    mut q: Query<(
        &NetEntity,
        Option<&Spline>,
        &mut Transform,
        Has<CreatureSwimming>,
    )>,
) {
    let filter = player_query_filter();
    for (net, spline, mut t, swimming) in &mut q {
        if net.kind != EntityKind::Unit {
            continue; // players + GameObjects own their Z
        }
        if spline.is_some_and(|s| !s.grounded) {
            continue; // a flying path is authoritative on Z
        }
        if swimming {
            continue; // in-liquid: the wire Z is the creature's swim depth
        }
        let origin = t.translation + Vec3::Y * GROUND_CLAMP_UP;
        let reach = GROUND_CLAMP_UP + GROUND_CLAMP_DOWN;
        if let Some(hit) = spatial.cast_ray(origin, Dir3::NEG_Y, reach, true, &filter) {
            t.translation.y = origin.y - hit.distance; // the down-ray's hit point Y = feet on the surface
        }
    }
}

/// What [`mark_swimming_creatures`] reads per unit: identity, kind, pose, its own collision height
/// (`None` only on a unit's first frame, before the stamp), and whether it is already marked.
type SwimMarkQuery = (
    Entity,
    &'static NetEntity,
    &'static Transform,
    Option<&'static CollisionHeight>,
    Has<CreatureSwimming>,
);

/// The exit band (yd) below the swim boundary where a marked swimmer stays marked — the player
/// boundary's **VERIFIED** 1/36-yd hysteresis (`0x7ff9d0`), an absolute distance independent of
/// collision height, so it is subtracted from the boundary rather than scaled with it.
const CREATURE_SWIM_EXIT_BAND: f32 = 1.0 / 36.0;

/// A **creature** currently past the swim boundary — the client-side derivation of the swim state
/// the wire never carries for creatures: vmangos sets `MOVEFLAG_SWIMMING` only from player packets,
/// and a water creature's create block + splines carry no swim marker at all (verified
/// `vmangos-src`, 2026-07-17). The real client derives every unit's swim mode locally per frame
/// (the `0x6030c0` boundary family — decision 0226 pinned it for the player). This derives it the
/// same way, on the creature's **own** collision height: marked once its feet sit deeper than
/// `0.75·h` ([`swim_enter_depth`]), unmarked once they rise a [`CREATURE_SWIM_EXIT_BAND`] above it
/// so the state can't flicker. The flat 2.0-yd stand-in it used before decision 0645 is retired
/// (0464's `collisionHeight` plumb was what it was waiting on); what stays open is the *reference
/// point* the real client measures a creature's depth from.
///
/// Consumers: the swim-gait leg of the animation selector (`creature_anim::select::unify`), the
/// [`ground_clamp_creatures`] exemption above, and the enter-water splash (`sound::water`).
#[derive(Component)]
pub(crate) struct CreatureSwimming;

/// Maintain [`CreatureSwimming`] on `Unit` creatures from the water over their feet (module docs on
/// the INTERIM boundary). Runs chained before [`ground_clamp_creatures`] so a fresh mark exempts the
/// clamp the same frame (Bevy inserts the deferred-command sync point for the chain).
pub(in crate::net) fn mark_swimming_creatures(
    mut commands: Commands,
    units: Query<SwimMarkQuery>,
    water: Query<&WaterChunkInfo>,
) {
    for (e, net, t, collision, marked) in &units {
        if net.kind != EntityKind::Unit {
            continue; // players carry the real flag on the wire; GameObjects don't swim
        }
        let wow = bevy_to_wow(t.translation);
        // `None`: no per-unit interior claim exists (CurrentWmoInterior is the local player's
        // down-ray), so both sources answer here — the pre-0634 behaviour, named in `liquid_at`.
        let depth = water_surface_at(water.iter(), wow, None).map_or(f32::MIN, |s| s - wow[2]);
        let boundary = swim_enter_depth(collision.copied().unwrap_or_default().0);
        let swimming = if marked {
            depth >= boundary - CREATURE_SWIM_EXIT_BAND
        } else {
            depth > boundary
        };
        if swimming != marked {
            if swimming {
                commands.entity(e).insert(CreatureSwimming);
            } else {
                commands.entity(e).remove::<CreatureSwimming>();
            }
        }
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
