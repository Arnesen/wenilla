//! Attaching a visual to a streamed entity (decision 0006) — the back half of [`super`].
//!
//! [`super`] resolves + builds a [`DisplayModel`](super::DisplayModel) per display id (shared across
//! every entity of that display); this module gives each net entity its visual once that model has
//! loaded: the submesh children + skeleton/animation infra (creatures + player bodies), the per-player
//! character geoset selection + skin material (decision 0041 — the appearance/material resolution
//! lives in [`char_skin`]), particle emitters, GameObject collision, or a colored cube fallback. It
//! reaches the shared types + caches in the parent via `super::`.

use avian3d::prelude::RigidBody;
use benilla_assets::ModelSkeleton;
use benilla_protocol::EntityKind;
use bevy::prelude::*;

use crate::assets::WorldAssets;
use bevy::animation::transition::AnimationTransitions;

use crate::creature_anim::AnimDriver;
use crate::debug_panel::ModelKind;
use crate::interact::WorldObject;
use crate::lighting::SharedLightBuffer;
use crate::model_fade::JoinedFade;
use crate::net::{NetEntity, ObjectStore};
use crate::particles;
use crate::player::CameraPivot;
use crate::target::SelectionRadius;
use crate::terrain::WowModelMaterial;

use super::{
    Characters, Creatures, CubeAssets, DisplayModel, EntityMaterials, GameObjects, ModelHandle,
    SkinComposites, SkinSections, VisualAttached,
};

mod char_skin;
use char_skin::{build_char_skin_materials, equip_geosets, resolve_char_look, resolve_worn_equip};
mod dress;
use dress::{spawn_part, PartDress};
mod glue_preview;
pub(super) use glue_preview::build_glue_preview;
mod redress;
pub(super) use redress::redress_player_looks;

