//! `--mount-tele`: **what the server actually sends when a teleport dismounts you** — B213's wire.
//!
//! The report: mount up, teleport somewhere that forbids mounting, and the auto-dismount leaves the
//! mount's run speed on your feet. vmangos does that strip inside the worldport ack handler, three
//! statements after the destination map's own create block goes out
//! (`MovementHandler.cpp` `HandleMoveWorldportAckOpcode`: `Map::Add` → `SendInitSelf`, then
//! `if (!mEntry->IsMountAllowed()) RemoveSpellsCausingAura(SPELL_AURA_MOUNTED)`), so the two land
//! back to back in one tick. Reading the source says they *should*; this probe measures that they
//! *do*, on the running deploy, and prints the gap.
//!
//! The scenario: `.aura` a real mount spell on open ground (a genuine `SPELL_AURA_MOUNTED` holder —
//! `.modify mount` sets a display id with no aura and would never be stripped), require the mounted
//! `SMSG_FORCE_RUN_SPEED_CHANGE`, then `.go xyz` into a dungeon map and require, in order: the
//! `SMSG_NEW_WORLD`, a self create block still carrying the **mounted** run speed, and the
//! dismount's `SMSG_FORCE_RUN_SPEED_CHANGE` back at base. Needs a GM account (the deploy's probes
//! are gmlevel 6).

use std::time::Instant;

use anyhow::{ensure, Result};
use benilla_protocol::{SessionEvent, SpeedKind};

use crate::probes::{Ctx, Probe};
use crate::world::ATTACK_TP;

/// Brown Horse (the 60% apprentice mount): `SPELL_AURA_MOUNTED` + `SPELL_AURA_MOD_INCREASE_MOUNTED_SPEED`,
/// so `.aura` on it produces the same holder a real mount cast would — the one
/// `RemoveSpellsCausingAura(SPELL_AURA_MOUNTED)` goes looking for.
const MOUNT_SPELL: u32 = 458;

/// Ragefire Chasm. A dungeon, and not one of `MapEntry::IsMountAllowed`'s four exceptions
/// (Zul'Gurub, Zul'Farrak, AQ Ruins, Caverns of Time), so arriving there strips the mount.
const DUNGEON_MAP: u32 = 389;

/// Inside Ragefire Chasm's entrance. The landing spot does not matter — the packets we want are
/// sent by the arrival itself — but a real one keeps the character somewhere sane if a run is
/// interrupted before the exit teleport in `verify`.
const DUNGEON_TP: &str = ".go xyz 3.0 -14.0 -18.0 389";

/// 1.12.1's base run speed (yd/s) — the value the strip must come back to.
const BASE_RUN: f32 = 7.0;

/// The map the scenario has to start on — open ground where a mount is allowed.
const OUTDOOR_MAP: u32 = 0;

#[derive(Default)]
pub(crate) struct MountTele {
    /// Sent `.aura` once we were confirmed standing on [`OUTDOOR_MAP`].
    mount_requested: bool,
    /// The mounted run speed the pre-teleport `SMSG_FORCE_RUN_SPEED_CHANGE` announced.
    mounted_run: Option<f32>,
    ported: bool,
    /// `SMSG_NEW_WORLD`'s map, and when it landed.
    arrived: Option<(u32, Instant)>,
    /// Our own create block on the destination map: its `LIVING` run speed + arrival instant.
    create_after: Option<(f32, Instant)>,
    /// The first self Run force-change after that create: its speed + arrival instant.
    strip_after: Option<(f32, Instant)>,
}

impl Probe for MountTele {
    fn stage(&mut self, cx: &mut Ctx) -> Result<()> {
        // Get onto open ground and clean of any leftover holder FIRST — an interrupted earlier run
        // can leave the character mounted inside the dungeon, and from there the scenario's own
        // teleport is a same-map port that sends no `SMSG_NEW_WORLD` at all. The map id is
        // explicit for the same reason `verify`'s exit is (`.go xyz`'s map argument defaults to
        // the map you are standing on). Mounting waits for the arrival, in `poll`.
        cx.session.send_chat(&format!(".unaura {MOUNT_SPELL}"))?;
        cx.session
            .send_chat(&format!("{ATTACK_TP} {OUTDOOR_MAP}"))?;
        cx.world.attack_tp_staged = true;
        Ok(())
    }

