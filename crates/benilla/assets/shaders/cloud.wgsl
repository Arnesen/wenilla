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
// over it. Forcing the far depth reproduces that: clouds survive only where no world geometry drew.

#import bevy_pbr::forward_io::VertexOutput

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var cloud_tex: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var cloud_samp: sampler;

/// Reverse-Z "infinitely far" — the sky pass's forced depth (`sky_order.rs`).
const SKY_FAR_DEPTH: f32 = 0.0;

struct CloudOutput {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

@fragment
fn fragment(in: VertexOutput) -> CloudOutput {
    let texel = textureSample(cloud_tex, cloud_samp, in.uv);
    var a = texel.a;
#ifdef VERTEX_COLORS
    a *= in.color.a; // the dome's rim fade (ring alphas)
#endif
    var out: CloudOutput;
    // Premultiplied gamma blend; the RGB is already the reference's byte math.
    out.color = vec4<f32>(texel.rgb * a, a);
    out.depth = SKY_FAR_DEPTH;
    return out;
}
