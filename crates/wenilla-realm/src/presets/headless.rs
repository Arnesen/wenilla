//! Builds one preset character by playing it: a native `benilla-protocol` session logs in to the
//! fresh account, creates the character, enters the world and — the account holding GM for the
//! duration — says the dot-commands that level it, teach it and hand it its items, equips each
//! item with the same `CMSG_AUTOEQUIP_ITEM` a player's right-click sends, teleports it to the
//! entrance and logs out, which saves it. The server applies and validates every step itself,
//! so nothing is written to the character database behind its back (the service may not).
//!
//! Blocking: the caller runs it on a blocking thread.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use benilla_protocol::messages::{CharCreateReq, CHAR_CREATE_NAME_IN_USE, CHAR_CREATE_SUCCESS};
use benilla_protocol::{decode, ObjectFields, SessionEvent, WorldSession};

use super::defs::Slot;

/// Where the game servers are, from the service's own point of view. The realm list's address
/// is the public name, which a container may not be able to reach, so the world server is
/// dialled directly.
#[derive(Clone, Debug)]
pub struct Servers {
    pub realmd: String,
    pub mangosd: String,
}

/// What to build.
pub struct Job<'a> {
    pub account: &'a str,
    pub password: &'a str,
    pub slot: &'a Slot,
    pub level: u8,
    pub tele: &'a str,
    pub money: u32,
    /// Name candidates, tried in order until one is free.
    pub names: &'a [String],
}

/// What was built.
#[derive(Debug, Clone)]
pub struct Built {
    pub name: String,
    pub guid: u64,
    /// Items that did not end up equipped, with the reason.
    pub not_equipped: Vec<(u32, String)>,
}

const STEP: Duration = Duration::from_secs(5);

struct Bot {
    s: WorldSession,
    me: u64,
    fields: ObjectFields,
    /// Item guid → template entry, for everything in our inventory the server has streamed.
    items: HashMap<u64, u32>,
    /// Bag guid → its descriptor (`CONTAINER_FIELD_SLOT_*`).
    bags: HashMap<u64, ObjectFields>,
    failures: Vec<(u64, u8)>,
    moved: bool,
    /// The server's last few system lines (GM command feedback), for error messages.
    said: std::collections::VecDeque<String>,
}

/// A read that hit the socket's timeout rather than a dead connection. The protocol crate
/// carries some io errors as text, so the message is checked as well as the chain.
fn is_timeout(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>().is_some_and(|io| {
            matches!(
                io.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            )
        })
    }) || {
        let m = format!("{e:#}");
        m.contains("temporarily unavailable")
            || m.contains("timed out")
            || m.contains("would block")
    }
}

