//! `--spells`: the spell/action wire. Require `SMSG_INITIAL_SPELLS` + `SMSG_ACTION_BUTTONS` at
//! login, cast one spell (self, then packed-guid targeted), require a `SMSG_CAST_RESULT` verdict,
//! and read our inventory back out of the descriptor as the round-trip evidence.

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use benilla_protocol::{decode, guid, EntityKind, SessionEvent};

use crate::probes::{Ctx, Probe};

/// The dest-cast phase's spell: 4054 "Rough Dynamite" — dest-targeted (`Targets = 0x40`), zero
/// mana, no reagents/totems, no equipped-item requirement, no aura state (live `spell_template`
/// sweep, decision 0792) — castable by ANY class the probe character happens to be, once GM-learnt.
const DEST_SPELL: u32 = 4054;

#[derive(Default)]
pub(crate) struct Spells {
    cast_sent: Option<u32>,
    targeted_cast_sent: Option<(u32, u64)>,
    dest_cast_sent: Option<[f32; 3]>,
    dest_learn_sent: bool,
    /// Set by `SMSG_LEARNED_SPELL` naming [`DEST_SPELL`] — the gate the dest cast waits behind.
    /// The `.learn` chat command executes DEFERRED on the server (past the session's opcode
    /// batch), so a cast sent in the same batch beats it and is silently dropped as unknown
    /// ("casts spell 4054 which he shouldn't have" — observed live, 2026-07-30).
    dest_spell_known: bool,
}

impl Probe for Spells {
    fn poll(&mut self, cx: &mut Ctx) -> Result<()> {
        // Cast once both the book and the bar are known. The pick must be an *active* spell —
        // vmangos silently drops a cast of a passive (HandleCastSpellOpcode returns before any
        // CAST_RESULT), and the book mixes proficiencies/passives in. A spell the player slotted
        // on the action bar is active by construction (6603 auto-attack excluded: it's the
        // attack toggle, not a cast); Battle Shout 6673 is the fallback. Either verdict (ok or a
        // failure reason like no-rage/bad-target) proves the round trip.
        if self.cast_sent.is_none() {
            if let (Some(book), Some(bar)) = (&cx.world.spell_book, &cx.world.bar_spells) {
                let spell = bar
                    .iter()
                    .find(|&&s| s != 6603 && book.contains(&s))
                    .copied()
                    .or_else(|| book.contains(&6673).then_some(6673));
                if let Some(spell) = spell {
                    cx.session.cast_spell(spell, None)?;
                    println!("sent CMSG_CAST_SPELL for spell {spell} (self)");
                    self.cast_sent = Some(spell);
                } else {
                    bail!("no castable spell to probe with (bar has no known active spell)");
                }
            }
        }
        // Phase 3 (decision 0792): the GROUND cast — mask 0x40 + three f32 WoW coords, the body
        // the targeting cursor's world-click commit sends. GM-learn the classless DEST_SPELL,
        // wait for the server's own `SMSG_LEARNED_SPELL` ack (the chat command executes deferred
        // — a cast in the same batch is dropped as unknown), then cast at our own feet
        // (`self_pose` — distance 0, always in range, always LOS). Routed to `dest_verdict` by
        // spell id, so it can't collide with phase 2's positional slot. Gated like phase 2 on
        // the phase-1 verdict (the book/bar round trip is proven).
        if self.dest_cast_sent.is_none() && cx.world.cast_verdict.is_some() {
            // A prior run's deferred .learn may already have stuck — the login book then
            // carries the spell and no fresh SMSG_LEARNED_SPELL will ever fire.
            if !self.dest_spell_known
                && cx
                    .world
                    .spell_book
                    .as_ref()
                    .is_some_and(|b| b.contains(&DEST_SPELL))
            {
                self.dest_spell_known = true;
            }
            if !self.dest_spell_known && !self.dest_learn_sent {
                cx.session.send_chat(&format!(".learn {DEST_SPELL}"))?;
                println!("sent .learn {DEST_SPELL} (Rough Dynamite — the dest-cast phase's spell)");
                self.dest_learn_sent = true;
            }
            if self.dest_spell_known {
                let (pos, _) = cx.world.self_pose();
                cx.world.dest_spell = Some(DEST_SPELL);
                cx.session.cast_spell_at_dest(DEST_SPELL, pos)?;
                println!(
                    "sent CMSG_CAST_SPELL for spell {DEST_SPELL} at dest ({:.2}, {:.2}, {:.2}) — mask 0x40",
                    pos[0], pos[1], pos[2]
                );
                self.dest_cast_sent = Some(pos);
            }
        }
        // Phase 2: once the self-cast AND the ground cast are answered, cast the bar spell AT
        // the first streamed creature — the mask-2 + PACKED-guid target block. Ordered AFTER the
        // dest phase deliberately: `.learn` executes deferred and resolves the SELECTION, so a
        // creature selected here before the command ran turned the learn into "Player not
        // found!" (observed live, 2026-07-30 — the `.cheat god` re-target trap, method.md).
        if self.targeted_cast_sent.is_none() && cx.world.dest_verdict.is_some() {
            if let Some(spell) = self.cast_sent {
                if let Some((&guid, _)) = cx
                    .world
                    .tracked
                    .iter()
                    .find(|(g, t)| t.kind == EntityKind::Unit && guid::is_creature_or_pet(**g))
                {
                    cx.session.set_selection(guid)?;
                    cx.session.cast_spell(spell, Some(guid))?;
                    println!("sent CMSG_CAST_SPELL for spell {spell} at {guid:#x} (packed target)");
                    self.targeted_cast_sent = Some((spell, guid));
                }
            }
        }
        Ok(())
    }

