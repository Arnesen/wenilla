//! The LIVE-run probe instruments — every plugin here rides a NORMAL connected session (unlike
//! the parent's server-less [`super::CapturePlugin`] harness): scripted chat sends
//! ([`ProbeChatPlugin`]), synthetic key taps ([`ProbeKeyPlugin`]), a Lua chunk in the live UI VM
//! ([`ProbeLuaPlugin`]), the bounded-lifetime self-exit ([`ProbeExitPlugin`]), and the live
//! frame-time sampler ([`LiveFpsPlugin`]). The live screenshot and its validity gates live in the
//! sibling [`super::live_shot`]. Each is env-gated and registered by `main`; compose them for
//! unattended "park, act, observe" probes.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::PROBE_WARMUP_FRAMES;

/// The clock **every probe schedule reads** — real time, never the virtual clock (decision 0789).
///
/// A probe knob is a wall-clock instruction: "send this at 20 s", "sample 300 frames from 25 s",
/// "resize at 12 s", "exit at 480 s". `Time<Virtual>` cannot honour one — it clamps every frame delta
/// to `max_delta` (250 ms, `bevy_time`'s default), so on any leg that hitches (a streaming burst, an
/// occluded window, a loaded machine) it falls behind real time and drags the whole schedule with it.
/// The knob then means something the operator never asked for, *silently*.
///
/// **Third time this clock has cost us a measurement.** 0615 moved the relayed-move replay off it
/// (and cites the UI script clock as the same lesson before that); then B131's first causal leg was
/// destroyed by it here — the probe-chat hops drifted 40 s → 75 s apart, so windows labelled
/// "parked, ticks off" in fact contained a teleport and live ticks, and an eight-minute leg had to be
/// thrown away (decision 0785's discarded run). A named alias is what makes the next probe get it
/// right without knowing the story: type this, and the mistake is unavailable.
///
/// The one deliberate virtual clock in the harness is the fixture **age** in
/// [`super::drive_capture`] — an age on the clock the effect animates on (and which the capture
/// freezes at save time), which is not a schedule at all.
pub(crate) type ProbeClock<'w> = Res<'w, Time<bevy::time::Real>>;

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

/// Keep a probe run's window **un-occludable** — the one defence against macOS's ~1 fps
/// throttle for a fully covered window (decisions 0713/0777, method.md's `caffeinate` note).
///
/// It used to live inside the FPS probe alone, which reads as "a frame-rate concern". It is not:
/// **every** scripted probe schedule is wall-clock ([`ProbeClock`]), so a throttled run doesn't
/// just measure slowly, it *executes the wrong script* — this session's mounted-jump run fired
/// `W@16` and `Space@19` in the SAME frame at ~1 fps, i.e. it jumped from a standstill instead of
/// mid-run, and the leg had to be re-read to notice (decision 0906). Any probe env arms it, so a
/// key/chat/Lua probe defends itself exactly like the FPS one. Write-gated: re-marking `Window`
/// every frame would re-apply its whole state through winit.
pub(crate) struct ProbeFocusPlugin;

impl Plugin for ProbeFocusPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, keep_probe_window_on_top);
    }
}

