//! Per-display model cache + build (decision 0006) — the front half of [`super`].
//!
//! Resolves a creature/GameObject/held-item display id to a [`DisplayModel`] (loading its M2/WMO
//! through the standard `AssetServer`, deduped by path) and, once the asset loads, builds its spawn
//! parts once — shared by every entity of that display. [`super::attach`] then gives each net entity
//! a visual from the built model.

use avian3d::prelude::Collider;
use benilla_assets::coords::wow_to_bevy;
use benilla_assets::{
    M2Model, ModelAnimations, ModelEmitter, ModelSkeleton, ModelSubmesh, WmoModel,
};
use benilla_formats::{
    CharSkinSlot, CollisionMesh, CreatureCatalog, GameObjectCatalog, ModelBlend, NpcAppearance,
};
use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::prelude::*;

use crate::model_render::{m2_url, model_material, skin_url, wmo_url, MaterialCache, ShadeSel};
use crate::terrain::WowModelMaterial;

/// A loaded display model's asset handle — an M2 (creatures, most GameObjects) or a WMO (building
/// GameObjects). `None` ⇒ the display resolved to no model (invisible trigger / missing) → cube/none.
pub(super) enum ModelHandle {
    M2(Handle<M2Model>),
    Wmo(Handle<WmoModel>),
    None,
}

