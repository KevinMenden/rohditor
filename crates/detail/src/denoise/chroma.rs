//! Chroma noise reduction: wavelet shrinkage of the colour-difference planes,
//! guided by luma so colour does not bleed across luma edges.
//!
//! The image is balanced and split into `Y, C1 = R-G, C2 = B-G` (see
//! [`split_opponent`]). `Y` is never filtered, so the luma of every written
//! pixel is preserved up to float rounding. `C1` and `C2` go through a
//! five-level edge-avoiding à-trous decomposition. Each detail band is
//! soft-thresholded against the colour noise expected at that pixel's
//! brightness (a Poisson–Gaussian fit from [`estimate_noise`]) and summed
//! back together with the residual.

use rayon::prelude::*;
use rohditor_image::LinearRgbImage;

use super::sample_bands;
use crate::{
    CancellationCheck, DetailError, NoiseProfile, Plane, estimate_noise, mirror, split_opponent,
};

const LEVELS: usize = 5;
/// Threshold multiplier per level (finer bands are noisier relative to the
/// colour signal they hide).
const LEVEL_WEIGHTS: [f32; LEVELS] = [1.0, 0.8, 0.6, 0.4, 0.3];
/// Threshold at `strength = 1`, in units of local colour-noise sigma.
const THRESHOLD_SIGMAS: f32 = 5.0;
/// Luma differences up to this many luma-noise sigmas count as "same
/// surface" and are averaged; larger ones are treated as edges.
const RANGE_SIGMAS: f32 = 4.0;
/// Binomial taps. A 3x3 support (9 taps) instead of the B3 spline's 5x5 (25)
/// keeps the guided filter affordable at full resolution; the dilation per
/// level still doubles, so the five levels cover the same scales.
const KERNEL: [f32; 3] = [0.25, 0.5, 0.25];
/// Planes alive at once: Y, two current, two smoothed and two accumulated.
const SCRATCH_PLANES: usize = 7;

/// Parameters of one chroma denoise pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChromaDenoise {
    /// Strength in `0..=1`; 0 leaves the image untouched.
    pub strength: f32,
    /// Per-channel gains that make neutral greys have `C1 = C2 = 0`
    /// (as-shot white balance).
    pub balance: [f32; 3],
    /// Per-channel values at or above which a pixel counts as blown and
    /// keeps its chroma (same ceilings capture sharpening masks with).
    pub ceilings: [f32; 3],
    /// Luma noise of `image` for the same `balance` (see
    /// [`estimate_luma_noise`](super::estimate_luma_noise)). It sets how
    /// strongly luma edges stop the colour averaging.
    pub luma_noise: NoiseProfile,
}

/// Heap bytes the filter allocates on top of the image itself.
#[must_use]
pub fn chroma_scratch_bytes(width: usize, height: usize) -> Option<usize> {
    width
        .checked_mul(height)?
        .checked_mul(SCRATCH_PLANES * size_of::<f32>())
}

