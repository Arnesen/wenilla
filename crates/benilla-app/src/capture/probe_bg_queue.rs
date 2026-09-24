//! The battleground-queue live probe (`WOW_PROBE_BGQUEUE=1`) — the instrument that makes
//! "**log in while queued for a battleground**" a shape anyone can reproduce, inert without the
//! env.
//!
//! Decision 2232 audited the 29 drains that could still lose a login burst and argued this one
//! safe on paper: `SMSG_BATTLEFIELD_STATUS` is published as `UPDATE_BATTLEFIELD_STATUS` off a
//! `mem::take` latch that no `VmMemo` re-arms, so a message consumed in 2214's one-frame window
//! is gone — but [`crate::ui_battlefield::reset_on_world_enter`] re-sends
//! `CMSG_BATTLEFIELD_STATUS` on entry and the server answers slot by slot, so the queue heals
//! itself. That record named the argument **unexercised**, and the reason it stayed unexercised
//! is that the setup is three server-side gates deep, not that nobody thought to look.
//!
//! So: level past the bracket floor, GM-hop to the Stormwind Warsong Gulch battlemaster, greet
//! him on the real wire (`CMSG_BATTLEMASTER_HELLO` → `SMSG_BATTLEFIELD_LIST`), queue through the
//! guid that list carried (`CMSG_BATTLEMASTER_JOIN`), and report the slot the server put us in.
//! The probe then **leaves the body queued** — vmangos keeps the queue entry across a logout
//! (`BattleGroundQueue::PlayerLoggedOut` only marks it offline) and re-sends the status from
//! `Player::LoadFromDB`, i.e. inside the login burst. The NEXT plain login is the measurement;
//! this run is its fixture.
//!
//! **Why the guid and not `CMSG_BATTLEFIELD_JOIN`.** vmangos routes the bodyless join through
//! `RequestBgJoinQueue(ObjectGuid{}, …)`, whose `GetNPCIfCanInteractWith(battlemaster,
//! UNIT_NPC_FLAG_BATTLEMASTER)` cannot resolve an empty guid — it logs an anticheat line and
//! returns, silently. Only the battlemaster leg queues anyone, which is why this probe has to
//! stand next to one.
//!
//! **The undo is one line**, and it belongs here rather than in whatever record cites the probe:
//! the body stays queued until something dequeues it, so leave it clean with
//! `WOW_PROBE_LUA='AcceptBattlefieldPort(1,0)'` on a plain login — `GetBattlefieldStatus(1)` then
//! answers `none`. A body left queued is not harmful, only surprising: every later login on that
//! account carries an extra `SMSG_BATTLEFIELD_STATUS` in its burst.
//!
//! Non-combat. Pair with the checkout's probe identity (`.probe-identity`, or WOW_USER/WOW_PASS/WOW_CHAR — the `probe` skill), and `WOW_NOSOUND=1` when
//! it runs unattended. One `timeout`'d run plus a grep for `PROBE bgqueue:` is the whole harness.

use bevy::prelude::*;

use super::probes::ProbeClock;
use crate::net::{ClientCommand, Guid, NetEntity, ObjectStore, SelfPlayer};
use crate::player::Player;
use crate::target::cursor_mode::npc_flags;
use crate::ui_battlefield::Battlefield;
use crate::ui_dialog_verbs::BattlefieldQueue;

/// Elfarran's spawn (vmangos `creature` guid 54614, entry 14981, map 0) — Stormwind's Warsong
/// Gulch battlemaster, `battlemaster_entry` row `14981 → bg_template 2`. The `.go xyz` target;
/// the battlemaster himself is then found in the streamed world by his npc flag, never by a guid.
const BATTLEMASTER_AT: [f32; 3] = [-8454.62, 318.85, 120.97];
/// Warsong Gulch's Map.dbc row — what `GetBattleGroundTypeIdByMapId` turns back into template 2.
const WSG_MAP: u32 = 489;
/// The level the probe body is raised to before queueing.
///
/// `battleground_template` carries one row per content patch, and the server picks the highest at
/// or below its own: WSG opens at **10** on patch 6, **20** on patch 5 and **21** on patch 3. 25
/// clears all three, so the probe never has to know which patch the local server is running —
/// a wrong guess there fails as a bare `LANG_YOUR_BG_LEVEL_REQ_ERROR` notification with no queue,
/// which reads exactly like a broken client.
const QUEUE_LEVEL: u32 = 25;
/// `UNIT_NPC_FLAG_BATTLEMASTER`'s service range is the ordinary NPC one; 15 yd is the hop's
/// settle radius, not a gate — the server applies its own interaction check.
const NEAR_YD: f32 = 15.0;

pub(crate) struct ProbeBgQueuePlugin;

impl Plugin for ProbeBgQueuePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BgQueueProbe>()
            .add_systems(Update, bg_queue_probe);
    }
}

#[derive(Resource, Default)]
struct BgQueueProbe {
    phase: Phase,
}

