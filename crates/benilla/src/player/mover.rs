//! The kinematic mover step — the walk/fall physics and the step-down snap, split out of the
//! `control` system ([`super`] keeps the input/camera/wire glue and the knob table this reads).
//! One call per frame: [`step`].
//!
//! Thin kinematic controller (decision 0009) over avian's `MoveAndSlide` — kept simple and robust
//! on the triangulated heightmap:
//!   - probe down to classify the ground (walkable iff its normal is within ~50° of up);
//!   - "grounded" = on walkable ground AND not rising, so a jump cleanly leaves the ground (and
//!     isn't re-grounded the next frame — the bug that ate most jumps). While airborne the probe
//!     tightens to [`LAND_PROBE`], so the arc ends where the slide actually contacts the floor
//!     rather than snapping the last fraction of a yard (decision 0190);
//!   - grounded → move horizontally only, with NO gravity fed into the slide (gravity-slide was
//!     the downhill creep on micro-sloped terrain), then snap onto the surface to follow it;
//!   - a walkable slope never slows or deflects the walk: the real client's walk is
//!     two-dimensional (speed·dt of *horizontal* distance), so an opposing walkable plane rides
//!     instead of clipping ([`walkable_ride_velocity`]) — full 2D speed on every ≤50° surface;
//!   - a steep face in the way runs the atomic step-up ([`step_up`]): rise–advance–settle onto
//!     a walkable floor, committed whole within the frame or not at all (decision 0209);
//!   - a steep face never *lifts* the mover: when the slide's clip would convert a push into
//!     upward motion, the face clips as a vertical wall instead ([`steep_wall_plane`]) — you
//!     rub along trunks and steep banks, never up them;
//!   - airborne → gravity carries the arc, with a one-shot nudge to steer a standstill jump;
//!   - a fall whose descent stalls (a capsule wedged between steep faces — the
//!     tree-pinch funnel) *lands there*: standing, walking control live, instead of hanging in
//!     the falling pose forever with mid-air control locked (decisions 0211/0212).

use avian3d::character_controller::move_and_slide::MoveHitData;
use avian3d::math::Dir;
use avian3d::prelude::*;
use bevy::prelude::*;

use crate::collision::player_query_filter;

use super::{
    move_trace, Player, AIR_NUDGE_SPEED, CAPSULE_HEIGHT, GRAVITY, GROUND_COS, GROUND_PROBE,
    JUMP_SPEED, LAND_PROBE, SETTLE_REACH, SKIN_WIDTH, STEP_SLOPE_RATIO, STEP_SNAP_SLACK,
    STEP_UP_HEIGHT, TERMINAL_VELOCITY, WEDGE_MIN_FALL, WEDGE_STALL_RATIO, WEDGE_STILL_FRAMES,
};

/// What the step decided — read by the move-flags / wire logic that follows it in `control`.
pub(super) struct Outcome {
    /// Settling (post-teleport world stream-in): frozen in place, gravity off.
    pub held: bool,
    /// On walkable ground and not rising this frame.
    pub grounded: bool,
    /// A jump took off this frame.
    pub jumped: bool,
    /// The standstill-jump air nudge fired (re-seeds the frozen airborne direction flags).
    pub air_nudged: bool,
    /// The collider entity of the walkable floor supporting us — the end-of-frame snap probe's
    /// hit when it ran, else the classify probe's. `None` airborne, held, or wedged (a wedge
    /// rests *between* steep faces, standing on nothing walkable). The transport attach keys
    /// off this: support on a boat's collider enters its platform frame (decision 0438 phase 2).
    pub ground: Option<Entity>,
}

