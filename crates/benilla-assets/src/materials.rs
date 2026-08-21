//! **The render materials** — the four `ExtendedMaterial`s a WoW 1.12.1 world is drawn with
//! (terrain splat, M2/WMO model, WDL far band, liquid surface) and the WGSL each one binds.
//!
//! They live *below* the renderer, in the crate that turns WoW files into GPU resources, because
//! that is what they are: the thing an `.m2` submesh or an `.adt` chunk BECOMES. Decision 1164 —
//! the material type is the value type of the shared dedup cache (`WorldAssets::model_materials`),
//! and 26 gameplay files name `WowModelMaterial` for spell effects, equipment and portraits with
//! no terrain streamer anywhere in sight. Verified before the move: this file names nothing from
//! the client at all — its whole import surface is `bevy::*` — so it is a leaf that belongs here,
//! not a piece of the renderer dragged down with the caches.
//!
//! **The WGSL is embedded, not served.** `shaders/*.wgsl` next to this file are compiled in via
//! [`register_shaders`] and addressed as `embedded://benilla_assets/shaders/…`. A relative
//! `"shaders/x.wgsl"` would resolve against whatever `AssetPlugin::file_path` the *host binary*
//! happened to set, so a second program standing on this crate would get a material that silently
//! renders nothing — the same trap `boot.rs` already documents for the capture harness. Embedding
//! makes the crate answer for its own shaders.
//!
//! Phase 6 terrain splat material: an [`ExtendedMaterial`] over `StandardMaterial` that blends up
//! to 4 tiled layer textures by a packed alpha map, keeping PBR lighting/shadows/fog.
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
    AsBindGroup, BlendComponent, BlendFactor, BlendOperation, BlendState, Buffer, ColorWrites,
    CompareFunction, RenderPipelineDescriptor, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

/// Compile the four WGSL files into the binary and register them under
/// `embedded://benilla_assets/shaders/…`. Call **after** Bevy's `AssetPlugin` (it fills the
/// registry that plugin creates); [`crate::register_asset_loaders`] already does.
pub fn register_shaders(app: &mut App) {
    bevy::asset::embedded_asset!(app, "shaders/terrain.wgsl");
    bevy::asset::embedded_asset!(app, "shaders/wow_model.wgsl");
    bevy::asset::embedded_asset!(app, "shaders/wdl.wgsl");
    bevy::asset::embedded_asset!(app, "shaders/liquid.wgsl");
}

/// The WDL far-band shader's source, for the law tests that live beside the renderer rather than
/// beside the material (`wdl.rs`). Exported instead of letting a sibling crate `include_str!` its
/// way across the workspace by relative path.
pub const WDL_WGSL: &str = include_str!("shaders/wdl.wgsl");

