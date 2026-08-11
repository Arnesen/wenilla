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
//! **The lifecycle is event-raised, readiness-cleared (decision 0737).** The reference *blocks* on
//! its world load, so its screen covers from the entry edge until the world is loaded by
//! construction; an async-streaming client has to build that observable explicitly. The screen
//! rises at the explicit edges — the character pick's `Connected` edge (before the glue tears
//! down), `SMSG_TRANSFER_PENDING` (the portal walk-in), the worldport snap — all observed *here*,
//! from the messages/resources the net bridge already publishes; a residency backstop catches any
//! path with no edge, **in world only** — it used to fire at boot too, and that is what warmed the
//! world's pipelines behind the glue (0540), which only worked while a world was being streamed
//! there at all. Since 0777 none is, and the warm-up rides the entry cover instead. It clears when
//! the destination is **scene-presentable** ([`WorldLoadProgress`],
//! published by `terrain_stream` + the collider queue): every wanted tile spawned, the focus
//! neighbourhood's placements up, colliders quiet, the snap no longer awaited. Never anything
//! about the *body* — feet-on-ground is not a load condition (the flying-teleport hang, 0737).

use bevy::prelude::*;
use std::collections::HashMap;

use benilla_formats::{load_loading_screens, LoadingScreenCatalog};

use benilla_assets::LockRecover;
use benilla_assets::MapCatalogRes;
use benilla_assets::{AssetSet, WorldAssets};
use benilla_world::schedule::WorldStage;
use benilla_world::terrain_stream::WorldLoadProgress;
use benilla_world::world_map::CurrentMap;

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
/// The wait instrument (decision 0737): once the screen has been up this long, say *which term*
/// still blocks the clear, every [`WAIT_LOG_EVERY`] seconds — so a "loading screen stuck" report is
/// a one-line diagnosis instead of a session. Ordinary loads finish under the threshold and log
/// nothing extra.
const WAIT_LOG_AFTER: f32 = 3.0;
const WAIT_LOG_EVERY: f32 = 2.0;

/// Bevy resource wrapper around the format-crate [`LoadingScreenCatalog`] (the `LoadingScreenID` → BLP
/// path table). Paired with [`MapCatalogRes`] (the `mapId` → `LoadingScreenID` FK) to resolve art.
#[derive(Resource)]
struct LoadingScreenCatalogRes(LoadingScreenCatalog);

/// Loading-screen state machine (decision 0737: event-raised, readiness-cleared).
#[derive(Resource, Default)]
pub(crate) struct LoadingScreen {
    active: bool,
    /// A raise from an entry edge (the pick's `Connected`, `SMSG_TRANSFER_PENDING`) holds until the
    /// destination snap (worldport/teleport) actually lands — so the OLD location's readiness can
    /// never clear a screen raised for the NEW one. This is what closes the world-entry flash: the
    /// login-vista tiles are fully resident the moment the glue tears down, and without this hold
    /// that residency would clear the screen seconds before `SMSG_LOGIN_VERIFY_WORLD` arrives.
    awaiting_snap: bool,
    /// Destination art before `CurrentMap` catches up: the roster's `Character.map` at the pick
    /// edge, the transfer's map at `SMSG_TRANSFER_PENDING`. Cleared by the snap (from which frame
    /// `CurrentMap` itself is correct).
    pending_map: Option<u32>,
    /// **Plain black cover, no art, no bar** — the logout transition (decision 0738). The logout
    /// teardown despawns the avatar and the streamed world in the same frame's Net stage, but
    /// `CharSelect` (and the glue screen with it) only applies at the NEXT frame's state
    /// transition — without this the dead world renders uncovered for that frame, and it is
    /// exactly the teardown-burst frame, so it lingers. The ref's world→glue swap shows black
    /// there too. Dropped the moment the state leaves `InWorld` (the glue owns the screen from
    /// then, at a higher z); any real raise replaces it with the full screen.
    blackout: bool,
    /// Consecutive presentable frames while active (see [`CLEAR_AFTER_READY_FRAMES`]).
    ready_frames: u32,
    /// Monotonic bar fill, 0..1 — only ever advances within a load (reset to 0 on each activation), so
    /// the bar reads as one continuous stream even though the raw residency ratio dips as `desired`
    /// shifts while the view moves. A real loading bar never goes backwards.
    displayed: f32,
    /// Decoded backdrop art by BLP path, so repeated teleports to a continent don't re-decode.
    art_cache: HashMap<String, Handle<Image>>,
    /// `Time::elapsed_secs` at the last raise + at the last wait-instrument line (see
    /// [`WAIT_LOG_AFTER`]).
    active_since: f32,
    last_wait_log: f32,
}

impl LoadingScreen {
    /// Whether the opaque loading backdrop currently covers the frame — the world camera renders
    /// under it (pipeline warm-up), never behind the glue screens (decision 0540).
    pub(crate) fn covering(&self) -> bool {
        self.active
    }

