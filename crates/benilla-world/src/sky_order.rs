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
//! Bevy instead sorts every transparent by `view-z of the mesh center + depth_bias`, drawn
//! back-to-front — and our celestial entities are camera-anchored, so their raw distances are
//! sort-order accidents (the camera-centred cloud dome sat at ~0 = drawn LAST, painting clouds
//! over rain and over the glare — backwards on both counts). These biases turn the accident back
//! into the reference's fixed order: spaced far below any real in-scene view-z (≳ −far plane, a
//! few ×10³) so world distances can never reorder the sky and every world transparent draws over
//! it, with the glare *above* zero so it draws after the world and the rain — but still below
//! [`Rung::NAMEPLATE`], which keeps world text on top of the flare (the
//! reference draws its text later still).
//!
//! Precipitation deliberately has NO bias: an unbiased view-z of a few tens of units lands rain
//! after the biased sky and before the glare — the reference's `weather` slot — without pinning
//! rain-vs-world-transparent order we have no byte law for.
//!
//! ## Which way the bias points — the sign, read off Bevy 0.18 (never guessed again)
//!
//! Every rung below is a *signed* number in one direction, and the direction was inverted here for
//! a week (decision 0639 — the whole ladder ran upside down; names sorted UNDER the water that
//! erased them). Verified in the bevy 0.18.1 sources, so the next edit can check rather than
//! recall:
//!
//! - `ViewRangefinder3d::distance` (`bevy_render/src/render_phase/rangefinder.rs`) returns the
//!   **view-space z** of the point — *negative* in front of the camera, and MORE negative the
//!   farther away (its own unit test puts points behind the eye at `+1`/`+2`).
//! - `Transparent3d` sorts **ascending** on that value (`bevy_core_pipeline/src/core_3d/mod.rs`:
//!   "Values increase towards the camera. Back-to-front ordering for transparent means we need an
//!   ascending sort") — so the first item drawn is the most negative = the farthest.
//! - The queue adds the material's bias to it (`bevy_pbr/src/material.rs`,
//!   `RenderPhaseType::Transparent`: `rangefinder.distance(&center) + depth_bias`).
//!
//! ⇒ **A POSITIVE bias draws LATER (on top); a NEGATIVE bias draws EARLIER (behind).** Bevy's own
//! `StandardMaterial::depth_bias` doc says it in words: "a positive depth bias will render closer
//! to the camera while negative values cause the material to render behind other objects".
//!
//! The magnitude is not free, because the same field is *also* the rasterizer's depth bias
//! (`bevy_pbr/src/pbr_material.rs` packs it into the pipeline key → `DepthBiasState::constant`,
//! applied in depth ULPs ≈ 2⁻²³ *relative*, the same knob [`crate::target::ring`] uses at 8192 to
//! beat coplanar noise). It is harmless for the sky rungs (every sky fragment overwrites its own
//! depth, below) but it perturbs the depth TEST of anything depth-tested, so the world-side rungs
//! stay as small as their ordering job allows.
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

