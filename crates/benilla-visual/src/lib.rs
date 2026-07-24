//! Perceptual image-diff metrics for the Phase-5 visual A/B render harness (decision 0008).
//!
//! The harness captures deterministic screenshots of benilla (`$WOW_CAPTURE`, see the `capture` module
//! in the `benilla` crate) and diffs them. This crate is the pure-math half: given two equally-sized
//! images it reports how far they diverge ([`Metrics`]) and renders a heatmap of *where* ([`diff_image`]).
//!
//! Two uses, one tool: (1) **self-regression** — capture baselines on the current pipeline, then diff
//! every linear-HDR rework step against them so a machine catches a regression before the director's
//! eye; (2) **determinism check** — diffing two captures of the same scenario must come out ≈0, which is
//! what makes (1) trustworthy.

use image::RgbImage;

/// A pixel counts as "changed" for [`Metrics::pct_over`] if any channel differs by more than this many
/// byte units. 8/255 ≈ 3% — above dithering/rounding noise, below a real visual shift.
pub const OVER_THRESHOLD: u8 = 8;

/// Per-image difference metrics. Channel deltas are in 0..255 byte units; `pct_over` is a fraction 0..1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Mean absolute per-channel difference (0..255). The headline "how different" number.
    pub mae: f64,
    /// Root-mean-square per-channel difference (0..255) — weights large local deltas more than `mae`.
    pub rmse: f64,
    /// Largest single-channel difference anywhere (0..255).
    pub max_delta: u8,
    /// Fraction of pixels (0..1) whose largest channel delta exceeds [`OVER_THRESHOLD`].
    pub pct_over: f64,
}

/// Compare two equally-sized RGB images. Errors if the dimensions differ.
pub fn compare(a: &RgbImage, b: &RgbImage) -> anyhow::Result<Metrics> {
    if a.dimensions() != b.dimensions() {
        anyhow::bail!(
            "image size mismatch: {:?} vs {:?}",
            a.dimensions(),
            b.dimensions()
        );
    }
    let mut sum_abs = 0u64;
    let mut sum_sq = 0u64;
    let mut max_delta = 0u8;
    let mut over = 0u64;
    for (pa, pb) in a.pixels().zip(b.pixels()) {
        let mut pixel_max = 0u8;
        for c in 0..3 {
            let d = (pa[c] as i32 - pb[c] as i32).unsigned_abs() as u8;
            sum_abs += d as u64;
            sum_sq += (d as u64) * (d as u64);
            max_delta = max_delta.max(d);
            pixel_max = pixel_max.max(d);
        }
        if pixel_max > OVER_THRESHOLD {
            over += 1;
        }
    }
    let n_pixels = (a.width() as u64) * (a.height() as u64);
    let n_chan = (n_pixels * 3).max(1) as f64;
    Ok(Metrics {
        mae: sum_abs as f64 / n_chan,
        rmse: (sum_sq as f64 / n_chan).sqrt(),
        max_delta,
        pct_over: over as f64 / n_pixels.max(1) as f64,
    })
}

/// Render an amplified per-channel abs-difference image: each output channel is `|a-b| * amplify`,
/// clamped to 255. This colourises both *where* and *in which channel* the images diverge (a red shift
/// shows red), so the diff is readable at a glance. Errors on a size mismatch.
pub fn diff_image(a: &RgbImage, b: &RgbImage, amplify: u32) -> anyhow::Result<RgbImage> {
    if a.dimensions() != b.dimensions() {
        anyhow::bail!(
            "image size mismatch: {:?} vs {:?}",
            a.dimensions(),
            b.dimensions()
        );
    }
    let (w, h) = a.dimensions();
    let mut out = RgbImage::new(w, h);
    for (x, y, p) in out.enumerate_pixels_mut() {
        let pa = a.get_pixel(x, y);
        let pb = b.get_pixel(x, y);
        for c in 0..3 {
            let d = (pa[c] as i32 - pb[c] as i32).unsigned_abs();
            p[c] = (d * amplify).min(255) as u8;
        }
    }
    Ok(out)
}

