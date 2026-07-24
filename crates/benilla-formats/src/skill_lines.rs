//! `SkillLine.dbc` + `SkillLineAbility.dbc` loader — spell id → skill line → {name, icon}, the
//! spellbook's tab source (decision 0216 §8: "tabs = the class skill lines of known spells").
//!
//! Layout — VERIFIED against the **vmangos server source**
//! (`vmangos-src/src/game/Database/DBCStructure.h`'s `SkillLineEntry`/`SkillLineAbilityEntry`
//! structs + `DBCfmt.h`'s `SkillLinefmt`/`SkillLineAbilityfmt` format strings): vmangos parses
//! these two tables straight off the same build-5875 client data benilla reads, so its struct
//! layout — not an empirical guess, not even wow-re's own notes (none exist for these two tables,
//! grepped `system/dbc`) — is the strongest ground available, the same standing this codebase
//! already gives vmangos's wire handlers (decision 0216's own citations of `ItemHandler.cpp`/
//! `Player.cpp`).
//!
//! `SkillLine.dbc` — `SkillLinefmt = "nixssssssssxxxxxxxxxxi"` (22 fields, 88 B/record):
//! `id`(0, indexed) · `categoryId`(1, int32) · `skillCostID`(2, unused) · the 8-locale
//! `displayName_lang` block (3..10, enUS first ⇒ **NameEnUs = column 3**) + its flags word (11) ·
//! the 8-locale `description_lang` block (12..19; enUS **column 12** = the skills pane's
//! detail-pane body, `GetSkillLineInfo`'s 13th return) + its flags word (20) ·
//! **`spellIcon` = column 21** (a `SpellIcon.dbc` id — [`crate::dbc::load_spell_icon_map`], the
//! same table `spells.rs`'s action-bar catalog joins against).
//!
//! `SkillLineAbility.dbc` — `SkillLineAbilityfmt = "niiiixxiiiiixxi"` (15 fields, 60 B/record):
//! `id`(0, indexed) · **`skillId` = column 1** · **`spellId` = column 2** · `racemask`(3) ·
//! `classmask`(4) · `req_skill_value`(7; 5-6 unused: `racemaskNot`/`classmaskNot`, always 0 this
//! build) · `forward_spellid`(8) · `learnOnGetSkill`(9) · `max_value`(10) · `min_value`(11) ·
//! `reqtrainpoints`(14; 12-13 unused). Read into one [`SlaInfo`] per spell (a spell can carry
//! more than one row across race/class variants; the FIRST row wins, deterministic by file
//! order — [`SkillLineCatalog::spell_to_line`]'s long-standing convention). `max_value`/
//! `min_value` are the recipe-difficulty trivial ranks (TrivialSkillLineRankHigh/Low): pinned on
//! the raw 5875 file this session — Bolt of Linen Cloth 2963 → (line 197, req 1, low 25,
//! high 50), Minor Healing Potion 2330 → (171, 1, 55, 95), which reproduces its known classic
//! orange 1 / yellow 55 / green 75 / gray 95 progression under the color law TU-C confirmed at
//! the bytes (decision 0446).
//!
//! `SkillRaceClassInfo.dbc` — `SkillRaceClassInfofmt = "diiiiiix"` (8 fields, 32 B/record):
//! `id`(0) · **`skillId` = column 1** · **`raceMask` = column 2** · **`classMask` = column 3** ·
//! **`flags` = column 4** · `reqLevel`(5) · `skillTierId`(6) · `skillCostID`(7, unused). This is
//! the table the client's spellbook tab classifier routes through (decision 0228): a spell's skill
//! line is looked up here for the player's race+class, and if the matching row's `flags` bit `0x80`
//! (`SKILL_FLAG_DISPLAY_SORTED`, cmangos `DBCEnums.h`) is set — or no row matches — the spell's tab
//! is **General** (key 0) instead of the line's own tab. Byte-verified: wow-re
//! `system/ui/scratch/spellbook-book-build.md` §3 (`0x6ddf90(skillLine, class, race) → variant`;
//! `(int8)[variant+0x10] < 0 → key 0`; `[variant+4]` = skillId, `[variant+0x10]` = flags — the
//! struct offsets confirm the column read). The `flags`/`raceMask`/`classMask` semantics follow
//! vmangos `DBCStructure.h`'s `SkillRaceClassInfoEntry`; the row-match (first row whose masks
//! admit the race/class) is the standard classic semantics, validated against the real build-5875
//! data by [`SkillLineCatalog::spell_tab`]'s tests.
//!
//! `SkillLineCategory.dbc` — byte-checked on the raw 5875 file (a struct-unpack dump: 8 records ×
//! 11 fields, 44 B/record): `id`(0) · the 8-locale `name` block (enUS ⇒ **column 1**) + flags(9) ·
//! **`displayOrder` = column 10** — the skills pane's header vocabulary and group order (decision
//! 0437 phase 4): Class Skills(7, order 2) · Professions(11, 3) · Secondary(9, 4) · Weapon(6, 5) ·
//! Armor(8, 6) · Languages(10, 7); `Attributes`(5, 1) never carries player rows and
//! `Not Displayed`(12, 8) is the hide bucket. A skill line's own `categoryId` is `SkillLine.dbc`
//! column 1 (the `SkillLinefmt` layout above).
//!
//! Skill line ids are stable, well-known constants across the whole classic tool ecosystem
//! (vmangos `SharedDefines.h`'s `SkillType` enum, itself commented "Data from SpellLine.dbc (1.12.1
//! checked)") — Frost=6, Fire=8, … — cross-checked directly by this module's own real-data tests.

