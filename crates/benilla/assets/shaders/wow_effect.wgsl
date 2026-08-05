// WoW effect-lane shader — the shared effect stream's own pipeline (decisions 0732 P1/P2,
// 0733: particles, ribbons, the decal family, water foam, precipitation). The fragment IS
// wow_particle.wgsl's combine, freed of the StandardMaterial shell: the whole product is
// authored gamma bytes — texture (Rgba8Unorm, deliberately NOT decoded on sample, the house
// invariant) × the raw authored track colour riding the vertex colour. Multiplying them in
// gamma space IS the vanilla math; the output is raw gamma — the buffer holds bytes end-to-end
// and the frame's one decode lives in the FFXGlow combine (GAMMA LANE, 0161).
//
// Blend variants (pipeline shader defs, one per EffectBlend — 0733 §4):
// - BLEND_ADD:      the premultiplied-alpha fold in GAMMA space (rgb ×= α, α = 0 under the
//   (One, 1−srcα) blend state = pure addition) — the reference adds `src_g·α` bytes;
//   premultiplying after a linear conversion inflates every soft edge by α^(1/2.2)
//   (decision 0160, the fat glow-disc family).
// - BLEND_ALPHA:    straight (rgb, α) under standard alpha blending.
// - BLEND_OPAQUE:   (rgb, 1) with blending off — the alpha-ignoring shape the material path's
//   AlphaMode::Opaque produced.
// - BLEND_MULTIPLY: premultiplied (rgb·α, α) under (Dst, 1−srcα) — `dst·lerp(1, rgb, α)`, the
//   blob shadow's modulate-with-fade (and ModelBlend::Mod at α = 1).
// - BLEND_MOD2X:    (rgb, 1) under (Dst, Src) = `2·src·dst` — rain's state; reads no alpha.

#import bevy_render::view::View

// The shared global light/fog buffer (`lighting::global_light`) — the SAME layout every world
// shader mirrors (see wow_model.wgsl's declaration, the canonical copy). This lane reads only
// the fog rows; the rest is layout ballast.
struct WowLight {
    light_ambient: vec4<f32>,
    light_diffuse: vec4<f32>,
    light_sun: vec4<f32>,
    light_spec: vec4<f32>,
    fog_color: vec4<f32>,  // rgb row-7 fog (gamma); w = enable (>0.5)
    fog_params: vec4<f32>, // x=start y=end z=linear-lighting A/B flag w=farclip wall
    sh_c10_r: vec4<f32>,
    sh_c10_g: vec4<f32>,
    sh_c10_b: vec4<f32>,
    sh_c13_r: vec4<f32>,
    sh_c13_g: vec4<f32>,
    sh_c13_b: vec4<f32>,
    sh_c16: vec4<f32>,
    _water: array<vec4<f32>, 4>,
    grade: vec4<f32>,
};

@group(0) @binding(0) var<uniform> view: View;
@group(1) @binding(0) var effect_texture: texture_2d<f32>;
@group(1) @binding(1) var effect_sampler: sampler;
@group(1) @binding(2) var<storage, read> wow_light: WowLight;
// Per-draw params (a dynamic offset into the canonical trio): x = the fog COLOUR policy
// (0 = off — the "unfogged" file flag 0x8; 1 = scene — the ordinary day-night fog; 2 = BLACK —
// an Add-blend emitter fades under a veil instead of gaining grey, the same per-blend fog
// table M2 batches take, `0x70baf0`). y = forced-fog mode with zw = its start/end (the rain
// lanes — unused until precipitation joins the lane in slice P2, kept so the struct is P2's
// already).
@group(1) @binding(3) var<uniform> wow_ext_params: vec4<f32>;

// The rain pass's forced fog colour: 0x80808080 → grey (render-state 0x0d).
const RAIN_FOG_GREY: vec3<f32> = vec3<f32>(0.50196078, 0.50196078, 0.50196078);

