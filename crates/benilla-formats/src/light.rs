//! Authentic vanilla atmosphere from the client's lighting DBCs — so fog distance/color and
//! sun/ambient tint come from Blizzard's authored data, not invented constants.
//!
//! Pipeline: [`Light.dbc`] gives a sphere per area (a world
//! position via [`dbc_to_world`] + a radius in yards) and a `LightParams` id per weather condition;
//! that id indexes the band tables — [`LightIntBand.dbc`] (18 color rows/param) and
//! [`LightFloatBand.dbc`] (6 float rows/param) — each row a value over time-of-day. We sample the
//! **clear-weather** param at a given time and return the fog/sun/ambient/sky values.
//!
//! **Verified against build 5875** (raw-byte decode cross-checked with wowdev.wiki, 2026-05-25):
//! headers are Light = 12 fields/48 B (5 param refs), both bands = 34 fields/136 B
//! (`ID, num, time[16], value[16]`). LightIntBand = 7668 = 426×18 rows and LightFloatBand =
//! 2556 = 426×6 rows — i.e. exactly 18 int / 6 float rows per LightParams, confirming the band row
//! id for param `P` (1-based), band index `b`: `(P-1)*per + b + 1` (== wiki `P*18−17` / `P*6−5`).
//! The int-row→meaning map is the **vanilla** layout (rows 2–6 = the 5 sky stops, 7 = fog, 9 = sun),
//! confirmed by the data itself (row 8 is flat gray = shadow-opacity slot, not a sun). The
//! decoded values match our parser exactly (e.g. Northshire/global-light param 12 noon SkyMiddle
//! = `(58,162,207)`).

use std::collections::HashMap;

use anyhow::{Context, Result};
use benilla_dbc::{FieldType, Schema, SchemaField};

use crate::dbc::{f32_at, parse, u32_at};
use crate::Chain;

mod atmosphere; // the resolved `Atmosphere` output type + the DBC row/field → meaning constants
mod tables; // raw LightIntBand/FloatBand decode + time-of-day interpolation

pub use atmosphere::Atmosphere;
use atmosphere::{
    FB_CLOUD_DENSITY, FB_FOG_END, FB_FOG_START_MULT, IB_AMBIENT, IB_CLOUD_GBASE, IB_CLOUD_SLOPE,
    IB_CLOUD_SUN, IB_DIFFUSE, IB_FOG_COLOR, IB_OCEAN_DEEP, IB_OCEAN_SHALLOW, IB_RIVER_DEEP,
    IB_RIVER_SHALLOW, IB_SKY0, IB_SUN_COLOR, LP_GLOW, LP_HIGHLIGHT, LP_OCEAN_DEEP_ALPHA,
    LP_OCEAN_SHALLOW_ALPHA, LP_WATER_DEEP_ALPHA, LP_WATER_SHALLOW_ALPHA,
};
use tables::{band_schema, load_bands, load_float_bands, sample_color, sample_float, Band, DAY};

const LIGHT: &str = "DBFilesClient\\Light.dbc";
const INT_BAND: &str = "DBFilesClient\\LightIntBand.dbc";
const FLOAT_BAND: &str = "DBFilesClient\\LightFloatBand.dbc";
const LIGHT_PARAMS: &str = "DBFilesClient\\LightParams.dbc";

/// World-unit scale factor on Light.dbc positions/radii (stored as yards × 36).
const POS_SCALE: f32 = 36.0;

/// Light.dbc weather param slots: [0]=clear, [1]=clear-underwater, [2]=storm, [3]=storm-underwater,
/// [4]=death.
const SLOT_CLEAR: usize = 0;
/// `[1]` clear-underwater: the denser fog + cooler light the client switches to when the eye is
/// submerged in **water** (VERIFIED apitrace WoW.18: ~38 yd teal fog + blue ambient/diffuse vs the
/// ~1284 yd surface fog). Same band-row meanings as the clear slot, just a different `LightParams` id.
/// Reached only by [`Submersion::Water`] — magma and slime never consult a zone slot at all
/// (see [`PARAM_MAGMA`]).
const SLOT_CLEAR_UNDERWATER: usize = 1;
const SLOT_STORM: usize = 2;
const SLOT_STORM_UNDERWATER: usize = 3;
/// The ghost-world profile — byte-VERIFIED (wow-re death-light.md): the client's ghost watcher
/// writes slot index 4 into the active-slot global `[0xce9bb0]` and the day/night color-table
/// rebuild consumes it INSTANTLY (per-frame rebuild, no blend). Selected while `ghost`, taking
/// priority over weather/underwater exactly as the single global slot does.
const SLOT_DEATH: usize = 4;

