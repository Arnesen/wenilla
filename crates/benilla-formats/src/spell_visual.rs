//! `SpellVisual.dbc` + `SpellVisualKit.dbc` — the per-lifecycle-stage kit chain a cast plays
//! (decision 0099 phase 2's data plane; schemas pinned by decision 0107).
//!
//! Layout — VERIFIED against build 5875 (WDBC header dump of the extracted files, 2026-07-04;
//! wow-re `system/dbc/scratch/spellvisual-schema.md` @ commit `ff91e7eb`, corroborated by vmangos
//! `DBCStructure.h:613-640` `SpellVisualEntry` and our own header bytes):
//!
//! - **SpellVisual**: 2165 records × 16 fields × 64 B, all-`u32` (a 1-byte empty string block).
//!   Field 0 = id; **field 1 = precastKit · field 2 = castKit · field 3 = impactKit · field 4 =
//!   stateKit · field 5 = channelKit** — each a `SpellVisualKit` id, `0` = no kit at that stage.
//!   The missile block (decision 0099 phase 4, wow-re `spell-visual-lifecycle.md` §Q4 — every
//!   consumption byte-cited in the client's missile spawn `0x60a3d0`): **field 7 = the missile's
//!   `SpellVisualEffectName` id** (`<1` → the ammo/weapon model; unresolvable → the client's
//!   literal `Spells\ErrorCube.mdx`), **field 9 = the destination-attachment ordinal** (an index
//!   into [`MISSILE_ATTACH_TABLE`], the client's `0x860a18` — dumped in wow-re
//!   `spell-visual-apply.md` §5), and **field 10 = the missile's in-flight LOOP sound**
//!   ([`SoundEntries`] id — Fireball's `FireMissileLoop`, the thrown dagger's `WeaponLoop`; the
//!   client's per-missile loop handle `CMissile+0x44`, wow-re `w2f1.md`). Field 6 (`hasMissile`)
//!   is never read — the spawn gate is `Spell.dbc` Speed alone; field 8 (`missilePathType`) is
//!   dead-by-absence; field 13 (the caster's ground-arrival kit) is not read here yet. Stage
//!   semantics (decision 0107 verdict 2): the stage sets *lifetime policy* only —
//!   every populated slot on a reached row fires, precast persisting (reaped spell-id-keyed)
//!   while cast/impact self-terminate.
//! - **SpellVisualKit**: 1772 records × 35 fields × 140 B, all-`u32`. Field 0 = id; **field 2 =
//!   the `AnimationData.dbc` animation id** (fed to the same `PlayAnimation` one-shot/overlay
//!   route as melee swings — decision 0107 verdict 3); **field 13 = a `SoundEntries.dbc` id**;
//!   **fields 3–11 are the nine `SpellVisualEffectName` emitter slots** (attach-point VFX —
//!   decision 0099 phase 3, read here since that phase). Each slot's attach tag is a compile-time
//!   immediate in the client's slot loop (`0x60edf0`, byte-cited push sites — wow-re
//!   `spell-visual-apply.md` §1.3) and a **direct M2 `AttachmentID`**: in kit-field order,
//!   [`KIT_SLOT_TAGS`]. Field 12 is the missile effect slot (phase 4), field 14 a visual-group
//!   fallback id — neither is read here.
//! - **SpellVisualEffectName**: 5 fields × 20 B. Field 0 = id; field 1 = a name string — a debug
//!   label, and the lookup key for the client's boot-time HARDCODED-effect matcher (`0x61f5b0`
//!   over a 14-string table: loot art, footsteps, breath, level-up…; wow-re
//!   `loot-corpse-effect.md` + `levelup-ding.md`) — the `"HARDCODED *"` rows load into
//!   [`SpellVisualCatalog::hardcoded_effect`]'s name→path map (two consumers today: the corpse
//!   sparkle and the level-up ding); **field 2 = the effect model's `.mdx` path** (the
//!   column every consumer reads — fields 3/4 are dead-by-absence, wow-re
//!   `spellvisual-schema.md`; the emitter *scale* comes from kit CharProc params × the quality
//!   tier, never from this table).
//!
//! **The none-sentinel, found empirically here (not pinned in the wow-re note):** on the real
//! table, "no value" for both field 2 (anim) and field 13 (sound) is written as **either `0` or
//! `0xFFFFFFFF`**, not consistently one or the other. Scanning all 1772 kits: 41 carry anim `0`,
//! 875 carry anim `0xFFFFFFFF`, 856 carry a real id (max 203, inside `AnimationData.dbc`'s 208
//! rows). Restricting to the 1433 kits actually reachable from a live `spell_template` visual
//! chain doesn't change the picture (692 are `-1` vs 26 `0` vs 715 real) — the `-1` form is the
//! majority for **impact**-stage kits specifically (966/1200: most magic impacts carry no body
//! reaction) while **cast**-stage kits are majority real (1168/1347: most casts do animate).
//! `sound` (field 13) shows the same dual encoding on a handful of rows (5 kits carry
//! `0xFFFFFFFF` alongside 647 carrying plain `0`). [`VisualKit::anim_id`]/[`VisualKit::sound`]
//! fold both forms to `None` so no consumer ever sees the raw sentinels.
//!
//! **Verified chain (Fireball, spell 133 → visual 67):** precast 30 / cast 38 / impact 286 /
//! state 0 / channel 0; kit 38 (cast) → anim 53 (`AnimationData` "SpellCastDirected") / sound
//! 1484; kit 286 (impact) → anim 9 (`AnimationData` "CombatWound" — a hit-reaction anim on the
//! target, corroborating decision 0107's wound-flinch ids 8–10) / sound 1507.

