//! `benilla` — a from-scratch World of Warcraft 1.12.1 client on Bevy, talking to a local vmangos server.
//!
//! Opens the vanilla patch chain from `$WOW_DATA` (default `WoW/Data`) and streams the world around the
//! player through Bevy's `AssetServer` (the `benilla-assets` `mpq://` pipeline): ADT terrain tiles within
//! `$WOW_TILE_RADIUS` — their splat-blended ground (tiling `$WOW_TEX_TILES`), doodads/WMOs, water, and
//! ground clutter — plus the avian colliders the character controller walks on. Lit by a time-of-day WoW
//! lighting model (`Light.dbc` sampled against the server clock) with a sky dome, sun/moon discs, and
//! distance fog; a faithful `EffectGlow` bloom on top.
//!
//! In parallel a background thread ([`net`]) logs in (`$WOW_USER`/`$WOW_PASS`/`$WOW_HOST`, default
//! `one`/`pone`/`localhost`), enters the world, and streams object updates. NPCs and GameObjects
//! render as their real models (resolved from the display id via the creature/GameObject catalogs); other
//! players stay cyan cubes, and our own avatar is blue until we take third-person control of it. If the
//! server is down, the world still renders.
//!
//! Controls: WASD walks the avatar (Ctrl sprints); right-drag turns it, left-drag orbits the camera
//! (both hide/freeze the cursor while held), scroll wheel zooms; `F` toggles free-fly (then WASD flies
//! the camera with Space up / C down, Ctrl boost).

mod area;
mod area_trigger;
mod assets;
mod bgwin;
mod billboard;
mod blob_shadow;
mod bowstring;
mod capture;
mod char_create;
mod char_select;
mod chat_bubble;
mod clouds;
mod clutter;
mod collision;
mod combat_text;
mod cooldowns;
mod creature_anim;
mod cursor;
mod dbg_trace;
mod death;
mod debug_panel;
mod decal;
mod doodad_anim;
mod entities;
mod entity_shade;
mod ffx_glow;
mod glue;
mod glue_strings;
mod go_anim;
mod go_templates;
mod ground_fx;
mod interact;
mod interior;
mod items;
mod lighting;
mod liquid;
mod loading_screen;
mod login;
mod map_proj;
mod mesh_tag;
mod minimap;
mod model_fade;
mod model_render;
mod modkeys;
mod nameplates;
mod names;
mod net;
mod npc_text;
mod particles;
mod pending_item_ops;
mod perf;
mod player;
mod portrait;
mod preflight;
mod probe_shield;
mod quest_markers;
mod raid_marks;
mod ribbons;
mod rig_palette;
mod schedule;
mod sky;
mod sky_order;
mod smart_rect;
mod sound;
mod sun;
mod target;
mod terrain;
mod terrain_stream;
mod textinput;
mod thread_qos;
mod transport;
mod ui_action;
mod ui_aura;
mod ui_bank;
mod ui_cast;
mod ui_char;
mod ui_chat;
mod ui_craft;
mod ui_duel;
mod ui_gamma;
mod ui_gossip;
mod ui_hide;
mod ui_inspect;
mod ui_item_text;
mod ui_items;
mod ui_logout;
mod ui_loot;
mod ui_loot_roll;
mod ui_mail;
mod ui_merchant;
mod ui_net;
mod ui_party;
mod ui_pass;
mod ui_quest;
mod ui_quest_log;
mod ui_script;
mod ui_session;
mod ui_shapeshift;
mod ui_social;
mod ui_spellbook;
mod ui_talent;
mod ui_taxi;
mod ui_text;
mod ui_tooltip;
mod ui_trade;
mod ui_tradeskill;
mod ui_trainer;
mod ui_unit;
mod ui_world_map;
mod view;
mod vplates;
mod water_fx;
mod wdl;
mod weather;
mod wmo_portal;
mod world_map;
mod world_state;

use assets::AssetPlugin;
use avian3d::prelude::*;
use bevy::prelude::*;
use billboard::BillboardPlugin;
use blob_shadow::BlobShadowPlugin;
use bowstring::BowstringPlugin;
use clouds::CloudsPlugin;
use clutter::ClutterPlugin;
use creature_anim::CreatureAnimPlugin;
use cursor::CursorPlugin;
use debug_panel::DebugPanelPlugin;
use doodad_anim::DoodadAnimPlugin;
use entities::EntitiesPlugin;
use entity_shade::EntityShadePlugin;
use interact::InteractPlugin;
use interior::InteriorPlugin;
use lighting::LightingPlugin;
use liquid::LiquidPlugin;
use loading_screen::LoadingScreenPlugin;
use net::NetPlugin;
use particles::ParticlePlugin;
use perf::PerfPlugin;
use player::PlayerPlugin;
use portrait::PortraitPlugin;
use quest_markers::QuestMarkersPlugin;
use ribbons::RibbonPlugin;
use schedule::SchedulePlugin;
use sky::SkyPlugin;
use sound::SoundPlugin;
use sun::SunPlugin;
use target::TargetPlugin;
use terrain::{TerrainMaterial, WowModelMaterial};
use terrain_stream::TerrainPlugin;
use textinput::TextInputPlugin;
use transport::TransportPlugin;
use ui_action::UiActionPlugin;
use ui_aura::UiAuraPlugin;
use ui_bank::UiBankPlugin;
use ui_cast::UiCastPlugin;
use ui_char::UiCharPlugin;
use ui_chat::UiChatPlugin;
use ui_craft::UiCraftPlugin;
use ui_duel::UiDuelPlugin;
use ui_gossip::UiGossipPlugin;
use ui_item_text::UiItemTextPlugin;
use ui_items::UiItemsPlugin;
use ui_logout::UiLogoutPlugin;
use ui_loot::UiLootPlugin;
use ui_loot_roll::UiLootRollPlugin;
use ui_mail::UiMailPlugin;
use ui_merchant::UiMerchantPlugin;
use ui_net::UiNetPlugin;
use ui_party::UiPartyPlugin;
use ui_pass::PlayerUiPlugin;
use ui_quest::UiQuestPlugin;
use ui_quest_log::UiQuestLogPlugin;
use ui_script::UiScriptPlugin;
use ui_shapeshift::UiShapeshiftPlugin;
use ui_social::UiSocialPlugin;
use ui_spellbook::UiSpellbookPlugin;
use ui_talent::UiTalentPlugin;
use ui_taxi::UiTaxiPlugin;
use ui_text::UiTextPlugin;
use ui_tooltip::UiTooltipPlugin;
use ui_trade::UiTradePlugin;
use ui_tradeskill::UiTradeSkillPlugin;
use ui_trainer::UiTrainerPlugin;
use ui_unit::UiUnitPlugin;
use wdl::WdlPlugin;
use weather::WeatherPlugin;
use wmo_portal::WmoPortalPlugin;
use world_map::WorldMapPlugin;