/// The **fixed global `LightParams` rows** the reference uses for submersion in the two fullbright
/// liquids — byte-VERIFIED (`0x6d2371` routes magma and slime to their own leg *before* the slot
/// selection: magma at `0x6d23e1` reads row 7, slime at `0x6d239e` reads row 6, each guarded on the
/// table's `maxId`). These are **zone-independent** and nothing in `Light.dbc` references them, which
/// is why no position-keyed dump ever surfaced them: submersion in lava does not read the zone's
/// underwater slot at all.
///
/// The shipped rows are exactly the vanilla look, which is the cross-check that earned this:
/// magma fog `[200,52,0]` at **27 yd** with start fraction **−2.0** (67 % fiery orange at the eye),
/// slime fog `[0,255,0]` at **50 yd** with **−1.0** (50 % green). There is no separate magma/slime
/// fog constant anywhere in the binary (VERIFIED negative) — these two rows *are* the distinct look.
const PARAM_SLIME: u32 = 6;
const PARAM_MAGMA: u32 = 7;

/// What the camera eye is submerged in, if anything — the atmosphere selector. Water and ocean pick
/// the zone's *underwater slot*; magma and slime replace the whole area blend with a fixed global row
/// (see [`PARAM_MAGMA`]/[`PARAM_SLIME`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Submersion {
    /// Dry — the ordinary clear/storm slot.
    #[default]
    Dry,
    /// Under water, ocean or rapids: the zone's own underwater `LightParams`, area-blended as usual.
    Water,
    /// Under magma: the fixed global row, verbatim.
    Magma,
    /// Under slime: the fixed global row, verbatim.
    Slime,
}

impl Submersion {
    /// Submerged in **any** liquid.
    pub fn any(self) -> bool {
        self != Submersion::Dry
    }

    /// Submerged specifically in a WATER kind (water / ocean / rapids) — the predicate for the
    /// consumers that are about water rather than about liquid: the `UnderWaterLoop` ambience bed and
    /// the "Underwater" reverb-preference column.
    ///
    /// Deliberately NOT [`Self::any`]: whether the reference swaps the submerged ambience bed and the
    /// reverb preset for **lava and slime** as well is unverified, and widening it silently on the way
    /// past would be inventing a behaviour. The atmosphere is verified per kind; the audio is not.
    pub fn is_water(self) -> bool {
        self == Submersion::Water
    }

    /// The fixed global `LightParams` row this submersion replaces the whole blend with, if any.
    /// `None` for [`Submersion::Dry`] and [`Submersion::Water`], which go through the slot selection.
    fn fixed_param(self) -> Option<u32> {
        match self {
            Submersion::Magma => Some(PARAM_MAGMA),
            Submersion::Slime => Some(PARAM_SLIME),
            Submersion::Dry | Submersion::Water => None,
        }
    }
}

/// Pick the `LightParams` slot for the current ghost/weather/submersion state (see `SLOT_*`).
/// Ghost wins outright — the client keeps ONE active slot, and the ghost watcher owns it while
/// the flag is up. Only [`Submersion::Water`] reaches the underwater slots; the fullbright liquids
/// never get here (their fixed row short-circuits the blend).
fn weather_slot(ghost: bool, stormy: bool, underwater: bool) -> usize {
    if ghost {
        return SLOT_DEATH;
    }
    match (stormy, underwater) {
        (false, false) => SLOT_CLEAR,
        (false, true) => SLOT_CLEAR_UNDERWATER,
        (true, false) => SLOT_STORM,
        (true, true) => SLOT_STORM_UNDERWATER,
    }
}

struct Light {
    map: u32,
    /// WoW net-protocol **world** coords (yards), converted from the DBC's mirrored/axis-swapped
    /// inch frame via [`dbc_to_world`]. Meaningless when `global` (the sentinel was `(0,0,0)`).
    pos: [f32; 3],
    /// Distance (yards) at which this light is at full strength; inside it `blendAlpha = 1`.
    falloff_start: f32,
    falloff_end: f32,
    /// The continent's `(0,0,0)` fallback light — matches everywhere, lowest priority.
    global: bool,
    /// LightParams id per weather slot (see `SLOT_*`); 0 = unset.
    params: [u32; 5],
}

