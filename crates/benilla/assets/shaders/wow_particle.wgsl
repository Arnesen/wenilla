// WoW particle/ribbon combine — the effect family's output stage, sibling of wow_model.wgsl's.
//
// The whole quad product is authored gamma bytes: the texture (Rgba8Unorm — deliberately NOT
// decoded on sample, the house invariant) × the raw authored track colour riding ATTRIBUTE_COLOR.
// Multiplying them in gamma space IS the vanilla math; the output is raw gamma — the buffer
// holds bytes end-to-end and the frame's one decode lives in the FFXGlow combine (GAMMA LANE,
// 0161; supersedes 0152's per-shader output decode).
//
// Why StandardMaterial couldn't do this: its unlit path assumes the sampled texture is already
// linear, so the texture term skipped the decode and showed gamma-BRIGHTENED — the bonfire's
// dark authored smoke reading pale and "a bit too thick" even after the vertex colours were
// fixed (0150). Alpha is a blend weight — never encoded, passes straight through.
//
// `main_pass_post_lighting_processing` supplies the Add-mode premultiply (rgb ×= a, a = 0 under
// the PREMULTIPLY_ALPHA def) exactly as the StandardMaterial fragment did — without it, additive
// quads would darken the scene behind them under the (One, 1−srcα) blend state.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::main_pass_post_lighting_processing,
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings::view,
}

