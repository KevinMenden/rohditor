//! RAW-domain highlight handling for normalized Bayer mosaics.
//!
//! This crate deliberately knows nothing about RAW metadata, edit recipes, or
//! UI state. It receives already-normalized camera samples and validated
//! channel levels, then applies the selected RAW-stage operation in place.

mod cells;
mod clip;
mod detect;
mod local_ratios;
mod opposed;

pub use clip::{ClipOutput, clip, clip_cancellable};
pub use detect::{
    detect_clipping, detect_clipping_cancellable, detect_local_ratios,
    detect_local_ratios_cancellable, detect_opposed, detect_opposed_cancellable,
};
pub use local_ratios::{
    local_ratio_scratch_bytes, reconstruct_local_ratios, reconstruct_local_ratios_cancellable,
};
pub use opposed::{opposed_scratch_bytes, reconstruct_opposed, reconstruct_opposed_cancellable};

/// Version of the deterministic Local ratios estimator used in recipe/cache
/// identities. Numerical contract changes must increment this value.
pub const LOCAL_RATIOS_ALGORITHM_VERSION: u8 = 1;

/// Version of the deterministic Opposed estimator used in recipe/cache
/// identities. Numerical contract changes must increment this value.
pub const OPPOSED_ALGORITHM_VERSION: u8 = 1;

use rohditor_image::ImageError;
use thiserror::Error;

/// Effective pre-white-balance ceilings for normalized red, green, and blue
/// CFA sites.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelClipLevels {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
}

impl ChannelClipLevels {
    #[must_use]
    pub const fn for_color(self, color: rohditor_image::CfaColor) -> f32 {
        match color {
            rohditor_image::CfaColor::Red => self.red,
            rohditor_image::CfaColor::Green => self.green,
            rohditor_image::CfaColor::Blue => self.blue,
        }
    }

    pub(crate) fn validate(self) -> Result<(), HighlightError> {
        for (channel, value) in [
            ("red", self.red),
            ("green", self.green),
            ("blue", self.blue),
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(HighlightError::InvalidLevel { channel, value });
            }
        }
        Ok(())
    }
}

/// Camera-native thresholds used to classify samples that may have lost
/// highlight information. These levels intentionally do not contain active
/// white-balance gains; Local ratios and Opposed run before white balance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelDetectionLevels {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
}

impl ChannelDetectionLevels {
    #[must_use]
    pub const fn for_color(self, color: rohditor_image::CfaColor) -> f32 {
        match color {
            rohditor_image::CfaColor::Red => self.red,
            rohditor_image::CfaColor::Green => self.green,
            rohditor_image::CfaColor::Blue => self.blue,
        }
    }

    pub(crate) fn validate(self) -> Result<(), HighlightError> {
        for (channel, value) in [
            ("red", self.red),
            ("green", self.green),
            ("blue", self.blue),
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(HighlightError::InvalidLevel { channel, value });
            }
        }
        Ok(())
    }

    pub(crate) fn for_channel(self, channel: usize) -> f32 {
        match channel {
            0 => self.red,
            1 => self.green,
            2 => self.blue,
            _ => unreachable!("Bayer channel index is always in range"),
        }
    }
}

/// Configuration for version-one local-ratio reconstruction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalRatioOptions {
    pub detection_levels: ChannelDetectionLevels,
}

/// Counts produced by Local ratios reconstruction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconstructionStats {
    pub suspected_clipped_sites: usize,
    pub reconstructed_sites: usize,
    pub changed_sites: usize,
    pub fallback_sites: usize,
    pub fully_unsupported_sites: usize,
    pub suspected_by_channel: [usize; 3],
}

impl ReconstructionStats {
    pub(crate) fn add_assign(&mut self, other: Self) {
        self.suspected_clipped_sites += other.suspected_clipped_sites;
        self.reconstructed_sites += other.reconstructed_sites;
        self.changed_sites += other.changed_sites;
        self.fallback_sites += other.fallback_sites;
        self.fully_unsupported_sites += other.fully_unsupported_sites;
        for (target, source) in self
            .suspected_by_channel
            .iter_mut()
            .zip(other.suspected_by_channel)
        {
            *target += source;
        }
    }
}

