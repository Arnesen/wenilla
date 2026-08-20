//! **`WOW_STATIC_MERGE=1` — the production static-world consolidation, doodad lanes** (design
//! 1417, lane order re-ranked by 1418's density verdict; the measured premise is 1413/1416's
//! 44 ns/row tax).
//!
//! The assembler diverts every ADT-doodad batch that is fully static (the bracket's anim
//! exclusions), **order-free** (`Opaque`/`AlphaTest`, not additive: 0858's law that authored
//! draw order exists only on transparent-pass batches) and not an interior-slot prop, into
//! this buffer; the flush bakes each `(owner tile, 133⅓-yd cell, material)` group into ONE
//! mesh entity with placement transforms baked into the vertices. **Faders merge too** (1418
//! — they are the dense population, 4.5 batches per blob at the SW pin): each vertex carries
//! its placement's fade sphere ([`benilla_assets::ATTRIBUTE_WOW_FADE_SPHERE`]) and
//! `wow_model.wgsl`'s `WOW_MERGED_FADE` lane computes the faithful fade curve per vertex —
//! alpha in-shader, `Hidden` as a clip-space collapse at zero. A fader blob draws on its BLEND
//! TWIN permanently (1420): the reference's own fading render state, whose output at fade 1.0
//! is pixel-identical to the steady cutout — so the feather is the reference's smooth
//! translucent ramp, per placement, with no material swap and no dither (1419's ordered-dither
//! stand-in quantized the ramp to 16 visible steps — the director's report killed it).
//!
//! The cell key preserves the frustum-cull locality the bracket's round 1 proved load-bearing
//! (+1.38 without it, −0.93 with); the owner tile buys the weld's whole lifetime story — the
//! blob lands in `TileState::merged` and despawns with its tile.
//!
//! **WMO group geometry never diverts (1418's verdict):** `batch_order` is a `MatKey` axis, so
//! every WMO batch already owns a unique material handle — under the correct
//! `(uid, group, material)` key the measured merge is EXACTLY 1:1, zero rows saved. The WMO
//! share of the frame belongs to option B's cross-material retained draw, not to any
//! entity-level lane. [`MergeSite::Wmo`] survives only to feed the census predictor.
//!
//! The close rule is the weld's (1369), not the bracket's wall clock: vertex cap + idle-frame
//! tail, one quiet frame in normal play, [`MERGE_IDLE_FRAMES`] under the arrival cover.
//! Dead-owner accumulators are discarded, and the whole buffer clears on map drop for the same
//! tile-keys-repeat-across-maps reason the weld's does.
//!
//! **Known v1 gaps, deliberate** (each named in 1417, none reachable at a parked measuring
//! pin): a tile-straddling doodad's blob dies with its owner tile and nothing re-emits it on
//! the owner's reload while a neighbour still holds the placement (1369's re-cross gap, shared;
//! the owner-handoff re-emit is the recorded fix and lands before any default-on). And the
//! flush is not yet counted into the settle release the way `HullWelds::unflushed` is — under
//! the cover the idle tail is frames, not seconds, so the reveal race is narrow; still a
//! before-default-on item.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::NoAutoAabb;
use bevy::mesh::MeshTag;
use bevy::prelude::*;

use benilla_assets::materials::WowModelMaterial;
use benilla_assets::merged_static_mesh_faded;
use benilla_formats::{ModelBlend, RenderSubmesh};

use crate::interact::WorldObject;
use crate::mesh_tag::alpha_bits;
use crate::model_render::{ModelKind, ModelPart};
use crate::wmo_portal::WmoGroupVis;

/// Vertex cap per blob: bounds any one bake + upload, and keeps a blob's cull bound a
/// neighbourhood rather than a zone (the weld's `WELD_MAX_TRIS` argument, in render units). A
/// single oversized batch closes its blob immediately, same as an oversized hull.
const MERGE_MAX_VERTS: usize = 65_536;

/// Quiet frames that close a live accumulator under the arrival cover; in normal play ONE
/// quiet frame closes it, so a crossing's new tile blobs appear without a visible hold (the
/// weld's exact rule — its comment owns the reasoning).
const MERGE_IDLE_FRAMES: u32 = 15;