    fn on_event(&mut self, ev: &SessionEvent, _cx: &mut Ctx) -> Result<()> {
        // The dest phase's learn ack: the book already carrying the spell (a prior probe run's
        // leftover) counts the same — `SMSG_INITIAL_SPELLS` is handled at the gate below instead,
        // since the book arrives before the learn is ever sent.
        if let SessionEvent::SpellLearned { spell_id } = ev {
            if *spell_id == DEST_SPELL {
                self.dest_spell_known = true;
            }
        }
        Ok(())
    }

    fn verify(&mut self, cx: &mut Ctx) -> Result<()> {
        let world = &mut *cx.world;
        let session = &mut *cx.session;

        // --spells inventory readout: what the server says One is actually holding.
        if let Some(sf) = &world.self_fields {
            let slot_entry = |guid: Option<u64>| {
                guid.filter(|g| *g != 0)
                    .and_then(|g| world.item_entries.get(&g).copied())
            };
            let mut wanted: Vec<u32> = (0..23)
                .filter_map(|i| slot_entry(sf.player_inv_slot(i)))
                .chain((0..16).filter_map(|i| slot_entry(sf.player_pack_slot(i))))
                .collect();
            wanted.sort_unstable();
            wanted.dedup();
            for e in &wanted {
                if !world.item_names.contains_key(e) {
                    session.item_query(*e, 0)?;
                }
            }
            let drain_until = Instant::now() + Duration::from_secs(3);
            while Instant::now() < drain_until
                && wanted.iter().any(|e| !world.item_names.contains_key(e))
            {
                if let Ok(msg) = session.recv() {
                    for ev in decode(msg) {
                        if let SessionEvent::ItemTemplate {
                            entry,
                            info: Some(i),
                        } = ev
                        {
                            world.item_names.insert(
                                entry,
                                format!("{} [class {} subclass {}]", i.name, i.class, i.subclass),
                            );
                        }
                    }
                }
            }
            let show_equipped = |label: &str, slot: u8| {
                let text = match (sf.player_inv_slot(slot), sf.player_visible_item_entry(slot)) {
                    (_, Some(e)) => world
                        .item_names
                        .get(&e)
                        .cloned()
                        .unwrap_or_else(|| format!("entry {e}")),
                    (Some(g), None) if g != 0 => format!("guid {g:#x} (no visible entry)"),
                    _ => "EMPTY".to_string(),
                };
                println!("  {label:<10} {text}");
            };
            let show = |label: &str, guid: Option<u64>| {
                let text = match guid {
                    None => "(not sent)".to_string(),
                    Some(0) => "EMPTY".to_string(),
                    Some(g) => match world.item_entries.get(&g) {
                        Some(e) => world
                            .item_names
                            .get(e)
                            .cloned()
                            .unwrap_or_else(|| format!("entry {e}")),
                        None => format!("guid {g:#x} (no item object streamed)"),
                    },
                };
                println!("  {label:<10} {text}");
            };
            // Equipment names resolve through the PUBLIC visible-item entries (item objects
            // have no decode path yet — a named gap; the private INV guids prove presence).
            for slot in [15u8, 16, 17] {
                if let Some(e) = sf.player_visible_item_entry(slot) {
                    if !world.item_names.contains_key(&e) {
                        session.item_query(e, 0)?;
                    }
                }
            }
            println!(
                "
--- One's hands + pack (server truth) ---"
            );
            show_equipped("mainhand", 15);
            show_equipped("offhand", 16);
            show_equipped("ranged", 17);
            for i in 0..16 {
                if let Some(g) = sf.player_pack_slot(i).filter(|g| *g != 0) {
                    show(&format!("pack {i}"), Some(g));
                }
            }
        } else {
            println!("(no self descriptor captured — inventory readout skipped)");
        }

        // --spells verdict: book + bar must have arrived and parsed; the cast must have been answered.
        let book = world
            .spell_book
            .clone()
            .context("no SMSG_INITIAL_SPELLS arrived")?;
        if book.is_empty() {
            bail!("SMSG_INITIAL_SPELLS parsed to an empty spell book");
        }
        let bar = world
            .bar_spells
            .clone()
            .context("no SMSG_ACTION_BUTTONS arrived")?
            .len();
        let sent = self.cast_sent.context("cast never sent (empty book?)")?;
        let (spell, success, reason) = world
            .cast_verdict
            .context("no SMSG_CAST_RESULT for our CMSG_CAST_SPELL")?;
        if spell != sent {
            bail!("cast result names spell {spell}, we cast {sent}");
        }
        match (world.item_asked, &world.item_answer) {
            (Some(e), Some((entry, Some(name)))) if *entry == e => {
                println!("✅ item query: entry {e} → '{name}'.");
            }
            (Some(e), Some((entry, None))) if *entry == e => {
                println!("✅ item query: entry {e} → unknown (miss shape parsed).");
            }
            (Some(e), _) => bail!("no SMSG_ITEM_QUERY_SINGLE_RESPONSE for entry {e}"),
            (None, _) => println!("(no item-kind button on the bar to item-query)"),
        }
        // --spells dest-cast verdict (decision 0792): the wire shape must not merely parse — the
        // cast must be ACCEPTED. A missing verdict means the body desynced the server's reader
        // (the CMSG was dropped or the session died); a refusal names the CheckCast reason.
        match (self.dest_cast_sent, &world.dest_verdict) {
            (Some(pos), Some((_, true, _))) => {
                println!(
                    "✅ ground cast (mask 0x40 + dest): spell {DEST_SPELL} at ({:.2}, {:.2}, {:.2}) → ok.",
                    pos[0], pos[1], pos[2]
                );
            }
            (Some(_), Some((_, false, r))) => {
                bail!(
                    "ground cast {DEST_SPELL} REFUSED (reason {:#04x}) — the dest body parsed but CheckCast said no",
                    r.unwrap_or(0)
                )
            }
            (Some(_), None) => {
                bail!("no SMSG_CAST_RESULT for the GROUND cast of {DEST_SPELL} — the mask-0x40 dest body desyncs the server")
            }
            (None, _) => bail!("ground cast never sent (phase 1 never resolved?)"),
        }
        match (self.targeted_cast_sent, &world.targeted_verdict) {
            (Some((spell, guid)), Some((s2, ok2, r2))) if *s2 == spell => {
                println!(
                    "✅ targeted cast (packed guid): spell {spell} at {guid:#x} → {}.",
                    if *ok2 {
                        "ok".to_string()
                    } else {
                        format!("failed (reason {:#04x})", r2.unwrap_or(0))
                    }
                );
            }
            (Some((spell, _)), _) => {
                bail!("no SMSG_CAST_RESULT for the TARGETED cast of {spell} — the mask-2 packed-guid body desyncs the server")
            }
            (None, _) => println!("(no creature in range for the targeted-cast phase)"),
        }
        println!(
            "\n✅ spells: {} known, {bar} bar slot(s), cast {sent} → {}.",
            book.len(),
            if success {
                "ok".to_string()
            } else {
                format!(
                    "failed (reason {:#04x}) — round trip proven",
                    reason.unwrap_or(0)
                )
            }
        );
        Ok(())
    }
}
