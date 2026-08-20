//! The render-world half of the B1 retained pass (see `mod.rs`; decision 1429): extraction of
//! the published cell set, per-cell GPU assembly (texture-array classes + the item→layer
//! table), the pipeline family, and the draw node between the main opaque and transparent
//! passes.
//!
//! Assembly happens where each fact lives: the MAIN world bakes geometry (it owns the
//! submeshes) but cannot know texture dims/format (BLP images are `RENDER_WORLD`-only), so
//! classing into `texture_2d_array`s happens HERE, per cell, once its members' `GpuImage`s are
//! all resident — the layer copies ride the node's own encoder, before its pass begins. A cell
//! whose textures aren't all loaded yet simply isn't drawn that frame (the entity path streams
//! batches in piecewise; cell-granular appearance is the same arrival class, mostly under the
//! load cover).

use bevy::camera::primitives::Aabb;
use bevy::ecs::query::QueryItem;
use bevy::image::Image;
use bevy::mesh::VertexBufferLayout;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::mesh::allocator::MeshAllocator;
use bevy::render::mesh::RenderMesh;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_graph::{
    NodeRunError, RenderGraphContext, RenderGraphExt, RenderLabel, ViewNode, ViewNodeRunner,
};
use bevy::render::render_resource::binding_types::{
    sampler, storage_buffer_read_only_sized, texture_2d_array, uniform_buffer, uniform_buffer_sized,
};
use bevy::render::render_resource::*;
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue};
use bevy::render::texture::GpuImage;
use bevy::render::view::{
    ExtractedView, Msaa, ViewDepthTexture, ViewTarget, ViewUniform, ViewUniformOffset, ViewUniforms,
};
use bevy::render::{Render, RenderSystems};
use bevy::shader::ShaderDefVal;
use std::ops::Range;

use bevy::core_pipeline::core_3d::graph::{Core3d, Node3d};
use bevy::core_pipeline::core_3d::CORE_3D_DEPTH_FORMAT;

/// One baked item's draw facts (index-parallel with the bake order; the vertex word's low bits
/// carry this item's index, which the record table resolves to an array layer + the WMO
/// per-item record).
#[derive(Clone)]
pub(crate) struct GxItemDraw {
    pub index_range: Range<u32>,
    pub texture: Option<AssetId<Image>>,
    pub cutout: bool,
    pub two_sided: bool,
    #[allow(dead_code)] // bake-side bookkeeping; the node draws by index range alone
    pub vertex_range: Range<u32>,
    /// The WMO group this item belongs to (`None` on cell items) — the range-selection key:
    /// a run never crosses a group boundary, so the flood's verdict selects whole runs.
    pub group: Option<u16>,
    /// The authored batch order (the coplanar-MOBA clip-z nudge; 0 on cell items).
    pub order: u16,
    /// The MOMT SIDN night-glow colour (gamma bytes; zero on cell items).
    pub sidn: [u8; 3],
}

/// One baked cell (or WMO region), published by the main-world flush.
#[derive(Clone)]
pub(crate) struct GxCellDraw {
    pub mesh: Handle<Mesh>,
    /// The recentring origin (0974's precision split): shader world = vertex + origin.
    pub origin: Vec3,
    /// Mesh-local bound (recentred); world bound = origin + this.
    pub aabb: Aabb,
    pub draws: Vec<GxItemDraw>,
    /// The exile kill bitmap (B2, 1431): bit *i* set ⇒ item *i* is punched out of the
    /// retained draw (its placement is feathering as ordinary entities, or fully faded).
    /// Rebuilt in place by the main-world scan; all-zero on WMO regions.
    pub killed: Vec<u64>,
    /// Bumped by the scan on every bitmap change — the render side syncs the record table's
    /// kill column when it sees a revision it hasn't applied.
    pub killed_rev: u32,
    /// Per-GROUP mesh-local bounds (WMO regions only; empty for cells) — what the cull's
    /// per-group admission tests.
    pub groups: Vec<(u16, Aabb)>,
}

