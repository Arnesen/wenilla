//! Spawning streamed placements once their model assets land: the per-frame budgeted
//! [`spawn_loaded_placements`] driver + WMO doodad-prop resolution and the small per-placement
//! helpers (inspector tags, fade spheres). The submesh/material/fade component assembly lives in
//! [`assemble`] ([`spawn_model_entities`]), the per-model point lights/emitters/ribbons in [`fx`].

mod assemble;
mod fx;
pub(crate) mod prop_light;

pub(crate) use assemble::spawn_model_entities;
pub(crate) use fx::point_light;
use fx::{spawn_emitters_for, spawn_lights_for, spawn_ribbons_for, spawn_wmo_lights_for};

use std::sync::Arc;
use std::time::Instant;

use benilla_assets::coords::{wmo_doodad_local, wow_to_bevy};
use benilla_assets::{AdtTile, DoodadBase, M2Model, WmoModel};
use benilla_formats::{world_to_tile, M2Bounds};
use bevy::prelude::*;

use crate::collision::{camera_layers, walk_layers, GroundDecalSurface, PickOccluder};
use crate::debug_panel::ModelKind;
use crate::doodad_anim::wants_rig;
use crate::interact::WorldObject;
use crate::lighting::SharedLightBuffer;
use crate::lighting::{PropProbeSlot, PropProbes};
use crate::liquid::{spawn_wmo_liquids, LiquidAssets};
use crate::model_forms::{FormSlices, ModelForms, ModelKey, WANT_SKINNED, WANT_STATIC};
use crate::model_render::{m2_url, ShadeSel};
use crate::terrain::WowModelMaterial;
use crate::wmo_portal::{WmoGroupVis, WmoPortalInstance, WmoRoom};

use super::collider::{build_collider_task, placement_collider_data, PendingCollider};
use super::{
    doodad_ground_shade, ModelHandle, Placements, ShadeResolve, TerrainStreamer, SPAWN_BUDGET,
};
use prop_light::{fold_interior_probe, PropLight, PropLobeLight, WmoDoodadInst};

/// Placements (models or WMO props) spawned per frame while live — the count half of the landing
/// budget; see the cap's comment in [`spawn_loaded_placements`]. ~150 × the measured ~50 µs
/// downstream cost per landed placement ≈ half a 60 Hz frame of render-side work, and a fresh
/// Undercity row (~3900 placements) fully furnishes in under half a second, two tiles out.
const SPAWN_COUNT_CAP: usize = 150;

