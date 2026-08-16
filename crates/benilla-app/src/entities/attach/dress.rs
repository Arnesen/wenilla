//! **Dressing** a unit's model parts — the per-batch spawn [`super::attach_entity_visuals`] runs
//! once per visual, factored out so the **re-dress** ([`super::redress`]) can run the identical law
//! over the parts a unit is already standing in.
//!
//! The reference never rebuilds a character to change what it is wearing: the composite atlas is
//! re-blitted into the component's own 256² target and the geosets are re-selected on the SAME
//! `CM2Model` (its per-instance visibility array `+0x98`, whose only writer is the character
//! compositor — wow-re `charactermodel.md` "Assembly orchestration" + `models.md`
//! §"geoset-visibility-default"). Its attachments — held items, the helm, the shoulders and
//! everything hanging off them — are never touched by that path at all. This module is the half of
//! that law that concerns one part: which materials it draws through, and how it is born.

use benilla_formats::CharSkinSlot;
use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::MeshTag;
use bevy::prelude::*;

use benilla_assets::materials::WowModelMaterial;
use benilla_world::billboard::BillboardCard;
use benilla_world::interact::WorldObject;
use benilla_world::interior::part_interior_lit;
use benilla_world::model_fade::{FadeSet, JoinedFade, PartFade};
use benilla_world::model_render::{ModelKind, ModelPart};

use super::super::EntityPart;
use super::char_skin::CharSkinMaterials;

/// The display-model batch a spawned part child was built from — its index into
/// [`DisplayModel::parts`](super::super::DisplayModel), plus the world-root billboard card that
/// belongs to it when the batch is a camera-facing one (decision 0153: the card is a ROOT entity,
/// so it does not cascade with its anchor's despawn and must be named here).
///
/// This is the **re-dress edge** ([`super::redress`]): a gear change re-selects the geoset set and
/// re-composites the atlas, and each standing part is found again through this index — its geoset
/// id for "does it still show?", its char slot for "which atlas does it draw?". The index is stable
/// for as long as the visual lives, because the only thing that can rebuild `parts` is the display
/// itself changing, which is a teardown (`live_display::refresh_live_display`).
#[derive(Component, Clone, Copy)]
pub(in crate::entities) struct DressedPart {
    pub(in crate::entities) index: u32,
    pub(in crate::entities) card: Option<Entity>,
}

/// The material variants one part draws through, after the per-appearance character swap: the
/// steady exterior material, its interior-matte / interior-bake law twins, and the `Blend` twins the
/// appear/despawn ramps feather on.
///
/// One function answers it for both the spawn and the re-dress, which is the point: "which material
/// does this batch wear?" has exactly one implementation, so a gear change can never dress a body
/// differently from the way it was first built.
pub(super) struct PartMaterials<'a> {
    pub(super) steady: &'a Handle<WowModelMaterial>,
    pub(super) interior: Option<&'a Handle<WowModelMaterial>>,
    pub(super) fade_blend: Option<&'a Handle<WowModelMaterial>>,
    pub(super) bake: Option<&'a Handle<WowModelMaterial>>,
    pub(super) bake_blend: Option<&'a Handle<WowModelMaterial>>,
    /// The depth-prime twin (decision 0831) — carried per appearance like the rest, since its
    /// cutout discard samples the same swapped texture.
    pub(super) zfill: Option<&'a Handle<WowModelMaterial>>,
}

