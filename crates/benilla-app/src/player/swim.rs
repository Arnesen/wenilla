//! Player **swim mode** — the water-movement regime the ground mover ([`super::mover`]) can't express:
//! when the water gets deep enough the avatar leaves the floor, floats, and swims in 3D along its
//! pitched facing, instead of walking the lakebed. Setting [`Player::swimming`] here is what lights
//! the swim gaits ([`crate::creature_anim`] ids 41–45) for the local player and streams
//! `MOVEFLAG_SWIMMING` (+ the pitch tail) on the wire (the decision-0052 swim follow-up).
//!
//! The enter/exit boundary is **VERIFIED** against the real client (wow-re
//! `collision/scratch/swim-transition.md`, resolving benilla-pins **B7**): the per-frame local decision
//! `0x6030c0` fires `MOVEFLAG_SWIMMING 0x00200000` off `depth = liquidSurface − feetZ` vs a fraction of
//! the unit's collision height, with a genuine 1/36-yd hysteresis band (see [`swim_enter_depth`]).
//! There is **no** separate wade/water movement flag (`0x40000000` is HOVER — wading is just the
//! implicit in-liquid-below-threshold state).
//!
//! The **vertical law is VERIFIED** (the swim §5's TU-B, wow-re `swim-mechanism.md`, superseding the
//! earlier surface-follow reading): while SWIMMING the mover routes through the client's *floating*
//! resolver, which **bypasses gravity entirely** — an idle swimmer's depth is **frozen** (no sink, no
//! rise, no ease), the vertical comes only from the pitched travel velocity, and the one constraint is
//! a **hard top-cap at `surface − 0.75·collisionHeight`** (`0x632ba0` ×0.75): the feet can never be
//! shallower than the resting waterline, so a surfacing swimmer stops ~three-quarters submerged, head
//! out. That cap constrains the **position, both ways** — wow-re's Q5 states it as "feet cannot come
//! shallower than `surface − 0.75·collisionHeight`", a hard clamp re-tested each active substep — and
//! what leaves a *stroking* swimmer above the line is usually not a rise but the surface **descending**
//! under them, which is why the constraint has to be re-satisfied and not merely guarded against
//! ([`settle_to_rest`], decision 0644). It is still no spring and no resting *seek* — the old
//! `REST_SUBMERSION`/`BUOYANCY_RATE` ease is gone, and a swimmer *below* the line is never pulled up.
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

use benilla_world::world_point::{Subject, WorldPoint};

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

/// The `UNIT_FIELD_FLAGS` bits that permit a unit to **enter** swim at all — **VERIFIED** in the
/// shipped 5875 image, the gate `0x6030c0` runs *after* `depth > 0.75·h` and *before* TryStartSwim
/// `0x60df70`:
///
/// ```text
/// 60310b mov ecx,[esi+0x110]      ; the unit's descriptor block base (&m_uint32Values[OBJECT_END])
/// 603111 mov eax,[ecx+0xa0]       ; +0xa0 = field OBJECT_END+0x28 = UNIT_FIELD_FLAGS (1.12.1 idx 46)
/// 603117 test ah,ah   ; js  60312c    ; bit 15 set → proceed
/// 60311b shr edx,3    ; test dl,1 ; jne 60312c   ; bit  3 set → proceed
/// 603125 shr eax,0xb  ; test al,1 ; je  60314a   ; bit 11 clear → SKIP TryStartSwim
/// ```
///
/// Any **one** of the three permits entry; with none of them the unit stays in walk mode however
/// deep the water, and **walks the lakebed**. What each bit is (vmangos `UnitDefines.h`, and its
/// own writers):
///
/// - `0x8` **PLAYER_CONTROLLED** — every body a player can steer: players (set at creation,
///   `Player.cpp:462`), pets, totems, guardians, companions, charms (`SpellAuras.cpp` charm/possess).
/// - `0x800` **PET_IN_COMBAT**.
/// - `0x8000` **USE_SWIM_ANIMATION** — the server's "this creature swims" bit, applied on spawn from
///   `creature_template.static_flags1 & CREATURE_STATIC_FLAG_CAN_SWIM` (`Creature::
///   ToggleUnitFlagsFromStaticFlags`). vmangos names the client behaviour it is naming exactly:
///   *"Without it units walk on the sea floor instead of swimming"*, and, at its pathing site,
///   *"Giant type creatures walk underwater"*. **956 of the 10912** rows on our server carry it.
///
/// The gate is on **ENTER only** — the exit leg `0x6031b4` reads no flags, so a unit that is
/// already swimming leaves on depth alone.
///
/// **The local avatar does not consult this**, and that is a verified no-op rather than an
/// omission: bit 3 is set on every body a player can be [`crate::net::Embodied`] in — its own
/// character, a charmed creature, a possessed one — so the gate always passes there. It bites only
/// on the creature marker (`crate::net::motion::spline::mark_swimming_creatures`), which is where
/// benilla derives a creature's swim state the wire never carries.
const SWIM_ENTER_UNIT_FLAGS: u32 = 0x8 | 0x800 | 0x8000;

