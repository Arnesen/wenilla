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

use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::window::PrimaryWindow;

use crate::ui_pass::{UiQuad, UiQuads};
use benilla_world::view::WorldCamera;

/// The off-screen image the world camera renders into, and which the UI pass draws first.
///
/// Sized in **physical** pixels to match the swapchain 1:1 — the backdrop quad covers the window
/// exactly, so the sample is an identity resample and opaque world pixels survive untouched.
#[derive(Resource)]
pub(crate) struct WorldBackdrop {
    pub(crate) image: Handle<Image>,
    /// The size the image was last built at, in physical px — the resize gate.
    size: UVec2,
    /// The scale factor the camera's target was last stamped with — the re-stamp gate, and the
    /// number that keeps every logical↔physical conversion on the world camera honest. See
    /// [`retarget_world_camera`].
    scale_factor: f32,
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
) {
    let size = windows
        .single()
        .map(window_physical_size)
        .unwrap_or(UVec2::new(1280, 720));
    let image = images.add(new_backdrop_image(size));
    commands.insert_resource(WorldBackdrop {
        image,
        size,
        // Deliberately not the window's: `retarget_world_camera` stamps the real one and must see
        // a mismatch on its first run, before any camera exists to point at the image.
        scale_factor: f32::NAN,
    });
}

/// Keep the backdrop the window's size. A stale-sized backdrop would still *work* (the quad covers
/// the window either way) but the sample would stop being 1:1 and opaque world pixels would start
/// moving under a resample — the one property this whole lane is built to preserve.
fn track_window_size(
    mut backdrop: ResMut<WorldBackdrop>,
    mut images: ResMut<Assets<Image>>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    let size = window_physical_size(window);
    if size == backdrop.size {
        return;
    }
    let handle = backdrop.image.clone();
    if let Some(image) = images.get_mut(&handle) {
        *image = new_backdrop_image(size);
        backdrop.size = size;
    }
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
/// and a non-Retina display changes it mid-session.
fn retarget_world_camera(
    mut backdrop: ResMut<WorldBackdrop>,
    windows: Query<&Window, With<PrimaryWindow>>,
    added: Query<(), Added<WorldCamera>>,
    mut cameras: Query<&mut RenderTarget, With<WorldCamera>>,
) {
    let scale_factor = windows
        .single()
        .map_or(1.0, |w| w.resolution.scale_factor());
    // `!=` on the stored NAN seed is true, so the first run always stamps.
    #[allow(clippy::float_cmp)]
    let moved_display = scale_factor != backdrop.scale_factor;
    if !moved_display && added.is_empty() {
        return;
    }
    backdrop.scale_factor = scale_factor;
    for mut current in &mut cameras {
        *current = RenderTarget::Image(bevy::camera::ImageRenderTarget {
            handle: backdrop.image.clone(),
            scale_factor,
        });
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
        app.add_systems(Startup, setup_backdrop).add_systems(
            Update,
            (track_window_size, retarget_world_camera, emit_backdrop_quad)
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
        app.add_systems(Startup, setup_backdrop)
            .add_systems(Update, retarget_world_camera);
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
}
