//! Liquid (water) rendering: the animated lake/river/ocean surfaces the reference draws over MCLQ
//! geometry. The parse + flat mesh live in `benilla_formats::liquid` (built into each `ChunkMesh.liquid`
//! by the terrain loader); this is the Bevy render glue — one shared [`LiquidMaterial`] per
//! [`LiquidKind`], a `texture_2d_array` of its animated frames, and a 24 fps frame-index cycler.
//!
//! **This is the ADT path, and we render WMO liquid through it too — which is wrong.** The reference
//! has three liquid renderers, not one: ADT MCLQ (below), WMO MLIQ water (`0x6b62e0` category 0,
//! split again into exterior `0x6b6630` / interior `0x6b6420` on the group's `MOGP.flags & 0x48` —
//! one texture stage, no depth ramp, alpha from a per-vertex authored byte), and magma/slime. The
//! `wmo_liquid_arms` census sizes them: 30 exterior water groups (Stormwind's canals) against 134
//! interior (every dungeon pool, Blackfathom's included). Neither is what this file implements.
//! Recorded in wow-re `terrain/scratch/water-shading-law.md`.
//!
//! Faithful ADT model — the combiner is an **asset**, extracted from `patch.MPQ` and read, not
//! inferred: `Shaders\Pixel\ocean0_s.bls` is `rgb = primary·colorTex.rgb + detailTex.rgb +
//! (secondary+0.25)·detailTex.a`, `alpha = colorTex.a`, with `0.25` its own scalar `PARAM`. The
//! formula this module has always carried is right verbatim; its **provenance was not** — the
//! citation here used to name an "apitrace WoW.17 program 159" and a `docs/knowledge/terrain.md`,
//! neither of which exists, and wow-re had the program attributed to a character draw. The body
//! colour is **`primary · waterTint`**, where:
//! - **`waterTint`** is a plain **2-endpoint linear lerp** of the zone's dedicated `Light.dbc` water
//!   rows, RAW (no ×0.711): IntBand rows 16/17 (river/lake) or 14/15 (ocean), shallow→deep, by the
//!   per-vertex depth `V` (river/lake `V = clamp(byte/42)`, VERIFIED `c81768`/`FUN_0068d790`; saturates
//!   ~5 yd so the channel middle reaches the deep/teal row). The swatch is rebuilt **per world frame**,
//!   not baked once at bootstrap: the dirty flags `[0xc8117c]`/`[0xc81b70]` clear at
//!   `0x680b90`/`0x680b97` and refill via `0x58acd0`, so colour and opacity track the zone and the
//!   clock. (The earlier "reflected sky × 0.711 via `FUN_0068c250`" model fingered the WRONG builder —
//!   a separate grey edge texture never bound on the water unit; and `byte/255` was the wrong LUT →
//!   river never went teal. The `FUN_0068a830` attribution this line used to carry was also wrong:
//!   wow-re reads `0x68a830` as a sky-band row fill.)
//! - **`primary`** is the lit vertex colour `clamp(ambient + N·L·sun)`.
//! - the animated `lake_a`/`ocean_h` frame is the **`detailTex`** (near-black RGB + ripple alpha): a
//!   faint flat lift + an achromatic shimmer on crests — NOT the body colour. Mipped + 16× aniso so the
//!   ripple averages out at distance (near-field samples mip 0, so near sparkle is the term itself).
//! - **opacity** = the SAME `V` indexes both colour and alpha (one swatch row → RGB + A): a ramp between
//!   the LightParams shallow/deep alphas — river 0.5→1.0, ocean 0.75→1.0. The river channel reaches
//!   α=1.0 (opaque, deep teal) by byte 42 ≈ 5 yd; the shore stays see-through (the pale edge band —
//!   the bottom shows). **OPEN:** we ramp it linearly (`127+2·row`), but wow-re reads the byte-verified
//!   ADT water alpha as the `0xc7fbc0` LUT's `1.6·(i/63)^8` — a curve that stays near-transparent far
//!   longer and then climbs hard. Unchanged so far: it is a look change to every ADT water surface in
//!   the game, so it belongs in one A/B with the WMO arms rather than landing on its own.
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
//! Three concerns, so three files behind this face:
//!
//! * [`query`] — **where the liquid is, and whether you are in it.** The world-space grid every
//!   surface publishes ([`WaterChunkInfo`]), the position queries the swim/sound/foam/lighting
//!   systems ask it ([`liquid_at`], [`water_surface_at`]), and the per-frame camera submersion
//!   verdict ([`Underwater`]). Nothing here renders.
//! * [`spatial`] — **which surfaces are near an XY**, without walking them all: the grid-hash
//!   [`WaterIndex`] the per-draw consumers (the water-plane interleave's three lanes) pre-filter
//!   through. The once-a-frame askers keep the plain walk.
//! * [`surface`] — **the Bevy render glue.** The per-kind animated materials, the two spawn paths
//!   (MCLQ and WMO MLIQ), the flat mesh build, and the 24 fps frame cycler.
//!
//! The one cross-feed runs query → lighting: `detect_submersion` publishes WHICH liquid the eye is
//! in, and `lighting::update_time_lighting` selects the whole submerged atmosphere from it.

