//! Bayer reconstruction algorithms operating on normalized camera samples.
//!
//! Algorithms in this module deliberately know nothing about RAW decoding,
//! editor state, caches, or output codecs. Allocation, input validation, and
//! cancellation contracts are shared here; each implementation only fills
//! camera-native linear RGB rows.

mod amaze;
mod amaze_stages;
mod bilinear;
mod malvar_he_cutler;
mod rcd;
mod rcd_geometry;
mod rcd_stages;

pub use rcd_geometry::{
    RCD_BORDER, RCD_MIN_DIMENSION, RCD_TILE_SIZE, RCD_TILE_STEP, RcdTileGeometry, rcd_tiles,
};

use rohditor_image::{
    Halo, ImageError, LinearRgbImage, LinearRgbSpace, MosaicImage, allocate_zeroed_f32,
};
use thiserror::Error;

/// Failures specific to Bayer reconstruction.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DemosaicError {
    #[error("demosaicing was cancelled")]
    Cancelled,

    #[error("invalid white-balance gains: {reason}")]
    InvalidGains { reason: &'static str },

    #[error("invalid image dimensions {width}x{height} with row stride {row_stride}: {reason}")]
    InvalidDimensions {
        width: usize,
        height: usize,
        row_stride: usize,
        reason: String,
    },

    #[error("{stage} received a non-finite sample at ({x}, {y})")]
    NonFiniteImageData {
        stage: &'static str,
        x: usize,
        y: usize,
    },

    #[error(transparent)]
    Image(#[from] ImageError),
}

/// Minimal cancellation contract accepted by reconstruction algorithms.
pub trait CancellationCheck: Sync {
    fn is_cancelled(&self) -> bool;
}

impl<F> CancellationCheck for F
where
    F: Fn() -> bool + Sync,
{
    fn is_cancelled(&self) -> bool {
        self()
    }
}

/// Neighborhood required to reconstruct a bilinear output region.
pub const BILINEAR_HALO: Halo = Halo {
    left: 1,
    right: 1,
    top: 1,
    bottom: 1,
};

/// Neighborhood required to reconstruct an MHC output region.
pub const MALVAR_HE_CUTLER_HALO: Halo = Halo {
    left: 2,
    right: 2,
    top: 2,
    bottom: 2,
};

/// Neighborhood required to reconstruct an RCD output region.
pub const RCD_HALO: Halo = Halo {
    left: RCD_BORDER,
    right: RCD_BORDER,
    top: RCD_BORDER,
    bottom: RCD_BORDER,
};

/// Neighborhood required to reconstruct an AMaZE output region.
pub const AMAZE_HALO: Halo = Halo {
    left: 16,
    right: 16,
    top: 16,
    bottom: 16,
};

/// Selectable CPU Bayer reconstruction algorithms.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DemosaicAlgorithm {
    /// Fast reference interpolation using nearest same-color neighbors.
    Bilinear,
    /// Malvar-He-Cutler gradient-corrected linear interpolation.
    #[default]
    MalvarHeCutler,
    /// Ratio-corrected directional interpolation.
    Rcd,
    /// Aliasing Minimization and Zipper Elimination interpolation.
    Amaze,
}

impl DemosaicAlgorithm {
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Bilinear => "bilinear",
            Self::MalvarHeCutler => "mhc",
            Self::Rcd => "rcd",
            Self::Amaze => "amaze",
        }
    }

    /// Resolve this algorithm's spatial requirements and deterministic edge
    /// behavior for a backend executor.
    #[must_use]
    pub const fn contract(self) -> DemosaicContract {
        DemosaicContract::for_algorithm(self)
    }
}

/// Deterministic policy for pixels without a complete algorithm neighborhood.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemosaicBorderPolicy {
    /// Use the bilinear reference reconstruction at image and incomplete-tile
    /// borders. This is also the complete Bilinear algorithm policy.
    BilinearReference,
}

/// Pixel-storage-independent execution facts for a selectable demosaic method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemosaicContract {
    algorithm: DemosaicAlgorithm,
    required_halo: Halo,
    border_policy: DemosaicBorderPolicy,
    algorithm_version: u8,
}

impl DemosaicContract {
    #[must_use]
    pub const fn for_algorithm(algorithm: DemosaicAlgorithm) -> Self {
        let required_halo = match algorithm {
            DemosaicAlgorithm::Bilinear => BILINEAR_HALO,
            DemosaicAlgorithm::MalvarHeCutler => MALVAR_HE_CUTLER_HALO,
            DemosaicAlgorithm::Rcd => RCD_HALO,
            DemosaicAlgorithm::Amaze => AMAZE_HALO,
        };
        Self {
            algorithm,
            required_halo,
            border_policy: DemosaicBorderPolicy::BilinearReference,
            algorithm_version: 1,
        }
    }

    #[must_use]
    pub const fn algorithm(self) -> DemosaicAlgorithm {
        self.algorithm
    }

    #[must_use]
    pub const fn required_halo(self) -> Halo {
        self.required_halo
    }

