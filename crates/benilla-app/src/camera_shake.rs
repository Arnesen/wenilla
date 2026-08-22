//! **Camera shake** — the thump a heavy creature's footfall puts through the camera, and the
//! one-off jolt as its body lands (B298; decision 1540).
//!
//! Not an animation event. `$SHK` exists in the M2 vocabulary but **`CGUnit_C::HandleAnimEvent`
//! does not decode it** — all 73 tags it dispatches were enumerated and `$SHK` is not among them;
//! the only two handlers that decode it hang off the GameObject (typemask `0x20`) and
//! DynamicObject (`0x40`) trampolines. A `$SHK` authored on a creature M2 logs
//! `UNHANDLEDANIMEVENT` and does nothing, so no creature shake can be `$SHK`-driven — and in fact
//! neither Ancient authors one.
//!
//! The real chain is **data, not animation**:
//!
//! - **`CreatureModelData.FootstepShakeSize`** (field 11) → a `CameraShakes.dbc` row id, fired from
//!   the **visual** footfall channel — the per-foot plant tags (`$FL0`/`$FR0`/`$RL0`/`$RR0`…), the
//!   same stream [`crate::footprints`] reads. **Not `$FSD`**, which is the sound handler's alone.
//! - **`CreatureModelData.DeathThudShakeSize`** (field 12) → the same table, fired on `$DTH`.
//!
//! Only 25 of the 430 shipped models carry a footstep shake, and the set is exactly the
//! thumping-giant list; 49 carry one of the two. See `benilla-extract shakecensus`.
//!
//! ## The law (wow-re `ui/scratch/camera-shake-law.md`, §5-verified)
//!
//! **Emission — the footstep**, gated in this order (`0x5fbf70`): not hovering · `BYTES_1` byte 3
//! bit `0x2` clear (our stealth bit) · not a player-ghost · **`|camera − footplant|² ≤ 2500`
//! (50 yd)**. The shake sits *outside* the `showfootprints` CVar gate and *outside* the 25 yd gate
//! that follows it, so it reaches twice as far as a footprint decal and does not turn off with
//! them. The position is the planted foot's world point, exactly as the decal derives it.
//!
//! **Emission — the death thud** (`0x625c30`): **no gates at all** — no hover, no ghost, no
//! distance, no CVar — at the unit's own world position.
//!
//! **Per live record, per frame** (`0x511760`/`0x5116e0`):
//!
//! ```text
//! t = (now − start) + phase                 // seconds; phase is a TIME pre-roll, not an angle
//! if !(t < duration) → retire
//! A = amplitude / 36                        // the DBC column is inches; yards on the wire out
//! d² = |eye − pos|²                         // pos is snapshotted at spawn
//! if d² > 6400 → contribute nothing (the record survives)
//! if d² >   81 → A *= 0.7^((√d² − 9) / 9)
//! a = A · sin(2π · frequency · t)
//! if shake_type == 1 → a *= exp(−coefficient · t)
//! ```
//!
//! Three corrections to the conventional column map, all from the consumer: **`Phase` is a time
//! pre-roll in seconds**, not an angle — it advances the sine *and* the decay, and shortens the
//! real life to `duration − phase`; **`Duration` is a hard cutoff with no taper**, so a
//! `shake_type == 0` row is cut off mid-swing at full strength; and **`ShakeType` is a one-bit
//! decay switch**, not a type — `== 1` exactly enables the `exp` envelope, and `Coefficient` is
//! that rate in 1/s and is dead data on every `shake_type == 0` row. The decay is base **e**.
//!
//! **What moves.** A pure world-space **translation of the eye** — no rotation, no FOV change; the
//! look-at target is rebuilt as `eye + forward`, so it rides along. `Direction` selects an axis in
//! the **followed unit's body frame**, re-read every frame, which is why turning the player
//! rotates an in-flight horizontal shake:
//!
//! | `direction` | reference (WoW axes) | ours (Bevy) |
//! |---|---|---|
//! | 0 | `(a·cos φ, a·sin φ, 0)` | `a ·` forward |
//! | 1 | `(−a·sin φ, a·cos φ, 0)` | `a ·` left |
//! | 2 | `(0, 0, a)` | `a ·` up |
//!
//! (`bevy = (−wow.y, wow.z, −wow.x)` — decision 0002 — and our yaw about `+Y` equals the WoW
//! facing, so axis 0/1 land exactly on the unit's forward/left.) **Every creature row is
//! `direction = 2`**, so in practice a footstep shake is purely vertical; the other two axes are
//! reachable only from the spell-side table and are implemented for completeness. Axes 0 and 1 are
//! *not* independent — both sum into the horizontal plane.
//!
//! **Combination.** Three slots, one per axis, each keeping the single strongest live shake — same-axis
//! shakes do **not** sum, the losers are dropped outright, and at most three shakes play in a frame.
//! Ties keep the incumbent, and the walk is oldest→newest, so ties keep the **older** record.
//!
//! **Lifetime.** A record holds a *snapshot* position and no reference to its source, so a shake
//! fully outlives the creature that spawned it. The whole evaluation is skipped — offset zero,
//! nothing expiring — while the followed unit is **swimming**.
//!
//! Not modelled, deliberately: the reference's second wholesale free on `SetTarget(nullptr)`, and
//! its destructor's unlink-without-free (a shipped leak — our `Vec` cannot reproduce it). The
//! flying-spline (taxi) arm of the skip is noted in [`CameraShakes::evaluate`].

