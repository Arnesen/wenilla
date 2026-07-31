//! Per-model light/particle dressing for placed models: the shared [`point_light`] recipe and the
//! world-baked spawners the terrain path rides (M2/WMO point lights, particle emitters, ribbon
//! trails). A transport prop takes the entity-owned shapes instead (`crate::entities`' `wmo_props`,
//! decision 0474) — riding owners and prop-local children, never a world bake.

use benilla_assets::coords::wow_to_bevy;
use benilla_assets::{ModelEmitter, ModelLight};
use benilla_formats::WmoLight;
use bevy::prelude::*;

use crate::particles;

/// Bevy `PointLight` intensity (lumens) for an M2 light of unit `diffuse_intensity`. Bevy stores the GPU
/// colour as `linear_color × intensity/(4π)` (bevy_pbr light.rs), so `4π` makes the shader read exactly
/// `diffuse_color × diffuse_intensity` — a clean, predictable base. `wow_model.wgsl` then applies the
/// faithful WoW falloff `1/(0.7d+0.03d²)` (decision 0016). There is no gain dial any
/// more: the 0273 `sh_c16.w` gain is retired and the commit is the reference's raw product
/// (`global_light::commit_raw`).
const POINT_LIGHT_INTENSITY: f32 = 4.0 * std::f32::consts::PI;
/// The packed per-light `range` (yd) — since 0285 this is the ≤3-nearest **selection-candidacy**
/// radius around the receiving unit's anchor, not an evaluation cutoff (a committed GL light has
/// none; the shader evaluates the selected lights at any distance, and att(48 yd) ≈ 0.01 is already
/// invisible). Generous so a hall-scale WMO group whose center sits ~20 yd from its wall fixtures
/// still gathers them — erring wide only ever admits negligible candidates. Also Bevy's clustering
/// range for the `PointLight` itself, which our shaders don't consume.
const POINT_LIGHT_RANGE: f32 = 48.0;

/// The one `PointLight` recipe for an authored WoW light source (M2 or WMO MOLT). The PointLight only
/// carries position + colour into the shared packed table; every lit surface — terrain, M2
/// doodads/NPCs, and WMO walls/floors (decision 0273: the reference lights them all) — then selects
/// its unit's ≤3 NEAREST from that table and applies the faithful falloff in-shader (decision 0285;
/// the committed light is diffuse-only). `color` is the authored WoW RGB (linear, hue preserved);
/// `intensity_scale` folds in the per-light authored intensity.
///
/// `pub(crate)`: the terrain path spawns it at a world-baked point below; a transport prop's light
/// ([`crate::entities`]'s `wmo_props`) spawns it as a CHILD of the moving gameobject instead, so
/// propagation carries the source with the hull.
pub(crate) fn point_light(color: [f32; 3], intensity_scale: f32) -> PointLight {
    PointLight {
        color: Color::linear_rgb(color[0], color[1], color[2]),
        intensity: POINT_LIGHT_INTENSITY * intensity_scale.max(0.0),
        range: POINT_LIGHT_RANGE,
        shadows_enabled: false,
        ..default()
    }
}

/// Spawn one Bevy `PointLight` at `world` (Bevy space), appended to `out` so it rides the placement's
/// lifecycle. The light recipe is [`point_light`].
fn spawn_point_light(
    commands: &mut Commands,
    world: Vec3,
    color: [f32; 3],
    intensity_scale: f32,
    out: &mut Vec<Entity>,
) {
    let e = commands
        .spawn((
            point_light(color, intensity_scale),
            Transform::from_translation(world),
        ))
        .id();
    out.push(e);
}

/// Spawn a `PointLight` for each casting `type==1` (point) **M2** light of a prop at `transform` (the
/// campfire/torch/brazier/lantern sources). Position is M2 model space at rest (bone matrix identity at
/// rest), placed via the prop's transform like the emitters — a placed prop never animates its light
/// bone, so the rest pose is exact here (the entity path, whose bones DO move, rides joints instead).
pub(super) fn spawn_lights_for(
    commands: &mut Commands,
    lights: &[ModelLight],
    transform: Transform,
    out: &mut Vec<Entity>,
) {
    for l in lights.iter().map(|l| &l.def) {
        if !l.casts() {
            continue; // directional lights feed an ambient term; a static `0` visibility key is dark
        }
        let world = transform.transform_point(wow_to_bevy(l.position));
        spawn_point_light(commands, world, l.diffuse_color, l.diffuse_intensity, out);
    }
}

/// Spawn a `PointLight` for each omni (`type==0`) **WMO MOLT** light at the building's `transform` (the
/// interior fixture sources — forge fire, inn fireplaces, chapel candles/chandeliers). These radiate onto
/// the NPCs/doodads near each fixture and onto the building's own walls/floor (decision 0273). Position
/// is WMO model space. The MOLT curve is the same fixed WoW falloff as the M2 path — byte-verified
/// (wow-re wmo-molt-runtime: the disk attenStart/End do not shape the GL falloff) and confirmed live in
/// the reference GL trace (committed c/l/q = 0/0.7/0.03, diffuse = colour × intensity, ambient 0).
pub(super) fn spawn_wmo_lights_for(
    commands: &mut Commands,
    lights: &[WmoLight],
    transform: Transform,
    out: &mut Vec<Entity>,
) {
    for l in lights {
        if !l.is_omni() {
            continue; // spot/directional/ambient MOLT types are not point sources
        }
        let world = transform.transform_point(wow_to_bevy(l.position));
        spawn_point_light(commands, world, l.color, l.intensity, out);
    }
}

