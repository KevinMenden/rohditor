//! Shared GPU color processing for native preview and headless export.
//!
//! RAW decoding, normalization, demosaicing, and optics remain in
//! `rohditor-core`'s CPU preparation path. Optional capture sharpening runs on
//! bounded f32 GPU tiles before a temporary readback for CPU optics/reduction.
//! Preview and export share f32 color
//! kernels; export quantizes directly to 8/16-bit integers in bounded bands and
//! returns codec-independent pixels for CPU encoding and transactional writes.
//! The desktop path uploads one camera-native
//! [`rohditor_core::ReconstructedPreview`] without source float quantization and
//! applies white balance, the camera transform, exposure, contrast, saturation,
//! HSL, three-way grading, orientation, and the
//! explicit sRGB output transform as GPU parameters. A legacy converted
//! [`rohditor_core::DemosaicedBase`] upload remains available for lower-level
//! callers. Normal interaction never reads the display result back to CPU
//! memory.

mod capabilities;
mod memory;
pub use memory::{GpuMemoryReservations, gpu_memory_reservations};
mod capture;
pub use capture::{CaptureMetrics, GpuCaptureProcessor, GpuCaptureResult};
mod preview;

pub use preview::{GpuExportProcessor, GpuExportResult};

pub use capabilities::GpuCapabilities;
pub use preview::{
    GpuDisplayReadback, GpuDisplayReadbackPending, GpuPreviewFrame, GpuPreviewProcessor,
    GpuPreviewSource, GpuPreviewUpload,
};

use thiserror::Error;

/// Failure while creating, uploading, or executing GPU color processing.
#[derive(Debug, Error)]
pub enum GpuPreviewError {
    /// CPU-side preparation was superseded before an upload was submitted.
    #[error("GPU processing was cancelled")]
    Cancelled,

    /// The eframe-created device cannot perform the required texture operations.
    #[error("the selected wgpu device cannot support GPU processing: {reason}")]
    Unsupported { reason: String },

    /// A preview dimension cannot be represented by the selected device.
    #[error("GPU image dimensions {width}x{height} are not supported: {reason}")]
    InvalidDimensions {
        width: usize,
        height: usize,
        reason: String,
    },

    /// The caller attempted to apply a recipe to a base made with a different
    /// white-balance selection.
    #[error("GPU source does not match the recipe: {reason}")]
    BaseMismatch { reason: String },

    /// The recipe contains stages that this GPU backend does not implement.
    #[error("GPU processing does not support these edits: {reason}")]
    UnsupportedEdits { reason: String },

    /// Waiting for already-submitted GPU work failed.
    #[error("GPU queue synchronization failed: {reason}")]
    Synchronization { reason: String },

    /// The core recipe or image state is invalid for the GPU boundary.
    #[error("GPU processing input is invalid: {reason}")]
    InvalidInput { reason: String },

    /// A test-only or diagnostics-only readback operation failed.
    #[error("GPU readback failed: {reason}")]
    Readback { reason: String },
}
