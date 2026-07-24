//! The mouseover pick — which unit is under the cursor, recomputed each frame into
//! [`super::Hovered`] the way the real client finds it (wow-re pick-volume RE `bd630be` +
//! `31562f1d`): a **broad phase** ray-vs-sphere on the current animation's bounds, then a
//! **narrow phase** ray-vs-triangle against the unit's **posed render mesh**, with the
//! reference's generous +1-model-unit halo retry when nothing hits exactly. The selection /
//! interaction story that consumes the verdict lives in the [`super`] module doc.

use std::collections::HashSet;

use benilla_assets::ModelAnimations;
use benilla_protocol::EntityKind;
use bevy::camera::primitives::Aabb;
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::mesh::{Indices, VertexAttributeValues};
use bevy::picking::mesh_picking::ray_cast::MeshRayCast;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::collision::PickOccluder;
use crate::creature_anim::AnimDriver;
use crate::debug_panel::ModelKind;
use crate::interact::WorldObject;
use crate::net::{Guid, NetEntity, ObjectStore, SelfPlayer};
use crate::player::{CameraControl, WorldCamera};
use crate::ui_script::PointerOverUi;

use super::{Hovered, HoveredObject, PickOcclusion};

/// Trace this frame's **world-occlusion distance** (the reference's `CWorld::Intersect` leg of the
/// scene trace `0x480df0` — wow-re selection-circle PART 3, §5-cross-checked 2026-07-20): one
/// physics ray from the cursor through the [`PickOccluder`] set (terrain, the WMO walk-bake faces
/// ≈ the byte-decoded `0x84` reject-mask, static doodad hulls). Both object picks below run
/// **unbounded** and post-compare against it — the reference discards the object hit iff the world
/// hit is *strictly* nearer (`0x480eb4`: tie keeps the object). No cursor / mouselook →
/// `INFINITY` (the picks bail on their own guards). The occluder set deliberately excludes net
/// entities (units, GameObject hulls, transports) — they are the *object* trace; a chest must not
/// occlude itself.
pub(super) fn update_pick_occlusion(
    camera: Query<(&Camera, &GlobalTransform), With<WorldCamera>>,
    window: Query<&Window, With<PrimaryWindow>>,
    spatial: avian3d::prelude::SpatialQuery,
    occluders: Query<(), With<PickOccluder>>,
    mut occlusion: ResMut<PickOcclusion>,
) {
    occlusion.distance = f32::INFINITY;
    let (Ok((camera, cam_tf)), Ok(window)) = (camera.single(), window.single()) else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let Ok(ray) = camera.viewport_to_world(cam_tf, cursor) else {
        return;
    };
    if let Some(hit) = spatial.cast_ray_predicate(
        ray.origin,
        ray.direction,
        f32::MAX,
        true,
        // Default (terrain/doodads) + the WMO *walk* bake — the mask that reaches every
        // `PickOccluder`-marked collider; the marker picks the exact occluder set within it.
        &crate::collision::player_query_filter(),
        &|e| occluders.contains(e),
    ) {
        occlusion.distance = hit.distance;
    }
}

