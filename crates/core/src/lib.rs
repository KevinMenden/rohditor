//! Rohditor's deterministic CPU reference pipeline and processing orchestration.
//!
//! Sensor mosaics, scene-linear RGB, and display-encoded RGB deliberately use
//! distinct public types. The CPU implementation in this crate is the behavior
//! that later preview and GPU implementations must match.

mod analysis;
mod cancel;
mod color;
mod cpu;
mod demosaic;
mod error;
mod export;
mod geometry;
mod highlight;
mod optics;
mod output;
mod pipeline;
mod rendering;
mod resample;
mod white_balance;

pub use rohditor_camera_profile::CAMERA_PROFILE_EVALUATOR_VERSION;

pub use analysis::Histogram;
pub use cancel::CancellationToken;
pub use color::{
    CameraCalibration, CameraColorTransform, CameraProfileKey, CameraProfileProvenance,
    LINEAR_REC2020_TO_XYZ_D65, Matrix3, ResolvedCameraColour, XYZ_D65_TO_LINEAR_REC2020,
    XYZ_D65_TO_LINEAR_SRGB, adapt_xyz_to_d65, camera_color_transform, camera_profile_key,
    clip_linear_srgb_for_output, convert_rec2020_to_display_srgb, linear_srgb_to_srgb,
    resolve_camera_colour, srgb_to_linear_srgb,
};
pub use cpu::{
    HSL_CHANNEL_CENTERS, HSL_HUE_SHIFT_PER_FULL_VALUE, apply_adjustments, evaluate_tone_curve,
    hsl_channel_weights, hsl_channel_weights_from_display_rgb, normalize_raw,
    normalize_raw_preview, render_display_srgb8, render_display_srgb8_dithered,
    render_display_srgb8_dithered_with_geometry, render_display_srgb8_with_geometry,
    render_display_srgb16, render_display_srgb16_with_geometry, white_balance_gains,
    white_balance_gains_from_calibration,
};
pub use error::PipelineError;
pub use export::{
    DitherMode, ExportError, ExportFormat, ExportImage, ExportMetadataPolicy, ExportReport,
    ExportSettings, JPEG_QUALITY_DEFAULT, JPEG_QUALITY_MAX, JPEG_QUALITY_MIN, OutputBitDepth,
    PngBitDepth, export_image,
};
pub use geometry::{OutputGeometry, ResolvedCropRect};
pub use highlight::HighlightDiagnostics;
pub use optics::optics_query_from_info;
pub use output::{paths_refer_to_same_file, write_output_bytes};
pub use pipeline::{
    CPU_WORKING_SET_LIMIT_BYTES, CpuPipeline, CpuPreviewWorkspace, DEFAULT_PREVIEW_LONG_EDGE,
    DemosaicedBase, ExportRenderResult, MemoryEstimate, OutputPolicy, PreviewOptions,
    RawCropPolicy, ReconstructedPreview, RenderOptions, RenderResult, StageTimings,
};
pub use rendering::{
    BASE_RENDERING_LUT_SIZE, BaseRenderingLut, ROHDITOR_STANDARD_MIDDLE_GRAY,
    standard_base_rendering_lut,
};
pub use rohditor_highlight::{
    ClipStats, LOCAL_RATIOS_ALGORITHM_VERSION, OPPOSED_ALGORITHM_VERSION, OpposedStats,
    ReconstructionStats,
};
pub use rohditor_optics::{
    CorrectionComponents, DatabaseProvenance, LensProfileSummary, MetadataField,
    OPTICS_ALGORITHM_VERSION, OpticsError, OpticsProvenance, OpticsQuery, OpticsService,
    ProfileMatch, ProfileRequest,
};
pub use white_balance::{
    WHITE_BALANCE_ALGORITHM_VERSION, WhiteBalanceCoordinates,
    camera_gains_from_as_shot_coordinates, coordinates_from_camera_gains_relative_to_as_shot,
};
