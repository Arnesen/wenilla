use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::math::Rect;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::window::PrimaryWindow;
use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, Wrap};

use benilla_assets::{LockRecover, WorldAssets};

use super::DEFAULT_FONT_SIZE;

/// The base set of sizes baked into the atlas **for every registered face** — the distinct
/// heights our shipped `Fonts.xml` objects use (`GameFontNormalSmall` 10 … `GameFontNormalHuge`
/// 20). A `FontString` requesting a height not in its face's baked set snaps to the nearest baked
/// size ([`nearest_size`] via [`UiFontAtlas::snap_for`] — per face, so a face never snaps to a
/// size only another face bakes).
const FONT_SIZES: &[f32] = &[
    10.0, 12.0, 13.0, 14.0, 15.0, 16.0, 18.0, 20.0, 24.0, 36.0, 48.0,
];

/// Extra sizes baked for **Friz Quadrata only** — the zone-splash fonts (decision 0287, sizes
/// corrected by the one-to-one fold-back): `SubZoneTextFont` 26, and 32 = the
/// [`super::FONTSTRING_EM_CAP`] every over-cap request lands on (`ZoneTextFont`/
/// `WorldMapTextFont`'s nominal 102, the world-map label's `SetFont` 33) — baked exact so the
/// splash never snaps off its lawful size. The original 102 bake is gone with the law that
/// capped it: no UI request can reach it anymore, and a retina 102 was a 204px raster per glyph.
/// Friz is the only face any shipped `Fonts.xml` object names at these heights; baking them for
/// all four faces would cost glyphs nothing draws.
const FRIZ_SPLASH_SIZES: &[f32] = &[26.0, 32.0];

/// The four client TTFs (verified present in `fonts.MPQ`, plain TTF — no exotic wrapper), read through
/// the app's own patch chain rather than `std::fs` (there is no `std::fs` path to client data — see
/// [`WorldAssets`]). Index 0 (Friz Quadrata) is the only one baked into [`UiFontAtlas`] in v1; see the
/// module doc.
const CLIENT_FONTS: &[&str] = &[
    "Fonts\\FRIZQT__.TTF",
    "Fonts\\ARIALN.TTF",
    "Fonts\\MORPHEUS.TTF",
    "Fonts\\SKURRI.TTF",
];

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Font loading — ported from probes/text-glyph/src/font.rs
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The face's baseline ascender as a fraction of the em — `hhea.asc / (hhea.asc + |hhea.desc|)` —
/// straight from the raw sfnt bytes. This is the term in the client's glyph *placement* law: the
/// ink hangs from the pixel ascender `[CGxFont+0x17c] = round(em · asc/(asc+|desc|))`, threaded
/// unchanged into `glyph_vplace` (0x5d1360) as the operand that fixes `baseline = cellTop +
/// ascender` (wow-re `system/font`, §5-verified 2026-07-09: the call chain `0x5ca160 → 0x5d1120
/// [ebp+0xc] → 0x5d1360 [ebp+0x10]`, where the cell's own `[[FT_Face+0x54]+0x68]` is the
/// per-glyph `bitmap_top`, the *subordinate* operand). This is the `ComputeRasterMetrics`
/// `load_param`; the FreeType scaled hhea ascender (`asc/upem` ≈ 0.965) appears NOWHERE in the
/// placement path — seating with it drops every line ~3px too low for Friz (965/1215 ≈ 0.794 vs
/// 0.965 → baseline row 10 vs 13 in a 13-tall cell). A tiny table-directory walk (`hhea` →
/// ascender@+4, descender@+6); `None` on any malformed/missing table.
fn hhea_ascent_ratio(bytes: &[u8]) -> Option<f32> {
    let num = u16::from_be_bytes(bytes.get(4..6)?.try_into().ok()?) as usize;
    let (mut asc, mut desc) = (None, None);
    for i in 0..num {
        let rec = bytes.get(12 + 16 * i..12 + 16 * i + 16)?;
        let toff = u32::from_be_bytes(rec[8..12].try_into().ok()?) as usize;
        if &rec[0..4] == b"hhea" {
            asc = Some(i16::from_be_bytes(bytes.get(toff + 4..toff + 6)?.try_into().ok()?) as f32);
            desc = Some(i16::from_be_bytes(bytes.get(toff + 6..toff + 8)?.try_into().ok()?) as f32);
        }
    }
    let (asc, desc) = (asc?, desc?);
    // The denominator is narrowed to f32 exactly as the binary does (`fstp m32` @0x5ca0d?),
    // then the ratio; `em · ratio + 0.5` floors to the load_param at the call site below.
    let denom = asc + desc.abs();
    (denom > 0.0 && asc > 0.0).then_some(asc / denom)
}

