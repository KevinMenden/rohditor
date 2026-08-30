//! Rohditor-owned editor-domain types and deterministic CPU reference pipeline.
//!
//! Sensor mosaics, scene-linear RGB, and display-encoded RGB deliberately use
//! distinct public types. The CPU implementation in this crate is the behavior
//! that later preview and GPU implementations must match.

mod cancel;
mod color;
mod cpu;
mod edit;
mod error;
mod export;
mod image;
mod orientation;
mod output;
mod pipeline;

pub use cancel::CancellationToken;
pub use color::{
    CameraColorTransform, LINEAR_REC2020_TO_XYZ_D65, Matrix3, XYZ_D65_TO_LINEAR_REC2020,
    XYZ_D65_TO_LINEAR_SRGB, adapt_xyz_to_d65, camera_color_transform, clip_linear_srgb_for_output,
    convert_rec2020_to_display_srgb, linear_srgb_to_srgb, srgb_to_linear_srgb,
};
pub use cpu::{
    WhiteBalanceGains, apply_adjustments, demosaic, normalize_raw, normalize_raw_preview,
    render_display_srgb8, render_display_srgb8_dithered, render_display_srgb16,
    white_balance_gains,
};
pub use edit::{
    CONTRAST_RANGE, EDIT_RECIPE_SCHEMA_VERSION, EXPOSURE_EV_RANGE, EditRecipe, ParameterRange,
    SATURATION_RANGE, WHITE_BALANCE_MULTIPLIER_RANGE, WhiteBalance,
};
pub use error::PipelineError;
pub use export::{
    DitherMode, ExportError, ExportFormat, ExportImage, ExportMetadataPolicy, ExportReport,
    ExportSettings, JPEG_QUALITY_DEFAULT, JPEG_QUALITY_MAX, JPEG_QUALITY_MIN, OutputBitDepth,
    PngBitDepth, export_image,
};
pub use image::{
    BayerPattern, CfaColor, DisplayRgbImage, DisplayTransfer, Halo, ImageRegion, LinearRgbImage,
    LinearRgbSpace, MosaicImage,
};
pub use orientation::OrientationMap;
pub use output::{paths_refer_to_same_file, write_output_bytes};
pub use pipeline::{
    CPU_WORKING_SET_LIMIT_BYTES, CpuPipeline, CpuPreviewWorkspace, CropPolicy,
    DEFAULT_PREVIEW_LONG_EDGE, DemosaicAlgorithm, DemosaicedBase, ExportRenderResult,
    MemoryEstimate, NormalizedPreview, OutputPolicy, PreviewOptions, RenderOptions, RenderResult,
    StageTimings,
};