/// The doodad spatial cell, ¼ of an ADT tile — the bracket's measured 133⅓-yd locality key
/// (`mega_static`'s comment records the round-1 failure without it).
const CELL: f32 = 533.333_3 / 4.0;

/// One accumulating blob: shared geometry + placement transforms + per-placement fade
/// spheres (index-parallel with `parts`), baked at flush.
struct MergeAcc {
    parts: Vec<(Arc<RenderSubmesh>, Transform)>,
    spheres: Vec<Vec4>,
    /// Interior-prop accs only (index-parallel with `parts`): each part's SH-probe slot, baked
    /// per vertex at flush. Empty on every other lane — homogeneous per key by construction,
    /// because the interior flag is a material axis and the material is in the key.
    slots: Vec<u32>,
    verts: usize,
    blend: ModelBlend,
    kind: ModelKind,
    /// [`StaticMerge::frame`] at the last append — the idle clock.
    last_add: u32,
}

impl MergeAcc {
    fn ready(&self, frame: u32, idle_frames: u32) -> bool {
        self.verts >= MERGE_MAX_VERTS || frame.wrapping_sub(self.last_add) >= idle_frames
    }
}

/// Where a diverted batch belongs — built once per placement by the spawn driver, consumed per
/// batch by the assembler's divert.
pub enum MergeSite<'a> {
    /// An ADT map doodad: owned by its first-registering tile (the weld's ownership).
    Doodad { owner: (i32, i32) },
    /// WMO group geometry: owned by its placement; `groups` is the asset's per-submesh group
    /// index table (index-parallel with the submeshes the assembler iterates).
    Wmo {
        uid: u32,
        groups: &'a [u16],
        portal_gated: bool,
    },
    /// A WMO doodad prop (1418 lane 3): owned by its placement, keyed by the referrer-set of
    /// rooms that name it (`groups` — the blob takes the same set-valued `WmoGroupVis` its
    /// members carried) and, for an interior prop, carrying the per-prop SH-probe slot the
    /// bake writes per vertex.
    Prop {
        uid: u32,
        groups: &'a Arc<[u16]>,
        slot: Option<u16>,
    },
}

impl MergeSite<'_> {
    /// The would-be merge key of one batch under this site, hashed — the census's blob-count
    /// predictor (each lane's expected blob count = distinct keys in its class). Every site's
    /// key here mirrors its real divert key exactly.
    pub fn census_key(
        &self,
        batch_idx: usize,
        mat: &Handle<WowModelMaterial>,
        transform: &Transform,
    ) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        match self {
            MergeSite::Doodad { owner } => {
                let cell = (
                    (transform.translation.x / CELL).floor() as i32,
                    (transform.translation.z / CELL).floor() as i32,
                );
                (0u8, owner, cell, mat.id()).hash(&mut h);
            }
            MergeSite::Wmo { uid, groups, .. } => {
                (1u8, uid, groups.get(batch_idx)?, mat.id()).hash(&mut h);
            }
            MergeSite::Prop { uid, groups, .. } => {
                (2u8, uid, groups, mat.id()).hash(&mut h);
            }
        }
        Some(h.finish())
    }
}

/// (owner tile, 133⅓-yd cell, material) — a doodad blob's identity.
type DoodadKey = ((i32, i32), (i32, i32), Handle<WowModelMaterial>);
/// (placement uid, referrer-set, material) — a prop blob's identity. The `Arc<[u16]>` hashes
/// by CONTENT, so two props named by the same rooms share a blob and distinct sets never can.
type PropKey = (u32, Arc<[u16]>, Handle<WowModelMaterial>);

/// The in-flight merge accumulators. Same lifecycle discipline as [`super::weld::HullWelds`]:
/// fed by the spawn chain, drained one chain-step later, cleared with the world it describes.
#[derive(Resource, Default)]
pub struct StaticMerge {
    /// Flush-system tick, the idle clock. Wrapping u32 — only ever read as a difference.
    frame: u32,
    doodads: HashMap<DoodadKey, MergeAcc>,
    props: HashMap<PropKey, MergeAcc>,
    /// Running totals since the last drain report (1417's VRAM honesty line): blobs spawned,
    /// batches baked into them, vertices BAKED (every placement a copy) vs the vertices the
    /// members' SHARED assets hold (each distinct geometry once — Arc identity), so the log
    /// states the duplication factor the desk estimate guessed at ~3×.
    blobs: u64,
    batches: u64,
    baked_verts: u64,
    shared_verts: u64,
    seen_geometry: std::collections::HashSet<usize>,
    reported: bool,
}

