use rayon::prelude::*;
use rohditor_image::{LinearRgbImage, LinearRgbSpace, allocate_zeroed_f32};

use crate::plan::map_coordinates;
use crate::resample::cubic_sample;
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
        mut image: LinearRgbImage<f32>,
        cancellation: &dyn Cancellation,
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

        if self.applied.vignetting {
            apply_vignetting(&mut image, &self, cancellation)?;
        }
        if self.applied.distortion || self.applied.chromatic_aberration {
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
            output
                .data_mut()
                .par_chunks_mut(output_row_stride)
                .enumerate()
                .try_for_each(|(y, row)| -> Result<(), OpticsError> {
                    if cancellation.is_cancelled() {
                        return Err(OpticsError::Cancelled);
                    }
                    for x in 0..self.width {
                        let coordinates = map_coordinates(&self, x, y)?;
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
            image = output;
        }

        let output_bytes = if self.applied.distortion || self.applied.chromatic_aberration {
            image
                .data()
                .len()
                .saturating_mul(std::mem::size_of::<f32>())
        } else {
            0
        };
        let scratch_bytes = if self.applied.distortion || self.applied.chromatic_aberration {
            self.width
                .checked_mul(6)
                .and_then(|elements| elements.checked_mul(std::mem::size_of::<f32>()))
                .unwrap_or(usize::MAX)
        } else {
            0
        };
        Ok(CorrectionResult {
            image,
            provenance: self.provenance(),
            output_bytes,
            scratch_bytes,
        })
    }
}

fn apply_vignetting(
    image: &mut LinearRgbImage<f32>,
    plan: &LensCorrectionPlan,
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
            for pixel in row.chunks_exact_mut(3).take(plan.width) {
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

#[allow(dead_code)]
fn _requested_components(plan: &LensCorrectionPlan) -> CorrectionComponents {
    plan.provenance().requested
}