/// Advance the player mover one frame: settle gate, ground classify, the slide, and the
/// step-down snap. Writes `player.pos`/`vel_y`/`horiz_vel`/`settling`.
#[allow(clippy::too_many_arguments)]
pub(super) fn step(
    player: &mut Player,
    time: &Time,
    ms: &MoveAndSlide<'_, '_>,
    capsule: &Collider,
    moving: bool,
    dir: Vec3,
    speed: f32,
    want_jump: bool,
) -> Outcome {
    let dt = time.delta_secs();
    let input_horiz = if moving {
        dir.normalize() * speed
    } else {
        Vec3::ZERO
    };
    let half_h = Vec3::Y * (CAPSULE_HEIGHT * 0.5);
    let mut center = player.pos + half_h;
    // Player body collides with terrain/doodads/GameObjects + the WMO *walking* faces (not the
    // camera-only ones); the camera sweep uses its own filter (see `crate::collision`).
    let filter = player_query_filter();
    let cast = |from: Vec3, disp: Vec3| {
        ms.cast_move(capsule, from, Quat::IDENTITY, disp, SKIN_WIDTH, &filter)
    };
    let probe_down = |c: Vec3, dist: f32| cast(c, Vec3::NEG_Y * dist);

    // While airborne, "on the ground" means where the slide actually contacts the floor
    // ([`LAND_PROBE`], ~skin scale). The wider walking probe would end the arc up to 0.2 yd
    // early and close the gap with a same-frame snap — the visible pop at every silent landing
    // (decision 0190); the fall's own collision already stops the capsule exactly at contact.
    let ground_reach = if player.airborne_since.is_some() {
        LAND_PROBE
    } else {
        GROUND_PROBE
    };
    let classify = probe_down(center, ground_reach);
    let on_walkable = classify.as_ref().is_some_and(|h| h.normal1.y >= GROUND_COS);
    // Who we stand on (frame start); the end-of-frame snap probe below refreshes it post-move.
    let mut ground_entity = if on_walkable {
        classify.map(|h| h.entity)
    } else {
        None
    };
    // Settle gate (post-teleport/summon/login): the streamed world — terrain *and* WMO building
    // floors + their colliders — arrives over several frames, so the ground under the snap isn't
    // there yet. End settling once a probe finds the ground under our feet (`SETTLE_REACH`, kept
    // small so it's the *close* floor we stand on, not distant terrain seen through an unloaded
    // building) or once we time out. Until then `held` keeps gravity OFF and freezes us in place,
    // so we don't fall through the not-yet-loaded city/building (the loading screen stays up too).
    if player.settling
        && (probe_down(center, SETTLE_REACH).is_some_and(|h| h.normal1.y >= GROUND_COS)
            || time.elapsed_secs() >= player.settle_deadline)
    {
        player.settling = false;
    }
    let held = player.settling;
    let on_floor = !held && on_walkable && player.vel_y <= 0.0;

    // The wedged rest (decision 0211) stands until real ground takes over or the support
    // vanishes — we walked off the funnel wall into open air, which resumes a normal fresh fall.
    if player.wedged && (on_floor || held || probe_down(center, LAND_PROBE).is_none()) {
        player.wedged = false;
    }
    let grounded = on_floor || player.wedged;

    let mut jumped = false;
    if held {
        // Frozen at the snap position with no velocity until the ground loads under us.
        player.vel_y = 0.0;
        player.horiz_vel = Vec3::ZERO;
    } else if grounded {
        player.vel_y = 0.0;
        if want_jump {
            player.vel_y = JUMP_SPEED;
            player.wedged = false;
            jumped = true;
        }
    } else {
        player.vel_y = (player.vel_y - GRAVITY * dt).max(-TERMINAL_VELOCITY);
    }
    let mut air_nudged = false;
    if grounded {
        player.horiz_vel = input_horiz;
    } else if !held && moving && player.horiz_vel.length_squared() < 0.01 {
        // Air control: one nudge to steer a jump that took off from a standstill (a moving jump
        // keeps its momentum locked, since horiz_vel is already non-zero). The pressed direction
        // *really* moves us, so it re-seeds the frozen airborne direction flags.
        player.horiz_vel = dir.normalize_or_zero() * AIR_NUDGE_SPEED;
        air_nudged = true;
    }

    let pre_move = center;
    // The grounded walk is the SHARED resolve ([`grounded_step`]) — step-up, slide, election
    // snap — the same code every remote mover's dead-reckon runs. Held and airborne/jumping
    // frames keep their own slide here: no step-up, no snap, and gravity in the velocity.
    let (mut climb, mut snap_probe) = (None, None);
    if !held && grounded && !jumped {
        let g = grounded_step(ms, capsule, &filter, center, player.horiz_vel, time.delta());
        center = g.center;
        climb = g.climb;
        snap_probe = g.snap;
        if let Some(e) = g.ground {
            ground_entity = Some(e);
        }
    } else {
        // Held: zero velocity (no move) — `held` already zeroed both terms, but say it outright.
        // Jumping/airborne: gravity carries the arc.
        let velocity = if held {
            Vec3::ZERO
        } else {
            player.horiz_vel + Vec3::Y * player.vel_y
        };
        // The airborne slide is the OTHER shared resolve ([`airborne_step`]) — the same code a
        // remote mover's arc runs, so a jump meets our walls whoever is jumping (decision 0627).
        center = airborne_step(ms, capsule, &filter, center, velocity, time.delta());
    }
    // Wedge-rest detection (decisions 0211/0212): airborne, already falling fast, yet the
    // descent achieved is a sliver of what gravity intended — [`WEDGE_STILL_FRAMES`] in a row
    // is a capsule held between steep faces (a ball in a V-groove; the trunk-base funnel's
    // walls lean, n.y ≈ +0.2, so there is no downward exit). Land it. Free fall achieves ~100%
    // of its intent and a steep-slope slide ≥75%, and a jump apex is slower than
    // [`WEDGE_MIN_FALL`], so neither can trip this; measuring against the intent (which keeps
    // growing) catches the funnel's pinch-in as it happens — 0211's absolute-stillness test
    // waited out the decelerating millimeter creep, a visible hang in the falling pose.
    if !held
        && !grounded
        && !jumped
        && player.vel_y < -WEDGE_MIN_FALL
        && (pre_move.y - center.y) < -player.vel_y * dt * WEDGE_STALL_RATIO
    {
        player.wedge_still += 1;
        if player.wedge_still >= WEDGE_STILL_FRAMES {
            player.wedged = true;
            player.wedge_still = 0;
            player.vel_y = 0.0;
            let feet = center - half_h;
            crate::dbg_trace::line(
                "move",
                &format!(
                    "wedge rest at ({:8.2},{:7.2},{:8.2}) -> landed standing",
                    feet.x, feet.y, feet.z
                ),
            );
        }
    } else {
        player.wedge_still = 0;
    }
    // The frame that detects the wedge reports grounded immediately, so the falling pose ends
    // and the wire sees a normal landing (`MSG_MOVE_FALL_LAND`) this frame, not next.
    let grounded = grounded || player.wedged;

    player.pos = center - half_h;
    move_trace::frame(move_trace::Frame {
        y_in: pre_move.y - half_h.y,
        y_out: player.pos.y,
        grounded,
        on_walkable,
        vel_y: player.vel_y,
        snap: snap_probe,
        climb,
    });

    Outcome {
        held,
        grounded,
        jumped,
        air_nudged,
        ground: if grounded && !held {
            ground_entity
        } else {
            None
        },
    }
}

