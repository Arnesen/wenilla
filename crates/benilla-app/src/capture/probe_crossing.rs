//! The sea-crossing live probe (`WOW_PROBE=crossing`) — decision 0455's instrument, inert
//! without the env: once in-world, wait for a cross-continent transport docked on our map,
//! GM-drop onto its deck (`.go xyz`; probe accounts are gmlevel 6), then just stand there and
//! report the seam: aboard → map flip (TRANSFER_PENDING / NEW_WORLD riding branch, logged by
//! the net layer) → still riding → arrived docked on the far continent. Every phase edge prints
//! a `PROBE crossing:` line, so an outer `timeout`d run + grep is the whole harness. Non-combat.
//! Pair with the SLOT-KEYED probe identity (`WOW_USER=probeN WOW_PASS=pprobeN
//! WOW_CHAR=Probe<N-spelled>` for a `pool-N` worktree — method.md "The local vmangos server";
//! a shared account gets kicked by parallel sessions mid-ride).

use bevy::prelude::*;

use super::probes::ProbeClock;
use crate::net::{ClientCommand, Guid, SelfPlayer};
use crate::player::Player;
use crate::transport::{Transport, TransportAnchor};
use benilla_world::world_map::CurrentMap;

pub(crate) struct ProbeCrossingPlugin;

impl Plugin for ProbeCrossingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CrossingProbe>()
            .add_systems(Update, crossing_probe);
    }
}

/// The probe's phase machine. `Wait` → (deck drop sent) `Boarding` → (ride attached) `Aboard` →
/// (CurrentMap flipped, still riding) `Crossed` → (docked on the new map, still riding) done.
/// A failed boarding (window closed under the settle, landed in the drink) retries the next
/// docked window; a lost ride after the flip is a loud FAILURE line.
#[derive(Resource, Default)]
struct CrossingProbe {
    phase: Phase,
    /// The one-shot [`BOOTSTRAP_DOCK`] send — so a body that still cannot see a ferry after the
    /// hop (wrong continent, a `.go` the server refused) doesn't re-teleport every frame.
    bootstrapped: bool,
}

#[derive(Default, PartialEq)]
enum Phase {
    #[default]
    Wait,
    Boarding {
        boat: u64,
        sent_at: f64,
    },
    Aboard {
        boat: u64,
        start_map: u32,
    },
    Crossed {
        boat: u64,
        to_map: u32,
    },
    Done,
}

/// Yards above the boat's sampled origin the GM drop aims: high enough to clear the deck
/// wherever the model origin sits (waterline or deck), low enough that the post-teleport settle
/// hold (6 s) + the free fall land well inside a ~20 s dock window.
const DROP_HEIGHT: f32 = 10.0;
/// Seconds after the deck drop before conceding the boarding failed (settle 6 s + fall + attach,
/// with slack) and re-arming for the next docked window.
const BOARD_DEADLINE: f64 = 15.0;
/// Solid ground on the Booty Bay pier, beside the Ratchet ferry's berth (WoW coords) — where the
/// probe sends itself when no cross-continent transport is in range at all.
///
/// The probe used to *assume* it was standing at a dock: its `Wait` arm only reacts to a transport
/// the server has already put in visibility range, so a probe body parked anywhere else waited
/// forever, printing nothing. That made the instrument unrunnable from a cold login, which is the
/// only way an unattended session ever starts it.
const BOOTSTRAP_DOCK: [f32; 3] = [-14297.2, 531.0, 8.8];
/// `WOW_PROBE_DOCK=x,y,z` — go to *this* dock, unconditionally, before waiting for anything.
///
/// Without it the probe rides whichever cross-continent ferry the login happens to be standing
/// next to, which is not a choice at all: the 1.12 fleet's seams are not interchangeable (one
/// path crosses mid-cycle, another crosses at the cycle wrap), so "it worked" on the ferry that
/// answered says nothing about the one a report names. Overriding the destination is how a
/// specific seam gets measured.
fn dock_override() -> Option<[f32; 3]> {
    let raw = std::env::var("WOW_PROBE_DOCK").ok()?;
    let mut it = raw.split(',').map(|p| p.trim().parse::<f32>());
    match (it.next(), it.next(), it.next(), it.next()) {
        (Some(Ok(x)), Some(Ok(y)), Some(Ok(z)), None) => Some([x, y, z]),
        _ => {
            warn!("WOW_PROBE_DOCK={raw:?} is not `x,y,z` — ignored");
            None
        }
    }
}
/// Seconds the probe watches for a cross-continent transport before sending itself to
/// [`BOOTSTRAP_DOCK`] — long enough for a login's object stream to deliver one if we are already
/// somewhere it sails from.
const BOOTSTRAP_AFTER: f64 = 20.0;