/// Alpha-test reference for `Blend_AlphaKey` (M2 blend mode 1) materials — the value below which a
/// fragment is discarded (`D3DCMP_GREATEREQUAL`). Per wowdev.wiki M2/Rendering this is **version
/// dependent**: `224/255 ≈ 0.878` on **≤ WotLK** (our target is 1.12.1 build 5875), vs `128/255 ≈
/// 0.5` on Cata+. We initially hardcoded the Cata value (0.5), which left foliage too dense and a
/// white fringe on cutout edges. The spec multiplies this by the element's animated alpha (1.0 for
/// static doodads), so the bare constant is correct until we add doodad alpha fades.
/// Source: <https://wowdev.wiki/M2/Rendering> § Alpha Testing.
pub const VANILLA_ALPHA_KEY_REF: f32 = 224.0 / 255.0;

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
///   cross-quads occlude its far ones. Silhouette stable for an AlphaKey source (the shader re-applies
///   the 224/255 cutout via `clutter_fade.z` bit 10 — an Opaque source never alpha-tests; 0842).
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
    /// Depth-prime twin (`clutter_fade.z` bit 9 — `model_render::zfill_material`, the reference's
    /// `M2UseZFill` clone command, wow-re `m2-blend-promotion-zfill.md` §4): `specialize` masks the
    /// colour writes off, turns blend off, and forces depth-write ON.
    zfill: bool,
    /// Far side of the water plane (`clutter_fade.z` bit 11 — `model_render`'s far twin, the
    /// water-plane interleave's mesh lane): the material's huge negative SORT bias must stay
    /// sort-only, so `specialize` zeroes the rasterizer `DepthBiasState` constant the base
    /// `StandardMaterial` derived from the same field (at −4e4 that constant is a ~0.5% relative
    /// depth pull — enough to clip a blade's coplanar sheen/glow layers behind the blade itself).
    far_side: bool,
    /// The **WMO-skybox lane** (`clutter_fade.z` bit 13 — `model_render::SKY_DEPTH_MARKER`): the
    /// building-owned painted sky, drawn as the ordinary M2 it is. `specialize` compiles the
    /// `WOW_SKY_DEPTH` fragment def, which emits `@builtin(frag_depth) = 0.0` — reverse-Z far, the
    /// sky depth law every other sky shader already obeys (`benilla_world::sky_order`). It is a key
    /// axis because writing `frag_depth` costs the whole pipeline its early-Z, and the model lane
    /// draws every doodad and wall in the frame; only this one camera-anchored model may pay it.
    /// Like [`Self::far_side`] it also zeroes the rasterizer bias constant: the lane's rung is a
    /// SORT rung (`sky_order::WMO_SKYBOX_BIAS`, ~−6e4), and the depth it writes is a constant the
    /// rasterizer must not be perturbing behind the shader's back.
    sky_depth: bool,
    // NB: the WMO authored batch order is deliberately NOT a key axis. It used to be (a
    // per-batch-index `DepthBiasState` constant), which made every batch index its own pipeline —
    // the city first-sight compile stall (decision 0837). The coplanar-layering nudge now rides
    // `sun_scale.y` into `wow_model.wgsl`'s vertex stage as uniform data; `model_render::MatKey`
    // still dedups materials per order, so per-batch identity is intact.
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
            zfill: markers & 0x200 != 0,
            far_side: markers & 0x800 != 0,
            sky_depth: markers & 0x2000 != 0,
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
    /// the distance-fade BLEND twin → `specialize` forces depth-write ON; the 224/255 stable-silhouette
    /// cutout rides `clutter_fade.z` bit 10 separately, set only for AlphaKey sources — 0842; see
    /// [`WowModelKey`]). `zw` reserved.
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
    /// `doodad_anim::tick_anim_materials` for texanim batches (flowing waterfalls).
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
    /// The shared mat-anim TABLE slots (decision 1381): `x` = the UV-scroll slot + 1, `y` = the
    /// animated-tint slot + 1 — `0` = not table-animated, and the shader uses the static lanes
    /// (`sun_scale.zw` / `tint.xyz`) exactly as before. Baked when the batch registers its
    /// sampler; the per-frame samples live in the shared light buffer's `matanim` region, so an
    /// animating material is never mutated again (no per-frame `Modified`, no bind-group
    /// rebuild, no whole-population `AssetChanged` walks — B131's chain, severed at the root).
    /// `zw` free.
    #[uniform(100)]
    pub anim_slots: Vec4,
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
        "embedded://benilla_assets/shaders/wow_model.wgsl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "embedded://benilla_assets/shaders/wow_model.wgsl".into()
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
        // attributes (the skinned twin — `crate::build_skinned_submesh_mesh`) compiles
        // the WOW_RIG_SKIN vertex path, which skins from the shared buffer's palette region by
        // the instance's MeshTag rig field. The base mesh pipeline built the vertex buffer
        // layout without the joint attributes (they're not Bevy's, so it doesn't know them);
        // rebuild it with the same conditionals (mesh.rs `specialize`, locations 0-5) plus ours
        // at 10/11. Bevy's own SKINNED branch never fires for these meshes — that's the point.
        if layout.0.contains(crate::ATTRIBUTE_WOW_JOINT_INDEX) {
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
            attrs.push(crate::ATTRIBUTE_WOW_JOINT_INDEX.at_shader_location(10));
            attrs.push(crate::ATTRIBUTE_WOW_JOINT_WEIGHT.at_shader_location(11));
            descriptor.vertex.buffers = vec![layout.0.get_layout(&attrs)?];
        }
        // The merged fader blob (decisions 1418/1420): a mesh carrying the per-vertex fade
        // sphere compiles the WOW_MERGED_FADE path — the faithful doodad fade curve computed
        // per vertex (folded into the tag fade the fragment already consumes) and a clip-space
        // collapse at zero (the Hidden channel). The blob's material is its blend TWIN, so the
        // feather is the reference's own translucent ramp. Same rebuild pattern as the joints
        // above; the two attribute sets never co-occur (merged blobs never skin).
        if layout.0.contains(crate::ATTRIBUTE_WOW_FADE_SPHERE) {
            descriptor.vertex.shader_defs.push("WOW_MERGED_FADE".into());
            if let Some(fragment) = descriptor.fragment.as_mut() {
                fragment.shader_defs.push("WOW_MERGED_FADE".into());
            }
            let mut attrs = Vec::with_capacity(6);
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
            attrs.push(crate::ATTRIBUTE_WOW_FADE_SPHERE.at_shader_location(12));
            // The interior-prop half (1418 lane 3): the baked SH-probe slot replaces the
            // per-entity MeshTag payload.
            if layout.0.contains(crate::ATTRIBUTE_WOW_MERGED_SLOT) {
                descriptor.vertex.shader_defs.push("WOW_MERGED_SLOT".into());
                if let Some(fragment) = descriptor.fragment.as_mut() {
                    fragment.shader_defs.push("WOW_MERGED_SLOT".into());
                }
                attrs.push(crate::ATTRIBUTE_WOW_MERGED_SLOT.at_shader_location(13));
            }
            descriptor.vertex.buffers = vec![layout.0.get_layout(&attrs)?];
        }
        // M2 render flags 0x10 (no depth-write) / 0x08 (no depth-test), per batch. Default: write depth
        // (LEQUAL) like the real client — including transparent batches, which fixes the bleed-through.
        if let Some(ds) = descriptor.depth_stencil.as_mut() {
            ds.depth_write_enabled = !key.bind_group_data.no_depth_write;
            if key.bind_group_data.no_depth_test {
                ds.depth_compare = CompareFunction::Always;
            }
        }
        // A far-side-of-water twin's bias is a SORT rung only (the water-plane interleave,
        // `sky_order::FAR_SIDE_BIAS`): the base `StandardMaterial::specialize` has just packed the
        // same f32 into the rasterizer `DepthBiasState` constant (bevy 0.18 `pbr_material.rs`,
        // `depth_stencil.bias.constant`), where −4e4 ULPs would pull every far fragment ~0.5%
        // deeper and clip a blade's coplanar sheen/glow against the blade's own opaque depth.
        // Zero it back — the effect lane splits sort from raster by construction; this bit is the
        // mesh lane's split.
        if key.bind_group_data.far_side {
            if let Some(ds) = descriptor.depth_stencil.as_mut() {
                ds.bias.constant = 0;
            }
        }
        // The WMO-skybox lane: force the sky's far depth in the fragment, and — like the far-side
        // twin above — keep its big negative rung sort-only. The rung (`sky_order::WMO_SKYBOX_BIAS`)
        // exists to sink a camera-anchored backdrop under every world transparent; as a rasterizer
        // constant it would be perturbing an interpolated depth this pipeline discards anyway.
        if key.bind_group_data.sky_depth {
            if let Some(fragment) = descriptor.fragment.as_mut() {
                fragment.shader_defs.push("WOW_SKY_DEPTH".into());
            }
            if let Some(ds) = descriptor.depth_stencil.as_mut() {
                ds.bias.constant = 0;
            }
        }
        // The WMO authored-batch-order depth nudge (the coplanar MOBA layering determinism) is NOT
        // here any more: as a fixed-function `DepthBiasState` constant it made every batch index its
        // own PIPELINE — Stormwind alone queued ~3000 variants of this one shader, each a
        // synchronous render-thread compile on macOS (the city first-sight stall, decision 0837).
        // The nudge now lives in `wow_model.wgsl`'s vertex stage, an exact relative scale of clip z
        // driven by `sun_scale.y` (uniform DATA, no pipeline axis) — same one-ULP-per-index
        // semantics, byte-verified intent unchanged (wow-5875-re wmo-batch-blend-depth-state.md).
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
        if key.bind_group_data.zfill {
            // The depth-prime twin (`model_render::zfill_material` — the reference's `M2UseZFill`
            // clone, wow-re `m2-blend-promotion-zfill.md` §4): a translucent model's z-writing
            // batches draw once colour-masked-off, blend-off, z-write ON, sorted before its colour
            // batches (the material's negative sort bias) — so each colour fragment passes
            // GreaterEqual only at the model's own nearest surface, and interior/overlapped layers
            // fail. One blended layer everywhere: no self-overlap darkening on a stealthed body.
            // The fragment still runs its discards (farclip, the bit-10 twin cutout);
            // its colour output is computed and masked (an early return would be the natural
            // spelling, but naga's MSL backend miscompiles the dead tail — see the shader note).
            if let Some(frag) = descriptor.fragment.as_mut() {
                if let Some(target) = frag.targets.get_mut(0).and_then(|t| t.as_mut()) {
                    target.blend = None;
                    target.write_mask = ColorWrites::empty();
                }
            }
            if let Some(ds) = descriptor.depth_stencil.as_mut() {
                ds.depth_write_enabled = true;
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
        "embedded://benilla_assets/shaders/wdl.wgsl".into()
    }
    fn fragment_shader() -> ShaderRef {
        "embedded://benilla_assets/shaders/wdl.wgsl".into()
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
    /// - `x` = **which renderer** this surface belongs to (`liquid::surface::LiquidPath`): `0` = ADT
    ///   MCLQ (`ocean0_s.bls`), `1` = WMO exterior (`MapObjExtWater0.bls`), `2` = WMO interior
    ///   (fixed-function, unlit). The reference has three liquid renderers with genuinely different
    ///   combines, stage counts and opacity sources; this is which one `liquid.wgsl` runs.
    /// - `y`/`z`/`w` reserved.
    #[uniform(102)]
    pub path: Vec4,
    /// `x` = reserved (frame 0), `y` = frame count, `z` = scroll flag, `w` = clock enable —
    /// the shader derives frame index and scroll from `globals.time` (liquid.wgsl `anim_time`);
    /// nothing mutates this uniform after build.
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
        "embedded://benilla_assets/shaders/liquid.wgsl".into()
    }
    fn fragment_shader() -> ShaderRef {
        "embedded://benilla_assets/shaders/liquid.wgsl".into()
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
        "embedded://benilla_assets/shaders/terrain.wgsl".into()
    }
    fn fragment_shader() -> ShaderRef {
        "embedded://benilla_assets/shaders/terrain.wgsl".into()
    }
}

