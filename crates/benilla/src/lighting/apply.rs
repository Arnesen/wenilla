//! Per-frame lighting APPLY for the materials NOT yet on the shared global-light buffer — **WDL +
//! liquid** — plus the sky `ClearColor`. Terrain and models read the one shared light storage buffer
//! ([`super::global_light`]) directly, so they're no longer touched here (that was the per-material
//! re-push that re-created every bind group). WDL (1 material) and liquid (≈3, already rewritten every
//! frame for its ripple-frame index) are negligible, so they keep the simple change-gated push.
//! Split from the resolve half in `super`.

use bevy::prelude::*;

use super::{WowLighting, WATER_SHININESS};
use crate::debug_panel::DebugState;
use crate::terrain::{LiquidMaterial, WdlMaterial};

/// Snapshot of the WoW-lighting inputs (colors + sun dir + material count + Step-5 fog + the
/// fog-disable toggle + farclip). The WDL/liquid push only fires when one of these changes.
// Tuple lanes: (ambient, diffuse, spec, sun_dir, material_count, fog_color_u32_bits,
// fog_params_u32_bits, disable_fog, farclip_bits). Floats stored as raw u32 bits = byte-equality
// without float NaN/-0.0 churn.
type TerrainLightSnap = (
    [f32; 3],
    [f32; 3],
    [f32; 3],
    Vec3,
    usize,
    [u32; 3],
    [u32; 2],
    bool,
    u32,
);

/// Push the `Light.dbc` fog + (for liquid) light onto the **WDL + liquid** materials — the ones still
/// off the shared global-light buffer. Only writes when something changes (a new material appearing, a
/// light/fog transition, or the farclip slider moving). `light_sun.w = 1.0` (full directional); liquid
/// specular shininess is [`WATER_SHININESS`]. (Terrain + models read the shared buffer directly.)
#[allow(clippy::too_many_arguments)] // a Bevy system: each arg is a distinct resource/material store
pub(super) fn apply_wow_lighting(
    lighting: Option<Res<WowLighting>>,
    debug: Res<DebugState>,
    view: Res<crate::view::ViewDistance>,
    mut last: Local<Option<TerrainLightSnap>>,
    mut wdl_materials: ResMut<Assets<WdlMaterial>>,
    mut liquid_materials: ResMut<Assets<LiquidMaterial>>,
    liquid_assets: Option<Res<crate::liquid::LiquidAssets>>,
) {
    let Some(l) = lighting else {
        return;
    };
    let t = &debug.lighting;
    // Hard far-clip "wall" distance (yd) — the reference's projection far plane (`farclip`). Shares the
    // `ViewDistance.farclip` with the per-object cull (one "view distance" lever); fed to shaders via
    // fog_params.w so models discard fragments beyond it (per-pixel, closest-part-first reveal).
    // (Terrain now reads farclip from the shared global-light buffer instead — `lighting::global_light`.)
    let farclip = view.farclip;
    let snap: TerrainLightSnap = (
        l.ambient,
        l.diffuse,
        l.spec,
        l.sun_dir,
        wdl_materials.len() + liquid_materials.len(),
        [
            l.fog_color[0].to_bits(),
            l.fog_color[1].to_bits(),
            l.fog_color[2].to_bits(),
        ],
        [l.fog_start.to_bits(), l.fog_end.to_bits()],
        t.disable_fog,
        farclip.to_bits(),
    );
    if *last == Some(snap) {
        return;
    }
    *last = Some(snap);

    // The shared fog + light uniforms (still pushed per-material for model/wdl/liquid until they're
    // converted to the shared global-light buffer too — terrain already reads that buffer directly).
    // `light_sun.w = 1.0` (full directional / MCSH on). The panel "disable distance fog" toggle forces
    // fog off via `fog_color.w`. (The sky-dome horizon is unaffected — it reads the fog *colour*.)
    let mut tu = l.terrain_uniforms(t.disable_fog);
    // Live hard far-clip wall: terrain.wgsl + wow_model.wgsl read fog_params.w (from the shared buffer)
    // and discard fragments beyond it. WDL/liquid ignore .w (they render to the horizon behind the wall).
    tu.fog_params.w = farclip;
    // WDL distant terrain shares the terrain fog exactly (it's unlit white × this fog — the only
    // lever it has). One shared material, so this is a single write on each light change.
    for (_, m) in wdl_materials.iter_mut() {
        m.extension.fog_color = tu.fog_color;
        m.extension.fog_params = tu.fog_params;
    }
    // Water (lake/river/ocean): body colour = `primary · waterTint`, `primary` = the lit vertex colour
    // (ambient + N·L·sun), `waterTint` = the per-vertex-depth lerp `shallow_rgb → deep_rgb` of the zone's
    // dedicated `Light.dbc` water rows — IntBand 16/17 (river/lake) or 14/15 (ocean), RAW, a 2-endpoint
    // swatch (VERIFIED `WoW.exe FUN_0068a830`). The from-above depth cue is this COLOUR gradient PLUS a
    // shallow→deep alpha ramp; BOTH are indexed by the same per-vertex V (river/lake V = clamp(byte/42)).
    // So we push the per-KIND shallow/deep colours + alphas here every light change — the materials are
    // per kind, so we walk `LiquidAssets`' kind→handle map (not a blind material sweep). The `lake_a`
    // frame is just the ripple detail; its index is driven separately by `liquid::animate_liquid`.
    let water_spec = Vec4::new(
        tu.light_spec.x,
        tu.light_spec.y,
        tu.light_spec.z,
        WATER_SHININESS,
    );
    if let Some(assets) = &liquid_assets {
        for (kind, handle) in assets.iter() {
            let Some(m) = liquid_materials.get_mut(handle) else {
                continue;
            };
            let (shallow, deep, shallow_a, deep_a) = l.water_colors(kind);
            m.extension.light_ambient = tu.light_ambient;
            m.extension.light_diffuse = tu.light_diffuse;
            m.extension.light_sun = tu.light_sun;
            m.extension.light_spec = water_spec;
            m.extension.water_shallow = Vec4::new(shallow[0], shallow[1], shallow[2], shallow_a);
            m.extension.water_deep = Vec4::new(deep[0], deep[1], deep[2], deep_a);
            m.extension.fog_color = tu.fog_color;
            m.extension.fog_params = tu.fog_params;
        }
    }
}