/// Convert a `Light.dbc` position to WoW net-protocol **world** coords (yards). The DBC stores
/// continent coords as **inches** (yards × 36) in a mirrored, axis-swapped frame, so it is *not* a
/// plain `/36`. Per wowdev.wiki `LightRec::ConvertDBToGameCoords` (the world-coord correction):
/// `world.x = 17066.666 − z/36`, `world.y = 17066.666 − x/36`, `world.z = y/36`.
/// `17066.666` = half the 34133⅓-yard map (32 tiles × 533⅓). Verified against decoded entries
/// (e.g. Light #51 → `(-8480, 548, 81)`).
fn dbc_to_world(x: f32, y: f32, z: f32) -> [f32; 3] {
    const MAP_HALF: f32 = 17066.666;
    [
        MAP_HALF - z / POS_SCALE,
        MAP_HALF - x / POS_SCALE,
        y / POS_SCALE,
    ]
}

/// Parsed lighting tables; query with [`LightCatalog::sample`].
pub struct LightCatalog {
    lights: Vec<Light>,
    int_bands: HashMap<u32, Band<u32>>,
    float_bands: HashMap<u32, Band<f32>>,
    /// `LightParams.dbc` id → glow scalar (field [`LP_GLOW`]); the per-zone bloom composite weight.
    light_params_glow: HashMap<u32, f32>,
    /// `LightParams.dbc` id → `highlightSky` flag (field [`LP_HIGHLIGHT`]) as 0.0/1.0; gates the
    /// per-zone dawn/dusk sky-dome warp.
    light_params_highlight: HashMap<u32, f32>,
    /// `LightParams.dbc` id → water-blend alphas `[waterShallow, waterDeep, oceanShallow, oceanDeep]`
    /// (fields 5–8). The from-above swatch's per-zone depth-alpha ramp endpoints.
    light_params_water_alpha: HashMap<u32, [f32; 4]>,
}

fn light_schema() -> Schema {
    let mut s = Schema::new("Light");
    for (n, t) in [
        ("ID", FieldType::UInt32),
        ("continent", FieldType::UInt32),
        ("x", FieldType::Float32),
        ("y", FieldType::Float32),
        ("z", FieldType::Float32),
        ("falloffStart", FieldType::Float32),
        ("falloffEnd", FieldType::Float32),
    ] {
        s.add_field(SchemaField::new(n, t));
    }
    for i in 0..5 {
        s.add_field(SchemaField::new(format!("param{i}"), FieldType::UInt32));
    }
    s
}

/// `LightParams.dbc` — 9 fields, 36 B/record. We only consume `glow` (field [`LP_GLOW`]); the rest are
/// named for clarity (highlightSky flag, skybox/cloud ids, and the four water/ocean blend alphas we
/// don't yet use). The `+0x0C` `cloudTypeID`/reserved slot is 0 in all 5875 records — the engine's glow
/// is the *next* field (+0x10). Field types per wowdev + the byte-exact RE in `bloom-per-zone/`.
fn light_params_schema() -> Schema {
    let mut s = Schema::new("LightParams");
    for (n, t) in [
        ("ID", FieldType::UInt32),
        ("highlightSky", FieldType::UInt32),
        ("lightSkyboxID", FieldType::UInt32),
        ("cloudTypeID", FieldType::UInt32),
        ("glow", FieldType::Float32),
        ("waterShallowAlpha", FieldType::Float32),
        ("waterDeepAlpha", FieldType::Float32),
        ("oceanShallowAlpha", FieldType::Float32),
        ("oceanDeepAlpha", FieldType::Float32),
    ] {
        s.add_field(SchemaField::new(n, t));
    }
    s
}

/// The client's distance→strength ramp for an area light: `1` within `start`, falling linearly to
/// `0` at `end` (and `0` beyond). Matches `getEnvInfo`'s `blendAlpha`.
fn blend_alpha(dist: f32, start: f32, end: f32) -> f32 {
    if dist >= end {
        0.0
    } else if dist <= start || end <= start {
        1.0
    } else {
        (end - dist) / (end - start)
    }
}

