//! The LIVE-run probe instruments — every plugin here rides a NORMAL connected session (unlike
//! the parent's server-less [`super::CapturePlugin`] harness): scripted chat sends
//! ([`ProbeChatPlugin`]), synthetic key taps ([`ProbeKeyPlugin`]), a Lua chunk in the live UI VM
//! ([`ProbeLuaPlugin`]), the bounded-lifetime self-exit ([`ProbeExitPlugin`]), and the live
//! frame-time sampler ([`LiveFpsPlugin`]). The live screenshot and its validity gates live in the
//! sibling [`super::live_shot`]. Each is env-gated and registered by `main`; compose them for
//! unattended "park, act, observe" probes.

use bevy::prelude::*;

use super::PROBE_WARMUP_FRAMES;

/// The PROBE CHAT one-shot (`WOW_PROBE_CHAT="<line>[;<line>…]"`, delay via `WOW_PROBE_CHAT_AT`
/// seconds, default 8): send each `;`-separated line as Say once we are in-world — the "park the
/// probe character anywhere" instrument. The probe account (gmlevel 6) makes `.go xyz …`, `.gm on`,
/// `.additem` etc. work headlessly, which a direct `characters` DB edit does NOT (the live world
/// server's logout save overwrites it, and the row is only re-read at login). Pair with
/// [`LiveShotPlugin`] at a later `WOW_LIVE_SHOT_AT` so the destination has streamed in.
/// `WOW_PROBE_CHAT_EVERY=<secs>` spaces the lines apart instead of sending them in one burst —
/// the "do X, wait, then do Y" probe (a mount-then-dismount transition, a buff-then-cancel):
/// two field flips inside one drain merge to a no-op, so time-separated sends are what actually
/// exercise a transition (decision 0441's teardown verification).
pub(crate) struct ProbeChatPlugin;

