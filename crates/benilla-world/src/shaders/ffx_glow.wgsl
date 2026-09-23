// FFXGlow, the reference's full-screen glow, from the shipped Shaders\Pixel\FFXGlow.bls and
// FFXGauss4.bls:
//   blur = Gauss4(Gauss4(Box4(scene → ¼), horizontal), vertical)     weights ⅛ ⅜ ⅜ ⅛
//   out  = lerp(screen, blur, z) + w · blur²
// `w` is the zone's LightParams glow weight, `z` the drunk/underwater haze mix (0 sober and dry).
// Tap offsets are exact source-texel counts from texel-centre UVs, with no half-texel shift.
// The math runs on gamma bytes; the float scene RT does not saturate per draw as the reference's
// byte RT does, so every scene read clamps to 1.0 first.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var in_tex: texture_2d<f32>;
@group(0) @binding(1) var in_samp: sampler;
// Combine-pass bindings, unbound in the downsample and blur passes.
@group(0) @binding(2) var blur_tex: texture_2d<f32>;
struct FfxCombine {
    // x = zone glow weight `w` (both combines), y = FFXDeath gate (1 while a ghost, no ramp),
    // z = haze mix, w = deband dither on.
    lane: vec4<f32>,
    // xy = the GlowWave phases `(t mod 3174)/3174` and `(t mod 2805)/2805`, t in ms; zw unused.
    wave: vec4<f32>,
}
@group(0) @binding(3) var<uniform> ffx: FfxCombine;
// The 128×128 wave LUT and its repeat sampler: bound on every combine, sampled only underwater.
@group(0) @binding(4) var wave_tex: texture_2d<f32>;
@group(0) @binding(5) var wave_samp: sampler;



// Box4, the reference's one downsample, full → ¼: 4 bilinear taps at source-texel offsets
// {−1.5, +0.5}², a 4×4 footprint (table 0xce89cc). Each tap clamps to 1.0 before the average,
// as the byte RT saturated; an unclamped super-white would square into a hard glow disc.
@fragment
fn fs_downsample(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(in_tex));
    let one = vec4<f32>(1.0);
    return (min(textureSample(in_tex, in_samp, in.uv + vec2<f32>(-1.5, -1.5) * texel), one)
        + min(textureSample(in_tex, in_samp, in.uv + vec2<f32>(0.5, -1.5) * texel), one)
        + min(textureSample(in_tex, in_samp, in.uv + vec2<f32>(0.5, 0.5) * texel), one)
        + min(textureSample(in_tex, in_samp, in.uv + vec2<f32>(-1.5, 0.5) * texel), one))
        * 0.25;
}

// Gauss4: taps at ±0.5 and ±2.5 source texels (0x6cad10), weights from FFXGauss4.bls.
fn gauss4(uv: vec2<f32>, axis: vec2<f32>) -> vec4<f32> {
    let texel = axis / vec2<f32>(textureDimensions(in_tex));
    return textureSample(in_tex, in_samp, uv - 2.5 * texel) * 0.125
        + textureSample(in_tex, in_samp, uv - 0.5 * texel) * 0.375
        + textureSample(in_tex, in_samp, uv + 0.5 * texel) * 0.375
        + textureSample(in_tex, in_samp, uv + 2.5 * texel) * 0.125;
}

@fragment
fn fs_gauss_h(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    return gauss4(in.uv, vec2<f32>(1.0, 0.0));
}

