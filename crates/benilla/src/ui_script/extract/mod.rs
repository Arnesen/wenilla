//! The extraction pass: [`drive_script`] turns [`UiScript::extract`]'s per-frame output into
//! [`UiQuads`] for the render pass (screen size → tick → resolve → extract → quads), including the
//! held-cursor icon overlay ([`cursor_icon_quad`]) — CAPTURE-ONLY since decision 0216 §5, where
//! the held payload's icon became the hardware cursor ([`crate::cursor`]) in a normal run; the
//! quad survives only as the visual harness's machine-checkable view (the OS cursor can't appear
//! in capture pixels). Split out of [`super`] purely for size — the plugin wiring and the input
//! pass live there and in [`super::input`] respectively.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use benilla_ui::script::{QuadContent, TexCoords, UiScript};

use crate::assets::WorldAssets;
use crate::ui_pass::{UiQuad, UiQuads, UvRect};
use crate::ui_text::UiFontAtlas;

mod cooldown;
mod text;
use cooldown::cooldown_quads;

/// `WOW_UI_COST=1` — the untraced per-frame cost meter for this system's phases (the premise
/// instrument for the UI epoch-gate lane, 0730's warm slice): one `[ui-cost]` line per frame with
/// each phase's wall μs, the quad counts, the layout gate's decision, and whether the diff found
/// the produced quads changed. Untraced by design — the campaign grades untraced cpu_ms, and
/// `trace_chrome` inflates exactly the fine-grained spans this measures (0718's calibration).
fn ui_cost_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("WOW_UI_COST").as_deref() == Ok("1"))
}

/// Last frame's extract-gate inputs (decision 0740), held together as one `Local` — the gate
/// compares all four or none, and a Bevy system has a hard param budget this was eating.
#[derive(Default)]
pub(super) struct GateInputs {
    extracted: Vec<benilla_ui::script::ExtractedQuad>,
    text_ui: Option<benilla_ui::script::EditBoxTextUi>,
    dims: Option<(u32, u32, u32)>,
    portraits: std::collections::HashMap<String, crate::portrait::PortraitSource>,
}

