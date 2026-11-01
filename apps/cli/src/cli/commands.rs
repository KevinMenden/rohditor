// CLI argument and command implementation, called by the tiny binary facade.
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use rohditor_camera_profile::parse_dcp_file;
use rohditor_core::{
    CameraCalibration, CpuPipeline, DitherMode, ExportFormat, ExportImage, ExportMetadataPolicy,
    ExportSettings, HighlightDiagnostics, JPEG_QUALITY_DEFAULT, JPEG_QUALITY_MAX, JPEG_QUALITY_MIN,
    OpticsService, OutputPolicy, OutputSize, PngBitDepth, RawCropPolicy, RenderOptions,
    StageTimings, WhiteBalanceCoordinates, export_image, paths_refer_to_same_file,
    resolve_camera_colour, write_output_bytes,
};
use rohditor_demosaic::DemosaicAlgorithm;
use rohditor_edit::{
    BLACKS_RANGE, CONTRAST_RANGE, CameraProfileSelection, EXPOSURE_EV_RANGE, EditRecipe,
    HIGHLIGHT_THRESHOLD_RANGE, HIGHLIGHTS_RANGE, HighlightMethod, LensProfileSelection,
    OpticsAdjustments, RenderingProfileSelection, SATURATION_RANGE, SHADOWS_RANGE,
    TEMPERATURE_RANGE, TINT_RANGE, TONE_CURVE_RANGE, VIBRANCE_RANGE, WHITES_RANGE, WhiteBalance,
};
use rohditor_gpu::{ExportBackend, ExportRender, FallbackExporter};
use rohditor_image::{DisplayRgbImage, DisplayTransfer, Orientation};
use rohditor_raw::{
    EncodedPreviewFormat, ImageRect, PhotometricInterpretation, RawDecoder, RawFileInfo,
    RawlerDecoder,
};
use serde::{Deserialize, Serialize};
use tracing_subscriber::EnvFilter;

use super::detail_compare::{DetailStage, StageSettings, detail_compare};
use super::highlight_compare::highlight_compare;
use super::look_report::{fit_recipe_camera_tone, look_report};

