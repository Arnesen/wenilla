//! Creature voice: **CreatureDisplayInfo.SoundID → CreatureSoundData.dbc** (decision 0070
//! slice 3) — the per-display voice kit set (exertion/wound/death/aggro/fidgets…).
//!
//! Layouts — VERIFIED against build 5875 (header + row decode, 2026-07-02):
//! `CreatureSoundData` **406 × 30 × 120 B**: `ID, ExertionID, ExertionCriticalID, InjuryID,
//! InjuryCriticalID, InjuryCrushingBlowID, DeathID, StunID, StandID, FootstepID (a
//! FootstepTerrainLookup **class**, NOT a kit), AggroID, WingFlapID, WingGlideID, AlertID,
//! Fidget[4], CustomAttack[4], NPCSoundID, LoopSoundID, CreatureImpactType (0 flesh · 1 stone ·
//! 2 wood · 3 ethereal), JumpStartID, JumpEndID, PetAttackID, PetOrderID, PetDismissID`.
//! Spot-check row 26: exertion 312/313, injury 315/316/0, death 314, stun 690, stand 317,
//! footstep class 8, aggro 694, alert 1107 — all coherent kit-id ranges.
//! The chain is `UNIT_FIELD_DISPLAYID` → `CreatureDisplayInfo.SoundID`, **falling back to
//! `CreatureModelData.SoundID`** (col 13 of the 430 × 16 × 64 B table) when the display's own FK
//! is 0 — the client's generic resolution (wow-re `benilla-pins.md` B11b; the earlier "no model
//! fallback in 1.12" note here was wrong). The fallback is load-bearing: 10 261 of 10 534
//! displays carry SoundID 0, and with the model link 10 533/10 534 resolve a row (byte-census
//! 2026-07-03) — character displays reach footstep class 7 this way, as data, not client logic.

use std::collections::HashMap;

use crate::Chain;
use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, u32_at};

/// One `CreatureSoundData` row — every field is a SoundEntries kit id (0 = none) except where
/// noted.
pub struct CreatureVoice {
    /// Attack grunt `[normal, critical]`.
    pub exertion: [u32; 2],
    /// Wound vocal `[normal, critical, crushing]`.
    pub injury: [u32; 3],
    pub death: u32,
    pub stun: u32,
    /// Fired by the `$FDX` anim event.
    pub stand: u32,
    /// `FootstepTerrainLookup.CreatureFootstepID` — the creature's footstep **class**, joined
    /// against the terrain type (NOT a kit id).
    pub footstep_class: u32,
    pub aggro: u32,
    /// Fired by `$WNG` / `$WGG`.
    pub wing_flap: u32,
    pub wing_glide: u32,
    pub alert: u32,
    /// Fired by `$FD1`..`$FD4`.
    pub fidget: [u32; 4],
    /// Fired by `$AH0`..`$AH3`.
    pub custom_attack: [u32; 4],
    /// A looping body sound (SoundEntries type 27).
    pub loop_sound: u32,
    /// Melee impact material: 0 flesh · 1 stone · 2 wood · 3 ethereal (WeaponImpactSounds slot
    /// class — consumed with combat impacts).
    pub impact_type: u32,
    pub jump_start: u32,
    pub jump_end: u32,
}

/// display id → voice rows, joined through `CreatureDisplayInfo.SoundID`.
pub struct CreatureVoiceCatalog {
    display_to_sound: HashMap<u32, u32>,
    rows: HashMap<u32, CreatureVoice>,
}