/// Anchor the loaded terrain block on the Human start (Northshire), where `one`/`One`
/// logs in — so the player sits in the middle of the block instead of Stormwind's edge.
const SPAWN_XY: (f32, f32) = (-8949.95, -132.49);

fn main() -> AppExit {
    // `WOW_CAPTURE=list` just prints the harness scenario names (the source of truth `scripts/visual.sh`
    // reads) and exits before any window/asset setup.
    if std::env::var("WOW_CAPTURE").as_deref() == Ok("list") {
        capture::print_scenario_names();
        return AppExit::Success;
    }

    let mut app = App::new();

    // Visual A/B harness (decision 0008): with `$WOW_CAPTURE` set, the app runs a deterministic,
    // server-less capture (net off so no NPCs stream in nondeterministically) and exits. See `capture`.
    let capturing = capture::scenario_active();
    // Any instrumented run — captures AND the live-probe fleet — opens in the background so it
    // never fights the director's screen (decision 0703; `WOW_BG` overrides). See `bgwin`.
    let background = bgwin::background_run();
    if capturing {
        // Ground clutter scatters with per-run randomness, so disable it for byte-stable baselines
        // — clutter isn't what the lighting rework validates, and the regression diff must not be
        // masked by grass wobble. Set before plugins build so `ClutterConfig::from_env` reads it.
        // It is not the only source of per-run drift, though it was long documented as such: the
        // other is the frame clock itself, frozen in `capture` (decision 0723).
        std::env::set_var("WOW_CLUTTER_DENSITY", "0");
    }

    // The `mpq://` asset source must be registered BEFORE `AssetPlugin` (inside `DefaultPlugins`)
    // builds. Reads the same `$WOW_DATA` dir as the `WorldAssets` foundation; on failure the source is
    // simply absent and the AdtTile-pipeline loads fail gracefully (the terrain just doesn't appear).
    let data_dir =
        std::path::PathBuf::from(std::env::var("WOW_DATA").unwrap_or_else(|_| "WoW/Data".into()));
    if let Err(e) = benilla_assets::register_mpq_source(&mut app, &data_dir) {
        eprintln!("benilla-assets: mpq:// source unavailable ({e:#})");
    }

    app.add_plugins(
        DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "benilla".into(),
                    // UI-fixture captures shrink the window so the docked panel fills the frame —
                    // the capture is the look-pass instrument and the window is its subject. The
                    // action bar is the exception: it spans 1024px + 128px end caps along the screen
                    // bottom, so it gets a wide, short window instead of the tall default (else the
                    // caps crop). The vplates scenario pins the 1:1 gx window: at 1024×768 one gx
                    // unit = 1280 px, so the plate must land at the border texture's native
                    // 128×32 — directly diffable against the decoded BLP. Sized per-capture off
                    // WOW_CAPTURE.
                    resolution: if capturing
                        && std::env::var("WOW_CAPTURE_UI").as_deref() == Ok("1")
                    {
                        match std::env::var("WOW_CAPTURE").as_deref() {
                            Ok("ui-actionbar") => UVec2::new(1300, 260),
                            Ok("vplates") => UVec2::new(1024, 768),
                            // The director's small-window shape: short enough that the action bar
                            // strip overlaps the chat edit box rows — the overlap is the subject.
                            Ok("ui-chatedit") => UVec2::new(566, 377),
                            // The fullscreen map's chrome is a centered 1024×768 block; a hair of
                            // margin shows the blackout doing its job.
                            Ok("ui-worldmap") => UVec2::new(1100, 800),
                            _ => UVec2::new(640, 700),
                        }
                        .into()
                    } else {
                        UVec2::new(1600, 900).into()
                    },
                    // `$WOW_NOVSYNC=1`: uncap presentation so a headless FPS-journal run measures
                    // true frame cost, not the vsync ceiling — the same uncap the capture probe
                    // flips mid-run, available from boot for non-capture probes (perf triage at
                    // the glue screens, where no capture scenario runs).
                    present_mode: if std::env::var("WOW_NOVSYNC").as_deref() == Ok("1") {
                        bevy::window::PresentMode::AutoNoVsync
                    } else {
                        bevy::window::PresentMode::default()
                    },
                    // An instrumented run must never fight the director's screen (decision 0703).
                    // Focused, it steals the keyboard — on 2026-07-19 a login-shot run swallowed
                    // their keystrokes out of another app and typed them into the account box,
                    // which is also how that capture lost the bare caret it was taken to measure.
                    // So every probe/capture/regression run (`bgwin`) opens unfocused; an ordinary
                    // `cargo run` is unaffected and focuses normally.
                    //
                    // A background run is BORN at `AlwaysOnBottom` (`kCGNormalWindowLevel - 1`)
                    // so it can never flash over their work on the way up — winit raises a new
                    // window twice before our first frame runs, and at the normal level that
                    // showed as ~half a second of probe window on top (measured). But it does not
                    // STAY there: that level is a cage, and 0703 leaving it on for the whole run
                    // is why an instrumented window could never be raised again however hard you
                    // clicked it. `BgWinPlugin` promotes it back to Normal the moment the launch
                    // settles (decision 0709), and owns the app-level half — winit's forced macOS
                    // app activation — as well.
                    focused: !background,
                    window_level: if background {
                        bevy::window::WindowLevel::AlwaysOnBottom
                    } else {
                        bevy::window::WindowLevel::Normal
                    },
                    ..default()
                }),
                ..default()
            })
            // Quiet wgpu/naga; our own crates stay at info. (This filter once also quieted the
            // warcraft-rs parsers `wow_m2`/`wow_blp` — retired in-repo by decision 0021; the
            // in-repo parsers don't log, so those entries were dead and are gone.)
            .set(bevy::log::LogPlugin {
                filter: "wgpu=error,naga=warn".into(),
                ..default()
            })
            // Asset streaming is this client's load bottleneck: every M2/WMO/BLP read decompresses
            // from MPQ and parses **synchronously** on Bevy's IO task pool, and the AssetServer runs
            // *all* loads there. Bevy's default caps that pool at 4 threads, so a teleport into a
            // dense area — terrain + WMOs + their doodad props, all bursting at once — saturates it
            // and the net-driven NPC/GameObject models queue behind the flood. Give IO more of the
            // box (it sits idle when not streaming); the world-render path is GPU/IO-bound, not
            // compute-bound, so trading some compute threads for streaming throughput is the right call.
            // Thread QoS (decision 0609): Bevy's workers spawn at default QoS — the same Darwin
            // scheduling class as rustc or an OBS encoder — so under a background build the
            // frame's own threads queue behind the compiler for P-core time. Promote them at
            // spawn: compute runs this frame's systems (user-interactive); IO/async-compute feed
            // upcoming frames (user-initiated — above default, below the frame itself). The
            // render thread has no spawn hook; `ThreadQosPlugin` promotes it from inside.
            .set(TaskPoolPlugin {
                task_pool_options: TaskPoolOptions {
                    io: bevy::app::TaskPoolThreadAssignmentPolicy {
                        min_threads: 2,
                        max_threads: 8,
                        percent: 0.5,
                        on_thread_spawn: Some(std::sync::Arc::new(|| {
                            thread_qos::promote_current_thread(thread_qos::QosClass::UserInitiated)
                        })),
                        on_thread_destroy: None,
                    },
                    async_compute: bevy::app::TaskPoolThreadAssignmentPolicy {
                        on_thread_spawn: Some(std::sync::Arc::new(|| {
                            thread_qos::promote_current_thread(thread_qos::QosClass::UserInitiated)
                        })),
                        ..TaskPoolOptions::default().async_compute
                    },
                    compute: bevy::app::TaskPoolThreadAssignmentPolicy {
                        on_thread_spawn: Some(std::sync::Arc::new(|| {
                            thread_qos::promote_current_thread(
                                thread_qos::QosClass::UserInteractive,
                            )
                        })),
                        // `WOW_THREADS=1` serialises the systems that run this frame. Not a
                        // performance dial — a **diagnostic**: a defect that alternates frame to
                        // frame with no camera, geometry or draw-order change behind it is what an
                        // unordered write between two systems looks like, and that is separable from
                        // every other cause only by taking the concurrency away. Anything that
                        // survives `WOW_THREADS=1` is not a race.
                        max_threads: match std::env::var("WOW_THREADS").ok().as_deref() {
                            Some("1") => 1,
                            _ => TaskPoolOptions::default().compute.max_threads,
                        },
                        ..TaskPoolOptions::default().compute
                    },
                    ..default()
                },
            })
            // Sound is kira behind our own mixer seam (decision 0070); Bevy's AudioPlugin would
            // only open a second, never-used OS output stream at startup. Off (0530). Its
            // rodio/cpal stack still compiles in via bevy's default feature — trimming the
            // feature set is a separate, wider call.
            .disable::<bevy::audio::AudioPlugin>(),
    )
    .add_plugins(thread_qos::ThreadQosPlugin)
    // The app-side half of background instrumented runs: undo winit's forced macOS app
    // activation so a probe/capture launch never yanks focus off the director's screen. The
    // window-side half (unfocused + always-on-bottom) is in the `Window` above. Decision 0703.
    .add_plugins(bgwin::BgWinPlugin)
    .add_plugins(MaterialPlugin::<TerrainMaterial>::default())
    .add_plugins(MaterialPlugin::<WowModelMaterial>::default())
    // Physics (avian3d): collider storage + broadphase BVH + shape-casts for the character
    // controller (decision 0009). The streamed terrain/placement entities carry `Collider`s; the
    // player drives `MoveAndSlide` against them. WoW gravity (19.29 yd/s², binary-derived — now a
    // feel knob, not a fidelity target) replaces avian's 9.81 default.
    .add_plugins(PhysicsPlugins::default())
    // One solver substep, not avian's 6: the world has NO dynamic bodies (static terrain,
    // kinematic transports/attachments; the player is a shape-cast controller), so the substep
    // loop's contact/joint solving iterates over nothing — and kinematic motion integrates
    // exactly (constant velocity, no forces) at any substep count. Six substeps were pure
    // fixed-tick schedule overhead (~10 substep-schedule runs per frame on the idle-floor
    // ledger). Revisit if a dynamic body ever enters the world.
    .insert_resource(avian3d::prelude::SubstepCount(1))
    .insert_resource(Gravity(Vec3::NEG_Y * 19.291_105))
    // The per-frame world-transition ordering (Input → Stream → Present) the loading screen relies
    // on to cover a teleport the same frame it happens. See `schedule.rs`.
    .add_plugins(SchedulePlugin)
    // The faithful view distance (`farclip`) — one source of truth for the wall + the per-object
    // cull (and, post-split, the stream radius). See `view.rs`.
    .init_resource::<view::ViewDistance>()
    .add_plugins(DebugPanelPlugin)
    // World-interaction foundation: mouseover picking + object identity (the debug inspector reads it
    // now; hover tooltips / contextual cursor / targeting will later).
    .add_plugins(InteractPlugin)
    // M2 billboard cards (glow halos, chains) — faced to the camera each frame.
    .add_plugins(BillboardPlugin)
    // The owned skin palette (decision 0720): every skinned rig's joint matrices, computed by
    // us and skinned in wow_model.wgsl — Bevy's SkinnedMesh lane is fully replaced.
    .add_plugins(rig_palette::plugin)
    .add_plugins(BowstringPlugin)
    .add_plugins(QuestMarkersPlugin)
    // Frame-time HUD + diagnostics — the performance standard.
    // After DebugPanelPlugin so the egui plugin/context it sets up already exists. Toggle: P.
    .add_plugins(PerfPlugin)
    // Foundation: opens the patch chain + inserts WorldAssets/RenderConfig (AssetSet::Open), which
    // every other subsystem's startup runs after.
    .add_plugins(AssetPlugin)
    // World-map state (Map.dbc catalog + CurrentMap), loaded right after the chain opens — the
    // terrain/WDL streamers, loading screen, and lighting all key off it.
    .add_plugins(WorldMapPlugin)
    // Time-of-day lighting: sun + WoW shader colors, sky background, per-frame update→apply.
    .add_plugins(LightingPlugin)
    // Sky dome: the Light.dbc gradient backdrop (camera-centred), driven by the same lighting.
    .add_plugins(SkyPlugin)
    // Cloud coverage: the reference's procedural field — glare occlusion (occ1) + the visible layer.
    .add_plugins(CloudsPlugin)
    // Weather: the SMSG_WEATHER state machine driving the storm light-blend + precipitation
    // (decision 0310). Lighting reads its densities `.after(WeatherTick)`.
    .add_plugins(WeatherPlugin)
    // Sun disc + glow halo: the celestial sprites WoW draws at the sun (RE'd from CSky::Render).
    .add_plugins(SunPlugin)
    // Interior lighting classifier: lights M2 entities (GameObjects/NPCs/other players) standing inside a
    // WMO room off the baked floor colour, day/night-independent (the streamer fills its volume registry).
    .add_plugins(InteriorPlugin)
    .add_plugins(EntityShadePlugin)
    // WMO portal visibility: per-frame, decides which of a building's groups are reachable through
    // portals from the camera's group, so the Stormwind cathedral culls from the Trade District. Only
    // computes the PVS; the Visibility authority (DebugPanelPlugin) applies it (decisions 0025/0031).
    .add_plugins(WmoPortalPlugin)
    // Streamed world entities: cube assets + display catalogs at startup, sync each frame.
    .add_plugins(EntitiesPlugin)
    // Creature animation: pick Stand/Walk/Run from each creature's movement state each frame (Milestone C).
    .add_plugins(CreatureAnimPlugin)
    // The unit blob shadow: the dark ground oval under every unit, sized from the playing
    // animation's box (the byte-verified law — wow-re unit-blob-shadow RE), on the same
    // surface-decal projector as the selection ring.
    .add_plugins(BlobShadowPlugin)
    // Doodad animation (decision 0130): placed M2s loop their first sequence + global sequences,
    // gated to drawn instances.
    .add_plugins(DoodadAnimPlugin)
    // GameObject animation (decision 0242): net-streamed GObjects (doors/chests) play an M2 sequence
    // on GAMEOBJECT_STATE change — the state-machine sibling of the doodad idle loop above.
    .add_plugins(go_anim::plugin)
    // Ground clutter: the GroundEffect catalog + the lazy per-chunk build lifecycle, owned
    // independently of the terrain streamer (so the streamer can be swapped). Whichever streamer is
    // active scatters into the ClutterChunks this builds.
    .add_plugins(ClutterPlugin)
    // Terrain streaming (the AdtTile pipeline) is added after this chain — see below.
    // Distant low-detail terrain (WDL): the fogged horizon hills beyond the streamed tiles.
    .add_plugins(WdlPlugin)
    // Liquid: animated lake/river/ocean water surfaces (MCLQ), spawned with their terrain tile.
    .add_plugins(LiquidPlugin)
    // Particle emitters: the additive flames/glows of campfires, torches, braziers (decision 0014).
    .add_plugins(ParticlePlugin)
    // Water foam decals (CWater0Ripple wake/ring/step-in splash) — the record model, rebuilt from
    // the byte RE + two reference-trace reconstructions (decision 0264).
    .add_plugins(water_fx::WaterFxPlugin)
    .add_plugins(ffx_glow::FfxGlowPlugin)
    .add_plugins(RibbonPlugin)
    // Avatar + camera + input.
    .add_plugins(PlayerPlugin)
    // The real client's hardware mouse cursor (native NSCursor on macOS).
    .add_plugins(CursorPlugin)
    // Stuck-modifier reconciliation: macOS system shortcuts (⇧⌘5) swallow modifier releases
    // without a focus loss, wedging every bare-key binding (decision 0606).
    .add_plugins(modkeys::ModKeysPlugin)
    // Net↔ECS bridge: spawns the world thread, exposes the snapshot + writer resources. In capture
    // mode the IO thread is skipped (`connect: false`) so the scene is deterministic.
    .add_plugins(NetPlugin {
        connect: !capturing,
    })
    // The death arc (decision 0308): the wire-fed death stores + the root/water-walk ack messages.
    .add_plugins(death::DeathPlugin)
    // The shared glue vocabulary both pre-world screens stand on (decision 0465): the ADD-mode UI
    // material, the client-data art set, the GlueStrings table.
    .add_plugins(glue::GluePlugin)
    // The glue layer (decision 0193): the ClientState machine + the character-select screen
    // that answers the parked IO thread's pick. Captures boot straight InWorld (no net, no picker).
    .add_plugins(char_select::CharSelectPlugin {
        start_in_world: capturing,
    })
    // The login screen (decision 0539): the faithful AccountLogin glue + the credential policy
    // that answers the IO thread's pre-logon park.
    .add_plugins(login::LoginPlugin)
    // The character-creation screen + its live preview booth (decision 0423).
    .add_plugins(char_create::CharCreatePlugin)
    // The session preflight (decision 0649): one banner per world entry naming the body we logged
    // into, and loud warnings for the states — dead/ghost, GM mode, server-blocked movement — that
    // silently invalidate a session's readings. Never env-gated; a warning nobody switches on isn't one.
    .add_plugins(preflight::PreflightPlugin)
    // The probe shield (decision 0677): a body on a probe account is put into vmangos's `.cheat
    // god` on every world entry — damage clamps at 1 hp instead of killing — and GM mode is turned
    // OFF, because the shield replaces the only reason it was ever on. Inert on any other account.
    .add_plugins(probe_shield::ProbeShieldPlugin)
    // Audio: the delegated mixer + WoW's owned selection layer (decision 0070).
    .add_plugins(SoundPlugin)
    // Targeting: left-click a unit to select it (→ CMSG_SET_SELECTION) + draw its ground ring.
    .add_plugins(TargetPlugin)
    .add_plugins(TransportPlugin)
    // Faithful world-load splash + progress bar on startup + cross-map teleport (the load latency
    // streaming can't hide); per-map art via the Map.dbc→LoadingScreens.dbc→BLP chain.
    .add_plugins(LoadingScreenPlugin)
    // The player-UI quad pass (decision 0068 §2): its own composited-above-the-world,
    // below-the-egui-dev-overlays camera + sorted-quad renderer. `$WOW_UI_DEMO=1` seeds a proof scene.
    .add_plugins(PlayerUiPlugin)
    // The HUD minimap (decision 0203 phase 1): fills the `<Minimap>` widget's extracted hole with
    // the streamed tile window + mask + player arrow, and feeds the zone text.
    .add_plugins(minimap::MinimapPlugin)
    // The shared AreaTable catalog + the ZONE_CHANGED event family / zone-text host globals
    // behind GetZoneText & co. (the zone-entry splash arc, decision 0287).
    .add_plugins(area::AreaPlugin)
    // The `AreaTrigger.dbc` volumes + the per-frame containment check that reports walking into
    // one (`CMSG_AREATRIGGER`) — the client's whole part in portals, instance entrances and
    // explore objectives; the server owns what each trigger means.
    .add_plugins(area_trigger::AreaTriggerPlugin)
    .add_plugins(ui_world_map::WorldMapUiPlugin)
    // The glyph atlas (client TTFs -> baked bitmap) `ui_script`'s extraction draws `FontString`
    // regions through. Loads at Startup, after the asset chain opens (decision 0068 §2).
    .add_plugins(UiTextPlugin)
    // Unit-frame portraits: the token -> off-screen-baked-face bridge the UI extract samples for a
    // `SetPortraitTexture`-bound region (the modern high-res 2D model bake).
    .add_plugins(PortraitPlugin)
    .add_plugins((TextInputPlugin, UiScriptPlugin))
    // The unit snapshot + event feed (decision 0068 §3): pushes ECS game state into the VM as the
    // plain data the `Unit*` bindings read, and fires the matching WoW events.
    .add_plugins(UiUnitPlugin)
    .add_plugins(UiPartyPlugin)
    // Duels (decision 0633): the wire session, the client-side countdown tick, the four Era
    // events, and the accept/cancel/challenge intents.
    .add_plugins(UiDuelPlugin)
    // Leaving (decision 0674): the game menu's Logout/Exit Game — the request, the server's
    // 20-second answer narrated as the CAMP/QUIT countdown, and the process exit.
    .add_plugins(UiLogoutPlugin)
    .add_plugins(UiSocialPlugin)
    .add_plugins(UiTooltipPlugin)
    // The character-window feed (decision 0208): the combat-stats/inventory snapshots + events
    // the paper doll reads, and the paper-doll booth's yaw mirror.
    .add_plugins(UiCharPlugin)
    // The inspect feed (decision 0631): another player's equipment off their PUBLIC visible-item
    // entries, plus the "inspect" booth's unit + yaw. Right after the character feed it mirrors.
    .add_plugins(ui_inspect::InspectUiPlugin)
    .add_plugins(UiActionPlugin)
    // The aura feed (decisions 0255/0257): the player's insertion-ordered buff/debuff cache + the
    // self-only durations, pushed as the data the `UnitAura` bindings read; fires UNIT_AURA and
    // drains the right-click cancels. After UiActionPlugin (shares its `Spells` catalog).
    .add_plugins(UiAuraPlugin)
    // The spellbook window feed (decision 0216 §8, slice 5): builds the book from
    // PlayerActions.spells through the Spell.dbc/SkillLine.dbc join and drives
    // SpellBookFrame.xml's snapshot + cast-drain seam — the spell SOURCE for the cursor payload
    // arc (bags/doll/bars/book). After UiActionPlugin (shares its `Spells` resource + the cast
    // tail `send_spell_cast`).
    .add_plugins(UiSpellbookPlugin)
    // The talent window feed (decision 0304): builds the class pages from Talent.dbc × the
    // known-spell set + PLAYER_CHARACTER_POINTS, drives TalentFrame.xml through the engine's
    // talent seam, and drains learn clicks into CMSG_LEARN_TALENT. After UiActionPlugin
    // (shares its `Spells` catalog), beside the spellbook it mirrors.
    .add_plugins(UiTalentPlugin)
    // The stance/shapeshift bar feed (wow-re shapeshift-bar-api.md): builds the form list from
    // PlayerActions.spells per the byte-verified admission/order, drives StanceBar.xml through
    // the engine's shapeshift seam, and drains its clicks (cancel-if-active else cast). After
    // UiActionPlugin (shares `Spells`, the `usable` walk, and the cast tail).
    .add_plugins(UiShapeshiftPlugin)
    // The connection-telemetry feed: the averaged ping RTT behind `GetNetStats()`, which the main
    // bar's performance meter polls (decision 0658).
    .add_plugins(UiNetPlugin)
    .add_plugins(UiCastPlugin)
    // Floating combat text (decision 0137 phase 2): the WORLDTEXTSTRING law — world-anchored
    // damage numbers/outcome words projected into the UI quad pass each frame.
    .add_plugins(combat_text::CombatTextPlugin)
    // Overhead unit names (nameplates): world-billboard name text over players + NPCs.
    .add_plugins(nameplates::NameplatesPlugin)
    // Raid-target marker billboards (0434 §6): the mark icon over marked units, one line-pitch
    // above the overhead name; plated units show the plate's raid child instead.
    .add_plugins(raid_marks::RaidMarksPlugin)
    // V-key nameplates (0167): the toggled health-bar plates, a 2-D overlay replacing the
    // overhead name on plated units.
    .add_plugins(vplates::VPlatesPlugin)
    // Chat speech bubbles (0598): the over-the-head bubble a say/yell/party line spawns, the
    // plates' 2-D overlay sibling — mutually exclusive with both the plate and the name.
    .add_plugins(chat_bubble::ChatBubblePlugin)
    // TOGGLEUI (`CTRL-Z`/`Cmd-Z`): the whole quad layer goes dark — frames, minimap, plates,
    // bubbles, combat text — leaving the world and the cursor.
    .add_plugins(ui_hide::UiHidePlugin)
    .add_plugins(UiItemsPlugin)
    // The gossip window (decision 0081): fills from the net drain's GossipState and drives
    // GossipFrame.xml over the Era gossip API.
    .add_plugins(UiGossipPlugin)
    // The merchant window (decision 0081 phase 4): fills from the net drain's MerchantOpen and
    // drives MerchantFrame.xml over the Era vendor API + the money display.
    .add_plugins(UiMerchantPlugin)
    // The bank window (decision 0604): the SHOW_BANK session (BankOpen) + the purchase row;
    // the vault's slots ride the container feed as bags −1/5..=10.
    .add_plugins(UiBankPlugin)
    // The mail window (decision 0544 P1/P2): the client-side mailbox session (MailOpen), the
    // NPC-session range guard, and MailFrame.xml over the Era mail API (inbox, open-letter,
    // send tab).
    .add_plugins(UiMailPlugin)
    // Player-to-player trade (TradeFrame.xml): the two-sided trade window, driven server-side over
    // the P0 wire; the partner's portrait rides the shared "npc" booth (decision 0592 P1).
    .add_plugins(UiTradePlugin)
    // The item-text reader (ItemTextFrame.xml): right-clicked bag letters (mail-made permanent
    // copies) read in the reference reader window over the shared ask-once item-text cache.
    .add_plugins(UiItemTextPlugin)
    .add_plugins(UiTrainerPlugin)
    // The taxi map (decision 0484 phases 1-2): the SMSG_SHOWTAXINODES-fed TaxiState resource, the
    // NPC-session range guard, and the TaxiFrame.xml window feed/drain (catalogs, node
    // projection/route computation, the activate send, the UnitOnTaxi ride flag).
    .add_plugins(UiTaxiPlugin)
    .add_plugins(UiTradeSkillPlugin)
    .add_plugins(UiCraftPlugin)
    // The loot window (decision 0084): fills from the net drain's LootState and drives
    // LootFrame.xml over the Era loot API (coin + rows, paging).
    .add_plugins(UiLootPlugin)
    .add_plugins(UiLootRollPlugin)
    // The questgiver window (decision 0088): fills from the net drain's QuestGiver and drives
    // QuestFrame.xml's four sub-panels over the Era quest API.
    .add_plugins(UiQuestPlugin)
    // The quest-log window (decision 0088's deferred second slice): fills from the self player's
    // PLAYER_QUEST_LOG descriptor slots + the SMSG_QUEST_QUERY_RESPONSE template cache, and drives
    // QuestLogFrame.xml over the Era quest-log API.
    .add_plugins(UiQuestLogPlugin)
    .add_plugins(UiChatPlugin);

    // Terrain streaming: the benilla-assets `AdtTile` pipeline — streams tiles around the player through
    // the `AssetServer`, owning the terrain mesh/material/collision, doodads/WMOs, liquid, clutter, and
    // loading-screen residency.
    app.add_plugins(TerrainPlugin);

    // Register benilla-assets' loaders AFTER `AssetPlugin` (they go into the live `AssetServer`).
    benilla_assets::register_asset_loaders(&mut app);

    // The capture harness drives one deterministic screenshot then exits — added last so it observes
    // the fully-built app. Inert unless `$WOW_CAPTURE` is set.
    if capturing {
        app.add_plugins(capture::CapturePlugin);
    }
    // The LIVE probe shot (orthogonal to the harness): `WOW_LIVE_SHOT=<png>` on a NORMAL connected
    // run writes one screenshot `WOW_LIVE_SHOT_AT` seconds (default 12) after startup and keeps
    // running — the agent-side instrument for seeing a live server scene (NPCs, GameObjects, event
    // spawns) without a scenario. Pair with `WOW_USER`/`WOW_CHAR` + an outer `timeout`.
    if std::env::var("WOW_LIVE_SHOT").is_ok() {
        app.add_plugins(capture::LiveShotPlugin);
    }
    // The probe RIG (decision 0651): `WOW_RIG="tauren druid 60 gear:heal-preraid-bis"` finds-or-
    // creates that body on this slot's probe account, logs in as it, and applies level/spells/gear/
    // spec/place — the one verb that replaces the hand-assembled GM recipe every session used to
    // re-derive (see `capture::ProbeRigPlugin`).
    if std::env::var("WOW_RIG").is_ok() {
        app.add_plugins(capture::ProbeRigPlugin);
    }
    // The probe-chat one-shot: `WOW_PROBE_CHAT=".go xyz …"` sends GM/chat lines once in-world —
    // the "park the probe character anywhere" instrument (see `capture::ProbeChatPlugin`).
    if std::env::var("WOW_PROBE_CHAT").is_ok() {
        app.add_plugins(capture::ProbeChatPlugin);
    }
    // The probe-lua one-shot: `WOW_PROBE_LUA="CastSpell(…)"` runs a chunk in the live UI VM once
    // in-world — the "press the button headlessly" instrument (see `capture::ProbeLuaPlugin`).
    if std::env::var("WOW_PROBE_LUA").is_ok() {
        app.add_plugins(capture::ProbeLuaPlugin);
    }
    // The probe-key taps: `WOW_PROBE_KEY="Space@14"` presses keys once in-world — the "press
    // space headlessly" instrument for input-gated behavior (see `capture::ProbeKeyPlugin`).
    if std::env::var("WOW_PROBE_KEY").is_ok() {
        app.add_plugins(capture::ProbeKeyPlugin);
    }
    // The probe self-termination: `WOW_PROBE_EXIT_AT=<secs>` bounds any scripted live probe's
    // lifetime — its own knob, not a rider on the Lua probe (see `capture::ProbeExitPlugin`).
    if std::env::var("WOW_PROBE_EXIT_AT").is_ok() {
        app.add_plugins(capture::ProbeExitPlugin);
    }
    // The ray pick: `WOW_PICK="<x>,<y>"` names every surface along the ray through a screenshot
    // pixel, nearest first — "what is at the spot `benilla-visual hotspot` flagged, and what is
    // right behind it" (see `capture::PickProbePlugin`).
    if std::env::var("WOW_PICK").is_ok() {
        app.add_plugins(capture::PickProbePlugin);
    }
    // The render-phase census: `WOW_PHASE=<uniqueId>` reports, per frame, which phase each of one
    // placement's batches landed in and where in the draw order — the one thing every scene-side
    // instrument is blind to, namely whether a surface was submitted at all (see
    // `capture::PhaseProbePlugin`).
    if std::env::var("WOW_PHASE").is_ok() {
        app.add_plugins(capture::PhaseProbePlugin);
    }
    // The depth readback: `WOW_DEPTH="<x>,<y>"` reports what depth actually won each named pixel,
    // per frame, as a distance in yards — the link past submission that decides the pixel. Pair it
    // with `WOW_PICK` at the same pixels to turn "what won" into "whose it was" (see
    // `capture::DepthProbePlugin`).
    // `WOW_DEPTH_QUADS=<bone>…` is the same readback taken at a particle quad's OWN pixels — the
    // moving-subject form, which no hand-written pixel list can hold (see `capture::depth_probe`).
    if std::env::var("WOW_DEPTH").is_ok() || std::env::var("WOW_DEPTH_QUADS").is_ok() {
        app.add_plugins(capture::DepthProbePlugin);
    }
    // The bevy_ui node census — "who owns this rectangle" for UI outside the FrameXML quad pass
    // (see `capture::NodeProbePlugin`).
    if std::env::var("WOW_NODE_PROBE").is_ok() {
        app.add_plugins(capture::NodeProbePlugin);
    }
    // The mid-run window resize: `WOW_PROBE_RESIZE="<secs>:<W>x<H>"` — the headless fullscreen-
    // toggle stand-in for resize-reactive layout (see `capture::ProbeResizePlugin`).
    if std::env::var("WOW_PROBE_RESIZE").is_ok() {
        app.add_plugins(capture::ProbeResizePlugin);
    }
    // The particle census: `WOW_PARTICLE_CENSUS=<secs>` prints per-emitter live counts once —
    // the trace-comparable coverage number (see `capture::ParticleCensusPlugin`).
    if std::env::var("WOW_PARTICLE_CENSUS").is_ok() {
        app.add_plugins(capture::ParticleCensusPlugin);
    }
    // The entity census: `WOW_ENTITY_CENSUS=<secs>` prints per-archetype entity counts once —
    // what the resident entity count is made of (see `capture::EntityCensusPlugin`).
    if std::env::var("WOW_ENTITY_CENSUS").is_ok() {
        app.add_plugins(capture::EntityCensusPlugin);
    }
    // The melee live probe: `WOW_PROBE=melee` auto-fights the nearest enemy so the dbg-trace
    // sink can record the combat-text timeline (see `capture::ProbeMeleePlugin`).
    if std::env::var("WOW_PROBE").as_deref() == Ok("melee") {
        app.add_plugins(capture::ProbeMeleePlugin);
    }
    // The partner live probe: `WOW_PROBE=partner` auto-accepts group invites — the party arc's
    // second-client instrument (decision 0434; see `capture::ProbePartnerPlugin`).
    if std::env::var("WOW_PROBE").as_deref() == Ok("partner") {
        app.add_plugins(capture::ProbePartnerPlugin);
    }
    // The sea-crossing live probe: `WOW_PROBE=crossing` boards a cross-continent boat and reports
    // the map seam surviving — decision 0455's instrument (see `capture::ProbeCrossingPlugin`).
    if std::env::var("WOW_PROBE").as_deref() == Ok("crossing") {
        app.add_plugins(capture::ProbeCrossingPlugin);
    }
    // The taxi-flight live probe: `WOW_PROBE=taxi` opens the flight-master menu on the real wire
    // and rides Stormwind → Sentinel Hill to a measured verdict — decision 0484's end-to-end
    // instrument (see `capture::ProbeTaxiPlugin`).
    if std::env::var("WOW_PROBE").as_deref() == Ok("taxi") {
        app.add_plugins(capture::ProbeTaxiPlugin);
    }
    // The mail-arc live probe: `WOW_PROBE_MAIL=1` GM-mails the probe's own character, opens the
    // Goldshire mailbox on the real wire, and drives the inbox/take/send/delete surface through
    // the live Lua VM — decisions 0544/0548's end-to-end instrument (see `capture::ProbeMailPlugin`).
    if std::env::var("WOW_PROBE_MAIL").is_ok() {
        app.add_plugins(capture::ProbeMailPlugin);
    }
    // The bank-arc live probe: `WOW_PROBE_BANK=1` GM-hops to a pure banker, drives the whole
    // six-opcode bank wire (activate/deposit/withdraw/buy-slot/refusal) — decision 0604's
    // end-to-end instrument (see `capture::ProbeBankPlugin`).
    if std::env::var("WOW_PROBE_BANK").is_ok() {
        app.add_plugins(capture::ProbeBankPlugin);
    }
    // The cast-cancel live probe: `WOW_PROBE=castcancel` hearths and presses W mid-cast — the
    // local self-cancel's end-to-end timing instrument (see `capture::ProbeCastCancelPlugin`).
    if std::env::var("WOW_PROBE").as_deref() == Ok("castcancel") {
        app.add_plugins(capture::ProbeCastCancelPlugin);
    }
    // The char-create live probe: `WOW_PROBE_CHARCREATE="<name>[,race,class,gender,…]"` creates (and
    // cleans up) a character at select to verify the char-create/delete wire (decision 0423 phase 1;
    // see `capture::ProbeCharCreatePlugin`).
    if std::env::var("WOW_PROBE_CHARCREATE").is_ok() {
        app.add_plugins(capture::ProbeCharCreatePlugin);
    }
    // The live FPS probe: `WOW_LIVE_FPS=<frames>` samples frame times on a NORMAL connected run
    // and exits — the harness probe's numbers with the live world in (see `capture::LiveFpsPlugin`).
    if std::env::var("WOW_LIVE_FPS").is_ok() {
        app.add_plugins(capture::LiveFpsPlugin);
    }
    // The FPS journal: `WOW_FPS_JOURNAL=<csv>` appends per-second position + frame-time rows on a
    // director-driven run — "where does it dip" as coordinates (see `perf::FpsJournalPlugin`).
    if std::env::var("WOW_FPS_JOURNAL").is_ok() {
        app.add_plugins(perf::FpsJournalPlugin);
    }

    // Return the app's own exit status instead of dropping it: a failed capture writes
    // `AppExit::error()` (see `capture::drive_capture`), and discarding it made the process exit 0
    // with no PNG on disk — which is how a sweep carried on around a missing shot (decision 0743).
    app.run()
}
