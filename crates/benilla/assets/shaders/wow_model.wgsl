// WoW model lighting — paired with terrain.wgsl. Step 7 (matte) restores the faithful gamma-space
// combine for M2/WMO/creature meshes; trees, doodads, and buildings stop being black. **Step 8d:**
// ground clutter (grass/flowers) is lit by the **terrain ground normal under each tuft** (baked onto
// its vertices in clutter.rs), not the grass-quad's own normal — VERIFIED faithful: WoW writes the
// terrain quadrant-plane normal onto the clutter vertex normal channel and lights per-vertex with it
// (ground-effects.md), so a tuft darkens with the dirt it stands on as slope / shade / sun change.
// MCSH grey from q12 still rides on `ATTRIBUTE_COLOR → pbr_input.material.base_color` so per-doodad
// shadowing remains.
//
//   color = clamp(A + D·I·f(N·u)) × tex × tint          // M2 doodads: the Model2.bls order-2 lobe (0803)
//   color = clamp(ambient + diffuse·max(N·L,0)) × …      // clutter / WMO: FFP directional matte (sun-scale 1)
//   color = mix(fog_color, color, fog_factor)                                // Step 5 fog
//   out   = color                            // raw gamma — the frame's ONE decode is in FFXGlow (0161)
//
// For trees / WMOs / creatures, `material_tint` is the StandardMaterial base_color (white by default
// — vertex colour attribute absent → VERTEX_COLORS shader-def not set → no per-vertex factor). For
// detail clutter, the merged mesh ships `ATTRIBUTE_COLOR = (mcsh_tint, mcsh_tint, mcsh_tint, 1)`,
// which Bevy folds into base_color → the lit factor is multiplied by the MCSH grey per-doodad.
//
// **Two different lighting laws live in this file, and confusing them has cost us three decisions.**
// Terrain, clutter and WMO genuinely ARE fixed-function (GL_LIGHTING + GL_LIGHT0 + GL_COLOR_MATERIAL,
// byte-verified off WoW.exe 5875 — and all five FFP light-commit call sites are terrain's), so they take
// `clamp(ambient + diffuse·max(N·L,0))`. The exterior M2 lane is NOT fixed-function: the reference loads
// `Shaders\Vertex\Model2.bls` out of misc.MPQ (gated on the VERTEX cvar `M2UseShaders`, which defaults to
// "1") and that program is an order-2 irradiance lobe, running on every exterior M2 it draws — doodad,
// GameObject, creature and player alike. So M2 doodads take that lobe, `E = A + D·I·(4/17)(0.375 + 2μ +
// 1.875μ²)` with μ = N·u toward-light, clamp01 on the SUM (0803, and the lane comment at step 7).
// Per-instance `I` is the terrain-shade family sampled at the doodad's base (2.5 lit / 0.5 MCSH-shadowed,
// `[def+0xa4]`) — see step 7 for the one part of that still open.
//
// Two retracted claims are recorded rather than deleted, because each stood long enough to seed a
// decision record and the next reader should know they were retired, not that they were never made:
// (a) "there is NO M2 irradiance lobe … no such program runs — M2UsePixelShaders defaults off" — wrong
// twice, wrong cvar and the program does run; (b) "on the exterior M2 lane that matte is OUR choice"
// (0796's framing) — true while 0410's cutoff stood, retired by 0803, which put the lane back on the
// reference's own curve.
//
// Specular (row 9 separate-specular, local viewer) is verified by q4/q5/Q13 but kept OUT of this
// step — M2 per-material shininess (q4 §6 INFERRED) is its own A/B and lives in Step 7b. WMO
// per-group authored colour (q4 §5) is also deferred.
//
// The clutter distance-fade alpha ramp (~52.5→70 yd) still applies on top — that's a draw-distance
// concern, not a lighting one (ground-effects.md Q4/Q10).

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::alpha_discard,
    forward_io::{VertexOutput, FragmentOutput},
    mesh_view_bindings::view,
    mesh_functions,
}

// Per-material model uniforms packed at binding 100 (see `WowModelExt` in terrain.rs). Light + fog + the
// SH coeffs moved OUT to the shared global-light storage buffer (below); only the per-material draw flags
// remain. Field order MUST match the Rust struct.
//   clutter_fade — x = full-opacity radius (yd); y = fully-gone radius (yd); w = enabled (>0.5).
//                  The client draws clutter only within ~70 yd with a ramp over the last quarter;
//                  we reproduce by multiplying cutout alpha by clamp((y−d)/(y−x)) so distant grass
//                  erodes away through the alpha test (ground-effects.md Q4/Q10). 0 = off.
//   model_flags  — x = is_wmo (>0.5 ⇒ the WMO surface lanes); y = fade-blend twin;
//                  z = interior (>0.5): a WMO interior group (with is_wmo ⇒ the INT/TRANS batch-class
//                      lanes below) OR an interior M2 doodad prop (without is_wmo ⇒ lit by its folded
//                      SH probe, slot per-instance in MeshTag — day/night-independent);
//                  w = unlit fullbright (>0.5 ⇒ bypass lighting): M2 UNLIT (0x01), or WMO UNLIT on an
//                      exterior-group batch (the interior drawer ignores the flag — section law).
struct ModelParams {
    clutter_fade: vec4<f32>,
    model_flags: vec4<f32>,
    // x = per-material MCSH terrain-shade SELECTOR (≥0.5 ⇒ lit ground, <0.5 ⇒ MCSH-shadowed); the shader
    // thresholds it into the lit/shaded doodad sun INTENSITY family below. yzw reserved.
    sun_scale: vec4<f32>,
    // xyz = the M2Color RGB tint for batches whose colour track ANIMATES (the static vertex bake is
    // skipped for those — WowModelExt::tint): folded into the albedo exactly where the vertex tint
    // folds. (1,1,1) — identity — for everything else. w = the WMO interior BATCH-CLASS lane
    // (wow-re trace-forensics-abbey-interior-d3d §2): 0 = exterior law, 1 = interior INT (unlit
    // tex × MOCV), 2 = interior TRANS (per-vertex MOCV-alpha lit↔bake lerp).
    tint: vec4<f32>,
    // The WMO window/glass law (wow-re wmo-lit-selector / wmo-interior-night-light; 0 for all M2):
    // xyz = the MOMT SIDN (0x10) authored emissive colour (gamma bytes /255) — multiplied by the live
    // night fraction (wow_light.grade.x) and added INSIDE the lit sum on lit lanes, like the
    // reference's glMaterialfv(GL_EMISSION): tex × (lit + sidn·night). Windows glow warm at night,
    // nothing by day; dead on the unlit INT lane and under UNLIT, exactly like the FFP.
    // w = the MOMT WINDOW (0x20) flag (>0.5): an interior-group batch swaps GL_LIGHT0 to the brighter
    // midpoint pair — ambient AND diffuse = (Direct + Ambient)/2, ambient +16/255 — the warm pane
    // seen from inside a building (derivation 0x6d37e0, byte-verified).
    sidn: vec4<f32>,
};
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> m: ModelParams;

