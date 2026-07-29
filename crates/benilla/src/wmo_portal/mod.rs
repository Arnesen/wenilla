//! WMO portal-based visibility culling — the per-frame pass that decides which of a building's groups
//! are reachable through portals from the camera's current group, so an interior the camera can't see
//! through any doorway (the Stormwind cathedral above the Trade District) is not drawn.
//!
//! This is the **faithful** mechanism, re-derived from `WoW.exe` 5875 (`wow-5875-re`
//! `system/models/models.md` → "WMO portal-based visibility culling", VERIFIED). The client carries a
//! 2-D screen rectangle as its working frustum and floods the portal graph from the camera's group:
//! for each portal it (1) tests the camera is on the portal's front side, (2) projects the portal
//! polygon to a screen rect, (3) intersects that with the incoming rect, and (4) recurses into the
//! neighbour with the narrowed rect — terminating a branch the moment the rect collapses to zero area.
//! That collapse is exactly why the cathedral culls from the Trade District but draws from the gates.
//!
//! Exterior shells: a building's outer-shell group (an inn's walls/roof) often carries **no portals**, so
//! the interior flood can't reach it. The client draws exterior groups from inside via a separate
//! *deferred-window* pass, **gated** on the flood having reached a doorway onto the outdoors. We
//! reproduce that gate (`wow-5875-re` `wmo-insideleg-phase3.md`): reach a portal onto an exterior group ⇒
//! draw the exterior shell (frustum-culled); reach none ⇒ leave it culled (you can't see out).
//!
//! Seam: this module only **computes** the per-group visible set ([`WmoPortalInstance::visible`]). The
//! single `Visibility` authority ([`crate::debug_panel`]'s `apply_model_visibility`, decision 0025)
//! reads it and ANDs it with the dev toggles + the far-clip cull — we never write `Visibility` here.
//!
//! Current-group seed: the **downward raycast** in [`seed`] — the byte-audited client mechanism
//! (`wow-5875-re` `wmo-current-group.md` + `wmo-portal-audit.md`): walking-collision faces race portal
//! crossings under the eye, and the verdict is a **seed set** (in-group + across-group), each flooded
//! as an independent root. The epsilons here (side test exactly `0.0`, rect collapse `0.001`, the
//! `w`-clamp pair, the `0.1` near-parallel snap) are the client's own constants, read from the binary
//! — do not "fix" them to tolerances without re-deriving there first.

use std::sync::Arc;

mod fog;
mod interior;
mod probe;
mod seed;

use fog::select_wmo_fog;
pub use fog::{CameraWmoFog, WmoFogTarget};
pub(crate) use interior::{
    indoor_verdict_at, indoors_at, terrain_z_local, IndoorVerdict, INTERIOR_PROBE_HEIGHT,
};
use interior::{track_area_interior, track_current_interior, track_unit_interiors};
pub use interior::{CurrentAreaInterior, CurrentWmoInterior, PlayerWmoRoom, UnitWmoRoom};
pub use probe::WmoCullProbe;
use probe::{TraceLog, PROBE_DUMP_PATH};
use seed::dominant_axes;
pub(crate) use seed::{down_ray_seeds, floor_z_at, DownRaySeeds};

use crate::player::WorldCamera;
use crate::terrain_stream::{terrain_height_under, TerrainStreamer};
use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use benilla_assets::{AdtTile, WmoModel, WmoPortalInfo};
use bevy::camera::Projection;
use bevy::math::{Affine3A, Vec4};
use bevy::prelude::*;

/// MOGP EXTERIOR flag — an "outdoor" group. The flood defers these to the open world rather than
/// treating them as portal-gated interiors (and seeds the outside leg from them).
const EXTERIOR: u32 = 0x8;

/// MOGP EXTERIOR_LIT flag — an interior-graph group lit as OUTDOORS. The unit light classify
/// (`0x6a87f0`) forks its LIGHTING class on `MOGI & 0x48` — `0x8` **or** `0x40` → the exterior leg
/// (`or [node+0xc],0x4`: sun diffuse × the MCSH-driven 2.5/0.5 intensity, scene fog) — while the
/// zone-text indoor bit (`[node+0x90]` bit 0) keys on `0x8` alone, so a `0x40`-only group is
/// "indoors" for area naming but sunlit (wow-re `zonetext-indoor-bit.md` §b,
/// `m2-interior-doodad-base-light.md` §the-classify, `unit-m2-shader-light.md`). The city street
/// WMOs are exactly this: Stormwind's streets (115 of 306 groups) and Orgrimmar's valleys are
/// `0x40`-without-`0x8` (decision 0475).
const EXTERIOR_LIT: u32 = 0x40;

