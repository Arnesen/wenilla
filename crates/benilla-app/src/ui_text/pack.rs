//! The glyph cache's texture — one shelf-packed sheet, and the sub-rect uploads that carry each
//! new cell to the GPU.
//!
//! **Why it never repacks.** The cache fills on demand ([`super::engine::TextEngine`]), so cells
//! arrive one at a time forever; the allocator's job is to hand each one a permanent home.
//! Permanent is the load-bearing word: a cell's UV is copied into places this module cannot reach —
//! most sharply into the overhead-name meshes' `ATTRIBUTE_UV_0` ([`crate::nameplates`]), which
//! persist on the GPU across frames. So the sheet is **append-only**: it is packed into shelves, it
//! is never compacted, and a UV is normalized against the sheet's FIXED side length rather than
//! against how full it happens to be. (The old whole-charset bake normalized against a packed
//! height that moved with the size ladder, which is why a re-bake turned every cached nameplate
//! mesh into fragments of other letters — decision 1339's fault 2.)
//!
//! ## The upload path, and why it is not the obvious one
//!
//! The obvious way to add a cell in Bevy is to mutate the sheet's `Image` through
//! `Assets::get_mut`. That is wrong here, for a reason that has nothing to do with cost. In Bevy
//! 0.18.1 `Assets::get_mut` queues `AssetEvent::Modified`, `extract_render_asset` deep-clones the
//! whole `Image`, and `GpuImage::prepare_asset` calls `create_texture_with_data` — it **allocates a
//! new `wgpu::Texture`** and never consults the previous one. Meanwhile [`crate::ui_pass`]'s
//! material pool is keyed on `AssetId<Image>` and its `AsBindGroup` bind group is rebuilt only when
//! the *material* asset changes; nothing in Bevy invalidates a `Material2d` bind group when an
//! `Image` it references is modified. A full re-upload would therefore leave the UI sampling a dead
//! texture through a cached bind group, while the stale view pinned the old allocation alive. (That
//! is also why the old bake published a brand-new `Handle<Image>` every time: the handle churn was
//! a workaround for the missing invalidation — and the thing that made every atlas-derived cache in
//! the app go stale at once, on every window resize.)
//!
//! So cells go up as **sub-rect `RenderQueue::write_texture` writes into the existing texture**, the
//! way glyphon does it. Texture identity never moves, so there is nothing to invalidate: a handle
//! minted once is correct for the life of the process. The sheet is created data-less
//! ([`Image::new_uninit`] — a GPU texture with no CPU mirror, lazily zero-initialized by wgpu), and
//! `Assets::get_mut` must **never** be called on it: a stray `Modified` would hand us a fresh blank
//! texture and silently erase every glyph.
//!
//! ## One sheet, not a growing set of pages
//!
//! The real client keeps up to 8 pages per font (`CGxFont+0x18c`), and pages are the obvious answer
//! to "what happens when it fills". We keep one, because **the world pass binds one texture per
//! mesh**: a nameplate's glyph UVs are baked into a single `Mesh` with a single
//! `StandardMaterial`, so a name whose letters straddled two pages would need its mesh split and
//! its material set doubled — real complexity in [`crate::nameplates`] to serve an event that would
//! fire when a page boundary happened to fall inside one font's cell run. Growing the sheet is not
//! an alternative either: a grown texture is a *new* texture, which is the identity change this
//! whole path exists to avoid.
//!
//! So [`Sheet::reset`] is the eviction policy: when the shelves fill, drop everything at once and
//! refill from what is actually on screen. A shelf allocator cannot reclaim an interior cell, so
//! there is no piecemeal eviction to reach for, and the thing that fills the sheet is not glyph
//! variety but **raster-size churn** — every window resize retires a whole set of cells nothing will
//! ask for again. Dropping all of them is exactly right. It is the only event that can move a UV,
//! which is why it is a frame-boundary operation with a generation counter attached; see
//! [`super::engine::TextEngine::note_exhausted`].

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::math::Rect;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

