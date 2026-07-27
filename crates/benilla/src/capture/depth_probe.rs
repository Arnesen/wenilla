//! `WOW_DEPTH` — **what depth actually won this pixel, and how far away is it?**
//!
//! The last unread link in B38. There, two surfaces of the Far Watch Post tower trade places: the
//! awning is nearer, writes depth, compares `GreaterEqual`, is not discarded, is not culled, keeps
//! the same mesh/material/texture every frame, and is submitted into `AlphaMask3d` at a stable draw
//! position on every single frame (decisions 0662, 0665, 0667) — and on some frames the plank behind
//! it wins the pixel anyway. Every one of those facts was established on the **CPU** side. None of
//! them says what value the GPU wrote into the depth buffer, which is the one thing that decides the
//! pixel and the one thing we could not read.
//!
//! So: `WOW_DEPTH="<x>,<y>[;<x>,<y>…]"` copies the view's depth texture back after the main pass and
//! logs, per frame, the raw reverse-Z value at each pixel and **where that puts the surface** — both
//! as a distance along the pixel's ray and as the perpendicular distance to the camera plane, because
//! those are different numbers and only the first is comparable with a ray cast.
//! `WOW_DEPTH_AT=<secs>` (default 20) / `WOW_DEPTH_COUNT=<n>` (default 1) shape the sampling like the
//! screenshot burst and the ray pick, so the three line up frame for frame.
//!
//! **Run it together with `WOW_PICK` at the same pixels.** This probe answers *what depth won*; the
//! ray pick answers *what is standing there* — every hit along the ray with its distance. Reading the
//! won distance against the hit distances is what turns a number into "**whose** depth won": if a
//! frame's depth matches hit 1's distance rather than hit 0's, the nearer surface never wrote depth
//! there, and no argument about draw order or culling survives that. Splitting the two keeps each
//! probe single-purpose, and because the geometry is static the pick's distances hold across the
//! whole burst.
//!
//! Coordinates are **screenshot pixels** — the same space `benilla-visual` reports boxes in and
//! `WOW_PICK` takes — and here they are used directly, because the depth texture is allocated in
//! physical pixels. (`WOW_PICK` has to divide by the window scale factor; its ray cast works in
//! logical units. Same input space, different reason.)
//!
//! **MSAA must be off** (`WOW_MSAA=off`). A multisampled depth texture cannot be copied to a buffer
//! at all, and there is no single "the" depth at a pixel to report if it could — there are four. The
//! probe refuses rather than reporting one of them, because a number that looks like the measurement
//! you wanted, taken slightly wrong, is how this bug has already produced four confident wrong
//! answers (0667).

use bevy::core_pipeline::core_3d::graph::{Core3d, Node3d};
use bevy::ecs::query::QueryItem;
use bevy::prelude::*;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::render_graph::{
    NodeRunError, RenderGraphContext, RenderGraphExt, RenderLabel, ViewNode, ViewNodeRunner,
};
use bevy::render::render_resource::{
    Buffer, BufferDescriptor, BufferUsages, Extent3d, MapMode, Origin3d, TexelCopyBufferInfo,
    TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect, TextureUsages,
};
use bevy::render::renderer::{RenderContext, RenderDevice};
use bevy::render::view::{ExtractedView, ViewDepthTexture};
use bevy::render::{Render, RenderApp, RenderSystems};

use crate::player::WorldCamera;

pub(crate) struct DepthProbePlugin;

