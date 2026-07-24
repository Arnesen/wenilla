// Sky-dome gradient — unlit. Paired with `sky.rs` (SkyExt). The five Light.dbc SkyColor stops
// (zenith→horizon) + the fog colour (row 7) are interpolated PER-FRAGMENT by the view-direction
// elevation. RE-VERIFIED mapping (WoW.exe FUN_006d0d10/FUN_006d0f50 + apitrace WoW.5 dome vertices):
// one stop per ring at GEOMETRIC elevations above the horizon —
//   SkyColor0 @ 90° (zenith) … SkyColor1 @ 16.8° … SkyColor2 @ 9.8° … SkyColor3 @ 3.7° …
//   SkyColor4 @ 1.8° … and the horizon (0°) and below = the FOG colour (row 7).
// So SkyColor0 dominates the whole upper sky; the gradient is crushed into the bottom ~17° and
// converges into the distance fog at the horizon. Linear interp in elevation ≈ WoW's ring Gouraud.
// SKY-PASS DEPTH (see `sky_order.rs`, "The depth law"): the dome's radius does not decide occlusion —
// the reference draws the sky FIRST and the opaque world paints over it. Forcing the far depth makes
// the dome a true backdrop: it fills only pixels no world geometry claimed, so a WDL hill drawn beyond
// the dome's shell can never be over-painted by the gradient.
#import bevy_pbr::{
    forward_io::VertexOutput,
    mesh_view_bindings::view,
}

/// Reverse-Z "infinitely far" — the sky pass's forced depth (`sky_order.rs`).
const SKY_FAR_DEPTH: f32 = 0.0;

struct SkyOutput {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
}

struct SkyColors {
    sky0: vec4<f32>, // zenith (90°)
    sky1: vec4<f32>, // 16.8°
    sky2: vec4<f32>, // 9.8°
    sky3: vec4<f32>, // 3.7°
    sky4: vec4<f32>, // 1.8°
    fog: vec4<f32>,  // horizon (0°) and below — LightIntBand row 7
    warp: vec4<f32>, // x = dawn/dusk warp strength S (0 = off), y = sun azimuth (rad), zw reserved
};
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> sky: SkyColors;

// Sun-relative azimuth phase → glow factor `g` (`FUN_006d0f50` table `0xce9af8`, VERIFIED byte-exact off
// its writer `FUN_006ce210`). 6 wrap-around keyframes, piecewise-linear like the engine's table sampler,
// symmetric about phase 0.625. **Semantics (capture-reconciled — see the warp note below):** `g = +1.0`
// (phase 0.125) is the IDENTITY case `(1−g)=0` → ring left untouched, and that phase lands on the SUN
// bearing → the sun side is the bright, unwarped DBC stop. `g < 0` (trough −0.7 at phase 0.625) is the
// ANTI-sun side → it pulls the ring toward the dark apex. `phase` is fract()'d into [0,1).
fn azimuth_glow(phase: f32) -> f32 {
    let p = fract(phase);
    if (p < 0.125) { return mix(0.0, 1.0, (p + 0.125) / 0.25); } // wrap 0.875(g0)→1.125(g1)
    else if (p < 0.375) { return mix(1.0, 0.0, (p - 0.125) / 0.25); }
    else if (p < 0.5) { return mix(0.0, -0.5, (p - 0.375) / 0.125); }
    else if (p < 0.625) { return mix(-0.5, -0.7, (p - 0.5) / 0.125); }
    else if (p < 0.75) { return mix(-0.7, -0.5, (p - 0.625) / 0.125); }
    else if (p < 0.875) { return mix(-0.5, 0.0, (p - 0.75) / 0.125); }
    else { return mix(0.0, 1.0, (p - 0.875) / 0.25); } // wrap 0.875(g0)→1.125(g1)
}

// One mid-ring's warped colour for a single glow factor `g` (`FUN_006d0f50`, VERIFIED + dusk-capture
// reconciled). `g ≥ 0`: desaturate toward SkyColor1 by `(1−g)·S²` (so g=+1 on the sun bearing = identity =
// the bright raw stop). `g < 0` (anti-sun): a prepass toward SkyColor1 by `S`, then toward the apex
// SkyColor0 by `0.7·(−g)·S²` — the darkest, desaturated away side. (`S²` = the prepass-S × per-segment-S
// nesting; `0.7` is the binary const `0x7ffd7c`. All byte-verified; reproduces the WoW.10 dusk dome to
// ~2/channel.)
fn warp_one(base: vec3<f32>, g: f32, s: f32) -> vec3<f32> {
    let s2 = s * s;
    if (g >= 0.0) {
        return mix(base, sky.sky1.rgb, (1.0 - g) * s2);
    }
    return mix(mix(base, sky.sky1.rgb, s), sky.sky0.rgb, 0.7 * (-g) * s2);
}

