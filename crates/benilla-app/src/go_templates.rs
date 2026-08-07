//! The ask-once GameObject template cache (decision 0239) — benilla's `GAMEOBJECT_QUERY` store.
//!
//! A GameObject's **lockId** (a type-specific slot of its template `data[]`) decides its right-click
//! action: a locked object (chest / mining vein / herb node / locked door) casts an `OPEN_LOCK` spell
//! at it, an unlocked one sends `CMSG_GAMEOBJ_USE`. The lockId isn't in the create packet, so it's
//! fetched ask-once by entry when a GameObject streams in ([`GameObjectTemplates::request`], warmed
//! from [`crate::net::apply`]'s `ObjectCreate`, the same discipline as the name cache), the answer
//! resolved in [`GameObjectTemplates::insert`], and read by the interact routing ([`crate::target`]).

use std::collections::{HashMap, HashSet};

use benilla_formats::{LockCatalog, LockTypeCatalog};
use benilla_protocol::guid;
use bevy::prelude::*;

use crate::net::{ClientCommand, NetCommands};

/// `Lock.dbc` as a resource (decision 0239) — `lockId → requirement slots`, read by the interact
/// routing to decide use-vs-cast and, for a lockable object, which `LockType` the opener spell must
/// match. Loaded once at startup ([`crate::entities`]); absent when the client data is.
#[derive(Resource)]
pub(crate) struct Locks(pub(crate) LockCatalog);

/// `LockType.dbc` as a resource (decision 0236) — `LockType.Id → cursor stem`, read by the world
/// cursor's GameObject branch ([`crate::target`]) to name a lockable object's cursor by data
/// (`PickLock`/`GatherHerbs`/`Mine`), the client's own lock → LockType → CursorName chain. Loaded
/// once at startup; absent when the client data is, in which case a lock-bearing GO shows the
/// generic Interact gear like any other base type.
#[derive(Resource)]
pub(crate) struct LockTypes(pub(crate) LockTypeCatalog);

/// A cached GameObject template — what the interact routing and the hover tooltip need. The
/// `type_id` is consumed transiently to pick the lock's `data[]` slot (the entity already
/// carries it).
#[derive(Clone)]
pub(crate) struct GoTemplate {
    /// The `Lock.dbc` id from the type's data slot — `0` means no lock (opens by `CMSG_GAMEOBJ_USE`).
    pub(crate) lock_id: u32,
    /// The display name — the hover tooltip's gold first line (decision 0276's GO law).
    pub(crate) name: String,
    /// The vanilla **highlight** column, for the two types whose mouseover-eligibility slot reads
    /// it instead of running a predicate (decision 1106): GENERIC(5)'s `data[1]` (`0x5f4830`,
    /// decision 0762 — nonzero on 1387 of the 1870 shipped type-5 templates, which is why a road
    /// signpost hovers and the scenery beside it never does) and CAPTURE_POINT(29)'s `data[19]`
    /// (`0x5f6d80` — byte-for-byte the same shape, a different slot). `false` for every other type,
    /// which never consults it.
    pub(crate) highlight_column: bool,
    /// MEETINGSTONE (type 23) only: the template's `data[2]` = **areaID**, the sole input of that
    /// type's own `highlightable` slot (`0x5f6990` — decision 1106). `None` for every other type.
    pub(crate) meeting_stone_area: Option<u32>,
    /// MO_TRANSPORT (type 15) path parameters — `Some` only for boats/zeppelins (decision 0438):
    /// the template's `data0..2` = (taxiPathId, moveSpeed, accelRate), the inputs the transport
    /// timetable is built from.
    pub(crate) mo_transport: Option<MoTransport>,
    /// TEXT (type 9) only: the book/plaque's page chain head + frame material (decision 1105).
    /// `Some` with a nonzero `page_id` is what makes a right-click *read* it; a type-9 template
    /// with no page (vanilla ships a handful) opens nothing at all, exactly like the reference.
    pub(crate) text_page: Option<TextPage>,
}