#[cfg(test)]
mod tests {
    /// The sky depth law, for the one sky element that draws on the MODEL lane: the WMO skybox
    /// ([`WowModelKey::sky_depth`]). Every other sky shader is checked the same way, together, in
    /// `benilla_world::sky_order::every_sky_shader_forces_the_far_depth` — this half lives here
    /// because the shader does. Without it a skybox silently goes back to being occluded by its own
    /// 94-yard shell radius instead of by world geometry (the regression decision 0588 fixed).
    #[test]
    fn the_sky_lane_forces_the_far_depth() {
        let src = include_str!("shaders/wow_model.wgsl");
        assert!(
            src.contains("#ifdef WOW_SKY_DEPTH\n    out.depth = 0.0;\n#endif"),
            "the model lane's sky branch no longer forces the far depth — a WMO skybox's shell \
             radius is deciding occlusion again (benilla_world::sky_order, \"The depth law\")"
        );
        // The declaration must stay BEHIND the ifdef: writing `frag_depth` unconditionally costs
        // every doodad, creature and wall in the frame its early-Z, and this lane draws all of
        // them. Matched with its guard attached, so ungating it fails here rather than in a
        // frame-time regression nobody attributes to this file. (Counting occurrences instead
        // would trip over the prose above the struct, which names the builtin too.)
        assert!(
            src.contains("#ifdef WOW_SKY_DEPTH\n    // Reverse-Z \"infinitely far\"",)
                && src.contains("    @builtin(frag_depth) depth: f32,\n#endif"),
            "the model lane's sky output no longer declares frag_depth behind WOW_SKY_DEPTH — \
             either the sky writes no depth, or every model draw just lost its early-Z"
        );
    }
}
