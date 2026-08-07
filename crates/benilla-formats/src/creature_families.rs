//! `CreatureFamily.dbc` + `ItemPetFood.dbc` — the pet's **family word** and the **diet** it implies
//! (decision 1062): `UnitCreatureFamily`'s "Imp"/"Wolf"/"Cat", and `GetPetFoodTypes`'s
//! "Meat, Fish".
//!
//! Both layouts below were **dumped from this install's own shipped files** rather than taken from
//! memory or a wiki — the WDBC header first, then every column of every row, then a census of what
//! each column actually holds. What that dump says:
//!
//! **`CreatureFamily.dbc` — 18 fields, `record_size` 0x48, 23 rows, string block 933 bytes.**
//!
//! | field | byte | holds |
//! |---|---|---|
//! | 0 | 0x00 | `ID` — 1..28 with gaps (10, 13, 14, 18, 22 are absent) |
//! | 1 | 0x04 | `minScale`, a **float** (0.4 … 1.0) |
//! | 2 | 0x08 | `minScaleLevel` — 1 for every hunter family, 0 for the warlock ones and row 28 |
//! | 3 | 0x0c | `maxScale`, a **float** (0.6 … 1.1) |
//! | 4 | 0x10 | `maxScaleLevel` — 60, or 0 on the same rows |
//! | 5 | 0x14 | `skillLine[0]` — a `SkillLine.dbc` id (188…758), distinct on every row |
//! | 6 | 0x18 | `skillLine[1]` — 270 on every tameable family, 0 on the rest |
//! | 7 | 0x1c | **`petFoodMask`** — the bitfield this module's other half indexes |
//! | 8..15 | 0x20 | the localized `Name` block; **enUS is field 8**, the other seven are all zero |
//! | 16 | 0x40 | the name block's locale-present flags (0x7EFFFE or 0x3F007E) — **not** zero |
//! | 17 | 0x44 | `iconFile` — `Interface\Icons\Ability_Hunter_Pet_*` (a string, not a dword) |
//!
//! That confirms the `0x48` record and the 23 rows [`crate::pet_stats`]'s header already recorded,
//! and **corrects its "every dword from `0x28` up is zero"**: fields 10..15 (`0x28`–`0x3c`) are
//! indeed all zero — which is what that argument actually rested on, since the reads it was ruling
//! out were at `+0x28`/`+0x30`/`+0x3c`/`+0x48` — but `0x40` and `0x44` are not. The conclusion
//! stands (a `rec+0x48` read still runs off the end of a 0x48-byte record); the sentence was one
//! column too wide.
//!
//! **`ItemPetFood.dbc` — 10 fields, `record_size` 0x28, 8 rows, string block 55 bytes**: `ID`@0,
//! the localized `Name` block@1..8 (**enUS is field 1**, the rest zero), name flags@9. The eight
//! rows in id order are exactly `Meat / Fish / Cheese / Bread / Fungus / Fruit / Raw Meat /
//! Raw Fish`.
//!
//! **The mask's bit → row mapping is `bit b` → `ItemPetFood` row `b + 1`** (see
//! [`PetFoodNames::for_mask`]) — DATA-derived twice over. The highest bit any shipped row sets is
//! **7** (Turtle, mask 178) and the table has exactly 8 rows, so a 0-based mapping would leave row
//! 8 permanently unreachable and Wolf (mask 1) with an empty diet. And vmangos, reading the same
//! file, tests exactly `1 << (item->FoodType - 1)` against this column (`Objects/Pet.cpp:1503-1504`,
//! `Database/DBCStructure.h:251` — its `CreatureFamilyfmt` `"nfifiiiissssssssxx"` is this table's
//! layout above, field for field). Every resulting diet matches vanilla's documented ones: Wolf =
//! Meat, Cat = Meat+Fish, Bear/Boar = all six, Gorilla = Fungus+Fruit, Wind Serpent = Fish+Cheese+
//! Bread.
//!
//! **All of the above is now also byte-VERIFIED** (wow-re, 2026-08-06 — `GetPetFoodTypes 0x4bea10`
//! and `UnitCreatureFamily 0x51a310`), and the two dumps agree line for line with nothing left
//! over: the mask is column 7, the bit map is `1 << (recordID - 1)`, the family name is column
//! `8 + locale`, the food name is column `1 + locale`, the id space really does have null rows at
//! 10/13/14/18/22, and the binary's own positive controls (Wolf → Meat; Bear and Boar → all six;
//! Turtle → Fish/Fungus/Fruit/Raw Fish) are the rows asserted below. The one claim that had been
//! flagged INFERRED here — that the walk is **low bit first** — is settled the same way: the client
//! pushes in **record order**, which is what ascending bits produce.
//!
//! What this module deliberately does **not** model is `GetPetFoodTypes`' own `0x6116e0` gate
//! (owner-is-me + the local player is a Hunter). That is a live-state question, not a table one, so
//! it lives at the feed (`benilla_app::ui_pet_stats`) — and it is why a charmed boar under a priest
//! answers an empty diet despite a mask of 63.