/// May this unit **enter** swim from its own depth? — [`SWIM_ENTER_UNIT_FLAGS`]'s one spelling.
pub(crate) fn may_enter_swim(unit_flags: u32) -> bool {
    unit_flags & SWIM_ENTER_UNIT_FLAGS != 0
}

/// Submersion depth (yd, water surface above the feet) to **start** swimming — VERIFIED `0.75·h`
/// from the feet, where `h` is **the unit's own collision height**
/// ([`crate::entities::CollisionHeight`]): water covering ~three-quarters of its collision box,
/// chest/neck deep. ≈1.52 yd for a human male, ≈0.86 for a gnome female, ≈1.83 for a night elf male.
///
/// It was `0.75 × CAPSULE_HEIGHT` — one constant for every race — until decision 0645. The
/// *fraction* was right all along; the height it multiplied was a human's, so a gnome's rest line
/// sat below her own head and she could never surface (B76).
///
/// This is also the **wade ceiling**: wading is the implicit in-liquid-but-not-swimming state
/// (there is no wade movement flag — `0x40000000` is HOVER), so "deepest water you can still wade"
/// and "shallowest water you swim in" are one number. The consumers with no `Player` of their own —
/// the creature swim marker and the footstep splash slot — call it through `crate::player`, rather
/// than keeping the second spelling `liquid::WADE_MAX` was.
pub(crate) fn swim_enter_depth(h: f32) -> f32 {
    SWIM_DEPTH_FRAC * h
}
/// Submersion depth (yd) below which swimming **stops** — VERIFIED `0.75·h − 1/36` (the lower edge
/// of the hysteresis band); also stops the instant there's no liquid over the feet. The band itself
/// is an absolute `1/36` yd, independent of `h` (wow-re: "the band is exactly 1/36 yd … independent
/// of `h`") — so it is subtracted, never scaled.
pub(super) fn swim_exit_depth(h: f32) -> f32 {
    swim_enter_depth(h) - SWIM_HYSTERESIS
}

/// The hard **top-cap line** (yd below the surface) a rising swimmer stops at — feet at
/// `surface − 0.75·h`, i.e. ~three-quarters submerged, head out. VERIFIED: the floating resolver's
/// collision top-cap plane at `surface − 0.75·collisionHeight` (`0x632ba0` ×0.75, the open-bottom
/// k-DOP's one top plane). The same `0.75·h` as the enter threshold, so a capped swimmer sits above
/// the leave threshold and can't flicker out of the mode.
fn rest_cap(h: f32) -> f32 {
    swim_enter_depth(h)
}

