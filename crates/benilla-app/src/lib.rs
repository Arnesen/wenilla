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
//! players stay cyan cubes, and our own avatar is blue until we take third-person control of it.
//! **The world is loaded when a character enters it, and released when they leave** — the glue
//! screens have no world behind them (decision 0777). With no server there is no world: the client
//! sits at the login screen, which is what the real one does. The scene harness (`$WOW_CAPTURE`)
//! boots straight in-world and is unaffected.
//!
//! Controls: WASD walks the avatar (Ctrl sprints); right-drag turns it, left-drag orbits the camera
//! (both hide/freeze the cursor while held), scroll wheel zooms; `F` toggles free-fly (then WASD flies
//! the camera with Space up / C down, Ctrl boost).

mod area;
mod area_trigger;
mod art_scope;
mod asset_churn;
mod assets;
mod aura_visual;
mod bgwin;
mod billboard;
mod blob_shadow;
mod bowstring;
mod build_id;
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
mod cvars;
mod dbg_trace;
mod death;
mod debug_panel;
mod decal;
mod doodad_anim;
mod entities;
mod entity_shade;
mod exterior_cull;
mod ffx_glow;
mod glue;
mod glue_strings;
mod go_anim;
mod go_templates;
mod ground_fx;
mod hover_log;
mod instance_tint;
mod interact;
mod interior;
mod items;
mod lighting;
mod liquid;
mod loading_screen;
mod local_state;
mod login;
mod map_proj;
mod mesh_tag;
mod minimap;
mod model_fade;
mod model_forms;
mod model_render;
mod modkeys;
mod nameplates;
mod names;
mod net;
mod npc_text;
mod particles;
mod pending_item_ops;
mod perf;
mod pipe_warm;
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
mod ui_follow;
mod ui_gamma;
mod ui_gossip;
mod ui_hide;
mod ui_inspect;
mod ui_item_text;
mod ui_items;
mod ui_logout;
mod ui_loot;
mod ui_loot_roll;
mod ui_macro;
mod ui_mail;
mod ui_merchant;
mod ui_mirror;
mod ui_net;
mod ui_party;
mod ui_pass;
mod ui_pet;
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
mod wmo_sky;
mod world_map;
mod world_state;
mod zfill;

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
use ui_follow::UiFollowPlugin;
use ui_gossip::UiGossipPlugin;
use ui_item_text::UiItemTextPlugin;
use ui_items::UiItemsPlugin;
use ui_logout::UiLogoutPlugin;
use ui_loot::UiLootPlugin;
use ui_loot_roll::UiLootRollPlugin;
use ui_mail::UiMailPlugin;
use ui_merchant::UiMerchantPlugin;
use ui_mirror::UiMirrorPlugin;
use ui_net::UiNetPlugin;
use ui_party::UiPartyPlugin;
use ui_pass::PlayerUiPlugin;
use ui_pet::UiPetPlugin;
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

// The `benilla` launcher shim (the bin package) is this library's only caller: it stamps the
// build id at compile time and hands it into [`run`]. Re-exported so the shim needs no bevy
// dep of its own.
pub use bevy::app::AppExit;
pub use build_id::BuildId;

/// Anchor the loaded terrain block on the Human start (Northshire), where `one`/`One`
/// logs in — so the player sits in the middle of the block instead of Stormwind's edge.
const SPAWN_XY: (f32, f32) = (-8949.95, -132.49);

