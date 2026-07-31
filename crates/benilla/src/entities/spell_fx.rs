//! Spell-visual **effect models** (decision 0099 phase 3): the attach-point `.mdx` glows a
//! spell-visual kit hangs on a casting unit — Fireball's burning hands, a heal's golden sparkle.
//! The **lootable-corpse sparkle** rides the same plumbing (the client hangs it on the same
//! `Effect_C` node type): `creature_anim::spell_visual::arm_loot_fx` writes its Begin/Reap edges
//! under a reserved key.
//!
//! The router (`crate::creature_anim::spell_visual::route_cast_visuals`) resolves each cast edge's
//! kit to `(M2 attachment id, model path)` pairs and emits [`SpellKitFx`] edges; this module owns
//! the render side: a path-keyed [`DisplayModel`] cache (the held-items pattern — entries created
//! here, parts built by `super::update_display_models` once the M2 loads), and per-unit effect
//! instances spawned under the unit's [`BoneAttach`] joints so they ride the animating bone —
//! exactly the client's `CEffect` bone attach, re-resolved per frame off the live bone matrix
//! (wow-re `spell-visual-apply.md` §1.5/§5; riding the joint entity gives us the same for free).
//!
//! Lifetime is the client's stage policy (decision 0107 verdict 2): a **persistent** instance
//! (precast/channel) lives until its spell-id-keyed [`SpellKitFx::Reap`] (the client's
//! `0x614150`); a **self-terminating** one (cast release, kit push) despawns after one pass of
//! its model's sequence 0 — the stage-0/1 completion callback's clock, which runs whether or not
//! the sequence moves a bone (the eat/drink tankard is a 6.667 s sequence with zero bone keys;
//! against the ~5 s kit-resend cadence its instances overlap into a continuously held jug). The
//! attach cascade for a model lacking the requested point is the client's: tag → `0xf` → `0x13`
//! → the unit's base (wow-re §5, `0x61ceb0`).
//!
//! Effect models run their **bone rigs** ([`arm_effect_rig`] — the first clip + global sequences
//! pose the joints that meshes skin to and emitters/ribbons/cards ride) and their **material
//! animation**: each part's colour-alpha × transparency-weight loops sample per instance on the
//! attach clock (a [`MatAnim`](crate::doodad_anim::MatAnim) that owns the part's render-alpha
//! tag — Battle Shout's staggered crescent pulses), and an animated M2Color RGB ticks a
//! **per-instance material clone**'s tint uniform ([`FxTintAnims`] — the white-hot flash cooling
//! to red; per instance because one cast = one phase, unlike the doodad lane's shared-clock
//! loops in [`crate::doodad_anim::TintAnimMaterials`]).
//!
//! Effect instances **fire their model's event track** ([`fire_fx_anim_events`], decision 0304):
//! the playing clip's `$SND`-family keyframes emit the same [`AnimSoundEvent`] stream creatures
//! emit (spatialized at the host unit), so an effect whose sound lives in its own M2 — the
//! level-up pillar's `$SND(888)` at t=0.033s — rings without any code-side kit. (The ding's
//! WATCHER — the `UNIT_FIELD_LEVEL` edge that spawns the pillar — lives with its lootable-edge
//! sibling in `creature_anim::spell_visual::arm_level_up_fx`, the SpellKitFx writers' home.)
//!
//! Approximations, named: the UV-scroll channel still doesn't run here (0130's scope was placed
//! doodads); the span-based self-termination stands in for the client's model-event completion
//! callback; and the kit sound keeps playing unconditionally where the client gates it on
//! no-visual-attached.

use std::collections::HashMap;

use benilla_assets::bone_target_id;
use bevy::animation::AnimatedBy;
use bevy::ecs::entity::EntityHashMap;
use bevy::prelude::*;

use crate::creature_anim::{scan_events, AnimSoundEvent, FxClass, SpellKitFx};
use crate::debug_panel::{ModelKind, ModelPart};
use crate::model_render::m2_url;
use crate::particles;
use crate::terrain::WowModelMaterial;

use super::{BoneAttach, DisplayModel, EntityPart, ModelHandle};

/// The client's attach fallback cascade when a model lacks the requested point (wow-re
/// `spell-visual-apply.md` §5, `0x61ceb0`/`0x61fae0`): retry `0xf`, then `0x13`, then the unit's
/// base position.
const ATTACH_FALLBACKS: [u16; 2] = [0xf, 0x13];

/// The self-termination span when an effect model has no sequence table at all (the cube
/// fallback and kin) — long enough to read as a flash, short enough never to linger. Shared
/// with the dest-anchored lane's one-shots ([`super::dest_fx`]) — same completion-callback
/// stand-in.
pub(super) const FALLBACK_SPAN: f32 = 1.0;

/// The `fxview` unit lane's synthetic guid (`WOW_FX_DISPLAY`) — a high-word `0xF130` creature guid
/// like the wire's, in a serverless capture where nothing else claims one.
const FXVIEW_UNIT_GUID: u64 = (0xF130u64 << 48) | 0xFC0FEE;

/// How long a self-terminating instance may wait unspawned (model/skeleton never loading — the
/// cube-fallback caster) before it is dropped instead of pending forever.
const PENDING_TIMEOUT: f32 = 10.0;

