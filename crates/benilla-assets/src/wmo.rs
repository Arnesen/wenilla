//! WMO → model asset loader.
//!
//! A WMO (building) is a **root** file plus N **group** files (`<stem>_NNN.wmo`). The loader reads the
//! root from the [`Reader`], then reads each group via [`LoadContext::read_asset_bytes`] (raw bytes,
//! recorded as a dependency — *not* `load()`, which would recurse back into this loader), building one
//! [`ModelSubmesh`] per group render batch with its texture resolved against the root's material
//! tables.
//!
//! Same app-independent shape as the M2 loader (meshes + texture handles + metadata; materials at
//! spawn). WMOs don't size-fade, so there are no bounds.

use crate::column_grid::ColumnGrid;
use std::sync::Arc;

use benilla_formats::{
    accumulate_wmo_group_camera_collision, accumulate_wmo_group_camera_only_collision,
    accumulate_wmo_group_collision, parse_wmo_lights, parse_wmo_root, wmo_group_doodad_refs,
    wmo_group_footprint_tris, wmo_group_header, wmo_group_light_refs, wmo_group_liquid_mesh,
    wmo_group_submeshes, CollisionMesh, FootprintTris, LiquidMesh, WmoDoodad, WmoDoodadSet, WmoFog,
    WmoGroupInfo, WmoLight, WmoPortalInfo, WmoPortalRef,
};
use bevy::asset::io::Reader;
use bevy::asset::{Asset, AssetLoader, LoadContext};
use bevy::prelude::*;
use bevy::reflect::TypePath;

use crate::model::{build_submesh_mesh, ModelSubmesh};

/// Per-WMO-group navigation data for the portal visibility cull (`crate::wmo_portal`): the group's
/// MOGP flags (the EXTERIOR `0x8` bit the flood defers on), its bounding box (WMO model space, WoW
/// axes — the camera current-group containment test), and its slice of [`WmoModel::portal_refs`].
/// Indexed by **absolute group index** (parallel gaps for unreadable groups carry a degenerate box +
/// zero refs, so they're never a current-group candidate and never flood).
#[derive(Clone, Copy)]
pub struct WmoGroupNav {
    pub flags: u32,
    pub bbox_min: [f32; 3],
    pub bbox_max: [f32; 3],
    pub ref_start: u16,
    pub ref_count: u16,
    /// The group's `WMOAreaTable.WMOGroupID` key (MOGP `uniqueID`; 0 = none carried).
    pub area_table_id: u32,
    /// The group's MOGP fog indices (disk `+0x30`, empirically pinned against the Goldshire inn) —
    /// up to four indices into [`WmoModel::fogs`], walked by the camera-in-interior fog selector
    /// (`0x69de20`, wow-re `rf-weather-emission-timeline` ROUND 5).
    pub fog_indices: [u8; 4],
}

