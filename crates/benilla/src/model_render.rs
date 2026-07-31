//! Shared world-model material build — the `WowModelMaterial` (a `StandardMaterial` base carrying the
//! texture/alpha/cull + the `WowModelExt` shared-light extension that does the WoW shading) used for
//! every M2/WMO instance: doodads, WMO buildings, creatures, and GameObjects. Deduped per owner by
//! `(texture, blend, sidedness, kind, fade-variant)` so instances sharing a look batch into one draw.

use benilla_formats::{ModelBlend, WmoBatchClass};
use bevy::asset::AssetId;
use bevy::pbr::ExtendedMaterial;
use bevy::prelude::*;
use bevy::render::render_resource::{Buffer, Face};

use crate::terrain::{WowModelExt, WowModelMaterial};

/// Alpha-test reference for `Blend_AlphaKey` (M2 blend mode 1) materials — the value below which a
/// fragment is discarded (`D3DCMP_GREATEREQUAL`). Per wowdev.wiki M2/Rendering this is **version
/// dependent**: `224/255 ≈ 0.878` on **≤ WotLK** (our target is 1.12.1 build 5875), vs `128/255 ≈
/// 0.5` on Cata+. We initially hardcoded the Cata value (0.5), which left foliage too dense and a
/// white fringe on cutout edges. The spec multiplies this by the element's animated alpha (1.0 for
/// static doodads), so the bare constant is correct until we add doodad alpha fades.
/// Source: <https://wowdev.wiki/M2/Rendering> § Alpha Testing.
pub const VANILLA_ALPHA_KEY_REF: f32 = 224.0 / 255.0;

/// Material-dedup key: same texture + blend + sidedness + kind + fade-variant → one shared material.
#[derive(PartialEq, Eq, Hash)]
pub(crate) struct MatKey {
    texture: Option<AssetId<Image>>,
    blend: ModelBlend,
    two_sided: bool,
    is_wmo: bool,
    is_interior: bool,
    is_emissive: bool,
    is_additive: bool,
    fade_variant: bool,
    /// M2 render flag 0x10 / 0x08 — disable depth write / depth test (`specialize` honours them).
    no_depth_write: bool,
    no_depth_test: bool,
    /// The batch's fog COLOUR policy ([`benilla_formats::FogPolicy`]) — packed into `clutter_fade.z`
    /// bits 4-6 (`wow_model.wgsl`'s step-5 fog).
    fog_policy: benilla_formats::FogPolicy,
    /// The batch's static terrain-shade selector ([`ShadeSel`]). Static per placement, so it dedups a
    /// lit / matte / shaded material variant.
    shade: ShadeSel,
    /// WMO authored batch index + 1 (0 = non-WMO). Per-batch, so each biased batch is its own
    /// pipeline variant — see `terrain::WowModelKey::wmo_batch_order` for the why (the byte-verified
    /// MOBA draw-order determinism, wow-5875-re models/scratch/wmo-batch-blend-depth-state.md).
    wmo_batch_order: u16,
    /// The batch's UV-animation identity (decision 0130 phase 3): the `Arc<UvAnim>` pointer, so
    /// batches scrolling on different loops never share a material (their `sun_scale.zw` offsets
    /// diverge every frame) while every instance of the same model batch does. Sound because the
    /// `Arc` lives in the loaded model asset — one allocation per model per batch, stable while
    /// loaded. `None` = static UVs (the overwhelming majority).
    uv_anim: Option<usize>,
    /// The batch's animated-RGB-tint identity: the `Arc<RgbAnim>` pointer (same soundness argument
    /// as [`Self::uv_anim`]), so batches tinting on different tracks never share a material while
    /// every instance of the same model batch does. `None` = a static tint (the vertex bake).
    rgb_anim: Option<usize>,
    /// The WMO batch's MOBA section (`None` for M2): an interior group's INT and TRANS batches take
    /// different lighting lanes (`tint.w`), so they must never dedupe onto one material.
    wmo_class: Option<WmoBatchClass>,
    /// The WMO MOMT SIDN night-glow colour (RGB gamma bytes; `None` = no SIDN / M2) — part of the
    /// material identity so glass authored with different glow colours never dedupes together.
    sidn: Option<[u8; 3]>,
    /// The WMO MOMT WINDOW flag — an interior-group batch on the brighter midpoint light.
    window: bool,
    /// This material is a **depth-prime twin** ([`zfill_material`] — the reference's `M2UseZFill`
    /// clone command, wow-re `m2-blend-promotion-zfill.md` §4): colour writes masked off, blend off,
    /// z-write on, drawn before its model's colour batches. Its own key axis so a twin can never
    /// dedupe onto a colour material.
    zfill: bool,
}