/// One spawn part of a display model: a submesh's mesh + its built `WowModelMaterial` + blend (for the
/// `ModelPart` toggle). Built once the model asset loads **and** the model-forms furnisher has
/// built its render forms (decision 0834 — the handles here come from the app-side cache, not the
/// loader).
pub(super) struct EntityPart {
    pub(super) mesh: Handle<Mesh>,
    /// The batch's decoded geometry — the model's resident CPU copy (`ModelSubmesh::geometry`),
    /// cloned onto every spawned part/card as [`crate::interact::PickMesh`] so the ray pickers read
    /// triangles from here: the static render form is `RENDER_WORLD`-only (decision 0834), so no
    /// main-world mesh data exists to ray-cast (decision 0857).
    pub(super) geometry: std::sync::Arc<benilla_formats::RenderSubmesh>,
    /// The static form's build-time `Aabb` (decision 0834): the static mesh is `RENDER_WORLD`-only,
    /// so a consumer that used to compute a bound from its main-world data (the attach path's
    /// picker-volume fallback) reads this instead. `None` for degenerate geometry.
    pub(super) aabb: Option<bevy::camera::primitives::Aabb>,
    /// The skinned twin of [`Self::mesh`] (decision 0019), present for every M2 part. An animated
    /// instance (a creature, or a GameObject that runs the state machine / loops a loader-idle seq)
    /// renders this, skinned through its palette slot's rows (decision 0720); a truly static
    /// instance uses `mesh`. `None` for WMO-display parts (no skeleton).
    pub(super) skinned_mesh: Option<Handle<Mesh>>,
    pub(super) material: Handle<WowModelMaterial>,
    /// The interior-lit variant (M2 only): the same material in the plain day/night matte
    /// (`interior = false`, sun ×1.0 — the base-CGLight law), which the interior classifier
    /// ([`crate::interior`]) swaps to when the entity stands inside a WMO room. `None` for
    /// WMO-display entities (their interior is per-submesh, already baked into `material`).
    pub(super) material_interior: Option<Handle<WowModelMaterial>>,
    /// The interior BAKE variant (every M2 part): the material in interior-PROP mode, whose
    /// `MeshTag` payload the shader reads as an SH-probe slot — the footprint-MOCV law. Units and
    /// GameObjects alike consume it: the reference registers EVERY entity M2 with the same
    /// entity-node fill (`Node::SetModel`), so one law lights them all indoors (wow-re
    /// `unit-m2-shader-light.md`, superseding the 0315 unit/GO split). `None` for WMO parts.
    pub(super) material_interior_bake: Option<Handle<WowModelMaterial>>,
    /// The bake variant's own `AlphaMode::Blend` twin — a fade (the self-avatar zoom feather, a
    /// despawn ramp) on a bake-classified part rides THIS, keeping the probe light through the
    /// feather instead of jumping to the exterior twin's lit-outdoor intensity (0355).
    pub(super) material_interior_bake_blend: Option<Handle<WowModelMaterial>>,
    /// The `AlphaMode::Blend` twin of the exterior material, for the spawn appear-fade ([`RenderFade`]):
    /// the entity feathers in on this while `α < 1`, then swaps back to `material`. `Some` for M2 parts
    /// (creatures + M2 GameObjects, the CGObjects that appear-fade); `None` for WMO-display parts.
    pub(super) fade_blend: Option<Handle<WowModelMaterial>>,
    /// The **depth-prime twin** material ([`crate::model_render::zfill_material`] — the reference's
    /// `M2UseZFill` clone, wow-re `m2-blend-promotion-zfill.md` §4). While the part draws
    /// translucent, `model_fade::sync_zfill_twins` spawns a child mesh on this: colour-masked,
    /// blend-off, z-writing, sorted before the colour parts — one blended layer everywhere, no
    /// self-overlap darkening on a stealthed or fading body. `None` when the batch's material
    /// disables z-write/z-test (the reference's own `(flags & 0x10) == 0` twin gate) or cannot
    /// fade at all (Mod/Mod2x), and for WMO-display parts.
    pub(super) zfill: Option<Handle<WowModelMaterial>>,
    pub(super) blend: ModelBlend,
    /// Whether this part blends **additively** (M2 blend mode 3 `NoAlphaAdd` / 4 `Add`).
    /// [`ModelBlend`] deliberately folds "alpha-blended / additive" into its single `Blend`
    /// variant, so [`Self::blend`] alone CANNOT answer this — the material path recovers it from
    /// [`crate::model_render`]'s separate `is_additive` input (marker bit 2 of `clutter_fade.z`,
    /// which `specialize` turns into the pure-add state). Any consumer that re-derives a blend
    /// state from `blend` needs this alongside it, or every additive batch silently draws
    /// alpha-blended — decision 0748, the black ground-decal tile.
    pub(super) additive: bool,
    /// Whether this part renders two-sided (M2 material `0x04`). Retained so a per-appearance material
    /// swap (the character hair, decision 0045 — hair cards are two-sided) preserves it; `part.material`
    /// already bakes it for the un-swapped parts.
    pub(super) two_sided: bool,
    /// The part's `skinSectionId` (geoset / mesh-part ID). For a player body the per-entity attach
    /// filters parts by it (only the selected hair/facial/body geosets spawn); **unfiltered for
    /// everything else — which is correct, but NOT because those models have a single geoset.**
    ///
    /// Plenty of them do not: 101 creature models author several (`Creature\Banshee` carries `0`
    /// and `402`). They still draw whole, because the reference's per-model visibility array is
    /// allocated **filled with `1`** — all visible — and the only writer is the character
    /// compositor, which a creature never reaches (VERIFIED, wow-re `models.md` §"M2 geoset
    /// visibility": the `CCharacterComponent` is created on exactly two guarded paths and a
    /// creature fails both). That array is indexed by **submesh ordinal**, not by this id — the
    /// client never decomposes a `group*100 + variant` id anywhere in its render band.
    pub(super) geoset_id: u16,
    /// The character runtime texture slot this part carries (M2 type 1 = body, type 6 = hair). For a
    /// player the per-entity attach swaps its material to one carrying that per-appearance texture
    /// (decisions 0041 / 0044 / 0045); `None` and ignored otherwise (it keeps its built material).
    pub(super) char_slot: Option<CharSkinSlot>,
    /// A billboard batch (glow card / chain): its pivot + facing info. The spawn sites must NOT
    /// spawn it as an ordinary child — its mesh is centred at the bone pivot and its transform is
    /// owned by the billboard system (a following [`crate::billboard::BillboardCard`], decision
    /// 0153: the brazier/torch "glow on the ground" family). `None` for ordinary geometry.
    pub(super) billboard: Option<benilla_assets::BillboardInfo>,
    /// This part's geometry is **welded** to a billboard bone the card split refused
    /// ([`benilla_formats::RenderSubmesh::welded_billboard`], decision 0839): a shoulder flap whose
    /// root ring is half-weighted to the plate and whose tip swings to the camera. It draws right
    /// only through a joint palette, which is why a display carrying one makes even the **item**
    /// lane rig ([`DisplayModel::welds_billboard`], decision 0841). `false` for every ordinary part.
    pub(super) welded_billboard: bool,
    /// The part's animated material-alpha loops (colour-alpha × transparency-weight, decision 0130
    /// phase 2, per-sequence since 0641). The **effect lane** ([`super::spell_fx`]) samples them
    /// per instance on its armed slot; the unit/GameObject lane follows the host's live playing
    /// sequence (`MatAnim::following` — 0641 collected the old deferral here). `None` for static
    /// batches.
    pub(super) alpha_anim: Option<std::sync::Arc<benilla_formats::AlphaAnim>>,
    /// The part's animated RGB tint (the M2Color colour track, time-varying only). The static
    /// vertex tint was skipped at parse for these parts: `material`/its variants are built with
    /// the **first key** seeded as the material tint (pixel-identical to the old static bake);
    /// the effect lane clones + ticks the tint per instance. `None` for constant tints.
    pub(super) rgb_anim: Option<std::sync::Arc<benilla_formats::RgbAnim>>,
    /// The part's flat **ground-plane quad** shape (detected at M2 load — see
    /// [`benilla_formats::GroundQuad`]). On a base-anchored spell effect the fx attach renders it
    /// as a projected surface decal ([`crate::ground_fx`]) instead of free geometry, so it drapes
    /// terrain like the selection ring. `None` for ordinary geometry and all WMO parts.
    pub(super) ground_quad: Option<benilla_formats::GroundQuad>,
}

