//! A BLP2 texture decoder for **WoW 1.12.1 (build 5875)** — in-repo, replacing `wow-blp` (decision
//! 0021). Decodes to `Rgba8Unorm` (the gamma-byte form the renderer uploads verbatim).
//!
//! 1.12 ships **BLP2 / Direct** content in three shapes (no JPEG in practice):
//! - **Raw1** — a 256-entry **BGRA** palette + 1-byte indices, with a separate 0/1/4/8-bit alpha block.
//! - **Raw3** — uncompressed BGRA8.
//! - **DXTC** — S3TC blocks; `alpha_type` selects DXT1 (0) / DXT3 (1) / DXT5 (7), decoded via the same
//!   `texpresso` codec `wow-blp` uses, so the output is byte-identical.
//!
//! Two fidelity points that cost the old `wow-blp` two forks, folded in here natively:
//! 1. the palette is **BGRA**, not RGBA (reading it as RGBA swaps R↔B on every palettized Blizzard
//!    atlas — the "Westfall clutter is blue" bug);
//! 2. a stale/unknown `alpha_type` byte (e.g. `2` on alpha-less particle atlases) is **not fatal** —
//!    alpha presence is governed by `alpha_bits`; `alpha_type` only picks the DXT variant.
//!
//! Proven byte-for-byte against `wow-blp` over the real corpus during the decision-0021 migration
//! (oracle test in git history); the `benilla-formats` texture loaders exercise it on every run.
//!
//! Byte access goes through `benilla-bytes` (decision 0064): every header read is bounds-checked,
//! and `width`/`height` are capped at [`MAX_DIM`] so a corrupt header can't turn into a
//! multi-gigabyte allocation in [`decode_dxt`].

use benilla_bytes::ByteExt;

/// Decode error. Anything outside the 1.12 BLP2 envelope surfaces here rather than guessing.
#[derive(Debug)]
pub enum Error {
    NotBlp2,
    Truncated(&'static str),
    UnknownCompression(u8),
    BadColorMap,
    OutOfBounds {
        level: usize,
    },
    /// `width`/`height` from the header exceed [`MAX_DIM`] — most likely a corrupt or hostile
    /// header, since no real 1.12 BLP is anywhere near this large.
    DimensionsTooLarge {
        width: u32,
        height: u32,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotBlp2 => write!(f, "not a BLP2 file (bad magic)"),
            Error::Truncated(what) => write!(f, "truncated BLP: {what}"),
            Error::UnknownCompression(c) => write!(f, "unknown BLP compression {c}"),
            Error::BadColorMap => write!(f, "BLP color map shorter than 256 entries"),
            Error::OutOfBounds { level } => write!(f, "BLP mip level {level} out of bounds"),
            Error::DimensionsTooLarge { width, height } => write!(
                f,
                "BLP dimensions {width}x{height} exceed the {MAX_DIM} sanity cap"
            ),
        }
    }
}

impl std::error::Error for Error {}

type Result<T> = std::result::Result<T, Error>;

/// A decoded BLP: mip-major `Rgba8Unorm`. `mips[0]` is the full image and is always present; deeper
/// levels follow while the file carries them. [`mip_chain_count`](DecodedBlp::mip_chain_count) is the
/// count `wow-blp` reports for the dimensions (what the renderer's authored-mip upload iterates).
pub struct DecodedBlp {
    pub width: u32,
    pub height: u32,
    /// One RGBA8 buffer per decoded mip level, level 0 first.
    pub mips: Vec<MipLevel>,
    mip_chain_count: usize,
}

