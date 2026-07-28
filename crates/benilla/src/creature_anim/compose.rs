//! The collapsed rig's two composition passes (decision 0724) — what transform propagation and
//! the billboard joint pass used to do through ~59 k bone entities, done on the [`RigPose`]
//! arrays instead.
//!
//! **Model pass** ([`compose_rig_models`], pre-propagation): a pose-dirty rig forward-folds its
//! `locals` into model-space affines and re-seats its consumer **anchors** (children of the rig's
//! `joints_root`) from them — so ordinary propagation carries every consumer subtree (held items,
//! nested effect rigs, the mount seat) at this frame's pose, exactly as the joint hierarchy did.
//! Runs after [`PosePost`] (body twist, global sequences — the writers that follow the evaluator).
//!
//! **World pass** ([`finalize_rig_worlds`], post-propagation, inside
//! [`crate::billboard::BillboardPlace`]): per rig needing it — pose-dirty, `joints_root` moved,
//! or camera-faced — compose the world chain `world_from_model × local…`, apply the byte-law
//! bone replacements (`billboard_joint_palette`'s math verbatim: billboard kinds take the camera
//! basis, bone flag `0x04` resets to the holder's rotation, descendants chain onto the replaced
//! frames), write the palette rows (`world × inverse_bindpose`, decision 0720), and re-seat the
//! anchors sitting on replaced subtrees — including the same rigid-child re-walk and the same
//! nested-rig do-not-enter rule as the entity pass. A re-seated subtree that carries another
//! rig's model frame (the mounted rider's seat anchor, [`RigFrame`]) cascades: that rig
//! re-finalizes in the same pass, so its palette never lags its seat.
//!
//! A parked, stationary rig runs neither pass and uploads zero bytes (decision 0448's park
//! observables carry over unchanged).

use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::prelude::*;

use crate::billboard::{billboard_basis, BillboardJointRig};
use crate::player::WorldCamera;
use crate::rig_palette::{RigPalettes, RigSkin};

use super::{AnimParked, RigFrame, RigPose};

/// The pose post-pass window: every writer of [`RigPose`] locals that runs after the evaluator
/// (the body twist, the global-sequence channels) is a member; the model compose runs after the
/// whole set. Configured after [`bevy::app::AnimationSystems`], before transform propagation.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PosePost;

/// Pre-propagation: fold each pose-dirty rig's locals into model-space affines and re-seat its
/// anchors' local `Transform`s, so this frame's propagation places every consumer subtree at
/// this frame's pose. `pose_dirty` stays raised — the world pass consumes and clears it.
fn compose_rig_models(mut rigs: Query<&mut RigPose>, mut anchors: Query<&mut Transform>) {
    for rig in &mut rigs {
        if !rig.pose_dirty {
            continue;
        }
        let rig = rig.into_inner();
        rig.compose();
        for &(bone, anchor) in &rig.anchors {
            let Some(m) = rig.model.get(bone as usize) else {
                continue;
            };
            let Ok(mut t) = anchors.get_mut(anchor) else {
                continue;
            };
            let (scale, rotation, translation) = m.to_scale_rotation_translation();
            *t = Transform {
                translation,
                rotation,
                scale,
            };
        }
    }
}

/// One rig's world chain + which bones sit in a replaced subtree. `root_g` is the rig's
/// `joints_root` propagated world, `root_rot` the HOLDER's world rotation (the frame a bone-flag
/// `0x04` joint snaps to — the entity pass read `BillboardJointRig::root`, which spawn sites fed
/// the holder), `cam` the camera basis (`None` = no camera, replacements limited to `0x04`).
/// The math mirrors `billboard_joint_palette` operation for operation — `mul_transform`, then
/// decompose/replace/recompose — so the collapsed lane is bit-compatible with the entity lane.
fn rig_worlds(
    rig: &RigPose,
    root_g: GlobalTransform,
    root_rot: Quat,
    cam: Option<(Vec3, Vec3, Vec3)>,
) -> (Vec<GlobalTransform>, Vec<bool>) {
    let n = rig.locals.len();
    let mut worlds = Vec::with_capacity(n);
    let mut touched = vec![false; n];
    for i in 0..n {
        let parent = usize::try_from(rig.parents[i]).ok().filter(|&p| p < i);
        let mut g = match parent {
            Some(p) => worlds[p],
            None => root_g,
        }
        .mul_transform(rig.locals[i]);
        if rig.ignore_rot[i] {
            let (scale, _, translation) = g.to_scale_rotation_translation();
            g = GlobalTransform::from(Transform {
                translation,
                rotation: root_rot,
                scale,
            });
            touched[i] = true;
        } else if let (Some(kind), Some((fwd, right, up))) = (rig.kinds[i], cam) {
            let (scale, rot, translation) = g.to_scale_rotation_translation();
            g = GlobalTransform::from(Transform {
                translation,
                rotation: billboard_basis(kind, rot, fwd, right, up),
                scale,
            });
            touched[i] = true;
        } else if let Some(p) = parent {
            touched[i] = touched[p];
        }
        worlds.push(g);
    }
    (worlds, touched)
}

