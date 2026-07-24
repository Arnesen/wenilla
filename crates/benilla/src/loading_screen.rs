//! Loading screen — the faithful full-screen world-load splash + progress bar shown on initial
//! world entry and on cross-map teleport (the load latency async streaming can't hide). Built on the
//! VERIFIED reference mechanism: per-map art via the
//! `Map.dbc` → `LoadingScreens.dbc` → BLP FK chain (resolved by `benilla-formats`), under the engine's
//! bar — which in 1.12 is exactly two layers, `Loading-BarBorder` + `Loading-BarFill`, at the
//! screen-fraction rects byte-verified from `LoadingScreen.cpp` (NOT the 5-texture stack the asset
//! names suggest; Background/Glow/Glass are never composited in vanilla).
//!
//! **Foundation, not a one-off.** The art lookup is the *same* mechanism for every map kind (open
//! world / instance / battleground) — only the BLP row differs — so this hosts all of them for free.
//! The two cases out of current scope (taxi/boat/zeppelin's moving flight-path icon; a richer
//! progress model) slot in as an extra overlay layer / a different progress source without restructure.
//!
//! Trigger (scope locked with the user): **startup + `CurrentMap` change only** — normal streaming
//! stays seamless. Cleared once the tiles the streamer wants around the view are resident
//! ([`WorldLoadProgress`], published by `terrain_stream`). The exact reference clear-condition +
//! progress-fraction backing are still INFERRED (see the knowledge note); the streamer residency
//! ratio is our faithful-enough stand-in until a capture says otherwise.

use bevy::prelude::*;
use std::collections::HashMap;

use benilla_formats::{load_loading_screens, LoadingScreenCatalog};

use crate::assets::LockRecover;
use crate::assets::{AssetSet, WorldAssets};
use crate::schedule::WorldStage;
use crate::world_map::{CurrentMap, MapCatalogRes};

// Bar layout — VERIFIED THREE ways (build 5875): the `WoW.exe` `LoadingScreen.cpp` bar descriptor
// table (@0x7ffd34, `FUN_00407150`) gives entry = {cx, cy, halfW, halfH} with rect = [cx ± halfW·0.5]
// × [cy ± halfH·0.5] and fill right edge = left + progress·halfW; Border {0.5,0.075,0.600,0.050}, Fill
// {0.5,0.075,0.525,0.025}. An apitrace of the live reference (WoW.12/WoW.17) confirmed these rects, and
// a reference screenshot measured the fill at left=0.245 (≈0.2375), ~9% from the BOTTOM. The bar sits
// at the BOTTOM — `cy=0.075` is measured from the bottom (the engine's GL ortho origin); the trace's
// "top" reading was the wined3d D3D→GL y-flip (a host representation, see memory
// `apitrace-is-crossover-translated`), refuted by the screenshot + the binary. ONLY Border + Fill are
// drawn in 1.12 (Background/Glow/Glass are never composited). Rects are viewport fractions; y is the
// distance from the BOTTOM edge (Bevy UI `bottom:`).
const BORDER_LEFT: f32 = 0.200; // 0.5 − 0.600·0.5
const BORDER_WIDTH: f32 = 0.600;
const BORDER_BOTTOM: f32 = 0.050; // 0.075 − 0.050·0.5
const BORDER_HEIGHT: f32 = 0.050;
const FILL_LEFT: f32 = 0.2375; // 0.5 − 0.525·0.5
const FILL_BOTTOM: f32 = 0.0625; // 0.075 − 0.025·0.5
const FILL_HEIGHT: f32 = 0.025;
const FILL_MAX_WIDTH: f32 = 0.525; // halfW; fill width = progress · FILL_MAX_WIDTH

