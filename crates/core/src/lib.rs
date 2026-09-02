//! Rohditor-owned editor-domain types and deterministic CPU reference pipeline.
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
mod output;
mod pipeline;
mod resample;

pub use analysis::Histogram;
pub use cancel::CancellationToken;
pub use color::{
    CameraColorTransform, LINEAR_REC2020_TO_XYZ_D65, Matrix3, XYZ_D65_TO_LINEAR_REC2020,
    XYZ_D65_TO_LINEAR_SRGB, adapt_xyz_to_d65, camera_color_transform, clip_linear_srgb_for_output,
    convert_rec2020_to_display_srgb, linear_srgb_to_srgb, srgb_to_linear_srgb,
};
pub use cpu::{
    HSL_CHANNEL_CENTERS, apply_adjustments, evaluate_tone_curve, hsl_channel_weights,
    hsl_channel_weights_from_display_rgb, normalize_raw, normalize_raw_preview,
    render_display_srgb8, render_display_srgb8_dithered, render_display_srgb16,
    white_balance_gains, white_balance_gains_from_calibration,
};
pub use demosaic::{DemosaicAlgorithm, MALVAR_HE_CUTLER_HALO, WhiteBalanceGains, demosaic};
pub use rohditor_edit::{
    BLACKS_RANGE, COLOR_GRADING_RANGE, CONTRAST_RANGE, ColorAdjustments, ColorGradingAdjustments,
    EDIT_RECIPE_SCHEMA_VERSION, EXPOSURE_EV_RANGE, EditError, EditRecipe, GeometryAdjustments,
    HIGHLIGHTS_RANGE, HSL_CHANNEL_COUNT, HSL_HUE_RANGE, HSL_LUMINANCE_RANGE, HSL_SATURATION_RANGE,
    HslAdjustments, HslChannelAdjustments, LightAdjustments, ParameterRange, SATURATION_RANGE,
    SHADOWS_RANGE, TEMPERATURE_RANGE, TINT_RANGE, TONE_CURVE_RANGE, ToneCurve, VIBRANCE_RANGE,
    WHITE_BALANCE_MULTIPLIER_RANGE, WHITES_RANGE, WhiteBalance,
};
pub use error::PipelineError;
pub use export::{
    DitherMode, ExportError, ExportFormat, ExportImage, ExportMetadataPolicy, ExportReport,
    ExportSettings, JPEG_QUALITY_DEFAULT, JPEG_QUALITY_MAX, JPEG_QUALITY_MIN, OutputBitDepth,
    PngBitDepth, export_image,
};
pub use rohditor_image::{
    BayerPattern, CfaColor, DisplayRgbImage, DisplayTransfer, Halo, ImageRegion, LinearRgbImage,
    ImageError, LinearRgbSpace, MosaicImage, OrientationMap,
};
pub use rohditor_edit::{LIGHT_TONE_LUT_SIZE, LightToneLut};
pub use output::{paths_refer_to_same_file, write_output_bytes};
pub use pipeline::{
    CPU_WORKING_SET_LIMIT_BYTES, CpuPipeline, CpuPreviewWorkspace, CropPolicy,
    DEFAULT_PREVIEW_LONG_EDGE, DemosaicedBase, ExportRenderResult, MemoryEstimate, OutputPolicy,
    PreviewOptions, ReconstructedPreview, RenderOptions, RenderResult, StageTimings,
};
