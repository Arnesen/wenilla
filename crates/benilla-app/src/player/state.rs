//! The avatar's shared state + the movement constants — [`Player`] (the controller's resource),
//! its transport attachment ([`PlayerRide`]), the capsule/speed resources, and every binary-derived
//! movement constant the controller family reads. Pure state, no systems: [`super::control`] and
//! the concern modules beside it (`mover`, `swim`, `arc`, `movement_net`, …) all borrow from here.

use avian3d::prelude::*;
use benilla_protocol::MoveMode;
use bevy::prelude::*;

/// Backpedal speed as a fraction of run: vanilla `MOVE_RUN_BACK` 4.5 / `MOVE_RUN` 7.0. Moving backward
/// (the `0x2` backward move-flag) selects the backward speed over run, **dominating strafe** —
/// binary-VERIFIED 1v1 (`backward-speed-{a,b}.md`; the speed getter `FUN_007c4c90` tests only bit `0x2`
/// on the forward/back axis, never the forward/strafe bits) — and the backward arm is a
/// **`min(runBack, run)`**, not a plain select (`0x7c4d1d`, the swim-feel §5's TU-H; identical in
/// template to the swim pair's `min(swimBack, swim)`). runBack is server-seeded (the ctor zeroes it;
/// standard vanilla 4.5), so we keep it as a ratio of the configured run speed → a `$WOW_MOVE_SPEED`
/// override / Ctrl sprint scales backpedal too. A backward *jump* inherits this automatically: takeoff
/// freezes the current horizontal speed (`FUN_007c61f0` never rewrites it), so a backward jump lands
/// ~36% shorter — no separate constant.
pub(super) const RUN_BACK_RATIO: f32 = 4.5 / 7.0;

/// Character turn rate (rad/s) — how fast A/D rotate the avatar's facing when not mouse-looking (the
/// vanilla turn-vs-strafe model, VERIFIED wow-5875-re `0x7c4f30` heading integrate). It is the unit's
/// 6th movement speed (`CMovement+0x9c`, the server-seeded `TURN_RATE`; vanilla default ≈π rad/s),
/// reduced to 0.75× while also translating (the `flags & 0x200f` case). Decision 0050.
pub(super) const TURN_RATE: f32 = std::f32::consts::PI;

/// The mouselook swim-pitch clamp (radians) — **VERIFIED** ±89.0° = 1.5533431 (`0x8089d8` =
/// `0x3fc6d3f2`, the camera→SetPitch path's clamp; wow-re `swim-camera-pitch.md`, decision
/// 0492). NOT ±π/2 — that clamp belongs to the separate, rate-limited pitch-KEY integrator
/// (`0x7c4f80`), whose keys are default-unbound and which we don't bind.
pub(super) const MOUSELOOK_PITCH_CLAMP: f32 = 1.553_343;
/// Turn-rate scale while also translating (moving/strafing) — the verified `×0.75` (`flags & 0x200f`).
pub(super) const TURN_RATE_MOVING: f32 = 0.75;

/// The stationary body catch-up: once steering input stops, the rendered body closes on the aim at
/// `turnRate × 8` rad/s, gap-clamped (the client's chase, `0x607ed0` tail — its clock is stamped
/// every non-steering frame, so elapsed ≈ one frame). While steering, the catch-up is FROZEN and
/// only the 90° ceiling moves the body — the head-leads-then-body-follows turn-in-place (wow-re
/// `b947e5aa`, decision 0106).
pub(super) const STATIONARY_CHASE_RATE: f32 = 8.0;

// ── Character-controller feel knobs (decision 0009) ──────────────────────────────────────────────
// These are binary-derived values kept because they give the WoW feel cheaply — *tunables*, not
// fidelity targets. The mechanism is a thin kinematic controller over avian's `MoveAndSlide`; further
// refinements (accel/decel curves, partial air control beyond the one-shot nudge) dial up from here.

/// Player capsule radius (yd) — the vanilla `CMovement` ctor's ±1/3 half-width, and a *tunable* per
/// the block header above, not a fidelity target.
///
/// Known since to be the ctor **placeholder**, not the real mover's radius: `0x6174b0` overwrites
/// `+0xb0`/`+0xb4` from `CreatureModelData` on every model build (`0x5fb880` → `0x5fb9dd` passes
/// `force = 1`, which skips the only refusal path), so the live extents are per-model — human male
/// radius **0.30555**, human female **0.20835** (VERIFIED, wow-re `mover-collision-scalars.md`;
/// decision 1125). Adopting them is the same movement-fidelity question [`CAPSULE_HEIGHT`] describes
/// below, and this radius is its other half.
pub(super) const CAPSULE_RADIUS: f32 = 1.0 / 3.0;
/// Player capsule total height (yd) — the **movement** capsule avian sweeps, deliberately a
/// constant. Numerically the vanilla ctor-default collision height it was derived from
/// ([`DEFAULT_COLLISION_HEIGHT`]), but it is not the same quantity and no longer stands in for one:
/// a unit's real collision height is per-model and lives on [`crate::entities::CollisionHeight`]
/// (decision 0645). This one is a *feel knob* per the block header above — it feeds the swept box,
/// the step-vs-fall election's reach and the head/feet offsets, where going per-race would change
/// where every short race can walk, step and fit. That is a movement-fidelity question of its own
/// (our kinematic capsule vs the reference's k-DOP), not the depth-line question 0645 settled, so
/// the two are kept apart on purpose rather than by oversight.
pub(crate) const CAPSULE_HEIGHT: f32 = 2.027_777_7;

