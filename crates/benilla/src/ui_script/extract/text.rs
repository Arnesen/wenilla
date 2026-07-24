//! The Text-arm rasterization: one `QuadContent::Text` quad — a region FontString, a
//! message-frame ring line, or the focused editbox's windowed text — into glyph [`UiQuad`]s:
//! the editbox window + selection/caret, the ellipsis-truncate seam, the drop shadow, and the
//! hyperlink span collection. Split out of the extraction pass ([`super`]) when it crossed the
//! size budget; the arm's host-loop context arrives bundled as [`TextHost`].

use bevy::prelude::*;

use benilla_ui::order::ZTarget;
use benilla_ui::script::{EditBoxTextUi, FontShadow, JustifyH, JustifyV, Outline};
use benilla_ui::widget::FrameHandle;

use crate::ui_pass::{UiQuad, UvRect};
use crate::ui_text::{layout_text_quads, layout_text_quads_links, UiFontAtlas};

/// The `QuadContent::Text` payload minus the text itself — the region's resolved style, passed
/// verbatim from the destructure at the [`super::drive_script`] call site.
pub(super) struct TextStyle {
    pub color: Option<[f32; 4]>,
    pub justify_h: JustifyH,
    pub justify_v: JustifyV,
    pub font: Option<String>,
    pub font_height: Option<f32>,
    pub text_height: Option<f32>,
    pub shadow: Option<FontShadow>,
    pub outline: Outline,
    pub alpha_gradient: Option<(f32, f32)>,
}

/// The host-loop context one Text quad draws under: the extracted quad's identity (z/alpha/
/// target), its y-flipped rect + clip, the focused editbox's text-UI geometry (matched to this
/// quad inside), and the screen height for the link-span flip back to engine space.
pub(super) struct TextHost<'a> {
    pub z: u64,
    pub alpha: f32,
    pub target: ZTarget,
    pub rect: Rect,
    pub clip: Option<Rect>,
    /// The focused editbox's text-UI geometry, unfiltered — [`emit`] matches it to this quad
    /// by [`EditBoxTextUi::target`].
    pub ebox: Option<&'a EditBoxTextUi>,
    pub screen_h: f32,
    /// The 768-virtual scale `s = windowH/768` (decision 0582): rects arrive pre-scaled (px);
    /// this converts the remaining unit-space inputs — font heights, shadow offsets, the
    /// engine's editbox caret/selection x-offsets — into the same px space.
    pub scale: f32,
    /// Captures pin the caret blink ON (deterministic pixels); live, the engine's phase decides.
    pub caret_pinned: bool,
}