fn keep_probe_window_on_top(mut windows: Query<&mut Window, With<bevy::window::PrimaryWindow>>) {
    if let Ok(mut w) = windows.single_mut() {
        if w.window_level != bevy::window::WindowLevel::AlwaysOnTop {
            w.window_level = bevy::window::WindowLevel::AlwaysOnTop;
        }
    }
}

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
        // The detached free-fly toggle (`player.rs`) and its Ctrl speed boost (`camera.rs`, ×5).
        // Added because the harness could not reach the leg that broke decision 0793: the director's
        // first real run was a boosted free-fly, whose camera crosses the art radius in ~5 s, and
        // reproducing it needed a held `Ctrl` + `W` behind an `F`.
        //
        // The toggle is the dev chord + `F` since decision 1043 (it was a bare `F`), so a probe that
        // wants free-fly holds `Ctrl` and `Shift` across the `F` tap — hence `Shift` here. `Ctrl`
        // stays held afterwards for the ×5 boost, which is unchanged.
        "F" => KeyCode::KeyF,
        "Ctrl" => KeyCode::ControlLeft,
        "Shift" => KeyCode::ShiftLeft,
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
///
/// Both input currencies, deliberately (0997): `ButtonInput` for every held-state reader (and
/// the binding dispatch's stuck-latch sweep, which treats "latched but not pressed" as a missed
/// release), plus the raw [`KeyboardInput`] message the binding dispatch's press/release edges
/// actually consume — a state-only synthetic press was invisible to the chord latcher, which
/// would have silently killed this instrument the day the dispatch landed.
fn fire_probe_key(
    mut probe: ResMut<ProbeKeys>,
    time: ProbeClock,
    self_player: Query<(), With<crate::net::SelfPlayer>>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut events: MessageWriter<bevy::input::keyboard::KeyboardInput>,
) {
    if probe.taps.is_empty() || self_player.is_empty() {
        return;
    }
    let now = time.elapsed_secs();
    let mut synth = |key: KeyCode, state: bevy::input::ButtonState| {
        events.write(bevy::input::keyboard::KeyboardInput {
            key_code: key,
            logical_key: bevy::input::keyboard::Key::Unidentified(
                bevy::input::keyboard::NativeKey::Unidentified,
            ),
            state,
            text: None,
            repeat: false,
            window: Entity::PLACEHOLDER,
        });
    };
    for tap in &mut probe.taps {
        if !tap.pressed && now >= tap.at {
            info!(
                "probe-key: {:?} down ({now:.1}s, hold {:.2}s)",
                tap.key, tap.hold
            );
            keys.press(tap.key);
            synth(tap.key, bevy::input::ButtonState::Pressed);
            tap.pressed = true;
        } else if tap.pressed && !tap.released && now >= tap.at + tap.hold {
            keys.release(tap.key);
            synth(tap.key, bevy::input::ButtonState::Released);
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
    time: ProbeClock,
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
    time: ProbeClock,
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
    time: ProbeClock,
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
    time: ProbeClock,
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
            occluded_now: false,
            occluded_frames: 0,
        })
        .init_resource::<ChurnCensus>()
        .add_systems(Update, drive_live_fps)
        .add_systems(
            First,
            (
                (
                    churn_counter::<bevy::image::Image>("image"),
                    churn_counter::<Mesh>("mesh"),
                    churn_counter::<StandardMaterial>("std"),
                    churn_counter::<crate::terrain::TerrainMaterial>("terrain"),
                    churn_counter::<crate::terrain::WowModelMaterial>("model"),
                    churn_counter::<crate::terrain::WdlMaterial>("wdl"),
                    churn_counter::<crate::terrain::LiquidMaterial>("liquid"),
                ),
                (
                    churn_counter::<crate::sky::SkyMaterial>("sky"),
                    churn_counter::<crate::sun::CelestialMaterial>("celestial"),
                    churn_counter::<crate::sun::StarMaterial>("star"),
                    churn_counter::<crate::clouds::CloudMaterial>("cloud"),
                    churn_counter::<crate::ui_pass::UiQuadMaterial>("uiquad"),
                ),
            ),
        );
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

/// Asset residency — the leak meter (the #bugs teleport leak: caches hold strong handles, so maps
/// visited earlier keep their materials/meshes/images resident, and every uv/tint registry survivor
/// is re-uploaded per frame). A tour probe reading the same counts as a fresh control is what
/// "torn down" means, machine-checked. One struct because `drive_live_fps` is at Bevy's
/// system-param arity limit.
#[derive(bevy::ecs::system::SystemParam)]
struct ResidencyMeter<'w> {
    mats: Res<'w, Assets<crate::terrain::WowModelMaterial>>,
    meshes: Res<'w, Assets<Mesh>>,
    images: Res<'w, Assets<bevy::image::Image>>,
    uv_reg: Res<'w, crate::doodad_anim::UvAnimMaterials>,
    tint_reg: Res<'w, crate::doodad_anim::TintAnimMaterials>,
    models: Res<'w, Assets<benilla_assets::M2Model>>,
    server: Res<'w, AssetServer>,
    churn: ResMut<'w, ChurnCensus>,
}

