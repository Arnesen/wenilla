//! The debug **state** — resource-only, no egui: the per-subsystem dev toggles the render and
//! gameplay systems read, with **player-faithful defaults** (fog on, dome on, everything visible,
//! follow the server clock, no weather override). Decision 0026's "config out of the editor":
//! this file is the always-present layer; the panel (`super`) is only the *editor* that writes
//! it. When the compile-time `dev` seam lands (0026's deferred phase), this module stays in a
//! player build — the defaults ARE the player behaviour — and only the egui surfaces gate out.

use bevy::prelude::*;

/// Root debug state. One resource, grouped into per-subsystem sections. Defaults: panel hidden
/// (`open: false`), each section its own `Default` (so this derives cleanly).
#[derive(Resource, Default)]
pub struct DebugState {
    /// Panel visible? Hidden by default; toggled with the backtick key.
    pub open: bool,
    pub models: ModelDebug,
    pub lighting: LightingDebug,
    pub sound: SoundDebug,
    pub weather: WeatherDebug,
}

/// Weather-instrument state: a panel-armed override that drives the same `WeatherState::apply`
/// path as the wire (decision 0302), so any type/grade transition can be exercised without a GM
/// `.wchange`. While `force` is on, wire weather is consumed and ignored.
#[derive(Default)]
pub struct WeatherDebug {
    /// Override armed: the scrub below substitutes for the wire.
    pub force: bool,
    /// One-shot: the scrub changed — re-apply it (set by the panel, taken by `weather_tick`).
    pub dirty: bool,
    /// Wire weather type (0 fine / 1 rain / 2 snow / 3 sand).
    pub kind: u32,
    /// Wire grade (0..1).
    pub grade: f32,
    /// Apply instantly (the wire's `instant` flag) instead of the ramp.
    pub instant: bool,
}

/// Sound-instrument state: the kit probe (the real play path — variation pick + per-shot
/// variation + category mix), consumed by `sound::kit`. The *config* (enable, master volume)
/// lives in [`crate::sound::SoundConfig`] — always-on player state per decision 0026; the panel
/// only edits it. (The pre-kit raw-path probes retired once the kit player became the one real
/// path — the kit probe exercises device + MPQ + decode end-to-end anyway.)
pub struct SoundDebug {
    /// A `SoundEntries` kit id or name for the "Play kit" probe.
    pub kit_query: String,
    /// One-shot: play `kit_query` through the kit player.
    pub play_kit: bool,
}

impl Default for SoundDebug {
    fn default() -> Self {
        Self {
            // A UI kit with several variations — exercises the depleting weighted pick.
            kit_query: "igMiniMapZoomIn".into(),
            play_kit: false,
        }
    }
}

/// Scene lighting controls. Colors come from `Light.dbc` sampled at the current time of day (the
/// **server** game-clock by default). The lighting section is
/// mostly a readout — the resolved DBC values + sun direction at the current time — plus the time
/// scrub and a fog-disable toggle. (The tone-gap discovery knobs were removed once that investigation
/// closed; the faithful path is the only path now.)
pub struct LightingDebug {
    /// Follow the live server game-clock (default). When `false`, scrub time with `manual_minute`.
    pub follow_server_time: bool,
    /// Manually-set minute of the game day (`0..1440`), used when `follow_server_time` is off.
    pub manual_minute: u32,
    /// Disable the `Light.dbc` distance fog on terrain + models (debug). The sky-dome horizon colour
    /// is unaffected (it's the sky band, not the distance fog). Default `false` = faithful fog on.
    pub disable_fog: bool,
    /// Hide the gradient sky dome (debug). Default `false` = dome shown (faithful). Useful for A/B and
    /// for the FPS-debug Performance toggles.
    pub disable_sky_dome: bool,
}

impl Default for LightingDebug {
    fn default() -> Self {
        // `WOW_CLOCK=<minute 0..1439>` arms the manual scrub from the environment — the
        // matched-hour capture instrument (a headless probe can't reach the panel slider, and a
        // time-of-day A/B against a reference screenshot is meaningless at the wrong hour —
        // bug B33's whole diagnosis hinged on one). Unset = follow the server clock, as before.
        let clock = std::env::var("WOW_CLOCK")
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .map(|m| m % 1440);
        Self {
            follow_server_time: clock.is_none(),
            manual_minute: clock.unwrap_or(720), // noon, only used when not following the server
            disable_fog: false,
            disable_sky_dome: false,
        }
    }
}

/// Toggles for the world-model render: per-layer (blend) and per-type visibility (a scene
/// inspector). Material values (alpha-key cutoff, two-sided culling, and the WoW lighting) are
/// baked in `model_material` / driven from the Lighting section.
pub struct ModelDebug {
    /// Visible flags indexed by `ModelKind::index` (see `super`).
    pub kind_visible: [bool; 4],
    /// Visible flags indexed by `blend_index` (see `super`).
    pub blend_visible: [bool; 5],
    /// WMO portal visibility culling on/off (decision 0031). On = the faithful per-group PVS; off =
    /// every group of a building always drawn (the pre-portal behaviour). An A/B switch for the
    /// director: flip it off in the Trade District and the cathedral above reappears.
    pub portal_cull: bool,
}

impl Default for ModelDebug {
    fn default() -> Self {
        Self {
            kind_visible: [true; 4],
            blend_visible: [true; 5],
            portal_cull: true,
        }
    }
}
