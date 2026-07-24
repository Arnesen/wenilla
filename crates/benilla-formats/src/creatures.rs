//! Creature display resolution: `displayId` → M2 model + skin textures + scale.
//!
//! An NPC's `UNIT_FIELD_DISPLAYID` indexes **CreatureDisplayInfo.dbc**, which gives a `ModelID`
//! (into **CreatureModelData.dbc**, the `.mdx` path), a per-display scale, and up to three skin
//! texture names. Creature M2s leave their `Monster1/2/3` texture slots blank and pull the skin
//! from these names — the texture lives **in the same directory as the model** (wowdev.wiki). The
//! effective render scale is `CreatureModelData.modelScale * CreatureDisplayInfo.creatureModelScale`.
//!
//! **Character-model NPCs.** A humanoid NPC (guard, questgiver, townsfolk) uses a `Character\…` body
//! M2 — the same model a player wears — but its appearance is *not* on the wire (as a player's is);
//! it lives in **CreatureDisplayInfoExtra.dbc**, reached via `CreatureDisplayInfo.ExtendedDisplayInfoID`
//! (0 for a plain beast). That row supplies race/sex + the customization selectors and, in field 18, a
//! **bake name** — a pre-composited body atlas the client ships under `Textures\BakedNpcTextures\` and
//! loads directly (rather than compositing live like the local player). We surface it as
//! [`NpcAppearance`]; the beast skin path ([`CreatureModel::textures`]) is untouched.
//!
//! Layouts verified against build 5875 (field counts from the file header, cross-checked with
//! wowdev.wiki + vmangos `DBCStructure.h`): CreatureModelData = 16 fields (ID@0, ModelName@2,
//! ModelScale@4); CreatureDisplayInfo = 12 fields (ID@0, ModelID@1, ExtendedDisplayInfoID@3, Scale@4,
//! TextureVariation@6/7/8); CreatureDisplayInfoExtra = 19 fields (ID@0, Race@1, Sex@2, Skin@3, Face@4,
//! HairStyle@5, HairColor@6, FacialHair@7, Equipment@8..17, BakeName@18).

use std::collections::HashMap;

use crate::Chain;
use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{f32_at, parse, str_at, u32_at};

const CREATURE_MODEL_DATA: &str = "DBFilesClient\\CreatureModelData.dbc";
const CREATURE_DISPLAY_INFO: &str = "DBFilesClient\\CreatureDisplayInfo.dbc";
const CREATURE_DISPLAY_INFO_EXTRA: &str = "DBFilesClient\\CreatureDisplayInfoExtra.dbc";

/// A resolved creature: model path + effective scale + its (up to three) skin texture names, plus —
/// for a character-model NPC — the [`NpcAppearance`] that skins its body.
#[derive(Debug, Clone)]
pub struct CreatureModel {
    /// `.mdx` path (the M2 loader normalizes to `.m2`), e.g. `Creature\Basilisk\Basilisk.mdx`.
    pub model_path: String,
    /// `CreatureModelData.modelScale * CreatureDisplayInfo.creatureModelScale`.
    pub scale: f32,
    /// `textureVariation[0..2]` — bare names (no dir/extension); `None` where empty. The renderer
    /// resolves a used one to `<dir-of-model_path>\<name>.blp` for the model's `Monster1/2/3` slots.
    /// For a character-model NPC the body skin comes from [`Self::npc_appearance`]'s baked atlas
    /// regardless; these slots are empty on ~98% of such rows and unused for the body on the rest
    /// (wow-re-confirmed — a `Monster`-slot binding on a character M2 is a separate, untraced mechanism).
    pub textures: [Option<String>; 3],
    /// A character-model NPC's body appearance (from CreatureDisplayInfoExtra, via the display's
    /// `ExtendedDisplayInfoID`). `None` for a plain beast/monster (ExtendedDisplayInfoID 0) — those
    /// skin from [`Self::textures`], not here.
    pub npc_appearance: Option<NpcAppearance>,
    /// The resolved **UnitBloodLevels** key for the melee blood spurt (decision 0137 phase 3,
    /// wow-re `melee-blood-spurt.md`): `CreatureDisplayInfo.BloodLevel` when nonzero, else
    /// `CreatureModelData.BloodID`; `≤ 0` = a bloodless model (elementals, ghosts — the client's
    /// negative-id skip). Consumed via [`crate::BloodCatalog::effect_id`].
    pub blood: i32,
}

