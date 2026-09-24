// Liquid shader: every liquid surface, one arm per reference liquid renderer.
//   ADT MCLQ river/ocean (`0x6851b0`/`0x685010`): the `ocean0_s.bls` combine of the depth swatch
//     on stage 0 and the animated sheet on stage 1; the only arm with a depth ramp.
//   WMO MLIQ water (`0x6b62e0` category 0), split on `MOGP.flags & 0x48`: exterior `0x6b6630`
//     binds `MapObjExtWater0.bls`, interior `0x6b6420` is fixed-function and unlit; one texture
//     stage, alpha from the per-vertex authored byte.
//   Magma/slime (`0x6b68f0` WMO, `0x68dca0` ADT): the sheet is the opaque body, ADT or WMO alike.
//
// The ADT combine, `Shaders\Pixel\ocean0_s.bls` in `patch.MPQ` (0.25 is the program's own `PARAM`):
//   rgb   = primary·colorTex.rgb + detail.rgb + (secondary + 0.25)·detail.a
//   alpha = colorTex.a
// colorTex is the depth swatch (`swatch_row`) off the zone's `Light.dbc` water bands, taken raw
// (IntBand 16/17 river/lake, 14/15 ocean) and rebuilt every frame (`0x680b90`, refill `0x58acd0`).
// detail is the `lake_a`/`ocean_h` frame: RGB near-black, alpha the ripple, which the authored mips
// fade out with distance, so the sampler's mips and anisotropy matter.
// primary = clamp(ambient + diffuse·N·L): the vertex has no colour, so the default white is tracked
// into ambient+diffuse by `glColorMaterial`, and lighting is on at both water draws.
// secondary is the sun sheen (`sun_sheen`).
// Deviation: the reference's ADT water alpha is the `0xc7fbc0` LUT curve `1.6·(i/63)^8`; this keeps
// the linear swatch alpha, because switching changes the look of every ADT water surface.
//
// Deviation: this is the `specular`/`pixelShaders` = 1 leg. Both CVars (`0x6886a0`/`0x688712`)
// default to 0, where water has no program and no specular, a plain ADD combine and no blend; the
// reference install runs both at 1. An active ARB program bypasses the texture environment.
//
// The material turns culling off for every kind: all four reference liquid passes disable
// GL_CULL_FACE at entry. Blend is per kind, set CPU-side: water blends with depth write off,
// magma/slime are opaque and write depth. Every kind takes planar eye-Z GL_LINEAR fog in gamma
// space (`apply_fog`, as terrain.wgsl); output is raw gamma.

#import bevy_pbr::{
    mesh_functions,
    forward_io::Vertex,
    view_transformations::position_world_to_clip,
    mesh_view_bindings::{view, globals},
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var frames: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var frames_samp: sampler;

struct LiquidParams {
    // x = fullbright (magma/slime); y = ocean swatch; z = interior fog; w = sun-sheen shininess.
    kind: vec4<f32>,
    // x = renderer (`LiquidPath`): 0 = ADT MCLQ, 1 = WMO exterior, 2 = WMO interior; yzw reserved.
    path: vec4<f32>,
    // x reserved; y = frame count; z = scroll flag (1 only on WMO magma/slime, liquid nibbles 6/7);
    // w = clock enable (0 on a deterministic run: frame 0, no scroll).
    anim: vec4<f32>,
};
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var<uniform> w: LiquidParams;

// The shared global light (`lighting::global_light`) that terrain and the models also read,
// mirrored as the prefix of its row layout, which must match row for row.
struct WowLight {
    light_ambient: vec4<f32>,      // 0  rgb = ambient; w = Mod2x scale
    light_diffuse: vec4<f32>,      // 1  rgb = sun diffuse; w = clamp flag
    light_sun: vec4<f32>,          // 2  xyz = sun travel direction (to-light = −xyz)
    light_spec: vec4<f32>,         // 3  rgb = row-9 specular colour; w = terrain shininess, unread
    fog_color: vec4<f32>,          // 4  rgb = scene fog (block 1, gamma 0..1); w = enable (>0.5)
    fog_params: vec4<f32>,         // 5  x = start yd; y = end yd; w = the farclip wall
    _sh: array<vec4<f32>, 6>,      // 6-11  model SH coefficients, unread here
    _sh_c16: vec4<f32>,            // 12
    water_river: array<vec4<f32>, 2>, // 13-14 river/lake shallow, deep (IntBand 16/17); w = alpha
    water_ocean: array<vec4<f32>, 2>, // 15-16 ocean shallow, deep (IntBand 14/15); w = alpha
    _grade: vec4<f32>,             // 17
    wmo_fog_color: vec4<f32>,      // 18 rgb = interior fog (block 2); w = enable
    wmo_fog_params: vec4<f32>,     // 19 x = start yd; y = end yd
};
@group(#{MATERIAL_BIND_GROUP}) @binding(90) var<storage, read> wow_light: WowLight;

struct LiquidVsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) depth: f32,
    // The sun sheen, evaluated per vertex and interpolated, as the reference's fixed-function
    // vertex stage computes it.
    @location(4) secondary_vtx: vec3<f32>,
    // Vertex colour: a WMO interior pool's `MOMT.diffColor`, as the reference's interior water
    // vertex carries it; white elsewhere.
    @location(5) vcolor: vec4<f32>,
    // `MeshTag` bit 30: the room's per-frame interior-fog gate, the reference's `[0xca7f00]`; flat,
    // since each surface is one instance.
    @location(6) @interpolate(flat) room_fog: u32,
}

