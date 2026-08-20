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

use crate::ui_pass::{UiQuad, UiQuads, UvRect};
use crate::ui_text::UiFontAtlas;
use benilla_assets::WorldAssets;

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

/// `WOW_UI_GATE=1` — the extract gate's miss reporter ([`report_gate_miss`]). Off by default and
/// read once, the `[ui-cost]` meter's posture.
fn gate_log_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("WOW_UI_GATE").as_deref() == Ok("1"))
}

/// `WOW_UI_DIFF=1` — the base-lane rebuild-trigger probe (`ui_pass`' twin). Read once; it also
/// pins the conversion to the FULL path (the probe's job is naming the first differing quad of a
/// whole-list diff, which the per-entry splice below deliberately never computes).
fn ui_diff_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WOW_UI_DIFF").is_some())
}

/// One line naming why the extract gate did not skip this frame — the input that differed, and for
/// the render list the first entry that moved. A settled UI is what makes the paint pass free; when
/// it is never settled, this says what is moving.
fn report_gate_miss(
    script: &UiScript,
    now: &[benilla_ui::script::ExtractedQuad],
    prev: &[benilla_ui::script::ExtractedQuad],
    dims_eq: bool,
    generation_eq: bool,
    text_ui_eq: bool,
    portraits_eq: bool,
) {
    if !dims_eq {
        eprintln!("[ui-gate] miss: window size / seam scale / DPI changed");
        return;
    }
    if !generation_eq {
        // Named, because it is the one miss with no visible cause at all: the glyph sheet filled
        // and was repacked from empty, so every held quad's UV moved. It should be very rare —
        // `WOW_GLYPH_CACHE=1` reports the occupancy that led here (decision 1342).
        eprintln!("[ui-gate] miss: the glyph sheet reset");
        return;
    }
    if !text_ui_eq {
        eprintln!("[ui-gate] miss: focused editbox text-UI changed");
        return;
    }
    if !portraits_eq {
        eprintln!("[ui-gate] miss: portrait sources changed");
        return;
    }
    if now.len() != prev.len() {
        eprintln!(
            "[ui-gate] miss: render list {} -> {} entries",
            prev.len(),
            now.len()
        );
        return;
    }
    match now.iter().zip(prev).position(|(a, b)| a != b) {
        Some(i) => {
            let (a, b) = (&now[i], &prev[i]);
            let (was, now_s) = (format!("{b:?}"), format!("{a:?}"));
            let owner = match a.target {
                benilla_ui::order::ZTarget::Frame(fh) => script.frame_name(fh),
                benilla_ui::order::ZTarget::Region(_) => None,
            };
            eprintln!(
                "[ui-gate] miss: entry {i}/{} target={:?} name={owner:?}\n           was {}\n           now {}",
                now.len(),
                a.target,
                &was[..was.len().min(280)],
                &now_s[..now_s.len().min(280)],
            );
        }
        // Nothing in this pass's own inputs moved — the miss came from the capture-mode arm.
        None => eprintln!("[ui-gate] miss: capture mode (the gate never skips under a capture)"),
    }
}

/// Last frame's extract-gate inputs (decision 0740), held together as one `Local` — the gate
/// compares all of them or none, and a Bevy system has a hard param budget this was eating.
#[derive(Default)]
pub(super) struct GateInputs {
    extracted: Vec<benilla_ui::script::ExtractedQuad>,
    text_ui: Option<benilla_ui::script::EditBoxTextUi>,
    dims: Option<(u32, u32, u32, u32)>,
    portraits: std::collections::HashMap<String, crate::portrait::PortraitSource>,
    /// The `UiFontAtlas::generation` the held quads' glyph UVs came from — see the gate's own
    /// note below. Moves only when the glyph sheet resets (decisions 1339, 1342).
    generation: Option<u64>,
    /// Per-entry prefix ends into the conversion's output: entry `i`'s quads occupy
    /// `spans[i-1]..spans[i]` of `UiQuads::quads` (0 for `i = 0`). Recorded by the full
    /// conversion, kept current by the splice — this is what lets a one-entry change (the resting
    /// blink) re-convert one entry instead of the whole interface.
    spans: Vec<u32>,
}

