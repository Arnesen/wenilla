//! The background networking threads, the socket half of the net bridge.
//!
//! [`spawn_net`]'s read thread parks pre-logon for a [`LoginRequest`], walks logon, realm and
//! world handshake, then parks at character select until the app picks a guid. The pick sends
//! `CMSG_PLAYER_LOGIN` and announces the connection without waiting for the verdict, as the
//! reference's world is already loading when it sends the login (`0x46c272`), so the destination's
//! tiles stream a round trip early; a later `SMSG_CHARACTER_LOGIN_FAILED` ends the cycle like a
//! logout ([`Cycle::LoginRefused`]). A stream failure emits
//! [`SessionEvent::Disconnected`] with [`SessionEnd::Lost`] and re-parks. All policy lives in
//! [`crate::login`] and [`crate::char_select`]; this thread only sequences and never sleeps.
//!
//! One long-lived write thread drains [`ClientCommand`](super::ClientCommand)s; each connection
//! hands it the fresh [`WorldWriter`] over a swap channel, so there is only ever one writer.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(not(target_arch = "wasm32"))]
use std::thread;
use std::time::Duration;

// Not `bevy::platform::time::Instant`: a browser has no monotonic clock behind that name (it panics), and this
// one is the ping clock's stopwatch. `bevy::platform` is std's `Instant` natively and `performance
// .now()` in a page.
use bevy::platform::time::Instant;

use anyhow::{anyhow, Result};
use benilla_assets::LockRecover;
use benilla_protocol::{
    host_port, messages, AuthReject, CharAction, LoginStage, Poll, SessionEnd, SessionEvent,
    WardenRequired, WorldSession, WorldWriter, WORLD_PORT,
};
use crossbeam_channel::{Receiver, Sender};

use super::{CharRequest, ClientCommand, RealmRequest};

/// The inbound census: every packet off the world socket, and the unix ms of the latest, which
/// tell a silent server from a dead socket for the runaway watch ([`crate::net::motion`]).
pub(crate) static INBOUND_PACKETS: AtomicU64 = AtomicU64::new(0);
static LAST_INBOUND_UNIX_MS: AtomicU64 = AtomicU64::new(0);

/// Counts one packet, parsed or skipped: either proves the socket is alive.
fn note_inbound() {
    INBOUND_PACKETS.fetch_add(1, Ordering::Relaxed);
    LAST_INBOUND_UNIX_MS.store(unix_ms(), Ordering::Relaxed);
}

/// Wall-clock unix ms, the clock two processes' traces share (the trace header's `t0`).
fn unix_ms() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// `(packets seen, ms since the last one)`.
pub(crate) fn inbound_census() -> (u64, Option<u64>) {
    let last = LAST_INBOUND_UNIX_MS.load(Ordering::Relaxed);
    (
        INBOUND_PACKETS.load(Ordering::Relaxed),
        (last != 0).then(|| unix_ms().saturating_sub(last)),
    )
}

/// One login attempt and the abandon generation at submit time; the thread discards the attempt
/// at its next stage boundary once a Cancel has moved the counter. A counter, since a flag cleared
/// by the next submit would un-cancel the attempt in flight.
#[derive(Clone)]
pub(crate) struct LoginRequest {
    pub(crate) user: String,
    pub(crate) pass: String,
    /// The realmlist to dial, `host[:port]`, per attempt so an edit mid-dial cannot retarget the
    /// connection in flight.
    pub(crate) host: String,
    pub(crate) generation: u64,
}

/// **The three park-answer receivers**, in the order a cycle blocks on them: credentials, realm,
/// character. One struct rather than three parameters because they are one concern — the app's side
/// of every place this thread stops and waits — and because a cycle that grew a fourth park should
/// not have to re-thread every signature to say so.
///
/// All three are `async_channel`: they are the points the sequencer *waits* at, and a browser task
/// cannot block a thread to wait. The ECS side never waits on them — it `try_send`s into unbounded
/// queues — so nothing above this line changed shape.
pub(super) struct Parks {
    pub(super) login_rx: async_channel::Receiver<LoginRequest>,
    pub(super) realm_rx: async_channel::Receiver<RealmRequest>,
    pub(super) pick_rx: async_channel::Receiver<CharRequest>,
}

/// Which of the character park's two channels woke it — the `Fired`/[`Parked`] pair standing in
/// for `crossbeam_channel::select!`, which has no browser twin. `or` over two `recv` futures can
/// only answer with one value, so the two request types travel back under one enum and are matched
/// apart at the park.
enum Fired {
    Realm(Result<RealmRequest, async_channel::RecvError>),
    Pick(Result<CharRequest, async_channel::RecvError>),
}

/// How long a realm-list refresh waits for realmd; the park serving the list is deaf that long.
const REALM_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);

/// The keepalive cadence — the real client's 30 000 ms ping timer (`0x537ff0`: the connection
/// drain arms the next ping 30 s out). vmangos *kicks* a player socket
/// whose pings repeat faster than 27 s apart more than twice (`WorldSocket::_HandlePing`), so this
/// must never shrink below that.
pub(super) const PING_INTERVAL: Duration = Duration::from_secs(30);

/// Round trips [`PingClock`] keeps: 15. The reference's ring (`conn+0x1a6c`) has 16 slots, but
/// `read == write` means empty (`0x537fa8`) and `HandlePong` advances the read index (`0x537de8`),
/// so at most 15 are ever averaged.
const RTT_RING: usize = 15;

/// The connection's ping stats, the reference's per-connection block (`conn+0x1a6c` ring,
/// `+0x1aac/+0x1ab0` head and tail, `+0x1a64` send stamp, `+0x1a68` expected sequence) under one
/// lock, as `conn+0x1ac0` guards it for `HandlePong` (`0x537d60`) and `GetNetStats` (`0x537f20`).
///
/// The write thread stamps each `CMSG_PING`; the read thread times the `SMSG_PONG` the instant it
/// lands, as the reference's `OnData` (`0x537b10`) hands it to `HandlePong` inline, since timing
/// it in the frame drain would add a client frame to every reading; the app reads
/// [`Self::avg_latency_ms`].
#[derive(Default)]
pub(crate) struct PingClock {
    /// Sequence of the latest ping sent, counting from 1 per connection.
    pub(crate) sequence: u32,
    /// When that ping went out.
    pub(crate) sent_at: Option<Instant>,
    /// The last round trip (ms), sent as the next ping's `lastRtt` as the reference does.
    pub(crate) last_rtt_ms: Option<u32>,
    /// The latest [`RTT_RING`] round trips (ms), oldest first.
    rtt_ring: std::collections::VecDeque<u32>,
}

impl PingClock {
    /// `HandlePong`: times and files one `SMSG_PONG`; a mismatched sequence is dropped, as in
    /// the reference.
    pub(crate) fn record_pong(&mut self, sequence: u32) -> Option<u32> {
        let sent = self.sent_at.filter(|_| self.sequence == sequence)?;
        let rtt = sent.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
        self.file_rtt(rtt);
        Some(rtt)
    }

    /// The filing half of [`Self::record_pong`], apart so a test can give exact milliseconds.
    fn file_rtt(&mut self, rtt: u32) {
        self.last_rtt_ms = Some(rtt);
        if self.rtt_ring.len() == RTT_RING {
            self.rtt_ring.pop_front();
        }
        self.rtt_ring.push_back(rtt);
    }

