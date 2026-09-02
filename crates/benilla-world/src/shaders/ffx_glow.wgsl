// FFXGlow — the reference's full-screen glow, transcribed from the SHIPPED ARB programs
// (Shaders\Pixel\FFXGlow.bls / FFXGauss4.bls, read straight out of the MPQ) + wow-re's ffxeffects
// node (T3: the CPU-side pass math diffed bit-exact).
//
//   blur   = Gauss4(Gauss4(Box4(scene → ¼), horizontal), vertical)     weights ⅛ ⅜ ⅜ ⅛ (shipped)
//   out    = lerp(screen, blur, z) + w · blur²                          (FFXGlow.bls, exact)
//
// `w` is the per-zone LightParams glow weight (authored data — ≈0.647 in Elwynn), `z` is the haze
// mix — the drunk/underwater screen-toward-blur cross-fade (0 sober and dry; wow-re
// drunk-blur-z.md, decision 1009 §A). The reference runs this in GAMMA bytes, and since 0161 the
// whole frame composites in gamma too — but in a FLOAT buffer, which unlike the reference's byte
// buffer does not saturate at 1.0 per draw. Every read of the scene RT below therefore clamps to
// 1.0 first (the byte semantics restored at the read), does the byte math, and decodes once — the
// present-encode then lands the reference's exact byte.
// The blur² is the whole character of the vanilla glow: mid-tones (0.5 → 0.25) barely bloom,
// highlights bloom fully — a square-law a linear-composite bloom cannot express (decision 0158).
//
// The geometry is fully byte-pinned (wow-re ffxeffects/scratch/blur-geometry.md): one Box4
// downsample full→¼, Gauss4 at ±0.5/±2.5 texels, texel-centre base UVs with zero half-texel
// residual — offsets transcribe as exact source-texel counts.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var in_tex: texture_2d<f32>;
@group(0) @binding(1) var in_samp: sampler;
// Combine-pass extras (unused — and unbound — in the downsample/blur passes).
@group(0) @binding(2) var blur_tex: texture_2d<f32>;
struct FfxCombine {
    // x = the zone glow weight `w` (BOTH combines' blur² weight — the death pack reuses it as
    // primary.w); y = the FFXDeath gate (1 while ghost, else 0 — byte-verified instant, no ramp);
    // z = the drunk/underwater haze mix; w = the deband-dither arm.
    lane: vec4<f32>,
    // xy = the GlowWave phases `(t mod 3174)/3174` and `(t mod 2805)/2805`; zw unused. Written on
    // every frame, read only by `fs_combine_wave`.
    wave: vec4<f32>,
}
@group(0) @binding(3) var<uniform> ffx: FfxCombine;
// The 128×128 wave LUT and its own REPEAT sampler — bound on EVERY combine, sampled only by the
// underwater entry. A bound-but-unsampled texture costs a dry frame nothing, and one layout keeps
// the two entries interchangeable at draw time (the alternative, a second layout, would make the
// pipeline swap a bind-group swap too).
@group(0) @binding(4) var wave_tex: texture_2d<f32>;
@group(0) @binding(5) var wave_samp: sampler;



// The reference's ONE downsample: full → ¼ directly, via Box4 — 4 bilinear taps at source-texel
// offsets {−1.5, +0.5}² (a 4×4 footprint; byte-pinned table 0xce89cc, wow-re blur-geometry.md).
//
// Each tap clamps to 1.0 BEFORE the average: the reference reads a BYTE scene RT, where an
// additive stack saturated at 255 draw by draw — our float buffer keeps summing past 1.0, and an
// unclamped read here feeds that super-white into `w·blur²`, which squares it into a huge hard
// glow disc (the Frost Nova mist stack — ~400 additive puffs — reaches ~5–10 in-buffer). The
// clamp is the byte buffer's saturating-add semantics applied at the lane's read (0161's own
// premise: the framebuffer holds bytes).
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

// Gauss4: taps at ±0.5 (weight 0.375) and ±2.5 (weight 0.125) source texels — byte-pinned
// (0x6cad10 tap arrays; corrects the earlier INFERRED ±1.5 outer tap). Weights are the shipped
// FFXGauss4.bls constants.
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

