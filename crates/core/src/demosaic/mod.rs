//! Bayer reconstruction algorithms operating on normalized camera samples.
//!
//! Algorithms in this module deliberately know nothing about RAW decoding,
//! editor state, caches, or output codecs. Allocation, input validation, and
//! cancellation contracts are shared here; each implementation only fills
//! camera-native linear RGB rows.

mod bilinear;
mod malvar_he_cutler;

use crate::image::allocate_zeroed_f32;
use crate::{CancellationToken, Halo, LinearRgbImage, LinearRgbSpace, MosaicImage, PipelineError};

/// Neighborhood required to reconstruct an MHC output region.
pub const MALVAR_HE_CUTLER_HALO: Halo = Halo {
    left: 2,
    right: 2,
    top: 2,
    bottom: 2,
};

/// Selectable CPU Bayer reconstruction algorithms.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DemosaicAlgorithm {
    /// Fast reference interpolation using nearest same-color neighbors.
    #[default]
    Bilinear,
    /// Malvar-He-Cutler gradient-corrected linear interpolation.
    MalvarHeCutler,
}

impl DemosaicAlgorithm {
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Bilinear => "bilinear",
            Self::MalvarHeCutler => "mhc",
        }
    }
}

/// Effective multipliers applied to reconstructed camera-native R, G, and B.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WhiteBalanceGains {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
}

impl WhiteBalanceGains {
    #[must_use]
    pub const fn identity() -> Self {
        Self {
            red: 1.0,
            green: 1.0,
            blue: 1.0,
        }
    }

    pub(crate) fn apply(self, rgb: &mut [f32; 3]) {
        rgb[0] *= self.red;
        rgb[1] *= self.green;
        rgb[2] *= self.blue;
    }

    pub(crate) fn validate(self) -> Result<(), PipelineError> {
        if [self.red, self.green, self.blue]
            .into_iter()
            .all(|value| value.is_finite() && value > 0.0)
        {
            Ok(())
        } else {
            Err(PipelineError::InvalidMetadata {
                field: "as_shot_white_balance",
                reason: "effective R, G, and B gains must be finite and positive".to_owned(),
            })
        }
    }
}

/// Reconstruct a normalized Bayer mosaic into camera-native linear RGB.
///
/// White-balance gains are applied only after all three channels have been
/// reconstructed. Values are not clipped, so negative filter lobes and
/// over-range highlights remain available to later pipeline stages.
pub fn demosaic(
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
    algorithm: DemosaicAlgorithm,
) -> Result<LinearRgbImage<f32>, PipelineError> {
    demosaic_cancellable(mosaic, gains, algorithm, &CancellationToken::new())
}

pub(crate) fn demosaic_cancellable(
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
    algorithm: DemosaicAlgorithm,
    cancellation: &CancellationToken,
) -> Result<LinearRgbImage<f32>, PipelineError> {
    let span = tracing::info_span!(
        "cpu.demosaic",
        width = mosaic.width(),
        height = mosaic.height(),
        algorithm = ?algorithm
    );
    let _guard = span.enter();
    cancellation.checkpoint()?;
    gains.validate()?;
    validate_mosaic(mosaic, cancellation)?;

    let row_stride = mosaic
        .width()
        .checked_mul(3)
        .ok_or_else(|| invalid_dimensions(mosaic, 0, "RGB stride overflowed"))?;
    let elements = row_stride
        .checked_mul(mosaic.height())
        .ok_or_else(|| invalid_dimensions(mosaic, row_stride, "RGB sample count overflowed"))?;
    let mut output = allocate_zeroed_f32(elements)?;
    match algorithm {
        DemosaicAlgorithm::Bilinear => {
            bilinear::reconstruct(mosaic, gains, cancellation, row_stride, &mut output)?;
        }
        DemosaicAlgorithm::MalvarHeCutler => {
            malvar_he_cutler::reconstruct(mosaic, gains, cancellation, row_stride, &mut output)?
        }
    }
    cancellation.checkpoint()?;
    LinearRgbImage::new(
        mosaic.width(),
        mosaic.height(),
        row_stride,
        LinearRgbSpace::CameraNative,
        output,
    )
}

fn validate_mosaic(
    mosaic: &MosaicImage<f32>,
    cancellation: &CancellationToken,
) -> Result<(), PipelineError> {
    if mosaic.width() < 2 || mosaic.height() < 2 {
        return Err(invalid_dimensions(
            mosaic,
            mosaic.row_stride(),
            "Bayer demosaicing requires at least 2x2 samples",
        ));
    }
    for y in 0..mosaic.height() {
        cancellation.checkpoint()?;
        for x in 0..mosaic.width() {
            if !mosaic.sample(x, y).is_finite() {
                return Err(PipelineError::NonFiniteImageData {
                    stage: "demosaicing",
                    x,
                    y,
                });
            }
        }
    }
    Ok(())
}

fn invalid_dimensions(mosaic: &MosaicImage<f32>, row_stride: usize, reason: &str) -> PipelineError {
    PipelineError::InvalidDimensions {
        width: mosaic.width(),
        height: mosaic.height(),
        row_stride,
        reason: reason.to_owned(),
    }
}

fn require_finite_output(rgb: &[f32; 3], x: usize, y: usize) -> Result<(), PipelineError> {
    if rgb.iter().all(|value| value.is_finite()) {
        Ok(())
    } else {
        Err(PipelineError::NonFiniteImageData {
            stage: "demosaicing output",
            x,
            y,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BayerPattern;

    #[test]
    fn mhc_honors_cancellation_before_starting_rows() {
        let mosaic =
            MosaicImage::new(8, 8, 8, BayerPattern::Rggb, vec![0.5; 64]).expect("valid mosaic");
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = demosaic_cancellable(
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::MalvarHeCutler,
            &cancellation,
        )
        .expect_err("cancelled reconstruction must stop");
        assert!(matches!(error, PipelineError::Cancelled));
    }
}
