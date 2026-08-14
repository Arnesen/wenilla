//! Text rendering for `QuadContent::Text` (decision 0068 §2): a glyph atlas baked from the client's
//! own TTFs, shaped through `cosmic-text` 0.16, emitted as [`crate::ui_pass::UiQuad`]s the quad pass
//! draws with no special case. Ported from `probes/text-glyph/src/{font,atlas,markup,layout}.rs`
//! (screenshot-verified there, 2026-07-02) into the real render path; the port's only structural change
//! is the font source (the app's own [`WorldAssets`] patch chain, not a probe-local `Chain::open`) and
//! the output type (this crate's real `UiQuad`, not the probe's mirror struct).
//!
//! ## Font objects (decision 0084) and remaining v1 simplifications
//!
//! - **Per-`FontString` size + face.** [`QuadContent::Text`] now carries the resolved `{ font, height }`
//!   from the engine's font registry (`Fonts.xml`'s named virtual `<Font>` objects). The atlas bakes
//!   every registered client TTF at each of [`FONT_SIZES`]; [`layout_text_quads`] selects the face by
//!   `font` path (via [`UiFontAtlas::path_to_family`]) and the nearest baked size. A `FontString` with
//!   no font object falls back to Friz Quadrata at [`DEFAULT_FONT_SIZE`] (the pre-0084 behavior).
//! - **DPI-aware bake.** The atlas is baked at `logical_size × Window::scale_factor` (physical
//!   resolution), and [`layout_text_quads`]/[`measure_text`] shape at that physical size, divide the
//!   metrics back to logical, and snap glyph origins to the physical-pixel grid — so a baked cell
//!   draws one texel per device pixel instead of a half-res bitmap upscaled by a retina framebuffer.
//! - **Outlines are rasterized** as the classic bitmap outline: [`layout_text_quads`] stamps each
//!   glyph's baked coverage in black over a filled `r×r` neighbourhood (`r=1` NORMAL, `r=2` THICK)
//!   behind the fill — `cosmic-text` has no native outline. The Number* fonts (action-bar
//!   hotkeys/counts) carry it.
//! - **`justifyH` and `justifyV` are honored** (see [`layout_text_quads`]): lines justify
//!   horizontally per line, and the line block vertically (default MIDDLE, the client's
//!   FontString default — what seats sized labels like the money numbers on their icons'
//!   centerline).
//! - **`|T...|t` inline textures are stripped**, matching the probe — a `FontString` never draws an
//!   inline icon in v1.
//!
//! ## The metric-fidelity seam
//!
//! [`layout_text_quads`] shapes every run through `cosmic-text`'s shaper for glyph SELECTION and
//! rasterization, but the pen now **advances by the client's own step law** (wow-re
//! `system/font`): per glyph, `floor(advance) + 1` px (`rasterize_glyph` 0x5d1120's
//! `out[5] = (FT_advance>>6)+1.0`), plus another `+1` for a **THICK**-outlined font only
//! (`GlyphStepBase` 0x5ca2b0 biases solely under the THICK flag — `outline-bake-tint.md`,
//! correcting this module's earlier any-outline reading) — the extra tracking that makes real
//! client text read wider/denser than raw font metrics. Remaining divergences, all confined here: kerning is dropped (the client applies only
//! negative pair kerns and rounds — ~0 at UI sizes), the law is applied at logical ppem (the
//! 768-window reference look), and glyph BITMAPS are cosmic/swash unhinted coverage where the
//! client runs 2004-era FreeType with hinting flags (0x208a/0x20c2) — slightly slimmer stems at
//! small sizes. A future bit-exact replacement (wow-re's font kernel) only changes this module;
//! nothing in its callers moves.

mod atlas;
mod layout;
mod markup;
mod measurer;

pub(crate) use atlas::{UiFontAtlas, UiTextPlugin};
pub(crate) use measurer::{measure_request, AtlasMeasurer};

/// The width law itself, for the differential test that pins it to the pre-table shaped sum
/// (`atlas::metrics_tests`). Not a production seam — every caller goes through [`measure_text`].
#[cfg(test)]
pub(crate) use layout::measure_line_width_for_test;
pub(crate) use layout::{
    ellipsize_to_fit, layout_text_quads, layout_text_quads_links, line_advances, line_origin,
    line_rows, measure_text, measure_wrapped_rows, FontSpec, Justify, UI_SEAT_NUDGE,
};

/// The default body text size (logical px) — WoW's own `GameFontNormal`. A `FontString` with no
/// resolved font object (no `inherits=`/`SetFont`) bakes and lays out at this, preserving the
/// pre-font-object behavior; a font object supplies a per-`FontString` height instead.
const DEFAULT_FONT_SIZE: f32 = 12.0;