/// Select `part`'s materials: on a character-slot batch the per-appearance variants over the
/// composited body atlas / hair / cape / extra-skin textures (decisions 0041 / 0044 / 0045) — the
/// body and the extra skin at the batch's own sidedness, since a robe skirt is authored two-sided
/// and the closed body is not — and on every other batch the ones the shared model was built with.
pub(super) fn part_materials<'a>(
    part: &'a EntityPart,
    char_mats: &'a CharSkinMaterials,
) -> PartMaterials<'a> {
    let slot_mats = match part.char_slot {
        Some(CharSkinSlot::Body) => {
            char_mats
                .0
                .as_ref()
                .map(|(single, two)| if part.two_sided { two } else { single })
        }
        Some(CharSkinSlot::Hair) => char_mats.1.as_ref(),
        Some(CharSkinSlot::Object) => char_mats.2.as_ref(),
        Some(CharSkinSlot::SkinExtra) => {
            let (single, two) = &char_mats.3;
            if part.two_sided { two } else { single }.as_ref()
        }
        None => None,
    };
    match slot_mats {
        Some((ext, int, fade, bake, bake_blend, zfill)) => PartMaterials {
            steady: ext,
            interior: Some(int),
            fade_blend: Some(fade),
            bake: Some(bake),
            bake_blend: Some(bake_blend),
            zfill: zfill.as_ref(),
        },
        None => PartMaterials {
            steady: &part.material,
            interior: part.material_interior.as_ref(),
            fade_blend: part.fade_blend.as_ref(),
            bake: part.material_interior_bake.as_ref(),
            bake_blend: part.material_interior_bake_blend.as_ref(),
            zfill: part.zfill.as_ref(),
        },
    }
}

impl<'a> PartMaterials<'a> {
    /// The three handles the appear/despawn ramps feather through — this batch's slice of
    /// [`FadeSet`], so the spawn and the fade can never disagree about which twin it wears.
    pub(super) fn fade_set(&self) -> FadeSet<'a> {
        FadeSet {
            steady: self.steady,
            blend: self.fade_blend,
            bake_blend: self.bake_blend,
            zfill: self.zfill,
        }
    }
}

/// Everything one part's spawn reads from its UNIT — gathered once per visual so the per-part body
/// isn't a twenty-argument call, and so the spawn path and the re-dress path provably feed it the
/// same things.
pub(super) struct PartDress<'a> {
    /// The unit whose model this is: the parent, the interior classifier's anchor, the animation
    /// clock every `MatAnim` follows.
    pub(super) unit: Entity,
    pub(super) kind: ModelKind,
    pub(super) char_mats: &'a CharSkinMaterials,
    /// Identity for the mouseover inspector, cloned onto every part and card.
    pub(super) object: &'a WorldObject,
    /// `0` = the palette table was full: parts fall back to the static bind-pose mesh.
    pub(super) inst_slot: u16,
    /// Whether the unit built a rig at all — the gate on a part drawing its skinned twin.
    pub(super) rigged: bool,
    /// Bone index → anchor entity, for a billboard batch's card to ride its live joint. Resolved
    /// by the CALLER through `RigPose::anchor_for` for exactly the card bones this dress will
    /// spawn (decision 1355) — owned, because the resolver needs `&mut` on the pose and this
    /// context is shared immutably across every part.
    pub(super) anchors: std::collections::HashMap<u16, Entity>,
    /// The model's interior fold reference (model-local) — one verdict per unit, so a body can
    /// never split across the two light laws.
    pub(super) bake_center: Vec3,
    /// The armed idle's authored CAaBox — the mouseover picker's volume for a skinned part.
    pub(super) idle_aabb: Option<Aabb>,
    /// `Time::elapsed_secs`, for a part joining a ramp already in progress.
    pub(super) now: f32,
    /// What this part should do about the unit's appear-fade. A fresh visual passes
    /// `Pending { since: now }` (decision 0032); a part spawned by a **re-dress** JOINS whatever the
    /// unit's own clock says ([`benilla_world::model_fade::join_unit_appear_fade`]), exactly as a
    /// late-resolving held item does — so a gear change during the login cascade neither pops the
    /// new geoset opaque over a still-feathering body nor restarts a second ramp beside it.
    pub(super) fade: JoinedFade,
}

