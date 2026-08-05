//! The item layer — decision 0068's T2 (containers) groundwork.
//!
//! The wire splits item knowledge in two, so this module holds two stores:
//!
//! - **Objects** — the item/container *instances* the server streamed at us (`ItemCreate`: our own
//!   inventory at login, loot, trades; they are private, so only ours ever arrive). Keyed by guid,
//!   holding the merged descriptor fields — entry, stack count, a bag's slot guids. Which *slot*
//!   holds a guid lives one level up, in the player descriptor's `INV_SLOT`/`PACK_SLOT` arrays and
//!   a bag's `CONTAINER_FIELD_SLOT` array; this store resolves those guids to actual items. Fed by
//!   the net bridge (create seed → `Values` merges → destroy), cleared whole on disconnect (the
//!   server re-streams inventory at login; stale bags are worse than empty ones).
//!
//! - **Templates** — the static item *definitions* (`SMSG_ITEM_QUERY_SINGLE_RESPONSE`: name,
//!   quality, class, display id), keyed by entry and shared by every copy. The exact twin of
//!   [`crate::names::NameCache`], with the same **ask-once** discipline: [`Items::template`]
//!   returns the answer when known, otherwise sends the query (deduped while in flight) and
//!   reports "not yet". Negative answers are cached — a bad entry never becomes a query loop.
//!   Templates survive disconnect: item definitions are stable across sessions.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use benilla_protocol::{ItemInfo, ObjectFields};

use crate::net::{ClientCommand, NetCommands};

/// The slice of an item template that equipment rendering + combat animation consume (decisions
/// 0072/0073): the ItemDisplayInfo key, the two placement inputs, and the weapon class pair the
/// swing/ready selectors key on. A Copy **view** of the cached [`ItemInfo`] ([`Items::held`]).
#[derive(Clone, Copy)]
pub(crate) struct HeldTemplate {
    pub(crate) display_info_id: u32,
    pub(crate) inventory_type: u32,
    pub(crate) sheath: u32,
    pub(crate) class: u32,
    pub(crate) subclass: u32,
    /// `Material` — the item's `Material.dbc` id (1 metal · 2 wood · 5 chain · 6 plate · 7 cloth ·
    /// 8 leather · 0 undefined). On the wire in `SMSG_ITEM_QUERY_SINGLE_RESPONSE`, and the **only**
    /// input to the draw/stow sound pick: `SheatheSoundLookups` carries one row per weapon subclass
    /// per material, and every row of a material agrees — the subclass is inert (decision 0882).
    pub(crate) material: u32,
}

/// `SpellItemEnchantment.dbc`'s two consumer columns, loaded once and read by both lanes that
/// need them: the **visual** by the weapon-glow chain (decision 0805,
/// [`crate::entities::item_glow`]) and the **name** by the item tooltip's enchant line (decision
/// 0915, [`crate::ui_items`]). It lives here rather than inside either consumer because it is
/// item *data*, and because a second loader over one DBC is how a schema quietly drifts.
///
/// Optional, like every DBC-backed resource: absent, weapons draw unadorned and no tooltip prints
/// an enchant line — each lane's own pre-existing behaviour.
#[derive(Resource)]
pub(crate) struct Enchants(pub(crate) benilla_formats::EnchantCatalog);

/// One enchant SLOT's contribution, as the app resolved it — `(slot index, id, charges,
/// remaining ms)`. The id is **signed**, because its sign is load-bearing downstream: it picks the
/// line's colour and nothing else (wow-re §E3 — `abs(id)` names the DBC row either way).
pub(crate) type EnchantSlot = (u8, i32, u32, Option<u64>);

