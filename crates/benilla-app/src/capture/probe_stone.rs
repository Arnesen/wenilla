//! The meeting-stone live probe (`WOW_PROBE_STONE=1`) — decision 2283's instrument: **can a
//! player actually get into the LFG queue, and does everything downstream of that light up?**
//!
//! Until 2283 a right-click on a meeting stone sent `CMSG_GAMEOBJ_USE`, which vmangos' own
//! `GameObject::Use` answers with an explicit `return` for type 23. So the whole receiving half —
//! the two globals, the five-way status table, `MEETINGSTONE_CHANGED`, the stock
//! `MiniMapMeetingStoneFrame` — had been built and tested against synthetic packets and had never
//! once run off a real one. This probe is what closes that: it drives the click's own route and
//! then reads the answer out of the **live VM**, not out of our own state.
//!
//! **Why a probe and not a capture:** none of this is a picture. Every reading here is a number or
//! a Lua truth value — the queued area id, `IsInMeetingStoneQueue()`, whether the stock minimap
//! button is shown — so the verdict needs no eye in the loop (`docs/METHOD.md` step 4).
//!
//! **Where it enters the chain, exactly.** It writes the very
//! [`MeetingStoneUse`] the click ladder writes, so everything **downstream** of that seam runs
//! for real — the validator, `CMSG 0x292`, the server, `SMSG 0x295`, the feed, the two globals,
//! the stock frame. It does **not** cover the mouse hit-test or the shared gates *above* the seam
//! (highlightable, mounted, range); those are the click test's
//! (`target::click::tests::a_right_click_on_a_meeting_stone_joins_it_and_sends_no_gameobj_use`).
//! Neither alone closes the chain; together they do.
//!
//! ## The five legs, in order
//!
//! 1. **CONTROL** — parked at the stone, nothing clicked: `IsInMeetingStoneQueue()` must be `nil`
//!    and the stock icon hidden. A probe that only ever sees the queued state cannot tell a fix
//!    from a stuck flag.
//! 2. **REFUSED** — the body demoted **below** the stone's `data[0]`, then clicked: the client's
//!    own level refusal must fire, and **nothing** may reach the wire. This is the one of the four
//!    refusals the server does not duplicate — `HandleMeetingStoneJoinOpcode` never looks at the
//!    stone's level band — so if our copy is wrong, the server queues a level-1 character for
//!    Deadmines and nobody notices.
//! 3. **JOINED** — the body levelled **into** the band, then clicked: `SMSG 0x295` must arrive with
//!    this stone's own `data[2]`, `IsInMeetingStoneQueue()` must answer the number `1`,
//!    `GetMeetingStoneStatusText()` a real string, and `MiniMapMeetingStoneFrame` must be shown.
//! 4. **DARKENED** — with that queue held, the stone's own `highlightable` slot must now refuse it
//!    ([`crate::target::cursor_mode::meeting_stone_queued`] over the live area): the stone you are
//!    queued at stops being clickable, which is the whole of `0x5f6990`.
//! 5. **LEFT** — `CancelMeetingStoneRequest()` through the live VM: back to `nil`, icon hidden.
//!
//! ## The run recipe
//!
//! ```text
//! WOW_NOSOUND=1 WOW_UNATTENDED=1 WOW_USER=probe3 WOW_PASS=pprobe3 WOW_CHAR=Probethree \
//!     WOW_PROBE_STONE=1 cargo run -q -p benilla
//! ```
//! (the checkout's probe identity — `.probe-identity`, or WOW_USER/WOW_PASS/WOW_CHAR; the `probe`
//! skill). `WOW_PROBE_STONE=<x>,<y>,<z>[,<map>]` aims it elsewhere.
//!
//! **It leaves the probe body at the stone's minimum level**, deliberately and without putting it
//! back: the level is the probe's own instrument (leg 2 needs one side of the band and leg 3 the
//! other), and a probe character's level is scratch. Grep `PROBE_STONE:` for the verdict; the
//! probe self-exits when it lands.

use bevy::prelude::*;

use benilla_protocol::EntityKind;
use benilla_ui::script::UiScript;

use super::probes::ProbeClock;
use crate::go_templates::{GameObjectTemplates, MeetingStoneTemplate};
use crate::net::{ChatKind, ClientCommand, Guid, NetCommands, NetEntity, ObjectStore, SelfPlayer};
use crate::player::Player;
use crate::target::cursor_mode::meeting_stone_queued;
use crate::ui_dialog_verbs::{MeetingStone, MeetingStoneUse};