/// A character-model NPC's appearance, from **CreatureDisplayInfoExtra.dbc**. A humanoid NPC wears a
/// `Character\…` body M2 whose skin is not on the wire; this carries what the client needs to render
/// it: race/sex + the customization selectors (for the hair mesh + the geoset selection), the ten worn
/// **equipment** display ids (the armor geosets + the helm/shoulder attach models), and a `bake_name`
/// — the pre-composited body atlas the client ships under `Textures\BakedNpcTextures\` and loads
/// directly. `skin`/`face` are already baked into that atlas; they're kept for completeness and for the
/// live-composite fallback when a row carries no bake name.
#[derive(Debug, Clone)]
pub struct NpcAppearance {
    pub race: u8,
    pub sex: u8,
    pub skin: u8,
    pub face: u8,
    pub hair_style: u8,
    pub hair_color: u8,
    pub facial_hair: u8,
    /// The ten worn-equipment `ItemDisplayInfo` display ids (fields 8..17), **bodyslot-indexed**:
    /// `0` head · `1` shoulder · `2` shirt · `3` chest · `4` belt · `5` pants · `6` boots · `7` wrist ·
    /// `8` gloves · `9` tabard (no cloak column — the row stops at bodyslot 9). `0` = the slot is
    /// empty. Direct display ids (not item entries — no template round-trip): the head/shoulder ids
    /// drive the helm/pauldron attach sub-models and the shirt..tabard ids drive the equipment geosets,
    /// through the same `ItemDisplayInfo` catalog + geoset machinery the player wire path uses.
    pub equipment: [u32; 10],
    /// The pre-baked body-atlas file name (bare, no dir) under `Textures\BakedNpcTextures\`; `None`
    /// when field 18 is empty (then the body composites live from the fields above, like a player).
    pub bake_name: Option<String>,
}

/// One CreatureDisplayInfo row (the parts we use).
#[derive(Debug, Clone)]
struct DisplayRow {
    model_id: u32,
    /// `ExtendedDisplayInfoID` — the CreatureDisplayInfoExtra key for a character-model NPC; 0 = none.
    extended_id: u32,
    scale: f32,
    textures: [Option<String>; 3],
    /// `BloodLevel` (field 10) — a per-display UnitBloodLevels override; 0 = fall through to the
    /// model's `BloodID`.
    blood_level: u32,
}

/// Display/model tables loaded from the DBCs, resolving `displayId` → [`CreatureModel`].
pub struct CreatureCatalog {
    /// CreatureDisplayInfo: displayId → row.
    display: HashMap<u32, DisplayRow>,
    /// CreatureModelData: modelId → (model path, model scale, BloodID — see [`CreatureModel::blood`]).
    models: HashMap<u32, (String, f32, i32)>,
    /// CreatureDisplayInfoExtra: extendedDisplayInfoId → character-model NPC appearance.
    extra: HashMap<u32, NpcAppearance>,
}

impl CreatureCatalog {
    /// A display's own `creatureModelScale` column alone — the MOUNT scale law (byte-verified,
    /// wow-re `mount-composition.md` / `0x613ef0`: a rendered mount = `OBJECT_FIELD_SCALE_X ×
    /// CreatureDisplayInfo.creatureModelScale`; `CreatureModelData.modelScale` does NOT multiply
    /// in, unlike [`CreatureModel::scale`]'s spawned-creature product). `None` when the display
    /// id misses.
    pub fn display_scale(&self, display_id: u32) -> Option<f32> {
        self.display.get(&display_id).map(|r| r.scale)
    }