/// The client's own **empty-world collision height** (yd) — the `CMovement` ctor's immediate
/// `0x4001c71c` at `0x616fd8` (VERIFIED, wow-re `collision.md` "the collision volume"), which the
/// per-unit param setter `0x6174b0` overwrites from the unit's model. This is the fallback every
/// depth line takes for a unit whose display id doesn't resolve to a `CreatureModelData` row — the
/// same role it plays in the reference, and what vmangos falls back to as well (its `2.f`).
///
/// It is **not** a stand-in for a real unit's height: see [`crate::entities::CollisionHeight`].
pub(crate) const DEFAULT_COLLISION_HEIGHT: f32 = 2.027_777_7;
/// Downward gravity (yd/s²) — binary-VERIFIED vanilla value (set on avian's `Gravity` too; matches
/// vmangos `Movement::gravity` exactly). Shared with the remote dead-reckoner ([`crate::net`]), which
/// integrates a relayed jump's arc under the same gravity so an observer's view matches the mover's.
pub(crate) const GRAVITY: f32 = 19.291_105;
/// Jump take-off speed (yd/s) — binary-VERIFIED vanilla value.
pub(super) const JUMP_SPEED: f32 = 7.955_547;
/// Terminal fall speed (yd/s) — binary-VERIFIED vanilla value (matches vmangos `terminalVelocity`).
/// Shared with [`crate::net`]'s ballistic integration (caps a long fall's vertical speed).
pub(crate) const TERMINAL_VELOCITY: f32 = 60.148_003;
/// **Terminal fall speed under feather fall** (yd/s) — the *whole* of what Slow Fall does. The
/// reference's gravity integrate `0x7c5d20` picks its clamp from one flag test
/// (`0x7c5d23 test [ecx+0x40], 0x20000000`): the ordinary cap `[0x87d894]` = 60.148, or this one
/// `[0x87d898]` when `MOVEFLAG_SAFE_FALL` is set. VERIFIED — wow-re `system/collision/ledger.tsv`
/// (`0x7c5d20`, sweep2 §5) and `scratch/spec-ground.md`'s terminal-vel select.
///
/// The same 7.0 shows up on the server: vmangos raises its anticheat's expected jump speed to
/// `max(current, 7.0f)` on receiving a feather-fall ack (`HandleMovementFlagChangeToggleAck:576`) —
/// it is matching this clamp.
pub(crate) const FEATHER_TERMINAL_VELOCITY: f32 = 7.0;
/// **How far above the ground a hovering body rests** (yd) — the whole of what Hover does. The
/// reference extends the walk resolver's down-probe by `[0x7ff9d8]` = 1.0 (`0x636dd2`) and then
/// subtracts it back out of the snap, landing the body exactly a yard clear. VERIFIED — wow-re
/// `scratch/moveflag-family.md` §4, which settled it as a **direct write to `CMovement.pos.z`**
/// rather than a probe widening, closing `step-vs-fall-election.md`'s open bit-identity handoff.
pub(crate) const HOVER_HEIGHT: f32 = 1.0;
/// **How fast a hovering body rises to that clearance** (yd/s). The snap itself can only *lower* the
/// body — `pos.z −= max(L − 1.0, 0)` (`0x636e52`–`0x636ea9`) skips its write entirely when the floor
/// is already within a yard — so the rise is a second, rate-limited pass at `0x636fa1`–`0x6370f1`
/// whose rate comes from `0x7c61b0` returning `[0x87d898]`: the very same 7.0 the feather-fall clamp
/// reads. Without it a granted hover reads as an instant pop instead of a float.
pub(crate) const HOVER_CLIMB_RATE: f32 = 7.0;
/// Standability gate: a surface is walkable iff its normal is within ~50° of straight up (cos 50° —
/// the vanilla threshold). Steeper than this you can't climb and you slide back down.
pub(super) const GROUND_COS: f32 = 0.642_788;
/// Downward probe distance (yd) to decide whether we're standing on ground.
pub(super) const GROUND_PROBE: f32 = 0.2;
/// The post-move downward snap's **slope ratio** — the client's step-vs-fall election
/// (`0x6367b0`, constant `[0x80c740]` = 1.8493990; wow-re `step-vs-fall-election.md`): the snap
/// probe reaches `d_h · ratio + slack + collision height` below the post-move position, where
/// `d_h` is the frame's achieved horizontal travel. Scaling by the travel makes the absorbed
/// *slope* the constant (atan 1.8494 ≈ 61.6°, comfortably above the 50° walkable limit),
/// frame-rate independent; the collision-height term (our [`CAPSULE_HEIGHT`] — decision 0182) is
/// what absorbs a discrete ledge: a fence-height drop is a silent straight-down step, only a deeper
/// floor becomes a fall. That term is a **known deviation** on two counts — the reference's `H` is
/// 1.0, not 2.028, and its extension is `0x4000000`-gated rather than always-on (decision 1125).
///
/// The ratio itself is confirmed to be one constant with two jobs, exactly as we use it: `0x80c740`
/// has four references image-wide, and they are the foot cone's waist ring (`0x631c0b`) and this
/// step-down term (`0x636dda`, inside `0x6367b0`) among them — the same 61.6° serving the climb and
/// the descent (wow-re `mover-collision-scalars.md`).
pub(super) const STEP_SLOPE_RATIO: f32 = 1.849_399;
/// The election's fixed slack (yd) added to the travel-scaled snap reach — `[0x7ff9d0]` = 1/36 yd.
pub(super) const STEP_SNAP_SLACK: f32 = 0.027_777_8;
/// The step-up **rise ceiling** (yd): how tall an obstacle the atomic step-up can walk you onto —
/// the reference's own `H`, VERIFIED (decision 1126).
///
/// 0209 set this to 0.7 as a deliberately modest tunable, on the understanding that the reference's
/// budget was a ~2 yd body height we did not want. That understanding was wrong on its central fact:
/// `0x617430` returns `CMovement+0xb8`, which is **not a height** but the dimensionless ratio
/// `max(SCALE_X / CreatureModelScale, 1)`, and a complete writer census puts it at **1.0** for a
/// clean local player (decision 1125). The reference's rise budget was never 2 — it was 1.0, and the
/// gap we were preserving was 0.7 vs 1.0, not 0.7 vs 2.
///
/// 0209's invariant is untouched by closing it: a fence's collision top is 1.8–2.3 yd, so fences,
/// trunks and walls still slide at exactly the height they always did. What changes is the class
/// 0209's note invited us to fix — "one number to nudge if a real spot feels too restrictive" —
/// which the director's captures found twice: a 0.91 yd Stormwind step (`14282v1`, a 66° face onto a
/// flat top) and 1121's deferred 1.04 yd ledge.
pub(super) const STEP_UP_HEIGHT: f32 = 1.0;
/// The step-up **certify advance** (yd): how far ahead the maneuver looks for the tread it would
/// stand on. A property of the BODY, never of the frame (decision 1121).
///
/// 0209 advanced by this frame's own travel, which made every kerb in the game a frame-rate
/// lottery: at 60 fps a run is 0.117 yd of travel, at 144 fps it is 0.049, and walking is 0.041 —
/// while the capsule's own radius is [`CAPSULE_RADIUS`]. A settle probe that far forward is still
/// over the riser, so it lands on the riser's face and the walkable gate rejects it. Measured at
/// Stormwind's Trade District kerbs (decision 1121): the live 0.117 rung fails, 0.20 and beyond
/// commit onto the tread, at every one of the seven captured contacts. Sidewalks 0.28 yd tall
/// refused to be stepped onto because the probe never reached them.
///
/// A body radius was the principled *guess* — exactly the distance at which the capsule's footprint
/// has cleared the lip it is standing against, and the same number whatever the frame rate or the
/// gait. It is now superseded by the measured one (decision 1126): the reference steps
/// `max(H·tan50°, radius + 1/720)` square into the certified face, which with the verified `H` of 1.0
/// is **1.1918 yd** — three and a half times our radius. 1121 reached for a body-scaled number in the
/// absence of this one and got the *shape* of the answer right; the magnitude was never ours to
/// invent. The frame's travel still wins when it is longer, so this remains a floor, not a fixed
/// reach.
///
/// It does not widen what may be climbed, and that argument is what makes the larger reach safe: the
/// rise ceiling is still [`STEP_UP_HEIGHT`], the settle must still find a *walkable* floor higher
/// than the feet, and — the load-bearing part — **the elevated forward sweep is clipped by anything
/// in the way**, so the advance only ever reaches as far as there is clear air at the raised height.
/// A fence, a trunk and a two-trunk pinch still block at the same body height, with a clipped advance
/// and a net-zero settle. What the longer reach buys is the case the director's capture found: a step
/// whose *face* is unwalkable for most of a yard before the flat top begins, where a probe that stops
/// at one radius is still over the face and reads it as a steep floor.
pub(super) const STEP_UP_ADVANCE: f32 = 1.191_753_6;
/// The **foot cone's height** (yd): how far above the feet the reference's movement solid is still
/// narrower than its full radius — and therefore the band within which a blocking edge is *ridden
/// up* rather than stepped onto (decision 1123).
///
/// The real client's movement solid is not a capsule. Its lower half is a **cone**: the k-DOP build
/// at `0x631440` emits 9 planes — 4 vertical box sides at `center.xy ± radius`, and 4 bevels running
/// from a point at the foot out to `(pos.xy ± radius, pos.z + r')` with `r' = radius · 1.8493990`
/// (`[0x80c740]` — our [`STEP_SLOPE_RATIO`]), above which it is a plain vertical box. There is no
/// −Z plane: the solid is open at the bottom (wow-re `climb-vs-slide.md` §2).
///
/// So a *low* edge never meets a wall — it meets the slanted skirt, and the resolver's ordinary
/// slide carries the body up it at the cone's own surface slope, atan 1.8494 ≈ 61.6°, gaining
/// `1.8494 · cosθ · len` per step (the note's `T` at §4). Only an edge **above** this height meets
/// the vertical box square, and that is the case the instant step-up ([`STEP_UP_HEIGHT`]) exists
/// for. One solid, two behaviours, selected by the edge's height above the feet.
///
/// Not a new tunable: it is our own two constants multiplied, so it is *our* mover's cone — the band
/// is a property of the body's radius, and ours is [`CAPSULE_RADIUS`].
///
/// It is **not** the real client's band, and the gap is recorded rather than papered over (decision
/// 1125, correcting 1123's claim that it matched exactly). Because [`CAPSULE_RADIUS`] is the ctor
/// placeholder, ours comes out at 0.616 against a live human male's **0.565** and a human female's
/// **0.385** — ~9% and ~60% generous. It only decides ride-vs-pop for obstacles *between* the two
/// bands, so the cost is a narrow slice of over-smooth rides on tall-ish steps; closing it properly
/// means per-model mover extents, which moves far more than this constant.
pub(super) const FOOT_CONE_HEIGHT: f32 = CAPSULE_RADIUS * STEP_SLOPE_RATIO;
/// The landing probe (yd): while airborne, walk mode resumes only this close to the floor, so
/// the arc ends where the slide actually contacts (skin scale) instead of [`GROUND_PROBE`]
/// early — which cut the last ~0.2 yd of every fall into a same-frame snap, the visible pop at
/// every silent landing (decision 0190).
pub(super) const LAND_PROBE: f32 = 0.05;
/// Wedge-rest detection (decisions 0211/0212): a "fall" that is no longer falling. A capsule can
/// come to rest held between two steep faces — the flaring trunk bases at the Northshire trees
/// form exactly this funnel (contact normals ~0.2 up) — where gravity feeds the slide, the
/// opposing contacts cancel it, and with mid-air control locked (vanilla momentum rules) the
/// falling pose is permanent. This many consecutive *stalled* frames (see
/// [`WEDGE_STALL_RATIO`]) is unambiguously a rest — land there: the fall ends, walking control
/// returns, and stepping off the support resumes a normal fall. Nothing becomes walkable or
/// climbable by this.
pub(super) const WEDGE_STILL_FRAMES: u8 = 3;
/// A frame counts as stalled when the achieved descent is under this fraction of the descent
/// gravity intended (`vel_y·dt`), already falling faster than [`WEDGE_MIN_FALL`]. Free fall
/// achieves ~100% and a steep-slope slide ≥75% (the steeper the face, the *freer* the
/// vertical), so only opposing contacts hold an arc under this — and because the intent keeps
/// growing while the funnel eats the motion, the pinch-in registers the frame it starts
/// instead of after a visible millimeter-creep tail in the falling pose (0211's absolute
/// stillness test — decision 0212).
pub(super) const WEDGE_STALL_RATIO: f32 = 0.15;
/// Fall speed (yd/s) the arc must exceed before stalled frames count: a jump apex hovers near 0
/// and never qualifies; a wedge accumulates gravity while frozen and passes within a few frames.
pub(super) const WEDGE_MIN_FALL: f32 = 1.0;
/// One-shot air-control nudge (yd/s): a jump from a standstill can be steered this much in the pressed
/// direction; a jump taken with momentum keeps it locked (vanilla feel). Less than a walking jump.
pub(super) const AIR_NUDGE_SPEED: f32 = 2.5;
/// The FALLINGFAR **distance leg** (yd): a *jump* arc (launch vz ≠ 0) latches MOVEFLAG_FALLINGFAR
/// once it descends this far below its launch height — the fall resolver's `0x633240`, constant
/// `[0x80dff8]` = 1/9 yd (wow-re `land-anim-height-gate.md`). Latched, the arc is a **far fall**:
/// the anim layer swaps to Fall(40) mid-air. A flat jump never descends below its takeoff, so it
/// never latches — its hang stays Jump(38). The legs are exclusive on the launch vz: step-off
/// falls take [`FALL_FAR_TIME`] instead (decision 0179).
pub(super) const FALL_FAR_DROP: f32 = 0.111_11;
/// The FALLINGFAR **timer leg** (s): a *step-off fall* (launch vz = 0 — the walk election's
/// `StartFalling(0)`) latches once airborne this long — `0x633240`'s accumulator test,
/// `0x1f4` = 500 ms. Free-falling from rest that is ≈ 2.41 yd of descent. Since the election
/// absorbs anything up to ~collision height as a step (decision 0182), elected step-off falls
/// start just under this: a wagon-height drop crosses it a frame or two before the floor.
pub(super) const FALL_FAR_TIME: f32 = 0.5;
/// Skin width (yd) kept between the capsule and geometry on casts.
pub(super) const SKIN_WIDTH: f32 = 0.02;