// The FFXGlow combine, byte-for-byte in gamma space (z = 0: the standard glow pass) — and, while
// the ghost flag is up, the FFXDeath combine that REPLACES it (decision 0308 §7, byte-VERIFIED
// wow-re death-pass.md: one active-pass slot `[0xce8bb4]` — the death composite swaps in whole,
// activation is PLAYER_FLAGS_GHOST only, INSTANT, no time ramp). The shipped FFXDeath.bls:
//   luma = sat(dot(screen + w·blur², (0.299, 0.587, 0.144)))     (Blizzard's 0.144, not 0.114)
//   out  = luma + primary.rgb · sat(4·luma·(1−luma))
// The CPU pack `0x6cb930` builds primary as `(LightParams.glow·255)<<24 | 0x5393A8`: rgb is the
// CONSTANT steel blue-gray 0x53/0x93/0xA8, and primary.w (the blur² weight) is the same zone glow
// the glow pass uses — so ffx.lane.x serves both combines and ffx.lane.y is a pure 0/1 gate.
// **Deband dither** (`ffx.lane.w`, off by default — see `ffx_glow.rs`). The frame's ONE quantization
// to 8 bits happens on `outg` below: it is a gamma-space float, and the surface's sRGB
// present-encode rounds it to a byte. A smooth surface therefore steps in 1/255, and at the
// gradients a lit character actually has (~1.3 levels/px on a bare arm) a body drifting the
// 0.11 px/frame of a breathing idle needs ~7 FRAMES to accumulate one step — so the shading
// updates at ~8 Hz on a 60 Hz display and every iso-luma contour crosses its threshold together.
// Large motions clear a level per frame and hide it completely, which is exactly the
// small-moves-tick / big-moves-smooth split the director reported.
//
// The hash is Bevy's own `screen_space_dither` (its tonemapping pass applies it and we skip that
// pass entirely — `Tonemapping::None` returns before the node does anything, so
// `DebandDither::Enabled` on our camera is dead code). It is a pure function of the PIXEL, not of
// time: a still frame stays bitwise still, while a moving gradient's contour dissolves across
// neighbouring pixels instead of snapping as one line.
//
// **This is a deliberate divergence.** The reference's framebuffer was 8-bit and undithered, so
// byte-exactness and this are mutually exclusive; that is why it is opt-in and why the default
// keeps the byte lane intact.
fn screen_space_dither(frag_coord: vec2<f32>) -> vec3<f32> {
    var dither = vec3<f32>(dot(vec2<f32>(171.0, 231.0), frag_coord)).xxx;
    dither = fract(dither.rgb / vec3<f32>(103.0, 71.0, 97.0));
    return (dither - vec3<f32>(0.5)) / 255.0;
}

