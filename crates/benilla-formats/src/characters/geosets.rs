use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, u32_at};
use crate::Chain;

const CHAR_HAIR_GEOSETS: &str = "DBFilesClient\\CharHairGeosets.dbc";
const CHAR_FACIAL_HAIR: &str = "DBFilesClient\\CharacterFacialHairStyles.dbc";
const HELMET_GEOSET_VIS: &str = "DBFilesClient\\HelmetGeosetVisData.dbc";

/// The default 16 region-base geosets (`cc+0x144`, RF-0038) — increments of 100 (group 7's base is the
/// outlier 702). Entries 0–3 (hair + the 3 facial-hair groups) are overwritten by the customization;
/// 4–15 (the equipment-group bases: glove/boot/sleeve/…/cloak) stay as these bare-skin defaults.
const REGION_BASES: [u16; 16] = [
    1, 101, 201, 301, 401, 501, 601, 702, 801, 901, 1001, 1101, 1201, 1301, 1401, 1501,
];

/// The customization → geoset tables, resolving an appearance to the set of visible geosets.
pub struct CharacterGeosets {
    /// (race, sex, hairStyle) → hair `GeosetID` (group 0).
    hair: HashMap<(u8, u8, u8), u32>,
    /// (race, sex, facialHair) → the 3 facial-hair geoset variations (DBC fields gA/gB/gC, in file
    /// order — gA→group 1 (+100), gB→group 3 (+300), gC→group 2 (+200), per RF-0073's `0x478660`).
    facial: HashMap<(u8, u8, u8), [u32; 3]>,
    /// HelmetGeosetVisData row id → its 5 **race** bitmasks (VERIFIED wow-re RF-0083, consumer
    /// `0x4799a0`): a set bit `1 << race` in column *c* forces one region-base slot back to its
    /// group base — hiding the styled hair/facial geoset (and the ears) under a worn helm.
    helmet_vis: HashMap<u32, [u32; 5]>,
}

/// The worn geoset selectors the RF-0038 equipment branches read (decision 0074): per bodyslot 2–9
/// (shirt, chest, belt, pants, boots, wrist, gloves, tabard) the item's `geosetGroup[0..2]` (`None`
/// = the slot is empty), plus the cloak's `geosetGroup[0]`, plus the worn helm's
/// `HelmetGeosetVisData` row pair (`[male, female]` — ItemDisplayInfo cols 12/13). Default = naked.
#[derive(Default, Clone, Copy)]
pub struct EquipGeosets {
    pub bodyslots: [Option<[u32; 3]>; 8],
    pub cloak: Option<u32>,
    pub helm_vis: Option<[u32; 2]>,
}

