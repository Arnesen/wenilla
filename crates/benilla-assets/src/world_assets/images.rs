//! Texture/image helpers: turn decoded BLP mip chains + raw RGBA into Bevy `Image`s (correct format,
//! sampler, mip layout) plus the small solid-colour / cursor / liquid-frame textures. Pure helpers the
//! asset foundation (`super`) and other subsystems call. Split out of `assets/mod.rs`.

use bevy::asset::RenderAssetUsages;
use bevy::image::{Image, ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};

use benilla_formats::BlpMipChain;

/// Build a full mip chain for an RGBA texture (mip 0 = `mip0`), halving each level to 1×1 with a
/// triangle (box-ish) downsample. Returns the concatenated mip data + the level count. For a single
/// layer this is the `LayerMajor`/`MipMajor`-agnostic order (mip0, mip1, …).
/// Color (albedo) texture format: **non-sRGB `Rgba8Unorm`**, so the GPU does NOT linearize albedo on
/// sample. Vanilla's fixed-function pipeline multiplied gamma (sRGB) texture bytes by gamma light
/// bytes in **gamma/byte space**; loading albedo as `…Unorm` keeps our shader math in that same space.
/// This is the RE'd faithful **invariant** (no sRGB↔linear round-trip anywhere). Used for model +
/// terrain albedo.
pub fn color_texture_format() -> TextureFormat {
    TextureFormat::Rgba8Unorm
}

/// A repeating, **mipmapped + anisotropically-filtered** texture built from the BLP's **authored**
/// mip pyramid — the levels the real 1.12 client uploads verbatim (`WoW.exe` has no GLU /
/// `glGenerateMipmap`; it cannot regenerate mips, only upload the stored ones). Using the authored
/// levels instead of CPU-resampling mip 0 with a box/Triangle filter in gamma byte space is the
/// model-side of the C2 fix: gamma-byte averaging darkens mid-tones and, on alpha-cutout foliage,
/// skews the leaf colour cool (it averages the dark *transparent* texels' RGB into the leaves, 5–18
/// LSB darker in R/G) and drifts coverage with distance. The authored mips already encode WoW's
/// coverage-correct, leaf-coloured downsample, so we lay them in as-is under the alpha test — no
/// re-filter, no alpha-to-coverage (1.12 does neither). Mips + anisotropy give the no-shimmer look
/// at distance without softening the up-close art.
pub fn repeat_texture_authored(chain: BlpMipChain, wrap: (bool, bool)) -> Image {
    let levels = chain.mips.len() as u32;
    let mip0 = chain.mips[0].clone(); // satisfies Image::new's length assert; full chain set below
    let mut data = Vec::with_capacity(chain.mips.iter().map(Vec::len).sum());
    for mip in &chain.mips {
        data.extend_from_slice(mip);
    }
    let mut image = Image::new(
        Extent3d {
            width: chain.width,
            height: chain.height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        mip0,
        color_texture_format(),
        // `RENDER_WORLD` — see [`image_gpu_bytes`] and the `blp.rs` `WorldArt` variant this is the
        // synchronous twin of: the render world takes the chain rather than cloning it and keeping
        // a main-world copy. Its callers are `WorldAssets::texture` and the character-skin
        // composite, and neither reads the result back; a composite is built CPU-side and handed
        // over whole.
        RenderAssetUsages::RENDER_WORLD,
    );
    image.data = Some(data);
    image.texture_descriptor.mip_level_count = levels;
    // The address mode is the M2 texture record's own (`flags & 0x1/0x2`), not a blanket Repeat.
    // On a cutout sheet it decides the SILHOUETTE: content authors a card's UVs past `0..1` so the
    // margin clamps to the transparent border and the card ends there — repeat instead folds that
    // margin into the opposite edge, drawing the sheet's opaque middle as solid geometry with a hard
    // seam at the crossing. 1250 batches across 775 models author clamp and sample outside the unit
    // square (`benilla-extract uvwrapscan`); 802 are cutout/blend, where it changes shape and not
    // merely colour. Decision 0763 (bugs B52/B96 — Dun Morogh's snow-firs, the Plaguelands bush).
    let mode = |repeat: bool| {
        if repeat {
            ImageAddressMode::Repeat
        } else {
            ImageAddressMode::ClampToEdge
        }
    };
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: mode(wrap.0),
        address_mode_v: mode(wrap.1),
        // Trilinear + anisotropic: smooth, sharp-where-it-should-be, no shimmer — the vanilla look.
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..default()
    });
    image
}