/// The glue/loading screen is authored 4:3; on a wider window the reference fits it to height and
/// letterboxes (black bars L/R). Measured from a reference screenshot (content ≈ 1878×1385 ≈ 4:3 in a
/// 1999-wide window). The square BLP is stretched to this aspect (a mild widen).
const BACKDROP_ASPECT: f32 = 4.0 / 3.0;
/// Frames the world must read fully-resident before we clear the screen — debounces the post-teleport
/// frame where `loaded` is drained (`total > 0`, `ready` momentarily 0) so we don't flicker off/on.
const CLEAR_AFTER_READY_FRAMES: u32 = 3;

/// How many of the tiles the streamer wants around the view focus are actually spawned. Written each
/// frame by `terrain_stream::stream_terrain`; read here to drive the bar + the clear condition.
#[derive(Resource, Default)]
pub(crate) struct WorldLoadProgress {
    pub(crate) ready: usize,
    pub(crate) total: usize,
    /// Whether the tile under the view focus (the nearest desired tile) is spawned. `false` until the
    /// streamer first runs, and again whenever a teleport/worldport drops the focus onto unloaded
    /// ground — this is what triggers the loading screen (covers startup, cross-map worldport, AND
    /// far same-map teleports like a cross-continent `.tele`, which don't change `CurrentMap`). During
    /// normal streaming the focus tile is always resident, so it never fires spuriously.
    pub(crate) focus_resident: bool,
}

impl WorldLoadProgress {
    /// 0..1 residency fraction for the bar — **0.0 when nothing is wanted yet** (the cold-start /
    /// pre-`desired` frame), so the bar starts empty rather than flashing full. (Distinct from the
    /// clear test [`Self::is_ready`], which treats `total == 0` as not-ready.)
    fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.ready as f32 / self.total as f32).clamp(0.0, 1.0)
        }
    }

    /// True once every wanted tile is resident. Requires `total > 0` so the cold-start / post-swap
    /// frame (before `desired` is computed) doesn't read as "done".
    fn is_ready(&self) -> bool {
        self.total > 0 && self.ready >= self.total
    }
}

/// Bevy resource wrapper around the format-crate [`LoadingScreenCatalog`] (the `LoadingScreenID` → BLP
/// path table). Paired with [`MapCatalogRes`] (the `mapId` → `LoadingScreenID` FK) to resolve art.
#[derive(Resource)]
struct LoadingScreenCatalogRes(LoadingScreenCatalog);

/// Loading-screen state machine.
#[derive(Resource, Default)]
pub(crate) struct LoadingScreen {
    active: bool,
    /// Consecutive fully-resident frames while active (see [`CLEAR_AFTER_READY_FRAMES`]).
    ready_frames: u32,
    /// Monotonic bar fill, 0..1 — only ever advances within a load (reset to 0 on each activation), so
    /// the bar reads as one continuous stream even though the raw residency ratio dips as `desired`
    /// shifts while the view moves. A real loading bar never goes backwards.
    displayed: f32,
    /// Decoded backdrop art by BLP path, so repeated teleports to a continent don't re-decode.
    art_cache: HashMap<String, Handle<Image>>,
}

impl LoadingScreen {
    /// Whether the opaque loading backdrop currently covers the frame — the world camera renders
    /// under it (pipeline warm-up), never behind the glue screens (decision 0540).
    pub(crate) fn covering(&self) -> bool {
        self.active
    }
}

// UI entity markers.
#[derive(Component)]
struct LoadingRoot;
#[derive(Component)]
struct LoadingBackdrop;
#[derive(Component)]
struct LoadingBarFill;

pub(crate) struct LoadingScreenPlugin;

impl Plugin for LoadingScreenPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WorldLoadProgress>()
            .init_resource::<LoadingScreen>()
            .add_systems(Startup, setup_loading_screen.after(AssetSet::Open))
            // In `WorldStage::Present` (after Input + Stream): we read residency for the SAME frame's
            // player position — a teleport snaps in Input → the streamer recomputes focus in Stream →
            // we cover it here. Visibility set now propagates in PostUpdate and renders this frame, so
            // the swap never flashes.
            .add_systems(Update, drive_loading_screen.in_set(WorldStage::Present));
    }
}

