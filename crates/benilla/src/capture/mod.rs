//! Deterministic capture mode — the machine half of the Phase-5 visual A/B harness (decision 0008).
//!
//! With `$WOW_CAPTURE=<scenario>` set, the app boots server-less (net disabled in `main`), pins the
//! game-clock and the camera to a named viewpoint, waits for terrain streaming to settle, writes one
//! PNG of the primary window to `$WOW_CAPTURE_OUT`, and exits. The render rework (decision 0008) is the
//! single riskiest change in the architecture — it can regress the whole look at once — so it goes
//! behind this harness: baselines are captured on the *current* pipeline, then every rework step is
//! diffed against them by the `benilla-visual` tool, catching regressions by machine before the
//! director's eye. Driven by `scripts/visual.sh`.
//!
//! Captures contain Blizzard-derived imagery (rendered terrain/models), so they live under the
//! gitignored `target/visual/` and are never committed.
//!
//! ## Running one capture by hand
//! **Run through Cargo — never the built binary directly:**
//! ```text
//! WOW_DATA=<vanilla Data dir> WOW_CAPTURE_UI=1 WOW_CAPTURE=ui-unitframes \
//!     WOW_CAPTURE_OUT=/tmp/shot.png cargo run -q -p benilla
//! ```
//! `cargo run` exports `CARGO_MANIFEST_DIR`, which is how Bevy's `AssetServer` finds
//! `crates/benilla/assets/` — in particular the WGSL shaders (`ui_quad.wgsl`, `sky.wgsl`, …).
//! Launching `target/debug/benilla` directly has no manifest dir, so it resolves `assets/` next
//! to the executable (`target/debug/assets/`, which doesn't exist), silently loads **no** shaders,
//! and every custom-shader layer — the *entire* player UI, sky, liquid, models — renders blank
//! while only the built-in-pipeline terrain still draws. A UI capture that comes back as bare sky
//! is this mistake, not a UI bug. `WOW_CAPTURE_UI=1` opts the player UI into the shot (off by
//! default so world baselines stay UI-free; omit it for world-only scenes). `WOW_CAPTURE=list`
//! prints the scenario names. `scripts/visual.sh` wraps all of this.

use std::path::Path;

use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Capturing, Screenshot};

use benilla_assets::coords::wow_to_bevy;

use crate::debug_panel::DebugState;
use crate::loading_screen::WorldLoadProgress;
use crate::perf::PerfHud;
use crate::player::WorldCamera;
use crate::schedule::WorldStage;

// The UI fixture seeding (the synthetic window states), the scenario table, and the live-run
// probe instruments (`probes`) each live in their own file — the server-less harness (settle,
// screenshot, perf probe) is this one's concern.
mod fixtures;
mod pick_probe;
mod probe_bank;
mod probe_castcancel;
mod probe_charcreate;
mod probe_crossing;
mod probe_mail;
mod probe_melee;
mod probe_partner;
mod probe_rig;
mod probe_taxi;
mod probes;
mod scenarios;
use fixtures::seed_ui_fixture;
pub(crate) use pick_probe::PickProbePlugin;
pub(crate) use probe_bank::ProbeBankPlugin;
pub(crate) use probe_castcancel::ProbeCastCancelPlugin;
pub(crate) use probe_charcreate::ProbeCharCreatePlugin;
pub(crate) use probe_crossing::ProbeCrossingPlugin;
pub(crate) use probe_mail::ProbeMailPlugin;
pub(crate) use probe_melee::ProbeMeleePlugin;
pub(crate) use probe_partner::ProbePartnerPlugin;
pub(crate) use probe_rig::{rig_char_name_from_env, ProbeRigPlugin};
pub(crate) use probe_taxi::ProbeTaxiPlugin;
pub(crate) use probes::{
    LiveFpsPlugin, LiveShotPlugin, NodeProbePlugin, ParticleCensusPlugin, ProbeChatPlugin,
    ProbeExitPlugin, ProbeKeyPlugin, ProbeLuaPlugin, ProbeResizePlugin,
};
use scenarios::{Scenario, UiFixture, GROUND_EYE, SCENARIOS};

