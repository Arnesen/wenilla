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
    /// Creature template records by entry; the outer `None` = the server flagged the entry
    /// unknown. The subname is the overhead/tooltip title line ("Stable Master", …); the type is
    /// the `CreatureType.dbc` id the TAB-target critter filter reads; rank/civilian feed the
    /// unit tooltip's level-line word + CIVILIAN line (decision 0276).
    creatures: HashMap<u32, Option<CreatureRecord>>,
    pending_players: HashSet<u64>,
    pending_creatures: HashSet<u32>,
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
        } else if guid::is_creature_or_pet(guid_val) {
            let entry = guid::entry(guid_val)?;
            self.resolve_creature(entry, guid_val, commands)
        } else {
            None
        }
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
        } else if guid::is_creature_or_pet(guid_val) {
            self.creatures
                .get(&guid::entry(guid_val)?)
                .and_then(|n| n.as_ref().map(|r| r.name.as_str()))
        } else {
            None
        }
    }

    /// Record a player-name answer (`SMSG_NAME_QUERY_RESPONSE`). An empty wire name means the server
    /// doesn't know the guid — cached as a negative answer.
    pub(crate) fn insert_player(&mut self, guid: u64, name: String) {
        self.pending_players.remove(&guid);
        self.players
            .insert(guid, (!name.is_empty()).then_some(name));
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

    /// Forget the in-flight asks (a disconnect may have dropped them on the writer floor); the
    /// resolved names stay — they are stable across sessions.
    pub(crate) fn clear_pending(&mut self) {
        self.pending_players.clear();
        self.pending_creatures.clear();
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
        cache.insert_player(g, String::new()); // server: unknown guid
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