/// The effect-model cache: model path → its [`DisplayModel`] (the held-items pattern — the shell
/// is created here on first use; `super::update_display_models` builds `parts` once the asset
/// loads, shared by every instance of that path).
#[derive(Resource, Default)]
pub(super) struct SpellFx {
    pub(super) models: HashMap<String, DisplayModel>,
}

/// The live **per-instance tint clones**: an effect part whose M2Color RGB animates gets its own
/// material clone at attach (one cast = one phase — the doodad lane's shared-clock registry
/// [`crate::doodad_anim::TintAnimMaterials`] would run every Battle Shout on one global pulse),
/// keyed by the clone's asset id → `(RGB loop, attach-time clock origin)`. An entry drops with
/// its material asset — the instance root's despawn releases the only strong handle.
#[derive(Resource, Default)]
pub(crate) struct FxTintAnims(
    HashMap<
        bevy::asset::AssetId<WowModelMaterial>,
        (std::sync::Arc<benilla_formats::RgbAnim>, f32),
    >,
);

/// Re-sample every live fx clone's tint on its instance clock. Deliberately NOT capture-gated
/// (unlike the doodad lane's shared-clock ticks): the `fxview` instrument ages an effect inside a
/// capture, and golden scenarios spawn no effects, so determinism is unaffected.
pub(crate) fn tick_fx_tint(
    time: Res<Time>,
    mut reg: ResMut<FxTintAnims>,
    mut materials: ResMut<Assets<WowModelMaterial>>,
) {
    if reg.0.is_empty() {
        return;
    }
    let now = time.elapsed_secs();
    reg.0.retain(|id, (anim, origin)| {
        let Some(mat) = materials.get_mut(*id) else {
            return false; // the instance despawned — its clone unloaded with it
        };
        let rgb = anim.sample(now - *origin);
        mat.extension.tint = Vec4::new(rgb[0], rgb[1], rgb[2], 1.0);
        true
    });
}

/// Resolve one part's material for a NEW effect instance: the shared handle, unless the part's
/// M2Color RGB animates — then a per-instance clone seeded at the loop's first key and registered
/// for the per-frame tint tick ([`FxTintAnims`]).
fn fx_part_material(
    part: &EntityPart,
    now: f32,
    wow_materials: &mut Assets<WowModelMaterial>,
    tint_reg: &mut FxTintAnims,
) -> Handle<WowModelMaterial> {
    let Some(anim) = &part.rgb_anim else {
        return part.material.clone();
    };
    let Some(mut mat) = wow_materials.get(part.material.id()).cloned() else {
        return part.material.clone(); // shared material not built yet — parts were checked ready
    };
    let t0 = anim.sample(0.0);
    mat.extension.tint = Vec4::new(t0[0], t0[1], t0[2], 1.0);
    let handle = wow_materials.add(mat);
    tint_reg.0.insert(handle.id(), (anim.clone(), now));
    handle
}

// (The ground-decal material clone died with 0733: a ground-quad part's draw identity —
// texture, blend, fog policy, RGB/alpha loops — rides its `GroundFxDecal` record and the
// effect stream's per-vertex tint instead of a per-instance `WowModelMaterial`.)