/// Rasterize one Text quad into `out` (and its hyperlink spans into `link_spans`, message-frame
/// lines only). The glyph quads share the owning region's `z` — see [`layout_text_quads`]'s doc
/// for why that's already the correct total-order slot.
pub(super) fn emit(
    atlas: &mut UiFontAtlas,
    text: &str,
    style: TextStyle,
    host: TextHost<'_>,
    out: &mut Vec<UiQuad>,
    link_spans: &mut Vec<(FrameHandle, benilla_ui::layout::Rect, String, String)>,
) {
    let base_color = style.color.unwrap_or([1.0, 1.0, 1.0, 1.0]);
    let spec = crate::ui_text::FontSpec {
        path: style.font.as_deref(),
        // The drawn px under the two size regimes × the 768-virtual scale (`drawn_px`, decision
        // 0582): one-to-one text unit-caps at 32 then scales; a SetTextHeight override scales
        // uncapped. The shadow twin below inherits the spec (`..spec`).
        height: crate::ui_text::drawn_px(style.font_height, style.text_height, host.scale),
        outline: style.outline,
        paint_halo: true,
        alpha_gradient: style.alpha_gradient,
    };
    // The focused edit box's text draws WINDOWED (`0x77da80`): the scroll window's substring,
    // left-anchored at the line origin in an unbounded rect (the window never wraps — the box
    // edge clips instead), the selection highlight behind everything, the white caret after.
    // `line_origin` gives the origin + the line cell; the x-offsets inside it are the engine's
    // advance-derived geometry.
    let ebox = host.ebox.filter(|u| u.target == host.target);
    let mut draw_text: &str = text;
    let mut draw_rect = host.rect;
    let mut draw_justify = crate::ui_text::Justify {
        h: style.justify_h,
        v: style.justify_v,
    };
    let mut text_clip = host.clip;
    let mut ebox_geom = None;
    if let Some(ui) = ebox {
        let drawn = &text[ui.display_from.min(text.len())..];
        let (x0, top, cell_h) =
            crate::ui_text::line_origin(atlas, drawn, host.rect, draw_justify, spec);
        ebox_geom = Some((x0, top, cell_h));
        text_clip = Some(host.clip.map_or(host.rect, |c| c.intersect(host.rect)));
        if !ui.multi_line {
            // Single-line: the windowed draw — the scroll window's substring, left-anchored in
            // an unbounded rect (the box edge clips instead of wrapping).
            draw_text = drawn;
            draw_rect = Rect::new(x0, host.rect.min.y, x0 + 100_000.0, host.rect.max.y);
            draw_justify = crate::ui_text::Justify {
                h: JustifyH::Left,
                v: style.justify_v,
            };
        }
        // Multiline draws the whole block through the ordinary wrapped path below (the text
        // region's justify is already the editbox law: TOP/LEFT) — only the caret/selection are
        // seated by `(row, x)` at the same row pitch the wrap answered.
        for &(row, sx0, sx1) in &ui.selection {
            let hc = ui.highlight_color;
            #[allow(clippy::cast_precision_loss)]
            let ry = top + row as f32 * cell_h;
            // The engine's selection x-span is in UI units (its advance table was fed ÷scale).
            out.push(UiQuad {
                rect: Rect::new(
                    x0 + sx0 * host.scale,
                    ry,
                    x0 + sx1 * host.scale,
                    ry + cell_h,
                ),
                z_key: host.z,
                texture: None,
                uv: UvRect::FULL,
                color: [hc[0], hc[1], hc[2], hc[3] * host.alpha],
                additive: false,
                circular: false,
                clip: text_clip,
                rotation: 0.0,
                mask: None,
                corners: None,
            });
        }
    }
    // The height-gated ellipsis-truncate (`ellipsize_to_fit` — CSimpleFontString `0x771ec0`,
    // decision 0292's residue landed): a region FontString whose wrapped text needs more lines
    // than its height-pinned rect allows draws `prefix + "..."` instead — the bag title, the
    // unit-frame names, the minimap zone text. Region FontStrings only, exactly the client's
    // seam: the editbox windows (never truncates), message-frame ring lines size their band FROM
    // the text, and auto-height FontStrings fit by construction (the measure round-trip), so the
    // gate inside is geometric and needs no fixed-vs-measured flag. Computed before the shadow
    // so the shadow pass draws the same display string (the client's shadow is a second draw of
    // the same truncated CGxString).
    let ellipsized = if ebox.is_none() && !matches!(host.target, ZTarget::Frame(_)) {
        crate::ui_text::ellipsize_to_fit(atlas, draw_text, draw_rect, spec)
    } else {
        None
    };
    if let Some(display) = ellipsized.as_deref() {
        draw_text = display;
    }
    // The probe prints the DISPLAY string (post-ellipsis, post-editbox-window) — what this pass
    // actually draws, which is what a truncation report needs to show.
    let probe = std::env::var("WOW_TEXT_PROBE").as_deref() == Ok("1");
    if probe {
        info!(
            "text probe: [{:.0},{:.0} {:.0}x{:.0}] h={:?} {:?}",
            draw_rect.min.x,
            draw_rect.min.y,
            host.rect.width(),
            host.rect.height(),
            style.font_height,
            &draw_text[..draw_text.len().min(30)]
        );
    }
    // The drop shadow (font object `<Shadow>` — MasterFont's (1,-1) black covers the whole
    // GameFont* family): the same layout at an offset rect in the shadow color, pushed FIRST so
    // the stable z-sort keeps it behind the glyph pass. Offset is WoW y-up (`y="-1"` = down) →
    // y-down screen dy = −y. Markup color codes inside the text tint the shadow run too (v1
    // corner, invisible for solid-color strings). The shadow is a single flat offset copy —
    // never itself outlined (an outlined shadow would be a muddy black blob), so it suppresses
    // the halo PAINT only: it keeps the font's true outline for the step law, laying out
    // identically to its fill (a shadow with different steps would smear under long strings).
    if let Some(sh) = style.shadow {
        let srect = Rect::new(
            draw_rect.min.x + sh.offset[0] * host.scale,
            draw_rect.min.y - sh.offset[1] * host.scale,
            draw_rect.max.x + sh.offset[0] * host.scale,
            draw_rect.max.y - sh.offset[1] * host.scale,
        );
        let shadow_spec = crate::ui_text::FontSpec {
            paint_halo: false,
            ..spec
        };
        let mut sq = layout_text_quads(
            atlas,
            draw_text,
            srect,
            sh.color,
            draw_justify,
            host.z,
            shadow_spec,
        );
        for q in &mut sq {
            // Flatten rgb to the shadow color (markup tints ride the fill only), but multiply
            // the layout's own alpha — it carries the shadow's base alpha AND the write-on
            // gradient's per-glyph ramp (a revealing char's shadow fades with its fill).
            q.color = [
                sh.color[0],
                sh.color[1],
                sh.color[2],
                q.color[3] * host.alpha,
            ];
            // Every glyph quad inherits the owning Text quad's ScrollFrame clip — the
            // FontString's own extract-time clip, not a per-glyph concept.
            q.clip = text_clip;
        }
        out.extend(sq);
    }
    // `font`/`font_height` come from the region's resolved font object (`Fonts.xml`).
    let mut glyphs = if let ZTarget::Frame(fh) = host.target {
        // A frame-targeted Text quad is a message-frame ring line: collect its hyperlink spans
        // for the engine's click hit-test (y-down → y-up flip).
        let mut spans = Vec::new();
        let g = layout_text_quads_links(
            atlas,
            text,
            host.rect,
            base_color,
            crate::ui_text::Justify {
                h: style.justify_h,
                v: style.justify_v,
            },
            host.z,
            spec,
            &mut spans,
        );
        for sp in spans {
            // px → the engine's y-up UI-unit space (÷scale after the flip).
            link_spans.push((
                fh,
                benilla_ui::layout::Rect::new(
                    (host.screen_h - sp.rect.max.y) / host.scale,
                    sp.rect.min.x / host.scale,
                    (host.screen_h - sp.rect.min.y) / host.scale,
                    sp.rect.max.x / host.scale,
                ),
                sp.link,
                sp.markup,
            ));
        }
        g
    } else {
        layout_text_quads(
            atlas,
            draw_text,
            draw_rect,
            base_color,
            draw_justify,
            host.z,
            spec,
        )
    };
    for q in &mut glyphs {
        q.color[3] *= host.alpha;
        // Every glyph quad inherits the Text quad's ScrollFrame clip (decision 0112) —
        // `ui_pass`'s CPU clip already applies uniformly to any `UiQuad`, glyph or not.
        q.clip = text_clip;
    }
    // The probe's seat line: the drawn INK rows (glyph-quad union, logical px, relative to the
    // rect top) — the measurable half of the vertical-seat law
    // (`fontstring-vertical-placement.md`): compare `ink` against the law's `d + ascender` seat
    // when hunting a vertical offset. Fill quads only (the shadow pass above would smear the
    // bounds one px down-right).
    if probe && !glyphs.is_empty() {
        let (mut y0, mut y1) = (f32::MAX, f32::MIN);
        for q in &glyphs {
            y0 = y0.min(q.rect.min.y);
            y1 = y1.max(q.rect.max.y);
        }
        info!(
            "seat probe: top={:.2} ink=[{:.2}..{:.2}] (rel {:.2}..{:.2}) {:?}",
            host.rect.min.y,
            y0,
            y1,
            y0 - host.rect.min.y,
            y1 - host.rect.min.y,
            &draw_text[..draw_text.len().min(20)]
        );
    }
    out.extend(glyphs);
    // The focused box's caret: a 1-px WHITE bar (the client's ctor `0xffffffff` caret texture —
    // never the text color) one line cell tall at the engine's advance-derived x, pushed after
    // the glyphs so the stable sort draws it on top.
    if let (Some(ui), Some((x0, top, cell_h))) = (ebox, ebox_geom) {
        if host.caret_pinned || ui.caret_on {
            #[allow(clippy::cast_precision_loss)]
            let top = top + ui.caret_row as f32 * cell_h;
            // caret_x is engine UI units (÷scale advances); the 1-px bar width stays device-thin
            // (the client's 4-unit caret is a named residual, ui.md's caret law).
            let cx = x0 + ui.caret_x * host.scale;
            out.push(UiQuad {
                rect: Rect::new(cx, top, cx + 1.0, top + cell_h),
                z_key: host.z,
                texture: None,
                uv: UvRect::FULL,
                color: [1.0, 1.0, 1.0, host.alpha],
                additive: false,
                circular: false,
                clip: text_clip,
                rotation: 0.0,
                mask: None,
                corners: None,
            });
        }
    }
}
