use std::collections::HashMap;

use bevy::math::Rect;
use bevy::prelude::*;
use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Wrap};

use benilla_ui::script::{JustifyH, JustifyV, Outline};

use crate::ui_pass::{UiQuad, UvRect};

use super::atlas::{GlyphInfo, GlyphKey, UiFontAtlas};
use super::markup::{parse_markup, ColorRun};
use super::DEFAULT_FONT_SIZE;

mod overflow;
mod wrap;
use wrap::{greedy_pack, tokenize_words};

/// The smallest resolved rect width (px) treated as a **pinned wrap constraint**. A `FontString`
/// with no explicit `<Size>`/`setAllPoints`/opposing anchor resolves through [`crate::ui_text`]'s
/// caller to a near-zero-width rect (the single-point case — the engine's layout derives
/// `left == right` from the pinned center, see `benilla-ui`'s `layout` resolver); such a rect is
/// "no width constraint" and text lays out on one line from its justify point, never wrapped. Only a
/// rect wider than this engages word wrap. No shipped wrappable FontString is this narrow (the
/// narrowest, a merchant row's price field, is tens of px); the degenerate titles are ~0.
const WRAP_MIN_WIDTH: f32 = 1.0;

/// The smallest resolved rect height (px) treated as a **pinned height limit** — the vertical twin
/// of [`WRAP_MIN_WIDTH`]: a degenerate (single-point / pre-measure) rect resolves to ~zero height,
/// which is "no height constraint", never "a zero-line box". Only a taller rect engages the
/// line-stack stop ([`overflow::lines_allowed`]) and the ellipsis gate ([`ellipsize_to_fit`]).
const HEIGHT_LIMIT_MIN: f32 = 1.0;

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Layout — ported from probes/text-glyph/src/layout.rs, retargeted to the real `UiQuad`
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// A `FontString`'s two justification axes (`justifyH`/`justifyV`), bundled — they arrive
/// together from the region's paint and travel together into [`layout_text_quads`].
#[derive(Clone, Copy)]
pub(crate) struct Justify {
    pub h: JustifyH,
    pub v: JustifyV,
}

/// The resolved face + size a `FontString` draws with (from its font object / `SetFont`) — bundled
/// so [`layout_text_quads`] stays within one argument budget. `None`s fall back to Friz Quadrata at
/// [`DEFAULT_FONT_SIZE`].
#[derive(Clone, Copy)]
pub(crate) struct FontSpec<'a> {
    pub path: Option<&'a str>,
    pub height: Option<f32>,
    /// The font's TRUE glyph outline ([`Outline::None`] for the common case). It selects the
    /// baked composite-cell variant (ring+fill in one texture — the fallback is the legacy halo
    /// stamps) AND, for THICK only, the step law's extra `+1` (`GlyphStepBase` 0x5ca2b0 biases
    /// solely under the THICK flag — wow-re `outline-bake-tint.md`), so [`measure_text`] must see
    /// it too: a THICK outline changes measured/wrapped width.
    pub outline: Outline,
    /// Paint the outline halo stamps. `false` only for the drop-shadow pass, which must lay out
    /// IDENTICALLY to its (possibly outlined) fill but never paints halos — an outlined shadow
    /// would be a muddy black blob.
    pub paint_halo: bool,
    /// The write-on reveal (`SetAlphaGradient` — per-FontString paint state, like `outline`):
    /// [`layout_text_quads`] multiplies each glyph's alpha by the ramp at its character position.
    /// Riding the spec means the drop-shadow pass (`..spec`) inherits it for free — a revealing
    /// character's shadow fades with its fill. `None` = draw whole; [`measure_text`] ignores it
    /// (alpha never changes metrics).
    pub alpha_gradient: Option<(f32, f32)>,
}

/// The client's per-glyph **step law** (verified in wow-re, `system/font`): the rasterizer stores
/// each glyph's step base as `(FT_advance >> 6) + 1.0` — the FreeType advance floored to integer
/// pixels **plus one** (`rasterize_glyph` 0x5d1120, `out[5]`); an outlined font biases it another
/// `+1.0` (`GlyphStepBase` 0x5ca2b0); `ComputeStep` 0x5ca2d0 consumes exactly that (plus
/// only-negative kerning scaled by the face's advance-adjust, then rounds). This extra tracking is
/// look-defining — it is why real client text reads wider/denser than the raw font metrics.
///
/// **Applied in PHYSICAL device pixels** (the ppem the face is actually rasterized at), then divided
/// back to logical — because that is where the real client's law lives: `FT_advance >> 6` floors the
/// device-pixel advance and the `+1`/`+1` biases are whole *device* pixels. Flooring in logical px
/// instead (the pre-fix shortcut) over-tracks on any window where `scale ≠ 1`: at retina `scale = 2`
/// a 6.385px-logical (12.77px-physical) ARIALN digit steps `(⌊12.77⌋+2)/2 = 7.0px`, not the logical
/// `⌊6.385⌋+2 = 8.0px` — a full logical pixel of extra gap *per digit*, proportionally huge on small
/// outlined numbers (bag counts, money) and the cause of their "digits too wide apart" look. The two
/// agree only at `scale = 1` (the 768-tall reference), so the divergence hid until a retina look-pass.
///
/// Deliberate v1 simplification (module doc): kerning is dropped entirely (the client applies only
/// *negative* pair kerns and rounds the sum — at UI sizes the rounded contribution is almost always 0).
fn client_step(raw_physical_advance: f32, step_extra: f32, scale: f32) -> f32 {
    (raw_physical_advance.floor() + step_extra) / scale
}

/// The step-law bias for a font's outline flag: the base `+1` for everyone, and one more `+1`
/// for **THICK only** — `GlyphStepBase 0x5ca2b0` adds its extra pixel solely under the THICK
/// font flag (wow-re `outline-bake-tint.md`, §5 + difftests, commit `f80ce699`, CORRECTING the
/// earlier "any outline" reading this module shipped with: a NORMAL-outlined font steps exactly
/// like a plain one, its 1px ring riding the base `+1` tracking; mirroring the cell pad onto the
/// advance was the flagged trap). NumberFontNormal digits (money, counts, hotkeys) are the
/// visible beneficiaries: one less pixel of gap per glyph.
fn step_extra_of(outline: Outline) -> f32 {
    match outline {
        Outline::Thick => 2.0,
        _ => 1.0,
    }
}

/// Shape `text` on one line and return its total laid-out width (logical px) at `font_size` under
/// the client's step law ([`client_step`]): the sum of per-glyph steps — the baked atlas advance
/// (or cosmic-text's own `glyph.w` for a char with no baked glyph), floored, `+ step_extra`. This
/// is the client's `GetTextWidth` (a sum of `ComputeStep`s), so measure == render == the real
/// client's width. Empty text is zero-width. Shares [`UiFontAtlas`]'s two disjoint fields by
/// direct field access, so the caller keeps its `&mut atlas` intact.
fn measure_line_width(
    font_system: &mut FontSystem,
    glyphs: &HashMap<GlyphKey, GlyphInfo>,
    attrs: &Attrs,
    font_size: f32,
    scale: f32,
    step_extra: f32,
    text: &str,
) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    // Shape at the PHYSICAL size (the resolution the atlas baked at), then divide the physical
    // advances back to logical — the same scaled-face-divided-back path the emit pass uses.
    let phys = font_size * scale;
    let mut buffer = Buffer::new(font_system, Metrics::new(phys, phys));
    buffer.set_wrap(font_system, Wrap::None);
    buffer.set_text(font_system, text, attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(font_system, false);
    let mut w = 0.0f32;
    for run in buffer.layout_runs() {
        for g in run.glyphs {
            // Measurement always reads the plain (r=0) cell: advances are variant-invariant,
            // and every face/size bakes a plain cell (outlined variants are a planned subset).
            let key: GlyphKey = (g.font_id, g.glyph_id, font_size.to_bits(), 0);
            let adv = glyphs.get(&key).map(|i| i.advance).unwrap_or(g.w);
            w += client_step(adv, step_extra, scale);
        }
    }
    w
}

