//! The **effect lane** — the render-world half of the shared effect stream (decisions 0732
//! P1/P2, 0733): one vertex buffer + one per-frame index stream for every dynamic-effect
//! family, drawn as directly-constructed [`Transparent3d`] items.
//!
//! The shape is bevy_ui_render's (`draw_indexed(range, 0, 0..1)` over a shared `RawBufferVec`,
//! and its sorted-items batching walk), moved to the 3-D transparent phase:
//!
//! - **extract** copies the frame's CPU stream;
//! - **queue** adds one item per draw record with **sort distance = view-space z of the draw
//!   anchor + the ladder rung** — exactly the metric the material path produced
//!   (`rangefinder.distance(center) + depth_bias`, bevy_pbr `material.rs:1307`), so ordering
//!   against M2 blend batches, model-particle instances and the sky ladder is unchanged by
//!   construction;
//! - **prepare** (after `PhaseSort`) rebases each draw's vertices against its target view's
//!   camera position (0733 §2 — the `ViewUniform.world_position` source value, so the shader's
//!   reconstruction is exact), uploads them, then walks the sorted items building the frame's
//!   index stream in draw order and **merging sort-adjacent items that share (pipeline,
//!   texture, light, fog)** into one draw call. The merge rides bevy's own sorted-phase
//!   contract: the phase renderer advances by `batch_range.len()` (`render_phase/mod.rs:1487`),
//!   so a batch-opening item whose range spans its followers absorbs them — bevy_ui_render's
//!   exact mechanism. Bevy's mesh batcher leaves items whose main entity owns no registered
//!   mesh untouched (0732 audit III), so nothing else rewrites the ranges.
//!
//! Pipeline variants (0733 §4): Add = premultiplied-alpha + the shader's gamma `rgb·a` fold,
//! Alpha = standard, Opaque = no blend with depth-write ON (in the transparent bracket at the
//! owner rung — the 0719 reference law), Multiply = bevy's `AlphaMode::Multiply` state (the
//! blob shadow's modulate; `ModelBlend::Mod` at α=1), Mod2x = 0528's `(Dst, Src)`. The decal
//! family's rasterizer depth bias rides [`EffectPipelineKey::raster_bias`] — the half of the
//! old material `depth_bias` that settles the coplanar depth tie against the drawn ground.

use std::ops::Range;

use bevy::asset::{AssetEvent, AssetId};
use bevy::core_pipeline::core_3d::{Transparent3d, CORE_3D_DEPTH_FORMAT};
use bevy::ecs::system::lifetimeless::{Read, SRes};
use bevy::ecs::system::SystemParamItem;
use bevy::image::BevyDefault as _;
use bevy::mesh::VertexBufferLayout;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_phase::{
    AddRenderCommand, DrawFunctions, PhaseItem, PhaseItemExtraIndex, RenderCommand,
    RenderCommandResult, SetItemPipeline, TrackedRenderPass, ViewSortedRenderPhases,
};
use bevy::render::render_resource::binding_types::{
    sampler, storage_buffer_read_only_sized, texture_2d, uniform_buffer,
};
use bevy::render::render_resource::{
    BindGroup, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries, BlendComponent,
    BlendFactor, BlendOperation, BlendState, Buffer, BufferId, BufferUsages, ColorTargetState,
    ColorWrites, CompareFunction, DepthBiasState, DepthStencilState, DynamicUniformBuffer,
    FragmentState, IndexFormat, MultisampleState, PipelineCache, PrimitiveState, RawBufferVec,
    RenderPipelineDescriptor, SamplerBindingType, ShaderStages, SpecializedRenderPipeline,
    SpecializedRenderPipelines, StencilState, TextureFormat, TextureSampleType, VertexFormat,
    VertexState, VertexStepMode,
};
use bevy::render::renderer::{RenderDevice, RenderQueue};
use bevy::render::sync_world::MainEntity;
use bevy::render::texture::GpuImage;
use bevy::render::view::{
    ExtractedView, Msaa, ViewTarget, ViewUniform, ViewUniformOffset, ViewUniforms,
};
use bevy::render::{Extract, ExtractSchedule, Render, RenderApp, RenderStartup, RenderSystems};
use bevy::shader::Shader;