impl CharacterGeosets {
    /// The geoset IDs a character of this appearance renders — RF-0038's opening block (the naked
    /// set) plus the equipment branches B1–B8 (decision 0074 — implemented from the transcription's
    /// *arithmetic*; its prose labels are known-mislabeled). A body submesh whose `skinSectionId` is
    /// in this set is drawn; all others hidden. A branch's gate is its `geosetGroup` field being
    /// non-zero ("section present"); the recorded disables are the ranges shown — the boot/kneepad
    /// enables deliberately leave their naked defaults on, as the client does.
    pub fn visible_geosets(
        &self,
        race: u8,
        sex: u8,
        hair_style: u8,
        facial_hair: u8,
        equip: &EquipGeosets,
    ) -> Vec<u16> {
        let mut set = REGION_BASES.to_vec();
        set.push(0); // geoset 0 (the body), enabled unconditionally
                     // Entry 0 (group 0 = hair): the chosen hairstyle's geoset, clamped ≥1 (`0x478540`
                     // returns `max(1, geosetId)`, so a bald style still resolves to geoset 1, the scalp).
        if let Some(&g) = self.hair.get(&(race, sex, hair_style)) {
            set[0] = g.max(1) as u16;
        }
        // Entries 1–3 (groups 1/2/3 = facial hair): DBC variation + the group base (`0x478660` adds
        // 100/300/200 to fields gA/gB/gC respectively). Absent row ⇒ the default bases 101/201/301 stand.
        if let Some(&[a, b, c]) = self.facial.get(&(race, sex, facial_hair)) {
            set[1] = (a + 100) as u16; // gA → group 1
            set[3] = (b + 300) as u16; // gB → group 3
            set[2] = (c + 200) as u16; // gC → group 2
        }
        // A worn helm's vis row (VERIFIED wow-re RF-0083, `0x4799a0`): each of the 5 columns is a
        // **race** bitmask; a set `1 << race` bit forces one region-base slot back to its group
        // base, dropping the styled geoset the customization just selected — hair to the bare scalp
        // (1), the facial groups to their bases, and the ears to **701** (over the 702 default no
        // customization touches). Runs after the customization overwrites, exactly the client's
        // order; the slots {0,1,2,3,7} are disjoint from every equipment branch below.
        if let Some(rows) = equip.helm_vis {
            let row = rows[usize::from(sex == 1)];
            if let Some(masks) = self.helmet_vis.get(&row) {
                const FORCED: [(usize, u16); 5] = [(0, 1), (1, 101), (2, 201), (3, 301), (7, 701)];
                let bit = 1u32 << (race & 0x1f);
                for (mask, (slot, forced)) in masks.iter().zip(FORCED) {
                    if mask & bit != 0 {
                        set[slot] = forced;
                    }
                }
            }
        }
        // The equipment branches. `g(slot, sub)` = the slot's geosetGroup[sub], gated non-zero.
        // EquipGeosets bodyslot indices: 0 shirt · 1 chest · 2 belt · 3 pants · 4 boots · 5 wrist ·
        // 6 gloves · 7 tabard.
        let g = |slot: usize, sub: usize| {
            equip.bodyslots[slot]
                .map(|groups| groups[sub])
                .filter(|v| *v != 0)
        };
        let disable = |set: &mut Vec<u16>, lo: u16, hi: u16| set.retain(|id| *id < lo || *id > hi);
        // The robe bit: chest or pants geosetGroup[2] (B4's guard) — it also suppresses the tabard
        // and pant-leg branches below.
        let robe = g(1, 2).or_else(|| g(3, 2));
        // B1: gloves replace the glove group (401+v, own range disabled); else the chest's sleeves.
        if let Some(v) = g(6, 0) {
            disable(&mut set, 401, 499);
            set.push(401 + v as u16);
        } else if let Some(v) = g(1, 0) {
            set.push(801 + v as u16);
        }
        // B3: shirt sleeves, only with no chest item over them.
        if equip.bodyslots[1].is_none() {
            if let Some(v) = g(0, 0) {
                set.push(801 + v as u16);
            }
        }
        // B4: a robe hides boots/kneepads/pant-legs/trousers and shows its skirt (1301+v); else
        // boots enable kneepad base + their boot geoset; else the pants' kneepads.
        if let Some(v) = robe {
            disable(&mut set, 501, 599);
            disable(&mut set, 902, 999);
            disable(&mut set, 1100, 1199);
            disable(&mut set, 1300, 1399);
            set.push(1301 + v as u16);
        } else if let Some(v) = g(4, 0) {
            set.push(501 + v as u16); // the naked 501 stays on, as recorded
        } else if let Some(v) = g(3, 1) {
            set.push(901 + v as u16);
        }
        // B5: the tabard flap (robes hide it).
        if robe.is_none() {
            if let Some(v) = g(7, 0) {
                set.push(1201 + v as u16);
            }
        }
        // B7: the shirt's doublet + the pants' leg geoset (the recorded 1102 base — not 1101).
        if let Some(v) = g(0, 1) {
            set.push(1001 + v as u16);
        }
        if robe.is_none() {
            if let Some(v) = g(3, 0) {
                set.push(1102 + v as u16);
            }
        }
        // B8: a cloak replaces the cloak group (1501+v, range disabled).
        if let Some(v) = equip.cloak.filter(|v| *v != 0) {
            disable(&mut set, 1500, 1599);
            set.push(1501 + v as u16);
        }
        set
    }