/// Recompute the unit under the cursor each frame — the real client's two-phase pick (wow-re
/// pick-volume RE `bd630be` + `31562f1d`): **broad** = the cursor ray vs the *current animation's*
/// bounds sphere (world-placed + world-scaled, no pad); **pass 1** = the ray vs the unit's **posed
/// render mesh** (each drawn vertex skinned through the live joint pose, per-triangle), nearest
/// world-distance hit wins; **pass 2, only when pass 1 hit nothing anywhere** (the mouse pick's
/// generous retry — never LoS/spell traces): the same posed mesh with every vertex displaced
/// **+1 model-unit along its skinned normal** (a ~1-yd halo, fattest where the body is widest),
/// resolved by the reference's priority ladder — last frame's pick sticks (anti-flicker), else an
/// alive unit beats a dead one even when farther, ties by distance. A unit with no skinned parts
/// (cube fallback) keeps the interim render-mesh AABB test, competing at pass-1 level. Inert while
/// mouse-looking (cursor hidden) or over the dev UI.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(super) fn update_hover(
    camera: Query<(&Camera, &GlobalTransform), With<WorldCamera>>,
    window: Query<&Window, With<PrimaryWindow>>,
    rig: Res<CameraControl>,
    pointer_over_ui: Res<PointerOverUi>,
    // The frame's world-occlusion distance: the final pick is discarded iff the world hit is
    // strictly nearer (behind terrain/WMO/doodad geometry — the post-hoc compare below).
    occlusion: Res<PickOcclusion>,
    // The V-plates' screen rects (last frame's layout — the plate drive runs after this chain).
    plate_rects: Res<crate::vplates::PlateRects>,
    mut hovered: ResMut<Hovered>,
    mesh_assets: Res<Assets<Mesh>>,
    ibp_assets: Res<Assets<SkinnedMeshInverseBindposes>>,
    // Last frame's pick, for pass 2's sticky-hover (the reference's anti-flicker cache: the previous
    // pick outranks everything in the halo retry, so the hover doesn't strobe between two units).
    mut last_pick: Local<Option<Entity>>,
    // Unit roots: pose + scale, the playing animation (for its bounds sphere), the descriptor store
    // (alive-vs-dead sets the pass-2 priority), and the part children.
    roots: Query<
        (
            Entity,
            &GlobalTransform,
            &NetEntity,
            Option<&ModelAnimations>,
            Option<&AnimDriver>,
            Option<&ObjectStore>,
            Option<&Children>,
            Option<&crate::entities::mount::MountChild>,
        ),
        (With<Guid>, Without<SelfPlayer>),
    >,
    // A mounted unit's mount-child part children (decision 0441): the mount's geometry joins the
    // unit's pick set — the reference draws mount + rider as one clickable unit.
    child_sets: Query<&Children>,
    // A part child's mesh + skinned binding (the joints its vertices pose through).
    parts: Query<(&Mesh3d, &SkinnedMesh)>,
    joint_tfs: Query<&GlobalTransform>,
    // Fallback path: a unit's pickable mesh children — `WorldObject` kind, model-local `Aabb`, world
    // transform, and the link to the streamed parent (whose `Guid` we resolve the hit to).
    meshes: Query<(&ChildOf, &Aabb, &GlobalTransform, &WorldObject)>,
    units: Query<&Guid, Without<SelfPlayer>>,
) {
    hovered.target = None;
    hovered.guid = None;
    hovered.distance = f32::MAX;
    if rig.is_looking() || pointer_over_ui.0 {
        *last_pick = None;
        return;
    }
    let (Ok((camera, cam_tf)), Ok(window)) = (camera.single(), window.single()) else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    // The plate is mouse-enabled UI on the reference: the cursor inside a plate's rect makes
    // that plate's unit the mouseover (OnEnter `0x7cb850` → `[0xb4e2c8]`) and the frame walk
    // stops there — no world pick behind it. So plate hover brightens the model, lights the
    // plate bar, and click-selects, exactly like hovering the body. Reverse order = the later-drawn
    // (visually topmost) plate wins an overlap.
    if let Some(&(_, entity)) = plate_rects.0.iter().rev().find(|(r, _)| r.contains(cursor)) {
        hovered.target = Some(entity);
        hovered.guid = units.get(entity).ok().map(|g| g.0);
        hovered.distance = 0.0; // a plate is topmost UI — it beats any world GameObject at a tie
        *last_pick = Some(entity);
        return;
    }
    let Ok(ray) = camera.viewport_to_world(cam_tf, cursor) else {
        return;
    };
    let (origin, dir) = (ray.origin, *ray.direction);
    let limit = occlusion.distance;

    // ── The faithful path: collect broad-sphere-passing candidates, then pass 1 / pass 2 ────────
    // A pass-2-eligible candidate: the unit, its pass-2 priority (alive 3 / dead 2 — the lootable
    // refinement awaits loot state), its parts' mesh ids, and its posed joint palette.
    let mut candidates: Vec<(Entity, u8, Vec<AssetId<Mesh>>, Vec<Mat4>)> = Vec::new();
    // Units the faithful path *owns* (they have skinned parts): excluded from the AABB fallback
    // even when the broad phase rejects them — the reference wouldn't click them there either.
    let mut faithful: HashSet<Entity> = HashSet::new();
    for (entity, gt, net, anims, drv, store, children, mount_child) in &roots {
        if !matches!(net.kind, EntityKind::Unit | EntityKind::Player) {
            continue;
        }
        let Some(children) = children else { continue };
        let skinned: Vec<_> = children.iter().filter_map(|c| parts.get(c).ok()).collect();
        if skinned.is_empty() {
            continue; // static/cube unit → the AABB fallback below
        }
        faithful.insert(entity);
        // Broad phase: the playing sequence's bounds sphere, world-placed + world-scaled (uniform
        // scale — the root bakes `OBJECT_FIELD_SCALE_X`). An unknown clip / unauthored radius passes
        // through (the reference falls back to the header sphere; permissive is the safe direction).
        // A MOUNTED unit passes through too: the rider's Mount-pose sphere doesn't cover the
        // mount's body (the reference recomputes the pick radius from the mount's box — a P2
        // refinement, decision 0441).
        let clip = match (anims, drv) {
            (Some(a), Some(d)) => d.active_anim().and_then(|id| a.find(id)),
            _ => None,
        };
        if let Some(c) = clip.filter(|c| c.bounds_radius > 0.0 && mount_child.is_none()) {
            let centre = gt.transform_point(c.bounds_center);
            let radius = c.bounds_radius * gt.scale().max_element();
            // Ray-sphere: reject when the ray's closest approach to the centre exceeds the radius
            // (dir is normalized; a centre behind the origin projects to t=0, keeping units we
            // stand inside clickable).
            let along = (centre - origin).dot(dir).max(0.0);
            if (centre - origin - dir * along).length_squared() > radius * radius {
                continue;
            }
        }
        // The posed joint palette (parts of one skeleton share the joint set): the same
        // world-from-bind-pose matrices GPU skinning applies.
        let palette_of = |sk: &[(&Mesh3d, &SkinnedMesh)]| -> Option<Vec<Mat4>> {
            sk.first().and_then(|(_, skin)| {
                let ibp = ibp_assets.get(&skin.inverse_bindposes)?;
                Some(
                    skin.joints
                        .iter()
                        .zip(ibp.iter())
                        .map(|(&j, ibp)| {
                            joint_tfs.get(j).map_or(Mat4::IDENTITY, |t| t.to_matrix()) * *ibp
                        })
                        .collect(),
                )
            })
        };
        let Some(palette) = palette_of(&skinned) else {
            continue;
        };
        let mesh_ids = skinned.iter().map(|(m, _)| m.id()).collect();
        let priority = if store.is_some_and(|s| s.0.unit_is_dead()) {
            2
        } else {
            3
        };
        candidates.push((entity, priority, mesh_ids, palette));
        // The mount child's parts join as a second candidate resolving to the SAME unit
        // (decision 0441): its own skeleton, its own palette, one hoverable whole.
        if let Some(mc) = mount_child {
            if let Ok(mc_children) = child_sets.get(mc.0) {
                let mount_skinned: Vec<_> = mc_children
                    .iter()
                    .filter_map(|c| parts.get(c).ok())
                    .collect();
                if let Some(mount_palette) = palette_of(&mount_skinned) {
                    let mount_mesh_ids = mount_skinned.iter().map(|(m, _)| m.id()).collect();
                    candidates.push((entity, priority, mount_mesh_ids, mount_palette));
                }
            }
        }
    }

    // Pass 1 — the exact posed render mesh, pure nearest-wins (the reference's first pass).
    // UNBOUNDED, like the reference: the object trace never carries the world distance; occlusion
    // is one strict compare on the final result (below).
    let mut best: Option<(f32, Entity)> = None;
    for (entity, _, mesh_ids, palette) in &candidates {
        for id in mesh_ids {
            let Some(t) = ray_posed_mesh(&mesh_assets, *id, palette, origin, dir, false) else {
                continue;
            };
            if best.is_none_or(|(bt, _)| t < bt) {
                best = Some((t, *entity));
            }
        }
    }

    // ── Fallback for skinless units only: the model-bounds box test (pre-RE interim) ────────────
    // A unit's parts are separate meshes; testing each and keeping the closest parent is equivalent
    // to testing the union. `dir` is normalized, so `t` is a world distance like the hits above.
    for (child_of, aabb, gt, obj) in &meshes {
        if obj.kind != ModelKind::Creature {
            continue;
        }
        let parent = child_of.parent();
        if faithful.contains(&parent) || units.get(parent).is_err() {
            continue; // posed-mesh-tested above, or not a targetable unit (e.g. a GameObject part)
        }
        let (min, max) = world_aabb(aabb, gt);
        if let Some(t) = ray_aabb(origin, dir, min, max) {
            if best.is_none_or(|(bt, _)| t < bt) {
                best = Some((t, parent));
            }
        }
    }

    // Pass 2 — nothing exactly hit: the reference's generous retry (mouse-pick only). Same
    // candidates, every vertex displaced +1 model-unit along its skinned normal — a ~1-yd halo
    // around the drawn body. Accept by the priority ladder, not pure distance: last frame's pick
    // sticks (anti-flicker), else higher priority wins even when farther, ties by distance.
    if best.is_none() {
        let mut best2: Option<(f32, u32, Entity)> = None;
        for (entity, priority, mesh_ids, palette) in &candidates {
            let hit = mesh_ids
                .iter()
                .filter_map(|id| ray_posed_mesh(&mesh_assets, *id, palette, origin, dir, true))
                .min_by(f32::total_cmp);
            let Some(t) = hit else { continue };
            let prio = if *last_pick == Some(*entity) {
                u32::MAX // the sticky-hover cache outranks everything
            } else {
                *priority as u32
            };
            let wins = best2.is_none_or(|(bt, bp, _)| prio > bp || (prio == bp && t < bt));
            if wins {
                best2 = Some((t, prio, *entity));
            }
        }
        best = best2.map(|(t, _, e)| (t, e));
    }

    // The occlusion verdict — the reference's single strict compare (`0x480df0` @ `0x480eb4`):
    // the object hit is discarded iff the world hit is STRICTLY nearer (a tie keeps the object).
    if best.is_some_and(|(t, _)| limit < t) {
        best = None;
    }
    if let Some((t, entity)) = best {
        hovered.target = Some(entity);
        hovered.guid = units.get(entity).ok().map(|g| g.0);
        hovered.distance = t;
    }
    *last_pick = best.map(|(_, e)| e);
}

