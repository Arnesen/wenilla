//! Player-facing **video settings** — the knobs that reach the window and the presentation path.
//!
//! Today that is exactly one: **VSync** (`gxVSync`, the Graphics page's *Vertical Sync* row).
//!
//! **Why it is a player setting and not a dev toggle.** It briefly lived as a checkbox on the perf
//! HUD, where it existed to answer one instrument question — "is the GPU keeping up?" — because a
//! synced frame reads as the display's grant whether it needed 3 ms or 16 (0717). That is the wrong
//! home twice over: the HUD is `#[cfg(feature = "dev")]`, so a player build could never reach it,
//! and vsync is not a diagnostic in the first place. It is the same option 1.12 shipped —
//! `OptionsFrameCheckButtons["VERTICAL_SYNC"] = { index = 5, cvar = "gxVSync", gxRestart = 1 }`,
//! Video Options, checkbox 5 — and the same one every engine since has kept. The instrument
//! question keeps `$WOW_NOVSYNC=1`, which is where a measurement knob belongs.
//!
//! **We do not require the restart 1.12 did.** The reference's row carries `gxRestart = 1` because
//! its device could not swap the presentation interval live; wgpu reconfigures the surface on the
//! next frame, so the checkbox takes effect as you click it. A deliberate, stated departure.
//!
//! **`AutoNoVsync`, never `Immediate`.** On macOS/Metal, explicit `Immediate` both rails *and*
//! takes ~1 s `nextDrawable` stalls — measured, and pinned at [`crate::capture::probe_uncap_mode`].

use bevy::prelude::*;
use bevy::window::{PresentMode, PrimaryWindow};

/// `$WOW_NOVSYNC=1` — the session-only measurement override. It wins over the config for the run
/// and never reaches `config.toml` (registered in [`crate::cvars`]'s `env_overridden` set), so a
/// headless FPS-journal run can uncap without making the player's setting sticky.
pub(crate) fn novsync_env() -> bool {
    std::env::var("WOW_NOVSYNC").as_deref() == Ok("1")
}

/// The video knobs a CVar write can land on. Default = what the client ships: synced, matching the
/// primary window's own `PresentMode::default()` (Fifo) — see `lib.rs`'s window literal.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct VideoConfig {
    pub(crate) vsync: bool,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            vsync: !novsync_env(),
        }
    }
}

/// The present mode a vsync setting means.
///
/// **On is `PresentMode::default()` — Bevy's default, deliberately, not `AutoVsync`.** 0294 recorded
/// running the engine default and that is `Fifo`: strict, never tears. `AutoVsync` is *not* a
/// synonym — it resolves to `FifoRelaxed` where available, which permits a tear on a late frame.
/// The dev HUD's old checkbox wrote `AutoVsync` on the way back on, so toggling it off and on
/// silently left the client on a different mode than it booted with; naming one function the single
/// mapping is what closes that.
pub(crate) fn present_mode(vsync: bool) -> PresentMode {
    if vsync {
        PresentMode::default()
    } else {
        PresentMode::AutoNoVsync
    }
}

pub(crate) struct VideoPlugin;

impl Plugin for VideoPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<VideoConfig>()
            .add_systems(Update, apply_present_mode);
    }
}

/// Push the setting to the window **when the setting moves**, and only then.
///
/// The value compare is the `rescatter_clutter` pattern (0992), and it is load-bearing for two
/// separate reasons. `Res::is_changed()` over-fires: the cvar sync builds its `Knobs` bundle by
/// deref-mutting *every* knob resource, so any CVar write anywhere flags this one. And re-asserting
/// the setting every frame would fight the capture probes, which write `present_mode` on the window
/// directly mid-run (`capture::mod`, `probes::live_fps`) to uncap a measurement — their override
/// has to stick.
///
/// First sight deliberately does **not** just arm: `load_config` applies the saved value at
/// `Startup`, after the window already exists at its boot mode, so the first run is the one that
/// reconciles them.
fn apply_present_mode(
    cfg: Res<VideoConfig>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    mut last: Local<Option<bool>>,
) {
    if last.replace(cfg.vsync) == Some(cfg.vsync) {
        return;
    }
    let want = present_mode(cfg.vsync);
    let Ok(mut window) = windows.single_mut() else {
        return;
    };
    // Guarded so an unchanged mode does not deref-mut `Window` — a spurious change there is a
    // surface reconfigure.
    if window.present_mode != want {
        window.present_mode = want;
        info!(
            "video: vsync {} ({want:?})",
            if cfg.vsync { "on" } else { "off" }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped default is synced, and "synced" is the engine default 0294 chose — `Fifo`, not
    /// `AutoVsync`. Written as an assertion because the two are easy to conflate and are not the
    /// same: `AutoVsync` resolves to `FifoRelaxed` where available, which tears on a late frame.
    #[test]
    fn the_default_is_synced_like_the_window_literal() {
        assert!(VideoConfig::default().vsync);
        assert_eq!(present_mode(true), PresentMode::default());
        assert_eq!(present_mode(true), PresentMode::Fifo);
    }

    /// Uncapped is `AutoNoVsync` and never `Immediate` — the Metal finding this module exists to
    /// hold (`Immediate` rails and takes ~1 s `nextDrawable` stalls).
    #[test]
    fn uncapped_is_autonovsync_not_immediate() {
        assert_eq!(present_mode(false), PresentMode::AutoNoVsync);
    }
}
