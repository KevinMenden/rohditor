use rayon::prelude::*;
use rohditor_image::{LinearRgbImage, LinearRgbSpace, allocate_zeroed_f32};

use crate::plan::map_coordinates_execution;
use crate::radial::RadialMultipliers;
use crate::resample::{cubic_sample, cubic_weight};
use crate::{CorrectionComponents, LensCorrectionPlan, OpticsError, OpticsProvenance};

/// Minimal cancellation contract implemented by core's cancellation token.
pub trait Cancellation: Sync {
    fn is_cancelled(&self) -> bool;
}

struct NeverCancelled;

impl Cancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Corrected image and resource accounting for one plan application.
#[derive(Debug)]
pub struct CorrectionResult {
    pub image: LinearRgbImage<f32>,
    pub provenance: OpticsProvenance,
    pub output_bytes: usize,
    pub scratch_bytes: usize,
}

impl LensCorrectionPlan {
    /// Apply a plan without cancellation.
    pub fn apply(self, image: LinearRgbImage<f32>) -> Result<CorrectionResult, OpticsError> {
        self.apply_cancellable(image, &NeverCancelled)
    }

    /// Apply a plan with per-row cooperative cancellation.
    pub fn apply_cancellable(
        self,
        image: LinearRgbImage<f32>,
        cancellation: &dyn Cancellation,
    ) -> Result<CorrectionResult, OpticsError> {
        self.execution().apply_cancellable(image, cancellation)
    }
}

impl crate::OpticsExecution {
    /// Execute a previously resolved Lensfun-free correction contract.
    pub fn apply_cancellable(
        &self,
        image: LinearRgbImage<f32>,
        cancellation: &dyn Cancellation,
    ) -> Result<CorrectionResult, OpticsError> {
        self.apply_inner(image, cancellation, None)
    }

    /// Test seam: run one apply with an externally supplied radial table, so
    /// crafted-invalid tables can prove the fallback stays authority-identical.
    #[cfg(test)]
    pub(crate) fn apply_cancellable_forced(
        &self,
        image: LinearRgbImage<f32>,
        cancellation: &dyn Cancellation,
        radial: &RadialMultipliers,
    ) -> Result<CorrectionResult, OpticsError> {
        self.apply_inner(image, cancellation, Some(radial))
    }

    fn apply_inner(
        &self,
        mut image: LinearRgbImage<f32>,
        cancellation: &dyn Cancellation,
        forced_radial: Option<&RadialMultipliers>,
    ) -> Result<CorrectionResult, OpticsError> {
        if image.space() != LinearRgbSpace::CameraNative
            || image.width() != self.width
            || image.height() != self.height
        {
            return Err(OpticsError::WrongImageState);
        }
        if cancellation.is_cancelled() {
            return Err(OpticsError::Cancelled);
        }

        if self.provenance.applied.vignetting {
            apply_vignetting(&mut image, self, cancellation)?;
        }
        if self.provenance.applied.distortion || self.provenance.applied.chromatic_aberration {
            let elements = self
                .width
                .checked_mul(self.height)
                .and_then(|pixels| pixels.checked_mul(3))
                .ok_or(OpticsError::Allocation {
                    elements: usize::MAX,
                })?;
            let data =
                allocate_zeroed_f32(elements).map_err(|_| OpticsError::Allocation { elements })?;
            let mut output = LinearRgbImage::new(
                self.width,
                self.height,
                self.width * 3,
                LinearRgbSpace::CameraNative,
                data,
            )
            .map_err(|error| OpticsError::Correction {
                reason: error.to_string(),
            })?;
            let input = image.data();
            let output_row_stride = output.row_stride();
            // The radial-LUT branch is chosen once per apply, before the row
            // loop, so a partially LUT-applied frame is impossible. An invalid
            // (or forced-invalid) table keeps the authority loop byte-for-byte.
            let built;
            let radial = match forced_radial {
                Some(table) if table.valid => Some(table),
                Some(_) => None,
                None => {
                    built = RadialMultipliers::build(self);
                    built.as_ref().filter(|table| table.valid)
                }
            };
            if let Some(table) = radial {
                let (norm_scale, norm_unscale) = self.normalization();
                let (center_x, center_y) = self.center();
                let scale = f64::from(self.scale());
                let center_px_x = center_x * norm_unscale;
                let center_px_y = center_y * norm_unscale;
                output
                    .data_mut()
                    .par_chunks_mut(output_row_stride)
                    .enumerate()
                    .try_for_each(|(y, row)| -> Result<(), OpticsError> {
                        if cancellation.is_cancelled() {
                            return Err(OpticsError::Cancelled);
                        }
                        let mapped_y = center_px_y + (y as f64 - center_px_y) / scale;
                        let input_y = (mapped_y * norm_scale - center_y) as f32;
                        for x in 0..self.width {
                            let mapped_x = center_px_x + (x as f64 - center_px_x) / scale;
                            let input_x = (mapped_x * norm_scale - center_x) as f32;
                            let multipliers = table.lookup(input_x.hypot(input_y));
                            for channel in 0..3 {
                                // The authority's exact post-multiply step:
                                // f32 multiply, widen, add center, unscale,
                                // narrow once.
                                let source_x =
                                    ((f64::from(input_x * multipliers[channel]) + center_x)
                                        * norm_unscale) as f32;
                                let source_y =
                                    ((f64::from(input_y * multipliers[channel]) + center_y)
                                        * norm_unscale) as f32;
                                row[x * 3 + channel] = sample_cubic_hoisted(
                                    input,
                                    image.row_stride(),
                                    self.width,
                                    self.height,
                                    channel,
                                    source_x,
                                    source_y,
                                )?;
                            }
                        }
                        Ok(())
                    })?;
            } else {
                output
                    .data_mut()
                    .par_chunks_mut(output_row_stride)
                    .enumerate()
                    .try_for_each(|(y, row)| -> Result<(), OpticsError> {
                        if cancellation.is_cancelled() {
                            return Err(OpticsError::Cancelled);
                        }
                        for x in 0..self.width {
                            let coordinates = map_coordinates_execution(self, x, y)?;
                            for channel in 0..3 {
                                row[x * 3 + channel] = cubic_sample(
                                    input,
                                    image.row_stride(),
                                    self.width,
                                    self.height,
                                    channel,
                                    coordinates[channel].0,
                                    coordinates[channel].1,
                                )?;
                            }
                        }
                        Ok(())
                    })?;
            }
            image = output;
        }

        let output_bytes =
            if self.provenance.applied.distortion || self.provenance.applied.chromatic_aberration {
                image
                    .data()
                    .len()
                    .saturating_mul(std::mem::size_of::<f32>())
            } else {
                0
            };
        let scratch_bytes =
            if self.provenance.applied.distortion || self.provenance.applied.chromatic_aberration {
                self.width
                    .checked_mul(6)
                    .and_then(|elements| elements.checked_mul(std::mem::size_of::<f32>()))
                    .unwrap_or(usize::MAX)
            } else {
                0
            };
        Ok(CorrectionResult {
            image,
            provenance: self.provenance.clone(),
            output_bytes,
            scratch_bytes,
        })
    }
}