/// Post-propagation (inside `BillboardPlace`, chained after the entity lane's
/// `billboard_joint_palette` and before `face_billboards`): finalize every rig that needs it —
/// palette rows + replaced-subtree anchor re-seats, with the seat-frame cascade.
#[allow(clippy::type_complexity, clippy::too_many_arguments)] // billboard_joint_palette's shape
pub(crate) fn finalize_rig_worlds(
    cam: Query<&GlobalTransform, With<WorldCamera>>,
    mut rigs: Query<(Entity, &mut RigPose, &RigSkin, Has<AnimParked>)>,
    // B0001: the `Changed` filter reads `GlobalTransform` ticks, which conflicts with the
    // mutable frame query — a `ParamSet` sequences them (the refresh set is collected first).
    mut worlds_params: ParamSet<(
        Query<(), Changed<GlobalTransform>>,
        Query<(&Transform, &mut GlobalTransform), Without<WorldCamera>>,
    )>,
    children: Query<&Children>,
    hosts: Query<&BillboardJointRig>,
    frames: Query<&RigFrame>,
    ibps: Res<Assets<SkinnedMeshInverseBindposes>>,
    mut palettes: ResMut<RigPalettes>,
) {
    let cam_basis = cam
        .single()
        .ok()
        .map(|t| (*t.forward(), *t.right(), *t.up()));
    // Which rigs refresh this frame: pose-dirty, model frame moved, or camera-faced live.
    let refresh: Vec<Entity> = {
        let roots_changed = worlds_params.p0();
        rigs.iter()
            .filter(|(_, rig, _, parked)| {
                rig.pose_dirty
                    || roots_changed.contains(rig.joints_root)
                    || (rig.has_billboard && !parked)
            })
            .map(|(holder, ..)| holder)
            .collect()
    };
    if refresh.is_empty() {
        return;
    }
    let mut globals = worlds_params.p1();
    // The rigid-child re-walk's do-not-enter set, exactly the entity pass's: a nested rig with
    // its own billboard output owns its interior (and its root keeps the propagated frame).
    let fx_roots: bevy::platform::collections::HashSet<Entity> =
        hosts.iter().map(|r| r.root()).collect();
    // Model frames re-seated by a patch walk → the rigs riding them re-finalize below.
    let mut cascade: Vec<Entity> = Vec::new();
    let mut finalize =
        |rig: &mut RigPose,
         holder: Entity,
         skin: &RigSkin,
         globals: &mut Query<(&Transform, &mut GlobalTransform), Without<WorldCamera>>,
         cascade: &mut Vec<Entity>| {
            let Ok(root_g) = globals.get(rig.joints_root).map(|(_, g)| *g) else {
                return;
            };
            let root_rot = globals
                .get(holder)
                .map(|(_, g)| g.rotation())
                .unwrap_or_default();
            let (worlds, touched) = rig_worlds(rig, root_g, root_rot, cam_basis);
            if let Some(ibp) = ibps.get(&skin.ibp) {
                palettes.write_rig_worlds(skin, &worlds, ibp);
            }
            if !rig.has_special {
                return;
            }
            // Anchors in replaced subtrees: re-seat their globals and re-compose their rigid
            // children from the replaced frames — the entity pass's child walk, anchor-rooted.
            let mut stack: Vec<(Entity, GlobalTransform)> = Vec::new();
            for &(bone, anchor) in &rig.anchors {
                let b = bone as usize;
                if !touched.get(b).copied().unwrap_or(false) {
                    continue;
                }
                if let Ok((_, mut g)) = globals.get_mut(anchor) {
                    *g = worlds[b];
                }
                if let Ok(cs) = children.get(anchor) {
                    stack.extend(
                        cs.iter()
                            .filter(|c| !fx_roots.contains(c))
                            .map(|c| (c, worlds[b])),
                    );
                }
            }
            while let Some((e, parent_g)) = stack.pop() {
                if let Ok(rf) = frames.get(e) {
                    cascade.push(rf.0);
                }
                let Ok((local, mut global)) = globals.get_mut(e) else {
                    continue;
                };
                let g = parent_g.mul_transform(*local);
                *global = g;
                if let Ok(cs) = children.get(e) {
                    stack.extend(cs.iter().filter(|c| !fx_roots.contains(c)).map(|c| (c, g)));
                }
            }
        };
    for &holder in &refresh {
        let Ok((_, rig, skin, _)) = rigs.get_mut(holder) else {
            continue;
        };
        let rig = rig.into_inner();
        finalize(rig, holder, skin, &mut globals, &mut cascade);
        rig.pose_dirty = false;
    }
    // The cascade: a patch walk above moved some rig's model frame after it (or before it) ran —
    // re-finalize against the fresh seat. One level deep by construction (a seat anchor's
    // subtree holds no further seat anchors).
    if !cascade.is_empty() {
        for (holder, rig, skin, _) in &mut rigs {
            if !cascade.contains(&holder) {
                continue;
            }
            let rig = rig.into_inner();
            let mut ignore = Vec::new();
            finalize(rig, holder, skin, &mut globals, &mut ignore);
            rig.pose_dirty = false;
        }
    }
}