/// The probe's default object: the **Stockade** meeting stone in Stormwind (`gameobject.guid`
/// 26635, template 179595 — `data[0..2] = 24, 32, 717`). Chosen because it is a plain unpooled
/// spawn in a capital, its level band is narrow enough that a level-1 body is comfortably outside
/// it, and area 717 resolves to a real `AreaTable` row, so the status text comes out a sentence
/// ("Looking for more for The Stockade") rather than the `UNKNOWN` fallback — which makes the
/// JOINED leg's `text=yes` mean the whole rebuild ran, not just that a string exists.
const STONE_AT: [f32; 4] = [-8810.5, 798.0, 98.2, 0.0];
/// `GAMEOBJECT_TYPE_MEETINGSTONE`.
const GO_TYPE_MEETINGSTONE: i32 = 23;
/// The level leg 2 demotes to — below every shipped stone's `data[0]`.
const LEVEL_BELOW: u32 = 1;

const SCAN_RANGE: f32 = 30.0;
const SETTLE_SECS: f64 = 6.0;
const SCAN_TIMEOUT_SECS: f64 = 30.0;
const LEVEL_TIMEOUT_SECS: f64 = 20.0;
/// How long a refusal is given to *not* happen. The join round trip below is the same server, so
/// anything that was going to arrive has arrived.
const REFUSAL_WINDOW_SECS: f64 = 6.0;
const QUEUE_TIMEOUT_SECS: f64 = 20.0;

type NearbyObject = (
    &'static Guid,
    &'static NetEntity,
    &'static ObjectStore,
    &'static Transform,
);

pub(crate) struct ProbeStonePlugin;

impl Plugin for ProbeStonePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<StoneProbe>()
            .add_systems(Update, stone_probe);
    }
}