impl Bot {
    /// Read and handle packets until `pred` holds or `timeout` passes; `Ok(false)` on timeout.
    fn pump_until(&mut self, timeout: Duration, pred: impl Fn(&Bot) -> bool) -> Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            if pred(self) {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            match self.s.recv() {
                Ok(msg) => {
                    for ev in decode(msg) {
                        self.on_event(ev)?;
                    }
                }
                Err(e) if is_timeout(&e) => {}
                Err(e) => return Err(e.context("world connection lost")),
            }
        }
    }

    fn settle(&mut self, d: Duration) -> Result<()> {
        self.pump_until(d, |_| false).map(|_| ())
    }

    fn on_event(&mut self, ev: SessionEvent) -> Result<()> {
        match ev {
            SessionEvent::ObjectCreate { guid, fields, .. } if guid == self.me => {
                self.fields = fields;
            }
            SessionEvent::ItemCreate {
                guid,
                container,
                fields,
            } => {
                if let Some(entry) = fields.object_entry() {
                    self.items.insert(guid, entry);
                }
                if container {
                    self.bags.insert(guid, fields);
                }
            }
            SessionEvent::ObjectValues { guid, fields } => {
                if guid == self.me {
                    self.fields.merge(fields);
                } else if let Some(bag) = self.bags.get_mut(&guid) {
                    bag.merge(fields);
                }
            }
            SessionEvent::ObjectDestroyed(guid) => {
                self.items.remove(&guid);
                self.bags.remove(&guid);
            }
            SessionEvent::InventoryFailure {
                reason, item_guid, ..
            } => self.failures.push((item_guid, reason)),
            SessionEvent::CinematicTriggered { .. } => self.s.complete_cinematic()?,
            SessionEvent::Teleport { guid, counter, .. } if guid == self.me => {
                self.s.teleport_ack(guid, counter)?;
                self.moved = true;
            }
            SessionEvent::Worldport { needs_ack, .. } => {
                if needs_ack {
                    self.s.worldport_ack()?;
                    self.moved = true;
                }
            }
            SessionEvent::Chat(m) if m.chat_type == 0x0A => self.heard(m.text),
            SessionEvent::Notification { text } => self.heard(text),
            SessionEvent::Disconnected { reason, .. } => bail!("disconnected: {reason}"),
            _ => {}
        }
        Ok(())
    }

    fn heard(&mut self, line: String) {
        if self.said.len() == 6 {
            self.said.pop_front();
        }
        self.said.push_back(line);
    }

    /// What the server said last, for an error message.
    fn last_said(&self) -> String {
        let lines: Vec<&str> = self.said.iter().map(String::as_str).collect();
        if lines.is_empty() {
            "the server said nothing".into()
        } else {
            format!("the server said: {}", lines.join(" / "))
        }
    }

    fn gm(&mut self, cmd: &str) -> Result<()> {
        self.s.send_chat(cmd)?;
        self.settle(Duration::from_millis(150))
    }

    /// Where an item sits as `(bag, slot)` for `CMSG_AUTOEQUIP_ITEM`: the backpack is bag 255
    /// slots 23..39, an equipped bag is its own inventory slot 19..23 with its container slots.
    fn locate(&self, item: u64) -> Option<(u8, u8)> {
        if let Some(i) = (0..16).find(|&i| self.fields.player_pack_slot(i) == Some(item)) {
            return Some((255, 23 + i));
        }
        for b in 19..23u8 {
            let Some(bag) = self.fields.player_inv_slot(b).filter(|g| *g != 0) else {
                continue;
            };
            let Some(desc) = self.bags.get(&bag) else {
                continue;
            };
            let n = desc.container_num_slots().unwrap_or(0).min(36) as u8;
            if let Some(j) = (0..n).find(|&j| desc.container_slot(j) == Some(item)) {
                return Some((b, j));
            }
        }
        None
    }

    fn equipped(&self, item: u64, slots: std::ops::Range<u8>) -> bool {
        slots
            .into_iter()
            .any(|i| self.fields.player_inv_slot(i) == Some(item))
    }

    /// `.additem` one of `entry`, then auto-equip it into `slots` (equipment 0..19 or bags 19..23).
    fn add_and_equip(&mut self, entry: u32, slots: std::ops::Range<u8>) -> Result<(), String> {
        let before: HashSet<u64> = self
            .items
            .iter()
            .filter(|(_, e)| **e == entry)
            .map(|(g, _)| *g)
            .collect();
        self.s
            .send_chat(&format!(".additem {entry}"))
            .map_err(|e| e.to_string())?;
        let mut found = None;
        let _ = self.pump_until(STEP, |b| {
            b.items
                .iter()
                .any(|(g, e)| *e == entry && !before.contains(g) && b.locate(*g).is_some())
        });
        for (g, e) in &self.items {
            if *e == entry && !before.contains(g) {
                if let Some(at) = self.locate(*g) {
                    found = Some((*g, at));
                }
            }
        }
        let Some((guid, (bag, slot))) = found else {
            return Err("the server did not add it (unknown item?)".into());
        };
        self.failures.clear();
        self.s
            .auto_equip_item(bag, slot)
            .map_err(|e| e.to_string())?;
        let range = slots.clone();
        let ok = self
            .pump_until(STEP, |b| {
                b.equipped(guid, range.clone()) || b.failures.iter().any(|(g, _)| *g == guid)
            })
            .map_err(|e| e.to_string())?;
        if self.equipped(guid, slots) {
            return Ok(());
        }
        match self.failures.iter().find(|(g, _)| *g == guid) {
            Some((_, reason)) => Err(format!("refused (inventory result {reason:#04x})")),
            None if ok => Err("refused".into()),
            None => Err("no answer to the equip".into()),
        }
    }
}

