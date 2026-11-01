//! CPU pipeline facade.
//!
//! Preparation, rendering, and memory accounting remain one core-owned
//! contract. The implementation module is private while those responsibilities
//! are split further.

mod defringe;
mod detail;
mod detail_contract;
mod memory;
mod orchestration;
mod prepare;
mod render;
pub(crate) mod sensor;
mod types;
pub use orchestration::{CapturedCameraSource, DemosaicedCameraSource, SensorCameraMetadata};
pub use sensor::{NormalizationContract, SensorCrop, SensorDevelopmentDescription};

pub use defringe::{DEFRINGE_ALGORITHM_VERSION, DefringeProvenance};
pub use detail::{
    DetailProvenance, FALSE_COLOR_ALGORITHM_VERSION, NOISE_REDUCTION_ALGORITHM_VERSION,
    detail_is_active,
};
pub use detail_contract::DetailContract;
pub use memory::{CPU_WORKING_SET_LIMIT_BYTES, MemoryEstimate};
pub use prepare::{CpuPreviewWorkspace, DemosaicedBase, ReconstructedPreview};
pub use render::CpuPipeline;
pub use types::{
    ExportRenderResult, OutputPolicy, PreviewOptions, RawCropPolicy, RenderOptions, RenderResult,
    SpatialCompletionDescription, SpatialOptics, StageTimings,
};

pub const DEFAULT_PREVIEW_LONG_EDGE: usize = orchestration::DEFAULT_PREVIEW_LONG_EDGE;
