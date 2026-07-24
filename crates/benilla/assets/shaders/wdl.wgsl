// WDL distant-terrain shader — the coarse horizon hills the reference draws beyond the streamed
// detailed tiles (docs/knowledge/terrain.md "WDL"). Both stages custom.
//
// The reference draws WDL UNLIT + UNTEXTURED with white vertex diffuse, then fogs it with the SAME
// scene fog as terrain/M2/WMO (VERIFIED apitrace WoW.8: prog 96 vertex-white, fog.color matching the
// zone haze). So the colour is entirely the fog: past `fog_end` it's pure haze, and the visible result
// is fog-coloured hill silhouettes occluding the (un-fogged) sky. Fog math + gamma handling mirror
// terrain.wgsl exactly: planar eye-Z, GL_LINEAR, gamma space; the output is raw gamma — the buffer
// holds bytes and the frame's one decode lives in the FFXGlow combine (GAMMA LANE, 0161).

#import bevy_pbr::{
    mesh_functions,
    forward_io::Vertex,
    view_transformations::position_world_to_clip,
    mesh_view_bindings::view,
}

struct WdlParams {
    fog_color: vec4<f32>,  // rgb = Light.dbc row 7 (gamma 0..1); w = enable (>0.5 ⇒ blend)
    fog_params: vec4<f32>, // x = fog_start yd; y = fog_end yd; zw reserved
};
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> w: WdlParams;

struct WdlVsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
}

@vertex
fn vertex(in: Vertex) -> WdlVsOut {
    var out: WdlVsOut;
    let world_from_local = mesh_functions::get_world_from_local(in.instance_index);
    out.world_position =
        mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(in.position, 1.0));
    out.clip_position = position_world_to_clip(out.world_position.xyz);
    return out;
}

@fragment
fn fragment(in: WdlVsOut) -> @location(0) vec4<f32> {
    // PLANAR eye-Z (view-space depth), NOT radial — same as terrain.wgsl (apitrace-verified). Used for
    // both the far-clip partition (below) and the fog.
    let eye_z = -(view.view_from_world * vec4<f32>(in.world_position.xyz, 1.0)).z;
    // INVERSE of the detailed-world far-clip wall: WDL draws ONLY beyond the wall (`fog_params.w` =
    // farclip), while terrain/wow_model/liquid discard *beyond* it — so the detailed world and the WDL
    // backdrop partition cleanly at the SAME plane (no overlap, no gap). WDL can therefore be drawn as a
    // full ring (a constant backdrop) without toggling against the detailed streaming radius, which is
    // what made the horizon pop in/out. The coarse-vs-fine height seam at the plane sits at ~farclip,
    // deep in the fog, so it's hidden (the reference relies on the same). (0 ⇒ disabled.)
    if (w.fog_params.w > 0.0 && eye_z < w.fog_params.w) {
        discard;
    }
    // White vertex diffuse (the reference's WDL colour); fog does all the colouring.
    var rgb = vec3<f32>(1.0);
    if (w.fog_color.w > 0.5) {
        let denom = max(w.fog_params.y - w.fog_params.x, 0.001);
        let factor = clamp((w.fog_params.y - eye_z) / denom, 0.0, 1.0);
        rgb = mix(w.fog_color.xyz, rgb, factor);
    }
    // GAMMA LANE (0161): raw gamma out; the frame decodes once in the FFXGlow combine.
    return vec4<f32>(rgb, 1.0);
}