/// Everything needed to render one display id, cached so a model is loaded + built once and shared by
/// every entity of that display. `parts` is `None` while the asset loads, then `Some` — empty if the
/// model failed / has no geometry (→ cube/none fallback), else the spawn parts.
pub(super) struct DisplayModel {
    pub(super) handle: ModelHandle,
    /// The model's directory (for creature skin paths); empty for WMOs / no-skin models.
    pub(super) dir: String,
    /// Creature `Monster1/2/3` skin variation names; all `None` for GameObjects.
    pub(super) skins: [Option<String>; 3],
    /// A character-model NPC's body appearance (CreatureDisplayInfoExtra, decision 0041's creature
    /// chain): its race/sex + customization selectors + the pre-baked body-atlas name. `Some` only for a
    /// humanoid NPC display; `None` for a beast (Monster skins instead), a GameObject, or a *player*
    /// display (a player's appearance is on the wire — its `ObjectStore`, not here). The per-entity
    /// attach reads it to skin + geoset-filter a character-model NPC the same way it does a player.
    pub(super) npc_appearance: Option<NpcAppearance>,
    /// A held item display's runtime **object skin** — the ItemDisplayInfo model-texture basename
    /// (decision 0072), bound to the model's type-2 batches ([`CharSkinSlot::Object`]) at build, from
    /// [`Self::dir`]. `None` for creature/GameObject displays (their type-2 batches — a body model's
    /// cape slot — stay untextured until equipment provides one).
    pub(super) object_texture: Option<String>,
    pub(super) parts: Option<Vec<EntityPart>>,
    /// The model's particle emitters (flames/glows), captured alongside `parts` when the asset loads.
    /// Spawned per entity in [`attach_entity_visuals`], owned-by + following that entity. Empty for
    /// WMOs / no-emitter models.
    pub(super) emitters: Vec<ModelEmitter>,
    /// The model's ribbon emitters (weapon trails, wisp streamers), captured like `emitters`.
    pub(super) ribbons: Vec<benilla_assets::ModelRibbon>,
    /// The model's **M2 light blocks**, captured like `emitters` — the authored dynamic light an
    /// entity carries into the world (decision 0016). The common case is the **held torch**:
    /// `Club_1H_Torch_A_01.m2` authors one warm `type==1` point light, which is why a torch-bearing
    /// NPC lights the fence rails and grass around him on the reference. Empty for WMOs and for the
    /// ~all models that author none.
    pub(super) lights: Vec<benilla_assets::ModelLight>,
    /// Model-local collision collider, baked once from the model's collision hull when the asset loads —
    /// built **only for GameObjects** (creatures use unit-collision, not modeled here). `None` for a
    /// hull-less model (collide-iff-hull): that's why a herb/small prop is non-solid while a chest, mining
    /// vein, or door collides. Cloned (Arc-shared shape) onto each instance in [`attach_entity_visuals`],
    /// where the entity's own pose places it.
    pub(super) collider: Option<Collider>,
    /// The model's rest skeleton (decision 0019), captured with `parts` when an M2 asset loads. The
    /// creature attach path spawns one joint entity per bone from it, per instance. Empty for WMO /
    /// boneless / model-less displays.
    pub(super) skeleton: ModelSkeleton,
    /// The model's attachment points (decision 0072), captured with `parts` on load: id → bone +
    /// Bevy-space offset. The attach path folds them into each instance's [`BoneAttach`] so held
    /// items (and future bone riders) can hang from the hand/hip/back joints. Empty for WMO / static.
    pub(super) attachments: Vec<benilla_assets::ModelAttachment>,
    /// The model's animation-event positional markers, captured with `parts` on load: 4CC →
    /// bone + Bevy-space offset, file order (first-match queries). The attach path folds them into
    /// [`BoneAttach`] beside the attachments — the missile launch points ($CSL/$CSR/$CST/$BWR).
    pub(super) markers: Vec<benilla_assets::ModelMarker>,
    /// The matching inverse bind poses, shared across every instance of this display. `Some` for an M2
    /// display, `None` for WMO / model-less. Paired with `skeleton` to build each instance's palette rig.
    pub(super) inverse_bindposes: Option<Handle<SkinnedMeshInverseBindposes>>,
    /// The model's animations (decision 0019), captured with `parts` on load. The creature attach path
    /// gives each instance an `AnimationPlayer` driving them (playing Stand). `None` for WMO / static.
    pub(super) animations: Option<ModelAnimations>,
    /// The file-order-first sequence's authored duration ([`M2Model::first_seq_span`]), captured with
    /// `parts` on load — the spell-fx self-termination clock for a model whose sequences build no clip
    /// (the eat/drink tankard: a 6.667 s sequence, zero bone keys). `None` for WMO / sequence-less.
    pub(super) first_seq_span: Option<f32>,
    /// Camera framing-pivot height in **model-local yards, pre-scale** — `0.9 × bbox_z_extent` from the
    /// M2 authored bounds, captured in [`build_parts`]. Stamped onto each instance as [`CameraPivot`] so
    /// the third-person camera targets ~neck height rather than a fixed offset (wow-re `follow-camera`).
    /// `0.0` for a bounds-less / WMO / model-less display (→ the camera floors it).
    pub(super) pivot_height_local: f32,
    /// The target selection-ring radius in **model-local yards, pre-scale**: the Stand-animation footprint
    /// `sqrt(0.5 · sqrt(dx² + dy²))` ([`M2Bounds::ring_footprint`]). The ring's world radius is this × the
    /// unit's `OBJECT_FIELD_SCALE_X` (wow-re selection-ring RE, `0x608e00`/`0x60aee0`, emulated to the
    /// reference pixels). `0.0` for a bounds-less / WMO / model-less display (the ring then uses a fallback).
    pub(super) ground_radius_local: f32,
    /// The model's authored **portrait camera** (wow-re portrait-render §4), captured with `parts` on
    /// load — the exact rig the portrait booth frames through. `None` for WMO / model-less / the few
    /// camera-less M2s (the booth then falls back to heuristic framing).
    pub(super) portrait_camera: Option<benilla_assets::PortraitCamera>,
    /// A bow display's `$WTT`/`$WTB` bowstring anchors (wow-re `nocked-ammo-cancel.md` §G2),
    /// captured with `parts`: `[top, bottom]` as `(bone, model-local Bevy position)`. The held-item
    /// attach marks the prop root with them so the string drawer can span the tips. `None` for
    /// every non-bow model.
    pub(super) string_anchors: Option<[(u16, Vec3); 2]>,
    /// The authored bbox z-extent (`maxZ − minZ`, model-local yards, pre-scale) — the overhead-anchor
    /// fallback's input (`0x608640`: a unit whose model has no PlayerName attachment anchors overhead
    /// text at `feet + scale × this × 1.25`). `0.0` for a bounds-less display.
    pub(super) bbox_z_local: f32,
    /// The M2 vertex-box CENTRE in Bevy model-local (pre-scale) — the interior fold's MOLR
    /// reference point for a GameObject's footprint bake (the byte-cited `[def+0x5c]` anchor
    /// family; `crate::interior`). `Vec3::ZERO` for a bounds-less / WMO / model-less display.
    pub(super) bake_center_local: Vec3,
    /// The model's terrain-conform gate — MD20 `GlobalModelFlags & 3` (wow-re `terrain-tilt.md`,
    /// §5): `1` = pitch to slope, `3` = pitch+roll, else level. Captured with `parts` on load;
    /// `0` for WMO / model-less / still-loading displays.
    pub(super) terrain_tilt: u8,
    /// Whether this display resolves to a **character body** (a `Character\…` model path) — the
    /// gate for the char-customization pipeline (geoset filter + skin composite). The look follows
    /// the DISPLAY, not the entity kind (decision 0695): a druid in bear form is a Player-kind
    /// entity wearing a plain creature model, and the reference's own race/gender getters answer
    /// from the display's cached row, not the unit's descriptor (wow-re `w2d2.md`'s `0x60c690`
    /// getter family).
    pub(super) is_character_body: bool,
}

