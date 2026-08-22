//! The booth **bake spawn** — how a mirrored look becomes the posed throwaway instance the
//! camera shoots (the ref mechanism, wow-re portrait-render §4 D2). [`spawn_booth_model`] is the
//! whole surface; the sync systems in [`super`] build [`BoothPart`]/[`BoothRider`] lists from the
//! unit's mirrored children and hand them here.
//!
//! Since decision 1443 the bake rides the **collapsed rig lane** (0724/1365): the skeleton is a
//! [`RigPose`](benilla_world::rig_anim::RigPose) buffer on the booth root — no joint entities,
//! no per-bone `AnimatedBy` seats in bevy's `animate_targets` sweep (the booth doll was the last
//! consumer of that lane; ~0.35 ms/frame of the open char window, decision 1441). Consumers
//! (riders, cards, effect hosts) seat on demand-spawned anchors ([`BoothRig::anchor`]), and the
//! park is one [`AnimParked`](benilla_world::rig_anim::AnimParked) marker on the root.

use benilla_formats::BillboardKind;
use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;

use benilla_assets::materials::WowModelMaterial;

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

/// Marks a booth part whose render-alpha `MeshTag` **and `Visibility`** are driven by its own
/// [`MatAnim`](benilla_world::doodad_anim::MatAnim) sample — the booth twin of the world lane's writer.
///
/// The world path's writer is the *visibility authority* (`debug_panel::apply_model_visibility`),
/// which is scoped to `ModelPart` + `GlobalTransform` and culls by distance to the **world** camera.
/// A booth part must never enter that query — it would be far-clipped against a camera it has
/// nothing to do with — so the booth owns this one small writer instead
/// ([`push_booth_mat_alpha`]). Marker-scoped rather than inferred from "has no `ModelPart`", so the
/// two lanes cannot silently overlap.
///
/// Being out of that query means being out of **both** halves of what it does, and the booth owes
/// its parts both: the alpha field *and* the `mat_factor > 0.0` term in its `desired` verdict — the
/// reference's `A <= 0` cull, which fires before the blend mode is ever read (wow-re
/// `m2-alpha-combine-cull.md`). Marking the marker is therefore also a claim of sole `Visibility`
/// authority over the parts that carry it; nothing else in the booth writes theirs.
#[derive(Component)]
pub(super) struct BoothMatAlpha;

/// One bone rider headed into a booth bake ([`PortraitRider`], studio-lit).
pub(super) struct BoothRider {
    pub(super) mesh: Handle<Mesh>,
    pub(super) material: Handle<WowModelMaterial>,
    pub(super) bone: u16,
    pub(super) offset: Vec3,
}

/// One camera-facing batch headed into a booth bake — the undead/night-elf **eye-glow** (a quad on
/// the eye bone, geoset 302 / geoset 0, additive-fullbright), or an equipped item's / item glow's own
/// such batch. The world path splits these into camera-facing cards ([`benilla_world::billboard`]); a booth
/// is a *separate* camera, so the booth seats the same centred quad under its bone's joint and
/// re-faces it to the booth camera itself ([`face_booth_billboards`]). Its centred quad, material,
/// the bone it rides, where its pivot sits in that joint's frame, and the flag arm.
pub(super) struct BoothBillboardSpec {
    pub(super) mesh: Handle<Mesh>,
    pub(super) material: Handle<WowModelMaterial>,
    pub(super) bone: u16,
    /// The card pivot's offset under that bone's joint (Bevy axes). `ZERO` for a batch of the rigged
    /// model itself — the joint frame already bakes the bone pivot (the 0130 rig identity).
    pub(super) offset: Vec3,
    pub(super) kind: BillboardKind,
}

/// A spawned booth billboard card: the centred quad seated on its billboard bone's joint (which
/// bakes the bone pivot — the 0130 rig identity), re-faced to THIS booth's camera every frame by
/// [`face_booth_billboards`]. The card despawns with the booth root's joints on the next re-bake.
#[derive(Component)]
pub(super) struct BoothBillboard {
    kind: BillboardKind,
}

impl BoothBillboard {
    /// A booth billboard **frame** — a [`BoothBillboard`] with no `Mesh3d`: an entity whose live
    /// world rotation is a billboard bone's replaced palette basis about this booth's camera, for
    /// the other consumers of that matrix to ride. It draws nothing; only its transform matters.
    ///
    /// The booth twin of [`benilla_world::billboard::BillboardCard::frame_following`], and the same
    /// reasoning (decision 0813 §3, itself 0153's rule): same basis function, same law, one system
    /// — not a second mechanism. Today's caller is the glue booth's equipment riders
    /// ([`super::glue_booth`]): an item model spawns no rig, so nothing else would apply the
    /// replacement to a particle emitter hanging under its billboard bone, and the reference folds
    /// the emitter's record position through exactly this matrix (wow-re
    /// `part-anchoring-live-bone.md` §1 row 3).
    ///
    /// The caller owns the frame's **translation** (the billboard bone's pivot in its parent's
    /// frame); [`face_booth_billboards`] writes only the rotation.
    pub(super) fn frame(kind: BillboardKind) -> Self {
        Self { kind }
    }
}

