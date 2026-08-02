//! The pure animation-selection logic (RF-0057/0073 tables, movement/Special state, gait/swing/ready
//! picks, playback-rate math) — kept in its own file as it carries the bulk of the unit-tested selector
//! logic, separate from the Bevy driver systems in [`super::driver`].

use benilla_assets::AnimClip;
use bevy::prelude::*;

use crate::net::{RemoteMotion, Spline};

/// Fallback walk speed (yd/s) for a unit whose movement block didn't carry one — vanilla's default
/// creature walk speed. The run boundary is 2× this (`RecomputeBaseAnim`, RF-0057).
pub(super) const DEFAULT_WALK_SPEED: f32 = 2.5;

/// Fast-run cutoff (yd/s): at or above it the unit plays the sprint animation (id 143) ahead of Run
/// (`[0x80c484]` in the real selector, RF-0057).
const FAST_RUN_SPEED: f32 = 11.0;

/// Below this ground speed (yd/s) a streamed mover counts as standing still — guards a near-zero residual.
const MOVING_EPSILON: f32 = 0.1;

pub(super) const STAND: u16 = 0;

pub(super) const DEATH: u16 = 1;

/// StealthWalk (119) — the **prowl creep**, and with [`STEALTH_STAND`] the entire difference
/// stealth makes to a body's *pose*. Byte-verified (wow-re `rf57-movement-anim-select.md`, the
/// 2026-07-18 §5): both ids are gated by the same descriptor bit — `[[unit+0x110]+0x213] & 2`,
/// `UNIT_FIELD_BYTES_1` byte 3's CREEP flag ([`crate::net::ObjectStore`]'s `unit_is_stealthed`) —
/// 119 from the core cascade at `0x5fd1d3`, tested **after** the backward branch and **before the
/// whole speed tail**, so a prowling unit never plays Run or Sprint however fast it travels.
///
/// No aura, no visual kit, no spell id is involved. Stealth's *translucency* is its aura state
/// kit's CharProc-14 (decision 0806) and its *pose* is this flag: two independent mechanisms off
/// one server byte, which is exactly why the body could go translucent and still stand bolt
/// upright. (The same flag's other three read sites are nameplate/marker suppression — it drives
/// no body render at all; wow-re `ghost-death-visuals.md`.)
///
/// **119 is NOT rate-scaled.** The client's locomotion-rate whitelist `0x5fee80` is exactly
/// `{4,5,11,12,13,37,38,39,42,43,44,45,135,143,187}` (byte-verified, §5-agreed; wow-re
/// `rf57b-rate-jump-standstate.md`) and 119 is absent — the creep cycle plays at 1× whatever the
/// live speed, so [`playback_rate`] leaves it alone.
pub(super) const STEALTH_WALK: u16 = 119;

/// StealthStand (120) — the prowl idle, from the **last** resolver in the chain (the fallback idle
/// `0x5fd830`: swim-idle → `[110]+0x213&2` → jump/float → plain Stand). Being last is its
/// precedence: every other resolver — the sit/sleep/kneel and chair stand-states, the turn-in-place
/// shuffle, the combat Ready idle, loot — outranks it. See [`STEALTH_WALK`] for the shared gate.
pub(super) const STEALTH_STAND: u16 = 120;

/// Mount (91) — the rider's seated pose while a mount model is attached (decision 0441;
/// byte-verified, wow-re `mount-composition.md` B1): held **unconditionally** — moving, turning,
/// airborne. The client arms it outside the base selector (once at attach by `0x607a00`, then
/// re-forced on every `PlayAnimation` by `0x5fe2f0`'s mounted branch — there is NO mount leg in
/// the `0x5fd8b0` chain); benilla pins the gait slot instead, the same rendered result. The
/// locomotion the selector would pick plays on the mount model (same `0x5fd100` thresholds),
/// via the mount child's own driver pass. Variations roll as any base loop (HumanMale authors 3).
pub(super) const MOUNT: u16 = 91;

/// Loot (50) — the kneel-and-rummage held while a unit's loot window is open. Byte-verified end
/// to end (wow-re `loot-anim-leg.md`, the 2026-07-18 §5; decision 0515): the `0x5fd8b0` chain's
/// loot leg `0x5fd260` → 0x32 = 50, chain order locomotion → **LOOT** → standState →
/// combat/channel (movement outranks by position — the leg itself reads no movement state), leg
/// gates `[+0xdc]==0` (never mounted) + the `[+0xd58]&0x40` enable. The **trigger predicate**
/// (`0x6126b0`) splits on IsActivePlayer: the **self** unit keys the client-local loot-target
/// latch (`[player+0x1d28]`, ours [`crate::ui_loot::LootLatch`]) — armed at the `CMSG_LOOT` send,
/// so the kneel is client-predicted — while a **remote** unit keys its descriptor:
/// [`UNIT_FLAG_LOOTING`] set and [`UNIT_FLAG_LOOT_SUPPRESS`] clear. The clip is authored
/// **clamp** (HumanMale: one 0.5 s sequence, no variations): kneel down, freeze in the rummage
/// pose; the rise back is the ordinary cross-fade to Stand when the trigger drops. Weapons stow
/// for free — row 50 carries the `WeaponFlags & 4` force-stow the per-animation sheath
/// reconcile already applies.
pub(super) const LOOT: u16 = 50;

/// `UNIT_FIELD_FLAGS` bit `0x400` — `UNIT_FLAG_LOOTING` (vmangos `UnitDefines.h`: "Displays loot
/// animation", set by `Player::SendLoot` for corpse loot, removed at `DoLootRelease`): the
/// **remote** half of the loot-kneel predicate (`0x6126db shr 0xa; and 1` — see [`LOOT`]); the
/// self unit reads its latch instead.
pub(super) const UNIT_FLAG_LOOTING: u32 = 0x400;

/// `UNIT_FIELD_FLAGS` bit `0x1000_0000` — must be CLEAR for a remote unit's loot kneel
/// (`0x612710`, byte-verified; the §5's one INFERRED residue is this bit's Blizzard *name*, not
/// its role). vmangos never sets it on players, so the gate is inert on our wire — carried for
/// the faithful predicate shape.
pub(super) const UNIT_FLAG_LOOT_SUPPRESS: u32 = 0x1000_0000;

/// MountSpecial (94) — the mounted flourish (the horse rears; decision 0441 P2). Plays on the
/// MOUNT model as a one-shot (§5-verified, wow-re `mount-composition.md`: MountSpecial routes to
/// the mount via the same `0x5fe2f0` op4 target as its locomotion), never on the rider — the
/// rider holds [`MOUNT`] throughout. Fired by the space-bar gate (self, locally at send time)
/// and by `SMSG_MOUNTSPECIAL_ANIM` (observed riders; our own echo is dropped in the net drain).
pub(crate) const MOUNT_SPECIAL: u16 = 94;

