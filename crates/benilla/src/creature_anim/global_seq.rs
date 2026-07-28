//! Global-sequence bone channels — free-clock loops *independent* of the playing animation (the
//! character eye-blink eyelid scale; resting fidget pulses). benilla's per-sequence reader deliberately
//! drops them: they key off their own global-sequence timer, not the playing sequence's time band
//! (`benilla_formats::parse_m2_animations`). This samples them on a per-instance clock and writes the
//! driven joint components *after* the [`AnimationPlayer`] posed the skeleton.
//!
//! Ground truth (wow-5875-re `animation.md` / `doodad-anim-host.md`): global sequences loop with **zero
//! arming**, clock-driven off the model's own time field, wrapped modulo each sequence's duration —
//! `globalSequences[gseq]`. There is no per-frame re-arm; the default-animation op4 and the global
//! sequences coexist, the latter purely clock-driven. The canonical consumer is the eyelid: its scale is
//! `0` (lid retracted, eye open) for ~96% of the loop and `1` (lid full, eye shut) for ~100 ms — the
//! blink. Without this pass the eyelid sits at its default identity scale (full size) forever: eyes shut.

use bevy::prelude::*;

use benilla_assets::GlobalBone;

/// One channel's write target: a live joint entity (the doodad/effect/booth lane) or a bone index
/// into the host's [`super::RigPose`] locals (the collapsed unit lane, decision 0724).
enum SeqTarget {
    Joint(Entity),
    Bone(u16),
}

/// Per-instance driver for a model's global-sequence bone channels: each channel's write target
/// paired with its baked channels, plus the free-running clock they wrap on. Attached beside the
/// [`AnimationPlayer`] on a skinned instance whose model carries any global-sequence track.
#[derive(Component)]
pub(crate) struct GlobalSeqDrive {
    /// `(write target, its baked global-sequence channels)`.
    bones: Vec<(SeqTarget, GlobalBone)>,
    /// Seconds since spawn — the model clock each channel wraps (`t mod period`).
    elapsed: f32,
    /// Paused: skip sampling entirely (the doodad host gates animation to drawn instances — wow-re
    /// `doodad-anim-host.md`: the ref's kernel ticks at draw time, so a culled model isn't evaluated).
    /// Creatures never pause. A resume re-syncs the clock via [`Self::sync`], so pausing is drift-free.
    paused: bool,
}

impl GlobalSeqDrive {
    /// Map each of the model's global-sequence bones to this instance's joint entity. `None` when the
    /// model has no global-sequence tracks (the common case) or none resolve to a joint — the entity
    /// then gets no component and the driver skips it.
    pub(crate) fn new(global_bones: &[GlobalBone], joints: &[Entity]) -> Option<Self> {
        let bones: Vec<_> = global_bones
            .iter()
            .filter_map(|g| {
                joints
                    .get(g.bone as usize)
                    .map(|&e| (SeqTarget::Joint(e), g.clone()))
            })
            .collect();
        (!bones.is_empty()).then_some(Self {
            bones,
            elapsed: 0.0,
            paused: false,
        })
    }

    /// The collapsed-rig lane (decision 0724): channels write the host's [`super::RigPose`]
    /// locals by bone index — no joint entities exist. Same `None` gate as [`Self::new`].
    pub(crate) fn new_rig(global_bones: &[GlobalBone], nbones: usize) -> Option<Self> {
        let bones: Vec<_> = global_bones
            .iter()
            .filter(|g| (g.bone as usize) < nbones)
            .map(|g| (SeqTarget::Bone(g.bone), g.clone()))
            .collect();
        (!bones.is_empty()).then_some(Self {
            bones,
            elapsed: 0.0,
            paused: false,
        })
    }

    /// Pause/resume sampling (the doodad draw gate). While paused the joints hold their last pose.
    pub(crate) fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Re-seat the clock at `elapsed` seconds since spawn — the resume-from-pause seek that keeps the
    /// channel clock-indexed like the ref's (`cursor = clock − startOffset`): a re-appearing doodad
    /// shows the pose the shared clock dictates, and same-frame spawns stay in phase across pauses.
    pub(crate) fn sync(&mut self, elapsed: f32) {
        self.elapsed = elapsed;
    }
}