/// The tooltip lines an item instance's enchant slots contribute — the one place the app turns
/// enchant *ids* into text (decisions 0915/0920). Every tooltip surface feeds through here, so a
/// bag hover, a paper-doll hover and an inspect hover can never disagree.
///
/// The per-slot gate is the reference's, byte-verified (wow-re
/// `ui/scratch/tooltip-content-law.md` §E3): `id != 0`, then `abs(id)` must name a real
/// `SpellItemEnchantment` row — **the sign never changes which row**, only the colour the engine
/// paints. An id that names no row contributes nothing rather than a placeholder.
///
/// `slots` is the caller's source, and it differs by surface for a reason the wire fixes: our own
/// items stream as OBJECTS, so all 7 `ITEM_FIELD_ENCHANTMENT` slots (plus charges, plus the
/// `SMSG_ITEM_ENCHANT_TIME_UPDATE` timer) are readable; anybody else's are visible only through
/// the slots their descriptor broadcasts, which in 1.12 vmangos fills for PERM and TEMP only.
pub(crate) fn enchant_lines(
    slots: impl IntoIterator<Item = EnchantSlot>,
    enchants: Option<&Enchants>,
) -> Vec<benilla_ui::script::EnchantView> {
    let Some(enchants) = enchants else {
        return Vec::new();
    };
    let lines: Vec<benilla_ui::script::EnchantView> = slots
        .into_iter()
        .filter(|&(_, id, _, _)| id != 0)
        // `SpellItemEnchantment.Flags & 0x2` — the row prints NO line at all. Both of the
        // reference's enchant-line printers open with it and return before they ever read the
        // name (`6290e4` / `62923e`, each `testb $0x2, 0x5c(...)` → `jne <retl>`). Twelve shipped
        // rows, one family: the totem-granted weapon imbues, Firestone, Orb of Fire — buffs whose
        // source already shows elsewhere on screen, so the weapon does not repeat them. Found and
        // closed while transcribing the *other* bit of that column (decision 0928); 0915 read the
        // name column alone and printed all twelve.
        .filter(|&(_, id, _, _)| !enchants.0.tooltip_hides_name(id.unsigned_abs()))
        .filter_map(|(slot, id, charges, remaining_ms)| {
            let name = enchants.0.name(id.unsigned_abs())?.to_string();
            Some(benilla_ui::script::EnchantView {
                slot,
                name,
                negative: id < 0,
                charges,
                remaining_ms,
            })
        })
        .collect();
    // One breadcrumb per session the first time any surface resolves an enchant — the
    // machine-readable "this lane is live" signal for a feature whose whole symptom is ABSENCE
    // (`entities::item_glow`'s idiom, and the reason this one exists: the enchant ids reaching the
    // tooltip travel a DIFFERENT wire field from the ones the weapon glow reads, so a silent miss
    // here would look exactly like an unenchanted item).
    if !lines.is_empty() {
        static FIRST: std::sync::Once = std::sync::Once::new();
        // The whole view, not just the name: the countdown and the charges each ride a different
        // wire lane from the id, so "which of the three arrived" is exactly what this must answer.
        FIRST.call_once(|| info!("item enchant: {lines:?} (the first resolved this session)"));
    }
    lines
}

/// The item stores: instances by guid, templates by entry (+ the in-flight ask-once set).
/// Filled by the net bridge; read by the container APIs (`GetContainerItemInfo` and kin).
#[derive(Resource, Default)]
pub(crate) struct Items {
    objects: HashMap<u64, ObjectFields>,
    /// `None` = the server flagged the entry unknown (the top-bit miss branch) — cached negative.
    templates: HashMap<u32, Option<ItemInfo>>,
    pending: HashSet<u32>,
    /// Entries whose template landed since the last [`Self::take_fresh`] drain — the push half of
    /// the tooltip store (every landed template goes to the UI unprompted, so the first hover of
    /// an item whose name is already on screen never misses).
    fresh: Vec<u32>,
    /// Bumped by every landed answer ([`Self::insert_template`]), positive or negative. The
    /// **broadcast** twin of [`Self::fresh`]: `fresh` is a DRAIN (exactly one consumer can take
    /// it — the tooltip feed does), so a second consumer that caches a template-derived view
    /// needs its own signal. A consumer keeps the epoch it last resolved at and re-resolves when
    /// it advances — the modern stand-in for the ref's `DBCACHECALLBACK` redisplay (`0x6e29b0`),
    /// which is how the real client repaints a view that was drawn while the item cache was still
    /// answering. Any cache that gates a template read behind a change-flag needs this: the read
    /// itself is what ISSUES the ask-once query, so the first resolve of a cold entry ALWAYS
    /// misses (decision 0660).
    template_epoch: u64,
    /// The **temporary-enchant deadlines**, keyed `(item guid, enchant slot)` — the reference's
    /// per-item `[obj + slot*4 + 0x324]` array (wow-re `tooltip-content-law.md` §E3), whose only
    /// feed is `SMSG_ITEM_ENCHANT_TIME_UPDATE`. NOT derived from `ITEM_FIELD_ENCHANTMENT`'s
    /// duration field, which the reference's tooltip never reads. Session state, so it clears with
    /// the objects (decision 0920).
    enchant_deadlines: HashMap<(u64, u32), std::time::Instant>,
}