/// Movement direction/mode flag bits, matching the client's CMovement `MOVEMENTFLAGS` (cached at
/// `unit+0x9e8`; VERIFIED wow-5875-re RF-0057 + the jump §5 cross-check). The selector tests these
/// exactly as the binary does, so a streamed unit can eventually drop the server's raw `u32` straight in.
pub(crate) mod move_flags {
    pub const FORWARD: u32 = 0x1;
    pub const BACKWARD: u32 = 0x2;
    pub const STRAFE_LEFT: u32 = 0x4;
    pub const STRAFE_RIGHT: u32 = 0x8;
    /// TURN-left / TURN-right — the keyboard turn keys (vanilla A/D when not mouse-looking). The client
    /// rotates the facing while these are set (`0x7c4f30` heading integrate) and plays the turn-in-place
    /// foot-shuffle (11/12) when turning with no translation. Strafe (above) slides without turning.
    pub const TURN_LEFT: u32 = 0x10;
    pub const TURN_RIGHT: u32 = 0x20;
    /// WALK-mode (vs run). In the client this only scales the rate numerator (walk vs run speed), never
    /// the id choice. The net bridge reads it to extrapolate a `/walk`-toggled remote mover at walk speed.
    pub const WALK_MODE: u32 = 0x100;
    /// MOVEFLAG_LEVITATING — **the free-flight bit, and it works by SUPPRESSION.** It is the very
    /// first test in the client's per-frame swim decision `0x6030c0` (`0x6030d2 test ah,4` → bail to
    /// `0x6031fa`; VERIFIED, wow-re `collision/scratch/swim-transition.md`): set, and the whole
    /// water/depth decision is skipped — neither the ENTER arm nor the STOP arm runs, so liquid can
    /// no longer latch *or unlatch* [`SWIMMING`].
    ///
    /// That suppression is the entirety of GM flight. vmangos's `.cheat fly` sends
    /// `LEVITATING | SWIMMING | MOVED | FLYING` (`Player::SetFly`): `SWIMMING` puts the mover in the
    /// 3-D floating regime (gravity bypassed, vertical from the aim pitch), and `LEVITATING` stops
    /// the dry-land depth test from clearing it again the very next frame. `MOVED` (`0x800000`) and
    /// `FLYING` (`0x1000000`) carry no behaviour on this build — vmangos's own header marks both
    /// doubtful — and we model neither. Decision 0726.
    pub const LEVITATING: u32 = 0x400;
    /// JUMPING/FALLING — set for the whole airborne arc; drives the jump Special state.
    pub const FALLING: u32 = 0x2000;
    /// MOVEFLAG_FALLINGFAR — the arc has become a **far fall** (`0x633220`/`0x633240`; wow-re
    /// `land-anim-height-gate.md`, legs corrected by decision 0179): a *jump* (launch vz ≠ 0)
    /// latches once it descends 1/9 yd below its launch height; a *step-off fall* (launch vz = 0)
    /// latches at 500 ms airborne (≈ 2.41 yd of free fall — a fence hop never latches, a
    /// wagon-height drop latches just before the floor). Cleared only with FALLING at StopFalling
    /// (`0x7c6290`). Mid-air it swaps the pose to Fall(40) and it opens the landing-anim gate; a
    /// flat jump never descends below its takeoff, so its hang stays Jump(38).
    pub const FALLING_FAR: u32 = 0x4000;
    /// MOVEFLAG_ROOT — the server rooted this mover (death, until release). The controller MUST
    /// carry it in the root-apply ack's MovementInfo (vmangos `HandleMoveRootAck:715` KICKS a
    /// root-apply ack without it — live-verified 2026-07-11, `Movement.log`), and moving bits
    /// must never accompany it (they freeze the real client).
    pub const ROOT: u32 = 0x1000;
    pub const SWIMMING: u32 = 0x20_0000;
    /// MOVEFLAG_ONTRANSPORT — standing on a boat/zepp (1.12 bit 25, vmangos `MovementInfo.h:56`
    /// `0x02000000`; NOT the TBC-era 0x200). Set while the mover rides a transport's platform
    /// frame; every packet carrying it also carries the local-pose tail (decision 0438 phase 2).
    pub const ON_TRANSPORT: u32 = 0x0200_0000;
    /// MOVEFLAG_WATERWALKING — the liquid surface counts as walkable ground (Water Walking,
    /// Levitate, and the ghost form; decisions 0308/0866). Reference consumers: the liquid-mask
    /// selector `0x6315f0` (`0x63160d test eax,0x10000000`, taken *only* when not swimming) and the
    /// opcode-0x22 apply `0x61a430`.
    pub const WATER_WALKING: u32 = 0x1000_0000;
    /// MOVEFLAG_SAFE_FALL — **feather fall** (Slow Fall, Levitate; decision 0866). It has exactly
    /// one effect: the gravity integrate `0x7c5d20` picks its terminal clamp on this bit
    /// (`0x7c5d23 test [ecx+0x40],0x20000000`) — 7.0 yd/s `[0x87d898]` instead of the ordinary
    /// 60.148 `[0x87d894]`. wow-re's ledger labels it "in-water; selects swim gravity", which is the
    /// bit's *shape* read without the server-side name: vmangos sets it only from
    /// `SPELL_AURA_FEATHER_FALL`, and swimming is the separate [`SWIMMING`] (0x200000).
    pub const SAFE_FALL: u32 = 0x2000_0000;
    /// MOVEFLAG_HOVER — the body rests 1.0 yd above the ground (Levitate; decision 0866). VERIFIED
    /// as hover rather than a wade bit in wow-re `system/collision/collision.md`: the WALK resolver
    /// `0x6367b0` gates a `[0x7ff9d8]` = +1.0-yd surface offset on it, and the step-down reach
    /// widens by the same yard (`0x633e35`).
    pub const HOVER: u32 = 0x4000_0000;

    /// The bits a **server-authored move packet owns**, and the only ones it may write — the
    /// reference's flag *merge* mask, VERIFIED at `0x618c30 @0x618deb`
    /// (`new = old ^ ((old ^ wire) & 0x75a07dff)`; wow-re `self-addressed-move.md`, decision 0725).
    /// Applying a `MSG_MOVE_*` is not an assignment: bits outside this mask are kept from local
    /// state, because they are the client's own (`0x618c30` also holds a second mask,
    /// `0x75a01dff`, for a caller arm that passes a non-zero 4th arg — the plain state family
    /// passes zero and takes this one).
    ///
    /// Two of ours land outside it and the omissions are the point. **[`ON_TRANSPORT`]** (bit 25)
    /// is client-owned: a server-authored pose relocates you, it never boards or deboards you —
    /// which is why the self-move arm re-anchors the ride rather than dropping it. Bit 27 (the
    /// reference's free-advance selector) and bit 31 (its clock baseline) are CMovement internals
    /// with no benilla analog. Every bit we *do* model besides `ON_TRANSPORT` — the direction and
    /// turn bits, walk mode, root, [`FALLING`]/[`FALLING_FAR`], swim, water-walk — is inside.
    pub const SERVER_AUTHORED: u32 = 0x75a0_7dff;

    /// Any horizontal-movement direction bit (forward/back/strafe) — the client's `[9e8] & 0xf` gate.
    pub const ANY_MOVE: u32 = FORWARD | BACKWARD | STRAFE_LEFT | STRAFE_RIGHT;

    /// The **committed-lower-body** move mask that routes a one-shot to the masked overlay (decision
    /// 0087): the client's `[9e8] & 0x20003f` test at `0x5fe6dc` — every direction bit (`0x3f`,
    /// which folds in the keyboard turn keys) plus swim (`0x200000`). Any of these set ⇒ the legs are
    /// committed, so a swing/emote plays masked over them rather than full-body. (The client's
    /// *separate* facing-delta turn test `d58 & 0x1800` — a mouse turn-in-place with no move flag —
    /// is a field benilla does not model; keyboard turning is already covered here via `0x30`.)
    pub const ROUTE_COMMITTED_MOVE: u32 =
        FORWARD | BACKWARD | STRAFE_LEFT | STRAFE_RIGHT | TURN_LEFT | TURN_RIGHT | SWIMMING;

    /// The stationary-cast pin's "moving" test — the client's `[9e8] & 0x20000f` in the `0x5fde80`
    /// cast-override gate (wow-re `spell-visual-apply.md` §2.1, VERIFIED): the direction bits plus
    /// swim, and NOTHING else. The turn bits are absent — a caster turning in place (keys or a
    /// mouselook body-step) keeps the full-body pin. Distinct from [`ROUTE_COMMITTED_MOVE`]'s
    /// `0x20003f` (the one-shot route test at `0x5fe6dc` — a different byte site, a different
    /// mask); using that mask here made the pin flap at mouse-event cadence — the transient
    /// chase-step TURN flags demoted the hold to shuffle+overlay on every mouse-delta frame and
    /// re-pinned on every quiet one (decision 0491, the frostbolt right-drag jitter).
    pub const CAST_PIN_MOVE: u32 = ANY_MOVE | SWIMMING;
}

