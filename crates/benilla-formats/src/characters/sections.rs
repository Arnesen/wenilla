use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{parse, str_at, u32_at};
use crate::items::ItemDisplay;
use crate::{read_texture_mip_chain, BlpMipChain, Chain};

const CHAR_SECTIONS: &str = "DBFilesClient\\CharSections.dbc";

/// CharSections `sectionType` values (wow-re RF-0074): the layer a row supplies. `skin` is the full
/// base atlas; the rest are region overlays the body composite blends on top (decision 0044).
const SECTION_SKIN: u8 = 0;
const SECTION_FACE: u8 = 1;
const SECTION_FACIAL_HAIR: u8 = 2;
const SECTION_HAIR: u8 = 3;
const SECTION_UNDERWEAR: u8 = 4;

/// The hair variation the client's type-6 binder substitutes when the selected style resolves no
/// texture — a **literal 1** in the binary at two of its three call sites (`0x478445`, `0x4786f2`),
/// not a search. See [`CharSections::hair_mesh_texture`]; wow-re `rf84`, decision 0536.
const HAIR_SUBSTITUTE_VARIATION: u8 = 1;

/// An atlas rect `(x, y, w, h)` in pixels — a composite destination tile.
type Tile = (u32, u32, u32, u32);

/// The body-atlas tiles the head + pelvis overlays composite into — three of the RF-0062 static 256²
/// partition's 10 rects. g8/g9 are the head strip (left column, Y 160–256: g8 the 32-tall upper band, g9
/// the 64-tall lower band); g5 is the pelvis (right column, Y 96–160).
const TILE_G8: Tile = (0, 160, 128, 32);
const TILE_G9: Tile = (0, 192, 128, 64);
const TILE_G5: Tile = (128, 96, 128, 64);

/// The eight equipment tiles g0–g7 (RF-0062), in **layer order** — the load-bearing identity of
/// decision 0074: ItemDisplayInfo texture column *i* = compositor layer *i* = tile g*i*. Note g5 ==
/// [`TILE_G5`] (equipment LegUpper shares the pelvis tile; underwear blits first, so it sits under).
const EQUIP_TILES: [Tile; 8] = [
    (0, 0, 128, 64),     // g0 ArmUpper
    (0, 64, 128, 64),    // g1 ArmLower
    (0, 128, 128, 32),   // g2 Hand
    (128, 0, 128, 64),   // g3 TorsoUpper
    (128, 64, 128, 32),  // g4 TorsoLower
    (128, 96, 128, 64),  // g5 LegUpper
    (128, 160, 128, 64), // g6 LegLower
    (128, 224, 128, 32), // g7 Foot
];

/// The `Item\TextureComponents\` region directory per layer/column (decision 0074; empirically
/// suffix-matched — the on-disk dirs are exactly these eight).
const EQUIP_TEX_DIRS: [&str; 8] = [
    "ArmUpperTexture",
    "ArmLowerTexture",
    "HandTexture",
    "TorsoUpperTexture",
    "TorsoLowerTexture",
    "LegUpperTexture",
    "LegLowerTexture",
    "FootTexture",
];

/// The `[0x803bf8]` bodyslot×layer stacking table (decision 0074), rows = bodyslots 2–9 (shirt,
/// chest, belt, pants, boots, wrist, gloves, tabard), columns = layers 0–7: the priority a slot's
/// contribution stacks at within the layer's tile (ascending blits later ⇒ on top), `-1` = this slot
/// never touches the layer. Transcribed from the RE'd const table; every column's ordering is
/// art-coherent (pants under robe under belt; shirt under chest under wrist under glove; …).
const EQUIP_LAYER_PRIORITY: [[i8; 8]; 8] = [
    [0, 0, -1, 0, 0, -1, -1, -1],    // shirt
    [1, 1, -1, 1, 1, 1, 1, -1],      // chest (robes reach the legs)
    [-1, -1, -1, -1, -1, 2, -1, -1], // belt
    [-1, -1, -1, -1, -1, 0, 0, -1],  // pants
    [-1, -1, -1, -1, -1, -1, 2, 0],  // boots
    [-1, 2, -1, -1, -1, -1, -1, -1], // wrist
    [-1, 3, 0, -1, -1, -1, -1, -1],  // gloves
    [-1, -1, -1, 3, 4, -1, -1, -1],  // tabard
];