/// Recompute the **GameObject** under the cursor each frame into [`HoveredObject`] (decision 0236):
/// a mesh-accurate ray pick against GameObject parts *only*, reusing the inspector's picker
/// ([`crate::interact::pick_at_cursor`] — bevy `MeshRayCast`, which hits the colliderless props a
/// physics ray misses). Kept separate from [`update_hover`]'s unit pick because a GameObject is
/// usable-but-not-*selected*: this drives the Interact cursor and the right-click USE, never
/// selection. Inert while mouse-looking (cursor hidden) or over the dev UI, exactly like the unit
/// pick. Cheap — only the handful of GO parts on screen are in the pick set.
#[allow(clippy::too_many_arguments)]
pub(super) fn update_hovered_object(
    camera: Query<(&Camera, &GlobalTransform), With<WorldCamera>>,
    window: Query<&Window, With<PrimaryWindow>>,
    rig: Res<CameraControl>,
    pointer_over_ui: Res<PointerOverUi>,
    occlusion: Res<PickOcclusion>,
    mut hovered: ResMut<HoveredObject>,
    // Every pickable GameObject part carries `WorldObject { kind: GameObject }` (units/doodads/WMOs
    // carry other kinds and are excluded — units have their own posed-mesh pick).
    go_parts: Query<(Entity, &WorldObject)>,
    child_of: Query<&ChildOf>,
    guids: Query<&Guid>,
    stores: Query<&ObjectStore>,
    mut ray_cast: MeshRayCast,
) {
    hovered.target = None;
    hovered.guid = None;
    hovered.distance = f32::MAX;
    if rig.is_looking() || pointer_over_ui.0 {
        return;
    }
    let (Ok((camera, cam_tf)), Ok(window)) = (camera.single(), window.single()) else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    // `pick_at_cursor` takes bevy's `HashSet` (not the `std` one this module uses elsewhere).
    // A transport-family GO (TRANSPORT 11 / MAP_OBJECT 14 / MO_TRANSPORT 15) never joins the pick
    // set: in the reference it has no pick geometry at all — the GO model resolver `0x5f80e0`
    // returns 0 for these three types (they render via the WMO/spline path, not an M2) and their
    // strategy's interaction predicates are constant-false (w2c1 + go-render-gate + the +0x14
    // vtable dump). Concretely: a ship's hull must not swallow the pick — an NPC on deck stays
    // hoverable through the railing, and the hull shows no gear or tooltip.
    let pickable: bevy::platform::collections::HashSet<Entity> = go_parts
        .iter()
        .filter(|(_, o)| o.kind == ModelKind::GameObject)
        .filter(|(e, _)| {
            let root = child_of.get(*e).map_or(*e, |c| c.parent());
            !stores
                .get(root)
                .is_ok_and(|s| matches!(s.0.gameobject_type_id(), 11 | 14 | 15))
        })
        .map(|(e, _)| e)
        .collect();
    if pickable.is_empty() {
        return;
    }
    let Some((hit, _point, distance)) =
        crate::interact::pick_at_cursor(cursor, camera, cam_tf, &pickable, &mut ray_cast)
    else {
        return;
    };
    // The occlusion verdict (`0x480df0` @ `0x480eb4`): discard the GO hit iff the world hit is
    // STRICTLY nearer — a tie keeps the object. (Monotonic: any other GO hit is farther still.)
    if occlusion.distance < distance {
        return;
    }
    // The hit is a mesh part; its GameObject net entity (which carries the `Guid`) is one `ChildOf`
    // hop up — the attach convention (the same one `update_hover`'s fallback walks). Guard the rare
    // case the pickable itself is the guid-bearing entity.
    let net_entity = if guids.contains(hit) {
        hit
    } else {
        child_of.get(hit).map_or(hit, |c| c.parent())
    };
    let Ok(guid) = guids.get(net_entity) else {
        return;
    };
    hovered.target = Some(net_entity);
    hovered.guid = Some(guid.0);
    hovered.distance = distance;
}