/// Max seconds to hold the avatar after a teleport while the world streams in (see [`Player::settling`]).
/// Generous — a dense city's spawn + collider queue drains in a couple seconds; this only backstops a
/// world that never becomes resident (missing data, a stalled stream) so we never hang forever. The
/// release itself is the terrain streamer's (decision 0737), which also pushes the deadline while the
/// resident world is still the departed map's (0710's fail-closed law).
pub(crate) const SETTLE_TIMEOUT: f32 = 6.0;

/// The player body's collision capsule, built once at startup and swept by avian's `MoveAndSlide`
/// each frame. Its origin is the capsule centre; the player's `pos` is its feet (centre −
/// half-height·Y).
///
/// **Every player body, not only ours.** A remote mover's dead-reckon sweeps this same capsule
/// against the same colliders ([`super::mover::grounded_step`]) — the reference drives non-local
/// units through the *same* movement controller as the local player (decision 0059's byte trail:
/// `0x616620` integrates any mover; the local-player GUID compare gates only a timing budget). The
/// reference reads each unit's own collision height (`[unit+0xb8]`); we use the one player capsule
/// for every player body, which the 1.12 races are close enough to share.
#[derive(Resource)]
pub(crate) struct PlayerCapsule(pub(crate) Collider);

/// The avatar run-speed fallback + dev override. `value` is `$WOW_MOVE_SPEED` when set
/// (`env_override` — the absolute dev knob, backpedal scaled by [`RUN_BACK_RATIO`] under it), else
/// the vanilla 7.0 used only until the server's own speeds stream in: the self create's `LIVING`
/// block seeds [`crate::net::UnitSpeeds`], and every `SMSG_FORCE_*_SPEED_CHANGE` updates it live —
/// the controller reads those as the authoritative run/runback/swim speeds.
#[derive(Resource)]
pub(super) struct MoveSpeed {
    pub(super) value: f32,
    pub(super) env_override: bool,
}

/// **The granted mover modes — one system, five bits** (decision 0866).
///
/// A *mode* is a `MOVEMENTFLAGS` bit the **server grants** that changes how our mover behaves rather
/// than where it is heading. They live here as typed fields, not as bits in
/// [`Player::move_flags`], for the reason decision 0726 first hit: `move_flags` is last-streamed
/// wire bookkeeping, rebuilt from state every frame, so a mode parked there alone would be gone
/// before the mover ever read it. The flag word is *rebuilt from these fields* each frame instead —
/// which is also what keeps the server's copy of our mode alive (drop the bit from our stream and
/// the next server-authored move echoes a mode-less word back and clears it under us).
///
/// **Two arrival routes, and the difference matters.** Four of the five come through the *ack'd*
/// family ([`benilla_protocol::MoveMode`]) — an addressed opcode we must answer with the echoed
/// counter or the grant never lands. [`Self::levitating`] does not: it arrives merged out of a
/// server-authored `MSG_MOVE_*` (the `SERVER_AUTHORED` mask), unhandshaked.
///
/// **Each one's effect is the reference's, byte-verified** — the addresses are in
/// [`benilla_protocol::MoveMode`]'s table; here is where each is consumed:
///
/// - [`rooted`](Self::rooted) — translation and jumps die; **turning stays live** ([`super::control`]).
/// - [`feather_fall`](Self::feather_fall) — terminal fall speed becomes [`FEATHER_TERMINAL_VELOCITY`]
///   ([`super::mover::step`]).
/// - [`hover`](Self::hover) — ground contact rises by [`HOVER_HEIGHT`] ([`super::mover::step`]).
/// - [`water_walking`](Self::water_walking) — the liquid surface is walkable ground, and the swim
///   latch cannot arm ([`super::swim`]).
/// - [`levitating`](Self::levitating) — the swim/depth decision is suppressed entirely
///   ([`super::swim::update_swimming`]).
///
/// **Levitate (spell 1706) grants three at once** — feather fall + hover + water walk — which is why
/// they are one struct rather than four unrelated booleans.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct MoveModes {
    /// The server rooted our mover (`SMSG_FORCE_MOVE_ROOT` — death until release, and every root or
    /// stun; decision 0308). Translation input and jumps are dead — the faithful "can't move between
    /// death and release" — but **turning stays live, and by design rather than by omission**
    /// (corrected by decision 0872): the reference's input tick consults an allow-list
    /// (`0x615c71` → the byte table at `0x618054`) that blocks the translation command ids and
    /// **explicitly permits** turn, pitch, run/walk and SetFacing. `SetRoot 0x7c7340` additionally
    /// clears the direction bits once, at apply. A character who cannot even pivot is *stunned* —
    /// a separate `UNIT_FIELD_FLAGS` gate ([`crate::player::UNIT_FLAG_STUNNED`]).
    pub(crate) rooted: bool,
    /// Water-walking (`SMSG_MOVE_WATER_WALK`, `SPELL_AURA_WATER_WALK` — Water Walking, Levitate, and
    /// the ghost form): the liquid surface counts as walkable ground, so we stand on it instead of
    /// sinking, and the swim latch cannot arm underneath us.
    pub(crate) water_walking: bool,
    /// Feather fall (`SMSG_MOVE_FEATHER_FALL`, `SPELL_AURA_FEATHER_FALL` — Slow Fall, Levitate): the
    /// fall integrator's terminal velocity drops to [`FEATHER_TERMINAL_VELOCITY`]. Nothing else
    /// changes — the arc, the flags and the landing report are the ordinary ones.
    pub(crate) feather_fall: bool,
    /// Hover (`SMSG_MOVE_SET_HOVER`, `SPELL_AURA_HOVER` — Levitate): ground contact sits
    /// [`HOVER_HEIGHT`] above the surface, so the body floats and walks along a yard up.
    pub(crate) hover: bool,
    /// **Free flight** (`MOVEFLAG_LEVITATING`, GM `.cheat fly` — decision 0726). The one mode that
    /// arrives *unhandshaked*, merged out of a server-authored move ([`super::wire_in`]).
    ///
    /// It does exactly one thing, and it does it by suppression: while set, the water/depth swim
    /// decision does not run at all ([`super::swim::update_swimming`] — the reference's
    /// `0x6030d2 test ah,4` bail). So a [`Player::swimming`] the server switched on stays on with no
    /// water under it — which *is* GM flight — and, symmetrically, real water can no longer put us
    /// into swim while it is set.
    pub(crate) levitating: bool,
}

