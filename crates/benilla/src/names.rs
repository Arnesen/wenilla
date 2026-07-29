//! Unit-name resolution — the query-cache seam of decision 0068 §3.
//!
//! The 1.12 wire carries **no names in descriptors**: a player's name answers `CMSG_NAME_QUERY`
//! (keyed by guid), a creature's answers `CMSG_CREATURE_QUERY` (keyed by the template *entry*
//! embedded in its guid, shared by every spawn of that template — exactly how the real client
//! recovers it). This module owns the cache and the **ask-once** discipline: a consumer calls
//! [`NameCache::resolve`], which returns the name when known and otherwise issues the query (deduped
//! while in flight) and reports "not yet". The net bridge ([`crate::net`]) fills the cache from the
//! decoded `PlayerName`/`CreatureName` events and clears the in-flight sets on disconnect (a query
//! dropped by a dead writer must be re-askable after reconnect).
//!
//! A *negative* answer (the server doesn't know the guid/entry) is cached too — resolving to
//! "unknown, and asking again won't help" — so a bad id can never turn into a query loop.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use benilla_protocol::guid;

use crate::net::{ClientCommand, NetCommands};

/// The name cache: players by guid, creatures by template entry, plus the in-flight ask-once sets.
/// Filled by the net bridge; read (and query-triggered) through [`Self::resolve`].
#[derive(Resource, Default)]
pub(crate) struct NameCache {
    /// Player names by guid. `Some(None)`-shaped answers are stored as `None`: the server was asked
    /// and didn't know (an empty wire name) — cached so we never re-ask a dead guid.
    players: HashMap<u64, Option<String>>,
    /// `(race, class, gender)` from the same `SMSG_NAME_QUERY_RESPONSE` that carried the name — the
    /// wire has always sent these three and we used to drop them. They exist for one reason: the
    /// `$`-macro expander's non-player-subject path. The reference resolves a macro subject from the
    /// object manager first and falls back to **this** cache record when the unit isn't streamed
    /// (`questtext-macro-expander.md` §1), so without them `$R`/`$C`/`$G` against an off-screen
    /// player would silently read race 0.
    player_traits: HashMap<u64, (u8, u8, u8)>,
    /// Creature template records by entry; the outer `None` = the server flagged the entry
    /// unknown. The subname is the overhead/tooltip title line ("Stable Master", …); the type is
    /// the `CreatureType.dbc` id the TAB-target critter filter reads; rank/civilian feed the
    /// unit tooltip's level-line word + CIVILIAN line (decision 0276).
    creatures: HashMap<u32, Option<CreatureRecord>>,
    /// Pet names by **pet number** — the third naming path (see [`Self::resolve`]'s pet branch).
    /// No negative entry exists: the server answers for a live pet or says nothing at all, so an
    /// unanswered ask stays unanswered, exactly as it does for the real client.
    pets: HashMap<u32, String>,
    pending_players: HashSet<u64>,
    pending_creatures: HashSet<u32>,
    pending_pets: HashSet<u32>,
}

/// One cached creature template head (see [`NameCache::creatures`]).
#[derive(Clone, Debug)]
pub(crate) struct CreatureRecord {
    pub(crate) name: String,
    pub(crate) subname: Option<String>,
    pub(crate) creature_type: u32,
    /// Elite rank 0..4 — the tooltip rank word `{"", Elite, Elite, Boss, ""}`.
    pub(crate) rank: u32,
    /// Template type flags — bit `0x10` (HIDE_FACTION_TOOLTIP) suppresses the tooltip's
    /// faction-name line (the client's `0x612610` gate).
    pub(crate) type_flags: u32,
    pub(crate) civilian: bool,
    /// Racial leader — the tooltip's white LEADER line (`0x6125c0`).
    pub(crate) racial_leader: bool,
}

impl NameCache {
    /// The name for `guid`, if known. On a miss, sends the right query (once per guid/entry per
    /// connection) and returns `None` — call again after the answer lands. A guid family that has no
    /// name on the 1.12 wire (GameObjects resolve via their own query, not modeled yet) is `None`
    /// without a query.
    pub(crate) fn resolve(&mut self, guid_val: u64, commands: &NetCommands) -> Option<&str> {
        if guid::is_player(guid_val) {
            if !self.players.contains_key(&guid_val) {
                if self.pending_players.insert(guid_val) {
                    debug!("names: asking player name (guid {guid_val})");
                    let _ = commands.0.send(ClientCommand::NameQuery { guid: guid_val });
                }
                return None;
            }
            self.players.get(&guid_val).and_then(|n| n.as_deref())
        } else if let Some(pet_number) = guid::pet_number(guid_val) {
            // A pet is a `TYPEID_UNIT` like any creature, but it carries a pet number where a
            // creature carries its template entry, so it has its own query. Asking the creature
            // query for that number is what left every summoned pet nameless.
            self.resolve_pet(pet_number, guid_val, commands)
        } else if guid::is_creature_or_pet(guid_val) {
            let entry = guid::entry(guid_val)?;
            self.resolve_creature(entry, guid_val, commands)
        } else {
            None
        }
    }