impl Plugin for DepthProbePlugin {
    fn build(&self, app: &mut App) {
        let pixels = std::env::var("WOW_DEPTH")
            .ok()
            .map(|s| parse_pixels(&s))
            .unwrap_or_default();
        if pixels.is_empty() {
            warn!("depth: WOW_DEPTH wants \"<x>,<y>[;<x>,<y>…]\" screenshot pixels — inert");
            return;
        }
        let at = std::env::var("WOW_DEPTH_AT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(20.0);
        let count = std::env::var("WOW_DEPTH_COUNT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1u32)
            .max(1);
        app.insert_resource(DepthWatch {
            pixels,
            at,
            count,
            armed: false,
        })
        .add_systems(Update, arm)
        .add_plugins((
            ExtractResourcePlugin::<DepthWatch>::default(),
            ExtractComponentPlugin::<DepthProbeView>::default(),
        ));
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            warn!("depth: no render app — inert");
            return;
        };
        render_app
            .init_resource::<DepthFramesRead>()
            .add_systems(
                Render,
                (
                    prepare_staging.in_set(RenderSystems::PrepareResources),
                    // The node encodes the copy inside the graph; by the time the graph has run and
                    // submitted, the staging buffer holds it and can be mapped.
                    read_depth.after(RenderSystems::Render),
                ),
            )
            .add_render_graph_node::<ViewNodeRunner<DepthReadbackNode>>(Core3d, DepthReadbackLabel)
            // Between the opaque pass (which draws `Opaque3d` *and* `AlphaMask3d`) and the
            // transmissive/transparent ones. That is the depth as it stood when the opaque fight was
            // decided, which is the question — see [`DepthReadbackNode`] for what reading it later
            // gets you instead.
            //
            // `WOW_DEPTH_AFTER=1` moves the copy to AFTER the transparent pass — deliberately
            // measuring the thing the placement note warns about: the depth the transparent pass
            // itself wrote. B38 first observed that phantom ("5–8 yd at pixels where the ray cast
            // finds the tower at 32–43 yd … from something that tracks the camera") and never named
            // it; B16's eye quads lose a depth compare that the opaque-pass buffer says they win,
            // so the same writer is now the suspect. Before-vs-after at the same pixels names its
            // depth, and its depth names it.
            .add_render_graph_edges(
                Core3d,
                if std::env::var_os("WOW_DEPTH_AFTER").is_some() {
                    (
                        Node3d::MainTransparentPass,
                        DepthReadbackLabel,
                        Node3d::EndMainPass,
                    )
                } else {
                    (
                        Node3d::MainOpaquePass,
                        DepthReadbackLabel,
                        Node3d::MainTransmissivePass,
                    )
                },
            );
    }
}

/// The pixels to read and the sampling window. Extracted to the render world as a plain clone (a
/// handful of coordinates once per sampled frame).
#[derive(Resource, Clone)]
struct DepthWatch {
    pixels: Vec<(u32, u32)>,
    at: f32,
    count: u32,
    armed: bool,
}

impl ExtractResource for DepthWatch {
    type Source = DepthWatch;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

/// Marks the one view whose depth to read. The depth texture is cached per *render target*, so the
/// world camera and the UI camera on the same window share it — but picking a view by viewport size
/// would be picking by coincidence, and this bug is made of numbers taken slightly wrong.
#[derive(Component, Clone, Copy, ExtractComponent)]
struct DepthProbeView;

/// The staging buffer, kept across frames so a 24-frame burst allocates once.
#[derive(Resource)]
struct DepthStaging {
    buffer: Buffer,
    bytes_per_row: u32,
    height: u32,
}

/// Wait for the sampling window, then mark the world camera: opt its depth texture into `COPY_SRC`
/// (the default is `RENDER_ATTACHMENT` alone, which cannot be copied), and refuse the whole probe if
/// MSAA is on. Checking the live `Msaa` component rather than `$WOW_MSAA` means the refusal tracks
/// what the renderer is actually doing, not what the environment asked for.
fn arm(
    mut watch: ResMut<DepthWatch>,
    time: Res<Time>,
    mut cam: Query<(Entity, &mut Camera3d, &Msaa), With<WorldCamera>>,
    mut commands: Commands,
) {
    if watch.armed || time.elapsed_secs() < watch.at {
        return;
    }
    let Ok((entity, mut camera, msaa)) = cam.single_mut() else {
        return;
    };
    if *msaa != Msaa::Off {
        error!(
            "depth: MSAA is {msaa:?} — a multisampled depth texture cannot be copied, and there is \
             no single depth per pixel to report. Re-run with WOW_MSAA=off. Probe disabled."
        );
        watch.armed = true;
        watch.count = 0;
        return;
    }
    camera.depth_texture_usages =
        (TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC).into();
    commands.entity(entity).insert(DepthProbeView);
    info!(
        "depth: reading {} pixels for {} frames",
        watch.pixels.len(),
        watch.count
    );
    watch.armed = true;
}

/// Allocate the staging buffer to match the view's depth texture, before the graph runs.
fn prepare_staging(
    watch: Option<Res<DepthWatch>>,
    view: Query<&ViewDepthTexture, With<DepthProbeView>>,
    staging: Option<Res<DepthStaging>>,
    device: Res<RenderDevice>,
    mut commands: Commands,
) {
    let Some(watch) = watch else { return };
    if !watch.armed {
        return;
    }
    let Ok(depth) = view.single() else { return };
    let size = depth.texture.size();
    // `Depth32Float` is 4 bytes a texel, and a buffer copy's row stride must be 256-aligned — so the
    // stride is the aligned *byte* count, which is not the aligned pixel count times four.
    let bytes_per_row = RenderDevice::align_copy_bytes_per_row(size.width as usize * 4) as u32;
    if staging.is_some_and(|s| s.bytes_per_row == bytes_per_row && s.height == size.height) {
        return;
    }
    commands.insert_resource(DepthStaging {
        buffer: device.create_buffer(&BufferDescriptor {
            label: Some("wow_depth_readback"),
            size: u64::from(bytes_per_row) * u64::from(size.height),
            usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
            mapped_at_creation: false,
        }),
        bytes_per_row,
        height: size.height,
    });
}

#[derive(RenderLabel, Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DepthReadbackLabel;

/// Copies the depth texture aside **right after the opaque pass** — the moment the question is about.
///
/// Placement is the whole correctness of this probe, and getting it wrong is quiet. Read at the end of
/// the main pass instead and the numbers come back 5–8 yd at pixels where the ray cast finds the tower
/// at 32–43 yd, with a distance even at pixels that have **no** geometry along the ray: by then the
/// *transparent* pass has written depth of its own, from something that tracks the camera. Those were
/// stable, plausible, reproducible numbers about the wrong thing — which is worse than no probe,
/// because it looks like a measurement.
///
/// `MainOpaquePass` draws `Opaque3d` and then `AlphaMask3d`, which is exactly the pair the B38 fight is
/// between, so this is the depth that decided it.
#[derive(Default)]
struct DepthReadbackNode;

impl ViewNode for DepthReadbackNode {
    type ViewQuery = (&'static ViewDepthTexture, &'static DepthProbeView);