use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, str_at, u32_at};
use crate::Chain;

const SKILL_LINE: &str = "DBFilesClient\\SkillLine.dbc";
const SKILL_LINE_ABILITY: &str = "DBFilesClient\\SkillLineAbility.dbc";
const SKILL_RACE_CLASS_INFO: &str = "DBFilesClient\\SkillRaceClassInfo.dbc";

const SKILL_LINE_FIELDS: usize = 22;
const COL_SL_NAME_ENUS: usize = 3;
const COL_SL_DESC_ENUS: usize = 12;
const COL_SL_SPELL_ICON: usize = 21;

const SKILL_LINE_ABILITY_FIELDS: usize = 15;
const COL_SLA_SKILL_ID: usize = 1;
const COL_SLA_SPELL_ID: usize = 2;
const COL_SLA_REQ_SKILL_VALUE: usize = 7;
const COL_SLA_TRIVIAL_HIGH: usize = 10;
const COL_SLA_TRIVIAL_LOW: usize = 11;

const SKILL_LINE_CATEGORY: &str = "DBFilesClient\\SkillLineCategory.dbc";
const SKILL_LINE_CATEGORY_FIELDS: usize = 11;
const COL_SLC_NAME_ENUS: usize = 1;
const COL_SLC_ORDER: usize = 10;
/// `SkillLineCategory` id 12 — "Not Displayed": the skills pane hides lines in this bucket.
pub const SKILL_CATEGORY_NOT_DISPLAYED: u32 = 12;

const SKILL_RACE_CLASS_INFO_FIELDS: usize = 8;
const COL_SRCI_SKILL_ID: usize = 1;
const COL_SRCI_RACE_MASK: usize = 2;
const COL_SRCI_CLASS_MASK: usize = 3;
const COL_SRCI_FLAGS: usize = 4;

/// `SkillRaceClassInfo.flags` bit `0x80` — cmangos `DBCEnums.h`'s `SKILL_FLAG_DISPLAY_SORTED`. The
/// spellbook tab classifier reads it as the low byte's sign (`(int8) < 0`): set ⇒ the skill line's
/// spells sort into the **General** tab rather than the line's own (decision 0228). Real
/// build-5875 data for a human warrior: set on `Racial - Human`, `GENERIC (DND)`, the proficiency/
/// language/riding lines; clear on the class combat lines (`Arms`/`Fury`/`Protection`).
const SKILL_FLAG_DISPLAY_SORTED: u32 = 0x80;

/// `SkillRaceClassInfo.flags` bit `0x20` — vmangos `DBCEnums.h`'s `SKILL_FLAG_UNLEARNABLE`
/// ("Skill can be unlearned"): the skills pane's unlearn-button gate, and the exact bit the
/// server's own `CMSG_UNLEARN_SKILL` handler enforces (vmangos `SkillHandler.cpp` — a request
/// for a line without it is dropped and anticheat-flagged, so the client must never offer it).
const SKILL_FLAG_UNLEARNABLE: u32 = 0x20;

