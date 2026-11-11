//! Texture and clarity: boosting (or softening) the difference between a pixel's luma and a
//! blurred copy of it.
//!
//! The two controls are the same operation at two scales. Working on a perceptually encoded
//! luma keeps the boost even across tones, and applying it to the pixel as a gain keeps hue and
//! saturation. A soft limit on large differences stops a strong edge from growing a halo, and
//! also means softening smooths texture without blurring the edge it sits on.

use rayon::prelude::*;
use rohditor_image::LinearRgbImage;

use super::blur::fast_gaussian;
use super::luma_of;
use crate::{CancellationCheck, DetailError};

/// Added before the square root so the encoding has a finite slope at black, which keeps noise
/// in the darkest tones from being amplified.
const ENCODING_FLOOR: f32 = 1.0e-4;
/// Added to both sides of the gain ratio so a black pixel gets a finite, harmless gain.
const GAIN_FLOOR: f32 = 1.0e-3;
/// Never blur with less than this sigma: smaller kernels collapse to the identity. Where the
/// band's own sigma is smaller (previews below ~1400 px for texture), the amount is scaled
/// down instead; see [`apply_band`].
const MIN_SIGMA: f32 = 1.0;
/// Lowest factor the amount is scaled down to, so tiny images still show the effect.
const MIN_AMOUNT_SCALE: f32 = 0.5;
/// Weight clarity keeps at black and white, where the tone weighting is lowest.
const CLARITY_TONE_FLOOR: f32 = 0.3;

/// One scale of local contrast.
#[derive(Debug, Clone, Copy)]
pub(super) struct Band {
    /// Blur sigma as a fraction of the long edge, so a preview and a full-size export of the
    /// same picture get the same look.
    pub sigma_fraction: f32,
    /// Differences (in encoded luma) at which the effect has fallen to half.
    pub edge_tolerance: f32,
    /// Concentrate the effect in the mid-tones.
    pub favour_midtones: bool,
}

/// Fine detail: a few pixels at preview size.
pub(super) const TEXTURE: Band = Band {
    sigma_fraction: 0.0007,
    edge_tolerance: 0.08,
    favour_midtones: false,
};
/// Broad local contrast, around a hundredth of the frame.
pub(super) const CLARITY: Band = Band {
    sigma_fraction: 0.012,
    edge_tolerance: 0.15,
    favour_midtones: true,
};

/// Square-root encoding of non-negative linear luma. Negative (out-of-gamut) luma encodes as 0.
pub(super) fn encode(luma: f32) -> f32 {
    (luma.max(0.0) + ENCODING_FLOOR).sqrt()
}

fn decode(encoded: f32) -> f32 {
    (encoded * encoded - ENCODING_FLOOR).max(0.0)
}

