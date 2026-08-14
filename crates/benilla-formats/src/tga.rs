//! TGA (Targa) decoder — the OTHER half of the reference's UI texture table.
//!
//! `TextureCreate`'s extension table is `{".tga", ".blp"}` (`0x835248`; see
//! `world_assets::sprite_candidates` in benilla-assets), so every UI sprite reference is two
//! candidate files and one of them is a TGA. Inside the MPQs that arm is a near-no-op — 1.12 ships
//! its UI art as BLP — but **addon folders are where TGAs actually live**: BLP tooling was rare in
//! 2006 and the ecosystem shipped loose `.tga` art constantly, so the loose-file resolve
//! (decision 1322) is what finally makes this decoder load-bearing.
//!
//! The slice implemented is the slice that art actually uses: image types 2/10 (truecolor,
//! raw/RLE) and 3/11 (grayscale, raw/RLE), 8/24/32-bit pixels, both vertical origins and the
//! (vanishingly rare) right-origin mirror. Color-mapped TGAs (types 1/9) are refused — none have
//! surfaced in the corpus, and a silent palette guess is worse than a loud miss.
//!
//! [`tga_to_rgba`] validates the header before touching pixels, so it is safe to use as a
//! *fallback* decoder on bytes of uncertain format (TGA has no magic; the caller sniffs `BLP2`
//! first and only then tries this — a non-TGA almost always fails the type/depth/size checks).

use anyhow::{bail, Result};