impl CreatureVoiceCatalog {
    /// The voice set for a creature **display id** (the `NetEntity.display_id` the wire gives us).
    pub fn for_display(&self, display_id: u32) -> Option<&CreatureVoice> {
        self.rows.get(self.display_to_sound.get(&display_id)?)
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

fn csd_schema() -> Schema {
    let mut s = Schema::new("CreatureSoundData");
    for i in 0..30 {
        s.add_field(SchemaField::new(format!("f{i}"), FieldType::UInt32));
    }
    s
}

fn cdi_schema() -> Schema {
    let mut s = Schema::new("CreatureDisplayInfo");
    for i in 0..12 {
        s.add_field(SchemaField::new(format!("f{i}"), FieldType::UInt32));
    }
    s
}

fn cmd_schema() -> Schema {
    let mut s = Schema::new("CreatureModelData");
    for i in 0..16 {
        s.add_field(SchemaField::new(format!("f{i}"), FieldType::UInt32));
    }
    s
}

/// Read both tables off the patch chain into the joined catalog.
pub fn load_creature_voice_catalog(chain: &mut Chain) -> Result<CreatureVoiceCatalog> {
    let bytes = chain
        .read_file("DBFilesClient\\CreatureSoundData.dbc")
        .context("reading CreatureSoundData.dbc")?;
    let rs = parse(&bytes, csd_schema(), "CreatureSoundData")?;
    let mut rows = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        let g = |i: usize| u32_at(r, i).unwrap_or(0);
        rows.insert(
            id,
            CreatureVoice {
                exertion: [g(1), g(2)],
                injury: [g(3), g(4), g(5)],
                death: g(6),
                stun: g(7),
                stand: g(8),
                footstep_class: g(9),
                aggro: g(10),
                wing_flap: g(11),
                wing_glide: g(12),
                alert: g(13),
                fidget: [g(14), g(15), g(16), g(17)],
                custom_attack: [g(18), g(19), g(20), g(21)],
                loop_sound: g(23),
                impact_type: g(24),
                jump_start: g(25),
                jump_end: g(26),
            },
        );
    }

    let bytes = chain
        .read_file("DBFilesClient\\CreatureModelData.dbc")
        .context("reading CreatureModelData.dbc")?;
    let rs = parse(&bytes, cmd_schema(), "CreatureModelData")?;
    let mut model_to_sound = HashMap::new();
    for r in rs.records() {
        if let (Some(id), Some(sound)) = (u32_at(r, 0), u32_at(r, 13)) {
            if sound != 0 {
                model_to_sound.insert(id, sound);
            }
        }
    }

    let bytes = chain
        .read_file("DBFilesClient\\CreatureDisplayInfo.dbc")
        .context("reading CreatureDisplayInfo.dbc")?;
    let rs = parse(&bytes, cdi_schema(), "CreatureDisplayInfo")?;
    let mut display_to_sound = HashMap::new();
    for r in rs.records() {
        let (Some(id), Some(sound), Some(model)) = (u32_at(r, 0), u32_at(r, 2), u32_at(r, 1))
        else {
            continue;
        };
        // The display's own FK wins; 0 falls back to the model's (module docs — B11b).
        let sound = if sound != 0 {
            Some(sound)
        } else {
            model_to_sound.get(&model).copied()
        };
        if let Some(sound) = sound {
            display_to_sound.insert(id, sound);
        }
    }
    Ok(CreatureVoiceCatalog {
        display_to_sound,
        rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The join works on the real 5875 tables: display 26 resolves the byte-decoded row (death
    /// kit 314, footstep class 8), and a majority of sound-linked displays resolve to a real row.
    #[test]
    fn real_creature_voice_resolves() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_creature_voice_catalog(&mut chain).expect("load creature voices");
        assert_eq!(cat.len(), 406, "all CreatureSoundData rows load");

        let v = cat.for_display(26).expect("display 26 has a voice");
        assert_eq!(v.death, 314);
        assert_eq!(v.exertion, [312, 313]);
        assert_eq!(v.footstep_class, 8);
        assert_eq!(v.aggro, 694);

        // The CreatureModelData fallback (B11b): the Elwynn wolf display (903) has
        // CreatureDisplayInfo.SoundID 0 and resolves through its model's SoundID (43);
        // the human-male character display (49) reaches footstep class 7 the same way.
        let wolf = cat.for_display(903).expect("wolf resolves via the model");
        assert_eq!(wolf.footstep_class, 8);
        let human = cat
            .for_display(49)
            .expect("human male resolves via the model");
        assert_eq!(human.footstep_class, 7);
    }
}