impl LightCatalog {
    /// Read the three lighting DBCs off the patch chain.
    pub fn load(chain: &mut Chain) -> Result<Self> {
        let lights = {
            let bytes = chain
                .read_file(LIGHT)
                .with_context(|| format!("reading {LIGHT}"))?;
            let rs = parse(&bytes, light_schema(), "Light")?;
            let mut v = Vec::with_capacity(rs.records().len());
            for r in rs.records() {
                if let (Some(map), Some(x), Some(y), Some(z), Some(start), Some(end)) = (
                    u32_at(r, 1),
                    f32_at(r, 2),
                    f32_at(r, 3),
                    f32_at(r, 4),
                    f32_at(r, 5),
                    f32_at(r, 6),
                ) {
                    // Fields 7..12 are the 5 LightParams refs (clear, clear-water, storm, …).
                    let mut params = [0u32; 5];
                    for (i, p) in params.iter_mut().enumerate() {
                        *p = u32_at(r, 7 + i).unwrap_or(0);
                    }
                    // `(0,0,0)` is the continent-wide fallback sentinel — detect it on the raw
                    // values (it does NOT survive the world transform).
                    let global = x == 0.0 && y == 0.0 && z == 0.0;
                    v.push(Light {
                        map,
                        pos: if global {
                            [0.0; 3]
                        } else {
                            dbc_to_world(x, y, z)
                        },
                        falloff_start: start / POS_SCALE,
                        falloff_end: end / POS_SCALE,
                        global,
                        params,
                    });
                }
            }
            v
        };
        let int_bands = load_bands(
            chain,
            INT_BAND,
            band_schema("LightIntBand", FieldType::UInt32),
        )?;
        let float_bands = load_float_bands(
            chain,
            FLOAT_BAND,
            band_schema("LightFloatBand", FieldType::Float32),
        )?;
        let (light_params_glow, light_params_highlight, light_params_water_alpha) = {
            let bytes = chain
                .read_file(LIGHT_PARAMS)
                .with_context(|| format!("reading {LIGHT_PARAMS}"))?;
            let rs = parse(&bytes, light_params_schema(), "LightParams")?;
            let mut glow = HashMap::with_capacity(rs.records().len());
            let mut highlight = HashMap::with_capacity(rs.records().len());
            let mut water_alpha = HashMap::with_capacity(rs.records().len());
            for r in rs.records() {
                let Some(id) = u32_at(r, 0) else { continue };
                if let Some(g) = f32_at(r, LP_GLOW) {
                    glow.insert(id, g);
                }
                // highlightSky is an int 0/1 flag; store 0.0/1.0 as the dawn/dusk-warp gate.
                if let Some(h) = u32_at(r, LP_HIGHLIGHT) {
                    highlight.insert(id, if h != 0 { 1.0 } else { 0.0 });
                }
                // Water-blend alphas (fields 5–8). Stored only when all four parse; consumers fall back
                // to the DEFAULT alphas otherwise. Per-zone the from-above swatch alpha ramp endpoints.
                if let (Some(ws), Some(wd), Some(os), Some(od)) = (
                    f32_at(r, LP_WATER_SHALLOW_ALPHA),
                    f32_at(r, LP_WATER_DEEP_ALPHA),
                    f32_at(r, LP_OCEAN_SHALLOW_ALPHA),
                    f32_at(r, LP_OCEAN_DEEP_ALPHA),
                ) {
                    water_alpha.insert(id, [ws, wd, os, od]);
                }
            }
            (glow, highlight, water_alpha)
        };
        Ok(LightCatalog {
            lights,
            int_bands,
            float_bands,
            light_params_glow,
            light_params_highlight,
            light_params_water_alpha,
        })
    }

    /// Sample the atmosphere for `pos` (raw WoW yards) on `map` at `time` (half-minutes, 1440=noon).
    /// `stormy` selects the rainy/snowy param slot over the clear one. Picks the most-local light
    /// sphere containing `pos`, else the continent's global `(0,0,0)` light, else
    /// [`Atmosphere::DEFAULT`].
    pub fn sample(&self, map: u32, pos: [f32; 3], time: u32, stormy: bool) -> Atmosphere {
        let Some(light) = self.pick_light(map, pos) else {
            return Atmosphere::DEFAULT;
        };
        // Fall back to the clear slot if the requested weather slot is unset.
        let slot = if stormy { SLOT_STORM } else { SLOT_CLEAR };
        let param = match light.params[slot] {
            0 => light.params[SLOT_CLEAR],
            p => p,
        };
        if param < 1 {
            return Atmosphere::DEFAULT;
        }
        self.sample_param(param, time)
    }

    /// [`LightCatalog::sample`] at noon, clear weather.
    pub fn sample_noon(&self, map: u32, pos: [f32; 3]) -> Atmosphere {
        self.sample(map, pos, DAY / 2, false)
    }

    /// Debug: print every `LightIntBand` row (0..18) and `LightFloatBand` row (0..6) for the clear
    /// LightParams covering `pos`, sampled at `time`. Used to verify which rows hold which colors
    /// (the five sky-gradient stops, sun/cloud colors, etc.) rather than guessing the layout.
    pub fn debug_bands(&self, map: u32, pos: [f32; 3], time: u32) {
        let Some(light) = self.pick_light(map, pos) else {
            println!("(no light covers this position)");
            return;
        };
        let p = match light.params[SLOT_CLEAR] {
            0 => {
                println!("(no clear LightParams)");
                return;
            }
            x => x,
        };
        self.debug_param(p, time);
    }