use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, str_at, u32_at};
use crate::Chain;

const SPELL_VISUAL: &str = "DBFilesClient\\SpellVisual.dbc";
const SPELL_VISUAL_KIT: &str = "DBFilesClient\\SpellVisualKit.dbc";
const SPELL_VISUAL_EFFECT_NAME: &str = "DBFilesClient\\SpellVisualEffectName.dbc";

const SPELL_VISUAL_FIELDS: usize = 16;
const SPELL_VISUAL_KIT_FIELDS: usize = 35;

/// The nine kit emitter slots' attach tags, **in kit-field order** (fields 3–11 ↔ this array's
/// indices 0–8). Byte-pinned compile-time immediates in the client's slot loop (wow-re
/// `spell-visual-apply.md` §1.3), each a **direct M2 `AttachmentID`**: Head(0x14), Chest(0x22),
/// Base(0x13), LeftHand(0x15), RightHand(0x16), Breath(0x11), Special1–3(0x17–0x19).
pub const KIT_SLOT_TAGS: [u16; 9] = [0x14, 0x22, 0x13, 0x15, 0x16, 0x11, 0x17, 0x18, 0x19];

/// The missile **destination-attachment** ordinal table — the client's `0x860a18`, dumped
/// byte-for-byte (wow-re `spell-visual-apply.md` §5, 11 live entries): [`KIT_SLOT_TAGS`] in
/// kit-field order plus `0xf`/`0x10` at ordinals 9/10. `SpellVisual` field 9 indexes here to the
/// M2 attach tag the missile homes to on a live target ([`VisualStages::missile_attach`]).
pub const MISSILE_ATTACH_TABLE: [u16; 11] = [
    0x14, 0x22, 0x13, 0x15, 0x16, 0x11, 0x17, 0x18, 0x19, 0xf, 0x10,
];

/// The DBC's alternate "no value" encoding for a foreign-key-ish column (module docs) — folded to
/// `None` alongside plain `0`.
const NONE_SENTINEL: u32 = u32::MAX;