/// Spawn the submesh entities for any placement whose model asset has finished loading. Each submesh
/// gets the production [`WowModelMaterial`] (cutout + blend twin) and the same `ModelPart`/`DoodadFade`/
/// `MeshTag` components the old spawn used — so the existing visibility/fade/lighting systems take over.
/// The nested resource tuple of [`spawn_loaded_placements`] (the 16-SystemParam ceiling): the
/// prop-probe table, the stream-trace counters, the live/settling state the landing cap reads,
/// and the model-forms cache (0834). The skin-palette table left with decision 0863 — a
/// placement's rig slot is the draw gate's to claim at first wake now, not the spawner's.
type SpawnTables<'w> = (
    ResMut<'w, PropProbes>,
    ResMut<'w, crate::perf::StreamActivity>,
    Option<Res<'w, crate::player::Player>>,
    ResMut<'w, ModelForms>,
);

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_loaded_placements(
    mut commands: Commands,
    placements: ResMut<Placements>,
    m2s: Res<Assets<M2Model>>,
    wmos: Res<Assets<WmoModel>>,
    materials: ResMut<Assets<WowModelMaterial>>,
    asset_server: Res<AssetServer>,
    shared_light: Option<Res<SharedLightBuffer>>,
    mut meshes: ResMut<Assets<Mesh>>,
    // The shared per-kind animated liquid materials — WMO groups spawn their embedded water/lava/slime
    // on these, same as MCLQ terrain water. Absent when the client has no data.
    liquid_assets: Option<Res<LiquidAssets>>,
    // Read-only: the loaded-tile map + decoded tiles, for the global MCSH ground-shade lookup at spawn.
    // `stream_terrain` (chained before this) owns them mutably; here we only read.
    streamer: Res<TerrainStreamer>,
    adt_tiles: Res<Assets<AdtTile>>,
    time: Res<Time>,
    mut uv_reg: ResMut<crate::doodad_anim::UvAnimMaterials>,
    mut tint_reg: ResMut<crate::doodad_anim::TintAnimMaterials>,
    // Nested to stay inside Bevy's 16-element system-param tuple limit: the prop-probe table +
    // the stream-trace counters + the live/settling state the landing cap reads + the
    // model-forms cache (decision 0834).
    tables: SpawnTables,
) {
    let (mut probes, mut activity, player, mut forms) = tables;
    let Some(shared_light) = shared_light else {
        return;
    };
    let t0 = Instant::now();
    // The landing COUNT cap (B181), on top of the time budget below. The clock gates main-thread
    // cost — but ~1000 one-submesh doodads pass a 4 ms budget in 2.5 ms and hand the render world
    // their whole extract/specialize/upload wave in one frame: the measured 60–80 ms landing
    // frame when a fresh Undercity row streams in. Counting bounds the downstream wave the clock
    // cannot see. Idle until the body is live-and-settled — the entry/teleport cover exists to
    // absorb exactly this burst, and capping under it would only lengthen the reveal.
    let count_cap = match player.as_deref() {
        Some(p) if p.active && !p.settling && !p.world_stale => SPAWN_COUNT_CAP,
        _ => usize::MAX,
    };
    let mut spawned_n = 0usize;
    // The animated-doodad clock origin (decision 0130): per-instance phase = spawn time, as in the
    // reference (the arm-time cursor offset), and the draw gate seeks against it on resume.
    let now = time.elapsed_secs();
    let light = &shared_light.0;
    let materials = materials.into_inner();
    let Placements {
        by_id,
        materials: mat_cache,
    } = placements.into_inner();

    // Per-frame spawn budget. On cold start every tile's doodads/WMOs finish decoding near-together;
    // spawning them all in one frame builds thousands of parry trimesh colliders synchronously and
    // freezes the window for seconds. Spend at most `SPAWN_BUDGET` per frame and resume next frame
    // (the `spawned` flags make this re-entrant) — a self-tuning cap: cheap doodads spawn many per
    // frame, an expensive WMO collider one. At least one spawn always happens (the check is after the
    // work), so progress is guaranteed.
    let deadline = Instant::now() + SPAWN_BUDGET;

    'placements: for (&unique_id, p) in by_id.iter_mut() {
        // 1. Spawn the model's own geometry once, on first load. For a WMO, also resolve its doodad
        //    props (`p.doodads`) — spawned individually in step 2 as each prop's M2 asset arrives.
        if !p.spawned {
            let entities = match &p.model {
                ModelHandle::M2(h) => {
                    let Some(m) = m2s.get(h) else {
                        continue; // model still loading (or missing) — try next frame
                    };
                    // The model's app-built render forms (decision 0834): request static — plus
                    // the skinned twins iff the anim host will rig this model — and wait for the
                    // paced furnisher, exactly as the placement waits for the asset itself.
                    let key = ModelKey::from(h);
                    let kinds = WANT_STATIC | if wants_rig(m) { WANT_SKINNED } else { 0 };
                    if !forms.require(
                        key,
                        kinds,
                        placement_priority(&streamer, p.transform.translation),
                    ) {
                        continue;
                    }
                    // Resolve the MCSH ground-shade the reference way: a GLOBAL world→tile→chunk lookup at
                    // the doodad's origin, independent of which tile registered it or in what order. An ADT
                    // map doodad on lit ground takes the boosted ADT sun level (`ShadeSel::Lit` — the
                    // binary's 2.5, wow-re m2-interior-doodad-base-light §6).
                    let shade =
                        match doodad_ground_shade(&streamer, &adt_tiles, p.transform.translation) {
                            ShadeResolve::Ready(true) => ShadeSel::Shaded,
                            ShadeResolve::Ready(false) => ShadeSel::Lit,
                            // The doodad's own ground tile is requested but still decoding — wait, so we don't
                            // bake the lit fallback into a straddling tree whose true tile lands a frame later.
                            ShadeResolve::Pending => continue,
                        };
                    let (radius, center) = m2_fade(&m.bounds, p.transform.scale.x);
                    let (mut ents, host) = spawn_model_entities(
                        &mut commands,
                        mat_cache,
                        materials,
                        light,
                        &m.submeshes,
                        FormSlices {
                            stat: forms.static_meshes(key).unwrap_or(&[]),
                            skin: forms.skinned_meshes(key),
                        },
                        p.transform,
                        false,
                        shade,
                        None, // map doodad: exterior sky lighting (no interior probe)
                        radius,
                        center,
                        Some((m, now)),
                        &mut uv_reg,
                        &mut tint_reg,
                        None, // world-static placement: cards bake their world pivot
                    );
                    // ADT doodads are exterior scene: from inside a WMO they draw only through a
                    // portal window (`0x683700`, fed solely by the per-window walk `0x682fa0` — see
                    // `crate::exterior_cull`). Tagged per submesh because a placement has no root
                    // entity to carry the bound; the deviation from the reference's whole-object test
                    // is recorded in `exterior_cull`'s module doc.
                    //
                    // The anim-host ROOT is skipped: it is a joint hierarchy, not geometry, so it has
                    // no `Aabb` and lands in the cull's fail-open arm — harmless to draw (it draws
                    // nothing) but it was 484 of the 6917 "tested" objects at one Stratholme pin,
                    // which is exactly the kind of noise that makes the instrument stop meaning
                    // anything. Tag what is drawn (decision 0784).
                    let anim_root = host.as_ref().map(|h| h.root);
                    for e in ents.iter().filter(|e| Some(**e) != anim_root) {
                        commands
                            .entity(*e)
                            .insert(crate::exterior_cull::ExteriorScene);
                    }
                    // Doodad collider (avian): a static trimesh hull baked at this placement's transform,
                    // built off-thread, as its own entity so it despawns with the placement. `None` ⇒
                    // the model has no collision hull. A world-static doodad hull also clamps the
                    // mouse pick (`PickOccluder` — a hull-less tree canopy stays pick-through,
                    // matching the reference's collision-flagged-doodads-only world trace).
                    if let Some((verts, tris)) =
                        placement_collider_data(m.collision.as_ref(), &p.transform)
                    {
                        ents.push(
                            commands
                                .spawn((
                                    PendingCollider::new(
                                        build_collider_task(verts, tris),
                                        None,
                                        true,
                                    ),
                                    PickOccluder,
                                ))
                                .id(),
                        );
                    }
                    spawn_emitters_for(
                        &mut commands,
                        &m.emitters,
                        p.transform,
                        host.as_ref().map(|h| h.joints.as_slice()),
                        host.as_ref().and_then(|h| h.arm),
                        (radius, center),
                        None, // an ADT map doodad belongs to no building
                        &mut ents,
                    );
                    spawn_ribbons_for(
                        &mut commands,
                        &m.ribbons,
                        p.transform,
                        host.as_ref().map(|h| h.joints.as_slice()),
                        ents.first().copied(),
                    );
                    spawn_lights_for(&mut commands, &m.lights, p.transform, &mut ents);
                    tag_world_object(
                        &mut commands,
                        &ents,
                        ModelKind::Doodad,
                        handle_label(h),
                        unique_id,
                        format!("emitters: {}", m.emitters.len()),
                    );
                    ents
                }
                ModelHandle::Wmo(h) => {
                    let Some(m) = wmos.get(h) else {
                        continue;
                    };
                    // The building's app-built render forms (0834): static only — WMO group
                    // geometry never skins. A city root's thousands of batches are exactly the
                    // burst the paced furnisher exists to spread.
                    let key = ModelKey::from(h);
                    let prio = placement_priority(&streamer, p.transform.translation);
                    if !forms.require(key, WANT_STATIC, prio) {
                        continue;
                    }
                    // WMOs carry no authored bounds → never size-fade (∞ radius), rely on the far-clip.
                    // (`m2: None` ⇒ no anim host, so the joint half of the return is always empty.)
                    let (mut ents, _) = spawn_model_entities(
                        &mut commands,
                        mat_cache,
                        materials,
                        light,
                        &m.submeshes,
                        FormSlices {
                            stat: forms.static_meshes(key).unwrap_or(&[]),
                            skin: None,
                        },
                        p.transform,
                        true,
                        ShadeSel::Matte, // WMO lights on the FFP N·L path — the selector is unread
                        None, // WMO groups carry their own per-submesh interior flag + batch class
                        f32::INFINITY,
                        Vec3::ZERO,
                        None, // WMO group geometry is not an M2 — its doodad props animate below
                        &mut uv_reg,
                        &mut tint_reg,
                        None, // world-static placement: cards bake their world pivot
                    );
                    // A world WMO placement is exterior scene too (`0x6856c0`, fed by the same
                    // per-window populate `0x682fa0`): from inside one building, another building
                    // draws only through a portal window. 0774 left this ungated because these
                    // entities already had a `Visibility` authority and a second writer would have
                    // fought it — that is fixed at the root now, with the window term folded INTO
                    // that authority (decision 0784), so the tag is safe to add.
                    //
                    // Tagging is unconditional and the **exemption is dynamic**: the camera's own
                    // containing placement is not exterior to itself, which the authority decides
                    // per frame from `CameraInteriorClaim`. A static "is this the player's building"
                    // could not be right — the player walks in and out of it.
                    for e in &ents {
                        commands
                            .entity(*e)
                            .insert(crate::exterior_cull::ExteriorScene);
                    }
                    // Portal visibility + interior tracking: tie every group submesh entity (the `ents`
                    // here are exactly the submeshes, in order, before colliders/lights are appended
                    // below) back to one per-placement instance that holds the placement transform + the
                    // per-frame visible set. The cull is per-group, so a building with a portal graph
                    // gets tagged; a portal-less prop keeps all groups visible but still spawns the
                    // instance when it carries a WMOAreaTable identity — the interior down-ray
                    // (`wmo_portal::track_current_interior`) needs it. See `crate::wmo_portal`.
                    let has_portals = !m.portal_refs.is_empty() && !m.portal_infos.is_empty();
                    if has_portals || m.wmo_id != 0 {
                        let instance = commands
                            .spawn(WmoPortalInstance {
                                handle: h.clone(),
                                world_from_local: p.transform.compute_affine(),
                                name_set: p.name_set,
                                visible: vec![true; m.group_nav.len()],
                            })
                            .id();
                        if has_portals {
                            // One shared single-group key per group — geometry belongs to exactly
                            // one room, and sharing keeps the tag one allocation per group rather
                            // than per submesh (a city is ~100k submeshes).
                            let group_key: Vec<Arc<[u16]>> = (0..m.group_nav.len() as u16)
                                .map(|g| Arc::from([g].as_slice()))
                                .collect();
                            for (&entity, &group) in ents.iter().zip(&m.submesh_group) {
                                let Some(groups) = group_key.get(group as usize).cloned() else {
                                    continue;
                                };
                                commands
                                    .entity(entity)
                                    .insert(WmoGroupVis { instance, groups });
                            }
                        }
                        // The props spawn later — each waits on its own M2 — so they can't be
                        // tagged here; hold the instance for them (decision 0689). Held for EVERY
                        // instance, not just a portal-bearing one: since 0696 it is also the
                        // placement identity each embedded pool is scoped by, and a portal-less
                        // building's pool needs an owner exactly as much as a canal does.
                        p.portal_instance = Some(instance);
                        ents.push(instance); // despawns with the placement
                    }
                    // The building's embedded MLIQ liquid (Stormwind's canals + fountains, the
                    // Ironforge lava, dungeon pools): one flat animated surface per group with water,
                    // spawned at the placement transform on the shared per-kind material. Appended
                    // AFTER the portal-instance zip above, which requires `ents` to still be exactly the
                    // submeshes in order.
                    // Spawned a group at a time so each surface can take that group's cull key: a
                    // pool belongs to the room it sits in, and a culled room's lava must go with it
                    // (decision 0689 — the same defect as the props, on the same building).
                    for (gi, lq) in m.group_liquids.iter().enumerate() {
                        let Some(lq) = lq else { continue };
                        let first = ents.len();
                        spawn_wmo_liquids(
                            &mut commands,
                            std::iter::once(lq),
                            liquid_assets.as_deref(),
                            &mut meshes,
                            p.transform,
                            // The group's own interior class (`MOGI & 0x48 == 0`) — which FOG BLOCK
                            // its pool draws under, so an indoor pool hazes with the room the way
                            // the walls beside it do (decision 0691's open lane).
                            m.group_bounds.get(gi).is_some_and(|g| g.interior),
                            // …and the pool's SCOPE: the room it belongs to (0696 — only a subject
                            // standing in this placement can be in it) plus that room's own floor
                            // in world Z (0701 — and only one at or above it).
                            crate::liquid::WmoPool::new(
                                p.portal_instance.map(|instance| WmoRoom {
                                    instance,
                                    group: gi as u16,
                                }),
                                &p.transform,
                                m.group_bounds.get(gi),
                            ),
                            &mut ents,
                        );
                        if let Some(instance) = p.portal_instance {
                            let groups: Arc<[u16]> = Arc::from([gi as u16].as_slice());
                            for &e in &ents[first..] {
                                commands.entity(e).insert((
                                    WmoGroupVis {
                                        instance,
                                        groups: groups.clone(),
                                    },
                                    // Another building's canal is exterior scene like its walls; the
                                    // one you are standing in is exempted by instance (0784). Only
                                    // tagged alongside `WmoGroupVis`, because that component is what
                                    // carries the instance the exemption is keyed on.
                                    crate::exterior_cull::ExteriorScene,
                                ));
                            }
                        }
                    }
                    // Building colliders (avian): the flattened group tris baked at this placement's
                    // transform — *two* meshes, because the client gathers different WMO faces for the
                    // player body (walking: drops DETAIL) and the camera/LOS (drops NOCAMCOLLIDE, keeps
                    // DETAIL). Each goes on its own collision layer so the player and camera queries pick
                    // their audience (see `crate::collision`); a single trimesh can't be filtered per-face.
                    if let Some((verts, tris)) =
                        placement_collider_data(m.collision.as_ref(), &p.transform)
                    {
                        ents.push(
                            commands
                                .spawn((
                                    PendingCollider::new(
                                        build_collider_task(verts, tris),
                                        Some(walk_layers()),
                                        true,
                                    ),
                                    // WMO walkable faces receive the selection ring (floors, steps).
                                    GroundDecalSurface,
                                    // …and clamp the mouse pick: the byte-verified occluder face
                                    // set is MOPY reject-mask 0x84, and the walk bake (reject 0x04)
                                    // is its nearest existing bake — see `crate::collision`.
                                    PickOccluder,
                                ))
                                .id(),
                        );
                    }
                    if let Some((verts, tris)) =
                        placement_collider_data(m.collision_camera.as_ref(), &p.transform)
                    {
                        ents.push(
                            commands
                                .spawn(PendingCollider::new(
                                    build_collider_task(verts, tris),
                                    Some(camera_layers()),
                                    true,
                                ))
                                .id(),
                        );
                    }
                    // Interior MOLT lights (forge fire, inn fireplaces, chapel candles) — the radiating
                    // sources that light nearby NPCs/doodads AND the building's own walls/floor over
                    // their baked MOCV (decision 0273).
                    spawn_wmo_lights_for(&mut commands, &m.lights, p.transform, &mut ents);
                    tag_world_object(
                        &mut commands,
                        &ents,
                        ModelKind::Wmo,
                        handle_label(h),
                        unique_id,
                        String::new(),
                    );
                    // Resolve the interior props for this instance's doodad set (set 0 + the selected
                    // set), each composed onto the WMO's world transform; their M2s load async.
                    p.doodads = resolve_wmo_doodads(m, p.doodad_set, p.transform, &asset_server);
                    ents
                }
            };
            p.spawned = true;
            p.entities = entities;
            activity.placements_spawned += 1;
            spawned_n += 1;
            // Spent the frame's spawn budget on this model (a WMO with two collider meshes is the
            // costly case) — defer the rest, including this placement's WMO props, to next frame.
            if Instant::now() >= deadline || spawned_n >= count_cap {
                break 'placements;
            }
        }

        // 2. Spawn each WMO doodad prop as its M2 asset finishes loading (across frames). Their
        //    entities join `p.entities`, so they despawn with the placement. Empty for M2 doodads.
        // Copied out before the loop borrows `p.doodads`: the props tag onto the same portal
        // instance their building's groups did (decision 0689).
        let portal_instance = p.portal_instance;
        for d in &mut p.doodads {
            if d.spawned {
                continue;
            }
            let Some(m) = m2s.get(&d.handle) else {
                continue; // this prop's M2 still loading
            };
            // The prop's app-built render forms (0834) — same gate as its owning placement's.
            let key = ModelKey::from(&d.handle);
            let kinds = WANT_STATIC | if wants_rig(m) { WANT_SKINNED } else { 0 };
            if !forms.require(
                key,
                kinds,
                placement_priority(&streamer, d.transform.translation),
            ) {
                continue;
            }
            // The verified MODD lighting: an EXTERIOR-group WMO prop samples the terrain MCSH at
            // its footprint like the reference's per-frame refresh `0x698c50` — plain matte (sun
            // ×1.0) on lit ground, the shaded level on MCSH-shadowed ground, and NEVER the ADT 2.5
            // boost (§8b: its one read site is unreachable from the WMO render band). An INTERIOR
            // prop takes its per-instance SH probe instead — the selector is unread there.
            let shade = if matches!(d.light, PropLight::Interior { .. }) {
                ShadeSel::Matte // interior lane; the selector is unread
            } else {
                match doodad_ground_shade(&streamer, &adt_tiles, d.transform.translation) {
                    ShadeResolve::Ready(true) => ShadeSel::Shaded,
                    ShadeResolve::Ready(false) => ShadeSel::Matte,
                    // The prop's ground tile is still decoding — defer this prop a frame (same rule
                    // as an ADT doodad; the per-prop `spawned` flag makes it re-entrant).
                    ShadeResolve::Pending => continue,
                }
            };
            let (radius, center) = m2_fade(&m.bounds, d.transform.scale.x);
            // An interior prop's committed light, folded ONCE into its SH probe (the reference folds
            // at doodad create + per-frame light commit; everything here is static, so once): the
            // MODD-colour ambient + the diffuse lobe on the fixed interior axis + the owning group's
            // MOLR lights windowed by their disk attenStart/attenEnd from the prop's REFERENCE POINT
            // — the placement-transformed M2 vertex-box centre (the byte-cited `[def+0x5c]` anchor
            // family `0x6952a0`/`0x713680`; the trace fit put it ~1.1 yd up on the abbey stand,
            // between origin and centre — the centre satisfies every observed gain, residual OPEN).
            let interior_slot = match &d.light {
                PropLight::Exterior => None,
                PropLight::Interior {
                    ambient,
                    diffuse,
                    lights,
                } => {
                    let ref_point = d.transform.transform_point(center);
                    let coeffs = fold_interior_probe(*ambient, *diffuse, ref_point, lights);
                    let slot = probes.alloc(coeffs);
                    if slot.is_none() {
                        // Once per burst, not per prop — a city WMO can flood thousands in a frame.
                        let (live, peak) = probes.occupancy();
                        warn_once!(
                            "interior-prop probe table full (live {live}, peak {peak});                              overflowing props fall back to exterior light"
                        );
                    }
                    slot
                }
            };
            let (mut ents, host) = spawn_model_entities(
                &mut commands,
                mat_cache,
                materials,
                light,
                &m.submeshes,
                FormSlices {
                    stat: forms.static_meshes(key).unwrap_or(&[]),
                    skin: forms.skinned_meshes(key),
                },
                d.transform,
                false,
                shade,
                interior_slot, // interior props light off their folded probe, not the sky
                radius,
                center,
                Some((m, now)),
                &mut uv_reg,
                &mut tint_reg,
                None, // world-static placement: cards bake their world pivot
            );
            // The slot frees itself when the prop despawns (streaming out) — the component hook
            // returns it to the table whoever does the despawn.
            if let (Some(slot), Some(&first)) = (interior_slot, ents.first()) {
                commands.entity(first).insert(PropProbeSlot(slot));
            }
            // Portal-cull the prop with the rooms that name it — the reference commits a WMO's
            // doodads per VISIBLE group (`0x695aa0` over the group's MODR refs, from the
            // visible-group walk `0x698720`), so furniture never outlives its room (decision 0689)
            // and a prop several rooms name is drawn from ANY of them. Every submesh spawned above
            // is this one prop, so they all share the one key.
            if let (Some(instance), false) = (portal_instance, d.groups.is_empty()) {
                for &entity in &ents {
                    commands.entity(entity).insert((
                        WmoGroupVis {
                            instance,
                            groups: d.groups.clone(),
                        },
                        // Furniture is exterior scene when its building is (0784) — and exempt when
                        // that building is the one the camera stands in. Keyed on the instance
                        // `WmoGroupVis` carries, so a prop no group names (no key, no exemption
                        // possible) is deliberately left untagged rather than gated blind.
                        crate::exterior_cull::ExteriorScene,
                    ));
                }
            }
            // Prop collider (avian): a static trimesh from the prop's collision hull at its world
            // transform — collide-iff-hull, exactly like a map doodad, so a weapon rack / crate / cargo
            // net is solid to both the player and the camera while a hull-less prop (banner, small
            // candle) isn't. Default collision layer ⇒ both audiences. Joins `ents`, so it despawns with
            // the placement. (WMO doodad props were previously render-only — no collision at all.)
            if let Some((verts, tris)) = placement_collider_data(m.collision.as_ref(), &d.transform)
            {
                ents.push(
                    commands
                        .spawn((
                            PendingCollider::new(build_collider_task(verts, tris), None, true),
                            // A world-static WMO prop's hull clamps the mouse pick like any doodad.
                            PickOccluder,
                        ))
                        .id(),
                );
            }
            spawn_emitters_for(
                &mut commands,
                &m.emitters,
                d.transform,
                host.as_ref().map(|h| h.joints.as_slice()),
                host.as_ref().and_then(|h| h.arm),
                (radius, center),
                portal_instance, // a WMO prop rides its building's exemption
                &mut ents,
            );
            spawn_ribbons_for(
                &mut commands,
                &m.ribbons,
                d.transform,
                host.as_ref().map(|h| h.joints.as_slice()),
                ents.first().copied(),
            );
            spawn_lights_for(&mut commands, &m.lights, d.transform, &mut ents);
            tag_world_object(
                &mut commands,
                &ents,
                ModelKind::Doodad,
                handle_label(&d.handle),
                unique_id,
                format!(
                    "emitters: {} · WMO prop · {}",
                    m.emitters.len(),
                    d.light.inspector_label()
                ),
            );
            p.entities.extend(ents);
            d.spawned = true;
            activity.placements_spawned += 1;
            spawned_n += 1;
            if Instant::now() >= deadline || spawned_n >= count_cap {
                break 'placements;
            }
        }
    }
    activity.spawn_ms += t0.elapsed().as_secs_f32() * 1000.0;
}

