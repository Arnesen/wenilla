// Liquid shader — a port of the reference's **ADT** water path (`ocean0_s.bls`) to WGSL.
//
// ## SCOPE — this file implements ONE of the reference's three liquid renderers
//
// This is the ADT MCLQ river/ocean path and nothing else. It is also, today, what benilla runs on
// WMO-embedded liquid, and that is **wrong** — see `liquid/surface.rs`'s `spawn_wmo_liquids` and the
// `wmo_liquid_arms` census. The reference has three:
//
//   * **ADT MCLQ river/ocean** — passes `0x6851b0`/`0x685010`. Two texture stages: a static depth-ramp
//     on stage 0, the animated sheet on stage 1, combined by the pixel program `ocean0_s.bls`. The
//     only liquid path with a depth ramp, and the only one whose vertex carries NO colour. **This
//     file.**
//   * **WMO MLIQ water** — `0x6b62e0` category 0, split again on the group's `MOGP.flags & 0x48`:
//     exterior `0x6b6630` (binds `MapObjExtWater0.bls`), interior `0x6b6420` (no shader, lighting
//     forced off). One texture stage, no depth ramp at all — zero references to the ADT ramp globals
//     `0xc7fbc0`/`0xc81768`/`0xc7fcd8` anywhere in `[0x6b0000, 0x6c4000)` — and alpha comes from a
//     per-vertex authored byte, not from a depth swatch. 164 groups; 90% of them interior.
//   * **magma/slime** — `0x6b68f0` (WMO) / `0x68dca0` (ADT), arm-blind, the sheet IS the body.
//
// ## The ADT combine (VERIFIED — the program is an asset, extracted and read)
//
// `Shaders\Pixel\ocean0_s.bls` out of `patch.MPQ`, verbatim ARB:
//
//   PARAM c[1] = { { 0.25 } };
//   TEX R0, fragment.texcoord[0], texture[0], 2D;   # colorTex  = the depth ramp
//   TEX R1, fragment.texcoord[1], texture[1], 2D;   # detailTex = the animated sheet
//   MAD R1.xyz, fragment.color.primary, R0, R1;
//   ADD R0.xyz, fragment.color.secondary, c[0].x;
//   MAD result.color.xyz, R0, R1.w, R1;
//   MOV result.color.w, R0;                          # R0.w still holds colorTex.a
//
//   ⇒ rgb = primary*colorTex.rgb + detailTex.rgb + (secondary + 0.25)*detailTex.a
//     alpha = colorTex.a
//
// The `+0.25` is the program's own scalar `PARAM`, not an FFP env colour (`glTexEnvfv` has zero call
// sites image-wide) and not a material. **The formula this file has always carried is right, verbatim
// — but its provenance was fiction**: the header used to cite "apitrace WoW.17 program 159" and a
// `docs/knowledge/terrain.md`, neither of which exists, and wow-re had the program attributed to a
// *character* draw (`Model2.bls` ships 32 ARBfp permutations, none containing `0.25` or
// `fragment.color.secondary`). Corrected and recorded: wow-re `terrain/scratch/water-shading-law.md`.
//
//   * colorTex (unit 0) is the depth swatch — a 2-endpoint linear lerp of the zone's dedicated
//     `Light.dbc` water rows, RAW (no ×0.711): `water_shallow.rgb` = IntBand row 16 (river/lake) / 14
//     (ocean), `water_deep.rgb` = row 17 / 15. Rebuilt **per world frame**, not baked once: the dirty
//     flags `[0xc8117c]`/`[0xc81b70]` clear at `0x680b90`/`0x680b97` and refill via `0x58acd0`, so the
//     colour and the opacity track the zone and the clock. (The earlier "reflected sky × 0.711 via
//     `FUN_0068c250`" model fingered the WRONG builder — a separate grey edge texture never bound on
//     the water unit. Rows 14–17 were right all along.)
//   * detailTex (unit 1) is the animated `lake_a`/`ocean_h` frame: RGB near-black, ALPHA = the ripple.
//     MEASURED off the shipped BLPs (DXT3, 9 authored mips): lake_a RGB mean 0.0140 and achromatic to
//     ±1 LSB, alpha mean 0.21 / p50 51 / p99 255. So it adds a faint flat lift + an achromatic shimmer
//     on the crests — NOT the body. The authored mip chain deliberately kills the shimmer with
//     distance (per-mip alpha max 255, 255, 255, 136, 68, then flat 51), which is why the sampler's
//     mips and 16× aniso are load-bearing rather than a nicety.
//   * primary = the vertex's lit colour `clamp(ambient + N·L·sun)`. The ADT liquid vertex has no
//     colour element, so `glColor` is the device default `(1,1,1,1)` tracked into material
//     ambient+diffuse by `glColorMaterial(FRONT_AND_BACK, AMBIENT_AND_DIFFUSE)`, and `GL_LIGHTING` is
//     ON at both water draws (lava explicitly turns it off; water does not).
//   * secondary = the specular sheen — see `sun_sheen` for what is verified in it and what is not.
//   * alpha = swatch.a over the SAME V as the colour. LightParams endpoints: river 0.5→1.0, ocean
//     0.75→1.0. Deeper water = more opaque. **Open**: wow-re reads the byte-verified ADT water alpha
//     as the `0xc7fbc0` LUT's `1.6·(i/63)^8` curve rather than the linear `127+2·row` this file
//     applies — a much later-breaking ramp. Not changed here; it is a look change to every ADT water
//     surface in the game and it belongs in the same A/B as the WMO arms.
//
// **Both `ocean0_s.bls` and `MapObjExtWater0.bls` are CVar-gated** — `specular` and `pixelShaders`
// (registered `0x6886a0`/`0x688712`) default to `"0"`, and with them off there is no program, no
// specular, the stage-1 combine is a plain ADD, and blend is never set so water draws OPAQUE. Our
// reference install's `Config.wtf` sets both to `"1"`, so every capture and every director comparison
// is against the shader leg — which is the leg we implement. The two do not compose: an active ARB
// program bypasses the texture environment entirely.
//
// A SINGLE swatch row (V) indexes both the colour and the alpha — they track together. V is `clamp(byte/42)`
// for river/lake (VERIFIED `c81768`, saturates ~5 yd → the channel middle hits the deep teal row) and
// byte/255 for ocean (placeholder; ocean uses a non-LUT UV path). (Earlier cuts: ripple-as-colour → black;
// ×8 over-saturated; FLAT colour killed the gradient; sky×0.711 was the wrong builder; `byte/255` was the
// wrong LUT → river middle never went teal. Corrected to rows 14–17 raw lerp + the /42 V, 2026-05-31.)
//
// Two-sided comes from the material (cull off) and is right for EVERY kind: all four reference liquid
// passes force GL_CULL_FACE off at pass entry against a cull-ON device baseline, and `glFrontFace` is
// not even imported (VERIFIED wow-re `liquid-render-state-sided` §6). Blending is per KIND, decided in
// liquid.rs: water/ocean blend with depth-write off; magma/slime are opaque and depth-write. Fog + gamma
// mirror terrain.wgsl (planar eye-Z GL_LINEAR fog in gamma space; raw gamma out — GAMMA LANE, 0161), fog
// applies to every kind — magma and slime included — and WHICH fog block a surface takes is per-surface:
// a WMO interior group's own pool fogs with the interior block, everything else with the scene block
// (see `apply_fog`). Light, fog and both water swatches all come off the ONE shared global-light buffer.