/// Eye-on-portal-plane band (WMO yards): an eye within this of a portal's plane **and inside its
/// polygon** gets the full-screen rect for that portal (the client's `0x6b46f0` special case, band
/// `|d| <= 0.01` at `0x8029d0`). The side test still runs — the recorded "skip the side test" flag is
/// dead code in the client (`wow-5875-re` `wmo-portal-audit.md` Q1 correction).
const ON_PLANE_EPS: f32 = 0.01;
/// The `|w|` band below which a clip-space vertex's `w` is substituted before the perspective divide —
/// the client's `0x801360` (`0.001`), strict `<`.
const W_CLAMP_BAND: f32 = 0.001;
/// The substituted `w` — **positive regardless of the vertex's sign** (the client's `1e-5` immediate
/// `0x3727c5ac`). A vertex with `w <= -0.001` is NOT clamped: it divides by its real negative `w`.
const W_CLAMP_SUB: f32 = 1.0e-5;
/// Minimum NDC extent for a narrowed screen rect to count as non-empty — the client's zero-area eps
/// `0x801360` (`0.001`); a collapsed window kills the branch.
const RECT_EPS: f32 = 0.001;
/// Ray-vs-plane near-parallel threshold on the denominator (the client's f64 `0x811658` = `0.0001`):
/// below it, the down-ray is parallel to the portal plane and the crossing exists only via the snap.
const PORTAL_NEAR_PARALLEL: f32 = 1.0e-4;
/// The "eye embedded in the plane" snap window (yd) for a near-parallel portal (the client's `0.1`
/// immediate pushed at `0x6a40c0`): a vertical doorway counts as crossed by the vertical down-ray only
/// while the eye is within this of its plane.
const PORTAL_PLANE_SNAP: f32 = 0.1;
/// Nearest-crossing accept eps for the portal leg (the client's `0x80c4f4` ≈ `1e-4`): a portal crossing
/// within this of the running nearest face hit still wins — a floor-hole portal coincident with the
/// slab's collision faces flips the room at the plane.
const NEAREST_TIE_EPS: f32 = 1.0e-4;
/// Recursion depth cap (the client carries one) — a backstop against pathological portal cycles.
const DEPTH_CAP: u32 = 64;
/// Hard cap on flood iterations — a runaway-graph backstop; real WMOs settle far below this.
const MAX_ITERS: u32 = 1 << 16;

/// An NDC axis-aligned rectangle `[min_x, min_y, max_x, max_y]` — the "screen frustum" carried through
/// the flood. The full screen is `[-1, -1, 1, 1]`.
type Rect = [f32; 4];
const FULL_SCREEN: Rect = [-1.0, -1.0, 1.0, 1.0];

/// Static tag on every portal-culled WMO piece — a group render submesh, a group's MLIQ surface, a
/// MODD prop — naming which placement instance it belongs to and which absolute group indices can
/// draw it, so the visibility authority can look up their per-frame PVS bits.
///
/// Geometry and liquid belong to exactly one group. A **prop** can be named by many: the reference
/// draws doodads per *visible* group out of that group's own MODR, so every referrer is an
/// independent chance to be drawn (see [`Self::drawn_by`]). Built once per group/prop at spawn, so
/// the `Arc` is one allocation per key, not one per submesh.
#[derive(Component, Clone)]
pub struct WmoGroupVis {
    pub instance: Entity,
    pub groups: Arc<[u16]>,
}

impl WmoGroupVis {
    /// The portal verdict: drawn iff **any** referencing group is in this frame's PVS. An index
    /// past the instance's visible set (a stale or short set) reads as visible — the same fail-open
    /// the rest of the cull takes, so a lookup miss can never blank a building.
    pub fn drawn_by(&self, inst: &WmoPortalInstance) -> bool {
        self.groups
            .iter()
            .any(|&g| inst.visible.get(g as usize).copied().unwrap_or(true))
    }
}

/// One placed WMO building's portal-cull state. Spawned alongside the building's geometry; despawns
/// with the placement (it's pushed into the placement's entity list).
#[derive(Component)]
pub struct WmoPortalInstance {
    /// The building asset (portal graph + per-group nav).
    pub handle: Handle<WmoModel>,
    /// Placement transform as Bevy **model→world**. A portal vertex is WoW model space, so it reaches
    /// world via `world_from_local * wow_to_bevy(v)` — the same path `build_submesh_mesh` bakes the
    /// rendered geometry through, so portals and walls live in one space.
    pub world_from_local: Affine3A,
    /// The placement's `MODF.nameSet` — the `WMOAreaTable.NameSetID` audio/naming variant
    /// (one abbey model serves Northshire and Tyr's Hand by name set).
    pub name_set: u16,
    /// Per-group visibility (indexed by absolute group index) for this frame; `true` = in the portal
    /// PVS. Seeded all-`true` (everything shows until the first compute, and forever for portal-less
    /// props).
    pub visible: Vec<bool>,
}

/// One WMO **room**: a placed building's instance entity plus the absolute group index inside it.
///
/// This is the client's `[0xc7b748]` (the camera-containing placed map-object — wow-re
/// `models/scratch/wmo-current-group.md`) paired with the current group index its visible-group set
/// `0xc7cd88` carries. It is the identity every "which room is this subject in" answer resolves to,
/// and — since decision 0696 — the **scope key of the liquid query**: a building's pool belongs to a
/// room, so only a subject standing in that placement can be in it. A liquid footprint has no floor,
/// so without an owner a pool claims every position under its XY forever, in any building, at any
/// depth (Uldaman read as submerged under a *mushroom cave's* water 186 yd overhead).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WmoRoom {
    /// The placement's [`WmoPortalInstance`] entity.
    pub instance: Entity,
    /// Absolute group index within the building.
    pub group: u16,
}

