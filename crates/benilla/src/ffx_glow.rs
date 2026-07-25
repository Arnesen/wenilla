//! The faithful **FFXGlow** post pass (decision 0158) — the reference's full-screen glow
//! (`FFXEffects.cpp` / `FFXGlow.bls`), replacing the Bevy-`Bloom` approximation and its two
//! eye-tuned constants.
//!
//! Pipeline (all byte-grounded — the shipped ARB programs + wow-re's `ffxeffects` T3 node):
//! scene → ½ → ¼ downsample (dims floored at 8, `ffx_compute_rt_dims`) → separable Gauss4
//! (weights ⅛ ⅜ ⅜ ⅛, shipped constants) → `out = screen + w·blur²` in gamma bytes, `w` = the
//! per-zone `LightParams.glow` weight (authored data; the ONLY input, no knobs). The gamma-space
//! byte math and the square-law are in `shaders/ffx_glow.wgsl`.
//!
//! This is the frame's sole glow pass — it won the A/B against Bevy's `Bloom`, and that fallback
//! (plus its debug toggle) is gone. Applied to both the world camera and the portrait-booth bake
//! cameras (`portrait::mod`): the booth rides the same final node so its bake reads at exact world
//! parity, the FFXGlow combine owning the frame's ONE gamma decode.

use bevy::core_pipeline::core_3d::graph::{Core3d, Node3d};
use bevy::core_pipeline::FullscreenShader;
use bevy::ecs::query::QueryItem;
use bevy::prelude::*;
use bevy::render::camera::ExtractedCamera;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_graph::{
    NodeRunError, RenderGraphContext, RenderGraphExt, RenderLabel, ViewNode, ViewNodeRunner,
};
use bevy::render::render_resource::binding_types::{sampler, texture_2d, uniform_buffer_sized};
use bevy::render::render_resource::*;
use bevy::render::renderer::{RenderContext, RenderDevice};
use bevy::render::texture::{CachedTexture, TextureCache};
use bevy::render::view::ViewTarget;
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};

use crate::player::WorldCamera;

/// Marks a camera rendering with the faithful FFXGlow pass (extracted to the render world).
///
/// The pass is TWO things in one: the frame's single gamma→linear decode (mandatory on every view
/// that draws our gamma-byte materials — decision 0161) and the glow add on top. `gain_scale`
/// separates them: it multiplies the zone's [`FfxGlowGain`] for this view, so `0.0` keeps the
/// decode and drops the glow.
#[derive(Component, Clone, Copy, ExtractComponent)]
pub struct FfxGlow {
    /// Per-view multiplier on the zone glow weight. See [`Self::WORLD`] / [`Self::UI_PANE`].
    pub gain_scale: f32,
}

impl FfxGlow {
    /// The reference's own full-screen glow, at the zone's authored weight — the world camera, and
    /// every booth bake that wants world parity.
    pub const WORLD: Self = Self { gain_scale: 1.0 };
    /// **Decode only, no glow** — for a bake that stands in for a 1.12 *UI model widget*. The
    /// reference applies its FFX pass inside the WorldFrame's own paint (`0x48350e` → the apply
    /// hook `0x6cd890`, wow-re `ffxeffects.md`); every UI frame paints afterwards, at its own
    /// strata, so a `<PlayerModel>` pane is composited over an already-glowed world and never
    /// glows itself. (The give-away in-game: the reference's chat text and buttons don't bloom.)
    pub const UI_PANE: Self = Self { gain_scale: 0.0 };
}

impl Default for FfxGlow {
    fn default() -> Self {
        Self::WORLD
    }
}

/// The per-zone glow weight `w` (`LightParams.glow` — authored data, synced from
/// [`crate::lighting::WowLighting`] every frame).
#[derive(Resource, Clone, ExtractResource)]
pub struct FfxGlowGain(pub f32);