/// Is the app running a capture (`$WOW_CAPTURE` set)? Read by `main` to disable net + add the harness.
pub(crate) fn scenario_active() -> bool {
    std::env::var("WOW_CAPTURE").is_ok()
}

/// The `fxview` fixture request — the **effect-viewer instrument**: spawn one effect/missile
/// model (full rig + emitters + ribbons + cards, the same `attach_effect_visuals` body the game
/// uses), let it run `age` seconds, and shoot it from a chosen angle. The agent's own eye on
/// spell visuals: every "this effect looks wrong from angle X" report becomes a reproducible
/// headless capture instead of a director round-trip. Not a golden scenario (deliberately
/// excluded from `print_scenario_names` — output depends on the model/age/angle knobs):
///
/// ```text
/// WOW_DATA=<Data> WOW_CAPTURE=fxview WOW_FX_MODEL='Spells\DemonArmor_Impact_Head.mdx' \
///   WOW_FX_AGE=1.2 WOW_FX_AZ=60 WOW_FX_EL=15 WOW_CAPTURE_OUT=/tmp/fx.png cargo run -q -p benilla
/// ```
///
/// Knobs: `WOW_FX_MODEL` (required, internal path), `WOW_FX_AGE` (seconds after attach, default
/// 1.0), `WOW_FX_AZ`/`WOW_FX_EL` (camera orbit degrees, default 0/10), `WOW_FX_DIST` (yards,
/// default 5), `WOW_FX_FLY` (yd/s along the model's facing — a missile only trails in motion;
/// default 0), `WOW_FX_YAW` (model facing, degrees, default 0), `WOW_FX_TURN` (deg/s the fixture
/// keeps turning after spawn — the attached-model heading-since-birth fan, wow-re
/// `part-kit-effect-attach-orient.md`; default 0), `WOW_FX_GROUND` (=1 seats the
/// fixture ON the terrain via a down-ray — required to see a ground-anchored effect's projected
/// surface decals, `crate::ground_fx`; default 0 = the mid-air point), `WOW_FX_HOLD` (=1 keeps
/// the fixture alive past one sequence pass — previewing a persistent HOLD kit's steady state;
/// default 0 = the game's discrete-instance reap at one pass, then the pool drains).
#[derive(Resource)]
pub(crate) struct FxViewRequest {
    pub(crate) model_path: String,
    pub(crate) age: f32,
    pub(crate) az_deg: f32,
    pub(crate) el_deg: f32,
    pub(crate) dist: f32,
    pub(crate) fly: f32,
    pub(crate) yaw_deg: f32,
    /// See `WOW_FX_TURN` above (deg/s).
    pub(crate) turn: f32,
    /// See `WOW_FX_GROUND` above.
    pub(crate) ground: bool,
    /// See `WOW_FX_HOLD` above.
    pub(crate) hold: bool,
}

/// The fixture's live state, written by `entities::spell_fx::drive_fx_view` and the phase
/// driver below.
#[derive(Resource, Default)]
pub(crate) struct FxViewState {
    /// Set by the capture driver once the scene has settled — the fixture spawns only then, so
    /// a one-shot effect's age at the shot is the REQUESTED age, not age + settle time.
    pub(crate) armed: bool,
    pub(crate) root: Option<Entity>,
    /// `time.elapsed_secs()` at the frame the visuals attached — the age clock's zero.
    pub(crate) attached_at: Option<f32>,
    /// The fixture ran its one sequence pass and the root was reaped (the game's discrete-kit
    /// completion callback, mirrored so captures past the span tell the truth: emitters drain,
    /// they don't pour). `WOW_FX_HOLD=1` disables the reap.
    pub(crate) expired: bool,
}

