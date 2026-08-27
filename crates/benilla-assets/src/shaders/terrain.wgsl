// Phase 6 terrain splat shader — extends Bevy's StandardMaterial with a CUSTOM vertex + fragment stage.
//
// Blends up to 4 tiled layer textures by a per-chunk alpha map, then lights the result with WoW's
// faithful terrain lighting (fixed-function GL_LIGHTING, reproduced):
//   • DIFFUSE — `clamp(ambient_row1 + diffuse_row0·max(N·L,0) + Σ point·att·N·L)`, modulated into
//     the texture (MOD 1×). Evaluated AND CLAMPED per VERTEX (the GL T&L locus), then
//     Gouraud-interpolated — the clamp position matters once an over-gamut point light is in the
//     sum (see the `primary` varying note).
//   • SPECULAR (the bare-ground "sheen") — `clamp(row9 · pow(max(N·H,0),20)) · sheen_mask`, added
//     AFTER the texture modulate (separate-specular, LOCAL_VIEWER). The Blinn term is computed
//     PER-VERTEX (clamped per vertex, Gouraud-interpolated); the `sheen_mask` is the `_s` texture's
//     per-texel ALPHA, blended like the colour (so the sheen rides only shiny texels — stone, not
//     grass). See docs/knowledge/lighting.md
//     ⚠️ Two things bound it, BOTH needed (each was a separate blow-out): (a) per-vertex undersampling
//     of the sharp lobe (sampled only at the ~4 yd MCVT vertices, clamped, linearly smeared) vs a
//     per-PIXEL eval that saturates; (b) the `_s` alpha sheen-mask (ref mean ≈ 0.1) vs applying it at
//     a uniform 1.0 — row 9 ≈ near-white, so unmasked it washes the whole lit face to cream.
//
// Because Bevy's `VertexOutput` has no slot for the interpolated specular, we do our own (minimal)
// vertex transform and a custom IO struct, and drop `main_pass_post_lighting_processing` (fog returns
// IN-SHADER at Step 5, per docs/plans/lighting-rebuild.md — not Bevy's DistanceFog).
//
// One material serves a whole ADT tile (see terrain.rs): layer textures live in a `texture_2d_array`
// (binding 100), per-chunk alpha maps in another (104). Each merged-mesh vertex carries its chunk's 4
// layer indices in `color` (vertex COLOR) and its alpha-layer index in `uv_b.x` (UV1) — constant across
// the chunk, so interpolation lands back on the integer index.

#import bevy_pbr::{
    mesh_functions,
    forward_io::Vertex,
    view_transformations::position_world_to_clip,
    mesh_view_bindings::view,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var layer_array: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var alpha_array: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var splat_samp: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(110) var shadow_array: texture_2d_array<f32>;

// Per-tile Vec4 uniforms packed into ONE buffer (binding 106) — the field order here MUST match the
// Rust `TerrainExtension` struct. Light + fog live in the shared global-light storage buffer (below);
// what's left is just the per-tile layer tiling factor.
//   params — x = layer tiling factor; yzw unused.
struct TerrainParams {
    params: vec4<f32>,
};
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var<uniform> t: TerrainParams;