/// The **Bevy-Y liquid surface** at the avatar's feet, if any liquid covers that XY — the shared
/// swim/buoyancy query. The world answers in WoW Z; WoW Z maps straight to Bevy Y, so the
/// surface height above the feet is the same delta in either space and we lift the feet's Bevy Y by it.
///
/// Asks for **any** liquid, not the water-only wrapper: **you swim in lava and slime too** —
/// Blackrock's magma and Undercity's sludge are surfaces you enter, not ones you fall through
/// (decision 0634). The subject is the player, so which ROOM's liquid answers is its own live
/// interior claim (decision 0696).
pub(super) fn surface_over_feet(world: &WorldPoint, feet: Vec3) -> Option<f32> {
    let wow = bevy_to_wow(feet);
    world
        .liquid_at(Subject::Player, wow)
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
/// The latch deliberately does NOT touch [`Player::settling`]: the settle release is the terrain
/// streamer's, keyed on the destination's residency in every mover mode alike (decision 0737 —
/// this latch used to release it as a special case, because the old release was a ground probe
/// only the walk mover ran).
pub(super) fn update_swimming(player: &mut Player, surface_y: Option<f32>, now: f32) -> bool {
    // **LEVITATING bails the whole decision** — the reference's very first instruction here
    // (`0x6030d2 test ah,4` → `0x6031fa`; VERIFIED, wow-re `swim-transition.md`): neither the ENTER
    // arm nor the STOP arm runs, so the latch is left exactly as it stands. Not an optimisation —
    // it IS the mechanism of GM flight (decision 0726). The server sets SWIMMING and LEVITATING in
    // one packet, and the second is what stops the dry ground under us clearing the first on the
    // very next frame.
    if player.modes.levitating {
        return player.swimming;
    }
    let Some(surface) = surface_y else {
        player.swimming = false; // not in liquid
        return false;
    };
    let depth = surface - player.pos.y;
    let h = player.collision_height.0;
    player.swimming = if player.swimming {
        depth >= swim_exit_depth(h)
    } else {
        let hop_blocked = player.vel_y > 0.0
            && player
                .airborne_since
                .is_some_and(|t0| now - t0 < player.jump_zspeed / (2.0 * GRAVITY));
        depth > swim_enter_depth(h) && !hop_blocked
    };
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
    world: &benilla_world::collision::WorldCollision<'_, '_>,
    capsule: &Collider,
) -> Outcome {
    player.vel_y = SWIM_JUMP_SPEED;
    let half_h = Vec3::Y * (CAPSULE_HEIGHT * 0.5);
    let out = world.slide_body(
        capsule,
        player.pos + half_h,
        player.horiz_vel + Vec3::Y * player.vel_y,
        time.delta(),
        &MoveAndSlideConfig::default(),
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

/// How far the feet must sink to satisfy the rest-line constraint — the excess above
/// `surface − 0.75·h`, or zero when already at or under it. Pure so the law is pinned without a
/// physics world; the *sweep* that carries it out is [`swim_step`]'s, which is what leaves a rising
/// lakebed free to hold the feet higher.
fn settle_to_rest(feet_y: f32, surface_y: f32, h: f32) -> f32 {
    (feet_y - (surface_y - rest_cap(h))).max(0.0)
}

/// **Does the water own this mover's vertical this frame?** — the rest line [`swim_step`] enforces,
/// or `None` for none at all.
///
/// `None` means exactly one thing: **GM flight** (decision 0726). `LEVITATING` is the reference's
/// suppression of the water's authority over the mover (`0x6030d2`, the same bail
/// [`update_swimming`] takes), and the swim regime with no liquid that owns it has nothing to
/// constrain against — an ascent must be free, or you could dive but never climb.
///
/// A *momentary* sample miss in real water is deliberately **not** that case: it maps to
/// `Some(own feet Y)`, a rest line at our own depth, so the avatar simply holds for the frame.
/// Collapsing the two would make a chunk-seam glitch and GM flight indistinguishable.
///
/// This exists as one named function because the constraint has **two arms** — refuse the rise,
/// satisfy the drop — and B128 was what happened when they read different sources: 0644 added the
/// second arm after 0726 wrote the first, so flight was exempt from the cap and not from the settle
/// (decision 0882).
pub(super) fn rest_line(player: &Player, surface_y: Option<f32>) -> Option<f32> {
    (!player.modes.levitating).then(|| surface_y.unwrap_or(player.pos.y))
}

/// Advance the avatar one swim frame: the pitched travel velocity through the client's *floating*
/// physics (VERIFIED, TU-B — gravity bypassed: an idle swimmer's depth is **frozen**, the vertical
/// comes only from `input_vel`), a collide-and-slide against the lakebed/banks, and the hard
/// [`rest_cap`] line — where a rise doesn't stop dead but **redirects level into full-speed
/// surface swimming** ([`cap_redirect`], decision 0499), and where a stroke that ends up *above*
/// the line settles back onto it ([`settle_to_rest`], decision 0644).
///
/// `input_vel` is the desired 3D velocity from the swim controls (the pitched travel basis +
/// ascent); `surface_y` is the Bevy-Y waterline over the feet at the START of the frame (the rise
/// cap is a limit on *this* frame's climb, so it belongs to the position we climb from), and
/// `surface_at` resamples it at wherever the stroke actually lands (the settle has to satisfy the
/// constraint where the swimmer ends up — on a river reading the entry waterline instead lags by a
/// frame's descent, which on a steep enough surface is the whole hysteresis band). Writes
/// `player.pos`/`vel_y`/`horiz_vel`.
pub(super) fn swim_step(
    player: &mut Player,
    time: &Time,
    world: &benilla_world::collision::WorldCollision<'_, '_>,
    capsule: &Collider,
    input_vel: Vec3,
    surface_y: Option<f32>,
    surface_at: impl Fn(Vec3) -> Option<f32>,
) -> SwimOutcome {
    let dt = time.delta_secs();
    let half_h = Vec3::Y * (CAPSULE_HEIGHT * 0.5);
    let center = player.pos + half_h;

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
    // **No waterline, no rest line.** `None` is GM flight (decision 0726): the swim regime with no
    // liquid under it at all, where the one vertical constraint below has nothing to constrain
    // against and an ascent must be free. It is deliberately NOT the same as a *momentary* sample
    // miss in real water — the caller keeps that case as `Some(own feet Y)`, which caps the rise at
    // zero and holds for the frame.
    let cap = match surface_y {
        Some(surface) if dt > 0.0 => {
            let rest_feet_y = surface - rest_cap(player.collision_height.0);
            ((rest_feet_y - player.pos.y) / dt).max(0.0)
        }
        _ => f32::INFINITY,
    };
    let (vel, surface_pitch) = cap_redirect(input_vel, cap);

    let out = world.slide_body(
        capsule,
        center,
        vel,
        time.delta(),
        &MoveAndSlideConfig::default(),
        |_hit| MoveAndSlideHitResponse::Accept,
    );
    let mut c = out.position;
    // **Satisfy the rest line, don't merely guard it.** The cap above only ever refuses to *rise*
    // through the waterline; that is enough on a lake and wrong on everything else, because the
    // common way a swimmer ends up above the line is the line coming down to meet them. With the
    // vertical frozen (no gravity here — verified) a stroke down a river surface that falls ~10%
    // (measured: Felwood's Felfire Hill channel drops 5.94 yd across 60) loses 0.47 yd/s of depth,
    // and the whole enter/leave hysteresis band is 1/36 yd — so the latch left swim every ~0.05 s,
    // the walk mover took over and fell a few centimetres, swim re-latched, forever: the avatar
    // spent 47% of a downhill swim inside the FALL mover, flipping mode ~10×/s and alternating
    // SWIMMING/FALLING on the wire (director-reported as bouncing off the surface; live-probe
    // measured, decision 0644).
    //
    // The correction is a **swept** drop, not a position clamp: an earlier attempt clamped
    // `pos.y` after the slide, which overrode terrain collision, shoved the feet onto the rest
    // line even where a shallow bottom held them higher, and pinned `depth` at ≥ rest so the
    // shore exit could never fire. Sweeping it leaves the bottom in charge — wading out of a
    // lake, the bank holds the feet up, the depth falls under the leave threshold, and we walk
    // out exactly as before — and unlike folding the sink into `input_vel` it cannot eat the
    // horizontal stroke that climbs the bank in the first place.
    //
    // Gated on a stroke, matching the resolver's own outer gate (`0x634100 test [esi+0x40],0x200f`
    // — translation or falling, never idle): an idle floater is not resolved at all, so its depth
    // stays frozen (TU-B(c)).
    //
    // …and gated on **`surface_y.is_some()` — the very same rest line the cap above reads**, which
    // is the whole of B128 (decision 0882). The rest line has two arms — refuse the rise, satisfy
    // the drop — and they must share one gate, because `None` here means exactly one thing: GM
    // flight, the swim regime with no liquid that owns it (0726; a momentary sample miss is
    // `Some(own feet Y)` at the caller, never `None`). 0644 added this second arm *after* 0726 and
    // read the liquid straight from `surface_at` instead, so the exemption covered the cap and not
    // the settle: flying over a lake, `excess` is the entire altitude, the down-cast through open
    // air hits nothing, and one frame teleports the avatar onto the waterline. That is both
    // reporters' wording exactly — "from any height", and "*when moving* from land to water", the
    // `input_vel != ZERO` gate right here.
    if surface_y.is_some() && input_vel != Vec3::ZERO {
        if let Some(surface_now) = surface_at(c - half_h) {
            let excess = settle_to_rest(c.y - half_h.y, surface_now, player.collision_height.0);
            if excess > 0.0 {
                let drop = world
                    .cast_body(capsule, c, Vec3::NEG_Y * excess, SKIN_WIDTH)
                    .map_or(excess, |h| h.distance.min(excess));
                c.y -= drop;
            }
        }
    }
    player.pos = c - half_h;
    // Swim owns its vertical directly; leave a clean zero so exiting into a fall starts from rest and
    // horiz_vel drives the swim gait's playback rate like every other locomotion clip.
    player.vel_y = 0.0;
    player.horiz_vel = Vec3::new(vel.x, 0.0, vel.z);

    let probe = world.cast_body(capsule, c, Vec3::NEG_Y * GROUND_PROBE, SKIN_WIDTH);
    let grounded = probe.is_some_and(|h| h.normal1.y >= GROUND_COS);
    SwimOutcome {
        grounded,
        surface_pitch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped `CreatureModelData.collisionHeight × displayScale` for three real bodies —
    /// the numbers `benilla_formats`' `collision_height_is_the_m2_collision_box` pins against the
    /// client, restated here so these pure tests need no DBCs. Human male is the one the old
    /// constant happened to match (2.031 vs 2.0278 — 2 mm apart, which is why B76 hid for so long).
    const HUMAN_MALE: f32 = 2.031;
    const GNOME_FEMALE: f32 = 1.150; // 1.000 column × 1.15 display scale
    const NIGHT_ELF_MALE: f32 = 2.438;

    fn player_at(y: f32) -> Player {
        player_of_height(y, HUMAN_MALE)
    }

    fn player_of_height(y: f32, h: f32) -> Player {
        Player {
            pos: Vec3::new(0.0, y, 0.0),
            collision_height: crate::entities::CollisionHeight(h),
            ..Default::default()
        }
    }

    /// The ENTER gate's three bits, named — `0x6030c0`'s `js` / `shr 3` / `shr 0xb` tests.
    /// PET_IN_COMBAT is the one that reads like a typo and is not: the reference really does let a
    /// pet's combat bit admit it to swim, and it is bit 11 of `UNIT_FIELD_FLAGS` in 1.12.1.
    #[test]
    fn the_swim_enter_gate_is_those_three_unit_flags_and_no_others() {
        const PLAYER_CONTROLLED: u32 = 0x8;
        const PET_IN_COMBAT: u32 = 0x800;
        const USE_SWIM_ANIMATION: u32 = 0x8000;
        assert_eq!(
            SWIM_ENTER_UNIT_FLAGS,
            PLAYER_CONTROLLED | PET_IN_COMBAT | USE_SWIM_ANIMATION
        );
        for bit in [PLAYER_CONTROLLED, PET_IN_COMBAT, USE_SWIM_ANIMATION] {
            assert!(
                may_enter_swim(bit),
                "any ONE of the three admits ({bit:#x})"
            );
        }
        assert!(!may_enter_swim(0), "no flags at all → walks the lakebed");
        // Not vacuous: a unit carrying every OTHER unit flag still cannot enter. IN_COMBAT,
        // SKINNABLE, PVP, NOT_SELECTABLE — none of them is a swim permit.
        assert!(!may_enter_swim(!SWIM_ENTER_UNIT_FLAGS));
    }

    /// Swim mode latches with the verified 1/36-yd hysteresis: enter is a strict `depth > 0.75·h`, leave
    /// is `depth < 0.75·h − 1/36`, so a depth inside the band holds whatever state we were in — wading
    /// the boundary can't flicker the physics regime frame to frame (wow-re swim-transition `0x6030c0`).
    #[test]
    fn swim_entry_and_exit_hysteresis() {
        // The band is exactly 1/36 yd, independent of height — so it holds at every race's line.
        for h in [HUMAN_MALE, GNOME_FEMALE, NIGHT_ELF_MALE] {
            assert!((swim_enter_depth(h) - swim_exit_depth(h) - 1.0 / 36.0).abs() < 1e-6);
            // Enter is 0.75·collisionHeight from the feet.
            assert!((swim_enter_depth(h) - 0.75 * h).abs() < 1e-6);
        }

        let (enter, exit) = (swim_enter_depth(HUMAN_MALE), swim_exit_depth(HUMAN_MALE));
        let mut p = player_at(0.0); // feet at y=0, so the surface height *is* the submersion depth
        let mid_band = (enter + exit) * 0.5;
        assert!(
            !update_swimming(&mut p, Some(enter), 0.0),
            "exactly at the enter depth does not yet swim (strict >)"
        );
        assert!(
            !update_swimming(&mut p, Some(mid_band), 0.0),
            "a depth in the band, entered from walking, stays walking"
        );
        assert!(
            update_swimming(&mut p, Some(enter + 0.01), 0.0),
            "past the enter depth starts swimming"
        );
        assert!(
            update_swimming(&mut p, Some(mid_band), 0.0),
            "the same in-band depth, now entered from swimming, keeps swimming (hysteresis)"
        );
        assert!(
            !update_swimming(&mut p, Some(exit - 0.01), 0.0),
            "dropping below the leave threshold returns to walking"
        );
        // Re-enter, then prove no-liquid stops it regardless of depth.
        assert!(update_swimming(&mut p, Some(enter + 0.5), 0.0));
        assert!(
            !update_swimming(&mut p, None, 0.0),
            "no liquid over the feet stops swimming (the binary's inLiquid == 0 → STOP)"
        );
    }

    /// **GM flight is the suppression, not a lift** (decision 0726). `LEVITATING` bails the whole
    /// water decision (`0x6030d2 test ah,4`), so the two things that would otherwise happen the very
    /// next frame both stop happening: dry land cannot clear a server-granted swim (which is what
    /// keeps you airborne), and deep water cannot grant one (the same bail, the other arm).
    #[test]
    fn levitating_bails_the_water_decision_in_both_directions() {
        // The server just sent `.cheat fly on`: SWIMMING + LEVITATING, standing on dry ground.
        let mut flying = player_at(0.0);
        flying.swimming = true;
        flying.modes.levitating = true;
        assert!(
            update_swimming(&mut flying, None, 0.0),
            "no liquid at all must NOT stop a levitating swimmer — this is the whole of GM flight"
        );
        assert!(
            update_swimming(&mut flying, Some(-50.0), 0.0),
            "nor may a surface far below the feet, which is what flying over a lake looks like"
        );

        // The other arm, and it is the same instruction: water cannot latch swim while levitating.
        let mut dry = player_at(0.0);
        dry.modes.levitating = true;
        assert!(
            !update_swimming(&mut dry, Some(swim_enter_depth(HUMAN_MALE) + 1.0), 0.0),
            "the ENTER arm is skipped too — the bail is before the branch, not inside it"
        );

        // And clearing the mode (`.cheat fly off` sends flags 0) hands the water its job back.
        flying.modes.levitating = false;
        assert!(
            !update_swimming(&mut flying, None, 0.0),
            "with the mode gone, no liquid stops swimming again"
        );
    }

    /// The latch never touches the settle hold (decision 0737): the release is the terrain
    /// streamer's, keyed on residency in every mover mode alike. This pins the *absence* of the
    /// old special case — a swim latch that cleared `settling` here would race the streamer's
    /// judgement (the pre-0737 rule existed only because the old release was a walk-mover ground
    /// probe a swimmer never reached; the Booty Bay underwater-login hang, 2026-07-18).
    #[test]
    fn the_latch_leaves_the_settle_hold_alone() {
        for (surface, name) in [
            (Some(swim_enter_depth(HUMAN_MALE) + 1.0), "swim depth"),
            (Some(swim_exit_depth(HUMAN_MALE) - 0.5), "wading depth"),
            (None, "dry land"),
        ] {
            let mut p = player_at(0.0);
            p.settling = true;
            update_swimming(&mut p, surface, 0.0);
            assert!(p.settling, "{name} must leave the hold to the streamer");
        }
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
        let deep = Some(swim_enter_depth(HUMAN_MALE) + 1.0);
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

    /// The rest line is a **constraint**, not a one-way cap (decision 0644): a stroking swimmer
    /// left above it settles back onto it, while one below it is never pulled up — the verified
    /// floating resolver has no upward force but your own stroke, and no downward one at all.
    #[test]
    fn the_rest_line_is_satisfied_from_above_and_never_pulls_up() {
        // The law is the same shape at every body height; run it at all three.
        for h in [HUMAN_MALE, GNOME_FEMALE, NIGHT_ELF_MALE] {
            let cap = rest_cap(h);
            // Exactly on the line: nothing to do.
            assert_eq!(settle_to_rest(10.0 - cap, 10.0, h), 0.0);
            // Below it — a dive, or a river swum upstream: no pull up at any depth.
            assert_eq!(settle_to_rest(10.0 - cap - 0.01, 10.0, h), 0.0);
            assert_eq!(settle_to_rest(10.0 - cap - 30.0, 10.0, h), 0.0);
            // Above it, which on a river is the surface having come down to meet the feet: the
            // sink is exactly the excess, so one satisfied frame lands on the line, not past it.
            let excess = 0.25;
            assert!((settle_to_rest(10.0 - cap + excess, 10.0, h) - excess).abs() < 1e-6);
        }
    }

    /// **B128, pinned.** `.gm fly` over water yanked the avatar down onto the waterline instantly,
    /// from any height (two reporters). Cause: the rest line's two arms read different sources —
    /// [`rest_line`] exempted flight from the *cap*, while the *settle* re-read the liquid itself.
    /// This pins the exemption at its single source, in both directions, so the two can't diverge
    /// again: while levitating there is no line at all, and the moment the mode clears the water
    /// has its authority back.
    #[test]
    fn gm_flight_hands_the_water_no_authority_over_the_vertical() {
        let lake_surface = 40.0;

        // 300 yd above a lake, `.gm fly` on. This is the exact geometry of B128: liquid is over our
        // XY (the query answers `Some`), and the drop the settle would compute is the whole
        // altitude — through open air, so nothing stops the cast and one frame lands us in the lake.
        let mut flying = player_at(lake_surface + 300.0);
        flying.swimming = true;
        flying.modes.levitating = true;
        assert_eq!(
            rest_line(&flying, Some(lake_surface)),
            None,
            "no rest line while levitating — this is the gate BOTH arms take, not just the cap"
        );
        // What the settle would have done had it reached its arithmetic: teleport us 300 yd down.
        // Asserted so the fix is measured against the defect rather than merely asserted away.
        assert!(
            settle_to_rest(flying.pos.y, lake_surface, flying.collision_height.0) > 299.0,
            "the ungated settle's drop is the whole altitude — that was the yank"
        );

        // Clear the mode (`.gm fly off` sends flags 0) and the water is back in charge immediately.
        flying.modes.levitating = false;
        assert_eq!(rest_line(&flying, Some(lake_surface)), Some(lake_surface));

        // And the case flight must not be confused with: a *momentary* surface-query miss in real
        // water is a rest line at our own feet — the avatar holds for the frame, it does not fly.
        let swimmer = player_at(lake_surface - 2.0);
        assert_eq!(
            rest_line(&swimmer, None),
            Some(swimmer.pos.y),
            "a chunk-seam miss must stay a Some, or it is indistinguishable from GM flight"
        );
    }

    /// **B76, pinned.** The bug the director reported: a gnome could not swim up to the surface,
    /// her head stayed well under. The rest line is `0.75·h` below the waterline, so the head
    /// clears it iff `0.75·h < h` — trivially true *for the unit's own h*, and false the moment
    /// one body's h is used for another's. With the old constant a gnome female (1.15 yd tall)
    /// was held 1.52 yd down: 0.37 yd of water **above her head**, no stroke able to reach it,
    /// because the cap is a hard collision plane and the resolver has no upward force but the cap.
    ///
    /// Asserted as "every race floats with its head out", which is the property that actually
    /// matters and which no single constant can satisfy for all of them.
    #[test]
    fn every_race_floats_with_its_head_out_of_the_water() {
        for (label, h) in [
            ("human male", HUMAN_MALE),
            ("gnome female", GNOME_FEMALE),
            ("night elf male", NIGHT_ELF_MALE),
        ] {
            let submerged = rest_cap(h);
            assert!(
                submerged < h,
                "{label}: rest line {submerged} is at or below the top of a {h}-yd body"
            );
            // A quarter of the body clears the water — the verified 1−0.75.
            assert!(((h - submerged) / h - 0.25).abs() < 1e-6, "{label}");
        }

        // And the shape of the defect itself, as the loop above would have run it pre-0645: the
        // one constant applied to a gnome puts the rest line ABOVE her head, so `submerged < h`
        // fails and she is held under with water to spare. This is what the fix removes.
        let pre_0645 = rest_cap(crate::player::DEFAULT_COLLISION_HEIGHT);
        assert!(
            pre_0645 > GNOME_FEMALE,
            "the old constant rest line ({pre_0645}) must sit above a {GNOME_FEMALE}-yd gnome — \
             that was the bug, and if this ever fails the test below is no longer testing it"
        );
        assert!(
            pre_0645 - GNOME_FEMALE > 0.3,
            "…by a third of a yard of water over her head, not a rounding error"
        );
    }

    /// **The director's downhill-river jitter** (decision 0644), as the frame loop that produces
    /// it. `SLOPE` is Felwood's Felfire Hill channel, measured off the shipped ADT and pinned by
    /// `liquid::real_data::the_felfire_channel_falls_about_a_tenth_of_a_yard_per_yard`.
    ///
    /// The two halves are the two vertical laws over the same river. Frozen feet cannot hold the
    /// latch for a tenth of a second — the surface descends through the whole 1/36-yd hysteresis
    /// band almost at once, and every crossing hands the avatar to the fall mover and back
    /// (live-probe measured at ~10 mode flips/s, 47% of the swim spent falling). Satisfying the
    /// constraint instead pins the depth on the rest line for a run longer than any river.
    #[test]
    fn a_descending_surface_does_not_flap_the_swim_latch() {
        const SLOPE: f32 = 0.099;
        const DT: f32 = 1.0 / 60.0;
        const H: f32 = HUMAN_MALE;
        let cap = rest_cap(H);
        let surface_after = |secs: f32| 100.0 - SLOPE * SWIM_SPEED * secs;

        // (a) The pre-0644 law — the feet keep their Y while the water leaves them behind.
        let mut frozen = player_of_height(surface_after(0.0) - cap, H);
        frozen.swimming = true;
        let mut left_at = None;
        for i in 0..600 {
            let t = i as f32 * DT;
            if !update_swimming(&mut frozen, Some(surface_after(t)), t) {
                left_at = Some(t);
                break;
            }
        }
        let left_at = left_at.expect("frozen feet cannot hold the latch on a slope");
        assert!(
            left_at < 0.1,
            "the whole 1/36-yd band is spent in {left_at:.3}s of downhill swimming"
        );

        // (b) The shipped law. The settle runs where `swim_step` runs it — after the stroke, on
        // the surface at the position the stroke reached.
        let mut held = player_of_height(surface_after(0.0) - cap, H);
        held.swimming = true;
        for i in 0..600 {
            let t = i as f32 * DT;
            let surface = surface_after(t);
            held.pos.y -= settle_to_rest(held.pos.y, surface, H);
            assert!(
                update_swimming(&mut held, Some(surface), t),
                "left swim at frame {i} ({t:.2}s)"
            );
            assert!(
                (surface - held.pos.y - cap).abs() < 1e-4,
                "frame {i}: depth {} is off the rest line",
                surface - held.pos.y
            );
        }
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
