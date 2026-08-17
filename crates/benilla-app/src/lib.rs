//! `benilla` — a from-scratch World of Warcraft 1.12.1 client on Bevy, talking to a local vmangos server.
//!
//! Opens the vanilla patch chain from wherever the install is (`$WOW_DATA`, the project folder on a
//! dev build, else beside the binary — decision 1175) and streams the world around the
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
//! Controls: WASD walks the avatar; right-drag turns it, left-drag orbits the camera (both
//! hide/freeze the cursor while held), scroll wheel zooms. Those are all **bindings** now (0997) —
//! rebindable, and nothing in the client squats on a bare key beside them (1043). The dev chord
//! (`Ctrl`+`Shift`) + `F` toggles free-fly, then WASD flies the camera with Space up / C down and
//! `Ctrl` boosts — a boost that exists only inside free-fly, itself behind the chord.

// **A player build's dead code is the seam working, not a defect.** With `--no-default-features`
// every symbol whose only caller is an instrument goes unused — the camera-park seam, the aim
// seam, the pool-slot identity, a dozen accessors the panel and the probes read. Warning about
// each one would bury a REAL warning in the one build nobody looks at daily, and the alternative
// (a `cfg` attribute per item) would spread seam knowledge back across the gameplay modules 1174
// spent its whole diff clearing it out of. Dev builds still warn normally.
#![cfg_attr(not(feature = "dev"), allow(dead_code))]

pub mod addon_harness;
mod area;
mod area_trigger;
#[cfg(feature = "dev")]
mod asset_churn;
mod aura_visual;
mod autocast_shine;
mod bindings;
mod blob_shadow;
mod bowstring;
#[cfg(feature = "dev")]
mod capture;
mod char_create;
mod char_select;
mod chat_bubble;
mod combat_text;
mod cooldowns;
mod creature_anim;
mod cursor;
mod cvars;
mod death;
#[cfg(feature = "dev")]
mod debug_panel;
/// **The dev/player seam** (decisions 0026/1173, built in 1174) — the group, and the boundary
/// rule, in one file. Always compiled; what it *holds* is not.
mod dev;
mod entities;
mod fishing_line;
mod footprints;
mod glue;
mod glue_strings;
mod go_anim;
mod go_templates;
#[cfg(feature = "dev")]
mod hover_log;
mod items;
mod loading_screen;
mod local_state;
mod login;
mod minimap;
mod nameplates;
mod names;
mod net;
mod npc_text;
mod pending_item_ops;
#[cfg(feature = "dev")]
mod perf;
mod pipe_warm;
mod player;
mod portrait;
#[cfg(feature = "dev")]
mod preflight;
#[cfg(feature = "dev")]
mod probe_shield;
mod quest_markers;
mod raid_marks;
mod run_mode;
mod shaders;
mod smart_rect;
mod sound;
mod target;
mod textinput;
mod transport;
mod ui_action;
mod ui_aura;
mod ui_bank;
mod ui_binder;
mod ui_cast;
mod ui_char;
mod ui_chat;
mod ui_craft;
mod ui_dressup;
mod ui_duel;
mod ui_follow;
mod ui_gamma;
mod ui_gossip;
mod ui_guild;
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
mod ui_pet_book;
mod ui_pet_doll;
mod ui_pet_stats;
mod ui_quest;
mod ui_quest_log;
mod ui_reputation;
mod ui_saved;
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
mod video;
mod vplates;
mod world_state;

