//! Player-facing **video settings** — the knobs that reach the window and the presentation path.
//!
//! Two, both of them 1.12's own CVars over the Graphics page: **Display Mode** (`gxWindow`, the
//! *Windowed Mode* row) and **VSync** (`gxVSync`, the *Vertical Sync* row).
//!
//! # Display mode — the reference's CVar, deliberately not the reference's meaning (decision 1627)
//!
//! 1.12 ships a **two-state** display model: `gxWindow "0"` takes the display with an exclusive
//! mode-set at `gxResolution`, `gxWindow "1"` runs in a window (with `gxMaximize` for a maximized
//! one). We keep the CVar, the row, and the default; we redefine `"0"`.
//!
//! **`"0"` here means BORDERLESS fullscreen** — a normal window sized to the monitor, flagged
//! fullscreen to the compositor (`xdg_toplevel.set_fullscreen` on Wayland,
//! `_NET_WM_STATE_FULLSCREEN` on X11, a native fullscreen window on macOS). We ship no exclusive
//! mode at all, and that is not a shortcut — it is unavailable or pointless on all three targets:
//!
//! - **Wayland has no client-side mode-setting**, so there is nothing to implement: winit 0.30.13
//!   answers `Fullscreen::Exclusive` with ``warn!("`Fullscreen::Exclusive` is ignored on Wayland")``
//!   and leaves the window as it was (`platform_impl/linux/wayland/window/mod.rs:147,488`).
//! - **On X11 it is XRandR mode-setting**, which changes the *desktop's* mode and, as winit's own
//!   comment says, "does not provide a mechanism to … restore this to the desktop video mode as
//!   macOS and Windows do" — a crash leaves the player's desktop at our resolution.
//! - **On macOS** there is no exclusive mode to take.
//!
//! And the industry moved: SDL3 deleted `SDL_WINDOW_FULLSCREEN_DESKTOP` and made borderless-desktop
//! what a fullscreen window *is* unless you opt into a mode with `SDL_SetWindowFullscreenMode`;
//! WoW itself removed exclusive fullscreen in **8.0.1**, leaving Windowed and Windowed
//! (Fullscreen). Bevy's own `WindowMode::Fullscreen` arm is a liability besides — it `expect`s a
//! monitor at creation and `panic!`s on a live change that cannot resolve one
//! (`bevy_winit::winit_windows:91`, `system.rs:333`).
//!
//! **What this is worth, concretely.** Before 1627 the window was born `WindowMode::Windowed` at a
//! hard-coded 1600×900 with nothing able to change it — larger than a Steam Deck's 1280×800 panel,
//! and never flagged fullscreen. A window that does not fill gamescope's nested output is a
//! documented input break upstream (ValveSoftware/gamescope#1086 — pointer trapped in the nested
//! rect, clicks outside it dead; #1209 — tapping the letterbox warps the cursor to centre forever),
//! whose recorded workaround is "set the game to fullscreen".
//!
//! **No `gxRestart`.** 1.12 flags its whole video block restart-required; ours takes effect on the
//! click, the same stated departure [`apply_present_mode`] already makes for `gxVSync`.
//!
//! # VSync
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
use bevy::window::{MonitorSelection, PresentMode, PrimaryWindow, WindowMode, WindowResolution};

/// `$WOW_NOVSYNC=1` — the session-only measurement override. It wins over the config for the run
/// and never reaches `config.toml` (registered in [`crate::cvars`]'s `env_overridden` set), so a
/// headless FPS-journal run can uncap without making the player's setting sticky.
pub(crate) fn novsync_env() -> bool {
    std::env::var("WOW_NOVSYNC").as_deref() == Ok("1")
}