/// One skill line's display identity (`SkillLine.dbc`) — a spellbook tab's name + icon, and the
/// skills pane's grouping key.
#[derive(Clone, Debug, PartialEq)]
pub struct SkillLineInfo {
    pub name: String,
    /// `categoryId` (column 1) — the [`SkillLineCategory`](Self) bucket the skills pane groups
    /// this line under ([`SkillLineCatalog::category`]); 0 when absent.
    pub category_id: u32,
    /// The tab icon's MPQ path (`Interface\Icons\…`, extensionless); `None` when the line's
    /// `spellIcon` id is 0/unresolved (render the fallback question mark, the spell catalog's own
    /// convention).
    pub icon: Option<String>,
    /// `description_lang` enUS (column 12) — the skills pane's detail-pane body
    /// (`GetSkillLineInfo`'s 13th return). Professions carry the trade's flavor sentence, weapon
    /// lines the shared "Higher weapon skill increases your chance to hit."; empty when the row
    /// has none.
    pub description: String,
}

/// One `SkillRaceClassInfo.dbc` row's tab-classification inputs: which race/class it admits and
/// whether its skill line's spells sort into General ([`SKILL_FLAG_DISPLAY_SORTED`]).
#[derive(Clone, Copy, Debug)]
struct SrciRow {
    race_mask: u32,
    class_mask: u32,
    flags: u32,
}

/// One spell's `SkillLineAbility.dbc` row (module doc columns; first row wins across race/class
/// variants): the skill line it belongs to, the rank required to learn it, and the trivial ranks
/// the crafting book's difficulty colors band against (the color law TU-C confirmed at the bytes,
/// decision 0446).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SlaInfo {
    /// `skillId` (column 1) — the owning skill line.
    pub skill_id: u32,
    /// `req_skill_value` (column 7) — the line rank required to learn/use the ability.
    pub req_skill_value: u32,
    /// `min_value` (column 11) — TrivialSkillLineRankLow: yellow at-or-above, orange below.
    pub trivial_low: u32,
    /// `max_value` (column 10) — TrivialSkillLineRankHigh: gray at-or-above; green at-or-above
    /// the low/high midpoint. Both 0 on non-recipe rows (class abilities, the openers).
    pub trivial_high: u32,
}

/// `SkillLine.dbc` × `SkillLineAbility.dbc` × `SkillRaceClassInfo.dbc`, joined: a spell's skill
/// line, a line's display, and the per-race/class tab routing (the General collapse).
pub struct SkillLineCatalog {
    lines: HashMap<u32, SkillLineInfo>,
    abilities: HashMap<u32, SlaInfo>,
    /// `SkillLineCategory.dbc`: id → (enUS name, displayOrder) — the skills pane's headers.
    categories: HashMap<u32, (String, u32)>,
    /// skill line id → its `SkillRaceClassInfo` rows (empty when the DBC failed to load — then
    /// [`Self::spell_tab`] skips the General collapse and keeps each line its own tab).
    race_class: HashMap<u32, Vec<SrciRow>>,
}

impl SkillLineCatalog {
    /// The skill line a spell belongs to, if `SkillLineAbility.dbc` names one.
    pub fn spell_to_line(&self, spell_id: u32) -> Option<u32> {
        self.abilities.get(&spell_id).map(|a| a.skill_id)
    }

    /// A spell's full `SkillLineAbility` row ([`SlaInfo`]) — the crafting book's difficulty and
    /// requirement source (0437).
    pub fn ability(&self, spell_id: u32) -> Option<&SlaInfo> {
        self.abilities.get(&spell_id)
    }

    /// The spellbook **tab** a spell lands in for a character of `race`/`class` (1-based unit
    /// bytes): the spell's skill line, unless that line routes to General (decision 0228). Returns
    /// `0` (the General tab) when the spell has no skill line, no `SkillRaceClassInfo` row admits
    /// this race/class, or the matching row carries [`SKILL_FLAG_DISPLAY_SORTED`]; the line's own
    /// id otherwise. With `race`/`class` `0` or out of range (unknown character), or when no
    /// `SkillRaceClassInfo` data loaded, the collapse is skipped — the raw skill line is returned.
    pub fn spell_tab(&self, spell_id: u32, race: u8, class: u8) -> u32 {
        let Some(line) = self.spell_to_line(spell_id) else {
            return 0; // no skill line → General
        };
        // No character context, or no routing data — keep the raw line (pre-collapse behavior).
        if self.race_class.is_empty() || !(1..=32).contains(&race) || !(1..=32).contains(&class) {
            return line;
        }
        match self.srci_row(line, race, class) {
            // A matching row without the sort flag keeps the line's own tab.
            Some(r) if r.flags & SKILL_FLAG_DISPLAY_SORTED == 0 => line,
            // The sort flag, or no admitting row for this race/class → General.
            _ => 0,
        }
    }

