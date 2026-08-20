//! The standing dev HUD: a collapsed cost pill you can read while playing, and the full readout
//! behind a click.
//!
//! **The pill is two small numbers and a bare latch arrow** (1448 pared it back). fps dim — the
//! familiar anchor, and by construction the number that cannot see cost (0717) — then process-CPU
//! cost per frame, the meter vsync cannot rail; both under-size, because the pill sits over the
//! game all session. The arrow is the spike latch: a burst is ~250 ms (0610) and the director is
//! looking at the game when it happens, so the evidence has to outlive the event — and its
//! appearance and colour are the *whole* signal (it once printed `▲261.9 main ×94`, a report
//! where an alarm belongs; the numbers wait behind the click). What left the pill entirely: the
//! minute-trend sparkline — a wiggle at 72 px; it is the expanded panel's full-width lane now,
//! beside the line that names its step.
//!
//! **Drawn from a 4 Hz snapshot, not the live meters.** A label whose string changes every frame
//! re-shapes its galley every frame — cost that scales with every widget the surface carries,
//! booked into the very number the pill reports. Between snapshots every string is byte-identical,
//! so egui's galley cache absorbs the text; the meters keep sampling every frame — only the *view*
//! is quantized, and 250 ms is inside a human read of a number. (1448 *bounds* the price: the
//! drawing cost, before and after, sat below the ~0.4 ms noise floor of the interleaved legs —
//! 1370's "0.4–1.2 ms est" reads at its floor, and the diet is cheap insurance plus legibility,
//! not a measured megawin.)
//!
//! **Detail lives behind the click, and nowhere else.** The pill briefly carried a hover tooltip
//! holding the whole readout, on the theory that hovering costs no screen — but the pill is
//! top-center, the mouse crosses it constantly, and a panel-sized popup over the game is worse than
//! the panel it was avoiding. One surface, opened deliberately.
//!
//! **The HUD does not own settings.** It briefly carried a VSync checkbox and an MSAA readout;
//! VSync is a player video option now ([`crate::video`], the Graphics page's `gxVSync` row) and
//! MSAA is a startup knob (`$WOW_MSAA`). An instrument reports — it does not double as the control
//! panel, and this one is `#[cfg(feature = "dev")]`, so anything a player must reach cannot live
//! here at all. The measurement knob the checkbox actually existed for is `$WOW_NOVSYNC=1`.

use bevy::diagnostic::{DiagnosticsStore, EntityCountDiagnosticsPlugin};
use bevy::prelude::*;
use bevy::time::Real;
use bevy::window::{PresentMode, PrimaryWindow};
use bevy_egui::{egui, EguiContexts};

use super::stats::{FrameStats, Series};
use super::FRAME_BUDGET_MS;
use crate::debug_panel::{overlay_text, OVERLAY_FILL, OVERLAY_TEXT, OVERLAY_TEXT_DIM};

const RED: egui::Color32 = egui::Color32::from_rgb(240, 120, 120);
const AMBER: egui::Color32 = egui::Color32::from_rgb(240, 190, 110);
const GREEN: egui::Color32 = egui::Color32::from_rgb(140, 220, 140);

/// A latched spike is drawn amber until its peak is this many times its own baseline, then red.
const SPIKE_RED_RATIO: f32 = 3.0;

/// The trend lane's height in the expanded panel (its width is the panel's).
const TREND_HEIGHT: f32 = 13.0;

/// The pill's number size — under the 12 pt monospace default. The pill sits over the game all
/// session; the director asked for the numbers at a glance-size, not a reading-size (1448).
const PILL_NUM_SIZE: f32 = 10.0;

/// How often the drawn snapshot advances. Fast enough that the numbers read as live and a latched
/// badge appears within a perceptual beat; slow enough that between refreshes every galley is
/// byte-identical and egui's shape cache absorbs the whole text layout (see the module doc).
const HUD_REFRESH_SECS: f32 = 0.25;