/// How fast a strafing unit's rendered root yaw eases toward its offset heading — the client blends
/// the display facing a **quarter of the remaining gap per frame** (`0x607ed0` tail, `×0.25`
/// `[0x8029b0]`); at the reference 60 fps that is the continuous exponential rate
/// `−ln(0.75) × 60 ≈ 17.3 /s` (we take the time-based form rather than aping the frame-rate
/// dependence). Applied by [`ease_strafe_yaw`], the pose owners' one blend.
const STRAFE_BLEND_RATE: f32 = 17.26;

/// Advance a unit's rendered yaw toward `aim + offset` by easing the **aim-relative offset** as a
/// plain scalar ([`STRAFE_BLEND_RATE`] exponential). For a stationary aim this is the same curve as
/// easing the absolute yaw — EXCEPT at the strafe flip (±90° → ∓90°), where the absolute
/// shortest-arc is an exact 180° tie that float noise resolves to either side ("sometimes it spins
/// around the back" — director). Offset space has no tie: the swing always passes through the aim,
/// i.e. around the front, and the spine/head twist gap unwinds through 0 instead of snap-flipping
/// at ±180°.
pub(crate) fn ease_strafe_yaw(current_yaw: f32, aim: f32, offset: f32, dt: f32) -> f32 {
    let cur = super::wrap_pi(current_yaw - aim);
    let eased = cur + (offset - cur) * (1.0 - (-STRAFE_BLEND_RATE * dt).exp());
    super::wrap_pi(aim + eased)
}

/// The strafe **body-heading offset** (radians) a unit's rendered root yaw sits from its aim — the
/// client's display-facing strafe blend (wow-5875-re `body-facing-pipeline.md` §3, `0x607ed0` tail):
/// ±π/2 for a pure strafe, ±π/4 when forward/back is also held (`flags & 3`). Yaw is left-positive
/// (WoW orientation and our Bevy yaw alike), so strafe-left is `+` — **mirrored while backpedaling**
/// (the client's sign fold `((flags>>1)&2) == (flags&2)`): a back-left diagonal faces the body
/// forward-right and backpedals along the movement line, legs never crossing. `0` when not strafing
/// (both strafe bits set cancel — no lateral motion results, unlike the client's left-bit-only fold,
/// an unobservable edge). The [`super::twist::BodyTwist`] counter-twist then walks the upper body
/// back toward the aim.
pub(crate) fn strafe_body_offset(flags: u32) -> f32 {
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};
    let left = flags & move_flags::STRAFE_LEFT != 0;
    let right = flags & move_flags::STRAFE_RIGHT != 0;
    if left == right {
        return 0.0;
    }
    let diagonal = flags & (move_flags::FORWARD | move_flags::BACKWARD) != 0;
    let magnitude = if diagonal { FRAC_PI_4 } else { FRAC_PI_2 };
    let back = flags & move_flags::BACKWARD != 0;
    if left != back {
        magnitude
    } else {
        -magnitude
    }
}

/// The per-frame movement descriptor that drives a unit's animation (wow-5875-re RF-0057). The player
/// controller fills it for our avatar from input; a streamed unit's is derived from its [`Spline`].
#[derive(Component, Clone, Copy, Default)]
pub(crate) struct MovementState {
    /// Live movement speed (yd/s) — `0` while standing. The rate-scaler's numerator and the
    /// walk/run threshold read this; it is already the *directional* speed (runBack when
    /// backpedaling). On the ground it's the horizontal speed; for a swimmer it's the flag-scalar
    /// 3D travel speed (swim/swimBack regardless of pitch — a vertical stroke plays at full rate).
    pub(crate) speed: f32,
    /// Vertical speed (yd/s, +up); non-zero while airborne. Its sign on the arc's FIRST airborne
    /// frame splits a **jump** (upward — the JumpStart/Jump bracket) from a **step-off fall** (the
    /// gait freeze) — the information the real client carries as the MSG_MOVE_JUMP event vs a bare
    /// StartFalling, which our flag-collapsed machine reads from the launch velocity instead.
    pub(crate) vertical_speed: f32,
    /// CMovement `MOVEMENTFLAGS` subset (see [`move_flags`]).
    pub(crate) flags: u32,
    /// Stand-state (`UNIT_FIELD_BYTES_1` byte 0): 0 Stand · 1 Sit · 3 Sleep · 4/5/6 chair low/med/high ·
    /// 8 Kneel — drives the idle pose / the sit-sleep-kneel transition while standing.
    pub(crate) stand_state: u8,
    /// **Stealthed** — `UNIT_FIELD_BYTES_1` byte 3's CREEP bit, the gate on [`STEALTH_WALK`] /
    /// [`STEALTH_STAND`]. Stamped by the driver from the unit's OWN descriptor every frame, self
    /// and remote alike (the client reads `[[unit+0x110]+0x213]` at select time), so [`unify`]'s
    /// legs never touch it: unlike the stand state there is nothing to predict controller-side —
    /// the crouch waits on the server's aura, exactly as the reference does.
    pub(crate) stealthed: bool,
    /// Riding a **flying** server spline (the taxi flight): the client's selector reads the active
    /// CMovement's spline flags (`[[unit+0x118]+0xa4]+0x18`) and plays Fly 135 when the wire Flying
    /// bit (0x200) is set — RF-0057 `0x5fd19c`. Stamped by [`unify`] from the entity's live
    /// [`Spline`] each frame (the same read-through the client does); the controller's stored
    /// component leaves it `false`.
    pub(crate) flying: bool,
}

/// A transition-bracketed animation state — a one-shot **enter** → **loop** → one-shot **exit**, as
/// opposed to the directly-cross-faded gaits. The ids are from the verified selector + the HumanMale.m2
/// sequence table (wow-5875-re; decisions 0047/0049).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Special {
    /// Airborne from a **jump** (the arc launched upward): JumpStart(37) → Jump(38) hang.
    /// **VERIFIED** (the swim-hop §5, wow-re `swim-jump-anim-law.md`): `0x60e480` case 0xbb
    /// (MSG_MOVE_JUMP) arms **37 directly**, local and observed alike, and 38 follows only by
    /// 37's own 833 ms window elapsing — a short arc (the ~0.24 s deep swim hop) never shows
    /// 38, which the Entering→`oneshot_finished` machine reproduces. (rf57b's dispatcher
    /// divergence is settled: `0x5fc3f0`'s events 0x25/0x26 → 38 is the separate
    /// emote/anim-event dispatcher, not the jump path.) FALLINGFAR
    /// latching mid-arc hands off to [`Special::Fall`]; the landing is [`jump_land_pick`]. A
    /// **step-off fall** never enters here — its gait freezes (the base selector's keep-current,
    /// via the takeoff-frozen flags/speed) until FALLINGFAR latches.
    Jump,
    /// Airborne and **fallen far** (FALLINGFAR latched): the Fall(40) loop, entered *directly* —
    /// the client plays 40 the tick the flag latches (`0x602c40`), no enter one-shot — so
    /// `enter_special` skips [`Mode::Entering`] for it. Lands like Jump, via [`jump_land_pick`].
    Fall,
    /// A stand-state pose keyed on the standState value (1 Sit · 3 Sleep · 8 Kneel): down → loop → up.
    Pose(u8),
}