/// The camera's PVS-flood interior claim this frame — `None` with the camera over open world or
/// an EXTERIOR (`0x8`) group's floor; an EXTERIOR_LIT-only porch (`0x40`) still claims (the
/// `[0xc7b748]` writer's 0x8-only rejection, round-6 Q-H(c)). Written by [`compute_wmo_pvs`] off
/// the same down-ray seed the flood runs; consumed by the weather-visible gate
/// (`weather::precip`) and by the camera-eye submersion verdict (`liquid::detect_submersion` —
/// the reference's per-frame environment probe `0x6809c0` picks the WMO group's MLIQ over the ADT
/// liquid on exactly this global, wow-re `terrain/scratch/fog-env-state.md` §1).
/// When `Some`, `exterior_visible` reports whether this placement's flood
/// reached an EXTERIOR-flagged group (`flags & 0x148`) through a screen-rect-surviving portal
/// window — the carved pass-2 law of the weather flag `[0xca80c4]` (`0x6b42d9`, round-6 Q-H(a)):
/// rain stays visible — and falling — through a doorway the camera can SEE; face away and the
/// window clips out, killing the draw that frame (view-dependent by design).
#[derive(Resource, Default, Clone, Copy, PartialEq)]
pub struct CameraInteriorClaim(pub Option<InteriorClaim>);

/// See [`CameraInteriorClaim`].
#[derive(Clone, Copy, PartialEq)]
pub struct InteriorClaim {
    /// The room the camera EYE is in — the placement + its current group.
    pub room: WmoRoom,
    /// An EXTERIOR-flagged group (`flags & 0x148`) is in the claiming placement's PVS.
    pub exterior_visible: bool,
}

/// Ordering handle so the `Visibility` authority can run **after** the PVS is computed this frame.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WmoPvsSet;

/// Registers the per-frame PVS compute. The apply side lives in [`crate::debug_panel`].
pub struct WmoPortalPlugin;

impl Plugin for WmoPortalPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CurrentWmoInterior>()
            .init_resource::<PlayerWmoRoom>()
            .init_resource::<CurrentAreaInterior>()
            .init_resource::<CameraWmoFog>()
            .init_resource::<CameraInteriorClaim>()
            .init_resource::<WmoCullProbe>()
            .add_systems(
                Update,
                (
                    compute_wmo_pvs,
                    track_current_interior,
                    track_area_interior,
                    track_unit_interiors,
                )
                    .in_set(WmoPvsSet),
            );
    }
}

/// Max drop (yd) for the current-group down-ray: a floor more than this far below the eye doesn't count
/// as "the room you're in" (you're over open space ⇒ outside). Matches the client's ray length (`1760`).
const MAX_FLOOR_DROP: f32 = 1760.0;

