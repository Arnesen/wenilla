//! Time-of-day lighting, driven by `Light.dbc` sampled against the server game-clock. **Lighting
//! rebuild — Phase 0 (pitch black).** This module now only *resolves the faithful light values* —
//! [`WowLighting`] (ambient/diffuse/specular colors + the vanilla `DayNight::SetDirection` sun
//! direction) — and pushes them onto the terrain/model materials. It no longer spawns a PBR sun /
//! ambient / distance fog / sky; the faithful lighting is rebuilt in-shader, one verified step at a
//! time, on top of the Phase-0 black baseline.

use bevy::prelude::*;

use crate::assets::AssetSet;
use benilla_formats::{LightCatalog, LiquidKind};

mod daynight; // the two sun directions + day/night interp + the dawn/dusk warp curve
mod global_light; // the one shared global-light storage buffer (replaces the per-material push)
mod prop_probes; // the per-instance interior-prop SH probe table (slot ↔ MeshTag payload)
mod resolve; // the per-frame time-of-day sample into WowLighting + the WMO interior-fog crossfade
mod sh; // the model SH light-probe coefficient math
pub(crate) use global_light::{
    commit_raw, light_blob_bytes, new_shared_light_buffer, pack_model_core_rows, SharedLightBuffer,
    LIGHT_HEADER_ROWS,
};
pub(crate) use prop_probes::{
    prop_probe_region_offset, PropProbeSlot, PropProbes, MAX_PROP_PROBES,
};
pub(crate) use resolve::fog_range;
use resolve::{apply_sky_backdrop, setup_lighting, update_time_lighting};
pub(crate) use sh::prop_probe_coeffs;