/// Stack the animated liquid frames into one **mipmapped + anisotropic** `texture_2d_array` from each
/// frame's BLP **authored** mip pyramid — same fidelity rule as [`repeat_texture_authored`]: the real
/// 1.12 client uploads the BLP's stored mips verbatim (no client-side regeneration). Without mips, the
/// 256² ripple tiled over ~16 yd aliases into dense sparkle "noise" at distance; the authored mips +
/// anisotropy give the reference's smooth, streaky wave highlights instead. Data is laid out
/// **LayerMajor** (`frame0[mip0..mipN], frame1[mip0..], …`, the wgpu/Bevy default) with a full
/// `size→1` chain; a frame whose authored chain is shorter/odd is filled by `NEAREST` from its nearest
/// authored level (gamma-safe, no averaging — mirrors [`chain_to_layer`]). All frames must share mip-0
/// `size` (the caller enforces this). Returns a `D2Array`-viewed image with `mip_level_count` set.
pub fn liquid_frame_array(frames: Vec<BlpMipChain>) -> Image {
    let size = frames[0].width;
    // Full mip chain down to 1×1 (square 1.12 frames): levels = log2(size) + 1.
    let mip_level_count = {
        let (mut n, mut s) = (1u32, size);
        while s > 1 {
            s >>= 1;
            n += 1;
        }
        n
    };
    let mut data = Vec::with_capacity(frames.len() * mip_chain_byte_size(size, mip_level_count));
    for blp in &frames {
        for level in 0..mip_level_count {
            let lw = (size >> level).max(1);
            // Match the array level to the authored mip of the same size (normally identity — water
            // frames carry a full chain); fall back to the nearest authored level via NEAREST.
            let src_level = if blp.width >= lw {
                let ratio = (blp.width / lw).max(1);
                (ratio.trailing_zeros() as usize).min(blp.mips.len() - 1)
            } else {
                0
            };
            let (sw, sh) = blp.mip_size(src_level as u32);
            extend_nearest(&mut data, &blp.mips[src_level], sw, sh, lw, lw);
        }
    }
    let mut image = Image::new_uninit(
        Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: frames.len() as u32,
        },
        TextureDimension::D2,
        color_texture_format(),
        // `RENDER_WORLD`: 136 frames of a 256² nine-level RGBA8 pyramid is 47,535,264 B (the figure
        // `gpu_bytes_count_every_array_layer` pins), built at `Startup` and resident for the whole
        // process. The default usage held that twice — once on the GPU and once in system RAM,
        // which nothing ever read.
        RenderAssetUsages::RENDER_WORLD,
    );
    image.data = Some(data);
    image.texture_descriptor.mip_level_count = mip_level_count;
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::D2Array),
        ..default()
    });
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        // Trilinear + 16× anisotropic — VERIFIED the reference ripple sampler (apitrace WoW.20: tex 442,
        // LINEAR_MIPMAP_LINEAR + MAX_ANISOTROPY 16). This is what turns the unmipped sparkle into the
        // reference's sparse, smooth, elongated wave-streaks at the grazing water surface.
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 16,
        ..default()
    });
    image
}

/// A single-mip RGBA8 image for winit's custom OS cursor (`CursorIcon::Custom`). Vanilla cursors are
/// tiny (`Point.blp` is 32×32), so unlike [`repeat_texture`] this needs neither a mip chain nor a
/// repeat sampler. macOS doesn't use this path (it builds an `NSCursor` directly — see `crate::cursor`).
#[cfg(not(target_os = "macos"))]
pub fn cursor_texture(width: u32, height: u32, rgba: Vec<u8>) -> Image {
    Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        rgba,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    )
}

/// A single-mip, **sRGB**, clamp-sampled image for an emissive celestial billboard (the sun/moon
/// discs — see [`WorldAssets::sprite_texture`]). sRGB so an unlit pass round-trips the authored gamma
/// bytes to screen; clamp-to-edge + linear (no repeat, no mips) since the whole texture maps to one
/// quad and the disc never tiles or minifies far.
pub fn sprite_image(width: u32, height: u32, rgba: Vec<u8>) -> Image {
    let mut image = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        rgba,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        ..default()
    });
    image
}

