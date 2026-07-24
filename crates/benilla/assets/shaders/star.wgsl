// Night-sky stars (the `Stars.m2` patches) — premultiplied white dots over the sky dome.
//
// GAMMA-SPACE BLEND — the gamma composite lane (decision 0161): the reference's framebuffer holds
// gamma bytes and all its hardware blending happens in gamma space, so the stars' faithful blend is
// a GAMMA-space alpha blend of white-on-transparent dots (`Stars.blp`, ~98% transparent, mostly low
// alpha). Under that lane our framebuffer likewise holds gamma bytes and ALL blending is gamma, so the
// faithful output is simply premultiplied white at the dot's alpha: `(a, a, a, a)` with
// `a` = texture alpha × the star-curve global alpha (`StandardMaterial::base_color` alpha, set
// per-frame). Over the near-black night sky that lands on screen at gamma value `a` — the
// reference's byte — with zero conversion math.

// SKY-PASS DEPTH (see `sky_order.rs`, "The depth law"): the star dome's own geometry depth is not
// what decides occlusion — the reference draws its whole sky FIRST, in a squashed back depth slice,
// and the opaque world paints over it. Forcing the far depth reproduces that from the transparent
// pass: stars survive only on pixels no world geometry claimed.
#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    forward_io::VertexOutput,
}

/// Reverse-Z "infinitely far" — the sky pass's forced depth (`sky_order.rs`).
const SKY_FAR_DEPTH: f32 = 0.0;

struct StarOutput {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> StarOutput {
    let pbr_input = pbr_input_from_standard_material(in, is_front);
    let a = pbr_input.material.base_color.a; // dot texture alpha × the star-curve global alpha
    var out: StarOutput;
    out.color = vec4<f32>(vec3<f32>(a), a); // premultiplied white — raw (GAMMA LANE, 0161)
    out.depth = SKY_FAR_DEPTH;
    return out;
}
