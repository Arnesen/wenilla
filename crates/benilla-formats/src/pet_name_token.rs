//! `ChrClasses.dbc`'s **pet name token** — `HasPetSpells()`'s second return, and the only thing
//! that decides whether the spellbook's second tab reads "Pet" or "Demon" (decision 1032).
//!
//! `HasPetSpells 0x4b4410` pushes it verbatim off a class-indexed record table:
//!
//! ```text
//! 0x4b445d  eax = [player + 0x110]                  ; the descriptor fields
//! 0x4b4463  eax = byte [fields + 0x79]              ; UNIT_FIELD_BYTES_0 byte 1 = the CLASS
//! 0x4b446b  if (class < 0 || class > [0xc0def8]) -> the out-of-range arm
//! 0x4b4473  ecx = [0xc0def4]                        ; the class record table
//! 0x4b4479  eax = ecx[class]                        ; 1-BASED — index 0 is never a class
//! 0x4b447c  edx = [rec + 0x10]                      ; <- the token string
//! 0x4b44a6  (player did not resolve) edx = "PET"    ; the literal 0x846a40
//! ```
//!
//! **`rec + 0x10` is field 4, and the file says which field that is**: dumped from the real 5875
//! `ChrClasses.dbc` (9 rows, 17 columns, 0x44-byte records), field 4 is `"PET"` in every row except
//! Warlock (id 9), which is `"DEMON"`. Field 5 is the class name (`Warrior`/`Paladin`/…), so a
//! one-column slip would put "Warlock" on the tab — plausible enough to ship, which is why
//! [`tests`] anchors both columns.
//!
//! FrameXML then does `getglobal("PET_TYPE_"..token)` (`SpellBookFrame.lua:173`) against
//! `PET_TYPE_PET = "Pet"` / `PET_TYPE_DEMON = "Demon"` (`GlobalStrings.lua:3057-3058`) — so the
//! token is a **key**, never display text, and it is right that it is not localized here.

use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::chain::Chain;
use crate::dbc::{parse, str_at, u32_at};

const CHR_CLASSES: &str = "DBFilesClient\\ChrClasses.dbc";

/// `ChrClasses.dbc`'s column count in 5875 (the header's `field_count`; `benilla-dbc` enforces it).
const CHR_CLASSES_FIELDS: usize = 17;

/// The pet-name-token column — byte `0x10` in the client's own record read, i.e. field 4.
const PET_NAME_TOKEN_FIELD: usize = 0x10 / 4;

/// The literal the client pushes when the player object does not resolve (`0x846a40`). Also the
/// value nine of the ten class rows carry, which is why an unloaded table degrades invisibly.
pub const PET_NAME_TOKEN_FALLBACK: &str = "PET";

/// `ChrClasses.dbc` → class id ⇒ pet name token.
#[derive(Debug, Default, Clone)]
pub struct PetNameTokens(HashMap<u32, String>);

impl PetNameTokens {
    /// The token for a class id, or the client's own [`PET_NAME_TOKEN_FALLBACK`] for a class with
    /// no row. Mirrors `0x4b44a6`: the reference never answers nil here, only ever a string.
    pub fn token(&self, class: u32) -> &str {
        self.0
            .get(&class)
            .map_or(PET_NAME_TOKEN_FALLBACK, String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

fn schema() -> Schema {
    let mut s = Schema::new("ChrClasses");
    for i in 0..CHR_CLASSES_FIELDS {
        // Only the token column is read; everything else stays an opaque dword. The class NAME
        // block (field 5 up) is deliberately not decoded — nothing here displays it.
        let ty = if i == PET_NAME_TOKEN_FIELD {
            FieldType::String
        } else {
            FieldType::UInt32
        };
        s.add_field(SchemaField::new(format!("F{i}"), ty));
    }
    s
}

/// Load `ChrClasses.dbc`'s pet-name-token column from the patch chain.
pub fn load_pet_name_tokens(chain: &mut Chain) -> Result<PetNameTokens> {
    let bytes = chain
        .read_file(CHR_CLASSES)
        .with_context(|| format!("reading {CHR_CLASSES}"))?;
    let rs = parse(&bytes, schema(), "ChrClasses.dbc")?;
    let mut by_id = HashMap::new();
    for r in rs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        if let Some(token) = str_at(&rs, r, PET_NAME_TOKEN_FIELD) {
            by_id.insert(id, token);
        }
    }
    Ok(PetNameTokens(by_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain() -> Option<crate::chain::Chain> {
        let data = crate::wow_data_or_skip!(None);
        Some(crate::open_chain(&data).expect("open chain"))
    }

    /// The real 5875 table, byte-anchored on the one column that matters. The neighbouring field
    /// holds the class NAME, so a one-column slip reads "Warlock"/"Hunter" — which would still
    /// resolve a `PET_TYPE_*` global lookup to nil and blank the tab, i.e. fail silently.
    #[test]
    fn warlocks_pet_is_a_demon_and_everyone_elses_is_a_pet() {
        let Some(mut chain) = chain() else { return };
        let t = load_pet_name_tokens(&mut chain).expect("load ChrClasses.dbc");
        assert!(!t.is_empty());
        assert_eq!(t.token(9), "DEMON", "Warlock");
        for (class, who) in [(1, "Warrior"), (3, "Hunter"), (11, "Druid")] {
            assert_eq!(t.token(class), "PET", "{who}");
        }
        // 6 and 10 have no row in 1.12 (Death Knight / Monk arrive later); the client's own
        // out-of-range arm answers the literal, and so does this.
        assert_eq!(t.token(6), PET_NAME_TOKEN_FALLBACK);
        assert_eq!(t.token(0), PET_NAME_TOKEN_FALLBACK);
    }
}
