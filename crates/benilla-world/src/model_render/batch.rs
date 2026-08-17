//! [`M2BatchMaterials`] — one authored batch's material identity, built in one place.
//!
//! An authored render batch ([`ModelSubmesh`]) already *carries* almost its whole material
//! identity: texture, blend, sidedness, the M2 render flags (`0x04`/`0x08`/`0x10`), the fog
//! policy, the generated-texcoord bit, the WMO MOMT law (MOBA class, SIDN, WINDOW). The only
//! things a spawner chooses are which light lane the batch draws on and how many *variants* of it
//! the lane needs. Everything else is read off the batch here — which is why the twenty-argument
//! builder ([`super::model_material`]) has exactly two callers left outside this file, both of
//! them the engine's own streamer and pipeline-warmer.
//!
//! **The variant set is the point.** One authored submesh renders as up to six deduped handles —
//! steady, interior-matte, interior-bake, the bake's blend twin, the appear/distance-fade blend
//! twin, and the depth-prime twin — and the law that decides which of them collapse onto each
//! other (a multiply batch cannot feather through alpha; an authored-Blend batch already is its
//! own twin; a `no_depth_write` batch primes nothing) is one law, recorded across decisions 0842,
//! 0865, 0831 and 0355. It was written out by hand at seven spawn sites before this facade, each
//! copy free to drift from the others.
//!
//! The dedup cache is one cache ([`ModelMaterials`]). It keys on the full material identity, so
//! two lanes that build the same batch the same way get the same handle — which is the whole
//! reason the cache exists, and was true of four separate caches only by luck.

use benilla_assets::materials::WowModelMaterial;
use benilla_assets::ModelSubmesh;
use benilla_formats::ModelBlend;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::render::render_resource::Buffer;

use super::{model_material, zfill_material, MaterialCache, ShadeSel};
use crate::lighting::SharedLightBuffer;

/// The engine's one `WowModelMaterial` dedup cache. Swept by distance like every other art cache
/// (`art_scope`) and cleared whole on a map change; an evicted entry costs one rebuild, never a
/// wrong material, because [`super::MatKey`] determines the material completely.
#[derive(Resource, Default)]
pub struct ModelMaterials(pub(crate) MaterialCache);

/// The deduped handles one authored batch renders as. Field-accessed, never named by a caller:
/// which of these a lane actually spawns is the lane's business, but *building* them is not.
pub struct BatchVariants {
    /// The steady exterior look — sky-lit, the material an instance draws with almost always.
    pub steady: Handle<WowModelMaterial>,
    /// The interior MATTE variant: the plain day/night pair at sun ×1.0 — the reference's
    /// null-node fallback, taken when the footprint probe misses.
    pub interior: Handle<WowModelMaterial>,
    /// The interior BAKE variant: interior-PROP mode, the shader evaluating the model's SH probe
    /// by its `MeshTag` slot — the steady indoor law for every entity M2.
    pub interior_bake: Handle<WowModelMaterial>,
    /// The bake lane's blend twin — the probe-lit feather (decision 0355). Without it a fade
    /// indoors swaps to the EXTERIOR twin and the light jumps mid-feather.
    pub interior_bake_blend: Handle<WowModelMaterial>,
    /// The `AlphaMode::Blend` twin every feather pass rides (spawn appear-fade, distance fade,
    /// aura fades). Equal to [`Self::steady`] when the steady material already blends.
    pub fade_blend: Handle<WowModelMaterial>,
    /// The depth-prime twin (`m2-blend-promotion-zfill.md` §4). `None` for a batch that writes no
    /// depth to prime — one that disables depth write/test, or a multiply batch.
    pub zfill: Option<Handle<WowModelMaterial>>,
}

/// Build the material(s) an authored render batch draws with.
///
/// Every method returns `None` until the shared light buffer exists (it is created at startup in
/// the render world's shadow; a spawner that runs before it must retry, not bake a material
/// against nothing). [`Self::ready`] is the same question asked without building anything.
#[derive(SystemParam)]
pub struct M2BatchMaterials<'w> {
    cache: ResMut<'w, ModelMaterials>,
    materials: ResMut<'w, Assets<WowModelMaterial>>,
    light: Option<Res<'w, SharedLightBuffer>>,
}