/// **The ruler.** The expanded panel is as wide as this string renders in the monospace face —
/// or as wide as the pill above it, whichever is more — and every line inside either fits that or
/// wraps into it. No line is allowed to decide the width by being long.
///
/// It is a *measured* string rather than a pixel constant so the panel stays correct at any font
/// size or UI scale — and it is written at full precision (`000.0`, not `20.2`) so the widest
/// value a field can ever hold still fits. Three tests hold the invariant from both sides: the
/// panel never exceeds its bound, always reaches it, and the ruler (not the pill) is what governs
/// whenever no spike badge is up.
///
/// **Why a governed width at all, twice over.** First, the numbers change width as they change
/// value (`8.55` → `11.73`), so an auto-sized panel jitters under your eye while you read it.
/// Second — and this is what actually shipped broken — auto-sizing here does not merely wobble, it
/// blows up:
///
/// - [`crate::debug_panel::overlay_text`] sets `TextWrapMode::Extend`, so **every label allocates
///   its full natural width and ignores `max_rect` entirely.** One long line therefore *defines*
///   the window width, and `set_width` alone cannot stop it. (The line that shipped broken was
///   `SpikeKind::describe`, ~110 characters; 1448 moved it to hover, but the `Wrap` below stays —
///   it is the fence that stops the *next* long label, not a patch for that one.)
/// - `Area` runs its sizing pass only on the frame the area is *created*
///   (`sizing_pass = state.is_none()`, egui 0.33 `containers/area.rs`). This HUD is born collapsed,
///   so that pass measures the pill; every later expand has no sizing pass, and `Separator` falls
///   through to `available_size_before_wrap()` — the room it *could* take.
///
/// Together those are the ragged panel the director reported: prose setting the width, separators
/// and the graph stretching to whatever that came out as, and the short numeric lines not. The fix
/// is both halves — a ruler, and `TextWrapMode::Wrap` inside the panel so prose wraps instead of
/// measuring.
const PANEL_RULER: &str = "trend 000.0 → 000.0 ms (+000.0/min)";

/// The panel's content width: [`PANEL_RULER`], measured in the live monospace face.
///
/// Measured as *glyph advance × character count* rather than by laying the string out: that is
/// what a monospace column actually is, and it costs one cached glyph lookup instead of a full
/// text layout on a path that runs every frame the panel is open.
fn panel_width(ui: &egui::Ui) -> f32 {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let advance = ui.ctx().fonts_mut(|f| f.glyph_width(&font, '0'));
    advance * PANEL_RULER.chars().count() as f32
}

/// Does this present mode sync to the display? Synced, the HUD's wall-clock numbers measure the
/// present grant, not our cost — the stats lines switch meaning on it.
pub(super) fn synced_mode(mode: PresentMode) -> bool {
    !matches!(
        mode,
        PresentMode::AutoNoVsync | PresentMode::Immediate | PresentMode::Mailbox
    )
}

/// HUD state. The **dev chord + `P`** toggles `visible` (default on — it's a standing dev surface);
/// `expanded` is the click-to-open full readout (default off — the cost pill until clicked).
/// `visible` is `pub(crate)` so the capture harness ([`crate::capture`]) can force the overlay off
/// for pristine, UI-free screenshots.
///
/// **`WOW_PERF_HUD=0` starts it hidden**, which is how the HUD gets priced. 1370 records the open
/// gap: every campaign anchor is measured on a binary that is drawing this overlay, at a cost
/// booked as "est 0.4–1.2 ms CPU + unquantified GPU" — an estimate, never a measurement, because
/// nothing could turn the fixture off without also changing the binary. One env var makes it an
/// interleaved A/B on *one* binary instead (`scripts/leg.sh`), so the constant baked into every
/// anchor becomes a number. The meters keep sampling either way: only the drawing stops, which is
/// the half being priced.
#[derive(Resource)]
pub(crate) struct PerfHud {
    pub(crate) visible: bool,
    /// Full stats shown? Toggled by clicking the pill; the cost pill alone when `false`.
    expanded: bool,
    /// The snapshot the HUD draws from — a copy of the meters taken every [`HUD_REFRESH_SECS`],
    /// so the strings it formats hold still between refreshes (the module doc's cost argument).
    /// The live [`FrameStats`] keeps sampling every frame; this is only the view.
    snap: FrameStats,
    /// The clock `snap` was taken at. Spike ages and hold windows are computed against this, not
    /// the live clock, so they hold still with the rest of the view — and it doubles as the
    /// refresh timer.
    snap_at: f32,
}

impl Default for PerfHud {
    fn default() -> Self {
        Self {
            visible: std::env::var("WOW_PERF_HUD").as_deref() != Ok("0"),
            expanded: false,
            snap: FrameStats::default(),
            // −∞, so the very first frame refreshes rather than drawing an empty snapshot.
            snap_at: f32::NEG_INFINITY,
        }
    }
}