/// CharSections texture lookup: (race, sex, sectionType, variation, colorIndex) → that row's up-to-3
/// `TextureName` columns (empty strings for absent columns). Feeds both the base body skin
/// (`sectionType 0`) and the head/pelvis region overlays the body composite blends on top (decision 0044).
pub struct CharSections {
    sections: HashMap<(u8, u8, u8, u8, u8), [String; 3]>,
}

impl CharSections {
    /// The base body-skin BLP for an appearance — CharSections `sectionType 0`, the single full 256²
    /// body-layout texture keyed by skinColor. `None` if the row is absent.
    pub fn skin_texture(&self, race: u8, sex: u8, skin_color: u8) -> Option<&str> {
        self.tex(race, sex, SECTION_SKIN, 0, skin_color, 0)
    }

    /// The standalone **extra-skin** BLP for an appearance — CharSections `sectionType 0`,
    /// `TextureName[1]`, keyed by skinColor (`…Skin00_NN_Extra.blp`). The texture the client binds to a
    /// character body's M2 texture type 8 batches, loaded plain — never composited (the client's extra
    /// loader is a bare TextureCreate). Only fur races author the column: tauren bind their head/leg fur
    /// to it; every other race's rows leave it empty (and their models carry no type-8 batch).
    pub fn skin_extra_texture(&self, race: u8, sex: u8, skin_color: u8) -> Option<&str> {
        self.tex(race, sex, SECTION_SKIN, 0, skin_color, 1)
    }

    /// The hair-**mesh** texture for an appearance — CharSections `sectionType 3`, `TextureName[0]`,
    /// keyed by hairStyle (variation) + hairColor. The single BLP the client binds to the hair geometry's
    /// M2 texture type 6 (decision 0045); the colour is baked into the chosen file, not a runtime tint.
    /// `None` when the row/column is empty (e.g. a bald style, variation 0 — its columns are blank).
    /// (`TextureName[1]/[2]` of the same row are the scalp-on-skin overlays the body composite blends into
    /// the head atlas — those go through [`Self::composite_body`], not here.)
    pub fn hair_texture(&self, race: u8, sex: u8, hair_style: u8, hair_color: u8) -> Option<&str> {
        self.tex(race, sex, SECTION_HAIR, hair_style, hair_color, 0)
    }

    /// The texture to bind to a character body's M2 **type 6** batches — the hair sheet, which on
    /// several races dresses the *facial* hair geometry (beard/mustache geosets) as well as the
    /// scalp hair. Prefer this over [`Self::hair_texture`] anywhere a mesh is being textured;
    /// `hair_texture` is the raw row accessor, and a bald row is genuinely blank.
    ///
    /// **The mechanism** (byte-verified, wow-re `rf84-hair-texture-type6-resolution.md`; decision
    /// 0536). The client has no fallback *lookup*. `0x478220(cc, variationIdx)` is the sole type-6
    /// binder, it always reads `TextureName[0]` (never column-indexed), and an **empty name is a
    /// no-op that leaves the slot untouched** (`0x47827d cmp BYTE PTR [ecx],0x0` → `je 0x4782d8`) —
    /// not a null bind. Three sites call it, in build order skin → hairStyle → facialHair:
    ///
    /// 1. `0x478445` — variation **literal 1**, gated `sex==0 && hairStyle==0 && ChrRaces.Flags & 8`
    /// 2. `0x478450` — variation `hairStyle`, unconditional
    /// 3. `0x4786f2` — variation **literal 1**, gated on the selected beard having geoset geometry
    ///    and the hairStyle row missing a detail column; no sex/race/hairStyle test
    ///
    /// Because no site ever clears the slot, the result is simply the **last site that resolved a
    /// non-empty name** — which reduces to: take the hairStyle row, and when it is blank take
    /// **variation 1** at the same colour. That is the fixpoint of the client's incremental apply
    /// pipeline, and it is what this implements; we resolve a whole look at once rather than
    /// re-applying per dial, so emulating the three sites separately would add machinery without
    /// changing an output.
    ///
    /// The literal 1 is load-bearing where a race authors several sheets per colour: Human male
    /// resolves `Hair03_<colour>.blp` specifically, not an arbitrary non-blank row.
    ///
    /// **Where this can diverge** (verified inert on 1.12.1 data, kept honest rather than dropped):
    /// site 3's trigger is broader than "the hairStyle row is blank" — it also fires for Dwarf M
    /// {0,2,4,9}, Scourge M {1,3,4,8} and Orc F {7}, i.e. 14 of 145 variation groups rather than the
    /// 5 blank ones. In every one of those the variation-1 sheet is byte-identical to what the real
    /// hairStyle already bound, so the reduction above is exact on the shipped build. It would only
    /// part company with the client on data where a site-3 group's variation-1 sheet differs from
    /// its own — which build 5875 does not contain.
    pub fn hair_mesh_texture(
        &self,
        race: u8,
        sex: u8,
        hair_style: u8,
        hair_color: u8,
    ) -> Option<&str> {
        self.hair_texture(race, sex, hair_style, hair_color)
            .or_else(|| self.hair_texture(race, sex, HAIR_SUBSTITUTE_VARIATION, hair_color))
    }