/// Spawn one part of `dress.unit`'s model as a child of it, dressed by [`part_materials`]. Returns
/// whether it armed an appear-fade — the attach path mirrors that onto the unit root so a held item
/// resolving later can join the same ramp.
///
/// A **billboard** batch (a glow card / chain) can't spawn as an ordinary child: its mesh is centred
/// at the bone pivot and its transform belongs to the billboard system, so as a plain child it would
/// render at the model origin (the "glow on the ground" family, decision 0153). It spawns as a
/// lightweight mirror anchor under the unit plus a world-root card following the anchor's live joint.
pub(super) fn spawn_part(
    commands: &mut Commands,
    part: &EntityPart,
    index: usize,
    dress: &PartDress,
) -> bool {
    if let Some(info) = &part.billboard {
        return spawn_billboard_part(commands, part, index, info, dress);
    }
    let mats = part_materials(part, dress.char_mats);
    let set = mats.fade_set();
    // A freshly-streamed CGObject appear-fades in (decision 0032): spawn already on the blend twin
    // with a ≈0 `MeshTag`, so it doesn't flash opaque for a frame before `apply_render_fade` ramps
    // `α = t³`. A joiner computes the ramp's *current* alpha rather than 0, so it doesn't flash
    // invisible for a frame either.
    let effective = PartFade::resolve(dress.fade, &set);
    let (init_mat, tag_alpha) = effective.seed(&set, dress.now);
    // A skinned creature part draws its skinned-mesh twin (the WOW joint attributes → the
    // owned-palette `WOW_RIG_SKIN` shader path, decision 0720); everything else — and the
    // palette-full fallback (slot 0) — the static mesh. Keyed on the part having a twin, which the
    // slot alone no longer implies.
    let skinned = dress.inst_slot != 0 && part.skinned_mesh.is_some();
    let mesh = match &part.skinned_mesh {
        Some(sm) if skinned => sm.clone(),
        _ => part.mesh.clone(),
    };
    let mut child = commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(init_mat),
        Transform::default(),
        ChildOf(dress.unit),
        ModelPart {
            kind: dress.kind,
            blend: part.blend,
        },
        // The portrait booth mirrors this part ([`crate::portrait`]): both mesh twins — the booth
        // poses the skinned twin at Stand on its own throwaway skeleton (the ref bake, wow-re §4
        // D2), falling back to the static bind-pose twin for a boneless model — + the steady
        // exterior material (not the appear-fade/interior variant the child may wear now).
        crate::portrait::PortraitPart {
            static_mesh: part.mesh.clone(),
            skinned_mesh: part.skinned_mesh.clone(),
            material: mats.steady.clone(),
        },
        dress.object.clone(),
        // The picker's triangles (decision 0857): the render meshes are `RENDER_WORLD`-only, so
        // the ray pickers read the model's resident geometry instead.
        benilla_world::interact::PickMesh(part.geometry.clone()),
        DressedPart {
            index: index as u32,
            card: None,
        },
    ));
    // Every part gets a tag: the instance slot plus the live alpha field. Unconditional because the
    // slot is a per-instance identity every part needs (the tint) rather than a skinning detail —
    // and because the two conditional writers that used to seed it (the interior classifier, the
    // animated-alpha compose) each covered only their own subset, leaving a part in neither with no
    // tag at all.
    child.insert(MeshTag(benilla_world::mesh_tag::spawn_tag(
        dress.inst_slot,
        tag_alpha,
    )));
    // `RigPart` stays gated on actually being skinned by that rig — it is the CPU-side link for the
    // mouseover picker's skinned ray test (decision 0720), which has nothing to say about a static
    // part.
    if skinned {
        child.insert(benilla_world::rig_palette::RigPart(dress.unit));
    }
    if dress.rigged && part.skinned_mesh.is_some() {
        // A streamed entity's M2 is **never view-culled** — the reference registers entity render
        // records with effectively-infinite bounds and the one frustum cull in its machinery is
        // map-doodad-only (wow-re `unit-anim-visibility-gate.md` §2/§4). Bevy's bind-pose `Aabb` is
        // the wrong stand-in (the duel flag plants itself 9 yd below it) and is stomped anyway by
        // `calculate_bounds` on any `Mesh3d` change. So the renderer gets the faithful
        // `NoFrustumCulling`, and the `Aabb` beside it serves ONE master: the mouseover picker
        // (`target/hover.rs`) — the armed idle's authored CAaBox when it has one, else the bind
        // box, read from the part's build-time bound (decision 0834: the static mesh is
        // `RENDER_WORLD`-only, so its main-world data — which the old `compute_aabb` fallback
        // read — is gone after extract). Both `calculate_bounds` queries skip `NoFrustumCulling`
        // entities, so this box survives.
        let picker_aabb = dress.idle_aabb.or(part.aabb);
        if let Some(aabb) = picker_aabb {
            child.insert((aabb, NoFrustumCulling));
        }
    } else if let Some(aabb) = part.aabb {
        // A STATIC part (a boneless GameObject model, a WMO-display batch) keeps Bevy's ordinary
        // frustum cull — but its bind-pose box must be inserted here now (decision 0834):
        // `calculate_bounds` used to derive it from the mesh's main-world data, which the
        // `RENDER_WORLD`-only static form no longer has after extract.
        child.insert(aabb);
    }
    // M2 parts can light off a WMO room they stand in: a `MeshTag` + the classifier pick the law by
    // location (0354). Anchored at the unit root so every part shares the root's verdict, and the
    // indoor LAW is one for every entity M2 — the footprint-MOCV bake (wow-re
    // `unit-m2-shader-light.md`), with the matte ×1.0 as the bake's miss fallback.
    if let Some(lit) = part_interior_lit(
        mats.steady,
        mats.interior,
        mats.bake,
        dress.bake_center,
        dress.unit,
    ) {
        child.insert(lit);
    }
    // The batch's **animated material alpha** (the verified combine's runtime half, wow-re
    // `m2-alpha-combine-cull.md`): a creature's colour-alpha/transparency tracks are authored PER
    // SEQUENCE, so which of its batches draw is a function of what it is playing. Sampling follows
    // the unit's own `AnimationPlayer`, so the alpha stays in phase with the pose.
    if let Some(anim) = &part.alpha_anim {
        child.insert(benilla_world::doodad_anim::MatAnim::following(
            anim.clone(),
            dress.unit,
        ));
    }
    effective.dress(&mut child, &set)
}

