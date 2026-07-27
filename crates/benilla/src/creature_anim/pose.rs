//! The direct M2 pose evaluator (decision 0712): turn each rig's `AnimationPlayer` state into
//! bone `Transform`s ourselves, off the baked [`PoseSource`], instead of routing per-bone entities
//! through Bevy's `animate_targets` (0711's second lane — ~1.9 µs *per bone* of graph-walk +
//! hash-lookup + boxed-curve machinery to produce, for most bones, a constant).
//!
//! The player stays the playback state machine — the driver's arms, cross-fades, weights, seeks,
//! and completions are untouched — and this system reproduces `animate_targets`' *output* exactly
//! (the goldens below run both side by side and assert bone-for-bone equality). The law, read from
//! `bevy_animation-0.18.1` and pinned in 0712: per (bone, property), active nodes contribute in
//! **ascending node-index order**, folded by `total += w; acc = interpolate(acc, v, w/total)`
//! (`Vec3::lerp` / `Quat::slerp` — the same `Animatable` calls, so the math is bit-identical); a
//! node contributes iff its weight ≠ 0 and `bone_masks[bone] & node_mask == 0` and its clip keys
//! the channel; sampling clamps into the key span; a property no active clip keys is left alone
//! (there is no rest-pose reset — the joint keeps what it has). Paused animations still evaluate
//! at their frozen seek, exactly as Bevy does.
//!
//! Parking (decision 0448) is the [`AnimParked`] marker: a parked rig is skipped here — clocks,
//! driver, and events all keep running — which preserves 0448's two observables (absolute-clock
//! snap on wake, off-screen combat audibility) with no `AnimatedBy` repoint machinery.

use benilla_assets::{ModelAnimations, PoseSource};
use bevy::animation::animatable::Animatable;
use bevy::app::AnimationSystems;
use bevy::prelude::*;
use bevy::transform::TransformSystems;

use super::AnimParked;

/// The evaluator's rig handle, on the entity carrying the `AnimationPlayer`: bone index → joint
/// entity, in skeleton order (decision 0712). Rigs spawn this **instead of** per-joint
/// `AnimationTargetId`/`AnimatedBy` pairs — with no targets, Bevy's `animate_targets` never
/// touches these bones.
#[derive(Component)]
pub(crate) struct PosedRig {
    pub(crate) joints: Vec<Entity>,
}

/// One active animation's evaluation inputs, resolved against the rig's [`PoseSource`].
struct Active {
    /// `AnimationNodeIndex::index()` — the sort key that reproduces Bevy's blend order.
    node: usize,
    clip: usize,
    mask: u64,
    weight: f32,
    seek: f32,
    /// The merge walk's position in the clip's bone list.
    cursor: usize,
}

/// Evaluate every live rig's pose. Runs where `animate_targets` runs — inside [`AnimationSystems`]
/// after `advance_animations` ticked the seek clocks — so the pose post-passes (body twist,
/// global-sequence writes) and transform propagation see it at the same point in the frame.
fn evaluate_rig_poses(
    rigs: Query<(&AnimationPlayer, &ModelAnimations, &PosedRig), Without<AnimParked>>,
    mut joints: Query<&mut Transform>,
) {
    for (player, anims, rig) in &rigs {
        let src = &anims.pose;
        // This rig's contributing animations, ascending by node index (Bevy's effective blend
        // order — sorted children folded through a LIFO stack, see the module doc).
        let mut active: Vec<Active> = Vec::new();
        for (&node, anim) in player.playing_animations() {
            // Bevy's exact gate: a weight of literal 0.0 skips the node (paused does not).
            if anim.weight() == 0.0 {
                continue;
            }
            let Some(pn) = src.node(node) else { continue };
            if src.clips.get(pn.clip as usize).is_none() {
                continue;
            }
            active.push(Active {
                node: node.index(),
                clip: pn.clip as usize,
                mask: pn.mask,
                weight: anim.weight(),
                seek: anim.seek_time(),
                cursor: 0,
            });
        }
        active.sort_unstable_by_key(|a| a.node);
        match active.as_mut_slice() {
            [] => {}
            [one] => {
                // The steady state: one looping gait. A single contribution commits at full
                // value whatever its weight (Bevy's register initializes unnormalized).
                for pb in &src.clips[one.clip].bones {
                    if src.bone_masks.get(pb.bone as usize).copied().unwrap_or(0) & one.mask != 0 {
                        continue;
                    }
                    let Some(&joint) = rig.joints.get(pb.bone as usize) else {
                        continue;
                    };
                    let Ok(mut tf) = joints.get_mut(joint) else {
                        continue;
                    };
                    if let Some(v) = pb.translation.sample(one.seek) {
                        tf.translation = v;
                    }
                    if let Some(v) = pb.rotation.sample(one.seek) {
                        tf.rotation = v;
                    }
                    if let Some(v) = pb.scale.sample(one.seek) {
                        tf.scale = v;
                    }
                }
            }
            many => blend_rig(src, many, rig, &mut joints),
        }
    }
}