    /// `GetNetStats`' latency: the unsigned truncating mean of the samples present (`0x537f20`,
    /// `div` at `0x537fce`); `None` when empty, which the UI shows as the reference's 0
    /// (`0x537fd2`).
    pub(crate) fn avg_latency_ms(&self) -> Option<u32> {
        if self.rtt_ring.is_empty() {
            return None;
        }
        let sum: u64 = self.rtt_ring.iter().map(|&ms| u64::from(ms)).sum();
        Some((sum / self.rtt_ring.len() as u64) as u32)
    }

    /// Forgets this connection's measurements.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Cap on consecutive "command dropped/failed" warns per connection epoch, so a movement stream
/// during an outage can't flood the log. Reset when a fresh writer arrives.
pub(super) const SEND_WARN_CAP: u32 = 8;

/// What one wake-up at the character park asked for — the `select!`'s answer, so that every jump
/// out of that park is made in one readable `match` rather than inside a macro's expansion.
enum Parked {
    /// `CMSG_PLAYER_LOGIN` with this guid.
    Play(u64),
    /// A create or delete was serviced in place; its result byte still has to go out.
    Acted(CharAction, u8),
    /// Stay parked: a realm-list refresh, or a Cancel over this screen.
    StayPut,
    /// Drop the parked world session and dial this realm.
    Realm(benilla_protocol::RealmInfo),
    /// Select's Back: return to the pre-logon park.
    Repark,
    /// The app dropped a channel end.
    Exit,
}

/// How one connection cycle ended; a stream failure is the `Err` of `Result`.
enum Cycle {
    /// The app dropped a channel end: end the read thread.
    Exit,
    /// Back to the pre-logon park with nothing to announce: a pre-roster failure (already
    /// reported), a canceled attempt, or select's Back ([`CharRequest::Abandon`]).
    Repark,
    /// A clean logout: emit the teardown `Disconnected`, then park; the app's pending credentials
    /// fetch the roster again.
    LoggedOut,
    /// `SMSG_CHARACTER_LOGIN_FAILED`: the announced entry is taken back and the cycle ends like a
    /// logout. Deviation: the reference keeps its connection, but here the session is already
    /// split and the writer handed away, so the logout's reconnect and relist are reused.
    LoginRefused,
}

/// Everything [`spawn_net`] hands the app.
pub(super) struct NetHandles {
    pub(super) events: Receiver<SessionEvent>,
    pub(super) commands: Sender<ClientCommand>,
    // The three park channels are `async_channel` — see [`Parks`]. The ECS side never waits on
    // them (it `try_send`s into unbounded queues), so nothing above this line changed shape.
    pub(super) pick: async_channel::Sender<CharRequest>,
    pub(super) realm: async_channel::Sender<RealmRequest>,
    pub(super) login: async_channel::Sender<LoginRequest>,
    pub(super) login_abandon: Arc<AtomicU64>,
    pub(super) ping: Arc<Mutex<PingClock>>,
    /// The write half's two inputs — **web only**. A page has no threads, so what
    /// [`writer_loop`] owns natively is owned instead by `crate::net`'s per-frame pump, and these
    /// are how it gets hold of them.
    #[cfg(target_arch = "wasm32")]
    pub(super) writer_rx: Receiver<WorldWriter>,
    #[cfg(target_arch = "wasm32")]
    pub(super) commands_rx: Receiver<ClientCommand>,
}

/// Spawns the read thread with its park and cycle loop, and the one long-lived write thread.
pub(super) fn spawn_net(connect: bool) -> NetHandles {
    let (events_tx, events_rx) = crossbeam_channel::unbounded();
    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
    let (pick_tx, pick_rx) = async_channel::unbounded::<CharRequest>();
    let (realm_tx, realm_rx) = async_channel::unbounded::<RealmRequest>();
    let (login_tx, login_rx) = async_channel::unbounded::<LoginRequest>();
    // The writer outlives connections; the sequencer hands it each new WorldWriter across here.
    let (writer_tx, writer_rx) = crossbeam_channel::unbounded::<WorldWriter>();
    let login_abandon = Arc::new(AtomicU64::new(0));
    let ping_clock = Arc::new(Mutex::new(PingClock::default()));
    if connect {
        let parks = Parks {
            login_rx,
            realm_rx,
            pick_rx,
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            let clock = Arc::clone(&ping_clock);
            // The sequencer's own handle: `SMSG_PONG` is measured where it lands, not where it is
            // drained (B346 — see [`PingClock`]).
            let read_clock = Arc::clone(&ping_clock);
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
                    // The same dedicated thread as ever, now turning the sequencer future over:
                    // every await inside it is a blocking socket read, so this is the old loop with
                    // an `.await` written where the call used to be.
                    futures_lite::future::block_on(sequencer(
                        events_tx, writer_tx, parks, abandon, read_clock,
                    ));
                })
                .expect("spawn wow-net thread");
        }
        #[cfg(target_arch = "wasm32")]
        {
            // No threads in a page: the sequencer is one task on the browser's own event loop, and
            // the writer is drained by a Bevy system instead of a sibling thread (`crate::net`).
            let abandon = Arc::clone(&login_abandon);
            let read_clock = Arc::clone(&ping_clock);
            wasm_bindgen_futures::spawn_local(sequencer(
                events_tx, writer_tx, parks, abandon, read_clock,
            ));
        }
    }
    // Not connecting (capture mode): the channel ends drop here, sends become ignored `Err`s and
    // the event stream stays empty.
    NetHandles {
        events: events_rx,
        commands: cmd_tx,
        pick: pick_tx,
        realm: realm_tx,
        login: login_tx,
        login_abandon,
        ping: ping_clock,
        #[cfg(target_arch = "wasm32")]
        writer_rx,
        #[cfg(target_arch = "wasm32")]
        commands_rx: cmd_rx,
    }
}