impl M2BatchMaterials<'_> {
    /// Is the shared light buffer resident? A spawner that has other work to skip asks first.
    pub fn ready(&self) -> bool {
        self.light.is_some()
    }

    /// One steady material for an authored batch drawn in the world on the sky lane.
    ///
    /// `order` is the authored batch index + 1 (`0` = unordered), the transparent-pass sort bias
    /// that keeps one model's coplanar layers in file order ([`super::BATCH_ORDER_SORT_EPS`]).
    /// A WMO group batch takes the FFP N·L lane ([`ShadeSel::Matte`]) rather than the M2 lobe —
    /// that is a property of the batch, not a caller's choice, so it is read off the batch here.
    pub fn steady(
        &mut self,
        sub: &ModelSubmesh,
        texture: Option<Handle<Image>>,
        order: u16,
    ) -> Option<Handle<WowModelMaterial>> {
        let light = self.light.as_ref()?.0.clone();
        Some(self.build(
            sub,
            texture,
            order,
            shade_for(sub),
            false,
            false,
            false,
            &light,
        ))
    }

    /// One material for a batch of a **WMO skybox** — the painted sky a building owns
    /// ([`crate::wmo_sky`]), which is an ordinary M2 and is drawn as one (decision 1264).
    ///
    /// Everything that makes it a *skybox* rather than a doodad is here, and it is only three
    /// things. Blend mode, sidedness, the UNLIT and UNFOGGED bits, the alpha-key reference, the
    /// multiply factors and the additive gamma premultiply all come off the authored batch, like
    /// any other model's — which is the whole point: the lane this replaced read positions, UVs and
    /// one texture and drew every batch opaque, so `CavernsOfTimeSky`'s six ADDITIVE star sheets
    /// (near-white RGB whose stars live in the ALPHA channel) painted a flat white sheet over the
    /// painted sky, and its planets and asteroid belts drew as dark opaque cards.
    ///
    /// - **The far depth** (`sky_depth`): the sky's whole occlusion law is that it writes the
    ///   reverse-Z far value and lets the world paint over it ([`crate::sky_order`], "The depth
    ///   law").
    /// - **Depth-write OFF, always**, whatever the batch authored — a sky element leaves the
    ///   z-buffer at its clear value so the forced-far glare quads stay occluded by world geometry
    ///   alone (the rule [`crate::sky`] gives the gradient dome).
    /// - **Depth-test ON, always**, and this one *overrides* the batch. Every `CavernsOfTimeSky`
    ///   batch sets M2 flag `0x08` (no depth test), which on this lane becomes
    ///   `CompareFunction::Always` and would paint the sky straight over the room around you. The
    ///   flag is how the reference orders the sky pass *within* its own squashed depth slice
    ///   (`[0.975, 0.98]`, painter's order, depth-write off); our port of that ordering is the sort
    ///   rung ([`super::skybox_sort_bias`]), so honouring the flag here would be reading it twice.
    ///
    /// No variant set: a skybox never feathers, never primes depth, and is never indoors or
    /// outdoors — it *is* the outdoors.
    pub fn skybox(
        &mut self,
        sub: &benilla_formats::RenderSubmesh,
        texture: Option<Handle<Image>>,
        order: u16,
    ) -> Option<Handle<WowModelMaterial>> {
        let light = self.light.as_ref()?.0.clone();
        Some(model_material(
            &mut self.cache.0,
            &mut self.materials,
            texture,
            sub.blend,
            sub.two_sided,
            false, // an M2, whichever building names it
            false,
            sub.emissive,
            sub.additive,
            false, // never feathers
            true,  // …never writes depth
            false, // …and never skips the test — see above
            sub.fog_policy,
            sub.env_map,
            ShadeSel::Lit, // unread: every skybox batch the chain ships authors UNLIT (0x01)
            order,
            // No texture-transform or M2Color loop is wired on this lane, and neither shipped
            // skybox authors one (`CavernsOfTimeSky` has 0 texture transforms and 0 animated colour
            // tracks; `StratholmeSkybox` is fully static). Wiring them means loading the skybox
            // through the `M2Model` asset lane — which holds these as the `Arc`s the material key
            // identifies them by, and is also what the asteroid belts' BONE animation needs. One
            // move, when that lands; a locally-minted `Arc` here would be a dedup key with no owner.
            None,
            None,
            None, // M2: no MOBA class, no SIDN, no WINDOW
            None,
            false,
            true, // the sky lane
            &light,
            None, // one shared sky material — no per-sequence channel to key on
        ))
    }

    /// One steady material for a batch drawn **off-world**, against a render target's own light
    /// buffer instead of the world's: a portrait booth's frozen studio rig, the model pane's, the
    /// char-select stage's. The buffer is a key axis ([`super::MatKey::light`]), so these share
    /// the one dedup cache with the world lane without ever colliding with it.
    ///
    /// `rig` picks the scene's **own authored M2 light rig** ([`ShadeSel::Rig`] — the probe-slot SH
    /// eval plus the buffer's point table, decisions 0429/0435) over the sky lane, and plays the
    /// batch's authored UV animation: together, what a char-select scene is and a portrait bake
    /// is not (a booth freezes at t = 0 and is never ground-shaded).
    pub fn off_world(
        &mut self,
        sub: &ModelSubmesh,
        texture: Option<Handle<Image>>,
        light: &Buffer,
        rig: bool,
    ) -> Handle<WowModelMaterial> {
        let shade = if rig { ShadeSel::Rig } else { ShadeSel::Lit };
        self.build(sub, texture, 0, shade, false, false, rig, light)
    }

    /// The full variant set an **entity** part needs: every M2 entity — unit, player, GameObject,
    /// held item, spell effect — is built LIT and carries the same indoor pair, because the
    /// reference hands every entity M2 the same entity-node fill (wow-re `unit-m2-shader-light`).
    pub fn entity_variants(
        &mut self,
        sub: &ModelSubmesh,
        texture: Option<Handle<Image>>,
        order: u16,
    ) -> Option<BatchVariants> {
        let light = self.light.as_ref()?.0.clone();
        let steady = self.build(
            sub,
            texture.clone(),
            order,
            ShadeSel::Lit,
            false,
            false,
            false,
            &light,
        );
        let interior = self.build(
            sub,
            texture.clone(),
            order,
            ShadeSel::Matte,
            false,
            false,
            false,
            &light,
        );
        let interior_bake = self.build(
            sub,
            texture.clone(),
            order,
            ShadeSel::Matte,
            true,
            false,
            false,
            &light,
        );
        // A multiply batch (Mod/Mod2x) and an authored-Blend batch are their own twin: the first
        // because its blend equation reads no alpha at all — the reference's instanceAlpha ramp
        // cannot feather it, and benilla's deliberate deviation feathers it through the shader's
        // identity-lerp on the tag alpha instead (decision 0865) — the second because its colour
        // pass already blends. Everything else gets a real twin, built from the SOURCE blend so
        // the 224/255 cutout marker matches what the colour pass discards (decision 0842).
        let own_twin = matches!(
            sub.blend,
            ModelBlend::Blend | ModelBlend::Mod | ModelBlend::Mod2x
        );
        let fade_blend = if own_twin {
            steady.clone()
        } else {
            self.build(
                sub,
                texture.clone(),
                order,
                ShadeSel::Lit,
                false,
                true,
                false,
                &light,
            )
        };
        let interior_bake_blend = if own_twin {
            interior_bake.clone()
        } else {
            self.build(
                sub,
                texture.clone(),
                order,
                ShadeSel::Matte,
                true,
                true,
                false,
                &light,
            )
        };
        Some(BatchVariants {
            steady,
            interior,
            interior_bake,
            interior_bake_blend,
            fade_blend,
            zfill: self.zfill_for(sub, texture, &light),
        })
    }

    /// The variant set for a **character composite** texture — a runtime-built body/hair atlas,
    /// not an authored batch, so the caller supplies the blend and sidedness the composite is
    /// drawn with (the body is opaque, hair alpha-cuts, a robe skirt keeps its own `0x04`).
    ///
    /// A composite sheet carries no blend-mode fog policy of its own, no generated texcoords, no
    /// M2Color track, and is never a multiply batch — so unlike [`Self::entity_variants`] every
    /// twin here is real and the depth-prime twin always exists.
    pub fn char_variants(
        &mut self,
        texture: Handle<Image>,
        blend: ModelBlend,
        two_sided: bool,
    ) -> Option<BatchVariants> {
        let light = self.light.as_ref()?.0.clone();
        let mut mk = |shade: ShadeSel, probe: bool, fade: bool| {
            model_material(
                &mut self.cache.0,
                &mut self.materials,
                Some(texture.clone()),
                blend,
                two_sided,
                false, // a composite sheet is never a WMO batch
                probe, // interior-PROP mode: the shader reads the MeshTag as an SH-probe slot
                false, // …never UNLIT
                false, // …nor additive
                fade,
                false, // …nor depth-write/test disabled: body and hair are ordinary geometry
                false,
                // Body/hair are always opaque/alpha-cut, which the byte table fogs toward the
                // scene colour anyway.
                benilla_formats::FogPolicy::Scene,
                // A composite body/hair sheet is sampled by the body's own authored UVs.
                // Env-mapping reaches a character through the ArmorReflect batches of its HELD
                // items, which spawn as ordinary M2 submeshes and carry their own flag.
                false,
                shade,
                0,     // a composite is one sheet, not a batch in an authored order
                None,  // …with no texture transform
                None,  // …and no animated M2Color tint
                None,  // worn/held part: light selection anchors at the instance origin
                None,  // M2 carries no MOMT SIDN colour
                false, // …nor the WINDOW flag
                false, // a character composite is never a skybox
                &light,
                None, // a composite sheet carries no animated UV/tint channel at all
            )
        };
        Some(BatchVariants {
            steady: mk(ShadeSel::Lit, false, false),
            interior: mk(ShadeSel::Matte, false, false),
            interior_bake: mk(ShadeSel::Matte, true, false),
            interior_bake_blend: mk(ShadeSel::Matte, true, true),
            fade_blend: mk(ShadeSel::Lit, false, true),
            // Decision 0831: character parts are opaque/alpha-cut and always z-writing, so every
            // one twins. `cutout` mirrors the colour twin's 224/255 discard exactly — only an
            // AlphaKey source alpha-tests while fading (decision 0842).
            zfill: Some(zfill_material(
                &mut self.cache.0,
                &mut self.materials,
                Some(texture),
                two_sided,
                blend == ModelBlend::AlphaTest,
                &light,
            )),
        })
    }

    /// The three pieces the engine's **placed**-instance spawner still takes by hand: its cache,
    /// its material store and the light buffer.
    ///
    /// A temporary seam, and a narrow one — the streamer's `spawn_model_entities` builds a
    /// different variant set (two handles, not six: a placement's indoor law is decided once at
    /// placement time by its probe slot, where an entity's is decided every frame by which room it
    /// walked into), and its one caller outside the engine is `entities::wmo_props`, which 1163
    /// classified as mis-filed engine code. The seam closes when that file moves, not by widening
    /// this facade to a lane it does not serve.
    pub fn pieces(
        &mut self,
    ) -> Option<(&mut MaterialCache, &mut Assets<WowModelMaterial>, Buffer)> {
        let light = self.light.as_ref()?.0.clone();
        Some((&mut self.cache.0, &mut self.materials, light))
    }

    /// The depth-prime twin, or `None` for a batch that primes nothing: one that disables depth
    /// write or test (it never writes the depth a twin would prime), or a multiply batch (which
    /// never feathers, so there is no translucent frame to prime for).
    fn zfill_for(
        &mut self,
        sub: &ModelSubmesh,
        texture: Option<Handle<Image>>,
        light: &Buffer,
    ) -> Option<Handle<WowModelMaterial>> {
        if sub.no_depth_write || sub.no_depth_test {
            return None;
        }
        match sub.blend {
            ModelBlend::Mod | ModelBlend::Mod2x => None,
            b => Some(zfill_material(
                &mut self.cache.0,
                &mut self.materials,
                texture,
                sub.two_sided,
                b == ModelBlend::AlphaTest,
                light,
            )),
        }
    }

    /// The one call into the twenty-argument builder: every argument but the four this facade's
    /// entry points choose is read straight off the authored batch.
    #[allow(clippy::too_many_arguments)]
    fn build(
        &mut self,
        sub: &ModelSubmesh,
        texture: Option<Handle<Image>>,
        order: u16,
        shade: ShadeSel,
        probe: bool,
        fade: bool,
        play_uv: bool,
        light: &Buffer,
    ) -> Handle<WowModelMaterial> {
        model_material(
            &mut self.cache.0,
            &mut self.materials,
            texture,
            sub.blend,
            // A billboard card is culled by the SAME rule as any other batch — the material's
            // `0x04` flag, nothing else (decision 0629, bugs B05/B34).
            sub.two_sided,
            sub.wmo_batch.is_some(),
            // Interior-PROP mode (the SH-probe lane) when a variant asks for it; otherwise the
            // batch's own authored interior bit, which is set for WMO group batches and never
            // for M2.
            probe || sub.interior,
            sub.emissive,
            sub.additive,
            fade,
            sub.no_depth_write,
            sub.no_depth_test,
            sub.fog_policy,
            sub.env_map,
            shade,
            order,
            play_uv.then_some(sub.uv_anim.as_ref()).flatten(),
            sub.rgb_anim.as_ref(),
            sub.wmo_batch,
            sub.sidn,
            sub.window,
            // The sky lane is [`Self::skybox`]'s alone: it overrides depth state the authored batch
            // asks for, so it cannot be one more variant built from `sub` here.
            false,
            light,
            // The ENTITY lane's batches (units, GameObjects, held items): shared per batch, as
            // ever. The per-placement lane is the world streamer's alone (decision 1408) — every
            // affected model is a placed `World\…` prop, and an entity already resolves its
            // sequence through `MatAnim::host`.
            None,
        )
    }
}

/// A WMO group batch shades through the FFP N·L path, not the M2 lobe, so its `sun_scale` lane is
/// unread — a property of the batch, not of the lane that spawns it. Every other batch is LIT: the
/// verified §9 chain gives entity M2s the same 2.5/0.5 lane as ADT doodads, and the dynamic half
/// (the MCSH sample at their feet) rides the per-instance `MeshTag` shade byte, not the material.
fn shade_for(sub: &ModelSubmesh) -> ShadeSel {
    if sub.wmo_batch.is_some() {
        ShadeSel::Matte
    } else {
        ShadeSel::Lit
    }
}