/// Output of Local ratios reconstruction.
#[derive(Debug, PartialEq)]
pub struct LocalRatioOutput {
    pub mosaic: rohditor_image::MosaicImage<f32>,
    pub stats: ReconstructionStats,
}

/// Configuration for version-one Opposed / local-inpainting reconstruction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpposedOptions {
    pub detection_levels: ChannelDetectionLevels,
}

/// Counts produced by Opposed / local-inpainting reconstruction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpposedStats {
    pub suspected_clipped_sites: usize,
    pub reconstructed_sites: usize,
    pub changed_sites: usize,
    pub fallback_sites: usize,
    pub fully_unsupported_sites: usize,
    pub suspected_by_channel: [usize; 3],
}

impl OpposedStats {
    pub(crate) fn add_assign(&mut self, other: Self) {
        self.suspected_clipped_sites += other.suspected_clipped_sites;
        self.reconstructed_sites += other.reconstructed_sites;
        self.changed_sites += other.changed_sites;
        self.fallback_sites += other.fallback_sites;
        self.fully_unsupported_sites += other.fully_unsupported_sites;
        for (target, source) in self
            .suspected_by_channel
            .iter_mut()
            .zip(other.suspected_by_channel)
        {
            *target += source;
        }
    }
}

/// Output of Opposed / local-inpainting reconstruction.
#[derive(Debug, PartialEq)]
pub struct OpposedOutput {
    pub mosaic: rohditor_image::MosaicImage<f32>,
    pub stats: OpposedStats,
}

/// Counts produced by the fused Clip pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClipStats {
    pub affected_sites: usize,
    pub changed_sites: usize,
    pub nominal_over_white_sites: usize,
    pub affected_by_channel: [usize; 3],
}

impl ClipStats {
    pub(crate) fn add_assign(&mut self, other: Self) {
        self.affected_sites += other.affected_sites;
        self.changed_sites += other.changed_sites;
        self.nominal_over_white_sites += other.nominal_over_white_sites;
        for (target, source) in self
            .affected_by_channel
            .iter_mut()
            .zip(other.affected_by_channel)
        {
            *target += source;
        }
    }
}

/// A materialized affected-site mask for diagnostics and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClippingMask {
    width: usize,
    height: usize,
    row_stride: usize,
    data: Vec<bool>,
}

impl ClippingMask {
    pub(crate) fn new(width: usize, height: usize, row_stride: usize, data: Vec<bool>) -> Self {
        Self {
            width,
            height,
            row_stride,
            data,
        }
    }

    #[must_use]
    pub const fn width(&self) -> usize {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> usize {
        self.height
    }

    #[must_use]
    pub const fn row_stride(&self) -> usize {
        self.row_stride
    }

    #[must_use]
    pub fn data(&self) -> &[bool] {
        &self.data
    }

    #[must_use]
    pub fn get(&self, x: usize, y: usize) -> Option<bool> {
        (x < self.width && y < self.height).then(|| self.data[y * self.row_stride + x])
    }
}

/// Cancellation contract shared with callers that run highlight processing in
/// a background worker.
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

/// Errors returned before or during normalized-mosaic highlight processing.
#[derive(Debug, Error, PartialEq)]
pub enum HighlightError {
    #[error("highlight processing was cancelled")]
    Cancelled,

    #[error("invalid {channel} highlight level {value}; levels must be finite and positive")]
    InvalidLevel { channel: &'static str, value: f32 },

    #[error("highlight processing received a non-finite sample at ({x}, {y})")]
    NonFiniteSample { x: usize, y: usize },

    #[error(transparent)]
    Image(#[from] ImageError),
}

pub(crate) fn checkpoint(cancellation: &dyn CancellationCheck) -> Result<(), HighlightError> {
    if cancellation.is_cancelled() {
        Err(HighlightError::Cancelled)
    } else {
        Ok(())
    }
}