impl Special {
    /// The one-shot played on entry (JumpStart / SitDown / SleepDown / KneelDown). Fall has no
    /// enter — `enter_special` sends it straight to its loop; its arm here is unreachable, kept
    /// for the exhaustive match.
    pub(super) fn enter(self) -> u16 {
        match self {
            Special::Jump => 37,
            Special::Fall => 40,
            Special::Pose(1) => 96,
            Special::Pose(3) => 99,
            Special::Pose(8) => 114,
            Special::Pose(_) => STAND,
        }
    }
    /// The looping middle (Jump hang / Fall / Sit / Sleep / Kneel).
    pub(super) fn loop_id(self) -> u16 {
        match self {
            Special::Jump => 38,
            Special::Fall => 40,
            Special::Pose(1) => 97,
            Special::Pose(3) => 100,
            Special::Pose(8) => 115,
            Special::Pose(_) => STAND,
        }
    }
    /// The one-shot played on a *pose's* exit (SitUp / SleepUp / KneelUp). An airborne state's
    /// landing never routes here — it is [`jump_land_pick`] (the `0x602c60` dispatcher) via
    /// `Mode::Land`; the Jump/Fall arms are unreachable by flow, kept for the exhaustive match.
    pub(super) fn exit(self) -> u16 {
        match self {
            Special::Jump | Special::Fall => STAND,
            Special::Pose(1) => 98,
            Special::Pose(3) => 101,
            Special::Pose(8) => 116,
            Special::Pose(_) => STAND,
        }
    }

    /// Whether starting to move cuts this state's exit short. A pose's stand-up (SitUp / …) is abandoned
    /// the instant the unit moves — you don't watch yourself stand up when you walk off; the gait
    /// cross-fade carries the half-pose into the walk. Jump/Fall never reach [`Mode::Exiting`] any
    /// more — their landing is [`Mode::Land`], a freely-overwritten pick (decisions 0083/0087) — so
    /// their `false` here is vestigial, kept for the exhaustive match.
    pub(super) fn interruptible_by_move(self) -> bool {
        matches!(self, Special::Pose(_))
    }
}