/// Entry `i`'s quad range under `spans` (the [`GateInputs::spans`] encoding).
fn span_bounds(spans: &[u32], i: usize) -> (usize, usize) {
    let a = if i == 0 { 0 } else { spans[i - 1] as usize };
    (a, spans[i] as usize)
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
    // Both directions of the booth seam: the token -> off-screen-baked-face bridge a
    // `SetPortraitTexture`-bound region samples, and the pane geometry this pass publishes back
    // for the booths' projection aspect + render gate (decision 1069).
    mut booths: crate::portrait::BoothBridge,
    // Two facts about the RUN, tupled to stay inside Bevy's 16-element system-param limit (the
    // same squeeze `player::control` and `GateInputs` above record):
    // · the held-cursor icon quad (decision 0216 §5) is CAPTURE-ONLY, the same presence check
    //   every other capture-only system uses (`ui_script::capture_ui_active`'s sibling pattern);
    // · whether anything wants this pass's phase split at all ([`super::UiCostWanted`]).
    run: (
        Option<Res<crate::run_mode::CaptureMode>>,
        Res<super::UiCostWanted>,
    ),
    // The append-lane holes this pass parks for their UiQuadAppend producers, tupled for the
    // same param-budget squeeze as `run`:
    // · the `<Minimap>` widget slot (`minimap::emit_minimap` fills it with tile/arrow quads —
    //   decision 0203 phase 1);
    // · the autocast shine sites (`autocast_shine::emit_shine` draws the spark trails there —
    //   decision 1383, B282).
    mut parked: (
        ResMut<crate::minimap::MinimapWidget>,
        ResMut<crate::autocast_shine::ShineSites>,
    ),
    // The seam scale the engine's text-metric caches were answered under. When `s` moves (window
    // resize / fullscreen toggle / uiScale change), every cached measure is stale — see the
    // invalidation below.
    //
    // Deliberately NOT a `VmMemo` (decision 1290's sweep): this is a fact about the RASTER, not
    // about what the VM has been told, and it stays true across the VM's death and rebirth. A fresh
    // VM has no cached measures to invalidate and no measurer at all, and the re-seat below already
    // catches that on its own test (`!script.has_text_measurer()`).
    mut last_seam: Local<f32>,
    // The `scale_factor` this pass last answered measures under — the other half of the same
    // staleness edge (decision 1342). `0.0` until the first frame, which is also a real edge.
    mut last_dpi: Local<f32>,
    // The uiScale dial folded into the seam scale (decision 0584).
    ui_scale: Res<super::UiScaleCvar>,
    // ── The extract gate's memory (decision 0740): last frame's conversion inputs ─────────────
    // The conversion loop below is a pure function of (extracted, text_ui, the RASTER
    // ENVIRONMENT, the portrait token map, the glyph-sheet generation) — the sprite caches are
    // monotone path→handle, so equal inputs reproduce the same `UiQuads` the diff would then
    // discard. Capture mode never skips (the harness wants exact per-frame output, including the
    // cursor-icon quad's live mouse position).
    //
    // The raster environment is window size × seam scale × **`scale_factor`**, and that last term
    // is decision 1342's correction. A monitor hop at an unchanged window size moves nothing else
    // in this list, but it changes the integer device-pixel size every glyph rasterizes at
    // (`TextEngine::ppem`) — so the quads held from last frame are the wrong size and the gate
    // must miss. (1339 caught the same hole through an atlas-bake counter, which was the right
    // fix for a design where the atlas re-baked; there is nothing to re-bake now, so the term that
    // survives is the one that actually moved.)
    //
    // The generation term is the glyph sheet RESETTING — the one event that moves a cached cell's
    // UV, which happens when the sheet fills and is repacked from empty. Held quads carry the old
    // UVs and would draw letters as fragments of other letters.
    //
    // A [`crate::ui_script::VmMemo`] because a skip is not only a quad-conversion skip: it also skips
    // `set_link_spans` and the minimap-slot / booth-pane refills, which are pushes INTO the VM. The
    // VM lives for one login (decision 1290), so a fresh VM must never be gated on what the previous
    // one extracted — its first frame is always a real conversion.
    mut prev: Local<crate::ui_script::VmMemo<GateInputs>>,
    // This frame's phase split, published for whoever asked (the `[ui-cost]` line, `hover_log`).
    mut ui_cost: ResMut<super::UiFrameCost>,
) {
    let (capture, ui_cost_wanted) = run;
    let Some(mut script) = script else {
        // No VM ⇒ no UI is sampling any booth pane. The panes map must not outlive its writer:
        // every OTHER return in this system provably keeps pane presence unchanged (the settled
        // gate compares the whole extracted list; the splice refuses `portrait_unit` entries),
        // but a VM dying mid-world with the character window up would strand its pane here and
        // the paper-doll camera would render behind a dead UI until the next full conversion.
        booths.panes.0.clear();
        return;
    };
    let prev = prev.get(&script);
    let Ok(window) = window.single() else {
        // Same law as the no-VM arm: no window, no sampling — a pane map with no writer lies.
        booths.panes.0.clear();
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
    //
    // **The DPI is the other half of the same edge** (decision 1342). A logical height becomes an
    // integer DEVICE-pixel raster size, so a monitor hop at an unchanged window size leaves `s`
    // exactly where it was and still moves every measured width. (1296/1339 caught this through
    // the atlas's bake generation, which followed the same two terms; with nothing to re-bake, the
    // honest thing to watch is the term itself.)
    let dpi = window.scale_factor();
    let generation = font_atlas.as_deref().map(|a| a.generation);
    let seam_moved = *last_seam != s || *last_dpi != dpi;
    if seam_moved {
        if *last_seam != 0.0 {
            script.invalidate_text_measures();
        }
        *last_seam = s;
        *last_dpi = dpi;
    }
    // …and re-seat the VM's own font engine at the same edge, for the same reason: an
    // [`crate::ui_text::AtlasMeasurer`] answers only for the seam it was built under. This is what
    // makes a `SetText` → `GetStringWidth` pair *inside one Lua update* return a real number
    // instead of 0 — the reference answers that getter inline (`0x79e510` → `0x772890`), and the
    // corpus writes it that way (`Bagnon_Forever/database/ui.lua:58-59`).
    //
    // Seated BEFORE the tick below, so the first update that runs already has it, and rebuilt only
    // on the seam edge or the frame the atlas first exists — an `Arc` clone and an `f32`, never a
    // per-frame cost.
    if let Some(atlas) = font_atlas.as_deref() {
        if seam_moved || !script.has_text_measurer() {
            script.set_text_measurer(Box::new(crate::ui_text::AtlasMeasurer::new(
                atlas.engine(),
                s,
            )));
        }
    }
    // Phase spans (visible under `bevy/trace_chrome`): this system is the biggest flat CPU cost
    // on an idle frame, and the ledger can only rank what has a name — tick (Lua OnUpdate),
    // resolve (layout), measure (the text round-trips), extract (tree walk + rasterize), diff.
    // The phase marks feed two consumers now: the `[ui-cost]` line and the hover recorder
    // (`hover_log`), which writes the same split per frame to a file. Either one arms them.
    let printing = ui_cost_enabled();
    let cost_on = printing || ui_cost_wanted.0;
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
        measured_any = measure_fontstrings(&mut script, atlas, s, &mut ui_cost, ui_cost_wanted.0);
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
            if measure_fontstrings(&mut script, atlas, s, &mut ui_cost, ui_cost_wanted.0) {
                script.resolve();
            }
        }
    }
    let us_resolve = lap();
    let measure_span = bevy::log::info_span!("ui_script: measure").entered();
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
                        &mut atlas.lock(),
                        &r.text,
                        // wrap_width is the RESOLVED frame width (scale already in it) — ×s only;
                        // the font height is frame-local — ×s × the frame's scale, the drawn size.
                        r.wrap_width * s,
                        crate::ui_text::FontSpec {
                            path: r.font.as_deref(),
                            // The band's own drawn px (one-to-one regime; inert ≤ 32).
                            height: crate::ui_text::drawn_px(r.height, None, s * r.scale),
                            outline: r.outline, // step-law width bias, as in the measures above
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
    // The player's half of the error contract before the host's (decision 1305): every caught
    // script error goes to the Lua error handler — `_ERRORMESSAGE` → the ScriptErrors dialog
    // since BasicControls installs it, an addon's own handler if it chose one — and then the
    // same errors drain to the log as always. Dispatch first so a handler that itself fails
    // lands in this frame's drain, not the next one's.
    script.dispatch_script_errors_to_handler();
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
    // This frame's autocast shine sites (decision 1383) — refilled by the loop below like the
    // minimap slot; the settled and spliced paths above keep last conversion's set.
    let mut shine_sites: Vec<crate::autocast_shine::ShineSite> = Vec::new();
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
                alpha_gradient: None, // alpha never changes metrics
            };
            let cum: Vec<f32> = crate::ui_text::line_advances(&mut atlas.lock(), &req.text, spec)
                .iter()
                .map(|a| a / s)
                .collect();
            // A multiline box also gets its wrapped-row starts + row pitch — the same wrap pass
            // the draw uses, so the engine's (row, x) caret/click law lands on the drawn rows.
            // Advances/pitch return in UI units (÷s): the engine's click→index and row math
            // compare them against the ÷s mouse feed.
            let (rows, cell_h) = match req.wrap_width {
                Some(w) => crate::ui_text::line_rows(&mut atlas.lock(), &req.text, w * s, spec),
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
    // an identical `Vec`. On a skip, `quads`/the parked minimap slot and shine sites/the
    // engine's link spans all keep last frame's values, which the equal inputs prove are this
    // frame's values too.
    let dims = (w.to_bits(), h.to_bits(), s.to_bits(), dpi.to_bits());
    let settled = capture.is_none()
        && prev.dims == Some(dims)
        && prev.generation == generation
        && text_ui == prev.text_ui
        && booths.images.0 == prev.portraits
        && extracted == prev.extracted;
    if settled {
        drop(extract_span);
        let us_cmp = lap();
        if cost_on {
            let solves = script.layout_solves() - solves_before.unwrap_or(0);
            *ui_cost = super::UiFrameCost {
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
                spliced: 0,
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
    // `WOW_UI_GATE=1` — why the gate MISSED. A skip is worth the whole conversion, so the
    // question "what moved?" is where a per-frame cost always ends, and a `Vec` inequality does
    // not answer it. Off by default, like the `[ui-cost]` meter beside it.
    if gate_log_enabled() {
        report_gate_miss(
            &script,
            &extracted,
            &prev.extracted,
            prev.dims == Some(dims),
            prev.generation == generation,
            text_ui == prev.text_ui,
            booths.images.0 == prev.portraits,
        );
    }
    // ── The per-entry splice: 1361's shape one layer up ──────────────────────────────────────
    // The gate above is all-or-nothing, so ONE animating entry (the resting blink, a sweeping
    // cooldown) used to re-convert the whole interface every frame. When the only input that
    // moved is the extracted list itself, the lists are index-aligned (same length — the
    // traversal is stable when nothing was created/destroyed/restacked), few entries differ,
    // and every differing entry converts to nothing but quads (no link spans, no minimap slot,
    // no booth panes), the changed entries re-convert alone — through the same [`convert_entry`]
    // body, which is what makes the spliced output equal the full path's by construction — and
    // stitch into last frame's `UiQuads` at the ranges `prev.spans` remembers. Everything else
    // (the side channels, the panes map, the engine's link spans) keeps last frame's values,
    // which the unchanged entries prove are this frame's too: the settled path's own argument,
    // applied per entry.
    'splice: {
        if capture.is_some()
            || ui_diff_enabled()
            || prev.dims != Some(dims)
            || prev.generation != generation
            || text_ui != prev.text_ui
            || booths.images.0 != prev.portraits
            || prev.extracted.len() != extracted.len()
            || prev.spans.len() != extracted.len()
            // spans describe `quads.quads` — if any future writer replaces the base lane out
            // from under this pass, degrade to the full conversion instead of mis-stitching.
            || prev.spans.last().copied().unwrap_or(0) as usize != quads.quads.len()
        {
            break 'splice;
        }
        // The changed entry set, bounded: a real UI transition (a window opening) moves many
        // entries, and the full conversion is the right tool there anyway.
        const SPLICE_MAX: usize = 8;
        let mut changed: Vec<usize> = Vec::with_capacity(SPLICE_MAX);
        for (i, (now, was)) in extracted.iter().zip(&prev.extracted).enumerate() {
            if now != was {
                if changed.len() == SPLICE_MAX {
                    break 'splice;
                }
                changed.push(i);
            }
        }
        // The all-equal case belongs to the settled gate above; reaching here empty means a
        // comparison razor slipped — take the full path rather than risk skipping a change.
        if changed.is_empty() {
            break 'splice;
        }
        // Only entries whose conversion writes quads alone are spliceable — old AND new: an
        // entry morphing into a side-channel kind must reach the full path's plumbing. The
        // autocast-shine token (decision 1383) is a side-channel kind wearing a Texture's
        // clothes, so it is excluded by path.
        let simple = |eq: &benilla_ui::script::ExtractedQuad| match &eq.content {
            QuadContent::Frame | QuadContent::Cooldown { .. } | QuadContent::Backdrop { .. } => {
                true
            }
            QuadContent::Texture {
                portrait_unit: None,
                path,
                ..
            } => !path
                .as_deref()
                .is_some_and(|p| crate::autocast_shine::token_model_scale(p).is_some()),
            _ => false,
        };
        if !changed
            .iter()
            .all(|&i| simple(&extracted[i]) && simple(&prev.extracted[i]))
        {
            break 'splice;
        }
        // Re-convert just the changed entries. The scratch side channels are a tripwire:
        // `simple` makes them unreachable today, and if an arm ever grows a new side effect
        // the full path is the only safe answer.
        let mut scratch: Vec<UiQuad> = Vec::new();
        let mut scratch_ranges: Vec<std::ops::Range<usize>> = Vec::with_capacity(changed.len());
        let mut scratch_links = Vec::new();
        let mut scratch_slot = None;
        let mut scratch_shine = Vec::new();
        for &i in &changed {
            let at = scratch.len();
            convert_entry(
                extracted[i].clone(),
                s,
                w,
                h,
                &mut assets,
                &mut images,
                &mut font_atlas,
                &mut booths,
                text_ui.as_ref(),
                false,
                &mut scratch,
                &mut scratch_links,
                &mut scratch_slot,
                &mut scratch_shine,
            );
            scratch_ranges.push(at..scratch.len());
        }
        if !scratch_links.is_empty() || scratch_slot.is_some() || !scratch_shine.is_empty() {
            break 'splice;
        }
        let counts_stable = changed.iter().zip(&scratch_ranges).all(|(&i, r)| {
            let (a, b) = span_bounds(&prev.spans, i);
            b - a == r.len()
        });
        let mut dirtied = false;
        if counts_stable {
            // The blink's shape — same quad count, new bytes: write the spans in place.
            for (&i, r) in changed.iter().zip(&scratch_ranges) {
                let (a, b) = span_bounds(&prev.spans, i);
                if quads.quads[a..b] != scratch[r.clone()] {
                    quads.quads[a..b].clone_from_slice(&scratch[r.clone()]);
                    dirtied = true;
                }
            }
        } else {
            // An entry's quad count moved: stitch a fresh list from the kept spans plus the
            // re-converted ones, and re-derive the span table.
            let mut new_out: Vec<UiQuad> = Vec::with_capacity(quads.quads.len() + scratch.len());
            let mut new_spans: Vec<u32> = Vec::with_capacity(prev.spans.len());
            let mut ci = 0;
            for i in 0..extracted.len() {
                if ci < changed.len() && changed[ci] == i {
                    new_out.extend_from_slice(&scratch[scratch_ranges[ci].clone()]);
                    ci += 1;
                } else {
                    let (a, b) = span_bounds(&prev.spans, i);
                    new_out.extend_from_slice(&quads.quads[a..b]);
                }
                new_spans.push(new_out.len() as u32);
            }
            prev.spans = new_spans;
            quads.quads = new_out;
            dirtied = true;
        }
        if dirtied {
            quads.dirty = true;
        }
        for &i in &changed {
            prev.extracted[i] = extracted[i].clone();
        }
        drop(extract_span);
        let us_spl = lap();
        if cost_on {
            let solves = script.layout_solves() - solves_before.unwrap_or(0);
            *ui_cost = super::UiFrameCost {
                measured: ui_cost.measured,
                measured_texts: std::mem::take(&mut ui_cost.measured_texts),
                tick: us_tick,
                resolve: us_resolve,
                measure: us_measure,
                extract: us_exm,
                convert: us_spl,
                diff: 0,
                quads: quads.quads.len(),
                solves,
                skipped: false,
                spliced: changed.len(),
            };
        }
        if printing {
            let solves = script.layout_solves() - solves_before.unwrap_or(0);
            eprintln!(
                "[ui-cost] tick={us_tick} resolve={us_resolve} measure={us_measure} \
                 exm={us_exm} exa={us_spl} diff=0 eq={n_extracted} quads={} \
                 solves={solves} changed={} skip=0 spliced={}",
                quads.quads.len(),
                u8::from(dirtied),
                changed.len()
            );
        }
        return;
    }
    prev.dims = Some(dims);
    prev.generation = generation;
    prev.text_ui = text_ui.clone();
    prev.portraits = booths.images.0.clone();
    prev.extracted = extracted.clone();
    // This frame's booth panes, refilled by the loop below (decision 1069). Cleared only HERE, on
    // the un-skipped path: a settled frame draws exactly what the last one did, so the map it left
    // is still this frame's truth — clearing it above the gate would make every quiet frame put the
    // body panes' cameras to sleep and freeze their animation.
    booths.panes.0.clear();
    let mut spans: Vec<u32> = Vec::with_capacity(prev.extracted.len());
    for eq in extracted {
        convert_entry(
            eq,
            s,
            w,
            h,
            &mut assets,
            &mut images,
            &mut font_atlas,
            &mut booths,
            text_ui.as_ref(),
            capture.is_some(),
            &mut out,
            &mut link_spans,
            &mut minimap_slot,
            &mut shine_sites,
        );
        spans.push(out.len() as u32);
    }
    prev.spans = spans;
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
            CursorPayload::Macro(m) => m.texture,
            CursorPayload::PetAction(p) => p.texture,
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

    // Park this frame's Minimap widget slot and autocast shine sites (or clear them — a hidden
    // cluster extracts nothing); `minimap::emit_minimap` / `autocast_shine::emit_shine` run
    // later in the frame (UiQuadAppend) and fill the holes.
    parked.0 .0 = minimap_slot;
    parked.1 .0 = shine_sites;
    drop(extract_span);
    let us_exa = lap();

    let _span = bevy::log::info_span!("ui_script: diff").entered();
    let n_quads = out.len();
    let changed = quads.quads != out;
    if changed {
        // `WOW_UI_DIFF=1` — the base-lane half of the rebuild-trigger probe (`ui_pass`' twin):
        // names the first Lua-UI quad that differs from last frame's extraction.
        if ui_diff_enabled() {
            match quads.quads.iter().zip(&out).position(|(a, b)| a != b) {
                Some(i) => {
                    let (b, a) = (&quads.quads[i], &out[i]);
                    eprintln!(
                        "[ui-diff-base] quad {i}/{n_quads}: tex={:?} rect {:?} -> {:?} uv_changed={} color_changed={}",
                        a.texture.as_ref().and_then(|t| t.path()),
                        b.rect,
                        a.rect,
                        a.uv != b.uv,
                        a.color != b.color,
                    );
                }
                None => eprintln!(
                    "[ui-diff-base] quad COUNT changed: {} -> {}",
                    quads.quads.len(),
                    out.len()
                ),
            }
        }
        quads.quads = out;
        quads.dirty = true;
    }
    if cost_on {
        let us_diff = lap();
        let solves = script.layout_solves() - solves_before.unwrap_or(0);
        *ui_cost = super::UiFrameCost {
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
            spliced: 0,
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

/// One extracted entry converted to its screen quads, pushed onto `out` — with the side channels
/// some arms carry: chat link spans (Text), the minimap widget slot (Minimap), the booth pane
/// aspects (portrait-bound Texture), the autocast shine sites (the token-path Texture, decision
/// 1383). The ONE conversion body: the full pass and the per-entry
/// splice both call this, which is what makes the splice's output equal the full path's by
/// construction. An arm is splice-eligible only if it writes nothing but `out` — keep
/// [`splice_simple`] in agreement when an arm's side effects change.
#[allow(clippy::too_many_arguments)] // the conversion's whole environment, threaded explicitly
fn convert_entry(
    eq: benilla_ui::script::ExtractedQuad,
    s: f32,
    // The window, logical px. `h` flips y-up WoW space into the y-down quad pass; `w` is here for
    // the one producer whose law is in SCREEN pixels rather than FrameXML units — the autocast
    // shine's particle size, which the reference projects by the screen diagonal (decision 1390).
    w: f32,
    h: f32,
    assets: &mut Option<ResMut<WorldAssets>>,
    images: &mut Assets<Image>,
    font_atlas: &mut Option<ResMut<UiFontAtlas>>,
    booths: &mut crate::portrait::BoothBridge,
    text_ui: Option<&benilla_ui::script::EditBoxTextUi>,
    caret_pinned: bool,
    out: &mut Vec<UiQuad>,
    link_spans: &mut Vec<(
        benilla_ui::widget::FrameHandle,
        benilla_ui::layout::Rect,
        String,
        String,
    )>,
    minimap_slot: &mut Option<crate::minimap::MinimapSlot>,
    shine_sites: &mut Vec<crate::autocast_shine::ShineSite>,
) {
    let Some(r) = eq.rect else { return };
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
        QuadContent::Frame => {}
        // The `<Minimap>` widget's content hole: parked for the minimap renderer (an
        // UiQuadAppend producer), which fills it at this exact z — the widget slot itself
        // emits nothing here (decision 0203 phase 1).
        QuadContent::Minimap { zoom, inside_zoom } => {
            *minimap_slot = Some(crate::minimap::MinimapSlot {
                rect,
                z: eq.z,
                zoom,
                inside_zoom,
                alpha: eq.alpha,
            });
        }
        // The Cooldown widget's pie wipe + finish flash (decision 0137 phase 4) — the
        // byte-pinned look of `UI-Cooldown-Indicator.m2`, rebuilt natively (see
        // [`cooldown_quads`]).
        QuadContent::Cooldown { fraction, flash } => {
            cooldown_quads(
                rect, eq.z, eq.alpha, fraction, flash, clip, assets, images, out,
            );
        }
        QuadContent::Texture {
            path,
            color,
            additive,
            tex_coords,
            circular,
            portrait_unit,
            rotation,
            desaturated,
        } => {
            // The autocast-shine token (decision 1383, B282): a shown marker registers WHERE the
            // shine plays — rect, paint order, clip, alpha, and the star art resolved through
            // the same resolver as any texture — and draws nothing itself;
            // `autocast_shine::emit_shine` (UiQuadAppend) animates the sparks there with zero
            // per-frame script-layout traffic. A side-channel arm, so deliberately NOT
            // splice-simple (the `simple` predicate above excludes the token by path).
            if let Some(model_scale) = path
                .as_deref()
                .and_then(crate::autocast_shine::token_model_scale)
            {
                // A no-assets context (the extract test harness; a capture booted without the
                // archive) records the site with the default handle — the site's GEOMETRY is
                // the registration, and the live app always has the resolver.
                let star = assets
                    .as_mut()
                    .and_then(|a| a.sprite_texture(crate::autocast_shine::STAR_TEXTURE, images))
                    .unwrap_or_default();
                shine_sites.push(crate::autocast_shine::ShineSite {
                    rect,
                    z: eq.z,
                    clip,
                    alpha: eq.alpha,
                    scale: s,
                    model_scale,
                    diag: (w * w + h * h).sqrt(),
                    texture: star,
                });
                return;
            }
            // A live unit portrait (`SetPortraitTexture(region, unit)`): sample this token's
            // source ([`crate::portrait::PortraitImages`]) — the off-screen model bake, or the
            // ref's 2D TemporaryPortrait stand-in while the model streams in. Absent entry (no
            // booth yet) draws nothing rather than the run-splitter's white default.
            if let Some(token) = &portrait_unit {
                use crate::portrait::PortraitSource;
                // A **square** binding is a booth pane (`BenillaSetBoothTexture`, decision
                // 0208 §5), not a round unit portrait: publish the rect's aspect so the booth
                // can bake at the shape it will be stretched into, and know it is on screen at
                // all (decision 1069). Recorded before the readiness `continue` below — a pane
                // whose bake hasn't landed yet is still a pane being drawn. The region's rect
                // is the whole answer because no pane crops its bake; a pane that grew
                // `<TexCoords>` would have to fold that UV window in here too.
                if !circular && rect.height() > 0.0 {
                    booths
                        .panes
                        .0
                        .insert(token.clone(), rect.width() / rect.height());
                }
                // The bake is a render target and carries PREMULTIPLIED colour; the 2D stand-in
                // is an ordinary straight-alpha BLP. The quad pass has to be told which, or it
                // premultiplies the bake a second time and erases every effect the pane draws
                // over empty space (see [`crate::ui_pass::UiQuad::premultiplied`]).
                let (handle, premultiplied) = match booths.images.0.get(token) {
                    Some(PortraitSource::Live(h)) => (Some(h.clone()), true),
                    Some(PortraitSource::File(p)) => (
                        assets.as_mut().and_then(|a| a.sprite_texture(p, images)),
                        false,
                    ),
                    None => (None, false),
                };
                let Some(handle) = handle else {
                    return;
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
                    premultiplied,
                    clip,
                    ..default()
                });
                return;
            }
            // An unset texture region (no file, no color — e.g. a cleared icon) draws nothing;
            // defaulting it to white would paint phantom quads.
            if path.is_none() && color.is_none() {
                return;
            }
            // A portrait region samples the circular-masked variant so the square icon/model
            // doesn't poke past the frame ring's thin band (SetPortraitToTexture, decision 0084).
            //
            // A path the archives don't have draws **nothing** — never a white slab. This arm
            // used to fall through to `color.unwrap_or(WHITE)` with a `None` texture, which
            // `ui_pass` renders as the shared 1×1 white image tinted white: an opaque white
            // rectangle at the region's rect, which is how B221's macro icons reached the
            // director's screen. wow-re settles what the reference does: `TextureCreate` does
            // build an 8×8 placeholder, but `CSimpleTexture::SetTexture` (`0x770200`) checks the
            // status severity and at ≥2 releases it and returns **without touching the widget's
            // texture** — the widget keeps what it had, and Lua gets `nil`. Nothing goes white.
            // We can't keep the *previous* art (a path is re-resolved per frame at extract, not
            // latched at `SetTexture`), so the faithful-enough result is an empty cell; what
            // matters is that it is never a phantom quad. The `Backdrop` and live-portrait arms
            // already guard exactly this way.
            //
            // `assets` missing ENTIRELY is a data-less run (the headless UI tests), not a bad
            // path — those keep the old behaviour rather than blanking every textured quad.
            let handle = match (path.as_deref(), assets.as_mut()) {
                (Some(p), Some(a)) => {
                    let resolved = if circular {
                        a.portrait_texture(p, images)
                    } else {
                        a.sprite_texture(p, images)
                    };
                    if resolved.is_none() {
                        return;
                    }
                    resolved
                }
                _ => None,
            };
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
                desaturated,
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
                    a.sprite_texture_tiled(&path, images)
                } else {
                    a.sprite_texture(&path, images)
                }
            });
            // No texture (missing BLP) ⇒ draw nothing rather than a phantom solid quad.
            let Some(handle) = handle else { return };
            // The 8 border pieces share one atlas, so bilinear at a piece's own edge blends in the
            // NEIGHBOURING piece's first column unless the UVs are pulled half a texel inward
            // (1402). `tile` is exactly the border-piece predicate here — every one of the 8 sets
            // it, and a stretched bg (the only `tile: false` piece) is a whole texture, not a
            // slice of one, so it is left alone.
            let uvs = match images.get(&handle) {
                Some(img) if tile => {
                    let sz = img.texture_descriptor.size;
                    benilla_ui::script::inset_atlas_bleed(uvs, sz.width as f32, sz.height as f32)
                }
                _ => uvs,
            };
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
                return;
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
                    ebox: text_ui,
                    screen_h: h,
                    scale: s,
                    font_scale: eq.scale,
                    caret_pinned,
                },
                out,
                link_spans,
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
    ui_cost: &mut super::UiFrameCost,
    // The churn column is the recorder's alone ([`super::UiCostWanted`]) — the `[ui-cost]` line
    // reports counts, not the strings behind them, and collecting them is not free.
    record_texts: bool,
) -> bool {
    let requests = script.fontstrings_needing_measure();
    // The recorder's churn column: WHICH strings a frame had to re-shape. A steady hover that
    // keeps asking is the whole question (`hover_log`), and the answer is a string, not a count —
    // so the first few come along by name.
    if record_texts {
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
    // Every answer goes through the ONE measure body ([`crate::ui_text::measure_request`]) the VM's
    // own synchronous measurer calls, against the same shared engine — so a string measured here
    // and the same string measured mid-tick are not two computations that agree, they are one.
    let measures: Vec<(u32, f32, f32, f32, u64)> = requests
        .iter()
        .map(|r| {
            let (w, h, natural) = crate::ui_text::measure_request(&mut atlas.lock(), s, r);
            (r.id, w, h, natural, r.key)
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
            marker:SetAllPoints()               -- templateless Lua region: no implicit anchor (1310)
        "#,
            )
            .unwrap();

        let mut app = App::new();
        app.insert_non_send_resource(script);
        app.init_resource::<UiQuads>();
        app.init_resource::<Assets<Image>>();
        app.init_resource::<PortraitImages>();
        app.init_resource::<crate::portrait::BoothPanes>();
        app.init_resource::<crate::minimap::MinimapWidget>();
        app.init_resource::<crate::autocast_shine::ShineSites>();
        app.init_resource::<crate::ui_script::UiFrameCost>();
        app.init_resource::<crate::ui_script::UiCostWanted>();
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
            m:SetAllPoints()  -- templateless Lua region: no implicit anchor (1310)
        "#,
            )
            .unwrap();

        let mut app = App::new();
        app.insert_non_send_resource(script);
        app.init_resource::<UiQuads>();
        app.init_resource::<Assets<Image>>();
        app.init_resource::<PortraitImages>();
        app.init_resource::<crate::portrait::BoothPanes>();
        app.init_resource::<crate::minimap::MinimapWidget>();
        app.init_resource::<crate::autocast_shine::ShineSites>();
        app.init_resource::<crate::ui_script::UiFrameCost>();
        app.init_resource::<crate::ui_script::UiCostWanted>();
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
        app_from_script(
            r#"
            local plain = CreateFrame("Frame", "Plain")
            plain:SetPoint("TOPLEFT", 0, 0)
            plain:SetSize(50, 50)
            marker = plain:CreateTexture(nil, "ARTWORK")
            marker:SetTexture(1, 0, 0)
            marker:SetAllPoints()  -- templateless Lua region: no implicit anchor (1310)
        "#,
        )
    }

    /// The same headless app around an arbitrary boot script. The splice tests boot a SECOND
    /// app straight into a mutated app's final state: its first frame is by construction the
    /// full conversion the spliced output must equal.
    fn app_from_script(lua: &str) -> App {
        let script = UiScript::new().unwrap();
        script.run(lua).unwrap();
        let mut app = App::new();
        app.insert_non_send_resource(script);
        app.init_resource::<UiQuads>();
        app.init_resource::<Assets<Image>>();
        app.init_resource::<PortraitImages>();
        app.init_resource::<crate::portrait::BoothPanes>();
        app.init_resource::<crate::minimap::MinimapWidget>();
        app.init_resource::<crate::autocast_shine::ShineSites>();
        app.init_resource::<crate::ui_script::UiFrameCost>();
        app.init_resource::<crate::ui_script::UiCostWanted>();
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

    /// **A booth bake reaches the quad pass flagged PREMULTIPLIED, an ordinary region does not**
    /// (decision 1347). This is the wiring half of the paper-doll/dressing-room fix, and it is the
    /// half that can silently rot: the shader's `select(a, k, premultiplied)` is only correct if
    /// exactly the render-target quads carry the flag. Drop it and every additive effect the pane
    /// draws over EMPTY space is multiplied by its own zero alpha again — the R14 pauldrons' fire
    /// gone, a weapon glow chopped at the model's silhouette (Goudy, `#bugs` 2026-07-27).
    #[test]
    fn a_booth_bake_quad_is_flagged_premultiplied_and_a_plain_one_is_not() {
        let mut app = app_with_marker();
        // The paper doll's own binding — the square booth pane (`BenillaSetBoothTexture`).
        app.world_mut()
            .non_send_resource_mut::<UiScript>()
            .run("BenillaSetBoothTexture(marker, 'paperdoll')")
            .unwrap();
        // The booth publishes a live bake for that slot; without an entry the region draws nothing.
        let bake = app
            .world_mut()
            .resource_mut::<Assets<Image>>()
            .add(Image::default());
        app.world_mut().resource_mut::<PortraitImages>().0.insert(
            "paperdoll".to_string(),
            crate::portrait::PortraitSource::Live(bake.clone()),
        );
        app.update();

        let quads = &app.world().resource::<UiQuads>().quads;
        let pane = quads
            .iter()
            .find(|q| q.texture.as_ref() == Some(&bake))
            .expect("the booth pane's quad reached the pass");
        assert!(
            pane.premultiplied,
            "a render-target bake carries premultiplied colour — the pass must not re-weight it"
        );
        assert!(
            quads
                .iter()
                .filter(|q| q.texture.as_ref() != Some(&bake))
                .all(|q| !q.premultiplied),
            "every straight-alpha region stays unflagged — the flag is the booth's alone"
        );
    }

    /// The per-entry splice, in-place half: a one-entry paint write must ride the splice
    /// (`UiFrameCost::spliced == 1` — nonzero is the proof the path FIRED, without which this
    /// test is vacuous) and produce byte-identically what the full conversion produces —
    /// checked against a fresh app booted straight into the final state.
    #[test]
    fn a_one_entry_paint_write_splices_and_matches_the_full_conversion() {
        let mut app = app_with_marker();
        app.world_mut()
            .resource_mut::<crate::ui_script::UiCostWanted>()
            .0 = true;
        app.update();
        app.world_mut().resource_mut::<UiQuads>().dirty = false;
        app.world_mut()
            .non_send_resource_mut::<UiScript>()
            .run("marker:SetTexture(0, 0, 1)")
            .unwrap();
        app.update();
        assert_eq!(
            app.world()
                .resource::<crate::ui_script::UiFrameCost>()
                .spliced,
            1,
            "the color write is one splice-simple entry"
        );
        assert!(
            app.world().resource::<UiQuads>().dirty,
            "the spliced write re-marks the quads dirty"
        );
        let mut reference = app_from_script(
            r#"
            local plain = CreateFrame("Frame", "Plain")
            plain:SetPoint("TOPLEFT", 0, 0)
            plain:SetSize(50, 50)
            marker = plain:CreateTexture(nil, "ARTWORK")
            marker:SetTexture(0, 0, 1)
            marker:SetAllPoints()  -- templateless Lua region: no implicit anchor (1310)
        "#,
        );
        reference.update();
        assert!(
            app.world().resource::<UiQuads>().quads
                == reference.world().resource::<UiQuads>().quads,
            "spliced output equals the full conversion of the same model"
        );
    }

    /// The autocast-shine token (decision 1383, B282): a shown marker converts to a parked SITE
    /// and zero quads; a spliced frame keeps last conversion's sites; hiding the marker reaches
    /// the full path and clears them. The site's rect/z/scale are the registration — the
    /// producer (`autocast_shine::emit_shine`) draws there with no script traffic at all.
    #[test]
    fn the_shine_token_parks_a_site_and_never_a_quad() {
        let mut app = app_from_script(
            r#"
            local b = CreateFrame("Button", "PetB")
            b:SetPoint("BOTTOMLEFT", 72, 652)
            b:SetSize(30, 30)
            local shine = b:CreateTexture(nil, "OVERLAY")
            shine:SetTexture("benilla:autocast-shine")
            shine:SetAllPoints()
            marker = b:CreateTexture(nil, "ARTWORK")
            marker:SetTexture(1, 0, 0)
            marker:SetAllPoints()
        "#,
        );
        app.update();
        {
            let sites = app.world().resource::<crate::autocast_shine::ShineSites>();
            assert_eq!(sites.0.len(), 1, "one shown token, one site");
            let site = &sites.0[0];
            // 1024x768 window ⇒ the 768-virtual scale is 1.0 and y flips through 768: the
            // button's y-up [652, 682] lands at y-down [86, 116].
            assert_eq!(site.rect, Rect::new(72.0, 86.0, 102.0, 116.0));
            assert_eq!(site.scale, 1.0);
            assert!(site.clip.is_none());
            assert_eq!(site.alpha, 1.0);
            assert_eq!(
                app.world().resource::<UiQuads>().quads.len(),
                1,
                "the red marker's quad alone — the token itself draws nothing"
            );
        }

        // A paint write elsewhere splices — and the parked site survives it untouched.
        app.world_mut()
            .resource_mut::<crate::ui_script::UiCostWanted>()
            .0 = true;
        app.world_mut().resource_mut::<UiQuads>().dirty = false;
        app.world_mut()
            .non_send_resource_mut::<UiScript>()
            .run("marker:SetTexture(0, 0, 1)")
            .unwrap();
        app.update();
        assert_eq!(
            app.world()
                .resource::<crate::ui_script::UiFrameCost>()
                .spliced,
            1,
            "the marker write splices; the token entry did not change"
        );
        assert_eq!(
            app.world()
                .resource::<crate::autocast_shine::ShineSites>()
                .0
                .len(),
            1,
            "a spliced frame keeps last conversion's sites"
        );

        // Hiding the marker changes the extracted list's length — the full path runs and the
        // site set empties: the shine goes out with the region that registered it.
        app.world_mut()
            .non_send_resource_mut::<UiScript>()
            .run("getglobal('PetB'):Hide()")
            .unwrap();
        app.update();
        assert!(
            app.world()
                .resource::<crate::autocast_shine::ShineSites>()
                .0
                .is_empty(),
            "a hidden token registers nothing"
        );
    }

    /// The stitch half: a write that CHANGES an entry's quad count (a cleared texture emits
    /// nothing) re-derives the span table — and a later splice against the re-derived spans
    /// still matches the full conversion, in both directions (1 → 0 quads, then 0 → 1).
    #[test]
    fn a_count_changing_write_stitches_and_keeps_the_spans_true() {
        let mut app = app_with_marker();
        app.world_mut()
            .resource_mut::<crate::ui_script::UiCostWanted>()
            .0 = true;
        app.update();
        let before = app.world().resource::<UiQuads>().quads.len();
        app.world_mut().resource_mut::<UiQuads>().dirty = false;
        app.world_mut()
            .non_send_resource_mut::<UiScript>()
            .run("marker:SetTexture(nil)")
            .unwrap();
        app.update();
        assert_eq!(
            app.world()
                .resource::<crate::ui_script::UiFrameCost>()
                .spliced,
            1,
            "the clear rides the splice (stitch branch)"
        );
        let quads = app.world().resource::<UiQuads>();
        assert!(quads.dirty, "a vanished quad is a real change");
        assert_eq!(
            quads.quads.len(),
            before - 1,
            "the cleared marker's quad is gone from the stitched list"
        );
        // The follow-up (0 → 1 quads) splices against the RE-DERIVED spans.
        app.world_mut().resource_mut::<UiQuads>().dirty = false;
        app.world_mut()
            .non_send_resource_mut::<UiScript>()
            .run("marker:SetTexture(0, 1, 0)")
            .unwrap();
        app.update();
        assert_eq!(
            app.world()
                .resource::<crate::ui_script::UiFrameCost>()
                .spliced,
            1
        );
        let mut reference = app_from_script(
            r#"
            local plain = CreateFrame("Frame", "Plain")
            plain:SetPoint("TOPLEFT", 0, 0)
            plain:SetSize(50, 50)
            marker = plain:CreateTexture(nil, "ARTWORK")
            marker:SetTexture(0, 1, 0)
            marker:SetAllPoints()  -- templateless Lua region: no implicit anchor (1310)
        "#,
        );
        reference.update();
        assert!(
            app.world().resource::<UiQuads>().quads
                == reference.world().resource::<UiQuads>().quads,
            "post-stitch splice output equals the full conversion"
        );
    }

    /// A raster-environment change (window resize) must pin the conversion to the FULL path —
    /// the splice's held quads were rasterized under the old seam and every one of them is
    /// wrong-sized (decision 1342's edge, applied to the splice).
    #[test]
    fn a_resize_takes_the_full_path_not_the_splice() {
        let mut app = app_with_marker();
        app.world_mut()
            .resource_mut::<crate::ui_script::UiCostWanted>()
            .0 = true;
        app.update();
        let win = app
            .world_mut()
            .query_filtered::<Entity, With<PrimaryWindow>>()
            .single(app.world())
            .unwrap();
        app.world_mut().get_mut::<Window>(win).unwrap().resolution = UVec2::new(800, 600).into();
        app.world_mut()
            .non_send_resource_mut::<UiScript>()
            .run("marker:SetTexture(0, 0, 1)")
            .unwrap();
        app.update();
        assert_eq!(
            app.world()
                .resource::<crate::ui_script::UiFrameCost>()
                .spliced,
            0,
            "a moved raster environment forbids splicing"
        );
    }
}