/// One **effect-bearing model** headed into a booth bake, as its particle emitters plus the composed
/// seat: the body bone it rides and its offset in that bone's frame. The booth spawns one host there
/// and owns the emitters off it (the scene-brazier recipe, decision 0539 §5); any geometry the same
/// model carries rides the ordinary [`BoothRider`] / [`BoothBillboardSpec`] lists at the same seat.
///
/// The two lanes that feed it are the two lanes that mirror a dressed look — a live unit's
/// [`crate::portrait::PortraitEffects`] markers, and the glue bake's [`super::PreviewEffects`] —
/// and both mean the same two things: an equipped item's own emitters (decision 0813, `#bugs` B118)
/// and the `ItemVisuals` glow a held weapon hangs on its own attachment points (decision 0805).
pub(super) struct BoothEffects {
    pub(super) bone: u16,
    pub(super) offset: Vec3,
    pub(super) emitters: Vec<benilla_assets::ModelEmitter>,
}

/// The in-flight bake's rig: the booth root plus its pending [`RigPose`] buffer (`None` = the
/// boneless bake). Consumers seat on demand-spawned bone anchors via [`Self::anchor`]; the caller
/// MUST [`Self::finish`] once every consumer is seated — the pose buffer only reaches the ECS
/// then, anchors included (they are registered *in* the component).
#[must_use = "call finish(): the pose buffer reaches the booth root only then"]
pub(super) struct BoothRig {
    root: Entity,
    rig: Option<benilla_world::rig_anim::RigPose>,
}

impl BoothRig {
    /// The anchor entity standing in for `bone` (decision 1355's shape, shared with every world
    /// rig): spawned on first demand, seated at the current composed pose, re-seated by the
    /// compose pass every animated frame. `None` = boneless bake or a bone outside the skeleton —
    /// the consumer misses, exactly as a bad joint index always did.
    pub(super) fn anchor(&mut self, commands: &mut Commands, bone: u16) -> Option<Entity> {
        let root = self.root;
        self.rig.as_mut()?.anchor_for(commands, root, bone)
    }

    /// Whether a rig stands at all — the park gate's "is there anything to park".
    pub(super) fn rigged(&self) -> bool {
        self.rig.is_some()
    }

    /// Commit the pose buffer onto the booth root — after every consumer is seated. `StageRig`
    /// rides with it (decision 1447): the pair exists together or not at all, so the world-view
    /// parker can never see a booth `RigPose` without the marker that exempts it.
    pub(super) fn finish(self, commands: &mut Commands) {
        if let Some(rig) = self.rig {
            commands.entity(self.root).insert((rig, super::StageRig));
        }
    }
}

/// Strip a booth root's whole rig state — the bake teardown twin of the child despawn. Every
/// empty/re-bake arm must run this: `despawn_related::<Children>` reaps the meshes and anchors,
/// but the pose buffer, the player, the palette slot and the park marker all live ON the root,
/// and a leftover `RigPose`+`AnimationPlayer` pair keeps evaluating (and re-writing palette rows
/// through a leaked `RigSkin` slot) for as long as the booth stands empty.
pub(super) fn clear_booth_rig(commands: &mut Commands, root: Entity) {
    commands.entity(root).remove::<(
        AnimationPlayer,
        AnimationGraphHandle,
        benilla_assets::ModelAnimations,
        benilla_world::rig_anim::GlobalSeqDrive,
        benilla_world::rig_anim::RigPose,
        benilla_world::rig_anim::AnimParked,
        super::StageRig,
        benilla_world::rig_palette::RigSkin,
    )>();
}

