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
    /// The batch's **animated material alpha** (`ModelSubmesh::alpha_anim`) — a time-varying
    /// colour-alpha/weight loop or, far more often here, a plain authored dimming *constant*. Every
    /// world lane has sampled this into the render-alpha `MeshTag` since decision 0130 phase 2; the
    /// booths never did, so a booth batch drew at alpha 1.0 whatever the artist authored (decision
    /// 0807 — B121's black corners were UI_Tauren's 0.55 `LENSALPHA` vignette at full strength).
    /// `None` for the overwhelming majority of batches, which are opaque.
    pub(super) alpha_anim: Option<std::sync::Arc<benilla_formats::AlphaAnim>>,
}

/// Marks a booth part whose render-alpha `MeshTag` is driven by its own
/// [`MatAnim`](crate::doodad_anim::MatAnim) sample — the booth twin of the world lane's writer.
///
/// The world path's writer is the *visibility authority* (`debug_panel::apply_model_visibility`),
/// which is scoped to `ModelPart` + `GlobalTransform` and culls by distance to the **world** camera.
/// A booth part must never enter that query — it would be far-clipped against a camera it has
/// nothing to do with — so the booth owns this one small writer instead
/// ([`push_booth_mat_alpha`]). Marker-scoped rather than inferred from "has no `ModelPart`", so the
/// two lanes cannot silently overlap.
#[derive(Component)]
pub(super) struct BoothMatAlpha;

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
    palettes: &mut crate::rig_palette::RigPalettes,
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
    // A re-bake must not inherit the previous model's animation state on the shared root — nor a
    // stale global-sequence drive holding the despawned joints, nor the previous rig's palette
    // slot (`RigSkin`'s on_replace hook frees it; the boneless bake below has no new one).
    commands.entity(root).remove::<(
        AnimationPlayer,
        AnimationGraphHandle,
        crate::creature_anim::GlobalSeqDrive,
        crate::rig_palette::RigSkin,
    )>();
    let Some((skeleton, ibp, anims)) = rig.filter(|(s, _, _)| !s.joints.is_empty()) else {
        for p in parts {
            let mut child = commands.spawn((
                Mesh3d(p.static_mesh.clone()),
                MeshMaterial3d(p.material.clone()),
                Transform::IDENTITY,
                layer.clone(),
                ChildOf(root),
            ));
            // The authored material alpha reaches the boneless bake too — it is a property of the
            // batch, not of the rig.
            if let Some(anim) = &p.alpha_anim {
                let mat_anim = crate::doodad_anim::MatAnim::driving_tag(anim.clone(), 0.0, None);
                child.insert((
                    bevy::mesh::MeshTag(crate::mesh_tag::alpha_bits(mat_anim.current)),
                    mat_anim,
                    BoothMatAlpha,
                ));
            }
        }
        return Vec::new();
    };
    let joints = crate::entities::spawn_joints(commands, root, root, skeleton);
    // The owned palette rig (decision 0720): the booth's skinned parts tag this slot; the palette
    // compute reads these joints like any world rig, and the booth's studio light buffer mirrors
    // the palette region (`rig_palette::RigPaletteMirrors`), so the booth camera sees the pose.
    let rig_slot = crate::rig_palette::RigSkin::allocate(palettes, joints.clone(), ibp.clone())
        .map_or(0, |rig| {
            let slot = rig.slot;
            commands.entity(root).insert(rig);
            // Booth materials bind a STUDIO light buffer, not the shared one — route this
            // rig's rows to the registered mirrors too.
            palettes.mark_mirrored(slot);
            slot
        });
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
        let use_rig = p.skinned.is_some() && rig_slot != 0;
        let mesh = if use_rig {
            p.skinned.clone().expect("use_rig ⇒ skinned twin present")
        } else {
            p.static_mesh.clone()
        };
        let mut child = commands.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(p.material.clone()),
            Transform::IDENTITY,
            layer.clone(),
            ChildOf(root),
        ));
        // The batch's authored material alpha, sampled onto the render-alpha tag field — the same
        // `MatAnim` the world lanes use, in its self-driving form (`drives_tag`: nothing else writes
        // a booth part's alpha). The rig field rides the same tag, so compose rather than overwrite;
        // `alpha_bits` floors a true zero at ≈0 so the shader's whole-payload-`0` *untagged ⇒
        // opaque* sentinel can't fire on a batch the artist authored invisible.
        let rig_tag = if use_rig {
            crate::mesh_tag::rig_bits(rig_slot)
        } else {
            0
        };
        if let Some(anim) = &p.alpha_anim {
            let mat_anim = crate::doodad_anim::MatAnim::driving_tag(anim.clone(), 0.0, None);
            child.insert((
                bevy::mesh::MeshTag(rig_tag | crate::mesh_tag::alpha_bits(mat_anim.current)),
                mat_anim,
                BoothMatAlpha,
            ));
        } else if use_rig {
            child.insert(bevy::mesh::MeshTag(
                rig_tag | crate::mesh_tag::alpha_bits(1.0),
            ));
        }
        if use_rig {
            child.insert(crate::rig_palette::RigPart(root));
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

/// Push each booth part's sampled material alpha onto its render-alpha `MeshTag` field — the booth's
/// own tiny twin of the world visibility authority's write (see [`BoothMatAlpha`] for why the world
/// one can't serve here).
///
/// `doodad_anim::sample_mat_anim` already ticks **every** `MatAnim` in the world, booth parts
/// included, so this only moves the sampled value onto the channel: `with_alpha` writes the alpha
/// field alone, so the rig slot (and the probe slot the rig lane reads from the same tag) ride
/// through untouched. Write-on-change, so the overwhelmingly common case — an authored *constant*
/// like UI_Tauren's 0.55 vignette — costs one compare per part per frame and never re-batches.
pub(super) fn push_booth_mat_alpha(
    mut parts: Query<(&crate::doodad_anim::MatAnim, &mut bevy::mesh::MeshTag), With<BoothMatAlpha>>,
) {
    for (anim, mut tag) in &mut parts {
        let bits = crate::mesh_tag::with_alpha(tag.0, anim.current);
        if tag.0 != bits {
            tag.0 = bits;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doodad_anim::MatAnim;

    /// A constant authored alpha, the shape every glue scene actually uses (UI_Tauren's vignette is
    /// `0.55..0.55`, its ground shadow `0.99..0.99`).
    fn constant_alpha(v: f32) -> std::sync::Arc<benilla_formats::AlphaAnim> {
        std::sync::Arc::new(
            benilla_formats::AlphaAnim::new(vec![benilla_formats::AlphaSeq {
                color: None,
                weight: Some(benilla_formats::ScalarAnim {
                    period: 0.0,
                    step: true,
                    keys: vec![(0.0, v)],
                }),
            }])
            .expect("a dimming constant is worth carrying"),
        )
    }

    /// The writer moves the sampled alpha onto the tag **and leaves the rig slot alone** — the whole
    /// reason it goes through `with_alpha` rather than assigning the field. A booth part is skinned,
    /// so a writer that clobbered bits 19..=29 would silently unbind its palette.
    #[test]
    fn the_booth_alpha_writer_preserves_the_rig_slot() {
        let mut app = App::new();
        app.add_systems(Update, push_booth_mat_alpha);
        let rig_slot = 7u16;
        let anim = MatAnim::driving_tag(constant_alpha(0.55), 0.0, None);
        let part = app
            .world_mut()
            .spawn((
                bevy::mesh::MeshTag(
                    crate::mesh_tag::rig_bits(rig_slot) | crate::mesh_tag::alpha_bits(1.0),
                ),
                anim,
                BoothMatAlpha,
            ))
            .id();
        app.update();

        let tag = app
            .world()
            .entity(part)
            .get::<bevy::mesh::MeshTag>()
            .unwrap();
        assert_eq!(
            crate::mesh_tag::rig_of(tag.0),
            rig_slot,
            "the palette slot must survive an alpha write"
        );
        let alpha = crate::mesh_tag::alpha_of(tag.0);
        assert!(
            (alpha - 0.55).abs() <= 1.0 / 63.0,
            "authored 0.55 reached the tag (got {alpha})"
        );
    }

    /// An unmarked part is not ours to write: the world lane's parts carry `MatAnim` too, and their
    /// alpha is composed by the visibility authority against the fade and the interior classifier.
    #[test]
    fn the_booth_alpha_writer_ignores_unmarked_parts() {
        let mut app = App::new();
        app.add_systems(Update, push_booth_mat_alpha);
        let part = app
            .world_mut()
            .spawn((
                bevy::mesh::MeshTag(crate::mesh_tag::alpha_bits(1.0)),
                MatAnim::driving_tag(constant_alpha(0.1), 0.0, None),
            ))
            .id();
        app.update();
        assert_eq!(
            crate::mesh_tag::alpha_of(
                app.world()
                    .entity(part)
                    .get::<bevy::mesh::MeshTag>()
                    .unwrap()
                    .0
            ),
            1.0,
            "no BoothMatAlpha marker ⇒ untouched"
        );
    }
}
