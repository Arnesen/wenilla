//! The live frame-time sample ([`LiveFpsPlugin`]) and everything printed from the same sample
//! window: the `FPS_PROBE` line itself, the visible-submesh census
//! (`VIS_CENSUS`/`VIS_ESCAPED`/`VIS_DUMP`), the asset-churn ratchet (`MAT_CHURN`) and the
//! residency meter (`ASSET_DUMP`). One file because one system, `drive_live_fps`, prints all four
//! from the same frame at the same instant — they are one measurement with four line families,
//! not four instruments.
//!
//! **What is counted lives in [`benilla_world::world_census`]; what is printed lives here.** The census
//! is the engine's own published account of the frame it drew (decision 1164) — this probe adds
//! the timing window around it and owns the line shapes, which are a greppable contract of ours
//! and no business of the renderer's.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::capture::PROBE_WARMUP_FRAMES;
use benilla_world::world_census::WorldCensus;

/// The LIVE FPS probe (`WOW_LIVE_FPS=<frames>`, delay via `WOW_LIVE_FPS_AT` seconds, default 25;
/// `WOW_LIVE_FPS_MOVE=1` holds W through warmup + sampling, so the probe measures RUNNING through
/// the scene — streaming, spawns, re-classification — not a parked camera; the 0366 hunt's
/// "running around SW" gap):
/// the [`crate::capture::CapturePlugin`] probe's numbers on a NORMAL connected run — streamed units, net
/// apply, quest markers, everything the server-less harness deliberately excludes. Built for the
/// 0362 residual: the serverless stormwind probe pinned 60 while the director's live session read
/// 20, so the gap IS the live world — this instrument measures it. Waits for in-world + the delay
/// (park the character first with [`super::ProbeChatPlugin`]), uncaps vsync, warms
/// [`PROBE_WARMUP_FRAMES`], samples, prints the same machine-greppable `FPS_PROBE` line
/// (scenario=`live`), and exits.
pub(crate) struct LiveFpsPlugin;

/// How long past `WOW_LIVE_FPS_AT` the probe waits for a world before declaring the run dead.
/// Entry normally lands well inside `at` itself; 60 s of grace covers a cold-cache load without
/// ever letting a stranded run ride out a harness timeout (see the `Waiting` arm).
const BOOT_DEADLINE_SECS: f32 = 60.0;

