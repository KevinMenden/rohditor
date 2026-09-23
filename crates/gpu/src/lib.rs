//! Shared GPU color processing for native preview and headless export.
//!
//! RAW decoding remains in `rohditor-core`; its immutable u16 mosaic is the
//! reference and recovery source. Bounded GPU sensor normalization and its
//! Off/Clip highlight methods feed GPU bilinear/MHC and optional capture without
//! an image readback. Application sensor-backend integration is still pending;
//! normal preview/export sensor preparation continues to use the CPU. Camera-native f32 planes stay
//! resident after optional bounded capture sharpening; optics and exact area
//! reduction execute from the same tiled
//! source without a camera-RGB readback. Preview and export share f32 color
//! kernels. Export evaluates optics and color in bounded bands, quantizes
//! directly to 8/16-bit integers, and returns only codec-independent final
//! pixels for CPU encoding and transactional writes. Fit preview retains a
//! reduced camera-native source, while Source 1:1 develops directly into its
//! display texture. A legacy converted
//! [`rohditor_core::DemosaicedBase`] upload remains available for lower-level
//! callers. Normal interaction never reads the display result back to CPU
//! memory.

mod capabilities;
mod memory;
pub use memory::{GpuMemoryReservations, gpu_memory_reservations};
mod capture;
pub use capture::{CaptureMetrics, GpuCaptureProcessor, GpuCaptureResult};
mod preview;
mod sensor;
mod spatial;

pub use preview::{GpuExportProcessor, GpuExportResult};
pub use spatial::{
    GpuCapturedSource, GpuSpatialFullSource, GpuSpatialPreview, GpuSpatialProcessor, SpatialMetrics,
};

pub use capabilities::GpuCapabilities;
pub use preview::{
    GpuDisplayReadback, GpuDisplayReadbackPending, GpuPreviewFrame, GpuPreviewProcessor,
    GpuPreviewSource, GpuPreviewUpload,
};
pub use sensor::{
    DemosaicMetrics, GpuHighlightedMosaic, GpuNormalizedMosaic, GpuSensorCameraSource,
    GpuSensorProcessor, HighlightMetrics, SensorMetrics,
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
