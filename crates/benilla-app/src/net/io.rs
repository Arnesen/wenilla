//! The background networking threads — the socket half of the net bridge.
//!
//! [`spawn_net`] starts a read thread with **two park points** (decisions 0193 + 0539): it first
//! parks **pre-logon** on the credentials channel — the login screen's pause — then, once a
//! [`LoginRequest`] walks logon → realm → world handshake (emitting [`SessionEvent::LoginStage`]s,
//! and [`SessionEvent::LoginFailed`] + re-park on any pre-roster failure), it **parks at character
//! select**: it emits the roster as a [`SessionEvent::CharacterList`] and blocks until the app
//! answers with a guid over the pick channel. The pick sends `CMSG_PLAYER_LOGIN`, and the thread
//! streams decoded [`SessionEvent`]s from there. All *policy* (credential auto-resubmit and its
//! pacing, the env fast path, auto-relogin on reconnect, the director's click) is app-side
//! ([`crate::login`], [`crate::char_select`]); this thread is a pure sequencer — it never sleeps.
//!
//! On a stream failure it emits a [`SessionEvent::Disconnected`] carrying
//! [`SessionEnd::Lost`] and returns to the login park; a clean in-game logout
//! ([`SessionEvent::LoggedOut`]) does the same with [`SessionEnd::LoggedOut`]. What happens next is
//! the app's, and the two answers differ (decision 1262): the logout relists, while a loss ends the
//! session at the account screen unless nobody is there to type. A single long-lived sibling write thread drains
//! [`ClientCommand`](super::ClientCommand)s down to the server; each successful connection hands it
//! the fresh [`WorldWriter`] over a swap channel, so "exactly one writer" is structural, not
//! signalled. The ECS half (draining the events into components each frame, and tearing the
//! streamed world down on disconnect) lives in the parent module; the halves communicate only
//! through the channels, so they split cleanly.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use benilla_protocol::{
    host_port, messages, AuthReject, CharAction, LoginStage, Poll, SessionEnd, SessionEvent,
    WardenRequired, WorldSession, WorldWriter, WORLD_PORT,
};
use crossbeam_channel::{Receiver, Sender};

use super::{CharRequest, ChatKind, ClientCommand};

/// **The inbound census** — every packet the read thread pulls off the world socket, whatever its
/// opcode, and the wall-clock of the most recent one (unix ms). Two numbers, one job: telling a
/// *silent server* apart from a *dead socket* (decision 0621).
///
/// When a remote mover freezes into a dead-reckoned runaway, the client cannot otherwise say which
/// of those it is looking at — and they are opposite bugs. If these keep climbing while the mover
/// starves, the connection is healthy and the server simply stopped relaying that unit; if they
/// freeze with it, the whole stream died without anyone noticing. Read by the runaway watch, which
/// prints them on every line ([`crate::net::motion`]).
pub(crate) static INBOUND_PACKETS: AtomicU64 = AtomicU64::new(0);
static LAST_INBOUND_UNIX_MS: AtomicU64 = AtomicU64::new(0);

/// Count one packet off the wire. Called for every `poll()` return — parsed or skipped, since a
/// packet we could not decode still proves the socket is alive.
fn note_inbound() {
    INBOUND_PACKETS.fetch_add(1, Ordering::Relaxed);
    LAST_INBOUND_UNIX_MS.store(unix_ms(), Ordering::Relaxed);
}

/// Wall-clock unix milliseconds — the one clock two processes share (the trace header's `t0` is in
/// the same units, so a mover's file and an observer's file line up).
fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// `(packets seen, ms since the last one)` — `None` for the age until a first packet has landed.
pub(crate) fn inbound_census() -> (u64, Option<u64>) {
    let last = LAST_INBOUND_UNIX_MS.load(Ordering::Relaxed);
    (
        INBOUND_PACKETS.load(Ordering::Relaxed),
        (last != 0).then(|| unix_ms().saturating_sub(last)),
    )
}

/// One submitted login attempt (decision 0539): the credentials, plus the abandon generation at
/// submit time — the thread discards the attempt at its next stage boundary if the shared counter
/// has moved (a Cancel bumps it). A counter, not a flag: a flag cleared by the *next* submit would
/// un-cancel the attempt still in flight.
pub(crate) struct LoginRequest {
    pub(crate) user: String,
    pub(crate) pass: String,
    pub(crate) generation: u64,
}

/// The keepalive cadence — the real client's 30 000 ms ping timer (VERIFIED wow-re net W1,
/// `0x537ff0`: the connection drain arms the next ping 30 s out). vmangos *kicks* a player socket
/// whose pings repeat faster than 27 s apart more than twice (`WorldSocket::_HandlePing`), so this
/// must never shrink below that.
const PING_INTERVAL: Duration = Duration::from_secs(30);

/// The ping clock — shared between the write thread (which stamps each `CMSG_PING`) and the app's
/// event drain (which matches the `SMSG_PONG` echo against it to measure the round trip). One
/// mutex, touched every 30 s by the writer and once per pong by the drain.
#[derive(Default)]
pub(crate) struct PingClock {
    /// Sequence of the most recent ping sent (the real client's ++counter; reset per connection).
    pub(crate) sequence: u32,
    /// When that ping went out — `None` until the first ping of a connection.
    pub(crate) sent_at: Option<Instant>,
    /// The last measured round trip (ms): echoed in the next ping's lastRtt field (what the real
    /// client reports) and surfaced as the debug panel's latency readout.
    pub(crate) last_rtt_ms: Option<u32>,
}

/// Cap on consecutive "command dropped/failed" warns per connection epoch, so a movement stream
/// during an outage can't flood the log. Reset when a fresh writer arrives.
const SEND_WARN_CAP: u32 = 8;