/// The FFXDeath gate (decision 0308 §7, byte-VERIFIED wow-re death-pass.md): `1.0` while the
/// player is a released ghost — the combine swaps to the FFXDeath program whole — else `0.0`.
/// INSTANT on both edges (the client has no time ramp; the ghost tint is a shader constant).
/// Driven by [`crate::death`] off `PLAYER_FLAGS_GHOST`; uploaded as the combine uniform's `y`.
#[derive(Resource, Clone, Default, ExtractResource)]
pub struct FfxDeathFade(pub f32);

/// Sync the gain from the live zone lighting (the same source `sync_bloom` used). Off-world there
/// is no `Light.dbc` zone — the reference runs its `LightParams` **default 0.5** (wow-re
/// death-pass.md: "the zone/time-of-day ambient glow scalar, default 0.5"): the glue screens'
/// soft glow. (Before this, the glue rendered with the derive-default 0.0 — no glow at all.)
fn sync_gain(
    lighting: Option<Res<crate::lighting::WowLighting>>,
    state: Option<Res<State<crate::char_select::ClientState>>>,
    mut gain: ResMut<FfxGlowGain>,
) {
    let in_world = state
        .as_deref()
        .is_some_and(|s| *s.get() == crate::char_select::ClientState::InWorld);
    let target = if in_world {
        lighting.map_or(0.5, |l| l.glow)
    } else {
        0.5
    };
    if gain.0 != target {
        gain.0 = target;
    }
}

/// The FFXGlow pass is MANDATORY on the world camera in the gamma lane (decision 0161): its
/// combine owns the frame's single gamma→linear decode — without it the whole frame presents
/// over-bright. Insert on any world camera that lacks it (idempotent; spawn sites also add it).
fn ensure_ffx_glow(
    mut commands: Commands,
    cam: Query<Entity, (With<WorldCamera>, Without<FfxGlow>)>,
    with: Query<Entity, With<FfxGlow>>,
) {
    // Perf-bisect kill-switch: $WOW_NO_FFX strips the pass from every camera (the frame then shows
    // undecoded gamma — ~2.2× bright — which is fine: this is a measurement mode, not a look mode).
    if std::env::var_os("WOW_NO_FFX").is_some() {
        for e in &with {
            commands.entity(e).remove::<FfxGlow>();
        }
        return;
    }
    for e in &cam {
        commands.entity(e).insert(FfxGlow::WORLD);
    }
}

// ---------------------------------------------------------------- render world

#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
struct FfxGlowLabel;

/// Layouts, sampler, and the four cached pipelines (downsample ×2 share one).
#[derive(Resource)]
struct FfxGlowPipelines {
    layout_filter: BindGroupLayoutDescriptor,
    layout_combine: BindGroupLayoutDescriptor,
    sampler: Sampler,
    downsample: CachedRenderPipelineId,
    gauss_h: CachedRenderPipelineId,
    gauss_v: CachedRenderPipelineId,
    combine: CachedRenderPipelineId,
}

