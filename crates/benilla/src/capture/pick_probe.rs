//! `WOW_PICK` — the headless **"what is at this pixel, all the way back"** probe.
//!
//! The flicker instruments (decisions 0653/0656) localise a defect: `benilla-visual hotspot` hands
//! back a pixel box and says *this* is what would not hold still. Naming what is actually there was
//! then a manual step — read the ADT placements, guess which model the report meant, hope. The
//! interactive inspector ([`crate::interact`]) answers it with a cursor, which an unattended probe
//! does not have.
//!
//! So: `WOW_PICK="<x>,<y>[;<x>,<y>…]"` (+ `WOW_PICK_AT=<secs>`, default 20) casts a ray through each
//! pixel and logs **every** hit along it, nearest first — not just the front one.
//!
//! `WOW_PICK_COUNT=<n>` / `WOW_PICK_EVERY=<secs>` (0 = one per frame) repeat the cast, the same shape
//! as the screenshot burst — because the cast honours `Visibility`, a hit that *vanishes* between
//! adjacent casts is a surface being **culled**, not one losing a depth test. That distinction is
//! invisible to a single cast and to the pixels alike: both look like the surface went away.
//!
//! Reporting the whole ray is the point. A surface that swaps with another between frames has a
//! rival *behind* it at nearly the same depth, and the gap between hit 0 and hit 1 is the number
//! that decides the diagnosis: an exact tie is a coplanar authoring tie (no depth precision can
//! break it — only a deterministic order or a bias), while a few millimetres is a precision
//! question. The nearest hit alone cannot tell those apart, and which one it is decides the fix.
//!
//! Coordinates are **screenshot pixels** — the same space `benilla-visual` reports boxes in — so a
//! hotspot box can be pasted straight in. They are divided by the window's scale factor here, since
//! `viewport_to_world` works in logical units and a Retina capture is 2× the logical window.

use bevy::picking::mesh_picking::ray_cast::{MeshRayCast, MeshRayCastSettings, RayCastVisibility};
use bevy::platform::collections::HashSet;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::interact::WorldObject;
use crate::player::WorldCamera;
use crate::terrain::WowModelMaterial;

pub(crate) struct PickProbePlugin;

impl Plugin for PickProbePlugin {
    fn build(&self, app: &mut App) {
        let pixels = std::env::var("WOW_PICK")
            .ok()
            .map(|s| parse_pixels(&s))
            .unwrap_or_default();
        let at = std::env::var("WOW_PICK_AT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20.0);
        let count = std::env::var("WOW_PICK_COUNT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1u32)
            .max(1);
        let every = std::env::var("WOW_PICK_EVERY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);
        if pixels.is_empty() {
            warn!("pick: no usable pixel in WOW_PICK (want e.g. 1600,900) — inert");
        }
        app.insert_resource(PickProbe {
            pixels,
            count,
            every,
            taken: 0,
            next_at: at,
        })
        .add_systems(Update, fire_pick);
    }
}

#[derive(Resource)]
struct PickProbe {
    /// Screenshot-space pixels to cast through.
    pixels: Vec<Vec2>,
    /// Casts to make (`WOW_PICK_COUNT`), `WOW_PICK_EVERY` seconds apart (0 = one per frame).
    count: u32,
    every: f32,
    taken: u32,
    next_at: f32,
}

fn parse_pixels(spec: &str) -> Vec<Vec2> {
    spec.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| {
            let (x, y) = s.split_once(',')?;
            Some(Vec2::new(x.trim().parse().ok()?, y.trim().parse().ok()?))
        })
        .collect()
}

/// What a hit entity is asked for: its identity, the batch class it draws as, and the material
/// carrying the WMO batch order (`Option` because a hit need not be a model batch at all).
type HitIdentity = (
    &'static WorldObject,
    Option<&'static crate::debug_panel::ModelPart>,
    Option<&'static MeshMaterial3d<WowModelMaterial>>,
);

/// Everything needed to turn a hit entity into a line of text — bundled so the system keeps one
/// parameter for "describe this hit" rather than two that must always travel together.
#[derive(bevy::ecs::system::SystemParam)]
struct HitNames<'w, 's> {
    identity: Query<'w, 's, HitIdentity>,
    materials: Res<'w, Assets<WowModelMaterial>>,
}

fn fire_pick(
    mut probe: ResMut<PickProbe>,
    time: Res<Time>,
    window: Query<&Window, With<PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<WorldCamera>>,
    objects: Query<Entity, With<WorldObject>>,
    names: HitNames,
    mut ray_cast: MeshRayCast,
) {
    if probe.taken >= probe.count || probe.pixels.is_empty() || time.elapsed_secs() < probe.next_at
    {
        return;
    }
    let (Ok((camera, cam_tf)), Ok(window)) = (camera.single(), window.single()) else {
        return; // no world camera / window yet — try again next frame
    };
    let cast = probe.taken;
    probe.taken += 1;
    probe.next_at = time.elapsed_secs() + probe.every;
    let scale = window.scale_factor();
    let pickable: HashSet<Entity> = objects.iter().collect();
    for &pixel in &probe.pixels {
        let logical = pixel / scale;
        let Ok(ray) = camera.viewport_to_world(cam_tf, logical) else {
            warn!("pick ({}, {}): outside the viewport", pixel.x, pixel.y);
            continue;
        };
        let filter = |e: Entity| pickable.contains(&e);
        // `never_early_exit` is the whole reason this exists: the default stops at the first hit,
        // and the rival surface behind it is exactly what we came for.
        let settings = MeshRayCastSettings::default()
            .with_visibility(RayCastVisibility::VisibleInView)
            .with_filter(&filter)
            .never_early_exit();
        let hits = ray_cast.cast_ray(ray, &settings);
        info!(
            "pick#{cast} ({}, {}) [logical {:.1}, {:.1}]: {} hits",
            pixel.x,
            pixel.y,
            logical.x,
            logical.y,
            hits.len()
        );
        let mut previous: Option<f32> = None;
        for (i, (entity, hit)) in hits.iter().enumerate() {
            let gap = previous.map_or(String::new(), |p: f32| {
                format!("  (+{:.5} yd behind the last)", hit.distance - p)
            });
            previous = Some(hit.distance);
            let Ok((obj, part, mat)) = names.identity.get(*entity) else {
                info!("  {i:2}  {:9.4} yd  <untagged>{gap}", hit.distance);
                continue;
            };
            // The WMO authored batch order rides in the material's `sun_scale.y` (see
            // `model_render`): 0 means "no bias applied", which for a WMO batch is itself a finding.
            let batch = mat
                .and_then(|m| names.materials.get(&m.0))
                .map(|m| m.extension.sun_scale.y as i32)
                .unwrap_or(-1);
            info!(
                "  {i:2}  {:9.4} yd  {:?} #{:<10} bias {batch:3}  {:?}  {}{gap}",
                hit.distance,
                obj.kind,
                obj.id,
                part.map(|p| p.blend),
                obj.label,
            );
        }
    }
}
