//! `--charge`: warrior Charge (spell 100). GM `.learn 100`, teleport near the kobold camp, cast
//! Charge at a creature 8–25 yd out, and require an `SMSG_MONSTER_MOVE` for our OWN guid (Charge is
//! a self spline) — then ack the spline (`CMSG_MOVE_SPLINE_DONE`) and prove the stream survives.

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use benilla_protocol::{guid, EntityKind, SessionEvent};

use crate::probes::{Ctx, Probe};

/// The `--charge` probe spot: open ground ~18 yd west of the kobold camp ([`crate::world::ATTACK_TP`]),
/// so the camp's kobolds stream in at charge range (8–25 yd) while we land *outside* the immediate
/// aggro cluster — Charge refuses to fire while the caster is in combat, so we must cast before a
/// kobold closes and engages us.
const CHARGE_TP: &str = ".go xyz -8798.71 -164.568 81.94";

#[derive(Default)]
pub(crate) struct Charge {
    charge_target: Option<u64>,
    /// The pending self-spline ack: (endpoint, splineId, when-to-send). Captured from the self
    /// `SMSG_MONSTER_MOVE`, sent after the ride's duration elapses — the round-trip that proves our
    /// `CMSG_MOVE_SPLINE_DONE` wire is right (a malformed body drops the session).
    charge_ack: Option<([f32; 3], u32, Instant)>,
    charge_acked: bool,
    /// `World::total` snapshotted when the ack went out; `verify` reads `total - total_at_ack` as the
    /// "packets after the ack" count (the old per-packet `msgs_after_ack` tally, exactly).
    total_at_ack: Option<u32>,
}

impl Probe for Charge {
    fn stage(&mut self, cx: &mut Ctx) -> Result<()> {
        cx.session.send_chat(".learn 100")?; // Charge rank 1 (GM: teach it to the warrior)
        cx.session.send_chat(CHARGE_TP)?;
        println!("sent GM: .learn 100 (Charge); teleport {CHARGE_TP}");
        Ok(())
    }

    fn poll(&mut self, cx: &mut Ctx) -> Result<()> {
        // Fire Charge at a creature in range once we've landed. Charge needs the target 8–25 yd out,
        // so pick the *farthest* streamed creature inside that band (maximises the odds of a valid
        // range, and keeps clear of a kobold already on top of us). One shot — the first packet the
        // server sends back for our own guid is the whole proof.
        if self.charge_target.is_none() {
            if let Some(pos) = cx.world.attack_pos {
                let pick = cx
                    .world
                    .tracked
                    .iter()
                    .filter(|(g, t)| t.kind == EntityKind::Unit && guid::is_creature_or_pet(**g))
                    .map(|(&g, t)| {
                        (
                            g,
                            t.position,
                            (t.position[0] - pos[0]).hypot(t.position[1] - pos[1]),
                        )
                    })
                    .filter(|(_, _, d)| (8.0..=25.0).contains(d))
                    .max_by(|a, b| a.2.total_cmp(&b.2));
                if let Some((guid, tpos, dist)) = pick {
                    // Face the target first — Charge refuses a target that isn't in front (reason 124,
                    // SPELL_FAILED_UNIT_NOT_INFRONT). Report our facing with a Stop at the landing spot
                    // (WoW orientation = atan2(Δy, Δx)), then select + cast.
                    let orientation = (tpos[1] - pos[1]).atan2(tpos[0] - pos[0]);
                    cx.session.stop(pos, orientation)?;
                    cx.session.set_selection(guid)?;
                    cx.session.cast_spell(100, Some(guid))?;
                    println!(
                        "sent CMSG_CAST_SPELL 100 (Charge) at {guid:#x} ({dist:.1} yd, faced)"
                    );
                    self.charge_target = Some(guid);
                }
            }
        }
        // Ack the self-spline once its ride would have finished (`CMSG_MOVE_SPLINE_DONE` at the
        // endpoint). The server holds a player mover as spline-pending until this arrives; a surviving
        // stream afterward is the live proof the ack wire parses.
        if let Some((endpoint, spline_id, at)) = self.charge_ack {
            if !self.charge_acked && Instant::now() >= at {
                cx.session.move_spline_done(endpoint, 0.0, spline_id)?;
                println!(
                    "sent CMSG_MOVE_SPLINE_DONE (splineId {spline_id}) at endpoint ({:.1}, {:.1}, {:.1})",
                    endpoint[0], endpoint[1], endpoint[2]
                );
                self.charge_acked = true;
                self.total_at_ack = Some(cx.world.total);
            }
        }
        Ok(())
    }

    fn on_event(&mut self, ev: &SessionEvent, cx: &mut Ctx) -> Result<()> {
        if let SessionEvent::MonsterMove {
            guid,
            spline_id,
            path,
            stop,
            duration_ms,
            ..
        } = ev
        {
            // Queue the SPLINE_DONE ack for once the ride would have finished:
            // the endpoint is the last waypoint; send after `duration_ms` (+ a
            // margin) so we don't ack a spline the server still thinks is running.
            if *guid == cx.world.self_guid && self.charge_ack.is_none() && !*stop {
                if let Some(&endpoint) = path.last() {
                    let at = Instant::now() + Duration::from_millis(u64::from(*duration_ms) + 200);
                    self.charge_ack = Some((endpoint, *spline_id, at));
                }
            }
        }
        Ok(())
    }

    fn verify(&mut self, cx: &mut Ctx) -> Result<()> {
        // --charge verdict: the port must have landed, a target been found in range and Charge cast, and
        // — the whole point — the server must have driven us with an SMSG_MONSTER_MOVE for our OWN guid.
        if cx.world.attack_pos.is_none() {
            bail!("--charge: the GM teleport never arrived (is the account gmlevel ≥ 2?)");
        }
        let target = self
            .charge_target
            .context("--charge: no creature streamed in charge range [8,25] yd to cast at")?;
        let self_moves = cx.world.self_moves;
        if self_moves == 0 {
            bail!("--charge: cast Charge at {target:#x} but the server sent NO SMSG_MONSTER_MOVE for our guid (check the CAST_RESULT reason above — combat / range / stance)");
        }
        println!("✅ charge: {self_moves} self SMSG_MONSTER_MOVE — Charge drives the caster via a server spline (target {target:#x}).");
        // The ack half: we sent CMSG_MOVE_SPLINE_DONE at the endpoint; a malformed body throws in the
        // server's parser and drops us, so a stream that keeps flowing afterward is the live proof.
        if self.charge_acked {
            let msgs_after_ack = cx.world.total
                - self
                    .total_at_ack
                    .expect("total_at_ack is set together with charge_acked");
            if msgs_after_ack == 0 {
                bail!("--charge: sent CMSG_MOVE_SPLINE_DONE but the stream went silent afterward — the server may have rejected/dropped us (malformed ack body?)");
            }
            println!("✅ charge ack: server accepted CMSG_MOVE_SPLINE_DONE — stream continued ({msgs_after_ack} packets after the ack).");
        } else {
            println!("⚠️  charge ack: never sent (no self spline endpoint captured to ack).");
        }
        Ok(())
    }
}
