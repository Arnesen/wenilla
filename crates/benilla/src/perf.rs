//! `perf.rs` — performance instrumentation + the standing dev HUD ([`PerfPlugin`]).
//!
//! Owns the **frame-time measurement layer** that every future subsystem is measured against (the
//! standard). Registers
//! Bevy's diagnostics (frame time / FPS, entity count, render-pass timing), keeps a rolling window of
//! recent real frame durations, and draws a **minimal, always-on FPS pill** (top-center) that you
//! **click to expand** into the full readout; **Ctrl+Cmd+P** toggles the whole HUD. Expanded, it shows:
//! - FPS + last frame ms (red when over budget),
//! - **p50 / p99 / worst** frame ms over the window (worst-frame is the metric that matters, not the
//!   average — averages hide the hitch you feel),
//! - frames-over-budget count/%,
//! - a frame-time graph with the budget line,
//! - a **VSync toggle** — the cheap "uncap and see if we have headroom" CPU-vs-GPU test.
//!
//! The budget is a **60 fps floor = 16.7 ms; no frame should exceed it.** Deep per-system attribution
//! is via Tracy (`cargo run --features tracy`) — the `info_span!` markers on the hot systems feed it.
//!
//! ⚠️ **macOS/Metal:** `RenderDiagnosticsPlugin` records render-pass **CPU** time only — GPU timestamp
//! queries are Vulkan/DX12-only (bevy_render `diagnostic/mod.rs` §Supported platforms). True GPU
//! timing here needs an Xcode/Instruments Metal frame capture; in-client, use the VSync-off headroom
//! test + Tracy for the CPU-vs-GPU picture.

use std::collections::VecDeque;

use bevy::diagnostic::{
    DiagnosticsStore, EntityCountDiagnosticsPlugin, FrameTimeDiagnosticsPlugin,
};
use bevy::prelude::*;
use bevy::render::diagnostic::RenderDiagnosticsPlugin;
use bevy::render::view::Msaa;
use bevy::time::Real;
use bevy::window::{PresentMode, PrimaryWindow};
use bevy_egui::{egui, EguiContexts, EguiPrimaryContextPass};

use crate::debug_panel::{overlay_text, OVERLAY_FILL, OVERLAY_TEXT_DIM};
use crate::player::WorldCamera;

/// The frame budget: a 60 fps floor. No frame should exceed this.
pub const FRAME_BUDGET_MS: f32 = 1000.0 / 60.0;

/// Recent frames kept for the percentiles + the graph (~5 s at 60 fps).
const SAMPLE_WINDOW: usize = 300;

/// Frame duration above which a frame is logged as a hitch (a stall the player feels). Far above the
/// 16.7 ms budget so it only fires on real stalls (load bursts), not normal over-budget frames.
const HITCH_LOG_MS: f32 = 250.0;

pub struct PerfPlugin;

impl Plugin for PerfPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            FrameTimeDiagnosticsPlugin::default(),
            EntityCountDiagnosticsPlugin::default(),
            // Render-pass timing (CPU-only on Metal — see the module header). Harmless if the backend
            // can't record it; also the hook Tracy GPU uses on Vulkan/DX12.
            RenderDiagnosticsPlugin,
        ))
        .init_resource::<FrameStats>()
        .init_resource::<PerfHud>()
        .add_systems(
            Update,
            // `toggle_hud` needs no ordering against the UI keyboard feed any more: its Ctrl+Cmd+P
            // chord can't be typed text, so there's no `UiKeyboardCapture` to read (decision 0585).
            (toggle_hud, sample_frame_time),
        )
        .add_systems(EguiPrimaryContextPass, perf_hud_ui);
    }
}

/// Rolling window of recent **real** frame durations (ms) + the derived hitch metrics.
#[derive(Resource)]
struct FrameStats {
    samples: VecDeque<f32>,
}

impl Default for FrameStats {
    fn default() -> Self {
        Self {
            samples: VecDeque::with_capacity(SAMPLE_WINDOW),
        }
    }
}

impl FrameStats {
    fn push(&mut self, ms: f32) {
        if self.samples.len() == SAMPLE_WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(ms);
    }

    fn last(&self) -> f32 {
        self.samples.back().copied().unwrap_or(0.0)
    }

    /// `(p50, p99, max)` over the window. Sorts a copy — fine for a few-hundred-element dev HUD.
    fn percentiles(&self) -> (f32, f32, f32) {
        if self.samples.is_empty() {
            return (0.0, 0.0, 0.0);
        }
        let mut v: Vec<f32> = self.samples.iter().copied().collect();
        v.sort_by(f32::total_cmp);
        let at = |q: f32| v[(((v.len() - 1) as f32) * q).round() as usize];
        (at(0.50), at(0.99), *v.last().unwrap())
    }