/// One `SpellVisual.dbc` row: the five lifecycle-stage `SpellVisualKit` ids a cast may fire
/// through (`0` = no kit at that stage, this crate's usual absent-FK convention) + the missile
/// block's live columns (model, dest-attach, flight sound — module docs; phase 4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VisualStages {
    pub precast: u32,
    pub cast: u32,
    pub impact: u32,
    pub state: u32,
    pub channel: u32,
    /// Field 7: the projectile's `SpellVisualEffectName` id (`0` here covers the client's `<1`
    /// ammo-fallback gate — the raw column is never negative on the real table). The missile
    /// *exists* whenever `Spell.dbc` Speed > 0, model or not.
    pub missile_model: u32,
    /// Field 9: the destination-attachment ordinal — index [`MISSILE_ATTACH_TABLE`] for the M2
    /// attach tag the missile homes to on a live target.
    pub missile_attach: u32,
    /// Field 10: the `SoundEntries.dbc` id the missile sounds while it travels — a LOOPING whoosh
    /// on the thrown/ranged projectiles (the thrown dagger's `WeaponLoop.wav`, id 3318), the
    /// client's per-missile loop handle (`CMissile+0x44`, wow-re `w2f1.md`; its volume is proximity
    /// -shaped by `0x61d790`). `None` = the row names no flight sound. Rung by
    /// `crate::entities::missile`, stopped when the projectile arrives.
    pub missile_sound: Option<u32>,
    /// Field 14: the `SoundEntries.dbc` id the `$TRD` anim event rings at the work/craft strike
    /// keyframe — the client's `$TRD` handler `0x62faa0` resolves the in-flight spell's visual to
    /// this 16-dword row and plays its dword 14 (`[row+0x38]`, wow-re
    /// `sound/scratch/gather-sound-anim-events.md`; decision 0562). Mining's visual 93 carries
    /// 1143 "Mining Impact" (the pick clang), Herb's 91 carries 1142, the smithing crafts' 395
    /// carries 1143 (the hammer). `None` = no strike sound.
    pub strike_sound: Option<u32>,
}

/// One `SpellVisualKit.dbc` row's body-animation + sound + the nine attach-point emitter slots
/// (the columns consumed through decision 0099 phase 3; see the module doc for what remains).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VisualKit {
    /// The `AnimationData.dbc` id this kit plays on the unit (`None` = no animation for this kit
    /// — see the module doc's none-sentinel finding; common on impact kits).
    pub anim_id: Option<u16>,
    /// The `SoundEntries.dbc` kit id this stage rings (`None` = silent).
    pub sound: Option<u32>,
    /// The nine emitter slots (kit fields 3–11): each a `SpellVisualEffectName` id, in kit-field
    /// order — slot `i` attaches at [`KIT_SLOT_TAGS`]`[i]`. Both none-sentinels fold to `None`
    /// like [`Self::anim_id`]. Iterate populated slots via [`Self::effects`].
    pub effect_slots: [Option<u32>; 9],
}

impl VisualKit {
    /// The populated emitter slots as `(M2 attachment id, SpellVisualEffectName id)` pairs, in
    /// kit-field order — the client fires **all** populated slots at **every** stage (stage sets
    /// lifetime policy only, wow-re `spell-visual-apply.md` §1.3).
    pub fn effects(&self) -> impl Iterator<Item = (u16, u32)> + '_ {
        self.effect_slots
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.map(|id| (KIT_SLOT_TAGS[i], id)))
    }
}

/// `SpellVisual.dbc` × `SpellVisualKit.dbc` × `SpellVisualEffectName.dbc`, each kept in its own
/// id space (three different tables' keys — never conflate them, mirroring
/// [`crate::AreaSoundCatalog`]'s per-table map split).
pub struct SpellVisualCatalog {
    visuals: HashMap<u32, VisualStages>,
    kits: HashMap<u32, VisualKit>,
    /// `SpellVisualEffectName` id → the effect model's `.mdx` path (field 2 — the table's one
    /// column the kit slots consume).
    effect_paths: HashMap<u32, String>,
    /// The `"HARDCODED *"`-named rows, name → path — the client's engine-spawned effect set,
    /// resolved BY NAME once at boot exactly like this (`0x61f5b0` matches a 14-string baked
    /// table against the name column: loot art, footsteps, breath, level-up…; wow-re
    /// `loot-corpse-effect.md` + `levelup-ding.md`). Two consumers today: "HARDCODED Loot Art"
    /// (id 14 → `Particles\LootFX.mdl`, the corpse sparkle) and "HARDCODED Unit Level Up"
    /// (id 21 → `Spells\LevelUp\LevelUp.mdl`, the ding).
    hardcoded: HashMap<String, String>,
}