/// Per frame: screen size → `tick` (OnUpdate) → `resolve` → `extract` → [`UiQuads`]. Script errors
/// drain to the log (throttled by being drained — each fires once).
#[allow(clippy::too_many_arguments)] // a Bevy system: each param is one resource, the app's convention
pub(super) fn drive_script(
    script: Option<NonSendMut<UiScript>>,
    window: Query<&Window, With<PrimaryWindow>>,
    // REAL time, deliberately: this drives the VM's `GetTime()` session clock, and the reference
    // clock is the OS wall clock. Bevy's default virtual time clamps every frame delta to
    // `max_delta` (250 ms), so hitches, loading stalls, and macOS occlusion throttling (~1 fps
    // when the window is covered) permanently drop UI-clock time — every GetTime-anchored timer
    // (the cooldown sweep, aura expirations, fades) then runs LONG against the wall-clock
    // cooldown store and the server, which is exactly "I can charge before the sweep ends"
    // (verified live: an occluded run's UI clock fell 27 s behind in 43 s of wall time).
    time: Res<Time<Real>>,
    mut ui_clock: ResMut<super::UiClock>,
    mut quads: ResMut<UiQuads>,
    world_assets: Option<ResMut<WorldAssets>>,
    mut images: ResMut<Assets<Image>>,
    mut font_atlas: Option<ResMut<UiFontAtlas>>,
    // The token -> off-screen-baked-face bridge a `SetPortraitTexture`-bound region samples.
    portraits: Res<crate::portrait::PortraitImages>,
    // Gates the held-cursor icon quad (decision 0216 §5): CAPTURE-ONLY, the same presence check
    // every other capture-only system uses (`ui_script::capture_ui_active`'s sibling pattern).
    capture: Option<Res<crate::capture::CaptureMode>>,
    // This frame's `<Minimap>` widget slot, parked for `minimap::emit_minimap` (the UiQuadAppend
    // producer that fills the hole with tile/arrow quads — decision 0203 phase 1).
    mut minimap_widget: ResMut<crate::minimap::MinimapWidget>,
    // Whether the digit-advance feed has run (once per raster environment: on the first frame
    // the atlas exists, and again after every seam-scale change — `last_seam` below re-arms it).
    mut digits_fed: Local<bool>,
    // The seam scale the engine's text-metric caches were answered under. When `s` moves (window
    // resize / fullscreen toggle / uiScale change), every cached measure is stale — see the
    // invalidation below.
    mut last_seam: Local<f32>,
    // The uiScale dial folded into the seam scale (decision 0584).
    ui_scale: Res<super::UiScaleCvar>,
    // ── The extract gate's memory (decision 0740): last frame's conversion inputs ─────────────
    // The conversion loop below is a pure function of (extracted, text_ui, window dims × seam
    // scale, the portrait token map) — the glyph atlas bakes once at Startup and the sprite
    // caches are monotone path→handle, so equal inputs reproduce the same `UiQuads` the diff
    // would then discard. Capture mode never skips (the harness wants exact per-frame output,
    // including the cursor-icon quad's live mouse position).
    mut prev: Local<GateInputs>,
    // This frame's phase split, published for the hover recorder (`hover_log`).
    mut ui_cost: ResMut<crate::hover_log::UiFrameCost>,
) {
    let Some(mut script) = script else {
        return;
    };
    let Ok(window) = window.single() else {
        return;
    };
    let (w, h) = (window.width(), window.height());
    // The 768-virtual UI space (decision 0582 — byte law: the client's FrameXML space is ALWAYS
    // 768 units tall, every aspect, mapped to the window; wow-re ui.md's converter identity
    // `f(screenH) = 768` + the caret `H_px/192` law), times the uiScale dial (decision 0584 —
    // the VM's screen is `768/uiScale` units tall, so a dial below 1 shrinks everything). The VM
    // lives entirely in that space: this seam scales quads ×s on the way out, mouse ÷s on the
    // way in (input.rs), and measures ÷s on the way back. At a 768-tall window with the dial at
    // 1 the whole pipeline is bit-identical to the pre-virtual behavior.
    let s = super::seam_scale(h, ui_scale.0);
    // The raster environment moved (resize / fullscreen / uiScale): every text metric the engine
    // caches — FontString measures, chat row counts, editbox advance tables, the digit feed — was
    // answered under the OLD `s`, and integer-stepped advances measured at one raster size do not
    // rescale to another (the font-size snap alone shifts a string's unit width by several
    // percent — enough that a boot-size measure fails the fullscreen fit test and the ellipsis
    // eats fitting text: the director's "Contr..." rows, reproduced by `WOW_RESIZE`). Declare the
    // staleness at the seam and let every round-trip re-answer under the new `s`.
    if *last_seam != s {
        if *last_seam != 0.0 {
            script.invalidate_text_measures();
            *digits_fed = false;
        }
        *last_seam = s;
    }
    // Phase spans (visible under `bevy/trace_chrome`): this system is the biggest flat CPU cost
    // on an idle frame, and the ledger can only rank what has a name — tick (Lua OnUpdate),
    // resolve (layout), measure (the text round-trips), extract (tree walk + rasterize), diff.
    // The phase marks feed two consumers now: the `[ui-cost]` line and the hover recorder
    // (`hover_log`), which writes the same split per frame to a file. Either one arms them.
    let printing = ui_cost_enabled();
    let cost_on = printing || crate::hover_log::enabled();
    let solves_before = cost_on.then(|| script.layout_solves());
    // The measure counters are PER FRAME: `measure_fontstrings` adds to them, and both publish
    // sites below carry them forward so the two measure passes sum into one frame's row. Nothing
    // else zeroes them, so without this the recorder's "re-shaped strings" column is a lifetime
    // total wearing a per-frame label — it read a flat 219/frame on a glue screen that measures
    // nothing after startup, and cost one wrong theory before the CSV contradicted it.
    ui_cost.measured = 0;
    ui_cost.measured_texts.clear();
    let mut t_mark = cost_on.then(std::time::Instant::now);
    // Marks phase boundaries under the meter: returns μs since the previous mark and re-arms.
    // With the meter off it does nothing (a run carries six no-op calls, not six clock reads).
    let mut lap = move || -> u128 {
        if !cost_on {
            return 0;
        }
        t_mark
            .replace(std::time::Instant::now())
            .map_or(0, |t| t.elapsed().as_micros())
    };
    {
        let _span = bevy::log::info_span!("ui_script: tick").entered();
        script.set_screen_size(w / s, if h > 0.0 { h / s } else { 768.0 });
        script.tick(time.delta_secs());
    }
    // The frame's clock pair ([`super::UiClock`]): the VM value the tick just produced, anchored
    // at the exact `Instant` whose frame-to-frame deltas that clock accumulates — `Time<Real>`'s
    // own last-update. Every Instant→GetTime conversion goes through this pair; sampling
    // `Instant::now()` at a conversion site instead re-measures the tick→feed scheduling gap
    // every frame and wobbles the derived start by that jitter (the resource's own doc).
    *ui_clock = super::UiClock {
        anchor: time.last_update().unwrap_or_else(std::time::Instant::now),
        ui_now: script.now(),
    };
    let us_tick = lap();
    // ── The FontString measure round-trip, BEFORE the resolve ────────────────────────────────
    // `fontstrings_needing_measure` reads only `region_data` — each FontString's text, font, and
    // its explicit/wrap-pinned size — and never a resolved rect; nothing in `resolve` writes a
    // region's size either. So the answers do not need a layout pass to exist, and running them
    // first means the frame's ONE resolve already sees them.
    //
    // Measuring after the resolve (the old order) cost a second FULL solve on every frame whose
    // text changed, because answering a measure moves the anchor solve's read set. On the shipped
    // UI that solve walks 2,164 frames and sweeps 6,297 regions per round — for a hover whose
    // change touches ten FontStrings inside one frame. Measured on the default UI with a tooltip
    // content change per frame (`resolve_bench`): 3.22 ms/frame at 2.00 solves → 2.37 ms at 1.00.
    // The live shape this comes from: a hover frame that solved cost ~10 ms against ~0.2 ms for
    // one that did not (`WOW_HOVER_LOG`, director's run).
    let mut measured_any = false;
    if let Some(atlas) = font_atlas.as_deref_mut() {
        measured_any = measure_fontstrings(&mut script, atlas, s, &mut ui_cost);
    }
    {
        let _span = bevy::log::info_span!("ui_script: resolve").entered();
        script.resolve();
    }
    // The backstop: only when this frame actually measured something can a fresh request exist
    // that the pass above could not have seen. Empty on every frame in the bench — and skipping it
    // otherwise keeps the quiet frame at ONE sweep of the 6,297-region map, not two.
    if measured_any {
        if let Some(atlas) = font_atlas.as_deref_mut() {
            if measure_fontstrings(&mut script, atlas, s, &mut ui_cost) {
                script.resolve();
            }
        }
    }
    let us_resolve = lap();
    let measure_span = bevy::log::info_span!("ui_script: measure").entered();
    // The digit-advance feed (the synchronous half of the money layout's metrics): measure
    // NumberFontNormal's '0'..'9' once per atlas scale and push them as `BENILLA_DIGIT_W`, so
    // `BenillaMoney_Set` can size its coin slots to their numbers *inside* one update — the real
    // SmallMoneyFrame's GetTextWidth-mid-update shrink (MoneyFrame.lua l.202-269), which the
    // frame-late GetStringWidth round-trip below can't serve. The font pairing comes from the
    // registered font object (Fonts.xml stays the single source of truth).
    if let Some(atlas) = font_atlas.as_deref_mut() {
        if !*digits_fed {
            if let Some(number_font) = script.font_object("NumberFontNormal") {
                let adv = crate::ui_text::digit_advances(
                    atlas,
                    crate::ui_text::FontSpec {
                        path: number_font.font.as_deref(),
                        // The one-to-one drawn px (cap inert at NumberFontNormal's 14) × the
                        // virtual scale; the widths divide back to UI units below.
                        height: crate::ui_text::drawn_px(number_font.height, None, s),
                        // The TRUE outline (NumberFontNormal is NORMAL-outlined): it selects the
                        // baked ring cell; only a THICK outline also biases the step law
                        // (`outline-bake-tint.md`) — either way the fed digit widths match what
                        // the render pass actually lays.
                        outline: number_font.outline,
                        paint_halo: true,
                        alpha_gradient: None,
                    },
                );
                let adv = adv.map(|a| a / s);
                script.set_digit_advances(&adv);
                *digits_fed = true;
            }
        }
    }
    // The message-line half of the round-trip: chat ring lines ask for their wrapped ROW COUNT at
    // the frame's resolved width, so the emit pass can stack real content heights (a long line
    // pushes older lines up instead of overlapping them). Same-frame like the FontString half, but
    // no re-resolve — rows shift only the emitted bands, never the anchor graph.
    if let Some(atlas) = font_atlas.as_deref_mut() {
        let requests = script.message_lines_needing_measure();
        if !requests.is_empty() {
            let rows: Vec<(u32, u32, u16, u64)> = requests
                .iter()
                .map(|r| {
                    let n = crate::ui_text::measure_wrapped_rows(
                        atlas,
                        &r.text,
                        // wrap_width is the RESOLVED frame width (scale already in it) — ×s only;
                        // the font height is frame-local — ×s × the frame's scale, the drawn size.
                        r.wrap_width * s,
                        crate::ui_text::FontSpec {
                            path: r.font.as_deref(),
                            // The band's own drawn px (one-to-one regime; inert ≤ 32).
                            height: crate::ui_text::drawn_px(r.height, None, s * r.scale),
                            outline: r.outline, // step-law width bias, as in the measures above
                            paint_halo: true,   // measure never paints; irrelevant here
                            alpha_gradient: None,
                        },
                    );
                    (r.frame, r.index, n, r.key)
                })
                .collect();
            script.set_message_line_rows(&rows);
        }
    }
    drop(measure_span);
    let us_measure = lap();
    for err in script.take_errors() {
        warn!("ui_script: {err}");
    }
    for w in script.take_warnings() {
        warn!("ui_script: {w}");
    }

    let extract_span = bevy::log::info_span!("ui_script: extract").entered();
    let mut out = Vec::new();
    let mut assets = world_assets;
    let mut minimap_slot = None;
    // Hyperlink spans collected while rasterizing message-line Text quads (frame-targeted only —
    // chat lines; FontString-region links are a later arc), fed back to the engine's click
    // hit-test after the loop. Rects flip back to the engine's y-up space.
    let mut link_spans: Vec<(
        benilla_ui::widget::FrameHandle,
        benilla_ui::layout::Rect,
        String,
        String,
    )> = Vec::new();
    // The EditBox advance-table answer (the metrics half of the mouse/selection law): the
    // focused box's display string measured per-byte under the same shaping + step law as its
    // draw, so the engine's click→index, drag-select, and scroll window land exactly on the
    // glyphs. (The request reads the region's load-resolved font; the transient state-font
    // overlay below never applies to an editbox text region.)
    if let Some(atlas) = font_atlas.as_deref_mut() {
        if let Some(req) = script.editbox_advances_request() {
            let spec = crate::ui_text::FontSpec {
                path: req.font.as_deref(),
                // Measured at the DRAWN raster size (seam × the box frame's scale) but divided
                // back by the seam alone: the advances return in screen UI units, the space the
                // engine's ÷s mouse feed and the box's scale-multiplied rect live in (the
                // request's `scale` doc).
                height: crate::ui_text::drawn_px(req.height, None, s * req.scale),
                outline: req.outline,
                paint_halo: true,     // measure never paints
                alpha_gradient: None, // alpha never changes metrics
            };
            let cum: Vec<f32> = crate::ui_text::line_advances(atlas, &req.text, spec)
                .iter()
                .map(|a| a / s)
                .collect();
            // A multiline box also gets its wrapped-row starts + row pitch — the same wrap pass
            // the draw uses, so the engine's (row, x) caret/click law lands on the drawn rows.
            // Advances/pitch return in UI units (÷s): the engine's click→index and row math
            // compare them against the ÷s mouse feed.
            let (rows, cell_h) = match req.wrap_width {
                Some(w) => crate::ui_text::line_rows(atlas, &req.text, w * s, spec),
                None => (vec![0], 0.0),
            };
            script.set_editbox_advances(req.id, req.key, cum, rows, cell_h / s);
        }
    }
    // The focused edit box's text-UI geometry (RF-0082 leaves caret/highlight geometry to the
    // host): which Text quad is the box's, the scroll window to draw, and the caret/selection
    // x-spans within it — advance-derived engine-side (the blink phase too, `0x77a790`'s 0.5 s
    // law in the engine tick), matched in the Text arm below.
    let text_ui = script.focused_editbox_text_ui();
    let _ = lap(); // re-arm: the editbox seam above is not the walk's cost
    let extracted = script.extract();
    let us_exm = lap();
    let n_extracted = extracted.len();
    // ── The extract gate (decision 0740) ──────────────────────────────────────────────────────
    // Every input the conversion below reads, compared against last frame's. Equal inputs make
    // the loop a pure re-derivation of `UiQuads` the diff at the bottom would discard — at the
    // LBRS pin that was every single settled frame, ~0.3 ms/frame of glyph re-rasterization for
    // an identical `Vec`. On a skip, `quads`/`minimap_widget`/the engine's link spans all keep
    // last frame's values, which the equal inputs prove are this frame's values too.
    let dims = (w.to_bits(), h.to_bits(), s.to_bits());
    let settled = capture.is_none()
        && prev.dims == Some(dims)
        && text_ui == prev.text_ui
        && portraits.0 == prev.portraits
        && extracted == prev.extracted;
    if settled {
        drop(extract_span);
        let us_cmp = lap();
        if cost_on {
            let solves = script.layout_solves() - solves_before.unwrap_or(0);
            *ui_cost = crate::hover_log::UiFrameCost {
                measured: ui_cost.measured,
                measured_texts: std::mem::take(&mut ui_cost.measured_texts),
                tick: us_tick,
                resolve: us_resolve,
                measure: us_measure,
                extract: us_exm,
                convert: us_cmp,
                diff: 0,
                quads: quads.quads.len(),
                solves,
                skipped: true,
            };
        }
        if printing {
            let solves = script.layout_solves() - solves_before.unwrap_or(0);
            eprintln!(
                "[ui-cost] tick={us_tick} resolve={us_resolve} measure={us_measure} \
                 exm={us_exm} exa={us_cmp} diff=0 eq={n_extracted} quads={} \
                 solves={solves} changed=0 skip=1",
                quads.quads.len()
            );
        }
        return;
    }
    prev.dims = Some(dims);
    prev.text_ui = text_ui.clone();
    prev.portraits = portraits.0.clone();
    prev.extracted = extracted.clone();
    for eq in extracted {
        let Some(r) = eq.rect else { continue };
        // WoW UI space is y-up from the bottom-left in 768-virtual units; the quad pass is
        // y-down window px from the top-left — scale ×s, then flip through the window height.
        let rect = Rect::new(r.left * s, h - r.top * s, r.right * s, h - r.bottom * s);
        // The ScrollFrame clip (decision 0112), through the same conversion as `rect` —
        // `UiQuad::clip` is the CPU-clip stand-in `ui_pass` already applies uniformly to
        // every quad (texture, backdrop, and glyph alike), so this is the entire app-side plumb.
        let clip = eq
            .clip
            .map(|c| Rect::new(c.left * s, h - c.top * s, c.right * s, h - c.bottom * s));
        match eq.content {
            // Frames draw nothing themselves in v1 (regions carry the visuals).
            QuadContent::Frame => continue,
            // The `<Minimap>` widget's content hole: parked for the minimap renderer (an
            // UiQuadAppend producer), which fills it at this exact z — the widget slot itself
            // emits nothing here (decision 0203 phase 1).
            QuadContent::Minimap { zoom, inside_zoom } => {
                minimap_slot = Some(crate::minimap::MinimapSlot {
                    rect,
                    z: eq.z,
                    zoom,
                    inside_zoom,
                    alpha: eq.alpha,
                });
                continue;
            }
            // The Cooldown widget's pie wipe + finish flash (decision 0137 phase 4) — the
            // byte-pinned look of `UI-Cooldown-Indicator.m2`, rebuilt natively (see
            // [`cooldown_quads`]).
            QuadContent::Cooldown { fraction, flash } => {
                cooldown_quads(
                    rect,
                    eq.z,
                    eq.alpha,
                    fraction,
                    flash,
                    clip,
                    &mut assets,
                    &mut images,
                    &mut out,
                );
                continue;
            }
            QuadContent::Texture {
                path,
                color,
                additive,
                tex_coords,
                circular,
                portrait_unit,
                rotation,
            } => {
                // A live unit portrait (`SetPortraitTexture(region, unit)`): sample this token's
                // source ([`crate::portrait::PortraitImages`]) — the off-screen model bake, or the
                // ref's 2D TemporaryPortrait stand-in while the model streams in. Absent entry (no
                // booth yet) draws nothing rather than the run-splitter's white default.
                if let Some(token) = &portrait_unit {
                    use crate::portrait::PortraitSource;
                    let handle = match portraits.0.get(token) {
                        Some(PortraitSource::Live(h)) => Some(h.clone()),
                        Some(PortraitSource::File(p)) => assets
                            .as_mut()
                            .and_then(|a| a.sprite_texture(p, &mut images)),
                        None => None,
                    };
                    let Some(handle) = handle else {
                        continue;
                    };
                    out.push(UiQuad {
                        rect,
                        z_key: eq.z,
                        texture: Some(handle),
                        // A portrait binding honours `<TexCoords>`/`SetTexCoord` like any other
                        // texture region — the bake is just the sampled image. The ref crops one
                        // this way: the character micro button samples the same portrait slot as
                        // the unit frame through a narrow (0.2..0.8, 0.0666..0.9) window, and
                        // swaps that window when the button is pushed. This branch used to pin
                        // UvRect::FULL, which silently squashed the whole square bake into
                        // whatever rect the region had.
                        uv: match tex_coords {
                            Some(TexCoords::Rect(edges)) => UvRect::from_tex_coords(edges),
                            Some(TexCoords::Corners(corners)) => UvRect::from_corners(corners),
                            None => UvRect::FULL,
                        },
                        color: [1.0, 1.0, 1.0, eq.alpha],
                        // The binding's mask flag: `SetPortraitTexture` regions cut the inscribed
                        // circle (the ref stamps the same shape into its 64² bake's alpha — and
                        // rounds the square stand-in art the same way); `BenillaSetBoothTexture`
                        // (the paper-doll model pane, decision 0208 §5) samples the bake square.
                        circular,
                        clip,
                        ..default()
                    });
                    continue;
                }
                // An unset texture region (no file, no color — e.g. a cleared icon) draws nothing;
                // defaulting it to white would paint phantom quads.
                if path.is_none() && color.is_none() {
                    continue;
                }
                // A portrait region samples the circular-masked variant so the square icon/model
                // doesn't poke past the frame ring's thin band (SetPortraitToTexture, decision 0084).
                let handle = path.as_deref().and_then(|p| {
                    assets.as_mut().and_then(|a| {
                        if circular {
                            a.portrait_texture(p, &mut images)
                        } else {
                            a.sprite_texture(p, &mut images)
                        }
                    })
                });
                // A pathless Texture region is a solid color; a textured one tints by it.
                let mut color = color.unwrap_or([1.0, 1.0, 1.0, 1.0]);
                color[3] *= eq.alpha;
                // `<TexCoords>`/`SetTexCoord` slices the sampled sub-rect. The 4-edge form
                // `[left,right,top,bottom]` maps to raw UV corners (`left→u0, right→u1, top→v0,
                // bottom→v1`) — carried through [`UvRect`] rather than a normalized `Rect` so a
                // mirrored slice (`left>right`, e.g. PlayerFrameTexture) keeps its flip to the
                // vertex buffer. The 8-arg affine form is already per-corner in the `push_quad`
                // winding (the route-line quads). Absent = the full texture.
                let uv = match tex_coords {
                    Some(TexCoords::Rect(edges)) => UvRect::from_tex_coords(edges),
                    Some(TexCoords::Corners(corners)) => UvRect::from_corners(corners),
                    None => UvRect::FULL,
                };
                out.push(UiQuad {
                    rect,
                    z_key: eq.z,
                    texture: handle,
                    uv,
                    color,
                    additive,
                    clip,
                    // The engine's SetRotation is counterclockwise-positive; the quad pass spins
                    // clockwise-on-screen (`UiQuad::rotation`) — negate to convert.
                    rotation: -rotation,
                    ..default()
                });
            }
            QuadContent::Backdrop {
                path,
                color,
                uvs,
                tile,
            } => {
                // A frame Backdrop piece (bg or one of the 8 border pieces). Repeat-sampled when
                // `tile` (edges tile past UV 1; a tiled bg wraps) — its own GPU image + cache
                // (`sprite_texture_tiled`) since clamp/repeat bake into the `Image`. The four UVs are
                // explicit per-corner (the rotated TOP/BOTTOM edges), so they go straight to `UvRect`.
                let handle = assets.as_mut().and_then(|a| {
                    if tile {
                        a.sprite_texture_tiled(&path, &mut images)
                    } else {
                        a.sprite_texture(&path, &mut images)
                    }
                });
                // No texture (missing BLP) ⇒ draw nothing rather than a phantom solid quad.
                let Some(handle) = handle else { continue };
                let mut color = color;
                color[3] *= eq.alpha;
                out.push(UiQuad {
                    rect,
                    z_key: eq.z,
                    texture: Some(handle),
                    uv: UvRect::from_corners(uvs),
                    color,
                    clip,
                    ..default()
                });
            }
            QuadContent::Text {
                text,
                color,
                justify_h,
                justify_v,
                font,
                font_height,
                text_height,
                shadow,
                outline,
                alpha_gradient,
            } => {
                // No atlas (no client data / Friz Quadrata unreadable — see `ui_text`'s startup
                // system) means text simply doesn't render, same graceful-absence posture as a
                // missing `WorldAssets`. The rasterization itself (editbox window, ellipsis,
                // shadow, links, caret) lives in `text::emit`.
                let (Some(atlas), Some(text)) = (font_atlas.as_deref_mut(), text) else {
                    continue;
                };
                text::emit(
                    atlas,
                    &text,
                    text::TextStyle {
                        color,
                        justify_h,
                        justify_v,
                        font,
                        font_height,
                        text_height,
                        shadow,
                        outline,
                        alpha_gradient,
                    },
                    text::TextHost {
                        z: eq.z,
                        alpha: eq.alpha,
                        target: eq.target,
                        rect,
                        clip,
                        ebox: text_ui.as_ref(),
                        screen_h: h,
                        scale: s,
                        font_scale: eq.scale,
                        caret_pinned: capture.is_some(),
                    },
                    &mut out,
                    &mut link_spans,
                );
            }
        }
    }
    // The payload held on the cursor draws last (a 32×32 icon at the mouse, over the whole UI) —
    // but ONLY in capture mode (decision 0216 §5): a normal run shows it as the hardware cursor
    // instead (`crate::cursor`), which can't appear in a screenshot's pixels, so the quad is the
    // capture harness's stand-in. Any arm, matching the hardware cursor's own `payload_icon`
    // (item/spell/action alike — the item-only read here predated the spell/action producers).
    // Purely visual either way — the drag state lives in the engine; the wire settles the move.
    if capture.is_some() {
        use benilla_ui::script::CursorPayload;
        let texture = script.cursor_payload().and_then(|p| match p {
            CursorPayload::Item(i) => i.texture,
            CursorPayload::Spell(s) => s.texture,
            CursorPayload::Action(a) => a.texture,
        });
        if let (Some(texture), Some(pos)) = (texture, window.cursor_position()) {
            if let Some(handle) = assets
                .as_mut()
                .and_then(|a| a.sprite_texture(&texture, &mut images))
            {
                out.push(cursor_icon_quad(pos, handle));
            }
        }
    }

    // The rasterized hyperlink spans replace last frame's set — the engine's release dispatch
    // (`OnHyperlinkClick`) hit-tests against exactly what is on screen.
    script.set_link_spans(link_spans);

    // Park this frame's Minimap widget slot (or clear it — a hidden cluster extracts nothing);
    // `minimap::emit_minimap` runs later in the frame (UiQuadAppend) and fills the hole.
    minimap_widget.0 = minimap_slot;
    drop(extract_span);
    let us_exa = lap();

    let _span = bevy::log::info_span!("ui_script: diff").entered();
    let n_quads = out.len();
    let changed = quads.quads != out;
    if changed {
        quads.quads = out;
        quads.dirty = true;
    }
    if cost_on {
        let us_diff = lap();
        let solves = script.layout_solves() - solves_before.unwrap_or(0);
        *ui_cost = crate::hover_log::UiFrameCost {
            measured: ui_cost.measured,
            measured_texts: std::mem::take(&mut ui_cost.measured_texts),
            tick: us_tick,
            resolve: us_resolve,
            measure: us_measure,
            extract: us_exm,
            convert: us_exa,
            diff: us_diff,
            quads: n_quads,
            solves,
            skipped: false,
        };
        if printing {
            eprintln!(
                "[ui-cost] tick={us_tick} resolve={us_resolve} measure={us_measure} \
                 exm={us_exm} exa={us_exa} diff={us_diff} eq={n_extracted} quads={n_quads} \
                 solves={solves} changed={} skip=0",
                u8::from(changed)
            );
        }
    }
}

