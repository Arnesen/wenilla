// FFXGlow — the reference's full-screen glow, transcribed from the SHIPPED ARB programs
// (Shaders\Pixel\FFXGlow.bls / FFXGauss4.bls, read straight out of the MPQ) + wow-re's ffxeffects
// node (T3: the CPU-side pass math diffed bit-exact).
//
//   blur   = Gauss4(Gauss4(Box4(scene → ¼), horizontal), vertical)     weights ⅛ ⅜ ⅜ ⅛ (shipped)
//   out    = lerp(screen, blur, z) + w · blur²                          (FFXGlow.bls, exact)
//
// `w` is the per-zone LightParams glow weight (authored data — ≈0.647 in Elwynn), `z` is the haze
// mix (0 in the standard glow pass). The reference runs this in GAMMA bytes, and since 0161 the
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
// x = the zone glow weight `w` (BOTH combines' blur² weight — the death pack reuses it as
// primary.w); y = the FFXDeath gate (1 while ghost, else 0 — byte-verified instant, no ramp);
// zw reserved.
@group(0) @binding(3) var<uniform> glow: vec4<f32>;

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
// the glow pass uses — so glow.x serves both combines and glow.y is a pure 0/1 gate.
@fragment
fn fs_combine(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    // GAMMA LANE (0161): the whole frame is already gamma bytes — the blur ran on gamma (like
    // the reference's byte RTs) and the combine is raw byte math. The single srgb_to_linear here
    // is THE frame's one decode: the sRGB present-encode then restores every byte exactly.
    let scene = textureSample(in_tex, in_samp, in.uv);
    // Clamped to [0, 1] like the downsample's taps: the byte framebuffer this term stands for
    // could never exceed 255, and an unclamped super-white here keeps the whole additive stack
    // hard-white after the min() below — the screen term must saturate BEFORE the glow add.
    let sg = clamp(scene.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let bg = max(textureSample(blur_tex, in_samp, in.uv).rgb, vec3<f32>(0.0));
    let glowed = sg + glow.x * bg * bg;
    // The scene alpha rides through untouched: the window surface ignores it and the opaque-clear
    // booths hold 1 everywhere — only the transparent-clear create booth reads it, to composite
    // its baked model straight over the UI page.
    if glow.y > 0.0 {
        let luma = clamp(dot(glowed, vec3<f32>(0.299, 0.587, 0.144)), 0.0, 1.0);
        let p = clamp(4.0 * luma * (1.0 - luma), 0.0, 1.0);
        let ghost_tint = vec3<f32>(83.0, 147.0, 168.0) / 255.0; // 0x5393A8
        let outg = min(vec3<f32>(luma) + ghost_tint * p, vec3<f32>(1.0));
        return vec4<f32>(srgb_to_linear(outg), scene.a);
    }
    let outg = min(glowed, vec3<f32>(1.0));
    return vec4<f32>(srgb_to_linear(outg), scene.a);
}