/// A loaded WMO building: the render batches across all its groups, flattened, plus the flattened
/// collidable triangles (the app bakes a collider from these per placement).
#[derive(Asset, TypePath, Clone)]
pub struct WmoModel {
    /// The building's `WMOAreaTable.WMOID` key (root `MOHD.wmoID`; NSabbey → 59).
    pub wmo_id: u32,
    pub submeshes: Vec<ModelSubmesh>,
    /// Group index per entry in [`Self::submeshes`] (parallel) — which WMO group each render batch
    /// belongs to, so the portal cull can hide a whole group by toggling its submeshes' `Visibility`.
    pub submesh_group: Vec<u16>,
    /// The portal graph (MOPV vertices / MOPT infos / MOPR refs), WMO model space (WoW axes). The
    /// per-frame cull floods this from the camera's current group (`crate::wmo_portal`). Empty ⇒ no
    /// portals (single-group props) ⇒ every group stays visible.
    pub portal_vertices: Vec<[f32; 3]>,
    pub portal_infos: Vec<WmoPortalInfo>,
    pub portal_refs: Vec<WmoPortalRef>,
    /// Per-group nav (flags + bbox + portal-ref slice), indexed by absolute group index.
    pub group_nav: Vec<WmoGroupNav>,
    /// The root's MFOG fog records — record 0 is the WMO default (the interior-fog selector's
    /// seed); a group's [`WmoGroupNav::fog_indices`] point here. Consumed by the camera-in-interior
    /// fog resolve (`crate::wmo_portal` in the app), per wow-re `rf-weather-emission-timeline`
    /// ROUND 5 (`0x69de20`).
    pub fogs: Vec<WmoFog>,
    /// Per-group **walking-collision** triangles (WoW model space, indexed by absolute group index) —
    /// the Leg-A face set for the current-group **down-ray** (`crate::wmo_portal`). The faithful "which
    /// room is the camera in" test casts a ray straight down from the eye against the same face set the
    /// player's walking collision uses — every non-DETAIL face, **no orientation filter** (`wow-5875-re`
    /// `system/models/scratch/wmo-portal-audit.md` Q3/Q4: the client's Leg A is the walking-collision
    /// BSP, mask `0x84`; an earlier render-face `|n.z|` floor heuristic mis-seeded doorways and slab
    /// edges). Same triangles as [`Self::collision`], kept per group for group attribution.
    pub group_collision_tris: Vec<Vec<[[f32; 3]; 3]>>,
    /// Per-group **camera-only** triangles (parallel to [`Self::group_collision_tris`]): the faces
    /// the camera gather keeps but the walking gather drops — DETAIL (`0x04`) set, NOCAMCOLLIDE
    /// (`0x02`) clear. The down-ray's **camera-void fallback** (decision 0692) races this set only
    /// after the faithful walking Leg A and portal Leg B both miss AND no terrain surface sits at or
    /// below the eye: an eye held between camera-collidable surfaces (the Deadmines entrance pocket,
    /// whose floor is all-DETAIL) then still names its room instead of blanking the building.
    pub group_camera_only_tris: Vec<Vec<[[f32; 3]; 3]>>,
    /// Per-group AABB of [`Self::group_collision_tris`] (parallel; `None` = no collision faces),
    /// computed at load **from the triangles themselves** — the broad phase for the faces-only
    /// down-ray (`area_down_ray`). Bounds derived from the faces can only summarize the geometry,
    /// never understate it the way an authored MOGI box can (NSabbey group 3's authored bottom
    /// floats 1.5 yd above its own floor polys — the 2026-07-12 indoor flap), so culling on them
    /// is exact: a group is skipped only when no face of it could own the probe column.
    pub group_collision_bounds: Vec<Option<([f32; 3], [f32; 3])>>,
    /// Per-group column index over [`Self::group_collision_tris`] (parallel; `None` = a face set
    /// too small to be worth indexing, and the caller keeps its linear scan). The narrow phase the
    /// per-group bounds above never had: a dungeon group holds ~11–16k faces, so bounds alone
    /// still left every down-ray scanning tens of thousands of triangles per unit per frame
    /// (LBRS, 2026-07-27). See [`crate::column_grid`].
    pub group_collision_grids: Vec<Option<ColumnGrid>>,
    /// The union of [`Self::group_collision_bounds`] — the whole-building broad phase: a probe
    /// column outside it cannot hit any collision face, so the per-frame indoor trackers skip the
    /// placed instance outright (a camera in open country pays one AABB test per building, not a
    /// face scan — the 2026-07-12 fps regression).
    pub collision_bounds: Option<([f32; 3], [f32; 3])>,
    /// **Walking** collidable triangles across all groups in raw WMO-local coords — the player-body
    /// gather (excludes DETAIL faces). `None` when the building carries no collidable geometry.
    pub collision: Option<CollisionMesh>,
    /// **Camera/LOS** collidable triangles — the third-person-camera gather (excludes NOCAMCOLLIDE,
    /// *keeps* DETAIL). Distinct from [`Self::collision`]; the app bakes a separate collider on the
    /// camera collision layer so the camera can't slip behind visible decals/overhangs (e.g. forge
    /// pipes) the player walks under. `None` when the building has no camera-collidable geometry.
    pub collision_camera: Option<CollisionMesh>,
    /// The root's placed doodads (MODD props — candle stands, banners), in WMO model space; the app
    /// spawns each as an M2 composed with the WMO instance transform, selected by doodad set.
    pub doodads: Vec<WmoDoodad>,
    /// The doodad sets (MODS) — ranges into [`Self::doodads`]; set 0 is global, a placement picks one more.
    pub doodad_sets: Vec<WmoDoodadSet>,
    /// The root's MOLT lights (the interior fixture lights — fireplaces, forge, candles, chandeliers),
    /// in WMO model space. Spawned as dynamic point lights (decisions 0016/0273) that warm nearby
    /// doodads/NPCs, terrain, and the building's own walls/floor over their baked MOCV.
    pub lights: Vec<WmoLight>,
    /// Per-group interior flag + bounding box (MOGI), WMO model space — classifies which placed doodads
    /// are interior (lit off the room/fixtures) vs exterior (lit off the sky). See [`WmoGroupInfo`].
    pub group_bounds: Vec<WmoGroupInfo>,
    /// Per-MODD lighting base, parallel to [`Self::doodads`] — resolved ONCE at load (placements are
    /// fixed in the root). See [`DoodadBase`]: an interior-group doodad's base is its own MODD
    /// entry's baked colour, and its point light comes only from its owning group's MOLR list. The
    /// footprint down-ray this replaced (decision 0290) is byte-real code, but it is the ADT-MDDF
    /// attach path — a WMO MODD doodad never reaches it (create never calls SetMatrix; wow-re
    /// `trace-forensics-abbey-interior-d3d` §1.1).
    pub doodad_base: Vec<DoodadBase>,
    /// Per-MODD **instantiating group** (the first group whose MODR refs name it), parallel to
    /// [`Self::doodads`]; `None` for a MODD no group references. The reference creates a doodad
    /// once, on the first visible-group walk that reaches it, and that create is what freezes its
    /// lighting lane — so this is the LIGHTING key ([`Self::doodad_base`]), not the cull key.
    pub doodad_owner: Vec<Option<u16>>,
    /// Per-MODD **referencing groups** — every group whose MODR names it, parallel to
    /// [`Self::doodads`]; empty for a MODD no group references.
    ///
    /// This is the prop's portal-cull key. The reference commits a WMO's doodads **per visible
    /// group** — `0x695aa0` loops the group's own MODR refs at `group+0xe8`/count `+0x144`, called
    /// from the visible-group walk `0x698720` (wow-re `m2-interior-doodad-base-light.md` §453,
    /// `trace-forensics-abbey-interior-d3d.md` §90) — so a group the portal flood culled draws none
    /// of its furniture, and a prop **any** of whose referrers is visible is drawn. Without the key
    /// at all, props outlive their own building: cull every group of a dungeon and its lanterns,
    /// crates and cobwebs hang in the void (decision 0689). With the key collapsed to one owner, a
    /// prop that hangs through several rooms — a lava fall — blinks out on every camera angle whose
    /// flood reaches a different one of its referrers.
    pub doodad_groups: Vec<Arc<[u16]>>,
    /// Per-group FOOTPRINT face set (render tris + baked MOCV + MOPY flags), indexed by absolute
    /// group index; `None` for exterior groups / groups without MOCV. The **GameObject M2** light
    /// lane samples it: the entity node's env-update attach down-rays the render mesh under the
    /// object and bakes the hit's barycentric MOCV, floor-168/cap-96, on the fixed interior axis
    /// (`0x69e4c0`; wow-re `wmo-lit-selector.md`, trace-decisive on the abbey benches).
    pub group_footprints: Vec<Option<FootprintTris>>,
    /// Per-group AABB of the faces [`Self::group_footprints`] actually references (parallel;
    /// `None` = no footprint faces), computed at load from the triangles themselves — the broad
    /// phase for the footprint down-ray, the same exact-cull argument as
    /// [`Self::group_collision_bounds`]: a probe column outside a group's face bounds in XY, or
    /// wholly below its lowest vertex, can hit none of its faces. Without it every re-rayed
    /// entity scanned EVERY interior group of the whole model — at Stormwind (one WMO, all
    /// districts) that was the live-session frame (decision 0364).
    pub group_footprint_bounds: Vec<Option<([f32; 3], [f32; 3])>>,
    /// Per-group column index over [`Self::group_footprints`] (parallel; `None` = not indexed).
    /// The footprint sample is the heavier of the two column rays — it walks render faces, not
    /// collision ones. See [`crate::column_grid`].
    pub group_footprint_grids: Vec<Option<ColumnGrid>>,
    /// Per-group MOLR light refs (indices into [`Self::lights`]), indexed by absolute group index —
    /// the point lights that fold into an interior GameObject's committed light are gated by the
    /// footprint-hit group's list, same as a MODD prop's by its owning group.
    pub group_light_refs: Vec<Vec<u16>>,
    /// Per-group MLIQ liquid surface (the animated water/lava/slime a group embeds — Stormwind's
    /// canals + fountains, the Ironforge lava, dungeon pools), indexed by absolute group index;
    /// `None` for a group with no `MLIQ`. Built in WMO model space (WoW axes); the app spawns each at
    /// the placement transform, on the shared per-kind liquid material.
    pub group_liquids: Vec<Option<LiquidMesh>>,
}