/// The asset-churn census (the probe's ratchet meter): `AssetEvent::Modified` counts per asset
/// type across the sample window, printed as one `MAT_CHURN` line beside `FPS_PROBE`. A modified
/// material re-creates its uniform buffers + bind group that frame (the Metal non-bindless path),
/// and a modified image/mesh re-uploads — the teleport leak's CPU engine was exactly a per-frame
/// ratchet of this shape, so the floor hunt names them instead of guessing suspects one at a
/// time. Counters register only under `WOW_LIVE_FPS` ([`LiveFpsPlugin`]) — a normal run carries
/// none of this.
#[derive(Resource, Default)]
struct ChurnCensus(std::collections::BTreeMap<&'static str, usize>);

/// One census counter for asset type `A`, folding this frame's `Modified` events in under
/// `label` (a short stable name — the `type_name` of an `ExtendedMaterial` alias is unreadable).
fn churn_counter<A: bevy::asset::Asset>(
    label: &'static str,
) -> impl FnMut(MessageReader<bevy::asset::AssetEvent<A>>, ResMut<ChurnCensus>) {
    move |mut reader, mut census| {
        let n = reader
            .read()
            .filter(|e| matches!(e, bevy::asset::AssetEvent::Modified { .. }))
            .count();
        if n > 0 {
            *census.0.entry(label).or_default() += n;
        }
    }
}

impl ResidencyMeter<'_> {
    /// `WOW_ASSET_DUMP=1`: one `ASSET_DUMP` line per resident image/mesh at sample time, path
    /// via the asset server (runtime-built assets have no path and print as `<unpathed>` counts).
    /// Diffing a tour probe's dump against a fresh control's names exactly which files a
    /// teardown left behind — the leak meter's magnifying glass.
    fn dump(&self) {
        let mut lines: Vec<String> = Vec::new();
        let mut unpathed = [0usize; 3];
        for (kind, ids) in [
            (
                "image",
                self.images.ids().map(|i| i.untyped()).collect::<Vec<_>>(),
            ),
            (
                "mesh",
                self.meshes.ids().map(|i| i.untyped()).collect::<Vec<_>>(),
            ),
            (
                "model",
                self.models.ids().map(|i| i.untyped()).collect::<Vec<_>>(),
            ),
        ] {
            let slot = match kind {
                "image" => 0,
                "mesh" => 1,
                _ => 2,
            };
            for id in ids {
                match self.server.get_path(id) {
                    Some(p) => lines.push(format!("ASSET_DUMP {kind} {p}")),
                    None => unpathed[slot] += 1,
                }
            }
        }
        lines.sort();
        for l in &lines {
            println!("{l}");
        }
        println!(
            "ASSET_DUMP <unpathed> images={} meshes={} models={} materials={}",
            unpathed[0],
            unpathed[1],
            unpathed[2],
            self.mats.len()
        );
    }
}

