//! Distant low-detail terrain (WDL): streams the map's coarse horizon heightmap around the view —
//! beyond the detailed ADT ring — drawn unlit-white + fogged, the hills the reference shows on the
//! horizon where ours used to fade to void. The parse + coarse mesh live in `benilla_formats::wdl`; this
//! is just the Bevy streaming + render glue (one shared [`WdlMaterial`], one mesh per ring tile).
//!
//! Mechanism + RE (geometry/shading/fog/depth all VERIFIED from apitrace WoW.8 + the real `.wdl`).
//! How it partitions against the detailed world — the reference's far-band **backdrop** law, and why a
//! shared clip plane could not work — is `wdl.wgsl`'s header (decision 0684).

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::ExtendedMaterial;
use bevy::prelude::*;

use crate::assets::LockRecover;
use crate::assets::{AssetSet, RenderConfig, WorldAssets};
use crate::player::{Player, WorldCamera};
use crate::terrain::{WdlExt, WdlMaterial};
use crate::world_map::{CurrentMap, MapCatalogRes};
use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use benilla_formats::WdlFile;

/// Chebyshev tile radius the WDL ring covers around the view. Past ~`fog_end` (~481 yd ≈ <1 tile) WDL
/// is pure haze, but hills 2–4 tiles out still rise above the horizon as fogged silhouettes, so we
/// extend toward the reference's `horizonfarclip` (~2112 yd ≈ 4 tiles). Drawn as a full ring; the
/// shader makes it a **backdrop** — near plane at `farclip − 33`, depth clamped behind everything the
/// detailed world can draw (`wdl.wgsl`'s header, decision 0684) — so it fills whatever the detailed
/// world leaves empty and can never overlap it. (The reference's own far walk is a ±3-tile window.)
const WDL_RADIUS: u32 = 5;

/// New WDL tiles built per frame — coarse meshes are cheap (545 verts), but a whole ring at once
/// would hitch on zone load, so spread it.
const WDL_LOADS_PER_FRAME: usize = 8;

/// The WDL subsystem: load the map's `.wdl` + a shared fog material at startup, then stream the
/// coarse horizon ring in/out around the view each frame.
pub(crate) struct WdlPlugin;

impl Plugin for WdlPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<WdlMaterial>::default())
            .add_systems(Startup, setup_wdl.after(AssetSet::Open))
            // The distant ring is world, and follows the world's lifecycle (decision 0777): it
            // spawns only while a character is in one, and its meshes go when they leave.
            .add_systems(Update, stream_wdl.run_if(crate::schedule::world_is_live))
            .add_systems(
                OnExit(crate::char_select::ClientState::InWorld),
                release_wdl_ring,
            );
    }
}

/// Streams the coarse WDL ring: the parsed file, the shared material every ring tile uses, and which
/// ring tiles are currently spawned. (The material holds no fog of its own — it reads the shared
/// global-light buffer, same as terrain and the models.)
#[derive(Resource)]
pub(crate) struct WdlStreamer {
    wdl: WdlFile,
    /// `mapId` the loaded `wdl` is for; a cross-map teleport (≠ [`CurrentMap`]) reloads it.
    map_id: u32,
    material: Handle<WdlMaterial>,
    loaded: HashMap<(u32, u32), Entity>,
}

impl WdlStreamer {
    /// The drawn WDL surface height (raw WoW `z`) under a **Bevy-space** position — whole-map
    /// coverage from the coarse horizon heightmap, exact to the rendered mesh
    /// (`WdlFile::height_at`). The far leg of the lens-flare occlusion march
    /// (`sun::follow::FlareGate`), beyond the resident detailed-terrain ring. `None` off the map
    /// or where no WDL tile is authored (open ocean) — never an occluder there.
    pub(crate) fn height_under(&self, bevy_pos: Vec3) -> Option<f32> {
        let wow = bevy_to_wow(bevy_pos);
        self.wdl.height_at(wow[0], wow[1])
    }
}

/// Marks a spawned WDL tile (so a future system could query/cull them as a group if needed).
#[derive(Component)]
struct WdlTile;

fn setup_wdl(
    mut commands: Commands,
    config: Option<Res<RenderConfig>>,
    world_assets: Option<ResMut<WorldAssets>>,
    mut materials: ResMut<Assets<WdlMaterial>>,
) {
    let (Some(_config), Some(world_assets)) = (config, world_assets) else {
        return; // no client data → no terrain at all, so no WDL
    };
    // Default map = Azeroth (Eastern Kingdoms), matching world_map's DEFAULT_MAP_ID.
    let wdl = match WdlFile::load(&mut world_assets.chain.lock_recover(), "Azeroth") {
        Ok(w) => {
            info!(
                "WDL: Azeroth.wdl loaded ({} distant tiles)",
                w.present_count()
            );
            w
        }
        Err(e) => {
            warn!("WDL unavailable, no distant terrain: {e:#}");
            return;
        }
    };
    // One shared material, reading the shared global light like terrain does — no seed and no
    // per-frame push: `build_light_data` has already packed the fog by the first draw. Opaque ⇒
    // depth-LEQUAL + depth-write, no blend (the verified WoW.8 state).
    let material = materials.add(ExtendedMaterial {
        base: StandardMaterial {
            base_color: Color::WHITE,
            unlit: true,
            cull_mode: None,
            ..default()
        },
        extension: WdlExt {
            light_buf: world_assets.shared_light.clone(),
        },
    });
    commands.insert_resource(WdlStreamer {
        wdl,
        map_id: 0,
        material,
        loaded: HashMap::new(),
    });
}

