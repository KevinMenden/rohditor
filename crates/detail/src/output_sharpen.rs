//! Output sharpening for display-encoded images.
//!
//! Runs on transfer-encoded floats right before quantization. It restores the
//! acutance lost to resizing or the output medium; it is not capture
//! sharpening (which deconvolves lens blur at source resolution).

use rayon::prelude::*;
use rohditor_image::DisplayRgbImage;

use crate::{CancellationCheck, DetailError, Plane, for_each_row, gaussian_blur, mirror};

/// Rec.709 luma weights applied to encoded values, so sharpening is
/// perceptually even across tones.
const LUMA_WEIGHTS: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// Unsharp-mask parameters. All values are in encoded (display) units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputSharpen {
    /// Gaussian radius of the blur subtracted from luma, in output pixels.
    pub sigma: f32,
    /// Fraction of the high-pass detail added back.
    pub amount: f32,
    /// Details with `|d| < threshold` are suppressed and fade in up to
    /// `2 * threshold`, so flat noise is not sharpened.
    pub threshold: f32,
    /// How far the result may exceed the 3x3 local luma range (halo limit).
    pub overshoot: f32,
}

impl OutputSharpen {
    fn validate(self) -> Result<(), DetailError> {
        let checks = [
            ("sigma", self.sigma, self.sigma > 0.0),
            ("amount", self.amount, self.amount >= 0.0),
            ("threshold", self.threshold, self.threshold >= 0.0),
            ("overshoot", self.overshoot, self.overshoot >= 0.0),
        ];
        for (name, value, ok) in checks {
            if !value.is_finite() || !ok {
                return Err(DetailError::InvalidParameter { name, value });
            }
        }
        Ok(())
    }
}

/// Scratch bytes [`sharpen_output`] needs beyond the image itself: the luma,
/// blur (with its horizontal pass) and delta planes. `None` on overflow.
#[must_use]
pub fn output_sharpen_scratch_bytes(width: usize, height: usize) -> Option<usize> {
    width.checked_mul(height)?.checked_mul(4 * size_of::<f32>())
}