/// The load-resolved lighting base of one placed MODD doodad. The real client fills an
/// interior-group doodad's slot-0 words at CREATE, from the MODD record's own colour field —
/// byte-verified (`0x694e90` → `6950de call 0x6a77e0(&MODD.colour, &diffuse, 0x70, &ambient, 0x60)`)
/// and read live off the reference's abbey draws, where both decoded stands' diffuse words equal
/// their MODD colour bytes verbatim (wow-re `trace-forensics-abbey-interior-d3d` §1).
#[derive(Clone, PartialEq, Debug)]
pub enum DoodadBase {
    /// Exterior-group doodad — the sky-lit lane (day/night sun; a WMO prop takes the plain matte).
    Exterior,
    /// Interior-group doodad: the MODD-colour base + the owning group's light refs.
    Interior(InteriorPropBase),
}

/// An interior MODD doodad's resolved base light — computed once at load, day/night-independent.
#[derive(Clone, PartialEq, Debug)]
pub struct InteriorPropBase {
    /// The ambient word: `cap96(MODD.colour)` (0–1 RGB). See [`cap96`].
    pub ambient: [f32; 3],
    /// The diffuse word: `floor112(MODD.colour)` (0–1 RGB), committed as a directional lobe on the
    /// fixed engine axis (−0.30822, −0.30822, −0.9) — never the day/night sun. See [`floor112`].
    pub diffuse: [f32; 3],
    /// The owning group's MOLR light refs (indices into [`WmoModel::lights`]): the ONLY point lights
    /// this doodad's committed light folds (range-gated by each light's disk attenStart/attenEnd at
    /// spawn). Empty — a group with no MOLR — means NO point light at all, its own flame included.
    pub light_refs: Vec<u16>,
}

/// Per-group AABBs of the footprint faces each group actually references —
/// [`WmoModel::group_footprint_bounds`], derived at load from the indexed vertices (not the whole
/// position array, which may carry vertices no footprint face uses). Shared with the fixture
/// builders so tests and the loader can never disagree on what the bounds mean.
pub fn footprint_tri_bounds(
    footprints: &[Option<FootprintTris>],
) -> Vec<Option<([f32; 3], [f32; 3])>> {
    footprints
        .iter()
        .map(|fp| {
            let fp = fp.as_ref()?;
            let mut bounds: Option<([f32; 3], [f32; 3])> = None;
            for &i in &fp.indices {
                let Some(v) = fp.positions.get(i as usize) else {
                    continue;
                };
                let (min, max) = bounds.get_or_insert((*v, *v));
                for a in 0..3 {
                    min[a] = min[a].min(v[a]);
                    max[a] = max[a].max(v[a]);
                }
            }
            bounds
        })
        .collect()
}

/// Per-group column indexes over the footprint faces — [`WmoModel::group_footprint_grids`].
/// Shared with the fixture builders for the same reason the bounds are: a test model and a loaded
/// one must be indexed by identical code, or an exactness test proves nothing about the real path.
pub fn footprint_tri_grids(footprints: &[Option<FootprintTris>]) -> Vec<Option<ColumnGrid>> {
    footprints
        .iter()
        .map(|fp| {
            let fp = fp.as_ref()?;
            let tris: Vec<&[u16]> = fp.indices.chunks_exact(3).collect();
            ColumnGrid::build(tris.len(), |i| {
                let mut lo = [f32::MAX; 2];
                let mut hi = [f32::MIN; 2];
                for &vi in tris[i] {
                    if let Some(v) = fp.positions.get(vi as usize) {
                        for a in 0..2 {
                            lo[a] = lo[a].min(v[a]);
                            hi[a] = hi[a].max(v[a]);
                        }
                    }
                }
                (lo, hi)
            })
        })
        .collect()
}