/// Drive the `fxview` capture fixture (see [`crate::capture::FxViewRequest`]) — the effect-
/// viewer instrument: once the capture driver arms it (scene settled), spawn a root at the
/// fixture point, create the model's [`SpellFx`] cache entry, attach the full visual set
/// through the same [`attach_effect_visuals`] body the game uses, and (for missiles) fly the
/// root along its facing so trails extend. Inert outside fxview captures (the request resource
/// only exists then).
#[allow(clippy::too_many_arguments)]
pub(crate) fn drive_fx_view(
    req: Option<Res<crate::capture::FxViewRequest>>,
    state: Option<ResMut<crate::capture::FxViewState>>,
    mut commands: Commands,
    fx: Option<ResMut<SpellFx>>,
    asset_server: Res<AssetServer>,
    time: Res<Time>,
    mut wow_materials: ResMut<Assets<WowModelMaterial>>,
    mut tint_reg: ResMut<FxTintAnims>,
    ibps: Res<Assets<bevy::mesh::skinning::SkinnedMeshInverseBindposes>>,
    mut palettes: ResMut<crate::rig_palette::RigPalettes>,
    spatial: avian3d::prelude::SpatialQuery,
    mut transforms: Query<&mut Transform>,
) {
    let (Some(req), Some(mut state)) = (req, state) else {
        return;
    };
    if !state.armed {
        return;
    }
    let Some(mut fx) = fx else {
        // Server-less capture boot: the cache resource normally rides the net session.
        commands.init_resource::<SpellFx>();
        return;
    };
    let root = *state.root.get_or_insert_with(|| {
        let mut pos = benilla_assets::coords::wow_to_bevy(crate::capture::FXVIEW_POS);
        // `WOW_FX_DISPLAY`: the UNIT lane. Spawn the live component set a streamed creature gets
        // (`net::apply`'s, the same one the `vplates` fixture's wolf uses) and let the ordinary
        // unit pipeline build the model — so what this shoots is a creature, not a model, and
        // every unit-only term (tag alpha, the fade gate, anim LOD, the emitters' sequence host)
        // is in the picture. Always seated on the terrain: a creature stands on the ground.
        if let Some(display) = req.display {
            if let Some(hit) = spatial.cast_ray(
                pos,
                Dir3::NEG_Y,
                500.0,
                true,
                &crate::collision::player_query_filter(),
            ) {
                pos.y -= hit.distance;
            }
            return commands
                .spawn((
                    crate::net::Guid(FXVIEW_UNIT_GUID),
                    crate::net::NetEntity {
                        kind: benilla_protocol::EntityKind::Unit,
                        display_id: Some(display),
                        scale: req.scale,
                    },
                    crate::net::ObjectStore(benilla_protocol::messages::ObjectFields::from_pairs(
                        &[
                            (22, 100), // UNIT_FIELD_HEALTH
                            (28, 100), // UNIT_FIELD_MAXHEALTH
                            (34, 60),  // UNIT_FIELD_LEVEL
                            (35, 35),  // UNIT_FIELD_FACTIONTEMPLATE — friendly, so no combat pose
                        ],
                    )),
                    Transform::from_translation(pos)
                        .with_rotation(Quat::from_rotation_y(req.yaw_deg.to_radians())),
                    Visibility::default(),
                ))
                .id();
        }
        // `WOW_FX_GROUND=1`: seat the fixture ON the terrain — a ground-anchored effect's flat
        // quads render as projected surface decals and need ground inside their vertical slab.
        // The scene settled before arming, so the streamed tile's collider is there to hit.
        if req.ground {
            if let Some(hit) = spatial.cast_ray(
                pos,
                Dir3::NEG_Y,
                500.0,
                true,
                &crate::collision::player_query_filter(),
            ) {
                pos.y -= hit.distance;
            }
        }
        commands
            .spawn((
                Transform::from_translation(pos)
                    .with_rotation(Quat::from_rotation_y(req.yaw_deg.to_radians())),
                Visibility::default(),
            ))
            .id()
    });
    // A missile only trails in motion: fly the root along its model-forward (local −Z).
    // WOW_FX_TURN keeps the fixture rotating — the attached-model heading-since-birth fan.
    if req.fly > 0.0 || req.turn != 0.0 {
        if let Ok(mut t) = transforms.get_mut(root) {
            let fwd = t.rotation * -Vec3::Z;
            t.translation += fwd * req.fly * time.delta_secs();
            if req.turn != 0.0 {
                t.rotation =
                    Quat::from_rotation_y((req.turn * time.delta_secs()).to_radians()) * t.rotation;
            }
        }
    }
    // The one-pass reap (the game's completion callback, `fx_attach`): a discrete kit
    // instance dies at exactly one pass of sequence 0 and its emitters DRAIN — the fixture
    // mirrors it so a capture past the span shows the truth. `WOW_FX_HOLD=1` previews a
    // persistent hold instead (reaped by its spell edge in game, so no clock here).
    // The one-pass reap is an effect-instance rule; a unit stands there.
    if !req.hold && !state.expired && req.display.is_none() {
        if let Some(at) = state.attached_at {
            let span = fx
                .models
                .get(&req.model_path)
                .and_then(|dm| dm.first_seq_span)
                .unwrap_or(FALLBACK_SPAN);
            if time.elapsed_secs() >= at + span {
                commands.entity(root).despawn();
                state.expired = true;
            }
        }
    }
    if state.attached_at.is_none() {
        // The unit lane has no attach step — the unit pipeline builds the model on its own. Its
        // "age" is therefore seconds since the unit appeared, which is what the animation phase is
        // measured in.
        if req.display.is_some() {
            state.attached_at = Some(time.elapsed_secs());
            return;
        }
        fx.models
            .entry(req.model_path.clone())
            .or_insert_with(|| DisplayModel {
                handle: ModelHandle::M2(asset_server.load(m2_url(&req.model_path))),
                ..super::empty_shell()
            });
        if attach_effect_visuals(
            &mut commands,
            root,
            &fx.models[&req.model_path],
            time.elapsed_secs(),
            true, // the fixture plants at a world point — ground quads decal onto the terrain
            // The fixture previews kit effects, which are attached models — but it hangs on
            // nothing (there is no host model in a preview), so its pool keeps the drain.
            EffectHost {
                attached: true,
                parent: None,
            },
            &mut wow_materials,
            &mut tint_reg,
            &ibps,
            &mut palettes,
            None, // the fixture previews a kit effect at its default (first) clip
        ) {
            state.attached_at = Some(time.elapsed_secs());
        }
    }
}

/// Where an effect-model instance sits in the client's **model graph** — the two facts every
/// caller of [`attach_effect_visuals`] states, together, because they are one fact seen from two
/// sides and a lane that answered only the first is what left a weapon's enchant glow with no
/// alpha source and no owner (decision 0833).
#[derive(Clone, Copy, Default)]
pub(super) struct EffectHost {
    /// Is this an **attached** model in the client's sense (`[model+0x17c] ≠ 0`)? It sets the
    /// emitters' attach frame `A`: a kit effect on a unit and the `fxview` fixture are attached
    /// (the cloud fans with the host's motion), a missile is not — its trail stays world-frozen
    /// (wow-re `part-kit-effect-attach-orient.md`).
    pub attached: bool,
    /// The model instance this one is **chained to** ([`crate::model_fade::ParentModel`]): the
    /// unit a kit effect is hung on, the item root a weapon glow rides. `None` for a model that
    /// belongs to no other — a missile, the fixture preview — which is also what keeps the 0202
    /// drain for the impacting trail.
    pub parent: Option<Entity>,
}

