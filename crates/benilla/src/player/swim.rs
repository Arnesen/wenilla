//! Player **swim mode** — the water-movement regime the ground mover ([`super::mover`]) can't express:
//! when the water gets deep enough the avatar leaves the floor, floats, and swims in 3D along its
//! pitched facing, instead of walking the lakebed. Setting [`Player::swimming`] here is what lights
//! the swim gaits ([`crate::creature_anim`] ids 41–45) for the local player and streams
//! `MOVEFLAG_SWIMMING` (+ the pitch tail) on the wire (the decision-0052 swim follow-up).
//!
//! The enter/exit boundary is **VERIFIED** against the real client (wow-re
//! `collision/scratch/swim-transition.md`, resolving benilla-pins **B7**): the per-frame local decision
//! `0x6030c0` fires `MOVEFLAG_SWIMMING 0x00200000` off `depth = liquidSurface − feetZ` vs a fraction of
//! the unit's collision height, with a genuine 1/36-yd hysteresis band (see [`SWIM_ENTER_DEPTH`]).
//! There is **no** separate wade/water movement flag (`0x40000000` is HOVER — wading is just the
//! implicit in-liquid-below-threshold state).
//!
//! The **vertical law is VERIFIED** (the swim §5's TU-B, wow-re `swim-mechanism.md`, superseding the
//! earlier surface-follow reading): while SWIMMING the mover routes through the client's *floating*
//! resolver, which **bypasses gravity entirely** — an idle swimmer's depth is **frozen** (no sink, no
//! rise, no ease), the vertical comes only from the pitched travel velocity, and the one constraint is
//! a **hard top-cap at `surface − 0.75·collisionHeight`** (`0x632ba0` ×0.75): the feet can never rise
//! above the resting waterline, so a surfacing swimmer stops ~three-quarters submerged, head out.
//! There is no spring and no resting *seek* — the old `REST_SUBMERSION`/`BUOYANCY_RATE` ease is gone.
//! Reaching the cap does NOT bleed the stroke off: the capped rise redirects **level, at full
//! speed** — surface swimming — and the presented pitch levels with it ([`cap_redirect`],
//! decisions 0499+0505). This is a **named divergence**: the §5 (wow-re
//! `swim-topcap-velocity.md`) found the exe's own-input resolver GRINDS a steep aim at the
//! cap (`0x634640` tangential re-sweep, no renormalization) — which contradicts the
//! director-confirmed ref behavior — and its only leveling regime (travel-derived
//! `asin(dir.z)` pitch) belongs to spline/CTM movers and would be overwritten per-frame by
//! the 0492 mouselook direct-set. The redirect is benilla's construction reproducing the
//! validated feel, kept until wow-re's live capture pins the real surface law (0505).
//!
//! The one way OUT through the top is the **swim jump** ([`breach_step`], TU-B(f)+TU-F
//! `0x7c6230`): jump clears SWIMMING unconditionally and launches the walk mover's FALLING arc at
//! [`SWIM_JUMP_SPEED`] — the breach hop. Re-entry is the same depth check plus the verified fall
//! gate `0x7c5de0` — swim re-latches once the upward velocity has decayed to half the launch
//! value, discarding the residual (see [`update_swimming`]).
//!
//! **Space maps to the ref's Jump routing, at any depth, one hop per PRESS** (decisions
//! 0487 + 0498): a swimming press fires the jump-exit wherever it happens — at the surface it
//! breaches out; submerged it's the ~1.6-yd dolphin-hop — and a held key does NOT re-fire
//! after the re-latch (director-verified on the ref; the byte gate behind that is an open
//! wow-re question, see 0498). The way to RISE is aiming up with the right mouse and swimming
//! forward — the ref's own mouselook→SetPitch mechanism, **VERIFIED** (wow-re
//! `swim-camera-pitch.md`, decision 0492).

use avian3d::prelude::*;
use bevy::prelude::*;

use crate::collision::player_query_filter;
use crate::liquid::WaterChunkInfo;

use super::mover::Outcome;
use super::{Player, CAPSULE_HEIGHT, GRAVITY, GROUND_COS, GROUND_PROBE, SKIN_WIDTH};
use benilla_assets::coords::bevy_to_wow;

