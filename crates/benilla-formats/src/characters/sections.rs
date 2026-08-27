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

/// The body-atlas tiles the head + underwear overlays composite into — four of the RF-0062 static 256²
/// partition's 10 rects. g8/g9 are the head strip (left column, Y 160–256: g8 the 32-tall upper band, g9
/// the 64-tall lower band); g3 (torso upper) and g5 (pelvis) are the right column's Y 0–64 and Y 96–160 —
/// the two tiles the underwear section dresses, one per texture column (RF-0062's group→cell map: the
/// group-3 handler `0x4772f0` takes underwear cell `cc+0x20c`, the group-5 handler `0x4773a0` takes
/// `cc+0x208`, and `0x478790` builds those two from the sectionType-4 row's `TextureName[1]`/`[0]`).
const TILE_G8: Tile = (0, 160, 128, 32);
const TILE_G9: Tile = (0, 192, 128, 64);
// The underwear's two tiles ARE two of the equipment tiles — the composite reaches them through
// `EQUIP_TILES` so there is one transcription of the rects, and these names exist for the tests,
// which read far better naming the group than indexing the array.
#[cfg(test)]
const TILE_G3: Tile = EQUIP_TILES[3];
#[cfg(test)]
const TILE_G5: Tile = EQUIP_TILES[5];

/// The underwear section's two blits: `(textureColumn, equipLayer, columnsTested)` — which
/// `TextureName` column dresses which equipment tile, and **how far into that tile's equipment
/// columns the client looks before it draws at all** (wow-re RF-0086, byte-derived from the two
/// handlers).
///
/// The underwear is not an under-layer — it is the tile's **fallback**. `0x4772f0` (TorsoUpper)
/// tests `cc+0x2e8/0x2ec/0x2f0` = columns 0–2 and `0x4773a0` (LegUpper) tests `cc+0x368/0x36c` =
/// columns 0–1, each `jne` straight past the underwear blit; only with every tested cell null does
/// the handler blit `cc+0x20c`/`cc+0x208`. So a shirt, a chest/robe or a guild tabard's background
/// hides the bra, and legs or a robe hide the panties — but the tested set is a strict **prefix**,
/// and the columns past it are not consulted: **a belt never hides the panties, and a plain tabard
/// never hides the bra**, even though both repaint their tile afterwards.
///
/// The base-skin blit is unconditional in both branches, so a suppressed tile shows *skin*, not a
/// hole — and the underwear itself is a REPLACE (alphaDepth 0, an opaque full-tile paste), never a
/// blend.
const UNDERWEAR_TILES: [(usize, usize, i8); 2] = [
    (0, 5, 2), // TextureName[0] → LegUpper: {Legs, Chest/Robe} tested; the belt (col 2) is not
    (1, 3, 3), // TextureName[1] → TorsoUpper: {Shirt, Chest/Robe, guild-tabard Background} tested
];

/// The eight equipment tiles g0–g7 (RF-0062), in **layer order** — the load-bearing identity of
/// decision 0074: ItemDisplayInfo texture column *i* = compositor layer *i* = tile g*i*. Note g3 ==
/// [`TILE_G3`] and g5 == [`TILE_G5`] — equipment TorsoUpper/LegUpper share the underwear's two tiles;
/// underwear blits first, so it sits under.
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

/// The `[0x803bf8]` bodyslot×layer table (wow-re RF-0088), rows = bodyslots 2–9 (shirt, chest, belt,
/// pants, boots, wrist, gloves, tabard), columns = layers 0–7: the **cell** within the layer's row
/// that this slot's contribution occupies, `-1` = this slot never touches the layer.
///
/// It is a **default, not a fixed priority** — that was decision 0074's mistake and B326/B327's
/// cause. `0x478ad0` tests the cell for `-1` and for a non-empty texture name, then hands the slot to
/// the layer's own **chooser** `[0xb42424 + layer*4]`, and two of the eight choosers overrule the
/// table from the item's `geosetGroup` (see [`equip_column`]). Layers 0/2/3/4/5/7 take it verbatim.
///
/// The row is 16 cells and holds **one record per cell**; the composite handler fans out over it by
/// ascending cell index, so a higher cell is blitted later and covers a lower one.
const EQUIP_LAYER_COLUMN: [[i8; 8]; 8] = [
    [0, 0, -1, 0, 0, -1, -1, -1],    // shirt
    [1, 1, -1, 1, 1, 1, 1, -1],      // chest (robes reach the legs)
    [-1, -1, -1, -1, -1, 2, -1, -1], // belt
    [-1, -1, -1, -1, -1, 0, 0, -1],  // pants
    [-1, -1, -1, -1, -1, -1, 2, 0],  // boots
    [-1, 2, -1, -1, -1, -1, -1, -1], // wrist
    [-1, 3, 0, -1, -1, -1, -1, -1],  // gloves
    [-1, -1, -1, 4, 4, -1, -1, -1], // tabard (RF-0088: TorsoUpper is cell 4, not 3 — 0074 mis-read it)
];