/// Reduce colour noise in `image` in place.
///
/// Pixels with a non-finite channel are copied through, as are pixels at or
/// above `ceilings`. Images too small to profile the colour noise are left
/// alone.
pub fn denoise_chroma(
    image: &mut LinearRgbImage<f32>,
    params: ChromaDenoise,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DetailError> {
    if !params.strength.is_finite() || !(0.0..=1.0).contains(&params.strength) {
        return Err(DetailError::InvalidParameter {
            name: "strength",
            value: params.strength,
        });
    }
    if params.strength == 0.0 {
        return Ok(());
    }
    let planes = split_opponent(image, params.balance)?;
    let (mut luma, mut c1, mut c2) = (planes.luma, planes.c1, planes.c2);
    // Non-finite pixels would poison their neighbours; they are restored
    // from the untouched image when writing back.
    for plane in [&mut luma, &mut c1, &mut c2] {
        plane
            .data_mut()
            .iter_mut()
            .filter(|v| !v.is_finite())
            .for_each(|v| *v = 0.0);
    }
    let (w, h) = (luma.width(), luma.height());
    // Profile only a subset of row bands: the estimate needs flat tiles
    // across the brightness range, not every pixel.
    let sampled_luma = sample_bands(&luma)?;
    let (Ok(noise1), Ok(noise2)) = (
        sample_bands(&c1).and_then(|c| estimate_noise(&c, &sampled_luma, cancellation)),
        sample_bands(&c2).and_then(|c| estimate_noise(&c, &sampled_luma, cancellation)),
    ) else {
        // Too small to profile (or cancelled; the next checkpoint reports it).
        return if cancellation.is_cancelled() {
            Err(DetailError::Cancelled)
        } else {
            Ok(())
        };
    };
    let shrink = Shrinkage {
        strength: params.strength,
        luma_noise: params.luma_noise,
        noise: [noise1, noise2],
    };
    let mut pair = Pair::new(c1, c2)?;
    let mut accumulated = Pair::zeros(w, h)?;
    let mut smoothed = Pair::zeros(w, h)?;
    for (level, &weight) in LEVEL_WEIGHTS.iter().enumerate() {
        smooth_and_shrink(
            &pair,
            &mut smoothed,
            &mut accumulated,
            &luma,
            &shrink,
            1 << level,
            weight,
            cancellation,
        )?;
        std::mem::swap(&mut pair, &mut smoothed);
    }
    // Residual (coarsest smoothing) joins the shrunk details.
    for (acc, residual) in [
        (&mut accumulated.c1, &pair.c1),
        (&mut accumulated.c2, &pair.c2),
    ] {
        for (a, r) in acc.data_mut().iter_mut().zip(residual.data()) {
            *a += r;
        }
    }
    write_back(image, &luma, &accumulated, &params, cancellation)
}

/// The two colour planes that are filtered together (they share the
/// edge-stopping weights, which dominate the cost).
struct Pair {
    c1: Plane,
    c2: Plane,
}

impl Pair {
    fn new(c1: Plane, c2: Plane) -> Result<Self, DetailError> {
        c1.same_size(&c2)?;
        Ok(Self { c1, c2 })
    }

    fn zeros(width: usize, height: usize) -> Result<Self, DetailError> {
        Self::new(
            Plane::filled(width, height, 0.0)?,
            Plane::filled(width, height, 0.0)?,
        )
    }
}

struct Shrinkage {
    strength: f32,
    luma_noise: NoiseProfile,
    noise: [NoiseProfile; 2],
}

impl Shrinkage {
    /// Soft threshold for both colour planes at luma level `y`.
    fn thresholds(&self, y: f32, level_weight: f32) -> [f32; 2] {
        let scale = level_weight * self.strength * THRESHOLD_SIGMAS;
        [
            scale * self.noise[0].sigma(y),
            scale * self.noise[1].sigma(y),
        ]
    }

    /// `1 / range_sigma^2` for the edge-stopping weight at luma level `y`.
    fn inv_range_variance(&self, y: f32) -> f32 {
        let sigma = self.luma_noise.sigma(y).max(1e-6) * RANGE_SIGMAS;
        1.0 / (sigma * sigma)
    }
}

/// `exp(-t)` for `t >= 0`, as `(1 - t/8)^8` clamped to zero: monotone, 1 at
/// 0 and ~1e-3 error where the weight still matters.
#[inline]
fn fast_exp_neg(t: f32) -> f32 {
    let x = (1.0 - t * 0.125).max(0.0);
    let x = x * x;
    let x = x * x;
    x * x
}

/// One à-trous level for both planes: write the guided smoothing of
/// `current` to `smoothed` and add the soft-thresholded detail
/// (`current - smoothed`) to `accumulated`.
#[allow(clippy::too_many_arguments)]
fn smooth_and_shrink(
    current: &Pair,
    smoothed: &mut Pair,
    accumulated: &mut Pair,
    luma: &Plane,
    shrink: &Shrinkage,
    step: usize,
    level_weight: f32,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DetailError> {
    let (w, h) = (luma.width(), luma.height());
    let guide = luma.data();
    let ctx = LevelContext {
        w,
        guide,
        src1: current.c1.data(),
        src2: current.c2.data(),
        shrink,
        level_weight,
    };
    let ctx = &ctx;
    smoothed
        .c1
        .data_mut()
        .par_chunks_mut(w)
        .zip(smoothed.c2.data_mut().par_chunks_mut(w))
        .zip(accumulated.c1.data_mut().par_chunks_mut(w))
        .zip(accumulated.c2.data_mut().par_chunks_mut(w))
        .enumerate()
        .for_each(|(y, (((out1, out2), acc1), acc2))| {
            if cancellation.is_cancelled() {
                return;
            }
            let rows: [usize; 3] = std::array::from_fn(|k| {
                mirror(y as isize + (k as isize - 1) * step as isize, h) * w
            });
            let mut row = Row {
                y,
                rows,
                out1,
                out2,
                acc1,
                acc2,
            };
            // Interior pixels have contiguous taps (vectorisable); only the
            // `step`-wide borders need mirrored columns.
            let interior = step.min(w)..w.saturating_sub(step).max(step.min(w));
            for x in (0..interior.start).chain(interior.end..w) {
                let columns = std::array::from_fn(|k| {
                    mirror(x as isize + (k as isize - 1) * step as isize, w)
                });
                filter_pixel(&mut row, x, columns, ctx);
            }
            filter_interior(&mut row, interior, step, ctx);
        });
    if cancellation.is_cancelled() {
        Err(DetailError::Cancelled)
    } else {
        Ok(())
    }
}

/// Read-only inputs of one level.
struct LevelContext<'a> {
    w: usize,
    guide: &'a [f32],
    src1: &'a [f32],
    src2: &'a [f32],
    shrink: &'a Shrinkage,
    level_weight: f32,
}

/// The output row being filled.
struct Row<'a> {
    y: usize,
    /// Start offset of each of the three tap rows.
    rows: [usize; 3],
    out1: &'a mut [f32],
    out2: &'a mut [f32],
    acc1: &'a mut [f32],
    acc2: &'a mut [f32],
}

