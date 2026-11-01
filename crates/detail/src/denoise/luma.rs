//! Luminance noise reduction: signal-dependent wavelet shrinkage of the
//! balanced luma `Y = (r + 2g + b) / 4`.
//!
//! `Y` goes through a plain five-level à-trous decomposition (the
//! edge-avoiding variant would use the noisy signal as its own guide). Each
//! detail band is soft-thresholded with a BayesShrink-style threshold
//! `T = strength · k · σ_noise² / σ_signal`:
//!
//! - `σ_noise` is the noise expected in that band at the local brightness
//!   (the shared [`NoiseProfile`] evaluated on the next-coarser smoothing,
//!   scaled by the band's white-noise gain), so shadows are smoothed harder
//!   than highlights, as shot noise demands;
//! - `σ_signal` is what the band's local 7×7 energy leaves after subtracting
//!   that noise, so textured areas get small thresholds and flat ones large.
//!
//! The filtered `Y` is applied back as a per-pixel common gain on the RGB
//! values, which keeps chromaticity and works for signed data.

use rayon::prelude::*;
use rohditor_image::LinearRgbImage;

use crate::atrous::smooth_into;
use crate::{
    CancellationCheck, DetailError, NoiseProfile, Plane, for_each_row, luma_plane, mirror,
};

const LEVELS: usize = 5;
/// Std-dev of each detail band for unit white noise, measured by
/// `band_gains_match_white_noise` (so the table cannot silently drift).
const BAND_NOISE_GAIN: [f32; LEVELS] = [0.890_8, 0.200_6, 0.085_6, 0.041_3, 0.020_3];
/// Threshold multiplier per level.
const LEVEL_WEIGHTS: [f32; LEVELS] = [1.0, 1.0, 1.0, 1.0, 1.0];
/// Local-energy window: `2 * RADIUS + 1` pixels square.
const WINDOW_RADIUS: usize = 3;
/// Floor of `σ_signal` relative to `σ_noise`; it also caps the threshold at
/// `strength · k / SIGNAL_FLOOR` noise sigmas in perfectly flat areas.
const SIGNAL_FLOOR: f32 = 0.4;
/// `luminance_detail` relaxes the thresholds of this many finest levels by
/// up to [`DETAIL_RELIEF`].
const DETAIL_LEVELS: usize = 2;
const DETAIL_RELIEF: f32 = 0.8;
/// Largest gain a pixel's RGB may be scaled by.
const MAX_GAIN: f32 = 4.0;
/// Luma at or below this is too small to scale by (same value capture
/// sharpening uses to guard its divisions).
const LUMA_FLOOR: f32 = 1.0e-6;
/// Planes alive at once: the working band, the smoothing, the accumulated
/// result and two for the local energy.
const SCRATCH_PLANES: usize = 5;

/// Parameters of one luminance denoise pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LumaDenoise {
    /// Strength in `0..=1`; 0 leaves the image untouched.
    pub strength: f32,
    /// Texture preservation in `0..=1`: higher relaxes the two finest
    /// levels. 0.5 is the neutral setting.
    pub detail: f32,
    /// Per-channel gains defining the luma (as-shot white balance); must be
    /// the ones the `noise` profile was estimated with.
    pub balance: [f32; 3],
    /// Per-channel values at or above which a pixel counts as blown and is
    /// left alone.
    pub ceilings: [f32; 3],
    /// Luma noise of the image (see
    /// [`estimate_luma_noise`](super::estimate_luma_noise)).
    pub noise: NoiseProfile,
}

/// Heap bytes the filter allocates on top of the image itself.
#[must_use]
pub fn luma_scratch_bytes(width: usize, height: usize) -> Option<usize> {
    width
        .checked_mul(height)?
        .checked_mul(SCRATCH_PLANES * size_of::<f32>())
}