impl MoveModes {
    /// Grant or revoke one ack'd mode. [`Self::levitating`] is deliberately unreachable here — it
    /// has no opcode of its own and arrives through the server-authored flag merge instead.
    pub(crate) fn set(&mut self, mode: MoveMode, apply: bool) {
        match mode {
            MoveMode::Root => self.rooted = apply,
            MoveMode::WaterWalk => self.water_walking = apply,
            MoveMode::FeatherFall => self.feather_fall = apply,
            MoveMode::Hover => self.hover = apply,
        }
    }

    /// The granted modes as `MOVEMENTFLAGS` bits — **what every outbound packet must carry**.
    ///
    /// This is the half that is easy to skip and expensive to skip: the reference has one
    /// `[cmov+0x40]` that is both the live state and the wire word, so a real client echoes back
    /// whatever the server granted for free. Ours rebuilds the word from state each frame, so
    /// dropping a mode here makes the server's copy forget it — and the next server-authored move
    /// echoes a mode-less word back, our merge clears the mode, and the aura silently stops working
    /// (decision 0726 hit exactly this with GM flight).
    pub(crate) fn wire_flags(&self) -> u32 {
        use crate::creature_anim::move_flags as f;
        let mut flags = 0;
        if self.rooted {
            flags |= f::ROOT;
        }
        if self.water_walking {
            flags |= f::WATER_WALKING;
        }
        if self.feather_fall {
            flags |= f::SAFE_FALL;
        }
        if self.hover {
            flags |= f::HOVER;
        }
        if self.levitating {
            flags |= f::LEVITATING;
        }
        flags
    }

    /// Lift the granted modes **out of a server-authored flag word** into typed state (decision
    /// 0726's step, now the whole family's). All five bits sit inside the reference's
    /// `SERVER_AUTHORED` merge mask (`0x75a0_7dff`), so a bare `MSG_MOVE_*` the server writes for
    /// our mover can grant or revoke them with no handshake — `.cheat fly` is the case that proved
    /// it, and it is why LEVITATING has no opcode of its own.
    ///
    /// **Root is a deliberate divergence, and not because the mask excludes it** — bit 12 is inside
    /// the mask, so the reference *does* merge root from a server-authored pose. We don't: in this
    /// client the ack'd opcode is root's only grant path, and letting a bare pose clear it would put
    /// a locally-unrooted body against a server that still has us rooted — which streams moving bits
    /// and trips `CHEAT_TYPE_ROOT_MOVE`. The reference gets away with it because vmangos re-adds
    /// `MOVEFLAG_ROOT` to anything it writes for a rooted mover (`MovementHandler.cpp:1064`), so its
    /// merge is a no-op in practice. Revisit if a case ever needs the wire to unroot.
    pub(crate) fn merge_from_wire(&mut self, wire: u32) {
        use crate::creature_anim::move_flags as f;
        self.water_walking = wire & f::WATER_WALKING != 0;
        self.feather_fall = wire & f::SAFE_FALL != 0;
        self.hover = wire & f::HOVER != 0;
        self.levitating = wire & f::LEVITATING != 0;
    }
}

/// **The two incapacitate suppressions, applied to a freshly built move-flag word** (decision 0880)
/// — the last step of [`super::control`]'s per-frame rebuild, before the word drives the animation
/// and goes on the wire.
///
/// The reference never needs this: its `[cmov+0x40]` is written *only* by emitters that already
/// refuse, so the bits are absent by construction. Ours rebuilds the word from raw input every
/// frame, so a suppression not re-applied here streams motion the body is not performing (decision
/// 0056's law) and feeds the same lie to the anim layer.
///
/// - **Rooted → no direction bits.** The input tick's allow-list (`0x615c71` → the byte table at
///   `0x618054`) DROPS command ids 0–5 — StartMove/StopMove/StartStrafe — while `0x1000` is set, so
///   a key pressed under a root never reaches `0x7c6ae0`/`0x7c6c50` and never sets a bit; `SetRoot
///   0x7c7340`'s own `and 0xffe07f00` wipes whatever was set, at apply. With the low nibble clear
///   the movement-anim resolver `0x5fd100` takes its not-moving exit at `5fd10c test al,0xf` before
///   it can reach any locomotion id — **that** is why a rooted character shows no walk/run
///   animation, and why ours was still showing one: it kept building FORWARD out of the raw axis
///   while the mover ignored it.
/// - **Stunned → no turn bits.** `0x514755` skips the turn emitter `0x514f50` outright and
///   force-stops one already in flight, so `0x7c6d90 StartTurn` never runs. Decision 0872 stopped
///   the *rotation* (`control` restores the aim) but left the bits streaming, which still told every
///   observer to play the turn-in-place shuffle.
///
/// They are keyed on different state on purpose, exactly as the reference has them: a pure root
/// (Frost Nova, Entangling Roots) still pivots — turn is on the allow-list *deliberately* — and only
/// `UNIT_FLAG_STUNNED` takes the pivot away (wow-re `unit-flags-movement-gates.md` §5). Nothing else
/// is touched: the granted modes ride on (drop one and the server forgets it), SWIMMING survives a
/// root exactly as `0xffe07f00` preserves `0x200000`, and FALLING is already gone by the time we get
/// here — the root ended the arc ([`super::mover`]'s anchor).
pub(crate) fn incapacitated_flags(flags: u32, rooted: bool, stunned: bool) -> u32 {
    use crate::creature_anim::move_flags as f;
    let mut out = flags;
    if rooted {
        out &= !f::ANY_MOVE;
    }
    if stunned {
        out &= !(f::TURN_LEFT | f::TURN_RIGHT);
    }
    out
}