impl Items {
    /// Seed an item object from its create block. A re-create for a guid we already hold overlays
    /// the fresh snapshot onto the existing store (same rule as the scene's `ObjectStore`).
    pub(crate) fn insert_object(&mut self, guid: u64, fields: ObjectFields) {
        match self.objects.entry(guid) {
            std::collections::hash_map::Entry::Occupied(mut e) => e.get_mut().merge(fields),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(fields);
            }
        }
    }

    /// Merge a `Values` delta into a tracked item; `false` when the guid isn't ours to track (a
    /// delta for an item we never saw created — dropped, same as the scene path's unknown guids).
    pub(crate) fn merge_object(&mut self, guid: u64, fields: ObjectFields) -> bool {
        if let Some(store) = self.objects.get_mut(&guid) {
            store.merge(fields);
            true
        } else {
            false
        }
    }

    /// The item ceased to exist (`SMSG_DESTROY_OBJECT` — consumed, sold, destroyed).
    pub(crate) fn remove_object(&mut self, guid: u64) {
        self.objects.remove(&guid);
        self.enchant_deadlines.retain(|&(g, _), _| g != guid);
    }

    /// `SMSG_ITEM_ENCHANT_TIME_UPDATE` landed: park the absolute deadline for that item's enchant
    /// slot. `seconds == 0` is the reference's own "no timer" store (`0x5d9cc0`'s `<= 0` arm), so
    /// it REMOVES the entry rather than parking an already-expired one.
    pub(crate) fn set_enchant_deadline(&mut self, guid: u64, slot: u32, seconds: u32) {
        match seconds {
            0 => {
                self.enchant_deadlines.remove(&(guid, slot));
            }
            n => {
                let at = std::time::Instant::now() + std::time::Duration::from_secs(u64::from(n));
                self.enchant_deadlines.insert((guid, slot), at);
                debug!("enchant timer: item {guid:#x} slot {slot} → {n}s");
            }
        }
    }

    /// Milliseconds left on an item's temporary enchant, or `None` when that slot carries no timer
    /// — including an expired one (`0x5d9d00` returns 0 past the deadline, and the tooltip's
    /// `!= 0` gate then prints the plain name).
    pub(crate) fn enchant_remaining_ms(&self, guid: u64, slot: u32) -> Option<u64> {
        let at = self.enchant_deadlines.get(&(guid, slot))?;
        let left = at.saturating_duration_since(std::time::Instant::now());
        (!left.is_zero()).then_some(left.as_millis() as u64)
    }

    /// A tracked item object's merged descriptor fields.
    pub(crate) fn object(&self, guid: u64) -> Option<&ObjectFields> {
        self.objects.get(&guid)
    }

    /// The template for `entry`, if known. On a miss, asks the server (once per entry per
    /// connection; `guid` rides along when the ask is about a concrete item, `0` for
    /// template-only) and returns `None` — call again after the answer lands. A cached negative
    /// (server doesn't know the entry) is also `None`, without a re-ask.
    pub(crate) fn template(
        &mut self,
        entry: u32,
        guid: u64,
        commands: &NetCommands,
    ) -> Option<&ItemInfo> {
        if !self.templates.contains_key(&entry) {
            if self.pending.insert(entry) {
                debug!("items: asking template (entry {entry})");
                let _ = commands.0.send(ClientCommand::ItemQuery { entry, guid });
            }
            return None;
        }
        self.templates.get(&entry).and_then(|t| t.as_ref())
    }

    /// Whether the server has ANSWERED the `entry` query with "unknown item" — the cached
    /// negative, distinct from a still-pending ask (both read `None` from [`Self::template`]).
    /// The cast-fail redisplay queue (decision 0552) keys on it: pending → keep waiting for the
    /// answer (the ref's `DBCACHECALLBACK` redisplay), negative → give up and show the ref's
    /// `"UNKNOWN"` fallback instead of waiting forever.
    pub(crate) fn template_answered_unknown(&self, entry: u32) -> bool {
        self.templates.get(&entry).is_some_and(|t| t.is_none())
    }

    /// The held/worn display head for `entry` — the [`HeldTemplate`] view of [`Self::template`]
    /// (same ask-once discipline, template-only ask). Equipment rendering + the swing selector
    /// consume this Copy slice instead of borrowing the full info.
    pub(crate) fn held(&mut self, entry: u32, commands: &NetCommands) -> Option<HeldTemplate> {
        self.template(entry, 0, commands).map(|i| HeldTemplate {
            display_info_id: i.display_info_id,
            inventory_type: i.inventory_type,
            sheath: i.sheath,
            class: i.class,
            subclass: i.subclass,
            material: i.material,
        })
    }

    /// Record a template answer (`SMSG_ITEM_QUERY_SINGLE_RESPONSE`); `None` = unknown entry.
    pub(crate) fn insert_template(&mut self, entry: u32, info: Option<ItemInfo>) {
        self.pending.remove(&entry);
        if info.is_some() {
            self.fresh.push(entry);
        }
        self.templates.insert(entry, info);
        // A NEGATIVE answer bumps too: it flips the entry from "still asking" to "answered
        // unknown", which is a real display transition for anything that waits on the ask (the
        // cast-fail redisplay's `"UNKNOWN"` literal, [`Self::template_answered_unknown`]).
        self.template_epoch = self.template_epoch.wrapping_add(1);
    }

    /// The landed-template broadcast counter — see the [`Self::template_epoch`] field. Advances on
    /// every answer; a consumer that caches a template-derived view compares it against the value
    /// it last resolved at and re-resolves when they differ.
    pub(crate) fn template_epoch(&self) -> u64 {
        self.template_epoch
    }

    /// Drain the entries whose template landed since the last drain (see the `fresh` field).
    /// Every entry with a CACHED template — the re-push sweep's domain (a `$z`-style
    /// player-state input changing means every already-pushed view may be stale).
    pub(crate) fn cached_template_ids(&self) -> Vec<u32> {
        self.templates
            .iter()
            .filter_map(|(&id, t)| t.is_some().then_some(id))
            .collect()
    }

    pub(crate) fn take_fresh(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.fresh)
    }

    /// Disconnect: drop the instances (the server re-streams inventory at login) and the in-flight
    /// asks (a query dropped by a dead writer must be re-askable); keep the templates (static).
    pub(crate) fn clear_session(&mut self) {
        self.objects.clear();
        self.enchant_deadlines.clear();
        self.pending.clear();
    }
}

