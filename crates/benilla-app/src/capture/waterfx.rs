//! The `waterfx` capture viewer — the foam **instrument** (decision 0022: see it before tuning
//! it). A server-less synthetic rig: one dummy wading unit over a synthetic water footprint
//! (a real 4.1667-yd wet-cell lattice, so patch building and bank clipping run for real) with a
//! flat backdrop for contrast, driven through the NORMAL emitter path — nothing here bypasses the
//! shipped systems; the rig only supplies a unit, water, and motion.
//!
//! `WOW_CAPTURE=waterfx` + knobs: `WOW_WFX_MODE` (`ring`|`wake`|`turn`), `WOW_WFX_SPEED` (yd/s),
//! `WOW_WFX_AGE` (s of motion before the shot), `WOW_WFX_DEPTH` (yd below the surface; > ~0.8
//! also exercises the step-in one-shot), camera `WOW_WFX_AZ`/`EL`/`DIST`. Not a golden scenario —
//! output depends on the knobs.

use bevy::prelude::*;

use benilla_assets::coords::wow_to_bevy;
use benilla_protocol::EntityKind;

use super::FxViewState;
use crate::net::NetEntity;
use benilla_world::liquid::{FoamPatch, WaterChunkInfo};

/// Which foam behaviour the viewer exercises.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum WfxMode {
    /// Standing unit → the pulsing RING.
    Ring,
    /// Unit translating in WoW +X → the trailing WAKE (ends at the rig centre).
    Wake,
    /// Unit turning in place → full-size RINGs (the `& 0x30` state).
    Turn,
}

/// The `waterfx` viewer request (built by [`crate::capture`] from env knobs). Its presence turns
/// on the rig systems below.
#[derive(Resource)]
pub(crate) struct WaterFxView {
    pub(crate) mode: WfxMode,
    /// Translation speed for [`WfxMode::Wake`] (yd/s, WoW +X).
    pub(crate) speed: f32,
    /// Seconds the unit moves/stands before the shot (foam accumulates).
    pub(crate) age: f32,
    /// Rig centre in raw WoW coords `(x, y, surface_z)` — where the unit ends up at shot time.
    pub(crate) center: [f32; 3],
    /// Feet depth below the surface (yd) — must land inside the wading gate.
    pub(crate) depth: f32,
}

/// Marks the rig's entities (spawn-once guard).
#[derive(Component)]
pub(crate) struct WaterFxDummy;

/// The MCLQ wet-cell edge (yd) — the synthetic lattice mirrors the real liquid granularity.
const CELL: f32 = 33.333_332 / 8.0;

/// Once the capture harness arms the scene, stand up the rig: a dark backdrop plane just under
/// the surface, a synthetic wet-cell lattice (12×12 cells ≈ 50 yd square), and one wading dummy.
/// Sets [`FxViewState::attached_at`] as the age-clock zero.
pub(crate) fn spawn(
    mut commands: Commands,
    view: Option<Res<WaterFxView>>,
    state: Option<ResMut<FxViewState>>,
    existing: Query<(), With<WaterFxDummy>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    time: Res<Time>,
) {
    let (Some(view), Some(mut state)) = (view, state) else {
        return;
    };
    if !state.armed || !existing.is_empty() {
        return;
    }
    let [cx, cy, surf] = view.center;

    // A big flat mid-gray "water body" backdrop 0.15 yd under the surface, so the additive foam
    // reads against a stable tone (the real liquid shader is beside the point here).
    commands.spawn((
        WaterFxDummy,
        Mesh3d(meshes.add(Plane3d::default().mesh().size(120.0, 120.0).build())),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.16, 0.22, 0.26),
            unlit: true,
            ..default()
        })),
        Transform::from_translation(wow_to_bevy([cx, cy, surf - 0.15])),
    ));

    // The synthetic water chunk: a 12×12 grid of wet cells centred on the rig, flat at the surface
    // — the same components the terrain streamer attaches to real liquid, so the foam emitter and
    // patch builder run the shipped path.
    let n = 12;
    let half = n as f32 * CELL * 0.5;
    let (x0, y0) = (cx - half, cy - half);
    let mut positions = Vec::new();
    for iy in 0..=n {
        for ix in 0..=n {
            positions.push([x0 + ix as f32 * CELL, y0 + iy as f32 * CELL, surf]);
        }
    }
    commands.spawn((
        WaterFxDummy,
        WaterChunkInfo::new(
            // The fixture stands in for an outdoor lake: ADT-sourced still water, so the
            // `liquid_at` delegation answers it for an outdoors subject.
            benilla_world::liquid::LiquidSource::AdtChunk,
            benilla_formats::LiquidKind::Still,
            [n + 1, n + 1],
            positions,
            vec![true; n * n],
        ),
        FoamPatch,
        Transform::IDENTITY,
    ));

    // The wading dummy: a plain streamed-unit shape (no display id, visual pre-attached so the
    // entity subsystem never gives it a fallback cube over the foam).
    let start_x = if view.mode == WfxMode::Wake {
        cx - view.speed * view.age
    } else {
        cx
    };
    commands.spawn((
        WaterFxDummy,
        NetEntity {
            kind: EntityKind::Unit,
            display_id: None,
            scale: 1.0,
        },
        // Stated at the spawn, not left to `entities::publish_world_units`: the reconciler runs
        // between the wire drain and the rest of the frame, and this fixture spawns in
        // `WorldStage::Present` — so waiting for it would cost the rig its first frame of foam
        // and shift every ripple's age by one step for the whole aged capture.
        benilla_world::world_unit::WorldUnit {
            kind: EntityKind::Unit,
            scale: 1.0,
            height: crate::entities::CollisionHeight::default().0,
        },
        crate::entities::VisualAttached,
        Transform::from_translation(wow_to_bevy([start_x, cy, surf - view.depth])),
    ));
    state.attached_at = Some(time.elapsed_secs());
    info!(
        "waterfx: rig armed (mode {}, speed {}, depth {}, age {})",
        match view.mode {
            WfxMode::Ring => "ring",
            WfxMode::Wake => "wake",
            WfxMode::Turn => "turn",
        },
        view.speed,
        view.depth,
        view.age
    );
}

/// Drive the dummy each frame: translate (wake) or spin (turn) until the age elapses; the foam
/// emitter reads the motion through its normal velocity/yaw proxies.
pub(crate) fn drive(
    view: Option<Res<WaterFxView>>,
    state: Option<Res<FxViewState>>,
    time: Res<Time>,
    mut units: Query<&mut Transform, (With<WaterFxDummy>, With<NetEntity>)>,
) {
    let (Some(view), Some(state)) = (view, state) else {
        return;
    };
    let Some(t0) = state.attached_at else {
        return;
    };
    if time.elapsed_secs() - t0 >= view.age {
        return; // hold still for the shot
    }
    for mut t in &mut units {
        match view.mode {
            WfxMode::Wake => {
                // WoW +X = Bevy −Z (bevy = (−wow.y, wow.z, −wow.x)).
                t.translation.z -= view.speed * time.delta_secs();
            }
            WfxMode::Turn => {
                t.rotate_y(2.0 * time.delta_secs());
            }
            WfxMode::Ring => {}
        }
    }
}