@fragment
fn fragment(in: VertexOutput) -> SkyOutput {
    // Direction from the camera to this dome fragment; elevation above the world horizon.
    let dir = normalize(in.world_position.xyz - view.world_position.xyz);
    let elev = degrees(asin(clamp(dir.y, -1.0, 1.0))); // −90..90, 0 = horizon

    // Dawn/dusk azimuthal warp (FUN_006d0f50) — only when S>0 (sunrise ~06:30 / sunset ~21:30 in
    // highlightSky=1 zones; the S-curve decays to 0 by ~midnight). RE-VERIFIED + reconciled against the
    // WoW.10 dusk capture. Two corrections over the old per-fragment version:
    //  (1) Warp ONLY the 4 mid rings (SkyColor1..4); the apex (SkyColor0) and the fog rim are UNWARPED.
    //      The engine's warp loop touches only ring offsets 4/8/12/16, and the capture shows the apex+rim
    //      uniform across azimuth. So we warp the mid-ring COLOURS here, then run the elevation gradient
    //      with the warped mid-rings + the *unwarped* apex/fog — the upper sky (>16.8°) interpolates the
    //      warped ring1 up to the unwarped apex, GL-Gouraud-faithful. (The old `ring_weight` smoothstep
    //      warped the whole upper sky → at night it dragged the entire dome toward the black apex.)
    //  (2) The lobe bearing: `+0.125` puts the SUN bearing at glow phase 0.125 (g=+1 → identity → the
    //      bright raw stop) and the anti-sun at 0.625 (g=−0.7 → darkest). The old `+0.625` had it 180°
    //      inverted — darkening the sun side and leaving the anti-sun bright. The warp does NOT brighten:
    //      the sunset glow IS the unwarped sun-side stop; the warp only darkens/desaturates the rest.
    // Per-vertex Gouraud: the engine bakes the warp at 24 azimuth segments and interpolates between, which
    // smears the genuine g=0 discontinuity (away `(1−g)·S²` vs warm prepass `S`) across one 15° segment
    // rather than the hard fragment edge a smooth eval would give. We emulate that — evaluate the warp at
    // the two bracketing segment phases and lerp.
    var s1 = sky.sky1.rgb;
    var s2c = sky.sky2.rgb;
    var s3 = sky.sky3.rgb;
    var s4 = sky.sky4.rgb;
    let warp_s = sky.warp.x;
    if (warp_s > 0.0) {
        let az = fract((atan2(dir.z, dir.x) - sky.warp.y) / 6.2831853 + 0.125);
        let seg = az * 24.0; // 24 azimuth segments, like the dome vertices
        let s0 = floor(seg);
        let f = seg - s0;
        let g0 = azimuth_glow(s0 / 24.0);
        let g1 = azimuth_glow((s0 + 1.0) / 24.0);
        s1 = mix(warp_one(sky.sky1.rgb, g0, warp_s), warp_one(sky.sky1.rgb, g1, warp_s), f);
        s2c = mix(warp_one(sky.sky2.rgb, g0, warp_s), warp_one(sky.sky2.rgb, g1, warp_s), f);
        s3 = mix(warp_one(sky.sky3.rgb, g0, warp_s), warp_one(sky.sky3.rgb, g1, warp_s), f);
        s4 = mix(warp_one(sky.sky4.rgb, g0, warp_s), warp_one(sky.sky4.rgb, g1, warp_s), f);
    }

    // Elevation gradient: warped mid-rings + UNWARPED apex (SkyColor0) and fog rim.
    var col: vec3<f32>;
    if (elev <= 0.0) {
        col = sky.fog.rgb; // horizon and below = fog colour (row 7), unwarped
    } else if (elev < 1.8) {
        col = mix(sky.fog.rgb, s4, elev / 1.8);
    } else if (elev < 3.7) {
        col = mix(s4, s3, (elev - 1.8) / (3.7 - 1.8));
    } else if (elev < 9.8) {
        col = mix(s3, s2c, (elev - 3.7) / (9.8 - 3.7));
    } else if (elev < 16.8) {
        col = mix(s2c, s1, (elev - 9.8) / (16.8 - 9.8));
    } else {
        col = mix(s1, sky.sky0.rgb, (elev - 16.8) / (90.0 - 16.8)); // warped ring1 → UNWARPED apex
    }

    // Raw gamma out (GAMMA LANE, 0161): the reference draws the sky with GL_FRAMEBUFFER_SRGB OFF
    // (CSky::Render @0x6d4940) — raw DBC bytes — which is now the whole pipeline's convention.
    let rgb = col;
    var out: SkyOutput;
    out.color = vec4<f32>(rgb, 1.0);
    out.depth = SKY_FAR_DEPTH;
    return out;
}