/// Where the fxview effect spawns: mid-air over the Northshire slope (raw WoW coords), inside
/// the ground scenario's streamed tiles, high enough that terrain never intersects the model.
pub(crate) const FXVIEW_POS: [f32; 3] = [-8960.0, -145.0, 90.0];

/// Print the BASELINE scenario names, one per line — the single source of truth
/// `scripts/visual.sh` reads so the driver never drifts from the code. Invoked by `main` for
/// `WOW_CAPTURE=list`. On-demand fixtures (the UI look-pass windows, sun/moon/sky, house-compass)
/// are deliberately absent: the blessed sweep is the director-chosen spot×time set only, so a
/// `visual.sh baseline` opens six windows on their screen and not thirty (decision 0632).
pub(crate) fn print_scenario_names() {
    for s in SCENARIOS {
        println!("{}", s.name);
    }
}

/// Marker that a capture is in progress — its presence gates `player::control` off so the harness is
/// the sole author of the camera (and thus the terrain stream focus).
#[derive(Resource)]
pub(crate) struct CaptureMode;

/// Frames the scene must stay fully streamed (residency reached) before the shot. This window is also
/// what warms the renderer: wgpu specialises the terrain/model/sky pipelines on their first draw, and
/// until they are ready Bevy draws only the clear colour — so a too-short settle photographs a uniform
/// fog-blue frame. 150 frames (~2.5 s) is generous margin for pipeline warm-up after the tiles spawn,
/// and also covers clutter/water/lighting settling and the loading-screen fade. (Residency reporting
/// alone is not enough: tile *entities* exist well before their pipelines are warm.)
const SETTLE_FRAMES: u32 = 150;