impl Plugin for LiveFpsPlugin {
    fn build(&self, app: &mut App) {
        let frames = std::env::var("WOW_LIVE_FPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300);
        let at = std::env::var("WOW_LIVE_FPS_AT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(25.0);
        app.insert_resource(LiveFps {
            frames,
            at,
            run: std::env::var("WOW_LIVE_FPS_MOVE").as_deref() == Ok("1"),
            phase: LiveFpsPhase::Waiting,
            samples: Vec::new(),
            cpu_at_start: None,
            sys_at_start: None,
            occluded_now: false,
            occluded_frames: 0,
        })
        .add_systems(Update, drive_live_fps);
        WorldCensus::churn_counters(app);
        // The engine counts its own materials; the UI pass is ours, so we fold it into the same
        // tally rather than keeping a second one that could disagree about the window.
        WorldCensus::count_churn::<crate::ui_pass::UiQuadMaterial>(app, "uiquad");
    }
}

#[derive(Clone, Copy, PartialEq)]
enum LiveFpsPhase {
    Waiting,
    Warmup(u32),
    Sampling,
    Done,
}

/// [`LiveFpsPlugin`] state.
#[derive(Resource)]
struct LiveFps {
    frames: usize,
    at: f32,
    /// Hold W while measuring (`WOW_LIVE_FPS_MOVE=1`) — the moving-workload probe.
    run: bool,
    phase: LiveFpsPhase,
    samples: Vec<f32>,
    /// Process CPU seconds at the first sampled frame ([`crate::perf::process_cpu_secs`]) — the
    /// baseline for the window's `cpu_ms`/`cpu_pct`.
    cpu_at_start: Option<f64>,
    /// Machine-wide CPU ticks at the first sampled frame ([`crate::perf::system_cpu_ticks`]) — the
    /// baseline for `sys_busy_pct`, which says whether anyone ELSE was competing for the cores
    /// while this leg ran. `cpu_ms` alone cannot: see that function's header for the 25.93-vs-18.8
    /// leg that motivated it (1157).
    sys_at_start: Option<(u64, u64)>,
    /// The window's occlusion state, maintained from `WindowOccluded` transitions. macOS
    /// throttles a FULLY covered window to ~1 fps (any covering window, not just the lock
    /// screen — the director's correction to 0729/0730's lock-screen reading): a probe launched
    /// detached spawns unfocused and can land completely behind other windows, and its leg then
    /// measures the throttle, not the client.
    occluded_now: bool,
    /// Sampled frames taken while occluded — `occluded_frames=` on the probe line, so a
    /// throttled leg names itself instead of being inferred from a ~1 s hitch signature.
    occluded_frames: usize,
}

/// **Where** the sample was taken — the 0705 prove-the-run law: a probe number is evidence only
/// once the body is known to be at the pin, and `WOW_PROBE_CHAT`'s `.go` can silently fail (a bad
/// map id, a refused command) leaving the run measuring the login spot.
///
/// The *scene-state* half of the pin — which room the camera claims, how many exterior windows,
/// what the cull did — is the engine's, and arrives with the rest of the census. Without it a
/// `drawn=` reading taken indoors cannot be read at all: a big number means either "we claimed a
/// room and the cull let everything through" or "we never claimed a room, so nothing was gated" —
/// opposite bugs with identical numbers, and the difference cost a measurement (0780).
#[derive(SystemParam)]
struct SamplePin<'w> {
    map: Option<Res<'w, benilla_world::world_map::CurrentMap>>,
    body: Option<Res<'w, crate::player::Player>>,
}

/// Wait for in-world + the delay, uncap, warm, sample, print, exit — the live twin of the
/// harness probe's `Phase::ProbeWarmup`/`Probing` arms.
#[allow(clippy::too_many_arguments)]
fn drive_live_fps(
    mut probe: ResMut<LiveFps>,
    time: Res<Time<bevy::time::Real>>,
    self_player: Query<(), With<crate::net::SelfPlayer>>,
    mut windows: Query<&mut Window, With<bevy::window::PrimaryWindow>>,
    // What the engine drew this frame and what it is holding — submeshes, the exterior-scene
    // gate, emitters, resident assets, the churn window. One param, one instant.
    mut census: WorldCensus,
    streamed: Query<(), With<crate::net::NetEntity>>,
    // The animation-LOD gate's effect, machine-readable per probe (decision 0448): how many
    // streamed rigs sat parked at sample end.
    parked: Query<(), With<benilla_world::rig_anim::AnimParked>>,
    entities: Query<()>,
    pin: SamplePin,
    // The owned skin-palette occupancy (decision 0720) — `rigs=live/peak bones=live/peak` on the
    // probe line proves the palette lane is actually populated (an all-zero table renders
    // origin-collapsed rigs, which no other probe number would catch).
    palettes: Option<Res<benilla_world::rig_palette::RigPalettes>>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut key_events: MessageWriter<bevy::input::keyboard::KeyboardInput>,
    mut exit: MessageWriter<AppExit>,
    mut occlusions: MessageReader<bevy::window::WindowOccluded>,
    monitors: Query<&bevy::window::Monitor>,
) {
    // Drain every frame so the state is current whichever phase we're in — the window can be
    // occluded before sampling ever starts (a detached launch spawns behind whatever is open).
    for o in occlusions.read() {
        probe.occluded_now = o.occluded;
    }
    match probe.phase {
        LiveFpsPhase::Done => {}
        LiveFpsPhase::Waiting => {
            // (The un-occludable window that keeps the SETTLE phase from streaming the world at
            // ~1 fps is [`ProbeFocusPlugin`]'s now — every probe needs it, not just this one.)
            if time.elapsed_secs() < probe.at || self_player.is_empty() {
                // The boot deadline: entering the world takes seconds, and `at` already grants a
                // settle window on top — a run still worldless this far past it is stranded on a
                // glue screen (dead server, refused login the marker arms didn't catch) and every
                // second more is the 1371 sitting's dead wall-clock again. `FATAL` is the marker
                // leg.sh keys on.
                if time.elapsed_secs() > probe.at + BOOT_DEADLINE_SECS {
                    error!(
                        "live-fps: FATAL — still not in world {:.0}s past the probe delay; a measurement run with no world is dead. exiting",
                        BOOT_DEADLINE_SECS
                    );
                    exit.write(AppExit::error());
                    probe.phase = LiveFpsPhase::Done;
                }
                return;
            }
            if let Ok(mut w) = windows.single_mut() {
                w.present_mode = crate::capture::probe_uncap_mode();
            }
            info!(
                "live-fps: in-world + settled; vsync off, warming {PROBE_WARMUP_FRAMES} frames{}",
                if probe.run { ", holding W" } else { "" }
            );
            if probe.run {
                // A synthetic held key: `ButtonInput` persists a press until its release, and the
                // winit feed only releases keys it saw go down, so this holds across frames. The
                // raw KeyboardInput message rides along for the binding dispatch's press edge
                // (0997 — MOVEFORWARD latches off the event, holds off the state).
                keys.press(KeyCode::KeyW);
                key_events.write(bevy::input::keyboard::KeyboardInput {
                    key_code: KeyCode::KeyW,
                    logical_key: bevy::input::keyboard::Key::Unidentified(
                        bevy::input::keyboard::NativeKey::Unidentified,
                    ),
                    state: bevy::input::ButtonState::Pressed,
                    text: None,
                    repeat: false,
                    window: Entity::PLACEHOLDER,
                });
            }
            probe.phase = LiveFpsPhase::Warmup(0);
        }
        LiveFpsPhase::Warmup(n) => {
            probe.phase = if n + 1 >= PROBE_WARMUP_FRAMES {
                LiveFpsPhase::Sampling
            } else {
                LiveFpsPhase::Warmup(n + 1)
            };
        }
        LiveFpsPhase::Sampling => {
            if probe.samples.is_empty() {
                probe.cpu_at_start = crate::perf::process_cpu_secs();
                probe.sys_at_start = crate::perf::system_cpu_ticks();
                // The churn census restarts with the window — warmup noise (streaming, shader
                // warms) would otherwise read as steady-state ratchets.
                census.restart_churn();
                probe.occluded_frames = 0;
            }
            if probe.occluded_now {
                probe.occluded_frames += 1;
            }
            let ms = time.delta_secs() * 1000.0;
            probe.samples.push(ms);
            if probe.samples.len() < probe.frames {
                return;
            }
            let mut v = probe.samples.clone();
            v.sort_by(f32::total_cmp);
            let at = |q: f32| v[(((v.len() - 1) as f32) * q).round() as usize];
            let mean = v.iter().sum::<f32>() / v.len() as f32;
            let seen = census.take();
            let px = windows
                .single()
                .map(|w| (w.physical_width(), w.physical_height()))
                .unwrap_or((0, 0));
            // The present mode actually measured under — an uncap that silently rails (0362) is
            // only diagnosable if the line says what was asked for.
            let present = windows
                .single()
                .map(|w| format!(" present={:?}", w.present_mode))
                .unwrap_or_default();
            // Which display the window sits on, and that display's refresh rate — the regime a
            // railed leg is railed AT. Two legs of one sitting have read 126.5 vs exactly 60.0 fps
            // under the same uncapped present mode (1388 round 3, 1395's pin), and nothing on the
            // line said why: this machine drives a 60 Hz external beside a 120 Hz built-in, and
            // present=AutoNoVsync resolves to Metal displaySync=false either way — the rail, when
            // one appears, is the WindowServer's, keyed to the display. Stamp it so a regime split
            // reads off the line instead of costing a discarded round.
            let display = {
                let center = windows.single().ok().and_then(|w| match w.position {
                    bevy::window::WindowPosition::At(p) => {
                        Some(p + bevy::math::IVec2::new(px.0 as i32, px.1 as i32) / 2)
                    }
                    _ => None,
                });
                match center {
                    Some(c) => monitors
                        .iter()
                        .find(|m| {
                            let min = m.physical_position;
                            let max = min
                                + bevy::math::IVec2::new(
                                    m.physical_width as i32,
                                    m.physical_height as i32,
                                );
                            c.x >= min.x && c.x < max.x && c.y >= min.y && c.y < max.y
                        })
                        .map(|m| {
                            format!(
                                " display={}@{}",
                                m.name.as_deref().unwrap_or("?").replace(' ', "-"),
                                m.refresh_rate_millihertz
                                    .map(|mhz| format!("{:.0}", mhz as f64 / 1000.0))
                                    .unwrap_or_else(|| "?".into())
                            )
                        })
                        .unwrap_or_else(|| " display=none-contains-center".to_string()),
                    None => " display=unpositioned".to_string(),
                }
            };
            // CPU cost per frame across every thread — the load-robust half of the measurement
            // (`perf::process_cpu_secs`), and directly comparable with a reporter's CPU %.
            let cpu = match (probe.cpu_at_start, crate::perf::process_cpu_secs()) {
                (Some(t0), Some(t1)) => {
                    let per_frame_ms = (t1 - t0) * 1000.0 / v.len() as f64;
                    format!(
                        " cpu_ms={per_frame_ms:.2} cpu_pct={:.0}",
                        per_frame_ms / mean as f64 * 100.0
                    )
                }
                _ => String::new(),
            };
            // How busy the WHOLE machine was across the same window — every core, every process.
            // `cpu_ms` is load-ROBUST, not load-immune (1157: the same pin leg read 25.93 with two
            // other slots compiling and 18.80-20.12 quiet), so a leg that does not say this cannot
            // be compared with one taken at a different time. A stamp, never a gate: legs at
            // similar `sys_busy_pct` are comparable, legs far apart are not.
            let sys = match (probe.sys_at_start, crate::perf::system_cpu_ticks()) {
                (Some((b0, t0)), Some((b1, t1))) if t1 > t0 => {
                    format!(
                        " sys_busy_pct={:.0}",
                        (b1 - b0) as f64 / (t1 - t0) as f64 * 100.0
                    )
                }
                _ => String::new(),
            };
            // The pin the number belongs to, in the `.go xyz` order, so a probe line can be
            // matched against the report's coordinates without a second instrument — followed by
            // the exterior-scene gate's state, which is what makes `drawn=` legible indoors.
            let at_pin = match (pin.map.as_ref(), pin.body.as_ref().filter(|b| b.active)) {
                (Some(m), Some(b)) => {
                    let [x, y, z] = benilla_assets::coords::bevy_to_wow(b.pos);
                    format!(" map={} pos={x:.1},{y:.1},{z:.1}", m.0)
                }
                _ => String::new(),
            };
            let gate = match (seen.room.as_deref(), seen.windows.as_deref()) {
                (Some(room), Some(w)) => format!(" room={room} windows={w}"),
                _ => String::new(),
            };
            // Beside `room=`, because the two answer the same question from opposite ends:
            // `room=none` says the camera claims no interior, `sky=` says whether some building's
            // PVS is painting its own backdrop over the world anyway.
            let sky = seen
                .sky
                .as_deref()
                .map(|s| format!(" sky={s}"))
                .unwrap_or_default();
            // The effect stream's other half. `emitters=`/`particles=` above count quad clouds
            // only, so a screen full of ribbon trails reads as an empty scene without this.
            let ribbons = seen
                .ribbons
                .map(|(n, d)| format!(" ribbons={n} ribbons_drawn={d}"))
                .unwrap_or_default();
            let culled = match seen.cull.as_ref() {
                Some(v) => format!(
                    " cull_windows={} cull_frusta={} cull_tested={} cull_hidden={} \
                     cull_unbounded={} cull_bodies={} cull_bodies_hidden={}",
                    v.windows, v.frusta, v.tested, v.hidden, v.unbounded, v.bodies, v.bodies_hidden
                ),
                None => String::new(),
            };
            let rigs = palettes
                .map(|p| {
                    let (s, b, ps, pb) = p.occupancy();
                    format!(
                        " rigs={s}/{ps} rig_bones={b}/{pb} rig_computed={}",
                        p.computed_rigs()
                    )
                })
                .unwrap_or_default();
            let residency_line = format!(
                " mats={} meshes={} images={} uv={} tint={}",
                seen.mats, seen.meshes, seen.images, seen.uv_anims, seen.tint_anims,
            );
            println!(
                "FPS_PROBE scenario=live frames={} mean_ms={mean:.2} p50_ms={:.2} p95_ms={:.2} p99_ms={:.2} max_ms={:.2} fps={:.1} emitters={} active={} particles={} submeshes={} drawn={} streamed={} parked={} entities={}{rigs}{residency_line} px={}x{}{cpu}{sys}{present}{display} occluded_frames={}{at_pin}{gate}{sky}{ribbons}{culled}",
                v.len(),
                at(0.50),
                at(0.95),
                at(0.99),
                v[v.len() - 1],
                1000.0 / mean,
                seen.emitters,
                seen.active_emitters,
                seen.particles,
                seen.submeshes,
                seen.drawn,
                streamed.iter().len(),
                parked.iter().len(),
                entities.iter().len(),
                px.0,
                px.1,
                probe.occluded_frames,
            );
            print_vis_census(&seen);
            // The window's Modified-event totals per asset type — a type at ~1×/frame here is a
            // per-frame re-upload ratchet; absent means quiet.
            if !seen.churn.is_empty() {
                let churn = seen
                    .churn
                    .iter()
                    .map(|(k, n)| format!("{k}={n}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!("MAT_CHURN frames={} {churn}", v.len());
            }
            if std::env::var_os("WOW_ASSET_DUMP").is_some() {
                let (lines, unpathed) = census.resident_assets();
                for l in &lines {
                    println!("ASSET_DUMP {l}");
                }
                println!(
                    "ASSET_DUMP <unpathed> images={} meshes={} models={} materials={}",
                    unpathed[0], unpathed[1], unpathed[2], seen.mats
                );
            }
            if probe.run {
                keys.release(KeyCode::KeyW);
            }
            probe.phase = LiveFpsPhase::Done;
            exit.write(AppExit::Success);
        }
    }
}

/// **What is on the screen, and who is accountable for it** — the `VIS_CENSUS` line beside
/// `FPS_PROBE`, plus a per-model breakdown under `WOW_VIS_DUMP=1`.
///
/// `drawn=` alone cannot answer "why can I still see that from in here?". It is one number over
/// every model submesh, and the interesting split is not visible-vs-not: it is which *subsystem* the
/// visible thing belongs to and whether anything is gating it at all. A tree that draws through a
/// wall is a completely different defect depending on whether it is tagged as exterior scene
/// (tagged but admitted — the cull or the bound is wrong) or is not (nothing is gating it — the
/// wrong lane spawned it). Naming which took a screenshot, an asset dig and a wrong guess; this
/// line answers it in one run.
///
/// `WOW_VIS_DUMP=1` then names the models: one `VIS_DUMP` line per distinct visible label, ungated
/// first, most-drawn first — which is the "so WHICH trees are they?" question. The counting is
/// [`benilla_world::world_census`]'s; the line shapes below are ours.
fn print_vis_census(seen: &benilla_world::world_census::CensusReport) {
    let line = seen
        .kinds
        .iter()
        .map(|(name, vis, gated)| format!("{name}={vis}/gated{gated}"))
        .collect::<Vec<_>>()
        .join(" ");
    println!(
        "VIS_CENSUS visible-submeshes {line} | tagged={} hidden={} exempt={} no_aabb={}",
        seen.tagged, seen.hidden, seen.exempt, seen.no_aabb
    );
    // The escapees always print: a tagged, bounded object the cull left un-hidden is a defect by
    // construction, and burying it behind a flag is how it stays unnoticed.
    for (label, card, n) in &seen.escaped {
        let c = if *card { " BILLBOARD-CARD" } else { "" };
        println!("VIS_ESCAPED {n}{c} {label}");
    }
    if std::env::var_os("WOW_VIS_DUMP").is_none() {
        return;
    }
    for (label, gated, n) in &seen.labels {
        let g = if *gated { "gated" } else { "UNGATED" };
        println!("VIS_DUMP {g} {n} {label}");
    }
}