impl PerfHud {
    /// Advance the snapshot if the current one is older than [`HUD_REFRESH_SECS`].
    fn maybe_refresh(&mut self, stats: &FrameStats, now: f32) {
        if now - self.snap_at >= HUD_REFRESH_SECS {
            self.snap = stats.clone();
            self.snap_at = now;
        }
    }
}

pub(super) fn toggle_hud(keys: Res<ButtonInput<KeyCode>>, mut hud: ResMut<PerfHud>) {
    // The dev chord + `P`, not a bare `p` — `P` is the reference's TOGGLESPELLBOOK, and a dev
    // doesn't get to squat on a game binding (decision 0585). The chord can't be mistaken for typed
    // text, so unlike the old bare key it needs no chat-bar/EditBox gate.
    if benilla_world::modkeys::dev_chord(&keys, KeyCode::KeyP) {
        hud.visible = !hud.visible;
    }
}

pub(super) fn perf_hud_ui(
    mut contexts: EguiContexts,
    mut hud: ResMut<PerfHud>,
    stats: Res<FrameStats>,
    time: Res<Time<Real>>,
    diagnostics: Res<DiagnosticsStore>,
    windows: Query<&Window, With<PrimaryWindow>>,
) -> Result {
    if !hud.visible {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;
    let synced = windows
        .single()
        .map(|w| synced_mode(w.present_mode))
        .unwrap_or(true);
    hud.maybe_refresh(&stats, time.elapsed_secs());

    // A title-less, anchored window — minimal chrome (no title bar, no resize) but the stable
    // auto-sizing of a `Window`, so expanding to the full readout doesn't flash a mislaid first
    // frame the way a raw `Area` does.
    let hud = &mut *hud;
    let mut clicked = false;
    egui::Window::new("perf_hud")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .movable(false)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 8.0))
        .frame(
            egui::Frame::NONE
                .inner_margin(egui::Margin::symmetric(8, 5))
                .corner_radius(5.0)
                .fill(OVERLAY_FILL),
        )
        .show(ctx, |ui| {
            clicked = draw_hud(
                ui,
                &hud.snap,
                &diagnostics,
                synced,
                hud.snap_at,
                hud.expanded,
            );
        });
    if clicked {
        hud.expanded = !hud.expanded;
    }
    Ok(())
}

/// Lay the whole HUD out into `ui`; returns whether the toggle strip was clicked.
///
/// Split out of [`perf_hud_ui`] so the layout can be driven headless by a real `egui::Context` in
/// a test — the width invariant this panel kept breaking is a *layout* property, and the only
/// honest way to hold it is to lay it out and measure (see `panel_layout_is_governed_by_the_ruler`).
pub(super) fn draw_hud(
    ui: &mut egui::Ui,
    stats: &FrameStats,
    diagnostics: &DiagnosticsStore,
    synced: bool,
    now: f32,
    expanded: bool,
) -> bool {
    overlay_text(ui);
    let pill = collapsed_pill(ui, stats, now);
    if expanded {
        // The wider of the ruler and the pill above it. The pill is drawn first and `set_width`
        // can only grow an already-allocated `min_rect`, never shrink it — so a pill carrying a
        // spike badge is genuinely wider than the ruler, and pretending otherwise would just move
        // the raggedness rather than fix it. A panel narrower than its own header is not a panel.
        ui.set_width(panel_width(ui).max(pill.width()));
        // The other half, and the one `set_width` alone could not do: `overlay_text` leaves labels
        // on `TextWrapMode::Extend`, where they allocate their natural width and ignore `max_rect`
        // entirely — so prose *measures* the window instead of fitting it (see [`PANEL_RULER`]).
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
        ui.separator();
        expanded_readout(ui, stats, diagnostics, synced, now);
    }
    // The click target is the whole top strip, not the row of glyphs: `min_rect` is now the
    // laid-out content, so when the readout below is wider than the numbers above, the space
    // beside them is still the toggle rather than dead pixels. Registered last, so it sits above
    // the labels it spans.
    let mut strip = pill;
    strip.max.x = strip.max.x.max(ui.min_rect().max.x);
    ui.interact(strip, ui.id().with("pill"), egui::Sense::click())
        .clicked()
}