// The shared global light (lighting::global_light): ONE storage buffer every material reads, updated
// once/frame in place — replaces the per-material light/fog uniforms `apply_wow_lighting` re-pushed
// each frame. Rows 0-5 (of the 18-row LightStd430) are the light + fog terrain needs, plus the grade
// row at the end (rows 6-16 — model SH + water swatches — are skipped as a pad). Read in BOTH stages
// (the per-vertex sheen needs light_sun/spec in the vertex stage).
struct WowLight {
    light_ambient: vec4<f32>, // rgb = row 1 ambient; w = Mod2x scale (×1).
    light_diffuse: vec4<f32>, // rgb = row 0 sun diffuse; w = clamp-light flag (>0.5 ⇒ saturate).
    light_sun: vec4<f32>,     // xyz = world-space sun TRAVEL direction (to-light = −xyz); w unused.
    light_spec: vec4<f32>,    // rgb = row 9 specular color; w = shininess (20). rgb == 0 disables.
    fog_color: vec4<f32>,     // rgb = row 7 fog (raw, gamma 0..1); w = enable (>0.5 ⇒ blend).
    fog_params: vec4<f32>,    // x = fog_start yd; y = fog_end yd; z unused; w = farclip wall.
    _sh: array<vec4<f32>, 6>, // rows 6-11: the model SH coeffs (live in wow_model.wgsl, 0354) — unread by terrain.
    sh_c16: vec4<f32>,        // row 12: xyz the models' c16 quad band; .w a FREE lane (the 0273 point gain is retired).
    _water: array<vec4<f32>, 4>, // rows 13-16: the liquid swatches — unread by terrain.
    grade: vec4<f32>,         // reserved (the 0282 interior A/B retired; 0163). Layout only.
    _wmo_fog: array<vec4<f32>, 2>, // rows 18-19: the interior fog triple — unread by terrain.
    // The dynamic point-light table (decision 0278), packed by `global_light::build_light_data`:
    // row 20 `.x` = live entry count; then TWO rows per light — `[pos.xyz, range]`, `[rgb, 0]`.
    point_count: vec4<f32>,
    points: array<vec4<f32>, 512>,
};
@group(#{MATERIAL_BIND_GROUP}) @binding(90) var<storage, read> wow_light: WowLight;

// Custom vertex→fragment payload: the standard fields the splat fragment needs, PLUS `specular` — the
// per-vertex-clamped sun glint that must be Gouraud-interpolated (Q14). Same struct on both stages, so
// `@builtin(position)` is clip-position out / frag-coord in (`world_position` is unused until Step 5 fog).
struct TerrainVsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) uv_b: vec2<f32>,
    @location(4) color: vec4<f32>,
    @location(5) specular: vec3<f32>,
    // The full Gouraud diffuse — `clamp01(ambient + sun·max(N·L,0) + Σ point·att·N·L)`, evaluated
    // AND CLAMPED per VERTEX: the GL T&L locus. The clamp position is load-bearing for a hot
    // carried light (the raw committed torch is (1.4, 0.87, 0.40)): GL clamps the summed vertex
    // colour BEFORE interpolation, so an over-gamut torch saturates one vertex and Gouraud falls
    // linearly away from it. Interpolating the raw sum and clamping per pixel — what we did
    // before — keeps pixels pinned at white far from the hot vertex: a wide flat plateau with a
    // hard knee, the director's "hard square". Sun N·L moving per-pixel → per-vertex is the
    // faithful direction (Step 3's own note: indistinguishable on the smooth term).
    @location(6) primary: vec3<f32>,
}

// Terrain's point-light **candidacy half-width** (yd): the reference's guaranteed covered box is
// `w + 10` where `w` is the chunk's bounding-sphere radius — `sqrt(2·16.666666² + (zExtent/2)²)`,
// i.e. 23.570166 on flat ground (`0x68df70` hard-codes that same constant at `68dfac`). Relief
// widens a chunk's own `w`; we use the flat value, so on steep ground we gather very slightly
// narrower than the reference — the lights that differ are ≥33 yd out, where `att ≈ 0.02`.
const TERRAIN_REACH: f32 = 33.570166;

