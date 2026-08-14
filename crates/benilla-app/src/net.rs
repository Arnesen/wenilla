//! The network↔ECS bridge: a typed-event channel into real ECS entities (decision 0006).
//!
//! A background **read** thread owns the blocking [`WorldSession`] (logon → world handshake → enter
//! world) and streams the world as a flat list of [`SessionEvent`]s over a channel; a sibling
//! **write** thread owns the writer and drains [`ClientCommand`]s. One ECS system,
//! [`apply_net_updates`], drains the event channel each frame and mutates **real Bevy entities** —
//! spawn on create, transform-write on move, despawn on remove — keyed by a [`Guid`] component with a
//! [`GuidIndex`] for O(1) lookup. There is no shadow world: a unit is one entity, full stop.
//!
//! Movement paths become a [`Spline`] component sampled by [`sample_splines`]; the server clock
//! becomes the [`ServerTime`] resource; one-shot directives (teleport, worldport) become Bevy
//! [`Message`]s the player/terrain consume. Outbound movement + chat go through [`NetCommands`].
//!
//! The bridge runs in [`WorldStage::Net`], before `Input`, so a server teleport snaps → streams →
//! covers in one frame. Coordinates cross from raw WoW into Bevy space here (`wow_to_bevy`).

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use benilla_protocol::{
    messages::WhoRequest, EntityKind, JumpInfo, MoveMode, MoveSpeeds, ObjectFields, SessionEvent,
    SpeedKind, TransportPose,
};
use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender};

use benilla_world::schedule::WorldStage;

mod apply;
pub(crate) mod io;
mod motion;

use apply::{apply_net_updates, tag_self_player};

// The per-frame motion model lives in [`motion`]: `RemoteMotion`/`Spline` are re-exported for the
// crate (the animation selector reads them); the integration systems + pose helpers stay `pub(super)`
// and are pulled in here for the plugin + the event bridge.
pub(crate) use io::LoginRequest;
use motion::{
    drain_pending_moves, extrapolate_remote_units, face_target, ground_clamp_creatures,
    mark_swimming_creatures, sample_splines,
};
pub(crate) use motion::{jump_seed, CreatureSwimming, FacingStep, RemoteMotion, Spline};

/// The net subsystem: spawns the background IO threads and drives the per-frame event drain.
pub(crate) struct NetPlugin {
    /// Connect to the server. `false` in capture mode ([`crate::capture`]): the channel resources
    /// still exist (so the player/chat command sends stay harmless no-ops), but no IO thread runs, so
    /// captures are deterministic regardless of whether a server happens to be up.
    pub(crate) connect: bool,
}

/// Marker for "this process has **no** IO thread" — inserted only when [`NetPlugin::connect`] is
/// false. Read by `crate::preflight`'s startup notice, which says so out loud, so a run that cannot
/// exercise the wire path is never mistaken for one that did (decision 0728).
///
/// A resource of its own rather than a bool on [`NetStatus`], because it answers a different
/// question: `connected` is *runtime* state that a refused or dropped connection also clears, while
/// this one says the attempt was never made and no packet can ever arrive.
#[derive(Resource)]
pub(crate) struct NetOffline;

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        let handles = io::spawn_net(io::NetConfig::from_env(), self.connect);
        if !self.connect {
            app.insert_resource(NetOffline);
        }
        // In the wire-drain stage, because that is what it is the product of. The lighting
        // resolve is ordered after that stage so it always reads THIS frame's clock rather than
        // whichever way the executor happened to break the tie.
        app.add_systems(
            Update,
            publish_world_time.in_set(benilla_world::schedule::WorldStage::Net),
        );
        app.insert_resource(NetEvents(handles.events))
            .insert_resource(NetCommands(handles.commands))
            .insert_resource(CharPick(handles.pick))
            .insert_resource(LoginSubmit(handles.login))
            .insert_resource(LoginAbandon(handles.login_abandon))
            .insert_resource(PingShared(handles.ping))
            .init_resource::<GuidIndex>()
            .init_resource::<SelfGuid>()
            .init_resource::<PendingTransfer>()
            .init_resource::<NetStatus>()
            .init_resource::<DroppedOpcodes>()
            .init_resource::<ServerTime>()
            .init_resource::<ServerWallClock>()
            .init_resource::<Reputations>()
            .init_resource::<HomeBind>()
            .init_resource::<Proficiencies>()
            .init_resource::<crate::names::NameCache>()
            .init_resource::<crate::go_templates::GameObjectTemplates>()
            .init_resource::<crate::items::Items>()
            .init_resource::<crate::world_state::WorldStates>()
            .add_message::<TeleportMessage>()
            .add_message::<SelfMoveMessage>()
            .add_message::<SpeedChangeMessage>()
            .add_message::<MoveModeMessage>()
            .add_message::<ServerSoundMessage>()
            .add_message::<EmoteMessage>()
            .add_message::<AiReactionMessage>()
            .add_message::<WorldportMessage>()
            .add_message::<CharListMessage>()
            .add_message::<CharActionResultMessage>()
            .add_message::<EnteredWorldMessage>()
            .add_message::<ServerSaidMessage>()
            .add_message::<LoggedOutMessage>()
            .add_message::<LoginStageMessage>()
            .add_message::<LoginFailedMessage>()
            .add_message::<DisconnectedMessage>()
            .add_systems(
                Update,
                (
                    apply_net_updates,
                    tag_self_player,
                    sample_splines,
                    // Derive each creature's swim state from the water over its feet (the wire never
                    // carries it for creatures) — before the clamp, so a swimmer is exempt same-frame.
                    mark_swimming_creatures,
                    // After `sample_splines` writes the raw spline Z: re-ground walking units onto our
                    // terrain (the client discards a ground spline's Z — decision 0059); a swimming
                    // creature keeps its wire Z (its path runs through the water volume).
                    ground_clamp_creatures,
                    // Fire due scheduled relays (decision 0601) before the extrapolator advances
                    // the freshly-applied state and reconcile-lerps toward the next queued head.
                    drain_pending_moves,
                    extrapolate_remote_units,
                    // Idle creatures turn to face their target here — after the movers (spline / remote)
                    // have set their poses, so a face turn reads the target's fresh position this frame.
                    face_target,
                )
                    .chain()
                    .in_set(WorldStage::Net),
            )
            // Not part of the movement chain above: one send on the world-enter message.
            .add_systems(Update, send_query_time.in_set(WorldStage::Net));
    }
}

// ── ECS state: components ────────────────────────────────────────────────────────────────────────

/// The server guid of a streamed entity. The one stable identity for a unit/player/GameObject.
#[derive(Component, Clone, Copy)]
pub(crate) struct Guid(pub(crate) u64);

/// A streamed entity's network-authoritative identity: its coarse kind + model display id. The
/// renderer ([`crate::entities`]) attaches a visual from these; the entity's [`Transform`] is its pose.
#[derive(Component)]
pub(crate) struct NetEntity {
    pub(crate) kind: EntityKind,
    pub(crate) display_id: Option<u32>,
    /// `OBJECT_FIELD_SCALE_X` — the unit/object's *complete* render scale: the server already folds
    /// the DBC scale (`CreatureModelData.modelScale × CreatureDisplayInfo.scale`, or a per-spawn
    /// override) into it, so the renderer ([`crate::entities`]) bakes this onto the transform *alone*,
    /// never times its own DBC scale (that would double-apply). `1.0` is the default (no rescale).
    /// A *live* SCALE_X change (a values delta on an existing unit) updates this and eases the
    /// render scale over 2 s with a cosine smoothstep — the reference's own transition
    /// (byte-verified, `0x614bbf`; the code once misread as a selection fade-in) — through
    /// `entities::live_display` (decision 0695), which likewise applies a live
    /// `UNIT_FIELD_DISPLAYID` change (druid forms, GM morphs — the old F04 deferral).
    pub(crate) scale: f32,
}

/// A unit's movement speeds (yd/s), decoded from its `LIVING` movement block. The creature animation
/// selector ([`crate::creature_anim`]) keys its walk-vs-run boundary on `walk` (run above 2× walk —
/// RF-0057); [`extrapolate_remote_units`] integrates a remote mover between packets at `run` /
/// `run_back` / `swim` and turns it in place at `turn_rate`. Present only on units (GameObjects don't
/// move under their own power).
#[derive(Component, Clone, Copy)]
pub(crate) struct UnitSpeeds(pub(crate) MoveSpeeds);

/// A streamed object's descriptor field set (`UpdateFields`) — the ECS's mirror of the object's
/// server-side values array (decision 0061). Seeded from the create mask and [`ObjectFields::merge`]d
/// with each `Values` delta, so it accumulates the object's full descriptor state over its life.
/// Consumers read named accessors on the inner [`ObjectFields`]: `unit_health`/`unit_max_health` for the
/// Death selector + the inspector, `unit_race`/`unit_gender` + the `player_*` customization for the
/// character compositor. Speeds and pose are **not** here — they're movement-block data ([`UnitSpeeds`]
/// + the `Transform`), not descriptor fields.
#[derive(Component, Clone, Default)]
pub(crate) struct ObjectStore(pub(crate) ObjectFields);

/// Marks our own player's streamed entity (guid == [`SelfGuid`]). The renderer skips it — the player
/// controller owns our avatar — but the controller reads its [`Transform`] to take control on login.
#[derive(Component)]
pub(crate) struct SelfPlayer;

// ── ECS state: resources ─────────────────────────────────────────────────────────────────────────

/// The inbound event channel — drained each frame by [`apply_net_updates`].
#[derive(Resource)]
struct NetEvents(Receiver<SessionEvent>);

/// The outbound command channel — cloned by the player/chat systems to send movement + chat.
#[derive(Resource)]
pub(crate) struct NetCommands(pub(crate) Sender<ClientCommand>);

/// The character-management channel: the app's answer to each [`CharListMessage`] while the IO
/// thread is parked at select. [`CharRequest::Enter`] picks a character to `CMSG_PLAYER_LOGIN` as
/// (decision 0193); [`CharRequest::Create`]/[`CharRequest::Delete`] are serviced *in place* by the
/// parked loop, which stays parked (re-enum, re-emit the roster, emit a [`SessionEvent::CharActionResult`])
/// until an `Enter` moves the session into the world (decision 0423). Sent by [`crate::char_select`]'s
/// pick policy and the char-create screen; the parked IO read thread blocks on the other end.
#[derive(Resource)]
pub(crate) struct CharPick(pub(crate) Sender<CharRequest>);

/// One request to the parked IO thread over the [`CharPick`] channel (decision 0423). `Delete`'s
/// wire + service path is live (proven by `WOW_PROBE_CHARCREATE`); its UI affordance is a later
/// slice (decision 0423's deferrals).
pub(crate) enum CharRequest {
    /// Log in as this character (`CMSG_PLAYER_LOGIN`) — the session leaves select for the world.
    Enter(u64),
    /// Create a character (`CMSG_CHAR_CREATE`), then stay parked at select with the fresh roster.
    Create(benilla_protocol::CharCreateReq),
    /// Delete a character by guid (`CMSG_CHAR_DELETE`), then stay parked at select.
    Delete(u64),
    /// Select's Back (decision 0539): drop the parked session and return the IO thread to the
    /// pre-logon park — the app is heading to the login screen.
    Abandon,
}

/// The credentials channel (decision 0539): the app's answer to the IO thread's **pre-logon park**.
/// Sent by [`crate::login`]'s policy (the screen's submit, the env fast path, the reconnect
/// resubmit); the parked read thread blocks on the other end.
#[derive(Resource)]
pub(crate) struct LoginSubmit(pub(crate) Sender<io::LoginRequest>);

/// The login abandon generation (decision 0539): Cancel bumps it; each [`io::LoginRequest`] carries
/// the value read at submit, and the IO thread discards an attempt whose value has been passed.
#[derive(Resource)]
pub(crate) struct LoginAbandon(pub(crate) std::sync::Arc<std::sync::atomic::AtomicU64>);

