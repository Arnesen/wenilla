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

use super::super::{item_glow::ItemGlow, spawn_carried_lights};
use super::{
    attach_id, BoneAttach, HeldAttached, HeldItems, HeldSlot, ItemDisplays, ATTACH_SLOTS, NO_GLOW,
};

/// A held-item part's resolved appear-fade join state for this spawn, folding [`JoinedFade`] with
/// whether the part itself is fade-capable ([`super::EntityPart::fade_blend`]).
#[derive(Clone, Copy)]
enum PartFade {
    Steady,
    Pending(f32),
    Live(f32),
}

/// Everything one slot's spawn needs from its WEARER, read once per unit — the context
/// [`spawn_slot`] carries so the per-slot body isn't a fifteen-argument call.
struct WearerCtx<'a> {
    /// The unit wearing the item — the parent whose tint, light collector and fade it inherits.
    wearer: Entity,
    bones: &'a BoneAttach,
    /// The unit's appear-fade clock, for a part spawning mid-ramp ([`join_unit_appear_fade`]).
    joined: JoinedFade,
    now: f32,
    /// The wearer's rig-palette slot, pre-shifted into `MeshTag` bits (decision 0812).
    rig_tag: u32,
    /// The wearer's body bake centre, when it has one — the interior classifier's fold reference.
    body_center: Option<Vec3>,
    /// The wire scale the unit renders at: the world yards its held effects' draw-order rung is
    /// measured in ([`crate::particles::owner_last_bias`]).
    scale: f32,
}

/// The **re-seat writers** (decision 0826): everything under an item root that caches *where on the
/// body* the item sits. A pure attach-point change — the sheath swap — MOVES the root instead of
/// rebuilding it, and these move by the same delta, so the item's glow instances, its effect hosts
/// and its live particle clouds all ride along instead of being orphaned and respawned.
#[derive(bevy::ecs::system::SystemParam)]
pub(in crate::entities) struct SeatWriters<'w, 's> {
    children: Query<'w, 's, &'static Children>,
    riders: Query<'w, 's, &'static mut crate::portrait::PortraitRider>,
    cards: Query<'w, 's, &'static mut crate::portrait::PortraitBillboard>,
    effects: Query<'w, 's, &'static mut crate::portrait::PortraitEffects>,
    glows: Query<'w, 's, &'static mut ItemGlow>,
}

impl SeatWriters<'_, '_> {
    /// Move every cached seat in `root`'s subtree to the item's new attach point: the body `bone`
    /// it now hangs from, and `delta` = `new_offset − old_offset` applied to each cached offset.
    ///
    /// The delta (rather than an assignment) is what makes this total: a mirror's offset is the
    /// attach point *plus* something model-local — a card's own pivot, a glow slot's point on the
    /// item — and only the attach-point term moves. The walk is recursive because an item's glow
    /// instances hang two levels down.
    fn reseat(&mut self, root: Entity, bone: u16, delta: Vec3) {
        let mut stack = vec![root];
        while let Some(e) = stack.pop() {
            if let Ok(mut r) = self.riders.get_mut(e) {
                r.bone = bone;
                r.offset += delta;
            }
            if let Ok(mut c) = self.cards.get_mut(e) {
                c.bone = bone;
                c.seat = match c.seat {
                    crate::portrait::PortraitSeat::Body => crate::portrait::PortraitSeat::Body,
                    crate::portrait::PortraitSeat::Rider(at) => {
                        crate::portrait::PortraitSeat::Rider(at + delta)
                    }
                };
            }
            if let Ok(mut f) = self.effects.get_mut(e) {
                f.bone = bone;
                f.offset += delta;
            }
            if let Ok(mut g) = self.glows.get_mut(e) {
                g.bone = bone;
                g.offset += delta;
            }
            if let Ok(kids) = self.children.get(e) {
                stack.extend(kids.iter());
            }
        }
    }
}

