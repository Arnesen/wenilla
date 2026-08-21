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
// `Texture:SetDesaturated(1)` — the greyed-out icon (decisions 1327, 1330). This is the verb every
// disabled affordance in the reference is built on: a bag icon under the game menu, an unusable pet
// action, a disconnected party portrait — and the unavailable talent B162 reported.
//
// It is not a tint and not a stage flag: `+0x128` on the texture object is a `CGxShader*`, and the
// one shader the UI ever loads is `Shaders\Pixel\Desaturate.bls` (wow-re
// `system/ui/scratch/texture-desaturate-law.md`, VERIFIED at the asset's own bytes). Its whole body
// is four instructions, and the two that matter are reproduced exactly below:
//
//     MUL result.color.w   , fragment.color.primary, texel   ; a = vertexColour.a x texel.a
//     DP3 result.color.xyz , texel, c[0]                     ; rgb = dot(texel.rgb, LUMA)
//
// **The vertex colour's RGB is DISCARDED, not modulated in.** A bound fragment program supersedes
// the fixed-function stage chain entirely, so the `MODULATE(TEXTURE, DIFFUSE)` law that governs
// every other UI quad simply does not run here — there is no desaturate-then-tint or
// tint-then-desaturate, the tint is *absent*. FrameXML walks straight into this:
// `SetItemButtonDesaturated(button, 1, 0.65, 0.65, 0.65)` still SETS that 0.65, and on
// shader-capable hardware it has no effect on colour. Only its ALPHA survives, which is why the
// alpha multiply below is shared with the ordinary path rather than special-cased — dropping it
// would make every desaturated icon ignore `SetAlpha` and its frame's alpha.
//
// The fold runs on the GAMMA byte (after `linear_to_srgb`) because the reference's UI is 8-bit
// arithmetic end to end; greying a linearized texel would land a different byte than the client's.
@group(2) @binding(7) var<uniform> desaturate: u32;
// The sampled texture is already PREMULTIPLIED — a portrait/paper-doll/dressing-room booth bake,
// and nothing else (see `UiQuad::premultiplied`).
//
// Every other texture this pass samples is authored STRAIGHT (a BLP's rgb means nothing where its
// alpha is 0), so the premultiply below has to happen here. A booth render target is the opposite
// by construction: its opaque geometry wrote `a = 1`, its alpha batches blended over that, and its
// ADDITIVE particles added light while contributing NO coverage (`wow_effect.wgsl` returns
// `(rgb·a, 0)`). Colour is emitted light, alpha is coverage — premultiplied. Weighting it by its own
// alpha again multiplies exactly the emitted light that sits over EMPTY pane space by zero, which is
// how a transparent-clear pane lost the R14 pauldrons' fire entirely and kept a weapon glow only
// where it overlapped the model's own opaque pixels.
@group(2) @binding(8) var<uniform> premultiplied: u32;
// **Alpha TEST** reference — `<= 0` disables. The WMO-interior minimap tiles, and nothing else so
// far: the reference draws them under EGxBlend **1**, whose applicator `glDisable`s blending
// outright, with the `SetRenderState` id-7→id-8 cascade arming `glAlphaFunc(GL_GEQUAL,
// 0.87843144)` — `.data 0x85ad20[1] = 224`, times the f32 reciprocal of 255 (wow-re
// `system/minimap/scratch/wmo-interior-minimap-composite.md`). So a tile fragment either writes
// FULLY OPAQUE or is discarded; partial coverage does not exist on that path, and two overlapping
// group tiles can never leave the clear colour showing between them. Blending them the ordinary
// way leaves `(1−a)(1−b)` of the black clear at every boundary — B141's "odd black lines".
//
// The tested value is `texel.a × colour.a`, the client's MODULATE of the texel against the
// stride-0 vertex dword `(frameAlpha << 24) | 0xFFFFFF`. The SCREEN MASK is deliberately NOT in
// it: the reference alpha-tests each tile into an offscreen and cuts the round mask at the BLIT,
// so folding the mask ramp into the test would saw the disc's soft rim into a hard, undersized
// circle.
@group(2) @binding(9) var<uniform> alpha_ref: f32;