/// The collapsed pill: fps (dim), the cost headline — both small — and the bare latch arrow.
/// Returns the row's rect; the caller makes the strip that spans it the toggle.
///
/// The arrow carries no numbers: the badge once printed `▲261.9 main ×94` over the game, which
/// is a report, not an alarm (director, 1448). Its appearance and its colour are the whole
/// signal; the numbers wait behind the click.
///
/// **No hover tooltip.** It used to carry the detail so the expanded panel would be unnecessary
/// while playing — but the pill sits top-center, the mouse passes through constantly, and the
/// result was a panel-sized popup covering the game unbidden. Detail lives behind the click now,
/// where it is asked for.
fn collapsed_pill(ui: &mut egui::Ui, stats: &FrameStats, now: f32) -> egui::Rect {
    let spike = stats.spike(now);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;

        // fps, dimmed: the familiar anchor, and by construction the number that cannot see
        // any of this (0717).
        ui.label(
            egui::RichText::new(format!("{:.0} fps", stats.fps()))
                .color(OVERLAY_TEXT_DIM)
                .monospace()
                .size(PILL_NUM_SIZE),
        );

        // The headline: process CPU per frame, the meter vsync cannot rail (0717).
        match stats.cpu.mean() {
            Some(cpu) => ui.label(
                egui::RichText::new(format!("{cpu:.1} ms"))
                    .color(OVERLAY_TEXT)
                    .monospace()
                    .strong()
                    .size(PILL_NUM_SIZE),
            ),
            None => ui.label(
                egui::RichText::new("-- ms")
                    .color(OVERLAY_TEXT_DIM)
                    .monospace()
                    .size(PILL_NUM_SIZE),
            ),
        };

        // The latch. Present only when something actually happened, so its mere appearance is
        // the signal — no scanning a number for a change.
        if let Some(s) = spike {
            let col = if s.peak_ms >= s.baseline_ms * SPIKE_RED_RATIO {
                RED
            } else {
                AMBER
            };
            ui.label(egui::RichText::new("▲").color(col).strong());
        }
    })
    .response
    .rect
}

/// The minute-long trend of per-second median CPU cost, at the expanded panel's full width (1448
/// moved it off the pill, where 72 px showed a wiggle, not a shape). Flat means nothing changed; a
/// step means the scene got more (or less) expensive and stayed there — the only lane that can see
/// that, since a sustained cost is its own baseline in every other one.
fn trend_sparkline(ui: &mut egui::Ui, trend: &Series, hi: f32) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), TREND_HEIGHT),
        egui::Sense::hover(),
    );
    if trend.len() < 2 {
        return;
    }
    let painter = ui.painter_at(rect);
    // Scaled to the window's p90, **not its max** — and the current value always fits, so nothing
    // that matters right now is ever off the top. One launch sample (a loading frame is ~110 ms
    // against a settled 8.5) is enough to press an entire minute of real signal into the bottom 7%
    // of the box, which is what the lane looked like on the director's screen: a flat line. The
    // clamp in `to_y` lets a true outlier clip instead of flattening its neighbours.
    let hi = (hi.max(trend.last())).max(0.001) * 1.15;
    let to_y = |ms: f32| rect.bottom() - (ms / hi).clamp(0.0, 1.0) * rect.height();

    // Where the window started, so a step reads as a departure from something rather than as an
    // anonymous wiggle.
    let start = trend.iter().next().unwrap_or(0.0);
    painter.hline(
        rect.x_range(),
        to_y(start),
        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(90)),
    );

    let dx = rect.width() / (trend.cap().max(2) - 1) as f32;
    let points: Vec<egui::Pos2> = trend
        .iter()
        .enumerate()
        .map(|(i, ms)| egui::pos2(rect.left() + i as f32 * dx, to_y(ms)))
        .collect();
    let col = if trend.last() > start * 1.15 {
        AMBER
    } else {
        GREEN
    };
    painter.add(egui::Shape::line(points, egui::Stroke::new(1.0_f32, col)));
}