/// The per-material static terrain-shade **selector** baked into `sun_scale.x` — NOT an intensity
/// itself. `wow_model.wgsl` thresholds the selector (≥0.85 / ≥0.5) into the byte-true INTENSITY
/// family (decision 0354: 2.5 lit / 1.0 day-night / 0.5 MCSH-shadowed — `[node+0xa4]`) scaling the
/// global SH eval. It is the static half of the verified per-category matrix (wow-re
/// `m2-interior-doodad-base-light` §6/§8/§9, decision 0173) — dynamic entities
/// (units/players/GameObjects) are always [`ShadeSel::Lit`] here and mix toward the shaded
/// intensity per instance via the `MeshTag` shade byte ([`crate::entity_shade`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ShadeSel {
    /// Lit ground, the boosted intensity family (the binary's 2.5): ADT map doodads on unshadowed
    /// ground, and every entity M2.
    ///
    /// Entities still select this, but what it *means* for them changed in 0809. The selector is
    /// only the static half — the per-instance `MeshTag` shade byte carries the rest, and
    /// [`crate::entity_shade`] now pins that byte to the day/night ×1.0 for units and players
    /// (`0x672a20`'s null-node fallback commits with no per-node intensity multiply at all), while
    /// GameObjects keep the real 2.5/0.5 chase. So for an entity this variant reads "on the light-node
    /// path"; the node decides the amplitude.
    Lit,
    /// Lit ground, intensity 1.0: an exterior WMO MODD prop — §8b, byte-verified never to reach
    /// the 2.5 site (a Stormwind street fountain is NOT brightened like an Elwynn tree).
    Matte,
    /// The base sits on MCSH-shadowed terrain: the dim intensity (the binary's 0.5).
    Shaded,
    /// Lit by an **authored M2 light rig** (the glue create booth, decision 0429): the lit value is
    /// the order-2 SH probe in slot 0 of the material's *own* light buffer — the scene's ambient +
    /// directional lights folded by `lighting::prop_probe_coeffs`, the same `Model2.bls` curve the
    /// reference's vertex program runs — plus the per-vertex ≤3-nearest point term (the scene's
    /// authored point lights ride the buffer's point table; a rig material is neither WMO nor
    /// interior, so the vertex stage computes it). The world's sun/intensity machinery never
    /// applies: a glue scene has no day/night, no MCSH, no storm band.
    Rig,
}

impl ShadeSel {
    /// The `sun_scale.x` encoding `wow_model.wgsl` thresholds (≥1.5 ⇒ authored-rig probe;
    /// ≥0.85 ⇒ ADT lit; ≥0.5 ⇒ matte; else shaded).
    pub(crate) fn selector(self) -> f32 {
        match self {
            ShadeSel::Lit => 1.0,
            ShadeSel::Matte => 0.6,
            ShadeSel::Shaded => 0.2,
            ShadeSel::Rig => 2.0,
        }
    }
}

