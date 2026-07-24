// The UI gamma lane's ONE decode (decision 0254) — the twin of `ffx_glow.wgsl`'s combine, which owns
// the world lane's single decode (0161).
//
// `ui_quad.wgsl` composites the whole UI in gamma bytes, like the reference's fixed-function
// backbuffer. The UI camera's target is `Rgba8UnormSrgb`, so a stored byte round-trips through the
// hardware's encode/decode and the sampler hands this pass back the gamma value the UI pass wrote.
// Emitting `srgb_to_linear` of it re-encodes on write to the exact client byte, which the output
// blit then carries into the swapchain unchanged.
//
// RGB is premultiplied by coverage; alpha is coverage and carries no gamma, so it passes through.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var screen_sampler: sampler;

// sRGB → linear (IEC 61966-2-1), the exact inverse of `ui_quad.wgsl`'s `linear_to_srgb`.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let higher = pow((max(c, vec3<f32>(0.0)) + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    let lower = c / 12.92;
    return select(higher, lower, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_decode(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let ui = textureSample(screen_texture, screen_sampler, in.uv);
    return vec4<f32>(srgb_to_linear(ui.rgb), ui.a);
}
