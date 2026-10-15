//! Error metrics between two linear RGB images of equal size.

use rohditor_image::LinearRgbImage;

/// Error summary over the interior of an image (peak signal is 1.0).
#[derive(Debug, Clone, Copy)]
pub struct Comparison {
    pub psnr_db: f64,
    pub rmse: f64,
    pub max_abs: f64,
    pub channel_psnr_db: [f64; 3],
}

/// Compare `test` against `reference`, ignoring `margin` pixels on each side.
///
/// # Panics
/// Panics if the images differ in size or the margin leaves no interior.
#[must_use]
pub fn compare(
    reference: &LinearRgbImage<f32>,
    test: &LinearRgbImage<f32>,
    margin: usize,
) -> Comparison {
    compare_where(reference, test, margin, |_, _| true)
}

/// Mark the `fraction` of pixels with the strongest green-channel gradient
/// of `image` (central differences). Flat regions are everything else.
#[must_use]
pub fn edge_mask(image: &LinearRgbImage<f32>, fraction: f64) -> Vec<bool> {
    let (w, h) = (image.width(), image.height());
    let green = |x: usize, y: usize| image.pixel(x, y).map_or(0.0, |p| p[1]);
    let mut gradient = vec![0.0_f32; w * h];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let gx = green(x + 1, y) - green(x - 1, y);
            let gy = green(x, y + 1) - green(x, y - 1);
            gradient[y * w + x] = gx.hypot(gy);
        }
    }
    let mut sorted = gradient.clone();
    sorted.sort_by(f32::total_cmp);
    let index = ((1.0 - fraction) * (sorted.len() - 1) as f64) as usize;
    let threshold = sorted[index];
    gradient.iter().map(|g| *g >= threshold).collect()
}

/// Like [`compare`], but only over pixels for which `keep(x, y)` holds.
///
/// # Panics
/// Panics if the images differ in size or the margin leaves no interior.
#[must_use]
pub fn compare_where(
    reference: &LinearRgbImage<f32>,
    test: &LinearRgbImage<f32>,
    margin: usize,
    keep: impl Fn(usize, usize) -> bool,
) -> Comparison {
    assert_eq!(
        (reference.width(), reference.height()),
        (test.width(), test.height()),
        "compared images must have equal dimensions"
    );
    assert!(2 * margin < reference.width().min(reference.height()));
    let mut squared = [0.0_f64; 3];
    let mut max_abs = 0.0_f64;
    let mut count = 0_usize;
    for y in margin..reference.height() - margin {
        for x in margin..reference.width() - margin {
            if !keep(x, y) {
                continue;
            }
            let (Some(a), Some(b)) = (reference.pixel(x, y), test.pixel(x, y)) else {
                continue;
            };
            for c in 0..3 {
                let diff = f64::from(a[c]) - f64::from(b[c]);
                squared[c] += diff * diff;
                max_abs = max_abs.max(diff.abs());
            }
            count += 1;
        }
    }
    let mse = |sum: f64| sum / count as f64;
    let psnr = |mse: f64| {
        if mse == 0.0 {
            f64::INFINITY
        } else {
            -10.0 * mse.log10()
        }
    };
    let total = mse(squared.iter().sum::<f64>() / 3.0);
    Comparison {
        psnr_db: psnr(total),
        rmse: total.sqrt(),
        max_abs,
        channel_psnr_db: squared.map(|s| psnr(mse(s))),
    }
}

/// RMS difference between a window of a reconstruction and the mosaic it came
/// from, measured only at the measured CFA sample of each pixel. A demosaic
/// that keeps measured data scores ~0; a large value means the input was
/// misread (wrong black level, matrix, white balance or Bayer phase).
///
/// `window` pixel (0, 0) corresponds to mosaic pixel (`origin`, `origin`).
/// Sites at or above `below` are skipped: tools treat near-saturated samples
/// differently on purpose (clipping, highlight handling).
#[must_use]
pub fn cfa_site_rmse(
    mosaic: &rohditor_image::MosaicImage<f32>,
    window: &LinearRgbImage<f32>,
    origin: usize,
    below: f32,
) -> f64 {
    let mut squared = 0.0_f64;
    let mut count = 0_usize;
    for y in 0..window.height() {
        for x in 0..window.width() {
            let (mx, my) = (x + origin, y + origin);
            let channel = mosaic.pattern().color_at(mx, my).channel_index();
            if *mosaic.sample(mx, my) >= below {
                continue;
            }
            count += 1;
            let measured = f64::from(*mosaic.sample(mx, my));
            let value = window.pixel(x, y).map_or(0.0, |px| f64::from(px[channel]));
            squared += (value - measured) * (value - measured);
        }
    }
    (squared / count.max(1) as f64).sqrt()
}

#[cfg(test)]
mod tests {
    use rohditor_image::{BayerPattern, LinearRgbSpace, MosaicImage};

    use super::*;

    fn rgb(
        width: usize,
        height: usize,
        f: impl Fn(usize, usize, usize) -> f32,
    ) -> LinearRgbImage<f32> {
        let mut data = Vec::new();
        for y in 0..height {
            for x in 0..width {
                data.extend((0..3).map(|c| f(x, y, c)));
            }
        }
        LinearRgbImage::new(width, height, width * 3, LinearRgbSpace::CameraNative, data)
            .expect("valid test image")
    }

    #[test]
    fn identical_images_have_infinite_psnr() {
        let image = rgb(8, 6, |x, y, c| (x + 2 * y + c) as f32 / 32.0);
        let result = compare(&image, &image, 1);
        assert!(result.psnr_db.is_infinite());
        assert_eq!(result.max_abs, 0.0);
    }

    #[test]
    fn single_channel_offset_is_reported_per_channel() {
        let a = rgb(8, 6, |_, _, _| 0.5);
        let b = rgb(8, 6, |_, _, c| if c == 1 { 0.6 } else { 0.5 });
        let result = compare(&a, &b, 0);
        assert!((result.max_abs - 0.1).abs() < 1e-6);
        assert!(result.channel_psnr_db[0].is_infinite());
        assert!((result.channel_psnr_db[1] - 20.0).abs() < 1e-3);
    }

    #[test]
    fn cfa_sites_follow_the_pattern_and_origin() {
        // 4x4 GRBG mosaic; an image that stores each pixel's measured value
        // in its own CFA channel matches exactly, one with swapped red/blue does not.
        let pattern = BayerPattern::Grbg;
        let value = |x: usize, y: usize| (x + 4 * y) as f32 / 16.0;
        let mosaic = MosaicImage::new(
            4,
            4,
            4,
            pattern,
            (0..16).map(|i| value(i % 4, i / 4)).collect(),
        )
        .expect("valid mosaic");
        let exact = rgb(2, 2, |x, y, c| {
            if pattern.color_at(x + 1, y + 1).channel_index() == c {
                value(x + 1, y + 1)
            } else {
                0.0
            }
        });
        assert_eq!(cfa_site_rmse(&mosaic, &exact, 1, 1.0), 0.0);
        let wrong = rgb(2, 2, |_, _, _| 0.0);
        assert!(cfa_site_rmse(&mosaic, &wrong, 1, 1.0) > 0.0);
    }
}
