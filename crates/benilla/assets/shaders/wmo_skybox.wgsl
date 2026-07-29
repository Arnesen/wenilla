// The WMO **skybox** (`wmo_sky.rs`) — the authored sky a building swaps in for the Light.dbc
// gradient dome while the camera stands in one of its `0x40000` groups. Stratholme's burning city is
// the one place 1.12 content reaches this: `StratholmeSkybox.m2`, a static emissive cube whose three
// texture pairs are the painted red sky.
//
// GAMMA LANE (decision 0161): the model's batches are authored `emissive` + `two-sided` + opaque, so
// the faithful output is the texel's own gamma bytes, unlit and unmodulated — the same treatment the
// gradient dome's stops get. No fog: this IS the backdrop the world's fog converges toward, and the
// reference's sky pass runs before (and outside) the fog stage.
//
// SKY-PASS DEPTH (see `sky_order.rs`, "The depth law"): the box is drawn camera-anchored at model
// scale, so its geometry sits *inside* the world — the shell radius must not decide occlusion.
// Forcing the far depth makes it a true backdrop: it fills only the pixels no world geometry claimed.
#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    forward_io::VertexOutput,
}

/// Reverse-Z "infinitely far" — the sky pass's forced depth (`sky_order.rs`).
const SKY_FAR_DEPTH: f32 = 0.0;

struct WmoSkyboxOutput {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> WmoSkyboxOutput {
    let pbr_input = pbr_input_from_standard_material(in, is_front);
    var out: WmoSkyboxOutput;
    // Opaque backdrop: the texel's raw gamma bytes, alpha forced to 1 (the sky art carries none).
    out.color = vec4<f32>(pbr_input.material.base_color.rgb, 1.0);
    out.depth = SKY_FAR_DEPTH;
    return out;
}