/// The jump-landing one-shot, from the flags at touchdown — the client's land dispatcher `0x602c60`
/// (wow-5875-re rf57b §2), transcribed: swimming → no landing anim (the recompute picks the swim
/// gait); stopped (`flags & 0xf == 0`) → JumpEnd **39**; moving — unless BACKWARD (`0x2`) or
/// WALK-mode (`0x100`) — → JumpLandRun **187**; a **backpedaling or walking** landing plays **no
/// one-shot at all**, dropping straight into the gait (the reference backpedals the instant it
/// touches down; 187 is a forward-run footplant and never plays backward — the "forward run flash
/// after a jump-then-hold-S" bug). `None` = no landing clip, go straight to `Mode::Gait`.
pub(super) fn jump_land_pick(flags: u32) -> Option<u16> {
    use move_flags::*;
    if flags & SWIMMING != 0 {
        None
    } else if flags & ANY_MOVE == 0 {
        Some(39)
    } else if flags & (BACKWARD | WALK_MODE) == 0 {
        Some(187)
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Mode {
    /// Cross-faded ground/swim gaits + idle.
    Gait,
    /// Playing a Special's enter one-shot; settles into its loop when the one-shot finishes.
    Entering(Special),
    /// Looping a Special; plays the exit one-shot when its condition clears.
    Looping(Special),
    /// Playing a *pose's* exit one-shot (SitUp/SleepUp/KneelUp — the `Special` being left + the exit
    /// clip id). Returns to Gait when it finishes — unless a new Special, or movement, interrupts it
    /// first. (Jump's landing is [`Mode::Land`], not this — its bracket was killed per decision 0083.)
    Exiting(Special, u16),
    /// The jump **landing** as a plain, freely-overwritten pick (decisions 0083/0087 (d)):
    /// [`jump_land_pick`] chooses from the input *at touchdown* (`flags`) — JumpEnd 39 stopped,
    /// JumpLandRun 187 moving forward; a backpedal/walk landing plays no clip and never enters this
    /// mode — and re-picks the instant any movement flag changes, so land-then-press runs
    /// immediately, land-then-release stands immediately, and a direction flip at landing shows no
    /// stale-direction flash. Plays through to Gait only if the input holds steady. Not a
    /// non-preemptible bracket.
    Land { id: u16, flags: u32 },
    /// Playing an action one-shot **full-body on the base track** (the bone-0 route of decision
    /// 0087): a standing melee swing (decision 0073), a standing `/cheer`-class emote — or a
    /// **mid-air cast/emote** (decision 0864: the airborne route test masks only COMBAT ids, so a
    /// jump-in-place cast replaces the hang on bone 0, exactly the ref's full-body mid-air cast).
    /// `under` is the Special whose base clip this play replaced (`None`: it replaced a gait).
    /// While `under` is airborne the one-shot obeys the client's airborne-freeze: a finished clip
    /// clamps and holds its last frame, flag changes are keep-current no-ops, and only a Special
    /// *edge* exits — the FALLINGFAR latch's Fall (played ONCE, at the latch edge
    /// `0x61a820` — 0864's per-tick re-assert was §5-refuted, decision 0868; a clip armed
    /// after the latch holds like any other) and the `0x602c60` land pick at touchdown. Grounded
    /// (`under == None`) it returns to Gait when it finishes; a new one restarts it — and, like
    /// [`Mode::Land`], any movement-flag change from `flags` (captured at play time) re-picks
    /// immediately: the client's locomotion re-arm overwrites bone 0 blindly on the change (the
    /// same re-arm decision 0280 named), so a shot fired standing yields to the run the instant
    /// the player moves instead of sliding the clip out. The *masked* route never enters here —
    /// it plays as an overlay beside `Mode`, leaving the base machine (this enum) running
    /// underneath.
    Swing {
        id: u16,
        flags: u32,
        under: Option<Special>,
    },
}

/// The gait an idle/moving unit should play in [`Mode::Gait`], most-specific id first with fallbacks so a
/// model lacking the ideal clip steps down rather than snapping (RF-0057 core, minus the Special states —
/// jump and the sit/sleep/kneel poses are handled by the state machine). `walk_speed` is the unit's own
/// walk speed (the run boundary is 2× it). `ready` is the engaged combat idle (decision 0073), played
/// only when standing on the ground — locomotion outranks it, and a swimming engaged unit treads
/// water (the client's own gate order).
pub(super) fn gait_candidates(
    state: &MovementState,
    walk_speed: f32,
    ready: Option<u16>,
    ranged_load: Option<u16>,
) -> &'static [u16] {
    use move_flags::*;
    let f = state.flags;
    // Swimming (RF-0057 core, `[9e8] & 0x200000`) — the VERIFIED `0x5fd100` cascade (the swim §5's
    // TU-E, wow-re `swim-mechanism.md`): **TURN > STRAFE > BACKWARD > FORWARD**, ids turn→41,
    // strafeL(0x4)→43 / strafeR(0x8)→44 (SwimLeft/SwimRight — names byte-read from
    // AnimationData.dbc), back→45, fwd→42, idle→41. So a turning swimmer treads water whatever its
    // travel bits, and a strafe diagonal (fwd+strafe, back+strafe) plays the side-stroke, not 42/45.
    if f & SWIMMING != 0 {
        return if f & (TURN_LEFT | TURN_RIGHT) != 0 {
            &[41, 0]
        } else if f & STRAFE_LEFT != 0 {
            &[43, 42, 41, 0]
        } else if f & STRAFE_RIGHT != 0 {
            &[44, 42, 41, 0]
        } else if f & BACKWARD != 0 {
            &[45, 41, 0]
        } else if f & FORWARD != 0 {
            &[42, 41, 0]
        } else {
            &[41, 0]
        };
    }
    // A flying server spline — the taxi ride (RF-0057 `0x5fd19c`, between the swim block and the
    // backward test in the byte cascade): the spline's wire Flying bit selects Fly 135 outright,
    // before the backward and speed branches — a 32 yd/s taxi plays Fly, never Sprint 143. The
    // one-step fallback mirrors AnimationData (Fly → 0 Stand); every shipped taxi mount authors
    // real Fly clips (the gryphon carries two variations).
    if state.flying {
        return &[135, 0];
    }
    // Backward dominates strafe (RF-0057 `[9e8] & 2` → WalkBackwards 13).
    if f & BACKWARD != 0 {
        return &[13, 4, 0];
    }
    // Stealthed and moving — the prowl (RF-0057 `0x5fd1d3`, sitting between the backward branch and
    // the speed tail; [`STEALTH_WALK`]). The gate outranks the ENTIRE speed tail, so a stealthed
    // unit creeps at any speed, while a *backpedaling* one plays WalkBackwards — backward is tested
    // first. The Walk 4 fallback is `AnimationData.dbc`'s own Fallback for row 119, which is also
    // what the baked PlayableAnimationLookup resolves 119 to on a model lacking the clip: HumanMale
    // and NightElfFemale author both stealth clips, `druidcat.m2` authors NEITHER (119→4, 120→0), so
    // a prowling cat shows its ordinary walk — in the reference too, through the same resolver.
    if state.stealthed && f & ANY_MOVE != 0 {
        return &[STEALTH_WALK, 4, 0];
    }
    // Ground gaits by live speed (RF-0057 core): fast-run ≥ 11, run > 2× walk, else walk.
    // (Strafe currently routes here to Run/Walk; the dedicated-shuffle question is under §5 verification.)
    if f & ANY_MOVE != 0 {
        let s = state.speed;
        return if s >= FAST_RUN_SPEED {
            &[143, 5, 4, 0]
        } else if s > 2.0 * walk_speed {
            &[5, 4, 0]
        } else {
            &[4, 0]
        };
    }
    // Turning in place (the turn keys with no translation): the foot-shuffle (RF-0057 `0x5fd3f0` →
    // ShuffleLeft 11 / ShuffleRight 12). Only reached when not moving — moving with a turn curves the
    // run path and plays the gait above.
    if f & TURN_LEFT != 0 {
        return &[11, 0];
    }
    if f & TURN_RIGHT != 0 {
        return &[12, 0];
    }
    // Standing + engaged in melee: the weapon-class Ready idle (decision 0073 — gated on engagement,
    // not sheath state; outranks the chair poses, since a fighting unit isn't seated).
    if let Some(r) = ready {
        return match r {
            26 => &[26, 25, 0],
            27 => &[27, 25, 0],
            28 => &[28, 25, 0],
            _ => &[25, 0],
        };
    }
    // Standing in the ranged stance with auto-repeat armed (the client's `0x5fd460`, gate
    // `sheath CUR == 2 && [+0xd58] & 0x200` — local player only, decision 0099 phase 5): the
    // ranged weapon's Load/Hold idle ([`ranged_load_anim`]). Placed after the engaged Ready
    // (they can't co-occur — auto-shot never sets the engaged GUID) and before the chair loops
    // (OUR ordering; the client's call-site order vs the chair resolver isn't pinned).
    if let Some(l) = ranged_load {
        return match l {
            105 => &[105, 25, 0],
            106 => &[106, 25, 0],
            109 => &[109, 25, 0],
            110 => &[110, 25, 0],
            111 => &[111, 25, 0],
            112 => &[112, 25, 0],
            _ => &[25, 0],
        };
    }
    // Standing: a chair-sit loop (server stand-states 4/5/6 — no down/up triple) else Stand. The
    // transition-bracketed poses (1 Sit / 3 Sleep / 8 Kneel) are Special, handled before this is called.
    // State 2 (generic SIT_CHAIR) is verified VESTIGIAL (decision 0280): the client's resolver row
    // for 2 writes no override (`0x5fd644` returns a bool; the 0xd0 sentinel stands), so falling to
    // plain Stand here is the faithful render, not a gap.
    // Standing stealthed: the prowl idle ([`STEALTH_STAND`]) — placed *after* the chair loops, since
    // its resolver (`0x5fd830`) is the last in the chain and the stand-state resolver precedes it.
    match state.stand_state {
        4 => &[102, 0],
        5 => &[103, 0],
        6 => &[104, 0],
        _ if state.stealthed => &[STEALTH_STAND, STAND],
        _ => &[STAND],
    }
}

/// The local auto-repeat **standing idle** id — the client's `0x5fd460` → LUT `0x5fd530`
/// (byte-verified, wow-re `ranged-shot-anim.md`): the RANGED-slot item's subclass picks a held
/// Load/Hold clip — Bow → LoadBow 105 · Gun/Crossbow → LoadRifle 106 · Thrown → LoadThrown 112 ·
/// Wand → HoldThrown 111 · anything else → ReadyUnarmed 25. **Not** ReadyBow/AttackBow: no code
/// in the client plays those rows, and the per-shot caster fire animation is a verified NEGATIVE
/// (an exhaustive `PlayAnimation` caller sweep) — the shot itself shows only the missile (and the
/// bow-prop string flex, a deferred polish).
pub(super) fn ranged_load_anim(ranged: Option<(u8, u8)>) -> u16 {
    match ranged {
        Some((2, 2)) => 105,           // Bow → LoadBow
        Some((2, 3) | (2, 18)) => 106, // Gun / Crossbow → LoadRifle
        Some((2, 16)) => 112,          // Thrown → LoadThrown
        Some((2, 19)) => 111,          // Wand → HoldThrown
        _ => 25,                       // no/odd ranged item → ReadyUnarmed
    }
}

/// The Load clip's post-nock **HOLD twin** — the drawn pose held between shots (LoadBow 105 →
/// HoldBow 109 · LoadRifle 106 → HoldRifle 110 · LoadThrown 112 → HoldThrown 111; the wand's
/// idle is already the hold). **INTERIM** (decision 0400 §2's dispatch, Q4): the director's
/// reference observable — the character nocks once, then "holds the arrow much longer in the
/// bow before each shot" — plus the recorded-but-underived `[+0xd58] |= 0x400` post-shot
/// hold-pose layer (`0x5fc3f0`, flagged open in wow-re and 0370's deferred list). The idle
/// plays the Load clip ONCE, then the driver swaps to this twin until the next shot resets it;
/// the dispatched verdict pins the real mechanism.
pub(super) fn ranged_hold_anim(load: u16) -> u16 {
    match load {
        105 => 109,
        106 => 110,
        112 => 111,
        other => other,
    }
}

/// Whether `id` is one of the ranged Load clips whose single pass arms the hold twin
/// ([`ranged_hold_anim`]). The wand's HoldThrown 111 is already a hold — not a Load.
pub(super) fn is_ranged_load(id: u16) -> bool {
    matches!(id, 105 | 106 | 112)
}

/// The victim **defense-reaction** id (decision 0279 — the `$CPP` dispatch, byte-verified
/// `0x624a01` → `0x60ec00`): DODGE/DEFLECTS → Dodge(30) · BLOCKS → ShieldBlock(24) · PARRY keys
/// the victim's own **mainhand** through the `0x60ec98` LUT (read off `WoW.exe` `.text`, its
/// third distinct weapon bucketing after the swing and Ready tables): 1H axes/maces/swords/
/// exotic1H/misc **and dagger** → Parry1H(21) · 2H axes/maces/swords/exotic2H → Parry2H(22) ·
/// polearm/staff/spear → Parry2HL(23) · **fist → ParryUnarmed(20)** · ranged/obsolete/
/// subclass>0x11/non-weapon/empty → the client bails, no clip (`0x60ec3a`/bucket 4 → ret).
/// Every other victimState plays nothing.
pub(super) fn defense_anim(victim_state: u32, main: Option<(u8, u8)>) -> Option<u16> {
    match victim_state {
        2 | 8 => Some(30), // DODGE / DEFLECTS → Dodge
        5 => Some(24),     // BLOCKS → ShieldBlock
        3 => match main {
            Some((2, sub)) => match sub {
                0 | 4 | 7 | 0xb | 0xe | 0xf => Some(21), // 1H family + misc + dagger
                1 | 5 | 8 | 0xc => Some(22),             // 2H family
                6 | 0xa | 0x11 => Some(23),              // polearm / staff / spear
                0xd => Some(20),                         // fist → ParryUnarmed
                _ => None,                               // ranged/obsolete/oddball: bail
            },
            _ => None, // empty or non-weapon mainhand: bail
        },
        _ => None,
    }
}

/// The per-packet melee swing one-shot ids (decision 0073's tables) — what the whiff slow-down
/// (decision 0279) is allowed to touch when it finds them on the masked overlay.
pub(super) fn is_swing_id(id: u16) -> bool {
    matches!(id, 16..=19 | 85 | 87 | 88 | 117)
}

/// The client's COMBAT-anim classifier (`0x5fcc10` — wow-re `combat-anim-fastpath.md` §3, the
/// exhaustive byte-decoded table, cross-confirmed against `anim-composition-model.md`): the gate
/// on `PlayAnimation`'s combat fast-path (decision 0406). A combat clip requested while another
/// combat clip is playing is NOT armed — the current clip's rate doubles and the request defers.
/// Members: the swings (16–19 main, 85/86 dagger, 87/88/117 off), the specials (57/58/118 —
/// Eviscerate's weapon-remapped spin among them — and 59), the defenses (20–23 parry, 24
/// shield-block, 30 dodge), 10, 36, and Kick 95. The Ready idles (25–28) are NOT combat — a
/// swing over a ready idle arms normally.
pub(super) fn is_combat_anim(id: u16) -> bool {
    matches!(id, 10 | 16..=24 | 30 | 36 | 57..=59 | 85..=88 | 95 | 117 | 118)
}

/// Whether `cands` (this frame's [`gait_candidates`] pick) is bare `[STAND]` — the one precedence
/// slot the looping **state-emote idle** (`UNIT_NPC_EMOTESTATE`: `/dance`, NPC cooking/sweeping
/// flavor loops) is allowed to fill instead, exactly where Stand itself would otherwise sit.
/// Everything that already outranks plain Stand keeps outranking it: swim/backward/moving/turning
/// return their own arrays above, the Ready idle requires `ready.is_some()` (i.e. engaged), and a
/// chair-loop stand-state (4/5/6) has its own arm before the default. `current_special` (jump, the
/// sit/sleep/kneel poses) is checked by the caller *before* [`gait_candidates`] is even invoked, so
/// this predicate never has to account for Specials itself — they simply never reach here. Movement
/// starting again drops straight out of the state-emote idle for the same reason Stand does: the
/// very next frame's `cands` is no longer bare.
///
/// The **prowl idle** counts as bare too: [`STEALTH_STAND`] comes from `0x5fd830`, the *last*
/// resolver in the client's chain, so anything with a resolver of its own outranks it. (OUR
/// ordering, on that structural argument — where the emote-state idle sits in the real chain is
/// unpinned; a stealthed unit holding an emote state is degenerate either way.)
pub(super) fn is_bare_stand(cands: &[u16]) -> bool {
    cands == [STAND] || cands == [STEALTH_STAND, STAND]
}

/// The candidate array for a state-emote idle occupying the bare-Stand slot: the resolved anim id
/// first, `STAND` as the fallback should the model lack it — resolved through the same
/// [`super::find_resolved`] path as every other gait candidate (decision 0082). Call only when
/// [`is_bare_stand`] held for this frame's `cands`.
pub(super) fn state_emote_gait(emote_anim: u16) -> [u16; 2] {
    [emote_anim, STAND]
}

/// The mainhand swing animation id (decision 0073 — byte-verified `0x6246a0`): keyed on the wielded
/// item's `(class, subclass)`. Anything that isn't an equipped melee weapon — empty hand, non-weapon
/// item, fist weapon, every ranged/wand, obsolete(9), unknown subclasses — swings AttackUnarmed(16).
pub(super) fn swing_anim_main(wielded: Option<(u8, u8)>) -> u16 {
    match wielded {
        Some((2, sub)) => match sub {
            0x0 | 0x4 | 0x7 | 0xb | 0xe => 17, // Attack1H: 1H axe/mace/sword/exotic/misc
            0x1 | 0x5 | 0x8 | 0xc => 18,       // Attack2H: 2H axe/mace/sword/exotic
            0x6 | 0xa | 0x11 | 0x14 => 19,     // Attack2HL: polearm/staff/spear/fishing pole
            0xf => 85,                         // Attack1HPierce: dagger
            _ => 16,                           // AttackUnarmed: fist/ranged/wand/obsolete/unknown
        },
        _ => 16,
    }
}

/// The offhand swing animation id (decision 0073, HitInfo bit `0x4`): a dagger stabs (88
/// AttackOffPierce), any other equipped weapon swings 87 (AttackOff), an empty/non-weapon offhand
/// punches (117 AttackUnarmedOff).
pub(super) fn swing_anim_off(wielded: Option<(u8, u8)>) -> u16 {
    match wielded {
        Some((2, 0xf)) => 88,
        Some((2, _)) => 87,
        _ => 117,
    }
}

/// The draw/stow clip for one weapon slot, by the item's **sheathe-type** byte — the byte-verified
/// pick (wow-re `sheath-anim-pick.md`, 8 sites, byte-identical): `(1 << (type & 0x1f)) & 0x88` →
/// HipSheath(90) for types {3, 7}, Sheath(89) for everything else (back-mounts, shields, staves).
/// The slot's own byte is the *only* input — mainhand, offhand, shield and ranged all run the same
/// test, each on its own record.
pub(super) fn sheath_clip(sheath_type: u8) -> u16 {
    if (1u32 << (sheath_type & 0x1f)) & 0x88 != 0 {
        90
    } else {
        89
    }
}

/// The ranged-handling anims exempt from the `&0x10` force-stow while ranged-drawn —
/// **byte-verified** (wow-re `ranged-sheath-exempt-autorepeat.md`, the 2026-07-15 §5): the
/// exemption is the predicate `0x5fe180` (sole caller `0x5fe04c`, the **CUR==2 path only**;
/// index table `0x5fe1a8`, targets `0x5fe1a0`), returning true for exactly these nine ids —
/// the ranged Load/Hold/Attack family. **ReadyThrown 108 is NOT exempt** and genuinely stows;
/// the real thrown wind-up survives by the snap **bracket** around every ranged kit play
/// (`driver`'s CastHold-ranged override), not by an exemption. The explicit trio compare at
/// `0x5fe037`–`0x5fe04a` (105/106/112) is a fast-path subset.
const SHEATH_RANGED_EXEMPT: [u16; 9] = [46, 49, 105, 106, 107, 109, 110, 111, 112];

/// The per-animation sheath reconcile (decision 0080 — the client's `0x5fdf80`, run after every
/// animation pick): given the unit's current sheath state and the playing clip's
/// `AnimationData.dbc` WeaponFlags, the state the policy *forces*, or `None` to leave it alone.
/// Priority-ordered, first match wins, every force is a SNAP:
///
/// 1. flag `&4` → stow (casts, swim, mount, sit-chair, loot);
/// 2. mounted → stow — the persistent mounted draw-block (`0x5fdfd9`: the reconcile forces
///    state 0 on every recompute while a mount model is attached, so weapons sit at their sheath
///    points and the manual toggle can never stick — wow-re `sheath-policy.md` §3, VERIFIED;
///    dismount does NOT restore the pre-mount state);
/// 3. flag `&0x10` → stow (emotes, unarmed attacks, sit-ground/kneel, bow/rifle/thrown shots) —
///    except the three ranged-handling anims while ranged-drawn;
/// 4. engaged **or** flag `&0x20` → draw melee (armed attacks, the Ready idles, fishing) — only
///    while not ranged-drawn;
/// 5. a remote unit with no force pulls back to the server's descriptor byte (`0x5fe16e`; the
///    local player's committed state is never server-reconciled).
pub(super) fn reconcile_sheath(
    cur: u8,
    anim: u16,
    weapon_flags: u32,
    engaged: bool,
    local: bool,
    server_byte: u8,
    mounted: bool,
) -> Option<u8> {
    if weapon_flags & 4 != 0 {
        return Some(0);
    }
    if mounted {
        return Some(0);
    }
    if cur == 2 {
        if weapon_flags & 0x10 != 0 && !SHEATH_RANGED_EXEMPT.contains(&anim) {
            return Some(0);
        }
    } else {
        if weapon_flags & 0x10 != 0 {
            return Some(0);
        }
        if engaged || weapon_flags & 0x20 != 0 {
            return Some(1);
        }
    }
    if !local && cur != server_byte {
        return Some(server_byte);
    }
    None
}

/// Whether a **looping base arm** keeps the deterministic HEAD variation instead of rolling a
/// fresh one — the client's re-zero gate on the arm helper (`0x5fdba0`, wow-re
/// `loop-replay-fidget.md` §5b): every base (re-)arm carries `variationIdx = −1` (a weighted
/// roll) **unless** the unit has an auto-attack target, is holding a cast/channel, or the
/// *outgoing* clip was a combat/cast/ready id — then it is forced to `0` (the head). This is why
/// a relaxed unit "looks around" on each Stand re-arm while a fighting one holds one steady idle.
/// The outgoing-id families here approximate the client's four classifier calls
/// (`0x5fcc10`/`0x5fcbb0`/`0x5fde40`/`0x5fde60`, exact id sets unpinned): the melee swings
/// (16–19), the ready stances (25–29, the 0111-cited set), and the spell ready/cast poses
/// (51–54) — engagement and the cast hold carry the main weight regardless (decision 0123).
pub(super) fn arm_forces_head(engaged: bool, casting: bool, outgoing: u16) -> bool {
    engaged || casting || matches!(outgoing, 16..=19 | 25..=29 | 51..=54)
}

/// The victim wound-flinch id (`0x60ea70`, decision 0111 §5.3 — byte-verified): decided **solely**
/// by `(severity, engaged)`. Crit (`HitInfo & 0x80`) → CombatCritical; the victim engaged in
/// melee (its auto-attack-target GUID set, `[unit+0xc48]`) → CombatWound; else StandWound.
pub(super) fn wound_anim(hit_info: u32, engaged: bool) -> u16 {
    if hit_info & 0x80 != 0 {
        10 // CombatCritical
    } else if engaged {
        9 // CombatWound
    } else {
        8 // StandWound
    }
}

/// Whether the wound overlay covers the **full body** (the client forces op4's key-bone to `-1` =
/// bone 0) or stays **masked** to the upper-body subtree — the two byte-decoded mechanisms
/// (decision 0111 §5.2, `0x60eae8` / `0x60eb9a`):
///
/// - **(A), all ids:** the victim's current bone-0 pose is a combat-ready stance {25–29} — a
///   weapon-drawn victim standing between its own swings flinches full-body. `base_anim` is the
///   **resolved** id actually driving the base track (the client reads the armed record,
///   `[block0+0xf8]`), so mid-swing the base is the swing — not ready — and the flinch masks.
/// - **(B), StandWound(8) only:** genuinely stationary — the client's `[+0x118]+0x40 & 0x20200f`
///   (move/jump/swim; note the keyboard-turn bits `0x30` are **not** in the mask) — **and not
///   mounted** (the secondary-blend note's `[unit+0xdc]==0` companion clause; a mounted rider's
///   flinch never replaces the seat pose, decision 0441). The transport-substate companion is
///   still a state benilla doesn't model.
///
/// Everything else is masked: the legs keep the base animation untouched.
pub(super) fn wound_full_body(id: u16, base_anim: u16, flags: u32, mounted: bool) -> bool {
    matches!(base_anim, 25..=29)
        || (id == 8
            && !mounted
            && flags & (move_flags::ANY_MOVE | move_flags::FALLING | move_flags::SWIMMING) == 0)
}

/// The wound overlay's decay amplitude λ₀ — the client seeds `+0x108 = 0.75f` on the op4
/// `linkFlag=0` path (`0x712682`, decision 0111): the flinch peaks at 75% wound, never fully
/// replacing the pose underneath. (A normal clip transition's outgoing-pose fade uses 1.0.)
pub(super) const WOUND_AMPLITUDE: f32 = 0.75;

/// The wound overlay's Bevy graph weight this frame (decision 0111): the client's kernel computes
/// `λ = smoothstep(t) · λ₀` with `smoothstep(t) = (3 − 2t)·t²` and `t` the remaining fraction of
/// the decay window (`0x714880`: t runs 1 → 0 over the clip's span, so λ **decays** λ₀ → 0 — then
/// the slot self-releases), and blends `out = primary + (secondary − primary)·λ`. Bevy's
/// normalized weighted blend gives `out = others·base/(others+w) + w·wound/(others+w)`, so the
/// weight that lands the blend exactly on λ is `w = others · λ/(1−λ)` — `others` being the total
/// weight of the other animations driving the same subtree. Bounded: λ ≤ 0.75 ⇒ `w ≤ 3·others`.
pub(super) fn wound_weight(remaining_frac: f32, others: f32) -> f32 {
    let t = remaining_frac.clamp(0.0, 1.0);
    let lambda = (3.0 - 2.0 * t) * t * t * WOUND_AMPLITUDE;
    others * lambda / (1.0 - lambda)
}

/// The client's `_rand` — the MSVCRT LCG (`state × 214013 + 2531011`, output `(state >> 16) &
/// 0x7fff`; byte-verified wow-re `rf36-rand-stub.md` at `0x7400e5`) — the roll feeding op4's
/// per-play **variation pick** (`ModelAnimations::pick_variation`) and its **replay-count roll**
/// ([`replay_count`] — the second `_rand` site). Owned exactly rather than delegating to a host
/// RNG, per the determinism guidance in the same note; one stream shared by every play, like the
/// client's single CRT stream.
pub(super) fn msvc_rand(state: &mut u32) -> u16 {
    *state = state.wrapping_mul(214013).wrapping_add(2531011);
    ((*state >> 16) & 0x7fff) as u16
}

/// The per-arm **replay-count roll** (wow-re `loop-replay-fidget.md`, op4's second `_rand` site
/// `0x712692..0x7126cd`): `R = max(1, min + ⌊roll·(max−min)/32768⌋)` from the sequence's
/// `(minReplay, maxReplay)`. The client multiplies `R` into the play window (`0x7126d8`) — a
/// clamp-flag one-shot runs its timeline `R` times before freezing; loop-flag sequences ignore it.
/// Benilla expresses the same window as a repeat count on the one-shot play. `(0, 0)` → 1.
pub(super) fn replay_count(replay: (u32, u32), roll: u16) -> u32 {
    let (min, max) = replay;
    let extra = if max > min {
        (u64::from(roll) * u64::from(max - min) / 32768) as u32
    } else {
        0
    };
    (min + extra).max(1)
}

/// The engaged standing idle (decision 0073 — the `0x5fd360` arm's weapon-class Ready pick,
/// `0x5fcdc0`). Note the buckets differ from the swing table: fist **and** dagger ready as 1H.
pub(super) fn ready_anim(main: Option<(u8, u8)>) -> u16 {
    match main {
        Some((2, sub)) => match sub {
            0x0 | 0x4 | 0x7 | 0xb | 0xd | 0xe | 0xf => 26, // Ready1H (incl. fist + dagger)
            0x1 | 0x5 | 0x8 | 0xc => 27,                   // Ready2H
            0x6 | 0xa | 0x11 => 28,                        // Ready2HL
            _ => 25,                                       // ReadyUnarmed: ranged/obsolete/unknown
        },
        _ => 25,
    }
}

/// The Special state a unit is in this frame, if any. Airborne splits three ways (wow-re
/// `land-anim-height-gate.md` + rf57b §2): FALLINGFAR latched → **Fall** (the 40 loop); else a
/// **jump** arc (launched upward, `jump_arc`) → Jump (the 37/38 bracket); else a **step-off fall**
/// → `None` — the base selector's keep-current freeze: the gait keeps playing off the
/// takeoff-frozen flags/speed until FALLINGFAR latches or the unit lands. Standing with a
/// transition-able stand-state (sit/sleep/kneel) → Pose; movement suppresses a pose.
pub(super) fn current_special(mv: &MovementState, jump_arc: bool) -> Option<Special> {
    if mv.flags & move_flags::FALLING != 0 {
        if mv.flags & move_flags::FALLING_FAR != 0 {
            Some(Special::Fall)
        } else if jump_arc {
            Some(Special::Jump)
        } else {
            None
        }
    } else if mv.flags & move_flags::ANY_MOVE == 0 && matches!(mv.stand_state, 1 | 3 | 8) {
        Some(Special::Pose(mv.stand_state))
    } else {
        None
    }
}

/// Where a requested one-shot (a swing or an emote) is routed this play (decision 0087, wow-re
/// `anim-composition-model.md` §3/§5 at `923ac7bc`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum OneShotRoute {
    /// Onto the SpineLow-subtree **masked overlay** ([`AnimClip::upper_node`]): the torso plays the
    /// clip while the legs keep whatever the base is doing (run / sit / jump-arc). `Mode` is untouched.
    Masked,
    /// **Full-body on the base track** (bone 0): the clip replaces the base, legs included. Standing
    /// idle only — the clip's authored leg keys then read as-is (a swing lunges, a `/cheer` is
    /// full-body, a `/clap`/`/bow` reads waist-up because the *asset* barely keys the legs).
    FullBody,
}