/// Sharpen `image` in place. Returns an error (leaving the image untouched)
/// on invalid parameters or cancellation.
pub fn sharpen_output(
    image: &mut DisplayRgbImage<f32>,
    params: OutputSharpen,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DetailError> {
    params.validate()?;
    let (width, height, stride) = (image.width(), image.height(), image.row_stride());
    let luma = luma_of(image)?;
    let blurred = gaussian_blur(&luma, params.sigma, cancellation)?;

    let mut delta = vec![0.0_f32; width * height];
    for_each_row(&mut delta, width, cancellation, |y, row| {
        for (x, out) in row.iter_mut().enumerate() {
            *out = pixel_delta(&luma, &blurred, x, y, params);
        }
    })?;

    image
        .data_mut()
        .par_chunks_mut(stride)
        .zip(delta.par_chunks(width))
        .for_each(|(row, delta)| {
            for (px, d) in row.as_chunks_mut::<3>().0.iter_mut().zip(delta) {
                // The same delta on all channels keeps hue.
                px.iter_mut().for_each(|c| *c = (*c + d).clamp(0.0, 1.0));
            }
        });
    Ok(())
}

fn luma_of(image: &DisplayRgbImage<f32>) -> Result<Plane, DetailError> {
    let (width, height) = (image.width(), image.height());
    let mut data = vec![0.0_f32; width * height];
    for (y, row) in data.chunks_mut(width).enumerate() {
        let start = y * image.row_stride();
        let pixels = image.data()[start..start + width * 3].as_chunks::<3>().0;
        for (out, px) in row.iter_mut().zip(pixels) {
            let luma = px[0] * LUMA_WEIGHTS[0] + px[1] * LUMA_WEIGHTS[1] + px[2] * LUMA_WEIGHTS[2];
            *out = luma.clamp(0.0, 1.0);
        }
    }
    Plane::new(width, height, data)
}

/// Luma change at one pixel: soft-thresholded unsharp mask, clamped to the
/// local 3x3 luma range widened by `overshoot`.
fn pixel_delta(luma: &Plane, blurred: &Plane, x: usize, y: usize, p: OutputSharpen) -> f32 {
    let centre = luma.get(x, y);
    let detail = centre - blurred.get(x, y);
    let gain = smoothstep(p.threshold, 2.0 * p.threshold, detail.abs());
    let target = centre + p.amount * detail * gain;

    let (width, height) = (luma.width(), luma.height());
    let (mut low, mut high) = (centre, centre);
    for dy in -1_isize..=1 {
        let sy = mirror(y as isize + dy, height);
        for dx in -1_isize..=1 {
            let value = luma.get(mirror(x as isize + dx, width), sy);
            low = low.min(value);
            high = high.max(value);
        }
    }
    target.clamp(low - p.overshoot, high + p.overshoot) - centre
}

/// 0 below `edge0`, 1 above `edge1`; a step at `edge0` when they coincide.
fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    if edge1 <= edge0 {
        return f32::from(value >= edge0);
    }
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use rohditor_image::DisplayTransfer;

    use super::*;
    use crate::NeverCancelled;

    const PARAMS: OutputSharpen = OutputSharpen {
        sigma: 1.0,
        amount: 1.0,
        threshold: 0.0,
        overshoot: 0.02,
    };

    /// Grey image whose value depends on the column, with unequal width and
    /// height so axis mix-ups show up.
    fn image(width: usize, height: usize, value: impl Fn(usize) -> f32) -> DisplayRgbImage<f32> {
        let data = (0..width * height)
            .flat_map(|i| [value(i % width); 3])
            .collect();
        DisplayRgbImage::new(width, height, width * 3, DisplayTransfer::Srgb, data).unwrap()
    }

    #[test]
    fn flat_image_is_unchanged() {
        let mut flat = image(9, 5, |_| 0.4);
        sharpen_output(&mut flat, PARAMS, &NeverCancelled).unwrap();
        // Blur rounding may leave sub-1e-6 residue; nothing visible.
        assert!(flat.data().iter().all(|v| (v - 0.4).abs() < 1e-6));
    }

    #[test]
    fn step_edge_overshoot_stays_within_halo_limit() {
        let mut edge = image(24, 5, |x| if x < 12 { 0.2 } else { 0.8 });
        sharpen_output(&mut edge, PARAMS, &NeverCancelled).unwrap();
        for y in 0..5 {
            for x in 0..24 {
                let v = edge.pixel(x, y).unwrap()[0];
                assert!((0.2 - 0.02 - 1e-6..=0.8 + 0.02 + 1e-6).contains(&v), "{v}");
            }
        }
        // The edge actually steepens: dark side dips, bright side rises.
        assert!(edge.pixel(11, 2).unwrap()[0] < 0.2);
        assert!(edge.pixel(12, 2).unwrap()[0] > 0.8);
    }

    #[test]
    fn channels_receive_the_same_delta_and_stay_in_range() {
        // 3x2 pixels: saturated colours, a mid grey and a clipped white.
        let data = vec![
            0.9, 0.1, 0.1, 0.1, 0.9, 0.1, 0.1, 0.1, 0.9, //
            0.5, 0.5, 0.5, 1.0, 1.0, 1.0, 0.2, 0.6, 0.4,
        ];
        let mut colour = DisplayRgbImage::new(3, 2, 9, DisplayTransfer::Srgb, data).unwrap();
        let before = colour.clone();
        sharpen_output(&mut colour, PARAMS, &NeverCancelled).unwrap();
        for (a, b) in colour.data().chunks(3).zip(before.data().chunks(3)) {
            let unclamped = (0..3).filter(|&c| a[c] > 0.0 && a[c] < 1.0);
            let deltas: Vec<f32> = unclamped.map(|c| a[c] - b[c]).collect();
            assert!(deltas.windows(2).all(|w| (w[0] - w[1]).abs() < 1e-6));
            assert!(a.iter().all(|c| (0.0..=1.0).contains(c)));
        }
    }

    #[test]
    fn threshold_suppresses_small_detail() {
        let wiggle = |x: usize| if x.is_multiple_of(2) { 0.50 } else { 0.51 };
        let mut noisy = image(16, 3, wiggle);
        let mut params = PARAMS;
        params.threshold = 0.05;
        sharpen_output(&mut noisy, params, &NeverCancelled).unwrap();
        assert_eq!(noisy, image(16, 3, wiggle));
    }

    #[test]
    fn invalid_parameters_and_cancellation_are_errors() {
        let mut img = image(4, 4, |_| 0.5);
        let mut bad = PARAMS;
        bad.sigma = 0.0;
        assert!(sharpen_output(&mut img, bad, &NeverCancelled).is_err());
        bad = PARAMS;
        bad.amount = f32::NAN;
        assert!(sharpen_output(&mut img, bad, &NeverCancelled).is_err());
        assert_eq!(
            sharpen_output(&mut img, PARAMS, &|| true),
            Err(DetailError::Cancelled)
        );
    }
}
