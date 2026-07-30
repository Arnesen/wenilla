//! `--groundfx <spell_id>`: capture the **dest-anchored effect wire** a ground cast produces —
//! the instrument for the B132 follow-up ("the cast lands but nothing shows"). GM-learn the
//! spell, GM-fill mana (any class can then channel it), cast it at our own feet through the same
//! `cast_spell_at_dest` body the client's world-click commit sends, and dump raw everything the
//! server anchors at the point: every `DynamicObject` create (all `DYNAMICOBJECT_*` fields,
//! labeled per vmangos `UpdateFields_1_12_1.h`), the `SPELL_GO`, and the removal edge with its
//! measured lifetime. Run with `--seconds 25`+ so a channeled spell's whole life fits the window
//! (Blizzard 10 channels 8 s and its object outlives the channel).

use std::time::Instant;

use anyhow::{bail, Result};
use benilla_protocol::{EntityKind, SessionEvent};

use crate::probes::{Ctx, Probe};

/// `DYNAMICOBJECT_*` UpdateField labels (vmangos `UpdateFields_1_12_1.h:325-334`; OBJECT_END=6).
fn dyn_field_label(index: u16) -> &'static str {
    match index {
        0 => "OBJECT_FIELD_GUID_LO",
        1 => "OBJECT_FIELD_GUID_HI",
        2 => "OBJECT_FIELD_TYPE",
        3 => "OBJECT_FIELD_ENTRY",
        4 => "OBJECT_FIELD_SCALE_X",
        6 => "DYNAMICOBJECT_CASTER_LO",
        7 => "DYNAMICOBJECT_CASTER_HI",
        8 => "DYNAMICOBJECT_BYTES",
        9 => "DYNAMICOBJECT_SPELLID",
        10 => "DYNAMICOBJECT_RADIUS",
        11 => "DYNAMICOBJECT_POS_X",
        12 => "DYNAMICOBJECT_POS_Y",
        13 => "DYNAMICOBJECT_POS_Z",
        14 => "DYNAMICOBJECT_FACING",
        _ => "?",
    }
}

/// Indices whose wire u32 is an f32 bit pattern.
fn dyn_field_is_f32(index: u16) -> bool {
    matches!(index, 4 | 10..=14)
}

/// `UNIT_FIELD_POWER1` (mana) for 1.12.1 — `OBJECT_END(6) + 0x11` (vmangos
/// `UpdateFields_1_12_1.h:50`). The mana-ack gate below watches this on the self guid.
const FIELD_UNIT_POWER1: u16 = 6 + 0x11;

pub(crate) struct GroundFx {
    spell: u32,
    staged: bool,
    known: bool,
    /// Set when the self unit's `UNIT_FIELD_POWER1` echoes the GM fill — BOTH chat commands
    /// (`.learn`, `.modify mana`) execute deferred, and each needs its own server-side evidence
    /// before the cast: a cast in the learn's batch is dropped as unknown (decision 0792's trap),
    /// and one in the mana fill's batch refuses with no-power (observed live, reason 77).
    mana_seen: bool,
    cast_sent: Option<Instant>,
    /// DynamicObject creates seen: (guid, seen-at, spell id from field 9).
    creates: Vec<(u64, Instant, u32)>,
    removals: usize,
    spell_go_seen: bool,
}

impl GroundFx {
    pub(crate) fn new(spell: u32) -> Self {
        Self {
            spell,
            staged: false,
            known: false,
            mana_seen: false,
            cast_sent: None,
            creates: Vec::new(),
            removals: 0,
            spell_go_seen: false,
        }
    }
}

impl Probe for GroundFx {
    fn poll(&mut self, cx: &mut Ctx) -> Result<()> {
        // Stage once the book is in: GM-learn the spell (skipped if already known — a prior run's
        // leftover) and fill a mana pool any class can spend (the probe char may be a warrior).
        // Both chat commands execute DEFERRED server-side, so the cast waits for the
        // SMSG_LEARNED_SPELL ack (the same trap the --spells dest phase hit, decision 0792).
        if !self.staged {
            let Some(book) = &cx.world.spell_book else {
                return Ok(());
            };
            if book.contains(&self.spell) {
                self.known = true;
                println!("groundfx: spell {} already known", self.spell);
            } else {
                cx.session.send_chat(&format!(".learn {}", self.spell))?;
                println!("groundfx: sent .learn {}", self.spell);
            }
            cx.session.send_chat(".modify mana 12000 12000")?;
            self.staged = true;
        }
        if self.known && self.mana_seen && self.cast_sent.is_none() {
            let pos = cx.world.self_pose().0;
            cx.world.dest_spell = Some(self.spell);
            cx.session.cast_spell_at_dest(self.spell, pos)?;
            println!(
                "groundfx: sent CMSG_CAST_SPELL {} at own feet ({:.2}, {:.2}, {:.2}) — mask 0x40",
                self.spell, pos[0], pos[1], pos[2]
            );
            self.cast_sent = Some(Instant::now());
        }
        Ok(())
    }

