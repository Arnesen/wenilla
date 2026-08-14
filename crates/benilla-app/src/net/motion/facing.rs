//! Facing: the wire's `MonsterMoveFacing` resolution ([`resolve_facing`]) and the client-local
//! idle re-face ([`face_target`] — a stationary mob squares up on its target with no packet at
//! all), with the [`FacingStep`] latch the anim layer's turn-shuffle reads (decision 0123).

use benilla_assets::coords::bevy_to_wow;
use benilla_protocol::{EntityKind, MonsterMoveFacing};
use bevy::prelude::*;

use super::super::{ActiveMover, GuidIndex, NetEntity, ObjectStore};
use super::{yaw_of, RemoteMotion, Spline};

/// Radians/second an idle creature turns to face its target — a smooth turn, not a snap. Tunable
/// (the *look* is the director's): the real client caps a per-frame face turn at a `turnRate` and the
/// display-facing chase runs it fast (wow-re object-layer: `facing_interp 0x6103d0`, capped by
/// `turnRate`; the display chase `~turnRate·8`). This is the one knob if the turn reads too fast/slow.
const FACE_TARGET_TURN_RATE: f32 = 8.0;

/// The remaining yaw error (radians) under which a facing ease counts as **settled** — the
/// [`FacingStep`] latch drops and the anim layer's turn-shuffle releases back to Stand. The
/// client's own small/large-delta thresholds (`0x80c5c4`/`0x80c5c8`, decision 0123) aren't
/// transcribed; this is an eyeballable stand-in (~3°). Shared with the remote facing interp
/// ([`super::remote`]) — the same latch drives a standing remote's mouse-turn shuffle.
pub(super) const FACING_SETTLED: f32 = 0.05;

/// A stationary unit's idle re-face ([`face_target`]) is still **stepping its yaw** this frame:
/// the signed remaining delta (WoW yaw, positive = counterclockwise = turning left). The anim
/// layer folds it into the unit's turn view — the client's facing-delta shuffle latch
/// (`0x607ed0` bits `0x800`/`0x1000`, wow-re `loop-replay-fidget.md` §5b; decision 0123) — so a
/// squaring-up creature foot-shuffles instead of pivoting frozen, and each shuffle's return to
/// Stand re-rolls the idle variation. Removed the frame the ease settles.
#[derive(Component)]
pub(crate) struct FacingStep(pub(crate) f32);

/// Turn `cur` toward `goal` by at most `max_step` radians, the **short way** — the byte-verified client
/// facing interp (`facing_interp`, wow-re object-layer `0x6103d0`): wrap the delta into `[0, 2π)`, take
/// the shorter arc, cap the step at `max_step`. Returns `cur` (wrapped) when already within `~0`.
pub(super) fn turn_toward(cur: f32, goal: f32, max_step: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let delta = (goal - cur).rem_euclid(TAU); // wrap to [0, 2π)
    let (mag, dir) = if delta > PI {
        (TAU - delta, -1.0) // shorter to turn negative
    } else {
        (delta, 1.0)
    };
    if mag < 1.0e-4 {
        return cur.rem_euclid(TAU);
    }
    (cur + dir * mag.min(max_step)).rem_euclid(TAU)
}

/// Resolve a [`MonsterMoveFacing`] to a WoW orientation (radians) for a unit at `unit_pos` (raw WoW).
/// `Angle` is the orientation verbatim; `Spot`/`Target` are the horizontal bearing from the unit to
/// the point / the target unit's position (`atan2(dy, dx)`, the WoW convention that maps straight to
/// [`Quat::from_rotation_y`]). `target_pos` looks up another unit's raw-WoW position for `Target`.
/// `None` when there is nothing to face — [`MonsterMoveFacing::None`], an unknown/unstreamed target,
/// or a point coincident with the unit (a degenerate bearing that would spin it to 0).
pub(in crate::net) fn resolve_facing(
    facing: MonsterMoveFacing,
    unit_pos: [f32; 3],
    target_pos: impl FnOnce(u64) -> Option<[f32; 3]>,
) -> Option<f32> {
    let bearing = |to: [f32; 3]| {
        let (dx, dy) = (to[0] - unit_pos[0], to[1] - unit_pos[1]);
        (dx * dx + dy * dy > 1e-6).then(|| dy.atan2(dx))
    };
    match facing {
        MonsterMoveFacing::None => None,
        MonsterMoveFacing::Angle(a) => Some(a),
        MonsterMoveFacing::Spot(spot) => bearing(spot),
        MonsterMoveFacing::Target(guid) => target_pos(guid).and_then(bearing),
    }
}

