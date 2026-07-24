//! Lock.dbc — the requirements a lockable GameObject (or item) carries (decision 0239). A
//! GameObject's template `lockId` (a type-specific slot of its query `data[]`) indexes this table;
//! each lock has up to 8 requirement **slots**. Interacting with a locked object casts a *known*
//! spell whose `SPELL_EFFECT_OPEN_LOCK` `EffectMiscValue` matches a **skill** slot's `LockType`
//! index (mining / herbalism / lockpicking), or consumes the **item** slot's key. A `lockId` of 0,
//! or a row whose every slot is empty, means "no lock" — the object opens by `CMSG_GAMEOBJ_USE`
//! instead of a cast (the split the RE pinned; see `wow-5875-re` cursor-system.md §8).
//!
//! Layout verified against build 5875 (mangos `LockEntry`, `DBCStructure.h`, and the RE's
//! `[lockRec+0x24]` = `Index[0]` = column 9): **33 fields** — `ID@0`, `Type[8]@1..8`,
//! `Index[8]@9..16`, `Skill[8]@17..24`, `Action[8]@25..32` (present in the file, unused here).

use std::collections::HashMap;

use crate::Chain;
use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, u32_at};

const LOCK: &str = "DBFilesClient\\Lock.dbc";
/// The file's column count (must equal the DBC header `field_count` — `benilla-dbc` enforces it).
const LOCK_FIELDS: usize = 33;
/// A lock has up to 8 requirement slots (`MAX_LOCK_CASE`).
pub const MAX_LOCK_SLOTS: usize = 8;

/// `Lock.dbc` `Type[i]` — a slot's key kind (mangos `LockKeyType`).
pub const LOCK_KEY_NONE: u32 = 0;
/// The slot is opened by holding a **key item**; `LockSlot::index` is that item's entry.
pub const LOCK_KEY_ITEM: u32 = 1;
/// The slot is opened by a **skill** (mining / herbalism / lockpicking); `LockSlot::index` is the
/// `LockType.dbc` index the opener spell's `EffectMiscValue` must match, `LockSlot::skill` the
/// required skill value.
pub const LOCK_KEY_SKILL: u32 = 2;

/// One of a lock's up-to-8 requirement slots. An all-zero slot is empty (unused).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LockSlot {
    /// `LOCK_KEY_NONE` / `LOCK_KEY_ITEM` / `LOCK_KEY_SKILL`.
    pub key_type: u32,
    /// Key item entry (ITEM) or `LockType` index (SKILL); `0` when empty.
    pub index: u32,
    /// Required skill value (SKILL slots); `0` otherwise.
    pub skill: u32,
}

/// `lockId → its 8 requirement slots`, from Lock.dbc.
pub struct LockCatalog {
    locks: HashMap<u32, [LockSlot; MAX_LOCK_SLOTS]>,
}

impl LockCatalog {
    /// Build a catalog from explicit rows — tests and tools; the game path is
    /// [`load_lock_catalog`].
    pub fn from_rows(rows: impl IntoIterator<Item = (u32, [LockSlot; MAX_LOCK_SLOTS])>) -> Self {
        Self {
            locks: rows.into_iter().collect(),
        }
    }

    /// The 8 requirement slots for a `lockId`, or `None` if the id isn't in the table (treat as no
    /// lock). A returned row may still be all-empty — [`LockCatalog::is_locked`] is the "must cast"
    /// test.
    pub fn slots(&self, lock_id: u32) -> Option<&[LockSlot; MAX_LOCK_SLOTS]> {
        self.locks.get(&lock_id)
    }

    /// Whether a `lockId` names a real lock (present, with at least one non-empty slot) — i.e. the
    /// object opens by a cast, not `CMSG_GAMEOBJ_USE`. A `0` id or an absent/all-empty row is "no
    /// lock".
    pub fn is_locked(&self, lock_id: u32) -> bool {
        lock_id != 0
            && self
                .locks
                .get(&lock_id)
                .is_some_and(|s| s.iter().any(|slot| slot.key_type != LOCK_KEY_NONE))
    }

    pub fn len(&self) -> usize {
        self.locks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.locks.is_empty()
    }
}

fn schema() -> Schema {
    let mut s = Schema::new("Lock");
    for i in 0..LOCK_FIELDS {
        s.add_field(SchemaField::new(format!("F{i}"), FieldType::UInt32));
    }
    s
}