/// Marks the ONE view the retained pass draws into — the world camera. Without this the node
/// would run for EVERY Core3d view, including the portrait-booth bakes, and paint world cells
/// into a portrait with the booth's view matrices (the cull list is the world camera's).
#[derive(Component, Clone, Copy, Default, ExtractComponent)]
pub(crate) struct StaticGxView;

/// Insert the marker on the world camera (idempotent — the camera can respawn).
fn mark_world_camera(
    mut commands: Commands,
    cam: Query<Entity, (With<crate::view::WorldCamera>, Without<StaticGxView>)>,
) {
    for e in &cam {
        commands.entity(e).insert(StaticGxView);
    }
}

/// The published half the render world clones each frame (handles + ranges — cheap).
#[derive(Clone, Default, Resource, ExtractResource)]
pub(crate) struct GxWorld {
    pub cells: HashMap<(i32, i32), GxCellDraw>,
    /// This frame's CPU scene-walk verdict (frustum + farclip + exterior gate, cell-granular).
    pub visible: Vec<(i32, i32)>,
    /// The WMO regions (slice 2), keyed by placement instance entity.
    pub wmos: HashMap<Entity, GxCellDraw>,
    /// This frame's per-group admission per region (indexed by absolute group index): the
    /// portal flood's verdict collapsed to CPU range selection — the node draws exactly the
    /// runs whose group bit is set.
    pub visible_wmos: Vec<(Entity, Vec<bool>)>,
}

/// A texture-dimension class within one cell: one `texture_2d_array` + the members feeding it.
struct GxClassGpu {
    array: Texture,
    bind_group: BindGroup,
    /// Copies still to encode (source GpuImage's texture, destination layer). Drained by the
    /// node's encoder on its next run; the cell draws only once every class is fully copied.
    pending: Vec<(Texture, u32)>,
    mip_count: u32,
    size: Extent3d,
}

/// One coalesced draw run: adjacent bake items sharing (class, pipeline bucket, group).
struct GxRun {
    class: usize,
    cutout: bool,
    two_sided: bool,
    index_range: Range<u32>,
    /// The WMO group every item in this run belongs to (`None` = a cell run, always drawn) —
    /// the bake sorts group inside (bucket, texture), so runs are group-homogeneous by
    /// construction and the flood's per-group verdict selects whole runs.
    group: Option<u16>,
}

/// A cell's assembled GPU state, cached across frames; rebuilt when the bake (mesh handle)
/// changes.
struct GxCellGpu {
    mesh: AssetId<Mesh>,
    classes: Vec<GxClassGpu>,
    record_table: Buffer,
    #[allow(dead_code)] // held alive for the bind groups that reference it
    cell_uniform: Buffer,
    runs: Vec<GxRun>,
    /// CPU copy of the record table — the kill-bit sync rewrites column 3 and re-uploads.
    records: Vec<[u32; 4]>,
    /// The `killed_rev` this table last uploaded.
    killed_applied: u32,
}

#[derive(Resource, Default)]
struct GxGpuCache {
    cells: HashMap<(i32, i32), GxCellGpu>,
    wmos: HashMap<Entity, GxCellGpu>,
}

#[derive(Resource)]
struct GxPipelines {
    view_layout: BindGroupLayoutDescriptor,
    cell_layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    sampler_clamp: Sampler,
    /// Keyed `(cutout, two_sided)`; specialized for the world view's (samples, format) pair —
    /// re-specialized if that pair ever changes (a window move across displays).
    pipelines: HashMap<(bool, bool), CachedRenderPipelineId>,
    specialized_for: Option<(u32, TextureFormat)>,
}

