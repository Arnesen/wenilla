// The player-UI quad material, on the UI gamma composite lane. The reference draws the UI
// fixed-function into an 8-bit backbuffer, so every UI multiply and blend is arithmetic on gamma
// bytes: the fragment returns its texel to byte space, tints and premultiplies there, and outputs
// raw gamma, which the `(One, OneMinusSrcAlpha)` blend composes in gamma, clamped at every write by
// the 8-bit unorm target:
//   BLEND (EGxBlend 2, `SrcAlpha/OneMinusSrcAlpha`): out = (rgb·a, a) ⇒ dst·(1−a) + rgb·a
//   ADD   (EGxBlend 3, `SrcAlpha/One`):              out = (rgb·a, 0) ⇒ dst      + rgb·a
// so a per-material flag picks the mode on one pipeline. The frame's one gamma-to-linear decode
// is `ui_gamma.wgsl`.
//
// BLPs load as `Rgba8UnormSrgb`, so the sampler returns a linearized texel and `linear_to_srgb`
// restores the authored byte (exact in f32). That holds for every texture here, the portrait
// booth's `Rgba8Unorm` bake (which stores linear bytes) included, except a SKIP_DECODE upload
// (`gamma_texel`), which already holds the byte and must not be encoded twice.
// Deviation: bilinear filtering runs on the decoded texel, not on the gamma byte the reference
// filters, because the booth bake stores linear bytes.

#import bevy_sprite::mesh2d_functions as mesh_functions

// The mesh vertex at bevy's fixed Mesh2d locations (`Mesh2dPipeline::specialize`: POSITION 0, UV_0
// 2, COLOR 4); `VERTEX_UVS`/`VERTEX_COLORS` are defined only for attributes the mesh carries.
struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
#ifdef VERTEX_UVS
    @location(2) uv: vec2<f32>,
#endif
#ifdef VERTEX_COLORS
    @location(4) color: vec4<f32>,
#endif
}

struct VertexOutput {
    // Clip position out; in the fragment, the pixel coordinate (physical px) the screen mask uses.
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
#ifdef VERTEX_COLORS
    @location(1) color: vec4<f32>,
#endif
    // The run's colour off the per-instance `MeshTag` (a one-colour run draws white vertices), so a
    // colour change is a component write, not a mesh or material change. One byte per channel, as
    // the reference's `CImVector` (`SetVertexColor` quantises `×255 + 0.5` and folds the frame's
    // alpha in bytes). Stored complemented, so an entity with no `MeshTag` (read as 0) is opaque
    // white, untinted.
    @location(2) @interpolate(flat) tint: vec4<f32>,
}

@vertex
fn vertex(vertex: Vertex) -> VertexOutput {
    var out: VertexOutput;
    let world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);
    let world_position = mesh_functions::mesh2d_position_local_to_world(
        world_from_local,
        vec4<f32>(vertex.position, 1.0),
    );
    out.position = mesh_functions::mesh2d_position_world_to_clip(world_position);
#ifdef VERTEX_UVS
    out.uv = vertex.uv;
#else
    out.uv = vec2<f32>(0.0);
#endif
#ifdef VERTEX_COLORS
    out.color = vertex.color;
#endif
    // `unpack4x8unorm` reads byte 0 into `.x`: r, g, b, a from the low byte up.
    out.tint = unpack4x8unorm(~mesh_functions::get_tag(vertex.instance_index));
    return out;
}

@group(2) @binding(0) var<uniform> additive: u32;
@group(2) @binding(1) var quad_texture: texture_2d<f32>;
@group(2) @binding(2) var quad_sampler: sampler;
// Mask the quad to its inscribed circle in UV space: the unit portrait, whose round alpha stencil
// the reference stamps into its 64² bake and this cuts at draw time.
@group(2) @binding(3) var<uniform> circular: u32;
// The screen-anchored alpha mask (the minimap's `MinimapMask.blp`, DXT3, the circle ramp in its
// alpha): `mask_rect` is its span in physical framebuffer px (min.xy, max.xy; z <= x disables), so
// world-anchored tile quads pan under a fixed window; outside the rect is dropped. Sampled at
// level 0: the sample sits in a branch on per-fragment coordinates, outside the uniform control
// flow implicit derivatives need.
@group(2) @binding(4) var<uniform> mask_rect: vec4<f32>;
@group(2) @binding(5) var mask_texture: texture_2d<f32>;
@group(2) @binding(6) var mask_sampler: sampler;
// `Texture:SetDesaturated(1)`: the texture object's `+0x128` `CGxShader*` binds
// `Shaders\Pixel\Desaturate.bls`, whose two working instructions are
//     MUL result.color.w   , fragment.color.primary, texel   ; a = vertexColour.a x texel.a
//     DP3 result.color.xyz , texel, c[0]                     ; rgb = dot(texel.rgb, LUMA)
// A bound fragment program replaces the fixed-function MODULATE, so the vertex colour's RGB is
// discarded (the 0.65 of FrameXML's `SetItemButtonDesaturated(button, 1, 0.65, 0.65, 0.65)` has
// no effect) and only its alpha survives. The dot runs on the gamma byte, after `linear_to_srgb`.
@group(2) @binding(7) var<uniform> desaturate: u32;
// The texture is already premultiplied: only a portrait, paper-doll or dressing-room booth bake
// (`UiQuad::premultiplied`), whose additive particles add light with no coverage. Every other
// texture is straight alpha and is premultiplied here.
@group(2) @binding(8) var<uniform> premultiplied: u32;
// Alpha-test reference, <= 0 disables; only the WMO-interior minimap tiles. The reference draws
// them under EGxBlend 1: blending off and `glAlphaFunc(GL_GEQUAL, 0.87843144)`
// (`.data 0x85ad20[1]` = 224, times the f32 reciprocal of 255), so a tile fragment is fully opaque
// or discarded. The tested value is `texel.a × colour.a`, the MODULATE against the vertex dword
// `(frameAlpha << 24) | 0xFFFFFF`. The screen mask stays out of it: the reference tests each tile
// into an offscreen and cuts the mask at the blit.
@group(2) @binding(9) var<uniform> alpha_ref: f32;
// The texture was uploaded undecoded (`BlpVariant::MapTile`'s `GL_SKIP_DECODE_EXT`, the minimap
// tiles), so the texel is already the authored gamma byte and skips `linear_to_srgb`. The
// alpha-test arm ignores this flag and decodes explicitly for its un-encoded target.
@group(2) @binding(10) var<uniform> gamma_texel: u32;
// The UV window this quad may sample, `(u_min, v_min, u_max, v_max)`, inset half a texel by the
// producer; an axis with `min > max` is unclamped (UVs past `[0,1]`, the backdrop's tiling).
// `CLAMP_TO_EDGE` clamps at the image edge, not at a `SetTexCoord` crop, so without this a
// magnified atlas cell's outer pixels filter in the neighbouring cell.
@group(2) @binding(11) var<uniform> uv_clamp: vec4<f32>;