fn apply_vignetting(
    image: &mut LinearRgbImage<f32>,
    plan: &crate::OpticsExecution,
    cancellation: &dyn Cancellation,
) -> Result<(), OpticsError> {
    let Some(crate::plan::VignettingModel::Pa { k1, k2, k3 }) = plan.vignetting else {
        return Ok(());
    };
    let ns = plan.norm_scale as f32;
    let radius_step = 2.0 * ns;
    let radius_step_squared = ns * ns;
    let start_x = (-plan.center_x) as f32;
    let start_y = (-plan.center_y) as f32;
    let row_stride = image.row_stride();
    let data = image.data_mut();
    data.par_chunks_mut(row_stride).enumerate().try_for_each(
        |(row_index, row)| -> Result<(), OpticsError> {
            if cancellation.is_cancelled() {
                return Err(OpticsError::Cancelled);
            }
            let y = start_y + ns * row_index as f32;
            let mut x = start_x;
            let mut radius_squared = x * x + y * y;
            for pixel in row.as_chunks_mut::<3>().0.iter_mut().take(plan.width) {
                let radius_fourth = radius_squared * radius_squared;
                let radius_sixth = radius_fourth * radius_squared;
                let gain = 1.0 + k1 * radius_squared + k2 * radius_fourth + k3 * radius_sixth;
                if !gain.is_finite() || gain == 0.0 {
                    return Err(OpticsError::Correction {
                        reason: "vignetting calibration produced an invalid gain".to_owned(),
                    });
                }
                let multiplier = 1.0 / gain;
                for value in pixel {
                    *value *= multiplier;
                    if !value.is_finite() {
                        return Err(OpticsError::Correction {
                            reason: "vignetting correction produced a non-finite pixel".to_owned(),
                        });
                    }
                }
                radius_squared += radius_step * x + radius_step_squared;
                x += ns;
            }
            Ok(())
        },
    )
}