    /// Resolve an NPC display id to its model, or `None` if either DBC lookup misses.
    pub fn model(&self, display_id: u32) -> Option<CreatureModel> {
        let row = self.display.get(&display_id)?;
        let (model_path, model_scale, model_blood) = self.models.get(&row.model_id)?;
        // A non-zero ExtendedDisplayInfoID that resolves to an extra row ⇒ a character-model NPC.
        let npc_appearance = (row.extended_id != 0)
            .then(|| self.extra.get(&row.extended_id).cloned())
            .flatten();
        Some(CreatureModel {
            model_path: model_path.clone(),
            scale: model_scale * row.scale,
            textures: row.textures.clone(),
            npc_appearance,
            blood: if row.blood_level != 0 {
                row.blood_level as i32
            } else {
                *model_blood
            },
        })
    }

    /// Number of display entries (for logging/diagnostics).
    pub fn len(&self) -> usize {
        self.display.len()
    }

    /// Whether the catalog has no display entries.
    pub fn is_empty(&self) -> bool {
        self.display.is_empty()
    }

    /// Number of character-model NPC appearance rows loaded (for logging/diagnostics). Zero here means
    /// CreatureDisplayInfoExtra failed to load ⇒ humanoid NPCs stay untextured.
    pub fn extra_len(&self) -> usize {
        self.extra.len()
    }
}

/// CreatureModelData.dbc — 16 fields in build 5875 (no `mountHeight`). We read ID, ModelName, scale.
fn creature_model_data_schema() -> Schema {
    let mut s = Schema::new("CreatureModelData");
    for (name, ty) in [
        ("ID", FieldType::UInt32),
        ("Flags", FieldType::UInt32),
        ("ModelName", FieldType::String),
        ("SizeClass", FieldType::UInt32),
        ("ModelScale", FieldType::Float32),
        ("BloodID", FieldType::UInt32),
        ("FootprintTextureID", FieldType::UInt32),
        ("FootprintTextureLength", FieldType::Float32),
        ("FootprintTextureWidth", FieldType::Float32),
        ("FootprintParticleScale", FieldType::Float32),
        ("FoleyMaterialID", FieldType::UInt32),
        ("FootstepShakeSize", FieldType::UInt32),
        ("DeathThudShakeSize", FieldType::UInt32),
        ("SoundID", FieldType::UInt32),
        ("CollisionWidth", FieldType::Float32),
        ("CollisionHeight", FieldType::Float32),
    ] {
        s.add_field(SchemaField::new(name, ty));
    }
    s
}

/// CreatureDisplayInfo.dbc — 12 fields in build 5875. We read ID, ModelID, scale, 3 skin textures.
fn creature_display_info_schema() -> Schema {
    let mut s = Schema::new("CreatureDisplayInfo");
    for (name, ty) in [
        ("ID", FieldType::UInt32),
        ("ModelID", FieldType::UInt32),
        ("SoundID", FieldType::UInt32),
        ("ExtendedDisplayInfoID", FieldType::UInt32),
        ("CreatureModelScale", FieldType::Float32),
        ("CreatureModelAlpha", FieldType::UInt32),
        ("TextureVariation0", FieldType::String),
        ("TextureVariation1", FieldType::String),
        ("TextureVariation2", FieldType::String),
        ("PortraitTextureName", FieldType::String),
        ("BloodLevel", FieldType::UInt32),
        // Labeled BloodID in some third-party maps, but the 5875 values (33..188, dense) are the
        // NPC sound-kit range, not blood ids — the blood override is BloodLevel above.
        ("NPCSoundID", FieldType::UInt32),
    ] {
        s.add_field(SchemaField::new(name, ty));
    }
    s
}

