//! BLP → Bevy [`Image`] loader.
//!
//! Decodes a WoW BLP texture into an `Image` in one of three variants (selected by loader settings):
//! - [`BlpVariant::WorldArt`] — tiling world/model albedo: `Rgba8Unorm` (the RE'd gamma-space
//!   invariant — the GPU does *not* linearize albedo on sample, so shader math stays in WoW's
//!   gamma/byte space), the BLP's **authored** mip pyramid used verbatim (the real 1.12 client cannot
//!   regenerate mips — it uploads the stored ones), repeat + trilinear + anisotropic sampling.
//! - [`BlpVariant::Sprite`] — emissive billboards (sun/moon discs): `Rgba8UnormSrgb`, clamp, mip 0.
//! - [`BlpVariant::Cursor`] — the OS cursor image: `Rgba8UnormSrgb`, single mip.
//!
//! Fidelity rationale (authored mips, the gamma-space `Unorm` invariant) is anchored in
//! `wow-5875-re/system/{terrain,lighting}`; here we only build the GPU resource.

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext, RenderAssetUsages};
use bevy::image::{Image, ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::reflect::TypePath;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use serde::{Deserialize, Serialize};

use benilla_formats::{blp_bytes_to_mip_chain, blp_to_rgba, BlpMipChain};

/// Which on-GPU form a BLP decodes to. See the module docs.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum BlpVariant {
    /// Tiling world/model albedo — `Rgba8Unorm`, authored mips, repeat + anisotropic. The default.
    #[default]
    WorldArt,
    /// Emissive billboard (celestial disc) — `Rgba8UnormSrgb`, clamp, mip 0 only.
    Sprite,
    /// Gamma-lane effect sprite (weather flakes/splashes) — `Rgba8Unorm` like [`Self::WorldArt`]
    /// but **mip 0 only** + clamp: thin cut-out shapes (a snowflake's arms) collapse to
    /// near-zero alpha under the authored mip chain when a small world-space quad minifies —
    /// the reference draws these through point-sprite/near-mip-0 paths and keeps them crisp.
    Effect,
    /// OS cursor image — `Rgba8UnormSrgb`, single mip.
    Cursor,
}

/// Per-load settings for [`BlpImageLoader`]: choose the [`BlpVariant`] via `load_with_settings`.
#[derive(Default, Clone, Serialize, Deserialize)]
pub struct BlpLoaderSettings {
    pub variant: BlpVariant,
}

/// Bevy [`AssetLoader`] decoding `*.blp` → [`Image`].
#[derive(Default, TypePath)]
pub struct BlpImageLoader;

impl AssetLoader for BlpImageLoader {
    type Asset = Image;
    type Settings = BlpLoaderSettings;
    type Error = std::io::Error;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        settings: &Self::Settings,
        ctx: &mut LoadContext<'_>,
    ) -> Result<Image, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let to_io = |e: anyhow::Error| std::io::Error::other(format!("{e:#}"));
        Ok(match settings.variant {
            BlpVariant::WorldArt => world_art_image(
                blp_bytes_to_mip_chain(&bytes).map_err(to_io)?,
                // The address mode rides the asset path (`crate::texture_url`, decision 0763): the
                // sampler is a property of this upload, and one `.blp` legitimately has two.
                crate::sampler_mode_of(&ctx.path().to_string()),
            ),
            BlpVariant::Sprite => {
                let (w, h, rgba) = blp_to_rgba(&bytes).map_err(to_io)?;
                sprite_image(w, h, rgba)
            }
            BlpVariant::Effect => {
                let (w, h, rgba) = blp_to_rgba(&bytes).map_err(to_io)?;
                effect_image(w, h, rgba)
            }
            BlpVariant::Cursor => {
                let (w, h, rgba) = blp_to_rgba(&bytes).map_err(to_io)?;
                cursor_image(w, h, rgba)
            }
        })
    }

    fn extensions(&self) -> &[&str] {
        &["blp"]
    }
}

/// Albedo format: non-sRGB `Rgba8Unorm` — the GPU does not linearize albedo on sample, keeping shader
/// math in WoW's gamma/byte space (the RE'd faithful invariant; `wow-5875-re/system/lighting`).
fn color_format() -> TextureFormat {
    TextureFormat::Rgba8Unorm
}

/// World/model albedo: the BLP's authored mip pyramid laid in verbatim, repeat + trilinear +
/// anisotropic. (Port of the old `repeat_texture_authored`.)
fn world_art_image(chain: BlpMipChain, wrap: (bool, bool)) -> Image {
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
        color_format(),
        RenderAssetUsages::default(),
    );
    image.data = Some(data);
    image.texture_descriptor.mip_level_count = levels;
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
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..Default::default()
    });
    image
}

/// Emissive billboard (celestial disc): sRGB so an unlit pass round-trips the authored gamma bytes to
/// screen; clamp + linear, mip 0 only (the disc maps to one quad and never tiles/minifies far).
fn sprite_image(width: u32, height: u32, rgba: Vec<u8>) -> Image {
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
        ..Default::default()
    });
    image
}

/// Gamma-lane effect sprite: `Rgba8Unorm` (the shader does WoW's byte-space combine), clamp,
/// **mip 0 only** — see [`BlpVariant::Effect`].
fn effect_image(width: u32, height: u32, rgba: Vec<u8>) -> Image {
    let mut image = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        rgba,
        color_format(),
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        ..Default::default()
    });
    image
}

/// OS cursor image: sRGB, single mip (cursors are tiny and need neither a mip chain nor repeat).
fn cursor_image(width: u32, height: u32, rgba: Vec<u8>) -> Image {
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