/// Swim travel speed (yd/s) — vanilla's default `MOVE_SWIM` (0.66× run). A real server-seeded speed
/// (the 4th `SpeedInfo` field, already parsed for remote swimmers in [`benilla_protocol`]); the local
/// avatar has no per-unit speed plumbing yet, so this stands in as the vanilla default. Independent of
/// `$WOW_MOVE_SPEED` (that overrides run), so overriding run leaves swim at its own rate.
pub(super) const SWIM_SPEED: f32 = 4.722_222;

/// Backward swim speed (yd/s) — vanilla's default `MOVE_SWIM_BACK` (vmangos `baseMoveSpeed[4]` =
/// 2.5), the fallback when the server's `SpeedInfo` hasn't streamed. A net-backward swim takes
/// `min(swimBack, swim)` — **VERIFIED** (`0x7c4c90`'s swim arm, the swim-feel §5's TU-H): the
/// backward bit `0x2` selects a min byte-identical in template to the run arm's
/// `min(runBack, run)`; strafe-only swims at the forward [`SWIM_SPEED`].
pub(super) const SWIM_BACK_SPEED: f32 = 2.5;

/// Swim-jump take-off speed (yd/s) — **VERIFIED** `0x7c6230`'s swim seed `0xc1118c48 = −9.096748`
/// (the client stores fall velocity down-positive; up for us), vs the land jump's 7.955547 — a
/// swim jump launches ~14% harder, enough to breach and hop onto a low bank.
const SWIM_JUMP_SPEED: f32 = 9.096_748;

/// The fraction of the unit's collision height the water must cover to start swimming — **VERIFIED**
/// `0.75` (`0x8012cc`, the compare at `0x6030c0`). Applied to the feet-referenced depth.
const SWIM_DEPTH_FRAC: f32 = 0.75;
/// The enter/leave hysteresis band (yd) — **VERIFIED** `1/36` (`0x7ff9d0`): enter compares depth against
/// `0.75·h`, leave against `0.75·h − 1/36`, so between them the swim state holds (`0x603100`/`0x6031c0`).
const SWIM_HYSTERESIS: f32 = 1.0 / 36.0;

/// Submersion depth (yd, water surface above the feet) to **start** swimming — VERIFIED `0.75·h` from the
/// feet (`h` = the unit's collision height, our [`CAPSULE_HEIGHT`]): ≈1.52 yd for a human, water covering
/// ~three-quarters of the box (chest/neck deep).
const SWIM_ENTER_DEPTH: f32 = SWIM_DEPTH_FRAC * CAPSULE_HEIGHT;
/// Submersion depth (yd) below which swimming **stops** — VERIFIED `0.75·h − 1/36` (the lower edge of the
/// hysteresis band); also stops the instant there's no liquid over the feet.
const SWIM_EXIT_DEPTH: f32 = SWIM_ENTER_DEPTH - SWIM_HYSTERESIS;

/// The hard **top-cap line** (yd below the surface) a rising swimmer stops at — feet at
/// `surface − 0.75·h`, i.e. ~three-quarters submerged, head out. VERIFIED: the floating resolver's
/// collision top-cap plane at `surface − 0.75·collisionHeight` (`0x632ba0` ×0.75, the open-bottom
/// k-DOP's one top plane). The same `0.75·h` as the enter threshold, so a capped swimmer sits above
/// the leave threshold and can't flicker out of the mode.
const REST_CAP: f32 = SWIM_ENTER_DEPTH;

/// The **Bevy-Y liquid surface** at the avatar's feet, if any liquid covers that XY — the shared
/// swim/buoyancy query. [`liquid_at`] answers in WoW Z; WoW Z maps straight to Bevy Y, so the
/// surface height above the feet is the same delta in either space and we lift the feet's Bevy Y by it.
///
/// Uses [`liquid_at`], not the water-only wrapper: **you swim in lava and slime too** — Blackrock's
/// magma and Undercity's sludge are surfaces you enter, not ones you fall through (decision 0634).
/// `indoors` is the player's live WMO-interior claim, which picks whose liquid answers.
pub(super) fn surface_over_feet(
    water: &Query<&WaterChunkInfo>,
    feet: Vec3,
    indoors: bool,
) -> Option<f32> {
    let wow = bevy_to_wow(feet);
    crate::liquid::liquid_at(water.iter(), wow, Some(indoors))
        .map(|hit| feet.y + (hit.surface_z - wow[2]))
}