/// One pass of the FontString measure round-trip: hand every unmeasured FontString to the font
/// engine and push the answers back. Returns whether anything was measured — the caller's gate for
/// its backstop pass (see the call site for why the order matters).
fn measure_fontstrings(
    script: &mut UiScript,
    atlas: &mut UiFontAtlas,
    s: f32,
    ui_cost: &mut crate::hover_log::UiFrameCost,
) -> bool {
    let requests = script.fontstrings_needing_measure();
    // The recorder's churn column: WHICH strings a frame had to re-shape. A steady hover that
    // keeps asking is the whole question (`hover_log`), and the answer is a string, not a count —
    // so the first few come along by name.
    if crate::hover_log::enabled() {
        ui_cost.measured += requests.len();
        ui_cost.measured_texts.extend(
            requests
                .iter()
                .take(3)
                .map(|r| r.text.chars().take(40).collect::<String>()),
        );
    }
    if requests.is_empty() {
        return false;
    }
    let measures: Vec<(u32, f32, f32, u64)> = requests
        .iter()
        .map(|r| {
            // The full raster scale: seam × the owner frame's effective_scale. Measure at the
            // exact drawn size and divide the whole product back out, so the frame-LOCAL answer
            // times the layout's scale lands on the drawn glyphs to the pixel (integer-stepped
            // advances don't commute with scaling — the request's `scale` doc).
            let rs = s * r.scale;
            let (w, h) = crate::ui_text::measure_text(
                atlas,
                &r.text,
                r.wrap_width.map(|w| w * rs),
                crate::ui_text::FontSpec {
                    path: r.font.as_deref(),
                    // The render pass's exact drawn px (two regimes × the full scale) —
                    // measure == render, and Lua's GetStringWidth/Height echo the DRAWN size
                    // (0x772890); results divide back to frame-local UI units below.
                    height: crate::ui_text::drawn_px(r.height, r.text_height, rs),
                    // The TRUE outline: THICK biases the client's step law (+1px per glyph —
                    // GlyphStepBase 0x5ca2b0, THICK-only per outline-bake-tint.md) and any outline
                    // adds the +2r line pitch, so measure must see it.
                    outline: r.outline,
                    paint_halo: true,     // measure never paints; irrelevant here
                    alpha_gradient: None, // alpha never changes metrics
                },
            );
            (r.id, w / rs, h / rs, r.key)
        })
        .collect();
    script.set_measured_text(&measures);
    true
}