/// Per-group column indexes over a per-group triangle set — [`WmoModel::group_collision_grids`].
pub fn collision_tri_grids(tris: &[Vec<[[f32; 3]; 3]>]) -> Vec<Option<ColumnGrid>> {
    tris.iter()
        .map(|group| {
            ColumnGrid::build(group.len(), |i| {
                let t = &group[i];
                (
                    [
                        t[0][0].min(t[1][0]).min(t[2][0]),
                        t[0][1].min(t[1][1]).min(t[2][1]),
                    ],
                    [
                        t[0][0].max(t[1][0]).max(t[2][0]),
                        t[0][1].max(t[1][1]).max(t[2][1]),
                    ],
                )
            })
        })
        .collect()
}

/// The per-group AABBs of a per-group triangle set + their union — [`WmoModel::group_collision_bounds`]
/// and [`WmoModel::collision_bounds`], derived from the faces at load (shared with the down-ray tests,
/// so fixtures and the loader can never disagree on what the bounds mean).
#[allow(clippy::type_complexity)]
pub fn collision_tri_bounds(
    tris: &[Vec<[[f32; 3]; 3]>],
) -> (
    Vec<Option<([f32; 3], [f32; 3])>>,
    Option<([f32; 3], [f32; 3])>,
) {
    let per_group: Vec<Option<([f32; 3], [f32; 3])>> = tris
        .iter()
        .map(|group| {
            let mut bounds: Option<([f32; 3], [f32; 3])> = None;
            for tri in group {
                for v in tri {
                    let (min, max) = bounds.get_or_insert((*v, *v));
                    for a in 0..3 {
                        min[a] = min[a].min(v[a]);
                        max[a] = max[a].max(v[a]);
                    }
                }
            }
            bounds
        })
        .collect();
    let union = per_group.iter().flatten().fold(
        None::<([f32; 3], [f32; 3])>,
        |acc, (gmin, gmax)| match acc {
            None => Some((*gmin, *gmax)),
            Some((mut min, mut max)) => {
                for a in 0..3 {
                    min[a] = min[a].min(gmin[a]);
                    max[a] = max[a].max(gmax[a]);
                }
                Some((min, max))
            }
        },
    );
    (per_group, union)
}

/// The ambient-word CAP of `0x6a77e0` (exact fixed-point arithmetic, byte-verified — re-derived in
/// wow-re `trace-forensics-abbey-interior-d3d` §1.1 and bit-exact against both decoded abbey stands):
/// a colour whose max channel exceeds 96 is scaled down so max = 96 — per channel
/// `(c·scale + 255) >> 8` with `scale = round(96·255/max − 0.5)`; max ≤ 96 passes through raw.
pub fn cap96(c: [u8; 3]) -> [f32; 3] {
    let max = c[0].max(c[1]).max(c[2]);
    if max <= 96 {
        return c.map(|v| f32::from(v) / 255.0);
    }
    let scale = ((96.0 * 255.0 / f32::from(max)) - 0.5).round_ties_even() as u32;
    c.map(|v| ((u32::from(v) * scale + 255) >> 8) as f32 / 255.0)
}

/// The diffuse-word FLOOR of `0x6a77e0`: a colour whose max channel is below `thresh` is raised —
/// HSV round-trip re-emitting at value `thresh`, which for an RGB triple is a hue/saturation-
/// preserving scale by `thresh/max`, **truncated** per channel. The rounding was OPEN until the
/// abbey INNBENCH decode exercised the raise leg live: bench MOCVs (59,65,92)/(69,63,83) raise to
/// diffuse (107,118,168)/(139,127,168) — 63·(168/83) = 127.52 lands on 127, which truncation gives
/// and nearest-rounding does not (wow-re `wmo-lit-selector.md`, machine-zero fits).
fn floor_raise(c: [u8; 3], thresh: u8) -> [f32; 3] {
    let max = c[0].max(c[1]).max(c[2]);
    if max >= thresh || max == 0 {
        return c.map(|v| f32::from(v) / 255.0);
    }
    // Integer scale — truncating by construction AND exact on the max channel (a float
    // `v · (thresh/max)` can land a hair under `thresh` and truncate to `thresh − 1`).
    c.map(|v| ((u32::from(v) * u32::from(thresh)) / u32::from(max)) as f32 / 255.0)
}

/// [`floor_raise`] at the MODD create site's diffuse threshold `0x70` (112). Max ≥ 112 passes
/// through RAW — both decoded abbey stands do (maxes 134/141 → the diffuse word IS the MODD colour
/// verbatim).
pub fn floor112(c: [u8; 3]) -> [f32; 3] {
    floor_raise(c, 112)
}

/// [`floor_raise`] at the entity/footprint attach site's diffuse threshold `0xA8` (168) — the
/// GameObject M2 lane (`0x69e4c0`, the entity twin of the ADT-MDDF `0x6a8410`; wow-re
/// `wmo-lit-selector.md`). Both decoded abbey benches take the raise leg (MOCV maxes 92/83 →
/// diffuse max exactly 168 on the wire).
pub fn floor168(c: [u8; 3]) -> [f32; 3] {
    floor_raise(c, 168)
}

/// Invert MODR: MODD index → its **instantiating** group. First referencing group wins — the
/// reference creates a doodad once, on the first visible-group walk that names it, and that create
/// is what freezes its lighting lane. `None` = referenced by no group at all (the reference never
/// instantiates such a MODD). This is the LIGHTING key only; the *cull* key is [`modr_refs`].
fn modr_owners(doodad_count: usize, group_doodad_refs: &[Vec<u16>]) -> Vec<Option<u16>> {
    let mut owner: Vec<Option<u16>> = vec![None; doodad_count];
    for (gi, refs) in group_doodad_refs.iter().enumerate() {
        for &di in refs {
            if let Some(slot) = owner.get_mut(di as usize) {
                slot.get_or_insert(gi as u16);
            }
        }
    }
    owner
}

