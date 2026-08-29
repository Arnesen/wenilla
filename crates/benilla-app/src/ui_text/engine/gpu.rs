//! **The Bevy face of the glyph cache** — building it, following the window, and getting each new
//! cell onto the GPU.
//!
//! The upload is a sub-rect `RenderQueue::write_texture` into the sheet's existing texture rather
//! than an `Assets<Image>` mutation, for a reason that is about correctness rather than cost: see
//! [`crate::ui_text::pack`]'s module doc.

use std::sync::{Arc, Mutex};

use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssets;
use bevy::render::render_resource::{
    Extent3d, Origin3d, TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect,
};
use bevy::render::renderer::RenderQueue;
use bevy::render::texture::GpuImage;
use bevy::render::{ExtractSchedule, Render, RenderApp, RenderSystems};
use bevy::window::PrimaryWindow;

use benilla_assets::WorldAssets;

use super::{TextEngine, UiFontAtlas};
use crate::ui_text::pack::CellUpload;

/// Cells rasterized this frame, waiting for the render world.
#[derive(Resource, Default)]
struct GlyphUploadQueue(Vec<CellUpload>);

/// The render world's copy, which also carries anything whose page texture was not ready yet.
#[derive(Resource, Default)]
struct GlyphUploads(Vec<CellUpload>);

/// Loads the client's TTFs and drives the on-demand glyph cache.
pub(crate) struct UiTextPlugin;

impl Plugin for UiTextPlugin {
    fn build(&self, app: &mut App) {
        // `init` runs each Update until it succeeds: the engine needs both the patch chain (opened
        // at `AssetSet::Open`) *and* the primary window's real `scale_factor`, which winit only
        // reports once the OS window exists. `publish_pages` runs in `Last`, after every producer
        // of text quads, so a cell rasterized this frame is queued before the render extract.
        app.init_resource::<GlyphUploadQueue>()
            .add_systems(Update, init)
            .add_systems(Last, publish_sheet);
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_resource::<GlyphUploads>()
            .add_systems(ExtractSchedule, extract_glyph_uploads)
            .add_systems(
                Render,
                // After `prepare_assets::<GpuImage>` (PrepareAssets) so a page created this frame
                // already has its texture, and before the frame's submit.
                upload_glyph_cells.in_set(RenderSystems::PrepareResources),
            );
    }
}

/// Build the engine on the first frame both the patch chain and the window's real `scale_factor`
/// are available.
fn init(
    mut commands: Commands,
    world_assets: Option<Res<WorldAssets>>,
    images: Res<Assets<Image>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    existing: Option<Res<UiFontAtlas>>,
) {
    if existing.is_some() {
        return;
    }
    let (Some(world_assets), Ok(window)) = (world_assets, windows.single()) else {
        return;
    };
    let Some(engine) = TextEngine::load(&world_assets, &images, window.scale_factor()) else {
        return;
    };
    commands.insert_resource(UiFontAtlas {
        engine: Arc::new(Mutex::new(engine)),
        generation: 0,
        ellipsis: crate::ui_text::EllipsisMemo::default(),
    });
}