/// Registers `bytes` (raw TTF) into `font_system`'s `fontdb`, returning `(face_id, family_name)` — the
/// family name is read back off the just-loaded face so callers build an exact `Attrs::family` match
/// with no reliance on hardcoding Blizzard's font-name strings.
fn register_font(
    font_system: &mut FontSystem,
    bytes: Vec<u8>,
) -> anyhow::Result<(fontdb::ID, String)> {
    let source = fontdb::Source::Binary(
        std::sync::Arc::new(bytes) as std::sync::Arc<dyn AsRef<[u8]> + Sync + Send>
    );
    let ids = font_system.db_mut().load_font_source(source);
    let id = *ids
        .first()
        .ok_or_else(|| anyhow::anyhow!("font source produced no faces (not a valid TTF?)"))?;
    let family = font_system
        .db_mut()
        .face(id)
        .ok_or_else(|| anyhow::anyhow!("face {id:?} vanished right after loading"))?
        .families
        .first()
        .map(|(name, _)| name.clone())
        .ok_or_else(|| anyhow::anyhow!("face {id:?} carries no family name"))?;
    Ok((id, family))
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Glyph atlas — ported from probes/text-glyph/src/atlas.rs
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// `(font, glyph, size, outline_radius)` — the atlas's lookup key. `size_bits` is `f32::to_bits()`
/// of the exact logical size the glyph was rasterized at; [`layout_text_quads`] must shape at the
/// same size it looks up. `outline_radius` selects the cell VARIANT: `0` = the plain coverage cell
/// (white × coverage — every face/size bakes one; all measurement reads these, advances are
/// variant-invariant), `1`/`2` = the NORMAL/THICK **outlined composite** cell (black ring + white
/// fill in one texture, the real client's baked-outline architecture — its blit dispatch `0x5cf310`
/// selects an outline blit variant by font flags, so ring and fill live in ONE cell and fade as one
/// under frame alpha; the fade-composite fold-back record). Outlined variants exist only for the
/// `(face, size, radius)` triples the shipped `Fonts.xml` registry actually names — an unplanned
/// combo misses and [`layout_text_quads`] falls back to the legacy stamped halos.
pub(super) type GlyphKey = (fontdb::ID, u16, u32, u8);

/// The baked-cell radius for a font's outline flag: the same `r` the legacy stamp synthesis used
/// (`r=1` NORMAL, `r=2` THICK — logical px), now the bake-time ring reach and the lookup key's
/// variant discriminant.
pub(super) fn outline_radius(outline: benilla_ui::script::Outline) -> u8 {
    match outline {
        benilla_ui::script::Outline::None => 0,
        benilla_ui::script::Outline::Normal => 1,
        benilla_ui::script::Outline::Thick => 2,
    }
}

/// One packed glyph's metadata — atlas UV rect, bitmap size, bearing (from the rasterized bitmap's
/// origin to its top-left, swash convention: `left` rightward, `top` upward from the baseline), and
/// the shaped advance width (`LayoutGlyph::w`, cosmic-text's own hitbox width — kerned/hinted, but
/// **not** the RE font kernel's bit-exact advance; see the module doc's fidelity note).
///
/// **All fields are PHYSICAL px** — the glyph was baked at `logical_size × bake_scale` (the DPI-aware
/// bake, [`UiFontAtlas::scale`]). [`layout_text_quads`]/[`measure_text`] divide these back by
/// `scale` to lay out in logical units, then snap positions to the physical-pixel grid; the net effect
/// is a bitmap rasterized at device resolution and drawn 1 texel : 1 physical pixel (crisp), not a
/// half-resolution bitmap upscaled by the retina framebuffer (the pre-DPI blur).
#[derive(Clone, Copy, Debug)]
pub(super) struct GlyphInfo {
    pub(super) uv: Rect,
    pub(super) px_w: f32,
    pub(super) px_h: f32,
    pub(super) bearing_x: f32,
    pub(super) bearing_top: f32,
    pub(super) advance: f32,
}

/// Simple shelf (row) packer: places rects left-to-right, wraps to a new row when a rect wouldn't fit,
/// 1px padding on every edge to avoid bilinear-sample bleed between neighbours.
struct ShelfPacker {
    width: u32,
    cursor_x: u32,
    cursor_y: u32,
    row_height: u32,
}

impl ShelfPacker {
    fn new(width: u32) -> Self {
        Self {
            width,
            cursor_x: 0,
            cursor_y: 0,
            row_height: 0,
        }
    }

    fn place(&mut self, w: u32, h: u32) -> (u32, u32) {
        if self.cursor_x + w + 1 > self.width {
            self.cursor_x = 0;
            self.cursor_y += self.row_height + 1;
            self.row_height = 0;
        }
        let pos = (self.cursor_x, self.cursor_y);
        self.cursor_x += w + 1;
        self.row_height = self.row_height.max(h);
        pos
    }

    fn total_height(&self) -> u32 {
        (self.cursor_y + self.row_height).max(1)
    }
}

/// A baked cell's texel payload: plain coverage (blitted as white × alpha) or a pre-composited
/// RGBA cell (the outlined variants — black ring + white fill, blitted verbatim).
enum CellData {
    Coverage(Vec<u8>), // one byte/px
    Rgba(Vec<u8>),     // four bytes/px, straight alpha
}

struct RasterEntry {
    key: GlyphKey,
    w: u32,
    h: u32,
    data: CellData,
    bearing_x: f32,
    bearing_top: f32,
    advance: f32,
}

/// The client's AA neighbour-count → outline-alpha LUT (`DAT_0080a8ec`, wow-re
/// `system/font/scratch/outline-bake-tint.md`, §5 double-pair + difftests, commit `f80ce699`):
/// index = the number of marked cells in the in-bounds 3×3 box (centre included), value = the
/// 4-bit alpha, here pre-widened to 8-bit (`nibble × 17`).
const AA_NEIGHBOUR_LUT: [u8; 10] = [0, 17, 17, 51, 85, 119, 153, 187, 221, 255];

/// Composite one glyph's coverage bitmap into an **outlined cell** — the byte recipe of the
/// client's AA-outline blit (`glyph_blit_aa_outline 0x5cea30`, dispatched by `0x5cf310`; wow-re
/// `outline-bake-tint.md`, difftested emulation `wow-5875-re/crates/font/src/raster.rs`):
///
/// 1. **Mark** every texel with non-zero coverage.
/// 2. **Dilate** iteratively — each pass marks every virgin texel 8-adjacent to a marked one.
///    The binary runs 1 pass for NORMAL, 2 for THICK; we run `r × round(scale)` so the ring keeps
///    its logical weight under the DPI-aware physical bake (at `scale = 1` this IS the byte
///    recipe; the finer grid at retina is the same deliberate resolution upgrade as the glyph
///    bake itself).
/// 3. **Alpha** per texel = [`AA_NEIGHBOUR_LUT`]`[count of marked cells in the in-bounds 3×3
///    box]` — the ring AND the fill edge take the same neighbourhood-graded alpha (interior = 9
///    marked ⇒ opaque; a THICK ring's outer corners grade down to ~⅓ — the real softer outer
///    edge).
/// 4. **Pack**: unmarked ⇒ transparent; zero-coverage (pure ring) ⇒ black at the LUT alpha; else
///    `RGB = the coverage as gray` (the fill ramps *toward the ring's black* at AA edges — not
///    white-with-thin-alpha) at the LUT alpha. The binary quantizes to ARGB4444; we keep 8-bit
///    (same law, no banding — the one deliberate widening, like the LUT `×17`).
///
/// Draw-side law this feeds (same note, prongs 2–3): one quad per glyph, vertex color MODULATE —
/// white fill takes the text tint, black ring stays black — and ONE alpha per pass, so a frame
/// fade thins ring+fill together (no `α(1−α)` blackening, the defect the stamped halos had).
///
/// Returns `(rgba, out_w, out_h, pad)`: the cell grows by `pad = r·round(scale)` texels each side
/// (the binary's cell `em+2/+4` and origin col `1/2`, generalized); the caller shifts bearings by
/// `pad`. The advance is untouched — the step law owns tracking (THICK `+1`, NORMAL none).
fn outlined_cell(cov: &[u8], w: u32, h: u32, r: u8, scale: f32) -> (Vec<u8>, u32, u32, u32) {
    let passes = (u32::from(r) * (scale.round().max(1.0) as u32)).max(1);
    let pad = passes;
    let (out_w, out_h) = (w + 2 * pad, h + 2 * pad);
    let cells = (out_w * out_h) as usize;

    // 1. Mark coverage into the map (1 = glyph ink), coverage kept alongside for the pack.
    let mut map = vec![0u8; cells];
    for row in 0..h {
        for col in 0..w {
            if cov[(row * w + col) as usize] != 0 {
                map[((row + pad) * out_w + (col + pad)) as usize] = 1;
            }
        }
    }

    // 2. Iterative 8-neighbour dilation: pass k marks virgin cells adjacent to any marked cell
    //    (the binary's mask-1-write-2 / mask-3-write-4 passes, generalized to k marks).
    for pass in 0..passes {
        let mark = (pass + 2) as u8; // 1 = ink, 2.. = ring generations
        for row in 0..out_h {
            for col in 0..out_w {
                let c = (row * out_w + col) as usize;
                if map[c] != 0 {
                    continue;
                }
                'probe: for dr in -1i32..=1 {
                    for dc in -1i32..=1 {
                        let (rr, cc) = (row as i32 + dr, col as i32 + dc);
                        if rr < 0 || rr >= out_h as i32 || cc < 0 || cc >= out_w as i32 {
                            continue;
                        }
                        let n = map[(rr as u32 * out_w + cc as u32) as usize];
                        if n != 0 && n < mark {
                            map[c] = mark;
                            break 'probe;
                        }
                    }
                }
            }
        }
    }

    // 3.+4. Neighbourhood-count alpha + pack.
    let mut rgba = vec![0u8; cells * 4];
    for row in 0..out_h {
        for col in 0..out_w {
            let c = (row * out_w + col) as usize;
            if map[c] == 0 {
                continue;
            }
            let mut count = 0usize;
            for dr in -1i32..=1 {
                for dc in -1i32..=1 {
                    let (rr, cc) = (row as i32 + dr, col as i32 + dc);
                    if rr < 0 || rr >= out_h as i32 || cc < 0 || cc >= out_w as i32 {
                        continue;
                    }
                    if map[(rr as u32 * out_w + cc as u32) as usize] != 0 {
                        count += 1;
                    }
                }
            }
            let alpha = AA_NEIGHBOUR_LUT[count];
            let fill = if row >= pad && row < pad + h && col >= pad && col < pad + w {
                cov[((row - pad) * w + (col - pad)) as usize]
            } else {
                0
            };
            let idx = c * 4;
            // Pure ring: black. Fill: the coverage as gray (ramps toward the ring at AA edges).
            rgba[idx] = fill;
            rgba[idx + 1] = fill;
            rgba[idx + 2] = fill;
            rgba[idx + 3] = alpha;
        }
    }
    (rgba, out_w, out_h, pad)
}