/// Attach one effect model's full visual set to `root` — THE one body for every effect-model
/// consumer (spell-kit instances, missiles, the `fxview` capture fixture): the authored rig
/// ([`arm_effect_rig`]), the part meshes (skinned twins on a rigged model, `NoFrustumCulling` —
/// a billboarded subtree renders wherever the camera is, so a bind-pose Aabb is wrong by
/// construction), the billboard cards (riding their bone's joint on a rigged model, decisions
/// 0153/0154), the emitters/ribbons on their host-bone joints (the 0130 rig identity), the
/// **material animation** (module docs): per-part `MatAnim` alpha samplers on the attach clock
/// (`now`) + per-instance tint clones for animated-RGB parts — and, on a **ground-anchored**
/// instance (`ground_anchor`: the kit slot resolved to the base point `0x13` or the unit root),
/// each flat ground-plane quad part as a projected surface decal ([`crate::ground_fx`]) riding
/// its joint, so Battle Shout's crescents drape sloped terrain instead of being buried by it.
/// `host` says where the instance sits in the model graph ([`EffectHost`]) — which decides its
/// emitters' attach frame, whose render alpha they inherit, and what becomes of them when the
/// instance goes. Returns `false` while the model's parts are still building — call again next
/// frame.
#[allow(clippy::too_many_arguments)]
pub(super) fn attach_effect_visuals(
    commands: &mut Commands,
    root: Entity,
    dm: &DisplayModel,
    now: f32,
    ground_anchor: bool,
    host: EffectHost,
    wow_materials: &mut Assets<WowModelMaterial>,
    tint_reg: &mut FxTintAnims,
    ibps: &Assets<bevy::mesh::skinning::SkinnedMeshInverseBindposes>,
    palettes: &mut crate::rig_palette::RigPalettes,
    preferred_anim: Option<u16>,
) -> bool {
    let Some(parts) = dm.parts.as_ref() else {
        return false; // still loading — attach on a later pass
    };
    let attach = host.attached.then_some(root);
    // Chain this instance onto the model it hangs from, so its effects compose through the
    // parent's computed alpha the way `0x714000` does — a weapon's glow through the item, the
    // item through its wearer (decision 0833).
    if let Some(parent) = host.parent {
        commands
            .entity(root)
            .insert(crate::model_fade::ParentModel(parent));
    }
    // …and, from the same fact, what losing the owner means: an instance hung on another model is
    // part of that model's tree, so its pool is freed with it (the reference's dtor) instead of
    // being left to finish in world space — the ghost clouds a gear change used to strand at the
    // body. A free-standing instance (a missile) keeps the 0202 drain.
    let on_owner_loss = if host.parent.is_some() {
        crate::particles::OwnerLoss::Free
    } else {
        crate::particles::OwnerLoss::Drain
    };
    let is_ground_decal = |part: &EntityPart| ground_anchor && part.ground_quad.is_some();
    // Per-part materials for THIS instance (a tint clone where the RGB animates), resolved
    // before the child-spawn closure so the material assets aren't borrowed inside it.
    // Ground-decal parts keep the shared handle unused — their identity rides the decal record.
    let part_materials: Vec<Handle<WowModelMaterial>> = parts
        .iter()
        .map(|p| {
            if is_ground_decal(p) {
                p.material.clone()
            } else {
                fx_part_material(p, now, wow_materials, tint_reg)
            }
        })
        .collect();
    let joints = arm_effect_rig(commands, root, dm, preferred_anim);
    // The owned palette rig (decision 0720): allocated when the effect draws skinned parts; the
    // hook frees the slot when the instance despawns (impact reap, missile arrival).
    let rig_slot = match (&dm.inverse_bindposes, joints.is_empty()) {
        (Some(ibp), false) => {
            crate::rig_palette::RigSkin::allocate(palettes, joints.clone(), ibp.clone()).map_or(
                0,
                |rig| {
                    let slot = rig.slot;
                    commands.entity(root).insert(rig);
                    slot
                },
            )
        }
        _ => 0,
    };
    let rigged = rig_slot != 0;
    // The clip this instance runs (the missile's InFlight, else the file-order-first) — the same
    // pick `arm_effect_rig` armed. Its **file sequence slot** is the key into each batch's
    // per-sequence material-alpha loops: an effect that plays a non-first sequence must read that
    // sequence's authored batch visibility, not sequence 0's.
    let played = dm
        .animations
        .as_ref()
        .and_then(|a| a.preferred_clip(preferred_anim));
    let played_seq = played.map(|c| c.seq_index);
    commands.entity(root).with_children(|children| {
        for (part, material) in parts.iter().zip(&part_materials) {
            if part.billboard.is_some() {
                continue; // spawned as a following card below (decision 0153)
            }
            if is_ground_decal(part) {
                continue; // spawned as a projected surface decal below
            }
            let mesh = match (rigged, &part.skinned_mesh) {
                (true, Some(sm)) => sm.clone(),
                _ => part.mesh.clone(),
            };
            let mut child = children.spawn((
                Mesh3d(mesh),
                MeshMaterial3d(material.clone()),
                Transform::default(),
                bevy::camera::visibility::NoFrustumCulling,
                ModelPart {
                    kind: ModelKind::Creature,
                    blend: part.blend,
                },
            ));
            if let (true, Some(_)) = (rigged, &part.skinned_mesh) {
                child.insert((
                    crate::rig_palette::RigPart(root),
                    bevy::mesh::MeshTag(
                        crate::mesh_tag::rig_bits(rig_slot) | crate::mesh_tag::alpha_bits(1.0),
                    ),
                ));
            }
            // The part's colour-alpha × weight loops, on this instance's clock: the sampler owns
            // the child's render-alpha tag (`drives_tag` — no other writer touches fx parts).
            // The rig field rides the whole-tag seed (decision 0720).
            if let Some(anim) = &part.alpha_anim {
                let mat_anim =
                    crate::doodad_anim::MatAnim::driving_tag(anim.clone(), now, played_seq);
                let rig_tag = if rigged && part.skinned_mesh.is_some() {
                    crate::mesh_tag::rig_bits(rig_slot)
                } else {
                    0
                };
                child.insert((
                    bevy::mesh::MeshTag(rig_tag | crate::mesh_tag::alpha_bits(mat_anim.current)),
                    mat_anim,
                ));
            }
        }
    });
    for (part, material) in parts.iter().zip(&part_materials) {
        let Some(info) = &part.billboard else {
            continue;
        };
        let card = match joints.get(info.bone as usize) {
            Some(&j) => crate::billboard::BillboardCard::following_joint(info, j),
            None => crate::billboard::BillboardCard::following(info, root),
        };
        let mut spawned = commands.spawn((
            Mesh3d(part.mesh.clone()),
            MeshMaterial3d(material.clone()),
            Transform::default(),
            ModelPart {
                kind: ModelKind::Creature,
                blend: part.blend,
            },
            card,
        ));
        // The card's build-time bound (decision 0834): `calculate_bounds` can no longer derive
        // one from the `RENDER_WORLD`-only static form's data.
        if let Some(aabb) = part.aabb {
            spawned.insert(aabb);
        }
        // A card shares its batch's material-alpha loops (the billboard split copies them).
        if let Some(anim) = &part.alpha_anim {
            let mat_anim = crate::doodad_anim::MatAnim::driving_tag(anim.clone(), now, played_seq);
            spawned.insert((
                bevy::mesh::MeshTag(crate::mesh_tag::alpha_bits(mat_anim.current)),
                mat_anim,
            ));
        }
    }
    // Ground-plane quad parts of a ground-anchored instance → projected surface decals
    // ([`crate::ground_fx`]): each rides its own joint (posed through the bone's inverse
    // bindpose, exactly the skinned-vertex path). The part's draw identity — texture, blend,
    // the shared material's baked `0x70baf0` fog policy, its RGB loop — rides the decal record
    // (0733); the alpha loop stays a `MatAnim` rider whose `current` the push samples.
    let binds = dm.inverse_bindposes.as_ref().and_then(|h| ibps.get(h));
    for part in parts.iter() {
        let Some(quad) = part.ground_quad.filter(|_| is_ground_decal(part)) else {
            continue;
        };
        let (joint, ibp) = match joints.get(quad.bone as usize) {
            Some(&j) => (
                j,
                binds
                    .and_then(|b| b.get(quad.bone as usize))
                    .copied()
                    .unwrap_or(Mat4::IDENTITY),
            ),
            None => (root, Mat4::IDENTITY), // boneless model: the quad rides the instance root
        };
        let (texture, fog) = match wow_materials.get(part.material.id()) {
            Some(mat) => (
                mat.base.base_color_texture.clone(),
                crate::particles::buffer::EffectFog::from_model_policy(
                    (mat.extension.clutter_fade.z as u32 >> 4) & 7,
                ),
            ),
            None => (None, crate::particles::buffer::EffectFog::Scene),
        };
        let Some(texture) = texture else {
            continue; // texture-less ground quad: nothing the stream could draw
        };
        let decal = crate::ground_fx::spawn_ground_fx_decal(
            commands,
            texture,
            crate::particles::buffer::EffectBlend::from_model(part.blend, part.additive),
            fog,
            part.rgb_anim.as_ref().map(|a| (a.clone(), now)),
            &quad,
            joint,
            ibp,
        );
        if let Some(anim) = &part.alpha_anim {
            let mat_anim = crate::doodad_anim::MatAnim::driving_tag(anim.clone(), now, played_seq);
            commands.entity(decal).insert(mat_anim);
        }
    }
    for em in &dm.emitters {
        let owner = joints
            .get(em.def.bone as usize)
            .map_or((root, [0.0; 3]), |&j| (j, em.bone_pivot));
        particles::spawn_emitter(
            commands,
            em,
            Transform::IDENTITY,
            particles::EmitterFrames {
                owner: Some(owner),
                attach,
                // The cloud anchors at the instance root — the emitter's bone only composes births
                // (the food sparkle's bone orbits; the risen stars must not swirl with it).
                anchor: Some(root),
                // Free with the model when this instance belongs to one, drain when it stands
                // alone (the impacting missile's trail — 0202's case). See `on_owner_loss` above.
                on_owner_loss,
                // This instance IS the model these particles belong to; its chain (set above)
                // carries the host's fade down to them — decision 0833.
                alpha: Some(root),
            },
            // The effect's rig arms ONE clip; the emitters' rate/enabled windows ride that slot
            // (a missile's InFlight is not file-order-first), on the instance's own spawn clock.
            played_seq.map_or(
                particles::EmitClock::Pinned,
                particles::EmitClock::PinnedSeq,
            ),
        );
    }
    // The same pick, as an `AnimationData` id — the ribbons' per-sequence visibility keys off it,
    // so the thrown weapon's trail shows in flight and never in the worn hand.
    let played_anim = played.map(|c| c.anim_id);
    for rb in &dm.ribbons {
        let (owner, use_pivot) = joints
            .get(rb.def.bone as usize)
            .map_or((root, false), |&j| (j, true));
        crate::ribbons::spawn_ribbon(
            commands,
            rb,
            owner,
            use_pivot,
            // An effect-model instance is armed unscaled — its emitters take the same
            // `Transform::IDENTITY` placement a few lines up, so both of a model's effect
            // families land on the same draw-order rung, which is the property that matters.
            1.0,
            played_anim,
            // This instance's own model alpha, chained to its host (0827/0833): a standalone
            // instance has none above it and draws exactly as before.
            Some(root),
        );
    }
    true
}

