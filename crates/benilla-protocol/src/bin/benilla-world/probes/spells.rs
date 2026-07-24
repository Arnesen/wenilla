//! `--spells`: the spell/action wire. Require `SMSG_INITIAL_SPELLS` + `SMSG_ACTION_BUTTONS` at
//! login, cast one spell (self, then packed-guid targeted), require a `SMSG_CAST_RESULT` verdict,
//! and read our inventory back out of the descriptor as the round-trip evidence.

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use benilla_protocol::{decode, guid, EntityKind, SessionEvent};

use crate::probes::{Ctx, Probe};

#[derive(Default)]
pub(crate) struct Spells {
    cast_sent: Option<u32>,
    targeted_cast_sent: Option<(u32, u64)>,
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
        // Phase 2: once the self-cast is answered, cast the same spell AT the first streamed
        // creature — this exercises the mask-2 + PACKED-guid target block, the path the action
        // bar uses with a selection (the self-cast only proves mask 0).
        if self.targeted_cast_sent.is_none() && cx.world.cast_verdict.is_some() {
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