/// Startup: load the LoadingScreens catalog + the bar texture stack off the shared chain, and spawn
/// the (initially hidden, then activated on the first `drive` frame) UI tree.
fn setup_loading_screen(
    mut commands: Commands,
    world_assets: Option<ResMut<WorldAssets>>,
    mut images: ResMut<Assets<Image>>,
) {
    let Some(mut assets) = world_assets else {
        return;
    };

    // Catalog (LoadingScreenID → BLP path).
    match load_loading_screens(&mut assets.chain.lock_recover()) {
        Ok(c) => {
            info!("LoadingScreens.dbc: {} screens catalogued", c.len());
            commands.insert_resource(LoadingScreenCatalogRes(c));
        }
        Err(e) => {
            error!("LoadingScreens.dbc unavailable, loading screen disabled: {e:#}");
            return;
        }
    }

    // The two layers 1.12 actually draws (sRGB clamp sprites — UI art, not tiling world art).
    let mut tex = |path: &str| assets.sprite_texture(path, &mut images);
    let Some(border) = tex("Interface\\Glues\\LoadingBar\\Loading-BarBorder.blp") else {
        error!("loading bar textures missing, loading screen disabled");
        return;
    };
    let fill = tex("Interface\\Glues\\LoadingBar\\Loading-BarFill.blp").unwrap_or_default();
    // The spawned ImageNodes below own these handles (the root is never despawned, only hidden), so
    // the textures stay resident without a separate holder resource.

    // Root: fullscreen black — this IS the pillarbox letterbox. Flex-centres the 4:3 content area.
    commands
        .spawn((
            LoadingRoot,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(Color::BLACK),
            // Above the 3D scene; UI already paints after the main pass, this orders within UI.
            GlobalZIndex(1000),
            Visibility::Hidden,
        ))
        .with_children(|root| {
            // The whole loading screen renders in one 4:3 area fit to viewport height, centred, with
            // the root's black showing as pillarbox bars L/R (the reference's behaviour — the glue/load
            // screen is authored 4:3; on a wider window it letterboxes). Width = height·4/3 in vh.
            // The backdrop + bar are children, so their verified fractions are relative to THIS area.
            // Sibling paint order = spawn order: backdrop → fill → border (border on top).
            root.spawn(Node {
                width: Val::Vh(100.0 * BACKDROP_ASPECT),
                height: Val::Vh(100.0),
                position_type: PositionType::Relative,
                ..default()
            })
            .with_children(|area| {
                // Backdrop art: the square BLP stretched to fill the 4:3 area (mild widen — what the
                // reference does). Starts black (default-white texture tinted) until the art resolves.
                area.spawn((
                    LoadingBackdrop,
                    ImageNode {
                        color: Color::BLACK,
                        image_mode: NodeImageMode::Stretch,
                        ..default()
                    },
                    Node {
                        position_type: PositionType::Absolute,
                        width: Val::Percent(100.0),
                        height: Val::Percent(100.0),
                        ..default()
                    },
                ));
                // Fill — left-anchored; width = progress·FILL_MAX_WIDTH (set each frame). y from the
                // BOTTOM (verified by screenshot + binary). The fill art is a horizontally-uniform
                // gradient, so width-scaling reads as a left→right reveal.
                area.spawn((
                    LoadingBarFill,
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Percent(FILL_LEFT * 100.0),
                        bottom: Val::Percent(FILL_BOTTOM * 100.0),
                        width: Val::Percent(0.0), // set each frame
                        height: Val::Percent(FILL_HEIGHT * 100.0),
                        ..default()
                    },
                    ImageNode {
                        image: fill,
                        image_mode: NodeImageMode::Stretch,
                        ..default()
                    },
                ));
                // Border frame, on top.
                area.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Percent(BORDER_LEFT * 100.0),
                        bottom: Val::Percent(BORDER_BOTTOM * 100.0),
                        width: Val::Percent(BORDER_WIDTH * 100.0),
                        height: Val::Percent(BORDER_HEIGHT * 100.0),
                        ..default()
                    },
                    ImageNode {
                        image: border,
                        image_mode: NodeImageMode::Stretch,
                        ..default()
                    },
                ));
            });
        });
}