/// guid → spawned ECS entity, for O(1) lookup on move/remove. Maintained solely by
/// [`apply_net_updates`]; read-only to everyone else (the merchant range-close resolves its vendor
/// through it).
#[derive(Resource, Default)]
pub(crate) struct GuidIndex(pub(crate) HashMap<u64, Entity>);

/// Our own player's guid, once the IO thread reports we're in the world. Used to tag
/// [`SelfPlayer`], and read by the combat-text emitters' source-ownership classifier
/// (`crate::combat_text::melee_impact_text` — the "mine" test against Summoned/CreatedBy).
#[derive(Resource, Default)]
pub(crate) struct SelfGuid(pub(crate) Option<u64>);

/// Connection status for the rest of the app (decision 0065): `connected` flips on
/// `Connected`/`Disconnected`; `last_reason` keeps the most recent failure so it can be surfaced
/// (debug panel, future UI). The streamed-world teardown itself happens in [`apply_net_updates`].
#[derive(Resource, Default)]
pub(crate) struct NetStatus {
    pub(crate) connected: bool,
    pub(crate) last_reason: Option<String>,
    /// The last measured ping round trip (ms), from the `SMSG_PONG` echo against [`PingShared`]'s
    /// clock. `None` until the first pong of a connection (and again after a disconnect).
    pub(crate) latency_ms: Option<u32>,
    /// The most recent [`RTT_RING`] round trips (ms), oldest first — the real client's own RTT
    /// history (wow-re net W1: `HandlePong 0x537d60` puts each sample into a ring with "head/tail
    /// wrap 16", which `0x537f20`'s avg-RTT math averages). [`Self::avg_latency_ms`] is what the UI
    /// reports through `GetNetStats`; the *last* sample stays separate because that is the one the
    /// wire echoes back in the next `CMSG_PING`'s lastRtt field. Cleared with the connection.
    pub(crate) rtt_ring: VecDeque<u32>,
}

/// How many round trips [`NetStatus::rtt_ring`] keeps — the reference ring's depth (wow-re net W1,
/// `HandlePong 0x537d60`: "head/tail wrap 16").
pub(crate) const RTT_RING: usize = 16;

impl NetStatus {
    /// Record one measured round trip: the newest sample in, the oldest out at depth.
    pub(crate) fn record_rtt(&mut self, ms: u32) {
        self.latency_ms = Some(ms);
        if self.rtt_ring.len() == RTT_RING {
            self.rtt_ring.pop_front();
        }
        self.rtt_ring.push_back(ms);
    }

    /// Forget this connection's measurements — the next one's latency is its own.
    pub(crate) fn clear_rtt(&mut self) {
        self.latency_ms = None;
        self.rtt_ring.clear();
    }

    /// The reported latency: the mean of the ring, truncated — `None` while it is empty (no pong
    /// since the connection came up). The reference averages its ring rather than reporting the
    /// last sample; the exact rounding is ours (`0x537f20`'s math is recorded as "avg-RTT" without
    /// a byte-level form), and at a 30 s ping cadence one sample's rounding is invisible anyway.
    pub(crate) fn avg_latency_ms(&self) -> Option<u32> {
        if self.rtt_ring.is_empty() {
            return None;
        }
        let sum: u64 = self.rtt_ring.iter().map(|&ms| u64::from(ms)).sum();
        Some((sum / self.rtt_ring.len() as u64) as u32)
    }
}

#[cfg(test)]
mod rtt_tests {
    use super::{NetStatus, RTT_RING};

    /// The reported latency is the ring's mean, and the ring is bounded at the reference depth —
    /// so a single spike moves the meter by a sixteenth, not the whole way (which is the point of
    /// averaging at all: the ping cadence is 30 s, and one bad sample must not sit on a red bar
    /// for eight minutes).
    #[test]
    fn the_reported_latency_is_the_mean_of_a_bounded_ring() {
        let mut status = NetStatus::default();
        assert_eq!(status.avg_latency_ms(), None, "no pong yet");

        status.record_rtt(40);
        assert_eq!(status.avg_latency_ms(), Some(40));
        status.record_rtt(60);
        assert_eq!(status.avg_latency_ms(), Some(50), "the mean, not the last");

        // Fill past the ring: only the newest RTT_RING samples count, so the two above age out.
        for _ in 0..RTT_RING {
            status.record_rtt(100);
        }
        assert_eq!(status.rtt_ring.len(), RTT_RING);
        assert_eq!(status.avg_latency_ms(), Some(100));
        assert_eq!(
            status.latency_ms,
            Some(100),
            "the last sample stays separate"
        );

        status.clear_rtt();
        assert_eq!(status.avg_latency_ms(), None, "a disconnect forgets it all");
        assert_eq!(status.latency_ms, None);
    }
}

/// The ping clock shared with the write thread's 30 s keepalive sender ([`io::PingClock`]): the
/// apply drain matches each `SMSG_PONG` echo against it to measure the round trip (stored back for
/// the next ping's lastRtt field, and surfaced in [`NetStatus::latency_ms`]).
#[derive(Resource)]
pub(crate) struct PingShared(pub(crate) std::sync::Arc<std::sync::Mutex<io::PingClock>>);

/// The dropped-packet tally — the wire-coverage instrument (decision 0022). Every server packet the
/// codec dropped on the floor lands here, keyed by opcode: `unknown` counts opcodes with **no parse
/// arm at all** (`ServerPacket::Other`), `unparseable` counts packets whose parser errored (skipped
/// to keep the stream aligned). Fed by [`apply_net_updates`] from `SessionEvent::PacketDropped`;
/// read by the debug panel's Net section. Never cleared on reconnect — the tally is about coverage,
/// not session state.
#[derive(Resource, Default)]
pub(crate) struct DroppedOpcodes(pub(crate) HashMap<u16, DropTally>);

/// Per-opcode drop counts (see [`DroppedOpcodes`]).
#[derive(Default, Clone, Copy)]
pub(crate) struct DropTally {
    pub(crate) unknown: u64,
    pub(crate) unparseable: u64,
}

/// The server in-game clock sample (`SMSG_LOGIN_SETTIMESPEED`), advanced by its timescale. `None`
/// until the first time packet. Read by the lighting subsystem to drive time-of-day.
#[derive(Resource, Default)]
pub(crate) struct ServerTime(pub(crate) Option<GameTime>);

/// Publish the session clock into the engine's [`benilla_world::lighting::WorldTime`] input, once a
/// frame. The renderer wants three scalars, not a wire sample with an `Instant` in it — and with
/// no server at all it wants noon, which is `WorldTime`'s own default rather than a branch here.
pub(crate) fn publish_world_time(
    server: Res<ServerTime>,
    mut world_time: ResMut<benilla_world::lighting::WorldTime>,
) {
    *world_time = match server.0 {
        Some(gt) => benilla_world::lighting::WorldTime {
            minute: gt.minute_of_day(),
            minute_f: gt.minute_of_day_f32(),
            day: gt.day_continuous(),
            live: true,
        },
        None => benilla_world::lighting::WorldTime::default(),
    };
}

/// The server's **wall clock** (`SMSG_QUERY_TIME_RESPONSE`, asked for on entering the world),
/// advanced monotonically from the sample. `None` until the first answer lands.
///
/// Not [`ServerTime`] just above: that is the *in-game* day/night clock the lighting reads, a
/// different quantity in a different unit which says nothing about the epoch here. This one is
/// unix-epoch seconds, and it exists because the server writes **absolute** stamps in that epoch
/// into descriptor fields — a timed quest's deadline is `time(nullptr) + limitTime` (vmangos
/// `Player::AddQuest`) and no packet ever restates it as a duration. So a countdown is
/// `deadline − now_unix()`, and the local machine's own wall clock is deliberately never consulted:
/// a player whose clock is a minute off would see every countdown a minute wrong, silently and only
/// on their screen (decision 1150).
#[derive(Resource, Default)]
pub(crate) struct ServerWallClock(pub(crate) Option<WallClockSample>);

/// One `SMSG_QUERY_TIME_RESPONSE` sample plus the monotonic instant it arrived — the [`GameTime`]
/// pattern, for the same reason: the clock advances between packets without re-reading a local wall
/// clock that an NTP step or a manual change can move under us.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WallClockSample {
    /// The server's `time(nullptr)` at `received`.
    base_unix: u32,
    /// When the answer arrived (monotonic).
    received: Instant,
}

impl ServerWallClock {
    /// Take a fresh sample (the `SMSG_QUERY_TIME_RESPONSE` handler).
    ///
    /// The reply is one round trip old, so the clock reads ~RTT/2 behind the server's; against a
    /// countdown displayed to the second that is below the noise floor, which is why there is no
    /// correction term here.
    pub(crate) fn sample(&mut self, unix_time: u32) {
        self.0 = Some(WallClockSample {
            base_unix: unix_time,
            received: Instant::now(),
        });
    }

    /// Server unix seconds now (fractional), or `None` before the first sample.
    pub(crate) fn now_unix(&self) -> Option<f64> {
        self.0
            .map(|s| f64::from(s.base_unix) + s.received.elapsed().as_secs_f64())
    }

    /// Whether the sample is old enough to re-ask (or absent entirely).
    fn stale(&self) -> bool {
        self.0.is_none_or(|s| s.received.elapsed() >= RESYNC_AFTER)
    }
}

/// How long a wall-clock sample stands before the client asks again — the reference's own hour
/// (wow-re: the resync site `0x4de836` is gated on `now > [0xbb749c]`, armed `= now + 0xe10` at
/// `0x4def11`; decision 1154).
const RESYNC_AFTER: Duration = Duration::from_secs(3600);

/// Ask for the server's wall clock (decision 1150) — on entering the world (login, worldport and
/// instance transfer alike, the same cascade the mail arc's `QueryNextMailTime` hangs off), and
/// hourly thereafter.
///
/// The hourly leg is the reference's, and it is not about our own drift: a monotonic base does not
/// wander against a wall clock at any rate a countdown could show. What it tracks is the **server**
/// being re-clocked under us — the one direction no local reasoning can see — for the price of four
/// bytes an hour (decision 1154). It also self-heals a session that entered the world before the
/// socket could answer.
fn send_query_time(
    mut entered: MessageReader<EnteredWorldMessage>,
    commands: Res<NetCommands>,
    clock: Res<ServerWallClock>,
    status: Res<NetStatus>,
    mut asked_at: Local<Option<Instant>>,
) {
    let entering = entered.read().next().is_some();
    // Only chase the cadence while a session exists — a command with no live writer evaporates
    // with a warn, and the world-enter send covers the reconnect.
    let due =
        status.connected && clock.stale() && asked_at.is_none_or(|t| t.elapsed() >= RESYNC_AFTER);
    if entering || due {
        *asked_at = Some(Instant::now());
        let _ = commands.0.send(ClientCommand::QueryTime);
    }
}

/// Our player's reputation store (`SMSG_INITIALIZE_FACTIONS`, once at login): `(flags, standing)`
/// per reputation-list slot, indexed by `Faction.dbc`'s `reputationIndex`. The standing excludes the
/// DBC race/class base. Read by the reaction decode (targeting): a reputation faction's NPCs colour
/// by our rank with them, before any faction-template comparison. Empty until the packet lands.
#[derive(Resource, Default)]
pub(crate) struct Reputations(pub(crate) Vec<(u8, i32)>);

/// The player's hearthstone bind point (`SMSG_BINDPOINTUPDATE`, at login + on re-bind): the
/// AreaTable id the hearthstone tooltip's `$z` token names ("Returns you to Goldshire.").
/// `None` until the packet lands.
#[derive(Resource, Default)]
pub(crate) struct HomeBind(pub(crate) Option<u32>);

