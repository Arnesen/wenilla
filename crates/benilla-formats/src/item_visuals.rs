//! `ItemVisuals.dbc` + `ItemVisualEffects.dbc` + `SpellItemEnchantment`'s visual column — the
//! **item / enchant glow chain** (decision 0805): the permanent weapon glows and the shaman/oil
//! enchant visuals, as up to five `Spells\Enchantments\*.mdx` effect models per item.
//!
//! ## The chain
//!
//! Two sources name an **ItemVisuals id**, and both land in the same consumer:
//!
//! - the item's own **intrinsic** visual — `ItemDisplayInfo` column 22
//!   ([`crate::ItemDisplay::item_visual`], record `+0x58`), and
//! - the **enchant's** visual — `SpellItemEnchantment` field 22 (record `+0x58`), read off the
//!   item's enchant slots.
//!
//! An ItemVisuals row is `{ id, effect[5] }`; each effect is an `ItemVisualEffects` id whose one
//! payload column is the effect model's `.mdx` path. So one id = **up to five glow models**, one
//! per slot, and the slot index *is* the M2 attachment id (0..4) they hang from on the item's own
//! model (`0x479700`'s `0x712f70(glow, item, attachId = loop index)`).
//!
//! ## Layout — VERIFIED against build 5875 (both tables dumped whole)
//!
//! | table | records | fields | record size | columns |
//! |---|---|---|---|---|
//! | `ItemVisuals` | 34 | 6 | 24 | id + 5 `ItemVisualEffects` ids |
//! | `ItemVisualEffects` | 35 | 2 | 8 | id + model `.mdx` path (string) |
//! | `SpellItemEnchantment` | 1460 | 24 | 96 | id · effect[3] · pointsMin[3] · pointsMax[3] · arg[3] · name[8]+mask · **ItemVisual (22)** · flags |
//!
//! The wow-re §5 note `object-layer/scratch/item-visual-enchant.md` byte-pins the same shapes from
//! the loaders' own `cmp fieldCount/recSize` asserts (`0x548760`/`0x548530`/`0x54f6e0`).
//!
//! ## The skip rules are the client's, applied here at load
//!
//! `0x479700` bounds-checks **twice**, signed, and both checks matter on the shipped data:
//!
//! - the ItemVisuals id: `jl` on negative, `jg` on `> maxId`, then a null-row test. **Five of the
//!   365 visual-carrying displays carry `-1`** — they resolve to nothing.
//! - each of the 5 effect ids, the same way — and **ItemVisuals row 28 carries two out-of-range
//!   garbage dwords** (`90148992`, `455344256`) in slots 0 and 3, which the reference skips and so
//!   do we (id 28 renders only its slot-4 `Sparkle_A`).
//! - an **empty** path string (`cmpb $0,(%eax)`) is skipped before the load. Effect id 61 is
//!   `Spells\Enchantments\` — a directory, not empty, so the reference *tries* it and the load
//!   fails; no ItemVisuals row references it, so this is theory either way.
//!
//! Every one of those becomes a `None` slot in [`ItemVisualCatalog`], so a consumer sees only real
//! model paths and can never spawn a glow the reference wouldn't.

use std::collections::HashMap;

use crate::Chain;
use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, str_at, u32_at};

const ITEM_VISUALS: &str = "DBFilesClient\\ItemVisuals.dbc";
const ITEM_VISUAL_EFFECTS: &str = "DBFilesClient\\ItemVisualEffects.dbc";
const SPELL_ITEM_ENCHANTMENT: &str = "DBFilesClient\\SpellItemEnchantment.dbc";

/// The five effect slots an ItemVisuals row carries — and, one-for-one, the M2 attachment ids
/// `0..4` on the item model each one hangs from.
pub const ITEM_VISUAL_SLOTS: usize = 5;

/// `ItemVisuals.dbc` joined with `ItemVisualEffects.dbc`: an ItemVisuals id → its five glow-model
/// paths (raw `.mdx` references, as the DBC stores them — the app's `m2_url` owns the `.m2` swap),
/// `None` where the slot is empty, out of range, or names an empty path.
pub struct ItemVisualCatalog {
    visuals: HashMap<u32, [Option<String>; ITEM_VISUAL_SLOTS]>,
}

impl ItemVisualCatalog {
    /// The five glow-model slots for an ItemVisuals id, or `None` when the id names no row.
    ///
    /// Takes the id **signed**, because that is how the client reads it: `0x479700` tests `jl`
    /// before its `maxId` compare, so `-1` (five shipped `ItemDisplayInfo` rows) and `0` name
    /// nothing. `> 0` with no row is equally nothing.
    pub fn effects(&self, visual_id: i32) -> Option<&[Option<String>; ITEM_VISUAL_SLOTS]> {
        (visual_id > 0).then(|| self.visuals.get(&(visual_id as u32)))?
    }