    /// Raise (or re-arm) the screen for a fresh load. `awaiting_snap` marks a raise whose
    /// destination snap is still in flight; `map` is the destination when the edge knows it.
    fn raise(&mut self, reason: &str, awaiting_snap: bool, map: Option<u32>, now: f32) {
        self.active = true;
        self.blackout = false;
        self.awaiting_snap = awaiting_snap;
        self.pending_map = map;
        self.ready_frames = 0;
        self.displayed = 0.0; // restart the fill for the new load
        self.active_since = now;
        self.last_wait_log = now;
        info!("loading screen: up ({reason}, map {map:?})");
    }
}

// UI entity markers.
#[derive(Component)]
struct LoadingRoot;
#[derive(Component)]
struct LoadingBackdrop;
#[derive(Component)]
struct LoadingBarFill;
#[derive(Component)]
struct LoadingBarBorder;

pub(crate) struct LoadingScreenPlugin;

impl Plugin for LoadingScreenPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LoadingScreen>()
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
                    LoadingBarBorder,
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

/// Per-frame: observe the lifecycle edges, run the trigger/clear state machine, resolve backdrop
/// art, and push the progress fraction into the bar.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn drive_loading_screen(
    mut screen: ResMut<LoadingScreen>,
    progress: Res<WorldLoadProgress>,
    current_map: Option<Res<CurrentMap>>,
    maps: Option<Res<MapCatalogRes>>,
    screens: Option<Res<LoadingScreenCatalogRes>>,
    mut assets: Option<ResMut<WorldAssets>>,
    mut images: ResMut<Assets<Image>>,
    mut root: Query<&mut Visibility, (With<LoadingRoot>, Without<LoadingBackdrop>)>,
    mut backdrop: Query<(&mut ImageNode, &mut Visibility), With<LoadingBackdrop>>,
    mut fill: Query<&mut Node, With<LoadingBarFill>>,
    mut bar_vis: Query<
        &mut Visibility,
        (
            Or<(With<LoadingBarFill>, With<LoadingBarBorder>)>,
            Without<LoadingRoot>,
            Without<LoadingBackdrop>,
        ),
    >,
    player: Option<Res<crate::player::Player>>,
    time: Res<Time>,
    // The 0837 warm pass: the cover holds until the menagerie's pipelines have compiled — the
    // point of the cover is that NOTHING first-sight-compiles after it lifts.
    warm: Res<crate::pipe_warm::WarmPass>,
    // The lifecycle edges (decisions 0737/0738), all observed from what the net bridge already
    // publishes — this module owns the whole state machine, nothing else is instrumented for it.
    edges: (
        Res<State<crate::char_select::ClientState>>,
        MessageReader<crate::net::EnteredWorldMessage>,
        MessageReader<crate::net::WorldportMessage>,
        MessageReader<crate::net::TeleportMessage>,
        MessageReader<crate::net::LoggedOutMessage>,
        Option<Res<crate::net::PendingTransfer>>,
        Option<Res<crate::char_select::Roster>>,
    ),
) {
    let (state, mut entered, mut worldports, mut teleports, mut logouts, transfer, roster) = edges;
    let now = time.elapsed_secs();
    let map_id = current_map.as_ref().map(|m| m.0);
    // The physics hold releases on the same residency signal that clears this screen (decision
    // 0737 — never on ground contact), so waiting for it costs nothing and guarantees the safe
    // order: the body's world is live before the reveal, never a reveal of a body about to drop.
    let player_settling = player.as_ref().is_some_and(|p| p.settling);

    // --- The edges. ---
    // A real glue entry (not 0065's seamless in-world reconnect, which must stay seamless): the
    // same frame `enter_on_connected` flips the state that tears the glue down, this raise puts
    // the cover up — raise and teardown are atomic by construction. The destination snap
    // (`SMSG_LOGIN_VERIFY_WORLD`) is still a server character-load away; `awaiting_snap` holds the
    // screen across that gap. Art resolves at once from the roster's own `Character.map`.
    if entered.read().next().is_some() && *state.get() != crate::char_select::ClientState::InWorld {
        let map = roster.as_ref().and_then(|r| r.pending_map());
        screen.raise("world entry", true, map, now);
    }
    // A portal walk-in (`SMSG_TRANSFER_PENDING`, no transport): the server is about to unload us
    // and the `SMSG_NEW_WORLD` snap follows after its own load — cover now, like the reference.
    // Transport crossings (0455) keep riding visibly until their worldport lands below.
    if let Some(t) = transfer.as_ref().filter(|t| t.is_changed()) {
        match &t.0 {
            Some(info) if info.transport_entry.is_none() => {
                screen.raise("transfer pending", true, Some(info.map_id), now);
            }
            // The latch cleared with no snap in flight = `SMSG_TRANSFER_ABORTED` (a worldport
            // clears it too, but that path also lands below this frame): stop awaiting, and the
            // still-resident old world clears the screen through the ordinary debounce.
            None => screen.awaiting_snap = false,
            Some(_) => {}
        }
    }
    // The snap: a worldport IS a fresh cross-map load (raise covers server-initiated ports that
    // had no preceding edge); a same-map teleport just ends any awaited snap — whether it needs a
    // screen at all is the backstop's call (a summon across the room shouldn't flash one).
    for w in worldports.read() {
        screen.raise("worldport", false, Some(w.map_id), now);
    }
    if teleports.read().next().is_some() {
        screen.awaiting_snap = false;
        screen.pending_map = None;
    }
    // Logout (decision 0738): the same frame's Net stage despawned the avatar and tore the world
    // down, but `CharSelect` only applies at the NEXT frame's state transition — cover the dead
    // world with plain black until the glue owns the screen. No art, no bar: the ref's
    // world→glue swap is a black cut, not a loading screen.
    if logouts.read().next().is_some() {
        screen.active = true;
        screen.blackout = true;
        screen.awaiting_snap = false;
        screen.pending_map = None;
        screen.ready_frames = 0;
        info!("loading screen: blackout (logout)");
    }
    if screen.blackout && *state.get() != crate::char_select::ClientState::InWorld {
        // The glue is up (it renders above this root); the cover's job is done.
        screen.blackout = false;
        screen.active = false;
    }

    // --- Backstop trigger: the ground under the view focus isn't resident and nothing raised us
    // (a far same-map `.tele`, or any path with no edge). Normal streaming keeps the focus tile
    // resident, so this never fires while walking.
    //
    // **In world only** (decision 0777). It used to fire at boot as well, and that was load-bearing
    // by accident: a raised screen keeps the world camera active (0540), which is what compiled the
    // world's pipelines behind the glue. But it only worked because the world was being streamed
    // behind the glue in the first place — with no world to load there, the same trigger would put
    // an invisible screen up forever against residency that never arrives. The warm-up is not lost,
    // it MOVED: the world now loads under the cover this same function raises on world entry, which
    // is where a warm-up belongs. ---
    if !screen.active
        && !progress.focus_resident
        && *state.get() == crate::char_select::ClientState::InWorld
    {
        screen.raise("focus not resident", false, None, now);
    }

    // --- Clear: the destination snap has landed, the scene is presentable (tiles + focus
    // placements + colliders — [`WorldLoadProgress::presentable`]), and the physics hold is done,
    // sustained a few frames. ---
    if screen.active {
        if progress.is_ready()
            && progress.presentable()
            && !screen.awaiting_snap
            && !player_settling
            && warm.satisfied()
        {
            screen.ready_frames += 1;
            if screen.ready_frames >= CLEAR_AFTER_READY_FRAMES {
                screen.active = false;
                info!(
                    "loading screen: cleared ({}/{} resident, {:.1}s)",
                    progress.ready,
                    progress.total,
                    now - screen.active_since
                );
            }
        } else {
            screen.ready_frames = 0;
            // The wait instrument (0737): a screen up past the threshold names the blocking term.
            if now - screen.active_since > WAIT_LOG_AFTER
                && now - screen.last_wait_log >= WAIT_LOG_EVERY
            {
                screen.last_wait_log = now;
                info!(
                    "loading screen: waiting {:.1}s — {}/{} resident, {} placements pending, \
                     {} colliders pending, awaiting_snap={}, settling={}, warm={}",
                    now - screen.active_since,
                    progress.ready,
                    progress.total,
                    progress.placements_pending,
                    progress.colliders_pending,
                    screen.awaiting_snap,
                    player_settling,
                    warm.satisfied(),
                );
            }
        }
    }

    // --- Visibility. The root is the cover; the backdrop art + bar hide under blackout (0738's
    // plain black cut — the root's black background IS the frame then). ---
    if let Ok(mut vis) = root.single_mut() {
        *vis = if screen.active {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
    let content_vis = if screen.blackout {
        Visibility::Hidden
    } else {
        Visibility::Inherited
    };
    if let Ok((_, mut vis)) = backdrop.single_mut() {
        if *vis != content_vis {
            *vis = content_vis;
        }
    }
    for mut vis in &mut bar_vis {
        if *vis != content_vis {
            *vis = content_vis;
        }
    }
    if !screen.active || screen.blackout {
        return;
    }

    // --- Backdrop art (resolve via the FK chain; cached by path). Before the snap lands the
    // destination is `pending_map` (the roster/transfer told us); after it, `CurrentMap`. ---
    let art_map = screen.pending_map.or(map_id);
    if let (Some(map_id), Some(maps), Some(screens), Some(assets)) =
        (art_map, maps.as_ref(), screens.as_ref(), assets.as_mut())
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
            if let (Some(handle), Ok((mut img, _))) = (handle, backdrop.single_mut()) {
                if img.image != handle {
                    img.image = handle;
                    img.color = Color::WHITE; // reveal the art (was tinted black)
                }
            }
        }
    }

    // --- Progress bar: monotonic fill, reveals left→right, width = progress·FILL_MAX_WIDTH.
    // While the snap is still in flight the residency being published is the OLD location's
    // (fully resident) — hold the bar at zero until the destination's own numbers exist. ---
    if !screen.awaiting_snap {
        screen.displayed = screen.displayed.max(progress.fraction());
    }
    let frac = screen.displayed;
    if let Ok(mut node) = fill.single_mut() {
        node.width = Val::Percent(frac * FILL_MAX_WIDTH * 100.0);
    }
}