    /// The first `SkillRaceClassInfo` row of `line_id` admitting a 1-based `race`/`class` (mask
    /// `0` admits all) — the standard classic row-match ([`Self::spell_tab`]'s own, factored out
    /// for [`Self::abandonable`]). `None` for out-of-range race/class or no admitting row.
    fn srci_row(&self, line_id: u32, race: u8, class: u8) -> Option<&SrciRow> {
        if !(1..=32).contains(&race) || !(1..=32).contains(&class) {
            return None;
        }
        let race_bit = 1u32 << (race - 1);
        let class_bit = 1u32 << (class - 1);
        self.race_class.get(&line_id).and_then(|rows| {
            rows.iter().find(|r| {
                (r.race_mask == 0 || r.race_mask & race_bit != 0)
                    && (r.class_mask == 0 || r.class_mask & class_bit != 0)
            })
        })
    }

    /// Whether `line_id` can be unlearned by a character of `race`/`class` (1-based unit bytes):
    /// the admitting `SkillRaceClassInfo` row carries [`SKILL_FLAG_UNLEARNABLE`] (`0x20`) — the
    /// skills pane's unlearn-button predicate, and byte-for-byte the server's own gate (vmangos
    /// `SkillHandler.cpp`). `false` with no routing data, unknown race/class, or no admitting
    /// row — a missing button beats offering an unlearn the server would anticheat-flag.
    pub fn abandonable(&self, line_id: u32, race: u8, class: u8) -> bool {
        self.srci_row(line_id, race, class)
            .is_some_and(|r| r.flags & SKILL_FLAG_UNLEARNABLE != 0)
    }

    /// A skill line's display (name + tab icon), by id.
    pub fn line(&self, line_id: u32) -> Option<&SkillLineInfo> {
        self.lines.get(&line_id)
    }