/// The effective settle window: `$WOW_CAPTURE_SETTLE` overrides [`SETTLE_FRAMES`] for scenes that
/// need longer to reach steady state before the shot (e.g. snow — flakes sink at 2-6.5 yd/s from
/// ~22 yd up, so the precipitation column takes ~600 frames to fill).
fn settle_frames() -> u32 {
    std::env::var("WOW_CAPTURE_SETTLE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(SETTLE_FRAMES)
}
/// Hard cap on how long to wait for the screenshot save to land, so a capture never hangs the harness.
const SAVE_TIMEOUT_FRAMES: u32 = 120;
/// A couple of grace frames after the save completes before exiting, so the file is flushed.
const EXIT_GRACE_FRAMES: u32 = 3;

/// Frames the perf probe discards after uncapping vsync (present-mode switch + pipeline settle).
const PROBE_WARMUP_FRAMES: u32 = 60;

/// Capture lifecycle. Advances one step per frame in [`drive_capture`].
#[derive(Clone, Copy)]
enum Phase {
    /// Waiting for terrain streaming to report the scene fully resident.
    Streaming,
    /// Resident; counting settle frames before the shot.
    Settling(u32),
    /// fxview only: scene settled, fixture armed; waiting for the effect to attach and run its
    /// requested age before the shot.
    FxAging,
    /// Perf-probe mode (`$WOW_FPS_PROBE`): vsync off, discarding warm-up frames.
    ProbeWarmup(u32),
    /// Perf-probe mode: sampling frame times until the target count, then print + exit.
    Probing(u32),
    /// Screenshot requested; waiting for the async save (`Capturing` marker) to appear and clear.
    Saving { frames: u32, seen: bool },
    /// Save done; a few grace frames, then `AppExit`.
    Done(u32),
}

#[derive(Resource)]
struct CaptureCtx {
    scenario: Scenario,
    out: String,
    phase: Phase,
    /// UI fixture already seeded (once, at residency) — see [`seed_ui_fixture`].
    ui_seeded: bool,
    /// `$WOW_FPS_PROBE` — sample this many frames (vsync off) instead of screenshotting, then print
    /// frame-time stats + scene counts and exit. The repeatable perf instrument: same scenario, same
    /// settle, numbers instead of pixels. 0 = normal capture.
    probe_frames: u32,
    /// Probe samples (frame ms).
    probe_samples: Vec<f32>,
}

pub(crate) struct CapturePlugin;

impl Plugin for CapturePlugin {
    fn build(&self, app: &mut App) {
        let name = std::env::var("WOW_CAPTURE").unwrap_or_default();
        // The fxview instrument: a synthetic scenario (ground scene, noon) + the fixture
        // request from env. Not in SCENARIOS — `scripts/visual.sh`'s golden sweep must never
        // run it (its output depends on the model/age/angle knobs, not just the name).
        let scenario = if name == "fxview" {
            let Ok(model_path) = std::env::var("WOW_FX_MODEL") else {
                eprintln!("WOW_CAPTURE=fxview needs WOW_FX_MODEL=<internal .mdx/.m2 path>");
                std::process::exit(2);
            };
            let knob = |k: &str, d: f32| {
                std::env::var(k)
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(d)
            };
            app.insert_resource(FxViewRequest {
                model_path,
                age: knob("WOW_FX_AGE", 1.0),
                az_deg: knob("WOW_FX_AZ", 0.0),
                el_deg: knob("WOW_FX_EL", 10.0),
                dist: knob("WOW_FX_DIST", 5.0),
                fly: knob("WOW_FX_FLY", 0.0),
                yaw_deg: knob("WOW_FX_YAW", 0.0),
                turn: knob("WOW_FX_TURN", 0.0),
                ground: knob("WOW_FX_GROUND", 0.0) > 0.5,
                hold: knob("WOW_FX_HOLD", 0.0) > 0.5,
            })
            .init_resource::<FxViewState>();
            Scenario {
                name: "fxview",
                eye: GROUND_EYE, // overridden per frame by the orbit in `pin_scene`
                look: FXVIEW_POS,
                minute: 720,
                ui: None,
            }
        } else if name == "waterfx" {
            // The water-foam viewer (see `water_fx::view`): a synthetic wading unit over a
            // synthetic wet lattice, framed by a fixed orbit around the rig centre. Knobs:
            // WOW_WFX_MODE (ring|wake|turn), WOW_WFX_SPEED (yd/s), WOW_WFX_AGE (s),
            // WOW_WFX_DEPTH (yd), camera WOW_WFX_AZ/EL/DIST. Not a golden scenario.
            let knob = |k: &str, d: f32| {
                std::env::var(k)
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(d)
            };
            let mode = match std::env::var("WOW_WFX_MODE").as_deref() {
                Ok("wake") => crate::water_fx::WfxMode::Wake,
                Ok("turn") => crate::water_fx::WfxMode::Turn,
                _ => crate::water_fx::WfxMode::Ring,
            };
            // Rig centre in raw WoW coords: over the Northshire ground scene, the synthetic
            // surface a few yards above the terrain so the backdrop plane reads clean.
            let center = [-8961.0_f32, -145.0, 95.0];
            let az = knob("WOW_WFX_AZ", 180.0).to_radians();
            let el = knob("WOW_WFX_EL", 35.0).to_radians();
            let dist = knob("WOW_WFX_DIST", 14.0);
            let eye = [
                center[0] + dist * el.cos() * az.cos(),
                center[1] + dist * el.cos() * az.sin(),
                center[2] + dist * el.sin(),
            ];
            app.insert_resource(crate::water_fx::WaterFxView {
                mode,
                speed: knob("WOW_WFX_SPEED", 4.0),
                age: knob("WOW_WFX_AGE", 1.5),
                center,
                depth: knob("WOW_WFX_DEPTH", 0.5),
            })
            .init_resource::<FxViewState>();
            Scenario {
                name: "waterfx",
                eye,
                look: center,
                minute: 720,
                ui: None,
            }
        // By name, EITHER table: the blessed six or an on-demand fixture. Only the sweep is
        // narrowed — every old viewpoint is still capturable by name (decision 0632).
        } else if let Some(&s) = SCENARIOS
            .iter()
            .chain(scenarios::ON_DEMAND.iter())
            .find(|s| s.name == name)
        {
            s
        } else {
            let known: Vec<_> = SCENARIOS
                .iter()
                .chain(scenarios::ON_DEMAND.iter())
                .map(|s| s.name)
                .collect();
            eprintln!(
                "WOW_CAPTURE={name:?} is not a known scenario; choose one of: {known:?} (or fxview, waterfx)"
            );
            std::process::exit(2);
        };
        let out = std::env::var("WOW_CAPTURE_OUT")
            .unwrap_or_else(|_| format!("target/visual/{}.png", scenario.name));
        if let Some(parent) = Path::new(&out).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "capture: cannot create output dir {}: {e}",
                    parent.display()
                );
                std::process::exit(2);
            }
        }
        app.insert_resource(CaptureMode)
            .insert_resource(CaptureCtx {
                scenario,
                out,
                phase: Phase::Streaming,
                ui_seeded: false,
                probe_frames: std::env::var("WOW_FPS_PROBE")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0),
                probe_samples: Vec::new(),
            })
            .add_systems(Update, pin_scene.in_set(WorldStage::Present))
            // Before the UnitFeed pass: the seed stands in for wire data that in live play
            // filled the app caches on EARLIER frames, so the same frame's feeds (item-template
            // / player-req pushes, then the merchant paint) must all see it. Unordered, the
            // one-shot MERCHANT_SHOW paint races feed_item_stats and the usable reds are
            // flaky in the capture.
            .add_systems(Update, seed_ui_fixture.before(crate::ui_unit::UnitFeed))
            .add_systems(Last, drive_capture);
    }
}