impl DisplayModel {
    /// Does any built part weld geometry to a billboard bone
    /// ([`EntityPart::welded_billboard`], decision 0839)? Such a part has no correct rigid
    /// placement — the reference blends it per vertex — so the lane that draws it must run a joint
    /// palette even where it otherwise wouldn't. The rigged lanes always do; this is the gate that
    /// makes the **item** lane rig for the seven shoulder models that need it (decision 0841).
    /// `false` while the model is still loading, and for the other 9684 models in the game.
    pub(super) fn welds_billboard(&self) -> bool {
        self.parts
            .as_ref()
            .is_some_and(|p| p.iter().any(|part| part.welded_billboard))
    }
}

/// Resolve a creature display id to a [`DisplayModel`]: load its M2 (no skins — the slots are filled at
/// build time from `textures`). A missing/zero-scale display gets an empty model → cube fallback.
pub(super) fn new_creature_display(
    catalog: &CreatureCatalog,
    display_id: u32,
    asset_server: &AssetServer,
) -> DisplayModel {
    match catalog.model(display_id) {
        Some(m) if m.scale > 0.0 => DisplayModel {
            handle: ModelHandle::M2(asset_server.load(m2_url(&m.model_path))),
            dir: model_dir(&m.model_path).to_string(),
            skins: m.textures,
            npc_appearance: m.npc_appearance,
            is_character_body: m
                .model_path
                .get(..10)
                .is_some_and(|p| p.eq_ignore_ascii_case("character\\")),
            ..empty_shell()
        },
        _ => empty_display(),
    }
}

/// Resolve a GameObject display id to a [`DisplayModel`]: load its M2 or WMO by path. No model path
/// (invisible trigger) gets an empty model → no render.
pub(super) fn new_gameobject_display(
    catalog: &GameObjectCatalog,
    display_id: u32,
    asset_server: &AssetServer,
) -> DisplayModel {
    let Some(path) = catalog.model_path(display_id) else {
        return empty_display();
    };
    let handle = if path.to_ascii_lowercase().ends_with(".wmo") {
        ModelHandle::Wmo(asset_server.load(wmo_url(path)))
    } else {
        ModelHandle::M2(asset_server.load(m2_url(path)))
    };
    DisplayModel {
        handle,
        ..empty_shell()
    }
}

/// A blank [`DisplayModel`] shell — `parts: None` (awaiting [`build_parts`]) and every capture field
/// at its default — for constructors that override only what their source provides.
pub(super) fn empty_shell() -> DisplayModel {
    DisplayModel {
        handle: ModelHandle::None,
        dir: String::new(),
        skins: Default::default(),
        npc_appearance: None,
        object_texture: None,
        parts: None,
        emitters: Vec::new(),
        ribbons: Vec::new(),
        lights: Vec::new(),
        collider: None,
        skeleton: ModelSkeleton::default(),
        attachments: Vec::new(),
        markers: Vec::new(),
        string_anchors: None,
        inverse_bindposes: None,
        animations: None,
        first_seq_span: None,
        pivot_height_local: 0.0,
        ground_radius_local: 0.0,
        portrait_camera: None,
        bbox_z_local: 0.0,
        bake_center_local: Vec3::ZERO,
        terrain_tilt: 0,
        is_character_body: false,
    }
}

