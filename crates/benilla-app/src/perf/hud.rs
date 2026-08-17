//! The standing dev HUD: a collapsed cost pill you can read while playing, and the full readout
//! behind a click.
//!
//! **The collapsed pill covers three timescales, because a regression can arrive on any of them.**
//! *Now* is the cost number. *The last few seconds* is the latched spike badge — a burst is ~250 ms
//! (0610) and the director is looking at the game when it happens, so the evidence has to outlive
//! the event. *The last minute* is the sparkline, which is the only lane that can see cost merely
//! sitting higher than it used to.
//!
//! **What is deliberately no longer the headline: fps.** 0717 established that while synced, wall
//! frame time measures the display's present grant rather than our cost — but it only ever applied
//! that to the expanded lines, and the pill went on reading framerate. On a 120 Hz-adaptive panel
//! that hides a doubling: cost can go 3 → 6 ms with the grant unchanged and the number unmoved, and
//! the old red threshold (fps < 58) sat ~5.7× above a healthy frame's cost. fps stays on the pill,
//! dimmed, as the familiar anchor it is — not as the thing being watched.
//!
//! **Detail lives in the hover tooltip, not in the expanded panel.** Hovering costs zero screen,
//! which is the whole point: the expanded view covers the game's own UI, so anything you might want
//! *while playing* has to be reachable without it.

use bevy::diagnostic::{DiagnosticsStore, EntityCountDiagnosticsPlugin};
use bevy::prelude::*;
use bevy::render::view::Msaa;
use bevy::time::Real;
use bevy::window::{PresentMode, PrimaryWindow};
use bevy_egui::{egui, EguiContexts};

use super::stats::{FrameStats, Series};
use super::FRAME_BUDGET_MS;
use crate::debug_panel::{overlay_text, OVERLAY_FILL, OVERLAY_TEXT, OVERLAY_TEXT_DIM};
use benilla_world::view::WorldCamera;

const RED: egui::Color32 = egui::Color32::from_rgb(240, 120, 120);
const AMBER: egui::Color32 = egui::Color32::from_rgb(240, 190, 110);
const GREEN: egui::Color32 = egui::Color32::from_rgb(140, 220, 140);

/// A latched spike is drawn amber until its peak is this many times its own baseline, then red.
const SPIKE_RED_RATIO: f32 = 3.0;

/// The trend sparkline's footprint on the collapsed pill.
const TREND_SIZE: egui::Vec2 = egui::vec2(72.0, 13.0);

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
}

impl Default for PerfHud {
    fn default() -> Self {
        Self {
            visible: std::env::var("WOW_PERF_HUD").as_deref() != Ok("0"),
            expanded: false,
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
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    cam_msaa: Query<&Msaa, With<WorldCamera>>,
) -> Result {
    if !hud.visible {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;
    let now = time.elapsed_secs();
    let synced = windows
        .single()
        .map(|w| synced_mode(w.present_mode))
        .unwrap_or(true);

    // A title-less, anchored window — minimal chrome (no title bar, no resize) but the stable
    // auto-sizing of a `Window`, so expanding to the full readout doesn't flash a mislaid first
    // frame the way a raw `Area` does.
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
            overlay_text(ui);
            if collapsed_pill(ui, &stats, now).clicked() {
                hud.expanded = !hud.expanded;
            }
            if !hud.expanded {
                return;
            }
            ui.separator();
            expanded_readout(ui, &stats, &diagnostics, synced);
            ui.separator();
            present_controls(ui, &mut windows, &cam_msaa);
        });
    Ok(())
}

/// The collapsed pill: fps (dim), the cost headline, the minute-long trend, and the latched spike.
/// The whole row is one click target.
fn collapsed_pill(ui: &mut egui::Ui, stats: &FrameStats, now: f32) -> egui::Response {
    let spike = stats.spike(now);
    let row = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;

            // fps stays, dimmed: the familiar anchor, and by construction the number that cannot
            // see any of this.
            ui.label(
                egui::RichText::new(format!("{:.0} fps", stats.fps()))
                    .color(OVERLAY_TEXT_DIM)
                    .monospace(),
            );

            // The headline: process CPU per frame, the meter vsync cannot rail (0717).
            match stats.cpu.mean() {
                Some(cpu) => ui.label(
                    egui::RichText::new(format!("{cpu:.1} ms"))
                        .color(OVERLAY_TEXT)
                        .monospace()
                        .strong(),
                ),
                None => ui.label(
                    egui::RichText::new("-- ms")
                        .color(OVERLAY_TEXT_DIM)
                        .monospace(),
                ),
            };

            trend_sparkline(ui, &stats.trend);

            // The latch. Present only when something actually happened, so its mere appearance is
            // the signal — no scanning a number for a change.
            if let Some(s) = spike {
                let col = if s.peak_ms >= s.baseline_ms * SPIKE_RED_RATIO {
                    RED
                } else {
                    AMBER
                };
                ui.label(
                    egui::RichText::new(format!(
                        "▲{:.1} {} ×{}",
                        s.peak_ms,
                        s.kind.tag(),
                        s.frames
                    ))
                    .color(col)
                    .monospace()
                    .strong(),
                );
            }
        })
        .response
        .interact(egui::Sense::click());

    row.on_hover_ui(|ui| pill_tooltip(ui, stats, now))
}

