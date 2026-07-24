// Liquid (lake/river/ocean) shader — a port of the reference's `ocean0_s.bls` path to WGSL.
// RE: docs/knowledge/terrain.md "Liquid" (WoW.exe + ocean0_s.bls + apitrace WoW.17 prog 159).
//
// Ground truth from the live draw (apitrace WoW.17 program 159, ocean0_s.bls) + WoW.exe RE:
//
//   result.rgb = primary*colorTex.rgb + detailTex.rgb + (secondary + 0.25)*detailTex.a
//   result.a   = colorTex.a
//
// where (VERIFIED from the trace's bound textures + uniforms + the binary's depth ramp):
//   * colorTex (unit 0) is the 8×64 depth swatch built by WoW.exe `FUN_0068a830` — a plain **2-endpoint
//     linear lerp** of the zone's dedicated `Light.dbc` water rows, RAW (no ×0.711): `water_shallow.rgb`
//     = IntBand row 16 (river/lake) / 14 (ocean), `water_deep.rgb` = row 17 / 15. Golden-vector-matched
//     to the apitrace swatch ≤1/255 over all 64 rows. (The earlier "reflected sky × 0.711 via
//     `FUN_0068c250`" model fingered the WRONG builder — that fills a separate symmetric grey edge
//     texture, tex 432, never bound on the water unit. Rows 14–17 were right all along.)
//   * detailTex (unit 1) is the animated `lake_a`/`ocean_h` frame: RGB near-black (≈0.014), ALPHA =
//     the ripple. So it adds a faint flat lift + an achromatic shimmer on the crests — NOT the body.
//   * primary = the vertex's lit colour `clamp(ambient + N·L·sun)`. secondary = the specular sheen; P1
//     keeps it + the verified +0.25 constant.
//   * alpha = swatch.a = `mix(water_shallow.w, water_deep.w, V)` over the SAME V as the colour (VERIFIED
//     `WoW.exe FUN_0068a830` α = `127+2·row`, apitrace-confirmed). LightParams endpoints: river 0.5→1.0,
//     ocean 0.75→1.0. Deeper water = more opaque.
//
// A SINGLE swatch row (V) indexes both the colour and the alpha — they track together. V is `clamp(byte/42)`
// for river/lake (VERIFIED `c81768`, saturates ~5 yd → the channel middle hits the deep teal row) and
// byte/255 for ocean (placeholder; ocean uses a non-LUT UV path). (Earlier cuts: ripple-as-colour → black;
// ×8 over-saturated; FLAT colour killed the gradient; sky×0.711 was the wrong builder; `byte/255` was the
// wrong LUT → river middle never went teal. Corrected to rows 14–17 raw lerp + the /42 V, 2026-05-31.)
//
// Two-sided + alpha-blended + depth-write-off comes from the material (AlphaMode::Blend, cull off) =
// the verified MCLQ water render state. Fog + gamma handling mirror terrain.wgsl (planar eye-Z
// GL_LINEAR fog in gamma space; raw gamma out — GAMMA LANE, 0161).

#import bevy_pbr::{
    mesh_functions,
    forward_io::Vertex,
    view_transformations::position_world_to_clip,
    mesh_view_bindings::view,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var frames: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var frames_samp: sampler;

struct LiquidParams {
    light_ambient: vec4<f32>, // rgb = ambient; w unused
    light_diffuse: vec4<f32>, // rgb = sun diffuse; w unused
    light_sun: vec4<f32>,     // xyz = sun TRAVEL dir (to-light = −xyz); w unused
    light_spec: vec4<f32>,    // rgb = sun specular colour; w = shininess (the `secondary` sheen term)
    water_shallow: vec4<f32>, // rgb = shore tint (IntBand row 16 river / 14 ocean, raw); w = shallow alpha
    water_deep: vec4<f32>,    // rgb = deep tint  (IntBand row 17 river / 15 ocean, raw); w = deep alpha (1.0)
    fog_color: vec4<f32>,     // rgb = fog colour (gamma 0..1); w = enable (>0.5)
    fog_params: vec4<f32>,    // x = fog_start yd; y = fog_end yd; zw reserved
    anim: vec4<f32>,          // x = current frame index; y = frame count; zw unused
};
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var<uniform> w: LiquidParams;

struct LiquidVsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) depth: f32,
    // The sun-sheen `secondary` evaluated PER-VERTEX (the faithful 1.12 path: the real client computes
    // the Blinn highlight in its FFP vertex shader and interpolates it across the coarse water mesh).
    // The fragment shader uses this interpolated value directly.
    @location(4) secondary_vtx: vec3<f32>,
}

// Sun sheen (`secondary`): a Blinn highlight of the sun on the flat water surface — the glint that's
// strongest at grazing (sunrise/sunset) sun. `secondary = light_spec.rgb · (N·H)^shininess`. Shared by
// both stages so the per-vertex (faithful) and per-pixel (current) paths run IDENTICAL math — only the
// EVALUATION DOMAIN differs (interpolated vs evaluated per fragment).
fn sun_sheen(world_normal: vec3<f32>, world_pos: vec3<f32>) -> vec3<f32> {
    let n = normalize(world_normal);
    let to_light = -normalize(w.light_sun.xyz);
    let to_view = normalize(view.world_position.xyz - world_pos);
    let half_v = normalize(to_light + to_view);
    let ndoth = max(dot(n, half_v), 0.0);
    return w.light_spec.rgb * pow(ndoth, max(w.light_spec.w, 1.0));
}

