use std::time::Duration;

use crate::coordinator::WorkerPreviewDiagnostics;

/// Timings and residency of the frame currently displayed, for the status bar
/// and the diagnostics window.
#[derive(Debug, Clone, Copy)]
pub struct DocumentPreviewDiagnostics {
    pub worker: WorkerPreviewDiagnostics,
    pub gpu_upload_preparation: Option<Duration>,
    pub gpu_submission: Option<Duration>,
    pub gpu_queue_completion: Option<Duration>,
    pub gpu_histogram_readback: Option<Duration>,
    pub gpu_textures_reused: Option<bool>,
    pub gpu_resident_bytes: usize,
}

impl DocumentPreviewDiagnostics {
    /// Diagnostics of a frame rendered entirely by the CPU worker.
    pub const fn cpu(worker: WorkerPreviewDiagnostics) -> Self {
        Self {
            worker,
            gpu_upload_preparation: None,
            gpu_submission: None,
            gpu_queue_completion: None,
            gpu_histogram_readback: None,
            gpu_textures_reused: None,
            gpu_resident_bytes: 0,
        }
    }
}