/// The player's equip proficiencies (`SMSG_SET_PROFICIENCY`, at login + on train): item class
/// (2 weapons / 4 armor) → allowed-subclass bitmask — the client's `0xc4d4a0[class]` store.
/// The item tooltip's slot-line red compares against it. Empty until the packets land.
#[derive(Resource, Default)]
pub(crate) struct Proficiencies(pub(crate) std::collections::HashMap<u32, u32>);

/// The server's in-game clock, from `SMSG_LOGIN_SETTIMESPEED`. WoW packs the server's `localtime()`
/// into the `DateTime`, so the in-game hour:minute follows the server's wall clock; `timescale` is how
/// many game-minutes pass per real second (vmangos ≈ `0.01667` → real-time). We keep the sample plus
/// the [`Instant`] it arrived so the clock advances between packets.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GameTime {
    /// Minute of the game day (`0..1440`) at `received`.
    base_minute: u32,
    /// Monotonic day serial at `received` (`year·372 + month·31 + day` of the packed server date —
    /// the packed convention's fixed 31-day months). Only differences matter: it feeds the
    /// celestial moon-phase precession (`(dayCounter + todPhase) mod 1.7`, decision 0485).
    base_day: u32,
    /// Game-minutes elapsed per real second.
    timescale: f32,
    /// When this sample arrived (monotonic; used to advance the clock).
    received: Instant,
}

impl GameTime {
    /// Build from a decoded `SMSG_LOGIN_SETTIMESPEED` (server `hours`/`minutes`/`day_serial` +
    /// `timescale`).
    pub(crate) fn new(hours: u8, minutes: u8, day_serial: u32, timescale: f32) -> Self {
        Self {
            base_minute: hours as u32 * 60 + minutes as u32,
            base_day: day_serial,
            timescale,
            received: Instant::now(),
        }
    }

    /// Current minute of the game day (`0..1440`), advanced from the sample by `timescale`.
    pub(crate) fn minute_of_day(&self) -> u32 {
        self.minute_of_day_f32() as u32
    }

    /// Current **fractional** minute of the game day (`0.0..1440.0`), advanced from the sample by
    /// `timescale` — same value as [`Self::minute_of_day`] but without truncation. Drives the celestial
    /// body *positions* continuously (sampling at the truncated integer minute steps them once per
    /// game-minute, a visible jump at a fast timescale).
    pub(crate) fn minute_of_day_f32(&self) -> f32 {
        let elapsed = self.received.elapsed().as_secs_f32() * self.timescale;
        (self.base_minute as f32 + elapsed).rem_euclid(1440.0)
    }

    /// Continuous **days + day-fraction** since the day-serial epoch, advanced by `timescale` —
    /// UNWRAPPED (it crosses midnight by growing past the next integer), in `f64` (the serial
    /// reaches ~12k and the consumer needs sub-minute resolution on top). Feeds the celestial
    /// moon-phase `(dayCounter + todPhase) mod 1.7` (`0x6d41b9`; decision 0485).
    pub(crate) fn day_continuous(&self) -> f64 {
        let elapsed = self.received.elapsed().as_secs_f64() * self.timescale as f64;
        self.base_day as f64 + (self.base_minute as f64 + elapsed) / 1440.0
    }
}

// ── Outbound commands + one-shot inbound events ──────────────────────────────────────────────────

/// One self-movement event the controller streams to the server. Each maps to a `MSG_MOVE_*` opcode
/// ([`Self::opcode`]); the writer thread attaches the current `MovementInfo`. This mirrors the real
/// client: it announces each movement-*axis* transition (start/stop forward-back, strafe, turn), a
/// jump, a facing change (mouse-turn while standing), and a periodic heartbeat while moving — so the
/// server relays a faithful movement stream to nearby players.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MoveKind {
    StartForward,
    StartBackward,
    Stop,
    StartStrafeLeft,
    StartStrafeRight,
    StopStrafe,
    StartTurnLeft,
    StartTurnRight,
    StopTurn,
    Jump,
    FallLand,
    StartSwim,
    StopSwim,
    SetFacing,
    Heartbeat,
}

impl MoveKind {
    /// The wire opcode this event sends.
    pub(crate) fn opcode(self) -> u16 {
        use benilla_protocol::messages::opcode as op;
        match self {
            MoveKind::StartForward => op::MSG_MOVE_START_FORWARD,
            MoveKind::StartBackward => op::MSG_MOVE_START_BACKWARD,
            MoveKind::Stop => op::MSG_MOVE_STOP,
            MoveKind::StartStrafeLeft => op::MSG_MOVE_START_STRAFE_LEFT,
            MoveKind::StartStrafeRight => op::MSG_MOVE_START_STRAFE_RIGHT,
            MoveKind::StopStrafe => op::MSG_MOVE_STOP_STRAFE,
            MoveKind::StartTurnLeft => op::MSG_MOVE_START_TURN_LEFT,
            MoveKind::StartTurnRight => op::MSG_MOVE_START_TURN_RIGHT,
            MoveKind::StopTurn => op::MSG_MOVE_STOP_TURN,
            MoveKind::Jump => op::MSG_MOVE_JUMP,
            MoveKind::FallLand => op::MSG_MOVE_FALL_LAND,
            MoveKind::StartSwim => op::MSG_MOVE_START_SWIM,
            MoveKind::StopSwim => op::MSG_MOVE_STOP_SWIM,
            MoveKind::SetFacing => op::MSG_MOVE_SET_FACING,
            MoveKind::Heartbeat => op::MSG_MOVE_HEARTBEAT,
        }
    }
}

/// The wire `ChatMsg` type byte an addon broadcast rides (decision 1235) — the client's own
/// four-lane whitelist at `0x49fa3f`-`0x49fa4e`, VERIFIED in `WoW.exe` (5875), wow-re
/// `system/ui/scratch/addon-chat-law.md` §5.
///
/// A **total** map from a closed enum, and a named function rather than an inline `match` in
/// [`io`]'s dispatch so that it is assertable: this is the one place a distribution becomes a wire
/// byte, and getting it wrong sends an addon's payload down a lane nobody is listening on. There
/// is no fallback arm and none can be added — that is the whole reason the distribution crosses
/// the crate boundary as an enum instead of a token string.
pub(crate) fn addon_wire_chat_type(distribution: benilla_ui::script::AddonDistribution) -> u32 {
    use benilla_protocol::messages as m;
    use benilla_ui::script::AddonDistribution as D;
    match distribution {
        D::Party => m::CHAT_TYPE_PARTY,
        D::Raid => m::CHAT_TYPE_RAID,
        D::Guild => m::CHAT_TYPE_GUILD,
        D::Battleground => m::CHAT_TYPE_BATTLEGROUND,
    }
}

/// The wire `ChatMsg` type a chat-bar line sends as — the FULL sendable set (decision 0288 P5;
/// vmangos `HandleChatMessageOpcode`'s switch). Each maps to its `WorldWriter` sender in [`io`]'s
/// dispatch; `Whisper` carries its target and `Channel` its channel name through
/// [`ClientCommand::Chat`]'s `target` field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ChatKind {
    Say,
    Yell,
    Emote,
    Whisper,
    Party,
    Raid,
    RaidLeader,
    RaidWarning,
    Guild,
    Officer,
    Battleground,
    BattlegroundLeader,
    Afk,
    Dnd,
    Channel,
}

/// Diagnostic gate (`WOW_CAST_TRACE=1`): log our own cast lifecycle — every self `SMSG_SPELL_START`
/// / `GO` / `CAST_RESULT` / `SPELL_FAILED_OTHER` / `SPELL_DELAYED` — **and** every outbound movement
/// packet, at info. A live "the cast bar vanished when I got hit" repro then shows exactly which
/// packet ended the cast (a pushback that should extend vs an interrupt that cancels) and whether a
/// movement packet preceded it — vmangos interrupts a cast on any movement report whose position
/// differs from the server's stored one (`Player::SetPosition`'s exact-float `positionChanged`). A
/// reusable instrument, off by default (decision 0022 — own the instruments).
pub(crate) static CAST_TRACE: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| std::env::var("WOW_CAST_TRACE").is_ok());

