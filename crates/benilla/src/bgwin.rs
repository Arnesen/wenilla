//! Background instrumented runs: probe/capture/regression windows that never fight the
//! director's screen.
//!
//! Agent-driven runs (captures, live probes, the rig, FPS journals) open a real window for
//! seconds to minutes on the same desktop the director is working on. Left alone, each launch
//! stole their focus twice over:
//!
//! - **Window-side** — the primary window opened focused (`makeKeyAndOrderFront`), swallowing
//!   whatever they were typing (on 2026-07-19 a login-shot run typed their keystrokes into the
//!   account box). `Window::focused = false` fixed that for capture/login-shot runs only; every
//!   live probe still opened key.
//! - **App-side (macOS)** — winit's AppKit backend unconditionally calls
//!   `activateIgnoringOtherApps` at `applicationDidFinishLaunching` and sets the Regular
//!   activation policy, so even an unfocused run activated the app: menu bar switched, a Dock
//!   icon appeared, and the window ordered in **front** of every other app's. bevy_winit 0.18
//!   exposes no `EventLoopBuilderExtMacOS` hook to turn either off, so it can't be prevented —
//!   only undone, immediately after launch (decision 0703).
//!
//! [`background_run`] detects an instrumented run from the env that drives it (no recipe or
//! script changes anywhere), `main` opens the window unfocused **and**
//! [`bevy::window::WindowLevel::AlwaysOnBottom`] — on screen (so surface rendering, readback
//! and screenshots are untouched; it sits at `kCGNormalWindowLevel - 1`) but *under* every
//! normal window — and [`BgWinPlugin`] demotes the macOS app to the Accessory activation
//! policy (no Dock icon, no Cmd-Tab entry, no menu-bar takeover) and hands activation straight
//! back to whatever the director was using.

use bevy::prelude::*;

/// Env prefixes that mark a run as agent-driven — the capture harness, shots/smokes, and the
/// live-probe fleet. Deliberately only *run-driving* switches: a plain modifier the director
/// might set on an attended run (`WOW_GM`, `WOW_FARCLIP`, traces…) must NOT push their own
/// window to the bottom of the stack. A new probe env usually starts with `WOW_PROBE`/
/// `WOW_CAPTURE` and is covered for free; one that doesn't shows up as "the new probe window
/// opened focused" and gets its prefix added here.
const BG_ENV_PREFIXES: &[&str] = &[
    "WOW_AUDIT_",
    "WOW_CAPTURE",
    "WOW_CHARCREATE_SHOT",
    "WOW_CHARSELECT_SHOT",
    "WOW_CREATE_TEST",
    "WOW_DEPTH",
    "WOW_FPS_",
    "WOW_GLUE_ROUNDTRIP",
    "WOW_LIVE_",
    "WOW_LOGIN_SHOT",
    "WOW_LOGIN_SMOKE",
    "WOW_LOGOUT_SMOKE",
    "WOW_MM_BLIP_PROBE",
    "WOW_MM_PROBE",
    "WOW_NODE_PROBE",
    "WOW_PARTICLE_CENSUS",
    "WOW_PHASE",
    "WOW_PICK",
    "WOW_PORTRAIT_TEST",
    "WOW_PROBE",
    "WOW_RIG",
];

/// Is this an instrumented background run? `WOW_BG=1` forces yes on any run, `WOW_BG=0` forces
/// no (the escape hatch when the director wants to *watch* a probe live); otherwise, auto-detect
/// from the run-driving env ([`BG_ENV_PREFIXES`]).
pub fn background_run() -> bool {
    match std::env::var("WOW_BG").as_deref() {
        Ok("0") => return false,
        Ok(_) => return true,
        Err(_) => {}
    }
    std::env::vars_os().any(|(name, _)| {
        name.to_str()
            .is_some_and(|name| BG_ENV_PREFIXES.iter().any(|p| name.starts_with(p)))
    })
}

/// On a [`background_run`], undo winit's launch-time app activation (macOS; no-op elsewhere).
/// The window-side half (unfocused + always-on-bottom) lives where the window is built, in
/// `main` — this plugin owns only the app-side half.
pub struct BgWinPlugin;

impl Plugin for BgWinPlugin {
    fn build(&self, app: &mut App) {
        if !background_run() {
            return;
        }
        info!(
            "bgwin: instrumented run — window opens unfocused at the bottom of the stack, \
             app demoted from Dock/Cmd-Tab (WOW_BG=0 for a normal focused window)"
        );
        #[cfg(target_os = "macos")]
        app.add_systems(PreStartup, macos::demote)
            .add_systems(Update, macos::hold_background);
        #[cfg(not(target_os = "macos"))]
        let _ = app;
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use bevy::ecs::system::NonSendMarker;
    use bevy::prelude::*;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::MainThreadMarker;

    /// How many frames [`hold_background`] keeps watching for a late-landing activation.
    /// winit's `activateIgnoringOtherApps` is a WindowServer round-trip: it can be *granted*
    /// after `PreStartup`'s [`demote`] already ran, which would leave the app active with no
    /// one to deactivate it. A couple of seconds of frames covers the grant comfortably.
    const HOLD_FRAMES: u32 = 120;

    /// `PreStartup` (after winit's `applicationDidFinishLaunching` set Regular + activated):
    /// demote to Accessory — no Dock icon, no Cmd-Tab entry, no menu-bar takeover — and give
    /// activation back to the app the director was using. The `NonSendMarker` param pins the
    /// system to the main thread (AppKit's requirement).
    pub(super) fn demote(_main_thread: NonSendMarker) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        // SAFETY: main thread (the `NonSendMarker` param + the marker check above).
        unsafe {
            if app.isActive() {
                app.deactivate();
            }
        }
    }

    /// The first [`HOLD_FRAMES`] frames: if the launch-time activation lands *after*
    /// [`demote`], hand it back again. Self-quiets after the window; a no-op thereafter.
    pub(super) fn hold_background(_main_thread: NonSendMarker, mut frames: Local<u32>) {
        if *frames > HOLD_FRAMES {
            return;
        }
        *frames += 1;
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: main thread (the `NonSendMarker` param + the marker check above).
        unsafe {
            if app.isActive() {
                app.deactivate();
            }
        }
    }
}