#[allow(clippy::too_many_arguments)]
fn stream_wdl(
    mut commands: Commands,
    streamer: Option<ResMut<WdlStreamer>>,
    assets: ResMut<WorldAssets>,
    player: Res<Player>,
    camera: Query<&Transform, With<WorldCamera>>,
    current_map: Res<CurrentMap>,
    map_catalog: Res<MapCatalogRes>,
    roster: Option<Res<crate::char_select::Roster>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let Some(mut streamer) = streamer else {
        return;
    };
    // The map the *focus* is on — `CurrentMap` once the snap has landed, the picked character's
    // own map during world entry, so the ring is never built for the map we are leaving.
    let entry = roster
        .as_deref()
        .and_then(crate::char_select::Roster::pending_entry);
    let map_id = crate::terrain_stream::focus_map(Some(current_map.0), &player, entry);

    // Cross-map teleport: reload the new map's `.wdl` and drop the old ring.
    if map_id != streamer.map_id {
        if let Some(dir) = map_catalog.0.directory(map_id) {
            match WdlFile::load(&mut assets.chain.lock_recover(), dir) {
                Ok(wdl) => {
                    for (_, e) in streamer.loaded.drain() {
                        commands.entity(e).despawn();
                    }
                    streamer.wdl = wdl;
                    streamer.map_id = map_id;
                }
                // No `.wdl` for this map (instances often lack one) → just clear the ring.
                Err(_) => {
                    for (_, e) in streamer.loaded.drain() {
                        commands.entity(e).despawn();
                    }
                    streamer.map_id = map_id;
                }
            }
        }
    }

    // The same view focus the detailed streamer uses — literally the same function now, rather
    // than a copy under a comment claiming they agree (decision 0777).
    let center = crate::terrain_stream::view_focus(
        &player,
        camera.single().ok().map(|c| c.translation),
        entry,
    );
    // The FULL window, the camera's own tile INCLUDED (`tiles_in_ring`'s doc is the why — at a
    // lowered view distance the own tile *is* the near horizon, and dropping it leaves a gap the sky
    // pours through). What bounds the band on the near side is the shader's near plane, never the
    // streamed set, so this cannot toggle against the detailed streaming radius — a constant backdrop.
    let desired = streamer.wdl.tiles_in_ring(center[0], center[1], WDL_RADIUS);

    // Unload ring tiles no longer wanted.
    let stale: Vec<(u32, u32)> = streamer
        .loaded
        .keys()
        .filter(|c| !desired.contains(c))
        .copied()
        .collect();
    for coords in stale {
        if let Some(e) = streamer.loaded.remove(&coords) {
            commands.entity(e).despawn();
        }
    }

    // Load a few missing tiles this frame.
    let missing: Vec<(u32, u32)> = desired
        .into_iter()
        .filter(|c| !streamer.loaded.contains_key(c))
        .take(WDL_LOADS_PER_FRAME)
        .collect();
    for coords in missing {
        let Some(tile) = streamer.wdl.tile_mesh(coords.0, coords.1) else {
            continue;
        };
        let positions: Vec<[f32; 3]> = tile
            .positions
            .iter()
            .map(|p| wow_to_bevy(*p).to_array())
            .collect();
        // Unlit + untextured: a constant up-normal and zero UV satisfy the StandardMaterial pipeline;
        // the custom WDL shader reads neither (it outputs white × fog).
        let n = positions.len();
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; n]);
        mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, vec![[0.0, 0.0]; n]);
        mesh.insert_indices(Indices::U32(tile.indices));
        let e = commands
            .spawn((
                Mesh3d(meshes.add(mesh)),
                MeshMaterial3d(streamer.material.clone()),
                Transform::IDENTITY,
                WdlTile,
            ))
            .id();
        streamer.loaded.insert(coords, e);
    }
}

/// Leaving the world drops the distant ring (decision 0777). The parsed `.wdl` itself stays — it is
/// one small file, the resource `stream_wdl` needs to exist at all, and `height_under` is read by
/// the sun's flare-occlusion march; what costs per frame is the spawned mesh set, and that goes.
fn release_wdl_ring(mut commands: Commands, streamer: Option<ResMut<WdlStreamer>>) {
    let Some(mut streamer) = streamer else { return };
    for (_, e) in streamer.loaded.drain() {
        commands.entity(e).despawn();
    }
}

/// The backdrop law (`wdl.wgsl`'s header, decision 0684) is a property of the **shader**, so it is
/// checked there — the same shape as `sky_order.rs`'s depth-law test, and for the same reason: the
/// failure it guards is invisible except at a ridge crest on a fogged horizon, and the obvious "tidy-up"
/// (go back to one shared clip plane, drop the frag-depth write) silently reintroduces it.
#[test]
fn the_far_band_stays_a_depth_pushed_backdrop() {
    let src = include_str!("../assets/shaders/wdl.wgsl");
    assert!(
        src.contains("const WDL_OVERLAP: f32 = 33.0;"),
        "wdl.wgsl: the far band's 33 yd overlap into the wall is gone — the coarse-vs-fine seam \
         reopens as a hole at the horizon (the reference's far-band near plane, [0x8101b0])"
    );
    assert!(
        src.contains("out.depth = depth;") && src.contains("view_z_to_depth_ndc(-farclip)"),
        "wdl.wgsl: the far band no longer clamps its depth behind the far-clip wall — its overlap \
         now pokes THROUGH the detailed terrain (the reference's compressed far-band depth range)"
    );
}