/// A material-dedup cache. Each model-spawning subsystem (terrain doodads/WMOs, streamed entities)
/// keeps its own so its handles drop with it — and, since decision 0793, so its entries expire by
/// **distance** ([`crate::art_scope::SpatialCache`]) instead of living until the next map change. A
/// cache nobody sweeps (the `Local<MaterialCache>`s in the glue booth and the portrait bake) behaves
/// exactly as it did: a plain dedup map.
pub(crate) type MaterialCache = crate::art_scope::SpatialCache<MatKey, Handle<WowModelMaterial>>;

/// Build (or fetch the deduped) [`WowModelMaterial`] for a model batch: a `StandardMaterial` base
/// carrying the texture/alpha/cull, plus the `WowModelExt` shared-light extension. `fade_variant`
/// marks the `AlphaMode::Blend` twin every feather pass rides — the doodad distance fade AND the
/// entity appear/despawn/aura fades (`entities::display` builds its twins with it too); steady
/// materials pass `false`. `specialize` keys depth-write-ON on this bit for the twin.
///
/// A twin builder passes the **SOURCE batch's blend mode** (never `ModelBlend::Blend`): the twin
/// always blends — the reference's promotion is transparent-pass membership, not a per-mode state
/// (`m2-blend-promotion-zfill.md` §1) — but the alpha TEST under promotion keys on the *stored*
/// blend mode (§2): AlphaKey keeps its 224/255 cutout while blending, Opaque runs with no alpha
/// test at all. The source blend is what sets [`TWIN_CUTOUT_MARKER`] right (decision 0842: a twin
/// built as `Blend` cut every texel under 224/255 out of a stealthed Opaque batch, which erased
/// Gressil's blade body and left only its high-alpha rune pattern).
#[allow(clippy::too_many_arguments)]
pub(crate) fn model_material(
    cache: &mut MaterialCache,
    materials: &mut Assets<WowModelMaterial>,
    texture: Option<Handle<Image>>,
    blend: ModelBlend,
    two_sided: bool,
    is_wmo: bool,
    is_interior: bool,
    is_emissive: bool,
    is_additive: bool,
    fade_variant: bool,
    no_depth_write: bool,
    no_depth_test: bool,
    fog_policy: benilla_formats::FogPolicy,
    shade: ShadeSel,
    wmo_batch_order: u16,
    uv_anim: Option<&std::sync::Arc<benilla_formats::UvAnim>>,
    rgb_anim: Option<&std::sync::Arc<benilla_formats::RgbAnim>>,
    // The WMO batch's MOBA section (`None` for M2 batches) — with `is_interior`, it picks the
    // batch's lighting lane (`tint.w`; see `WowModelExt::tint`).
    wmo_class: Option<WmoBatchClass>,
    // The WMO window/glass law (`None`/`false` for M2): the MOMT SIDN night-glow colour and the
    // WINDOW midpoint-light flag (`WowModelExt::sidn`).
    sidn: Option<[u8; 3]>,
    window: bool,
    light: &Buffer,
) -> Handle<WowModelMaterial> {
    let key = MatKey {
        texture: texture.as_ref().map(Handle::id),
        blend,
        two_sided,
        is_wmo,
        is_interior,
        is_emissive,
        is_additive,
        fade_variant,
        no_depth_write,
        no_depth_test,
        fog_policy,
        shade,
        wmo_batch_order,
        uv_anim: uv_anim.map(|a| std::sync::Arc::as_ptr(a) as usize),
        rgb_anim: rgb_anim.map(|a| std::sync::Arc::as_ptr(a) as usize),
        wmo_class,
        sidn,
        window,
        zfill: false,
    };
    if let Some(h) = cache.fetch(&key) {
        return h;
    }
    // Additive batches (M2 glow cards) ADD their colour to the framebuffer — so the warm glow isn't
    // muted by the (cool, at night) background bleeding through. They go in the transparent pass
    // (`AlphaMode::Blend`); the shader folds the radial alpha into the colour IN GAMMA SPACE
    // (decision 0160) and `specialize` overrides the blend STATE to a pure (ONE, ONE) add. The
    // additive marker is `clutter_fade.z` bit 2 — the shader gates the gamma-premultiply on the
    // SAME bit specialize keys on (a stale "model_flags.w == 2.0" claim here once desynced the
    // two: blend went pure-add while the premultiply never fired — the flat-square regression).
    // `WOW_NO_ALPHATEST=1` draws every cutout batch opaque — the A/B for "is the alpha test
    // itself discarding this surface?", which is otherwise indistinguishable in the pixels
    // from the surface losing a depth test or never being submitted. (B38: the flip
    // survives it unchanged, so the cutout is not what removes the awning.) It suppresses the
    // fade twin's cutout marker too, so the A/B answers the same question mid-fade.
    let source_cutout =
        blend == ModelBlend::AlphaTest && std::env::var("WOW_NO_ALPHATEST").is_err();
    let alpha_mode = if is_additive || fade_variant {
        // A fade twin blends whatever its source blend is — the reference's promotion is
        // transparent-pass membership (`m2-blend-promotion-zfill.md` §1); the source blend the
        // caller passed decides only the cutout marker below.
        AlphaMode::Blend
    } else {
        match blend {
            ModelBlend::Opaque => AlphaMode::Opaque,
            ModelBlend::AlphaTest if source_cutout => AlphaMode::Mask(VANILLA_ALPHA_KEY_REF),
            ModelBlend::AlphaTest => AlphaMode::Opaque,
            // Mod/Mod2x ride the transparent pass (they multiply what's already drawn, so the
            // scene under them must exist); `specialize` swaps the actual blend state to the
            // byte-verified multiply factors via the marker bits below (decision 0528).
            ModelBlend::Blend | ModelBlend::Mod | ModelBlend::Mod2x => AlphaMode::Blend,
        }
    };
    // Single-sided unless the M2's 0x04 flag is set (many canopy planes are one-directional).
    let cull_mode = if two_sided { None } else { Some(Face::Back) };
    let base = match texture {
        Some(image) => StandardMaterial {
            base_color_texture: Some(image),
            alpha_mode,
            double_sided: two_sided,
            cull_mode,
            ..default()
        },
        // No resolved texture → the reference DISABLES the texture stage and draws the batch in
        // its flat vertex/material colour — i.e. WHITE modulate, byte-verified for the no-source
        // runtime-texture case (wow-re m2-runtime-texture-null-bind.md, their 914a1abd: slot 0 →
        // glDisable, no default texture, alpha test passes everywhere — the Westfall lamppost's
        // "bulb on" pane). The old muted-brown debug tint was unfaithful. Blend/cull state still
        // honoured — an untextured fade twin must keep its per-instance alpha path.
        None => StandardMaterial {
            base_color: Color::WHITE,
            alpha_mode,
            double_sided: two_sided,
            cull_mode,
            ..default()
        },
    };
    let handle = materials.add(ExtendedMaterial {
        base,
        extension: WowModelExt {
            // `clutter_fade.z` is unread by the shader, so it carries the per-batch pipeline markers
            // `specialize` keys on: bit0 = no-depth-write (M2 0x10), bit1 = no-depth-test (0x08),
            // bit2 = additive blend. (`x`/`y`/`w` stay the ground-clutter distance fade — `0` here.)
            clutter_fade: Vec4::new(
                0.0,
                0.0,
                // Bit 3 = OPAQUE-INTENT: an opaque/alpha-key steady batch whose output alpha is
                // semantically meaningless (opaque/mask pipelines ignore it — only blend pipelines
                // read it). The shader pins such batches' output alpha to 1.0: a spec-level no-op
                // that armors against a multi-view pipeline-state mixup observed on macOS/Metal
                // (opaque WMO/M2 draws intermittently bound with blending enabled when an extra
                // camera exists, bleeding the garbage BLP alpha channel — the "pale film on
                // buildings" regression; full measured chain in the fix commit).
                // Bits 4-6 = the per-batch FOG POLICY (`FogPolicy` discriminant, wow-re
                // rf-weather-emission-timeline ROUND 4): 0 = scene so clutter/water materials —
                // which leave the byte's high bits 0 (see `WorldAssets::model_material`,
                // `water_fx`) — keep ordinary scene fog; 1/2/3 = the additive/Mod/Mod2x BLACK/
                // WHITE/GREY fog colours; 4 = fog disabled outright (render flag 0x02).
                // Bits 7/8 = the MULTIPLY blends (decision 0528): `specialize` swaps the pipeline
                // to the byte-verified factors — Mod DST_COLOR/ZERO, Mod2x DST_COLOR/SRC_COLOR
                // (exact on the 0161 gamma lane: the framebuffer holds gamma, like the reference).
                // Bit 10 = TWIN CUTOUT (decision 0842): this fade twin's SOURCE batch alpha-tests,
                // so the shader re-applies the hard 224/255 cutout on the unfaded alpha (the
                // reference's promoted-AlphaKey ALPHAREF = A×224 — the same fixed silhouette). An
                // Opaque source sets no bit: the reference disables its alpha test outright, steady
                // and promoted alike (`m2-blend-promotion-zfill.md` §2 keys ALPHAREF on the STORED
                // blend mode, mode 0 → ref 0).
                f32::from(
                    u16::from(no_depth_write)
                        | (u16::from(no_depth_test) << 1)
                        | (u16::from(is_additive) << 2)
                        | (u16::from(
                            matches!(blend, ModelBlend::Opaque | ModelBlend::AlphaTest)
                                && !fade_variant
                                && !is_additive,
                        ) << 3)
                        | (u16::from(fog_policy as u8) << 4)
                        | (u16::from(blend == ModelBlend::Mod) << 7)
                        | (u16::from(blend == ModelBlend::Mod2x) << 8)
                        | (u16::from(fade_variant && source_cutout) * TWIN_CUTOUT_MARKER),
                ),
                0.0,
            ),
            // x = WMO (FFP N·L × MOCV, not the M2 SH probe); y = distance-fade blend variant; z = WMO
            // interior group (sun off, baked MOCV carries the room); w = **unlit fullbright** (>0.5 ⇒
            // bypass lighting in wow_model.wgsl): the M2 UNLIT (0x01) flag, or WMO UNLIT on an
            // exterior-group batch (the interior drawer ignores it — section law, `wmo-lit-selector`).
            // Additive is NOT fullbright: the real client *lights* additive batches unless 0x01 is set
            // (wow-re `m2-no-envmap-texgen`), so an un-flagged additive (e.g. ArmorReflect shine) is lit.
            // M2 Mod/Mod2x ARE fullbright regardless of 0x01 — the lighting table
            // `DAT_00811fa8 = {1,1,1,1,1,0,0}` clears GL_LIGHTING for modes 5/6 (wow-re
            // `m2-depth-blend-state`); WMO lighting stays flag-driven only (decision 0528).
            model_flags: Vec4::new(
                if is_wmo { 1.0 } else { 0.0 },
                if fade_variant { 1.0 } else { 0.0 },
                if is_interior { 1.0 } else { 0.0 },
                if is_emissive || (!is_wmo && matches!(blend, ModelBlend::Mod | ModelBlend::Mod2x))
                {
                    1.0
                } else {
                    0.0
                },
            ),
            // x = the static MCSH terrain-shade SELECTOR ([`ShadeSel`]: 1.0 ADT-lit / 0.6 matte /
            // 0.2 shaded; shader thresholds at 0.85 and 0.5) that chooses which live sun LEVEL scales the
            // FFP matte's diffuse term. Static per material (a doodad doesn't move), so it dedups the
            // variants; moving entities are Lit here and mix per instance via the `MeshTag` shade byte.
            // y = the WMO authored batch order, read by `wow_model.wgsl`'s VERTEX stage: it scales
            // clip z by (1 + y·2⁻²³) so a later coplanar batch wins the reverse-Z depth test in any
            // draw order — the byte-verified MOBA draw-order determinism. Uniform data by design:
            // as a `WowModelKey` axis driving a fixed-function depth bias it made every batch index
            // its own PIPELINE, and a first city sight compiled ~3000 of them synchronously on the
            // render thread (decision 0837). `WOW_WMO_BIAS=0` (B38's A/B diagnostic) zeroes it here.
            // zw = the batch's live **UV-animation offset** (decision 0130 phase 3, wow-re
            // `m2-texanim-uv`: the real client adds the sampled translation to the stage UVs —
            // translation is un-pivoted, and no placed doodad uses rotation/scaling). Seeded at
            // t = 0 here; `doodad_anim::tick_anim_materials` re-samples it per drawn frame on the
            // shared clock (frozen in captures).
            sun_scale: {
                let uv0 = uv_anim.map_or([0.0, 0.0], |a| a.sample(0.0));
                let order = if matches!(std::env::var("WOW_WMO_BIAS").as_deref(), Ok("0")) {
                    0.0
                } else {
                    f32::from(wmo_batch_order)
                };
                Vec4::new(shade.selector(), order, uv0[0], uv0[1])
            },
            // The animated M2Color tint's first key (identity white for static batches — their
            // constant tint rides the vertex colours instead). A lane that never re-samples this
            // shows exactly the old static bake; the effect lane clones + ticks it per instance.
            // `w` = the WMO interior batch-class lane: an interior group's INT batches draw UNLIT
            // (pure tex × MOCV) and its TRANS batches lerp lit↔bake by the MOCV alpha (wow-re
            // `trace-forensics-abbey-interior-d3d` §2 — observed on the abbey at close range, the
            // northshire "lit interior batch" datum having been a mis-identified unit). Exterior
            // groups' batches (and every M2) ride 0 = the exterior law.
            tint: {
                let t0 = rgb_anim.map_or([1.0, 1.0, 1.0], |a| a.sample(0.0));
                let class_lane = match (is_interior && is_wmo, wmo_class) {
                    (true, Some(WmoBatchClass::Int)) => 1.0,
                    (true, Some(WmoBatchClass::Trans)) => 2.0,
                    _ => 0.0,
                };
                Vec4::new(t0[0], t0[1], t0[2], class_lane)
            },
            // The WMO window/glass law (`WowModelExt::sidn`): xyz = the authored SIDN emissive
            // (gamma bytes /255 — the shader ramps it by the live night fraction on lit lanes),
            // w = the WINDOW midpoint-light flag.
            sidn: {
                let c = sidn.unwrap_or([0, 0, 0]);
                Vec4::new(
                    f32::from(c[0]) / 255.0,
                    f32::from(c[1]) / 255.0,
                    f32::from(c[2]) / 255.0,
                    if window { 1.0 } else { 0.0 },
                )
            },
            light_buf: light.clone(),
        },
    });
    cache.insert(key, handle.clone());
    handle
}