/// Update [`Player::swimming`] from the water surface over the feet, with the verified enter/leave
/// hysteresis (`0x6030c0`) — returns the new state. `None` surface = not in liquid = not swimming (the
/// binary's `inLiquid == 0 → STOP`). Enter is a strict `depth > 0.75·h`; leave is `depth < 0.75·h − 1/36`
/// (i.e. swimming holds while `depth ≥` the leave threshold), matching the two byte compares.
///
/// The enter arm carries the **fall re-entry gate** — **VERIFIED** `0x7c5de0`, called from
/// `0x6030c0`'s ENTER branch (the Space-§5's TU-G): a fresh launch is not re-latched into swim
/// until its **upward velocity has decayed to HALF the launch value** — blocked iff
/// `FALLING ∧ v_up > 0 ∧ t_airborne < v_launch/(2g)` (≈0.236 s for a swim jump). Note the release
/// happens *while still rising*: the dolphin-hop tops out ≈1.6 yd up, then swim re-latches and the
/// floating resolver freezes the depth — the residual velocity is discarded (`0x7c6e50`→`0x7c6290`
/// clears FALLING, `+0xa0` left dormant), which our swim arm mirrors (`swim_step` zeroes `vel_y`,
/// the controller clears the arc). `now` is `Time::elapsed_secs`, for the airborne clock.
///
/// A latched swim also **ends the post-teleport/login settle** (benilla's own streaming gate —
/// the ref blocks on load and has no such state). Settling holds the avatar so it can't fall
/// through not-yet-streamed floors, and its release gate runs only in the walk mover
/// ([`super::mover::step`]) — which a swimmer never reaches — while the floating resolver can't
/// fall at all (gravity bypassed, depth frozen). The latch itself proves the tile's liquid is
/// resident, so the water IS the arrived support; without this, a login into swim-depth water
/// held `settling` — and the loading screen, which waits on it — forever.
pub(super) fn update_swimming(player: &mut Player, surface_y: Option<f32>, now: f32) -> bool {
    let Some(surface) = surface_y else {
        player.swimming = false; // not in liquid
        return false;
    };
    let depth = surface - player.pos.y;
    player.swimming = if player.swimming {
        depth >= SWIM_EXIT_DEPTH
    } else {
        let hop_blocked = player.vel_y > 0.0
            && player
                .airborne_since
                .is_some_and(|t0| now - t0 < player.jump_zspeed / (2.0 * GRAVITY));
        depth > SWIM_ENTER_DEPTH && !hop_blocked
    };
    if player.swimming {
        player.settling = false;
    }
    player.swimming
}

/// The **jump out of the water** — the takeoff frame of a jump while swimming (**VERIFIED**
/// mechanism, wow-re `swim-mechanism.md` TU-B(f)+TU-F, `0x7c6230`): SWIMMING selects the take-off
/// [`SWIM_JUMP_SPEED`] over the land 7.9555, then the handler clears SWIMMING and sets FALLING.
/// The Jump command routes here at *any* depth (no swim re-route, no surface-proximity gate —
/// the deep press is the dolphin-hop; decision 0487 restored the ref routing after 0479's
/// depth-gated interlude). Horizontal
/// momentum freezes at takeoff like every jump (`0x7c61f0` never rewrites it): the last swim
/// frame's travel carries the leap. Like the land jump's takeoff frame there is no gravity tick
/// here — the walk mover integrates gravity from the next frame — so the arc snapshot (and the
/// wire jump tail) carries the exact seed. The caller has already cleared [`Player::swimming`];
/// the walk/fall machinery owns the arc from here.
pub(super) fn breach_step(
    player: &mut Player,
    time: &Time,
    ms: &MoveAndSlide<'_, '_>,
    capsule: &Collider,
) -> Outcome {
    player.vel_y = SWIM_JUMP_SPEED;
    let half_h = Vec3::Y * (CAPSULE_HEIGHT * 0.5);
    let out = ms.move_and_slide(
        capsule,
        player.pos + half_h,
        Quat::IDENTITY,
        player.horiz_vel + Vec3::Y * player.vel_y,
        time.delta(),
        &MoveAndSlideConfig::default(),
        &player_query_filter(),
        |_hit| MoveAndSlideHitResponse::Accept,
    );
    player.pos = out.position - half_h;
    Outcome {
        held: false,
        grounded: false,
        jumped: true,
        air_nudged: false,
        ground: None,
    }
}