/// Our controllable avatar. Until `active`, the camera free-flies; once the server reports our
/// position we take control (third-person) and drive movement. Toggle free-fly with the dev
/// chord + `F` (decision 1043).
/// `active`/`pos`/`detached` are `pub(crate)` so terrain streaming can center the loaded block on the
/// avatar in third-person and on the free-flying camera while detached.
#[derive(Resource, Default)]
pub(crate) struct Player {
    pub(crate) active: bool,
    /// **The granted mover modes** — what the server has switched on for our mover, as typed state
    /// rather than raw bits. See [`MoveModes`].
    pub(super) modes: MoveModes,
    /// **Autorun** latched on — the reference's input bit `0x1000` in the local mover's input word
    /// `[MOVE+4]`, flipped by `ToggleAutoRun 0x513de0` (a read+invert: the command family's only
    /// *toggle*, where every directional command is a set/clear pair). VERIFIED, wow-re
    /// `rf78-movement-command-handlers.md`.
    ///
    /// It is not a movement of its own. The axis emitter `0x514da0` folds the bit into the
    /// **forward axis** (`test ah,0x10`), so autorun *is* held-forward: it nets against a held S
    /// and diagonals with a strafe, exactly like the both-button run. Ours folds in at the same
    /// places the other forward sources do — the direction vector, the wire flags, the swim
    /// amounts, and the turn-rate's "am I translating" test.
    pub(super) autorun: bool,
    /// **`/follow` is holding the forward key this frame** (decision 0890). Not a mode of its own:
    /// the reference's follow owns no translation and simply pushes the same move-forward bit the W
    /// key does (`0x60e790`), so this folds into [`forward_axis`] beside `autorun` and the
    /// both-button run rather than driving the mover itself. Rewritten every frame by
    /// [`super::follow::steer_follow`], which runs just before the controller reads it.
    pub(super) follow_forward: bool,
    /// Free-fly (`F`): the camera moves on its own and the avatar/server position is frozen.
    pub(crate) detached: bool,
    /// Feet position in **Bevy** coords (converted to raw WoW only when sending to the server).
    pub(crate) pos: Vec3,
    /// Vertical velocity (yd/s, Bevy +Y up) for gravity/jump/fall. Integrated each frame; zeroed while
    /// grounded. Fed into avian's `MoveAndSlide`.
    pub(super) vel_y: f32,
    /// Current horizontal velocity (yd/s). Live (from input) while grounded; while airborne it's the
    /// take-off momentum (a moving jump keeps its trajectory — the WoW feel), except a jump from a
    /// standstill gets one [`AIR_NUDGE_SPEED`] steer in the pressed direction. Zero when standing still.
    pub(super) horiz_vel: Vec3,
    /// The CMovement `moveFlags` we last streamed to the server (directional + turn bits, see
    /// [`crate::net::move_flags`]). Diffed against this frame's flags to emit a `MSG_MOVE_*` per
    /// movement-axis transition — the way the real client announces its movement.
    pub(super) move_flags: u32,
    /// The facing (WoW orientation) as of **last frame** — the reference's facing-change detector
    /// (`0x617170`, exact equality against the unit's live facing cell). Any change off the turn axis
    /// streams a `MSG_MOVE_SET_FACING` that frame, moving or standing, so observers see us aim
    /// (decision 0617). Updated every frame whether or not a packet went out.
    pub(super) last_facing: f32,
    /// The **position we last told the server** (WoW coords, exactly the floats that went on the
    /// wire). Diffed against this frame's live position to catch a drift we would otherwise never
    /// report: our resolver settles a resting body a fraction of a millimetre after the packet that
    /// reported the rest (a landing, a login, a teleport onto a server-authored pose), and while
    /// standing still nothing else goes out. vmangos compares an incoming position to its stored one
    /// with EXACT float equality and interrupts a movement-interrupt cast on any difference, so a
    /// stale copy turns the next packet — in practice the first `MSG_MOVE_SET_FACING` of a
    /// right-drag — into a mid-cast "you moved". Decision 0907; the reconcile lives in
    /// [`super::movement_net::stream_self_movement`].
    pub(super) last_pos: [f32; 3],
    /// The stand state we last volunteered (`CMSG_STANDSTATECHANGE`) whose echo into our
    /// `UNIT_FIELD_BYTES_1` hasn't landed yet — the local commit (the client's `SetStandState`
    /// `0x6127b0` applies immediately *and* sends; decision 0080c). `None` = at the echoed value.
    pub(super) stand_pending: Option<u8>,
    /// **Settling after a teleport/summon/login**: the streamed world (terrain *and* its WMO
    /// buildings + colliders) arrives over several frames, so the collision under the destination
    /// isn't there the instant we snap to it. While settling the movers hold the avatar in place
    /// with gravity **off** — otherwise it falls through the not-yet-loaded city/building floor —
    /// and the loading screen waits for the release. **Released by the terrain streamer** (decision
    /// 0737) once the destination is resident — scene spawned and collider queue quiet
    /// ([`crate::loading_screen::WorldLoadProgress`]) — or at [`SETTLE_TIMEOUT`]; **never by ground
    /// contact**, which a flyer, a swimmer, or a genuinely airborne teleport never produces (the
    /// old probe release ran only in the walk mover, and each other mover mode needed — and got —
    /// its own leak patch before 0737 deleted the class).
    pub(crate) settling: bool,
    /// `Time::elapsed_secs` deadline to give up settling and release (see [`Player::settling`]).
    /// Pushed forward by the streamer while [`Player::world_stale`] (0710's fail-closed law).
    pub(crate) settle_deadline: f32,
    /// **The colliders under our feet may still belong to the map we just left.** Set at every
    /// snap, cleared by the terrain streamer once the destination's own world is resident.
    ///
    /// The snap runs a whole `WorldStage` *before* the streamer swaps maps — so on the arrival
    /// frame the physics world is still entirely the old map's, and its despawn is a deferred
    /// command on top of that. Any settle judgement made in that window would be about the wrong
    /// world — and an **instance entrance sits within a yard of its own outdoor portal**
    /// (Zul'Gurub's pad is z 92.53 against the Stranglethorn ground you left at 92.30), which is
    /// how the pre-0710 ground probe declared the floor found on frame 0 and dropped the body
    /// 15 yd through the ZG city WMO. While this flag is set the streamer makes no release
    /// judgement and pushes [`Player::settle_deadline`] forward, so the timeout budget measures
    /// time waiting for the *destination's* world (decisions 0710 + 0737).
    pub(crate) world_stale: bool,
    /// A same-map teleport landed: the server relocated the mover, so any in-progress self
    /// server-ride (charge/taxi) is **void** — vmangos teleports at ITS flight end (its own spline
    /// finishes ~latency before ours) and its spline-done handler ignores acks while the teleport
    /// is pending, so the relocation IS the hand-back. `drive_self_ride` takes this flag first:
    /// it drops the ride + spline without mirroring the stale flight pose over the snap (the
    /// 4-yd-hover + full-6s-settle landing bug, decision 0501) and owes no `CMSG_MOVE_SPLINE_DONE`.
    pub(super) ride_abort: bool,
    /// `Time::elapsed_secs` when we last sent a heartbeat.
    pub(super) last_heartbeat: f32,
    /// `Time::elapsed_secs` when the current airborne phase (jump or step-off) began, else `None` on the
    /// ground. Drives the wire `fall_time` (ms airborne) and detects the take-off / landing transitions
    /// that emit `MSG_MOVE_JUMP` / `MSG_MOVE_FALL_LAND` (decision 0053).
    pub(super) airborne_since: Option<f32>,
    /// At rest wedged between steep faces ([`WEDGE_STILL_FRAMES`] stalled airborne frames):
    /// treated as standing — the fall is over, walking control is live — while a close down-probe
    /// still finds support. Cleared by real ground, by jumping, or by walking off the support into
    /// open air (a fresh fall). Decisions 0211/0212.
    pub(super) wedged: bool,
    /// Consecutive stalled airborne frames (see [`WEDGE_STALL_RATIO`]).
    pub(super) wedge_still: u8,
    /// **Supported by a certified steep contact, not a walkable floor** — the reference's
    /// `0x4000000`, whose meaning decision 1125 settled from the bytes. Two ways to earn it, one
    /// meaning: climbing the slanted skirt of a low edge already certified clearable (the foot-cone
    /// ride, 1123), or resting on the surface being followed down off a ledge (1127).
    ///
    /// Counts as standing. The frame-start ground probe looks straight down and sees only the steep
    /// face, so without this the frame reads as airborne and gravity takes the body off the surface:
    /// the climb undone into the mid-face dwelling 0209 exists to prevent, and on the way down the
    /// dive past the edge the director reported. Re-earned every frame from the certification going
    /// up and from the cone's descent bound coming down — neither can hold a body that has not just
    /// proved it is on a surface — and cleared the moment a walkable floor takes over.
    pub(super) steep_support: bool,
    /// The take-off vertical speed (yd/s, WoW +Z up) snapshotted when the airborne phase began — the
    /// client's `StartFalling` argument (`+0xa0`, constant per arc) and the `zspeed` we send in the
    /// jump tail: `JUMP_SPEED` for a jump, **exactly 0** for a step-off (the walk election calls
    /// `StartFalling(0)`). Observers replay the parabola from it, and the FALLINGFAR latch splits
    /// its distance/timer legs on it (decision 0179); held constant while `fall_time` advances.
    pub(super) jump_zspeed: f32,
    /// The translation-direction move-flag bits ([`crate::creature_anim::move_flags::ANY_MOVE`]) the
    /// current airborne arc launched with. Mid-air these are the *actual* motion — momentum is frozen at
    /// takeoff, so held keys move nothing — and the live flags (animation, pose, wire) read them instead
    /// of the keys (decision 0056: the flags mirror the avatar's motion, never raw key state). Re-seeded
    /// by the standstill-jump air nudge, the one input that really moves us mid-air. Stale while grounded.
    pub(super) airborne_dirs: u32,
    /// Launch height (Bevy Y) snapshotted when the airborne arc began — the client's StartFalling
    /// `+0x7c = +0x18` Z snapshot; the FALLINGFAR distance leg measures descent below it.
    pub(super) fall_start_y: f32,
    /// MOVEFLAG_FALLINGFAR latched for this arc: a jump descended [`FALL_FAR_DROP`] below its
    /// launch, or a step-off fall lasted [`FALL_FAR_TIME`] (the legs are exclusive on the launch
    /// vz — decision 0179). Latched once per arc (only landing clears it, like the client's
    /// StopFalling); sets [`crate::creature_anim::move_flags::FALLING_FAR`] on the live flags — the
    /// mid-air Fall(40) pose, the landing-anim gate, and the wire.
    pub(super) fall_far: bool,
    /// The character's facing (Bevy yaw, radians). Right-drag and movement keep this in sync with the
    /// camera; left-drag (camera-only orbit) leaves it alone, so it can diverge from the camera yaw —
    /// and that offset now persists (no auto-follow back behind while moving). Sent to the server as
    /// orientation, and the basis WASD move in. This is the *aim*/facing — distinct from the rendered
    /// body heading.
    pub(super) face_yaw: f32,
    /// In **swim mode** ([`super::swim`]): the water over the feet crossed the swim-enter depth, so
    /// the avatar floats and swims in 3D instead of walking, sets `MOVEFLAG_SWIMMING` (lighting the
    /// swim gait and streaming it), and pitches its body to the swim heading. Hysteresis-latched
    /// (see [`super::swim::update_swimming`]) so wading the boundary doesn't flicker.
    pub(crate) swimming: bool,
    /// **Our own collision height** — the avatar's copy of [`crate::entities::CollisionHeight`],
    /// mirrored onto this resource because the swim arm runs off it and never touches the ECS
    /// entity. Every swim depth line is a fraction of it, which is why a gnome floats with her head
    /// out and a night elf sits 0.3 yd deeper (decision 0645). Kept as the component's own type
    /// rather than a bare `f32` precisely so `Player::default()` cannot seed it to **zero** — at
    /// zero every depth line collapses to 0 and the avatar swims on dry land. It defaults to
    /// [`DEFAULT_COLLISION_HEIGHT`] and is replaced once our body's display id resolves.
    pub(crate) collision_height: crate::entities::CollisionHeight,
    /// The **swim pitch** (radians, +up) — the client's persistent per-unit pitch (`CMovement+0x20`,
    /// the swim §5's TU-B): **held** when unsteered (an idle floater keeps its pitch — never
    /// auto-leveled; the only zeroing writer `0x7c6e80` fires from stop-swim/teleport, not mouse
    /// release). Steered by mouselook as a **DIRECT set** of the camera aim pitch, clamped
    /// [`MOUSELOOK_PITCH_CLAMP`] (±89°) — **VERIFIED** (the camera-pitch §5, wow-re
    /// `swim-camera-pitch.md`, decision 0492, refuting the earlier no-camera-coupling census):
    /// the ref's mouse-move event chain lands in `SetPitch 0x7c6f70`, an unconditional store
    /// with no integrator and no rate limit, and the basis rebuild re-aims travel in-call —
    /// hence zero lag. The `0x7c4f80` 0.75·turnRate integrator (clamp ±π/2) belongs to the
    /// PitchUp/Down keys, default-unbound in 1.12, which we don't bind. A left-drag camera
    /// orbit steers nothing (it doesn't turn the character, so it must not bend the swim);
    /// Space never touches the pitch (it is the Jump command, 0487).
    /// Streamed on the wire's swim tail; the body renders pitched by it while swimming fwd/back
    /// (TU-A's `Ry` law, see the render block in [`super::control`]).
    pub(super) swim_pitch: f32,
    /// This frame's **flag-scalar swim travel speed** (yd/s) — the directional swim/swimBack
    /// speed when any swim translation input is live, else 0. The swim stroke's playback-rate
    /// numerator — **VERIFIED** (the swim-feel §5's TU-I): `0x5fe2f0` divides `GetCurrentSpeed`
    /// (flags + static speed fields only, never a velocity/pitch projection) by the clip's
    /// moveSpeed, so a vertically pitched stroke plays at full rate. Written by the controller's
    /// swim arm; read at the `MovementState` fill; stale while not swimming.
    pub(super) swim_stroke_speed: f32,
    /// The rendered **body** heading (Bevy yaw, radians) — the client's display-facing pose: while
    /// strafing it eases toward `face_yaw ± 90°/45°` ([`crate::creature_anim::strafe_body_offset`])
    /// with the
    /// SpineLow/Head counter-twist walking the upper body back onto the aim
    /// ([`crate::creature_anim::BodyTwist`]); moving without a strafe it snaps to `face_yaw`;
    /// standing it chases at [`STATIONARY_CHASE_RATE`] × turn rate.
    pub(super) model_yaw: f32,
    /// A server-authored spline owns the avatar this frame (Charge, and later knockback/taxi/fear):
    /// the server sent an `SMSG_MONSTER_MOVE` for our own guid, so `sample_splines` drives the
    /// transform and [`super::server_ride::drive_self_ride`] mirrors it into `pos`/facing while input,
    /// physics, and the outbound movement stream all yield. Set the frame the ride's [`Spline`]
    /// appears, cleared the frame it ends — where we send `CMSG_MOVE_SPLINE_DONE` and resume.
    pub(super) server_riding: bool,
    /// The `splineId` of the ride in progress (echoed in `CMSG_MOVE_SPLINE_DONE` when it ends).
    pub(super) ride_spline_id: u32,
    /// Standing on a transport (boat/zepp): the mover lives in that platform's frame (decision
    /// 0438 phase 2). Attached when the ground support is a [`crate::transport::Transport`]
    /// collider; kept through jumps above the deck (deck-frame ballistics — a jump on a moving
    /// boat lands where it took off); detached on world-ground support, on entering the water,
    /// or when the boat despawns. See the carry/attach blocks in [`super::control`].
    pub(super) ride: Option<PlayerRide>,
}