// Sun sheen (`secondary`): the Blinn highlight `light_spec.rgb · (N·H)^shininess`.
fn sun_sheen(world_normal: vec3<f32>, world_pos: vec3<f32>) -> vec3<f32> {
    let n = normalize(world_normal);
    let to_light = -normalize(wow_light.light_sun.xyz);
    // Local viewer: the reference sets `GL_LIGHT_MODEL_LOCAL_VIEWER = 1` at `0x59cf89`, so the eye
    // vector is per vertex. With an infinite viewer N·H is nearly constant over flat water and the
    // whole sheet saturates at once.
    let to_view = normalize(view.world_position.xyz - world_pos);
    let half_v = normalize(to_light + to_view);
    let ndoth = max(dot(n, half_v), 0.0);
    // The fixed-function `N·L > 0` specular gate is left out: the lighting sun's elevation stays
    // between +20° and +37° (`DayNight::SetDirection`), so N·L on the flat up normal is always > 0.
    //
    // Shininess is water's own `w.kind.w` (6.0, `[0x8102e8]`), not the terrain exponent; material
    // specular is white (`SetRenderState(3, 0xffffffff)`), so `light_spec.rgb` is the whole scale.
    // The reference's specular light colour (`CGLight+0x48` to `glLightfv(GL_SPECULAR)`) is not
    // pinned; row 9 stands in for it.
    return wow_light.light_spec.rgb * pow(ndoth, max(w.kind.w, 1.0));
}

// The liquid clock: `globals.time` seconds under the build-time enable `anim.w`.
fn anim_time() -> f32 {
    return w.anim.w * globals.time;
}

// The 24 fps frame flip, 30 frames over 1.25 s (`FUN_0068aac0`), floored to an integer frame as in
// the reference.
fn frame_layer() -> i32 {
    return i32(floor(anim_time() * 24.0) % max(w.anim.y, 1.0));
}

fn apply_scroll(uv: vec2<f32>) -> vec2<f32> {
    // Magma/slime scroll (liquid nibbles 6/7): the reference's stage-0 texture matrix (built at
    // `0x6b68f0`, pushed at `0x6b6ae3`) is the identity with element 13, the v translate, set to
    // `fmod(t, 10) · 0.1` (rate `[0x801620]`, period `[0x80e5a0]`), which is `t += phase`. It is
    // rebuilt per draw off a millisecond clock, so the scroll is continuous; its phase is machine
    // uptime (`GetTickCount`), so only rate and period match. REPEAT wrapping hides the reset.
    return vec2<f32>(uv.x, uv.y + w.anim.z * fract(anim_time() / 10.0));
}

// Distance fog: planar eye-Z GL_LINEAR in gamma space, as terrain.wgsl. Every liquid kind is
// fogged: GL_FOG defaults on (`0x593bf0` sets state `0x0f` = 1) and no liquid pass turns it off.
// The device holds two fog blocks: block 1 (`+0x70/74/78`) is the scene fog, submitted each frame
// from `WorldFrame::Render` (`0x66ff20`); block 2 (`+0x80/84/88`) is the interior haze, block 1
// eased toward the MFOG/zone target over about 4 s (`0x6cf054`). Only the WMO geometry pass
// (`0x6b51d9`/`0x6b51ea`) and the WMO liquid pass (`0x6b6323` to `0x6b6342`) submit block 2, both
// under the room gate `[0xca7f00]`, so an interior pool fogs like its walls and ADT liquid takes
// block 1. `w.kind.z` is the gate's static half (the group's `MOGI & 0x48`), `room_fog` the
// per-frame half.
fn apply_fog(rgb: vec3<f32>, world_pos: vec3<f32>, room_fog: u32) -> vec3<f32> {
    var fog_color = wow_light.fog_color;
    var fog_span = wow_light.fog_params.xy;
    if (w.kind.z > 0.5 && room_fog != 0u) {
        fog_color = wow_light.wmo_fog_color;
        fog_span = wow_light.wmo_fog_params.xy;
    }
    if (fog_color.w <= 0.5) {
        return rgb;
    }
    let eye_z = -(view.view_from_world * vec4<f32>(world_pos, 1.0)).z;
    let denom = max(fog_span.y - fog_span.x, 0.001);
    let factor = clamp((fog_span.y - eye_z) / denom, 0.0, 1.0);
    return mix(fog_color.xyz, rgb, factor);
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
#ifdef VERTEX_COLORS
    out.vcolor = in.color;
#else
    out.vcolor = vec4<f32>(1.0);
#endif
    // Depth coordinate V (0..1) in UV1.x: the swatch row on ADT water, the alpha ramp on WMO.
    out.depth = in.uv_b.x;
    out.secondary_vtx = sun_sheen(out.world_normal, out.world_position.xyz);
    // ADT surfaces carry no `MeshTag`, so they take the scene fog.
    out.room_fog = mesh_functions::get_tag(in.instance_index) & 0x40000000u;
    return out;
}