pub fn build(servers: &Servers, job: &Job<'_>, log: &mut dyn FnMut(&str)) -> Result<Built> {
    let logon = benilla_protocol::logon(&servers.realmd, job.account, job.password)
        .context("logging in to realmd")?;
    let world = if servers.mangosd.contains(':') {
        servers.mangosd.clone()
    } else {
        format!("{}:{}", servers.mangosd, benilla_protocol::WORLD_PORT)
    };
    let mut s = WorldSession::connect(&world, job.account, logon.session_key)
        .with_context(|| format!("entering the world server at {world}"))?;

    // A fresh account, always: one that already has a character is somebody else's.
    if !s.char_enum()?.is_empty() {
        bail!("{} already has a character", job.account);
    }
    let chars = {
        let mut created = false;
        for name in job.names {
            let req = CharCreateReq {
                name: name.clone(),
                race: job.slot.race,
                class: job.slot.class,
                gender: job.slot.gender,
                skin: 0,
                face: 0,
                hair_style: 0,
                hair_color: 0,
                facial_hair: 0,
            };
            match s.create_character(&req)? {
                CHAR_CREATE_SUCCESS => {
                    created = true;
                    break;
                }
                // Taken, reserved or refused by the name rules: the next candidate.
                CHAR_CREATE_NAME_IN_USE | 0x43..=0x4f => log(&format!("name {name} refused")),
                other => bail!("character creation failed (result {other:#04x})"),
            }
        }
        if !created {
            bail!("every candidate name was refused");
        }
        s.char_enum()?
    };
    let ch = chars
        .first()
        .context("no character after creation")?
        .clone();
    log(&format!("created {} (guid {})", ch.name, ch.guid));

    // Long enough that a read never times out halfway through a packet (a timed-out `read_exact`
    // would lose the bytes it had), short enough to keep the pump responsive.
    s.set_read_timeout(Some(Duration::from_millis(500)))?;
    s.player_login(ch.guid)?;
    s.set_active_mover(ch.guid)?;
    let mut bot = Bot {
        s,
        me: ch.guid,
        fields: ObjectFields::default(),
        items: HashMap::new(),
        bags: HashMap::new(),
        failures: Vec::new(),
        moved: false,
        said: Default::default(),
    };
    // In the world once our own descriptor has streamed.
    if !bot.pump_until(Duration::from_secs(20), |b| b.fields.unit_level().is_some())? {
        bail!("entered the world but our character never streamed");
    }
    bot.settle(Duration::from_secs(1))?;

    let slot = job.slot;
    // `.levelup` adds levels to the current one; asked again if the first did not land (a
    // command said in the first moments in the world can go unheard).
    let want = u32::from(job.level);
    for _ in 0..3 {
        let now = bot.fields.unit_level().unwrap_or(1);
        if now >= want {
            break;
        }
        bot.gm(&format!(".levelup {}", want - now))?;
        bot.pump_until(STEP, |b| b.fields.unit_level() >= Some(want))?;
    }
    if bot.fields.unit_level() < Some(want) {
        bail!(
            "level is {:?} after .levelup, wanted {want} ({})",
            bot.fields.unit_level(),
            bot.last_said()
        );
    }
    log("levelled");
    // Talents before the trainer spells: a talent ability's higher ranks come from the trainer,
    // and its first rank — the talent itself — is not taken once a higher rank is known.
    for id in slot
        .proficiencies
        .iter()
        .chain(&slot.talents)
        .chain(&slot.spells)
    {
        bot.gm(&format!(".learn {id}"))?;
    }
    bot.gm(".maxskill")?;
    bot.settle(Duration::from_millis(500))?;
    log("spells and talents learned");

    let mut not_equipped = Vec::new();
    for &entry in &slot.gear {
        if let Err(why) = bot.add_and_equip(entry, 0..19) {
            log(&format!("item {entry}: {why}"));
            not_equipped.push((entry, why));
        }
    }
    for &entry in &slot.bags {
        if let Err(why) = bot.add_and_equip(entry, 19..23) {
            log(&format!("bag {entry}: {why}"));
            not_equipped.push((entry, why));
        }
    }
    log("gear equipped");
    for &(entry, count) in &slot.consumables {
        bot.gm(&format!(".additem {entry} {count}"))?;
    }
    if job.money > 0 {
        bot.gm(&format!(".modify money {}", job.money))?;
    }
    bot.settle(Duration::from_millis(500))?;

    bot.moved = false;
    bot.gm(&format!(".tele {}", job.tele))?;
    if !bot.pump_until(Duration::from_secs(10), |b| b.moved)? {
        bail!(
            "the teleport to {} never arrived ({})",
            job.tele,
            bot.last_said()
        );
    }
    // Let the new map stream in before logging out there.
    bot.settle(Duration::from_secs(2))?;
    bot.s
        .logout(Duration::from_secs(15))
        .context("logging out (which saves the character)")?;
    log("at the entrance, logged out");
    Ok(Built {
        name: ch.name,
        guid: ch.guid,
        not_equipped,
    })
}
