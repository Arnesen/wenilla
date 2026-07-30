//! The melee live probe (`WOW_PROBE=melee`) — an agent-side instrument, inert without the env:
//! once in-world, periodically fire the attack-nearest core ([`AttackNearestRequest`] — the
//! action-bar attack's own no-selection path), so the probe character fights whatever is closest
//! while the `dbg_trace` sink (`WOW_MOVE_TRACE=<path>`) records the swing→impact→spawn combat-text
//! timeline for pair-birth analysis. Pair with the slot-keyed probe identity (method.md) + an outer
//! `timeout`.
//!
//! **Do NOT run this unattended** (director's rule, 2026-07-14, `method.md` "The local vmangos
//! server"): the probe fights back-to-back with no health awareness and the character DIES (Tri
//! did). Combat runs are director-assisted — ask, or hand an eyeball script.

use bevy::prelude::*;

use super::probes::ProbeClock;
use crate::net::SelfPlayer;
use crate::target::AttackNearestRequest;

pub(crate) struct ProbeMeleePlugin;

impl Plugin for ProbeMeleePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, melee_probe);
    }
}

/// Every few seconds: with a live selection, (re)send the attack swing at it (idempotent while
/// already auto-attacking; covers the auto-acquired-attacker case the acquire core skips); with
/// none, run the acquire core. Re-acquires the next victim after a kill clears the selection.
fn melee_probe(
    time: ProbeClock,
    mut last: Local<f64>,
    self_player: Query<(), With<SelfPlayer>>,
    selection: Res<crate::target::Selection>,
    net: Res<crate::net::NetCommands>,
    mut requests: MessageWriter<AttackNearestRequest>,
) {
    if self_player.is_empty() {
        return;
    }
    let now = time.elapsed_secs_f64();
    if now - *last >= 3.0 {
        *last = now;
        if let Some(guid) = selection.guid {
            info!("melee probe: swinging at {guid:#x}");
            let _ = net.0.send(crate::net::ClientCommand::AttackSwing { guid });
        } else {
            info!("melee probe: acquiring nearest");
            requests.write(AttackNearestRequest);
        }
    }
}