use std::f32::consts::TAU;

use bevy::prelude::*;

use benilla_formats::{CameraShake, CameraShakeCatalog};

use crate::creature_anim::{footfall_side, move_flags, AnimSoundEvent, MovementState};
use crate::entities::{BoneAttach, Creatures};
use crate::net::{Embodied, NetEntity, ObjectStore};
use benilla_assets::{AssetSet, LockRecover, WorldAssets};
use benilla_world::schedule::WorldStage;
use benilla_world::view::WorldCamera;

/// Beyond this the record contributes nothing but is **not** retired (`0x5116e9`, `80 yd²`).
const CULL_DISTANCE_SQ: f32 = 6400.0;
/// Inside this the shake plays at full authored strength (`9 yd²`).
const FULL_DISTANCE_SQ: f32 = 81.0;
/// The falloff's half-life base and its per-9-yd exponent divisor.
const FALLOFF_BASE: f32 = 0.7;
const FALLOFF_SPAN: f32 = 9.0;
/// The footstep emitter's own radius, camera→footplant (`0x5fc00a`, `2500 yd²` = 50 yd). **The
/// camera, not the player** — the same correction wow-re applied to `dist2-gate.md` this round.
const EMIT_DISTANCE_SQ: f32 = 2500.0;
/// `CameraShakes.Amplitude` is authored in inches; the client scales it at spawn (`0x511d78`).
const INCHES_TO_YARDS: f32 = 1.0 / 36.0;

/// One live shake: the authored row, where it happened, and when it started.
struct LiveShake {
    row: CameraShake,
    /// World position, **snapshotted at spawn** — the record never looks at its source again.
    pos: Vec3,
    /// App-clock seconds at spawn.
    start: f32,
}

/// The live shake set and the eye offset it composes for this frame.
#[derive(Resource, Default)]
pub(crate) struct CameraShakes {
    live: Vec<LiveShake>,
}

impl CameraShakes {
    /// Enqueue a shake at a world point. The row is copied in whole — the DBC is the only source
    /// of shape, and nothing downstream re-reads the catalog.
    fn add(&mut self, row: CameraShake, pos: Vec3, now: f32) {
        self.live.push(LiveShake {
            row,
            pos,
            start: now,
        });
    }

    /// Retire what has expired and compose this frame's offset.
    ///
    /// `swimming` is the reference's skip (`0x50ea87`/`0x50ea8b`): while the followed unit swims,
    /// the whole block is bypassed — the offset is zero **and nothing expires**, so a shake
    /// resumes mid-flight on surfacing. The sibling arm of that skip, a flying (taxi) spline, is
    /// not modelled here: we have no flying-spline predicate at the camera seat, and the case is a
    /// heavy creature's footfall within 50 yd of a taxi path. Recorded rather than guessed.
    fn evaluate(&mut self, eye: Vec3, facing_yaw: f32, now: f32, swimming: bool) -> Vec3 {
        if swimming {
            return Vec3::ZERO;
        }
        // Retire on the *unattenuated* clock: distance decides contribution, never lifetime.
        self.live.retain(|s| s.elapsed(now) < s.row.duration);

        // One slot per axis, each holding (compare key, signed value). The walk is oldest→newest
        // and the compare is strict, so a tie keeps the older record — the reference's own
        // `jne`-skips-the-write shape.
        let mut slots: [Option<(f32, f32)>; 3] = [None; 3];
        for s in &self.live {
            let Some((key, value)) = s.sample(eye, now) else {
                continue;
            };
            let axis = s.row.direction as usize;
            let Some(slot) = slots.get_mut(axis) else {
                continue; // direction ≥ 3 writes nothing (and corrupts the reference's frame)
            };
            if slot.is_none_or(|(best, _)| key > best) {
                *slot = Some((key, value));
            }
        }

        // Axis 0/1 are the followed unit's forward/left, 2 is world up (module doc's table). Our
        // yaw about +Y is the WoW facing, so forward is exactly `(−sin, 0, −cos)`.
        let (sin, cos) = facing_yaw.sin_cos();
        let forward = Vec3::new(-sin, 0.0, -cos);
        let left = Vec3::new(-cos, 0.0, sin);
        let value = |i: usize| slots[i].map_or(0.0, |(_, v)| v);
        forward * value(0) + left * value(1) + Vec3::Y * value(2)
    }
}