// The shared global light (lighting::global_light): ONE storage buffer every material reads, updated
// once/frame in place — replaces the per-material light/fog uniforms the old apply_wow_lighting re-pushed
// each frame. The model reads rows 0-2 (ambient/diffuse/sun) + fog, plus rows 6-12: the disassembled
// `Model2.bls` probe of the day/night scene light at intensity 1 — the reference's own exterior M2
// response, and since 0803 the live law on the doodad/entity lane.
// Shared with terrain.wgsl — a tree and the dirt under it fade into the same haze.
// (light_spec is part of the prefix layout but the model path never reads it.)
struct WowLight {
    light_ambient: vec4<f32>, // rgb ambient; w = Mod2x scale
    light_diffuse: vec4<f32>, // rgb sun diffuse; w = clamp-light flag (>0.5 ⇒ saturate)
    light_sun: vec4<f32>,     // xyz sun TRAVEL dir (to-light = −xyz); w = directional-light enable (>0.5)
    light_spec: vec4<f32>,    // rgb spec color; w = shininess (terrain's — unused by the matte model path)
    fog_color: vec4<f32>,     // rgb row-7 fog (gamma); w = enable (>0.5)
    fog_params: vec4<f32>,    // x=start y=end z=linear-lighting A/B flag w=farclip wall
    // The global Model2.bls SH rows (6-12): the scene day/night light as an order-2 probe at
    // intensity 1 — DC (ambient, `.w` of c10) + the sun's linear/quad bands in c10.xyz / c13 /
    // c16.xyz, the disassembled closed form (wow-re model2-bls-vertex-sh.md). Every sun band is
    // linear in the committed colour, so a consumer scales them ALL by the per-instance intensity
    // (never I²); the sun's DC redistribution rides `grade.yzw` (× intensity).
    //
    // **LIVE — the exterior doodad/entity response (0803).** History worth knowing before you touch
    // it: 0410 took this lane OFF the curve onto a hard-cutoff FFP matte on the director's look call,
    // and the block then sat packed-but-unread for months. 0796 refuted the fidelity premise behind
    // that retirement (the reference's M2 lane IS this SH shader, not an FFP light slot — the FFP
    // commits in a world frame are terrain's), which reopened the call as a pure look question;
    // 0799 restored the eval behind an A/B so it could be seen; 0803 is the director calling it,
    // and the cutoff branch came out with the flag.
    sh_c10_r: vec4<f32>,
    sh_c10_g: vec4<f32>,
    sh_c10_b: vec4<f32>,
    sh_c13_r: vec4<f32>,
    sh_c13_g: vec4<f32>,
    sh_c13_b: vec4<f32>,
    // xyz = the true c16 quad band (x²−y², per channel), live with the block above. w = FREE — it
    // carried the 0273 point gain, then the 0750/0751 sun dial, then 0799's response A/B, and every
    // one of those is retired. Nothing writes it and nothing reads it; claim it if you need a lane.
    sh_c16: vec4<f32>,
    _water: array<vec4<f32>, 4>, // rows 13-16: the liquid swatches — unread by models.
    // x = SIDN night fraction (1 overnight, 0 by day — scales m.sidn.rgb).
    // yzw = the sun's SH DC redistribution per channel, at intensity 1 (× the per-instance I).
    grade: vec4<f32>,
    // Rows 18-19: the INTERIOR fog triple — the 4 s camera-in-WMO MFOG crossfade (== the scene fog
    // outdoors). Consumed by the interior lanes only (round-6 Q-I): interior WMO-group surfaces
    // and that group's doodads (`0x6b5190` / `0x6b62e0`) — selected below by `m.model_flags.z`.
    wmo_fog_color: vec4<f32>,    // rgb interior fog (gamma); w = enable (mirrors fog_color.w)
    wmo_fog_params: vec4<f32>,   // x = start yd; y = end yd; zw = free lanes (retired A/B dials)
    // The dynamic point-light table (decision 0278), packed by `global_light::build_light_data`:
    // row 20 `.x` = live entry count; then TWO rows per light — `[pos.xyz, range]`, `[rgb, 0]`.
    // Rides this buffer (not bevy's clusterables) because the view layout exposes those to the
    // fragment stage only, and the Gouraud term is evaluated in the VERTEX stage.
    point_count: vec4<f32>,
    points: array<vec4<f32>, 512>,
    // The interior-prop SH probe table (lighting::prop_probes — 7 rows per slot, 8192 slots; keep in
    // sync with MAX_PROP_PROBES): the folded committed light of each lit interior MODD prop. The
    // prop's MeshTag payload is its slot; the interior-prop lane below evaluates rows
    // [7·slot .. 7·slot+7) over the fragment normal. Only this shader declares the region — the
    // other shaders mirror the buffer PREFIX and bind the same (larger) buffer.
    prop_probes: array<vec4<f32>, 57344>,
    // The owned skin palette (decision 0720; rig_palette.rs mirrors both sizes). `rig_table`:
    // one base bone index per rig slot (2048 = mesh_tag's 11-bit rig field; the instance's slot
    // rides its MeshTag bits 19-29). `palettes`: 3 vec4 rows per bone — the rows of
    // `rig_from_joint × inverse_bindpose`, the same matrix Bevy's skin lane would feed
    // `skin_model` except measured from the RIG's own origin rather than the map's (decision
    // 0974) — blended in the vertex stage below (WOW_RIG_SKIN).
    rig_table: array<u32, 2048>,
    // The per-instance body TINT, on the SAME slot index as `rig_table` (instance_tint.rs, decision
    // 0812): the CM2 `model+0x184/188/18c` modulate colour, packed `0xFFRRGGBB` exactly as the
    // reference packs its node value (`0x60d840`: `param | 0xff000000`). A word of **0 is identity**
    // — so slot 0 (every unskinned instance in the world), a zeroed studio buffer and an untinted
    // frame all cost nothing, while a genuine authored BLACK tint still reads as 0xFF000000.
    rig_tint: array<u32, 2048>,
    // The rig ORIGIN table (decision 0974), on the SAME slot index again: `xyz` = the world
    // position that rig's palette rows are measured from (`w` unused). The vertex stage adds it
    // back as `origin − camera`, so a skinned vertex is never expressed as an absolute world
    // coordinate in f32 — which is what the ~1 mm/frame character shimmer was.
    rig_origin: array<vec4<f32>, 2048>,
    palettes: array<vec4<f32>>,
};
@group(#{MATERIAL_BIND_GROUP}) @binding(90) var<storage, read> wow_light: WowLight;

// Vanilla M2 cutout alpha-test reference (224/255 on ≤ WotLK) — kept in sync with
// `debug_panel::VANILLA_ALPHA_KEY_REF`. Used to re-apply the hard cutout on the distance-fade blend
// twin so its silhouette matches the steady cutout exactly.
const VANILLA_ALPHA_KEY: f32 = 0.8784314;

// bevy's `VertexOutput` (same fields, same locations, same defs) + the per-vertex dynamic
// point-light term at a free location. One extra interpolant is why this can't BE `VertexOutput`;
// the fragment rebuilds one for `pbr_input_from_standard_material`. (Our meshes never carry
// tangents / morphs / visibility ranges, so those defs stay unset and unmirrored.)
struct WowVsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
#ifdef VERTEX_UVS_A
    @location(2) uv: vec2<f32>,
#endif
#ifdef VERTEX_UVS_B
    @location(3) uv_b: vec2<f32>,
#endif
#ifdef VERTEX_COLORS
    @location(5) color: vec4<f32>,
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    @location(6) @interpolate(flat) instance_index: u32,
#endif
    // The Gouraud-interpolated point-light sum (decision 0278): `Σ att·sat(N·L)·colour` evaluated at
    // the VERTEX, like the reference FFP — the tessellation-scale smoothing IS the authored look
    // (wide floor pools, dim hoods). The fragment folds in the live gain and the saturating clamp.
    @location(8) point_lit: vec3<f32>,
}