/// Invert MODR the other way: MODD index → **every** group whose refs name it. The draw loop is
/// per *visible* group over that group's own MODR (`0x695aa0` from the visible-group walk
/// `0x698720`), so a doodad named by N groups is reached by N independent chances to draw — it is
/// on screen when ANY of them is in the portal PVS, not only its first. Collapsing this to the
/// first owner is what made Blackrock's and the Great Forge's lava falls blink out by camera
/// angle: `BLACKROCKSTATUELAVAFLOW` is named by 9–19 groups apiece and `LAVAPOTS` by 13, so all
/// but one referrer's rooms hid a prop that the reference draws from every one of them.
fn modr_refs(doodad_count: usize, group_doodad_refs: &[Vec<u16>]) -> Vec<Arc<[u16]>> {
    let mut refs: Vec<Vec<u16>> = vec![Vec::new(); doodad_count];
    for (gi, group) in group_doodad_refs.iter().enumerate() {
        for &di in group {
            if let Some(slot) = refs.get_mut(di as usize) {
                // MODR may name the same doodad twice in one group; the cull only needs the set.
                if slot.last() != Some(&(gi as u16)) {
                    slot.push(gi as u16);
                }
            }
        }
    }
    refs.into_iter().map(Arc::from).collect()
}

/// Resolve every MODD placement's lighting base once, at load. Ownership and interior class follow
/// the faithful create path: a doodad belongs to the group(s) whose **MODR** references it (the
/// instantiate loop is per-group over MODR — never a spatial test), the owning group's
/// interior/exterior class (`MOGI flags & 0x48`) picks the lane, and the owning group's **MOLR**
/// list is its complete point-light candidate set. A MODD referenced by no group is never
/// instantiated by the reference at all — Exterior here (the harmless default if our spawner shows
/// it anyway).
///
/// `owner` is [`modr_owners`]' inversion, shared with the portal-cull key ([`WmoModel::doodad_owner`])
/// so the lighting lane and the cull can never disagree about which group a prop belongs to.
fn resolve_doodad_bases(
    doodads: &[WmoDoodad],
    groups: &[WmoGroupInfo],
    owner: &[Option<u16>],
    group_light_refs: &[Vec<u16>],
) -> Vec<DoodadBase> {
    doodads
        .iter()
        .zip(owner)
        .map(|(d, owner)| {
            let Some(gi) = owner else {
                return DoodadBase::Exterior;
            };
            let interior = groups
                .get(*gi as usize)
                .is_some_and(|g: &WmoGroupInfo| g.interior);
            if !interior {
                return DoodadBase::Exterior;
            }
            let rgb = [d.color[0], d.color[1], d.color[2]];
            DoodadBase::Interior(InteriorPropBase {
                ambient: cap96(rgb),
                diffuse: floor112(rgb),
                light_refs: group_light_refs
                    .get(*gi as usize)
                    .cloned()
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// Bevy [`AssetLoader`] decoding a WMO root `*.wmo` (+ its group files) → [`WmoModel`].
#[derive(Default, TypePath)]
pub struct WmoModelLoader;

impl AssetLoader for WmoModelLoader {
    type Asset = WmoModel;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        ctx: &mut LoadContext<'_>,
    ) -> Result<WmoModel, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let to_io = |e: anyhow::Error| std::io::Error::other(format!("{e:#}"));
        let root = parse_wmo_root(&bytes).map_err(to_io)?;

        // Root path → stem for the group files. Lowercased so the suffix strip + group URLs are
        // consistent (MPQ resolution is case-insensitive regardless).
        let root_path = ctx.path().path().to_string_lossy().to_ascii_lowercase();
        let stem = root_path
            .strip_suffix(".wmo")
            .unwrap_or(&root_path)
            .to_string();

        let mut submeshes = Vec::new();
        // Group index per submesh (parallel to `submeshes`) — the portal cull's handle on which group a
        // render batch belongs to.
        let mut submesh_group: Vec<u16> = Vec::new();
        // Per-group nav, pre-sized to the full group count and indexed by absolute group index so the
        // MOPR `group` references line up even when a group file is skipped below. Bounds come from the
        // root MOGI (available for every group); flags + portal-ref span are filled from each group
        // header as it's read.
        let mut group_nav: Vec<WmoGroupNav> = (0..root.group_count() as usize)
            .map(|gi| {
                let (bbox_min, bbox_max) = root
                    .group_infos()
                    .get(gi)
                    .map_or(([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]), |g| {
                        (g.bbox_min, g.bbox_max)
                    });
                WmoGroupNav {
                    flags: 0,
                    bbox_min,
                    bbox_max,
                    ref_start: 0,
                    ref_count: 0,
                    area_table_id: 0,
                    fog_indices: [0; 4],
                }
            })
            .collect();
        // Per-group walking-collision triangles for the current-group down-ray, indexed by absolute
        // group index (the flat collider below is fed from the same gather).
        let mut group_collision_tris: Vec<Vec<[[f32; 3]; 3]>> =
            vec![Vec::new(); root.group_count() as usize];
        // Per-group camera-only triangles (DETAIL set, NOCAMCOLLIDE clear) — the down-ray's
        // camera-void fallback set (decision 0692), disjoint from the walking gather above.
        let mut group_camera_only_tris: Vec<Vec<[[f32; 3]; 3]>> =
            vec![Vec::new(); root.group_count() as usize];
        // Collidable triangles flattened across every group (raw WMO-local coords), accumulated as we
        // read each group's bytes — no second pass / second read.
        let mut col_pos: Vec<[f32; 3]> = Vec::new();
        let mut col_idx: Vec<u32> = Vec::new();
        // Camera/LOS gather (keeps DETAIL, drops NOCAMCOLLIDE) — a separate mesh from the walking one.
        let mut cam_pos: Vec<[f32; 3]> = Vec::new();
        let mut cam_idx: Vec<u32> = Vec::new();
        // Per-group MODR (doodad ownership, transient — consumed into the doodad bases) + MOLR
        // (light refs — kept on the asset: the GameObject footprint lane gates its point lobes by
        // the hit group's list) + footprint face sets (interior groups only).
        let mut group_doodad_refs: Vec<Vec<u16>> = vec![Vec::new(); root.group_count() as usize];
        let mut group_light_refs: Vec<Vec<u16>> = vec![Vec::new(); root.group_count() as usize];
        let mut group_liquids: Vec<Option<LiquidMesh>> =
            (0..root.group_count()).map(|_| None).collect();
        let mut group_footprints: Vec<Option<FootprintTris>> =
            (0..root.group_count()).map(|_| None).collect();
        for gi in 0..root.group_count() {
            let group_url = format!("mpq://{stem}_{gi:03}.wmo");
            let Ok(gbytes) = ctx.read_asset_bytes(group_url).await else {
                continue; // a missing/unreadable group is skipped, like the chain reader
            };
            // Fill this group's flags + portal-ref span from its MOGP header (what the client's
            // visibility pass reads). Bounds were seeded from MOGI above.
            if let (Some(h), Some(nav)) =
                (wmo_group_header(&gbytes), group_nav.get_mut(gi as usize))
            {
                nav.flags = h.flags;
                nav.ref_start = h.portal_ref_start;
                nav.ref_count = h.portal_ref_count;
                nav.area_table_id = h.area_table_id;
                nav.fog_indices = h.fog_indices;
            }
            // Walking gather once per group: the per-group down-ray face set AND the flat collider
            // (appended with an index offset) come from the same buffers.
            let mut gpos: Vec<[f32; 3]> = Vec::new();
            let mut gidx: Vec<u32> = Vec::new();
            accumulate_wmo_group_collision(&gbytes, &mut gpos, &mut gidx);
            if let Some(tris) = group_collision_tris.get_mut(gi as usize) {
                for t in gidx.chunks_exact(3) {
                    if let (Some(&a), Some(&b), Some(&c)) = (
                        gpos.get(t[0] as usize),
                        gpos.get(t[1] as usize),
                        gpos.get(t[2] as usize),
                    ) {
                        tris.push([a, b, c]);
                    }
                }
            }
            let base = col_pos.len() as u32;
            col_pos.extend_from_slice(&gpos);
            col_idx.extend(gidx.iter().map(|i| i + base));
            accumulate_wmo_group_camera_collision(&gbytes, &mut cam_pos, &mut cam_idx);
            // Camera-only gather (the walking gather's DETAIL complement) — kept per group for the
            // down-ray's camera-void fallback (decision 0692).
            let (mut dpos, mut didx): (Vec<[f32; 3]>, Vec<u32>) = (Vec::new(), Vec::new());
            accumulate_wmo_group_camera_only_collision(&gbytes, &mut dpos, &mut didx);
            if let Some(tris) = group_camera_only_tris.get_mut(gi as usize) {
                for t in didx.chunks_exact(3) {
                    if let (Some(&a), Some(&b), Some(&c)) = (
                        dpos.get(t[0] as usize),
                        dpos.get(t[1] as usize),
                        dpos.get(t[2] as usize),
                    ) {
                        tris.push([a, b, c]);
                    }
                }
            }
            let subs = wmo_group_submeshes(&gbytes, &root).map_err(to_io)?;
            // This group's MODR (which MODD placements it owns/instantiates) + MOLR (which MOLT
            // lights fold into its doodads' committed light) — the per-doodad base resolution below
            // consumes both (wow-re trace-forensics-abbey-interior-d3d §1.1/§4).
            if let Some(slot) = group_doodad_refs.get_mut(gi as usize) {
                *slot = wmo_group_doodad_refs(&gbytes);
            }
            if let Some(slot) = group_light_refs.get_mut(gi as usize) {
                *slot = wmo_group_light_refs(&gbytes);
            }
            if let Some(slot) = group_footprints.get_mut(gi as usize) {
                *slot = wmo_group_footprint_tris(&gbytes);
            }
            if let Some(slot) = group_liquids.get_mut(gi as usize) {
                *slot = wmo_group_liquid_mesh(&gbytes);
            }
            for (j, sub) in subs.iter().enumerate() {
                let mesh =
                    ctx.add_labeled_asset(format!("group{gi}_sub{j}"), build_submesh_mesh(sub));
                // Lowercase the path: Bevy's loader lookup is case-sensitive and these tables carry
                // uppercase `.BLP`, so an uppercase extension falls back to type-based resolution —
                // ambiguous with Bevy's built-in image loader, which spams a "Multiple AssetLoaders
                // found" warning per texture. Lowercasing matches `BlpImageLoader`'s `blp` + dedupes case.
                let texture = sub.texture.as_deref().map(|t| {
                    ctx.load::<Image>(format!(
                        "mpq://{}",
                        t.replace('\\', "/").to_ascii_lowercase()
                    ))
                });
                submeshes.push(ModelSubmesh {
                    skinned_mesh: mesh.clone(), // WMO never skins; alias the static mesh (unused)
                    mesh,
                    texture,
                    skin_slot: sub.skin_slot, // always None for WMO groups (no creature skins)
                    geoset_id: 0,             // WMO has no M2 geoset concept
                    char_slot: None,          // WMO is never a character body
                    blend: sub.blend,
                    two_sided: sub.two_sided,
                    interior: sub.interior,
                    emissive: sub.emissive,
                    sidn: sub.sidn, // MOMT SIDN (0x10) — the authored night-glow colour
                    window: sub.window, // MOMT WINDOW (0x20) — the interior midpoint light
                    additive: sub.additive, // always false for WMO (additive WMO batches deferred)
                    no_depth_write: false, // WMO uses the standard opaque/transparent depth state
                    no_depth_test: false,
                    fog_policy: sub.fog_policy, // always Scene for WMO (remap_submesh's default)
                    billboard: None,            // WMO geometry isn't billboarded
                    alpha_anim: None,           // WMO batches carry no M2 colour/weight tracks
                    uv_anim: None,              // …nor texture transforms
                    rgb_anim: None,             // …nor M2Color tints
                    wmo_batch: sub.wmo_batch, // the MOBA section — an interior group's lighting law
                    ground_quad: None,        // the fx decal lane is M2-only
                });
                submesh_group.push(gi as u16);
            }
        }
        // Resolve every MODD placement's base once — placements, colours, MODR ownership, and MOLR
        // light lists are all fixed in the root/groups, so nothing here is ever re-derived. Both
        // MODR inversions come off the same refs: the first-owner one freezes the lighting lane at
        // create, the full-set one is the per-visible-group cull key.
        let doodad_owner = modr_owners(root.doodads().len(), &group_doodad_refs);
        let doodad_groups = modr_refs(root.doodads().len(), &group_doodad_refs);
        let doodad_base = resolve_doodad_bases(
            root.doodads(),
            root.group_infos(),
            &doodad_owner,
            &group_light_refs,
        );

        let collision = (!col_idx.is_empty()).then_some(CollisionMesh {
            positions: col_pos,
            indices: col_idx,
        });
        let collision_camera = (!cam_idx.is_empty()).then_some(CollisionMesh {
            positions: cam_pos,
            indices: cam_idx,
        });
        let portals = root.portals();
        let (group_collision_bounds, collision_bounds) =
            collision_tri_bounds(&group_collision_tris);
        let group_footprint_bounds = footprint_tri_bounds(&group_footprints);
        let group_collision_grids = collision_tri_grids(&group_collision_tris);
        let group_footprint_grids = footprint_tri_grids(&group_footprints);
        Ok(WmoModel {
            wmo_id: benilla_formats::wmo_root_id(&bytes),
            submeshes,
            submesh_group,
            portal_vertices: portals.vertices.clone(),
            portal_infos: portals.infos.clone(),
            portal_refs: portals.refs.clone(),
            group_nav,
            fogs: root.fogs().to_vec(),
            group_collision_tris,
            group_camera_only_tris,
            group_collision_bounds,
            group_collision_grids,
            collision_bounds,
            collision,
            collision_camera,
            doodads: root.doodads().to_vec(),
            doodad_sets: root.doodad_sets().to_vec(),
            lights: parse_wmo_lights(&bytes),
            group_bounds: root.group_infos().to_vec(),
            doodad_base,
            doodad_owner,
            doodad_groups,
            group_footprints,
            group_footprint_bounds,
            group_footprint_grids,
            group_light_refs,
            group_liquids,
        })
    }

    fn extensions(&self) -> &[&str] {
        &["wmo"]
    }
}

#[cfg(test)]
mod doodad_base_tests {
    use super::*;

    fn grp(interior: bool) -> WmoGroupInfo {
        WmoGroupInfo {
            interior,
            bbox_min: [-1.0; 3],
            bbox_max: [1.0; 3],
        }
    }

    fn prop(color: [u8; 4]) -> WmoDoodad {
        WmoDoodad {
            model: "candle.m2".into(),
            position: [0.0; 3],
            orientation: [0.0, 0.0, 0.0, 1.0],
            scale: 1.0,
            color,
        }
    }

    /// GOLDEN — the two abbey stands the reference trace decoded (wow-re
    /// `trace-forensics-abbey-interior-d3d` §1): MODD[18] colour (78,76,134) → ambient (56,55,96)
    /// diffuse raw; MODD[24] colour (90,86,141) → ambient (61,59,96) diffuse raw. Both bit-exact
    /// through the `0x6a77e0` fixed-point cap (scales 182 and 173).
    #[test]
    fn cap96_matches_the_decoded_abbey_stands_bit_exact() {
        let as_bytes = |c: [f32; 3]| c.map(|v| (v * 255.0).round() as u8);
        assert_eq!(as_bytes(cap96([78, 76, 134])), [56, 55, 96]);
        assert_eq!(as_bytes(cap96([90, 86, 141])), [61, 59, 96]);
        // max ≤ 96 passes through raw.
        assert_eq!(as_bytes(cap96([96, 40, 20])), [96, 40, 20]);
        // The diffuse floor: max ≥ 112 is RAW (both stands), below 112 raises hue-preserving.
        assert_eq!(as_bytes(floor112([78, 76, 134])), [78, 76, 134]);
        assert_eq!(as_bytes(floor112([90, 86, 141])), [90, 86, 141]);
        let raised = as_bytes(floor112([56, 28, 14]));
        assert_eq!(raised[0], 112); // max channel lands on the 112 value
        assert_eq!(raised, [112, 56, 28]); // pure scale — hue/saturation preserved
        assert_eq!(as_bytes(floor112([0, 0, 0])), [0, 0, 0]); // black stays black (no divide)
    }

    #[test]
    fn floor168_matches_the_decoded_abbey_benches_bit_exact() {
        // wow-re `wmo-lit-selector.md`: the INNBENCH GameObject draws (14150796/14150859) decode to
        // ambient = the raw MOCV (max ≤ 96 → cap96 pass-through) and diffuse = floor168(MOCV) with
        // TRUNCATING rounding — 63·168/83 = 127.52 lands on 127 (nearest would give 128).
        let as_bytes = |c: [f32; 3]| c.map(|v| (v * 255.0).round() as u8);
        assert_eq!(as_bytes(cap96([59, 65, 92])), [59, 65, 92]);
        assert_eq!(as_bytes(cap96([69, 63, 83])), [69, 63, 83]);
        assert_eq!(as_bytes(floor168([59, 65, 92])), [107, 118, 168]);
        assert_eq!(as_bytes(floor168([69, 63, 83])), [139, 127, 168]);
        // max ≥ 168 passes through raw.
        assert_eq!(as_bytes(floor168([200, 30, 10])), [200, 30, 10]);
    }

    /// Ownership is MODR, never spatial: an interior-group doodad takes its MODD colour (cap96
    /// ambient / floor112 diffuse) + its OWNING group's MOLR; an exterior-group doodad is sky-lit;
    /// an unreferenced MODD (never instantiated by the reference) defaults exterior.
    #[test]
    fn modr_ownership_picks_the_lane_and_the_light_refs() {
        let doodads = vec![
            prop([90, 86, 141, 255]), // owned by interior group 0
            prop([90, 86, 141, 255]), // owned by exterior group 1
            prop([90, 86, 141, 255]), // referenced by no group
        ];
        let groups = [grp(true), grp(false)];
        let modr = vec![vec![0u16], vec![1u16]];
        let molr = vec![vec![7u16, 9u16], vec![3u16]];
        let owner = modr_owners(doodads.len(), &modr);
        // The same inversion the portal cull keys props on (decision 0689): owned props name their
        // group, the unreferenced one names none — so it is the one prop the cull can't hide.
        assert_eq!(owner, vec![Some(0), Some(1), None]);
        let bases = resolve_doodad_bases(&doodads, &groups, &owner, &molr);
        match &bases[0] {
            DoodadBase::Interior(b) => {
                assert_eq!(b.light_refs, vec![7, 9]);
                assert_eq!(b.diffuse.map(|v| (v * 255.0).round() as u8), [90, 86, 141]);
                assert_eq!(b.ambient.map(|v| (v * 255.0).round() as u8), [61, 59, 96]);
            }
            other => panic!("interior-group doodad must take the MODD-colour base, got {other:?}"),
        }
        assert_eq!(bases[1], DoodadBase::Exterior);
        assert_eq!(bases[2], DoodadBase::Exterior);
    }

    /// A group with NO MOLR chunk gives its doodads an EMPTY light set — the abbey's group 3 case:
    /// the director's stand receives zero point light, its own flame included (machine-zero in the
    /// reference decode).
    #[test]
    fn no_molr_means_no_point_lights_at_all() {
        let bases = resolve_doodad_bases(
            &[prop([120, 110, 150, 255])],
            &[grp(true)],
            &[Some(0)],
            &[Vec::new()],
        );
        match &bases[0] {
            DoodadBase::Interior(b) => assert!(b.light_refs.is_empty()),
            other => panic!("expected interior base, got {other:?}"),
        }
    }

    /// A MODD named by several groups resolves to its **first** referencing group, and the mapping is
    /// dense over the doodad list — the LIGHTING lane keys off this (the create that freezes the base
    /// runs once, on the first visible-group walk to reach it). Out-of-range MODR entries (a
    /// malformed group) are ignored rather than panicking.
    #[test]
    fn a_shared_modd_is_lit_by_its_first_referencing_group() {
        // g0 names doodad 2; g1 names 0 and 2; g2 names 1 — plus a ref past the end of the list.
        let modr = vec![vec![2u16], vec![0u16, 2u16], vec![1u16, 99u16]];
        assert_eq!(
            modr_owners(3, &modr),
            vec![Some(1), Some(2), Some(0)],
            "doodad 2 is named by g0 and g1 — g0 wins"
        );
        // No groups at all ⇒ nothing is owned, and nothing is cullable.
        assert_eq!(modr_owners(2, &[]), vec![None, None]);
    }

    /// The CULL key is the whole referrer set, not the first: the draw loop runs per *visible* group
    /// over that group's MODR, so a prop several rooms name is drawn from every one of them. Keying
    /// it to the first owner instead is what blinked Blackrock's and the Great Forge's lava falls
    /// out by camera angle — real files name those props from 9–19 groups apiece.
    #[test]
    fn the_cull_key_is_every_referencing_group() {
        // Same fixture as above: g0 names 2; g1 names 0 and 2; g2 names 1 + an out-of-range ref.
        let modr = vec![vec![2u16], vec![0u16, 2u16], vec![1u16, 99u16]];
        let refs = modr_refs(3, &modr);
        assert_eq!(&*refs[0], &[1], "doodad 0 is named by g1 alone");
        assert_eq!(&*refs[1], &[2]);
        assert_eq!(
            &*refs[2],
            &[0, 1],
            "doodad 2 hangs in two rooms — either one draws it"
        );
        // A group naming the same doodad twice contributes it once; no groups ⇒ no key at all (the
        // prop is uncullable, matching `modr_owners`' `None`).
        assert_eq!(&*modr_refs(1, &[vec![0u16, 0u16]])[0], &[0]);
        assert!(modr_refs(2, &[])[0].is_empty());
    }
}