    /// Build from an explicit map — tests and synthetic fixtures.
    pub fn from_visuals(visuals: HashMap<u32, [Option<String>; ITEM_VISUAL_SLOTS]>) -> Self {
        ItemVisualCatalog { visuals }
    }

    pub fn len(&self) -> usize {
        self.visuals.len()
    }

    pub fn is_empty(&self) -> bool {
        self.visuals.is_empty()
    }
}

/// `SpellItemEnchantment.dbc`'s two consumer columns, from one load of the one table: the
/// **visual** (field 22) an enchant glows with, and the **name** (field 13, the enUS slot of the
/// 1.12 localized-string block) the item tooltip prints for it. Two lanes read this table — the
/// weapon-glow chain (decision 0805) and the tooltip's enchant line (decision 0915) — and one
/// adapter serves both: two loaders over one DBC is how a schema drifts.
///
/// Sparse on both axes: 102 of the 1460 rows carry a visual, and a row without a name simply has
/// none. The rest of the table (effects, points, args) belongs to whoever needs it, not here.
pub struct EnchantCatalog {
    visuals: HashMap<u32, i32>,
    names: HashMap<u32, String>,
}

impl EnchantCatalog {
    /// The ItemVisuals id an enchant glows with, or `None` for an enchant with no visual. The
    /// value is signed for the same reason as [`ItemVisualCatalog::effects`] — one shipped row
    /// carries `-1`.
    pub fn visual(&self, enchant_id: u32) -> Option<i32> {
        self.visuals.get(&enchant_id).copied()
    }

    /// The enchant's display name as the table stores it — `"Agility +15"`, `"Crusader"`,
    /// `"Stamina +7"`. `None` for an unknown id or a row with an empty name string.
    pub fn name(&self, enchant_id: u32) -> Option<&str> {
        self.names.get(&enchant_id).map(String::as_str)
    }

    /// Build from explicit maps — tests and synthetic fixtures.
    pub fn from_rows(visuals: HashMap<u32, i32>, names: HashMap<u32, String>) -> Self {
        EnchantCatalog { visuals, names }
    }

    /// Iterate `(enchant id, ItemVisuals id)` for the rows that carry one (order unspecified) —
    /// the cross-table join checks read it.
    pub fn iter_visuals(&self) -> impl Iterator<Item = (u32, i32)> + '_ {
        self.visuals.iter().map(|(k, v)| (*k, *v))
    }

    /// How many enchants carry a glow — the glow lane's startup census.
    pub fn visual_count(&self) -> usize {
        self.visuals.len()
    }

    /// How many enchants carry a printable name — the tooltip lane's.
    pub fn name_count(&self) -> usize {
        self.names.len()
    }
}

/// `ItemVisuals.dbc` — 6 fields / 24-byte records in build 5875.
pub(crate) fn item_visuals_schema() -> Schema {
    let mut s = Schema::new("ItemVisuals");
    s.add_field(SchemaField::new("ID", FieldType::UInt32));
    for i in 0..ITEM_VISUAL_SLOTS {
        s.add_field(SchemaField::new(
            format!("Effect{i}"),
            FieldType::UInt32, // signed in use — see the module doc's skip rules
        ));
    }
    s
}

/// `ItemVisualEffects.dbc` — 2 fields / 8-byte records in build 5875.
pub(crate) fn item_visual_effects_schema() -> Schema {
    let mut s = Schema::new("ItemVisualEffects");
    s.add_field(SchemaField::new("ID", FieldType::UInt32));
    s.add_field(SchemaField::new("Model", FieldType::String));
    s
}

/// `SpellItemEnchantment.dbc` — 24 fields / 96-byte records in build 5875. Fields 13 (`Name0`)
/// and 22 (`ItemVisual`) are read; the rest are typed to keep the field-count check exact. The
/// eight `Name_Lang` slots + their mask are the 1.12 localized-string block (only `enUS`, the
/// first, is filled).
pub(crate) fn spell_item_enchantment_schema() -> Schema {
    let mut s = Schema::new("SpellItemEnchantment");
    s.add_field(SchemaField::new("ID", FieldType::UInt32));
    for group in ["Effect", "EffectPointsMin", "EffectPointsMax", "EffectArg"] {
        for i in 0..3 {
            s.add_field(SchemaField::new(format!("{group}{i}"), FieldType::UInt32));
        }
    }
    for i in 0..8 {
        s.add_field(SchemaField::new(format!("Name{i}"), FieldType::String));
    }
    s.add_field(SchemaField::new("NameFlags", FieldType::UInt32));
    s.add_field(SchemaField::new("ItemVisual", FieldType::UInt32));
    s.add_field(SchemaField::new("Flags", FieldType::UInt32));
    s
}