/// Spawn `effects` into a booth bake: one host per effect model, seated on its bone's anchor
/// at the model's offset, with that model's emitters owned by the host. Returns
/// `(emitters, frames)` — how many emitters went up, and how many of them ride a billboard frame.
///
/// `light` is the booth's own light-storage buffer, bound onto every emitter
/// ([`benilla_world::particles::buffer::EffectLightOverride`]) for the same reason a booth part gets a
/// material twin: the world buffer would fog and shade a pane by the world's time of day.
///
/// **Which booths call this is a fidelity split, not a cost one.** The body panes
/// (`<PlayerModel>` widgets — the character window's paper doll, the inspect twin) are *live* in the
/// reference: the widget renders through the per-frame `CSimpleModel` path whose light its ctor
/// stages every frame (`0x76d680`, decision 0638) — the same widget family as the animating glue
/// preview (decision 0423). The round unit-frame portraits are not: `SetPortraitTexture` bakes a
/// fresh instance in ONE draw and caches the texture by GUID/displayId, returning it with no
/// re-render (wow-re `portrait-render.md` §2). A one-frame draw of a freshly-born particle pool
/// yields nothing, so emitters have no place in a portrait — and a booth that owns emitters must
/// keep its camera awake ([`crate::portrait::Booth::live`]), which is exactly what a still should
/// never need to do.
pub(super) fn spawn_booth_effects(
    commands: &mut Commands,
    rig: &mut BoothRig,
    layer: &RenderLayers,
    light: Option<&bevy::render::render_resource::Buffer>,
    effects: &[BoothEffects],
) -> (usize, usize) {
    let mut spawned = 0usize;
    let mut frames = 0usize;
    for fx in effects {
        let Some(joint) = rig.anchor(commands, fx.bone) else {
            continue; // bad bone index — bake the body without this model's effects
        };
        let host = commands
            .spawn((
                Transform::from_translation(fx.offset),
                Visibility::default(),
                ChildOf(joint),
                layer.clone(),
            ))
            .id();
        for em in &fx.emitters {
            // A **billboard** bone in the emitter's chain (decision 0813): its palette rows are
            // replaced with the rendering camera's basis about the bone's own pivot, and children
            // multiply onto that, so the origin is `pivot + camBasis·(position − pivot)`. A booth is
            // its own camera, so the frame carrying the replacement is the booth twin of the world
            // lane's card — a mesh-less [`BoothBillboard`], faced by [`face_booth_billboards`] and
            // seated at the pivot under the host. No billboard in the chain (every shipped glow
            // model, most items) → the host itself owns the emitter and the rest pose stands.
            let owner = match em.billboard {
                Some(bb) => {
                    let frame = commands
                        .spawn((
                            Transform::from_translation(benilla_assets::coords::wow_to_bevy(
                                bb.pivot,
                            )),
                            layer.clone(),
                            ChildOf(host),
                            BoothBillboard::frame(bb.kind),
                        ))
                        .id();
                    frames += 1;
                    (frame, bb.pivot)
                }
                None => (host, [0.0; 3]),
            };
            let Some(e) = benilla_world::particles::spawn_emitter(
                commands,
                em,
                Transform::IDENTITY,
                benilla_world::particles::EmitterFrames {
                    owner: Some(owner),
                    attach: Some(host), // an attached model — its fan swings with the item
                    anchor: Some(host), // the cloud anchors at the MODEL; bones compose births only
                    // A booth rider's host is torn down with the bake it belongs to.
                    on_owner_loss: benilla_world::particles::OwnerLoss::Free,
                    // A booth bake has no appear/despawn ramp and no self-avatar feather — its
                    // riders are always opaque (0827).
                    alpha: None,
                    // Scene-graph-carried: this model's world motion arrives on the reference's
                    // device stack, so its cloud RIDES (0986's baseline).
                    world_composed: false,
                },
                benilla_world::particles::EmitClock::Pinned, // an item's effects loop forever
            ) else {
                continue;
            };
            commands.entity(e).insert((layer.clone(), ChildOf(host)));
            if let Some(buf) = light {
                commands
                    .entity(e)
                    .insert(benilla_world::particles::buffer::EffectLightOverride(
                        buf.clone(),
                    ));
            }
            spawned += 1;
        }
    }
    (spawned, frames)
}

