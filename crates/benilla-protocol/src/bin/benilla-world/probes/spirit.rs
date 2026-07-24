//! `--spirit`: the spirit-healer res + the 25% durability loss's wire (decision 0318). Repair to a
//! full baseline (the [`crate::world::DeathArc`] dies + releases), teleport onto the graveyard's
//! Spirit Healer, `CMSG_SPIRIT_HEALER_ACTIVATE`, and require BOTH the res (ghost flag clears) and
//! the post-activate `ITEM_FIELD_DURABILITY` deltas the loss must push.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use benilla_protocol::{guid, EntityKind, ObjectFields, SessionEvent};

use crate::probes::{Ctx, Probe};

/// The merged per-item descriptor stores (create seed + every values-delta — the same shape the
/// app's `Items` store holds), the durability baseline snapshotted at activate time, and the raw
/// post-activate durability deltas the verdict requires.
#[derive(Default)]
pub(crate) struct Spirit {
    healer: Option<(u64, [f32; 3])>,
    healer_tp_sent: bool,
    healer_tp_landed: Option<Instant>,
    activate_sent: bool,
    dur_baseline: HashMap<u64, (u32, u32)>,
    dur_deltas: Vec<(u64, u32)>,
    item_fields: HashMap<u64, ObjectFields>,
}

impl Probe for Spirit {
    fn stage(&mut self, cx: &mut Ctx) -> Result<()> {
        // A full-durability baseline (`.repairitems`, vmangos `Chat.cpp:1297` SEC_GAMEMASTER):
        // every post-activate delta must then read as a DROP, idempotent across probe re-runs
        // (each spirit res costs a real 25%).
        cx.session.send_chat(".repairitems")?;
        println!("sent GM: .repairitems (full-durability baseline)");
        Ok(())
    }

    fn poll(&mut self, cx: &mut Ctx) -> Result<()> {
        // --spirit: once we're a released ghost and the graveyard's Spirit Healer has streamed,
        // teleport onto it — the repop spot can land outside the activate's interaction gate
        // (vmangos `GetNPCIfCanInteractWith`, INTERACTION_DISTANCE 5 yd).
        if !self.healer_tp_sent
            && cx
                .world
                .death_arc
                .as_ref()
                .is_some_and(|a| a.ghost_seen && a.graveyard_pos.is_some())
        {
            if let Some((hg, hp)) = self.healer {
                cx.session
                    .send_chat(&format!(".go xyz {} {} {}", hp[0], hp[1], hp[2]))?;
                println!("sent GM teleport onto Spirit Healer (guid {hg:#x})");
                self.healer_tp_sent = true;
            }
        }
        // --spirit: standing on the healer, snapshot the durability baseline (the merged item
        // stores: create seed + the `.repairitems` deltas) and ask for the res — the XP_LOSS
        // popup's accept. The 2s settle after the teleport ack lets the server PROCESS the ack
        // first: the movement-ack and world-packet queues are separate, and an activate racing
        // its own teleport ack is range-checked from the PRE-teleport spot (>5 yd → silently
        // refused; live-observed on the first probe run).
        if !self.activate_sent {
            if let (Some((hg, hp)), Some(landed)) = (self.healer, self.healer_tp_landed) {
                let (pos, _) = cx.world.self_pose();
                let d2 =
                    (pos[0] - hp[0]).powi(2) + (pos[1] - hp[1]).powi(2) + (pos[2] - hp[2]).powi(2);
                if d2 < 25.0 && landed.elapsed() > Duration::from_secs(2) {
                    self.dur_baseline = self
                        .item_fields
                        .iter()
                        .filter_map(|(&g, f)| {
                            f.item_durability()
                                .zip(f.item_max_durability())
                                .filter(|&(_, max)| max > 0)
                                .map(|p| (g, p))
                        })
                        .collect();
                    cx.session.spirit_healer_activate(hg)?;
                    println!(
                        "sent CMSG_SPIRIT_HEALER_ACTIVATE (guid {hg:#x}) — baseline: {} durable item(s)",
                        self.dur_baseline.len()
                    );
                    self.activate_sent = true;
                    if let Some(arc) = &mut cx.world.death_arc {
                        arc.revive_initiated = true;
                    }
                }
            }
        }
        Ok(())
    }