/// The held-cursor icon quad (CAPTURE-ONLY — see the module doc): a 32×32 icon TOP-LEFT anchored
/// at the mouse (`pos`, y-down logical px — the same space as the extracted quad rects and the
/// hardware cursor's own coordinate origin), `[pos, pos + 32]` — matching the hardware cursor's
/// `(0, 0)` hotspot (decision 0216 §5): the pointer sits at the icon's top-left corner and the
/// icon hangs down-right, the reference look, rather than centering on the mouse. Seated above the
/// whole UI (`z_key = u64::MAX` sorts last → drawn on top). Pure geometry so it's
/// machine-checkable without a live mouse.
fn cursor_icon_quad(pos: Vec2, texture: Handle<Image>) -> UiQuad {
    const SIZE: f32 = 32.0;
    UiQuad {
        rect: Rect::new(pos.x, pos.y, pos.x + SIZE, pos.y + SIZE),
        z_key: u64::MAX,
        texture: Some(texture),
        ..default()
    }
}

#[cfg(test)]
mod cursor_quad_tests {
    use super::cursor_icon_quad;
    use bevy::prelude::*;

    #[test]
    fn cursor_icon_quad_is_32px_top_left_anchored_at_the_hotspot() {
        let q = cursor_icon_quad(Vec2::new(100.0, 200.0), Handle::default());
        // 32×32, top-left anchored on the mouse (the hardware cursor's (0,0) hotspot) — hangs
        // down-right: [100..132, 200..232].
        assert_eq!(
            (q.rect.min.x, q.rect.min.y, q.rect.max.x, q.rect.max.y),
            (100.0, 200.0, 132.0, 232.0)
        );
        assert_eq!(q.rect.width(), 32.0);
        assert_eq!(q.rect.height(), 32.0);
        // Above every frame, and textured (straight-alpha, not additive).
        assert_eq!(q.z_key, u64::MAX);
        assert!(q.texture.is_some());
        assert!(!q.additive);
    }
}