use crate::lighting::SharedLightBuffer;

use super::buffer::{EffectBlend, EffectFog, EffectQuads, EffectTopology, EffectVertex};

/// One extracted draw: the render-world copy of a [`super::buffer::EffectDraw`].
struct ExtractedDraw {
    cam: Entity,
    main_entity: Entity,
    texture: AssetId<Image>,
    blend: EffectBlend,
    topology: EffectTopology,
    fog: EffectFog,
    anchor: Vec3,
    bias: f32,
    raster_bias: i32,
    /// Vertex range in the shared stream.
    range: Range<u32>,
    /// A booth's scene-light override (0539 §5); `None` = the world's shared light buffer.
    light: Option<Buffer>,
}

/// One GPU draw call after the merge walk: a contiguous index range plus the bind-group
/// identity every item folded into it shares.
struct MergedDraw {
    index_range: Range<u32>,
    texture: AssetId<Image>,
    light: Option<Buffer>,
    fog: EffectFog,
}

/// The lane's per-frame GPU state: the shared vertex stream (rebased camera-relative in
/// prepare), the frame's index stream (built in sorted-item order by the merge walk), the
/// draw and merged-draw records, and the canonical fog-params uniform (one `vec4` per
/// [`EffectFog`] policy, written once).
#[derive(Resource)]
pub struct EffectMeta {
    vertices: RawBufferVec<EffectVertex>,
    indices: RawBufferVec<u32>,
    draws: Vec<ExtractedDraw>,
    merged: Vec<MergedDraw>,
    view_bind_group: Option<BindGroup>,
    params: DynamicUniformBuffer<Vec4>,
    params_offsets: Option<[u32; 6]>,
}

impl Default for EffectMeta {
    fn default() -> Self {
        Self {
            vertices: RawBufferVec::new(BufferUsages::VERTEX),
            indices: RawBufferVec::new(BufferUsages::INDEX),
            draws: Vec::new(),
            merged: Vec::new(),
            view_bind_group: None,
            params: DynamicUniformBuffer::default(),
            params_offsets: None,
        }
    }
}

/// Per-(texture, light-buffer) bind groups (texture + sampler + light blob + params uniform),
/// cached across frames — invalidated by that image's asset events, mirroring bevy_ui_render's
/// cache. The light key is `None` for the world's shared buffer (startup-created, never
/// re-created, so it cannot stale a cached group) or a booth scene blob's id (0539 §5).
#[derive(Resource, Default)]
pub struct EffectBindGroups {
    images: HashMap<(AssetId<Image>, Option<BufferId>), BindGroup>,
}

/// The lane's pipeline: layouts + shader, specialized per (blend, raster bias, msaa, hdr).
#[derive(Resource)]
pub struct EffectPipeline {
    view_layout: BindGroupLayoutDescriptor,
    image_layout: BindGroupLayoutDescriptor,
    shader: Handle<Shader>,
}

pub fn init_effect_pipeline(mut commands: Commands, asset_server: Res<AssetServer>) {
    let view_layout = BindGroupLayoutDescriptor::new(
        "effect_view_layout",
        &BindGroupLayoutEntries::single(
            ShaderStages::VERTEX_FRAGMENT,
            uniform_buffer::<ViewUniform>(true),
        ),
    );
    let image_layout = BindGroupLayoutDescriptor::new(
        "effect_image_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                // The shared global light/fog blob (`lighting::global_light`) — sized at bind
                // time; the WGSL struct pins the layout.
                storage_buffer_read_only_sized(false, None),
                // The per-draw fog-params `vec4`, dynamic-offset into the canonical rows.
                uniform_buffer::<Vec4>(true),
            ),
        ),
    );
    commands.insert_resource(EffectPipeline {
        view_layout,
        image_layout,
        shader: asset_server.load("shaders/wow_effect.wgsl"),
    });
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct EffectPipelineKey {
    samples: u32,
    hdr: bool,
    /// The draw's blend — the whole state-variant axis (0733 §4).
    blend: EffectBlend,
    /// The rasterizer depth-bias constant (the decal family's coplanarity settle; 0 for
    /// free-floating geometry). Three values exist ({0, 4096, 8192}), so the key space stays
    /// small.
    raster_bias: i32,
}