/// The player's attachment to a transport's platform frame — see [`Player::ride`].
pub(super) struct PlayerRide {
    /// The transport's ECS entity (its collider is the ground support that attached us).
    pub(super) entity: Entity,
    /// The transport's guid — the wire tail names the boat by guid, not entity.
    pub(super) guid: u64,
    /// Feet position in the transport's local frame (Bevy axes), snapshotted at frame end;
    /// next frame's carry recomposes `world = boat_transform × local` before input integrates.
    pub(super) local_pos: Vec3,
    /// The boat yaw (Bevy, radians) at the snapshot — the carry applies the per-frame delta to
    /// `face_yaw` (the deck turns the standing player with it), and the wire's local orientation
    /// is `face_yaw − boat_yaw`.
    pub(super) boat_yaw: f32,
}

impl Player {
    /// The character's *facing* (Bevy yaw, radians) — the aim, kept in sync with the camera by
    /// right-drag/movement. This is the unit's orientation as sent to the server, distinct from the
    /// rendered body heading (`model_yaw`, which a strafe rotates). The 3D-audio listener panning
    /// tracks this (wow-re benilla-pins B14: the listener forward is the character facing, not the
    /// camera).
    pub(crate) fn facing(&self) -> f32 {
        self.face_yaw
    }

    /// End the post-snap settle hold (decision 0737) — called by the terrain streamer, the only
    /// system that knows the destination's residency. `resident` = the world arrived (scene +
    /// colliders); `false` = the [`SETTLE_TIMEOUT`] backstop fired without it. Which end it was is
    /// the whole diagnosis of a fall-through report, so it goes through the `sett` trace either way.
    pub(crate) fn end_settle(&mut self, resident: bool, now: f32) {
        self.settling = false;
        let waited = SETTLE_TIMEOUT - (self.settle_deadline - now);
        super::move_trace::settle(resident, waited, self.pos);
    }

    /// The avatar's current CMovement move-flags as last streamed (directional + turn bits — see
    /// [`crate::creature_anim::move_flags`]). The water-foam selector reads the same two bit-tests as
    /// the reference (`& 0xf` translating, `& 0x30` turning; wow-re CWater0Ripple driver `0x5fa760`).
    pub(crate) fn move_flags(&self) -> u32 {
        self.move_flags
    }

    /// The **commanded planar speed** in yd/s — the reference's `[[player+0x118]+0x84]`, the
    /// CMovement speed scalar its producer `0x7c4c90` returns:
    ///
    /// ```text
    /// mov  edx,[ecx+0x40]        ; movement flags
    /// test dl,0xf                ; the four DIRECTION bits
    /// jne  …                     ; moving → the speed
    /// fld  dword ptr [0x7ffd74]  ; = 0.0
    /// ```
    ///
    /// So it is **exactly zero with no direction key held** — not a measured velocity that decays,
    /// and not the weather wind's 149 ms positional average (`0x67c150`): it is live, and tracks a
    /// speed change on the same frame. The precipitation spawn slab's tilt keys on it through
    /// `mgr+0x7c` (wow-re `wx-snow-placement-law.md` §9), which is why the distinction earns an
    /// accessor: an averaged stand-in would leave the slab leaning for 150 ms after a stop, and a
    /// raw `horiz_vel` would keep it leaning through an entire fall on take-off momentum.
    pub(crate) fn planar_speed(&self) -> f32 {
        if self.move_flags & 0xf == 0 {
            return 0.0;
        }
        self.horiz_vel.with_y(0.0).length()
    }

