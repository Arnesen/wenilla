//! The LIVE-run probe instruments — every plugin here rides a NORMAL connected session (unlike
//! the parent's server-less [`super::CapturePlugin`] harness): one screenshot of the live scene
//! ([`LiveShotPlugin`]), scripted chat sends ([`ProbeChatPlugin`]), synthetic key taps
//! ([`ProbeKeyPlugin`]), a Lua chunk in the live UI VM ([`ProbeLuaPlugin`]), the bounded-lifetime
//! self-exit ([`ProbeExitPlugin`]), and the live frame-time sampler ([`LiveFpsPlugin`]). Each is
//! env-gated and registered by `main`; compose them for unattended "park, act, observe" probes.

use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};

use super::PROBE_WARMUP_FRAMES;

/// The LIVE probe shot (`WOW_LIVE_SHOT=<png>`, delay via `WOW_LIVE_SHOT_AT` seconds, default 12):
/// one screenshot of the primary window on a NORMAL connected run — the "what does the live scene
/// actually look like" instrument (server GameObjects, event spawns, NPCs — everything the
/// server-less harness deliberately excludes). The app keeps running after the save; pair with
/// `WOW_USER`/`WOW_CHAR` and an outer `timeout` for unattended probes. Unlike
/// [`super::CapturePlugin`], nothing is pinned — the shot shows the scene as a player would see it.
///
/// **`WOW_LIVE_SHOT_COUNT=<n>` makes it a burst** — `n` shots written as `<stem>-000.png`,
/// `<stem>-001.png`, … , spaced by `WOW_LIVE_SHOT_EVERY` seconds (**default 0 = one per frame**,
/// i.e. *adjacent* frames). One shot still writes the bare path, so every existing invocation is
/// unchanged.
///
/// The burst exists because **a flicker is a temporal artefact and a single frame cannot show one**
/// (decision 0653). "This object's textures flicker" was un-diagnosable with the instruments we had:
/// the reporter's stills prove nothing, and grading a live run by eye is exactly the loop rule 7
/// forbids. Adjacent frames from a *parked* camera ([`crate::player`]'s `WOW_PROBE_CAM`) turn it
/// into arithmetic: `benilla-visual flicker <dir>` collapses the burst to the envelope of what
/// would not hold still, and the lit pixels are the defect.
pub(crate) struct LiveShotPlugin;

impl Plugin for LiveShotPlugin {
    fn build(&self, app: &mut App) {
        let out = std::env::var("WOW_LIVE_SHOT").unwrap_or_default();
        let at = std::env::var("WOW_LIVE_SHOT_AT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(12.0);
        let count = std::env::var("WOW_LIVE_SHOT_COUNT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1u32)
            .max(1);
        let every = std::env::var("WOW_LIVE_SHOT_EVERY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);
        app.insert_resource(LiveShot {
            out,
            count,
            every,
            taken: 0,
            next_at: at,
        })
        .add_systems(Update, fire_live_shot);
    }
}

/// [`LiveShotPlugin`] state: the output path, how many shots the burst wants, their spacing
/// (`0` = one per frame), how many have gone out, and when the next one may — seeded to
/// `WOW_LIVE_SHOT_AT`, so the first-fire delay and the burst spacing are the same clock.
#[derive(Resource)]
struct LiveShot {
    out: String,
    count: u32,
    every: f32,
    taken: u32,
    next_at: f32,
}

/// `stem-007.png` for shot 7 of a burst — the bare path when the burst is one shot, so the
/// single-shot filename (which existing probes and docs name literally) never moves.
fn burst_path(out: &str, index: u32, count: u32) -> String {
    if count <= 1 {
        return out.to_string();
    }
    match out.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem}-{index:03}.{ext}"),
        None => format!("{out}-{index:03}"),
    }
}

/// Fire the live screenshot(s) once the startup delay has elapsed — one per frame by default, so a
/// burst samples *adjacent* frames and a per-frame instability has nowhere to hide.
fn fire_live_shot(mut shot: ResMut<LiveShot>, time: Res<Time>, mut commands: Commands) {
    if shot.taken >= shot.count || time.elapsed_secs() < shot.next_at {
        return;
    }
    let path = burst_path(&shot.out, shot.taken, shot.count);
    commands
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(path.clone()));
    shot.taken += 1;
    shot.next_at = time.elapsed_secs() + shot.every;
    if shot.count > 1 {
        info!("live-shot: writing {path} ({}/{})", shot.taken, shot.count);
    } else {
        info!("live-shot: writing {path}");
    }
}

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
                w.present_mode = bevy::window::PresentMode::AutoNoVsync;
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
            println!(
                "FPS_PROBE scenario=live frames={} mean_ms={mean:.2} p50_ms={:.2} p95_ms={:.2} p99_ms={:.2} max_ms={:.2} fps={:.1} emitters={emitters} active={active} particles={live} submeshes={submeshes} drawn={drawn} streamed={} parked={} entities={} px={}x{}",
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
    emitters: Query<&crate::particles::ParticleEmitter>,
) {
    if probe.fired || time.elapsed_secs() < probe.at {
        return;
    }
    probe.fired = true;
    let mut total = 0usize;
    let mut n = 0usize;
    for e in &emitters {
        let d = e.def();
        let rate_keys: Vec<String> = d
            .emission_rate
            .keys
            .iter()
            .map(|(t, v)| format!("{t}:{v:.1}"))
            .collect();
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
            "PARTICLE_CENSUS_EMITTER blend={:?} flags={:#06x} rate=[{}] life={:.2} tex={} live={}{plane}",
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
    println!("PARTICLE_CENSUS emitters={n} live_total={total}");
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
