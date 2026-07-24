//! `--death`: the death-arc slice-1 wire (decision 0308). The shared [`crate::world::DeathArc`] runs
//! the die → release → ghost transition; this probe adds the corpse query, the self-revive cleanup,
//! and the post-revive not-found re-query, then asserts the whole arc in wire order.

use anyhow::{bail, Context, Result};
use benilla_protocol::{EntityKind, SessionEvent};

use crate::probes::{Ctx, Probe};

#[derive(Default)]
pub(crate) struct Death {
    corpse_query_sent: bool,
    corpse_answer: Option<(bool, i32, [f32; 3], u32)>,
    revive_sent: bool,
    /// The post-revive corpse re-query — the app's marker-drop mechanism (the server's own
    /// "corpse gone" push is LOOTER-gated, vmangos Map.cpp:3617-3629, so a PvE res never sends
    /// one; the client must re-ask and hear not-found). Director-reported: the map tombstone
    /// survived a spirit-healer res before this was mirrored.
    post_revive_query_sent: bool,
    corpse_gone: Option<bool>,
    corpse_create: Option<(u64, [f32; 3])>,
}

impl Probe for Death {
    fn poll(&mut self, cx: &mut Ctx) -> Result<()> {
        // Once we're a released ghost AND the graveyard teleport has landed, ask where our corpse
        // is — the corpse-run marker's own source (decision 0308 §5).
        if !self.corpse_query_sent {
            if let Some(arc) = &cx.world.death_arc {
                if arc.ghost_seen && arc.graveyard_pos.is_some() {
                    cx.session.corpse_query()?;
                    println!("sent MSG_CORPSE_QUERY");
                    self.corpse_query_sent = true;
                }
            }
        }
        // Once the corpse-query answer is in, GM-revive so the round trip leaves the character
        // alive (cleanup, not part of the wire under test — a real player would corpse-run +
        // CMSG_RECLAIM_CORPSE instead).
        if !self.revive_sent && self.corpse_answer.is_some() {
            cx.session.send_chat(".revive")?;
            println!("sent GM: .revive (self-revive)");
            self.revive_sent = true;
            if let Some(arc) = &mut cx.world.death_arc {
                arc.revive_initiated = true;
            }
        }
        // Once revived, re-ask where the corpse is: the answer MUST be not-found (the corpse
        // unbinds at SpawnCorpseBones) — the round trip the app's unghost-edge marker drop rides.
        if !self.post_revive_query_sent {
            if let Some(arc) = &cx.world.death_arc {
                if arc.revived_seen {
                    cx.session.corpse_query()?;
                    println!("sent MSG_CORPSE_QUERY (post-revive — expecting not-found)");
                    self.post_revive_query_sent = true;
                }
            }
        }
        Ok(())
    }

