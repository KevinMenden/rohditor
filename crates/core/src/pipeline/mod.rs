//! CPU pipeline facade.
//!
//! Preparation, rendering, and memory accounting remain one core-owned
//! contract. The implementation module is private while those responsibilities
//! are split further.

mod memory;
mod orchestration;
mod prepare;
mod render;
mod types;

pub use memory::{CPU_WORKING_SET_LIMIT_BYTES, MemoryEstimate};
pub use prepare::{CpuPreviewWorkspace, DemosaicedBase, ReconstructedPreview};
pub use render::CpuPipeline;
pub use types::{
    ExportRenderResult, OutputPolicy, PreviewOptions, RawCropPolicy, RenderOptions, RenderResult,
    StageTimings,
};

pub const DEFAULT_PREVIEW_LONG_EDGE: usize = orchestration::DEFAULT_PREVIEW_LONG_EDGE;