/// Load Lock.dbc from the patch chain into a [`LockCatalog`].
pub fn load_lock_catalog(chain: &mut Chain) -> Result<LockCatalog> {
    let bytes = chain
        .read_file(LOCK)
        .with_context(|| format!("reading {LOCK}"))?;
    let rs = parse(&bytes, schema(), "Lock.dbc")?;
    let mut locks = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        let mut slots = [LockSlot::default(); MAX_LOCK_SLOTS];
        for (i, slot) in slots.iter_mut().enumerate() {
            slot.key_type = u32_at(r, 1 + i).unwrap_or(0);
            slot.index = u32_at(r, 9 + i).unwrap_or(0);
            slot.skill = u32_at(r, 17 + i).unwrap_or(0);
        }
        locks.insert(id, slots);
    }
    Ok(LockCatalog { locks })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Type/Index/Skill column offsets on the real build-5875 `Lock.dbc` — a column slip fails
    /// loudly. Anchors: Copper Vein (lockId 38, a Mining node) and Silverleaf (lockId 29, an
    /// Herbalism node), both skill 1 — the lowest of their profession. Skips without client data.
    #[test]
    fn real_lock_catalog_reads_skill_slots() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_lock_catalog(&mut chain).expect("load Lock.dbc");
        assert!(!cat.is_empty(), "Lock.dbc parsed empty");

        // Copper Vein (gameobject_template 1731, chest.data0 = lockId 38): a MINING skill lock. The
        // real row is `{Type[0]=2 SKILL, Index[0]=3 (Mining LockType), Skill[0]=0}` — `Type[0]` at
        // col 1, `Index[0]` at col 9, `Skill[0]` at col 17; a column slip lands elsewhere. (Lock.dbc
        // stores 0 in `Skill` for gathering nodes — the profession *spell* is the gate, not a value
        // here; the server enforces the node's grey level.)
        let vein = cat.slots(38).expect("lockId 38 (Copper Vein)");
        assert_eq!(
            vein[0].key_type, LOCK_KEY_SKILL,
            "copper vein is a skill lock"
        );
        assert_eq!(vein[0].index, 3, "Mining is LockType index 3");
        assert_eq!(
            vein[0].skill, 0,
            "gathering nodes carry no Skill value in Lock.dbc"
        );
        assert!(
            vein[1..].iter().all(|s| s.key_type == LOCK_KEY_NONE),
            "one slot only"
        );
        assert!(
            cat.is_locked(38),
            "a mining vein is a real lock (must cast, not USE)"
        );

        // Silverleaf (gameobject_template 1617, lockId 29): an HERBALISM skill lock — LockType index
        // 2, distinct from mining's 3.
        let herb = cat.slots(29).expect("lockId 29 (Silverleaf)");
        assert_eq!(
            herb[0].key_type, LOCK_KEY_SKILL,
            "silverleaf is a skill lock"
        );
        assert_eq!(herb[0].index, 2, "Herbalism is LockType index 2");
        assert_ne!(herb[0].index, vein[0].index, "herbalism ≠ mining LockType");

        // A lockId of 0 is never a lock (opens by USE, not a cast).
        assert!(!cat.is_locked(0));
    }

    /// The keyless-chest path: lockId 43 (Sunken Chest, Storage Chest, Worn Wooden Chest, …) is a
    /// single SKILL slot naming LockType **13** — the one spell 6478 "Opening" opens, and "Opening"
    /// is a default-known player spell, so any character can loot these. Confirms the routing matches
    /// keyless chests (not just gathering nodes) to a spell the player already has. Skips without data.
    #[test]
    fn real_lock_catalog_reads_keyless_chest() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_lock_catalog(&mut chain).expect("load Lock.dbc");

        let chest = cat.slots(43).expect("lockId 43 (simple chest)");
        assert!(
            cat.is_locked(43),
            "even a keyless chest is a real lock (cast, not USE)"
        );
        // The requirement sits in slot 1 here (slot 0 empty) — the routing must scan all 8 slots,
        // not just slot 0. LockType 13 = the "Opening" the default spell 6478 provides.
        assert_eq!(chest[1].key_type, LOCK_KEY_SKILL);
        assert_eq!(
            chest[1].index, 13,
            "the keyless-chest LockType spell 6478 opens"
        );
    }
}