    /// One `TextureName` column of a section row, or `None` if the row/column is absent or empty.
    fn tex(&self, race: u8, sex: u8, ty: u8, var: u8, color: u8, col: usize) -> Option<&str> {
        self.sections
            .get(&(race, sex, ty, var, color))
            .map(|t| t[col].as_str())
            .filter(|s| !s.is_empty())
    }

    /// Composite a character's full body-skin atlas: the base 256² skin with the face / facial-hair /
    /// hair / underwear region overlays blended in at their fixed atlas tiles, returned as one mip
    /// pyramid ready to upload (decision 0044). `Ok(None)` if the base skin row is absent; a missing or
    /// undecodable overlay is skipped (best-effort, like the rest of the asset pipeline).
    ///
    /// Each overlay is read from its own origin and source-over blitted onto the base at the verified
    /// tile (RF-0062 bbox table + RF-0067 per-layer src rect + RF-0074 head-section map). The overlay
    /// BLP's own alpha is the blend control, so an opaque overlay (`alphaDepth 0` → the real client's
    /// REPLACE: face, pelvis) overwrites the base and an alpha one (facial hair) blends — at full 8-bit
    /// precision, not the client's 16-bit RGB565 / 2-bit-coverage memory format (the modern-client
    /// choice; the decision has the why). Composited per **authored** mip level — no gamma-byte CPU
    /// downsample, the same C2 rule [`read_texture_mip_chain`]/the world-art upload follow.
    ///
    /// `equipment` is the dressed extension (decision 0074): the worn ItemDisplayInfo rows by
    /// **bodyslot − 2** (shirt, chest, belt, pants, boots, wrist, gloves, tabard; `None` = the slot
    /// is empty). Their region textures blit into the eight equipment tiles after the skin sections
    /// (so underwear sits under the pants), stacked per layer by the client's priority table.
    #[allow(clippy::too_many_arguments)]
    pub fn composite_body(
        &self,
        chain: &mut Chain,
        race: u8,
        sex: u8,
        skin: u8,
        face: u8,
        facial_hair: u8,
        hair_style: u8,
        hair_color: u8,
        equipment: [Option<&ItemDisplay>; 8],
    ) -> Result<Option<BlpMipChain>> {
        let Some(base_path) = self.skin_texture(race, sex, skin) else {
            return Ok(None);
        };
        let mut atlas = read_texture_mip_chain(chain, base_path)
            .with_context(|| format!("reading base skin '{base_path}'"))?;
        // (sectionType, variation, color, texColumn, destTile) — the verified fan-out (RF-0067 §"section
        // → cell core" + RF-0074 head map). Within a tile, order matters (later overwrites/blends over
        // earlier): base skin (already the canvas) → face → facial hair → hair. The pelvis (g5) is its own
        // tile. Note the columns differ by section: face/facial-hair use TextureName[0]/[1] (lower/upper),
        // hair uses [1]/[2]; underwear is TextureName[0]. Hair is blank for e.g. Human male (its texid
        // columns are empty), so those reads no-op there; included so the path is correct for races whose
        // hairline does composite into the head.
        let overlays: [(u8, u8, u8, usize, Tile); 7] = [
            (SECTION_FACE, face, skin, 0, TILE_G9),
            (SECTION_FACE, face, skin, 1, TILE_G8),
            (SECTION_FACIAL_HAIR, facial_hair, hair_color, 0, TILE_G9),
            (SECTION_FACIAL_HAIR, facial_hair, hair_color, 1, TILE_G8),
            (SECTION_HAIR, hair_style, hair_color, 1, TILE_G9),
            (SECTION_HAIR, hair_style, hair_color, 2, TILE_G8),
            (SECTION_UNDERWEAR, 0, skin, 0, TILE_G5),
        ];
        for (ty, var, color, col, tile) in overlays {
            let Some(path) = self.tex(race, sex, ty, var, color, col) else {
                continue;
            };
            if let Ok(overlay) = read_texture_mip_chain(chain, path) {
                blit_over(&mut atlas, &overlay, tile);
            }
        }
        // The equipment layers (decision 0074): per layer, the worn contributions stacked by the
        // priority table, each read gendered-first (`_M`/`_F` by the wearer's sex, `_U` fallback).
        for layer in 0..8 {
            let mut contributions: Vec<(i8, &str)> = equipment
                .iter()
                .enumerate()
                .filter_map(|(slot, d)| {
                    let prio = EQUIP_LAYER_PRIORITY[slot][layer];
                    let name = d.as_ref()?.region_textures[layer].as_deref()?;
                    (prio >= 0).then_some((prio, name))
                })
                .collect();
            contributions.sort_by_key(|(prio, _)| *prio);
            for (_, name) in contributions {
                if let Some(overlay) = read_equip_region(chain, layer, name, sex) {
                    blit_over(&mut atlas, &overlay, EQUIP_TILES[layer]);
                }
            }
        }
        Ok(Some(atlas))
    }