// The dynamic point-light term at a world-space point (decisions 0016/0273/0278, selection 0285) —
// the reference commits AT MOST THREE point lights per draw, the nearest to the receiving unit's own
// position (the gather sorts by squared distance from the unit, the commit seats GL slots 1-3 —
// wow-re `wmo-surface-dynamic-light` §4/§6). Terrain's unit is the 33.33-yd MCNK chunk, so the anchor
// is the chunk cell center under the vertex (our tiles merge chunks into one world-space mesh —
// `mcnk_cell_anchor` recovers the chunk analytically). Pass 1 selects; pass 2 evaluates only the
// selected lights at the vertex — the byte-verified falloff `1/(0.7·d + 0.03·d²)`, diffuse-only
// (committed ambient/specular are zero — VERIFIED: the staging struct's ambient/specular slots are
// explicitly zeroed at `71c7e3`-`71c7ec` right before the point loop), on the submitted vertex
// normal, no distance cutoff on the EVALUATION (a committed GL light has none; candidacy alone
// bounds the set).
//
// **Candidacy is the reference's spatial-hash sweep, not a radius** (VERIFIED, wow-re
// `terrain/scratch/terrain-dynamic-light-gather.md`): the gather walks a 20-yd-cell hash over a cell
// span `[floor((c − w − 10)/20), floor((c + w + 10)/20)]` — a **Chebyshev box**, no circular reject —
// where terrain's own `w` is the chunk's bounding-SPHERE radius `chunk+0x68`
// (`sqrt(2·16.666666² + (zExtent/2)²)` = 23.570166 on flat ground, `0x71bc30` copies it at
// `71bc47`-`71bc5f`). That gives a guaranteed covered half-width of `w + 10` = [`TERRAIN_REACH`].
// We used a 48-yd SPHERE here — an INFERRED stand-in from decision 0285, before anyone had read the
// terrain side. It over-gathered: with 29 live point lights measured around Darkshire against three
// committed slots, every extra candidate is another chance for a chunk's ≤3 set to flip as an
// emitter moves, which is chunk-shaped popping the reference never has.
//
// The M2 lane in wow_model.wgsl still uses the packed per-light range — its own `w` is 0 (a ±10 yd
// box, a *different* byte-pinned number), a change that would move the approved interior look, so it
// is the director's to weigh. The two lanes diverge on purpose; do not "fix" them back into one.
fn point_light_sum(P: vec3<f32>, N: vec3<f32>, anchor: vec3<f32>) -> vec3<f32> {
    let count = u32(wow_light.point_count.x);
    var sel = array<u32, 3>(0u, 0u, 0u);
    var sd = array<f32, 3>(1e30, 1e30, 1e30);
    for (var i = 0u; i < count; i = i + 1u) {
        let pos_range = wow_light.points[2u * i];
        let dv = pos_range.xyz - anchor;
        let d2 = dot(dv, dv);
        // The hash sweep is horizontal (the grid keys on WoW x/y = Bevy z/x) and has no vertical
        // bound; ranking below is the full 3-D distance, as `0x71bf90` does.
        if (max(abs(dv.x), abs(dv.z)) > TERRAIN_REACH) {
            continue;
        }
        if (d2 < sd[0]) {
            sd[2] = sd[1]; sel[2] = sel[1];
            sd[1] = sd[0]; sel[1] = sel[0];
            sd[0] = d2; sel[0] = i;
        } else if (d2 < sd[1]) {
            sd[2] = sd[1]; sel[2] = sel[1];
            sd[1] = d2; sel[1] = i;
        } else if (d2 < sd[2]) {
            sd[2] = d2; sel[2] = i;
        }
    }
    var sum = vec3<f32>(0.0);
    for (var s = 0u; s < 3u; s = s + 1u) {
        if (sd[s] > 9.9e29) {
            break;
        }
        let pos_range = wow_light.points[2u * sel[s]];
        let to_light = pos_range.xyz - P;
        let d = length(to_light);
        let atten = 1.0 / (0.7 * d + 0.03 * d * d);
        let nl = max(dot(N, to_light / max(d, 1e-4)), 0.0);
        sum += wow_light.points[2u * sel[s] + 1u].rgb * (atten * nl);
    }
    return sum;
}

// The MCNK chunk cell center under a world point — terrain's light-selection anchor (the terrain
// draw unit; the grid is fixed world-wide, chunk = 533.3333/16 yd, half-extent 32 tiles). WoW x/y
// are Bevy −z/−x and the grid is symmetric, so snapping Bevy x/z directly lands on the same cells.
//
// **The XY snap is VERIFIED** (wow-re `terrain-dynamic-light-gather.md`: `CMapChunk+0x5c..0x64` is
// the chunk's world AABB centre, written by the MCVT world-vert builder `0x6b0e50` at
// `6b106c`/`6b10ec`, and the 33.33-yd chunk really is the draw unit — gather → commit → draw pair
// 1:1 per record at `68478a`/`68479c`/`6847bb`/`6847c4`).
//
// **The height is KNOWN-WRONG and deliberately not fixed here.** The reference's anchor Z is the
// chunk's own mid-height `(minH + maxH)/2` over its 145 verts — one constant per chunk. We keep the
// vertex's own y, which means the anchor VARIES within a chunk, so on relief two vertices of the
// same chunk can select different light sets and seam mid-chunk — the reference cannot do that.
// Fixing it needs a per-chunk constant plumbed to the vertex stage (a custom mesh attribute + a
// `specialize` on the terrain material, which has none today), so it is scoped as its own change;
// on flat ground the two anchors coincide and nothing differs. Mirrored in wow_model.wgsl (clutter
// snaps to the same cells).
fn mcnk_cell_anchor(P: vec3<f32>) -> vec3<f32> {
    let cell = 533.33333 / 16.0;
    let half = 32.0 * 533.33333;
    let ix = floor((half + P.x) / cell);
    let iz = floor((half + P.z) / cell);
    return vec3<f32>((ix + 0.5) * cell - half, P.y, (iz + 0.5) * cell - half);
}