/// Strengthen (`amount > 0`) or soften (`amount < 0`) `band` by `amount`, in `-1..=1`.
pub(super) fn apply_band(
    image: &mut LinearRgbImage<f32>,
    luma_weights: [f32; 3],
    band: Band,
    amount: f32,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DetailError> {
    let (width, height, stride) = (image.width(), image.height(), image.row_stride());
    let ideal_sigma = band.sigma_fraction * width.max(height) as f32;
    let sigma = ideal_sigma.max(MIN_SIGMA);
    // The detail a blur removes from smooth structure grows with sigma squared, so when the
    // floor makes the blur wider than intended the amount shrinks by the same ratio. A small
    // preview then still looks like the export.
    let amount = amount * (ideal_sigma / sigma).powi(2).clamp(MIN_AMOUNT_SCALE, 1.0);
    let encoded = luma_of(image, luma_weights, encode)?;
    let base = fast_gaussian(&encoded, sigma, cancellation)?;

    image
        .data_mut()
        .par_chunks_mut(stride)
        .zip(encoded.data().par_chunks(width))
        .zip(base.data().par_chunks(width))
        .for_each(|((row, encoded), base)| {
            if cancellation.is_cancelled() {
                return;
            }
            let pixels = row[..width * 3].as_chunks_mut::<3>().0;
            for ((pixel, &encoded), &base) in pixels.iter_mut().zip(encoded).zip(base) {
                let detail = encoded - base;
                let mut weight = 1.0 / (1.0 + (detail / band.edge_tolerance).powi(2));
                if band.favour_midtones {
                    let tone = base.clamp(0.0, 1.0);
                    weight *=
                        CLARITY_TONE_FLOOR + (1.0 - CLARITY_TONE_FLOOR) * 4.0 * tone * (1.0 - tone);
                }
                let target = decode(encoded + amount * weight * detail);
                let current = decode(encoded);
                let gain = (target + GAIN_FLOOR) / (current + GAIN_FLOOR);
                for value in pixel.iter_mut() {
                    *value *= gain;
                }
            }
        });
    if cancellation.is_cancelled() {
        return Err(DetailError::Cancelled);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NeverCancelled;
    use rohditor_image::LinearRgbSpace;

    const WEIGHTS: [f32; 3] = [0.2627, 0.6780, 0.0593];

    fn image(
        width: usize,
        height: usize,
        f: impl Fn(usize, usize) -> [f32; 3],
    ) -> LinearRgbImage<f32> {
        let data = (0..width * height)
            .flat_map(|i| f(i % width, i / width))
            .collect();
        LinearRgbImage::new(width, height, width * 3, LinearRgbSpace::Rec2020D65, data).unwrap()
    }

    fn luma_std(image: &LinearRgbImage<f32>) -> f32 {
        let lumas: Vec<f32> = image
            .data()
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| encode(p[0] * WEIGHTS[0] + p[1] * WEIGHTS[1] + p[2] * WEIGHTS[2]))
            .collect();
        let mean = lumas.iter().sum::<f32>() / lumas.len() as f32;
        (lumas.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / lumas.len() as f32).sqrt()
    }

    /// Fine checkerboard on a mid-grey, so texture sees detail and clarity sees almost none.
    fn checker(x: usize, y: usize) -> [f32; 3] {
        let v = if (x + y).is_multiple_of(2) {
            0.22
        } else {
            0.14
        };
        [v, v * 0.9, v * 0.7]
    }

    #[test]
    fn flat_images_are_unchanged() {
        let mut flat = image(40, 24, |_, _| [0.2, 0.3, 0.1]);
        let before = flat.clone();
        for band in [TEXTURE, CLARITY] {
            for amount in [-1.0, 1.0] {
                apply_band(&mut flat, WEIGHTS, band, amount, &NeverCancelled).unwrap();
            }
        }
        for (a, b) in flat.data().iter().zip(before.data()) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn texture_strengthens_and_softens_fine_detail() {
        let source = image(64, 40, checker);
        let mut stronger = source.clone();
        apply_band(&mut stronger, WEIGHTS, TEXTURE, 1.0, &NeverCancelled).unwrap();
        let mut softer = source.clone();
        apply_band(&mut softer, WEIGHTS, TEXTURE, -1.0, &NeverCancelled).unwrap();
        assert!(luma_std(&stronger) > luma_std(&source) * 1.3);
        assert!(luma_std(&softer) < luma_std(&source) * 0.7);
    }

    #[test]
    fn clarity_adds_contrast_to_a_soft_mid_scale_pattern() {
        // A gentle stripe pattern a few clarity sigmas wide (the blur is about 8 px here).
        let stripes = |x: usize, _: usize| {
            let v = 0.18 + 0.04 * ((x as f32 / 6.4).sin());
            [v, v, v]
        };
        let source = image(640, 16, stripes);
        let mut clearer = source.clone();
        apply_band(&mut clearer, WEIGHTS, CLARITY, 1.0, &NeverCancelled).unwrap();
        assert!(luma_std(&clearer) > luma_std(&source) * 1.2);
    }

    #[test]
    fn hue_is_preserved() {
        let mut coloured = image(48, 30, |x, y| {
            let v = if (x / 2 + y / 2) % 2 == 0 { 0.3 } else { 0.1 };
            [v, v * 0.5, v * 0.25]
        });
        apply_band(&mut coloured, WEIGHTS, TEXTURE, 1.0, &NeverCancelled).unwrap();
        for p in coloured.data().as_chunks::<3>().0 {
            assert!((p[1] / p[0] - 0.5).abs() < 1e-4 && (p[2] / p[0] - 0.25).abs() < 1e-4);
        }
    }

    #[test]
    fn a_strong_edge_does_not_grow_a_halo() {
        let mut step = image(80, 16, |x, _| if x < 40 { [0.02; 3] } else { [0.8; 3] });
        apply_band(&mut step, WEIGHTS, CLARITY, 1.0, &NeverCancelled).unwrap();
        let (dark, bright) = (step.pixel(38, 8).unwrap()[0], step.pixel(41, 8).unwrap()[0]);
        assert!(dark > 0.02 * 0.5, "dark side undershoots to {dark}");
        assert!(bright < 0.8 * 1.15, "bright side overshoots to {bright}");
    }

    #[test]
    fn transposing_the_image_transposes_the_result() {
        let pattern = |x: usize, y: usize| {
            let v = 0.05 + 0.4 * (((x * 7 + y * 13) % 11) as f32 / 11.0);
            [v, v * 0.8, v * 0.6]
        };
        let (w, h) = (47, 29);
        let mut original = image(w, h, pattern);
        let mut transposed = image(h, w, |x, y| pattern(y, x));
        apply_band(&mut original, WEIGHTS, CLARITY, 0.8, &NeverCancelled).unwrap();
        apply_band(&mut transposed, WEIGHTS, CLARITY, 0.8, &NeverCancelled).unwrap();
        for y in 0..h {
            for x in 0..w {
                let (a, b) = (
                    original.pixel(x, y).unwrap(),
                    transposed.pixel(y, x).unwrap(),
                );
                for c in 0..3 {
                    assert!((a[c] - b[c]).abs() < 1e-5);
                }
            }
        }
    }

    #[test]
    fn negative_and_hdr_values_stay_finite() {
        let mut odd = image(30, 20, |x, y| match (x + y) % 3 {
            0 => [-0.05, 0.1, 0.2],
            1 => [4.0, 3.0, 2.0],
            _ => [0.0; 3],
        });
        apply_band(&mut odd, WEIGHTS, TEXTURE, 1.0, &NeverCancelled).unwrap();
        assert!(odd.data().iter().all(|v| v.is_finite()));
    }
}
