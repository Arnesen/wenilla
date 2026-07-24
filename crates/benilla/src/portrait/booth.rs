//! The booth **bake spawn** — how a mirrored look becomes the posed throwaway instance the
//! camera shoots (the ref mechanism, wow-re portrait-render §4 D2). [`spawn_booth_model`] is the
//! whole surface; the sync systems in [`super`] build [`BoothPart`]/[`BoothRider`] lists from the
//! unit's mirrored children and hand them here.

use benilla_formats::BillboardKind;
use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;

use crate::terrain::WowModelMaterial;

/// One mesh headed into a booth bake: the mirrored part's twins + its studio-lit material.
pub(super) struct BoothPart {
    pub(super) skinned: Option<Handle<Mesh>>,
    pub(super) static_mesh: Handle<Mesh>,
    pub(super) material: Handle<WowModelMaterial>,
}

/// One bone rider headed into a booth bake ([`PortraitRider`], studio-lit).
pub(super) struct BoothRider {
    pub(super) mesh: Handle<Mesh>,
    pub(super) material: Handle<WowModelMaterial>,
    pub(super) bone: u16,
    pub(super) offset: Vec3,
}

/// One character billboard batch headed into a booth bake — the undead/night-elf **eye-glow** (a
/// camera-facing quad on the eye bone, geoset 302 / geoset 0, additive-fullbright). The world path
/// splits these into camera-facing cards ([`crate::billboard`]); a booth is a *separate* camera, so
/// the booth seats the same centred quad on its billboard bone's joint and re-faces it to the booth
/// camera itself ([`face_booth_billboards`]). Its centred quad, fullbright material, the billboard
/// bone it rides, and the flag arm.
pub(super) struct BoothBillboardSpec {
    pub(super) mesh: Handle<Mesh>,
    pub(super) material: Handle<WowModelMaterial>,
    pub(super) bone: u16,
    pub(super) kind: BillboardKind,
}

/// A spawned booth billboard card: the centred quad seated on its billboard bone's joint (which
/// bakes the bone pivot — the 0130 rig identity), re-faced to THIS booth's camera every frame by
/// [`face_booth_billboards`]. The card despawns with the booth root's joints on the next re-bake.
#[derive(Component)]
pub(super) struct BoothBillboard {
    kind: BillboardKind,
}

/// How the booth bake's `AnimationPlayer` runs. Portraits are a **still** ([`Self::Frozen`] — Stand
/// paused at t = 0, the ref bake); the char-create preview is a **live scene** ([`Self::Loop`] —
/// Stand looping), the one case where the ref screen itself animates (decision 0423).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BoothMotion {
    Frozen,
    Loop,
}

