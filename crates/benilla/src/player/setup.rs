//! Startup: spawn the camera and seed the avatar resources the [`super::control`] system then drives.
//! Split from the controller because it's a one-shot `Startup` system (a distinct schedule phase), not
//! the per-frame loop — the plugin remains the stable face that wires both.

use avian3d::prelude::*;
use bevy::camera::{PerspectiveProjection, Projection};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::prelude::*;
use bevy::render::view::Hdr;

use benilla_assets::coords::wow_to_bevy;

use crate::assets::{RenderConfig, WorldAssets};
use crate::lighting::fog_range;
use crate::SPAWN_XY;

use super::{
    CameraControl, CameraProbe, FlyCam, MoveSpeed, Player, PlayerCapsule, CAM_COLLISION_RADIUS,
    CAM_DIST_DEFAULT, CAM_NEAR, CAPSULE_HEIGHT, CAPSULE_RADIUS,
};

/// Default avatar speed in yards/second — the **VERIFIED** vanilla run speed (`MOVE_RUN` 7.0). Ctrl
/// sprints 2.5× (≈17.5) for getting around; `$WOW_MOVE_SPEED`
/// overrides. (Was 60.0 as a fly-around convenience before collision; the faithful default now that
/// the character controller is in.)
const DEFAULT_MOVE_SPEED: f32 = 7.0;

fn spawn_fallback_camera(commands: &mut Commands) {
    commands.spawn((
        Camera3d::default(),
        super::WorldCamera,
        Hdr,
        Tonemapping::None,
        crate::ffx_glow::FfxGlow,
        Transform::from_xyz(0.0, 50.0, 100.0).looking_at(Vec3::ZERO, Vec3::Y),
        FlyCam {
            yaw: 0.0,
            pitch: 0.0,
            speed: 40.0,
        },
    ));
}

/// Startup: insert the move speed + avatar state, and spawn the camera. With client data present the
/// camera sits above the spawn with the scene `AmbientLight`/`DistanceFog` (driven each frame by
/// `update_time_lighting`) + a radius-derived far plane; without it, a plain free-fly fallback camera.
pub(super) fn setup_player(
    mut commands: Commands,
    config: Option<Res<RenderConfig>>,
    world_assets: Option<Res<WorldAssets>>,
) {
    let env_speed = std::env::var("WOW_MOVE_SPEED")
        .ok()
        .and_then(|s| s.parse::<f32>().ok());
    commands.insert_resource(MoveSpeed {
        value: env_speed.unwrap_or(DEFAULT_MOVE_SPEED),
        env_override: env_speed.is_some(),
    });
    commands.insert_resource(Player::default());
    // The character capsule swept by avian's `MoveAndSlide` (length = cylinder segment between the
    // hemisphere centres, so total height is `length + 2·radius`).
    commands.insert_resource(PlayerCapsule(Collider::capsule(
        CAPSULE_RADIUS,
        CAPSULE_HEIGHT - 2.0 * CAPSULE_RADIUS,
    )));
    commands.insert_resource(CameraProbe(Collider::sphere(CAM_COLLISION_RADIUS)));
    commands.insert_resource(CameraControl {
        distance: CAM_DIST_DEFAULT,
        target_distance: CAM_DIST_DEFAULT,
        collision_distance: CAM_DIST_DEFAULT,
        // Start opaque so the avatar never flashes invisible before `control`'s first fade computation.
        self_fade_alpha: 1.0,
        ..default()
    });

    // No client data → free-fly an empty scene.
    let (Some(config), Some(_)) = (config, world_assets) else {
        spawn_fallback_camera(&mut commands);
        return;
    };

    // Camera starts above the spawn (terrain streams in around it); `control` repositions it
    // third-person once we're in the world. The far plane is sized from the load radius (the
    // radius-derived view range); distance fog itself is added back in the lighting rebuild (step 5).
    let spawn = wow_to_bevy([SPAWN_XY.0, SPAWN_XY.1, 100.0]);
    let cam_far = (fog_range(config.tile_radius).1 + 800.0).max(3000.0);
    commands.spawn((
        Camera3d::default(),
        // THE world camera (the portrait booths are further `Camera3d`s — every "where is the viewer"
        // consumer filters on this marker, never on bare `Camera3d`; see its doc).
        super::WorldCamera,
        // MSAA from $WOW_MSAA (off/0/1 → Off, 2, 4 default, 8). A STARTUP knob, not a live toggle:
        // switching MSAA at runtime leaves our post-process passes (glow/egui) MSAA-mismatched and
        // freezes the view, so A/B by restarting. 4× is real GPU fill cost — performance-foundation.md.
        match std::env::var("WOW_MSAA").ok().as_deref() {
            Some("off") | Some("0") | Some("1") => bevy::render::view::Msaa::Off,
            Some("2") => bevy::render::view::Msaa::Sample2,
            Some("8") => bevy::render::view::Msaa::Sample8,
            _ => bevy::render::view::Msaa::Sample4,
        },
        Projection::from(PerspectiveProjection {
            far: cam_far,
            near: CAM_NEAR,
            ..default()
        }),
        // HDR render target (linear `Rgba16Float`) — the prerequisite for Bevy's `Bloom`. `Hdr` is a
        // marker component (`Bloom` requires it; added explicitly for clarity). Option 1 keeps the
        // vanilla look: `Tonemapping::None` (no filmic curve — the shaders still light in clamped gamma
        // space and output the same values), so the HDR pipeline reproduces the LDR look while letting
        // Bevy's bloom replace the hand-rolled `glow.rs`. (Option 2, on a branch, swaps in a filmic
        // tonemapper + scene-referred lighting for a modern look.)
        Hdr,
        Tonemapping::None,
        // The faithful FFXGlow pass (decision 0158/0161): the byte-pinned `scene + glow·blur²`
        // — and, in the gamma lane, the owner of the frame's single output decode.
        crate::ffx_glow::FfxGlow,
        Transform::from_translation(spawn + Vec3::new(0.0, 60.0, 60.0)).looking_at(spawn, Vec3::Y),
        // PHASE 0: no PBR ambient fill, no distance fog — pitch-black clean slate. The faithful scene
        // light is rebuilt in-shader from Light.dbc (terrain/model WGSL), not via Bevy PBR lights.
        FlyCam {
            yaw: 0.0,
            pitch: -0.5,
            speed: 100.0,
        },
    ));
}

/// The world camera's demand gate (decision 0540): active in world or under the opaque loading
/// screen — never behind the glue screens, where the streamed world (25 tiles, tens of thousands
/// of entities, MSAA 4×) otherwise renders unseen behind an opaque fullscreen glue scene every
/// frame. The loading-screen case is load-bearing: that covered render is what compiles the
/// world's pipelines at boot, so the first visible in-world frame doesn't hitch on shader builds.
/// Capture runs boot straight `InWorld` (`CharSelectPlugin::start_in_world`) — always active there.
pub(super) fn gate_world_camera(
    state: Res<State<crate::char_select::ClientState>>,
    loading: Res<crate::loading_screen::LoadingScreen>,
    mut cams: Query<&mut Camera, With<super::WorldCamera>>,
) {
    let active = *state.get() == crate::char_select::ClientState::InWorld || loading.covering();
    for mut cam in &mut cams {
        if cam.is_active != active {
            cam.is_active = active;
        }
    }
}
