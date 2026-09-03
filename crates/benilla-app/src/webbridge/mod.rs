//! **The JavaScript bridge** — the page's window onto the running game, and its hand on the
//! controls. The browser build's only other channels are the boot-time ones (`webenv`: the
//! page's env object and the URL, read once; `webprogress`: two progress stages out); this is
//! the live one, both ways, and it is what a web HUD, a proximity-chat overlay, a gamepad
//! mapper or an idle-play script is built on. `web/README.md` § "JavaScript bridge" is the
//! page-side contract; `web/bridge.js` is the page-side wrapper.
//!
//! **The shape**, in the house pattern of `webenv`/`webprogress`: the wasm exports nothing new.
//! Once a frame the bridge looks up `window.__wenilla_bridge` (`Reflect::get`, one call); an
//! absent object means an idle bridge at that one call's cost. Present, it is read and written
//! through plain properties the page defined:
//!
//! - **out:** `onFrame(snapshot)` at most `hz` times a second (a plain object, positions in WoW
//!   coordinates, guids as hex strings), and `onEvent(name, payload)` for every event as it
//!   happens — the whole FrameXML event stream through the VM's tap, plus the bridge's own
//!   (`ready`, `state`, `map`, `zone`, `chat` with the sender's guid, `lua` results, `input`,
//!   `error`).
//! - **in:** a `queue` array of command objects the page pushes and the bridge drains every
//!   frame: `hold`/`fire` a binding command **by name** (`MOVEFORWARD`, `JUMP`, `ACTIONBUTTON1`
//!   — the same 202 names the Key Bindings window shows, through the same [`BindingsState`]
//!   every engine system already reads, so a page cannot do anything a key cannot), `look`
//!   (the scripted mouse-turn, `Player::turn_aim`), `lua` (evaluate a chunk, answered by a
//!   `lua` event), `chat` (a line as if typed into the chat box, so `/say` and `/target` parse
//!   client-side), `release`.
//! - **`wake`:** a function the bridge installs on the object, which wakes the winit loop
//!   (`WinitUserEvent::WakeUp`). A hidden tab has no animation frames and throttled timers;
//!   the page keeps the game ticking from a Worker by calling this — the notifications-while-
//!   in-the-background feature stands on it.
//!
//! **Policy, not security.** Everything here is reachable only by same-origin page script
//! (the host's COOP/COEP isolate the page), which could already synthesize DOM key events on
//! the canvas and edit the saved variables in `localStorage`; the bridge adds convenience, not
//! reach. Whether *automation* is welcome on a realm is the operator's call, so a page can turn
//! the bridge off for a session with `bridge: "0"` in `window.__wenilla_env` (`?bridge=0`),
//! read once at build like every other env key.
//!
//! **The seams it stands on**, none of them dev-gated (this is a player-side plugin, compiled
//! into the browser build the dev instruments are compiled out of): [`BindingsState::synth_hold`]
//! and kin, the VM's event tap ([`UiScript::set_event_tap`]), the chat router's observer copy
//! ([`ChatWindows::routed`]), the zone resolve's resource ([`crate::area::ZoneInfo`]), and the
//! same snapshot builder the unit frames use ([`crate::ui_unit::snapshot`]).

mod sink;
mod snapshot;

use std::collections::HashSet;

use bevy::prelude::*;

use benilla_ui::script::plain::PlainValue;
use benilla_ui::script::UiScript;
use benilla_world::schedule::WorldStage;
use benilla_world::world_map::MapChange;

use crate::area::{ZoneFeed, ZoneInfo};
use crate::bindings::commands::{Cmd, Kind, SPECS};
use crate::bindings::{BindingSet, BindingsState};
use crate::char_select::ClientState;
use crate::player::Player;
use crate::ui_chat::{event_name, ChatWindows};
use crate::ui_script::{UiInput, UiKeyboardCapture, VmMemo};
use crate::ui_unit::UnitFeed;

pub(crate) use snapshot::BridgeReadout;

/// The contract version the `ready` event reports; bump on an incompatible schema change.
pub(crate) const VERSION: u32 = 1;

/// The inbound half: the queue drain and the VM-side ops, after this frame's wire and before
/// the VM ticks (a `lua` op that queues a cast is serviced by the drains after [`UiInput`]).
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct BridgeIn;