@fragment
fn fs_gauss_v(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    return gauss4(in.uv, vec2<f32>(0.0, 1.0));
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

// Deviation: an opt-in deband dither (`WOW_DITHER=1`, `ffx.lane.w`); the reference's 8-bit
// framebuffer is undithered. The hash is Bevy's `screen_space_dither`, a pure function of the
// pixel; Bevy applies it only in its tonemapping pass, which `Tonemapping::None` skips.
fn screen_space_dither(frag_coord: vec2<f32>) -> vec3<f32> {
    var dither = vec3<f32>(dot(vec2<f32>(171.0, 231.0), frag_coord)).xxx;
    dither = fract(dither.rgb / vec3<f32>(103.0, 71.0, 97.0));
    return (dither - vec3<f32>(0.5)) / 255.0;
}

// The combine's exit: dither in gamma space, where the present-encode rounds, then the frame's
// one decode, which the sRGB present-encode reverses byte-exactly; none under `GAMMA_OUT`.
fn combine_out(outg: vec3<f32>, alpha: f32, frag_coord: vec2<f32>) -> vec4<f32> {
    let dithered = clamp(
        outg + screen_space_dither(frag_coord) * step(0.5, ffx.lane.w),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
#ifdef GAMMA_OUT
    // As the UI camera's ground pass, store the byte as `ui_quad.wgsl` does (colour premultiplied
    // by alpha, alpha as coverage); the lane's end decodes the frame.
    return vec4<f32>(dithered * alpha, alpha);
#else
    return vec4<f32>(srgb_to_linear(dithered), alpha);
#endif
}

// The combine both entries run, at the fragment's UV or the warped one; `frag_coord` stays
// unwarped because the dither belongs to the destination pixel.
fn combine_body(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    let scene = textureSample(in_tex, in_samp, uv);
    // Clamped like the downsample taps: the screen term must saturate before the glow add.
    let sg = clamp(scene.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let bg = max(textureSample(blur_tex, in_samp, uv).rgb, vec3<f32>(0.0));
    // Scene alpha passes through; only the transparent-clear create booth reads it.
    if ffx.lane.y > 0.0 {
        // FFXDeath replaces the glow while a ghost (one active-pass slot `[0xce8bb4]`), unhazed:
        // luma = sat(dot(screen + w·blur², (0.299, 0.587, 0.144))), 0.144 as shipped, and
        // out = luma + tint · sat(4·luma·(1 − luma)), tint 0x5393A8 from the pack at `0x6cb930`.
        let glowed = sg + ffx.lane.x * bg * bg;
        let luma = clamp(dot(glowed, vec3<f32>(0.299, 0.587, 0.144)), 0.0, 1.0);
        let p = clamp(4.0 * luma * (1.0 - luma), 0.0, 1.0);
        let ghost_tint = vec3<f32>(83.0, 147.0, 168.0) / 255.0; // 0x5393A8
        let outg = min(vec3<f32>(luma) + ghost_tint * p, vec3<f32>(1.0));
        return combine_out(outg, scene.a, frag_coord);
    }
    // The haze cross-fades toward the blur before the glow add; z = max(drunk fraction, 84/255
    // while the eye is submerged), 1 at 100 inebriation. The blur term clamps like the screen.
    let hazed = mix(sg, min(bg, vec3<f32>(1.0)), ffx.lane.z);
    let outg = min(hazed + ffx.lane.x * bg * bg, vec3<f32>(1.0));
    return combine_out(outg, scene.a, frag_coord);
}

@fragment
fn fs_combine(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    return combine_body(in.uv, in.position.xy);
}

// FFXGlowWave, the underwater screen warp: with the camera eye in any liquid the reference runs it
// in place of FFXGlow (`CFFXGlow::Render` `0x6cc630`, GlowWave render `0x6cb310`). It is the same
// combine with the screen and blur texcoords displaced by a 128×128 two-channel LUT holding
// `du = sin(2πx/128)`, `dv = sin(2πy/128)`. The reference builds the wave texcoord per vertex
// (`0x7bca80`); it is affine in the screen UV, so evaluating it per fragment is exact:
//     uv' = uv · R(10°) · S(W/128, 0.88·H/128) · T(p1, p2)
// The phases run off one millisecond clock with periods 3.174 s and 2.805 s. The scale is in
// destination pixels, so the pattern is screen-fixed: 128 px across, 145.45 px down at any
// resolution. Amplitude is 3.0 full-res pixels on both samples (0.75 texels of the ¼ RT).
const WAVE_ROT_RAD: f32 = 0.174532925; // 10°, the reference's rotation row
const WAVE_LUT_EDGE: f32 = 128.0; // the LUT is 128×128 texels ([0xce89a0])
const WAVE_V_SCALE: f32 = 0.88; // [0xce89c0]: the v axis only; u's scale is 1.0
const WAVE_AMPLITUDE_PX: f32 = 3.0; // full-res pixels, both samples

// The warp's screen-UV offset in UV units; `screen` is the full-res scene size in pixels.
fn wave_offset(uv: vec2<f32>, screen: vec2<f32>) -> vec2<f32> {
    let c = cos(WAVE_ROT_RAD);
    let s = sin(WAVE_ROT_RAD);
    let scale = vec2<f32>(screen.x, WAVE_V_SCALE * screen.y) / WAVE_LUT_EDGE;
    let tex = vec2<f32>(c * uv.x - s * uv.y, s * uv.x + c * uv.y) * scale + ffx.wave.xy;
    // Sampled, not evaluated: the reference's sine is the 128-texel LUT under linear filtering.
    // The sampler must repeat: the texcoord spans about 10 cycles, and a clamp pins all but one.
    // The unsigned 8-bit LUT is unbiased by (x − 0.5)·2, as the shipped ps_2_0 permutation does.
    let d = (textureSample(wave_tex, wave_samp, tex).rg - vec2<f32>(0.5)) * 2.0;
    return d * WAVE_AMPLITUDE_PX / screen;
}

@fragment
fn fs_combine_wave(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let screen = vec2<f32>(textureDimensions(in_tex));
    return combine_body(in.uv + wave_offset(in.uv, screen), in.position.xy);
}