#import bevy_pbr::{
    mesh_functions,
    forward_io::Vertex,
    view_transformations::position_world_to_clip,
    mesh_view_bindings::{view, globals},
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var frames: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var frames_samp: sampler;

struct LiquidParams {
    // x = fullbright (magma/slime); y = ocean swatch; z = interior fog; w = sun-sheen shininess.
    kind: vec4<f32>,
    // x = WHICH RENDERER (see `LiquidPath`): 0 = ADT MCLQ, 1 = WMO exterior, 2 = WMO interior.
    // y/z/w reserved.
    path: vec4<f32>,
    // x = reserved (frame 0); y = frame count; z = the SCROLL FLAG (1 only on the nibble-6/7
    // WMO magma/slime lane — the reference's animated stage-0 texture matrix; see
    // `liquid/surface.rs`'s `scrolls`, and `apply_scroll` below); w = the clock enable (0 on a
    // deterministic run — the whole animation freezes at frame 0 / scroll 0, the 0600 capture
    // pin, baked at material build).
    anim: vec4<f32>,
};
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var<uniform> w: LiquidParams;

// The shared global light (`lighting::global_light`) — the SAME buffer terrain and the models read,
// mirrored here as its canonical row prefix. Liquid used to carry its own copy of every one of these
// values, re-pushed per material by `apply_wow_lighting`; that copy is what left it with only the
// scene fog and no way to see the interior block (decision 0691).
struct WowLight {
    light_ambient: vec4<f32>,      // 0  rgb = ambient; w = Mod2x scale
    light_diffuse: vec4<f32>,      // 1  rgb = sun diffuse; w = clamp flag
    light_sun: vec4<f32>,          // 2  xyz = sun TRAVEL dir (to-light = −xyz)
    light_spec: vec4<f32>,         // 3  rgb = row-9 specular colour; w = TERRAIN shininess (liquid uses w.kind.w)
    fog_color: vec4<f32>,          // 4  rgb = scene fog (block 1, gamma 0..1); w = enable (>0.5)
    fog_params: vec4<f32>,         // 5  x = start yd; y = end yd; w = the farclip wall
    _sh: array<vec4<f32>, 6>,      // 6-11  model SH coeffs — unread here
    _sh_c16: vec4<f32>,            // 12
    water_river: array<vec4<f32>, 2>, // 13-14 shallow/deep river-lake swatch (IntBand 16/17); w = alpha
    water_ocean: array<vec4<f32>, 2>, // 15-16 shallow/deep ocean swatch    (IntBand 14/15); w = alpha
    _grade: vec4<f32>,             // 17
    wmo_fog_color: vec4<f32>,      // 18 rgb = INTERIOR fog (block 2); w = enable
    wmo_fog_params: vec4<f32>,     // 19 x = start yd; y = end yd
};
@group(#{MATERIAL_BIND_GROUP}) @binding(90) var<storage, read> wow_light: WowLight;

struct LiquidVsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) depth: f32,
    // The sun-sheen `secondary` evaluated PER-VERTEX (the faithful 1.12 path: the real client computes
    // the Blinn highlight in its FFP vertex shader and interpolates it across the coarse water mesh).
    // The fragment shader uses this interpolated value directly.
    @location(4) secondary_vtx: vec3<f32>,
    // The mesh's vertex COLOUR, which on a WMO INTERIOR pool carries its `MOMT.diffColor` body
    // colour — the reference's own interior water vertex carries a colour dword for exactly this.
    // White on every other lane (and on any mesh with no colour attribute), where nothing reads it.
    @location(5) vcolor: vec4<f32>,
}

