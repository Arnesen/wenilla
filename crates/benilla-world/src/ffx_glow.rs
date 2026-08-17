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
use bevy::render::renderer::{RenderContext, RenderDevice, RenderQueue};
use bevy::render::texture::{CachedTexture, TextureCache};
use bevy::render::view::ViewTarget;
use bevy::render::{Render, RenderApp, RenderStartup, RenderSystems};

use crate::view::WorldCamera;

/// Marks a camera rendering with the faithful FFXGlow pass (extracted to the render world).
///
/// The pass is TWO things in one: the frame's single gamma→linear decode (mandatory on every view
/// that draws our gamma-byte materials — decision 0161) and the glow add on top. `gain_scale`
/// separates them: it multiplies the zone's [`FfxGlowGain`] for this view, so `0.0` keeps the
/// decode and drops the glow.
#[derive(Component, Clone, Copy, ExtractComponent)]
pub struct FfxGlow {
    /// Per-view multiplier on the zone glow weight. See [`Self::WORLD`] / [`Self::UI_PANE`].
    pub(crate) gain_scale: f32,
    /// Per-view multiplier on the haze mix `z` ([`FfxHazeMix`] — the drunk/underwater
    /// screen-toward-blur cross-fade). `1.0` only on the true world view: the haze is a
    /// *player-state* effect on the viewed scene, and a booth bake that stands in for a UI pane
    /// must never inherit it (the reference paints UI model widgets after the FFX pass).
    pub(crate) haze_scale: f32,
}

