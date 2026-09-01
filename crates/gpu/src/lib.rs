//! GPU preview processing built on the application's existing `wgpu` device.
//!
//! This crate owns only interactive preview work. RAW decoding, normalization,
//! demosaicing, and calibration remain in `rohditor-core`'s CPU reference path.
//! The desktop path uploads one camera-native
//! [`rohditor_core::ReconstructedPreview`] and applies white balance, the
//! camera transform, exposure, contrast, saturation, orientation, and the
//! explicit sRGB output transform as GPU parameters. A legacy converted
//! [`rohditor_core::DemosaicedBase`] upload remains available for lower-level
//! callers. Normal interaction never reads the display result back to CPU
//! memory.

mod capabilities;
mod preview;

pub use capabilities::GpuCapabilities;
pub use preview::{
    GpuDisplayReadback, GpuDisplayReadbackPending, GpuPreviewFrame, GpuPreviewProcessor,
    GpuPreviewSource, GpuPreviewUpload,
};

use thiserror::Error;

/// Failure while creating, uploading, or rendering a GPU preview.
#[derive(Debug, Error)]
pub enum GpuPreviewError {
    /// CPU-side preparation was superseded before an upload was submitted.
    #[error("GPU preview preparation was cancelled by a newer preview")]
    Cancelled,

    /// The eframe-created device cannot perform the required texture operations.
    #[error("the selected wgpu device cannot support GPU previews: {reason}")]
    Unsupported { reason: String },

    /// A preview dimension cannot be represented by the selected device.
    #[error("GPU preview dimensions {width}x{height} are not supported: {reason}")]
    InvalidDimensions {
        width: usize,
        height: usize,
        reason: String,
    },

    /// The caller attempted to apply a recipe to a base made with a different
    /// white-balance selection.
    #[error("GPU preview base does not match the recipe: {reason}")]
    BaseMismatch { reason: String },

    /// The recipe contains stages that this GPU backend does not implement.
    #[error("GPU preview does not support these edits: {reason}")]
    UnsupportedEdits { reason: String },

    /// Waiting for already-submitted GPU work failed.
    #[error("GPU preview queue synchronization failed: {reason}")]
    Synchronization { reason: String },

    /// The core recipe or image state is invalid for the GPU boundary.
    #[error("GPU preview input is invalid: {reason}")]
    InvalidInput { reason: String },

    /// A test-only or diagnostics-only readback operation failed.
    #[error("GPU preview readback failed: {reason}")]
    Readback { reason: String },
}