/// Connection parameters, from env (`WOW_HOST` as `host[:port]`, `WOW_CHAR`). Credentials are NOT here anymore
/// (decision 0539): they arrive per-attempt over the login channel — the app's policy owns the
/// env fast path and its `one`/`pone` defaults.
pub(super) struct NetConfig {
    host: String,
    /// `WOW_CHAR`, when explicitly set. Here it only names the create-if-empty character on a fresh
    /// account; as a *pick* it is app-side policy (`crate::char_select` auto-answers the roster with
    /// it — the dev fast path past the select screen).
    character: Option<String>,
}

impl NetConfig {
    pub(super) fn from_env() -> Self {
        NetConfig {
            host: std::env::var("WOW_HOST").unwrap_or_else(|_| "localhost".into()),
            character: std::env::var("WOW_CHAR").ok(),
        }
    }
}

/// How one connection cycle ended (the `Err` case — a stream failure — rides `Result` instead).
enum Cycle {
    /// The app dropped a channel end (exit) — end the read thread.
    Exit,
    /// Back to the pre-logon park with nothing to announce: a pre-roster failure (its
    /// [`SessionEvent::LoginFailed`] already went out), a canceled/superseded attempt, or a
    /// select-screen Back ([`CharRequest::Abandon`]).
    Repark,
    /// A clean in-game logout: emit the teardown `Disconnected` (decision 0065's path), then park.
    /// The app's pending credentials re-establish the roster silently (decision 0539 §3).
    LoggedOut,
}

/// Everything [`spawn_net`] hands the app: the inbound event stream, the outbound command sender,
/// the two park-answer channels (credentials + character pick), the shared abandon generation the
/// Cancel button bumps, and the ping clock.
pub(super) struct NetHandles {
    pub(super) events: Receiver<SessionEvent>,
    pub(super) commands: Sender<ClientCommand>,
    pub(super) pick: Sender<CharRequest>,
    pub(super) login: Sender<LoginRequest>,
    pub(super) login_abandon: Arc<AtomicU64>,
    pub(super) ping: Arc<Mutex<PingClock>>,
}