/// A coverage **mask** image (the minimap's `MinimapMask.blp`, decision 0203): identical sampling
/// to [`sprite_image`] (clamp, linear filtering, no mips) but uploaded **`Rgba8Unorm`** (not
/// sRGB) — the shader reads coverage, not color, so no gamma decode belongs anywhere in the path.
/// The coverage itself rides the ALPHA channel (`MinimapMask.blp` is DXT3: white color plane, the
/// circle ramp authored in its 8-bit alpha; alpha is gamma-free in either format, but the linear
/// upload keeps the color plane byte-exact too, and documents the intent).
pub fn mask_image(width: u32, height: u32, rgba: Vec<u8>) -> Image {
    let mut image = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        rgba,
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        ..default()
    });
    image
}

/// A portrait sprite: [`sprite_image`] with the RGBA pre-masked to its **inscribed circle** — the
/// alpha of every texel outside the largest centred circle is zeroed (with a 1px anti-aliased edge),
/// so a square icon or model reads as the round portrait the unit-frame ring expects. The ring in
/// `UI-TargetingFrame` is a thin band with transparent corners; without the mask the square texture's
/// corners poke past it (the placeholder `INV_Misc_QuestionMark` even bakes a grey border, so its
/// corners read as metal brackets). Same sRGB/clamp/linear/no-mip setup as [`sprite_image`].
pub fn portrait_image(width: u32, height: u32, mut rgba: Vec<u8>) -> Image {
    if width > 0 && height > 0 && rgba.len() == (width * height * 4) as usize {
        let (cx, cy) = (width as f32 / 2.0, height as f32 / 2.0);
        let radius = cx.min(cy);
        for y in 0..height {
            for x in 0..width {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                // Coverage: 1 well inside, 0 well outside, a 1px linear ramp across the edge.
                let coverage = (radius - dist).clamp(0.0, 1.0);
                let a = ((y * width + x) * 4 + 3) as usize;
                rgba[a] = (f32::from(rgba[a]) * coverage).round() as u8;
            }
        }
    }
    sprite_image(width, height, rgba)
}

/// A single-mip, **sRGB**, **repeat**-sampled image for a frame `Backdrop`'s tiled pieces (the
/// tooltip/panel border edges + a tiling background — `backdrop-mechanism.md` §2/§3). Identical to
/// [`sprite_image`] except the address mode is `Repeat`: a backdrop edge strip runs UVs `[0..N]`
/// (N = the edge count = frame-side / edgeSize − 2) and a tiled bg runs `[0..w/period]`, so the
/// slice must *wrap*, not clamp-stretch. The real client flags exactly this on the backdrop's
/// `SetTexture` (arg2 pushed twice into the load descriptor — `backdrop-mechanism.md` §2, INFERRED
/// U+V wrap). sRGB + no mips, same as the clamp sprite (UI art, one authored gamma round-trip).
pub fn sprite_image_tiled(width: u32, height: u32, rgba: Vec<u8>) -> Image {
    let mut image = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        rgba,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        ..default()
    });
    image
}

/// The bytes an [`Image`] occupies **on the GPU** — every mip level of every array layer, derived
/// from the `texture_descriptor` alone.
///
/// Read from the descriptor, never from `image.data.len()`: an asset declared
/// [`RenderAssetUsages::RENDER_WORLD`] has its bytes *moved* into the render world on extract
/// (`bevy_render::render_asset`), so main-side `data` is `None` for its whole life. A meter that
/// measures `data` therefore reads 0 for exactly the textures that cost the most — the terrain
/// arrays, the sprite sheets, the world albedo. This is the arithmetic bevy_image itself uses, and
/// it is block-aware, so it stays right if a format is ever uploaded compressed.
pub fn image_gpu_bytes(image: &Image) -> usize {
    let d = &image.texture_descriptor;
    let format = d.format;
    let (bw, bh) = format.block_dimensions();
    // A copy is defined per block, and a block is only whole: a 1×1 mip of a 4×4-block format still
    // costs one full block.
    let Some(block_bytes) = format.block_copy_size(None) else {
        return 0; // multi-planar/depth-stencil: no single block size, and we upload neither
    };
    let volume = d.dimension == TextureDimension::D3;
    (0..d.mip_level_count.max(1))
        .map(|i| {
            let w = (d.size.width >> i).max(1);
            let h = (d.size.height >> i).max(1);
            // Array layers do not halve with the mip level; a 3-D texture's depth does.
            let layers = if volume {
                (d.size.depth_or_array_layers >> i).max(1)
            } else {
                d.size.depth_or_array_layers.max(1)
            };
            let blocks = w.div_ceil(bw) * h.div_ceil(bh);
            (blocks * block_bytes * layers) as usize
        })
        .sum()
}