/// The built atlas bitmap plus its glyph table. Chars with no glyph in any baked font end up in
/// `missing` (reported, not fatal).
struct BuiltAtlas {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    glyphs: HashMap<GlyphKey, GlyphInfo>,
    missing: Vec<char>,
}

/// Rasterize `charset` for every `(font_id, family_name)` in `fonts`, at every size in `sizes`, into
/// one packed atlas — every glyph baked at **zero subpixel offset** (the standard bitmap-font-atlas
/// simplification; see `probes/text-glyph`'s report §7 for where this does/doesn't matter — never for
/// game UI at integer-pixel placement).
///
/// **DPI-aware:** each logical `size` is shaped and rasterized at `size × scale` (the window's
/// `scale_factor`), so on a retina display the bitmap carries the full device resolution instead of a
/// half-res upscale. The glyph is *keyed* by its **logical** size (`size.to_bits()`) — the physical
/// scale is an implementation detail of the bake, transparent to the lookup — and every stored metric
/// (advance/bearing/bitmap size) is in physical px, divided back by `scale` at layout time.
fn build_atlas(
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    fonts: &[(fontdb::ID, String, Vec<f32>)],
    charset: &[char],
    atlas_width: u32,
    scale: f32,
    // The outlined-cell bake plan: `(family, logical-size bits) → ring radii` — the distinct
    // outlined pairings the shipped font-object registry names (census in
    // [`load_fonts_and_build_atlas`]). Each planned radius bakes an [`outlined_cell`] variant
    // alongside the plain cell.
    outlined: &HashMap<(String, u32), Vec<u8>>,
) -> BuiltAtlas {
    let mut rasters: Vec<RasterEntry> = Vec::new();
    let mut missing = Vec::new();

    for (_font_id, family, sizes) in fonts {
        let attrs = Attrs::new().family(Family::Name(family));
        for &size in sizes.iter() {
            let radii: &[u8] = outlined
                .get(&(family.clone(), size.to_bits()))
                .map_or(&[], Vec::as_slice);
            // Shape + rasterize at the physical size; key by the logical size so layout looks up by
            // the height a `FontString` asks for, never by the retina-dependent physical size.
            let phys = size * scale;
            for &ch in charset {
                let mut one = [0u8; 4];
                let s = ch.encode_utf8(&mut one);

                let mut buffer = Buffer::new(font_system, Metrics::new(phys, phys));
                buffer.set_wrap(font_system, Wrap::None);
                buffer.set_text(font_system, s, &attrs, Shaping::Advanced, None);
                buffer.shape_until_scroll(font_system, false);

                let mut found = false;
                for run in buffer.layout_runs() {
                    for glyph in run.glyphs {
                        found = true;
                        // A lone first glyph on the line has x == y == 0.0, so this is the
                        // zero-subpixel canonical rasterization for (font, glyph, physical size).
                        let physical = glyph.physical((0.0, 0.0), 1.0);
                        let key: GlyphKey = (glyph.font_id, glyph.glyph_id, size.to_bits(), 0);
                        let advance = glyph.w;
                        if let Some(img) = swash_cache.get_image(font_system, physical.cache_key) {
                            if img.placement.width > 0 && img.placement.height > 0 {
                                let (w, h) = (img.placement.width, img.placement.height);
                                // The planned outlined variants: the composite ring+fill cell,
                                // bearings shifted out by the pad, advance untouched.
                                for &r in radii {
                                    let (rgba, ow, oh, pad) =
                                        outlined_cell(&img.data, w, h, r, scale);
                                    rasters.push(RasterEntry {
                                        key: (key.0, key.1, key.2, r),
                                        w: ow,
                                        h: oh,
                                        data: CellData::Rgba(rgba),
                                        bearing_x: img.placement.left as f32 - pad as f32,
                                        bearing_top: img.placement.top as f32 + pad as f32,
                                        advance,
                                    });
                                }
                                rasters.push(RasterEntry {
                                    key,
                                    w,
                                    h,
                                    data: CellData::Coverage(img.data.clone()),
                                    bearing_x: img.placement.left as f32,
                                    bearing_top: img.placement.top as f32,
                                    advance,
                                });
                            } else {
                                // Whitespace / zero-ink glyph (e.g. space): keep the advance, no
                                // bitmap — for the planned outlined radii too, so an outlined
                                // string's spaces hit their variant key instead of falling back.
                                for &r in radii {
                                    rasters.push(RasterEntry {
                                        key: (key.0, key.1, key.2, r),
                                        w: 0,
                                        h: 0,
                                        data: CellData::Coverage(Vec::new()),
                                        bearing_x: 0.0,
                                        bearing_top: 0.0,
                                        advance,
                                    });
                                }
                                rasters.push(RasterEntry {
                                    key,
                                    w: 0,
                                    h: 0,
                                    data: CellData::Coverage(Vec::new()),
                                    bearing_x: 0.0,
                                    bearing_top: 0.0,
                                    advance,
                                });
                            }
                        }
                    }
                }
                if !found {
                    missing.push(ch);
                }
            }
        }
    }

    let mut packer = ShelfPacker::new(atlas_width);
    let placements: Vec<(u32, u32)> = rasters
        .iter()
        .map(|r| {
            if r.w == 0 || r.h == 0 {
                (0, 0)
            } else {
                packer.place(r.w, r.h)
            }
        })
        .collect();
    let atlas_height = packer.total_height();

    let mut rgba = vec![0u8; (atlas_width as usize) * (atlas_height as usize) * 4];
    let mut glyphs = HashMap::with_capacity(rasters.len());
    for (r, (x, y)) in rasters.iter().zip(placements.iter().copied()) {
        if r.w > 0 && r.h > 0 {
            for row in 0..r.h {
                for col in 0..r.w {
                    let px = x + col;
                    let py = y + row;
                    let idx = ((py * atlas_width + px) * 4) as usize;
                    match &r.data {
                        // Plain coverage: white × alpha (vertex color supplies the ink color).
                        CellData::Coverage(cov) => {
                            rgba[idx] = 255;
                            rgba[idx + 1] = 255;
                            rgba[idx + 2] = 255;
                            rgba[idx + 3] = cov[(row * r.w + col) as usize];
                        }
                        // Pre-composited outlined cell: verbatim (black ring, white fill).
                        CellData::Rgba(cell) => {
                            let src = ((row * r.w + col) * 4) as usize;
                            rgba[idx..idx + 4].copy_from_slice(&cell[src..src + 4]);
                        }
                    }
                }
            }
        }
        let uv = Rect::new(
            x as f32 / atlas_width as f32,
            y as f32 / atlas_height as f32,
            (x + r.w) as f32 / atlas_width as f32,
            (y + r.h) as f32 / atlas_height as f32,
        );
        glyphs.insert(
            r.key,
            GlyphInfo {
                uv,
                px_w: r.w as f32,
                px_h: r.h as f32,
                bearing_x: r.bearing_x,
                bearing_top: r.bearing_top,
                advance: r.advance,
            },
        );
    }

    BuiltAtlas {
        rgba,
        width: atlas_width,
        height: atlas_height,
        glyphs,
        missing,
    }
}