/// Arm an effect-model instance's **authored rig** under `root` (a missile entity / fx instance
/// root): the joint hierarchy, an `AnimationPlayer` on the file-order-first clip, and the
/// free-running global-sequence channels. Effect models pose their emitter/ribbon/mesh bones with
/// this rig — the fireball missile's constant bone keys turn its authored long-axis-up frame into
/// flight-forward and set its two trail ribbons 90° apart, and its global-sequence bone tumbles
/// the molten core. The first clip plays regardless of the doodad content gate
/// (`ModelAnimations::first_seq`, which skips constant-pose sequences as render-identical — true
/// for skinned MESHES at bind pose, but an effect's emitters read the posed joints, so the
/// constant keys are load-bearing here). Returns the joint entities; empty for a boneless model
/// (everything then rides `root` directly, as before).
pub(super) fn arm_effect_rig(
    commands: &mut Commands,
    root: Entity,
    dm: &DisplayModel,
    preferred_anim: Option<u16>,
) -> Vec<Entity> {
    if dm.skeleton.joints.is_empty() {
        return Vec::new();
    }
    let joints = super::spawn_joints(commands, root, root, &dm.skeleton);
    // Billboard bones face the camera at the PALETTE level, children inheriting (the frost-armor
    // sheets skin to a lock-Z bone's child) — the joint pass needs the map.
    if let Some(bb) = crate::billboard::BillboardJointRig::new(&dm.skeleton, &joints, root) {
        commands.entity(root).insert(bb);
    }
    if let Some(anims) = dm.animations.as_ref() {
        // A caller can name the sequence this instance should run (the thrown-weapon missile asks
        // for InFlight, so its authored spin plays and its ribbon's per-sequence visibility keys
        // ON); absent that — or when the model has no such sequence (an arrow, a fireball whose
        // tumble is a global sequence) — fall to the file-order-first clip, as before.
        if let Some(clip) = anims.preferred_clip(preferred_anim) {
            let mut player = AnimationPlayer::default();
            let play = player.play(clip.node);
            if clip.looping {
                play.repeat();
            }
            commands
                .entity(root)
                .insert((player, AnimationGraphHandle(anims.graph.clone())));
            for (i, &j) in joints.iter().enumerate() {
                commands
                    .entity(j)
                    .insert((bone_target_id(i as u16), AnimatedBy(root)));
            }
        }
        if let Some(drive) = crate::creature_anim::GlobalSeqDrive::new(&anims.global_bones, &joints)
        {
            commands.entity(root).insert(drive);
        }
    }
    joints
}