    fn on_event(&mut self, ev: &SessionEvent, cx: &mut Ctx) -> Result<()> {
        match ev {
            SessionEvent::ObjectCreate {
                guid,
                kind,
                position,
                ..
            } => {
                // --death: the corpse streams in as an `Other`-classified create once
                // we've released (decision 0308 §4). No HIGHGUID_CORPSE classifier
                // exists in `guid.rs` (a corpse carries no template entry to extract
                // from its guid, unlike a creature/GameObject), so identify it
                // positionally instead: the only other wire type `entity_kind` folds
                // into `Other` is a DynamicObject (ground-effect markers), and nothing
                // casts one in this scenario — so any `Other` create arriving after
                // repop is the corpse.
                let repop_sent = cx.world.death_arc.as_ref().is_some_and(|a| a.repop_sent);
                if repop_sent && self.corpse_create.is_none() && *kind == EntityKind::Other {
                    self.corpse_create = Some((*guid, *position));
                    println!(
                        "corpse object streamed: guid {guid:#x} pos ({:.1}, {:.1}, {:.1})",
                        position[0], position[1], position[2]
                    );
                }
            }
            SessionEvent::CorpseQuery {
                found,
                display_map,
                position,
                corpse_map,
            } => {
                if self.post_revive_query_sent {
                    self.corpse_gone = Some(!*found);
                    println!("MSG_CORPSE_QUERY (post-revive): found={found}");
                } else {
                    self.corpse_answer = Some((*found, *display_map, *position, *corpse_map));
                    println!(
                        "MSG_CORPSE_QUERY: found={found} display_map={display_map} pos=({:.1}, {:.1}, {:.1}) corpse_map={corpse_map}",
                        position[0], position[1], position[2]
                    );
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn verify(&mut self, cx: &mut Ctx) -> Result<()> {
        let self_map = cx.world.self_map;
        let arc = cx
            .world
            .death_arc
            .as_ref()
            .expect("death_arc present when --death is set");

        // --death verdict (decision 0308 slice 1): every signal in the release→ghost→corpse arc, in
        // wire order — `bail!` on the first one that never arrived, so a partial run points straight
        // at the phase that broke.
        let death_pos = arc.death_pos.context(
            "--death: `.die` never dropped our health to 0 — is the account gmlevel ≥ 2?",
        )?;
        if !arc.rooted_seen {
            bail!("--death: no SMSG_FORCE_MOVE_ROOT at death");
        }
        if !arc.unroot_seen {
            bail!("--death: no SMSG_FORCE_MOVE_UNROOT after CMSG_REPOP_REQUEST");
        }
        if !arc.water_walk_seen {
            bail!("--death: no SMSG_MOVE_WATER_WALK — the ghost's walk-on-water grant");
        }
        let reclaim_delay_ms = arc
            .reclaim_delay_ms
            .context("--death: no SMSG_CORPSE_RECLAIM_DELAY at release")?;
        if !arc.ghost_seen {
            bail!("--death: PLAYER_FLAGS_GHOST (bit 0x10, field 190) never set");
        }
        // Full 3D distance: the corpse/graveyard/query checks below all compare against where we
        // died, and elevation is as telling as the ground plane (a graveyard directly below/above
        // death, or a corpse object mis-z'd, are both real wire bugs — not noise to filter out).
        let dist3 = |a: [f32; 3], b: [f32; 3]| {
            ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
        };
        let (corpse_guid, corpse_pos) = self
            .corpse_create
            .context("--death: the corpse object never streamed / streamed at the wrong spot")?;
        let corpse_dist = dist3(corpse_pos, death_pos);
        if corpse_dist > 10.0 {
            bail!(
                "--death: the corpse object never streamed / streamed at the wrong spot ({corpse_dist:.1} yd from where we died)"
            );
        }
        let graveyard_pos = arc
            .graveyard_pos
            .context("--death: no graveyard teleport after release")?;
        let graveyard_dist = dist3(graveyard_pos, death_pos);
        if graveyard_dist <= 20.0 {
            bail!(
                "--death: no graveyard teleport after release (landed only {graveyard_dist:.1} yd from where we died)"
            );
        }
        let (found, _display_map, query_pos, query_map) = self
            .corpse_answer
            .context("--death: MSG_CORPSE_QUERY answered not-found / wrong spot")?;
        let query_dist = dist3(query_pos, death_pos);
        if !found || query_dist > 10.0 || query_map != self_map {
            bail!(
                "--death: MSG_CORPSE_QUERY answered not-found / wrong spot (found={found}, {query_dist:.1} yd from death, map {query_map} vs {})",
                self_map
            );
        }
        if !arc.revived_seen {
            bail!(
                "--death: `.revive` never cleared the ghost flag — the character may be left dead: run with --say \".revive\" to clean up"
            );
        }
        let corpse_gone = self.corpse_gone;
        if corpse_gone != Some(true) {
            bail!(
                "--death: the post-revive corpse query still finds a corpse (got {corpse_gone:?}) — the app's unghost-edge re-query would leave the map tombstone standing"
            );
        }

        println!("\n✅ DEATH ARC SLICE-1 WIRE VERIFIED:");
        println!(
            "  died at        ({:.1}, {:.1}, {:.1})",
            death_pos[0], death_pos[1], death_pos[2]
        );
        println!("  root/unroot    SMSG_FORCE_MOVE_ROOT + SMSG_FORCE_MOVE_UNROOT acked");
        println!("  water-walk     SMSG_MOVE_WATER_WALK acked");
        println!("  reclaim delay  {reclaim_delay_ms} ms");
        println!("  ghost flag     PLAYER_FLAGS_GHOST set at release, cleared at revive");
        println!(
            "  corpse         guid {corpse_guid:#x} pos ({:.1}, {:.1}, {:.1})  ({corpse_dist:.1} yd from death)",
            corpse_pos[0], corpse_pos[1], corpse_pos[2]
        );
        println!(
            "  graveyard      ({:.1}, {:.1}, {:.1})  ({graveyard_dist:.1} yd from death)",
            graveyard_pos[0], graveyard_pos[1], graveyard_pos[2]
        );
        println!("  corpse query   found, map {query_map}  ({query_dist:.1} yd from death)");
        println!("  revive         PLAYER_FLAGS_GHOST cleared — character alive");
        Ok(())
    }
}
