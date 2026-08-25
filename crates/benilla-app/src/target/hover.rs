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
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::creature_anim::AnimDriver;
use crate::net::{Guid, NetEntity, ObjectStore, SelfPlayer};
use crate::player::CameraControl;
use crate::ui_script::PointerOverUi;
use benilla_world::billboard::BillboardCard;
use benilla_world::collision::PickOccluder;
use benilla_world::interact::{
    ray_mesh_bounds, ray_posed_mesh, CreaturePickPart, GoPickPart, PickParts,
};
use benilla_world::view::WorldCamera;

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
    occlusion.point = None;
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
        &benilla_world::collision::WorldCollision::body_filter(),
        &|e| occluders.contains(e),
    ) {
        occlusion.distance = hit.distance;
        occlusion.point = Some(ray.origin + *ray.direction * hit.distance);
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
    // The owned palette table (decision 0720): the picker reads the same world-space matrices
    // the vertex stage skins with, straight from the CPU rows.
    palettes: Res<benilla_world::rig_palette::RigPalettes>,
    rigs: Query<&benilla_world::rig_palette::RigSkin>,
    // Last frame's pick, for pass 2's sticky-hover (the reference's anti-flicker cache: the previous
    // pick outranks everything in the halo retry, so the hover doesn't strobe between two units).
    mut last_pick: Local<Option<Entity>>,
    // Unit roots: pose + scale, the playing animation (for its bounds sphere), the descriptor store
    // (alive-vs-dead sets the pass-2 priority), the part children — and whether the body is drawn
    // at all. **You cannot click what is not in the scene** (decision 1277): a body the
    // exterior-scene election sent to pass 2 never reaches the reference's draw list, so it takes
    // no mouseover, no sword cursor and no click. This lane used to ray-test every unit root with a
    // `Guid` and never ask, which is how the director could still target Tanaris mobs through a
    // sealed cavern ceiling after their models had correctly stopped drawing.
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
            Option<&InheritedVisibility>,
        ),
        (With<Guid>, Without<SelfPlayer>),
    >,
    // A mounted unit's mount-child part children (decision 0441): the mount's geometry joins the
    // unit's pick set — the reference draws mount + rider as one clickable unit.
    child_sets: Query<&Children>,
    // A part child's mesh + its palette-rig link (the rig its vertices pose through).
    parts: Query<(&Mesh3d, &benilla_world::rig_palette::RigPart)>,
    // Fallback path: a unit's pickable mesh children, by [`CreaturePickPart`] (stamped beside the
    // part's `WorldObject` at the attach sites, fallback cube included — the archetype filter that
    // replaced a per-frame kind-compare over every streamed row) — model-local `Aabb`, world
    // transform, and the link to the streamed parent (whose `Guid` we resolve the hit to).
    meshes: Query<
        (
            &ChildOf,
            &Aabb,
            &GlobalTransform,
            Option<&InheritedVisibility>,
        ),
        With<CreaturePickPart>,
    >,
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
    for (entity, gt, net, anims, drv, store, children, mount_child, drawn) in &roots {
        if !matches!(net.kind, EntityKind::Unit | EntityKind::Player) {
            continue;
        }
        // Not drawn ⇒ not clickable (see the query's note). Deliberately BEFORE `faithful.insert`
        // below, so an undrawn unit is not claimed by the faithful path either — otherwise it
        // would merely be excluded from the AABB fallback and stay unpickable by accident rather
        // than by rule.
        if !drawn.is_none_or(|v| v.get()) {
            continue;
        }
        let Some(children) = children else { continue };
        // Ownership probe only — no collect yet. The `skinned` Vec used to be built HERE, for
        // every drawn unit in the scene, ahead of the few-FLOP broad phase that rejects nearly
        // all of them (1473 §3's hover row); the survivors alone pay it now, below. Membership
        // in `faithful` must still be decided pre-reject — a broad-phase-rejected skinned unit
        // stays excluded from the AABB fallback by RULE, not by accident.
        if !children.iter().any(|c| parts.contains(c)) {
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
        let skinned: Vec<_> = children.iter().filter_map(|c| parts.get(c).ok()).collect();
        // The posed joint palette (parts of one skeleton share the rig): the same
        // world-from-bind-pose matrices GPU skinning applies, read from the owned palette rows
        // (decision 0720 — last frame's propagated pose, exactly what the previous
        // joint-GlobalTransform read gave).
        let palette_of =
            |sk: &[(&Mesh3d, &benilla_world::rig_palette::RigPart)]| -> Option<Vec<Mat4>> {
                sk.first().and_then(|(_, part)| {
                    let rig = rigs.get(part.0).ok()?;
                    palettes.world_palette(rig.slot, rig.bones() as usize)
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
    for (child_of, aabb, gt, drawn) in &meshes {
        // …and the same rule on the skinless fallback: a part inherits its root's pass-2 verdict.
        if !drawn.is_none_or(|v| v.get()) {
            continue;
        }
        let parent = child_of.parent();
        if faithful.contains(&parent) || units.get(parent).is_err() {
            continue; // posed-mesh-tested above, or not a targetable unit (e.g. the self player)
        }
        if let Some(t) = ray_mesh_bounds(origin, dir, aabb, gt) {
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
/// (the resident-geometry caster of decision 0857, which hits the colliderless props a physics ray
/// misses and the `RENDER_WORLD`-only static forms Bevy's `MeshRayCast` lost at 0834). Kept
/// separate from [`update_hover`]'s unit pick because a GameObject is usable-but-not-*selected*:
/// this drives the Interact cursor and the right-click USE, never selection. Inert while
/// mouse-looking (cursor hidden) or over the dev UI, exactly like the unit pick. Cheap — only the
/// handful of GO parts on screen are in the pick set.
///
/// **Two passes, like the unit pick** (decision 1071 — GameObjects are the same type-1 candidates
/// as units in the reference's resolve `0x7089c0`, wow-re object-layer mouse-pick): **pass 1** =
/// the exact resident mesh, pure nearest-wins; **pass 2, only when pass 1 hit nothing anywhere**
/// (the mouse pick's generous retry): the same geometry with every vertex displaced +1 model-unit
/// along its authored normal — a ~1-yd halo, which is what makes a wispy herb clickable *around*
/// its leaves instead of only pixel-on-texture. "Nothing anywhere" spans the unit pick too (one
/// resolve in the reference): a frame where [`Hovered`] holds any unit — exact hit or its own
/// halo — never opens the GO halo, because the pass-2 accept ladder ranks every unit (alive 3 /
/// dead 2) above every GameObject (highlightable 1 / else 0). Within the GO halo the same ladder
/// applies: last frame's pick sticks (anti-flicker), else higher priority wins even when farther,
/// ties by distance. Both passes stay world-occlusion-clamped. (Residual, same slice as the
/// eligibility note below: the reference's sticky slot is cross-type, so a stuck GO pick can
/// outrank a unit's halo there; our split picks give the unit that frame instead.)
/// The GO picker's own state, bundled — the pick-set cache with the stream edges that rebuild
/// it, plus the sticky-hover memory — one [`SystemParam`] because [`update_hovered_object`]
/// sits at Bevy's 16-param function-system ceiling. See the maintenance comment at the top of
/// that system for the cache contract.
#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct GoPickSet<'w, 's> {
    added: Query<'w, 's, (), Added<GoPickPart>>,
    removed: RemovedComponents<'w, 's, GoPickPart>,
    cache: Local<'s, bevy::platform::collections::HashSet<Entity>>,
    /// Last frame's GO pick, for pass 2's sticky-hover (the reference's anti-flicker cache, by
    /// net entity — the same rung the unit picker keeps).
    last_pick: Local<'s, Option<Entity>>,
    /// Every pickable billboard card, for [`net_entity_of`]'s first hop. Bundled here rather than
    /// taken as its own `SystemParam` because [`update_hovered_object`] is at Bevy's 16-param
    /// function-system ceiling.
    cards: Query<'w, 's, &'static BillboardCard>,
}

/// The number of `ChildOf` hops [`net_entity_of`] will climb before giving up — a malformed-data
/// guard, not a real depth. The deepest real chain is a card on a nested joint (bone hierarchies
/// in the shipped corpus are single digits deep), so this is orders of margin.
const NET_WALK_HOPS: usize = 64;

/// A picked GameObject part → the **net entity** that carries its `Guid` (the object the mouseover
/// publishes, the cursor classifies and the right-click USEs).
///
/// Two shapes reach here and they attach differently. An ordinary mesh part is a direct child of
/// the net entity — one `ChildOf` hop. A **billboard card is a world ROOT** (decision 0153: a card
/// writes an absolute world transform and lives at the root/identity), so it has no `ChildOf` edge
/// at all: its link to the model is [`BillboardCard::follows`] — the joint it rides on a rigged
/// host, the mirror anchor under the net entity otherwise — and the climb continues from there.
///
/// Before the card hop existed a card hit resolved to the **card**, which carries no `Guid`, so
/// [`update_hovered_object`] bailed and published the **null** mouseover: no cursor, no tooltip and
/// a dead right-click over exactly the screen area the card covers. The Lightwell found it (B169's
/// second half) — its light shaft is a lock-Z card standing 4.5 yd out of a 1.09 yd bowl, so the
/// card swallowed nearly every ray and only the sliver of bowl around it ever answered.
///
/// The reference has no such split to reconcile: the resolve `0x7089c0`'s narrow phase walks the
/// ONE model's render batches — *"the actual visible RENDER MESH, posed to the current animation"*,
/// billboard batch included, skinned through the live bone matrices (wow-re `object-layer.md`) — so
/// a hit on the shaft **is** a hit on the GameObject, which is what this restores.
fn net_entity_of(
    e: Entity,
    cards: &Query<&BillboardCard>,
    guids: &Query<&Guid>,
    child_of: &Query<&ChildOf>,
) -> Entity {
    let mut cur = cards
        .get(e)
        .ok()
        .and_then(BillboardCard::follows)
        .unwrap_or(e);
    for _ in 0..NET_WALK_HOPS {
        if guids.contains(cur) {
            return cur;
        }
        let Ok(parent) = child_of.get(cur) else { break };
        cur = parent.parent();
    }
    cur
}

#[allow(clippy::too_many_arguments)]
pub(super) fn update_hovered_object(
    camera: Query<(&Camera, &GlobalTransform), With<WorldCamera>>,
    window: Query<&Window, With<PrimaryWindow>>,
    rig: Res<CameraControl>,
    pointer_over_ui: Res<PointerOverUi>,
    occlusion: Res<PickOcclusion>,
    // The unit pick's verdict — [`update_hover`] runs earlier in the target chain. Any unit hover
    // means pass 1 + the unit halo already own the mouseover, so the GO pass 2 stays shut.
    unit_hovered: Res<Hovered>,
    mut hovered: ResMut<HoveredObject>,
    // Every pickable GameObject part carries [`GoPickPart`] (stamped beside its
    // `WorldObject { kind: GameObject }` at the attach sites; units/doodads/WMOs never enter this
    // query — units have their own posed-mesh pick).
    go_parts: Query<Entity, With<GoPickPart>>,
    child_of: Query<&ChildOf>,
    guids: Query<&Guid>,
    stores: Query<&ObjectStore>,
    // GENERIC's eligibility is its template's `data[1]` (decision 0762) — the ask-once cache.
    go_templates: Res<crate::go_templates::GameObjectTemplates>,
    // The faction term of eligibility (decision 0764): the GO's own template reaction toward us.
    factions: Option<Res<super::ring::Factions>>,
    self_q: Query<&ObjectStore, With<crate::net::SelfPlayer>>,
    parts: PickParts,
    // The picker's own state, one bundled param ([`GoPickSet`] — the fn sits at Bevy's
    // 16-param ceiling): the pick-set cache, its stream edges, and the sticky-hover memory.
    mut cache: GoPickSet,
) {
    let added_parts = &cache.added;
    let removed_parts = &mut cache.removed;
    let pickable = &mut *cache.cache;
    let last_pick = &mut *cache.last_pick;
    let cards = &cache.cards;
    // The hit part → its guid-bearing net entity ([`net_entity_of`]: the card hop, then the climb).
    let resolve_net = |e: Entity| net_entity_of(e, cards, &guids, &child_of);
    // The pick set (bevy's `HashSet` — the caster's type, not the `std` one this module uses
    // elsewhere), rebuilt on GO-part stream edges ONLY — the `WaterIndex` shape. It used to be
    // collected from every streamed GO part on every cursor frame (ChildOf hop + store probe +
    // hash insert each); membership only actually changes when GO models spawn or despawn, and
    // a GO's type id never changes after create. Maintained FIRST, ahead of every early return:
    // an `Added` edge is only visible on the frame it happens (the run advances this system's
    // change tick even through a `return`), so a rebuild parked behind the cursor gates would
    // silently drop parts streamed in while the pointer sat on UI. A despawned entry between
    // edges self-filters at the caster (`parts.get` on a dead entity misses).
    //
    // A transport-family GO (TRANSPORT 11 / MAP_OBJECT 14 / MO_TRANSPORT 15) never joins the
    // set: in the reference it has no pick geometry at all — the GO model resolver `0x5f80e0`
    // returns 0 for these three types (they render via the WMO/spline path, not an M2) and their
    // strategy's interaction predicates are constant-false (w2c1 + go-render-gate + the +0x14
    // vtable dump). Concretely: a ship's hull must not swallow the pick — an NPC on deck stays
    // hoverable through the railing, and the hull shows no gear or tooltip.
    if removed_parts.read().next().is_some() || !added_parts.is_empty() {
        pickable.clear();
        pickable.extend(go_parts.iter().filter(|&e| {
            // Through the same resolver the hit uses, so a card and its mesh siblings can never
            // disagree about which GameObject they belong to (a transport's card would otherwise
            // stay in the set while its meshes were filtered out).
            !stores
                .get(resolve_net(e))
                .is_ok_and(|s| matches!(s.0.gameobject_type_id(), 11 | 14 | 15))
        }));
    }
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
    if pickable.is_empty() {
        *last_pick = None;
        return;
    }
    let Ok(ray) = camera.viewport_to_world(cam_tf, cursor) else {
        return;
    };
    let self_store = self_q.single().ok();

    // Pass 1 — the exact resident geometry, pure nearest-wins (priority-independent).
    let mut best: Option<(f32, Entity)> =
        benilla_world::interact::cast_pick_ray(ray, pickable, &parts, false)
            .into_iter()
            .next()
            .map(|(e, h)| (h.distance, e));

    // Pass 2 — nothing exactly hit anywhere (no GO, and no unit either — the doc above): the
    // generous retry over the normal-inflated geometry, accepted by the priority ladder.
    if best.is_none() {
        if unit_hovered.target.is_some() {
            *last_pick = None;
            return;
        }
        let mut best2: Option<(f32, u32, Entity)> = None;
        for (part, hit) in benilla_world::interact::cast_pick_ray_inflated(ray, pickable, &parts) {
            let net = resolve_net(part);
            let prio = if *last_pick == Some(net) {
                u32::MAX // the sticky-hover cache outranks everything
            } else {
                // The classify priority (`0x480c90`): highlightable GameObject 1, else 0. A store
                // that hasn't streamed reads highlightable — the eligibility gate's own
                // permissive default, so a fresh spawn isn't a dead zone for its first frames.
                stores.get(net).map_or(1, |s| {
                    let reaction = crate::target::cursor_mode::go_reaction(
                        factions.as_deref(),
                        s.0.gameobject_faction(),
                        self_store,
                    );
                    let go_guid = guids.get(net).ok().map(|g| g.0);
                    let overrides = crate::target::cursor_mode::GoOverrides {
                        channel_owned: crate::target::cursor_mode::fishing_channel_owned(
                            self_store, go_guid,
                        ),
                        meeting_stone_queued: crate::target::cursor_mode::meeting_stone_queued(
                            go_guid.and_then(|g| go_templates.get(g)?.meeting_stone_area),
                        ),
                    };
                    u32::from(crate::target::cursor_mode::go_highlightable(
                        s, reaction, overrides,
                    ))
                })
            };
            let wins =
                best2.is_none_or(|(bt, bp, _)| prio > bp || (prio == bp && hit.distance < bt));
            if wins {
                best2 = Some((hit.distance, prio, part));
            }
        }
        best = best2.map(|(t, _, e)| (t, e));
    }

    // The occlusion verdict (`0x480df0` @ `0x480eb4`): discard the GO hit iff the world hit is
    // STRICTLY nearer — a tie keeps the object.
    if best.is_some_and(|(t, _)| occlusion.distance < t) {
        best = None;
    }
    let picked = best.map(|(t, e)| (t, resolve_net(e)));
    *last_pick = picked.map(|(_, net)| net);
    let Some((distance, net_entity)) = picked else {
        return;
    };
    let Ok(guid) = guids.get(net_entity) else {
        return;
    };
    // **The mouseover-eligibility gate** (`[obj->vtbl+0x54]` at `0x482982`, decision 0762): an
    // ineligible object publishes the NULL mouseover, so it gets no tooltip, no +64 brighten and no
    // cursor — nothing. Applied here, at the one place the GO mouseover is published, so all three
    // consumers fall out together exactly as they do in the reference (which reaches `0x492890`,
    // `0x52aa20` and `0x4945e0` only past this gate).
    //
    // KNOWN RESIDUAL: the reference nulls the **whole** mouseover, so a unit standing behind an
    // ineligible portcullis is not hoverable through it either. benilla picks unit and GameObject
    // separately and arbitrates by distance, so here the unit behind it stays hoverable. Narrower
    // than the old behaviour (which tooltipped the portcullis itself) and documented rather than
    // silently accepted; closing it wants the single-pick arbitration, which is its own slice.
    if let Ok(store) = stores.get(net_entity) {
        let tmpl = go_templates.get(guid.0);
        let reaction = crate::target::cursor_mode::go_reaction(
            factions.as_deref(),
            store.0.gameobject_faction(),
            self_store,
        );
        if !crate::target::cursor_mode::mouseover_eligible(
            store.0.gameobject_type_id(),
            store.0.gameobject_flags(),
            store.0.gameobject_dynamic_flags(),
            tmpl.map(|t| t.highlight_column),
            reaction,
            crate::target::cursor_mode::GoOverrides {
                channel_owned: crate::target::cursor_mode::fishing_channel_owned(
                    self_store,
                    Some(guid.0),
                ),
                meeting_stone_queued: crate::target::cursor_mode::meeting_stone_queued(
                    tmpl.and_then(|t| t.meeting_stone_area),
                ),
            },
        ) {
            return;
        }
    }
    hovered.target = Some(net_entity);
    hovered.guid = Some(guid.0);
    hovered.distance = distance;
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    fn card(bone: u16, owner: Entity) -> BillboardCard {
        let info = benilla_assets::BillboardInfo {
            pivot: Vec3::ZERO,
            bone,
            kind: benilla_formats::BillboardKind::LockZ,
            scale_anim: None,
            seq_translations: vec![],
        };
        BillboardCard::following(&info, owner)
    }

    /// **A billboard card's hit belongs to its GameObject** — bug B169's second half, the Lightwell
    /// you could see but not click (2026-08-25).
    ///
    /// A card is a world ROOT, so the old one-`ChildOf`-hop resolve answered with the card itself;
    /// the card carries no `Guid`, so `update_hovered_object` bailed and published the null
    /// mouseover — no cursor, no tooltip, a dead right-click — over the whole screen area the card
    /// covers. On `G_HolyLightWell.m2` that is a 4.5 yd shaft standing out of a 1.09 yd bowl, so
    /// nearly every ray landed on the dead card and only the ring of bowl around it answered:
    /// *"very hard to find the right position, and even when I see the cog nothing happens"*.
    ///
    /// Both card shapes are covered, because `spawn_billboard_part` picks between them on whether
    /// the host is rigged: following the **mirror anchor** (a direct child of the net entity) and
    /// following a **joint** (nested under the model root, so the climb is more than one hop).
    #[test]
    fn a_billboard_cards_hit_resolves_to_its_gameobject() {
        let mut world = World::new();
        let net = world.spawn(Guid(0xdead_beef)).id();
        // An ordinary mesh part: the direct child the old one-hop resolve was written for.
        let mesh_part = world.spawn(ChildOf(net)).id();
        // The rigless shape: the card follows the mirror anchor under the net entity.
        let anchor = world.spawn(ChildOf(net)).id();
        let anchor_card = world.spawn(card(0, anchor)).id();
        // The rigged shape: net → model root → root-bone joint → child joint, card on the deepest.
        let root = world.spawn(ChildOf(net)).id();
        let joint0 = world.spawn(ChildOf(root)).id();
        let joint1 = world.spawn(ChildOf(joint0)).id();
        let joint_card = world.spawn(card(1, joint1)).id();
        // A card whose owner is gone (despawned mid-frame) must not climb into someone else's tree.
        let orphan_card = world
            .spawn(card(2, Entity::from_raw_u32(9999).unwrap()))
            .id();

        let resolved = world
            .run_system_once(
                move |cards: Query<&BillboardCard>,
                      guids: Query<&Guid>,
                      child_of: Query<&ChildOf>|
                      -> Vec<Entity> {
                    [mesh_part, anchor_card, joint_card, orphan_card, net]
                        .into_iter()
                        .map(|e| net_entity_of(e, &cards, &guids, &child_of))
                        .collect()
                },
            )
            .unwrap();

        assert_eq!(
            resolved[0], net,
            "mesh part: the one-hop case still resolves"
        );
        assert_eq!(
            resolved[1], net,
            "anchor-following card: the world root hops through `follows()`, not `ChildOf`"
        );
        assert_eq!(
            resolved[2], net,
            "joint-following card: the climb continues up the joint hierarchy, not one hop"
        );
        assert_ne!(
            resolved[3], net,
            "a card whose owner is gone resolves to nothing that carries a guid"
        );
        assert_eq!(resolved[4], net, "the net entity resolves to itself");
    }
}