/// One live (or pending) effect-model instance on a unit.
struct FxInstance {
    /// The spell that owns it — the reap key (the client stores it at `CEffect+0x18`). The
    /// lootable-corpse sparkle reserves `u32::MAX` (`spell_visual::LOOT_FX_KEY` — not a spell).
    spell_id: u32,
    /// Precast/channel lifetime (reaped by spell id) vs cast-release (self-terminates).
    persistent: bool,
    /// Which owner's reap can kill a persistent instance (cast router vs aura watcher) — the
    /// client's reap-walk discriminator next to the spell id ([`FxClass`]).
    class: FxClass,
    /// The M2 attachment id to hang from ([`benilla_formats::KIT_SLOT_TAGS`]).
    tag: u16,
    /// The model-cache key.
    path: String,
    /// The spawned instance root (a child of the attach joint), `None` while the model loads.
    root: Option<Entity>,
    /// The self-termination deadline (`time.elapsed_secs()` clock), set at spawn for a
    /// non-persistent instance.
    expires: Option<f32>,
}

/// A unit's effect instances (the client's per-unit `+0xb4` effect list).
#[derive(Component, Default)]
pub(super) struct FxAttached {
    instances: Vec<FxInstance>,
}

/// Consume the router's [`SpellKitFx`] edges: `Begin` records instances (and creates their model
/// cache entries), `Reap` despawns the matching spell's persistent instances. Ordered per unit —
/// a GO's reap-then-begin lands in emission order, so the precast dies before the release flash
/// spawns.
pub(super) fn resolve_spell_fx(
    mut commands: Commands,
    mut events: MessageReader<SpellKitFx>,
    mut units: Query<&mut FxAttached>,
    fx: Option<ResMut<SpellFx>>,
    asset_server: Res<AssetServer>,
) {
    let Some(mut fx) = fx else { return };
    // Group the edges per unit, preserving order, so each unit's instance list is written once.
    let mut ops: EntityHashMap<Vec<&SpellKitFx>> = EntityHashMap::default();
    for ev in events.read() {
        let entity = match ev {
            SpellKitFx::Begin { entity, .. } | SpellKitFx::Reap { entity, .. } => *entity,
        };
        ops.entry(entity).or_default().push(ev);
    }
    for (entity, edges) in ops {
        // A unit may have despawned between the edge and this frame — drop its edges.
        if commands.get_entity(entity).is_err() {
            continue;
        }
        let existing = units.get_mut(entity).ok();
        let mut instances = match existing {
            Some(mut att) => std::mem::take(&mut att.instances),
            None => Vec::new(),
        };
        for edge in edges {
            match edge {
                SpellKitFx::Begin {
                    spell_id,
                    persistent,
                    class,
                    effects,
                    ..
                } => {
                    // A persistent Begin REPLACES the unit's live persistent instances of the
                    // same (spell, class) — despawn + re-begin at one sync point, no visible
                    // gap — so an aura whose remove/add edges land across frames (a re-apply)
                    // re-arms cleanly instead of stacking.
                    if *persistent {
                        instances.retain(|i| {
                            let dies = i.persistent && i.spell_id == *spell_id && i.class == *class;
                            if dies {
                                if let Some(root) = i.root {
                                    commands.entity(root).despawn();
                                }
                            }
                            !dies
                        });
                    }
                    for (tag, path) in effects {
                        fx.models
                            .entry(path.clone())
                            .or_insert_with(|| DisplayModel {
                                handle: ModelHandle::M2(asset_server.load(m2_url(path))),
                                ..super::empty_shell()
                            });
                        instances.push(FxInstance {
                            spell_id: *spell_id,
                            persistent: *persistent,
                            class: *class,
                            tag: *tag,
                            path: path.clone(),
                            root: None,
                            expires: None,
                        });
                    }
                }
                SpellKitFx::Reap {
                    spell_id, class, ..
                } => {
                    instances.retain(|i| {
                        let dies = i.persistent && i.spell_id == *spell_id && i.class == *class;
                        if dies {
                            if let Some(root) = i.root {
                                commands.entity(root).despawn();
                            }
                        }
                        !dies
                    });
                }
            }
        }
        match units.get_mut(entity) {
            Ok(mut att) => att.instances = instances,
            Err(_) => {
                commands.entity(entity).insert(FxAttached { instances });
            }
        }
    }
}