/// A message the ECS sends to the server through the write thread. Carries the player's pose so the
/// writer thread needs no shared state.
pub(crate) enum ClientCommand {
    /// A self-movement packet: the [`MoveKind`] opcode + the live CMovement `flags` + our pose, plus the
    /// airborne clock (`fall_time`, ms) and ballistic launch tail (`jump`) while jumping/falling. The
    /// `flags` set the base directional/turn/walk bits, `JUMPING` while airborne, and `ON_TRANSPORT`
    /// while standing on a boat/zepp (with its `transport` local-frame tail — decision 0438 phase 2);
    /// the writer serializes each tail iff its flag is set (a flag without its tail desyncs the
    /// server's parse).
    Move {
        kind: MoveKind,
        flags: u32,
        pos: [f32; 3],
        orientation: f32,
        /// Swim pitch (radians, +up) — written iff `SWIMMING` is in `flags`; `0` otherwise.
        pitch: f32,
        fall_time: u32,
        jump: Option<JumpInfo>,
        /// The rider's pose in the transport's local frame — written iff `ON_TRANSPORT` is in `flags`.
        transport: Option<TransportPose>,
    },
    /// Acknowledge a finished server-authored spline that drove our own player (Charge, and later
    /// knockback/taxi): `CMSG_MOVE_SPLINE_DONE` with our pose at the ride's endpoint and the
    /// `spline_id` we were driven by. The server holds a player mover as spline-pending until this
    /// arrives, so the controller sends it the instant the ride ends, then resumes its own stream.
    MoveSplineDone {
        flags: u32,
        pos: [f32; 3],
        orientation: f32,
        spline_id: u32,
    },
    /// Answer a `SMSG_FORCE_*_SPEED_CHANGE` (`CMSG_FORCE_*_SPEED_CHANGE_ACK`): echo the mover
    /// `guid`/`counter`/exact `speed`, carrying the same live wire payload a [`Self::Move`] would
    /// (the server relocates us to it and its anticheat runs the same position tests). Mandatory —
    /// unacked, the server force-resolves after ~4 s and cheat-flags the session. Sent by the
    /// controller (which owns the live pose) off a [`SpeedChangeMessage`].
    ForceSpeedAck {
        kind: SpeedKind,
        guid: u64,
        counter: u32,
        speed: f32,
        flags: u32,
        pos: [f32; 3],
        orientation: f32,
        pitch: f32,
        fall_time: u32,
        jump: Option<JumpInfo>,
        transport: Option<TransportPose>,
    },
    /// Echo a same-map teleport ack (`MSG_MOVE_TELEPORT_ACK_Client`) — without it the server freezes
    /// our movement until relog.
    TeleportAck { guid: u64, counter: u32 },
    /// Echo a cross-map worldport ack (`MSG_MOVE_WORLDPORT_ACK`) — unblocks the new map's stream.
    WorldportAck,
    /// Set (or clear, with `guid == 0`) our current target on the server (`CMSG_SET_SELECTION`).
    SetSelection { guid: u64 },
    /// Send a chat line as the given [`ChatKind`] (`CMSG_MESSAGECHAT`). `target` carries the
    /// whisper recipient's name — meaningful only when `kind == ChatKind::Whisper` — and is
    /// `None` otherwise. Plain lines with no leading `/` (including `.` dot commands, which
    /// vmangos parses as GM commands on the way in) ride as `ChatKind::Say`, unchanged from
    /// before slash commands existed. Built by [`crate::ui_chat`]'s command parser.
    Chat {
        kind: ChatKind,
        target: Option<String>,
        text: String,
    },
    /// **An addon broadcast** (`SendAddonMessage`, decision 1235) — a `CMSG_MESSAGECHAT` carrying
    /// `LANG_ADDON` in the language field instead of the speaker's tongue, which is the entire
    /// difference between addon data and speech (1.12.1 has no addon opcode).
    ///
    /// Deliberately **not** a flag on [`Self::Chat`]: that command's whole family speaks
    /// `WorldWriter::chat_language`, and a language field bolted onto it would be a `None` that
    /// fourteen arms have to remember to honour. This one carries no target (1.12's
    /// `SendAddonMessage` has no fourth argument, so there is no whispered addon message) and no
    /// language dial (there is exactly one legal value).
    ///
    /// `distribution` is the client's own four-value whitelist, already resolved and already
    /// downgraded for the outside-a-raid case by the binding
    /// ([`benilla_ui::script::AddonDistribution`]); `text` is the composed `prefix` TAB `message`.
    AddonMessage {
        distribution: benilla_ui::script::AddonDistribution,
        text: String,
    },
    /// Ask a player character's name (`CMSG_NAME_QUERY`); answered by a `PlayerName` event into the
    /// [`crate::names::NameCache`]. Sent by the cache's ask-once resolve, never directly.
    NameQuery { guid: u64 },
    /// Ask a creature template's name (`CMSG_CREATURE_QUERY`, entry from the guid's bits 24–47);
    /// answered by a `CreatureName` event into the cache.
    CreatureQuery { entry: u32, guid: u64 },
    /// Ask a pet's name (`CMSG_PET_NAME_QUERY`, pet number from the guid's bits 24–47 — where a
    /// creature keeps its template entry); answered by a `PetName` event into the cache. A pet
    /// cannot be named by [`Self::CreatureQuery`]; see [`benilla_protocol::guid::pet_number`].
    PetNameQuery { pet_number: u32, guid: u64 },
    /// Ask an item template (`CMSG_ITEM_QUERY_SINGLE`; `guid` = the concrete item when the ask is
    /// about one, `0` for template-only); answered by an `ItemTemplate` event into the
    /// [`crate::items::Items`] cache. Sent by the cache's ask-once resolve, never directly.
    ItemQuery { entry: u32, guid: u64 },
    /// Use an item by wire bag position (`CMSG_USE_ITEM` — bag 255 + absolute slot for anything
    /// in the player's own field array, i.e. an equipment slot, the backpack or the keyring; a
    /// bag's own player-array slot 19–22 + inner slot otherwise). The container drain maps the
    /// Lua `(bagID, slot)` space before sending. `spell_index` is the template block ordinal the
    /// server should cast (`ItemInfo::use_spell_index`) — 0 for every item whose on-use spell is
    /// its first block, which is nearly all of them.
    ///
    /// `target` is the cast-targets block, bound by the one cast ladder exactly as a
    /// [`Self::CastSpell`]'s is — the real client's `SendCast 0x6e54f0` writes one block for both
    /// opcodes. `Object` is the key-in-a-lock arm (decision 0769); `Unit` a bandage/soulstone;
    /// `SelfImplicit` every ordinary consumable.
    UseItem {
        bag_index: u8,
        slot: u8,
        spell_index: u8,
        target: benilla_protocol::messages::UseItemTarget,
    },
    /// Open a bag item (`CMSG_OPEN_ITEM`, same bag addressing) — the drain's fork for an
    /// *openable* click (`ItemInfo::openable`): a clam, an unlocked lockbox, a wrapped gift. The
    /// server answers `SMSG_LOOT_RESPONSE` on the **item's own guid**, so the ordinary loot feed
    /// opens a window over a thing in the bag; a wrapped gift instead swaps entry in place and
    /// sends no window. Refusals arrive as `InventoryFailure` (still locked, dead, flying).
    OpenItem { bag_index: u8, slot: u8 },
    /// Equip a bag item (`CMSG_AUTOEQUIP_ITEM`, same bag addressing) — the drain's fork for an
    /// *equippable* click, mirroring the real client's equip-vs-use decision. Refusals come back
    /// as `InventoryFailure` events onto the UI error line.
    AutoEquipItem { bag_index: u8, slot: u8 },
    /// Load ammo into the ammo slot (`CMSG_SET_AMMO`) — the equip drains' fork when the clicked/
    /// dropped item is ammo-class (INVTYPE_AMMO), mirroring the real client's own auto-equip fork
    /// (wow-re `cursor-dragdrop-slots.md`). Addressed by item `entry`, not a bag slot: the stack
    /// stays in the bag and `PLAYER_AMMO_ID` starts referencing it. A wrong/absent ranged weapon
    /// refuses via `InventoryFailure`. Decision 0526.
    SetAmmo { entry: u32 },
    /// Swap two of the player's own inventory slots (`CMSG_SWAP_INV_ITEM`) — the wire for a
    /// backpack-internal pick/place/swap, both slots on the player array (`INVENTORY_SLOT_ITEM_START`
    /// onward). The container-move drain maps the Lua backpack `(bag 0, slot)` space onto these; an
    /// empty destination is a move. Refusals surface as `InventoryFailure` events.
    SwapInvItem { src_slot: u8, dst_slot: u8 },
    /// The general bag↔bag move (`CMSG_SWAP_ITEM`, decision 0216 §6 slice 2) — either endpoint may
    /// be an equipped bag (unlike [`ClientCommand::SwapInvItem`], the player-array-only wire). The
    /// container-move drain sends this instead of `SwapInvItem` whenever either end's wire bag
    /// isn't [`benilla_protocol::messages::BAG_PLAYER_INVENTORY`] — VERIFIED vmangos
    /// `Packets/Item.cpp:30-36`: body order is dstbag, dstslot, srcbag, srcslot (opcode 0x10C).
    /// Refusals surface as `InventoryFailure` events.
    SwapItem {
        dst_bag: u8,
        dst_slot: u8,
        src_bag: u8,
        src_slot: u8,
    },
    /// Carry a partial stack from one bag position to another (`CMSG_SPLIT_ITEM`, decision 0216
    /// §6) — either endpoint may be an equipped bag, unlike [`ClientCommand::SwapInvItem`]. The
    /// container-move drain sends this instead of a swap when the queued move carries a split
    /// count. Refusals surface as `InventoryFailure` events.
    SplitItem {
        src_bag: u8,
        src_slot: u8,
        dst_bag: u8,
        dst_slot: u8,
        count: u8,
    },
    /// Destroy a bag item (`CMSG_DESTROYITEM`, decision 0216 §3): `count` 0 = the whole stack.
    /// Sent by the delete-confirm popup's accept (`DeleteCursorItem`), mapped from the engine's
    /// queued `(bag, slot, count)` destroy through the same wire-position map as every other
    /// container drain.
    DestroyItem { bag_index: u8, slot: u8, count: u8 },
    /// Perform a chat emote (`CMSG_TEXT_EMOTE`); the server echoes it back to us (and everyone
    /// in range) as `SMSG_TEXT_EMOTE`, so the local sound/anim ride the receive path.
    TextEmote { text_id: u32, target: u64 },
    /// Cast a spell (`CMSG_CAST_SPELL`): `target: None` = self/implicit-target cast, `Some(guid)`
    /// = explicit unit target. Answered by `SMSG_CAST_RESULT` (a `CastResult` event).
    CastSpell { spell_id: u32, target: Option<u64> },
    /// Cast a spell at a **ground point** (`CMSG_CAST_SPELL` with `TARGET_FLAG_DEST_LOCATION`,
    /// decision 0792): the targeting-cursor commit for a ground-targeted AOE. `dest` is the
    /// clicked world point in **WoW coords** (`bevy_to_wow` at the send site — the wire never
    /// sees Bevy space). Answered by `SMSG_CAST_RESULT`.
    CastSpellAtDest { spell_id: u32, dest: [f32; 3] },
    /// Cancel one of our own auras (`CMSG_CANCEL_AURA`, decision 0257): the right-click-a-buff wire,
    /// carrying the **spell id** (the server cancels by spell, not slot). No answer packet — the
    /// removal comes back as a `UNIT_FIELD_AURA` delta. Sent by the aura feed's cancel drain.
    CancelAura { spell_id: u32 },
    /// Set (or clear, `packed == 0`) one action-bar slot (`CMSG_SET_ACTION_BUTTON`, decision
    /// 0216 §7/0218 §4): `button` is the 0-based wire slot (lua action id − 1), `packed` the
    /// engine's own `kind<<24 | action` word. Sent by the action drain on every queued
    /// `PickupAction`/`PlaceAction` mutation — client-authoritative, no answer packet.
    SetActionButton { button: u8, packed: u32 },
    /// Press one pet bar slot (`CMSG_PET_ACTION`, decisions 0982/0988). `packed` is the slot's OWN
    /// word as the server last sent it — command, reaction and spell all ride this one command,
    /// because the server dispatches on the type byte inside the word. `target_guid` is the
    /// player's current selection, which is what the client always sends (wow-re §10.1).
    ///
    /// **Nothing answers it.** Unlike [`Self::SetActionButton`], whose silence is because the
    /// state is ours, this one is silent because the *server* simply does not reply — so the
    /// caller has already applied the visible change locally before queueing it.
    PetAction {
        pet_guid: u64,
        packed: u32,
        target_guid: u64,
    },
    /// Write pet bar slots (`CMSG_PET_SET_ACTION`, decision 0988) — `(0-based position, the whole
    /// new word)` pairs, one or two.
    ///
    /// **This is how the autocast toggle travels**, with one entry: the client flips bit 30 in the
    /// slot's word and posts the result, and the server reads the direction back out of the type
    /// byte it arrives in. `CMSG_PET_SPELL_AUTOCAST` (0x2F3) is a *different* binding's opcode —
    /// the pet spellbook's `ToggleSpellAutocast`, which indexes the spellbook rather than the bar
    /// and which we do not ship. The same send carries the drag when that lands.
    PetSetAction {
        pet_guid: u64,
        entries: Vec<(u32, u32)>,
    },
    /// Call the pet off its target (`CMSG_PET_STOP_ATTACK`) — the Attack button's second press.
    PetStopAttack { pet_guid: u64 },
    /// Cancel one of the **pet's** auras (`CMSG_PET_CANCEL_AURA`, decision 1007) — the pet bar's
    /// press-again-to-cancel, and the pet-shaped twin of [`Self::CancelAura`].
    ///
    /// It is a separate opcode rather than `CancelAura` with a guid because the server needs to
    /// know *whose* aura to drop, and `CMSG_CANCEL_AURA`'s body is a bare spell id. Like its
    /// player twin: no answer packet — the removal arrives as a `UNIT_FIELD_AURA` delta on the
    /// pet, which is also what puts the slot's icon back.
    PetCancelAura { pet_guid: u64, spell_id: u32 },
    /// Flip one pet **spellbook** entry's autocast (`CMSG_PET_SPELL_AUTOCAST` 0x2F3, decision
    /// 1032) — `ToggleSpellAutocast`'s send, and **not** the pet bar's autocast verb.
    ///
    /// The bar's right click is [`Self::PetSetAction`]: it rewrites a *slot's word*, so its body
    /// names a position. This one names a **spell id**, because the book has no positions —
    /// `0x4b4240` looks the id up in `0xb6f098` and hands `0x4bccb0` the id, never an index. Both
    /// end up flipping the same bit on the same word; only one of them can say which word by slot.
    /// No reply packet either way.
    PetSpellAutocast {
        pet_guid: u64,
        spell_id: u32,
        enabled: bool,
    },
    /// Give the pet up permanently (`CMSG_PET_ABANDON`, decision 1066) — the right-click menu's
    /// **Abandon**, and only that row.
    ///
    /// **Its Dismiss is not this**, however alike they read: `PetDismiss 0x4be4d0` opens no packet
    /// and goes down the pet bar's ordinary [`Self::PetAction`] path with the word `0x07000003`
    /// (wow-re §11c). Both would have worked against vmangos, which is why it is written here.
    ///
    /// No reply: the answer is `SMSG_PET_SPELLS` with a zero guid, and the pet object leaving.
    PetAbandon { pet_guid: u64 },
    /// Rename the pet (`CMSG_PET_RENAME`, decision 1066) — the `PETRENAMECONFIRM` popup's accept.
    ///
    /// The server may refuse the name outright, so nothing is applied locally; success arrives as a
    /// bumped `UNIT_FIELD_PET_NAME_TIMESTAMP` on the pet, which is what re-asks the name cache.
    PetRename { pet_guid: u64, name: String },
    /// Start melee auto-attack on `guid` (`CMSG_ATTACKSWING`); echoed as `SMSG_ATTACKSTART`.
    AttackSwing { guid: u64 },
    /// Stop melee auto-attack (`CMSG_ATTACKSTOP`); echoed as `SMSG_ATTACKSTOP`, whose receive path
    /// drops the attacker's [`crate::creature_anim::Engaged`] stance. Sent when the attack target
    /// is lost — Esc / click-off / target death (ref-observed: losing the target stops the swing
    /// and leaves the attack stance; the weapons stay drawn).
    AttackStop,
    /// Stop our ranged auto-repeat (`CMSG_CANCEL_AUTO_REPEAT_SPELL`, empty body) — the ack every
    /// local cancel sends (the client's sole send site `0x6ea0c6`, inside the cancel routine
    /// `0x6ea080`; wow-re `nocked-ammo-cancel.md`). vmangos interrupts the held auto-repeat
    /// spell; idempotent when the server already cancelled it first (the cast-result path).
    CancelAutoRepeat,
    /// Cancel a named in-flight cast (`CMSG_CANCEL_CAST`, one `u32` spell id). Sent by the
    /// wand-only auto-repeat handoff before its local cancel (`0x6095b8`, wow-re
    /// `nocked-ammo-cancel.md` §Q-B-5) and by the cast bar's local self-cancel (movement/Esc
    /// mid-cast — `ui_cast`'s mirror of the client's `AbortCast 0x6e4940` send leg).
    CancelCast { spell_id: u32 },
    /// End our own running channel (`CMSG_CANCEL_CHANNELLING`, one `u32` spell id the server
    /// reads and ignores — the real client still writes it). The channel half of the local
    /// self-cancel (`ui_cast`).
    CancelChannelling { spell_id: u32 },
    /// Volunteer our sheath state (`CMSG_SETSHEATHED`: 0 stowed · 1 melee · 2 ranged). The real
    /// client auto-sends `1` when initiating melee with weapons stowed (decision 0073, verified
    /// `0x5ecb70` → `0x611cf0`); it lands in our `UNIT_FIELD_BYTES_2`, which drives everyone's
    /// weapon placement — including our own, via the descriptor echo.
    SetSheathed { state: u32 },
    /// Volunteer our stand state (`CMSG_STANDSTATECHANGE`: 0 stand · 1 sit · 3 sleep · 8 kneel).
    /// The echo into `UNIT_FIELD_BYTES_1` byte 0 drives everyone's sit/stand pose — including our
    /// own, via the descriptor (decision 0080c: the same pattern as sheath).
    StandStateChange { state: u32 },
    /// The mounted space-bar flourish (`CMSG_MOUNTSPECIAL_ANIM`, empty body). The sender
    /// plays MountSpecial(94) on its own mount locally at send time; the broadcast echo —
    /// present or not by server config — is self-suppressed on receive (decision 0441 P2).
    MountSpecial,
    /// Open a gossip dialog on an NPC (`CMSG_GOSSIP_HELLO`, decision 0081 phase 2): the universal
    /// interact opener — the server's `CanInteractWithNPC` passes `UNIT_NPC_FLAG_NONE`, so it
    /// works on any interactable creature. Sent by the right-click interact route
    /// ([`crate::target`]); answered by `SMSG_GOSSIP_MESSAGE` (a `GossipMenu` event).
    GossipHello { guid: u64 },
    /// Choose a gossip menu option (`CMSG_GOSSIP_SELECT_OPTION`): `option` is the chosen line's
    /// echoed `index`. v1 never sends a password — coded options are greyed and never selected
    /// (decision 0081), so the writer always omits the trailing code. The server answers a fresh
    /// menu (`SMSG_GOSSIP_MESSAGE`) or closes it (`SMSG_GOSSIP_COMPLETE`).
    GossipSelectOption { guid: u64, option: u32 },
    /// Fetch a gossip menu's greeting text (`CMSG_NPC_TEXT_QUERY`) — auto-sent on menu receipt for
    /// its `text_id`; answered by `SMSG_NPC_TEXT_UPDATE` (an `NpcGreeting` event). Ask-once cached
    /// per `text_id`, like an item template (decision 0081 phase 3).
    NpcTextQuery { text_id: u32, guid: u64 },
    /// Open a vendor's stock (`CMSG_LIST_INVENTORY`, decision 0081 phase 2): the direct opener a
    /// right-click on a vendor-only NPC uses. Answered by `SMSG_LIST_INVENTORY` (a
    /// `VendorInventory` event; the merchant window is phase 4).
    ListInventory { guid: u64 },
    /// Buy from a vendor (`CMSG_BUY_ITEM`, decision 0081 phase 4): `entry` is the item **template**
    /// id (buy is by entry, not the vendor row's `muid`), `count` the number of stacks. Auto-places
    /// into the first free bag slot; success answers `SMSG_BUY_ITEM` + the item-create path, refusal
    /// `SMSG_BUY_FAILED` (a `VendorBuyFailed` event → the merchant error line).
    BuyItem { vendor: u64, entry: u32, count: u8 },
    /// Sell an item to a vendor (`CMSG_SELL_ITEM`, decision 0081 phase 4's sell affordance):
    /// `item_guid` is the concrete bag item, `count` 0 = the whole stack. Success is silent (the item
    /// vanishes + coinage rises via `UPDATE_OBJECT`); refusal answers `SMSG_SELL_ITEM`'s error shape
    /// (a `VendorSellFailed` event → the merchant error line).
    SellItem {
        vendor: u64,
        item_guid: u64,
        count: u8,
    },
    /// Buy a sold item back (`CMSG_BUYBACK_ITEM`): `slot` is the **absolute** player-array buyback
    /// slot 69–80 (the app maps the clicked 1-based, timestamp-sorted list index to it). Success is
    /// the item re-creating + coinage falling via `UPDATE_OBJECT`; refusal answers
    /// `SMSG_BUY_FAILED`.
    BuybackItem { vendor: u64, slot: u32 },
    /// Repair at a repair-capable vendor (`CMSG_REPAIR_ITEM`): `item_guid` 0 = repair everything,
    /// else the one item. Success is durability rising + coinage falling via `UPDATE_OBJECT`
    /// (no dedicated answer packet).
    RepairItem { vendor: u64, item_guid: u64 },
    /// Open the bank (`CMSG_BANKER_ACTIVATE`, decision 0604): the direct opener a right-click on
    /// a pure banker (bit 8 the lowest service bit) uses — a gossip-flagged banker routes through
    /// the gossip menu instead, whose bank option makes the server volunteer the same answer.
    /// Answered by `SMSG_SHOW_BANK` (a `ShowBank` event → the bank window opens off local state).
    BankerActivate { guid: u64 },
    /// Buy the next bank-bag slot (`CMSG_BUY_BANK_SLOT`, decision 0604): the purchase popup's
    /// accept. **No packet on success** — the descriptor's `PLAYER_BYTES_2` bank-bag count + the
    /// falling coinage are the confirmation; refusal answers `SMSG_BUY_BANK_SLOT_RESULT` (a
    /// `BuyBankSlotResult` event → the red error line).
    BuyBankSlot { guid: u64 },
    /// Deposit: auto-place a bag/doll item into the bank (`CMSG_AUTOBANK_ITEM`, decision 0604) —
    /// the right-click auto-move while the bank window is open. Wire `(bag, slot)`; refusal
    /// answers `SMSG_INVENTORY_CHANGE_FAILURE` (the red line), success moves the item via
    /// `UPDATE_OBJECT` field deltas.
    AutoBankItem { bag: u8, slot: u8 },
    /// Withdraw: auto-place a bank item into the bags (`CMSG_AUTOSTORE_BANK_ITEM`, decision 0604)
    /// — the right-click auto-move on a bank slot. Same wire shape and answers as
    /// [`Self::AutoBankItem`] (vmangos routes by whether the source is a bank position).
    AutoStoreBankItem { bag: u8, slot: u8 },
    /// Ask (or re-ask) a trainer's service list (`CMSG_TRAINER_LIST`, decision 0237): one trainer
    /// guid. The window first opens off the gossip trainer option's `SMSG_TRAINER_LIST`; this is the
    /// *refresh* verb, re-requested after a purchase to repaint the bought row green→gray (the server
    /// does not auto-resend on a buy). Answered by `SMSG_TRAINER_LIST` (a `TrainerList` event).
    TrainerList { trainer: u64 },
    /// Buy (learn) a trainer service (`CMSG_TRAINER_BUY_SPELL`, decision 0237): the trainer guid + the
    /// service's `spell_id`. Sent by the Train button ([`crate::ui_trainer`]'s buy drain). Success
    /// answers `SMSG_TRAINER_BUY_SUCCEEDED` and delivers the spell via `SMSG_LEARNED_SPELL`; refusal
    /// answers `SMSG_TRAINER_BUY_FAILED` (a `TrainerBuyFailed` event → the window's error line).
    TrainerBuySpell { trainer: u64, spell_id: u32 },
    /// Spend talent points (`CMSG_LEARN_TALENT`, decision 0304): a `Talent.dbc` row id + the
    /// requested rank (0-based, learn-up-to — the click sends the current rank count). Sent by
    /// the talent window's click-to-learn. No dedicated reply: success arrives as the rank
    /// spell's learn effects + the refreshed `PLAYER_CHARACTER_POINTS1`.
    LearnTalent { talent_id: u32, rank: u32 },
    /// Abandon a whole skill line (`CMSG_UNLEARN_SKILL`): the skills pane's unlearn button →
    /// the `UNLEARN_SKILL` popup's accept ([`crate::ui_char`]'s abandon drain). No ack — the
    /// server's `SetSkill(id, 0, 0)` returns as a `PLAYER_SKILL_INFO` field update, which the
    /// skills feed re-pushes (the engine never removes the line locally).
    UnlearnSkill { skill_id: u32 },
    /// Use a world GameObject (`CMSG_GAMEOBJ_USE`, decision 0236): a full guid naming the
    /// chest/door/quest-object/lever under the cursor. Sent by the right-click route
    /// ([`crate::target`]) when the nearest hovered CGObject is a usable GameObject. The server fans
    /// it out by GO type — a chest answers with `SMSG_LOOT_RESPONSE` (the loot window), a questgiver
    /// GO with the gossip/quest packets, a door with a `GAMEOBJECT_STATE` flip — or refuses silently.
    GameObjUse { guid: u64 },
    /// Report walking into an `AreaTrigger.dbc` volume (`CMSG_AREATRIGGER`): the trigger's id.
    /// Sent by [`crate::area_trigger`]'s per-frame containment check — the client's whole part in
    /// the system. The server answers a teleport trigger with the ordinary
    /// `SMSG_TRANSFER_PENDING`/`SMSG_NEW_WORLD` pair (or a same-map `MSG_MOVE_TELEPORT_ACK`), a
    /// refused one with `SMSG_AREA_TRIGGER_MESSAGE`, and most with nothing at all.
    AreaTrigger { trigger_id: u32 },
    /// Ask a GameObject's template (`CMSG_GAMEOBJECT_QUERY`, decision 0239): `entry` + the asking
    /// `guid`. Sent ask-once when a GameObject streams in ([`crate::go_templates`]); the answer's
    /// `lockId` decides whether a right-click uses it or casts an OPEN_LOCK spell.
    GameObjectQuery { entry: u32, guid: u64 },
    /// Ask for one page of a book (`CMSG_PAGE_TEXT_QUERY`, decision 1105): the `PageText` id + the
    /// asking object's `guid` (an item's or a TEXT GameObject's — the server discards it). Sent
    /// ask-once by [`crate::ui_item_text::PageTexts`] when a reader opens on a page it hasn't got;
    /// vmangos answers with the whole forward chain, one `SMSG_PAGE_TEXT_QUERY_RESPONSE` per page.
    PageTextQuery { page_id: u32, guid: u64 },
    /// Cast an OPEN_LOCK spell at a **GameObject** (`CMSG_CAST_SPELL`, decision 0239): the right-click
    /// on a locked chest / mining vein / herb node. The server runs `EffectOpenLock` → a chest opens
    /// its loot (`SMSG_LOOT_RESPONSE`); the profession/skill gate is the server's.
    CastSpellGameObject { spell_id: u32, go_guid: u64 },
    /// An item-targeted cast (`TARGET_FLAG_ITEM` + packed guid) — the CraftFrame enchant pick
    /// (decision 0437 phase 3).
    CastSpellItem { spell_id: u32, item_guid: u64 },
    /// Open a corpse/creature's loot (`CMSG_LOOT`, decision 0084): a full guid naming the lootable
    /// unit. Sent by the right-click loot route ([`crate::target`]) on the loot classification
    /// (a dead unit carrying `UNIT_DYNFLAG_LOOTABLE`). Answered by `SMSG_LOOT_RESPONSE` (a
    /// `LootResponse` event on success, `LootError` on refusal). Not usable on a GameObject guid —
    /// the server rejects those; a GameObject loots via `GameObjUse` above.
    Loot { guid: u64 },
    /// Take one loot row into the bags (`CMSG_AUTOSTORE_LOOT_ITEM`): `slot` is the **wire** loot slot
    /// (0-based, from `SMSG_LOOT_RESPONSE`), which the loot drain maps from the clicked 1-based
    /// display row. The server auto-places into the first free bag slot; success answers
    /// `SMSG_LOOT_REMOVED` + `SMSG_ITEM_PUSH_RESULT` + the item-create path.
    AutostoreLootItem { slot: u8 },
    /// Take the loot's coin pile (`CMSG_LOOT_MONEY`, empty body): the intent a click on the
    /// synthesized coin row queues. Answered by `SMSG_LOOT_CLEAR_MONEY` (+ the coinage rising via
    /// `UPDATE_OBJECT`; solo looting gets no `SMSG_LOOT_MONEY_NOTIFY` on this server).
    LootMoney,
    /// Close the loot window (`CMSG_LOOT_RELEASE`, decision 0084): the server ignores `guid` and
    /// releases whatever loot it has stored for us. Sent on `CloseLoot` (the window's `OnHide`).
    /// Answered by `SMSG_LOOT_RELEASE_RESPONSE` (a `LootReleaseResponse` event).
    LootRelease { guid: u64 },
    /// Cast a group-loot vote (`CMSG_LOOT_ROLL`, decision 0591): the Need/Greed/Pass click on a
    /// `GroupLootFrame`. The roll is addressed by the `(looted_target, item_slot)` pair the server
    /// opened it with — the client-internal `rollID` the Lua side uses never reaches the wire.
    /// Answered by an `SMSG_LOOT_ROLL` echo of our vote, then the roll's resolution.
    LootRoll {
        looted_target: u64,
        item_slot: u32,
        roll_type: u8,
    },
    /// Look at an available quest (`CMSG_QUESTGIVER_QUERY_QUEST`, decision 0088): a greeting/gossip
    /// quest row click. Answered by `SMSG_QUESTGIVER_QUEST_DETAILS` (a `QuestDetail` event → the
    /// accept panel).
    QuestgiverQuery { npc: u64, quest: u32 },
    /// Accept a quest (`CMSG_QUESTGIVER_ACCEPT_QUEST`): the detail panel's Accept button. Adds it to
    /// the log; the server closes the gossip window (`SMSG_GOSSIP_COMPLETE`).
    QuestgiverAccept { npc: u64, quest: u32 },
    /// Ask a quest's turn-in progress panel (`CMSG_QUESTGIVER_COMPLETE_QUEST`): an active greeting/
    /// gossip quest row click. Answered by `SMSG_QUESTGIVER_REQUEST_ITEMS` (a `QuestProgress` event),
    /// or OFFER_REWARD when there are no required items.
    QuestgiverComplete { npc: u64, quest: u32 },
    /// Advance from the progress panel to the reward panel (`CMSG_QUESTGIVER_REQUEST_REWARD`): the
    /// progress panel's Continue button. Answered by `SMSG_QUESTGIVER_OFFER_REWARD` (a `QuestOffer`
    /// event).
    QuestgiverRequestReward { npc: u64, quest: u32 },
    /// Choose a reward and finish the quest (`CMSG_QUESTGIVER_CHOOSE_REWARD`, `choice` = the 0-based
    /// choice index): the reward panel's Complete button. Answered by `SMSG_QUESTGIVER_QUEST_COMPLETE`
    /// (a `QuestComplete` event) + the XP/money/item grants via `UPDATE_OBJECT`.
    QuestgiverChooseReward { npc: u64, quest: u32, choice: u32 },
    /// Ask a quest's full template (`CMSG_QUEST_QUERY`, the quest-log slice, decision 0088's
    /// deferred second half): the log window's ask-once detail source, distinct from
    /// `QuestgiverQuery` (which needs an NPC guid, not just the quest id). Answered by
    /// `SMSG_QUEST_QUERY_RESPONSE` (a `QuestTemplate` event) into the [`crate::ui_quest_log::QuestLog`]
    /// cache.
    QuestQuery { quest: u32 },
    /// Ask an NPC's questgiver dialog status (`CMSG_QUESTGIVER_STATUS_QUERY`) — the overhead
    /// `!`/`?` marker's value, answered by `SMSG_QUESTGIVER_STATUS`.
    QuestgiverStatusQuery { npc: u64 },
    /// Abandon a quest-log slot (`CMSG_QUESTLOG_REMOVE_QUEST`): the log window's confirmed abandon
    /// (`AbandonQuest()`'s two-step). No ack SMSG — the server clears the `PLAYER_QUEST_LOG` slot
    /// fields directly, which the next feed pass reads as the slot going empty.
    QuestlogRemove { slot: u8 },
    // ── The mail arc (decision 0544; writer bodies in benilla-protocol `world/writer/mail.rs`). ──
    /// Ask the mailbox's inbox page (`CMSG_GET_MAIL_LIST`) — the window's open verb and its
    /// refresh. Answered by `SMSG_MAIL_LIST_RESULT` (a `MailList` event).
    GetMailList { mailbox: u64 },
    /// Send a mail (`CMSG_SEND_MAIL`): recipient/subject/body, an optional item attachment
    /// (`item_guid == 0` = none), money, and COD. Answered by `SMSG_SEND_MAIL_RESULT`.
    SendMail {
        mailbox: u64,
        receiver: String,
        subject: String,
        body: String,
        item_guid: u64,
        money: u32,
        cod: u32,
    },
    /// Take a mail's attached money (`CMSG_MAIL_TAKE_MONEY`).
    MailTakeMoney { mailbox: u64, mail_id: u32 },
    /// Take a mail's attached item (`CMSG_MAIL_TAKE_ITEM`).
    MailTakeItem { mailbox: u64, mail_id: u32 },
    /// Mark a mail read (`CMSG_MAIL_MARK_AS_READ`) — sent when a letter opens. No response packet.
    MailMarkAsRead { mailbox: u64, mail_id: u32 },
    /// Return a mail to its sender (`CMSG_MAIL_RETURN_TO_SENDER`).
    MailReturnToSender { mailbox: u64, mail_id: u32 },
    /// Delete a mail (`CMSG_MAIL_DELETE`) — the taken-letter close and the explicit Delete button
    /// alike.
    MailDelete { mailbox: u64, mail_id: u32 },
    /// Make a permanent copy of a letter's body (`CMSG_MAIL_CREATE_TEXT_ITEM`) — the open letter's
    /// attachment-row letter button. Answered by `SMSG_SEND_MAIL_RESULT` (action MADE_PERMANENT).
    MailCreateTextItem { mailbox: u64, mail_id: u32 },
    /// Fetch a letter's body text (`CMSG_ITEM_TEXT_QUERY`) — sent once per mail whose
    /// `item_text_id != 0`. Answered by `SMSG_ITEM_TEXT_QUERY_RESPONSE` (a `MailItemText` event).
    ItemTextQuery { text_id: u32, mail_id: u32 },
    /// Ask whether unread mail is waiting (`MSG_QUERY_NEXT_MAIL_TIME`, empty body). Sent once at
    /// login to seed `HasNewMail()`/the minimap letter icon (decision 0544 P3, sent by
    /// `crate::ui_mail`'s world-enter one-shot).
    QueryNextMailTime,
    /// Ask for the server's wall clock (`CMSG_QUERY_TIME`, empty body) — sent on every world
    /// entry, answered by `SMSG_QUERY_TIME_RESPONSE` into [`ServerWallClock`]. That clock is the
    /// only way to read the absolute deadlines the server writes into descriptor fields, which is
    /// how the timed-quest countdown gets its number (decision 1150).
    QueryTime,
    /// Ask to inspect a player (`CMSG_INSPECT`, `u64 target`) — the UnitPopup INSPECT row
    /// (decision 0631). Fire-and-forget: the reply echoes the guid and nothing else, and the window
    /// paints from the already-streamed PUBLIC `PLAYER_VISIBLE_ITEM_*` fields. Sent anyway because
    /// server-side it also sets our selection (`MiscHandler.cpp:945`), as the real client's does.
    Inspect { target: u64 },
    // ── The player-trade arc (decision 0592; writer bodies in benilla-protocol
    //    `world/writer/trade.rs`). ─────────────────────────────────────────────────────────────
    /// Offer to trade with a player (`CMSG_INITIATE_TRADE`, `u64 target`) — the UnitPopup TRADE row.
    /// The server answers the initiator on any refusal (`SMSG_TRADE_STATUS`) and, on success, sends
    /// the *target* `BEGIN_TRADE`.
    InitiateTrade { target: u64 },
    /// The target's auto-reply to `BEGIN_TRADE` (`CMSG_BEGIN_TRADE`, empty) — makes the server emit
    /// `OPEN_WINDOW` to both sides. Sent by the net bridge the frame it decodes a `BEGIN_TRADE`
    /// status (decision 0592 P1; the reference client auto-answers, `TradeHandler.cpp`).
    BeginTrade,
    /// Press the Trade button (`CMSG_ACCEPT_TRADE`) — the accept half of the two-sided confirm.
    AcceptTrade,
    /// Drop your own accept but stay in the trade (`CMSG_UNACCEPT_TRADE`).
    UnacceptTrade,
    /// Cancel/close the trade (`CMSG_CANCEL_TRADE`, empty) — the window's close/cancel verb.
    CancelTrade,
    /// Offer this many copper on our side (`CMSG_SET_TRADE_GOLD`, `u32`) — the money input's
    /// value-changed callback (decision 0592 P2). Clears both accepts + re-arms the 200 ms scam delay
    /// server-side; the server echoes our new gold back as `SMSG_TRADE_STATUS_EXTENDED`.
    SetTradeGold { copper: u32 },
    /// Place the item at inventory (`bag`, `slot`) into our trade slot `trade_slot` (0-based, 0..=6;
    /// 6 = the non-traded/enchant slot) — `CMSG_SET_TRADE_ITEM`, a bag item dropped onto the slot
    /// (decision 0592 P2). The server echoes the filled slot back as `SMSG_TRADE_STATUS_EXTENDED`.
    SetTradeItem { trade_slot: u8, bag: u8, slot: u8 },
    /// Clear our trade slot `trade_slot` (0-based) — `CMSG_CLEAR_TRADE_ITEM`, an empty-cursor click on
    /// a filled slot (decision 0592 P2).
    ClearTradeItem { trade_slot: u8 },
    /// Leave the world back to character select (`CMSG_LOGOUT_REQUEST`, the `/logout` command —
    /// decision 0193). The server answers `SMSG_LOGOUT_COMPLETE` (instant for a resting/GM
    /// character), which the IO thread turns into a [`LoggedOutMessage`] + an immediate
    /// reconnect whose fresh roster is the select screen.
    Logout,
    /// Call off a pending logout (`CMSG_LOGOUT_CANCEL`) — the CAMP/QUIT dialog's Cancel, decision
    /// 0674. Only the non-instant logout has anything to cancel; the server acks with
    /// `SMSG_LOGOUT_CANCEL_ACK`, which becomes the UI's `LOGOUT_CANCEL` event.
    LogoutCancel,
    /// Join a chat channel (`CMSG_JOIN_CHANNEL` — `/join`, and the 0288 P6 zone auto-join).
    JoinChannel { name: String, password: String },
    /// Leave a chat channel (`CMSG_LEAVE_CHANNEL` — `/leave`).
    LeaveChannel { name: String },
    /// Ask a channel's roster (`CMSG_CHANNEL_LIST` — `/chatlist <name>`).
    ChannelList { name: String },
    /// `/random [min] [max]` (`MSG_RANDOM_ROLL`).
    RandomRoll { min: u32, max: u32 },
    /// `/played` (`CMSG_PLAYED_TIME`).
    PlayedTime,
    /// Acknowledge a triggered cinematic as finished (`CMSG_COMPLETE_CINEMATIC`). benilla doesn't
    /// play cinematics yet, so the Net drain answers every `SMSG_TRIGGER_CINEMATIC` immediately —
    /// the skip a real player's ESC sends. Without the ack, vmangos anchors object visibility to
    /// the flying cinematic camera (a first login's race intro) and every NPC around the body
    /// despawns until relog. A future cinematic arc moves this send to the playback's end.
    CompleteCinematic,
    /// **Acknowledge a granted mover mode** — root, water-walk, feather-fall or hover (decisions
    /// 0308, 0866): the echoed `counter` + our live pose, on the opcode
    /// [`MoveMode::ack_opcode`] picks. Sent by the controller the frame the
    /// [`MoveModeMessage`] lands; un-acked, the server never applies the change and observers never
    /// see it.
    ///
    /// `flags` must already carry the applied mode bit — the controller ORs it in before sending
    /// (vmangos KICKS a root-apply ack whose `MovementInfo` lacks `MOVEFLAG_ROOT`, and reads the
    /// word as the mover's new flags for the other three).
    MoveModeAck {
        guid: u64,
        counter: u32,
        mode: MoveMode,
        apply: bool,
        flags: u32,
        pos: [f32; 3],
        orientation: f32,
    },
    /// Release the spirit (`CMSG_REPOP_REQUEST`, empty body) — the DEATH popup's Release Spirit
    /// (and its timeout's server-forced twin is server-side; decision 0308 slice 1).
    RepopRequest,
    /// Ask where our corpse is (`MSG_CORPSE_QUERY`, empty request) — on becoming a ghost and on
    /// login-while-dead; answered into [`crate::death::DeathNet`] (decision 0308 §5).
    CorpseQuery,
    /// Reclaim our corpse (`CMSG_RECLAIM_CORPSE` — RECOVER_CORPSE's Accept). Server-gated to
    /// ghost + delay elapsed + 39 yd; success returns as ordinary descriptor deltas.
    ReclaimCorpse { corpse: u64 },
    /// Take the spirit healer's resurrection (`CMSG_SPIRIT_HEALER_ACTIVATE` — the XP_LOSS
    /// confirm's final Accept): 50% res, 25% durability, sickness at level ≥ 11.
    SpiritHealerActivate { npc: u64 },
    /// Answer a resurrection offer (`CMSG_RESURRECT_RESPONSE` — the RESURRECT popups).
    ResurrectResponse { caster: u64, accept: bool },
    // ── The group/party family (decision 0434; writer bodies in benilla-protocol
    //    `world/writer/group.rs`) ──────────────────────────────────────────────────────────────
    /// Invite a player by name (`CMSG_GROUP_INVITE` — `/invite`, the unit menus, the invite ack
    /// comes back as `SMSG_PARTY_COMMAND_RESULT`).
    GroupInvite { name: String },
    /// Accept the pending group invite (`CMSG_GROUP_ACCEPT` — the PARTY_INVITE popup's Accept).
    GroupAccept,
    /// Decline the pending group invite (`CMSG_GROUP_DECLINE` — Decline/timeout/escape).
    GroupDecline,
    /// Kick a member by name (`CMSG_GROUP_UNINVITE` — `/uninvite`, leader only).
    GroupUninvite { name: String },
    /// Hand leadership to a member (`CMSG_GROUP_SET_LEADER` — `/promote`, leader only).
    GroupSetLeader { guid: u64 },
    /// Leave the group (`CMSG_GROUP_DISBAND` — the wire's leave verb, despite the name).
    GroupLeave,
    /// Set the loot rules (`CMSG_LOOT_METHOD`): `method` 0..4, `master` guid (master loot only),
    /// `threshold` quality 2..4. Leader only; echoes back as a fresh `SMSG_GROUP_LIST`.
    LootMethod {
        method: u32,
        master: u64,
        threshold: u32,
    },
    /// Mark a unit with a raid-target icon (`MSG_RAID_TARGET_UPDATE` outbound — the popup's
    /// submenu, decision 0434 §5): wire `icon` 0..7, `guid` 0 clears that icon's slot. Leader/
    /// assistant only server-side; echoes back as the delta form.
    SetRaidTarget { icon: u8, guid: u64 },
    // ── The duel family (decision 0633; writer bodies in benilla-protocol
    //    `world/writer/duel.rs`). Challenging is a `CastSpell` of the duel spell, not a verb here.
    /// Accept a duel challenge (`CMSG_DUEL_ACCEPTED`) — the popup's Accept, and the challenger's
    /// own auto-accept the instant its request echoes back.
    DuelAccepted { arbiter: u64 },
    /// Decline / cancel / forfeit a duel (`CMSG_DUEL_CANCELLED`) — one opcode for all three; the
    /// server reads the intent from the duel's state.
    DuelCancelled { arbiter: u64 },
    // ── The social family (decision 0668; writer bodies in benilla-protocol
    //    `world/writer/social.rs`). Note the wire's own asymmetry: add by NAME, remove by GUID.
    /// Refresh the friend list (`CMSG_FRIEND_LIST`) — the FrameXML's `ShowFriends()`. The list
    /// also arrives unasked at login, so this is never the only path.
    FriendListRequest,
    /// Befriend a character by name (`CMSG_ADD_FRIEND`).
    AddFriend { name: String },
    /// Drop a friend by guid (`CMSG_DEL_FRIEND`) — the caller resolves the name first.
    DelFriend { guid: u64 },
    /// Ignore a character by name (`CMSG_ADD_IGNORE`).
    AddIgnore { name: String },
    /// Stop ignoring, by guid (`CMSG_DEL_IGNORE`).
    DelIgnore { guid: u64 },
    /// Tell the server we dropped an ignored player's whisper (`CMSG_CHAT_IGNORED`) so it can
    /// answer them "X is ignoring you". The drop itself is entirely client-side — the server
    /// keeps delivering an ignored player's chat, which is why the client has to filter *and*
    /// report.
    ChatIgnored { guid: u64 },
    /// Run a `/who` (`CMSG_WHO`) — the filter string is already parsed into wire fields
    /// (`ui_social::who_query`, which needs the DBCs the parse resolves names against).
    Who { request: Box<WhoRequest> },
    /// Ask to flip our own PvP flag (`CMSG_TOGGLE_PVP`, empty body — decision 0646): `/pvp` and
    /// the unit popup's PvP row, both through the VM's intent queue. Nothing local changes; the
    /// answer is the descriptor's PvP bit (and flagging *off* waits out the server's 300 s timer).
    TogglePvp,
    // ── The taxi/flight-master family (decision 0484 phase 1; writer bodies in
    //    benilla-protocol `messages::taxi`) ──────────────────────────────────────────────────
    /// Ask a nearby flight master's known status (`CMSG_TAXINODE_STATUS_QUERY`): the guid of the
    /// flight master, not ours (vmangos's own comment on the wire field). Answered by
    /// `SMSG_TAXINODE_STATUS` (a `TaxiNodeStatus` event). No send site yet — phase 2's cursor/
    /// tooltip work; wired end-to-end now so that phase only adds a call site.
    #[allow(dead_code)]
    TaxiNodeStatusQuery { guid: u64 },
    /// Open a flight master's taxi map (`CMSG_TAXIQUERYAVAILABLENODES`, decision 0496 I4:
    /// CONFIRMED as built): the direct opener the taxi-cursor interact sends — the interact ladder
    /// is first-match-wins low→high over `UNIT_NPC_FLAGS`, so a gossip+taxi NPC pre-empts to
    /// gossip and only a pure flightmaster reaches this send. A known node answers
    /// `SMSG_SHOWTAXINODES` (a `TaxiNodesShown` event); a never-visited node instead answers the
    /// first-visit learn pair (`NewTaxiPath` + `TaxiNodeStatus`) and opens nothing on this click.
    TaxiQueryNodes { guid: u64 },
    /// Fly a single hop (`CMSG_ACTIVATETAXI`): the flight-master guid, the source node, the
    /// destination node. Answered by `SMSG_ACTIVATETAXIREPLY`; success continues into the mount +
    /// `SMSG_MONSTER_MOVE` flight (the existing self-spline rails, decision 0260). Sent by the
    /// taxi-map window's `TakeTaxiNode` drain on a one-hop route pick (`crate::ui_taxi`, phase 2).
    ActivateTaxi {
        guid: u64,
        source_node: u32,
        dest_node: u32,
    },
    /// Fly a multi-hop chain in one send (`CMSG_ACTIVATETAXIEXPRESS`, decision 0496 §TU-3): sent
    /// when no direct `TaxiPath` edge exists current→target — the byte-verified discriminator,
    /// not hop count. The flight-master guid, the route's combined fare, and the full node chain
    /// in order. Answered by `SMSG_ACTIVATETAXIREPLY`, same as [`Self::ActivateTaxi`]. Sent by the
    /// taxi-map window's `TakeTaxiNode` drain on an edge-less route pick (`crate::ui_taxi`, phase 2).
    ActivateTaxiExpress {
        guid: u64,
        total_cost: u32,
        nodes: Vec<u32>,
    },
}

