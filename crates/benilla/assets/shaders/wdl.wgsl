// WDL distant-terrain shader — the coarse horizon hills the reference draws beyond the streamed
// detailed tiles (docs/knowledge/terrain.md "WDL"). Both stages custom.
//
// The reference draws WDL UNLIT + UNTEXTURED with white vertex diffuse, then fogs it with the SAME
// scene fog as terrain/M2/WMO (VERIFIED apitrace WoW.8: prog 96 vertex-white, fog.color matching the
// zone haze). So the colour is entirely the fog: past `fog_end` it's pure haze, and the visible result
// is fog-coloured hill silhouettes occluding the (un-fogged) sky. Fog math + gamma handling mirror
// terrain.wgsl exactly: planar eye-Z, GL_LINEAR, gamma space; the output is raw gamma — the buffer
// holds bytes and the frame's one decode lives in the FFXGlow combine (GAMMA LANE, 0161).
//
// ## The far band is a BACKDROP, not the far half of a plane partition (decision 0684)
//
// The reference's far-band pass (`CWorldScene` drain `0x6841a0`, wow-re `terrain.md` + `rf35-render-list`)
// does NOT share a clip plane with the detailed world. It draws the coarse hulls under its own
// projection — `SetPerspective(fov, aspect, near = farclip − 33.0, far = horizonfarclip)`, byte-read at
// the two call sites `0x6842f2` (render) / `0x68317f` (the far walk's cull frustum), the `33.0` at
// `[0x8101b0]`, the horizon far from the runtime box `[0xc7b49c]` — into a **compressed depth range
// `[0.955, 0.96]`** (`[0x80febc]=0.96`, `[0x80fec4]=0.005`), i.e. squashed into the back half-percent of
// the depth buffer so it can never occlude anything the detailed world drew. Two properties fall out,
// and BOTH are load-bearing:
//
//   * the band **overlaps** the detailed world by 33 yd (≈ one WDL outer cell, spacing 33.333) — so a
//     coarse hull that crosses back inside the wall is still drawn, and no hole can open at the seam;
//   * it is **depth-pushed**, never near-clipped at the wall — so that overlap cannot poke through the
//     fine terrain, and nothing coarse near the camera can paint over the sky (its near plane stops it).
//
// We used to partition instead: terrain discarded past `farclip`, WDL discarded before it, sharing one
// plane with zero overlap. Two different surfaces cannot partition cleanly at one plane — wherever a ray
// grazed the coarse hull entirely INSIDE the wall while the fine mesh it approximates lay beyond it (a
// ridge crest, exactly), both passes discarded and the sky showed through as a band at the horizon
// (the director's "in-between area at the clip barrier", Weazel's Crater, 2026-07-26).
//
// Our depth buffer is Bevy's infinite REVERSE-Z (`perspective_infinite_reverse_rh`: depth = near/eye_z,
// 0 = infinitely far, and never exactly 0 for finite geometry), so the reference's `[0.955, 0.96]`
// window becomes a clamp rather than a `glDepthRange`: every detailed-world fragment lives at
// `depth ≥ near/farclip` (it is discarded past the wall), so clamping WDL just below that value puts the
// whole band behind the world by construction, while leaving it in front of the sky's forced `0.0`
// (`sky_order.rs`, "The depth law"). Order of the three backdrop tiers, all decided by depth and nothing
// else: sky (0.0) → WDL band → detailed world.

#import bevy_pbr::{
    mesh_functions,
    forward_io::Vertex,
    view_transformations::{position_world_to_clip, view_z_to_depth_ndc},
    mesh_view_bindings::view,
}

/// How far the far band reaches back INSIDE the detailed world's far-clip wall — the reference's
/// far-band near plane `farclip − 33.0` (`[0x8101b0]`, one WDL outer cell). This overlap is what
/// closes the coarse-vs-fine seam; the depth clamp below is what keeps it harmless.
const WDL_OVERLAP: f32 = 33.0;

/// Nudge off the wall depth so the band is strictly BEHIND a detailed-world fragment sitting exactly
/// at the wall, instead of tying it (reverse-Z `GreaterEqual` lets a tie through, and the winner would
/// then be draw order). Multiplicative so it can never cross zero into the sky.
const WDL_DEPTH_PUSH: f32 = 0.999;

struct WdlParams {
    fog_color: vec4<f32>,  // rgb = Light.dbc row 7 (gamma 0..1); w = enable (>0.5 ⇒ blend)
    fog_params: vec4<f32>, // x = fog_start yd; y = fog_end yd; z reserved; w = farclip wall (0 ⇒ off)
};
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> w: WdlParams;

struct WdlVsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
}

struct WdlFsOut {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
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
fn fragment(in: WdlVsOut) -> WdlFsOut {
    // PLANAR eye-Z (view-space depth), NOT radial — same as terrain.wgsl (apitrace-verified). Used for
    // the band's near plane (below) and the fog.
    let eye_z = -(view.view_from_world * vec4<f32>(in.world_position.xyz, 1.0)).z;
    let farclip = w.fog_params.w;

    // The far band's NEAR PLANE: `farclip − 33` (the reference's, see the header). Deliberately NOT the
    // wall — the 33 yd overlap is what stops a hole opening at the coarse-vs-fine seam. (0 ⇒ disabled.)
    if (farclip > 0.0 && eye_z < farclip - WDL_OVERLAP) {
        discard;
    }

    // White vertex diffuse (the reference's WDL colour); fog does all the colouring. Past `fog_end`
    // — and `fog_end = min(zone end, farclip)`, so that is the whole band — this is pure fog colour.
    var rgb = vec3<f32>(1.0);
    if (w.fog_color.w > 0.5) {
        let denom = max(w.fog_params.y - w.fog_params.x, 0.001);
        let factor = clamp((w.fog_params.y - eye_z) / denom, 0.0, 1.0);
        rgb = mix(w.fog_color.xyz, rgb, factor);
    }

    var out: WdlFsOut;
    // GAMMA LANE (0161): raw gamma out; the frame decodes once in the FFXGlow combine.
    out.color = vec4<f32>(rgb, 1.0);
    // The depth push (the reference's compressed far-band depth range). `view_z_to_depth_ndc` takes a
    // view-space z — negative in front of the camera — so the wall is at `-farclip`.
    var depth = in.clip_position.z;
    if (farclip > 0.0) {
        depth = min(depth, view_z_to_depth_ndc(-farclip) * WDL_DEPTH_PUSH);
    }
    out.depth = depth;
    return out;
}