impl SpellVisualCatalog {
    /// Build a catalog from explicit visual/kit tables — for tests and synthetic fixtures (the
    /// [`crate::SpellCatalog::from_displays`] convention). The live path is
    /// [`load_spell_visual_catalog`]. Carries no effect paths and no hardcoded rows.
    pub fn from_tables(visuals: HashMap<u32, VisualStages>, kits: HashMap<u32, VisualKit>) -> Self {
        Self::from_tables_with_paths(visuals, kits, HashMap::new())
    }

    /// [`Self::from_tables`] plus effect paths, for fixtures exercising the kit → effect-model
    /// chain. The live path is [`load_spell_visual_catalog`].
    pub fn from_tables_with_paths(
        visuals: HashMap<u32, VisualStages>,
        kits: HashMap<u32, VisualKit>,
        effect_paths: HashMap<u32, String>,
    ) -> Self {
        Self {
            visuals,
            kits,
            effect_paths,
            hardcoded: HashMap::new(),
        }
    }

    /// A `SpellVisual.dbc` row's stage kits by its id (`Spell.dbc` column 115, `spells::SpellDisplay::visual`).
    pub fn stages(&self, visual_id: u32) -> Option<&VisualStages> {
        self.visuals.get(&visual_id)
    }

    /// A `SpellVisualKit.dbc` row by its id (one of [`VisualStages`]'s five fields).
    pub fn kit(&self, kit_id: u32) -> Option<&VisualKit> {
        self.kits.get(&kit_id)
    }

    /// The number of `SpellVisual` rows loaded.
    pub fn len(&self) -> usize {
        self.visuals.len()
    }

    pub fn is_empty(&self) -> bool {
        self.visuals.is_empty()
    }

    /// The number of `SpellVisualKit` rows loaded.
    pub fn kit_len(&self) -> usize {
        self.kits.len()
    }

    /// A `SpellVisualEffectName` id's effect-model `.mdx` path (one of a kit's
    /// [`VisualKit::effects`] slot values). `None` for an unknown id or an empty path.
    pub fn effect_path(&self, effect_id: u32) -> Option<&str> {
        self.effect_paths
            .get(&effect_id)
            .map(String::as_str)
            .filter(|p| !p.is_empty())
    }

    /// An engine-spawned hardcoded effect's model path, by the client's own baked name string
    /// ([`SpellVisualCatalog::hardcoded`]). Mirrors the client's boot name-resolve
    /// (`0x61f5b0`, stricmp-family — hence the case-insensitive compare over the tiny set);
    /// `None` when the shipped table names no such row.
    pub fn hardcoded_effect(&self, name: &str) -> Option<&str> {
        self.hardcoded
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .filter(|p| !p.is_empty())
    }

    /// The `"HARDCODED Loot Art"` row's model path — the lootable-corpse sparkle, riding the
    /// shared hardcoded map (one name-resolve mechanism for the whole engine-spawned set).
    pub fn loot_art_path(&self) -> Option<&str> {
        self.hardcoded_effect("HARDCODED Loot Art")
    }
}

fn n_u32_schema(name: &str, fields: usize) -> Schema {
    let mut s = Schema::new(name);
    for i in 0..fields {
        s.add_field(SchemaField::new(format!("F{i}"), FieldType::UInt32));
    }
    s
}

/// `SpellVisualEffectName`'s 5-field schema: id · name (string — the HARDCODED-effect lookup
/// key) · model `.mdx` path (string) · two dead columns (module docs).
fn effect_name_schema() -> Schema {
    let mut s = Schema::new("SpellVisualEffectName");
    for i in 0..5 {
        let ty = if i == 1 || i == 2 {
            FieldType::String
        } else {
            FieldType::UInt32
        };
        s.add_field(SchemaField::new(format!("F{i}"), ty));
    }
    s
}

/// Fold the DBC's dual none-encoding (module docs) to `Option<u32>`.
fn some_unless_none(v: u32) -> Option<u32> {
    (v != 0 && v != NONE_SENTINEL).then_some(v)
}

