//! The player's WMO-interior claims — the two down-ray identities published each frame:
//!
//! - [`CurrentWmoInterior`] — the RENDER/AUDIO seed's claim (faces + portal crossings,
//!   [`super::seed::down_ray_seeds`]): what room the flood seeds from; consumed by the
//!   interior-audio resolver (`sound::interior`) and the minimap inside-zoom.
//! - [`CurrentAreaInterior`] — the ZONE-TEXT/AREA claim (faces only, [`super::seed::area_down_ray`];
//!   wow-re `zonetext-indoor-bit.md`, the CGLight node's `+0x90` bit 0): drives the leaf-area
//!   override (`terrain_stream`) and the `ZONE_CHANGED` family's indoor election + naming
//!   (`area`). The legs differ exactly where the abbey-yard bug lived — a doorway portal under
//!   the eye seeds the render flood without making you indoors.
//!
//! Split from `wmo_portal/mod.rs` (the portal-cull flood) when the second claim landed; the
//! public faces re-export from there.

use bevy::math::Affine3A;
use bevy::prelude::*;

use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use benilla_assets::{AdtTile, WmoModel};

use super::seed::{area_down_ray, down_ray_claim, down_ray_seeds, DownRayClaim};
use super::{WmoPortalInstance, WmoRoom, WorldCamera, EXTERIOR, EXTERIOR_LIT};
use crate::terrain_stream::{terrain_height_under, PropLobeLight, TerrainStreamer};

/// The WMO interior the camera eye is currently in — the `WMOAreaTable` join keys of the group
/// owning the nearest surface under the eye ([`down_ray_group`], the client's faithful down-ray), or
/// `None` in the open world. Written per frame here; consumed by the interior-audio resolver
/// (`sound::interior`) and any future interior consumer (light, fog, minimap names).
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq)]
pub struct CurrentWmoInterior(pub Option<WmoInteriorKeys>);

/// The **room** the player stands in — the same down-ray claim [`CurrentWmoInterior`] names, but as
/// the placement identity ([`WmoRoom`]) rather than the `WMOAreaTable` join keys. Written by
/// [`track_current_interior`] in the same pass (no second ray).
///
/// Its consumer is the liquid query's scope key (decision 0696): a building's MLIQ pool belongs to
/// that building, so only a subject standing in *that placement* can be in it. Before this, "indoors"
/// was a bare `bool` and every WMO pool on the map answered — the Uldaman entrance read as submerged
/// under a mushroom cave's water 186 yd overhead, in a building the player was 191 yd below.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq)]
pub struct PlayerWmoRoom(pub Option<WmoRoom>);

/// The WMO room **one remote unit** stands in — its own liquid claim, the per-unit twin of
/// [`PlayerWmoRoom`].
///
/// It exists because the liquid query's `None` ("no interior claim for this subject") arm was a
/// standing gap: every consumer that walks *units* rather than the local player — the creature swim
/// marker, the wade splash, the footstep splash slot, the remote-motion depth — passed it, so both
/// sources answered and an ADT lake claimed anything beneath it. In Undercity's Rogues' Quarter the
/// NPCs stood 95 yd under Tirisfal's water at z 32.93 and swam on dry stone, in the same rooms the
/// player (who *did* have a claim) walked normally (decision 0696).
///
/// Re-rayed only when the unit MOVES or a building streams in/out — a room changes at doorways, not
/// per frame — so a town full of standing NPCs costs one position compare each.
#[derive(Component, Clone, Copy, Default, PartialEq)]
pub struct UnitWmoRoom {
    room: Option<WmoRoom>,
    /// Where the claim was last sampled, and the WMO-residency generation then: the re-test gate.
    at: Vec3,
    generation: u32,
}

impl UnitWmoRoom {
    /// The room, for the liquid query's scope key.
    pub fn room(&self) -> Option<WmoRoom> {
        self.room
    }
}

/// `WMOAreaTable` lookup keys: `(WMOID, NameSetID, WMOGroupID)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WmoInteriorKeys {
    pub wmo_id: u32,
    pub name_set: u32,
    pub group_area_id: u32,
}