// ITU-R BT.601 luma — the `PARAM c[0]` of that shader, read as raw f32 words: `0x3E991687`,
// `0x3F1645A2`, `0x3DE978D5`. Not `(0.3, 0.3, 0.3)`, not `(0.3, 0.59, 0.11)` (this file's own first
// guess, corrected by the carve), not BT.709.
//
// **Do not normalise these and do not compute them in f64.** They sum to 1.0000000074505806, so a
// white texel evaluates just above 1.0 and relies on the output clamp; the ARB text and the D3D
// blob round-trip to the same three f32 words, so the backend cannot change a bit and neither may
// we.
const LUMA: vec3<f32> = vec3<f32>(0.299, 0.587, 0.114);

// Linear → sRGB (the exact IEC 61966-2-1 curve the hardware's sRGB store uses, so this inverts the
// sampler's decode bit-for-bit in f32). Alpha carries no gamma and never passes through here.
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let higher = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    let lower = c * 12.92;
    return select(higher, lower, c <= vec3<f32>(0.0031308));
}

// sRGB → linear — the inverse of `linear_to_srgb`, for a texture uploaded UNDECODED so the hardware
// filters its authored bytes (the minimap tiles' `GL_SKIP_DECODE_EXT`). The conversion happens here,
// after the filter, which is exactly the order the reference's fixed-function pipe uses.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let higher = pow((max(c, vec3<f32>(0.0)) + 0.055) / 1.055, vec3<f32>(2.4));
    let lower = c / 12.92;
    return select(higher, lower, c <= vec3<f32>(0.04045));
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
    let texel = linear_to_srgb(t.rgb);
    // The desaturated arm REPLACES the modulate — see the `desaturate` binding above. `c.rgb` is
    // deliberately unread here; only `c.a` carries into the alpha below, as it does on both paths.
    var rgb = texel * c.rgb;
    if desaturate != 0u {
        rgb = vec3<f32>(dot(texel, LUMA));
    }
    // `k` is the COVERAGE the UI itself imposes — the vertex colour's alpha (`SetAlpha`, the frame's
    // inherited alpha) and the two masks. It is kept apart from the texel's OWN alpha `t.a` because
    // the two premultiply differently: `k` scales a premultiplied source's colour and alpha alike,
    // while `t.a` must weight the colour only when the source is straight.
    var k = c.a;
    if circular != 0u {
        // Soft ~2%-of-width edge: reads as the ref's stencil at portrait size, no jaggies.
        k *= 1.0 - smoothstep(0.48, 0.5, distance(in.uv, vec2<f32>(0.5)));
    }
    if mask_rect.z > mask_rect.x {
        let muv = (in.position.xy - mask_rect.xy) / (mask_rect.zw - mask_rect.xy);
        let inside = f32(all(muv >= vec2<f32>(0.0)) && all(muv <= vec2<f32>(1.0)));
        let m = textureSampleLevel(mask_texture, mask_sampler, clamp(muv, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0).a;
        k *= m * inside;
    }
    // The alpha TEST arm (see `alpha_ref`): pass ⇒ fully opaque, fail ⇒ nothing.
    //
    // It returns the **un-encoded** texel, not `rgb`: this arm draws only into the minimap's own
    // 256² composite target, which is un-encoded float like the portrait booths' (decisions
    // 0254/0804 — the UI arc does its one sRGB encode at the end, so a target that pre-encoded
    // would land a second one downstream). The blit quad that samples that target is an ordinary
    // quad and takes the `linear_to_srgb` above, which is where the authored byte comes back.
    // Nothing is lost by skipping it here: the composite does no colour arithmetic at all — no
    // blend, no day-night tint, a white vertex colour — so this path is a pure copy, and there is
    // no gamma-space multiply to preserve.
    if alpha_ref > 0.0 {
        if t.a * c.a < alpha_ref {
            discard;
        }
        // The tile arrived as gamma bytes (SKIP_DECODE) and the composite target is un-encoded, so
        // the conversion the sampler did not do happens HERE — after the filter, which is the point.
        return vec4<f32>(srgb_to_linear(t.rgb) * c.rgb, 1.0);
    }
    let a = t.a * k;
    // Premultiply in GAMMA (decision 0160's lesson, here for the UI): the hardware `SrcAlpha` factor
    // would weight a linearized colour and inflate every soft edge and every dim additive skirt.
    // An already-premultiplied source (a booth bake) takes `k` alone — folding in `t.a` a second
    // time is the double multiply that erased the panes' effects.
    let weight = select(a, k, premultiplied != 0u);
    if additive != 0u {
        return vec4<f32>(rgb * weight, 0.0);
    }
    return vec4<f32>(rgb * weight, a);
}
