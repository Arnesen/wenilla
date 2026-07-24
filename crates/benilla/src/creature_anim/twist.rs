//! The **display-facing counter-twist** — the client's strafe/look body pose (wow-5875-re
//! `body-facing-pipeline.md` §3, the `0x607ed0` tail → the `0x711f10` bone channels).
//!
//! A unit's rendered root yaw can sit *offset* from its aim — a strafe turns the root toward the
//! slide (±90° pure, ±45° diagonal) while the aim (camera, server orientation) holds. The client
//! then counter-rotates two key-bone subtrees back toward the aim, proportional to the remaining
//! gap: **SpineLow (KeyBoneID 4)** takes half the gap capped at 45°, **Head (KeyBoneID 6)** takes
//! the remainder capped at 45°. For a pure 90° strafe that composes to: hips/legs fully at the
//! strafe heading, shoulders counter-twisted back ~45°, and the head landing *exactly* on the aim
//! (45° + 45° = 90°) — nothing tuned, the arithmetic closes. The gap owner ([`crate::player`]'s
//! controller for our avatar, [`crate::net::motion`] for remote movers) writes [`BodyTwist::yaw_gap`];
//! the [`apply_body_twist`] system composes the twist onto the animated bone locals each frame,
//! after Bevy's animation evaluation and before transform propagation.

use bevy::app::AnimationSystems;
use bevy::prelude::*;
use bevy::transform::TransformSystems;

/// The body's counter-twist state, on a skinned unit's root entity. Inserted at visual attach when
/// the model carries either twist key-bone ([`benilla_assets::ModelSkeleton::spine_bone`]/
/// [`head_bone`](benilla_assets::ModelSkeleton::head_bone)); absent on beasts/props (the client's
/// capability gates `[+0xd58] & 0x80/0x100` — a model without the key-bone plays no channel).
#[derive(Component)]
pub(crate) struct BodyTwist {
    /// `wrap(aim − rendered root yaw)`, radians — how far the aim sits from the heading the model
    /// renders at. Zero whenever the body faces its aim (everything but a strafe, today).
    pub(crate) yaw_gap: f32,
    spine: Option<Channel>,
    head: Option<Channel>,
}

impl BodyTwist {
    pub(crate) fn new(spine: Option<Entity>, head: Option<Entity>) -> Self {
        Self {
            yaw_gap: 0.0,
            spine: spine.map(Channel::new),
            head: head.map(Channel::new),
        }
    }
}

/// One twist channel's joint + composition bookkeeping.
struct Channel {
    joint: Entity,
    /// The animated local rotation the twist last composed on — the "base" under our twist.
    base: Quat,
    /// What we last wrote (`base * twist`). If the joint still holds exactly this next frame, the
    /// animation didn't retouch the bone this frame (a clip need not key every bone), so `base`
    /// stays authoritative — composing onto the joint's current value instead would accumulate the
    /// twist frame over frame and spin the bone.
    last_out: Quat,
}

impl Channel {
    fn new(joint: Entity) -> Self {
        Self {
            joint,
            base: Quat::IDENTITY,
            last_out: Quat::IDENTITY,
        }
    }
}

/// Wrap an angle to `(−π, π]` — the shortest-arc form every yaw-gap computation here uses.
pub(crate) fn wrap_pi(angle: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    PI - (PI - angle).rem_euclid(TAU)
}

/// Split a yaw gap into the (spine, head) counter-twist angles — the client's share math
/// (`0x607ed0` tail, byte-VERIFIED): spine = half the gap capped at ±45°, head = the remainder
/// capped at ±45° — so a pure 90° strafe composes to 45°+45° and the head lands exactly on the aim.
///
/// The binary carries a full-share branch too (`0x6103a0`: local player AND a live click-to-move
/// action), but `[0xc4d888]` is the click-to-move action type and `0xc` = disabled is its normal
/// in-world value (VERIFIED, wow-re `b947e5aa`) — so half is the effective share for everyone in
/// ordinary play, exactly as the director's reference eye called it when the full-share variant
/// was tried and rejected (decision 0104). Full share would fire only during click-to-move, which
/// benilla doesn't have.
fn twist_shares(gap: f32) -> (f32, f32) {
    use std::f32::consts::FRAC_PI_4;
    let spine = (gap * 0.5).clamp(-FRAC_PI_4, FRAC_PI_4);
    let head = (gap - spine).clamp(-FRAC_PI_4, FRAC_PI_4);
    (spine, head)
}

