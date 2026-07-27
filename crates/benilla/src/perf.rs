//! `perf.rs` — performance instrumentation + the standing dev HUD ([`PerfPlugin`]).
//!
//! Owns the **frame-time measurement layer** that every future subsystem is measured against (the
//! standard). Registers
//! Bevy's diagnostics (frame time / FPS, entity count, render-pass timing), keeps a rolling window of
//! recent real frame durations, and draws a **minimal, always-on FPS pill** (top-center) that you
//! **click to expand** into the full readout; **Ctrl+Cmd+P** toggles the whole HUD. Expanded, it shows:
//! - FPS + last frame ms,
//! - **p50 / p99 / worst** frame ms over the window (worst-frame is the metric that matters, not the
//!   average — averages hide the hitch you feel),
//! - **dropped** frames (synced: intervals actually missed) / **over-budget** frames (uncapped) —
//!   synced wall time rails at the display's present grant, so the two readings mean different things,
//! - **cpu ms/frame** — process CPU, all threads: the cost meter the rail *can't* fool, the HUD twin
//!   of the probes' `cpu_ms` (decision 0711),
//! - a frame-time graph with the budget line,
//! - a **VSync toggle** — the "uncap and see" headroom test. Trust it only when it moves: the uncap
//!   is `AutoNoVsync` (explicit `Immediate` rails AND takes 1 s `nextDrawable` stalls on Metal —
//!   measured, see `capture::probe_uncap_mode`), and a macOS power state can withhold the >60 grant
//!   entirely (0362), in which case no present mode uncaps and `cpu ms` is the number to read.
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

/// Synced, wall frame time rails at the display's present interval and jitters ±1 ms around it, so
/// counting frames over the 16.7 ms budget mostly counts jitter (a perfectly healthy 60 Hz rail
/// reads "48% over budget"). What a synced player *feels* is a missed interval: 1.5× budget (25 ms)
/// sits past any rail jitter and under the doubled frame (33 ms), so the synced HUD counts
/// **dropped** frames against this instead.
const DROPPED_FACTOR: f32 = 1.5;

/// Does this present mode sync to the display? Synced, the HUD's wall-clock numbers measure the
/// present grant, not our cost — the stats lines switch meaning on it.
fn synced_mode(mode: PresentMode) -> bool {
    !matches!(
        mode,
        PresentMode::AutoNoVsync | PresentMode::Immediate | PresentMode::Mailbox
    )
}

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
        #[cfg(target_os = "macos")]
        stall_sample::plugin(app);
    }
}

/// Rolling window of recent **real** frame durations (ms) + the derived hitch metrics.
#[derive(Resource)]
struct FrameStats {
    samples: VecDeque<f32>,
    /// Per-frame process-CPU deltas (ms, **user+system across every thread** —
    /// [`process_cpu_secs`]), same window. A blocked present-wait consumes no CPU, so this is the
    /// frame-cost meter vsync cannot rail: the HUD twin of the probes' `cpu_ms` (decision 0711),
    /// directly comparable with a reporter's "N % CPU".
    cpu_samples: VecDeque<f32>,
    /// [`process_cpu_secs`] at the previous sample, for the per-frame delta. `None` until the
    /// first frame (and forever on platforms where `getrusage` is unavailable).
    prev_cpu_secs: Option<f64>,
}