/// What one grounded walk step resolved against the world came out as ([`grounded_step`]).
pub(crate) struct GroundedStep {
    /// The resolved capsule centre.
    pub(crate) center: Vec3,
    /// The collider of the walkable floor the election snap settled onto, when it ran and hit one.
    /// `None` means "keep whatever the caller already believed" — a step-up commit and a missed
    /// snap both leave the support unchanged.
    pub(crate) ground: Option<Entity>,
    /// The atomic step-up's committed height gain (yd), when the maneuver ran.
    pub(crate) climb: Option<f32>,
    /// The election snap's `(probe reach, what it found)` — trace fodder, `None` when the step-up
    /// took the frame instead. The inner pair is `(hit distance, hit normal.y)`.
    pub(crate) snap: Option<(f32, Option<(f32, f32)>)>,
}

/// **One grounded walk step, resolved against the world** — step-up → slide → election snap, from
/// a capsule centre and this frame's horizontal velocity. The single place a walking body meets the
/// terrain, and deliberately so: the reference drives **every** mover through one controller (0059's
/// byte trail — `0x616620` integrates any mover; the local-player GUID compare at `0x6166a9` gates
/// only a timing budget; the grounded fork zeroes the vertical and commits through the swept world
/// query `0x633840` + the WALK resolver `0x6367b0`, which reads Z off the surface). So does benilla:
/// the local controller ([`step`]) calls this for its grounded frames, and every **remote** mover's
/// dead-reckon calls it for its extrapolated step ([`crate::net::motion::remote`]).
///
/// Why a remote needs it at all: dead-reckoning between packets is *our invention*, and an invention
/// that ignores the world walks a watched player into a hillside (they sink in, then the next packet
/// pops them out) and leaves their height wherever the last packet put it while the ground under them
/// rises or falls (they sink or float, and the height arrives as a 2 Hz snap). Both are one defect —
/// the extrapolator never touched the world — and both are gone the moment the step is resolved
/// through here.
///
/// Airborne and swimming frames are **not** this function's: a jump is a ballistic arc and a
/// swimmer's Z is its depth, exactly as the reference's grounded fork excludes both.
pub(crate) fn grounded_step(
    ms: &MoveAndSlide<'_, '_>,
    capsule: &Collider,
    filter: &SpatialQueryFilter,
    center: Vec3,
    horiz_vel: Vec3,
    dt: std::time::Duration,
) -> GroundedStep {
    let cast = |from: Vec3, disp: Vec3| {
        ms.cast_move(capsule, from, Quat::IDENTITY, disp, SKIN_WIDTH, filter)
    };
    let speed = horiz_vel.length();
    // The step-up (decision 0209): ATOMIC — a steep face in the way triggers rise →
    // advance-this-frame's-travel-at-the-raised-height → settle onto a walkable floor, all
    // committed inside this one frame, or nothing happens and the plain slide runs. There is
    // no in-between state to be seen wedged or bouncing in (the 0191 ride dwelled mid-face;
    // every stuck/bounce report of the step-up era was that dwelling). Grazing a face nets
    // back onto the same floor (reads as sliding); a square push onto a low step lands on its
    // top; anything taller than [`STEP_UP_HEIGHT`] never commits.
    let stepped = if speed > 1.0e-6 {
        step_up(&cast, center, horiz_vel / speed, speed * dt.as_secs_f32())
    } else {
        None
    };
    if let Some(landed) = stepped {
        // The committed maneuver IS this frame's motion — already settled on a walkable floor,
        // so the slide and the snap below are skipped.
        return GroundedStep {
            center: landed,
            ground: None,
            climb: Some(landed.y - center.y),
            snap: None,
        };
    }
    let out = ms.move_and_slide(
        capsule,
        center,
        Quat::IDENTITY,
        horiz_vel,
        dt,
        &MoveAndSlideConfig::default(),
        filter,
        |hit| {
            if let Some(ride) = walkable_ride_velocity(**hit.normal, *hit.velocity) {
                *hit.velocity = ride;
                return MoveAndSlideHitResponse::Accept;
            }
            if let Some(wall) = steep_wall_plane(**hit.normal, *hit.velocity) {
                if let Ok(wall) = Dir::new(wall) {
                    *hit.normal = wall;
                }
            }
            MoveAndSlideHitResponse::Accept
        },
    );
    let mut slid = out.position;
    // Snap onto the surface so we follow downhill slopes + steps down — the client's step-vs-fall
    // election (`0x6367b0`, wow-re `step-vs-fall-election.md`): the probe reaches
    // [`STEP_SLOPE_RATIO`]·travel + [`STEP_SNAP_SLACK`] + the unit's collision height (`0x617430` =
    // `[unit+0xb8]`, our [`CAPSULE_HEIGHT`]; the election's `0x4000000`-gated extension — decision
    // 0182) and snaps only onto a *walkable* floor (≤50°, the election's own `cos50°` =
    // [`GROUND_COS`]). A deeper or steeper floor is NOT absorbed: no snap, the next frame's ground
    // probe misses, and the gap becomes a fall (the client's `StartFalling(0)` election) — a short
    // ledge drop reads as a quick, continuous, steep descent, which is what the director's eye
    // confirmed against the reference (decision 0190; 0189's instant absorbed step read as a
    // teleport and was reverted).
    //
    // Standing still the reach is slack + the collision height, which is what re-grounds an
    // *idle* body every frame: the small float a raw wire Z leaves a watched player standing on
    // our terrain is taken out here, the same way [`crate::net::motion::ground_clamp_creatures`]
    // takes it out of an idle NPC.
    let d = slid - center;
    let reach = d.x.hypot(d.z) * STEP_SLOPE_RATIO + STEP_SNAP_SLACK + CAPSULE_HEIGHT;
    let hit = cast(slid, Vec3::NEG_Y * reach);
    let snap = Some((reach, hit.as_ref().map(|h| (h.distance, h.normal1.y))));
    let mut ground = None;
    if let Some(h) = hit.filter(|h| h.normal1.y >= GROUND_COS) {
        slid.y -= h.distance;
        ground = Some(h.entity);
    }
    GroundedStep {
        center: slid,
        ground,
        climb: None,
        snap,
    }
}