/// The full readout, in two answers: **is it running well** (the interval we're being granted and
/// how often we missed it), then **where the time goes** (the percentile table, the trend, the
/// graph, and the latched spike in full).
///
/// It used to be nine lines answering those two questions three times over — a `last` sample the
/// graph below already drew, the p50s of `cpu` and `main` restated underneath as means, and a pair
/// of controls — with a *second* copy of most of it in a hover tooltip that opened whenever the
/// mouse crossed the pill. Everything appears once, here, behind the click.
fn expanded_readout(
    ui: &mut egui::Ui,
    stats: &FrameStats,
    diagnostics: &DiagnosticsStore,
    synced: bool,
    now: f32,
) {
    let rail = stats.rail_ms();
    // Synced, a small over is rail jitter, not cost — red only past the missed-interval threshold
    // there. Uncapped, wall time is our cost and the 60 fps floor is the honest bar.
    let red_above = if synced {
        stats.dropped_above_ms()
    } else {
        FRAME_BUDGET_MS
    };

    // Both header lines are monospace and short enough to sit inside [`PANEL_RULER`]; the prose
    // they used to carry ("observed", "60 fps floor") moved to the hover, which is free.
    if synced {
        ui.label(
            egui::RichText::new(format!(
                "interval {rail:>6.2} ms ({:.0} Hz)",
                if rail > 0.0 { 1000.0 / rail } else { 0.0 }
            ))
            .color(OVERLAY_TEXT_DIM)
            .monospace(),
        )
        .on_hover_text(
            "the present interval as OBSERVED, not assumed — an adaptive panel has no fixed \
             rail (0294), so the only honest answer is what the display has been granting",
        );
    } else {
        ui.label(
            egui::RichText::new(format!("uncapped {FRAME_BUDGET_MS:>6.2} ms budget"))
                .color(OVERLAY_TEXT_DIM)
                .monospace(),
        )
        .on_hover_text("uncapped: wall time is our real frame cost, against the 60 fps floor");
    }

    let (over_n, over_frac) = stats.wall.frames_over(red_above);
    let ob_col = if over_frac > 0.0 {
        RED
    } else {
        OVERLAY_TEXT_DIM
    };
    // Synced: "missed" = missed present intervals — the felt metric; wall time can't say more
    // while it rails at the grant. Uncapped: the honest over-budget count against the 16.7 floor.
    ui.label(
        egui::RichText::new(format!(
            "{:<8} {over_n}/{} ({:.0}%)",
            if synced { "missed" } else { "over" },
            stats.wall.len(),
            over_frac * 100.0
        ))
        .color(ob_col)
        .monospace(),
    )
    .on_hover_text(if synced {
        "frames past 1.5x the OBSERVED present interval — a missed display interval. \
         The interval is measured, not assumed at 60 Hz, so this counts a 120 -> 60 drop \
         (which a fixed 25 ms threshold cannot see)"
    } else {
        "frames past the 16.7 ms budget (uncapped: wall time ~= real frame cost)"
    });

    ui.add_space(4.0);
    // One header instead of the labels repeated on every row — the columns are the same three
    // every time, so naming them once is both shorter and easier to read down.
    ui.label(
        egui::RichText::new(format!("{:<5}{:>8}{:>8}{:>8}", "ms", "p50", "p99", "max"))
            .color(OVERLAY_TEXT_DIM)
            .monospace(),
    );
    for (name, series, why) in [
        (
            "cpu",
            &stats.cpu,
            "process CPU per frame, summed across every thread — our work, not the display's \
             present grant; comparable with the probes' cpu_ms and a reporter's CPU %",
        ),
        (
            "main",
            &stats.main,
            "CPU consumed by the main thread alone (CLOCK_THREAD_CPUTIME_ID) — the serialized \
             half of the line above. A worker-pool burst inflates `cpu` without touching this \
             one, and only this one is what a stutter is made of",
        ),
        (
            "wall",
            &stats.wall,
            "the frame interval itself. While synced this measures the display's grant, not our \
             cost (0717) — read it for missed intervals, not for how expensive we are",
        ),
    ] {
        let (p50, p99, max) = series.percentiles();
        ui.label(
            egui::RichText::new(format!("{name:<5}{p50:>8.2}{p99:>8.2}{max:>8.2}")).monospace(),
        )
        .on_hover_text(why);
    }

    // The creep lane: the sparkline shows the shape, the line under it names the step.
    if let Some((first, last)) = stats.trend_ends() {
        ui.add_space(2.0);
        trend_sparkline(ui, &stats.trend, stats.trend_hi());
        let delta = last - first;
        ui.label(
            egui::RichText::new(format!("trend {first:.1} → {last:.1} ms ({delta:+.1}/min)"))
                // `.monospace()` is load-bearing, not styling: the proportional face has no glyph for
                // U+2192 and drew a tofu box on the director's screen. The mono face has it.
                .monospace()
                .color(if delta > first * 0.15 {
                    AMBER
                } else {
                    OVERLAY_TEXT_DIM
                }),
        );
    }

    frame_graph(ui, stats, red_above);

    let entities = diagnostics
        .get(&EntityCountDiagnosticsPlugin::ENTITY_COUNT)
        .and_then(|d| d.value())
        .unwrap_or(0.0);
    ui.label(
        egui::RichText::new(format!("{:<8} {entities:.0}", "entities"))
            .color(OVERLAY_TEXT_DIM)
            .monospace(),
    );

    // The latch in full — only when one is latched; a quiet panel says nothing about it (1448
    // dropped the "no spike" placeholder and the inline prose). The pill's arrow carries no
    // numbers, so this line is where the spike is actually read: kind, peak against baseline,
    // age. The frame count, the *meaning* of the kind, and the burst count wait on hover — the
    // panel stays numbers; the ~110-character `describe()` used to be the widest and least-read
    // thing on it.
    if let Some(s) = stats.spike(now) {
        let mut why = format!("{} frames — {}", s.frames, s.kind.describe());
        let bursts = stats.recent_bursts(now);
        if bursts > 1 {
            why = format!("{why} — {bursts} bursts in the last 10 s; the worst is shown");
        }
        ui.label(
            egui::RichText::new(format!(
                "spike {} {:.1} over {:.1}, {:.0}s ago",
                s.kind.tag(),
                s.peak_ms,
                s.baseline_ms,
                (now - s.at).max(0.0)
            ))
            .color(AMBER)
            .monospace(),
        )
        .on_hover_text(why);
    }
}