/// The camera-facing half of [`spawn_part`]: a mirror anchor under the unit (so the portrait /
/// paper-doll booths can rebuild the batch — the visible card is a world ROOT and can never be
/// mirrored) plus the card itself, following the batch's live joint when the unit is rigged and the
/// anchor otherwise. Returns whether the card armed the unit's appear-fade, exactly as a mesh part
/// does: the batch is the model's, so it fades with the model (decision 0836).
fn spawn_billboard_part(
    commands: &mut Commands,
    part: &EntityPart,
    index: usize,
    info: &benilla_assets::BillboardInfo,
    dress: &PartDress,
) -> bool {
    let anchor = commands
        .spawn((
            Transform::default(),
            Visibility::default(),
            ChildOf(dress.unit),
            crate::portrait::PortraitBillboard {
                mesh: part.mesh.clone(),
                material: part.material.clone(),
                bone: info.bone,
                // A rigged host's billboard bone already bakes the pivot into its booth joint (the
                // 0130 rig identity) — and the batch belongs to the host's body, so a mount's own
                // glow card prunes with the mount (`DressedLook::collect`).
                seat: crate::portrait::PortraitSeat::Body,
                kind: info.kind,
            },
        ))
        .id();
    let (owner, at_joint) = match dress.anchors.get(&info.bone).copied() {
        Some(j) => (j, true),
        None => (anchor, false),
    };
    let card_follow = if at_joint {
        BillboardCard::following_joint(info, owner)
    } else {
        BillboardCard::following(info, owner)
    };
    // A card is a batch of the unit's own model, so it joins the unit's appear-fade like any other
    // batch of it — the reference has ONE instance alpha per model and every batch draws through it
    // (decision 0836). Splitting the batch into a world-root entity is benilla's parenting detail;
    // it is not a reason for the glow on a weapon's gems to blaze at full strength over a body that
    // has not appeared yet.
    let set = FadeSet {
        steady: &part.material,
        blend: part.fade_blend.as_ref(),
        bake_blend: part.material_interior_bake_blend.as_ref(),
        zfill: part.zfill.as_ref(),
    };
    let effective = PartFade::resolve(dress.fade, &set);
    let (init_mat, tag_alpha) = effective.seed(&set, dress.now);
    let mut card = commands.spawn((
        Mesh3d(part.mesh.clone()),
        MeshMaterial3d(init_mat),
        Transform::default(),
        ModelPart {
            kind: dress.kind,
            blend: part.blend,
        },
        dress.object.clone(),
        // The picker's triangles (decision 0857) — the caster centres a card at its pivot, the
        // same bake the render form draws with.
        benilla_world::interact::PickMesh(part.geometry.clone()),
        card_follow,
    ));
    // A card takes its MODEL's indoor law through the same constructor as the sibling meshes it was
    // split out of (decision 0778), and carries the unit's INSTANCE slot like every sibling part
    // even though it is never skinned by it — it is a batch of the unit's own model, so a tinted
    // unit tints its eye-glow and torch cards too (decision 0812). No char-slot variants are
    // consulted: a body/hair/cape/skin-extra batch is never a billboard batch.
    card.insert(MeshTag(benilla_world::mesh_tag::spawn_tag(
        dress.inst_slot,
        tag_alpha,
    )));
    // The card's build-time bound (decision 0834) — `calculate_bounds` can no longer derive one
    // from the `RENDER_WORLD`-only static form's data.
    if let Some(aabb) = part.aabb {
        card.insert(aabb);
    }
    if let Some(lit) = part_interior_lit(
        &part.material,
        part.material_interior.as_ref(),
        part.material_interior_bake.as_ref(),
        dress.bake_center,
        dress.unit,
    ) {
        card.insert(lit);
    }
    // A card shares its batch's per-sequence alpha loops (the billboard split copies them onto every
    // group), sampled off the same unit clock as the mesh parts.
    if let Some(anim) = &part.alpha_anim {
        card.insert(benilla_world::doodad_anim::MatAnim::following(
            anim.clone(),
            dress.unit,
        ));
    }
    let armed = effective.dress(&mut card, &set);
    let card = card.id();
    // The card follows the JOINT when the unit is rigged, so it does not cascade with the anchor's
    // despawn — the re-dress has to name it to reap it (see [`DressedPart::card`]).
    commands.entity(anchor).insert(DressedPart {
        index: index as u32,
        card: Some(card),
    });
    armed
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_world::model_fade::{FadeMaterials, PendingAppearFade};

    /// One synthetic batch at `char_slot`/`two_sided`, carrying its own (distinguishable) built
    /// material set so the fallback arm is visible in the assertions.
    fn part(char_slot: Option<CharSkinSlot>, two_sided: bool) -> EntityPart {
        EntityPart {
            mesh: Handle::default(),
            geometry: std::sync::Arc::new(benilla_formats::RenderSubmesh::default()),
            aabb: None,
            skinned_mesh: None,
            welded_billboard: false,
            material: mat(0),
            material_interior: None,
            material_interior_bake: None,
            material_interior_bake_blend: None,
            fade_blend: None,
            zfill: None,
            blend: benilla_formats::ModelBlend::Opaque,
            additive: false,
            two_sided,
            geoset_id: 0,
            char_slot,
            billboard: None,
            alpha_anim: None,
            rgb_anim: None,
            ground_quad: None,
        }
    }

    /// A distinguishable material handle — a stable-id (`Uuid`) handle, which needs no `Assets`
    /// store behind it and compares by identity like any other.
    fn mat(n: u128) -> Handle<WowModelMaterial> {
        Handle::Uuid(
            bevy::asset::uuid::Uuid::from_u128(n + 1),
            std::marker::PhantomData,
        )
    }

    /// A set of six distinguishable handles, so which variant landed where is checkable.
    fn quint(seed: u128) -> super::super::char_skin::MatQuint {
        (
            mat(seed * 8),
            mat(seed * 8 + 1),
            mat(seed * 8 + 2),
            mat(seed * 8 + 3),
            mat(seed * 8 + 4),
            Some(mat(seed * 8 + 5)),
        )
    }

    /// The character swap picks a slot's quint — and the BODY (and extra skin) pick by the batch's
    /// own sidedness, because a robe skirt (geoset 1302) is authored two-sided while the closed body
    /// is not; flattening them to one variant culled the robe's inner faces (see-through from below).
    #[test]
    fn a_character_batch_takes_its_slots_variants_at_its_own_sidedness() {
        let single = quint(1);
        let two = quint(2);
        let hair = quint(3);
        let mats: CharSkinMaterials = (
            Some((single.clone(), two.clone())),
            Some(hair.clone()),
            None,
            (None, None),
        );

        let body_single = part(Some(CharSkinSlot::Body), false);
        assert_eq!(*part_materials(&body_single, &mats).steady, single.0);
        let body_two = part(Some(CharSkinSlot::Body), true);
        assert_eq!(*part_materials(&body_two, &mats).steady, two.0);

        let hair_part = part(Some(CharSkinSlot::Hair), false);
        let m = part_materials(&hair_part, &mats);
        assert_eq!(*m.steady, hair.0);
        assert_eq!(m.interior, Some(&hair.1));
        assert_eq!(m.fade_blend, Some(&hair.2));
        assert_eq!(m.bake, Some(&hair.3));
        assert_eq!(m.bake_blend, Some(&hair.4));
    }

    /// **`FadeMaterials` is a material RECORD, not a record of having armed a fade.** It used to be
    /// inserted only alongside the appear-fade arm, so a part spawned by anything that isn't a spawn
    /// — which, before decision 0835, meant a body rebuilt on *every gear change* — carried none at
    /// all. Two things read it and both silently stopped working for a player who had swapped so
    /// much as a shirt: the stream-out fade (`arm_despawn_descendants` gates on it) and, worse, the
    /// self-avatar first-person feather, whose query takes `&FadeMaterials` outright — so your own
    /// body stayed solid in your face. The item lane always did it this way; the body lane does now.
    #[test]
    fn a_steady_spawn_still_records_its_fade_materials() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>();
        let unit = app.world_mut().spawn(Transform::default()).id();
        let mut fadeable = part(None, false);
        fadeable.fade_blend = Some(mat(99));
        let anchors: std::collections::HashMap<u16, Entity> = std::collections::HashMap::new();
        let object = WorldObject {
            kind: ModelKind::Creature,
            label: String::new(),
            id: 0,
            detail: String::new(),
        };
        let empty: CharSkinMaterials = (None, None, None, (None, None));
        let dress = PartDress {
            unit,
            kind: ModelKind::Creature,
            char_mats: &empty,
            object: &object,
            inst_slot: 0,
            rigged: false,
            anchors,
            bake_center: Vec3::ZERO,
            idle_aabb: None,
            now: 0.0,
            fade: JoinedFade::Steady,
        };
        let mut queue = bevy::ecs::world::CommandQueue::default();
        let armed = {
            let world = app.world();
            let mut commands = Commands::new(&mut queue, world);
            spawn_part(&mut commands, &fadeable, 0, &dress)
        };
        queue.apply(app.world_mut());
        assert!(!armed, "steady: no ramp armed");
        let mut q = app
            .world_mut()
            .query::<(&FadeMaterials, Has<PendingAppearFade>)>();
        let found: Vec<_> = q
            .iter(app.world())
            .map(|(fm, p)| (fm.blend.clone(), p))
            .collect();
        assert_eq!(found.len(), 1, "the record is kept even with no ramp");
        assert_eq!(found[0].0, mat(99), "…and names the part's own blend twin");
        assert!(!found[0].1, "…without arming an appear fade");
    }

    /// **A camera-facing batch fades with its model** (decision 0836): the split into a world-ROOT
    /// card is benilla's parenting detail (0153), not a law — the reference has one instance alpha
    /// per `CM2Model` and every batch draws through it, billboard batches included. The card used
    /// to open at a flat opaque with no arm and no record, which is a night-elf's eye glow and an
    /// undead's shoulder wisps burning at full strength over a body still at α 0.
    ///
    /// Asserted on the CARD, not the anchor: the anchor is a bare mirror carrier with no geometry.
    #[test]
    fn a_billboard_card_joins_the_units_appear_fade() {
        const SINCE: f32 = 5.0;
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>();
        let unit = app.world_mut().spawn(Transform::default()).id();
        let mut card = part(None, false);
        card.fade_blend = Some(mat(99));
        card.billboard = Some(benilla_assets::BillboardInfo {
            pivot: Vec3::ZERO,
            bone: 1,
            kind: benilla_formats::BillboardKind::Spherical,
            scale_anim: None,
            seq_translations: Vec::new(),
        });
        let anchors: std::collections::HashMap<u16, Entity> = std::collections::HashMap::new();
        let object = WorldObject {
            kind: ModelKind::Creature,
            label: String::new(),
            id: 0,
            detail: String::new(),
        };
        let empty: CharSkinMaterials = (None, None, None, (None, None));
        let dress = PartDress {
            unit,
            kind: ModelKind::Creature,
            char_mats: &empty,
            object: &object,
            inst_slot: 0,
            rigged: false,
            anchors,
            bake_center: Vec3::ZERO,
            idle_aabb: None,
            now: 0.0,
            fade: JoinedFade::Pending { since: SINCE },
        };
        let mut queue = bevy::ecs::world::CommandQueue::default();
        let armed = {
            let world = app.world();
            let mut commands = Commands::new(&mut queue, world);
            spawn_part(&mut commands, &card, 0, &dress)
        };
        queue.apply(app.world_mut());
        assert!(
            armed,
            "a card arms the ramp like a mesh part — the attach path mirrors that onto the root",
        );
        let mut q = app.world_mut().query::<(
            &BillboardCard,
            &MeshTag,
            &MeshMaterial3d<WowModelMaterial>,
            &FadeMaterials,
            &PendingAppearFade,
        )>();
        let found: Vec<_> = q
            .iter(app.world())
            .map(|(_, t, m, fm, p)| (t.0, m.0.clone(), fm.blend.clone(), p.since))
            .collect();
        assert_eq!(found.len(), 1, "one billboard batch, one card");
        assert_eq!(found[0].3, SINCE, "joined the unit's own pending clock");
        assert_eq!(
            found[0].2,
            mat(99),
            "the record names the batch's blend twin"
        );
        assert_eq!(found[0].1, mat(99), "…and the card OPENS on it");
        assert!(
            benilla_world::mesh_tag::alpha_of(found[0].0) <= 1.0 / 63.0,
            "…at the encoder's ≈0 floor",
        );
    }

    /// A batch with no character slot — and a slot whose row the appearance didn't resolve (a bald
    /// style has no hair texture, only tauren author an extra skin) — keeps the shared model's own
    /// built materials. That is the arm a creature, a GameObject and an unresolvable look all take.
    #[test]
    fn an_unswapped_batch_keeps_the_models_own_materials() {
        let empty: CharSkinMaterials = (None, None, None, (None, None));
        let plain = part(None, false);
        assert_eq!(*part_materials(&plain, &empty).steady, plain.material);
        // A slot the look didn't fill falls to the same place, not to another slot's quint.
        let mats: CharSkinMaterials = (Some((quint(1), quint(2))), None, None, (None, None));
        let bald = part(Some(CharSkinSlot::Hair), false);
        assert_eq!(*part_materials(&bald, &mats).steady, bald.material);
    }
}