/// The FontString **one-to-one raster cap** (wow-re `system/ui/scratch/fontstring-overflow.md`,
/// §5 pair + byte arbitration, VERIFIED): every `CSimpleFontString` ships with the one-to-one bit
/// set (`+0x120` bit `0x200`, ctor `0x770d30` default `0x212`; the sole clearer is `SetTextHeight`
/// `0x771600`, which nothing we ship calls), so the size every build/measure actually uses is the
/// getter `0x7727b0(1)`'s return — the font's RASTERIZED pixel size drawn 1:1, not the requested
/// FontHeight — and the rasterizer clamps that pixel size to `min(32, max(2,
/// round((height/768)·deviceH)))` (`0x5ca030` → `[CGxFont+0x24c]`). Net law: any FontHeight ≥ 32
/// draws 32 px — the "102" zone splash has always rendered 32 px tall, which is also why it fits
/// its 512-wide box on ONE line ("Stormwind City" ≈ 253 px at em 32; even "The Temple of
/// Atal'Hakkar" ≈ 442 < 512). There is **no scale-to-fit anywhere** in the client's text pipeline.
///
/// Deliberate divergence (the fold-back record): the binary caps in DEVICE pixels — a 2004
/// raster-memory ceiling that, applied literally on a modern display, converges ALL text toward
/// the same 32 device px (the splash no taller than chat). We apply the law in LOGICAL units —
/// `min(32, height)` — byte-identical to the client at its 768-tall design resolution, and scaled
/// by the DPI-aware bake like every other size. **UI FontStrings only**: world text
/// ([`crate::combat_text`], the nameplates) sizes by its own byte-derived viewport laws and
/// deliberately renders crisp above the cap, where the real client stretched a ≤32 px raster
/// (the era's blurry big crits — a recorded upgrade, not an oversight).
pub(crate) const FONTSTRING_EM_CAP: f32 = 32.0;

/// Apply the one-to-one raster cap ([`FONTSTRING_EM_CAP`]) to a UI `FontString`'s requested
/// height — the app-side seam ([`crate::ui_script`]'s extraction + measure round-trips) caps every
/// height it forwards, so render and measure agree with the client's drawn-size echo
/// (`GetStringWidth` reports the natural width *at the capped size*, `0x772890`). `None` (no font
/// object) stays `None`: the [`DEFAULT_FONT_SIZE`] fallback is already under the cap.
pub(crate) fn fontstring_em(height: Option<f32>) -> Option<f32> {
    height.map(|h| h.min(FONTSTRING_EM_CAP))
}

/// The DRAWN pixel height of a UI FontString under the client's two size regimes (§5-verified,
/// wow-re `fontstring-overflow.md` + `font-size-to-freetype-em.md`), times the seam scale
/// `s = windowH/768 × uiScale` (decisions 0582 + 0584, `crate::ui_script::seam_scale`; `s = 1`
/// at the design height with the dial at 1, where this is byte-identical to the pre-scaling law):
///
/// - **one-to-one** (the ctor default, everything but SetTextHeight): the raster size — the
///   [`FONTSTRING_EM_CAP`] applied in UNITS (the recorded divergence above: the client caps in
///   device px, converging all big text to 32 px on tall windows; we keep 32 *units* so the
///   capped splash scales with the screen and stays crisp), then × `s`.
/// - **SetTextHeight** (`text_height`): the literal size × `s`, UNCAPPED — the client magnifies
///   the raster to it (`0x771600` clears bit `0x200`; the drawn em is `round(size/768·deviceH)`).
///
/// `None`/`None` scales the renderer default so fallback text obeys the same space.
pub(crate) fn drawn_px(font_height: Option<f32>, text_height: Option<f32>, s: f32) -> Option<f32> {
    match text_height {
        Some(t) => Some(t * s),
        None => Some(fontstring_em(font_height).unwrap_or(DEFAULT_FONT_SIZE) * s),
    }
}

#[cfg(test)]
mod cap_tests {
    use super::*;

    #[test]
    fn one_to_one_cap_matches_the_byte_law() {
        // ZoneTextFont/WorldMapTextFont's nominal 102 → the 32 px raster cap (0x5ca030).
        assert_eq!(fontstring_em(Some(102.0)), Some(32.0));
        // The world-map label's SetFont 33 (decision 0235's taste dial) — the law caps it too.
        assert_eq!(fontstring_em(Some(33.0)), Some(32.0));
        // At-cap and under-cap sizes pass through untouched (SubZoneTextFont 26, chat 14).
        assert_eq!(fontstring_em(Some(32.0)), Some(32.0));
        assert_eq!(fontstring_em(Some(26.0)), Some(26.0));
        assert_eq!(fontstring_em(Some(14.0)), Some(14.0));
        // No font object: the default-size fallback stays the caller's concern.
        assert_eq!(fontstring_em(None), None);
    }
}
// NOTE on line pitch: there is deliberately NO line-height factor. The client's line-step law
// (`LayoutLines` 0x5cdc20, wow-re `system/font`) is `lineStep = px(size) + spacing`, where
// `spacing` is the FontString's own extra line gap (XML `spacing`, default **0** — none of our
// shipped UI sets one), so the pitch IS the font height. The earlier `1.25` factor here was
// cosmic-text's convention, not the client's, and read as "line height too large" against the
// reference. Cosmic buffers get `size` as their `Metrics` line height too — every run shapes
// single-line (`Wrap::None`), so that value never affects layout.