/// **One airborne step, resolved against the world** — the arc's slide and nothing else. No
/// step-up and no election snap: the arc owns its own height (gravity carries it; the landing is
/// next frame's ground probe to call), so the only thing the world may do here is *stop* it. Steep
/// faces get [`steep_wall_plane`]'s treatment, exactly as they do on the ground.
///
/// The airborne twin of [`grounded_step`], and shared for the same reason (0059's one controller,
/// every mover): the local controller ([`step`]) calls it for its held/airborne frames, and a
/// **remote** mover's ballistic dead-reckon calls it for the arc it invents between packets
/// ([`crate::net::motion::remote`]). Without that, a watched player who jumps into a building is
/// drawn *inside* it for the length of the jump and pops back out on the landing packet — the
/// airborne half of the very defect 0626 fixed on the ground (decision 0627).
pub(crate) fn airborne_step(
    ms: &MoveAndSlide<'_, '_>,
    capsule: &Collider,
    filter: &SpatialQueryFilter,
    center: Vec3,
    velocity: Vec3,
    dt: std::time::Duration,
) -> Vec3 {
    ms.move_and_slide(
        capsule,
        center,
        Quat::IDENTITY,
        velocity,
        dt,
        &MoveAndSlideConfig::default(),
        filter,
        |hit| {
            if let Some(wall) = steep_wall_plane(**hit.normal, *hit.velocity) {
                if let Ok(wall) = Dir::new(wall) {
                    *hit.normal = wall;
                }
            }
            MoveAndSlideHitResponse::Accept
        },
    )
    .position
}