    #[must_use]
    pub const fn border_policy(self) -> DemosaicBorderPolicy {
        self.border_policy
    }

    /// Increment when this algorithm's numerical result changes.
    #[must_use]
    pub const fn algorithm_version(self) -> u8 {
        self.algorithm_version
    }

    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        self.algorithm.stable_name()
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

    fn apply(self, rgb: &mut [f32; 3]) {
        rgb[0] *= self.red;
        rgb[1] *= self.green;
        rgb[2] *= self.blue;
    }

    pub fn validate(self) -> Result<(), DemosaicError> {
        if [self.red, self.green, self.blue]
            .into_iter()
            .all(|value| value.is_finite() && value > 0.0)
        {
            Ok(())
        } else {
            Err(DemosaicError::InvalidGains {
                reason: "effective R, G, and B gains must be finite and positive",
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
) -> Result<LinearRgbImage<f32>, DemosaicError> {
    demosaic_cancellable(mosaic, gains, algorithm, &|| false)
}

pub fn demosaic_cancellable(
    mosaic: &MosaicImage<f32>,
    gains: WhiteBalanceGains,
    algorithm: DemosaicAlgorithm,
    cancellation: &dyn CancellationCheck,
) -> Result<LinearRgbImage<f32>, DemosaicError> {
    checkpoint(cancellation)?;
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
        DemosaicAlgorithm::Rcd => {
            rcd::reconstruct(mosaic, gains, cancellation, row_stride, &mut output)?
        }
        DemosaicAlgorithm::Amaze => {
            amaze::reconstruct(mosaic, gains, cancellation, row_stride, &mut output)?
        }
    }
    checkpoint(cancellation)?;
    LinearRgbImage::new(
        mosaic.width(),
        mosaic.height(),
        row_stride,
        LinearRgbSpace::CameraNative,
        output,
    )
    .map_err(Into::into)
}

fn validate_mosaic(
    mosaic: &MosaicImage<f32>,
    cancellation: &dyn CancellationCheck,
) -> Result<(), DemosaicError> {
    if mosaic.width() < 2 || mosaic.height() < 2 {
        return Err(invalid_dimensions(
            mosaic,
            mosaic.row_stride(),
            "Bayer demosaicing requires at least 2x2 samples",
        ));
    }
    for y in 0..mosaic.height() {
        checkpoint(cancellation)?;
        for x in 0..mosaic.width() {
            if !mosaic.sample(x, y).is_finite() {
                return Err(DemosaicError::NonFiniteImageData {
                    stage: "demosaicing",
                    x,
                    y,
                });
            }
        }
    }
    Ok(())
}

fn invalid_dimensions(mosaic: &MosaicImage<f32>, row_stride: usize, reason: &str) -> DemosaicError {
    DemosaicError::InvalidDimensions {
        width: mosaic.width(),
        height: mosaic.height(),
        row_stride,
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    #[test]
    fn contracts_expose_each_algorithm_halo_and_common_edge_policy() {
        for (algorithm, halo) in [
            (DemosaicAlgorithm::Bilinear, 1),
            (DemosaicAlgorithm::MalvarHeCutler, 2),
            (DemosaicAlgorithm::Rcd, 10),
            (DemosaicAlgorithm::Amaze, 16),
        ] {
            let contract = algorithm.contract();
            assert_eq!(contract.algorithm(), algorithm);
            assert_eq!(contract.required_halo().left, halo);
            assert_eq!(contract.required_halo().right, halo);
            assert_eq!(contract.required_halo().top, halo);
            assert_eq!(contract.required_halo().bottom, halo);
            assert_eq!(
                contract.border_policy(),
                DemosaicBorderPolicy::BilinearReference
            );
            assert_eq!(contract.stable_name(), algorithm.stable_name());
            assert_eq!(contract.algorithm_version(), 1);
        }
    }
}

fn require_finite_output(rgb: &[f32; 3], x: usize, y: usize) -> Result<(), DemosaicError> {
    if rgb.iter().all(|value| value.is_finite()) {
        Ok(())
    } else {
        Err(DemosaicError::NonFiniteImageData {
            stage: "demosaicing output",
            x,
            y,
        })
    }
}

fn checkpoint(cancellation: &dyn CancellationCheck) -> Result<(), DemosaicError> {
    if cancellation.is_cancelled() {
        Err(DemosaicError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rohditor_image::BayerPattern;

    #[test]
    fn mhc_honors_cancellation_before_starting_rows() {
        let mosaic =
            MosaicImage::new(8, 8, 8, BayerPattern::Rggb, vec![0.5; 64]).expect("valid mosaic");
        let cancellation = || true;
        let error = demosaic_cancellable(
            &mosaic,
            WhiteBalanceGains::identity(),
            DemosaicAlgorithm::MalvarHeCutler,
            &cancellation,
        )
        .expect_err("cancelled reconstruction must stop");
        assert!(matches!(error, DemosaicError::Cancelled));
    }
}
