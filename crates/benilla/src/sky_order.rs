//! The celestial draw-order ladder — the reference's **fixed sky pass order**, expressed as
//! `Transparent3d` sort biases (wow-re `celestial-frame-anatomy`, §5-verified off the binary).
//!
//! The real client draws the frame `sky → opaque world → weather → glare`, and inside its sky pass
//! (`CSky::Render 0x6d4940`, one squashed depth slice `[0.975, 0.98]`, depth-write off, painter's
//! order): **stars → sun disc → white moon → moon02 → gradient strip (additive) → cloud dome** —
//! so the clouds composite over a setting sun, rain falls in front of the whole sky, and the glare
//! quads (their own back slice `[0.995, 1.0]`, the frame's LAST draw) paint over everything the
//! z-buffer leaves visible.
//!
//! Bevy instead sorts every transparent by `view-z of the entity translation + depth_bias`, drawn
//! back-to-front — and our celestial entities are camera-anchored, so their raw distances are
//! sort-order accidents (the camera-centred cloud dome sat at ~0 = drawn LAST, painting clouds
//! over rain and over the glare — backwards on both counts). These biases turn the accident back
//! into the reference's fixed order: spaced far above any real in-scene view-z (≲ the far plane,
//! a few ×10³) so world distances can never reorder the sky, with the glare below zero so it draws
//! after every world transparent — but still above [`crate::nameplates::NAMEPLATE_DEPTH_BIAS`],
//! which keeps world text on top of the flare (the reference draws its text later still).
//!
//! Precipitation deliberately has NO bias: an unbiased view-z of a few tens of units lands rain
//! after the biased sky and before the glare — the reference's `weather` slot — without pinning
//! rain-vs-world-transparent order we have no byte law for.
//!
//! ## The depth law — every sky fragment forces the far depth
//!
//! The biases above order the sky *against itself*. What orders it against the **world** is depth,
//! and there the reference gives one rule for the whole pass: the sky draws FIRST, in a squashed
//! back slice (`[0.975, 0.98]`, the glare further back at `[0.995, 1.0]`), depth-write off — so the
//! opaque world simply paints over it, and no sky element can ever land in front of world geometry.
//!
//! Our sky draws *after* the world (Bevy's transparent pass), so the depth **test** has to do that
//! job — which it only does if the sky's depth is behind everything. Each shell used to rely on its
//! own camera-anchored radius for that (`far·0.85` discs, `far·0.87` clouds, `far·0.88` stars,
//! `far·0.9` gradient dome), on the assumption that world geometry is always nearer. **It isn't:**
//! the WDL horizon ring ([`crate::wdl`]) streams ±5 tiles ≈ 2.9 km and is drawn out to the far plane
//! (3 km), so distant hills land in a band *behind* every shell — and stars, clouds and discs then
//! passed the depth test in front of terrain the reference would have occluded them with (the
//! sighting: stars showing *through* a fogged mountain range at night, decision 0588).
//!
//! So every sky fragment now writes `SKY_FAR_DEPTH = 0.0` — reverse-Z "infinitely far" — under
//! Bevy's `GreaterEqual` test (`sky.wgsl`, `star.wgsl`, `cloud.wgsl`, `celestial.wgsl`, which
//! already did it for the glare). A sky element survives only where the depth buffer still holds
//! the clear value: exactly "the world paints over the sky", independent of any shell radius. The
//! shells now decide only *screen size and sky-internal parallax*, never occlusion.

/// Stars — the first celestial draw (`0x6d4a3f`): everything else in the sky paints over them.
pub(crate) const STARS_BIAS: f32 = 1.0e6;
/// The sun disc — second (`0x7e5b90` via `0x6d4a47`).
pub(crate) const SUN_DISC_BIAS: f32 = 8.2e5;
/// The white moon — third; where the discs cross, the moon paints over the sun.
pub(crate) const WHITE_MOON_BIAS: f32 = 8.1e5;
/// moon02 — fourth (invisible in clear weather; ordered for its weather-seed surfacing).
pub(crate) const MOON02_BIAS: f32 = 8.0e5;
/// The cloud dome — last of the sky pass (`0x6d4a71`): clouds blend over a setting sun.
pub(crate) const CLOUDS_BIAS: f32 = 6.0e5;
/// The sun/moon glare quads — the frame's last render (`0x483740` tail): over the clouds and the
/// rain, under the nameplates; the z-buffer (their forced far depth, `celestial.wgsl`) is what
/// occludes them.
pub(crate) const GLARE_BIAS: f32 = -5.0e4;

/// The ladder IS the reference order — checked at compile time: monotonic through the sky pass,
/// with rain (unbiased, view-z ≥ 0) between clouds and glare, the largest world-side decal bias
/// far below the sky rungs, the glare above the nameplates so text stays readable through a
/// flare, and rung gaps wide enough (> 10⁴ ≥ any real view-z at far ≲ 10⁴) that in-scene
/// distances can never climb past them.
const _: () = {
    assert!(STARS_BIAS - SUN_DISC_BIAS > 1.0e4);
    assert!(SUN_DISC_BIAS > WHITE_MOON_BIAS && WHITE_MOON_BIAS > MOON02_BIAS);
    assert!(MOON02_BIAS - CLOUDS_BIAS > 1.0e4);
    assert!(CLOUDS_BIAS - crate::ground_fx::GROUND_FX_DEPTH_BIAS > 1.0e4);
    assert!(GLARE_BIAS < -1.0e4);
    assert!(GLARE_BIAS > crate::nameplates::NAMEPLATE_DEPTH_BIAS + 1.0e4);
};

/// The depth law (module doc) is a property of the **shaders**, so it is checked there: every sky
/// fragment shader must force `SKY_FAR_DEPTH`. Without this, a shell radius silently becomes
/// load-bearing again the moment someone edits one of them — the exact regression 0588 fixed, and
/// one that only shows up at night, on a mountainous horizon, past 2.6 km.
#[test]
fn every_sky_shader_forces_the_far_depth() {
    for (name, src) in [
        ("sky.wgsl", include_str!("../assets/shaders/sky.wgsl")),
        ("star.wgsl", include_str!("../assets/shaders/star.wgsl")),
        ("cloud.wgsl", include_str!("../assets/shaders/cloud.wgsl")),
        (
            "celestial.wgsl",
            include_str!("../assets/shaders/celestial.wgsl"),
        ),
    ] {
        assert!(
            src.contains("const SKY_FAR_DEPTH: f32 = 0.0;"),
            "{name}: the sky pass's forced-far-depth constant is gone"
        );
        assert!(
            src.contains("out.depth = SKY_FAR_DEPTH;"),
            "{name}: a sky fragment no longer forces the far depth — its shell radius is deciding \
             occlusion again (sky_order.rs, \"The depth law\")"
        );
    }
}