/// The worn-slot indices the two overruling choosers name, in `equipment` order (bodyslot − 2).
const SLOT_CHEST: usize = 1;
const SLOT_PANTS: usize = 3;
const SLOT_BOOTS: usize = 4;
const SLOT_GLOVES: usize = 6;

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
    /// is empty). Their region textures blit into the eight equipment tiles after the skin sections,
    /// stacked per layer by the client's priority table — and they also **gate** the underwear, which
    /// is the tile's fallback rather than an under-layer ([`UNDERWEAR_TILES`]).
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
        // The head overlays: (sectionType, variation, color, texColumn, destTile) — the verified fan-out
        // (RF-0067 §"section → cell core" + RF-0074 head map). Within a tile, order matters (later
        // overwrites/blends over earlier): base skin (already the canvas) → face → facial hair → hair.
        // Note the columns differ by section: face/facial-hair use TextureName[0]/[1] (lower/upper),
        // hair uses [1]/[2]. Hair is blank for e.g. Human male (its texid columns are empty), so those
        // reads no-op there; included so the path is correct for races whose hairline does composite
        // into the head.
        let overlays: [(u8, u8, u8, usize, Tile); 6] = [
            (SECTION_FACE, face, skin, 0, TILE_G9),
            (SECTION_FACE, face, skin, 1, TILE_G8),
            (SECTION_FACIAL_HAIR, facial_hair, hair_color, 0, TILE_G9),
            (SECTION_FACIAL_HAIR, facial_hair, hair_color, 1, TILE_G8),
            (SECTION_HAIR, hair_style, hair_color, 1, TILE_G9),
            (SECTION_HAIR, hair_style, hair_color, 2, TILE_G8),
        ];
        for (ty, var, color, col, tile) in overlays {
            let Some(path) = self.tex(race, sex, ty, var, color, col) else {
                continue;
            };
            if let Ok(overlay) = read_texture_mip_chain(chain, path) {
                blit_over(&mut atlas, &overlay, tile);
            }
        }
        // The equipment plan, built once: the blits below consume it, and the underwear reads it as its
        // gate. One plan, so "what dresses this tile?" has a single answer (decision 0074's `equip_blits`).
        let plan = equip_blits(&equipment);
        // The underwear ([`UNDERWEAR_TILES`]) — the tile's fallback, not an under-layer: a contribution
        // in any of the group's TESTED columns and the client draws no underwear there at all, leaving
        // the base skin it already blitted. Drawn before the equipment because the untested columns (the
        // belt, the tabard) still stack on top of it.
        for (col, layer, tested) in UNDERWEAR_TILES {
            if plan.iter().any(|s| s.layer == layer && s.column < tested) {
                continue;
            }
            let Some(path) = self.tex(race, sex, SECTION_UNDERWEAR, 0, skin, col) else {
                continue;
            };
            if let Ok(overlay) = read_texture_mip_chain(chain, path) {
                blit_over(&mut atlas, &overlay, EQUIP_TILES[layer]);
            }
        }
        // The equipment layers (decision 0074), in the one order [`equip_blits`] decides, each read
        // `_U`-first with the wearer's gender letter as the fallback (RF-0088 §7).
        for step in &plan {
            if let Some(overlay) = read_equip_region(chain, step.layer, step.texture, sex) {
                blit_over(&mut atlas, &overlay, EQUIP_TILES[step.layer]);
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

/// One equipment contribution the body composite blits, carrying the worn bodyslot it came from,
/// the layer/tile it lands in, and the `ItemDisplayInfo` region-texture name it draws.
///
/// This is the *plan* [`CharSections::composite_body`] executes, exposed so the instrument
/// (`benilla-extract charatlas`) reads the composite off the **same law the composite runs** rather
/// than a second transcription that can drift. "Which worn slot repainted this tile?" is the
/// question both reported outfit bugs turned on, and it was unanswerable without this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EquipBlit<'a> {
    /// The worn bodyslot, indexed as [`CharSections::composite_body`]'s `equipment` is (bodyslot − 2:
    /// 0 shirt · 1 chest · 2 belt · 3 pants · 4 boots · 5 wrist · 6 gloves · 7 tabard).
    pub slot: usize,
    /// The compositor layer — `ItemDisplayInfo` texture column *i* = layer *i* = atlas tile g*i*.
    pub layer: usize,
    /// The cell this contribution occupies in the layer's row — [`equip_column`]'s answer, which is
    /// the `[0x803bf8]` default unless the layer's chooser overruled it. Ascending = blitted later.
    pub column: i8,
    /// The `ItemDisplayInfo` region-texture name (no directory, no gender suffix).
    pub texture: &'a str,
}

/// The cell a worn slot's contribution to `layer` occupies — the `[0x803bf8]` default, unless the
/// layer's chooser overrules it (wow-re RF-0088 §5, byte-true; `0x479210` and `0x4793f0`).
///
/// **This is the fix for B327 and half of B326.** Only two of the eight choosers look past the
/// table, and both read an `ItemDisplayInfo.geosetGroup` — so what a garment *is* moves where it
/// paints:
///
/// - **ArmLower** (`0x479210`): gloves with a glove geoset, and a chest with a sleeve geoset, are
///   promoted clear of the plain cells (to 6 and 5) so a sleeved chest paints over a bracer.
/// - **LegLower** (`0x4793f0`): a **robe** (chest `geosetGroup[2]`) is promoted to **4**, above a
///   boot's 3-or-2 — so footwear paints *under* a robe's skirt, never over its hem. Trousers
///   carrying their own robe bit take 3 when the chest is a robe too, else 4.
///
/// Note the asymmetry that makes the bug: a *plain* chest stays at 1 and a boot sits at 2 or 3, so
/// boots do cover ordinary trousers on the shin — which is right, and is why reading the table as a
/// fixed priority looked correct for years. Only the robe inverts it.
pub fn equip_column(equipment: &[Option<&ItemDisplay>; 8], slot: usize, layer: usize) -> i8 {
    let group = |s: usize, j: usize| equipment[s].is_some_and(|d| d.geoset_groups[j] != 0);
    match (layer, slot) {
        // ArmLower — `0x479210`, one `0x4774f0(rec, 0)` up front, applied on two bodyslots.
        (1, SLOT_GLOVES) if group(slot, 0) => 6,
        (1, SLOT_CHEST) if group(slot, 0) => 5,
        // LegLower — `0x4793f0`. The boot geoset lifts a boot 2 → 3; the robe bit lifts a chest
        // 1 → 4, over both. The trouser leg reads the CHEST's robe bit as well: a robe over robe-
        // trousers puts the trousers at 3 (under the robe), otherwise they take 4 themselves.
        (6, SLOT_BOOTS) if group(slot, 0) => 3,
        (6, SLOT_CHEST) if group(slot, 2) => 4,
        (6, SLOT_PANTS) if group(slot, 2) => {
            if group(SLOT_CHEST, 2) {
                3
            } else {
                4
            }
        }
        _ => EQUIP_LAYER_COLUMN[slot][layer],
    }
}

/// The ordered equipment blits a dressed composite performs: per layer, each worn slot that clears
/// `0x478ad0`'s two gates (a cell that is not `-1` in the default table, and a **non-empty** texture
/// name — the client's test is on the string's first byte, not on the pointer) placed in the cell
/// [`equip_column`] gives it, then emitted by ascending cell index.
///
/// A row holds **one record per cell**, so two slots landing on the same cell do not stack — the
/// later writer replaces the earlier, which is what the client's `0x478900` store does. Reachable
/// only when robe-trousers meet a robe chest and booted feet; `equipment` order decides it here,
/// where the client's is equip order.
pub fn equip_blits<'a>(equipment: &[Option<&'a ItemDisplay>; 8]) -> Vec<EquipBlit<'a>> {
    let mut plan = Vec::new();
    for (layer, _tile) in EQUIP_TILES.iter().enumerate() {
        let mut row: [Option<EquipBlit<'a>>; 8] = [None; 8];
        for (slot, display) in equipment.iter().enumerate() {
            let Some(display) = display else { continue };
            if EQUIP_LAYER_COLUMN[slot][layer] < 0 {
                continue;
            }
            let Some(texture) = display.region_textures[layer]
                .as_deref()
                .filter(|name| !name.is_empty())
            else {
                continue;
            };
            let column = equip_column(equipment, slot, layer);
            if let Some(cell) = row.get_mut(column as usize) {
                *cell = Some(EquipBlit {
                    slot,
                    layer,
                    column,
                    texture,
                });
            }
        }
        plan.extend(row.into_iter().flatten());
    }
    plan
}