/// `Wait` → (levelled + GM hop sent) `Hopped` → (battlemaster found, hello sent) `Greeted` →
/// (the list landed, join sent) `Joined` → verdict → `Done`.
#[derive(Default, PartialEq)]
enum Phase {
    #[default]
    Wait,
    Hopped {
        sent_at: f64,
    },
    Greeted {
        master: u64,
        sent_at: f64,
    },
    Joined {
        sent_at: f64,
    },
    Done,
}

// One Bevy system's full input set (the guard-poi probe's shape).
fn bg_queue_probe(
    time: ProbeClock,
    mut probe: ResMut<BgQueueProbe>,
    me: Query<&ObjectStore, With<SelfPlayer>>,
    player: Res<Player>,
    battlefield: Res<Battlefield>,
    queue: Res<BattlefieldQueue>,
    units: Query<(&Guid, &NetEntity, &ObjectStore, &Transform), Without<SelfPlayer>>,
    net: Res<crate::net::NetCommands>,
) {
    let Ok(store) = me.single() else {
        return;
    };
    let now = time.elapsed_secs_f64();
    match probe.phase {
        Phase::Wait => {
            // **Revive first, unconditionally.** `CanInteractWithNPC` refuses a dead player
            // outright (`!IsAlive() && !VISIBLE_TO_GHOSTS`), and the refusal surfaces as an
            // anticheat line about an "invalid creature" — which reads like a wrong guid, not
            // like a corpse. A probe body parks where the last run left it, and this one is
            // easy to leave drowned; `.revive` on a living character is a no-op.
            let _ = net.0.send(ClientCommand::Chat {
                kind: crate::net::ChatKind::Say,
                target: None,
                text: ".revive".to_string(),
            });
            // The level next: a body under the bracket floor is refused at the HELLO, before the
            // list is ever built, so there would be nothing to queue through.
            let level = store.0.unit_level().unwrap_or(0);
            if level < QUEUE_LEVEL {
                info!("PROBE bgqueue: level {level} — raising to {QUEUE_LEVEL} for the bracket");
                let _ = net.0.send(ClientCommand::Chat {
                    kind: crate::net::ChatKind::Say,
                    target: None,
                    text: format!(".levelup {}", QUEUE_LEVEL - level),
                });
            }
            let [x, y, z] = BATTLEMASTER_AT;
            info!("PROBE bgqueue: hopping to the Warsong Gulch battlemaster at ({x}, {y}, {z})");
            let _ = net.0.send(ClientCommand::Chat {
                kind: crate::net::ChatKind::Say,
                target: None,
                text: format!(".go xyz {x} {y} {z} 0"),
            });
            probe.phase = Phase::Hopped { sent_at: now };
        }
        Phase::Hopped { sent_at } => {
            if now - sent_at < 3.0 {
                return; // post-teleport settle: let the battlemaster stream in
            }
            let here = player.pos;
            let master = units.iter().find(|(_, net_e, store, tf)| {
                net_e.kind == benilla_protocol::EntityKind::Unit
                    && store.0.unit_npc_flags() & npc_flags::BATTLEMASTER != 0
                    && tf.translation.distance(here) < NEAR_YD
            });
            if let Some((guid, ..)) = master {
                info!(
                    "PROBE bgqueue: battlemaster {:#x} in range — greeting",
                    guid.0
                );
                let _ = net.0.send(ClientCommand::BattlemasterHello { npc: guid.0 });
                probe.phase = Phase::Greeted {
                    master: guid.0,
                    sent_at: now,
                };
            } else if now - sent_at > 15.0 {
                error!("PROBE bgqueue: FAILURE — no battlemaster streamed in within 15 s");
                probe.phase = Phase::Done;
            }
        }
        Phase::Greeted { master, sent_at } => {
            // Waiting for the LIST, not for a clock: the guid it carries is the one the join has
            // to quote, and its arrival is the proof the level gate was cleared.
            if battlefield.battlemaster() == Some(master) {
                info!("PROBE bgqueue: list landed — queueing for map {WSG_MAP}");
                let _ = net.0.send(ClientCommand::BattlemasterJoin {
                    battlemaster: master,
                    map_id: WSG_MAP,
                    instance_id: 0,
                    as_group: false,
                });
                probe.phase = Phase::Joined { sent_at: now };
            } else if now - sent_at > 8.0 {
                error!(
                    "PROBE bgqueue: FAILURE — no SMSG_BATTLEFIELD_LIST 8 s after the hello \
                     (a level under the bracket floor is refused here, silently)"
                );
                probe.phase = Phase::Done;
            }
        }
        Phase::Joined { sent_at } => {
            let queued = queue
                .slots()
                .iter()
                .enumerate()
                .find_map(|(i, s)| s.as_ref().map(|(status, _)| (i, status)));
            if let Some((slot, status)) = queued {
                info!(
                    "PROBE bgqueue: QUEUED — slot {slot}, map {}, status {}, instance {} \
                     (the body stays queued; the next plain login is the measurement)",
                    status.map_id, status.status, status.instance_id,
                );
                probe.phase = Phase::Done;
            } else if now - sent_at > 15.0 {
                error!("PROBE bgqueue: FAILURE — no queue slot 15 s after CMSG_BATTLEMASTER_JOIN");
                probe.phase = Phase::Done;
            }
        }
        Phase::Done => {}
    }
}