    fn fps(&self) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let mean = self.samples.iter().sum::<f32>() / self.samples.len() as f32;
        if mean > 0.0 {
            1000.0 / mean
        } else {
            0.0
        }
    }

    /// `(count, fraction)` of windowed frames that blew the budget.
    fn over_budget(&self) -> (usize, f32) {
        let n = self
            .samples
            .iter()
            .filter(|&&ms| ms > FRAME_BUDGET_MS)
            .count();
        let frac = if self.samples.is_empty() {
            0.0
        } else {
            n as f32 / self.samples.len() as f32
        };
        (n, frac)
    }
}

/// HUD state. **Ctrl+Cmd+P** toggles `visible` (default on — it's a standing dev surface); `expanded` is the
/// click-to-open full readout (default off — a minimal FPS pill until clicked). `visible` is
/// `pub(crate)` so the capture harness ([`crate::capture`]) can force the overlay off for pristine,
/// UI-free screenshots.
#[derive(Resource)]
pub(crate) struct PerfHud {
    pub(crate) visible: bool,
    /// Full stats shown? Toggled by clicking the FPS pill; minimal (FPS only) when `false`.
    expanded: bool,
}

impl Default for PerfHud {
    fn default() -> Self {
        Self {
            visible: true,
            expanded: false,
        }
    }
}

fn toggle_hud(keys: Res<ButtonInput<KeyCode>>, mut hud: ResMut<PerfHud>) {
    // Ctrl+Cmd+P, not a bare `p` — `P` is the reference's TOGGLESPELLBOOK, and a dev instrument
    // doesn't get to squat on a game binding (decision 0585). The chord can't be mistaken for typed
    // text, so unlike the old bare key it needs no chat-bar/EditBox gate.
    if crate::debug_panel::dev_chord(&keys, KeyCode::KeyP) {
        hud.visible = !hud.visible;
    }
}

fn sample_frame_time(time: Res<Time<Real>>, mut stats: ResMut<FrameStats>) {
    let ms = time.delta_secs() * 1000.0;
    stats.push(ms);
    // Log hard hitches so a load freeze is attributable from the log alone (one big stall vs many
    // medium ones, and roughly when). The first frame's delta is the startup gap, not a hitch.
    if ms > HITCH_LOG_MS && stats.samples.len() > 1 {
        warn!("frame hitch: {ms:.0} ms (main thread blocked this long)");
    }
}

fn perf_hud_ui(
    mut contexts: EguiContexts,
    mut hud: ResMut<PerfHud>,
    stats: Res<FrameStats>,
    diagnostics: Res<DiagnosticsStore>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    cam_msaa: Query<&Msaa, With<WorldCamera>>,
) -> Result {
    if !hud.visible {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;

    let fps = stats.fps();
    let last = stats.last();

    let red = egui::Color32::from_rgb(240, 120, 120);
    let green = egui::Color32::from_rgb(140, 220, 140);
    // The pill colour reads *sustained* health off the windowed mean, not the per-frame `last` (which
    // would flicker red on every isolated spike): green at/above the 60 fps floor, red when under it.
    let pill = if fps < 58.0 { red } else { green };

    // A title-less, anchored window — minimal chrome (no title bar, no resize) but the stable
    // auto-sizing of a `Window`, so expanding to the full readout doesn't flash a mislaid first frame
    // the way a raw `Area` does. `overlay_text` brightens the text and stops "60 fps" from wrapping.
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
            // Minimal by default: just a clickable FPS number. Click toggles the full readout.
            let label = egui::RichText::new(format!("{fps:.0} fps"))
                .color(pill)
                .monospace()
                .strong();
            let hint = if hud.expanded {
                "click to collapse · P to hide"
            } else {
                "click to expand · P to hide"
            };
            if ui
                .add(egui::Button::new(label).frame(false))
                .on_hover_text(hint)
                .clicked()
            {
                hud.expanded = !hud.expanded;
            }
            if !hud.expanded {
                return;
            }

            ui.separator();
            let (p50, p99, max) = stats.percentiles();
            let (over_n, over_frac) = stats.over_budget();
            let win_len = stats.samples.len();
            let entities = diagnostics
                .get(&EntityCountDiagnosticsPlugin::ENTITY_COUNT)
                .and_then(|d| d.value())
                .unwrap_or(0.0);

            let last_col = if last > FRAME_BUDGET_MS { red } else { green };
            ui.colored_label(last_col, format!("last {last:>6.2} ms"));
            ui.label(
                egui::RichText::new(format!("budget {FRAME_BUDGET_MS:.2} ms · 60 fps floor"))
                    .color(OVERLAY_TEXT_DIM),
            );
            ui.label(format!(
                "p50 {p50:>6.2}   p99 {p99:>6.2}   max {max:>6.2}  ms"
            ));
            let ob_col = if over_frac > 0.0 {
                red
            } else {
                OVERLAY_TEXT_DIM
            };
            ui.colored_label(
                ob_col,
                format!(
                    "over budget {over_n}/{win_len}  ({:.0}%)",
                    over_frac * 100.0
                ),
            );
            ui.label(format!("entities {entities:.0}"));

            frame_graph(ui, &stats);

            if let Ok(mut window) = windows.single_mut() {
                ui.separator();
                let mut vsync = !matches!(
                    window.present_mode,
                    PresentMode::AutoNoVsync | PresentMode::Immediate | PresentMode::Mailbox
                );
                // "Sync to display", not "cap at 60": on the ProMotion panel the display is
                // ADAPTIVE (up to 120 Hz — grants ~119 in High Power Mode, pinned to 60 only by
                // macOS power state), so synced fps floats with frame cost. A floating 70–119
                // with this ON is vsync working, not broken (decision 0294's companion finding;
                // measure the current grant with WOW_PROBE_VSYNC=1).
                if ui
                    .checkbox(
                        &mut vsync,
                        "VSync — sync to display (120 adaptive; off = uncap)",
                    )
                    .changed()
                {
                    window.present_mode = if vsync {
                        PresentMode::AutoVsync
                    } else {
                        PresentMode::AutoNoVsync
                    };
                }
            }
            if let Ok(m) = cam_msaa.single() {
                // Read-only: switching MSAA at runtime breaks our post-process graph (glow/egui)
                // and froze the view, so MSAA is a startup knob now ($WOW_MSAA=off/2/4); A/B by
                // restarting.
                let s = m.samples();
                let txt = if s <= 1 {
                    "off".to_string()
                } else {
                    format!("{s}×")
                };
                ui.label(format!("MSAA {txt}  ·  set $WOW_MSAA to change"));
            }
        });
    Ok(())
}