// The shared global light/fog buffer (`lighting::global_light`) — the SAME layout every world
// shader mirrors (see wow_model.wgsl's declaration, the canonical copy). Particles read only the
// fog rows; the rest is layout ballast.
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
@group(#{MATERIAL_BIND_GROUP}) @binding(90) var<storage, read> wow_light: WowLight;
// Per-material params: x = the fog COLOUR policy (0.0 = off — water foam, its reference render
// state sets FOG off; 1.0 = scene — the ordinary day-night fog, particles/ribbons on
// opaque/alpha-key/alpha blend; 2.0/3.0/4.0 = BLACK/WHITE/GREY — the same per-blend fog table M2
// batches take, wow-re rf-weather-emission-timeline ROUND 4: an Add-blend emitter fogs toward
// black so it fades under a veil instead of gaining grey). y = FORCED-fog mode (the rain weather
// pass, byte state 0x0a/0x0b/0x0d): fog toward grey-0.5 over the fixed z..w window, ignoring the
// scene fog entirely — under the rain pass's Mod2x blend grey-0.5 is NEUTRAL (2·0.5·dst = dst), so
// this forced fog IS the streak's distance fade (wow-re rf-weather-render.md). zw = start/end.
@group(#{MATERIAL_BIND_GROUP}) @binding(91) var<uniform> wow_ext_params: vec4<f32>;

// The rain pass's forced fog colour: 0x80808080 → (0.502, 0.502, 0.502) grey (render-state 0x0d).
const RAIN_FOG_GREY: vec3<f32> = vec3<f32>(0.50196078, 0.50196078, 0.50196078);

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    // `$WOW_PARTICLE_FLAT` (B16 instrument): solid magenta, no inputs — see material.rs.
#ifdef WOW_PARTICLE_FLAT
    var flat_out: FragmentOutput;
    flat_out.color = vec4<f32>(1.0, 0.0, 1.0, 1.0);
    return flat_out;
#else
    // HARD FAR-CLIP WALL (faithful `farclip` ~777 yd) — see terrain.wgsl for the law. The reference
    // has ONE projection far plane and it clips the whole detailed world: terrain, models, liquid AND
    // the effect family, which is drawn by the same scene pass through the same matrix. benilla
    // emulates that plane per-pixel (our projection far sits at ~3000 yd so the coarse WDL horizon can
    // draw behind the wall) — and every world shader took the emulation except this one. That gap IS
    // bug B39: campfires, braziers and portal effects kept drawing at any distance, over a horizon
    // where the terrain beneath them had already been discarded by the very same wall.
    //
    // Ribbons ride this shader too (`ribbons.rs` shares `WowParticleMaterial`), so trails are walled
    // by the same discard. `fog_params.w` = farclip (0 ⇒ disabled). No pop: the scene fog end is
    // `min(fog_end, farclip)`, so a quad reaching the wall has already faded to the fog colour —
    // toward BLACK for an Add-blend emitter (`wow_ext_params.x`), i.e. to nothing under `One` blend.
    if (wow_light.fog_params.w > 0.0) {
        let clip_z = -(view.view_from_world * vec4<f32>(in.world_position.xyz, 1.0)).z;
        if (clip_z > wow_light.fog_params.w) {
            discard;
        }
    }
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    // base_color = white base × tex (gamma bytes, Unorm) × vertex colour (raw authored track
    // values) — the gamma-space product, exactly what the reference multiplies.
    let c = pbr_input.material.base_color;
    var rgb = c.rgb;
    // The scene day-night fog — byte-verified ON the particle path (wow-re
    // part-scene-multipliers.md, their 3620c287): the reference synthesizes an M2 material per
    // emitter and runs the SAME per-vertex linear fog as world geometry — same start/end, same
    // day-night colour, applied in gamma space BEFORE blend, with NO additive special-case. The
    // blend equation then does the whole night split by itself: alpha smoke fogs toward the
    // near-black night sky (the faint dark smudge) while additive flame still adds full bright.
    // Missing fog was the "night smoke glows" bug. (Planar eye-Z, like terrain/model fog.)
    if (wow_light.fog_color.w > 0.5 && wow_ext_params.x > 0.5) {
        let eye_z = -(view.view_from_world * vec4<f32>(in.world_position.xyz, 1.0)).z;
        let denom = max(wow_light.fog_params.y - wow_light.fog_params.x, 0.001);
        let factor = clamp((wow_light.fog_params.y - eye_z) / denom, 0.0, 1.0);
        // The fog COLOUR follows the policy in wow_ext_params.x (see its declaration above): scene
        // by default, BLACK/WHITE/GREY for an Add/Mod/Mod2x-blend emitter.
        var fog_rgb = wow_light.fog_color.xyz;
        if (wow_ext_params.x > 1.5 && wow_ext_params.x < 2.5) { fog_rgb = vec3<f32>(0.0); }
        else if (wow_ext_params.x > 2.5 && wow_ext_params.x < 3.5) { fog_rgb = vec3<f32>(1.0); }
        else if (wow_ext_params.x > 3.5) { fog_rgb = RAIN_FOG_GREY; }
        rgb = mix(fog_rgb, rgb, factor);
    }
    // The FORCED fog (rain weather): grey-0.5 over the material's own start/end, regardless of
    // the scene fog state — the Mod2x streak/splash distance fade.
    if (wow_ext_params.y > 0.5) {
        let eye_z = -(view.view_from_world * vec4<f32>(in.world_position.xyz, 1.0)).z;
        let denom = max(wow_ext_params.w - wow_ext_params.z, 0.001);
        let factor = clamp((wow_ext_params.w - eye_z) / denom, 0.0, 1.0);
        rgb = mix(RAIN_FOG_GREY, rgb, factor);
    }
    var out: FragmentOutput;
    // ADD-mode quads (flames, sparks — the PREMULTIPLY_ALPHA def is set by the pipeline for
    // AlphaMode::Add): fold the alpha weight into the colour in GAMMA space — the reference adds
    // `src_g·α` bytes; premultiplying after the linear conversion (Bevy's stock helper) inflates
    // every soft edge by α^(1/2.2) (decision 0160, the fat glow-disc family). Alpha handed to the
    // helper as 1.0 so its own (linear) premultiply is a no-op and the blend adds our value.
#ifdef PREMULTIPLY_ALPHA
    // GAMMA LANE (0161): gamma-premultiplied, raw out — N stacked faint quads now SUM in gamma
    // like the reference's bytes (the wagon/pyre glow-ball stacking, the daylight flame cores).
    let final_color = vec4<f32>(rgb * c.a, 1.0);
#else
    let final_color = vec4<f32>(rgb, c.a);
#endif
    out.color = main_pass_post_lighting_processing(pbr_input, final_color);
    return out;
#endif
}