use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::chain::Chain;
use crate::dbc::{parse, str_at, u32_at};

const CREATURE_FAMILY: &str = "DBFilesClient\\CreatureFamily.dbc";
const ITEM_PET_FOOD: &str = "DBFilesClient\\ItemPetFood.dbc";

/// `CreatureFamily.dbc`'s column count (the DBC header's `field_count`; `benilla-dbc` enforces it,
/// so a wrong number fails loudly instead of misaligning every column).
const FAMILY_FIELDS: usize = 18;
/// `ItemPetFood.dbc`'s column count.
const FOOD_FIELDS: usize = 10;

/// `CreatureFamily.dbc`'s pet-food mask — byte `0x1c` = field 7.
const FAMILY_FOOD_MASK_FIELD: usize = 0x1c / 4;
/// `CreatureFamily.dbc`'s enUS `Name` — byte `0x20` = field 8.
const FAMILY_NAME_FIELD: usize = 0x20 / 4;
/// `CreatureFamily.dbc`'s `iconFile` — byte `0x44` = field 17. The last column, past the name
/// block's locale flags; not read by anything yet, typed as a string so the schema is honest.
const FAMILY_ICON_FIELD: usize = 0x44 / 4;
/// `ItemPetFood.dbc`'s enUS `Name` — field 1, the same shape every other localized table here uses.
const FOOD_NAME_FIELD: usize = 1;

/// The number of `ItemPetFood` rows a mask can name. Not a guess at the file's size: it is the
/// width the *mask* can address, and the shipped table happens to fill it exactly (bit 7 → row 8,
/// Turtle's "Raw Fish"). Bits above this are ignored rather than fabricating rows.
const MAX_FOOD_BITS: u32 = 8;

/// One `CreatureFamily.dbc` row, reduced to its two live columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatureFamily {
    /// The localized family word — "Wolf", "Imp", "Wind Serpent". What `UnitCreatureFamily` pushes.
    pub name: String,
    /// The pet-food bitfield (field 7). **`0` for every warlock minion** — Imp, Voidwalker,
    /// Succubus, Felhunter, Doomguard all ship a zero mask, and so does row 28 ("Remote Control") —
    /// which is why a diet list can be legitimately empty without anything having gone wrong.
    pub pet_food_mask: u32,
}

/// `CreatureFamily.dbc`, by id.
#[derive(Debug, Default, Clone)]
pub struct CreatureFamilies(HashMap<u32, CreatureFamily>);

impl CreatureFamilies {
    /// The row for a family id, or `None`.
    ///
    /// **Family `0` is `None` and that is the common case, not an error**: the file's ids start at
    /// 1, and every creature that is neither a tameable beast nor a warlock minion carries `0` in
    /// the wire's `pet_family` slot. `UnitCreatureFamily`'s nil is this `None`.
    pub fn get(&self, id: u32) -> Option<&CreatureFamily> {
        self.0.get(&id)
    }

