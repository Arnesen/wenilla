//! The kinematic mover step — the walk/fall physics and the step-down snap, split out of the
//! `control` system ([`super`] keeps the input/camera/wire glue and the knob table this reads).
//! One call per frame: [`step`].
//!
//! Thin kinematic controller (decision 0009) over the **one-sided** mirror of avian's
//! `MoveAndSlide` (`crate::collision::one_sided`, decision 0970: a face only blocks motion its
//! authored winding opposes, the reference's `0x632700` law) — kept simple and robust on the
//! triangulated heightmap:
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
//!   - a steep face in the way is first *certified* by the atomic step-up ([`step_up`]):
//!     rise–advance–settle onto a walkable floor, or nothing (decision 0209). What a certified
//!     obstacle then costs is the reference's two-regime law (decision 1123): a rise inside the
//!     foot cone ([`FOOT_CONE_HEIGHT`]) is **ridden** up the cone's 61.6° skirt over the frames the
//!     gait needs ([`foot_cone_ride`]) — a kerb takes three at a run; only a rise above the cone is
//!     the instant pop, committed whole within the frame. Uncertified, nothing rises at all;
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
    move_trace, Player, AIR_NUDGE_SPEED, CAPSULE_HEIGHT, FEATHER_TERMINAL_VELOCITY,
    FOOT_CONE_HEIGHT, GRAVITY, GROUND_COS, GROUND_PROBE, HOVER_CLIMB_RATE, HOVER_HEIGHT,
    JUMP_SPEED, LAND_PROBE, SKIN_WIDTH, STEP_SLOPE_RATIO, STEP_SNAP_SLACK, STEP_UP_ADVANCE,
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