/// Recompute each resident WMO's per-group visible set from the camera. Cheap: a handful of buildings,
/// each a small portal flood; a portal-less prop just stays all-visible.
#[allow(clippy::too_many_arguments)] // a Bevy system: each arg is a distinct resource/query
fn compute_wmo_pvs(
    wmos: Res<Assets<WmoModel>>,
    cam: Query<(&GlobalTransform, &Projection), With<WorldCamera>>,
    mut instances: Query<(Entity, &mut WmoPortalInstance)>,
    streamer: Res<TerrainStreamer>,
    adt_tiles: Res<Assets<AdtTile>>,
    mut probe: ResMut<WmoCullProbe>,
    mut camera_fog: ResMut<CameraWmoFog>,
    mut camera_claim: ResMut<CameraInteriorClaim>,
) {
    // The world Camera3d (the egui overlay is a Camera2d). None before it spawns ⇒ leave last frame's
    // sets untouched (everything was visible, which is the safe default).
    let Some((cam_t, proj)) = cam.iter().next() else {
        return;
    };
    let clip_from_world = proj.get_clip_from_view() * cam_t.to_matrix().inverse();
    let eye_world = cam_t.translation();
    // The terrain leg of the seed's down-ray, sampled once for the camera's column: the client casts the
    // same segment at the ground and drops the WMO hit when the ground is nearer (`FUN_006821f0`).
    let terrain = terrain_height_under(&streamer, &adt_tiles, eye_world);
    let dump = std::mem::take(&mut probe.dump_requested);
    let mut dump_text = dump.then(|| format!("eye world: {eye_world:?}\n"));

    // The camera's MFOG fog target + interior claim — resolved from the same down-ray seed the
    // flood runs anyway (see [`fog`]); the first placement whose seed claims a TRUE INTERIOR
    // group wins both.
    let mut fog_target: Option<WmoFogTarget> = None;
    let mut claim: Option<InteriorClaim> = None;
    for (entity, mut inst) in &mut instances {
        let Some(model) = wmos.get(&inst.handle) else {
            continue;
        };
        let groups = model.group_nav.len();
        // A WMO with no portal graph (single-group props, doors) is never culled — keep all groups on.
        if model.portal_refs.is_empty() || model.portal_infos.is_empty() {
            if inst.visible.len() != groups {
                inst.visible = vec![true; groups];
            } else {
                inst.visible.iter_mut().for_each(|v| *v = true);
            }
            continue;
        }
        let world_from_local = inst.world_from_local;
        // The eye in this WMO's model space (WoW axes): the down-ray origin (seed) and the portal
        // front-side test point. The down-ray from the camera is robust on its own — a collision-shoved
        // camera still drops onto the floor beneath it — so there's no player-position seed any more.
        let local_from_world = world_from_local.inverse();
        let eye_local = bevy_to_wow(local_from_world.transform_point3(eye_world));
        let terrain_local = terrain.map(|z| terrain_z_local(&local_from_world, eye_world, z));
        if let Some(text) = &mut dump_text {
            let mut trace = TraceLog::new(model, eye_local, terrain_local);
            inst.visible = compute_pvs_traced(
                model,
                eye_local,
                terrain_local,
                &clip_from_world,
                &world_from_local,
                &mut trace,
            );
            text.push_str(&trace.text);
            text.push_str(&format!("visible: {:?}\n\n", inst.visible));
        } else {
            let mut tap = SeedTap::default();
            inst.visible = compute_pvs_traced(
                model,
                eye_local,
                terrain_local,
                &clip_from_world,
                &world_from_local,
                &mut tap,
            );
            // Interior-fog + weather claims off the seed already computed. The claim mask is
            // bit 0x8 ALONE (`[0xc7b748]` writer `0x6be451/77` tests only EXTERIOR — round 6
            // Q-H(c)): an EXTERIOR_LIT-only porch/courtyard (`0x40`) still claims the camera
            // and engages this WMO's MFOG fog; the weather gate reads whether the placement's
            // flood reached an exterior group (already view-clipped — the deferred-window gate
            // fires only when the doorway's screen rect survives, the carved pass-1 law).
            if claim.is_none() {
                if let Some(gi) = tap.in_group {
                    if let Some(nav) = model.group_nav.get(gi).filter(|n| n.flags & 0x8 == 0) {
                        fog_target = select_wmo_fog(&model.fogs, nav.fog_indices, eye_local);
                        claim = Some(InteriorClaim {
                            room: WmoRoom {
                                instance: entity,
                                group: gi as u16,
                            },
                            exterior_visible: model
                                .group_nav
                                .iter()
                                .zip(&inst.visible)
                                .any(|(n, v)| *v && n.flags & 0x148 != 0),
                        });
                    }
                }
            }
        }
    }
    if camera_fog.0 != fog_target {
        camera_fog.0 = fog_target;
    }
    if camera_claim.0 != claim {
        camera_claim.0 = claim;
    }
    if let Some(text) = dump_text {
        match std::fs::write(PROBE_DUMP_PATH, &text) {
            Ok(()) => info!("wmo cull trace written to {PROBE_DUMP_PATH}"),
            Err(e) => warn!("wmo cull trace: cannot write {PROBE_DUMP_PATH}: {e}"),
        }
    }
}

/// The per-frame [`FloodTrace`] tap: records only the down-ray's in-group seed (the camera's
/// current group), which the interior-fog resolve reads — no duplicate down-ray.
#[derive(Default)]
struct SeedTap {
    in_group: Option<usize>,
}
impl FloodTrace for SeedTap {
    fn seed(&mut self, seeds: DownRaySeeds) {
        self.in_group = seeds.in_group;
    }
}

/// A tap on the flood's per-portal decisions. The `()` impl is a no-op the per-frame path uses; the
/// probe dump and the audit harness pass a real recorder — one flood implementation, no trace copy to
/// drift.
pub(crate) trait FloodTrace {
    fn seed(&mut self, _seeds: DownRaySeeds) {}
    fn side_fail(&mut self, _from: usize, _portal: u16, _to: usize, _d: f32) {}
    fn rect_none(&mut self, _from: usize, _portal: u16, _to: usize) {}
    fn rect_collapse(&mut self, _from: usize, _portal: u16, _to: usize) {}
    fn entered(&mut self, _from: usize, _portal: u16, _to: usize, _rect: Rect, _on_plane: bool) {}
}
impl FloodTrace for () {}

/// The portal flood. Returns the per-group visible set (indexed by absolute group index). `eye_local` is
/// the camera in WMO model space (WoW axes): it seeds the flood (the group whose floor it stands over —
/// the down-ray) and drives the per-portal front-side test. `clip_from_world` / `world_from_local`
/// project a portal vertex (WoW model space) to clip space via `clip_from_world * world_from_local *
/// wow_to_bevy(v)`. (The per-frame pass now calls [`compute_pvs_traced`] directly with a
/// [`SeedTap`] — the interior-fog resolve reads the seed — so this trace-less wrapper serves the
/// audit harness.)
#[cfg(test)]
fn compute_pvs(
    model: &WmoModel,
    eye_local: [f32; 3],
    terrain_z: Option<f32>,
    clip_from_world: &Mat4,
    world_from_local: &Affine3A,
) -> Vec<bool> {
    compute_pvs_traced(
        model,
        eye_local,
        terrain_z,
        clip_from_world,
        world_from_local,
        &mut (),
    )
}

