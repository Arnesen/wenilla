// The player-UI quad material (decision 0068 §2's sorted-quad pass), on the UI **gamma composite
// lane** (decision 0254 — the UI sibling of 0161's world lane).
//
// The reference draws the whole UI through its fixed-function device into an 8-bit backbuffer, so
// every UI multiply AND every UI blend is arithmetic on GAMMA BYTES. We reproduce that here: the
// fragment puts its texel back into the client's byte space, does the tint/premultiply there, and
// hands the pipeline a RAW GAMMA value. Under the `(One, OneMinusSrcAlpha)` blend state the
// hardware then composes in gamma, and (because the target is 8-bit unorm) clamps at every write,
// exactly like the reference's byte buffer:
//   BLEND (EGxBlend 2, `SrcAlpha/OneMinusSrcAlpha`): out = (rgb·a, a) ⇒ dst·(1−a) + rgb·a
//   ADD   (EGxBlend 3, `SrcAlpha/One`):              out = (rgb·a, 0) ⇒ dst      + rgb·a
// so a per-material flag picks the mode without splitting the pipeline. The frame's single
// gamma→linear decode happens once afterwards, in `ui_gamma.wgsl` — the twin of the FFXGlow
// combine owning the world lane's one decode.
//
// Why the encode below, rather than uploading UI art as raw `Rgba8Unorm`: our BLPs load as
// `Rgba8UnormSrgb`, so the sampler hands us a LINEARIZED texel. `linear_to_srgb` puts the authored
// byte back (exact in f32: encode ∘ decode = identity), and it is the one rule that holds for every
// texture the pass samples — sRGB art, the sRGB glyph atlas, and the portrait booth's `Rgba8Unorm`
// bake (which stores linear bytes). Uploading everything raw would be cheaper and would also move
// bilinear filtering into gamma (as the reference filters), but it forces the booth to emit gamma —
// a contained follow-up, not a look change.

#import bevy_sprite::mesh2d_vertex_output::VertexOutput

@group(2) @binding(0) var<uniform> additive: u32;
@group(2) @binding(1) var quad_texture: texture_2d<f32>;
@group(2) @binding(2) var quad_sampler: sampler;
// Mask the quad to its inscribed circle — the live unit PORTRAIT: the real client stamps a round
// alpha stencil into its 64² bake; ours cuts the same circle at draw time, so the opaque booth
// backdrop never pokes past the frame ring's thin band. UV-space, so it holds at any quad size.
@group(2) @binding(3) var<uniform> circular: u32;
// The screen-anchored alpha mask (the MINIMAP's MinimapMask.blp circle, decision 0203): the mask
// spans mask_rect (physical framebuffer px: min.xy, max.xy; z <= x disables), and the fragment's
// alpha multiplies by the mask's ALPHA channel there (MinimapMask.blp is DXT3 — white color, the
// circle ramp authored in its 8-bit alpha; header-verified this session after a mis-read as
// palettized cost a debug cycle). Screen-anchored — not quad UV space — so world-anchored tile
// quads pan under a fixed window. Outside the rect drops. Sampled at level 0 (a branch on
// per-fragment coords would break the uniform control flow implicit derivatives need).
@group(2) @binding(4) var<uniform> mask_rect: vec4<f32>;
@group(2) @binding(5) var mask_texture: texture_2d<f32>;
@group(2) @binding(6) var mask_sampler: sampler;

// Linear → sRGB (the exact IEC 61966-2-1 curve the hardware's sRGB store uses, so this inverts the
// sampler's decode bit-for-bit in f32). Alpha carries no gamma and never passes through here.
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let higher = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    let lower = c * 12.92;
    return select(higher, lower, c <= vec3<f32>(0.0031308));
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let t = textureSample(quad_texture, quad_sampler, in.uv);
#ifdef VERTEX_COLORS
    let c = in.color;
#else
    let c = vec4<f32>(1.0);
#endif
    // Back to the client's byte space, then tint there: `UiQuad.color` is already a client-space
    // sRGB value (FrameXML's `<Color>`, `|cff…`, quality colors), so this multiply IS the FFP's
    // gamma-space `tint × texel`.
    let rgb = linear_to_srgb(t.rgb) * c.rgb;
    var a = t.a * c.a;
    if circular != 0u {
        // Soft ~2%-of-width edge: reads as the ref's stencil at portrait size, no jaggies.
        a *= 1.0 - smoothstep(0.48, 0.5, distance(in.uv, vec2<f32>(0.5)));
    }
    if mask_rect.z > mask_rect.x {
        let muv = (in.position.xy - mask_rect.xy) / (mask_rect.zw - mask_rect.xy);
        let inside = f32(all(muv >= vec2<f32>(0.0)) && all(muv <= vec2<f32>(1.0)));
        let m = textureSampleLevel(mask_texture, mask_sampler, clamp(muv, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0).a;
        a *= m * inside;
    }
    // Premultiply in GAMMA (decision 0160's lesson, here for the UI): the hardware `SrcAlpha` factor
    // would weight a linearized colour and inflate every soft edge and every dim additive skirt.
    if additive != 0u {
        return vec4<f32>(rgb * a, 0.0);
    }
    return vec4<f32>(rgb * a, a);
}