/// Spawn the background read thread (with its park/cycle loop) and the single long-lived write
/// thread. Each cycle failure emits a [`SessionEvent`] and returns to the pre-logon park; the app
/// keeps rendering regardless, and its policy decides what (if anything) answers the park.
pub(super) fn spawn_net(cfg: NetConfig, connect: bool) -> NetHandles {
    let (events_tx, events_rx) = crossbeam_channel::unbounded();
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let (pick_tx, pick_rx) = crossbeam_channel::unbounded::<CharRequest>();
    let (login_tx, login_rx) = crossbeam_channel::unbounded::<LoginRequest>();
    let login_abandon = Arc::new(AtomicU64::new(0));
    let ping_clock = Arc::new(Mutex::new(PingClock::default()));
    if connect {
        // The writer thread outlives connections; the read thread hands it each new WorldWriter.
        let (writer_tx, writer_rx) = crossbeam_channel::unbounded::<WorldWriter>();
        let clock = Arc::clone(&ping_clock);
        let abandon = Arc::clone(&login_abandon);
        thread::Builder::new()
            .name("wow-net-write".into())
            .spawn(move || {
                // Latency-sensitive: movement packets queue here (thread QoS, decision 0609).
                benilla_world::thread_qos::promote_current_thread(
                    benilla_world::thread_qos::QosClass::UserInitiated,
                );
                writer_loop(&cmd_rx, writer_rx, &clock)
            })
            .expect("spawn wow-net-write thread");
        thread::Builder::new()
            .name("wow-net".into())
            .spawn(move || {
                benilla_world::thread_qos::promote_current_thread(
                    benilla_world::thread_qos::QosClass::UserInitiated,
                );
                loop {
                    match run(&cfg, &events_tx, &writer_tx, &pick_rx, &login_rx, &abandon) {
                        Ok(Cycle::Exit) => return,
                        Ok(Cycle::Repark) => {}
                        Ok(Cycle::LoggedOut) => {
                            // Clean logout: the Disconnected tears the streamed world down app-side
                            // (decision 0065's path); the app's pending credentials re-park us live.
                            if events_tx
                                .send(SessionEvent::Disconnected {
                                    reason: "logged out".into(),
                                    end: SessionEnd::LoggedOut,
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(e) => {
                            // A live-stream failure — including a displacement kick, which reaches
                            // us as a bare EOF and nothing else (decision 1262). No sleep: what
                            // happens next is the app's policy, not this thread's (0539 §3).
                            bevy::log::error!("net: {e:#}");
                            if events_tx
                                .send(SessionEvent::Disconnected {
                                    reason: format!("disconnected: {e:#}"),
                                    end: SessionEnd::Lost,
                                })
                                .is_err()
                            {
                                return; // app exited mid-failure
                            }
                        }
                    }
                }
            })
            .expect("spawn wow-net thread");
    }
    // When not connecting (capture mode), the receivers/`events_tx` drop here: outbound sends
    // become harmless `Err`s (every call site ignores them) and the event stream stays empty forever.
    NetHandles {
        events: events_rx,
        commands: cmd_tx,
        pick: pick_tx,
        login: login_tx,
        login_abandon,
        ping: ping_clock,
    }
}

/// One connection cycle: **park pre-logon** for credentials (decision 0539) → logon → world
/// handshake → the character roster → **park at character select** until the app picks → enter the
/// world, hand the writer to the write thread, then stream decoded [`SessionEvent`]s until the
/// socket dies (`Err`), the character logs out ([`Cycle::LoggedOut`]), or the app drops a channel
/// end ([`Cycle::Exit`]). Every pre-roster failure emits [`SessionEvent::LoginFailed`] and
/// re-parks ([`Cycle::Repark`]) — never a retry loop; resubmission is the app's policy.
fn run(
    cfg: &NetConfig,
    events_tx: &Sender<SessionEvent>,
    writer_tx: &Sender<WorldWriter>,
    pick_rx: &Receiver<CharRequest>,
    login_rx: &Receiver<LoginRequest>,
    abandon: &AtomicU64,
) -> Result<Cycle> {
    // ── The pre-logon park (decision 0539): block for credentials. ──────────────────────────────
    let Ok(req) = login_rx.recv() else {
        return Ok(Cycle::Exit);
    };
    // The attempt is live until the abandon generation moves past its submit-time value (Cancel
    // bumps it; the next submit carries the bumped value). Checked at every stage boundary — a
    // blocking dial can't be interrupted, but its result is discarded silently.
    let canceled = || abandon.load(Ordering::SeqCst) != req.generation;
    let stage = |s: LoginStage| {
        let _ = events_tx.send(SessionEvent::LoginStage { stage: s });
    };
    let fail = |code: Option<u8>, reason: String| {
        let _ = events_tx.send(SessionEvent::LoginFailed {
            code,
            reason,
            terminal: false,
        });
        Ok(Cycle::Repark)
    };
    // A failure resubmitting cannot fix — the app shows it and stops (no paced retry).
    let fail_terminal = |reason: String| {
        let _ = events_tx.send(SessionEvent::LoginFailed {
            code: None,
            reason,
            terminal: true,
        });
        Ok(Cycle::Repark)
    };

    // Logon (the dial + SRP6 exchange — one blocking sequence against realmd).
    stage(LoginStage::Connecting);
    let logon = match benilla_protocol::logon(&cfg.host, &req.user, &req.pass) {
        Ok(l) => l,
        Err(e) => {
            if canceled() {
                return Ok(Cycle::Repark);
            }
            // A server refusal carries its auth result byte (the app maps it to the client's
            // own AUTH_* string); a transport failure carries None.
            let code = e.downcast_ref::<AuthReject>().map(|r| r.code);
            return fail(code, format!("{e:#}"));
        }
    };
    if canceled() {
        return Ok(Cycle::Repark);
    }
    // The realm we connect to (`realms.first()`, decision 0193) — carried on each roster emit so
    // the select screen can banner its name + type (decision 0465).
    let realm = logon.realms.first().cloned();
    let world_addr = realm
        .as_ref()
        .map(|r| r.address.clone())
        // Strip any explicit auth `:port` off `WOW_HOST` — the fallback world port is its own.
        .unwrap_or_else(|| format!("{}:{}", host_port(&cfg.host, WORLD_PORT).0, WORLD_PORT));

    stage(LoginStage::Handshaking);
    let mut session = match WorldSession::connect(&world_addr, &req.user, logon.session_key) {
        Ok(s) => s,
        Err(e) => {
            if canceled() {
                return Ok(Cycle::Repark);
            }
            // A Warden refusal is the server's own answer, not a transport fault — say it plainly
            // rather than wrapping it in handshake noise.
            if let Some(w) = e.downcast_ref::<WardenRequired>() {
                return fail_terminal(w.to_string());
            }
            return fail(None, format!("world handshake with {world_addr}: {e:#}"));
        }
    };

    // The roster (creating a starter character on a fresh account so PLAYER_LOGIN has a target).
    // Failures here are still pre-roster: surface as LoginFailed, re-park. (An immediately-run
    // closure, so the `?`-shaped sequence borrows `session` only for the call.)
    let mut characters = match (|| -> Result<Vec<benilla_protocol::Character>> {
        let mut characters = session.char_enum()?;
        if characters.is_empty() {
            let name = cfg.character.as_deref().unwrap_or("One");
            let starter = messages::CharCreateReq {
                name: name.to_string(),
                race: messages::RACE_HUMAN,
                class: messages::CLASS_WARRIOR,
                gender: messages::GENDER_MALE,
                skin: 0,
                face: 0,
                hair_style: 0,
                hair_color: 0,
                facial_hair: 0,
            };
            match session.create_character(&starter)? {
                messages::CHAR_CREATE_SUCCESS | messages::CHAR_CREATE_NAME_IN_USE => {}
                other => bail!("character creation failed: result {other:#x}"),
            }
            characters = session.char_enum()?;
        }
        Ok(characters)
    })() {
        Ok(c) => c,
        Err(e) => {
            if canceled() {
                return Ok(Cycle::Repark);
            }
            if let Some(w) = e.downcast_ref::<WardenRequired>() {
                return fail_terminal(w.to_string());
            }
            return fail(None, format!("character roster: {e:#}"));
        }
    };
    if canceled() {
        return Ok(Cycle::Repark);
    }
    // A pick queued during a dead cycle must not answer THIS roster — the app re-sends what it
    // still wants (its pending pick), and a deliberate logout must land on the list, not bounce
    // straight back into the world off a stale pick.
    while pick_rx.try_recv().is_ok() {}
    if events_tx
        .send(SessionEvent::CharacterList {
            characters: characters.clone(),
            realm: realm.clone(),
        })
        .is_err()
    {
        return Ok(Cycle::Exit);
    }

    // Park at character select until the app answers (its pick policy: auto-relogin on reconnect,
    // the WOW_CHAR fast path, or the director's click). Create/delete requests are serviced *in
    // place* (decision 0423): send, read the one result byte, on success re-enum + re-emit the
    // roster, emit the result, and loop back to the park — the thread stays a policy-free blocking
    // sequencer. The channel only closes on app exit. If the server kicked the parked socket
    // meanwhile, the login below fails → the caller cycles → a fresh roster → the app auto-resends
    // its pick — self-healing, no keep-alive needed (decision 0193).
    let guid = loop {
        let Ok(req) = pick_rx.recv() else {
            return Ok(Cycle::Exit);
        };
        let (action, code) = match req {
            CharRequest::Enter(guid) => break guid,
            // Select's Back (decision 0539): drop the parked session, return to the login park.
            CharRequest::Abandon => return Ok(Cycle::Repark),
            CharRequest::Create(create) => (CharAction::Create, session.create_character(&create)?),
            CharRequest::Delete(target) => (CharAction::Delete, session.delete_character(target)?),
        };
        // Success (create's SUCCESS or delete's SUCCESS) changed the roster — re-enumerate and
        // re-emit it *before* the result, so the screen already has the fresh list when it reacts.
        let changed = match action {
            CharAction::Create => code == messages::CHAR_CREATE_SUCCESS,
            CharAction::Delete => code == messages::CHAR_DELETE_SUCCESS,
        };
        if changed {
            characters = session.char_enum()?;
            if events_tx
                .send(SessionEvent::CharacterList {
                    characters: characters.clone(),
                    realm: realm.clone(),
                })
                .is_err()
            {
                return Ok(Cycle::Exit);
            }
        }
        if events_tx
            .send(SessionEvent::CharActionResult { action, code })
            .is_err()
        {
            return Ok(Cycle::Exit);
        }
    };
    let name = characters
        .iter()
        .find(|c| c.guid == guid)
        .map(|c| c.name.clone())
        .unwrap_or_default();
    session.player_login(guid)?;
    session.set_active_mover(guid)?;

    let (mut reader, writer) = session.into_split()?;
    if events_tx
        .send(SessionEvent::Connected {
            self_guid: guid,
            name,
        })
        .is_err()
    {
        return Ok(Cycle::Exit);
    }
    if writer_tx.send(writer).is_err() {
        // The writer thread only ends when the app drops every command sender — app exit.
        return Ok(Cycle::Exit);
    }
    bevy::log::info!("net: connected to {world_addr}");

    // Blocking read loop. `poll` skips packets the message layer can't parse (keeping the stream
    // aligned); a long run of consecutive skips means the stream desynced — guard against a busy spin.
    let (mut skip_run, mut skip_logged) = (0u32, 0u32);
    loop {
        let polled = reader.poll()?;
        note_inbound(); // one packet off the wire, parsed or not — the census counts liveness
        match polled {
            Poll::Events { opcode, events } => {
                skip_run = 0;
                // The full inbound opcode stream (tag `in`, decision 0624) — the last place a
                // packet could hide. `skip` covers what failed to parse and `rly` covers what
                // reached the mover replay; between them sits the packet that parsed into *no*
                // event, which no instrument could see. With this line every packet off the wire
                // is accounted for by name, so "the server stopped relaying" and "we dropped it on
                // the floor" are finally different pictures instead of the same silence.
                if benilla_assets::trace::enabled() {
                    benilla_assets::trace::line(
                        "in",
                        &format!(
                            "{opcode:#06x} {} ev={}",
                            benilla_protocol::messages::opcode_name(opcode).unwrap_or("?"),
                            events.len()
                        ),
                    );
                }
                for ev in events {
                    // A confirmed logout ends the cycle *after* the app hears about it.
                    let logged_out = matches!(ev, SessionEvent::LoggedOut);
                    // Receiver dropped → the app exited; end the thread cleanly.
                    if events_tx.send(ev).is_err() {
                        return Ok(Cycle::Exit);
                    }
                    if logged_out {
                        return Ok(Cycle::LoggedOut);
                    }
                }
            }
            Poll::Skipped { opcode, reason } => {
                skip_run += 1;
                // **Every** skip, uncapped, into the trace (tag `skip`, decision 0623). A packet that
                // arrives and fails to parse is indistinguishable, from outside, from one that never
                // arrived: the inbound census counts it either way, and no `rly` line is emitted
                // either way. That ambiguity is what made a starving remote mover unattributable —
                // so the skips get their own line, with the opcode that died.
                if benilla_assets::trace::enabled() {
                    benilla_assets::trace::line("skip", &format!("opcode={opcode:#06x} {reason}"));
                }
                // Log which packet we dropped (opcode + message name), capped so it can't itself
                // flood — enough to capture a post-teleport burst of unparseable object updates.
                if skip_logged < 40 {
                    bevy::log::warn!("net: skipping unparseable packet — {reason}");
                    skip_logged += 1;
                }
                // Feed the app's dropped-packet tally (the debug panel instrument) — a parse
                // *error* is a coverage gap the same as an unknown opcode, just a worse one.
                if events_tx
                    .send(SessionEvent::PacketDropped {
                        opcode,
                        unparseable: true,
                    })
                    .is_err()
                {
                    return Ok(Cycle::Exit);
                }
                if skip_run > 1024 {
                    return Err(anyhow!("world stream desynced after 1024 skipped packets"));
                }
            }
        }
    }
}

/// The single write thread: `select!` between app commands, writer swaps from the read thread, and
/// the 30 s keepalive tick ([`PING_INTERVAL`] — the real client's ping cadence). While disconnected
/// (no writer yet, or the socket died under the current one), commands drop with a capped warn and
/// the tick no-ops — they are meaningless without a live session, and the server re-syncs our state
/// from the reconnect handshake anyway. Ends when the app drops every command sender.
fn writer_loop(
    cmd_rx: &Receiver<ClientCommand>,
    mut writer_rx: Receiver<WorldWriter>,
    ping_clock: &Mutex<PingClock>,
) {
    let mut writer: Option<WorldWriter> = None;
    let mut warned = 0u32;
    let ping_tick = crossbeam_channel::tick(PING_INTERVAL);
    loop {
        crossbeam_channel::select! {
            recv(writer_rx) -> w => match w {
                Ok(w) => {
                    writer = Some(w);
                    warned = 0;
                    // A fresh connection restarts the keepalive from scratch, like the real
                    // client: sequence 1 is the new socket's first ping, and a stale in-flight
                    // pong from the old socket can no longer match.
                    *ping_clock.lock().expect("ping clock") = PingClock::default();
                }
                // The read thread ended (app exit). Stop selecting the dead channel (a
                // disconnected receiver is always-ready — it would busy-spin the select);
                // drain commands until the app side closes too.
                Err(_) => writer_rx = crossbeam_channel::never(),
            },
            recv(ping_tick) -> _ => {
                // The keepalive (30 s, the verified real-client cadence). Sent only while a live
                // writer exists — the parked char-select socket goes without (decision 0193's
                // self-healing covers it), and vmangos would kick a faster cadence as overspeed.
                if let Some(w) = writer.as_mut() {
                    let (sequence, last_rtt) = {
                        let mut c = ping_clock.lock().expect("ping clock");
                        c.sequence += 1;
                        c.sent_at = Some(Instant::now());
                        (c.sequence, c.last_rtt_ms.unwrap_or(0))
                    };
                    if let Err(e) = w.ping(sequence, last_rtt) {
                        if warned < SEND_WARN_CAP {
                            bevy::log::warn!("net: ping send failed: {e:#}");
                            warned += 1;
                        }
                    }
                }
            },
            recv(cmd_rx) -> cmd => {
                let Ok(cmd) = cmd else { return }; // all app senders dropped → app exit
                let Some(w) = writer.as_mut() else {
                    // No live writer: the session is gone and this command evaporates. Traced
                    // unconditionally — this is the state in which a client keeps *deciding* to send
                    // movement (`snd` lines) that no one will ever receive (decision 0621).
                    if benilla_assets::trace::enabled() {
                        benilla_assets::trace::line("wire", "DROPPED — no live session");
                    }
                    if warned < SEND_WARN_CAP {
                        bevy::log::warn!("net: dropping command — not connected");
                        warned += 1;
                    }
                    continue;
                };
                let result = match cmd {
                    ClientCommand::Move {
                        kind,
                        flags,
                        pos,
                        orientation,
                        pitch,
                        fall_time,
                        jump,
                        transport,
                    } => w.send_movement(
                        kind.opcode(),
                        flags,
                        pos,
                        orientation,
                        pitch,
                        fall_time,
                        jump,
                        transport,
                    ),
                    ClientCommand::MoveSplineDone {
                        flags,
                        pos,
                        orientation,
                        spline_id,
                    } => w.move_spline_done(flags, pos, orientation, spline_id),
                    ClientCommand::ForceSpeedAck {
                        kind,
                        guid,
                        counter,
                        speed,
                        flags,
                        pos,
                        orientation,
                        pitch,
                        fall_time,
                        jump,
                        transport,
                    } => w.force_speed_change_ack(
                        kind,
                        guid,
                        counter,
                        speed,
                        flags,
                        pos,
                        orientation,
                        pitch,
                        fall_time,
                        jump,
                        transport,
                    ),
                    ClientCommand::SetActiveMover { guid } => w.set_active_mover(guid),
                    ClientCommand::NotActiveMover {
                        guid,
                        flags,
                        pos,
                        orientation,
                        fall_time,
                    } => w.move_not_active_mover(guid, flags, pos, orientation, fall_time),
                    ClientCommand::FarSight { engage } => w.far_sight(engage),
                    ClientCommand::TeleportAck { guid, counter } => w.teleport_ack(guid, counter),
                    ClientCommand::WorldportAck => w.worldport_ack(),
                    ClientCommand::SetSelection { guid } => w.set_selection(guid),
                    ClientCommand::CancelAutoRepeat => w.cancel_auto_repeat(),
                    ClientCommand::CancelCast { spell_id } => w.cancel_cast(spell_id),
                    ClientCommand::CancelChannelling { spell_id } => w.cancel_channelling(spell_id),
                    ClientCommand::Chat { kind, target, text } => match kind {
                        ChatKind::Say => w.send_chat(&text),
                        ChatKind::Yell => w.send_yell(&text),
                        ChatKind::Emote => w.send_emote_chat(&text),
                        ChatKind::Whisper => {
                            w.send_whisper(target.as_deref().unwrap_or_default(), &text)
                        }
                        ChatKind::Party => w.send_party(&text),
                        ChatKind::Raid => w.send_raid(&text),
                        ChatKind::RaidLeader => w.send_raid_leader(&text),
                        ChatKind::RaidWarning => w.send_raid_warning(&text),
                        ChatKind::Guild => w.send_guild(&text),
                        ChatKind::Officer => w.send_officer(&text),
                        ChatKind::Battleground => w.send_battleground(&text),
                        ChatKind::BattlegroundLeader => w.send_battleground_leader(&text),
                        ChatKind::Afk => w.send_afk(&text),
                        ChatKind::Dnd => w.send_dnd(&text),
                        ChatKind::Channel => {
                            w.send_channel(target.as_deref().unwrap_or_default(), &text)
                        }
                    },
                    // The addon lane (decision 1235). The distribution arrived as an enum and the
                    // map is TOTAL — no "unknown, guess SAY" arm exists, which is what the enum
                    // seam is for — so the whole arm is one call.
                    ClientCommand::AddonMessage { distribution, text } => {
                        w.send_addon_message(super::addon_wire_chat_type(distribution), &text)
                    }
                    ClientCommand::JoinChannel { name, password } => {
                        w.join_channel(&name, &password)
                    }
                    ClientCommand::LeaveChannel { name } => w.leave_channel(&name),
                    ClientCommand::ChannelList { name } => w.channel_list(&name),
                    ClientCommand::RandomRoll { min, max } => w.random_roll(min, max),
                    ClientCommand::PlayedTime => w.played_time(),
                    ClientCommand::NameQuery { guid } => w.name_query(guid),
                    ClientCommand::CreatureQuery { entry, guid } => w.creature_query(entry, guid),
                    ClientCommand::PetNameQuery { pet_number, guid } => {
                        w.pet_name_query(pet_number, guid)
                    }
                    ClientCommand::ItemQuery { entry, guid } => w.item_query(entry, guid),
                    ClientCommand::UseItem {
                        bag_index,
                        slot,
                        spell_index,
                        target,
                    } => w.use_item(bag_index, slot, spell_index, target),
                    ClientCommand::OpenItem { bag_index, slot } => w.open_item(bag_index, slot),
                    ClientCommand::AutoEquipItem { bag_index, slot } => {
                        w.auto_equip_item(bag_index, slot)
                    }
                    ClientCommand::SetAmmo { entry } => w.set_ammo(entry),
                    ClientCommand::SwapInvItem { src_slot, dst_slot } => {
                        w.swap_inv_item(src_slot, dst_slot)
                    }
                    ClientCommand::SwapItem {
                        dst_bag,
                        dst_slot,
                        src_bag,
                        src_slot,
                    } => w.swap_item(dst_bag, dst_slot, src_bag, src_slot),
                    ClientCommand::SplitItem {
                        src_bag,
                        src_slot,
                        dst_bag,
                        dst_slot,
                        count,
                    } => w.split_item(src_bag, src_slot, dst_bag, dst_slot, count),
                    ClientCommand::DestroyItem {
                        bag_index,
                        slot,
                        count,
                    } => w.destroy_item(bag_index, slot, count),
                    ClientCommand::CastSpell { spell_id, target } => w.cast_spell(spell_id, target),
                    ClientCommand::CastSpellAtDest { spell_id, dest } => {
                        w.cast_spell_at_dest(spell_id, dest)
                    }
                    ClientCommand::CancelAura { spell_id } => w.cancel_aura(spell_id),
                    ClientCommand::SetActionButton { button, packed } => {
                        w.set_action_button(button, packed)
                    }
                    ClientCommand::SetActionBarToggles { toggles } => {
                        w.set_actionbar_toggles(toggles)
                    }
                    ClientCommand::PetAction {
                        pet_guid,
                        packed,
                        target_guid,
                    } => w.pet_action(pet_guid, packed, target_guid),
                    ClientCommand::PetSetAction { pet_guid, entries } => {
                        w.pet_set_action(pet_guid, &entries)
                    }
                    ClientCommand::PetStopAttack { pet_guid } => w.pet_stop_attack(pet_guid),
                    ClientCommand::PetCancelAura { pet_guid, spell_id } => {
                        w.pet_cancel_aura(pet_guid, spell_id)
                    }
                    ClientCommand::PetSpellAutocast {
                        pet_guid,
                        spell_id,
                        enabled,
                    } => w.pet_spell_autocast(pet_guid, spell_id, enabled),
                    ClientCommand::PetAbandon { pet_guid } => w.pet_abandon(pet_guid),
                    ClientCommand::PetRename { pet_guid, name } => w.pet_rename(pet_guid, &name),
                    ClientCommand::AttackSwing { guid } => w.attack_swing(guid),
                    ClientCommand::AttackStop => w.attack_stop(),
                    ClientCommand::SetSheathed { state } => w.set_sheathed(state),
                    ClientCommand::StandStateChange { state } => w.stand_state_change(state),
                    ClientCommand::MountSpecial => w.mount_special(),
                    ClientCommand::TextEmote { text_id, target } => w.text_emote(text_id, target),
                    ClientCommand::GossipHello { guid } => w.gossip_hello(guid),
                    ClientCommand::GossipSelectOption { guid, option } => {
                        // v1 sends no code — coded options are greyed, never selected (decision 0081).
                        w.gossip_select_option(guid, option, None)
                    }
                    ClientCommand::NpcTextQuery { text_id, guid } => w.npc_text_query(text_id, guid),
                    ClientCommand::ListInventory { guid } => w.list_inventory(guid),
                    ClientCommand::BuyItem {
                        vendor,
                        entry,
                        count,
                    } => w.buy_item(vendor, entry, count),
                    ClientCommand::SellItem {
                        vendor,
                        item_guid,
                        count,
                    } => w.sell_item(vendor, item_guid, count),
                    ClientCommand::BuybackItem { vendor, slot } => w.buyback_item(vendor, slot),
                    ClientCommand::RepairItem { vendor, item_guid } => {
                        w.repair_item(vendor, item_guid)
                    }
                    ClientCommand::BinderActivate { binder } => w.binder_activate(binder),
                    ClientCommand::TalentWipeConfirm { trainer } => w.talent_wipe_confirm(trainer),
                    ClientCommand::BankerActivate { guid } => w.banker_activate(guid),
                    ClientCommand::BuyBankSlot { guid } => w.buy_bank_slot(guid),
                    ClientCommand::AutoBankItem { bag, slot } => w.autobank_item(bag, slot),
                    ClientCommand::AutoStoreBankItem { bag, slot } => {
                        w.autostore_bank_item(bag, slot)
                    }
                    ClientCommand::TrainerList { trainer } => w.trainer_list(trainer),
                    ClientCommand::TrainerBuySpell { trainer, spell_id } => {
                        w.trainer_buy_spell(trainer, spell_id)
                    }
                    ClientCommand::LearnTalent { talent_id, rank } => {
                        w.learn_talent(talent_id, rank)
                    }
                    ClientCommand::UnlearnSkill { skill_id } => w.unlearn_skill(skill_id),
                    ClientCommand::SetFactionAtWar {
                        rep_list_id,
                        at_war,
                    } => w.set_faction_at_war(rep_list_id, at_war),
                    ClientCommand::SetFactionInactive {
                        rep_list_id,
                        inactive,
                    } => w.set_faction_inactive(rep_list_id, inactive),
                    ClientCommand::SetWatchedFaction { rep_list_id } => {
                        w.set_watched_faction(rep_list_id)
                    }
                    ClientCommand::GameObjUse { guid } => w.gameobj_use(guid),
                    ClientCommand::AreaTrigger { trigger_id } => w.area_trigger(trigger_id),
                    ClientCommand::GameObjectQuery { entry, guid } => w.gameobject_query(entry, guid),
                    ClientCommand::PageTextQuery { page_id, guid } => {
                        w.page_text_query(page_id, guid)
                    }
                    ClientCommand::CastSpellGameObject { spell_id, go_guid } => {
                        w.cast_spell_gameobject(spell_id, go_guid)
                    }
                    ClientCommand::CastSpellItem {
                        spell_id,
                        item_guid,
                    } => w.cast_spell_item(spell_id, item_guid),
                    ClientCommand::Loot { guid } => w.loot(guid),
                    ClientCommand::AutostoreLootItem { slot } => w.autostore_loot_item(slot),
                    ClientCommand::LootMoney => w.loot_money(),
                    ClientCommand::LootRelease { guid } => w.loot_release(guid),
                    ClientCommand::LootRoll {
                        looted_target,
                        item_slot,
                        roll_type,
                    } => w.loot_roll(looted_target, item_slot, roll_type),
                    ClientCommand::QuestgiverQuery { npc, quest } => {
                        w.questgiver_query_quest(npc, quest)
                    }
                    ClientCommand::QuestgiverAccept { npc, quest } => {
                        w.questgiver_accept_quest(npc, quest)
                    }
                    ClientCommand::QuestgiverComplete { npc, quest } => {
                        w.questgiver_complete_quest(npc, quest)
                    }
                    ClientCommand::QuestgiverRequestReward { npc, quest } => {
                        w.questgiver_request_reward(npc, quest)
                    }
                    ClientCommand::QuestgiverChooseReward { npc, quest, choice } => {
                        w.questgiver_choose_reward(npc, quest, choice)
                    }
                    ClientCommand::QuestQuery { quest } => w.quest_query(quest),
                    ClientCommand::QuestgiverStatusQuery { npc } => w.questgiver_status_query(npc),
                    ClientCommand::QuestlogRemove { slot } => w.questlog_remove_quest(slot),
                    ClientCommand::GetMailList { mailbox } => w.get_mail_list(mailbox),
                    ClientCommand::SendMail {
                        mailbox,
                        receiver,
                        subject,
                        body,
                        item_guid,
                        money,
                        cod,
                    } => w.send_mail(
                        mailbox, &receiver, &subject, &body,
                        // stationery/package: vmangos discards both — player mail is always
                        // stored MAIL_STATIONERY_DEFAULT (41, decision 0544) regardless of what
                        // rides the wire here.
                        41, 0, item_guid, money, cod,
                    ),
                    ClientCommand::MailTakeMoney { mailbox, mail_id } => {
                        w.mail_take_money(mailbox, mail_id)
                    }
                    ClientCommand::MailTakeItem { mailbox, mail_id } => {
                        w.mail_take_item(mailbox, mail_id)
                    }
                    ClientCommand::MailMarkAsRead { mailbox, mail_id } => {
                        w.mail_mark_as_read(mailbox, mail_id)
                    }
                    ClientCommand::MailReturnToSender { mailbox, mail_id } => {
                        w.mail_return_to_sender(mailbox, mail_id)
                    }
                    ClientCommand::MailDelete { mailbox, mail_id } => {
                        w.mail_delete(mailbox, mail_id)
                    }
                    ClientCommand::MailCreateTextItem { mailbox, mail_id } => {
                        w.mail_create_text_item(mailbox, mail_id)
                    }
                    ClientCommand::ItemTextQuery { text_id, mail_id } => {
                        w.item_text_query(text_id, mail_id)
                    }
                    ClientCommand::QueryNextMailTime => w.query_next_mail_time(),
                    // The auction house arc (decision 1511 P0) — the CMSG verbs onto the
                    // P0 writers; the auctioneer guid rides on every one.
                    ClientCommand::AuctionHello { auctioneer } => w.auction_hello(auctioneer),
                    ClientCommand::AuctionListItems {
                        auctioneer,
                        list_from,
                        searched_name,
                        level_min,
                        level_max,
                        slot_id,
                        main_category,
                        sub_category,
                        quality,
                        usable,
                    } => w.auction_list_items(
                        auctioneer,
                        list_from,
                        &searched_name,
                        level_min,
                        level_max,
                        slot_id,
                        main_category,
                        sub_category,
                        quality,
                        usable,
                    ),
                    ClientCommand::AuctionListOwnerItems {
                        auctioneer,
                        list_from,
                    } => w.auction_list_owner_items(auctioneer, list_from),
                    ClientCommand::AuctionListBidderItems {
                        auctioneer,
                        list_from,
                        auction_ids,
                    } => w.auction_list_bidder_items(auctioneer, list_from, &auction_ids),
                    ClientCommand::AuctionSellItem {
                        auctioneer,
                        item_guid,
                        bid,
                        buyout,
                        etime_minutes,
                    } => w.auction_sell_item(auctioneer, item_guid, bid, buyout, etime_minutes),
                    ClientCommand::AuctionPlaceBid {
                        auctioneer,
                        auction_id,
                        price,
                    } => w.auction_place_bid(auctioneer, auction_id, price),
                    ClientCommand::AuctionRemoveItem {
                        auctioneer,
                        auction_id,
                    } => w.auction_remove_item(auctioneer, auction_id),
                    ClientCommand::QueryTime => w.query_time(),
                    // The inspect request (decision 0631) — no reply is awaited; see the writer.
                    ClientCommand::Inspect { target } => w.inspect(target),
                    // The inspect-honor query (decision 1512) — this one IS answered; the reply
                    // rides the same opcode back.
                    ClientCommand::InspectHonorStats { target } => w.inspect_honor_stats(target),
                    // The player-trade arc (decision 0592) — the CMSG verbs onto the P0 writers.
                    ClientCommand::InitiateTrade { target } => w.initiate_trade(target),
                    ClientCommand::BeginTrade => w.begin_trade(),
                    ClientCommand::AcceptTrade => w.accept_trade(),
                    ClientCommand::UnacceptTrade => w.unaccept_trade(),
                    ClientCommand::CancelTrade => w.cancel_trade(),
                    ClientCommand::SetTradeGold { copper } => w.set_trade_gold(copper),
                    ClientCommand::SetTradeItem {
                        trade_slot,
                        bag,
                        slot,
                    } => w.set_trade_item(trade_slot, bag, slot),
                    ClientCommand::ClearTradeItem { trade_slot } => w.clear_trade_item(trade_slot),
                    ClientCommand::Logout => w.logout_request(),
                    ClientCommand::LogoutCancel => w.logout_cancel(),
                    ClientCommand::CompleteCinematic => w.complete_cinematic(),
                    ClientCommand::MoveModeAck {
                        guid,
                        counter,
                        mode,
                        apply,
                        flags,
                        pos,
                        orientation,
                    } => w.move_mode_ack(guid, counter, mode, apply, flags, (pos, orientation)),
                    ClientCommand::RepopRequest => w.repop_request(),
                    ClientCommand::CorpseQuery => w.corpse_query(),
                    ClientCommand::ReclaimCorpse { corpse } => w.reclaim_corpse(corpse),
                    ClientCommand::SpiritHealerActivate { npc } => w.spirit_healer_activate(npc),
                    ClientCommand::ResurrectResponse { caster, accept } => {
                        w.resurrect_response(caster, accept)
                    }
                    ClientCommand::GroupInvite { name } => w.group_invite(&name),
                    ClientCommand::GroupAccept => w.group_accept(),
                    ClientCommand::GroupDecline => w.group_decline(),
                    ClientCommand::GroupUninvite { name } => w.group_uninvite(&name),
                    ClientCommand::GroupSetLeader { guid } => w.group_set_leader(guid),
                    ClientCommand::GroupLeave => w.group_disband(),
                    ClientCommand::GroupRaidConvert => w.group_raid_convert(),
                    ClientCommand::LootMethod {
                        method,
                        master,
                        threshold,
                    } => w.loot_method(method, master, threshold),
                    ClientCommand::SetRaidTarget { icon, guid } => w.raid_target_set(icon, guid),
                    ClientCommand::GroupChangeSubGroup { name, group } => {
                        w.group_change_sub_group(&name, group)
                    }
                    ClientCommand::GroupSwapSubGroup { name, other } => {
                        w.group_swap_sub_group(&name, &other)
                    }
                    ClientCommand::GroupAssistantLeader { guid, grant } => {
                        w.group_assistant_leader(guid, grant)
                    }
                    ClientCommand::ReadyCheckStart => w.ready_check_start(),
                    ClientCommand::ReadyCheckAnswer { ready } => w.ready_check_answer(ready),
                    ClientCommand::RequestRaidInfo => w.request_raid_info(),
                    ClientCommand::DuelAccepted { arbiter } => w.duel_accepted(arbiter),
                    ClientCommand::DuelCancelled { arbiter } => w.duel_cancelled(arbiter),
                    ClientCommand::TogglePvp => w.toggle_pvp(),
                    ClientCommand::ToggleHelm => w.toggle_helm(),
                    ClientCommand::ToggleCloak => w.toggle_cloak(),
                    ClientCommand::FriendListRequest => w.friend_list(),
                    ClientCommand::AddFriend { name } => w.add_friend(&name),
                    ClientCommand::DelFriend { guid } => w.del_friend(guid),
                    ClientCommand::AddIgnore { name } => w.add_ignore(&name),
                    ClientCommand::DelIgnore { guid } => w.del_ignore(guid),
                    ClientCommand::Who { request } => w.who(&request),
                    ClientCommand::ChatIgnored { guid } => w.chat_ignored(guid),
                    ClientCommand::GuildQuery { guild_id } => w.guild_query(guild_id),
                    ClientCommand::GuildCreate { name } => w.guild_create(&name),
                    ClientCommand::GuildInvite { name } => w.guild_invite(&name),
                    ClientCommand::GuildAccept => w.guild_accept(),
                    ClientCommand::GuildDecline => w.guild_decline(),
                    ClientCommand::GuildInfoRequest => w.guild_info(),
                    ClientCommand::GuildRosterRequest => w.guild_roster(),
                    ClientCommand::GuildPromote { name } => w.guild_promote(&name),
                    ClientCommand::GuildDemote { name } => w.guild_demote(&name),
                    ClientCommand::GuildLeave => w.guild_leave(),
                    ClientCommand::GuildRemove { name } => w.guild_remove(&name),
                    ClientCommand::GuildDisband => w.guild_disband(),
                    ClientCommand::GuildLeader { name } => w.guild_leader(&name),
                    ClientCommand::GuildMotd { motd } => w.guild_motd(&motd),
                    ClientCommand::GuildRank {
                        rank_id,
                        rights,
                        name,
                    } => w.guild_rank(rank_id, rights, &name),
                    ClientCommand::GuildAddRank { name } => w.guild_add_rank(&name),
                    ClientCommand::GuildDelRank => w.guild_del_rank(),
                    ClientCommand::GuildSetPublicNote { name, note } => {
                        w.guild_set_public_note(&name, &note)
                    }
                    ClientCommand::GuildSetOfficerNote { name, note } => {
                        w.guild_set_officer_note(&name, &note)
                    }
                    ClientCommand::GuildInfoText { text } => w.guild_info_text(&text),
                    ClientCommand::TaxiNodeStatusQuery { guid } => w.taxi_node_status_query(guid),
                    ClientCommand::TaxiQueryNodes { guid } => w.taxi_query_available_nodes(guid),
                    ClientCommand::ActivateTaxi {
                        guid,
                        source_node,
                        dest_node,
                    } => w.activate_taxi(guid, source_node, dest_node),
                    ClientCommand::ActivateTaxiExpress {
                        guid,
                        total_cost,
                        nodes,
                    } => w.activate_taxi_express(guid, total_cost, &nodes),
                };
                // **What actually reached the socket** (tag `wire`, decision 0621). The controller's
                // `snd` line is written before the command is even queued, so it records a decision,
                // not a transmission — a client whose session died goes on producing `snd` lines into
                // a dead channel forever, which is exactly the ambiguity that cost us a hunt. Only
                // failures are traced: a silent `wire` log beside a busy `snd` log means every packet
                // went out.
                if let Err(e) = result {
                    if benilla_assets::trace::enabled() {
                        benilla_assets::trace::line("wire", &format!("SEND FAILED: {e:#}"));
                    }
                    if warned < SEND_WARN_CAP {
                        bevy::log::warn!("net: send failed: {e:#}");
                        warned += 1;
                    }
                }
            },
        }
    }
}