/// The frame boundary: follow the window's DPI, carry out a pending reset, create any new page's
/// texture, and hand this frame's cells to the render world.
///
/// **Order matters, and it is the order below.** The reset runs *before* the pending cells are
/// handed over, so the frame that exhausted the pages does not upload into shelves it is about to
/// free; and it runs here rather than at the point of failure so no UV can move while quads that
/// reference it are still being pushed (see [`TextEngine::note_exhausted`]).
fn publish_sheet(
    atlas: Option<ResMut<UiFontAtlas>>,
    mut images: ResMut<Assets<Image>>,
    mut queue: ResMut<GlyphUploadQueue>,
    windows: Query<&Window, With<PrimaryWindow>>,
) {
    let Some(mut atlas) = atlas else {
        return;
    };
    let dpi = windows.single().map_or(1.0, Window::scale_factor);
    let (generation, dpi_moved) = {
        let mut e = atlas.lock();
        // A DPI change invalidates no cell — it changes which cells get *asked for*.
        let dpi_moved = (e.dpi - dpi).abs() > 1e-6;
        e.dpi = dpi;
        if e.reset_pending {
            e.reset_pending = false;
            e.generation += 1;
            e.stats.resets += 1;
            e.chars.clear();
            e.cells.clear();
            e.sheet.reset();
        }
        let (announce, mut cells) = e.sheet.take_pending();
        if announce {
            // `insert`, once, on a reserved handle — and never `get_mut` afterwards, which would
            // recreate the texture and blank every glyph on it (see `pack`'s module doc).
            // A duplicate insert cannot happen (the sheet announces itself once), and there is
            // no recovery from a failed one anyway — text simply would not draw.
            let _ = images.insert(e.sheet.handle().id(), crate::ui_text::pack::sheet_image());
        }
        queue.0.append(&mut cells);
        (e.generation, dpi_moved)
    };
    // The one thing a DPI change DOES stale: the ellipsis memo's answers are keyed by a logical
    // box against a raster size that just moved.
    if dpi_moved {
        atlas.ellipsis = crate::ui_text::EllipsisMemo::default();
    }
    atlas.generation = generation;
    report_cache(&atlas);
}

/// Hand the frame's cells to the render world, appending rather than replacing — anything the
/// previous frame could not write (its page's texture was not ready) is still in there.
fn extract_glyph_uploads(
    mut main_world: ResMut<bevy::render::MainWorld>,
    mut uploads: ResMut<GlyphUploads>,
) {
    if let Some(mut q) = main_world.get_resource_mut::<GlyphUploadQueue>() {
        uploads.0.append(&mut q.0);
    }
}

/// Write each new cell into its page's existing texture — a sub-rect `write_texture`, so the
/// texture's identity never changes and nothing downstream needs invalidating (see `pack`'s module
/// doc for what the obvious alternative would have broken). A cell whose page texture has not been
/// prepared yet stays queued for the next frame rather than being dropped.
fn upload_glyph_cells(
    mut uploads: ResMut<GlyphUploads>,
    gpu_images: Res<RenderAssets<GpuImage>>,
    render_queue: Res<RenderQueue>,
) {
    uploads.0.retain(|u| {
        let Some(gpu) = gpu_images.get(u.image) else {
            return true; // the page's texture is not up yet — try again next frame
        };
        render_queue.write_texture(
            TexelCopyTextureInfo {
                texture: &gpu.texture,
                mip_level: 0,
                origin: Origin3d {
                    x: u.x,
                    y: u.y,
                    z: 0,
                },
                aspect: TextureAspect::All,
            },
            &u.rgba,
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(u.w * 4),
                rows_per_image: Some(u.h),
            },
            Extent3d {
                width: u.w,
                height: u.h,
                depth_or_array_layers: 1,
            },
        );
        false
    });
}

/// `WOW_GLYPH_CACHE=1`: one line a second of what the cache holds.
///
/// It exists because [`super::pack::MAX_PAGES`] and [`super::pack::PAGE_SIZE`] are the two numbers
/// this design can get wrong quietly — too small and a session resets on a loading screen, too
/// large and we hold VRAM nobody reads. Both are cheap to re-choose *from a measurement* and
/// expensive to re-choose from an argument, so the measurement ships with them.
fn report_cache(atlas: &UiFontAtlas) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    if std::env::var_os("WOW_GLYPH_CACHE").is_none_or(|v| v == "0") {
        return;
    }
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if LAST.swap(now, Ordering::Relaxed) == now {
        return;
    }
    let e = atlas.lock();
    let (used, total) = e.sheet.occupancy();
    eprintln!(
        "[glyph-cache] {used}/{total} texels ({:.1}%) · {} chars shaped · \
         {} cells rasterized · {} resets · dpi {}",
        100.0 * used as f64 / total as f64,
        e.stats.chars_shaped,
        e.stats.cells_rasterized,
        e.stats.resets,
        e.dpi,
    );
}