/// Compose the counter-twist onto the animated bone locals — PostUpdate, after
/// [`AnimationSystems`] wrote this frame's pose and before [`TransformSystems::Propagate`].
///
/// Each channel yaws its subtree about **world up through the bone's own pivot**: with `g` the
/// bone's model-space rotation (ancestors × its animated local), `local' = local · Quat(g⁻¹·Y, θ)`
/// conjugates to a pure up-axis yaw of the subtree (units stand upright and their root rotation is
/// a Y-yaw, so model up and world up coincide). The head channel runs after the spine write, so its
/// ancestor chain already carries the spine's twist — the head counter-rotates relative to the
/// twisted spine, exactly the client's residual-gap composition.
pub(super) fn apply_body_twist(
    // A parked rig's bones are frozen (decision 0448) — composing the twist onto them would
    // re-dirty the subtree every frame for a unit no one sees; the wake re-seats `base` from the
    // fresh sample on its own (`cur != last_out`).
    mut units: Query<(Entity, &mut BodyTwist), Without<super::AnimParked>>,
    mut joints: Query<&mut Transform>,
    parents: Query<&ChildOf>,
) {
    for (unit, mut twist) in &mut units {
        let (spine, head) = twist_shares(twist.yaw_gap);
        let twist = &mut *twist;
        for (channel, angle) in [(&mut twist.spine, spine), (&mut twist.head, head)] {
            let Some(ch) = channel else { continue };
            let Ok(cur) = joints.get(ch.joint).map(|t| t.rotation) else {
                continue;
            };
            let base = if cur == ch.last_out { ch.base } else { cur };
            let out = if angle == 0.0 {
                base
            } else {
                // The bone's model-space rotation: walk local rotations up to (excluding) the unit
                // root. (Including the root would be harmless — its Y-yaw preserves the up axis —
                // but stopping there keeps the walk from wandering into the world hierarchy.)
                let mut g = base;
                let mut e = ch.joint;
                while let Ok(p) = parents.get(e).map(|c| c.parent()) {
                    if p == unit {
                        break;
                    }
                    if let Ok(t) = joints.get(p) {
                        g = t.rotation * g;
                    }
                    e = p;
                }
                (base * Quat::from_axis_angle(g.inverse() * Vec3::Y, angle)).normalize()
            };
            ch.base = base;
            ch.last_out = out;
            if out != cur {
                if let Ok(mut t) = joints.get_mut(ch.joint) {
                    t.rotation = out;
                }
            }
        }
    }
}

/// Register [`apply_body_twist`] in the post-animation window.
pub(super) fn plugin(app: &mut App) {
    app.add_systems(
        PostUpdate,
        apply_body_twist
            .after(AnimationSystems)
            .before(TransformSystems::Propagate),
    );
}

#[cfg(test)]
mod tests {
    use super::twist_shares;
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};

    #[test]
    fn pure_strafe_gap_closes_exactly_at_the_head() {
        // 90° gap: spine 45°, head 45° — the head lands back on the aim.
        let (spine, head) = twist_shares(-FRAC_PI_2);
        assert_eq!(spine, -FRAC_PI_4);
        assert_eq!(head, -FRAC_PI_4);
        assert_eq!(spine + head, -FRAC_PI_2);
    }

    #[test]
    fn diagonal_strafe_splits_evenly() {
        // 45° gap: spine 22.5°, head 22.5° — head on the aim again (the half share, everywhere:
        // the director's reference eye rejected the binary's local-player full-share branch).
        let (spine, head) = twist_shares(FRAC_PI_4);
        assert_eq!(spine, FRAC_PI_4 / 2.0);
        assert_eq!(head, FRAC_PI_4 / 2.0);
    }

    #[test]
    fn shares_cap_at_45_degrees_each() {
        // An extreme gap (π) can't be fully absorbed: both channels cap at 45°.
        let (spine, head) = twist_shares(PI);
        assert_eq!(spine, FRAC_PI_4);
        assert_eq!(head, FRAC_PI_4);
    }

    #[test]
    fn zero_gap_is_zero_twist() {
        assert_eq!(twist_shares(0.0), (0.0, 0.0));
    }

    #[test]
    fn wrap_pi_takes_the_shortest_arc() {
        use super::wrap_pi;
        assert_eq!(wrap_pi(0.0), 0.0);
        assert!((wrap_pi(3.0 * FRAC_PI_2) + FRAC_PI_2).abs() < 1e-6);
        assert!((wrap_pi(-3.0 * FRAC_PI_2) - FRAC_PI_2).abs() < 1e-6);
        assert_eq!(wrap_pi(PI), PI);
    }
}