/// CreatureDisplayInfoExtra.dbc — 19 fields in build 5875 (`19 × 4 == 76`-byte records; field map
/// cross-checked with vmangos `DBCStructure.h`). We read the appearance selectors, the 10 equipment
/// columns (8..17 — `ItemDisplayInfo` display ids for the worn armor geosets + helm/shoulder attach),
/// and the bake name.
fn creature_display_info_extra_schema() -> Schema {
    let mut s = Schema::new("CreatureDisplayInfoExtra");
    for (name, ty) in [
        ("ID", FieldType::UInt32),
        ("Race", FieldType::UInt32),
        ("Sex", FieldType::UInt32),
        ("SkinColor", FieldType::UInt32),
        ("FaceType", FieldType::UInt32),
        ("HairStyle", FieldType::UInt32),
        ("HairColor", FieldType::UInt32),
        ("FacialHair", FieldType::UInt32),
    ] {
        s.add_field(SchemaField::new(name, ty));
    }
    for i in 0..10 {
        s.add_field(SchemaField::new(format!("Equipment{i}"), FieldType::UInt32));
    }
    s.add_field(SchemaField::new("BakeName", FieldType::String));
    s
}

/// Load the creature DBCs from the patch chain into a [`CreatureCatalog`].
pub fn load_creature_catalog(chain: &mut Chain) -> Result<CreatureCatalog> {
    let models = {
        let bytes = chain
            .read_file(CREATURE_MODEL_DATA)
            .with_context(|| format!("reading {CREATURE_MODEL_DATA}"))?;
        let rs = parse(&bytes, creature_model_data_schema(), "CreatureModelData")?;
        let mut m = HashMap::with_capacity(rs.records().len());
        for r in rs.records() {
            if let (Some(id), Some(name)) = (u32_at(r, 0), str_at(&rs, r, 2)) {
                // BloodID (field 5) reads signed: −1 marks a bloodless model in the real data.
                let blood = u32_at(r, 5).map_or(0, |v| v as i32);
                m.insert(id, (name, f32_at(r, 4).unwrap_or(1.0), blood));
            }
        }
        m
    };

    let display = {
        let bytes = chain
            .read_file(CREATURE_DISPLAY_INFO)
            .with_context(|| format!("reading {CREATURE_DISPLAY_INFO}"))?;
        let rs = parse(
            &bytes,
            creature_display_info_schema(),
            "CreatureDisplayInfo",
        )?;
        let mut d = HashMap::with_capacity(rs.records().len());
        for r in rs.records() {
            if let (Some(id), Some(model_id)) = (u32_at(r, 0), u32_at(r, 1)) {
                d.insert(
                    id,
                    DisplayRow {
                        model_id,
                        extended_id: u32_at(r, 3).unwrap_or(0),
                        scale: f32_at(r, 4).unwrap_or(1.0),
                        textures: [str_at(&rs, r, 6), str_at(&rs, r, 7), str_at(&rs, r, 8)],
                        blood_level: u32_at(r, 10).unwrap_or(0),
                    },
                );
            }
        }
        d
    };

    // CreatureDisplayInfoExtra — the character-model NPC appearance table. Best-effort: a plain beast
    // catalog is still useful without it (only humanoid NPCs need it), so a load failure degrades to an
    // empty map (humanoid NPCs stay untextured) rather than sinking the whole catalog. The caller logs
    // `extra_len()` so a `0` (unexpected — it ships in patch.MPQ) is visible.
    let extra = load_creature_display_info_extra(chain).unwrap_or_default();

    Ok(CreatureCatalog {
        display,
        models,
        extra,
    })
}