/// Each frame, force the deterministic capture conditions: pinned time-of-day, no perf HUD, and the
/// fixed camera pose. Runs in `WorldStage::Present` (after `control` is gated off and after terrain
/// streaming reads the camera), so the harness is the sole, stable author of the view.
#[allow(clippy::too_many_arguments)]
fn pin_scene(
    ctx: Res<CaptureCtx>,
    mut debug: ResMut<DebugState>,
    mut perf: ResMut<PerfHud>,
    fx_req: Option<Res<FxViewRequest>>,
    fx_state: Option<Res<FxViewState>>,
    mut player: ResMut<crate::player::Player>,
    roots: Query<&Transform, Without<WorldCamera>>,
    mut cam: Query<&mut Transform, With<WorldCamera>>,
) {
    debug.lighting.follow_server_time = false;
    debug.lighting.manual_minute = ctx.scenario.minute;
    perf.visible = false; // the perf HUD is default-on; suppress it for a pristine, UI-free shot

    // `WOW_MM_PROBE=x,y,z` (raw WoW coords) drops the player at that point and marks them active, so
    // the interior minimap (which keys off `player.active` + `player.pos`) renders in a headless
    // capture — the harness otherwise leaves the player inactive at spawn, so interiors never show.
    // The camera looks straight down from above so the WMO streams in around it. Interior-minimap
    // debugging instrument (decision 0203 arc), not a golden scenario.
    let probe = std::env::var("WOW_MM_PROBE").ok().and_then(|s| {
        let v: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        (v.len() == 3).then(|| [v[0], v[1], v[2]])
    });
    if let Some(p) = probe {
        player.pos = wow_to_bevy(p);
        player.active = true;
        player.detached = false;
        // Above and off to the side (not straight down — that degenerates `looking_at` with Y up).
        let eye = wow_to_bevy([p[0] - 25.0, p[1] - 25.0, p[2] + 40.0]);
        for mut t in &mut cam {
            *t = Transform::from_translation(eye).looking_at(wow_to_bevy(p), Vec3::Y);
        }
        return;
    }

    let (eye, look) = match (&fx_req, &fx_state) {
        // fxview: orbit the fixture's LIVE root (a flying missile moves; the camera tracks it)
        // at the requested azimuth/elevation/distance, aimed one yard up — the effect models
        // author their bodies ~0.5–1.5 units above their root.
        (Some(req), Some(state)) => {
            let root_pos = state
                .root
                .and_then(|r| roots.get(r).ok())
                .map(|t| t.translation)
                .unwrap_or_else(|| wow_to_bevy(FXVIEW_POS));
            let look = root_pos + Vec3::Y;
            let orbit = Quat::from_rotation_y(req.az_deg.to_radians())
                * Quat::from_rotation_x(-req.el_deg.to_radians());
            (look + orbit * (Vec3::Z * req.dist), look)
        }
        _ => (
            wow_to_bevy(ctx.scenario.eye),
            wow_to_bevy(ctx.scenario.look),
        ),
    };
    for mut t in &mut cam {
        *t = Transform::from_translation(eye).looking_at(look, Vec3::Y);
    }
}

