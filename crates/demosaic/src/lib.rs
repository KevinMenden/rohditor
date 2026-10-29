//! Bayer reconstruction algorithms operating on normalized camera samples.
//!
//! Algorithms in this module deliberately know nothing about RAW decoding,
//! editor state, caches, or output codecs. Allocation, input validation, and
//! cancellation contracts are shared here; each implementation only fills
//! camera-native linear RGB rows.

mod amaze;
mod amaze_geometry;
mod balance;
mod bilinear;
mod malvar_he_cutler;
mod rcd;
mod rcd_geometry;
mod rcd_stages;

pub use amaze_geometry::{
    AMAZE_BORDER, AMAZE_TILE_SIZE, AMAZE_TILE_VALID, AmazeTileGeometry, AmazeWriteWindow,
    amaze_tile_count, amaze_tiles, mirror_coordinate,
};
pub use balance::ChannelBalance;
pub use rcd_geometry::{
    RCD_BORDER, RCD_MIN_DIMENSION, RCD_TILE_SIZE, RCD_TILE_STEP, RcdTileGeometry, rcd_tiles,
};

use rayon::prelude::*;
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
    left: AMAZE_BORDER,
    right: AMAZE_BORDER,
    top: AMAZE_BORDER,
    bottom: AMAZE_BORDER,
};

/// Selectable CPU Bayer reconstruction algorithms.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DemosaicAlgorithm {
    /// Fast reference interpolation using nearest same-color neighbors.
    Bilinear,
    /// Malvar-He-Cutler gradient-corrected linear interpolation.
    MalvarHeCutler,
    /// Ratio-corrected directional interpolation.
    Rcd,
    /// Aliasing Minimization and Zipper Elimination interpolation.
    #[default]
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
        // RCD and AMaZE version 2 reconstruct on a channel-balanced mosaic. RCD
        // version 3 also matches the reference tools' low-pass neighbours;
        // AMaZE version 3 keeps the whole 16 px border ring bilinear.
        let algorithm_version = match algorithm {
            DemosaicAlgorithm::Rcd => 3,
            DemosaicAlgorithm::Amaze => 3,
            DemosaicAlgorithm::Bilinear | DemosaicAlgorithm::MalvarHeCutler => 1,
        };
        Self {
            algorithm,
            required_halo,
            border_policy: DemosaicBorderPolicy::BilinearReference,
            algorithm_version,
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

/// White-balance multipliers for camera-native R, G, and B. Their overall
/// scale does not matter; only the ratios between channels are used.
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
/// `balance` is the white balance the image will be viewed with (normally the
/// as-shot gains). RCD and AMaZE reconstruct on a mosaic scaled by it, as
/// RawTherapee and darktable do, because unbalanced channels degrade their
/// direction and colour-difference decisions (see `balance.rs`). The scaling
/// is undone afterwards, so the result is camera-native regardless of
/// `balance` and white balance remains a separate later stage. Bilinear and
/// Malvar-He-Cutler are linear and ignore it. Values are not clipped, so
/// negative filter lobes and over-range highlights remain available to later
/// pipeline stages.
pub fn demosaic(
    mosaic: &MosaicImage<f32>,
    balance: WhiteBalanceGains,
    algorithm: DemosaicAlgorithm,
) -> Result<LinearRgbImage<f32>, DemosaicError> {
    demosaic_cancellable(mosaic, balance, algorithm, &|| false)
}

pub fn demosaic_cancellable(
    mosaic: &MosaicImage<f32>,
    balance: WhiteBalanceGains,
    algorithm: DemosaicAlgorithm,
    cancellation: &dyn CancellationCheck,
) -> Result<LinearRgbImage<f32>, DemosaicError> {
    checkpoint(cancellation)?;
    balance.validate()?;
    validate_mosaic(mosaic, cancellation)?;

    // Only the gradient/ratio based algorithms benefit from balanced input.
    let balance = ChannelBalance::new(balance);
    let balanced = match algorithm {
        DemosaicAlgorithm::Rcd | DemosaicAlgorithm::Amaze if !balance.is_identity() => {
            Some(balance.scale_mosaic(mosaic, cancellation)?)
        }
        _ => None,
    };
    let (mosaic, gains, clip_point) = match &balanced {
        // The algorithms apply `gains` to their output, which undoes the scaling.
        Some(scaled) => {
            let [red, green, blue] = balance.inverse();
            (
                scaled,
                WhiteBalanceGains { red, green, blue },
                balance.clip_point(),
            )
        }
        None => (mosaic, WhiteBalanceGains::identity(), 1.0),
    };

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
        DemosaicAlgorithm::Amaze => amaze::reconstruct(
            mosaic,
            gains,
            clip_point,
            cancellation,
            row_stride,
            &mut output,
        )?,
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
    // Rows are checked in parallel; the first offending sample in image order
    // is reported regardless of the thread count.
    let width = mosaic.width();
    let bad = mosaic
        .data()
        .par_chunks(mosaic.row_stride())
        .enumerate()
        .find_map_first(|(y, row)| {
            if cancellation.is_cancelled() {
                return None;
            }
            row[..width]
                .iter()
                .position(|value| !value.is_finite())
                .map(|x| (x, y))
        });
    checkpoint(cancellation)?;
    match bad {
        Some((x, y)) => Err(DemosaicError::NonFiniteImageData {
            stage: "demosaicing",
            x,
            y,
        }),
        None => Ok(()),
    }
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
            // AMaZE is at version 3 (channel-balanced, whole border ring
            // bilinear), RCD at 3 (channel-balanced, reference low-pass).
            let expected_version = match algorithm {
                DemosaicAlgorithm::Rcd => 3,
                DemosaicAlgorithm::Amaze => 3,
                _ => 1,
            };
            assert_eq!(contract.algorithm_version(), expected_version);
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