/// Cubic sampling core for the radial-LUT branch: identical weights, tap
/// order, accumulation and rounding as `resample::cubic_sample` (bit-exact
/// given the same coordinates — pinned by `radial_sampling_equals_cubic_sample`
/// in this file's tests), with the eight unique tap weights hoisted out of
/// the four-by-four tap loop. That hoist only pays off once the per-pixel
/// Newton solves are gone, so it lives only in this branch; the authority
/// path keeps `resample::cubic_sample` untouched.
fn sample_cubic_hoisted(
    data: &[f32],
    row_stride: usize,
    width: usize,
    height: usize,
    channel: usize,
    x: f32,
    y: f32,
) -> Result<f32, OpticsError> {
    if !x.is_finite() || !y.is_finite() {
        return Err(OpticsError::Correction {
            reason: "non-finite cubic source coordinate".to_owned(),
        });
    }
    let floor_x = x.floor() as isize;
    let floor_y = y.floor() as isize;
    if floor_x - 1 < 0
        || floor_y - 1 < 0
        || floor_x + 2 >= width as isize
        || floor_y + 2 >= height as isize
    {
        return Err(OpticsError::InvalidFootprint);
    }
    let mut weight_x = [0.0_f32; 4];
    for (offset, weight) in weight_x.iter_mut().enumerate() {
        *weight = cubic_weight(x - (floor_x + offset as isize - 1) as f32);
    }
    let base = (floor_x - 1) as usize * 3 + channel;
    let mut value = 0.0_f32;
    for offset_y in -1..=2 {
        let weight_y = cubic_weight(y - (floor_y + offset_y) as f32);
        let row_base = (floor_y + offset_y) as usize * row_stride + base;
        for (offset_x, weight_x) in weight_x.iter().enumerate() {
            value += data[row_base + offset_x * 3] * (weight_y * weight_x);
        }
    }
    if !value.is_finite() {
        return Err(OpticsError::Correction {
            reason: "cubic resampling produced a non-finite pixel".to_owned(),
        });
    }
    Ok(value)
}

#[allow(dead_code)]
fn _requested_components(plan: &LensCorrectionPlan) -> CorrectionComponents {
    plan.provenance().requested
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic xorshift for sampling interior coordinates.
    struct Lcg(u64);

    impl Lcg {
        fn next_f32(&mut self, low: f32, high: f32) -> f32 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            low + (x >> 40) as f32 / (1u64 << 24) as f32 * (high - low)
        }
    }

    fn assert_same_sample(
        data: &[f32],
        row_stride: usize,
        width: usize,
        height: usize,
        channel: usize,
        x: f32,
        y: f32,
    ) {
        let authority = cubic_sample(data, row_stride, width, height, channel, x, y);
        let branch = sample_cubic_hoisted(data, row_stride, width, height, channel, x, y);
        match (authority, branch) {
            (Ok(authority), Ok(branch)) => assert_eq!(
                authority.to_bits(),
                branch.to_bits(),
                "sampler diverged at channel {channel} coordinate ({x}, {y})"
            ),
            (Err(_), Err(_)) => {}
            (authority, branch) => panic!(
                "sampler error behaviour diverged at ({x}, {y}): {authority:?} vs {branch:?}"
            ),
        }
    }

    /// Many random interior coordinates on a deterministic pseudo-random
    /// image of the given size.
    fn assert_grid_parity(width: usize, height: usize, samples: usize) {
        let mut rng = Lcg(0x9E37_79B9_7F4A_7C15 ^ (width as u64) << 32);
        let image = LinearRgbImage::new(
            width,
            height,
            width * 3,
            LinearRgbSpace::CameraNative,
            (0..width * height * 3)
                .map(|index| 0.05 + (index % 977) as f32 * 0.001)
                .collect::<Vec<_>>(),
        )
        .expect("test image");
        let data = image.data();
        for _ in 0..samples {
            let x = rng.next_f32(2.0, width as f32 - 3.0);
            let y = rng.next_f32(2.0, height as f32 - 3.0);
            for channel in 0..3 {
                assert_same_sample(data, image.row_stride(), width, height, channel, x, y);
            }
        }
    }

    #[test]
    fn radial_sampling_equals_cubic_sample() {
        // Small asymmetric image, dense fractional grid over the safe range.
        let width = 48;
        let height = 36;
        let pixels = (0..width * height * 3)
            .map(|index| ((index % 133) as f32 - 66.0) / 64.0)
            .collect::<Vec<_>>();
        for channel in 0..3 {
            let mut y = 1.0;
            while y < height as f32 - 2.0 {
                let mut x = 1.0;
                while x < width as f32 - 2.0 {
                    assert_same_sample(&pixels, width * 3, width, height, channel, x, y);
                    x += 0.125;
                }
                y += 0.25;
            }
        }
        // Out-of-footprint and non-finite coordinates must agree too.
        for (x, y) in [
            (0.5_f32, 4.0_f32),
            (-0.25, 4.0),
            (width as f32 - 2.0, 4.0),
            (4.0, height as f32 - 2.0),
            (f32::NAN, 4.0),
            (4.0, f32::INFINITY),
        ] {
            let authority = cubic_sample(&pixels, width * 3, width, height, 0, x, y);
            let branch = sample_cubic_hoisted(&pixels, width * 3, width, height, 0, x, y);
            match (&authority, &branch) {
                (Err(authority), Err(branch)) => assert_eq!(
                    std::mem::discriminant(authority),
                    std::mem::discriminant(branch),
                    "({x}, {y})"
                ),
                _ => panic!("({x}, {y}) must fail in both samplers"),
            }
        }

        // The parity grid size on a deterministic pseudo-random image.
        assert_grid_parity(1_024, 768, 4_096);
    }

    #[test]
    #[ignore = "allocates a full-size 24 MP image (~290 MB) to sample 16k coordinates; run with --ignored for full-frame hoisting parity"]
    fn radial_sampling_equals_cubic_sample_full_frame() {
        assert_grid_parity(6_048, 4_024, 16_384);
    }
}