/// The probe height above the player's feet (yd) for the RENDER/AUDIO seed's down-ray
/// ([`down_ray_seeds`] — chest/head-ish, the listener). The faces-only AREA/light legs do NOT use
/// it: the client casts those from the position itself (`zonetext-indoor-bit.md` (b)), and lifting
/// the origin breaks the terrain race exactly where a hillside is buried ABOVE an interior floor —
/// NSabbey's NW room sits cut into the hill (buried terrain local z ≈ 2.0 vs floor 0.3), so a
/// feet+1.7 origin saw the buried terrain as the nearest surface and read the room as outdoors
/// (director-caught flap, 2026-07-12). A ray from the feet can never hit terrain above its origin.
pub(crate) const INTERIOR_PROBE_HEIGHT: f32 = 1.7;

/// Float-safety lift (yd) for the position-cast legs: feet rest ON the floor plane (physics skin,
/// server-snapped NPC z), and a coplanar `z <= eye.z` test must not lose the floor to a rounding
/// hair. Far below any buried-terrain scale — semantically still "cast from the position".
pub(crate) const POSITION_PROBE_LIFT: f32 = 0.1;

/// The **zone-text/area** indoor claim — the player's CGLight-node twin (wow-re
/// `zonetext-indoor-bit.md`, `[node+0x90]` bit 0): the WMOAreaTable keys of the group owning the
/// nearest **face** under the player, faces-only ([`area_down_ray`] — no portal leg, EXTERIOR
/// groups excluded, terrain race, 1000 yd), or `None` = outdoors. This — not the render/audio
/// seed [`CurrentWmoInterior`] — drives the leaf-area override (`terrain_stream`) and the
/// `ZONE_CHANGED`/`ZONE_CHANGED_INDOORS` election + indoor naming (`area`). The two diverge
/// exactly where the yard bug lived: a doorway portal under the eye seeds the RENDER flood
/// without making you indoors.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq)]
pub struct CurrentAreaInterior(pub Option<WmoInteriorKeys>);

/// Track the player's zone-text indoor claim ([`CurrentAreaInterior`]): per placed building, run
/// the faces-only down-ray from the player's position; first interior claim wins. Camera fallback
/// before login, like [`track_current_interior`].
pub(super) fn track_area_interior(
    wmos: Res<Assets<WmoModel>>,
    player: Res<crate::player::Player>,
    cam: Query<&GlobalTransform, With<WorldCamera>>,
    instances: Query<&WmoPortalInstance>,
    streamer: Res<TerrainStreamer>,
    adt_tiles: Res<Assets<AdtTile>>,
    mut current: ResMut<CurrentAreaInterior>,
) {
    // Cast from the POSITION (float-safety lift only) — the client's recipe (`zonetext-indoor-bit`
    // (b)); a lifted origin sees hillside buried ABOVE an interior floor and misreads outdoors
    // (see [`POSITION_PROBE_LIFT`]).
    let eye_world = if player.active && !player.detached {
        player.pos + Vec3::Y * POSITION_PROBE_LIFT
    } else if let Some(cam_t) = cam.iter().next() {
        cam_t.translation()
    } else {
        return;
    };
    let terrain = terrain_height_under(&streamer, &adt_tiles, eye_world);
    let mut found = None;
    for inst in &instances {
        let Some(model) = wmos.get(&inst.handle) else {
            continue;
        };
        if model.wmo_id == 0 {
            continue;
        }
        let local_from_world = inst.world_from_local.inverse();
        let eye_local = bevy_to_wow(local_from_world.transform_point3(eye_world));
        if !column_in_collision_bounds(model.collision_bounds, eye_local) {
            continue; // no face of this building can own the probe column
        }
        let terrain_local = terrain.map(|z| terrain_z_local(&local_from_world, eye_world, z));
        // The zone-text law: `0x8` alone — a `0x40`-only street group IS "indoors" for area naming
        // (`[node+0x90]` bit 0 is set before the `0x40` test; the light law differs, see
        // [`indoor_verdict_at`]).
        if let Some(gi) = area_down_ray(
            &model.group_collision_tris,
            &model.group_collision_bounds,
            &model.group_nav,
            eye_local,
            terrain_local,
            EXTERIOR,
        ) {
            found = Some(WmoInteriorKeys {
                wmo_id: model.wmo_id,
                name_set: u32::from(inst.name_set),
                group_area_id: model.group_nav.get(gi).map_or(0, |g| g.area_table_id),
            });
            break;
        }
    }
    if current.0 != found {
        current.0 = found;
    }
}