use bevy::prelude::*;
use blob_shadow::BlobShadowPlugin;
use bowstring::BowstringPlugin;
use creature_anim::CreatureAnimPlugin;
use cursor::CursorPlugin;
use entities::EntitiesPlugin;
use fishing_line::FishingLinePlugin;
use footprints::FootprintsPlugin;
use loading_screen::LoadingScreenPlugin;
use net::NetPlugin;
use player::PlayerPlugin;
use portrait::PortraitPlugin;
use quest_markers::QuestMarkersPlugin;
use sound::SoundPlugin;
use target::TargetPlugin;
use textinput::TextInputPlugin;
use transport::TransportPlugin;
use ui_action::UiActionPlugin;
use ui_aura::UiAuraPlugin;
use ui_bank::UiBankPlugin;
use ui_binder::UiBinderPlugin;
use ui_cast::UiCastPlugin;
use ui_char::UiCharPlugin;
use ui_chat::UiChatPlugin;
use ui_craft::UiCraftPlugin;
use ui_duel::UiDuelPlugin;
use ui_follow::UiFollowPlugin;
use ui_gossip::UiGossipPlugin;
use ui_guild::UiGuildPlugin;
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
use ui_pet_book::UiPetBookPlugin;
use ui_pet_doll::UiPetDollPlugin;
use ui_pet_stats::UiPetStatsPlugin;
use ui_quest::UiQuestPlugin;
use ui_quest_log::UiQuestLogPlugin;
use ui_saved::UiSavedPlugin;
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

// The `benilla` launcher shim (the bin package) is this library's only caller: it stamps the
// build id at compile time and hands it into [`run`]. Re-exported so the shim needs no bevy
// dep of its own.
pub use benilla_world::build_id::BuildId;
/// The world viewer's entry point — the engine with no game attached (decision 1160).
/// Its shim (`benilla-worldview`) is this library's second caller; see [`worldview`].
pub use benilla_world::worldview::run as run_worldview;
pub use bevy::app::AppExit;