/// The multi-animation path (a cross-fade, a masked overlay, the grip): merge-walk the
/// bone-sorted clips and fold each (bone, property)'s contributions in node order.
fn blend_rig(
    src: &PoseSource,
    active: &mut [Active],
    rig: &PosedRig,
    joints: &mut Query<&mut Transform>,
) {
    // Walk the next keyed bone across all clips until every cursor is spent.
    while let Some(bone) = active
        .iter()
        .filter_map(|a| src.clips[a.clip].bones.get(a.cursor).map(|b| b.bone))
        .min()
    {
        let bone_mask = src.bone_masks.get(bone as usize).copied().unwrap_or(0);
        // Fold each property over the clips keying this bone, in node order (`active` is sorted).
        let mut translation: Option<(Vec3, f32)> = None;
        let mut rotation: Option<(Quat, f32)> = None;
        let mut scale: Option<(Vec3, f32)> = None;
        for a in active.iter_mut() {
            let Some(pb) = src.clips[a.clip]
                .bones
                .get(a.cursor)
                .filter(|b| b.bone == bone)
            else {
                continue;
            };
            a.cursor += 1;
            if bone_mask & a.mask != 0 {
                continue; // masked out for this node — no contribution, cursor still advances
            }
            fold(&mut translation, pb.translation.sample(a.seek), a.weight);
            fold(&mut rotation, pb.rotation.sample(a.seek), a.weight);
            fold(&mut scale, pb.scale.sample(a.seek), a.weight);
        }
        if translation.is_none() && rotation.is_none() && scale.is_none() {
            continue;
        }
        let Some(&joint) = rig.joints.get(bone as usize) else {
            continue;
        };
        let Ok(mut tf) = joints.get_mut(joint) else {
            continue;
        };
        if let Some((v, _)) = translation {
            tf.translation = v;
        }
        if let Some((v, _)) = rotation {
            tf.rotation = v;
        }
        if let Some((v, _)) = scale {
            tf.scale = v;
        }
    }
}

/// One property's running fold — Bevy's blend register verbatim: the first contribution lands at
/// full value (carrying its weight), each later one at `w / total`.
#[inline]
fn fold<T: Animatable>(register: &mut Option<(T, f32)>, sample: Option<T>, weight: f32) {
    let Some(v) = sample else { return };
    *register = Some(match register.take() {
        None => (v, weight),
        Some((acc, total)) => {
            let total = total + weight;
            (T::interpolate(&acc, &v, weight / total), total)
        }
    });
}