/// The account's character roster (`SMSG_CHAR_ENUM`, bridged from the Net drain): the IO thread is
/// parked at character select waiting for the app's pick. Consumed by [`crate::char_select`]'s
/// policy — auto-answer (pending pick / `WOW_CHAR`) or show the roster and wait for the director.
/// `realm` is the auth realm-list entry this session connected to (name + type for the screen's
/// realm banner, decision 0465).
#[derive(Message)]
pub(crate) struct CharListMessage {
    pub(crate) characters: Vec<benilla_protocol::Character>,
    pub(crate) realm: Option<benilla_protocol::RealmInfo>,
}

/// The result of an in-place char create/delete the parked IO thread serviced (decision 0423),
/// bridged from the Net drain. A refreshed [`CharListMessage`] precedes a successful one. Consumed by
/// the char-create screen (`crate::char_create`) to show the outcome; `code` is the raw
/// `WorldResult` byte the screen maps to its 1.12 status text.
#[derive(Message, Clone, Copy)]
pub(crate) struct CharActionResultMessage {
    pub(crate) action: benilla_protocol::CharAction,
    pub(crate) code: u8,
}

/// We entered the world (the IO thread's `Connected`, bridged from the Net drain): flips
/// [`crate::char_select::ClientState`] to `InWorld`.
#[derive(Message)]
pub(crate) struct EnteredWorldMessage;

