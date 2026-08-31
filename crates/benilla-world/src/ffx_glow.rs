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
//!
//! **What a bake does NOT inherit.** The node's two other lanes are keyed on the *viewer's own
//! state*, not on the scene — the drunk/underwater haze and the ghost's FFXDeath combine — and the
//! reference runs its FFX pass inside the WorldFrame's paint, with every UI frame compositing
//! afterwards. [`FfxGlow::state_scale`] is that whole class in one field, `0` on every bake
//! (decision 1481).

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
    /// **Which FFX pass pair this view runs**, and therefore what drives the combine's `y` (the
    /// FFXDeath gate) and `z` (the haze mix) — see [`FfxState`].
    ///
    /// **The law, and why it is ONE field.** The reference runs its FFX pass inside the
    /// WorldFrame's own paint — a BEGIN/END pair, `0x6cd890`/`0x6cda70`, bracketed at
    /// `0x48350e`/`0x48379d` inside the one paint method `0x483460` (wow-re `death-pass.md` §5,
    /// VERIFIED). It **cannot reach a bake**, for reasons that need no frame ordering: the pass's
    /// render targets come from three module globals (`0xce8b5c` full, `0xce8ae8` quarter,
    /// `0xce8b98` backbuffer) and it never observes the ambient binding — while the reference's
    /// own portrait bake `0x524f60` binds no target at all and *copies* the framebuffer corner
    /// out (`0x58acd0`/`0x449bf0`), so the pixels it keeps are pre-pass in every ordering.
    ///
    /// The binary does not separate the two lanes: the haze is `primary.z` of the glow combine
    /// `0x6cb020`, which shares the single active-pass slot `0xce8bb4` with the death pass and
    /// runs through the same bracket into the same three targets. So they are **one class**, and
    /// this is one field. The zone glow is not in it (authored scene data, and a bake wants world
    /// parity for it — decision 0638), which is why [`Self::gain_scale`] stays separate.
    ///
    /// The haze had its own `haze_scale` and the death gate had none, so a released ghost's
    /// portraits baked through the FFXDeath combine and came back steel-blue luma (report B49,
    /// decision 1481). Naming the *class* rather than the member was the fix; naming the **pass
    /// pair** (decision 1731) is the same fix one step further, and it is the reference's own
    /// shape — see [`FfxState`].
    pub(crate) state: FfxState,
}

/// **Which of the reference's FFX pass pairs a view runs.** The binary builds *two*, and the
/// active-pass slot `[0xce8bb4]` holds whichever the screen that is painting installed:
///
/// - the **WorldFrame** pair — `0x6cc130` CFFXGlow → `[0xb4b350]`, `0x6cc690` CFFXDeath →
///   `[0xb4b39c]`, built at `0x481c46`; selected by `0x5de9c0` off `PLAYER_FLAGS_GHOST`;
/// - the **glue** pair — `[0xb414c4]` glow / `[0xb41468]` death, built at CGlueMgr init
///   `0x46a723`/`0x46a752`; selected by the select build's tail `0x472fd9 test dh,0x20` off the
///   selected roster record's `CHARSELECT+0xfc & 0x2000` (wow-re `death-pass.md` §4(c) +
///   `glue-select-model.md` §A2, both VERIFIED).
///
/// benilla's views coexist where the reference's screens take turns, so what the reference
/// expresses as one global slot written by whoever paints, we express as a property of the view.
/// That is *why* this is an enum and not a scale: a bake's [`Self::None`] cannot inherit a
/// player-state lane by arithmetic accident, which is the invariant decision 1481 legislated after
/// report B49, now structural.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FfxState {
    /// The **WorldFrame** pair — the live player's own state: the ghost death combine
    /// ([`FfxDeathFade`], off `PLAYER_FLAGS_GHOST`) and the drunk/underwater haze
    /// ([`FfxHazeMix`]). The true world view only.
    Player,
    /// The **glue** pair — the death combine iff the SELECTED ROSTER ROW is a ghost
    /// ([`GlueFfx::death`]), and no haze: a glue screen has no drunk player and no camera in a
    /// liquid, and the reference's glue pass pair carries no haze lane to read.
    Glue,
    /// Neither — a bake standing in for a 1.12 UI model widget, which the reference composites
    /// *after* the WorldFrame's own FFX bracket and which therefore sees no pass state at all.
    None,
}