    fn on_event(&mut self, ev: &SessionEvent, cx: &mut Ctx) -> Result<()> {
        match ev {
            SessionEvent::SpellLearned { spell_id } if *spell_id == self.spell => {
                self.known = true;
            }
            // The mana-ack gate: the self unit's POWER1 showing a real pool — either the login
            // create (a mage arrives full) or the `.modify mana` echo (the warrior's fill).
            SessionEvent::ObjectCreate { guid, fields, .. }
            | SessionEvent::ObjectValues { guid, fields }
                if *guid == cx.world.self_guid && !self.mana_seen =>
            {
                if fields
                    .raw_fields()
                    .any(|(i, v)| i == FIELD_UNIT_POWER1 && v >= 500)
                {
                    self.mana_seen = true;
                    println!("groundfx: self mana visible — the cast gate is open");
                }
            }
            // vmangos sends UPDATEFLAG_HAS_POSITION on every dynobj create, so the position is
            // always live here. Field 9 (SPELLID) is dumped with the rest raw.
            SessionEvent::ObjectCreate {
                guid,
                kind: EntityKind::DynamicObject,
                position,
                orientation,
                fields,
                ..
            } => {
                let mut raw: Vec<(u16, u32)> = fields.raw_fields().collect();
                raw.sort_unstable_by_key(|(i, _)| *i);
                let spell_id = raw
                    .iter()
                    .find(|(i, _)| *i == 9)
                    .map(|(_, v)| *v)
                    .unwrap_or(0);
                println!(
                    "groundfx: DYNAMICOBJECT CREATE guid={guid:#018x} pos=({:.2}, {:.2}, {:.2}) o={orientation:.3}{}",
                    position[0],
                    position[1],
                    position[2],
                    if spell_id != 0 {
                        format!(" — spell {spell_id}")
                    } else {
                        String::new()
                    }
                );
                for (index, value) in &raw {
                    if dyn_field_is_f32(*index) {
                        println!(
                            "    [{index:>3}] {:<28} = {:#010x}  (f32 {})",
                            dyn_field_label(*index),
                            value,
                            f32::from_bits(*value)
                        );
                    } else {
                        println!(
                            "    [{index:>3}] {:<28} = {:#010x}  ({})",
                            dyn_field_label(*index),
                            value,
                            value
                        );
                    }
                }
                if spell_id != 0 {
                    self.creates.push((*guid, Instant::now(), spell_id));
                }
            }
            SessionEvent::SpellGo {
                caster,
                spell_id,
                hits,
                misses,
                target,
                ..
            } if *spell_id == self.spell => {
                self.spell_go_seen = true;
                println!(
                    "groundfx: SPELL_GO spell {spell_id} caster={caster:#x} hits={} misses={} target={target:?} \
                     (NOTE: the wire's dest Vector3d is dropped by the event layer today)",
                    hits.len(),
                    misses.len()
                );
            }
            SessionEvent::ObjectsRemoved(guids) => {
                for g in guids {
                    if let Some((_, at, spell)) = self.creates.iter().find(|(cg, _, _)| cg == g) {
                        println!(
                            "groundfx: DYNAMICOBJECT {g:#018x} (spell {spell}) OUT-OF-RANGE — lived {:.2} s",
                            at.elapsed().as_secs_f32()
                        );
                        self.removals += 1;
                    }
                }
            }
            SessionEvent::ObjectDestroyed(g) => {
                if let Some((_, at, spell)) = self.creates.iter().find(|(cg, _, _)| cg == g) {
                    println!(
                        "groundfx: DYNAMICOBJECT {g:#018x} (spell {spell}) DESTROYED — lived {:.2} s",
                        at.elapsed().as_secs_f32()
                    );
                    self.removals += 1;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn verify(&mut self, cx: &mut Ctx) -> Result<()> {
        if self.cast_sent.is_none() {
            bail!(
                "groundfx: the cast was never sent (spell {} never became known — check the \
                 .learn ack)",
                self.spell
            );
        }
        match cx.world.dest_verdict {
            None => bail!(
                "groundfx: no SMSG_CAST_RESULT verdict for spell {}",
                self.spell
            ),
            Some((_, false, reason)) => bail!(
                "groundfx: dest cast of {} REFUSED by the server (reason {reason:?})",
                self.spell
            ),
            Some((_, true, _)) => {}
        }
        println!(
            "groundfx: spell {} — {} DynamicObject create(s), {} removal(s), SPELL_GO {}",
            self.spell,
            self.creates.len(),
            self.removals,
            if self.spell_go_seen {
                "seen"
            } else {
                "NOT seen"
            }
        );
        if self.creates.is_empty() {
            println!(
                "groundfx: NO DynamicObject arrived — either this spell anchors nothing (no \
                 persistent-area effect) or the window closed too early"
            );
        }
        Ok(())
    }
}