/// The atlas rect layer `layer`'s equipment contributions composite into — `(x, y, w, h)` in pixels
/// of the 256² body atlas (the RF-0062 bbox table). Out of range past layer 7.
pub fn equip_tile(layer: usize) -> Option<(u32, u32, u32, u32)> {
    EQUIP_TILES.get(layer).copied()
}

/// The `Item\TextureComponents\` subdirectory layer `layer` reads its art from.
pub fn equip_tex_dir(layer: usize) -> Option<&'static str> {
    EQUIP_TEX_DIRS.get(layer).copied()
}

/// Read one equipment region texture off the chain: `Item\TextureComponents\<dir>\<name>_<L>.blp`,
/// **`_U` first, the wearer's gender letter only if `_U` does not exist**. `None` when neither
/// decodes — the layer is skipped, best-effort like the skin overlays.
fn read_equip_region(chain: &mut Chain, layer: usize, name: &str, sex: u8) -> Option<BlpMipChain> {
    for path in equip_region_candidates(layer, name, sex) {
        if let Ok(mips) = read_texture_mip_chain(chain, &path) {
            return Some(mips);
        }
    }
    None
}

/// The chain paths a region texture is looked up under, in the order the composite tries them:
/// **`_U`, then the wearer's gender letter** (wow-re RF-0088 §7, byte-true).
///
/// The order is load-bearing and it is the inverse of what benilla shipped, which was **B326**.
/// `0x476e20` stages the literal `"U"` as the format's third `%s` before it computes anything else,
/// probes that exact path (`0x648a10`, an existence test), and patches the character's gender letter
/// over the `U` at `strlen-5` **only on a miss** — one byte, one branch, and no second attempt
/// anywhere in the image. So a gendered file that exists **loses to `_U` whenever `_U` also exists**.
///
/// Of 7944 shipped basenames, 5920 are `_U`-only (the probe hits), 1937 are `_M`+`_F` with no `_U`
/// (the patch is what those are for), and **43 ship both** — on those 43 the gendered art is dead.
/// `Leather_A_02_Pant_LL` is one: its `_F` is 18 rows shorter than its `_U`, so preferring `_F` left
/// a bare-skin ring below a night elf female's knee that the reference does not have.
///
/// Public so the instrument can report **which** candidate a contribution actually resolved to, and
/// name the ones that resolved to nothing — a silently-skipped region reads on the model as a
/// garment that stops early, which is exactly how it was first reported.
pub fn equip_region_candidates(layer: usize, name: &str, sex: u8) -> [String; 2] {
    let dir = EQUIP_TEX_DIRS[layer];
    let letter = if sex == 1 { 'F' } else { 'M' };
    ['U', letter].map(|c| format!("Item\\TextureComponents\\{dir}\\{name}_{c}.blp"))
}

