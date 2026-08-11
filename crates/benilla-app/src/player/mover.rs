//! The kinematic mover step — the walk/fall physics and the step-down snap, split out of the
//! `control` system ([`super`] keeps the input/camera/wire glue and the knob table this reads).
//! One call per frame: [`step`].
//!
//! Thin kinematic controller (decision 0009) over the **one-sided** mirror of avian's
//! `MoveAndSlide` (`benilla_world::collision::one_sided`, decision 0970: a face only blocks motion its
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
//!   - a steep face's response is **horizontal only** and the descent passes through it untouched
//!     ([`steep_contact_shear`], the reference's own `0x635090`): the body follows the surface
//!     down at full free-fall rate, no contact can manufacture lift at any angle, and a jump at a
//!     hillside costs the jump instead of banking its height;
//!   - airborne → gravity carries the arc, with a one-shot nudge to steer a standstill jump;
//!   - a fall whose descent stalls (a capsule wedged between steep faces — the
//!     tree-pinch funnel) *lands there*: standing, walking control live, instead of hanging in
//!     the falling pose forever with mid-air control locked (decisions 0211/0212).

use avian3d::character_controller::move_and_slide::MoveHitData;
use avian3d::prelude::*;
use bevy::prelude::*;

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
    world: &benilla_world::collision::WorldCollision<'_, '_>,
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
    // camera-only ones); the camera sweep uses its own filter (see `benilla_world::collision`).
    let cast = |from: Vec3, disp: Vec3| world.cast_body(capsule, from, disp, SKIN_WIDTH);
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
    let on_floor = !held && (on_walkable || player.steep_support) && player.vel_y <= 0.0;

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
            player.steep_support = false;
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
            world,
            capsule,
            center,
            player.horiz_vel,
            time.delta(),
            Support {
                offset: hover_offset,
                steep: player.steep_support,
            },
        );
        // The step-up probe (this is the LOCAL mover; a remote's dead-reckon is not a report
        // anyone is looking at): a walk frame that went nowhere writes the `stup` deep report —
        // the surface profile ahead, the advance ladder, the candidate faces.
        super::step_probe::watch(
            world,
            capsule,
            center,
            g.center,
            player.horiz_vel,
            dt,
            time.elapsed_secs(),
        );
        center = g.center;
        climb = g.climb;
        snap_probe = g.snap;
        player.steep_support = g.steep_support;
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
        center = airborne_step(world, capsule, center, velocity, time.delta());
        // Nothing here can be riding a cone: this arm is the arc, the hold and the anchor.
        player.steep_support = false;
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
            benilla_assets::trace::line(
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
        dx: (center - pre_move).xz().length(),
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

/// **What already holds the body up, entering the frame** — the two facts [`grounded_step`] cannot
/// work out for itself, because both are carried from the *previous* frame's resolve.
///
/// They travel together because they are the same question asked twice: how far below the feet does
/// "the ground" start, and does the thing under us count as a floor at all.
#[derive(Clone, Copy, Default)]
pub(crate) struct Support {
    /// The body rests this far **above** the surface — HOVER's float (decision 0866),
    /// [`super::HOVER_HEIGHT`] while the mode is up and `0.0` for everyone else, which is the
    /// ordinary case.
    pub(crate) offset: f32,
    /// The support entering the frame is a **certified steep contact**, not a walkable floor — the
    /// reference's `0x4000000` (decision 1125). It is the sole gate on the step-down probe's deep
    /// reach; see the reach in [`grounded_step`].
    pub(crate) steep: bool,
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
    /// The body is **supported by a certified steep contact** rather than standing on a walkable
    /// floor — climbing a foot-cone ride (1123) or following a surface down off a ledge (1127) —
    /// so the caller must keep treating it as grounded until a walkable floor takes over. This is
    /// the reference's `0x4000000` (decision 1125). `false` for an ordinary frame.
    pub(crate) steep_support: bool,
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
    world: &benilla_world::collision::WorldCollision<'_, '_>,
    capsule: &Collider,
    center: Vec3,
    horiz_vel: Vec3,
    dt: std::time::Duration,
    support: Support,
) -> GroundedStep {
    let surface_offset = support.offset;
    let cast = |from: Vec3, disp: Vec3| world.cast_body(capsule, from, disp, SKIN_WIDTH);
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
        if benilla_assets::trace::enabled_for("step") {
            let feet_y = center.y - CAPSULE_HEIGHT * 0.5;
            benilla_assets::trace::line(
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
    // **Which regime** (decisions 1123/1126, wow-re `climb-vs-slide.md` §2/§4/§6). The certification
    // above settles *whether* the obstacle can be cleared; the height of the **blocking edge** — the
    // face the look-ahead is pressed against, measured from the feet — settles *how*. The real
    // client's solid is a cone below [`FOOT_CONE_HEIGHT`] and a vertical box above it, so a low edge
    // never meets a wall to be lifted over: it meets the slanted skirt, and the ordinary slide
    // carries the body up it. Only an edge that clears the cone meets the box square, and that square
    // meeting is the instant step-up. One law, two outcomes:
    //   - edge inside the cone ⇒ **ride** the skirt at atan 1.8494 ≈ 61.6°, over as many frames as
    //     the gait needs — smooth, and never a teleport;
    //   - edge above the cone ⇒ the atomic pop of 0209, unchanged;
    //   - no certification at all ⇒ neither: the plain slide, and the body never rises.
    //
    // **The certification is the gate, not the height** — a tall wall's contact against a *capsule*
    // sits at the hemisphere centre, ≈0.33 yd up and squarely inside the band, so height alone would
    // ride a cliff. Only "the settle found a walkable floor" separates the two.
    //
    // 1123 selected on the certified *rise* instead, which was indistinguishable while the advance was
    // one body radius and became wrong the moment 1126 lengthened it: the rise then measures how high
    // the ground is a yard further on, not how tall the thing in front of you is. Booty Bay's 0.40 yd
    // step onto a rising ramp reads as a 0.755 yd obstacle under that rule and teleports; its blocking
    // edge is 0.40 and rides. The edge is the quantity the reference's own cone band tests, and unlike
    // the rise it does not move when the probe reaches further.
    let ride_to = match (attempt.verdict, attempt.contact) {
        (StepVerdict::Commit { landed, .. }, Some((p, _)))
            if p.y - (center.y - CAPSULE_HEIGHT * 0.5) <= FOOT_CONE_HEIGHT =>
        {
            Some(landed.y)
        }
        _ => None,
    };
    // **The pop is a RISE, not a jump forward** (decision 1130). 0209 committed the whole maneuver as
    // one position — rise, advance the probe's full [`STEP_UP_ADVANCE`], and land on the floor the
    // probe found — which put a **1.19 yd horizontal teleport** in every step-up: ten frames' worth
    // of travel in one frame, at ten times walking speed. That lurch, not the height, is what the
    // director reported as Goldshire's tables feeling teleporty.
    //
    // The reference does none of it (wow-re `ret2-commit-law.md`, a §5 trio):
    //   - `max(H·tan50°, r+1/720)` = 1.1917536 is **only ever a sweep distance**. `ebx` @`0x636193`
    //     has exactly two uses, both the length argument to `0x632ba0`. It is never added to `pos`.
    //   - the resolver holds `L / t_remaining` invariant across hits, deflections and misses, so the
    //     frame's **total horizontal is bounded by its own substep budget** — `dx = 1.19` where a
    //     walking frame is 0.117 cannot arise on this path at all;
    //   - all seven position writes in `[0x6367b0, 0x637140)` are **relative**. There is no absolute
    //     snap anywhere in the walk resolver, so landing the body *on* a probe's floor was wrong in
    //     kind and not merely in magnitude;
    //   - what the certified arm commits is a free vertical segment of `H` (`0x635d5b` — the response
    //     is always `(0,0,s)`), after which `0x63694d` restores **the caller's own heading** with the
    //     budget that is left.
    //
    // So: rise in place, then let the ordinary slide and the ordinary settle below own the frame,
    // exactly as they do on flat ground. The settle cannot strand the body up there — the reference
    // sets `0x4000000` *before* computing the settle's reach and only clears it *after*
    // (`0x636edf`, inside the settle), so the probe is always at least `H + 1/36` and always reaches
    // back past the height just gained: it finds the obstacle's top, or the ground we left.
    let popped = match attempt.verdict {
        StepVerdict::Commit { up, .. } if ride_to.is_none() => Some(up),
        _ => None,
    };
    let start = center + Vec3::Y * popped.unwrap_or(0.0);
    let mut rode = false;
    let out = world.slide_body(
        capsule,
        start,
        horiz_vel,
        dt,
        &MoveAndSlideConfig::default(),
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
            if let Some(followed) = steep_contact_shear(**hit.normal, *hit.velocity) {
                *hit.velocity = followed;
            }
            MoveAndSlideHitResponse::Accept
        },
    );
    let mut slid = out.position;
    // Snap onto the surface so we follow downhill slopes + steps down — the client's step-vs-fall
    // election (`0x6367b0`, wow-re `step-vs-fall-election.md`): the probe reaches
    // [`STEP_SLOPE_RATIO`]·travel + [`STEP_SNAP_SLACK`], and snaps only onto a *walkable* floor
    // (≤50°, the election's own `cos50°` = [`GROUND_COS`]).
    //
    // **The reach is the cone's own slope, and nothing else, on an ordinary walking frame**
    // (decision 1129). It used to carry a flat `+`[`CAPSULE_HEIGHT`] — 2.028 yd of unconditional
    // extra depth — on the reading that `0x617430` returned a collision height. Both halves of that
    // were wrong (decision 1125, wow-re `mover-collision-scalars.md` + `step-off-recourse.md`):
    // `0x617430` is `[unit+0xb8]`, the dimensionless scale ratio `max(SCALE_X / CreatureModelScale,
    // 1)`, so `H` is **1.0** for a player and not 2.028; and the reference adds it only while
    // `0x4000000` is set — "the current support is a certified STEEP contact, not a walkable
    // floor", which is [`Support::steep`]. So the deep reach belongs to the step-down *recourse*,
    // where a body already following a steep face down needs to see the walkable ground waiting at
    // its foot; the ordinary walking frame reaches exactly as far as the foot cone could rest.
    //
    // What that costs is real and intended: a drop deeper than the cone is NOT absorbed. No snap,
    // the next frame's ground probe misses, and the gap becomes a fall (the client's
    // `StartFalling(0)` election) — a short ledge drop reads as a quick, continuous, steep descent,
    // which is what the director's eye confirmed against the reference (decision 0190; 0189's
    // instant absorbed step read as a teleport and was reverted). At 2.028 yd we were absorbing
    // nearly two body-heights of ledge in one frame, which is the same teleport by another route.
    //
    // Standing still the reach collapses to the slack alone, so an idle body is re-grounded only
    // when it is genuinely resting on the floor; a body left floating above one falls the float and
    // lands, rather than being pulled down through up to two yards of air.
    //
    // [`Support::offset`] is HOVER (decision 0866): the reference's WALK resolver `0x6367b0` adds
    // `[0x7ff9d8]` = 1.0 to this same surface offset while `MOVEFLAG_HOVER` is set, and widens the
    // step-down reach by the same yard (`0x633e35`) so the float still follows the ground down.
    // Both halves are here: the reach grows by the offset, and the snap stops that far short of the
    // floor. Zero for everyone not hovering, which is the ordinary case and unchanged.
    let d = slid - start;
    let reach = d.x.hypot(d.z) * STEP_SLOPE_RATIO
        + STEP_SNAP_SLACK
        + if support.steep || popped.is_some() {
            STEP_UP_HEIGHT
        } else {
            0.0
        }
        + surface_offset;
    let hit = cast(slid, Vec3::NEG_Y * reach);
    let snap = Some((reach, hit.as_ref().map(|h| (h.distance, h.normal1.y))));
    let mut ground = None;
    let mut steep_support = false;
    // **A ride's height is earned; the snap follows ground *down*, it never undoes a climb.**
    //
    // This is where our capsule and the reference's cone part company, and it has to be said out
    // loud or the ride only works on bevels. Mid-ride the real client's foot cone is *resting on
    // the very edge it is climbing* — its skirt is in contact, so its down-probe is blocked at zero
    // and there is nothing to settle to. A capsule against a **vertical** riser touches it side-on
    // and hangs clear of everything below, so the same down-probe sees the floor it is climbing
    // away from, a full ride's height beneath it, and yanks it straight back down — every frame,
    // forever. Booty Bay's 0.40 yd step is exactly that: certified `COMMIT dy=+0.439` on every
    // frame, and stalled at +0.020.
    //
    // So while a ride is in progress the only floor that may end it is one **higher than the ride
    // began on** — the tread it is certified to be climbing to. A floor at or below the start is
    // the ground being left behind, and it gets no say.
    // The height this frame's ride earned, read before the snap can spend any of it. (The two snap
    // arms below are mutually exclusive — walkable or steep — so one value serves both.)
    let ride_rise = (slid.y - start.y).max(0.0);
    // **The step-down follows the surface; it does not leave it** (decision 1127). The reference's
    // finalize writes `pos.z -= min(clearance, d_h·1.8493990)` *before* it classifies anything, and a
    // steep landing is answered by the multipass rather than by a fall — a `ret 2` continues,
    // grounded, with the "my support is a certified steep contact" bit set (wow-re
    // `step-off-recourse.md`; the bit's meaning is decision 1125's).
    //
    // We refused every hit under 50° outright, so walking off a kerb the probe found only the ~61°
    // riser, declined it, and the body kept its full height while its forward speed carried it out
    // past the edge — then gravity took over. The director's capture is unambiguous: one frame of
    // `dy=+0.000 dx=0.118` with the surface 0.162 below, then eighteen airborne frames landing 2.1 yd
    // downrange. That is the dive.
    //
    // **The reach is how far we can SEE; the descent is how far we may GO — and for a capsule those
    // are not the same number** (decision 1132). Byte-exact the finalize is one line
    // (`0x636e45`–`0x636e52`, wow-re `step-off-recourse.md` §1): sweep down by `L`, write
    // `pos.z -= achieved`, *then* classify. `achieved` is the sweep's own output, so on the reference
    // the descent is bounded only by `L` — no second, smaller cap exists anywhere on that path.
    //
    // 1129 read that literally and dropped 1127's separate cone cap. The director's eye caught what
    // the reading missed: *"the pop down seems too instant while the ref seems more smoothed … the
    // ref is def not diving forward of the fence, it's still stepping down."*
    //
    // **The reason is the shape, and it is 1123's divergence on the down side.** The reference's
    // mover is a cone at the foot, so stepping off a fence its **skirt stays in contact with the
    // edge** the whole way down — its `achieved` is never large, because something is always just
    // beneath it. The descent comes out at the cone's own slope, `d_h·1.8494` a frame, without any
    // cap being needed: the geometry is the cap. Our capsule touches the edge side-on and then hangs
    // clear, so the identical instruction sees the entire remaining drop and spends it at once. Same
    // law, different body, opposite look.
    //
    // So we port the cone's *effect*, which is what 1123 said we would do for the solid we do not
    // have: **one cone's worth of descent per frame.** The deeper reach stays exactly as 1129 left
    // it — it is what lets us see the ground and stay grounded on a face steeper than the cone,
    // which is what keeps the dive gone. Seeing further and falling further are different questions,
    // and only the second one is the director's "instant".
    let cone_reach = d.x.hypot(d.z) * STEP_SLOPE_RATIO + STEP_SNAP_SLACK;
    if let Some(h) = hit {
        let drop = (h.distance - surface_offset).max(0.0);
        let walkable = h.normal1.y >= GROUND_COS;
        // A ride's height is earned (above): only a floor higher than the ride began on may end it.
        if !(rode && walkable && drop >= ride_rise) {
            slid.y -= drop.min(cone_reach);
            if drop > cone_reach {
                // **Further down than the cone could rest, but in sight.** This is a step-down still
                // in progress — the reference's `0x4000000` case exactly — so we stay grounded and
                // keep the deep reach open, and the next frames finish it a cone's worth at a time.
                // Without this the body would be left hanging and the fall would elect, which is the
                // dive coming back through the cap.
                steep_support = true;
            } else if walkable {
                ground = Some(h.entity);
            } else {
                // **Support is earned by descending, not by touching.** Resting *on* a steep face
                // gives a clearance of ~0, and standing still the bound is the slack alone — so a
                // "within reach" test alone would perch a motionless body on a 60° bank forever
                // instead of letting it slide off (1121's named hazard, and worse than the dive).
                // Real downward progress states the actual condition: descending, not resting.
                steep_support = drop > STEP_SNAP_SLACK;
            }
        }
    } else if !rode {
        // **Nothing in sight: descend, then let the fall elect** — the same `pos.z -= achieved` runs
        // on the no-hit leg with `achieved == L` ("no committed record: `pos.z` has ALREADY dropped
        // by the full `L`"), so the frame that leaves a ledge starts its fall lower instead of flat.
        // A ride is exempt: mid-ride the body legitimately hangs clear of everything below, and this
        // would spend the climb.
        //
        // **And it takes the same cone cap as the hit legs, for the same reason.** Left uncapped it
        // was harmless only while the reach was a cone's worth anyway — once the steep-support bit
        // opens the deep reach, "descend the full `L`" is a 1.25 yd drop in one frame. That is what
        // the director was still feeling after 1132 capped the other two legs: their fence
        // step-downs came through *this* branch, `snap miss (reach 1.25) dy=-1.250`, five of them in
        // one capture. The cap is a property of our body, not of which leg found the floor.
        slid.y -= (reach - surface_offset).max(0.0).min(cone_reach);
    }
    GroundedStep {
        center: slid,
        // A ride is a climb too, and so is a pop — the trace reads the same whichever regime
        // moved the body, and all three are measured from the frame's entry height.
        climb: (rode || popped.is_some()).then_some(slid.y - center.y),
        // **Supported by a certified steep contact** — the reference's `0x4000000`, whose meaning
        // decision 1125 settled. Two ways to earn it and one meaning: mid-ride the body is held up
        // by the edge under its skirt (climbing), and mid-step-down it is resting on the surface it
        // is following off a ledge (descending). Either way the frame-start ground probe looks
        // straight down and sees only a steep face, so without this the frame reads as airborne and
        // gravity takes the body off the surface — the climb undone on the way up, the dive on the
        // way down. A walkable floor ends it: ordinary grounding takes over from there.
        steep_support: ((rode || popped.is_some()) && ground.is_none()) || steep_support,
        ground,
        snap,
    }
}

/// **One airborne step, resolved against the world** — the arc's slide and nothing else. No
/// step-up and no election snap: the arc owns its own height (gravity carries it; the landing is
/// next frame's ground probe to call), so the only thing the world may do here is *stop* it. Steep
/// faces get [`steep_contact_shear`]'s treatment, exactly as they do on the ground.
///
/// The airborne twin of [`grounded_step`], and shared for the same reason (0059's one controller,
/// every mover): the local controller ([`step`]) calls it for its held/airborne frames, and a
/// **remote** mover's ballistic dead-reckon calls it for the arc it invents between packets
/// ([`crate::net::motion::remote`]). Without that, a watched player who jumps into a building is
/// drawn *inside* it for the length of the jump and pops back out on the landing packet — the
/// airborne half of the very defect 0626 fixed on the ground (decision 0627).
pub(crate) fn airborne_step(
    world: &benilla_world::collision::WorldCollision<'_, '_>,
    capsule: &Collider,
    center: Vec3,
    velocity: Vec3,
    dt: std::time::Duration,
) -> Vec3 {
    world
        .slide_body(
            capsule,
            center,
            velocity,
            dt,
            &MoveAndSlideConfig::default(),
            |hit| {
                if let Some(followed) = steep_contact_shear(**hit.normal, *hit.velocity) {
                    *hit.velocity = followed;
                }
                MoveAndSlideHitResponse::Accept
            },
        )
        .position
}

/// **The overhang line is a tolerance, not zero** — and it has to be, or half the world's vertical
/// walls behave differently from the other half for no physical reason.
///
/// Every "is this face steep-but-not-overhanging" test here is a range whose floor used to be `0.0`.
/// An axis-aligned authored quad — a doorstep, a crate, a dock plank, most of Booty Bay — computes a
/// normal whose `y` is `±0` decided by nothing but which way the cross product's rounding fell. The
/// director's capture caught both halves of that coin on the *same* step: `n.y = −4.08e-5` refused to
/// certify and refused to ride, where `+0.009` on the same geometry did both. A real overhang is a
/// ceiling, nowhere near this line, so widening the floor by a ten-thousandth costs nothing and
/// makes a vertical wall a vertical wall whichever way the rounding goes.
const OVERHANG_EPS: f32 = -1.0e-4;

/// A face the mover meets side-on: too steep to stand on, not a ceiling. The shared gate behind the
/// step-up's certification, the foot-cone ride and the wall flatten — one line, so the three can
/// never disagree about what a vertical wall is (see [`OVERHANG_EPS`]).
fn is_steep_face(ny: f32) -> bool {
    (OVERHANG_EPS..GROUND_COS).contains(&ny)
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
/// Steep faces stay with [`steep_contact_shear`], and any height the ride manufactures is
/// bounded by the end-of-frame snap, which only ever settles onto a walkable floor.
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
    if !is_steep_face(n.y) {
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

/// The steep-contact response: **the descent is never touched, and the push-out is horizontal.**
///
/// VERIFIED against the reference (wow-re `system/collision/scratch/fall-steep-response.md`, the
/// dispatch this decided): `0x635090` writes **exactly two floats** — an x/y push-out along the
/// horizontal projection of the contact normal — and `0x635600` adds them to `Δx`/`Δy` only, while
/// `Δz = dir.z · remaining` passes through **with no addend at all**. The normal's vertical is
/// annihilated in the response's own denominator (`0x635166 fmul [0x7ffd74]`, and that constant is
/// `0.0`). So the residual is placed **exactly in the contact plane** (`Δ'·A = 0`) but reaches it by
/// moving *only horizontally*: the **vertical-shear** projection onto the plane, not the orthogonal
/// one every collide-and-slide library ships.
///
/// ```text
/// v' = v − (v·n)/|n_h|² · n_h            n_h = (n.x, 0, n.z)
/// ```
///
/// Decomposing an approach as `a` into the face, `b` across it and `c` descending, against a face of
/// pitch θ:
///
/// | component | what survives |
/// |---|---|
/// | downhill | `c · cotθ` — exactly what following the surface requires |
/// | across   | `b` — preserved in full, never scaled |
/// | vertical | `−c` — **untouched** |
///
/// Three consequences, none of them a tuning choice:
///
/// - **No geometry can add upward motion, at any steepness or approach angle.** The predecessor
///   rules here reconstructed that invariant twice — first by testing the *sign* of the clipped
///   vertical (0970), then by stripping the into-face push before an orthogonal clip (1135). The
///   reference gets it structurally, because the vertical is not an output.
/// - **A fall against a steep face descends at the full free-fall rate**, so along-surface speed is
///   `V/sinθ` — *faster* than free fall. This is the reference's slippery steep-slope slide. An
///   orthogonal clip descends at `V·sin²θ` instead: the same path down the face, walked 1.2–1.7×
///   slower, which is that much longer in contact and so that much further across (the director's
///   *"we slide across it until we land … further than the ref"*, worst just past 50° — exactly
///   where players engage slopes).
/// - **How hard you press into the face leaves no trace in the residual** — the push-out cancels
///   `a` exactly. It survives only through *where* the sweep stopped.
///
/// A **grounded** frame is unchanged by all of this: its velocity is horizontal, `c = 0`, and the
/// formula collapses to "remove the into-face component" — which is what walking into a bank always
/// did here. Walkable faces ride ([`walkable_ride_velocity`]) and overhangs are ceilings; neither
/// reaches this. The reference's degeneracy guards (`|n_h|` under `2⁻²²`) cannot fire for us:
/// [`is_steep_face`] bounds `|n_h|` below by `sin 50°`.
fn steep_contact_shear(n: Vec3, v: Vec3) -> Option<Vec3> {
    if !is_steep_face(n.y) {
        return None;
    }
    let vn = v.dot(n);
    // A contact the motion is leaving is not this response's business.
    if vn >= 0.0 {
        return None;
    }
    let h = Vec3::new(n.x, 0.0, n.z);
    Some(v - (vn / h.length_squared()) * h)
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
    /// Certified: the obstacle can be cleared. What the frame then *does* is
    /// [`grounded_step`]'s — a foot-cone ride or a pop of `up`, never this probe's own advance
    /// (decision 1130). `landed` is where the probe's settle found floor, kept for the trace: it is
    /// a **diagnostic**, one full [`STEP_UP_ADVANCE`] downrange, and committing it was the teleport.
    Commit {
        landed: Vec3,
        up: f32,
        fwd: f32,
        dy: f32,
    },
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
    if n.y >= GROUND_COS || n.y < OVERHANG_EPS || n.dot(dir_h) >= 0.0 {
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
        world_from_profile(&PROFILE)
    }

    /// The fixture builder behind every profiled world here: a `(x, y)` polyline extruded across
    /// `z`, wound so every face's **authored** normal points up and back at the approaching body.
    fn world_from_profile(profile: &[(f32, f32)]) -> App {
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
        for w in profile.windows(2) {
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
            .run_system_once(move |world: benilla_world::collision::WorldCollision| {
                let capsule = player_capsule();
                let cast =
                    |from: Vec3, disp: Vec3| world.cast_body(&capsule, from, disp, SKIN_WIDTH);
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
    fn walk_kerb(start_y: f32, frames: usize) -> Vec<Row> {
        walk_profile(world_with_kerb(), start_y, frames)
    }

    /// One frame of a walked fixture: `(feet height, climb, steep support, walkable ground)`.
    type Row = (f32, Option<f32>, bool, bool);

    /// Walk the capsule into `world` and run `frames` consecutive **whole** grounded steps from
    /// wherever the last one left it — the mover's own loop, so what these assert is the behaviour
    /// on screen and not a single probe in isolation. Rows are `(feet height, climb, ride latch)`.
    fn walk_profile(world: App, start_y: f32, frames: usize) -> Vec<Row> {
        walk_from(world, Vec3::new(-1.0, start_y, 0.0), Vec3::X, frames)
    }

    /// The general walker: place the **feet** at `start`, push in `dir`, and run `frames` whole
    /// grounded steps from wherever the last one left the body.
    fn walk_from(mut world: App, start: Vec3, dir: Vec3, frames: usize) -> Vec<Row> {
        world
            .world_mut()
            .run_system_once(move |world: benilla_world::collision::WorldCollision| {
                let capsule = player_capsule();
                let cast =
                    |from: Vec3, disp: Vec3| world.cast_body(&capsule, from, disp, SKIN_WIDTH);
                let start = start + Vec3::Y * (CAPSULE_HEIGHT * 0.5);
                let run = cast(start, dir).map_or(1.0, |h| h.distance);
                let mut center = start + dir * run;
                let dt = std::time::Duration::from_secs_f32(TRAVEL_60FPS / 7.0);
                // The steep-support bit is carried frame to frame exactly as the driver carries it
                // (`Player::steep_support`), so the deep step-down reach is gated here the way it is
                // on screen — a fixture that reset it every frame would test a mover we do not ship.
                let mut support = Support::default();
                (0..frames)
                    .map(|_| {
                        let g = grounded_step(&world, &capsule, center, dir * 7.0, dt, support);
                        center = g.center;
                        support.steep = g.steep_support;
                        (
                            center.y - CAPSULE_HEIGHT * 0.5,
                            g.climb,
                            g.steep_support,
                            g.ground.is_some(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap()
    }

    /// **Booty Bay's step** — a 0.40 yd *vertical* riser onto a ~22° ramp, profiled from the
    /// director's steered capture at WoW `(-14314.3, 466.2, 18.5)`, collider `54046v0`: the wall
    /// probe reads `n=(-0.96,+0.00,-0.27)` with the blocking edge `h=+0.40` above the feet, and the
    /// surface ahead runs `+0.35:+0.40/+0.93` … `+1.40:+0.82/+0.93`.
    ///
    /// The riser is tilted by 0.002 yd across its 0.40 of rise for one reason: a *perfectly* axis-
    /// aligned quad computes a normal whose `y` is ±0 on a float coin flip, and this fixture must
    /// pin the ride, not that coin flip. `an_exactly_vertical_riser_still_certifies` owns the flip.
    fn world_with_vertical_step() -> App {
        const PROFILE: [(f32, f32); 4] = [(-2.0, 0.0), (0.30, 0.0), (0.302, 0.40), (3.0, 1.48)];
        world_from_profile(&PROFILE)
    }

    #[test]
    fn a_vertical_step_is_climbed_not_stalled() {
        // The 1123 regression the capture caught. A 0.40 yd rise is inside [`FOOT_CONE_HEIGHT`], so
        // it rides — and against a *vertical* riser a capsule hangs clear of everything below, so
        // the election snap saw the street a full ride beneath it and pulled the body back down
        // every frame. Certified `COMMIT dy=+0.439` on every one, and stalled at +0.020 forever.
        let frames = walk_profile(world_with_vertical_step(), 0.0, 6);
        let top = frames.last().unwrap().0;
        assert!(
            top > 0.40,
            "the body must finish on the step, not stalled below it: {frames:?}"
        );
        // Monotone: a ride that is undone and re-earned reads as a judder, which is what the
        // director would see even if the body eventually arrived.
        for w in frames.windows(2) {
            assert!(
                w[1].0 >= w[0].0 - 1.0e-3,
                "the climb must never go backwards: {frames:?}"
            );
        }
        assert!(
            frames[0].0 < 0.40,
            "…and it is still a ride, not a one-frame pop: {frames:?}"
        );
    }

    #[test]
    fn a_tall_step_behind_an_unwalkable_face_is_climbed() {
        // Stormwind, WoW `(-8427.4, 607.4, 95.1)`, collider `14282v1` — the director's second
        // capture. A 0.91 yd step whose face is a 66° bevel for most of a yard before the flat top
        // begins, so it defeated **both** of the old budgets at once: the top is above 0209's 0.7
        // rise ceiling, and 1121's one-radius advance put the settle probe still over the bevel,
        // where the walkable gate correctly refused it. The captured ladder is unanimous —
        // `STEEP fwd0.31 ny+0.41` at every rung out to 1.20.
        const P: [(f32, f32); 4] = [(-2.0, -0.02), (0.28, -0.02), (0.694, 0.91), (3.0, 0.91)];
        let frames = walk_profile(world_from_profile(&P), -0.02, 8);
        assert!(
            frames.last().unwrap().0 > 0.88,
            "the body must finish on the 0.91 yd top: {frames:?}"
        );
        for w in frames.windows(2) {
            assert!(
                w[1].0 >= w[0].0 - 1.0e-3,
                "the climb must never go backwards: {frames:?}"
            );
        }
        // The blocking edge is low on the bevel, inside the cone, so this rides — six frames of
        // smooth diagonal rather than a yard-high teleport.
        assert!(
            frames[0].0 < 0.3,
            "a 0.91 yd gain in one frame is the teleport 1126 exists to avoid: {frames:?}"
        );
    }

    #[test]
    fn stepping_off_a_kerb_follows_the_surface_down() {
        // Decision 1127, and the exact frame the director's capture caught: walking *off* the same
        // Stormwind kerb the down-probe finds only its ~61° riser, and refusing that outright left
        // the body at full height while its forward speed carried it past the edge — one frame of
        // `dy=+0.000 dx=0.118` with the surface 0.162 below, then eighteen airborne frames landing
        // 2.1 yd downrange. The dive.
        //
        // **Support** is the thing to assert, because losing it *is* the dive: the caller turns an
        // unsupported frame into gravity, and everything after that is an arc no snap can undo.
        let frames = walk_from(world_with_kerb(), Vec3::new(1.6, 0.28, 0.0), Vec3::NEG_X, 8);
        for (i, f) in frames.iter().enumerate() {
            assert!(
                f.2 || f.3,
                "frame {i} left the surface entirely — that is the dive: {frames:?}"
            );
        }
        assert!(
            frames.last().unwrap().0 < 0.05,
            "the walk-off must reach the street: {frames:?}"
        );
    }

    #[test]
    fn a_face_steeper_than_the_cone_still_descends_what_it_can() {
        // The "only sometimes" case. A face steeper than the cone cannot be followed exactly, and the
        // question is what the mover does with the part it *can* follow. The reference descends
        // `min(clearance, cap)` unconditionally and only then decides it is falling; the first cut of
        // 1127 descended the clearance **or nothing**, so a surface a hair steeper than the cone
        // dived — eight takeoffs in one capture, each missing the bound by 0.001–0.019.
        //
        // 66° face (`ny ≈ 0.41`, the captured value), walked off at a run: the first frame past the
        // lip must give up most of a cone's worth of height, not hold its own.
        const P: [(f32, f32); 4] = [(-2.0, 0.0), (0.60, 0.0), (1.0, -0.91), (3.0, -0.91)];
        // (`walk_from`'s approach cast falls back to a yard when nothing blocks — there is no wall
        // to stop against here — so the start is placed a yard short of the lip on purpose.)
        let frames = walk_from(
            world_from_profile(&P),
            Vec3::new(-0.6, 0.0, 0.0),
            Vec3::X,
            5,
        );
        let cone = TRAVEL_60FPS * STEP_SLOPE_RATIO;
        let biggest = frames
            .windows(2)
            .map(|w| w[0].0 - w[1].0)
            .fold(frames[0].0.abs(), f32::max);
        assert!(
            biggest > cone * 0.5,
            "some frame past the lip must follow the face down rather than hold height: \
             {frames:?} (best drop {biggest:.3}, a cone's worth is {cone:.3})"
        );
    }

    #[test]
    fn a_ledge_deeper_than_the_probe_is_a_fall_not_an_absorbed_step() {
        // **The pin on decision 1129's reach.** A 1.2 yd sheer drop — deeper than the step-down probe
        // can see, shallower than the 2.028 yd of flat extra depth the probe used to carry. Under the
        // old reach the snap found the lower floor from the top of the ledge and absorbed the whole
        // drop in ONE frame, still walkably grounded the entire way: a downward teleport, and the
        // step-up teleport's exact mirror.
        //
        // What must happen instead is what the reference does: the body leaves the ground before it
        // reaches the bottom, and the gap is a fall. So — **an airborne frame must come first.**
        // (This test fails on the pre-1129 reach, which is the only reason it is worth having.)
        const P: [(f32, f32); 4] = [(-2.0, 0.0), (0.60, 0.0), (0.62, -1.20), (3.0, -1.20)];
        let frames = walk_from(
            world_from_profile(&P),
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::X,
            12,
        );
        // The measurable form: **no single frame may give up an unbounded amount of height.** A
        // frame's descent is the slide's own drop plus the step-down snap's, each bounded by what
        // the foot cone could rest on over one frame's travel — so two cone-reaches is a generous
        // ceiling. The corrected mover clears it easily (its worst frame here gives up 0.257); the
        // old reach blows straight through, holding height to the lip and then taking the remaining
        // **1.126 yd in a single frame**, walkably grounded at both ends of it.
        let bound = 2.0 * (TRAVEL_60FPS * STEP_SLOPE_RATIO + STEP_SNAP_SLACK);
        let worst = frames
            .windows(2)
            .map(|w| w[0].0 - w[1].0)
            .fold(0.0_f32, f32::max);
        assert!(
            worst <= bound,
            "no frame may drop more than {bound:.3} yd; worst was {worst:.3}\n{frames:#?}"
        );
    }

    #[test]
    fn an_idle_body_is_not_pulled_down_through_open_air() {
        // The other half of the same reach. A motionless body a yard above flat ground: standing
        // still the probe is the slack and nothing more, so it must find nothing, keep its height,
        // and be handed to gravity. The old reach was `slack + 2.028` even at a dead stop — it found
        // the floor a yard down and yanked the body onto it, one frame, no fall.
        const P: [(f32, f32); 2] = [(-2.0, 0.0), (3.0, 0.0)];
        let (dy, ground) = world_from_profile(&P)
            .world_mut()
            .run_system_once(move |world: benilla_world::collision::WorldCollision| {
                let capsule = player_capsule();
                let dt = std::time::Duration::from_secs_f32(TRAVEL_60FPS / 7.0);
                let center = Vec3::new(0.0, CAPSULE_HEIGHT * 0.5 + 1.0, 0.0);
                let g = grounded_step(&world, &capsule, center, Vec3::ZERO, dt, Support::default());
                (g.center.y - center.y, g.ground.is_some())
            })
            .unwrap();
        assert!(!ground, "a body a yard up is standing on nothing");
        assert!(
            dy.abs() <= STEP_SNAP_SLACK * 2.0,
            "it must keep its height and fall, not be snapped a yard down: dy={dy:+.3}"
        );
    }

    #[test]
    fn a_step_up_never_outruns_the_frame_it_happens_in() {
        // **The pin on decision 1130.** The reference holds `L / t_remaining` invariant across every
        // hit, deflection and miss in the walk resolver, so a frame's total horizontal displacement
        // is bounded by its own substep budget — `dx = 1.19` where a walking frame is 0.117 "cannot
        // arise from this path" (wow-re `ret2-commit-law.md`, claim 2). 0209 committed the probe's
        // full advance as motion, which put exactly that lurch in every single step-up.
        //
        // **A Goldshire table, profiled from the director's own capture** — the shape they reported
        // as teleporty. The capture's pop reads `step hit h=+0.73 n=(-0.04,+0.00,+1.00) up=1.00
        // fwd=1.19 dy=+0.737 -> COMMIT`, and the frame it produced was `dy=+0.737 dx=1.191`.
        //
        // The `h=+0.73` is the whole point and it is why this fixture is a **table and not a step**:
        // against a capsule a plain vertical riser reports its contact at the hemisphere centre,
        // ≈0.33 up, which is inside the foot cone and rides. Only a face whose *lowest* collidable
        // part is already above the band contacts high — a top slab overhanging recessed legs, which
        // is exactly what a tavern table is. Before this test **no fixture in the suite reached the
        // pop regime at all**, which is how 0209's teleport survived every green run for a year.
        //
        //      0.737 ┌──────────────  table top
        //      0.62  ├──┐             slab front face  ← the contact, above the cone (0.617)
        //            ┆  └──┐          underside
        //      0.0   ┆     │────────  leg, recessed by more than a body radius
        //           0.60  0.95
        const P: [(f32, f32); 6] = [
            (-2.0, 0.0),
            (0.95, 0.0),
            (0.95, 0.62),
            (0.60, 0.62),
            (0.60, 0.737),
            (3.0, 0.737),
        ];
        let track = world_from_profile(&P)
            .world_mut()
            .run_system_once(move |world: benilla_world::collision::WorldCollision| {
                let capsule = player_capsule();
                let dt = std::time::Duration::from_secs_f32(TRAVEL_60FPS / 7.0);
                let mut center = Vec3::new(-1.0, CAPSULE_HEIGHT * 0.5, 0.0);
                let mut support = Support::default();
                let mut rows = Vec::new();
                for _ in 0..24 {
                    let g = grounded_step(&world, &capsule, center, Vec3::X * 7.0, dt, support);
                    rows.push((g.center.x - center.x, g.center.y - CAPSULE_HEIGHT * 0.5));
                    center = g.center;
                    support.steep = g.steep_support;
                }
                rows
            })
            .unwrap();
        let worst = track.iter().map(|r| r.0).fold(0.0_f32, f32::max);
        assert!(
            worst <= TRAVEL_60FPS + 1.0e-3,
            "no frame may travel further than the {TRAVEL_60FPS:.3} yd it asked to walk; worst \
             was {worst:.3}\n{track:#?}"
        );
        let top = track.last().expect("frames").1;
        assert!(
            top > 0.70,
            "and it still has to get onto the 0.737 yd table: ended at {top:.3}\n{track:#?}"
        );
    }

    #[test]
    fn stepping_off_a_fence_hugs_the_edge_down_instead_of_dropping_at_once() {
        // **The pin on decision 1132**, and the director's report: *"the pop down seems too instant
        // while the ref seems more smoothed … the ref is def not diving forward of the fence, it's
        // still stepping down."*
        //
        // A 0.72 yd fence with a ~53° chamfered lip — the geometry their Goldshire capture measured
        // (`snap ny=0.628 STEEP` on the lip frame, then the drop). The reference's foot cone keeps
        // its skirt on that edge the whole way down and descends at the cone's own slope; our capsule
        // hangs clear and, uncapped, spends the entire remaining drop in one frame (0.644 in the
        // capture, `reach 1.24`).
        //
        // Two things must hold together, and only together: **no frame gives up more than a cone's
        // worth**, and **the body never leaves the ground on the way down** — a cap alone would just
        // be the dive again, one frame later.
        const P: [(f32, f32); 5] = [
            (-2.0, 0.72),
            (0.60, 0.72),
            (0.69, 0.60),
            (0.692, 0.0),
            (3.0, 0.0),
        ];
        let frames = walk_from(
            world_from_profile(&P),
            Vec3::new(-1.0, 0.72, 0.0),
            Vec3::X,
            14,
        );
        let cone = TRAVEL_60FPS * STEP_SLOPE_RATIO + STEP_SNAP_SLACK;
        let worst = frames
            .windows(2)
            .map(|w| w[0].0 - w[1].0)
            .fold(0.0_f32, f32::max);
        assert!(
            worst <= cone + 1.0e-3,
            "the step-down must take at most a cone's worth ({cone:.3}) a frame; worst was \
             {worst:.3}\n{frames:#?}"
        );
        let air = frames
            .iter()
            .position(|&(_, _, steep, walk)| !steep && !walk);
        assert!(
            air.is_none(),
            "and it must stay on the surface the whole way down, not fall the rest: first \
             unsupported frame {air:?}\n{frames:#?}"
        );
        let bottom = frames.last().expect("frames").0;
        assert!(
            bottom < 0.05,
            "and it does have to get all the way down: ended at {bottom:.3}\n{frames:#?}"
        );
    }

    #[test]
    fn a_frame_that_finds_nothing_still_spends_only_one_cone() {
        // **The branch 1132's first cut missed.** When the probe finds nothing the reference still
        // writes `pos.z -= achieved` with `achieved == L` — harmless while `L` is a cone's worth, and
        // a **1.25 yd teleport** once the steep-support bit opens the deep reach. That is what the
        // director was still feeling after the two hit legs were capped: their fence step-downs came
        // through here, `snap miss (reach 1.25) dy=-1.250`, five of them in one capture.
        //
        // Stated at the branch rather than through a fixture, because reproducing the *miss* needs
        // the body clear of the wall it just left, and a profiled fence puts its riser directly under
        // the capsule (it hugs that riser down instead — which is `stepping_off_a_fence_hugs_the_edge
        // _down…`, the case that does have geometry). Here: carrying the bit, moving, with the floor
        // beyond even the deep reach, so the probe cannot hit anything.
        const P: [(f32, f32); 2] = [(-2.0, 0.0), (3.0, 0.0)];
        let dy = world_from_profile(&P)
            .world_mut()
            .run_system_once(move |world: benilla_world::collision::WorldCollision| {
                let capsule = player_capsule();
                let dt = std::time::Duration::from_secs_f32(TRAVEL_60FPS / 7.0);
                let center = Vec3::new(0.0, CAPSULE_HEIGHT * 0.5 + 1.6, 0.0);
                let g = grounded_step(
                    &world,
                    &capsule,
                    center,
                    Vec3::X * 7.0,
                    dt,
                    Support {
                        offset: 0.0,
                        steep: true,
                    },
                );
                g.center.y - center.y
            })
            .unwrap();
        let cone = TRAVEL_60FPS * STEP_SLOPE_RATIO + STEP_SNAP_SLACK;
        assert!(
            -dy <= cone + 1.0e-3,
            "a frame that found nothing may still spend only a cone's worth ({cone:.3}); it spent \
             {:.3}",
            -dy
        );
    }

    #[test]
    fn a_motionless_body_is_never_perched_on_a_steep_slope() {
        // A 60° bank. Walk onto it, then STOP. Standing still the cone bound collapses to the slack,
        // so does the steep support quietly keep a motionless body perched on it?
        const P: [(f32, f32); 3] = [(-2.0, 0.0), (0.0, 0.0), (3.0, -5.196)];
        let out = world_from_profile(&P)
            .world_mut()
            .run_system_once(move |world: benilla_world::collision::WorldCollision| {
                let capsule = player_capsule();
                let dt = std::time::Duration::from_secs_f32(TRAVEL_60FPS / 7.0);
                let mut center = Vec3::new(0.6, CAPSULE_HEIGHT * 0.5 - 1.039, 0.0);
                let mut rows = Vec::new();
                let mut support = Support::default();
                for i in 0..4 {
                    // Two frames walking, then two standing perfectly still.
                    let v = if i < 2 { Vec3::X * 7.0 } else { Vec3::ZERO };
                    let g = grounded_step(&world, &capsule, center, v, dt, support);
                    center = g.center;
                    support.steep = g.steep_support;
                    rows.push((v.length() > 0.0, g.steep_support));
                }
                rows
            })
            .unwrap();
        for (moving, supported) in out {
            assert!(
                moving || !supported,
                "a motionless body must never hold a steep face — it slides off"
            );
        }
    }

    #[test]
    fn an_exactly_vertical_riser_still_certifies() {
        // The other half of the same capture. An axis-aligned riser's normal has `y = ±0` depending
        // on nothing but the cross product's rounding, and an overhang guard written as `n.y < 0.0`
        // refuses the negative half — reading a plain vertical step as a ceiling. Half the world's
        // risers, decided by a float sign.
        const PROFILE: [(f32, f32); 4] = [(-2.0, 0.0), (0.30, 0.0), (0.30, 0.40), (3.0, 1.48)];
        let frames = walk_profile(world_from_profile(&PROFILE), 0.0, 6);
        assert!(
            frames.last().unwrap().0 > 0.40,
            "an exactly-vertical riser must climb exactly like a tilted one: {frames:?}"
        );
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
            .position(|&(y, _, _, _)| (y - 0.28).abs() < 0.03)
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
        for (y, climb, riding, _) in walk_kerb(-2.0, 4) {
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
            .run_system_once(move |world: benilla_world::collision::WorldCollision| {
                let capsule = player_capsule();
                let cast =
                    |from: Vec3, disp: Vec3| world.cast_body(&capsule, from, disp, SKIN_WIDTH);
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
            !matches!(v, StepVerdict::Commit { .. }),
            "a face above the rise ceiling must never certify, got {v}"
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
        assert!(steep_contact_shear(face(50.1), v).is_some());
    }

    #[test]
    fn walking_into_a_steep_face_spends_the_push_along_it() {
        // The grounded case, unchanged in outcome by the strip: a run straight at a 60° face
        // keeps nothing pointing into it, and manufactures no vertical of its own.
        let v = Vec3::new(7.0, 0.0, 0.0);
        let out = steep_contact_shear(face(60.0), v).expect("must strip");
        assert!(out.x.abs() < 1e-6 && out.y == 0.0);
    }

    #[test]
    fn a_fall_against_a_face_keeps_the_whole_of_its_descent() {
        // The ratchet's engine, and the slide's, in one place. Falling at 4.9 yd/s into a 55° face
        // with a run held into it, an orthogonal clip leaves the body descending — so 0970's
        // `v'.y > 0` flatten declined — at ~1% of what gravity intended, which the wedge rest reads
        // as a landing. The reference never touches the vertical at all.
        let (n, v) = (face(55.0), Vec3::new(7.0, -4.9, 0.0));
        let orthogonal = v - v.dot(n) * n;
        assert!(
            orthogonal.y > -0.2,
            "the orthogonal clip must be the near-cancel it was: {:.3}",
            orthogonal.y
        );
        let out = steep_contact_shear(n, v).expect("a steep contact responds");
        assert!(
            (out.y - v.y).abs() < 1.0e-4,
            "the descent passes through untouched: {:.3} vs {:.3}",
            out.y,
            v.y
        );
        // …and the horizontal is exactly what following the surface costs: `cotθ` per unit of drop,
        // downhill. The push held into the face leaves no trace (`a` cancels exactly).
        let cot = 1.0 / 55.0_f32.to_radians().tan();
        assert!(
            (out.x - v.y * cot).abs() < 1.0e-3,
            "downhill drift must be cot55° = {cot:.3} per unit of drop: {:.3}",
            out.x
        );
        // The residual lies in the plane, so nothing further is clipped off it.
        assert!(out.dot(n).abs() < 1.0e-4, "Δ'·A = 0: {:.5}", out.dot(n));
    }

    #[test]
    fn a_plumb_fall_is_carried_down_the_face_at_full_speed() {
        // A body dropping straight onto a steep face has no push to remove — and is still the
        // response's business, because following the surface *is* horizontal motion. It keeps every
        // bit of its 10 yd/s and gains the surface's own drift. (Under 1135's strip this returned
        // `None`, and the orthogonal clip then slowed the descent instead — the same error seen
        // from the other side.)
        let out = steep_contact_shear(face(60.0), Vec3::new(0.0, -10.0, 0.0))
            .expect("a plumb fall still follows the surface");
        assert!(
            (out.y + 10.0).abs() < 1.0e-4,
            "full free-fall rate: {:.3}",
            out.y
        );
        let cot = 1.0 / 60.0_f32.to_radians().tan();
        assert!(
            (out.x + 10.0 * cot).abs() < 1.0e-3,
            "downhill: {:.3}",
            out.x
        );
        // A contact the motion is leaving is not this response's: no push-out, no drift.
        assert!(steep_contact_shear(face(60.0), Vec3::new(7.0, 20.0, 0.0)).is_none());
    }

    #[test]
    fn a_rising_jump_keeps_its_own_lift() {
        // A jump rising along the face: the strip spends the push against the hill and the
        // mover's own +vy passes through untouched — the face adds nothing to it.
        let v = Vec3::new(7.0, 8.0, 0.0);
        let out = steep_contact_shear(face(60.0), v).expect("boost must strip");
        assert!((out.y - v.y).abs() < 1e-6);
        let n = face(60.0);
        // …and the true plane no longer opposes what is left, so nothing further is clipped.
        assert!(out.dot(n) >= -1e-6);
    }

    /// **The Elwynn hillside the director jump-climbed** — a 55° face (`ny = +0.574`, the normal
    /// the capture read off the real slope at WoW `(-9236.8, -341.9, 101.6)`) rising out of flat
    /// ground. Too steep to walk by a wide margin, so nothing here may ever end with the body
    /// standing part-way up it.
    fn world_with_steep_hillside() -> App {
        const P: [(f32, f32); 3] = [(-3.0, 0.0), (0.0, 0.0), (2.5, 3.570)];
        world_from_profile(&P)
    }

    /// One airborne frame's readings: `(feet height, vertical velocity, fraction of the descent
    /// gravity intended that the frame actually achieved)`.
    type AirRow = (f32, f32, f32);

    /// Jump into `world` from `start_feet` and fly the arc exactly as the mover flies it —
    /// gravity into `vel_y`, then [`airborne_step`] — holding `dir` at a run the whole way.
    /// This is the *airborne* half of the mover's loop, the half no fixture here exercised
    /// before: every other walker in this module drives [`grounded_step`].
    fn jump_into(mut world: App, start_feet: Vec3, dir: Vec3, frames: usize) -> Vec<AirRow> {
        world
            .world_mut()
            .run_system_once(move |world: benilla_world::collision::WorldCollision| {
                let capsule = player_capsule();
                let dt = std::time::Duration::from_secs_f32(TRAVEL_60FPS / 7.0);
                let secs = dt.as_secs_f32();
                let mut center = start_feet + Vec3::Y * (CAPSULE_HEIGHT * 0.5);
                let mut vel_y = JUMP_SPEED;
                (0..frames)
                    .map(|_| {
                        vel_y = (vel_y - GRAVITY * secs).max(-TERMINAL_VELOCITY);
                        let before = center.y;
                        center = airborne_step(
                            &world,
                            &capsule,
                            center,
                            dir * 7.0 + Vec3::Y * vel_y,
                            dt,
                        );
                        let intent = vel_y * secs;
                        let achieved = center.y - before;
                        // Only a descent has a stall to measure; a rise reports 1.0.
                        let frac = if intent < 0.0 { achieved / intent } else { 1.0 };
                        (center.y - CAPSULE_HEIGHT * 0.5, vel_y, frac)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap()
    }

    #[test]
    fn a_jump_into_a_steep_hillside_banks_no_height() {
        // **The climbing ratchet the director caught** (their capture: ten jumps up an Elwynn
        // hillside, +1.2 yd banked each, 100.0 -> 104.6 in half a minute). Jumping at a face too
        // steep to walk must cost the jump and return the body where it started: the face may
        // convert the body's own descent into a slide down itself, and nothing else.
        let arc = jump_into(
            world_with_steep_hillside(),
            Vec3::new(-0.6, 0.0, 0.0),
            Vec3::X,
            150,
        );
        let peak = arc.iter().map(|r| r.0).fold(f32::MIN, f32::max);
        let end = arc.last().unwrap().0;
        assert!(
            peak > 1.0,
            "the jump must actually leave the ground: {peak:.3}"
        );
        assert!(
            end < 0.05,
            "the arc must end back at the foot of the slope, not banked part-way up it: \
             ended at {end:.3} (peak {peak:.3})"
        );
        // …and it must never *read* as a landing on the way down, which is how the height gets
        // banked on screen: [`WEDGE_STILL_FRAMES`] consecutive frames under [`WEDGE_STALL_RATIO`]
        // of gravity's intent is what the mover calls a wedge rest, and it stands the body up
        // wherever it is. Above the foot of the slope there is nothing here to rest on.
        let (mut run, mut worst_run) = (0u8, 0u8);
        for r in arc.iter().filter(|r| r.0 > 0.2) {
            run = if r.1 < -WEDGE_MIN_FALL && r.2 < WEDGE_STALL_RATIO {
                run + 1
            } else {
                0
            };
            worst_run = worst_run.max(run);
        }
        assert!(
            worst_run < WEDGE_STILL_FRAMES,
            "the descent must never stall long enough to read as a wedge rest: \
             {worst_run} consecutive stalled frames"
        );
    }

    #[test]
    fn a_steep_face_never_cancels_a_fall() {
        // The ratchet's engine, stated where it lives. Collide-and-slide's true-plane clip turns a
        // horizontal push into upward motion (`v'.y - v.y = -(v·n)·n.y`), and against a 55° face a
        // run into the hill very nearly cancels gravity: the capture's stalled frames descended
        // 1-7% of what gravity intended, three in a row, which is what tripped the wedge rest into
        // "landed standing" (decisions 0211/0212) part-way up an open hillside.
        //
        // What geometry may take is bounded: a body sliding down a face of pitch θ keeps
        // `sin²θ` of its descent (0.67 at 55°). Anything under that is the push holding the body
        // up, not the slope carrying it down.
        let arc = jump_into(
            world_with_steep_hillside(),
            Vec3::new(-0.6, 0.0, 0.0),
            Vec3::X,
            150,
        );
        // Frames clear of the flat ground only: once the body is back at the foot of the slope it
        // is *standing*, and a standing frame achieving none of gravity's intent is the floor
        // doing its job. (This harness is the bare arc — it has no ground probe to tell it that.)
        let worst = arc
            .iter()
            .filter(|r| r.1 < -WEDGE_MIN_FALL && r.0 > 0.2)
            .map(|r| r.2)
            .fold(f32::MAX, f32::min);
        assert!(
            worst > 0.98,
            "a falling frame against the face keeps ALL of its descent — the reference's response \
             is horizontal only: worst was {worst:.2} of intent"
        );
    }

    #[test]
    fn walkable_and_overhanging_faces_are_untouched() {
        let push = Vec3::new(7.0, 0.0, 0.0);
        // Walkable floor: the slide's ordinary uphill walk.
        assert!(steep_contact_shear(face(40.0), push).is_none());
        // Overhang: the ceiling clip stands as-is.
        assert!(steep_contact_shear(Vec3::new(-0.5, -0.7, 0.0).normalize(), push).is_none());
        // A receding face never opposes the motion.
        assert!(steep_contact_shear(face(60.0), -push).is_none());
    }

    #[test]
    fn a_vertical_wall_takes_the_whole_push_and_no_more() {
        // A true vertical face manufactures no lift under either rule — the strip removes the
        // push, and there is no vertical left for the plane to take.
        let v = Vec3::new(7.0, -4.0, 0.0);
        let out = steep_contact_shear(face(90.0), v).expect("must strip");
        assert!(out.x.abs() < 1e-6 && (out.y - v.y).abs() < 1e-6);
    }
}