/// Track which WMO interior the **player** is in: run every placed building's down-ray and
/// publish the first interior claim. The probe is the player's position, not the camera — the
/// client's audio listener sits at the character (`SoundListenerAtCharacter` default "1", wow-re
/// `benilla-pins.md` B10) and the interior identity is character state (the minimap zone name),
/// so a camera swinging through a wall must not flap the chapel's ambience (director-caught
/// flicker, 2026-07-03). The camera is the fallback before login / detached free-fly.
/// Independent of the PVS compute (which early-outs on portal-less models — a portal-less hut
/// still has a floor to stand on; the *render* seed stays the camera, as the client's does).
#[allow(clippy::too_many_arguments)] // a Bevy system: each arg is a distinct resource/query
pub(super) fn track_current_interior(
    wmos: Res<Assets<WmoModel>>,
    player: Res<crate::player::Player>,
    cam: Query<&GlobalTransform, With<WorldCamera>>,
    instances: Query<(Entity, &WmoPortalInstance)>,
    streamer: Res<TerrainStreamer>,
    adt_tiles: Res<Assets<AdtTile>>,
    mut current: ResMut<CurrentWmoInterior>,
    mut room: ResMut<PlayerWmoRoom>,
) {
    let eye_world = if player.active && !player.detached {
        player.pos + Vec3::Y * INTERIOR_PROBE_HEIGHT
    } else if let Some(cam_t) = cam.iter().next() {
        cam_t.translation()
    } else {
        return;
    };
    // The terrain leg of the down-ray, sampled once for the eye's column (the client's `GetAreaID`
    // races the same two probes at `0x670345`) — a mine tunnel under a hill must not claim the
    // player standing on the grass above it.
    let terrain = terrain_height_under(&streamer, &adt_tiles, eye_world);
    let mut found = None;
    let mut found_room = None;
    for (entity, inst) in &instances {
        let Some(model) = wmos.get(&inst.handle) else {
            continue;
        };
        if model.wmo_id == 0 {
            continue;
        }
        let local_from_world = inst.world_from_local.inverse();
        let eye_local = bevy_to_wow(local_from_world.transform_point3(eye_world));
        let terrain_local = terrain.map(|z| terrain_z_local(&local_from_world, eye_world, z));
        if let Some(gi) = down_ray_seeds(model, eye_local, terrain_local).in_group {
            found = Some(WmoInteriorKeys {
                wmo_id: model.wmo_id,
                name_set: u32::from(inst.name_set),
                group_area_id: model.group_nav.get(gi).map_or(0, |g| g.area_table_id),
            });
            // The same claim as a placement identity — the liquid query's scope key (0696).
            found_room = Some(WmoRoom {
                instance: entity,
                group: gi as u16,
            });
            break;
        }
    }
    if current.0 != found {
        current.0 = found;
    }
    if room.0 != found_room {
        room.0 = found_room;
    }
}

/// Squared distance (yd²) a unit must move before its room claim is re-rayed. A room changes at a
/// doorway, not continuously, so this is a travel gate rather than a precision one: the claim can
/// lag a transition by at most this distance, which for a swim/submersion verdict is well inside the
/// wade band. Deliberately coarser than `interior::RESAMPLE_DIST_SQ` (the light classifier's ~0,
/// which must not quantize a *continuous* MOCV field into steps — a room index has no such field).
const UNIT_ROOM_RESAMPLE_DIST_SQ: f32 = 0.25 * 0.25;