    /// The transport we're standing on (its guid), if any — the platform-frame attachment
    /// (decision 0438 phase 2). For instruments (the crossing probe watches the ride survive
    /// the map seam).
    pub(crate) fn riding(&self) -> Option<u64> {
        self.ride.as_ref().map(|r| r.guid)
    }

    /// A server-authored spline currently owns the avatar (Charge/knockback/taxi — the
    /// [`super::server_ride`] state). For instruments (the taxi probe watches the flight run) and
    /// the UI's `UnitOnTaxi` feed.
    pub(crate) fn server_riding(&self) -> bool {
        self.server_riding
    }
}

/// The **forward/back axis** — a net accumulation, byte-verified at the reference's emitter
/// `0x514da0` (wow-re `rf79-autorun-cancel-set.md` §3):
/// `autorun(+1) + forward(+1) + both-buttons(+1) − backward(−1)`, then one START in `sign(axis)`,
/// or a genuine STOP at zero. A pure function so the state table it encodes can be pinned by test —
/// the controller reads it for the direction vector, the backpedal speed, the swim amounts, and the
/// streamed flags, so all four can never disagree (decision 0056).
pub(super) fn forward_axis(
    forward: bool,
    backward: bool,
    both_buttons: bool,
    autorun: bool,
) -> i32 {
    i32::from(forward) + i32::from(both_buttons) + i32::from(autorun) - i32::from(backward)
}

/// Does this frame's input **destroy** autorun? The cancel set (wow-re `rf79-autorun-cancel-set.md`
/// §1 — six writers clear the reference's `0x1000`; these are the four with a benilla analog).
///
/// `fwd_down`/`back_down` are **key-DOWN edges, not held state**: the clear lives in the shared SET
/// helper (`0x514a5a`, gated `test cl,0x30`), and the release path restores nothing. `both_engaged`
/// is the transition *into* both-buttons-held (`0x514a73`). `lost_mover` is death / root / stun / a
/// taxi hand-off, where the emitter's gate drops and writer #4 clears the bit — a level, not an edge.
///
/// A jump, a chat EditBox taking focus, and a zone change are each VERIFIED *survivors* and are
/// deliberately absent. Mounting is unsettled in the reference and is treated as a survivor here.
pub(super) fn autorun_cancelled(
    fwd_down: bool,
    back_down: bool,
    both_engaged: bool,
    lost_mover: bool,
) -> bool {
    fwd_down || back_down || both_engaged || lost_mover
}

#[cfg(test)]
mod autorun_tests {
    use super::{autorun_cancelled, forward_axis};

    /// The four states of wow-re RF-0079 §3's table, in its own terms.
    #[test]
    fn the_axis_reproduces_the_verified_state_table() {
        // autorun, nothing held → +1, runs forward.
        assert_eq!(forward_axis(false, false, false, true), 1);
        // "autorun + forward held" is the state AFTER W's key-down destroyed the bit, which is why
        // the table reads +1 and why pressing W is wire-silent: the axis sees forward alone, the
        // value it already had. The axis never sees the raw combination in this order.
        assert_eq!(forward_axis(true, false, false, false), 1);
        // Reaching it the other way (hold W, then toggle) DOES sum to 2 — `0x514da5`'s `mov ebx,1`
        // for autorun then `inc ebx` for forward. Only the sign is ever consumed, so it behaves
        // identically; asserted so the byte-shape is recorded rather than accidentally "fixed".
        assert_eq!(forward_axis(true, false, false, true), 2);
        // autorun ON, then S pressed → the key-down destroyed the bit first, so the axis sees
        // backward alone: −1, a clean reversal.
        assert_eq!(forward_axis(false, true, false, false), -1);
        // S held, THEN autorun toggled on → the toggle misses the clear helper, both live: 0.
        // The state no "autorun = held forward" reading can produce.
        assert_eq!(forward_axis(false, true, false, true), 0);
    }

    /// The order-dependence is the whole shape of the feature: the same two inputs, applied in the
    /// two orders, end in different places — and only one of them can resume.
    #[test]
    fn the_two_orders_differ_and_only_one_resumes() {
        // Order A — autorun first, then press S.
        let mut autorun = true;
        if autorun_cancelled(false, true, false, false) {
            autorun = false;
        }
        assert_eq!(
            forward_axis(false, true, false, autorun),
            -1,
            "walks backward"
        );
        // Releasing S leaves nothing behind: the release restores no bit.
        assert_eq!(
            forward_axis(false, false, false, autorun),
            0,
            "stops, does not resume"
        );

        // Order B — S held first, then toggle autorun on. The toggle is not a directional SET, so
        // the cancel set never fires.
        let autorun = true;
        assert!(!autorun_cancelled(false, false, false, false));
        assert_eq!(
            forward_axis(false, true, false, autorun),
            0,
            "STOP with S still held"
        );
        // Releasing S now DOES resume — the bit survived.
        assert_eq!(
            forward_axis(false, false, false, autorun),
            1,
            "resumes forward"
        );
    }

    /// Both-button run and autorun stack on the same axis, and a held S nets either to a standstill.
    #[test]
    fn both_button_run_shares_the_axis() {
        assert_eq!(forward_axis(false, false, true, false), 1);
        assert_eq!(
            forward_axis(false, true, true, false),
            0,
            "S nets the both-button run to a stop"
        );
        // Engaging the both-button run destroys autorun rather than stacking with it.
        assert!(autorun_cancelled(false, false, true, false));
    }

    /// The verified survivors: only the four listed inputs clear the bit.
    #[test]
    fn a_jump_or_a_chat_line_is_not_in_the_cancel_set() {
        assert!(!autorun_cancelled(false, false, false, false));
        // Losing the mover (death / root / taxi) is, and it is a level rather than an edge.
        assert!(autorun_cancelled(false, false, false, true));
    }
}

#[cfg(test)]
mod move_mode_tests {
    use super::{
        incapacitated_flags, MoveModes, FEATHER_TERMINAL_VELOCITY, GRAVITY, HOVER_CLIMB_RATE,
        HOVER_HEIGHT, TERMINAL_VELOCITY,
    };
    use crate::creature_anim::move_flags as f;
    use benilla_protocol::MoveMode;

    /// Every ack'd mode round-trips: granted through [`MoveModes::set`], out as its own
    /// `MOVEMENTFLAGS` bit, and revoked again. The bit values are the ones vmangos reads back out of
    /// our acks, so a mismatch is silent on our side and wrong on the server's.
    #[test]
    fn each_granted_mode_rides_the_wire_word_as_its_own_bit() {
        for (mode, bit) in [
            (MoveMode::Root, f::ROOT),
            (MoveMode::WaterWalk, f::WATER_WALKING),
            (MoveMode::FeatherFall, f::SAFE_FALL),
            (MoveMode::Hover, f::HOVER),
        ] {
            let mut modes = MoveModes::default();
            assert_eq!(modes.wire_flags(), 0);
            modes.set(mode, true);
            assert_eq!(
                modes.wire_flags(),
                bit,
                "{mode:?} must ride the wire as {bit:#x}"
            );
            assert_eq!(
                mode.flag(),
                bit,
                "{mode:?}: our bit and the protocol's agree"
            );
            modes.set(mode, false);
            assert_eq!(modes.wire_flags(), 0, "{mode:?} revokes cleanly");
        }
    }

    /// **Levitate grants three modes at once** (spell 1706 = feather fall + hover + water walk), so
    /// the word has to carry all three together — the case that proves this is one system and not
    /// four independent booleans.
    #[test]
    fn levitate_carries_its_three_modes_at_once() {
        let mut modes = MoveModes::default();
        for mode in [MoveMode::FeatherFall, MoveMode::Hover, MoveMode::WaterWalk] {
            modes.set(mode, true);
        }
        assert_eq!(
            modes.wire_flags(),
            f::SAFE_FALL | f::HOVER | f::WATER_WALKING
        );
        // …and dropping one leaves the other two standing.
        modes.set(MoveMode::Hover, false);
        assert_eq!(modes.wire_flags(), f::SAFE_FALL | f::WATER_WALKING);
    }

