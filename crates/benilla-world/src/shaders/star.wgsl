// Night-sky stars (the `Stars.m2` patches): white `Stars.blp` dots, alpha-blended in gamma space
// like the reference as premultiplied white, landing on the near-black sky as the reference's
// byte. Depth is the far-plane pin in `sky_vertex.wgsl`; this stage writes colour only.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    forward_io::VertexOutput,
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> @location(0) vec4<f32> {
    let pbr_input = pbr_input_from_standard_material(in, is_front);
    let a = pbr_input.material.base_color.a; // texel alpha × the per-frame `base_color` alpha
    return vec4<f32>(vec3<f32>(a), a); // premultiplied white, raw gamma
}