/// Total bytes for a mip pyramid of `mip_count` levels starting at `top²` RGBA8.
pub fn mip_chain_byte_size(top: u32, mip_count: u32) -> usize {
    (0..mip_count)
        .map(|i| {
            let w = (top >> i).max(1);
            (w * w * 4) as usize
        })
        .sum()
}

/// Append `src` (RGBA8 `sw × sh`) resized to `dw × dh` to `out`, using `NEAREST` filtering in
/// gamma byte space. NEAREST = pure texel-replication (or decimation), so no averaging happens —
/// preserves gamma-encoded bytes verbatim (no darkening). Identity at `(sw, sh) == (dw, dh)`.
pub fn extend_nearest(out: &mut Vec<u8>, src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) {
    if sw == dw && sh == dh {
        out.extend_from_slice(src);
        return;
    }
    let Some(buf) = image::RgbaImage::from_raw(sw, sh, src.to_vec()) else {
        // Shape mismatch — fall back to a black layer rather than panicking.
        out.extend(std::iter::repeat_n(0u8, (dw * dh * 4) as usize));
        return;
    };
    let resized = image::imageops::resize(&buf, dw, dh, image::imageops::FilterType::Nearest);
    out.extend_from_slice(resized.as_raw());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor_only(size: Extent3d, mips: u32, format: TextureFormat) -> Image {
        let mut image = Image::new_uninit(
            size,
            TextureDimension::D2,
            format,
            RenderAssetUsages::RENDER_WORLD,
        );
        image.texture_descriptor.mip_level_count = mips;
        image
    }

    /// The arithmetic must agree with [`mip_chain_byte_size`], which the liquid array already
    /// budgets with — two independent expressions of the same pyramid.
    #[test]
    fn gpu_bytes_agree_with_the_rgba8_chain_arithmetic() {
        for (top, mips) in [(256, 9), (128, 8), (64, 7), (1, 1)] {
            let image = descriptor_only(
                Extent3d {
                    width: top,
                    height: top,
                    depth_or_array_layers: 1,
                },
                mips,
                TextureFormat::Rgba8Unorm,
            );
            assert_eq!(
                image_gpu_bytes(&image),
                mip_chain_byte_size(top, mips),
                "{top}²/{mips}"
            );
        }
    }

    /// The number the liquid lane costs, stated once so it cannot drift silently: 136 frames of a
    /// 256² RGBA8 nine-level pyramid, stacked as array layers.
    #[test]
    fn gpu_bytes_count_every_array_layer() {
        let frames = 136;
        let image = descriptor_only(
            Extent3d {
                width: 256,
                height: 256,
                depth_or_array_layers: frames,
            },
            9,
            TextureFormat::Rgba8Unorm,
        );
        assert_eq!(image_gpu_bytes(&image), 47_535_264); // 45.33 MiB
        assert_eq!(
            image_gpu_bytes(&image),
            mip_chain_byte_size(256, 9) * frames as usize
        );
    }

    /// Block-compressed formats are the point of reading the descriptor rather than a pixel size:
    /// a 4×4-block format still owes a WHOLE block for its 2×2 and 1×1 mips.
    #[test]
    fn gpu_bytes_are_block_aware_and_round_up() {
        let bc1 = descriptor_only(
            Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            3,
            TextureFormat::Bc1RgbaUnorm,
        );
        // 4×4 → 1 block, 2×2 → 1 block, 1×1 → 1 block; BC1 is 8 bytes a block.
        assert_eq!(image_gpu_bytes(&bc1), 24);
        // The same pyramid uncompressed is 4 B/texel with no rounding: 64 + 16 + 4.
        let rgba = descriptor_only(
            Extent3d {
                width: 4,
                height: 4,
                depth_or_array_layers: 1,
            },
            3,
            TextureFormat::Rgba8Unorm,
        );
        assert_eq!(image_gpu_bytes(&rgba), 84);
    }
}