/// Turn each **idle** creature to face its `UNIT_FIELD_TARGET` — the client's own local re-face. A
/// stationary meleeing mob squares up on its victim even though the server sends **no** facing for it
/// (vmangos `SetInFront` is server-only; verified by a live sniff — no `MONSTER_MOVE` arrives when you
/// attack a standing mob). A moving unit (a [`Spline`]) faces its travel direction instead; a remote
/// player ([`RemoteMotion`]) and the body we drive ([`ActiveMover`]) own their facing — all excluded. The
/// turn is capped per frame at [`FACE_TARGET_TURN_RATE`], toward the horizontal bearing to the target.
#[allow(clippy::type_complexity)]
pub(in crate::net) fn face_target(
    mut commands: Commands,
    time: Res<Time>,
    index: Res<GuidIndex>,
    candidates: Query<
        (Entity, &NetEntity, &ObjectStore),
        (Without<Spline>, Without<RemoteMotion>, Without<ActiveMover>),
    >,
    mut transforms: Query<&mut Transform>,
    // A remote player's latch belongs to the facing interp in [`super::remote`] — the cleanup
    // here must not sweep it (the two systems would fight over the component every frame).
    latched: Query<Entity, (With<FacingStep>, Without<RemoteMotion>)>,
) {
    let max_step = FACE_TARGET_TURN_RATE * time.delta_secs();
    // Collect (unit, goal facing) from immutable position reads first, then apply — so the target's
    // Transform and the unit's Transform are never borrowed mutably at the same time.
    let mut turns: Vec<(Entity, f32)> = Vec::new();
    for (e, net, store) in &candidates {
        if net.kind != EntityKind::Unit {
            continue; // a GameObject sits at its authored facing; players are excluded above
        }
        let Some(target) = store.0.unit_target() else {
            continue; // no target → nothing to face
        };
        let Some(&te) = index.0.get(&target) else {
            continue; // target not streamed to us
        };
        let (Ok(unit_t), Ok(target_t)) = (transforms.get(e), transforms.get(te)) else {
            continue;
        };
        let (u, t) = (
            bevy_to_wow(unit_t.translation),
            bevy_to_wow(target_t.translation),
        );
        let (dx, dy) = (t[0] - u[0], t[1] - u[1]);
        if dx * dx + dy * dy < 1.0e-4 {
            continue; // essentially on top of each other — no meaningful bearing
        }
        turns.push((e, dy.atan2(dx)));
    }
    let mut stepping: bevy::ecs::entity::EntityHashSet = default();
    for (e, goal) in turns {
        if let Ok(mut tf) = transforms.get_mut(e) {
            let cur = yaw_of(tf.rotation);
            let new = turn_toward(cur, goal, max_step);
            tf.rotation = Quat::from_rotation_y(new);
            // The turn-shuffle latch (decision 0123): while the ease still has meaningful yaw
            // to cover, the anim layer sees a signed turning step; settled drops it below.
            let remaining = crate::creature_anim::wrap_pi(goal - new);
            if remaining.abs() > FACING_SETTLED {
                commands.entity(e).insert(FacingStep(remaining));
                stepping.insert(e);
            }
        }
    }
    // Drop stale latches: units that settled, lost their target, or started moving this frame.
    for e in &latched {
        if !stepping.contains(&e) {
            commands.entity(e).remove::<FacingStep>();
        }
    }
}