/// Spawn a booth model's **OWN** particle emitters — the ones authored on its own skeleton, riding
/// its own joints. The twin of [`spawn_booth_effects`], and the distinction is which model the
/// emitters belong to: that one seats a *separate* model's emitters (an equipped item's) on a body
/// bone through a host; this one is the model itself burning — the backdrop scene's braziers, the
/// select pet's flames. Returns `(emitters, frames)` like its twin.
///
/// `root` is the bake root: the fallback owner when a bone has no anchor (boneless bake, bad index),
/// the cloud's anchor, and the parent every emitter is hung under so a re-bake's
/// `despawn_related::<Children>` reaps them.
///
/// **The billboard arm is why this is a shared function and not two loops.** A booth pose drops the
/// camera arms ([`benilla_world::rig_anim::RigPose::without_camera_billboards`] — the world pass
/// would face them at the *world* camera), so unlike a rigged model in the world, a booth's emitter
/// whose chain reaches a billboard bone gets no replacement from its palette and would sit at its
/// rest-pose origin. It needs the same mesh-less [`BoothBillboard`] frame the no-rig lanes use —
/// seated here on the **billboard bone's own joint** at `ZERO`, because that joint already bakes the
/// pivot with the chain above it folded in (the 0130 rig identity, `BoothBillboardSpec::offset`'s
/// rule). Only when that bone has no anchor does it fall back to the rest-pose placement at the
/// pivot under `root`, which is all the no-rig lanes can ever do.
pub(super) fn spawn_booth_own_emitters(
    commands: &mut Commands,
    rig: &mut BoothRig,
    root: Entity,
    layer: &RenderLayers,
    light: Option<&bevy::render::render_resource::Buffer>,
    emitters: &[benilla_assets::ModelEmitter],
) -> (usize, usize) {
    let mut spawned = 0usize;
    let mut frames = 0usize;
    for em in emitters {
        let owner = match em.billboard {
            Some(bb) => {
                // The joint bakes the pivot → the frame sits at ZERO and only its rotation counts.
                // No joint (boneless bake) → the rest-pose placement, at the pivot under the root.
                let (seat, at) = match rig.anchor(commands, bb.bone) {
                    Some(joint) => (joint, Vec3::ZERO),
                    None => (root, benilla_assets::coords::wow_to_bevy(bb.pivot)),
                };
                let frame = commands
                    .spawn((
                        Transform::from_translation(at),
                        layer.clone(),
                        ChildOf(seat),
                        BoothBillboard::frame(bb.kind),
                    ))
                    .id();
                frames += 1;
                (frame, bb.pivot)
            }
            // The ordinary case: the emitter rides its own bone's joint, in that bone's frame.
            None => rig
                .anchor(commands, em.def.bone)
                .map_or((root, [0.0; 3]), |joint| (joint, em.bone_pivot)),
        };
        let Some(e) = benilla_world::particles::spawn_emitter(
            commands,
            em,
            Transform::IDENTITY,
            benilla_world::particles::EmitterFrames {
                owner: Some(owner),
                attach: None, // this model IS the host — nothing it hangs off swings it
                anchor: Some(root),
                // The bake root is torn down and rebuilt as a whole.
                on_owner_loss: benilla_world::particles::OwnerLoss::Free,
                // A booth bake has no appear/despawn ramp and no self-avatar feather (0827).
                alpha: None,
                // Scene-graph-carried: this model's motion arrives on the device stack (0986).
                world_composed: false,
            },
            // A booth loops its one authored clip forever — the doodad law.
            benilla_world::particles::EmitClock::Pinned,
        ) else {
            continue;
        };
        commands.entity(e).insert((layer.clone(), ChildOf(root)));
        if let Some(buf) = light {
            commands
                .entity(e)
                .insert(benilla_world::particles::buffer::EffectLightOverride(
                    buf.clone(),
                ));
        }
        spawned += 1;
    }
    (spawned, frames)
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
/// With a rig (skeleton + inverse bindposes; every M2 display), the booth builds a collapsed
/// [`RigPose`](benilla_world::rig_anim::RigPose) buffer (decision 1443 — no joint entities),
/// draws each part's **skinned** twin against its palette slot, seats riders on their bone's
/// anchor, and arms the model's own Stand (anim id 0 through its baked resolution — the ref's
/// loader-idle seed): `motion` decides whether that Stand is **frozen at t = 0** (a portrait
/// still) or **looping** (the live glue scenes/preview — decisions 0423 + 0539). (The ref's own
/// sampling clock is the one unsettled INFERRED point of the verdict — t≈0 vs live phase; a
/// frozen t=0 is inside its envelope either way.) Without a rig (boneless / WMO-display / rig
/// not built), the static bind-pose bake: parts at identity, riders dropped (no bones to seat
/// them on).
///
/// Returns the [`BoothRig`] handle — seat any remaining consumers on it (the effect hosts, the
/// glue scene's emitters), then `finish()` it.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_booth_model(
    commands: &mut Commands,
    palettes: &mut benilla_world::rig_palette::RigPalettes,
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
) -> BoothRig {
    // A re-bake must not inherit the previous model's animation state on the shared root — nor a
    // stale pose buffer (its anchors died with the child despawn), nor the previous rig's palette
    // slot (`RigSkin`'s on_replace hook frees it; the boneless bake below has no new one).
    clear_booth_rig(commands, root);
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
                let mat_anim =
                    benilla_world::doodad_anim::MatAnim::driving_tag(anim.clone(), 0.0, None);
                child.insert((
                    bevy::mesh::MeshTag(benilla_world::mesh_tag::spawn_tag(0, mat_anim.current)),
                    mat_anim,
                    BoothMatAlpha,
                ));
            }
        }
        return BoothRig { root, rig: None };
    };
    // The collapsed pose buffer (decision 1443, replaying 0724/1365 for the last entity-joint
    // rig lane): bind-pose composed, evaluated in place by the 0712 evaluator off the player
    // below. `without_camera_billboards` because a booth is not the world camera — see its doc;
    // the booth's cards counter-rotate to their own camera instead.
    let mut pose =
        benilla_world::rig_anim::RigPose::new(root, skeleton).without_camera_billboards();
    // The owned palette rig (decision 0720): the booth's skinned parts tag this slot; the world
    // pass writes its rows from the composed pose, and the booth's studio light buffer mirrors
    // the palette region (`rig_palette::RigPaletteMirrors`), so the booth camera sees the pose.
    let rig_slot = benilla_world::rig_palette::RigSkin::allocate_bones(
        palettes,
        skeleton.joints.len() as u32,
        ibp.clone(),
    )
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
                if let Some(drive) = benilla_world::rig_anim::GlobalSeqDrive::new_rig(
                    &anims.global_bones,
                    pose.locals.len(),
                ) {
                    commands.entity(root).insert(drive);
                }
            }
            BoothMotion::Frozen => {
                for gb in &anims.global_bones {
                    // `locals` are seeded at bind pose, so the untouched properties keep the
                    // rest transform — the same base the joint-entity form built from.
                    let Some(tf) = pose.locals.get_mut(gb.bone as usize) else {
                        continue;
                    };
                    if let Some(c) = &gb.translation {
                        tf.translation = c.sample(0.0);
                    }
                    if let Some(c) = &gb.rotation {
                        tf.rotation = c.sample(0.0);
                    }
                    if let Some(c) = &gb.scale {
                        tf.scale = c.sample(0.0);
                    }
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
        let tag_slot = if use_rig { rig_slot } else { 0 };
        if let Some(anim) = &p.alpha_anim {
            let mat_anim =
                benilla_world::doodad_anim::MatAnim::driving_tag(anim.clone(), 0.0, None);
            child.insert((
                bevy::mesh::MeshTag(benilla_world::mesh_tag::spawn_tag(
                    tag_slot,
                    mat_anim.current,
                )),
                mat_anim,
                BoothMatAlpha,
            ));
        } else if use_rig {
            child.insert(bevy::mesh::MeshTag(benilla_world::mesh_tag::spawn_tag(
                tag_slot, 1.0,
            )));
        }
        if use_rig {
            child.insert(benilla_world::rig_palette::RigPart(root));
        }
    }
    let mut booth_rig = BoothRig {
        root,
        rig: Some(pose),
    };
    for r in riders {
        let Some(anchor) = booth_rig.anchor(commands, r.bone) else {
            continue; // bad bone index — bake the body without this rider
        };
        commands.spawn((
            Mesh3d(r.mesh.clone()),
            MeshMaterial3d(r.material.clone()),
            Transform::from_translation(r.offset),
            layer.clone(),
            ChildOf(anchor),
        ));
    }
    // The camera-facing batches (the eye-glow, an item's gem, a glow model's quad): seat the centred
    // quad under its bone's anchor at the spec's offset — zero for the body's own batches, whose
    // anchor frame already bakes the bone pivot (the 0130 rig identity), so the quad lands at the
    // eye — and tag it for [`face_booth_billboards`], which rewrites its rotation to the booth
    // camera each frame. The rotation the anchor carries here (its Stand pose) is countered there.
    // A bone the rig lacks drops the card, like a rider.
    for bb in billboards {
        let Some(anchor) = booth_rig.anchor(commands, bb.bone) else {
            continue;
        };
        commands.spawn((
            Mesh3d(bb.mesh.clone()),
            MeshMaterial3d(bb.material.clone()),
            Transform::from_translation(bb.offset),
            layer.clone(),
            ChildOf(anchor),
            BoothBillboard { kind: bb.kind },
        ));
    }
    // Arm Stand and freeze: the player is configured *before* insertion (plain component data),
    // so the pose lands with the first animation pass — no play-after-spawn ordering dance.
    // `ModelAnimations` goes up beside it: the 0712 evaluator reads the player THROUGH the baked
    // pose source (`(&AnimationPlayer, &ModelAnimations, &mut RigPose)`) — no `AnimatedBy`
    // targets exist to route bevy's own sweep here, which is the point (decision 1443).
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
            commands.entity(root).insert((
                player,
                AnimationGraphHandle(anims.graph.clone()),
                anims.clone(),
            ));
        }
    }
    booth_rig
}