/// Build and run the client app. `build` is the launcher shim's compile-time git stamp
/// ([`build_id`]) — passed in as plain data so the sha lives in the shim's fingerprint, not
/// this crate's, and a commit stops recompiling the app (decision 0993).
pub fn run(build: BuildId) -> AppExit {
    // `WOW_CAPTURE=list` just prints the harness scenario names (the source of truth `scripts/visual.sh`
    // reads) and exits before any window/asset setup.
    // `WOW_HOVER_LOG_REPORT=<csv>` re-reads a recorded run and prints its report, then exits —
    // no window, no game. New analysis lands on runs already captured (see `hover_log`).
    if let Ok(path) = std::env::var("WOW_HOVER_LOG_REPORT") {
        hover_log::report_recorded_file(&path);
        return AppExit::Success;
    }
    if std::env::var("WOW_CAPTURE").as_deref() == Ok("list") {
        capture::print_scenario_names();
        return AppExit::Success;
    }

    let mut app = App::new();
    // The stamp is plain data from here on — the panel footer and preflight banner read it back.
    app.insert_resource(build);

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
        // Third source of per-run drift, and the one the lighting matrix (decision 0746) hit: the
        // anim-LOD park/wake gate (`creature_anim::lod::gate_rig_animation`). Whether a rig is
        // parked, and which pose it wakes into, depends on when its model finished loading relative
        // to the frustum/room evaluation — asset-load timing, which the frozen clock does not
        // control. MEASURED: the seeded wolf lands in one of exactly two poses, the pair always
        // MAE 4.123 apart, and three runs with the gate off are bit-identical.
        //
        // Off for captures, unless explicitly overridden. A still frame is meant to contain only
        // rigs that are IN view, and a rig in view is one the gate should never park — so what a
        // capture loses is the gate's own correctness, not the shot's subject. That loss is named,
        // not free: a rig wrongly parked in frame stays invisible to the sweep, and the deeper
        // question the measurement raises — why a woken rig does not converge on the absolute-clock
        // pose the gate promises — is open in 0746 and worth its own hunt.
        if std::env::var("WOW_NO_ANIM_LOD").is_err() {
            std::env::set_var("WOW_NO_ANIM_LOD", "1");
        }
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
                        // `$WOW_WIN` overrides here too — the resolution-A/B instrument for UI
                        // scenarios (a scale-dependent text bug looks fine at the scenario's
                        // default size and truncates at fullscreen heights).
                        if let Some(win) = std::env::var("WOW_WIN").ok().and_then(|v| {
                            let (w, h) = v.split_once('x')?;
                            Some(UVec2::new(w.parse().ok()?, h.parse().ok()?))
                        }) {
                            win
                        } else {
                            match std::env::var("WOW_CAPTURE").as_deref() {
                                Ok("ui-actionbar") => UVec2::new(1300, 260),
                                Ok("vplates") => UVec2::new(1024, 768),
                                // The director's small-window shape: short enough that the action bar
                                // strip overlaps the chat edit box rows — the overlap is the subject.
                                Ok("ui-chatedit") => UVec2::new(566, 377),
                                // The fullscreen map's chrome is a centered 1024×768 block; a hair of
                                // margin shows the blackout doing its job.
                                Ok("ui-worldmap") => UVec2::new(1100, 800),
                                // The 920×724 era options window wants air on every side so the
                                // straddling right-edge tile and the hung close X stay in frame.
                                Ok("ui-options")
                                | Ok("ui-options-audio")
                                | Ok("ui-options-graphics") => UVec2::new(1200, 900),
                                _ => UVec2::new(640, 700),
                            }
                        }
                        .into()
                    } else {
                        // `$WOW_WIN=WxH` (logical px): override the world capture/window size —
                        // the resolution-A/B instrument. The FFXGlow blur geometry is byte-pinned
                        // in TEXELS, so its angular footprint shrinks as resolution grows and
                        // thin bright features (fence rails) self-amplify at 4K where the
                        // 1024-era reference diluted them; matching the era's pixel density
                        // (e.g. `WOW_WIN=512x288` on a 2× display → 1024×576 physical) isolates
                        // that term. Also the knob for any future era-resolution comparison.
                        std::env::var("WOW_WIN")
                            .ok()
                            .and_then(|v| {
                                let (w, h) = v.split_once('x')?;
                                Some(UVec2::new(w.parse().ok()?, h.parse().ok()?))
                            })
                            .unwrap_or(UVec2::new(1600, 900))
                            .into()
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
            // Our file assets (the WGSL shaders, the default UI XML) live beside THIS crate —
            // but the binary is built by the `benilla` shim package, so Bevy's runtime
            // `CARGO_MANIFEST_DIR` fallback would resolve `assets/` in the shim's dir (and a
            // bare-binary run has no manifest dir at all — the silently-no-shaders trap in
            // `capture/mod.rs`'s header). Bake this crate's absolute path at compile time:
            // right under `cargo run` from any package, and a bare `target/debug/benilla`
            // now finds its shaders too. Machine-local, like any dev build (decision 0993).
            .set(bevy::asset::AssetPlugin {
                file_path: concat!(env!("CARGO_MANIFEST_DIR"), "/assets").into(),
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
    // The per-instance body tint (decision 0812), on the same slot index as that palette: the aura
    // state kit's CharProc-1 colour, uploaded to its own region of the shared light buffer.
    .add_plugins(instance_tint::plugin)
    .add_plugins(BowstringPlugin)
    .add_plugins(QuestMarkersPlugin)
    // Frame-time HUD + diagnostics — the performance standard.
    // After DebugPanelPlugin so the egui plugin/context it sets up already exists. Toggle: P.
    .add_plugins(PerfPlugin)
    // Pipeline-compile counters + the live-compile tripwire (decision 0837: macOS builds every
    // pipeline synchronously on the render thread, so a live compile is a felt stall).
    .add_plugins(pipe_warm::plugin)
    .add_plugins(hover_log::HoverLogPlugin)
    .add_plugins(asset_churn::AssetChurnPlugin)
    // Within-map art residency (decision 0793): the dedup caches expire by DISTANCE, so a
    // long flight inside one map stops ratcheting. Registered before AssetPlugin only so the
    // census resource exists for anything that reads it at startup; it needs no ordering.
    .add_plugins(art_scope::ArtScopePlugin)
    .add_plugins(particles::census::plugin)
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
    // WMO skybox: the authored sky a building's `0x40000` group swaps in for that gradient
    // (Stratholme's burning city) — registered after SkyPlugin, whose dome it stands down.
    .add_plugins(wmo_sky::WmoSkyPlugin)
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
    // The exterior scene draws only through portal windows the flood left behind (decision 0774):
    // from inside a building, terrain and ADT doodads are gated on the deferred window worklist.
    .add_plugins(exterior_cull::ExteriorCullPlugin)
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
    // The CVar host (decision 0954): registration, knob sync, config.toml persistence. After
    // UiScriptPlugin only for reading order — its systems gate on the VM existing anyway.
    .add_plugins(cvars::CvarPlugin)
    // The unit snapshot + event feed (decision 0068 §3): pushes ECS game state into the VM as the
    // plain data the `Unit*` bindings read, and fires the matching WoW events.
    .add_plugins(UiUnitPlugin)
    .add_plugins(UiPartyPlugin)
    // Duels (decision 0633): the wire session, the client-side countdown tick, the four Era
    // events, and the accept/cancel/challenge intents.
    .add_plugins(UiDuelPlugin)
    // Auto-follow's UI seam: the popup's Follow row + `FollowUnit`/`FollowByName` inbound, and
    // the AUTOFOLLOW_BEGIN/END pair that drives the centre-screen status line outbound.
    .add_plugins(UiFollowPlugin)
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
    // The macro system (decision 0983): the icon chooser's catalog, the `benilla/macros/`
    // files, `UPDATE_MACROS`, and the macro→bound-spell table the action bar's MACRO slots
    // resolve their cooldown/usability through. After UiSpellbookPlugin — the bound spell is
    // resolved against the book that feed pushes, by the same law `CastSpellByName` uses.
    .add_plugins(ui_macro::UiMacroPlugin)
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
    // The pet action bar (decision 0982) — the stance bar's mirror image: server-authoritative,
    // so this renders the ten packed words the last `SMSG_PET_SPELLS` delivered and sends
    // intents back. After UiActionPlugin (shares `Spells` and the cooldown triple's clock).
    .add_plugins(UiPetPlugin)
    // The connection-telemetry feed: the averaged ping RTT behind `GetNetStats()`, which the main
    // bar's performance meter polls (decision 0658).
    .add_plugins(UiNetPlugin)
    .add_plugins(UiCastPlugin)
    // The breath / fatigue bars (decision 0874): server-authoritative mirror timers off the
    // wire into the transcribed MirrorTimer1/2/3 frames. Beside the cast bar it shares its
    // feed→drain shape (and its art: the same UI-CastingBar-Border chrome).
    .add_plugins(UiMirrorPlugin)
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
    // Any scripted probe keeps its window un-occludable: a fully covered macOS window drops to
    // ~1 fps drawables, and every probe schedule is wall-clock — a throttled run doesn't measure
    // slowly, it runs the wrong script (see `capture::ProbeFocusPlugin`, decision 0906).
    // (`WOW_LIVE_FPS` is in the list because an occluded SETTLE phase streams the world at ~1 fps
    // and under-warms the scene before sampling even starts — the assertion has to be live from
    // the first tick, not at the uncap.)
    if [
        "WOW_PROBE",
        "WOW_PROBE_CHAT",
        "WOW_PROBE_KEY",
        "WOW_PROBE_LUA",
        "WOW_RIG",
        "WOW_LIVE_FPS",
    ]
    .iter()
    .any(|k| std::env::var(k).is_ok())
    {
        app.add_plugins(capture::ProbeFocusPlugin);
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