impl LiveShake {
    /// Seconds into the shake, including `phase` — which is a **time pre-roll**, so it advances
    /// both the sine and the decay and shortens the real life to `duration − phase`.
    fn elapsed(&self, now: f32) -> f32 {
        (now - self.start) + self.row.phase
    }

    /// This record's `(compare key, signed offset)` at `eye`, or `None` when out of cull range.
    ///
    /// The compare key is the **distance-attenuated amplitude** — a per-record quantity that does
    /// not swing with the sine — per wow-re's reading of the accumulator's stored slot.
    fn sample(&self, eye: Vec3, now: f32) -> Option<(f32, f32)> {
        let d2 = eye.distance_squared(self.pos);
        if d2 > CULL_DISTANCE_SQ {
            return None;
        }
        let mut amp = self.row.amplitude * INCHES_TO_YARDS;
        if d2 > FULL_DISTANCE_SQ {
            amp *= FALLOFF_BASE.powf((d2.sqrt() - FALLOFF_SPAN) / FALLOFF_SPAN);
        }
        let t = self.elapsed(now);
        let mut value = amp * (TAU * self.row.frequency * t).sin();
        // `shake_type == 1` exactly — the one-bit decay switch, base e, rate in 1/s.
        if self.row.shake_type == 1 {
            value *= (-self.row.coefficient * t).exp();
        }
        Some((amp, value))
    }
}

pub(crate) struct CameraShakePlugin;

impl Plugin for CameraShakePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CameraShakes>()
            .add_systems(Startup, load_shakes.after(AssetSet::Open))
            // Present, like the footprints and the footstep sounds: the same event stream, after
            // the frame's animation drive and transforms have settled the foot bones.
            //
            // Capture-gated in step with the applier ([`crate::player`] schedules that one). The
            // pairing is not cosmetic: the applier is what retires expired records, so an emitter
            // still running while it is gated off would push a record per footfall that nothing
            // ever reaps — a slow leak across a long capture.
            .add_systems(
                Update,
                fire_shakes
                    .in_set(WorldStage::Present)
                    .run_if(not(resource_exists::<crate::run_mode::CaptureMode>)),
            );
    }
}

/// Read `CameraShakes.dbc` into its catalog resource.
fn load_shakes(mut commands: Commands, assets: Option<Res<WorldAssets>>) {
    let Some(assets) = assets else { return };
    let table = {
        let mut chain = assets.chain.lock_recover();
        benilla_formats::load_camera_shakes(&mut chain)
    };
    match table {
        Ok(t) => {
            debug!("camera_shake: {} presets", t.len());
            commands.insert_resource(Shakes(t));
        }
        Err(e) => warn!("camera_shake: CameraShakes.dbc failed to load: {e:#}"),
    }
}

/// `CameraShakes.dbc`, loaded once.
#[derive(Resource)]
struct Shakes(CameraShakeCatalog);