    /// A `SkillLineCategory.dbc` row's `(name, displayOrder)` — the skills pane's header for a
    /// line's [`SkillLineInfo::category_id`]; `None` for 0/unknown.
    pub fn category(&self, category_id: u32) -> Option<(&str, u32)> {
        self.categories
            .get(&category_id)
            .map(|(n, o)| (n.as_str(), *o))
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

fn skill_line_schema() -> Schema {
    let mut s = Schema::new("SkillLine");
    for i in 0..SKILL_LINE_FIELDS {
        if i == COL_SL_NAME_ENUS {
            s.add_field(SchemaField::new("NameEnUs", FieldType::String));
        } else if i == COL_SL_DESC_ENUS {
            s.add_field(SchemaField::new("DescEnUs", FieldType::String));
        } else {
            s.add_field(SchemaField::new(format!("F{i}"), FieldType::UInt32));
        }
    }
    s
}

fn skill_line_ability_schema() -> Schema {
    let mut s = Schema::new("SkillLineAbility");
    for i in 0..SKILL_LINE_ABILITY_FIELDS {
        s.add_field(SchemaField::new(format!("F{i}"), FieldType::UInt32));
    }
    s
}

fn skill_line_category_schema() -> Schema {
    let mut s = Schema::new("SkillLineCategory");
    for i in 0..SKILL_LINE_CATEGORY_FIELDS {
        let ty = if i == COL_SLC_NAME_ENUS {
            FieldType::String
        } else {
            FieldType::UInt32
        };
        s.add_field(SchemaField::new(format!("F{i}"), ty));
    }
    s
}

/// Load `SkillLineCategory.dbc` — id → (name, displayOrder). Missing/unparseable degrades to an
/// empty map (the skills pane then renders one flat group).
fn load_categories(chain: &mut Chain) -> HashMap<u32, (String, u32)> {
    let mut map = HashMap::new();
    let Ok(bytes) = chain.read_file(SKILL_LINE_CATEGORY) else {
        return map;
    };
    let Ok(set) = parse(
        &bytes,
        skill_line_category_schema(),
        "SkillLineCategory.dbc",
    ) else {
        return map;
    };
    for r in set.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        if let Some(name) = str_at(&set, r, COL_SLC_NAME_ENUS) {
            map.insert(id, (name, u32_at(r, COL_SLC_ORDER).unwrap_or(0)));
        }
    }
    map
}

fn skill_race_class_info_schema() -> Schema {
    let mut s = Schema::new("SkillRaceClassInfo");
    for i in 0..SKILL_RACE_CLASS_INFO_FIELDS {
        s.add_field(SchemaField::new(format!("F{i}"), FieldType::UInt32));
    }
    s
}

/// The `SkillRaceClassInfo.dbc` rows keyed by skill line — the General-collapse routing table. A
/// missing/unparseable file returns an empty map (the caller degrades to "each line its own tab").
fn load_race_class_info(chain: &mut Chain) -> HashMap<u32, Vec<SrciRow>> {
    let mut map: HashMap<u32, Vec<SrciRow>> = HashMap::new();
    let bytes = match chain.read_file(SKILL_RACE_CLASS_INFO) {
        Ok(b) => b,
        Err(_) => return map,
    };
    let set = match parse(
        &bytes,
        skill_race_class_info_schema(),
        "SkillRaceClassInfo.dbc",
    ) {
        Ok(s) => s,
        Err(_) => return map,
    };
    for r in set.records() {
        let Some(skill) = u32_at(r, COL_SRCI_SKILL_ID) else {
            continue;
        };
        map.entry(skill).or_default().push(SrciRow {
            race_mask: u32_at(r, COL_SRCI_RACE_MASK).unwrap_or(0),
            class_mask: u32_at(r, COL_SRCI_CLASS_MASK).unwrap_or(0),
            flags: u32_at(r, COL_SRCI_FLAGS).unwrap_or(0),
        });
    }
    map
}

/// Load the joined skill-line catalog off the patch chain.
pub fn load_skill_line_catalog(chain: &mut Chain) -> Result<SkillLineCatalog> {
    let icons = crate::dbc::load_spell_icon_map(chain)?;

    let sl_bytes = chain
        .read_file(SKILL_LINE)
        .context("reading SkillLine.dbc")?;
    let sl_set = parse(&sl_bytes, skill_line_schema(), "SkillLine.dbc")?;
    let mut lines: HashMap<u32, SkillLineInfo> = HashMap::new();
    for r in sl_set.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        let name = str_at(&sl_set, r, COL_SL_NAME_ENUS).unwrap_or_default();
        let icon = u32_at(r, COL_SL_SPELL_ICON)
            .filter(|&i| i != 0)
            .and_then(|i| icons.get(&i).cloned());
        let category_id = u32_at(r, 1).unwrap_or(0);
        let description = str_at(&sl_set, r, COL_SL_DESC_ENUS).unwrap_or_default();
        lines.insert(
            id,
            SkillLineInfo {
                name,
                category_id,
                icon,
                description,
            },
        );
    }

    let sla_bytes = chain
        .read_file(SKILL_LINE_ABILITY)
        .context("reading SkillLineAbility.dbc")?;
    let sla_set = parse(
        &sla_bytes,
        skill_line_ability_schema(),
        "SkillLineAbility.dbc",
    )?;
    let mut abilities: HashMap<u32, SlaInfo> = HashMap::new();
    for r in sla_set.records() {
        if let (Some(skill_id), Some(spell_id)) =
            (u32_at(r, COL_SLA_SKILL_ID), u32_at(r, COL_SLA_SPELL_ID))
        {
            // First row wins (module doc): deterministic, and every probed spell in the tests
            // below has exactly one row anyway.
            abilities.entry(spell_id).or_insert(SlaInfo {
                skill_id,
                req_skill_value: u32_at(r, COL_SLA_REQ_SKILL_VALUE).unwrap_or(0),
                trivial_low: u32_at(r, COL_SLA_TRIVIAL_LOW).unwrap_or(0),
                trivial_high: u32_at(r, COL_SLA_TRIVIAL_HIGH).unwrap_or(0),
            });
        }
    }

    let race_class = load_race_class_info(chain);
    let categories = load_categories(chain);