/// Re-face each booth billboard card ([`BoothBillboard`]) to its booth's camera — the booth twin of
/// the world's [`benilla_world::billboard::face_billboards`]. Each booth owns one camera, matched here by
/// their shared render layer. The card is a child of its billboard bone's joint, so we set its
/// **local** rotation to counter the joint's world rotation and land the world rotation on the
/// camera basis; the joint carries translation/scale (the eye pivot, the booth/character scale). The
/// joint pose is read a frame stale (its global is last propagate's), invisible on the near-static
/// Stand loop the booth runs — the same latency budget the paper-doll/portrait stills already accept.
pub(super) fn face_booth_billboards(
    cams: Query<(&GlobalTransform, &RenderLayers, &Camera), With<super::BoothCam>>,
    joints: Query<&GlobalTransform>,
    mut cards: Query<(&BoothBillboard, &ChildOf, &RenderLayers, &mut Transform)>,
) {
    for (card, child_of, layers, mut tf) in &mut cards {
        let Some((cam, _, _)) = cams
            .iter()
            // A sleeping camera renders nothing — leave its cards be (the booth park); they
            // re-face on the wake window's first frame, before anything samples the target.
            .find(|(_, l, c)| c.is_active && l.intersects(layers))
        else {
            continue; // …or the card's booth camera isn't up at all (booth torn down)
        };
        let Ok(joint) = joints.get(child_of.parent()) else {
            continue;
        };
        let basis = benilla_world::billboard::billboard_basis(
            card.kind,
            Quat::IDENTITY,
            *cam.forward(),
            *cam.right(),
            *cam.up(),
        );
        tf.rotation = joint.rotation().inverse() * basis;
    }
}