// Sun sheen (`secondary`): a Blinn highlight of the sun on the flat water surface — the glint that's
// strongest at grazing (sunrise/sunset) sun. `secondary = light_spec.rgb · (N·H)^shininess`. Shared by
// both stages so the per-vertex (faithful) and per-pixel (current) paths run IDENTICAL math — only the
// EVALUATION DOMAIN differs (interpolated vs evaluated per fragment).
fn sun_sheen(world_normal: vec3<f32>, world_pos: vec3<f32>) -> vec3<f32> {
    let n = normalize(world_normal);
    let to_light = -normalize(wow_light.light_sun.xyz);
    // LOCAL viewer, not the infinite-viewer constant: the reference sets
    // `GL_LIGHT_MODEL_LOCAL_VIEWER = 1` at `0x59cf89`, so `H = normalize(L + normalize(eye − vertex))`
    // and the eye vector is recomputed per vertex (VERIFIED wow-re `water-shading-law.md`). It is
    // the difference between a highlight and a flood: with the infinite viewer `N·H` is very nearly
    // CONSTANT over a flat water plane, so at high sun the whole sheet saturates at once instead of
    // carrying a glint that moves with the camera.
    let to_view = normalize(view.world_position.xyz - world_pos);
    let half_v = normalize(to_light + to_view);
    let ndoth = max(dot(n, half_v), 0.0);
    // The reference's specular is additionally gated on `N·L > 0` (the fixed-function rule: a light
    // behind the surface contributes no specular). **Deliberately not ported**, because it cannot
    // fire here: `DayNight::SetDirection` holds the LIGHTING sun's azimuth constant and wobbles its
    // elevation only between +20° and +37° — it is always above the horizon, night being the colours
    // going dark rather than the sun setting (`lighting::daynight::sun_direction`, and the separate
    // *visible* sun is the one that rises and sets). Against a liquid surface's flat up normal `N·L`
    // is therefore positive at every minute of the day, so the gate is a branch that can only ever
    // take one arm. Named rather than added: it is real, it is verified, and it is inapplicable.
    //
    // Shininess is WATER's own (`w.kind.w`), not the shared row-3 terrain exponent. Material
    // specular is white (`SetRenderState(3, 0xffffffff)`) and the exponent is 6.0 (`[0x8102e8]`),
    // both VERIFIED — so `light_spec.rgb` is the whole scale, and it is the one input here that is
    // still INFERRED: wow-re pinned the mechanism (`CGLight+0x48` → `collector+0x6c` → `0x589d80(0)`
    // → `glLightfv(GL_SPECULAR)`) but not the number, and reads it as a warm ≈(1.0, 0.91, 0.76)
    // against the row-9 feed we use. Left on row 9 until that lands rather than swapped for an
    // estimate.
    return wow_light.light_spec.rgb * pow(ndoth, max(w.kind.w, 1.0));
}