// The combine's one exit: dither (when armed) in GAMMA space — the space the present-encode
// rounds in — then the frame's single decode.
fn combine_out(outg: vec3<f32>, alpha: f32, frag_coord: vec2<f32>) -> vec4<f32> {
    let dithered = clamp(
        outg + screen_space_dither(frag_coord) * step(0.5, ffx.lane.w),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
    return vec4<f32>(srgb_to_linear(dithered), alpha);
}

// The combine both entries run — the dry one at the fragment's own UV, the underwater one at the
// warped UV. `frag_coord` stays UNwarped: it seeds the deband dither, which is a property of the
// destination pixel, not of where the scene was read from.
fn combine_body(uv: vec2<f32>, frag_coord: vec2<f32>) -> vec4<f32> {
    // GAMMA LANE (0161): the whole frame is already gamma bytes — the blur ran on gamma (like
    // the reference's byte RTs) and the combine is raw byte math. The single srgb_to_linear here
    // is THE frame's one decode: the sRGB present-encode then restores every byte exactly.
    let scene = textureSample(in_tex, in_samp, uv);
    // Clamped to [0, 1] like the downsample's taps: the byte framebuffer this term stands for
    // could never exceed 255, and an unclamped super-white here keeps the whole additive stack
    // hard-white after the min() below — the screen term must saturate BEFORE the glow add.
    let sg = clamp(scene.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let bg = max(textureSample(blur_tex, in_samp, uv).rgb, vec3<f32>(0.0));
    // The scene alpha rides through untouched: the window surface ignores it and the opaque-clear
    // booths hold 1 everywhere — only the transparent-clear create booth reads it, to composite
    // its baked model straight over the UI page.
    if ffx.lane.y > 0.0 {
        // The FFXDeath pack has no z lane (its COLOR rgb is the constant tint) — the ghost
        // combine reads the un-hazed screen.
        let glowed = sg + ffx.lane.x * bg * bg;
        let luma = clamp(dot(glowed, vec3<f32>(0.299, 0.587, 0.144)), 0.0, 1.0);
        let p = clamp(4.0 * luma * (1.0 - luma), 0.0, 1.0);
        let ghost_tint = vec3<f32>(83.0, 147.0, 168.0) / 255.0; // 0x5393A8
        let outg = min(vec3<f32>(luma) + ghost_tint * p, vec3<f32>(1.0));
        return combine_out(outg, scene.a, frag_coord);
    }
    // The z lane — the drunk/underwater haze: cross-fade the screen toward the ¼-res blur
    // BEFORE the glow add (`out = lerp(screen, blur, z) + w·blur²`, the shipped FFXGlow.bls;
    // wow-re drunk-blur-z.md). z = max(drunkFraction, 84/255 while the eye is submerged) — 0 in
    // the ordinary sober pass, 1 = fully blurred at 100 inebriation. The blur term clamps like
    // the screen term: it stands for the same byte RT.
    let hazed = mix(sg, min(bg, vec3<f32>(1.0)), ffx.lane.z);
    let outg = min(hazed + ffx.lane.x * bg * bg, vec3<f32>(1.0));
    return combine_out(outg, scene.a, frag_coord);
}

@fragment
fn fs_combine(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    return combine_body(in.uv, in.position.xy);
}

// **FFXGlowWave — the underwater screen warp.** (wow-re `ffxeffects/scratch/glow-wave-underwater.md`,
// §5 cross-checked; benilla decision 1824.)
//
// While the camera eye is in ANY liquid the reference does not run the combine above at all:
// `CFFXGlow::Render 0x6cc630` walks a *second* pass list, whose third pass is **FFXGlowWave**
// (ctor `0x6cb1f0`, vtable `0x8114b4`, render `0x6cb310`) in place of FFXGlow. wow-re's note had
// long carried that swap as an undecided "GlowWave, *or* a duplicate Glow"; the arms are mutually
// exclusive (`ret` at `0x6cc403`, `push ebx` at `0x6cc404`) and the plain-Glow arm is unreachable
// on both backends, so underwater is ALWAYS the wave.
//
// The two passes are instruction-identical except that GlowWave binds a third texture — a 128×128
// two-channel bump map holding `du = sin(2πx/128)`, `dv = sin(2πy/128)` — and **displaces both the
// screen and the blur texcoords by it**. Everything downstream (the haze cross-fade, the blur²
// add, the gamma lane) is unchanged, which is why both entries share [`combine_body`]: the warp is
// a change of *where* the combine reads, not of what it computes.
//
// The wave texcoord is an affine map of the screen UV. The reference builds it CPU-side per vertex
// (`0x7bca80`) and hands the GPU three interpolated texcoord streams; an affine function of an
// interpolated UV is the interpolation of that affine function, so evaluating it per fragment here
// is the same map, not an approximation of it:
//
//     uv' = uv · R(10°) · S(W/128, 0.88·H/128) · T(p1, p2)
//
// with the two phases running INDEPENDENTLY off one millisecond clock — `(t mod 3174)/3174` and
// `(t mod 2805)/2805`, i.e. 3.174 s and 2.805 s, rejoining only every ~49 minutes. The scale
// constants multiply the destination's pixel dimensions (a `rewave.md` gloss had them multiplying
// the phases; corrected at the bytes), so the wavelength is 128 px across and 145.45 px down
// REGARDLESS of resolution, and the pattern is screen-fixed rather than world-locked.
//
// Amplitude is **3.0 full-res pixels** on both samples — the reference reaches that number twice
// over, `(3.0, 0, 0, 3.0)` texels of the full RT and `0.75` texels of the ¼ RT being the same
// distance, which is the cross-check that the blur sample warps by the same amount as the screen.
const WAVE_ROT_RAD: f32 = 0.174532925; // 10°, the reference's rotation row
const WAVE_LUT_EDGE: f32 = 128.0; // the LUT is 128×128 texels ([0xce89a0]'s 7-field descriptor)
const WAVE_V_SCALE: f32 = 0.88; // [0xce89c0] — the v axis alone carries it; u's twin is 1.0
const WAVE_AMPLITUDE_PX: f32 = 3.0; // full-res pixels, both samples

// The screen-UV offset the warp applies, in UV units. `screen` is the full-res scene in pixels.
fn wave_offset(uv: vec2<f32>, screen: vec2<f32>) -> vec2<f32> {
    let c = cos(WAVE_ROT_RAD);
    let s = sin(WAVE_ROT_RAD);
    let scale = vec2<f32>(screen.x, WAVE_V_SCALE * screen.y) / WAVE_LUT_EDGE;
    let tex = vec2<f32>(c * uv.x - s * uv.y, s * uv.x + c * uv.y) * scale + ffx.wave.xy;
    // SAMPLED, not evaluated: 128 texels of `sin` under LINEAR/REPEAT *is* the reference's sine —
    // a piecewise-linear one, and its own. REPEAT is load-bearing and was the round's costliest
    // near-miss: read as CLAMP, the effect dies over 90% of the screen (the scale runs the
    // texcoord to 10 cycles across, so all but the first would pin to the edge texel).
    //
    // The 8-bit pack is biased back exactly as the shipped ps_2_0 permutation does — that blob
    // carries `def c2 = (−0.5, 1.0, …)` and two extra instructions spelling `(x − 0.5)·2`, because
    // the caps tier the live client selects hands it an UNSIGNED texture.
    let d = (textureSample(wave_tex, wave_samp, tex).rg - vec2<f32>(0.5)) * 2.0;
    return d * WAVE_AMPLITUDE_PX / screen;
}

@fragment
fn fs_combine_wave(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let screen = vec2<f32>(textureDimensions(in_tex));
    return combine_body(in.uv + wave_offset(in.uv, screen), in.position.xy);
}