/// Scene lighting sampled from `Light.dbc` for the current time of day — fed into the terrain
/// shader's WoW lighting. Colors are sRGB 0..1; `sun_dir` is the world-space (Bevy) direction the
/// sun's light travels. Rewritten every frame by [`update_time_lighting`] as the clock advances.
#[derive(Resource, Clone, Copy, Default, PartialEq)]
pub(crate) struct WowLighting {
    pub(crate) ambient: [f32; 3],
    pub(crate) diffuse: [f32; 3],
    /// Specular highlight color — the DBC sun-halo color (row 9, warm-white). Drives the terrain
    /// sheen (Step 3b) and, later, model highlights (Step 7).
    pub(crate) spec: [f32; 3],
    pub(crate) sun_dir: Vec3,
    /// The **visible celestial sun** direction (camera→sun, Bevy space) — a *separate* body from the
    /// lighting `sun_dir`. Drives the sun-disc/glow sprite (`sun.rs`). Unlike the near-fixed lighting
    /// sun, this one genuinely rises and sets in elevation over the day (see [`daynight::celestial_sun_direction`]).
    pub(crate) celestial_dir: Vec3,
    /// **Step 5 fog** — gamma-space distance fog, per the q6 RE. Color = `Light.dbc` IntBand
    /// **row 7** raw (no sky-blend, no sun-glow). `GL_LINEAR`, applied in-shader in gamma space
    /// BEFORE the sRGB store cancel — NOT Bevy's `DistanceFog`, which blends in linear and breaks
    /// the gamma invariant.
    pub(crate) fog_color: [f32; 3],
    /// The **pushed** GL fog pair from [`resolve::scene_fog`] — `end = min(raw band end, farclip)`,
    /// `start = frac × end` **unclamped**: negative under storm (Elwynn frac −0.5), which is the
    /// reference's constant ~33% near veil in rain (wow-re `rf-weather-fog-veil.md`).
    pub(crate) fog_start: f32,
    pub(crate) fog_end: f32,
    /// The **interior** fog triple (DNState+0x80/84/88): `lerp(scene fog → the claimed WMO's MFOG
    /// fog, t)` on the 4 s camera-in-WMO ramp ([`resolve::WmoFogRamp`]) — equal to the scene triple while
    /// the camera is outside. Consumed ONLY by the interior lanes (round-6 Q-I consumer map): the
    /// interior WMO-group surfaces and THAT group's doodads (`0x6b5190` / the group-doodad drawer
    /// `0x6b62e0`) — `wow_model.wgsl` selects it by the material interior flag. Terrain, liquid,
    /// sky, exterior groups, and world units keep the scene fog — the storm stays grey through the
    /// inn's open door (director ref-shot, 2026-07-13).
    pub(crate) wmo_fog_color: [f32; 3],
    pub(crate) wmo_fog_start: f32,
    pub(crate) wmo_fog_end: f32,
    /// The five sky-dome gradient stops, zenith→horizon (`Atmosphere.sky`, LightIntBand rows 2–6).
    /// Fed to the `SkyPlugin` dome material; interpolated across the dome by elevation.
    pub(crate) sky: [[f32; 3]; 5],
    /// **Per-kind water-surface tint** `[shallow, deep]` — `Atmosphere.water_river` (IntBand rows
    /// 16/17) and `.water_ocean` (rows 14/15), RAW, resolved from the **area-light blend**, exactly
    /// like every other band: the client's gather record carries all 18 colour rows and
    /// `dn_record_overblend 0x6d30e0` merges all 18 per light (rows 14–17 are its `+0x34..+0x40`
    /// step-9 loop) — there is no single-sphere pick in the water path (decision 1104, superseding
    /// the `pick_light` split whose discontinuity snapped the tint at Tirisfal→Silverpine). The
    /// from-above depth swatch is a 2-endpoint linear lerp of these by the per-vertex depth `V`
    /// (river/lake `V = clamp(byte/42)`; VERIFIED `WoW.exe FUN_0068a830` + `c81768`). Pushed onto the
    /// per-kind liquid materials by [`apply_wow_lighting`] via [`WowLighting::water_colors`].
    pub(crate) water_river: [[f32; 3]; 2],
    pub(crate) water_ocean: [[f32; 3]; 2],
    /// **Per-kind water-blend alphas** `[shallow, deep]` — `LightParams.water/oceanShallow/DeepAlpha`,
    /// blended across the same spheres as the tint. The swatch's depth-alpha ramp endpoints. Per-zone:
    /// Elwynn/Loch Modan shallow ≈0.5, STV ≈0.85; deep ≈1.0. Replaces the hardcoded `*_SHALLOW_ALPHA`
    /// constants for the live path (they remain as the `setup_liquid` frame-0 seed + DBC-absent default).
    pub(crate) water_river_alpha: [f32; 2],
    pub(crate) water_ocean_alpha: [f32; 2],
    /// **Per-zone FFXGlow/bloom composite weight** — `LightParams.glow`, quantised `floor(g·255)/255`
    /// like the real client. Read by
    /// `glow.rs`'s `sync_glow_weight` as the faithful default for `GlowSettings.weight` (the panel
    /// slider is an override). Elwynn ≈ 0.647, Duskwood ≈ 0.498; fallback 0.5.
    pub(crate) glow: f32,
    /// **Dawn/dusk sky-dome warp strength** `S` — `dawn_dusk_curve(dayfrac) × highlightSky`, in
    /// `[0,1]`. Drives the per-zone azimuthal warp in `sky.wgsl` (`FUN_006d0f50`): the sun-facing
    /// quarter of the dome warms toward SkyColor0, the away side desaturates toward SkyColor1. The
    /// curve is **0 across all of midday and deep night** (spikes to 1 only at ~06:30/21:30), so this
    /// is 0 except at dawn/dusk — and 0 entirely in highlightSky=0 zones (Duskwood). At `S=0` the
    /// warp is identity, so daytime sky stays byte-faithful. See [`daynight::sky_warp`].
    pub(crate) sky_warp: f32,
    /// **Sun disc size multiplier** — `sun_disc_scale(dayfrac)` (vanilla size table `0xce8cac`): `1.0`
    /// across midday, up to `2.0` at the dawn/dusk horizon (the "huge sun at the horizon"). `sun.rs`
    /// multiplies the disc's base angular size (`sun_size`) by this. See [`daynight::sun_disc_scale`].
    pub(crate) sun_disc_scale: f32,
    /// **Sun lens-flare day/night envelope** — the per-body dnCurve table (`0xce9818`): `1.0` across
    /// the day (07:30→19:30), `0.0` all night with dawn/dusk dead-bands (off by 21:00, back at
    /// 06:30→07:30). A factor of the flare intensity's slew target in `sun::follow` (wow-re
    /// celestial-bodies Addendum #5, decision 0508). See [`daynight::sun_flare_dn`].
    pub(crate) sun_flare_dn: f32,
    /// **Visible moon direction** (Bevy camera→moon) — the white moon at azimuth 45° (the sun's
    /// bearing), up at night and below the horizon by day (`daynight::moon_direction`). The engine's
    /// second disc (`moon02.blp`) IS drawn but vertex-black (its colour field has no writer in the
    /// binary) at azimuth 135–165° on a phase-precessed schedule — never a visible second moon
    /// (decision 0485). Consumed by the moon billboards in `sun`.
    pub(crate) moon_dir_white: Vec3,
    /// **Moon disc size multiplier** — `1.0` overhead (~midnight) → `1.5` at moonrise/set (size table
    /// `0xce8c8c`); `sun` multiplies by the white moon's base ×1.75.
    pub(crate) moon_disc_scale: f32,
    /// **Moon lens-flare night envelope** — the moon's dnCurve table (`0xce9768`): flat `0.0` from
    /// 03:15 all the way to 22:45 (the whole day + early evening), ramping in 22:45→24:00, full
    /// 00:00→02:00. The moon's halo simply does not exist at a 22:30 moonrise (wow-re Addendum #5,
    /// decision 0508). See [`daynight::moon_flare_dn`].
    pub(crate) moon_flare_dn: f32,
    /// **moon02 direction** (Bevy camera→body) — the engine's third disc, drawn vertex-BLACK on its
    /// phase-precessed 1.7-day clock ([`daynight::moon02_state`]; decision 0485). Never a visible
    /// second moon; faithfully occludes stars behind it.
    pub(crate) moon_dir_02: Vec3,
    /// **moon02 size multiplier** — the shared `0xce8c8c` curve sampled on moon02's own phase
    /// clock (base ×1.0).
    pub(crate) moon02_disc_scale: f32,
    /// **Star-field global alpha** — `star_alpha(dayfrac)` (vanilla star curve `0xce9a98`): `1.0` deep
    /// night → `0.0` all day (fade in 22:30→00:00, out 03:00→04:30). `sun.rs` multiplies the star dome's
    /// base-colour alpha by this (the reference's model-global star fade). See [`daynight::star_alpha`].
    pub(crate) star_alpha: f32,
    /// **SIDN night fraction** (`DNState+0x1ac`, track `0xce9a34`): `1.0` overnight → `0.0` all day
    /// (ramps 20:30→21:30 / 06:00→07:00). `wow_model.wgsl` multiplies every WMO SIDN material's
    /// authored emissive colour by it — the windows-glow-at-night ramp. See
    /// [`daynight::sidn_night_fraction`].
    pub(crate) sidn_night: f32,
    /// **Celestial diffuse tint** (sRGB) — the one DayNight colour the client broadcasts into the sun
    /// disc, sun glare, white-moon disc, and moon glare every frame (`[0xce9c2c]`, broadcast by
    /// `0x6d2260`; decision 0485). **LightIntBand sub-9** — byte-pinned: the band gather `0x6d64d0`
    /// swaps positions 8/9 so table[8] = sub-9, whose alpha is forced 0xFF (`0x6d62e0`); the same
    /// row [`Self::spec`] samples (warm cream at night — matching the reference trace's moon VBO
    /// (254,240,228) — orange at dawn/dusk). The `sun` follow systems rewrite the disc/glare
    /// material tints from it per frame; the moon's TEAL rim is NOT this tint but the dome's teal
    /// night bands through the disc's feathered edge.
    pub(crate) celestial_tint: [f32; 3],
    /// **Authored cloud density `C`** — Light.dbc FloatBand sub-3, weather/area blends included.
    /// Drives the coverage-field threshold (`clouds`): 0 = cloudless, 1 = full overcast potential
    /// (wow-re `cloud-coverage-pipeline.md` §4).
    pub(crate) cloud_density: f32,
    /// **Cloud palette** `[sun-glow, slope, gbase]` — IntBand sub-10/11/12, the visible cloud
    /// dome's colors (wow-re `cloud-coverage-pipeline.md` §3c).
    pub(crate) cloud_colors: [[f32; 3]; 3],
    /// **Storm blend `bcc`** = `min(1, weather_density·4)` (`cloud_density_clamp 0x6d4500`) — the
    /// weight already lerping the storm LightParams over the clear one, published for the
    /// celestial-alpha seed (`floor(255·(1−bcc))` on the five body alphas, Addendum #6) and the
    /// cloud sun-glow dimming (`1 − 0.75·bcc`). Purely weather-driven — authored `C` never feeds
    /// it (wow-re `cloud-coverage-pipeline.md` §4).
    pub(crate) storm_bcc: f32,
    /// **Cloud glow body direction** (Bevy, camera→body) — the sun while the day fraction sits in
    /// ≈04:50–22:10, the moon otherwise (`0x6cfb00` setup; [`daynight::cloud_glow_is_sun`]). The
    /// cloud color pass projects it onto the coverage tile as the glow centre.
    pub(crate) cloud_glow_dir: Vec3,
    /// **Cloud glow track factor** — the internal 8-key day envelope (`0xce9ab8`, ≈1.0 all day,
    /// twilight notches; [`daynight::cloud_glow_track`]). Multiplied by the weather dim
    /// `1 − 0.75·bcc` to form the glow intensity.
    pub(crate) cloud_glow_track: f32,
}