/// Register the evaluator in `animate_targets`' window: after the seek clocks tick, before the
/// pose post-passes (which order `.after(AnimationSystems)`) and transform propagation.
pub(super) fn plugin(app: &mut App) {
    app.add_systems(
        PostUpdate,
        evaluate_rig_poses
            .in_set(AnimationSystems)
            .after(bevy::animation::advance_animations)
            .before(TransformSystems::Propagate),
    );
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use benilla_assets::{bone_target_id, PoseBone, PoseClip, PoseTrack};
    use bevy::animation::animation_curves::{AnimatableCurve, AnimatableKeyframeCurve};
    use bevy::animation::graph::{AnimationGraph, AnimationGraphHandle, AnimationNodeIndex};
    use bevy::animation::transition::AnimationTransitions;
    use bevy::animation::{animated_field, AnimatedBy, AnimationClip};

    use super::*;

    /// One test clip's authored channels: per bone, optional key lists for T/R/S.
    #[derive(Clone, Default)]
    struct ClipSpec {
        bones: Vec<(u16, BoneSpec)>,
        /// The `add_clip_with_mask` mask (0 = unmasked).
        mask: u64,
    }
    #[derive(Clone, Default)]
    struct BoneSpec {
        t: Vec<(f32, Vec3)>,
        r: Vec<(f32, Quat)>,
        s: Vec<(f32, Vec3)>,
    }

    /// Build the graph + clips + pose source from specs, exactly as `m2.rs` does: the Bevy clip
    /// through `AnimatableCurve`/`AnimatableKeyframeCurve`, the pose twin through
    /// `PoseTrack`/`PoseClip`, node table beside each graph add.
    fn build(
        app: &mut App,
        specs: &[ClipSpec],
        bone_masks: Vec<u64>,
    ) -> (Handle<AnimationGraph>, Vec<AnimationNodeIndex>, PoseSource) {
        let mut graph = AnimationGraph::new();
        let root = graph.root;
        let mut pose = PoseSource {
            bone_masks,
            ..Default::default()
        };
        // Mirror the graph's mask groups from the baked bone bits (group g ⇔ bit g).
        for (i, &bits) in pose.bone_masks.iter().enumerate() {
            for g in 0..8 {
                if bits & (1 << g) != 0 {
                    graph.add_target_to_mask_group(bone_target_id(i as u16), g);
                }
            }
        }
        let mut nodes = Vec::new();
        for spec in specs {
            let mut clip = AnimationClip::default();
            let mut pclip = PoseClip::default();
            for (bone, bs) in &spec.bones {
                let target = bone_target_id(*bone);
                if bs.t.len() >= 2 {
                    clip.add_curve_to_target(
                        target,
                        AnimatableCurve::new(
                            animated_field!(Transform::translation),
                            AnimatableKeyframeCurve::new(bs.t.iter().copied()).unwrap(),
                        ),
                    );
                }
                if bs.r.len() >= 2 {
                    clip.add_curve_to_target(
                        target,
                        AnimatableCurve::new(
                            animated_field!(Transform::rotation),
                            AnimatableKeyframeCurve::new(bs.r.iter().copied()).unwrap(),
                        ),
                    );
                }
                if bs.s.len() >= 2 {
                    clip.add_curve_to_target(
                        target,
                        AnimatableCurve::new(
                            animated_field!(Transform::scale),
                            AnimatableKeyframeCurve::new(bs.s.iter().copied()).unwrap(),
                        ),
                    );
                }
                pclip.push(PoseBone {
                    bone: *bone,
                    translation: if bs.t.len() >= 2 {
                        PoseTrack::new(&bs.t)
                    } else {
                        PoseTrack::default()
                    },
                    rotation: if bs.r.len() >= 2 {
                        PoseTrack::new(&bs.r)
                    } else {
                        PoseTrack::default()
                    },
                    scale: if bs.s.len() >= 2 {
                        PoseTrack::new(&bs.s)
                    } else {
                        PoseTrack::default()
                    },
                });
            }
            let handle = app
                .world_mut()
                .resource_mut::<Assets<AnimationClip>>()
                .add(clip);
            let clip_idx = pose.clips.len() as u32;
            pose.clips.push(pclip);
            let node = if spec.mask == 0 {
                graph.add_clip(handle, 1.0, root)
            } else {
                graph.add_clip_with_mask(handle, spec.mask, 1.0, root)
            };
            pose.set_node(node, clip_idx, spec.mask);
            nodes.push(node);
        }
        let graph = app
            .world_mut()
            .resource_mut::<Assets<AnimationGraph>>()
            .add(graph);
        (graph, nodes, pose)
    }

    /// A `ModelAnimations` carrying only what the evaluator reads (graph + pose).
    fn model_anims(graph: Handle<AnimationGraph>, pose: PoseSource) -> ModelAnimations {
        ModelAnimations {
            graph,
            clips: Vec::new(),
            playable_animation_lookup: Vec::new(),
            animation_lookup: Vec::new(),
            hand_close: [None, None],
            global_bones: Vec::new(),
            first_seq: None,
            pose: std::sync::Arc::new(pose),
        }
    }

    /// Spawn the two rigs: the Bevy-path oracle (per-joint targets, `animate_targets` evaluates
    /// it) and the evaluator rig (`PosedRig`). Returns `(oracle_root, oracle_joints, ours_root,
    /// our_joints)`.
    fn twin_rigs(
        app: &mut App,
        nbones: u16,
        graph: &Handle<AnimationGraph>,
        pose: &PoseSource,
    ) -> (Entity, Vec<Entity>, Entity, Vec<Entity>) {
        let spawn_root = |app: &mut App| {
            app.world_mut()
                .spawn((
                    Transform::default(),
                    AnimationPlayer::default(),
                    AnimationTransitions::new(),
                    AnimationGraphHandle(graph.clone()),
                ))
                .id()
        };
        let oracle = spawn_root(app);
        let oracle_joints: Vec<Entity> = (0..nbones)
            .map(|i| {
                app.world_mut()
                    .spawn((
                        Transform::default(),
                        ChildOf(oracle),
                        bone_target_id(i),
                        AnimatedBy(oracle),
                    ))
                    .id()
            })
            .collect();
        let ours = spawn_root(app);
        let our_joints: Vec<Entity> = (0..nbones)
            .map(|_| {
                app.world_mut()
                    .spawn((Transform::default(), ChildOf(ours)))
                    .id()
            })
            .collect();
        app.world_mut().entity_mut(ours).insert((
            model_anims(graph.clone(), pose.clone()),
            PosedRig {
                joints: our_joints.clone(),
            },
        ));
        (oracle, oracle_joints, ours, our_joints)
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            bevy::animation::AnimationPlugin,
        ));
        plugin(&mut app);
        app
    }

    /// Run `f` identically on both players, then step and assert every joint's `Transform` is
    /// **exactly** equal between the oracle and the evaluator — same inputs, same `Animatable`
    /// calls, bit-identical outputs.
    #[track_caller]
    fn assert_twins_equal(app: &mut App, oracle_joints: &[Entity], our_joints: &[Entity]) {
        for (i, (&a, &b)) in oracle_joints.iter().zip(our_joints).enumerate() {
            let ta = *app.world().entity(a).get::<Transform>().unwrap();
            let tb = *app.world().entity(b).get::<Transform>().unwrap();
            assert_eq!(ta, tb, "bone {i} diverged (oracle vs evaluator)");
        }
    }

    fn step(app: &mut App, secs: f32) {
        std::thread::sleep(Duration::from_secs_f32(secs));
        app.update();
    }

    /// Run the same player mutation on each rig — identical inputs is the whole point of the
    /// twin-rig golden.
    fn drive(
        app: &mut App,
        rigs: &[Entity],
        f: impl Fn(&mut AnimationPlayer, &mut AnimationTransitions),
    ) {
        for &rig in rigs {
            let mut e = app.world_mut().entity_mut(rig);
            // Take transitions out to appease the borrow checker, run, put back.
            let mut tr = e.take::<AnimationTransitions>().unwrap();
            {
                let player = e.get_mut::<AnimationPlayer>().unwrap();
                f(player.into_inner(), &mut tr);
            }
            e.insert(tr);
        }
    }

    /// The steady state (one looping clip): channels present and absent, exact equality mid-loop —
    /// including a bone the clip never keys (both paths must leave it at its spawn value).
    #[test]
    fn single_clip_matches_animate_targets() {
        let mut app = app();
        let spec = ClipSpec {
            bones: vec![
                (
                    0,
                    BoneSpec {
                        t: vec![(0.0, Vec3::ZERO), (0.4, Vec3::X), (0.8, Vec3::Y)],
                        r: vec![(0.0, Quat::IDENTITY), (0.8, Quat::from_rotation_y(2.0))],
                        s: vec![],
                    },
                ),
                (
                    1,
                    BoneSpec {
                        t: vec![],
                        r: vec![
                            (0.1, Quat::from_rotation_x(0.4)),
                            (0.5, Quat::from_rotation_z(-1.2)),
                        ],
                        s: vec![(0.0, Vec3::ONE), (0.8, Vec3::splat(2.0))],
                    },
                ),
                // bone 2 never keyed — the untouched-property law.
            ],
            mask: 0,
        };
        let (graph, nodes, pose) = build(&mut app, &[spec], vec![0, 0, 0]);
        let (oracle, oj, ours, uj) = twin_rigs(&mut app, 3, &graph, &pose);
        // One warm-up frame: Bevy's ThreadedAnimationGraphs builds off the graph asset's Added
        // event, so `animate_targets` bails on the spawn frame (our evaluator doesn't need it and
        // evaluates immediately — strictly earlier, not different). Compare from frame 2 on.
        app.update();
        drive(&mut app, &[oracle, ours], |p, _| {
            p.play(nodes[0]).repeat();
        });
        for _ in 0..5 {
            step(&mut app, 0.05);
            assert_twins_equal(&mut app, &oj, &uj);
        }
    }

    /// A cross-fade (`AnimationTransitions::play` mid-flight): main at weight 1 plus the fading
    /// node's decaying weight — the normalized fold must match Bevy's frame for frame, and the
    /// past-the-end clamp holds once the one-shot finishes.
    #[test]
    fn crossfade_matches_animate_targets() {
        let mut app = app();
        let walk = ClipSpec {
            bones: vec![(
                0,
                BoneSpec {
                    t: vec![(0.0, Vec3::ZERO), (0.6, Vec3::X * 3.0)],
                    r: vec![(0.0, Quat::IDENTITY), (0.6, Quat::from_rotation_z(1.0))],
                    s: vec![],
                },
            )],
            mask: 0,
        };
        let run = ClipSpec {
            bones: vec![(
                0,
                BoneSpec {
                    t: vec![(0.0, Vec3::Y), (0.3, Vec3::NEG_Y)],
                    r: vec![
                        (0.0, Quat::from_rotation_x(0.5)),
                        (0.3, Quat::from_rotation_x(-0.5)),
                    ],
                    s: vec![],
                },
            )],
            mask: 0,
        };
        let (graph, nodes, pose) = build(&mut app, &[walk, run], vec![0]);
        let (oracle, oj, ours, uj) = twin_rigs(&mut app, 1, &graph, &pose);
        app.update(); // warm-up: see single_clip_matches_animate_targets
        drive(&mut app, &[oracle, ours], |p, tr| {
            tr.play(p, nodes[0], Duration::ZERO).repeat();
        });
        step(&mut app, 0.1);
        // Cross-fade into the run over 0.25 s; sample mid-fade several times.
        drive(&mut app, &[oracle, ours], |p, tr| {
            tr.play(p, nodes[1], Duration::from_secs_f32(0.25)).repeat();
        });
        for _ in 0..6 {
            step(&mut app, 0.06);
            assert_twins_equal(&mut app, &oj, &uj);
        }
    }

    /// The masked upper-body overlay at weight 8 over a full-body base (the driver's one-shot
    /// route), plus a weight-0 node that must contribute nothing: bones in the mask group blend
    /// 8:1, bones outside it take the base alone.
    #[test]
    fn masked_overlay_matches_animate_targets() {
        let mut app = app();
        let base = ClipSpec {
            bones: vec![
                (
                    0, // "legs" — in mask group 2, the overlay must not touch it
                    BoneSpec {
                        t: vec![(0.0, Vec3::ZERO), (0.5, Vec3::X)],
                        r: vec![(0.0, Quat::IDENTITY), (0.5, Quat::from_rotation_y(1.0))],
                        s: vec![],
                    },
                ),
                (
                    1, // "torso" — both drive it, the 8:1 blend
                    BoneSpec {
                        t: vec![(0.0, Vec3::ZERO), (0.5, Vec3::Z)],
                        r: vec![(0.0, Quat::IDENTITY), (0.5, Quat::from_rotation_z(0.7))],
                        s: vec![],
                    },
                ),
            ],
            mask: 0,
        };
        let overlay = ClipSpec {
            bones: vec![
                (
                    0,
                    BoneSpec {
                        t: vec![(0.0, Vec3::splat(9.0)), (0.4, Vec3::splat(9.0))],
                        r: vec![],
                        s: vec![],
                    },
                ),
                (
                    1,
                    BoneSpec {
                        t: vec![(0.0, Vec3::NEG_Z), (0.4, Vec3::NEG_X)],
                        r: vec![
                            (0.0, Quat::from_rotation_x(1.2)),
                            (0.4, Quat::from_rotation_x(0.2)),
                        ],
                        s: vec![],
                    },
                ),
            ],
            mask: 1 << 2,
        };
        let idle = ClipSpec {
            bones: vec![(
                1,
                BoneSpec {
                    t: vec![(0.0, Vec3::splat(50.0)), (0.4, Vec3::splat(50.0))],
                    r: vec![],
                    s: vec![],
                },
            )],
            mask: 0,
        };
        // bone 0 is in mask group 2 (the "outside the upper subtree" group).
        let (graph, nodes, pose) = build(&mut app, &[base, overlay, idle], vec![1 << 2, 0]);
        let (oracle, oj, ours, uj) = twin_rigs(&mut app, 2, &graph, &pose);
        app.update(); // warm-up: see single_clip_matches_animate_targets
        drive(&mut app, &[oracle, ours], |p, _| {
            p.play(nodes[0]).repeat();
            p.play(nodes[1]).repeat().set_weight(8.0);
            p.play(nodes[2]).repeat().set_weight(0.0); // must be skipped entirely
        });
        for _ in 0..4 {
            step(&mut app, 0.07);
            assert_twins_equal(&mut app, &oj, &uj);
        }
        // And the masked bone really is base-only: it moved off spawn (the base wrote it), yet
        // never took the overlay's parked 9.0 constant.
        let t0 = app.world().entity(uj[0]).get::<Transform>().unwrap();
        assert_ne!(
            t0.translation,
            Vec3::splat(9.0),
            "mask kept the overlay off bone 0"
        );
    }

    /// A parked rig is skipped (the 0448 gate's new mechanism): bones freeze while the clock
    /// advances; unparking resumes sampling at the absolute clock.
    #[test]
    fn parked_rig_freezes_bones_only() {
        let mut app = app();
        let spec = ClipSpec {
            bones: vec![(
                0,
                BoneSpec {
                    t: vec![(0.0, Vec3::ZERO), (0.4, Vec3::X)],
                    r: vec![],
                    s: vec![],
                },
            )],
            mask: 0,
        };
        let (graph, nodes, pose) = build(&mut app, &[spec], vec![0]);
        let (oracle, oj, ours, uj) = twin_rigs(&mut app, 1, &graph, &pose);
        // The oracle twin runs unparked throughout — the absolute-clock witness.
        drive(&mut app, &[oracle, ours], |p, _| {
            p.play(nodes[0]).repeat();
        });
        step(&mut app, 0.05);
        app.world_mut().entity_mut(ours).insert(AnimParked);
        let frozen = app
            .world()
            .entity(uj[0])
            .get::<Transform>()
            .unwrap()
            .translation;
        step(&mut app, 0.1);
        assert_eq!(
            app.world()
                .entity(uj[0])
                .get::<Transform>()
                .unwrap()
                .translation,
            frozen,
            "parked bones hold"
        );
        app.world_mut().entity_mut(ours).remove::<AnimParked>();
        step(&mut app, 0.05);
        assert_twins_equal(&mut app, &oj, &uj); // woke to the absolute-clock pose
    }
}