// The dynamic point-light term at a world-space point (decisions 0016/0273/0278, selection 0285) —
// the reference commits AT MOST THREE point lights per draw, the nearest to the RECEIVING UNIT'S OWN
// position (byte law: the gather `0x71bf90` keeps the nearest by squared distance from the
// caller-supplied unit position — never the camera, never the vertex — and the commit `0x71c730`
// seats slots 1-3, dropping the 4th; wow-re `wmo-surface-dynamic-light` §4/§6). Summing the whole
// table instead lit every candelabra pole in the abbey from a dozen sideways fixtures the real
// client never commits for it — the director's "stands must not light up with the point gain".
//
// So: pass 1 picks the ≤3 nearest table entries to `anchor` (the unit position — a light's packed
// range bounds candidacy); pass 2 evaluates ONLY those at the vertex — the byte-verified falloff
// `1/(0.7·d + 0.03·d²)`, diffuse-only (committed ambient/specular are zero), on the SUBMITTED normal
// (the FFP never enables GL_LIGHT_MODEL_TWO_SIDE — no per-face flip). A selected light reaches every
// vertex of its unit with no distance cutoff, exactly like a committed GL light — selection pops at
// unit granularity (the authored vanilla behaviour), never mid-surface. Mirrored in terrain.wgsl.
fn point_light_sum(P: vec3<f32>, N: vec3<f32>, anchor: vec3<f32>) -> vec3<f32> {
    let count = u32(wow_light.point_count.x);
    var sel = array<u32, 3>(0u, 0u, 0u);
    var sd = array<f32, 3>(1e30, 1e30, 1e30);
    for (var i = 0u; i < count; i = i + 1u) {
        let pos_range = wow_light.points[2u * i];
        let dv = pos_range.xyz - anchor;
        let d2 = dot(dv, dv);
        if (d2 > pos_range.w * pos_range.w) {
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

// The MCNK chunk cell center under a world point — the light-selection anchor for geometry merged in
// WORLD space (clutter; terrain.wgsl mirrors this): the reference draws terrain per 33.33-yd MCNK
// chunk and gathers that unit's lights like any other. Grid constants per the ADT format (chunk =
// 533.3333/16 yd, world half-extent 32 tiles); WoW x/y are Bevy −z/−x, and the grid is symmetric, so
// snapping Bevy x/z directly lands on the same cells. Height keeps the vertex's own y (the ref anchors
// at the chunk record's position; lights sit near the surface, so the horizontal snap is what matters).
fn mcnk_cell_anchor(P: vec3<f32>) -> vec3<f32> {
    let cell = 533.33333 / 16.0;
    let half = 32.0 * 533.33333;
    let ix = floor((half + P.x) / cell);
    let iz = floor((half + P.z) / cell);
    return vec3<f32>((ix + 0.5) * cell - half, P.y, (iz + 0.5) * cell - half);
}

// The vertex input — bevy 0.18's `forward_io::Vertex` fields at bevy's shader locations (the
// VERTEX_* defs come from the base mesh-pipeline specialize, driven by what the mesh authors;
// tangents / morphs never — no model mesh has them), PLUS the owned-palette joint attributes at
// locations 10/11 under WOW_RIG_SKIN (`WowModelExt::specialize` sets the def and appends the
// attributes to the buffer layout when the mesh carries `ATTRIBUTE_WOW_JOINT_INDEX` — decision
// 0720; Bevy's `forward_io::Vertex` only declares joints under its own SKINNED path, which no
// benilla mesh triggers anymore).
struct WowVertex {
    @builtin(instance_index) instance_index: u32,
#ifdef VERTEX_POSITIONS
    @location(0) position: vec3<f32>,
#endif
#ifdef VERTEX_NORMALS
    @location(1) normal: vec3<f32>,
#endif
#ifdef VERTEX_UVS_A
    @location(2) uv: vec2<f32>,
#endif
#ifdef VERTEX_UVS_B
    @location(3) uv_b: vec2<f32>,
#endif
#ifdef VERTEX_COLORS
    @location(5) color: vec4<f32>,
#endif
#ifdef WOW_RIG_SKIN
    @location(10) joint_indices: vec4<u32>,
    @location(11) joint_weights: vec4<f32>,
#endif
}

#ifdef WOW_RIG_SKIN
// The instance's rig slot — the shared index into `rig_table`, `rig_tint` and `rig_origin`.
fn wow_rig_slot(instance_index: u32) -> u32 {
    return (mesh_functions::get_tag(instance_index) >> 19u) & 0x7ffu;
}

// The owned-palette skin model (decision 0720): the instance's rig slot from its MeshTag rig
// field (bits 19-29) → the rig's base bone index → the four indexed bones' palette rows blended
// by the vertex weights. Returns `rig_from_local` — structurally what Bevy's `skin_model` returns
// (and it REPLACES the mesh's world matrix, never composes with it), except the translation is
// measured from the rig's own origin rather than the map's (decision 0974). `rig_origin[slot]`
// carries the missing piece; the vertex stage applies it camera-relative.
fn wow_skin_model(instance_index: u32, indices: vec4<u32>, weights: vec4<f32>) -> mat4x4<f32> {
    let base = wow_light.rig_table[wow_rig_slot(instance_index)];
    let b0 = 3u * (base + indices.x);
    let b1 = 3u * (base + indices.y);
    let b2 = 3u * (base + indices.z);
    let b3 = 3u * (base + indices.w);
    let r0 = weights.x * wow_light.palettes[b0]
        + weights.y * wow_light.palettes[b1]
        + weights.z * wow_light.palettes[b2]
        + weights.w * wow_light.palettes[b3];
    let r1 = weights.x * wow_light.palettes[b0 + 1u]
        + weights.y * wow_light.palettes[b1 + 1u]
        + weights.z * wow_light.palettes[b2 + 1u]
        + weights.w * wow_light.palettes[b3 + 1u];
    let r2 = weights.x * wow_light.palettes[b0 + 2u]
        + weights.y * wow_light.palettes[b1 + 2u]
        + weights.z * wow_light.palettes[b2 + 2u]
        + weights.w * wow_light.palettes[b3 + 2u];
    // r0/r1/r2 are the affine's ROWS; a wgsl matrix is column-major.
    return mat4x4<f32>(
        vec4<f32>(r0.x, r1.x, r2.x, 0.0),
        vec4<f32>(r0.y, r1.y, r2.y, 0.0),
        vec4<f32>(r0.z, r1.z, r2.z, 0.0),
        vec4<f32>(r0.w, r1.w, r2.w, 1.0),
    );
}

// bevy_pbr::skinning's normal math verbatim (inverse-transpose via the adjugate), on our matrix.
fn inverse_transpose_3x3m(in: mat3x3<f32>) -> mat3x3<f32> {
    let x = cross(in[1], in[2]);
    let y = cross(in[2], in[0]);
    let z = cross(in[0], in[1]);
    let det = dot(in[2], z);
    return mat3x3<f32>(x / det, y / det, z / det);
}

// (Translation-free by construction — so the rig-relative frame of decision 0974 feeds it
// unchanged: a normal never cared where the rig stands.)
fn wow_skin_normals(frame_from_local: mat4x4<f32>, normal: vec3<f32>) -> vec3<f32> {
    return normalize(
        inverse_transpose_3x3m(mat3x3<f32>(
            frame_from_local[0].xyz,
            frame_from_local[1].xyz,
            frame_from_local[2].xyz
        )) * normal
    );
}
#endif

// Custom vertex stage — bevy 0.18's `mesh.wgsl` vertex verbatim (VERTEX_* attributes; morph
// targets omitted — no model mesh authors them) with the owned-palette skinning in place of
// Bevy's SKINNED path (decision 0720), plus the per-vertex point-light evaluation on the
// post-skin world position/normal. A `MaterialExtension` swaps the whole stage, so the mirror
// must track bevy's on upgrades.
@vertex
fn vertex(vertex: WowVertex) -> WowVsOut {
    var out: WowVsOut;

    let mesh_world_from_local = mesh_functions::get_world_from_local(vertex.instance_index);
    // **Split the instance placement into (frame, origin)** — decision 0974. `frame_from_local`
    // carries the orientation and a SMALL translation; `frame_origin` carries the ~9 k-yard world
    // position. Skinned: the palette rows are already rig-relative and `rig_origin` is the rig's
    // world position. Unskinned: the mesh matrix's own translation column moves over. Either way
    // the world position is `frame_from_local · v + frame_origin`, and the point is that neither
    // factor is a big-times-small product — that product is where a ~1 mm f32 ULP was landing on
    // every animated vertex, freshly every frame.
#ifdef WOW_RIG_SKIN
    var frame_from_local = wow_skin_model(
        vertex.instance_index,
        vertex.joint_indices,
        vertex.joint_weights
    );
    let frame_origin = wow_light.rig_origin[wow_rig_slot(vertex.instance_index)].xyz;
#else
    var frame_from_local = mesh_world_from_local;
    let frame_origin = mesh_world_from_local[3].xyz;
    frame_from_local[3] = vec4<f32>(0.0, 0.0, 0.0, 1.0);
#endif

#ifdef VERTEX_NORMALS
#ifdef WOW_RIG_SKIN
    out.world_normal = wow_skin_normals(frame_from_local, vertex.normal);
#else
    out.world_normal = mesh_functions::mesh_normal_local_to_world(
        vertex.normal,
        vertex.instance_index
    );
#endif
#endif

#ifdef VERTEX_POSITIONS
    // Camera-relative all the way to clip space. `p_cam` is built from two small quantities, so
    // it keeps ~1e-7 yd of precision where the absolute route kept ~1e-3 — and `clip_from_view ×
    // (R_view · p_cam)` has none of the catastrophic cancellation `clip_from_world × p_world`
    // suffers when camera and geometry both sit at ~9 k (0733 §2 fixed the same defect on the
    // effect lane; this is the model lane's). `world_position` goes back to absolute for the
    // lighting/fog/shadow consumers downstream, which are not precision consumers.
    let p_cam = (frame_from_local * vec4<f32>(vertex.position, 1.0)).xyz
        + (frame_origin - view.world_position);
    out.world_position = vec4<f32>(p_cam + view.world_position, 1.0);
    let view_rot = mat3x3<f32>(
        view.view_from_world[0].xyz,
        view.view_from_world[1].xyz,
        view.view_from_world[2].xyz,
    );
    out.position = view.clip_from_view * vec4<f32>(view_rot * p_cam, 1.0);
    // WMO authored batch order (`m.sun_scale.y`; 0 = non-WMO ⇒ exact no-op): the client resolves
    // coplanar batches (wall + decal/trim) by strict MOBA draw order under depth-write + LEQUAL
    // (wow-5875-re `wmo-batch-blend-depth-state.md`, byte-verified); Bevy orders draws for
    // batching, so a later batch must instead WIN the reverse-Z GreaterEqual test. Scaling clip z
    // by (1 + n·2⁻²³) raises the interpolated depth z/w by exactly n ULP-steps per fragment —
    // the same one-unit-per-index nudge the old fixed-function `DepthBiasState` constant applied,
    // but as uniform DATA: as pipeline state it made every batch index its own pipeline, and a
    // first city sight synchronously compiled ~3000 of them on the render thread (decision 0837).
    out.position.z *= 1.0 + m.sun_scale.y * 1.1920929e-7;
#endif

#ifdef VERTEX_UVS_A
    out.uv = vertex.uv;
    // **Environment-mapped batches GENERATE their texcoord — here, in the VERTEX stage, because
    // that is where the reference generates it.** `Shaders\Vertex\Model2.bls` writes it as a
    // vertex-program output and lets the rasteriser interpolate:
    //
    //     DP3 R0.x, R1.xyz, R2.xyz          ; dot(P, N)      P = view-space skinned position
    //     MUL R0.x, c0.w, R0.x              ; ×2             N = normalize(view-space normal)
    //     MAD R0.yzw, -R0.x, R2.xxyz, R1.xxyz   ; R = P − 2(P·N)N
    //     DP3/RSQ/MUL                       ; normalize(R)
    //     MAD result.texcoord[3].xy, R0.xyxx, c1.x, c1.x   ; ·0.5 + 0.5   (c0.w = 2, c1.x = 0.5)
    //
    // — the `(0.5,0,0,0.5 / 0,0.5,0,0.5)` remap wow-re byte-derived at `0x70b8d0` (models.md §944),
    // reached whenever `texture_unit_lookup[texCoordSet] > 2` (`0x70b8bd`). Its space is pinned by
    // the same program: `c2` (projection alone) × `c31` × vertex = clip, so `c31` — and therefore
    // P and N — are **view space**, and wow-re's `lookat_v1` (`0x5c3e70`) stores row0 = side,
    // row1 = up, row2 = forward, so `R.xy` is (camera-right, camera-up). Bevy's view basis is
    // −Z-forward, which is exactly `F = diag(1,1,−1)`: `P'·N' = P·N`, hence `R' = F·R` and `R'.xy`
    // is **identical**. No handedness fixup is needed or wanted.
    //
    // `view_rot * p_cam` is the view-space position already built for clip space above, so this
    // costs one normalize and reuses the camera-relative precision (0974) instead of round-tripping
    // an absolute world coordinate. Decision 0971 evaluated this per FRAGMENT instead; measured on
    // `GnomeSubwayGlass` the two differ by ≤0.004 UV across a whole ring (the sphere-map disk has
    // radius 0.5), so the deviation bought nothing and cost fidelity — see decision 0980.
#ifdef VERTEX_POSITIONS
#ifdef VERTEX_NORMALS
    if ((u32(m.clutter_fade.z) & 4096u) != 0u) {
        let p_view = view_rot * p_cam;
        let n_view = normalize(view_rot * out.world_normal);
        let refl = normalize(p_view - 2.0 * dot(p_view, n_view) * n_view);
        out.uv = refl.xy * 0.5 + vec2<f32>(0.5, 0.5);
    }
#endif
#endif
#endif
#ifdef VERTEX_UVS_B
    out.uv_b = vertex.uv_b;
#endif

#ifdef VERTEX_COLORS
    out.color = vertex.color;
#endif

#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    out.instance_index = vertex.instance_index;
#endif

    // Dynamic point lights, by receiver class (wow-re trace-forensics-abbey-interior-d3d §2/§4):
    // WMO surfaces take NONE (zero point lights on every observed WMO surface batch — the earlier
    // "point-lit abbey wall" was a mis-identified unit draw), and interior M2 props take none HERE
    // (their group-MOLR point lobes are folded into the per-instance SH probe). Everything else —
    // exterior doodads, entities, clutter — keeps the FFP ≤3-nearest selection: clutter (merged in
    // world space) anchors at its MCNK chunk cell (the terrain draw unit it belongs to), every M2
    // at its INSTANCE origin (wow-re wmo-surface-dynamic-light §6 — the receiving unit's own
    // position, deliberately not the skinned per-vertex matrix).
    if (m.model_flags.x > 0.5 || m.model_flags.z > 0.5) {
        out.point_lit = vec3<f32>(0.0);
    } else {
        var anchor = mesh_world_from_local[3].xyz;
        if (m.clutter_fade.w > 0.5) {
            anchor = mcnk_cell_anchor(out.world_position.xyz);
        }
        out.point_lit = point_light_sum(out.world_position.xyz, out.world_normal, anchor);
    }
    return out;
}

@fragment
fn fragment(in: WowVsOut, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    // HARD FAR-CLIP WALL (faithful `farclip` ~777 yd) — see terrain.wgsl. Per-pixel discard beyond the
    // projection far plane (planar eye-Z), so distant buildings/trees reveal closest-part-first and the
    // sky/WDL shows behind. `wow_light.fog_params.w` = farclip (0 ⇒ disabled). Clutter (≤70 yd) never hits it.
    if (wow_light.fog_params.w > 0.0) {
        let clip_z = -(view.view_from_world * vec4<f32>(in.world_position.xyz, 1.0)).z;
        if (clip_z > wow_light.fog_params.w) {
            discard;
        }
    }
    // Rebuild bevy's `VertexOutput` from our extended interstage struct (WowVsOut carries one extra
    // interpolant — the per-vertex point term — which the pbr entry point doesn't know about).
    // M2 UV animation folds in here (decision 0130 phase 3, wow-re m2-texanim-uv): add the batch's
    // live texture-transform translation to the stage UVs before the base-colour sample — the real
    // client's composed matrix collapses to exactly this for the translation-only doodad corpus
    // (translation is un-pivoted; rotation/scaling — pivoted at (0.5, 0.5) — are authored by no
    // placed world doodad). `sun_scale.zw` is 0 for static batches, so this is a no-op there.
    var vo: VertexOutput;
    vo.position = in.position;
    vo.world_position = in.world_position;
    vo.world_normal = in.world_normal;
#ifdef VERTEX_UVS_A
    // **Environment-mapped batches GENERATE their texture coordinates** (`clutter_fade.z` bit 12,
    // model_render's `ENV_MAP_MARKER`; the asset's `texture_unit_lookup[texCoordSet] > 2`). Such a
    // batch authors NO usable UVs — GnomeSubwayGlass's 330 vertices all sit at exactly (0,0),
    // because the runtime is meant to supply them — so reading the raw vertex UV paints the whole
    // surface in one corner texel of a reflection sheet (the Deeprun Tram tube's flat yellow).
    //
    // The coordinate itself is generated in the VERTEX stage, where the reference generates it
    // (see the derivation there); `in.uv` already carries it, interpolated. All that is left here
    // is to keep the UV **animation** off it: the reference's gate excludes an env stage from
    // `textureTransform` by construction (`m2-texanim-uv` §2), so adding the live translation
    // would drift a reflection that must stay pinned to the view.
    if ((u32(m.clutter_fade.z) & 4096u) != 0u) {
        vo.uv = in.uv;
    } else {
        vo.uv = in.uv + m.sun_scale.zw;
    }
#endif
#ifdef VERTEX_UVS_B
    vo.uv_b = in.uv_b;
#endif
#ifdef VERTEX_COLORS
    vo.color = in.color;
#endif
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    vo.instance_index = in.instance_index;
#endif
    var pbr_input = pbr_input_from_standard_material(vo, is_front);
    // Ground-clutter distance fade: multiply the cutout alpha by the camera-distance ramp BEFORE the
    // alpha test, so distant detail doodads erode out by `clutter_fade.y` yd (the client's ~70-yd
    // horizon). Disabled (no-op) for normal models where `clutter_fade.w == 0`. This is a draw-distance
    // concern, not lighting, so it survives the Phase-0 strip.
    var base_color = pbr_input.material.base_color;
    // Interior WMO batches carry LIGHTING data in the MOCV ALPHA — TRANS (tint.w == 2) the
    // lit↔bake lerp factor, INT (tint.w == 1) the ×4 self-illumination mask. Bevy pre-folds
    // ATTRIBUTE_COLOR (rgba) into base_color, so un-fold the alpha before any cutout test —
    // coverage stays the texel alpha, exactly the reference's interior pixel shader
    // (its output alpha is tex.a; MOCV.a never reaches coverage).
#ifdef VERTEX_COLORS
    if (m.model_flags.x > 0.5 && m.tint.w > 0.5) {
        base_color.a = base_color.a / max(in.color.a, 1.0 / 255.0);
    }
#endif
    // Ground-clutter distance fade: multiply the cutout alpha by the camera-distance ramp BEFORE the
    // alpha test, so distant detail doodads ERODE out by `clutter_fade.y` yd (the client's ~70-yd
    // horizon — clutter's own faithful alpha-test fade). No-op where `clutter_fade.w == 0`. Distinct
    // from the world-doodad fade below; this stays before the test, that one does not.
    if (m.clutter_fade.w > 0.5) {
        let d = distance(view.world_position.xyz, in.world_position.xyz);
        let f = clamp((m.clutter_fade.y - d) / max(m.clutter_fade.y - m.clutter_fade.x, 0.001), 0.0, 1.0);
        base_color.a = base_color.a * f;
    }
    // Faithful per-object WORLD-DOODAD distance fade (`FUN_00683f80`/`model_fade.rs`): the fade alpha
    // (1.0 = opaque) rides in the per-instance `MeshTag`; tag 0 (clutter/WMO) ⇒ 1.0 no-op.
    // VERIFIED reference behaviour (`RECONCILE-fade-render-state.md`): a fading doodad is the SAME draw
    // as the steady cutout — the alpha-test ref scales WITH the fade so the effective cutoff stays a
    // constant `tex0.a < 224/255` (STABLE silhouette, never grows/snaps) — with blend on and source
    // alpha = `tex0.a × fade`. On the blend twin `AlphaMode::Blend` does no discard, so we re-apply
    // that hard cutout here on the UNFADED alpha — but ONLY for a twin whose SOURCE batch alpha-tests
    // (clutter_fade.z bit 10, model_render's TWIN_CUTOUT_MARKER): the reference keys ALPHAREF on the
    // STORED blend mode (m2-blend-promotion-zfill.md §2), so a fading/stealthed OPAQUE batch blends
    // with no alpha test at all. Keying the discard on the twin bit itself cut every texel under
    // 224/255 out of promoted Opaque batches — Gressil's blade body erased to its rune pattern under
    // stealth (decision 0842). `specialize` keeps depth-write ON for every twin (`model_flags.y`).
    // The payload is TYPED (mesh_tag.rs, decisions 0173/0720): bits 0-5 = the fade alpha (6-bit
    // fraction; a whole payload of 0 = the untagged ⇒ opaque sentinel), bits 19-29 = the skin
    // rig slot (vertex-stage concern — the fragment never reads it, but it rides every payload,
    // so the 0-sentinel test uses the WHOLE masked payload as before). Between them the exterior
    // payload carries the per-instance ground-shade byte in bits 6-13 (0 lit → 255 MCSH-shadowed
    // — decoded at the doodad sun below; entities ramp it, statics leave 0). On an interior-mode
    // material (interior z, not WMO x) bits 6-18 carry the SH-probe SLOT instead — static MODD
    // props at spawn, and every indoor entity on the footprint-bake law (decision 0354: units
    // keep the probe lane indoors; the day/night state is the exterior material at the
    // intensity-1.0 shade byte, not a mode of its own).
    // Tag bits 31/30 are standalone flags (mesh_tag.rs), split off before the payload decode so the
    // 0-sentinel and both payload modes read the masked value: bit 31 = hover/target HIGHLIGHT,
    // bit 30 = INTERIOR FOG — the instance's model stands in a WMO interior, so it fogs with the
    // interior triple below (the reference stages unit fog by the unit's own classification,
    // wow-re m2-unit-interior-fog.md).
    let interior_prop = m.model_flags.z > 0.5 && m.model_flags.x < 0.5;
    let raw_tag = mesh_functions::get_tag(in.instance_index);
    let highlighted = (raw_tag & 0x80000000u) != 0u;
    let interior_fogged = (raw_tag & 0x40000000u) != 0u;
    // Bits 0-5 are the fade alpha in BOTH payload modes, so a feathering indoor entity keeps its
    // probe AND its alpha ramp — and a skinned part keeps its rig slot through either.
    let fade_tag = raw_tag & 0x3fffffffu;
    let alpha6 = f32(fade_tag & 0x3fu) / 63.0;
    let obj_fade = select(alpha6, 1.0, fade_tag == 0u);
    // The per-instance body TINT (instance_tint.rs, decision 0812) — the aura state kit's CharProc 1:
    // an aura painting the whole model one colour (ghost pale blue, poison green, Frostbolt blue).
    // Indexed by the SAME rig slot the vertex stage skins from (bits 19-29), so it needs no tag bits
    // of its own; a `0` word is identity, which is every unskinned instance (slot 0) and every
    // untinted unit. It is the material's ambient+diffuse colour (gx SetState(1) →
    // GL_COLOR_MATERIAL), so it multiplies the light sum INSIDE the clamp below and never the
    // emission terms — the same placement the WMO branch already gives MOCV.
    let tint_word = wow_light.rig_tint[(fade_tag >> 19u) & 0x7ffu];
    let inst_tint = select(
        vec3<f32>(
            f32((tint_word >> 16u) & 0xffu),
            f32((tint_word >> 8u) & 0xffu),
            f32(tint_word & 0xffu),
        ) * (1.0 / 255.0),
        vec3<f32>(1.0),
        tint_word == 0u,
    );
    if ((u32(m.clutter_fade.z) & 1024u) != 0u && base_color.a < VANILLA_ALPHA_KEY) {
        discard;
    }
    // DEPTH-PRIME TWIN (the zfill pipeline variant — terrain.rs `specialize`, the reference's
    // M2UseZFill clone command, wow-re m2-blend-promotion-zfill.md §4): colour writes are masked
    // off at the pipeline, so on that variant only the discards above (farclip wall, the
    // bit-10 twin cutout) shape what depth gets written; the lighting/fog below is computed and
    // thrown away. (A shader-def early return here would be the natural spelling, but naga's MSL
    // backend miscompiles the dead tail — "redefinition of '_tmp'" — so the twin pays the colour
    // math instead. Episodes are transient; the cost is bounded.)
    // Blend source alpha = texel alpha × fade (translucent fade of the fixed cutout shape). For steady
    // cutout/opaque draws blend is off so this is ignored; for the fade twin it drives the feather.
    let faded_alpha = base_color.a * obj_fade;
    // Steady cutout (Mask) / opaque discard per the StandardMaterial alpha mode (no-op for the blend twin).
    let base = alpha_discard(pbr_input.material, base_color);

    // --- STEPS 7+8d: matte lighting, split on clutter ---------------------------------------------
    // M2/WMO meshes get the same directional matte lighting as terrain — `lit = clamp(ambient +
    // diffuse·max(N·L,0))`, where N is the model's authored vertex normal and L is the Bevy-space
    // sun travel dir (to-light = `−light_sun`).
    //
    // **Clutter is lit by the GROUND normal under each tuft** (Step 8d). VERIFIED faithful: WoW's
    // CreateDetailDoodads computes the terrain quadrant-plane normal under the tuft and writes it onto
    // the clutter vertex's normal channel (docs/knowledge/ground-effects.md), and the reference's
    // clutter draw (apitrace WoW.5, prog 186 / alpha-ref 128/255) lights per-vertex with
    // `dot(L, that normal)` × a per-vertex colour, MODULATE × texture — so a tuft darkens with the
    // ground it stands on (shaded/sloped tufts go darker, like the dirt beneath). We bake that normal
    // in clutter.rs (`terrain_normal_at`). World-up was an earlier flat-ground approximation, removed.
    // **Exterior M2 doodads take the verified `Model2.bls` sun curve** (0747, below) with the
    // diffuse/sun term scaled by the terrain-shade at the doodad's base (lit vs MCSH-shadowed ground);
    // clutter and WMO keep the plain FFP `ambient + diffuse·max(N·L,0)` — clutter lit by the ground
    // normal, WMO by its own (both genuinely fixed-function reference programs).
    let is_clutter = m.clutter_fade.w > 0.5;
    let L = -normalize(wow_light.light_sun.xyz);
    let n_m2 = normalize(pbr_input.world_normal);
    // Bevy negates `world_normal` on the back faces of any DOUBLE-SIDED material — two-sided foliage
    // cross-quads (grass tufts, leaf cards) AND every WMO group face (our WMO loader marks all WMO
    // submeshes two-sided, models.rs). The reference has NO such per-face negation: WoW sets the GL
    // lighting model once (`FUN_0059ce30`) and NEVER enables `GL_LIGHT_MODEL_TWO_SIDE`, so BOTH faces of
    // every polygon — clutter, M2 doodad, WMO group — are lit from the SAME submitted normal. So we
    // un-flip Bevy's negation here and light EVERY path below from `n_lit`; the raw `n_m2` is never used
    // for lighting directly. For single-sided materials back faces are culled ⇒ `is_front` always true
    // ⇒ `n_lit == n_m2` (no-op). Universal "two-side-off" fix → no view-dependent lit/unlit seam, on
    // M2 foliage (doodad matte path) OR WMO group geometry (the FFP N·L path below) (foliage.md).
    let n_lit = select(-n_m2, n_m2, is_front);
    let ndotl = max(dot(n_lit, L), 0.0);
    let lit_nl = clamp(wow_light.light_ambient.rgb + wow_light.light_diffuse.rgb * ndotl, vec3<f32>(0.0), vec3<f32>(1.0));
    // The order-2 SH basis products over the fragment normal — shared by the global exterior eval
    // below and the interior-prop probe lane further down.
    let quad = vec4<f32>(n_lit.x * n_lit.y, n_lit.y * n_lit.z, n_lit.z * n_lit.z, n_lit.x * n_lit.z);
    let n1 = vec4<f32>(n_lit, 1.0);
    let x2y2 = n_lit.x * n_lit.x - n_lit.y * n_lit.y;
    // Exterior world doodad/entity: **the reference's own response** —
    // `clamp01(A + C·I·(4/17)(0.375 + 2μ + 1.875μ²))`, μ = N·u toward-light, the order-2
    // `Model2.bls` lobe evaluated over the SH rows above (0803; read that record and 0796 before
    // touching this lane).
    //
    // The long way round, because the scar tissue matters: 0410 replaced this curve with a
    // hard-cutoff FFP matte on the director's look call — a sunlit character's shadow side must read
    // the SAME as the same skin standing in shade, and the lobe's authored wrap (0.059·C on the
    // anti-sun side, dipping −0.037·C mid-back) tinted it warm and lifted it. 0753 then read the
    // reference's own apitrace as CORROBORATING the cutoff (an FFP light slot, "no SH constants
    // anywhere in the trace"). **0796 refuted that**: wow-re's §5 cross-check shows the reference's
    // M2 lane — ADT doodad, GameObject, creature, player alike — is its own authored `Model2.bls`
    // vertex shader running order-2 SH, and the FFP light commits visible in a world frame belong to
    // TERRAIN (`0x71c730`'s five `0x68xxxx` call sites) and the WMO path. M2's single FFP call site
    // (`70bdf6`) is reachable only with `M2UseShaders` off. "No SH constants" was a measurement
    // artifact — constants upload as dirty-range deltas whose start register varies. That left 0410
    // standing as a look preference against a verified curve, so 0799 put the two behind an A/B and
    // 0803 is the director choosing the curve. The cutoff branch left with the flag.
    //
    // The CPU light animator targets the per-instance intensity `[node+0xa4]` — 2.5 on lit ground
    // / 0.5 on MCSH-shadowed ground / 1.0 on the interior/WMO-prop leg (`0x69e4ad`, `0x69e280`;
    // the force-1.0 leg is `0x69e36b`). Lane history: `Model2.bls` SH lobe (0354/0358) → FFP
    // matte, hard cutoff (0410, director's call) → unclamped source (0706) → verified curve
    // (0747) → peak-norm (0750) → calibration dial (0751) → the 0753 trace law → 0796 (kept the
    // pixels, demoted the justification) → 0799 (the A/B) → 0803, back on the curve for good.
    //
    // The per-material `sun_scale.x` selector has THREE states (model_render::ShadeSel): ≥0.85 =
    // the lit-ground family (ADT doodads and every entity M2 — animator target 2.5, mixed toward
    // 0.5 by the per-instance tag shade byte, which units/players/GameObjects ramp CPU-side like
    // the binary's `0x69e770`; statics leave it 0), 0.5..0.85 = fixed intensity 1.0 (an exterior
    // WMO MODD prop — the 2.5 site is one a MODD prop never reaches, §8b), <0.5 = statically
    // MCSH-shadowed (0.5). The ramp runs in animator units and the COMMIT clamps.
    //
    // **`min(I, 1)` is OURS, and it is the one unfaithful term in this lane (0803 §3, 0814, 0821).**
    // The reference does not cap the gain. On the VS/SH lane — the default config, and the lane this
    // shader IS — `0x71c4e0` bakes `[node+0xa4]` straight into the SH moments with no clamp; on the
    // FFP lane `0x71c730`→`0x71ca80` clamps the **product** `D × I` (by max-channel, preserving hue),
    // never the multiplier. So the cap below is a benilla choice, and it costs us twice:
    //
    //   1. **Brightness.** A lit ADT doodad commits ×1.0 where the reference gives ×2.5, so doodads
    //      and characters in sun read dimmer than the reference. Still open — lifting it pushes a
    //      sun-facing surface well past 1.0 (with an over-gamut sun, into green as well), which is a
    //      world-wide look change and the director's call, not one to self-grade.
    //   2. **Timing — fixed CPU-side (0821).** Because the cap sits on the multiplier, every target
    //      from 2.5 down to 1.0 renders identically, so a unit ramping 2.5 → 0.5 spent its first
    //      0.45 s (75 % of the chase) invisibly pinned at 1.0 and then dropped in 0.15 s. It read as a
    //      dead pause then a snap — what the director reported walking sun → shade.
    //      `entity_shade::LIT_T` now aims the LIT target at the value this cap can actually show
    //      (1.0), so the chase moves only through visible range, at the reference's own
    //      3.3333 intensity-units/s. The cap is a backstop here, not the thing the ramp fights.
    //
    // Two faces of one bug, and they unwind together: **cut `min(I, 1)` and `LIT_T` goes back to 0.0**
    // so the full 2.5 → 0.5 sweep becomes visible on its own. Do not cut one alone. (Units are on this
    // chain again — 0809's flat ×1.0 pin was wrong and 0814 reverted it; the null fallback that
    // motivated it is real, but it is a lifecycle state we do not model.)
    let inst_shade = select(f32((fade_tag >> 6u) & 0xffu) / 255.0, 0.0, interior_prop);
    let mat_shade = select(0.0, 1.0, m.sun_scale.x < 0.5);
    let shade_t = max(mat_shade, inst_shade);
    let mid_band = m.sun_scale.x >= 0.5 && m.sun_scale.x < 0.85;
    let intensity = min(select(mix(2.5, 0.5, shade_t), 1.0, mid_band), 1.0);
    // The lobe. Same closed form the interior-prop lane below runs and the same one
    // `pack_model_core_rows` folds. Every sun band is linear in the committed colour, so ONE
    // `intensity` multiply covers the whole lobe (never I²); ambient rides the c10 `.w` lanes and
    // does NOT scale with it, and the sun's DC redistribution rides `grade.yzw`.
    let sun_dc = wow_light.grade.yzw * intensity;
    let sun_lobe = vec3<f32>(
        wow_light.sh_c10_r.w + sun_dc.x
            + intensity
                * (dot(wow_light.sh_c10_r.xyz, n_lit) + dot(wow_light.sh_c13_r, quad)
                    + wow_light.sh_c16.x * x2y2),
        wow_light.sh_c10_g.w + sun_dc.y
            + intensity
                * (dot(wow_light.sh_c10_g.xyz, n_lit) + dot(wow_light.sh_c13_g, quad)
                    + wow_light.sh_c16.y * x2y2),
        wow_light.sh_c10_b.w + sun_dc.z
            + intensity
                * (dot(wow_light.sh_c10_b.xyz, n_lit) + dot(wow_light.sh_c13_b, quad)
                    + wow_light.sh_c16.z * x2y2),
    );
    // clamp01 the SUM, never per term — the lobe dips to −0.037·C around μ≈−0.53 (low-order-SH
    // ringing, the reference's own authored response), and clamping the sun term alone would floor
    // that away and erase the mid-back dip. That dip is part of the response 0803 chose, not noise
    // to tidy up.
    let lit_doodad = clamp(sun_lobe, vec3<f32>(0.0), vec3<f32>(1.0));
    // WMO buildings (model_flags.x) use the FFP directional N·L at sun-scale 1.0 — the reference lights them
    // with `ambient + sun·max(N·L,0)` and does NOT apply the exterior doodad terrain-shade (verified: prog
    // 198/VS 151). Their per-vertex MOCV shade rides in `base_color` (ATTRIBUTE_COLOR) → folds into `albedo`,
    // giving tex × MOCV × lit. Exterior world doodads (not clutter, not WMO) take the terrain-shaded matte;
    // clutter uses its own ground-normal matte. `light_sun.w` is the directional-enable flag (on outdoors).
    let is_wmo = m.model_flags.x > 0.5;
    let is_interior = m.model_flags.z > 0.5;
    let use_doodad_shade = (wow_light.light_sun.w > 0.5) && !is_clutter && !is_wmo;
    let lit_exterior = select(lit_nl, lit_doodad, use_doodad_shade);
    // WMO INTERIOR surfaces (model_flags.z; groupFlags & 0x48 == 0) — by BATCH CLASS (tint.w; wow-re
    // trace-forensics-abbey-interior-d3d §2, observed on the abbey at close range):
    //   INT (tint.w = 1): UNLIT — the draw is pure `tex × MOCV`; the baked vertex colours (the
    //     artists' lamp/forge/hearth/candle warmth) ARE the room's light, constant day and night.
    //     No exterior light, no point lights (the reference commits zero to any WMO surface).
    //   TRANS (tint.w = 2): the per-vertex MOCV-ALPHA LERP between the day/night-lit surface and
    //     that unlit bake — the reference's two-pass (lit × SRC_ALPHA + unlit × (1−SRC_ALPHA))
    //     collapsed to one pass: `mix(1, extLit, MOCV.a)` as the lit factor.
    //   EXT (tint.w = 0): an interior group's exterior-law batches — plain `lit_nl`.
    //
    // WINDOW (MOMT 0x20, m.sidn.w) — interior drawer only: the batch's lit lanes swap GL_LIGHT0 to
    // the brighter interior pair, ambient AND diffuse = the MIDPOINT of the Direct (sun diffuse) and
    // Ambient bands, ambient +16/255 saturating (wow-re wmo-interior-night-light §2, 0x6d37e0). It
    // folds the warm Direct band in at full weight, so an interior pane reads bright and warm in
    // daylight instead of taking the flat exterior ambient — and still tracks time of day. The
    // exterior drawer has no WINDOW machinery, so exterior-group batches keep plain lit_nl.
    var trans_a = 1.0;
#ifdef VERTEX_COLORS
    trans_a = in.color.a;
#endif
    let window_mid = 0.5 * (wow_light.light_ambient.rgb + wow_light.light_diffuse.rgb);
    let lit_window = clamp(
        window_mid + vec3<f32>(16.0 / 255.0) + window_mid * ndotl,
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
    let lit_int_base = select(lit_nl, lit_window, m.sidn.w > 0.5);
    var lit_wmo_interior = vec3<f32>(1.0);
    if (m.tint.w > 1.5) {
        lit_wmo_interior = mix(vec3<f32>(1.0), lit_int_base, trans_a);
    } else if (m.tint.w < 0.5) {
        lit_wmo_interior = lit_int_base;
    }
    // INTERIOR M2 PROPS — WMO MODD doodads only (inn kegs/tables/candelabra; is_interior but NOT
    // is_wmo). The real client fills the prop's base ONCE at create from the MODD entry's own baked
    // colour (never a footprint sample — that chain is the ADT-MDDF path) and commits it with the
    // fixed-axis diffuse lobe + the owning group's MOLR point lobes as an order-2 SH probe the
    // vertex shader evaluates (wow-re trace-forensics-abbey-interior-d3d §1, decoded live off the
    // abbey stands to ~1e-7). benilla folds the identical closed form at spawn
    // (`lighting::prop_probe_coeffs`) into the per-instance probe table; the MeshTag payload is the
    // slot. Evaluated here per fragment over the same basis — note the SH lobe's soft wrap (side-on
    // ≈ 0.088·C) is the reference's authored response, deliberately NOT a hard max(N·L,0).
    // Units/GameObjects never reach this lane (base CGLight — plain day/night ×1.0, §8/§9).
    let probe = 7u * ((fade_tag >> 6u) & 0x1fffu);
    let lit_m2_interior = clamp(
        vec3<f32>(
            dot(wow_light.prop_probes[probe + 0u], n1)
                + dot(wow_light.prop_probes[probe + 3u], quad)
                + wow_light.prop_probes[probe + 6u].x * x2y2,
            dot(wow_light.prop_probes[probe + 1u], n1)
                + dot(wow_light.prop_probes[probe + 4u], quad)
                + wow_light.prop_probes[probe + 6u].y * x2y2,
            dot(wow_light.prop_probes[probe + 2u], n1)
                + dot(wow_light.prop_probes[probe + 5u], quad)
                + wow_light.prop_probes[probe + 6u].z * x2y2,
        ),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
    let lit_interior = select(lit_m2_interior, lit_wmo_interior, is_wmo);
    // AUTHORED-RIG lane (`ShadeSel::Rig`, sun_scale.x = 2.0 — the glue create booth, decision
    // 0429): the lit value is the probe-slot SH eval — the scene M2's ambient + directional
    // lights, folded into slot 0 of the material's OWN buffer (booth instances carry tag 0, so
    // `lit_m2_interior` above already evaluated exactly that probe). No sun, no intensity family,
    // no day/night — a glue scene's light is entirely its authored rig. Rig materials are neither
    // WMO nor interior, so the per-vertex point term (the rig's authored point lights) flows in
    // through `point_diffuse` below like any exterior entity.
    let is_rig = m.sun_scale.x >= 1.5;
    let lit = select(select(lit_exterior, lit_interior, is_interior), lit_m2_interior, is_rig);
    // Gamma-space albedo — the lane (0161): the buffer holds bytes, lighting math runs on the
    // authored values. (The old fog_params.z linear-space A/B is dead — settled by the lane.)
    // `m.tint` is the animated M2Color RGB (identity 1 for static batches) — the same per-batch
    // tint the vertex colours carry for constant tracks, so it folds at the same point.
    let albedo = base.rgb * m.tint.rgb;
    // Unlit fullbright (model_flags.w): M2 UNLIT (0x01) glass/glow cards, or WMO UNLIT on an
    // exterior-group batch (`tex × white` — the inn's always-lit outside panes). Wins over the lit
    // path; faithfully receives NO emission terms (lighting is off, so GL_EMISSION is dead there).
    let is_emissive = m.model_flags.w > 0.5;
    // SIDN night glow (MOMT 0x10, m.sidn.rgb): the authored emissive × the live night fraction
    // (grade.x — 1 overnight, 0 all day, ramps 20:30→21:30 / 06:00→07:00). A GL material EMISSION
    // term, so it adds INSIDE the clamped lit sum (tex × (lit + sidn·night)) and reaches LIT lanes
    // only: exterior-drawer lit batches and an interior group's EXT lane at full weight, TRANS by
    // its lit-pass weight (MOCV.a), and never the unlit INT lane (wow-re wmo-interior-night-light
    // §4, wmo-lit-selector §1.3). Zero for every M2 batch.
    var sidn_w = 1.0;
    if (is_interior && is_wmo) {
        if (m.tint.w > 1.5) {
            sidn_w = trans_a; // TRANS: emission rides the lit pass A, weighted by the lerp
        } else if (m.tint.w > 0.5) {
            sidn_w = 0.0; // INT: lighting off — the emissive write is dead, like the FFP
        }
    }
    let sidn_e = m.sidn.rgb * (wow_light.grade.x * sidn_w);
    // WoW dynamic point lights (decisions 0016/0273/0278, selection 0285) — exterior doodads,
    // entities, clutter, and terrain receive their unit's committed lights: the ≤3 NEAREST to the
    // receiving unit's own position, never the whole scene (`point_light_sum`). WMO surfaces take
    // ZERO (observed on every WMO surface batch in the abbey capture) and interior props fold their
    // group-MOLR lobes into the SH probe instead — both zeroed in the VERTEX stage. The term
    // arrives GOURAUD-INTERPOLATED — per-vertex like the reference FFP, whose tessellation-scale
    // smoothing is the authored look. Diffuse-only (committed ambient/specular are zero).
    let point_diffuse = in.point_lit;

    // Hover/target model brighten (tag bit 31): the client's per-model highlight emissive —
    // `glMaterialfv(GL_EMISSION, +64/255)` per channel (shipped config default 0xff404040), verified
    // wow-re selection-circle PART 2. GL_EMISSION adds INSIDE the lighting sum, which is clamped [0,1]
    // BEFORE the texture modulate — darks lift toward fully-lit, already-bright spots saturate. It
    // rides the lighting equation, so the fullbright/UNLIT path below faithfully never receives it.
    let highlight = select(0.0, 0.2509804, highlighted);
    // FFP combine (byte + trace verified, decision 0273): the LIGHT SUM — matte base + point lights +
    // the highlight emission — saturates per fragment FIRST, and the texture modulates the clamped
    // result, so a surface never exceeds its own fully-lit texture (a fixture light at zero distance
    // drives the prop to tex×1, not past it — the old `clamp(albedo·sum)` order blew emitter props to
    // saturated gold). WMO surfaces fold their vertex colour INSIDE the clamp (GL_COLOR_MATERIAL:
    // MOCV is the material ambient+diffuse — `tex × clamp(MOCV·sum + emission)`), so a strong fixture
    // light overdrives a dim bake toward the full texture exactly like the reference. Bevy pre-folds
    // ATTRIBUTE_COLOR into `base`, so un-fold it with a guarded divide (a dim channel's product is ~0
    // either way). At zero point contribution every factor is ≤1 and both forms collapse to the old
    // product — the approved interior/exterior base looks are preserved bit-for-bit.
    var lit_rgb: vec3<f32>;
#ifdef VERTEX_COLORS
    if (is_wmo) {
        let vc = in.color.rgb;
        // MOCV multiplies the lit terms (GL_COLOR_MATERIAL) but NOT the emission terms — SIDN and
        // the highlight add alongside, exactly the FFP's material-emission placement.
        let primary = clamp(
            vc * (lit + point_diffuse) + sidn_e + vec3<f32>(highlight),
            vec3<f32>(0.0),
            vec3<f32>(1.0),
        );
        let tex_rgb = base.rgb / max(vc, vec3<f32>(1.0 / 255.0));
        lit_rgb = tex_rgb * m.tint.rgb * primary;
        if (is_interior && m.tint.w > 0.5 && m.tint.w < 1.5) {
            // INT: the MOCV-ALPHA SELF-ILLUMINATION term. The reference's interior pixel shader is
            // `tex·MOCV.rgb·(1 + 4·MOCV.a)` with only the framebuffer's final [0,1] clamp — read
            // off the client's own D3D pixel shader in the Goldshire-inn trace (literal 4.0 in the
            // source, no lights referenced), so the glow multiplies the FULL product and may
            // overdrive it to white, never pre-clamped like the FFP light sum above. The alpha
            // channel is an authored emissive mask: the inn fireplace surround bakes α≈100 (×2.6),
            // hearths glow, and the FixColorVertexAlpha 255 at interior↔exterior portal seams
            // lifts doorways to full brightness. Near-zero everywhere unpainted (the abbey rooms),
            // where this collapses to the plain tex×MOCV it replaces.
            lit_rgb = clamp(
                tex_rgb * m.tint.rgb * vc * (1.0 + 4.0 * trans_a),
                vec3<f32>(0.0),
                vec3<f32>(1.0),
            );
        }
    } else {
        // An M2 instance: `inst_tint` is its CM2 modulate colour, multiplying the light terms inside
        // the clamp exactly where the WMO branch above multiplies MOCV — both are the material's
        // ambient+diffuse under GL_COLOR_MATERIAL — and never the emission terms beside them. It is
        // identity for everything untinted, so this is the old product bit-for-bit until an aura
        // writes a colour. (Left off the `is_wmo` branch on purpose: a WMO surface is not a CM2
        // instance and has no tint slot of its own.)
        let primary = clamp(
            inst_tint * (lit + point_diffuse) + sidn_e + vec3<f32>(highlight),
            vec3<f32>(0.0),
            vec3<f32>(1.0),
        );
        lit_rgb = albedo * primary;
    }
#else
    // (`sidn_e` is zero for every M2 batch; a WMO batch without MOCV lands here too and keeps it —
    // harmlessly, since a WMO instance's tint slot is the identity slot 0.)
    let primary = clamp(
        inst_tint * (lit + point_diffuse) + sidn_e + vec3<f32>(highlight),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
    lit_rgb = albedo * primary;
#endif
    // The fullbright/UNLIT path takes the tint too, unlike the highlight: with GL_LIGHTING off the
    // same gx state (SetState(1)) is a plain `glColor` modulate on the texture, while GL_EMISSION is
    // dead. So a ghost's glow cards tint with its body.
    var rgb = select(lit_rgb, albedo * inst_tint, is_emissive);

    // Step 5 fog — same gamma-space linear fog as terrain.wgsl. Same DBC values are pushed onto
    // both materials by `apply_wow_lighting`, so a tree and the dirt under it land on the same
    // haze byte at the same distance. Fog coordinate is PLANAR eye-Z (view-space depth), NOT radial
    // distance — see terrain.wgsl for the apitrace-verified rationale (radial over-fogs the edges).
    // The fog COLOUR is per-batch policy (M2 state setter 0x70baf0, wow-re ROUND 4): scene for
    // opaque/alpha, BLACK for additive (the batch fades under the storm veil instead of adding grey
    // — the level-up fix), WHITE for Mod, GREY for Mod2x; policy 4 (render flag 0x02) = unfogged.
    // Encoded in clutter_fade.z bits 4-6.
    // Interior lanes fog with the INTERIOR triple — the room keeps its warm MFOG haze while the
    // storm's veil stays on everything seen through the door. Two routes in: the MATERIAL flag
    // (WMO interior-group batches and their doodad props, m.model_flags.z — round-6 Q-I:
    // `0x6b5190`/`0x6b62e0` push DNState+0x80/84/88) and the per-INSTANCE tag bit 30 (an entity
    // M2 whose model stands in a WMO interior — the reference stages a unit's fog by the unit's
    // own light-node classification, `0x71c110`/`[node+0xc]`, wow-re m2-unit-interior-fog.md;
    // at camera-out the triples are equal, so the bit only diverges inside a fogged WMO).
    // Every other lane inherits the scene fog.
    var fog_color = wow_light.fog_color;
    var fog_span = wow_light.fog_params.xy;
    if (m.model_flags.z > 0.5 || interior_fogged) {
        fog_color = wow_light.wmo_fog_color;
        fog_span = wow_light.wmo_fog_params.xy;
    }
    let fog_policy = (u32(m.clutter_fade.z) >> 4u) & 7u;
    if (fog_color.w > 0.5 && fog_policy != 4u) {
        let eye_z = -(view.view_from_world * vec4<f32>(in.world_position.xyz, 1.0)).z;
        let denom = max(fog_span.y - fog_span.x, 0.001);
        let factor = clamp((fog_span.y - eye_z) / denom, 0.0, 1.0);
        var fog_rgb = fog_color.xyz;
        if (fog_policy == 1u) { fog_rgb = vec3<f32>(0.0); }
        else if (fog_policy == 2u) { fog_rgb = vec3<f32>(1.0); }
        else if (fog_policy == 3u) { fog_rgb = vec3<f32>(0.50196078); }
        rgb = mix(fog_rgb, rgb, factor);
    }


    var out: FragmentOutput;
    // Raw gamma out (GAMMA LANE, 0161 — the frame's one decode is the FFXGlow combine). Alpha = the
    // faded cutout alpha (tex × fade) for the blend twin; ignored (blend off) on steady/opaque draws.
    // OPAQUE-INTENT alpha pin (clutter_fade.z bit 3, set in model_render for steady opaque/alpha-key
    // batches): their output alpha is spec-meaningless — opaque/mask pipelines ignore it; only a blend
    // pipeline would read it, and none should ever be bound for them. Pinning it to 1.0 is therefore a
    // no-op under correct pipeline state, and armor under the observed multi-view pipeline mixup
    // (macOS/Metal: with an extra camera, some opaque WMO/M2 draws intermittently bind a blending
    // pipeline and bleed the BLP's garbage alpha — the "pale film on buildings"). Fade twins, genuine
    // Blend batches (glass), and additive glow cards keep their real alpha.
    let opaque_intent = (u32(m.clutter_fade.z) & 8u) != 0u;
    // ADDITIVE batches (glow cards — model_flags.w == 2.0): fold the alpha weight into the colour
    // HERE, in gamma space, exactly as the reference's byte pipeline weights its source term
    // (src_g·α added in bytes). The old hardware `SrcAlpha` blend multiplied AFTER the linear
    // conversion — α^(1/2.2) inflation that fattened every soft halo into a hard disc (the
    // director's brazier, decision 0160). The pipeline blend state is now a pure (ONE, ONE) add.
    // The additive marker is clutter_fade.z BIT 2 (the same word specialize keys on — NOT
    // model_flags.w, whose stale "== 2.0" comment caused the flat-square regression the director
    // caught: the gate never fired while the blend state had already become a pure add).
    let is_additive = (u32(m.clutter_fade.z) & 4u) != 0u;
    var out_rgb = rgb;
    if (is_additive) {
        out_rgb = out_rgb * faded_alpha;
    }
    // MULTIPLY batches (Mod bit 7 / Mod2x bit 8 — the ARMORREFLECT sheen family, 0528): their
    // blend equation reads no source alpha, so the instance fade cannot ride the alpha channel —
    // the reference genuinely pops these at full strength through every instanceAlpha ramp
    // (byte-confirmed for items: wow-re `m2-item-texture-fill.md` — the type-3 stage is never
    // bound and draws the flat primary colour, ~white, so Mod2x is a full-strength ×2 layer
    // mid-fade). benilla fades them anyway, a deliberate deviation (decision 0865): lerp the
    // source colour toward the blend IDENTITY — Mod: white (src·dst = dst); Mod2x: 0.5 grey
    // (2·0.5·dst = dst) — by the instance fade `obj_fade` (never the texture alpha). At
    // obj_fade 1 this is exact parity with the steady look; at 0 the layer contributes nothing,
    // so the sheen rides the appear/despawn/stealth/distance ramps like every other batch
    // instead of popping over a body that hasn't faded in.
    let is_mod = (u32(m.clutter_fade.z) & 128u) != 0u;
    let is_mod2x = (u32(m.clutter_fade.z) & 256u) != 0u;
    if (is_mod || is_mod2x) {
        let identity = select(vec3<f32>(1.0), vec3<f32>(0.5), is_mod2x);
        out_rgb = mix(identity, out_rgb, obj_fade);
    }
    // GAMMA LANE (0161): raw gamma out — blending (alpha AND additive) happens in gamma like
    // the reference's byte framebuffer; the frame decodes once in the FFXGlow combine. (The old
    // `lin` A/B emitted linear for the sRGB encode — subsumed by the lane.)
    out.color = vec4<f32>(out_rgb, select(faded_alpha, 1.0, opaque_intent));
    return out;
}