/// Maintain [`UnitWmoRoom`] on every net entity: the room its position stands in, from the same
/// faces-only down-ray the zone-text claim uses ([`area_down_ray`], EXTERIOR `0x8` excluded, terrain
/// raced) cast from the unit's own position.
///
/// The leg is the **position-cast faces** one, not the render seed's lifted eye + portal race: the
/// client casts its position legs from the position itself (`zonetext-indoor-bit.md` (b)), and a unit
/// is a position, not a camera. Which leg the reference's *own* per-unit liquid depth uses — and
/// whether it scopes to the containing GROUP rather than the placement — is the open half of the
/// wow-re dispatch 0696 re-asked; this is the claim that makes the query *scoped at all*, and it is
/// re-pointable at one call site when that lands.
pub(super) fn track_unit_interiors(
    mut commands: Commands,
    wmos: Res<Assets<WmoModel>>,
    instances: Query<(Entity, &WmoPortalInstance)>,
    streamer: Res<TerrainStreamer>,
    adt_tiles: Res<Assets<AdtTile>>,
    residency: Res<crate::interior::WmoResidency>,
    mut units: Query<
        (Entity, &GlobalTransform, Option<&mut UnitWmoRoom>),
        With<crate::net::NetEntity>,
    >,
) {
    let generation = residency.generation();
    for (entity, transform, claim) in &mut units {
        // World position, not the local one: a mounted unit's `Transform` is seat-relative
        // (0441), and its room is decided where it actually stands.
        let pos = transform.translation();
        // The re-test gate: a settled unit in an unchanged world keeps its room for free. A
        // building streaming in UNDER a standing NPC must still re-claim it — that is the whole
        // point of the generation leg, and without it a creature spawned before its own dungeon
        // would hold an "outdoors" claim and read the lake overhead forever.
        if let Some(claim) = claim.as_ref() {
            if claim.generation == generation
                && pos.distance_squared(claim.at) < UNIT_ROOM_RESAMPLE_DIST_SQ
            {
                continue;
            }
        }
        let room = room_at(&wmos, &instances, &streamer, &adt_tiles, pos);
        let next = UnitWmoRoom {
            room,
            at: pos,
            generation,
        };
        match claim {
            Some(mut claim) => {
                if *claim != next {
                    *claim = next;
                }
            }
            // `try_insert`: a unit despawning this frame (the net teardown) applies before this
            // command, and the insert must not panic at apply time.
            None => {
                commands.entity(entity).try_insert(next);
            }
        }
    }
}

/// The WMO room a world position stands in — the placement + group of the nearest **face** under it,
/// faces-only ([`area_down_ray`], EXTERIOR `0x8` excluded, terrain raced), first claim wins.
///
/// The shared body of the per-unit claim; the same shape as [`track_area_interior`]'s loop, returning
/// the placement identity instead of the `WMOAreaTable` keys.
fn room_at(
    wmos: &Assets<WmoModel>,
    instances: &Query<(Entity, &WmoPortalInstance)>,
    streamer: &TerrainStreamer,
    adt_tiles: &Assets<AdtTile>,
    feet_world: Vec3,
) -> Option<WmoRoom> {
    let probe_world = feet_world + Vec3::Y * POSITION_PROBE_LIFT;
    let terrain = terrain_height_under(streamer, adt_tiles, probe_world);
    for (entity, inst) in instances {
        let Some(model) = wmos.get(&inst.handle) else {
            continue;
        };
        if model.wmo_id == 0 {
            continue; // the same placement set the player's own claim walks
        }
        let local_from_world = inst.world_from_local.inverse();
        let probe_local = bevy_to_wow(local_from_world.transform_point3(probe_world));
        if !column_in_collision_bounds(model.collision_bounds, probe_local) {
            continue; // no face of this building can own the probe column
        }
        let terrain_local = terrain.map(|z| terrain_z_local(&local_from_world, probe_world, z));
        if let Some(gi) = area_down_ray(
            &model.group_collision_tris,
            &model.group_collision_bounds,
            &model.group_nav,
            probe_local,
            terrain_local,
            EXTERIOR,
        ) {
            return Some(WmoRoom {
                instance: entity,
                group: gi as u16,
            });
        }
    }
    None
}