/// Does this run **size its own window**, and therefore stay windowed whatever the config says?
///
/// Three sources, and the first two are named explicitly rather than left to the third because a
/// capture that silently went fullscreen would render at the display's resolution instead of the
/// scenario's — every visual regression diff in the tree is denominated in the window the scenario
/// asks for, so this one must not depend on the machine's panel. The third is the general rule: an
/// instrumented run's window is plumbing ([`benilla_world::bgwin`]), and the probe fleet sizes and
/// parks it deliberately (decisions 0703/0709/1148).
///
/// Session-only, exactly like [`novsync_env`]: `gxWindow`/`gxResolution` are registered
/// env-overridden while it holds, so the file's value neither reaches the window nor is saved over.
pub(crate) fn windowed_env() -> bool {
    std::env::var_os("WOW_WIN").is_some()
        || std::env::var_os("WOW_CAPTURE").is_some()
        || std::env::var_os("WOW_CAPTURE_UI").is_some()
        || benilla_world::bgwin::background_run()
}

/// `$WOW_WIN=WxH` in **logical** px — the one parser, so the window that is *asked for* and the
/// window that is *checked* can never drift. It was spelled twice inline in `lib.rs` (once for the
/// UI-fixture arm, once for the world arm) and is now spelled here once and read there.
pub(crate) fn requested_window_size() -> Option<UVec2> {
    let v = std::env::var("WOW_WIN").ok()?;
    let (w, h) = v.split_once('x')?;
    Some(UVec2::new(w.parse().ok()?, h.parse().ok()?))
}

/// `$WOW_DPI=<f32>` — **render at a player's pixel grid, not this machine's.**
///
/// Every session here runs on a 2× panel; nearly every text-under-scale report the channel files
/// comes from a 1080p/1440p one at 1×. That gap is not cosmetic. The two places our text meets the
/// grid — the raster size (`TextEngine::ppem`, `round(logical × dpi)`) and the per-block vertical
/// snap (`ui_text::layout::snap_block_top`) — are both *quantizers*, and a quantizer's error is
/// denominated in device pixels: at 1× the same layout rounds twice as coarsely as it does here.
/// A defect that is half a pixel at 2× is a whole one at 1×, which is the difference between
/// invisible and reported (B209, B231, B232 — all from 1×, all reproduced here only by forcing
/// this).
///
/// The override goes on the *window*, so `Window::scale_factor()` — the one number the text engine,
/// the vplates raster and the world backdrop all read — answers with it, and `WOW_WIN` then means
/// physical pixels one-to-one. The image a capture writes is byte-for-byte the framebuffer that
/// player's GPU would scan out; on this display it is simply drawn at half the size.
///
/// Absent, nothing changes: the window keeps whatever the display reports.
pub(crate) fn requested_dpi() -> Option<f32> {
    let v: f32 = std::env::var("WOW_DPI").ok()?.parse().ok()?;
    (v.is_finite() && v > 0.0).then_some(v)
}

/// Apply [`requested_dpi`] to a window resolution — the one place the knob is spent, so the size
/// that is asked for and the grid it is asked for on cannot drift apart.
pub(crate) fn at_requested_dpi(res: WindowResolution) -> WindowResolution {
    match requested_dpi() {
        Some(dpi) => res.with_scale_factor_override(dpi),
        None => res,
    }
}

/// The display modes benilla ships. **Two, and neither is the reference's mode-setting
/// fullscreen** — the module doc says why.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum DisplayMode {
    /// Borderless, filling the monitor. `gxWindow "0"` — the reference's own default value, and
    /// what every shipped game defaults to.
    #[default]
    Fullscreen,
    /// A window at [`VideoConfig::windowed`]. `gxWindow "1"`.
    Windowed,
}

/// The windowed size a fresh config gets: the 1600×900 that was the client's only size before
/// 1627, so nothing about a windowed run moved when the mode setting landed.
pub(crate) const DEFAULT_WINDOWED: UVec2 = UVec2::new(1600, 900);

/// `gxWindow`'s value → the mode. The reference's own parse for its 0/1 CVars is int + `!= 0`, and
/// one function is the whole law so [`crate::cvars`]'s arm and the boot read cannot drift.
pub(crate) fn display_from_flag(v: f32) -> DisplayMode {
    if v != 0.0 {
        DisplayMode::Windowed
    } else {
        DisplayMode::Fullscreen
    }
}

