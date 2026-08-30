use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::{EncodedPreview, RawFileInfo, RawFrame};

/// Hard limits checked before allocating a decoded sensor buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderLimits {
    pub max_source_bytes: u64,
    pub max_width: usize,
    pub max_height: usize,
    pub max_samples: usize,
    pub max_preview_width: usize,
    pub max_preview_height: usize,
    pub max_preview_pixels: usize,
    pub max_preview_bytes: usize,
}

impl Default for DecoderLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 16 * 1_024 * 1_024 * 1_024,
            max_width: 100_000,
            max_height: 100_000,
            max_samples: 300_000_000,
            max_preview_width: 32_768,
            max_preview_height: 32_768,
            max_preview_pixels: 100_000_000,
            max_preview_bytes: 256 * 1_024 * 1_024,
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

    #[error(
        "refusing {actual_bytes}-byte RAW file {path}; configured maximum is {max_bytes} bytes"
    )]
    SourceTooLarge {
        path: PathBuf,
        actual_bytes: u64,
        max_bytes: u64,
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
        "refusing embedded preview dimensions {width}x{height} in {path}; configured maximum is {max_width}x{max_height} and {max_pixels} pixels"
    )]
    InvalidPreviewDimensions {
        path: PathBuf,
        width: usize,
        height: usize,
        max_width: usize,
        max_height: usize,
        max_pixels: usize,
    },

    #[error(
        "refusing {actual_bytes}-byte embedded preview in {path}; configured maximum is {max_bytes} bytes"
    )]
    PreviewTooLarge {
        path: PathBuf,
        actual_bytes: usize,
        max_bytes: usize,
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

/// Operations on one coherently opened RAW source.
///
/// A session lets clients inspect metadata, request the optional loading
/// preview, and decode sensor data without reopening or remapping the file for
/// every operation.
pub trait RawSession: Send {
    /// Read normalized metadata without allocating the full sensor buffer.
    ///
    /// # Errors
    ///
    /// Returns [`RawError`] when the source is unsupported, corrupt, exceeds
    /// configured limits, or cannot be read.
    fn probe(&mut self) -> Result<RawFileInfo, RawError>;

    /// Decode and validate the complete sensor buffer.
    ///
    /// # Errors
    ///
    /// Returns [`RawError`] when metadata or pixel decoding fails, the source
    /// exceeds configured limits, or the decoded layout is inconsistent.
    fn decode(&mut self) -> Result<RawFrame, RawError>;

    /// Extract the embedded loading preview when the file contains one.
    ///
    /// # Errors
    ///
    /// Returns [`RawError`] when an advertised preview is corrupt, exceeds
    /// configured limits, or cannot be read.
    fn embedded_preview(&mut self) -> Result<Option<EncodedPreview>, RawError>;
}

/// Camera decoder behavior used by the rest of Rohditor.
pub trait RawDecoder: Send + Sync {
    /// Open one coherent view of a RAW source for one or more operations.
    ///
    /// # Errors
    ///
    /// Returns [`RawError`] when the source cannot be inspected or opened by a
    /// supported decoder.
    fn open(&self, path: &Path) -> Result<Box<dyn RawSession>, RawError>;

    /// Read normalized metadata without allocating the full sensor buffer.
    ///
    /// # Errors
    ///
    /// Returns any source-opening or metadata error from the decoder session.
    fn probe(&self, path: &Path) -> Result<RawFileInfo, RawError> {
        self.open(path)?.probe()
    }

    /// Decode and validate the complete sensor buffer.
    ///
    /// # Errors
    ///
    /// Returns any source-opening, metadata, or pixel error from the decoder
    /// session.
    fn decode(&self, path: &Path) -> Result<RawFrame, RawError> {
        self.open(path)?.decode()
    }

    /// Extract the embedded loading preview when the file contains one.
    ///
    /// # Errors
    ///
    /// Returns any source-opening or embedded-preview error from the decoder
    /// session.
    fn embedded_preview(&self, path: &Path) -> Result<Option<EncodedPreview>, RawError> {
        self.open(path)?.embedded_preview()
    }
}