/// The even-speed ramp ride: a walkable slope never slows or deflects the grounded walk. The
/// real client's walk step is two-dimensional — the resolver takes speed·dt as a *horizontal*
/// distance and a normalized 2D direction, and Z follows purely through the snap/step machinery
/// (`0x6367b0`'s own signature, wow-re `step-vs-fall-election.md`) — so on every walkable
/// (< 50°) surface the horizontal speed is exactly the run speed. Collide-and-slide's
/// true-plane clip breaks that invariant: `v' = v − (v·n)n` shortens the horizontal part to
/// `h·cos²θ` (half speed at 45°) and bends a diagonal approach off the input line. When the
/// grounded slide meets an opposing *walkable* plane (`n.y ≥ GROUND_COS`), replace the clip
/// with the vertical-lift projection: keep the horizontal velocity exactly, set the vertical so
/// the motion rides along the plane (`v'·n = 0` — the plane's own clip then passes it
/// untouched). Unreal's `bMaintainHorizontalGroundVelocity` is the same standard treatment.
/// Steep faces stay with [`steep_wall_plane`], airborne contacts keep the true clip (a landing
/// still slides naturally), and any height the ride manufactures is bounded by the end-of-frame
/// snap, which only ever settles onto a walkable floor.
fn walkable_ride_velocity(n: Vec3, v: Vec3) -> Option<Vec3> {
    if n.y < GROUND_COS || v.dot(n) >= 0.0 {
        return None;
    }
    // Walkability bounds n.y ≥ cos50° > 0; an opposing contact makes the recomputed vertical
    // strictly positive and ≤ h·tan50°. A prior facet's ride vertical is discarded, not stacked:
    // the grounded mover owns no vertical of its own.
    Some(Vec3::new(v.x, -(v.x * n.x + v.z * n.z) / n.y, v.z))
}