/// Set up the skinned + animated instance shared by creatures/players (decision 0019) and animated
/// GameObjects (decision 0242): the per-instance joints, the billboard glow rig, and — when the model
/// authors sequences — the `AnimationPlayer` + graph + `ModelAnimations` plus the base-track driver.
/// Three flavours share the rig, disjoint so one never touches the other's entities:
///   * a **creature/player** — the movement-driven gait picker (`AnimDriver`);
///   * a **state GameObject** (door/button/chest — `go_state_machine`) — the open/close state machine
///     (`GoAnim`), which cross-fades transitions through `AnimationTransitions`;
///   * **any other animated GameObject** (a mailbox's wind-swung flags, a banner) — *no* controller:
///     the reference arms the model's **Stand** sequence (animation id 0, resolved through the
///     model's own `playableAnimationLookup`) as the universal "loader-idle seed", and loops it iff
///     `M2Sequence.flags & 1 == 0` (wow-re `gameobject-anim-arm.md` §1/§2, byte-verified `0x71019b`
///     / `0x714585` — decision 0637, which corrected the earlier "arms the FIRST sequence" reading
///     this comment used to carry). We arm it here and let the kernel's modulo-wrap run it, ungated
///     like the creature path (GOs are few — the doodad host's draw-gate isn't warranted for them).
///
///     The reference *also* runs a second, GameObject-specific arm off `GAMEOBJECT_STATE` +
///     `GAMEOBJECT_ANIMPROGRESS` (same note, §2), and it lands AFTER this seed, so where both apply
///     the state arm is what is seen. benilla runs that arm only for the door/button/chest types
///     ([`crate::go_anim::go_animates`]). **That gate is a known narrowing, and it is not free:**
///     `benilla-extract goanimscan` measures 251 of 1582 GameObjectDisplayInfo models as
///     STATE-SENSITIVE (some reachable substate plays a different sequence from this seed), which on
///     the live world data is **1979 spawns** sitting on such a model with a GO type left off the
///     machine — books, traps, goobers, generic props. Widening it waits on the wow-re type census
///     (which of the 30 types allocate the `[GO+0x210]` handler at the `0x5f708c` dispatch); the
///     seed below is what those types render in the meantime.
///
/// Global-sequence channels ride along regardless of flavour. This lane spawns **no joint
/// entities** (decision 0724): the pose lives in a [`crate::creature_anim::RigPose`] array on
/// `entity`, and only the CONSUMER bones — attachment points, event markers, emitter/ribbon/
/// light hosts, billboard-card bones — get an anchor entity under `joints_root`, re-seated from
/// the composed pose each frame it changes. Returns the anchors + the palette rig slot
/// (decision 0720), or `None` when the model has no inverse bindposes. Slot `0` = the palette
/// table was full: the anchors still exist (emitters/attachments ride them), but parts fall
/// back to the static bind-pose mesh. No animations ⇒ the pose just holds bind pose
/// (Milestone A).
fn setup_skinned_instance(
    commands: &mut Commands,
    palettes: &mut crate::rig_palette::RigPalettes,
    entity: Entity,
    joints_root: Entity,
    d: &DisplayModel,
    kind: EntityKind,
    go_state_machine: bool,
) -> Option<RigBuild> {
    let ibp = d.inverse_bindposes.as_ref()?;
    let nbones = d.skeleton.joints.len();
    // `joints_root` — the rig's model-space frame — is normally `entity` itself; a MOUNTED
    // rider's frame is the seat anchor instead (decision 0441), a conform-tilted model's its
    // conform node, while the `AnimationPlayer`/driver components stay on `entity`. Skinned
    // parts render purely from the palette, so their own parentage is free.
    let mut pose = crate::creature_anim::RigPose::new(joints_root, &d.skeleton);
    // The consumer bones: every bone something in the world reaches by entity — an attachment
    // point (held items, spell effects, the mount seat, overhead anchors), an event marker, an
    // emitter/ribbon/light host, a billboard card's bone. Everything else is palette-only.
    let mut bone_set = std::collections::BTreeSet::new();
    bone_set.extend(d.attachments.iter().map(|a| a.bone));
    bone_set.extend(d.markers.iter().map(|m| m.bone));
    bone_set.extend(d.emitters.iter().map(|e| e.def.bone));
    bone_set.extend(d.ribbons.iter().map(|r| r.def.bone));
    bone_set.extend(
        d.lights
            .iter()
            .filter_map(|l| u16::try_from(l.def.bone).ok()),
    );
    if let Some(parts) = &d.parts {
        bone_set.extend(
            parts
                .iter()
                .filter_map(|p| p.billboard.as_ref().map(|b| b.bone)),
        );
    }
    let mut anchors = std::collections::HashMap::new();
    for bone in bone_set {
        let Some(m) = pose.model.get(bone as usize) else {
            continue; // an out-of-range authored bone reference — no anchor, consumers miss
        };
        let (scale, rotation, translation) = m.to_scale_rotation_translation();
        let anchor = commands
            .spawn((
                Transform {
                    translation,
                    rotation,
                    scale,
                },
                Visibility::default(),
                crate::creature_anim::RigAnchor { rig: entity, bone },
            ))
            .id();
        commands.entity(joints_root).add_child(anchor);
        pose.anchors.push((bone, anchor));
        anchors.insert(bone, anchor);
    }
    // The owned palette rig (decision 0720): the world pass writes this rig's composed frames ×
    // these bindposes into the slot; every skinned part below tags the slot so the vertex stage
    // finds its palette. The on-replace hook frees the slot with the visual teardown.
    let slot =
        match crate::rig_palette::RigSkin::allocate_bones(palettes, nbones as u32, ibp.clone()) {
            Some(rig) => {
                let slot = rig.slot;
                // The marker's invariant is "currently slot-less": a heal-triggered (or any
                // other) rebuild that lands a slot clears it, or the healer would loop.
                commands
                    .entity(entity)
                    .insert(rig)
                    .remove::<crate::rig_palette::RigStarved>();
                slot
            }
            None => {
                // Table full (warned): parts render the static bind-pose mesh — but no longer
                // for ever. The marker hands the unit to `heal_rig_starved` (decision 0863),
                // which rebuilds the visual once the table has headroom; without it a mob that
                // streamed in during a full-table window stayed a statue for its whole life.
                commands
                    .entity(entity)
                    .insert(crate::rig_palette::RigStarved);
                0
            }
        };
    if let Some(anims) = d.animations.as_ref() {
        // **Every** GameObject instance gets the loader-idle seed, state machine or not — the
        // reference's `0x70ebd0` tail arms bone 0 the moment the M2 goes LIVE and has exactly two
        // callers, so no M2 instance in the client ever exists with nothing armed (wow-re
        // `gameobject-anim-arm.md` §1/§2e). For a door/chest the object-layer arm lands *after* it
        // and overrides it (§2, "because it lands after the loader seed, it is the effective arm");
        // seeding first is what stops the one-frame BIND POSE our state GOs used to render on their
        // first displayed frame, before `go_anim` had a chance to run — the "explodes for a split
        // second" report. The seed is played THROUGH the transitions object so that first arm
        // cleanly fades out of it; playing it bare on the player would leave two clips live at once.
        let mut player = AnimationPlayer::default();
        let mut transitions = AnimationTransitions::new();
        if kind == EntityKind::GameObject {
            if let Some(clip) = anims.first_seq.and_then(|i| anims.clips.get(i)) {
                // Loop iff the sequence says so (`M2Sequence.flags & 1 == 0`) — the kernel's own
                // end-of-band law (wow-re `gameobject-anim-arm.md` §2, byte-verified at
                // `0x714585`): bit0 clear loops on the modulo wrap, bit0 set plays the window and
                // then FREEZES at `end_ms`. An unconditional repeat replayed one-shot idles.
                let active = transitions.play(&mut player, clip.node, std::time::Duration::ZERO);
                if clip.looping {
                    active.repeat();
                }
            }
        }
        commands.entity(entity).insert((
            player,
            AnimationGraphHandle(anims.graph.clone()),
            anims.clone(),
        ));
        match kind {
            EntityKind::GameObject if go_state_machine => {
                commands.entity(entity).insert((
                    // Cross-fades the open/close transition over the clip's blend-in time (0242/0049),
                    // and carries the seed above as the pose the first arm transitions out of.
                    transitions,
                    crate::go_anim::GoAnim::default(),
                ));
            }
            // A loader-idle GameObject needs no driver and no transitions — the looping player IS the
            // whole animation, and nothing will ever arm over it.
            EntityKind::GameObject => {}
            _ => {
                commands
                    .entity(entity)
                    .insert((
                        // Cross-fades over each clip's blend-in time, so a gait change eases (0049).
                        AnimationTransitions::new(),
                        AnimDriver::default(),
                    ))
                    // A (re)built rig is born live: fresh joints spawn pointing at the root, so a
                    // stale park marker from the torn-down visual would desync the LOD gate's
                    // edge-triggered bookkeeping (decision 0448). It re-parks on its own merits.
                    .remove::<crate::creature_anim::AnimParked>();
            }
        }
        // Global-sequence bone channels (the eye-blink eyelid scale, resting fidget pulses; a GO's
        // free-running flicker): free-clock loops the per-sequence reader drops, driven on their own clock.
        if let Some(drive) =
            crate::creature_anim::GlobalSeqDrive::new_rig(&anims.global_bones, nbones)
        {
            commands.entity(entity).insert(drive);
        }
    }
    // The pose buffer last — the anchors above registered themselves into it. The evaluator
    // (decision 0712) samples the player state straight into `locals`; with no joint entities and
    // no `AnimationTargetId`s, Bevy's `animate_targets` has nothing of ours to touch.
    commands.entity(entity).insert(pose);
    Some(RigBuild { anchors, slot })
}

/// A collapsed rig's build result (decision 0724): the consumer anchors by bone + the palette
/// slot each skinned part tags.
struct RigBuild {
    anchors: std::collections::HashMap<u16, Entity>,
    slot: u16,
}