/// The outbound half: events and the frame, after the VM tick, the unit feed and the zone
/// resolve, so one frame's snapshot, events and chat are coherent with each other.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct BridgeOut;

/// Which Lua events the page subscribed to (`__wenilla_bridge.events`): nothing, everything
/// (`"*"`), or a list of names.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))] // only the page's reader builds them
pub(crate) enum EventFilter {
    #[default]
    None,
    All,
    Some(HashSet<String>),
}

impl EventFilter {
    pub(crate) fn wants(&self, name: &str) -> bool {
        match self {
            EventFilter::None => false,
            EventFilter::All => true,
            EventFilter::Some(set) => set.contains(name),
        }
    }
}

/// The page's knobs, re-read from the hook object every frame (six property reads) so a page
/// can turn them live.
#[derive(Resource, Clone, Debug, PartialEq)]
pub(crate) struct BridgeConfig {
    /// `onFrame` calls per second; `0` is every frame.
    pub(crate) hz: f32,
    /// Units within this many yards of the player go in `units[]`.
    pub(crate) radius: f32,
    /// …and at most this many of them, nearest first.
    pub(crate) max_units: usize,
    pub(crate) events: EventFilter,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            hz: 20.0,
            radius: 60.0,
            max_units: 64,
            events: EventFilter::None,
        }
    }
}

/// One command object off the page's queue, parsed. Unknown ops and malformed objects never
/// get this far ([`sink`] reports them as `error` events).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))] // only the page's queue builds them
pub(crate) enum BridgeCommand {
    /// Assert or release a `Kind::Held` command by name.
    Hold { cmd: String, down: bool },
    /// Fire a command once: a `Kind::Host` edge with an amount, or an Edge/EdgeUpDown body.
    Fire { cmd: String, amount: f32 },
    /// Turn the aim by radians (positive = counter-clockwise, the wire's orientation sense).
    Look { dyaw: f32 },
    /// Evaluate a chunk; answered by a `lua` event carrying `id`.
    Lua { id: u32, chunk: String },
    /// A line as if typed into the chat box and submitted.
    Chat(String),
    /// Drop every synthetic hold.
    Release,
}

/// What the page asked for, resolved to commands and held until the systems that own each
/// seam run. `held` is stateful — the page sends edges, the bridge re-asserts every frame —
/// so a stick still deflected after a `/reload` or a focus edge resumes on its own.
#[derive(Resource, Default)]
pub(crate) struct BridgeInput {
    held: HashSet<Cmd>,
    /// Holds the page let go of this frame — their synthetic latches go with them.
    released: Vec<Cmd>,
    /// Held-kind commands the page *fired*: a one-frame tap — latched this frame, released
    /// the next (`tap_prev`), unless the page also holds them.
    taps: Vec<Cmd>,
    tap_prev: Vec<Cmd>,
    fires: Vec<(Cmd, f32)>,
    /// Edge / EdgeUpDown commands fired from the page: their Lua bodies run in the VM.
    edges: Vec<Cmd>,
    look: f32,
    lua: Vec<(u32, String)>,
    chat: Vec<String>,
    /// Drop every synthetic latch this frame (a `release`, or the hook object vanishing).
    drop_holds: bool,
}

impl BridgeInput {
    /// Take one parsed command in. A name that is not a registry command is reported once
    /// (an `error` event) and then ignored.
    pub(crate) fn apply(
        &mut self,
        cmd: BridgeCommand,
        memo: &mut BridgeMemo,
        out: &mut BridgeOutbox,
    ) {
        match cmd {
            BridgeCommand::Hold { cmd, down } => {
                let Some(c) = resolve(&cmd, memo, out) else {
                    return;
                };
                if down {
                    self.held.insert(c);
                } else if self.held.remove(&c) {
                    self.released.push(c);
                }
            }
            BridgeCommand::Fire { cmd, amount } => {
                let Some(c) = resolve(&cmd, memo, out) else {
                    return;
                };
                match SPECS[c.0 as usize].kind {
                    Kind::Host => self.fires.push((c, amount)),
                    Kind::Edge(_) | Kind::EdgeUpDown(..) => self.edges.push(c),
                    // A one-frame hold: the page meant a tap.
                    Kind::Held => self.taps.push(c),
                }
            }
            BridgeCommand::Look { dyaw } => self.look += dyaw,
            BridgeCommand::Lua { id, chunk } => self.lua.push((id, chunk)),
            BridgeCommand::Chat(line) => self.chat.push(line),
            BridgeCommand::Release => {
                self.held.clear();
                self.drop_holds = true;
            }
        }
    }
}