/// `gxResolution`'s value → a size. The reference's spelling (`"1280x800"`), whose own parse is
/// `sscanf("%d%c%d")`; ours is stricter by one thing only — a zero extent is refused rather than
/// handed to the windowing system.
pub(crate) fn parse_resolution(value: &str) -> Option<UVec2> {
    let (w, h) = value.split_once(['x', 'X'])?;
    let size = UVec2::new(w.trim().parse().ok()?, h.trim().parse().ok()?);
    (size.x > 0 && size.y > 0).then_some(size)
}

/// The `WindowMode` a display mode means, on a given monitor.
pub(crate) fn window_mode(display: DisplayMode, monitor: MonitorSelection) -> WindowMode {
    match display {
        DisplayMode::Fullscreen => WindowMode::BorderlessFullscreen(monitor),
        DisplayMode::Windowed => WindowMode::Windowed,
    }
}

/// The mode the primary window is **born** in — resolved before the `App` exists, because the
/// alternative is worse than the frame it costs. Booting windowed and flipping at `Startup` (where
/// [`crate::cvars`]'s `load_config` runs) is a visible flash on every launch, and under a
/// compositor that only maps a fullscreen surface 1:1 it is a first second spent in exactly the
/// broken input state 1627 exists to end.
///
/// `MonitorSelection::Primary`, not `Current`: at creation there is no current monitor —
/// `bevy_winit::select_monitor` warns and answers `None` for `Current` — and "the monitor the
/// window is on" is not a question with an answer before the window exists. A live toggle uses
/// `Current`; [`apply_window_mode`] carries why the two never fight.
pub(crate) fn boot_window_mode() -> WindowMode {
    let display = if windowed_env() {
        DisplayMode::Windowed
    } else {
        crate::cvars::boot_cvar("gxWindow")
            .and_then(|v| v.parse::<f32>().ok())
            .map_or_else(DisplayMode::default, display_from_flag)
    };
    window_mode(display, MonitorSelection::Primary)
}

/// The windowed size the primary window is **born** at — `gxResolution`, read at the same pre-`App`
/// moment and for the same reason as [`boot_window_mode`]. Ignored by winit while the mode is
/// fullscreen (`bevy_winit` applies an inner size only on the `Windowed` arm), and the value
/// [`apply_window_mode`] hands back on the way out of it.
pub(crate) fn boot_windowed_size() -> UVec2 {
    crate::cvars::boot_cvar("gxResolution")
        .and_then(|v| parse_resolution(&v))
        .unwrap_or(DEFAULT_WINDOWED)
}

/// The video knobs a CVar write can land on. Default = what the client ships: fullscreen, synced,
/// matching the primary window's own boot values — see `lib.rs`'s window literal.
///
/// The **file** is deliberately not read here, only the environment (the posture `vsync` has had
/// since 0294): the window literal resolves env-then-file for itself, `load_config` applies the
/// file to this resource at `Startup`, and because both read the same key the reconcile is a no-op
/// rather than a mode change one frame into the run.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct VideoConfig {
    pub(crate) vsync: bool,
    pub(crate) display: DisplayMode,
    /// The windowed size, `gxResolution`. Kept while fullscreen so leaving it can restore it.
    pub(crate) windowed: UVec2,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            vsync: !novsync_env(),
            display: if windowed_env() {
                DisplayMode::Windowed
            } else {
                DisplayMode::default()
            },
            windowed: DEFAULT_WINDOWED,
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
            .add_systems(Startup, (log_display_session, check_window_pinned).chain())
            .add_systems(Update, (apply_present_mode, apply_window_mode));
    }
}