struct Vertex {
    // CAMERA-RELATIVE (0733 §2): prepare rebased the stream against this view's own
    // world_position, so the big-coordinate cancellation never reaches the matrix product —
    // absolute-coordinate verts through `clip_from_world` shear thin geometry apart far from
    // the origin (the precip module's empirically-learned law at ~9000 yd).
    // EXCEPT under DECAL_WORLD_CLIP (decision 0781): the decal family's verts arrive ABSOLUTE
    // (prepare skips their rebase) and take the mesh path's own matrix in the vertex stage.
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,    // raw authored gamma RGBA; α is the blend weight
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    // View-space depth (+ in front of the camera) — the fog/farclip planar eye-Z (exact on
    // the cam-relative path, where the view transform is pure rotation).
    @location(2) view_z: f32,
    // World-space offset from the eye — what `EFFECT_LIT` builds its camera-facing normal from.
    // Already exactly `v.position` on the free-floating path (0733 §2's rebase); the decal path
    // carries absolute verts, so it subtracts the eye back out.
    @location(3) world_offset: vec3<f32>,
};

@vertex
fn vertex(v: Vertex) -> VertexOutput {
    var out: VertexOutput;
#ifdef DECAL_WORLD_CLIP
    // The decal family (raster_bias ≠ 0): ABSOLUTE world verts through `clip_from_world` —
    // the SAME matrix every world-mesh shader runs (`position_world_to_clip` in terrain.wgsl /
    // wow_model.wgsl). A decal's depth must tie against the drawn ground within the small
    // rasterizer bias; the cam-relative route below reaches the same plane through different
    // arithmetic, and at WoW-scale coordinates the two disagree by more than the bias — the
    // flaky/wedge-shaped blob shadow (decision 0781). Same matrix, same jitter: the decal
    // wobbles WITH its receiver instead of against it.
    out.clip_position = view.clip_from_world * vec4<f32>(v.position, 1.0);
    // Fog/farclip eye-Z via the full affine transform (yards-scale — rounding is irrelevant).
    out.view_z = -(view.view_from_world * vec4<f32>(v.position, 1.0)).z;
    out.world_offset = v.position - view.world_position;
#else
    // Free-floating families: cam-relative verts (0733 §2). view_from_world = [R | −R·cam];
    // for a cam-relative point the translation column is exactly the subtraction prepare
    // already did, so only the rotation remains.
    let view_pos = mat3x3<f32>(
        view.view_from_world[0].xyz,
        view.view_from_world[1].xyz,
        view.view_from_world[2].xyz,
    ) * v.position;
    out.clip_position = view.clip_from_view * vec4<f32>(view_pos, 1.0);
    out.view_z = -view_pos.z;
    out.world_offset = v.position;
#endif
    out.uv = v.uv;
    out.color = v.color;
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    // `$WOW_PARTICLE_FLAT` (B16 instrument): solid magenta, no inputs — see render.rs.
#ifdef WOW_PARTICLE_FLAT
    return vec4<f32>(1.0, 0.0, 1.0, 1.0);
#else
    // HARD FAR-CLIP WALL (faithful `farclip` ~777 yd) — see terrain.wgsl for the law. The
    // reference has ONE projection far plane clipping the whole detailed world including the
    // effect family; ours sits at ~3000 yd so every world shader emulates the wall per-pixel.
    // Missing it here was bug B39. No pop: the scene fog end is `min(fog_end, farclip)`, so a
    // quad reaching the wall has already faded to the fog colour — toward BLACK for an
    // Add-blend emitter, i.e. to nothing under `One` blend.
    if (wow_light.fog_params.w > 0.0 && in.view_z > wow_light.fog_params.w) {
        discard;
    }
    // The gamma-space product — texture bytes × raw authored vertex colour, exactly what the
    // reference multiplies.
    let c = textureSample(effect_texture, effect_sampler, in.uv) * in.color;
    var rgb = c.rgb;
#ifdef EFFECT_LIT
    // SCENE LIGHTING (EGxRs id 0x0e), for the emitters that ask for it. A particle emitter owns
    // no material: the reference synthesizes an `M2Material` from the emitter's file record every
    // draw (`0x70d8b0`) and runs the SAME batch state producer as an ordinary mesh submesh, so
    // `GL_LIGHTING` lands on a particle quad exactly as on geometry — enabled iff the emitter
    // CLEARS file flag 0x1 and its blend mode is not Mod/Mod2x (wow-re `part-scene-multipliers.md`
    // §1, byte-verified at `0x70baf0` @`0x70bb00`; the gate itself is `EffectDrawSpec::lit`).
    //
    // File bit 0x1 is the UNLIT flag — the inverse of the wowdev-wiki lore — so the fire and spell
    // corpus (which sets it) stays bright at night, while the environment sheets that clear it
    // take the world's light: waterfall spray, chimney smoke, blown dust, snow. Rendering those
    // unlit is a full-white cutout against shaded terrain (the Zul'Gurub waterfall foam).
    //
    // The term is the fixed-function matte `wow_model.wgsl` applies to a mesh —
    // `clamp(ambient + diffuse·max(N·L, 0))` — against the CAMERA-FACING normal the reference's
    // quad writer uploads for these emitters. On the cam-relative path a vertex position IS its
    // offset from the eye in world axes, so the direction back to the camera is its negation;
    // the degenerate at-the-eye case falls back to the sun-facing normal (full light), which is
    // the limit the billboard approaches anyway.
    let to_eye = -in.world_offset;
    let len2 = dot(to_eye, to_eye);
    let L = -normalize(wow_light.light_sun.xyz);
    let N = select(L, to_eye * inverseSqrt(len2), len2 > 1e-12);
    let lit = clamp(
        wow_light.light_ambient.rgb + wow_light.light_diffuse.rgb * max(dot(N, L), 0.0),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
    rgb = rgb * lit;
#endif
    // The scene day-night fog — byte-verified ON the particle path (wow-re
    // part-scene-multipliers.md): the reference runs the SAME per-vertex linear fog as world
    // geometry — same start/end, same day-night colour, applied in gamma space BEFORE blend,
    // with NO additive special-case. The blend equation then does the whole night split by
    // itself. Missing fog was the "night smoke glows" bug. (Planar eye-Z, like terrain fog.)
    if (wow_light.fog_color.w > 0.5 && wow_ext_params.x > 0.5) {
        let denom = max(wow_light.fog_params.y - wow_light.fog_params.x, 0.001);
        let factor = clamp((wow_light.fog_params.y - in.view_z) / denom, 0.0, 1.0);
        // The fog COLOUR follows the policy in wow_ext_params.x (see its declaration).
        var fog_rgb = wow_light.fog_color.xyz;
        if (wow_ext_params.x > 1.5 && wow_ext_params.x < 2.5) { fog_rgb = vec3<f32>(0.0); }
        else if (wow_ext_params.x > 2.5 && wow_ext_params.x < 3.5) { fog_rgb = vec3<f32>(1.0); }
        else if (wow_ext_params.x > 3.5) { fog_rgb = RAIN_FOG_GREY; }
        rgb = mix(fog_rgb, rgb, factor);
    }
    // The FORCED fog (rain weather): grey-0.5 over the params' own start/end, regardless of
    // the scene fog state — under Mod2x the grey is neutral, so this IS the streak/patter
    // distance fade (rf-weather-render Q3).
    if (wow_ext_params.y > 0.5) {
        let denom = max(wow_ext_params.w - wow_ext_params.z, 0.001);
        let factor = clamp((wow_ext_params.w - in.view_z) / denom, 0.0, 1.0);
        rgb = mix(RAIN_FOG_GREY, rgb, factor);
    }
#ifdef BLEND_ADD
    // GAMMA LANE (0161): gamma-premultiplied, raw out — N stacked faint quads SUM in gamma
    // like the reference's bytes. α = 0 turns (One, 1−srcα) into pure addition.
    return vec4<f32>(rgb * c.a, 0.0);
#else
#ifdef BLEND_OPAQUE
    return vec4<f32>(rgb, 1.0);
#else
#ifdef BLEND_MULTIPLY
    // The modulate family: premultiplied for bevy's Multiply state — `dst·lerp(1, rgb, α)`
    // (the blob shadow's fade-riding darken; `ModelBlend::Mod` at α = 1).
    return vec4<f32>(rgb * c.a, c.a);
#else
#ifdef BLEND_MOD2X
    // `(Dst, Src)` reads no alpha (0528's no-alpha law).
    return vec4<f32>(rgb, 1.0);
#else
    return vec4<f32>(rgb, c.a);
#endif
#endif
#endif
#endif
#endif
}