@vertex
fn vertex(in: Vertex) -> LiquidVsOut {
    var out: LiquidVsOut;
    let world_from_local = mesh_functions::get_world_from_local(in.instance_index);
    out.world_position =
        mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(in.position, 1.0));
    out.clip_position = position_world_to_clip(out.world_position.xyz);
    out.world_normal = mesh_functions::mesh_normal_local_to_world(in.normal, in.instance_index);
    out.uv = in.uv;
    // Per-vertex MCLQ depth (0..1) packed into UV1.x; drives the opacity ramp.
    out.depth = in.uv_b.x;
    // The faithful per-vertex sun sheen — interpolated across the coarse mesh by the fragment stage.
    out.secondary_vtx = sun_sheen(out.world_normal, out.world_position.xyz);
    return out;
}

@fragment
fn fragment(in: LiquidVsOut) -> @location(0) vec4<f32> {
    // HARD FAR-CLIP WALL (same as terrain/models, see terrain.wgsl): discard water beyond the
    // projection far plane so lakes/rivers don't render past the wall. `fog_params.w` = farclip
    // (0 ⇒ disabled).
    if (w.fog_params.w > 0.0) {
        let clip_z = -(view.view_from_world * vec4<f32>(in.world_position.xyz, 1.0)).z;
        if (clip_z > w.fog_params.w) {
            discard;
        }
    }

    // Animated frame. For water/ocean this is the DETAIL ripple (RGB ≈ near-black, ALPHA = ripple);
    // for magma/slime it is the OPAQUE BODY texture.
    let detail = textureSample(frames, frames_samp, in.uv, i32(round(w.anim.x)));

    // Magma / slime (anim.z > 0.5): the animated texture IS the opaque, fullbright body colour — no
    // depth swatch, no N·L lighting, no fog (VERIFIED wow-re `rf-water-liquid-type-texture-material`:
    // the magma vert-fill is a constant 1.0 with no LUT, on the emissive/no-darken GX render path).
    if (w.anim.z > 0.5) {
        return vec4<f32>(detail.rgb, 1.0);
    }

    // Per-vertex swatch coord V (in `in.depth`, computed CPU-side in wow-formats/liquid.rs): river/lake
    // = `clamp(byte/42)` (VERIFIED WoW.exe `c81768` LUT / `FUN_0068d790`, saturating ~5 yd so the channel
    // middle reaches the deep/teal row), ocean = byte/255 (placeholder, different path). The depth swatch
    // is a plain 2-endpoint lerp (`FUN_0068a830`), so a SINGLE V indexes BOTH the colour and the alpha
    // row: colour `shallow→deep` and opacity `shallow_α→deep_α` track together. (Earlier `×4` colour
    // compression + the gentle `byte/255` V were band-aids for a wrong "V tops at 0.31" belief — removed.)
    let depth = clamp(in.depth, 0.0, 1.0);
    let water_tint = mix(w.water_shallow.rgb, w.water_deep.rgb, depth);

    // Body colour: lit vertex colour × the depth-lerped water-row swatch colour (`primary·colorTex`).
    let n = normalize(in.world_normal);
    let to_light = -normalize(w.light_sun.xyz);
    let ndotl = max(dot(n, to_light), 0.0);
    let primary = clamp(
        w.light_ambient.rgb + w.light_diffuse.rgb * ndotl,
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );

    // Sun sheen (`secondary`): the `ocean0_s.bls` Blinn highlight, computed PER-VERTEX in `fn vertex`
    // and interpolated across the coarse ~4 yd MCLQ mesh — the faithful 1.12 path (the real client
    // evaluates it in its FFP vertex stage). Per-pixel evaluation of the sharply-peaked `pow(N·H,6)`
    // would fill its broad lobe at full value (a brighter, denser sheen); interpolating from the
    // vertices flattens the peak to match the reference. (A per-pixel/per-vertex A/B toggle proved the
    // two visually identical on our mesh — we keep per-vertex as the faithful mechanism; RE:
    // `docs/knowledge/scratch/liquid-depth/fleck-deep.md`.)
    let secondary = in.secondary_vtx;

    // primary·colorTex.rgb  +  detail.rgb  +  (secondary + 0.25)·detail.a   (the ocean0_s.bls math)
    var rgb = primary * water_tint + detail.rgb + (secondary + vec3<f32>(0.25)) * detail.a;

    // Opacity: depth ramp between the shallow/deep LightParams water alphas, over the SAME V as the
    // colour. Deeper = more opaque, up to α=1.0 where V saturates (river/lake byte 42 ≈ 5 yd), so the
    // channel middle is opaque + teal while the shore stays semi-transparent (V→0, α≈0.5) and the bottom
    // shows through (faithful — the pale edge band). One steep V drives both colour and opacity together.
    let alpha = mix(w.water_shallow.w, w.water_deep.w, depth);

    // Distance fog — planar eye-Z, GL_LINEAR, gamma space (mirrors terrain.wgsl); the fog colour is
    // also teal, so far water converges on the haze.
    if (w.fog_color.w > 0.5) {
        let eye_z = -(view.view_from_world * vec4<f32>(in.world_position.xyz, 1.0)).z;
        let denom = max(w.fog_params.y - w.fog_params.x, 0.001);
        let factor = clamp((w.fog_params.y - eye_z) / denom, 0.0, 1.0);
        rgb = mix(w.fog_color.xyz, rgb, factor);
    }


    // GAMMA LANE (0161): raw gamma out; alpha blends in gamma like the reference's bytes.
    return vec4<f32>(rgb, alpha);
}
