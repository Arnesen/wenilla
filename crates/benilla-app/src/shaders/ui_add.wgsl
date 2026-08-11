// WoW `alphaMode="ADD"` for Bevy UI — the glue highlight overlays (`ButtonHilight-Square`, the
// arrow/panel/scroll `Highlight`s, `UI-Common-MouseHilight`) draw with a TRUE additive blend
// state (set Rust-side in `AddUiMaterial::specialize`); this fragment only samples the authored
// sub-rect (uv min/max — the sheets store the button region in a corner/quarter).
//
// On the UI gamma composite lane (decision 0254) like its siblings `ui_node_gamma.wgsl` /
// `ui_slice_gamma.wgsl`: the texel goes back to the client's byte space, so the `SrcAlpha/One`
// blend state resolves to the reference's byte add `dst + texel·α` (EGxBlend 3 =
// `glBlendFunc(GL_SRC_ALPHA, GL_ONE)`; wow-re `system/gx/gx.md`). Emitting the linear texel instead
// would let the hardware add a linearised glow, landing every highlight's dim skirt at a different
// weight than the reference's — the same error 0160 caught for the world's additive sources.
#import bevy_ui::ui_vertex_output::UiVertexOutput

@group(1) @binding(0) var texture: texture_2d<f32>;
@group(1) @binding(1) var texture_sampler: sampler;
@group(1) @binding(2) var<uniform> rect: vec4<f32>;

// Linear → sRGB (IEC 61966-2-1) — the exact inverse of the sampler's sRGB decode.
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let higher = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    let lower = c * 12.92;
    return select(higher, lower, c <= vec3<f32>(0.0031308));
}

@fragment
fn fragment(in: UiVertexOutput) -> @location(0) vec4<f32> {
    let uv = mix(rect.xy, rect.zw, in.uv);
    let texel = textureSample(texture, texture_sampler, uv);
    return vec4<f32>(linear_to_srgb(texel.rgb), texel.a);
}