/// Decode a TGA to RGBA8: `(width, height, pixels)` — top-down row order, like
/// [`crate::blp_to_rgba`].
pub fn tga_to_rgba(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    let Some(header) = bytes.get(..18) else {
        bail!("TGA: truncated header ({} bytes)", bytes.len());
    };
    let id_length = header[0] as usize;
    let color_map_type = header[1];
    let image_type = header[2];
    let color_map_len = u16::from_le_bytes([header[5], header[6]]) as usize;
    let color_map_entry_bits = header[7] as usize;
    let width = u16::from_le_bytes([header[12], header[13]]) as u32;
    let height = u16::from_le_bytes([header[14], header[15]]) as u32;
    let bpp = header[16];
    let descriptor = header[17];

    let rle = match image_type {
        2 | 3 => false,
        10 | 11 => true,
        1 | 9 => bail!("TGA: color-mapped image (type {image_type}) not supported"),
        t => bail!("TGA: unknown image type {t}"),
    };
    let grayscale = matches!(image_type, 3 | 11);
    let bytes_per_pixel = match (bpp, grayscale) {
        (24, false) => 3,
        (32, false) => 4,
        (8, true) => 1,
        _ => bail!("TGA: unsupported depth {bpp} for image type {image_type}"),
    };
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        bail!("TGA: implausible dimensions {width}x{height}");
    }

    // Skip the id field and (for a truecolor image that still carries one) the color map.
    let map_bytes = if color_map_type == 1 {
        color_map_len * color_map_entry_bits.div_ceil(8)
    } else {
        0
    };
    let data_start = 18 + id_length + map_bytes;
    let Some(data) = bytes.get(data_start..) else {
        bail!("TGA: truncated before pixel data");
    };

    let pixel_count = (width as usize) * (height as usize);
    let mut pixels = Vec::with_capacity(pixel_count * bytes_per_pixel);
    if rle {
        // RLE packets: header bit 7 = run (one pixel, repeated count times), else raw
        // (count pixels follow); count = low 7 bits + 1. Runs may NOT cross the image end.
        let mut at = 0usize;
        while pixels.len() < pixel_count * bytes_per_pixel {
            let Some(&packet) = data.get(at) else {
                bail!("TGA: RLE stream truncated");
            };
            at += 1;
            let count = ((packet & 0x7f) as usize) + 1;
            if packet & 0x80 != 0 {
                let Some(px) = data.get(at..at + bytes_per_pixel) else {
                    bail!("TGA: RLE run truncated");
                };
                at += bytes_per_pixel;
                for _ in 0..count {
                    pixels.extend_from_slice(px);
                }
            } else {
                let n = count * bytes_per_pixel;
                let Some(px) = data.get(at..at + n) else {
                    bail!("TGA: RLE raw packet truncated");
                };
                at += n;
                pixels.extend_from_slice(px);
            }
        }
        pixels.truncate(pixel_count * bytes_per_pixel);
    } else {
        let n = pixel_count * bytes_per_pixel;
        let Some(px) = data.get(..n) else {
            bail!("TGA: pixel data truncated");
        };
        pixels.extend_from_slice(px);
    }

    // BGR(A)/gray → RGBA, honouring the descriptor's origin bits (bit 5: top-origin, bit 4:
    // right-origin — TGA's default is bottom-left, our output is top-down left-to-right).
    let top_down = descriptor & 0x20 != 0;
    let right_first = descriptor & 0x10 != 0;
    let mut rgba = vec![0u8; pixel_count * 4];
    for y in 0..height as usize {
        let src_y = if top_down { y } else { height as usize - 1 - y };
        for x in 0..width as usize {
            let src_x = if right_first {
                width as usize - 1 - x
            } else {
                x
            };
            let s = (src_y * width as usize + src_x) * bytes_per_pixel;
            let d = (y * width as usize + x) * 4;
            match bytes_per_pixel {
                1 => {
                    let g = pixels[s];
                    rgba[d..d + 4].copy_from_slice(&[g, g, g, 0xff]);
                }
                3 => {
                    rgba[d..d + 4].copy_from_slice(&[pixels[s + 2], pixels[s + 1], pixels[s], 0xff])
                }
                _ => {
                    rgba[d..d + 4].copy_from_slice(&[
                        pixels[s + 2],
                        pixels[s + 1],
                        pixels[s],
                        pixels[s + 3],
                    ]);
                }
            }
        }
    }
    Ok((width, height, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal TGA: 18-byte header + pixels.
    fn tga(image_type: u8, w: u16, h: u16, bpp: u8, descriptor: u8, data: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; 18];
        out[2] = image_type;
        out[12..14].copy_from_slice(&w.to_le_bytes());
        out[14..16].copy_from_slice(&h.to_le_bytes());
        out[16] = bpp;
        out[17] = descriptor;
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn uncompressed_32bit_bottom_origin_flips_and_swizzles() {
        // 2 wide × 2 tall, bottom-up rows: file row 0 is the image's BOTTOM row.
        // Bottom row: blue, green; top row: red, half-alpha white — as BGRA.
        let px = [
            255, 0, 0, 255, /* blue */ 0, 255, 0, 255, /* green */
            0, 0, 255, 255, /* red */ 255, 255, 255, 128, /* white a=128 */
        ];
        let (w, h, rgba) = tga_to_rgba(&tga(2, 2, 2, 32, 0, &px)).unwrap();
        assert_eq!((w, h), (2, 2));
        // Top-down output: row 0 = red, white; row 1 = blue, green — as RGBA.
        assert_eq!(
            rgba,
            vec![
                255, 0, 0, 255, 255, 255, 255, 128, //
                0, 0, 255, 255, 0, 255, 0, 255,
            ]
        );
    }

    #[test]
    fn uncompressed_24bit_top_origin_gets_opaque_alpha() {
        let px = [255, 0, 0, /* blue */ 0, 0, 255 /* red */];
        let (w, h, rgba) = tga_to_rgba(&tga(2, 2, 1, 24, 0x20, &px)).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(rgba, vec![0, 0, 255, 255, 255, 0, 0, 255]);
    }

    #[test]
    fn rle_run_and_raw_packets_expand() {
        // 4×1 top-origin: a run of 3 red + a raw packet of 1 green (BGR, 24-bit).
        let data = [0x82, 0, 0, 255, 0x00, 0, 255, 0];
        let (w, _, rgba) = tga_to_rgba(&tga(10, 4, 1, 24, 0x20, &data)).unwrap();
        assert_eq!(w, 4);
        assert_eq!(
            rgba,
            vec![
                255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, //
                0, 255, 0, 255,
            ]
        );
    }

    #[test]
    fn grayscale_replicates_channels() {
        let (_, _, rgba) = tga_to_rgba(&tga(3, 1, 1, 8, 0x20, &[7])).unwrap();
        assert_eq!(rgba, vec![7, 7, 7, 255]);
    }

    #[test]
    fn garbage_and_unsupported_types_are_loud_misses() {
        assert!(tga_to_rgba(b"BLP2xxxxxxxxxxxxxxxxxx").is_err()); // a BLP is not a TGA
        assert!(tga_to_rgba(&[0u8; 4]).is_err()); // truncated header
        assert!(tga_to_rgba(&tga(1, 2, 2, 8, 0, &[0; 32])).is_err()); // color-mapped
        assert!(tga_to_rgba(&tga(2, 2, 2, 16, 0, &[0; 32])).is_err()); // 16-bit
        assert!(tga_to_rgba(&tga(2, 2, 2, 32, 0, &[0; 4])).is_err()); // truncated pixels
    }
}
