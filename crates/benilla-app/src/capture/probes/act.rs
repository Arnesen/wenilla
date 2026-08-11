//! The **actuation** probes: the three headless channels into a live session — scripted chat
//! sends ([`ProbeChatPlugin`]), synthetic key taps ([`ProbeKeyPlugin`]), and a Lua chunk in the
//! live UI VM ([`ProbeLuaPlugin`]). Each is env-gated, waits for the session to be in-world, and
//! schedules on [`ProbeClock`]; together they are "do what the director would do, unattended".

use bevy::prelude::*;

use super::ProbeClock;

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
        // The text-editing keys. Added for decision 1077's hyperlink-atomicity law, whose whole
        // observable — "one BACKSPACE removes a whole item link" — is a keypress no chat command
        // and no Lua chunk can reach (`EditBox` has no Lua deletion API; the law lives behind the
        // host's chord table). Without these a caret/deletion defect could only be reproduced by
        // asking the director to type, which is the asymmetry probes exist to remove.
        "Backspace" => KeyCode::Backspace,
        "Delete" => KeyCode::Delete,
        "Left" => KeyCode::ArrowLeft,
        "Right" => KeyCode::ArrowRight,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
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

/// [`ProbeLuaPlugin`] state: the chunk, the fire time, and the once-latch.
#[derive(Resource)]
struct ProbeLua {
    chunk: String,
    at: f32,
    fired: bool,
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