/// The outcome of one swim step — read by the controller for the wire/animation flags.
pub(super) struct SwimOutcome {
    /// Solid walkable floor is right under the feet (a shallow bottom). With the exit hysteresis this
    /// is how a swim into the shallows resolves back onto the ground.
    pub grounded: bool,
    /// `Some` when the rest-line cap redirected the stroke (the surface-swim regime): the
    /// *effective* travel pitch after the redirect — what the body pose and the wire pitch tail
    /// present this frame (→0 pinned at the line). `None` when the cap didn't bite (free swim,
    /// idle, descending). The raw camera aim stays in [`Player::swim_pitch`] untouched.
    pub surface_pitch: Option<f32>,
}

/// Cap a rising stroke at the rest line — and REDIRECT the capped speed level rather than bleed
/// it off: reaching the surface flips a pitched-up swim into full-speed *surface swimming*
/// (director-verified on the ref, 2026-07-18; decisions 0499+0505). A plain slide against the
/// top-cap plane leaves only `cos(pitch)·speed` — ~0 at a steep aim — pinning the swimmer
/// under the waterline (the "invisible wall"), and per the §5 that grind IS what the exe's
/// own-input resolver computes (`0x634640`) — contradicting the confirmed ref behavior, so
/// this redirect stands as benilla's own construction (the 0505 named divergence). The
/// stroke's SPEED is preserved: the upward component is clamped to `cap` (how much rise
/// reaches the rest line this frame) and the remainder rotates into the level travel
/// direction. Returns the velocity and, when the cap bit, the effective travel pitch
/// (`atan2(up, level)`).
fn cap_redirect(input_vel: Vec3, cap: f32) -> (Vec3, Option<f32>) {
    if input_vel.y <= 0.0 || input_vel.y <= cap {
        return (input_vel, None);
    }
    let speed = input_vel.length();
    let level_dir = Vec3::new(input_vel.x, 0.0, input_vel.z).normalize_or_zero();
    let level_speed = (speed * speed - cap * cap).max(0.0).sqrt();
    (
        level_dir * level_speed + Vec3::Y * cap,
        Some(cap.atan2(level_speed)),
    )
}