/// Is lane 1 armed? Read once; the assembler divert and the flush both key on it.
pub fn merge_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("WOW_STATIC_MERGE").as_deref() == Ok("1"))
}

impl StaticMerge {
    /// Take one mergeable batch into its accumulator. `fade_sphere` = the placement's world
    /// fade center + radius, baked per vertex at flush (a never-fader carries its true radius
    /// and the shader's `> 7` arm pins it opaque). `false` = this site never merges (WMO group
    /// geometry and props — 1418's verdict / the referrer-set key) — the caller spawns the
    /// batch individually, the fail-open arm.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn divert(
        &mut self,
        site: &MergeSite<'_>,
        _batch_idx: usize,
        mat: &Handle<WowModelMaterial>,
        geometry: &Arc<RenderSubmesh>,
        transform: Transform,
        fade_sphere: Vec4,
        blend: ModelBlend,
        kind: ModelKind,
    ) -> bool {
        let frame = self.frame;
        let acc = match site {
            MergeSite::Doodad { owner } => {
                let cell = (
                    (transform.translation.x / CELL).floor() as i32,
                    (transform.translation.z / CELL).floor() as i32,
                );
                self.doodads
                    .entry((*owner, cell, mat.clone()))
                    .or_insert_with(|| MergeAcc {
                        parts: Vec::new(),
                        spheres: Vec::new(),
                        slots: Vec::new(),
                        verts: 0,
                        blend,
                        kind,
                        last_add: frame,
                    })
            }
            MergeSite::Prop { uid, groups, slot } => {
                let acc = self
                    .props
                    .entry((*uid, Arc::clone(groups), mat.clone()))
                    .or_insert_with(|| MergeAcc {
                        parts: Vec::new(),
                        spheres: Vec::new(),
                        slots: Vec::new(),
                        verts: 0,
                        blend,
                        kind,
                        last_add: frame,
                    });
                if let Some(slot) = slot {
                    acc.slots.push(u32::from(*slot));
                }
                // The material's interior axis makes a key all-interior or all-exterior; a
                // ragged slot list would misindex the per-vertex bake, so it is a hard error.
                debug_assert!(acc.slots.is_empty() || acc.slots.len() == acc.parts.len() + 1);
                acc
            }
            // WMO group geometry never merges: measured 1:1 under its correct key (1418 —
            // batch_order rides MatKey). The site exists for the census predictor.
            MergeSite::Wmo { .. } => return false,
        };
        acc.spheres.push(fade_sphere);
        let verts = geometry.positions.len();
        if self
            .seen_geometry
            .insert(Arc::as_ptr(geometry) as *const () as usize)
        {
            self.shared_verts += verts as u64;
        }
        self.baked_verts += verts as u64;
        self.batches += 1;
        acc.parts.push((geometry.clone(), transform));
        acc.verts += verts;
        acc.last_add = frame;
        true
    }

    /// Accumulators not yet baked — the reveal gate's term (`WorldLoadProgress::merge_pending`;
    /// the weld's `unflushed` argument, on the render side). An overcount only delays a
    /// release, never wrongs one.
    pub(super) fn unflushed(&self) -> usize {
        self.doodads.len() + self.props.len()
    }

    pub(super) fn clear(&mut self) {
        self.doodads.clear();
        self.props.clear();
        self.blobs = 0;
        self.batches = 0;
        self.baked_verts = 0;
        self.shared_verts = 0;
        self.seen_geometry.clear();
        self.reported = true;
    }
}