/// The app-side half of the ScrollFrame clip plumb (decision 0112): does a [`UiQuad`] built from a
/// clipped [`QuadContent::Texture`] actually carry `clip` through `drive_script`? Drives the real
/// system in a minimal headless `App` (no `DefaultPlugins` — just the resources `drive_script`
/// reads), rather than re-deriving the y-up→y-down flip by hand: that flip is exactly the seam this
/// test exists to catch a regression in.
#[cfg(test)]
mod clip_plumb_tests {
    use bevy::math::Rect;
    use bevy::prelude::*;
    use bevy::window::PrimaryWindow;

    use benilla_ui::script::UiScript;

    use super::{drive_script, UiQuad, UiQuads};
    use crate::portrait::PortraitImages;

    /// A headless app with exactly the resources/entities `drive_script` reads: a `NonSend` VM
    /// carrying a ScrollFrame + scrolled-out child + a colored (pathless) Texture region marker, a
    /// 1024×768 primary window (the 768-virtual design height — s = 1, so the WoW-space rects are
    /// known by hand; decision 0582), and every other resource
    /// left at its default (`WorldAssets`/`UiFontAtlas` absent — no BLP/font pipeline needed to prove
    /// the clip carries through a plain colored quad).
    fn app_with_scrolled_marker() -> App {
        let script = UiScript::new().unwrap();
        script
            .run(
                r#"
            local frame = CreateFrame("ScrollFrame", "SF")
            frame:SetPoint("TOPLEFT", 0, -100)  -- screen top 768 -> frame top 668
            frame:SetSize(300, 200)             -- frame bottom 300, right 300
            local child = CreateFrame("Frame", "Child")
            child:SetSize(300, 600)
            frame:SetScrollChild(child)
            local marker = child:CreateTexture(nil, "ARTWORK")
            marker:SetTexture(1, 0, 0)          -- pathless colored quad: no BLP asset needed
        "#,
            )
            .unwrap();

        let mut app = App::new();
        app.insert_non_send_resource(script);
        app.init_resource::<UiQuads>();
        app.init_resource::<Assets<Image>>();
        app.init_resource::<PortraitImages>();
        app.init_resource::<crate::minimap::MinimapWidget>();
        app.init_resource::<crate::hover_log::UiFrameCost>();
        app.init_resource::<Time>();
        app.init_resource::<Time<Real>>();
        app.init_resource::<crate::ui_script::UiClock>();
        // The uiScale dial at its identity Default (1.0) — tests pin the byte-identity base;
        // the shipped app inserts the taste default instead (decision 0584).
        app.init_resource::<crate::ui_script::UiScaleCvar>();
        app.world_mut().spawn((
            Window {
                resolution: UVec2::new(1024, 768).into(),
                ..default()
            },
            PrimaryWindow,
        ));
        app.add_systems(Update, drive_script);
        app
    }

