//! `--self-res`: the self-resurrect wire (decision 1746) — `PLAYER_SELF_RES_SPELL` arriving at the
//! death, `CMSG_SELF_RES` spending it, and the field zeroing as we stand back up.
//!
//! The whole mechanism is one private descriptor field and one bodyless opcode, so the only way to
//! be sure of either is to watch a real death. The shared [`crate::world::DeathArc`] does the dying
//! — with `hold_release` set, because the button being tested lives on the DEATH dialog and that
//! dialog only exists *before* the release.
//!
//! **Reincarnation, not a soulstone**, because a soulstone needs a second character casting on us
//! while a shaman's passive needs only what a GM account can grant itself: vmangos's
//! `Player::SelectResurrectionSpellId` gates the Reincarnation arm on `HasSpell(20608)` +
//! `IsSpellReady(21169)` + `HasItemCount(17030, 1)` and asks **no class question**, so the staging
//! below arms it on whatever body the slot's probe account has. The two paths converge one line
//! later — both write their effect id into the same field — so the field, the send and the
//! resurrection are the same wire either way.

use anyhow::{bail, Context, Result};
use benilla_protocol::SessionEvent;

use crate::probes::{Ctx, Probe};

/// Reincarnation's learnable passive — the `HasSpell` half of the server's gate.
const REINCARNATION_PASSIVE: u32 = 20608;
/// Reincarnation's *effect* spell — what the server writes into `PLAYER_SELF_RES_SPELL`, and what
/// `Spell.dbc` names **"Reincarnation"** (the DEATH dialog's button text).
const REINCARNATION_EFFECT: u32 = 21169;
/// Ankh — the reagent the same gate counts.
const ITEM_ANKH: u32 = 17030;

#[derive(Default)]
pub(crate) struct SelfRes {
    /// The first non-zero `PLAYER_SELF_RES_SPELL` seen, and whether we were already at 0 health
    /// when it landed — the ordering question the DEATH dialog's `OnShow` read depends on.
    self_res_spell: Option<u32>,
    self_res_after_death: bool,
    sent: bool,
    /// `PLAYER_SELF_RES_SPELL` read back as zero *after* the send — the server spending it.
    field_cleared: bool,
    /// Health back above 0 after the send, without ever having been a ghost.
    revived_without_releasing: bool,
}

impl Probe for SelfRes {
    fn stage(&mut self, cx: &mut Ctx) -> Result<()> {
        // Arm the passive path. Reincarnation has a one-hour cooldown and a **successful run is
        // what sets it**, so without this the probe passes once and then fails for an hour — which
        // is exactly what it did, and the failure reads as "the server refused the gate" rather
        // than "you already proved this". `.cooldown clear` (not `.cooldown`, which is a bare
        // subcommand table and silently does nothing) removes all of the selected unit's
        // cooldowns; with nothing selected `ChatHandler::GetSelectedUnit` falls back to self.
        // The Ankh goes in every run because the resurrection spends it.
        cx.session.send_chat(".cooldown clear")?;
        cx.session
            .send_chat(&format!(".learn {REINCARNATION_PASSIVE}"))?;
        cx.session.send_chat(&format!(".additem {ITEM_ANKH} 1"))?;
        println!(
            "sent GM: .cooldown clear; .learn {REINCARNATION_PASSIVE} (Reincarnation passive); \
             .additem {ITEM_ANKH} (Ankh)"
        );
        Ok(())
    }

    fn poll(&mut self, cx: &mut Ctx) -> Result<()> {
        // Spend it once the death has landed AND the field has arrived — the same two facts the
        // DEATH dialog's button2 waits on (`HasSoulstone()` non-nil while the popup is up).
        let died = cx
            .world
            .death_arc
            .as_ref()
            .is_some_and(|a| a.died_at.is_some());
        if !self.sent && died && self.self_res_spell.is_some() {
            cx.session.self_res()?;
            println!("sent CMSG_SELF_RES (the DEATH dialog's soulstone button)");
            self.sent = true;
        }
        Ok(())
    }

    fn on_event(&mut self, ev: &SessionEvent, cx: &mut Ctx) -> Result<()> {
        let SessionEvent::ObjectValues { guid, fields } = ev else {
            return Ok(());
        };
        if *guid != cx.world.self_guid {
            return Ok(());
        }
        // Read the DELTA for the arrival, not the merged store: the question is which packet
        // carried the field, and a merged read cannot tell "arrived now" from "arrived earlier".
        if let Some(spell) = fields.player_self_res_spell() {
            if self.self_res_spell.is_none() {
                self.self_res_spell = Some(spell);
                self.self_res_after_death = cx
                    .world
                    .death_arc
                    .as_ref()
                    .is_some_and(|a| a.died_at.is_some());
                println!(
                    "PLAYER_SELF_RES_SPELL → {spell} (arrived {} the health→0 flush)",
                    if self.self_res_after_death {
                        "after"
                    } else {
                        "before"
                    }
                );
            }
        } else if self.sent && !self.field_cleared {
            // Zero and absent are the same `None` here, so the clear is only meaningful once the
            // MERGED store also reads none — an unrelated delta must not be mistaken for it.
            if cx
                .world
                .self_fields
                .as_ref()
                .is_some_and(|sf| sf.player_self_res_spell().is_none())
            {
                self.field_cleared = true;
                println!("PLAYER_SELF_RES_SPELL cleared — the server spent it");
            }
        }
        if self.sent && !self.revived_without_releasing {
            if let Some(sf) = &cx.world.self_fields {
                if sf.unit_health().is_some_and(|h| h > 0) && !sf.player_is_ghost() {
                    self.revived_without_releasing = true;
                    println!(
                        "alive again at {} hp, never a ghost — self-resurrected in place",
                        sf.unit_health().unwrap_or(0)
                    );
                }
            }
        }
        Ok(())
    }

    fn verify(&mut self, cx: &mut Ctx) -> Result<()> {
        let arc = cx
            .world
            .death_arc
            .as_ref()
            .expect("death_arc present when --self-res is set");
        arc.death_pos.context(
            "--self-res: `.die` never dropped our health to 0 — is the account gmlevel ≥ 2?",
        )?;
        if arc.ghost_seen {
            bail!(
                "--self-res: released to a ghost — `hold_release` did not hold, and the state \
                 under test (dead, unreleased) was never occupied"
            );
        }
        let spell = self.self_res_spell.context(
            "--self-res: PLAYER_SELF_RES_SPELL never arrived — the server refused the \
             Reincarnation gate (`.learn`/`.additem` rejected, or the effect spell is still on \
             cooldown from a previous run and `.cooldown clear` did not take)",
        )?;
        if spell != REINCARNATION_EFFECT {
            bail!(
                "--self-res: PLAYER_SELF_RES_SPELL = {spell}, expected the Reincarnation EFFECT \
                 {REINCARNATION_EFFECT} (the passive {REINCARNATION_PASSIVE} is what we learn; the \
                 effect is what the field carries and what Spell.dbc names on the button)"
            );
        }
        if !self.field_cleared {
            bail!("--self-res: PLAYER_SELF_RES_SPELL never cleared after CMSG_SELF_RES");
        }
        if !self.revived_without_releasing {
            bail!("--self-res: never came back alive after CMSG_SELF_RES");
        }
        println!(
            "--self-res OK: field {spell} arrived {} the health-zero flush, CMSG_SELF_RES spent \
             it, resurrected dead-unreleased without ever releasing",
            if self.self_res_after_death {
                "after"
            } else {
                "before"
            }
        );
        Ok(())
    }
}