/// Advance the player mover one frame: settle hold, ground classify, the slide, and the
/// step-down snap. Writes `player.pos`/`vel_y`/`horiz_vel` (the settle *release* is the terrain
/// streamer's — decision 0737).
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
    water_floor: Option<f32>,
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
        crate::collision::one_sided::cast_move(ms, capsule, from, disp, SKIN_WIDTH, &filter)
    };
    let probe_down = |c: Vec3, dist: f32| cast(c, Vec3::NEG_Y * dist);

    // While airborne, "on the ground" means where the slide actually contacts the floor
    // ([`LAND_PROBE`], ~skin scale). The wider walking probe would end the arc up to 0.2 yd
    // early and close the gap with a same-frame snap — the visible pop at every silent landing
    // (decision 0190); the fall's own collision already stops the capsule exactly at contact.
    // A hovering body rests [`HOVER_HEIGHT`] above the floor (decision 0866), so every downward
    // reach that decides "am I standing on something" has to grow by the same amount — otherwise
    // the float reads as airborne and it falls, which is the hover bit doing nothing at all.
    let hover_offset = if player.modes.hover {
        HOVER_HEIGHT
    } else {
        0.0
    };
    let ground_reach = hover_offset
        + if player.airborne_since.is_some() {
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
    // Settle hold (post-teleport/summon/login): the streamed world — terrain *and* WMO building
    // floors + their colliders — arrives over several frames, so the ground under the snap isn't
    // there yet. While settling, `held` keeps gravity OFF and freezes us in place, so we don't
    // fall through the not-yet-loaded city/building (the loading screen stays up too). The
    // *release* does not live here (decision 0737): it is the terrain streamer's, keyed on the
    // destination's residency (scene + colliders, `WorldLoadProgress`) with the timeout backstop —
    // never on ground contact, which only the walk mover could observe and which a flyer, a
    // swimmer, or a genuinely airborne teleport never produces (the loading-screen-until-landing
    // hang). The streamer runs every frame in every mover mode, so every mode releases the same way.
    let held = player.settling;
    // **Rooted: the mover is ANCHORED — nothing advances the body, in any axis** (decision 0880).
    // `SetRoot 0x7c7340` is three acts, not one: set `0x1000`, `call 0x7c6290` **StopFalling**
    // (`and eax,0xffff9fff` — FALLING *and* FALLINGFAR together), then wipe the direction bits
    // (`and 0xffe07f00`) and re-run the basis recompute `0x7c5c20`, which with no direction bit left
    // to read builds a zero horizontal velocity. With FALLING clear the mode dispatcher `0x634040`
    // routes the next substep to the WALK resolver `0x6367b0` rather than the fall integrator
    // `0x635b00` — **the only place gravity lives** — and the walk resolver's own head gate returns
    // immediately when the substep's horizontal distance is under `2^-20`, so no down-probe, no
    // step-down snap and no fall election run either. Nothing is left that could move the body.
    //
    // That is why a root or a stun taken **mid-air leaves you hanging exactly where it caught you**,
    // and why the drop resumes only on release: `ClearRoot 0x7c7370` calls `0x7c61c0` StartFalling,
    // and `0x7c61c0` is precisely the entry that refuses while rooted
    // (`0x7c61d6 test dword ptr [ecx+0x40], 0x203800` — SWIMMING|FALLING|ROOT|FIXED_Z), so no fall
    // can begin under a root by any path. (wow-re `moveflag-family.md` §1/§5.3,
    // `step-vs-fall-election.md`.)
    let anchored = !held && player.modes.rooted;
    // A body part-way up a foot cone is standing (decision 1123). The probe above looks straight
    // down and finds only the steep riser it is riding, so on its own it would call a mid-ride frame
    // airborne — gravity would then undo the climb and the body would dwell on the face, the exact
    // failure 0209's atomic commit was built to make impossible. The ride is re-earned from the
    // certification every frame it continues, so this can hold nothing up that has not just proved
    // it can be climbed.
    let on_floor = !held && (on_walkable || player.cone_riding) && player.vel_y <= 0.0;

    // The wedged rest (decision 0211) stands until real ground takes over or the support
    // vanishes — we walked off the funnel wall into open air, which resumes a normal fresh fall.
    // Its own reach stays [`LAND_PROBE`] (not the classify reach above), plus the hover offset so a
    // hovering wedge is not read as having lost its support the moment the mode lands.
    if player.wedged
        && (on_floor || held || probe_down(center, LAND_PROBE + hover_offset).is_none())
    {
        player.wedged = false;
    }
    let grounded = on_floor || player.wedged;

    let mut jumped = false;
    if held || anchored {
        // Frozen: the settle's hold (no velocity until the ground loads under us) or the root's
        // anchor. The mover cannot tell them apart — both mean no gravity and no carried momentum.
        player.vel_y = 0.0;
        player.horiz_vel = Vec3::ZERO;
    } else if grounded {
        player.vel_y = 0.0;
        if want_jump {
            player.vel_y = JUMP_SPEED;
            player.wedged = false;
            player.cone_riding = false;
            jumped = true;
        }
    } else {
        // **Feather fall is a terminal-velocity substitution, and nothing else** (decision 0866).
        // The reference's gravity integrate `0x7c5d20` picks its clamp from one flag test
        // (`0x7c5d23 test [ecx+0x40], 0x20000000`) — the ordinary 60.148 or 7.0 under
        // `MOVEFLAG_SAFE_FALL`. Gravity itself is unchanged, so a Slow Fall still *accelerates*
        // normally for the first ~0.36 s and only then rides the cap: the drop starts like any
        // other and settles into a drift, which is what Slow Fall looks like.
        let terminal = if player.modes.feather_fall {
            FEATHER_TERMINAL_VELOCITY
        } else {
            TERMINAL_VELOCITY
        };
        player.vel_y = (player.vel_y - GRAVITY * dt).max(-terminal);
    }
    let mut air_nudged = false;
    // The anchor owns the horizontal too — the wipe leaves the basis recompute nothing to build a
    // velocity from, so a body rooted mid-jump stops dead in the air instead of coasting on its
    // frozen takeoff momentum. (Both arms below are already inert under a root — the caller zeroes
    // `dir`, so `moving` is false and `input_horiz` is zero — but the anchor says it itself rather
    // than inheriting it from a gate three functions away.)
    if grounded && !anchored {
        player.horiz_vel = input_horiz;
    } else if !held && !anchored && moving && player.horiz_vel.length_squared() < 0.01 {
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
    if !held && !anchored && grounded && !jumped {
        let g = grounded_step(
            ms,
            capsule,
            &filter,
            center,
            player.horiz_vel,
            time.delta(),
            hover_offset,
        );
        // The step-up probe (this is the LOCAL mover; a remote's dead-reckon is not a report
        // anyone is looking at): a walk frame that went nowhere writes the `stup` deep report —
        // the surface profile ahead, the advance ladder, the candidate faces.
        super::step_probe::watch(
            ms,
            capsule,
            &filter,
            center,
            g.center,
            player.horiz_vel,
            dt,
            time.elapsed_secs(),
        );
        center = g.center;
        climb = g.climb;
        snap_probe = g.snap;
        player.cone_riding = g.cone_riding;
        if let Some(e) = g.ground {
            ground_entity = Some(e);
        }
    } else {
        // Held or anchored: zero velocity (no move) — both already zeroed the two terms, but say it
        // outright. Jumping/airborne: gravity carries the arc.
        let velocity = if held || anchored {
            Vec3::ZERO
        } else {
            player.horiz_vel + Vec3::Y * player.vel_y
        };
        // The airborne slide is the OTHER shared resolve ([`airborne_step`]) — the same code a
        // remote mover's arc runs, so a jump meets our walls whoever is jumping (decision 0627).
        center = airborne_step(ms, capsule, &filter, center, velocity, time.delta());
        // Nothing here can be riding a cone: this arm is the arc, the hold and the anchor.
        player.cone_riding = false;
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
        && !anchored
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
    let mut grounded = grounded || player.wedged;

    // **The hover climb** (decision 0872): the snap above can only lower the body, so the *rise* to
    // the 1.0-yd clearance is this separate rate-limited pass — the reference's second writer at
    // `0x636fa1`–`0x6370f1`, which climbs toward the same clearance at [`HOVER_CLIMB_RATE`]. Without
    // it the grant reads as an instant pop; with it the body floats up over ~0.14 s.
    // (…and never while anchored: the climb is the walk resolver's own second pass, so the rooted
    // mover's stationary early-return skips it exactly like the snap above.)
    if hover_offset > 0.0 && !held && !anchored {
        if let Some(h) = probe_down(center, HOVER_HEIGHT + CAPSULE_HEIGHT) {
            let clearance = h.distance;
            if clearance < HOVER_HEIGHT {
                center.y += (HOVER_HEIGHT - clearance).min(HOVER_CLIMB_RATE * dt);
                player.vel_y = player.vel_y.max(0.0); // climbing, not falling
            }
        }
    }

    // **Water walking: the liquid surface IS the floor** (decision 0866). `water_floor` is the
    // surface Y the caller resolved, and it is `Some` only while the mode is granted AND we are not
    // already swimming — the reference's own gate, read at `0x631617` (`test eax,0x200000; jne`)
    // right after the water-walk test: a caster who is already submerged keeps swimming, and only
    // surfaces onto the water once out of it. Liquid is not a collider here (it is queried, not
    // swept), so it cannot come out of the probes above; it lands as a floor clamp instead — the
    // body may not sink past it, and resting on it is being grounded, which ends any arc.
    if let Some(surface) = water_floor {
        let feet = center.y - half_h.y;
        if feet <= surface {
            center.y = surface + half_h.y;
            player.vel_y = 0.0;
            player.wedged = false;
            grounded = true;
        }
    }

    player.pos = center - half_h;
    move_trace::frame(move_trace::Frame {
        y_in: pre_move.y - half_h.y,
        y_out: player.pos.y,
        grounded,
        on_walkable,
        vel_y: player.vel_y,
        snap: snap_probe,
        climb,
        anchored,
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
    /// The height gain (yd) this frame's climb achieved — the atomic step-up's committed rise, or
    /// the distance a foot-cone ride carried the body up its skirt.
    pub(crate) climb: Option<f32>,
    /// The body is part-way up a **foot-cone ride** and is *supported by the edge it is riding*,
    /// not standing on a floor — so the caller must keep treating it as grounded until a walkable
    /// floor takes over (decision 1123). Always `false` for a frame that did not ride.
    pub(crate) cone_riding: bool,
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
    surface_offset: f32,
) -> GroundedStep {
    let cast = |from: Vec3, disp: Vec3| {
        crate::collision::one_sided::cast_move(ms, capsule, from, disp, SKIN_WIDTH, filter)
    };
    let speed = horiz_vel.length();
    // The step-up (decision 0209): ATOMIC — a steep face in the way triggers rise →
    // advance-this-frame's-travel-at-the-raised-height → settle onto a walkable floor, all
    // committed inside this one frame, or nothing happens and the plain slide runs. There is
    // no in-between state to be seen wedged or bouncing in (the 0191 ride dwelled mid-face;
    // every stuck/bounce report of the step-up era was that dwelling). Grazing a face nets
    // back onto the same floor (reads as sliding); a square push onto a low step lands on its
    // top; anything taller than [`STEP_UP_HEIGHT`] never commits.
    let attempt = if speed > 1.0e-6 {
        let travel = speed * dt.as_secs_f32();
        // The look-ahead is this frame's travel — "is there a steep face in my way *now*" is a
        // question about this frame. The **advance** is not: how far forward the maneuver must
        // reach to see the tread it would stand on is a property of the body, so it is at least
        // [`STEP_UP_ADVANCE`] whatever the frame rate or the gait (decision 1121). Travel still
        // wins when it is longer, so a very low frame rate never steps you less far than you asked
        // to walk.
        step_up(
            &cast,
            center,
            horiz_vel / speed,
            travel,
            travel.max(STEP_UP_ADVANCE),
        )
    } else {
        StepAttempt {
            contact: None,
            verdict: StepVerdict::NoFace,
        }
    };
    // The certify trace (`WOW_MOVE_TRACE`, tag `step`): one line per attempt with every probe
    // number and the world-space contact, so a feel report pins to the exact placement and probe —
    // the instrument that broke the fence/tree cases, which reasoning alone could not. (The
    // *blocked-frame* deep report — the advance ladder and the surface profile — is the `stup` tag
    // in [`super::step_probe`], which the local mover fires when a walk frame goes nowhere.)
    if let Some((point, n)) = attempt.contact {
        if crate::dbg_trace::enabled_for("step") {
            let feet_y = center.y - CAPSULE_HEIGHT * 0.5;
            crate::dbg_trace::line(
                "step",
                &format!(
                    "hit ({:8.2},{:7.2},{:8.2}) h={:+.2} n=({:+.2},{:+.2},{:+.2}) {}",
                    point.x,
                    point.y,
                    point.z,
                    point.y - feet_y,
                    n.x,
                    n.y,
                    n.z,
                    attempt.verdict
                ),
            );
        }
    }
    // **Which regime** (decision 1123, wow-re `climb-vs-slide.md` §2/§4/§6). The certification above
    // settles *whether* the obstacle can be cleared; its committed rise settles *how*. The real
    // client's solid is a cone below [`FOOT_CONE_HEIGHT`] and a vertical box above it, so a low edge
    // never meets a wall to be lifted over — it meets the slanted skirt, and the ordinary slide
    // carries the body up it. Only an edge that clears the cone meets the box square, and that
    // square meeting is the instant step-up. One law, two outcomes, selected by the rise:
    //   - `dy ≤ FOOT_CONE_HEIGHT` ⇒ **ride** the skirt at atan 1.8494 ≈ 61.6°, over as many frames
    //     as the gait needs (a kerb takes two at a run) — smooth, and never a teleport;
    //   - `dy > FOOT_CONE_HEIGHT` ⇒ the atomic pop of 0209, unchanged;
    //   - no certification at all ⇒ neither: the plain slide, and the body never rises. (This is
    //     the gate that keeps the ride honest — a tall wall's contact point sits *inside* the cone
    //     band too, so height alone would ride it. Only the certification distinguishes them.)
    let ride_to = match attempt.verdict {
        StepVerdict::Commit { landed, dy, .. } if dy <= FOOT_CONE_HEIGHT => Some(landed.y),
        _ => None,
    };
    if ride_to.is_none() {
        if let Some(landed) = attempt.verdict.landed() {
            // The committed maneuver IS this frame's motion — already settled on a walkable floor,
            // so the slide and the snap below are skipped.
            return GroundedStep {
                center: landed,
                ground: None,
                climb: Some(landed.y - center.y),
                snap: None,
                cone_riding: false,
            };
        }
    }
    let mut rode = false;
    let out = crate::collision::one_sided::move_and_slide(
        ms,
        capsule,
        center,
        horiz_vel,
        dt,
        &MoveAndSlideConfig::default(),
        filter,
        |hit| {
            if let Some(ride) = walkable_ride_velocity(**hit.normal, *hit.velocity) {
                *hit.velocity = ride;
                return MoveAndSlideHitResponse::Accept;
            }
            // The foot cone's skirt, on a certified low edge only. The ceiling is the
            // certification's own landing: once the body is that high the obstacle is cleared, so
            // the skirt has nothing left to ride and any further contact is an ordinary wall. Any
            // overshoot inside a sub-step is taken back out by the election snap below, which only
            // ever descends onto a walkable floor.
            if ride_to.is_some_and(|ceiling| hit.position.y < ceiling) {
                if let Some(up) = foot_cone_ride(**hit.normal, *hit.velocity) {
                    *hit.velocity = up;
                    rode = true;
                    return MoveAndSlideHitResponse::Accept;
                }
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
    // `surface_offset` is HOVER (decision 0866): the reference's WALK resolver `0x6367b0` adds
    // `[0x7ff9d8]` = 1.0 to this same surface offset while `MOVEFLAG_HOVER` is set, and widens the
    // step-down reach by the same yard (`0x633e35`) so the float still follows the ground down.
    // Both halves are here: the reach grows by the offset, and the snap stops that far short of the
    // floor. Zero for everyone not hovering, which is the ordinary case and unchanged.
    let d = slid - center;
    let reach =
        d.x.hypot(d.z) * STEP_SLOPE_RATIO + STEP_SNAP_SLACK + CAPSULE_HEIGHT + surface_offset;
    let hit = cast(slid, Vec3::NEG_Y * reach);
    let snap = Some((reach, hit.as_ref().map(|h| (h.distance, h.normal1.y))));
    let mut ground = None;
    if let Some(h) = hit.filter(|h| h.normal1.y >= GROUND_COS) {
        // `max(…, 0)` is the reference's, and it is the difference between a float and a pop: the
        // snap **only ever descends** (`0x636e8d`–`0x636e9e` skips the write when `L − 1.0 < 0`), so
        // a hovering body that is already within its clearance is left where it is rather than
        // yanked up to it. The rise to clearance is a separate rate-limited climb — see [`step`].
        slid.y -= (h.distance - surface_offset).max(0.0);
        ground = Some(h.entity);
    }
    GroundedStep {
        center: slid,
        // A ride is a climb too — the trace reads the same whichever regime moved the body.
        climb: rode.then_some(slid.y - center.y),
        // Mid-ride the body is held up by the edge under its skirt, and the frame-start ground
        // probe can only see the steep riser there. Say so, so the caller keeps it standing
        // instead of letting gravity undo the climb. A snap onto a walkable floor ends the ride:
        // the tread is under the feet and ordinary grounding takes over from here.
        cone_riding: rode && ground.is_none(),
        ground,
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
    crate::collision::one_sided::move_and_slide(
        ms,
        capsule,
        center,
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

/// **The foot cone's ride** — the smooth half of the reference's climb law (decision 1123).
///
/// The real client's movement solid is a **cone below the waist**: the k-DOP build at `0x631440`
/// emits four bevels running from a point at the foot out to the full radius at
/// `foot + radius·1.8493990`, and only above that height is it a vertical box (wow-re
/// `climb-vs-slide.md` §2 — the `n.z < 0` sign on those planes is the tell that the cone narrows
/// *downward*, so it is a foot cone and not a top-rim chamfer). A low edge therefore never presents
/// the mover a wall to be lifted over: it presents the slanted skirt, and the resolver's own slide
/// runs the body up it.
///
/// The gain is the note's `T` at §4 — `1.8494 · cosθ · len`, where `θ` is how squarely the approach
/// meets the face — and that is exactly this projection: the closing horizontal speed (the dot
/// product supplies `cosθ`) times [`STEP_SLOPE_RATIO`], the cone's own surface slope. Horizontal
/// speed is untouched, as it is on every walkable ride ([`walkable_ride_velocity`]); the grounded
/// mover owns no vertical of its own, so this *sets* the vertical rather than adding to it.
///
/// Only steep, non-overhanging, opposing faces ride — a walkable face already had its ride, and an
/// overhang is a ceiling. **The caller owns the real gate:** this says only "here is what the skirt
/// would do", never "the body may climb". Whether the obstacle can be cleared at all is the
/// certification's call in [`grounded_step`], because a tall wall's contact point sits inside the
/// cone band too and height alone cannot tell the two apart.
fn foot_cone_ride(n: Vec3, v: Vec3) -> Option<Vec3> {
    if !(0.0..GROUND_COS).contains(&n.y) {
        return None;
    }
    // Steepness bounds the horizontal part below by sin 50°, so the normalize is safe.
    let h = Vec3::new(n.x, 0.0, n.z).normalize();
    let into = -(v.x * h.x + v.z * h.z);
    if into <= 0.0 {
        return None;
    }
    Some(Vec3::new(v.x, into * STEP_SLOPE_RATIO, v.z))
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

/// What one atomic step-up attempt decided — the structured form of the `step` trace line.
///
/// Structured rather than logged-and-forgotten because the diagnostic probe
/// ([`super::step_probe`]) re-runs the *same* maneuver at a ladder of forward advances and reads
/// the reason back off every rung. "Why did this step fail, and what would have made it succeed"
/// is then one table in the trace, not a text parse of six different format strings.
#[derive(Clone, Copy, Debug)]
pub(crate) enum StepVerdict {
    /// Nothing steep and opposing within the look-ahead — not a step-up frame at all.
    NoFace,
    /// The rise found no headroom above the capsule.
    NoHeadroom,
    /// The settle probe found no floor at all under the advanced point.
    NoFloor { up: f32, fwd: f32 },
    /// The settle found a floor, but one too steep to stand on (`ny` under [`GROUND_COS`]).
    SteepFloor {
        up: f32,
        fwd: f32,
        dist: f32,
        ny: f32,
    },
    /// The maneuver gained no height — a graze, a too-tall wall, or a pinch's gap floor. The plain
    /// slide owns the frame.
    NetZero {
        up: f32,
        fwd: f32,
        dist: f32,
        ny: f32,
        dy: f32,
    },
    /// Committed: this landing **is** the frame's motion.
    Commit {
        landed: Vec3,
        up: f32,
        fwd: f32,
        dy: f32,
    },
}

impl StepVerdict {
    /// The committed capsule centre, if this attempt took the frame.
    pub(crate) fn landed(self) -> Option<Vec3> {
        match self {
            StepVerdict::Commit { landed, .. } => Some(landed),
            _ => None,
        }
    }
}

impl std::fmt::Display for StepVerdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            StepVerdict::NoFace => write!(f, "no opposing face"),
            StepVerdict::NoHeadroom => write!(f, "up=0.00 NO-HEADROOM -> slide"),
            StepVerdict::NoFloor { up, fwd } => {
                write!(f, "up={up:.2} fwd={fwd:.2} down=miss NO-FLOOR -> slide")
            }
            StepVerdict::SteepFloor { up, fwd, dist, ny } => write!(
                f,
                "up={up:.2} fwd={fwd:.2} down=(d={dist:.2} ny={ny:+.2}) STEEP-FLOOR -> slide"
            ),
            StepVerdict::NetZero {
                up,
                fwd,
                dist,
                ny,
                dy,
            } => write!(
                f,
                "up={up:.2} fwd={fwd:.2} down=(d={dist:.2} ny={ny:+.2}) dy={dy:+.3} NET-ZERO -> slide"
            ),
            StepVerdict::Commit { up, fwd, dy, .. } => {
                write!(f, "up={up:.2} fwd={fwd:.2} dy={dy:+.3} -> COMMIT")
            }
        }
    }
}

/// One step-up attempt: the opposing face that triggered it (world contact point + its authored
/// normal), and what the maneuver decided about it.
pub(crate) struct StepAttempt {
    /// `None` when no steep opposing face was within the look-ahead — there was nothing to step.
    pub(crate) contact: Option<(Vec3, Vec3)>,
    pub(crate) verdict: StepVerdict,
}

/// The atomic step-up (decision 0209) — the standard kinematic-controller maneuver, *not* the
/// reference resolver's (that direction is closed, 0207): a steep opposing face within `look`
/// triggers **rise → advance → settle**, committed whole inside this one frame, or nothing.
///
/// - **Rise** by the free headroom, at most [`STEP_UP_HEIGHT`] — the deliberately low ceiling
///   that scopes this to stairs/doorsteps/low rocks and keeps fences and walls slide-only.
/// - **Advance** by `advance` along the *input* direction at the raised height.
/// - **Settle** back down by the walk election's own reach; commit **only onto a walkable
///   floor that is actually higher**.
///
/// Case by case: a square push at a low step lands ON its top this frame; a grazing rub
/// settles back onto the same floor — net zero, reads as *sliding along*; a face taller than
/// the ceiling leaves no forward clearance at the raised height ⇒ the settle lands back on
/// the origin floor ⇒ slide; a pinch between two tree trunks offers only steep landings ⇒
/// **no commit, ever** — the wedge/bounce class of 0191–0195 is impossible by construction,
/// because there is no intermediate mid-climb state to be caught in.
///
/// **`look` and `advance` are separate parameters only so the maneuver is measurable.** The live
/// mover passes this frame's own travel for both (0209's design — never a probe-length lunge);
/// the diagnostic probe ([`super::step_probe`]) sweeps `advance` to find the offset at which the
/// settle probe would have cleared the obstacle's lip, which is the number a "it won't step up
/// this curb" report is actually about.
pub(crate) fn step_up(
    cast: &impl Fn(Vec3, Vec3) -> Option<MoveHitData>,
    center: Vec3,
    dir_h: Vec3,
    look: f32,
    advance: f32,
) -> StepAttempt {
    let none = |verdict| StepAttempt {
        contact: None,
        verdict,
    };
    // A steep, non-overhanging face opposing the motion, within `look` (+skin).
    // No incidence gate — the verified ref has none; grazing nets zero through the settle.
    let Some(ahead) = cast(center, dir_h * look) else {
        return none(StepVerdict::NoFace);
    };
    let n = ahead.normal1;
    if n.y >= GROUND_COS || n.y < 0.0 || n.dot(dir_h) >= 0.0 {
        return none(StepVerdict::NoFace);
    }
    let at = |verdict| StepAttempt {
        contact: Some((ahead.point1, n)),
        verdict,
    };

    // Rise: the free headroom, at most H.
    let up = cast(center, Vec3::Y * STEP_UP_HEIGHT).map_or(STEP_UP_HEIGHT, |h| h.distance);
    if up < 1e-3 {
        return at(StepVerdict::NoHeadroom);
    }
    // Advance: along the input dir, swept at the raised height.
    let raised = center + Vec3::Y * up;
    let fwd = cast(raised, dir_h * advance).map_or(advance, |h| h.distance);
    let over = raised + dir_h * fwd;
    // Settle: the walk election's reach below the advanced point — the rise undone, plus the
    // travel-scaled step-down allowance (decisions 0182/0190) — onto a WALKABLE floor only.
    let reach = up + advance * STEP_SLOPE_RATIO + STEP_SNAP_SLACK;
    let Some(down) = cast(over, Vec3::NEG_Y * reach) else {
        return at(StepVerdict::NoFloor { up, fwd });
    };
    let (dist, ny) = (down.distance, down.normal1.y);
    if ny < GROUND_COS {
        return at(StepVerdict::SteepFloor { up, fwd, dist, ny });
    }
    let landed = over + Vec3::NEG_Y * dist;
    let dy = landed.y - center.y;
    // Commit only a landing that actually gained a floor. A net-zero maneuver (grazing a face,
    // pushing a too-tall wall, the tree pinch's gap grass) belongs to the plain slide — its
    // deflection is what "sliding along the fence" is; committing here would dead-stop it.
    if dy <= 0.05 {
        return at(StepVerdict::NetZero {
            up,
            fwd,
            dist,
            ny,
            dy,
        });
    }
    at(StepVerdict::Commit {
        landed,
        up,
        fwd,
        dy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::CAPSULE_RADIUS;
    use bevy::ecs::system::RunSystemOnce;

    /// A headless physics world holding **the kerb the director's capture measured** — Stormwind
    /// Trade District, decision 1121: a 0.28 yd sidewalk whose riser is a ~61° bevel, not a
    /// vertical face. Profiled from the `stup` down-ray scan at the real spot (street out to
    /// +0.20, the bevel's face normal `ny=+0.49`, flat tread `ny=+0.99` from +0.50 on), so the
    /// fixture is the geometry, not an idea of it.
    ///
    /// The profile is a `(x, y)` polyline extruded across `z`, wound so every face's **authored**
    /// normal points up and back at the approaching body — the one-sided law (0970) is live in
    /// these casts, so a mis-wound fixture would silently be a hole to fall through.
    fn world_with_kerb() -> App {
        const PROFILE: [(f32, f32); 4] = [(-2.0, 0.0), (0.29, 0.0), (0.446, 0.28), (3.0, 0.28)];
        const W: f32 = 3.0;
        let mut app = App::new();
        // avian's collider backend reads `Assets<Mesh>` and `SceneSpawner` even in a meshless
        // world, so the headless asset/scene plugins ride along.
        app.add_plugins((
            MinimalPlugins,
            bevy::transform::TransformPlugin,
            bevy::asset::AssetPlugin::default(),
            bevy::scene::ScenePlugin,
            PhysicsPlugins::new(bevy::app::PostUpdate),
        ));
        app.init_asset::<Mesh>();
        let (mut verts, mut tris) = (Vec::new(), Vec::new());
        for w in PROFILE.windows(2) {
            let (&(x0, y0), &(x1, y1)) = (&w[0], &w[1]);
            let b = verts.len() as u32;
            verts.extend([
                Vec3::new(x0, y0, -W),
                Vec3::new(x1, y1, -W),
                Vec3::new(x1, y1, W),
                Vec3::new(x0, y0, W),
            ]);
            // (a, c, b) / (a, d, c): normal = (-dy, dx, 0) — up for the flats, up-and-back for
            // the riser. The reverse winding is a backface and blocks nothing at all.
            tris.extend([[b, b + 2, b + 1], [b, b + 3, b + 2]]);
        }
        app.world_mut().spawn((
            RigidBody::Static,
            Collider::trimesh(verts, tris),
            Transform::default(),
        ));
        app.update(); // one frame builds Position/Rotation and the spatial-query trees
        app
    }

    fn player_capsule() -> Collider {
        Collider::capsule(CAPSULE_RADIUS, CAPSULE_HEIGHT - 2.0 * CAPSULE_RADIUS)
    }

    /// Walk the capsule into the kerb exactly as the mover does, then run one step-up attempt at
    /// `advance`. Returns the verdict from where the slide actually left the body — not from a
    /// hand-placed pose, which is how a fixture ends up testing a spot the mover never reaches.
    fn step_at(advance: f32) -> StepVerdict {
        world_with_kerb()
            .world_mut()
            .run_system_once(move |ms: MoveAndSlide| {
                let capsule = player_capsule();
                let filter = SpatialQueryFilter::default();
                let cast = |from: Vec3, disp: Vec3| {
                    crate::collision::one_sided::cast_move(
                        &ms, &capsule, from, disp, SKIN_WIDTH, &filter,
                    )
                };
                // Approach from 1 yd back along +X at street level and stop where the kerb stops us.
                let start = Vec3::new(-1.0, CAPSULE_HEIGHT * 0.5, 0.0);
                let run = cast(start, Vec3::X).map_or(1.0, |h| h.distance);
                let center = start + Vec3::X * run;
                step_up(&cast, center, Vec3::X, TRAVEL_60FPS, advance).verdict
            })
            .unwrap()
    }

    /// One frame's travel at a run (7.0 yd/s) and 60 fps — 0209's advance, and the number the
    /// capture caught failing.
    const TRAVEL_60FPS: f32 = 7.0 / 60.0;

    /// Walk the capsule into the kerb and run `frames` consecutive **whole** grounded steps from
    /// wherever the last one left it — the mover's own loop, so what these assert is the behaviour
    /// on screen and not a single probe in isolation.
    fn walk_kerb(start_y: f32, frames: usize) -> Vec<(f32, Option<f32>, bool)> {
        world_with_kerb()
            .world_mut()
            .run_system_once(move |ms: MoveAndSlide| {
                let capsule = player_capsule();
                let filter = SpatialQueryFilter::default();
                let cast = |from: Vec3, disp: Vec3| {
                    crate::collision::one_sided::cast_move(
                        &ms, &capsule, from, disp, SKIN_WIDTH, &filter,
                    )
                };
                let start = Vec3::new(-1.0, CAPSULE_HEIGHT * 0.5 + start_y, 0.0);
                let run = cast(start, Vec3::X).map_or(1.0, |h| h.distance);
                let mut center = start + Vec3::X * run;
                let dt = std::time::Duration::from_secs_f32(TRAVEL_60FPS / 7.0);
                (0..frames)
                    .map(|_| {
                        let g =
                            grounded_step(&ms, &capsule, &filter, center, Vec3::X * 7.0, dt, 0.0);
                        center = g.center;
                        // Feet height relative to the street, the climb, and the ride latch.
                        (center.y - CAPSULE_HEIGHT * 0.5, g.climb, g.cone_riding)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap()
    }

    #[test]
    fn the_kerb_is_ridden_up_its_skirt_never_popped() {
        // Decision 1123, the whole point: a 0.28 yd kerb is inside the foot cone
        // ([`FOOT_CONE_HEIGHT`] ≈ 0.62), so the certified obstacle is *ridden* — a smooth diagonal
        // over the frames the gait needs — instead of the body being teleported onto the tread in
        // one frame. Both halves are asserted, because either alone is satisfiable by a bug: it
        // must arrive on the tread, AND no single frame may deliver the whole rise.
        let frames = walk_kerb(0.0, 4);
        let arrived = frames
            .iter()
            .position(|&(y, _, _)| (y - 0.28).abs() < 0.03)
            .expect("the ride should put the feet on the 0.28 yd tread");
        assert!(
            arrived > 0,
            "arriving on the very first frame is the pop, not a ride: {frames:?}"
        );
        assert!(
            frames[0].2,
            "the first frame is mid-ride and must report itself grounded: {frames:?}"
        );
        assert!(
            frames[0].0 > 0.0 && frames[0].0 < 0.28,
            "the first frame should be part-way up the skirt, got {:+.3}",
            frames[0].0
        );
    }

    #[test]
    fn a_wall_is_never_ridden() {
        // The certification is the gate, not the height (a tall wall's contact point sits inside
        // the cone band too). Read the same geometry from a full kerb below — a 2.3 yd wall — and
        // nothing may rise: no ride, no climb, no lift, just the slide. This is the check that
        // stops the ride becoming a ladder up every cliff in the world.
        for (y, climb, riding) in walk_kerb(-2.0, 4) {
            assert!(!riding, "a wall must never start a cone ride");
            assert!(climb.is_none(), "a wall must never register a climb");
            assert!(
                y < -2.0 + 0.05,
                "the body must not rise against a wall, feet at {y:+.3}"
            );
        }
    }

    #[test]
    fn the_cone_ride_gains_the_reference_slope() {
        // wow-re `climb-vs-slide.md` §4: `T = 1.8494 · cosθ · len`. Head-on, the gain is the cone's
        // own surface slope times the speed…
        let head_on = foot_cone_ride(Vec3::new(-1.0, 0.0, 0.0), Vec3::X * 7.0).unwrap();
        assert!(
            (head_on.y - 7.0 * STEP_SLOPE_RATIO).abs() < 1e-4,
            "{head_on:?}"
        );
        assert_eq!(
            head_on.x, 7.0,
            "horizontal speed is never touched by a ride"
        );
        // …and meeting the same face at 60° gains exactly cos60° of it, which is the `cosθ` term
        // falling out of the closing-speed projection rather than being applied by hand.
        let oblique = foot_cone_ride(
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::new(7.0 * 0.5, 0.0, 7.0 * 0.866_025_4),
        )
        .unwrap();
        assert!(
            (oblique.y - 7.0 * 0.5 * STEP_SLOPE_RATIO).abs() < 1e-3,
            "{oblique:?}"
        );
    }

    #[test]
    fn the_cone_ride_declines_what_is_not_its_business() {
        let v = Vec3::X * 7.0;
        // A walkable face already had its ride ([`walkable_ride_velocity`])…
        assert!(foot_cone_ride(Vec3::new(-0.5, 0.866, 0.0).normalize(), v).is_none());
        // …an overhang is a ceiling, not a skirt…
        assert!(foot_cone_ride(Vec3::new(-0.5, -0.866, 0.0).normalize(), v).is_none());
        // …and a face we are moving away from is not in the way at all.
        assert!(foot_cone_ride(Vec3::new(1.0, 0.0, 0.0), v).is_none());
    }

    #[test]
    fn the_kerb_is_out_of_reach_of_one_frames_travel() {
        // The defect the capture pinned (decision 1121): the settle probe is still over the bevel
        // at 0.117 yd, so it lands on a 61° face and the walkable gate — correctly — refuses it.
        // The step-up is not wrong about the face; it never looked far enough to see the tread.
        let v = step_at(TRAVEL_60FPS);
        assert!(
            matches!(v, StepVerdict::SteepFloor { .. }),
            "one frame's travel should still be over the bevel, got {v}"
        );
    }

    #[test]
    fn a_body_scaled_advance_climbs_the_kerb() {
        // …and a body radius ahead, the same probe is over the tread and commits onto it. `dy` is
        // the kerb's real height, so this pins that we land ON the sidewalk, not part-way up its
        // bevel.
        let v = step_at(STEP_UP_ADVANCE);
        let StepVerdict::Commit { dy, .. } = v else {
            panic!("a body-radius advance should reach the tread, got {v}");
        };
        assert!(
            (dy - 0.28).abs() < 0.03,
            "should land on the 0.28 yd tread, gained {dy:+.3}"
        );
    }

    #[test]
    fn the_advance_never_climbs_past_the_rise_ceiling() {
        // The reach grew; what may be climbed did not (decision 1121). A wall taller than
        // [`STEP_UP_HEIGHT`] clips the elevated sweep, so the settle falls back to the origin
        // floor and the plain slide keeps the frame — the fence/trunk behaviour 0209 was built
        // for, asserted at the advance that made the kerb work.
        let v = world_with_kerb()
            .world_mut()
            .run_system_once(move |ms: MoveAndSlide| {
                let capsule = player_capsule();
                let filter = SpatialQueryFilter::default();
                let cast = |from: Vec3, disp: Vec3| {
                    crate::collision::one_sided::cast_move(
                        &ms, &capsule, from, disp, SKIN_WIDTH, &filter,
                    )
                };
                // Stand the body a full kerb below the tread — the same geometry read as a 2.3 yd
                // wall by dropping the approach to y = −2.0, where the tread is far overhead.
                let start = Vec3::new(-1.0, CAPSULE_HEIGHT * 0.5 - 2.0, 0.0);
                let run = cast(start, Vec3::X).map_or(1.0, |h| h.distance);
                step_up(
                    &cast,
                    start + Vec3::X * run,
                    Vec3::X,
                    TRAVEL_60FPS,
                    STEP_UP_ADVANCE,
                )
                .verdict
            })
            .unwrap();
        assert!(
            v.landed().is_none(),
            "a face above the rise ceiling must never commit, got {v}"
        );
    }

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
