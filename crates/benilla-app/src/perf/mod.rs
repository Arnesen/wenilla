//! `perf` — performance instrumentation + the standing dev HUD ([`PerfPlugin`]).
//!
//! Owns the **frame-cost measurement layer** that every future subsystem is measured against (the
//! standard), and draws the always-on cost pill (top-center) — the whole HUD since 1454; anything
//! deeper is an instrument's job (the journal, the probes, Tracy). The **dev chord + `P`** toggles
//! it (`Ctrl+Shift+P` — decisions 0585/0867/0870).
//!
//! The concerns, one file each:
//! - [`clock`] — the three CPU clocks (process, main thread, machine) every number is denominated in,
//! - [`stats`] — the rolling windows the pill reads,
//! - [`hud`] — the standing cost pill,
//! - [`trace`] / [`journal`] — the two CSV instruments (`WOW_STREAM_TRACE`, `WOW_FPS_JOURNAL`),
//! - [`stall`] — the stuck-main-thread self-sampler (macOS),
//! - [`census`] — the env-gated premise counters.
//!
//! **The law the whole surface obeys (0717): while synced, wall frame time measures the display's
//! present grant, not our cost.** Only the CPU series measure work. That is why the pill's headline
//! is `cpu ms` and not fps — on a 120 Hz-adaptive panel, cost can double with the granted interval
//! unchanged and framerate unmoved, and the pill's old red threshold (fps < 58) sat ~5.7× above a
//! healthy frame. fps stays on the pill — dim and small, the familiar anchor, never the thing
//! being watched (1448).
//!
//! The budget is a **60 fps floor = 16.7 ms; no frame should exceed it** — but note that the
//! *missed-interval* threshold is derived from the interval we actually observe, not from this
//! constant (see [`stats::DROPPED_FACTOR`]). Deep per-system attribution is via Tracy
//! (`cargo run --features tracy`) — the `info_span!` markers on the hot systems feed it.
//!
//! ⚠️ **macOS/Metal — GPU timing.** We have `Features::TIMESTAMP_QUERY` on this machine, but
//! **`TIMESTAMP_QUERY_INSIDE_PASSES` is `false`**: Apple GPUs sample counters only at stage
//! boundaries (`MTLCounterSamplingPoint::AtStageBoundary`). Bevy's `RenderDiagnosticsPlugin` writes
//! its timestamps *inside* passes, so on Apple Silicon every span falls through to the CPU-only
//! branch and the store carries **zero** `elapsed_gpu` paths — verified on a live run: 14 diagnostic
//! paths, all `elapsed_cpu`. (bevy_render's own "Vulkan and DX12 only" comment is stale as a
//! statement about the *platform* — an Intel Mac would report `INSIDE_PASSES` and emit GPU spans.)
//! Pass-boundary `timestamp_writes` **does** work here and is the route to real GPU-ms — measured
//! 0.015/0.019/0.049 ms across 256²/1920×1080/4096² clears, 12/12 reproducible. It needs two
//! sentinel render-graph nodes at ~0.03 ms/frame, and the query set must be resolved in a
//! *different* command buffer than the timed pass or Metal returns zeros. Until that is built, a
//! GPU-bound frame is identified rather than timed: it is the one that runs long while the CPU
//! meters stay flat (read them side by side in the journal or a probe line).

mod census;
mod clock;
mod gpu;
mod hud;
mod journal;
mod phases;
#[cfg(target_os = "macos")]
mod stall;
mod stats;
mod trace;

use bevy::prelude::*;
use bevy::render::diagnostic::RenderDiagnosticsPlugin;

pub(crate) use clock::{process_cpu_secs, system_cpu_ticks};
pub(crate) use gpu::GpuMsShared;
pub(crate) use hud::PerfHud;
pub(crate) use journal::FpsJournalPlugin;

/// The frame budget: a 60 fps floor. No frame should exceed this.
pub const FRAME_BUDGET_MS: f32 = 1000.0 / 60.0;

pub struct PerfPlugin;