/// The narrow phase for one part: skin its vertices through `palette` (world-from-bind-pose joint
/// matrices — the same transform GPU skinning applies) and ray-test every triangle. With `inflate`
/// (the reference's pass 2), each vertex is additionally displaced by its **skinned normal, un-
/// normalized** — the M2 normal is unit-length and the palette carries the world scale, so the
/// halo is 1 model-unit × scale outward, exactly the binary's `skinned_pos + rot(palette)·normal`
/// with no extra constant. Returns the nearest world-space hit distance, or `None`. Cost is
/// bounded by the broad phase: only units near the cursor get here.
fn ray_posed_mesh(
    mesh_assets: &Assets<Mesh>,
    mesh_id: AssetId<Mesh>,
    palette: &[Mat4],
    origin: Vec3,
    dir: Vec3,
    inflate: bool,
) -> Option<f32> {
    let mesh = mesh_assets.get(mesh_id)?;
    let VertexAttributeValues::Float32x3(positions) = mesh.attribute(Mesh::ATTRIBUTE_POSITION)?
    else {
        return None;
    };
    let VertexAttributeValues::Uint16x4(joints) = mesh.attribute(Mesh::ATTRIBUTE_JOINT_INDEX)?
    else {
        return None;
    };
    let VertexAttributeValues::Float32x4(weights) = mesh.attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT)?
    else {
        return None;
    };
    let normals = match mesh.attribute(Mesh::ATTRIBUTE_NORMAL) {
        Some(VertexAttributeValues::Float32x3(n)) if inflate => Some(n),
        _ if inflate => return None, // can't build the halo without normals
        _ => None,
    };
    // Skin every vertex to world space once (blended matrix, as GPU skinning sums it), then walk
    // the index triangles. Pass 2 adds the rotated normal, translation-free, to the position.
    let world: Vec<Vec3> = positions
        .iter()
        .enumerate()
        .zip(joints.iter().zip(weights.iter()))
        .map(|((i, p), (j, w))| {
            let mut m = Mat4::ZERO;
            for k in 0..4 {
                if w[k] > 0.0 {
                    if let Some(mk) = palette.get(j[k] as usize) {
                        m += *mk * w[k];
                    }
                }
            }
            let mut out = (m * Vec4::new(p[0], p[1], p[2], 1.0)).truncate();
            if let Some(ns) = normals {
                let n = ns[i];
                out += m.transform_vector3(Vec3::new(n[0], n[1], n[2]));
            }
            out
        })
        .collect();
    let tri = |a: usize, b: usize, c: usize| -> Option<f32> {
        ray_triangle(origin, dir, &[world[a], world[b], world[c]])
    };
    let hits = match mesh.indices()? {
        Indices::U16(ix) => ix
            .chunks_exact(3)
            .filter_map(|c| tri(c[0] as usize, c[1] as usize, c[2] as usize))
            .fold(None::<f32>, |acc, t| Some(acc.map_or(t, |a| a.min(t)))),
        Indices::U32(ix) => ix
            .chunks_exact(3)
            .filter_map(|c| tri(c[0] as usize, c[1] as usize, c[2] as usize))
            .fold(None::<f32>, |acc, t| Some(acc.map_or(t, |a| a.min(t)))),
    };
    hits
}

