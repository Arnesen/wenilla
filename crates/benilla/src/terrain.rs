//! Phase 6 terrain splat material: an [`ExtendedMaterial`] over `StandardMaterial` that blends up
//! to 4 tiled layer textures by a packed alpha map, keeping PBR lighting/shadows/fog. The WGSL
//! lives in `assets/shaders/terrain.wgsl`.
//!
//! **Per-tile, not per-chunk.** A whole ADT tile shares ONE material: its ground textures are stacked
//! into a `layer_array` (`texture_2d_array`) and its per-chunk alpha maps into an `alpha_array`. Each
//! merged-mesh vertex carries its chunk's 4 layer indices (vertex `COLOR`) + alpha-layer index
//! (`UV1.x`), so all 256 chunks draw from one material — letting Bevy batch the tile into a single
//! draw instead of 256. (The old design bound 4 textures + an alpha map per chunk → a material each.)

use bevy::image::Image;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{
    ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline,
};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, Buffer, CompareFunction,
    RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

/// `StandardMaterial` + our per-tile layer-blend extension.
pub type TerrainMaterial = ExtendedMaterial<StandardMaterial, TerrainExtension>;

/// World models (doodads/WMO/creatures/GameObjects) lit by the **same WoW lighting as terrain** —
/// `tex × saturate(ambient + diffuse·N·L) × scale` in gamma space — instead of PBR. Reuses
/// `StandardMaterial` for the texture/alpha/cull (set in `model_material`); the extension just
/// carries the shared light. Keeping models and terrain on one lighting model is what makes the
/// scene coherent (PBR couldn't clamp, so it tinted models orange).
pub type WowModelMaterial = ExtendedMaterial<StandardMaterial, WowModelExt>;

/// Pipeline-specialization key for [`WowModelExt`] — picks the two distance-fade pipeline tweaks Bevy
/// has no built-in `AlphaMode` for. Both make the fade **OPACITY** with **depth-write ON**, matching the
/// reference (`RECONCILE-fade-render-state.md` / `-clutter-fade-render-state.md`):
/// - `fade` (`model_flags.y`) = the **M2-doodad** fade blend twin: `AlphaMode::Blend` (transparent pass,
///   depth-write normally OFF) → `specialize` forces depth-write back ON so a fading haystack's near
///   cross-quads occlude its far ones. Silhouette stable (the shader re-applies the 224/255 cutout).
/// - `clutter` (`clutter_fade.w`) = ground clutter: an `AlphaMode::Mask` (alpha-mask pass, depth-write
///   already ON) that the reference also **blends** (prog 201: `SRC_ALPHA/ONE_MINUS_SRC_ALPHA`) so the
///   ~70 yd ramp fades opacity. `specialize` forces that over-blend on in place; the 128/255 discard +
///   `tex × ramp` output alpha are already done in the shader.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct WowModelKey {
    fade: bool,
    clutter: bool,
    /// Additive glow card (`model_flags.w == 2.0`) — `specialize` forces a `SrcAlpha`-additive blend
    /// state (`rgb·α + dst`) so the glow texture's radial alpha shapes a soft halo. See `model_render`.
    additive: bool,
    /// M2 render flag 0x10 — disable depth WRITE for this batch (packed into `clutter_fade.z` bit 0 by
    /// `model_material`; `clutter_fade.z` is unread by the shader, so it carries this Rust-side marker).
    no_depth_write: bool,
    /// M2 render flag 0x08 — disable depth TEST (packed into `clutter_fade.z` bit 1).
    no_depth_test: bool,
    /// The MULTIPLY blends (clutter_fade.z bits 7/8; decision 0528) — `specialize` swaps the blend
    /// state to the byte-verified factors: Mod `DST_COLOR/ZERO`, Mod2x `DST_COLOR/SRC_COLOR`
    /// (wow-re `m2-depth-blend-state`). Exact on the 0161 gamma lane — the framebuffer holds gamma
    /// values, so the hardware multiply IS the reference's byte multiply.
    modulate: bool,
    modulate2x: bool,
    /// WMO authored batch index + 1 (0 = not a WMO batch), packed into `sun_scale.y` (shader-unread).
    /// Drives a per-batch pipeline depth bias in `specialize`: the client draws WMO batches in strict
    /// MOBA file order with depth-write ON + LEQUAL, so a later coplanar batch (wall decal/trim) wins
    /// by ORDER (wow-5875-re `models/scratch/wmo-batch-blend-depth-state.md`, byte-verified). Bevy
    /// orders draws for batching, not authorship, so coplanar ties were unstable (the run-to-run
    /// "pale film" on buildings); the bias reproduces the authored layering deterministically.
    wmo_batch_order: u16,
}