/// Spawn pending instances whose model finished building, and run the self-termination clock.
/// The instance root is a child of the attach joint at the attachment offset (the cascade above),
/// so the whole effect — meshes and particle emitters alike — rides the animating bone.
#[allow(clippy::too_many_arguments)]
pub(super) fn attach_spell_fx(
    mut commands: Commands,
    mut units: Query<(Entity, &mut FxAttached, Option<&BoneAttach>)>,
    fx: Option<Res<SpellFx>>,
    time: Res<Time>,
    mut wow_materials: ResMut<Assets<WowModelMaterial>>,
    mut tint_reg: ResMut<FxTintAnims>,
    ibps: Res<Assets<bevy::mesh::skinning::SkinnedMeshInverseBindposes>>,
    mut palettes: ResMut<crate::rig_palette::RigPalettes>,
) {
    let Some(fx) = fx else {
        return;
    };
    let now = time.elapsed_secs();
    for (unit, mut att, bones) in &mut units {
        att.instances.retain_mut(|inst| {
            // Self-termination (the cast-release flash ran its span).
            if let Some(expires) = inst.expires {
                if now >= expires {
                    if let Some(root) = inst.root {
                        commands.entity(root).despawn();
                    }
                    if crate::dbg_trace::enabled() {
                        crate::dbg_trace::line(
                            "fx",
                            &format!("kit expire unit={unit} path={}", inst.path),
                        );
                    }
                    return false;
                }
            }
            if inst.root.is_some() {
                return true;
            }
            // Still pending: a self-terminating instance gets a generous deadline so a unit whose
            // model never materialises (the cube fallback) can't accumulate unspawnable flashes —
            // persistent ones are reaped by their spell edge regardless. A successful spawn below
            // overwrites this with the real clip span.
            if !inst.persistent && inst.expires.is_none() {
                inst.expires = Some(now + PENDING_TIMEOUT);
            }
            let Some(dm) = fx.models.get(&inst.path) else {
                return true; // cache entry pending (shouldn't happen — created with the instance)
            };
            if dm.parts.is_none() {
                return true; // model still loading — spawn on a later pass
            }
            // The attach cascade: the slot's tag, the client's two fallbacks, else the unit root.
            let point = bones.and_then(|b| {
                std::iter::once(inst.tag)
                    .chain(ATTACH_FALLBACKS)
                    .find_map(|tag| b.points.get(&tag).copied().map(|p| (tag, p)))
                    .and_then(|(tag, (bone, offset))| {
                        b.anchor(bone).map(|joint| (tag, joint, offset))
                    })
            });
            // Ground-anchored: the cascade landed on the model's BASE point (`0x13`) or fell
            // through to the unit root — the two feet-level anchors. A hand/head/chest-anchored
            // instance keeps its flat quads as ordinary geometry (the ProtectionFrom* chest
            // shields author flat quads too — they must NOT decal to the ground).
            let ground_anchor = point.is_none_or(|(tag, ..)| tag == 0x13);
            let (parent, offset) = point.map_or((unit, Vec3::ZERO), |(_, j, o)| (j, o));
            let root = commands
                .spawn((Transform::from_translation(offset), Visibility::default()))
                .id();
            commands.entity(parent).add_child(root);
            // The one shared effect-visuals body — rig, skinned parts, joint-riding
            // cards/emitters/ribbons, ground decals, material animation. Parts were checked
            // ready above, so this always attaches.
            attach_effect_visuals(
                &mut commands,
                root,
                dm,
                now,
                ground_anchor,
                // A kit instance on a unit is an attached model, chained to that unit: it fades
                // with the body it is cast on, and it is freed with it (0833) — where before, a
                // gear change that tore the unit's visual down left its cloud in the air.
                EffectHost {
                    attached: true,
                    parent: Some(unit),
                },
                &mut wow_materials,
                &mut tint_reg,
                &ibps,
                &mut palettes,
                None, // an attach-point kit effect runs its default (first) clip
            );
            inst.root = Some(root);
            if !inst.persistent {
                // The client's completion-callback moment: one full pass of the sequence the
                // instance arms — sequence 0, whether or not it moves a bone. Read from the raw
                // sequence table, NOT the built clips: a sequence with zero bone keys builds no
                // clip at all (the eat/drink tankard/sparkle models are exactly this), and the
                // old clips-only read collapsed their span to the 1 s fallback — the jug lived
                // 1 s of every 5 s kit resend instead of overlapping seamlessly. INFERRED at the
                // loop boundary: a looping sequence is counted as one pass (a stage-0 effect that
                // never completed would outlive the drink; the ref's tankard does not).
                let span = dm.first_seq_span.unwrap_or(FALLBACK_SPAN);
                inst.expires = Some(now + span);
            }
            if crate::dbg_trace::enabled() {
                crate::dbg_trace::line(
                    "fx",
                    &format!(
                        "kit spawn unit={unit} path={} persistent={} span={:?}",
                        inst.path,
                        inst.persistent,
                        inst.expires.map(|e| e - now)
                    ),
                );
            }
            true
        });
    }
}