fn crossing_probe(
    time: ProbeClock,
    mut probe: ResMut<CrossingProbe>,
    self_player: Query<(), With<SelfPlayer>>,
    player: Res<Player>,
    current_map: Option<Res<CurrentMap>>,
    transports: Query<(&Guid, &Transport, &TransportAnchor)>,
    net: Res<crate::net::NetCommands>,
) {
    if self_player.is_empty() {
        return;
    }
    let Some(map) = current_map.as_deref().map(|m| m.0) else {
        return;
    };
    let now = time.elapsed_secs_f64();
    match probe.phase {
        Phase::Wait => {
            // Nothing that crosses the sea is in range at all — we are not at a ferry dock. Send
            // the body to one (once; `bootstrapped` latches) rather than waiting out the run.
            let forced = dock_override();
            let adrift = !transports
                .iter()
                .any(|(_, t, _)| t.touches_map(0) && t.touches_map(1));
            if (forced.is_some() || adrift) && !probe.bootstrapped && now > BOOTSTRAP_AFTER {
                probe.bootstrapped = true;
                let [x, y, z] = forced.unwrap_or(BOOTSTRAP_DOCK);
                info!(
                    "PROBE crossing: going to the dock ({x:.1}, {y:.1}, {z:.1}) — {}",
                    if forced.is_some() {
                        "WOW_PROBE_DOCK"
                    } else {
                        "no cross-continent transport in range"
                    }
                );
                let _ = net.0.send(ClientCommand::Chat {
                    kind: crate::net::ChatKind::Say,
                    target: None,
                    text: format!(".go xyz {x} {y} {z} "),
                });
                return;
            }
            if adrift {
                return;
            }
            // A cross-continent transport (the 1.12 fleet crosses EK↔Kalimdor, maps 0↔1),
            // currently docked on OUR map: drop onto its deck. `sample.pos` is WoW coords —
            // exactly what `.go xyz` takes.
            for (guid, transport, anchor) in &transports {
                if !(transport.touches_map(0) && transport.touches_map(1)) {
                    continue;
                }
                let sample = transport.sample_at(anchor, map);
                if sample.map != map || sample.moving {
                    continue;
                }
                let [x, y, z] = sample.pos;
                info!(
                    "PROBE crossing: boat {:#x} docked on map {map} at ({x:.1}, {y:.1}, {z:.1}) \
                     — dropping onto its deck",
                    guid.0
                );
                let _ = net.0.send(ClientCommand::Chat {
                    kind: crate::net::ChatKind::Say,
                    target: None,
                    text: format!(".go xyz {x} {y} {} ", z + DROP_HEIGHT),
                });
                probe.phase = Phase::Boarding {
                    boat: guid.0,
                    sent_at: now,
                };
                break;
            }
        }
        Phase::Boarding { boat, sent_at } => {
            if player.riding() == Some(boat) {
                info!("PROBE crossing: aboard {boat:#x} on map {map} — riding to the seam");
                probe.phase = Phase::Aboard {
                    boat,
                    start_map: map,
                };
            } else if now - sent_at > BOARD_DEADLINE {
                info!("PROBE crossing: boarding missed the window — waiting for the next dock");
                probe.phase = Phase::Wait;
            }
        }
        Phase::Aboard { boat, start_map } => {
            if map != start_map {
                if player.riding() == Some(boat) {
                    info!("PROBE crossing: map flipped {start_map} → {map} STILL ABOARD {boat:#x}");
                    probe.phase = Phase::Crossed { boat, to_map: map };
                } else {
                    error!(
                        "PROBE crossing: FAILURE — map flipped {start_map} → {map} but the ride \
                         did not survive the seam"
                    );
                    probe.phase = Phase::Done;
                }
            } else if player.riding() != Some(boat) {
                info!("PROBE crossing: lost the deck before the seam — re-boarding");
                probe.phase = Phase::Wait;
            }
        }
        Phase::Crossed { boat, to_map } => {
            if player.riding() != Some(boat) {
                error!("PROBE crossing: FAILURE — detached after the flip, before the far dock");
                probe.phase = Phase::Done;
            } else if let Some((_, transport, anchor)) =
                transports.iter().find(|(g, ..)| g.0 == boat)
            {
                let sample = transport.sample_at(anchor, map);
                if sample.map == to_map && !sample.moving {
                    info!(
                        "PROBE crossing: SUCCESS — docked on map {to_map} still aboard {boat:#x}"
                    );
                    probe.phase = Phase::Done;
                }
            }
        }
        Phase::Done => {}
    }
}
