//! The shared world state every probe reads: the entity tracker, the merged self descriptor, the
//! item/vendor stores, the opcode tally — plus the session-keeping acks (teleport, force-speed, the
//! granted movement-mode family) that keep *any* session alive regardless of probe mix, and the shared
//! [`DeathArc`] scenario machinery (`--death`/`--spirit` both read it, so neither owns it).

use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

use anyhow::Result;
use benilla_protocol::{
    Character, EntityKind, MoveMode, ObjectFields, ServerPacket, SessionEvent, SpeedKind,
    WorldSession,
};

/// A Kobold Vermin spawn in Northshire (mangos `creature` guid 79992) — the `--attack` teleport
/// target: hostile, level 1, melee-reach the moment we land on it. Lives here (not in a probe)
/// because attack staging, loot staging, and the [`DeathArc`] staging all teleport to it.
pub(crate) const ATTACK_TP: &str = ".go xyz -8780.71 -164.568 81.94";

/// One entity decoded from the stream, as `benilla-world` reports it (raw WoW coords).
pub(crate) struct Tracked {
    pub(crate) kind: EntityKind,
    pub(crate) position: [f32; 3],
    pub(crate) orientation: f32,
}

/// The shared die→release→ghost→corpse scenario machinery (decision 0308 slice 1). Created when
/// `--death` or `--spirit` is set; both probes read it, so neither owns it. The staging is in
/// [`World::stage`]; the die/release pumps in [`World::poll`]; the ghost/graveyard/reclaim evidence
/// in [`World::on_event`].
#[derive(Default)]
pub(crate) struct DeathArc {
    pub(crate) die_sent: bool,
    pub(crate) died_at: Option<Instant>,
    pub(crate) death_pos: Option<[f32; 3]>,
    pub(crate) rooted_seen: bool,
    pub(crate) repop_sent: bool,
    pub(crate) unroot_seen: bool,
    pub(crate) water_walk_seen: bool,
    pub(crate) ghost_seen: bool,
    pub(crate) graveyard_pos: Option<[f32; 3]>,
    pub(crate) reclaim_delay_ms: Option<u32>,
    /// Set when the death probe sends `.revive` OR the spirit probe sends the healer activate — the
    /// ghost-clear check keys off this instead of either probe's own flag, so the two are decoupled.
    pub(crate) revive_initiated: bool,
    pub(crate) revived_seen: bool,
}

/// Everything more than one probe reads, plus the session-keeping acks and the [`DeathArc`].
pub(crate) struct World {
    pub(crate) self_guid: u64,
    pub(crate) self_name: String,
    pub(crate) self_level: u8,
    pub(crate) self_map: u32,
    /// The `SMSG_CHAR_ENUM` spawn pose, the [`Self::self_pose`] fallback before our own create streams.
    pub(crate) spawn_pos: [f32; 3],
    pub(crate) tracked: HashMap<u64, Tracked>,
    /// Our own descriptor fields (equipment/pack slots are PRIVATE-flagged — sent only for our own
    /// player) merged across the create + every values delta.
    pub(crate) self_fields: Option<ObjectFields>,
    pub(crate) item_entries: HashMap<u64, u32>,
    pub(crate) item_stacks: HashMap<u64, u32>,
    pub(crate) item_names: HashMap<u32, String>,
    /// Streamed NPCs that advertise UNIT_NPC_FLAG_VENDOR, by guid → position.
    pub(crate) vendors: HashMap<u64, [f32; 3]>,
    /// The last same-map teleport landing point (the `--attack`/`--charge`/`--loot`/`--death` spot).
    pub(crate) attack_pos: Option<[f32; 3]>,
    /// Every self force-speed change seen (kind, counter, flat speed).
    pub(crate) speed_changes_seen: Vec<(SpeedKind, u32, f32)>,
    pub(crate) player_name_answer: Option<String>,
    pub(crate) creature_name_answer: Option<(u32, Option<String>)>,
    pub(crate) spell_book: Option<Vec<u32>>,
    pub(crate) bar_spells: Option<Vec<u32>>,
    pub(crate) item_asked: Option<u32>,
    pub(crate) item_answer: Option<(u32, Option<String>)>,
    pub(crate) cast_verdict: Option<(u32, bool, Option<u8>)>,
    pub(crate) targeted_verdict: Option<(u32, bool, Option<u8>)>,
    /// The `--spells` dest-cast phase's spell (decision 0792): a `SMSG_CAST_RESULT` naming it
    /// routes to [`Self::dest_verdict`] instead of the positional pair above — the dest phase
    /// runs concurrently with the targeted phase, so position alone can't route it.
    pub(crate) dest_spell: Option<u32>,
    pub(crate) dest_verdict: Option<(u32, bool, Option<u8>)>,
    pub(crate) swings_seen: u32,
    pub(crate) self_moves: u32,
    pub(crate) tally: BTreeMap<String, u32>,
    pub(crate) total: u32,
    pub(crate) death_arc: Option<DeathArc>,
    /// Shared staging flags: the ATTACK_TP (attack/loot/DeathArc all want it) and the McBride
    /// teleport (questlog/giverstatus both want it) each go out once, set by whichever stages first.
    pub(crate) attack_tp_staged: bool,
    pub(crate) mcbride_staged: bool,
}