impl Default for FrameStats {
    fn default() -> Self {
        Self {
            samples: VecDeque::with_capacity(SAMPLE_WINDOW),
            cpu_samples: VecDeque::with_capacity(SAMPLE_WINDOW),
            prev_cpu_secs: None,
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

    fn push_cpu(&mut self, ms: f32) {
        if self.cpu_samples.len() == SAMPLE_WINDOW {
            self.cpu_samples.pop_front();
        }
        self.cpu_samples.push_back(ms);
    }

    /// Windowed mean CPU ms/frame, all threads. `None` where the platform can't answer.
    fn cpu_ms(&self) -> Option<f32> {
        (!self.cpu_samples.is_empty())
            .then(|| self.cpu_samples.iter().sum::<f32>() / self.cpu_samples.len() as f32)
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

    /// `(count, fraction)` of windowed frames above `threshold_ms`.
    fn frames_over(&self, threshold_ms: f32) -> (usize, f32) {
        let n = self.samples.iter().filter(|&&ms| ms > threshold_ms).count();
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
    let cpu = process_cpu_secs();
    if let (Some(prev), Some(now)) = (stats.prev_cpu_secs, cpu) {
        stats.push_cpu(((now - prev) * 1000.0) as f32);
    }
    stats.prev_cpu_secs = cpu;
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
    let synced = windows
        .single()
        .map(|w| synced_mode(w.present_mode))
        .unwrap_or(true);
    // Synced, a small over is rail jitter, not cost — red only past the dropped threshold there.
    let red_above = if synced {
        FRAME_BUDGET_MS * DROPPED_FACTOR
    } else {
        FRAME_BUDGET_MS
    };

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
            let (over_n, over_frac) = stats.frames_over(red_above);
            let win_len = stats.samples.len();
            let entities = diagnostics
                .get(&EntityCountDiagnosticsPlugin::ENTITY_COUNT)
                .and_then(|d| d.value())
                .unwrap_or(0.0);

            let last_col = if last > red_above { red } else { green };
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
            // Synced: "dropped" = missed present intervals (> DROPPED_FACTOR × budget) — the felt
            // metric; wall time can't say more while it rails at the grant. Uncapped: the honest
            // over-budget count against the 16.7 floor.
            ui.colored_label(
                ob_col,
                format!(
                    "{} {over_n}/{win_len}  ({:.0}%)",
                    if synced { "dropped" } else { "over budget" },
                    over_frac * 100.0
                ),
            )
            .on_hover_text(if synced {
                "frames past 1.5× budget — a missed display interval; \
                 small overs while synced are present-grant jitter, not cost"
            } else {
                "frames past the 16.7 ms budget (uncapped: wall time ≈ real frame cost)"
            });
            // The vsync-immune cost meter: process CPU per frame, every thread summed (the probes'
            // `cpu_ms`, decision 0711). ~6 ms is the 1.12.1 reference client's whole-scene cost —
            // the long-term bar.
            if let Some(cpu) = stats.cpu_ms() {
                let mean_ms = if fps > 0.0 { 1000.0 / fps } else { 0.0 };
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
                    // AutoNoVsync, never Immediate: on Metal, explicit Immediate both rails and
                    // takes ~1 s nextDrawable stalls (measured — `capture::probe_uncap_mode`).
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

/// Whole-process CPU seconds consumed so far — **user + system, summed across every thread**
/// (`getrusage(RUSAGE_SELF)`).
///
/// The perf probes report wall-clock frame time, which on this machine is not a usable regression
/// instrument: parallel session worktrees build on the same 14 cores, and two identical probe runs
/// of the same pin came back 49.6 ms and 28.6 ms apart purely on machine load. CPU-per-frame moves
/// with the work we actually do, not with who else is compiling — and it is the metric the Mac
/// report is written in ("250 % CPU at 59 fps" against 1.12.1's "100 % at 160"), so a probe that
/// prints it can be compared against a reporter's number directly.
///
/// Non-unix returns `None`: the probes print the field only where the platform answers.
pub(crate) fn process_cpu_secs() -> Option<f64> {
    #[cfg(unix)]
    {
        // SAFETY: `getrusage` writes a fully-initialized `rusage` into the out-param and reads
        // nothing from it; zeroed is a valid starting value for a C struct of plain integers.
        unsafe {
            let mut ru: libc::rusage = std::mem::zeroed();
            if libc::getrusage(libc::RUSAGE_SELF, &mut ru) != 0 {
                return None;
            }
            let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 * 1e-6;
            Some(secs(ru.ru_utime) + secs(ru.ru_stime))
        }
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// The stuck-main-thread self-sampler (`WOW_STALL_SAMPLE=0` to disable; macOS only): a watchdog
/// thread notices the main-thread heartbeat has gone stale and shells the stock profiler
/// (`/usr/bin/sample`) at our own PID, so an intermittent stall or teardown hang **diagnoses
/// itself the next time it happens** — on anyone's run, no reproduction needed. Two bugs that
/// each lost their reproduction motivated it (decision 0713): the ~1 s silent frame stalls at
/// the BWL pin, and the on-close beachball the director had to force-quit — where even the probe
/// backstop's `process::exit(0)` can wedge, because libc `exit(3)` runs the same atexit teardown
/// the hang may own. Samples land in `~/Library/Logs/benilla/` and the path prints on stderr
/// (the tracing subscriber may already be gone during teardown).
///
/// After a teardown sample, an [`EXIT_KILL_MS`](stall_sample::EXIT_KILL_MS) backstop `_exit(0)`
/// ends the wedged process — diagnosis first, then the force-quit the director would otherwise
/// perform by hand. `libc::_exit`, never `process::exit`: it skips the atexit handlers.
///
/// Verification is end-to-end via two injectors (used by the 0713 probe rounds, kept as standing
/// test affordances): `WOW_STALL_INJECT=<at_secs>:<ms>` sleeps the main thread mid-run, and
/// `WOW_TEARDOWN_INJECT=<ms>` wedges World drop via a resource whose `Drop` sleeps — the
/// director's beachball, synthetically.
#[cfg(target_os = "macos")]
mod stall_sample {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    use bevy::prelude::*;
    use bevy::time::Real;

    /// The monotonic epoch every heartbeat is measured against. `Instant`, not `SystemTime`:
    /// a wall-clock step (NTP) must not fake a stall — an instrument that lies once is poison.
    static START: OnceLock<Instant> = OnceLock::new();
    /// Last main-thread heartbeat, ms since [`START`]. 0 = no full frame yet, watchdog stays
    /// quiet — startup (shader compiles, first loads) never false-positives.
    static HEARTBEAT_MS: AtomicU64 = AtomicU64::new(0);
    /// Latched the frame `AppExit` is written: switches the watchdog to the teardown threshold
    /// and arms the post-sample `_exit` backstop.
    static EXITING: AtomicBool = AtomicBool::new(false);

    /// In-frame staleness that means a real stall (ms) — above the ~250 ms loading-screen
    /// hitches, below the ~1030 ms stall class it exists to catch.
    const STALL_MS: u64 = 600;
    /// In-frame sampling stays disarmed this long after launch: startup/login legitimately
    /// stalls past [`STALL_MS`] (a 683 ms hitch at 1.7 s uptime, caught by the 0713 pristine-run
    /// gate), and the classes this instrument hunts are steady-state. Teardown is unaffected.
    const STARTUP_GRACE_MS: u64 = 15_000;
    /// Post-`AppExit` staleness that means teardown is wedged, not merely slow (ms). A clean
    /// teardown is sub-second (26/26 measured exit cycles); the probe hard-exit backstop sits
    /// at 5 s, so a 2.5 s trigger + 2 s capture completes before it.
    const EXIT_STALL_MS: u64 = 2_500;
    /// Teardown backstop: still alive this long after `AppExit` (sample already taken) → `_exit`.
    const EXIT_KILL_MS: u64 = 8_000;
    /// Rate limit: at most this many samples per run, at least [`SAMPLE_GAP_MS`] apart.
    const SAMPLE_CAP: u32 = 5;
    const SAMPLE_GAP_MS: u64 = 20_000;

    fn since_start_ms() -> u64 {
        START.get().map_or(0, |s| s.elapsed().as_millis() as u64)
    }

    /// `Last`-schedule heartbeat: stamp the clock; latch [`EXITING`] the frame the app decides
    /// to quit (no later frame will run to unlatch anything).
    fn beat(mut exits: MessageReader<AppExit>) {
        if exits.read().next().is_some() {
            EXITING.store(true, Ordering::SeqCst);
        }
        HEARTBEAT_MS.store(since_start_ms().max(1), Ordering::SeqCst);
    }

    pub(super) fn plugin(app: &mut App) {
        if std::env::var("WOW_STALL_SAMPLE").is_ok_and(|v| v == "0") {
            return;
        }
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let dir = std::path::PathBuf::from(home).join("Library/Logs/benilla");
        let _ = std::fs::create_dir_all(&dir);
        START
            .set(Instant::now())
            .expect("stall_sample plugin built twice");
        app.add_systems(Last, beat);
        std::thread::Builder::new()
            .name("stall-sample".into())
            .spawn(move || watchdog(&dir))
            .expect("spawn stall-sample watchdog");

        // The injectors (doc above): a mid-run main-thread sleep, and a wedged World drop.
        if let Some((at, ms)) = std::env::var("WOW_STALL_INJECT")
            .ok()
            .and_then(|v| v.split_once(':').map(|(a, m)| (a.to_owned(), m.to_owned())))
            .and_then(|(a, m)| Some((a.parse::<f32>().ok()?, m.parse::<u64>().ok()?)))
        {
            app.insert_resource(StallInject {
                at,
                ms,
                fired: false,
            })
            .add_systems(Update, stall_inject);
        }
        if let Ok(ms) = std::env::var("WOW_TEARDOWN_INJECT") {
            if let Ok(ms) = ms.parse::<u64>() {
                app.insert_resource(TeardownWedge(ms));
            }
        }
    }

    fn watchdog(dir: &std::path::Path) {
        let pid = std::process::id().to_string();
        let mut taken = 0u32;
        let mut last_sample_ms = 0u64;
        let mut exit_seen_ms = 0u64;
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let hb = HEARTBEAT_MS.load(Ordering::SeqCst);
            if hb == 0 {
                continue;
            }
            let now = since_start_ms();
            let exiting = EXITING.load(Ordering::SeqCst);
            if exiting && exit_seen_ms == 0 {
                exit_seen_ms = now;
            }
            if exiting && now.saturating_sub(exit_seen_ms) > EXIT_KILL_MS {
                eprintln!(
                    "stall-sample: teardown still wedged {} ms after AppExit — _exit(0)",
                    now.saturating_sub(exit_seen_ms)
                );
                use std::io::Write;
                let _ = std::io::stdout().flush();
                // SAFETY: terminates the process immediately; no locks, no atexit — the whole
                // point, since the wedge may own both.
                unsafe { libc::_exit(0) };
            }
            let age = now.saturating_sub(hb);
            if !exiting && now < STARTUP_GRACE_MS {
                continue;
            }
            let threshold = if exiting { EXIT_STALL_MS } else { STALL_MS };
            if age > threshold
                && taken < SAMPLE_CAP
                // The gap gates *between* samples only — gating the first one against
                // `last_sample_ms = 0` would silently forbid any sample in the first
                // [`SAMPLE_GAP_MS`] of process uptime (caught by the 0713 injector runs).
                && (taken == 0 || now.saturating_sub(last_sample_ms) > SAMPLE_GAP_MS)
            {
                taken += 1;
                last_sample_ms = now;
                let file = dir.join(format!(
                    "stall-{}{}.txt",
                    std::process::id(),
                    if exiting {
                        format!("-teardown-{now}")
                    } else {
                        format!("-{now}")
                    }
                ));
                eprintln!(
                    "stall-sample: main thread stale {age} ms{} — sampling to {}",
                    if exiting { " (teardown)" } else { "" },
                    file.display()
                );
                // 1 s in-frame so the stall dominates its own capture (a ~1 s stall in a 2 s
                // window is half idle); 2 s for teardown, where the wedge holds the whole time.
                let _ = std::process::Command::new("/usr/bin/sample")
                    .arg(&pid)
                    .arg(if exiting { "2" } else { "1" })
                    .arg("-file")
                    .arg(&file)
                    .status();
            }
        }
    }

    /// `WOW_STALL_INJECT=<at_secs>:<ms>` — one deliberate main-thread sleep, mid-run.
    #[derive(Resource)]
    struct StallInject {
        at: f32,
        ms: u64,
        fired: bool,
    }

    fn stall_inject(mut inject: ResMut<StallInject>, time: Res<Time<Real>>) {
        if !inject.fired && time.elapsed_secs() >= inject.at {
            inject.fired = true;
            warn!("stall-inject: sleeping the main thread {} ms", inject.ms);
            std::thread::sleep(Duration::from_millis(inject.ms));
        }
    }

    /// `WOW_TEARDOWN_INJECT=<ms>` — wedge World drop: this resource's `Drop` runs on the main
    /// thread during `App` teardown and sleeps there, reproducing the beachball synthetically.
    #[derive(Resource)]
    struct TeardownWedge(u64);

    impl Drop for TeardownWedge {
        fn drop(&mut self) {
            eprintln!("teardown-inject: wedging World drop {} ms", self.0);
            std::thread::sleep(Duration::from_millis(self.0));
        }
    }
}