/// Close ready accumulators into blob entities and hand each to its owner tile
/// (`TileState::merged` — despawned with the tile, like the welds). A dead owner discards its
/// accumulator: spawning a blob nothing owns is a leak (the weld's rule, and its reachability
/// argument — the owner died past the unload line).
///
/// Runs in the Stream chain right after `flush_hull_welds`, for the weld's own reason: the
/// frame's appends see the flush at a deterministic point and the owner lookups race nothing.
pub(super) fn flush_static_merge(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    merge: ResMut<StaticMerge>,
    mut streamer: ResMut<super::TerrainStreamer>,
    mut placements: ResMut<super::Placements>,
    focus: Res<super::ViewFocus>,
    mut progress: Option<ResMut<super::WorldLoadProgress>>,
) {
    let merge = merge.into_inner();
    merge.frame = merge.frame.wrapping_add(1);
    let frame = merge.frame;
    let idle = if focus.paced { MERGE_IDLE_FRAMES } else { 1 };
    let mut blobs = 0u64;
    merge.doodads.retain(|key, acc| {
        let Some(tile) = streamer.tiles.get_mut(&key.0) else {
            return false;
        };
        if !acc.ready(frame, idle) {
            return true;
        }
        blobs += 1;
        tile.merged.push(spawn_blob(
            &mut commands,
            &mut meshes,
            &key.2,
            acc,
            None,
            true,
        ));
        false
    });
    merge.props.retain(|key, acc| {
        let Some(p) = placements.by_id.get_mut(&key.0) else {
            return false;
        };
        if !acc.ready(frame, idle) {
            return true;
        }
        blobs += 1;
        // The blob takes exactly the vis/tagging its members had (spawn/mod.rs's prop site):
        // the set-valued `WmoGroupVis` + `ExteriorScene` when the building has an instance and
        // rooms name the prop; untagged otherwise (no key ⇒ no exemption possible — 0784).
        let vis = (!key.1.is_empty())
            .then_some(p.portal_instance)
            .flatten()
            .map(|instance| WmoGroupVis {
                instance,
                groups: Arc::clone(&key.1),
            });
        let exterior = vis.is_some();
        p.entities.push(spawn_blob(
            &mut commands,
            &mut meshes,
            &key.2,
            acc,
            vis,
            exterior,
        ));
        false
    });
    merge.blobs += blobs;
    if blobs > 0 {
        merge.reported = false;
    }
    // Publish the backlog for the reveal gate (this system sits in the Stream chain, so the
    // consumers read this frame's depth — the weld's publish discipline).
    if let Some(progress) = progress.as_mut() {
        progress.merge_pending = merge.unflushed();
    }
    // The drain report (1417's VRAM honesty line), once per settled wave: what the merge took
    // and what the transform-baking duplication actually costs against the shared assets.
    if !merge.reported && merge.doodads.is_empty() && merge.props.is_empty() {
        merge.reported = true;
        info!(
            "static-merge: {} blobs from {} batches; baked {}kv vs {}kv shared ({:.2}x duplication)",
            merge.blobs,
            merge.batches,
            merge.baked_verts / 1000,
            merge.shared_verts / 1000,
            merge.baked_verts as f64 / merge.shared_verts.max(1) as f64
        );
    }
}

/// One closed accumulator → one blob entity carrying exactly what its members carried minus
/// the per-placement machinery the shader lane now owns (1418): no `DoodadFade` (the baked
/// fade spheres drive `WOW_MERGED_FADE`; `MeshTag` stays at opaque), no `PickMesh` (nameable,
/// not pickable — the weld's identity rule, 0929), `ExteriorScene` (every member had it), the
/// union `Aabb` (authored: `NoAutoAabb`).
fn spawn_blob(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    mat: &Handle<WowModelMaterial>,
    acc: &mut MergeAcc,
    vis: Option<WmoGroupVis>,
    exterior: bool,
) -> Entity {
    let parts = std::mem::take(&mut acc.parts);
    let spheres = std::mem::take(&mut acc.spheres);
    let slots = std::mem::take(&mut acc.slots);
    let n = parts.len();
    let (mesh, mn, mx) =
        merged_static_mesh_faded(&parts, &spheres, (!slots.is_empty()).then_some(&slots[..]));
    // An interior blob's tag keeps the members' INTERIOR_FOG staging bit (the slot half of the
    // payload is dead under WOW_MERGED_SLOT — the vertices carry it).
    let tag = if slots.is_empty() {
        alpha_bits(1.0)
    } else {
        crate::mesh_tag::probe_bits(0)
    };
    let mut blob = commands.spawn((
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(mat.clone()),
        Transform::IDENTITY,
        ModelPart {
            kind: acc.kind,
            blend: acc.blend,
        },
        MeshTag(tag),
        Aabb::from_min_max(mn, mx),
        NoAutoAabb,
        WorldObject {
            kind: acc.kind,
            label: "static-merge".into(),
            id: 0,
            detail: format!("{n} batches merged"),
        },
    ));
    if exterior {
        blob.insert(crate::exterior_cull::ExteriorScene);
    }
    if let Some(vis) = vis {
        blob.insert(vis);
    }
    blob.id()
}