/// Stitch two images side by side (`left | right`) with a `gap`-px dark separator, for at-a-glance A/B
/// comparison (e.g. the faithful vs modern render). Heights may differ; the output is the max height
/// with each image top-aligned.
pub fn compose_side_by_side(left: &RgbImage, right: &RgbImage, gap: u32) -> RgbImage {
    let h = left.height().max(right.height());
    let w = left.width() + gap + right.width();
    let mut out = image::RgbImage::from_pixel(w, h, image::Rgb([24, 24, 24]));
    image::imageops::overlay(&mut out, left, 0, 0);
    image::imageops::overlay(&mut out, right, (left.width() + gap) as i64, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> RgbImage {
        RgbImage::from_pixel(w, h, Rgb(rgb))
    }

    #[test]
    fn identical_images_are_zero() {
        let img = solid(4, 4, [120, 60, 200]);
        let m = compare(&img, &img).unwrap();
        assert_eq!(m.max_delta, 0);
        assert_eq!(m.mae, 0.0);
        assert_eq!(m.rmse, 0.0);
        assert_eq!(m.pct_over, 0.0);
    }

    #[test]
    fn constant_offset_matches_offset() {
        // Every channel of `b` is 10 below `a`, so mae == rmse == max_delta == 10, and (10 > 8) so
        // every pixel is "over" → pct_over == 1.
        let a = solid(8, 5, [100, 100, 100]);
        let b = solid(8, 5, [90, 90, 90]);
        let m = compare(&a, &b).unwrap();
        assert_eq!(m.max_delta, 10);
        assert!((m.mae - 10.0).abs() < 1e-9);
        assert!((m.rmse - 10.0).abs() < 1e-9);
        assert!((m.pct_over - 1.0).abs() < 1e-9);
    }

    #[test]
    fn small_offset_is_under_threshold() {
        // A 5-byte shift is below OVER_THRESHOLD (8), so no pixel counts as changed even though mae>0.
        let a = solid(8, 5, [100, 100, 100]);
        let b = solid(8, 5, [95, 95, 95]);
        let m = compare(&a, &b).unwrap();
        assert_eq!(m.max_delta, 5);
        assert_eq!(m.pct_over, 0.0);
    }

    #[test]
    fn single_channel_delta() {
        // Only the red channel differs by 30 in one of four pixels.
        let a = solid(2, 2, [10, 10, 10]);
        let mut b = a.clone();
        b.put_pixel(0, 0, Rgb([40, 10, 10]));
        let m = compare(&a, &b).unwrap();
        assert_eq!(m.max_delta, 30);
        // one changed pixel of four
        assert!((m.pct_over - 0.25).abs() < 1e-9);
        // 30 over 12 channels = 2.5
        assert!((m.mae - 2.5).abs() < 1e-9);
    }

    #[test]
    fn size_mismatch_errors() {
        let a = solid(4, 4, [0, 0, 0]);
        let b = solid(4, 5, [0, 0, 0]);
        assert!(compare(&a, &b).is_err());
    }

    #[test]
    fn diff_image_amplifies_and_clamps() {
        let a = solid(2, 1, [10, 10, 10]);
        let mut b = a.clone();
        b.put_pixel(0, 0, Rgb([20, 10, 10])); // red delta 10
        let d = diff_image(&a, &b, 8).unwrap();
        // 10*8 = 80 in red at (0,0); other channels/pixels zero.
        assert_eq!(d.get_pixel(0, 0), &Rgb([80, 0, 0]));
        assert_eq!(d.get_pixel(1, 0), &Rgb([0, 0, 0]));
        // amplify saturates: 10*32 = 320 -> 255.
        let d2 = diff_image(&a, &b, 32).unwrap();
        assert_eq!(d2.get_pixel(0, 0), &Rgb([255, 0, 0]));
    }

    #[test]
    fn compose_places_both_with_gap() {
        let l = solid(3, 2, [10, 20, 30]);
        let r = solid(4, 2, [40, 50, 60]);
        let out = compose_side_by_side(&l, &r, 1);
        assert_eq!(out.dimensions(), (3 + 1 + 4, 2));
        assert_eq!(out.get_pixel(0, 0), &Rgb([10, 20, 30])); // left image
        assert_eq!(out.get_pixel(3, 0), &Rgb([24, 24, 24])); // separator
        assert_eq!(out.get_pixel(4, 0), &Rgb([40, 50, 60])); // right image
    }
}
