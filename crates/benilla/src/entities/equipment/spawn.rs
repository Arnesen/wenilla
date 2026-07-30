//! Equipment **attach** (decisions 0072/0074, split out of `super`'s one file): the sub-model
//! children a unit's resolved [`HeldItems`] spawns — each item model's parts under its attach
//! point's joint entity, plus everything that rides an item (its billboard cards, emitters,
//! lights, ribbons, and the glow its `ItemVisuals` id names — decision 0805).

use bevy::mesh::MeshTag;
use bevy::prelude::*;

use crate::billboard::BillboardCard;
use crate::debug_panel::{ModelKind, ModelPart};
use crate::interior::part_interior_lit;
use crate::model_fade::{
    fade_alpha, join_unit_appear_fade, FadeMaterials, JoinedFade, PendingAppearFade, RenderFade,
    UnitAppearFade, APPEAR_FADE_SECS,
};

use super::super::spawn_carried_lights;
use super::{attach_id, BoneAttach, HeldAttached, HeldItems, ItemDisplays, NO_GLOW};

/// A held-item part's resolved appear-fade join state for this spawn, folding [`JoinedFade`] with
/// whether the part itself is fade-capable ([`super::EntityPart::fade_blend`]).
#[derive(Clone, Copy)]
enum PartFade {
    Steady,
    Pending(f32),
    Live(f32),
}

