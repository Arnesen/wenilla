//! `Languages.dbc` × `ChrRaces.dbc` — the one join behind `GetDefaultLanguage()`.
//!
//! The binding `0x49fcd0` (94 bytes; wow-re `ui/scratch/bag-language-combat-action-bindings.md`
//! §2, §5-cross-checked) is a two-hop table walk and nothing else:
//!
//! ```text
//! player race byte  ->  [0xc0dee0][race] + 0x20   =  base language id
//!                   ->  [0xc0db48][id]  + 4 + 4·[0xc0e080]  =  the localized name
//! ```
//!
//! `+0x20` on a `ChrRaces` record is byte offset 32, i.e. **field 8** — which the shipped 5875
//! data confirms independently: field 8 reads **7 for Human/Dwarf/Night Elf/Gnome/Goblin and 1 for
//! Orc/Undead/Tauren/Troll**, and `Languages.dbc` rows 7 and 1 are `Common` and `Orcish`. So the
//! "default" language is the **faction** language, not the racial one — a Night Elf's default is
//! Common, an Undead's is Orcish, and Darnassian/Gutterspeak are *additional* languages the
//! `GetLanguageByIndex` family enumerates. (Field 9, its neighbour, is `CreatureType` = 7 Humanoid
//! for every playable race — already anchored elsewhere in this crate's consumers, which is what
//! makes the field-8 identification a cross-check rather than a count.)
//!
//! Verified against the real chain below: 13 language rows, 9 race rows, and the nine races
//! resolving to exactly two distinct names.
//!
//! **Locale.** `[0xc0e080]` is the client's locale slot and only column 0 (enUS) is populated in
//! this install — every other DBC catalog in this crate reads column 0 for the same reason. The
//! loader keeps the whole locale row so a localized build has somewhere to go; the app asks for
//! the slot it wants.

use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, str_at, u32_at};
use crate::Chain;

const LANGUAGES: &str = "DBFilesClient\\Languages.dbc";
const CHR_RACES: &str = "DBFilesClient\\ChrRaces.dbc";

/// How many locale columns a `Name_lang` block carries in 5875 (`Languages.dbc` is
/// `ID + 8 names + NameFlags` = 10 fields).
const LOCALES: usize = 8;

/// race id → the localized name of that race's base language, one entry per locale column.
#[derive(Debug, Default, Clone)]
pub struct DefaultLanguages(HashMap<u32, [Option<String>; LOCALES]>);

impl DefaultLanguages {
    /// The race's default chat language in `locale`'s column, or `None` — which is the reference's
    /// own answer shape at three of its four failure edges (no such race row, a language id past
    /// the table, a null record), all of which push **zero Lua values** rather than `nil`.
    pub fn name(&self, race: u32, locale: usize) -> Option<&str> {
        self.0.get(&race)?.get(locale)?.as_deref()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// `Languages.dbc` — `ID`, eight `Name_lang` strings, `NameFlags`.
fn languages_schema() -> Schema {
    let mut s = Schema::new("Languages");
    s.add_field(SchemaField::new("ID", FieldType::UInt32));
    for i in 0..LOCALES {
        s.add_field(SchemaField::new(format!("Name{i}"), FieldType::String));
    }
    s.add_field(SchemaField::new("NameFlags", FieldType::UInt32));
    s
}

/// `ChrRaces.dbc` — 29 fields in 5875. Only field 8 (`BaseLanguage`) is read here, so the rest are
/// declared as plain dwords: the schema's field *count* is what has to match the header, and a
/// string column read as a dword is just its unresolved offset.
fn chr_races_schema() -> Schema {
    let mut s = Schema::new("ChrRaces");
    for i in 0..29 {
        s.add_field(SchemaField::new(format!("f{i}"), FieldType::UInt32));
    }
    s
}

/// The field 8 the binding reads at `record + 0x20` (module doc).
const CHR_RACES_BASE_LANGUAGE: usize = 8;

/// Load and join both tables into the race → language-name map.
pub fn load_default_languages(chain: &mut Chain) -> Result<DefaultLanguages> {
    let lang_bytes = chain
        .read_file(LANGUAGES)
        .with_context(|| format!("reading {LANGUAGES}"))?;
    let langs = parse(&lang_bytes, languages_schema(), "Languages.dbc")?;
    let mut by_id: HashMap<u32, [Option<String>; LOCALES]> = HashMap::new();
    for r in langs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        let mut names: [Option<String>; LOCALES] = Default::default();
        for (locale, slot) in names.iter_mut().enumerate() {
            *slot = str_at(&langs, r, 1 + locale);
        }
        by_id.insert(id, names);
    }

    let race_bytes = chain
        .read_file(CHR_RACES)
        .with_context(|| format!("reading {CHR_RACES}"))?;
    let races = parse(&race_bytes, chr_races_schema(), "ChrRaces.dbc")?;
    let mut out = HashMap::new();
    for r in races.records() {
        let (Some(race), Some(lang)) = (u32_at(r, 0), u32_at(r, CHR_RACES_BASE_LANGUAGE)) else {
            continue;
        };
        if let Some(names) = by_id.get(&lang) {
            out.insert(race, names.clone());
        }
    }
    Ok(DefaultLanguages(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real chain, because the whole point of this module is a join between two shipped
    /// tables — a synthetic fixture would only test the plumbing.
    #[test]
    fn the_shipped_tables_join_to_two_faction_languages() {
        let data = crate::wow_data_or_skip!();
        let mut chain = Chain::open(&data).expect("open patch chain");
        let langs = load_default_languages(&mut chain).expect("load");

        // All nine `ChrRaces` rows resolve (the eight playable ones plus Goblin).
        assert_eq!(langs.len(), 9, "one entry per ChrRaces row");
        // Alliance → Common, Horde → Orcish. This is the finding that makes the field-8
        // identification falsifiable: if 8 were the *racial* language, Night Elf would read
        // "Darnassian" and Undead "Gutterspeak".
        for race in [1, 3, 4, 7] {
            assert_eq!(langs.name(race, 0), Some("Common"), "race {race}");
        }
        for race in [2, 5, 6, 8] {
            assert_eq!(langs.name(race, 0), Some("Orcish"), "race {race}");
        }
        // A race the table has no row for is `None` — the reference's "no such record" edge.
        assert_eq!(langs.name(99, 0), None);
        // Only enUS is populated in this install; a higher locale column is empty, not a panic.
        assert_eq!(langs.name(1, 5), None);
    }
}