/// Fire the event keyframes each live effect instance's playing clip crossed since last frame
/// (module doc; decision 0304) — the same `(prev, cur]` window scan as the creature scanner
/// ([`scan_events`]), emitting with the HOST UNIT as the event entity (the unit's transform is
/// the world position; an instance root's own is joint-local). Unlike a streamed creature, an
/// instance is born under our eyes at t = 0, so first sight fires the head window `[0, cur]` —
/// the level-up pillar's `$SND(888)` sits at 0.033 s and depends on it.
pub(super) fn fire_fx_anim_events(
    units: Query<(Entity, &FxAttached)>,
    players: Query<&AnimationPlayer>,
    fx: Option<Res<SpellFx>>,
    mut last: Local<EntityHashMap<f32>>,
    mut seen: Local<Vec<Entity>>,
    mut out: MessageWriter<AnimSoundEvent>,
) {
    let Some(fx) = fx else { return };
    seen.clear();
    for (unit, att) in &units {
        for inst in &att.instances {
            let Some(root) = inst.root else { continue };
            let Some(clip) = fx
                .models
                .get(&inst.path)
                .and_then(|dm| dm.animations.as_ref())
                .and_then(|a| a.clips.first())
                .filter(|c| !c.events.is_empty())
            else {
                continue;
            };
            let Ok(player) = players.get(root) else {
                continue;
            };
            let Some(active) = player.animation(clip.node) else {
                continue;
            };
            let cur = active.seek_time();
            seen.push(root);
            let prev = last.insert(root, cur).unwrap_or(-1.0);
            // The emit entity is decoupled from the scanned track's owner by design — the fx
            // scan fires at the unit, not the joint-local instance root.
            scan_events(clip, unit, prev, cur, &mut out);
        }
    }
    // Roots despawn on reap/expiry — drop their seek memory with them.
    if last.len() > seen.len() {
        let live: bevy::ecs::entity::EntityHashSet = seen.iter().copied().collect();
        last.retain(|e, _| live.contains(e));
    }
}