    /// Load both customization DBCs from the patch chain.
    ///
    /// **A duplicated `(race, sex, variation)` key resolves to the FIRST row** (decision 0682). Both
    /// consumers are *linear scans* of the loaded table — `0x478540` for the hair geoset, `0x478740`
    /// for the facial-hair record (wow-re RF-0073; `object-layer/scratch/r1.md:358` calls `0x478540`
    /// "a loaded-DBC linear scan, NOT a pool") — so the earliest matching record is the one the
    /// reference returns, where a last-write `HashMap::insert` would take the latest.
    ///
    /// It matters exactly once in the shipped data, and it was B11: `CharHairGeosets` carries **four**
    /// rows for **(race 9 goblin, sex 0, variation 0)** — ids 241–244, geosets 1 / 2 / 1 / 2, the only
    /// duplicated key in the file (243/244 read as the goblin-*female* pair with a zeroed sex column,
    /// which is why goblin females, having no row at all, were always right). Geoset 1 is the bare
    /// scalp; geoset 2 is that scalp **plus** a 38-vertex topknot whose texture slot is `Monster1` —
    /// a slot no character-model goblin display fills, so it drew flat white (decision 0157). All 314
    /// character-model goblin displays select variation 0, so last-write put a white tuft on every one
    /// of them. `CharacterFacialHairStyles` has no duplicate keys today; it takes the same rule
    /// because the same scan law governs it.
    pub fn load(chain: &mut Chain) -> Result<Self> {
        let hair = {
            let bytes = chain
                .read_file(CHAR_HAIR_GEOSETS)
                .with_context(|| format!("reading {CHAR_HAIR_GEOSETS}"))?;
            let rs = parse(&bytes, char_hair_geosets_schema(), "CharHairGeosets")?;
            let mut m = HashMap::with_capacity(rs.records().len());
            for r in rs.records() {
                // fields: ID, RaceID, SexID, VariationID(hairStyle), GeosetID, ShowScalp.
                if let (Some(race), Some(sex), Some(var), Some(geoset)) =
                    (u32_at(r, 1), u32_at(r, 2), u32_at(r, 3), u32_at(r, 4))
                {
                    // FIRST row wins — see the duplicate-key note on [`CharacterGeosets::load`].
                    m.entry((race as u8, sex as u8, var as u8))
                        .or_insert(geoset);
                }
            }
            m
        };
        let facial = {
            let bytes = chain
                .read_file(CHAR_FACIAL_HAIR)
                .with_context(|| format!("reading {CHAR_FACIAL_HAIR}"))?;
            let rs = parse(
                &bytes,
                char_facial_hair_schema(),
                "CharacterFacialHairStyles",
            )?;
            let mut m = HashMap::with_capacity(rs.records().len());
            for r in rs.records() {
                // fields: RaceID, SexID, VariationID(facialHair), 3×unused, gA, gB, gC (no ID column).
                if let (Some(race), Some(sex), Some(var)) =
                    (u32_at(r, 0), u32_at(r, 1), u32_at(r, 2))
                {
                    let g = [
                        u32_at(r, 6).unwrap_or(0),
                        u32_at(r, 7).unwrap_or(0),
                        u32_at(r, 8).unwrap_or(0),
                    ];
                    // FIRST row wins — see the duplicate-key note on [`CharacterGeosets::load`].
                    m.entry((race as u8, sex as u8, var as u8)).or_insert(g);
                }
            }
            m
        };
        // HelmetGeosetVisData — the helm hide-masks (RF-0083). Soft-optional: a missing/undecodable
        // table just means helms never hide hair (the pre-0083 behavior), like the other soft-fails.
        let helmet_vis = match chain.read_file(HELMET_GEOSET_VIS) {
            Ok(bytes) => {
                let rs = parse(&bytes, helmet_vis_schema(), "HelmetGeosetVisData")?;
                let mut m = HashMap::with_capacity(rs.records().len());
                for r in rs.records() {
                    if let Some(id) = u32_at(r, 0) {
                        m.insert(id, std::array::from_fn(|i| u32_at(r, 1 + i).unwrap_or(0)));
                    }
                }
                m
            }
            Err(_) => HashMap::new(),
        };
        Ok(Self {
            hair,
            facial,
            helmet_vis,
        })
    }
}