/// `clutter_fade.z` marker bit 9: this material is a depth-prime twin ([`zfill_material`]).
pub(crate) const ZFILL_MARKER: u16 = 1 << 9;

/// `clutter_fade.z` marker bit 10: this twin's SOURCE batch alpha-tests, so the shader re-applies
/// the hard 224/255 cutout while blending (`wow_model.wgsl`). Set from the source blend mode by
/// [`model_material`] (fade twins) and [`zfill_material`] (depth-prime twins); never on an
/// Opaque-source twin — the reference runs mode 0 with the alpha test disabled, steady and
/// promoted alike (`m2-blend-promotion-zfill.md` §2; decision 0842).
pub(crate) const TWIN_CUTOUT_MARKER: u16 = 1 << 10;

/// The twin's `Transparent3d` sort bias (yards; **negative = drawn earlier** — the sign law is
/// `sky_order`'s module doc). It must clear the spread of one model's part AABB centres, so **all**
/// of a fading model's twins draw before **any** of its colour parts — the reference achieves the
/// same by tying every command of one instance to a single sort key (`cmd+0x14 = model+0x84`) and
/// putting twins first inside the tie (`m2-blend-promotion-zfill.md` §4/§6). 8 yd covers every
/// humanoid and mount; the residue (a model taller than 8 yd may keep a little self-overlap
/// darkening at its extremities, and transparent scene content sorting within 8 yd behind a fading
/// body draws after the prime and is depth-clipped where the body covers it) is recorded in
/// decision 0831. The same field doubles as the rasterizer depth bias in relative ULPs — at −8 that
/// is ~2⁻²⁰ relative, and its direction (twin marginally farther) only makes the colour pass's
/// GreaterEqual test safer against cross-pipeline ULP noise.
pub(crate) const ZFILL_SORT_BIAS: f32 = -8.0;

