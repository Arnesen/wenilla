//! The world as the UI's **backdrop quad** — the seam that put the UI-over-world blend back into
//! gamma bytes (the third and last piece of the composite lane 0161 and 0254 built).
//!
//! ## The seam that was left
//!
//! Both halves of the frame already composite the way the reference's fixed-function device does,
//! and each does it by the same trick. The world (0161): every world shader emits raw gamma bytes,
//! the blur runs on them, and the FFXGlow combine takes the frame's one `srgb_to_linear` so the
//! sRGB present-encode restores the byte. The UI (0254): `ui_quad.wgsl` emits gamma values into an
//! `Rgba8UnormSrgb` target, so the store encodes, the blend's destination read decodes, and every
//! hardware blend is therefore arithmetic on the gamma value — `alphaMode="ADD"` really is
//! `dst + texel·α`, clamped, exactly as EGxBlend 3 does it.
//!
//! Two correct byte lanes, and **the UI-over-world blend fell in the seam between them.** The UI
//! camera composited its finished image onto the swapchain through its output blit, and the
//! swapchain view is sRGB, so that one blend — the only one that mixes UI with world — ran in
//! linear. 0254 named it as a residual and described it as "a small deviation on antialiased UI
//! edges over the world". It is not an edge artefact: it is every translucent UI pixel over the
//! 3D world, at full area.
//!
//! Measured on the chat dock, which is the most translucent surface the client has.
//! `ChatFrameTab`'s body is black at α = 102/255, so a docked tab must leave the scene at **60 %**
//! of its brightness; the unselected tab (frame α 0.5) at 80 %. Against a bare-scene capture of
//! the same pixels (`ui-chat-tabhover` at `$WOW_TABHOVER=9`) they measured **77.5 %** and **89 %** —
//! and a linear composite predicts exactly that, to within a byte, at every point checked
//! (scene 93 → 72 vs 72.5 predicted; 70 → 54 vs 53.9; 95 → 74 vs 74.1). The tab had almost no
//! plate, so the hover glow — an ADD, at full strength — sat on nothing and read as a blue lozenge
//! floating on grass, louder than the tab that was actually selected. That is what the director
//! reported, and it was never a chat bug.
//!
//! ## The fix: put the world *inside* the UI's byte buffer
//!
//! Not a third lane. The world camera renders to an off-screen image instead of the swapchain, and
//! that image is drawn as the **first quad of the UI pass** — the ground everything else is painted
//! on. Every UI blend over the world is then the same blend as every UI blend over UI: the one
//! 0254 already verified, in the target it already verified it in. The output blit stops blending
//! entirely (it now carries an opaque frame), which retires the `rgb·a²` hazard 0254 had to patch
//! `PREMULTIPLIED_ALPHA_BLENDING` around.
//!
//! **The image is un-encoded float, and both halves of that are borrowed rather than invented** —
//! it is the portrait booths' own target format, chosen there for the same two reasons
//! ([`crate::portrait`]'s `new_target_image`). Un-encoded, because the UI arc composites in gamma
//! and takes its one decode at the end: a backdrop that pre-encoded would land a second encode in
//! that chain. `ui_quad.wgsl`'s ordinary arm re-encodes what it samples (`linear_to_srgb`), which
//! turns FFXGlow's linear output back into the client's byte — the same round trip a booth image
//! takes, and exact in f32. Float rather than `Rgba8Unorm`, because quantizing *un-encoded* values
//! to 8 bits is B126's banding collapse (decision 0804): below display byte 100 an un-encoded 8-bit
//! grid reaches ~25 levels where the gamma backbuffer has 100.
//!
//! Nothing about the world lane changes: FFXGlow keeps its decode, the frame still holds exactly
//! one, and an opaque world pixel must come out byte-identical to before. That identity is the
//! regression test, the same one 0161 used.
//!
//! ## …and that seam turned out to be a dial: **render scale** (decision 1639)
//!
//! Once the world is a picture the UI paints on, the picture does not have to be the window's size.
//! [`RenderScale`] sizes it — the world renders at `window × scale`, the quad still covers the
//! window, and **the UI is untouched at native resolution**, because [`emit_backdrop_quad`]
//! measures the quad in the window's LOGICAL size and nothing here can move that. That is the whole
//! reason this is worth having rather than "run the game in a smaller window": text, icons and
//! frame art stay exactly as sharp as they were, and only the 3D pays.
//!
//! **Below 1 it buys frames; above 1 it is supersampling** — and above 1 is the half this machine
//! can measure, since `gxMultisample` defaults to off (1629) and the client therefore ships with no
//! antialiasing of any kind. At exactly 2.0 the plain bilinear resolve *is* a 2×2 box average (the
//! destination pixel centre lands on the corner four texels share, weighting each 0.25), so SSAA×2
//! needs no filter of its own.
//!
//! **The one number that must not move is the camera's LOGICAL viewport**, and
//! [`render_target_for`] is built around holding it fixed — [`retarget_world_camera`] says what
//! reads it (every pick ray in the client) and why getting it wrong is B169 over again.