/// The **CLASS_A** membership (wow-re `0x5fed90`) — the maskable-eligible set that gates the whole
/// state-route block; a non-CLASS_A id is always full-body. The load-bearing memberships were
/// byte-decoded (17/66/68/80 ∈; the 37–45 jump/swim/locomotion band excluded); the patchy interior of
/// the wide ranges is INFERRED (wow-re note §3 Open) but never load-bearing here — the only ids that
/// reach [`route_oneshot`] are swings, emotes, and the spell-kit cast anims (32/33/51–54 — wow-re's
/// own CLASS_A gloss: "swings, emotes, casts"), all squarely inside these ranges.
fn is_class_a(id: u16) -> bool {
    matches!(id,
        2 | 8..=10 | 14..=36 | 46..=49 | 51..=90 | 105..=113 | 117..=118 | 122..=138 | 185..=186 | 195)
}

/// The **COMBAT** membership (wow-re `0x5fcc10`) — the set whose airborne test can route to the mask
/// (a mid-jump swing). Byte-decoded memberships: **17 ∈**, and **66/68/80 ∉** (the emotes never mask
/// on airborne alone). Every swing id (16–19/85/87/88/117) is in it; no emote id is — and no CAST id
/// (32/33/51–54) either, which is why a jump-in-place cast routes FULL-BODY and replaces the hang
/// (decision 0864), while the same cast over a moving jump masks via the frozen-in move bits.
fn is_combat(id: u16) -> bool {
    matches!(id, 10 | 16..=24 | 30 | 36 | 57..=59 | 85..=88 | 95 | 117 | 118)
}