@vertex
fn vertex(in: Vertex) -> TerrainVsOut {
    var out: TerrainVsOut;
    let world_from_local = mesh_functions::get_world_from_local(in.instance_index);
    out.world_position =
        mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(in.position, 1.0));
    out.clip_position = position_world_to_clip(out.world_position.xyz);
    let world_normal = mesh_functions::mesh_normal_local_to_world(in.normal, in.instance_index);
    out.uv = in.uv;
    out.uv_b = in.uv_b;
    out.color = in.color;

    // STEP 3b: terrain SUN SPECULAR, computed PER-VERTEX (Q14). Material spec = white, shininess =
    // wow_light.light_spec.w (20), spec color = row 9 (wow_light.light_spec.rgb); H = halfway(to-light, to-eye) with
    // the LOCAL viewer (per-vertex eye dir). Clamp HERE, per vertex — rasterizer Gouraud-interpolates.
    let n = normalize(world_normal);
    let l = -normalize(wow_light.light_sun.xyz); // to-light (the sun travels along +light_sun)
    let v = normalize(view.world_position.xyz - out.world_position.xyz); // to-eye (local viewer)
    let h = normalize(l + v);
    let ndoth = max(dot(n, h), 0.0);
    out.specular = clamp(wow_light.light_spec.rgb * pow(ndoth, wow_light.light_spec.w), vec3<f32>(0.0), vec3<f32>(1.0));

    // STEP 3 at the faithful locus — the WHOLE diffuse sum, clamped HERE per vertex (see the
    // struct note). Sun: `ambient + diffuse·max(N·L,0)` on the MCNR normal; points: the ≤3-nearest
    // committed lights of this vertex's MCNK chunk cell — the terrain draw unit (0285) — at the
    // byte-verified `1/(0.7d + 0.03d²)`, raw over-gamut colours in.
    let ndotl = max(dot(n, l), 0.0);
    let points = point_light_sum(out.world_position.xyz, n, mcnk_cell_anchor(out.world_position.xyz));
    out.primary = clamp(
        wow_light.light_ambient.rgb + wow_light.light_diffuse.rgb * ndotl + points,
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
    return out;
}