use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::prelude::*;
use bevy::render::camera::MipBias;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::window::PrimaryWindow;

use crate::ui_pass::{UiQuad, UiQuads};
use benilla_world::view::WorldCamera;

/// The **render scale** — benilla's own CVar `renderScale`, no 1.12 counterpart.
///
/// The era's answer to "my machine is too slow" was `gxResolution`: drop the whole backbuffer, UI
/// and all, and in fullscreen mode-set the display to match. We ship no exclusive mode at all
/// (decision 1627) and our UI is a separate pass over an off-screen world, so we can offer the
/// strictly better version of that trade — **shrink the 3D, keep the interface**. It is the same
/// knob every engine since has grown (Godot's `scaling_3d_scale`, Unity URP's `renderScale`,
/// Unreal's `r.ScreenPercentage`), and it is the standard lever for the one machine class we have a
/// real measurement from: the Steam Deck sitting at 94 % GPU busy (B329).
///
/// **Default 1.0 — off, and it has to be**, so every visual golden in the tree keeps meaning what
/// it meant: at 1.0 [`render_target_for`] returns the window's own physical size and the window's
/// own scale factor, unrounded and unmultiplied, which is bit-for-bit the pre-1639 lane.
#[derive(Resource, Clone, Copy, PartialEq, Debug)]
pub(crate) struct RenderScale(pub(crate) f32);

/// The settable range of [`RenderScale`], shared by the CVar apply and the `$WOW_RENDER_SCALE` env
/// knob so the two cannot drift.
///
/// Wider than a settings row would offer (a slider belongs at 50–200 %, where every engine puts
/// it): this is the clamp that stops an absurd *value*, not the one that shapes the UI. The upper
/// end is deliberately past 2 because supersampling is also the instrument — the only way to price
/// a pixel on a machine whose present is railed at the display's grant (0362, and `crate::video`'s
/// note that macOS honours neither `AutoNoVsync` nor `Immediate`).
pub(crate) const RENDER_SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.25..=4.0;

/// The per-axis ceiling on the backdrop, in physical px — `wgpu::Limits::default()`'s
/// `max_texture_dimension_2d`, which is what Bevy asks the adapter for unless `WgpuSettings` says
/// otherwise.
///
/// Not a render-scale concern in origin: a window wider than this has always been a texture wgpu
/// refuses to create, and before 1639 nothing here looked. The ceiling is applied to the *ratio*
/// rather than to each axis (see [`render_target_for`]) so hitting it shrinks the picture instead of
/// reshaping it.
const MAX_RENDER_AXIS: u32 = 8192;

impl Default for RenderScale {
    /// `$WOW_RENDER_SCALE` overrides the default, **session-only** — the A/B lever, the same posture
    /// as `$WOW_MSAA` and `$WOW_FARCLIP`: a value pinned into `config.toml` would make a
    /// measurement sticky across relaunches.
    fn default() -> Self {
        // Through `webenv` so the browser build has the same lever as `?render_scale=0.75` — the
        // one knob that matters there: the world is GPU-bound at a 2560×1600 DPR-scaled canvas on
        // an iGPU (87 fps → 154 fps at ~40 % of the pixels, measured 2026-08-29).
        let scale = crate::webenv::var("WOW_RENDER_SCALE")
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|v| v.is_finite())
            .map_or(1.0, |v| {
                v.clamp(*RENDER_SCALE_RANGE.start(), *RENDER_SCALE_RANGE.end())
            });
        Self(scale)
    }
}

/// The backdrop's size and the world camera's target scale factor, for a window of `px` physical
/// pixels at `window_factor`, rendered at `scale`.
///
/// **Two invariants, and the second is the load-bearing one.**
///
/// 1. The image keeps the window's aspect, so the ratio — never an individual axis — is what gets
///    clamped to [`MAX_RENDER_AXIS`]. Clamping the axes independently would letterbox the world
///    inside a quad that is still the window's shape.
/// 2. `size.x / factor` — the camera's LOGICAL viewport width — comes out the window's logical
///    width, whatever `scale` is. That is why the factor is derived from the size actually built
///    rather than from `scale`: the `round()` moves the true ratio by up to half a pixel, and it is
///    the logical number every pick ray is denominated in (see [`retarget_world_camera`]).
///
/// At `scale == 1.0` both lines are exact identities in IEEE — `px × 1.0` is `px`, `size.x / px.x`
/// is `1.0`, `window_factor × 1.0` is `window_factor` — so the pre-1639 numbers come back
/// bit-for-bit and no golden can move. A test welds that.
fn render_target_for(px: UVec2, window_factor: f32, scale: f32) -> (UVec2, f32) {
    let px = px.max(UVec2::ONE);
    let ceiling = |axis: u32| MAX_RENDER_AXIS as f32 / axis as f32;
    let scale = scale
        .clamp(*RENDER_SCALE_RANGE.start(), *RENDER_SCALE_RANGE.end())
        .min(ceiling(px.x))
        .min(ceiling(px.y));
    let size = (px.as_vec2() * scale)
        .round()
        .as_uvec2()
        .clamp(UVec2::ONE, UVec2::splat(MAX_RENDER_AXIS));
    (size, window_factor * size.x as f32 / px.x as f32)
}