/// Sample each instance's global-sequence channels on its own clock and write the driven bone —
/// in the pose post-pass window ([`super::PosePost`], the same as the body twist), so the model
/// compose folds it. A channel overwrites only its own component; a bone the playing animation
/// never keyed (the eyelid) keeps its rest translation/rotation and takes only the global scale,
/// so the eye opens and blinks over whatever gait is playing.
fn apply_global_sequences(
    time: Res<Time>,
    mut drives: Query<(Entity, &mut GlobalSeqDrive, Has<super::AnimParked>)>,
    mut joints: Query<&mut Transform>,
    mut rigs: Query<&mut super::RigPose>,
) {
    let dt = time.delta_secs();
    for (host, mut drive, parked) in &mut drives {
        if drive.paused {
            continue;
        }
        // A parked unit's channel clock keeps running — only the bone writes stop (decision
        // 0448: units are absolute-clock, never freeze-and-resume; the doodad lane pauses via
        // [`GlobalSeqDrive::set_paused`] + [`GlobalSeqDrive::sync`] instead and never parks).
        drive.elapsed += dt;
        if parked {
            continue;
        }
        let t = drive.elapsed;
        let mut rig = rigs.get_mut(host).ok();
        for (target, bone) in &drive.bones {
            let tf: &mut Transform = match target {
                SeqTarget::Joint(joint) => {
                    let Ok(tf) = joints.get_mut(*joint) else {
                        continue;
                    };
                    tf.into_inner()
                }
                SeqTarget::Bone(b) => {
                    let Some(rig) = rig.as_mut() else { continue };
                    rig.pose_dirty = true;
                    let Some(tf) = rig.locals.get_mut(*b as usize) else {
                        continue;
                    };
                    tf
                }
            };
            if let Some(c) = &bone.translation {
                tf.translation = c.sample(t);
            }
            if let Some(c) = &bone.rotation {
                tf.rotation = c.sample(t);
            }
            if let Some(c) = &bone.scale {
                tf.scale = c.sample(t);
            }
        }
    }
}

/// Register [`apply_global_sequences`] in the pose post-pass window (beside the body twist).
pub(super) fn plugin(app: &mut App) {
    app.add_systems(PostUpdate, apply_global_sequences.in_set(super::PosePost));
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_assets::GlobalSeqChannel;

    fn eyelid_bone() -> GlobalBone {
        GlobalBone {
            bone: 75,
            translation: None,
            rotation: None,
            // The real eyelid shape: open (0) at the loop start, shut (1) for the blink window, open again.
            scale: Some(GlobalSeqChannel {
                period: 6.633,
                keys: vec![
                    (0.0, Vec3::ZERO),
                    (0.033, Vec3::ONE),
                    (0.100, Vec3::ONE),
                    (0.133, Vec3::ZERO),
                ],
            }),
        }
    }

    /// The driver writes the sampled global-sequence scale onto the mapped joint each tick — the eyelid
    /// reads `1` (shut) when the clock sits in the blink window and `0` (open) the rest of the loop, so a
    /// skinned character actually opens and blinks instead of freezing lid-down. Time advances by real
    /// dt; both clocks sit on a constant plateau of the channel, so the assert is jitter-proof.
    #[test]
    fn driver_writes_eyelid_scale_from_the_clock() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins); // Time
        app.add_systems(Update, apply_global_sequences);

        // Two instances: one clock parked mid-blink (shut), one parked in the open tail.
        let shut_joint = app.world_mut().spawn(Transform::default()).id();
        app.world_mut().spawn(GlobalSeqDrive {
            bones: vec![(SeqTarget::Joint(shut_joint), eyelid_bone())],
            elapsed: 0.06, // inside [0.033, 0.100] → scale 1
            paused: false,
        });
        let open_joint = app.world_mut().spawn(Transform::default()).id();
        app.world_mut().spawn(GlobalSeqDrive {
            bones: vec![(SeqTarget::Joint(open_joint), eyelid_bone())],
            elapsed: 3.0, // inside [0.133, 6.633] → scale 0
            paused: false,
        });

        app.update();

        let scale = |e: Entity| app.world().entity(e).get::<Transform>().unwrap().scale;
        assert!(
            scale(shut_joint).abs_diff_eq(Vec3::ONE, 1e-3),
            "eyelid shut mid-blink"
        );
        assert!(
            scale(open_joint).abs_diff_eq(Vec3::ZERO, 1e-3),
            "eyelid open the rest of the loop"
        );
    }
}
