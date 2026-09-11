//! Capture sharpening v1: masked, eight-iteration Richardson–Lucy on camera RGB.
//! The guide is the mean of positive camera channels (not colorimetric luminance).
//! A common bounded RGB gain preserves chromaticity and signed scene-linear data.

use rayon::prelude::*;
use rohditor_edit::CaptureSharpening;
use rohditor_image::LinearRgbImage;

use crate::{CancellationToken, PipelineError};

pub const CAPTURE_SHARPENING_ALGORITHM_VERSION: u16 = 1;
pub const CAPTURE_SHARPENING_ITERATIONS: usize = 8;
const FLOOR: f32 = 1.0e-6;

/// Settings and implementation identity of an already processed base.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptureSharpeningProvenance {
    pub settings: CaptureSharpening,
    pub algorithm_version: u16,
}

impl CaptureSharpeningProvenance {
    #[must_use]
    pub fn for_settings(settings: CaptureSharpening) -> Option<Self> {
        settings.is_active().then_some(Self {
            settings,
            algorithm_version: CAPTURE_SHARPENING_ALGORITHM_VERSION,
        })
    }
}

/// Six reusable single-channel planes; kernel storage is bounded to nine floats.
pub(crate) fn scratch_bytes(width: usize, height: usize) -> Result<usize, PipelineError> {
    width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(6 * size_of::<f32>()))
        .and_then(|n| n.checked_add(9 * size_of::<f32>()))
        .ok_or(PipelineError::InvalidDimensions {
            width,
            height,
            row_stride: width,
            reason: "capture sharpening scratch size overflow".to_owned(),
        })
}

/// `clip_levels` are camera-native per-channel highlight detection ceilings.
/// RGB is untouched for disabled/zero amount settings, including padding bytes.
pub(crate) fn apply_cancellable(
    image: &mut LinearRgbImage<f32>,
    settings: CaptureSharpening,
    clip_levels: [f32; 3],
    cancellation: &CancellationToken,
) -> Result<(), PipelineError> {
    settings.validate()?;
    cancellation.checkpoint()?;
    if !settings.is_active() {
        return Ok(());
    }
    let (width, height, stride) = (image.width(), image.height(), image.row_stride());
    scratch_bytes(width, height)?;
    let n = width * height;
    let kernel = gaussian(settings.radius);
    let mut guide = vec![0.0; n];
    let mut mask = vec![0.0; n];
    guide
        .par_chunks_mut(width)
        .zip(mask.par_chunks_mut(width))
        .enumerate()
        .try_for_each(|(y, (guide_row, mask_row))| {
            cancellation.checkpoint()?;
            for (x, (g, m)) in guide_row.iter_mut().zip(mask_row).enumerate() {
                let rgb = &image.data()[y * stride + x * 3..][..3];
                if rgb.iter().any(|v| !v.is_finite()) {
                    return Err(PipelineError::NonFiniteImageData {
                        stage: "capture sharpening",
                        x,
                        y,
                    });
                }
                *g = rgb.iter().map(|v| v.max(0.0) / 3.0).sum::<f32>().max(FLOOR);
                let peak = rgb
                    .iter()
                    .zip(clip_levels)
                    .map(|(v, level)| v / level)
                    .fold(0.0_f32, f32::max);
                *m = 1.0 - smoothstep(0.85, 0.98, peak);
            }
            Ok(())
        })?;
    let mut estimate = guide.clone();
    let mut temporary = vec![0.0; n];
    let mut blurred = vec![0.0; n];
    let mut ratio = vec![0.0; n];

    // Expand highlight protection into neighboring pixels using the PSF support.
    // Retain the local minimum as well as the softened neighborhood protection.
    blur(
        &mask,
        &mut temporary,
        &mut blurred,
        width,
        &kernel,
        cancellation,
    )?;
    mask.par_iter_mut()
        .zip(&blurred)
        .for_each(|(m, b)| *m = m.min(b.powi(4)));
    blur(
        &guide,
        &mut temporary,
        &mut blurred,
        width,
        &kernel,
        cancellation,
    )?;
    mask.par_chunks_mut(width)
        .enumerate()
        .try_for_each(|(y, row)| {
            cancellation.checkpoint()?;
            for (x, m) in row.iter_mut().enumerate() {
                let i = y * width + x;
                // Fixed scene-linear floor plus a relative term: never sharpen flat
                // patches simply because their mean is close to black.
                let threshold = 0.0005
                    + settings.noise_protection * 0.008
                    + blurred[i] * settings.noise_protection * 0.02;
                let detail = (guide[i] - blurred[i]).abs();
                *m *= smoothstep(threshold, threshold * 3.0, detail)
                    * smoothstep(0.001, 0.01, guide[i]);
            }
            Ok::<_, PipelineError>(())
        })?;

    // For a symmetric Gaussian and half-sample mirrored borders, the blur is
    // self-adjoint. Each iteration uses the same source observation, not its mask.
    for _ in 0..CAPTURE_SHARPENING_ITERATIONS {
        blur(
            &estimate,
            &mut temporary,
            &mut blurred,
            width,
            &kernel,
            cancellation,
        )?;
        ratio
            .par_iter_mut()
            .zip(&guide)
            .zip(&blurred)
            .for_each(|((r, g), b)| *r = g / b.max(FLOOR));
        blur(
            &ratio,
            &mut temporary,
            &mut blurred,
            width,
            &kernel,
            cancellation,
        )?;
        estimate
            .par_chunks_mut(width)
            .enumerate()
            .try_for_each(|(y, row)| {
                cancellation.checkpoint()?;
                for (x, e) in row.iter_mut().enumerate() {
                    let i = y * width + x;
                    // Bound runaway estimates at every iteration, as well as the
                    // final gain. This intentionally regularizes classical RL.
                    *e = (*e * blurred[i]).clamp(guide[i] * 0.5, guide[i] * 2.0);
                }
                Ok::<_, PipelineError>(())
            })?;
    }
    image
        .data_mut()
        .par_chunks_mut(stride)
        .enumerate()
        .try_for_each(|(y, row)| {
            cancellation.checkpoint()?;
            for (x, rgb) in row[..width * 3].chunks_exact_mut(3).enumerate() {
                let i = y * width + x;
                let gain = 1.0 + settings.amount * mask[i] * (estimate[i] / guide[i] - 1.0);
                if gain != 1.0 && rgb.iter().all(|v| (v * gain).is_finite()) {
                    for channel in rgb {
                        *channel *= gain;
                    }
                }
            }
            Ok(())
        })
}