/// One `CHAT_MSG_SYSTEM` line — the server's own answer to a GM dot-command ("You set god mode to
/// on for …", "There is no such command", "Player not found!"). It has always been *logged*
/// (`net: server says — …`, decision 0651); this makes it *readable*, which is what lets a sender
/// know whether its command landed. That matters most for server state the descriptor does not
/// carry: vmangos's god mode is a runtime-only `Unit::m_invincibilityHpThreshold` with no field and
/// no flag on the wire, so this line is the only ground truth there is (decision 0677).
#[derive(Message, Clone)]
pub(crate) struct ServerSaidMessage {
    pub(crate) text: String,
}

/// The server confirmed our logout (`SMSG_LOGOUT_COMPLETE`): back to character select. The Net
/// drain does the self-teardown (the part 0065's disconnect teardown deliberately keeps); this
/// message flips [`crate::char_select::ClientState`] and clears the pick policy's pending state.
#[derive(Message)]
pub(crate) struct LoggedOutMessage;

/// A login attempt progressed (the IO thread's `LoginStage`, bridged from the Net drain): the
/// login screen's connecting dialog quotes the matching `LOGIN_STATE_*` string (decision 0539).
#[derive(Message, Clone, Copy)]
pub(crate) struct LoginStageMessage {
    pub(crate) stage: benilla_protocol::LoginStage,
}