/// The minute-long trend of per-second median CPU cost. Flat means nothing changed; a step means
/// the scene got more (or less) expensive and stayed there — the only lane that can see that, since
/// a sustained cost is its own baseline in every other one.
fn trend_sparkline(ui: &mut egui::Ui, trend: &Series) {
    let (rect, _) = ui.allocate_exact_size(TREND_SIZE, egui::Sense::hover());
    if trend.len() < 2 {
        return;
    }
    let painter = ui.painter_at(rect);
    let hi = trend.iter().fold(f32::MIN, f32::max).max(0.001) * 1.15;
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

/// Everything the pill cannot fit, on hover — the affordance that makes the expanded panel
/// unnecessary while playing.
fn pill_tooltip(ui: &mut egui::Ui, stats: &FrameStats, now: f32) {
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);

    let row = |ui: &mut egui::Ui, name: &str, s: &Series| {
        let (p50, p99, max) = s.percentiles();
        ui.label(
            egui::RichText::new(format!(
                "{name:<5} p50 {p50:>6.2}   p99 {p99:>6.2}   max {max:>6.2}  ms"
            ))
            .monospace(),
        );
    };
    ui.label(
        egui::RichText::new("per-frame cost over the last ~300 frames")
            .color(OVERLAY_TEXT_DIM)
            .monospace(),
    );
    row(ui, "cpu", &stats.cpu);
    row(ui, "main", &stats.main);
    row(ui, "wall", &stats.wall);

    ui.separator();
    let rail = stats.rail_ms();
    let (dropped, frac) = stats.wall.frames_over(stats.dropped_above_ms());
    ui.label(
        egui::RichText::new(format!(
            "interval {rail:.2} ms (observed) · missed {dropped}/{} ({:.0}%)",
            stats.wall.len(),
            frac * 100.0
        ))
        .monospace(),
    );

    if let Some((first, last)) = stats.trend_ends() {
        let delta = last - first;
        ui.label(
            egui::RichText::new(format!(
                "trend {first:.1} → {last:.1} ms ({delta:+.1}) over the last minute"
            ))
            .color(if delta > first * 0.15 {
                AMBER
            } else {
                OVERLAY_TEXT
            })
            .monospace(),
        );
    }

    match stats.spike(now) {
        Some(s) => {
            ui.separator();
            ui.label(
                egui::RichText::new(format!(
                    "spike  {:.2} ms peak over a {:.2} ms baseline, {} frame(s), {:.1}s ago",
                    s.peak_ms,
                    s.baseline_ms,
                    s.frames,
                    (now - s.at).max(0.0)
                ))
                .color(AMBER)
                .monospace(),
            );
            ui.label(egui::RichText::new(s.kind.describe()).color(OVERLAY_TEXT_DIM));
            let bursts = stats.recent_bursts(now);
            if bursts > 1 {
                ui.label(
                    egui::RichText::new(format!(
                        "{bursts} bursts in the last 10 s — the worst is shown"
                    ))
                    .color(OVERLAY_TEXT_DIM),
                );
            }
        }
        None => {
            ui.separator();
            ui.label(egui::RichText::new("no spike in the last 10 s").color(OVERLAY_TEXT_DIM));
        }
    }

    ui.separator();
    ui.label(egui::RichText::new("click to expand · Ctrl+Shift+P to hide").color(OVERLAY_TEXT_DIM));
}