/// Spawn/refresh the held-item children for every unit whose [`HeldItems`] changed (or whose item
/// model finished loading): each slot's model parts spawn under the attach point's joint entity at
/// the attachment offset, so they ride the bone. Slots whose model is still loading are left pending
/// (the `applied` diff key keeps them un-applied) and picked up on a later pass. A part spawning while
/// the unit's own appear-fade is still in flight joins it ([`join_unit_appear_fade`]) instead of
/// popping in opaque (decision 0032 read as a per-unit property).
///
/// **The diff is per SLOT, and an attach-point change is a MOVE** (decision 0826). The reference's
/// sheath paths touch only the weapon/quiver attach ids (`0x611770`, wow-re `sheath-policy.md` /
/// `ranged-sheath-display.md`) and stow a melee weapon by detaching the sub-model and **re-parenting
/// it** at the sheath point (`0x60b590` → `0x712f70`) — the model instance, and everything riding
/// it, survives the swap. Rebuilding a unit's whole kit on any change did neither: drawing a sword
/// respawned the shoulders' and helm's emitters too, and every orphaned pool then lived out its
/// lifespan FROZEN in world space (`particles::sim`'s drain) — the sparkle cloud that hung behind
/// the character on every weapon draw and every step of the login gear cascade.
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
        // The WEARER's rig, for its instance slot: every part spawned below carries it in its tag so
        // the wearer's body tint reaches its helm, shoulders and held items (decision 0812 — the
        // reference's attached models inherit the parent CM2's computed colours, `0x714000`). Never
        // used to skin these parts: they are static meshes, and the vertex stage's slot read is gated
        // on the mesh's own joint attributes.
        Option<&crate::rig_palette::RigSkin>,
    )>,
    held: Option<Res<ItemDisplays>>,
    time: Res<Time>,
    mut seats: SeatWriters,
) {
    let Some(held) = held else {
        return;
    };
    let now = time.elapsed_secs();
    for (items, bones, attached, entity, unit_fade, body_center, unit_tf, skin) in &mut units {
        // A held item / helm / shoulder resolves and spawns asynchronously (a template round trip, a
        // model load) — often *after* the body has already armed its appear-fade (decision 0032 is a
        // per-unit property: the reference fades the whole unit, attachments included, as one). Read
        // the unit root's fade clock once per unit so every part spawned below joins the same ramp
        // instead of popping in opaque.
        let ctx = WearerCtx {
            wearer: entity,
            bones,
            joined: join_unit_appear_fade(unit_fade.copied()),
            now,
            rig_tag: crate::mesh_tag::rig_bits(skin.map_or(0, |rb| rb.slot)),
            body_center: body_center.map(|c| c.0),
            scale: unit_tf.map_or(1.0, |t| t.scale.max_element()),
        };
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
        let (applied, mut roots) = attached.as_ref().map_or_else(
            || (HeldItems::default(), [None; ATTACH_SLOTS]),
            |a| (a.applied.clone(), a.spawned),
        );
        for (slot_idx, (root, (was, wants))) in roots
            .iter_mut()
            .zip(
                applied
                    .slots
                    .iter()
                    .copied()
                    .zip(next.slots.iter().copied()),
            )
            .enumerate()
        {
            if was == wants {
                continue;
            }
            // The MOVE: same item, new attach point (the sheath swap). Everything riding the root
            // — meshes, cards, glow instances, lights, ribbons and the live particle pools that
            // owner-follow it — comes along, and the cached booth seats shift with it.
            if let (Some(w), Some(n), Some(root)) = (was, wants, *root) {
                if w.same_item(&n) {
                    if let Some((&(bone, offset), &(_, old))) = ctx
                        .bones
                        .points
                        .get(&n.attach)
                        .zip(ctx.bones.points.get(&w.attach))
                    {
                        if let Some(joint) = ctx.bones.anchor(bone) {
                            commands.entity(joint).add_child(root);
                            commands
                                .entity(root)
                                .insert(Transform::from_translation(offset));
                            seats.reseat(root, bone, offset - old);
                            debug!(
                                "held move: unit {entity} display {} → attach {} (bone {bone})",
                                n.display, n.attach
                            );
                            continue;
                        }
                    }
                }
            }
            // Otherwise this slot really is a different item (or none): tear the old one down —
            // model gone, effects gone with it — and build the new one.
            if let Some(old) = root.take() {
                commands.entity(old).despawn();
            }
            *root = wants.and_then(|hs| spawn_slot(&mut commands, &ctx, &held, slot_idx, &hs));
        }
        let applied = HeldAttached {
            applied: next,
            spawned: roots,
        };
        match attached {
            Some(mut a) => *a = applied,
            None => {
                commands.entity(entity).insert(applied);
            }
        }
    }
}

