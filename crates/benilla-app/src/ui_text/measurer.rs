//! **The font engine, as the script VM holds it** — the app's half of
//! [`benilla_ui::script::TextMeasure`].
//!
//! The engine seam (`benilla-ui`'s `script::measure`) exists because the reference answers
//! `GetStringWidth` from its font engine *inline* and ours could not: measuring needed the baked
//! atlas, and the atlas only met the VM at the frame boundary. What closes the gap is that the
//! client's width law is a **table**, not a computation — a sum of per-glyph steps with no kerning
//! and no neighbour term — so [`TextMetrics`] carries everything a measure needs and nothing that
//! needs `&mut`. That table is `Arc`-shared with [`UiFontAtlas`](super::atlas::UiFontAtlas) rather
//! than copied, so the answer a Lua call gets mid-tick and the answer the extract pass computes are
//! not merely equal — they are the same lookup in the same table.
//!
//! **One body, both callers.** [`measure_request`] is the whole of it; the batch pass
//! (`ui_script::extract::measure_fontstrings`) and [`AtlasMeasurer`] both call it and neither has
//! any measuring logic of its own. That is the trait's own contract, kept structurally rather than
//! by discipline.

use std::sync::Arc;

use benilla_ui::script::{MeasureRequest, TextMeasure};

use super::atlas::TextMetrics;

/// Answer one [`MeasureRequest`]: `(laid_out_w, laid_out_h, natural_w)` in the region's own
/// frame-local units.
///
/// `seam` is the host's screen scale ([`crate::ui_script::seam_scale`]); the request's own `scale`
/// is the owner frame's `effective_scale`. Measuring happens at the product — the exact drawn
/// raster size — and the answer divides the whole product back out, because integer-stepped glyph
/// advances do not commute with scaling (see [`MeasureRequest::scale`]).
pub(crate) fn measure_request(
    metrics: &TextMetrics,
    seam: f32,
    r: &MeasureRequest,
) -> (f32, f32, f32) {
    let rs = seam * r.scale;
    let spec = || super::FontSpec {
        path: r.font.as_deref(),
        // The render pass's exact drawn px (two regimes × the full scale) — measure == render, and
        // Lua's GetStringWidth/Height echo the DRAWN size (0x772890); results divide back to
        // frame-local UI units below.
        height: super::drawn_px(r.height, r.text_height, rs),
        // The TRUE outline: THICK biases the client's step law (+1px per glyph — GlyphStepBase
        // 0x5ca2b0, THICK-only per outline-bake-tint.md) and any outline adds the +2r line pitch,
        // so measure must see it.
        outline: r.outline,
        paint_halo: true,     // measure never paints; irrelevant here
        alpha_gradient: None, // alpha never changes metrics
    };
    let wrap = r.wrap_width.map(|w| w * rs);
    let (w, h) = super::measure_text(metrics, &r.text, wrap, spec());
    // …and the NATURAL width, which is what `GetStringWidth` answers with (the reference measures
    // its getter's string with no wrap constraint — wow-re `fontstring-overflow.md`, "The
    // measurement echo"). A second pass only for the regions that actually carry a declared width;
    // for the rest the two are one number.
    let natural = if wrap.is_some() {
        super::measure_text(metrics, &r.text, None, spec()).0
    } else {
        w
    };
    (w / rs, h / rs, natural / rs)
}

/// The measurer installed into the VM — the shared [`TextMetrics`] plus the host's current screen
/// seam.
///
/// **Rebuilt, not mutated, when the seam moves.** A measurer is only correct for the raster
/// environment it was made under (glyph advances step to whole *physical* pixels), so the host
/// installs a fresh one at exactly the moment it calls
/// [`UiScript::forget_text_metrics`](benilla_ui::script::UiScript::forget_text_metrics) — the same
/// edge, for the same reason. Building one is an `Arc` clone and an `f32`.
pub(crate) struct AtlasMeasurer {
    metrics: Arc<TextMetrics>,
    seam: f32,
}

impl AtlasMeasurer {
    pub(crate) fn new(metrics: Arc<TextMetrics>, seam: f32) -> Self {
        Self { metrics, seam }
    }
}

impl TextMeasure for AtlasMeasurer {
    fn measure(&mut self, req: &MeasureRequest) -> (f32, f32, f32) {
        measure_request(&self.metrics, self.seam, req)
    }
}