// The lava/slime **surface scroll**: the reference's animated stage-0 texture matrix, which for
// liquid-type nibbles 6 and 7 is the identity with element 13 — the **v translate** — set to
// `fmod(uptime_s, 10.0) · 0.1` (VERIFIED wow-re `liquid-uv-scroll-law.md`, six-agent §5, matrix
// built at `0x6b68f0` and pushed at stage 0 by `0x6b6ae3`). A texture matrix times `(s, t, 0, 1)`
// with only element 13 non-identity is exactly `t += phase`, so the whole mechanism is this add —
// no matrix, no second sampler, no cost on the paths that do not scroll (`anim.z` is a hard 0
// there, which the CPU side guarantees rather than the shader branching on it).
//
// A full repeat every 10 s, so `REPEAT` wrapping makes the sawtooth's reset invisible. Only the
// rate and the period are reproducible — the reference's phase comes off `GetTickCount`, i.e. the
// machine's uptime, so its absolute value is not a thing to match.
// The liquid clock: `globals.time` (the same wall-elapsed seconds the CPU cycler used to read)
// under the build-time enable. Both animations below are pure functions of it — the CPU-side
// 24 Hz `Assets::get_mut` tick this replaces mutated ~14 materials a tick and its Modified
// fallout (uniform re-uploads, bind-group rebuilds, whole-population `AssetChanged` arming)
// measured 0.28 cpu_ms/frame at the SW pin (2026-08-18 bracket).
fn anim_time() -> f32 {
    return w.anim.w * globals.time;
}

// The 24 fps frame flip — 30 frames over 1.25 s (VERIFIED `FUN_0068aac0`), floor-quantized to
// the tick exactly as the reference's integer frame index is.
fn frame_layer() -> i32 {
    return i32(floor(anim_time() * 24.0) % max(w.anim.y, 1.0));
}