/// The sequencer's outer loop: run one connection cycle, say how it ended, park again. Shared by
/// both spawns — a thread under `block_on` natively, a `spawn_local` task on the web — so the two
/// targets cannot drift on what a lost connection or a clean logout means.
async fn sequencer(
    events_tx: Sender<SessionEvent>,
    writer_tx: Sender<WorldWriter>,
    parks: Parks,
    abandon: Arc<AtomicU64>,
    ping_clock: Arc<Mutex<PingClock>>,
) {
    let mut tails_announced = std::collections::HashSet::new();
    loop {
        // **A cycle starts with no measurements.** Every way the last one ended — a stream
        // failure, a logout, a re-park — lands here, so one clear covers them all, and it runs on
        // the task that owns the connection instead of racing in from the app's drain a frame
        // later. (`writer_loop` clears again when the fresh writer arrives, and still has to:
        // between the old socket dying and that handover the keepalive tick can still fire on the
        // stale writer.)
        ping_clock.lock_recover().clear();
        match run(
            &events_tx,
            &writer_tx,
            &parks,
            &abandon,
            &ping_clock,
            &mut tails_announced,
        )
        .await
        {
            Ok(Cycle::Exit) => return,
            Ok(Cycle::Repark) => {}
            Ok(end @ (Cycle::LoggedOut | Cycle::LoginRefused)) => {
                // Clean logout: the Disconnected tears the streamed world down app-side (decision
                // 0065's path); the app's pending credentials re-park us live. A refused login
                // rides the same edge — the world it tears down is the one the entry had only
                // started to build.
                let reason = match end {
                    Cycle::LoggedOut => "logged out",
                    _ => "character login refused",
                };
                if events_tx
                    .send(SessionEvent::Disconnected {
                        reason: reason.into(),
                        end: SessionEnd::LoggedOut,
                    })
                    .is_err()
                {
                    return;
                }
            }
            Err(e) => {
                // A live-stream failure — including a displacement kick, which reaches us as a bare
                // EOF and nothing else (decision 1262). No sleep: what happens next is the app's
                // policy, not this task's (0539 §3).
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
}

/// One connection cycle: **park pre-logon** for credentials (decision 0539) → logon → **park at
/// the realm list** until the app names a realm → world handshake → the character roster → **park
/// at character select** until the app picks → enter the
/// world, hand the writer to the write thread, then stream decoded [`SessionEvent`]s until the
/// socket dies (`Err`), the character logs out ([`Cycle::LoggedOut`]), or the app drops a channel
/// end ([`Cycle::Exit`]). Every pre-roster failure emits [`SessionEvent::LoginFailed`] and
/// re-parks ([`Cycle::Repark`]) — never a retry loop; resubmission is the app's policy.
async fn run(
    events_tx: &Sender<SessionEvent>,
    writer_tx: &Sender<WorldWriter>,
    parks: &Parks,
    abandon: &AtomicU64,
    ping_clock: &Mutex<PingClock>,
    tails_announced: &mut std::collections::HashSet<u16>,
) -> Result<Cycle> {
    let Parks {
        login_rx,
        realm_rx,
        pick_rx,
    } = parks;
    // ── The pre-logon park (decision 0539): wait for credentials. ───────────────────────────────
    bevy::log::info!("net: parked at the login screen — waiting for credentials");
    let Ok(req) = login_rx.recv().await else {
        return Ok(Cycle::Exit);
    };
    // Checked at every stage boundary; a blocking dial's result is discarded.
    let generation = req.generation;
    let canceled = move || abandon.load(Ordering::SeqCst) != generation;
    let stage = |s: LoginStage| {
        let _ = events_tx.send(SessionEvent::LoginStage { stage: s });
    };
    let fail = |refusal: Option<benilla_protocol::LoginRefusal>, reason: String| {
        let _ = events_tx.send(SessionEvent::LoginFailed {
            refusal,
            reason,
            terminal: false,
            dial: None,
        });
        Ok(Cycle::Repark)
    };
    // A dial that never opened a socket, the one failure the screen can advise on.
    let fail_dial = |dial: benilla_protocol::DialFailure, reason: String| {
        let _ = events_tx.send(SessionEvent::LoginFailed {
            refusal: None,
            reason,
            terminal: false,
            dial: Some(dial),
        });
        Ok(Cycle::Repark)
    };
    // A failure resubmitting cannot fix: the app shows it and does not retry.
    let fail_terminal = |reason: String| {
        let _ = events_tx.send(SessionEvent::LoginFailed {
            refusal: None,
            reason,
            terminal: true,
            dial: None,
        });
        Ok(Cycle::Repark)
    };

    // Logon: the dial and the SRP6 exchange against realmd.
    let mut logon = {
        stage(LoginStage::Connecting);
        match benilla_protocol::logon_async(&req.host, &req.user, &req.pass).await {
            Ok(l) => l,
            Err(e) => {
                if canceled() {
                    return Ok(Cycle::Repark);
                }
                // A server refusal carries its auth result byte for the `AUTH_*` string; a
                // transport failure carries none.
                if let Some(dial) = e.downcast_ref::<benilla_protocol::DialFailure>() {
                    return fail_dial(dial.clone(), format!("{e:#}"));
                }
                let refusal = e
                    .downcast_ref::<AuthReject>()
                    .map(|r| benilla_protocol::LoginRefusal::Logon(r.code));
                return fail(refusal, format!("{e:#}"));
            }
        }
    };
    if canceled() {
        return Ok(Cycle::Repark);
    }

    // ── The realm park ─────────────────────────────────────────────────────────────────────────
    //
    // The login-side list: `RealmList` is a `frameStrata="DIALOG"` frame over the current glue
    // screen and `RealmList_OnCancel` only hides it, so Cancel here returns to the login screen.
    // Change Realm is served at the character park. A server with no realms skips the park and
    // dials the fallback address.
    let mut realm = if logon.realms.is_empty() {
        None
    } else {
        // An answer queued during a dead cycle must not answer this list.
        while realm_rx.try_recv().is_ok() {}
        loop {
            if events_tx
                .send(SessionEvent::RealmList {
                    realms: logon.realms.clone(),
                })
                .is_err()
            {
                return Ok(Cycle::Exit);
            }
            bevy::log::info!(
                "net: parked at the realm list — {} realm(s) published",
                logon.realms.len()
            );
            let Ok(req) = realm_rx.recv().await else {
                return Ok(Cycle::Exit);
            };
            if canceled() {
                return Ok(Cycle::Repark);
            }
            match req {
                // Cancel over the login screen.
                RealmRequest::Abandon => return Ok(Cycle::Repark),
                // The reference re-requests the list every 5 s while it is up; a failed refresh
                // keeps the list on screen (`Logon::refresh_realms`).
                RealmRequest::Refresh => {
                    logon.refresh_realms(REALM_REFRESH_TIMEOUT).await;
                }
                // By name, so a list that changed under the click cannot enter the wrong realm;
                // a vanished name re-publishes the list.
                RealmRequest::Enter(name) => {
                    if let Some(realm) = logon.realms.iter().find(|r| r.name == name) {
                        break Some(realm.clone());
                    }
                }
            }
        }
    };

    // ── The world half, once per realm ─────────────────────────────────────────────────────────
    //
    // Change Realm re-enters this loop: the SRP6 session key authenticates against any world
    // server on the account's list, so a switch costs one world dial and no re-authentication.
    'realm: loop {
        // Named, since the address alone cannot tell a chosen realm from the fallback.
        if let Some(r) = &realm {
            bevy::log::info!(
                "net: realm {:?} ({} of {})",
                r.name,
                r.address,
                logon.realms.len()
            );
        }
        let world_addr = realm
            .as_ref()
            .map(|r| r.address.clone())
            // The fallback drops any auth `:port` from the realmlist for the world port.
            .unwrap_or_else(|| format!("{}:{}", host_port(&req.host, WORLD_PORT).0, WORLD_PORT));

        stage(LoginStage::Handshaking);
        // For the queue dialog to name; the roster that carries it comes after the queue.
        let realm_name = realm.as_ref().map(|r| r.name.clone());
        // A queue can last minutes, so it tests the abandon generation itself; otherwise a Cancel
        // would close the dialog while this thread held its place and entered the world anyway.
        let mut on_queue = |position: Option<u32>| {
            let _ = events_tx.send(SessionEvent::LoginQueued {
                position,
                realm: realm_name.clone(),
            });
            !canceled()
        };
        let mut session = match WorldSession::connect_queued_async(
            &world_addr,
            &req.user,
            logon.session_key,
            &mut on_queue,
        )
        .await
        {
            Ok(s) => s,
            Err(e) => {
                if canceled() {
                    return Ok(Cycle::Repark);
                }
                // A Warden refusal is the server's answer, not a transport fault.
                if let Some(w) = e.downcast_ref::<WardenRequired>() {
                    return fail_terminal(w.to_string());
                }
                // The world server's refusal code, for the screen's `AUTH_*` string.
                if let Some(r) = e.downcast_ref::<benilla_protocol::WorldAuthReject>() {
                    return fail(
                        Some(benilla_protocol::LoginRefusal::World(r.code)),
                        format!("{e:#}"),
                    );
                }
                return fail(None, format!("world handshake with {world_addr}: {e:#}"));
            }
        };
        // A cancel during the queue must not be overtaken by the roster.
        if canceled() {
            return Ok(Cycle::Repark);
        }

        // The roster as the account holds it, an empty one included: the reference builds
        // `CMSG_CHAR_CREATE` in one place (`0x5aac50`), reached only from the create screen.
        // Failures here are still pre-roster.
        let roster = session.char_enum_async().await;
        let mut characters = match roster {
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
        // A pick or realm answer queued during a dead cycle must not answer this roster: a logout
        // must land on the list, not bounce back in off a stale pick. The app re-sends its wants.
        while pick_rx.try_recv().is_ok() {}
        while realm_rx.try_recv().is_ok() {}
        if events_tx
            .send(SessionEvent::CharacterList {
                characters: characters.clone(),
                realm: realm.clone(),
            })
            .is_err()
        {
            return Ok(Cycle::Exit);
        }

        // Park at character select until the app answers. Create and delete are serviced in place.
        // If the server kicked the parked socket meanwhile, the login below fails, the cycle
        // restarts and the app re-sends its pick, so the park needs no keepalive.
        bevy::log::info!("net: parked at character select");
        let guid = loop {
            // Two channels, one park: `RealmList.xml` is a `frameStrata="DIALOG"` frame, so Change
            // Realm raises it over character select and both must be served while it is open.
            //
            // Named `pick` rather than `req`: the cycle's own `req` (the credentials) is still live.
            //
            // **Waiting on two channels at once, without a thread to block.** Natively this park
            // was `crossbeam_channel::select!`; a browser task cannot block, so the same two-way
            // wait is `futures_lite`'s `or` over the two `recv` futures — it polls the realm side,
            // then the pick side, and suspends on the event loop until one of them wakes. Both
            // futures are cancel-safe (`async_channel` hands a message to a `Recv` only when that
            // `Recv` is polled to completion), so the loser being dropped cannot swallow a request.
            //
            // The [`Parked`] answer stays exactly as it is natively, and for the same reason: the
            // arms decide, and every `break`/`continue` is made below, in plain sight.
            let fired =
                futures_lite::future::or(async { Fired::Realm(realm_rx.recv().await) }, async {
                    Fired::Pick(pick_rx.recv().await)
                })
                .await;
            let answer = match fired {
                Fired::Realm(req) => match req {
                    Err(_) => Parked::Exit,
                    // `RequestRealmList`: refresh on the realmd socket this cycle holds, then
                    // republish; also Change Realm's first list.
                    Ok(RealmRequest::Refresh) => {
                        logon.refresh_realms(REALM_REFRESH_TIMEOUT).await;
                        if events_tx
                            .send(SessionEvent::RealmList {
                                realms: logon.realms.clone(),
                            })
                            .is_err()
                        {
                            Parked::Exit
                        } else {
                            Parked::StayPut
                        }
                    }
                    // `RealmList_OnCancel` over character select: only the dialog hides.
                    Ok(RealmRequest::Abandon) => Parked::StayPut,
                    // `ChangeRealm(category, index)`: dial the chosen realm with the session key
                    // we hold; a vanished name changes nothing.
                    Ok(RealmRequest::Enter(name)) => logon
                        .realms
                        .iter()
                        .find(|r| r.name == name)
                        .cloned()
                        .map_or(Parked::StayPut, Parked::Realm),
                },
                Fired::Pick(pick) => match pick {
                    Err(_) => Parked::Exit,
                    Ok(CharRequest::Enter(guid)) => Parked::Play(guid),
                    // Select's Back: drop the parked session, return to the login park.
                    Ok(CharRequest::Abandon) => Parked::Repark,
                    Ok(CharRequest::Create(create)) => Parked::Acted(
                        CharAction::Create,
                        session.create_character_async(&create).await?,
                    ),
                    Ok(CharRequest::Delete(target)) => Parked::Acted(
                        CharAction::Delete,
                        session.delete_character_async(target).await?,
                    ),
                },
            };
            let (action, code) = match answer {
                Parked::Exit => return Ok(Cycle::Exit),
                Parked::Repark => return Ok(Cycle::Repark),
                Parked::StayPut => continue,
                Parked::Play(guid) => break guid,
                Parked::Realm(chosen) => {
                    realm = Some(chosen);
                    continue 'realm;
                }
                Parked::Acted(action, code) => (action, code),
            };
            // A success changed the roster: re-emit it before the result, so the screen has the
            // fresh list when it reacts.
            let changed = match action {
                CharAction::Create => code == messages::CHAR_CREATE_SUCCESS,
                CharAction::Delete => code == messages::CHAR_DELETE_SUCCESS,
            };
            if changed {
                characters = session.char_enum_async().await?;
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
        session.player_login_async(refuse_once(guid)).await?;
        session.set_active_mover_async(guid).await?;

        let billing_time_rested = session.billing_time_rested();
        let tutorial_flags = session.take_tutorial_flags();
        // `SMSG_ADDON_INFO` carries no names, so it is paired back against the block we sent. No
        // reply means no AddOn index space at all, carried as `None`.
        let statuses = session.take_addon_info();
        let addon_info = statuses.as_deref().map(|statuses| {
            benilla_protocol::messages::hidden_from_reply(
                statuses,
                &benilla_protocol::messages::STOCK_SECURE_ADDONS,
            )
        });
        match (&statuses, &addon_info) {
            (Some(statuses), Some(hidden)) => bevy::log::info!(
                "net: SMSG_ADDON_INFO — {} record(s), {} hidden from the AddOn index space",
                statuses.len(),
                hidden.len()
            ),
            _ => bevy::log::warn!(
                "net: no SMSG_ADDON_INFO — GetNumAddOns() stays 0, as the reference's would"
            ),
        }
        let (mut reader, writer) = session.into_split()?;
        if events_tx
            .send(SessionEvent::Connected {
                self_guid: guid,
                name,
                billing_time_rested,
                tutorial_flags,
                addon_info,
            })
            .is_err()
        {
            return Ok(Cycle::Exit);
        }
        if writer_tx.send(writer).is_err() {
            // The writer thread ends only on app exit.
            return Ok(Cycle::Exit);
        }
        bevy::log::info!("net: connected to {world_addr}");

        // `poll` skips packets it cannot parse; a long run of skips means the stream desynced.
        let (mut skip_run, mut skip_logged) = (0u32, 0u32);
        loop {
            let polled = reader.poll_async().await?;
            note_inbound(); // one packet off the wire, parsed or not — the census counts liveness
            match polled {
                Poll::Events {
                    opcode,
                    events,
                    tail,
                } => {
                    skip_run = 0;
                    // A body is length-framed, so a decoder shorter than the server's layout
                    // succeeds silently; report it once per opcode, and never skip the packet.
                    if tail > 0 && tails_announced.insert(opcode) {
                        bevy::log::info!(
                            "net: opcode {} ({opcode:#06x}) left {tail} trailing byte(s) after \
                             decode (first occurrence; announced once per opcode)",
                            benilla_protocol::messages::opcode_name(opcode).unwrap_or("?"),
                        );
                    }
                    // Every inbound opcode by name (tag `in`), including one that parsed into no
                    // event.
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
                        // The pong bypass: the reference's `OnData` (`0x537b10`) hands `SMSG_PONG`
                        // to `HandlePong` (`0x537d60`) inline, not onto the game thread's queue, so
                        // it is timed and consumed here.
                        if let SessionEvent::Pong { sequence } = ev {
                            if let Some(rtt) = ping_clock.lock_recover().record_pong(sequence) {
                                bevy::log::debug!("net: pong seq={sequence} rtt={rtt}ms");
                            }
                            continue;
                        }
                        // A logout or a refused login ends the cycle after the app hears it, so
                        // the screen has the reason before the teardown.
                        let ends = match ev {
                            SessionEvent::LoggedOut => Some(Cycle::LoggedOut),
                            SessionEvent::CharacterLoginFailed { .. } => Some(Cycle::LoginRefused),
                            _ => None,
                        };
                        // Receiver dropped: the app exited.
                        if events_tx.send(ev).is_err() {
                            return Ok(Cycle::Exit);
                        }
                        if let Some(cycle) = ends {
                            return Ok(cycle);
                        }
                    }
                }
                Poll::Skipped { opcode, reason } => {
                    skip_run += 1;
                    // Every skip, uncapped, into the trace (tag `skip`): otherwise a packet that
                    // failed to parse looks like one that never arrived.
                    if benilla_assets::trace::enabled() {
                        benilla_assets::trace::line(
                            "skip",
                            &format!("opcode={opcode:#06x} {reason}"),
                        );
                    }
                    // Capped, but enough for a post-teleport burst of unparseable object updates.
                    if skip_logged < 40 {
                        bevy::log::warn!("net: skipping unparseable packet — {reason}");
                        skip_logged += 1;
                    }
                    // A parse error feeds the dropped-packet tally like an unknown opcode.
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
    } // 'realm
}

/// `WOW_REFUSE_LOGIN=1` makes the server refuse the run's first character login, which cannot be
/// staged by hand. Only the wire guid changes, to one vmangos's `!packet.guid.IsPlayer()` guard
/// refuses; the app still sees the real character. Once only, so the run reaches the recovery
/// instead of looping on the `WOW_CHAR` fast path.
fn refuse_once(guid: u64) -> u64 {
    /// `HIGHGUID_UNIT | 1`: no player's guid. Shared with `benilla-protocol`'s
    /// `login_refusal_probe`.
    const NOT_A_PLAYER: u64 = 0xF130_0000_0000_0001;
    static ARMED: std::sync::OnceLock<AtomicU64> = std::sync::OnceLock::new();
    let armed = ARMED
        .get_or_init(|| AtomicU64::new(u64::from(std::env::var_os("WOW_REFUSE_LOGIN").is_some())));
    if armed.swap(0, Ordering::SeqCst) == 1 {
        bevy::log::warn!("net: WOW_REFUSE_LOGIN — sending this pick with a non-player guid");
        NOT_A_PLAYER
    } else {
        guid
    }
}

/// Drain the writer's sent-packet log into the trace as `out` lines — one per packet that reached
/// the socket, by opcode name and body length. A no-op unless the `out` tag armed the log when the
/// connection was handed over. `pub(super)` because the browser's per-frame pump
/// (`crate::net::web_writer_pump`) is the other write loop and wants the same `out` lines.
pub(super) fn trace_sends(w: &mut WorldWriter) {
    w.drain_sent(|opcode, len| {
        benilla_assets::trace::line(
            "out",
            &format!(
                "{opcode:#06x} {} len={len}",
                benilla_protocol::messages::opcode_name(opcode).unwrap_or("?")
            ),
        );
    });
}

/// The single write thread (native only — a page has no threads; `crate::net`'s `web_writer_pump`
/// is the browser's answer, and it drains the same two channels through the same [`dispatch`]).
///
/// `select!` between app commands, writer swaps from the read thread, and
/// the 30 s keepalive tick ([`PING_INTERVAL`] — the real client's ping cadence). While disconnected
/// (no writer yet, or the socket died under the current one), commands drop with a capped warn and
/// the tick no-ops — they are meaningless without a live session, and the server re-syncs our state
/// from the reconnect handshake anyway. Ends when the app drops every command sender.
#[cfg(not(target_arch = "wasm32"))]
fn writer_loop(
    cmd_rx: &Receiver<ClientCommand>,
    mut writer_rx: Receiver<WorldWriter>,
    ping_clock: &Mutex<PingClock>,
) {
    let mut writer: Option<WorldWriter> = None;
    let mut warned = 0u32;
    // Armed at connect and re-armed by each send, never free-running: the reference pings when
    // `now - lastSent - 30000 >= 0` (`0x537ff0`), with `lastSent` stamped at connect (`0x537bcf`).
    let mut ping_tick = crossbeam_channel::never();
    loop {
        crossbeam_channel::select! {
            recv(writer_rx) -> w => match w {
                Ok(mut w) => {
                    // Arm the outbound opcode trace (tag `out`); a fresh socket starts a fresh log.
                    if benilla_assets::trace::enabled_for("out") {
                        w.watch_sends();
                    }
                    writer = Some(w);
                    warned = 0;
                    // Sequence 1 is the new socket's first ping, so an old socket's pong cannot
                    // match.
                    ping_clock.lock_recover().clear();
                    // The first keepalive is a full interval after connect (`0x537bcf`).
                    ping_tick = crossbeam_channel::after(PING_INTERVAL);
                }
                // The read thread ended (app exit). A disconnected receiver is always ready and
                // would spin the select, so stop selecting it.
                Err(_) => writer_rx = crossbeam_channel::never(),
            },
            recv(ping_tick) -> _ => {
                // Sent only with a live writer; the parked character-select socket goes without.
                // Disarmed unless a send is attempted, so a dead connection stops pinging.
                ping_tick = crossbeam_channel::never();
                if let Some(w) = writer.as_mut() {
                    // Re-armed from the send, as the reference measures it; a failed write counts.
                    ping_tick = crossbeam_channel::after(PING_INTERVAL);
                    let (sequence, last_rtt) = {
                        let mut c = ping_clock.lock_recover();
                        c.sequence += 1;
                        c.sent_at = Some(Instant::now());
                        // `lastRtt` is the latest sample, not the mean (`ring[write-1]`, at
                        // `0x537e87`). Deviation: the reference's guard at `0x537e85` sends 0
                        // whenever the write index is 0, one ping in sixteen; we send 0 only before
                        // the first pong, because the rest is an artefact of its index arithmetic
                        // that only the server stores.
                        (c.sequence, c.last_rtt_ms.unwrap_or(0))
                    };
                    if let Err(e) = w.ping(sequence, last_rtt) {
                        if warned < SEND_WARN_CAP {
                            bevy::log::warn!("net: ping send failed: {e:#}");
                            warned += 1;
                        }
                    }
                    trace_sends(w);
                }
            },
            recv(cmd_rx) -> cmd => {
                let Ok(cmd) = cmd else { return }; // all app senders dropped → app exit
                let Some(w) = writer.as_mut() else {
                    // No live writer: the command is dropped, and both lines name it.
                    if benilla_assets::trace::enabled() {
                        benilla_assets::trace::line(
                            "wire",
                            &format!("DROPPED — no live session: {cmd:?}"),
                        );
                    }
                    if warned < SEND_WARN_CAP {
                        bevy::log::warn!("net: dropping command — not connected: {cmd:?}");
                        warned += 1;
                    }
                    continue;
                };
                        let result = dispatch(w, cmd);
                        // **What actually reached the socket** (tag `wire`, decision 0621). The controller's
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
                // What reached the socket, by name (tag `out`); one command can be several packets.
                trace_sends(w);
            },
        }
    }
}

/// Send one [`ClientCommand`] down the wire — the whole outbound verb table, one arm per thing the
/// player can do. Lifted out of [`writer_loop`] verbatim when the web build arrived (decision: the
/// browser has no write thread, so its per-frame pump in [`crate::net`] needs the same body without
/// the `select!` around it). The map is TOTAL: every command has exactly one send, and a new
/// command that forgets one fails to compile here rather than going quietly missing.
pub(super) fn dispatch(w: &mut WorldWriter, cmd: ClientCommand) -> Result<()> {
    match cmd {
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
        ClientCommand::MoveTimeSkipped { guid, lag_ms } => w.move_time_skipped(guid, lag_ms),
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
        ClientCommand::Chat {
            kind,
            target,
            text,
            language,
        } => w.send_message_chat(kind.chat_type(), language, target.as_deref(), &text),
        // The addon lane (decision 1235). The distribution arrived as an enum and the
        // map is TOTAL — no "unknown, guess SAY" arm exists, which is what the enum
        // seam is for — so the whole arm is one call.
        ClientCommand::AddonMessage { distribution, text } => {
            w.send_addon_message(super::addon_wire_chat_type(distribution), &text)
        }
        ClientCommand::JoinChannel { name, password } => w.join_channel(&name, &password),
        ClientCommand::LeaveChannel { name } => w.leave_channel(&name),
        ClientCommand::ChannelList { name } => w.channel_list(&name),
        ClientCommand::RandomRoll { min, max } => w.random_roll(min, max),
        ClientCommand::ChannelOwner { name } => w.channel_owner(&name),
        ClientCommand::ChannelSetOwner { name, player } => w.channel_set_owner(&name, &player),
        ClientCommand::ChannelPassword { name, password } => w.channel_password(&name, &password),
        ClientCommand::ChannelModerator { name, player } => w.channel_moderator(&name, &player),
        ClientCommand::ChannelUnmoderator { name, player } => w.channel_unmoderator(&name, &player),
        ClientCommand::ChannelMute { name, player } => w.channel_mute(&name, &player),
        ClientCommand::ChannelUnmute { name, player } => w.channel_unmute(&name, &player),
        ClientCommand::ChannelInvite { name, player } => w.channel_invite(&name, &player),
        ClientCommand::ChannelKick { name, player } => w.channel_kick(&name, &player),
        ClientCommand::ChannelBan { name, player } => w.channel_ban(&name, &player),
        ClientCommand::ChannelUnban { name, player } => w.channel_unban(&name, &player),
        ClientCommand::ChannelAnnouncements { name } => w.channel_announcements(&name),
        ClientCommand::ChannelModerate { name } => w.channel_moderate(&name),
        ClientCommand::PlayedTime => w.played_time(),
        ClientCommand::NameQuery { guid } => w.name_query(guid),
        ClientCommand::CreatureQuery { entry, guid } => w.creature_query(entry, guid),
        ClientCommand::PetNameQuery { pet_number, guid } => w.pet_name_query(pet_number, guid),
        ClientCommand::ItemQuery { entry, guid } => w.item_query(entry, guid),
        ClientCommand::UseItem {
            bag_index,
            slot,
            spell_index,
            target,
        } => w.use_item(bag_index, slot, spell_index, target),
        ClientCommand::OpenItem { bag_index, slot } => w.open_item(bag_index, slot),
        ClientCommand::WrapItem {
            gift_bag,
            gift_slot,
            item_bag,
            item_slot,
        } => w.wrap_item(gift_bag, gift_slot, item_bag, item_slot),
        ClientCommand::AutoEquipItem { bag_index, slot } => w.auto_equip_item(bag_index, slot),
        ClientCommand::SetAmmo { entry } => w.set_ammo(entry),
        ClientCommand::SwapInvItem { src_slot, dst_slot } => w.swap_inv_item(src_slot, dst_slot),
        ClientCommand::SwapItem {
            dst_bag,
            dst_slot,
            src_bag,
            src_slot,
        } => w.swap_item(dst_bag, dst_slot, src_bag, src_slot),
        ClientCommand::AutoStoreBagItem {
            src_bag,
            src_slot,
            dst_bag,
        } => w.auto_store_bag_item(src_bag, src_slot, dst_bag),
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
        ClientCommand::CastSpellCorpse {
            spell_id,
            target,
            corpse_guid,
        } => w.cast_spell_corpse(spell_id, target, corpse_guid),
        ClientCommand::CastSpellAtDest { spell_id, dest } => w.cast_spell_at_dest(spell_id, dest),
        ClientCommand::CastSpellAtSource { spell_id, src } => w.cast_spell_at_source(spell_id, src),
        ClientCommand::CancelAura { spell_id } => w.cancel_aura(spell_id),
        ClientCommand::SetActionButton { button, packed } => w.set_action_button(button, packed),
        ClientCommand::SetActionBarToggles { toggles } => w.set_actionbar_toggles(toggles),
        ClientCommand::PetAction {
            pet_guid,
            packed,
            target_guid,
        } => w.pet_action(pet_guid, packed, target_guid),
        ClientCommand::PetSetAction { pet_guid, entries } => w.pet_set_action(pet_guid, &entries),
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
        ClientCommand::BuyItemInSlot {
            vendor,
            entry,
            bag_guid,
            bag_slot,
            count,
        } => w.buy_item_in_slot(vendor, entry, bag_guid, bag_slot, count),
        ClientCommand::SellItem {
            vendor,
            item_guid,
            count,
        } => w.sell_item(vendor, item_guid, count),
        ClientCommand::BuybackItem { vendor, slot } => w.buyback_item(vendor, slot),
        ClientCommand::RepairItem { vendor, item_guid } => w.repair_item(vendor, item_guid),
        ClientCommand::GmTicketCreate {
            category,
            map,
            pos,
            text,
        } => w.gm_ticket_create(category, map, pos, &text),
        ClientCommand::GmTicketUpdate { category, text } => w.gm_ticket_updatetext(category, &text),
        ClientCommand::GmTicketGet => w.gm_ticket_get(),
        ClientCommand::GmTicketDelete => w.gm_ticket_delete(),
        ClientCommand::GmTicketSystemStatus => w.gm_ticket_system_status(),
        ClientCommand::BinderActivate { binder } => w.binder_activate(binder),
        ClientCommand::SummonResponse { summoner } => w.summon_response(summoner),
        ClientCommand::TalentWipeConfirm { trainer } => w.talent_wipe_confirm(trainer),
        ClientCommand::ForceLogout => w.player_logout(),
        ClientCommand::AreaSpiritHealerQueue { healer } => w.area_spirit_healer_queue(healer),
        ClientCommand::BattlefieldPort { map_id, accept } => w.battlefield_port(map_id, accept),
        ClientCommand::RequestBattlefieldScoreData => w.request_battlefield_score_data(),
        ClientCommand::LeaveBattlefield { map_id } => w.leave_battlefield(map_id),
        ClientCommand::MeetingStoneJoin { go_guid } => w.meeting_stone_join(go_guid),
        ClientCommand::AreaSpiritHealerQuery { healer } => w.area_spirit_healer_query(healer),
        ClientCommand::MeetingStoneLeave => w.meeting_stone_leave(),
        ClientCommand::MeetingStoneStatusQuery => w.meeting_stone_status_query(),
        ClientCommand::TutorialFlag { id } => w.tutorial_flag(id),
        ClientCommand::TutorialClear => w.tutorial_clear(),
        ClientCommand::TutorialReset => w.tutorial_reset(),
        ClientCommand::BattlefieldList { map_id } => w.battlefield_list(map_id),
        ClientCommand::RequestBattlefieldPositions => w.request_battlefield_positions(),
        ClientCommand::TabardVendorActivate { npc } => w.tabard_vendor_activate(npc),
        ClientCommand::SaveGuildEmblem { vendor, design } => w.save_guild_emblem(vendor, design),
        ClientCommand::BattlemasterHello { npc } => w.battlemaster_hello(npc),
        ClientCommand::BattlemasterJoin {
            battlemaster,
            map_id,
            instance_id,
            as_group,
        } => w.battlemaster_join(battlemaster, map_id, instance_id, as_group),
        ClientCommand::BattlefieldJoin {
            map_id,
            instance_id,
            as_group,
        } => w.battlefield_join(map_id, instance_id, as_group),
        ClientCommand::BattlefieldStatusRequest => w.battlefield_status(),
        ClientCommand::PetUnlearn { trainer } => w.pet_unlearn(trainer),
        ClientCommand::BankerActivate { guid } => w.banker_activate(guid),
        ClientCommand::BuyBankSlot { guid } => w.buy_bank_slot(guid),
        ClientCommand::AutoBankItem { bag, slot } => w.autobank_item(bag, slot),
        ClientCommand::AutoStoreBankItem { bag, slot } => w.autostore_bank_item(bag, slot),
        ClientCommand::TrainerList { trainer } => w.trainer_list(trainer),
        ClientCommand::TrainerBuySpell { trainer, spell_id } => {
            w.trainer_buy_spell(trainer, spell_id)
        }
        ClientCommand::ListStabledPets { npc } => w.list_stabled_pets(npc),
        ClientCommand::StablePet { npc } => w.stable_pet(npc),
        ClientCommand::UnstablePet { npc, pet_number } => w.unstable_pet(npc, pet_number),
        ClientCommand::StableSwapPet { npc, pet_number } => w.stable_swap_pet(npc, pet_number),
        ClientCommand::BuyStableSlot { npc } => w.buy_stable_slot(npc),
        ClientCommand::LearnTalent { talent_id, rank } => w.learn_talent(talent_id, rank),
        ClientCommand::UnlearnSkill { skill_id } => w.unlearn_skill(skill_id),
        ClientCommand::SetFactionAtWar {
            rep_list_id,
            at_war,
        } => w.set_faction_at_war(rep_list_id, at_war),
        ClientCommand::SetFactionInactive {
            rep_list_id,
            inactive,
        } => w.set_faction_inactive(rep_list_id, inactive),
        ClientCommand::SetWatchedFaction { rep_list_id } => w.set_watched_faction(rep_list_id),
        ClientCommand::GameObjUse { guid } => w.gameobj_use(guid),
        ClientCommand::AreaTrigger { trigger_id } => w.area_trigger(trigger_id),
        ClientCommand::GameObjectQuery { entry, guid } => w.gameobject_query(entry, guid),
        ClientCommand::PageTextQuery { page_id, guid } => w.page_text_query(page_id, guid),
        ClientCommand::CastSpellGameObject { spell_id, go_guid } => {
            w.cast_spell_gameobject(spell_id, go_guid)
        }
        ClientCommand::CastSpellItem {
            spell_id,
            item_guid,
        } => w.cast_spell_item(spell_id, item_guid),
        ClientCommand::LootMasterGive { guid, slot, target } => {
            w.loot_master_give(guid, slot, target)
        }
        ClientCommand::Loot { guid } => w.loot(guid),
        ClientCommand::AutostoreLootItem { slot } => w.autostore_loot_item(slot),
        ClientCommand::LootMoney => w.loot_money(),
        ClientCommand::LootRelease { guid } => w.loot_release(guid),
        ClientCommand::LootRoll {
            looted_target,
            item_slot,
            roll_type,
        } => w.loot_roll(looted_target, item_slot, roll_type),
        ClientCommand::QuestgiverQuery { npc, quest } => w.questgiver_query_quest(npc, quest),
        ClientCommand::QuestgiverAccept { npc, quest } => w.questgiver_accept_quest(npc, quest),
        ClientCommand::QuestgiverComplete { npc, quest } => w.questgiver_complete_quest(npc, quest),
        ClientCommand::QuestgiverRequestReward { npc, quest } => {
            w.questgiver_request_reward(npc, quest)
        }
        ClientCommand::QuestgiverChooseReward { npc, quest, choice } => {
            w.questgiver_choose_reward(npc, quest, choice)
        }
        ClientCommand::QuestQuery { quest } => w.quest_query(quest),
        ClientCommand::QuestgiverStatusQuery { npc } => w.questgiver_status_query(npc),
        ClientCommand::QuestgiverHello { npc } => w.questgiver_hello(npc),
        ClientCommand::QuestlogRemove { slot } => w.questlog_remove_quest(slot),
        ClientCommand::PushQuestToParty { quest } => w.push_quest_to_party(quest),
        ClientCommand::QuestConfirmAccept { quest } => w.quest_confirm_accept(quest),
        ClientCommand::QuestPushResult { sharer, msg } => w.quest_push_result(sharer, msg),
        ClientCommand::GetMailList { mailbox } => w.get_mail_list(mailbox),
        ClientCommand::SendMail {
            mailbox,
            receiver,
            subject,
            body,
            stationery,
            package,
            item_guid,
            money,
            cod,
        } => w.send_mail(
            mailbox, &receiver, &subject, &body,
            // vmangos stores `MAIL_STATIONERY_DEFAULT` (41) regardless of the chosen
            // stationery, and reads the package id (`Packets/Mail.cpp:10`) unused.
            stationery, package, item_guid, money, cod,
        ),
        ClientCommand::MailTakeMoney { mailbox, mail_id } => w.mail_take_money(mailbox, mail_id),
        ClientCommand::MailTakeItem { mailbox, mail_id } => w.mail_take_item(mailbox, mail_id),
        ClientCommand::MailMarkAsRead { mailbox, mail_id } => w.mail_mark_as_read(mailbox, mail_id),
        ClientCommand::MailReturnToSender { mailbox, mail_id } => {
            w.mail_return_to_sender(mailbox, mail_id)
        }
        ClientCommand::MailDelete { mailbox, mail_id } => w.mail_delete(mailbox, mail_id),
        ClientCommand::MailCreateTextItem { mailbox, mail_id } => {
            w.mail_create_text_item(mailbox, mail_id)
        }
        ClientCommand::ItemTextQuery { text_id, mail_id } => w.item_text_query(text_id, mail_id),
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
        ClientCommand::BusyTrade => w.busy_trade(),
        ClientCommand::IgnoreTrade => w.ignore_trade(),
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
        ClientCommand::OpeningCinematic => w.opening_cinematic(),
        ClientCommand::CompleteCinematic => w.complete_cinematic(),
        ClientCommand::NextCinematicCamera => w.next_cinematic_camera(),
        ClientCommand::MoveModeAck {
            guid,
            counter,
            mode,
            apply,
            flags,
            pos,
            orientation,
        } => w.move_mode_ack(guid, counter, mode, apply, flags, (pos, orientation)),
        ClientCommand::KnockBackAck {
            guid,
            counter,
            launch,
            flags,
            pos,
            orientation,
            transport,
        } => w.knock_back_ack(guid, counter, launch, flags, (pos, orientation), transport),
        ClientCommand::RepopRequest => w.repop_request(),
        ClientCommand::CorpseQuery => w.corpse_query(),
        ClientCommand::ReclaimCorpse { corpse } => w.reclaim_corpse(corpse),
        ClientCommand::SelfRes => w.self_res(),
        ClientCommand::SpiritHealerActivate { npc } => w.spirit_healer_activate(npc),
        ClientCommand::ResurrectResponse { caster, accept } => w.resurrect_response(caster, accept),
        ClientCommand::GroupInvite { name } => w.group_invite(&name),
        ClientCommand::GroupAccept => w.group_accept(),
        ClientCommand::GroupDecline => w.group_decline(),
        ClientCommand::GroupUninvite { name } => w.group_uninvite(&name),
        ClientCommand::GroupSetLeader { guid } => w.group_set_leader(guid),
        ClientCommand::GroupLeave => w.group_disband(),
        ClientCommand::GroupRaidConvert => w.group_raid_convert(),
        ClientCommand::RequestPartyMemberStats { guid } => w.request_party_member_stats(guid),
        ClientCommand::LootMethod {
            method,
            master,
            threshold,
        } => w.loot_method(method, master, threshold),
        ClientCommand::SetRaidTarget { icon, guid } => w.raid_target_set(icon, guid),
        ClientCommand::MinimapPing { x, y } => w.minimap_ping(x, y),
        ClientCommand::GroupChangeSubGroup { name, group } => {
            w.group_change_sub_group(&name, group)
        }
        ClientCommand::GroupSwapSubGroup { name, other } => w.group_swap_sub_group(&name, &other),
        ClientCommand::GroupAssistantLeader { guid, grant } => {
            w.group_assistant_leader(guid, grant)
        }
        ClientCommand::ReadyCheckStart => w.ready_check_start(),
        ClientCommand::ReadyCheckAnswer { ready } => w.ready_check_answer(ready),
        ClientCommand::RequestRaidInfo => w.request_raid_info(),
        ClientCommand::ResetInstances => w.reset_instances(),
        ClientCommand::DuelAccepted { arbiter } => w.duel_accepted(arbiter),
        ClientCommand::DuelCancelled { arbiter } => w.duel_cancelled(arbiter),
        ClientCommand::TogglePvp => w.toggle_pvp(),
        ClientCommand::ToggleHelm => w.toggle_helm(),
        ClientCommand::ToggleCloak => w.toggle_cloak(),
        ClientCommand::FriendListRequest => w.friend_list(),
        ClientCommand::AddFriend { name } => w.add_friend(&name),
        ClientCommand::SetLookingForGroup { slots, comment } => {
            w.set_looking_for_group(slots, &comment)
        }
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
        ClientCommand::GuildSetPublicNote { name, note } => w.guild_set_public_note(&name, &note),
        ClientCommand::GuildSetOfficerNote { name, note } => w.guild_set_officer_note(&name, &note),
        ClientCommand::GuildInfoText { text } => w.guild_info_text(&text),
        // The petition family (decision 1672) — founding a guild.
        ClientCommand::PetitionShowList { npc } => w.petition_show_list(npc),
        ClientCommand::PetitionBuy { npc, name } => w.petition_buy(npc, &name),
        ClientCommand::PetitionShowSignatures { item } => w.petition_show_signatures(item),
        ClientCommand::PetitionSign { item, byte } => w.petition_sign(item, byte),
        ClientCommand::OfferPetition { item, player } => w.offer_petition(item, player),
        ClientCommand::TurnInPetition { item } => w.turn_in_petition(item),
        ClientCommand::PetitionQuery { petition_id, item } => w.petition_query(petition_id, item),
        ClientCommand::PetitionRename { item, name } => w.petition_rename(item, &name),
        ClientCommand::PetitionDecline { item } => w.petition_decline(item),
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
    }
}

#[cfg(test)]
mod cycle_tests;

#[cfg(test)]
mod rtt_tests {
    use super::{PingClock, RTT_RING};
    use std::time::Instant;

    /// Arms the clock as the write thread does.
    fn sent(clock: &mut PingClock, sequence: u32) {
        clock.sequence = sequence;
        clock.sent_at = Some(Instant::now());
    }

    /// At a 30 s cadence one spike moves the meter by a fifteenth, not the whole way.
    #[test]
    fn the_reported_latency_is_the_mean_of_a_bounded_ring() {
        let mut clock = PingClock::default();
        assert_eq!(clock.avg_latency_ms(), None, "no pong yet");

        clock.file_rtt(40);
        assert_eq!(clock.avg_latency_ms(), Some(40));
        clock.file_rtt(60);
        assert_eq!(clock.avg_latency_ms(), Some(50), "the mean, not the last");
        assert_eq!(
            clock.last_rtt_ms,
            Some(60),
            "the last sample stays separate"
        );

        // Fill past the ring: only the newest RTT_RING samples count, so the two above age out.
        for _ in 0..RTT_RING {
            clock.file_rtt(100);
        }
        assert_eq!(clock.rtt_ring.len(), RTT_RING);
        assert_eq!(clock.avg_latency_ms(), Some(100));

        clock.clear();
        assert_eq!(clock.avg_latency_ms(), None, "a disconnect forgets it all");
        assert_eq!(clock.last_rtt_ms, None);
    }

    /// A pong from a dead socket (straddling a reconnect, after `clear`) must not enter the ring.
    #[test]
    fn only_the_pong_we_are_waiting_for_is_recorded() {
        let mut clock = PingClock::default();
        assert_eq!(clock.record_pong(1), None, "no ping is in flight");

        sent(&mut clock, 7);
        assert_eq!(
            clock.record_pong(6),
            None,
            "a stale sequence is not our ping"
        );
        assert_eq!(clock.record_pong(8), None, "nor is one we never sent");
        assert!(clock.rtt_ring.is_empty(), "neither entered the ring");

        assert!(clock.record_pong(7).is_some(), "the one we are timing");
        assert_eq!(clock.rtt_ring.len(), 1);
        assert!(clock.avg_latency_ms().is_some());

        // The reconnect edge: cleared, so the old socket's echo has nothing to match.
        clock.clear();
        assert_eq!(
            clock.record_pong(7),
            None,
            "the dead socket's echo is dropped"
        );
        assert_eq!(clock.avg_latency_ms(), None);
    }
}