/// Build (or fetch) the **depth-prime twin** material for one fadeable entity batch — the
/// reference's `M2UseZFill` clone (wow-re `m2-blend-promotion-zfill.md` §4, VERIFIED): while a
/// model draws translucent (`0 < A < 1`), a colour-masked, blend-off, z-writing copy of each of its
/// depth-writing batches draws first, so every colour fragment behind the model's own nearest
/// surface fails the depth test — one blended layer everywhere, no self-overlap darkening.
///
/// `cutout` = **the source batch alpha-tests** (AlphaKey). It mirrors what the part's COLOUR pass
/// discards, and must keep mirroring it: a twin that writes depth where the colour pass discards
/// leaves an invisible depth wall inside the cutout holes. Only an AlphaKey source rides the fade
/// twin's hard 224/255 cutout ([`TWIN_CUTOUT_MARKER`]); Opaque sources never alpha-test
/// (`m2-blend-promotion-zfill.md` §2/§4 — the reference's twin keeps mode 1's test, mode 0 has
/// none; decision 0842) and authored-Blend sources discard nothing either.
///
/// Everything that shapes only the *colour* (lighting lane, shade, fog, tint) is canonicalized so
/// twins dedupe maximally — the `WOW_ZFILL` fragment returns before any of it.
pub(crate) fn zfill_material(
    cache: &mut MaterialCache,
    materials: &mut Assets<WowModelMaterial>,
    texture: Option<Handle<Image>>,
    two_sided: bool,
    cutout: bool,
    light: &Buffer,
) -> Handle<WowModelMaterial> {
    let key = MatKey {
        texture: texture.as_ref().map(Handle::id),
        blend: if cutout {
            ModelBlend::AlphaTest
        } else {
            ModelBlend::Blend
        },
        two_sided,
        is_wmo: false,
        is_interior: false,
        is_emissive: false,
        is_additive: false,
        fade_variant: cutout,
        no_depth_write: false,
        no_depth_test: false,
        fog_policy: benilla_formats::FogPolicy::Scene,
        shade: ShadeSel::Lit,
        wmo_batch_order: 0,
        uv_anim: None,
        rgb_anim: None,
        wmo_class: None,
        sidn: None,
        window: false,
        zfill: true,
    };
    if let Some(h) = cache.fetch(&key) {
        return h;
    }
    let cull_mode = if two_sided { None } else { Some(Face::Back) };
    let base = StandardMaterial {
        base_color_texture: texture,
        // Blend keeps the twin in the transparent pass, where the sort bias can place it before
        // the colour parts; `specialize` masks the colour writes off and turns blend off.
        alpha_mode: AlphaMode::Blend,
        depth_bias: ZFILL_SORT_BIAS,
        double_sided: two_sided,
        cull_mode,
        ..default()
    };
    let handle = materials.add(ExtendedMaterial {
        base,
        extension: WowModelExt {
            // Bit 10 (the twin-cutout marker) drives the shader's hard 224/255 cutout — set
            // exactly when the colour pass discards, so depth and colour coverage agree.
            clutter_fade: Vec4::new(
                0.0,
                0.0,
                f32::from(ZFILL_MARKER | if cutout { TWIN_CUTOUT_MARKER } else { 0 }),
                0.0,
            ),
            // Not a fade twin (`model_flags.y` = 0): the zfill pipeline branch forces its own
            // depth-write, and the cutout rides bit 10 above, so the bit has no work here.
            model_flags: Vec4::ZERO,
            sun_scale: Vec4::new(ShadeSel::Lit.selector(), 0.0, 0.0, 0.0),
            tint: Vec4::new(1.0, 1.0, 1.0, 0.0),
            sidn: Vec4::ZERO,
            light_buf: light.clone(),
        },
    });
    cache.insert(key, handle.clone());
    handle
}