    #[test]
    fn a_clipped_texture_quad_carries_its_clip_into_the_uiquad() {
        let mut app = app_with_scrolled_marker();
        app.update();

        let quads = &app.world().resource::<UiQuads>().quads;
        let marker: &UiQuad = quads
            .iter()
            .find(|q| q.color[0] == 1.0 && q.color[1] == 0.0 && q.color[2] == 0.0)
            .expect("the colored marker quad extracted");

        // WoW space: frame rect [bottom 468, left 0, top 668, right 300]; the quad pass is y-down
        // from the top-left, so `y_down = window_height - y_up`: bottom(468)->300, top(668)->100.
        assert_eq!(
            marker.clip,
            Some(Rect::new(0.0, 100.0, 300.0, 300.0)),
            "the engine's ScrollFrame clip survives the y-up→y-down flip into UiQuad::clip"
        );
    }

    /// The uiScale dial (decision 0584) through the same plumb, by hand: at dial 0.5 on the same
    /// 1024×768 window, s = 0.5 and the VM sees a 2048×1536 virtual screen — the TOPLEFT-anchored
    /// frame hangs from virtual top 1536, and every window-px rect halves.
    #[test]
    fn the_uiscale_dial_scales_the_extracted_rects() {
        let mut app = app_with_scrolled_marker();
        app.insert_resource(crate::ui_script::UiScaleCvar(0.5));
        app.update();

        let quads = &app.world().resource::<UiQuads>().quads;
        let marker: &UiQuad = quads
            .iter()
            .find(|q| q.color[0] == 1.0 && q.color[1] == 0.0 && q.color[2] == 0.0)
            .expect("the colored marker quad extracted");

        // WoW space: frame top = 1536 − 100 = 1436, bottom 1236, right 300. ×0.5 → px y-up
        // [618..718, 0..150]; y-down through the 768 window: top 768−718 = 50, bottom 768−618 = 150.
        assert_eq!(
            marker.clip,
            Some(Rect::new(0.0, 50.0, 150.0, 150.0)),
            "dial 0.5 halves the px rect and re-hangs the frame from the taller virtual top"
        );
    }