impl From<&WowModelExt> for WowModelKey {
    fn from(e: &WowModelExt) -> Self {
        let markers = e.clutter_fade.z as u32;
        Self {
            fade: e.model_flags.y > 0.5,
            clutter: e.clutter_fade.w > 0.5,
            additive: markers & 4 != 0,
            no_depth_write: markers & 1 != 0,
            no_depth_test: markers & 2 != 0,
            modulate: markers & 0x80 != 0,
            modulate2x: markers & 0x100 != 0,
            wmo_batch_order: e.sun_scale.y as u16,
        }
    }
}

/// The WoW light, shared by every model material. **All Vec4 uniforms merge onto one binding
/// (100)** — `AsBindGroup` packs multiple `#[uniform(N)]` fields at the same `N` into a single
/// buffer entry, which is what keeps the model pipeline under Metal's 16-buffer vertex-stage cap.
/// (Bevy 0.18's AsBindGroup macro hardcodes uniform-binding visibility to `FRAGMENT|VERTEX|COMPUTE`
/// regardless of `visibility(fragment)`, so the only effective lever for the BUFFER count is the
/// number of bindings — pack, don't narrow.) The WGSL struct order **must** match the field order
/// below (each Vec4 is 16 bytes, no implicit padding).
#[derive(Asset, AsBindGroup, Clone, TypePath)]
#[bind_group_data(WowModelKey)]
pub struct WowModelExt {
    /// Ground-clutter distance fade: `x` = full-opacity radius (yd), `y` = fully-faded radius (yd),
    /// `w` = enabled (>0.5). `Vec4::ZERO` (the default for trees/WMOs/creatures) disables it; clutter
    /// sets it to the detail-doodad horizon (~52.5→70 yd) so grass erodes out with distance. Per-material
    /// (set at creation, not light) so it stays on the cheap packed uniform, not the shared buffer.
    #[uniform(100)]
    pub clutter_fade: Vec4,
    /// Per-material flags (set at material creation, NOT light). `x` = **is_wmo** (>0.5 ⇒ FFP directional
    /// `ambient + sun·N·L` × MOCV at sun-scale 1, no exterior terrain-shade). `y` = **fade variant** (>0.5 ⇒
    /// the distance-fade BLEND twin → the shader re-applies the hard 224/255 cutout for a stable silhouette
    /// and `specialize` forces depth-write ON; see [`WowModelKey`]). `zw` reserved.
    #[uniform(100)]
    pub model_flags: Vec4,
    /// Per-material MCSH terrain-shade **selector** (`wow_model.wgsl`, the exterior doodad matte). `x` picks
    /// which live doodad sun LEVEL scales the FFP matte's diffuse/sun term — `1.0` ⇒ lit ground, `0.2` ⇒
    /// MCSH-shadowed ground (the shader thresholds at 0.5), so a doodad inherits the shade it stands in like
    /// the clutter/terrain beside it. Set at creation (a doodad doesn't move, so its shade is static ⇒ it
    /// rides the deduped material, not per-instance `MeshTag`). Clutter/WMO ignore it (sun-scale 1);
    /// creatures/player default `1.0` until the live-sample pass. `y` = the WMO authored batch order
    /// (shader-unread; `WowModelKey` reads it back for the per-batch depth bias). `zw` = the batch's
    /// live **UV-animation offset** (decision 0130 phase 3): added to the stage UVs in
    /// `wow_model.wgsl`; `0` for static batches, re-sampled per frame by
    /// `doodad_anim::tick_uv_anim_materials` for texanim batches (flowing waterfalls).
    #[uniform(100)]
    pub sun_scale: Vec4,
    /// Per-material **RGB tint** (`xyz`): the M2Color colour multiplied into the albedo exactly
    /// where the static vertex-colour tint folds — carrying the tint of batches whose colour track
    /// ANIMATES (the vertex bake is skipped for those, `benilla-formats` `m2_batches`). Seeded at
    /// the track's first key (pixel-identical to the old static bake for a lane that never ticks
    /// it); the effect lane clones the material per instance and re-samples per frame (a spell
    /// effect's white-hot flash cooling to red). `xyz = 1` — the identity — for the overwhelming
    /// majority. **`w` = the WMO interior batch-class lane** (`0` = exterior law, `1` = interior
    /// INT ⇒ unlit `tex × MOCV`, `2` = interior TRANS ⇒ the per-vertex MOCV-alpha lit↔bake lerp —
    /// wow-re `trace-forensics-abbey-interior-d3d` §2); `0` for every non-WMO batch.
    #[uniform(100)]
    pub tint: Vec4,
    /// The WMO window/glass law (byte-verified, wow-re `wmo-lit-selector` / `wmo-interior-night-light`).
    /// `xyz` = the MOMT **SIDN** authored emissive colour (gamma bytes /255; `0` for non-SIDN and every
    /// M2): the shader multiplies it by the live night fraction (`wow_light.grade.x`) and adds it inside
    /// the lit sum on lit lanes — windows glow warm at night, nothing by day. `w` = the MOMT **WINDOW**
    /// flag (>0.5): an interior-group batch swaps to the brighter Direct/Ambient-midpoint light
    /// (ambient +16/255) — the warm pane seen from inside a building.
    #[uniform(100)]
    pub sidn: Vec4,
    /// **The shared global light** (`lighting::global_light`): one storage buffer every material reads,
    /// updated once/frame in place — replaces the per-material light/fog/SH uniforms (ambient/diffuse/
    /// sun/spec + fog + the 7 Model2.bls SH-probe coeffs) the old `apply_wow_lighting` re-pushed every
    /// frame (re-creating every bind group). `wow_model.wgsl` reads it as `var<storage, read> wow_light`
    /// (rows 0-12 + the point-light table). Both stages: the fragment does the custom shading, the
    /// vertex evaluates the Gouraud point-light term from the appended table (decision 0278 — bevy's
    /// own clusterable buffer is fragment-only in the view layout, so the lights ride our buffer).
    /// Set once, never mutated.
    #[storage(90, read_only, buffer, visibility(vertex, fragment))]
    pub light_buf: Buffer,
}