/// Replace the fog-policy bits (4-6) inside a packed `clutter_fade.z` marker word, preserving
/// every other pipeline marker — bits 0-3 AND the Mod/Mod2x multiply markers in bits 7-8
/// (decision 0528). The mask lives here, beside the packer above: the portrait booth's rig twin
/// once truncated the word to `u8` and hand-masked `& 0x0f`, silently dropping the multiply
/// markers — the char-select white-blade regression.
pub(crate) fn replace_fog_policy(z: f32, policy: benilla_formats::FogPolicy) -> f32 {
    f32::from((z as u16 & !(7 << 4)) | ((policy as u16) << 4))
}

/// A model reference path (`.mdx`/`.mdl`, mixed case, backslashes) → its `mpq://…m2` load URL.
/// Lowercased so case variants share one `AssetServer` handle; the physical archive file is `.m2`.
pub(crate) fn m2_url(raw: &str) -> String {
    let p = raw.to_ascii_lowercase().replace('\\', "/");
    let stem = p
        .strip_suffix(".mdx")
        .or_else(|| p.strip_suffix(".mdl"))
        .or_else(|| p.strip_suffix(".m2"))
        .unwrap_or(&p);
    format!("mpq://{stem}.m2")
}

/// A WMO root path → its `mpq://…wmo` load URL (already `.wmo`; lowercased for handle dedup).
pub(crate) fn wmo_url(raw: &str) -> String {
    format!("mpq://{}", raw.to_ascii_lowercase().replace('\\', "/"))
}