// ── The ADT depth swatch ─────────────────────────────────────────────────────────────────────
//
// `FUN_0068a830` fills an 8×64 texture, each row replicated across the 8 columns, with an exact
// byte-space integer accumulator rather than a float lerp (`step = ((c1 - c0) << 8) >> 6` loses
// no bits): `row(i) = c0 + floor(i * (c1 - c0) / 64)`, i = 0..63, so row 63 stops short of the
// deep endpoint.
// On the ocean only (selector 0), the last row's HSV value channel is scaled by 0.9 (`0x68aa13
// fmul [0x8102ec]`), which `floor(0.9 * byte)` per channel reproduces to within 1/255, and its
// alpha is forced to 255 (`0x7bbec0`/`0x7bbec8`).
// Sampling is LINEAR/LINEAR, no mip, clamped (flags `0x201`), so V maps to texel `V*64 - 0.5` and
// the ocean darkening ramps in over the last 1/64 of V instead of stepping.
// The WMO arms do not use this: their opacity is the 256-entry ramp `0xca7f10`, which a plain lerp
// reproduces.
fn swatch_row(shallow: vec4<f32>, deep: vec4<f32>, i: f32, ocean: bool) -> vec4<f32> {
    // RGB endpoints are bytes already (`0x68a8fb`/`0x68a902` read packed dwords from DayNight
    // state), so they round back exactly; alpha endpoints are `LightParams` floats the reference
    // quantizes with `floor(v*255)`.
    let c0 = vec4<f32>(round(shallow.rgb * 255.0), floor(shallow.w * 255.0));
    let c1 = vec4<f32>(round(deep.rgb * 255.0), floor(deep.w * 255.0));
    let row = c0 + floor(i * (c1 - c0) / 64.0);
    if ocean && i >= 63.0 {
        return vec4<f32>(floor(row.rgb * 0.9), 255.0) / 255.0;
    }
    return row / 255.0;
}

/// The swatch sampled at depth coord `v`, LINEAR across the two rows it falls between.
fn swatch_at(shallow: vec4<f32>, deep: vec4<f32>, v: f32, ocean: bool) -> vec4<f32> {
    let t = clamp(v * 64.0 - 0.5, 0.0, 63.0);
    let i0 = floor(t);
    return mix(
        swatch_row(shallow, deep, i0, ocean),
        swatch_row(shallow, deep, min(i0 + 1.0, 63.0), ocean),
        t - i0,
    );
}