/// **Did the window actually get the size `$WOW_WIN` asked for?** Refuse the run if not.
///
/// The window manager is free to clamp a requested inner size to the display, and macOS does. The
/// failure is silent and it invalidates comparisons: on 2026-08-26 an MSAA A/B produced one leg at
/// 3200x1800 and the other at 3024x1800, because the director had the laptop panel on overnight
/// instead of the external monitor, and the window landed on a smaller screen. Nothing said so.
/// `benilla-visual` refused the pair with `image size mismatch` — the right outcome by luck, from a
/// tool three steps downstream that could only report the symptom, and a full A/B cycle was spent
/// getting there.
///
/// Measured, not assumed: `WOW_WIN=4000x3000` on this machine yields `1920x1048 logical` — clamped
/// to the display the window happened to open on, minus its chrome.
///
/// **Fatal under a capture, a warning otherwise**, and the split is the point. Every visual
/// regression diff in the tree is denominated in the window the scenario asks for
/// ([`windowed_env`]'s doc says so), so a capture at the wrong size is not a worse capture, it is
/// an invalid one that will be diffed against a valid one. A non-capture run with `$WOW_WIN` is
/// somebody looking at something; a clamp there is surprising, not wrong, so it says so and
/// carries on.
fn check_window_pinned(
    windows: Query<&Window, With<PrimaryWindow>>,
    mut exit: MessageWriter<AppExit>,
) {
    let Some(want) = requested_window_size() else {
        return;
    };
    let Ok(window) = windows.single() else {
        return;
    };
    let got = UVec2::new(
        window.resolution.width() as u32,
        window.resolution.height() as u32,
    );
    if got == want {
        return;
    }
    let capturing =
        std::env::var_os("WOW_CAPTURE").is_some() || std::env::var_os("WOW_CAPTURE_UI").is_some();
    if !capturing {
        warn!(
            "window: asked for {}x{} logical, got {}x{} — the window manager clamped it to the \
             display. Harmless here; it would invalidate a capture.",
            want.x, want.y, got.x, got.y
        );
        return;
    }
    error!(
        "window: REFUSING this capture — asked for {}x{} logical, got {}x{}. The window manager \
         clamped the request to the display this window opened on, so the image would not be the \
         size the scenario is denominated in and any diff against it would be meaningless. Use a \
         size that fits the current display (or move the window to a bigger one) and re-run.",
        want.x, want.y, got.x, got.y
    );
    exit.write(AppExit::error());
}

/// One line at boot naming what the window actually got — and, on a Linux session, what it is
/// talking to.
///
/// **The instrument half of 1627**, and it is not decoration. Every display and input report this
/// client has had from a Linux player came from a machine nobody here can run: the Steam Deck
/// perf reports behind 1624 and 1626, and the gamescope input report 1627 answers, were all
/// diagnosed from prose. `bevy_winit` already logs the monitor, its scale factor and its refresh
/// rate at creation; the two things it never says are which **backend** winit picked and whether a
/// nested compositor is in the way — and a surprising amount hangs on the first of those.
/// `CursorGrabMode::Locked`, which is what mouse-look asks for, is a real pointer lock on Wayland
/// and is **rejected outright on X11** (`winit`'s x11 `set_cursor_grab` returns `NotSupported`),
/// where `bevy_winit::attempt_grab` quietly falls back to `Confined`. "Which one am I on" should
/// never again be a thing we reason about instead of read.
///
/// Not `#[cfg(feature = "dev")]`: the whole point is that it is in the log a *player* pastes.
fn log_display_session(windows: Query<&Window, With<PrimaryWindow>>) {
    let Ok(window) = windows.single() else {
        return;
    };
    let res = &window.resolution;
    info!(
        "video: {:?}, {}x{} logical / {}x{} physical (scale {}){}",
        window.mode,
        res.width(),
        res.height(),
        res.physical_width(),
        res.physical_height(),
        res.scale_factor(),
        display_session(),
    );
}