/// The steep-face wall rule: a steep (non-walkable, non-overhanging) face must never *lift*
/// the mover. Collide-and-slide clips velocity onto each contact plane, and on a tilted plane
/// that clip manufactures upward motion out of a horizontal push (`v'.y − v.y = −(v·n)·n.y`,
/// positive for every opposing contact) — which walked the capsule straight up 50–80° trunks
/// and hillsides, and, while falling with locked forward momentum, cancelled enough of the
/// descent to trip the wedge rest (decisions 0211/0212 modeled a vertical-only fall) into
/// landing mid-face: together, a climbing ratchet. When the true-plane clip would leave the
/// mover moving *upward* (`v'.y > 0`), return the face's vertical-wall flatten to clip against
/// instead: the push slides along the wall line and only the mover's own vertical motion
/// survives. A descending clip (`v'.y ≤ 0`) keeps the true plane — that IS the natural slide
/// down a steep surface; flattening those stalls real falls against the face (the hover the
/// module note warned about). Walkable floors and overhangs (`n.y < 0`) always keep their
/// plane. This is the standard controller treatment (Unreal `HandleSlopeBoosting`, Godot
/// `floor_block_on_wall`); penetration safety is untouched — the slide's sweeps still stop at
/// the real surface, the plane only shapes the deflection.
fn steep_wall_plane(n: Vec3, v: Vec3) -> Option<Vec3> {
    if !(0.0..GROUND_COS).contains(&n.y) {
        return None;
    }
    let vn = v.dot(n);
    if vn >= 0.0 || v.y - vn * n.y <= 0.0 {
        return None;
    }
    // Steepness bounds the horizontal part below by sin 50°, so the normalize is safe.
    Some(Vec3::new(n.x, 0.0, n.z).normalize())
}

