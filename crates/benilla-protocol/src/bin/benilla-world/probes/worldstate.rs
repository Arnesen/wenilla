//! `--worldstate`: the world-state table's two wires, end to end against the live server.
//!
//! Both are worth a live run for different reasons. `SMSG_UPDATE_WORLD_STATE` is *deterministic* —
//! `.debug send worldstate <id> <value>` makes the server send one pair we chose, so the probe can
//! require that exact pair back and a wrong reader shows up as a mismatch rather than a plausible
//! number. `SMSG_INIT_WORLD_STATES` is the one with real parse risk (two leading dwords, a `u16`
//! count, a run of pairs, a `(0,0)` terminator that *is* counted) and it can't be requested — so
//! the probe forces one by teleporting across a zone boundary, which is what makes vmangos re-send
//! it (`Player::UpdateZone` → `SendInitWorldStates`, `Player.cpp:6660`).
//!
//! **The update leg needs a temporary grant.** `.debug send worldstate` sits in
//! `debugSendCommandTable` at **SEC_DEVELOPER = 5** (vmangos `Chat.cpp:323`) — one above the
//! gmlevel 6 the slot-keyed probe accounts carry (decisions 0450/0651). Without it the server answers
//! *"This command is not available to you."* in chat and sends no packet, and the probe fails on
//! the update leg while the init leg still passes.
//!
//! The effective level lives in **`realmd.account_access`**, not `realmd.account.gmlevel` —
//! mangosd reads the former (rows per `RealmID`, plus a `-1` catch-all) and ignores the latter for
//! command gating. Grant, run, revert, with the account offline:
//!
//! ```text
//! UPDATE realmd.account_access SET gmlevel=5 WHERE id=<account id>;   -- both rows
//! cargo run -p benilla-protocol --bin benilla-world -- probeN pprobeN --worldstate --seconds 16
//! UPDATE realmd.account_access SET gmlevel=3 WHERE id=<account id>;   -- put it straight back
//! ```

use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use benilla_protocol::SessionEvent;

use crate::probes::{Ctx, Probe};

/// Northshire (zone 9, Elwynn Forest) — the same spot the quest probes stage on.
const ELWYNN_TP: &str = ".go xyz -8902.59 -162.606 82.0223";
/// Stormwind's Trade District (zone 1519) — a different zone on the same map, so the hop from
/// [`ELWYNN_TP`] is a genuine `UpdateZone` and forces a fresh `SMSG_INIT_WORLD_STATES`.
const STORMWIND_TP: &str = ".go xyz -8913.0 554.0 93.8";
/// How long to sit in Elwynn before hopping, so the first zone's init has landed and the second is
/// unambiguously a *second* packet.
const HOP_AFTER: Duration = Duration::from_secs(4);

/// A synthetic `(id, value)` no real content uses, so a match proves our reader and not a
/// coincidence with a live Silithus/AQ counter.
const PROBE_ID: u32 = 0xBEEF;
const PROBE_VALUE: u32 = 1_234_567;

/// One received `SMSG_INIT_WORLD_STATES`: the packet's `(map, zone)` scope and its pair run
/// verbatim — terminator included, which [`WorldState::verify`] asserts on.
struct Init {
    map: u32,
    zone: u32,
    states: Vec<(u32, u32)>,
}

#[derive(Default)]
pub(crate) struct WorldState {
    staged_at: Option<Instant>,
    hopped: bool,
    inits: Vec<Init>,
    /// Every `SMSG_UPDATE_WORLD_STATE` pair.
    updates: Vec<(u32, u32)>,
}

impl Probe for WorldState {
    fn stage(&mut self, cx: &mut Ctx) -> Result<()> {
        cx.session.send_chat(ELWYNN_TP)?;
        self.staged_at = Some(Instant::now());
        println!("worldstate: teleported to Elwynn (zone 9) {ELWYNN_TP}");
        Ok(())
    }

    fn poll(&mut self, cx: &mut Ctx) -> Result<()> {
        if self.hopped || self.staged_at.is_none_or(|t| t.elapsed() < HOP_AFTER) {
            return Ok(());
        }
        self.hopped = true;
        // The zone hop (forces an init) and the one deterministic pair, back to back.
        cx.session.send_chat(STORMWIND_TP)?;
        cx.session
            .send_chat(&format!(".debug send worldstate {PROBE_ID} {PROBE_VALUE}"))?;
        println!(
            "worldstate: hopped to Stormwind (zone 1519) and sent .debug send worldstate {PROBE_ID} {PROBE_VALUE}"
        );
        Ok(())
    }

    fn on_event(&mut self, ev: &SessionEvent, _cx: &mut Ctx) -> Result<()> {
        if let SessionEvent::WorldStates { scope, states } = ev {
            match *scope {
                Some((map, zone)) => self.inits.push(Init {
                    map,
                    zone,
                    states: states.clone(),
                }),
                None => self.updates.extend(states.iter().copied()),
            }
        }
        Ok(())
    }

    fn verify(&mut self, _cx: &mut Ctx) -> Result<()> {
        println!("\n--- world states ---");
        for init in &self.inits {
            println!(
                "SMSG_INIT_WORLD_STATES  map {} zone {}  {} pair(s)",
                init.map,
                init.zone,
                init.states.len()
            );
            for (id, value) in &init.states {
                println!("    {id:>6} = {:<12} (raw {value:#010x})", *value as i32);
            }
        }
        for (id, value) in &self.updates {
            println!(
                "SMSG_UPDATE_WORLD_STATE  {id} = {} (raw {value:#010x})",
                *value as i32
            );
        }

        if self.inits.is_empty() {
            bail!("no SMSG_INIT_WORLD_STATES arrived — the zone hop should have forced one");
        }
        // The terminator is counted by the server and read by us: a well-formed init ends on (0,0).
        // Assert it rather than assume, since dropping it would be the natural "helpful" bug.
        for init in &self.inits {
            if init.states.last() != Some(&(0, 0)) {
                bail!(
                    "init for map {} zone {} did not end on the (0,0) terminator: {:?}",
                    init.map,
                    init.zone,
                    init.states.last()
                );
            }
        }
        if !self.updates.contains(&(PROBE_ID, PROBE_VALUE)) {
            bail!(
                "no SMSG_UPDATE_WORLD_STATE carrying our ({PROBE_ID}, {PROBE_VALUE}) — got {:?}. \
                 `.debug send worldstate` is gmlevel 5 (SEC_DEVELOPER); a lower account is silently refused",
                self.updates
            );
        }
        println!(
            "\nworldstate: OK — {} init packet(s), and the ({PROBE_ID}, {PROBE_VALUE}) round trip landed",
            self.inits.len()
        );
        Ok(())
    }
}