    /// The localized family word for an id — [`Self::get`]'s name half.
    pub fn name(&self, id: u32) -> Option<&str> {
        self.get(id).map(|f| f.name.as_str())
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// `ItemPetFood.dbc`: food-type id → its localized name.
#[derive(Debug, Default, Clone)]
pub struct PetFoodNames(HashMap<u32, String>);

impl PetFoodNames {
    /// The name for a food-type id (1-based, as the file numbers them).
    pub fn name(&self, id: u32) -> Option<&str> {
        self.0.get(&id).map(String::as_str)
    }

    /// Expand a [`CreatureFamily::pet_food_mask`] into the localized names it selects — **bit `b`
    /// selects row `b + 1`**, ascending, which is the client's own **record order**
    /// (`0x4bea10`'s `1 << (recordID - 1)`; the module header carries the derivation).
    ///
    /// An empty result is a real answer, not a failure: a warlock minion's family ships mask `0`.
    /// `GetPetFoodTypes` then returns nothing at all, which is exactly what the reference's own
    /// `BuildListString()` turns into nil — and the diet icon that would show it is hidden for a
    /// minion anyway (`HasPetUI`'s second return, decision 1005).
    ///
    /// **This is the table half only.** The binding's live gate — owner-is-me and the local player
    /// is a Hunter (`0x6116e0`) — is the feed's to apply; a charmed boar has a mask of 63 and still
    /// answers nothing under a non-hunter.
    pub fn for_mask(&self, mask: u32) -> Vec<&str> {
        (0..MAX_FOOD_BITS)
            .filter(|b| mask & (1 << b) != 0)
            .filter_map(|b| self.name(b + 1))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

fn family_schema() -> Schema {
    let mut s = Schema::new("CreatureFamily");
    for i in 0..FAMILY_FIELDS {
        let ty = match i {
            // minScale / maxScale — the only two floats in the record.
            1 | 3 => FieldType::Float32,
            FAMILY_NAME_FIELD | FAMILY_ICON_FIELD => FieldType::String,
            // The rest of the locale block reads as dwords: every one is 0 in the shipped file, and
            // typing them as strings would make the parser chase seven zero offsets per row.
            _ => FieldType::UInt32,
        };
        s.add_field(SchemaField::new(format!("F{i}"), ty));
    }
    s
}

fn food_schema() -> Schema {
    let mut s = Schema::new("ItemPetFood");
    for i in 0..FOOD_FIELDS {
        let ty = if i == FOOD_NAME_FIELD {
            FieldType::String
        } else {
            FieldType::UInt32
        };
        s.add_field(SchemaField::new(format!("F{i}"), ty));
    }
    s
}

/// Load `CreatureFamily.dbc` from the patch chain.
pub fn load_creature_families(chain: &mut Chain) -> Result<CreatureFamilies> {
    let bytes = chain
        .read_file(CREATURE_FAMILY)
        .with_context(|| format!("reading {CREATURE_FAMILY}"))?;
    let rs = parse(&bytes, family_schema(), "CreatureFamily.dbc")?;
    let mut by_id = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        // A row with no enUS name is dropped rather than stored empty: the binding's contract is
        // "a word or nil", and an empty string would render as a trailing space on the level line.
        let Some(name) = str_at(&rs, r, FAMILY_NAME_FIELD).filter(|n| !n.is_empty()) else {
            continue;
        };
        by_id.insert(
            id,
            CreatureFamily {
                name,
                pet_food_mask: u32_at(r, FAMILY_FOOD_MASK_FIELD).unwrap_or(0),
            },
        );
    }
    Ok(CreatureFamilies(by_id))
}

/// Load `ItemPetFood.dbc` from the patch chain.
pub fn load_pet_food_names(chain: &mut Chain) -> Result<PetFoodNames> {
    let bytes = chain
        .read_file(ITEM_PET_FOOD)
        .with_context(|| format!("reading {ITEM_PET_FOOD}"))?;
    let rs = parse(&bytes, food_schema(), "ItemPetFood.dbc")?;
    let mut by_id = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        if let Some(name) = str_at(&rs, r, FOOD_NAME_FIELD) {
            by_id.insert(id, name);
        }
    }
    Ok(PetFoodNames(by_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain() -> Option<Chain> {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return None;
        }
        Some(crate::open_chain(&data).expect("open chain"))
    }

    /// The real 5875 `CreatureFamily.dbc`, byte-anchored: 23 rows, the ids with their gaps, and
    /// the two columns we read.
    ///
    /// **The name column is the trap.** Field 8 sits immediately after `petFoodMask` (7) and is
    /// followed by seven zero locale slots — read one column early and every name is a *number*;
    /// read one late and every name is empty. Both fail silently at runtime (a blank level line),
    /// so the names are asserted verbatim, not merely for presence.
    #[test]
    fn the_real_creature_families_name_every_pet() {
        let Some(mut chain) = chain() else { return };
        let f = load_creature_families(&mut chain).expect("load CreatureFamily.dbc");
        assert_eq!(f.len(), 23, "5875 ships 23 creature families");

        // The hunter families, at both ends of the id range.
        assert_eq!(f.name(1), Some("Wolf"));
        assert_eq!(f.name(2), Some("Cat"));
        assert_eq!(f.name(7), Some("Carrion Bird"));
        assert_eq!(f.name(27), Some("Wind Serpent"));
        // …the warlock minions, which is what the pet page's level line reads most often.
        assert_eq!(f.name(15), Some("Felhunter"));
        assert_eq!(f.name(16), Some("Voidwalker"));
        assert_eq!(f.name(17), Some("Succubus"));
        assert_eq!(f.name(19), Some("Doomguard"));
        assert_eq!(f.name(23), Some("Imp"));

        // The id column really has gaps — 10/13/14/18/22 are absent from the shipped file, so a
        // dense 1..=23 assumption would silently shift half the table.
        for missing in [10, 13, 14, 18, 22] {
            assert_eq!(f.name(missing), None, "id {missing} is not in the file");
        }
        // Family 0 is the wire's "no family", and it must be nil rather than the first row.
        assert_eq!(f.name(0), None);
    }

    /// The eight food names, verbatim and in id order — the row set the mask indexes.
    #[test]
    fn the_real_pet_food_names_are_the_eight_diets() {
        let Some(mut chain) = chain() else { return };
        let n = load_pet_food_names(&mut chain).expect("load ItemPetFood.dbc");
        assert_eq!(n.len(), 8);
        for (id, want) in [
            (1, "Meat"),
            (2, "Fish"),
            (3, "Cheese"),
            (4, "Bread"),
            (5, "Fungus"),
            (6, "Fruit"),
            (7, "Raw Meat"),
            (8, "Raw Fish"),
        ] {
            assert_eq!(n.name(id), Some(want), "food id {id}");
        }
        assert_eq!(n.name(0), None, "the file is 1-based; there is no row 0");
        assert_eq!(n.name(9), None);
    }

    /// **The join, on the real data** — the half neither file can check alone, and the one that
    /// would fail silently as "a bear eats fish and cheese".
    ///
    /// Wolf, Bear, Boar and Turtle are **the binary's own positive controls** (wow-re's
    /// `0x4bea10` carve, 2026-08-06); the rest are vanilla's documented diets, which is the
    /// independent corroboration that `bit b → row b+1` is right (vmangos's
    /// `1 << (FoodType - 1)` is the third).
    #[test]
    fn the_shipped_masks_expand_to_vanillas_own_diets() {
        let Some(mut chain) = chain() else { return };
        let fam = load_creature_families(&mut chain).expect("families");
        let food = load_pet_food_names(&mut chain).expect("foods");
        let diet = |id: u32| food.for_mask(fam.get(id).expect("family").pet_food_mask);

        const EVERYTHING: [&str; 6] = ["Meat", "Fish", "Cheese", "Bread", "Fungus", "Fruit"];
        assert_eq!(diet(1), ["Meat"], "Wolf — control");
        assert_eq!(diet(2), ["Meat", "Fish"], "Cat");
        assert_eq!(diet(4), EVERYTHING, "Bear — control");
        assert_eq!(diet(5), EVERYTHING, "Boar — control");
        assert_eq!(diet(9), ["Fungus", "Fruit"], "Gorilla");
        assert_eq!(diet(12), ["Cheese", "Fungus", "Fruit"], "Tallstrider");
        assert_eq!(diet(27), ["Fish", "Cheese", "Bread"], "Wind Serpent");
        // Turtle (mask 178) is the control that PINS the bit width: it is the only family to set
        // bit 7, and bit 7 → row 8 is what makes "Raw Fish" reachable at all.
        assert_eq!(
            diet(21),
            ["Fish", "Fungus", "Fruit", "Raw Fish"],
            "Turtle — control"
        );

        // **Exactly which families have an EMPTY diet**, because the pet page's diet tooltip
        // depends on it: `BuildListString()` returns nil for an empty list and `format`'s "%s"
        // errors on nil, so the icon is safe only while no HUNTER-tameable family has a zero mask.
        // The shipped file's zero-mask set is the five warlock minions plus row 28 ("Remote
        // Control") — no tameable beast among them. Asserted as an exact set, not a spot check: a
        // future row with a zero mask is exactly the thing that would break that tooltip.
        let empty: Vec<u32> = {
            let mut v: Vec<u32> = fam
                .0
                .iter()
                .filter(|(_, f)| food.for_mask(f.pet_food_mask).is_empty())
                .map(|(id, _)| *id)
                .collect();
            v.sort_unstable();
            v
        };
        assert_eq!(
            empty,
            [15, 16, 17, 19, 23, 28],
            "Felhunter/Voidwalker/Succubus/Doomguard/Imp + Remote Control — no tameable beast"
        );
    }

    /// The mask expansion's own edges, away from the shipped data: an unknown bit names no row
    /// rather than panicking or inventing one, and bits past the table are ignored.
    #[test]
    fn mask_bits_past_the_table_are_ignored() {
        let mut names = HashMap::new();
        names.insert(1, "Meat".to_string());
        names.insert(3, "Cheese".to_string());
        let n = PetFoodNames(names);

        assert_eq!(n.for_mask(0), Vec::<&str>::new());
        assert_eq!(n.for_mask(0b101), ["Meat", "Cheese"]);
        // Bit 1 names row 2, which this cut-down table doesn't have: skipped, not empty-stringed.
        assert_eq!(n.for_mask(0b111), ["Meat", "Cheese"]);
        // Bit 31 is past `MAX_FOOD_BITS` entirely.
        assert_eq!(n.for_mask(0x8000_0001), ["Meat"]);
    }
}