impl World {
    pub(crate) fn new(character: &Character) -> Self {
        World {
            self_guid: character.guid,
            self_name: character.name.clone(),
            self_level: character.level,
            self_map: character.map,
            spawn_pos: [
                character.position.x,
                character.position.y,
                character.position.z,
            ],
            tracked: HashMap::new(),
            self_fields: None,
            item_entries: HashMap::new(),
            item_stacks: HashMap::new(),
            item_names: HashMap::new(),
            vendors: HashMap::new(),
            attack_pos: None,
            speed_changes_seen: Vec::new(),
            player_name_answer: None,
            creature_name_answer: None,
            spell_book: None,
            bar_spells: None,
            item_asked: None,
            item_answer: None,
            cast_verdict: None,
            targeted_verdict: None,
            dest_spell: None,
            dest_verdict: None,
            swings_seen: 0,
            self_moves: 0,
            tally: BTreeMap::new(),
            total: 0,
            death_arc: None,
            attack_tp_staged: false,
            mcbride_staged: false,
        }
    }

    /// Our tracked pose, for the movement acks the server expects while dead/ghost (root,
    /// water-walk): the probe holds still throughout the arc, so any recent `tracked` position is
    /// truthful — fall back to the SMSG_CHAR_ENUM spawn pose for the sliver of time before our own
    /// ObjectCreate has streamed.
    pub(crate) fn self_pose(&self) -> ([f32; 3], f32) {
        match self.tracked.get(&self.self_guid) {
            Some(t) => (t.position, t.orientation),
            None => (self.spawn_pos, 0.0),
        }
    }

    /// Count a received packet: one per `recv`, before decode (the opcode tally + total).
    pub(crate) fn tally_packet(&mut self, msg: &ServerPacket) {
        self.total += 1;
        *self.tally.entry(msg.name()).or_default() += 1;
    }

    /// One-time pre-stream staging for the [`DeathArc`]. Called BEFORE the probes' stage loop, so
    /// for `--spirit` the order is preserved: the arc's `.revive` + teleport, then spirit's
    /// `.repairitems`.
    pub(crate) fn stage(&mut self, session: &mut WorldSession) -> Result<()> {
        if self.death_arc.is_some() {
            // Stage AWAY from any graveyard first (the kobold spot the other probes use): dying AT a
            // graveyard makes the release teleport a 0-yd no-op and the >20-yd graveyard assertion
            // reads it as a missing port (live-observed: a cleanup `.revive` leaves the character at
            // the graveyard, and the next `.die` there re-pops in place). The `.die` itself goes out
            // in-loop, once this staging teleport has landed. The `.revive` first is defensive: a
            // prior aborted run can leave the character a ghost (live-observed), and the arc under
            // test must start from alive (it's a no-op on a living character).
            session.send_chat(".revive")?;
            // The ATTACK_TP is the shared teleport — mark it staged so attack/loot don't re-send it.
            session.send_chat(ATTACK_TP)?;
            self.attack_tp_staged = true;
            println!("sent GM: .revive (defensive); teleport (death staging): {ATTACK_TP}");
        }
        Ok(())
    }