/// The frame-time graph: the windowed wall samples as a polyline with the missed-interval line in
/// red. Scaled to at least twice the observed interval so a dropped (doubled) frame reads clearly.
fn frame_graph(ui: &mut egui::Ui, stats: &FrameStats, red_above: f32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 52.0), egui::Sense::hover());
    if stats.wall.is_empty() {
        return;
    }
    let painter = ui.painter_at(rect);
    let floor = (stats.rail_ms() * 2.0).max(33.0);
    let max_ms = stats.wall.iter().fold(floor, f32::max);
    let to_y = |ms: f32| rect.bottom() - (ms / max_ms).clamp(0.0, 1.0) * rect.height();

    painter.hline(
        rect.x_range(),
        to_y(red_above),
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(200, 80, 80)),
    );

    let dx = rect.width() / stats.wall.cap() as f32;
    let points: Vec<egui::Pos2> = stats
        .wall
        .iter()
        .enumerate()
        .map(|(i, ms)| egui::pos2(rect.left() + i as f32 * dx, to_y(ms)))
        .collect();
    painter.add(egui::Shape::line(points, egui::Stroke::new(1.0_f32, GREEN)));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The panel, laid out by a real `egui::Context`, headless.
    ///
    /// **It must run more than one pass, and that is the whole point.** `Area` runs its sizing
    /// pass only on the frame it is created; the width bug lived exclusively on every *later*
    /// frame, where `Separator` and unwrapped labels reach for room the content never asked for.
    /// A one-pass test would have passed against the broken build.
    ///
    /// Returns `(content width, ruler width)` after the final pass.
    fn lay_out(stats: &FrameStats, now: f32, expanded: bool, passes: usize) -> (f32, f32) {
        let ctx = egui::Context::default();
        let diagnostics = DiagnosticsStore::default();
        let (mut width, mut ruler) = (0.0, 0.0);
        for _ in 0..passes {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::Window::new("perf_hud")
                    .title_bar(false)
                    .resizable(false)
                    .collapsible(false)
                    .movable(false)
                    .show(ctx, |ui| {
                        draw_hud(ui, stats, &diagnostics, true, now, expanded);
                        width = ui.min_rect().width();
                        ruler = panel_width(ui);
                    });
            });
        }
        (width, ruler)
    }

    /// A settled window plus a latched `Stalled` spike — whose `describe()` is ~110 characters,
    /// the longest string the panel can ever draw, and precisely what used to measure the window.
    fn stats_with_a_latched_spike() -> (FrameStats, f32) {
        let mut stats = FrameStats::default();
        let calm: Vec<_> = (0..400).map(|_| (16.6, 20.0, 5.0)).collect();
        let mut t = stats.feed_frames(&calm, 0.0, 1.0 / 60.0);
        t = stats.feed_frames(&[(40.0, 20.0, 5.0)], t, 1.0 / 60.0);
        t = stats.feed_frames(&[(16.6, 20.0, 5.0)], t, 1.0 / 60.0);
        assert!(
            stats.spike(t).is_some(),
            "the fixture must actually latch — otherwise this measures the easy case"
        );
        (stats, t)
    }

    /// **The invariant the panel kept breaking:** the expanded panel is exactly as wide as the
    /// wider of [`PANEL_RULER`] and the pill above it — never wider. Prose wraps, numbers fit, and
    /// no single long line gets to decide the width by being long.
    ///
    /// Three shipped builds widened this panel, the worst to ~5x the ruler, and each was reported
    /// by the director rather than caught here. All three fail this test.
    #[test]
    fn panel_layout_is_governed_by_the_ruler() {
        let (stats, t) = stats_with_a_latched_spike();
        let (pill, _) = lay_out(&stats, t, false, 3);
        let (width, ruler) = lay_out(&stats, t, true, 3);
        let governing = ruler.max(pill);
        assert!(
            width <= governing + 1.0,
            "the expanded panel is {width:.1} px against a {governing:.1} px bound \
             (ruler {ruler:.1}, pill {pill:.1}) — something in it is refusing to wrap \
             (TextWrapMode::Extend) or reaching for available_width"
        );
    }

    /// And it must be genuinely governed, not accidentally narrow: the panel should actually
    /// reach its bound, or the guard above is vacuous.
    #[test]
    fn the_expanded_panel_fills_its_bound() {
        let (stats, t) = stats_with_a_latched_spike();
        let (pill, _) = lay_out(&stats, t, false, 3);
        let (width, ruler) = lay_out(&stats, t, true, 3);
        let governing = ruler.max(pill);
        assert!(
            width >= governing - 1.0,
            "the expanded panel is only {width:.1} px of a {governing:.1} px bound"
        );
    }

    /// With no spike latched the pill is comfortably inside the ruler, so the ruler is what
    /// governs in the ordinary case — the pill only takes over while a badge is up.
    #[test]
    fn the_ruler_governs_when_no_spike_is_latched() {
        let mut stats = FrameStats::default();
        let calm: Vec<_> = (0..400).map(|_| (16.6, 20.0, 5.0)).collect();
        let t = stats.feed_frames(&calm, 0.0, 1.0 / 60.0);
        assert!(stats.spike(t).is_none(), "the fixture must be quiet");

        let (pill, ruler) = lay_out(&stats, t, false, 3);
        assert!(
            pill < ruler,
            "the quiet pill is {pill:.1} px against a {ruler:.1} px ruler — the ruler should \
             govern the ordinary panel, or its width tracks the badge and jitters"
        );
        let (width, _) = lay_out(&stats, t, true, 3);
        assert!(
            (width - ruler).abs() <= 1.0,
            "the quiet panel is {width:.1} px, not the {ruler:.1} px ruler"
        );
    }

    /// The view the HUD draws only advances on the refresh interval — between ticks it holds
    /// still, which is the entire cost argument (identical strings are what the galley cache can
    /// absorb) — and a refresh adopts the live meters wholesale.
    #[test]
    fn the_snapshot_advances_on_the_interval_not_per_frame() {
        let mut live = FrameStats::default();
        let frames: Vec<_> = (0..60).map(|_| (16.6, 20.0, 5.0)).collect();
        let t = live.feed_frames(&frames, 0.0, 1.0 / 60.0);

        let mut hud = PerfHud::default();
        hud.maybe_refresh(&live, t);
        assert_eq!(
            hud.snap.wall.len(),
            60,
            "the first refresh adopts the meters"
        );
        assert_eq!(hud.snap_at, t);

        let t2 = live.feed_frames(&[(40.0, 20.0, 5.0)], t, 1.0 / 60.0);
        hud.maybe_refresh(&live, t2);
        assert_eq!(
            hud.snap.wall.len(),
            60,
            "one frame later — inside the interval — the view holds still"
        );

        hud.maybe_refresh(&live, t + HUD_REFRESH_SECS);
        assert_eq!(
            hud.snap.wall.len(),
            61,
            "past the interval the view catches up"
        );
    }
}