    /// The server-authored merge owns the four unhandshaked bits and **must not touch root**. Root
    /// is inside the reference's `0x75a0_7dff` mask, so this is our deliberate divergence, not the
    /// mask's doing — and the failure it prevents is concrete: a bare pose clearing root locally
    /// while the server still holds us rooted streams moving bits into `CHEAT_TYPE_ROOT_MOVE`.
    #[test]
    fn the_wire_merge_never_unroots_us() {
        let mut modes = MoveModes {
            rooted: true,
            ..Default::default()
        };
        modes.merge_from_wire(0); // a pose claiming no modes at all
        assert!(modes.rooted, "only the ack'd opcode may unroot");
        assert!(!modes.hover && !modes.feather_fall && !modes.water_walking && !modes.levitating);

        // The other four do follow the wire, in both directions — this is how `.cheat fly` arrives.
        modes.merge_from_wire(f::SAFE_FALL | f::HOVER | f::WATER_WALKING | f::LEVITATING);
        assert!(modes.hover && modes.feather_fall && modes.water_walking && modes.levitating);
        assert!(modes.rooted, "still ours alone");
    }

    /// **Feather fall is a terminal-velocity substitution and nothing more** (`0x7c5d20`): gravity
    /// is untouched, so the drop *accelerates normally* and only then rides the lower cap. Pinning
    /// the shape, not just the constant — an implementation that scaled gravity instead would give
    /// a fall that starts wrong, and would pass a constants-only test.
    #[test]
    fn feather_fall_caps_the_descent_without_softening_gravity() {
        let dt = 1.0 / 60.0;
        let step = |v: f32, terminal: f32| (v - GRAVITY * dt).max(-terminal);

        // One frame from rest is identical with and without the aura — gravity is the same.
        assert_eq!(
            step(0.0, FEATHER_TERMINAL_VELOCITY),
            step(0.0, TERMINAL_VELOCITY),
            "the first frame of a slow fall is an ordinary fall"
        );

        // Held long enough, the two settle at their own caps.
        let settle = |terminal: f32| {
            let mut v = 0.0;
            for _ in 0..600 {
                v = step(v, terminal);
            }
            v
        };
        assert_eq!(
            settle(FEATHER_TERMINAL_VELOCITY),
            -FEATHER_TERMINAL_VELOCITY
        );
        assert_eq!(settle(TERMINAL_VELOCITY), -TERMINAL_VELOCITY);

        // And the cap is reached in well under half a second, which is why Slow Fall reads as a
        // drift rather than a fall: v = g·t ⇒ t = 7.0/19.291105 ≈ 0.363 s.
        let mut v = 0.0f32;
        let mut frames = 0;
        while v > -FEATHER_TERMINAL_VELOCITY {
            v = step(v, FEATHER_TERMINAL_VELOCITY);
            frames += 1;
        }
        assert!(
            (frames as f32 * dt - 0.363).abs() < 0.02,
            "capped after {frames} frames ({:.3} s), expected ≈0.363 s",
            frames as f32 * dt
        );
    }

    /// The two mover constants are the reference's, not round numbers we liked. A drift here is
    /// invisible in play and wrong everywhere.
    #[test]
    fn the_mode_constants_are_the_verified_ones() {
        assert_eq!(FEATHER_TERMINAL_VELOCITY, 7.0); // [0x87d898]
        assert_eq!(HOVER_HEIGHT, 1.0); // [0x7ff9d8]
        assert_eq!(TERMINAL_VELOCITY, 60.148_003); // [0x87d894]
    }

    /// Feather fall must be the SLOWER cap — the whole point of the aura. A *compile-time* assert,
    /// because both sides are constants: swapped, they would otherwise only show up in play.
    const _: () = assert!(FEATHER_TERMINAL_VELOCITY < TERMINAL_VELOCITY);

    /// **A root and a stun are different states, and the difference is the pivot** (decision 0872).
    /// This is the distinction the first pass got wrong — B179 was filed under the movement-mode
    /// family, and it is not a member of it: root is a `MOVEMENTFLAGS` bit that leaves turning live
    /// by the input tick's *authored* allow-list, while the freeze is `UNIT_FIELD_FLAGS` bit
    /// `0x40000` gating the turn emitter. vmangos's `HandleModStun` grants both at once, which is
    /// the only reason they look like one thing.
    ///
    /// The bit values are what the gate reads, so a transcription slip here silently un-freezes
    /// every stun; `UNIT_FLAG_STUNNED` is checked against vmangos `UnitDefines.h` and against the
    /// reference's own `shr eax, 0x12` (bit 18 → `1 << 18` = `0x40000`).
    #[test]
    fn a_stun_is_not_a_root() {
        assert_eq!(crate::player::UNIT_FLAG_STUNNED, 1 << 18);
        assert_eq!(crate::player::UNIT_FLAG_STUNNED, 0x0004_0000);
        // It is a UNIT_FIELD_FLAGS bit, not a MOVEMENTFLAGS one — they are different words, and the
        // whole confusion was reading the freeze as a movement flag. Nothing in the mode family may
        // collide with it.
        for mode in [
            MoveMode::Root,
            MoveMode::WaterWalk,
            MoveMode::FeatherFall,
            MoveMode::Hover,
        ] {
            assert_ne!(
                mode.flag(),
                crate::player::UNIT_FLAG_STUNNED,
                "{mode:?} is a MOVEMENTFLAGS bit; the stun gate is a descriptor bit"
            );
        }
        // …and a root, alone, is not a stun: granting it touches no unit flag at all.
        let mut modes = MoveModes::default();
        modes.set(MoveMode::Root, true);
        assert_eq!(modes.wire_flags(), crate::creature_anim::move_flags::ROOT);
    }

    /// The hover clearance is reached by a **rate-limited climb**, not a snap — the reference's
    /// second writer. Pins the rate and, more importantly, that the rise takes real time: a body a
    /// full yard low needs ~0.143 s, which is the difference between a float and a pop.
    #[test]
    fn hover_climbs_to_its_clearance_rather_than_popping() {
        assert_eq!(HOVER_CLIMB_RATE, 7.0); // [0x87d898], via 0x7c61b0
        let dt = 1.0 / 60.0;
        let mut clearance = 0.0_f32;
        let mut frames = 0;
        while clearance < HOVER_HEIGHT {
            clearance = (clearance + HOVER_CLIMB_RATE * dt).min(HOVER_HEIGHT);
            frames += 1;
        }
        assert!(
            frames > 1,
            "a snap would arrive in one frame; this must not"
        );
        assert!(
            (frames as f32 * dt - HOVER_HEIGHT / HOVER_CLIMB_RATE).abs() < 0.02,
            "climbed in {frames} frames, expected ≈0.143 s"
        );
    }

    /// **The two incapacitate suppressions take exactly their own bits** (decision 0880) — the
    /// difference between a pure root and a stun, which is the whole distinction B179 was drawing,
    /// expressed on the one word that drives both the animation and the wire.
    #[test]
    fn the_incapacitate_suppressions_take_exactly_their_own_bits() {
        // Every reported motion a body can hold at once, plus every mode a grant can have riding.
        let keys = f::FORWARD | f::STRAFE_LEFT | f::TURN_LEFT;
        let modes = f::ROOT | f::SWIMMING | f::SAFE_FALL | f::HOVER | f::WATER_WALKING;

        assert_eq!(
            incapacitated_flags(keys | modes, false, false),
            keys | modes,
            "neither gate: the word passes through untouched"
        );
        assert_eq!(
            incapacitated_flags(keys | modes, true, false),
            f::TURN_LEFT | modes,
            "a PURE root (Frost Nova) takes the direction bits and leaves the pivot — turn is on \
             the reference's allow-list on purpose"
        );
        assert_eq!(
            incapacitated_flags(keys, false, true),
            f::FORWARD | f::STRAFE_LEFT,
            "the two are keyed on different state: the stun bit alone takes only the pivot"
        );
        // A stun is both at once (vmangos `HandleModStun` = SetFlag(STUNNED) + SetRooted(true)).
        let blocked = incapacitated_flags(keys | modes, true, true);
        assert_eq!(
            blocked, modes,
            "Ice Block leaves NO reported motion — which is what starves the anim resolver"
        );
        // …and the modes must survive it, or the next server-authored echo clears them under us
        // (decision 0726's failure mode). ROOT above all: dropping it unroots us locally against a
        // server that still has us rooted.
        assert_eq!(blocked & f::ROOT, f::ROOT, "the root bit itself rides on");
        assert_eq!(
            blocked & f::SWIMMING,
            f::SWIMMING,
            "a rooted swimmer is still swimming — `0xffe07f00` preserves 0x200000"
        );
    }
}