/// A MO_TRANSPORT template's path tuple (`gameobject_template.data0..2`, decision 0438).
#[derive(Clone, Copy)]
pub(crate) struct MoTransport {
    pub(crate) taxi_path_id: u32,
    pub(crate) move_speed: f32,
    pub(crate) accel_rate: f32,
}

/// A TEXT (type 9) template's readable head — `data[0]`/`data[2]` (vmangos `GameObjectInfo::text`
/// = `pageID, language, pageMaterial, allowMounted`; decision 1105). The client reads both through
/// the same per-type attribute→slot table (`0x621b00`, attribute `0x11` = pageMaterial), so the
/// two layouts are the one layout.
#[derive(Clone, Copy)]
pub(crate) struct TextPage {
    /// `data[0]` — the first page's `PageText` id; `0` = nothing to read.
    pub(crate) page_id: u32,
    /// `data[2]` — the `PageTextMaterial.dbc` id the reader's frame paints with.
    pub(crate) material: u32,
}

/// `entry → template`, ask-once per connection (mirrors [`crate::names::NameCache`]'s discipline).
#[derive(Resource, Default)]
pub(crate) struct GameObjectTemplates {
    templates: HashMap<u32, GoTemplate>,
    pending: HashSet<u32>,
}

impl GameObjectTemplates {
    /// Ask the server for a GameObject's template if not already known or in flight (once per entry).
    /// `guid` names the asking object; the server answers by entry, so all spawns of a template share
    /// the one query.
    pub(crate) fn request(&mut self, guid: u64, commands: &NetCommands) {
        let Some(entry) = guid::entry(guid) else {
            return;
        };
        if self.templates.contains_key(&entry) || !self.pending.insert(entry) {
            return;
        }
        debug!("go: asking template (entry {entry}, guid {guid:#x})");
        let _ = commands
            .0
            .send(ClientCommand::GameObjectQuery { entry, guid });
    }

    /// Record a `SMSG_GAMEOBJECT_QUERY_RESPONSE`, resolving the lockId from the type-specific
    /// `data[]` slot. A miss (server didn't know the entry) arrives zeroed → `lock_id = 0` (no lock).
    pub(crate) fn insert(&mut self, entry: u32, type_id: u32, name: String, data: &[i32; 24]) {
        self.pending.remove(&entry);
        let lock_id = go_lock_slot(type_id)
            .and_then(|slot| data.get(slot))
            .map(|&v| v.max(0) as u32)
            .unwrap_or(0);
        // The highlight column, at the slot its type reads it from (decision 1106): GENERIC(5)
        // `data[1]`, CAPTURE_POINT(29) `data[19]`. Both slots are resolved by the same
        // `0x621b00(type, semantic 0x12)` lookup in the reference; the two shipped answers are
        // inlined here for the same reason [`go_lock_slot`] inlines the lock's.
        let highlight_column = match type_id {
            5 => data[1] != 0,
            29 => data[19] != 0,
            _ => false,
        };
        // MEETINGSTONE (23): data[2] = areaID (vmangos `gameobject_template`), the one input of
        // that type's own highlightable slot.
        let meeting_stone_area = (type_id == 23).then(|| data[2].max(0) as u32);
        // MO_TRANSPORT (15): data0..2 = taxiPathId / moveSpeed / accelRate (vmangos
        // `GameObjectInfo::moTransport`; decision 0438).
        let mo_transport = (type_id == 15).then(|| MoTransport {
            taxi_path_id: data[0].max(0) as u32,
            move_speed: data[1].max(0) as f32,
            accel_rate: data[2].max(0) as f32,
        });
        // TEXT (9): data[0] = pageID, data[2] = pageMaterial (decision 1105).
        let text_page = (type_id == 9).then(|| TextPage {
            page_id: data[0].max(0) as u32,
            material: data[2].max(0) as u32,
        });
        self.templates.insert(
            entry,
            GoTemplate {
                lock_id,
                name,
                highlight_column,
                meeting_stone_area,
                mo_transport,
                text_page,
            },
        );
    }

    /// The cached template for a GameObject guid, or `None` if its query hasn't answered yet.
    pub(crate) fn get(&self, guid: u64) -> Option<&GoTemplate> {
        guid::entry(guid).and_then(|e| self.templates.get(&e))
    }
}