/// Ray–triangle intersection (Möller–Trumbore), **two-sided**, returning `t ≥ 0` along `dir`
/// (unnormalized is fine — `t` is in `dir` lengths) or `None` on miss/parallel. Two-sided because a
/// posed mesh can present back faces at silhouette edges and a generous pick beats a strict one.
fn ray_triangle(origin: Vec3, dir: Vec3, tri: &[Vec3; 3]) -> Option<f32> {
    let (e1, e2) = (tri[1] - tri[0], tri[2] - tri[0]);
    let p = dir.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-8 {
        return None;
    }
    let inv = 1.0 / det;
    let s = origin - tri[0];
    let u = s.dot(p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(e1);
    let v = dir.dot(q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = e2.dot(q) * inv;
    (t >= 0.0).then_some(t)
}

/// A mesh's model-local [`Aabb`] transformed into a world-space axis-aligned box `(min, max)` — its 8
/// corners run through the entity's world transform, then min/max'd. (An AABB of the rotated box: a hair
/// larger than the true oriented box, which only makes the hover more forgiving.)
fn world_aabb(aabb: &Aabb, gt: &GlobalTransform) -> (Vec3, Vec3) {
    let center = Vec3::from(aabb.center);
    let he = Vec3::from(aabb.half_extents);
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for sx in [-1.0, 1.0] {
        for sy in [-1.0, 1.0] {
            for sz in [-1.0, 1.0] {
                let world = gt.transform_point(center + he * Vec3::new(sx, sy, sz));
                min = min.min(world);
                max = max.max(world);
            }
        }
    }
    (min, max)
}

/// Ray vs axis-aligned box (the slab test): the entry distance along `dir` if the ray hits, else `None`.
/// `dir` need not be normalized; a zero component is handled by the infinities `recip` produces.
fn ray_aabb(origin: Vec3, dir: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let inv = dir.recip();
    let t1 = (min - origin) * inv;
    let t2 = (max - origin) * inv;
    let tmin = t1.min(t2).max_element();
    let tmax = t1.max(t2).min_element();
    (tmax >= tmin.max(0.0)).then_some(tmin.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRI: [Vec3; 3] = [
        Vec3::new(-1.0, 0.0, -1.0),
        Vec3::new(1.0, 0.0, -1.0),
        Vec3::new(0.0, 0.0, 1.0),
    ];

    #[test]
    fn ray_triangle_hits_at_the_right_distance() {
        let t = ray_triangle(Vec3::new(0.0, 5.0, 0.0), Vec3::NEG_Y, &TRI).expect("hit");
        assert!((t - 5.0).abs() < 1e-5);
    }

    #[test]
    fn ray_triangle_is_two_sided() {
        // Same triangle hit from below (reversed winding relative to the ray) still connects.
        assert!(ray_triangle(Vec3::new(0.0, -5.0, 0.0), Vec3::Y, &TRI).is_some());
    }

    #[test]
    fn ray_triangle_misses_outside_and_behind() {
        // Outside the triangle's extent.
        assert!(ray_triangle(Vec3::new(3.0, 5.0, 0.0), Vec3::NEG_Y, &TRI).is_none());
        // Triangle behind the ray origin (t < 0).
        assert!(ray_triangle(Vec3::new(0.0, 5.0, 0.0), Vec3::Y, &TRI).is_none());
        // Parallel to the plane.
        assert!(ray_triangle(Vec3::new(0.0, 5.0, 0.0), Vec3::X, &TRI).is_none());
    }
}