fn apply_scroll(uv: vec2<f32>) -> vec2<f32> {
    // v += (t mod 10) · 0.1 — repeats/s `[0x801620]` = 0.1, period `[0x80e5a0]` = 10.0 (VERIFIED
    // wow-re `liquid-uv-scroll-law.md` §5): a sawtooth over exactly one repeat, invisible under
    // REPEAT wrapping. CONTINUOUS now, where the CPU tick quantized it to 1/24 s — the reference
    // itself rebuilds the matrix per draw off a millisecond clock, so this is the more faithful
    // reading, not a new liberty. anim.z is the flag: hard 0 on every non-scrolling lane.
    return vec2<f32>(uv.x, uv.y + w.anim.z * fract(anim_time() / 10.0));
}

// Distance fog — planar eye-Z, GL_LINEAR, gamma space (mirrors terrain.wgsl). Applied to EVERY liquid
// kind, because the reference never disables fog for a liquid batch: the device default for GL_FOG is
// **ON** (`0x593bf0` writes state id `0x0f` = 1) and all 42 fog-enable setters in the binary are
// Push/Pop-scoped, so what a batch inherits at its draw is that default. The ADT lava pass sets only
// cull/lighting/blend (`0x6855ca`/`0x6855d6`/`0x6855e2`) and the WMO magma/slime arm sets only lighting
// (`0x6b6afe`) — neither touches fog — while the WMO *river* arm goes out of its way to re-assert
// `(0x0f, 1)`, which only makes sense in a world where fog-on is liquid's intended state. (VERIFIED
// wow-re `liquid-render-state-sided` §1–§3, §5.)
//
// WHICH fog is a per-surface choice, and it is the reference's own (VERIFIED wow-re `fog-env-state`
// §5, the complete 6-site submit census). The device holds two fog blocks: **block 1** (`+0x70/74/78`)
// is the scene fog, submitted once a frame from `WorldFrame::Render` (`0x66ff20`), and **block 2**
// (`+0x80/84/88`) is block 1 smoothed toward the MFOG/zone target over ~4 s (`0x6cf054`+) — the
// interior haze. Only two call sites in the whole binary re-submit block 2, and they are the WMO
// *geometry* pass (`0x6b51d9`/`0x6b51ea`) and the WMO *liquid* pass (`0x6b6323`–`0x6b6342`), both under
// the same `[0xca7f00]` gate. So an interior room's pool takes the room's fog, in lockstep with the
// walls around it; ADT liquid submits nothing and draws under the scene block. `w.kind.z` is that
// gate, resolved at spawn from the group's `MOGI & 0x48` — the same interior test `wow_model.wgsl`
// uses, which is what keeps the two in step.
fn apply_fog(rgb: vec3<f32>, world_pos: vec3<f32>) -> vec3<f32> {
    var fog_color = wow_light.fog_color;
    var fog_span = wow_light.fog_params.xy;
    if (w.kind.z > 0.5) {
        fog_color = wow_light.wmo_fog_color;
        fog_span = wow_light.wmo_fog_params.xy;
    }
    if (fog_color.w <= 0.5) {
        return rgb;
    }
    let eye_z = -(view.view_from_world * vec4<f32>(world_pos, 1.0)).z;
    let denom = max(fog_span.y - fog_span.x, 0.001);
    let factor = clamp((fog_span.y - eye_z) / denom, 0.0, 1.0);
    return mix(fog_color.xyz, rgb, factor);
}

@vertex
fn vertex(in: Vertex) -> LiquidVsOut {
    var out: LiquidVsOut;
    let world_from_local = mesh_functions::get_world_from_local(in.instance_index);
    out.world_position =
        mesh_functions::mesh_position_local_to_world(world_from_local, vec4<f32>(in.position, 1.0));
    out.clip_position = position_world_to_clip(out.world_position.xyz);
    out.world_normal = mesh_functions::mesh_normal_local_to_world(in.normal, in.instance_index);
    out.uv = in.uv;
#ifdef VERTEX_COLORS
    out.vcolor = in.color;
#else
    out.vcolor = vec4<f32>(1.0);
#endif
    // Per-vertex MCLQ depth (0..1) packed into UV1.x; drives the opacity ramp.
    out.depth = in.uv_b.x;
    // The faithful per-vertex sun sheen — interpolated across the coarse mesh by the fragment stage.
    out.secondary_vtx = sun_sheen(out.world_normal, out.world_position.xyz);
    return out;
}