/// The texture LOD bias a render at `effective` scale owes its mipmapped world textures.
///
/// Rendering smaller doubles every screen-space derivative, so every sampler picks a **coarser**
/// mip — and that blur then gets stretched back up by the resolve, on top of the resolution loss.
/// Every engine that ships a render scale compensates with `log2(scale)`, and every one of them has
/// to do it in the shader, because WebGPU dropped the sampler's LOD bias: `wgpu::SamplerDescriptor`
/// (27.0.1) carries `lod_min_clamp`, `lod_max_clamp` and `anisotropy_clamp`, and nothing else.
///
/// **Clamped at 0 — downscaling is compensated, upscaling is left alone.** Above 1.0 the smaller
/// derivatives already pick a *sharper* mip, and that is not an artefact to correct: it is exactly
/// what makes supersampling work, since the resolve then averages detail that a native-resolution
/// frame could never have resolved. Feeding it `+log2(scale)` would hand back the blurrier mip and
/// throw the win away.
///
/// Bevy carries the plumbing already: [`MipBias`] on the camera is extracted into the view uniform
/// (`View::mip_bias`, defaulting 0.0 when absent), and `pbr_input_from_standard_material` applies it
/// to every sample — so the whole M2/WMO/creature/doodad lane gets this for free and only our own
/// hand-written samplers had to be taught (`terrain.wgsl`, `static_gx.wgsl`, `liquid.wgsl`, and the
/// coverage re-sample in `wow_model.wgsl`).
fn mip_bias(effective: f32) -> f32 {
    effective.log2().min(0.0)
}

/// The off-screen image the world camera renders into, and which the UI pass draws first.
///
/// Sized in physical pixels **× [`RenderScale`]**; at the default 1.0 that matches the swapchain 1:1
/// and the UI pass's sample is an identity resample, which is the property the composite lane was
/// built on and the reason the scale ships off.
#[derive(Resource)]
pub(crate) struct WorldBackdrop {
    pub(crate) image: Handle<Image>,
    /// The size the image was last built at, in physical px — the resize gate.
    size: UVec2,
    /// What the world camera's target was last stamped with: **which image**, and the scale factor
    /// that went with it. Both halves gate the re-stamp — the factor because a stale one is B169,
    /// the image because a rebuilt backdrop is a NEW asset (see [`track_render_size`]) and a camera
    /// left aiming at the retired one renders into nothing. `None` until the first stamp.
    stamped: Option<(AssetId<Image>, f32)>,
}

impl WorldBackdrop {
    /// The size the world is actually being rendered at, in physical px — the window's size times
    /// [`RenderScale`], after the rounding and the axis ceiling. The number an FPS probe has to
    /// print beside its frame time, since nothing else in the line can imply it.
    pub(crate) fn render_size(&self) -> UVec2 {
        self.size
    }
}

/// A fresh backdrop image at `size` physical px. See the module doc for the format's two halves.
fn new_backdrop_image(size: UVec2) -> Image {
    let mut image = Image::new_fill(
        Extent3d {
            width: size.x.max(1),
            height: size.y.max(1),
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0; 8],
        TextureFormat::Rgba16Float,
        RenderAssetUsages::default(),
    );
    image.texture_descriptor.usage =
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST | TextureUsages::RENDER_ATTACHMENT;
    image
}

fn window_physical_size(window: &Window) -> UVec2 {
    UVec2::new(
        window.physical_width().max(1),
        window.physical_height().max(1),
    )
}

fn setup_backdrop(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    scale: Res<RenderScale>,
) {
    let (size, _) = windows.single().map_or((UVec2::new(1280, 720), 1.0), |w| {
        render_target_for(
            window_physical_size(w),
            w.resolution.scale_factor(),
            scale.0,
        )
    });
    let image = images.add(new_backdrop_image(size));
    commands.insert_resource(WorldBackdrop {
        image,
        size,
        // Nothing stamped yet, so `retarget_world_camera`'s first run always fires — before any
        // camera exists to point at the image.
        stamped: None,
    });
}

