//! `WeaponSwingSounds2.dbc` — the *connecting* melee swing's whoosh, by weapon weight.
//!
//! The reference builds a flat six-slot cache out of this file at load
//! (`0x45cb00`: a reverse walk over the 16-byte records writing
//! `cache[(critical != 0) + swingType*2] = SoundEntriesID`, bounded `swingType < 3` and
//! `critical < 2`), and the play site indexes it with exactly that arithmetic
//! (`0x457f8d lea eax,[eax+ecx*2]`, `0x457f91 mov edx,[eax*4 + 0xb06bd4]`). This catalog is that
//! cache, built the same way, so a file whose rows moved would still land where the reference
//! puts them.
//!
//! Layout — VERIFIED against the shipped build 5875 file (6 records × 4 fields × 16 B, one-byte
//! string block): `ID(0), SwingType(1), Critical(2), SoundEntriesID(3)`. The six rows are the
//! full cross product, and the kits they name are `LightWeaponNormal/Critical` (233/234),
//! `MediumWeaponNormal/Critical` (235/236) and `HeavyWeaponNormal/Critical` (237/238) — the
//! `mWooshSmall*` / `mWooshMedium*` / `mWooshLarge*` samples.
//!
//! **The weight is not a heuristic**: `swingType` is `ItemSubClass.WeaponSwingSize` for the
//! swinging weapon's `(class 2, subclass)` — see [`crate::ItemSubClassCatalog::weapon_swing_size`].

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, u32_at};
use crate::Chain;

/// How many `swingType` values the reference's cache holds — `0x457f63 cmp ecx,3 / jge` bails
/// above this, and the loader's own `cmp edi,3 / jge` refuses to write past it.
const SWING_TYPES: usize = 3;

/// The six-slot swing-kit cache (`0xb06bd4`), indexed `critical + swingType*2`.
pub struct WeaponSwingCatalog {
    cache: [u32; SWING_TYPES * 2],
}

impl WeaponSwingCatalog {
    /// The kit for a swing of `swing_type` (0 light · 1 medium · 2 heavy), critical or not.
    /// `None` above the reference's ceiling — its play site *returns without playing anything*
    /// on `swingType >= 3`, so an out-of-range weight is silence, never a fallback to light.
    pub fn kit(&self, swing_type: u32, critical: bool) -> Option<u32> {
        let slot = usize::try_from(swing_type).ok()?;
        if slot >= SWING_TYPES {
            return None;
        }
        let kit = self.cache[usize::from(critical) + slot * 2];
        (kit != 0).then_some(kit)
    }
}

fn schema() -> Schema {
    let mut s = Schema::new("WeaponSwingSounds2");
    for name in ["ID", "SwingType", "Critical", "SoundEntriesID"] {
        s.add_field(SchemaField::new(name, FieldType::UInt32));
    }
    s
}

/// Read `WeaponSwingSounds2.dbc` off the patch chain into the reference's cache shape.
pub fn load_weapon_swing_catalog(chain: &mut Chain) -> Result<WeaponSwingCatalog> {
    let bytes = chain
        .read_file("DBFilesClient\\WeaponSwingSounds2.dbc")
        .context("reading WeaponSwingSounds2.dbc")?;
    let rs = parse(&bytes, schema(), "WeaponSwingSounds2")?;
    let mut cache = [0u32; SWING_TYPES * 2];
    for r in rs.records() {
        let g = |i: usize| u32_at(r, i).unwrap_or(0);
        let (swing_type, critical) = (g(1), g(2));
        // The loader's own bounds, kept verbatim: a row outside them is dropped, not clamped.
        if swing_type as usize >= SWING_TYPES || critical >= 2 {
            continue;
        }
        cache[(critical != 0) as usize + swing_type as usize * 2] = g(3);
    }
    Ok(WeaponSwingCatalog { cache })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped table: the full 3 × 2 cross product, on the six `mWoosh*` kits. Skips without
    /// client data.
    #[test]
    fn real_weapon_swing_sounds_decode() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_weapon_swing_catalog(&mut chain).expect("load weapon swing sounds");

        assert_eq!(cat.kit(0, false), Some(233), "LightWeaponNormal");
        assert_eq!(cat.kit(0, true), Some(234), "LightWeaponCritical");
        assert_eq!(cat.kit(1, false), Some(235), "MediumWeaponNormal");
        assert_eq!(cat.kit(1, true), Some(236), "MediumWeaponCritical");
        assert_eq!(cat.kit(2, false), Some(237), "HeavyWeaponNormal");
        assert_eq!(cat.kit(2, true), Some(238), "HeavyWeaponCritical");
    }

    /// Above the reference's `swingType < 3` ceiling there is no sound at all — `0x457f60`
    /// returns before touching the cache. Nothing to skip: the bound is ours, not the file's.
    #[test]
    fn a_weight_past_the_ceiling_is_silence_not_a_fallback() {
        let cat = WeaponSwingCatalog {
            cache: [233, 234, 235, 236, 237, 238],
        };
        assert_eq!(cat.kit(3, false), None);
        assert_eq!(cat.kit(u32::MAX, true), None);
        // …and a slot the file never filled reads as absent rather than kit 0.
        let sparse = WeaponSwingCatalog {
            cache: [233, 0, 0, 0, 0, 0],
        };
        assert_eq!(sparse.kit(0, false), Some(233));
        assert_eq!(sparse.kit(0, true), None);
    }
}