/// **Step 6** — drive the camera's `ClearColor` from the same Light.dbc row-7 fog colour the
/// terrain and model shaders blend in (Step 5). Sky / skybox proper is deferred; for now the
/// backdrop is just the fog colour, so a fully-fogged texel at the far plane lands on the same
/// gamma byte as the void behind it (no horizon seam). q6 RE: the reference client's sky pass
/// actually sets its own tiny 70→75 fog and draws in the fog colour, so the horizon converges on
/// row 7 the same way; we approximate that with `glClearColor` until the sky dome is built
/// (out-of-scope per the plan).
///
/// `Color::srgb` takes gamma-space 0..1 — that's exactly the form `WowLighting.fog_color` carries
/// (the DBC byte triple divided by 255). Bevy's clear pass writes the bytes to the sRGB surface
/// target directly, so the on-screen byte equals the DBC byte (same invariant as the shader output
/// decode). Only writes when the colour actually changes, so frame-to-frame churn stays at zero.
pub(super) fn apply_sky_backdrop(
    lighting: Option<Res<WowLighting>>,
    mut clear: ResMut<ClearColor>,
    mut last: Local<Option<[f32; 3]>>,
) {
    let Some(l) = lighting else {
        return;
    };
    if *last == Some(l.fog_color) {
        return;
    }
    *last = Some(l.fog_color);
    // GAMMA LANE (0161): the buffer holds gamma bytes — the clear writes the authored DBC
    // value RAW (`linear_rgb` = no conversion); the frame's one decode is the FFXGlow combine.
    clear.0 = Color::linear_rgb(l.fog_color[0], l.fog_color[1], l.fog_color[2]);
}