/// One decoded mip level.
pub struct MipLevel {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl DecodedBlp {
    /// The authored mip-chain length for these dimensions, matching `wow-blp`'s `mipmaps_count()`
    /// (`max(log2 w, log2 h)` when the file has mipmaps, else 0). The renderer's mip-chain upload
    /// iterates exactly this many levels; `mips` may hold one more (level 0) when this is 0.
    pub fn mip_chain_count(&self) -> usize {
        self.mip_chain_count
    }
}

const HEADER_SIZE: usize = 148; // magic..=mip_sizes[16]
const PALETTE_SIZE: usize = 256 * 4;

/// Sanity cap on header `width`/`height`. Real 1.12 art tops out at 1024×1024; this is pure
/// headroom above any real asset, and its only job is bounding allocation: [`decode_dxt`] sizes
/// both its block-padding buffer and its `w*h*4` RGBA output straight from these fields, so a
/// corrupt header claiming e.g. 65535×65535 would otherwise reserve gigabytes before a single
/// byte of pixel data is checked.
const MAX_DIM: u32 = 8192;

/// `wow-blp`'s mip count formula (kept bit-identical, float `log2` and all, so the chain length we
/// report matches the oracle exactly).
fn mip_chain_count(width: u32, height: u32, has_mipmaps: bool) -> usize {
    if has_mipmaps {
        let w = (width as f32).log2() as usize;
        let h = (height as f32).log2() as usize;
        w.max(h)
    } else {
        0
    }
}

fn level_size(width: u32, height: u32, level: usize) -> (u32, u32) {
    if level == 0 {
        (width, height)
    } else {
        ((width >> level).max(1), (height >> level).max(1))
    }
}

/// Decode a BLP2 texture (raw archive bytes) to RGBA8 mip levels.
pub fn decode(bytes: &[u8]) -> Result<DecodedBlp> {
    if bytes.len() < HEADER_SIZE || &bytes[0..4] != b"BLP2" {
        return Err(Error::NotBlp2);
    }
    // content @4 (u32; 1 = Direct — we only handle direct, the only kind 1.12 ships).
    // These reads are already covered by the `HEADER_SIZE` length check above; routed through
    // `ByteExt` anyway for uniformity (decision 0064) — no future reader should have to ask
    // "is this offset guarded?".
    let compression = bytes.u8_at(8).ok_or(Error::Truncated("header"))?;
    let alpha_bits = bytes.u8_at(9).ok_or(Error::Truncated("header"))? as u32;
    let alpha_type = bytes.u8_at(10).ok_or(Error::Truncated("header"))?;
    let has_mipmaps = bytes.u8_at(11).ok_or(Error::Truncated("header"))? != 0;
    let width = bytes.u32_at(12).ok_or(Error::Truncated("header"))?;
    let height = bytes.u32_at(16).ok_or(Error::Truncated("header"))?;
    let offsets: Vec<u32> = (0..16)
        .map(|i| bytes.u32_at(20 + i * 4).ok_or(Error::Truncated("header")))
        .collect::<Result<_>>()?;
    let sizes: Vec<u32> = (0..16)
        .map(|i| bytes.u32_at(84 + i * 4).ok_or(Error::Truncated("header")))
        .collect::<Result<_>>()?;

    // The corrupt-header guard (see `MAX_DIM`): bound before anything downstream sizes an
    // allocation from these fields. Zero is left alone — see the decode_dxt/decode_raw1/
    // decode_raw3 zero-pixel behavior, unchanged from before this migration.
    if width > MAX_DIM || height > MAX_DIM {
        return Err(Error::DimensionsTooLarge { width, height });
    }

    // BLP2 direct content always carries a 256-entry palette right after the header (used only by
    // Raw1; present-but-ignored for Raw3/DXT). Read as BGRA bytes.
    let palette = bytes
        .get(HEADER_SIZE..HEADER_SIZE + PALETTE_SIZE)
        .ok_or(Error::BadColorMap)?;

    let chain = mip_chain_count(width, height, has_mipmaps);
    // Always decode level 0 (blp_to_rgba needs it even when the chain count is 0); then deeper levels
    // up to the chain count, stopping if the file doesn't carry one.
    let n = chain.clamp(1, 16);

    let mut mips = Vec::with_capacity(n);
    for level in 0..n {
        let (lw, lh) = level_size(width, height, level);
        let off = offsets[level] as usize;
        let sz = sizes[level] as usize;
        if level > 0 && (sz == 0 || off == 0) {
            break; // chain ends early (some textures stop before the formula's count)
        }
        let data = bytes
            .get(off..off + sz)
            .ok_or(Error::OutOfBounds { level })?;
        let rgba = match compression {
            1 => decode_raw1(palette, data, lw, lh, alpha_bits),
            2 => decode_dxt(data, lw, lh, alpha_type),
            3 => decode_raw3(data, lw, lh),
            other => return Err(Error::UnknownCompression(other)),
        }?;
        mips.push(MipLevel {
            width: lw,
            height: lh,
            rgba,
        });
    }

    Ok(DecodedBlp {
        width,
        height,
        mips,
        mip_chain_count: chain,
    })
}

/// Palettized: 1-byte indices into a 256-entry BGRA palette, then a packed alpha block (`alpha_bits`).
fn decode_raw1(palette: &[u8], data: &[u8], w: u32, h: u32, alpha_bits: u32) -> Result<Vec<u8>> {
    let px = (w as usize) * (h as usize);
    let indices = data.get(..px).ok_or(Error::Truncated("raw1 indices"))?;
    let alpha = &data[px..];
    let mut out = vec![0u8; px * 4];
    for i in 0..px {
        let ci = indices[i] as usize;
        let p = ci * 4;
        // Palette is BGRA: byte0=B, byte1=G, byte2=R. (The fork fix — RGBA reading swaps R↔B.)
        out[i * 4] = palette[p + 2]; // R
        out[i * 4 + 1] = palette[p + 1]; // G
        out[i * 4 + 2] = palette[p]; // B
        out[i * 4 + 3] = alpha_at(alpha, i, alpha_bits);
    }
    Ok(out)
}

/// Per-pixel alpha from the packed block. `0` ⇒ opaque (no alpha channel); `1/4/8` ⇒ packed bits.
fn alpha_at(alpha: &[u8], i: usize, alpha_bits: u32) -> u8 {
    match alpha_bits {
        1 => {
            let bit = (alpha.get(i / 8).copied().unwrap_or(0) >> (i % 8)) & 1;
            if bit == 1 {
                255
            } else {
                0
            }
        }
        4 => {
            let block = alpha.get(i / 2).copied().unwrap_or(0);
            let nib = if i.is_multiple_of(2) {
                block & 0x0F
            } else {
                block >> 4
            };
            (nib << 4) | nib
        }
        8 => alpha.get(i).copied().unwrap_or(255),
        _ => 255, // alpha_bits == 0: fully opaque
    }
}

/// Uncompressed BGRA8 → RGBA8.
fn decode_raw3(data: &[u8], w: u32, h: u32) -> Result<Vec<u8>> {
    let px = (w as usize) * (h as usize);
    let src = data.get(..px * 4).ok_or(Error::Truncated("raw3 pixels"))?;
    let mut out = vec![0u8; px * 4];
    for i in 0..px {
        out[i * 4] = src[i * 4 + 2]; // R
        out[i * 4 + 1] = src[i * 4 + 1]; // G
        out[i * 4 + 2] = src[i * 4]; // B
        out[i * 4 + 3] = src[i * 4 + 3]; // A
    }
    Ok(out)
}

/// DXTC: alpha_type 0→DXT1(Bc1), 1→DXT3(Bc2), 7→DXT5(Bc3); anything else (stale byte) → DXT1, since
/// `alpha_bits` already governs whether alpha is meaningful. Undersized small-mip data is zero-padded
/// to the block size (matching wow-blp / SereniaBLPLib), so a short tail mip still decodes.
fn decode_dxt(data: &[u8], w: u32, h: u32, alpha_type: u8) -> Result<Vec<u8>> {
    let fmt = match alpha_type {
        1 => texpresso::Format::Bc2,
        7 => texpresso::Format::Bc3,
        _ => texpresso::Format::Bc1,
    };
    let need = fmt.compressed_size(w as usize, h as usize);
    let blocks = if data.len() < need {
        let mut p = vec![0u8; need];
        p[..data.len()].copy_from_slice(data);
        std::borrow::Cow::Owned(p)
    } else {
        std::borrow::Cow::Borrowed(data)
    };
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    fmt.decompress(&blocks, w as usize, h as usize, &mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal-but-complete BLP2 header (magic..=mip_sizes[16], `HEADER_SIZE` bytes).
    #[allow(clippy::too_many_arguments)]
    fn header(
        compression: u8,
        alpha_bits: u8,
        alpha_type: u8,
        has_mipmaps: u8,
        width: u32,
        height: u32,
        offsets: [u32; 16],
        sizes: [u32; 16],
    ) -> Vec<u8> {
        let mut b = vec![0u8; HEADER_SIZE];
        b[0..4].copy_from_slice(b"BLP2");
        b[4..8].copy_from_slice(&1u32.to_le_bytes()); // content: 1 = Direct
        b[8] = compression;
        b[9] = alpha_bits;
        b[10] = alpha_type;
        b[11] = has_mipmaps;
        b[12..16].copy_from_slice(&width.to_le_bytes());
        b[16..20].copy_from_slice(&height.to_le_bytes());
        for (i, o) in offsets.iter().enumerate() {
            b[20 + i * 4..24 + i * 4].copy_from_slice(&o.to_le_bytes());
        }
        for (i, s) in sizes.iter().enumerate() {
            b[84 + i * 4..88 + i * 4].copy_from_slice(&s.to_le_bytes());
        }
        b
    }

    #[test]
    fn corrupt_header_huge_dims_errors_cleanly() {
        // The fix under test: a header claiming an absurd width/height must fail fast, not size a
        // multi-gigabyte allocation in decode_dxt. No palette/pixel bytes needed — the dimension
        // check runs before either is touched.
        let b = header(3, 0, 0, 0, 65535, 65535, [0; 16], [0; 16]);
        assert!(matches!(
            decode(&b),
            Err(Error::DimensionsTooLarge {
                width: 65535,
                height: 65535
            })
        ));
        // Right at the cap is fine (still fails later for lack of palette/pixel bytes, but not on
        // the dimension check); just over the cap on either axis alone is rejected.
        let b = header(3, 0, 0, 0, MAX_DIM + 1, 4, [0; 16], [0; 16]);
        assert!(matches!(decode(&b), Err(Error::DimensionsTooLarge { .. })));
        let b = header(3, 0, 0, 0, 4, MAX_DIM + 1, [0; 16], [0; 16]);
        assert!(matches!(decode(&b), Err(Error::DimensionsTooLarge { .. })));
    }

    #[test]
    fn truncated_header_errors_cleanly() {
        // Shorter than HEADER_SIZE, even with a valid magic, is rejected before any field read.
        let mut b = vec![0u8; 10];
        b[0..4].copy_from_slice(b"BLP2");
        assert!(matches!(decode(&b), Err(Error::NotBlp2)));
        assert!(matches!(decode(&[]), Err(Error::NotBlp2)));
        assert!(matches!(decode(b"not a blp at all"), Err(Error::NotBlp2)));
    }

    #[test]
    fn tiny_raw3_decodes_expected_pixels() {
        // A 2x2 uncompressed BGRA8 (Raw3) image, no mipmaps: the smallest concrete shape decode()
        // handles. Pixel data sits right after the header + 256-entry BGRA palette.
        let pixel_offset = (HEADER_SIZE + PALETTE_SIZE) as u32;
        let pixel_size = 2 * 2 * 4;
        let mut offsets = [0u32; 16];
        let mut sizes = [0u32; 16];
        offsets[0] = pixel_offset;
        sizes[0] = pixel_size;
        let mut b = header(3, 8, 0, 0, 2, 2, offsets, sizes);
        b.resize(HEADER_SIZE + PALETTE_SIZE, 0); // palette bytes: unused by Raw3, present anyway
                                                 // Four BGRA8 pixels.
        let bgra: &[u8] = &[
            10, 20, 30, 40, // B G R A
            50, 60, 70, 80, //
            90, 100, 110, 120, //
            130, 140, 150, 160,
        ];
        b.extend_from_slice(bgra);

        let decoded = decode(&b).expect("valid tiny BLP decodes");
        assert_eq!(decoded.width, 2);
        assert_eq!(decoded.height, 2);
        assert_eq!(decoded.mip_chain_count(), 0); // no mipmaps: chain formula reports 0
        assert_eq!(decoded.mips.len(), 1); // level 0 is still decoded regardless

        let mip = &decoded.mips[0];
        assert_eq!(mip.width, 2);
        assert_eq!(mip.height, 2);
        // BGRA -> RGBA per pixel.
        assert_eq!(
            mip.rgba,
            vec![
                30, 20, 10, 40, // R G B A
                70, 60, 50, 80, //
                110, 100, 90, 120, //
                150, 140, 130, 160,
            ]
        );
    }
}
