// The sky pass's shared VERTEX stage — bevy 0.18.1's `mesh.wgsl` vertex verbatim (VERTEX_*
// attributes; morph targets and skinning omitted — no sky mesh authors them) plus ONE line: the
// clip-space z is pinned to the far plane. Every sky element — the gradient dome (`sky.wgsl`),
// the cloud dome (`cloud.wgsl`), the star patches (`star.wgsl`), the celestial discs and glares
// (`celestial.wgsl`) — draws through this stage; their fragments write colour only.
//
// THE DEPTH LAW, moved up a stage (`sky_order.rs`, "The depth law"; decision 2016). The reference
// draws its whole sky first, in a squashed back depth slice, and the opaque world paints over it;
// we draw the sky after the world, so the depth TEST does that job, and it only does it if the
// sky's depth is behind everything. Until 2016 each sky fragment shader wrote
// `@builtin(frag_depth) = 0.0` (reverse-Z "infinitely far"). That is the same number this stage
// produces — clip z = 0 interpolates to NDC z = 0 for every fragment, exactly, whatever w is — but
// a fragment-stage depth write costs the whole pipeline its early-Z: the hardware cannot reject a
// fragment before the shader that decides its depth has run, so every sky fragment under a hill,
// a wall or a leaf was shaded in full and then thrown away. A dome is a full-screen draw and its
// gradient is not cheap; on an immediate-mode GPU (the Steam Deck's RDNA2, every Windows part)
// that was the whole covered fraction of the screen shaded for nothing, twice a frame at night.
// With the depth known at the vertex, the rasterizer's early test rejects those fragments
// before the shader runs. Same pixels, less work — the change is invisible by construction.
//
// A `MaterialExtension` swaps the whole stage, so the mirror below must track bevy's on upgrades.

#import bevy_pbr::{
    mesh_functions,
    forward_io::{Vertex, VertexOutput},
    view_transformations::position_world_to_clip,
}

/// Reverse-Z "infinitely far" — clip-space z = 0 ⇒ NDC depth 0.0 for every fragment, whatever w.
/// Under bevy's `GreaterEqual` test a sky fragment then passes only where the depth buffer still
/// holds its clear value (0.0): exactly the pixels no world geometry claimed.
const SKY_FAR_CLIP_Z: f32 = 0.0;

@vertex
fn vertex(vertex_no_morph: Vertex) -> VertexOutput {
    var out: VertexOutput;

    var vertex = vertex_no_morph;

    let mesh_world_from_local = mesh_functions::get_world_from_local(vertex_no_morph.instance_index);

    // Use vertex_no_morph.instance_index instead of vertex.instance_index to work around a wgpu dx12 bug.
    // See https://github.com/gfx-rs/naga/issues/2416 .
    var world_from_local = mesh_world_from_local;

#ifdef VERTEX_NORMALS
    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        vertex.normal,
        // Use vertex_no_morph.instance_index instead of vertex.instance_index to work around a wgpu dx12 bug.
        // See https://github.com/gfx-rs/naga/issues/2416
        vertex_no_morph.instance_index
    );
#endif

#ifdef VERTEX_POSITIONS
    out.world_position = mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(vertex.position, 1.0));
    out.position = position_world_to_clip(out.world_position.xyz);
    // The one line that is ours (the header): the sky's depth is the far plane, decided here so
    // the fragment stage writes none and keeps its early-Z.
    out.position.z = SKY_FAR_CLIP_Z;
#endif

#ifdef VERTEX_UVS_A
    out.uv = vertex.uv;
#endif
#ifdef VERTEX_UVS_B
    out.uv_b = vertex.uv_b;
#endif

#ifdef VERTEX_TANGENTS
    out.world_tangent = mesh_functions::mesh_tangent_local_to_world(
        world_from_local,
        vertex.tangent,
        // Use vertex_no_morph.instance_index instead of vertex.instance_index to work around a wgpu dx12 bug.
        // See https://github.com/gfx-rs/naga/issues/2416
        vertex_no_morph.instance_index
    );
#endif

#ifdef VERTEX_COLORS
    out.color = vertex.color;
#endif

#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    // Use vertex_no_morph.instance_index instead of vertex.instance_index to work around a wgpu dx12 bug.
    // See https://github.com/gfx-rs/naga/issues/2416
    out.instance_index = vertex_no_morph.instance_index;
#endif

#ifdef VISIBILITY_RANGE_DITHER
    out.visibility_range_dither = mesh_functions::get_visibility_range_dither_level(
        vertex_no_morph.instance_index, mesh_world_from_local[3]);
#endif

    return out;
}