@fragment
fn fragment(in: TerrainVsOut) -> @location(0) vec4<f32> {
    // HARD FAR-CLIP WALL (faithful `farclip`): the reference clips the detailed world at its projection
    // far plane (~777 yd). Geometry beyond is GPU-clipped per-pixel, so a tall object reveals
    // closest-part-first as you approach and the sky/WDL shows behind it. We reproduce that with a
    // per-fragment discard on PLANAR eye-Z (same coordinate as the fog) beyond `fog_params.w` = farclip
    // (0 ⇒ disabled). WDL/sky use other shaders, so they still render to the horizon behind the wall.
    if (wow_light.fog_params.w > 0.0) {
        let clip_z = -(view.view_from_world * vec4<f32>(in.world_position.xyz, 1.0)).z;
        if (clip_z > wow_light.fog_params.w) {
            discard;
        }
    }

    // Per-chunk array indices, baked into the merged mesh: COLOR = 4 layer indices, UV1.x = alpha.
    let li = vec4<i32>(round(in.color));
    let ai = i32(round(in.uv_b.x));

    let tiled = in.uv * t.params.x;
    // Full RGBA: `.rgb` = diffuse, `.a` = the `_s` texture's per-texel SHEEN MASK (see assets.rs /
    // terrain.rs — the base `.blp` has no alpha, so we load the `_s` variant; matte where no mask).
    // `view.mip_bias` (decision 1639): a render scale below 1 doubles every derivative and so
    // picks a coarser mip, and that blur is then stretched back up by the resolve. The bias undoes
    // exactly that shift and is 0.0 at native and above, so this is an identity at scale 1.
    // Only the LAYER array takes it: `alpha_array`/`shadow_array` are one 64x64 grid per chunk,
    // built with a single mip level (terrain.rs `array_image`), so no bias could move them.
    let s0 = textureSampleBias(layer_array, splat_samp, tiled, li.x, view.mip_bias);
    let s1 = textureSampleBias(layer_array, splat_samp, tiled, li.y, view.mip_bias);
    let s2 = textureSampleBias(layer_array, splat_samp, tiled, li.z, view.mip_bias);
    let s3 = textureSampleBias(layer_array, splat_samp, tiled, li.w, view.mip_bias);
    // Alpha + shadow maps are one 64² grid per chunk in 0..1 (NOT tiled), yet they share the layer
    // textures' Repeat sampler. The old "repeat == clamp here" assumption is FALSE under LINEAR
    // filtering: at a chunk edge (uv→0/1) the bilinear footprint WRAPS to the chunk map's opposite
    // edge, blending unrelated weights → a thin seam at every chunk border (the creases; introduced
    // when 228d336 switched Nearest→Linear). Inset by half a texel so the footprint clamps instead.
    let auv = clamp(in.uv, vec2<f32>(0.5 / 64.0), vec2<f32>(1.0 - 0.5 / 64.0));
    let a = textureSample(alpha_array, splat_samp, auv, ai);

    var color = s0.rgb;
    color = mix(color, s1.rgb, a.r);
    color = mix(color, s2.rgb, a.g);
    color = mix(color, s3.rgb, a.b);

    // Sheen mask: blend the layers' `_s` alpha with the SAME splat weights as the colour, so the sun
    // specular rides only the shiny texels (stone/pebbles), not matte grass/dirt. This per-pixel mask
    // is what bounds the highlight — the real client's mask averages ≈ 0.1 (Q14); applying the sheen at
    // a uniform 1.0 is what blew out the floor. (FP: `secondary · texture[0].w`.)
    var specmask = s0.a;
    specmask = mix(specmask, s1.a, a.r);
    specmask = mix(specmask, s2.a, a.g);
    specmask = mix(specmask, s3.a, a.b);

    // STEP 3 (diffuse) arrives from the vertex stage, ALREADY summed (ambient + sun N·L + the
    // chunk's ≤3 committed point lights) and ALREADY clamped — Gouraud of the clamped vertex
    // colour, byte-faithful to GL T&L (see the `primary` struct note; the MCNR normal and the
    // DayNight sun share a space per the Phase-0 validation). The MCSH ×(0.3·s + 0.7) factor below
    // still scales the whole modulate, as the traced combine does.
    let primary = in.primary;

    // STEP 4: MCSH baked shadow. On the reference path (pixelShaders+specular) terrain is ONE pass
    // through `terrainp_s.bls`, which carries the MCSH bit in the blend texture's alpha (1.0 lit / 0.0
    // shadowed) and uses it TWO ways (Q15) — NOT the Q11 separate ambient-tint overlay (that's the
    // legacy/non-pixelShaders path). Our `shadow_array` R8 is 255=shadowed/0=lit, so `shadow_lit =
    // 1 - mcsh`. Chunks with no MCSH map (`uv_b.y < 0`) are fully lit.
    var shadow_lit = 1.0;
    let si = in.uv_b.y;
    if (si >= 0.0) {
        let mcsh = textureSample(shadow_array, splat_samp, auv, i32(round(si))).r;
        shadow_lit = 1.0 - mcsh;
    }

    // The faithful `terrainp_s` combine (Q15):
    //   diffuse  = tex · primary · (0.3·shadow + 0.7)   → a flat −30% in shadow, NO colour tint
    //   specular = per-vertex sheen · gloss_mask · shadow → gated to ZERO in shadow (no sheen in shade)
    // (`tex·primary` is the MODULATE-1× diffuse; the sheen is added after, separate-specular.) Then
    // LDR-clamp; gamma/byte throughout; raw gamma out (GAMMA LANE, 0161).
    let diffuse_term = color * primary * (0.3 * shadow_lit + 0.7);
    let spec_term = in.specular * specmask * shadow_lit;
    var tuned = clamp(diffuse_term + spec_term, vec3<f32>(0.0), vec3<f32>(1.0));

    // STEP 5: gamma-space linear fog (q6). Applied AFTER tone/curve (so the diagnostic knobs see
    // the unfogged lit pixel) and BEFORE the viz bypass + raw output. The fog coordinate is
    // PLANAR eye-Z (view-space depth along the camera forward axis), NOT radial distance: the
    // reference VS computes `fogcoord` as a linear function of a single axial coordinate (eye-Z) —
    // VERIFIED from the host GLSL fog source in two apitraces (WoW.5/WoW.8). Radial `length()` is
    // ≥ planar everywhere off-axis, so it OVER-fogs the screen edges (worst in short-fog zones like
    // Duskwood, +0.26–0.32 fog factor at the corners). The per-vertex T&L original and this
    // per-pixel form land on the same byte under LINEAR fog (the factor interpolates exactly), and
    // the per-pixel form is what survives Bevy's mesh interpolators without a custom slot.
    if (wow_light.fog_color.w > 0.5) {
        let eye_z = -(view.view_from_world * vec4<f32>(in.world_position.xyz, 1.0)).z;
        let denom = max(wow_light.fog_params.y - wow_light.fog_params.x, 0.001);
        let factor = clamp((wow_light.fog_params.y - eye_z) / denom, 0.0, 1.0);
        tuned = mix(wow_light.fog_color.xyz, tuned, factor);
    }

    // GAMMA LANE (decision 0161): the framebuffer HOLDS gamma bytes — all blending happens in
    // gamma exactly like the reference's byte framebuffer; the frame's ONE decode lives in the
    // FFXGlow combine (the last node). Output raw.
    return vec4<f32>(tuned, 1.0);
}