/// Interior pixels of a row, written over equal-length slices so the
/// compiler can drop bounds checks and vectorise across `x`.
fn filter_interior<'a>(
    row: &mut Row<'_>,
    interior: std::ops::Range<usize>,
    step: usize,
    ctx: &LevelContext<'a>,
) {
    let n = interior.len();
    if n == 0 {
        return;
    }
    let start = interior.start;
    // `taps[j][k]` is tap row j shifted by (k - 1) * step.
    let rows = row.rows;
    let tap = |data: &'a [f32], j: usize, k: usize| -> &'a [f32] {
        let from = rows[j] + start + k * step - step;
        &data[from..from + n]
    };
    let guide: [[&[f32]; 3]; 3] =
        std::array::from_fn(|j| std::array::from_fn(|k| tap(ctx.guide, j, k)));
    let src1: [[&[f32]; 3]; 3] =
        std::array::from_fn(|j| std::array::from_fn(|k| tap(ctx.src1, j, k)));
    let src2: [[&[f32]; 3]; 3] =
        std::array::from_fn(|j| std::array::from_fn(|k| tap(ctx.src2, j, k)));
    let base = row.y * ctx.w + start;
    let (centre_guide, centre1, centre2) = (
        &ctx.guide[base..base + n],
        &ctx.src1[base..base + n],
        &ctx.src2[base..base + n],
    );
    let (out1, out2) = (
        &mut row.out1[start..start + n],
        &mut row.out2[start..start + n],
    );
    let (acc1, acc2) = (
        &mut row.acc1[start..start + n],
        &mut row.acc2[start..start + n],
    );
    for t in 0..n {
        let centre = centre_guide[t];
        let inv = ctx.shrink.inv_range_variance(centre);
        let (mut sum1, mut sum2, mut weight_sum) = (0.0_f32, 0.0_f32, 0.0_f32);
        for j in 0..3 {
            for k in 0..3 {
                let d = guide[j][k][t] - centre;
                let weight = KERNEL[j] * KERNEL[k] * fast_exp_neg(d * d * inv);
                sum1 += weight * src1[j][k][t];
                sum2 += weight * src2[j][k][t];
                weight_sum += weight;
            }
        }
        let (s1, s2) = (sum1 / weight_sum, sum2 / weight_sum);
        out1[t] = s1;
        out2[t] = s2;
        let [t1, t2] = ctx.shrink.thresholds(centre, ctx.level_weight);
        acc1[t] += soft_threshold(centre1[t] - s1, t1);
        acc2[t] += soft_threshold(centre2[t] - s2, t2);
    }
}

#[inline(always)]
fn filter_pixel(row: &mut Row<'_>, x: usize, columns: [usize; 3], ctx: &LevelContext<'_>) {
    let i = row.y * ctx.w + x;
    let centre = ctx.guide[i];
    let inv = ctx.shrink.inv_range_variance(centre);
    let (mut sum1, mut sum2, mut weight_sum) = (0.0_f32, 0.0_f32, 0.0_f32);
    for (&base, &ky) in row.rows.iter().zip(&KERNEL) {
        for (&column, &kx) in columns.iter().zip(&KERNEL) {
            let q = base + column;
            let d = ctx.guide[q] - centre;
            let weight = ky * kx * fast_exp_neg(d * d * inv);
            sum1 += weight * ctx.src1[q];
            sum2 += weight * ctx.src2[q];
            weight_sum += weight;
        }
    }
    // The centre tap has weight > 0, so weight_sum never vanishes.
    let (s1, s2) = (sum1 / weight_sum, sum2 / weight_sum);
    row.out1[x] = s1;
    row.out2[x] = s2;
    let [t1, t2] = ctx.shrink.thresholds(centre, ctx.level_weight);
    row.acc1[x] += soft_threshold(ctx.src1[i] - s1, t1);
    row.acc2[x] += soft_threshold(ctx.src2[i] - s2, t2);
}