/// Per-frame: run the trigger state machine, resolve backdrop art on (re)activation, and push the
/// progress fraction into the bar.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn drive_loading_screen(
    mut screen: ResMut<LoadingScreen>,
    progress: Res<WorldLoadProgress>,
    current_map: Option<Res<CurrentMap>>,
    maps: Option<Res<MapCatalogRes>>,
    screens: Option<Res<LoadingScreenCatalogRes>>,
    mut assets: Option<ResMut<WorldAssets>>,
    mut images: ResMut<Assets<Image>>,
    mut root: Query<&mut Visibility, With<LoadingRoot>>,
    mut backdrop: Query<&mut ImageNode, With<LoadingBackdrop>>,
    mut fill: Query<&mut Node, With<LoadingBarFill>>,
    player: Option<Res<crate::player::Player>>,
) {
    let map_id = current_map.as_ref().map(|m| m.0);
    // The avatar is "settling" after a teleport until the collision under it (terrain *and* the
    // destination's WMO building floors) has streamed in. Hold the screen until then — otherwise it
    // clears the moment the terrain is ready, before the buildings load (and the avatar is mid-settle).
    let player_settling = player.as_ref().is_some_and(|p| p.settling);

    // --- Trigger: show whenever the ground under the view focus isn't resident (startup, cross-map
    // worldport, or a far same-map teleport — all land us on unloaded tiles); clear once the desired
    // ring is fully resident for a few frames. Normal streaming keeps the focus tile resident, so this
    // never fires while walking. ---
    if !screen.active && !progress.focus_resident {
        screen.active = true;
        screen.ready_frames = 0;
        screen.displayed = 0.0; // restart the fill for the new load
        info!("loading screen: up (map {map_id:?})");
    }
    if screen.active {
        if progress.is_ready() && !player_settling {
            screen.ready_frames += 1;
            if screen.ready_frames >= CLEAR_AFTER_READY_FRAMES {
                screen.active = false;
                info!(
                    "loading screen: cleared ({}/{} tiles resident, settle done)",
                    progress.ready, progress.total
                );
            }
        } else {
            screen.ready_frames = 0;
        }
    }

    // --- Visibility. ---
    if let Ok(mut vis) = root.single_mut() {
        *vis = if screen.active {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    if !screen.active {
        return;
    }

    // --- Backdrop art (resolve once per map via the FK chain; cached by path). ---
    if let (Some(map_id), Some(maps), Some(screens), Some(assets)) =
        (map_id, maps.as_ref(), screens.as_ref(), assets.as_mut())
    {
        if let Some(path) = maps
            .0
            .loading_screen_id(map_id)
            .and_then(|id| screens.0.path(id))
        {
            let path = path.to_string();
            let handle = match screen.art_cache.get(&path) {
                Some(h) => Some(h.clone()),
                None => assets.sprite_texture(&path, &mut images).inspect(|h| {
                    screen.art_cache.insert(path.clone(), h.clone());
                }),
            };
            if let (Some(handle), Ok(mut img)) = (handle, backdrop.single_mut()) {
                if img.image != handle {
                    img.image = handle;
                    img.color = Color::WHITE; // reveal the art (was tinted black)
                }
            }
        }
    }

    // --- Progress bar: monotonic fill, reveals left→right, width = progress·FILL_MAX_WIDTH. ---
    screen.displayed = screen.displayed.max(progress.fraction());
    let frac = screen.displayed;
    if let Ok(mut node) = fill.single_mut() {
        node.width = Val::Percent(frac * FILL_MAX_WIDTH * 100.0);
    }
}