/// A creature skin variation → its `mpq://…blp` URL: `<model-dir>\<name>.blp`. `model_dir` is the
/// directory of the creature's model path (where its `Monster1/2/3` skins live).
pub(crate) fn skin_url(model_dir: &str, name: &str) -> String {
    let dir = model_dir.replace('\\', "/").to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    if dir.is_empty() {
        format!("mpq://{name}.blp")
    } else {
        format!("mpq://{dir}/{name}.blp")
    }
}

#[cfg(test)]
mod tests {
    use super::replace_fog_policy;
    use benilla_formats::FogPolicy;

    /// Swapping the fog policy must preserve EVERY pipeline marker bit — 0-3 and the 0528
    /// multiply markers in 7-8. The portrait booth's old hand-rolled `as u8 & 0x0f` dropped
    /// bits 7-8 (the char-select white-blade regression this pins against).
    #[test]
    fn replace_fog_policy_preserves_pipeline_markers() {
        // All marker bits set (0-3, 7-8) + fog policy Grey (3) in bits 4-6.
        let z = f32::from(0b1_1000_1111u16 | (3 << 4));
        let out = replace_fog_policy(z, FogPolicy::Off) as u16;
        assert_eq!(out & 0b1_1000_1111, 0b1_1000_1111, "markers preserved");
        assert_eq!((out >> 4) & 7, FogPolicy::Off as u16, "fog swapped");
        // And the reverse direction: no stray bits invented.
        let out2 = replace_fog_policy(f32::from(0u16), FogPolicy::Grey) as u16;
        assert_eq!(out2, (FogPolicy::Grey as u16) << 4);
    }
}
