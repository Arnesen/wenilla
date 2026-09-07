// The visible CLOUD LAYER — the reference's sky-dome cloud strip (wow-re
// `cloud-coverage-pipeline.md` §3 + Addendum A).
//
// All color math already happened CPU-side, exactly like the reference: the kernel's `0x6cfb00`
// port builds the RGBA image per regen (gradient + sun-aligned glow in gamma bytes, alpha = the
// curve-mapped coverage byte) and uploads it — the reference binds that color buffer zero-copy to
// its gx texture (`0x58ac70`). The fragment therefore only samples the texel and applies the dome
// mesh's vertex-colour rim fade (ring alphas 0xff×9, 0x80, 0, 0 — `0x6d0530`).
//
// GAMMA: the texels are gamma bytes in a NON-sRGB texture (sampling returns them raw), and the
// output is a premultiplied-gamma blend over the (gamma) sky, like the celestial discs
// (decision 0161).

//
// SKY-PASS DEPTH (see `sky_order.rs`, "The depth law"): the dome's radius does not decide occlusion —
// the reference draws the whole sky first, in a squashed back depth slice, and the opaque world paints
// over it. The far depth reproduces that: clouds survive only where no world geometry drew. It is
// pinned in the VERTEX stage (`sky_vertex.wgsl`, shared by every sky shader); this fragment writes
// colour only and keeps its early-Z (2016).

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