    /// Load CharSections.dbc from the patch chain.
    pub fn load(chain: &mut Chain) -> Result<Self> {
        let bytes = chain
            .read_file(CHAR_SECTIONS)
            .with_context(|| format!("reading {CHAR_SECTIONS}"))?;
        let rs = parse(&bytes, char_sections_schema(), "CharSections")?;
        let mut sections = HashMap::with_capacity(rs.records().len());
        for r in rs.records() {
            // fields: ID, Race, Sex, SectionType, Variation, Color, Tex0, Tex1, Tex2, Flags.
            if let (Some(race), Some(sex), Some(ty), Some(var), Some(color)) = (
                u32_at(r, 1),
                u32_at(r, 2),
                u32_at(r, 3),
                u32_at(r, 4),
                u32_at(r, 5),
            ) {
                let texs = [
                    str_at(&rs, r, 6).unwrap_or_default(),
                    str_at(&rs, r, 7).unwrap_or_default(),
                    str_at(&rs, r, 8).unwrap_or_default(),
                ];
                let key = (race as u8, sex as u8, ty as u8, var as u8, color as u8);
                // The standard player sections are `flags == 0`. Some (race,sex,type,color) keys ALSO have
                // an `EXTRA`-flagged row (`flags & 0x1`) sharing the same colorIndex — e.g. human-male
                // skinColor 0/1 carry both `HumanMaleSkin00_00` (flags 0) and the late `…_100` (flags 1).
                // Those are not the chargen-selectable skins; a player with skinColor 0/1 must get `…_00`.
                // So a standard row always wins; an extra row only fills a key no standard row provides
                // (order-independent — robust to the extras appearing before or after the standards).
                let extra = u32_at(r, 9).unwrap_or(0) & 0x1 != 0;
                if extra {
                    sections.entry(key).or_insert(texs);
                } else {
                    sections.insert(key, texs);
                }
            }
        }
        Ok(Self { sections })
    }
}