/// Source-over composite of one region overlay's authored mip pyramid onto the body atlas at a fixed
/// tile. Per mip level `i` the destination tile is `(x>>i, y>>i, w>>i, h>>i)` and the overlay's level-`i`
/// pixels are read from its **own origin** — the RF-0067 overlay src rect (`src = (0,0)`, `dst = tile.xy`,
/// extent `tile.wh`), so the overlay BLP is authored exactly tile-sized. Straight 8-bit source-over per
/// channel (`out = src·a + dst·(1−a)`, `out_a = a + dst_a·(1−a)`): an opaque overlay (`a == 255`) is a
/// plain copy (the client's REPLACE), an alpha one blends. Levels past either pyramid's end, and the
/// sub-pixel remainder of a degenerate deep-mip tile, are clamped/skipped.
fn blit_over(dst: &mut BlpMipChain, src: &BlpMipChain, tile: Tile) {
    // The composite is a per-texel source-over blend, so both sides must be decoded pixels. Every
    // reader here goes through `read_texture_mip_chain` (never the block-passthrough twin) — this
    // says so out loud, because a chain that arrived as DXT blocks would blend garbage silently
    // rather than fail (decision 1626).
    debug_assert!(
        dst.is_rgba8() && src.is_rgba8(),
        "character-skin compositing needs decoded chains on both sides"
    );
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
pub(crate) fn char_sections_schema() -> Schema {
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

    /// A bare [`ItemDisplay`] carrying only the region textures and geoset groups a test cares about.
    fn worn(regions: [Option<&str>; 8], geoset_groups: [u32; 3]) -> ItemDisplay {
        ItemDisplay {
            region_textures: regions.map(|r| r.map(str::to_string)),
            geoset_groups,
            ..Default::default()
        }
    }

    /// Legwear + footwear + a chest, named so the cases below read as outfits.
    fn leg(tex: &str) -> [Option<&str>; 8] {
        [None, None, None, None, None, Some("lu"), Some(tex), None]
    }

    /// [`equip_blits`] is the composite's whole equipment law, so the two things a reported outfit
    /// defect turns on are pinned here: **which** slots reach a tile, and in **what cell order**.
    ///
    /// The `[0x803bf8]` value is a **default**, not a fixed priority (wow-re RF-0088 §5): the layer's
    /// chooser may overrule it from the item's `geosetGroup`, and the row is walked by ascending cell
    /// with later covering earlier. Reading it as a fixed priority is what shipped **B327**.
    #[test]
    fn equip_blits_places_each_slot_in_its_chooser_cell() {
        let plain_chest = worn(
            [
                Some("au"),
                Some("al"),
                None,
                Some("tu"),
                None,
                None,
                None,
                None,
            ],
            [0, 0, 0],
        );
        let sleeved_chest = worn(
            [
                Some("au"),
                Some("al"),
                None,
                Some("tu"),
                None,
                None,
                None,
                None,
            ],
            [1, 0, 0],
        );
        let bracer = worn(
            [None, Some("bracer_al"), None, None, None, None, None, None],
            [0, 0, 0],
        );
        let plain_gloves = worn(
            [
                None,
                Some("glove_al"),
                Some("glove_ha"),
                None,
                None,
                None,
                None,
                None,
            ],
            [0, 0, 0],
        );
        let geoset_gloves = worn(
            [
                None,
                Some("glove_al"),
                Some("glove_ha"),
                None,
                None,
                None,
                None,
                None,
            ],
            [1, 0, 0],
        );

        // ArmLower (`0x479210`): plain items take the table (shirt 0, chest 1, wrist 2, gloves 3);
        // a glove/sleeve geoset lifts gloves to 6 and the chest to 5, clear of the plain cells.
        let eq = [
            None,
            Some(&plain_chest),
            None,
            None,
            None,
            Some(&bracer),
            Some(&plain_gloves),
            None,
        ];
        let g1: Vec<_> = equip_blits(&eq)
            .into_iter()
            .filter(|s| s.layer == 1)
            .map(|s| (s.column, s.texture))
            .collect();
        assert_eq!(g1, [(1, "al"), (2, "bracer_al"), (3, "glove_al")]);
        let eq = [
            None,
            Some(&sleeved_chest),
            None,
            None,
            None,
            Some(&bracer),
            Some(&geoset_gloves),
            None,
        ];
        let g1: Vec<_> = equip_blits(&eq)
            .into_iter()
            .filter(|s| s.layer == 1)
            .map(|s| (s.column, s.texture))
            .collect();
        assert_eq!(
            g1,
            [(2, "bracer_al"), (5, "al"), (6, "glove_al")],
            "a sleeved chest paints over a bracer"
        );
    }

    /// **B327, the carve.** On LegLower the chooser `0x4793f0` lifts a *robe* (chest `geosetGroup[2]`)
    /// to cell 4 — above a boot's 3-or-2 — so footwear paints under a robe's skirt and can never
    /// repaint its hem. The control that must not move: a **plain** chest stays at 1 and boots still
    /// cover ordinary trousers, which is why the fixed-priority reading looked right for years.
    #[test]
    fn a_robe_outranks_footwear_on_leglower() {
        let robe = worn(leg("robe_ll"), [1, 0, 1]);
        let plain_chest = worn(leg("chest_ll"), [0, 0, 0]);
        let trousers = worn(leg("pant_ll"), [0, 0, 0]);
        let robe_trousers = worn(leg("robetrouser_ll"), [0, 0, 1]);
        let shoes = worn(
            [
                None,
                None,
                None,
                None,
                None,
                None,
                Some("shoe_ll"),
                Some("shoe_fo"),
            ],
            [0, 0, 0],
        );
        let boots = worn(
            [
                None,
                None,
                None,
                None,
                None,
                None,
                Some("boot_ll"),
                Some("boot_fo"),
            ],
            [3, 0, 0],
        );

        fn g6(eq: &[Option<&ItemDisplay>; 8]) -> Vec<(i8, String)> {
            equip_blits(eq)
                .into_iter()
                .filter(|s| s.layer == 6)
                .map(|s| (s.column, s.texture.to_string()))
                .collect()
        }
        /// The expected cells, spelled the way the assertions read.
        fn cells(want: &[(i8, &str)]) -> Vec<(i8, String)> {
            want.iter().map(|(c, t)| (*c, t.to_string())).collect()
        }

        // The report: a robe with sandals (no boot geoset) and with real boots. Either way the robe
        // is last, so the skirt's hem is the robe's own art.
        let eq = [
            None,
            Some(&robe),
            None,
            None,
            Some(&shoes),
            None,
            None,
            None,
        ];
        assert_eq!(
            g6(&eq),
            cells(&[(2, "shoe_ll"), (4, "robe_ll")]),
            "sandals under the robe"
        );
        let eq = [
            None,
            Some(&robe),
            None,
            None,
            Some(&boots),
            None,
            None,
            None,
        ];
        assert_eq!(
            g6(&eq),
            cells(&[(3, "boot_ll"), (4, "robe_ll")]),
            "boots under the robe too"
        );

        // The control: a plain chest does NOT get lifted, so boots still cover ordinary trousers.
        let eq = [
            None,
            Some(&plain_chest),
            None,
            Some(&trousers),
            Some(&boots),
            None,
            None,
            None,
        ];
        assert_eq!(
            g6(&eq),
            cells(&[(0, "pant_ll"), (1, "chest_ll"), (3, "boot_ll")]),
            "footwear still paints over trousers"
        );

        // Trousers carrying their own robe bit: 3 under a robe chest, 4 when there is no robe over
        // them. The first case collides with a geoset-boot's 3 — one cell holds one record, and the
        // later writer (the boots) wins, exactly as `0x478900` overwrites.
        let eq = [
            None,
            Some(&robe),
            None,
            Some(&robe_trousers),
            None,
            None,
            None,
            None,
        ];
        assert_eq!(g6(&eq), cells(&[(3, "robetrouser_ll"), (4, "robe_ll")]));
        let eq = [
            None,
            Some(&plain_chest),
            None,
            Some(&robe_trousers),
            None,
            None,
            None,
            None,
        ];
        assert_eq!(g6(&eq), cells(&[(1, "chest_ll"), (4, "robetrouser_ll")]));
    }

    /// `0x478ad0`'s two gates: a `-1` cell is a hard "this slot never touches this layer", and the
    /// texture test is on the **string**, not the pointer — an empty name contributes nothing.
    #[test]
    fn equip_blits_drops_ungated_and_empty_contributions() {
        let odd = worn(
            [None, None, None, Some("boot_tu"), None, None, None, None],
            [0, 0, 0],
        );
        let eq = [None, None, None, None, Some(&odd), None, None, None];
        assert!(
            equip_blits(&eq).is_empty(),
            "a -1 cell drops the contribution entirely"
        );

        let blank = worn(
            [None, None, None, Some(""), None, None, None, None],
            [0, 0, 0],
        );
        let eq = [None, Some(&blank), None, None, None, None, None, None];
        assert!(
            equip_blits(&eq).is_empty(),
            "an empty texture name is not a contribution"
        );
    }

    /// **B326.** The region filename resolves `_U` FIRST and falls back to the wearer's gender letter
    /// only when `_U` is absent (wow-re RF-0088 §7) — the inverse of what benilla shipped. On the 43
    /// basenames that carry both, the gendered art is dead: `Leather_A_02_Pant_LL_F` is 18 rows
    /// shorter than its `_U`, and preferring it left a bare ring below a night elf female's knee.
    #[test]
    fn region_textures_resolve_unisex_before_the_gender_letter() {
        let female = equip_region_candidates(6, "Leather_A_02_Pant_LL", 1);
        assert_eq!(
            female,
            [
                "Item\\TextureComponents\\LegLowerTexture\\Leather_A_02_Pant_LL_U.blp",
                "Item\\TextureComponents\\LegLowerTexture\\Leather_A_02_Pant_LL_F.blp",
            ]
        );
        let male = equip_region_candidates(6, "Leather_A_02_Pant_LL", 0);
        assert!(male[0].ends_with("_U.blp") && male[1].ends_with("_M.blp"));
        // There is no third attempt anywhere in the image — a bare name never resolves.
        assert_eq!(male.len(), 2);
    }

    /// A single-level RGBA helper for the blit test.
    fn chain(width: u32, height: u32, px: Vec<u8>) -> BlpMipChain {
        BlpMipChain {
            width,
            height,
            texels: crate::BlpTexels::Rgba8Unorm,
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

    /// End-to-end regression on the **real** build-5875 files: compositing a Human-male body must yield
    /// a 256² mip pyramid whose head (g8/g9) + pelvis (g5) tiles are overlaid from the base, while a
    /// control tile that carries no naked-body overlay (g3) is untouched. Guards the section→tile map +
    /// the CharSections schema against a silent break (a wrong tile or a shifted column would move/lose
    /// these diffs). Skips when the client data isn't present.
    #[test]
    fn composite_body_overlays_land_on_real_human_male() {
        let data = crate::wow_data_or_skip!();
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
        // The torso tile is the underwear's second column — and **no male row authors it** (the bra is
        // female-only; every sectionType-4 male row leaves `TextureName[1]` empty), so a human male's g3
        // must stay byte-identical to the base. The female half is the next test.
        assert_eq!(
            changed(TILE_G3),
            0,
            "no male underwear row authors the naked-torso column"
        );

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

    /// The underwear section spends **both** its texture columns (bug B325): `TextureName[0]` is the
    /// pelvis (g5) and `TextureName[1]` the naked **torso** (g3) — the bra, authored only on the female
    /// rows. Before this, a bare-chested female composited panties and nothing above them.
    ///
    /// The pin is byte-exact rather than a diff count: `…NakedTorsoSkin00_00.blp` is a 128×64 fully
    /// opaque sheet (BLP alphaDepth 0 → the client's REPLACE), so if it lands at the right tile from its
    /// own origin the composite's g3 rect must equal the file's mip 0 pixel-for-pixel. A wrong tile, a
    /// wrong column, or a src-origin slip all break that equality.
    #[test]
    fn composite_body_dresses_the_female_torso_underwear() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cs = CharSections::load(&mut chain).expect("load CharSections");
        // Night elf female, skinColor 0 — the appearance the B325 report shipped a screenshot of.
        let (race, sex, skin) = (4u8, 1u8, 0u8);
        assert_eq!(
            cs.tex(race, sex, SECTION_UNDERWEAR, 0, skin, 1),
            Some("Character\\NightElf\\Female\\NightElfFemaleNakedTorsoSkin00_00.blp"),
            "the sectionType-4 row's second column is the naked torso"
        );
        let torso = read_texture_mip_chain(
            &mut chain,
            "Character\\NightElf\\Female\\NightElfFemaleNakedTorsoSkin00_00.blp",
        )
        .expect("read naked torso");
        assert_eq!(
            (torso.width, torso.height),
            (128, 64),
            "the sheet is authored exactly tile-sized"
        );
        let comp = cs
            .composite_body(&mut chain, race, sex, skin, 0, 0, 0, 0, [None; 8])
            .expect("composite ok")
            .expect("base skin row present");

        let (tx, ty, tw, th) = TILE_G3;
        for row in 0..th {
            let d = ((ty + row) * 256 + tx) as usize * 4;
            let s = (row * tw) as usize * 4;
            assert_eq!(
                &comp.mips[0][d..d + tw as usize * 4],
                &torso.mips[0][s..s + tw as usize * 4],
                "g3 row {row} is the naked-torso sheet verbatim"
            );
        }
    }

    /// The underwear is the tile's **fallback**, and only the group's TESTED columns suppress it
    /// (wow-re RF-0086; the byte-derived prefix in [`UNDERWEAR_TILES`]). This is the discriminating
    /// pin, because the shipped art cannot make it: a real chest or a real pair of pants repaints
    /// its whole tile opaquely, so "the underwear was suppressed" and "the underwear was painted
    /// over" produce the same pixels.
    ///
    /// So the fixture is a display that **occupies a region column while painting nothing** — its
    /// texture name resolves to no shipped file, so `read_equip_region` finds nothing to blit while
    /// the column is still taken. That is the client's own gate: `0x4772f0`/`0x4773a0` test the
    /// composite *cell*, not the equipped item. The same name at a different bodyslot lands in a
    /// different column, and the outcome flips — which is the whole claim.
    #[test]
    fn only_the_tested_equipment_columns_suppress_the_underwear() {
        let data = crate::wow_data_or_skip!();
        let mut chain = crate::open_chain(&data).expect("open chain");
        let cs = CharSections::load(&mut chain).expect("load CharSections");
        let (race, sex, skin) = (4u8, 1u8, 0u8); // night elf female, skinColor 0
        let dir = "Character\\NightElf\\Female\\NightElfFemale";
        let mut read = |n: &str| {
            read_texture_mip_chain(&mut chain, &format!("{dir}{n}00_00.blp")).expect("read sheet")
        };
        let (base, torso, pelvis) = (
            read("Skin"),
            read("NakedTorsoSkin"),
            read("NakedPelvisSkin"),
        );

        // The mip-0 pixels of a tile, out of a `stride`-wide image.
        let rect = |img: &BlpMipChain, (x, y, w, h): Tile, stride: u32| -> Vec<u8> {
            (0..h)
                .flat_map(|r| {
                    let o = (((y + r) * stride + x) * 4) as usize;
                    img.mips[0][o..o + (w * 4) as usize].to_vec()
                })
                .collect()
        };
        let sheet = |img: &BlpMipChain| rect(img, (0, 0, 128, 64), 128);
        let (torso_sheet, pelvis_sheet) = (sheet(&torso), sheet(&pelvis));
        let (base_g3, base_g5) = (rect(&base, TILE_G3, 256), rect(&base, TILE_G5, 256));

        // A display that TAKES the column without painting it (see the doc above).
        let occupies = |layers: &[usize]| {
            let mut region_textures: [Option<String>; 8] = Default::default();
            for l in layers {
                region_textures[*l] = Some("benilla-no-such-region".into());
            }
            ItemDisplay {
                region_textures,
                ..Default::default()
            }
        };
        // bodyslot − 2: 0 shirt · 1 chest · 2 belt · 3 pants · 7 tabard.
        let (shirt, chest, belt, pants, tabard) = (
            occupies(&[3]),
            occupies(&[3, 5]), // a robe reaches both groups
            occupies(&[5]),
            occupies(&[5]),
            occupies(&[3]),
        );
        fn worn<'a>(slots: &[(usize, &'a ItemDisplay)]) -> [Option<&'a ItemDisplay>; 8] {
            let mut e: [Option<&ItemDisplay>; 8] = [None; 8];
            for (i, d) in slots {
                e[*i] = Some(d);
            }
            e
        }

        // Byte-exact, but reported as a texel count and a name — a raw 32 KiB `assert_eq!` dump of a
        // 128×64 RGBA tile is unreadable, and the useful fact is *which* of the three it is not.
        let differing = |got: &[u8], want: &[u8]| {
            got.chunks_exact(4)
                .zip(want.chunks_exact(4))
                .filter(|(a, b)| a != b)
                .count()
        };
        // (label, equipment, expected g3, expected g5)
        type Want<'a> = (&'a str, &'a Vec<u8>);
        let cases: [(&str, [Option<&ItemDisplay>; 8], Want, Want); 6] = [
            (
                "naked",
                worn(&[]),
                ("the bra", &torso_sheet),
                ("the panties", &pelvis_sheet),
            ),
            // Shirt is TorsoUpper column 0 — tested. Torso suppressed; the pelvis is a different group.
            (
                "shirt",
                worn(&[(0, &shirt)]),
                ("bare skin", &base_g3),
                ("the panties", &pelvis_sheet),
            ),
            // A tabard occupies TorsoUpper too, but past the tested prefix: the bra still draws.
            (
                "tabard",
                worn(&[(7, &tabard)]),
                ("the bra", &torso_sheet),
                ("the panties", &pelvis_sheet),
            ),
            // Legs is LegUpper column 0 — tested. Pelvis suppressed, torso untouched.
            (
                "pants",
                worn(&[(3, &pants)]),
                ("the bra", &torso_sheet),
                ("bare skin", &base_g5),
            ),
            // A belt is LegUpper column 2, past the prefix: the panties survive it.
            (
                "belt",
                worn(&[(2, &belt)]),
                ("the bra", &torso_sheet),
                ("the panties", &pelvis_sheet),
            ),
            // A robe reaches both groups at column 1, and suppresses both.
            (
                "chest",
                worn(&[(1, &chest)]),
                ("bare skin", &base_g3),
                ("bare skin", &base_g5),
            ),
        ];
        for (label, equipment, (g3_want, g3), (g5_want, g5)) in cases {
            let comp = cs
                .composite_body(&mut chain, race, sex, skin, 0, 0, 0, 0, equipment)
                .expect("composite ok")
                .expect("base skin row present");
            for (tile, name, want_name, want) in [
                (TILE_G3, "torso", g3_want, g3),
                (TILE_G5, "pelvis", g5_want, g5),
            ] {
                let n = differing(&rect(&comp, tile, 256), want);
                assert_eq!(
                    n, 0,
                    "{label}: the {name} tile is not {want_name} ({n}/8192 texels)"
                );
            }
        }
        // The fixture only means something if the three expectations are actually distinguishable.
        assert_ne!(torso_sheet, base_g3, "the bra differs from bare skin");
        assert_ne!(pelvis_sheet, base_g5, "the panties differ from bare skin");
    }

    /// Equipment layers on the **real** files (decision 0074): dressing the Human male in One's
    /// starter kit (shirt 9891 / pants 9892 / boots 10141) must repaint exactly the tiles those
    /// displays' region columns name — torso from the shirt, LegUpper from the pants, Foot from the
    /// boots — and leave a tile no item touches (g2 Hand) byte-identical to the naked composite.
    /// Also pins the boots-over-pants stacking on the shared LegLower tile (both contribute; the
    /// priority table puts the boot layer on top, so g6 must differ from the pants-only composite).
    #[test]
    fn composite_body_equipment_layers_land_on_real_human_male() {
        let data = crate::wow_data_or_skip!();
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