/// Spawn one slot's item model under its attach point, with everything that rides it: the mesh
/// parts, the camera-facing cards, the booth mirrors, the item glow, the emitters, the lights and
/// the ribbons. `None` when the display has no built parts or the body has no such attach point.
fn spawn_slot(
    commands: &mut Commands,
    ctx: &WearerCtx,
    held: &ItemDisplays,
    slot_idx: usize,
    hs: &HeldSlot,
) -> Option<Entity> {
    let entity = ctx.wearer;
    let (bones, joined, now, rig_tag) = (ctx.bones, ctx.joined, ctx.now, ctx.rig_tag);
    let dm = held.models.get(&(hs.display, hs.kind))?;
    let parts = dm.parts.as_ref()?;
    // Body model has no such attach point (a non-character skeleton) — hold nothing.
    let &(bone, offset) = bones.points.get(&hs.attach)?;
    let joint = bones.anchor(bone)?;
    let root = commands
        .spawn((
            Transform::from_translation(offset),
            Visibility::default(),
            // This item model is CHAINED to the body wearing it (`0x712f70` attach → the
            // `[model+0x1cc]` parent link): the wearer's computed render alpha multiplies
            // everything this root carries, and everything chained below it in turn — its glow
            // instances included (decision 0833).
            crate::model_fade::ParentModel(entity),
        ))
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
                // The item's seat on the BODY, carried so the glow attach can publish its own
                // booth mirrors at a seat composed from it (decision 0822) — the glow spawns
                // asynchronously and knows only this root.
                bone,
                offset,
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
            //
            // The tag is unconditional (it used to ride the classifier's `Some`): it carries
            // the WEARER's instance slot, which is what puts a tinted body's colour on its
            // helm and shoulders — the director's report on the dwarf Stoneform tint. A part
            // with no interior variant got no tag at all before, so it was also invisible to
            // the ground-shade ramp that darkens its wielder; both now follow the body, which
            // is the same light-collector aliasing the comment above describes.
            child.insert(MeshTag(rig_tag | crate::mesh_tag::alpha_bits(tag_alpha)));
            // The item part's build-time bound (decision 0834): `calculate_bounds` can no longer
            // derive one from the `RENDER_WORLD`-only static form's data.
            if let Some(aabb) = part.aabb {
                child.insert(aabb);
            }
            if let Some(lit) = part_interior_lit(
                &part.material,
                part.material_interior.as_ref(),
                part.material_interior_bake.as_ref(),
                ctx.body_center.unwrap_or(dm.bake_center_local),
                entity,
            ) {
                child.insert(lit);
            }
            // `FadeMaterials` is persistent bookkeeping (self-avatar zoom fade, decision 0032's
            // despawn-fade-out), not tied to whether *this* spawn happens to join an in-flight
            // unit fade — attach it whenever the part is fade-capable at all, steady or not.
            if let Some(blend) = &part.fade_blend {
                child.insert(FadeMaterials {
                    cutout: part.material.clone(),
                    blend: blend.clone(),
                    bake_blend: part.material_interior_bake_blend.clone(),
                    zfill: part.zfill.clone(),
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
        // …and, under `root`, the booth **mirror carrier** for that card (decision 0822).
        // The card itself is a world-ROOT entity, so the portrait / paper-doll booths — which
        // mirror the unit's dressed descendants — cannot see it; without this marker an
        // item's camera-facing batch (a wand's gem, this torch's `GLOWWHITE32` halo) simply
        // did not exist in those panes. Its seat is the attach point **plus the batch's own
        // model-local pivot**: an item model spawns no rig, so nothing else bakes that pivot.
        commands.entity(root).with_child((
            Transform::default(),
            Visibility::default(),
            crate::portrait::PortraitBillboard {
                mesh: part.mesh.clone(),
                material: part.material.clone(),
                bone,
                seat: crate::portrait::PortraitSeat::Rider(offset + info.pivot),
                kind: info.kind,
            },
        ));
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
        // The card's build-time bound (decision 0834) — same rule as its mesh siblings above.
        if let Some(aabb) = part.aabb {
            card.insert(aabb);
        }
        // Same interior-light membership the item's mesh parts get above, through the same
        // constructor and anchored at the same WEARER (decision 0778) — so a held torch's
        // glow card can never split from the arm holding it. Spawned at a steady alpha: a
        // card joins no appear-fade today (it carries neither `RenderFade` nor
        // `FadeMaterials`), and the classifier preserves the alpha field regardless.
        // …and the wearer's instance slot, like its mesh siblings: a tinted body colours the
        // torch's glow card too (decision 0812).
        card.insert(MeshTag(rig_tag | crate::mesh_tag::alpha_bits(1.0)));
        if let Some(lit) = part_interior_lit(
            &part.material,
            part.material_interior.as_ref(),
            part.material_interior_bake.as_ref(),
            ctx.body_center.unwrap_or(dm.bake_center_local),
            entity,
        ) {
            card.insert(lit);
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
    let spawn_tf = Transform::from_translation(Vec3::splat(root.to_bits() as f32))
        .with_scale(Vec3::splat(ctx.scale));
    // The booth mirror for those same emitters (decision 0822): they spawn as FREE entities
    // below (the owner contract), never unit descendants, so a booth that mirrors the dressed
    // tree cannot see them — which is why the R14 pauldron's sparkle was absent from the paper
    // doll exactly as it was from the select screen (`#bugs` B118). One marker per item, on
    // the root that already reaps with it.
    if !dm.emitters.is_empty() {
        commands
            .entity(root)
            .insert(crate::portrait::PortraitEffects {
                bone,
                offset,
                emitters: dm.emitters.clone(),
            });
    }
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
            commands,
            em,
            spawn_tf,
            crate::particles::EmitterFrames {
                owner: Some(owner),
                // A held item is an attached model — the flame fans with the swing.
                attach: Some(root),
                // The cloud anchors at the MODEL; the bone composes births only.
                anchor: Some(root),
                // The item model is destroyed when the item is replaced or unequipped, and the
                // reference frees a model's emitters at its dtor — so no cloud is left hanging in
                // the air behind the character (decision 0826). A sheath swap no longer comes
                // through here at all: the root is MOVED, and this pool rides it.
                on_owner_loss: crate::particles::OwnerLoss::Free,
                // This emitter's own model instance is the item root; `ParentModel` above chains
                // it to the wearer, and the chain is what an ATTACHED model's composed alpha is
                // (`0x714000`) — so the sparkle on a pauldron fades in with the body wearing it
                // and vanishes with the avatar in first person (0827/0833).
                alpha: Some(root),
            },
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
    spawn_carried_lights(commands, &dm.lights, root, |_| None);
    // The item's ribbon trails (weapon enchant streaks): ride the item root — a held item
    // poses at rest, so the bone-local origin is model-space (no pivot rebase). A held item
    // rests in Stand (anim 0): a thrown weapon's trail is keyed dark there, so the flight
    // ribbon never shows in the hand — it lights only on the InFlight missile.
    for rb in &dm.ribbons {
        crate::ribbons::spawn_ribbon(
            commands,
            rb,
            root,
            false,
            ctx.scale,
            Some(0),
            // Its own model instance — chained to the wearer above — so an enchant streamer is
            // gone with the avatar in first person and absent until the body is shown (0827/0833).
            Some(root),
        );
    }
    debug!(
        "held attach: unit {entity} display {} → attach {} (bone {bone}, {} parts)",
        hs.display,
        hs.attach,
        parts.len()
    );
    Some(root)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::super::display::{empty_display, EntityPart};
    use super::super::{HeldSlot, ItemModelKind};
    use super::*;

    /// One synthetic item part. `interior` picks whether it carries an interior material variant —
    /// the axis that used to decide whether the part got a `MeshTag` at all, and so whether it could
    /// ever be tinted or ground-shaded.
    fn part(interior: bool) -> EntityPart {
        EntityPart {
            mesh: Handle::default(),
            aabb: None,
            skinned_mesh: None,
            material: Handle::default(),
            material_interior: interior.then(Handle::default),
            material_interior_bake: None,
            material_interior_bake_blend: None,
            fade_blend: None,
            zfill: None,
            blend: benilla_formats::ModelBlend::Opaque,
            additive: false,
            two_sided: false,
            geoset_id: 0,
            char_slot: None,
            billboard: None,
            alpha_anim: None,
            rgb_anim: None,
            ground_quad: None,
        }
    }

    /// Spawn a wearer with one helm slot and run the attach. Returns the spawned parts' tags plus the
    /// wearer's own instance slot (`0` when `rigged` is false — a boneless wearer).
    fn attach_a_helm(rigged: bool, interior: bool) -> (Vec<u32>, u16) {
        const HELM_KIND: ItemModelKind = ItemModelKind::Helm { race: 3, sex: 0 };
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        // `RigSkin`'s free hook frees the slot through this on teardown.
        app.init_resource::<crate::rig_palette::RigPalettes>();
        let mut displays = ItemDisplays::icons_for_tests(
            benilla_formats::ItemDisplayCatalog::from_displays(HashMap::new()),
        );
        let mut dm = empty_display();
        dm.parts = Some(vec![part(interior)]);
        displays.models.insert((7, HELM_KIND), dm);
        app.insert_resource(displays);

        // The bone the helm hangs on, and the wearer.
        let joint = app.world_mut().spawn(Transform::default()).id();
        let bones = BoneAttach {
            anchors: HashMap::from([(3u16, joint)]),
            points: HashMap::from([(attach_id::HELM, (3u16, Vec3::ZERO))]),
            markers: HashMap::new(),
        };
        let mut items = HeldItems::default();
        items.slots[3] = Some(HeldSlot {
            display: 7,
            kind: HELM_KIND,
            attach: attach_id::HELM,
            visual: NO_GLOW,
        });
        let skin = rigged.then(|| {
            crate::rig_palette::RigSkin::allocate_bones(
                app.world_mut()
                    .resource_mut::<crate::rig_palette::RigPalettes>()
                    .as_mut(),
                8,
                Handle::default(),
            )
            .expect("a fresh palette has room")
        });
        let slot = skin.as_ref().map_or(0, |s| s.slot);
        let mut wearer = app.world_mut().spawn((items, bones, Transform::default()));
        if let Some(skin) = skin {
            wearer.insert(skin);
        }
        app.add_systems(Update, attach_held_items);
        app.update();

        let mut tags: Vec<u32> = app
            .world_mut()
            .query::<&MeshTag>()
            .iter(app.world())
            .map(|t| t.0)
            .collect();
        tags.sort_unstable();
        (tags, slot)
    }

    /// **The director's report (dwarf Stoneform tinted the body but not the helm/shoulders).** An
    /// attachment carries its WEARER's instance slot, so the body tint reaches it — decision 0812's
    /// named gap, closed. The reference's own rule: an attached model inherits the parent CM2's
    /// computed colours (`0x714000`).
    #[test]
    fn an_attachment_wears_its_wearers_instance_slot() {
        for interior in [true, false] {
            let (tags, slot) = attach_a_helm(true, interior);
            assert!(slot >= 1, "the wearer really has a rig");
            assert_eq!(tags.len(), 1, "one part, one tag (interior={interior})");
            assert_eq!(
                crate::mesh_tag::rig_of(tags[0]),
                slot,
                "the wearer's slot, interior={interior}",
            );
        }
    }

    /// The tag is unconditional now. It used to ride the interior classifier's `Some`, so a part with
    /// no interior variant carried none at all — invisible to the tint AND to the ground-shade ramp
    /// that darkens its wielder. Both halves of that are asserted: a tag exists, and it is opaque.
    #[test]
    fn a_part_without_an_interior_variant_still_gets_a_tag() {
        let (tags, _) = attach_a_helm(true, false);
        assert_eq!(tags.len(), 1);
        assert_ne!(tags[0], 0, "not the untagged ⇒ opaque sentinel");
        assert!((crate::mesh_tag::alpha_of(tags[0]) - 1.0).abs() < 1.0 / 63.0);
    }

    /// A wearer with no rig at all (a boneless model holding something) lands on the identity slot 0
    /// rather than borrowing someone else's colour.
    #[test]
    fn a_rigless_wearer_leaves_the_slot_at_identity() {
        let (tags, _) = attach_a_helm(false, true);
        assert_eq!(tags.len(), 1);
        assert_eq!(crate::mesh_tag::rig_of(tags[0]), 0);
    }

    /// **What a booth can see of an equipped item** (decision 0822, `#bugs` B118's paper-doll half).
    /// An item model's camera-facing batch spawns as a world-ROOT card and its emitters as free
    /// owner-followed entities — neither is a unit descendant, so the portrait / paper-doll booths,
    /// which mirror the dressed tree, could not see either one and a worn item's effects were absent
    /// from every pane. The attach must therefore publish a mirror for both, at the seat the booth
    /// needs: the attach point **plus the batch's own model-local pivot** for a card (an item spawns no
    /// rig, so nothing else bakes that pivot), the bare attach point for the effect host.
    ///
    /// Both offsets are asserted against a nonzero attach point AND a nonzero pivot, so publishing
    /// either one alone — or adding them in the wrong frame — fails.
    #[test]
    fn an_equipped_items_card_and_emitters_are_published_for_the_booths() {
        const SHOULDER_KIND: ItemModelKind = ItemModelKind::ShoulderRight;
        const ATTACH: Vec3 = Vec3::new(0.21, 1.42, 0.06);
        const PIVOT: Vec3 = Vec3::new(-0.06, 0.162, -0.012);
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<crate::rig_palette::RigPalettes>();
        let mut displays = ItemDisplays::icons_for_tests(
            benilla_formats::ItemDisplayCatalog::from_displays(HashMap::new()),
        );
        let mut dm = empty_display();
        // The R14 pauldron's shape: a plain mesh batch, a camera-facing batch, and emitters.
        let mut card = part(false);
        card.billboard = Some(benilla_assets::BillboardInfo {
            pivot: PIVOT,
            bone: 1,
            kind: benilla_formats::BillboardKind::Spherical,
            scale_anim: None,
            seq_translations: Vec::new(),
        });
        dm.parts = Some(vec![part(false), card]);
        dm.emitters = vec![benilla_assets::ModelEmitter {
            def: crate::particles::tests::plain_def(),
            texture: None,
            bone_pivot: [0.0; 3],
            billboard: Some((benilla_formats::BillboardKind::Spherical, [0.0; 3])),
            recursion: None,
            geometry: None,
            owner_reach: 0.0,
        }];
        displays.models.insert((7, SHOULDER_KIND), dm);
        app.insert_resource(displays);

        let joint = app.world_mut().spawn(Transform::default()).id();
        let bones = BoneAttach {
            anchors: HashMap::from([(3u16, joint)]),
            points: HashMap::from([(attach_id::SHOULDER_RIGHT, (3u16, ATTACH))]),
            markers: HashMap::new(),
        };
        let mut items = HeldItems::default();
        items.slots[4] = Some(HeldSlot {
            display: 7,
            kind: SHOULDER_KIND,
            attach: attach_id::SHOULDER_RIGHT,
            visual: NO_GLOW,
        });
        app.world_mut().spawn((items, bones, Transform::default()));
        app.add_systems(Update, attach_held_items);
        app.update();

        let mut cards = app
            .world_mut()
            .query::<&crate::portrait::PortraitBillboard>();
        let published: Vec<_> = cards.iter(app.world()).collect();
        assert_eq!(published.len(), 1, "one camera-facing batch, one mirror");
        assert_eq!(published[0].bone, 3, "the BODY bone, not the item's bone 1");
        assert_eq!(
            published[0].seat,
            crate::portrait::PortraitSeat::Rider(ATTACH + PIVOT),
            "a rig-less rider's card carries attach + its own pivot",
        );

        let mut fx = app.world_mut().query::<&crate::portrait::PortraitEffects>();
        let published: Vec<_> = fx.iter(app.world()).collect();
        assert_eq!(published.len(), 1, "one effect-bearing model, one mirror");
        assert_eq!(published[0].bone, 3);
        assert_eq!(
            published[0].offset, ATTACH,
            "the host seats at the attach point; each emitter's own pivot is applied inside",
        );
        assert_eq!(published[0].emitters.len(), 1);
        assert!(
            published[0].emitters[0].billboard.is_some(),
            "the billboard-chain arm survives the carry — it is what the booth builds a frame from",
        );
    }

    /// A wearer holding a **weapon** (mainhand, drawn in the hand) and wearing a **shoulder** that
    /// carries emitters — the bench for the sheath-swap tests below. Returns the app and the wearer.
    fn dress_a_wearer() -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<crate::rig_palette::RigPalettes>();
        let mut displays = ItemDisplays::icons_for_tests(
            benilla_formats::ItemDisplayCatalog::from_displays(HashMap::new()),
        );
        for (display, kind, effects) in [
            (7, ItemModelKind::Weapon, false),
            (9, ItemModelKind::Weapon, false),
            (8, ItemModelKind::ShoulderRight, true),
        ] {
            let mut dm = empty_display();
            dm.parts = Some(vec![part(false)]);
            if effects {
                dm.emitters = vec![benilla_assets::ModelEmitter {
                    def: crate::particles::tests::plain_def(),
                    texture: None,
                    bone_pivot: [0.0; 3],
                    billboard: None,
                    recursion: None,
                    geometry: None,
                    owner_reach: 0.0,
                }];
            }
            displays.models.insert((display, kind), dm);
        }
        app.insert_resource(displays);

        // Three attach points on three bones: the hand, the back (where a stowed weapon rides) and
        // the shoulder.
        let (hand, back, shoulder) = (
            app.world_mut().spawn(Transform::default()).id(),
            app.world_mut().spawn(Transform::default()).id(),
            app.world_mut().spawn(Transform::default()).id(),
        );
        let bones = BoneAttach {
            anchors: HashMap::from([(1u16, hand), (2u16, back), (3u16, shoulder)]),
            points: HashMap::from([
                (attach_id::HAND_RIGHT, (1u16, HAND_AT)),
                (SHEATH_BACK, (2u16, BACK_AT)),
                (attach_id::SHOULDER_RIGHT, (3u16, SHOULDER_AT)),
            ]),
            markers: HashMap::new(),
        };
        let mut items = HeldItems::default();
        items.slots[0] = Some(HeldSlot {
            display: 7,
            kind: ItemModelKind::Weapon,
            attach: attach_id::HAND_RIGHT,
            visual: NO_GLOW,
        });
        items.slots[4] = Some(HeldSlot {
            display: 8,
            kind: ItemModelKind::ShoulderRight,
            attach: attach_id::SHOULDER_RIGHT,
            visual: NO_GLOW,
        });
        let wearer = app
            .world_mut()
            .spawn((items, bones, Transform::default()))
            .id();
        app.add_systems(Update, attach_held_items);
        app.update();
        (app, wearer)
    }

    const HAND_AT: Vec3 = Vec3::new(0.1, 1.0, 0.0);
    const BACK_AT: Vec3 = Vec3::new(-0.2, 1.3, -0.15);
    const SHOULDER_AT: Vec3 = Vec3::new(0.21, 1.42, 0.06);
    /// A back sheath point (`attach_id`'s `BACK_SHEATH` family — any id the body publishes).
    const SHEATH_BACK: u16 = 6;

    /// The roots this wearer currently has attached, per slot.
    fn roots_of(app: &App, wearer: Entity) -> [Option<Entity>; ATTACH_SLOTS] {
        app.world()
            .entity(wearer)
            .get::<HeldAttached>()
            .unwrap()
            .spawned
    }

    /// **The director's report, at its cause** (decision 0826): drawing/stowing a weapon changed one
    /// slot's attach point, and the old code rebuilt the unit's WHOLE kit — so the shoulders' and
    /// helm's emitters were orphaned mid-swing and their live particles hung in world space while
    /// the character walked on ("armor and weapon particles … lag behind when doing a weapon draw").
    ///
    /// Three things are asserted, and each one alone would reproduce the bug if it regressed: the
    /// untouched slot keeps its root entity (so its pool is never orphaned), the moved slot keeps
    /// ITS root (so the weapon's own effects ride the swap, as the reference's re-parent does), and
    /// the moved root really is re-seated — new parent joint, new local offset.
    #[test]
    fn a_sheath_swap_moves_the_weapon_and_leaves_the_other_slots_alone() {
        let (mut app, wearer) = dress_a_wearer();
        let before = roots_of(&app, wearer);
        let (weapon, shoulder_root) = (before[0].expect("weapon"), before[4].expect("shoulder"));

        // Stow it: the same item, a new attach point — nothing else about the kit changes.
        let mut items = app.world_mut().entity_mut(wearer);
        let mut items = items.get_mut::<HeldItems>().unwrap();
        items.slots[0].as_mut().unwrap().attach = SHEATH_BACK;
        app.update();

        let after = roots_of(&app, wearer);
        assert_eq!(
            after[0],
            Some(weapon),
            "the weapon MOVED — same model instance"
        );
        assert_eq!(
            after[4],
            Some(shoulder_root),
            "an untouched slot is not rebuilt by someone else's sheath swap"
        );
        assert!(
            app.world().get_entity(shoulder_root).is_ok(),
            "…and its root really is alive: every effect riding it survives the swap"
        );
        assert_eq!(
            app.world()
                .entity(weapon)
                .get::<ChildOf>()
                .map(|c| c.parent()),
            app.world()
                .entity(wearer)
                .get::<BoneAttach>()
                .unwrap()
                .anchor(2),
            "re-parented onto the sheath point's joint"
        );
        assert_eq!(
            app.world()
                .entity(weapon)
                .get::<Transform>()
                .unwrap()
                .translation,
            BACK_AT,
            "…at the new attach point's offset"
        );
        // The booth mirrors move with it — a paper doll must show the stowed weapon on the back.
        let seat = app
            .world_mut()
            .query::<&crate::portrait::PortraitRider>()
            .iter(app.world())
            .find(|r| r.offset.distance(BACK_AT) < 1e-5)
            .map(|r| r.bone);
        assert_eq!(seat, Some(2), "the rider's cached seat followed the move");
    }

    /// The other half of the per-slot diff: a slot whose item genuinely CHANGED is rebuilt (its old
    /// model is destroyed — the reference's dtor, which takes that model's emitters with it), and
    /// still nobody else's slot is touched.
    #[test]
    fn a_real_item_change_rebuilds_only_its_own_slot() {
        let (mut app, wearer) = dress_a_wearer();
        let before = roots_of(&app, wearer);
        let (weapon, shoulder_root) = (before[0].expect("weapon"), before[4].expect("shoulder"));

        let mut items = app.world_mut().entity_mut(wearer);
        let mut items = items.get_mut::<HeldItems>().unwrap();
        items.slots[0].as_mut().unwrap().display = 9;
        app.update();

        let after = roots_of(&app, wearer);
        assert!(
            after[0].is_some_and(|e| e != weapon),
            "a different display is a different model: rebuilt"
        );
        assert!(
            app.world().get_entity(weapon).is_err(),
            "the old model is destroyed, not left behind"
        );
        assert_eq!(after[4], Some(shoulder_root), "the shoulders are untouched");
    }

    /// The chain's first link, on the real spawn path (decision 0833): an item model is CHAINED to
    /// the body wearing it, and the emitters it spawns point at **their own** root rather than at
    /// the wearer. Both halves matter — the item's own sparkle would fade correctly either way,
    /// but a glow instance hung on this root two links down can only reach the wearer through it,
    /// and that is the link the enchant-glow lane never had.
    ///
    /// It survives the sheath MOVE for the same reason the pool does: the root is re-parented, not
    /// rebuilt.
    #[test]
    fn an_item_model_is_chained_to_its_wearer() {
        use crate::model_fade::ParentModel;

        let (mut app, wearer) = dress_a_wearer();
        let shoulder = roots_of(&app, wearer)[4].expect("shoulder");
        assert_eq!(
            app.world()
                .entity(shoulder)
                .get::<ParentModel>()
                .map(|p| p.0),
            Some(wearer),
            "the item chains to the body wearing it"
        );

        let mut items = app.world_mut().entity_mut(wearer);
        let mut items = items.get_mut::<HeldItems>().unwrap();
        items.slots[0].as_mut().unwrap().attach = SHEATH_BACK;
        app.update();
        assert_eq!(
            app.world()
                .entity(shoulder)
                .get::<ParentModel>()
                .map(|p| p.0),
            Some(wearer),
            "…and a sheath swap elsewhere leaves that link alone"
        );
    }
}