/// Whether a world position stands INDOORS **for the unit LIGHT law** — the nearest surface under
/// it (cast from the position itself, [`POSITION_PROBE_LIFT`]) is a face of some placed WMO group
/// whose flags carry neither EXTERIOR (`0x8`) nor EXTERIOR_LIT (`0x40`) ([`area_down_ray`], faces
/// only, terrain-raced). This is the ENTITY light classifier's predicate (`crate::interior`): the
/// client's classify `0x6a87f0` forks the node's LIGHTING class on `MOGI & 0x48` — both bits →
/// the exterior leg (`or [node+0xc],0x4`: sun diffuse, MCSH 2.5/0.5 intensity, scene fog; wow-re
/// `m2-interior-doodad-base-light.md`, `unit-m2-shader-light.md`) — which DIVERGES from the
/// zone-text indoor bit (`0x8` alone) exactly on the `0x40`-only city street groups: Stormwind's
/// pavement and Orgrimmar's valleys are "indoors" for area naming yet sunlit (decision 0475; keying
/// the light off the zone-text bit flat-lit every unit in those cities). The face geometry is
/// robust exactly where group BOUNDING BOXES are not — a room's box bottom can float above its own
/// walkable floor when the floor polys belong to a neighbouring group (NSabbey group 3: box min z
/// 1.84 vs floor ≈0.3), which split a standing body's feet-level parts onto the exterior law
/// (director-caught, 2026-07-12). Unlike the AREA claim ([`track_area_interior`]), a WMO without a
/// `WMOAreaTable` identity still counts — lighting is about the room's geometry, not its name.
pub(crate) fn indoors_at<'a>(
    wmos: &Assets<WmoModel>,
    instances: impl IntoIterator<Item = &'a WmoPortalInstance>,
    streamer: &TerrainStreamer,
    adt_tiles: &Assets<AdtTile>,
    feet_world: Vec3,
) -> bool {
    matches!(
        indoor_verdict_at(wmos, instances, streamer, adt_tiles, feet_world),
        IndoorVerdict::DayNight | IndoorVerdict::Baked { .. }
    )
}

/// A position's resolved interior LIGHT verdict — [`indoors_at`] plus, for the GameObject lane,
/// the footprint bake ([`crate::interior`] folds it into the object's SH probe).
pub(crate) enum IndoorVerdict {
    /// No WMO face claim under the position — standing on/over open terrain: the exterior law
    /// with the MCSH-driven 2.5/0.5 intensity.
    Outdoors,
    /// The nearest claim is an outdoor-class WMO face (`MOGI & 0x48` — a street, deck, porch):
    /// the exterior law at the FORCED-LIT 2.5 target — the terrain MCSH beneath the building is
    /// NOT sampled. Byte-verified (0477 interim → 0480 confirmed; wow-re `unit-wmo-mcsh-gate.md`,
    /// the §5 that CORRECTED `unit-light-combine-storm.md` a4): the shared down-ray attach
    /// `0x6a8a20` sets skip-shadow `[node+0xd]|=0x2` on its WMO branch for EVERY node subclass
    /// (`0x6a8bc7` — a4's "doodad-only" reading was wrong) and clears it on the terrain branch;
    /// the exterior intensity leg then commits constant 2.5 when set, samples MCSH only when
    /// clear. The director's Booty Bay A/B (deck NPCs sunlit over shadowed buried terrain) was
    /// this gate, not a decode error.
    OutdoorsOnWmo,
    /// Indoors, but the footprint lane yields the day/night pair instead of a bake: the render-mesh
    /// ray missed every MOCV face, or the hit face's MOPY bit 0x1 selects the exterior colours
    /// (wow-re `m2-interior-doodad-base-light` §11's binary selector). The plain matte variant.
    DayNight,
    /// Indoors over baked floor: the hit's barycentric MOCV bytes (the floor-168/cap-96 inputs)
    /// plus the hit group's MOLR lobes in world (Bevy) space, colour × intensity.
    Baked {
        mocv: [u8; 3],
        lobes: Vec<PropLobeLight>,
    },
}