impl Plugin for ProbeChatPlugin {
    fn build(&self, app: &mut App) {
        let lines = std::env::var("WOW_PROBE_CHAT").unwrap_or_default();
        let at = std::env::var("WOW_PROBE_CHAT_AT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8.0);
        let every = std::env::var("WOW_PROBE_CHAT_EVERY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);
        app.insert_resource(ProbeChat {
            lines,
            at,
            every,
            sent: 0,
        })
        .add_systems(Update, fire_probe_chat);
    }
}

/// [`ProbeChatPlugin`] state: the `;`-separated lines, the first-fire time, the per-line spacing
/// (`0` = one burst), and how many lines have gone out.
#[derive(Resource)]
struct ProbeChat {
    lines: String,
    at: f32,
    every: f32,
    sent: usize,
}

/// The PROBE KEY one-shots (`WOW_PROBE_KEY="<key>@<secs>[:<hold>][;…]"`): synthesize a key press
/// at each given time once in-world, released `<hold>` seconds later ([`PROBE_KEY_TAP_SECS`] when
/// omitted — the tap this instrument shipped with). The "press space headlessly" instrument for
/// input-gated behavior (the mounted flourish, a jump, the X/Z toggles), which neither a chat
/// command nor a Lua chunk can reach (1.12 has no jump Lua API; the gate lives in the
/// controller's key read).
///
/// The optional hold is what makes *sustained* locomotion reachable headlessly: a 0.25 s W tap
/// travels ~1.2 yd, far too little to cross a liquid surface's own slope, so a swim defect that
/// only appears while moving over water could not be reproduced without asking the director to
/// drive (decision 0644 — the gap `WOW_PROBE_LOOK` closed for mouse-turns, on the key side).
///
/// Runs in `PreUpdate`
/// after winit's input processing ([`bevy::input::InputSystems`]) so the synthetic
/// `just_pressed` is visible to every `Update` reader that same frame — a press from inside
/// `Update` would be cleared at the next frame's input pass before an earlier-ordered
/// controller ever saw it.
pub(crate) struct ProbeKeyPlugin;

impl Plugin for ProbeKeyPlugin {
    fn build(&self, app: &mut App) {
        let spec = std::env::var("WOW_PROBE_KEY").unwrap_or_default();
        let taps = spec
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter_map(|s| {
                let (key, rest) = s.split_once('@')?;
                let (at, hold) = match rest.split_once(':') {
                    Some((at, hold)) => (at, hold.trim().parse::<f32>().ok()?),
                    None => (rest, PROBE_KEY_TAP_SECS),
                };
                match (probe_key_by_name(key.trim()), at.trim().parse::<f32>()) {
                    (Some(key), Ok(at)) => Some(ProbeKeyTap {
                        key,
                        at,
                        hold,
                        pressed: false,
                        released: false,
                    }),
                    _ => {
                        warn!("probe-key: unparseable tap {s:?} (want e.g. Space@14 or W@20:6) — skipped");
                        None
                    }
                }
            })
            .collect();
        app.insert_resource(ProbeKeys { taps }).add_systems(
            bevy::app::PreUpdate,
            fire_probe_key.after(bevy::input::InputSystems),
        );
    }
}

/// How long a probe press stays held when the spec gives no `:<hold>`. Long enough that a
/// `pressed`-reader (a held-key gate) sees it across several frames; short enough to stay a tap.
const PROBE_KEY_TAP_SECS: f32 = 0.25;

/// The key names [`ProbeKeyPlugin`] accepts — the controller-read set; extend as probes need.
fn probe_key_by_name(name: &str) -> Option<KeyCode> {
    Some(match name {
        "Space" => KeyCode::Space,
        "W" => KeyCode::KeyW,
        "A" => KeyCode::KeyA,
        "S" => KeyCode::KeyS,
        "D" => KeyCode::KeyD,
        "Q" => KeyCode::KeyQ,
        "E" => KeyCode::KeyE,
        "X" => KeyCode::KeyX,
        "Z" => KeyCode::KeyZ,
        "Tab" => KeyCode::Tab,
        _ => return None,
    })
}

/// [`ProbeKeyPlugin`] state: one entry per scheduled tap.
#[derive(Resource)]
struct ProbeKeys {
    taps: Vec<ProbeKeyTap>,
}

struct ProbeKeyTap {
    key: KeyCode,
    at: f32,
    /// Seconds the key stays down — the spec's `:<hold>`, else [`PROBE_KEY_TAP_SECS`].
    hold: f32,
    pressed: bool,
    released: bool,
}

/// Press each due tap (in-world gated, like the chat probe) and release it after its hold window.
fn fire_probe_key(
    mut probe: ResMut<ProbeKeys>,
    time: Res<Time>,
    self_player: Query<(), With<crate::net::SelfPlayer>>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
) {
    if probe.taps.is_empty() || self_player.is_empty() {
        return;
    }
    let now = time.elapsed_secs();
    for tap in &mut probe.taps {
        if !tap.pressed && now >= tap.at {
            info!(
                "probe-key: {:?} down ({now:.1}s, hold {:.2}s)",
                tap.key, tap.hold
            );
            keys.press(tap.key);
            tap.pressed = true;
        } else if tap.pressed && !tap.released && now >= tap.at + tap.hold {
            keys.release(tap.key);
            tap.released = true;
        }
    }
}

/// The PROBE LUA one-shot (`WOW_PROBE_LUA="<chunk>"`, delay via `WOW_PROBE_LUA_AT` seconds,
/// default 10): run one Lua chunk in the live UI VM once we are in-world — the "press the button
/// headlessly" instrument. The chunk drives the REAL FrameXML API surface (`CastSpell`,
/// `UseAction`, `TargetUnit`, …), so whatever it triggers takes the exact app path a click
/// takes — a headless wire probe can measure the server, but only the live VM exercises the
/// button feed and the widget clock.
pub(crate) struct ProbeLuaPlugin;

impl Plugin for ProbeLuaPlugin {
    fn build(&self, app: &mut App) {
        let chunk = std::env::var("WOW_PROBE_LUA").unwrap_or_default();
        let at = std::env::var("WOW_PROBE_LUA_AT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10.0);
        app.insert_resource(ProbeLua {
            chunk,
            at,
            fired: false,
        })
        .add_systems(Update, fire_probe_lua);
    }
}

/// The probe self-termination as its own plugin, registered whenever `WOW_PROBE_EXIT_AT` is set
/// — it used to ride inside [`ProbeLuaPlugin`], so a chat/key-only probe's exit knob silently
/// did nothing (the 0441 flourish probe hung past its window on exactly that).
pub(crate) struct ProbeExitPlugin;

impl Plugin for ProbeExitPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ProbeExit {
            at: std::env::var("WOW_PROBE_EXIT_AT")
                .ok()
                .and_then(|v| v.parse().ok()),
            fired: false,
        })
        .add_systems(Update, fire_probe_exit);
    }
}