/// The **forced-full-body** carve-outs (wow-re §3): Death-class `{1,6,131,132}` (`0x5fda90`) and the
/// sit-transition ids `{57,58,118}` (`0x5fec60`) route to bone 0 regardless of state. Kept faithful
/// though benilla never feeds these through the one-shot path.
fn is_forced_full_body(id: u16) -> bool {
    matches!(id, 1 | 6 | 131 | 132 | 57 | 58 | 118)
}

/// Route a requested one-shot (swing/emote id) by the unit's **live state**, per play — the client's
/// `esi` decision `0x5fe6c8..0x5fe74d` (decision 0087, wow-re §3/§5): masked onto the SpineLow overlay
/// when the lower body is committed — **moving/turning/swimming** (`[9e8] & 0x20003f`), a **non-Stand
/// stand-state** (seated/sleep/kneel/chair, `standState ≠ 0`), or a **combat** id while **airborne**
/// (`activeCMovement+0x40 & 0x2000`); **full-body** on bone 0 when standing idle (none of those). The
/// id gates only *which* tests apply (CLASS_A gates the block; COMBAT gates the airborne test); it
/// never decides maskability by itself — the same Attack1H is full-body standing and masked running.
pub(super) fn route_oneshot(id: u16, flags: u32, stand_state: u8) -> OneShotRoute {
    if is_forced_full_body(id) || !is_class_a(id) {
        return OneShotRoute::FullBody;
    }
    let committed_lower = flags & move_flags::ROUTE_COMMITTED_MOVE != 0
        || stand_state != 0
        || (is_combat(id) && flags & move_flags::FALLING != 0);
    if committed_lower {
        OneShotRoute::Masked
    } else {
        OneShotRoute::FullBody
    }
}