impl WowLighting {
    /// Per-kind **water swatch endpoints**: `(shallow_rgb, deep_rgb, shallow_alpha, deep_alpha)`. The
    /// from-above depth swatch is a plain **2-endpoint linear lerp** of the zone's dedicated `Light.dbc`
    /// water rows — IntBand 16/17 (river/lake) or 14/15 (ocean), **RAW** (no ×0.711) — by the per-vertex
    /// depth `V` (river/lake `V = clamp(byte/42)`, built in `benilla-formats::liquid`). VERIFIED from WoW.exe
    /// `FUN_0068a830`, golden-vector-matched to the apitrace swatch (≤1/255 over all 64 rows). The shader
    /// (`liquid.wgsl`) lerps both colour and opacity by the *same* V, so they track together.
    ///
    /// Alpha endpoints are the **per-zone `LightParams` water-blend alphas** (`water_*_alpha`, decoded
    /// from fields 5–8): Elwynn/Loch Modan shallow ≈0.5, STV ≈0.85 (its shallows read pale from the
    /// colour, not transparency) — VERIFIED vs the apitrace swatch alpha + user-confirmed in-game.
    /// (The earlier "water reflects the sky × 0.711 via `FUN_0068c250`" derivation fingered the WRONG
    /// builder; the dedicated rows 14-17 we'd originally used were right.)
    ///
    /// The opacity ramp itself is indexed by the raw MCLQ depth byte (0 = shore → 255 = deep) with
    /// **no scale** — VERIFIED `WoW.exe FUN_006b6b60` builds `ca7f10[d] = shallow + d·(deep−shallow)/256`
    /// from `gWorldLight+0x114/+0x118`, corroborated by the apitrace swatch (`α = 127 + 2·row` ⇒
    /// 0.5→1.0 for river/lake, 0.75→1.0 for ocean). The alpha and the colour ride the SAME per-vertex
    /// `V`, and the river/lake `V` is the **steep `clamp(byte/42)`** (VERIFIED `c81768` LUT /
    /// `FUN_0068d790`) — NOT `byte/255`: a river channel saturates to opaque deep teal by **byte 42 ≈
    /// 5 yd** (ramp ≈8.5 byte/yd, VERIFIED `probe_water_depth`), leaving only the shore edge
    /// see-through. Ocean uses a different non-LUT path (placeholder `/255`, pending its own RE+A/B).
    /// (Earlier bugs: `×8 DEPTH_RAMP_SCALE` saturated at ~4 yd; then the gentle `byte/255` was the
    /// WRONG LUT and the river middle never reached teal.)
    pub(crate) fn water_colors(&self, kind: LiquidKind) -> ([f32; 3], [f32; 3], f32, f32) {
        let (shallow_rgb, deep_rgb, alpha) = if kind == LiquidKind::Ocean {
            (
                self.water_ocean[0],
                self.water_ocean[1],
                self.water_ocean_alpha,
            )
        } else {
            (
                self.water_river[0],
                self.water_river[1],
                self.water_river_alpha,
            )
        };
        (shallow_rgb, deep_rgb, alpha[0], alpha[1])
    }
}