/// Spawn a booth bake under `root` on the booth's layer — the ref mechanism (wow-re §4 D2): a
/// **fresh throwaway instance posed at Stand**, never the unit's live world pose.
///
/// With a rig (skeleton + inverse bindposes; every M2 display), the booth builds a joint hierarchy,
/// draws each part's **skinned** twin bound to it, seats riders under their bone's joint, and arms
/// the model's own Stand (anim id 0 through its baked resolution — the ref's loader-idle seed):
/// `motion` decides whether that Stand is **frozen at t = 0** (a portrait still) or **looping** (the
/// live glue scenes/preview — decisions 0423 + 0539). (The ref's own sampling clock is the one
/// unsettled INFERRED point of the verdict — t≈0 vs live phase; a frozen t=0 is inside its
/// envelope either way.) Without a rig (boneless / WMO-display / rig not built), the static
/// bind-pose bake: parts at identity, riders dropped (no bones to seat them on).
///
/// Returns the joint entities (empty for the boneless bake) — the glue scene seats its particle
/// emitters on them (decision 0539 §5).
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_booth_model(
    commands: &mut Commands,
    root: Entity,
    layer: RenderLayers,
    parts: &[BoothPart],
    riders: &[BoothRider],
    rig: Option<(
        &benilla_assets::ModelSkeleton,
        &Handle<bevy::mesh::skinning::SkinnedMeshInverseBindposes>,
        Option<&benilla_assets::ModelAnimations>,
    )>,
    catalog: Option<&benilla_formats::AnimDataCatalog>,
    motion: BoothMotion,
    // Per-hand weapon grip `[right, left]` — hold that hand's `HandsClosed` finger pose because a weapon
    // occupies its attach point (the paperdoll rule, wow-re `hand-grip-mechanism.md` §4c). The glue
    // preview draws its weapons into the hands, so it grips; the still portraits/paper-doll sheath theirs
    // (decision 0465) → `[false, false]`, hands stay open.
    grip: [bool; 2],
    // Character billboard batches (the undead/night-elf eye-glow) — seated on their billboard bone's
    // joint and re-faced to the booth camera by [`face_booth_billboards`]. Needs the rig (no bones =
    // no eye bone); the boneless bake below drops them. `&[]` for booths that dress none.
    billboards: &[BoothBillboardSpec],
) -> Vec<Entity> {
    use bevy::mesh::skinning::SkinnedMesh;
    // A re-bake must not inherit the previous model's animation state on the shared root — nor a
    // stale global-sequence drive holding the despawned joints.
    commands.entity(root).remove::<(
        AnimationPlayer,
        AnimationGraphHandle,
        crate::creature_anim::GlobalSeqDrive,
    )>();
    let Some((skeleton, ibp, anims)) = rig.filter(|(s, _, _)| !s.joints.is_empty()) else {
        for p in parts {
            commands.spawn((
                Mesh3d(p.static_mesh.clone()),
                MeshMaterial3d(p.material.clone()),
                Transform::IDENTITY,
                layer.clone(),
                ChildOf(root),
            ));
        }
        return Vec::new();
    };
    let joints = crate::entities::spawn_joints(commands, root, skeleton);
    // The model's global-sequence bone channels, by motion (decision 0539 §5):
    // - **Loop** (the glue scenes + the create/select character): LIVE, on the world's own
    //   clock-driven sampler — the login gate's fires flicker, the Tauren windmill turns, the
    //   character blinks.
    // - **Frozen** (portrait stills): frozen at t = 0 — for the eyelid that is scale 0 there (lid
    //   retracted, eye OPEN), matching "Stand frozen at t = 0" (a still must hold the open frame).
    //   Stand keys no global-sequence bone, so the paused player never overwrites the freeze.
    //   Without it the eyelid sits at identity scale — eye shut.
    if let Some(anims) = anims {
        match motion {
            BoothMotion::Loop => {
                if let Some(drive) =
                    crate::creature_anim::GlobalSeqDrive::new(&anims.global_bones, &joints)
                {
                    commands.entity(root).insert(drive);
                }
            }
            BoothMotion::Frozen => {
                for gb in &anims.global_bones {
                    let Some(&j) = joints.get(gb.bone as usize) else {
                        continue;
                    };
                    let rest = skeleton
                        .joints
                        .get(gb.bone as usize)
                        .map_or(Vec3::ZERO, |jt| jt.local_translation);
                    let mut tf = Transform::from_translation(rest);
                    if let Some(c) = &gb.translation {
                        tf.translation = c.sample(0.0);
                    }
                    if let Some(c) = &gb.rotation {
                        tf.rotation = c.sample(0.0);
                    }
                    if let Some(c) = &gb.scale {
                        tf.scale = c.sample(0.0);
                    }
                    commands.entity(j).insert(tf);
                }
            }
        }
    }
    for p in parts {
        let mesh = p.skinned.clone().unwrap_or_else(|| p.static_mesh.clone());
        let mut child = commands.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(p.material.clone()),
            Transform::IDENTITY,
            layer.clone(),
            ChildOf(root),
        ));
        if p.skinned.is_some() {
            child.insert(SkinnedMesh {
                inverse_bindposes: ibp.clone(),
                joints: joints.clone(),
            });
        }
    }
    for r in riders {
        let Some(&joint) = joints.get(usize::from(r.bone)) else {
            continue; // bad bone index — bake the body without this rider
        };
        commands.spawn((
            Mesh3d(r.mesh.clone()),
            MeshMaterial3d(r.material.clone()),
            Transform::from_translation(r.offset),
            layer.clone(),
            ChildOf(joint),
        ));
    }
    // The eye-glow (and any character billboard): seat the centred quad on its billboard bone's
    // joint — the joint frame bakes the bone pivot, so the quad lands at the eye — and tag it for
    // [`face_booth_billboards`], which rewrites its rotation to the booth camera each frame. The
    // rotation the joint carries here (its Stand pose) is countered there. A bone the rig lacks
    // drops the card, like a rider.
    for bb in billboards {
        let Some(&joint) = joints.get(usize::from(bb.bone)) else {
            continue;
        };
        commands.spawn((
            Mesh3d(bb.mesh.clone()),
            MeshMaterial3d(bb.material.clone()),
            Transform::IDENTITY,
            layer.clone(),
            ChildOf(joint),
            BoothBillboard { kind: bb.kind },
        ));
    }
    // Arm Stand and freeze: the player is configured *before* insertion (plain component data),
    // so the pose lands with the first animation pass — no play-after-spawn ordering dance.
    if let Some(anims) = anims {
        let stand = catalog.map_or(0, |c| anims.resolve(0, c).id);
        if let Some(clip) = anims.find(stand) {
            let mut player = AnimationPlayer::default();
            // A portrait is a still (Stand paused at t = 0); the char-create preview is a live scene
            // (Stand looping) — the one case the ref screen itself animates (decision 0423).
            match motion {
                BoothMotion::Frozen => {
                    player.play(clip.node).pause();
                }
                BoothMotion::Loop => {
                    player.play(clip.node).repeat();
                }
            }
            // Close each hand that holds a weapon: play its `HandsClosed` finger overlay *over* Stand
            // (masked to that hand's finger subtree, weight-dominant), held with `.repeat()` because it
            // is a single-key clamp pose — the same arming the live [`drive_hand_grip`] does, applied
            // once at spawn since a booth bake's grip never changes after it's built.
            for (hand, want) in grip.into_iter().enumerate() {
                if let (true, Some(node)) = (want, anims.hand_close[hand]) {
                    let active = player.play(node);
                    active.repeat();
                    active.set_weight(crate::creature_anim::HAND_GRIP_WEIGHT);
                }
            }
            commands
                .entity(root)
                .insert((player, AnimationGraphHandle(anims.graph.clone())));
            for (i, &j) in joints.iter().enumerate() {
                commands.entity(j).insert((
                    benilla_assets::bone_target_id(i as u16),
                    bevy::animation::AnimatedBy(root),
                ));
            }
        }
    }
    joints
}