    /// Every pump iteration, before recv: the [`DeathArc`] die/release steps.
    pub(crate) fn poll(&mut self, session: &mut WorldSession) -> Result<()> {
        if let Some(arc) = &mut self.death_arc {
            // --death/--spirit: kill ourselves once the staging teleport has landed (`.die` with
            // nothing selected targets the caster).
            if !arc.die_sent && self.attack_pos.is_some() {
                session.send_chat(".die")?;
                println!("sent GM: .die (self-kill)");
                arc.die_sent = true;
            }
            // --death: once we've hit 0 health AND the server has force-rooted us (the wire doesn't
            // guarantee which arrives first — see the MoveRoot/ObjectValues arms below), release the
            // spirit: the RELEASE SPIRIT button's `CMSG_REPOP_REQUEST` (decision 0308 §1).
            if !arc.repop_sent && arc.died_at.is_some() && arc.rooted_seen {
                session.repop_request()?;
                println!("sent CMSG_REPOP_REQUEST (release spirit)");
                arc.repop_sent = true;
            }
        }
        Ok(())
    }

    /// Every decoded event, before the probes' `on_event`: the ungated narration/evidence + the
    /// unconditional session-keeping acks + the [`DeathArc`] evidence.
    pub(crate) fn on_event(&mut self, ev: &SessionEvent, session: &mut WorldSession) -> Result<()> {
        match ev {
            SessionEvent::ObjectCreate {
                guid,
                kind,
                position,
                orientation,
                fields,
                ..
            } => {
                if *guid == self.self_guid {
                    self.self_fields = Some(fields.clone());
                }
                // A vendor advertises UNIT_NPC_FLAG_VENDOR (0x4 — NOT 0x80, which is
                // INNKEEPER in 1.12; VERIFIED vmangos `UnitDefines.h:659`) in NPC_FLAGS.
                if *kind == EntityKind::Unit && fields.unit_npc_flags() & 0x4 != 0 {
                    self.vendors.insert(*guid, *position);
                }
                self.tracked.insert(
                    *guid,
                    Tracked {
                        kind: *kind,
                        position: *position,
                        orientation: *orientation,
                    },
                );
            }
            // Our inventory: position-less item/container creates.
            SessionEvent::ItemCreate { guid, fields, .. } => {
                if let Some(entry) = fields.object_entry() {
                    self.item_entries.insert(*guid, entry);
                }
                self.item_stacks
                    .insert(*guid, fields.item_stack_count().unwrap_or(1));
            }
            SessionEvent::ObjectValues { guid, fields } if *guid == self.self_guid => {
                // --death: the server force-flushes UNIT_FIELD_HEALTH the instant we
                // die — a values delta carrying an EXPLICIT 0 (not merely absent) is
                // the death instant itself. Must be read off `fields` (this delta)
                // before the merge below moves it.
                let pose = self.self_pose();
                if let Some(arc) = &mut self.death_arc {
                    if arc.died_at.is_none() && fields.unit_health() == Some(0) {
                        arc.died_at = Some(Instant::now());
                        let pos = pose.0;
                        arc.death_pos = Some(pos);
                        println!(
                            "UNIT_FIELD_HEALTH → 0: died at ({:.1}, {:.1}, {:.1})",
                            pos[0], pos[1], pos[2]
                        );
                    }
                }
                if let Some(sf) = &mut self.self_fields {
                    sf.merge(fields.clone());
                    // PLAYER_FLAGS_GHOST (bit 0x10, field 190) sets at release and
                    // clears at revive — read off the MERGED store, not this delta
                    // alone: a delta that doesn't touch PLAYER_FLAGS reads as an
                    // absent field on `fields`, not "still whatever it last was".
                    if let Some(arc) = &mut self.death_arc {
                        if !arc.ghost_seen && sf.player_is_ghost() {
                            arc.ghost_seen = true;
                            println!("PLAYER_FLAGS_GHOST set — released as a ghost");
                        } else if arc.ghost_seen
                            && arc.revive_initiated
                            && !arc.revived_seen
                            && !sf.player_is_ghost()
                        {
                            arc.revived_seen = true;
                            println!("PLAYER_FLAGS_GHOST cleared — revived");
                        }
                    }
                }
            }
            SessionEvent::ObjectMove {
                guid,
                position,
                orientation,
            } => {
                if let Some(t) = self.tracked.get_mut(guid) {
                    t.position = *position;
                    t.orientation = *orientation;
                }
            }
            SessionEvent::ObjectsRemoved(guids) => {
                for g in guids {
                    self.tracked.remove(g);
                }
            }
            SessionEvent::ObjectDestroyed(guid) => {
                self.tracked.remove(guid);
            }
            SessionEvent::TimeSpeed {
                hours,
                minutes,
                timescale,
                ..
            } => {
                println!(
                    "server game-time {hours:02}:{minutes:02}  (timescale {timescale} game-min/sec)"
                );
            }
            SessionEvent::PlayerName { guid, name, .. } => {
                println!("SMSG_NAME_QUERY_RESPONSE: guid {guid} → '{name}'");
                self.player_name_answer = Some(name.clone());
            }
            SessionEvent::CreatureName { entry, name, .. } => {
                println!("SMSG_CREATURE_QUERY_RESPONSE: entry {entry} → {name:?}");
                self.creature_name_answer = Some((*entry, name.clone()));
            }
            SessionEvent::SpellBook { spell_ids, .. } => {
                println!(
                    "SMSG_INITIAL_SPELLS: {} spells (first: {:?})",
                    spell_ids.len(),
                    &spell_ids[..spell_ids.len().min(8)]
                );
                self.spell_book = Some(spell_ids.clone());
            }
            SessionEvent::ActionButtons { buttons } => {
                println!("SMSG_ACTION_BUTTONS: {} occupied slot(s)", buttons.len());
                for b in buttons.iter().take(12) {
                    println!(
                        "  slot {:>3}  action {:>5}  kind {:#04x}",
                        b.slot, b.action, b.kind
                    );
                }
                // Item-query the first item-kind button (T2 wire groundwork), once.
                if self.item_asked.is_none() {
                    if let Some(b) = buttons.iter().find(|b| b.kind == 0x80) {
                        session.item_query(b.action, 0)?;
                        println!("sent CMSG_ITEM_QUERY_SINGLE for entry {}", b.action);
                        self.item_asked = Some(b.action);
                    }
                }
                self.bar_spells = Some(
                    buttons
                        .iter()
                        .filter(|b| b.kind == 0)
                        .map(|b| b.action)
                        .collect(),
                );
            }
            SessionEvent::Chat(m) => {
                println!(
                    "chat [{:#04x}] {}{}",
                    m.chat_type,
                    m.sender_name
                        .as_deref()
                        .map(|n| format!("{n}: "))
                        .unwrap_or_default(),
                    m.text
                );
            }
            SessionEvent::ItemTemplate { entry, info } => {
                println!(
                    "SMSG_ITEM_QUERY_SINGLE_RESPONSE: entry {entry} → {:?}",
                    info.as_ref().map(|i| (&i.name, i.class, i.subclass))
                );
                if let Some(i) = info {
                    self.item_names.insert(
                        *entry,
                        format!("{} [class {} subclass {}]", i.name, i.class, i.subclass),
                    );
                }
                self.item_answer = Some((*entry, info.as_ref().map(|i| i.name.clone())));
            }
            SessionEvent::ForceSpeedChange {
                guid,
                kind,
                counter,
                speed,
            } if *guid == self.self_guid => {
                // Ack with our live (streamed) pose — a zeroed fallback would ask the
                // server to relocate us to (0,0,0), so no known pose means no ack (the
                // post-stream ensure then fails loudly instead).
                let Some(t) = self.tracked.get(guid) else {
                    println!(
                        "force {kind:?} speed change before our own create streamed — cannot ack"
                    );
                    return Ok(());
                };
                session.force_speed_ack(
                    *kind,
                    *guid,
                    *counter,
                    *speed,
                    t.position,
                    t.orientation,
                )?;
                println!(
                    "SMSG_FORCE_{kind:?}_SPEED_CHANGE: counter {counter}, {speed} yd/s — acked"
                );
                self.speed_changes_seen.push((*kind, *counter, *speed));
            }
            SessionEvent::Teleport {
                guid,
                counter,
                position,
                ..
            } if *guid == self.self_guid => {
                // Ack the same-map port (the server freezes our movement otherwise).
                session.teleport_ack(*guid, *counter)?;
                self.attack_pos = Some(*position);
                // Move our tracked self to the landing point so range-based picks (the
                // --vendor nearest-vendor search) measure from where we actually are.
                if let Some(t) = self.tracked.get_mut(guid) {
                    t.position = *position;
                }
                println!(
                    "teleported to ({:.1}, {:.1}, {:.1}) — ack sent",
                    position[0], position[1], position[2]
                );
                // --death: the graveyard port after release rides this same event —
                // record it without duplicating the ack above. The spirit probe's
                // healer_tp_landed capture (the old else-if branch) now lives in its own
                // on_event: `healer_tp_sent` can only become true after `graveyard_pos` is
                // Some (the healer-TP poll requires it), so the two are temporally disjoint.
                if let Some(arc) = &mut self.death_arc {
                    if arc.repop_sent && arc.graveyard_pos.is_none() {
                        arc.graveyard_pos = Some(*position);
                        println!(
                            "graveyard teleport: ({:.1}, {:.1}, {:.1})",
                            position[0], position[1], position[2]
                        );
                    }
                }
            }
            SessionEvent::AttackStart { attacker, victim } => {
                println!("SMSG_ATTACKSTART: {attacker:#x} → {victim:#x}");
            }
            SessionEvent::AttackStop { attacker, victim } => {
                println!("SMSG_ATTACKSTOP: {attacker:#x} → {victim:#x}");
            }
            SessionEvent::AttackerState(s) => {
                self.swings_seen += 1;
                println!(
                    "SMSG_ATTACKERSTATEUPDATE: {:#x} → {:#x}  hitInfo {:#x}  damage {}  victimState {}",
                    s.attacker, s.victim, s.hit_info, s.damage, s.victim_state
                );
            }
            SessionEvent::CastResult {
                spell_id,
                success,
                reason,
            } => {
                println!("SMSG_CAST_RESULT: spell {spell_id} success={success} reason={reason:?}");
                if self.dest_spell == Some(*spell_id) {
                    self.dest_verdict = Some((*spell_id, *success, *reason));
                } else if self.cast_verdict.is_none() {
                    self.cast_verdict = Some((*spell_id, *success, *reason));
                } else {
                    self.targeted_verdict = Some((*spell_id, *success, *reason));
                }
            }
            SessionEvent::MonsterMove {
                guid,
                start,
                spline_id,
                path,
                facing,
                stop,
                duration_ms,
                flying,
            } => {
                let mine = *guid == self.self_guid;
                if mine {
                    self.self_moves += 1;
                }
                let speed = {
                    let len: f32 = path
                        .windows(2)
                        .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
                        .sum();
                    len / ((*duration_ms).max(1) as f32 / 1000.0)
                };
                println!(
                    "SMSG_MONSTER_MOVE {}{guid:#x}: splineId {spline_id}, {} pts, facing {facing:?}, {duration_ms}ms, flying={flying}, stop={stop}, ~{speed:.1} yd/s",
                    if mine { "[SELF] " } else { "" },
                    path.len(),
                );
                if mine {
                    println!(
                        "    start ({:.2}, {:.2}, {:.2})",
                        start[0], start[1], start[2]
                    );
                    for (i, p) in path.iter().enumerate() {
                        println!("    pt[{i}] ({:.2}, {:.2}, {:.2})", p[0], p[1], p[2]);
                    }
                }
            }
            // **A granted mover mode** (root / water-walk / feather-fall / hover — decision 0866)
            // must be acked with the echoed counter + our current pose, or the server never applies
            // the change and observers never see it. Unconditional session-keeping (like
            // teleport/force-speed): a mode granted outside the death arc would otherwise go
            // un-acked, and the real client always acks.
            SessionEvent::MoveMode {
                guid,
                counter,
                mode,
                apply,
            } if *guid == self.self_guid => {
                let pose = self.self_pose();
                // The ack's MovementInfo must carry the applied mode's own bit: for root the server
                // KICKS one without it (vmangos `HandleMoveRootAck:715-723`; live-verified — the
                // flags-0 ack drew "movement info does not have rooted movement flag" in
                // Movement.log and the root never confirmed, so release sent no unroot), and for the
                // other three the word IS the mover's new flags. This probe holds no mover state of
                // its own, so the applied bit alone is the honest word.
                let flags = if *apply { mode.flag() } else { 0 };
                session.move_mode_ack(*guid, *counter, *mode, *apply, flags, pose)?;
                if let Some(arc) = &mut self.death_arc {
                    match (mode, apply) {
                        (MoveMode::Root, true) => arc.rooted_seen = true,
                        (MoveMode::Root, false) => arc.unroot_seen = true,
                        (MoveMode::WaterWalk, true) => arc.water_walk_seen = true,
                        _ => {}
                    }
                }
                println!(
                    "mover mode {mode:?} {} — acked",
                    if *apply { "granted" } else { "revoked" }
                );
            }
            SessionEvent::CorpseReclaimDelay { delay_ms } => {
                if let Some(arc) = &mut self.death_arc {
                    arc.reclaim_delay_ms = Some(*delay_ms);
                    println!("SMSG_CORPSE_RECLAIM_DELAY: {delay_ms} ms");
                }
            }
            _ => {}
        }
        Ok(())
    }
}
