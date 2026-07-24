//! `--attack`: melee-swing wire (decision 0073). GM-teleport onto a Northshire Kobold Vermin, swing
//! the nearest creature, require ≥1 `SMSG_ATTACKERSTATEUPDATE`.

use anyhow::{bail, Context, Result};
use benilla_protocol::{guid, EntityKind};

use crate::probes::{Ctx, Probe};
use crate::world::ATTACK_TP;

#[derive(Default)]
pub(crate) struct Attack {
    attack_target: Option<u64>,
}

impl Probe for Attack {
    fn stage(&mut self, cx: &mut Ctx) -> Result<()> {
        // --loot reuses this teleport spot to guarantee a killable creature in range when no
        // explicit --loot-guid was given; avoid sending it twice if both flags are set (the shared
        // `attack_tp_staged` flag — attack/loot/DeathArc all key off it).
        if !cx.world.attack_tp_staged {
            cx.session.send_chat(ATTACK_TP)?;
            cx.world.attack_tp_staged = true;
            println!("sent GM teleport: {ATTACK_TP}");
        }
        Ok(())
    }

    fn poll(&mut self, cx: &mut Ctx) -> Result<()> {
        // Swing at the nearest creature once we've landed (the port repopulates `tracked` around the
        // kobold spawn; anything within 20 yd of the landing point is a candidate — the nearest is
        // the kobold we landed on, in melee reach).
        if self.attack_target.is_none() {
            if let Some(pos) = cx.world.attack_pos {
                let nearest = cx
                    .world
                    .tracked
                    .iter()
                    .filter(|(g, t)| t.kind == EntityKind::Unit && guid::is_creature_or_pet(**g))
                    .map(|(&g, t)| {
                        let d = (t.position[0] - pos[0]).hypot(t.position[1] - pos[1]);
                        (g, d)
                    })
                    .filter(|(_, d)| *d < 20.0)
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                if let Some((guid, dist)) = nearest {
                    cx.session.set_selection(guid)?;
                    cx.session.attack_swing(guid)?;
                    println!("sent CMSG_ATTACKSWING at {guid:#x} ({dist:.1} yd)");
                    self.attack_target = Some(guid);
                }
            }
        }
        Ok(())
    }

    fn verify(&mut self, cx: &mut Ctx) -> Result<()> {
        // --attack verdict: the port must have landed, the swing gone out, and at least one completed
        // swing decoded — the full decision-0073 trigger chain against the live server.
        if cx.world.attack_pos.is_none() {
            bail!("--attack: the GM teleport never arrived (is the account gmlevel ≥ 2?)");
        }
        let target = self
            .attack_target
            .context("--attack: no creature within 20 yd of the landing")?;
        let swings_seen = cx.world.swings_seen;
        if swings_seen == 0 {
            bail!("--attack: swung at {target:#x} but no SMSG_ATTACKERSTATEUPDATE decoded");
        }
        println!("✅ attack: {swings_seen} SMSG_ATTACKERSTATEUPDATE swing(s) decoded (target {target:#x}).");
        Ok(())
    }
}
