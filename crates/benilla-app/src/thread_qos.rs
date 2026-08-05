//! macOS thread QoS: keep frame-critical threads on the P-cores under system load.
//!
//! Apple Silicon schedules by QoS class, not fairness: a default-QoS thread queues behind any
//! higher class for P-core time. The main thread is user-interactive out of the box (verified by
//! probe — even bare `cargo run`, no bundle), but every worker Bevy spawns — compute pool, IO
//! pool, the pipelined-rendering thread — starts at default, the same class as `rustc` or an OBS
//! encoder. Under a background build the frame's own workers wait in the compiler's queue and the
//! client drops to 20 fps while the retail client (whose workers are promoted, plus Game Mode)
//! holds steady. Promoting the workers is the per-thread half of that story; the Game Mode half
//! needs an app bundle and is release-track (decision 0609).
//!
//! `pthread_set_qos_class_self_np` is not bound by the `libc` crate, so the extern lives here.
//! Everything is a no-op off macOS.

use bevy::prelude::*;
use bevy::render::{Render, RenderApp, RenderSystems};

/// QoS classes we actually use. Values are Darwin's `qos_class_t`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum QosClass {
    /// Work on the critical path of the current frame (compute pool, render thread).
    UserInteractive = 0x21,
    /// Work the frame is waiting on soon but not this frame (asset IO, async compute, net IO).
    UserInitiated = 0x19,
}

/// Promote the calling thread to `class`. Safe to call repeatedly; logs once on failure.
pub fn promote_current_thread(class: QosClass) {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
        }
        let rc = unsafe { pthread_set_qos_class_self_np(class as u32, 0) };
        if rc != 0 {
            warn_once!("thread QoS promotion to {class:?} failed (rc={rc})");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = class;
}

/// Promotes the pipelined-rendering thread. Task-pool threads are promoted at spawn via
/// `TaskPoolOptions::on_thread_spawn` (see `main.rs`), but Bevy spawns the render thread with a
/// bare `std::thread::spawn` and no hook. An exclusive system runs on whichever thread drives the
/// render schedule — the main thread during startup, the render thread once pipelined rendering
/// takes over — so it re-runs each frame behind a thread-local latch instead of `run_once`.
pub struct ThreadQosPlugin;

impl Plugin for ThreadQosPlugin {
    fn build(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.add_systems(Render, promote_render_thread.in_set(RenderSystems::Prepare));
    }
}

/// Read back the calling thread's QoS class (support for the macOS-only tests below).
#[cfg(all(test, target_os = "macos"))]
fn current_thread_qos() -> Option<u32> {
    unsafe extern "C" {
        fn pthread_self() -> *mut core::ffi::c_void;
        fn pthread_get_qos_class_np(
            thread: *mut core::ffi::c_void,
            qos_class: *mut u32,
            relative_priority: *mut i32,
        ) -> i32;
    }
    let mut qos: u32 = 0;
    let rc = unsafe { pthread_get_qos_class_np(pthread_self(), &mut qos, core::ptr::null_mut()) };
    (rc == 0).then_some(qos)
}

fn promote_render_thread(_world: &mut World) {
    std::thread_local! {
        static PROMOTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    PROMOTED.with(|p| {
        if !p.get() {
            promote_current_thread(QosClass::UserInteractive);
            debug!(
                "thread QoS: promoted render-schedule thread {:?}",
                std::thread::current().name().unwrap_or("<unnamed>")
            );
            p.set(true);
        }
    });
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// The promotion really lands: a fresh (default-QoS) thread reads back the class it set.
    #[test]
    fn promotion_applies_to_spawned_thread() {
        for class in [QosClass::UserInteractive, QosClass::UserInitiated] {
            let observed = std::thread::spawn(move || {
                let before = current_thread_qos();
                promote_current_thread(class);
                (before, current_thread_qos())
            })
            .join()
            .unwrap();
            // A bare std thread spawns at default (0x15) — the gap this module closes.
            assert_eq!(observed.0, Some(0x15), "spawned thread not default-QoS");
            assert_eq!(observed.1, Some(class as u32), "promotion did not apply");
        }
    }
}