/// Where — and in what scene state — the sample was taken. Bundled because [`drive_live_fps`] sits
/// at Bevy's 16-param ceiling, and because these four are read together or not at all.
///
/// The `room`/`windows` half is the exterior-scene gate's two terms (decision 0774), the same pair
/// the debug panel's World section shows. Without them a `drawn=` reading taken indoors cannot be
/// read at all: a big number means either "we claimed a room and the cull let everything through" or
/// "we never claimed a room, so nothing was gated" — opposite bugs with identical numbers, and the
/// difference cost a measurement (0780).
#[derive(SystemParam)]
struct SamplePin<'w> {
    /// The map the sample landed on (0705's prove-the-run law): a probe number is evidence only once
    /// the body is known to be at the pin, and `WOW_PROBE_CHAT`'s `.go` can silently fail (a bad map
    /// id, a refused command) leaving the run measuring the login spot.
    map: Option<Res<'w, crate::world_map::CurrentMap>>,
    body: Option<Res<'w, crate::player::Player>>,
    room: Option<Res<'w, crate::wmo_portal::CameraInteriorClaim>>,
    windows: Option<Res<'w, crate::wmo_portal::ExteriorWindows>>,
    /// What the cull DID, not just what it was told — see [`crate::exterior_cull::ExteriorCullVerdict`].
    verdict: Option<Res<'w, crate::exterior_cull::ExteriorCullVerdict>>,
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
    // Every spawned model submesh, with the two facts that make a visible one accountable: which
    // subsystem it belongs to, and whether it is gated by the exterior-scene cull. See
    // [`VisCensus`] for why "visible" alone was not enough to diagnose anything.
    parts: Query<CensusData>,
    streamed: Query<(), With<crate::net::NetEntity>>,
    // The animation-LOD gate's effect, machine-readable per probe (decision 0448): how many
    // streamed rigs sat parked at sample end.
    parked: Query<(), With<crate::creature_anim::AnimParked>>,
    entities: Query<()>,
    pin: SamplePin,
    // The owned skin-palette occupancy (decision 0720) — `rigs=live/peak bones=live/peak` on the
    // probe line proves the palette lane is actually populated (an all-zero table renders
    // origin-collapsed rigs, which no other probe number would catch).
    palettes: Option<Res<crate::rig_palette::RigPalettes>>,
    mut residency: ResidencyMeter,
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut key_events: MessageWriter<bevy::input::keyboard::KeyboardInput>,
    mut exit: MessageWriter<AppExit>,
    mut occlusions: MessageReader<bevy::window::WindowOccluded>,
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
                // The churn census restarts with the window — warmup noise (streaming, shader
                // warms) would otherwise read as steady-state ratchets.
                residency.churn.0.clear();
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
            let (emitters, active, live) = particles
                .iter()
                .fold((0usize, 0usize, 0usize), |(e, a, l), p| {
                    (e + 1, a + usize::from(p.live() > 0), l + p.live())
                });
            let mut census = VisCensus {
                own_instance: pin.room.as_ref().and_then(|r| r.0).map(|c| c.room.instance),
                ..Default::default()
            };
            let (submeshes, drawn) = parts.iter().fold((0usize, 0usize), |(n, d), row| {
                census.add(row);
                (n + 1, d + usize::from(row.0.get()))
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
            // matched against the report's coordinates without a second instrument — followed by
            // the exterior-scene gate's state, which is what makes `drawn=` legible indoors.
            let at_pin = match (pin.map.as_ref(), pin.body.as_ref().filter(|b| b.active)) {
                (Some(m), Some(b)) => {
                    let [x, y, z] = benilla_assets::coords::bevy_to_wow(b.pos);
                    format!(" map={} pos={x:.1},{y:.1},{z:.1}", m.0)
                }
                _ => String::new(),
            };
            let gate = {
                let room = match pin.room.as_ref().and_then(|r| r.0) {
                    Some(claim) => format!("g{:02}", claim.room.group),
                    None => "none".to_string(),
                };
                match pin.windows.as_deref() {
                    Some(crate::wmo_portal::ExteriorWindows::Windows(rects)) => {
                        format!(" room={room} windows={}", rects.len())
                    }
                    Some(crate::wmo_portal::ExteriorWindows::Unrestricted) => {
                        format!(" room={room} windows=unrestricted")
                    }
                    None => String::new(),
                }
            };
            let culled = match pin.verdict.as_deref() {
                Some(v) => format!(
                    " cull_windows={} cull_frusta={} cull_tested={} cull_hidden={} cull_unbounded={}",
                    v.windows
                        .map_or("unrestricted".to_string(), |n| n.to_string()),
                    v.frusta,
                    v.tested,
                    v.hidden,
                    v.unbounded
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
                residency.mats.len(),
                residency.meshes.len(),
                residency.images.len(),
                residency.uv_reg.0.len(),
                residency.tint_reg.0.len(),
            );
            println!(
                "FPS_PROBE scenario=live frames={} mean_ms={mean:.2} p50_ms={:.2} p95_ms={:.2} p99_ms={:.2} max_ms={:.2} fps={:.1} emitters={emitters} active={active} particles={live} submeshes={submeshes} drawn={drawn} streamed={} parked={} entities={}{rigs}{residency_line} px={}x{}{cpu}{present} occluded_frames={}{at_pin}{gate}{culled}",
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
                probe.occluded_frames,
            );
            census.print();
            // The window's Modified-event totals per asset type — a type at ~1×/frame here is a
            // per-frame re-upload ratchet (see [`ChurnCensus`]); absent means quiet.
            if !residency.churn.0.is_empty() {
                let churn = residency
                    .churn
                    .0
                    .iter()
                    .map(|(k, n)| format!("{k}={n}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!("MAT_CHURN frames={} {churn}", v.len());
            }
            if std::env::var_os("WOW_ASSET_DUMP").is_some() {
                residency.dump();
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
/// wall is a completely different defect depending on whether it carries
/// [`crate::exterior_cull::ExteriorScene`] (tagged but admitted — the cull or the bound is wrong) or
/// does not (nothing is gating it — the wrong lane spawned it). Naming which took a screenshot, an
/// asset dig and a wrong guess; this line answers it in one run.
///
/// `WOW_VIS_DUMP=1` then names the models: one `VIS_DUMP` line per distinct visible label, ungated
/// first, most-drawn first — which is the "so WHICH trees are they?" question.
#[derive(Default)]
struct VisCensus {
    /// Per [`ModelKind`] index: `(visible, visible-and-gated)`.
    kinds: [(usize, usize); 4],
    /// `label -> (visible count, gated)`, for the dump.
    labels: std::collections::HashMap<(String, bool), usize>,
    /// Of every `ExteriorScene`-tagged submesh: how many the cull actually wrote `Hidden` on, and
    /// how many carry **no `Aabb`** — the cull's fail-open arm, which admits them unconditionally.
    /// A tagged-but-drawn object is one of those two, and they are opposite bugs.
    gated_total: usize,
    gated_hidden: usize,
    gated_no_aabb: usize,
    /// Tagged, bounded, NOT exempt, and yet NOT `Hidden` — the escapees, by `(label, is a
    /// billboard card)`. **Exempt** means the piece belongs to the placement the camera is standing
    /// in, which is not exterior scene to itself (decision 0784) and is *supposed* to draw; without
    /// that subtraction this list is all room-you-are-in furniture and says nothing.
    escaped: std::collections::HashMap<(String, bool), usize>,
    /// The placement the camera is inside, for that subtraction.
    own_instance: Option<Entity>,
    /// How many tagged pieces were exempt — the number that explains the `tagged` vs `hidden` gap.
    exempt: usize,
}

/// What [`VisCensus`] reads off every model submesh — the query shape and its fetched row.
type CensusData = (
    &'static ViewVisibility,
    &'static crate::debug_panel::ModelPart,
    Has<crate::exterior_cull::ExteriorScene>,
    Option<&'static crate::interact::WorldObject>,
    &'static Visibility,
    Option<&'static bevy::camera::primitives::Aabb>,
    Has<crate::billboard::BillboardCard>,
    Option<&'static crate::wmo_portal::WmoGroupVis>,
);

type CensusRow<'a> = (
    &'a ViewVisibility,
    &'a crate::debug_panel::ModelPart,
    bool,
    Option<&'a crate::interact::WorldObject>,
    &'a Visibility,
    Option<&'a bevy::camera::primitives::Aabb>,
    bool,
    Option<&'a crate::wmo_portal::WmoGroupVis>,
);

impl VisCensus {
    fn add(&mut self, (vis, part, gated, object, want, aabb, card, group): CensusRow) {
        if gated {
            let exempt = group.is_some_and(|g| Some(g.instance) == self.own_instance);
            self.gated_total += 1;
            self.gated_hidden += usize::from(*want == Visibility::Hidden);
            self.gated_no_aabb += usize::from(aabb.is_none());
            self.exempt += usize::from(exempt);
            if *want != Visibility::Hidden && aabb.is_some() && !exempt {
                let label = object.map_or("<unlabelled>", |o| o.label.as_str());
                *self.escaped.entry((label.to_string(), card)).or_default() += 1;
            }
        }
        if !vis.get() {
            return;
        }
        let slot = &mut self.kinds[kind_index(part.kind)];
        slot.0 += 1;
        slot.1 += usize::from(gated);
        if let Some(o) = object {
            *self.labels.entry((o.label.clone(), gated)).or_default() += 1;
        }
    }

    fn print(&self) {
        let line = ["doodad", "wmo", "creature", "gameobject"]
            .iter()
            .zip(&self.kinds)
            .map(|(name, (vis, gated))| format!("{name}={vis}/gated{gated}"))
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "VIS_CENSUS visible-submeshes {line} | tagged={} hidden={} exempt={} no_aabb={}",
            self.gated_total, self.gated_hidden, self.exempt, self.gated_no_aabb
        );
        // The escapees always print: a tagged, bounded object the cull left un-hidden is a defect
        // by construction, and burying it behind a flag is how it stays unnoticed.
        let mut escapees: Vec<_> = self.escaped.iter().collect();
        escapees.sort_by(|a, b| b.1.cmp(a.1));
        for ((label, card), n) in escapees {
            let c = if *card { " BILLBOARD-CARD" } else { "" };
            println!("VIS_ESCAPED {n}{c} {label}");
        }
        if std::env::var_os("WOW_VIS_DUMP").is_none() {
            return;
        }
        let mut rows: Vec<_> = self.labels.iter().collect();
        // Ungated first (the leak candidates), then most-drawn first.
        rows.sort_by(|a, b| a.0 .1.cmp(&b.0 .1).then(b.1.cmp(a.1)));
        for ((label, gated), n) in rows {
            let g = if *gated { "gated" } else { "UNGATED" };
            println!("VIS_DUMP {g} {n} {label}");
        }
    }
}

/// [`ModelKind`] has a private `index`; the census needs its own (and pins the column order).
fn kind_index(kind: crate::debug_panel::ModelKind) -> usize {
    match kind {
        crate::debug_panel::ModelKind::Doodad => 0,
        crate::debug_panel::ModelKind::Wmo => 1,
        crate::debug_panel::ModelKind::Creature => 2,
        crate::debug_panel::ModelKind::GameObject => 3,
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
///
/// **`WOW_PARTICLE_CENSUS=+<secs>` fires that long after the world is first SHOWN** (the loading
/// screen dropping) instead of after app start. Everything that rides the appear ramp — decision
/// 0827/0833's `alpha` column above all — lives in a 2-second window whose start moves by *seconds*
/// between a warm and a cold load, and a wall-clock timer either lands in it or does not: three
/// runs in a row missed it while the question was "does a weapon glow ramp with its wearer?". A
/// probe should not be a dice roll, and the ramp's own trigger is the thing to time from.
pub(crate) struct ParticleCensusPlugin;

impl Plugin for ParticleCensusPlugin {
    fn build(&self, app: &mut App) {
        let spec = std::env::var("WOW_PARTICLE_CENSUS").unwrap_or_default();
        let after_shown = spec.starts_with('+');
        let at = spec
            .trim_start_matches('+')
            .parse()
            .unwrap_or(if after_shown { 1.0 } else { 10.0 });
        app.insert_resource(ParticleCensus {
            at,
            after_shown,
            fired: false,
        })
        .add_systems(Update, fire_particle_census);
    }
}

/// [`ParticleCensusPlugin`] state: the fire time and the once-latch.
#[derive(Resource)]
struct ParticleCensus {
    at: f32,
    /// `at` is measured from the world being shown, not from app start — and is rewritten into an
    /// absolute time the first frame the loading screen stops covering.
    after_shown: bool,
    fired: bool,
}

fn fire_particle_census(
    mut probe: ResMut<ParticleCensus>,
    screen: Res<crate::loading_screen::LoadingScreen>,
    time: ProbeClock,
    view: Res<crate::view::ViewDistance>,
    cam: Query<&GlobalTransform, With<crate::player::WorldCamera>>,
    emitters: Query<(
        &crate::particles::ParticleEmitter,
        Option<&crate::particles::EmitterFade>,
        Option<&bevy::camera::visibility::RenderLayers>,
    )>,
) {
    // Latch the shown-relative deadline the first frame the screen drops (and only then: while it
    // still covers there is no ramp to be relative to).
    if probe.after_shown {
        if screen.covering() {
            return;
        }
        probe.at += time.elapsed_secs();
        probe.after_shown = false;
    }
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
    for (e, fade, layers) in &emitters {
        let world_layer =
            layers.is_none_or(|l| l.intersects(&bevy::camera::visibility::RenderLayers::default()));
        // Depth to the OWNER's fade sphere where there is one (the gate's own subject), else the
        // emitter's live anchor — so the number always names what the gate actually tests.
        let depth = cam_tf.map(|t| {
            let center = fade.map_or_else(|| e.anchor_world(), |f| f.center);
            let radius = fade.map_or(0.0, |f| f.radius);
            (center - t.translation()).dot(Vec3::from(t.forward())) - radius
        });
        let drawn = e.drawn();
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
            "PARTICLE_CENSUS_EMITTER blend={:?} flags={:#06x} rate=[{}] life={:.2} tex={} live={} alpha={:.2}{dist}{plane}",
            d.blend,
            d.flags,
            rate_keys.join(","),
            d.params.sample(None, 0.0, 0.0).lifespan,
            d.texture.as_deref().unwrap_or("-"),
            e.live(),
            // The frame's composed MODEL alpha (decision 0827/0833) — the number that answers
            // "this cloud is drawing, why can't I see it / why is it full strength?". An effect on
            // a unit that has not appeared yet reads ~0; one with no model above it reads 1.
            e.render_alpha(),
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

#[cfg(test)]
mod tests {
    /// **The invariant, checked instead of remembered** (decision 0789).
    ///
    /// Naming the right clock [`ProbeClock`] makes it easy to reach for; it does not make the wrong
    /// one unavailable, and `Res<Time>` is the shorter, prelude-blessed, obvious spelling. That
    /// asymmetry is precisely why this same clock has now cost three lanes a correctness bug
    /// (0615's replay clock, the UI script clock, and B131's discarded probe leg) — each time it was
    /// fixed where it was found and left available everywhere else. So the fix is not another
    /// convention: it is this test, in the suite the gates already run.
    ///
    /// Adding a virtual clock to the probe harness now means adding yourself to [`ALLOWED`] with a
    /// reason, which is a much better conversation than discovering the drift in a thrown-away leg.
    #[test]
    fn probe_schedules_read_the_wall_clock() {
        /// `(file, system, why it is genuinely an age or a delta on the animating clock)`.
        const ALLOWED: &[(&str, &str, &str)] = &[(
            "capture/mod.rs",
            "drive_capture",
            "the fixture AGE runs on the clock the effect animates on, and the capture freezes that \
             same clock at save time — an age, not a schedule",
        )];

        // The needles are assembled at runtime so the checker does not flag **its own source** —
        // which is exactly what it did on its first run, and is the cheapest possible proof that it
        // has teeth.
        let bare = format!(": Res<{}>,", "Time");
        let bare_last = format!(": Res<{}>", "Time");
        let explicit = format!("Res<{}<{}>>", "Time", "Virtual");

        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut allowed_seen = 0usize;
        // The probe harness: everything under `capture/`, plus the one probe that lives beside the
        // player controller because it writes `face_yaw` directly.
        let mut stack = vec![src.join("capture")];
        let mut files = vec![src.join("player/probe_look.rs")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("probe harness dir is readable") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push(path);
                }
            }
        }
        for path in files {
            let rel = path
                .strip_prefix(&src)
                .expect("under src")
                .to_string_lossy()
                .replace('\\', "/");
            let text = std::fs::read_to_string(&path).expect("source is readable");
            for (n, line) in text.lines().enumerate() {
                let t = line.trim();
                // A system param binding the virtual clock. `Time<Real>` is the whole point, and
                // `ResMut<Time<Virtual>>` is the capture's own clock *control*, not a read of it.
                let virtual_clock =
                    (t.ends_with(&bare) || t.ends_with(&bare_last)) || t.contains(&explicit);
                if !virtual_clock {
                    continue;
                }
                match ALLOWED.iter().find(|(f, _, _)| *f == rel) {
                    Some(_) => allowed_seen += 1,
                    None => offenders.push(format!("{rel}:{}  {t}", n + 1)),
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "the probe harness must schedule on the wall clock (`ProbeClock`), not the virtual \
             clock — it is clamped to max_delta (250 ms), so any hitching leg silently drifts every \
             `<secs>` knob out from under the operator (decision 0789). Offenders:\n  {}",
            offenders.join("\n  "),
        );
        assert!(
            allowed_seen > 0,
            "the ALLOWED exception list is stale — nothing matched it. If the fixture-age clock \
             moved or went away, drop its entry rather than leaving a rule guarding nothing.",
        );
    }
}