/// Resolve a WMO instance's visible doodad props: the global set (0, always shown) plus the
/// placement's MODF-selected set, each prop composed onto the WMO's world transform and given its M2
/// handle (loaded async, spawned by [`spawn_loaded_placements`] when ready). Props with an unresolved
/// name are skipped.
fn resolve_wmo_doodads(
    wmo: &WmoModel,
    doodad_set: u16,
    wmo_world: Transform,
    asset_server: &AssetServer,
) -> Vec<WmoDoodadInst> {
    // Set 0 is the always-shown global set; a non-zero placement set adds one more.
    let mut ranges: Vec<(u32, u32)> = wmo
        .doodad_sets
        .first()
        .map(|s| (s.start, s.count))
        .into_iter()
        .collect();
    if doodad_set != 0 {
        if let Some(s) = wmo.doodad_sets.get(doodad_set as usize) {
            ranges.push((s.start, s.count));
        }
    }
    let mut out = Vec::new();
    for (start, count) in ranges {
        for (di, d) in wmo
            .doodads
            .iter()
            .enumerate()
            .skip(start as usize)
            .take(count as usize)
        {
            if d.model.is_empty() {
                continue; // MODN name offset didn't resolve
            }
            let local = wmo_doodad_local(d.position, d.orientation, d.scale);
            // Interior classification (MODR ownership) + the MODD-colour base + the owning group's
            // MOLR refs were all resolved ONCE at asset load (`WmoModel::doodad_base`); here the
            // placement composes the referenced lights into WORLD space so the spawn-time fold
            // never needs the WMO asset again.
            let light = match wmo.doodad_base.get(di) {
                Some(DoodadBase::Interior(b)) => PropLight::Interior {
                    ambient: b.ambient,
                    diffuse: b.diffuse,
                    lights: b
                        .light_refs
                        .iter()
                        .filter_map(|&li| wmo.lights.get(li as usize))
                        .filter(|l| l.is_omni())
                        .map(|l| PropLobeLight {
                            pos: wmo_world.transform_point(wow_to_bevy(l.position)),
                            color_i: l.color.map(|c| c * l.intensity.max(0.0)),
                            atten_start: l.attenuation_start,
                            atten_end: l.attenuation_end,
                        })
                        .collect(),
                },
                _ => PropLight::Exterior,
            };
            out.push(WmoDoodadInst {
                handle: asset_server.load(m2_url(&d.model)),
                transform: wmo_world.mul_transform(local),
                // Every MODR referrer, inverted with the lighting base at load — the prop's
                // portal-cull key, so a lantern is hidden with the room it hangs in (decision 0689)
                // and a lava fall the rooms below and above both name survives either being culled.
                groups: wmo.doodad_groups.get(di).cloned().unwrap_or_default(),
                light,
                spawned: false,
            });
        }
    }
    out
}