/// Re-face each booth billboard card ([`BoothBillboard`]) to its booth's camera — the booth twin of
/// the world's [`crate::billboard::face_billboards`]. Each booth owns one camera, matched here by
/// their shared render layer. The card is a child of its billboard bone's joint, so we set its
/// **local** rotation to counter the joint's world rotation and land the world rotation on the
/// camera basis; the joint carries translation/scale (the eye pivot, the booth/character scale). The
/// joint pose is read a frame stale (its global is last propagate's), invisible on the near-static
/// Stand loop the booth runs — the same latency budget the paper-doll/portrait stills already accept.
pub(super) fn face_booth_billboards(
    cams: Query<(&GlobalTransform, &RenderLayers), With<super::BoothCam>>,
    joints: Query<&GlobalTransform>,
    mut cards: Query<(&BoothBillboard, &ChildOf, &RenderLayers, &mut Transform)>,
) {
    for (card, child_of, layers, mut tf) in &mut cards {
        let Some((cam, _)) = cams.iter().find(|(_, l)| l.intersects(layers)) else {
            continue; // the card's booth camera isn't up (booth torn down) — leave it be
        };
        let Ok(joint) = joints.get(child_of.parent()) else {
            continue;
        };
        let basis = crate::billboard::billboard_basis(
            card.kind,
            Quat::IDENTITY,
            *cam.forward(),
            *cam.right(),
            *cam.up(),
        );
        tf.rotation = joint.rotation().inverse() * basis;
    }
}