/// Resolve a position's interior light verdict: the [`down_ray_claim`] classify (collision faces,
/// the `0x48` LIGHTING-class law — decision 0475, terrain race) runs per placement and the
/// **nearest claim across placements wins** — the client's one global nearest-hit (a unit inside a
/// Booty Bay hut sits in the same column as the city deck 30 yd below; the hut's floor must own
/// the verdict regardless of instance iteration order). An outdoor-class winner is
/// [`IndoorVerdict::OutdoorsOnWmo`]; an interior winner runs the FOOTPRINT ray on the same
/// placement's render mesh + baked MOCV (the entity node's env-update attach `0x6717d0` →
/// `0x69e4c0` — wow-re `wmo-lit-selector.md`, trace-decisive on the abbey INNBENCH draws). The
/// MOLR gate is the FOOTPRINT hit's group — the group whose bake the sample took (the byte
/// plumbing between the two rays is OPEN; the abbey benches satisfy both readings).
pub(crate) fn indoor_verdict_at<'a>(
    wmos: &Assets<WmoModel>,
    instances: impl IntoIterator<Item = &'a WmoPortalInstance>,
    streamer: &TerrainStreamer,
    adt_tiles: &Assets<AdtTile>,
    feet_world: Vec3,
) -> IndoorVerdict {
    let probe_world = feet_world + Vec3::Y * POSITION_PROBE_LIFT;
    let terrain = terrain_height_under(streamer, adt_tiles, probe_world);
    let mut best: Option<(DownRayClaim, &WmoModel, &WmoPortalInstance, [f32; 3])> = None;
    for inst in instances {
        let Some(model) = wmos.get(&inst.handle) else {
            continue;
        };
        let local_from_world = inst.world_from_local.inverse();
        let probe_local = bevy_to_wow(local_from_world.transform_point3(probe_world));
        if !column_in_collision_bounds(model.collision_bounds, probe_local) {
            continue; // no face of this building can own the probe column
        }
        let terrain_local = terrain.map(|z| terrain_z_local(&local_from_world, probe_world, z));
        let Some(claim) = down_ray_claim(
            &model.group_collision_tris,
            &model.group_collision_bounds,
            &model.group_nav,
            probe_local,
            terrain_local,
            EXTERIOR | EXTERIOR_LIT,
        ) else {
            continue;
        };
        if best.as_ref().is_none_or(|(b, ..)| claim.depth < b.depth) {
            best = Some((claim, model, inst, probe_local));
        }
    }
    let Some((claim, model, inst, probe_local)) = best else {
        return IndoorVerdict::Outdoors;
    };
    if claim.outdoor {
        return IndoorVerdict::OutdoorsOnWmo;
    }
    // Indoors — run the footprint sample on the winning placement's render mesh.
    let Some((gi, mocv, mopy_daynight)) = footprint_sample(model, probe_local) else {
        return IndoorVerdict::DayNight;
    };
    if mopy_daynight {
        return IndoorVerdict::DayNight;
    }
    let lobes = model
        .group_light_refs
        .get(gi)
        .into_iter()
        .flatten()
        .filter_map(|&li| model.lights.get(li as usize))
        .filter(|l| l.is_omni())
        .map(|l| PropLobeLight {
            pos: inst
                .world_from_local
                .transform_point3(wow_to_bevy(l.position)),
            color_i: l.color.map(|c| c * l.intensity.max(0.0)),
            atten_start: l.attenuation_start,
            atten_end: l.attenuation_end,
        })
        .collect();
    IndoorVerdict::Baked { mocv, lobes }
}