    #[test]
    fn an_unclipped_texture_quad_carries_no_clip() {
        let script = UiScript::new().unwrap();
        script
            .run(
                r#"
            local plain = CreateFrame("Frame", "Plain")
            plain:SetPoint("TOPLEFT", 0, 0)
            plain:SetSize(50, 50)
            local m = plain:CreateTexture(nil, "ARTWORK")
            m:SetTexture(0, 1, 0)
        "#,
            )
            .unwrap();

        let mut app = App::new();
        app.insert_non_send_resource(script);
        app.init_resource::<UiQuads>();
        app.init_resource::<Assets<Image>>();
        app.init_resource::<PortraitImages>();
        app.init_resource::<crate::minimap::MinimapWidget>();
        app.init_resource::<crate::hover_log::UiFrameCost>();
        app.init_resource::<Time>();
        app.init_resource::<Time<Real>>();
        app.init_resource::<crate::ui_script::UiClock>();
        app.init_resource::<crate::ui_script::UiScaleCvar>();
        app.world_mut().spawn((
            Window {
                resolution: UVec2::new(1024, 768).into(),
                ..default()
            },
            PrimaryWindow,
        ));
        app.add_systems(Update, drive_script);
        app.update();

        let quads = &app.world().resource::<UiQuads>().quads;
        let marker = quads
            .iter()
            .find(|q| q.color[0] == 0.0 && q.color[1] == 1.0 && q.color[2] == 0.0)
            .expect("the green marker quad extracted");
        assert_eq!(marker.clip, None);
    }
}