    fn on_event(&mut self, ev: &SessionEvent, cx: &mut Ctx) -> Result<()> {
        match ev {
            // The mount landed: its speed change is our cue to teleport. Gate on the *tracked*
            // self like `--speed` does — World only acks once our pose is known, and an unacked
            // change is one vmangos has not applied yet.
            SessionEvent::ForceSpeedChange {
                guid, kind, speed, ..
            } if *guid == cx.world.self_guid && *kind == SpeedKind::Run => {
                if !cx.world.tracked.contains_key(guid) {
                    return Ok(());
                }
                if self.ported {
                    // Post-arrival: the first Run change after the destination's create block is
                    // the dismount we came for.
                    if self.create_after.is_some() && self.strip_after.is_none() {
                        self.strip_after = Some((*speed, Instant::now()));
                    }
                } else if self.mounted_run.is_none() && *speed > BASE_RUN + 0.05 {
                    self.mounted_run = Some(*speed);
                    println!("mounted: run {speed} yd/s — teleporting into map {DUNGEON_MAP}");
                }
            }
            // `SMSG_LOGIN_VERIFY_WORLD` decodes to this too, so login itself announces map 0 —
            // gate on having actually sent the teleport, or the whole scenario latches onto the
            // login and every later check reads the wrong packets.
            SessionEvent::Worldport { map_id, .. } if self.ported && self.arrived.is_none() => {
                self.arrived = Some((*map_id, Instant::now()));
                println!("SMSG_NEW_WORLD: map {map_id}");
            }
            // Our own create on the destination map — sent by `Map::Add` → `SendInitSelf`, before
            // the strip. Its LIVING block is the speed set the client seeds a fresh entity with.
            SessionEvent::ObjectCreate { guid, speeds, .. }
                if *guid == cx.world.self_guid && self.ported && self.arrived.is_some() =>
            {
                if let (None, Some(s)) = (self.create_after, speeds) {
                    self.create_after = Some((s.run, Instant::now()));
                    println!("self create block on the new map: run {} yd/s", s.run);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn poll(&mut self, cx: &mut Ctx) -> Result<()> {
        // Mount only once the staging teleport has actually landed us outdoors — proving the run's
        // starting state from a packet rather than assuming it (`method.md` step 6).
        if !self.mount_requested && cx.world.self_map == OUTDOOR_MAP {
            self.mount_requested = true;
            cx.session.send_chat(&format!(".aura {MOUNT_SPELL}"))?;
            println!(
                "on map {OUTDOOR_MAP}; sent GM: .aura {MOUNT_SPELL} (Brown Horse) — expecting the \
                 mounted run speed"
            );
        }
        if self.mounted_run.is_some() && !self.ported {
            self.ported = true;
            cx.session.send_chat(DUNGEON_TP)?;
        }
        Ok(())
    }

    fn verify(&mut self, cx: &mut Ctx) -> Result<()> {
        // Leave the character as found before asserting: a failed run must not strand it in a
        // dungeon on a mount. The exit names map 0 explicitly — `.go xyz`'s map argument is
        // OPTIONAL and defaults to the map you are standing on, so the bare [`ATTACK_TP`] would
        // port us to Northshire's coordinates *inside Ragefire Chasm*.
        cx.session.send_chat(&format!(".unaura {MOUNT_SPELL}"))?;
        cx.session
            .send_chat(&format!("{ATTACK_TP} {OUTDOOR_MAP}"))?;
        // …and stay on the wire long enough to ACK that exit port. A far teleport the client never
        // acks does not happen: dropping the session here would leave the probe character standing
        // in a dungeon for the next run to find. Bounded, and only advisory — `stage` recovers from
        // a stranded character anyway, so a quiet stream here is not a failure.
        let until = Instant::now() + std::time::Duration::from_secs(5);
        while Instant::now() < until && cx.world.self_map != OUTDOOR_MAP {
            match cx.session.recv() {
                Ok(msg) => {
                    for ev in benilla_protocol::decode(msg) {
                        cx.world.on_event(&ev, cx.session)?;
                    }
                }
                Err(_) => continue,
            }
        }

        ensure!(
            self.mount_requested,
            "--mount-tele: never reached map {OUTDOOR_MAP} — the staging teleport did not land \
             (raise --seconds, or check the `.go` GM level)"
        );
        let mounted = self.mounted_run.ensure_some(
            "--mount-tele: no mounted run speed arrived — did `.aura` take (GM account?), or does \
             this build's mount spell differ?",
        )?;
        let (map, _) = self
            .arrived
            .ensure_some("--mount-tele: no SMSG_NEW_WORLD — the `.go xyz <map>` never ported us")?;
        ensure!(
            map == DUNGEON_MAP,
            "--mount-tele: ported to map {map}, wanted {DUNGEON_MAP}"
        );
        let (create_run, create_at) = self.create_after.ensure_some(
            "--mount-tele: no self create block on the destination map — was the worldport acked?",
        )?;
        let (strip_run, strip_at) = self.strip_after.ensure_some(
            "--mount-tele: the arrival sent no Run force-change — this map did not strip the mount",
        )?;

        ensure!(
            (create_run - mounted).abs() < 0.01,
            "--mount-tele: the destination's create block should still carry the MOUNTED run \
             speed ({mounted}), got {create_run} — the strip would then precede the create and \
             B213's ordering would not be this one"
        );
        ensure!(
            (strip_run - BASE_RUN).abs() < 0.01,
            "--mount-tele: the dismount should restore the {BASE_RUN} base run speed, got {strip_run}"
        );
        let gap = strip_at.duration_since(create_at);
        println!(
            "\n--mount-tele PASS: arrived on map {map}; create block carried the mount's \
             {create_run} yd/s, the dismount's SMSG_FORCE_RUN_SPEED_CHANGE followed {} µs later \
             at {strip_run} yd/s.\n  Both packets are written by one HandleMoveWorldportAckOpcode \
             call, so a client that drains its socket once a frame sees them in ONE drain — which \
             is what B213 (decision 1478) turns on.",
            gap.as_micros()
        );
        Ok(())
    }
}

/// `Option::ok_or_else` with the probe's message shape, so each `verify` line reads as one claim.
trait EnsureSome<T> {
    fn ensure_some(&self, msg: &str) -> Result<T>;
}

impl<T: Copy> EnsureSome<T> for Option<T> {
    fn ensure_some(&self, msg: &str) -> Result<T> {
        self.ok_or_else(|| anyhow::anyhow!("{msg}"))
    }
}