/// The footprint down-ray: the nearest render-mesh face below `probe_local` across the model's
/// interior groups' [`FootprintTris`] — its group index, the hit's barycentric MOCV (rounded to
/// bytes: the reference's `0x6a77e0` consumes a byte colour), and whether the face's MOPY bit 0x1
/// selects the day/night lane. The candidate set is pre-filtered at parse (COLLISION/VISITED faces
/// rejected, the client's BSP-walk mask 0x88 — `wmo_group_footprint_tris`); an exact-distance tie
/// keeps the LATER face, the client's inclusive `test ah,0x41; jp` compare (wow-re
/// `unit-light-combine-storm.md` — its BSP traversal order is not modelled here, so which face is
/// "later" can differ; after the 0x88 filter no observed floor still carries coplanar ties).
pub(super) fn footprint_sample(
    model: &WmoModel,
    probe_local: [f32; 3],
) -> Option<(usize, [u8; 3], bool)> {
    let (px, py, pz) = (probe_local[0], probe_local[1], probe_local[2]);
    let mut best: Option<(f32, usize, [f32; 3], bool)> = None;
    for (gi, fp) in model.group_footprints.iter().enumerate() {
        let Some(fp) = fp else { continue };
        // Broad phase on the load-derived face bounds (the 0330 rule — from the faces, never an
        // authored box, so the cull is exact): a down-ray at (px, py) can hit no face of a group
        // whose XY face bounds exclude the column, and no face whose LOWEST vertex is already
        // above the probe (interpolated z ≥ the face set's min z). Without this every re-rayed
        // entity scanned every interior group of the whole model — the Stormwind live-frame cost
        // (decision 0364).
        if let Some(Some((min, max))) = model.group_footprint_bounds.get(gi) {
            if px < min[0] || px > max[0] || py < min[1] || py > max[1] || min[2] > pz {
                continue;
            }
        }
        for (ti, tri) in fp.indices.chunks_exact(3).enumerate() {
            let (Some(&a), Some(&b), Some(&c)) = (
                fp.positions.get(tri[0] as usize),
                fp.positions.get(tri[1] as usize),
                fp.positions.get(tri[2] as usize),
            ) else {
                continue;
            };
            // 2D barycentric in the down-ray's projection (x/y), inclusive edges.
            let det = (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1]);
            if det.abs() < 1e-9 {
                continue;
            }
            let wb = ((px - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (py - a[1])) / det;
            let wc = ((b[0] - a[0]) * (py - a[1]) - (px - a[0]) * (b[1] - a[1])) / det;
            let wa = 1.0 - wb - wc;
            if wa < 0.0 || wb < 0.0 || wc < 0.0 {
                continue;
            }
            let z = wa * a[2] + wb * b[2] + wc * c[2];
            if z > pz || best.is_some_and(|(bz, ..)| z < bz) {
                continue;
            }
            let (Some(ca), Some(cb), Some(cc)) = (
                fp.mocv.get(tri[0] as usize),
                fp.mocv.get(tri[1] as usize),
                fp.mocv.get(tri[2] as usize),
            ) else {
                continue;
            };
            let mocv = [
                wa * f32::from(ca[0]) + wb * f32::from(cb[0]) + wc * f32::from(cc[0]),
                wa * f32::from(ca[1]) + wb * f32::from(cb[1]) + wc * f32::from(cc[1]),
                wa * f32::from(ca[2]) + wb * f32::from(cb[2]) + wc * f32::from(cc[2]),
            ];
            let mopy = fp.mopy_flags.get(ti).is_some_and(|f| f & 0x1 != 0);
            best = Some((z, gi, mocv, mopy));
        }
    }
    best.map(|(_, gi, mocv, mopy)| (gi, mocv.map(|v| v.round().clamp(0.0, 255.0) as u8), mopy))
}

/// Whether a probe column (model-local WoW coords) can hit any of a building's collision faces:
/// inside the whole-model face AABB ([`WmoModel::collision_bounds`]) in XY, with faces reaching
/// at/below the probe. Exact-conservative for the faces-only down-ray — `floor_z_at` accepts only
/// inside a triangle's XY projection at z ≥ the set's min — so a skipped instance could never have
/// answered. This is what keeps a camera in open country at one AABB test per resident building
/// instead of a full face scan (the 2026-07-12 fps regression).
fn column_in_collision_bounds(bounds: Option<([f32; 3], [f32; 3])>, probe: [f32; 3]) -> bool {
    bounds.is_some_and(|(min, max)| {
        probe[0] >= min[0]
            && probe[0] <= max[0]
            && probe[1] >= min[1]
            && probe[1] <= max[1]
            && min[2] <= probe[2]
    })
}