/// The extract gate (decision 0740): a settled frame skips the whole conversion loop, and — the
/// dangerous direction — any extract-visible change must reopen it, INCLUDING paint-only writes
/// that never dirty the layout gate. Uses the same minimal headless app as `clip_plumb_tests`.
#[cfg(test)]
mod extract_gate_tests {
    use bevy::prelude::*;
    use bevy::window::PrimaryWindow;

    use benilla_ui::script::UiScript;

    use super::{drive_script, UiQuads};
    use crate::portrait::PortraitImages;

    fn app_with_marker() -> App {
        let script = UiScript::new().unwrap();
        script
            .run(
                r#"
            local plain = CreateFrame("Frame", "Plain")
            plain:SetPoint("TOPLEFT", 0, 0)
            plain:SetSize(50, 50)
            marker = plain:CreateTexture(nil, "ARTWORK")
            marker:SetTexture(1, 0, 0)
        "#,
            )
            .unwrap();
        let mut app = App::new();
        app.insert_non_send_resource(script);
        app.init_resource::<UiQuads>();
        app.init_resource::<Assets<Image>>();
        app.init_resource::<PortraitImages>();
        app.init_resource::<crate::minimap::MinimapWidget>();
        app.init_resource::<crate::hover_log::UiFrameCost>();
        app.init_resource::<Time>();
        app.init_resource::<Time<Real>>();
        app.init_resource::<crate::ui_script::UiClock>();
        app.init_resource::<crate::ui_script::UiScaleCvar>();
        app.world_mut().spawn((
            Window {
                resolution: UVec2::new(1024, 768).into(),
                ..default()
            },
            PrimaryWindow,
        ));
        app.add_systems(Update, drive_script);
        app
    }

    #[test]
    fn a_settled_frame_skips_and_a_paint_write_reopens_the_gate() {
        let mut app = app_with_marker();
        app.update(); // frame 1: builds the quads
        assert!(app.world().resource::<UiQuads>().dirty, "first frame draws");
        app.world_mut().resource_mut::<UiQuads>().dirty = false;

        app.update(); // frame 2: identical inputs — the gate must leave the resource alone
        assert!(
            !app.world().resource::<UiQuads>().dirty,
            "a settled frame must not re-mark the quads dirty"
        );

        // A PAINT-ONLY write: invisible to the layout gate (no anchors/size moved), so only the
        // extracted-list compare can reopen extraction. If the gate wrongly ate it, the marker
        // would stay red on screen.
        app.world_mut()
            .non_send_resource_mut::<UiScript>()
            .run("marker:SetTexture(0, 0, 1)")
            .unwrap();
        app.update();
        let quads = app.world().resource::<UiQuads>();
        assert!(quads.dirty, "a paint write must reopen the extract gate");
        assert!(
            quads
                .quads
                .iter()
                .any(|q| q.color[0] == 0.0 && q.color[2] == 1.0),
            "and the produced quads carry the new color"
        );
    }

    #[test]
    fn a_layout_write_reopens_the_gate_too() {
        let mut app = app_with_marker();
        app.update();
        app.update();
        app.world_mut().resource_mut::<UiQuads>().dirty = false;

        app.world_mut()
            .non_send_resource_mut::<UiScript>()
            .run("getglobal('Plain'):SetPoint('TOPLEFT', 40, -40)")
            .unwrap();
        app.update();
        assert!(
            app.world().resource::<UiQuads>().dirty,
            "a moved frame must re-extract"
        );
    }
}