/// Build and run the client app. `build` is the launcher shim's compile-time git stamp
/// ([`build_id`]) — passed in as plain data so the sha lives in the shim's fingerprint, not
/// this crate's, and a commit stops recompiling the app (decision 0993).
pub fn run(build: BuildId) -> AppExit {
    // `WOW_CAPTURE=list` just prints the harness scenario names (the source of truth `scripts/visual.sh`
    // reads) and exits before any window/asset setup.
    // `WOW_HOVER_LOG_REPORT=<csv>` re-reads a recorded run and prints its report, then exits —
    // no window, no game. New analysis lands on runs already captured (see `hover_log`).
    if let Ok(path) = std::env::var("WOW_HOVER_LOG_REPORT") {
        dev::report_recorded_hover_log(&path);
        return AppExit::Success;
    }
    if std::env::var("WOW_CAPTURE").as_deref() == Ok("list") {
        dev::print_scenario_names();
        return AppExit::Success;
    }

    let mut app = App::new();
    // The stamp is plain data from here on — the panel footer and preflight banner read it back.
    app.insert_resource(build);
    // Pin Bevy's static-scene transform tracking ON (decision 1356). At the default threshold
    // `mark_dirty_trees` re-decides per frame by counting every changed-Transform row and every
    // tree row — two full scans of the very population the tracking exists to skip. This scene
    // is provably static-heavy (terrain/WMO batches and parked units barely move), so the
    // auto-tuner can only ever confirm what this line states. The guard it removes bites only
    // when MOST rows move in one frame (a load burst into a near-empty world), where
    // dirty-marking briefly costs more than it saves — a loading-band term, accepted knowingly.
    app.insert_resource(bevy::transform::systems::StaticTransformOptimizations::enabled());
    // The Update schedule runs on the SINGLE-THREADED executor (decision 1366). Not a tuning
    // whim: ~60% of our per-frame Update systems are non-Send (mlua's UiScript, kira's audio
    // handles) and serialize through the multi-threaded executor's one `local_thread_running`
    // flag anyway, so the cross-thread dispatch machinery was pure overhead — measured
    // −1.30 cpu_ms at the LBRS pin under 5-round grading with the wall tail (p95/p99/max)
    // flat-to-better (the 1364 "fatter tail" reading did not reproduce; 1366 has the tables,
    // both pins). PostUpdate stays multi-threaded — its wide parallel bands
    // (propagation, visibility) are real parallelism; re-grading it is a named open probe.
    // `WOW_MT_UPDATE=1` is the A/B lever back to the multi-threaded executor.
    if std::env::var_os("WOW_MT_UPDATE").is_none() {
        app.edit_schedule(Update, |s| {
            s.set_executor_kind(bevy::ecs::schedule::ExecutorKind::SingleThreaded);
        });
    }
    // `WOW_ST_POSTUPDATE=1` — 1366's named open probe: PostUpdate on the single-threaded
    // executor, an EXPERIMENT lever only. The expectation is a REGRESSION (propagation and
    // visibility are real parallel bands there, not flag-serialized non-Send work); the lever
    // exists so the expectation gets measured instead of assumed.
    if std::env::var_os("WOW_ST_POSTUPDATE").is_some() {
        app.edit_schedule(PostUpdate, |s| {
            s.set_executor_kind(bevy::ecs::schedule::ExecutorKind::SingleThreaded);
        });
    }
    // …and one startup line says which build produced this log. Registered HERE, beside the stamp
    // it prints, rather than in `preflight` where it sat until decision 1179: **which build is
    // this** is the first thing a bug report from someone else's machine has to establish, and a
    // player build is the one whose logs always come from someone else's machine. Gating it out
    // with the instruments got the argument exactly backwards (`preflight`'s own module doc makes
    // the case; 1174 moved the file without re-reading it).
    app.add_systems(Startup, benilla_world::build_id::banner);

    // Visual A/B harness (decision 0008): with `$WOW_CAPTURE` set, the app runs a deterministic,
    // server-less capture (net off so no NPCs stream in nondeterministically) and exits. See `capture`.
    let capturing = run_mode::scenario_active();
    // Any instrumented run — captures AND the live-probe fleet — opens in the background so it
    // never fights the director's screen (decision 0703; `WOW_BG` overrides). See `bgwin`.
    let background = benilla_world::bgwin::background_run();
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
    // builds. Finds the install the one way anything does (`benilla_formats::wow_data`, decision
    // 1175) — the same answer the `WorldAssets` foundation gets. On failure the source is simply
    // absent and the AdtTile-pipeline loads fail gracefully (the terrain just doesn't appear).
    match benilla_formats::wow_data() {
        Some(data_dir) => {
            if let Err(e) = benilla_assets::register_mpq_source(&mut app, &data_dir) {
                eprintln!("benilla-assets: mpq:// source unavailable ({e:#})");
            }
        }
        None => eprintln!(
            "benilla: no WoW install found — looked in {:?}",
            benilla_formats::candidates()
        ),
    }

    // NOTE: there is deliberately no `game://` asset source here (decision 1175). 1171 gave the
    // game's five UI shaders their own source pointed at this crate's `assets/` — with the path
    // baked from `CARGO_MANIFEST_DIR`, so it named the build machine's source tree and resolved
    // to nothing anywhere else. The line 1171 drew survives: those five are still this crate's,
    // now compiled in by `crate::shaders` and addressed `embedded://benilla_app/shaders/…`,
    // because `embedded_asset!` is per-crate by construction. Only the directory is gone.

    app.add_plugins(benilla_world::boot::tuned_default_plugins(Window {
        title: "benilla".into(),
        // UI-fixture captures shrink the window so the docked panel fills the frame — the capture
        // is the look-pass instrument and the window is its subject. The action bar is the
        // exception: it spans 1024px + 128px end caps along the screen bottom, so it gets a wide,
        // short window instead of the tall default (else the caps crop). The vplates scenario pins
        // the 1:1 gx window: at 1024×768 one gx unit = 1280 px, so the plate must land at the
        // border texture's native 128×32 — directly diffable against the decoded BLP. Sized
        // per-capture off WOW_CAPTURE.
        resolution: if capturing && std::env::var("WOW_CAPTURE_UI").as_deref() == Ok("1") {
            // `$WOW_WIN` overrides here too — the resolution-A/B instrument for UI scenarios (a
            // scale-dependent text bug looks fine at the scenario's default size and truncates at
            // fullscreen heights).
            if let Some(win) = std::env::var("WOW_WIN").ok().and_then(|v| {
                let (w, h) = v.split_once('x')?;
                Some(UVec2::new(w.parse().ok()?, h.parse().ok()?))
            }) {
                win
            } else {
                match std::env::var("WOW_CAPTURE").as_deref() {
                    Ok("ui-actionbar") => UVec2::new(1300, 260),
                    Ok("vplates") => UVec2::new(1024, 768),
                    // The director's small-window shape: short enough that the action bar strip
                    // overlaps the chat edit box rows — the overlap is the subject.
                    Ok("ui-chatedit") => UVec2::new(566, 377),
                    // The fullscreen map's chrome is a centered 1024×768 block; a hair of margin
                    // shows the blackout doing its job.
                    Ok("ui-worldmap") => UVec2::new(1100, 800),
                    // The 920×724 era options window wants air on every side so the straddling
                    // right-edge tile and the hung close X stay in frame.
                    Ok("ui-options") | Ok("ui-options-audio") | Ok("ui-options-graphics") => {
                        UVec2::new(1200, 900)
                    }
                    _ => UVec2::new(640, 700),
                }
            }
            .into()
        } else {
            // `$WOW_WIN=WxH` (logical px): override the world capture/window size — the
            // resolution-A/B instrument. The FFXGlow blur geometry is byte-pinned in TEXELS, so its
            // angular footprint shrinks as resolution grows and thin bright features (fence rails)
            // self-amplify at 4K where the 1024-era reference diluted them; matching the era's
            // pixel density (e.g. `WOW_WIN=512x288` on a 2× display → 1024×576 physical) isolates
            // that term. Also the knob for any future era-resolution comparison.
            std::env::var("WOW_WIN")
                .ok()
                .and_then(|v| {
                    let (w, h) = v.split_once('x')?;
                    Some(UVec2::new(w.parse().ok()?, h.parse().ok()?))
                })
                // A run that reads no pixels gets a SMALL window. It is held `AlwaysOnTop` for its
                // whole life so it can never be occluded into the ~1 fps throttle
                // (`capture::ProbeFocusPlugin`, decision 0906) — at the full default that meant
                // every agent probe planted a screen-filling window over the director's work. Small
                // + cornered (`ProbeFocusPlugin` parks it) is un-occludable AND out of the way;
                // anything photographing pixels keeps the full size, and `WOW_WIN` overrides either
                // way (decision 1148).
                .unwrap_or(if benilla_world::bgwin::no_pixel_run() {
                    UVec2::new(640, 360)
                } else {
                    UVec2::new(1600, 900)
                })
                .into()
        },
        // The boot present mode. `$WOW_NOVSYNC=1` uncaps presentation so a headless FPS-journal
        // run measures true frame cost, not the vsync ceiling — the same uncap the capture probe
        // flips mid-run, available from boot for non-capture probes (perf triage at the glue
        // screens, where no capture scenario runs). Absent it, we boot synced and the player's
        // `gxVSync` setting takes over from `Startup` on ([`crate::video`]).
        present_mode: video::present_mode(!video::novsync_env()),
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
    }))
    // The game's own WGSL, compiled into the binary (decision 1175) — before anything that could
    // ask for one. The engine's seven register themselves inside `WorldPlugins` below.
    .add_plugins(shaders::plugin)
    .add_plugins(benilla_world::thread_qos::ThreadQosPlugin)
    // The app-side half of background instrumented runs: undo winit's forced macOS app
    // activation so a probe/capture launch never yanks focus off the director's screen. The
    // window-side half (unfocused + always-on-bottom) is in the `Window` above. Decision 0703.
    .add_plugins(benilla_world::bgwin::BgWinPlugin)
    // **The engine, as one name** (decision 1164). Everything `benilla-world` will own,
    // in the order both binaries used before this group existed — see `world_plugins.rs`
    // for the two ordering edges inside it that are load-bearing, and for what is
    // deliberately left out (`pipe_warm`).
    .add_plugins(benilla_world::world_plugins::WorldPlugins)
    // The instruments, which the engine group deliberately does not carry (1160: instruments at
    // the top of the stack). The panel first — `PerfPlugin` needs the egui plugin/context it sets
    // up. Toggles: the dev chord + D, and P.
    // **The instruments** (decisions 1173/1174): the debug panel, the perf HUD, the object
    // inspector, the hover-cost recorder, the asset-churn meter, the session preflight and the
    // probe shield — one group, in the slot the panel has always held (it sets up the egui
    // context the perf pill needs). `--no-default-features` compiles every one of them out; see
    // `dev.rs` for what is in the group and the one rule that governs the boundary.
    .add_plugins(dev::DevToolsPlugin)
    .add_plugins(BowstringPlugin)
    .add_plugins(FishingLinePlugin)
    .add_plugins(QuestMarkersPlugin)
    // Pipeline-compile counters + the live-compile tripwire (decision 0837: macOS builds every
    // pipeline synchronously on the render thread, so a live compile is a felt stall).
    .add_plugins(pipe_warm::plugin)
    // Streamed world entities: cube assets + display catalogs at startup, sync each frame.
    .add_plugins(EntitiesPlugin)
    // Creature animation: pick Stand/Walk/Run from each creature's movement state each frame (Milestone C).
    .add_plugins(CreatureAnimPlugin)
    // The unit blob shadow: the dark ground oval under every unit, sized from the playing
    // animation's box (the byte-verified law — wow-re unit-blob-shadow RE), on the same
    // surface-decal projector as the selection ring.
    .add_plugins(BlobShadowPlugin)
    // Footprint decals (B212, decision 1006): the prints a walking unit leaves on snow/sand,
    // spawn-once projections on the same decal projector, fading off the effect stream.
    .add_plugins(FootprintsPlugin)
    // GameObject animation (decision 0242): net-streamed GObjects (doors/chests) play an M2 sequence
    // on GAMEOBJECT_STATE change — the state-machine sibling of the doodad idle loop above.
    .add_plugins(go_anim::plugin)
    // Avatar + camera + input.
    .add_plugins(PlayerPlugin)
    // The real client's hardware mouse cursor (native NSCursor on macOS).
    .add_plugins(CursorPlugin)
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
    // that answers the parked IO thread's pick. A world capture boots straight InWorld (no net,
    // no picker); a glue capture boots onto the screen it photographs.
    .add_plugins(char_select::CharSelectPlugin {
        start: run_mode::start_state(),
    })
    // The login screen (decision 0539): the faithful AccountLogin glue + the credential policy
    // that answers the IO thread's pre-logon park.
    .add_plugins(login::LoginPlugin)
    // The character-creation screen + its live preview booth (decision 0423).
    .add_plugins(char_create::CharCreatePlugin)
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
    // The pet-bar / spellbook autocast shine, drawn on the append lane from the conversion's
    // parked sites — zero per-frame script-layout traffic (decision 1383, B282).
    .add_plugins(autocast_shine::AutocastShinePlugin)
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
    // The video knobs the CVar host writes into (today: `gxVSync`). Before CvarPlugin so the
    // resource exists when `load_config` applies the saved value at Startup.
    .add_plugins(video::VideoPlugin)
    // The CVar host (decision 0954): registration, knob sync, config.toml persistence. After
    // UiScriptPlugin only for reading order — its systems gate on the VM existing anyway.
    .add_plugins(cvars::CvarPlugin)
    // The key-binding engine (decision 0997): the chord→command dispatch every rebindable input
    // runs through, its persistence, and the Key Bindings window's capture seam.
    .add_plugins(bindings::BindingsPlugin)
    // The unit snapshot + event feed (decision 0068 §3): pushes ECS game state into the VM as the
    // plain data the `Unit*` bindings read, and fires the matching WoW events.
    .add_plugins(UiUnitPlugin)
    .add_plugins(UiPartyPlugin)
    // Duels (decision 0633): the wire session, the client-side countdown tick, the four Era
    // events, and the accept/cancel/challenge intents.
    .add_plugins(UiDuelPlugin)
    // Setting your hearthstone (decision 1331): the innkeeper's SMSG_BINDER_CONFIRM question, the
    // CONFIRM_BINDER dialog it raises, and the CMSG_BINDER_ACTIVATE its Accept sends — the only
    // packet in the flow that actually binds anything.
    .add_plugins(UiBinderPlugin)
    // Auto-follow's UI seam: the popup's Follow row + `FollowUnit`/`FollowByName` inbound, and
    // the AUTOFOLLOW_BEGIN/END pair that drives the centre-screen status line outbound.
    .add_plugins(UiFollowPlugin)
    // Leaving (decision 0674): the game menu's Logout/Exit Game — the request, the server's
    // 20-second answer narrated as the CAMP/QUIT countdown, and the process exit.
    .add_plugins(UiLogoutPlugin)
    .add_plugins(UiSocialPlugin)
    // Guilds (decision 1257): the identity/roster mirror behind the four guild windows, the
    // membership verbs, and the `ERR_GUILD_*` lines. Right after the social session, whose
    // FriendsFrame it shares a window with and whose ignore list its sign-on lines consult.
    .add_plugins(UiGuildPlugin)
    .add_plugins(UiTooltipPlugin)
    // The character-window feed (decision 0208): the combat-stats/inventory snapshots + events
    // the paper doll reads, and the paper-doll booth's yaw mirror.
    .add_plugins(UiCharPlugin)
    // The reputation-pane feed: the player's wire faction slots resolved against Faction.dbc into
    // the pane's snapshot, plus the pane's three outbound verbs. Beside the character feed because
    // it is the same window's other tab.
    .add_plugins(ui_reputation::UiReputationPlugin)
    // The inspect feed (decision 0631): another player's equipment off their PUBLIC visible-item
    // entries, plus the "inspect" booth's unit + yaw. Right after the character feed it mirrors.
    .add_plugins(ui_inspect::InspectUiPlugin)
    // The dressing-room feed (decision 1060): the window's try-on intents → the player's own look
    // with the tried-on items substituted in, plus the "dressup" booth's yaw. Beside the inspect
    // feed, whose shape it shares (intents in, a booth look out).
    .add_plugins(ui_dressup::DressUpUiPlugin)
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
    // The macro system (decision 0983): the icon chooser's catalog, the `benilla-config/macros/`
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
    .add_plugins(UiPetBookPlugin)
    // The pet's paper-doll stat block (happiness/loyalty/XP/training points). Its own plugin
    // because it runs off descriptor fields and two DBC tables rather than off `SMSG_PET_SPELLS`.
    .add_plugins(UiPetStatsPlugin)
    // The pet paper doll's SHARED surface (decision 1057) — the combat-stats snapshot under the
    // `"pet"` token and the page's model booth. Apart from the block above because these values
    // pass through the character sheet's own bindings and events, with no hunter gate.
    .add_plugins(UiPetDollPlugin)
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
    .add_plugins(UiSavedPlugin)
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

    // Register benilla-assets' loaders AFTER `AssetPlugin` (they go into the live `AssetServer`).
    benilla_assets::register_asset_loaders(&mut app);

    // **The probe fleet** — the capture harness and every scripted live probe, each armed by its
    // own environment variable and inert without it. Added last so they observe the fully-built
    // app; compiled out entirely by `--no-default-features` (decisions 1173/1174), which is why
    // this is one line and the twenty env checks behind it live in `dev.rs`.
    app.add_plugins(dev::DevProbesPlugin);

    // Return the app's own exit status instead of dropping it: a failed capture writes
    // `AppExit::error()` (see `capture::drive_capture`), and discarding it made the process exit 0
    // with no PNG on disk — which is how a sweep carried on around a missing shot (decision 0743).
    app.run()
}