@fragment
fn fragment(in: LiquidVsOut) -> @location(0) vec4<f32> {
    // HARD FAR-CLIP WALL (same as terrain/models, see terrain.wgsl): discard water beyond the
    // projection far plane so lakes/rivers don't render past the wall. `fog_params.w` = farclip
    // (0 ⇒ disabled).
    if (wow_light.fog_params.w > 0.0) {
        let clip_z = -(view.view_from_world * vec4<f32>(in.world_position.xyz, 1.0)).z;
        if (clip_z > wow_light.fog_params.w) {
            discard;
        }
    }

    // Animated frame. For water/ocean this is the DETAIL ripple (RGB ≈ near-black, ALPHA = ripple);
    // for magma/slime it is the OPAQUE BODY texture.
    let detail = textureSample(frames, frames_samp, apply_scroll(in.uv), frame_layer());

    // Magma / slime (kind.x > 0.5): the animated texture IS the opaque body colour — no depth swatch
    // (the ADT liquid vertex format carries no colour element at all, and the WMO one is a hard
    // `0xffffffff`, so there is nothing to modulate the sheet by) and no N·L (lighting state 0 on both
    // paths). It IS fogged, like every other liquid batch.
    //
    // The earlier "emissive / no-darken / no fog" reading here was WRONG, and wrong twice over: it came
    // from the ADT-lava row of `rf-water-liquid-type-texture-material`, which read GX state `0x37` as an
    // emissive path when `0x37` is the per-stage TEXTURE-MATRIX enable pushing an identity — a texgen
    // *reset*; and that row is the ADT queue, which never dispatches slime at all (Undercity's slime is
    // WMO liquid). Skipping fog is what made a submerged slime surface a flat unshaded sheet at any
    // depth instead of one that recedes into the murk. (VERIFIED wow-re `liquid-render-state-sided`
    // §3/§3.1/§5, which corrects that row.)
    if (w.kind.x > 0.5) {
        return vec4<f32>(apply_fog(detail.rgb, in.world_position.xyz), 1.0);
    }

    // Per-vertex swatch coord V (in `in.depth`, computed CPU-side in wow-formats/liquid.rs): river/lake
    // = `clamp(byte/42)` (VERIFIED WoW.exe `c81768` LUT / `FUN_0068d790`, saturating ~5 yd so the channel
    // middle reaches the deep/teal row), ocean = byte/255 (placeholder, different path). The depth swatch
    // is a plain 2-endpoint lerp (`FUN_0068a830`), so a SINGLE V indexes BOTH the colour and the alpha
    // row: colour `shallow→deep` and opacity `shallow_α→deep_α` track together. (Earlier `×4` colour
    // compression + the gentle `byte/255` V were band-aids for a wrong "V tops at 0.31" belief — removed.)
    let depth = clamp(in.depth, 0.0, 1.0);
    // The kind's swatch endpoints, off the shared light: ocean reads rows 15/16 (IntBand 14/15),
    // river/lake rows 13/14 (IntBand 16/17). Both are packed every frame by `build_light_data`.
    var shallow = wow_light.water_river[0];
    var deep = wow_light.water_river[1];
    if (w.kind.y > 0.5) {
        shallow = wow_light.water_ocean[0];
        deep = wow_light.water_ocean[1];
    }
    let water_tint = mix(shallow.rgb, deep.rgb, depth);

    // ---- The two WMO water arms ------------------------------------------------------------
    //
    // Neither is the ADT combine below. `0x6b62e0`'s category 0 splits on the owning group's
    // `MOGP.flags & 0x48`, and both halves bind ONE texture (the animated sheet) with no depth ramp
    // anywhere — there is not a single reference to the ADT ramp globals `0xc7fbc0`/`0xc81768`/
    // `0xc7fcd8` in all of `[0x6b0000, 0x6c4000)`. Opacity on both is the per-vertex authored byte
    // through the zone's linear alpha ramp, which `in.depth` carries and this lerp reproduces
    // (`wmo_water_alpha_v`). VERIFIED wow-re `terrain/scratch/water-shading-law.md` §11.
    let vtx_alpha = mix(shallow.w, deep.w, depth);
    if (w.path.x > 1.5) {
        // ---- WMO INTERIOR (`0x6b6420`) — 134 of the game's 164 water groups, Blackfathom included.
        //
        // Fixed-function, always: the kernel body contains no `mov ecx,0x3f` at all (a positive
        // finding — the same scan on the exterior kernel finds two), and `[0xc9607c]`, the
        // specular/pixelShaders gate, is never read on this path. So it ignores those CVars, runs
        // with lighting OFF (`0x0e = 0`) and fog ON (`0x0f = 1`), and its whole output is the
        // combine preset `(0x1f, 3)` over one texture stage:
        //
        //     rgb = clamp(Cf + Ct)      alpha = clamp(Af + At)
        //
        // `Cf` is the pool's `MOMT[materialId].diffColor` taken RAW — baked into the mesh's vertex
        // colour, which is where the reference's own 6-float vertex carries it. NO sun term and no
        // sheen: that vertex has no normal to compute one from.
        //
        // The ALPHA op is the part worth stating. Preset 3 is `GL_ADD` on both channels via
        // `GL_COMBINE` (`COMBINE_ALPHA = GL_ADD` @`0x85c2fc`, operands left at the GL default), NOT
        // the legacy `GL_TEXTURE_ENV_MODE = GL_ADD` whose alpha would be `Af · At`. The client takes
        // the COMBINE path because `GL_ARB_texture_env_combine` is GL 1.3 core. So the ripple **adds**
        // to the pool's opacity instead of multiplying it away — a pool is at its authored opacity in
        // the troughs and saturates opaque on the crests, which is the opposite of what the legacy
        // reading would have drawn.
        let body = clamp(in.vcolor.rgb + detail.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
        return vec4<f32>(
            apply_fog(body, in.world_position.xyz),
            clamp(vtx_alpha + detail.a, 0.0, 1.0),
        );
    }
    if (w.path.x > 0.5) {
        // ---- WMO EXTERIOR (`0x6b6630`) — Stormwind's canals and fountains.
        //
        // This arm DOES bind a pixel program, `Shaders\Pixel\MapObjExtWater0.bls` (bound at
        // `0x6b6654`, unbound `0x6b689c`), under the `[0xc9607c]` specular/pixelShaders gate. Both
        // CVars default to "0", but our reference install's `Config.wtf` sets both to "1", so the
        // shader leg is what every director comparison is against and it is the leg we implement.
        // Decoded verbatim from the asset:
        //
        //     rgb = primary.rgb + detail.rgb + secondary·detail.a      alpha = primary.a
        //
        // **No `+0.25`.** That constant is the ADT program's own `PARAM` and has no counterpart
        // here, so carrying it over — which is what we did — added a flat achromatic lift to every
        // canal pixel at all times, sun or no sun. Against the sheet's real texel distribution that
        // is ~3.5x the ripple contrast off the glint, and it is why the reference's canal shimmers
        // only where the sun is while ours sparkled everywhere.
        //
        // `primary` is FFP-lit over a constant up-normal with the vertex colour tracked into
        // ambient+diffuse (`glColorMaterial(GL_FRONT_AND_BACK, GL_AMBIENT_AND_DIFFUSE)`), so the band
        // is the MATERIAL colour and `primary = band · clamp(ambient + diffuse·max(N·L, 0))`.
        // Lighting really is on here: neither this kernel nor its dispatch touches render-state id
        // `0x0e`, and the control that such a call would be findable is the interior kernel, which
        // does exactly that at `0x6b65bf`.
        //
        // The band is a SINGLE one — there is no bathymetry to lerp by — and it is the **deep** river
        // row, `LightIntBand` sub-17, i.e. `water_river[1]`. Read as a hard immediate
        // (`0x6b66be add edi, 0xec`), so nibbles 0, 4 and 8 all take it; exterior ocean cannot arise
        // (category 2 falls to a bare epilogue with no draw).
        //
        // **Sub-17, not sub-16, and the distinction cost a round trip.** A DayNight band *slot* is not
        // a `LightIntBand` *sub*: `0x6d64d0` displaces sub-8 out to record `+0x4c`, so `sub = slot + 1`
        // across slots 8–16, and the kernel's slot 16 is sub-17. The shipped data says the same thing
        // twice over — across all 367 LightParams rows carrying river bands, sub-16 is browns, olives
        // and muddy yellows (G > B in 71%: the shallow colour of water over a riverbed) while sub-17 is
        // blues and teals. At Stormwind sub-16 is `(79, 93, 20)`, which renders the canals olive-green;
        // sub-17 is `(51, 82, 85)`. The ADT ramp runs sub-16 → sub-17 across its 64 texels; WMO
        // exterior water takes the deep end alone, flat.
        let n_ext = normalize(in.world_normal);
        let to_light_ext = -normalize(wow_light.light_sun.xyz);
        let primary_ext = clamp(
            wow_light.light_ambient.rgb + wow_light.light_diffuse.rgb
                * max(dot(n_ext, to_light_ext), 0.0),
            vec3<f32>(0.0),
            vec3<f32>(1.0),
        ) * deep.rgb;
        let rgb_ext = primary_ext + detail.rgb + in.secondary_vtx * detail.a;
        // `result.color.w = fragment.color.primary` — the vertex alpha ALONE. The bound program
        // bypasses the texture environment entirely, so the interior arm's `+ At` does not apply here.
        return vec4<f32>(apply_fog(rgb_ext, in.world_position.xyz), vtx_alpha);
    }

    // Body colour: lit vertex colour × the depth-lerped water-row swatch colour (`primary·colorTex`).
    let n = normalize(in.world_normal);
    let to_light = -normalize(wow_light.light_sun.xyz);
    let ndotl = max(dot(n, to_light), 0.0);
    let primary = clamp(
        wow_light.light_ambient.rgb + wow_light.light_diffuse.rgb * ndotl,
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );

    // Sun sheen (`secondary`): the `ocean0_s.bls` Blinn highlight, computed PER-VERTEX in `fn vertex`
    // and interpolated across the coarse ~4 yd MCLQ mesh — the faithful 1.12 path (the real client
    // evaluates it in its FFP vertex stage). Per-pixel evaluation of the sharply-peaked `pow(N·H,6)`
    // would fill its broad lobe at full value (a brighter, denser sheen); interpolating from the
    // vertices flattens the peak to match the reference. (A per-pixel/per-vertex A/B toggle proved the
    // two visually identical on our mesh — we keep per-vertex as the faithful mechanism; RE:
    // `docs/knowledge/scratch/liquid-depth/fleck-deep.md`.)
    let secondary = in.secondary_vtx;

    // primary·colorTex.rgb  +  detail.rgb  +  (secondary + 0.25)·detail.a   (the ocean0_s.bls math)
    var rgb = primary * water_tint + detail.rgb + (secondary + vec3<f32>(0.25)) * detail.a;

    // Opacity: depth ramp between the shallow/deep LightParams water alphas, over the SAME V as the
    // colour. Deeper = more opaque, up to α=1.0 where V saturates (river/lake byte 42 ≈ 5 yd), so the
    // channel middle is opaque + teal while the shore stays semi-transparent (V→0, α≈0.5) and the bottom
    // shows through (faithful — the pale edge band). One steep V drives both colour and opacity together.
    let alpha = mix(shallow.w, deep.w, depth);

    // Distance fog (see `apply_fog`) — the water fog colour is also teal, so far water converges on the
    // haze.
    rgb = apply_fog(rgb, in.world_position.xyz);

    // GAMMA LANE (0161): raw gamma out; alpha blends in gamma like the reference's bytes.
    return vec4<f32>(rgb, alpha);
}