/// The session ended (socket death, logout's teardown edge) — bridged from the Net drain's
/// `Disconnected` arm. [`crate::login`]'s policy reads it as "the IO thread is back at its
/// pre-logon park": with pending credentials it schedules the silent re-auth (decision 0539 §3 —
/// 0065's seamless reconnect, paced app-side).
#[derive(Message, Clone)]
pub(crate) struct DisconnectedMessage {
    pub(crate) reason: String,
}

/// A login attempt failed before the roster (decision 0539): `code` is the server's auth result
/// byte on a refusal (mapped to its `AUTH_*` glue string), `None` on a transport failure. The IO
/// thread is back at its pre-logon park; [`crate::login`]'s policy decides what happens next.
/// `terminal` marks a codeless failure that retrying cannot fix (the server is unusable by this
/// client — e.g. it requires Warden): show `reason` and stop, never resubmit.
#[derive(Message, Clone)]
pub(crate) struct LoginFailedMessage {
    pub(crate) code: Option<u8>,
    pub(crate) reason: String,
    pub(crate) terminal: bool,
}

/// A same-map teleport for our player: snap to the pose, then echo the ack. Written by
/// [`apply_net_updates`] (only for our guid), read by the player controller in `WorldStage::Input`.
#[derive(Message, Clone, Copy)]
pub(crate) struct TeleportMessage {
    pub(crate) guid: u64,
    pub(crate) counter: u32,
    pub(crate) position: [f32; 3],
    pub(crate) orientation: f32,
}

