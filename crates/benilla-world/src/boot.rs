//! The **tuned Bevy boot** — the `DefaultPlugins` set every benilla binary stands on.
//!
//! Extracted from `benilla_app::run` the moment there was a second binary to boot ([`crate::worldview`],
//! decision 1160): the window differs per binary, but the *engine* tuning — the asset root, the log
//! filter, the task-pool sizing and QoS, and the disabled audio plugin — must not. Each of those
//! four is load-bearing and each was learned the hard way; a second boot that quietly re-derived
//! them would be a second place for them to rot.
//!
//! The `Window` stays the caller's, because that is genuinely per-binary: the client's is shaped by
//! the capture harness and the background-run rules (decisions 0703/0709/1148), the world viewer's
//! is a plain window.

use bevy::app::{PluginGroupBuilder, TaskPoolOptions, TaskPoolPlugin};
use bevy::prelude::*;

use crate::thread_qos;

/// `DefaultPlugins` with benilla's engine tuning applied, around the caller's primary window.
pub fn tuned_default_plugins(primary_window: Window) -> PluginGroupBuilder {
    DefaultPlugins
        .set(WindowPlugin {
            primary_window: Some(primary_window),
            ..default()
        })
        // Our file assets (the WGSL shaders, the default UI XML) live beside THIS crate — but the
        // binary is built by a shim package, so Bevy's runtime `CARGO_MANIFEST_DIR` fallback would
        // resolve `assets/` in the shim's dir (and a bare-binary run has no manifest dir at all —
        // the silently-no-shaders trap in `capture/mod.rs`'s header). Bake this crate's absolute
        // path at compile time: right under `cargo run` from any package, and a bare
        // `target/debug/benilla` now finds its shaders too. Machine-local, like any dev build
        // (decision 0993).
        //
        // This root is the ENGINE's and only the engine's (decision 1171). It sits in
        // `benilla-world` because `boot` does, and `boot` is here because both binaries need a
        // window and a plugin set. The game's own files — its FrameXML and its five UI shaders —
        // moved to `benilla-app/assets` and reach the server through the `game://` source that
        // crate registers, so a path with no scheme is the engine's by construction and
        // `benilla-worldview` ships nothing it cannot draw.
        .set(bevy::asset::AssetPlugin {
            file_path: concat!(env!("CARGO_MANIFEST_DIR"), "/assets").into(),
            ..default()
        })
        // Quiet wgpu/naga; our own crates stay at info.
        .set(bevy::log::LogPlugin {
            filter: "wgpu=error,naga=warn".into(),
            ..default()
        })
        // Asset streaming is this client's load bottleneck: every M2/WMO/BLP read decompresses from
        // MPQ and parses **synchronously** on Bevy's IO task pool, and the AssetServer runs *all*
        // loads there. Bevy's default caps that pool at 4 threads, so a teleport into a dense area —
        // terrain + WMOs + their doodad props, all bursting at once — saturates it and the
        // net-driven NPC/GameObject models queue behind the flood. Give IO more of the box (it sits
        // idle when not streaming); the world-render path is GPU/IO-bound, not compute-bound, so
        // trading some compute threads for streaming throughput is the right call.
        // Thread QoS (decision 0609): Bevy's workers spawn at default QoS — the same Darwin
        // scheduling class as rustc or an OBS encoder — so under a background build the frame's own
        // threads queue behind the compiler for P-core time. Promote them at spawn: compute runs
        // this frame's systems (user-interactive); IO/async-compute feed upcoming frames
        // (user-initiated — above default, below the frame itself). The render thread has no spawn
        // hook; `ThreadQosPlugin` promotes it from inside.
        .set(TaskPoolPlugin {
            task_pool_options: TaskPoolOptions {
                io: bevy::app::TaskPoolThreadAssignmentPolicy {
                    min_threads: 2,
                    max_threads: 8,
                    percent: 0.5,
                    on_thread_spawn: Some(std::sync::Arc::new(|| {
                        thread_qos::promote_current_thread(thread_qos::QosClass::UserInitiated)
                    })),
                    on_thread_destroy: None,
                },
                async_compute: bevy::app::TaskPoolThreadAssignmentPolicy {
                    on_thread_spawn: Some(std::sync::Arc::new(|| {
                        thread_qos::promote_current_thread(thread_qos::QosClass::UserInitiated)
                    })),
                    ..TaskPoolOptions::default().async_compute
                },
                compute: bevy::app::TaskPoolThreadAssignmentPolicy {
                    on_thread_spawn: Some(std::sync::Arc::new(|| {
                        thread_qos::promote_current_thread(thread_qos::QosClass::UserInteractive)
                    })),
                    // `WOW_THREADS=1` serialises the systems that run this frame. Not a performance
                    // dial — a **diagnostic**: a defect that alternates frame to frame with no
                    // camera, geometry or draw-order change behind it is what an unordered write
                    // between two systems looks like, and that is separable from every other cause
                    // only by taking the concurrency away. Anything that survives `WOW_THREADS=1`
                    // is not a race.
                    max_threads: match std::env::var("WOW_THREADS").ok().as_deref() {
                        Some("1") => 1,
                        _ => TaskPoolOptions::default().compute.max_threads,
                    },
                    ..TaskPoolOptions::default().compute
                },
                ..default()
            },
        })
        // Sound is kira behind our own mixer seam (decision 0070); Bevy's AudioPlugin would only
        // open a second, never-used OS output stream at startup. Off (0530). Its rodio/cpal stack
        // still compiles in via bevy's default feature — trimming the feature set is a separate,
        // wider call.
        .disable::<bevy::audio::AudioPlugin>()
}
