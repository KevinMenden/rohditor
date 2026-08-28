use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::{EncodedPreview, RawFileInfo, RawFrame};

/// Hard limits checked before allocating a decoded sensor buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderLimits {
    pub max_width: usize,
    pub max_height: usize,
    pub max_samples: usize,
}

impl Default for DecoderLimits {
    fn default() -> Self {
        Self {
            max_width: 100_000,
            max_height: 100_000,
            max_samples: 300_000_000,
        }
    }
}

/// Errors normalized at Rohditor's decoder boundary.
#[derive(Debug, Error)]
pub enum RawError {
    #[error("cannot read RAW file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("unsupported RAW file {path}: {reason}")]
    Unsupported { path: PathBuf, reason: String },

    #[error("failed to decode RAW file {path}: {reason}")]
    Decode { path: PathBuf, reason: String },

    #[error("corrupt or incomplete RAW file {path}: {reason}")]
    Corrupt { path: PathBuf, reason: String },

    #[error(
        "refusing RAW dimensions {width}x{height} with {components} component(s) in {path}; configured maximum is {max_width}x{max_height} and {max_samples} samples"
    )]
    InvalidDimensions {
        path: PathBuf,
        width: usize,
        height: usize,
        components: usize,
        max_width: usize,
        max_height: usize,
        max_samples: usize,
    },

    #[error(
        "decoder returned {actual} samples for {path}, but dimensions require exactly {expected}"
    )]
    InvalidSampleCount {
        path: PathBuf,
        expected: usize,
        actual: usize,
    },

    #[error("RAW file {path} contains floating-point sensor data, which is not supported yet")]
    UnsupportedPixelData { path: PathBuf },

    #[error("decoder panicked while {operation} {path}: {reason}")]
    DecoderPanic {
        path: PathBuf,
        operation: &'static str,
        reason: String,
    },
}

/// Camera decoder behavior used by the rest of Rohditor.
pub trait RawDecoder: Send + Sync {
    /// Read normalized metadata without allocating the full sensor buffer.
    fn probe(&self, path: &Path) -> Result<RawFileInfo, RawError>;

    /// Decode and validate the complete sensor buffer.
    fn decode(&self, path: &Path) -> Result<RawFrame, RawError>;

    /// Extract the embedded loading preview when the file contains one.
    fn embedded_preview(&self, path: &Path) -> Result<Option<EncodedPreview>, RawError>;
}