    fn on_event(&mut self, ev: &SessionEvent, cx: &mut Ctx) -> Result<()> {
        match ev {
            // --spirit: the graveyard's Spirit Healer advertises UNIT_NPC_FLAG_SPIRITHEALER (0x20 —
            // vmangos `UnitDefines.h:662`, the flag `GetNPCIfCanInteractWith` gates the activate on).
            // It only streams to ghosts, so it can't appear before release. (The capture condition is
            // the arm guard so the whole match reads as one dispatch — clippy's collapsible_match.)
            SessionEvent::ObjectCreate {
                guid,
                kind,
                position,
                fields,
                ..
            } if *kind == EntityKind::Unit && fields.unit_npc_flags() & 0x20 != 0 => {
                if self.healer.is_none() {
                    println!(
                        "Spirit Healer streamed: guid {guid:#x} at ({:.1}, {:.1}, {:.1})",
                        position[0], position[1], position[2]
                    );
                }
                self.healer = Some((*guid, *position));
            }
            SessionEvent::ItemCreate { guid, fields, .. } => {
                // --spirit: seed/overlay the item's merged store (the app's `Items`
                // discipline) — the durability baseline reads off these. The print
                // is the created-semantics live proof: a broken item's create OMITS
                // its zero `DURABILITY` word, and the pair must still read `0/max`
                // (the director's "100% on broken gear" bug).
                if let Some((d, m)) = fields
                    .item_durability()
                    .zip(fields.item_max_durability())
                    .filter(|&(_, m)| m > 0)
                {
                    println!(
                        "item create: guid {guid:#x} entry {} durability {d}/{m}",
                        fields.object_entry().unwrap_or(0)
                    );
                }
                match self.item_fields.entry(*guid) {
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        e.get_mut().merge(fields.clone())
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(fields.clone());
                    }
                }
            }
            // --spirit: an item values-delta — the durability wire under test. Log
            // every `ITEM_FIELD_DURABILITY` it carries (post-activate ones are the
            // verdict's evidence) and merge into the item's store.
            SessionEvent::ObjectValues { guid, fields } if guid::is_item(*guid) => {
                if let Some(d) = fields.item_durability() {
                    if self.activate_sent {
                        self.dur_deltas.push((*guid, d));
                    }
                    println!(
                        "item durability delta: guid {guid:#x} → {d}{}",
                        if self.activate_sent {
                            " (post-activate)"
                        } else {
                            ""
                        }
                    );
                }
                match self.item_fields.entry(*guid) {
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        e.get_mut().merge(fields.clone())
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(fields.clone());
                    }
                }
            }
            // The old Teleport arm's `else if cli.spirit … healer_tp_landed` branch. Its condition is
            // equivalent to the else-if: `healer_tp_sent` can only become true after `graveyard_pos`
            // is Some (the healer-TP poll requires it), so this and World's graveyard capture (the
            // other branch) are temporally disjoint. (Folded into the arm guard — collapsible_match.)
            SessionEvent::Teleport { guid, .. }
                if *guid == cx.world.self_guid
                    && self.healer_tp_sent
                    && self.healer_tp_landed.is_none() =>
            {
                self.healer_tp_landed = Some(Instant::now());
            }
            _ => {}
        }
        Ok(())
    }

    fn verify(&mut self, cx: &mut Ctx) -> Result<()> {
        let ghost_seen = cx.world.death_arc.as_ref().is_some_and(|a| a.ghost_seen);
        let revived_seen = cx.world.death_arc.as_ref().is_some_and(|a| a.revived_seen);
        let healer_tp_sent = self.healer_tp_sent;
        let session = &mut *cx.session;

        // Cleanup before judging: the res left the character alive at 75% — a GM repair keeps
        // re-runs (and the shared GM character's gear) clean. Fire-and-forget; the stream is done.
        session.send_chat(".repairitems")?;

        if !self.activate_sent {
            bail!(
                "--spirit: never reached the activate (ghost={ghost_seen}, healer streamed={}, tp sent={healer_tp_sent})",
                self.healer.is_some()
            );
        }
        if !revived_seen {
            // Don't leave the shared GM character a ghost (live-observed: the first run did).
            session.send_chat(".revive")?;
            bail!(
                "--spirit: CMSG_SPIRIT_HEALER_ACTIVATE never cleared the ghost flag — the res didn't land (a cleanup .revive was sent)"
            );
        }
        if self.dur_baseline.is_empty() {
            bail!(
                "--spirit: no durable items in the baseline — equip the character with gear that has MaxDurability"
            );
        }
        if self.dur_deltas.is_empty() {
            bail!(
                "--spirit: the res landed but the wire carried NO post-activate ITEM_FIELD_DURABILITY delta ({} durable item(s) at baseline) — the 25% loss never reached the client; a tooltip can only show 100%",
                self.dur_baseline.len()
            );
        }
        println!("\n✅ SPIRIT-HEALER RES + DURABILITY WIRE VERIFIED:");
        println!("  res            ghost flag cleared after CMSG_SPIRIT_HEALER_ACTIVATE");
        println!(
            "  deltas         {} post-activate durability delta(s) over {} durable item(s):",
            self.dur_deltas.len(),
            self.dur_baseline.len()
        );
        let mut dropped = 0u32;
        for (guid, after) in &self.dur_deltas {
            let entry = cx.world.item_entries.get(guid).copied().unwrap_or(0);
            match self.dur_baseline.get(guid) {
                Some(&(before, max)) => {
                    if *after < before {
                        dropped += 1;
                    }
                    println!(
                        "    item {entry:>5} (guid {guid:#x})  {before}/{max} → {after}/{max}"
                    );
                }
                None => println!("    item {entry:>5} (guid {guid:#x})  ?/? → {after}"),
            }
        }
        if dropped == 0 {
            bail!(
                "--spirit: durability deltas arrived but none DROPPED below its baseline — the loss is not visible in the values"
            );
        }
        Ok(())
    }
}