/// The display-server facts worth naming, as a trailing clause. Empty everywhere but a Linux/BSD
/// session, where the same `cfg` the Wayland clipboard uses (decision 0702) marks the one platform
/// whose windowing backend is a runtime choice rather than the only one there is.
fn display_session() -> String {
    #[cfg(all(unix, not(any(target_os = "macos", target_os = "android"))))]
    {
        let set = |k: &str| std::env::var_os(k).is_some();
        // Which backend winit took: it prefers Wayland when `WAYLAND_DISPLAY` names a socket and
        // falls back to X11 (which, under a Wayland compositor, means XWayland). Both features are
        // on — `bevy`'s `default_platform` carries `x11` *and* `wayland`, so this really is a
        // runtime pick and not a build-time one.
        let backend = match (set("WAYLAND_DISPLAY"), set("DISPLAY")) {
            (true, _) => "wayland",
            (false, true) => "x11",
            (false, false) => "none",
        };
        // gamescope exports its own socket name to children; the SteamOS session also stamps the
        // Deck. Named because a nested compositor is the difference between "the window is wrong"
        // and "the window is right and the compositor is scaling it".
        let nested = if set("GAMESCOPE_WAYLAND_DISPLAY") {
            ", gamescope"
        } else {
            ""
        };
        let deck = if set("SteamDeck") { ", steamdeck" } else { "" };
        return format!(
            " [{backend}{nested}{deck}, XDG_SESSION_TYPE={}]",
            std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unset".into()),
        );
    }
    #[cfg(not(all(unix, not(any(target_os = "macos", target_os = "android")))))]
    String::new()
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

/// The display-mode half, on the same change-gated shape as [`apply_present_mode`] and for the same
/// two reasons — with one wrinkle of its own.
///
/// **The guard asks whether the window is fullscreen, never on which monitor.** Birth uses
/// `MonitorSelection::Primary` (nothing else has an answer yet) and a live toggle uses `Current`
/// (fill the monitor the window is actually on), so the two `WindowMode`s that both mean
/// "fullscreen" are not equal values, and a `!=` compare would re-assert fullscreen on the first
/// frame of every launch. Matching on the variant is the honest question.
fn apply_window_mode(
    cfg: Res<VideoConfig>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    mut last: Local<Option<DisplayMode>>,
) {
    if last.replace(cfg.display) == Some(cfg.display) {
        return;
    }
    let Ok(mut window) = windows.single_mut() else {
        return;
    };
    let want = window_mode(cfg.display, MonitorSelection::Current);
    let already = matches!(
        (&window.mode, &want),
        (WindowMode::Windowed, WindowMode::Windowed)
            | (
                WindowMode::BorderlessFullscreen(_),
                WindowMode::BorderlessFullscreen(_)
            )
    );
    if already {
        return;
    }
    // Leaving fullscreen has to hand the size back, because entering it **overwrote**
    // `window.resolution` with the monitor's — `bevy_window`'s own documented behaviour for
    // `BorderlessFullscreen` — so a bare mode flip returns a monitor-sized "window". `gxResolution`
    // is the record of what windowed means, which is exactly why it persists.
    //
    // Both writes land in one frame on purpose: `bevy_winit::changed_windows` applies `mode` before
    // `resolution` in the same pass, so the inner size is set after the window has left fullscreen.
    if cfg.display == DisplayMode::Windowed {
        window
            .resolution
            .set(cfg.windowed.x as f32, cfg.windowed.y as f32);
    }
    window.mode = want;
    info!("video: display mode {:?} ({want:?})", cfg.display);
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

    /// The shipped display mode is fullscreen, and fullscreen is **borderless** — the whole point
    /// of 1627. Welded as an assertion because `WindowMode::Fullscreen` is one word away and is the
    /// mode that is ignored on Wayland and panics without a monitor.
    #[test]
    fn the_default_is_borderless_fullscreen_not_exclusive() {
        assert_eq!(DisplayMode::default(), DisplayMode::Fullscreen);
        assert!(matches!(
            window_mode(DisplayMode::Fullscreen, MonitorSelection::Primary),
            WindowMode::BorderlessFullscreen(_)
        ));
        assert_eq!(
            window_mode(DisplayMode::Windowed, MonitorSelection::Primary),
            WindowMode::Windowed
        );
    }

    /// `gxWindow` reads like every other 0/1 CVar in the tree: int + `!= 0`, with `"1"` the
    /// *windowed* state — the reference's polarity, which is why the row is "Windowed Mode" and
    /// not "Fullscreen".
    #[test]
    fn gxwindow_one_is_windowed() {
        assert_eq!(display_from_flag(0.0), DisplayMode::Fullscreen);
        assert_eq!(display_from_flag(1.0), DisplayMode::Windowed);
    }

    /// The mode round-trip, driven through the **real system** rather than around it — because the
    /// two things that are easy to get wrong here are both invisible to a single live run.
    ///
    /// 1. A window born fullscreen must not be re-asserted on the first frame. Birth carries
    ///    `MonitorSelection::Primary` and a live toggle carries `Current`, so the two `WindowMode`s
    ///    that both mean "fullscreen" are unequal *values*; a `!=` guard would fire every launch.
    /// 2. Leaving fullscreen must hand `gxResolution` back. Entering it overwrote
    ///    `window.resolution` with the monitor's, so a bare mode flip returns a monitor-sized
    ///    "window" — and on the 3440-wide panel B242/1619 came from, that is not a subtle miss.
    #[test]
    fn the_fullscreen_round_trip_keeps_the_monitor_out_of_the_windowed_size() {
        let mut app = App::new();
        app.insert_resource(VideoConfig {
            vsync: true,
            display: DisplayMode::Fullscreen,
            windowed: UVec2::new(1024, 768),
        })
        .add_systems(Update, apply_window_mode);
        let win = app
            .world_mut()
            .spawn((Window::default(), PrimaryWindow))
            .id();
        // Born the way `lib.rs` builds it: already fullscreen, on the boot monitor selection.
        app.world_mut()
            .entity_mut(win)
            .get_mut::<Window>()
            .unwrap()
            .mode = window_mode(DisplayMode::Fullscreen, MonitorSelection::Primary);

        app.update();
        assert!(
            matches!(
                app.world().entity(win).get::<Window>().unwrap().mode,
                WindowMode::BorderlessFullscreen(MonitorSelection::Primary)
            ),
            "an already-fullscreen window must not be re-asserted onto another monitor selection"
        );

        // Stand in for the compositor's half of being fullscreen — `bevy_window` documents that
        // the resolution becomes the monitor's — then leave.
        app.world_mut()
            .entity_mut(win)
            .get_mut::<Window>()
            .unwrap()
            .resolution
            .set(3440.0, 1440.0);
        app.world_mut().resource_mut::<VideoConfig>().display = DisplayMode::Windowed;
        app.update();
        let w = app.world().entity(win).get::<Window>().unwrap();
        assert_eq!(w.mode, WindowMode::Windowed);
        assert_eq!(
            (w.resolution.width(), w.resolution.height()),
            (1024.0, 768.0),
            "leaving fullscreen restores gxResolution, never the monitor's size"
        );

        // And back in, which must reach `Current` — the monitor the window is on by then, not the
        // one it was born on.
        app.world_mut().resource_mut::<VideoConfig>().display = DisplayMode::Fullscreen;
        app.update();
        assert!(matches!(
            app.world().entity(win).get::<Window>().unwrap().mode,
            WindowMode::BorderlessFullscreen(MonitorSelection::Current)
        ));
    }

    /// `gxResolution` is the reference's `"WxH"` string, and a value that cannot be a window is
    /// refused rather than passed on.
    #[test]
    fn gxresolution_parses_the_reference_spelling() {
        assert_eq!(parse_resolution("1280x800"), Some(UVec2::new(1280, 800)));
        assert_eq!(parse_resolution("1600X900"), Some(UVec2::new(1600, 900)));
        assert_eq!(parse_resolution("1024 x 768"), Some(UVec2::new(1024, 768)));
        assert_eq!(parse_resolution("0x600"), None);
        assert_eq!(parse_resolution("1280x"), None);
        assert_eq!(parse_resolution("fullscreen"), None);
    }
}