/// Spawn a particle-emitter entity for each of a model's emitters at `transform`, appending them to
/// `out` so they ride the placement's lifecycle (despawn when it streams out). Emitters with no
/// resolved texture are skipped by [`particles::spawn_emitter`].
///
/// `joints` is the placement's anim-host joint set when it animates (0130 phase 4): each emitter then
/// rides its host bone's joint — the torch flame follows the flame-bone jiggle, the inn chandelier's
/// candle flames swing with it. Riding is unconditional given a joint set: for a bone whose chain
/// never animates the joint holds its telescoped rest pivot, so `joint · (position − pivot)` is
/// byte-for-byte the static `placement · position` — no per-emitter classification needed.
///
/// `armed_seq` is the FILE sequence slot this placement's own arm rolled, and the emitters sample
/// their rate/gate tracks against **that** slot. This site used to pass `EmitClock::Pinned` — slot 0,
/// unconditionally — on the reasoning that a placed doodad arms once, so slot 0 is all it can ever
/// play. The arm has rolled a frequency-weighted *variation* since 0130, so that stopped being true
/// and nothing noticed: an emitter whose rate is keyed only in a later variation sampled a flat zero
/// for ever and emitted nothing, on every placement, while still building, pooling and ticking.
/// `benilla-extract partslotscan` counts **947** such emitters across **184** models; the one that
/// surfaced it is the Blasted Lands lightning strike, whose whole burst lives in slot 1
/// (decision 0760, bug B63).
#[allow(clippy::too_many_arguments)] // a spawn plumbing fn: every arg is a distinct resource
pub(super) fn spawn_emitters_for(
    commands: &mut Commands,
    emitters: &[ModelEmitter],
    transform: Transform,
    joints: Option<&[Entity]>,
    arm: Option<Entity>,
    fade: (f32, Vec3),
    // The WMO placement this model's doodad set belongs to (`None` for an ADT map doodad) — the
    // exterior-window exemption for a prop of the building the camera is standing in (0786).
    instance: Option<Entity>,
    out: &mut Vec<Entity>,
) {
    // The owner doodad's fade sphere (radius, local centre) → the emitter's draw-set gate
    // ([`particles::EmitterFade`]): world centre = the placement applied to the bbox centre, the
    // same point the doodad's own visibility gate measures to.
    let (fade_radius, fade_center) = fade;
    let world_center = transform.transform_point(fade_center);
    for em in emitters {
        // Static placements (no anim host — the ~90% tier, and every capture) have no owner to
        // follow; they despawn via the placement's entity list.
        let owner = joints
            .and_then(|js| js.get(em.def.bone as usize))
            .map(|&j| (j, em.bone_pivot));
        if let Some(e) = particles::spawn_emitter(
            commands,
            em,
            transform,
            particles::EmitterFrames {
                owner,
                attach: None, // placed doodads/props are never attached models
                anchor: None, // anchor at the placement: an animated bone never drags the cloud
                ..default()
            },
            // A placed doodad's arm is NOT one-time: it re-rolls its variation every play-window
            // (decision 0768), so the emitters read the slot AND the clip time off the host's live
            // player each frame — the same lane units and GameObjects use, and what the reference's
            // `m2_animate` does for every model without distinction. A pinned slot was right only
            // under the superseded "armed once at load" contract.
            match arm {
                Some(root) => particles::EmitClock::Host(root),
                None => particles::EmitClock::Pinned,
            },
        ) {
            commands.entity(e).insert(particles::EmitterFade {
                radius: fade_radius,
                center: world_center,
                instance,
            });
            out.push(e);
        }
    }
}

/// Spawn a ribbon trail for each of a model's ribbon emitters (wisp streamers, the Caverns-of-Time
/// energy trails). A trail rides its host bone's joint when the placement animates (same ride as
/// [`spawn_emitters_for`]); a static placement's trail rides the first spawned submesh entity (its
/// transform IS the placement). Trails self-despawn when their owner goes, so they don't join
/// `out` — the placement's despawn cascades within a frame.
pub(super) fn spawn_ribbons_for(
    commands: &mut Commands,
    ribbons: &[benilla_assets::ModelRibbon],
    transform: Transform,
    joints: Option<&[Entity]>,
    fallback_owner: Option<Entity>,
) {
    for rb in ribbons {
        let (owner, use_pivot) = match joints.and_then(|js| js.get(rb.def.bone as usize)).copied() {
            Some(j) => (j, true),
            None => match fallback_owner {
                Some(e) => (e, false),
                None => continue, // nothing to ride (an entity-less placement)
            },
        };
        // Placed doodads carry only always-on trails (wisp streamers, banner ribbons) — no
        // per-sequence visibility gate (that's the thrown weapon's InFlight keying), so the anim
        // is immaterial here.
        crate::ribbons::spawn_ribbon(
            commands,
            rb,
            owner,
            use_pivot,
            transform.scale.max_element(),
            None,
            // No model-alpha source: a placed prop / effect instance is always drawn (0827).
            None,
        );
    }
}