impl SpecializedRenderPipeline for EffectPipeline {
    type Key = EffectPipelineKey;

    fn specialize(&self, key: Self::Key) -> RenderPipelineDescriptor {
        let vertex_layout = VertexBufferLayout::from_vertex_formats(
            VertexStepMode::Vertex,
            vec![
                // position (camera-relative — prepare rebased it; 0733 §2)
                VertexFormat::Float32x3,
                // uv
                VertexFormat::Float32x2,
                // color (raw authored gamma RGBA)
                VertexFormat::Float32x4,
            ],
        );
        // The blend states each family's material carried (0733 §4): Add/Alpha/Opaque are the
        // P1 trio; Multiply is bevy's own `AlphaMode::Multiply` state (mesh.rs:2486 — the blob
        // shadow's `dst·lerp(1, src, α)` with the shader-side premultiply); Mod2x is 0528's
        // `(Dst, Src)` = `2·src·dst` (rain's verified state, ARMORREFLECT's law).
        let (blend, depth_write, blend_def) = match key.blend {
            EffectBlend::Add => (
                Some(BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                false,
                "BLEND_ADD",
            ),
            EffectBlend::Alpha => (Some(BlendState::ALPHA_BLENDING), false, "BLEND_ALPHA"),
            EffectBlend::Opaque => (None, true, "BLEND_OPAQUE"),
            EffectBlend::Multiply => (
                Some(BlendState {
                    color: BlendComponent {
                        src_factor: BlendFactor::Dst,
                        dst_factor: BlendFactor::OneMinusSrcAlpha,
                        operation: BlendOperation::Add,
                    },
                    alpha: BlendComponent::OVER,
                }),
                false,
                "BLEND_MULTIPLY",
            ),
            EffectBlend::Mod2x => (
                Some(BlendState {
                    color: BlendComponent {
                        src_factor: BlendFactor::Dst,
                        dst_factor: BlendFactor::Src,
                        operation: BlendOperation::Add,
                    },
                    alpha: BlendComponent {
                        src_factor: BlendFactor::Zero,
                        dst_factor: BlendFactor::One,
                        operation: BlendOperation::Add,
                    },
                }),
                false,
                "BLEND_MOD2X",
            ),
        };
        let mut shader_defs = vec![blend_def.into()];
        // `$WOW_PARTICLE_FLAT` — the fragment-input A/B (B16): solid magenta, no inputs.
        if std::env::var_os("WOW_PARTICLE_FLAT").is_some() {
            shader_defs.push("WOW_PARTICLE_FLAT".into());
        }
        // `$WOW_PARTICLE_NODEPTH` — the occlusion A/B (B16): force the depth COMPARE to
        // `Always`, splitting "nothing is emitted" from "emitted and the depth buffer eats it".
        let depth_compare = if std::env::var_os("WOW_PARTICLE_NODEPTH").is_some() {
            CompareFunction::Always
        } else {
            CompareFunction::GreaterEqual
        };
        RenderPipelineDescriptor {
            label: Some("wow_effect_pipeline".into()),
            layout: vec![self.view_layout.clone(), self.image_layout.clone()],
            vertex: VertexState {
                shader: self.shader.clone(),
                shader_defs: shader_defs.clone(),
                buffers: vec![vertex_layout],
                ..default()
            },
            fragment: Some(FragmentState {
                shader: self.shader.clone(),
                shader_defs,
                targets: vec![Some(ColorTargetState {
                    format: if key.hdr {
                        ViewTarget::TEXTURE_FORMAT_HDR
                    } else {
                        TextureFormat::bevy_default()
                    },
                    blend,
                    write_mask: ColorWrites::ALL,
                })],
                ..default()
            }),
            primitive: PrimitiveState {
                cull_mode: None, // billboards, trails, decals: never backface-cull
                ..default()
            },
            depth_stencil: Some(DepthStencilState {
                format: CORE_3D_DEPTH_FORMAT,
                depth_write_enabled: depth_write,
                depth_compare,
                stencil: StencilState::default(),
                bias: DepthBiasState {
                    constant: key.raster_bias,
                    slope_scale: 0.0,
                    clamp: 0.0,
                },
            }),
            multisample: MultisampleState {
                count: key.samples,
                ..default()
            },
            ..default()
        }
    }
}

/// Copy the main world's frame stream into the lane, and drop bind-group cache entries for
/// images that changed (the UI cache-invalidation shape).
fn extract_effects(
    mut meta: ResMut<EffectMeta>,
    mut bind_groups: ResMut<EffectBindGroups>,
    quads: Extract<Res<EffectQuads>>,
    mut image_events: Extract<MessageReader<AssetEvent<Image>>>,
) {
    for event in image_events.read() {
        match event {
            AssetEvent::Added { id }
            | AssetEvent::Modified { id }
            | AssetEvent::Removed { id }
            | AssetEvent::Unused { id } => {
                bind_groups.images.retain(|(image, _), _| image != id);
            }
            AssetEvent::LoadedWithDependencies { .. } => {}
        }
    }
    meta.vertices.clear();
    meta.vertices.extend(quads.verts.iter().copied());
    meta.draws.clear();
    meta.draws.extend(quads.draws.iter().map(|d| ExtractedDraw {
        cam: d.cam,
        main_entity: d.main_entity,
        texture: d.texture,
        blend: d.blend,
        topology: d.topology,
        fog: d.fog,
        anchor: d.anchor,
        bias: d.bias,
        raster_bias: d.raster_bias,
        range: d.range.clone(),
        light: d.light.clone(),
    }));
}

/// Add one [`Transparent3d`] item per draw record to the matching camera's phase.
fn queue_effects(
    effect_pipeline: Res<EffectPipeline>,
    mut pipelines: ResMut<SpecializedRenderPipelines<EffectPipeline>>,
    pipeline_cache: Res<PipelineCache>,
    draw_functions: Res<DrawFunctions<Transparent3d>>,
    meta: Res<EffectMeta>,
    mut phases: ResMut<ViewSortedRenderPhases<Transparent3d>>,
    views: Query<(&ExtractedView, &Msaa)>,
) {
    if meta.draws.is_empty() {
        return;
    }
    let draw_function = draw_functions.read().id::<DrawEffects>();
    for (view, msaa) in &views {
        // A draw targets ONE main-world camera (the world camera or a booth's — resolved by
        // the sim); the phase map is keyed by the retained view, whose main entity IS that
        // camera. Views without a transparent phase (shadow/prepass) fall out here.
        let Some(phase) = phases.get_mut(&view.retained_view_entity) else {
            continue;
        };
        let rangefinder = view.rangefinder3d();
        for (i, draw) in meta.draws.iter().enumerate() {
            if view.retained_view_entity.main_entity != MainEntity::from(draw.cam) {
                continue;
            }
            let pipeline = pipelines.specialize(
                &pipeline_cache,
                &effect_pipeline,
                EffectPipelineKey {
                    samples: msaa.samples(),
                    hdr: view.hdr,
                    blend: draw.blend,
                    raster_bias: draw.raster_bias,
                },
            );
            phase.add(Transparent3d {
                // The material path's exact metric: view-space z of the sort point + the
                // ladder rung (see the module doc).
                distance: rangefinder.distance(&draw.anchor) + draw.bias,
                pipeline,
                entity: (Entity::PLACEHOLDER, MainEntity::from(draw.main_entity)),
                draw_function,
                // The draw-record index rides here until the prepare walk rewrites it to a
                // merged-draw index (whose length spans the items it absorbed — the sorted
                // phase renderer's own batching contract).
                batch_range: (i as u32)..(i as u32 + 1),
                extra_index: PhaseItemExtraIndex::None,
                indexed: true,
            });
        }
    }
}

/// The run identity the merge walk groups by: everything two adjacent items must share to be
/// one GPU draw — pipeline (blend/bias/msaa/hdr), bind group (texture + light), params row.
type RunKey = (
    bevy::render::render_resource::CachedRenderPipelineId,
    AssetId<Image>,
    Option<BufferId>,
    u32,
);

/// After `PhaseSort`: rebase each draw's vertices against its target view's camera position
/// (0733 §2), upload them, build the frame's index stream in sorted-item order while merging
/// sort-adjacent compatible items into single draws, and write the canonical fog-params rows
/// once.
#[allow(clippy::too_many_arguments)] // one render system's full input set
fn prepare_effects(
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    draw_functions: Res<DrawFunctions<Transparent3d>>,
    mut meta: ResMut<EffectMeta>,
    mut phases: ResMut<ViewSortedRenderPhases<Transparent3d>>,
    views: Query<&ExtractedView>,
) {
    let meta = &mut *meta;
    // The rebase (0733 §2): subtract the draw's target camera position — the exact value the
    // view's `ViewUniform.world_position` is built from (`view/mod.rs:985`), so the shader's
    // `view_from_world`-rotation reconstruction is bitwise-consistent. The stream stays
    // world-space main-world-side (instruments read it); only the upload copy moves.
    // (A handful of extracted views; prepass/shadow views of the same camera collapse to the
    // same translation.)
    let mut cams: HashMap<MainEntity, Vec3> = HashMap::default();
    for view in &views {
        cams.insert(
            view.retained_view_entity.main_entity,
            view.world_from_view.translation(),
        );
    }
    for draw in &meta.draws {
        let Some(cam) = cams.get(&MainEntity::from(draw.cam)) else {
            continue;
        };
        let offset = cam.to_array();
        for v in &mut meta.vertices.values_mut()[draw.range.start as usize..draw.range.end as usize]
        {
            v.pos[0] -= offset[0];
            v.pos[1] -= offset[1];
            v.pos[2] -= offset[2];
        }
    }
    meta.vertices.write_buffer(&device, &queue);

    // The merge walk (0733 §3, the bevy_ui_render shape): one pass over each LIVE view's sorted
    // items — the same views queue targeted, never the raw phase map (a map entry bevy hasn't
    // swept yet would carry a dead frame's indices). Every effect item's indices are appended
    // (quad pattern or identity tri-list); adjacent items sharing a [`RunKey`] fold into the
    // run-opening item, whose `batch_range` is rewritten to
    // `merged_index .. merged_index + run_len` — the phase renderer draws the opener once and
    // advances past the absorbed items (`render_phase/mod.rs:1487`).
    let effect_fn = draw_functions.read().id::<DrawEffects>();
    meta.indices.clear();
    meta.merged.clear();
    let mut walked: Vec<bevy::render::view::RetainedViewEntity> = Vec::new();
    for view in &views {
        // Prepass/shadow views share a camera; only the first walk of a phase counts.
        if walked.contains(&view.retained_view_entity) {
            continue;
        }
        walked.push(view.retained_view_entity);
        let Some(phase) = phases.get_mut(&view.retained_view_entity) else {
            continue;
        };
        let mut open: Option<(usize, u32, RunKey)> = None;
        let close = |items: &mut Vec<Transparent3d>,
                     open: &mut Option<(usize, u32, RunKey)>,
                     merged_len: usize| {
            if let Some((item_idx, run_len, _)) = open.take() {
                let m = merged_len as u32 - 1;
                items[item_idx].batch_range = m..m + run_len;
            }
        };
        for i in 0..phase.items.len() {
            let item = &phase.items[i];
            if item.draw_function != effect_fn {
                close(&mut phase.items, &mut open, meta.merged.len());
                continue;
            }
            let (pipeline, draw_idx) = (item.pipeline, item.batch_range.start as usize);
            // Defense in depth: `batch_range` is only a draw index if nothing else rewrote it.
            // Every draw's probe identity is mesh-less, which keeps bevy's sorted-phase batcher
            // off our items (it claims any item whose MAIN entity has a registered mesh and
            // rewrites the range to an instance index — the Goldshire-teleport crash, where
            // foam draws carried their water chunk). If a future violation sneaks in, degrade
            // to one skipped item — never a render-thread panic.
            let Some(draw) = meta.draws.get(draw_idx) else {
                close(&mut phase.items, &mut open, meta.merged.len());
                phase.items[i].batch_range = 0..0;
                continue;
            };
            let index_start = meta.indices.len() as u32;
            match draw.topology {
                EffectTopology::Quads => {
                    let mut b = draw.range.start;
                    while b < draw.range.end {
                        for k in [b, b + 1, b + 2, b, b + 2, b + 3] {
                            meta.indices.push(k);
                        }
                        b += 4;
                    }
                }
                EffectTopology::Tris => {
                    for k in draw.range.clone() {
                        meta.indices.push(k);
                    }
                }
            }
            let index_end = meta.indices.len() as u32;
            let key: RunKey = (
                pipeline,
                draw.texture,
                draw.light.as_ref().map(|b| b.id()),
                draw.fog.slot(),
            );
            match &mut open {
                Some((_, run_len, open_key)) if *open_key == key => {
                    meta.merged
                        .last_mut()
                        .expect("open run has a merged record")
                        .index_range
                        .end = index_end;
                    *run_len += 1;
                }
                _ => {
                    close(&mut phase.items, &mut open, meta.merged.len());
                    meta.merged.push(MergedDraw {
                        index_range: index_start..index_end,
                        texture: draw.texture,
                        light: draw.light.clone(),
                        fog: draw.fog,
                    });
                    open = Some((i, 1, key));
                }
            }
        }
        close(&mut phase.items, &mut open, meta.merged.len());
    }
    if !meta.indices.is_empty() {
        meta.indices.write_buffer(&device, &queue);
    }

    if meta.params_offsets.is_none() {
        // Slot order = `EffectFog::slot`: off, scene, black, white, grey, rain-forced —
        // `params.x` carries the shader's fog COLOUR policy (the `0x70baf0` table), `params.y`
        // the forced-fog enable with `zw` its start/end (rain's verified 70..75 window — the
        // constants live with their law in `weather::precip`).
        let offsets = [
            meta.params.push(&Vec4::new(0.0, 0.0, 0.0, 0.0)),
            meta.params.push(&Vec4::new(1.0, 0.0, 0.0, 0.0)),
            meta.params.push(&Vec4::new(2.0, 0.0, 0.0, 0.0)),
            meta.params.push(&Vec4::new(3.0, 0.0, 0.0, 0.0)),
            meta.params.push(&Vec4::new(4.0, 0.0, 0.0, 0.0)),
            meta.params.push(&Vec4::new(
                0.0,
                1.0,
                crate::weather::RAIN_FOG_START,
                crate::weather::RAIN_FOG_END,
            )),
        ];
        meta.params.write_buffer(&device, &queue);
        meta.params_offsets = Some(offsets);
    }
}

/// Build the view bind group and any missing per-texture groups for this frame's draws.
#[allow(clippy::too_many_arguments)] // one render system's full input set
fn prepare_effect_bind_groups(
    device: Res<RenderDevice>,
    pipeline_cache: Res<PipelineCache>,
    pipeline: Res<EffectPipeline>,
    view_uniforms: Res<ViewUniforms>,
    gpu_images: Res<RenderAssets<GpuImage>>,
    light: Option<Res<SharedLightBuffer>>,
    mut meta: ResMut<EffectMeta>,
    mut bind_groups: ResMut<EffectBindGroups>,
) {
    let Some(view_binding) = view_uniforms.uniforms.binding() else {
        return;
    };
    meta.view_bind_group = Some(device.create_bind_group(
        "effect_view_bind_group",
        &pipeline_cache.get_bind_group_layout(&pipeline.view_layout),
        &BindGroupEntries::single(view_binding),
    ));
    let Some(light) = light else { return };
    let Some(params_binding) = meta.params.binding() else {
        return;
    };
    for draw in &meta.draws {
        let key = (draw.texture, draw.light.as_ref().map(|b| b.id()));
        if bind_groups.images.contains_key(&key) {
            continue;
        }
        // Not yet prepared GPU-side: the draw is skipped this frame (the same "withhold until
        // resident" the main-world gate applies one asset-layer earlier).
        let Some(image) = gpu_images.get(draw.texture) else {
            continue;
        };
        let light_buf = draw.light.as_ref().unwrap_or(&light.0);
        bind_groups.images.insert(
            key,
            device.create_bind_group(
                "effect_image_bind_group",
                &pipeline_cache.get_bind_group_layout(&pipeline.image_layout),
                &BindGroupEntries::sequential((
                    &image.texture_view,
                    &image.sampler,
                    light_buf.as_entire_binding(),
                    params_binding.clone(),
                )),
            ),
        );
    }
}

pub type DrawEffects = (SetItemPipeline, SetEffectViewBindGroup<0>, DrawEffectBatch);

pub struct SetEffectViewBindGroup<const I: usize>;
impl<P: PhaseItem, const I: usize> RenderCommand<P> for SetEffectViewBindGroup<I> {
    type Param = SRes<EffectMeta>;
    type ViewQuery = Read<ViewUniformOffset>;
    type ItemQuery = ();

