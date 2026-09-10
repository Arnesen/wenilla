//! Wenilla's reusable composition workspace; keep the math in sync with compose::rig_worlds.
use super::{billboard_basis, parent_arm_matrix, RigPose};
use bevy::prelude::*;

#[derive(Default)]
pub struct RigWorldScratch {
    worlds: Vec<GlobalTransform>,
    touched: Vec<bool>,
}

impl RigWorldScratch {
    pub(super) fn compose(
        &mut self,
        rig: &RigPose,
        root_g: GlobalTransform,
        cam: Option<(Vec3, Vec3, Vec3)>,
    ) -> (&[GlobalTransform], &[bool]) {
        let n = rig.locals.len();
        self.worlds.clear();
        self.touched.resize(n, false);
        self.touched.fill(false);
        let worlds = &mut self.worlds;
        let touched = &mut self.touched;
        for i in 0..n {
            let parent = usize::try_from(rig.parents[i]).ok().filter(|&p| p < i);
            let parent_world = match parent {
                Some(p) => worlds[p],
                None => root_g,
            };
            // `flags & 0x7` first: it changes the INPUT the billboard law is applied to, and the
            // billboard switch below still runs (wow-re `billboard-bone-law.md` §9.1).
            let mut g = match rig.arms[i] {
                Some(arm) => {
                    touched[i] = true;
                    GlobalTransform::from(parent_arm_matrix(
                        arm,
                        parent_world.affine(),
                        root_g.affine(),
                        rig.binds[i],
                    ))
                }
                None => parent_world,
            }
            .mul_transform(rig.locals[i]);
            if let (Some(kind), Some((fwd, right, up))) = (rig.kinds[i], cam) {
                let (scale, rot, translation) = g.to_scale_rotation_translation();
                g = GlobalTransform::from(Transform {
                    translation,
                    rotation: billboard_basis(kind, rot, fwd, right, up),
                    scale,
                });
                touched[i] = true;
            } else if !touched[i] {
                touched[i] = parent.is_some_and(|p| touched[p]);
            }
            worlds.push(g);
        }
        (worlds, touched)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_assets::{ModelJoint, ModelSkeleton};
    use benilla_formats::{BillboardKind, ParentArm, ParentBasis};

    #[test]
    fn reused_workspace_matches_upstream_across_rigs_and_camera_changes() {
        let mut scratch = RigWorldScratch::default();
        for count in [8, 2, 0, 12, 3] {
            let skeleton = ModelSkeleton {
                joints: (0..count)
                    .map(|i| ModelJoint {
                        parent: i as i16 - 1,
                        local_translation: Vec3::new(i as f32 * 0.1, 0.5, 0.25),
                        billboard: (i % 3 == 0).then_some(BillboardKind::LockZ),
                        parent_arm: (i % 3 == 2).then_some(ParentArm {
                            ignore_translate: false,
                            basis: ParentBasis::RootBasis,
                        }),
                    })
                    .collect(),
                spine_bone: None,
                head_bone: None,
            };
            let mut rig = RigPose::new(Entity::PLACEHOLDER, &skeleton);
            for step in 0..5 {
                for local in &mut rig.locals {
                    local.rotation = Quat::from_rotation_y(step as f32 * 0.13);
                    local.scale = Vec3::new(1.1, 0.9, 1.0);
                }
                let cam = (step % 2 == 0).then_some((-Vec3::Z, Vec3::X, Vec3::Y));
                let root = GlobalTransform::from_rotation(Quat::from_rotation_y(0.9));
                let expected = super::super::rig_worlds(&rig, root, cam);
                let actual = scratch.compose(&rig, root, cam);
                assert_eq!(actual.1, expected.1);
                assert_eq!(actual.0.len(), expected.0.len());
                for (a, b) in actual.0.iter().zip(&expected.0) {
                    assert_eq!(a.affine(), b.affine());
                }
            }
        }
    }

    #[test]
    fn warm_workspace_keeps_allocations_for_smaller_rigs() {
        let mut scratch = RigWorldScratch::default();
        let mut pointers = None;
        for count in [64, 8, 32, 64] {
            let skeleton = ModelSkeleton {
                joints: (0..count)
                    .map(|i| ModelJoint {
                        parent: i as i16 - 1,
                        local_translation: Vec3::Y,
                        billboard: None,
                        parent_arm: None,
                    })
                    .collect(),
                spine_bone: None,
                head_bone: None,
            };
            let rig = RigPose::new(Entity::PLACEHOLDER, &skeleton);
            scratch.compose(&rig, GlobalTransform::IDENTITY, None);
            let current = (scratch.worlds.as_ptr(), scratch.touched.as_ptr());
            if let Some(previous) = pointers {
                assert_eq!(previous, current);
            }
            pointers = Some(current);
        }
    }
}