/// Which `data[]` slot holds a GameObject type's lockId (mangos `GameObjectInfo::GetLockId`), or
/// `None` for a type that never carries one. DOOR/BUTTON keep it at slot 1 (slot 0 is `startOpen`),
/// FISHINGHOLE at slot 4, and the rest that have a lock at slot 0.
fn go_lock_slot(type_id: u32) -> Option<usize> {
    match type_id {
        // DOOR(0), BUTTON(1): data[0] = startOpen, data[1] = lockId.
        0 | 1 => Some(1),
        // QUESTGIVER(2), CHEST(3), TRAP(6), GOOBER(10), AREADAMAGE(12), CAMERA(13), FLAGSTAND(24),
        // FLAGDROP(26): data[0] = lockId. (Gathering nodes are CHEST(3).)
        2 | 3 | 6 | 10 | 12 | 13 | 24 | 26 => Some(0),
        // FISHINGHOLE(25): data[4] = lockId.
        25 => Some(4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_slot_by_type_matches_getlockid() {
        assert_eq!(go_lock_slot(3), Some(0)); // CHEST (and gathering nodes)
        assert_eq!(go_lock_slot(0), Some(1)); // DOOR (slot 0 is startOpen)
        assert_eq!(go_lock_slot(1), Some(1)); // BUTTON
        assert_eq!(go_lock_slot(25), Some(4)); // FISHINGHOLE
        assert_eq!(go_lock_slot(5), None); // GENERIC — never a lock
        assert_eq!(go_lock_slot(19), None); // MAILBOX — no lock (opens by USE)
    }

    #[test]
    fn insert_captures_mo_transport_tuple() {
        let mut t = GameObjectTemplates::default();
        let mut data = [0i32; 24];
        // The Menethil–Theramore boat (entry 176231): taxiPathId 292, moveSpeed 30, accelRate 1
        // (vmangos gameobject_template, decision 0438).
        data[0] = 292;
        data[1] = 30;
        data[2] = 1;
        t.insert(176231, 15, "Proudmore's Treasure".into(), &data);
        let mo = t.templates[&176231].mo_transport.expect("type 15 captures");
        assert_eq!(mo.taxi_path_id, 292);
        assert_eq!(mo.move_speed, 30.0);
        assert_eq!(mo.accel_rate, 1.0);
        // A chest doesn't.
        t.insert(2, 3, "Chest".into(), &data);
        assert!(t.templates[&2].mo_transport.is_none());
    }

    /// TEXT (9) captures the readable head — `data[0]` page id, `data[2]` material (decision
    /// 1105); no other type does.
    #[test]
    fn insert_captures_the_text_page_head() {
        let mut t = GameObjectTemplates::default();
        let mut data = [0i32; 24];
        data[0] = 1416; // pageID
        data[1] = 0; // language
        data[2] = 2; // pageMaterial (Stone)
        t.insert(2036, 9, "Book".into(), &data);
        let page = t.templates[&2036].text_page.expect("type 9 captures");
        assert_eq!(page.page_id, 1416);
        assert_eq!(page.material, 2);
        // A goober with the same bytes does not — its data[0] is a lockId.
        t.insert(2037, 10, "Lever".into(), &data);
        assert!(t.templates[&2037].text_page.is_none());
    }

    #[test]
    fn insert_resolves_chest_lockid_from_slot_0() {
        let mut t = GameObjectTemplates::default();
        let mut data = [0i32; 24];
        data[0] = 38; // a Copper Vein's lockId lives in chest slot 0
                      // guid 0xF110_0000_026C_3xxx — HIGHGUID_GAMEOBJECT with entry 1731 (0x6C3) in bits 24..47.
        let guid = 0xF110_0000_0000_0000 | (1731u64 << 24) | 0x40;
        t.insert(
            guid::entry(guid).unwrap(),
            3,
            "Alliance Chest".into(),
            &data,
        );
        assert_eq!(t.get(guid).map(|g| g.lock_id), Some(38));
    }
}
