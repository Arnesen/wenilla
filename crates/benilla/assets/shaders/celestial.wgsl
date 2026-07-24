// Celestial DISCS (the sun, the white moon, moon02) and their additive lens-flare GLARES — unlit,
// gamma-correct billboards drawn over the sky dome. Discs get a world-space HORIZON CLIP + the
// reference's PER-VERTEX ALPHA FADE, so a setting body melts into the horizon as a whole-disc gradient;
// glares (mode `celestial.z` = 1) skip the clip — the reference's flare draw `0x7e57e0` never calls the
// clip `0x6d1960`; the lens-flare envelope gates them — and ADD in gamma instead of blending.
//
// HORIZON (RE: 0x6d1960, wow-re celestial-bodies note Addendum #7 / decisions 0485+0523+0529 — VERIFIED
// off the binary): every disc routes through the same clip+fade — the quad is clipped to the horizon
// plane (height 0; new crossing vertices get alpha 0, a fully-below body is skipped), and the per-vertex
// fade store is CONDITIONAL (guard `0x6d1ac5`): only vertices inside the 0.4-unit near-horizon band get
// `alpha = clamp(2.5·height, 0, 1)`; every vertex ABOVE the band keeps the disc COLOUR's own alpha byte
// — 0xFF for the sun/white moon (the per-frame diffuse broadcast), 0 for moon02 (its colour dword has no
// writer), which is why the reference never shows a second moon: the draw runs, the blender paints
// nothing. The rasterizer interpolates those vertex alphas, so a straddling sun grades 0-at-the-cut →
// full-at-the-top across the WHOLE disc. We reconstruct exactly that per-fragment, in distance-invariant
// sin-elevation terms (`k = 2.5·12 = 30` — the binary's band at its radius-12 disc) from the quad's
// per-frame `span`; the earlier per-fragment band approximation (0485, then 0524's workaround for the
// bar it caused) is superseded by decision 0529.
//
// GAMMA-SPACE BLEND — the gamma composite lane (decision 0161): the reference's framebuffer holds gamma
// bytes and ALL its hardware blending happens in gamma space, so the faithful sky-body blend is a
// GAMMA-space one. We emit a **premultiplied GAMMA** colour under `AlphaMode::Premultiplied`:
// `out = (disc_gamma · a, a)` — the disc blends over the (gamma) dome exactly like the reference, letting the
// blue night-dome bleed through the moon's feathered edges + horizon fade (the teal "halo"). Bevy hands us
// `base.rgb` already LINEARISED (sRGB texture × linear base_color), so we round-trip `linear_to_srgb` to
// recover the gamma disc colour first — the one conversion left in this shader.
//
// GAMMA-SPACE ADD (glare mode, `celestial.z` = 1; decision 0502): the reference's lens flares blend
// SRC_ALPHA, ONE onto the gamma framebuffer (`0x7e5a16`) — the flare's gamma texel × the intensity byte is
// ADDED to the scene's gamma bytes, saturating per channel (a warm disc + a warm flare clip to WHITE at
// the core — the blown-out reference sun). A plain Bevy `AlphaMode::Add` adds LINEAR values, which
// under-weights every mid-tone and can never saturate — the "too yellow" sun (the residual 0485/0500
// recorded). Here: `out = (glare_gamma · a, 0.0)` under `AlphaMode::Add`'s (ONE, 1−src_alpha) blend
// ⇒ `dst + gamma·a` — the reference's byte addition, with `a` = the lens-flare intensity envelope.
//
// SKY-PASS DEPTH (celestial-frame-anatomy pin; the law + its history in `sky_order.rs`): the reference
// draws its whole sky in a squashed back depth slice with the depth TEST on — the glare last of all,
// further back still (`[0.995, 1.0]`) — so the z-buffer occludes every sky element per-pixel behind
// everything the world drew (a ridge, a wall, one leaf), while the sky itself (which writes no depth)
// never blocks it. Same mechanism here: the quads' geometry stays on their shells (12 units for the
// glare — its pinned screen footprint — and `far·0.85` for the discs), but EVERY fragment FORCES its
// depth to 0.0 — reverse-Z "infinitely far" — under Bevy's GreaterEqual test: it passes only where the
// depth buffer still holds the clear value, i.e. where no opaque geometry drew. Discs used to pass
// their own rasterized depth through, on the assumption that their shell sits beyond all world
// geometry; the WDL horizon ring reaches past it, so a distant hill lost to a disc it should occlude.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::alpha_discard,
    forward_io::VertexOutput,
    mesh_view_bindings::view,
}