    Ok(SkillLineCatalog {
        lines,
        abilities,
        categories,
        race_class,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolve real spells to their real skill lines on the build-5875 data, cross-checked
    /// against vmangos `SharedDefines.h`'s `SkillType` enum (Frost=6, Fire=8 — the module doc's
    /// own citation). Skips without client data.
    #[test]
    fn real_skill_line_catalog_resolves_known_spells() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_skill_line_catalog(&mut chain).expect("load SkillLine/SkillLineAbility");
        assert!(
            cat.len() > 50,
            "a real skill-line table has hundreds of rows"
        );

        // Fireball (133) -> the Fire line (SKILL_FIRE = 8).
        let fire_line = cat.spell_to_line(133).expect("Fireball has a skill line");
        assert_eq!(fire_line, 8);
        let fire = cat.line(fire_line).expect("the Fire line resolves");
        assert_eq!(fire.name, "Fire");
        assert!(fire.icon.is_some(), "the Fire tab has an icon");

        // Frost Armor (168) -> the Frost line (SKILL_FROST = 6).
        let frost_line = cat
            .spell_to_line(168)
            .expect("Frost Armor has a skill line");
        assert_eq!(frost_line, 6);
        let frost = cat.line(frost_line).expect("the Frost line resolves");
        assert_eq!(frost.name, "Frost");
        assert!(frost.icon.is_some(), "the Frost tab has an icon");

        // An unknown spell id has no line.
        assert_eq!(cat.spell_to_line(0), None);

        // The description column (12): a profession line carries the flavor sentence, a weapon
        // line is blank — a column slip lands on another locale (empty for enUS data) or the
        // name flags word (a parse error) and fails loudly.
        let smithing = cat.line(164).expect("Blacksmithing resolves");
        assert!(
            smithing.description.to_lowercase().contains("blacksmith"),
            "Blacksmithing's description names the trade: {:?}",
            smithing.description
        );
        let swords = cat.line(43).expect("Swords resolves");
        assert_eq!(
            swords.description, "Higher weapon skill increases your chance to hit.",
            "the weapon lines' shared byte-exact sentence"
        );
    }

    /// The General collapse on real build-5875 `SkillRaceClassInfo.dbc` (decision 0228), traced on
    /// concrete spells for a human warrior (race 1, class 1) vs. a human mage (race 1, class 8):
    /// class-native combat lines keep their own tab, racials collapse to General, and a cross-class
    /// spell (a warrior's cheated Fireball) collapses to General while the SAME spell keeps its Fire
    /// tab for a mage — the class/race dependence, the whole point of the routing. Skips without
    /// client data.
    #[test]
    fn real_spell_tab_collapses_general_by_race_and_class() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_skill_line_catalog(&mut chain).expect("load skill lines");
        const HUMAN: u8 = 1;
        const WARRIOR: u8 = 1;
        const MAGE: u8 = 8;

        // Warrior class abilities keep their own class-line tabs (flag clear for a warrior):
        // Charge/Heroic Strike/Rend on Arms (26), Battle Shout on Fury (256).
        for (id, line) in [(100u32, 26u32), (78, 26), (772, 26), (6673, 256)] {
            assert_eq!(cat.spell_to_line(id), Some(line));
            assert_eq!(
                cat.spell_tab(id, HUMAN, WARRIOR),
                line,
                "warrior class ability {id} keeps its own tab {line}"
            );
        }

        // A human racial (Perception, line 754 "Racial - Human") collapses to General (0).
        assert_eq!(cat.spell_to_line(20600), Some(754));
        assert_eq!(
            cat.spell_tab(20600, HUMAN, WARRIOR),
            0,
            "a human racial routes to General"
        );

        // Fireball (Fire line 8): class-dependent. A warrior has no Fire race/class row → General;
        // a mage has one, flag clear → its own Fire tab. Same spell, different tab by class.
        assert_eq!(cat.spell_to_line(133), Some(8));
        assert_eq!(
            cat.spell_tab(133, HUMAN, WARRIOR),
            0,
            "a warrior's cross-class Fireball collapses to General"
        );
        assert_eq!(
            cat.spell_tab(133, HUMAN, MAGE),
            8,
            "a mage's Fireball keeps its Fire tab"
        );

        // Unknown character (race/class 0): the collapse is skipped — the raw line stands.
        assert_eq!(cat.spell_tab(133, 0, 0), 8);
    }