/// A display with no model — `parts` already resolved to empty, so attach renders a cube/nothing.
pub(super) fn empty_display() -> DisplayModel {
    DisplayModel {
        parts: Some(Vec::new()),
        ..empty_shell()
    }
}

/// Build a display model's spawn parts once its asset has loaded — each submesh's `WowModelMaterial`,
/// with a creature skin slot filled from the display's variation (`<dir>\<name>.blp`). Returns early
/// (leaving `parts` `None`) while the asset is still loading.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_parts(
    dm: &mut DisplayModel,
    m2s: &Assets<M2Model>,
    wmos: &Assets<WmoModel>,
    // The app-built render forms (decision 0834): parts wait for the furnisher exactly as they
    // wait for the asset. Entity displays request at priority 0 — a mob walking into view never
    // queues behind a city crossing's scenery (whose placements request at 16+).
    forms: &mut crate::model_forms::ModelForms,
    asset_server: &AssetServer,
    materials: &mut Assets<WowModelMaterial>,
    cache: &mut MaterialCache,
    light: &bevy::render::render_resource::Buffer,
    // Whether this display is a GAMEOBJECT: gates the hull-collider build (chests/veins/doors;
    // creatures use unit collision).
    gameobject: bool,
) {
    let mut emitters = Vec::new();
    let mut ribbons = Vec::new();
    let mut lights = Vec::new();
    let mut collider = None;
    let mut terrain_tilt = 0u8;
    let mut skeleton = ModelSkeleton::default();
    let mut attachments = Vec::new();
    let mut markers = Vec::new();
    let mut string_anchors = None;
    let mut inverse_bindposes = None;
    let mut animations = None;
    let mut first_seq_span = None;
    let mut pivot_height_local = 0.0;
    let mut ground_radius_local = 0.0;
    let mut portrait_camera = None;
    let mut bbox_z_local = 0.0;
    let mut bake_center_local = Vec3::ZERO;
    let parts = match &dm.handle {
        ModelHandle::M2(h) => {
            let Some(model) = m2s.get(h) else {
                return; // still loading
            };
            // Every M2 entity lane can rig (creatures, animated GameObjects, held items with
            // billboard chains), so both forms are requested; the static form still serves the
            // truly static instances and the billboard cards.
            let key = crate::model_forms::ModelKey::from(h);
            let want = crate::model_forms::WANT_STATIC | crate::model_forms::WANT_SKINNED;
            if !forms.require(key, want, 0) {
                return; // forms still building — same retry-next-frame as the asset itself
            }
            emitters = model.emitters.clone();
            ribbons = model.ribbons.clone();
            lights = model.lights.clone();
            terrain_tilt = (model.global_flags & 3) as u8;
            // Target selection-ring radius (model-local, pre-scale): the **Stand-animation footprint**
            // `sqrt(0.5 × sqrt(dx² + dy²))` ([`M2Bounds::ring_footprint`], dx/dy = the Stand sequence
            // box's horizontal extents). This is the exact model-local input the real client's living-unit
            // ring uses (wow-re selection-ring RE, `0x608e00`/`0x60aee0` — byte-verified + emulated to the
            // reference pixels); the ring's world radius = this × OBJECT_FIELD_SCALE_X. NOT the render
            // sphere (`0xCC`) — that is the *corpse* decal's source (`0x5d6fe0`); the nested-sqrt footprint
            // is why the sphere never fit (it over-sized tall humans, under-sized the squat chicken).
            ground_radius_local = model.bounds.map_or(0.0, |b| b.ring_footprint);
            // Camera framing-pivot height (model-local, pre-scale), the reference's follow-camera target
            // (`0x50cbc0`, wow-re `follow-camera`): **attachment id 17's Z + 0.0972** when present (every
            // character model has it → ~neck height, e.g. 1.90 human / 0.88 gnome), else the fallback
            // `0.9 × vertex-box Z-extent` for a non-character model. The vertex box alone is the WRONG
            // source for characters — it's the all-animation extent (a human's is ~3.8, way over the
            // head), exactly why the old fixed 3.0 rode high.
            pivot_height_local = model.bounds.map_or(0.0, |b| {
                b.pivot_z
                    .map(|z| z + 0.0972)
                    .unwrap_or_else(|| 0.9 * (b.bbox_max[2] - b.bbox_min[2]).max(0.0))
            });
            // The raw vertex-box z-extent — the overhead-anchor FALLBACK's input (see the struct
            // field). Kept separate from the pivot: the fallback is the client's own formula, and
            // only fires for a model with no PlayerName attachment.
            bbox_z_local = model
                .bounds
                .map_or(0.0, |b| (b.bbox_max[2] - b.bbox_min[2]).max(0.0));
            // The vertex-box centre (Bevy local) — a GameObject's interior-fold reference point.
            bake_center_local = model.bounds.map_or(Vec3::ZERO, |b| {
                benilla_assets::coords::wow_to_bevy([
                    (b.bbox_min[0] + b.bbox_max[0]) * 0.5,
                    (b.bbox_min[1] + b.bbox_max[1]) * 0.5,
                    (b.bbox_min[2] + b.bbox_max[2]) * 0.5,
                ])
            });
            // The rest skeleton + shared inverse bind poses + the animation graph (decision 0019) — the
            // creature attach path spawns a joint hierarchy + an AnimationPlayer from these. Captured
            // here, with `parts`, on load.
            skeleton = model.skeleton.clone();
            attachments = model.attachments.clone();
            markers = model.markers.clone();
            string_anchors = model.string_anchors;
            inverse_bindposes = Some(model.inverse_bindposes.clone());
            animations = model.animations.clone();
            first_seq_span = model.first_seq_span;
            portrait_camera = model.portrait_camera;
            if gameobject {
                collider = model.collision.as_ref().and_then(model_local_collider);
            }
            let stat_forms = forms.static_meshes(key).unwrap_or(&[]);
            let skin_forms = forms.skinned_meshes(key).unwrap_or(&[]);
            model
                .submeshes
                .iter()
                .enumerate()
                .map(|(pi, sub)| {
                    let texture = resolve_skin(
                        sub,
                        &dm.dir,
                        &dm.skins,
                        dm.object_texture.as_deref(),
                        asset_server,
                    );
                    // The authored batch order (index + 1), into every variant's transparent sort
                    // bias — one model's coplanar transparent batches (the Naxx items' Mod2x
                    // sheen + Blend overlay) draw in file order instead of re-flipping a sort tie
                    // every frame (`model_render::BATCH_ORDER_SORT_EPS`).
                    let order = u16::try_from(pi + 1).unwrap_or(u16::MAX);
                    let exterior = model_material(
                        cache,
                        materials,
                        texture.clone(),
                        sub.blend,
                        sub.two_sided,
                        false,
                        false,        // exterior (sky-lit) variant
                        sub.emissive, // M2 UNLIT (0x01) glass/glow → fullbright
                        sub.additive, // M2 blend 3/4 → additive (glow cards)
                        false,
                        sub.no_depth_write, // M2 render flag 0x10
                        sub.no_depth_test,  // M2 render flag 0x08
                        sub.fog_policy,
                        // Every entity M2 (unit/player/GameObject/held/fx) is built LIT — the verified
                        // §9 chain gives them the same 2.5/0.5 lane as ADT doodads, and the DYNAMIC half
                        // (the MCSH sample at their feet) rides the per-instance MeshTag shade byte
                        // (`entity_shade`), not the shared material.
                        ShadeSel::Lit,
                        order,
                        None, // entities/portrait paths: UV anim deferred (0130 scope = placed doodads)
                        sub.rgb_anim.as_ref(), // animated M2Color tint, seeded at its first key
                        None, // entity M2: light selection anchors at the instance origin
                        None, // M2 carries no MOMT SIDN colour
                        false, // …nor the WINDOW flag
                        light,
                    );
                    // The AlphaMode::Blend twin for the spawn appear-fade (decision 0032) — the exterior
                    // look feathered, the same variant DoodadFade uses. Reuse the exterior when it's
                    // already Blend (its cutout already feathers; no separate twin needed). A MULTIPLY
                    // batch (Mod/Mod2x) gets NO twin: its blend equation reads no alpha, so it cannot
                    // fade — the reference's instanceAlpha ramp leaves the sheen at full strength while
                    // the base fades under it (decision 0528); `None` spawns the part Steady.
                    // The depth-prime twin (wow-re `m2-blend-promotion-zfill.md` §4): every batch
                    // that can fade AND writes depth gets one. `cutout` mirrors what the part's
                    // colour pass discards while translucent — the fade twin's hard 224/255 for
                    // AlphaKey sources only (an Opaque source never alpha-tests, steady or
                    // promoted — §2; decision 0842), nothing for authored-Blend sources (their
                    // colour pass is the plain blend material) — so depth and colour coverage agree.
                    let zfill = if sub.no_depth_write || sub.no_depth_test {
                        None
                    } else {
                        match sub.blend {
                            ModelBlend::Mod | ModelBlend::Mod2x => None,
                            b => Some(crate::model_render::zfill_material(
                                cache,
                                materials,
                                texture.clone(),
                                sub.two_sided,
                                b == ModelBlend::AlphaTest,
                                light,
                            )),
                        }
                    };
                    let fade_blend = match sub.blend {
                        ModelBlend::Mod | ModelBlend::Mod2x => None,
                        ModelBlend::Blend => Some(exterior.clone()),
                        // The SOURCE blend rides into the twin (Opaque or AlphaKey here): with
                        // fade_variant it still builds AlphaMode::Blend, but the source decides
                        // the twin's 224/255 cutout marker (decision 0842).
                        _ => Some(model_material(
                            cache,
                            materials,
                            texture.clone(),
                            sub.blend,
                            sub.two_sided,
                            false,
                            false, // exterior
                            sub.emissive,
                            sub.additive,
                            true, // fade_variant — the blend twin the shader feathers
                            sub.no_depth_write,
                            sub.no_depth_test,
                            sub.fog_policy,
                            ShadeSel::Lit, // matches the exterior variant above
                            order,
                            None, // entities/portrait paths: UV anim deferred (0130 scope = placed doodads)
                            sub.rgb_anim.as_ref(),
                            None, // entity M2 fade twin: same instance-origin anchor
                            None,
                            false,
                            light,
                        )),
                    };
                    // The interior BAKE variant (every M2 entity): interior-PROP mode — the shader
                    // evaluates the model's SH probe (footprint MOCV + MOLR lobes, `crate::interior`)
                    // by the MeshTag slot. The reference hands EVERY entity M2 — unit, player,
                    // GameObject — the same entity-node fill (`Node::SetModel` → the footprint bake;
                    // wow-re `unit-m2-shader-light.md`), so all classes build the variant and take it
                    // indoors. Cache-deduped like every variant.
                    let interior_bake = Some(model_material(
                        cache,
                        materials,
                        texture.clone(),
                        sub.blend,
                        sub.two_sided,
                        false,
                        true, // interior-PROP mode — the probe lane
                        sub.emissive,
                        sub.additive,
                        false,
                        sub.no_depth_write,
                        sub.no_depth_test,
                        sub.fog_policy,
                        ShadeSel::Matte, // unread on the probe lane
                        order,
                        None,
                        sub.rgb_anim.as_ref(),
                        None,
                        None,
                        false,
                        light,
                    ));
                    // The bake lane's Blend twin — the probe-lit feather (the self-avatar zoom,
                    // a despawn ramp): same probe mode, alpha-blend state. Without it a fade swaps
                    // an indoor entity to the EXTERIOR blend twin and its light jumps to the lit
                    // outdoor intensity mid-feather (director-caught, 2026-07-13). Reuse the bake
                    // variant when the part is already Blend, like the exterior twin above; a
                    // multiply batch fades never (no fade twin at all), so it builds none either.
                    let interior_bake_blend = interior_bake.as_ref().and_then(|bake| {
                        match sub.blend {
                            ModelBlend::Mod | ModelBlend::Mod2x => None,
                            ModelBlend::Blend => Some(bake.clone()),
                            // Source blend through to the twin, like the exterior twin above (0842).
                            _ => Some(model_material(
                                cache,
                                materials,
                                texture.clone(),
                                sub.blend,
                                sub.two_sided,
                                false,
                                true, // interior-PROP mode — the probe lane
                                sub.emissive,
                                sub.additive,
                                true, // fade_variant — the blend twin the shader feathers
                                sub.no_depth_write,
                                sub.no_depth_test,
                                sub.fog_policy,
                                ShadeSel::Matte, // unread on the probe lane
                                order,
                                None,
                                sub.rgb_anim.as_ref(),
                                None,
                                None,
                                false,
                                light,
                            )),
                        }
                    });
                    // The interior MATTE variant: the plain day/night pair at sun ×1.0 — the
                    // reference's null-node fallback (`0x672a20` with no registered node), which the
                    // classifier applies when the footprint ray misses / hits MOPY&1 / the probe
                    // table is full. Not the steady indoor law — that's the bake above.
                    let interior = model_material(
                        cache,
                        materials,
                        texture,
                        sub.blend,
                        sub.two_sided,
                        false,
                        false, // NOT the prop MOCV path — entities have no interior colour leg
                        sub.emissive,
                        sub.additive,
                        false,
                        sub.no_depth_write,
                        sub.no_depth_test,
                        sub.fog_policy,
                        ShadeSel::Matte, // sun ×1.0, the forced indoor intensity — const so it dedups
                        order,
                        None, // entities/portrait paths: UV anim deferred (0130 scope = placed doodads)
                        sub.rgb_anim.as_ref(),
                        None, // entity M2 interior variant: same instance-origin anchor
                        None,
                        false,
                        light,
                    );
                    EntityPart {
                        // Index-parallel with the submeshes by the forms contract; a miss is a
                        // broken contract — a default (dead) handle draws nothing rather than panic.
                        mesh: stat_forms
                            .get(pi)
                            .map(|(h, _)| h.clone())
                            .unwrap_or_default(),
                        geometry: sub.geometry.clone(),
                        aabb: stat_forms.get(pi).and_then(|(_, a)| *a),
                        skinned_mesh: skin_forms.get(pi).cloned(),
                        material: exterior,
                        material_interior: Some(interior),
                        material_interior_bake: interior_bake,
                        material_interior_bake_blend: interior_bake_blend,
                        fade_blend,
                        zfill,
                        blend: sub.blend,
                        additive: sub.additive,
                        two_sided: sub.two_sided,
                        geoset_id: sub.geoset_id,
                        char_slot: sub.char_slot,
                        billboard: sub.billboard.clone(),
                        welded_billboard: sub.geometry.welded_billboard,
                        alpha_anim: sub.alpha_anim.clone(),
                        rgb_anim: sub.rgb_anim.clone(),
                        ground_quad: sub.ground_quad,
                    }
                })
                .collect()
        }
        ModelHandle::Wmo(h) => {
            let Some(model) = wmos.get(h) else {
                return;
            };
            // WMO-display GameObjects never skin — static forms only.
            let key = crate::model_forms::ModelKey::from(h);
            if !forms.require(key, crate::model_forms::WANT_STATIC, 0) {
                return;
            }
            if gameobject {
                collider = model.collision.as_ref().and_then(model_local_collider);
            }
            let stat_forms = forms.static_meshes(key).unwrap_or(&[]);
            model
                .submeshes
                .iter()
                .enumerate()
                .map(|(pi, sub)| EntityPart {
                    mesh: stat_forms
                        .get(pi)
                        .map(|(h, _)| h.clone())
                        .unwrap_or_default(),
                    geometry: sub.geometry.clone(),
                    aabb: stat_forms.get(pi).and_then(|(_, a)| *a),
                    geoset_id: sub.geoset_id, // 0 for WMO — no geoset selection
                    char_slot: None,          // WMO is never a character body
                    skinned_mesh: None,       // WMO-display GameObjects don't skin (no skeleton)
                    material: model_material(
                        cache,
                        materials,
                        sub.texture.clone(),
                        sub.blend,
                        sub.two_sided,
                        true,
                        sub.interior,
                        sub.emissive,
                        sub.additive, // false for WMO (additive WMO batches deferred)
                        false,
                        sub.no_depth_write, // false for WMO (standard depth state)
                        sub.no_depth_test,
                        sub.fog_policy,  // always Scene for WMO
                        ShadeSel::Matte, // WMO uses the FFP N·L path, not the lobe — sun_scale unused
                        0,
                        None, // entities/portrait paths: UV anim deferred (0130 scope = placed doodads)
                        None, // WMO batches carry no M2Color tint
                        sub.wmo_batch, // an interior group's INT/TRANS lighting law rides tint.w
                        sub.sidn, // MOMT SIDN night-glow colour
                        sub.window, // MOMT WINDOW midpoint light
                        light,
                    ),
                    material_interior: None, // WMO entity: interior is per-submesh, baked into `material`
                    material_interior_bake: None, // …and never the M2 footprint lane
                    material_interior_bake_blend: None,
                    fade_blend: None, // WMO-display GameObjects don't appear-fade (rare; M2 only)
                    zfill: None,      // …so they never need the depth-prime twin either
                    blend: sub.blend,
                    additive: false, // WMO MOMT carries no additive mode (`RenderSubmesh::additive`)
                    two_sided: sub.two_sided,
                    billboard: None,         // WMO groups have no billboard bones
                    welded_billboard: false, // …so nothing can be welded to one
                    alpha_anim: None,        // …nor colour/weight loops
                    rgb_anim: None,
                    ground_quad: None, // the fx decal lane is M2-only
                })
                .collect()
        }
        ModelHandle::None => Vec::new(),
    };
    dm.parts = Some(parts);
    dm.emitters = emitters;
    dm.ribbons = ribbons;
    dm.lights = lights;
    dm.collider = collider;
    dm.skeleton = skeleton;
    dm.attachments = attachments;
    dm.markers = markers;
    dm.string_anchors = string_anchors;
    dm.inverse_bindposes = inverse_bindposes;
    dm.animations = animations;
    dm.first_seq_span = first_seq_span;
    dm.pivot_height_local = pivot_height_local;
    dm.ground_radius_local = ground_radius_local;
    dm.portrait_camera = portrait_camera;
    dm.bbox_z_local = bbox_z_local;
    dm.bake_center_local = bake_center_local;
    dm.terrain_tilt = terrain_tilt;
}