/// [`compute_pvs`] with a [`FloodTrace`] tap on every decision — the same single flood; the probe dump
/// and the audit harness pass a recorder, the per-frame path passes `()`.
fn compute_pvs_traced<T: FloodTrace>(
    model: &WmoModel,
    eye_local: [f32; 3],
    terrain_z: Option<f32>,
    clip_from_world: &Mat4,
    world_from_local: &Affine3A,
    trace: &mut T,
) -> Vec<bool> {
    let nav = &model.group_nav;
    let mut visible = vec![false; nav.len()];

    // Seed: the down-ray's set — the camera's current group AND, when the nearest crossing was a
    // portal, the group on its other side. Each is an independent flood ROOT with the full screen
    // (the client appends both to the visible-group set `0xc7cd88` and the inside leg loops `visit()`
    // over the whole set — `wow-5875-re` `wmo-portal-audit.md` Q2; seeding only the containing group
    // under-floods at exactly the near-portal moment the second root exists to cover). Over no surface
    // (open air) or an exterior one ⇒ the outside leg, seeding each EXTERIOR group full-screen (Bevy
    // then frustum-culls the genuinely off-screen ones by their Aabb). Stack item: (group, came_from,
    // working screen rect, depth).
    let mut stack: Vec<(usize, usize, Rect, u32)> = Vec::new();
    let seeds = down_ray_seeds(model, eye_local, terrain_z);
    trace.seed(seeds);
    match seeds.in_group {
        Some(c) => {
            stack.push((c, usize::MAX, FULL_SCREEN, 0));
            if let Some(a) = seeds.across {
                stack.push((a, usize::MAX, FULL_SCREEN, 0));
            }
        }
        None => {
            for (gi, g) in nav.iter().enumerate() {
                if g.flags & EXTERIOR != 0 {
                    stack.push((gi, usize::MAX, FULL_SCREEN, 0));
                }
            }
        }
    }

    // Did the interior flood reach a doorway onto an exterior group? The client's deferred-window gate
    // (`wow-5875-re` `wmo-insideleg-phase3.md`): only then is the exterior shell drawn (below).
    let mut deferred_exterior = false;
    let mut iters = 0u32;
    while let Some((g, came, rect, depth)) = stack.pop() {
        iters += 1;
        if iters > MAX_ITERS || g >= nav.len() {
            continue;
        }
        visible[g] = true;
        if depth >= DEPTH_CAP {
            continue;
        }
        let nav_g = &nav[g];
        let start = nav_g.ref_start as usize;
        let end = (start + nav_g.ref_count as usize).min(model.portal_refs.len());
        for r in &model.portal_refs[start..end] {
            let neighbour = r.group as usize;
            // Never re-cross the entry portal, and skip the "no group" sentinel.
            if r.group == u16::MAX || neighbour == came {
                continue;
            }
            let Some(info) = model.portal_infos.get(r.portal as usize) else {
                continue;
            };
            // Front-side test: the camera must be on the `side`-oriented half-space to enter. The
            // threshold IS zero — skip iff `d' < 0` strictly, enter on exactly-0 (the client's
            // `DAT_007ffd74 = 0.0`) — and it ALWAYS runs: the recorded on-plane "skip the side test"
            // flag is dead code in the client (`wmo-portal-audit.md` Q1 correction).
            let mut d = info.plane[0] * eye_local[0]
                + info.plane[1] * eye_local[1]
                + info.plane[2] * eye_local[2]
                + info.plane[3];
            if r.side < 0 {
                d = -d;
            }
            if d < 0.0 {
                trace.side_fail(g, r.portal, neighbour, d);
                continue;
            }
            // The eye standing in this portal's plane (inside its polygon) gets the full-screen rect
            // (the client's `0x6b46f0` special case). Without this, a camera crossing a doorway clips
            // the room ahead to nothing for a frame.
            let on_plane = eye_on_portal(&model.portal_vertices, info, eye_local);
            // Project the portal polygon to a screen rect; intersect with the carried rect. Either
            // collapsing to zero area kills this branch (the cathedral case).
            let prect = if on_plane {
                FULL_SCREEN
            } else {
                match portal_screen_rect(model, info, clip_from_world, world_from_local) {
                    Some(p) => p,
                    None => {
                        trace.rect_none(g, r.portal, neighbour);
                        continue;
                    }
                }
            };
            let Some(inter) = intersect_rect(rect, prect) else {
                trace.rect_collapse(g, r.portal, neighbour);
                continue;
            };
            trace.entered(g, r.portal, neighbour, inter, on_plane);
            // Reaching a portal onto an exterior group records a deferred window; the interior neighbour
            // is still flooded normally.
            if nav.get(neighbour).is_some_and(|n| n.flags & EXTERIOR != 0) {
                deferred_exterior = true;
            }
            stack.push((neighbour, g, inter, depth + 1));
        }
    }

    // Deferred exterior-window pass — the faithful approximation of the client's Pass 2 (`wow-5875-re`
    // `wmo-insideleg-phase3.md`): once the interior flood reaches a doorway onto an exterior group, the
    // building's exterior shell is drawn — INCLUDING portal-disconnected groups (an inn's outer walls
    // carry no portals, so the flood alone never reaches them, which is why they vanished from inside).
    // Gated exactly like the client: no doorway onto an exterior reached ⇒ the shell stays culled (you
    // can't see out, so it isn't drawn). We draw the shell to the full frustum (Bevy's per-Aabb cull
    // bounds it) rather than clipped to each doorway window — a benign over-draw, since from inside you're
    // meant to see the enclosing walls anyway.
    if deferred_exterior {
        for (gi, g) in nav.iter().enumerate() {
            if g.flags & EXTERIOR != 0 {
                visible[gi] = true;
            }
        }
    }
    visible
}