/// Register the model pass; the world pass is chained by [`crate::billboard::BillboardPlugin`]
/// between the entity lane's joint pass and the card facing, where its readers sit.
pub(super) fn plugin(app: &mut App) {
    app.configure_sets(
        PostUpdate,
        PosePost
            .after(bevy::app::AnimationSystems)
            .before(bevy::transform::TransformSystems::Propagate),
    )
    .add_systems(
        PostUpdate,
        compose_rig_models
            .after(PosePost)
            .before(bevy::transform::TransformSystems::Propagate),
    );
}

#[cfg(test)]
mod tests {
    use benilla_assets::{ModelJoint, ModelSkeleton};
    use benilla_formats::BillboardKind;

    use super::*;

    fn skeleton(joints: Vec<ModelJoint>) -> ModelSkeleton {
        ModelSkeleton {
            joints,
            spine_bone: None,
            head_bone: None,
        }
    }

    fn joint(parent: i16, t: Vec3) -> ModelJoint {
        ModelJoint {
            parent,
            local_translation: t,
            billboard: None,
            ignore_parent_rotation: false,
        }
    }

    /// The model compose is the exact affine product entity propagation computed: a three-bone
    /// chain with rotation + non-uniform scale folds to the same matrices as chained
    /// `GlobalTransform::mul_transform`.
    #[test]
    fn compose_matches_entity_propagation() {
        let sk = skeleton(vec![
            joint(-1, Vec3::new(1.0, 2.0, 3.0)),
            joint(0, Vec3::Y),
            joint(1, Vec3::X),
        ]);
        let mut rig = RigPose::new(Entity::PLACEHOLDER, &sk);
        rig.locals[0].rotation = Quat::from_rotation_y(0.7);
        rig.locals[1].scale = Vec3::new(2.0, 1.0, 0.5);
        rig.locals[1].rotation = Quat::from_rotation_x(-0.3);
        rig.compose();
        // The oracle: GlobalTransform chaining from an identity root, exactly what propagation
        // did through the joint entities. Compared as matrices — decomposing two identical
        // affines can NaN an `angle_between` on a dot marginally past 1.
        let mut g = GlobalTransform::IDENTITY;
        for i in 0..3 {
            g = g.mul_transform(rig.locals[i]);
            let oracle = Mat4::from(g.affine());
            let ours = Mat4::from(rig.model[i]);
            assert!(
                oracle.abs_diff_eq(ours, 1e-5),
                "bone {i}: {oracle} vs {ours}"
            );
        }
    }