/// Read `SpellVisual.dbc` + `SpellVisualKit.dbc` off the patch chain.
pub fn load_spell_visual_catalog(chain: &mut Chain) -> Result<SpellVisualCatalog> {
    let sv_bytes = chain
        .read_file(SPELL_VISUAL)
        .context("reading SpellVisual.dbc")?;
    let sv_set = parse(
        &sv_bytes,
        n_u32_schema("SpellVisual", SPELL_VISUAL_FIELDS),
        "SpellVisual.dbc",
    )?;
    let mut visuals = HashMap::with_capacity(sv_set.records().len());
    for r in sv_set.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        let g = |i: usize| u32_at(r, i).unwrap_or(0);
        visuals.insert(
            id,
            VisualStages {
                precast: g(1),
                cast: g(2),
                impact: g(3),
                state: g(4),
                channel: g(5),
                missile_model: g(7),
                missile_attach: g(9),
                missile_sound: u32_at(r, 10).and_then(some_unless_none),
                strike_sound: u32_at(r, 14).and_then(some_unless_none),
            },
        );
    }

    let svk_bytes = chain
        .read_file(SPELL_VISUAL_KIT)
        .context("reading SpellVisualKit.dbc")?;
    let svk_set = parse(
        &svk_bytes,
        n_u32_schema("SpellVisualKit", SPELL_VISUAL_KIT_FIELDS),
        "SpellVisualKit.dbc",
    )?;
    let mut kits = HashMap::with_capacity(svk_set.records().len());
    for r in svk_set.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        let anim_id = u32_at(r, 2).and_then(some_unless_none).map(|a| a as u16);
        let sound = u32_at(r, 13).and_then(some_unless_none);
        let mut effect_slots = [None; 9];
        for (i, slot) in effect_slots.iter_mut().enumerate() {
            *slot = u32_at(r, 3 + i).and_then(some_unless_none);
        }
        kits.insert(
            id,
            VisualKit {
                anim_id,
                sound,
                effect_slots,
            },
        );
    }

    let sven_bytes = chain
        .read_file(SPELL_VISUAL_EFFECT_NAME)
        .context("reading SpellVisualEffectName.dbc")?;
    let sven_set = parse(
        &sven_bytes,
        effect_name_schema(),
        "SpellVisualEffectName.dbc",
    )?;
    let mut effect_paths = HashMap::with_capacity(sven_set.records().len());
    let mut hardcoded = HashMap::new();
    for r in sven_set.records() {
        let Some(id) = u32_at(r, 0) else { continue };
        if let Some(path) = str_at(&sven_set, r, 2) {
            // The engine-spawned set is looked up by the name column — keep exactly the rows
            // the client's own boot name-resolve can hit (the "HARDCODED " prefix; its
            // matchers are stricmp-family, so lookups compare case-insensitively).
            if let Some(name) = str_at(&sven_set, r, 1).filter(|n| n.starts_with("HARDCODED ")) {
                hardcoded.insert(name, path.clone());
            }
            effect_paths.insert(id, path);
        }
    }

    Ok(SpellVisualCatalog {
        visuals,
        kits,
        effect_paths,
        hardcoded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal synthetic WDBC (20-byte header + fixed-width u32 records, no strings — both
    /// tables are all-`u32`) — the same shape `benilla-dbc`'s own tests build (`items.rs`
    /// reproduces it too), so this adapter is testable without a real client install.
    fn build_wdbc(record_count: u32, field_count: u32, records: &[u8]) -> Vec<u8> {
        let record_size = field_count * 4;
        assert_eq!(records.len(), (record_count * record_size) as usize);
        let mut b = Vec::new();
        b.extend_from_slice(b"WDBC");
        b.extend_from_slice(&record_count.to_le_bytes());
        b.extend_from_slice(&field_count.to_le_bytes());
        b.extend_from_slice(&record_size.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // empty string block
        b.extend_from_slice(records);
        b
    }

    fn u32le(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }

    /// One `SpellVisual` row: id, the five stage kits, then the missile block — field 7 (model
    /// effect id) and field 9 (dest-attach ordinal) live, the rest zeroed to fill 16 fields.
    #[allow(clippy::too_many_arguments)]
    fn spell_visual_row(
        id: u32,
        precast: u32,
        cast: u32,
        impact: u32,
        state: u32,
        channel: u32,
        missile_model: u32,
        missile_attach: u32,
    ) -> Vec<u8> {
        let mut rec = Vec::new();
        for v in [id, precast, cast, impact, state, channel] {
            rec.extend(u32le(v));
        }
        for f in 6..SPELL_VISUAL_FIELDS {
            rec.extend(u32le(match f {
                7 => missile_model,
                9 => missile_attach,
                _ => 0,
            }));
        }
        rec
    }

    /// One `SpellVisualKit` row: id, an unpinned field 1, anim (field 2), nine zeroed emitter
    /// slots (3..12), sound (field 13), then zeroed trailing columns to fill 35 fields.
    fn spell_visual_kit_row(id: u32, anim: u32, sound: u32) -> Vec<u8> {
        let mut rec = Vec::new();
        rec.extend(u32le(id));
        rec.extend(u32le(0)); // field 1, unpinned
        rec.extend(u32le(anim)); // field 2
        for _ in 3..13 {
            rec.extend(u32le(0)); // fields 3..12 (9 emitter slots + missile)
        }
        rec.extend(u32le(sound)); // field 13
        for _ in 14..SPELL_VISUAL_KIT_FIELDS {
            rec.extend(u32le(0));
        }
        rec
    }

    #[test]
    fn header_shape_matches_the_pinned_layout() {
        // A single-row file of each still has to satisfy the pinned field/record-size shape —
        // exercises the schema's own field-count check against a header lying about it would be
        // a separate (missing) test; this one just guards our row builders stay in lock-step with
        // SPELL_VISUAL_FIELDS/SPELL_VISUAL_KIT_FIELDS.
        let sv = spell_visual_row(1, 0, 0, 0, 0, 0, 0, 0);
        assert_eq!(sv.len(), SPELL_VISUAL_FIELDS * 4, "64B record");
        let svk = spell_visual_kit_row(1, 0, 0);
        assert_eq!(svk.len(), SPELL_VISUAL_KIT_FIELDS * 4, "140B record");
    }

    #[test]
    fn parses_stage_kits_and_zero_means_no_kit_at_that_stage() {
        let mut records = Vec::new();
        records.extend(spell_visual_row(67, 30, 38, 286, 0, 0, 365, 1)); // Fireball's visual row
        records.extend(spell_visual_row(1, 0, 0, 0, 0, 0, 0, 0)); // an all-silent row
        let bytes = build_wdbc(2, SPELL_VISUAL_FIELDS as u32, &records);
        let rs = parse(
            &bytes,
            n_u32_schema("SpellVisual", SPELL_VISUAL_FIELDS),
            "t",
        )
        .unwrap();

        let mut visuals = HashMap::new();
        for r in rs.records() {
            let id = u32_at(r, 0).unwrap();
            let g = |i: usize| u32_at(r, i).unwrap_or(0);
            visuals.insert(
                id,
                VisualStages {
                    precast: g(1),
                    cast: g(2),
                    impact: g(3),
                    state: g(4),
                    channel: g(5),
                    missile_model: g(7),
                    missile_attach: g(9),
                    missile_sound: u32_at(r, 10).and_then(some_unless_none),
                    strike_sound: u32_at(r, 14).and_then(some_unless_none),
                },
            );
        }
        assert_eq!(
            visuals[&67],
            VisualStages {
                precast: 30,
                cast: 38,
                impact: 286,
                state: 0,
                channel: 0,
                missile_model: 365,
                missile_attach: 1,
                missile_sound: None,
                strike_sound: None,
            }
        );
        assert_eq!(
            visuals[&1],
            VisualStages::default(),
            "an all-zero row is all-silent"
        );
    }

    #[test]
    fn kit_anim_and_sound_fold_both_none_sentinels_to_none() {
        let mut records = Vec::new();
        records.extend(spell_visual_kit_row(38, 53, 1484)); // Fireball's cast kit
        records.extend(spell_visual_kit_row(1, 0, 0)); // plain-zero "none"
        records.extend(spell_visual_kit_row(2, u32::MAX, u32::MAX)); // the -1 "none" form
        let bytes = build_wdbc(3, SPELL_VISUAL_KIT_FIELDS as u32, &records);
        let rs = parse(
            &bytes,
            n_u32_schema("SpellVisualKit", SPELL_VISUAL_KIT_FIELDS),
            "t",
        )
        .unwrap();

        let mut kits = HashMap::new();
        for r in rs.records() {
            let id = u32_at(r, 0).unwrap();
            let anim_id = u32_at(r, 2).and_then(some_unless_none).map(|a| a as u16);
            let sound = u32_at(r, 13).and_then(some_unless_none);
            kits.insert(
                id,
                VisualKit {
                    anim_id,
                    sound,
                    ..Default::default()
                },
            );
        }
        assert_eq!(
            kits[&38],
            VisualKit {
                anim_id: Some(53),
                sound: Some(1484),
                ..Default::default()
            }
        );
        assert_eq!(kits[&1], VisualKit::default(), "plain 0 = none");
        assert_eq!(kits[&2], VisualKit::default(), "0xFFFFFFFF = none too");
    }

    /// The emitter-slot surface: kit-field order maps to [`KIT_SLOT_TAGS`], both none-sentinels
    /// fold out, and `effects()` yields only the populated pairs.
    #[test]
    fn kit_effect_slots_pair_with_their_attach_tags() {
        let kit = VisualKit {
            // Fireball's precast shape: LeftHand (slot index 3) + RightHand (slot index 4).
            effect_slots: [
                None,
                None,
                None,
                Some(287),
                Some(287),
                None,
                None,
                None,
                None,
            ],
            ..Default::default()
        };
        assert_eq!(
            kit.effects().collect::<Vec<_>>(),
            vec![(0x15, 287), (0x16, 287)],
            "LeftHand then RightHand, kit-field order"
        );
        assert_eq!(VisualKit::default().effects().count(), 0);
    }

    /// The repo root's `WoW/Data` (gitignored; the real-data tests skip when absent) — this
    /// crate's established gate (`anim_data.rs`, `spell_catalog.rs`, …).
    fn vanilla_data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
    }

    /// End-to-end on the real build-5875 tables: the byte-verified header shape (2165×16/64B ·
    /// 1772×35/140B) and the full Fireball chain (module doc's "Verified chain") — a schema drift
    /// or column slip fails loudly. Skips without client data.
    #[test]
    fn real_spell_visual_chain_resolves_fireball() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_spell_visual_catalog(&mut chain).expect("load SpellVisual/SpellVisualKit");
        assert_eq!(cat.len(), 2165, "all 5875 SpellVisual rows load");
        assert_eq!(cat.kit_len(), 1772, "all 5875 SpellVisualKit rows load");

        // Fireball (spell 133) → visual 67 (spells.rs's column-115 pin).
        let stages = cat.stages(67).expect("Fireball's SpellVisual row");
        assert_eq!(
            *stages,
            VisualStages {
                precast: 30,
                cast: 38,
                impact: 286,
                state: 0,
                channel: 0,
                missile_model: 365,
                missile_attach: 1,
                // Field 10: the fireball's in-flight loop (SoundEntries 3011 → FireMissileLoop.wav).
                missile_sound: Some(3011),
                strike_sound: None,
            }
        );
        // The gathering/work strike sounds (decision 0562): Mining's visual 93 carries the pick
        // clang in field 14 (SoundEntries 1143 "Mining Impact" = MiningHitA-E), Herb's 91 the
        // search rustle (1142) - the $TRD anim event's operands.
        assert_eq!(
            cat.stages(93).and_then(|s| s.strike_sound),
            Some(1143),
            "Mining's visual carries the field-14 strike clang"
        );
        assert_eq!(
            cat.stages(91).and_then(|s| s.strike_sound),
            Some(1142),
            "Herb Gathering's visual carries the field-14 rustle"
        );

        // A basic thrown attack borrows the equipped weapon's substitute visual (98 — the thrown
        // dagger's ItemDisplayInfo col 10): no missile MODEL (it flies the weapon itself) but a
        // flight loop in field 10 (SoundEntries 3318 → WeaponLoop.wav) — the whoosh the projectile
        // carries while it travels.
        let thrown = cat
            .stages(98)
            .expect("the thrown-weapon substitute SpellVisual");
        assert_eq!(
            thrown.missile_model, 0,
            "a thrown weapon flies its own model, not an effect"
        );
        assert_eq!(
            thrown.missile_sound,
            Some(3318),
            "the thrown weapon's flight loop"
        );
        // The missile chain (phase 4): field 7 → the projectile's own model, field 9 → the
        // chest attach the missile homes to (ordinal 1 → 0x22).
        assert_eq!(
            cat.effect_path(stages.missile_model),
            Some("Spells\\Fireball_Missile_Low.mdx"),
            "the flying fireball's model"
        );
        assert_eq!(
            MISSILE_ATTACH_TABLE[stages.missile_attach as usize], 0x22,
            "Fireball homes onto the target's chest"
        );

        // The engine-spawned hardcoded set resolves by the client's own baked names — the ding
        // (row 21, byte-verified `0x61f5b0`/`0x8618e0`, decision 0304's §5 fold-back).
        assert_eq!(
            cat.hardcoded_effect("HARDCODED Unit Level Up"),
            Some("Spells\\LevelUp\\LevelUp.mdl"),
            "the level-up pillar resolves by name"
        );
        assert!(
            cat.hardcoded_effect("HARDCODED Loot Art").is_some(),
            "the sibling hardcoded rows ride the same map"
        );

        let cast_kit = cat.kit(stages.cast).expect("cast kit");
        assert_eq!(
            cast_kit.anim_id,
            Some(53),
            "AnimationData id 53 = SpellCastDirected"
        );
        assert_eq!(cast_kit.sound, Some(1484));

        let impact_kit = cat.kit(stages.impact).expect("impact kit");
        assert_eq!(
            impact_kit.anim_id,
            Some(9),
            "AnimationData id 9 = CombatWound, the target's hit reaction"
        );
        assert_eq!(impact_kit.sound, Some(1507));

        // The precast kit's emitter slots (phase 3): effect 287 on both hands, and its
        // SpellVisualEffectName path — the glowing-hands chain end to end.
        let precast_kit = cat.kit(stages.precast).expect("precast kit");
        assert_eq!(
            precast_kit.effects().collect::<Vec<_>>(),
            vec![(0x15, 287), (0x16, 287)],
            "Fire_Precast_Hand on LeftHand + RightHand"
        );
        assert_eq!(
            cat.effect_path(287),
            Some("Spells\\Fire_Precast_Hand.mdx"),
            "SpellVisualEffectName field 2 = the effect model path"
        );
        // The impact kit's chest burst — the phase-4 arrival hand-off will play this.
        assert_eq!(
            cat.kit(stages.impact)
                .unwrap()
                .effects()
                .collect::<Vec<_>>(),
            vec![(0x22, 321)],
            "MoltenBlast_Impact_Chest on the Chest attach"
        );
    }

    /// The real 5875 `SpellVisualEffectName`: the boot-time HARDCODED name matcher's one consumed
    /// row — `"HARDCODED Loot Art"` is id 14 → `Particles\LootFX.mdl` (values pre-checked against
    /// the raw DBC bytes), and the `.m2` it names ships in the chain, so the lootable-corpse
    /// sparkle can actually load (wow-re `loot-corpse-effect.md`). Skips without client data.
    #[test]
    fn real_effect_name_table_resolves_the_loot_art_row() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_spell_visual_catalog(&mut chain).expect("load the visual catalog");
        assert_eq!(cat.loot_art_path(), Some("Particles\\LootFX.mdl"));
        // The model the row names ships in the chain (consumers rewrite .mdl → .m2 to load it).
        assert!(
            chain.read_file("Particles\\LootFX.m2").is_ok(),
            "LootFX.m2 must ship in the chain"
        );
    }
}