#[inline]
fn soft_threshold(value: f32, threshold: f32) -> f32 {
    value.signum() * (value.abs() - threshold).max(0.0)
}

/// Rebuild RGB from the untouched luma and the filtered colour planes,
/// skipping pixels that must keep their input values.
fn write_back(
    image: &mut LinearRgbImage<f32>,
    luma: &Plane,
    chroma: &Pair,
    params: &ChromaDenoise,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DetailError> {
    let (w, stride) = (image.width(), image.row_stride());
    let (luma, c1, c2) = (luma.data(), chroma.c1.data(), chroma.c2.data());
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
                let i = y * w + x;
                // Y = g + (C1 + C2) / 4 (see `merge_opponent`).
                let g = luma[i] - (c1[i] + c2[i]) * 0.25;
                let out = [
                    (g + c1[i]) / balance[0],
                    g / balance[1],
                    (g + c2[i]) / balance[2],
                ];
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
    use crate::{NeverCancelled, estimate_luma_noise};
    use rohditor_image::LinearRgbSpace;

    const BALANCE: [f32; 3] = [2.0, 1.0, 1.5];
    const NO_CEILING: [f32; 3] = [f32::INFINITY; 3];

    fn params(image: &LinearRgbImage<f32>, strength: f32) -> ChromaDenoise {
        ChromaDenoise {
            strength,
            balance: BALANCE,
            ceilings: NO_CEILING,
            luma_noise: luma_noise(image, BALANCE),
        }
    }

    /// Luma profile of `image`; tiny images get an arbitrary one (the
    /// filter then declines to run on its own).
    fn luma_noise(image: &LinearRgbImage<f32>, balance: [f32; 3]) -> NoiseProfile {
        estimate_luma_noise(image, balance, &NeverCancelled)
            .unwrap()
            .unwrap_or(NoiseProfile {
                a: 0.0,
                b: 1e-4,
                bins: 0,
            })
    }

    /// Camera RGB whose balanced version is `luma(x)` grey plus colour noise
    /// of `sigma` on C1/C2 only. Returns the image and the balanced luma.
    fn colour_noise_image(w: usize, h: usize, stride: usize, sigma: f32) -> LinearRgbImage<f32> {
        let mut rng = Gauss(11);
        let mut data = vec![-7.0_f32; stride * h]; // padding sentinel
        for y in 0..h {
            for x in 0..w {
                let luma = 0.2 + 0.6 * x as f32 / w as f32;
                let (c1, c2) = (sigma * rng.next(), sigma * rng.next());
                let g = luma - (c1 + c2) * 0.25;
                let i = y * stride + x * 3;
                data[i] = (g + c1) / BALANCE[0];
                data[i + 1] = g / BALANCE[1];
                data[i + 2] = (g + c2) / BALANCE[2];
            }
        }
        LinearRgbImage::new(w, h, stride, LinearRgbSpace::CameraNative, data).unwrap()
    }

    /// RMS of the balanced colour-difference planes.
    fn chroma_rms(image: &LinearRgbImage<f32>) -> f64 {
        let p = split_opponent(image, BALANCE).unwrap();
        let sum: f64 =
            p.c1.data()
                .iter()
                .chain(p.c2.data())
                .map(|v| f64::from(*v).powi(2))
                .sum();
        (sum / (2 * p.c1.data().len()) as f64).sqrt()
    }

    #[test]
    fn zero_strength_is_a_no_op_and_bad_strength_is_rejected() {
        let mut image = colour_noise_image(64, 48, 64 * 3, 0.02);
        let before = image.clone();
        let p = params(&image, 0.0);
        denoise_chroma(&mut image, p, &NeverCancelled).unwrap();
        assert_eq!(image, before);
        for bad in [-0.1, 1.5, f32::NAN] {
            let p = params(&image, bad);
            assert!(denoise_chroma(&mut image, p, &NeverCancelled).is_err());
        }
    }

    #[test]
    fn halves_colour_noise_and_keeps_luma_and_padding() {
        // Asymmetric size with padded stride.
        let (w, h, stride) = (150, 90, 150 * 3 + 5);
        let noisy = colour_noise_image(w, h, stride, 0.03);
        let mut image = noisy.clone();
        let p = params(&image, 0.5);
        denoise_chroma(&mut image, p, &NeverCancelled).unwrap();
        assert!(
            chroma_rms(&image) < 0.5 * chroma_rms(&noisy),
            "{} vs {}",
            chroma_rms(&image),
            chroma_rms(&noisy)
        );
        let (a, b) = (
            split_opponent(&noisy, BALANCE).unwrap().luma,
            split_opponent(&image, BALANCE).unwrap().luma,
        );
        assert!(
            a.data()
                .iter()
                .zip(b.data())
                .all(|(x, y)| (x - y).abs() < 1e-5)
        );
        for y in 0..h {
            let pad = &image.data()[y * stride + w * 3..(y + 1) * stride];
            assert!(pad.iter().all(|v| *v == -7.0), "row {y} padding changed");
        }
    }

    #[test]
    fn colour_does_not_bleed_across_a_luma_edge() {
        // Left half dark red, right half bright blue, plus colour noise.
        let (w, h) = (128, 64);
        let mut rng = Gauss(5);
        let mut data = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let (r, g, b) = if x < w / 2 {
                    (0.30, 0.10, 0.05)
                } else {
                    (0.15, 0.50, 0.80)
                };
                let n = 0.01 * rng.next();
                data.extend_from_slice(&[r + n, g, b - n]);
            }
        }
        let mut image =
            LinearRgbImage::new(w, h, w * 3, LinearRgbSpace::CameraNative, data).unwrap();
        let before = image.clone();
        let balance = [1.0; 3];
        denoise_chroma(
            &mut image,
            ChromaDenoise {
                strength: 1.0,
                balance,
                ceilings: NO_CEILING,
                luma_noise: luma_noise(&before, balance),
            },
            &NeverCancelled,
        )
        .unwrap();
        // Pixels next to the edge keep their side's hue.
        for (x, red_side) in [(w / 2 - 2, true), (w / 2 + 1, false)] {
            let px = image.pixel(x, 32).unwrap();
            let original = before.pixel(x, 32).unwrap();
            assert!(
                (px[0] - original[0]).abs() < 0.05 && (px[2] - original[2]).abs() < 0.05,
                "x={x} red_side={red_side}: {px:?} vs {original:?}"
            );
        }
    }

    #[test]
    fn non_finite_and_blown_pixels_pass_through_without_poisoning_neighbours() {
        let mut image = colour_noise_image(96, 64, 96 * 3, 0.03);
        let nan_at = image.row_stride() * 20 + 40 * 3;
        image.data_mut()[nan_at] = f32::NAN;
        image.data_mut()[nan_at + 1] = f32::INFINITY;
        let blown_at = image.row_stride() * 30 + 10 * 3;
        image.data_mut()[blown_at..blown_at + 3].copy_from_slice(&[0.9, 0.2, 0.1]);
        let before = image.clone();
        let ceilings = [0.8, 0.8, 0.8];
        denoise_chroma(
            &mut image,
            ChromaDenoise {
                strength: 0.8,
                balance: BALANCE,
                ceilings,
                luma_noise: luma_noise(&before, BALANCE),
            },
            &NeverCancelled,
        )
        .unwrap();
        assert!(image.data()[nan_at].is_nan());
        assert_eq!(image.data()[nan_at + 1], f32::INFINITY);
        assert_eq!(
            &image.data()[blown_at..blown_at + 3],
            &before.data()[blown_at..blown_at + 3]
        );
        let finite_before = before.data().iter().filter(|v| v.is_finite()).count();
        assert!(image.data().iter().filter(|v| v.is_finite()).count() >= finite_before);
        assert!(
            image.pixel(41, 20).unwrap().iter().all(|v| v.is_finite()),
            "neighbour of a NaN pixel stayed finite"
        );
    }

    #[test]
    fn images_smaller_than_the_estimator_tile_are_left_alone() {
        let mut image = colour_noise_image(10, 10, 30, 0.05);
        let before = image.clone();
        let p = params(&image, 1.0);
        denoise_chroma(&mut image, p, &NeverCancelled).unwrap();
        assert_eq!(image, before);
    }

    #[test]
    fn cancellation_is_reported() {
        let mut image = colour_noise_image(64, 64, 64 * 3, 0.02);
        let p = params(&image, 0.5);
        let result = denoise_chroma(&mut image, p, &|| true);
        assert_eq!(result.unwrap_err(), DetailError::Cancelled);
    }
}
