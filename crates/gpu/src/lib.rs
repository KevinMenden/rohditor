//! GPU preview processing built on the application's existing `wgpu` device.
//!
//! This crate owns only downstream preview work. RAW decoding, normalization,
//! demosaicing, and camera-to-Rec.2020 conversion remain in `rohditor-core`'s
//! CPU reference path. A [`rohditor_core::DemosaicedBase`] is uploaded once,
//! then this crate evaluates exposure, contrast, saturation, orientation, and
//! the explicit sRGB output transform without reading the display result back
//! to CPU memory.

mod capabilities;
mod preview;

pub use capabilities::GpuCapabilities;
pub use preview::{
    GpuDisplayReadback, GpuPreviewFrame, GpuPreviewProcessor, GpuPreviewSource, GpuPreviewUpload,
};

use thiserror::Error;

/// Failure while creating, uploading, or rendering a GPU preview.
#[derive(Debug, Error)]
pub enum GpuPreviewError {
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

    /// The core recipe or image state is invalid for the GPU boundary.
    #[error("GPU preview input is invalid: {reason}")]
    InvalidInput { reason: String },

    /// A test-only or diagnostics-only readback operation failed.
    #[error("GPU preview readback failed: {reason}")]
    Readback { reason: String },
}