/// CharHairGeosets.dbc — 6 fields in build 5875 (verified against the file header).
pub(crate) fn char_hair_geosets_schema() -> Schema {
    let mut s = Schema::new("CharHairGeosets");
    for (name, ty) in [
        ("ID", FieldType::UInt32),
        ("RaceID", FieldType::UInt32),
        ("SexID", FieldType::UInt32),
        ("VariationID", FieldType::UInt32),
        ("GeosetID", FieldType::UInt32),
        ("ShowScalp", FieldType::UInt32),
    ] {
        s.add_field(SchemaField::new(name, ty));
    }
    s
}

/// HelmetGeosetVisData.dbc — 6 fields in build 5875 (verified: the loader `0x546f00` asserts
/// fieldCount 6 / recordSize 0x18; the 5 mask columns are **race** bitmasks, RF-0083).
pub(crate) fn helmet_vis_schema() -> Schema {
    let mut s = Schema::new("HelmetGeosetVisData");
    for (name, ty) in [
        ("ID", FieldType::UInt32),
        ("HideHair", FieldType::UInt32),
        ("HideFacial1", FieldType::UInt32),
        ("HideFacial2", FieldType::UInt32),
        ("HideFacial3", FieldType::UInt32),
        ("HideEars", FieldType::UInt32),
    ] {
        s.add_field(SchemaField::new(name, ty));
    }
    s
}