/// Water surface **specular shininess** — the water material's Phong power for the sun sheen
/// (`ffp_material.shininess` at the traced water draw ≈ 6; terrain uses 20). Lower ⇒ a broader,
/// softer glint that spreads at grazing (sunrise/sunset) sun.
pub(crate) const WATER_SHININESS: f32 = 6.0;

/// Quantise a raw `LightParams.glow` (0..1) to the byte the reference packs into the composite-quad
/// colour: `floor(g·255)/255`. (Elwynn 0.65 → 0.647.)
pub(crate) fn quantize_glow(glow: f32) -> f32 {
    (glow * 255.0).floor() / 255.0
}

/// The parsed `Light.dbc` family, kept resident so [`update_time_lighting`] can re-sample it each
/// frame as the server clock advances. Absent if the DBCs failed to load (we fall back to a neutral
/// day). Loaded once in `setup_lighting`.
#[derive(Resource)]
pub(crate) struct LightSampler(pub(crate) LightCatalog);

/// Where the time-of-day currently driving lighting comes from — a readout for the debug panel.
#[derive(Default, Clone, Copy, PartialEq, Debug)]
pub(crate) enum ClockSource {
    /// Pre-connect (or DBC-less): hardcoded noon.
    #[default]
    Fallback,
    /// Live server game-clock (`SMSG_LOGIN_SETTIMESPEED`, advanced by its timescale).
    Server,
    /// Manually scrubbed in the debug panel.
    Manual,
}