fn init_pipelines(
    mut commands: Commands,
    render_device: Res<RenderDevice>,
    fullscreen_shader: Res<FullscreenShader>,
    asset_server: Res<AssetServer>,
    pipeline_cache: Res<PipelineCache>,
) {
    let shader: Handle<Shader> = asset_server.load("shaders/ffx_glow.wgsl");
    // Filter passes bind (tex, sampler); the combine additionally binds (blur tex, gain uniform).
    let layout_filter = BindGroupLayoutDescriptor::new(
        "ffx_glow_filter_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
            ),
        ),
    );
    let layout_combine = BindGroupLayoutDescriptor::new(
        "ffx_glow_combine_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                texture_2d(TextureSampleType::Float { filterable: true }),
                uniform_buffer_sized(false, Some(std::num::NonZero::new(16).unwrap())),
            ),
        ),
    );
    let sampler = render_device.create_sampler(&SamplerDescriptor {
        min_filter: FilterMode::Linear,
        mag_filter: FilterMode::Linear,
        address_mode_u: AddressMode::ClampToEdge,
        address_mode_v: AddressMode::ClampToEdge,
        ..Default::default()
    });
    let pipeline = |label: &'static str,
                    layout: &BindGroupLayoutDescriptor,
                    entry: &'static str|
     -> RenderPipelineDescriptor {
        RenderPipelineDescriptor {
            label: Some(label.into()),
            layout: vec![layout.clone()],
            vertex: fullscreen_shader.to_vertex_state(),
            fragment: Some(FragmentState {
                shader: shader.clone(),
                shader_defs: vec![],
                entry_point: Some(entry.into()),
                targets: vec![Some(ColorTargetState {
                    format: ViewTarget::TEXTURE_FORMAT_HDR,
                    blend: None,
                    write_mask: ColorWrites::ALL,
                })],
            }),
            ..default()
        }
    };
    let downsample = pipeline_cache.queue_render_pipeline(pipeline(
        "ffx_glow_downsample",
        &layout_filter,
        "fs_downsample",
    ));
    let gauss_h =
        pipeline_cache.queue_render_pipeline(pipeline("ffx_glow_h", &layout_filter, "fs_gauss_h"));
    let gauss_v =
        pipeline_cache.queue_render_pipeline(pipeline("ffx_glow_v", &layout_filter, "fs_gauss_v"));
    let combine = pipeline_cache.queue_render_pipeline(pipeline(
        "ffx_glow_combine",
        &layout_combine,
        "fs_combine",
    ));
    commands.insert_resource(FfxGlowPipelines {
        layout_filter,
        layout_combine,
        sampler,
        downsample,
        gauss_h,
        gauss_v,
        combine,
    });
}

/// The two ¼-res ping-pong targets (the reference downsamples full→¼ in ONE Box4 pass —
/// wow-re blur-geometry.md; a ½ intermediate would be one downsample too many).
#[derive(Component)]
struct FfxGlowTextures {
    quarter_a: CachedTexture,
    quarter_b: CachedTexture,
}