/// [`ProbeLuaPlugin`] state: the chunk, the fire time, and the once-latch.
#[derive(Resource)]
struct ProbeLua {
    chunk: String,
    at: f32,
    fired: bool,
}

/// The probe run's clean self-termination (`WOW_PROBE_EXIT_AT=<secs>`, off when unset): exit the
/// app after N wall seconds, so a scripted live probe (`WOW_PROBE_LUA`/`WOW_PROBE_CHAT`) is one
/// foreground command with a bounded lifetime — no external kill, no orphaned window (0437's
/// probe rounds prompted it; generic to every future live probe).
#[derive(Resource)]
struct ProbeExit {
    at: Option<f32>,
    fired: bool,
}

fn fire_probe_exit(
    mut probe: ResMut<ProbeExit>,
    time: Res<Time>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(at) = probe.at else { return };
    if !probe.fired && time.elapsed_secs() >= at {
        info!("probe-exit: {at}s elapsed — exiting");
        probe.fired = true;
        exit.write(AppExit::Success);
        // The hard backstop rides its own OS thread: the polite AppExit stops the Update
        // schedule, so an in-schedule backstop can never fire — exactly the hang it existed
        // for (a winit/net-thread teardown hang leaves a zombie client holding the account;
        // the 0451 probe reproduced it). A probe run has nothing to lose.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(5));
            warn!("probe-exit: still alive 5s after AppExit — hard exit");
            std::process::exit(0);
        });
    }
}

/// The window-resize probe (`WOW_PROBE_RESIZE="<secs>:<W>x<H>"`, logical units): resize the
/// primary window mid-run — the headless stand-in for a mac fullscreen toggle or a window drag,
/// so resize-reactive layout (the glue screens' rescale rebuild) is verifiable in one scripted
/// run: open, resize at `t`, shoot after (`WOW_LOGIN_SHOT_OUT` fires at 8 s).
pub(crate) struct ProbeResizePlugin;

impl Plugin for ProbeResizePlugin {
    fn build(&self, app: &mut App) {
        let spec = std::env::var("WOW_PROBE_RESIZE").unwrap_or_default();
        let parsed = spec.split_once(':').and_then(|(t, wh)| {
            let (w, h) = wh.split_once('x')?;
            Some((t.parse().ok()?, w.parse().ok()?, h.parse().ok()?))
        });
        match parsed {
            Some((at, w, h)) => {
                app.insert_resource(ProbeResize {
                    at,
                    size: Vec2::new(w, h),
                    fired: false,
                })
                .add_systems(Update, fire_probe_resize);
            }
            None => warn!("WOW_PROBE_RESIZE: expected \"<secs>:<W>x<H>\", got {spec:?}"),
        }
    }
}

/// [`ProbeResizePlugin`] state: the fire time, the target logical size, and the once-latch.
#[derive(Resource)]
struct ProbeResize {
    at: f32,
    size: Vec2,
    fired: bool,
}

fn fire_probe_resize(
    mut probe: ResMut<ProbeResize>,
    time: Res<Time>,
    mut windows: Query<&mut Window, With<bevy::window::PrimaryWindow>>,
) {
    if probe.fired || time.elapsed_secs() < probe.at {
        return;
    }
    probe.fired = true;
    if let Ok(mut w) = windows.single_mut() {
        w.resolution.set(probe.size.x, probe.size.y);
        info!(
            "probe-resize: window -> {}x{} logical",
            probe.size.x, probe.size.y
        );
    }
}

/// Run the probe chunk once the delay has elapsed AND the session is in-world.
fn fire_probe_lua(
    mut probe: ResMut<ProbeLua>,
    time: Res<Time>,
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    self_player: Query<(), With<crate::net::SelfPlayer>>,
) {
    if probe.fired || probe.chunk.is_empty() || time.elapsed_secs() < probe.at {
        return;
    }
    if self_player.is_empty() {
        return; // not in-world yet — keep waiting past the delay
    }
    let Some(script) = script else {
        return;
    };
    probe.fired = true;
    // `ProbeLog(text)` — the chunk's data channel OUT of the VM (greppable `probe-log:` lines);
    // until now a probe could only report through screenshots or by erroring. Installed only
    // when a probe chunk actually fires — never part of the shipping API surface.
    let install = script.lua().create_function(|_, text: String| {
        info!("probe-log: {text}");
        Ok(())
    });
    match install {
        Ok(f) => {
            if let Err(e) = script.lua().globals().set("ProbeLog", f) {
                error!("probe-lua: installing ProbeLog: {e}");
            }
        }
        Err(e) => error!("probe-lua: creating ProbeLog: {e}"),
    }
    info!("probe-lua: running {:?}", probe.chunk);
    if let Err(e) = script.run(&probe.chunk) {
        error!("probe-lua: {e}");
    }
}