/// Reduce luminance noise in `image` in place.
///
/// Pixels with a non-finite channel or at or above `ceilings` are copied
/// through, as are pixels whose luma is too small to scale.
pub fn denoise_luma(
    image: &mut LinearRgbImage<f32>,
    params: LumaDenoise,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DetailError> {
    for (name, value) in [("strength", params.strength), ("detail", params.detail)] {
        if !(0.0..=1.0).contains(&value) {
            return Err(DetailError::InvalidParameter { name, value });
        }
    }
    if params.strength == 0.0 {
        return Ok(());
    }
    let shrink = Shrinkage::new(&params);
    let (w, h) = (image.width(), image.height());
    // `current` is the input of the level being processed (and then its
    // detail band); `accumulated` collects the shrunk bands and finally the
    // residual. All buffers are allocated once and reused by every level.
    let mut current = luma_plane(image, params.balance)?;
    let mut smoothed = Plane::filled(w, h, 0.0)?;
    let mut accumulated = Plane::filled(w, h, 0.0)?;
    let mut energy = Plane::filled(w, h, 0.0)?;
    let mut temp = vec![0.0_f32; w * h];
    for level in 0..LEVELS {
        smooth_into(
            current.data(),
            w,
            &mut temp,
            smoothed.data_mut(),
            1 << level,
            cancellation,
        )?;
        // `current` becomes the detail band; `smoothed` stays the input of
        // the next level and the brightness reference of this one.
        current
            .data_mut()
            .par_iter_mut()
            .zip(smoothed.data().par_iter())
            .for_each(|(band, coarse)| *band -= coarse);
        local_mean_square(&current, &mut temp, &mut energy, cancellation)?;
        shrink_band(
            &mut accumulated,
            &current,
            &smoothed,
            &energy,
            &shrink.level(level),
            cancellation,
        )?;
        std::mem::swap(&mut current, &mut smoothed);
    }
    accumulated
        .data_mut()
        .par_iter_mut()
        .zip(current.data().par_iter())
        .for_each(|(acc, residual)| *acc += residual);
    write_back(image, &accumulated, &params, cancellation)
}

/// Strength, detail and noise profile, turned into per-level constants.
struct Shrinkage {
    strength: f32,
    detail: f32,
    noise: NoiseProfile,
}

impl Shrinkage {
    fn new(params: &LumaDenoise) -> Self {
        Self {
            strength: params.strength,
            detail: params.detail,
            noise: params.noise,
        }
    }

    fn level(&self, level: usize) -> LevelShrinkage {
        // Only the finest levels carry texture; relax them as `detail` rises.
        let relief = if level < DETAIL_LEVELS {
            1.0 - DETAIL_RELIEF * self.detail
        } else {
            1.0
        };
        LevelShrinkage {
            scale: self.strength * LEVEL_WEIGHTS[level] * relief,
            noise_gain_squared: BAND_NOISE_GAIN[level] * BAND_NOISE_GAIN[level],
            noise: self.noise,
        }
    }
}

/// Threshold rule of one level.
struct LevelShrinkage {
    scale: f32,
    noise_gain_squared: f32,
    noise: NoiseProfile,
}

impl LevelShrinkage {
    /// Soft threshold for a band pixel whose coarser smoothing is
    /// `brightness` and whose 7×7 neighbourhood has mean square `energy`.
    #[inline]
    fn threshold(&self, brightness: f32, energy: f32) -> f32 {
        let noise_variance = self.noise.variance(brightness) * self.noise_gain_squared;
        let floor = SIGNAL_FLOOR * SIGNAL_FLOOR * noise_variance;
        let sigma_signal = (energy - noise_variance).max(floor).sqrt();
        // A noise-free pixel (variance 0) has nothing to remove.
        if sigma_signal > 0.0 {
            self.scale * noise_variance / sigma_signal
        } else {
            0.0
        }
    }
}

/// Add the soft-thresholded `band` to `accumulated`.
fn shrink_band(
    accumulated: &mut Plane,
    band: &Plane,
    coarse: &Plane,
    energy: &Plane,
    level: &LevelShrinkage,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DetailError> {
    let w = band.width();
    let (band, coarse, energy) = (band.data(), coarse.data(), energy.data());
    accumulated
        .data_mut()
        .par_chunks_mut(w)
        .enumerate()
        .for_each(|(y, acc)| {
            if cancellation.is_cancelled() {
                return;
            }
            let row = y * w..(y + 1) * w;
            for (((acc, &band), &coarse), &energy) in acc
                .iter_mut()
                .zip(&band[row.clone()])
                .zip(&coarse[row.clone()])
                .zip(&energy[row])
            {
                *acc += soft_threshold(band, level.threshold(coarse, energy));
            }
        });
    if cancellation.is_cancelled() {
        Err(DetailError::Cancelled)
    } else {
        Ok(())
    }
}

#[inline]
fn soft_threshold(value: f32, threshold: f32) -> f32 {
    value.signum() * (value.abs() - threshold).max(0.0)
}

/// Mean of `band²` over the `(2 * WINDOW_RADIUS + 1)²` window around each
/// pixel (mirrored at the borders), written to `out`. `temp` holds the
/// horizontal pass.
fn local_mean_square(
    band: &Plane,
    temp: &mut [f32],
    out: &mut Plane,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DetailError> {
    const WINDOW: usize = 2 * WINDOW_RADIUS + 1;
    const NORMALISATION: f32 = 1.0 / (WINDOW * WINDOW) as f32;
    let (w, h) = (band.width(), band.height());
    let src = band.data();
    for_each_row(temp, w, cancellation, |y, row| {
        // Squares with mirrored margins, so every window is contiguous.
        let line = &src[y * w..(y + 1) * w];
        let padded: Vec<f32> = (0..w + 2 * WINDOW_RADIUS)
            .map(|i| {
                let v = line[mirror(i as isize - WINDOW_RADIUS as isize, w)];
                v * v
            })
            .collect();
        row.fill(0.0);
        for k in 0..WINDOW {
            for (sum, v) in row.iter_mut().zip(&padded[k..k + w]) {
                *sum += v;
            }
        }
    })?;
    let horizontal: &[f32] = temp;
    for_each_row(out.data_mut(), w, cancellation, |y, row| {
        row.fill(0.0);
        for k in 0..WINDOW {
            let sy = mirror(y as isize + k as isize - WINDOW_RADIUS as isize, h);
            for (sum, v) in row.iter_mut().zip(&horizontal[sy * w..(sy + 1) * w]) {
                *sum += v;
            }
        }
        for sum in row.iter_mut() {
            *sum *= NORMALISATION;
        }
    })
}

/// Scale each pixel's RGB so its luma becomes the filtered one.
fn write_back(
    image: &mut LinearRgbImage<f32>,
    filtered: &Plane,
    params: &LumaDenoise,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DetailError> {
    let (w, stride) = (image.width(), image.row_stride());
    let filtered = filtered.data();
    let (balance, ceilings) = (params.balance, params.ceilings);
    image
        .data_mut()
        .par_chunks_mut(stride)
        .enumerate()
        .for_each(|(y, row)| {
            if cancellation.is_cancelled() {
                return;
            }
            for (x, px) in row[..w * 3].as_chunks_mut::<3>().0.iter_mut().enumerate() {
                let finite = px.iter().all(|v| v.is_finite());
                let blown = px.iter().zip(&ceilings).any(|(v, c)| v >= c);
                if !finite || blown {
                    continue;
                }
                let old =
                    (px[0] * balance[0] + 2.0 * px[1] * balance[1] + px[2] * balance[2]) * 0.25;
                if old <= LUMA_FLOOR {
                    continue;
                }
                let gain = (filtered[y * w + x] / old).clamp(0.0, MAX_GAIN);
                let out = px.map(|v| v * gain);
                if out.iter().all(|v| v.is_finite()) {
                    *px = out;
                }
            }
        });
    if cancellation.is_cancelled() {
        Err(DetailError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Gauss;
    use crate::{NeverCancelled, atrous_decompose};
    use rohditor_image::LinearRgbSpace;

    #[test]
    fn band_gains_match_white_noise() {
        let (w, h) = (512, 512);
        let mut rng = Gauss(21);
        let noise = Plane::new(w, h, (0..w * h).map(|_| rng.next()).collect()).unwrap();
        let bands = atrous_decompose(&noise, LEVELS as u32, &NeverCancelled).unwrap();
        for (level, band) in bands.details.iter().enumerate() {
            let n = band.data().len() as f64;
            let std = (band
                .data()
                .iter()
                .map(|v| f64::from(*v).powi(2))
                .sum::<f64>()
                / n)
                .sqrt();
            let expected = f64::from(BAND_NOISE_GAIN[level]);
            // Coarse bands are strongly correlated, so allow sampling error.
            assert!(
                (std - expected).abs() / expected < 0.08,
                "level {level}: measured {std}, table {expected}"
            );
        }
    }

    fn mean_square(band: &Plane) -> Plane {
        let (w, h) = (band.width(), band.height());
        let mut temp = vec![0.0; w * h];
        let mut out = Plane::filled(w, h, 0.0).unwrap();
        local_mean_square(band, &mut temp, &mut out, &NeverCancelled).unwrap();
        out
    }

    #[test]
    fn mean_square_is_exact_on_a_constant_and_a_single_spike() {
        let flat = Plane::filled(9, 7, 3.0).unwrap();
        let energy = mean_square(&flat);
        assert!(energy.data().iter().all(|v| (v - 9.0).abs() < 1e-5));
        // Asymmetric plane: a spike inside is seen by exactly the pixels
        // within the window around it.
        let mut data = vec![0.0; 20 * 11];
        data[5 * 20 + 8] = 7.0;
        let spike = Plane::new(20, 11, data).unwrap();
        let energy = mean_square(&spike);
        for y in 0..11_usize {
            for x in 0..20_usize {
                let inside = x.abs_diff(8) <= WINDOW_RADIUS && y.abs_diff(5) <= WINDOW_RADIUS;
                let expected = if inside { 49.0 / 49.0 } else { 0.0 };
                assert!((energy.get(x, y) - expected).abs() < 1e-5, "({x},{y})");
            }
        }
    }

    #[test]
    fn threshold_is_larger_in_flat_areas_and_in_the_shadows() {
        let noise = NoiseProfile {
            a: 0.01,
            b: 0.0004,
            bins: 8,
        };
        let level = LevelShrinkage {
            scale: 1.0,
            noise_gain_squared: 1.0,
            noise,
        };
        let sigma = noise.sigma(0.5);
        let flat = level.threshold(0.5, sigma * sigma);
        let textured = level.threshold(0.5, 100.0 * sigma * sigma);
        assert!(
            flat > 2.0 * sigma && textured < 0.2 * sigma,
            "{flat} {textured}"
        );
        // Same band energy relative to its own noise, darker pixel: the
        // absolute threshold follows the (smaller) noise there, but the
        // shadow noise is relatively larger, so a fixed energy is flatter.
        let dark_sigma = noise.sigma(0.02);
        assert!(level.threshold(0.02, 4.0 * dark_sigma * dark_sigma) < flat);
        assert!(level.threshold(0.02, 0.0) < flat);
        assert_eq!(
            LevelShrinkage {
                scale: 1.0,
                noise_gain_squared: 1.0,
                noise: NoiseProfile {
                    a: 0.0,
                    b: 0.0,
                    bins: 0
                }
            }
            .threshold(0.5, 0.0),
            0.0
        );
    }

    /// Camera RGB = neutral grey ramp + luma noise, plus a padding sentinel.
    fn noisy_image(w: usize, h: usize, stride: usize, sigma: f32) -> LinearRgbImage<f32> {
        let mut rng = Gauss(4);
        let mut data = vec![-7.0_f32; stride * h];
        for y in 0..h {
            for x in 0..w {
                let value = 0.2 + 0.6 * x as f32 / w as f32 + sigma * rng.next();
                let i = y * stride + x * 3;
                data[i..i + 3].fill(value);
            }
        }
        LinearRgbImage::new(w, h, stride, LinearRgbSpace::CameraNative, data).unwrap()
    }

    fn params(strength: f32, sigma: f32) -> LumaDenoise {
        LumaDenoise {
            strength,
            detail: 0.5,
            balance: [1.0; 3],
            ceilings: [f32::INFINITY; 3],
            noise: NoiseProfile {
                a: 0.0,
                b: sigma * sigma,
                bins: 0,
            },
        }
    }

    fn luma_rms_error(image: &LinearRgbImage<f32>, reference: impl Fn(usize) -> f32) -> f64 {
        let luma = luma_plane(image, [1.0; 3]).unwrap();
        let w = luma.width();
        let sum: f64 = luma
            .data()
            .iter()
            .enumerate()
            .map(|(i, v)| f64::from(v - reference(i % w)).powi(2))
            .sum();
        (sum / luma.data().len() as f64).sqrt()
    }

    #[test]
    fn zero_strength_is_a_no_op_and_bad_parameters_are_rejected() {
        let mut image = noisy_image(64, 48, 64 * 3, 0.03);
        let before = image.clone();
        denoise_luma(&mut image, params(0.0, 0.03), &NeverCancelled).unwrap();
        assert_eq!(image, before);
        for bad in [-0.1, 1.5, f32::NAN] {
            assert!(denoise_luma(&mut image, params(bad, 0.03), &NeverCancelled).is_err());
            let mut p = params(0.5, 0.03);
            p.detail = bad;
            assert!(denoise_luma(&mut image, p, &NeverCancelled).is_err());
        }
    }

    #[test]
    fn reduces_noise_keeps_padding_and_neutral_colour() {
        // Asymmetric size with padded stride.
        let (w, h, stride) = (150, 90, 150 * 3 + 5);
        let sigma = 0.03;
        let noisy = noisy_image(w, h, stride, sigma);
        let mut image = noisy.clone();
        denoise_luma(&mut image, params(0.6, sigma), &NeverCancelled).unwrap();
        let truth = |x: usize| 0.2 + 0.6 * x as f32 / w as f32;
        let (before, after) = (luma_rms_error(&noisy, truth), luma_rms_error(&image, truth));
        assert!(after < 0.5 * before, "{before} -> {after}");
        for y in 0..h {
            let pad = &image.data()[y * stride + w * 3..(y + 1) * stride];
            assert!(pad.iter().all(|v| *v == -7.0), "row {y} padding changed");
            for x in 0..w {
                let px = image.pixel(x, y).unwrap();
                assert!(px[0] == px[1] && px[1] == px[2], "common gain keeps grey");
            }
        }
    }

    #[test]
    fn a_strong_edge_survives() {
        let (w, h) = (128, 64);
        let sigma = 0.01;
        let mut rng = Gauss(9);
        let mut data = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 0.2 } else { 0.8 } + sigma * rng.next();
                data.extend_from_slice(&[v; 3]);
            }
        }
        let mut image =
            LinearRgbImage::new(w, h, w * 3, LinearRgbSpace::CameraNative, data).unwrap();
        denoise_luma(&mut image, params(1.0, sigma), &NeverCancelled).unwrap();
        let (dark, bright) = (
            image.pixel(w / 2 - 2, 32).unwrap()[0],
            image.pixel(w / 2 + 1, 32).unwrap()[0],
        );
        assert!(bright - dark > 0.5, "edge contrast {dark} -> {bright}");
    }

    #[test]
    fn non_finite_blown_and_tiny_luma_pixels_pass_through() {
        let mut image = noisy_image(96, 64, 96 * 3, 0.03);
        let stride = image.row_stride();
        let nan_at = stride * 20 + 40 * 3;
        image.data_mut()[nan_at] = f32::NAN;
        let blown_at = stride * 30 + 10 * 3;
        image.data_mut()[blown_at..blown_at + 3].copy_from_slice(&[0.9, 0.9, 0.9]);
        let dark_at = stride * 40 + 50 * 3;
        image.data_mut()[dark_at..dark_at + 3].copy_from_slice(&[-0.01, 0.0, 0.0]);
        let before = image.clone();
        let mut p = params(0.8, 0.03);
        p.ceilings = [0.88; 3];
        denoise_luma(&mut image, p, &NeverCancelled).unwrap();
        assert!(image.data()[nan_at].is_nan());
        for at in [blown_at, dark_at] {
            assert_eq!(&image.data()[at..at + 3], &before.data()[at..at + 3]);
        }
        assert!(
            image.pixel(41, 20).unwrap().iter().all(|v| v.is_finite()),
            "neighbour of a NaN pixel stayed finite"
        );
    }

    #[test]
    fn cancellation_is_reported() {
        let mut image = noisy_image(64, 64, 64 * 3, 0.02);
        let result = denoise_luma(&mut image, params(0.5, 0.02), &|| true);
        assert_eq!(result.unwrap_err(), DetailError::Cancelled);
    }
}