    /// Debug: [`Self::debug_bands`] for an explicitly named `LightParams` id, with no position and no
    /// slot selection. The instrument for the rows **nothing in `Light.dbc` references**: magma and
    /// slime submersion do not read a zone's underwater slot at all, they read the fixed global rows
    /// 7 and 6 (VERIFIED `0x6d2371`, guards `maxId >= 7`/`>= 6`), which no position-keyed dump can
    /// ever reach.
    pub fn debug_param(&self, p: u32, time: u32) {
        if p < 1 {
            println!("(LightParams ids are 1-based)");
            return;
        }
        println!("LightParams {p} @ time {time} half-min — all int rows (sRGB 0..255):");
        for b in 0..18u32 {
            let key = (p - 1) * 18 + b + 1;
            match self
                .int_bands
                .get(&key)
                .and_then(|band| sample_color(band, time))
            {
                Some(c) => println!(
                    "  int[{b:2}] = [{:3}, {:3}, {:3}]",
                    (c[0] * 255.0) as u8,
                    (c[1] * 255.0) as u8,
                    (c[2] * 255.0) as u8
                ),
                None => println!("  int[{b:2}] = (unset)"),
            }
        }
        for b in 0..6u32 {
            let key = (p - 1) * 6 + b + 1;
            match self
                .float_bands
                .get(&key)
                .and_then(|band| sample_float(band, time))
            {
                Some(v) => println!("  float[{b}] = {v:.4}"),
                None => println!("  float[{b}] = (unset)"),
            }
        }
    }

    /// Faithful **area-light blend** (step 2 isolation): replicates the real client's
    /// `calculateLightParamBlends` + the sequential lerp in `getLightResultsFromDB`. Start from the
    /// continent's global `(0,0,0)` light, then for every local light sphere covering `pos` (sorted by
    /// `blendAlpha` descending) lerp the accumulated atmosphere toward that light by its `blendAlpha`
    /// (`1` inside `falloffStart`, ramping to `0` at `falloffEnd`). Contrast with [`pick_light`], which
    /// picks just the single most-local sphere — this tells us whether a faithful blend resolves a
    /// **different** (e.g. redder) light at our spot than the single-sample approximation.
    #[allow(clippy::too_many_arguments)]
    pub fn sample_blended(
        &self,
        map: u32,
        pos: [f32; 3],
        time: u32,
        stormy: bool,
        submersion: Submersion,
        ghost: bool,
    ) -> Atmosphere {
        // Magma/slime submersion replaces the WHOLE area blend with one fixed global row — the
        // reference branches out at `0x6d2371`, before the slot selection, so there is no zone
        // sphere blend and no storm lerp on this leg (see [`PARAM_MAGMA`]). Guarded on the row
        // actually existing, matching the binary's own `maxId` guard: a chain without it falls
        // through to the ordinary atmosphere rather than inventing one.
        if let Some(p) = submersion.fixed_param() {
            if self.has_param(p) {
                return self.sample_param(p, time);
            }
        }
        let slot = weather_slot(ghost, stormy, submersion == Submersion::Water);
        let atmo_of = |l: &Light| -> Option<Atmosphere> {
            // Fall back to the clear slot if the requested slot is unset for this light (e.g. a zone
            // with no dedicated underwater param degrades to the above-water atmosphere).
            let param = match l.params[slot] {
                0 => l.params[SLOT_CLEAR],
                p => p,
            };
            (param >= 1).then(|| self.sample_param(param, time))
        };

        // Base = the continent global light (lowest priority, covers everywhere).
        let mut acc = self
            .lights
            .iter()
            .find(|l| l.map == map && l.global)
            .and_then(atmo_of)
            .unwrap_or(Atmosphere::DEFAULT);

        // Collect local spheres covering `pos` with their blendAlpha, strongest first.
        let mut locals: Vec<(f32, &Light)> = self
            .lights
            .iter()
            .filter(|l| l.map == map && !l.global)
            .filter_map(|l| {
                let d = (0..3)
                    .map(|i| (l.pos[i] - pos[i]).powi(2))
                    .sum::<f32>()
                    .sqrt();
                let a = blend_alpha(d, l.falloff_start, l.falloff_end);
                (a > 0.0).then_some((a, l))
            })
            .collect();
        locals.sort_by(|a, b| b.0.total_cmp(&a.0));

        for (alpha, l) in locals {
            if let Some(atmo) = atmo_of(l) {
                acc = acc.lerp(&atmo, alpha);
            }
        }
        acc
    }