impl FfxGlow {
    /// The reference's own full-screen glow, at the zone's authored weight — the world camera:
    /// zone glow AND the player-state passes (drunk / camera-eye-submerged blur, ghost death
    /// combine). The only view that carries the latter.
    pub const WORLD: Self = Self {
        gain_scale: 1.0,
        state: FfxState::Player,
    };
    /// The **glue screens' own** fullscreen render (login / create / character select) — the
    /// reference's glue pass pair, installed by CGlueMgr and applied around the glue scene's paint
    /// (`0x46fad3 call 0x6cd890` … `0x46fae0 jmp 0x6cda70`). It is not a bake standing in for a UI
    /// widget: it is the screen, so it runs the zone glow AND, for a ghost selection, the death
    /// combine — while the GlueXML frames over it composite afterwards, untinted, exactly as the
    /// reference's character list and buttons do.
    pub const GLUE_SCENE: Self = Self {
        gain_scale: 1.0,
        state: FfxState::Glue,
    };
    /// A portrait/booth bake at world parity for *lighting and glow* (decision 0638) — but never
    /// a player-state pass: a bake stands in for a UI model widget, which the reference
    /// composites after the WorldFrame's FFX pass. So a drunk player's unit frame stays sharp
    /// while the world swims, and a **ghost's portrait keeps its living face** while the world
    /// goes steel-blue (decision 1481, report B49).
    pub const BOOTH: Self = Self {
        gain_scale: 1.0,
        state: FfxState::None,
    };
    /// **Decode only, no glow** — for a bake that stands in for a 1.12 *UI model widget*. The
    /// reference applies its FFX pass inside the WorldFrame's own paint (the `0x6cd890`/`0x6cda70`
    /// BEGIN/END bracket at `0x48350e`/`0x48379d`, wow-re `death-pass.md` §5); every UI frame
    /// paints afterwards, at its own strata, so a `<PlayerModel>` pane is composited over an
    /// already-glowed world and never glows itself. (The give-away in-game: the reference's chat
    /// text and buttons don't bloom.)
    pub const UI_PANE: Self = Self {
        gain_scale: 0.0,
        state: FfxState::None,
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
/// Driven by `benilla-app`'s death arc off `PLAYER_FLAGS_GHOST`; uploaded as the combine
/// uniform's `y`, scaled per view by [`FfxGlow::state_scale`] — it is a **player-state** pass and
/// reaches the world view only (decision 1481).
#[derive(Resource, Clone, Default, ExtractResource)]
pub struct FfxDeathFade(pub f32);

/// **The glue screens' FFX state** — the two things the select build's tail writes, and nothing
/// else (`0x472fba`–`0x473007`, byte-read from wow-re's own disassembly; the fork is
/// `472fd9 test dh,0x20` on the selected record's `CHARSELECT+0xfc`):
///
/// ```text
/// 472fde  mov ecx,ds:0xb41468 ; call 0x6cde60   ; ghost  → install the glue DEATH pass
/// 472fe9  mov edx,ds:0x838570 ; mov [esi+0x110],edx      ; …and pin LightParams.glow
/// 472ff7  mov ecx,ds:0xb414c4 ; call 0x6cde60   ; living → install the glue GLOW pass
/// 473002  mov eax,ds:0x838574 ; mov [esi+0x110],eax      ; …and pin LightParams.glow
/// ```
///
/// `[esi+0x110]` is the DN/lighting singleton's `LightParams.glow` (`esi` = `0x6d48b0()` =
/// `&0xce9b60`) — the same scalar `0x6cb930` reads for the death combine's **alpha byte**
/// (`glow × 255`, `death-pass.md` §3) and the glow combine reads for its blur² weight. So on a glue
/// screen the glow weight is not a zone value at all: it is one of two constants, chosen by the
/// same bit that chooses the pass.
///
/// Written by the game's char-select feed; read by [`sync_gain`]'s off-world arm and by
/// [`FfxState::Glue`]'s combine.
#[derive(Resource, Clone, Copy, Default, PartialEq, Eq, Debug, ExtractResource)]
pub enum GlueFfx {
    /// **A glue ModelFFX widget is up and nothing has re-pinned it** — the login screen, the create
    /// screen, and a character-select screen with an empty account (where no select build runs).
    /// `CSimpleModelFFX`'s own **OnShow** override `0x46fa60` pins `[+0x110] = *0x8380b4 = 0.30f`
    /// and installs the glue GLOW pass (`0x46fa74`). OnShow, not per-frame: it tail-jumps to
    /// `0x76b260`, which runs the widget's `[frame+0x130]` handler (`0x76a0d0` maps `"OnShow"` to
    /// that slot), which is why the select build's own pin below is not overwritten every frame.
    #[default]
    Shown,
    /// **The character-select screen, LIVING selection.** The select build's tail installs the glue
    /// GLOW pass and pins `*0x838574 = 0.40f` (`0x472ff7`/`0x473002`). Note what this is *not*: a
    /// living selection is not "no post-process" — it is the glow pass at a pinned weight.
    SelectLiving,
    /// **The character-select screen, GHOST selection.** The glue DEATH pass, and `*0x838570 = 0.15f`
    /// (`0x472fde`/`0x472fe9`).
    SelectGhost,
}

impl GlueFfx {
    /// The glue pair's death gate — `1.0` only for [`Self::SelectGhost`]. Instant on both edges,
    /// like the world's: the reference's swap is a slot write with no time anchor in it, so
    /// clicking from a ghost row to a living one un-washes the scene on the same frame. And it runs
    /// on **every** selection, not only the first: `0x472a6d jne 0x472fba` sends an already-built
    /// record straight into the swap block.
    fn death(self) -> f32 {
        match self {
            Self::SelectGhost => 1.0,
            Self::Shown | Self::SelectLiving => 0.0,
        }
    }

    /// `LightParams.glow` while a glue screen is up — the DN/lighting singleton's `0xce9c70`
    /// (`0x6d48b0` is `mov eax,0xce9b60; ret`, so `[eax+0x110]` is that field).
    ///
    /// **All three values are byte-VERIFIED**, and an accessor-call-site census over all 41
    /// `call 0x6d48b0` sites found exactly these three writers and three readers (the ffx packs
    /// `0x6cb0e2`/`0x6cb557`/`0x6cb9e1`). Each constant has exactly one reference image-wide — the
    /// read itself — so they are literals, not `.data` defaults that something else moves.
    ///
    /// This is the scalar the death combine turns into its **primary alpha byte** (`0x6cb9ec`:
    /// `×255.0`, `+512.0`, `fstp`, `shr eax,0xe`) — 0.15 → 38, 0.40 → 102 — and which both combines
    /// use as the blur² weight. So a ghost's select screen glows at **less than half** a living
    /// one's, which is the opposite of what "the ghost look is the loud one" would suggest.
    ///
    /// (benilla keeps the float rather than the quantized byte: 38/255 = 0.14902 against 0.15 is
    /// under a thousandth, and the same scalar feeds the in-world glow lane where the reference
    /// carries a float too.)
    fn glow(self) -> f32 {
        match self {
            // `*0x8380b4`, the widget's OnShow pin.
            Self::Shown => 0.30,
            // `*0x838574`, the select build's living arm.
            Self::SelectLiving => 0.40,
            // `*0x838570`, the select build's ghost arm.
            Self::SelectGhost => 0.15,
        }
    }
}

/// The haze mix `z` — the combine's screen-toward-blur cross-fade
/// (`out = lerp(screen, blur, z) + w·blur²`, the shipped FFXGlow.bls). The reference's glow
/// render packs it per frame from the **active player's** state (`0x6cb134`/`0x6cb599`, wow-re
/// `ffxeffects/scratch/drunk-blur-z.md`, decision 1009 §A):
/// `z = max(min(drunkByte,100)/100, submerged ? 84/255 : 0)` — fully blurred at 100 inebriation,
/// and a fixed ≈0.329 floor whenever the **camera eye** is in any liquid (the vanilla underwater
/// blur; `0x672470`'s eye-liquid probe, `0xf` = dry). Synced by [`sync_haze`]; uploaded as the
/// combine uniform's `z`, scaled per view by [`FfxGlow::state_scale`].
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
    glue: Res<GlueFfx>,
    mut gain: ResMut<FfxGlowGain>,
) {
    let target = if live.0 {
        lighting.map_or(0.5, |l| l.glow)
    } else {
        // Off-world the glue screen pins it outright — the widget's OnShow, or the select build's
        // own fork ([`GlueFfx::glow`], all three byte-verified). It is never `LightParams`' default
        // here: this arm used to read a flat 0.5 on the reasoning that an off-world client falls
        // back to the table default, and the bytes say the glue screens pin it instead.
        glue.glow()
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

/// `WOW_DITHER=1` — arm the combine's deband dither (the shader's `glow.w`).
///
/// The frame's ONE quantization to 8 bits is the surface's sRGB present-encode of the combine's
/// gamma-space output, so a smooth surface steps in 1/255. Measured on a lit bare arm the shading
/// gradient is ~1.3 levels/px; a breathing idle drifts the body ~0.11 px/frame; so a pixel needs
/// ~7 FRAMES to cross one step and the shading updates at ~8 Hz under a 60 Hz frame rate. Motion
/// large enough to clear a level every frame hides it entirely — the reported
/// small-moves-tick / big-moves-smooth split.
///
/// **Off by default because it is a divergence.** The reference's framebuffer was 8-bit and
/// undithered; this lane is byte-exact against it (0161) and dithering trades that for a smoother
/// gradient. Bevy would normally apply its own in the tonemapping pass, but `Tonemapping::None`
/// makes that node return immediately, so the `DebandDither::Enabled` our camera inherits from
/// `Camera3d` never runs — this is the only place it can live.
fn dither_armed() -> f32 {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    match *ON.get_or_init(|| std::env::var_os("WOW_DITHER").is_some()) {
        true => 1.0,
        false => 0.0,
    }
}

/// The combine's `(x, y, z, w)` uniform for one view — the whole per-view scaling law in one
/// pure function, so it can be tested without a render world.
///
/// - **x** — the zone glow weight, scaled by [`FfxGlow::gain_scale`]: authored *scene* data, so a
///   portrait bake carries it at world parity (decision 0638) and only a UI model pane drops it,
///   keeping the combine for its gamma decode alone.
/// - **y/z** — the FFXDeath gate and the haze mix, both selected by [`FfxGlow::state`]: which pass
///   pair this view runs decides what drives them, and a bake runs neither pair (decision 1481,
///   report B49 — now structural rather than arithmetic, decision 1731).
/// - **w** — the deband-dither arm ([`dither_armed`]), 0 or 1.
fn combine_uniform(
    zone_gain: f32,
    world: FfxPassState,
    glue: FfxPassState,
    glow: &FfxGlow,
) -> [f32; 4] {
    let feed = match glow.state {
        FfxState::Player => world,
        FfxState::Glue => glue,
        FfxState::None => FfxPassState::INERT,
    };
    [
        zone_gain * glow.gain_scale,
        feed.death,
        feed.haze,
        dither_armed(),
    ]
}

/// One pass pair's live state, as [`combine_uniform`] consumes it: the death gate and the haze mix
/// that pair carries. The glue pair has no haze lane at all, which is a fact about the reference
/// and not a value we happen to leave at zero.
#[derive(Clone, Copy)]
struct FfxPassState {
    death: f32,
    haze: f32,
}

impl FfxPassState {
    /// The **WorldFrame** pair's live state: the ghost gate off `PLAYER_FLAGS_GHOST` and the
    /// drunk/underwater haze, both of the live player.
    fn world(death: f32, haze: f32) -> Self {
        Self { death, haze }
    }

    /// The **glue** pair's live state. Death only, and the constructor is where that is enforced:
    /// the reference's haze is `primary.z` of the *WorldFrame* glow combine `0x6cb020`, packed from
    /// the active player's inebriation and the camera-eye liquid probe (`drunk-blur-z.md`) — a glue
    /// screen has neither, and CGlueMgr's pair has no lane to read them into. Passing a haze here
    /// should be impossible rather than merely wrong.
    fn glue(death: f32) -> Self {
        Self { death, haze: 0.0 }
    }

    /// What a view running neither pair reads — a bake.
    const INERT: Self = Self {
        death: 0.0,
        haze: 0.0,
    };
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
        let uniform = combine_uniform(
            world.resource::<FfxGlowGain>().0,
            FfxPassState::world(
                world.resource::<FfxDeathFade>().0,
                world.resource::<FfxHazeMix>().0,
            ),
            FfxPassState::glue(world.resource::<GlueFfx>().death()),
            glow,
        );
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
            bytemuck::cast_slice(&uniform),
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
            .init_resource::<GlueFfx>()
            .init_resource::<FfxHazeMix>()
            .add_plugins((
                ExtractComponentPlugin::<FfxGlow>::default(),
                ExtractResourcePlugin::<FfxGlowGain>::default(),
                ExtractResourcePlugin::<FfxDeathFade>::default(),
                ExtractResourcePlugin::<GlueFfx>::default(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The world pair, live: a released ghost, sober.
    fn ghost_world() -> FfxPassState {
        FfxPassState::world(1.0, 0.0)
    }
    /// The glue pair with a living selection — the common case.
    fn living_glue() -> FfxPassState {
        FfxPassState::glue(0.0)
    }

    /// The zone glow is scene data — a portrait bake wants it at world parity (decision 0638) —
    /// while the death and haze lanes belong to a *pass pair*, and a bake runs neither
    /// (decision 1481, now structural: 1731). One preset table, checked as a whole so a new preset
    /// can't quietly join the wrong side.
    #[test]
    fn only_the_two_screen_views_run_a_pass_pair() {
        for (name, view) in [("BOOTH", FfxGlow::BOOTH), ("UI_PANE", FfxGlow::UI_PANE)] {
            assert_eq!(
                view.state,
                FfxState::None,
                "{name} is a bake standing in for a UI widget — it runs no pass pair"
            );
        }
        assert_eq!(FfxGlow::WORLD.state, FfxState::Player);
        assert_eq!(FfxGlow::GLUE_SCENE.state, FfxState::Glue);
        // The glow half is unchanged by that law: a bake still glows like the world.
        assert_eq!(FfxGlow::BOOTH.gain_scale, 1.0);
        assert_eq!(FfxGlow::GLUE_SCENE.gain_scale, 1.0);
        assert_eq!(FfxGlow::UI_PANE.gain_scale, 0.0);
    }

    /// B49: a released ghost's portrait baked through the FFXDeath combine and came back steel-blue
    /// luma. The gate reaches the world view and nothing else — while the zone glow still does.
    #[test]
    fn a_ghosts_bake_is_not_death_combined() {
        let zone = 0.5;
        assert_eq!(
            combine_uniform(zone, ghost_world(), living_glue(), &FfxGlow::WORLD),
            [0.5, 1.0, 0.0, 0.0]
        );
        assert_eq!(
            combine_uniform(zone, ghost_world(), living_glue(), &FfxGlow::BOOTH),
            [0.5, 0.0, 0.0, 0.0],
            "the booth keeps the zone glow and drops the death gate"
        );
        assert_eq!(
            combine_uniform(zone, ghost_world(), living_glue(), &FfxGlow::UI_PANE),
            [0.0, 0.0, 0.0, 0.0]
        );
    }

    /// The control this refactor must not disturb: the drunk/underwater haze was already
    /// world-only, and still is.
    #[test]
    fn a_drunk_players_bake_stays_sharp() {
        let (zone, drunk) = (0.5, FfxPassState::world(0.0, 1.0));
        assert_eq!(
            combine_uniform(zone, drunk, living_glue(), &FfxGlow::WORLD),
            [0.5, 0.0, 1.0, 0.0]
        );
        assert_eq!(
            combine_uniform(zone, drunk, living_glue(), &FfxGlow::BOOTH),
            [0.5, 0.0, 0.0, 0.0]
        );
    }

    /// **The two pairs are independent, which is the whole point of naming them** (decision 1731).
    /// The reference has one active-pass slot because its screens take turns; ours coexist, so the
    /// invariant has to be stated: a ghost on the CHARACTER-SELECT list death-combines the glue
    /// scene and nothing else, and a released ghost in the WORLD never reaches the glue view.
    #[test]
    fn each_pass_pair_reaches_only_its_own_view() {
        let zone = 0.5;
        let ghost_glue = FfxPassState::glue(1.0);
        let alive_world = FfxPassState::world(0.0, 0.0);
        assert_eq!(
            combine_uniform(zone, alive_world, ghost_glue, &FfxGlow::GLUE_SCENE),
            [0.5, 1.0, 0.0, 0.0],
            "a ghost roster row death-combines the glue scene"
        );
        assert_eq!(
            combine_uniform(zone, alive_world, ghost_glue, &FfxGlow::WORLD),
            [0.5, 0.0, 0.0, 0.0],
            "…and never the world view, whose own player is alive"
        );
        assert_eq!(
            combine_uniform(zone, ghost_world(), living_glue(), &FfxGlow::GLUE_SCENE),
            [0.5, 0.0, 0.0, 0.0],
            "and a world ghost never reaches the glue view"
        );
    }

    /// The glue pair carries **no haze lane** — a fact about the reference (its pair is built by
    /// CGlueMgr; the haze is `primary.z` of the *WorldFrame* glow combine), enforced by
    /// [`FfxPassState::glue`] having no way to say otherwise. Pinned so the day someone widens that
    /// constructor, a test argues back — and pinned against a fully hazed WORLD, which is the state
    /// that would leak if the two pairs were ever refolded into one.
    #[test]
    fn the_glue_pair_never_hazes() {
        let drunk_world = FfxPassState::world(0.0, 1.0);
        assert_eq!(
            combine_uniform(
                0.5,
                drunk_world,
                FfxPassState::glue(1.0),
                &FfxGlow::GLUE_SCENE
            ),
            [0.5, 1.0, 0.0, 0.0],
            "the glue view death-combines on its own gate and never hazes"
        );
    }

    /// GOLDEN — the three writers of the glue screens' `LightParams.glow`, and the alpha byte the
    /// death combine quantizes the ghost one into. Every number byte-VERIFIED (wow-re
    /// `glue-select-ghost-treatment.md`); each constant has exactly one reference image-wide.
    ///
    /// The default is a shown-but-unselected glue screen — login, create, and an empty account —
    /// which is the state the login screen renders in and which is NOT either select arm.
    #[test]
    fn the_glue_screens_pin_three_verified_glow_weights() {
        assert_eq!(GlueFfx::default(), GlueFfx::Shown);
        assert_eq!(
            GlueFfx::Shown.glow(),
            0.30,
            "*0x8380b4, the widget's OnShow"
        );
        assert_eq!(GlueFfx::SelectLiving.glow(), 0.40, "*0x838574");
        assert_eq!(GlueFfx::SelectGhost.glow(), 0.15, "*0x838570");
        // Only the ghost arm installs the death pass, and it is the only arm that does.
        assert_eq!(GlueFfx::SelectGhost.death(), 1.0);
        assert_eq!(GlueFfx::Shown.death(), 0.0);
        assert_eq!(GlueFfx::SelectLiving.death(), 0.0);
        // The combine's primary alpha byte (`0x6cb9ec`: ×255, +512, fstp, shr 14) — 38 ghosted,
        // 102 living. A ghost's screen glows at less than half a living one's.
        let alpha_byte = |glow: f32| ((glow * 255.0 + 512.0).to_bits() >> 14) & 0xff;
        assert_eq!(alpha_byte(GlueFfx::SelectGhost.glow()), 38);
        assert_eq!(alpha_byte(GlueFfx::SelectLiving.glow()), 102);
    }
}