/// Keep the backdrop at `window × `[`RenderScale`]. A stale-sized backdrop would still *work* (the
/// quad covers the window either way) — what it would break is the pairing: the size and the
/// factor [`retarget_world_camera`] stamps are two halves of one number, and a size that moved
/// without the factor following it is exactly the B169 defect with a different constant.
///
/// Runs before the stamp (the plugin's `.chain()`), so the factor is always computed against the
/// image that now exists.
///
/// ## The rebuild publishes a NEW asset — it does not write through the old handle (decision 1647)
///
/// It used to do exactly that (`*images.get_mut(&handle) = new_backdrop_image(size)`), and **the
/// world froze**: change any graphics setting or resize the window while in the world and the 3D
/// stopped dead at the frame of the change, stretched over the new window, while the interface
/// carried on. That is the director's report of 2026-08-27, and it reproduces on the bench (see the
/// decision record's measurement).
///
/// The mechanism is a **texture-identity lie**, and it is Bevy's prepared-bind-group model meeting
/// our material cache:
///
/// - Mutating an `Image` behind its handle makes a whole new GPU texture — `GpuImage`'s
///   `prepare_asset` (`bevy_render/texture/gpu_image.rs`) calls `create_texture` afresh on every
///   `AssetEvent::Modified`.
/// - A material's bind group, by contrast, is captured **once**: `PreparedMaterial2d::prepare_asset`
///   (`bevy_sprite_render/mesh2d/material.rs`) calls `as_bind_group` when the *material* asset is
///   added or modified, and nothing re-prepares it when a texture it named is re-created.
/// - [`crate::ui_pass`] keys its material cache on `AssetId<Image>`. Same id ⇒ same material ⇒ the
///   same bind group, still holding the `TextureView` of the texture we just threw away.
///
/// So the world camera drew into the new texture and the UI kept sampling the old one, forever.
///
/// A new handle fixes it at the identity rather than by invalidating a cache: **`AssetId<Image>` is
/// the tree's name for a GPU texture, so a new GPU texture gets a new name.** Every consumer then
/// reacts on its own — the cache misses and builds a fresh material, [`emit_backdrop_quad`]'s quad
/// differs and flags a rebuild, and [`retarget_world_camera`] re-points the camera.
///
/// It is also the only version that is safe against prepare *order*. `Material2dPlugin` registers
/// `RenderAssetPlugin::<PreparedMaterial2d<M>>::default()` — `AFTER = ()`, i.e. **not** ordered
/// after `prepare_assets::<GpuImage>` — so the "touch the material so its bind group rebuilds"
/// remedy (which `benilla_world::clouds` uses, and whose comment already named this whole hazard)
/// can re-bind the *previous* `GpuImage` if the material happens to prepare first. A fresh
/// `AssetId` cannot: it either finds its own `GpuImage` or `as_bind_group` returns
/// `RetryNextUpdate` and Bevy retries next frame.
///
/// The retired asset is **removed**, not merely dropped, because the stale cache entry is still
/// holding a strong handle to it; `ui_pass`'s rebuild forgets materials whose image was removed,
/// which is what keeps a window drag from retiring tens of megabytes a frame into a live cache.
fn track_render_size(
    mut backdrop: ResMut<WorldBackdrop>,
    mut images: ResMut<Assets<Image>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    scale: Res<RenderScale>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    let (size, _) = render_target_for(
        window_physical_size(window),
        window.resolution.scale_factor(),
        scale.0,
    );
    if size == backdrop.size {
        return;
    }
    let retired = std::mem::replace(&mut backdrop.image, images.add(new_backdrop_image(size)));
    images.remove(&retired);
    backdrop.size = size;
    // Said out loud on every change, because a measurement taken at the wrong scale looks
    // exactly like a measurement: the pixels the GPU is actually being asked for are the one
    // term no probe line could infer from the window.
    info!(
        "render scale {:.3}: world renders at {}x{} into a {}x{} window",
        scale.0,
        size.x,
        size.y,
        window.physical_width(),
        window.physical_height()
    );
}