impl MaterialExtension for WowModelExt {
    /// Custom vertex stage (decision 0278): bevy's mesh vertex verbatim plus the GOURAUD point-light
    /// term — the dynamic light sum evaluates per VERTEX like the reference FFP and interpolates,
    /// which is what spreads a fixture's floor pool and keeps the forge hood dim.
    fn vertex_shader() -> ShaderRef {
        "shaders/wow_model.wgsl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "shaders/wow_model.wgsl".into()
    }

    /// Per-batch depth + blend overrides Bevy's `AlphaMode` can't express. **Depth:** the real client
    /// writes depth for EVERY M2 batch — opaque or transparent — and tests `LEQUAL`, *unless* the
    /// material's render flag 0x10 (no-write) / 0x08 (no-test) clears it (VERIFIED, wow-re
    /// `m2-depth-blend-state`). Bevy's transparent pass defaults depth-write OFF, so a model's own
    /// transparent cards bleed through / flicker from some angles; we set it per-flag. The distance-fade
    /// blend twin (`fade`) additionally forces depth-write ON (benilla's feather pass) regardless.
    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // The owned-palette skinning path (decision 0720): a mesh carrying the WOW joint
        // attributes (the skinned twin — `benilla_assets::build_skinned_submesh_mesh`) compiles
        // the WOW_RIG_SKIN vertex path, which skins from the shared buffer's palette region by
        // the instance's MeshTag rig field. The base mesh pipeline built the vertex buffer
        // layout without the joint attributes (they're not Bevy's, so it doesn't know them);
        // rebuild it with the same conditionals (mesh.rs `specialize`, locations 0-5) plus ours
        // at 10/11. Bevy's own SKINNED branch never fires for these meshes — that's the point.
        if layout.0.contains(benilla_assets::ATTRIBUTE_WOW_JOINT_INDEX) {
            descriptor.vertex.shader_defs.push("WOW_RIG_SKIN".into());
            let mut attrs = Vec::with_capacity(7);
            for (attr, loc) in [
                (Mesh::ATTRIBUTE_POSITION, 0),
                (Mesh::ATTRIBUTE_NORMAL, 1),
                (Mesh::ATTRIBUTE_UV_0, 2),
                (Mesh::ATTRIBUTE_UV_1, 3),
                (Mesh::ATTRIBUTE_TANGENT, 4),
                (Mesh::ATTRIBUTE_COLOR, 5),
            ] {
                if layout.0.contains(attr) {
                    attrs.push(attr.at_shader_location(loc));
                }
            }
            attrs.push(benilla_assets::ATTRIBUTE_WOW_JOINT_INDEX.at_shader_location(10));
            attrs.push(benilla_assets::ATTRIBUTE_WOW_JOINT_WEIGHT.at_shader_location(11));
            descriptor.vertex.buffers = vec![layout.0.get_layout(&attrs)?];
        }
        // M2 render flags 0x10 (no depth-write) / 0x08 (no depth-test), per batch. Default: write depth
        // (LEQUAL) like the real client — including transparent batches, which fixes the bleed-through.
        if let Some(ds) = descriptor.depth_stencil.as_mut() {
            ds.depth_write_enabled = !key.bind_group_data.no_depth_write;
            if key.bind_group_data.no_depth_test {
                ds.depth_compare = CompareFunction::Always;
            }
            // WMO authored batch order (see `WowModelKey::wmo_batch_order`): a constant pipeline depth
            // bias per batch index. The client resolves coplanar batches (wall + decal/trim layers) by
            // strict MOBA draw order under depth-write+LEQUAL; Bevy's draw order is batching-defined,
            // so we bias later batches to win the depth test instead. Bevy renders reverse-Z
            // (GreaterEqual): a POSITIVE constant raises the depth value = nearer = a later batch beats
            // an earlier coplanar one, in any draw order. One unit per index is the minimum resolvable
            // depth step — decisive for coplanar ties, negligible (sub-mm) against real separation.
            let order = key.bind_group_data.wmo_batch_order;
            if order != 0 {
                // DIAGNOSTIC (B38): `WOW_WMO_BIAS=0` drops the bias entirely, so the "both batches
                // saturate to depth 1.0" theory can be tested. The log proves the branch was taken.
                // `WOW_WMO_BIAS=0` drops the per-batch bias, the A/B that says whether a given
                // flicker is this bias resolving (or failing to resolve) a tie. It answered B38:
                // the flip is bit-identical with the bias gone, which also settles what the Metal
                // backend does with the constant — `wgpu-hal` hands `bias.constant` to
                // `setDepthBias` raw, and were that raw value ADDED to a reverse-Z depth (~0.02
                // here) every WMO batch would clamp to 1.0 and tie, so removing it could not
                // possibly leave the picture unchanged. It does, so the constant is being scaled by
                // the depth format's ULP and the comment above means what it says.
                let on = !matches!(std::env::var("WOW_WMO_BIAS").as_deref(), Ok("0"));
                ds.bias.constant = if on { i32::from(order) } else { 0 };
                debug!(
                    "wmo batch order {order}: bias {}  compare {:?}  depth_write {}",
                    ds.bias.constant, ds.depth_compare, ds.depth_write_enabled,
                );
            }
        }
        if key.bind_group_data.fade {
            // The distance-fade blend twin needs depth-write ON so near geometry occludes far within the
            // same fading model — force it regardless of the per-flag rule above.
            if let Some(ds) = descriptor.depth_stencil.as_mut() {
                ds.depth_write_enabled = true;
            }
        }
        if key.bind_group_data.clutter {
            // Ground clutter (AlphaMode::Mask → alpha-mask pass, depth-write already ON): force the
            // reference's over-blend on in place, so the ~70 yd ramp fades OPACITY instead of a hard
            // alpha-test cut. The 128/255 discard (crisp blade silhouette) + `tex × ramp` output alpha
            // are already produced in the shader; we only flip the blend state.
            if let Some(target) = descriptor
                .fragment
                .as_mut()
                .and_then(|f| f.targets.get_mut(0))
                .and_then(|t| t.as_mut())
            {
                target.blend = Some(BlendState::ALPHA_BLENDING);
            }
        }
        if key.bind_group_data.additive {
            // Additive glow cards: a PURE (ONE, ONE) add — the shader has already folded the
            // radial alpha into the colour IN GAMMA SPACE (wow_model.wgsl, decision 0160: a
            // hardware `SrcAlpha` factor would multiply after the linear conversion, inflating
            // every soft skirt by α^(1/2.2) — the fat hard-disc halo). Not Bevy's `AlphaMode::Add`
            // (its premultiply is linear-side too).
            if let Some(target) = descriptor
                .fragment
                .as_mut()
                .and_then(|f| f.targets.get_mut(0))
                .and_then(|t| t.as_mut())
            {
                target.blend = Some(BlendState {
                    color: BlendComponent {
                        src_factor: BlendFactor::One,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                    alpha: BlendComponent {
                        src_factor: BlendFactor::Zero,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                });
            }
        }
        let key = &key.bind_group_data;
        if key.modulate || key.modulate2x {
            // The MULTIPLY blends (decision 0528, byte-verified wow-re `m2-depth-blend-state`):
            // Mod (M2 mode 5 / WMO 4) = DST_COLOR/ZERO → out = src·dst; Mod2x (M2 6 / WMO 5) =
            // DST_COLOR/SRC_COLOR → out = 2·src·dst — the ARMORREFLECT weapon/armor sheen (neutral
            // at mid-grey, brightening at the streak). The framebuffer holds gamma (0161), so these
            // factors reproduce the reference's byte math exactly. Alpha: the equation reads no
            // source alpha — keep the destination's.
            if let Some(target) = descriptor
                .fragment
                .as_mut()
                .and_then(|f| f.targets.get_mut(0))
                .and_then(|t| t.as_mut())
            {
                target.blend = Some(BlendState {
                    color: BlendComponent {
                        src_factor: BlendFactor::Dst,
                        dst_factor: if key.modulate2x {
                            BlendFactor::Src
                        } else {
                            BlendFactor::Zero
                        },
                        operation: BlendOperation::Add,
                    },
                    alpha: BlendComponent {
                        src_factor: BlendFactor::Zero,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                });
            }
        }
        Ok(())
    }
}

/// Distant low-detail terrain (WDL): unlit white geometry fogged into the scene haze — the horizon
/// hills the reference draws beyond the streamed detailed tiles. Both shader stages are custom
/// (`shaders/wdl.wgsl`): white verts × the SAME planar-eye-Z fog as terrain (so WDL fades into the
/// identical haze), opaque (`AlphaMode::Opaque` ⇒ depth-LEQUAL + depth-write, no blend — the verified
/// WoW.8 state).
pub type WdlMaterial = ExtendedMaterial<StandardMaterial, WdlExt>;

/// WDL reads the scene fog straight off the shared global light — it has no per-material input of
/// its own (the band is white geometry whose entire colour is the fog).
#[derive(Asset, AsBindGroup, Clone, TypePath)]
pub struct WdlExt {
    /// **The shared global light** (`lighting::global_light`), the same buffer terrain and the models
    /// bind. `wdl.wgsl` reads rows 4/5 only — the SCENE fog (block 1) plus the farclip wall, which is
    /// right by construction: the band *is* the horizon, so it is never inside a WMO. Set once at
    /// startup, never mutated (this replaces the per-material `apply_wow_lighting` push).
    #[storage(90, read_only, buffer)]
    pub light_buf: Buffer,
}

impl MaterialExtension for WdlExt {
    fn vertex_shader() -> ShaderRef {
        "shaders/wdl.wgsl".into()
    }
    fn fragment_shader() -> ShaderRef {
        "shaders/wdl.wgsl".into()
    }
}

/// MCLQ liquid surfaces (lakes/rivers/ocean): a port of the reference's `ocean0_s.bls` shader path.
/// The animated frame set is a `texture_2d_array` whose RGB is near-black and whose **ALPHA is the
/// ripple** (→ surface transparency); the visible colour is the dark texture **hardware-lit**
/// (`ambient + N·L·sun`) and **fog-tinted by distance** — NOT a flat tint (the blue/teal is the blue
/// ambient + fog). Two-sided, alpha-blended, depth-write off (Bevy's transparent pass = the verified
/// WoW state). P1 omits the unit-1 detail ripple + specular term.
pub type LiquidMaterial = ExtendedMaterial<StandardMaterial, LiquidExt>;

/// Liquid material inputs: the animated frame array, the per-material kind lanes, and the current
/// animation frame index. Everything that is *light* — ambient/diffuse/sun/spec, both water swatches
/// and both fog blocks — comes off the shared global-light buffer, exactly like terrain and the
/// models; this material carries only what genuinely varies per material. **The Vec4 uniforms merge
/// onto one binding (102)** (same packing trick as the other extensions). WGSL struct order must
/// match the field order.
#[derive(Asset, AsBindGroup, Clone, TypePath)]
pub struct LiquidExt {
    /// The kind's animated frames (`lake_a`/`fast_a`/`ocean_h`), stacked as `2d_array` layers
    /// (`Rgba8Unorm`, repeat-sampled). RGB near-black; alpha = the ripple/wave → transparency.
    #[texture(100, dimension = "2d_array", visibility(fragment))]
    #[sampler(101, visibility(fragment))]
    pub frames: Handle<Image>,
    /// The per-material constants that pick which *lanes* of the shared light this surface reads.
    /// None of them is a light value; all four are fixed at material creation.
    ///
    /// - `x` = **fullbright** (>0.5 ⇒ magma/slime): the animated texture IS the opaque body — skip
    ///   the depth swatch and the N·L term. Not "skip the fog"; see `liquid.wgsl` and decision 0691.
    /// - `y` = **ocean** (>0.5): read the ocean water swatch (shared-light rows 15/16, `Light.dbc`
    ///   IntBand 14/15) instead of the river/lake one (rows 13/14, IntBand 16/17).
    /// - `z` = **interior fog** (>0.5): this surface is a WMO *interior* group's own liquid, so it
    ///   fogs with the interior block (rows 18/19) like the walls around it, not the scene fog. The
    ///   reference gates the WMO liquid pass's fog submit (`0x6b6323`–`0x6b6342`) on exactly the same
    ///   `[0xca7f00]` as the WMO *geometry* pass (`0x6b51d9`/`0x6b51ea`) — one flag, so the pool and
    ///   the room can never disagree. ADT liquid is 0: the ADT pass submits no fog of its own and
    ///   draws under the once-a-frame scene submit.
    /// - `w` = the sun-sheen **shininess** (`lighting::WATER_SHININESS`) for the `ocean0_s.bls`
    ///   `secondary` Blinn term — water's own exponent, not the shared row-3 terrain shininess.
    #[uniform(102)]
    pub kind: Vec4,
    /// `x` = current frame index (set each frame by `animate_liquid`), `y` = frame count; `zw` unused.
    #[uniform(102)]
    pub anim: Vec4,
    /// **The shared global light** (`lighting::global_light`): the one storage buffer terrain and the
    /// models already read, now liquid's source too. `liquid.wgsl` reads rows 0-5 (light + scene fog +
    /// farclip), 13-16 (the two water swatches) and 18/19 (the interior fog block). Read in BOTH
    /// stages — the vertex stage evaluates the faithful per-vertex sun sheen. Set once at material
    /// creation, never mutated.
    #[storage(90, read_only, buffer, visibility(vertex, fragment))]
    pub light_buf: Buffer,
}

impl MaterialExtension for LiquidExt {
    fn vertex_shader() -> ShaderRef {
        "shaders/liquid.wgsl".into()
    }
    fn fragment_shader() -> ShaderRef {
        "shaders/liquid.wgsl".into()
    }
}

/// Per-tile splat inputs: a `texture_2d_array` of the tile's ground textures, a `texture_2d_array`
/// of its per-chunk alpha maps, and the tiling factor. Which array layers a fragment blends comes
/// from the merged mesh's vertex `COLOR` (4 layer indices) and `UV1.x` (alpha index).
///
/// **One shared sampler** (repeating, on `layer_array`) covers both arrays: `StandardMaterial`
/// already uses ~6 of Metal's 16 fragment samplers and the view adds more, so extra samplers risk
/// the per-stage limit. Layer UVs are tiled; the alpha map is sampled in 0..1 where repeat == clamp.
/// **All terrain Vec4 uniforms merge onto one binding (106).** AsBindGroup packs multiple
/// `#[uniform(N)]` fields at the same `N` into a single buffer entry; the WGSL declares one
/// `var<uniform> t: TerrainParams;` whose fields land in the SAME order as the Rust declaration
/// (each Vec4 is 16 bytes, no padding). One buffer entry per pipeline stage instead of eleven —
/// which is what keeps us under Metal's 16-buffer vertex-stage cap once Step 5's fog uniforms are
/// added. Visibility-narrowing via `visibility(fragment)` doesn't work on uniforms (Bevy 0.18
/// hardcodes them to all stages), so packing is the effective lever for the buffer count.
///
/// Textures + sampler ARE fragment-narrowable (the macro respects `visibility(fragment)` there)
/// so the splat / alpha / shadow arrays stay fragment-only.
#[derive(Asset, AsBindGroup, Clone, TypePath)]
pub struct TerrainExtension {
    #[texture(100, dimension = "2d_array", visibility(fragment))]
    #[sampler(105, visibility(fragment))]
    pub layer_array: Handle<Image>,
    #[texture(104, dimension = "2d_array", visibility(fragment))]
    pub alpha_array: Handle<Image>,
    /// Per-chunk MCSH baked shadow maps (one R8 layer each, `R` = shadowed). The merged mesh's
    /// `UV1.y` carries the layer index, or `-1` for a chunk with no shadow map. Shares sampler 105.
    #[texture(110, dimension = "2d_array", visibility(fragment))]
    pub shadow_array: Handle<Image>,

    // ------------------------------------------------------------------------------------------
    // Packed uniform buffer at binding 106 — fields below appear in the WGSL `TerrainParams` struct
    // in the SAME order (each Vec4 16 bytes, no padding). Reordering is a breaking change.
    // ------------------------------------------------------------------------------------------
    /// `x` = texture tiling factor (repeats per chunk); other lanes unused.
    #[uniform(106)]
    pub params: Vec4,

    /// **The shared global light** (`lighting::global_light`): one persistent storage buffer all
    /// materials reference, updated once/frame in place — the faithful replacement for the old
    /// per-material light/fog uniforms (ambient/diffuse/sun/spec + fog) that `apply_wow_lighting`
    /// re-pushed every frame (re-creating every bind group). Set once at tile spawn, never mutated.
    /// `terrain.wgsl` reads it as `var<storage, read> wow_light` (rows 0-5: light + fog + farclip).
    #[storage(90, read_only, buffer)]
    pub light_buf: Buffer,
}

impl MaterialExtension for TerrainExtension {
    // Custom VERTEX shader too (not just fragment): the sun specular is computed per-vertex (Q14), and
    // Bevy's `VertexOutput` has no slot to carry the interpolated result.
    fn vertex_shader() -> ShaderRef {
        "shaders/terrain.wgsl".into()
    }
    fn fragment_shader() -> ShaderRef {
        "shaders/terrain.wgsl".into()
    }
}