/// Read one equipment region texture off the chain: `Item\TextureComponents\<dir>\<name>_<L>.blp`,
/// the gender letter by the wearer's sex (`M`/`F`), falling back to `_U` (unisex — the majority of
/// the shipped files) then the bare name (defensive; nothing shipped is bare). `None` when no
/// variant decodes — the layer is skipped, best-effort like the skin overlays.
fn read_equip_region(chain: &mut Chain, layer: usize, name: &str, sex: u8) -> Option<BlpMipChain> {
    let dir = EQUIP_TEX_DIRS[layer];
    let letter = if sex == 1 { 'F' } else { 'M' };
    for suffix in [format!("_{letter}"), "_U".into(), String::new()] {
        let path = format!("Item\\TextureComponents\\{dir}\\{name}{suffix}.blp");
        if let Ok(mips) = read_texture_mip_chain(chain, &path) {
            return Some(mips);
        }
    }
    None
}

/// Source-over composite of one region overlay's authored mip pyramid onto the body atlas at a fixed
/// tile. Per mip level `i` the destination tile is `(x>>i, y>>i, w>>i, h>>i)` and the overlay's level-`i`
/// pixels are read from its **own origin** — the RF-0067 overlay src rect (`src = (0,0)`, `dst = tile.xy`,
/// extent `tile.wh`), so the overlay BLP is authored exactly tile-sized. Straight 8-bit source-over per
/// channel (`out = src·a + dst·(1−a)`, `out_a = a + dst_a·(1−a)`): an opaque overlay (`a == 255`) is a
/// plain copy (the client's REPLACE), an alpha one blends. Levels past either pyramid's end, and the
/// sub-pixel remainder of a degenerate deep-mip tile, are clamped/skipped.
fn blit_over(dst: &mut BlpMipChain, src: &BlpMipChain, tile: Tile) {
    let (tx, ty, tw, th) = tile;
    let levels = dst.mips.len().min(src.mips.len());
    for i in 0..levels {
        let dw = (dst.width >> i).max(1) as usize;
        let dh = (dst.height >> i).max(1) as usize;
        let sw = (src.width >> i).max(1) as usize;
        let sh = (src.height >> i).max(1) as usize;
        let (ox, oy) = ((tx >> i) as usize, (ty >> i) as usize);
        // Copy extent: the tile size at this level, clamped to what both the source and the dest hold.
        let cw = ((tw >> i).max(1) as usize)
            .min(sw)
            .min(dw.saturating_sub(ox));
        let ch = ((th >> i).max(1) as usize)
            .min(sh)
            .min(dh.saturating_sub(oy));
        let (d, s) = (&mut dst.mips[i], &src.mips[i]);
        for row in 0..ch {
            for col in 0..cw {
                let si = (row * sw + col) * 4;
                let di = ((oy + row) * dw + (ox + col)) * 4;
                if si + 4 > s.len() || di + 4 > d.len() {
                    continue;
                }
                let a = s[si + 3] as u32;
                if a == 0 {
                    continue; // fully transparent overlay texel — leave the base
                }
                if a == 255 {
                    d[di..di + 4].copy_from_slice(&s[si..si + 4]);
                    continue;
                }
                let ia = 255 - a;
                for c in 0..3 {
                    d[di + c] = ((s[si + c] as u32 * a + d[di + c] as u32 * ia + 127) / 255) as u8;
                }
                d[di + 3] = (a + (d[di + 3] as u32 * ia + 127) / 255).min(255) as u8;
            }
        }
    }
}