    /// The name for a live pet by its `pet_number` — the pet twin of [`Self::resolve_creature`],
    /// same ask-once discipline. Unlike the creature/player queries this one has **no negative
    /// answer**: vmangos returns early without a packet when the guid is not a live pet bearing that
    /// number (`PetHandler.cpp:190-192`), so an unanswered ask simply stays unresolved rather than
    /// caching a "server doesn't know" — and is re-asked after a reconnect like the others.
    fn resolve_pet(&mut self, pet_number: u32, guid: u64, commands: &NetCommands) -> Option<&str> {
        if !self.pets.contains_key(&pet_number) {
            if self.pending_pets.insert(pet_number) {
                debug!("names: asking pet name (pet {pet_number})");
                let _ = commands
                    .0
                    .send(ClientCommand::PetNameQuery { pet_number, guid });
            }
            return None;
        }
        self.pets.get(&pet_number).map(String::as_str)
    }

    /// The name for a creature template `entry`, if known — the entry-keyed twin of
    /// [`Self::resolve`]'s creature branch (same cache, same ask-once discipline), for a caller that
    /// has no live spawn guid to decode an entry from: a quest objective names its kill target only
    /// by the template's raw `creature_or_go` entry (`crate::ui_quest_log`). `guid` rides along for
    /// the query body when a real spawn is known, `0` otherwise — the server answers by entry
    /// regardless of which spawn asked (the same template-only convention as
    /// [`crate::items::Items::template`]'s `guid: 0`).
    pub(crate) fn resolve_creature(
        &mut self,
        entry: u32,
        guid: u64,
        commands: &NetCommands,
    ) -> Option<&str> {
        if !self.creatures.contains_key(&entry) {
            if self.pending_creatures.insert(entry) {
                debug!("names: asking creature name (entry {entry})");
                let _ = commands
                    .0
                    .send(ClientCommand::CreatureQuery { entry, guid });
            }
            return None;
        }
        self.creatures
            .get(&entry)
            .and_then(|n| n.as_ref().map(|r| r.name.as_str()))
    }

    /// The cached name for `guid`, read-only — no query on a miss (the trace/diagnostic twin of
    /// [`Self::resolve`], for callers that must not mutate the ask-once state).
    pub(crate) fn peek(&self, guid_val: u64) -> Option<&str> {
        if guid::is_player(guid_val) {
            self.players.get(&guid_val).and_then(|n| n.as_deref())
        } else if let Some(pet_number) = guid::pet_number(guid_val) {
            self.pets.get(&pet_number).map(String::as_str)
        } else if guid::is_creature_or_pet(guid_val) {
            self.creatures
                .get(&guid::entry(guid_val)?)
                .and_then(|n| n.as_ref().map(|r| r.name.as_str()))
        } else {
            None
        }
    }

    /// Record a player-name answer. An empty wire name means the server doesn't know the guid —
    /// cached as a negative answer.
    ///
    /// `traits` is the `(race, class, gender)` triple that rides `SMSG_NAME_QUERY_RESPONSE`, kept
    /// for [`Self::player_traits`]. It is `None` for the seams that learn a name **without** that
    /// packet — the login-time seed of our own name, and the capture fixtures — rather than having
    /// them invent a zero triple that would read as a real (and wrong) race.
    pub(crate) fn insert_player(&mut self, guid: u64, name: String, traits: Option<(u8, u8, u8)>) {
        self.pending_players.remove(&guid);
        if !name.is_empty() {
            if let Some(t) = traits {
                self.player_traits.insert(guid, t);
            }
        }
        self.players
            .insert(guid, (!name.is_empty()).then_some(name));
    }

    /// `(race, class, gender)` for a player guid the name query has answered for — the macro
    /// expander's fallback when the unit isn't streamed. See [`NameCache::player_traits`].
    pub(crate) fn player_traits(&self, guid: u64) -> Option<(u8, u8, u8)> {
        self.player_traits.get(&guid).copied()
    }

    /// Record a pet-name answer (`SMSG_PET_NAME_QUERY_RESPONSE`), keyed by pet number.
    pub(crate) fn insert_pet(&mut self, pet_number: u32, name: String) {
        self.pending_pets.remove(&pet_number);
        self.pets.insert(pet_number, name);
    }

    /// Record a creature-name answer (`SMSG_CREATURE_QUERY_RESPONSE`); `None` = unknown entry.
    pub(crate) fn insert_creature(&mut self, entry: u32, record: Option<CreatureRecord>) {
        self.pending_creatures.remove(&entry);
        self.creatures.insert(entry, record);
    }

    /// The cached subname (the overhead/tooltip title line) for a creature entry — read-only: the
    /// nameplate asks only after [`Self::resolve`] already returned the name (same answer packet).
    pub(crate) fn creature_subname(&self, entry: u32) -> Option<&str> {
        self.creatures
            .get(&entry)?
            .as_ref()
            .and_then(|r| r.subname.as_deref())
    }