/// The EditBox advance-table answer ([`benilla_ui::script::UiScript::set_editbox_advances`]'s
/// payload): per-BYTE cumulative laid-out widths of `text` under the exact shaping + step law
/// [`measure_line_width`] uses — len+1 entries, `[0] = 0`, each char's full step landing on its
/// END boundary and its interior (continuation) bytes holding the lead's value, so a mid-char
/// index degrades to the char's start and char-boundary lookups are exact. Single-line is the
/// EditBox law; a `\n`-bearing (multiLine) string accumulates per line via the run's line index
/// (x resets never — the table stays monotonic, which is all the hit-test needs).
///
/// **Indexed by the box's RAW byte, measured over what is DRAWN** (decision 1075). The box stores
/// the escaped string and draws only the visible one, so every `|c…`/`|r`/`|H…|h`/`|T…|t` byte
/// costs zero width: [`crate::ui_text::markup::visible_map`] carries each drawn byte back to its
/// raw offset, and the forward-fill below hands every escape byte the previous boundary's width.
/// Measuring the raw string instead is what parked the caret half a chat bar right of a
/// shift-clicked item link.
///
/// **Stated approximation:** the drawn string is shaped as ONE buffer, while the draw shapes each
/// color run separately — so kerning across a color boundary can differ by a fraction of a pixel.
/// (This predates 1075: the raw string was shaped as one buffer too.)
pub(crate) fn line_advances(atlas: &mut UiFontAtlas, text: &str, font: FontSpec) -> Vec<f32> {
    let mut cum = vec![0.0f32; text.len() + 1];
    if text.is_empty() {
        return cum;
    }
    let (drawn, bounds) = crate::ui_text::markup::visible_map(text);
    if drawn.is_empty() {
        return cum; // pure markup draws nothing — every boundary sits at x = 0
    }
    let family = font
        .path
        .and_then(|p| atlas.path_to_family.get(&p.to_ascii_lowercase()))
        .unwrap_or(&atlas.default_family)
        .clone();
    let attrs = Attrs::new().family(Family::Name(&family));
    let font_size = atlas.snap_for(&family, font.height.unwrap_or(DEFAULT_FONT_SIZE));
    let scale = atlas.scale;
    let step_extra = step_extra_of(font.outline);
    // Byte offset of each buffer line ('\n'-split) within the DRAWN string — glyph cluster ranges
    // are line-relative, and the map below is indexed in drawn bytes.
    let mut line_starts = vec![0usize];
    for (i, b) in drawn.bytes().enumerate() {
        if b == b'\n' {
            line_starts.push(i + 1);
        }
    }
    let phys = font_size * scale;
    let mut written = vec![false; text.len() + 1];
    written[0] = true;
    {
        let font_system = &mut atlas.font_system;
        let mut buffer = Buffer::new(font_system, Metrics::new(phys, phys));
        buffer.set_wrap(font_system, Wrap::None);
        buffer.set_text(font_system, &drawn, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(font_system, false);
        let mut x = 0.0f32;
        for run in buffer.layout_runs() {
            let base = line_starts.get(run.line_i).copied().unwrap_or(0);
            for g in run.glyphs {
                let key: GlyphKey = (g.font_id, g.glyph_id, font_size.to_bits(), 0);
                let adv = atlas.glyphs.get(&key).map(|i| i.advance).unwrap_or(g.w);
                x += client_step(adv, step_extra, scale);
                // The glyph's end boundary in DRAWN bytes → the RAW boundary it sits on.
                let end = bounds[(base + g.end).min(drawn.len())];
                cum[end] = x;
                written[end] = true;
            }
        }
    }
    // Forward-fill the unwritten slots (cluster interiors, separators the shaper consumed):
    // each carries the previous boundary's value.
    for i in 1..cum.len() {
        if !written[i] {
            cum[i] = cum[i - 1];
        }
    }
    // Into DRAWN space ([`drawn_k`]): the caret/click/selection geometry these feed must land on
    // the glyphs the 0581 rescale actually put on screen, not on the pre-rescale shaping.
    let k = drawn_k(font.height, font_size);
    if k != 1.0 {
        for v in &mut cum {
            *v *= k;
        }
    }
    cum
}

/// The exact-height rescale factor the DRAW applies (decision 0581, [`layout_text_quads_inner`]'s
/// tail): a requested height between baked sizes shapes at the snapped `font_size` and scales the
/// finished quads by `req / font_size` about the justify anchor. Every measure that hands geometry
/// back onto the drawn glyphs — the editbox advance table, the caret cell, the row pitch — must
/// return values in that DRAWN space, so they apply the same factor. Identity for a baked size and
/// for `height: None` (the draw's rescale gate is `font.height`-Some too). This stopped being a
/// theoretical case when the era-shaped windows started riding `SetScale` (0950): their frame
/// scale multiplies into every drawn height, landing between bakes — the options search box's
/// caret sat 28% past its own text (director, 2026-08-05; decision 0989).
fn drawn_k(font_height: Option<f32>, font_size: f32) -> f32 {
    match font_height {
        Some(req) if ((req / font_size) - 1.0).abs() > 1e-3 => req / font_size,
        _ => 1.0,
    }
}

/// The multiline-EditBox row answer (the 2-D half of
/// [`benilla_ui::script::UiScript::set_editbox_advances`]'s payload): the byte offset where each
/// wrapped display row of `text` begins at `wrap_width` — the exact [`wrap_line`] pass the render
/// wraps with, so the engine's `(row, x)` caret/click law lands on the drawn rows — plus the row
/// pitch (the snapped font em, the same `N·S` block law [`measure_text`] heights with). Row
/// starts are reconstructed by walking the source string past each wrapped row's verbatim text
/// and the separator the break swallowed (wrap keeps inter-word whitespace verbatim and drops
/// only the trailing separator, so the walk is exact) — in DRAWN bytes, mapped back to RAW ones,
/// since the rows index the same raw buffer [`line_advances`] does (decision 1075). Never empty
/// (`[0]` for empty text).
pub(crate) fn line_rows(
    atlas: &mut UiFontAtlas,
    text: &str,
    wrap_width: f32,
    font: FontSpec,
) -> (Vec<usize>, f32) {
    let family = font
        .path
        .and_then(|p| atlas.path_to_family.get(&p.to_ascii_lowercase()))
        .unwrap_or(&atlas.default_family)
        .clone();
    let attrs = Attrs::new().family(Family::Name(&family));
    let font_size = atlas.snap_for(&family, font.height.unwrap_or(DEFAULT_FONT_SIZE));
    let scale = atlas.scale;
    let step_extra = step_extra_of(font.outline);
    let mut rows = Vec::new();
    let mut base = 0usize; // byte offset of the current '\n' segment within `text`
    for seg in text.split('\n') {
        let lines = parse_markup(seg, [1.0, 1.0, 1.0, 1.0]);
        let sub: Vec<Vec<ColorRun>> = if wrap_width > WRAP_MIN_WIDTH {
            lines
                .iter()
                .flat_map(|line| {
                    wrap_line(
                        &mut atlas.font_system,
                        &atlas.glyphs,
                        &attrs,
                        font_size,
                        scale,
                        step_extra,
                        line,
                        wrap_width,
                    )
                })
                .collect()
        } else {
            lines
        };
        segment_row_starts(seg, &sub, base, &mut rows);
        base += seg.len() + 1; // + the '\n' itself (swallowed by the split)
    }
    if rows.is_empty() {
        rows.push(0);
    }
    // Row PITCH in drawn space ([`drawn_k`] — vertical distances rescale with the quads). The row
    // STARTS stay: both this wrap and the draw's break at unscaled steps against the same width,
    // so the byte boundaries already agree.
    (rows, font_size * drawn_k(font.height, font_size))
}

/// [`line_rows`]'s reconstruction walk for one `\n` segment: push the byte start of each wrapped
/// sub-line of `seg` (offset by `base`, `seg`'s position in the full string) into `rows`. Wrap
/// keeps inter-word whitespace verbatim inside a row and drops only the separator a break
/// swallows, so each subsequent row starts past the previous row's bytes plus any whitespace
/// (a force-broken word swallowed none — the skip loop stops at its first byte). A blank
/// segment still occupies one row.
fn segment_row_starts(seg: &str, sub: &[Vec<ColorRun>], base: usize, rows: &mut Vec<usize>) {
    if sub.is_empty() {
        rows.push(base);
        return;
    }
    // A wrapped row is made of DRAWN text (the markup is gone by then) while a row start must be a
    // RAW byte — the offset space the box's cursor and [`line_advances`] both live in. So the walk
    // runs in drawn bytes and maps each start back (decision 1075).
    let (drawn, bounds) = crate::ui_text::markup::visible_map(seg);
    let mut p = 0usize; // byte cursor within `drawn`
    for (j, line) in sub.iter().enumerate() {
        if j > 0 {
            while let Some(c) = drawn[p.min(drawn.len())..].chars().next() {
                if c.is_whitespace() {
                    p += c.len_utf8();
                } else {
                    break;
                }
            }
        }
        rows.push(base + bounds[p.min(drawn.len())]);
        p += line.iter().map(|r| r.text.len()).sum::<usize>();
    }
}

/// Greedy word-boundary wrap of one markup line (a `\n`-delimited run sequence) into sub-lines that
/// each fit within `max_width` px.
///
/// The break law is the byte-verified regime-2 wrap (`system/ui/scratch/fontstring-overflow.md`,
/// the `0x5c6c50`/`0x5c7780` kernel): break at the last break opportunity; a word with none that
/// overflows alone force-breaks at the last fitting glyph — a rendered line never exceeds the wrap
/// width. Inter-word whitespace is preserved verbatim (a double space after a period stays a
/// double space — see [`tokenize_words`]), and only the trailing separator at a line break drops.
/// Colors are preserved per word (the separating whitespace attaches to the preceding run, since it
/// draws no ink). **Remaining approximation:** break opportunities are ASCII/Unicode whitespace
/// only — the kernel's kinsoku (CJK) opportunity classes and the `nonspacewrap` flag (ui `0x1000`:
/// mid-word breaks become opportunities even when a space exists) are unmodeled; no shipped
/// FontString renders CJK, and the one `nonspacewrap` consumer (MinimapZoneText) sits in a
/// one-line box where opportunity choice can't change the outcome.
#[allow(clippy::too_many_arguments)] // the shared shaping context + the step-law bias, one each
fn wrap_line(
    font_system: &mut FontSystem,
    glyphs: &HashMap<GlyphKey, GlyphInfo>,
    attrs: &Attrs,
    font_size: f32,
    scale: f32,
    step_extra: f32,
    line: &[ColorRun],
    max_width: f32,
) -> Vec<Vec<ColorRun>> {
    // Flatten the line's color runs into words, each tagged with its run color and its verbatim
    // inter-word separator (so Blizzard's double spaces survive the wrap).
    let words = tokenize_words(line);
    if words.is_empty() {
        // A blank line (only whitespace / empty) still occupies a row — keep it as one empty line.
        return vec![line.to_vec()];
    }

    greedy_pack(words, max_width, |t| {
        measure_line_width(font_system, glyphs, attrs, font_size, scale, step_extra, t)
    })
}

/// Lays out `text` top-left within `rect` (screen px, **y-down** — the same space
/// [`crate::ui_script::extract::drive_script`] already flips frame rects into) and returns one [`UiQuad`] per
/// non-blank glyph, textured with [`UiFontAtlas`]'s baked atlas. `z_key` is shared by every glyph — the
/// owning `FontString` region's own [`crate::ui_pass`]-order key (regions already sort after their
/// frame and by draw layer/decl, so reusing it keeps text in the client's total order with no extra
/// bookkeeping; ties break by push order, i.e. left-to-right/top-to-bottom, which is what a stable sort
/// preserves). `base_color` is the region's own color (`SetVertexColor`, default opaque white);
/// `|cAARRGGBB`/`|r` runs in `text` override it per the markup rules above.
///
/// Chars outside [`crate::ui_text`]'s default charset (no baked glyph) are silently skipped — no
/// fallback glyph in v1.
///
/// `justify_h` positions each line's runs horizontally *within* `rect`: `Left` flushes to
/// `rect.min.x` (the v1 default before justify existed), `Center` centers the line's measured width,
/// `Right` flushes to `rect.max.x`. `justify_v` positions the line block vertically the same way
/// (default `Middle`, the client's FontString default; degenerate zero-height rects keep top).
/// Each line is measured once (a first pass laying glyphs at a zero line-origin), then shifted by the
/// justification offset — so kerned advances and the shift stay consistent.
/// Measure `text`'s laid-out size — width of the widest line, height of all (wrapped) lines — at
/// the resolved face/size, wrapping at `wrap_width` when given (the same [`wrap_line`] pass
/// rendering uses, so measure == render). The engine's measure round-trip
/// ([`benilla_ui::script::UiScript::fontstrings_needing_measure`]) sizes height-less FontStrings
/// with this — the real client's layout asks its font engine for string metrics the same way.
pub(crate) fn measure_text(
    atlas: &mut UiFontAtlas,
    text: &str,
    wrap_width: Option<f32>,
    font: FontSpec,
) -> (f32, f32) {
    let lines = parse_markup(text, [1.0, 1.0, 1.0, 1.0]);
    let family = font
        .path
        .and_then(|p| atlas.path_to_family.get(&p.to_ascii_lowercase()))
        .unwrap_or(&atlas.default_family)
        .clone();
    let attrs = Attrs::new().family(Family::Name(&family));
    let font_size = atlas.snap_for(&family, font.height.unwrap_or(DEFAULT_FONT_SIZE));
    let scale = atlas.scale;
    // The step-law bias ([`client_step`]): measure MUST see the same outline-biased steps the
    // render pass lays, or wrap points and measured widths drift from what's drawn.
    let step_extra = step_extra_of(font.outline);
    let render_lines: Vec<Vec<ColorRun>> = match wrap_width {
        Some(w) if w > WRAP_MIN_WIDTH => lines
            .iter()
            .flat_map(|line| {
                wrap_line(
                    &mut atlas.font_system,
                    &atlas.glyphs,
                    &attrs,
                    font_size,
                    scale,
                    step_extra,
                    line,
                    w,
                )
            })
            .collect(),
        _ => lines,
    };
    let mut max_w = 0.0f32;
    for line in &render_lines {
        let mut w = 0.0f32;
        for run in line {
            if !run.text.is_empty() {
                w += measure_line_width(
                    &mut atlas.font_system,
                    &atlas.glyphs,
                    &attrs,
                    font_size,
                    scale,
                    step_extra,
                    &run.text,
                );
            }
        }
        max_w = max_w.max(w);
    }
    // A hair of headroom: the render pass re-wraps inside this same width, and a line that fits
    // EXACTLY can lose the comparison to float noise (the title "Marshal McBride" wrapped inside
    // its own measured width). One pixel is invisible and keeps measure ⊇ render.
    // Height: N lines at the client's intrinsic pitch — the font em, NO outline pad (the byte
    // law: `font_textblock_height 0x5c2070` = N·S + (N−1)·gap, spacing 0 for all shipped UI;
    // the +2r lives only in the atlas CELL, never the block math —
    // `fontstring-vertical-placement.md`). An outlined line's ring pokes past this height by
    // design, exactly as it does in the client.
    (max_w.ceil() + 1.0, render_lines.len() as f32 * font_size)
}

/// How many display rows `text` wraps into at `wrap_width` — the message-line half of the measure
/// round-trip ([`benilla_ui::script::UiScript::message_lines_needing_measure`]): the engine's
/// ScrollingMessageFrame allocates `rows × font-height` per ring line from this. Runs the exact
/// [`wrap_line`] pass rendering uses (same step law, same outline bias), so the band height always
/// equals the drawn block height. Never 0 (empty text is one blank row).
pub(crate) fn measure_wrapped_rows(
    atlas: &mut UiFontAtlas,
    text: &str,
    wrap_width: f32,
    font: FontSpec,
) -> u16 {
    let family = font
        .path
        .and_then(|p| atlas.path_to_family.get(&p.to_ascii_lowercase()))
        .unwrap_or(&atlas.default_family)
        .clone();
    let attrs = Attrs::new().family(Family::Name(&family));
    // Per-family size snap (the rebase-landed law): rows must count at the exact size the render
    // pass bakes this face at, or band heights drift from the drawn block.
    let font_size = atlas.snap_for(&family, font.height.unwrap_or(DEFAULT_FONT_SIZE));
    let scale = atlas.scale;
    let step_extra = step_extra_of(font.outline);
    let rows = wrapped_rows(
        &mut atlas.font_system,
        &atlas.glyphs,
        &attrs,
        font_size,
        scale,
        step_extra,
        text,
        wrap_width,
    );
    rows.clamp(1, usize::from(u16::MAX)) as u16
}

/// The wrapped display-row count of `text` at `wrap_width` — the exact [`wrap_line`] pass the
/// render uses (same step law, same outline bias), shared by [`measure_wrapped_rows`] (the chat
/// band half) and [`ellipsize_to_fit`]'s fit test. An unconstrained width counts the `\n` lines.
#[allow(clippy::too_many_arguments)] // the shared shaping context, same shape as wrap_line's
fn wrapped_rows(
    font_system: &mut FontSystem,
    glyphs: &HashMap<GlyphKey, GlyphInfo>,
    attrs: &Attrs,
    font_size: f32,
    scale: f32,
    step_extra: f32,
    text: &str,
    wrap_width: f32,
) -> usize {
    let lines = parse_markup(text, [1.0, 1.0, 1.0, 1.0]);
    if wrap_width > WRAP_MIN_WIDTH {
        lines
            .iter()
            .map(|line| {
                wrap_line(
                    font_system,
                    glyphs,
                    attrs,
                    font_size,
                    scale,
                    step_extra,
                    line,
                    wrap_width,
                )
                .len()
            })
            .sum()
    } else {
        lines.len()
    }
}

/// The FontString display string under the height-gated ellipsis-truncate — `CSimpleFontString
/// 0x771ec0`, regime 3 of the overflow verdict (`fontstring-overflow.md`, decision 0292's named
/// residue): when the wrapped text needs more lines than the box allows, the tail is replaced by
/// `"..."`, backed off one char at a time until the candidate fits. `None` = draw the raw text
/// (fits, or the gate fails). The gate is geometric — `boxW > 0 && boxH > 0` on the resolved rect
/// (`maxLines` unmodeled; nothing shipped sets it): an auto-height FontString's rect height IS its
/// wrapped block (the measure round-trip), so it always fits and never truncates — the byte law's
/// intrinsic-height escape. Applies at the region-FontString paint seam only
/// ([`crate::ui_script::extract`]): the client's ellipsis is a FontString mechanism — editbox,
/// message-frame lines, and world text (`combat_text`/nameplates) never take it, exactly as their
/// C++ counterparts never call `0x771ec0`.
pub(crate) fn ellipsize_to_fit(
    atlas: &mut UiFontAtlas,
    text: &str,
    rect: Rect,
    font: FontSpec,
) -> Option<String> {
    if rect.width() <= WRAP_MIN_WIDTH || rect.height() <= HEIGHT_LIMIT_MIN {
        return None;
    }
    let family = font
        .path
        .and_then(|p| atlas.path_to_family.get(&p.to_ascii_lowercase()))
        .unwrap_or(&atlas.default_family)
        .clone();
    let attrs = Attrs::new().family(Family::Name(&family));
    let font_size = atlas.snap_for(&family, font.height.unwrap_or(DEFAULT_FONT_SIZE));
    let scale = atlas.scale;
    let step_extra = step_extra_of(font.outline);
    // The line pitch — identical to the emit pass's ([`layout_text_quads`]: the font em, NO
    // outline pad — the byte law, `fontstring-vertical-placement.md`), so the fit test and the
    // drawn stack agree line for line.
    let font_system = &mut atlas.font_system;
    let glyphs = &atlas.glyphs;
    // The line-count law lives in `overflow` (it is the FIT law here, not the render stack's) —
    // see `overflow::ellipsize_in_box`.
    overflow::ellipsize_in_box(text, rect.height(), font_size, |candidate| {
        wrapped_rows(
            font_system,
            glyphs,
            &attrs,
            font_size,
            scale,
            step_extra,
            candidate,
            rect.width(),
        )
    })
}

/// Per-digit (`'0'..='9'`) step widths (logical px) at the resolved face/size — the synchronous
/// number-metrics feed behind the script side's money layout ([`benilla_ui::script::UiScript::
/// set_digit_advances`]). The real `SmallMoneyFrame` sizes each denomination button with
/// `GetTextWidth` *mid-update* (ref MoneyFrame.lua l.202); our `GetStringWidth` measure round-trip
/// is a frame late, so the app feeds these once per atlas scale and the Lua sums digits. Each is
/// the glyph's [`client_step`] under the font's true outline (NumberFontNormal is outlined ⇒ +2),
/// exactly the width the render pass lays — none of [`measure_text`]'s wrap headroom.
pub(crate) fn digit_advances(atlas: &mut UiFontAtlas, font: FontSpec) -> [f32; 10] {
    let family = font
        .path
        .and_then(|p| atlas.path_to_family.get(&p.to_ascii_lowercase()))
        .unwrap_or(&atlas.default_family)
        .clone();
    let attrs = Attrs::new().family(Family::Name(&family));
    let font_size = atlas.snap_for(&family, font.height.unwrap_or(DEFAULT_FONT_SIZE));
    let scale = atlas.scale;
    let step_extra = step_extra_of(font.outline);
    let mut out = [0.0f32; 10];
    for (i, slot) in out.iter_mut().enumerate() {
        let s = char::from_digit(i as u32, 10).expect("0..=9").to_string();
        *slot = measure_line_width(
            &mut atlas.font_system,
            &atlas.glyphs,
            &attrs,
            font_size,
            scale,
            step_extra,
            &s,
        );
    }
    out
}

/// The drawn line's origin for the EditBox text-UI overlays: the line's x0 under `justifyH`
/// (where the engine's advance-derived caret/selection x-offsets are measured from), plus the
/// single-line cell's top and height — the same family/size snap, step law, and justify math as
/// [`layout_text_quads`], so the overlays sit exactly on the glyphs. Single-line by construction
/// (the EditBox law; a multiLine box would need the wrapped-line walk).
pub(crate) fn line_origin(
    atlas: &mut UiFontAtlas,
    drawn: &str,
    rect: Rect,
    justify: Justify,
    font: FontSpec,
) -> (f32, f32, f32) {
    let family = font
        .path
        .and_then(|p| atlas.path_to_family.get(&p.to_ascii_lowercase()))
        .unwrap_or(&atlas.default_family)
        .clone();
    let attrs = Attrs::new().family(Family::Name(&family));
    let font_size = atlas.snap_for(&family, font.height.unwrap_or(DEFAULT_FONT_SIZE));
    let scale = atlas.scale;
    let step_extra = step_extra_of(font.outline);
    // Everything below returns DRAWN-space geometry ([`drawn_k`]): the string's drawn width, the
    // caret cell's height, and the cell top all carry the 0581 rescale the glyph quads take.
    let k = drawn_k(font.height, font_size);
    // The line origin under justifyH (LEFT needs no width measure — the chat case).
    let x0 = match justify.h {
        JustifyH::Left => rect.min.x,
        JustifyH::Center | JustifyH::Right => {
            let w = k * measure_line_width(
                &mut atlas.font_system,
                &atlas.glyphs,
                &attrs,
                font_size,
                scale,
                step_extra,
                drawn,
            );
            if matches!(justify.h, JustifyH::Center) {
                rect.min.x + ((rect.width() - w) * 0.5).max(0.0)
            } else {
                rect.max.x - w
            }
        }
    };
    // The single-line block's v_offset — [`layout_text_quads_inner`]'s law with block_h = 1 line,
    // through the same ONE vertical snap ([`snap_block_top`]) so the caret/selection cell sits
    // exactly on the drawn glyphs — then the 0581 rescale about the same v anchor the draw
    // scales its quads about (snap first, scale second: the draw's own order).
    let top = if rect.height() > f32::EPSILON {
        let v_offset = match justify.v {
            JustifyV::Top => 0.0,
            JustifyV::Middle => (rect.height() - font_size) * 0.5,
            JustifyV::Bottom => rect.height() - font_size,
        };
        let snapped = snap_block_top(rect.min.y + v_offset);
        let anchor_y = match justify.v {
            JustifyV::Top => rect.min.y,
            JustifyV::Middle => (rect.min.y + rect.max.y) * 0.5,
            JustifyV::Bottom => rect.max.y,
        };
        anchor_y + (snapped - anchor_y) * k
    } else {
        rect.min.y
    };
    (x0, top, font_size * k)
}

/// The client's SINGLE vertical anchor snap (`anchor_justify_snap 0x5cdf70` @`0x5ce051`,
/// `fontstring-vertical-placement.md`): the composed block top — rect top plus the justify
/// offset — rounds ONCE to the integer pixel grid; the per-line ladder and the within-cell
/// ascender are added as exact integers after it, never re-snapped. The client's round runs in
/// the y-UP frame (`round(viewH·anchor.y)`), so a half-pixel tie pushes the block UP on screen —
/// in y-down units that is round-half-DOWN, `ceil(y − 0.5)` (the money row's H=13/S=14 MIDDLE
/// tie: offset −1, not 0 — the note's Case 3). Applied in LOGICAL units: the byte law at the
/// 768-tall design resolution (the 0292 posture), so a retina window seats text exactly as the
/// design-resolution layout, scaled.
fn snap_block_top(y: f32) -> f32 {
    snap_block_top_law(y) + UI_SEAT_NUDGE
}

/// The pure byte law of the snap ([`snap_block_top`] minus the taste nudge) — kept separate so
/// the tests pin the verified law itself, independent of the dial.
fn snap_block_top_law(y: f32) -> f32 {
    (y - 0.5).ceil()
}

/// The director's seat nudge: every UI FontString's text block sits one px LOWER than the byte
/// law's row. A deliberate **taste deviation** (decision 0351 — the 0104/0235 precedent: the
/// director's eye outranks the byte reading): the law is triple-confirmed (0338/0346), yet UI
/// text consistently reads high on the director's display; this is their call, applied at the
/// one seat every UI FontString shares ([`snap_block_top`] — world text's degenerate rects skip
/// it, keeping nameplates/combat text on their own approved seats). Rendering only — measures,
/// wrap, and the Lua metric echoes are untouched.
///
/// Public to the crate because a scissor that means to admit a block's ink has to know the seat
/// pushed it down — see the message-band clip in [`crate::ui_script`]'s text arm.
pub(crate) const UI_SEAT_NUDGE: f32 = 1.0;

/// The `SetAlphaGradient` ramp at one character position: opaque before `start`, a linear 1→0
/// fade across the next `length` characters, invisible beyond (hard edge when `length` ≤ 0).
fn gradient_alpha(index: usize, start: f32, length: f32) -> f32 {
    let i = index as f32;
    if i < start {
        1.0
    } else if length > 0.0 && i < start + length {
        1.0 - (i - start) / length
    } else {
        0.0
    }
}

/// A clickable hyperlink span the glyph layout produced: the union rect (y-down screen px, this
/// layout's own space) of one link's glyphs on ONE laid-out line — a wrapped link yields one span
/// per line — plus the link payload and its full `|H…|h…|h` markup for
/// `OnHyperlinkClick(link, markup, button)`.
pub(crate) struct LinkSpan {
    pub(crate) rect: Rect,
    pub(crate) link: String,
    pub(crate) markup: String,
}

pub(crate) fn layout_text_quads(
    atlas: &mut UiFontAtlas,
    text: &str,
    rect: Rect,
    base_color: [f32; 4],
    justify: Justify,
    z_key: u64,
    font: FontSpec,
) -> Vec<UiQuad> {
    layout_text_quads_inner(atlas, text, rect, base_color, justify, z_key, font, None)
}

/// [`layout_text_quads`] that also collects the laid-out [`LinkSpan`]s — the message-frame path
/// (chat lines carry `|H` item/player links; the app feeds the spans back to the engine's click
/// hit-test, `benilla_ui::script::UiScript::set_link_spans`).
#[allow(clippy::too_many_arguments)] // the shared layout context, plus one out-param
pub(crate) fn layout_text_quads_links(
    atlas: &mut UiFontAtlas,
    text: &str,
    rect: Rect,
    base_color: [f32; 4],
    justify: Justify,
    z_key: u64,
    font: FontSpec,
    links_out: &mut Vec<LinkSpan>,
) -> Vec<UiQuad> {
    layout_text_quads_inner(
        atlas,
        text,
        rect,
        base_color,
        justify,
        z_key,
        font,
        Some(links_out),
    )
}

#[allow(clippy::too_many_arguments)] // the public pair above is the real surface; this is their body
fn layout_text_quads_inner(
    atlas: &mut UiFontAtlas,
    text: &str,
    rect: Rect,
    base_color: [f32; 4],
    justify: Justify,
    z_key: u64,
    font: FontSpec,
    mut links_out: Option<&mut Vec<LinkSpan>>,
) -> Vec<UiQuad> {
    let gradient = font.alpha_gradient;
    let lines = parse_markup(text, base_color);
    // Resolve the face: the FontString's `font` path → its baked family, else Friz (the fallback).
    let family = font
        .path
        .and_then(|p| atlas.path_to_family.get(&p.to_ascii_lowercase()))
        .unwrap_or(&atlas.default_family)
        .clone();
    let attrs = Attrs::new().family(Family::Name(&family));
    // Resolve the size: the requested height snapped to the nearest baked size (default 12 if none).
    let font_size = atlas.snap_for(&family, font.height.unwrap_or(DEFAULT_FONT_SIZE));
    // DPI-aware layout: shape at the PHYSICAL size the atlas baked at, then divide advances/bearings
    // back to logical (kerning/hinting computed once at device resolution — never logical shaping with
    // scaled bitmaps pasted in, which drifts). `snap` rounds a logical coordinate to the nearest
    // physical pixel (a multiple of `1/scale`) so glyph edges land on device pixels — the same
    // integer-device-pixel snap the real client does, which keeps a physical-res bitmap crisp instead
    // of resampled. The quad's far edge is then automatically physical-aligned (a baked cell is an
    // integer count of physical texels).
    let scale = atlas.scale;
    let phys_size = font_size * scale;
    let snap = |v: f32| (v * scale).round() / scale;

    // Outline (`outline="NORMAL"/"THICK"`, the Number* fonts): cosmic-text bakes plain coverage, so
    // we synthesize the classic bitmap outline — stamp each glyph's baked bitmap in black at every
    // integer-logical offset in a filled `r×r` neighbourhood (`r=1` NORMAL, `r=2` THICK), *behind*
    // the fill. A filled block (not just the 8 ring cells) so a THICK 2px band has no gap between the
    // ink and its edge. Empty for the common un-outlined case (one alloc, skipped) — and for the
    // drop-shadow pass (`paint_halo: false`), which must STEP like its outlined fill (the law below)
    // but never paints halos (an outlined shadow would be a muddy black blob).
    let outline_offsets: Vec<(f32, f32)> = match font.outline {
        Outline::None => Vec::new(),
        _ if !font.paint_halo => Vec::new(),
        other => {
            let r: i32 = if matches!(other, Outline::Thick) {
                2
            } else {
                1
            };
            (-r..=r)
                .flat_map(|dy| (-r..=r).map(move |dx| (dx, dy)))
                .filter(|&(dx, dy)| dx != 0 || dy != 0)
                .map(|(dx, dy)| (dx as f32, dy as f32))
                .collect()
        }
    };
    // The client's step law ([`client_step`]) — the same bias measure/wrap use, so measure == render.
    let step_extra = step_extra_of(font.outline);
    // The THICK ink rise (`fontstring-baseline-row.md`, §5 trio): the client's THICK quad shift
    // (−2, `0x5cd0e1`) against its blit's baked seat nets the INK **1 row above** the plain
    // baseline law; NORMAL cancels exactly (outline-invariant). Our baked cell reproduces the
    // blit, so the quad carries the net: THICK cells rise 1 logical px. (The legacy stamped-halo
    // fallback skips this — an unplanned-combo path, noted not chased.)
    let thick_rise = if matches!(font.outline, Outline::Thick) {
        1.0
    } else {
        0.0
    };

    // Word-wrap within the rect when it carries a pinned width; an unsized single-point FontString
    // resolves to a ~zero-width rect (no constraint) and keeps its single-line behavior. Wrapping
    // runs first, fully into `render_lines`, so its `&mut atlas.font_system` borrow is released
    // before the emit pass takes it again.
    let mut render_lines: Vec<Vec<ColorRun>> = if rect.width() > WRAP_MIN_WIDTH {
        lines
            .iter()
            .flat_map(|line| {
                wrap_line(
                    &mut atlas.font_system,
                    &atlas.glyphs,
                    &attrs,
                    font_size,
                    scale,
                    step_extra,
                    line,
                    rect.width(),
                )
            })
            .collect()
    } else {
        lines
    };
    // The height-limit line stack (regime 2's vertical half, `CGxString+0x40`,
    // `fontstring-overflow.md`): a height-pinned rect stops emitting wrapped lines when the
    // accumulated pitch passes it. A FontString the ellipsis seam ([`ellipsize_to_fit`]) already
    // truncated fits by construction, so this is the belt under any caller that skips that seam —
    // in the client the same split: the ui truncate feeds the gx string, and the gx line stack
    // clamps regardless. The pitch is the font em (the emit's `pitch` below — same law).
    if rect.height() > HEIGHT_LIMIT_MIN {
        render_lines.truncate(overflow::lines_allowed(rect.height(), font_size));
    }

    let mut quads = Vec::new();
    // `justifyV` positions the line *block* — N lines at the client's pitch, which IS the font
    // height (line-step law, the module doc's NOTE on line pitch) — vertically within `rect`. The
    // real FontString default is MIDDLE, which is what seats the money digits on their coin icons'
    // centerline. A rect sized *by* the text (the host-measure round-trip's height-less
    // FontStrings) has `height == block`, so the offset degenerates to 0 there — only
    // explicitly-sized rects shift. A degenerate rect (unsized single-point anchor, pre-measure)
    // keeps the v1 top placement rather than hoisting text above its anchor.
    // The baked outlined-cell variant this spec draws from (`0` = plain). Shadow passes
    // (`paint_halo: false`) use the SAME variant — the client's shadow is a second draw of the
    // same string (fontstring-shadow.md), so it redraws the composited cells in the shadow color
    // (the black ring stays black under the tint; a black shadow is indistinguishable either way).
    let cell_r = super::atlas::outline_radius(font.outline);
    // The line pitch is the font em, NO outline pad — the byte law
    // (`fontstring-vertical-placement.md`, VERIFIED at `0x5cdc20`/`0x5cdf70`): the client's
    // origin ladder steps `S + spacingPx` (spacing 0 shipped) and the justify block is
    // `h = N·S`; the `+2r` lives ONLY in the atlas cell (`[font+0x178]`), which the layout
    // never reads. (The pre-verdict `+2r` here seated every outlined MIDDLE string r px too
    // high — the money-digit bug.) Stacked outlined lines' rings may touch — faithful.
    let pitch = font_size;
    let block_h = render_lines.len() as f32 * pitch;
    let v_offset = if rect.height() > f32::EPSILON {
        match justify.v {
            JustifyV::Top => 0.0,
            JustifyV::Middle => (rect.height() - block_h) * 0.5,
            JustifyV::Bottom => rect.height() - block_h,
        }
    } else {
        0.0
    };
    // Baseline of the first line: the row the client hangs ink from — the face's pixel ascender
    // `[CGxFont+0x17c] = round(size · asc/(asc+|desc|))`, threaded unchanged into `glyph_vplace`
    // 0x5d1360 as the operand that fixes `baseline = cellTop + ascender` (wow-re `system/font`,
    // §5-verified 2026-07-09). The per-family ratio was read off the raw TTF at atlas bake
    // ([`UiFontAtlas::family_ascent`]); the fallback only fires for a face whose tables failed to
    // parse (never the four shipped client fonts) and approximates Friz's 0.794 (965/1215). NOT
    // `asc/upem` ≈ 0.965 — that is the FreeType scaled hhea ascender, which appears nowhere in the
    // placement path; seating with it dropped every line ~3px too low (baseline row 13 vs 10 in a
    // 13-tall cell — the gossip/quest row misalignment against its 16-tall icon).
    let ascent_ratio = atlas.family_ascent.get(&family).copied().unwrap_or(0.794);
    let baseline_in_cell = (f64::from(font_size) * f64::from(ascent_ratio) + 0.5).floor() as f32;
    // The block top takes the client's ONE vertical snap ([`snap_block_top`]); a degenerate
    // (single-point / world-text) rect keeps its exact fractional origin — those callers
    // (nameplates/vplates/combat_text) own their own seating laws and re-seat by ink.
    let block_top = if rect.height() > f32::EPSILON {
        snap_block_top(rect.min.y + v_offset)
    } else {
        rect.min.y + v_offset
    };
    let mut pen_y = block_top + baseline_in_cell;

    // One glyph laid at a line-relative x-origin of 0, pending the line's justification shift.
    struct PendingGlyph {
        x_rel: f32,
        /// Vertical offset from the line baseline (`pen_y`): `glyph.y − bearing_top`.
        y_rel: f32,
        uv: Rect,
        px_w: f32,
        px_h: f32,
        color: [f32; 4],
        /// True = this glyph drew from a PLAIN cell and still owes its outline to the legacy
        /// stamped-halo pass (an outlined font whose `(face, size, r)` variant wasn't baked —
        /// an unplanned runtime combo). False = the cell already composites ring+fill (the
        /// baked-outline architecture, blit dispatch `0x5cf310`): one quad, fades as one, and
        /// the halo pass must skip it or the ring would double.
        haloed: bool,
    }

    // The write-on gradient's character counter (`SetAlphaGradient`): a glyph's position is its
    // char index in the emitted run stream — markup codes stripped, wrap-carried whitespace
    // counted where it rides. That drifts from the source string by at most the newline count
    // (the engine-side return condition counts every source char), which only pads the tail of
    // the reveal by a frame or two — the ramp itself is exact per drawn glyph.
    let mut chars_before: usize = 0;
    for line in &render_lines {
        // First pass: lay the line's glyphs at line-origin 0 and measure its total width.
        let mut pending: Vec<PendingGlyph> = Vec::new();
        // This line's hyperlink x-ranges (line-origin space), one entry per distinct link — a
        // link's runs are contiguous, so min/max of its runs' pen extents is its span.
        let mut line_links: Vec<(std::sync::Arc<super::markup::LinkInfo>, f32, f32)> = Vec::new();
        let mut pen_x = 0.0f32;
        for run in line {
            if run.text.is_empty() {
                continue;
            }
            let run_x0 = pen_x;
            let mut buffer =
                Buffer::new(&mut atlas.font_system, Metrics::new(phys_size, phys_size));
            buffer.set_wrap(&mut atlas.font_system, Wrap::None);
            buffer.set_text(
                &mut atlas.font_system,
                &run.text,
                &attrs,
                Shaping::Advanced,
                None,
            );
            buffer.shape_until_scroll(&mut atlas.font_system, false);

            // Physical shaping → logical layout: every metric off the buffer / baked cell is
            // physical, so divide by `scale` on the way into logical `pen`/`x_rel`/cell space.
            // The pen advances by the client's STEP LAW ([`client_step`]) per glyph — cosmic's own
            // cumulative `glyph.x` (fractional kerned advances) is deliberately unused for
            // horizontal placement: the real client lays each glyph at pen + bearing and steps by
            // its integer-floored advance + 1 (+1 outlined). measure_line_width sums the identical
            // steps, so measure == render == the client's GetTextWidth.
            for layout_run in buffer.layout_runs() {
                for glyph in layout_run.glyphs {
                    // An outlined spec prefers its composited (ring+fill) cell variant; a miss —
                    // an unplanned runtime (face, size, r) — falls back to the plain cell plus
                    // the stamped-halo pass. Advances agree across variants, so the step law
                    // (and with it measure == render) is untouched by which cell drew.
                    let plain: GlyphKey = (glyph.font_id, glyph.glyph_id, font_size.to_bits(), 0);
                    let (info, haloed) = if cell_r > 0 {
                        match atlas.glyphs.get(&(plain.0, plain.1, plain.2, cell_r)) {
                            Some(outlined) => (Some(outlined), false),
                            None => (atlas.glyphs.get(&plain), true),
                        }
                    } else {
                        (atlas.glyphs.get(&plain), false)
                    };
                    match info {
                        Some(info) => {
                            if info.px_w > 0.0 && info.px_h > 0.0 {
                                let mut color = run.color;
                                if let Some((start, length)) = gradient {
                                    let idx =
                                        chars_before + run.text[..glyph.start].chars().count();
                                    color[3] *= gradient_alpha(idx, start, length);
                                }
                                // `thick_rise` applies only through the baked composite cell —
                                // the client's net is quad-vs-blit, and the halo fallback has
                                // no shifted quad to cancel against.
                                let rise = if haloed { 0.0 } else { thick_rise };
                                pending.push(PendingGlyph {
                                    x_rel: pen_x + info.bearing_x / scale,
                                    y_rel: glyph.y / scale - info.bearing_top / scale - rise,
                                    uv: info.uv,
                                    px_w: info.px_w / scale,
                                    px_h: info.px_h / scale,
                                    color,
                                    haloed,
                                });
                            }
                            pen_x += client_step(info.advance, step_extra, scale);
                        }
                        None => {
                            // No baked glyph (outside default_charset) — step past it anyway so
                            // later glyphs on the same line don't overlap it.
                            pen_x += client_step(glyph.w, step_extra, scale);
                        }
                    }
                }
            }
            chars_before += run.text.chars().count();
            if let Some(info) = &run.link {
                match line_links
                    .iter_mut()
                    .find(|(a, _, _)| std::sync::Arc::ptr_eq(a, info))
                {
                    Some((_, _, x1)) => *x1 = pen_x,
                    None => line_links.push((info.clone(), run_x0, pen_x)),
                }
            }
        }

        // Second pass: shift the whole line by the justification offset and emit.
        let line_width = pen_x;
        let origin_x = match justify.h {
            JustifyH::Left => rect.min.x,
            JustifyH::Center => rect.min.x + (rect.width() - line_width) * 0.5,
            JustifyH::Right => rect.max.x - line_width,
        };
        // LEGACY outline layer — only for glyphs whose baked outlined cell is missing (`haloed`):
        // every such glyph's black halo, pushed **before** any fill — so the stable z-sort's
        // push-order tiebreak keeps every outline stamp behind every fill (including a later
        // glyph's fill, where tight kerning overlaps them). Stacked stamps under a translucent
        // fill BLACKEN mid-fade (the `α(1−α)` compositing term) — which is exactly why the baked
        // composite cell is the primary path (the fade-composite fold-back record); this pass
        // survives as the any-combo fallback, correct at full alpha.
        for &(dx, dy) in &outline_offsets {
            for g in pending.iter().filter(|g| g.haloed) {
                let gx = snap(origin_x + g.x_rel) + dx;
                let gy = snap(pen_y + g.y_rel) + dy;
                quads.push(UiQuad {
                    rect: Rect::new(gx, gy, gx + g.px_w, gy + g.px_h),
                    z_key,
                    texture: Some(atlas.image.clone()),
                    uv: UvRect::from_rect(g.uv),
                    // Black, riding the fill's alpha so a fading line's outline fades with it.
                    color: [0.0, 0.0, 0.0, g.color[3]],
                    ..default()
                });
            }
        }
        for g in &pending {
            // Snap the glyph origin to the physical-pixel grid (not the logical grid — that would
            // re-blur on a retina display). `px_w/px_h` are an integer texel count divided by `scale`,
            // so `gx + px_w` lands on a device pixel too.
            let gx = snap(origin_x + g.x_rel);
            let gy = snap(pen_y + g.y_rel);
            quads.push(UiQuad {
                rect: Rect::new(gx, gy, gx + g.px_w, gy + g.px_h),
                z_key,
                texture: Some(atlas.image.clone()),
                // Glyph atlas cells are always normalized (never mirrored); pass them straight through.
                uv: UvRect::from_rect(g.uv),
                color: g.color,
                ..default()
            });
        }
        // The line's hyperlink spans, shifted by the same justification origin the glyphs took
        // (hit rects cover the glyph cells: cell top = baseline − ascender, one font-height tall).
        if let Some(out) = links_out.as_deref_mut() {
            let cell_top = pen_y - baseline_in_cell;
            for (info, x0, x1) in line_links.drain(..) {
                out.push(LinkSpan {
                    rect: Rect::new(origin_x + x0, cell_top, origin_x + x1, cell_top + font_size),
                    link: info.link.clone(),
                    markup: info.markup.clone(),
                });
            }
        }
        // Next line: the client's pitch — the font height plus the outlined pad (see `pitch`;
        // lineStep = px(size) + spacing, spacing 0; `LayoutLines` 0x5cdc20).
        pen_y += pitch;
    }

    // The exact-height rescale (decision 0581): a requested height BETWEEN baked sizes — the
    // runtime `SetTextHeight` animations (CombatFeedback's ×1.5 crit, CombatText's 30→60 pop;
    // the real client scales the laid-out string exactly there) — shapes at the snapped size
    // above and scales the finished quads about the justify anchor to the true height. Every
    // registry-declared size is baked (the atlas census), so this pass is identity for all
    // ordinary UI text and the physical-pixel snaps above stay untouched; the worldtext caller
    // passes its own pre-snapped size (k = 1) and keeps its private seat law. The height is
    // taken RAW: regime capping is the caller's (`drawn_px` — a SetTextHeight size is uncapped,
    // decision 0582), so no re-cap here.
    if let Some(req) = font.height {
        let k = req / font_size;
        if (k - 1.0).abs() > 1e-3 {
            let anchor = Vec2::new(
                match justify.h {
                    JustifyH::Left => rect.min.x,
                    JustifyH::Center => (rect.min.x + rect.max.x) * 0.5,
                    JustifyH::Right => rect.max.x,
                },
                match justify.v {
                    JustifyV::Top => rect.min.y,
                    JustifyV::Middle => (rect.min.y + rect.max.y) * 0.5,
                    JustifyV::Bottom => rect.max.y,
                },
            );
            for q in &mut quads {
                q.rect = Rect {
                    min: anchor + (q.rect.min - anchor) * k,
                    max: anchor + (q.rect.max - anchor) * k,
                };
            }
            if let Some(out) = links_out {
                for l in out.iter_mut() {
                    l.rect = Rect {
                        min: anchor + (l.rect.min - anchor) * k,
                        max: anchor + (l.rect.max - anchor) * k,
                    };
                }
            }
        }
    }

    quads
}

#[cfg(test)]
mod seat_tests {
    use super::*;

    /// The composed MIDDLE seat for one line: block top from box top, per
    /// `fontstring-vertical-placement.md` — `d = H − round_half_away((H+h)/2)` in the client's
    /// y-up frame, which [`snap_block_top`] reproduces in y-down over the exact offset.
    fn middle_d(box_h: f32, block_h: f32) -> f32 {
        snap_block_top_law((box_h - block_h) * 0.5)
    }

    #[test]
    fn middle_seat_matches_the_three_verified_cases() {
        // Case 1 — Friz 12 in a 12-tall box (the bag title): d = 0.
        assert_eq!(middle_d(12.0, 12.0), 0.0);
        // Case 2 — Friz 10 in a 10-tall box (unit-frame name): d = 0.
        assert_eq!(middle_d(10.0, 10.0), 0.0);
        // Case 3 — arialn 14 (NO outline pad in the block!) in a 13-tall box (the money row):
        // d = 13 − round(13.5) = −1 — the celltop one px ABOVE the box top, the S>H degenerate.
        assert_eq!(middle_d(13.0, 14.0), -1.0);
    }

    #[test]
    fn the_tie_rounds_up_on_screen() {
        // The client's one snap runs in the y-up frame (round-half-away), so a .5 tie pushes
        // the block UP on screen = down-rounds in y-down units.
        assert_eq!(snap_block_top_law(10.5), 10.0);
        assert_eq!(snap_block_top_law(-0.5), -1.0);
        // Non-ties round normally.
        assert_eq!(snap_block_top_law(10.4), 10.0);
        assert_eq!(snap_block_top_law(10.6), 11.0);
    }

    #[test]
    fn the_seat_is_the_law_plus_the_directors_nudge() {
        // The drawn seat = the byte law + the 1px taste nudge (decision 0351, director's call).
        assert_eq!(snap_block_top(10.5), snap_block_top_law(10.5) + 1.0);
        assert_eq!(snap_block_top(0.0), 1.0);
    }

    #[test]
    fn the_measures_ride_the_drawn_space_factor() {
        // [`drawn_k`] mirrors the 0581 rescale gate exactly: identity at a baked size and for
        // height-None; the requested/snapped ratio between bakes (0989 — the era-scaled options
        // window put every request between bakes, and the caret drifted 28% past its text).
        assert_eq!(drawn_k(None, 8.0), 1.0);
        assert_eq!(drawn_k(Some(8.0), 8.0), 1.0);
        assert_eq!(drawn_k(Some(8.004), 8.0), 1.0); // inside the draw's 1e-3 gate
        let k = drawn_k(Some(6.398), 8.203);
        assert!((k - 6.398 / 8.203).abs() < 1e-6);
    }
}

#[cfg(test)]
mod gradient_tests {
    use super::*;

    #[test]
    fn gradient_ramp_is_opaque_then_linear_then_invisible() {
        // SetAlphaGradient(10, 30): chars 0..10 opaque, 10..40 ramp 1→0, 40+ invisible.
        assert_eq!(gradient_alpha(0, 10.0, 30.0), 1.0);
        assert_eq!(gradient_alpha(9, 10.0, 30.0), 1.0);
        assert_eq!(gradient_alpha(10, 10.0, 30.0), 1.0); // ramp start = leading edge, fully lit
        assert!((gradient_alpha(25, 10.0, 30.0) - 0.5).abs() < 1e-6);
        assert_eq!(gradient_alpha(40, 10.0, 30.0), 0.0);
        assert_eq!(gradient_alpha(1000, 10.0, 30.0), 0.0);
        // Degenerate length: a hard reveal edge, never a divide-by-zero.
        assert_eq!(gradient_alpha(5, 10.0, 0.0), 1.0);
        assert_eq!(gradient_alpha(15, 10.0, 0.0), 0.0);
    }
}

#[cfg(test)]
mod row_start_tests {
    use super::*;

    fn runs(texts: &[&str]) -> Vec<Vec<ColorRun>> {
        texts
            .iter()
            .map(|t| {
                vec![ColorRun {
                    text: (*t).to_string(),
                    color: [1.0, 1.0, 1.0, 1.0],
                    link: None,
                }]
            })
            .collect()
    }

    fn starts(seg: &str, sub: &[Vec<ColorRun>], base: usize) -> Vec<usize> {
        let mut rows = Vec::new();
        segment_row_starts(seg, sub, base, &mut rows);
        rows
    }

    #[test]
    fn a_word_break_swallows_its_separator() {
        // wrap dropped the space after "quick": row 2 starts past it.
        assert_eq!(
            starts("the quick brown", &runs(&["the quick", "brown"]), 0),
            vec![0, 10]
        );
    }

    #[test]
    fn a_force_broken_word_swallows_nothing() {
        assert_eq!(starts("abcdefgh", &runs(&["abcde", "fgh"]), 0), vec![0, 5]);
    }

    #[test]
    fn a_double_space_separator_is_swallowed_whole() {
        assert_eq!(starts("a.  b", &runs(&["a.", "b"]), 0), vec![0, 4]);
    }

    #[test]
    fn inner_whitespace_stays_verbatim_inside_a_row() {
        // "a  b" wrapped as one row keeps its double space; "c" starts past the break's space.
        assert_eq!(starts("a  b c", &runs(&["a  b", "c"]), 0), vec![0, 5]);
    }

    #[test]
    fn a_blank_segment_still_occupies_a_row_and_base_offsets() {
        assert_eq!(starts("", &[], 7), vec![7]);
        assert_eq!(starts("xy", &runs(&["xy"]), 3), vec![3]);
    }
}