/// The terrain surface under `eye_world` (given as raw WoW `z`), expressed in a placement's model-space
/// `z` — the frame [`down_ray_pick`](seed::down_ray_pick) races in. The surface point sits in the eye's
/// own column, so it maps straight through the inverse placement: MODF placements carry yaw only, and
/// under a yaw a world-vertical column is a model-vertical one. (A pitched/rolled placement would tilt
/// the down-ray itself, an approximation the whole probe already makes.)
pub(crate) fn terrain_z_local(
    local_from_world: &Affine3A,
    eye_world: Vec3,
    terrain_wow_z: f32,
) -> f32 {
    let eye_wow = bevy_to_wow(eye_world);
    let surface = wow_to_bevy([eye_wow[0], eye_wow[1], terrain_wow_z]);
    bevy_to_wow(local_from_world.transform_point3(surface))[2]
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_assets::footprint_tri_bounds;
    use benilla_formats::FootprintTris;

    /// A model with nothing but footprint groups — the sample under test reads only those.
    fn bare_model() -> WmoModel {
        WmoModel {
            wmo_id: 0,
            submeshes: Vec::new(),
            submesh_group: Vec::new(),
            portal_vertices: Vec::new(),
            portal_infos: Vec::new(),
            portal_refs: Vec::new(),
            group_nav: Vec::new(),
            fogs: Vec::new(),
            group_collision_tris: Vec::new(),
            group_camera_only_tris: Vec::new(),
            group_collision_bounds: Vec::new(),
            collision_bounds: None,
            collision: None,
            collision_camera: None,
            doodads: Vec::new(),
            doodad_sets: Vec::new(),
            lights: Vec::new(),
            group_bounds: Vec::new(),
            group_footprints: Vec::new(),
            group_footprint_bounds: Vec::new(),
            group_light_refs: Vec::new(),
            group_liquids: Vec::new(),
            doodad_base: Default::default(),
            doodad_owner: Default::default(),
            doodad_groups: Default::default(),
        }
    }

    /// Two single-face footprint groups: group 0 owns the floor under the probe (z = 0), group 1
    /// sits far away in XY with a HIGHER face (z = 5) that would shadow group 0's hit if a broken
    /// cull ever let its column test slip.
    fn two_group_footprints() -> Vec<Option<FootprintTris>> {
        let face = |offset: [f32; 3]| FootprintTris {
            positions: vec![
                [offset[0], offset[1], offset[2]],
                [offset[0] + 10.0, offset[1], offset[2]],
                [offset[0], offset[1] + 10.0, offset[2]],
            ],
            indices: vec![0, 1, 2],
            mocv: vec![[10, 20, 30]; 3],
            mopy_flags: vec![0],
        };
        vec![
            Some(face([-5.0, -5.0, 0.0])),
            Some(face([100.0, 100.0, 5.0])),
        ]
    }

    /// The broad phase is a pure skip, never a re-ranking: the verdict with load-derived face
    /// bounds equals the verdict with NO bounds at all (the fixtures' `Vec::new()` — every group
    /// scanned). Pins the 0364 footprint cull's exactness contract, the 0330 argument one lane
    /// over: a column outside a group's face bounds could never have answered.
    #[test]
    fn footprint_bounds_cull_is_exact() {
        let mut model = bare_model();
        model.group_footprints = two_group_footprints();
        // Probe over group 0's face, above it. Unculled baseline first (no bounds).
        let probe = [-1.0, -1.0, 2.0];
        let unculled = footprint_sample(&model, probe);
        assert_eq!(unculled, Some((0, [10, 20, 30], false)));
        model.group_footprint_bounds = footprint_tri_bounds(&model.group_footprints);
        assert_eq!(footprint_sample(&model, probe), unculled);
        // Between the groups: neither owns the column — both agree on a miss.
        model.group_footprint_bounds = Vec::new();
        assert_eq!(footprint_sample(&model, [50.0, 50.0, 2.0]), None);
        model.group_footprint_bounds = footprint_tri_bounds(&model.group_footprints);
        assert_eq!(footprint_sample(&model, [50.0, 50.0, 2.0]), None);
        // Below every face: the bounds' min-z leg and the scan agree on a miss.
        model.group_footprint_bounds = Vec::new();
        assert_eq!(footprint_sample(&model, [-1.0, -1.0, -1.0]), None);
        model.group_footprint_bounds = footprint_tri_bounds(&model.group_footprints);
        assert_eq!(footprint_sample(&model, [-1.0, -1.0, -1.0]), None);
    }

    /// The derived bounds cover only vertices a face actually references — an orphan vertex
    /// (parsed but unindexed) must not inflate the cull box.
    #[test]
    fn footprint_bounds_ignore_unreferenced_vertices() {
        let mut fps = two_group_footprints();
        if let Some(fp) = fps[0].as_mut() {
            fp.positions.push([1000.0, 1000.0, 1000.0]); // orphan
        }
        let bounds = footprint_tri_bounds(&fps);
        let (min, max) = bounds[0].expect("group 0 has faces");
        assert!(max[0] <= 5.0 + 1e-6 && max[1] <= 5.0 + 1e-6);
        assert_eq!(min[2], 0.0);
    }
}