/// Load `ItemVisuals.dbc` + `ItemVisualEffects.dbc` off the patch chain and join them into an
/// [`ItemVisualCatalog`] — the client's per-slot skip rules (module doc) applied at load, so a
/// resolved slot is always a real model path.
pub fn load_item_visual_catalog(chain: &mut Chain) -> Result<ItemVisualCatalog> {
    let bytes = chain
        .read_file(ITEM_VISUAL_EFFECTS)
        .with_context(|| format!("reading {ITEM_VISUAL_EFFECTS}"))?;
    let rs = parse(&bytes, item_visual_effects_schema(), "ItemVisualEffects")?;
    // `str_at` already drops the empty string — the reference's own `cmpb $0,(%eax)` skip.
    let mut effects: HashMap<u32, String> = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let (Some(id), Some(model)) = (u32_at(r, 0), str_at(&rs, r, 1)) else {
            continue;
        };
        effects.insert(id, model);
    }

    let bytes = chain
        .read_file(ITEM_VISUALS)
        .with_context(|| format!("reading {ITEM_VISUALS}"))?;
    let rs = parse(&bytes, item_visuals_schema(), "ItemVisuals")?;
    let mut visuals = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        let slots = std::array::from_fn(|i| {
            let raw = u32_at(r, 1 + i).unwrap_or(0) as i32;
            (raw > 0)
                .then(|| effects.get(&(raw as u32)).cloned())
                .flatten()
        });
        visuals.insert(id, slots);
    }
    Ok(ItemVisualCatalog { visuals })
}