    fn run(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext,
        (depth, _): QueryItem<Self::ViewQuery>,
        world: &World,
    ) -> Result<(), NodeRunError> {
        let (Some(watch), Some(staging)) = (
            world.get_resource::<DepthWatch>(),
            world.get_resource::<DepthStaging>(),
        ) else {
            return Ok(());
        };
        // `read_depth` counts the frames; the copy is cheap but not free, so stop with it.
        if !watch.armed || world.resource::<DepthFramesRead>().0 >= watch.count {
            return Ok(());
        }
        let size = depth.texture.size();
        // `COPY_SRC` only lands on the texture allocated *after* `arm` patched the camera, so the
        // first armed frame still has the old one. Skip it rather than trip wgpu validation.
        if !depth.texture.usage().contains(TextureUsages::COPY_SRC) {
            return Ok(());
        }
        // Depth-stencil formats reject partial copies (wgpu-core `validate_texture_copy_range`), so
        // the whole texture goes across even though we want a handful of pixels. At one debug frame
        // each, the simple thing and the correct thing are the same.
        render_context.command_encoder().copy_texture_to_buffer(
            TexelCopyTextureInfo {
                texture: &depth.texture,
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: TextureAspect::DepthOnly,
            },
            TexelCopyBufferInfo {
                buffer: &staging.buffer,
                layout: TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(staging.bytes_per_row),
                    rows_per_image: Some(size.height),
                },
            },
            Extent3d {
                width: size.width,
                height: size.height,
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }
}

/// How many frames [`read_depth`] has reported — read by the graph node, which cannot hold a `Local`.
#[derive(Resource, Default)]
struct DepthFramesRead(u32);

/// Map back what the node copied and log the named pixels.
fn read_depth(
    watch: Option<Res<DepthWatch>>,
    view: Query<&ExtractedView, With<DepthProbeView>>,
    device: Res<RenderDevice>,
    staging: Option<Res<DepthStaging>>,
    depth: Query<&ViewDepthTexture, With<DepthProbeView>>,
    mut read: ResMut<DepthFramesRead>,
) {
    let (Some(watch), Some(staging)) = (watch, staging) else {
        return;
    };
    if !watch.armed || read.0 >= watch.count {
        return;
    }
    let (Ok(view), Ok(depth)) = (view.single(), depth.single()) else {
        return;
    };
    // Mirrors the node's own skip: no copy was encoded this frame, so there is nothing to map.
    if !depth.texture.usage().contains(TextureUsages::COPY_SRC) {
        return;
    }
    let size = depth.texture.size();
    let slice = staging.buffer.slice(..);
    slice.map_async(MapMode::Read, |_| {});
    // Block. A probe run is not gameplay, and a frame's numbers are worth nothing if they arrive
    // attached to a later frame's index.
    if let Err(e) = device.poll(bevy::render::render_resource::PollType::wait_indefinitely()) {
        error!("depth: poll failed: {e}");
        return;
    }
    let frame = read.0;
    read.0 += 1;
    // The projection the frame was actually drawn with, once per burst. Stated rather than assumed:
    // the reported distances are only as good as this matrix, and an instrument whose calibration is
    // invisible is one whose numbers cannot be audited later.
    if frame == 0 {
        info!(
            "depth: {}x{} view, clip_from_view P₂₂ {} P₃₂ {} P₀₀ {} P₁₁ {}",
            size.width,
            size.height,
            view.clip_from_view.z_axis.z,
            view.clip_from_view.w_axis.z,
            view.clip_from_view.x_axis.x,
            view.clip_from_view.y_axis.y,
        );
    }
    let view_from_clip = view.clip_from_view.inverse();
    {
        let data = slice.get_mapped_range();
        for &(x, y) in &watch.pixels {
            if x >= size.width || y >= size.height {
                warn!(
                    "depth#{frame} ({x}, {y}): outside the {}x{} view",
                    size.width, size.height
                );
                continue;
            }
            let at = (y * staging.bytes_per_row + x * 4) as usize;
            let d = f32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
            match view_point(&view_from_clip, ndc_of(x, y, size.width, size.height), d) {
                // Both distances, because they are not the same number and the difference is 15% at
                // the edge of the frame: `along` is measured along the ray (what `WOW_PICK` reports,
                // so this is the one to compare), `z` is the perpendicular distance to the camera
                // plane (what the depth buffer natively encodes).
                Some(p) => info!(
                    "depth#{frame} ({x}, {y}): {d:.9}  =  {:.4} yd along the ray  ({:.4} yd view z)",
                    p.length(),
                    -p.z
                ),
                // Reverse-Z clears to 0 = infinitely far: nothing drew here at all.
                None => info!("depth#{frame} ({x}, {y}): {d:.9}  =  nothing drew (cleared)"),
            }
        }
    }
    staging.buffer.unmap();
}

/// A physical pixel's centre in NDC. Framebuffer rows run down, NDC y runs up.
fn ndc_of(x: u32, y: u32, width: u32, height: u32) -> Vec2 {
    Vec2::new(
        (x as f32 + 0.5) / width as f32 * 2.0 - 1.0,
        1.0 - (y as f32 + 0.5) / height as f32 * 2.0,
    )
}

/// Unproject a pixel's depth back to where it is in **view space**, in yards.
///
/// It has to be the whole point, not just the depth: a depth value alone linearises to the distance
/// to the camera *plane*, while a ray cast measures along the *ray*, and off-axis those differ by
/// `1/cos θ` — 15% at the edge of a 45° frame, six yards at this bug's range. Comparing the two
/// without converting is the same axis mix-up that mis-measured B38's surface gap twice (0665), and
/// it is 100× the gap the readback exists to resolve. So unproject the actual pixel and hand back the
/// point; the caller reports both lengths.
///
/// Going through the inverse of the live matrix, rather than a reverse-Z formula, means the reading
/// cannot silently disagree with the projection the frame was drawn with.
fn view_point(view_from_clip: &Mat4, ndc: Vec2, d: f32) -> Option<Vec3> {
    let p = *view_from_clip * Vec4::new(ndc.x, ndc.y, d, 1.0);
    // Behind the camera or at infinity (a reverse-Z clear reads 0 ⇒ w 0) means nothing drew here.
    (p.w.abs() > f32::MIN_POSITIVE)
        .then(|| p.truncate() / p.w)
        .filter(|v| v.is_finite() && v.z < 0.0)
}

/// `"x,y;x,y"` → pixels. Malformed pairs are dropped with a warning rather than failing the run:
/// a typo in one coordinate of a list of eight should not cost a capture.
fn parse_pixels(spec: &str) -> Vec<(u32, u32)> {
    spec.split(';')
        .filter(|s| !s.trim().is_empty())
        .filter_map(|pair| {
            let (x, y) = pair.split_once(',')?;
            match (x.trim().parse().ok(), y.trim().parse().ok()) {
                (Some(x), Some(y)) => Some((x, y)),
                _ => {
                    warn!("depth: skipping malformed pixel {pair:?}");
                    None
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_pixel_list_and_skips_junk() {
        assert_eq!(
            parse_pixels("412,396; 565,445 ;;nope;1,"),
            vec![(412, 396), (565, 445)]
        );
    }

    /// The projection the world camera actually draws with: Bevy's reverse-Z, infinite far.
    fn proj() -> Mat4 {
        Mat4::perspective_infinite_reverse_rh(
            std::f32::consts::FRAC_PI_4,
            3200.0 / 1800.0,
            crate::player::CAM_NEAR,
        )
    }

    /// What the rasteriser writes for a point `dist` yards straight ahead down the view axis.
    fn depth_of(dist: f32) -> f32 {
        let clip = proj() * Vec4::new(0.0, 0.0, -dist, 1.0);
        clip.z / clip.w
    }

    #[test]
    fn a_depth_unprojects_to_the_distance_it_came_from() {
        let inv = proj().inverse();
        for dist in [2.0f32, 22.0, 46.0253, 46.0897, 3000.0] {
            let p =
                view_point(&inv, Vec2::ZERO, depth_of(dist)).expect("a drawn pixel has a point");
            assert!(
                (p.length() - dist).abs() < dist * 1e-3,
                "{dist} yd -> depth {} -> {} yd",
                depth_of(dist),
                p.length()
            );
        }
    }

    #[test]
    fn off_axis_the_ray_is_longer_than_the_perpendicular_distance() {
        // The bug this test exists for: reporting view-space z as if it were the ray-cast distance.
        // Straight ahead the two agree; at the frame edge they must not.
        let inv = proj().inverse();
        let d = depth_of(46.0);
        let centre = view_point(&inv, Vec2::ZERO, d).unwrap();
        assert!(
            (centre.length() - (-centre.z)).abs() < 1e-3,
            "on-axis they agree"
        );
        let edge = view_point(&inv, Vec2::new(-0.78, 0.42), d).unwrap();
        assert!(
            (-edge.z - 46.0).abs() < 0.05,
            "the perpendicular distance is what depth encodes: {} yd",
            -edge.z
        );
        assert!(
            edge.length() > 46.0 * 1.1,
            "off-axis the ray must be materially longer, got {} yd",
            edge.length()
        );
    }

    #[test]
    fn a_cleared_pixel_has_no_position() {
        // Reverse-Z clears to 0.0: infinitely far, i.e. nothing drew.
        assert_eq!(view_point(&proj().inverse(), Vec2::ZERO, 0.0), None);
    }

    #[test]
    fn the_awning_and_the_plank_are_thousands_of_ulps_apart() {
        // The whole instrument rests on this: at the B38 pin the two surfaces are 1.4 cm apart
        // perpendicular, and a readback can only name the winner if that gap survives `f32`.
        // If this ever fails, the probe cannot answer the question it exists for.
        let (awning, plank) = (46.0253f32, 46.0897f32);
        let (da, dp) = (depth_of(awning), depth_of(plank));
        let ulps = ((da.to_bits() as i64) - (dp.to_bits() as i64)).abs();
        assert!(
            ulps > 1000,
            "only {ulps} ULPs apart — a readback could not tell them apart"
        );
        // And they must not round to the same reported distance either.
        let inv = proj().inverse();
        let (ba, bp) = (
            view_point(&inv, Vec2::ZERO, da).unwrap().length(),
            view_point(&inv, Vec2::ZERO, dp).unwrap().length(),
        );
        assert!(
            (ba - bp).abs() > 0.01,
            "{ba} yd vs {bp} yd is not a distinguishable pair"
        );
    }
}