/// Attach a visual to each net entity that doesn't have one yet: its built model (creature / GameObject
/// / player body) as submesh children, or a colored cube fallback. The entity's pose is owned by the
/// net bridge — or, for our own avatar, the player controller — we only add the geometry (and bake
/// per-display scale onto the root). Our own avatar is the same streamed entity and renders here too.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(super) fn attach_entity_visuals(
    mut commands: Commands,
    pending: Query<
        (
            Entity,
            &NetEntity,
            Option<&super::Equipment>,
            Has<super::equipment::Reattached>,
            Option<&super::mount::MountChild>,
            Option<&super::mount::MountBody>,
            Has<crate::transport::TransportAnchor>,
        ),
        Without<VisualAttached>,
    >,
    // The mount children's build state (decision 0441): a mounted unit's rider waits on its mount
    // child's attach — the seat is the mount's attachment-0 joint out of this `BoneAttach`; the
    // `NetEntity` is the staleness check (the field moved while the unit was still pending).
    mount_children: Query<
        (Has<VisualAttached>, Option<&super::BoneAttach>, &NetEntity),
        With<super::mount::MountBody>,
    >,
    // The ItemDisplayInfo catalog (armor region textures, decision 0074). Read-only here; the
    // held-item systems in the same chain hold it mutably in their own turns.
    displays: Option<Res<super::ItemDisplays>>,
    mut transforms: Query<&mut Transform>,
    assets: Res<CubeAssets>,
    creatures: Option<Res<Creatures>>,
    gameobjects: Option<Res<GameObjects>>,
    // Character geoset selection (decision 0041, Milestone B): the customization tables + the entity's
    // decoded appearance, to pick which body geosets a player shows. Absent ⇒ no filtering (every geoset).
    characters: Option<Res<Characters>>,
    // A character body's race/sex + customization for the geoset filter + skin materials: for a player,
    // its decoded descriptor fields ([`ObjectStore`]); a character-model NPC reads its display instead.
    stores: Query<&ObjectStore>,
    // Character skin (decisions 0041 / 0044 / 0045): the CharSections lookup + the bits to composite +
    // upload the per-appearance body atlas and to load + build the hair material — the shared chain (read
    // the BLPs), the `Image` assets + per-appearance composite cache, the asset server (async hair-BLP
    // load), the `WowModelMaterial` assets + dedup cache, and the light buffer. Nested into one param to
    // stay within Bevy's 16-element system-param tuple limit.
    skin_build: (
        Option<Res<SkinSections>>,
        Option<Res<WorldAssets>>,
        Option<Res<SharedLightBuffer>>,
        ResMut<Assets<Image>>,
        ResMut<SkinComposites>,
        Res<AssetServer>,
        ResMut<Assets<WowModelMaterial>>,
        ResMut<EntityMaterials>,
    ),
    // The owned skin-palette table (decision 0720): every skinned instance claims a rig slot.
    mut palettes: ResMut<crate::rig_palette::RigPalettes>,
    time: Res<Time>,
) {
    let (
        sections,
        world_assets,
        shared_light,
        mut images,
        mut skin_composites,
        asset_server,
        mut materials,
        mut entity_mats,
    ) = skin_build;
    // Arm each entity's appear-fade at the moment its visual attaches (≈ its first-visible moment).
    let now = time.elapsed_secs();
    for (entity, net, equipment, reattached, mount_child, mount_body, anchored) in &pending {
        // A player attaches only once its worn-equipment resolution settles (decision 0074): the
        // template round trips are far faster than the model load, and waiting composites the
        // dressed atlas directly instead of flashing naked. (`None` = the resolver hasn't run yet.)
        if net.kind == EntityKind::Player && !equipment.is_some_and(|e| e.settled) {
            continue;
        }
        // The entity's built display model (if it has one): None ⇒ no display / not a modeled kind.
        let dm = net.display_id.and_then(|disp| match net.kind {
            EntityKind::Unit | EntityKind::Player => {
                creatures.as_deref().and_then(|c| c.models.get(&disp))
            }
            EntityKind::GameObject => gameobjects.as_deref().and_then(|g| g.models.get(&disp)),
            _ => None,
        });
        // Worn equipment driving the geoset selection (and, for a player, the region composite): a
        // player from its resolved `Equipment`, a character-model NPC from its display's
        // CreatureDisplayInfoExtra columns. Zeroed (a beast / GameObject / no data) = the naked body.
        let worn = resolve_worn_equip(net, equipment, dm);
        let equip = worn.bodyslots;
        // A model still loading (`parts == None`) waits — leave it un-attached and retry next frame
        // rather than flash a cube we'd swap out. A built-but-empty model falls through to a cube.
        let model = match dm {
            Some(d) => match &d.parts {
                None => continue,
                Some(parts) if !parts.is_empty() => Some(parts.as_slice()),
                Some(_) => None,
            },
            None => None,
        };

        // Invisible interaction-zone GameObjects (the forge, fishing-bobber zone, aura generators, …)
        // carry a *transparent* placeholder M2: the real client's mesh gate is **type-independent** —
        // it draws any loaded model and the per-batch zero-alpha cull skips the transparent geometry
        // (decision 0024, superseding 0023's wrong marker-type gate; verified wow-re go-render-gate).
        // Our M2 alpha cull already reduces those models to zero submeshes, so `model` is `None` here
        // and they render nothing — no GameObject-type special-case needed.
        if let Some(parts) = model {
            // ── Mounts (decision 0441): a mounted unit is TWO skeletons. The mount is a child
            // entity carrying a plain creature `NetEntity` — this very system builds it like any
            // beast next frame(s) — and the rider's joints then root under the mount's
            // attachment-0 seat joint. Until the mount child has attached, the unit builds
            // nothing (no naked-at-the-ground flash; the model loads dominate the wait anyway).
            // A mount child itself has no `ObjectStore`, so it can never take this branch.
            let mount_display = match net.kind {
                EntityKind::Unit | EntityKind::Player => stores
                    .get(entity)
                    .map_or(0, |s| s.0.unit_mount_display_id()),
                _ => 0,
            };
            // Reconcile a stale mount child first: the field moved (or zeroed) while this unit
            // was still pending, or the child entity is gone — drop it and retry fresh next frame.
            if let Some(&super::mount::MountChild(child)) = mount_child {
                let built = mount_children.get(child).map(|(_, _, n)| n.display_id).ok();
                if built != Some(Some(mount_display)) {
                    if let Ok(mut ec) = commands.get_entity(child) {
                        ec.despawn();
                    }
                    commands.entity(entity).remove::<super::mount::MountChild>();
                    continue;
                }
            }
            // Where the rider's root bones parent: the unit entity, or the mounted seat anchor.
            let mut rider_root = entity;
            if mount_display != 0 {
                // The mount scale law (byte-verified, wow-re mount-composition B3): rendered =
                // `SCALE_X × CreatureDisplayInfo.creatureModelScale` — the CDI column ALONE (no
                // CreatureModelData.modelScale). The unit root already bakes SCALE_X, so the
                // child carries just the column; the seat anchor counter-scales it so the rider
                // keeps its own size (the client's compensating own/mount base ratio).
                let mount_scale = creatures
                    .as_deref()
                    .and_then(|c| c.catalog.display_scale(mount_display))
                    .unwrap_or(1.0);
                match mount_child {
                    None => {
                        // Spawn the mount child. Its `NetEntity` registers the display want
                        // (`update_display_models` scans every NetEntity), and this system
                        // attaches it as a creature. Scale: the CDI column alone (the byte law
                        // above); the unit root's SCALE_X composes through the hierarchy.
                        let child = commands
                            .spawn((
                                NetEntity {
                                    kind: EntityKind::Unit,
                                    display_id: Some(mount_display),
                                    scale: mount_scale,
                                },
                                super::mount::MountBody { host: entity },
                                Transform::default(),
                                Visibility::default(),
                            ))
                            .id();
                        if reattached {
                            // A mount-up on a unit already in view isn't a spawn — the mount
                            // joins the rebuild's fade-skip.
                            commands.entity(child).insert(super::equipment::Reattached);
                        }
                        commands.entity(entity).add_child(child);
                        commands
                            .entity(entity)
                            .insert(super::mount::MountChild(child));
                        continue; // wait for the mount to attach
                    }
                    Some(&super::mount::MountChild(child)) => {
                        match mount_children.get(child) {
                            Ok((true, Some(bones), _)) => {
                                if let Some(&(bone, offset)) = bones.points.get(&0) {
                                    if let Some(joint) = bones.anchor(bone) {
                                        // The seat anchor: a child of the mount's attachment-0
                                        // joint at the authored offset, counter-scaled so the
                                        // rider keeps its own size (byte-verified: the client's
                                        // body carries a compensating own/mount base ratio —
                                        // wow-re mount-composition B3).
                                        let anchor = commands
                                            .spawn((
                                                Transform::from_translation(offset).with_scale(
                                                    Vec3::splat(1.0 / mount_scale.max(0.001)),
                                                ),
                                                Visibility::default(),
                                                // The rider's model frame lives inside the
                                                // MOUNT's anchor subtree — the world pass
                                                // cascades a re-seat into the rider's palette
                                                // (decision 0724).
                                                crate::creature_anim::RigFrame(entity),
                                            ))
                                            .id();
                                        commands.entity(joint).add_child(anchor);
                                        rider_root = anchor;
                                    }
                                } else {
                                    // The reference logs exactly this and leaves the body at the
                                    // unit matrix (`0x60ce70`'s present-test miss).
                                    warn!(
                                        "MOUNTDISPLAYIDNOMOUNTATTACHMENT: display {mount_display} \
                                         authors no attachment 0 — rider stays at the unit matrix"
                                    );
                                }
                            }
                            _ => continue, // the mount child is still building — wait
                        }
                    }
                }
            }
            // Real model: submesh children inherit the entity's (pose-driven) transform; bake scale onto it.
            let kind = match net.kind {
                EntityKind::GameObject => ModelKind::GameObject,
                _ => ModelKind::Creature,
            };
            let emitters = dm.map(|d| d.emitters.as_slice()).unwrap_or_default();
            let model_lights = dm.map(|d| d.lights.as_slice()).unwrap_or_default();
            // A GameObject's interior-fold reference point (model-local; the anchor transform
            // applies the entity scale when the classifier folds).
            let bake_center = dm.map(|d| d.bake_center_local).unwrap_or(Vec3::ZERO);
            // The dynamic ground-shade root (decision 0173): one light-node state per object, like
            // the reference's `[obj+0xe0]`; every M2 part below (body, held items) reads it from the
            // tree walk. Kind-independent — a unit and a GameObject run the same 2.5/0.5 MCSH chase,
            // which is the byte-shared target law (0814 restored this; 0809 had split it by kind on
            // the strength of a delivery state we do not model). `insert_if_new` so a gear-change
            // re-attach keeps the already-ramped state instead of resetting it (no one-frame
            // lighting pop).
            commands
                .entity(entity)
                .insert_if_new(crate::entity_shade::GroundShade::default());
            // The root's canonical fold reference: held items share the root's interior verdict
            // (one light node per unit — the reference aliases the wearer's collector into each
            // equipped item, wow-re `unit-light-combine-storm.md`), and their classifier fold must
            // reference the BODY's centre, not the carried position. Plain `insert`: a display-id
            // change re-derives it with the new body model.
            commands
                .entity(entity)
                .insert(crate::interior::BodyBakeCenter(bake_center));
            // The light node's ATTACH MODE — the reference's `[node+0x90]` bit 13, written once at
            // node creation from the descriptor TYPEMASK (`0x613e10`/`0x670db0`) and dispatched at
            // `0x6a86d0`: a GameObject attaches by CONTAINMENT (`0x6a8c10`), anchored at the world
            // bounding-box centre; a unit/player by DOWN-RAY (`0x6a8a20`), anchored at the position.
            // The kind can't change under a live display-id swap, but the remove arm keeps the two
            // mutually exclusive by construction rather than by that argument (decision 0776).
            match net.kind {
                EntityKind::GameObject => {
                    commands
                        .entity(entity)
                        .insert(crate::interior::ContainmentAttach);
                }
                _ => {
                    commands
                        .entity(entity)
                        .remove::<crate::interior::ContainmentAttach>();
                }
            }
            // Identity for the mouseover inspector (and, later, hover tooltips / targeting).
            let object = WorldObject {
                kind,
                label: dm.map(|d| display_label(&d.handle)).unwrap_or_default(),
                id: net.display_id.unwrap_or(0),
                detail: format!("emitters: {}", emitters.len()),
            };
            // Skeletal skin (decision 0019): a creature (Unit/Player) — and now any animated
            // GameObject — draws through the skinned-mesh twin and a **per-instance** joint hierarchy
            // whose entities are children of this entity (so they inherit its world pose; at bind pose
            // every joint matrix collapses to that pose, so the model renders exactly where the static
            // mesh did). Truly static props keep the static mesh. `skin` is `Some((joints,
            // inverse_bindposes, palette_slot))` when instanced (decision 0720; slot 0 = palette
            // full, parts fall back to the static mesh).
            let skin: Option<RigBuild> = match (net.kind, dm) {
                // A creature (or player body — decision 0041) with a real skeleton. The `!is_empty`
                // guard keeps a degenerate boneless model on the static mesh (its skinned twin would
                // carry joint attributes but have no joints to index — out of bounds).
                (EntityKind::Unit | EntityKind::Player, Some(d))
                    if !d.skeleton.joints.is_empty() =>
                {
                    // `rider_root`: the unit itself, or — mounted — the seat anchor under the
                    // mount's attachment-0 joint (decision 0441). The `AnimationPlayer` stays
                    // on the unit entity either way (targets bind by entity, not by path).
                    //
                    // Terrain conform (decisions 0482/0486): a flagged model's root bones
                    // parent one level deeper, under a conform node `conform_units` rotates —
                    // wild quadruped and mount child alike. A mounted RIDER never gets one
                    // (`rider_root != entity`): the ref's `0x7106c0` dispatch is on the
                    // mount-PREFERRED model, and the composite tilts through the mount's
                    // node, seat joint included.
                    let mut joints_root = rider_root;
                    if d.terrain_tilt != 0 && rider_root == entity {
                        let node = commands
                            .spawn((
                                super::conform::ConformNode {
                                    // The ground/yaw source: the streamed unit — for a
                                    // mount child, its HOST (the child sits at the unit
                                    // matrix; its own `Transform` is local).
                                    unit: mount_body.map_or(entity, |mb| mb.host),
                                    mode: d.terrain_tilt,
                                },
                                Transform::default(),
                                Visibility::default(),
                            ))
                            .id();
                        commands.entity(entity).add_child(node);
                        joints_root = node;
                    }
                    setup_skinned_instance(
                        &mut commands,
                        &mut palettes,
                        entity,
                        joints_root,
                        d,
                        net.kind,
                        false,
                    )
                }
                // A GameObject whose model authors a real skeleton + animation draws through the
                // skinned twin like a creature. Two flavours share the rig: a door/button/chest
                // (`go_animates`) runs the open/close state machine off GAMEOBJECT_STATE (decision
                // 0242); ANY other animated GO — a mailbox's wind-swung flags, a banner, a windmill —
                // loops its first sequence as the reference's universal loader-idle seed (wow-re
                // `doodad-anim-host.md`: a non-transport CGGameObject animates identically to a placed
                // doodad). The content gate for the non-state flavour is the doodad classifier's: a GO
                // whose first sequence is a constant pose and which has no global sequences
                // (`DoodadAnimTier::Static`) has nothing to loop, so it keeps the static mesh.
                (EntityKind::GameObject, Some(d))
                    if !d.skeleton.joints.is_empty() && d.animations.is_some() =>
                {
                    let state_machine = stores
                        .get(entity)
                        .is_ok_and(|s| crate::go_anim::go_animates(s.0.gameobject_type_id()));
                    let ambient = !matches!(
                        crate::doodad_anim::classify(&d.skeleton, d.animations.as_ref()),
                        crate::doodad_anim::DoodadAnimTier::Static
                    );
                    (state_machine || ambient)
                        .then(|| {
                            setup_skinned_instance(
                                &mut commands,
                                &mut palettes,
                                entity,
                                entity,
                                d,
                                net.kind,
                                state_machine,
                            )
                        })
                        .flatten()
                }
                _ => None,
            };
            // The bone-riding surface (decision 0072): the instance's joints + the model's attachment
            // points, so held items (and future bone riders) can hang from the hand/hip/back joints.
            if let (Some(rb), Some(d)) = (&skin, dm) {
                // The event markers keep the client's first-match scan order: an ident already
                // present wins (character models carry six `$CSD` records — the first is the one
                // `0x7130e0` would return).
                let mut markers = std::collections::HashMap::new();
                for m in &d.markers {
                    markers.entry(m.ident).or_insert((m.bone, m.offset));
                }
                commands.entity(entity).insert(super::BoneAttach {
                    anchors: rb.anchors.clone(),
                    points: d
                        .attachments
                        .iter()
                        .map(|a| (a.id, (a.bone, a.offset)))
                        .collect(),
                    markers,
                });
                // The display-facing counter-twist channels (the strafe body pose): the model's
                // SpineLow/Head key-bones, straight into the pose buffer. Models without either
                // key-bone (beasts, props) get no component — the client's capability gates.
                let nb = d.skeleton.joints.len();
                let in_range = |b: Option<u16>| b.filter(|&i| (i as usize) < nb);
                let (spine, head) = (
                    in_range(d.skeleton.spine_bone),
                    in_range(d.skeleton.head_bone),
                );
                if spine.is_some() || head.is_some() {
                    commands
                        .entity(entity)
                        .insert(crate::creature_anim::BodyTwist::new(spine, head));
                }
            }
            // Character geoset selection (decision 0041, Milestone B): a player body model carries
            // *every* hairstyle / facial-hair / body-option geoset; show only the selected ones. The
            // model (and its `parts`) is shared across all players of this displayId, so the filter is
            // **per-entity** here — from this player's decoded appearance — not baked into the cache.
            // `None` (no look, or the tables unavailable) ⇒ render every part, as before.
            //
            // The character look: a player takes it from the wire, a character-model NPC from its
            // display's CreatureDisplayInfoExtra (decision 0041). Both then drive the same geoset filter
            // + skin/hair materials below; a beast NPC / GameObject has no look and is unaffected.
            let look = resolve_char_look(net, dm, entity, &stores);
            // The worn geoset selectors (decision 0074, the B1–B8 branches): a player's from the
            // resolved equipment display rows; an NPC / naked default otherwise.
            // (The helm's hide-mask row pair, RF-0083: hair/facial/ears tuck under it. For a
            // character-model NPC the helm id is its CreatureDisplayInfoExtra head column.)
            let equip_geosets = equip_geosets(displays.as_deref(), &equip, worn.cloak, worn.helm);
            let visible_geosets: Option<Vec<u16>> = look.as_ref().and_then(|l| {
                let cg = characters.as_deref()?;
                Some(cg.0.visible_geosets(
                    l.race,
                    l.sex,
                    l.hair_style,
                    l.facial_hair,
                    &equip_geosets,
                ))
            });
            // Character skin (decisions 0041 / 0044 / 0045): a character body's body-skin batches (M2
            // type 1) get the body atlas, and its hair batches (type 6) get the hair-mesh texture — both
            // per-appearance over the shared model, so built here (not in the shared model cache);
            // `model_material` then dedups by texture so bodies of one look share them. `(None, None)` ⇒
            // those parts keep their built (untextured) material (no look, or tables/chain absent).
            let char_mats = match look.as_ref() {
                Some(l) => build_char_skin_materials(
                    l,
                    equip,
                    worn.cloak,
                    displays.as_deref(),
                    sections.as_deref(),
                    world_assets.as_deref(),
                    shared_light.as_deref(),
                    parts,
                    &mut images,
                    &mut skin_composites.0,
                    &asset_server,
                    &mut materials,
                    &mut entity_mats.0,
                ),
                None => (None, None, None, (None, None)),
            };
            // Whether any spawned child armed a `PendingAppearFade` this pass — mirrored onto the unit
            // root below as `UnitAppearFade` so a held item / helm / shoulder that resolves and spawns
            // *later* (`entities::equipment::attach_held_items`, async — a template round trip, a model
            // load) can join the same ramp instead of popping in opaque or racing its own fade from
            // zero. Decision 0032 read as a per-unit property, not a per-mesh-at-attach-time stamp.
            let mut unit_will_fade = false;
            // This unit's **instance slot**, in tag-field form — one value for the whole unit, worn by
            // every part and card below. See the per-part note at the spawn for why it is the
            // instance's identity rather than a skinning detail (decision 0812).
            let inst_slot = skin.as_ref().map_or(0, |rb| rb.slot);
            let rig_tag = crate::mesh_tag::rig_bits(inst_slot);
            // The armed idle's **authored** CAaBox (decision 0637) — the mouseover picker's
            // volume for a skinned part, NOT a culling volume (skinned entity parts are never
            // frustum-culled; see the `NoFrustumCulling` note at the insert below). The bind-pose
            // box the mesh would otherwise get is only a fair stand-in while the animation keeps
            // the model near rest — the duel flag breaks it: `DuelingFlag.m2` is modelled 9 yards
            // in the air and its Stand translates the root `−9.124` to plant it, so the bind box
            // sits a whole model-height above the drawn geometry. The M2 authors a per-sequence
            // CAaBox for exactly this; for the flag's Stand it is ground-to-tip, which is also
            // what makes the planted flag hoverable where it is actually seen.
            let idle_aabb = dm.and_then(|m| m.animations.as_ref()).and_then(|a| {
                let clip = a.first_seq.and_then(|i| a.clips.get(i))?;
                (clip.bounds_max.cmpgt(clip.bounds_min).all()).then(|| {
                    bevy::camera::primitives::Aabb::from_min_max(clip.bounds_min, clip.bounds_max)
                })
            });
            // Everything one part's spawn reads from this unit, gathered once (`attach::dress`) —
            // the same context the gear-change **re-dress** feeds `spawn_part`, so a body built at
            // stream-in and a geoset that appears when a belt is swapped are dressed by one law.
            let no_anchors = std::collections::HashMap::new();
            let dress = PartDress {
                unit: entity,
                kind,
                char_mats: &char_mats,
                object: &object,
                rig_tag,
                inst_slot,
                rigged: skin.is_some(),
                anchors: skin.as_ref().map_or(&no_anchors, |rb| &rb.anchors),
                bake_center,
                idle_aabb,
                now,
                // A fresh visual arms the appear-fade on every fade-capable part (decision 0032).
                // A rebuild that is NOT a spawn — a mount transition, a display swap — spawns
                // steady: `Reattached` says the unit was already standing there.
                fade: if reattached {
                    JoinedFade::Steady
                } else {
                    JoinedFade::Pending { since: now }
                },
            };
            for (i, part) in parts.iter().enumerate() {
                // Skip a geoset this character doesn't show (an unselected hair/facial/body
                // variant, or a body region its worn gear replaces). The equipment half of that
                // selection is re-run in place on a gear change (`attach::redress`); this is the
                // same predicate, evaluated once at build.
                if visible_geosets
                    .as_ref()
                    .is_some_and(|vis| !vis.contains(&part.geoset_id))
                {
                    continue;
                }
                unit_will_fade |= spawn_part(&mut commands, part, i, &dress);
            }
            // Mirror the appear-fade clock onto the unit root (see `unit_will_fade` above): a held item
            // / helm / shoulder attaching later reads this to join the same ramp
            // (`entities::equipment::attach_held_items`). A `Reattached` rebuild — a mount
            // transition, a display swap — abandons any in-flight fade outright, matching the body
            // parts it just rebuilt steady, so anything spawned for that same rebuild is steady too.
            // (A GEAR change no longer reaches here at all: it re-dresses in place, and the parts it
            // spawns join this clock instead of clearing it — `attach::redress`.)
            if unit_will_fade {
                commands
                    .entity(entity)
                    .insert(crate::model_fade::UnitAppearFade::Pending { since: now });
            } else if reattached {
                commands
                    .entity(entity)
                    .remove::<crate::model_fade::UnitAppearFade>();
            }
            // Final size = the server's per-object scale (`OBJECT_FIELD_SCALE_X`) alone. The server
            // already folds the unit's DBC scale (`CreatureModelData.modelScale ×
            // CreatureDisplayInfo.scale`, or an explicit per-spawn override) into this field, and the
            // real client renders units at the field alone (verified: wow-re `world_model_scale`
            // `0x613ef0`, vmangos `Unit::GetScaleForDisplayId`). Multiplying our own DBC scale on top
            // double-applies it — `native²`, worst for the sub-1.0 starting-zone scales. A GameObject's
            // display scale was always 1.0, so this is unchanged for it.
            let placement = if let Ok(mut t) = transforms.get_mut(entity) {
                t.scale = Vec3::splat(net.scale);
                *t
            } else {
                Transform::default()
            };
            // The equipment this visual was dressed with (decision 0074): `redress_player_looks`
            // diffs it against the live resolution and re-dresses the standing visual on a change.
            if let (EntityKind::Player, Some(e)) = (net.kind, equipment) {
                commands
                    .entity(entity)
                    .insert(super::equipment::AppliedEquipment(*e));
            }
            // Camera framing-pivot height (model-derived, pre-scale): the self-avatar reads it in the
            // camera controller to target ~neck height instead of a fixed offset (harmless on NPCs).
            commands.entity(entity).insert(CameraPivot {
                height_local: dm.map(|d| d.pivot_height_local).unwrap_or(0.0),
            });
            // The overhead-anchor fallback input (combat text over a model with no PlayerName
            // attachment — `0x608640`'s defensive branch).
            commands.entity(entity).insert(super::OverheadFallback(
                dm.map(|d| d.bbox_z_local).unwrap_or(0.0),
            ));
            // Selection-ring radius (model-local sphere radius, pre-scale) — the targeting ring reads it
            // × the unit's scale (harmless on non-unit models, which are never ringed).
            commands.entity(entity).insert(SelectionRadius(
                dm.map(|d| d.ground_radius_local).unwrap_or(0.0),
            ));
            // Particle emitters (flames/glows) — spawned per entity, despawning with it. A skinned
            // creature's emitter rides its host bone's joint with the model-space origin rebased
            // into the bone frame (`position − pivot`), exactly like a doodad emitter (0130 phase 4,
            // same rig identity) — the kobold's candle flame follows the head through the crouch
            // instead of floating at the rest-pose height. GameObjects/boneless models keep
            // whole-entity follow (no joints; their bones hold rest pose anyway).
            {
                for em in emitters {
                    let owner = skin
                        .as_ref()
                        .and_then(|rb| rb.anchors.get(&em.def.bone))
                        .map_or((entity, [0.0; 3]), |&j| (j, em.bone_pivot));
                    particles::spawn_emitter(
                        &mut commands,
                        em,
                        placement,
                        particles::EmitterFrames {
                            owner: Some(owner),
                            // A unit's OWN model is not an attached model (`[model+0x17c]` = 0).
                            attach: None,
                            // The cloud anchors at the unit; bones compose births only.
                            anchor: Some(entity),
                            // The unit's model going away IS this emitter's model going away
                            // (stream-out, a visual rebuild) — free the pool with it (0826).
                            on_owner_loss: particles::OwnerLoss::Free,
                            // The unit IS the model instance here, so its own render alpha
                            // (appear ramp, stream-out ramp, the self-avatar feather) multiplies
                            // its clouds — decision 0827.
                            alpha: Some(entity),
                        },
                        // The emitters' rate/enabled read this instance's PLAYING sequence — a
                        // unit's or GameObject's `AnimationPlayer` on the root. A quest object's
                        // explosion is authored inside its one-shot clips with an OFF window at
                        // idle (B27); a creature's death-only smoke is the same shape.
                        particles::EmitClock::Host(entity),
                    );
                }
            }
            // The model's own M2 point lights (decision 0016) — a fire elemental's glow, a lit
            // GameObject brazier. Same host-bone ride as the emitters, for the same reason: the
            // reference re-registers each light at its LIVE bone position every frame. (The far more
            // common carried light is the held torch — that one spawns on the item model, in
            // `equipment`.)
            super::spawn_carried_lights(&mut commands, model_lights, entity, |bone| {
                skin.as_ref()
                    .zip(u16::try_from(bone).ok())
                    .and_then(|(rb, b)| rb.anchors.get(&b))
                    .copied()
            });
            // Ribbon trails (wisp streamers, trailing quest-object crystals) — the same host-bone
            // ride as the emitters; the trail self-despawns when its owner joint/entity goes.
            {
                for rb in dm.map(|d| d.ribbons.as_slice()).unwrap_or_default() {
                    let (owner, use_pivot) = skin
                        .as_ref()
                        .and_then(|build| build.anchors.get(&rb.def.bone))
                        .map_or((entity, false), |&j| (j, true));
                    // A streamed unit's body trails are always-on (wisp streamers, crystal
                    // trails); the per-sequence visibility gate is the thrown weapon's InFlight
                    // keying, which body models don't author — so the running gait is immaterial.
                    crate::ribbons::spawn_ribbon(
                        &mut commands,
                        rb,
                        owner,
                        use_pivot,
                        placement.scale.max_element(),
                        None,
                        // The unit's own render alpha gates its trail (0827).
                        Some(entity),
                    );
                }
            }
            // Static collision for solid GameObjects (chests, mining veins, doors…): the model-local
            // collider baked at build time rides the entity's pose, so player + camera collide with it.
            // Hull-less GameObjects (herbs, small props) carry none — collide-iff-hull. GameObjects only;
            // creatures use unit-collision, not modeled here. An anchored transport (boat/lift) is
            // Kinematic — its body moves every frame, and a Static insert here would silently
            // overwrite the arm's label when the asset finished loading after the arm ran.
            if matches!(net.kind, EntityKind::GameObject) {
                if let Some(col) = dm.and_then(|d| d.collider.clone()) {
                    let body = if anchored {
                        RigidBody::Kinematic
                    } else {
                        RigidBody::Static
                    };
                    commands.entity(entity).insert((body, col));
                }
            }
        } else {
            // Cube fallback: other players (cyan, slim block) and NPCs (red, person-box) without a usable
            // model. A model-less GameObject renders *nothing* — it's an effect-only/invisible/trigger
            // object (all particle-only in the real client), so a floating cube would be noise. The cube
            // origin is centered, so a child offset lifts it onto the ground.
            let fallback = match net.kind {
                EntityKind::Player => {
                    Some((assets.player_mat.clone(), assets.player_mesh.clone(), 1.0))
                }
                EntityKind::Unit => Some((assets.npc_mat.clone(), assets.mesh.clone(), 2.0)),
                EntityKind::GameObject => {
                    debug!(
                        "gameobject (display {:?}) has no usable model — not rendering",
                        net.display_id
                    );
                    None
                }
                // A DynamicObject is deliberately invisible as an *object* — its look is the
                // spell's area effect (the dest-anchored visual lane), never a fallback cube.
                EntityKind::DynamicObject | EntityKind::Other => None,
            };
            if let Some((material, mesh, lift)) = fallback {
                // Tag the cube as a pickable unit too (kind `Creature` covers units + player bodies), so
                // a model-less NPC / other player can still be inspected and, crucially, targeted.
                let object = WorldObject {
                    kind: ModelKind::Creature,
                    label: format!("{:?} (no model)", net.kind),
                    id: net.display_id.unwrap_or(0),
                    detail: String::new(),
                };
                commands.entity(entity).with_children(|parent| {
                    parent.spawn((
                        Mesh3d(mesh),
                        MeshMaterial3d(material),
                        Transform::from_translation(Vec3::Y * lift),
                        object,
                    ));
                });
            }
        }
        // The mount this visual was built with (decision 0441, the `AppliedEquipment` pattern):
        // `refresh_mounts` diffs it against the live field and rebuilds on any transition. Written
        // on the cube fallback too (same read, no seat), so a model-less unit can never churn.
        if matches!(net.kind, EntityKind::Unit | EntityKind::Player) {
            let applied = stores
                .get(entity)
                .map_or(0, |s| s.0.unit_mount_display_id());
            commands
                .entity(entity)
                .insert(super::mount::AppliedMount(applied));
        }
        commands
            .entity(entity)
            .insert(VisualAttached)
            // The display this visual was built with (decision 0695, the same pattern):
            // `refresh_live_display` diffs it against the live descriptor and rebuilds on a
            // change (druid form, GM morph). Stamped on the cube fallback too, so a model-less
            // unit can never churn.
            .insert(super::live_display::AppliedDisplay(net.display_id))
            .remove::<super::equipment::Reattached>();
    }
}

