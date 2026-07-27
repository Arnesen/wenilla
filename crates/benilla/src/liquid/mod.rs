//! Liquid (water) rendering: the animated lake/river/ocean surfaces the reference draws over MCLQ
//! geometry. The parse + flat mesh live in `benilla_formats::liquid` (built into each `ChunkMesh.liquid`
//! by the terrain loader); this is the Bevy render glue — one shared [`LiquidMaterial`] per
//! [`LiquidKind`], a `texture_2d_array` of its animated frames, and a 24 fps frame-index cycler.
//!
//! Faithful model (RE'd from `WoW.exe` + `ocean0_s.bls` + apitrace WoW.17 program 159, all agree).
//! `ocean0_s.bls`: `rgb = primary·colorTex.rgb + detailTex.rgb + (secondary+0.25)·detailTex.a`,
//! `alpha = colorTex.a`. The body colour is **`primary · waterTint`**, where:
//! - **`waterTint`** is a plain **2-endpoint linear lerp** of the zone's dedicated `Light.dbc` water
//!   rows, RAW (no ×0.711): IntBand rows 16/17 (river/lake) or 14/15 (ocean), shallow→deep, by the
//!   per-vertex depth `V` (river/lake `V = clamp(byte/42)`, VERIFIED `c81768`/`FUN_0068d790`; saturates
//!   ~5 yd so the channel middle reaches the deep/teal row). Swatch builder VERIFIED: WoW.exe
//!   `FUN_0068a830`, golden-vector-matched to the apitrace swatch ≤1/255 over all 64 rows. (The earlier
//!   "reflected sky × 0.711 via `FUN_0068c250`" model fingered the WRONG builder — a separate grey edge
//!   texture never bound on the water unit; and `byte/255` was the wrong LUT → river never went teal.)
//! - **`primary`** is the lit vertex colour `clamp(ambient + N·L·sun)`.
//! - the animated `lake_a`/`ocean_h` frame is the **`detailTex`** (near-black RGB + ripple alpha): a
//!   faint flat lift + an achromatic shimmer on crests — NOT the body colour. Mipped + 16× aniso so the
//!   ripple averages out at distance (near-field samples mip 0, so near sparkle is the term itself).
//! - **opacity** = the SAME `V` indexes both colour and alpha (one swatch row → RGB + A): a ramp between
//!   the LightParams shallow/deep alphas — river 0.5→1.0, ocean 0.75→1.0 (VERIFIED WoW.exe `FUN_0068a830`
//!   α = `127+2·row`). The river channel reaches α=1.0 (opaque, deep teal) by byte 42 ≈ 5 yd; the shore
//!   stays see-through (the pale edge band, faithful — the bottom shows).
//!
//! River/lake `V = clamp(byte/42)` (steep `c81768` LUT, `FUN_0068d790`) — NOT `byte/255` (the `c7fcd8`
//! LUT, a different draw list the from-above river path doesn't use; it left the river middle stuck on
//! the shallow green row). Ocean uses a non-LUT UV path → placeholder `/255` pending its own RE+A/B.
//! (Earlier cuts: ripple-as-colour → black; `×8` → "deep too early"; FLAT colour → "completely gone";
//! sky × 0.711 → wrong builder; `byte/255` → wrong LUT, no teal centre. Faithful = rows 14–17 raw lerp
//! + the /42 V. 2026-05-31.)
//!
//! Two-sided, alpha-blended, depth-write off (Bevy's transparent pass = the verified MCLQ water render
//! state).
//!
//! The frame-flip is the client's first render animation — a deliberate **one-off** (a frame-index
//! uniform off Bevy real `Time`), NOT a general animation system. Two clocks: animation =
//! wall-clock; day/night = server game-time.

//! ## Layout
//!
//! Two concerns, so two files behind this face:
//!
//! * [`query`] — **where the liquid is, and whether you are in it.** The world-space grid every
//!   surface publishes ([`WaterChunkInfo`]), the position queries the swim/sound/foam/lighting
//!   systems ask it ([`liquid_at`], [`water_surface_at`]), and the per-frame camera submersion
//!   verdict ([`Underwater`]). Nothing here renders.
//! * [`surface`] — **the Bevy render glue.** The per-kind animated materials, the two spawn paths
//!   (MCLQ and WMO MLIQ), the flat mesh build, and the 24 fps frame cycler.
//!
//! The one cross-feed runs query → lighting: `detect_submersion` publishes WHICH liquid the eye is
//! in, and `lighting::update_time_lighting` selects the whole submerged atmosphere from it.

use bevy::pbr::MaterialPlugin;
use bevy::prelude::*;

use crate::assets::AssetSet;
use crate::terrain::LiquidMaterial;

mod query;
#[cfg(test)]
mod real_data;
mod surface; // the against-real-client-files tests — they span both halves

// The submodules are private, so this list IS the subsystem's face: everything the rest of the
// client may name. `LiquidHit` and `LiquidSurface` are deliberately absent — they are returned and
// spawned, never named from outside; add them here the day something needs to.
pub(crate) use query::{
    camera_claim, describe_at, liquid_at, player_claim, unit_claim, water_surface_at, FoamPatch,
    LiquidClaim, LiquidSource, Underwater, WaterChunkInfo, WmoPool,
};
pub(crate) use surface::{spawn_liquids, spawn_wmo_liquids, LiquidAssets, LiquidSoundSource};

/// The water subsystem: load the per-kind frame arrays + shared materials at startup, then cycle the
/// animation frame each update. Spawning the per-chunk surfaces happens in the terrain streamer (via
/// [`spawn_liquids`], water lives *with* its tile), reading [`LiquidAssets`].
pub(crate) struct LiquidPlugin;

impl Plugin for LiquidPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<LiquidMaterial>::default())
            .init_resource::<Underwater>()
            .add_systems(Startup, surface::setup_liquid.after(AssetSet::Open))
            .add_systems(
                Update,
                (
                    surface::animate_liquid,
                    // AFTER the portal pass, which publishes the camera eye's room this frame
                    // (`CameraInteriorClaim`): the submersion verdict is the eye's room applied to
                    // the eye's position, and a frame-old room would flash the wrong atmosphere for
                    // one frame on every doorway crossing — which, for a whole-screen filter, is
                    // exactly the artefact that reads worst.
                    query::detect_submersion.after(crate::wmo_portal::WmoPvsSet),
                ),
            )
            // The surface-render kill-switch (see [`hide_liquid_surfaces`]) — inert without the env
            // var, so it costs nothing when it isn't being used.
            .add_systems(
                Update,
                surface::hide_liquid_surfaces
                    .run_if(|| std::env::var_os("WOW_NO_LIQUID").is_some()),
            );
    }
}