/// The portal's polygon vertices (WoW model space), or `None` if the span is out of range or
/// degenerate (< 3 vertices).
fn portal_poly<'a>(
    portal_vertices: &'a [[f32; 3]],
    info: &WmoPortalInfo,
) -> Option<&'a [[f32; 3]]> {
    let start = info.start_vertex as usize;
    let verts = portal_vertices.get(start..start + info.count as usize)?;
    (verts.len() >= 3).then_some(verts)
}

/// Does the eye lie **in** this portal — within [`ON_PLANE_EPS`] of its plane (inclusive, the client's
/// `|d| <= 0.01`) and inside its polygon (dominant-axis 2-D projection, the client's `0x7c23e0`)? The
/// flood then gives the portal the full-screen rect — the client's `0x6b46f0` special case. The side
/// test still applies (Q1 correction: the skip flag is dead).
fn eye_on_portal(portal_vertices: &[[f32; 3]], info: &WmoPortalInfo, eye: [f32; 3]) -> bool {
    let [nx, ny, nz, d] = info.plane;
    if (nx * eye[0] + ny * eye[1] + nz * eye[2] + d).abs() > ON_PLANE_EPS {
        return false;
    }
    let Some(verts) = portal_poly(portal_vertices, info) else {
        return false;
    };
    // Project out the plane's dominant axis; the eye is on the plane, so its projection is itself.
    let (u, v) = dominant_axes(info.plane);
    point_in_poly_2d(verts.iter().map(|p| (p[u], p[v])), (eye[u], eye[v]))
}

/// Even-odd point-in-polygon on 2-D projected vertices.
fn point_in_poly_2d(pts: impl Iterator<Item = (f32, f32)> + Clone, p: (f32, f32)) -> bool {
    let mut inside = false;
    let mut prev = pts.clone().last();
    for cur in pts {
        if let Some(pr) = prev {
            if (cur.1 > p.1) != (pr.1 > p.1)
                && p.0 < (pr.0 - cur.0) * (p.1 - cur.1) / (pr.1 - cur.1) + cur.0
            {
                inside = !inside;
            }
        }
        prev = Some(cur);
    }
    inside
}