fn init_pipelines(mut commands: Commands, render_device: Res<RenderDevice>) {
    let view_layout = BindGroupLayoutDescriptor::new(
        "static_gx_view_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::VERTEX_FRAGMENT,
            (
                uniform_buffer::<ViewUniform>(true),
                storage_buffer_read_only_sized(false, None),
            ),
        ),
    );
    let cell_layout = BindGroupLayoutDescriptor::new(
        "static_gx_cell_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::VERTEX_FRAGMENT,
            (
                // origin (xyz) + pad
                uniform_buffer_sized(false, Some(std::num::NonZero::new(16).unwrap())),
                // item → texture-array layer
                storage_buffer_read_only_sized(false, None),
                texture_2d_array(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                sampler(SamplerBindingType::Filtering),
            ),
        ),
    );
    // TWO samplers per array — repeat and clamp, both matching the BLP loader's model albedo
    // sampler exactly (linear tri-filtered, ANISOTROPY 8 — `blp.rs`; the aniso is load-bearing
    // for parity, oblique minification reads visibly softer without it). The shader selects by
    // the vertex word's wrap bits: a shared array cannot carry per-layer address modes. The
    // rare MIXED-wrap batch (repeat one axis, clamp the other) keeps the repeat sampler plus
    // the shader's half-texel inset clamp on its clamped axis — an approximation confined to
    // that class (decision 0763's silhouette concern, honoured per axis).
    let make = |label: &'static str, mode: AddressMode| {
        render_device.create_sampler(&SamplerDescriptor {
            label: Some(label),
            min_filter: FilterMode::Linear,
            mag_filter: FilterMode::Linear,
            mipmap_filter: FilterMode::Linear,
            address_mode_u: mode,
            address_mode_v: mode,
            anisotropy_clamp: 8,
            ..Default::default()
        })
    };
    commands.insert_resource(GxPipelines {
        view_layout,
        cell_layout,
        sampler: make("static_gx_repeat", AddressMode::Repeat),
        sampler_clamp: make("static_gx_clamp", AddressMode::ClampToEdge),
        pipelines: HashMap::default(),
        specialized_for: None,
    });
}

/// The pipeline-key query: the world view's (samples, format) inputs (the marker keeps booth
/// views out of it).
type GxViewKey = (
    &'static ExtractedView,
    &'static Msaa,
    &'static ViewTarget,
    &'static StaticGxView,
);