// BT.601 luma, `Desaturate.bls`'s `PARAM c[0]` as raw f32 words `0x3E991687`, `0x3F1645A2`,
// `0x3DE978D5`. Do not normalise them or compute them in f64: they sum to 1.0000000074505806, so
// white lands just above 1.0 and relies on the output clamp.
const LUMA: vec3<f32> = vec3<f32>(0.299, 0.587, 0.114);

// Linear to sRGB: the IEC 61966-2-1 curve the hardware's sRGB conversion uses, so this inverts the
// sampler's decode exactly in f32. Alpha has no gamma and never passes through here.
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let higher = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    let lower = c * 12.92;
    return select(higher, lower, c <= vec3<f32>(0.0031308));
}

// sRGB to linear, for an undecoded texture (the minimap tiles): the hardware filters the authored
// bytes and the conversion follows the filter, the reference's fixed-function order.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let higher = pow((max(c, vec3<f32>(0.0)) + 0.055) / 1.055, vec3<f32>(2.4));
    let lower = c / 12.92;
    return select(higher, lower, c <= vec3<f32>(0.04045));
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // Branchless, so the sample stays in uniform control flow for implicit derivatives. The bounds
    // are sorted first because `select` evaluates both arms: a disabled (`min > max`) axis must
    // still hand `clamp` an ordered range.
    let lo = min(uv_clamp.xy, uv_clamp.zw);
    let hi = max(uv_clamp.xy, uv_clamp.zw);
    let uv = select(in.uv, clamp(in.uv, lo, hi), uv_clamp.xy <= uv_clamp.zw);
    let t = textureSample(quad_texture, quad_sampler, uv);
#ifdef VERTEX_COLORS
    let c = in.color * in.tint;
#else
    let c = in.tint;
#endif
    // Back to the client's byte space, then tint there: `UiQuad.color` is a client-space sRGB value
    // (FrameXML `<Color>`, `|cff…`, quality colours), so this is the fixed-function gamma-space
    // `tint × texel`. A SKIP_DECODE texture (`gamma_texel`) is already in byte space.
    let texel = select(linear_to_srgb(t.rgb), t.rgb, gamma_texel != 0u);
    // Desaturate replaces the modulate: `c.rgb` is unread there, and only `c.a` reaches the alpha.
    var rgb = texel * c.rgb;
    if desaturate != 0u {
        rgb = vec3<f32>(dot(texel, LUMA));
    }
    // `k` is the coverage the UI imposes: the vertex alpha (`SetAlpha`, the inherited frame alpha)
    // and the two masks. It stays apart from the texel's own `t.a` because they premultiply
    // differently: `k` scales a premultiplied source's colour and alpha alike, `t.a` weights the
    // colour only on a straight source.
    var k = c.a;
    if circular != 0u {
        // A soft edge 2% of the width wide, like the reference's stencil at portrait size.
        k *= 1.0 - smoothstep(0.48, 0.5, distance(in.uv, vec2<f32>(0.5)));
    }
    if mask_rect.z > mask_rect.x {
        let muv = (in.position.xy - mask_rect.xy) / (mask_rect.zw - mask_rect.xy);
        let inside = f32(all(muv >= vec2<f32>(0.0)) && all(muv <= vec2<f32>(1.0)));
        let m = textureSampleLevel(mask_texture, mask_sampler, clamp(muv, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0).a;
        k *= m * inside;
    }
    // The alpha-test arm (`alpha_ref`): pass is fully opaque, fail draws nothing. It returns the
    // un-encoded texel, not `rgb`: it draws only into the minimap's 256² composite, an un-encoded
    // float target whose blit quad takes `linear_to_srgb`, and the composite does no colour
    // arithmetic, so there is no gamma-space multiply to keep.
    if alpha_ref > 0.0 {
        if t.a * c.a < alpha_ref {
            discard;
        }
        return vec4<f32>(srgb_to_linear(t.rgb) * c.rgb, 1.0);
    }
    let a = t.a * k;
    // Premultiply in gamma: the hardware `SrcAlpha` factor would weight a linearized colour and
    // inflate every soft edge and dim additive skirt. A premultiplied source (a booth bake) takes
    // `k` alone; folding in `t.a` again would zero its additive light over empty pane space.
    let weight = select(a, k, premultiplied != 0u);
    if additive != 0u {
        return vec4<f32>(rgb * weight, 0.0);
    }
    return vec4<f32>(rgb * weight, a);
}