/// Spawn/refresh the held-item children for every unit whose [`HeldItems`] changed (or whose item
/// model finished loading): each slot's model parts spawn under the attach point's joint entity at
/// the attachment offset, so they ride the bone. Slots whose model is still loading are left pending
/// (the `applied` diff key keeps them un-applied) and picked up on a later pass. A part spawning while
/// the unit's own appear-fade is still in flight joins it ([`join_unit_appear_fade`]) instead of
/// popping in opaque (decision 0032 read as a per-unit property).
#[allow(clippy::type_complexity)]
pub(in crate::entities) fn attach_held_items(
    mut commands: Commands,
    mut units: Query<(
        &HeldItems,
        &BoneAttach,
        Option<&mut HeldAttached>,
        Entity,
        Option<&UnitAppearFade>,
        Option<&crate::interior::BodyBakeCenter>,
        // The unit root's own transform — its scale is the wire `NetEntity::scale` the streamer
        // writes (`entities::attach`), and the held effects' draw-order rung is measured in the
        // world yards that scale produces.
        Option<&Transform>,
    )>,
    held: Option<Res<ItemDisplays>>,
    time: Res<Time>,
) {
    let Some(held) = held else {
        return;
    };
    let now = time.elapsed_secs();
    for (items, bones, attached, entity, unit_fade, body_center, unit_tf) in &mut units {
        // A held item / helm / shoulder resolves and spawns asynchronously (a template round trip, a
        // model load) — often *after* the body has already armed its appear-fade (decision 0032 is a
        // per-unit property: the reference fades the whole unit, attachments included, as one). Read
        // the unit root's fade clock once per unit so every part spawned below joins the same ramp
        // instead of popping in opaque.
        let joined = join_unit_appear_fade(unit_fade.copied());
        // Diff against what's spawned; skip when unchanged. A slot whose model hasn't built parts yet
        // is masked out of `next` so it stays "not yet applied" and re-attaches once the parts exist.
        let mut next = items.clone();
        for slot in next.slots.iter_mut() {
            let ready = slot.is_some_and(|hs| {
                held.models
                    .get(&(hs.display, hs.kind))
                    .and_then(|dm| dm.parts.as_ref())
                    .is_some()
            });
            if !ready {
                *slot = None;
            }
        }
        if attached.as_ref().is_some_and(|a| a.applied == next) {
            continue;
        }
        // Despawn the previous children, then spawn the new set.
        let mut spawned = Vec::new();
        if let Some(a) = &attached {
            for e in &a.spawned {
                commands.entity(*e).despawn();
            }
        }
        for (slot_idx, hs) in next
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.as_ref().map(|hs| (i, hs)))
        {
            let Some(dm) = held.models.get(&(hs.display, hs.kind)) else {
                continue;
            };
            let Some(parts) = dm.parts.as_ref() else {
                continue;
            };
            let Some(&(bone, offset)) = bones.points.get(&hs.attach) else {
                // Body model has no such attach point (a non-character skeleton) — hold nothing.
                continue;
            };
            let Some(joint) = bones.anchor(bone) else {
                continue;
            };
            let root = commands
                .spawn((Transform::from_translation(offset), Visibility::default()))
                .id();
            commands.entity(joint).add_child(root);
            // The item/enchant glow (decision 0805): its instances hang off the ITEM's own
            // attachment points, so they are children of this root — spawned by
            // [`super::item_glow::attach_item_glows`] once the glow models build, and reaped with
            // the root on any gear/sheath change, which is the whole lifetime rule.
            if hs.visual != NO_GLOW {
                commands
                    .entity(root)
                    .insert(super::super::item_glow::ItemGlow {
                        display: hs.display,
                        kind: hs.kind,
                        visual: hs.visual,
                    });
            }
            // The engine-drawn bowstring (0408 §G2) — for the drawn BOW only: the ranged slot's
            // left-hand fork (a bow is the one ranged weapon placed in HAND_LEFT; the client
            // registers the string callback bow-only, from the ranged-draw path). The `$WTT`/
            // `$WTB` anchors alone are NOT the gate — they are generic weapon-TRAIL begin/end
            // markers (wow-re w2d2: `WTBT`/`WTTT`, the swing-trail vertex build) that melee
            // weapons author too; keying on their presence drew a phantom "bowstring" chord
            // across the Whirlwind Axe's blade tips (decision 0531).
            if slot_idx == 2 && hs.attach == attach_id::HAND_LEFT {
                if let Some([top, bottom]) = dm.string_anchors {
                    commands.entity(root).insert(crate::bowstring::Bowstring {
                        owner: entity,
                        top: top.1,
                        bottom: bottom.1,
                    });
                }
            }
            // Billboard batches (the torch's glow card) collected for the world-root card spawn
            // below — as plain children they'd render at the item root (the grip), not the
            // authored pivot (the torch head). Decision 0153.
            let mut billboard_parts = Vec::new();
            commands.entity(root).with_children(|parent| {
                for part in parts {
                    if let Some(info) = &part.billboard {
                        billboard_parts.push((info.clone(), part));
                        continue;
                    }
                    // Per-part join decision: `joined` (the unit's clock) combined with whether this
                    // part is fade-capable at all (`fade_blend` — WMO-display parts have none, though
                    // held items are always M2). Steady = spawn opaque now, exactly as before this fix.
                    let effective = match (joined, part.fade_blend.is_some()) {
                        (_, false) => PartFade::Steady,
                        (JoinedFade::Steady, true) => PartFade::Steady,
                        (JoinedFade::Pending { since }, true) => PartFade::Pending(since),
                        (JoinedFade::Live { started }, true) => PartFade::Live(started),
                    };
                    let (init_mat, tag_alpha) = match effective {
                        PartFade::Steady => (part.material.clone(), 1.0),
                        PartFade::Pending(_) => (part.fade_blend.clone().unwrap(), 0.0),
                        // Compute the *current* alpha (not 0) so a late joiner doesn't flash invisible
                        // for a frame before `apply_render_fade` catches up — it's already mid-ramp.
                        PartFade::Live(started) => (
                            part.fade_blend.clone().unwrap(),
                            fade_alpha(0.0, 1.0, (now - started) / APPEAR_FADE_SECS),
                        ),
                    };
                    let mut child = parent.spawn((
                        Mesh3d(part.mesh.clone()),
                        MeshMaterial3d(init_mat),
                        Transform::default(),
                        ModelPart {
                            kind: ModelKind::Creature,
                            blend: part.blend,
                        },
                        // The portrait booth mirrors this rider ([`crate::portrait`]): steady
                        // material (not the fade twin) + where it sits, so the bake can seat it at
                        // the bone's bind-pose global (the booth spawns no skeleton).
                        crate::portrait::PortraitRider {
                            static_mesh: part.mesh.clone(),
                            material: part.material.clone(),
                            bone,
                            offset,
                        },
                    ));
                    // Interior-light parity with the body: both material variants + a MeshTag, so the
                    // classifier relights the weapon inside a WMO room like it does its wielder. While
                    // joining the unit's appear-fade the tag carries its alpha instead — the classifier
                    // yields via its own `Without<RenderFade>`/`Without<PendingAppearFade>` filter, same
                    // as a body part.
                    // Anchored at the WEARER's root: an equipped item M2 aliases its wearer's
                    // light collector by pointer (`[item+0x3b8]=[wearer+0x3b8]`, `0x718960` —
                    // wow-re `unit-light-combine-storm.md`), so it never runs its own
                    // classify/footprint at the carried position. The animating hand joint
                    // once anchored these, and a swing alone could trip the resample gate and
                    // split the shield's light from the body's (director-caught, 2026-07-13).
                    // The fold reference is the wearer's BODY centre for the same reason.
                    if let Some(lit) = part_interior_lit(
                        &part.material,
                        part.material_interior.as_ref(),
                        part.material_interior_bake.as_ref(),
                        body_center.map_or(dm.bake_center_local, |c| c.0),
                        entity,
                    ) {
                        child.insert((MeshTag(crate::mesh_tag::alpha_bits(tag_alpha)), lit));
                    }
                    // `FadeMaterials` is persistent bookkeeping (self-avatar zoom fade, decision 0032's
                    // despawn-fade-out), not tied to whether *this* spawn happens to join an in-flight
                    // unit fade — attach it whenever the part is fade-capable at all, steady or not.
                    if let Some(blend) = &part.fade_blend {
                        child.insert(FadeMaterials {
                            cutout: part.material.clone(),
                            blend: blend.clone(),
                            bake_blend: part.material_interior_bake_blend.clone(),
                        });
                    }
                    match effective {
                        PartFade::Steady => {}
                        PartFade::Pending(since) => {
                            child.insert(PendingAppearFade { since });
                        }
                        PartFade::Live(started) => {
                            child.insert(RenderFade {
                                started,
                                duration: APPEAR_FADE_SECS,
                                from: 0.0,
                                to: 1.0,
                            });
                        }
                    }
                }
            });
            // The billboard cards (decision 0153): world-root entities FOLLOWING `root` — it sits
            // at the attach offset under the hand joint, is fresh per attach, and despawns on a
            // gear change, so the card's lifecycle and frame both come for free (same owner
            // contract as the item's emitters below).
            for (info, part) in billboard_parts {
                let mut card = commands.spawn((
                    Mesh3d(part.mesh.clone()),
                    MeshMaterial3d(part.material.clone()),
                    Transform::default(),
                    ModelPart {
                        kind: ModelKind::Creature,
                        blend: part.blend,
                    },
                    BillboardCard::following(&info, root),
                ));
                // Same interior-light membership the item's mesh parts get above, through the same
                // constructor and anchored at the same WEARER (decision 0778) — so a held torch's
                // glow card can never split from the arm holding it. Spawned at a steady alpha: a
                // card joins no appear-fade today (it carries neither `RenderFade` nor
                // `FadeMaterials`), and the classifier preserves the alpha field regardless.
                if let Some(lit) = part_interior_lit(
                    &part.material,
                    part.material_interior.as_ref(),
                    part.material_interior_bake.as_ref(),
                    body_center.map_or(dm.bake_center_local, |c| c.0),
                    entity,
                ) {
                    card.insert((MeshTag(crate::mesh_tag::alpha_bits(1.0)), lit));
                }
            }
            // The item's own particle emitters — the held torch's flame (0130 phase 4: the same
            // owner-follow rider as doodad emitters). `root` sits at the attach offset under the
            // hand joint and its frame IS the item's model frame; a held item spawns no skeleton,
            // so the rest pose applies and no pivot rebase is needed — the flame burns at its
            // authored spot (the torch tip) and follows the hand through the swing. Free entities:
            // they self-despawn with `root` via the owner contract (gear change, unit despawn).
            // The spawn transform does two jobs, and neither is placing the emitter (the owner
            // overwrites the position every frame): its TRANSLATION seeds the flicker RNG —
            // root's entity bits de-sync two torch-bearers standing side by side — and its SCALE
            // is the wearer's, which is what the effects' draw-order rung is measured in
            // (`particles::owner_last_bias`). An item on a twice-size wielder reaches twice as
            // far from its own origin, so its rung has to grow with it; reading the scale off a
            // transform built as a bare RNG seed silently pinned every held effect at 1×.
            let unit_scale = unit_tf.map_or(1.0, |t| t.scale.max_element());
            let spawn_tf = Transform::from_translation(Vec3::splat(root.to_bits() as f32))
                .with_scale(Vec3::splat(unit_scale));
            for em in &dm.emitters {
                // …with ONE exception to "the rest pose applies": a **billboard** bone in the
                // emitter's chain. Its palette rows are replaced with the camera basis about its
                // own pivot every frame and children multiply onto that, so the reference's
                // emitter origin is `pivot + camBasis·(position − pivot)` — camera-dependent, and
                // up to two chain-offsets away from where the rest pose puts it (decision 0813).
                // The rig lane gets this from its joint palette; an item model has no rig, so the
                // frame is realized as a mesh-less billboard card the emitter OWNS-follows
                // (`BillboardCard::frame_following`). Nothing else in the chain is live: of the
                // 95 item models whose emitters ride a billboard bone, none animates its chain.
                let owner = match em.billboard {
                    Some((kind, pivot)) => {
                        let frame = commands
                            .spawn(BillboardCard::frame_following(
                                kind,
                                benilla_assets::coords::wow_to_bevy(pivot),
                                root,
                            ))
                            .id();
                        let d = (0..3)
                            .map(|c| (em.def.position[c] - pivot[c]).powi(2))
                            .sum::<f32>()
                            .sqrt();
                        debug!(
                            "item fx: display {} bone {} rides a {kind:?} billboard frame \
                             (pivot {pivot:?}, chain offset {d:.3} yd)",
                            hs.display, em.def.bone
                        );
                        (frame, pivot)
                    }
                    None => (root, [0.0; 3]),
                };
                crate::particles::spawn_emitter(
                    &mut commands,
                    em,
                    spawn_tf,
                    Some(owner),
                    Some(root), // a held item is an attached model — the flame fans with the swing
                    Some(root), // the cloud anchors at the MODEL; the bone composes births only
                    // A held item spawns no rig; its emitters run the item model's own slot-0
                    // loop on the spawn clock (the torch burns always — the doodad law).
                    crate::particles::EmitClock::Pinned,
                );
            }
            // The item's own M2 point light — **the held torch's glow** (decision 0016's law on the
            // entity half of the scene; see `super::carried_light`). `Club_1H_Torch_A_01.m2` — the
            // torch every torch-bearing NPC carries — authors exactly one: a warm
            // `(0.467, 0.290, 0.133) × 3.0` point light 0.58 yd up the shaft. It rides `root` like
            // the emitters and for the same reason (the item poses at rest, so its model space IS
            // the bone-local frame), which walks it through the hand's swing; the fence rails and
            // grass around the bearer then gather it like any other scene point light.
            spawn_carried_lights(&mut commands, &dm.lights, root, |_| None);
            // The item's ribbon trails (weapon enchant streaks): ride the item root — a held item
            // poses at rest, so the bone-local origin is model-space (no pivot rebase). A held item
            // rests in Stand (anim 0): a thrown weapon's trail is keyed dark there, so the flight
            // ribbon never shows in the hand — it lights only on the InFlight missile.
            for rb in &dm.ribbons {
                crate::ribbons::spawn_ribbon(&mut commands, rb, root, false, unit_scale, Some(0));
            }
            debug!(
                "held attach: unit {entity} display {} → attach {} (bone {bone}, {} parts)",
                hs.display,
                hs.attach,
                parts.len()
            );
            spawned.push(root);
        }
        let applied = HeldAttached {
            applied: next,
            spawned,
        };
        match attached {
            Some(mut a) => *a = applied,
            None => {
                commands.entity(entity).insert(applied);
            }
        }
    }
}