    /// The whole cached record for a creature entry — the unit tooltip's read (subtitle, type,
    /// rank word, civilian — decision 0276's level-line law). Read-only, the subname's ask-once
    /// discipline.
    pub(crate) fn creature_record(&self, entry: u32) -> Option<&CreatureRecord> {
        self.creatures.get(&entry)?.as_ref()
    }

    /// The cached `CreatureType.dbc` id for a creature entry — read-only, same ask-once discipline
    /// as the subname (the TAB-target scan reads it; an unresolved entry is `None`, which the scan
    /// treats as targetable — the client's own out-of-range skip).
    pub(crate) fn creature_type(&self, entry: u32) -> Option<u32> {
        self.creatures
            .get(&entry)?
            .as_ref()
            .map(|r| r.creature_type)
    }

    /// Forget the in-flight asks (a disconnect may have dropped them on the writer floor). Resolved
    /// player/creature names stay — those are identities, stable across sessions. Resolved **pet**
    /// names do not: a pet number names one live spawn rather than a template, and nothing that
    /// answered before the disconnect is still on the map after it.
    pub(crate) fn clear_pending(&mut self) {
        self.pending_players.clear();
        self.pending_creatures.clear();
        self.pending_pets.clear();
        self.pets.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::TryRecvError;

    /// Compose a guid the way the server does: `counter | (entry << 24) | (high << 48)`.
    fn compose(high: u16, entry: u32, counter: u32) -> u64 {
        u64::from(counter) | (u64::from(entry) << 24) | (u64::from(high) << 48)
    }

    fn commands() -> (NetCommands, crossbeam_channel::Receiver<ClientCommand>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        (NetCommands(tx), rx)
    }

    /// A summoned pet resolves through `CMSG_PET_NAME_QUERY`, keyed by the pet number in its guid —
    /// NOT through the creature query, which would ask for a template entry that does not exist and
    /// leave the pet nameless (the reported bug: an NPC-summoned voidwalker with no name).
    #[test]
    fn a_pet_asks_the_pet_query_not_the_creature_query() {
        let (cmds, rx) = commands();
        let mut cache = NameCache::default();
        let voidwalker = compose(guid::HIGH_PET, 137, 9);

        assert_eq!(cache.resolve(voidwalker, &cmds), None);
        assert!(matches!(
            rx.try_recv(),
            Ok(ClientCommand::PetNameQuery {
                pet_number: 137,
                ..
            })
        ));
        // Ask-once, like every other name.
        assert_eq!(cache.resolve(voidwalker, &cmds), None);
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));

        cache.insert_pet(137, "Voidwalker".into());
        assert_eq!(cache.resolve(voidwalker, &cmds), Some("Voidwalker"));
        assert_eq!(cache.peek(voidwalker), Some("Voidwalker"));
        // Pet names are per-spawn, so a disconnect drops them.
        cache.clear_pending();
        assert_eq!(cache.peek(voidwalker), None);
    }

    #[test]
    fn creature_miss_queries_once_then_serves_the_answer() {
        let (cmds, rx) = commands();
        let mut cache = NameCache::default();
        let wolf_a = compose(guid::HIGH_UNIT, 69, 1);
        let wolf_b = compose(guid::HIGH_UNIT, 69, 2);

        assert_eq!(cache.resolve(wolf_a, &cmds), None);
        // Same entry, different spawn: no second query.
        assert_eq!(cache.resolve(wolf_b, &cmds), None);
        assert!(matches!(
            rx.try_recv(),
            Ok(ClientCommand::CreatureQuery { entry: 69, .. })
        ));
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));

        cache.insert_creature(
            69,
            Some(CreatureRecord {
                name: "Young Wolf".into(),
                subname: None,
                creature_type: 0,
                rank: 0,
                type_flags: 0,
                civilian: false,
                racial_leader: false,
            }),
        );
        assert_eq!(cache.resolve(wolf_b, &cmds), Some("Young Wolf"));
    }

    #[test]
    fn player_negative_answer_is_cached() {
        let (cmds, rx) = commands();
        let mut cache = NameCache::default();
        let g = compose(guid::HIGH_PLAYER, 0, 7);

        assert_eq!(cache.resolve(g, &cmds), None);
        assert!(matches!(
            rx.try_recv(),
            Ok(ClientCommand::NameQuery { guid }) if guid == g
        ));
        cache.insert_player(g, String::new(), None); // server: unknown guid
        assert_eq!(cache.resolve(g, &cmds), None);
        // …and no re-ask.
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn clear_pending_allows_a_reconnect_reask() {
        let (cmds, rx) = commands();
        let mut cache = NameCache::default();
        let g = compose(guid::HIGH_UNIT, 100, 1);

        assert_eq!(cache.resolve(g, &cmds), None);
        let _ = rx.try_recv();
        // The answer never lands (disconnect); pending cleared → the next resolve re-asks.
        cache.clear_pending();
        assert_eq!(cache.resolve(g, &cmds), None);
        assert!(matches!(
            rx.try_recv(),
            Ok(ClientCommand::CreatureQuery { entry: 100, .. })
        ));
    }
}