/// ASCII printable (0x20..=0x7E) plus a representative handful of Latin-1 accented glyphs (the
/// German/French client's own charset overlap).
fn default_charset() -> Vec<char> {
    let mut cs: Vec<char> = (0x20u32..=0x7E).filter_map(char::from_u32).collect();
    cs.extend([
        'ä', 'ö', 'ü', 'Ä', 'Ö', 'Ü', 'ß', 'é', 'è', 'ê', 'à', 'â', 'ç', 'î', 'ï', 'ô', 'û', 'ù',
    ]);
    cs
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Integration — the resource, plugin, and Bevy Startup system
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The baked glyph atlas + live shaping state, built once at Startup from the client's own Friz
/// Quadrata TTF. See the module doc's v1 simplifications for why only one face/size is baked.
#[derive(Resource)]
pub(crate) struct UiFontAtlas {
    /// Live shaper state — [`layout_text_quads`] needs `&mut FontSystem` to shape each run (kerned
    /// advances), even though the bitmap glyphs themselves are pre-baked; see the module doc's
    /// metric-fidelity seam note.
    pub(super) font_system: FontSystem,
    /// `(font, glyph, size_bits) -> baked glyph metadata`, looked up by what `font_system` shapes.
    pub(super) glyphs: HashMap<GlyphKey, GlyphInfo>,
    /// The packed atlas bitmap, already uploaded.
    pub(super) image: Handle<Image>,
    /// Blizzard font path (lowercased, e.g. `"fonts\\arialn.ttf"`) → `fontdb` family name — how a
    /// `FontString`'s resolved `font` path selects its baked face. A path not here falls back to
    /// [`Self::default_family`].
    pub(super) path_to_family: HashMap<String, String>,
    /// Per-family baseline-ascender fraction `hhea.asc / (asc + |desc|)` (see [`hhea_ascent_ratio`])
    /// — the term in the client's placement law; [`crate::ui_text::layout`] seats each line's
    /// baseline at `round(size · ratio)`, the face pixel ascender `[CGxFont+0x17c]` that
    /// `glyph_vplace` 0x5d1360 hangs ink from.
    pub(super) family_ascent: HashMap<String, f32>,
    /// The fallback family (Friz Quadrata) for a `FontString` with no/unknown font path.
    pub(super) default_family: String,
    /// The default family's baked sizes (Friz — the superset: [`FONT_SIZES`] +
    /// [`FRIZ_SPLASH_SIZES`], ascending) — what [`Self::snap_size`] and any family miss snap to.
    pub(super) sizes: Vec<f32>,
    /// Per-family baked sizes (ascending) — [`Self::snap_for`]'s lookup, so a face only ever
    /// snaps to a size it actually baked (a non-Friz face never lands on the Friz-only 26/32).
    pub(super) sizes_by_family: HashMap<String, Vec<f32>>,
    /// The physical/logical scale the atlas was baked at (`Window::scale_factor` at build). Every
    /// baked [`GlyphInfo`] metric is physical px = logical × this; [`layout_text_quads`]/
    /// [`measure_text`] divide by it to work in logical units and snap positions to `1/scale`
    /// (the physical-pixel grid). Read once at build; a runtime monitor hop is **ignored** (logged),
    /// not rebaked — see [`load_fonts_and_build_atlas`].
    pub(super) scale: f32,
}

impl UiFontAtlas {
    /// The resolved baseline-ascender fraction (`hhea.asc / (asc + |desc|)`) for a font path
    /// (`None` = the default family, Friz ≈ 0.794) — the baseline's seat inside a one-pitch cell.
    /// The overhead-name mesh bake (`crate::nameplates`) uses it to hang each line from its
    /// byte-law baseline (the world-mode seat, wow-re `name-render-geometry-law.md`).
    pub(crate) fn ascent_ratio(&self, path: Option<&str>) -> f32 {
        let family = path
            .and_then(|p| self.path_to_family.get(&p.to_ascii_lowercase()))
            .unwrap_or(&self.default_family);
        self.family_ascent.get(family).copied().unwrap_or(0.794)
    }

    /// The baked size [`nearest_size`] snaps a requested height to — for a caller outside the
    /// layout path (world combat text) that needs to know the snap *before* handing the height to
    /// [`crate::ui_text::layout_text_quads`], so it can rescale the shaped quads to an unbaked
    /// target size around its own anchor. Snaps against the default family (Friz) — the superset
    /// list, and the face those callers draw.
    pub(crate) fn snap_size(&self, want: f32) -> f32 {
        nearest_size(&self.sizes, want)
    }

    /// The baked size `want` snaps to **for a specific family** — the layout path's snap, keyed
    /// by the face it resolved, so a request never snaps to a size only another face bakes.
    pub(super) fn snap_for(&self, family: &str, want: f32) -> f32 {
        let sizes = self.sizes_by_family.get(family).unwrap_or(&self.sizes);
        nearest_size(sizes, want)
    }

    /// The packed atlas texture — for the world-pass consumers (the nameplate meshes) that bind
    /// glyph cells onto 3-D geometry instead of UI quads.
    pub(crate) fn image(&self) -> Handle<Image> {
        self.image.clone()
    }
}

/// Loads the client's TTFs + bakes the glyph atlas at Startup, feeding [`crate::ui_script`]'s
/// `QuadContent::Text` extraction. See the module doc.
pub(crate) struct UiTextPlugin;

impl Plugin for UiTextPlugin {
    fn build(&self, app: &mut App) {
        // Runs each Update but builds the atlas exactly **once** — it needs both the patch chain
        // (opened at `AssetSet::Open`) *and* the primary window's real `scale_factor`, which winit
        // only reports once the OS window exists (a Startup read can still see the placeholder 1.0).
        // Gating on the window being present + guarding on `UiFontAtlas` already existing lets the
        // bake happen on the first frame the real scale is known, and never again.
        app.add_systems(Update, load_fonts_and_build_atlas);
    }
}

/// Read the client TTFs through the app's own patch chain ([`WorldAssets::chain`] — never
/// `std::fs`), register them into one `fontdb`, and bake **every** registered face at every
/// [`FONT_SIZES`] into a [`UiFontAtlas`] (so a font object naming ARIALN/MORPHEUS at 10..20px resolves
/// to real baked glyphs). Index 0 (Friz Quadrata) is the fallback face + required — if it's
/// unreadable (or no client data is open) text simply won't render, the same graceful-absence posture
/// [`crate::ui_script`]'s extraction takes (a missing [`UiFontAtlas`] means "skip `Text` regions").
fn load_fonts_and_build_atlas(
    mut commands: Commands,
    world_assets: Option<Res<WorldAssets>>,
    mut images: ResMut<Assets<Image>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    existing: Option<Res<UiFontAtlas>>,
    // The engine host (loaded at Startup, so present by this first-Update bake) — read-only, for
    // the outlined-cell census off its font-object registry. `None` (a UI-less run) just means no
    // outlined variants: the stamped-halo fallback covers.
    script: Option<bevy::prelude::NonSend<benilla_ui::script::UiScript>>,
    // The uiScale dial folded into the bake scale (decision 0584); optional so a UI-less app
    // (no `UiScriptPlugin`) still bakes at the 768 base.
    scale_cvar: Option<Res<crate::ui_script::UiScaleCvar>>,
) {
    // Build once (this runs every Update); a later monitor hop that changes the scale is ignored.
    if existing.is_some() {
        return;
    }
    // Wait for the window so the DPI-aware bake reads the *real* scale, not winit's placeholder 1.0.
    let Ok(window) = windows.single() else {
        return;
    };
    let scale = window.scale_factor();
    let Some(world_assets) = world_assets else {
        // No client data open — text won't render (the missing-atlas graceful path in `ui_script`).
        // Nothing to log-throttle here: this returns before inserting, so it retries next frame until
        // the chain opens; the chain-open failure is already logged by the asset foundation.
        return;
    };
    info!(
        "ui_text: baking glyph atlas at scale_factor {scale} (physical-resolution glyphs; a \
         runtime monitor hop to a different scale is ignored, not rebaked)"
    );

    let mut font_system = FontSystem::new_with_fonts(std::iter::empty());
    let mut swash_cache = SwashCache::new();
    // The faces we successfully register, in CLIENT_FONTS order (index 0 = Friz = fallback), each
    // with its baked-size list (Friz gets the splash extras), plus the Blizzard-path → family map
    // a resolved `font` path selects its baked face through.
    let mut faces: Vec<(fontdb::ID, String, Vec<f32>)> = Vec::new();
    let mut path_to_family: HashMap<String, String> = HashMap::new();
    let mut family_ascent: HashMap<String, f32> = HashMap::new();
    let mut friz_family: Option<String> = None;
    for (i, &path) in CLIENT_FONTS.iter().enumerate() {
        let bytes = {
            let chain = world_assets.chain.lock_recover();
            match chain.read(path) {
                Ok(b) => b,
                Err(e) => {
                    warn!("ui_text: failed to read {path} from the patch chain: {e:#}");
                    continue;
                }
            }
        };
        let ascent_ratio = hhea_ascent_ratio(&bytes);
        match register_font(&mut font_system, bytes) {
            Ok((id, family)) => {
                path_to_family.insert(path.to_ascii_lowercase(), family.clone());
                if let Some(r) = ascent_ratio {
                    family_ascent.insert(family.clone(), r);
                }
                let mut sizes = FONT_SIZES.to_vec();
                if i == 0 {
                    friz_family = Some(family.clone());
                    // Friz alone carries the zone-splash sizes (decision 0287); keep the list
                    // ascending so nearest-tie behavior stays order-stable.
                    sizes.extend_from_slice(FRIZ_SPLASH_SIZES);
                    sizes.sort_by(|a, b| a.partial_cmp(b).expect("finite font sizes"));
                }
                faces.push((id, family, sizes));
            }
            Err(e) => warn!("ui_text: failed to register {path}: {e:#}"),
        }
    }

    let Some(friz_family) = friz_family else {
        warn!(
            "ui_text: Friz Quadrata (FRIZQT__.TTF) unavailable — FontString text will not render"
        );
        return;
    };

    // The 768-virtual scale at bake time (decision 0582, × 0584's uiScale dial): UI text draws
    // at unit-height × s (`drawn_px`), so the census below bakes the SCALED sizes — crisp at
    // the startup window. A runtime window-height change re-derives s per frame at the seam but
    // keeps this bake (the snapped-nearest + exact-height rescale covers, slightly soft — the
    // same accepted policy as a monitor DPI hop). The dial resource is absent only in a UI-less
    // app, where the census it scales is empty anyway.
    let ui_scale = crate::ui_script::seam_scale(window.height(), scale_cvar.map_or(1.0, |c| c.0));

    // The registry-size census (decision 0581): every height the shipped `Fonts.xml` font-object
    // registry declares joins its face's baked set — the hardcoded FONT_SIZES list had drifted
    // from the registry (no 25/30), so NumberFontNormalHuge 30 snapped to 24 and the portrait
    // hit indicator rendered ~20% small. Baking what the registry declares (at the drawn ×s
    // size) makes the declared sizes exact; runtime SetTextHeight sizes BETWEEN baked ones
    // rescale in the layout (`layout_text_quads`' exact-height scale, same decision).
    if let Some(script) = script.as_deref() {
        for fo in script.font_objects() {
            let family = fo
                .font
                .as_deref()
                .and_then(|p| path_to_family.get(&p.to_ascii_lowercase()))
                .unwrap_or(&friz_family)
                .clone();
            let Some(h) = super::fontstring_em(fo.height).map(|u| u * ui_scale) else {
                continue;
            };
            if let Some((_, _, sizes)) = faces.iter_mut().find(|(_, f, _)| *f == family) {
                if !sizes.iter().any(|s| (s - h).abs() < 0.01) {
                    sizes.push(h);
                    sizes.sort_by(|a, b| a.partial_cmp(b).expect("finite font sizes"));
                }
            }
        }
    }

    // The outlined-cell census: every (face, size, ring radius) the shipped font-object registry
    // can actually request — read from the engine's `Fonts.xml` registry (loaded at Startup,
    // strictly before this first-Update bake), heights run through the same one-to-one cap +
    // per-face snap the layout path resolves with, so bake plan == lookup. Runtime `SetFont`
    // combos outside the registry miss their variant and fall back to the legacy stamped halos
    // (correct at full alpha; only a fade would show the stamp artifact — no shipped caller).
    let mut outlined: HashMap<(String, u32), Vec<u8>> = HashMap::new();
    if let Some(script) = script.as_deref() {
        for fo in script.font_objects() {
            let r = outline_radius(fo.outline);
            if r == 0 {
                continue;
            }
            let family = fo
                .font
                .as_deref()
                .and_then(|p| path_to_family.get(&p.to_ascii_lowercase()))
                .unwrap_or(&friz_family);
            let Some(sizes) = faces
                .iter()
                .find(|(_, f, _)| f == family)
                .map(|(_, _, s)| s)
            else {
                continue;
            };
            let capped =
                super::fontstring_em(fo.height).unwrap_or(super::DEFAULT_FONT_SIZE) * ui_scale;
            let snapped = nearest_size(sizes, capped);
            let radii = outlined
                .entry((family.clone(), snapped.to_bits()))
                .or_default();
            if !radii.contains(&r) {
                radii.push(r);
            }
        }
    } else {
        warn!(
            "ui_text: no UiScript at atlas bake — outlined cells unplanned, outlines will use \
             the stamped-halo fallback (fade shows the composite artifact)"
        );
    }

    let charset = default_charset();
    // The atlas width scales with the bake resolution so a retina bake (2× the texels per glyph)
    // doesn't overflow the shelves into an over-tall texture.
    let atlas_width = (1024.0 * scale).ceil() as u32;
    let built = build_atlas(
        &mut font_system,
        &mut swash_cache,
        &faces,
        &charset,
        atlas_width,
        scale,
        &outlined,
    );
    if !built.missing.is_empty() {
        warn!(
            "ui_text: {} char(s) missing from a baked face: {:?}",
            built.missing.len(),
            built.missing
        );
    }

    let image = images.add(Image::new(
        Extent3d {
            width: built.width,
            height: built.height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        built.rgba,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    ));

    let sizes_by_family: HashMap<String, Vec<f32>> = faces
        .iter()
        .map(|(_, family, sizes)| (family.clone(), sizes.clone()))
        .collect();
    let default_sizes = sizes_by_family
        .get(&friz_family)
        .cloned()
        .unwrap_or_else(|| FONT_SIZES.to_vec());
    commands.insert_resource(UiFontAtlas {
        font_system,
        glyphs: built.glyphs,
        image,
        path_to_family,
        family_ascent,
        default_family: friz_family,
        sizes: default_sizes,
        sizes_by_family,
        scale,
    });
}

/// Snap a requested font height to the nearest baked size in `sizes` (never empty — always at least
/// one baked face/size). Keeps the shaped size identical to a baked bitmap variant.
pub(super) fn nearest_size(sizes: &[f32], requested: f32) -> f32 {
    sizes
        .iter()
        .copied()
        .min_by(|a, b| {
            (a - requested)
                .abs()
                .partial_cmp(&(b - requested).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(DEFAULT_FONT_SIZE)
}

#[cfg(test)]
mod outlined_cell_tests {
    use super::*;

    /// Read texel (x, y) of an RGBA cell.
    fn px(rgba: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    #[test]
    fn normal_ring_takes_the_lut_grades() {
        // A 1×1 fully-inked glyph, NORMAL (1 pass), scale 1 → a 3×3 cell, all 9 texels marked.
        // Per the byte recipe the alpha is AA_NEIGHBOUR_LUT[in-bounds 3×3 marked count]:
        // centre 9 → 255, edge-mid 6 → 153, corner 4 → 85 — the graded ring, not hard black.
        let (rgba, w, h, pad) = outlined_cell(&[255], 1, 1, 1, 1.0);
        assert_eq!((w, h, pad), (3, 3, 1));
        assert_eq!(px(&rgba, w, 1, 1), [255, 255, 255, 255], "fill core");
        assert_eq!(px(&rgba, w, 1, 0), [0, 0, 0, 153], "edge-mid ring: count 6");
        assert_eq!(px(&rgba, w, 0, 1), [0, 0, 0, 153]);
        assert_eq!(px(&rgba, w, 0, 0), [0, 0, 0, 85], "corner ring: count 4");
        assert_eq!(px(&rgba, w, 2, 2), [0, 0, 0, 85]);
    }

    #[test]
    fn thick_outer_ring_is_the_soft_second_pass() {
        // THICK = a second dilation pass (the binary's mask-3→4 pass): 5×5, all marked; the
        // inner ring sits fully surrounded (count 9 → opaque) while the outer edge grades
        // 153/85 — the real client's softer THICK outer edge.
        let (rgba, w, h, pad) = outlined_cell(&[255], 1, 1, 2, 1.0);
        assert_eq!((w, h, pad), (5, 5, 2));
        assert_eq!(px(&rgba, w, 2, 2), [255, 255, 255, 255], "fill core");
        assert_eq!(
            px(&rgba, w, 1, 1),
            [0, 0, 0, 255],
            "inner ring: fully surrounded"
        );
        assert_eq!(
            px(&rgba, w, 2, 0),
            [0, 0, 0, 153],
            "outer edge-mid: count 6"
        );
        assert_eq!(px(&rgba, w, 0, 0), [0, 0, 0, 85], "outer corner: count 4");
    }

    #[test]
    fn retina_scales_the_pass_count_with_the_bake() {
        // scale 2, NORMAL: 2 passes ⇒ a 2-physical-px (1 logical) ring, dense — the iterative
        // dilation leaves no stride holes (the legacy stamp offsets did).
        let (rgba, w, h, pad) = outlined_cell(&[255], 1, 1, 1, 2.0);
        assert_eq!((w, h, pad), (5, 5, 2));
        assert_eq!(px(&rgba, w, 2, 2), [255, 255, 255, 255]);
        assert_ne!(px(&rgba, w, 1, 1)[3], 0, "no hole inside the 2-px ring");
        assert_eq!(px(&rgba, w, 0, 0), [0, 0, 0, 85]);
    }

    #[test]
    fn aa_fill_ramps_gray_toward_the_ring() {
        // Half coverage (128): the fill texel takes the coverage as GRAY at the LUT alpha —
        // the byte recipe's `RGB = coverage, A = LUT[count]` — never white-with-thin-alpha.
        let (rgba, w, _, _) = outlined_cell(&[128], 1, 1, 1, 1.0);
        assert_eq!(
            px(&rgba, w, 1, 1),
            [128, 128, 128, 255],
            "gray fill, LUT alpha"
        );
        assert_eq!(px(&rgba, w, 1, 0), [0, 0, 0, 153], "ring stays black");
    }

    #[test]
    fn pad_law_is_r_times_scale() {
        // Cell grows by r·round(scale) each side; the advance is the caller's (untouched here).
        let (_, w, h, pad) = outlined_cell(&[255, 255, 255, 255], 2, 2, 2, 1.0);
        assert_eq!((w, h, pad), (6, 6, 2));
    }
}