/// Load `SpellItemEnchantment.dbc`'s two consumer columns off the patch chain — one parse, both
/// lanes (see [`EnchantCatalog`]).
pub fn load_enchant_catalog(chain: &mut Chain) -> Result<EnchantCatalog> {
    let bytes = chain
        .read_file(SPELL_ITEM_ENCHANTMENT)
        .with_context(|| format!("reading {SPELL_ITEM_ENCHANTMENT}"))?;
    let rs = parse(
        &bytes,
        spell_item_enchantment_schema(),
        "SpellItemEnchantment",
    )?;
    let mut visuals = HashMap::new();
    let mut names = HashMap::new();
    for r in rs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        let visual = u32_at(r, 22).unwrap_or(0) as i32;
        if visual != 0 {
            visuals.insert(id, visual);
        }
        // `str_at` drops the empty string, so a nameless row simply never lands.
        if let Some(name) = str_at(&rs, r, 13) {
            names.insert(id, name);
        }
    }
    Ok(EnchantCatalog { visuals, names })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vanilla_data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
    }

    /// The two glow tables as they actually ship, including both traps: row **28**'s two
    /// out-of-range garbage slots and the reference's per-slot skip.
    #[test]
    fn real_item_visuals_join_their_effect_models() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_item_visual_catalog(&mut chain).expect("load ItemVisuals");
        assert_eq!(cat.len(), 34, "34 ItemVisuals rows in build 5875");

        // The common shape: one effect repeated across all five attach slots.
        let all_blue = ["Spells\\Enchantments\\BlueGlow_Med.mdx"; ITEM_VISUAL_SLOTS];
        let got = cat.effects(2).expect("visual 2");
        assert_eq!(
            got.each_ref().map(|s| s.as_deref().unwrap_or("")),
            all_blue,
            "visual 2 glows on every slot"
        );

        // A single-slot shape — slot 3 only.
        let one = cat.effects(1).expect("visual 1");
        assert_eq!(
            one[3].as_deref(),
            Some("Spells\\Enchantments\\SkullBalls.mdx")
        );
        assert!(
            [0, 1, 2, 4].iter().all(|&i| one[i].is_none()),
            "visual 1 authors only slot 3"
        );

        // A mixed row: slot 0 differs from the other four.
        let rune = cat.effects(30).expect("visual 30");
        assert_eq!(
            rune[0].as_deref(),
            Some("Spells\\Enchantments\\Rune_Intellect.mdx")
        );
        assert_eq!(
            rune[4].as_deref(),
            Some("Spells\\Enchantments\\YellowGlow_Low.mdx")
        );

        // **The garbage row.** Slots 0 and 3 hold 90148992 / 455344256 — far past the effect
        // table's maxId (152) — and the reference's `jg` skips them.
        let junk = cat.effects(28).expect("visual 28");
        assert_eq!(
            junk.each_ref().map(|s| s.is_some()),
            [false, false, false, false, true],
            "only the in-range slot-4 effect survives row 28"
        );
        assert_eq!(
            junk[4].as_deref(),
            Some("Spells\\Enchantments\\Sparkle_A.mdx")
        );

        // The id gate: 0 and the shipped -1 name nothing.
        assert!(cat.effects(0).is_none());
        assert!(cat.effects(-1).is_none());
        assert!(cat.effects(9999).is_none(), "past maxId");
    }

    /// The display half of the join, on real data: 365 of the 29 604 displays carry a visual, five
    /// of them the unresolvable `-1`, and every other one resolves to at least one glow model.
    #[test]
    fn real_displays_carrying_a_visual_all_resolve() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let visuals = load_item_visual_catalog(&mut chain).expect("load ItemVisuals");
        let displays = crate::load_item_display_catalog(&mut chain).expect("load ItemDisplayInfo");

        let (mut carried, mut minus_one, mut resolved, mut models) = (0, 0, 0, 0);
        for d in displays.iter() {
            if d.item_visual == 0 {
                continue;
            }
            carried += 1;
            if d.item_visual == -1 {
                minus_one += 1;
            }
            match visuals.effects(d.item_visual) {
                Some(slots) => {
                    resolved += 1;
                    models += slots.iter().flatten().count();
                }
                None => assert_eq!(
                    d.item_visual, -1,
                    "the only unresolvable visual ids on the shipped table are -1"
                ),
            }
        }
        assert_eq!(carried, 365, "displays carrying a nonzero ItemVisuals id");
        assert_eq!(minus_one, 5, "…of which five are the skipped -1");
        assert_eq!(resolved, 360);
        assert_eq!(
            models, 1588,
            "glow-model instances the shipped displays add up to"
        );
    }

    /// The enchant half: `SpellItemEnchantment` field 22 lands in the ItemVisuals id space on
    /// every shipped row but one (`-1`) — the independent corroboration that column 22 (record
    /// `+0x58`) is the visual, and the three shaman weapon buffs to name it.
    #[test]
    fn real_enchant_visuals_resolve() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let visuals = load_item_visual_catalog(&mut chain).expect("load ItemVisuals");
        let enchants = load_enchant_catalog(&mut chain).expect("load SpellItemEnchantment");
        assert_eq!(enchants.visual_count(), 102, "enchants carrying a visual");

        let resolved = enchants
            .iter_visuals()
            .filter(|(_, v)| visuals.effects(*v).is_some())
            .count();
        assert_eq!(resolved, 101, "…all but the one -1 row resolve to a row");

        // Rockbiter 3 (enchant 1) → visual 61 → the slot-3 rock glow.
        let rockbiter = visuals
            .effects(enchants.visual(1).expect("enchant 1 has a visual"))
            .expect("visual 61");
        assert_eq!(
            rockbiter[3].as_deref(),
            Some("Spells\\Enchantments\\Shaman_Rock.mdx")
        );
        // A sharpening stone (enchant 13) → visual 28 → the garbage row's surviving sparkle.
        let sharpened = visuals
            .effects(enchants.visual(13).expect("enchant 13 has a visual"))
            .expect("visual 28");
        assert_eq!(
            sharpened[4].as_deref(),
            Some("Spells\\Enchantments\\Sparkle_A.mdx")
        );
        // A plain +stat enchant carries none (241 "Weapon Damage +2", 929 "Stamina +7").
        assert_eq!(enchants.visual(241), None);
        assert_eq!(enchants.visual(929), None);
    }

    /// The **name** column (field 13), on real data — the string the tooltip's enchant line
    /// prints (decision 0915). Pinned on the case that opened the lane plus one of each other
    /// shape, and on the two properties the consumer rests on: the name is stored in the table's
    /// own word order (`"Agility +15"`, NOT a reformat), and it is independent of the visual
    /// column — a glowing enchant and a plain +stat one both have one.
    #[test]
    fn real_enchant_names_read_off_the_table() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let enchants = load_enchant_catalog(&mut chain).expect("load SpellItemEnchantment");

        // 2564 = the permanent weapon enchant on the director's Hatchet of Sundered Bone: a name
        // AND a visual (125 → GreenGlow_Low), the two columns joined on one row.
        assert_eq!(enchants.name(2564), Some("Agility +15"));
        assert_eq!(enchants.visual(2564), Some(125));
        // Named, no glow — the +stat family.
        assert_eq!(enchants.name(241), Some("Weapon Damage +2"));
        assert_eq!(enchants.name(929), Some("Stamina +7"));
        // Glow, and a name that is a proper noun rather than a stat phrase.
        assert_eq!(enchants.name(1900), Some("Crusader"));
        // An id past the table names nothing at all.
        assert_eq!(enchants.name(999_999), None);
        assert!(
            enchants.name_count() > enchants.visual_count(),
            "far more enchants print a name than carry a glow"
        );
    }
}