/// A registry command by its `Bindings.xml` name.
pub(crate) fn command_by_name(name: &str) -> Option<Cmd> {
    SPECS
        .iter()
        .position(|s| s.name == name)
        .and_then(|i| u16::try_from(i).ok())
        .map(Cmd)
}

fn resolve(name: &str, memo: &mut BridgeMemo, out: &mut BridgeOutbox) -> Option<Cmd> {
    let found = command_by_name(name);
    if found.is_none() && memo.warned.insert(name.to_string()) {
        out.push(
            "error",
            PlainValue::Map(vec![
                ("reason".into(), PlainValue::Str("unknown command".into())),
                ("cmd".into(), PlainValue::Str(name.to_string())),
            ]),
        );
    }
    found
}

/// Events waiting to go out — filled by any system, drained by [`publish_frame`] in order.
#[derive(Resource, Default)]
pub(crate) struct BridgeOutbox(Vec<(String, PlainValue)>);

impl BridgeOutbox {
    pub(crate) fn push(&mut self, name: &str, payload: PlainValue) {
        self.0.push((name.to_string(), payload));
    }
}

/// What the bridge remembers between frames.
#[derive(Resource, Default)]
pub(crate) struct BridgeMemo {
    /// The hook object was there last frame.
    hook: bool,
    /// The seconds (`Time::elapsed`) the next `onFrame` is due.
    next_publish: f64,
    seq: u64,
    /// The last `state` event sent: `(state name, connected)`.
    state: Option<(&'static str, bool)>,
    /// The last `map` event sent.
    map: Option<u32>,
    /// Unknown command names already reported.
    warned: HashSet<String>,
    /// Whether the event tap is armed in the CURRENT VM (a new VM starts with it off).
    tap: VmMemo<bool>,
}

/// The bridge, wired. Absent entirely when the session opted out (`bridge: "0"`).
pub(crate) struct WebBridgePlugin;

impl Plugin for WebBridgePlugin {
    fn build(&self, app: &mut App) {
        if crate::webenv::var("WOW_BRIDGE").as_deref() == Some("0") {
            info!("webbridge: off for this session (bridge=0)");
            return;
        }
        app.init_resource::<BridgeConfig>()
            .init_resource::<BridgeInput>()
            .init_resource::<BridgeOutbox>()
            .init_resource::<BridgeMemo>()
            .init_non_send_resource::<sink::WakeHook>()
            .configure_sets(Update, BridgeIn.after(WorldStage::Net).before(UiInput))
            .configure_sets(
                Update,
                BridgeOut.after(UiInput).after(UnitFeed).after(ZoneFeed),
            )
            .add_systems(
                Update,
                (drain_page_queue, run_vm_ops).chain().in_set(BridgeIn),
            )
            .add_systems(
                Update,
                apply_synth_input
                    .in_set(UiInput)
                    .after(BindingSet)
                    .before(WorldStage::Input)
                    .run_if(in_state(ClientState::InWorld)),
            )
            .add_systems(Update, publish_frame.in_set(BridgeOut))
            // Both edges of the world. On the way out for the obvious reason; on the way IN
            // because `apply_synth_input` is the only consumer of `fires`/`edges`/`taps`/`look`
            // and it does not run outside the world — so a page driving the character screen
            // would otherwise pile them up and have them all replay in the first in-world frame,
            // Lua bodies and one accumulated `turn_aim` together.
            .add_systems(OnEnter(ClientState::InWorld), release_synth_input)
            .add_systems(OnExit(ClientState::InWorld), release_synth_input);
    }
}

/// The inbound drain: notice the hook coming and going, read the knobs, parse the queue.
fn drain_page_queue(
    mut cfg: ResMut<BridgeConfig>,
    mut input: ResMut<BridgeInput>,
    mut memo: ResMut<BridgeMemo>,
    mut out: ResMut<BridgeOutbox>,
    mut windows: Option<ResMut<ChatWindows>>,
    mut wake: NonSendMut<sink::WakeHook>,
    proxy: Option<Res<bevy::winit::EventLoopProxyWrapper>>,
) {
    let present = sink::hook_present();
    if !present {
        if memo.hook {
            // The page let go of the object (a reload of bridge.js, a page tearing its HUD
            // down): every hold it asserted goes with it, and nothing is cloned for it any more.
            memo.hook = false;
            input.held.clear();
            input.drop_holds = true;
            if let Some(w) = windows.as_deref_mut() {
                w.observe = false;
                w.routed.clear();
            }
            wake.0 = None;
        }
        return;
    }
    if !memo.hook {
        memo.hook = true;
        out.push("ready", ready_payload());
        wake.0 = proxy.as_deref().and_then(sink::install_wake);
    }
    sink::read_config(&mut cfg);
    if let Some(w) = windows.as_deref_mut() {
        w.observe = true;
    }
    for cmd in sink::drain_queue(&mut out) {
        input.apply(cmd, &mut memo, &mut out);
    }
}

/// The `ready` payload: the contract version and every command name the page may `hold`/`fire`.
fn ready_payload() -> PlainValue {
    let commands = SPECS
        .iter()
        .map(|s| {
            let kind = match s.kind {
                Kind::Held => "held",
                Kind::Host => "host",
                Kind::Edge(_) | Kind::EdgeUpDown(..) => "lua",
            };
            PlainValue::Map(vec![
                ("name".into(), PlainValue::Str(s.name.into())),
                ("kind".into(), PlainValue::Str(kind.into())),
                ("category".into(), PlainValue::Str(s.category.into())),
            ])
        })
        .collect();
    PlainValue::Map(vec![
        ("version".into(), PlainValue::Num(f64::from(VERSION))),
        ("commands".into(), PlainValue::List(commands)),
    ])
}

/// The ops that need the VM: arming the event tap, chat lines, Lua evaluation.
fn run_vm_ops(
    script: Option<NonSendMut<UiScript>>,
    cfg: Res<BridgeConfig>,
    mut input: ResMut<BridgeInput>,
    mut memo: ResMut<BridgeMemo>,
    mut out: ResMut<BridgeOutbox>,
) {
    let Some(mut script) = script else {
        // No VM (the character screen): a chunk cannot run, and saying so beats a promise that
        // never resolves.
        for (id, _) in input.lua.drain(..) {
            out.push("lua", lua_result(id, Err("no VM in this state".into())));
        }
        input.chat.clear();
        return;
    };
    let want = memo.hook && cfg.events != EventFilter::None;
    let armed = memo.tap.get(&script);
    if *armed != want {
        script.set_event_tap(want);
        *armed = want;
    }
    for line in input.chat.drain(..) {
        script.push_chat_input(line);
    }
    for (id, chunk) in input.lua.drain(..) {
        out.push("lua", lua_result(id, script.eval_plain(&chunk)));
    }
}

fn lua_result(id: u32, result: Result<Vec<PlainValue>, String>) -> PlainValue {
    let mut m = vec![("id".into(), PlainValue::Num(f64::from(id)))];
    match result {
        Ok(values) => {
            m.push(("ok".into(), PlainValue::Bool(true)));
            m.push(("values".into(), PlainValue::List(values)));
        }
        Err(e) => {
            m.push(("ok".into(), PlainValue::Bool(false)));
            m.push(("error".into(), PlainValue::Str(e)));
        }
    }
    PlainValue::Map(m)
}

/// The control seam: after the key dispatch ([`BindingSet`]) cleared this frame's edges, before
/// the controller reads them. Holds are re-asserted every frame; a chat box with focus stops them like it
/// stops keys (and, a named divergence, they resume when it loses focus — a stick has no
/// re-press).
fn apply_synth_input(
    mut binds: ResMut<BindingsState>,
    mut input: ResMut<BridgeInput>,
    capture: Res<UiKeyboardCapture>,
    script: Option<NonSendMut<UiScript>>,
    player: Option<ResMut<Player>>,
    mut out: ResMut<BridgeOutbox>,
) {
    if input.drop_holds {
        input.drop_holds = false;
        input.released.clear();
        binds.synth_release_all();
        out.push(
            "input",
            PlainValue::Map(vec![("heldCleared".into(), PlainValue::Bool(true))]),
        );
    }
    for c in input.released.drain(..) {
        binds.synth_release(c);
    }
    let prev_taps = std::mem::take(&mut input.tap_prev);
    for c in prev_taps {
        if !input.held.contains(&c) {
            binds.synth_release(c);
        }
    }
    if capture.typing {
        binds.synth_release_all();
        input.taps.clear();
    } else {
        for &c in &input.held {
            binds.synth_hold(c);
        }
        let taps = std::mem::take(&mut input.taps);
        for &c in &taps {
            binds.synth_hold(c);
        }
        input.tap_prev = taps;
    }
    for (c, amount) in input.fires.drain(..) {
        binds.synth_fire(c, amount);
    }
    if !input.edges.is_empty() {
        if let Some(script) = script.as_deref() {
            for c in input.edges.drain(..) {
                match SPECS[c.0 as usize].kind {
                    Kind::Edge(body) => crate::ui_script::run_or_warn(script, body),
                    // A page's "fire" of a button is a press AND its release, back to back —
                    // the wheel-notch law, which is what makes an action button cast.
                    Kind::EdgeUpDown(down, up) => {
                        crate::ui_script::run_or_warn(script, down);
                        crate::ui_script::run_or_warn(script, up);
                    }
                    Kind::Held | Kind::Host => {}
                }
            }
        } else {
            input.edges.clear();
        }
    }
    if input.look != 0.0 {
        if let Some(mut player) = player {
            player.turn_aim(input.look);
        }
        input.look = 0.0;
    }
}

/// Drop every synthetic assertion and everything queued for one, on both edges of the world.
fn release_synth_input(mut binds: ResMut<BindingsState>, mut input: ResMut<BridgeInput>) {
    binds.synth_release_all();
    input.held.clear();
    input.released.clear();
    input.taps.clear();
    input.tap_prev.clear();
    input.fires.clear();
    input.edges.clear();
    input.look = 0.0;
    // The pending "clear everything" flag goes too: it is consumed by `apply_synth_input`, which
    // does not run out of world, so a hook that vanished at the character screen would otherwise
    // leave it set and fire a spurious `heldCleared` on the next world entry.
    input.drop_holds = false;
}

/// The outbound half: this frame's events, then the frame itself if it is due.
#[allow(clippy::too_many_arguments)]
fn publish_frame(
    readout: BridgeReadout,
    cfg: Res<BridgeConfig>,
    mut memo: ResMut<BridgeMemo>,
    mut out: ResMut<BridgeOutbox>,
    script: Option<NonSendMut<UiScript>>,
    mut windows: Option<ResMut<ChatWindows>>,
    zone: Option<Res<ZoneInfo>>,
    mut map_changes: MessageReader<MapChange>,
) {
    // Drain what would pile up regardless, so a page that is not there costs nothing.
    let mut chat = windows
        .as_deref_mut()
        .map(|w| std::mem::take(&mut w.routed))
        .unwrap_or_default();
    let mut tapped = script
        .map(|mut s| s.take_tapped_events())
        .unwrap_or_default();
    let map_changed = map_changes.read().count() > 0;
    if !memo.hook {
        out.0.clear();
        return;
    }

    // ── events, in order: the bridge's own, then the session's ──
    for (name, payload) in out.0.drain(..) {
        sink::emit_event(&name, &payload);
    }
    let state = (readout.state_name(), readout.connected());
    if memo.state != Some(state) {
        memo.state = Some(state);
        sink::emit_event(
            "state",
            &PlainValue::Map(vec![
                ("state".into(), PlainValue::Str(state.0.into())),
                ("connected".into(), PlainValue::Bool(state.1)),
            ]),
        );
    }
    let map = readout.map_id();
    if map_changed || memo.map != map {
        memo.map = map;
        sink::emit_event(
            "map",
            &PlainValue::Map(vec![(
                "id".into(),
                map.map_or(PlainValue::Null, |m| PlainValue::Num(f64::from(m))),
            )]),
        );
    }
    let zone_changed = zone.as_ref().is_some_and(|z| z.is_changed());
    if let Some(zone) = zone.as_deref() {
        if zone.zone_id != 0 && (memo.seq == 0 || zone_changed) {
            sink::emit_event("zone", &snapshot::zone_payload(zone));
        }
    }
    for event in chat.drain(..) {
        let Some(kind) = event.kind else { continue };
        sink::emit_event("chat", &snapshot::chat_payload(event_name(kind), &event));
    }
    for (name, args) in tapped.drain(..) {
        if cfg.events.wants(&name) {
            sink::emit_event(
                "event",
                &PlainValue::Map(vec![
                    ("name".into(), PlainValue::Str(name)),
                    (
                        "args".into(),
                        PlainValue::List(args.iter().map(PlainValue::from).collect()),
                    ),
                ]),
            );
        }
    }

    // ── the frame, throttled ──
    let now = readout.now();
    if cfg.hz > 0.0 {
        if now < memo.next_publish {
            return;
        }
        // Only the throttled path schedules. Doing it unconditionally meant `hz: 0` ("every
        // frame") clamped to 0.1 and booked the next slot TEN SECONDS out — invisible while
        // `hz` stayed 0, then a ten-second silence the moment the page set a real rate.
        memo.next_publish = now + 1.0 / f64::from(cfg.hz.clamp(0.1, 120.0));
    }
    memo.seq += 1;
    let frame = readout.build(&cfg, memo.seq, zone.as_deref());
    sink::emit_frame(&frame);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::cmd;

    #[test]
    fn commands_resolve_by_their_bindings_xml_name() {
        assert_eq!(command_by_name("MOVEFORWARD"), Some(cmd::MOVE_FORWARD));
        assert_eq!(command_by_name("JUMP"), Some(cmd::JUMP));
        assert_eq!(command_by_name("nope"), None);
    }

    #[test]
    fn the_input_sorts_commands_by_kind_and_reports_unknown_names_once() {
        let mut input = BridgeInput::default();
        let mut memo = BridgeMemo::default();
        let mut out = BridgeOutbox::default();
        input.apply(
            BridgeCommand::Hold {
                cmd: "MOVEFORWARD".into(),
                down: true,
            },
            &mut memo,
            &mut out,
        );
        input.apply(
            BridgeCommand::Fire {
                cmd: "JUMP".into(),
                amount: 1.0,
            },
            &mut memo,
            &mut out,
        );
        input.apply(
            BridgeCommand::Fire {
                cmd: "ACTIONBUTTON1".into(),
                amount: 1.0,
            },
            &mut memo,
            &mut out,
        );
        input.apply(
            BridgeCommand::Fire {
                cmd: "STRAFELEFT".into(),
                amount: 1.0,
            },
            &mut memo,
            &mut out,
        );
        assert!(input.held.contains(&cmd::MOVE_FORWARD));
        assert_eq!(input.fires, vec![(cmd::JUMP, 1.0)]);
        assert_eq!(input.edges, vec![command_by_name("ACTIONBUTTON1").unwrap()]);
        assert_eq!(
            input.taps,
            vec![cmd::STRAFE_LEFT],
            "a fired hold is a one-frame tap"
        );
        assert!(!input.held.contains(&cmd::STRAFE_LEFT));
        assert!(out.0.is_empty());

        for _ in 0..2 {
            input.apply(
                BridgeCommand::Hold {
                    cmd: "NOPE".into(),
                    down: true,
                },
                &mut memo,
                &mut out,
            );
        }
        assert_eq!(out.0.len(), 1, "one error per unknown name");
        assert_eq!(out.0[0].0, "error");

        input.apply(
            BridgeCommand::Hold {
                cmd: "MOVEFORWARD".into(),
                down: false,
            },
            &mut memo,
            &mut out,
        );
        assert!(!input.held.contains(&cmd::MOVE_FORWARD));
        assert!(input.released.contains(&cmd::MOVE_FORWARD));
    }

    #[test]
    fn the_ready_payload_lists_every_registry_command() {
        let PlainValue::Map(m) = ready_payload() else {
            panic!()
        };
        let PlainValue::List(cmds) = &m[1].1 else {
            panic!()
        };
        assert_eq!(cmds.len(), SPECS.len());
    }
}