/// Enqueue a shake for each qualifying footfall and death thud on the frame's event stream.
#[allow(clippy::too_many_arguments)]
fn fire_shakes(
    mut events: MessageReader<AnimSoundEvent>,
    time: Res<Time>,
    units: Query<(
        &NetEntity,
        &GlobalTransform,
        Option<&BoneAttach>,
        Option<&benilla_world::rig_anim::RigPose>,
    )>,
    parents: Query<&ChildOf>,
    roots: Query<(
        Option<&ObjectStore>,
        Option<&MovementState>,
        Option<&NetEntity>,
    )>,
    joints: Query<&GlobalTransform>,
    camera: Query<&GlobalTransform, With<WorldCamera>>,
    creatures: Option<Res<Creatures>>,
    shakes: Option<Res<Shakes>>,
    mut live: ResMut<CameraShakes>,
) {
    if events.is_empty() {
        return;
    }
    let (Some(creatures), Some(shakes)) = (creatures, shakes) else {
        return;
    };
    let Ok(eye) = camera.single().map(|t| t.translation()) else {
        return;
    };
    let now = time.elapsed_secs();
    for ev in events.read() {
        let thud = &ev.ident == b"$DTH";
        if !thud && footfall_side(&ev.ident).is_none() {
            continue; // `$FSD` is the sound handler's; only the VISUAL channel shakes
        }
        let Ok((net, transform, attach, pose)) = units.get(ev.entity) else {
            continue;
        };
        // The shake reads the ROOT unit's own model row — a mount's row is never stored there, so
        // a rider on a kodo gets the kodo's *footprints* and not its shake (`0x607a00` writes only
        // the decal fields). The root is also where the state gates live.
        let mut root = ev.entity;
        while let Ok(child_of) = parents.get(root) {
            root = child_of.parent();
        }
        let Ok((store, movement, root_net)) = roots.get(root) else {
            continue;
        };
        let display = root_net.unwrap_or(net).display_id;
        let Some(id) = display.and_then(|d| {
            if thud {
                creatures.death_thud_shake(d)
            } else {
                creatures.footstep_shake(d)
            }
        }) else {
            continue; // the overwhelming majority of models shake nothing
        };
        let Some(row) = shakes.0.get(id) else {
            continue;
        };
        if thud {
            // No gates whatsoever, and the unit's own world position — not a bone.
            live.add(*row, transform.translation(), now);
            continue;
        }
        // The footstep's three state gates, in the reference's order.
        if movement.is_some_and(|m| m.flags & move_flags::HOVER != 0) {
            continue;
        }
        if let Some(store) = store {
            if store.0.unit_is_stealthed() || store.0.player_is_ghost() {
                continue;
            }
        }
        // The planted foot: the event's own marker through the live joint, exactly as the decal
        // derives it. No marker/joint = the unit origin.
        let foot = attach
            .zip(pose)
            .and_then(|(a, p)| {
                let (bone, offset) = a.markers.get(&ev.ident).copied()?;
                p.posed_point(joints.get(p.joints_root).ok()?, bone, offset)
            })
            .unwrap_or_else(|| transform.translation());
        if eye.distance_squared(foot) > EMIT_DISTANCE_SQ {
            continue;
        }
        live.add(*row, foot, now);
    }
}

/// The applier's read of the followed unit: its facing (the shake's body frame) and whether it is
/// swimming (the reference's skip).
type FollowedUnit = (&'static Transform, Option<&'static MovementState>);