/// The sheet's side length in **physical texels**. 2048² of RGBA8 is 16 MB of VRAM, allocated the
/// first time a glyph is drawn and never reallocated.
///
/// Sized by how long it postpones a [reset][Sheet::reset]. At ~110 texels for a typical body-text
/// cell (an 8×11 glyph plus its padding) a sheet holds tens of thousands of cells — order a hundred
/// distinct `(face, raster size)` sets, where one window size in one session uses a handful. The
/// occupancy instrument (`WOW_GLYPH_CACHE=1`) exists so this number gets re-chosen from a
/// measurement rather than from an argument.
pub(super) const SHEET_SIZE: u32 = 2048;

/// Padding between neighbouring cells, in texels — one row/column left untouched, so a bilinear tap
/// at a cell edge cannot reach into its neighbour.
const PAD: u32 = 1;

/// A cell's texel payload, in the two shapes the rasterizer produces.
pub(super) enum Cell<'a> {
    /// One byte per texel: coverage, expanded to white × alpha (the vertex color supplies the ink).
    Coverage(&'a [u8]),
    /// Four bytes per texel, straight alpha: a pre-composited outlined cell (black ring + white
    /// fill), taken verbatim.
    Rgba(&'a [u8]),
}

/// One cell's texels, waiting for the render world to write them into the sheet.
pub(super) struct CellUpload {
    pub(super) image: AssetId<Image>,
    pub(super) x: u32,
    pub(super) y: u32,
    pub(super) w: u32,
    pub(super) h: u32,
    pub(super) rgba: Vec<u8>,
}

/// The glyph sheet: one texture, a shelf cursor, and the pending uploads.
pub(super) struct Sheet {
    /// Reserved up front, before any pixels exist, because the engine that allocates cells lives
    /// behind a lock the script VM reaches into mid-tick and can never hold `Assets<Image>`.
    handle: Handle<Image>,
    /// Whether the `Image` asset exists yet. Announced once, on the first cell.
    created: bool,
    announce: bool,
    /// Left edge of the next cell on the current shelf.
    cursor_x: u32,
    /// Top edge of the current shelf.
    cursor_y: u32,
    /// Tallest cell placed on the current shelf.
    row_h: u32,
    uploads: Vec<CellUpload>,
}

impl Sheet {
    /// Reserve the handle. Costs nothing but an id: no texels are allocated and no `Image` asset
    /// exists until a cell actually lands.
    pub(super) fn new(images: &Assets<Image>) -> Self {
        Self {
            handle: images.reserve_handle(),
            created: false,
            announce: false,
            cursor_x: 0,
            cursor_y: 0,
            row_h: 0,
            uploads: Vec::new(),
        }
    }

    /// The texture every glyph quad samples.
    pub(super) fn handle(&self) -> Handle<Image> {
        self.handle.clone()
    }

    /// Place one cell and queue its texels. `None` means the sheet is full (or the cell could never
    /// fit) — the caller's cue to ask for a reset, and to draw nothing for this glyph meanwhile,
    /// which is a missing letter for at most one frame.
    pub(super) fn alloc(&mut self, w: u32, h: u32, cell: &Cell<'_>) -> Option<Rect> {
        if w == 0 || h == 0 || w > SHEET_SIZE || h > SHEET_SIZE {
            return None; // a zero-ink glyph (a space), or a cell bigger than the whole sheet
        }
        // Walked on a copy of the cursor and committed only on success, so a cell too tall for
        // the remaining height does not close the open shelf on its way out — a narrower or
        // shorter one behind it can still land there.
        let (mut x, mut y, mut row_h) = (self.cursor_x, self.cursor_y, self.row_h);
        if x + w > SHEET_SIZE {
            x = 0; // next shelf
            y += row_h + PAD;
            row_h = 0;
        }
        if y + h > SHEET_SIZE {
            return None;
        }
        self.cursor_x = x + w + PAD;
        self.cursor_y = y;
        self.row_h = row_h.max(h);

        let rgba = match cell {
            // Plain coverage: white × alpha, so the vertex color is the ink color.
            Cell::Coverage(cov) => {
                let mut out = vec![255u8; (w * h * 4) as usize];
                for (i, &a) in cov.iter().enumerate().take((w * h) as usize) {
                    out[i * 4 + 3] = a;
                }
                out
            }
            // Pre-composited outlined cell: verbatim (black ring, white fill).
            Cell::Rgba(rgba) => rgba[..(w * h * 4) as usize].to_vec(),
        };
        if !self.created {
            self.created = true;
            self.announce = true;
        }
        self.uploads.push(CellUpload {
            image: self.handle.id(),
            x,
            y,
            w,
            h,
            rgba,
        });
        // Normalized against the FIXED side, never against how full the sheet is — the whole reason
        // a cached UV stays valid for the life of the texture (module doc).
        let s = SHEET_SIZE as f32;
        Some(Rect::new(
            x as f32 / s,
            y as f32 / s,
            (x + w) as f32 / s,
            (y + h) as f32 / s,
        ))
    }

    /// Take whether the `Image` asset still has to be created, and the cells waiting to be written.
    pub(super) fn take_pending(&mut self) -> (bool, Vec<CellUpload>) {
        (
            std::mem::take(&mut self.announce),
            std::mem::take(&mut self.uploads),
        )
    }

    /// Free every shelf. The texture, its handle and its texels all survive — only the allocator's
    /// memory of what is where resets, so a quad emitted before the reset keeps drawing correctly
    /// until its own re-emit replaces it. The caller owns telling the world; see the generation
    /// counter on [`super::engine::TextEngine`].
    pub(super) fn reset(&mut self) {
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.row_h = 0;
    }

    /// `(texels committed, texels available)` — the occupancy instrument's numbers. Committed
    /// counts closed shelves whole plus the open one, i.e. it includes inter-cell padding and shelf
    /// slack, which is the honest answer to "how full is it".
    pub(super) fn occupancy(&self) -> (u64, u64) {
        let used = u64::from(self.cursor_y) * u64::from(SHEET_SIZE)
            + u64::from(self.cursor_x) * u64::from(self.row_h);
        (used, u64::from(SHEET_SIZE) * u64::from(SHEET_SIZE))
    }
}

/// The sheet's `Image`: a GPU texture with **no CPU mirror** (`data: None`), lazily zero-initialized
/// by wgpu and thereafter written only by sub-rect `write_texture`. Straight (non-premultiplied)
/// alpha in sRGB, matching what the quad pass has always sampled.
///
/// `MAIN_WORLD | RENDER_WORLD` rather than `RENDER_WORLD` alone: a `RENDER_WORLD`-only image has its
/// data taken on first extract and cannot be re-extracted. There is no data to take here, so the
/// usage costs nothing and leaves a device-lost recreate possible.
pub(super) fn sheet_image() -> Image {
    Image::new_uninit(
        Extent3d {
            width: SHEET_SIZE,
            height: SHEET_SIZE,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet() -> Sheet {
        // The handle is the only thing `Assets` is consulted for, and a bare `Assets` needs no app.
        Sheet::new(&Assets::<Image>::default())
    }

    fn cov(w: u32, h: u32) -> Vec<u8> {
        vec![255u8; (w * h) as usize]
    }

    /// A cell's UV normalizes against the fixed side, so it is the same rect no matter what lands
    /// afterwards. This is the property the nameplate meshes bake into the GPU, and the one whose
    /// absence made a re-bake draw letters as fragments of other letters (1339).
    #[test]
    fn a_uv_is_fixed_against_the_sheet_and_never_moves() {
        let mut s = sheet();
        let c = cov(8, 11);
        let first = s.alloc(8, 11, &Cell::Coverage(&c)).expect("room");
        let side = SHEET_SIZE as f32;
        assert_eq!(first, Rect::new(0.0, 0.0, 8.0 / side, 11.0 / side));
        for _ in 0..2000 {
            s.alloc(8, 11, &Cell::Coverage(&c)).expect("room");
        }
        // Nothing repacked: the first cell's texels are still where its UV says they are.
        let (_, uploads) = s.take_pending();
        assert_eq!((uploads[0].x, uploads[0].y), (0, 0));
        assert_eq!((uploads[0].w, uploads[0].h), (8, 11));
    }

    /// Coverage becomes white × alpha; a composited cell goes up verbatim. The upload IS the
    /// texture write, so this is where that conversion has to be right.
    #[test]
    fn coverage_uploads_as_white_times_alpha() {
        let mut s = sheet();
        s.alloc(2, 1, &Cell::Coverage(&[0, 128])).expect("room");
        let (_, uploads) = s.take_pending();
        assert_eq!(uploads[0].rgba, vec![255, 255, 255, 0, 255, 255, 255, 128]);

        let mut s = sheet();
        let ring = vec![0u8, 0, 0, 153, 200, 200, 200, 255];
        s.alloc(2, 1, &Cell::Rgba(&ring)).expect("room");
        let (_, uploads) = s.take_pending();
        assert_eq!(uploads[0].rgba, ring, "a composited cell is not re-tinted");
    }

    /// Shelves wrap downward when a cell will not fit beside its neighbour, and the sheet refuses
    /// rather than overrunning its own bounds.
    #[test]
    fn shelves_wrap_downward_and_the_sheet_eventually_refuses() {
        let mut s = sheet();
        // Just under half the sheet wide, so exactly two fit on a shelf and the third must wrap.
        let (w, h) = (SHEET_SIZE / 2 - PAD, 64);
        let c = cov(w, h);
        let first = s.alloc(w, h, &Cell::Coverage(&c)).expect("first");
        let second = s.alloc(w, h, &Cell::Coverage(&c)).expect("second");
        let third = s.alloc(w, h, &Cell::Coverage(&c)).expect("third");
        assert_eq!(first.min.y, second.min.y, "the first two share a shelf");
        assert!(second.min.x > first.min.x, "…side by side");
        assert!(
            third.min.y > first.min.y,
            "the third wrapped to the next shelf"
        );

        // …and it fills rather than growing. The bound is generous; what matters is that the loop
        // terminates at all, which is the property a growable allocator would not have.
        let mut placed = 3usize;
        while s.alloc(w, h, &Cell::Coverage(&c)).is_some() {
            placed += 1;
            assert!(placed < 10_000, "the sheet must fill, not grow");
        }
        let (used, total) = s.occupancy();
        assert!(
            used * 10 > total * 9,
            "the shelves should be ~full at refusal, not fragmented away: {used}/{total}"
        );
    }

    /// A cell taller or wider than the sheet can never be placed, and says so instead of silently
    /// corrupting a neighbour — without consuming shelf space finding out.
    #[test]
    fn an_oversized_or_empty_cell_is_refused_for_free() {
        let mut s = sheet();
        let c = cov(4, 4);
        assert!(s.alloc(SHEET_SIZE + 1, 4, &Cell::Coverage(&c)).is_none());
        assert!(s.alloc(4, SHEET_SIZE + 1, &Cell::Coverage(&c)).is_none());
        assert!(s.alloc(0, 0, &Cell::Coverage(&[])).is_none());
        assert_eq!(s.occupancy().0, 0);
        assert!(s.take_pending().1.is_empty());
    }

    /// The `Image` is announced for creation exactly once — the asset is created on the first cell
    /// and written into ever after. Announcing it twice would recreate the texture and erase
    /// everything on it.
    #[test]
    fn the_texture_is_announced_for_creation_exactly_once() {
        let mut s = sheet();
        let c = cov(8, 11);
        s.alloc(8, 11, &Cell::Coverage(&c)).expect("room");
        let (announce, uploads) = s.take_pending();
        assert!(announce, "created on the first cell");
        assert_eq!(uploads.len(), 1);
        s.alloc(8, 11, &Cell::Coverage(&c)).expect("room");
        let (announce, uploads) = s.take_pending();
        assert!(!announce, "…and never again");
        assert_eq!(uploads.len(), 1, "but the cell still goes up");
    }

    /// A reset frees the shelves and keeps everything the GPU knows about — the handle, and the
    /// texels themselves. A quad emitted before the reset keeps drawing correctly.
    #[test]
    fn a_reset_frees_the_shelves_and_keeps_the_texture() {
        let mut s = sheet();
        let c = cov(8, 11);
        s.alloc(8, 11, &Cell::Coverage(&c)).expect("room");
        let handle = s.handle();
        assert!(s.take_pending().0);
        assert!(s.occupancy().0 > 0);

        s.reset();
        assert_eq!(s.occupancy().0, 0, "the shelves are free");
        assert_eq!(s.handle(), handle, "the texture survives");
        // Critically: no re-announcement. Re-creating the image would blank the sheet.
        s.alloc(8, 11, &Cell::Coverage(&c)).expect("room");
        let (announce, uploads) = s.take_pending();
        assert!(!announce, "a reset must not recreate the texture");
        assert_eq!((uploads[0].x, uploads[0].y), (0, 0), "the shelf is reused");
    }
}