fn prepare_textures(
    mut commands: Commands,
    mut texture_cache: ResMut<TextureCache>,
    render_device: Res<RenderDevice>,
    views: Query<(Entity, &ExtractedCamera), With<FfxGlow>>,
) {
    for (entity, camera) in &views {
        let Some(vp) = camera.physical_viewport_size else {
            continue;
        };
        // The reference's RT-dim chain: ½ and ¼, floored (clamp ≥8 — `ffx_compute_rt_dims`).
        let mut tex = |label: &'static str, w: u32, h: u32| {
            texture_cache.get(
                &render_device,
                TextureDescriptor {
                    label: Some(label),
                    size: Extent3d {
                        width: w.max(8),
                        height: h.max(8),
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: TextureDimension::D2,
                    format: ViewTarget::TEXTURE_FORMAT_HDR,
                    usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
            )
        };
        let quarter_a = tex("ffx_glow_quarter_a", vp.x / 4, vp.y / 4);
        let quarter_b = tex("ffx_glow_quarter_b", vp.x / 4, vp.y / 4);
        commands.entity(entity).insert(FfxGlowTextures {
            quarter_a,
            quarter_b,
        });
    }
}

#[derive(Default)]
struct FfxGlowNode;

impl ViewNode for FfxGlowNode {
    type ViewQuery = (
        &'static ViewTarget,
        &'static FfxGlowTextures,
        &'static FfxGlow,
    );

    fn run<'w>(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext<'w>,
        (view_target, textures, glow): QueryItem<'w, '_, Self::ViewQuery>,
        world: &'w World,
    ) -> Result<(), NodeRunError> {
        let pipelines = world.resource::<FfxGlowPipelines>();
        let pipeline_cache = world.resource::<PipelineCache>();
        // The zone weight, scaled per view: 1× on the world (and the portrait bakes), 0× on a UI
        // model pane, where the combine runs only for its gamma decode ([`FfxGlow::UI_PANE`]).
        let gain = world.resource::<FfxGlowGain>().0 * glow.gain_scale;
        let death = world.resource::<FfxDeathFade>().0;
        let (Some(downsample), Some(gauss_h), Some(gauss_v), Some(combine)) = (
            pipeline_cache.get_render_pipeline(pipelines.downsample),
            pipeline_cache.get_render_pipeline(pipelines.gauss_h),
            pipeline_cache.get_render_pipeline(pipelines.gauss_v),
            pipeline_cache.get_render_pipeline(pipelines.combine),
        ) else {
            return Ok(()); // pipelines still compiling — draw the frame un-glowed
        };
        let render_device = render_context.render_device().clone();
        let gain_buf = render_device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some("ffx_glow_gain"),
            contents: bytemuck::cast_slice(&[gain, death, 0.0, 0.0]),
            usage: BufferUsages::UNIFORM,
        });
        let post = view_target.post_process_write();
        let layout_filter = pipeline_cache.get_bind_group_layout(&pipelines.layout_filter);
        let layout_combine = pipeline_cache.get_bind_group_layout(&pipelines.layout_combine);

        // The filter passes (byte-pinned chain): source→¼ (one Box4), ¼a→¼b (H), ¼b→¼a (V).
        let filter_passes: [(&str, &RenderPipeline, &TextureView, &TextureView); 3] = [
            (
                "ffx_glow_down_quarter",
                downsample,
                post.source,
                &textures.quarter_a.default_view,
            ),
            (
                "ffx_glow_gauss_h",
                gauss_h,
                &textures.quarter_a.default_view,
                &textures.quarter_b.default_view,
            ),
            (
                "ffx_glow_gauss_v",
                gauss_v,
                &textures.quarter_b.default_view,
                &textures.quarter_a.default_view,
            ),
        ];
        for (label, pipeline, src, dst) in filter_passes {
            let bind = render_device.create_bind_group(
                label,
                &layout_filter,
                &BindGroupEntries::sequential((src, &pipelines.sampler)),
            );
            let mut pass =
                render_context
                    .command_encoder()
                    .begin_render_pass(&RenderPassDescriptor {
                        label: Some(label),
                        color_attachments: &[Some(RenderPassColorAttachment {
                            view: dst,
                            depth_slice: None,
                            resolve_target: None,
                            ops: Operations::default(),
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                    });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }

        // Combine: screen + w·blur² (gamma-space byte math in the shader) → the post destination.
        let bind = render_device.create_bind_group(
            "ffx_glow_combine",
            &layout_combine,
            &BindGroupEntries::sequential((
                post.source,
                &pipelines.sampler,
                &textures.quarter_a.default_view,
                gain_buf.as_entire_binding(),
            )),
        );
        let mut pass = render_context
            .command_encoder()
            .begin_render_pass(&RenderPassDescriptor {
                label: Some("ffx_glow_combine"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: post.destination,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations::default(),
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        pass.set_pipeline(combine);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

pub struct FfxGlowPlugin;

impl Plugin for FfxGlowPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(FfxGlowGain(0.647)) // overwritten by sync_gain from zone data
            .init_resource::<FfxDeathFade>()
            .add_plugins((
                ExtractComponentPlugin::<FfxGlow>::default(),
                ExtractResourcePlugin::<FfxGlowGain>::default(),
                ExtractResourcePlugin::<FfxDeathFade>::default(),
            ))
            .add_systems(Update, (sync_gain, ensure_ffx_glow));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .add_systems(RenderStartup, init_pipelines)
            .add_systems(
                Render,
                prepare_textures.in_set(RenderSystems::PrepareResources),
            )
            .add_render_graph_node::<ViewNodeRunner<FfxGlowNode>>(Core3d, FfxGlowLabel)
            .add_render_graph_edges(
                Core3d,
                (
                    Node3d::StartMainPassPostProcessing,
                    FfxGlowLabel,
                    Node3d::Bloom,
                ),
            );
    }
}