impl Plugin for PerfPlugin {
    fn build(&self, app: &mut App) {
        // Render-pass timing (CPU-only on Apple Silicon — see the module header). Harmless if
        // the backend can't record it; also the hook Tracy GPU uses on Vulkan/DX12.
        app.add_plugins(RenderDiagnosticsPlugin)
            .init_resource::<stats::FrameStats>()
            .init_resource::<PerfHud>()
            .insert_resource(trace::StreamTrace {
                path: std::env::var("WOW_STREAM_TRACE").unwrap_or_default(),
                frame: 0,
                log_until: 0,
                prev_cpu_secs: None,
                prev_pipes_created: 0,
            })
            .add_systems(
                Update,
                // `toggle_hud` needs no ordering against the UI keyboard feed any more: its dev
                // chord can't be typed text, so there's no `UiKeyboardCapture` to read (decision 0585).
                (
                    hud::toggle_hud,
                    hud::refresh_hud_snapshot,
                    stats::sample_frame_time,
                ),
            )
            // The pill rides the player-UI quad pass (1453): appended with the other overlay
            // producers, so the HUD costs a Vec clone and never touches the egui lane (1454).
            .add_systems(Update, hud::pill_quads.in_set(crate::ui_pass::UiQuadAppend))
            // `Last`, so a row carries everything this frame's Stream chain did — and the reset runs
            // whether or not anything is tracing, or the counters would accumulate forever.
            .add_systems(Last, trace::trace_stream);
        // `WOW_MESH_EVENTS=1` — who churns Mesh assets per frame? The premise counter behind
        // the Stormwind trace's `allocate_and_free_meshes` row (0.86 ms/frame at a PARKED pin):
        // the allocator answers every Modified with a free+realloc, so a steady scene should
        // show ~zero here. Printed once a second with the top mutated ids' first sighting.
        if std::env::var_os("WOW_MESH_EVENTS").is_some() {
            app.add_systems(Update, census::count_mesh_events);
        }
        // `WOW_PART_CHURN=1` — the moving-regime premise counters (1461): how many frames per
        // second despawn an M2 part (= `classify_water_side` full-walk promotions), and how
        // often model materials fire Modified (= the `AssetChanged` scan wake-ups).
        if std::env::var_os("WOW_PART_CHURN").is_some() {
            app.add_systems(Update, census::count_part_churn);
        }
        // `WOW_MESH_HOLDERS=1` — who OWNS the meshes `WOW_MESH_EVENTS` counts? Prints each
        // holder's archetype signature once a second (1461's writer hunt).
        if std::env::var_os("WOW_MESH_HOLDERS").is_some() {
            app.add_systems(Last, census::mesh_holders);
        }
        // `WOW_CAM_CHANGED=1` — is the world camera's transform bit-stable at a parked pin?
        // The premise counter for gating the per-submesh visibility sweep on frame-stable
        // camera inputs: if the controller rewrites an equal transform every frame, change
        // detection fires anyway and the gate can never hold — the controller's no-op write
        // is then the first fix, not the sweep's.
        if std::env::var_os("WOW_CAM_CHANGED").is_some() {
            app.add_systems(bevy::app::PostUpdate, census::count_camera_changes);
        }
        // `WOW_ARCH_CENSUS=<secs>` — one archetype census at t=secs: every non-empty
        // archetype's entity count beside its component set, largest first. The exact by-lane
        // entity picture (1354's anchor census generalized to every lane at once), for sizing
        // which populations are worth collapsing before designing any collapse.
        if let Some(at) = std::env::var("WOW_ARCH_CENSUS")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
        {
            app.insert_resource(census::ArchCensusAt(at));
            app.add_systems(Last, census::arch_census);
        }
        // `WOW_ROW_BLOAT=<n>` — spawn n inert clones of a live static row (see the system doc):
        // the d(cpu_ms)/d(rows) instrument the consolidation option divides by.
        if std::env::var("WOW_ROW_BLOAT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .is_some_and(|n| n > 0)
        {
            app.add_systems(Update, census::row_bloat);
        }
        // `WOW_MESH_TOUCH=<secs>` — the `AssetChanged` tax meter (see the system's doc): from
        // `secs` onward, one scratch `Assets<Mesh>` modification per frame and nothing else, so
        // the arming's price is read as a within-run paired delta with the UI's own work removed.
        if let Some(at) = std::env::var("WOW_MESH_TOUCH")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
        {
            app.insert_resource(census::MeshTouchAt(at));
            app.add_systems(Update, census::mesh_touch);
        }
        // `WOW_CPU_CENSUS=<at>:<secs>` — the per-thread CPU census (see its module doc): where
        // the pill's cpu-ms goes, thread by thread, summing exactly to the process total.
        #[cfg(target_os = "macos")]
        if let Some(c) = census::cpu_census::CpuCensus::from_env() {
            app.insert_resource(c);
            app.add_systems(Update, census::cpu_census::cpu_census);
        }
        #[cfg(target_os = "macos")]
        stall::plugin(app);
        // `WOW_FRAME_PHASES=<ms>` — which PHASE of a slow frame spent it (see the module doc).
        phases::plugin(app);
        // `WOW_GPU_MS=1` — the whole-frame GPU meter (its module doc owns the design and the
        // 1389 resolve-on-a-later-submission trap). Registers nothing when off, so campaign
        // anchors never carry its ~0.03 ms sentinel cost uninvited.
        gpu::plugin(app);
    }
}