/// Stars — the first celestial draw (`0x6d4a3f`): everything else in the sky paints over them, so
/// theirs is the ladder's LOWEST rung (see the sign law above).
pub(crate) const STARS_BIAS: f32 = -1.0e6;
/// **The WMO skybox** ([`crate::skybox`]) — the building-owned painted sky, which replaces this
/// whole ladder while it draws, so its rung never competes with one above. It needs a rung at all
/// because a skybox is **an ordinary M2 and not an opaque backdrop**, which is what this module
/// asserted until decision 1264: `CavernsOfTimeSky.m2` is 21 batches across four blend modes — a
/// painted cube, six additive star sheets on the cube's own faces, five alpha-blended planet cards
/// and three alpha-tested asteroid belts on rotating bones. Drawn opaque (the old lane read
/// positions, UVs and one texture and nothing else) the additive star sheet — near-white RGB whose
/// stars live in its ALPHA — paints a flat white sheet over the painted sky. That is the director's
/// "the whole ceiling is white" in Caverns of Time.
///
/// The blended half of a skybox therefore lands in the transparent pass, camera-anchored, so its
/// view-z is a few tens of yards and it would sort *after* the world. This rung sinks it under
/// every world transparent: the deepest of those is [`FAR_SIDE_BIAS`] plus a far-plane view-z,
/// ≈ −4.3e4, and the shell's own view-z rides on top of the rung — all four properties of the band
/// are machine-checked in `model_render`'s
/// `the_skybox_band_orders_its_batches_on_one_pipeline_key`. Kept as small as that job allows (the
/// magnitude rationale above), because the rung is also what the batch-order epsilon has to
/// survive: see `model_render::SKYBOX_ORDER_EPS`.
pub(crate) const WMO_SKYBOX_BIAS: f32 = -6.0e4;
/// The sun disc — second (`0x7e5b90` via `0x6d4a47`).
pub(crate) const SUN_DISC_BIAS: f32 = -8.2e5;
/// The white moon — third; where the discs cross, the moon paints over the sun.
pub(crate) const WHITE_MOON_BIAS: f32 = -8.1e5;
/// moon02 — fourth (invisible in clear weather; ordered for its weather-seed surfacing).
pub(crate) const MOON02_BIAS: f32 = -8.0e5;
/// The cloud dome — last of the sky pass (`0x6d4a71`): clouds blend over a setting sun.
pub(crate) const CLOUDS_BIAS: f32 = -6.0e5;
/// **Far-side model transparents** — the water-plane interleave's early half (byte-VERIFIED,
/// wow-re `water-frame-straddle.md`): the reference splits M2 transparents into an above-water
/// and a below-water list per model (per **emitter** for particles — each classifies onto exactly
/// ONE list, `0x7084a0`), and `0x483460` draws the list on the eye's FAR side of the water plane
/// *before* the water pass, the near side *after*. A draw classified far-side takes this rung —
/// an effect plus its owner-last rung (capped at 32), a translucent M2 mesh batch plus its batch
/// eps (≤ ~65 × 1e-3, `model_render::BATCH_ORDER_SORT_EPS`), a zfill twin minus its 8 — so it
/// lands under [`WATER_BIAS`] with the owner-last law intact inside the band; near-side draws
/// keep their natural slot above it. The flip on submersion is the interleave inversion
/// (`0x4836d6 cmp eax,0xf`). Sort-only on every lane: the effect stream splits sort from raster
/// by construction, and the mesh lane's far twin zeroes the raster constant back in
/// `WowModelExt::specialize` (`clutter_fade.z` bit 11).
pub(crate) const FAR_SIDE_BIAS: f32 = -4.0e4;
/// **The water surface** — river/ocean/WMO water draw in a fixed frame slot between the two
/// transparent halves (`0x6701d0 → 0x6816d0`: ocean → river → WMO liquid → foam), never view-z
/// sorted against model transparents. One rung below the world band puts every unclassified
/// world transparent after the water (the reference's near-side default) and the far-side
/// effects before it. Magnitude: must clear the far plane (a few ×10³) so no world view-z can
/// reorder a water chunk across it, while staying small as a rasterizer bias (this one rides a
/// `StandardMaterial` base, so it IS also a depth-test bias — ~0.24% depth pull, half of what
/// the nameplates ship at +4e4; the water-noon/name-water capture diffs are the shoreline check).
pub(crate) const WATER_BIAS: f32 = -2.0e4;
/// The sun/moon glare quads — the frame's last render (`0x483740` tail): over the clouds and the
/// rain, under the nameplates; the z-buffer (their forced far depth, `celestial.wgsl`) is what
/// occludes them. Only ~6× the far plane, not ~10⁵: a rung this far up is also a rasterizer bias
/// (the sign law above), and nothing is gained by making it bigger than the ordering needs.
pub(crate) const GLARE_BIAS: f32 = 2.0e4;

/// **The world-side rungs** — the biases lanes *outside* this module apply to their own draw, and
/// the reason they are here rather than each next to its lane: they are one ladder. 1163 found the
/// four of them scattered across `blob_shadow`, `ground_fx`, `nameplates` and `target::ring`, each
/// documenting itself by pointing at one of the others ("the same constant, for the same reason,
/// as the ring's"), which is a ladder held together by prose. The compile-time assert below can
/// only check an order it can see.
///
/// Named as one type so a lane reaches the ladder, not a constant: `Rung::GROUND_FX`.
pub struct Rung;