/// Submit the probe lines once the delay has elapsed AND the session is in-world (the self player
/// exists) — a `.go` sent before world-enter would be dropped server-side.
///
/// Lines go in through the **chat EditBox seam**, not straight to the wire: a probe line is "what
/// the director would type", so a client-side slash command (`/duel`, `/reaction`) is parsed by
/// the same drain that serves the real chat box, while plain text and `.gm`/`.go` server commands
/// still leave as Say exactly as before. Sending them as Say instead — the original shape — meant
/// every client-side command silently went out as public chat and did nothing (decision 0637).
fn fire_probe_chat(
    mut probe: ResMut<ProbeChat>,
    time: Res<Time>,
    script: Option<NonSendMut<benilla_ui::script::UiScript>>,
    self_player: Query<(), With<crate::net::SelfPlayer>>,
) {
    if probe.lines.is_empty() {
        return;
    }
    if self_player.is_empty() {
        return; // not in-world yet — keep waiting past the delay
    }
    let Some(mut script) = script else {
        return;
    };
    // With no spacing every line goes in the first eligible frame (the original burst); with
    // `every`, line N waits until `at + N·every` — the "do X, wait, then do Y" cadence.
    loop {
        let Some(line) = probe
            .lines
            .split(';')
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .nth(probe.sent)
        else {
            return; // all sent
        };
        let due = probe.at + probe.every * probe.sent as f32;
        if time.elapsed_secs() < due {
            return;
        }
        info!("probe-chat: sending {line:?}");
        script.push_chat_input(line.to_string());
        probe.sent += 1;
    }
}

/// The LIVE FPS probe (`WOW_LIVE_FPS=<frames>`, delay via `WOW_LIVE_FPS_AT` seconds, default 25;
/// `WOW_LIVE_FPS_MOVE=1` holds W through warmup + sampling, so the probe measures RUNNING through
/// the scene — streaming, spawns, re-classification — not a parked camera; the 0366 hunt's
/// "running around SW" gap):
/// the [`super::CapturePlugin`] probe's numbers on a NORMAL connected run — streamed units, net
/// apply, quest markers, everything the server-less harness deliberately excludes. Built for the
/// 0362 residual: the serverless stormwind probe pinned 60 while the director's live session read
/// 20, so the gap IS the live world — this instrument measures it. Waits for in-world + the delay
/// (park the character first with [`ProbeChatPlugin`]), uncaps vsync, warms
/// [`PROBE_WARMUP_FRAMES`], samples, prints the same machine-greppable `FPS_PROBE` line
/// (scenario=`live`), and exits.
pub(crate) struct LiveFpsPlugin;

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
        })
        .add_systems(Update, drive_live_fps);
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
}