/// A tiny frame-time graph: the windowed samples as a polyline with the budget line in red. Scaled to
/// at least 33 ms so a dropped (doubled) frame reads clearly against the 16.7 ms budget.
fn frame_graph(ui: &mut egui::Ui, stats: &FrameStats) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(232.0, 52.0), egui::Sense::hover());
    if stats.samples.is_empty() {
        return;
    }
    let painter = ui.painter_at(rect);
    let max_ms = stats.samples.iter().copied().fold(33.0_f32, f32::max);
    let to_y = |ms: f32| rect.bottom() - (ms / max_ms).clamp(0.0, 1.0) * rect.height();

    let budget_y = to_y(FRAME_BUDGET_MS);
    painter.hline(
        rect.x_range(),
        budget_y,
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(200, 80, 80)),
    );

    let dx = rect.width() / SAMPLE_WINDOW as f32;
    let points: Vec<egui::Pos2> = stats
        .samples
        .iter()
        .enumerate()
        .map(|(i, &ms)| egui::pos2(rect.left() + i as f32 * dx, to_y(ms)))
        .collect();
    painter.add(egui::Shape::line(
        points,
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(140, 220, 140)),
    ));
}

/// The FPS JOURNAL (`WOW_FPS_JOURNAL=<csv path>`): once a second, append
/// `t, x, y, z, mean_ms, p95_ms, streamed, entities` (player position in raw WoW coords) — the
/// "where does it dip" instrument for a director-driven run. They play normally; the journal
/// turns "it drops in the Dwarven District" into coordinates a headless probe can tele straight
/// back to. Negligible cost: one line of IO a second, samples reused from [`FrameStats`].
pub(crate) struct FpsJournalPlugin;

impl Plugin for FpsJournalPlugin {
    fn build(&self, app: &mut App) {
        let path = std::env::var("WOW_FPS_JOURNAL").unwrap_or_default();
        app.insert_resource(FpsJournal {
            path,
            window: Vec::new(),
            last_flush: 0.0,
        })
        .add_systems(Update, journal_fps);
    }
}

#[derive(Resource)]
struct FpsJournal {
    path: String,
    window: Vec<f32>,
    last_flush: f32,
}

fn journal_fps(
    mut journal: ResMut<FpsJournal>,
    time: Res<Time<Real>>,
    player: Option<Res<crate::player::Player>>,
    streamed: Query<(), With<crate::net::NetEntity>>,
    entities: Query<()>,
) {
    let now = time.elapsed_secs();
    journal.window.push(time.delta_secs() * 1000.0);
    if now - journal.last_flush < 1.0 {
        return;
    }
    journal.last_flush = now;
    let mut v = std::mem::take(&mut journal.window);
    if v.is_empty() {
        return;
    }
    v.sort_by(f32::total_cmp);
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    let p95 = v[((v.len() - 1) as f32 * 0.95).round() as usize];
    // Raw WoW coords, so the line pastes straight into a `.go xyz` probe.
    let pos = player
        .filter(|p| p.active)
        .map(|p| benilla_assets::coords::bevy_to_wow(p.pos))
        .unwrap_or([0.0; 3]);
    let line = format!(
        "{now:.1},{:.1},{:.1},{:.1},{mean:.2},{p95:.2},{},{}\n",
        pos[0],
        pos[1],
        pos[2],
        streamed.iter().len(),
        entities.iter().len()
    );
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&journal.path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}