impl FfxGlow {
    /// The reference's own full-screen glow, at the zone's authored weight — the world camera:
    /// zone glow AND the player-state haze (drunk / camera-eye-submerged blur).
    pub const WORLD: Self = Self {
        gain_scale: 1.0,
        haze_scale: 1.0,
    };
    /// A portrait/booth bake at world parity for *lighting and glow* (decision 0638) — but never
    /// the haze: a bake stands in for a UI model widget, which the reference composites after
    /// the WorldFrame's FFX pass, so a drunk player's unit frame stays sharp while the world
    /// swims.
    pub const BOOTH: Self = Self {
        gain_scale: 1.0,
        haze_scale: 0.0,
    };
    /// **Decode only, no glow** — for a bake that stands in for a 1.12 *UI model widget*. The
    /// reference applies its FFX pass inside the WorldFrame's own paint (`0x48350e` → the apply
    /// hook `0x6cd890`, wow-re `ffxeffects.md`); every UI frame paints afterwards, at its own
    /// strata, so a `<PlayerModel>` pane is composited over an already-glowed world and never
    /// glows itself. (The give-away in-game: the reference's chat text and buttons don't bloom.)
    pub const UI_PANE: Self = Self {
        gain_scale: 0.0,
        haze_scale: 0.0,
    };
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

/// The haze mix `z` — the combine's screen-toward-blur cross-fade
/// (`out = lerp(screen, blur, z) + w·blur²`, the shipped FFXGlow.bls). The reference's glow
/// render packs it per frame from the **active player's** state (`0x6cb134`/`0x6cb599`, wow-re
/// `ffxeffects/scratch/drunk-blur-z.md`, decision 1009 §A):
/// `z = max(min(drunkByte,100)/100, submerged ? 84/255 : 0)` — fully blurred at 100 inebriation,
/// and a fixed ≈0.329 floor whenever the **camera eye** is in any liquid (the vanilla underwater
/// blur; `0x672470`'s eye-liquid probe, `0xf` = dry). Synced by [`sync_haze`]; uploaded as the
/// combine uniform's `z`, scaled per view by [`FfxGlow::haze_scale`].
#[derive(Resource, Clone, Default, ExtractResource)]
pub struct FfxHazeMix(pub f32);

/// The underwater haze floor: 84/255 (the reference's byte 84 in the COLOR z lane whenever the
/// eye-liquid probe reads non-dry — applied regardless of sobriety).
const HAZE_SUBMERGED_FLOOR: f32 = 84.0 / 255.0;

/// Sync [`FfxHazeMix`] from the two player-state inputs the reference's glow render reads each
/// frame: our own drunk byte (`PLAYER_BYTES_3` byte 1 → `min(b,100)/100`) and the camera-eye
/// submersion claim. Off-world both read empty → 0 (the glue screens never haze).
fn sync_haze(
    viewer: Res<crate::view::Viewer>,
    underwater: Option<Res<crate::liquid::Underwater>>,
    mut haze: ResMut<FfxHazeMix>,
) {
    let drunk = viewer.drunk;
    let submerged = if underwater.is_some_and(|u| u.0 != benilla_formats::Submersion::Dry) {
        HAZE_SUBMERGED_FLOOR
    } else {
        0.0
    };
    let target = drunk.max(submerged);
    if haze.0 != target {
        haze.0 = target;
    }
}

/// Sync the gain from the live zone lighting (the same source `sync_bloom` used). Off-world there
/// is no `Light.dbc` zone — the reference runs its `LightParams` **default 0.5** (wow-re
/// death-pass.md: "the zone/time-of-day ambient glow scalar, default 0.5"): the glue screens'
/// soft glow. (Before this, the glue rendered with the derive-default 0.0 — no glow at all.)
fn sync_gain(
    lighting: Option<Res<crate::lighting::WowLighting>>,
    live: Res<crate::schedule::WorldLive>,
    mut gain: ResMut<FfxGlowGain>,
) {
    let target = if live.0 {
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
    let shader: Handle<Shader> =
        asset_server.load("embedded://benilla_world/shaders/ffx_glow.wgsl");
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
/// wow-re blur-geometry.md; a ½ intermediate would be one downsample too many), plus the
/// GPU objects derived from them, so the node does not recreate them per frame.
#[derive(Component)]
struct FfxGlowTextures {
    quarter_a: CachedTexture,
    quarter_b: CachedTexture,
    /// The combine's 16-byte `(gain, death, haze, pad)` uniform — persistent, rewritten each
    /// frame with a queue write (queue writes land before the graph's submit executes).
    gain_buf: Buffer,
    /// The two Gauss bind groups bind only the ¼-res ping-pong views + sampler — all stable
    /// while the textures hold. The downsample and combine bind groups bind the view target's
    /// post-process source, which `ViewTarget::post_process_write` flips per call, so those two
    /// CANNOT be cached (they would sample last frame's texture) and stay per-frame in `run`.
    gauss_h_bind: BindGroup,
    gauss_v_bind: BindGroup,
}

fn prepare_textures(
    mut commands: Commands,
    mut texture_cache: ResMut<TextureCache>,
    render_device: Res<RenderDevice>,
    pipeline_cache: Res<PipelineCache>,
    pipelines: Res<FfxGlowPipelines>,
    views: Query<(Entity, &ExtractedCamera, Option<&FfxGlowTextures>), With<FfxGlow>>,
) {
    for (entity, camera, existing) in &views {
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
        // `TextureCache::get` hands the same textures back while the viewport holds, and the
        // derived objects only depend on them — rebuilding the component anyway would be right
        // back to per-frame bind-group creation.
        if existing.is_some_and(|t| {
            t.quarter_a.texture.id() == quarter_a.texture.id()
                && t.quarter_b.texture.id() == quarter_b.texture.id()
        }) {
            continue;
        }
        let layout_filter = pipeline_cache.get_bind_group_layout(&pipelines.layout_filter);
        let gauss_h_bind = render_device.create_bind_group(
            "ffx_glow_gauss_h",
            &layout_filter,
            &BindGroupEntries::sequential((&quarter_a.default_view, &pipelines.sampler)),
        );
        let gauss_v_bind = render_device.create_bind_group(
            "ffx_glow_gauss_v",
            &layout_filter,
            &BindGroupEntries::sequential((&quarter_b.default_view, &pipelines.sampler)),
        );
        let gain_buf = render_device.create_buffer(&BufferDescriptor {
            label: Some("ffx_glow_gain"),
            size: 16,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        commands.entity(entity).insert(FfxGlowTextures {
            quarter_a,
            quarter_b,
            gain_buf,
            gauss_h_bind,
            gauss_v_bind,
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
        // The haze mix, world-view only ([`FfxGlow::haze_scale`]): a booth bake never inherits
        // the drunk/underwater screen blur.
        let haze = world.resource::<FfxHazeMix>().0 * glow.haze_scale;
        let (Some(downsample), Some(gauss_h), Some(gauss_v), Some(combine)) = (
            pipeline_cache.get_render_pipeline(pipelines.downsample),
            pipeline_cache.get_render_pipeline(pipelines.gauss_h),
            pipeline_cache.get_render_pipeline(pipelines.gauss_v),
            pipeline_cache.get_render_pipeline(pipelines.combine),
        ) else {
            return Ok(()); // pipelines still compiling — draw the frame un-glowed
        };
        let render_device = render_context.render_device().clone();
        world.resource::<RenderQueue>().write_buffer(
            &textures.gain_buf,
            0,
            bytemuck::cast_slice(&[gain, death, haze, 0.0]),
        );
        let post = view_target.post_process_write();
        let layout_filter = pipeline_cache.get_bind_group_layout(&pipelines.layout_filter);
        let layout_combine = pipeline_cache.get_bind_group_layout(&pipelines.layout_combine);

        // The downsample binds `post.source`, which `post_process_write` flips per call — this
        // bind group cannot be cached (it would sample last frame's texture); the two Gauss ones
        // bind only the stable ¼-res ping-pong and ride prepared on [`FfxGlowTextures`].
        let down_bind = render_device.create_bind_group(
            "ffx_glow_down_quarter",
            &layout_filter,
            &BindGroupEntries::sequential((post.source, &pipelines.sampler)),
        );

        // The filter passes (byte-pinned chain): source→¼ (one Box4), ¼a→¼b (H), ¼b→¼a (V).
        let filter_passes: [(&str, &RenderPipeline, &BindGroup, &TextureView); 3] = [
            (
                "ffx_glow_down_quarter",
                downsample,
                &down_bind,
                &textures.quarter_a.default_view,
            ),
            (
                "ffx_glow_gauss_h",
                gauss_h,
                &textures.gauss_h_bind,
                &textures.quarter_b.default_view,
            ),
            (
                "ffx_glow_gauss_v",
                gauss_v,
                &textures.gauss_v_bind,
                &textures.quarter_a.default_view,
            ),
        ];
        for (label, pipeline, bind, dst) in filter_passes {
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
            pass.set_bind_group(0, bind, &[]);
            pass.draw(0..3, 0..1);
        }

        // Combine: screen + w·blur² (gamma-space byte math in the shader) → the post destination.
        // Also binds the flipping `post.source` — per-frame for the downsample's reason.
        let bind = render_device.create_bind_group(
            "ffx_glow_combine",
            &layout_combine,
            &BindGroupEntries::sequential((
                post.source,
                &pipelines.sampler,
                &textures.quarter_a.default_view,
                textures.gain_buf.as_entire_binding(),
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
            .init_resource::<FfxHazeMix>()
            .add_plugins((
                ExtractComponentPlugin::<FfxGlow>::default(),
                ExtractResourcePlugin::<FfxGlowGain>::default(),
                ExtractResourcePlugin::<FfxDeathFade>::default(),
                ExtractResourcePlugin::<FfxHazeMix>::default(),
            ))
            .add_systems(Update, (sync_gain, sync_haze, ensure_ffx_glow));
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