/// Wait for in-world + the delay, uncap, warm, sample, print, exit — the live twin of the
/// harness probe's `Phase::ProbeWarmup`/`Probing` arms.
#[allow(clippy::too_many_arguments)]
fn drive_live_fps(
    mut probe: ResMut<LiveFps>,
    time: Res<Time<bevy::time::Real>>,
    self_player: Query<(), With<crate::net::SelfPlayer>>,
    mut windows: Query<&mut Window, With<bevy::window::PrimaryWindow>>,
    particles: Query<&crate::particles::ParticleEmitter>,
    parts: Query<&ViewVisibility, With<crate::debug_panel::ModelPart>>,
    streamed: Query<(), With<crate::net::NetEntity>>,
    // The animation-LOD gate's effect, machine-readable per probe (decision 0448): how many
    // streamed rigs sat parked at sample end.
    parked: Query<(), With<crate::creature_anim::AnimParked>>,
    entities: Query<()>,
    // Where the sample was actually taken (0705's prove-the-run law): a probe number is evidence
    // only once the body is known to be at the pin, and `WOW_PROBE_CHAT`'s `.go` can silently
    // fail (a bad map id, a refused command) leaving the run measuring the login spot.
    map: Option<Res<crate::world_map::CurrentMap>>,
    body: Option<Res<crate::player::Player>>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut exit: MessageWriter<AppExit>,
) {
    match probe.phase {
        LiveFpsPhase::Done => {}
        LiveFpsPhase::Waiting => {
            if time.elapsed_secs() < probe.at || self_player.is_empty() {
                return;
            }
            if let Ok(mut w) = windows.single_mut() {
                w.present_mode = super::probe_uncap_mode();
            }
            info!(
                "live-fps: in-world + settled; vsync off, warming {PROBE_WARMUP_FRAMES} frames{}",
                if probe.run { ", holding W" } else { "" }
            );
            if probe.run {
                // A synthetic held key: `ButtonInput` persists a press until its release, and the
                // winit feed only releases keys it saw go down, so this holds across frames.
                keys.press(KeyCode::KeyW);
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
            let (emitters, active, live) = particles
                .iter()
                .fold((0usize, 0usize, 0usize), |(e, a, l), p| {
                    (e + 1, a + usize::from(p.live() > 0), l + p.live())
                });
            let (submeshes, drawn) = parts.iter().fold((0usize, 0usize), |(n, d), v| {
                (n + 1, d + usize::from(v.get()))
            });
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
            // The pin the number belongs to, in the `.go xyz` order, so a probe line can be
            // matched against the report's coordinates without a second instrument.
            let at_pin = match (map.as_ref(), body.as_ref().filter(|b| b.active)) {
                (Some(m), Some(b)) => {
                    let [x, y, z] = benilla_assets::coords::bevy_to_wow(b.pos);
                    format!(" map={} pos={x:.1},{y:.1},{z:.1}", m.0)
                }
                _ => String::new(),
            };
            println!(
                "FPS_PROBE scenario=live frames={} mean_ms={mean:.2} p50_ms={:.2} p95_ms={:.2} p99_ms={:.2} max_ms={:.2} fps={:.1} emitters={emitters} active={active} particles={live} submeshes={submeshes} drawn={drawn} streamed={} parked={} entities={} px={}x{}{cpu}{present}{at_pin}",
                v.len(),
                at(0.50),
                at(0.95),
                at(0.99),
                v[v.len() - 1],
                1000.0 / mean,
                streamed.iter().len(),
                parked.iter().len(),
                entities.iter().len(),
                px.0,
                px.1,
            );
            if probe.run {
                keys.release(KeyCode::KeyW);
            }
            probe.phase = LiveFpsPhase::Done;
            exit.write(AppExit::Success);
        }
    }
}

/// The particle census (`WOW_PARTICLE_CENSUS=<secs>`): once, `t` seconds in, print one line per
/// live emitter (blend, file flags, sampled rate keys, texture, live count) plus a machine-
/// readable total — the like-for-like number to put beside a reference-trace quad count (the
/// login whirlpool investigation: the real client draws 793 particle quads across 23 draws in
/// one `UI_MainMenu` frame). Works at any state — the glue screens included, unlike the
/// in-world-gated FPS probe.
///
/// It also measures **draw distance** (decision 0678): each emitter's planar depth along
/// camera-forward — the coordinate the far-clip wall uses — and the draw-set gate's verdict, with
/// `drawn_beyond_wall` on the summary line. That is the numeric form of "effects render at
/// unlimited distance" (bug B39): emitters still ticking and drawing past the wall that has already
/// discarded the terrain beneath them. **It must read 0**; a non-zero value is the bug, live.
pub(crate) struct ParticleCensusPlugin;

impl Plugin for ParticleCensusPlugin {
    fn build(&self, app: &mut App) {
        let at = std::env::var("WOW_PARTICLE_CENSUS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10.0);
        app.insert_resource(ParticleCensus { at, fired: false })
            .add_systems(Update, fire_particle_census);
    }
}

/// [`ParticleCensusPlugin`] state: the fire time and the once-latch.
#[derive(Resource)]
struct ParticleCensus {
    at: f32,
    fired: bool,
}

fn fire_particle_census(
    mut probe: ResMut<ParticleCensus>,
    time: Res<Time>,
    view: Res<crate::view::ViewDistance>,
    cam: Query<&GlobalTransform, With<crate::player::WorldCamera>>,
    emitters: Query<(
        &crate::particles::ParticleEmitter,
        Option<&crate::particles::EmitterFade>,
        &Visibility,
        Option<&bevy::camera::visibility::RenderLayers>,
    )>,
) {
    if probe.fired || time.elapsed_secs() < probe.at {
        return;
    }
    probe.fired = true;
    let mut total = 0usize;
    let mut n = 0usize;
    // The B39 columns (decision 0678). Per emitter: its planar depth along camera-forward (the
    // coordinate the far-clip wall is measured in) and the draw-set gate's live verdict.
    //
    // **`drawn_beyond_wall` is the number that names the bug.** It counts emitters the gate is
    // still ticking and drawing at a depth where the detailed world — the terrain under them
    // included — has already been discarded by the wall. Before 0678 it was routinely non-zero,
    // because `doodad_fade_alpha` admits any owner over `NEVER_FADE_RADIUS` at *every* distance
    // and nothing else bounded depth; that is precisely "all effects render at unlimited
    // distance", and precisely the reporter's "the terrain is not even rendered that far".
    // It must now be **0**: past the wall the gate hides the emitter and freezes its pool.
    //
    // `beyond_wall` (verdict ignored) stays as the denominator — emitters *exist* out there and
    // should, they are simply frozen. A fix that despawned them would be the wrong fix.
    //
    // **Booth-layered emitters are excluded from the distance accounting** — the same layer filter
    // `simulate_particles` uses to pick a booth's camera. The portrait/glue scenes are parked
    // thousands of yards from the world and drawn by their OWN camera, so the world camera's wall
    // says nothing about them; counting them read as 28 phantom "effects past the wall" (all of
    // them Karazahn braziers and night-elf glows at ~7080 yd) on a build where the world was
    // already clean. Measuring the right subject is the instrument's job, not the reader's.
    let cam_tf = cam.iter().next();
    let mut beyond_wall = 0usize;
    let mut drawn_beyond_wall = 0usize;
    let mut drawn_beyond_wall_live = 0usize;
    let mut booth = 0usize;
    let mut max_drawn_depth = f32::NEG_INFINITY;
    for (e, fade, vis, layers) in &emitters {
        let world_layer =
            layers.is_none_or(|l| l.intersects(&bevy::camera::visibility::RenderLayers::default()));
        // Depth to the OWNER's fade sphere where there is one (the gate's own subject), else the
        // emitter's live anchor — so the number always names what the gate actually tests.
        let depth = cam_tf.map(|t| {
            let center = fade.map_or_else(|| e.anchor_world(), |f| f.center);
            let radius = fade.map_or(0.0, |f| f.radius);
            (center - t.translation()).dot(Vec3::from(t.forward())) - radius
        });
        let drawn = *vis != Visibility::Hidden;
        if !world_layer {
            booth += 1;
        }
        if let Some(d) = depth.filter(|_| world_layer) {
            if drawn {
                max_drawn_depth = max_drawn_depth.max(d);
            }
            if d > view.farclip {
                beyond_wall += 1;
                if drawn {
                    drawn_beyond_wall += 1;
                    drawn_beyond_wall_live += e.live();
                }
            }
        }
        let dist = depth
            .map(|d| {
                let lane = if world_layer { "world" } else { "booth" };
                let c = fade.map_or_else(|| e.anchor_world(), |f| f.center);
                format!(
                    " depth={d:.1} drawn={drawn} lane={lane} gated={} at=({:.0},{:.0},{:.0})",
                    fade.is_some(),
                    c.x,
                    c.y,
                    c.z
                )
            })
            .unwrap_or_default();
        let d = e.def();
        // The rate summary: the constant (the common shape), else each slot's key count — the
        // full per-sequence choreography lives in `benilla-extract m2anim`, not a census line.
        let rate_keys: Vec<String> = match d.timing.constant_rate() {
            Some(r) => vec![format!("{r:.1}")],
            None => d
                .timing
                .slot_views()
                .iter()
                .enumerate()
                .map(|(s, (_, r, _))| format!("s{s}:{}k", r.map_or(0, <[(f32, f32)]>::len)))
                .collect(),
        };
        // The orientation fingerprint (world plane normal + thickness/radius) is the numeric
        // "which way does this cloud face" — the flat-vs-standing question a screenshot can
        // only suggest (the InstancePortal swirl-plane investigation).
        let plane = e
            .cloud_fingerprint()
            .map(|(c, nrm, thick, radius)| {
                format!(
                    " ctr=({:.1},{:.1},{:.1}) normal=({:+.2},{:+.2},{:+.2}) thick={thick:.2} radius={radius:.2}",
                    c.x, c.y, c.z, nrm.x, nrm.y, nrm.z
                )
            })
            .unwrap_or_default();
        println!(
            "PARTICLE_CENSUS_EMITTER blend={:?} flags={:#06x} rate=[{}] life={:.2} tex={} live={}{dist}{plane}",
            d.blend,
            d.flags,
            rate_keys.join(","),
            d.lifespan,
            d.texture.as_deref().unwrap_or("-"),
            e.live(),
        );
        total += e.live();
        n += 1;
    }
    let max_drawn_depth = if max_drawn_depth.is_finite() {
        max_drawn_depth
    } else {
        0.0
    };
    // The camera pose goes on the line so a census is self-describing: every distance number here
    // is measured from it, and a probe whose `.go` silently failed otherwise reports crisp numbers
    // about the wrong place.
    let where_ = cam_tf
        .map(|t| {
            let p = t.translation();
            format!(" cam=({:.1},{:.1},{:.1})", p.x, p.y, p.z)
        })
        .unwrap_or_else(|| " cam=none".into());
    println!(
        "PARTICLE_CENSUS emitters={n} booth={booth} live_total={total} farclip={:.0} \
         beyond_wall={beyond_wall} drawn_beyond_wall={drawn_beyond_wall} \
         drawn_beyond_wall_live={drawn_beyond_wall_live} max_drawn_depth={max_drawn_depth:.1}{where_}",
        view.farclip,
    );
}

/// The bevy_ui node census (`WOW_NODE_PROBE=<secs>`): once, `t` seconds in, print one line per
/// live `ComputedNode` entity — resolved rect (logical px, y-down), visibility, and the entity's
/// full component list — the "who owns this rectangle" instrument for UI drawn OUTSIDE the
/// FrameXML quad pass (the glue widgets, loading screen, overlays), which `WOW_UI_PROBE`'s quad
/// dump can't see. Born hunting a phantom gold-bordered box over the mail window's send tab.
pub(crate) struct NodeProbePlugin;

impl Plugin for NodeProbePlugin {
    fn build(&self, app: &mut App) {
        let at = std::env::var("WOW_NODE_PROBE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10.0);
        app.insert_resource(NodeProbe { at, fired: false })
            .add_systems(Update, fire_node_probe);
    }
}

/// [`NodeProbePlugin`] state: the fire time and the once-latch.
#[derive(Resource)]
struct NodeProbe {
    at: f32,
    fired: bool,
}

fn fire_node_probe(world: &mut World) {
    {
        let time = world.resource::<Time>().elapsed_secs();
        let probe = world.resource::<NodeProbe>();
        if probe.fired || time < probe.at {
            return;
        }
    }
    world.resource_mut::<NodeProbe>().fired = true;
    let scale = world
        .query::<&bevy::window::Window>()
        .iter(world)
        .next()
        .map_or(1.0, bevy::window::Window::scale_factor);
    let mut q = world.query::<(
        Entity,
        &bevy::ui::ComputedNode,
        &GlobalTransform,
        Option<&InheritedVisibility>,
    )>();
    let rows: Vec<(Entity, Vec2, Vec3, bool)> = q
        .iter(world)
        .map(|(e, node, gt, vis)| {
            (
                e,
                node.size(),
                gt.translation(),
                vis.is_none_or(|v| v.get()),
            )
        })
        .collect();
    info!("node probe: {} nodes, scale {scale}", rows.len());
    for (e, size, center, vis) in rows {
        let comps: Vec<String> = world.inspect_entity(e).map_or_else(
            |_| Vec::new(),
            |it| {
                it.map(|c| c.name().shortname().to_string())
                    .filter(|n| {
                        // Drop the ubiquitous plumbing components — the signal is the rest.
                        !matches!(
                            n.as_str(),
                            "Transform"
                                | "GlobalTransform"
                                | "Visibility"
                                | "InheritedVisibility"
                                | "ViewVisibility"
                                | "ChildOf"
                                | "Children"
                        )
                    })
                    .collect()
            },
        );
        // ComputedNode is physical px; translation is the node's center, also physical.
        info!(
            "node probe: [{:.0},{:.0} {:.0}x{:.0}] vis={} {:?}",
            (center.x - size.x * 0.5) / scale,
            (center.y - size.y * 0.5) / scale,
            size.x / scale,
            size.y / scale,
            vis,
            comps
        );
    }
}

/// The entity census (`WOW_ENTITY_CENSUS=<secs>`, REAL seconds): once, `t` seconds in, print one
/// line per live archetype — entity count plus its signal components, largest first — and a machine-readable
/// summary. The "what IS the entity count made of" instrument: the standing HUD reads tens of
/// thousands of entities, and every per-frame cost that scales with *residency* (0362's
/// change-tick sweeps, transform propagation, render extraction) is only attributable once
/// residency itself has names. Born with the cost-ledger campaign.
pub(crate) struct EntityCensusPlugin;

impl Plugin for EntityCensusPlugin {
    fn build(&self, app: &mut App) {
        let at = std::env::var("WOW_ENTITY_CENSUS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10.0);
        app.insert_resource(EntityCensus { at, fired: false })
            .add_systems(Update, fire_entity_census);
    }
}

/// [`EntityCensusPlugin`] state: the fire time and the once-latch.
#[derive(Resource)]
struct EntityCensus {
    at: f32,
    fired: bool,
}

/// Archetype lines the census prints; everything smaller folds into the summary's `other_n`.
const ENTITY_CENSUS_ROWS: usize = 60;

/// Signal components shown per archetype line — enough to name what the entities are without
/// drowning the line in a 30-component render archetype.
const ENTITY_CENSUS_COMPS: usize = 14;

fn fire_entity_census(world: &mut World) {
    {
        // REAL seconds, not virtual: the census is timed to compose with `WOW_LIVE_FPS_AT`
        // (also real), and virtual time lags real by the load stalls — a virtual-timed one-shot
        // scheduled "just before sampling" fires after the probe has already exited.
        let time = world.resource::<Time<bevy::time::Real>>().elapsed_secs();
        let probe = world.resource::<EntityCensus>();
        if probe.fired || time < probe.at {
            return;
        }
    }
    world.resource_mut::<EntityCensus>().fired = true;
    let components = world.components();
    let mut rows: Vec<(usize, String)> = world
        .archetypes()
        .iter()
        .filter(|a| !a.is_empty())
        .map(|a| {
            let full: Vec<String> = a
                .components()
                .iter()
                .filter_map(|id| components.get_info(*id))
                .map(|c| c.name().shortname().to_string())
                .collect();
            let signal: Vec<String> = full
                .iter()
                .filter(|n| {
                    // Drop the ubiquitous plumbing components — the signal is the rest.
                    !matches!(
                        n.as_str(),
                        "Transform"
                            | "GlobalTransform"
                            | "Visibility"
                            | "InheritedVisibility"
                            | "ViewVisibility"
                            | "ChildOf"
                            | "Children"
                    )
                })
                .cloned()
                .collect();
            // A bare transform node has no signal left after the filter — and two such
            // archetypes differing only in plumbing (Children vs not) would print as identical
            // rows. For those, the plumbing IS the signal: print the full list.
            let names = if signal.len() <= 1 { full } else { signal };
            let shown = names.len().min(ENTITY_CENSUS_COMPS);
            let more = names.len() - shown;
            let mut comps = names[..shown].join(", ");
            if more > 0 {
                comps.push_str(&format!(" +{more}"));
            }
            (a.len() as usize, comps)
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    let (total_arch, total_n) = (rows.len(), rows.iter().map(|r| r.0).sum::<usize>());
    let other_n = rows
        .iter()
        .skip(ENTITY_CENSUS_ROWS)
        .map(|r| r.0)
        .sum::<usize>();
    for (n, comps) in rows.iter().take(ENTITY_CENSUS_ROWS) {
        println!("ENTITY_CENSUS_ARCH n={n} comps=[{comps}]");
    }
    println!(
        "ENTITY_CENSUS total={total_n} archetypes={total_arch} \
         rows={} other_n={other_n}",
        rows.len().min(ENTITY_CENSUS_ROWS),
    );
}