    /// The world pass reproduces `billboard_joint_palette`'s replacements: a lock-Z billboard
    /// bone takes the camera basis (pivot/scale kept), its child chains onto the replaced frame,
    /// and a bone-flag-0x04 helper resets to the holder's rotation while its pivot rides the
    /// animated parent — the entity pass's three verified behaviours, computed from the arrays.
    #[test]
    fn world_pass_matches_the_entity_billboard_law() {
        let sk = skeleton(vec![
            ModelJoint {
                parent: -1,
                local_translation: Vec3::new(5.0, 1.0, 0.0),
                billboard: Some(BillboardKind::LockZ),
                ignore_parent_rotation: false,
            },
            joint(0, Vec3::Y),
            ModelJoint {
                parent: 1,
                local_translation: Vec3::Y,
                billboard: None,
                ignore_parent_rotation: true,
            },
        ]);
        let mut rig = RigPose::new(Entity::PLACEHOLDER, &sk);
        rig.locals[0].scale = Vec3::splat(2.0);
        rig.locals[1].scale = Vec3::splat(0.5);
        rig.locals[1].rotation = Quat::from_rotation_x(std::f32::consts::FRAC_PI_2);
        rig.compose();
        let root_rot = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        // The identity camera frame: looking down −Z, up Y, right X.
        let cam = (-Vec3::Z, Vec3::X, Vec3::Y);
        let (worlds, touched) = rig_worlds(
            &rig,
            GlobalTransform::from(Transform::from_rotation(root_rot)),
            root_rot,
            Some(cam),
        );
        assert_eq!(touched, vec![true; 3], "the whole chain is replaced");
        // Bone 0: pivot at the root-rotated authored spot, scale kept, lock-Z basis — upright
        // kept axis, local −Z toward the viewer (the billboard.rs test's assertions).
        let (s0, r0, t0) = worlds[0].to_scale_rotation_translation();
        assert!((t0 - root_rot * Vec3::new(5.0, 1.0, 0.0)).length() < 1e-4);
        assert!((s0 - Vec3::splat(2.0)).length() < 1e-5, "scale preserved");
        assert!((r0 * Vec3::Y).dot(Vec3::Y) > 0.999, "kept axis upright");
        assert!((r0 * -Vec3::Z).dot(Vec3::Z) > 0.999, "faces the camera");
        // Bone 1 chains onto the REPLACED parent: one parent-scaled unit up the new Y.
        let (s1, _, t1) = worlds[1].to_scale_rotation_translation();
        assert!((t1 - (t0 + r0 * (Vec3::Y * 2.0))).length() < 1e-4, "{t1}");
        assert!((s1 - Vec3::ONE).length() < 1e-5, "2 × 0.5 scale chain");
        // Bone 2 (flag 0x04): pivot carried by the animated parent's frame, rotation snapped to
        // the holder's.
        let (_, r2, t2) = worlds[2].to_scale_rotation_translation();
        let expect_t = worlds[1].transform_point(Vec3::Y);
        assert!((t2 - expect_t).length() < 1e-4, "pivot rides the parent");
        assert!(
            r2.angle_between(root_rot) < 1e-3,
            "rotation resets to the holder"
        );
    }

    /// An unreplaced chain has no touched bones and composes root × model verbatim — the common
    /// rig costs the palette rows and nothing else.
    #[test]
    fn plain_rigs_touch_nothing() {
        let sk = skeleton(vec![joint(-1, Vec3::X), joint(0, Vec3::Y)]);
        let mut rig = RigPose::new(Entity::PLACEHOLDER, &sk);
        rig.locals[0].rotation = Quat::from_rotation_z(0.4);
        rig.compose();
        let root = GlobalTransform::from_translation(Vec3::new(0.0, 0.0, 7.0));
        let (worlds, touched) = rig_worlds(&rig, root, Quat::IDENTITY, None);
        assert_eq!(touched, vec![false; 2]);
        for (i, w) in worlds.iter().enumerate() {
            let expect = GlobalTransform::from(root.affine() * rig.model[i]);
            assert!(
                (w.translation() - expect.translation()).length() < 1e-5,
                "bone {i}"
            );
        }
    }
}