/// A model handle's source path as a readable label for the object inspector (the asset path without
/// the `mpq://` source prefix). Empty if the handle carries no path.
/// A placement's model-forms build priority (decision 0834): its Chebyshev tile distance to the
/// stream focus, scaled to leave the band below for the entity lane — a mob walking into view
/// never queues behind a city's scenery. `translation` is Bevy space (the placement transform);
/// `world_to_tile` wants WoW ground coords, the inverse of decision 0002's rotation.
fn placement_priority(streamer: &TerrainStreamer, translation: Vec3) -> i32 {
    let (tx, ty) = world_to_tile(-translation.z, -translation.x);
    let d = (tx as i32 - streamer.focus.0)
        .abs()
        .max((ty as i32 - streamer.focus.1).abs());
    16 + d * 16
}

fn handle_label<A: Asset>(handle: &Handle<A>) -> String {
    handle
        .path()
        .map(|p| p.path().to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Tag every spawned entity of a placement with its [`WorldObject`] identity (so the mouseover inspector
/// can name it). Harmless on the non-mesh entities (colliders) — only mesh entities are ray-picked.
fn tag_world_object(
    commands: &mut Commands,
    ents: &[Entity],
    kind: ModelKind,
    label: String,
    id: u32,
    detail: String,
) {
    let object = WorldObject {
        kind,
        label,
        id,
        detail,
    };
    for &e in ents {
        commands.entity(e).insert(object.clone());
    }
}

/// The distance-fade size for an M2: world radius = authored bounding-sphere radius × placement scale;
/// centre = authored bbox centre in Bevy model-local space. Absent bounds ⇒ never size-fade.
pub(crate) fn m2_fade(bounds: &Option<M2Bounds>, scale: f32) -> (f32, Vec3) {
    match bounds {
        Some(b) => {
            let c = [
                (b.bbox_min[0] + b.bbox_max[0]) * 0.5,
                (b.bbox_min[1] + b.bbox_max[1]) * 0.5,
                (b.bbox_min[2] + b.bbox_max[2]) * 0.5,
            ];
            (b.sphere_radius * scale, wow_to_bevy(c))
        }
        None => (f32::INFINITY, Vec3::ZERO),
    }
}
