//! World-map state: the `Map.dbc` catalog + the current `mapId`, shared by every subsystem that keys
//! off "which map are we on" — the terrain and WDL streamers, the loading screen, and time-of-day
//! lighting.
//!
//! These were historically defined inside the terrain streamer, but they're world state, not
//! streamer-private: four other subsystems reach for them. They live here so any of them — and either
//! terrain streamer, old or new — can read them without depending on the terrain streamer's setup.

use benilla_formats::{load_map_catalog, MapCatalog};
use bevy::prelude::*;

use crate::assets::LockRecover;
use crate::assets::{AssetSet, WorldAssets};

/// Default `mapId` (Eastern Kingdoms / Azeroth) — terrain is set up here before the world stream
/// arrives and tells us the character's real map. A worldport then bumps [`CurrentMap`].
pub(crate) const DEFAULT_MAP_ID: u32 = 0;

/// The map the player is currently on. Written by `player::control` when it drains the world stream's
/// worldport; watched by the terrain + WDL streamers (reload ADTs/WDL for the new map), the loading
/// screen (cover the swap), and time-of-day lighting (per-map outdoor light).
#[derive(Resource, Clone, Copy)]
pub(crate) struct CurrentMap(pub(crate) u32);

/// Bevy resource wrapper around the format-crate [`MapCatalog`] (`mapId` → directory + `LoadingScreenID`)
/// — Bevy's `Resource` trait can't be implemented on a foreign type, so we newtype it.
#[derive(Resource)]
pub(crate) struct MapCatalogRes(pub(crate) MapCatalog);

/// A cross-map transition, observed as a [`CurrentMap`] flip — the **world-scope teardown
/// signal**. Every module that dedups map-scoped assets behind a strong-handle cache reads this
/// and clears its own cache (the #bugs teleport leak: those caches pinned every map ever visited,
/// and each retained uv/tint-animated material re-uploaded per frame, forever). A clear is always
/// safe mid-session — live users hold handle clones, so it only drops the *dedup*; the assets die
/// when their last user despawns, and the next spawn rebuilds under the loading screen. Carries
/// no payload: every evictor clears unconditionally, and [`announce_map_change`] logs the
/// transition itself.
#[derive(Message, Clone, Copy)]
pub(crate) struct MapChange;

/// Startup set the world-map catalog loads in, so the terrain/WDL streamers can order their own setup
/// after it (they read [`MapCatalogRes`]/[`CurrentMap`] the moment they initialize).
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct WorldMapLoad;

/// Loads `Map.dbc` into [`MapCatalogRes`] and seeds [`CurrentMap`] at startup (after the patch chain
/// opens), before any map-keyed subsystem sets up.
pub(crate) struct WorldMapPlugin;

impl Plugin for WorldMapPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<MapChange>().add_systems(
            Startup,
            load_world_map.after(AssetSet::Open).in_set(WorldMapLoad),
        );
        app.add_systems(Update, announce_map_change);
    }
}

/// Announce every [`CurrentMap`] flip as a [`MapChange`]. The `Local` holds the last *announced*
/// map, so the startup seed (the first observation) announces nothing — there is no old map to
/// tear down. Watching the resource rather than the worldport wire keeps one definition of
/// "the map changed", and a same-map worldport (an instance re-enter) correctly stays silent.
fn announce_map_change(
    map: Option<Res<CurrentMap>>,
    mut last: Local<Option<u32>>,
    mut changes: MessageWriter<MapChange>,
) {
    let Some(map) = map else { return };
    if let Some(prev) = last.replace(map.0) {
        if prev != map.0 {
            info!(
                "map change: {prev} → {} — evicting map-scoped caches",
                map.0
            );
            changes.write(MapChange);
        }
    }
}

/// Read `Map.dbc` off the shared patch chain into [`MapCatalogRes`] and seed [`CurrentMap`].
fn load_world_map(mut commands: Commands, world_assets: Option<Res<WorldAssets>>) {
    let Some(world_assets) = world_assets else {
        return;
    };
    let mut chain = world_assets.chain.lock_recover();
    match load_map_catalog(&mut chain) {
        Ok(c) => {
            info!("Map.dbc: {} maps catalogued", c.len());
            commands.insert_resource(MapCatalogRes(c));
            // `$WOW_MAP` seeds a different continent for a SERVER-LESS run (a `WOW_CAPTURE=vista`
            // shot in Kalimdor, say — a `Map.dbc` id; 1 = Kalimdor). Live sessions are unaffected:
            // `player::wire_in` overwrites this from the server's world-verify the moment it says
            // where we are. Without it every headless capture is stuck in Eastern Kingdoms, and a
            // horizon report from anywhere else cannot be reproduced.
            let map = std::env::var("WOW_MAP")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_MAP_ID);
            commands.insert_resource(CurrentMap(map));
        }
        Err(e) => error!("failed to load Map.dbc, cross-map teleport disabled: {e:#}"),
    }
}