/// `AnimationData` ids whose playback rate the client scales by movement speed (wow-5875-re `0x5fee80`):
/// the locomotion gaits. An id outside this set plays at rate 1×.
const RATE_SCALED: &[u16] = &[4, 5, 11, 12, 13, 37, 38, 39, 42, 43, 44, 45, 135, 143, 187];

/// The playback rate for a clip given the unit's live speed (wow-5875-re `0x5fe2f0`): `speed / moveSpeed`
/// for a rate-scaled locomotion clip, else 1×. (The client also divides by the model's scale magnitude,
/// 1.0 for an unscaled character.) Guarded on `moveSpeed > 0`.
pub(super) fn playback_rate(clip: &AnimClip, speed: f32) -> f32 {
    if clip.move_speed > 0.0 && RATE_SCALED.contains(&clip.anim_id) {
        speed / clip.move_speed
    } else {
        1.0
    }
}

/// Build the unified movement view, in precedence order: a populated [`MovementState`] (our avatar,
/// driven by the controller) wins; else a remote player's live `moveFlags` + extrapolation speed
/// ([`RemoteMotion`], from the relayed `MSG_MOVE_*` stream); else a creature's server spline (it moves
/// forward at the path speed); else stationary. The remote flags are raw CMovement bits, which already
/// match [`move_flags`], so the same selector animates a remote player's walk/run/strafe/backpedal/turn.
///
/// `creature_swimming` is the creature legs' swim state ([`crate::net::CreatureSwimming`] — the wire
/// never carries `SWIMMING` for a creature, so it's derived from the water over its feet): it folds
/// the flag into the spline/stationary views, so a pathing water creature plays Swim 42 and an idle
/// one treads water (SwimIdle 41) instead of running/standing on the lakebed. Ignored for the
/// self/remote legs — their flag arrives properly.
pub(super) fn unify(
    movement: Option<&MovementState>,
    remote: Option<&RemoteMotion>,
    spline: Option<&Spline>,
    creature_swimming: bool,
) -> MovementState {
    // The flying-spline fact is read through to the live `Spline` on EVERY leg — the client's
    // selector reads the active CMovement's spline flags at select time (RF-0057 `0x5fd19c`),
    // so a self ride (MovementState leg) and a remote taxi (spline leg) both fly.
    let flying = spline.is_some_and(|s| !s.grounded);
    if let Some(m) = movement {
        return MovementState { flying, ..*m };
    }
    if let Some(r) = remote {
        return MovementState {
            speed: r.speed,
            flags: r.flags,
            vertical_speed: r.vertical_velocity,
            flying,
            ..default()
        };
    }
    let swim = if creature_swimming {
        move_flags::SWIMMING
    } else {
        0
    };
    let s = spline.map(|s| s.speed()).filter(|s| *s > MOVING_EPSILON);
    MovementState {
        speed: s.unwrap_or(0.0),
        flags: swim | if s.is_some() { move_flags::FORWARD } else { 0 },
        flying,
        ..default()
    }
}

#[cfg(test)]
mod tests;
