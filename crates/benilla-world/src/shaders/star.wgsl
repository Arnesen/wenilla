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
// and the opaque world paints over it. The far depth reproduces that from the transparent pass:
// stars survive only on pixels no world geometry claimed. It is pinned in the VERTEX stage
// (`sky_vertex.wgsl`, shared by every sky shader); this fragment writes colour only (2016).
#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    forward_io::VertexOutput,
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> @location(0) vec4<f32> {
    let pbr_input = pbr_input_from_standard_material(in, is_front);
    let a = pbr_input.material.base_color.a; // dot texture alpha × the star-curve global alpha
    return vec4<f32>(vec3<f32>(a), a); // premultiplied white — raw (GAMMA LANE, 0161)
}