impl Rung {
    /// The **surface-decal** rung — sort bias AND rasterizer depth-bias constant, one number for
    /// both roles. Projected decal vertices are geometrically coplanar with the drawn ground, so
    /// without a bias the depth test dissolves into per-pixel f32 noise (stipple, view-dependent
    /// bites); 8192 · 2⁻²³ pushes their depth ~1e-3·distance toward the camera — far above the
    /// coplanar noise floor (an ulp of Elwynn's 10⁴-yard coords), far below anything visible.
    /// Geometric lifts (0.1, 0.02) read as hovering at grazing angles — director-confirmed. The
    /// reference fights the same coplanarity with polygon offset (trace-verified), the
    /// fixed-function twin of this bias.
    pub const GROUND_FX: f32 = 8192.0;
    /// The **selection ring**, the same rung and the same rationale: ties against the ground-fx
    /// decal do not matter — all of these write no depth, so the bias only ever competes with the
    /// opaque ground.
    pub const RING: f32 = 8192.0;
    /// The **blob shadow**'s sort rung: half the ring's, so where both decals stack the ring draws
    /// later deterministically.
    pub const SHADOW_SORT: f32 = 4096.0;
    /// The blob shadow's **rasterizer** margin, sized independently of its sort rung (B131 split
    /// one constant into its two roles). It funds the `GreaterEqual` tie against the drawn ground
    /// (`DECAL_WORLD_CLIP`, 0781). The reference's mechanism is the same class: `shadowBias` cvar
    /// 0.1 → a constant-only polygon offset (−102.4 LSBs of its 24-bit buffer; no slope term
    /// exists in the binary — wow-re unit-blob-shadow RE, corroborated across every shadow draw of
    /// four apitraces). The *size* is ours: our residual is 0781's — the decal's CPU-baked world
    /// verts vs the receiver's GPU-transformed verts diverge by ~1–3 ulps of the world coordinate,
    /// millimetres at city magnitudes, landing on the depth tie where the receiver is *sloped*
    /// (the confirmed B131 flicker walk was the Stormwind gate ramp). 4096 absorbed under half the
    /// 3-ulp worst case at close zoom and lost the tie while moving; 32768 dominates it ≥2× at
    /// every zoom ≥1 yd and stays centimetre-order at 30 yd. Sizing pinned by
    /// `raised_bias_dominates_the_bake_residual` (particles/render.rs).
    pub const SHADOW_RASTER: i32 = 32768;
    /// **World text** — above the celestial glare, so a flare never washes a nameplate, and above
    /// every sky rung. The reference draws its world text late in the frame (decision 0519).
    /// Small on purpose: 6× the far plane is all the ordering needs, and this same field doubles
    /// as the rasterizer depth bias on a layer that is depth-TESTED (walls must keep occluding
    /// names). The sign was inverted here once and the water erased the glyphs (decision 0639).
    pub const NAMEPLATE: f32 = 4.0e4;
}

/// The ladder IS the reference order — checked at compile time: monotonic through the sky pass,
/// every sky rung below the world (so world transparents draw over it) and below the world-side
/// decal rung, then rain (unbiased view-z, ≈ 0), the glare above it, and the nameplates above the
/// glare so text stays readable through a flare. Rung gaps stay wider than any view-z that could
/// reorder them: > 10⁴ inside the sky (camera-anchored shells spread ±far·0.85 ≈ 2.6e3), and
/// > 10⁴ on the world side (> the far plane, a few ×10³).
const _: () = {
    assert!(SUN_DISC_BIAS - STARS_BIAS > 1.0e4);
    assert!(WHITE_MOON_BIAS > SUN_DISC_BIAS && MOON02_BIAS > WHITE_MOON_BIAS);
    assert!(CLOUDS_BIAS - MOON02_BIAS > 1.0e4);
    // The water-plane interleave: sky < far-side transparents < water < world transparents (the
    // near-side default). The far-side band is FAR_SIDE_BIAS − 8 (a zfill twin) … + owner rung;
    // owner rungs are capped well under the 1e4 margin (benilla_formats::owner_last_rung's
    // ceiling) and the mesh batch eps under that.
    assert!(FAR_SIDE_BIAS - CLOUDS_BIAS > 1.0e4);
    assert!(WATER_BIAS - FAR_SIDE_BIAS > 1.0e4);
    assert!(-3.0e3 - WATER_BIAS > 1.0e4); // world view-z floor = −far (the ~3 km projection) stays above
    assert!(Rung::GROUND_FX - CLOUDS_BIAS > 1.0e4);
    assert!(GLARE_BIAS - Rung::GROUND_FX > 1.0e4);
    assert!(Rung::NAMEPLATE - GLARE_BIAS > 1.0e4);
    // The two decal rungs stack deterministically where both draw, and the shadow's raster margin
    // is its own axis (B131) — not comparable to the sort rungs, only to zero.
    assert!(Rung::RING > Rung::SHADOW_SORT && Rung::SHADOW_RASTER > 0);
};

/// The depth law (module doc) is a property of the **shaders**, so it is checked there: every sky
/// fragment shader must force `SKY_FAR_DEPTH`. Without this, a shell radius silently becomes
/// load-bearing again the moment someone edits one of them — the exact regression 0588 fixed, and
/// one that only shows up at night, on a mountainous horizon, past 2.6 km.
#[test]
fn every_sky_shader_forces_the_far_depth() {
    for (name, src) in [
        ("sky.wgsl", include_str!("shaders/sky.wgsl")),
        ("star.wgsl", include_str!("shaders/star.wgsl")),
        ("cloud.wgsl", include_str!("shaders/cloud.wgsl")),
        ("celestial.wgsl", include_str!("shaders/celestial.wgsl")),
        // The WMO skybox is no longer a shader of its own: it draws on the shared model lane, whose
        // `WOW_SKY_DEPTH` branch obeys the same law and is asserted beside it
        // (`benilla_assets::materials`, `the_sky_lane_forces_the_far_depth`).
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