/// A minimal, VALID item template named `name` — the shared test seam for every module that
/// needs a landed template (the sentinels that matter are `allowable_*` = −1 and `stackable` = 1).
#[cfg(test)]
pub(crate) fn test_template(name: &str) -> ItemInfo {
    ItemInfo {
        class: 0,
        subclass: 0,
        name: name.into(),
        display_info_id: 1,
        quality: 1,
        flags: 0,
        buy_price: 0,
        sell_price: 0,
        inventory_type: 0,
        allowable_class: -1,
        allowable_race: -1,
        item_level: 0,
        required_level: 0,
        required_skill: 0,
        required_skill_rank: 0,
        required_spell: 0,
        required_honor_rank: 0,
        required_city_rank: 0,
        required_rep_faction: 0,
        required_rep_rank: 0,
        max_count: 0,
        stackable: 1,
        container_slots: 0,
        stats: Vec::new(),
        damages: Vec::new(),
        dmg_min: 0.0,
        dmg_max: 0.0,
        dmg_type: 0,
        armor: 0,
        resistances: [0; 6],
        delay_ms: 0,
        ammo_type: 0,
        ranged_mod_range: 0.0,
        spells: Vec::new(),
        spell_charges_0: 0,
        use_spell: None,
        bonding: 0,
        description: String::new(),
        page_text: 0,
        language_id: 0,
        page_material: 0,
        start_quest: 0,
        lock_id: 0,
        material: 0,
        sheath: 0,
        random_property: 0,
        block: 0,
        item_set: 0,
        max_durability: 0,
        area: 0,
        map: 0,
        bag_family: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::test_template as info;
    use benilla_protocol::messages::{
        ITEM_DYNFLAG_UNLOCKED, ITEM_DYNFLAG_WRAPPED, ITEM_FLAG_LOOTABLE, ITEM_FLAG_WRAPPER,
    };
    use crossbeam_channel::TryRecvError;

    fn commands() -> (NetCommands, crossbeam_channel::Receiver<ClientCommand>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        (NetCommands(tx), rx)
    }

    #[test]
    fn template_miss_queries_once_then_serves_the_answer() {
        let (cmds, rx) = commands();
        let mut items = Items::default();

        assert!(items.template(117, 0x42, &cmds).is_none());
        // Second copy of the same item: no second query.
        assert!(items.template(117, 0x43, &cmds).is_none());
        assert!(matches!(
            rx.try_recv(),
            Ok(ClientCommand::ItemQuery {
                entry: 117,
                guid: 0x42
            })
        ));
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));

        items.insert_template(117, Some(info("Tough Jerky")));
        assert_eq!(
            items.template(117, 0x43, &cmds).map(|i| i.name.as_str()),
            Some("Tough Jerky")
        );
    }

    /// The push half of the tooltip store: a landed template is marked fresh exactly once (the
    /// drain empties), and a cached negative never is (there's nothing to push).
    #[test]
    fn landed_templates_drain_as_fresh_once() {
        let mut items = Items::default();
        items.insert_template(117, Some(info("Tough Jerky")));
        items.insert_template(9999, None);
        assert_eq!(items.take_fresh(), vec![117]);
        assert!(items.take_fresh().is_empty(), "a drain empties the queue");
    }

    /// The right-click-to-open predicates — and the fact that the **line and the click are not the
    /// same test** (decision 0896). The tooltip's `shows_open_line` carries the lock sub-gate (a
    /// `LockID` template earns the line only once the INSTANCE says UNLOCKED); the click's
    /// `opens_loot` is the BARE template bit, so a still-locked junkbox sends anyway and the
    /// server supplies the refusal. Getting that backwards eats the click in silence.
    #[test]
    fn open_line_carries_the_lock_gate_the_open_send_does_not() {
        let plain = |flags: u32, lock_id: u32| {
            let mut t = info("Small Barnacled Clam");
            t.flags = flags;
            t.lock_id = lock_id;
            t
        };

        // The clam: LOOTABLE, no lock — line and send agree, instance flags irrelevant.
        assert!(plain(ITEM_FLAG_LOOTABLE, 0).shows_open_line(0));
        assert!(plain(ITEM_FLAG_LOOTABLE, 0).opens_loot());
        // An ordinary item does neither, however its instance is flagged.
        assert!(!plain(0, 0).shows_open_line(ITEM_DYNFLAG_UNLOCKED | ITEM_DYNFLAG_WRAPPED));
        assert!(!plain(0, 0).opens_loot());
        // A junkbox: LOOTABLE but locked. **The two predicates part company here** — no line
        // until the instance says UNLOCKED, but the click goes out either way.
        assert!(!plain(ITEM_FLAG_LOOTABLE, 7).shows_open_line(0));
        assert!(plain(ITEM_FLAG_LOOTABLE, 7).shows_open_line(ITEM_DYNFLAG_UNLOCKED));
        assert!(
            plain(ITEM_FLAG_LOOTABLE, 7).opens_loot(),
            "the send ignores LockID entirely — the server owns the refusal"
        );
        // Gift wrap: the WRAPPER template unwraps only while the instance is still WRAPPED, and
        // that arm is its own dispatcher position, not a nested case of the loot arm.
        assert!(!plain(ITEM_FLAG_WRAPPER, 0).unwraps_gift(0));
        assert!(plain(ITEM_FLAG_WRAPPER, 0).unwraps_gift(ITEM_DYNFLAG_WRAPPED));
        assert!(
            !plain(ITEM_FLAG_WRAPPER, 0).opens_loot(),
            "WRAPPER is not LOOTABLE"
        );
        assert!(plain(ITEM_FLAG_WRAPPER, 0).shows_open_line(ITEM_DYNFLAG_WRAPPED));
        // A wrapped gift of a locked box: the gift arm ignores the lock gate on both sides.
        let gift_box = plain(ITEM_FLAG_WRAPPER | ITEM_FLAG_LOOTABLE, 7);
        assert!(gift_box.unwraps_gift(ITEM_DYNFLAG_WRAPPED));
        assert!(gift_box.shows_open_line(ITEM_DYNFLAG_WRAPPED));
    }

    #[test]
    fn negative_template_answer_is_cached() {
        let (cmds, rx) = commands();
        let mut items = Items::default();

        assert!(items.template(9999, 0, &cmds).is_none());
        let _ = rx.try_recv();
        items.insert_template(9999, None); // server: unknown entry
        assert!(items.template(9999, 0, &cmds).is_none());
        assert!(
            matches!(rx.try_recv(), Err(TryRecvError::Empty)),
            "no re-ask"
        );
    }

    #[test]
    fn objects_track_create_merge_destroy_and_session_clear() {
        let mut items = Items::default();

        items.insert_object(0x42, ObjectFields::default());
        assert!(items.object(0x42).is_some());
        assert!(items.merge_object(0x42, ObjectFields::default()));
        assert!(
            !items.merge_object(0x99, ObjectFields::default()),
            "a delta for an unseen guid is not tracked"
        );

        items.remove_object(0x42);
        assert!(items.object(0x42).is_none());

        // A disconnect clears instances + in-flight asks, keeps templates.
        let (cmds, rx) = commands();
        items.insert_object(0x43, ObjectFields::default());
        items.insert_template(117, Some(info("Tough Jerky")));
        assert!(items.template(118, 0, &cmds).is_none()); // leaves 118 in flight
        items.clear_session();
        assert!(items.object(0x43).is_none());
        assert!(items.template(117, 0, &cmds).is_some(), "templates survive");
        // 118's ask was dropped with the writer — it must re-ask now.
        let _ = rx.try_recv();
        assert!(items.template(118, 0, &cmds).is_none());
        assert!(matches!(
            rx.try_recv(),
            Ok(ClientCommand::ItemQuery { entry: 118, .. })
        ));
    }

    /// The id → row join (decisions 0915/0920): slot order is preserved, a `0` slot and an id with
    /// no `SpellItemEnchantment` name are both silently absent (never a placeholder line), and with
    /// no catalog at all nothing renders. Plus the carve's sign rule — **`abs(id)` names the row,
    /// the sign only travels** (wow-re §E3), which is why a negative id resolves at all.
    #[test]
    fn enchant_lines_join_named_ids_in_slot_order() {
        let cat = Enchants(benilla_formats::EnchantCatalog::from_rows(
            Default::default(),
            [
                (2564, "Agility +15".to_string()),
                (1900, "Crusader".to_string()),
            ]
            .into_iter()
            .collect(),
            Default::default(),
        ));
        let named = |v: Vec<benilla_ui::script::EnchantView>| -> Vec<String> {
            v.into_iter().map(|e| e.name).collect()
        };
        assert_eq!(
            named(enchant_lines(
                [(0, 2564, 0, None), (1, 0, 0, None), (4, 1900, 0, None)],
                Some(&cat)
            )),
            vec!["Agility +15".to_string(), "Crusader".to_string()]
        );
        // A NEGATIVE id resolves off `abs(id)` — the same row — and only carries its sign onward.
        let neg = enchant_lines([(0, -2564, 0, None)], Some(&cat));
        assert_eq!(neg.len(), 1);
        assert_eq!(neg[0].name, "Agility +15");
        assert!(neg[0].negative, "the sign travels to the colour rule");
        // Charges and a countdown ride the slot through untouched (the engine formats them).
        let temp = enchant_lines([(1, 1900, 5, Some(90_000))], Some(&cat));
        assert_eq!((temp[0].charges, temp[0].remaining_ms), (5, Some(90_000)));
        // An id the table doesn't name contributes nothing at all.
        assert!(enchant_lines([(0, 999_999, 0, None)], Some(&cat)).is_empty());
        // No DBC → no lines, the pre-0915 tooltip.
        assert!(enchant_lines([(0, 2564, 0, None)], None).is_empty());
    }
}