/// Drive the capture lifecycle: wait for streaming, settle, screenshot, exit.
#[allow(clippy::too_many_arguments)]
fn drive_capture(
    mut ctx: ResMut<CaptureCtx>,
    progress: Res<WorldLoadProgress>,
    capturing: Query<(), With<Capturing>>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
    time: Res<Time<bevy::time::Real>>,
    mut windows: Query<&mut Window, With<bevy::window::PrimaryWindow>>,
    particles: Query<&crate::particles::ParticleEmitter>,
    parts: Query<&ViewVisibility, With<crate::debug_panel::ModelPart>>,
    entities: Query<()>,
    fx_req: Option<Res<FxViewRequest>>,
    wfx_req: Option<Res<crate::water_fx::WaterFxView>>,
    mut fx_state: Option<ResMut<FxViewState>>,
    game_time: Res<Time>,
) {
    let resident =
        progress.total > 0 && progress.ready == progress.total && progress.focus_resident;
    // Both fixture viewers (fxview / waterfx) share the settle→arm→age→shoot flow; only the
    // requested age differs.
    let fixture_age = fx_req
        .as_ref()
        .map(|r| r.age)
        .or(wfx_req.as_ref().map(|r| r.age));
    ctx.phase = match ctx.phase {
        Phase::Streaming => {
            if resident {
                Phase::Settling(0)
            } else {
                Phase::Streaming
            }
        }
        Phase::FxAging => {
            // The fixture was armed when settling completed; shoot once it has attached and run
            // its requested age on the game clock.
            let aged = match (fixture_age, &fx_state) {
                (Some(age), Some(state)) => state
                    .attached_at
                    .is_some_and(|t0| game_time.elapsed_secs() - t0 >= age),
                _ => true,
            };
            if aged {
                commands
                    .spawn(Screenshot::primary_window())
                    .observe(save_to_disk(ctx.out.clone()));
                info!("capture: effect aged, writing {}", ctx.out);
                Phase::Saving {
                    frames: 0,
                    seen: false,
                }
            } else {
                Phase::FxAging
            }
        }
        Phase::Settling(n) => {
            if !resident {
                Phase::Streaming // a late tile dropped out of residency — wait for it again
            } else if n + 1 >= settle_frames() && fixture_age.is_some() {
                if let Some(state) = fx_state.as_deref_mut() {
                    state.armed = true; // scene ready — the fixture spawns now, age clock clean
                }
                Phase::FxAging
            } else if n + 1 >= settle_frames() {
                if ctx.probe_frames > 0 {
                    // Probe: uncap presentation so we measure true frame cost, not the vsync
                    // ceiling. `$WOW_PROBE_VSYNC=1` keeps vsync ON instead — the probe then
                    // measures the PRESENT ceiling itself (what fps the display sync actually
                    // grants this window), the instrument for "what is the vsync cap right now".
                    let keep_vsync = std::env::var("WOW_PROBE_VSYNC").as_deref() == Ok("1");
                    if !keep_vsync {
                        if let Ok(mut w) = windows.single_mut() {
                            w.present_mode = bevy::window::PresentMode::AutoNoVsync;
                        }
                    }
                    info!(
                        "probe: settled; vsync {}, warming {PROBE_WARMUP_FRAMES} frames",
                        if keep_vsync { "KEPT ON" } else { "off" }
                    );
                    Phase::ProbeWarmup(0)
                } else {
                    commands
                        .spawn(Screenshot::primary_window())
                        .observe(save_to_disk(ctx.out.clone()));
                    info!("capture: scene settled, writing {}", ctx.out);
                    Phase::Saving {
                        frames: 0,
                        seen: false,
                    }
                }
            } else {
                Phase::Settling(n + 1)
            }
        }
        Phase::ProbeWarmup(n) => {
            if n + 1 >= PROBE_WARMUP_FRAMES {
                Phase::Probing(0)
            } else {
                Phase::ProbeWarmup(n + 1)
            }
        }
        Phase::Probing(n) => {
            let ms = time.delta_secs() * 1000.0;
            ctx.probe_samples.push(ms);
            if n + 1 >= ctx.probe_frames {
                let mut v = ctx.probe_samples.clone();
                v.sort_by(f32::total_cmp);
                let at = |q: f32| v[(((v.len() - 1) as f32) * q).round() as usize];
                let mean = v.iter().sum::<f32>() / v.len() as f32;
                let (emitters, active, live) = particles
                    .iter()
                    .fold((0usize, 0usize, 0usize), |(e, a, l), p| {
                        (e + 1, a + usize::from(p.live() > 0), l + p.live())
                    });
                let px = windows
                    .single()
                    .map(|w| (w.physical_width(), w.physical_height()))
                    .unwrap_or((0, 0));
                // Scene population: model submeshes (the per-frame visibility walk's N), how many
                // survived the cull to render, and the whole-world entity count — the scale terms
                // behind every O(N) per-frame cost (the Stormwind fps hunt's instrument).
                let (submeshes, drawn) = parts.iter().fold((0usize, 0usize), |(n, d), v| {
                    (n + 1, d + usize::from(v.get()))
                });
                let entity_count = entities.iter().len();
                // Machine-greppable one-liner + a human block. stdout, not the log, so a script can
                // capture it without log-filter noise.
                println!(
                    "FPS_PROBE scenario={} frames={} mean_ms={mean:.2} p50_ms={:.2} p95_ms={:.2} p99_ms={:.2} max_ms={:.2} fps={:.1} emitters={emitters} active={active} particles={live} submeshes={submeshes} drawn={drawn} entities={entity_count} px={}x{}",
                    ctx.scenario.name,
                    v.len(),
                    at(0.50),
                    at(0.95),
                    at(0.99),
                    v[v.len() - 1],
                    1000.0 / mean,
                    px.0,
                    px.1,
                );
                exit.write(AppExit::Success);
                Phase::Probing(n)
            } else {
                Phase::Probing(n + 1)
            }
        }
        Phase::Saving { frames, seen } => {
            let busy = !capturing.is_empty();
            let seen = seen || busy;
            // The save spans a few frames; `Capturing` marks it in-flight. Done once it has appeared
            // and cleared — or on timeout, so a missed marker can't hang the harness.
            if (seen && !busy) || frames + 1 >= SAVE_TIMEOUT_FRAMES {
                Phase::Done(0)
            } else {
                Phase::Saving {
                    frames: frames + 1,
                    seen,
                }
            }
        }
        Phase::Done(n) => {
            if n + 1 >= EXIT_GRACE_FRAMES {
                info!("capture: saved {}, exiting", ctx.out);
                exit.write(AppExit::Success);
                Phase::Done(n)
            } else {
                Phase::Done(n + 1)
            }
        }
    };
}