/// The effective game-clock driving lighting this frame — written by [`update_time_lighting`], read
/// by the debug panel for its time readout.
#[derive(Resource, Default, Clone, Copy)]
pub(crate) struct GameClock {
    /// Minute of the game day (`0..1440`) being rendered.
    pub(crate) minute: u32,
    pub(crate) source: ClockSource,
}

/// The lighting subsystem: registers the WoW-light resource + game-clock, a **black** background
/// (Phase 0 — the DBC sky comes back in a later step), and the two per-frame systems that resolve the
/// `Light.dbc` values for the time of day and push them onto the materials.
pub(crate) struct LightingPlugin;

impl Plugin for LightingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WowLighting>()
            .init_resource::<PropProbes>()
            .init_resource::<GameClock>()
            .insert_resource(ClearColor(Color::BLACK))
            .add_systems(Startup, setup_lighting.after(AssetSet::Open))
            .add_systems(
                Update,
                (update_time_lighting, apply_sky_backdrop)
                    .chain()
                    // The storm blend reads this frame's weather densities (decision 0302).
                    .after(crate::weather::WeatherTick)
                    // The submerged atmosphere reads THIS frame's submersion verdict — unordered,
                    // the murk could arrive a frame after the eye went under (and out of step with
                    // the sky-pass suppression, which reads the same verdict).
                    .after(crate::liquid::SubmersionVerdict),
            );
        // The shared global-light buffer (build_light_data after the resolve above; the extract +
        // render-world upload). Materials read this instead of carrying their own light copy.
        global_light::register(app);
    }
}