@fragment
fn fragment(in: LiquidVsOut) -> @location(0) vec4<f32> {
    // The far-clip wall, as terrain and models: discard beyond `fog_params.w` (0 disables it).
    if (wow_light.fog_params.w > 0.0) {
        let clip_z = -(view.view_from_world * vec4<f32>(in.world_position.xyz, 1.0)).z;
        if (clip_z > wow_light.fog_params.w) {
            discard;
        }
    }

    // The animated frame: the ripple on water, the opaque body on magma/slime. `view.mip_bias` is
    // the render-scale LOD compensation, 0 at native scale and above.
    let detail = textureSampleBias(
        frames,
        frames_samp,
        apply_scroll(in.uv),
        frame_layer(),
        view.mip_bias,
    );

    // Magma/slime: the sheet is the opaque body, unmodulated (the ADT vertex has no colour, the WMO
    // one is `0xffffffff`) and unlit (lighting off on both paths), but fogged.
    if (w.kind.x > 0.5) {
        return vec4<f32>(apply_fog(detail.rgb, in.world_position.xyz, in.room_fog), 1.0);
    }

    // V, computed CPU-side from the authored depth byte: clamp(byte/42) on river/lake (LUT
    // `0xc81768`, `FUN_0068d790`, saturating near 5 yd), clamp(byte/255) on ocean (LUT `0xc7fcd8`,
    // `FUN_0068d690`); both LUTs are built in `FUN_0068c4c0`. One V indexes colour and alpha alike.
    let depth = clamp(in.depth, 0.0, 1.0);
    // The kind's swatch endpoints, packed every frame by `build_light_data`.
    var shallow = wow_light.water_river[0];
    var deep = wow_light.water_river[1];
    if (w.kind.y > 0.5) {
        shallow = wow_light.water_ocean[0];
        deep = wow_light.water_ocean[1];
    }
    // ---- The WMO water arms: opacity is the per-vertex authored byte through the zone's linear
    // alpha ramp (`wmo_water_alpha_v`), carried in `in.depth`.
    let vtx_alpha = mix(shallow.w, deep.w, depth);
    if (w.path.x > 1.5) {
        // ---- WMO interior (`0x6b6420`): fixed-function whatever the CVars (`[0xc9607c]` is never
        // read here), lighting off (`0x0e = 0`), fog on (`0x0f = 1`), combine preset `(0x1f, 3)`:
        //     rgb = clamp(Cf + Ct)      alpha = clamp(Af + At)
        // Cf is the pool's `MOMT.diffColor`, raw, carried in the vertex colour; the vertex has no
        // normal, so no sun term and no sheen. Preset 3 is `GL_ADD` through `GL_COMBINE` on both
        // channels (`COMBINE_ALPHA` at `0x85c2fc`), not the legacy `GL_TEXTURE_ENV_MODE = GL_ADD`
        // whose alpha would be `Af · At`: the ripple adds to the pool's opacity.
        let body = clamp(in.vcolor.rgb + detail.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
        return vec4<f32>(
            apply_fog(body, in.world_position.xyz, in.room_fog),
            clamp(vtx_alpha + detail.a, 0.0, 1.0),
        );
    }
    if (w.path.x > 0.5) {
        // ---- WMO exterior (`0x6b6630`): `Shaders\Pixel\MapObjExtWater0.bls` (bound `0x6b6654`),
        // the shader leg of the `[0xc9607c]` gate:
        //     rgb = primary.rgb + detail.rgb + secondary·detail.a      alpha = primary.a
        // There is no `+0.25`: that is the ADT program's own `PARAM`. Lighting is on (the kernel
        // never sets `0x0e`) with the vertex colour tracked into ambient+diffuse, so
        // primary = band · clamp(ambient + diffuse·max(N·L, 0)). The band is one flat colour for
        // every nibble, the deep river row `LightIntBand` sub-17 (`water_river[1]`), a hard
        // immediate at `0x6b66be`. A DayNight slot is not a sub: `0x6d64d0` moves sub-8 out to
        // `+0x4c`, so slots 8 to 16 are subs 9 to 17 and the kernel's slot 16 is sub-17.
        let n_ext = normalize(in.world_normal);
        let to_light_ext = -normalize(wow_light.light_sun.xyz);
        let primary_ext = clamp(
            wow_light.light_ambient.rgb + wow_light.light_diffuse.rgb
                * max(dot(n_ext, to_light_ext), 0.0),
            vec3<f32>(0.0),
            vec3<f32>(1.0),
        ) * deep.rgb;
        let rgb_ext = primary_ext + detail.rgb + in.secondary_vtx * detail.a;
        // Alpha is `fragment.color.primary` alone: the bound program bypasses the texture
        // environment, so the interior arm's `+ At` does not apply.
        return vec4<f32>(apply_fog(rgb_ext, in.world_position.xyz, in.room_fog), vtx_alpha);
    }

    // The ADT arm. `primary`: the lit white vertex, clamp(ambient + diffuse·N·L).
    let n = normalize(in.world_normal);
    let to_light = -normalize(wow_light.light_sun.xyz);
    let ndotl = max(dot(n, to_light), 0.0);
    let primary = clamp(
        wow_light.light_ambient.rgb + wow_light.light_diffuse.rgb * ndotl,
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );

    let secondary = in.secondary_vtx;

    // colorTex: the stage-0 depth swatch.
    let swatch = swatch_at(shallow, deep, depth, w.kind.y > 0.5);

    // The `ocean0_s.bls` combine.
    var rgb = primary * swatch.rgb + detail.rgb + (secondary + vec3<f32>(0.25)) * detail.a;

    // Alpha is `colorTex.a`, over the same V as the colour: deeper water is more opaque.
    let alpha = swatch.w;

    rgb = apply_fog(rgb, in.world_position.xyz, in.room_fog);

    // Raw gamma out; alpha blends in gamma space like the reference's bytes.
    return vec4<f32>(rgb, alpha);
}
