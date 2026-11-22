//! Toolkit-free preview runtime of an open document.
//!
//! What a frontend needs to show and update the image: where the displayed
//! frame came from, whether a resident GPU base can serve a new request, the
//! ticket guards for asynchronous results, and the GPU-with-CPU-fallback
//! orchestration. Frontends inject their wgpu device ([`WgpuContext`]), keep
//! one [`DocumentPreview`] per document, and register the GPU display texture
//! ([`DocumentPreview::gpu_display`]) with their own toolkit.

mod diagnostics;
mod gpu;
mod histogram;
mod routing;
mod runtime;
mod source;
mod state;

pub use diagnostics::DocumentPreviewDiagnostics;
pub use gpu::{GpuDocumentPreview, GpuDocumentSource, GpuRuntime, WgpuContext};
pub use histogram::{DisplayPixels, HistogramPoll};
pub use routing::{
    gpu_base_algorithm_matches, gpu_supports_recipe, preview_intent_matches_resolution,
};
pub use runtime::{
    GpuUpload, MessageUpdate, PreviewOutcome, PreviewRequest, PreviewRuntime, ProcessorPreference,
};
pub use source::PreviewSource;
pub use state::{DocumentPreview, GpuDisplay};