    /// Debug (step 2): dump every light sphere on `map` near `pos` — distance, falloff, `blendAlpha`,
    /// and its clear-weather ambient + sun colors — then the faithful blended result vs the single
    /// [`pick_light`] sample, so we can see whether the residual under-red lives in the **values**.
    pub fn debug_blend(&self, map: u32, pos: [f32; 3], time: u32) {
        let mut rows: Vec<(f32, f32, &Light)> = self
            .lights
            .iter()
            .filter(|l| l.map == map && !l.global)
            .map(|l| {
                let d = (0..3)
                    .map(|i| (l.pos[i] - pos[i]).powi(2))
                    .sum::<f32>()
                    .sqrt();
                (d, blend_alpha(d, l.falloff_start, l.falloff_end), l)
            })
            .collect();
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));

        let c = |x: [f32; 3]| {
            format!(
                "[{:3} {:3} {:3}]",
                (x[0] * 255.0) as u8,
                (x[1] * 255.0) as u8,
                (x[2] * 255.0) as u8
            )
        };
        println!(
            "Lights on map {map} near [{:.0} {:.0} {:.0}] (nearest 8) — dist | start->end | alpha | amb / sun:",
            pos[0], pos[1], pos[2]
        );
        for (d, alpha, l) in rows.iter().take(8) {
            let a = if l.params[SLOT_CLEAR] >= 1 {
                self.sample_param(l.params[SLOT_CLEAR], time)
            } else {
                Atmosphere::DEFAULT
            };
            println!(
                "  d={d:7.0} | {:6.0}->{:6.0} | a={alpha:.3} | amb {} sun {}",
                l.falloff_start,
                l.falloff_end,
                c(a.ambient),
                c(a.sun_diffuse)
            );
        }
        let global = self
            .lights
            .iter()
            .find(|l| l.map == map && l.global)
            .and_then(|l| {
                (l.params[SLOT_CLEAR] >= 1).then(|| self.sample_param(l.params[SLOT_CLEAR], time))
            });
        if let Some(g) = global {
            println!(
                "  GLOBAL (0,0,0) base      | amb {} sun {}",
                c(g.ambient),
                c(g.sun_diffuse)
            );
        }
        let picked = self.sample(map, pos, time, false);
        let blended = self.sample_blended(map, pos, time, false, Submersion::Dry, false);
        println!(
            "  => pick_light : amb {} sun {}",
            c(picked.ambient),
            c(picked.sun_diffuse)
        );
        println!(
            "  => blended    : amb {} sun {}",
            c(blended.ambient),
            c(blended.sun_diffuse)
        );
    }

    /// Debug: the **per-weather-slot** resolve at `pos` — for each of the five `Light.dbc` param
    /// slots (clear / clear-underwater / storm / storm-underwater / death), which `LightParams` id
    /// the picked sphere and the continent global each name, whether the slot is UNSET (and so falls
    /// back to clear), and the fog/ambient/diffuse the blend actually resolves.
    ///
    /// The companion to [`Self::debug_bands`], which only ever dumps the clear slot: an underwater or
    /// ghost atmosphere that reads wrong is either a slot the data leaves unset (→ we correctly show
    /// the surface atmosphere) or a slot whose values we mis-consume, and those two look identical
    /// from inside the game. This tells them apart in one command.
    pub fn debug_slots(&self, map: u32, pos: [f32; 3], time: u32) {
        const NAMES: [&str; 5] = [
            "clear",
            "clear-underwater",
            "storm",
            "storm-underwater",
            "death",
        ];
        let c = |x: [f32; 3]| {
            format!(
                "[{:3} {:3} {:3}]",
                (x[0] * 255.0) as u8,
                (x[1] * 255.0) as u8,
                (x[2] * 255.0) as u8
            )
        };
        let picked = self.pick_light(map, pos);
        let global = self.lights.iter().find(|l| l.map == map && l.global);
        println!(
            "Light slots on map {map} at [{:.1} {:.1} {:.1}], time {time} half-min:",
            pos[0], pos[1], pos[2]
        );
        match picked {
            Some(l) if l.global => {
                println!("  picked sphere : (none local — the continent global)")
            }
            Some(l) => println!(
                "  picked sphere : local, falloff {:.0}->{:.0} yd, params {:?}",
                l.falloff_start, l.falloff_end, l.params
            ),
            None => println!("  picked sphere : (no light covers this position)"),
        }
        if let Some(g) = global {
            println!("  continent glob: params {:?}", g.params);
        }
        println!(
            "  {:<17} {:>6} {:>6} {:>9} {:>6}  {:<15} {:<15} {:<15}",
            "slot", "picked", "global", "fog_end", "frac", "fog_color", "ambient", "diffuse"
        );
        for (slot, name) in NAMES.iter().enumerate() {
            // The same UNSET→clear fallback both sample paths apply, reported per source so an
            // "unset everywhere" slot is visibly distinct from one carrying real values.
            let show = |l: Option<&Light>| match l.map(|l| l.params[slot]) {
                None => "     -".to_string(),
                Some(0) => " unset".to_string(),
                Some(p) => format!("{p:>6}"),
            };
            let a = self.sample_blended(
                map,
                pos,
                time,
                slot == SLOT_STORM || slot == SLOT_STORM_UNDERWATER,
                if slot == SLOT_CLEAR_UNDERWATER || slot == SLOT_STORM_UNDERWATER {
                    Submersion::Water
                } else {
                    Submersion::Dry
                },
                slot == SLOT_DEATH,
            );
            println!(
                "  {name:<17} {} {} {:>9.1} {:>6.2}  {:<15} {:<15} {:<15}",
                show(picked),
                show(global),
                a.fog_end,
                a.fog_start_frac,
                c(a.fog_color),
                c(a.ambient),
                c(a.sun_diffuse)
            );
        }
    }

    fn pick_light(&self, map: u32, pos: [f32; 3]) -> Option<&Light> {
        let mut local: Option<&Light> = None;
        let mut global: Option<&Light> = None;
        for l in self.lights.iter().filter(|l| l.map == map) {
            if l.global {
                global = Some(l);
                continue;
            }
            let d2 = (0..3).map(|i| (l.pos[i] - pos[i]).powi(2)).sum::<f32>();
            if d2 <= l.falloff_end * l.falloff_end
                && local.is_none_or(|b| l.falloff_end < b.falloff_end)
            {
                local = Some(l);
            }
        }
        local.or(global)
    }

    /// Whether `LightParams` `p` actually has band rows in this chain — the client's `maxId` guard on
    /// the fixed magma/slime rows. Checks the fog-colour row, which every real record carries.
    fn has_param(&self, p: u32) -> bool {
        p >= 1
            && self
                .int_bands
                .contains_key(&((p - 1) * 18 + IB_FOG_COLOR + 1))
    }

    fn sample_param(&self, p: u32, t: u32) -> Atmosphere {
        let ib = |b: u32| self.int_bands.get(&((p - 1) * 18 + b + 1));
        let fb = |b: u32| self.float_bands.get(&((p - 1) * 6 + b + 1));
        let d = Atmosphere::DEFAULT;
        // Fog distances carry the ×36 storage scale (yards = raw/36): Elwynn clear 18000→500 yd,
        // storm 10000→278 yd. Byte-VERIFIED (0327, wow-re rf-weather-fog-veil Q2·LOAD): the client
        // scales ONCE at DBC load — `0x53f504 → 0x6d6160 → 0x6d6100` runs `dn_array_scale_36`
        // (`0x6d6090`, ×1/36 @0x7ff9d0) over each float-band record with `rowIndex % 6 == 0`, i.e.
        // ONLY the sub-0 fog-END band; the sub-1 start FRACTION is never scaled. We apply the same
        // /36 at sample time (a time-interp of scaled values ≡ the scale of the interp).
        let fog_end = fb(FB_FOG_END)
            .and_then(|b| sample_float(b, t))
            .map(|v| v / POS_SCALE)
            .filter(|&v| v > 1.0)
            .unwrap_or(d.fog_end);
        let fog_start_frac = fb(FB_FOG_START_MULT)
            .and_then(|b| sample_float(b, t))
            .unwrap_or(d.fog_start_frac);
        let col = |idx: u32, fallback: [f32; 3]| {
            ib(idx).and_then(|b| sample_color(b, t)).unwrap_or(fallback)
        };
        Atmosphere {
            fog_end,
            // UNCLAMPED, negative under storm (Elwynn −0.5) — the negative start IS the constant
            // near veil the reference shows in rain; clamping it to 0 was the veil-killing bug.
            fog_start_frac,
            fog_color: col(IB_FOG_COLOR, d.fog_color),
            sun_diffuse: col(IB_DIFFUSE, d.sun_diffuse),
            sun_color: col(IB_SUN_COLOR, d.sun_color),
            ambient: col(IB_AMBIENT, d.ambient),
            sky: std::array::from_fn(|i| col(IB_SKY0 + i as u32, d.sky[i])),
            // Dedicated water-tint rows (RAW, no scale): 16/17 river/lake, 14/15 ocean — the 2-endpoint
            // depth-swatch lerp (`FUN_0068a830`). These are real per-zone rows, NOT the sky gradient.
            water_river: [
                col(IB_RIVER_SHALLOW, d.water_river[0]),
                col(IB_RIVER_DEEP, d.water_river[1]),
            ],
            water_ocean: [
                col(IB_OCEAN_SHALLOW, d.water_ocean[0]),
                col(IB_OCEAN_DEEP, d.water_ocean[1]),
            ],
            // Per-zone water-blend alphas (static LightParams fields 5–8); fall back to the DEFAULT
            // ramp endpoints when the record didn't carry them.
            water_river_alpha: self
                .light_params_water_alpha
                .get(&p)
                .map(|a| [a[0], a[1]])
                .unwrap_or(d.water_river_alpha),
            water_ocean_alpha: self
                .light_params_water_alpha
                .get(&p)
                .map(|a| [a[2], a[3]])
                .unwrap_or(d.water_ocean_alpha),
            // Per-zone glow (static LightParams field, not a time band); fallback 0.5.
            glow: self.light_params_glow.get(&p).copied().unwrap_or(d.glow),
            // Cloud density C (float sub-3) + the three cloud palette rows (int sub-10/11/12) —
            // the coverage threshold and the visible dome's colors (wow-re cloud pipeline §3c/§4).
            cloud_density: fb(FB_CLOUD_DENSITY)
                .and_then(|b| sample_float(b, t))
                .unwrap_or(d.cloud_density),
            cloud_colors: [
                col(IB_CLOUD_SUN, d.cloud_colors[0]),
                col(IB_CLOUD_SLOPE, d.cloud_colors[1]),
                col(IB_CLOUD_GBASE, d.cloud_colors[2]),
            ],
            // Per-zone highlightSky flag (static); gates the dawn/dusk dome warp. Fallback 0.0.
            highlight_sky: self
                .light_params_highlight
                .get(&p)
                .copied()
                .unwrap_or(d.highlight_sky),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dbc_to_world_matches_wiki_convertdbtogamecoords() {
        // The `(0,0,0)` global sentinel maps to the map center (we special-case it before this).
        assert_eq!(dbc_to_world(0.0, 0.0, 0.0), [17066.666, 17066.666, 0.0]);

        // Round-trip a known world point through the wiki inverse
        // (DBC.x = (17066.666 − world.y)·36, DBC.y = world.z·36, DBC.z = (17066.666 − world.x)·36):
        // applying `dbc_to_world` must recover the world coords. Light #51 → (-8480, 548, 81).
        let world = [-8480.4f32, 548.3, 80.9];
        let dbc = [
            (17066.666 - world[1]) * POS_SCALE,
            world[2] * POS_SCALE,
            (17066.666 - world[0]) * POS_SCALE,
        ];
        let back = dbc_to_world(dbc[0], dbc[1], dbc[2]);
        for i in 0..3 {
            assert!(
                (back[i] - world[i]).abs() < 0.1,
                "axis {i}: {} != {}",
                back[i],
                world[i]
            );
        }
    }

    #[test]
    fn glow_defaults_blends_and_quantises() {
        // Fallback = the binary's "no active world-light" 0.5.
        assert_eq!(Atmosphere::DEFAULT.glow, 0.5);
        // Glow blends across light spheres like the colours (Duskwood 0.50 ↔ a 0.85 area).
        let a = Atmosphere {
            glow: 0.50,
            ..Atmosphere::DEFAULT
        };
        let b = Atmosphere {
            glow: 0.85,
            ..Atmosphere::DEFAULT
        };
        assert!((a.lerp(&b, 0.5).glow - 0.675).abs() < 1e-6);
        // The renderer quantises floor(glow·255)/255 — Elwynn's 0.65 → 165/255 = 0.647 (the
        // apitrace composite weight), confirming LP_GLOW (field 4) is the right field.
        let q = |g: f32| (g * 255.0).floor() / 255.0;
        assert!((q(0.65) - 165.0 / 255.0).abs() < 1e-6);
        assert!((q(0.50) - 127.0 / 255.0).abs() < 1e-6); // Duskwood → 0.498
    }
}