    fn render<'w>(
        _item: &P,
        view_uniform: &'w ViewUniformOffset,
        _entity: Option<()>,
        meta: SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        let Some(view_bind_group) = meta.into_inner().view_bind_group.as_ref() else {
            return RenderCommandResult::Failure("effect view bind group not available");
        };
        pass.set_bind_group(I, view_bind_group, &[view_uniform.offset]);
        RenderCommandResult::Success
    }
}

pub struct DrawEffectBatch;
impl<P: PhaseItem> RenderCommand<P> for DrawEffectBatch {
    type Param = (SRes<EffectMeta>, SRes<EffectBindGroups>);
    type ViewQuery = ();
    type ItemQuery = ();

    #[inline]
    fn render<'w>(
        item: &P,
        _view: (),
        _entity: Option<()>,
        (meta, bind_groups): SystemParamItem<'w, '_, Self::Param>,
        pass: &mut TrackedRenderPass<'w>,
    ) -> RenderCommandResult {
        let meta = meta.into_inner();
        // `batch_range.start` is the merged-draw index (the prepare walk's rewrite); its length
        // is the item span the phase renderer advances by — not read here.
        let Some(draw) = meta.merged.get(item.batch_range().start as usize) else {
            return RenderCommandResult::Skip;
        };
        // GPU image not prepared yet: withheld, exactly like the main-world residency gate.
        let key = (draw.texture, draw.light.as_ref().map(|b| b.id()));
        let Some(image_bind_group) = bind_groups.into_inner().images.get(&key) else {
            return RenderCommandResult::Skip;
        };
        let (Some(vertices), Some(indices), Some(offsets)) = (
            meta.vertices.buffer(),
            meta.indices.buffer(),
            meta.params_offsets,
        ) else {
            return RenderCommandResult::Failure("effect lane buffers not available");
        };
        pass.set_bind_group(1, image_bind_group, &[offsets[draw.fog.slot() as usize]]);
        pass.set_vertex_buffer(0, vertices.slice(..));
        pass.set_index_buffer(indices.slice(..), IndexFormat::Uint32);
        pass.draw_indexed(draw.index_range.clone(), 0, 0..1);
        RenderCommandResult::Success
    }
}

/// Registers the render-world half. The main-world [`EffectQuads`] resource and the family
/// writers are their plugins' ([`super::ParticlePlugin`], [`crate::ribbons::RibbonPlugin`],
/// the decal/foam/precip modules).
pub struct EffectLanePlugin;

impl Plugin for EffectLanePlugin {
    fn build(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_resource::<EffectMeta>()
            .init_resource::<EffectBindGroups>()
            .init_resource::<SpecializedRenderPipelines<EffectPipeline>>()
            .add_render_command::<Transparent3d, DrawEffects>()
            .add_systems(RenderStartup, init_effect_pipeline)
            .add_systems(ExtractSchedule, extract_effects)
            .add_systems(
                Render,
                (
                    queue_effects.in_set(RenderSystems::Queue),
                    // After `PhaseSort` (the set ordering) — the merge walk needs the final
                    // item order.
                    prepare_effects.in_set(RenderSystems::PrepareResources),
                    prepare_effect_bind_groups.in_set(RenderSystems::PrepareBindGroups),
                ),
            );
    }
}
