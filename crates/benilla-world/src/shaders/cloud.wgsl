// The visible cloud layer, the reference's sky-dome cloud strip. Its colour math is CPU-side as in
// the reference: the `0x6cfb00` port builds the RGBA image per regen (gradient and sun-aligned glow
// in gamma bytes, alpha the curve-mapped coverage byte), the buffer the reference binds directly as
// its gx texture (`0x58ac70`). This stage samples it and applies the dome's vertex-colour rim fade
// (ring alphas: nine 0xff, then 0x80, 0, 0; `0x6d0530`). The texture is not sRGB, so texels come
// back as raw gamma bytes, blended premultiplied over the gamma sky.

// Depth is the far-plane pin in `sky_vertex.wgsl`; this stage writes colour only, keeping early-Z.

#import bevy_pbr::forward_io::VertexOutput

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var cloud_tex: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var cloud_samp: sampler;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(cloud_tex, cloud_samp, in.uv);
    var a = texel.a;
#ifdef VERTEX_COLORS
    a *= in.color.a; // the dome's rim fade (ring alphas)
#endif
    // Premultiplied gamma blend; the RGB is already the reference's byte math.
    return vec4<f32>(texel.rgb * a, a);
}