/// The atomic step-up (decision 0209) — the standard kinematic-controller maneuver, *not* the
/// reference resolver's (that direction is closed, 0207): a steep opposing face within this
/// frame's travel triggers **rise → advance → settle**, committed whole inside this one frame,
/// or nothing.
///
/// - **Rise** by the free headroom, at most [`STEP_UP_HEIGHT`] — the deliberately low ceiling
///   that scopes this to stairs/doorsteps/low rocks and keeps fences and walls slide-only.
/// - **Advance** this frame's own travel along the *input* direction at the raised height —
///   never a probe-length lunge.
/// - **Settle** back down by the walk election's own reach; commit **only onto a walkable
///   floor that is actually higher**.
///
/// Case by case: a square push at a low step lands ON its top this frame; a grazing rub
/// settles back onto the same floor — net zero, reads as *sliding along*; a face taller than
/// the ceiling leaves no forward clearance at the raised height ⇒ the settle lands back on
/// the origin floor ⇒ slide; a pinch between two tree trunks offers only steep landings ⇒
/// **no commit, ever** — the wedge/bounce class of 0191–0195 is impossible by construction,
/// because there is no intermediate mid-climb state to be caught in.
fn step_up(
    cast: &impl Fn(Vec3, Vec3) -> Option<MoveHitData>,
    center: Vec3,
    dir_h: Vec3,
    travel: f32,
) -> Option<Vec3> {
    // A steep, non-overhanging face opposing the motion, within this frame's travel (+skin).
    // No incidence gate — the verified ref has none; grazing nets zero through the settle.
    let ahead = cast(center, dir_h * travel)?;
    let n = ahead.normal1;
    if n.y >= GROUND_COS || n.y < 0.0 || n.dot(dir_h) >= 0.0 {
        return None;
    }
    // The certify trace (`WOW_MOVE_TRACE`): one `step` line per attempt with every probe number
    // and the world-space contact, so a feel report pins to the exact placement and probe —
    // the instrument that broke the fence/tree cases, which reasoning alone could not.
    let feet_y = center.y - CAPSULE_HEIGHT * 0.5;
    let hit = ahead.point1;
    let log = |verdict: &str| {
        if crate::dbg_trace::enabled() {
            crate::dbg_trace::line(
                "step",
                &format!(
                    "hit ({:8.2},{:7.2},{:8.2}) h={:+.2} n=({:+.2},{:+.2},{:+.2}) {}",
                    hit.x,
                    hit.y,
                    hit.z,
                    hit.y - feet_y,
                    n.x,
                    n.y,
                    n.z,
                    verdict
                ),
            );
        }
    };

    // Rise: the free headroom, at most H.
    let up_t = cast(center, Vec3::Y * STEP_UP_HEIGHT).map_or(STEP_UP_HEIGHT, |h| h.distance);
    if up_t < 1e-3 {
        log("up=0.00 NO-HEADROOM -> slide");
        return None;
    }
    // Advance: this frame's travel along the input dir, swept at the raised height.
    let raised = center + Vec3::Y * up_t;
    let fwd_t = cast(raised, dir_h * travel).map_or(travel, |h| h.distance);
    let over = raised + dir_h * fwd_t;
    // Settle: the walk election's reach below the advanced point — the rise undone, plus the
    // travel-scaled step-down allowance (decisions 0182/0190) — onto a WALKABLE floor only.
    let reach = up_t + travel * STEP_SLOPE_RATIO + STEP_SNAP_SLACK;
    let Some(down) = cast(over, Vec3::NEG_Y * reach) else {
        log(&format!(
            "up={up_t:.2} fwd={fwd_t:.2} down=miss NO-FLOOR -> slide"
        ));
        return None;
    };
    if down.normal1.y < GROUND_COS {
        log(&format!(
            "up={up_t:.2} fwd={fwd_t:.2} down=(d={:.2} ny={:+.2}) STEEP-FLOOR -> slide",
            down.distance, down.normal1.y
        ));
        return None;
    }
    let landed = over + Vec3::NEG_Y * down.distance;
    let dy = landed.y - center.y;
    // Commit only a landing that actually gained a floor. A net-zero maneuver (grazing a face,
    // pushing a too-tall wall, the tree pinch's gap grass) belongs to the plain slide — its
    // deflection is what "sliding along the fence" is; committing here would dead-stop it.
    if dy <= 0.05 {
        log(&format!(
            "up={up_t:.2} fwd={fwd_t:.2} down=(d={:.2} ny={:+.2}) dy={dy:+.3} NET-ZERO -> slide",
            down.distance, down.normal1.y
        ));
        return None;
    }
    log(&format!(
        "up={up_t:.2} fwd={fwd_t:.2} down=(d={:.2} ny={:+.2}) dy={dy:+.3} -> COMMIT",
        down.distance, down.normal1.y
    ));
    Some(landed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Outward normal of a face rising toward +x, tilted `deg` from horizontal.
    fn face(deg: f32) -> Vec3 {
        let r = deg.to_radians();
        Vec3::new(-r.sin(), r.cos(), 0.0)
    }

    #[test]
    fn a_walkable_ramp_rides_at_full_horizontal_speed() {
        // 45° uphill at run speed: the ride keeps the 2D velocity exactly (the true-plane clip
        // would halve it to h·cos²45° = 3.5) and lies in the plane, so the clip passes it.
        let n = face(45.0);
        let v = Vec3::new(7.0, 0.0, 0.0);
        let ride = walkable_ride_velocity(n, v).expect("must ride");
        assert_eq!((ride.x, ride.z), (7.0, 0.0));
        assert!(ride.y > 0.0);
        assert!(ride.dot(n).abs() < 1e-6);
    }

    #[test]
    fn a_diagonal_approach_is_not_deflected() {
        // Walking diagonally up a face rising toward +x: the true-plane clip bends the path
        // toward across-slope; the ride keeps both horizontal components untouched.
        let v = Vec3::new(5.0, 0.0, 5.0);
        let ride = walkable_ride_velocity(face(40.0), v).expect("must ride");
        assert_eq!((ride.x, ride.z), (5.0, 5.0));
    }

    #[test]
    fn a_prior_facet_ride_is_recomputed_not_stacked() {
        // Crossing a facet boundary mid-slide: the incoming vertical (facet A's ride) is
        // discarded and rebuilt for facet B — the grounded mover owns no vertical of its own.
        let n = face(45.0);
        let ride = walkable_ride_velocity(n, Vec3::new(7.0, 3.0, 0.0)).expect("must ride");
        assert_eq!((ride.x, ride.z), (7.0, 0.0));
        assert!(ride.dot(n).abs() < 1e-6);
    }

    #[test]
    fn steep_flat_and_receding_planes_never_ride() {
        let push = Vec3::new(7.0, 0.0, 0.0);
        // Steep (>50°) is the wall rule's, not the ride's.
        assert!(walkable_ride_velocity(face(60.0), push).is_none());
        // Flat floor underfoot: no opposition, nothing to rewrite.
        assert!(walkable_ride_velocity(Vec3::Y, push).is_none());
        // A receding walkable plane (walking downhill away from it) keeps the plain move + snap.
        assert!(walkable_ride_velocity(face(40.0), -push).is_none());
    }

    #[test]
    fn the_ride_covers_the_walkable_range_up_to_the_gate() {
        // Just inside the gate (49.9°) still rides at full speed; just outside (50.1°) does not
        // ride — it falls to the steep-wall rule instead.
        let v = Vec3::new(7.0, 0.0, 0.0);
        let ride = walkable_ride_velocity(face(49.9), v).expect("must ride");
        assert_eq!((ride.x, ride.z), (7.0, 0.0));
        assert!(ride.y <= 7.0 * 50.0_f32.to_radians().tan() + 1e-3);
        assert!(walkable_ride_velocity(face(50.1), v).is_none());
        assert!(steep_wall_plane(face(50.1), v).is_some());
    }

    #[test]
    fn walking_into_a_steep_face_clips_as_a_wall() {
        let wall = steep_wall_plane(face(60.0), Vec3::new(7.0, 0.0, 0.0)).expect("must flatten");
        assert_eq!(wall.y, 0.0);
        assert!(wall.x < 0.0 && wall.is_normalized());
    }

    #[test]
    fn the_wedge_misfire_window_flattens() {
        // Falling slowly with locked forward momentum: the true-plane clip would end
        // RISING (v'.y = +2.06) — the descent-cancel that tripped the wedge rest.
        assert!(steep_wall_plane(face(60.0), Vec3::new(7.0, -1.3, 0.0)).is_some());
    }

    #[test]
    fn a_real_fall_keeps_the_true_plane() {
        // The natural slide down a steep surface must survive: descent-dominated clips
        // stay on the true plane (flattening them hovers the fall mid-face).
        assert!(steep_wall_plane(face(60.0), Vec3::new(0.0, -10.0, 0.0)).is_none());
        assert!(steep_wall_plane(face(60.0), Vec3::new(7.0, -20.0, 0.0)).is_none());
    }

    #[test]
    fn rising_contacts_flatten_but_a_wall_keeps_own_lift() {
        // A jump rising along the face: the flatten removes the face's manufactured
        // boost; the mover's own +vy passes through the vertical wall untouched.
        let v = Vec3::new(7.0, 8.0, 0.0);
        let wall = steep_wall_plane(face(60.0), v).expect("boost must flatten");
        let clipped = v - v.dot(wall) * wall;
        assert!((clipped.y - v.y).abs() < 1e-6);
    }

    #[test]
    fn walkable_overhanging_and_vertical_faces_are_untouched() {
        let push = Vec3::new(7.0, 0.0, 0.0);
        // Walkable floor: the slide's ordinary uphill walk.
        assert!(steep_wall_plane(face(40.0), push).is_none());
        // Overhang: the ceiling clip stands as-is.
        assert!(steep_wall_plane(Vec3::new(-0.5, -0.7, 0.0).normalize(), push).is_none());
        // A true vertical wall manufactures no lift — nothing to fix.
        assert!(steep_wall_plane(face(90.0), push).is_none());
        // A receding face never opposes the motion.
        assert!(steep_wall_plane(face(60.0), -push).is_none());
    }
}