/// Point the world camera at the backdrop instead of the swapchain — **carrying the window's own
/// scale factor**.
///
/// A system rather than a component on the spawn, because there are two spawn sites for the same
/// camera (the real one and `player::setup`'s no-client-data fallback) and neither should have to
/// know about the composite lane. `benilla-worldview` is unaffected — it links `benilla-world`, not
/// this crate, and has no UI camera to composite with, so its world camera keeps the swapchain.
///
/// **The scale factor is the load-bearing half.** A camera's logical↔physical conversions all go
/// through its target's `RenderTargetInfo`: a WINDOW target reports the window's physical size and
/// the window's scale factor, so `logical_viewport_size` is the window's LOGICAL size — which is
/// the space `Window::cursor_position` reports in, and the space every caller here works in
/// (`capture::pick_probe`'s header says it outright: *"`viewport_to_world` works in logical units
/// and a Retina capture is 2× the logical window"*). `From<Handle<Image>>` builds an
/// `ImageRenderTarget` with `scale_factor: 1.0`, and this backdrop is sized in **physical** px — so
/// the moment the world camera was retargeted at it, its logical viewport became the PHYSICAL size
/// and every conversion silently gained a factor of the display's scale.
///
/// On a 2× display that aims every world pick ray at a quarter of the screen: the ray for a cursor
/// at the centre goes to the upper-left quadrant. It hit the whole client at once — the GameObject
/// pick (the cog that "barely appears" and the right-click that "does nothing half the time" —
/// B169's fourth half), the unit pick and its occlusion ray, and the `world_to_viewport` side
/// (chat bubbles, `target::scan`). Stamping the window's real scale factor restores the
/// pre-composite semantics for all of them at once, which is why it lives here and not as a
/// conversion at each of the eight call sites.
///
/// Re-stamped when the factor changes, not only on `Added`: dragging the window between a Retina
/// and a non-Retina display changes it mid-session — and, since 1639, so does moving
/// [`RenderScale`], which multiplies into exactly the same number.
///
/// **Render scale rides here rather than anywhere else precisely because of the paragraph above.**
/// The camera's logical viewport is `image_size / scale_factor`; scaling both by the same ratio
/// leaves it — and therefore the projection (`camera_system` feeds `logical_viewport_size` into
/// `Projection::update`) and every pick ray (`viewport_to_ndc` and `world_to_viewport_core` read
/// `logical_viewport_rect()` and nothing else) — **arithmetically unchanged**. Picking is not
/// "still correct after render scale"; it cannot see render scale at all.
///
/// The camera also carries the [`MipBias`] that a scaled render owes its textures — see
/// [`mip_bias`]. Absent, Bevy's view uniform reads 0.0, so the component is inserted every stamp
/// rather than only when non-zero: a camera left holding last scale's bias is the same class of
/// stale-pairing bug as a stale factor.
fn retarget_world_camera(
    mut commands: Commands,
    mut backdrop: ResMut<WorldBackdrop>,
    windows: Query<&Window, With<PrimaryWindow>>,
    added: Query<(), Added<WorldCamera>>,
    mut cameras: Query<(Entity, &mut RenderTarget), With<WorldCamera>>,
) {
    let (px, window_factor) = windows.single().map_or((UVec2::ONE, 1.0), |w| {
        (window_physical_size(w), w.resolution.scale_factor())
    });
    // The image's real size, not the requested one: `track_render_size` ran first and may have
    // clamped, and the factor must describe the texture that exists.
    let scale_factor = window_factor * backdrop.size.x as f32 / px.x.max(1) as f32;
    // **The image as well as the factor.** A rebuild retires the old asset and publishes a new one
    // ([`track_render_size`]), and on a pure window resize the factor does NOT move — so a
    // factor-only gate would leave every world camera aiming at an asset that no longer exists.
    let want = (backdrop.image.id(), scale_factor);
    let moved = backdrop.stamped != Some(want);
    if !moved && added.is_empty() {
        return;
    }
    backdrop.stamped = Some(want);
    let bias = MipBias(mip_bias(scale_factor / window_factor));
    for (entity, mut current) in &mut cameras {
        *current = RenderTarget::Image(bevy::camera::ImageRenderTarget {
            handle: backdrop.image.clone(),
            scale_factor,
        });
        commands.entity(entity).insert(bias.clone());
    }
}

/// Publish the backdrop quad for this frame — full-window, opaque, ahead of every other quad.
///
/// Emitted only while the world camera is actually drawing. With no world (the glue screens, the
/// loading screen, a gated camera) the image holds a stale or never-written frame, and painting it
/// would be worse than the transparent clear the UI pass falls back to.
fn emit_backdrop_quad(
    backdrop: Res<WorldBackdrop>,
    mut quads: ResMut<UiQuads>,
    cameras: Query<&Camera, With<WorldCamera>>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    let drawing = cameras.iter().any(|c| c.is_active);
    let next = windows.single().ok().filter(|_| drawing).map(|window| {
        // Logical px, y-down from the top-left — `rebuild_ui_mesh`'s own rect space.
        UiQuad {
            rect: Rect::from_corners(Vec2::ZERO, Vec2::new(window.width(), window.height())),
            texture: Some(backdrop.image.clone()),
            ..UiQuad::default()
        }
    });
    // **Flag the rebuild only when the quad itself changes** — its arrival, its departure, a
    // resize. Its CONTENTS change every frame and must not: the mesh batch holds the image handle
    // and the material samples whatever the world camera just rendered into it, so a per-frame
    // `dirty` here would drag every batch in the pass through a full rebuild for a picture that
    // did not move — the 0365 live-city churn, re-introduced from the one producer that runs every
    // single frame.
    if quads.backdrop != next {
        quads.backdrop = next;
        quads.dirty = true;
    }
}

/// Owns the backdrop image, the world camera's target, and the quad. See the module doc.
pub(crate) struct WorldBackdropPlugin;