/// Project a portal polygon to its NDC bounding rectangle — the client's `0x6b46f0`: transform to
/// clip space, Sutherland–Hodgman clip against the four **side** planes of the view pyramid (there is
/// NO near-plane clip), then min/max the per-vertex NDC with the faithful `w` handling: `|w| < 0.001`
/// substitutes `w = +1e-5` (positive regardless of the vertex's sign); a vertex still carrying
/// `w <= -0.001` divides by its real negative `w`, so its **mirrored** NDC enters the rect. That is
/// what keeps a doorway the eye is straddling wide open (the boundary points at the eye clamp to
/// `+1e-5` and blow the rect out) instead of collapsing it for a frame. The rect is returned raw
/// (un-clamped) — the caller's intersect with the carried rect bounds it. `None` iff the polygon is
/// fully clipped (degenerate / off-screen) — the client's `rc.flags |= 0x1` skip.
fn portal_screen_rect(
    model: &WmoModel,
    info: &WmoPortalInfo,
    clip_from_world: &Mat4,
    world_from_local: &Affine3A,
) -> Option<Rect> {
    let verts = portal_poly(&model.portal_vertices, info)?;
    // WoW model vertex → world → clip (the same path the rendered geometry takes).
    let clip: Vec<Vec4> = verts
        .iter()
        .map(|v| {
            let world = world_from_local.transform_point3(wow_to_bevy(*v));
            *clip_from_world * world.extend(1.0)
        })
        .collect();
    let clipped = clip_side_planes(&clip)?;
    let (mut min_x, mut min_y) = (f32::INFINITY, f32::INFINITY);
    let (mut max_x, mut max_y) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for c in &clipped {
        let w = if c.w.abs() < W_CLAMP_BAND {
            W_CLAMP_SUB
        } else {
            c.w
        };
        let inv = 1.0 / w;
        let (x, y) = (c.x * inv, c.y * inv);
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    Some([min_x, min_y, max_x, max_y])
}

/// Sutherland–Hodgman clip of a clip-space polygon against the four **side** planes of the view
/// pyramid (`x ≥ −w`, `x ≤ w`, `y ≥ −w`, `y ≤ w`, each linear in clip space — the client's guard
/// pyramid `0xc7d0dc` fed to `poly_clip 0x6b4a80`). Deliberately no near plane: a polygon spanning
/// the eye survives as boundary points at/near `w = 0`, which the caller's `w`-clamp handles — the
/// faithful behaviour. `None` if fewer than 3 vertices remain (fully clipped).
fn clip_side_planes(poly: &[Vec4]) -> Option<Vec<Vec4>> {
    const PLANES: [fn(&Vec4) -> f32; 4] =
        [|v| v.w + v.x, |v| v.w - v.x, |v| v.w + v.y, |v| v.w - v.y];
    let mut cur = poly.to_vec();
    for f in PLANES {
        let n = cur.len();
        let mut out: Vec<Vec4> = Vec::with_capacity(n + 2);
        for i in 0..n {
            let a = cur[i];
            let b = cur[(i + 1) % n];
            let (fa, fb) = (f(&a), f(&b));
            if fa >= 0.0 {
                out.push(a);
            }
            if (fa >= 0.0) != (fb >= 0.0) {
                let t = fa / (fa - fb);
                out.push(a + (b - a) * t);
            }
        }
        cur = out;
        if cur.len() < 3 {
            return None;
        }
    }
    Some(cur)
}

/// Intersect two NDC rects; `None` if they don't overlap with non-zero area.
fn intersect_rect(a: Rect, b: Rect) -> Option<Rect> {
    let rect = [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ];
    (rect[2] - rect[0] >= RECT_EPS && rect[3] - rect[1] >= RECT_EPS).then_some(rect)
}

#[cfg(test)]
mod audit;

#[cfg(test)]
mod tests {
    use super::*;

    /// A prop is drawn while ANY room that names it is visible — the reference's per-visible-group
    /// MODR walk. Blackrock's lava falls hang through up to 19 groups; keying them to one owner hid
    /// the fall from every camera angle whose flood reached a different referrer. Geometry (a
    /// single-group key) is unaffected, and an index past the visible set fails OPEN.
    #[test]
    fn a_prop_is_drawn_while_any_of_its_rooms_is_visible() {
        let inst = WmoPortalInstance {
            handle: Handle::default(),
            world_from_local: Affine3A::IDENTITY,
            name_set: 0,
            visible: vec![false, true, false],
        };
        let key = |groups: &[u16]| WmoGroupVis {
            instance: Entity::PLACEHOLDER,
            groups: Arc::from(groups),
        };
        assert!(key(&[1]).drawn_by(&inst), "its own room is visible");
        assert!(!key(&[0]).drawn_by(&inst), "its own room is culled");
        assert!(
            key(&[0, 2, 1]).drawn_by(&inst),
            "one visible referrer out of three draws the fall"
        );
        assert!(!key(&[0, 2]).drawn_by(&inst), "no referrer is visible");
        assert!(
            !key(&[]).drawn_by(&inst),
            "no referrers at all: nothing ORs"
        );
        assert!(key(&[9]).drawn_by(&inst), "index past the set fails open");
    }

    #[test]
    fn eye_on_portal_only_within_the_band_and_polygon() {
        // A vertical doorway portal in the x=0 plane, spanning y ∈ [-2,2], z ∈ [0,4].
        let verts = vec![
            [0.0, -2.0, 0.0],
            [0.0, 2.0, 0.0],
            [0.0, 2.0, 4.0],
            [0.0, -2.0, 4.0],
        ];
        let info = WmoPortalInfo {
            start_vertex: 0,
            count: 4,
            plane: [1.0, 0.0, 0.0, 0.0],
        };
        // In the plane, inside the polygon ⇒ the doorway is wide open.
        assert!(eye_on_portal(&verts, &info, [0.0, 0.0, 2.0]));
        assert!(eye_on_portal(&verts, &info, [0.005, 1.5, 3.5]));
        // In the plane but outside the polygon (above the door) ⇒ not standing in it.
        assert!(!eye_on_portal(&verts, &info, [0.0, 0.0, 5.0]));
        // Inside the polygon's span but off the plane ⇒ the normal side/rect path applies.
        assert!(!eye_on_portal(&verts, &info, [0.5, 0.0, 2.0]));
    }

    #[test]
    fn point_in_poly_2d_hits_inside_and_misses_outside() {
        let sq = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)];
        assert!(point_in_poly_2d(sq.iter().copied(), (0.0, 0.0)));
        assert!(point_in_poly_2d(sq.iter().copied(), (-0.9, 0.9)));
        assert!(!point_in_poly_2d(sq.iter().copied(), (1.5, 0.0)));
        assert!(!point_in_poly_2d(sq.iter().copied(), (0.0, -1.5)));
    }

    #[test]
    fn intersect_rect_collapses_to_none() {
        // Overlapping rects intersect to the overlap.
        assert_eq!(
            intersect_rect([-1.0, -1.0, 0.0, 0.0], [-0.5, -0.5, 1.0, 1.0]),
            Some([-0.5, -0.5, 0.0, 0.0])
        );
        // Disjoint rects ⇒ the window collapses (the cathedral-cull termination).
        assert_eq!(
            intersect_rect([-1.0, -1.0, -0.5, -0.5], [0.5, 0.5, 1.0, 1.0]),
            None
        );
        // A sliver thinner than the client's 0.001 zero-area eps also collapses.
        assert_eq!(
            intersect_rect([-1.0, -1.0, 1.0, 1.0], [0.0, -1.0, 0.0005, 1.0]),
            None
        );
    }

    #[test]
    fn clip_side_planes_drops_behind_and_off_screen_keeps_straddling() {
        // Fully behind the eye (w < 0, inside the mirror cone) ⇒ outside every side half-space ⇒
        // fully clipped, like the client's rc.flags |= 0x1.
        let behind = [
            Vec4::new(0.0, 0.0, 0.0, -1.0),
            Vec4::new(0.1, 0.0, 0.0, -1.0),
            Vec4::new(0.0, 0.1, 0.0, -1.0),
        ];
        assert!(clip_side_planes(&behind).is_none());
        // Fully in front and on-screen ⇒ unchanged triangle.
        let front = [
            Vec4::new(0.0, 0.0, 0.0, 1.0),
            Vec4::new(0.5, 0.0, 0.0, 1.0),
            Vec4::new(0.0, 0.5, 0.0, 1.0),
        ];
        assert_eq!(clip_side_planes(&front).map(|p| p.len()), Some(3));
        // Fully off-screen laterally (x > w for every vertex) ⇒ fully clipped.
        let off = [
            Vec4::new(2.0, 0.0, 0.0, 1.0),
            Vec4::new(3.0, 0.0, 0.0, 1.0),
            Vec4::new(2.0, 1.0, 0.0, 1.0),
        ];
        assert!(clip_side_planes(&off).is_none());
        // Straddling the eye plane (one vertex behind): survives — the boundary points land at/near
        // w = 0 for the w-clamp to handle. THIS is the case a near-plane clip would mishandle.
        let straddle = [
            Vec4::new(0.0, -0.5, 0.0, 1.0),
            Vec4::new(0.0, 0.5, 0.0, 1.0),
            Vec4::new(0.0, 0.0, 0.0, -0.5),
        ];
        assert!(clip_side_planes(&straddle).is_some());
    }

    #[test]
    fn portal_rect_explodes_open_on_the_doorway_approach() {
        // The camera 0.05 yd before the doorway plane (outside the 0.01 on-plane band), looking
        // through it: the polygon fills far past the screen edges, and with no near-plane clip the
        // rect must blow out to cover the whole screen — the client's protection for the approach
        // frames (the crossing frame itself is the on-plane full-screen case).
        let model = portal_model();
        let info = &model.portal_infos[0];
        // WoW (−0.05, 0, 0) → Bevy (0, 0, 0.05); looking along WoW +x <=> Bevy −z.
        let view = Mat4::look_at_rh(
            bevy::math::Vec3::new(0.0, 0.0, 0.05),
            bevy::math::Vec3::new(0.0, 0.0, -10.0),
            bevy::math::Vec3::Y,
        );
        let clip = Mat4::perspective_rh(0.9, 16.0 / 9.0, 0.1, 1000.0) * view;
        let rect = portal_screen_rect(&model, info, &clip, &Affine3A::IDENTITY)
            .expect("a doorway ahead must keep a rect");
        let inter = intersect_rect(FULL_SCREEN, rect).expect("must overlap the screen");
        // The window is the whole screen while the eye is this deep in the doorway.
        assert!(inter[0] <= -0.99 && inter[1] <= -0.99 && inter[2] >= 0.99 && inter[3] >= 0.99);
    }

    #[test]
    fn portal_rect_is_none_when_fully_behind_the_camera() {
        let model = portal_model();
        let info = &model.portal_infos[0];
        // Camera 5 yd past the doorway (WoW +x maps to Bevy −z), still looking further along −z:
        // the doorway is squarely behind ⇒ fully clipped ⇒ no rect.
        let view = Mat4::look_at_rh(
            bevy::math::Vec3::new(0.0, 0.0, -5.0),
            bevy::math::Vec3::new(0.0, 0.0, -10.0),
            bevy::math::Vec3::Y,
        );
        let clip = Mat4::perspective_rh(0.9, 16.0 / 9.0, 0.1, 1000.0) * view;
        assert!(portal_screen_rect(&model, info, &clip, &Affine3A::IDENTITY).is_none());
    }

    /// A bare model holding one vertical doorway portal in the WoW x=0 plane (y ∈ [-1,1], z ∈ [-1,1]),
    /// i.e. straddling the world origin.
    fn portal_model() -> WmoModel {
        WmoModel {
            wmo_id: 0,
            submeshes: Vec::new(),
            submesh_group: Vec::new(),
            portal_vertices: vec![
                [0.0, -1.0, -1.0],
                [0.0, 1.0, -1.0],
                [0.0, 1.0, 1.0],
                [0.0, -1.0, 1.0],
            ],
            portal_infos: vec![WmoPortalInfo {
                start_vertex: 0,
                count: 4,
                plane: [1.0, 0.0, 0.0, 0.0],
            }],
            portal_refs: Vec::new(),
            group_nav: Vec::new(),
            fogs: Vec::new(),
            skybox: None,
            group_collision_tris: Vec::new(),
            group_camera_only_tris: Vec::new(),
            group_collision_bounds: Vec::new(),
            group_collision_grids: Vec::new(),
            collision_bounds: None,
            collision: None,
            collision_camera: None,
            doodads: Vec::new(),
            doodad_sets: Vec::new(),
            lights: Vec::new(),
            group_bounds: Vec::new(),
            group_footprints: Vec::new(),
            group_footprint_bounds: Vec::new(),
            group_footprint_grids: Vec::new(),
            group_light_refs: Vec::new(),
            group_liquids: Vec::new(),
            doodad_base: Default::default(),
            doodad_owner: Default::default(),
            doodad_groups: Default::default(),
        }
    }
}