/// Advance the avatar one swim frame: the pitched travel velocity through the client's *floating*
/// physics (VERIFIED, TU-B — gravity bypassed: an idle swimmer's depth is **frozen**, the vertical
/// comes only from `input_vel`), a collide-and-slide against the lakebed/banks, and the hard
/// [`REST_CAP`] top line — where a rise doesn't stop dead but **redirects level into full-speed
/// surface swimming** ([`cap_redirect`], decision 0499). `input_vel` is the desired 3D velocity
/// from the swim controls (the pitched travel basis + ascent); `surface_y` is the Bevy-Y waterline
/// over the feet. Writes `player.pos`/`vel_y`/`horiz_vel`.
pub(super) fn swim_step(
    player: &mut Player,
    time: &Time,
    ms: &MoveAndSlide<'_, '_>,
    capsule: &Collider,
    input_vel: Vec3,
    surface_y: f32,
) -> SwimOutcome {
    let dt = time.delta_secs();
    let half_h = Vec3::Y * (CAPSULE_HEIGHT * 0.5);
    let center = player.pos + half_h;
    let filter = player_query_filter();

    // The one vertical constraint: never rise ABOVE the resting waterline — cap the *upward velocity*
    // so the feet reach at most the rest line this frame, NOT the position after the slide. That
    // distinction is load-bearing: a position clamp (the earlier bug) overrode terrain collision,
    // shoving the feet down onto the rest line even where a shallow bottom held them higher — which
    // clipped us into the floor AND pinned `depth` at ≥ rest, so `update_swimming` never saw water
    // shallow enough to leave (you couldn't get out onto land, and Space-ascend fought a wall). A
    // velocity cap leaves the bottom to collision: in the shallows the feet rest on the floor and
    // `depth` drops below the leave threshold, so we walk out. dt-based so it can't overshoot the
    // rest line and dip back under the exit depth (a flicker). No other vertical force exists here —
    // no gravity, no surface-seek (the verified floating resolver).
    let rest_feet_y = surface_y - REST_CAP;
    let cap = if dt > 0.0 {
        ((rest_feet_y - player.pos.y) / dt).max(0.0)
    } else {
        f32::INFINITY
    };
    let (vel, surface_pitch) = cap_redirect(input_vel, cap);

    let out = ms.move_and_slide(
        capsule,
        center,
        Quat::IDENTITY,
        vel,
        time.delta(),
        &MoveAndSlideConfig::default(),
        &filter,
        |_hit| MoveAndSlideHitResponse::Accept,
    );
    let c = out.position;
    player.pos = c - half_h;
    // Swim owns its vertical directly; leave a clean zero so exiting into a fall starts from rest and
    // horiz_vel drives the swim gait's playback rate like every other locomotion clip.
    player.vel_y = 0.0;
    player.horiz_vel = Vec3::new(vel.x, 0.0, vel.z);

    let probe = ms.cast_move(
        capsule,
        c,
        Quat::IDENTITY,
        Vec3::NEG_Y * GROUND_PROBE,
        SKIN_WIDTH,
        &filter,
    );
    let grounded = probe.is_some_and(|h| h.normal1.y >= GROUND_COS);
    SwimOutcome {
        grounded,
        surface_pitch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player_at(y: f32) -> Player {
        Player {
            pos: Vec3::new(0.0, y, 0.0),
            ..Default::default()
        }
    }

    /// Swim mode latches with the verified 1/36-yd hysteresis: enter is a strict `depth > 0.75·h`, leave
    /// is `depth < 0.75·h − 1/36`, so a depth inside the band holds whatever state we were in — wading
    /// the boundary can't flicker the physics regime frame to frame (wow-re swim-transition `0x6030c0`).
    #[test]
    fn swim_entry_and_exit_hysteresis() {
        // The band is exactly 1/36 yd, independent of height.
        assert!((SWIM_ENTER_DEPTH - SWIM_EXIT_DEPTH - 1.0 / 36.0).abs() < 1e-6);
        // Enter is 0.75·collisionHeight from the feet.
        assert!((SWIM_ENTER_DEPTH - 0.75 * CAPSULE_HEIGHT).abs() < 1e-6);

        let mut p = player_at(0.0); // feet at y=0, so the surface height *is* the submersion depth
        let mid_band = (SWIM_ENTER_DEPTH + SWIM_EXIT_DEPTH) * 0.5;
        assert!(
            !update_swimming(&mut p, Some(SWIM_ENTER_DEPTH), 0.0),
            "exactly at the enter depth does not yet swim (strict >)"
        );
        assert!(
            !update_swimming(&mut p, Some(mid_band), 0.0),
            "a depth in the band, entered from walking, stays walking"
        );
        assert!(
            update_swimming(&mut p, Some(SWIM_ENTER_DEPTH + 0.01), 0.0),
            "past the enter depth starts swimming"
        );
        assert!(
            update_swimming(&mut p, Some(mid_band), 0.0),
            "the same in-band depth, now entered from swimming, keeps swimming (hysteresis)"
        );
        assert!(
            !update_swimming(&mut p, Some(SWIM_EXIT_DEPTH - 0.01), 0.0),
            "dropping below the leave threshold returns to walking"
        );
        // Re-enter, then prove no-liquid stops it regardless of depth.
        assert!(update_swimming(&mut p, Some(SWIM_ENTER_DEPTH + 0.5), 0.0));
        assert!(
            !update_swimming(&mut p, None, 0.0),
            "no liquid over the feet stops swimming (the binary's inLiquid == 0 → STOP)"
        );
    }

    /// A login/teleport into swim-depth water must end the settle hold: the settle release gate
    /// runs only in the walk mover, which a swimmer never reaches — before this rule, `settling`
    /// stayed latched forever and the loading screen (which clears only on `!settling`) hung at a
    /// full bar (the Booty Bay underwater-login hang, 2026-07-18). Wading depth and dry land keep
    /// the hold — there the walk mover runs and its own gate (ground probe / timeout) decides.
    #[test]
    fn latching_swim_ends_the_settle_hold() {
        let mut p = player_at(0.0);
        p.settling = true;
        assert!(update_swimming(&mut p, Some(SWIM_ENTER_DEPTH + 1.0), 0.0));
        assert!(!p.settling, "the water is the support settling waited for");

        let mut wading = player_at(0.0);
        wading.settling = true;
        assert!(!update_swimming(
            &mut wading,
            Some(SWIM_EXIT_DEPTH - 0.5),
            0.0
        ));
        assert!(
            wading.settling,
            "wading depth leaves the hold to the walk gate"
        );

        let mut dry = player_at(0.0);
        dry.settling = true;
        assert!(!update_swimming(&mut dry, None, 0.0));
        assert!(dry.settling, "no liquid leaves the hold to the walk gate");
    }

    /// The verified fall re-entry gate (`0x7c5de0`): a fresh swim jump is not re-latched into swim
    /// until its upward velocity has decayed to HALF the launch value (`t ≥ v₀/(2g)` ≈ 0.236 s) —
    /// and the release happens *while still rising*, which is what tops the dolphin-hop at
    /// ≈1.6 yd rather than the full ballistic apex. The leave arm is untouched: a swimmer's
    /// vertical is owned by `swim_step` (which zeroes `vel_y`), so the gate can never hold
    /// someone *in* the water.
    #[test]
    fn the_hop_relatches_at_half_launch_velocity() {
        let half_decay = SWIM_JUMP_SPEED / (2.0 * GRAVITY);
        let deep = Some(SWIM_ENTER_DEPTH + 1.0);
        let mut p = player_at(0.0);
        p.airborne_since = Some(0.0);
        p.jump_zspeed = SWIM_JUMP_SPEED;
        p.vel_y = SWIM_JUMP_SPEED;
        assert!(
            !update_swimming(&mut p, deep, half_decay * 0.5),
            "young launch, still fast — the hop keeps rising"
        );
        p.vel_y = SWIM_JUMP_SPEED * 0.49;
        assert!(
            update_swimming(&mut p, deep, half_decay + 1e-3),
            "velocity decayed to half — swim re-latches while STILL rising (the ~1.6 yd hop top)"
        );
        // A plain fall into water (no upward velocity) enters regardless of the clock.
        let mut q = player_at(0.0);
        q.airborne_since = Some(0.0);
        q.jump_zspeed = SWIM_JUMP_SPEED;
        q.vel_y = -0.1;
        assert!(
            update_swimming(&mut q, deep, 0.01),
            "descending into depth enters swim — the gate only guards a rising launch"
        );
    }

    /// The surface redirect (decision 0499): a pitched-up stroke reaching the rest line keeps its
    /// SPEED and turns level — the ref's full-speed surface swimming — instead of sliding against
    /// the cap at `cos(pitch)·speed ≈ 0` (the director's "invisible wall"). The presented pitch
    /// levels with it; free/descending strokes pass through untouched.
    #[test]
    fn the_rest_line_redirects_the_stroke_level_at_full_speed() {
        // Cap far away (deep): untouched, no presented-pitch override.
        let free = Vec3::new(0.1, 3.0, 0.2);
        assert_eq!(cap_redirect(free, 100.0), (free, None));
        assert_eq!(cap_redirect(free, f32::INFINITY), (free, None));
        // Descending: the cap never touches a dive.
        let dive = Vec3::new(1.0, -2.0, 0.0);
        assert_eq!(cap_redirect(dive, 0.0), (dive, None));

        // Pinned at the line (cap 0): a near-vertical stroke becomes a FULL-speed level stroke.
        let steep = Vec3::new(0.08, 4.72, 0.0); // ~89° aim, forward at swim speed
        let (vel, pitch) = cap_redirect(steep, 0.0);
        assert!((vel.length() - steep.length()).abs() < 1e-4, "speed kept");
        assert_eq!(vel.y, 0.0, "level at the line");
        assert!(vel.x > 4.7, "the whole speed went forward");
        assert_eq!(pitch, Some(0.0), "the presented pitch levels out");

        // Approaching the line (partial cap): speed still preserved, pitch eases toward level.
        let (vel2, pitch2) = cap_redirect(steep, 2.0);
        assert!((vel2.length() - steep.length()).abs() < 1e-4);
        assert_eq!(vel2.y, 2.0);
        let aim = steep.y.atan2(steep.x);
        let eased = pitch2.expect("the cap bit");
        assert!(eased > 0.0 && eased < aim, "between level and the raw aim");
    }
}