#[cfg(test)]
mod tests {
    use super::super::{ModelHandle, Placement, Placements, TerrainStreamer, TileState, ViewFocus};
    use super::*;
    use benilla_assets::{ATTRIBUTE_WOW_FADE_SPHERE, ATTRIBUTE_WOW_MERGED_SLOT};
    use bevy::app::TaskPoolPlugin;
    use bevy::ecs::system::RunSystemOnce;

    fn geometry(verts: usize) -> Arc<RenderSubmesh> {
        Arc::new(RenderSubmesh {
            positions: vec![[0.0, 0.0, 0.0]; verts],
            ..Default::default()
        })
    }

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(TaskPoolPlugin::default());
        app.init_resource::<Assets<Mesh>>();
        app.init_resource::<StaticMerge>();
        app.init_resource::<TerrainStreamer>();
        app.init_resource::<super::super::Placements>();
        app.init_resource::<ViewFocus>();
        app
    }

    fn blank_tile() -> TileState {
        TileState {
            handle: Default::default(),
            entity: None,
            material: None,
            next_cell: 0,
            furnished: false,
            placements: Vec::new(),
            liquid: Vec::new(),
            wall: None,
            clutter: Vec::new(),
            welds: Vec::new(),
            merged: Vec::new(),
        }
    }

    fn divert_doodad(merge: &mut StaticMerge, owner: (i32, i32), at: Vec3, verts: usize) {
        let mat = Handle::<WowModelMaterial>::default();
        assert!(merge.divert(
            &MergeSite::Doodad { owner },
            0,
            &mat,
            &geometry(verts),
            Transform::from_translation(at),
            Vec4::new(at.x, at.y, at.z, 1.5),
            ModelBlend::Opaque,
            ModelKind::Doodad,
        ));
    }

    /// Two doodads in the same cell on the same material accumulate into ONE blob; a third in
    /// a different cell opens a second accumulator (the locality key round 1 proved out).
    #[test]
    fn cell_key_partitions_doodad_accumulators() {
        let mut merge = StaticMerge::default();
        divert_doodad(&mut merge, (0, 0), Vec3::new(1.0, 0.0, 1.0), 3);
        divert_doodad(&mut merge, (0, 0), Vec3::new(2.0, 0.0, 2.0), 3);
        divert_doodad(&mut merge, (0, 0), Vec3::new(CELL + 1.0, 0.0, 1.0), 3);
        assert_eq!(merge.doodads.len(), 2);
        let joint = merge
            .doodads
            .values()
            .find(|a| a.parts.len() == 2)
            .expect("same-cell parts share an accumulator");
        assert_eq!(joint.verts, 6);
        assert_eq!(joint.spheres.len(), 2);
    }

    /// WMO group geometry never diverts (1418's 1:1 verdict) — the site exists for the census
    /// predictor only.
    #[test]
    fn wmo_site_refuses_the_divert() {
        let mut merge = StaticMerge::default();
        let mat = Handle::<WowModelMaterial>::default();
        assert!(!merge.divert(
            &MergeSite::Wmo {
                uid: 7,
                groups: &[4, 9],
                portal_gated: true,
            },
            0,
            &mat,
            &geometry(3),
            Transform::IDENTITY,
            Vec4::new(0.0, 0.0, 0.0, f32::INFINITY),
            ModelBlend::Opaque,
            ModelKind::Wmo,
        ));
        assert!(merge.doodads.is_empty() && merge.props.is_empty());
    }

    fn blank_placement(portal_instance: Option<Entity>) -> Placement {
        Placement {
            model: ModelHandle::M2(Default::default()),
            transform: Transform::IDENTITY,
            entities: Vec::new(),
            spawned: true,
            doodad_set: 0,
            name_set: 0,
            doodads: Vec::new(),
            portal_instance,
            refs: 1,
            owner: (0, 0),
        }
    }

    /// An interior-prop blob (1418 lane 3) lands in its placement's entity list carrying the
    /// set-valued room key, the per-vertex probe slots, and the INTERIOR_FOG-staged tag.
    #[test]
    fn interior_prop_blob_carries_rooms_and_baked_slots() {
        let mut app = test_app();
        app.world_mut().resource_mut::<ViewFocus>().paced = false;
        let instance = app.world_mut().spawn_empty().id();
        app.world_mut()
            .resource_mut::<Placements>()
            .by_id
            .insert(7, blank_placement(Some(instance)));
        let rooms: Arc<[u16]> = Arc::from([3u16, 5].as_slice());
        {
            let mut merge = app.world_mut().resource_mut::<StaticMerge>();
            let mat = Handle::<WowModelMaterial>::default();
            for slot in [11u16, 12] {
                assert!(merge.divert(
                    &MergeSite::Prop {
                        uid: 7,
                        groups: &rooms,
                        slot: Some(slot),
                    },
                    0,
                    &mat,
                    &geometry(3),
                    Transform::IDENTITY,
                    Vec4::new(0.0, 0.0, 0.0, f32::INFINITY),
                    ModelBlend::Opaque,
                    ModelKind::Doodad,
                ));
            }
        }
        app.world_mut().run_system_once(flush_static_merge).unwrap();
        app.world_mut().run_system_once(flush_static_merge).unwrap();
        let placements = app.world().resource::<Placements>();
        let owned = placements.by_id.get(&7).unwrap().entities.clone();
        assert_eq!(owned.len(), 1);
        let blob = owned[0];
        let vis = app.world().get::<WmoGroupVis>(blob).unwrap();
        assert_eq!(vis.instance, instance);
        assert_eq!(&*vis.groups, &[3, 5]);
        // The tag keeps the interior-fog staging bit with the slot half dead (probe 0).
        let tag = app.world().get::<MeshTag>(blob).unwrap();
        assert_eq!(tag.0, crate::mesh_tag::probe_bits(0));
        let mesh3d = app.world().get::<Mesh3d>(blob).unwrap().0.clone();
        let meshes = app.world().resource::<Assets<Mesh>>();
        let mesh = meshes.get(&mesh3d).unwrap();
        // 3 verts per part, slots 11 then 12 replicated per vertex.
        match mesh.attribute(ATTRIBUTE_WOW_MERGED_SLOT).unwrap() {
            bevy::mesh::VertexAttributeValues::Uint32(v) => {
                assert_eq!(v, &[11, 11, 11, 12, 12, 12]);
            }
            other => panic!("slot attribute has the wrong format: {other:?}"),
        }
    }

    /// An EXTERIOR prop blob (no slots) bakes no slot attribute, and a prop no room names
    /// takes no vis key and no exterior tag (the untagged-not-gated-blind rule, 0784).
    #[test]
    fn exterior_and_unnamed_prop_blobs_stay_plain() {
        let mut app = test_app();
        app.world_mut().resource_mut::<ViewFocus>().paced = false;
        app.world_mut()
            .resource_mut::<Placements>()
            .by_id
            .insert(9, blank_placement(None));
        let rooms: Arc<[u16]> = Arc::from([].as_slice());
        {
            let mut merge = app.world_mut().resource_mut::<StaticMerge>();
            let mat = Handle::<WowModelMaterial>::default();
            assert!(merge.divert(
                &MergeSite::Prop {
                    uid: 9,
                    groups: &rooms,
                    slot: None,
                },
                0,
                &mat,
                &geometry(3),
                Transform::IDENTITY,
                Vec4::new(0.0, 0.0, 0.0, 4.0),
                ModelBlend::Opaque,
                ModelKind::Doodad,
            ));
        }
        app.world_mut().run_system_once(flush_static_merge).unwrap();
        app.world_mut().run_system_once(flush_static_merge).unwrap();
        let placements = app.world().resource::<Placements>();
        let owned = placements.by_id.get(&9).unwrap().entities.clone();
        assert_eq!(owned.len(), 1);
        let blob = owned[0];
        assert!(app.world().get::<WmoGroupVis>(blob).is_none());
        assert!(app
            .world()
            .get::<crate::exterior_cull::ExteriorScene>(blob)
            .is_none());
        let mesh3d = app.world().get::<Mesh3d>(blob).unwrap().0.clone();
        let meshes = app.world().resource::<Assets<Mesh>>();
        let mesh = meshes.get(&mesh3d).unwrap();
        assert!(mesh.attribute(ATTRIBUTE_WOW_MERGED_SLOT).is_none());
        assert!(mesh.attribute(ATTRIBUTE_WOW_FADE_SPHERE).is_some());
    }

    /// The idle tail closes a quiet doodad accumulator into a blob owned by its tile, with the
    /// authored bound, the per-vertex fade spheres, and no per-entity fade enrollment.
    #[test]
    fn idle_tail_closes_a_doodad_blob_onto_its_tile() {
        let mut app = test_app();
        app.world_mut().resource_mut::<ViewFocus>().paced = true;
        app.world_mut()
            .resource_mut::<TerrainStreamer>()
            .tiles
            .insert((3, 4), blank_tile());
        divert_doodad(
            &mut app.world_mut().resource_mut::<StaticMerge>(),
            (3, 4),
            Vec3::new(5.0, 0.0, 5.0),
            3,
        );
        for _ in 0..(MERGE_IDLE_FRAMES - 1) {
            app.world_mut().run_system_once(flush_static_merge).unwrap();
        }
        assert_eq!(app.world().resource::<StaticMerge>().doodads.len(), 1);
        app.world_mut().run_system_once(flush_static_merge).unwrap();
        assert!(app.world().resource::<StaticMerge>().doodads.is_empty());
        let streamer = app.world().resource::<TerrainStreamer>();
        let owned = streamer.tiles.get(&(3, 4)).unwrap().merged.clone();
        assert_eq!(owned.len(), 1);
        let blob = owned[0];
        assert!(app.world().get::<NoAutoAabb>(blob).is_some());
        assert!(app
            .world()
            .get::<crate::model_fade::DoodadFade>(blob)
            .is_none());
        // The baked mesh carries one fade sphere per vertex — the WOW_MERGED_FADE contract.
        let mesh3d = app.world().get::<Mesh3d>(blob).unwrap().0.clone();
        let meshes = app.world().resource::<Assets<Mesh>>();
        let mesh = meshes.get(&mesh3d).unwrap();
        let spheres = mesh.attribute(ATTRIBUTE_WOW_FADE_SPHERE).unwrap();
        assert_eq!(spheres.len(), 3);
    }

    /// A dead owner discards the accumulator — no blob, no leak (the weld's rule).
    #[test]
    fn dead_owner_discards_the_accumulator() {
        let mut app = test_app();
        divert_doodad(
            &mut app.world_mut().resource_mut::<StaticMerge>(),
            (9, 9),
            Vec3::ZERO,
            3,
        );
        let before = app.world().entities().len();
        app.world_mut().run_system_once(flush_static_merge).unwrap();
        assert!(app.world().resource::<StaticMerge>().doodads.is_empty());
        assert_eq!(app.world().entities().len(), before);
    }

    /// The vertex cap closes an accumulator without waiting for the idle tail.
    #[test]
    fn vert_cap_closes_immediately() {
        let mut app = test_app();
        app.world_mut().resource_mut::<ViewFocus>().paced = true;
        app.world_mut()
            .resource_mut::<TerrainStreamer>()
            .tiles
            .insert((0, 0), blank_tile());
        divert_doodad(
            &mut app.world_mut().resource_mut::<StaticMerge>(),
            (0, 0),
            Vec3::ZERO,
            MERGE_MAX_VERTS,
        );
        app.world_mut().run_system_once(flush_static_merge).unwrap();
        let streamer = app.world().resource::<TerrainStreamer>();
        assert_eq!(streamer.tiles.get(&(0, 0)).unwrap().merged.len(), 1);
    }
}