/// Add this frame's shake to the camera's seated eye.
///
/// Filtered on [`WorldCamera`], never on `Camera3d`: the portrait booths are `Camera3d`s too, and a
/// bare `Camera3d` query here would shake an off-screen booth — the same trap that once yanked the
/// booths to the scenario eye and blanked every portrait. `WorldCamera` and `FlyCam` sit on the one
/// entity `control` seats, which is the entity this must add to.
///
/// Runs **after** the camera is seated, which is what makes the falloff honest: `control` rewrites
/// the base pose every frame, so the transform this reads is the un-shaken eye rather than last
/// frame's shaken one. A zero offset writes nothing at all, preserving the camera's bit-equality
/// no-op gate (decision 1362) — a still camera stays bit-stable and its propagation stays quiet.
pub(crate) fn apply_camera_shake(
    mut live: ResMut<CameraShakes>,
    time: Res<Time>,
    mut camera: Query<&mut Transform, With<WorldCamera>>,
    player: Query<FollowedUnit, (With<Embodied>, Without<WorldCamera>)>,
) {
    let Ok(mut cam) = camera.single_mut() else {
        return;
    };
    let (yaw, swimming) = player.single().map_or((0.0, false), |(t, mv)| {
        (
            t.rotation.to_euler(EulerRot::YXZ).0,
            mv.is_some_and(|m| m.flags & move_flags::SWIMMING != 0),
        )
    });
    let offset = live.evaluate(cam.translation, yaw, time.elapsed_secs(), swimming);
    if offset != Vec3::ZERO {
        cam.translation += offset;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CameraShakes.dbc` row **1** — the Ancient Protector's / kodo's footstep, verbatim.
    const FOOTSTEP_1: CameraShake = CameraShake {
        id: 1,
        shake_type: 1,
        direction: 2,
        amplitude: 2.0,
        frequency: 3.0,
        duration: 0.4,
        phase: 0.06,
        coefficient: 1.0,
    };
    /// Row **2** — the Ancient of Lore/War, the giants and the dragons. Amplitude 7.0.
    const FOOTSTEP_2: CameraShake = CameraShake {
        amplitude: 7.0,
        ..FOOTSTEP_1
    };

    fn at(row: CameraShake, pos: Vec3) -> CameraShakes {
        let mut s = CameraShakes::default();
        s.add(row, pos, 0.0);
        s
    }

    /// The signed vertical offset a shake produces, with the eye at `d` yards from it.
    fn vertical(row: CameraShake, d: f32, t: f32) -> f32 {
        at(row, Vec3::ZERO).evaluate(Vec3::X * d, 0.0, t, false).y
    }

    /// `Phase` is a **time pre-roll in seconds**, not an angle: at `t = 0` the sine is already
    /// advanced to `2π·f·phase`. Row 1 opens at `sin(2π·3·0.06) = +0.905`, so a footstep kicks the
    /// eye UP first — the verdict's own observable.
    #[test]
    fn phase_is_a_time_preroll_and_the_first_kick_is_upward() {
        let expected = (2.0 / 36.0) * (TAU * 3.0 * 0.06).sin() * (-0.06f32).exp();
        let got = vertical(FOOTSTEP_1, 0.0, 0.0);
        assert!((got - expected).abs() < 1e-6, "{got} vs {expected}");
        assert!(got > 0.0, "the first kick is UP, not down: {got}");
        // Read as an angle instead, the opening sample would be sin(0.06) ≈ 0.06 — 15× smaller.
        let as_angle = (2.0 / 36.0) * 0.06f32.sin();
        assert!(got > as_angle * 10.0, "phase must not be read as an angle");
    }

    /// `Phase` also shortens the real life: the record retires at `elapsed + phase >= duration`.
    #[test]
    fn phase_shortens_the_life() {
        let mut s = at(FOOTSTEP_1, Vec3::ZERO);
        // 0.33 s in: 0.33 + 0.06 = 0.39 < 0.4, still alive.
        assert!(s.evaluate(Vec3::ZERO, 0.0, 0.33, false).y != 0.0);
        assert_eq!(s.live.len(), 1);
        // 0.35 s in: 0.41 >= 0.4 — gone, a full 0.05 s before its nominal duration.
        assert_eq!(s.evaluate(Vec3::ZERO, 0.0, 0.35, false), Vec3::ZERO);
        assert!(s.live.is_empty(), "retired at duration − phase");
    }

    /// `Duration` is a hard cutoff with **no taper** — a `shake_type == 0` row is still at full
    /// authored strength on its last frame. (The creature rows all decay; this is the spell shape.)
    #[test]
    fn duration_is_a_cutoff_not_a_taper() {
        let undecayed = CameraShake {
            shake_type: 0,
            phase: 0.0,
            duration: 1.0,
            frequency: 0.25, // a quarter-cycle at t = 1 → sine near its peak
            ..FOOTSTEP_1
        };
        let last = vertical(undecayed, 0.0, 0.999);
        let peak = 2.0 / 36.0;
        assert!(
            (last - peak).abs() < 1e-3,
            "no taper: {last} should still be ~{peak}"
        );
        assert_eq!(
            vertical(undecayed, 0.0, 1.0),
            0.0,
            "and then it is simply gone"
        );
    }

    /// The decay is base **e** at `coefficient` 1/s, and only when `shake_type == 1`.
    #[test]
    fn the_decay_switch_is_one_bit() {
        let t = 0.2;
        let decayed = vertical(FOOTSTEP_1, 0.0, t);
        let plain = vertical(
            CameraShake {
                shake_type: 0,
                ..FOOTSTEP_1
            },
            0.0,
            t,
        );
        let ratio = decayed / plain;
        let expected = (-(t + 0.06)).exp();
        assert!(
            (ratio - expected).abs() < 1e-5,
            "base-e decay on the full elapsed (phase included): {ratio} vs {expected}"
        );
    }

    /// Full strength inside 9 yd; `0.7^((d−9)/9)` beyond; **nothing past 80 yd — but the record
    /// survives**, so walking back into range resumes it.
    #[test]
    fn the_distance_falloff_culls_without_retiring() {
        let near = vertical(FOOTSTEP_2, 0.0, 0.0);
        assert_eq!(
            vertical(FOOTSTEP_2, 9.0, 0.0),
            near,
            "≤ 9 yd is full strength"
        );
        let far = vertical(FOOTSTEP_2, 18.0, 0.0);
        assert!(
            (far / near - 0.7f32).abs() < 1e-5,
            "one 9-yd span out = ×0.7, got {}",
            far / near
        );
        let mut s = at(FOOTSTEP_2, Vec3::ZERO);
        assert_eq!(s.evaluate(Vec3::X * 81.0, 0.0, 0.0, false), Vec3::ZERO);
        assert_eq!(s.live.len(), 1, "culled, not retired");
    }

    /// Same-axis shakes do **not** sum — the strongest wins outright — and the key is the
    /// distance-attenuated amplitude, so a tie keeps the OLDER record.
    #[test]
    fn same_axis_shakes_do_not_sum() {
        let mut s = CameraShakes::default();
        s.add(FOOTSTEP_1, Vec3::ZERO, 0.0); // amplitude 2
        s.add(FOOTSTEP_2, Vec3::ZERO, 0.0); // amplitude 7 — wins
        let both = s.evaluate(Vec3::ZERO, 0.0, 0.0, false).y;
        let alone = vertical(FOOTSTEP_2, 0.0, 0.0);
        assert!(
            (both - alone).abs() < 1e-6,
            "the loser is dropped, not added: {both} vs {alone}"
        );
        // A tie keeps the incumbent, which is the older — the reference's jne-skips-the-write.
        let mut t = CameraShakes::default();
        t.add(FOOTSTEP_1, Vec3::ZERO, 0.0);
        t.add(FOOTSTEP_1, Vec3::X * 40.0, 0.0); // same row, further away ⇒ strictly smaller key
        assert_eq!(t.evaluate(Vec3::ZERO, 0.0, 0.0, false).y, alone / 3.5);
    }

    /// `Direction` is an axis in the followed unit's **body frame**: 2 is up, 0 is forward, 1 is
    /// left — so turning the player rotates an in-flight horizontal shake.
    #[test]
    fn direction_selects_the_body_frame_axis() {
        let up = at(FOOTSTEP_1, Vec3::ZERO).evaluate(Vec3::ZERO, 0.0, 0.0, false);
        assert!(
            up.x.abs() < 1e-7 && up.z.abs() < 1e-7 && up.y > 0.0,
            "{up:?}"
        );

        let surge = CameraShake {
            direction: 0,
            ..FOOTSTEP_1
        };
        // Yaw 0: Bevy forward is −Z.
        let f = at(surge, Vec3::ZERO).evaluate(Vec3::ZERO, 0.0, 0.0, false);
        assert!(f.y.abs() < 1e-7 && f.x.abs() < 1e-6 && f.z < 0.0, "{f:?}");
        // Yaw +90°: forward becomes −X.
        let turned =
            at(surge, Vec3::ZERO).evaluate(Vec3::ZERO, std::f32::consts::FRAC_PI_2, 0.0, false);
        assert!(turned.z.abs() < 1e-6 && turned.x < 0.0, "{turned:?}");

        let sway = CameraShake {
            direction: 1,
            ..FOOTSTEP_1
        };
        let l = at(sway, Vec3::ZERO).evaluate(Vec3::ZERO, 0.0, 0.0, false);
        assert!(l.y.abs() < 1e-7 && l.z.abs() < 1e-6 && l.x < 0.0, "{l:?}");
    }

    /// A `direction` the reference cannot index writes nothing (rather than panicking on our side).
    #[test]
    fn an_out_of_range_direction_contributes_nothing() {
        let bad = CameraShake {
            direction: 3,
            ..FOOTSTEP_1
        };
        assert_eq!(
            at(bad, Vec3::ZERO).evaluate(Vec3::ZERO, 0.0, 0.0, false),
            Vec3::ZERO
        );
    }

    /// Swimming bypasses the whole block: zero offset, and **nothing expires** — the shake resumes
    /// on surfacing.
    #[test]
    fn swimming_freezes_rather_than_retires() {
        let mut s = at(FOOTSTEP_1, Vec3::ZERO);
        assert_eq!(s.evaluate(Vec3::ZERO, 0.0, 10.0, true), Vec3::ZERO);
        assert_eq!(s.live.len(), 1, "long past its duration, still not retired");
        assert_eq!(s.evaluate(Vec3::ZERO, 0.0, 10.0, false), Vec3::ZERO);
        assert!(s.live.is_empty(), "and it retires the moment we surface");
    }
}