/// CharSections.dbc — 10 fields in build 5875 (verified); 3 string columns are texture-name offsets.
fn char_sections_schema() -> Schema {
    let mut s = Schema::new("CharSections");
    for (name, ty) in [
        ("ID", FieldType::UInt32),
        ("RaceID", FieldType::UInt32),
        ("SexID", FieldType::UInt32),
        ("SectionType", FieldType::UInt32),
        ("VariationIndex", FieldType::UInt32),
        ("ColorIndex", FieldType::UInt32),
        ("TextureName0", FieldType::String),
        ("TextureName1", FieldType::String),
        ("TextureName2", FieldType::String),
        ("Flags", FieldType::UInt32),
    ] {
        s.add_field(SchemaField::new(name, ty));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The base skin resolves the full-body BLP for (race, sex, skinColor) — the path the renderer
    /// applies to the body-skin batches (value = the real build-5875 row for Human male skinColor 3).
    #[test]
    fn base_skin_texture_resolves() {
        let mut sections = HashMap::new();
        sections.insert(
            (1, 0, SECTION_SKIN, 0, 3),
            [
                "Character\\Human\\Male\\HumanMaleSkin00_03.blp".to_string(),
                String::new(),
                String::new(),
            ],
        );
        let cs = CharSections { sections };
        assert_eq!(
            cs.skin_texture(1, 0, 3),
            Some("Character\\Human\\Male\\HumanMaleSkin00_03.blp")
        );
        assert_eq!(cs.skin_texture(1, 0, 99), None, "absent color → no texture");
    }

    /// A single-level RGBA helper for the blit test.
    fn chain(width: u32, height: u32, px: Vec<u8>) -> BlpMipChain {
        BlpMipChain {
            width,
            height,
            mips: vec![px],
        }
    }

    /// `blit_over`: an opaque overlay REPLACEs the tile; an alpha overlay source-over blends; a fully
    /// transparent texel leaves the base; pixels outside the tile are untouched.
    #[test]
    fn blit_over_replaces_blends_and_clamps() {
        // 2×2 mid-grey opaque base (4 px of RGBA [128,128,128,255]).
        let mut dst = chain(2, 2, [128, 128, 128, 255].repeat(4));

        // 1×1 opaque red at tile (0,0,1,1) → replaces pixel 0, leaves the rest.
        blit_over(&mut dst, &chain(1, 1, vec![255, 0, 0, 255]), (0, 0, 1, 1));
        assert_eq!(
            &dst.mips[0][0..4],
            &[255, 0, 0, 255],
            "opaque overlay replaces"
        );
        assert_eq!(
            &dst.mips[0][4..8],
            &[128, 128, 128, 255],
            "neighbour untouched"
        );

        // 1×1 half-alpha blue at (1,1) → blends 50/50 with grey: R≈64, B≈191, stays opaque.
        blit_over(&mut dst, &chain(1, 1, vec![0, 0, 255, 128]), (1, 1, 1, 1));
        let px = &dst.mips[0][12..16];
        assert!((px[0] as i32 - 64).abs() <= 1, "blended R ≈ 128·(1−a)");
        assert!(
            (px[2] as i32 - 191).abs() <= 1,
            "blended B ≈ 255·a + 128·(1−a)"
        );
        assert_eq!(px[3], 255, "over an opaque base the result stays opaque");

        // A fully transparent overlay texel is a no-op (the base shows through).
        let before = dst.mips[0].clone();
        blit_over(&mut dst, &chain(1, 1, vec![1, 2, 3, 0]), (0, 0, 1, 1));
        assert_eq!(dst.mips[0], before, "transparent texel is a no-op");
    }

    /// The repo root's `WoW/Data` (gitignored; the real-data tests skip when absent).
    fn vanilla_data_dir() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../WoW/Data")
    }

    /// End-to-end regression on the **real** build-5875 files: compositing a Human-male body must yield
    /// a 256² mip pyramid whose head (g8/g9) + pelvis (g5) tiles are overlaid from the base, while a
    /// control tile that carries no naked-body overlay (g3) is untouched. Guards the section→tile map +
    /// the CharSections schema against a silent break (a wrong tile or a shifted column would move/lose
    /// these diffs). Skips when the client data isn't present.
    #[test]
    fn composite_body_overlays_land_on_real_human_male() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cs = CharSections::load(&mut chain).expect("load CharSections");
        let base =
            read_texture_mip_chain(&mut chain, "Character\\Human\\Male\\HumanMaleSkin00_03.blp")
                .expect("read base skin");
        let comp = cs
            .composite_body(&mut chain, 1, 0, 3, 0, 1, 0, 0, [None; 8])
            .expect("composite ok")
            .expect("base skin row present");

        assert_eq!((comp.width, comp.height), (256, 256), "atlas is 256²");
        assert_eq!(comp.mips.len(), base.mips.len(), "keeps the base mip count");

        // Count mip-0 pixels in a tile that differ from the base atlas.
        let changed = |t: Tile| {
            let (x, y, w, h) = t;
            let (b, c) = (&base.mips[0], &comp.mips[0]);
            (y..y + h)
                .flat_map(|row| (x..x + w).map(move |col| (row, col)))
                .filter(|&(row, col)| {
                    let i = ((row * 256 + col) * 4) as usize;
                    b[i..i + 4] != c[i..i + 4]
                })
                .count()
        };
        // Face replaces the head tiles; pelvis replaces g5 — each should change most of its tile.
        assert!(changed(TILE_G9) > 4000, "face lower overlaid into g9");
        assert!(changed(TILE_G8) > 2000, "face upper overlaid into g8");
        assert!(changed(TILE_G5) > 4000, "pelvis overlaid into g5");
        // A right-column tile with no naked-body overlay must be byte-identical to the base.
        assert_eq!(changed((128, 0, 128, 64)), 0, "g3 carries no naked overlay");

        // Hair-mesh texture (decision 0045): a real hairstyle resolves a `Hair…` BLP; the bald style
        // (variation 0) has none. Guards the `SECTION_HAIR` constant + the type-3 row keying.
        let hair = cs
            .hair_texture(1, 0, 1, 0)
            .expect("hairStyle 1 has a hair texture");
        assert!(
            hair.contains("Hair"),
            "hair texture is a Hair BLP, got {hair:?}"
        );
        assert_eq!(
            cs.hair_texture(1, 0, 0, 0),
            None,
            "bald style has no hair texture"
        );

        // The type-6 MESH resolver's substitute (decision 0536, byte-verified wow-re `rf84`): when the
        // selected style resolves nothing, the client's binder has already bound variation **1** and an
        // empty name leaves that slot untouched. A bald orc/gnome male still wears a beard, and on those
        // races the beard is geometry on the hair unit — so the blank bald row must not leave it
        // untextured (the flat-white bug: decision 0157's fallback showing through).
        for (race, sex, name) in [(2u8, 0u8, "Orc"), (7, 0, "Gnome")] {
            let styled = cs
                .hair_mesh_texture(race, sex, 1, 0)
                .unwrap_or_else(|| panic!("{name} hairStyle 1 has a hair sheet"));
            let bald = cs
                .hair_mesh_texture(race, sex, 0, 0)
                .unwrap_or_else(|| panic!("{name} bald must still resolve a sheet for the beard"));
            assert_eq!(
                bald, styled,
                "{name} carries the style in the geometry — one sheet per colour"
            );
            assert!(
                bald.contains("Hair00_00"),
                "{name} sheet is Hair00_00, got {bald:?}"
            );
            // The fallback tracks hair COLOUR (the beard is hair-coloured), not just any row.
            let bald_c2 = cs
                .hair_mesh_texture(race, sex, 0, 2)
                .unwrap_or_else(|| panic!("{name} bald at colour 2 resolves"));
            assert!(
                bald_c2.contains("Hair00_02"),
                "{name} colour 2 sheet, got {bald_c2:?}"
            );
            assert_ne!(bald, bald_c2, "{name} colour must change the sheet");
        }

        // Human authors TWO distinct sheets per colour, which is exactly where the literal-1 substitute
        // is load-bearing: a bald human male resolves variation 1's sheet specifically, not "whichever
        // non-blank row we happened to find". The earlier interim searched for a unique non-blank
        // variation and returned None here — behaviourally inert (a bald human's facial hair is a
        // painted overlay, so there is no type-6 geometry to texture) but not the client's mechanism.
        let human_bald = cs
            .hair_mesh_texture(1, 0, 0, 0)
            .expect("the substitute resolves variation 1 even where a race authors several sheets");
        assert_eq!(
            Some(human_bald),
            cs.hair_texture(1, 0, HAIR_SUBSTITUTE_VARIATION, 0),
            "the substitute is variation 1, not an arbitrary non-blank row"
        );
        assert!(
            human_bald.contains("Hair03_00"),
            "human male variation 1 is the Hair03 sheet, got {human_bald:?}"
        );
        // A real style is never touched by the substitute, on any race.
        assert_eq!(
            cs.hair_mesh_texture(1, 0, 1, 0),
            cs.hair_texture(1, 0, 1, 0)
        );

        // Extra-skin texture (the tauren fur, M2 type 8): the type-0 row's SECOND column. Tauren male
        // skinColor 0 resolves the real build-5875 `_Extra` BLP; a race that doesn't author the column
        // (human) yields None — its models carry no type-8 batch.
        assert_eq!(
            cs.skin_extra_texture(6, 0, 0),
            Some("Character\\Tauren\\Male\\TaurenMaleSkin00_00_Extra.blp"),
            "tauren male extra skin resolves"
        );
        assert_eq!(
            cs.skin_extra_texture(1, 0, 0),
            None,
            "human male authors no extra skin"
        );

        // skinColor 0/1 must resolve the standard chargen skin, NOT the EXTRA-flagged `…_100/_101` row
        // sharing the same colorIndex (the flags-precedence fix).
        assert_eq!(
            cs.skin_texture(1, 0, 0),
            Some("Character\\Human\\Male\\HumanMaleSkin00_00.blp"),
            "skinColor 0 is the standard skin, not the EXTRA-flagged _100"
        );
        assert_eq!(
            cs.skin_texture(1, 0, 1),
            Some("Character\\Human\\Male\\HumanMaleSkin00_01.blp")
        );
    }

    /// Equipment layers on the **real** files (decision 0074): dressing the Human male in One's
    /// starter kit (shirt 9891 / pants 9892 / boots 10141) must repaint exactly the tiles those
    /// displays' region columns name — torso from the shirt, LegUpper from the pants, Foot from the
    /// boots — and leave a tile no item touches (g2 Hand) byte-identical to the naked composite.
    /// Also pins the boots-over-pants stacking on the shared LegLower tile (both contribute; the
    /// priority table puts the boot layer on top, so g6 must differ from the pants-only composite).
    #[test]
    fn composite_body_equipment_layers_land_on_real_human_male() {
        let data = vanilla_data_dir();
        if !data.is_dir() {
            eprintln!("skipping: vanilla client not present at {}", data.display());
            return;
        }
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cs = CharSections::load(&mut chain).expect("load CharSections");
        let items = crate::load_item_display_catalog(&mut chain).expect("load ItemDisplayInfo");
        let (shirt, pants, boots) = (
            items.get(9891).expect("shirt display"),
            items.get(9892).expect("pants display"),
            items.get(10141).expect("boots display"),
        );
        let mut compose = |equipment: [Option<&ItemDisplay>; 8]| {
            cs.composite_body(&mut chain, 1, 0, 3, 0, 1, 0, 0, equipment)
                .expect("composite ok")
                .expect("base skin row present")
        };
        let naked = compose([None; 8]);
        // bodyslot-2 indexing: shirt = slot 2 (idx 0), pants = slot 5 (idx 3), boots = slot 6 (idx 4).
        let mut equipment: [Option<&ItemDisplay>; 8] = [None; 8];
        equipment[0] = Some(shirt);
        equipment[3] = Some(pants);
        let pants_only = compose(equipment);
        equipment[4] = Some(boots);
        let dressed = compose(equipment);

        let changed = |a: &BlpMipChain, b: &BlpMipChain, t: Tile| {
            let (x, y, w, h) = t;
            (y..y + h)
                .flat_map(|row| (x..x + w).map(move |col| (row, col)))
                .filter(|&(row, col)| {
                    let i = ((row * 256 + col) * 4) as usize;
                    a.mips[0][i..i + 4] != b.mips[0][i..i + 4]
                })
                .count()
        };
        assert!(
            changed(&naked, &dressed, EQUIP_TILES[3]) > 2000,
            "shirt repaints TorsoUpper (g3)"
        );
        assert!(
            changed(&naked, &dressed, EQUIP_TILES[5]) > 2000,
            "pants repaint LegUpper (g5)"
        );
        assert!(
            changed(&naked, &dressed, EQUIP_TILES[7]) > 1000,
            "boots repaint Foot (g7)"
        );
        assert_eq!(
            changed(&naked, &dressed, EQUIP_TILES[2]),
            0,
            "nothing touches Hand (g2)"
        );
        assert!(
            changed(&pants_only, &dressed, EQUIP_TILES[6]) > 1000,
            "boots stack over the pants' LegLower (g6)"
        );
    }
}