impl Plugin for WorldBackdropPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RenderScale>()
            .add_systems(Startup, setup_backdrop)
            .add_systems(
                Update,
                (track_render_size, retarget_world_camera, emit_backdrop_quad)
                    .chain()
                    .before(crate::ui_pass::UiQuadAppend),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::image::TextureFormatPixelInfo as _;

    /// The backdrop is float and un-encoded — the two halves the module doc argues for, and the
    /// pair a future "make it 8-bit, it's only a backdrop" edit would quietly break (an sRGB label
    /// double-encodes through `ui_quad`; an 8-bit un-encoded grid is B126's banding).
    #[test]
    fn the_backdrop_is_unencoded_float() {
        let image = new_backdrop_image(UVec2::new(320, 200));
        assert_eq!(image.texture_descriptor.format, TextureFormat::Rgba16Float);
        assert!(
            !image.texture_descriptor.format.is_srgb(),
            "an sRGB label would land a second encode in a chain that decodes once, at the end"
        );
        assert!(
            !TextureFormat::Rgba16Float.is_srgb(),
            "the swapchain's own view is sRGB — the backdrop deliberately is not"
        );
    }

    /// It must be usable as a camera target AND samplable by the UI pass. Dropping either usage
    /// bit fails at device level, far from here.
    #[test]
    fn the_backdrop_is_both_a_target_and_a_texture() {
        let image = new_backdrop_image(UVec2::new(320, 200));
        let usage = image.texture_descriptor.usage;
        assert!(usage.contains(TextureUsages::RENDER_ATTACHMENT));
        assert!(usage.contains(TextureUsages::TEXTURE_BINDING));
    }

    /// **The world camera's target carries the WINDOW's scale factor, not `1.0`** — the pick
    /// regression of 2026-08-26 (B169's fourth half).
    ///
    /// `From<Handle<Image>>` builds an `ImageRenderTarget` with `scale_factor: 1.0`, and this
    /// backdrop is sized in PHYSICAL px. A camera whose target says "physical size, scale 1"
    /// reports that physical size as its LOGICAL viewport — but every caller works in logical
    /// units, because that is what `Window::cursor_position` reports in. So on a 2× display the
    /// world's pick ray for a centred cursor went to the upper-left quadrant, and the cog "barely
    /// appeared" while right-click "did nothing half the time". It hit all eight
    /// `viewport_to_world`/`world_to_viewport` sites at once, not just the GameObject pick.
    ///
    /// The scale factor is re-stamped when it CHANGES, not only on `Added`: dragging the window
    /// from a Retina display to a non-Retina one changes it mid-session, and a stale factor is the
    /// same defect with a different constant.
    #[test]
    fn the_world_cameras_target_carries_the_windows_scale_factor() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Image>();
        let window = |sf: f32| {
            let mut w = Window::default();
            w.resolution.set_scale_factor(sf);
            w
        };
        app.world_mut().spawn((window(2.0), PrimaryWindow));
        app.init_resource::<RenderScale>()
            .add_systems(Startup, setup_backdrop)
            // Chained exactly as the plugin chains them: since 1639 the stamp reads the size the
            // resize pass settled on, so a test that ran the stamp alone would be testing a pairing
            // that never happens.
            .add_systems(Update, (track_render_size, retarget_world_camera).chain());
        let cam = app
            .world_mut()
            .spawn((WorldCamera, RenderTarget::default()))
            .id();
        app.update();

        let stamped = |app: &App, e: Entity| match app.world().entity(e).get::<RenderTarget>() {
            Some(RenderTarget::Image(t)) => t.scale_factor,
            _ => panic!("the world camera must target the backdrop image"),
        };
        assert_eq!(
            stamped(&app, cam),
            2.0,
            "a 2× display: the target must report the window's scale factor, or every logical \
             cursor position is read as a physical one and the pick ray misses by 2×"
        );
        // …and it follows the window across displays.
        let mut w = app.world_mut().query::<&mut Window>();
        w.single_mut(app.world_mut())
            .unwrap()
            .resolution
            .set_scale_factor(1.0);
        app.update();
        assert_eq!(
            stamped(&app, cam),
            1.0,
            "moving to a 1× display re-stamps: a stale factor is the same defect, inverted"
        );
    }

    /// Physical, not logical: the backdrop matches the swapchain 1:1 so the UI pass's sample is an
    /// identity resample. A zero-size window (minimised on some platforms) must still produce a
    /// legal texture rather than a device error.
    #[test]
    fn a_degenerate_size_still_builds_a_legal_texture() {
        let image = new_backdrop_image(UVec2::ZERO);
        assert_eq!(image.texture_descriptor.size.width, 1);
        assert_eq!(image.texture_descriptor.size.height, 1);
        assert_eq!(
            image.data.as_ref().map(Vec::len),
            TextureFormat::Rgba16Float.pixel_size().ok()
        );
    }

    /// **Scale 1.0 is bit-for-bit the pre-1639 lane** — the property every visual golden in the
    /// tree rests on. Not "within a pixel": the same `UVec2` and the same `f32`, because
    /// `px × 1.0`, `size.x / px.x` and `f × 1.0` are all exact in IEEE and a rounding introduced
    /// here would move the whole world by a sub-pixel resample.
    #[test]
    fn scale_one_reproduces_the_windows_own_numbers_exactly() {
        for px in [
            UVec2::new(1280, 720),
            UVec2::new(3200, 1800),
            UVec2::new(1601, 901), // odd on both axes — the case a `/ 2` would round
        ] {
            for sf in [1.0, 1.5, 2.0] {
                assert_eq!(render_target_for(px, sf, 1.0), (px, sf), "{px} at {sf}×");
            }
        }
    }

    /// **The camera's logical viewport does not move** — the whole design, and the invariant that
    /// makes render scale invisible to picking. `logical = image_size / scale_factor`
    /// (`bevy_camera`'s `to_logical`), and that is what `viewport_to_ndc` and
    /// `world_to_viewport_core` read; if it drifts, B169 comes back at the drift's magnitude.
    ///
    /// Sub-pixel is the tolerance because the image is integer: the ratio is derived back out of
    /// the size that was actually built, so x is exact and y carries at most one `round()`.
    #[test]
    fn every_scale_keeps_the_logical_viewport_the_windows_own() {
        for px in [UVec2::new(1280, 720), UVec2::new(3200, 1800)] {
            for sf in [1.0, 2.0] {
                let want = px.as_vec2() / sf;
                for scale in [0.25, 0.5, 0.6667, 0.75, 1.0, 1.25, 2.0, 4.0] {
                    let (size, factor) = render_target_for(px, sf, scale);
                    let logical = size.as_vec2() / factor;
                    assert!(
                        (logical.x - want.x).abs() < 0.001,
                        "{px} at {sf}× scaled {scale}: logical width {logical:?}, want {want:?}"
                    );
                    assert!(
                        (logical.y - want.y).abs() < 1.0,
                        "{px} at {sf}× scaled {scale}: logical height {logical:?}, want {want:?}"
                    );
                }
            }
        }
    }

    /// The axis ceiling clamps the **ratio**, so the picture keeps the window's aspect instead of
    /// being letterboxed inside a quad that is still the window's shape. It binds on an oversized
    /// window at scale 1.0 too — a >8192 px window was a texture wgpu refuses to create long before
    /// this dial existed, and nothing here used to look.
    #[test]
    fn the_axis_ceiling_shrinks_the_picture_it_does_not_reshape_it() {
        let px = UVec2::new(3840, 2160);
        let (size, _) = render_target_for(px, 1.0, 4.0);
        assert_eq!(size.x, MAX_RENDER_AXIS);
        let aspect = |v: UVec2| v.x as f32 / v.y as f32;
        assert!(
            (aspect(size) - aspect(px)).abs() < 0.001,
            "aspect kept: {size}"
        );
        // …and the clamp is not render scale's alone.
        let huge = UVec2::new(10240, 4320);
        let (size, _) = render_target_for(huge, 1.0, 1.0);
        assert!(
            size.x <= MAX_RENDER_AXIS && size.y <= MAX_RENDER_AXIS,
            "{size}"
        );
        assert!(
            (aspect(size) - aspect(huge)).abs() < 0.001,
            "aspect kept: {size}"
        );
    }

    /// **The bias compensates downscaling and leaves supersampling alone.** A negative bias where a
    /// scale is > 1 would hand back the coarser mip and throw away the only thing SSAA buys; a
    /// missing one where it is < 1 is the double blur every engine's render scale corrects.
    #[test]
    fn the_mip_bias_is_log2_below_one_and_nothing_above_it() {
        assert!((mip_bias(0.5) - -1.0).abs() < 1e-6);
        assert!((mip_bias(0.25) - -2.0).abs() < 1e-6);
        assert_eq!(
            mip_bias(1.0),
            0.0,
            "off must be exactly off — 0.0, not -0.0's cousin"
        );
        assert_eq!(mip_bias(2.0), 0.0);
        assert_eq!(mip_bias(4.0), 0.0);
    }

    /// **A rebuild publishes a NEW image, and the camera follows it onto the new one** — the
    /// 2026-08-27 world-freeze (decision 1647).
    ///
    /// Writing the new size through the OLD handle is what froze the world: `ui_pass` keys its
    /// material cache on `AssetId<Image>`, and Bevy prepares a material's bind group once, so the
    /// same id handed back the same bind group — still holding the `TextureView` of the texture
    /// that had just been thrown away. The world camera drew into the new texture; the UI kept
    /// sampling the dead one, forever.
    ///
    /// Three assertions, because the fix has three halves and any one of them alone is still broken:
    /// the id must MOVE (or the cache cannot tell), the old asset must be GONE (or the retired
    /// image is pinned by whatever still names it), and the camera must be re-stamped onto the new
    /// handle — that last one is the half a factor-only gate misses, since a pure window resize
    /// does not move the scale factor at all.
    #[test]
    fn a_rebuilt_backdrop_is_a_new_asset_and_the_camera_follows_it() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Image>();
        let mut w = Window::default();
        w.resolution.set_scale_factor(2.0);
        app.world_mut().spawn((w, PrimaryWindow));
        app.insert_resource(RenderScale(1.0))
            .add_systems(Startup, setup_backdrop)
            .add_systems(Update, (track_render_size, retarget_world_camera).chain());
        let cam = app
            .world_mut()
            .spawn((WorldCamera, RenderTarget::default()))
            .id();
        app.update();
        let first = app.world().resource::<WorldBackdrop>().image.id();

        // The director's own path: move the render scale while the world is up.
        app.insert_resource(RenderScale(0.5));
        app.update();

        let second = app.world().resource::<WorldBackdrop>().image.id();
        assert_ne!(
            first, second,
            "a rebuilt backdrop is a NEW GPU texture, so it must be a new AssetId — \
             the same id hands `ui_pass` back a bind group pointing at the retired texture"
        );
        assert!(
            !app.world().resource::<Assets<Image>>().contains(first),
            "the retired image must be removed, not merely dropped: a cached material still \
             holds a strong handle to it"
        );
        match app.world().entity(cam).get::<RenderTarget>() {
            Some(RenderTarget::Image(t)) => assert_eq!(
                t.handle.id(),
                second,
                "the camera must be re-stamped onto the new image"
            ),
            _ => panic!("the world camera must target the backdrop image"),
        }
    }

    /// A pure WINDOW resize moves the image without moving the scale factor — the case a
    /// factor-only re-stamp gate silently misses, leaving the camera aimed at a removed asset.
    #[test]
    fn a_window_resize_restamps_the_camera_even_though_the_factor_does_not_move() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Image>();
        let mut w = Window::default();
        w.resolution.set_scale_factor(2.0);
        let win = app.world_mut().spawn((w, PrimaryWindow)).id();
        app.insert_resource(RenderScale(1.0))
            .add_systems(Startup, setup_backdrop)
            .add_systems(Update, (track_render_size, retarget_world_camera).chain());
        let cam = app
            .world_mut()
            .spawn((WorldCamera, RenderTarget::default()))
            .id();
        app.update();
        let before = match app.world().entity(cam).get::<RenderTarget>() {
            Some(RenderTarget::Image(t)) => (t.handle.id(), t.scale_factor),
            _ => panic!("the world camera must target the backdrop image"),
        };

        app.world_mut()
            .entity_mut(win)
            .get_mut::<Window>()
            .expect("the primary window")
            .resolution
            .set(640.0, 400.0);
        app.update();

        let after = match app.world().entity(cam).get::<RenderTarget>() {
            Some(RenderTarget::Image(t)) => (t.handle.id(), t.scale_factor),
            _ => panic!("the world camera must target the backdrop image"),
        };
        assert_ne!(before.0, after.0, "the resize must re-point the camera");
        assert!(
            (before.1 - after.1).abs() < 1e-6,
            "at scale 1.0 the factor is the window's own and does not move on a resize — \
             which is exactly why the re-stamp cannot be gated on it alone: {} vs {}",
            before.1,
            after.1
        );
        assert!(
            app.world().resource::<Assets<Image>>().contains(after.0),
            "the camera must point at an image that still exists"
        );
    }

    /// End to end on a live `App`: a half-scale world on a 2× display renders into a half-size
    /// image, stamps a factor of 1.0 — `image / factor` is still the window's logical size — and
    /// carries `MipBias(-1)`.
    #[test]
    fn a_half_scale_world_halves_the_image_the_factor_and_the_mip() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::asset::AssetPlugin::default()))
            .init_asset::<Image>();
        let mut w = Window::default();
        w.resolution.set_scale_factor(2.0);
        let px = window_physical_size(&w);
        app.world_mut().spawn((w, PrimaryWindow));
        app.insert_resource(RenderScale(0.5))
            .add_systems(Startup, setup_backdrop)
            .add_systems(Update, (track_render_size, retarget_world_camera).chain());
        let cam = app
            .world_mut()
            .spawn((WorldCamera, RenderTarget::default()))
            .id();
        app.update();

        let backdrop = app.world().resource::<WorldBackdrop>();
        assert_eq!(backdrop.size, UVec2::new(px.x / 2, px.y / 2));
        match app.world().entity(cam).get::<RenderTarget>() {
            Some(RenderTarget::Image(t)) => assert!(
                (t.scale_factor - 1.0).abs() < 1e-6,
                "half the pixels at half the factor is the same logical viewport: {}",
                t.scale_factor
            ),
            _ => panic!("the world camera must target the backdrop image"),
        }
        let bias = app.world().entity(cam).get::<MipBias>().expect("a bias");
        assert!(
            (bias.0 - -1.0).abs() < 1e-6,
            "half scale owes one mip level: {}",
            bias.0
        );
    }
}