/// Spawn a rig's joint-entity hierarchy under `root` (decision 0019): one entity per bone
/// carrying its rest-local translation, parented per the skeleton — root bones under `root` so
/// they inherit the entity's world pose, others under their parent joint. Returns the joints in
/// bone order, so a vertex's joint index maps straight in and every submesh's palette rig shares
/// this one set. **The doodad/effect/booth lane only** (decision 0724): a streamed unit's rig is
/// the joint-less [`crate::creature_anim::RigPose`] buffer instead — this hierarchy remains for
/// the Bevy-graph-driven hosts. `holder` is the rig root that carries (or will carry) the
/// [`crate::rig_palette::RigSkin`] — every joint marks it with a `RigJoint`, which is what the
/// palette change-sweep iterates (0720).
pub(crate) fn spawn_joints(
    commands: &mut Commands,
    root: Entity,
    holder: Entity,
    skeleton: &ModelSkeleton,
) -> Vec<Entity> {
    let joints: Vec<Entity> = skeleton
        .joints
        .iter()
        .map(|j| {
            commands
                // Visibility too, not just Transform: held items and spell effects hang their
                // visible roots under joints, and a gap in the chain both trips Bevy's B0004 and
                // orphans those subtrees from the unit root's visibility (a hidden unit would
                // keep its weapon on screen).
                .spawn((
                    Transform::from_translation(j.local_translation),
                    Visibility::default(),
                    crate::rig_palette::RigJoint(holder),
                ))
                .id()
        })
        .collect();
    for (i, j) in skeleton.joints.iter().enumerate() {
        let parent = usize::try_from(j.parent)
            .ok()
            .and_then(|p| joints.get(p).copied())
            .unwrap_or(root);
        commands.entity(parent).add_child(joints[i]);
    }
    joints
}

/// A display model's source path as a readable inspector label (the asset path, sans `mpq://` source).
/// Empty for the model-less variant or a path-less handle.
fn display_label(handle: &ModelHandle) -> String {
    let path = match handle {
        ModelHandle::M2(h) => h.path(),
        ModelHandle::Wmo(h) => h.path(),
        ModelHandle::None => None,
    };
    path.map(|p| p.path().to_string_lossy().into_owned())
        .unwrap_or_default()
}