/// Bake a model-local avian trimesh collider from a model's raw-WoW collision hull, mapping each vertex
/// through `wow_to_bevy` so it coincides with the entity's rendered submeshes (same convention as the
/// map-doodad [`crate::terrain_stream`] bake, minus the placement — the entity's own pose places it).
/// `None` for a hull-less or degenerate mesh: collide-iff-hull, so herbs/props stay non-solid.
fn model_local_collider(hull: &CollisionMesh) -> Option<Collider> {
    if hull.indices.len() < 3 {
        return None;
    }
    let verts: Vec<Vec3> = hull.positions.iter().map(|p| wow_to_bevy(*p)).collect();
    let tris: Vec<[u32; 3]> = hull
        .indices
        .chunks_exact(3)
        .map(|c| [c[0], c[1], c[2]])
        .collect();
    Some(Collider::trimesh(verts, tris))
}

/// A submesh's texture: its embedded one; for an unfilled creature skin slot, the display's variation
/// (`<dir>\<name>.blp`); for a runtime **object** slot (M2 type 2 — a held item's blade/face skin),
/// the display's ItemDisplayInfo model texture, same folder (decision 0072). `None` ⇒ untextured
/// (the material's muted fallback — e.g. a body model's cape slot with nothing equipped).
fn resolve_skin(
    sub: &ModelSubmesh,
    dir: &str,
    skins: &[Option<String>; 3],
    object_texture: Option<&str>,
    asset_server: &AssetServer,
) -> Option<Handle<Image>> {
    if sub.char_slot == Some(CharSkinSlot::Object) {
        return object_texture.map(|t| asset_server.load(skin_url(dir, t)));
    }
    match (&sub.texture, sub.skin_slot) {
        (Some(t), _) => Some(t.clone()),
        (None, Some(slot)) => skins
            .get(slot as usize)
            .and_then(|o| o.as_ref())
            .map(|name| asset_server.load(skin_url(dir, name))),
        (None, None) => None,
    }
}

/// The directory of a model path (`Creature\Kobold\Kobold.mdx` → `Creature\Kobold`), where its skin
/// variation BLPs live. Empty if the path has no directory component.
fn model_dir(model_path: &str) -> &str {
    model_path
        .rsplit_once(['\\', '/'])
        .map(|(dir, _)| dir)
        .unwrap_or("")
}