    /// The `SlaInfo` columns on the real build-5875 `SkillLineAbility.dbc` (0437) — expected
    /// values read straight off the raw file's rows this session (a struct-unpack dump, module
    /// doc): recipes carry the trivial ranks, the profession openers carry zeros. A column slip
    /// fails loudly. Skips without client data.
    #[test]
    fn real_skill_line_ability_reads_requirement_and_trivial_ranks() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_skill_line_catalog(&mut chain).expect("load skill lines");

        // Recipes: (spell, line, req, trivial_low, trivial_high) — raw rows, module doc.
        // 2963 Bolt of Linen Cloth (Tailoring 197), 2330 Minor Healing Potion (Alchemy 171),
        // 2538 Charred Wolf Meat (Cooking 185), 3920 Crafted Light Shot (Engineering 202).
        for (spell, line, req, low, high) in [
            (2963u32, 197u32, 1u32, 25u32, 50u32),
            (2330, 171, 1, 55, 95),
            (2538, 185, 1, 45, 85),
            (3920, 202, 1, 30, 60),
        ] {
            let sla = cat.ability(spell).expect("recipe has an SLA row");
            assert_eq!(
                (
                    sla.skill_id,
                    sla.req_skill_value,
                    sla.trivial_low,
                    sla.trivial_high
                ),
                (line, req, low, high),
                "spell {spell}"
            );
        }

        // The openers (effect-47 window spells) sit on their line with zero trivial ranks:
        // 3908 Tailoring → 197, 7411 Enchanting → 333.
        for (spell, line) in [(3908u32, 197u32), (7411, 333)] {
            let sla = cat.ability(spell).expect("opener has an SLA row");
            assert_eq!(
                (sla.skill_id, sla.trivial_low, sla.trivial_high),
                (line, 0, 0)
            );
        }
    }

    /// `SkillLineCategory.dbc` + the line→category join on the real build-5875 files (0437 phase
    /// 4) — expected values read straight off the raw dump (module doc). Skips without client
    /// data.
    #[test]
    fn real_skill_categories_name_and_order_the_pane_groups() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_skill_line_catalog(&mut chain).expect("load skill lines");

        // The raw rows: (id, name, displayOrder).
        assert_eq!(cat.category(7), Some(("Class Skills", 2)));
        assert_eq!(cat.category(11), Some(("Professions", 3)));
        assert_eq!(cat.category(9), Some(("Secondary Skills", 4)));
        assert_eq!(cat.category(6), Some(("Weapon Skills", 5)));
        assert_eq!(cat.category(10), Some(("Languages", 7)));
        assert_eq!(
            cat.category(SKILL_CATEGORY_NOT_DISPLAYED),
            Some(("Not Displayed", 8))
        );
        assert_eq!(cat.category(0), None);

        // The join: Tailoring (197) is a Profession; First Aid (129) is Secondary; the Fire
        // school (8) is a Class Skill; Common (98) is a Language.
        for (line, category) in [(197u32, 11u32), (129, 9), (8, 7), (98, 10)] {
            assert_eq!(
                cat.line(line).map(|l| l.category_id),
                Some(category),
                "line {line}"
            );
        }
    }

    /// `SkillRaceClassInfo.flags & 0x20` on the real build-5875 file: professions and secondary
    /// skills are abandonable, class/weapon/language lines are not — the unlearn button's real
    /// data split (a human warrior, race 1 / class 1). Skips without client data.
    #[test]
    fn real_abandonable_split_professions_yes_weapons_no() {
        let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data");
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_skill_line_catalog(&mut chain).expect("load skill lines");

        // PRIMARY professions carry 0xA0 (0x80 sort | 0x20 unlearnable) — the red circle-slash:
        // Blacksmithing, Tailoring, Engineering, Alchemy, Enchanting, Leatherworking, Skinning.
        for line in [164u32, 197, 202, 171, 333, 165, 393] {
            assert!(cat.abandonable(line, 1, 1), "line {line} is abandonable");
        }
        // SECONDARY skills (First Aid/Fishing/Cooking) are 0x80 only — famously NOT droppable in
        // 1.12 — and class school (Fire: no human-warrior row at all) / weapon / Defense /
        // language / riding lines aren't either.
        for line in [129u32, 356, 185, 8, 43, 95, 98, 762] {
            assert!(!cat.abandonable(line, 1, 1), "line {line} is not");
        }
        // Unknown race/class → no button (the conservative arm).
        assert!(!cat.abandonable(164, 0, 0));
    }
}