fn expanded_readout(
    ui: &mut egui::Ui,
    stats: &FrameStats,
    diagnostics: &DiagnosticsStore,
    synced: bool,
) {
    let rail = stats.rail_ms();
    // Synced, a small over is rail jitter, not cost — red only past the missed-interval threshold
    // there. Uncapped, wall time is our cost and the 60 fps floor is the honest bar.
    let red_above = if synced {
        stats.dropped_above_ms()
    } else {
        FRAME_BUDGET_MS
    };
    let last = stats.wall.last();
    let last_col = if last > red_above { RED } else { GREEN };
    ui.colored_label(last_col, format!("last {last:>6.2} ms"));

    if synced {
        ui.label(
            egui::RichText::new(format!(
                "present interval {rail:.2} ms observed ({:.0} Hz)",
                if rail > 0.0 { 1000.0 / rail } else { 0.0 }
            ))
            .color(OVERLAY_TEXT_DIM),
        );
    } else {
        ui.label(
            egui::RichText::new(format!("budget {FRAME_BUDGET_MS:.2} ms · 60 fps floor"))
                .color(OVERLAY_TEXT_DIM),
        );
    }

    for (name, series) in [
        ("cpu ", &stats.cpu),
        ("main", &stats.main),
        ("wall", &stats.wall),
    ] {
        let (p50, p99, max) = series.percentiles();
        ui.label(format!(
            "{name} p50 {p50:>6.2}  p99 {p99:>6.2}  max {max:>6.2}  ms"
        ));
    }

    let (over_n, over_frac) = stats.wall.frames_over(red_above);
    let ob_col = if over_frac > 0.0 {
        RED
    } else {
        OVERLAY_TEXT_DIM
    };
    // Synced: "dropped" = missed present intervals — the felt metric; wall time can't say more
    // while it rails at the grant. Uncapped: the honest over-budget count against the 16.7 floor.
    ui.colored_label(
        ob_col,
        format!(
            "{} {over_n}/{}  ({:.0}%)",
            if synced { "dropped" } else { "over budget" },
            stats.wall.len(),
            over_frac * 100.0
        ),
    )
    .on_hover_text(if synced {
        "frames past 1.5x the OBSERVED present interval — a missed display interval. \
         The interval is measured, not assumed at 60 Hz, so this counts a 120 -> 60 drop \
         (which a fixed 25 ms threshold cannot see)"
    } else {
        "frames past the 16.7 ms budget (uncapped: wall time ~= real frame cost)"
    });

    // ~6 ms is the 1.12.1 reference client's whole-scene cost — the long-term bar (0718).
    if let Some(cpu) = stats.cpu.mean() {
        let mean_ms = stats.wall.mean().unwrap_or(0.0);
        let pct = if mean_ms > 0.0 {
            cpu / mean_ms * 100.0
        } else {
            0.0
        };
        ui.label(format!("cpu {cpu:>6.2} ms · {pct:.0}%  (all threads)"))
            .on_hover_text(
                "process CPU consumed per frame — measures our work, not the display's \
                 present grant; comparable with the probes' cpu_ms / a reporter's CPU %",
            );
    }
    if let Some(main) = stats.main.mean() {
        ui.label(format!("main {main:>5.2} ms  (main thread only)"))
            .on_hover_text(
                "CPU consumed by the main thread alone (CLOCK_THREAD_CPUTIME_ID). The \
                 serialized half of the line above: a worker-pool burst inflates `cpu` \
                 without touching this one, and only this one is what a stutter is made of",
            );
    }
    let entities = diagnostics
        .get(&EntityCountDiagnosticsPlugin::ENTITY_COUNT)
        .and_then(|d| d.value())
        .unwrap_or(0.0);
    ui.label(format!("entities {entities:.0}"));

    frame_graph(ui, stats, red_above);
}

/// The frame-time graph: the windowed wall samples as a polyline with the missed-interval line in
/// red. Scaled to at least twice the observed interval so a dropped (doubled) frame reads clearly.
fn frame_graph(ui: &mut egui::Ui, stats: &FrameStats, red_above: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(232.0, 52.0), egui::Sense::hover());
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

fn present_controls(
    ui: &mut egui::Ui,
    windows: &mut Query<&mut Window, With<PrimaryWindow>>,
    cam_msaa: &Query<&Msaa, With<WorldCamera>>,
) {
    if let Ok(mut window) = windows.single_mut() {
        let mut vsync = synced_mode(window.present_mode);
        // "Sync to display", not "cap at 60": on the ProMotion panel the display is ADAPTIVE (up
        // to 120 Hz — grants ~119 in High Power Mode, pinned to 60 only by macOS power state), so
        // synced fps floats with frame cost. A floating 70–119 with this ON is vsync working, not
        // broken (decision 0294's companion finding; measure the grant with WOW_PROBE_VSYNC=1).
        if ui
            .checkbox(
                &mut vsync,
                "VSync — sync to display (120 adaptive; off = uncap)",
            )
            .changed()
        {
            // AutoNoVsync, never Immediate: on Metal, explicit Immediate both rails and takes
            // ~1 s nextDrawable stalls (measured — `capture::probe_uncap_mode`).
            window.present_mode = if vsync {
                PresentMode::AutoVsync
            } else {
                PresentMode::AutoNoVsync
            };
        }
    }
    if let Ok(m) = cam_msaa.single() {
        // Read-only: switching MSAA at runtime breaks our post-process graph (glow/egui) and froze
        // the view, so MSAA is a startup knob now ($WOW_MSAA=off/2/4); A/B by restarting.
        let s = m.samples();
        let txt = if s <= 1 {
            "off".to_string()
        } else {
            format!("{s}×")
        };
        ui.label(format!("MSAA {txt}  ·  set $WOW_MSAA to change"));
    }
}