/// Load CreatureDisplayInfoExtra.dbc → `extendedDisplayInfoId` → [`NpcAppearance`].
fn load_creature_display_info_extra(chain: &mut Chain) -> Result<HashMap<u32, NpcAppearance>> {
    let bytes = chain
        .read_file(CREATURE_DISPLAY_INFO_EXTRA)
        .with_context(|| format!("reading {CREATURE_DISPLAY_INFO_EXTRA}"))?;
    let rs = parse(
        &bytes,
        creature_display_info_extra_schema(),
        "CreatureDisplayInfoExtra",
    )?;
    let mut e = HashMap::with_capacity(rs.records().len());
    for r in rs.records() {
        if let Some(id) = u32_at(r, 0) {
            e.insert(
                id,
                NpcAppearance {
                    race: u32_at(r, 1).unwrap_or(0) as u8,
                    sex: u32_at(r, 2).unwrap_or(0) as u8,
                    skin: u32_at(r, 3).unwrap_or(0) as u8,
                    face: u32_at(r, 4).unwrap_or(0) as u8,
                    hair_style: u32_at(r, 5).unwrap_or(0) as u8,
                    hair_color: u32_at(r, 6).unwrap_or(0) as u8,
                    facial_hair: u32_at(r, 7).unwrap_or(0) as u8,
                    // fields 8..17 — the ten worn-equipment ItemDisplayInfo display ids, bodyslot-indexed
                    // (0 head · 1 shoulder · 2 shirt … 9 tabard); `0` = the slot is empty.
                    equipment: std::array::from_fn(|i| u32_at(r, 8 + i).unwrap_or(0)),
                    // field 18 — the baked body-atlas name.
                    bake_name: str_at(&rs, r, 18),
                },
            );
        }
    }
    Ok(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repo root's `WoW/Data` (gitignored; the real-data tests skip when absent).
    fn vanilla_data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
    }

    /// End-to-end on the **real** build-5875 DBCs: the `ExtendedDisplayInfoID` chain resolves
    /// character-model NPCs (guards/townsfolk) to a CreatureDisplayInfoExtra appearance whose body is a
    /// `Character\` M2 skinned by a pre-baked atlas that actually ships under `Textures\BakedNpcTextures\`.
    /// Guards the extra schema (a shifted column would misread the bake name), the display→extra join, and
    /// the baked-texture-path convention. Skips when the client data isn't present.
    #[test]
    fn character_model_npcs_resolve_a_shipped_baked_atlas() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_creature_catalog(&mut chain).expect("load creature catalog");
        assert!(
            cat.extra_len() > 1000,
            "CreatureDisplayInfoExtra loaded ({} rows)",
            cat.extra_len()
        );

        // Every display that resolves both an appearance and a bake name is definitively a character
        // model — confirm the body path + that its baked atlas is a real, readable file.
        let mut verified = 0;
        for (&disp, row) in &cat.display {
            if row.extended_id == 0 {
                continue;
            }
            let Some(m) = cat.model(disp) else { continue };
            let Some(bake) = m.npc_appearance.as_ref().and_then(|a| a.bake_name.as_ref()) else {
                continue;
            };
            assert!(
                m.model_path.to_ascii_lowercase().starts_with("character\\"),
                "an extended-display NPC wears a Character\\ body, got {}",
                m.model_path
            );
            let path = format!("Textures\\BakedNpcTextures\\{bake}");
            assert!(
                chain.read_file(&path).is_ok(),
                "the baked body atlas ships: {path}"
            );
            verified += 1;
            if verified >= 20 {
                break;
            }
        }
        assert!(
            verified >= 5,
            "found + verified several baked character-model NPCs (got {verified})"
        );
    }

    /// The worn-equipment columns (fields 8..17) decode in bodyslot order, anchored on live
    /// server-truth: the **Stormwind City Guard** (display 3167) carries a plate helm (slot 0), a
    /// pauldron pair (slot 1), and boot/glove/tabard geosets (slots 6/8/9), with empty chest/wrist
    /// (slots 3/7). These ids are the real `CreatureDisplayInfoExtra` values read off the build-5875
    /// DBC; a shifted column or a wrong field offset would misread them. Skips without the client data.
    #[test]
    fn stormwind_guard_equipment_columns_decode_in_bodyslot_order() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cat = load_creature_catalog(&mut chain).expect("load creature catalog");
        let guard = cat
            .model(3167)
            .expect("Stormwind City Guard display 3167 resolves");
        let npc = guard
            .npc_appearance
            .expect("display 3167 is a character-model NPC with an appearance row");
        // 0 head · 1 shoulder · 2 shirt · 3 chest · 4 belt · 5 pants · 6 boots · 7 wrist · 8 gloves · 9 tabard.
        assert_eq!(
            npc.equipment,
            [14964, 7541, 7223, 0, 7224, 7225, 7255, 0, 7698, 6255],
            "SW Guard worn-equipment ids in bodyslot order"
        );
    }
}