// `forward_io::FragmentOutput` + the forced-depth builtin: every sky fragment writes the far depth.
struct CelestialOutput {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

/// Reverse-Z "infinitely far" — the sky pass's forced depth (`sky_order.rs`).
const SKY_FAR_DEPTH: f32 = 0.0;

// Per-material control (set in `sun/materials.rs` `CelestialExt`). `.x` = the horizon alpha-ramp scale `k`
// (`k = 30.0`: the binary's 0.4-unit band at its radius-12 disc, in sin-elevation terms — 0x6d1960).
// `.y` = brightness multiplier — 1.0 everywhere (the old moon HDR boost died with
// decision 0163; the moon's glow is FFXGlow's blur² on the disc's own bytes). `.z` = mode: 0 = disc
// (horizon clip + premultiplied gamma blend), 1 = additive glare (no clip, gamma add). `.w` = the disc
// COLOUR's own alpha byte (1.0 sun/white moon, 0.0 moon02 — its dword has no writer; Addendum #7).
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> celestial: vec4<f32>;
// The disc quad's vertical span in sin-elevation: `.x` = bottom edge, `.y` = top edge (written per
// frame by the follow systems; unused in glare mode). The reference fade is PER-VERTEX (0x6d1960
// stores alpha on the quad's vertices — conditionally, guard 0x6d1ac5 — and the rasterizer
// interpolates), so the fragment reconstructs that interpolation from the span (decision 0529).
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var<uniform> span: vec4<f32>;

// The reference's per-VERTEX alpha rule at sin-elevation `y` (0x6d1960 + guard 0x6d1ac5): inside
// the near-horizon band (`k·y < 1`) the vertex takes the fade ramp; above it it keeps the disc
// colour's own alpha byte (`celestial.w`).
fn vertex_alpha(y: f32) -> f32 {
    let ramp = y * celestial.x;
    return select(celestial.w, clamp(ramp, 0.0, 1.0), ramp < 1.0);
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> CelestialOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    let base = alpha_discard(pbr_input.material, pbr_input.material.base_color);

    var a = base.a;
    if celestial.z < 0.5 {
        // DISC: horizon clip + the reference's PER-VERTEX fade, reconstructed (0x6d1960 +
        // Addendum #7, decision 0529). The binary clips the quad at height 0 (new crossing
        // vertices, alpha exactly 0 — a fully-below body is skipped) and stores the band ramp on
        // its vertices; the rasterizer interpolates. A setting sun is therefore a whole-disc
        // gradient melting to 0 at the horizon cut — not a solid disc with a thin faded strip
        // (the old per-fragment band, 0485/0524, sighted twice: moon02's black bar and the
        // too-hard sunset). Here: interpolate the vertex rule linearly between the quad's
        // clipped bottom edge and its top edge (`span`, sin-elevation — camera-facing quad, so
        // the vertical axis maps to elevation), clip below the horizon. Elevated bodies
        // (`k·span.x ≥ 1`) reduce to `a × celestial.w` exactly — the fixtures' regime. Glares
        // skip all of it (0x7e57e0 never calls the clip; the lens-flare envelope gates them).
        // The reference also alpha-tests GEQUAL 1/255; under our premultiplied blend a 0-alpha
        // fragment already contributes nothing, so no discard.
        let dir = normalize(in.world_position.xyz - view.world_position.xyz);
        let y = dir.y;
        if y <= 0.0 {
            a = 0.0;
        } else {
            let y_bot = max(span.x, 0.0); // the horizon cut, or the bottom edge above it
            let a_bot = vertex_alpha(y_bot);
            let a_top = vertex_alpha(span.y);
            let t = clamp((y - y_bot) / max(span.y - y_bot, 1e-6), 0.0, 1.0);
            a *= mix(a_bot, a_top, t);
        }
    }

    // GAMMA output — see the header. `base.rgb` is linear (Bevy); recover the gamma colour,
    // premultiply by `a` in gamma space, emit raw onto the gamma buffer (0161). `celestial.y` =
    // brightness (1.0 everywhere; multiplies the RGB only, so the blend's dst term is unaffected).
    let gamma = linear_to_srgb(base.rgb);
    var out: CelestialOutput;
    if celestial.z >= 0.5 {
        // ADDITIVE GLARE: (rgb·a, alpha 0) under (ONE, 1−src_alpha) ⇒ `dst + gamma·a` — the
        // reference's SRC_ALPHA, ONE byte addition, `a` = the lens-flare intensity envelope.
        out.color = vec4<f32>(gamma * a * celestial.y, 0.0);
    } else {
        // DISC: premultiplied gamma blend (AlphaMode::Premultiplied).
        out.color = vec4<f32>(gamma * a * celestial.y, a);
    }
    // Forced far depth for BOTH (see the header): a sky element survives only on pixels no opaque
    // geometry claimed — the reference's back-slice depth test.
    out.depth = SKY_FAR_DEPTH;
    return out;
}