#[derive(Resource, Default)]
struct StoneProbe {
    phase: Phase,
    stone: Option<u64>,
    tmpl: Option<MeetingStoneTemplate>,
    fails: u32,
    exited: bool,
    /// What each leg actually read, for the DONE line.
    joined_area: u32,
    status_text: Option<String>,
    icon_shown: bool,
    darkened: bool,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Phase {
    #[default]
    Wait,
    Settling {
        sent_at: f64,
    },
    /// The stone is in range; waiting for its ask-once template to answer.
    WaitTemplate {
        since: f64,
    },
    Control,
    /// `.character level` issued; waiting for the store to report it. `join` says which leg the
    /// click that follows belongs to — **not** the level value, because a stone whose `data[0]`
    /// is 1 would make the two indistinguishable and loop the probe forever.
    WaitLevel {
        want: u32,
        since: f64,
        join: bool,
    },
    /// Clicked below the band; waiting out the window in which nothing may happen.
    Refused {
        since: f64,
    },
    /// Clicked in the band; waiting for `SMSG 0x295`.
    WaitQueue {
        since: f64,
    },
    Joined,
    /// `CancelMeetingStoneRequest()` issued; waiting for the queue to clear.
    WaitLeft {
        since: f64,
    },
    Done,
}

/// Where the probe is aimed: `WOW_PROBE_STONE=<x>,<y>,<z>[,<map>]`, else [`STONE_AT`]. Anything
/// unparseable falls back to the default rather than failing the run — the common value is `1`.
fn target() -> [f32; 4] {
    let Ok(raw) = std::env::var("WOW_PROBE_STONE") else {
        return STONE_AT;
    };
    let parts: Vec<f32> = raw
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    match parts.len() {
        3 => [parts[0], parts[1], parts[2], STONE_AT[3]],
        4 => [parts[0], parts[1], parts[2], parts[3]],
        _ => STONE_AT,
    }
}

/// One leg's verdict line. `got == want` is the whole test; the line says both either way, so a
/// failure reads as a measurement rather than as an assertion name.
fn check(label: &str, want: &str, got: &str) -> u32 {
    if want == got {
        info!("PROBE_STONE: {label:<9} PASS — {got}");
        0
    } else {
        error!("PROBE_STONE: {label:<9} FAIL — wanted {want}, read {got}");
        1
    }
}

/// The three live VM readings every leg is written in, asked of the **stock files**: the two
/// bindings `Minimap.xml` calls, and whether its button is actually up.
fn vm_state(script: &UiScript) -> (bool, Option<String>, bool) {
    let queued = script
        .eval::<bool>("return IsInMeetingStoneQueue() and true or false")
        .unwrap_or(false);
    let text = script
        .eval::<String>("return GetMeetingStoneStatusText()")
        .ok();
    let shown = script
        .eval::<bool>(
            "return MiniMapMeetingStoneFrame and MiniMapMeetingStoneFrame:IsShown() and true or false",
        )
        .unwrap_or(false);
    (queued, text, shown)
}

#[allow(clippy::too_many_arguments)]
fn stone_probe(
    time: ProbeClock,
    mut probe: ResMut<StoneProbe>,
    script: Option<NonSendMut<UiScript>>,
    self_store: Query<&ObjectStore, With<SelfPlayer>>,
    player: Res<Player>,
    objects: Query<NearbyObject, Without<SelfPlayer>>,
    templates: Res<GameObjectTemplates>,
    queue: Res<MeetingStone>,
    mut uses: MessageWriter<MeetingStoneUse>,
    net: Res<NetCommands>,
    mut exit: MessageWriter<AppExit>,
) {
    let Ok(store) = self_store.single() else {
        return; // not in-world yet
    };
    let Some(script) = script else {
        return; // no UI VM this build — nothing this probe can read
    };
    let now = time.elapsed_secs_f64();
    let level = store.0.unit_level().unwrap_or(0);

    match probe.phase {
        Phase::Wait => {
            let [x, y, z, map] = target();
            info!("PROBE_STONE: heading to the meeting stone ({x} {y} {z} map {map})");
            let _ = net.0.send(ClientCommand::Chat {
                kind: ChatKind::Say,
                target: None,
                text: format!(".go xyz {x} {y} {z} {map}"),
            });
            probe.phase = Phase::Settling { sent_at: now };
        }
        Phase::Settling { sent_at } => {
            if now - sent_at < SETTLE_SECS {
                return;
            }
            let me = player.pos;
            let found = objects.iter().find(|(_, net_e, store, tf)| {
                net_e.kind == EntityKind::GameObject
                    && store.0.gameobject_type_id() == GO_TYPE_MEETINGSTONE
                    && tf.translation.distance(me) < SCAN_RANGE
            });
            if let Some((guid, ..)) = found {
                info!("PROBE_STONE: stone {:#x} in range", guid.0);
                probe.stone = Some(guid.0);
                probe.phase = Phase::WaitTemplate { since: now };
            } else if now - sent_at > SCAN_TIMEOUT_SECS {
                error!(
                    "PROBE_STONE: FAIL — no type-{GO_TYPE_MEETINGSTONE} GameObject within \
                     {SCAN_RANGE} yd in {SCAN_TIMEOUT_SECS}s. This is NOT a passing run"
                );
                probe.fails += 1;
                probe.phase = Phase::Done;
            }
        }
        Phase::WaitTemplate { since } => {
            let Some(guid) = probe.stone else {
                probe.phase = Phase::Done;
                return;
            };
            if let Some(t) = templates.get(guid).and_then(|t| t.meeting_stone) {
                info!(
                    "PROBE_STONE: template answered — levels {}..={}, area {}",
                    t.min_level, t.max_level, t.area
                );
                probe.tmpl = Some(t);
                probe.phase = Phase::Control;
            } else if now - since > SCAN_TIMEOUT_SECS {
                error!(
                    "PROBE_STONE: FAIL — the stone's GAMEOBJECT_QUERY never answered in \
                     {SCAN_TIMEOUT_SECS}s; the level band cannot be read"
                );
                probe.fails += 1;
                probe.phase = Phase::Done;
            }
        }
        Phase::Control => {
            let (queued, _, shown) = vm_state(&script);
            probe.fails += check(
                "CONTROL",
                "unqueued icon=false",
                &format!("{}queued icon={shown}", if queued { "" } else { "un" }),
            );
            info!("PROBE_STONE: demoting to level {LEVEL_BELOW} for the refusal leg");
            let _ = net.0.send(ClientCommand::Chat {
                kind: ChatKind::Say,
                target: None,
                text: format!(".character level {LEVEL_BELOW}"),
            });
            probe.phase = Phase::WaitLevel {
                want: LEVEL_BELOW,
                since: now,
                join: false,
            };
        }
        Phase::WaitLevel { want, since, join } => {
            if level == want {
                let guid = probe.stone.unwrap_or_default();
                uses.write(MeetingStoneUse { go_guid: guid });
                if join {
                    info!("PROBE_STONE: level {level} — clicking INSIDE the band");
                    probe.phase = Phase::WaitQueue { since: now };
                } else {
                    info!(
                        "PROBE_STONE: level {level} — clicking BELOW the band; nothing may go out"
                    );
                    probe.phase = Phase::Refused { since: now };
                }
            } else if now - since > LEVEL_TIMEOUT_SECS {
                error!(
                    "PROBE_STONE: FAIL — `.character level {want}` never took (still {level}) in \
                     {LEVEL_TIMEOUT_SECS}s. Check the account's GM level: the command needs \
                     SEC_DEVELOPER 5 and a probe account is 6"
                );
                probe.fails += 1;
                probe.phase = Phase::Done;
            }
        }
        Phase::Refused { since } => {
            if now - since < REFUSAL_WINDOW_SECS {
                return;
            }
            let (queued, _, shown) = vm_state(&script);
            probe.fails += check(
                "REFUSED",
                "area=0 unqueued icon=false",
                &format!(
                    "area={} {}queued icon={shown}",
                    queue.area,
                    if queued { "" } else { "un" }
                ),
            );
            // `.character level 0` is not a level; a template that ships `data[0] = 0` means
            // "no floor", which for this leg is simply level 1.
            let want = probe.tmpl.map_or(1, |t| t.min_level.max(1));
            info!("PROBE_STONE: levelling to {want} for the join leg");
            let _ = net.0.send(ClientCommand::Chat {
                kind: ChatKind::Say,
                target: None,
                text: format!(".character level {want}"),
            });
            probe.phase = Phase::WaitLevel {
                want,
                since: now,
                join: true,
            };
        }
        Phase::WaitQueue { since } => {
            if queue.area != 0 {
                probe.joined_area = queue.area;
                probe.phase = Phase::Joined;
            } else if now - since > QUEUE_TIMEOUT_SECS {
                error!(
                    "PROBE_STONE: FAIL — no SMSG 0x295 within {QUEUE_TIMEOUT_SECS}s of the join. \
                     Either CMSG 0x292 never left, or the server refused it (it re-checks leader / \
                     raid / full and interact distance, never the level band)"
                );
                probe.fails += 1;
                probe.phase = Phase::Done;
            }
        }
        Phase::Joined => {
            let (queued, text, shown) = vm_state(&script);
            probe.status_text = text.clone();
            probe.icon_shown = shown;
            let area = probe.tmpl.map_or(0, |t| t.area);
            probe.fails += check(
                "JOINED",
                &format!("area={area} queued icon=true text=yes"),
                &format!(
                    "area={} {}queued icon={shown} text={}",
                    probe.joined_area,
                    if queued { "" } else { "un" },
                    if text.is_some() { "yes" } else { "no" }
                ),
            );
            // Leg 4 — `0x5f6990` over the LIVE queue: this stone is now the one we are queued at,
            // so its highlightable slot must refuse it.
            probe.darkened = meeting_stone_queued(Some(area), queue.area);
            probe.fails += check("DARKENED", "true", &probe.darkened.to_string());
            info!("PROBE_STONE: leaving through the live VM's own CancelMeetingStoneRequest()");
            let _ = script.eval::<()>("CancelMeetingStoneRequest()");
            probe.phase = Phase::WaitLeft { since: now };
        }
        Phase::WaitLeft { since } => {
            if queue.area == 0 {
                let (queued, _, shown) = vm_state(&script);
                probe.fails += check(
                    "LEFT",
                    "area=0 unqueued icon=false",
                    &format!(
                        "area={} {}queued icon={shown}",
                        queue.area,
                        if queued { "" } else { "un" }
                    ),
                );
                probe.phase = Phase::Done;
            } else if now - since > QUEUE_TIMEOUT_SECS {
                error!(
                    "PROBE_STONE: FAIL — still queued at {} {QUEUE_TIMEOUT_SECS}s after \
                     CancelMeetingStoneRequest()",
                    queue.area
                );
                probe.fails += 1;
                probe.phase = Phase::Done;
            }
        }
        Phase::Done => {
            if probe.exited {
                return;
            }
            probe.exited = true;
            info!(
                "PROBE_STONE: DONE stone={:#x} band={:?} area={} icon={} darkened={} text={:?} fail={}",
                probe.stone.unwrap_or_default(),
                probe.tmpl.map(|t| (t.min_level, t.max_level)),
                probe.joined_area,
                probe.icon_shown,
                probe.darkened,
                probe.status_text,
                probe.fails,
            );
            exit.write(AppExit::Success);
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(5));
                warn!("PROBE_STONE: still alive 5s after AppExit — hard exit");
                std::process::exit(0);
            });
        }
    }
}