fn smoothstep(low: f32, high: f32, value: f32) -> f32 {
    let t = ((value - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn gaussian(sigma: f32) -> Vec<f32> {
    let radius = (3.0 * sigma).ceil() as i32;
    let mut kernel: Vec<_> = (-radius..=radius)
        .map(|x| (-0.5 * (x as f32 / sigma).powi(2)).exp())
        .collect();
    let sum: f32 = kernel.iter().sum();
    kernel.iter_mut().for_each(|v| *v /= sum);
    kernel
}

/// Reflect about the outer pixel boundary, including for 1-pixel images.
fn mirror(index: isize, length: usize) -> usize {
    let period = (length * 2) as isize;
    let folded = index.rem_euclid(period) as usize;
    if folded < length {
        folded
    } else {
        length * 2 - 1 - folded
    }
}

fn blur(
    input: &[f32],
    temporary: &mut [f32],
    output: &mut [f32],
    width: usize,
    kernel: &[f32],
    cancellation: &CancellationToken,
) -> Result<(), PipelineError> {
    let height = input.len() / width;
    let radius = (kernel.len() / 2) as isize;
    temporary
        .par_chunks_mut(width)
        .enumerate()
        .try_for_each(|(y, row)| {
            cancellation.checkpoint()?;
            row.fill(0.0);
            let r = radius as usize;
            if width > 2 * r {
                // Interior loops have no modulo or bounds-dependent mirror branch;
                // iterating over taps outside pixels permits SIMD across each row.
                for (k, weight) in kernel.iter().enumerate() {
                    let source = &input[y * width + k..][..width - 2 * r];
                    for (v, sample) in row[r..width - r].iter_mut().zip(source) {
                        *v += sample * weight;
                    }
                }
            }
            for (x, v) in row.iter_mut().enumerate() {
                if width <= 2 * r || x < r || x >= width - r {
                    *v = kernel
                        .iter()
                        .enumerate()
                        .map(|(k, weight)| {
                            input[y * width + mirror(x as isize + k as isize - radius, width)]
                                * weight
                        })
                        .sum();
                }
            }
            Ok::<_, PipelineError>(())
        })?;
    output
        .par_chunks_mut(width)
        .enumerate()
        .try_for_each(|(y, row)| {
            cancellation.checkpoint()?;
            row.fill(0.0);
            for (k, weight) in kernel.iter().enumerate() {
                let source_y = mirror(y as isize + k as isize - radius, height);
                let source = &temporary[source_y * width..][..width];
                for (v, sample) in row.iter_mut().zip(source) {
                    *v += sample * weight;
                }
            }
            Ok(())
        })
}

#[cfg(test)]
mod qualification;
#[cfg(test)]
mod tests;