/// A **server-authored move for our own mover** — an inbound `MSG_MOVE_*` whose guid is ours. It is
/// never an echo of our own reporting: vmangos stamps every one `MovementInfo::SetAsServerSide`
/// (`ctime = 0`, "not a client packet"), and the senders are all server-side edges with no
/// handshake and no ack owed — `.go forward`/`up`/`relative` (`Unit::NearLandTo` →
/// `MSG_MOVE_FALL_LAND`), `.cheat fly`/`fixedz` and the movement anticheat's snap-back
/// (`SendHeartBeat(true)` → `MSG_MOVE_HEARTBEAT`).
///
/// **The real client applies these** — its inbound move path has no mover-guid gate at all, and the
/// local player sits in the object manager under its own guid, so a self-addressed packet resolves
/// and applies exactly like a remote's (wow-re `system/collision/scratch/self-addressed-move.md`;
/// decision 0725, which corrects the drop this used to take). Our avatar's motion source is the
/// controller rather than the `RemoteMotion` lane, so the pose crosses as this message and lands in
/// `player::wire_in`.
#[derive(Message, Clone, Copy)]
pub(crate) struct SelfMoveMessage {
    pub(crate) position: [f32; 3],
    pub(crate) orientation: f32,
    /// The wire's `MOVEMENTFLAGS`. Merged into ours under a mask, never assigned — see
    /// [`crate::creature_anim::move_flags::SERVER_AUTHORED`].
    pub(crate) flags: u32,
    pub(crate) pitch: f32,
    pub(crate) fall_time: u32,
    pub(crate) jump: Option<benilla_protocol::JumpInfo>,
}

/// The server changed one of our own mover's speeds (`SMSG_FORCE_*_SPEED_CHANGE` — aura, mount, GM
/// `.modify speed`). Written by [`apply_net_updates`] (which also applies the new value to the
/// entity's [`UnitSpeeds`], and only forwards changes addressed to our guid); read by the player
/// controller, which answers with [`ClientCommand::ForceSpeedAck`] carrying its live wire state —
/// the same pattern as [`TeleportMessage`], because only the controller owns the honest pose.
#[derive(Message, Clone, Copy)]
pub(crate) struct SpeedChangeMessage {
    pub(crate) guid: u64,
    pub(crate) kind: SpeedKind,
    pub(crate) counter: u32,
    pub(crate) speed: f32,
}

/// **The server granted or revoked a mover mode** on our own mover — root, water-walk, feather-fall
/// or hover (the ack'd movement-mode family; decision 0866). Sibling of [`SpeedChangeMessage`] in
/// every respect: written by [`apply_net_updates`] for our guid only, read by the player controller,
/// which is the one place that owns both the mode state and the honest pose the ack must carry.
///
/// This used to be two death-arc-shaped messages (`MoveRootMessage`/`WaterWalkMessage` in
/// `crate::death`), which is why the other two modes were never noticed as missing: root arrived at
/// death, water-walk arrived in ghost form, and nothing framed them as one family. They are one —
/// the server's own `IsFlagAckOpcode` set.
#[derive(Message, Clone, Copy)]
pub(crate) struct MoveModeMessage {
    /// Our mover's guid (the bridge only emits ours) — echoed in the ack body.
    pub(crate) guid: u64,
    pub(crate) counter: u32,
    pub(crate) mode: MoveMode,
    pub(crate) apply: bool,
}

/// A cross-map worldport (`.tele Orgrimmar`, initial-login map, a boat crossing the sea): snap
/// the avatar, swap the map's ADTs, and ack if required. Written by [`apply_net_updates`], read
/// by the player controller.
#[derive(Message, Clone, Copy)]
pub(crate) struct WorldportMessage {
    pub(crate) map_id: u32,
    /// **Boat-local** when `transport_entry` is set (vmangos `SendNewWorld` sends the rider's
    /// `GetTransportPos()` — decision 0455); world otherwise.
    pub(crate) position: [f32; 3],
    pub(crate) orientation: f32,
    pub(crate) needs_ack: bool,
    /// The `gameobject_template` entry of the transport carrying us through this transfer, from
    /// the preceding `SMSG_TRANSFER_PENDING` (the [`PendingTransfer`] latch). `Some` ⇒ the pose
    /// above is boat-local and the ride survives the port; `None` ⇒ world pose, and any ride is
    /// stale (the server detached us — e.g. a GM `.tele` off a deck).
    pub(crate) transport_entry: Option<u32>,
}

/// The `SMSG_TRANSFER_PENDING` latch (decision 0455): a far teleport was announced; the value
/// tells the coming worldport whether we ride a transport through it (and which). Set by the
/// apply layer's TransferPending arm, consumed by its worldport arm, cleared on abort and on
/// disconnect.
#[derive(Resource, Default)]
pub(crate) struct PendingTransfer(pub(crate) Option<PendingTransferInfo>);

#[derive(Clone, Copy)]
pub(crate) struct PendingTransferInfo {
    pub(crate) map_id: u32,
    pub(crate) transport_entry: Option<u32>,
}

/// A server-pushed sound (`SMSG_PLAY_SOUND`/`PLAY_MUSIC`/`PLAY_OBJECT_SOUND`): a SoundEntries kit
/// to play — 2D, on the music channel, or 3D at `source` (guid already resolved → entity by
/// [`apply_net_updates`]; `None` when the source isn't streamed to us — consumers play it 2D
/// then, audible beats dropped). Read by the sound subsystem.
#[derive(Message, Clone, Copy)]
pub(crate) struct ServerSoundMessage {
    pub(crate) kind: ServerSoundKind,
    pub(crate) sound_id: u32,
    pub(crate) source: Option<Entity>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServerSoundKind {
    Sound2d,
    Music,
    ObjectSound,
}

/// A nearby unit's emote (`SMSG_TEXT_EMOTE` / `SMSG_EMOTE`, bridged from the Net drain), guid
/// already resolved to its entity. `None` source = the performer isn't streamed (drop — an
/// emote needs its performer for voice race/sex and position).
#[derive(Message, Clone, Copy)]
pub(crate) struct EmoteMessage {
    pub(crate) source: Option<Entity>,
    pub(crate) kind: EmoteKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmoteKind {
    /// A chat emote — the `EmotesText.dbc` id (voice by performer race/sex).
    Text(u32),
    /// An anim emote — the `Emotes.dbc` id (its `EventSoundID`).
    Anim(u32),
}

/// A creature flared at someone (`SMSG_AI_REACTION`, bridged from the Net drain; decision 0280):
/// `hostile` = reaction 2 HOSTILE (sent on every creature melee-attack start) vs reaction 0, the
/// stealth pre-aggro ALERT. Pure audio in the client (`0x6056e0` — no animation, nameplate, or
/// UI); consumer: `sound::creature`.
#[derive(Message, Clone, Copy)]
pub(crate) struct AiReactionMessage {
    pub(crate) unit: Entity,
    pub(crate) hostile: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The addon lane's four wire bytes** (decision 1235) — the client's own whitelist at
    /// `0x49fa3f`-`0x49fa4e`, and the last link in the chain between an addon's Lua call and the
    /// bytes `benilla-protocol`'s `addon_message_bodies_golden` pins. Wrong here and the payload
    /// goes down a lane nobody listens on, which is silent at both ends.
    ///
    /// `RAID_LEADER`/`RAID_WARNING`/`OFFICER`/`BATTLEGROUND_LEADER`/`CHANNEL` are asserted absent
    /// from the map's *image*: vmangos would accept `LANG_ADDON` on all five, and the client sends
    /// on none of them, so a future widening has to fail a test rather than pass unnoticed.
    #[test]
    fn addon_distributions_map_to_the_clients_own_four_wire_types() {
        use benilla_protocol::messages as m;
        use benilla_ui::script::AddonDistribution as D;

        assert_eq!(addon_wire_chat_type(D::Party), 0x01, "CHAT_MSG_PARTY");
        assert_eq!(addon_wire_chat_type(D::Raid), 0x02, "CHAT_MSG_RAID");
        assert_eq!(addon_wire_chat_type(D::Guild), 0x03, "CHAT_MSG_GUILD");
        assert_eq!(
            addon_wire_chat_type(D::Battleground),
            0x5C,
            "CHAT_MSG_BATTLEGROUND"
        );

        // The image is exactly those four — no distribution reaches a lane the reference refuses.
        let image: Vec<u32> = [D::Party, D::Raid, D::Guild, D::Battleground]
            .into_iter()
            .map(addon_wire_chat_type)
            .collect();
        for refused in [
            m::CHAT_TYPE_SAY,
            m::CHAT_TYPE_YELL,
            m::CHAT_TYPE_WHISPER,
            m::CHAT_TYPE_EMOTE,
            m::CHAT_TYPE_OFFICER,
            m::CHAT_TYPE_CHANNEL,
            m::CHAT_TYPE_RAID_LEADER,
            m::CHAT_TYPE_RAID_WARNING,
            m::CHAT_TYPE_BATTLEGROUND_LEADER,
        ] {
            assert!(
                !image.contains(&refused),
                "{refused:#04x} is a lane the client never sends addon data on"
            );
        }
    }
}