use bevy::pbr::MaterialPlugin;
use bevy::prelude::*;

use benilla_assets::materials::LiquidMaterial;
use benilla_assets::AssetSet;

mod query;
#[cfg(test)]
mod real_data;
mod spatial;
mod surface; // the against-real-client-files tests — they span both halves

// The submodules are private, so this list IS the subsystem's face: everything the rest of the
// client may name. `LiquidSurface` is deliberately absent — it is spawned, never named from
// outside; add it here the day something needs it. Most of this list is now reached through
// `crate::world_point::WorldPoint` rather than directly (decision 1164).
pub use query::{
    camera_claim, describe_at, liquid_at, player_claim, surfaces_at, unit_claim, water_surface_at,
    FoamPatch, LiquidClaim, LiquidHit, LiquidSource, RoomPlacements, Underwater, WaterChunkInfo,
    WmoPool,
};
pub(crate) use spatial::{maintain_water_index, WaterIndex};
pub(crate) use surface::{spawn_liquids, spawn_wmo_liquids, LiquidAssets, LiquidSoundSource};

/// The frame slot where [`Underwater`] is written — the label every consumer of the submersion
/// verdict orders itself `.after(..)`. The submerged view is a whole-screen swap (atmosphere, clear
/// colour, and the sky-pass suppression all flip on it), so a consumer reading a frame-old verdict
/// shows one mixed frame on every surface crossing — the murk with the sun still up, or clear water
/// with the sky already gone. One label, so the "same frame" contract is written once instead of
/// re-derived per consumer.
#[derive(bevy::ecs::schedule::SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SubmersionVerdict;

/// The water subsystem: load the per-kind frame arrays + shared materials at startup, then cycle the
/// animation frame each update. Spawning the per-chunk surfaces happens in the terrain streamer (via
/// [`spawn_liquids`], water lives *with* its tile), reading [`LiquidAssets`].
pub(crate) struct LiquidPlugin;

impl Plugin for LiquidPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<LiquidMaterial>::default())
            .init_resource::<Underwater>()
            .init_resource::<WaterIndex>()
            // PreUpdate: surfaces stream in/out via Update-side commands, so the edge is visible
            // here the frame after — before any of that frame's consumers ask. A despawn's stale
            // entry in between self-filters at the consumer (`Query::get` misses).
            .add_systems(PreUpdate, maintain_water_index)
            .add_systems(Startup, surface::setup_liquid.after(AssetSet::Open))
            .add_systems(
                Update,
                (
                    // AFTER the portal pass, which publishes the camera eye's room this frame
                    // (`CameraInteriorClaim`): the submersion verdict is the eye's room applied to
                    // the eye's position, and a frame-old room would flash the wrong atmosphere for
                    // one frame on every doorway crossing — which, for a whole-screen filter, is
                    // exactly the artefact that reads worst.
                    query::detect_submersion
                        .after(crate::wmo_portal::WmoPvsSet)
                        .in_set(SubmersionVerdict),
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