/// The fixed interleaved vertex layout the bake authors — **attribute-ID order**, which is
/// how Bevy interleaves a mesh's buffer: position (0), normal (1), uv (2), COLOR (5 — MOCV /
/// the baked constant tint, white default), then the custom word + anchor (988_101/988_102).
/// Kept in sync with `bake_cell` and `static_gx.wgsl`.
fn vertex_layout() -> VertexBufferLayout {
    VertexBufferLayout {
        array_stride: 64,
        step_mode: VertexStepMode::Vertex,
        attributes: vec![
            VertexAttribute {
                format: VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            VertexAttribute {
                format: VertexFormat::Float32x3,
                offset: 12,
                shader_location: 1,
            },
            VertexAttribute {
                format: VertexFormat::Float32x2,
                offset: 24,
                shader_location: 2,
            },
            VertexAttribute {
                format: VertexFormat::Float32x4,
                offset: 32,
                shader_location: 5,
            },
            VertexAttribute {
                format: VertexFormat::Uint32,
                offset: 48,
                shader_location: 3,
            },
            VertexAttribute {
                format: VertexFormat::Float32x3,
                offset: 52,
                shader_location: 4,
            },
        ],
    }
}

/// (Re-)specialize the four pipelines for the world view's (samples, format), and assemble
/// visible cells' GPU state: classes, arrays, layer table, bind groups, runs.
#[allow(clippy::too_many_arguments)]
fn prepare_static_gx(
    gx: Res<GxWorld>,
    mut cache: ResMut<GxGpuCache>,
    mut pipes: ResMut<GxPipelines>,
    pipeline_cache: Res<PipelineCache>,
    render_device: Res<RenderDevice>,
    render_queue: Res<RenderQueue>,
    asset_server: Res<AssetServer>,
    images: Res<RenderAssets<GpuImage>>,
    views: Query<GxViewKey>,
) {
    let _t = super::gx_perf_guard(3);
    // The world view's pipeline key (the marker keeps booth views out of it).
    let Some((view, msaa, _, _)) = views.iter().next() else {
        return;
    };
    let format = if view.hdr {
        ViewTarget::TEXTURE_FORMAT_HDR
    } else {
        TextureFormat::bevy_default()
    };
    let key = (msaa.samples(), format);
    if pipes.specialized_for != Some(key) {
        let shader: Handle<Shader> =
            asset_server.load("embedded://benilla_world/shaders/static_gx.wgsl");
        pipes.pipelines.clear();
        for cutout in [false, true] {
            for two_sided in [false, true] {
                let mut defs = vec![];
                if cutout {
                    defs.push(ShaderDefVal::from("GX_CUTOUT"));
                }
                let id = pipeline_cache.queue_render_pipeline(RenderPipelineDescriptor {
                    label: Some(
                        format!("static_gx c{} t{}", u8::from(cutout), u8::from(two_sided)).into(),
                    ),
                    layout: vec![pipes.view_layout.clone(), pipes.cell_layout.clone()],
                    vertex: VertexState {
                        shader: shader.clone(),
                        shader_defs: defs.clone(),
                        entry_point: Some("vertex".into()),
                        buffers: vec![vertex_layout()],
                    },
                    fragment: Some(FragmentState {
                        shader: shader.clone(),
                        shader_defs: defs,
                        entry_point: Some("fragment".into()),
                        targets: vec![Some(ColorTargetState {
                            format,
                            blend: None,
                            write_mask: ColorWrites::ALL,
                        })],
                    }),
                    primitive: PrimitiveState {
                        cull_mode: (!two_sided).then_some(Face::Back),
                        ..Default::default()
                    },
                    depth_stencil: Some(DepthStencilState {
                        format: CORE_3D_DEPTH_FORMAT,
                        depth_write_enabled: true,
                        depth_compare: CompareFunction::GreaterEqual,
                        stencil: StencilState::default(),
                        bias: DepthBiasState::default(),
                    }),
                    multisample: MultisampleState {
                        count: msaa.samples(),
                        ..Default::default()
                    },
                    ..default()
                });
                pipes.pipelines.insert((cutout, two_sided), id);
            }
        }
        pipes.specialized_for = Some(key);
    }

    // Drop cache entries whose region vanished or re-baked.
    cache
        .cells
        .retain(|c, gpu| gx.cells.get(c).is_some_and(|d| d.mesh.id() == gpu.mesh));
    cache
        .wmos
        .retain(|e, gpu| gx.wmos.get(e).is_some_and(|d| d.mesh.id() == gpu.mesh));

    for cell in &gx.visible {
        if cache.cells.contains_key(cell) {
            continue;
        }
        let Some(draw) = gx.cells.get(cell) else {
            continue;
        };
        if let Some(gpu) = assemble_region(
            draw,
            &pipes,
            &pipeline_cache,
            &render_device,
            &render_queue,
            &images,
        ) {
            cache.cells.insert(*cell, gpu);
        }
    }
    for (entity, _) in &gx.visible_wmos {
        if cache.wmos.contains_key(entity) {
            continue;
        }
        let Some(draw) = gx.wmos.get(entity) else {
            continue;
        };
        if let Some(gpu) = assemble_region(
            draw,
            &pipes,
            &pipeline_cache,
            &render_device,
            &render_queue,
            &images,
        ) {
            cache.wmos.insert(*entity, gpu);
        }
    }
    // The exile kill-bit sync (B2, 1431): when the scan's bitmap revision moved, rewrite the
    // record table's kill column and re-upload. One whole-table write per changed cell per
    // change frame — band crossings are rare and a table is tens of KB; a cell that changed
    // while out of view syncs on re-entry (the revision mismatch persists until applied).
    for cell in &gx.visible {
        let (Some(gpu), Some(draw)) = (cache.cells.get_mut(cell), gx.cells.get(cell)) else {
            continue;
        };
        if gpu.killed_applied == draw.killed_rev {
            continue;
        }
        for (i, rec) in gpu.records.iter_mut().enumerate() {
            rec[3] = kill_bit(&draw.killed, i);
        }
        render_queue.write_buffer(&gpu.record_table, 0, bytemuck::cast_slice(&gpu.records));
        gpu.killed_applied = draw.killed_rev;
    }
}

/// Assemble one region's GPU state (texture classes + arrays, the per-item record table, bind
/// groups, coalesced runs). `None` while any member texture is not yet resident — the region
/// simply isn't drawn that frame (the entity path streams batches in piecewise; this is the
/// same arrival class).
fn assemble_region(
    draw: &GxCellDraw,
    pipes: &GxPipelines,
    pipeline_cache: &PipelineCache,
    render_device: &RenderDevice,
    render_queue: &RenderQueue,
    images: &RenderAssets<GpuImage>,
) -> Option<GxCellGpu> {
    // Class by (size, format, mips); assign layers DEDUPED by texture — many items share one
    // texture (Stormwind's region carries 3,042 items over a few hundred distinct BLPs), and
    // per-item layers blew past the D2-array layer limit the moment a city root baked. A
    // layer's content depends only on its source texture, so sharing is exact. A class that
    // still fills to the device limit overflows into a sibling class with the same key.
    let max_layers = render_device.limits().max_texture_array_layers as usize;
    type ClassAcc<'a> = (Extent3d, TextureFormat, u32, Vec<&'a GpuImage>);
    let mut classes: Vec<ClassAcc<'_>> = Vec::new();
    let mut item_class_layer: Vec<(u32, u32)> = vec![(0, 0); draw.draws.len()];
    let mut assigned: HashMap<AssetId<Image>, (u32, u32)> = HashMap::default();
    for (i, item) in draw.draws.iter().enumerate() {
        let Some(tex) = item.texture else { continue };
        if let Some(&cl) = assigned.get(&tex) {
            item_class_layer[i] = cl;
            continue;
        }
        // Every member texture must be resident to class the region at all.
        let g = images.get(tex)?;
        let sz = g.texture.size();
        let key = (
            Extent3d {
                width: sz.width,
                height: sz.height,
                depth_or_array_layers: 1,
            },
            g.texture.format(),
            g.texture.mip_level_count(),
        );
        let ci = classes
            .iter()
            .position(|(s, f, m, mem)| {
                (*s, *f, *m) == (key.0, key.1, key.2) && mem.len() < max_layers
            })
            .unwrap_or_else(|| {
                classes.push((key.0, key.1, key.2, Vec::new()));
                classes.len() - 1
            });
        let layer = u32::try_from(classes[ci].3.len()).unwrap();
        classes[ci].3.push(g);
        let cl = (u32::try_from(ci).unwrap(), layer);
        assigned.insert(tex, cl);
        item_class_layer[i] = cl;
    }
    // The per-item record table: [layer, batch-order nudge, packed SIDN, kill bit] per item —
    // the vertex word's low bits index it. Untextured items ride class 0 (never sampled — the
    // TEXTURED bit is clear); a region of ONLY untextured items still needs one dummy array.
    // Column 3 is the exile kill bit (B2): folded from the published bitmap here, kept in
    // sync per frame by `prepare_static_gx`'s revision check — hence COPY_DST.
    let records: Vec<[u32; 4]> = draw
        .draws
        .iter()
        .enumerate()
        .map(|(i, item)| {
            [
                item_class_layer[i].1,
                u32::from(item.order),
                u32::from(item.sidn[0])
                    | (u32::from(item.sidn[1]) << 8)
                    | (u32::from(item.sidn[2]) << 16),
                kill_bit(&draw.killed, i),
            ]
        })
        .collect();
    let record_table = render_device.create_buffer_with_data(&BufferInitDescriptor {
        label: Some("static_gx_records"),
        contents: bytemuck::cast_slice(&records),
        usage: BufferUsages::STORAGE | BufferUsages::COPY_DST,
    });
    let cell_uniform = render_device.create_buffer_with_data(&BufferInitDescriptor {
        label: Some("static_gx_cell"),
        contents: bytemuck::cast_slice(&[draw.origin.x, draw.origin.y, draw.origin.z, 0.0f32]),
        usage: BufferUsages::UNIFORM,
    });
    let cell_layout = pipeline_cache.get_bind_group_layout(&pipes.cell_layout);
    let mut gpu_classes: Vec<GxClassGpu> = Vec::new();
    for (size, tex_format, mips, members) in &classes {
        // The VRAM meter (1431's regression hunt): approximate bytes for the array about to
        // be created — block-compressed at their block rate, else 4 B/texel — ×4/3 for mips.
        if super::gx_perf_enabled() {
            let per_layer = match tex_format {
                TextureFormat::Bc1RgbaUnorm | TextureFormat::Bc1RgbaUnormSrgb => {
                    u64::from(size.width) * u64::from(size.height) / 2
                }
                TextureFormat::Bc2RgbaUnorm
                | TextureFormat::Bc2RgbaUnormSrgb
                | TextureFormat::Bc3RgbaUnorm
                | TextureFormat::Bc3RgbaUnormSrgb => u64::from(size.width) * u64::from(size.height),
                _ => u64::from(size.width) * u64::from(size.height) * 4,
            };
            let bytes = per_layer * members.len().max(1) as u64 * if *mips > 1 { 4 } else { 3 } / 3;
            super::GX_VRAM.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
        }
        let array = render_device.create_texture(&TextureDescriptor {
            label: Some("static_gx_array"),
            size: Extent3d {
                width: size.width,
                height: size.height,
                depth_or_array_layers: u32::try_from(members.len().max(1)).unwrap(),
            },
            mip_level_count: *mips,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: *tex_format,
            usage: TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = array.create_view(&TextureViewDescriptor {
            dimension: Some(TextureViewDimension::D2Array),
            ..Default::default()
        });
        let bind_group = render_device.create_bind_group(
            "static_gx_cell",
            &cell_layout,
            &BindGroupEntries::sequential((
                cell_uniform.as_entire_binding(),
                record_table.as_entire_binding(),
                &view,
                &pipes.sampler,
                &pipes.sampler_clamp,
            )),
        );
        gpu_classes.push(GxClassGpu {
            array,
            bind_group,
            pending: members
                .iter()
                .enumerate()
                .map(|(layer, g)| (g.texture.clone(), u32::try_from(layer).unwrap()))
                .collect(),
            mip_count: *mips,
            size: *size,
        });
    }
    if gpu_classes.is_empty() {
        // All-untextured region: a 1×1 white dummy array so the bind group exists.
        let white = render_device.create_texture(&TextureDescriptor {
            label: Some("static_gx_white"),
            size: Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8UnormSrgb,
            usage: TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        render_queue.write_texture(
            white.as_image_copy(),
            &[255, 255, 255, 255],
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: None,
            },
            Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let view = white.create_view(&TextureViewDescriptor {
            dimension: Some(TextureViewDimension::D2Array),
            ..Default::default()
        });
        let bind_group = render_device.create_bind_group(
            "static_gx_cell",
            &cell_layout,
            &BindGroupEntries::sequential((
                cell_uniform.as_entire_binding(),
                record_table.as_entire_binding(),
                &view,
                &pipes.sampler,
                &pipes.sampler_clamp,
            )),
        );
        gpu_classes.push(GxClassGpu {
            array: white,
            bind_group,
            pending: Vec::new(),
            mip_count: 1,
            size: Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        });
    }
    // Coalesce adjacent items sharing (class, bucket, group) into draw runs (the bake sorted
    // by (bucket, texture[, group]), so repeated textures and same-bucket spans fuse; a WMO
    // run never crosses a group boundary — the selection grain).
    let mut runs: Vec<GxRun> = Vec::new();
    for (i, item) in draw.draws.iter().enumerate() {
        let class = item_class_layer[i].0 as usize;
        match runs.last_mut() {
            Some(r)
                if r.class == class
                    && r.cutout == item.cutout
                    && r.two_sided == item.two_sided
                    && r.group == item.group
                    && r.index_range.end == item.index_range.start =>
            {
                r.index_range.end = item.index_range.end;
            }
            _ => runs.push(GxRun {
                class,
                cutout: item.cutout,
                two_sided: item.two_sided,
                index_range: item.index_range.clone(),
                group: item.group,
            }),
        }
    }
    Some(GxCellGpu {
        mesh: draw.mesh.id(),
        classes: gpu_classes,
        record_table,
        cell_uniform,
        runs,
        records,
        killed_applied: draw.killed_rev,
    })
}

/// Record-table column 3: item `i`'s exile kill bit from the published bitmap.
fn kill_bit(killed: &[u64], i: usize) -> u32 {
    u32::from(
        killed
            .get(i / 64)
            .is_some_and(|w| w & (1u64 << (i % 64)) != 0),
    )
}

/// The per-frame view bind group (group 0): bevy's view uniform + the shared light buffer —
/// the SAME `wow_shared_light` storage every material binds (1429: identical lighting by
/// construction).
#[derive(Resource)]
struct GxViewBind(BindGroup);

fn prepare_view_bind(
    mut commands: Commands,
    pipes: Res<GxPipelines>,
    pipeline_cache: Res<PipelineCache>,
    render_device: Res<RenderDevice>,
    view_uniforms: Res<ViewUniforms>,
    light: Option<Res<crate::lighting::SharedLightBuffer>>,
) {
    let (Some(view_binding), Some(light)) = (view_uniforms.uniforms.binding(), light) else {
        return;
    };
    let layout = pipeline_cache.get_bind_group_layout(&pipes.view_layout);
    commands.insert_resource(GxViewBind(render_device.create_bind_group(
        "static_gx_view",
        &layout,
        &BindGroupEntries::sequential((view_binding, light.0.as_entire_binding())),
    )));
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
struct StaticGxLabel;

#[derive(Default)]
struct StaticGxNode;

impl ViewNode for StaticGxNode {
    type ViewQuery = (
        &'static ViewTarget,
        &'static ViewDepthTexture,
        &'static ViewUniformOffset,
        // The world camera only — a booth bake must never receive world cells (see the marker).
        &'static StaticGxView,
    );

    fn run<'w>(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext<'w>,
        (target, depth, view_offset, _marker): QueryItem<'w, '_, Self::ViewQuery>,
        world: &'w World,
    ) -> Result<(), NodeRunError> {
        let _t = super::gx_perf_guard(4);
        let gx = world.resource::<GxWorld>();
        if gx.visible.is_empty() && gx.visible_wmos.is_empty() {
            return Ok(());
        }
        let cache = world.resource::<GxGpuCache>();
        let pipes = world.resource::<GxPipelines>();
        let pipeline_cache = world.resource::<PipelineCache>();
        let Some(view_bind) = world.get_resource::<GxViewBind>() else {
            return Ok(());
        };
        let meshes = world.resource::<RenderAssets<RenderMesh>>();
        let allocator = world.resource::<MeshAllocator>();
        // Cells draw whole; a WMO region draws only the runs whose group the flood admitted
        // this frame (the selection rides beside the gpu state — `None` = draw everything).
        let mut resolved: Vec<(&GxCellGpu, &GxCellDraw, Option<&Vec<bool>>)> = Vec::new();
        for cell in &gx.visible {
            if let (Some(gpu), Some(draw)) = (cache.cells.get(cell), gx.cells.get(cell)) {
                resolved.push((gpu, draw, None));
            }
        }
        for (entity, sel) in &gx.visible_wmos {
            if let (Some(gpu), Some(draw)) = (cache.wmos.get(entity), gx.wmos.get(entity)) {
                resolved.push((gpu, draw, Some(sel)));
            }
        }
        if resolved.is_empty() {
            return Ok(());
        }
        // Encode any outstanding layer copies OUTSIDE the pass. `pending` drains through
        // interior mutability-free re-borrow: the cache is not mutable here, so copies are
        // (re-)encoded from a snapshot; encoding the same copy twice is idempotent (same src →
        // same dst), and PrepareResources rebuilds cells only on bake changes, so the window
        // is one frame. Simplicity over a drain flag — B2 revisits if it ever shows.
        {
            let encoder = render_context.command_encoder();
            for (gpu, _, _) in &resolved {
                for class in &gpu.classes {
                    for (src, layer) in &class.pending {
                        for mip in 0..class.mip_count.min(src.mip_level_count()) {
                            let mut dst = class.array.as_image_copy();
                            dst.mip_level = mip;
                            dst.origin.z = *layer;
                            let mut s = src.as_image_copy();
                            s.mip_level = mip;
                            encoder.copy_texture_to_texture(
                                s,
                                dst,
                                Extent3d {
                                    width: (class.size.width >> mip).max(1),
                                    height: (class.size.height >> mip).max(1),
                                    depth_or_array_layers: 1,
                                },
                            );
                        }
                    }
                }
            }
        }
        // The four bucket pipelines must all be compiled before the first draw (all-or-none:
        // a cell drawing only its opaque half would flash cutout content off for a frame).
        let mut ready: HashMap<(bool, bool), &RenderPipeline> = HashMap::default();
        for (k, id) in &pipes.pipelines {
            match pipeline_cache.get_render_pipeline(*id) {
                Some(p) => {
                    ready.insert(*k, p);
                }
                None => return Ok(()),
            }
        }
        let depth_attachment = depth.get_attachment(StoreOp::Store);
        let color_attachment = target.get_color_attachment();
        let mut pass = render_context.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some("static_gx"),
            color_attachments: &[Some(color_attachment)],
            depth_stencil_attachment: Some(depth_attachment),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_bind_group(0, &view_bind.0, &[view_offset.offset]);
        for (gpu, draw, sel) in &resolved {
            let Some(mesh) = meshes.get(draw.mesh.id()) else {
                continue;
            };
            let (Some(vslice), Some(islice)) = (
                allocator.mesh_vertex_slice(&draw.mesh.id()),
                allocator.mesh_index_slice(&draw.mesh.id()),
            ) else {
                continue;
            };
            let index_format = match &mesh.buffer_info {
                bevy::render::mesh::RenderMeshBufferInfo::Indexed { index_format, .. } => {
                    *index_format
                }
                bevy::render::mesh::RenderMeshBufferInfo::NonIndexed => continue,
            };
            pass.set_vertex_buffer(0, vslice.buffer.slice(..));
            pass.set_index_buffer(islice.buffer.slice(..), index_format);
            for run in &gpu.runs {
                // The PVS range selection (1429's collapse): a WMO run draws iff its group's
                // admission bit is set this frame; a cell run always draws.
                if let (Some(sel), Some(group)) = (sel, run.group) {
                    if !sel.get(usize::from(group)).copied().unwrap_or(false) {
                        continue;
                    }
                }
                pass.set_render_pipeline(ready[&(run.cutout, run.two_sided)]);
                pass.set_bind_group(1, &gpu.classes[run.class].bind_group, &[]);
                pass.draw_indexed(
                    (islice.range.start + run.index_range.start)
                        ..(islice.range.start + run.index_range.end),
                    i32::try_from(vslice.range.start).unwrap_or(0),
                    0..1,
                );
            }
        }
        Ok(())
    }
}

/// Wire the render half (called by the plugin only when armed).
pub(super) fn build(app: &mut App) {
    // (The shader registers in `crate::shaders` with the other engine WGSL — `embedded_asset!`
    // derives its path from the CALLING file, so registering here would mis-prefix it.)
    // The main-world half lives inside `StaticGx`; `publish_gx_world` (registered by the
    // plugin, chained after the scene walk) mirrors it into this standalone resource for
    // `ExtractResourcePlugin` to clone.
    app.add_plugins((
        ExtractResourcePlugin::<GxWorld>::default(),
        ExtractComponentPlugin::<StaticGxView>::default(),
    ));
    app.init_resource::<GxWorld>();
    app.add_systems(Update, mark_world_camera);
    let Some(render_app) = app.get_sub_app_mut(bevy::render::RenderApp) else {
        return;
    };
    render_app
        .init_resource::<GxGpuCache>()
        .add_systems(bevy::render::RenderStartup, init_pipelines)
        .add_systems(
            Render,
            (
                prepare_static_gx.in_set(RenderSystems::PrepareResources),
                prepare_view_bind.in_set(RenderSystems::PrepareBindGroups),
            ),
        )
        .add_render_graph_node::<ViewNodeRunner<StaticGxNode>>(Core3d, StaticGxLabel)
        .add_render_graph_edges(
            Core3d,
            (
                Node3d::MainOpaquePass,
                StaticGxLabel,
                Node3d::MainTransparentPass,
            ),
        );
}

/// Copy the collector's published half into the extractable resource.
pub(super) fn publish_gx_world(gx: Res<super::StaticGx>, mut out: ResMut<GxWorld>) {
    let _t = super::gx_perf_guard(2);
    out.cells.clone_from(&gx.world.cells);
    out.visible.clone_from(&gx.world.visible);
    out.wmos.clone_from(&gx.world.wmos);
    out.visible_wmos.clone_from(&gx.world.visible_wmos);
}
