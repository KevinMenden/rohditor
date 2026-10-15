//! Dev-only harness that scores Rohditor's demosaicing against ground truth
//! and against RawTherapee/darktable. Nothing else may depend on this crate.

pub mod dng;
pub mod external;
pub mod fixtures;
pub mod metrics;
pub mod tiff_read;
pub mod truth;

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ReferenceError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid image: {0}")]
    Image(#[from] rohditor_image::ImageError),
    #[error("raw decode failed: {0}")]
    Raw(#[from] rohditor_raw::RawError),
    #[error("TIFF decode failed: {0}")]
    Tiff(#[from] tiff::TiffError),
    #[error("{0}")]
    Other(String),
}

/// Scratch directory for generated inputs and tool outputs (gitignored).
#[must_use]
pub fn output_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/generated/reference")
}