#[derive(Debug, Parser)]
#[command(
    name = "rohditor-cli",
    version,
    about = "Headless tools for Rohditor RAW files"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Inspect normalized metadata and, by default, decode the sensor mosaic.
    Inspect {
        /// RAW file to inspect. Detection uses file contents, not this extension.
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,

        /// Skip full sensor decoding and only probe metadata.
        #[arg(long)]
        metadata_only: bool,
    },

    /// Extract the embedded loading preview without developing the RAW mosaic.
    ExtractPreview {
        /// RAW file containing the preview.
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// JPEG destination (`.jpg` or `.jpeg`).
        #[arg(value_name = "OUTPUT")]
        output: PathBuf,

        /// Replace an existing output file.
        #[arg(long)]
        force: bool,
    },

    /// Develop the RAW mosaic and transactionally export an sRGB JPEG or PNG.
    Develop {
        /// RAW file to develop. Detection uses file contents, not this extension.
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// sRGB destination (`.jpg`, `.jpeg`, or `.png`).
        #[arg(value_name = "OUTPUT")]
        output: PathBuf,

        /// Exposure compensation in stops (-5 to +5).
        #[arg(long, default_value_t = EXPOSURE_EV_RANGE.neutral, allow_hyphen_values = true)]
        exposure: f32,

        /// Contrast in stops of slope around 18% gray (-1 to +1).
        #[arg(long, default_value_t = CONTRAST_RANGE.neutral, allow_hyphen_values = true)]
        contrast: f32,

        /// Recover or increase highlight luminance (-1 to +1).
        #[arg(long, default_value_t = HIGHLIGHTS_RANGE.neutral, allow_hyphen_values = true)]
        highlights: f32,

        /// Recover or increase shadow luminance (-1 to +1).
        #[arg(long, default_value_t = SHADOWS_RANGE.neutral, allow_hyphen_values = true)]
        shadows: f32,

        /// Adjust the brightest white tones (-1 to +1).
        #[arg(long, default_value_t = WHITES_RANGE.neutral, allow_hyphen_values = true)]
        whites: f32,

        /// Adjust the darkest black tones (-1 to +1).
        #[arg(long, default_value_t = BLACKS_RANGE.neutral, allow_hyphen_values = true)]
        blacks: f32,

        /// Lift or lower the tone-curve shadow region (-0.25 to +0.25).
        #[arg(long, default_value_t = TONE_CURVE_RANGE.neutral, allow_hyphen_values = true)]
        tone_shadows: f32,

        /// Lift or lower the tone-curve dark region (-0.25 to +0.25).
        #[arg(long, default_value_t = TONE_CURVE_RANGE.neutral, allow_hyphen_values = true)]
        tone_darks: f32,

        /// Lift or lower the tone-curve light region (-0.25 to +0.25).
        #[arg(long, default_value_t = TONE_CURVE_RANGE.neutral, allow_hyphen_values = true)]
        tone_lights: f32,

        /// Lift or lower the tone-curve highlight region (-0.25 to +0.25).
        #[arg(long, default_value_t = TONE_CURVE_RANGE.neutral, allow_hyphen_values = true)]
        tone_highlights: f32,

        /// Rec.2020 luminance-relative saturation (0 to 2; 1 is neutral).
        #[arg(long, default_value_t = SATURATION_RANGE.neutral)]
        saturation: f32,

        /// Increase or reduce saturation preferentially in less-saturated colors (-1 to +1).
        #[arg(long, default_value_t = VIBRANCE_RANGE.neutral, allow_hyphen_values = true)]
        vibrance: f32,

        /// RAW-stage highlight handling.
        #[arg(long, value_enum, default_value_t = CliHighlightMethod::Clip)]
        highlight_reconstruction: CliHighlightMethod,

        /// Effective normalized white threshold for highlight clipping (0.5 to 1.5).
        #[arg(
            long = "highlight-clip-threshold",
            alias = "highlight-threshold",
            allow_hyphen_values = true
        )]
        highlight_clip_threshold: Option<f32>,

        /// Camera-native detection threshold for Local ratios or Opposed (0.5 to 1.5).
        #[arg(long, allow_hyphen_values = true)]
        highlight_detection_threshold: Option<f32>,

        /// R,G,B multipliers relative to the as-shot white balance.
        #[arg(long, value_name = "RED,GREEN,BLUE")]
        white_balance: Option<RgbMultipliers>,

        /// DNG Camera Profile (DCP) to embed in the recipe: its matrices, hue/saturation map, and look table.
        #[arg(long, value_name = "PROFILE.dcp")]
        camera_profile: Option<PathBuf>,

        /// Moiré / false-colour suppression: median passes over the colour
        /// differences (0 to 5; 0 is off). Runs on the CPU.
        #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u8).range(0..=5))]
        false_color_steps: u8,

        /// Purple defringe strength (0 to 1; 0 is off). Runs on the CPU.
        #[arg(long, default_value_t = 0.0)]
        defringe_purple: f32,

        /// Hue range of purple fringes in degrees, `START,END` (10 to 90 wide).
        #[arg(long, value_name = "START,END", default_value = "270,330", value_parser = parse_hue_range)]
        defringe_purple_hue: [f32; 2],

        /// Green defringe strength (0 to 1; 0 is off). Runs on the CPU.
        #[arg(long, default_value_t = 0.0)]
        defringe_green: f32,

        /// Hue range of green fringes in degrees, `START,END` (10 to 90 wide).
        #[arg(long, value_name = "START,END", default_value = "75,150", value_parser = parse_hue_range)]
        defringe_green_hue: [f32; 2],

        /// Chroma noise reduction strength (0 to 1; 0 is off). Runs on the CPU.
        #[arg(long, default_value_t = 0.0)]
        chroma_nr: f32,

        /// Luminance noise reduction strength (0 to 1; 0 is off). Runs on the CPU.
        #[arg(long, default_value_t = 0.0)]
        luma_nr: f32,
        /// Fine texture the luminance noise reduction keeps (0 to 1; higher keeps more).
        #[arg(long, default_value_t = 0.5)]
        luma_nr_detail: f32,

        /// Enable source-resolution capture sharpening (eight RL iterations).
        #[arg(long)]
        capture_sharpening: bool,
        /// Capture sharpening blend (0 to 1).
        #[arg(long, default_value_t = 0.5)]
        capture_amount: f32,
        /// Capture Gaussian sigma in source pixels (0.3 to 1.2).
        #[arg(long, default_value_t = 0.6)]
        capture_radius: f32,
        /// Suppress sharpening in low-contrast/noisy areas (0 to 1).
        #[arg(long, default_value_t = 0.5)]
        capture_noise_protection: f32,

        /// Base rendering: Matched (tone and colour fitted to the camera JPEG),
        /// Standard tone rendering, or Neutral identity rendering.
        #[arg(long, value_enum, default_value_t = CliRenderingProfile::Matched)]
        rendering_profile: CliRenderingProfile,

        /// Target sRGB gamut handling after rendering and creative edits.
        #[arg(long, value_enum, default_value_t = CliOutputGamut::Clip)]
        output_gamut: CliOutputGamut,

        /// Color execution backend for full-resolution export.
        #[arg(long, value_enum, default_value_t = ExportProcessor::Auto)]
        processor: ExportProcessor,

        /// Absolute camera-calibrated white-balance temperature in Kelvin
        /// (2000 to 25000). If omitted, the source's As Shot estimate is used.
        #[arg(long, allow_hyphen_values = true)]
        temperature: Option<f32>,

        /// Green/magenta tint offset from As Shot (-1 to +1); implies
        /// Temperature/Tint white balance.
        #[arg(long, default_value_t = TINT_RANGE.neutral, allow_hyphen_values = true)]
        tint: f32,

        /// RAW sensor crop policy, applied before normalization.
        #[arg(long, value_enum, default_value_t = CliCropPolicy::Recommended)]
        crop: CliCropPolicy,

        /// CPU demosaic algorithm.
        #[arg(long, value_enum, default_value_t = CliDemosaic::Amaze)]
        demosaic: CliDemosaic,

        /// Lensfun profile selection: off, auto, or a stable profile ID.
        #[arg(long, default_value = "off")]
        lens_profile: String,

        /// Do not apply the selected profile's distortion correction.
        #[arg(long)]
        no_lens_distortion: bool,

        /// Do not apply the selected profile's vignetting correction.
        #[arg(long)]
        no_lens_vignetting: bool,

        /// Do not apply the selected profile's chromatic-aberration correction.
        #[arg(long)]
        no_lens_tca: bool,

        /// Replace the RAW orientation metadata with an explicit transform.
        #[arg(long, value_enum)]
        orientation: Option<CliOrientation>,

        /// JPEG quality from 1 (smallest) to 100 (highest); JPEG only.
        #[arg(
            long,
            value_name = "1-100",
            value_parser = clap::value_parser!(u8).range(i64::from(JPEG_QUALITY_MIN)..=i64::from(JPEG_QUALITY_MAX))
        )]
        jpeg_quality: Option<u8>,

        /// Integer sample depth; PNG only (default: 8).
        #[arg(long, value_enum, value_name = "8|16")]
        png_bit_depth: Option<CliPngBitDepth>,

        /// Resize so the longer edge has this many pixels (never upscales).
        #[arg(long, value_name = "PIXELS", conflicts_with = "fit", value_parser = clap::value_parser!(u32).range(1..))]
        long_edge: Option<u32>,

        /// Resize to fit inside a WxH bounding box (never upscales).
        #[arg(long, value_name = "WxH", value_parser = parse_fit_box)]
        fit: Option<(u32, u32)>,

        /// Apply deterministic ordered dithering before integer quantization.
        #[arg(long)]
        dither: bool,

        /// Preserve selected safe capture EXIF fields or omit source metadata.
        #[arg(long, value_enum, default_value_t = CliMetadata::Safe)]
        metadata: CliMetadata,

        /// Replace an existing output file.
        #[arg(long)]
        force: bool,
    },

    /// Emit named 100% and 200% neutral crops for Phase 9 quality review.
    QualityCrops {
        /// JSON manifest containing private source names and crop coordinates.
        #[arg(value_name = "MANIFEST")]
        manifest: PathBuf,

        /// Directory containing the private RAW sources named by the manifest.
        #[arg(value_name = "CORPUS_DIR")]
        corpus: PathBuf,

        /// Destination directory for generated PNGs and report.json.
        #[arg(value_name = "OUTPUT_DIR")]
        output: PathBuf,

        /// CPU demosaic algorithm under review.
        #[arg(long, value_enum, default_value_t = CliDemosaic::Amaze)]
        demosaic: CliDemosaic,

        /// Replace existing generated crops and report.json.
        #[arg(long)]
        force: bool,
    },

    /// Score a detail stage (default: no-op baseline) on the synthetic
    /// ground-truth scenes at several noise levels, and print a score table.
    DetailCompare {
        /// Stage to evaluate.
        #[arg(long, value_enum, default_value_t = DetailStage::Noop)]
        stage: DetailStage,

        /// Stage strength in 0..=1, for stages that have one.
        #[arg(long, default_value_t = 0.5)]
        strength: f32,

        /// Texture preservation in 0..=1, for stages that have one.
        #[arg(long, default_value_t = 0.5)]
        detail: f32,

        /// Median passes of the false-colour stages (0 to 5).
        #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(0..=5))]
        steps: u8,

        /// Demosaic the synthetic scenes go through before the stage.
        #[arg(long, value_enum, default_value_t = CliDemosaic::Bilinear)]
        demosaic: CliDemosaic,

        /// Also score every readable RAW file in this directory.
        #[arg(long, value_name = "RAW_DIR")]
        raw_dir: Option<PathBuf>,
    },

    /// Render every highlight method on the quality-crop manifest and report
    /// reconstruction counts, scratch bytes, wall time, and differences from Clip.
    HighlightCompare {
        /// JSON manifest containing private source names and crop coordinates.
        #[arg(value_name = "MANIFEST")]
        manifest: PathBuf,

        /// Directory containing the private RAW sources named by the manifest.
        #[arg(value_name = "CORPUS_DIR")]
        corpus: PathBuf,

        /// Destination directory for per-method crop PNGs and report.json.
        #[arg(value_name = "OUTPUT_DIR")]
        output: PathBuf,

        /// CPU demosaic algorithm used for every method.
        #[arg(long, value_enum, default_value_t = CliDemosaic::Amaze)]
        demosaic: CliDemosaic,

        /// Also write each method's full-resolution PNG export.
        #[arg(long)]
        full_exports: bool,

        /// Replace existing generated files and report.json.
        #[arg(long)]
        force: bool,
    },

    /// Compare Neutral, Standard and camera-matched renderings with each RAW's
    /// embedded camera JPEG and report the tone and colour error.
    LookReport {
        /// RAW files to measure. Each must contain an embedded JPEG preview.
        #[arg(value_name = "FILE", required = true)]
        files: Vec<PathBuf>,

        /// Render with this DCP camera profile instead of the built-in matrices.
        #[arg(long, value_name = "DCP")]
        camera_profile: Option<PathBuf>,

        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Compare Rohditor's decoded sensor mosaic with LibRaw unprocessed_raw PGM output.
    VerifyLibraw {
        /// RAW source decoded by Rohditor/rawler.
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// 16-bit PGM emitted by LibRaw's unprocessed_raw sample tool.
        #[arg(value_name = "LIBRAW_PGM")]
        libraw_pgm: PathBuf,

        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,

        /// Largest accepted per-sample decoder difference in digital numbers.
        #[arg(long, default_value_t = 8)]
        max_error: u16,

        /// Largest accepted aggregate decoder RMSE in digital numbers.
        #[arg(long, default_value_t = 1.1)]
        max_rmse: f64,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliCropPolicy {
    ActiveArea,
    Recommended,
}

impl From<CliCropPolicy> for RawCropPolicy {
    fn from(value: CliCropPolicy) -> Self {
        match value {
            CliCropPolicy::ActiveArea => Self::ActiveArea,
            CliCropPolicy::Recommended => Self::Recommended,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliDemosaic {
    Bilinear,
    #[value(name = "mhc")]
    MalvarHeCutler,
    #[value(name = "rcd")]
    Rcd,
    #[value(name = "amaze")]
    Amaze,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum CliHighlightMethod {
    #[default]
    Off,
    Clip,
    #[value(name = "local-ratios")]
    LocalRatios,
    #[value(name = "opposed")]
    Opposed,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum CliRenderingProfile {
    Standard,
    Neutral,
    /// Tone and colour fitted to the camera's embedded JPEG; falls back to a
    /// typical camera look when the fit is not possible.
    #[default]
    Matched,
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum CliOutputGamut {
    #[default]
    Clip,
    #[value(name = "chroma-compress")]
    ChromaCompress,
}

impl From<CliOutputGamut> for OutputPolicy {
    fn from(value: CliOutputGamut) -> Self {
        match value {
            CliOutputGamut::Clip => Self::ClipToSrgb,
            CliOutputGamut::ChromaCompress => Self::ChromaCompressToSrgb,
        }
    }
}

impl From<CliRenderingProfile> for RenderingProfileSelection {
    fn from(value: CliRenderingProfile) -> Self {
        match value {
            CliRenderingProfile::Standard => Self::STANDARD,
            CliRenderingProfile::Neutral => Self::NEUTRAL,
            CliRenderingProfile::Matched => Self::MATCHED,
        }
    }
}

impl From<CliHighlightMethod> for HighlightMethod {
    fn from(value: CliHighlightMethod) -> Self {
        match value {
            CliHighlightMethod::Off => Self::Off,
            CliHighlightMethod::Clip => Self::Clip,
            CliHighlightMethod::LocalRatios => Self::LocalRatios,
            CliHighlightMethod::Opposed => Self::Opposed,
        }
    }
}

impl CliDemosaic {
    const fn label(self) -> &'static str {
        match self {
            Self::Bilinear => "bilinear",
            Self::MalvarHeCutler => "mhc",
            Self::Rcd => "rcd",
            Self::Amaze => "amaze",
        }
    }
}

impl From<CliDemosaic> for DemosaicAlgorithm {
    fn from(value: CliDemosaic) -> Self {
        match value {
            CliDemosaic::Bilinear => Self::Bilinear,
            CliDemosaic::MalvarHeCutler => Self::MalvarHeCutler,
            CliDemosaic::Rcd => Self::Rcd,
            CliDemosaic::Amaze => Self::Amaze,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliOrientation {
    Normal,
    HorizontalFlip,
    Rotate180,
    VerticalFlip,
    Transpose,
    Rotate90,
    Transverse,
    Rotate270,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliPngBitDepth {
    #[value(name = "8", alias = "eight")]
    Eight,
    #[value(name = "16", alias = "sixteen")]
    Sixteen,
}

impl From<CliPngBitDepth> for PngBitDepth {
    fn from(value: CliPngBitDepth) -> Self {
        match value {
            CliPngBitDepth::Eight => Self::Eight,
            CliPngBitDepth::Sixteen => Self::Sixteen,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, ValueEnum)]
enum CliMetadata {
    None,
    #[default]
    Safe,
}

impl From<CliMetadata> for ExportMetadataPolicy {
    fn from(value: CliMetadata) -> Self {
        match value {
            CliMetadata::None => Self::None,
            CliMetadata::Safe => Self::Safe,
        }
    }
}

impl From<CliOrientation> for Orientation {
    fn from(value: CliOrientation) -> Self {
        match value {
            CliOrientation::Normal => Self::Normal,
            CliOrientation::HorizontalFlip => Self::HorizontalFlip,
            CliOrientation::Rotate180 => Self::Rotate180,
            CliOrientation::VerticalFlip => Self::VerticalFlip,
            CliOrientation::Transpose => Self::Transpose,
            CliOrientation::Rotate90 => Self::Rotate90,
            CliOrientation::Transverse => Self::Transverse,
            CliOrientation::Rotate270 => Self::Rotate270,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RgbMultipliers {
    red: f32,
    green: f32,
    blue: f32,
}

impl FromStr for RgbMultipliers {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let values = value
            .split(',')
            .map(|component| {
                component
                    .parse::<f32>()
                    .map_err(|error| format!("invalid RGB multiplier {component:?}: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let [red, green, blue] = values.as_slice() else {
            return Err("expected exactly three comma-separated values: RED,GREEN,BLUE".to_owned());
        };
        Ok(Self {
            red: *red,
            green: *green,
            blue: *blue,
        })
    }
}

#[derive(Debug, Serialize)]
struct InspectionOutput<'a> {
    file: String,
    decoded: bool,
    decoded_sample_count: Option<usize>,
    metadata: &'a RawFileInfo,
}

pub(crate) fn run() -> Result<()> {
    initialize_tracing();
    let cli = Cli::parse();

    match cli.command {
        Command::Inspect {
            file,
            json,
            metadata_only,
        } => inspect(&file, json, metadata_only),
        Command::ExtractPreview {
            file,
            output,
            force,
        } => extract_preview(&file, &output, force),
        Command::Develop {
            file,
            output,
            exposure,
            contrast,
            highlights,
            shadows,
            whites,
            blacks,
            tone_shadows,
            tone_darks,
            tone_lights,
            tone_highlights,
            saturation,
            vibrance,
            highlight_reconstruction,
            highlight_clip_threshold,
            highlight_detection_threshold,
            white_balance,
            camera_profile,
            rendering_profile,
            false_color_steps,
            defringe_purple,
            defringe_purple_hue,
            defringe_green,
            defringe_green_hue,
            chroma_nr,
            luma_nr,
            luma_nr_detail,
            capture_sharpening,
            capture_amount,
            capture_radius,
            capture_noise_protection,
            output_gamut,
            processor,
            temperature,
            tint,
            crop,
            demosaic,
            lens_profile,
            no_lens_distortion,
            no_lens_vignetting,
            no_lens_tca,
            orientation,
            jpeg_quality,
            png_bit_depth,
            long_edge,
            fit,
            dither,
            metadata,
            force,
        } => develop(
            &file,
            &output,
            DevelopArguments {
                exposure,
                contrast,
                highlights,
                shadows,
                whites,
                blacks,
                tone_shadows,
                tone_darks,
                tone_lights,
                tone_highlights,
                saturation,
                vibrance,
                highlight_reconstruction,
                highlight_clip_threshold,
                highlight_detection_threshold,
                white_balance,
                camera_profile,
                rendering_profile,
                false_color_steps,
                defringe_purple,
                defringe_purple_hue,
                defringe_green,
                defringe_green_hue,
                chroma_nr,
                luma_nr,
                luma_nr_detail,
                capture_sharpening,
                capture_amount,
                capture_radius,
                capture_noise_protection,
                output_gamut,
                processor,
                temperature,
                tint,
                crop,
                demosaic,
                lens_profile,
                no_lens_distortion,
                no_lens_vignetting,
                no_lens_tca,
                orientation,
                jpeg_quality,
                png_bit_depth,
                long_edge,
                fit,
                dither,
                metadata,
                force,
            },
        ),
        Command::QualityCrops {
            manifest,
            corpus,
            output,
            demosaic,
            force,
        } => quality_crops(&manifest, &corpus, &output, demosaic, force),
        Command::DetailCompare {
            stage,
            strength,
            detail,
            steps,
            demosaic,
            raw_dir,
        } => detail_compare(
            stage,
            StageSettings {
                strength,
                detail,
                steps,
                demosaic: demosaic.into(),
            },
            raw_dir.as_deref(),
        ),
        Command::HighlightCompare {
            manifest,
            corpus,
            output,
            demosaic,
            full_exports,
            force,
        } => highlight_compare(
            &manifest,
            &corpus,
            &output,
            demosaic.into(),
            full_exports,
            force,
        ),
        Command::LookReport {
            files,
            camera_profile,
            json,
        } => look_report(&files, camera_profile.as_deref(), json),
        Command::VerifyLibraw {
            file,
            libraw_pgm,
            json,
            max_error,
            max_rmse,
        } => verify_libraw(&file, &libraw_pgm, json, max_error, max_rmse),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QualityCropManifest {
    schema_version: u32,
    pub(super) crops: Vec<QualityCropSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QualityCropSpec {
    pub(super) source: String,
    pub(super) name: String,
    pub(super) category: String,
    pub(super) x: usize,
    pub(super) y: usize,
    pub(super) width: usize,
    pub(super) height: usize,
}

#[derive(Debug, Serialize)]
struct QualityCropReport {
    schema_version: u32,
    pipeline_version: &'static str,
    recipe: &'static str,
    crop_policy: &'static str,
    orientation: &'static str,
    algorithm: &'static str,
    reconstruction_policy: &'static str,
    manifest: String,
    sources: Vec<QualitySourceReport>,
    crops: Vec<QualityCropArtifact>,
}

#[derive(Debug, Serialize)]
struct QualitySourceReport {
    source: String,
    source_identity: Option<rohditor_raw::SourceIdentity>,
    source_bits_per_sample: Option<usize>,
    decoded_dimensions: [usize; 2],
    developed_dimensions: [usize; 2],
    decode_ms: f64,
    timings_ms: QualityTimingReport,
    estimated_peak_bytes: usize,
}

#[derive(Debug, Serialize)]
struct QualityTimingReport {
    metadata: f64,
    normalization: f64,
    highlight_processing: f64,
    demosaic: f64,
    false_color: f64,
    chroma_noise_reduction: f64,
    luminance_noise_reduction: f64,
    capture_sharpening: f64,
    optics: f64,
    defringe: f64,
    resampling: f64,
    color_conversion: f64,
    adjustments: f64,
    output_conversion: f64,
    total: f64,
}

impl From<StageTimings> for QualityTimingReport {
    fn from(value: StageTimings) -> Self {
        Self {
            metadata: milliseconds(value.metadata),
            normalization: milliseconds(value.normalization),
            highlight_processing: milliseconds(value.highlight_processing),
            demosaic: milliseconds(value.demosaic),
            false_color: milliseconds(value.false_color),
            chroma_noise_reduction: milliseconds(value.chroma_noise_reduction),
            luminance_noise_reduction: milliseconds(value.luminance_noise_reduction),
            capture_sharpening: milliseconds(value.capture_sharpening),
            optics: milliseconds(value.optics),
            defringe: milliseconds(value.defringe),
            resampling: milliseconds(value.resampling),
            color_conversion: milliseconds(value.color_conversion),
            adjustments: milliseconds(value.adjustments),
            output_conversion: milliseconds(value.output_conversion),
            total: milliseconds(value.total),
        }
    }
}

#[derive(Debug, Serialize)]
struct QualityCropArtifact {
    source: String,
    name: String,
    category: String,
    coordinates: [usize; 4],
    output_100_percent: String,
    output_200_percent: String,
}

#[derive(Debug, Serialize)]
struct LibRawVerificationReport {
    schema_version: u32,
    rohditor_decoder: &'static str,
    independent_decoder: &'static str,
    source: String,
    source_identity: Option<rohditor_raw::SourceIdentity>,
    source_bits_per_sample: Option<usize>,
    dimensions: [usize; 2],
    row_stride: usize,
    cfa: String,
    recommended_crop: Option<ImageRect>,
    black_levels: Vec<f32>,
    white_levels: Vec<f32>,
    compared_samples: usize,
    mismatched_samples: usize,
    maximum_absolute_error: u16,
    root_mean_squared_error: f64,
    accepted_maximum_absolute_error: u16,
    accepted_root_mean_squared_error: f64,
    within_tolerance: bool,
    first_mismatch: Option<LibRawMismatch>,
}

#[derive(Debug, Serialize)]
struct LibRawMismatch {
    x: usize,
    y: usize,
    rohditor: u16,
    libraw: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum ExportProcessor {
    Auto,
    Cpu,
    Gpu,
}

impl From<ExportProcessor> for ExportBackend {
    fn from(processor: ExportProcessor) -> Self {
        match processor {
            ExportProcessor::Auto => Self::PreferGpu,
            ExportProcessor::Cpu => Self::Cpu,
            ExportProcessor::Gpu => Self::RequireGpu,
        }
    }
}

#[derive(Debug, Clone)]
struct DevelopArguments {
    processor: ExportProcessor,
    exposure: f32,
    contrast: f32,
    highlights: f32,
    shadows: f32,
    whites: f32,
    blacks: f32,
    tone_shadows: f32,
    tone_darks: f32,
    tone_lights: f32,
    tone_highlights: f32,
    saturation: f32,
    vibrance: f32,
    highlight_reconstruction: CliHighlightMethod,
    highlight_clip_threshold: Option<f32>,
    highlight_detection_threshold: Option<f32>,
    white_balance: Option<RgbMultipliers>,
    camera_profile: Option<PathBuf>,
    rendering_profile: CliRenderingProfile,
    false_color_steps: u8,
    defringe_purple: f32,
    defringe_purple_hue: [f32; 2],
    defringe_green: f32,
    defringe_green_hue: [f32; 2],
    chroma_nr: f32,
    luma_nr: f32,
    luma_nr_detail: f32,
    capture_sharpening: bool,
    capture_amount: f32,
    capture_radius: f32,
    capture_noise_protection: f32,
    output_gamut: CliOutputGamut,
    temperature: Option<f32>,
    tint: f32,
    crop: CliCropPolicy,
    demosaic: CliDemosaic,
    lens_profile: String,
    no_lens_distortion: bool,
    no_lens_vignetting: bool,
    no_lens_tca: bool,
    orientation: Option<CliOrientation>,
    jpeg_quality: Option<u8>,
    png_bit_depth: Option<CliPngBitDepth>,
    long_edge: Option<u32>,
    fit: Option<(u32, u32)>,
    dither: bool,
    metadata: CliMetadata,
    force: bool,
}

fn initialize_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("error"));
    let _subscriber_result = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(io::stderr)
        .try_init();
}

fn inspect(file: &Path, json: bool, metadata_only: bool) -> Result<()> {
    let decoder = RawlerDecoder::default();
    let (metadata, decoded_sample_count) = if metadata_only {
        let metadata = decoder
            .probe(file)
            .with_context(|| format!("could not inspect {}", file.display()))?;
        (metadata, None)
    } else {
        let frame = decoder
            .decode(file)
            .with_context(|| format!("could not decode {}", file.display()))?;
        let sample_count = frame.mosaic.len();
        (frame.info, Some(sample_count))
    };

    let output = InspectionOutput {
        file: file.display().to_string(),
        decoded: decoded_sample_count.is_some(),
        decoded_sample_count,
        metadata: &metadata,
    };

    if json {
        let json =
            serde_json::to_string_pretty(&output).context("could not serialize inspection JSON")?;
        write_stdout(&json)?;
    } else {
        write_stdout(&human_readable(&output))?;
    }
    Ok(())
}

fn extract_preview(file: &Path, output: &Path, force: bool) -> Result<()> {
    if paths_refer_to_same_file(file, output).with_context(|| {
        format!(
            "could not compare preview source {} with output {}",
            file.display(),
            output.display()
        )
    })? {
        bail!(
            "refusing to replace source RAW file {} with its embedded preview",
            file.display()
        );
    }

    let decoder = RawlerDecoder::default();
    let preview = decoder
        .embedded_preview(file)
        .with_context(|| format!("could not extract a preview from {}", file.display()))?;
    let Some(preview) = preview else {
        bail!("{} does not contain an embedded preview", file.display());
    };
    validate_preview_extension(output, preview.format)?;

    write_output_bytes(output, &preview.bytes, force)
        .with_context(|| format!("could not commit preview output {}", output.display()))?;

    let representation = if preview.is_original_encoding {
        "original embedded"
    } else {
        "normalized"
    };
    write_stdout(&format!(
        "Extracted {representation} {} preview ({}x{}, {} bytes) to {}",
        preview.format,
        preview.width,
        preview.height,
        preview.bytes.len(),
        output.display()
    ))
}

fn white_balance_from_arguments(
    arguments: &DevelopArguments,
    as_shot: Option<WhiteBalanceCoordinates>,
) -> WhiteBalance {
    match (
        arguments.white_balance,
        arguments.temperature,
        arguments.tint,
    ) {
        (Some(value), None, 0.0) => WhiteBalance::ManualMultipliers {
            red: value.red,
            green: value.green,
            blue: value.blue,
        },
        (None, temperature, tint) if temperature.is_some() || tint != 0.0 => {
            let as_shot = as_shot.unwrap_or(WhiteBalanceCoordinates {
                temperature: TEMPERATURE_RANGE.neutral,
                tint: TINT_RANGE.neutral,
            });
            WhiteBalance::TemperatureTint {
                temperature: temperature.unwrap_or(as_shot.temperature),
                tint: if tint == TINT_RANGE.neutral {
                    as_shot.tint
                } else {
                    tint
                },
            }
        }
        (None, None, 0.0) => WhiteBalance::AsShot,
        _ => unreachable!("white balance conflict was rejected above"),
    }
}

fn develop(file: &Path, output: &Path, arguments: DevelopArguments) -> Result<()> {
    validate_highlight_options(
        arguments.highlight_reconstruction,
        arguments.highlight_clip_threshold,
        arguments.highlight_detection_threshold,
    )?;
    let export_settings = develop_export_settings(output, &arguments)?;
    if paths_refer_to_same_file(file, output).with_context(|| {
        format!(
            "could not compare RAW source {} with output {}",
            file.display(),
            output.display()
        )
    })? {
        bail!(
            "refusing to replace source RAW file {} with developed output",
            file.display()
        );
    }
    if !export_settings.overwrite && output.exists() {
        bail!(
            "output {} already exists; pass --force to replace it",
            output.display()
        );
    }

    if arguments.white_balance.is_some()
        && (arguments.temperature.is_some() || arguments.tint != 0.0)
    {
        bail!("--white-balance cannot be combined with --temperature or --tint");
    }
    let mut recipe = EditRecipe {
        optics: optics_adjustments(&arguments)?,
        ..EditRecipe::default()
    };
    let camera_profile = arguments
        .camera_profile
        .as_deref()
        .map(parse_dcp_file)
        .transpose()
        .with_context(|| "could not parse the requested camera profile")?;
    if let Some(profile) = camera_profile {
        recipe.color.camera_profile = CameraProfileSelection::Matrix(profile);
    }
    recipe.false_color = rohditor_edit::FalseColorSuppression {
        steps: arguments.false_color_steps,
    };
    recipe.defringe = rohditor_edit::Defringe {
        purple_amount: arguments.defringe_purple,
        purple_hue: arguments.defringe_purple_hue,
        green_amount: arguments.defringe_green,
        green_hue: arguments.defringe_green_hue,
    };
    recipe.noise_reduction = rohditor_edit::NoiseReduction {
        chroma: arguments.chroma_nr,
        luminance: arguments.luma_nr,
        luminance_detail: arguments.luma_nr_detail,
    };
    recipe.capture_sharpening = rohditor_edit::CaptureSharpening {
        enabled: arguments.capture_sharpening,
        amount: arguments.capture_amount,
        radius: arguments.capture_radius,
        noise_protection: arguments.capture_noise_protection,
    };
    recipe.rendering.profile = arguments.rendering_profile.into();
    recipe.light.exposure_ev = arguments.exposure;
    recipe.light.contrast = arguments.contrast;
    recipe.light.highlights = arguments.highlights;
    recipe.light.shadows = arguments.shadows;
    recipe.light.whites = arguments.whites;
    recipe.light.blacks = arguments.blacks;
    recipe.light.tone_curve.shadows = arguments.tone_shadows;
    recipe.light.tone_curve.darks = arguments.tone_darks;
    recipe.light.tone_curve.lights = arguments.tone_lights;
    recipe.light.tone_curve.highlights = arguments.tone_highlights;
    recipe.color.saturation = arguments.saturation;
    recipe.color.vibrance = arguments.vibrance;
    recipe.raw.highlights.method = arguments.highlight_reconstruction.into();
    recipe.raw.highlights.clip.threshold = arguments
        .highlight_clip_threshold
        .unwrap_or(HIGHLIGHT_THRESHOLD_RANGE.neutral);
    let detection_threshold = arguments
        .highlight_detection_threshold
        .unwrap_or(HIGHLIGHT_THRESHOLD_RANGE.neutral);
    match recipe.raw.highlights.method {
        HighlightMethod::LocalRatios => {
            recipe.raw.highlights.local_ratios.detection_threshold = detection_threshold;
        }
        HighlightMethod::Opposed => {
            recipe.raw.highlights.opposed.detection_threshold = detection_threshold;
        }
        HighlightMethod::Off | HighlightMethod::Clip => {}
    }
    recipe.geometry.orientation_override = arguments.orientation.map(Into::into);
    // Validate explicitly supplied values before decoding so malformed CLI
    // input reports a recipe error even when the source itself is unreadable.
    recipe.color.white_balance = white_balance_from_arguments(&arguments, None);
    recipe
        .validate()
        .context("could not validate the development recipe")?;

    let decoder = RawlerDecoder::default();
    let pipeline = match recipe.optics.profile {
        LensProfileSelection::Off => CpuPipeline::default(),
        LensProfileSelection::Automatic | LensProfileSelection::Lensfun { .. } => {
            let service = OpticsService::load_bundled()
                .context("could not load the bundled Lensfun profile database")?;
            CpuPipeline::new(Arc::new(service))
        }
    };
    let decode_started = Instant::now();
    let frame = decoder
        .decode(file)
        .with_context(|| format!("could not decode {} for development", file.display()))?;
    let decode_time = decode_started.elapsed();
    let as_shot_coordinates = resolve_camera_colour(
        &CameraCalibration::from_raw_info(&frame.info),
        &recipe.color.camera_profile,
        WhiteBalance::AsShot,
    )
    .ok()
    .and_then(|resolved| resolved.as_shot_coordinates);
    recipe.color.white_balance = white_balance_from_arguments(&arguments, as_shot_coordinates);
    recipe
        .validate()
        .context("could not validate the development recipe")?;
    if recipe.rendering.profile == RenderingProfileSelection::MATCHED {
        fit_recipe_camera_tone(&decoder, &pipeline, file, &frame, &mut recipe)?;
    }
    let rendered = FallbackExporter::default()
        .render(
            arguments.processor.into(),
            &pipeline,
            &frame,
            &recipe,
            RenderOptions {
                raw_crop_policy: arguments.crop.into(),
                demosaic: arguments.demosaic.into(),
                output_policy: arguments.output_gamut.into(),
            },
            export_settings.format.bit_depth(),
            export_settings.dithering,
            export_settings.size,
            |error| eprintln!("GPU export unavailable; using CPU: {error}"),
        )
        .with_context(|| format!("could not develop {}", file.display()))?;
    let result = match rendered {
        ExportRender::Gpu {
            result,
            capabilities,
        } => {
            let encode_started = Instant::now();
            let report = export_image(output, &result.image, &frame.info, export_settings)
                .with_context(|| {
                    format!("could not export developed image to {}", output.display())
                })?;
            return write_stdout(&format!(
                "Developed {}x{} {}-bit sRGB {} to {} ({} bytes)\nColor processor: GPU, {} ({}, {} {})\nSensor processor: {}\nRAW decode: {:?}; Full-quality preparation: {:?}; GPU upload: {:?}; color and readback: {:?}; encoding: {:?}\nGPU capture: {} tiles of {} pixels, halo {}, compute/wait {:?}, upload {:?}, readback {:?}\nGPU allocation estimate: {} MiB; CPU buffer estimate: {} MiB; uploaded {} bytes; read back {} bytes in {} submissions\nOutput gamut pixel counts: unavailable on GPU",
                report.width,
                report.height,
                report.bit_depth.bits(),
                export_settings.format.description(),
                output.display(),
                report.bytes_written,
                capabilities.adapter_name,
                capabilities.backend,
                capabilities.driver,
                capabilities.driver_info,
                if result.sensor_gpu { "GPU" } else { "CPU" },
                decode_time,
                result.preparation,
                result.upload_time,
                result.color_and_readback_time,
                encode_started.elapsed(),
                result.capture.tiles,
                result.capture.tile_edge,
                result.capture.halo,
                result.capture.compute_and_wait,
                result.capture.upload,
                result.capture.readback,
                result.estimated_gpu_bytes / (1024 * 1024),
                result.estimated_cpu_bytes / (1024 * 1024),
                result.uploaded_bytes,
                result.readback_bytes,
                result.submissions,
            ));
        }
        ExportRender::Cpu { result, .. } => result,
    };
    let source_info = frame.info.clone();
    drop(frame);
    let encode_started = Instant::now();
    let report = export_image(output, &result.image, &source_info, export_settings)
        .with_context(|| format!("could not export developed image to {}", output.display()))?;
    let encode_time = encode_started.elapsed();

    let highlight_report = match result.highlight_diagnostics {
        HighlightDiagnostics::Off => "Highlight reconstruction: off".to_owned(),
        HighlightDiagnostics::Clip(stats) => format!(
            "Highlight Clip: {} affected, {} changed, {} nominally over-white (R {}, G {}, B {})",
            stats.affected_sites,
            stats.changed_sites,
            stats.nominal_over_white_sites,
            stats.affected_by_channel[0],
            stats.affected_by_channel[1],
            stats.affected_by_channel[2],
        ),
        HighlightDiagnostics::LocalRatios(stats) => format!(
            "Highlight Local ratios: {} suspected, {} reconstructed, {} changed, {} fallback, {} fully unsupported (R {}, G {}, B {})",
            stats.suspected_clipped_sites,
            stats.reconstructed_sites,
            stats.changed_sites,
            stats.fallback_sites,
            stats.fully_unsupported_sites,
            stats.suspected_by_channel[0],
            stats.suspected_by_channel[1],
            stats.suspected_by_channel[2],
        ),
        HighlightDiagnostics::Opposed(stats) => format!(
            "Highlight Opposed: {} suspected, {} reconstructed, {} changed, {} fallback, {} fully unsupported (R {}, G {}, B {})",
            stats.suspected_clipped_sites,
            stats.reconstructed_sites,
            stats.changed_sites,
            stats.fallback_sites,
            stats.fully_unsupported_sites,
            stats.suspected_by_channel[0],
            stats.suspected_by_channel[1],
            stats.suspected_by_channel[2],
        ),
    };
    let optics_report = format!(
        "{}\nFalse colour: {:?} (algorithm v{})\nDefringe: {:?} (algorithm v{})\nNoise reduction: {:?} (algorithm v{})\nCapture sharpening: {:?} (algorithm v{}, {} iterations)",
        format_optics_report(&recipe.optics, result.optics_provenance.as_ref()),
        recipe.false_color,
        rohditor_core::FALSE_COLOR_ALGORITHM_VERSION,
        recipe.defringe,
        rohditor_core::DEFRINGE_ALGORITHM_VERSION,
        recipe.noise_reduction,
        rohditor_core::NOISE_REDUCTION_ALGORITHM_VERSION,
        recipe.capture_sharpening,
        rohditor_core::CAPTURE_SHARPENING_ALGORITHM_VERSION,
        rohditor_core::CAPTURE_SHARPENING_ITERATIONS
    );
    write_stdout(&format!(
        "Developed {}x{} {}-bit sRGB {}{} with {} demosaic to {} ({} bytes, {})\nRendering profile: {}{}\nOutput gamut policy: {}{}\nOutput gamut pixels: {} in gamut, {} compressed, {} limited, {} clipped fallback, {} invalid\n{}\n{}\n{}\nEstimated CPU buffer peak: {} MiB",
        report.width,
        report.height,
        report.bit_depth.bits(),
        export_settings.format.description(),
        format_quality(export_settings.format),
        arguments.demosaic.label(),
        output.display(),
        report.bytes_written,
        if report.metadata_embedded {
            "safe EXIF"
        } else {
            "no EXIF"
        },
        recipe.rendering.profile.display_name(),
        recipe
            .rendering
            .profile
            .process_version()
            .map_or_else(String::new, |version| format!(" v{version}")),
        OutputPolicy::from(arguments.output_gamut).display_name(),
        OutputPolicy::from(arguments.output_gamut)
            .algorithm_version()
            .map_or_else(String::new, |version| format!(" v{version}")),
        result.output_gamut_diagnostics.in_gamut_pixels,
        result.output_gamut_diagnostics.compressed_pixels,
        result.output_gamut_diagnostics.limited_pixels,
        result.output_gamut_diagnostics.clipped_fallback_pixels,
        result.output_gamut_diagnostics.invalid_pixels,
        format_stage_timings(decode_time, result.timings, encode_time),
        highlight_report,
        optics_report,
        bytes_to_mib(result.memory.estimated_peak_bytes),
    ))
}

fn optics_adjustments(arguments: &DevelopArguments) -> Result<OpticsAdjustments> {
    Ok(OpticsAdjustments {
        profile: parse_lens_profile(&arguments.lens_profile)?,
        distortion: !arguments.no_lens_distortion,
        vignetting: !arguments.no_lens_vignetting,
        chromatic_aberration: !arguments.no_lens_tca,
    })
}

/// Parse `START,END` degrees. Range checks are left to recipe validation.
fn parse_hue_range(value: &str) -> Result<[f32; 2], String> {
    let (start, end) = value
        .split_once(',')
        .ok_or_else(|| "expected START,END in degrees".to_owned())?;
    let parse = |text: &str| {
        text.trim()
            .parse::<f32>()
            .map_err(|error| error.to_string())
    };
    Ok([parse(start)?, parse(end)?])
}

fn parse_lens_profile(value: &str) -> Result<LensProfileSelection> {
    let value = value.trim();
    if value.is_empty() {
        bail!("--lens-profile cannot be empty");
    }
    if value.eq_ignore_ascii_case("off") {
        return Ok(LensProfileSelection::Off);
    }
    if value.eq_ignore_ascii_case("auto") || value.eq_ignore_ascii_case("automatic") {
        return Ok(LensProfileSelection::Automatic);
    }
    Ok(LensProfileSelection::Lensfun {
        profile_id: value.to_owned(),
    })
}

fn format_optics_report(
    adjustments: &OpticsAdjustments,
    provenance: Option<&rohditor_core::OpticsProvenance>,
) -> String {
    let Some(provenance) = provenance else {
        return "Lens correction: off".to_owned();
    };
    format!(
        "Lens profile: {} [{}] ({})\nOptics components: requested {} applied {}\nOptics database: {} {} (fingerprint {:016x})\nOptics fallback distance: {}\nOptics correction scale: {:.6}",
        provenance.profile.lens,
        provenance.profile.id,
        match adjustments.profile {
            LensProfileSelection::Automatic => "automatic",
            LensProfileSelection::Lensfun { .. } => "explicit",
            LensProfileSelection::Off => "off",
        },
        format_components(provenance.requested),
        format_components(provenance.applied),
        provenance.database.source,
        provenance.database.snapshot,
        provenance.database.fingerprint,
        if provenance.used_infinity_distance_fallback {
            "1000 m (infinity fallback)"
        } else {
            "capture distance"
        },
        provenance.scale,
    )
}

fn format_components(components: rohditor_core::CorrectionComponents) -> String {
    format!(
        "distortion={}, vignetting={}, tca={}",
        components.distortion, components.vignetting, components.chromatic_aberration
    )
}

fn validate_highlight_options(
    method: CliHighlightMethod,
    clip_threshold: Option<f32>,
    detection_threshold: Option<f32>,
) -> Result<()> {
    if clip_threshold.is_some() && !matches!(method, CliHighlightMethod::Clip) {
        bail!("--highlight-clip-threshold requires --highlight-reconstruction clip");
    }
    if detection_threshold.is_some()
        && !matches!(
            method,
            CliHighlightMethod::LocalRatios | CliHighlightMethod::Opposed
        )
    {
        bail!(
            "--highlight-detection-threshold requires --highlight-reconstruction local-ratios or opposed"
        );
    }
    for (name, value) in [
        ("--highlight-clip-threshold", clip_threshold),
        ("--highlight-detection-threshold", detection_threshold),
    ] {
        if let Some(value) = value
            && !HIGHLIGHT_THRESHOLD_RANGE.contains(value)
        {
            bail!(
                "{name} must be finite and within {}..={}",
                HIGHLIGHT_THRESHOLD_RANGE.minimum,
                HIGHLIGHT_THRESHOLD_RANGE.maximum
            );
        }
    }
    Ok(())
}

/// Read, version-check, and validate a quality-crop manifest.
pub(super) fn load_quality_manifest(manifest_path: &Path) -> Result<QualityCropManifest> {
    let manifest_bytes = fs::read(manifest_path)
        .with_context(|| format!("could not read crop manifest {}", manifest_path.display()))?;
    let manifest: QualityCropManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("could not parse crop manifest {}", manifest_path.display()))?;
    if manifest.schema_version != 1 {
        bail!(
            "unsupported quality-crop manifest schema {}; expected 1",
            manifest.schema_version
        );
    }
    if manifest.crops.is_empty() {
        bail!("quality-crop manifest contains no crops");
    }
    for crop in &manifest.crops {
        validate_quality_crop_spec(crop)?;
    }
    Ok(manifest)
}

fn quality_crops(
    manifest_path: &Path,
    corpus: &Path,
    output: &Path,
    demosaic: CliDemosaic,
    force: bool,
) -> Result<()> {
    let manifest = load_quality_manifest(manifest_path)?;

    fs::create_dir_all(output).with_context(|| {
        format!(
            "could not create crop output directory {}",
            output.display()
        )
    })?;
    let report_path = output.join("report.json");
    if !force && report_path.exists() {
        bail!(
            "output {} already exists; pass --force to replace the crop set",
            report_path.display()
        );
    }

    let decoder = RawlerDecoder::default();
    let algorithm: DemosaicAlgorithm = demosaic.into();
    let render_options = RenderOptions {
        raw_crop_policy: RawCropPolicy::Recommended,
        demosaic: algorithm,
        output_policy: OutputPolicy::ClipToSrgb,
    };
    let mut recipe = EditRecipe::default();
    recipe.rendering.profile = RenderingProfileSelection::NEUTRAL;
    recipe.geometry.orientation_override = Some(Orientation::Normal);
    let settings = ExportSettings {
        format: ExportFormat::Png {
            bit_depth: PngBitDepth::Eight,
        },
        dithering: DitherMode::None,
        metadata: ExportMetadataPolicy::None,
        overwrite: force,
        size: OutputSize::Original,
    };
    let mut source_reports = Vec::new();
    let mut artifacts = Vec::new();
    let mut rendered_sources = std::collections::HashSet::new();

    for requested in &manifest.crops {
        if !rendered_sources.insert(requested.source.clone()) {
            continue;
        }
        let source_path = corpus.join(&requested.source);
        let decode_started = Instant::now();
        let frame = decoder.decode(&source_path).with_context(|| {
            format!(
                "could not decode private quality source {}",
                source_path.display()
            )
        })?;
        let decode_time = decode_started.elapsed();
        let rendered = CpuPipeline::default()
            .render_export(
                &frame,
                &recipe,
                render_options,
                rohditor_core::OutputBitDepth::Eight,
                DitherMode::None,
                OutputSize::Original,
            )
            .with_context(|| format!("could not develop quality source {}", requested.source))?;
        let image = match &rendered.image {
            ExportImage::Rgb8(image) => image,
            ExportImage::Rgb16(_) => unreachable!("quality crops explicitly request 8-bit output"),
        };

        for crop in manifest
            .crops
            .iter()
            .filter(|candidate| candidate.source == requested.source)
        {
            let cropped = crop_display_image(image, crop)
                .with_context(|| format!("invalid crop {} for {}", crop.name, crop.source))?;
            let enlarged = nearest_neighbor_2x(&cropped)?;
            let stem = source_stem_for_artifact(&crop.source)?;
            let base = format!("{stem}--{}--{}", crop.name, algorithm.stable_name());
            let name_100 = format!("{base}--100.png");
            let name_200 = format!("{base}--200.png");
            export_image(
                &output.join(&name_100),
                &ExportImage::Rgb8(cropped),
                &frame.info,
                settings,
            )?;
            export_image(
                &output.join(&name_200),
                &ExportImage::Rgb8(enlarged),
                &frame.info,
                settings,
            )?;
            artifacts.push(QualityCropArtifact {
                source: crop.source.clone(),
                name: crop.name.clone(),
                category: crop.category.clone(),
                coordinates: [crop.x, crop.y, crop.width, crop.height],
                output_100_percent: name_100,
                output_200_percent: name_200,
            });
        }

        source_reports.push(QualitySourceReport {
            source: requested.source.clone(),
            source_identity: frame.info.source_identity,
            source_bits_per_sample: frame.info.source_bits_per_sample,
            decoded_dimensions: [frame.info.width, frame.info.height],
            developed_dimensions: [rendered.image.width(), rendered.image.height()],
            decode_ms: milliseconds(decode_time),
            timings_ms: rendered.timings.into(),
            estimated_peak_bytes: rendered.memory.estimated_peak_bytes,
        });
    }

    let report = QualityCropReport {
        schema_version: 1,
        pipeline_version: env!("CARGO_PKG_VERSION"),
        recipe: "neutral-as-shot-v1",
        crop_policy: "recommended",
        orientation: "unrotated-sensor-crop",
        algorithm: algorithm.stable_name(),
        reconstruction_policy: "full-crop-demosaic-no-preview-resampling",
        manifest: manifest_path.display().to_string(),
        sources: source_reports,
        crops: artifacts,
    };
    let report_json = serde_json::to_vec_pretty(&report).context("could not encode crop report")?;
    write_output_bytes(&report_path, &report_json, force)
        .with_context(|| format!("could not commit crop report {}", report_path.display()))?;
    write_stdout(&format!(
        "Generated {} named crop pairs from {} source(s) with {} demosaic in {}",
        report.crops.len(),
        report.sources.len(),
        algorithm.stable_name(),
        output.display()
    ))
}

fn verify_libraw(
    file: &Path,
    libraw_pgm: &Path,
    json: bool,
    max_error: u16,
    max_rmse: f64,
) -> Result<()> {
    if !max_rmse.is_finite() || max_rmse < 0.0 {
        bail!("--max-rmse must be finite and non-negative");
    }
    let frame = RawlerDecoder::default()
        .decode(file)
        .with_context(|| format!("could not decode {} with Rohditor", file.display()))?;
    let pgm_bytes = fs::read(libraw_pgm).with_context(|| {
        format!(
            "could not read LibRaw unprocessed mosaic {}",
            libraw_pgm.display()
        )
    })?;
    let pgm = parse_libraw_pgm(&pgm_bytes)?;
    if (pgm.width, pgm.height) != (frame.info.width, frame.info.height) {
        bail!(
            "LibRaw mosaic is {}x{}, but Rohditor decoded {}x{}",
            pgm.width,
            pgm.height,
            frame.info.width,
            frame.info.height
        );
    }
    if frame.row_stride != pgm.width {
        bail!(
            "Rohditor row stride {} does not match the packed LibRaw width {}",
            frame.row_stride,
            pgm.width
        );
    }

    let mut mismatched_samples = 0_usize;
    let mut maximum_absolute_error = 0_u16;
    let mut squared_error = 0.0_f64;
    let mut first_mismatch = None;
    for (index, (&rohditor, libraw_bytes)) in frame
        .mosaic
        .iter()
        .zip(pgm.pixels.as_chunks::<2>().0.iter())
        .enumerate()
    {
        let libraw = u16::from_be_bytes([libraw_bytes[0], libraw_bytes[1]]);
        let error = rohditor.abs_diff(libraw);
        if error != 0 {
            mismatched_samples += 1;
            maximum_absolute_error = maximum_absolute_error.max(error);
            first_mismatch.get_or_insert(LibRawMismatch {
                x: index % pgm.width,
                y: index / pgm.width,
                rohditor,
                libraw,
            });
        }
        squared_error += f64::from(error) * f64::from(error);
    }
    let compared_samples = frame.mosaic.len();
    let cfa = match &frame.info.photometric_interpretation {
        PhotometricInterpretation::Cfa { pattern } => pattern.name.clone(),
        other => format!("{other:?}"),
    };
    let root_mean_squared_error = (squared_error / compared_samples as f64).sqrt();
    let within_tolerance =
        maximum_absolute_error <= max_error && root_mean_squared_error <= max_rmse;
    let report = LibRawVerificationReport {
        schema_version: 1,
        rohditor_decoder: "rawler 0.7.2 adapter",
        independent_decoder: "LibRaw unprocessed_raw 0.21.5",
        source: file.display().to_string(),
        source_identity: frame.info.source_identity,
        source_bits_per_sample: frame.info.source_bits_per_sample,
        dimensions: [frame.info.width, frame.info.height],
        row_stride: frame.row_stride,
        cfa,
        recommended_crop: frame.info.crop_area,
        black_levels: frame.info.black_levels.values.clone(),
        white_levels: frame.info.white_levels.clone(),
        compared_samples,
        mismatched_samples,
        maximum_absolute_error,
        root_mean_squared_error,
        accepted_maximum_absolute_error: max_error,
        accepted_root_mean_squared_error: max_rmse,
        within_tolerance,
        first_mismatch,
    };
    if json {
        write_stdout(&serde_json::to_string_pretty(&report)?)?;
    } else {
        write_stdout(&format!(
            "Compared {} {}-bit sensor samples at {}x{} ({} CFA)\nMismatches: {} · maximum absolute error: {} · RMSE: {:.6} · tolerance: {} / {:.3} ({})\nCrop: {} · black: {:?} · white: {:?}",
            report.compared_samples,
            report
                .source_bits_per_sample
                .map_or_else(|| "unknown".to_owned(), |bits| bits.to_string()),
            report.dimensions[0],
            report.dimensions[1],
            report.cfa,
            report.mismatched_samples,
            report.maximum_absolute_error,
            report.root_mean_squared_error,
            report.accepted_maximum_absolute_error,
            report.accepted_root_mean_squared_error,
            if report.within_tolerance {
                "pass"
            } else {
                "fail"
            },
            format_rect(report.recommended_crop),
            report.black_levels,
            report.white_levels,
        ))?;
    }
    if !report.within_tolerance {
        bail!("Rohditor and LibRaw sensor mosaics exceed the accepted decoder tolerance");
    }
    Ok(())
}

struct ParsedPgm<'a> {
    width: usize,
    height: usize,
    pixels: &'a [u8],
}

fn parse_libraw_pgm(bytes: &[u8]) -> Result<ParsedPgm<'_>> {
    let mut cursor = 0_usize;
    let magic = next_pgm_token(bytes, &mut cursor)?;
    if magic != b"P5" {
        bail!("LibRaw mosaic must be a binary P5 PGM");
    }
    let width = parse_pgm_usize(next_pgm_token(bytes, &mut cursor)?, "width")?;
    let height = parse_pgm_usize(next_pgm_token(bytes, &mut cursor)?, "height")?;
    let maximum = parse_pgm_usize(next_pgm_token(bytes, &mut cursor)?, "maximum")?;
    if maximum != usize::from(u16::MAX) {
        bail!("LibRaw PGM maximum is {maximum}; expected 65535");
    }
    let expected = width
        .checked_mul(height)
        .and_then(|samples| samples.checked_mul(2))
        .context("LibRaw PGM dimensions overflowed")?;
    let pixels = bytes
        .get(cursor..)
        .context("LibRaw PGM has no pixel payload")?;
    if pixels.len() != expected {
        bail!(
            "LibRaw PGM contains {} pixel bytes; expected {expected}",
            pixels.len()
        );
    }
    Ok(ParsedPgm {
        width,
        height,
        pixels,
    })
}

fn next_pgm_token<'a>(bytes: &'a [u8], cursor: &mut usize) -> Result<&'a [u8]> {
    loop {
        while bytes.get(*cursor).is_some_and(u8::is_ascii_whitespace) {
            *cursor += 1;
        }
        if bytes.get(*cursor) != Some(&b'#') {
            break;
        }
        while bytes.get(*cursor).is_some_and(|byte| *byte != b'\n') {
            *cursor += 1;
        }
    }
    let start = *cursor;
    while bytes
        .get(*cursor)
        .is_some_and(|byte| !byte.is_ascii_whitespace())
    {
        *cursor += 1;
    }
    if start == *cursor {
        bail!("LibRaw PGM header ended before all fields were read");
    }
    let token = &bytes[start..*cursor];
    match bytes.get(*cursor) {
        Some(b'\r') if bytes.get(*cursor + 1) == Some(&b'\n') => *cursor += 2,
        Some(_) => *cursor += 1,
        None => bail!("LibRaw PGM header is missing its payload separator"),
    }
    Ok(token)
}

fn parse_pgm_usize(token: &[u8], field: &str) -> Result<usize> {
    std::str::from_utf8(token)
        .with_context(|| format!("LibRaw PGM {field} is not ASCII"))?
        .parse()
        .with_context(|| format!("LibRaw PGM {field} is not an integer"))
}

fn validate_quality_crop_spec(crop: &QualityCropSpec) -> Result<()> {
    let source = Path::new(&crop.source);
    if source.file_name().and_then(|name| name.to_str()) != Some(crop.source.as_str()) {
        bail!(
            "quality source must be one plain file name: {:?}",
            crop.source
        );
    }
    if crop.name.is_empty()
        || !crop
            .name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!(
            "crop name {:?} must use lowercase ASCII letters, digits, and hyphens",
            crop.name
        );
    }
    if crop.category.trim().is_empty() {
        bail!("crop {} has an empty quality category", crop.name);
    }
    if crop.width == 0 || crop.height == 0 {
        bail!("crop {} must have non-zero dimensions", crop.name);
    }
    Ok(())
}

pub(super) fn source_stem_for_artifact(source: &str) -> Result<&str> {
    Path::new(source)
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("quality source {source:?} has no UTF-8 file stem"))
}

pub(super) fn crop_display_image(
    image: &DisplayRgbImage<u8>,
    crop: &QualityCropSpec,
) -> Result<DisplayRgbImage<u8>> {
    let end_x = crop
        .x
        .checked_add(crop.width)
        .context("crop x overflowed")?;
    let end_y = crop
        .y
        .checked_add(crop.height)
        .context("crop y overflowed")?;
    if end_x > image.width() || end_y > image.height() {
        bail!(
            "crop [{}, {}, {}, {}] exceeds developed image {}x{}",
            crop.x,
            crop.y,
            crop.width,
            crop.height,
            image.width(),
            image.height()
        );
    }
    let row_stride = crop
        .width
        .checked_mul(3)
        .context("crop stride overflowed")?;
    let mut pixels = Vec::with_capacity(
        row_stride
            .checked_mul(crop.height)
            .context("crop size overflowed")?,
    );
    for y in crop.y..end_y {
        let start = y * image.row_stride() + crop.x * 3;
        pixels.extend_from_slice(&image.data()[start..start + row_stride]);
    }
    Ok(DisplayRgbImage::new(
        crop.width,
        crop.height,
        row_stride,
        image.transfer(),
        pixels,
    )?)
}

fn nearest_neighbor_2x(image: &DisplayRgbImage<u8>) -> Result<DisplayRgbImage<u8>> {
    let width = image
        .width()
        .checked_mul(2)
        .context("200% width overflowed")?;
    let height = image
        .height()
        .checked_mul(2)
        .context("200% height overflowed")?;
    let row_stride = width.checked_mul(3).context("200% stride overflowed")?;
    let mut pixels = Vec::with_capacity(
        row_stride
            .checked_mul(height)
            .context("200% size overflowed")?,
    );
    for y in 0..image.height() {
        let source_row =
            &image.data()[y * image.row_stride()..y * image.row_stride() + image.width() * 3];
        for _ in 0..2 {
            for pixel in source_row.as_chunks::<3>().0 {
                pixels.extend_from_slice(pixel);
                pixels.extend_from_slice(pixel);
            }
        }
    }
    Ok(DisplayRgbImage::new(
        width,
        height,
        row_stride,
        DisplayTransfer::Srgb,
        pixels,
    )?)
}

pub(super) fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

/// Parse `WxH` (e.g. `2048x1365`) with both sides at least one pixel.
fn parse_fit_box(value: &str) -> Result<(u32, u32), String> {
    let (width, height) = value
        .split_once(['x', 'X'])
        .ok_or_else(|| "expected WIDTHxHEIGHT, e.g. 2048x1365".to_owned())?;
    let parse = |part: &str| {
        part.trim()
            .parse::<u32>()
            .ok()
            .filter(|&pixels| pixels > 0)
            .ok_or_else(|| format!("{part:?} is not a positive pixel count"))
    };
    Ok((parse(width)?, parse(height)?))
}

fn develop_export_settings(output: &Path, arguments: &DevelopArguments) -> Result<ExportSettings> {
    let extension = output
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let format = if extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg")
    {
        if arguments.png_bit_depth.is_some() {
            bail!("--png-bit-depth can only be used with a .png destination");
        }
        ExportFormat::Jpeg {
            quality: arguments.jpeg_quality.unwrap_or(JPEG_QUALITY_DEFAULT),
        }
    } else if extension.eq_ignore_ascii_case("png") {
        if arguments.jpeg_quality.is_some() {
            bail!("--jpeg-quality can only be used with a .jpg or .jpeg destination");
        }
        ExportFormat::Png {
            bit_depth: arguments
                .png_bit_depth
                .unwrap_or(CliPngBitDepth::Eight)
                .into(),
        }
    } else {
        bail!(
            "developed output {} must use a .jpg, .jpeg, or .png extension",
            output.display()
        );
    };
    let settings = ExportSettings {
        format,
        dithering: if arguments.dither {
            DitherMode::Ordered8x8
        } else {
            DitherMode::None
        },
        metadata: arguments.metadata.into(),
        overwrite: arguments.force,
        size: match (arguments.long_edge, arguments.fit) {
            (Some(pixels), _) => OutputSize::LongEdge { pixels },
            (None, Some((width, height))) => OutputSize::Fit { width, height },
            (None, None) => OutputSize::Original,
        },
    };
    settings
        .validate_destination(output)
        .context("could not validate export settings")?;
    Ok(settings)
}

fn format_quality(format: ExportFormat) -> String {
    match format {
        ExportFormat::Jpeg { quality } => format!(" (quality {quality})"),
        ExportFormat::Png { .. } => String::new(),
    }
}

fn format_stage_timings(decode: Duration, timings: StageTimings, encode: Duration) -> String {
    format!(
        "CPU stages: decode {:.1} ms, metadata {:.1} ms, normalize {:.1} ms, highlight processing {:.1} ms, demosaic {:.1} ms, false colour {:.1} ms, chroma NR {:.1} ms, luminance NR {:.1} ms, capture sharpening {:.1} ms, optics {:.1} ms, defringe {:.1} ms, area resize {:.1} ms, color {:.1} ms, adjustments {:.1} ms, output {:.1} ms, pipeline total {:.1} ms, export encode/commit {:.1} ms",
        decode.as_secs_f64() * 1_000.0,
        timings.metadata.as_secs_f64() * 1_000.0,
        timings.normalization.as_secs_f64() * 1_000.0,
        timings.highlight_processing.as_secs_f64() * 1_000.0,
        timings.demosaic.as_secs_f64() * 1_000.0,
        timings.false_color.as_secs_f64() * 1_000.0,
        timings.chroma_noise_reduction.as_secs_f64() * 1_000.0,
        timings.luminance_noise_reduction.as_secs_f64() * 1_000.0,
        timings.capture_sharpening.as_secs_f64() * 1_000.0,
        timings.optics.as_secs_f64() * 1_000.0,
        timings.defringe.as_secs_f64() * 1_000.0,
        timings.resampling.as_secs_f64() * 1_000.0,
        timings.color_conversion.as_secs_f64() * 1_000.0,
        timings.adjustments.as_secs_f64() * 1_000.0,
        timings.output_conversion.as_secs_f64() * 1_000.0,
        timings.total.as_secs_f64() * 1_000.0,
        encode.as_secs_f64() * 1_000.0,
    )
}

fn bytes_to_mib(bytes: usize) -> usize {
    bytes.div_ceil(1024 * 1024)
}

fn validate_preview_extension(output: &Path, format: EncodedPreviewFormat) -> Result<()> {
    let extension = output.extension().and_then(|value| value.to_str());
    if extension.is_some_and(|extension| format.accepts_extension(extension)) {
        return Ok(());
    }

    bail!(
        "preview output {} must use a .{} extension for {} data",
        output.display(),
        format.extension(),
        format.media_type()
    )
}

pub(super) fn write_stdout(value: &str) -> Result<()> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    match writeln!(handle, "{value}") {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error).context("could not write command output"),
    }
}

fn human_readable(output: &InspectionOutput<'_>) -> String {
    let info = output.metadata;
    let mut text = String::new();
    let _ = writeln!(text, "File:              {}", output.file);
    let _ = writeln!(text, "Format:            {}", info.format);
    let _ = writeln!(text, "Camera:            {} {}", info.make, info.model);
    let _ = writeln!(
        text,
        "Normalized camera: {} {}",
        info.clean_make, info.clean_model
    );
    let _ = writeln!(text, "File size:         {} bytes", info.source_size_bytes);
    let _ = writeln!(text, "Raw dimensions:    {}x{}", info.width, info.height);
    let _ = writeln!(
        text,
        "Source precision:  {}",
        format_option(info.source_bits_per_sample)
    );
    let _ = writeln!(
        text,
        "Decoded samples:   {} bit, {} component(s) per pixel",
        info.decoded_bits_per_sample, info.components_per_pixel
    );
    let _ = writeln!(
        text,
        "Compression:       {}",
        info.compression.as_deref().unwrap_or("unknown")
    );
    let _ = writeln!(text, "Active area:       {}", format_rect(info.active_area));
    let _ = writeln!(text, "Recommended crop:  {}", format_rect(info.crop_area));
    let _ = writeln!(
        text,
        "CFA/photometric:   {}",
        format_photometric(&info.photometric_interpretation)
    );
    let _ = writeln!(
        text,
        "Black levels:      {:?} (repeat {}x{}, cpp {})",
        info.black_levels.values,
        info.black_levels.repeat_width,
        info.black_levels.repeat_height,
        info.black_levels.components_per_pixel
    );
    let _ = writeln!(text, "White levels:      {:?}", info.white_levels);
    let _ = writeln!(
        text,
        "As-shot WB:        {}",
        format_optional_floats(&info.as_shot_white_balance)
    );
    let _ = writeln!(text, "Orientation:       {}", info.orientation);
    let _ = writeln!(
        text,
        "ISO:               {}",
        format_option(info.capture.iso)
    );
    let _ = writeln!(
        text,
        "Exposure time:     {}",
        info.capture
            .exposure_time
            .map_or_else(|| "unknown".to_owned(), |value| format!("{value} s"))
    );
    let _ = writeln!(
        text,
        "Aperture:          {}",
        info.capture.aperture.map_or_else(
            || "unknown".to_owned(),
            |value| value
                .as_f64()
                .map_or_else(|| value.to_string(), |number| format!("f/{number:.1}"))
        )
    );
    let _ = writeln!(
        text,
        "Focal length:      {}",
        info.capture.focal_length.map_or_else(
            || "unknown".to_owned(),
            |value| value
                .as_f64()
                .map_or_else(|| value.to_string(), |number| format!("{number:.1} mm"))
        )
    );
    let _ = writeln!(
        text,
        "Captured at:       {}",
        info.capture.captured_at.as_deref().unwrap_or("unknown")
    );
    let _ = writeln!(
        text,
        "Lens:              {}",
        info.capture.lens_model.as_deref().unwrap_or("unknown")
    );
    let _ = writeln!(
        text,
        "Embedded preview:  {}",
        info.embedded_preview.as_ref().map_or_else(
            || "none".to_owned(),
            |preview| format!(
                "{}x{}, {}",
                preview.width, preview.height, preview.color_type
            )
        )
    );
    let _ = writeln!(
        text,
        "Color matrices:    {} calibration matrix/matrices",
        info.color_matrices.len()
    );
    for matrix in &info.color_matrices {
        let _ = writeln!(text, "  {}: {:?}", matrix.illuminant, matrix.values);
    }
    let _ = writeln!(text, "XYZ to camera:     {:?}", info.xyz_to_camera);
    let _ = write!(
        text,
        "Sensor decoded:    {}",
        output.decoded_sample_count.map_or_else(
            || "no (metadata-only)".to_owned(),
            |count| format!("yes, {count} samples")
        )
    );
    text
}

fn format_rect(rect: Option<ImageRect>) -> String {
    rect.map_or_else(
        || "none".to_owned(),
        |rect| format!("{}x{} at {},{}", rect.width, rect.height, rect.x, rect.y),
    )
}

fn format_photometric(value: &PhotometricInterpretation) -> String {
    match value {
        PhotometricInterpretation::Cfa { pattern } => format!(
            "CFA {} ({}x{} repeat)",
            pattern.name, pattern.width, pattern.height
        ),
        PhotometricInterpretation::LinearRaw => "linear RAW".to_owned(),
        PhotometricInterpretation::BlackIsZero => "black-is-zero".to_owned(),
    }
}

fn format_optional_floats(values: &[Option<f32>]) -> String {
    values
        .iter()
        .map(|value| value.map_or_else(|| "n/a".to_owned(), |number| format!("{number:.6}")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_option<T: ToString>(value: Option<T>) -> String {
    value.map_or_else(|| "unknown".to_owned(), |value| value.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::str::FromStr;
    use std::time::{SystemTime, UNIX_EPOCH};

    use clap::Parser;
    use rohditor_image::{DisplayRgbImage, DisplayTransfer};
    use rohditor_raw::{CfaPattern, EncodedPreviewFormat, PhotometricInterpretation};

    use super::{
        Cli, CliCropPolicy, CliDemosaic, CliHighlightMethod, CliMetadata, CliOutputGamut,
        CliRenderingProfile, Command, DemosaicAlgorithm, DevelopArguments, QualityCropSpec,
        RgbMultipliers, WhiteBalance, crop_display_image, develop_export_settings, extract_preview,
        format_photometric, nearest_neighbor_2x, parse_lens_profile, parse_libraw_pgm,
        validate_highlight_options, validate_preview_extension, white_balance_from_arguments,
    };

    #[test]
    fn cfa_format_includes_pattern_and_dimensions() {
        let value = PhotometricInterpretation::Cfa {
            pattern: CfaPattern {
                name: "RGGB".to_owned(),
                width: 2,
                height: 2,
            },
        };

        assert_eq!(format_photometric(&value), "CFA RGGB (2x2 repeat)");
    }

    #[test]
    fn preview_extension_must_match_its_encoding() {
        assert!(
            validate_preview_extension(Path::new("preview.JPEG"), EncodedPreviewFormat::Jpeg)
                .is_ok()
        );
        assert!(
            validate_preview_extension(Path::new("preview.png"), EncodedPreviewFormat::Jpeg)
                .is_err()
        );
    }

    #[test]
    fn develop_selects_export_format_and_parses_three_relative_wb_values() {
        let base = DevelopArguments {
            processor: super::ExportProcessor::Cpu,
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            tone_shadows: 0.0,
            tone_darks: 0.0,
            tone_lights: 0.0,
            tone_highlights: 0.0,
            saturation: 1.0,
            vibrance: 0.0,
            highlight_reconstruction: CliHighlightMethod::Off,
            highlight_clip_threshold: None,
            highlight_detection_threshold: None,
            white_balance: None,
            camera_profile: None,
            rendering_profile: CliRenderingProfile::Standard,
            false_color_steps: 0,
            defringe_purple: 0.0,
            defringe_purple_hue: [270.0, 330.0],
            defringe_green: 0.0,
            defringe_green_hue: [75.0, 150.0],
            chroma_nr: 0.0,
            luma_nr: 0.0,
            luma_nr_detail: 0.5,
            capture_sharpening: false,
            capture_amount: 0.5,
            capture_radius: 0.6,
            capture_noise_protection: 0.5,
            output_gamut: CliOutputGamut::Clip,
            temperature: None,
            tint: 0.0,
            crop: CliCropPolicy::Recommended,
            demosaic: CliDemosaic::Bilinear,
            lens_profile: "off".to_owned(),
            no_lens_distortion: false,
            no_lens_vignetting: false,
            no_lens_tca: false,
            orientation: None,
            jpeg_quality: None,
            png_bit_depth: None,
            long_edge: None,
            fit: None,
            dither: false,
            metadata: CliMetadata::Safe,
            force: false,
        };
        assert!(develop_export_settings(Path::new("developed.PNG"), &base).is_ok());
        assert!(develop_export_settings(Path::new("developed.jpg"), &base).is_ok());
        assert!(develop_export_settings(Path::new("developed.tiff"), &base).is_err());

        let values = RgbMultipliers::from_str("1.2,1.0,0.8").expect("valid multipliers");
        assert_eq!((values.red, values.green, values.blue), (1.2, 1.0, 0.8));
        assert!(RgbMultipliers::from_str("1.0,1.0").is_err());

        let mut custom = base.clone();
        custom.temperature = Some(4_500.0);
        assert_eq!(
            white_balance_from_arguments(
                &custom,
                Some(rohditor_core::WhiteBalanceCoordinates {
                    temperature: 5_800.0,
                    tint: -0.15,
                }),
            ),
            WhiteBalance::TemperatureTint {
                temperature: 4_500.0,
                tint: -0.15,
            }
        );
        custom.temperature = None;
        custom.tint = 0.2;
        assert_eq!(
            white_balance_from_arguments(
                &custom,
                Some(rohditor_core::WhiteBalanceCoordinates {
                    temperature: 5_800.0,
                    tint: -0.15,
                }),
            ),
            WhiteBalance::TemperatureTint {
                temperature: 5_800.0,
                tint: 0.2,
            }
        );
    }

    #[test]
    fn develop_accepts_the_stable_mhc_cli_value() {
        let parsed = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--demosaic",
            "mhc",
        ])
        .expect("mhc is a supported development algorithm");
        let Command::Develop { demosaic, .. } = parsed.command else {
            panic!("expected develop command");
        };
        assert!(matches!(demosaic, CliDemosaic::MalvarHeCutler));

        let rcd = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--demosaic",
            "rcd",
        ])
        .expect("rcd is a supported development algorithm");
        let Command::Develop { demosaic, .. } = rcd.command else {
            panic!("expected develop command");
        };
        assert!(matches!(demosaic, CliDemosaic::Rcd));
        assert_eq!(DemosaicAlgorithm::from(demosaic), DemosaicAlgorithm::Rcd);

        let amaze = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--demosaic",
            "amaze",
        ])
        .expect("amaze is a supported development algorithm");
        let Command::Develop { demosaic, .. } = amaze.command else {
            panic!("expected develop command");
        };
        assert!(matches!(demosaic, CliDemosaic::Amaze));
        assert_eq!(DemosaicAlgorithm::from(demosaic), DemosaicAlgorithm::Amaze);

        let defaulted = Cli::try_parse_from(["rohditor-cli", "develop", "input.arw", "output.jpg"])
            .expect("development defaults parse");
        let Command::Develop {
            demosaic,
            highlight_reconstruction,
            ..
        } = defaulted.command
        else {
            panic!("expected develop command");
        };
        assert!(matches!(demosaic, CliDemosaic::Amaze));
        assert!(matches!(highlight_reconstruction, CliHighlightMethod::Clip));
    }

    #[test]
    fn develop_carries_a_camera_profile_path_without_installing_it() {
        let parsed = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--camera-profile",
            "studio.dcp",
        ])
        .expect("camera profile option should parse");
        let Command::Develop { camera_profile, .. } = parsed.command else {
            panic!("expected develop command");
        };
        assert_eq!(camera_profile, Some(PathBuf::from("studio.dcp")));
    }

    #[test]
    fn develop_parses_capture_controls_and_defaults_off() {
        let defaults = Cli::try_parse_from(["rohditor-cli", "develop", "photo.ARW", "out.png"])
            .expect("default CLI");
        let Command::Develop {
            capture_sharpening, ..
        } = defaults.command
        else {
            panic!("develop command");
        };
        assert!(!capture_sharpening);
        let parsed = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "photo.ARW",
            "out.png",
            "--capture-sharpening",
            "--capture-amount",
            "0.7",
            "--capture-radius",
            "0.8",
            "--capture-noise-protection",
            "0.3",
        ])
        .expect("capture CLI");
        let Command::Develop {
            capture_sharpening,
            capture_amount,
            capture_radius,
            capture_noise_protection,
            ..
        } = parsed.command
        else {
            panic!("develop command");
        };
        assert!(capture_sharpening);
        assert_eq!(
            (capture_amount, capture_radius, capture_noise_protection),
            (0.7, 0.8, 0.3)
        );
    }

    #[test]
    fn develop_parses_rendering_profile_and_defaults_to_matched() {
        let defaulted = Cli::try_parse_from(["rohditor-cli", "develop", "input.arw", "output.jpg"])
            .expect("default rendering should parse");
        let Command::Develop {
            rendering_profile, ..
        } = defaulted.command
        else {
            panic!("expected develop command");
        };
        assert!(matches!(rendering_profile, CliRenderingProfile::Matched));

        let neutral = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--rendering-profile",
            "neutral",
        ])
        .expect("neutral rendering should parse");
        let Command::Develop {
            rendering_profile, ..
        } = neutral.command
        else {
            panic!("expected develop command");
        };
        assert!(matches!(rendering_profile, CliRenderingProfile::Neutral));
    }

    #[test]
    fn develop_parses_output_gamut_and_defaults_to_clip() {
        let defaulted = Cli::try_parse_from(["rohditor-cli", "develop", "input.arw", "output.jpg"])
            .expect("default output gamut should parse");
        let Command::Develop { output_gamut, .. } = defaulted.command else {
            panic!("expected develop command");
        };
        assert!(matches!(output_gamut, CliOutputGamut::Clip));

        let compressed = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--output-gamut",
            "chroma-compress",
        ])
        .expect("chroma compression should parse");
        let Command::Develop { output_gamut, .. } = compressed.command else {
            panic!("expected develop command");
        };
        assert!(matches!(output_gamut, CliOutputGamut::ChromaCompress));
    }

    #[test]
    fn lens_profile_values_and_component_flags_parse() {
        assert!(matches!(
            parse_lens_profile("off").expect("off should parse"),
            rohditor_edit::LensProfileSelection::Off
        ));
        assert!(matches!(
            parse_lens_profile("AUTO").expect("auto should parse"),
            rohditor_edit::LensProfileSelection::Automatic
        ));
        let explicit =
            parse_lens_profile("lensfun:sony:profile").expect("opaque profile IDs should parse");
        assert!(matches!(
            explicit,
            rohditor_edit::LensProfileSelection::Lensfun { profile_id }
                if profile_id == "lensfun:sony:profile"
        ));
        assert!(parse_lens_profile(" ").is_err());

        let parsed = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--lens-profile",
            "auto",
            "--no-lens-distortion",
            "--no-lens-tca",
        ])
        .expect("lens profile flags should parse");
        let Command::Develop {
            lens_profile,
            no_lens_distortion,
            no_lens_vignetting,
            no_lens_tca,
            ..
        } = parsed.command
        else {
            panic!("expected develop command");
        };
        assert_eq!(lens_profile, "auto");
        assert!(no_lens_distortion);
        assert!(!no_lens_vignetting);
        assert!(no_lens_tca);
    }

    #[test]
    fn highlight_cli_options_require_clip_and_validate_the_threshold() {
        let parsed = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--highlight-reconstruction",
            "clip",
            "--highlight-clip-threshold",
            "1.25",
        ])
        .expect("clip threshold options should parse");
        let Command::Develop {
            highlight_reconstruction,
            highlight_clip_threshold,
            highlight_detection_threshold,
            ..
        } = parsed.command
        else {
            panic!("expected develop command");
        };
        assert!(matches!(highlight_reconstruction, CliHighlightMethod::Clip));
        assert_eq!(highlight_clip_threshold, Some(1.25));
        assert_eq!(highlight_detection_threshold, None);
        assert!(
            validate_highlight_options(highlight_reconstruction, highlight_clip_threshold, None)
                .is_ok()
        );
        assert!(validate_highlight_options(CliHighlightMethod::Off, Some(1.0), None).is_err());
        assert!(validate_highlight_options(CliHighlightMethod::Clip, Some(2.0), None).is_err());
        assert!(
            validate_highlight_options(CliHighlightMethod::Clip, Some(f32::NAN), None).is_err()
        );

        let local = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--highlight-reconstruction",
            "local-ratios",
            "--highlight-detection-threshold",
            "0.75",
        ])
        .expect("local-ratios threshold options should parse");
        let Command::Develop {
            highlight_reconstruction,
            highlight_clip_threshold,
            highlight_detection_threshold,
            ..
        } = local.command
        else {
            panic!("expected develop command");
        };
        assert!(matches!(
            highlight_reconstruction,
            CliHighlightMethod::LocalRatios
        ));
        assert_eq!(highlight_clip_threshold, None);
        assert_eq!(highlight_detection_threshold, Some(0.75));
        assert!(
            validate_highlight_options(
                highlight_reconstruction,
                highlight_clip_threshold,
                highlight_detection_threshold,
            )
            .is_ok()
        );

        let opposed = Cli::try_parse_from([
            "rohditor-cli",
            "develop",
            "input.arw",
            "output.jpg",
            "--highlight-reconstruction",
            "opposed",
            "--highlight-detection-threshold",
            "0.8",
        ])
        .expect("Opposed threshold options should parse");
        let Command::Develop {
            highlight_reconstruction,
            highlight_detection_threshold,
            ..
        } = opposed.command
        else {
            panic!("expected develop command");
        };
        assert!(matches!(
            highlight_reconstruction,
            CliHighlightMethod::Opposed
        ));
        assert_eq!(highlight_detection_threshold, Some(0.8));
        assert!(
            validate_highlight_options(
                highlight_reconstruction,
                None,
                highlight_detection_threshold,
            )
            .is_ok()
        );
    }

    #[test]
    fn quality_crops_default_to_amaze() {
        let parsed = Cli::try_parse_from([
            "rohditor-cli",
            "quality-crops",
            "crops.json",
            "private",
            "generated",
        ])
        .expect("quality crop command parses");
        let Command::QualityCrops { demosaic, .. } = parsed.command else {
            panic!("expected quality-crops command");
        };
        assert!(matches!(demosaic, CliDemosaic::Amaze));
    }

    #[test]
    fn quality_crop_and_pixel_enlargement_preserve_exact_samples() {
        let image = DisplayRgbImage::new(
            3,
            2,
            9,
            DisplayTransfer::Srgb,
            vec![
                1, 2, 3, 4, 5, 6, 7, 8, 9, // row 0
                10, 11, 12, 13, 14, 15, 16, 17, 18, // row 1
            ],
        )
        .expect("valid display image");
        let spec = QualityCropSpec {
            source: "sample.ARW".to_owned(),
            name: "detail".to_owned(),
            category: "fine detail".to_owned(),
            x: 1,
            y: 0,
            width: 2,
            height: 2,
        };
        let crop = crop_display_image(&image, &spec).expect("crop succeeds");
        assert_eq!(crop.data(), &[4, 5, 6, 7, 8, 9, 13, 14, 15, 16, 17, 18]);
        let enlarged = nearest_neighbor_2x(&crop).expect("enlargement succeeds");
        assert_eq!((enlarged.width(), enlarged.height()), (4, 4));
        assert_eq!(enlarged.pixel(0, 0), enlarged.pixel(1, 0));
        assert_eq!(enlarged.pixel(0, 0), enlarged.pixel(0, 1));
        assert_eq!(enlarged.pixel(2, 2), Some(&[16, 17, 18][..]));
    }

    #[test]
    fn libraw_pgm_parser_preserves_big_endian_sensor_bytes() {
        let pgm = parse_libraw_pgm(b"P5\n# LibRaw fixture\n2 1\n65535\n\x01\x02\xfe\xff")
            .expect("valid 16-bit PGM");
        assert_eq!((pgm.width, pgm.height), (2, 1));
        assert_eq!(pgm.pixels, &[1, 2, 254, 255]);
        assert!(parse_libraw_pgm(b"P5\n2 1\n255\n\x01\x02").is_err());
    }

    #[test]
    fn preview_extraction_rejects_a_hard_link_to_its_source_before_decoding() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "preview-alias-test-{}-{unique}",
                std::process::id()
            ));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("source.ARW");
        let destination = directory.join("source-alias.jpg");
        fs::write(&source, b"sentinel RAW bytes").expect("write source sentinel");
        fs::hard_link(&source, &destination).expect("create hard link");

        let error = extract_preview(&source, &destination, true)
            .expect_err("source alias must be rejected before RAW decoding");
        assert!(error.to_string().contains("refusing to replace source RAW"));
        assert_eq!(
            fs::read(&source).expect("read preserved source"),
            b"sentinel RAW bytes"
        );

        fs::remove_dir_all(directory).expect("remove test directory");
    }
}