/// CharacterFacialHairStyles.dbc — 9 fields in build 5875 (verified); no ID column (keyed race/sex/var).
pub(crate) fn char_facial_hair_schema() -> Schema {
    let mut s = Schema::new("CharacterFacialHairStyles");
    for (name, ty) in [
        ("RaceID", FieldType::UInt32),
        ("SexID", FieldType::UInt32),
        ("VariationID", FieldType::UInt32),
        ("Unused3", FieldType::UInt32),
        ("Unused4", FieldType::UInt32),
        ("Unused5", FieldType::UInt32),
        ("Geoset100", FieldType::UInt32), // gA → group 1 (+100)
        ("Geoset300", FieldType::UInt32), // gB → group 3 (+300)
        ("Geoset200", FieldType::UInt32), // gC → group 2 (+200)
    ] {
        s.add_field(SchemaField::new(name, ty));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The equipment geoset branches (decision 0074, from the RF-0038 arithmetic): gloves/robe/cloak
    /// replace their groups, boots stack over the naked default, and a robe suppresses the tabard +
    /// pant-leg branches.
    #[test]
    fn equipment_geoset_branches() {
        let cg = CharacterGeosets {
            hair: HashMap::new(),
            facial: HashMap::new(),
            helmet_vis: HashMap::new(),
        };
        let naked = cg.visible_geosets(1, 0, 0, 0, &EquipGeosets::default());
        assert!(naked.contains(&401) && naked.contains(&1101) && naked.contains(&1501));

        // Gloves v=1: the glove group is replaced (401 out, 402 in).
        let mut eq = EquipGeosets::default();
        eq.bodyslots[6] = Some([1, 0, 0]);
        let set = cg.visible_geosets(1, 0, 0, 0, &eq);
        assert!(set.contains(&402) && !set.contains(&401));

        // Boots v=2: 503 stacks; the naked 501 deliberately stays (the recorded no-disable).
        let mut eq = EquipGeosets::default();
        eq.bodyslots[4] = Some([2, 0, 0]);
        let set = cg.visible_geosets(1, 0, 0, 0, &eq);
        assert!(set.contains(&503) && set.contains(&501));

        // A robe (chest group[2]=1) shows its skirt (1302) and hides boots/pant-legs/trousers —
        // including a worn tabard's flap and the pants' own branches.
        let mut eq = EquipGeosets::default();
        eq.bodyslots[1] = Some([1, 0, 1]);
        eq.bodyslots[3] = Some([2, 1, 0]);
        eq.bodyslots[4] = Some([2, 0, 0]);
        eq.bodyslots[7] = Some([1, 0, 0]);
        let set = cg.visible_geosets(1, 0, 0, 0, &eq);
        assert!(set.contains(&1302), "robe skirt on");
        assert!(
            !set.contains(&1101) && !set.contains(&1301),
            "pant legs off"
        );
        assert!(!set.contains(&501) && !set.contains(&503), "boot group off");
        assert!(!set.contains(&1202), "tabard suppressed under a robe");
        assert!(set.contains(&802), "the chest's sleeves still show");

        // Cloak v=4: the cloak group is replaced (1501 out, 1505 in).
        let eq = EquipGeosets {
            cloak: Some(4),
            ..Default::default()
        };
        let set = cg.visible_geosets(1, 0, 0, 0, &eq);
        assert!(set.contains(&1505) && !set.contains(&1501));
    }

    /// The helm-vis force (VERIFIED wow-re RF-0083): each vis column is a **race** bitmask; a set
    /// `1 << race` bit forces the slot back to its group base — hair to the bare scalp (1), facial
    /// groups to 101/201/301, ears to 701 — dropping the styled geosets. The row is sex-selected
    /// (ItemDisplayInfo col 12 male / 13 female); a mask without our race bit leaves the style.
    #[test]
    fn helm_vis_forces_group_bases_by_race() {
        let mut hair = HashMap::new();
        hair.insert((1, 0, 1), 5u32); // styled hair → geoset 5
        let mut facial = HashMap::new();
        facial.insert((1, 0, 1), [2u32, 3, 4]); // styled facial → 102/303/204
        let mut helmet_vis = HashMap::new();
        helmet_vis.insert(368, [u32::MAX; 5]); // hide-everything row (real id, dumped)
        helmet_vis.insert(245, [0u32; 5]); // hide-nothing row (real id, dumped)
        let cg = CharacterGeosets {
            hair,
            facial,
            helmet_vis,
        };
        let helm = EquipGeosets {
            helm_vis: Some([368, 245]),
            ..Default::default()
        };
        // Male (sex 0) picks row 368: everything forced to base.
        let set = cg.visible_geosets(1, 0, 1, 1, &helm);
        assert!(set.contains(&1) && !set.contains(&5), "hair → bare scalp");
        assert!(set.contains(&101) && !set.contains(&102));
        assert!(set.contains(&201) && !set.contains(&204));
        assert!(set.contains(&301) && !set.contains(&303));
        assert!(set.contains(&701) && !set.contains(&702), "ears → 701");
        // The female column of this pair is the hide-nothing row: styles survive.
        let mut hair = HashMap::new();
        hair.insert((1, 1, 1), 5u32);
        let cg2 = CharacterGeosets {
            hair,
            facial: HashMap::new(),
            helmet_vis: cg.helmet_vis.clone(),
        };
        let set = cg2.visible_geosets(1, 1, 1, 1, &helm);
        assert!(
            set.contains(&5) && set.contains(&702),
            "row 245 hides nothing"
        );
        // A mask covering only race 2 leaves a race-1 wearer styled.
        let mut helmet_vis = HashMap::new();
        helmet_vis.insert(300, [1u32 << 2; 5]);
        let cg3 = CharacterGeosets {
            hair: cg2.hair.clone(),
            facial: HashMap::new(),
            helmet_vis,
        };
        let masked = EquipGeosets {
            helm_vis: Some([300, 300]),
            ..Default::default()
        };
        let set = cg3.visible_geosets(1, 1, 1, 1, &masked);
        assert!(set.contains(&5), "race bit miss → style kept");
    }

    /// The naked selection for a known Human-male appearance (rows dumped from the build-5875 files):
    /// hairStyle 1 → GeosetID 2; facialHair 1 → gA/gB/gC = 1/2/1. So the set carries the body (0), the
    /// hair geoset (2), the three facial geosets (101 / 201 / 302), and the default group bases.
    #[test]
    fn naked_human_male_selection() {
        let mut hair = HashMap::new();
        hair.insert((1, 0, 1), 2u32);
        let mut facial = HashMap::new();
        facial.insert((1, 0, 1), [1u32, 2, 1]);
        let cg = CharacterGeosets {
            hair,
            facial,
            helmet_vis: HashMap::new(),
        };
        let set = cg.visible_geosets(1, 0, 1, 1, &EquipGeosets::default());

        assert!(set.contains(&0), "body geoset 0 always on");
        assert!(set.contains(&2), "hair: max(1, GeosetID 2)");
        assert!(set.contains(&101), "facial group 1: gA 1 + 100");
        assert!(set.contains(&302), "facial group 3: gB 2 + 300");
        assert!(set.contains(&201), "facial group 2: gC 1 + 200");
        // Default equipment-group bases (entries 4–15) stand for a naked body.
        assert!(set.contains(&401) && set.contains(&702) && set.contains(&1501));
        // A non-selected hair variant is hidden (geoset 5 is hairStyle 4, not chosen).
        assert!(!set.contains(&5), "unselected hair variant hidden");
    }

    /// A bald hairstyle (GeosetID 0) still resolves to geoset 1 (the scalp), never 0-the-body-collision.
    #[test]
    fn bald_resolves_to_scalp_geoset_one() {
        let mut hair = HashMap::new();
        hair.insert((1, 0, 0), 0u32); // variation 0 → GeosetID 0
        let cg = CharacterGeosets {
            hair,
            facial: HashMap::new(),
            helmet_vis: HashMap::new(),
        };
        let set = cg.visible_geosets(1, 0, 0, 0, &EquipGeosets::default());
        assert!(set.contains(&1), "max(1, 0) = scalp geoset 1");
    }

    /// The repo root's `WoW/Data` (gitignored; the real-data tests skip when absent).
    fn vanilla_data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
    }

    /// **B11, against the shipped DBC** (decision 0682): `CharHairGeosets` carries four rows for the
    /// goblin male's only hairstyle, and the reference's linear scan takes the first — geoset **1**,
    /// the bare scalp. Last-write took id 244's geoset **2**, which adds an untextured topknot.
    ///
    /// Three things are pinned here because each on its own could rot: that the shipped file really
    /// does duplicate that key (the defect only exists if it does), that we resolve it to 1, and that
    /// **no other key in the table is duplicated** — which is what bounds this change to goblins.
    #[test]
    fn goblin_male_hair_takes_the_first_of_four_duplicate_rows() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");

        // The file's own rows, read independently of the loader, so the test states the premise it
        // rests on rather than assuming it.
        let bytes = chain.read_file(CHAR_HAIR_GEOSETS).expect("CharHairGeosets");
        let rs = parse(&bytes, char_hair_geosets_schema(), "CharHairGeosets").expect("parse");
        let mut keys: HashMap<(u32, u32, u32), Vec<u32>> = HashMap::new();
        for r in rs.records() {
            if let (Some(race), Some(sex), Some(var), Some(geoset)) =
                (u32_at(r, 1), u32_at(r, 2), u32_at(r, 3), u32_at(r, 4))
            {
                keys.entry((race, sex, var)).or_default().push(geoset);
            }
        }
        let dups: Vec<_> = keys.iter().filter(|(_, v)| v.len() > 1).collect();
        assert_eq!(
            dups.len(),
            1,
            "exactly one duplicated (race,sex,variation) key ships; got {dups:?}"
        );
        assert_eq!(
            keys.get(&(9, 0, 0)).map(Vec::as_slice),
            Some([1u32, 2, 1, 2].as_slice()),
            "the goblin-male block is the duplicate, in file order"
        );

        // And the loader resolves it the reference's way.
        let cg = CharacterGeosets::load(&mut chain).expect("load customization tables");
        let set = cg.visible_geosets(9, 0, 0, 0, &EquipGeosets::default());
        assert!(
            set.contains(&1),
            "goblin male shows the bare scalp (geoset 1)"
        );
        assert!(
            !set.contains(&2),
            "goblin male does NOT show geoset 2 — the white topknot of B11"
        );
        // The goblin female has no row at all (race 9 ships only sex-0 rows), so she falls to the
        // region base, which is the same scalp. She was never affected; assert it so a future
        // "fix" to the sex column has to notice her.
        let f = cg.visible_geosets(9, 1, 0, 0, &EquipGeosets::default());
        assert!(f.contains(&1) && !f.contains(&2), "goblin female unchanged");
    }
}