/// Resolve each booth part's sampled material alpha — **cull it at `A <= 0`, dim it otherwise** —
/// the booth's own tiny twin of the world visibility authority (see [`BoothMatAlpha`] for why the
/// world one can't serve here).
///
/// `doodad_anim::sample_mat_anim` already ticks **every** `MatAnim` in the world, booth parts
/// included, so this only spends the sampled value. Two ways, exactly as
/// `model_render::visibility` spends it on a world part:
///
/// - **`A <= 0` → `Hidden`.** The reference culls the batch before it looks at the blend mode, so a
///   zero is a *disappearance*, not a fade to nothing — and only a `Visibility` write can say that.
///   The tag alpha reaches the shader's blend source, which an Opaque draw ignores outright
///   (`wow_model.wgsl`: "For steady cutout/opaque draws blend is off so this is ignored"), so a
///   batch the artist keyed off in this sequence would otherwise draw **solid**. That is not an
///   edge case: `alphascan` counts 56 of 420 creature models authoring per-sequence batch
///   visibility, and the shipped one that stands in this booth is the Voidwalker, whose two
///   shoulder props are keyed to zero for the whole of Stand and hung in the air without this.
///   `Inherited` rather than `Visible`, so a hidden bake root still takes its parts with it.
/// - **Otherwise → the alpha field.** `with_alpha` writes that field alone, so the rig slot (and the
///   probe slot the rig lane reads from the same tag) ride through untouched.
///
/// Both are write-on-change, so the overwhelmingly common case — an authored *constant* like
/// UI_Tauren's 0.55 vignette — costs two compares per part per frame and never re-batches.
///
/// `Visibility` is required, not optional, and that is safe by construction: every site that adds
/// [`BoothMatAlpha`] spawns the part with a `Mesh3d`, which requires `Visibility` — and a part
/// without one is not a booth draw at all. Required rather than `Option`, because an optional match
/// would quietly ship the dimming half alone again, which is the whole defect this system had.
pub(super) fn push_booth_mat_alpha(
    mut parts: Query<
        (
            &benilla_world::doodad_anim::MatAnim,
            &mut bevy::mesh::MeshTag,
            &mut Visibility,
        ),
        With<BoothMatAlpha>,
    >,
) {
    for (anim, mut tag, mut vis) in &mut parts {
        // The cull, the same term the world authority ANDs into its verdict as `mat_factor > 0.0`.
        let want = if anim.current > 0.0 {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *vis != want {
            *vis = want;
        }
        let bits = benilla_world::mesh_tag::with_alpha(tag.0, anim.current);
        if tag.0 != bits {
            tag.0 = bits;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_world::doodad_anim::MatAnim;

    /// A constant authored alpha, the shape every glue scene actually uses (UI_Tauren's vignette is
    /// `0.55..0.55`, its ground shadow `0.99..0.99`).
    fn constant_alpha(v: f32) -> std::sync::Arc<benilla_formats::AlphaAnim> {
        std::sync::Arc::new(
            benilla_formats::AlphaAnim::new(vec![benilla_formats::AlphaSeq {
                color: None,
                weight: Some(benilla_formats::ScalarAnim {
                    period: 0.0,
                    step: true,
                    wrap: true, // period 0: a constant has no clock
                    gseq: false,
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
                bevy::mesh::MeshTag(benilla_world::mesh_tag::spawn_tag(rig_slot, 1.0)),
                anim,
                BoothMatAlpha,
                // Mirrors production: both marker sites spawn a `Mesh3d`, which *requires*
                // `Visibility` — the writer's query takes it unconditionally, so a fixture without
                // one is not a booth part and would silently fall out.
                Visibility::Inherited,
            ))
            .id();
        app.update();

        let tag = app
            .world()
            .entity(part)
            .get::<bevy::mesh::MeshTag>()
            .unwrap();
        assert_eq!(
            benilla_world::mesh_tag::rig_of(tag.0),
            rig_slot,
            "the palette slot must survive an alpha write"
        );
        let alpha = benilla_world::mesh_tag::alpha_of(tag.0);
        assert!(
            (alpha - 0.55).abs() <= 1.0 / 63.0,
            "authored 0.55 reached the tag (got {alpha})"
        );
    }

    /// The `A <= 0` cull — the half the booth twin was missing, and the reason the Voidwalker
    /// stood in the char-select booth with two shoulder props hung in the air. The reference culls
    /// the batch before it reads the blend mode (wow-re `m2-alpha-combine-cull.md`), and the world
    /// lane says so as `mat_factor > 0.0` in its `desired` verdict
    /// (`model_render::visibility`); a tag write alone cannot express it, because an Opaque draw
    /// never looks at the blend source the tag alpha feeds. Both directions, in one run: a zeroed
    /// batch goes `Hidden`, a live one stays drawable, and the writer keeps up when the sample moves.
    #[test]
    fn a_batch_the_artist_zeroed_in_this_sequence_is_culled_not_merely_dimmed() {
        let mut app = App::new();
        app.add_systems(Update, push_booth_mat_alpha);
        let spawn = |app: &mut App, v: f32| {
            app.world_mut()
                .spawn((
                    bevy::mesh::MeshTag(benilla_world::mesh_tag::spawn_tag(0, 1.0)),
                    MatAnim::driving_tag(constant_alpha(v), 0.0, None),
                    BoothMatAlpha,
                    Visibility::Inherited,
                ))
                .id()
        };
        // The Voidwalker's Stand: the shoulder shackles keyed to 0, the wrist pair left at 1.
        let shoulder = spawn(&mut app, 0.0);
        let wrist = spawn(&mut app, 1.0);
        app.update();

        let vis = |app: &App, e: Entity| *app.world().entity(e).get::<Visibility>().unwrap();
        assert_eq!(
            vis(&app, shoulder),
            Visibility::Hidden,
            "A <= 0 is a disappearance, not a fade to nothing — an Opaque draw ignores the tag"
        );
        assert_eq!(
            vis(&app, wrist),
            Visibility::Inherited,
            "and a live batch stays drawable — Inherited, so a hidden bake root still wins"
        );

        // Not a one-shot verdict: the sample is per frame, so a batch keyed back on comes back.
        app.world_mut()
            .entity_mut(shoulder)
            .insert(MatAnim::driving_tag(constant_alpha(1.0), 0.0, None));
        app.update();
        assert_eq!(
            vis(&app, shoulder),
            Visibility::Inherited,
            "the cull tracks the sample; it does not latch"
        );
    }

    /// **The booth twin of the equipped-item emitter's billboard frame** (decision 0813, carried to
    /// the select screen for `#bugs` B118). Same real numbers as
    /// `billboard::tests::an_item_emitters_billboard_frame_puts_it_behind_the_pivot`
    /// (`LShoulder_Mail_PVPAlliance_C_01`: billboard bone 1 pivot `(-0.012, 0.162, -0.060)`, sparkle
    /// emitter `(-0.252, 0.178, -0.046)`, raw WoW model space), but through THIS system — and here the
    /// frame is a **child**: the item's host sits under a posed body joint, so the law holds only if
    /// the parent's rotation is cancelled rather than inherited. That cancellation is the one thing
    /// that could silently be wrong on the booth path, so it is what the joint pose below is for.
    ///
    /// Asserted: the frame sits AT the pivot; the emitter's composed origin is 0.24 yd along the
    /// **booth** camera's view axis, away from the eye, with only centimetres off it; and it FOLLOWS
    /// that camera — the half a rest-pose placement cannot do.
    #[test]
    fn a_booth_billboard_frame_puts_an_item_emitter_behind_its_pivot() {
        use benilla_assets::coords::wow_to_bevy;
        const PIVOT: [f32; 3] = [-0.012, 0.162, -0.060];
        const EMITTER: [f32; 3] = [-0.252, 0.178, -0.046];
        // What `spawn_emitter`'s pivot rebase stores: the chain offset, raw WoW axes.
        let local = wow_to_bevy([
            EMITTER[0] - PIVOT[0],
            EMITTER[1] - PIVOT[1],
            EMITTER[2] - PIVOT[2],
        ]);

        let mut app = App::new();
        app.add_systems(Update, face_booth_billboards);
        let layer = RenderLayers::layer(9);
        // The booth camera, aimed off every world axis so nothing can pass by coincidence.
        let mut cam_tf = Transform::from_translation(Vec3::new(1.1, 1.6, 2.4))
            .looking_at(Vec3::new(0.0, 1.1, 0.0), Vec3::Y);
        let cam = app
            .world_mut()
            .spawn((
                crate::portrait::BoothCam("glue".to_string()),
                // `Camera` because the facer skips a sleeping camera (the booth park);
                // the default is active — the state a rendering booth is in.
                Camera::default(),
                GlobalTransform::from(cam_tf),
                layer.clone(),
            ))
            .id();
        // The item's host: the shoulder attach point under a joint holding a Stand-pose rotation.
        let host_tf = Transform {
            translation: Vec3::new(0.21, 1.42, 0.06),
            rotation: Quat::from_euler(EulerRot::YXZ, 0.7, -0.3, 0.2),
            scale: Vec3::ONE,
        };
        let host_gt = GlobalTransform::from(host_tf);
        let host = app.world_mut().spawn(host_gt).id();
        let frame = app
            .world_mut()
            .spawn((
                Transform::from_translation(wow_to_bevy(PIVOT)),
                layer.clone(),
                ChildOf(host),
                BoothBillboard::frame(BillboardKind::Spherical),
            ))
            .id();
        let pivot_world = host_gt.transform_point(wow_to_bevy(PIVOT));

        // The system writes the frame's LOCAL transform; composing it onto the host is what
        // propagation does in PostUpdate, before the particle sim reads the result.
        let sparkle_offset = |app: &App| {
            let local_tf = *app.world().entity(frame).get::<Transform>().unwrap();
            let world = host_gt.mul_transform(local_tf).compute_transform();
            assert!(
                (world.translation - pivot_world).length() < 1e-5,
                "the frame sits AT the billboard pivot — only the rotation is replaced"
            );
            world.transform_point(local) - pivot_world
        };

        app.update();
        let fwd = *cam_tf.forward();
        let sparkle = sparkle_offset(&app);
        assert!(
            (sparkle.dot(fwd) - 0.240).abs() < 2e-3,
            "0.24 yd along the BOOTH camera's view axis, away from the eye: {sparkle:?}"
        );
        assert!(
            (sparkle - fwd * sparkle.dot(fwd)).length() < 0.025,
            "…and all but ~2 cm of the offset is in that one axis: {sparkle:?}"
        );

        // Move the booth camera (the preview's yaw drag does exactly this relative to the model):
        // the offset must follow it. A rest-pose placement would return the same vector twice.
        cam_tf = Transform::from_translation(Vec3::new(-2.6, 1.0, 0.4))
            .looking_at(Vec3::new(0.0, 1.1, 0.0), Vec3::Y);
        app.world_mut()
            .entity_mut(cam)
            .insert(GlobalTransform::from(cam_tf));
        app.update();
        let turned = sparkle_offset(&app);
        let fwd2 = *cam_tf.forward();
        assert!(
            (turned.dot(fwd2) - 0.240).abs() < 2e-3
                && (turned - fwd2 * turned.dot(fwd2)).length() < 0.025,
            "the same 0.24 yd, now along the new view axis: {turned:?}"
        );
        assert!(
            (turned - sparkle).length() > 0.2,
            "…which means it MOVED — a rest-pose placement would not have"
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
                bevy::mesh::MeshTag(benilla_world::mesh_tag::spawn_tag(0, 1.0)),
                MatAnim::driving_tag(constant_alpha(0.1), 0.0, None),
            ))
            .id();
        app.update();
        assert_eq!(
            benilla_world::mesh_tag::alpha_of(
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
