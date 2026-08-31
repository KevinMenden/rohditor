use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use rohditor_core::{
    CONTRAST_RANGE, CpuPipeline, CropPolicy, DemosaicAlgorithm, DisplayRgbImage, DisplayTransfer,
    DitherMode, EXPOSURE_EV_RANGE, EditRecipe, ExportFormat, ExportImage, ExportMetadataPolicy,
    ExportSettings, JPEG_QUALITY_DEFAULT, JPEG_QUALITY_MAX, JPEG_QUALITY_MIN, OutputPolicy,
    PngBitDepth, RenderOptions, SATURATION_RANGE, StageTimings, WhiteBalance, export_image,
    paths_refer_to_same_file, write_output_bytes,
};
use rohditor_raw::{
    EncodedPreviewFormat, ImageRect, PhotometricInterpretation, RawDecoder, RawFileInfo,
    RawOrientation, RawlerDecoder,
};
use serde::{Deserialize, Serialize};
use tracing_subscriber::EnvFilter;

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

        /// Rec.2020 luminance-relative saturation (0 to 2; 1 is neutral).
        #[arg(long, default_value_t = SATURATION_RANGE.neutral)]
        saturation: f32,

        /// R,G,B multipliers relative to the as-shot white balance.
        #[arg(long, value_name = "RED,GREEN,BLUE")]
        white_balance: Option<RgbMultipliers>,

        /// Sensor crop policy.
        #[arg(long, value_enum, default_value_t = CliCropPolicy::Recommended)]
        crop: CliCropPolicy,

        /// CPU demosaic algorithm.
        #[arg(long, value_enum, default_value_t = CliDemosaic::MalvarHeCutler)]
        demosaic: CliDemosaic,

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
        #[arg(long, value_enum, default_value_t = CliDemosaic::MalvarHeCutler)]
        demosaic: CliDemosaic,

        /// Replace existing generated crops and report.json.
        #[arg(long)]
        force: bool,
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

impl From<CliCropPolicy> for CropPolicy {
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
}

impl CliDemosaic {
    const fn label(self) -> &'static str {
        match self {
            Self::Bilinear => "bilinear",
            Self::MalvarHeCutler => "mhc",
        }
    }
}

impl From<CliDemosaic> for DemosaicAlgorithm {
    fn from(value: CliDemosaic) -> Self {
        match value {
            CliDemosaic::Bilinear => Self::Bilinear,
            CliDemosaic::MalvarHeCutler => Self::MalvarHeCutler,
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

impl From<CliOrientation> for RawOrientation {
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

fn main() -> Result<()> {
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
            saturation,
            white_balance,
            crop,
            demosaic,
            orientation,
            jpeg_quality,
            png_bit_depth,
            dither,
            metadata,
            force,
        } => develop(
            &file,
            &output,
            DevelopArguments {
                exposure,
                contrast,
                saturation,
                white_balance,
                crop,
                demosaic,
                orientation,
                jpeg_quality,
                png_bit_depth,
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
struct QualityCropManifest {
    schema_version: u32,
    crops: Vec<QualityCropSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QualityCropSpec {
    source: String,
    name: String,
    category: String,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
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
    demosaic: f64,
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
            demosaic: milliseconds(value.demosaic),
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

#[derive(Debug, Clone, Copy)]
struct DevelopArguments {
    exposure: f32,
    contrast: f32,
    saturation: f32,
    white_balance: Option<RgbMultipliers>,
    crop: CliCropPolicy,
    demosaic: CliDemosaic,
    orientation: Option<CliOrientation>,
    jpeg_quality: Option<u8>,
    png_bit_depth: Option<CliPngBitDepth>,
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

fn develop(file: &Path, output: &Path, arguments: DevelopArguments) -> Result<()> {
    let export_settings = develop_export_settings(output, arguments)?;
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

    let white_balance = arguments
        .white_balance
        .map_or(WhiteBalance::AsShot, |value| {
            WhiteBalance::ManualMultipliers {
                red: value.red,
                green: value.green,
                blue: value.blue,
            }
        });
    let recipe = EditRecipe {
        white_balance,
        exposure_ev: arguments.exposure,
        contrast: arguments.contrast,
        saturation: arguments.saturation,
        orientation_override: arguments.orientation.map(Into::into),
        ..EditRecipe::default()
    };
    recipe
        .validate()
        .context("could not validate the development recipe")?;

    let decoder = RawlerDecoder::default();
    let decode_started = Instant::now();
    let frame = decoder
        .decode(file)
        .with_context(|| format!("could not decode {} for development", file.display()))?;
    let decode_time = decode_started.elapsed();
    let result = CpuPipeline
        .render_export(
            &frame,
            &recipe,
            RenderOptions {
                crop_policy: arguments.crop.into(),
                demosaic: arguments.demosaic.into(),
                output_policy: OutputPolicy::ClipToSrgb,
            },
            export_settings.format.bit_depth(),
            export_settings.dithering,
        )
        .with_context(|| format!("could not develop {}", file.display()))?;
    let source_info = frame.info.clone();
    drop(frame);
    let encode_started = Instant::now();
    let report = export_image(output, &result.image, &source_info, export_settings)
        .with_context(|| format!("could not export developed image to {}", output.display()))?;
    let encode_time = encode_started.elapsed();

    write_stdout(&format!(
        "Developed {}x{} {}-bit sRGB {}{} with {} demosaic to {} ({} bytes, {})\n{}\nEstimated CPU buffer peak: {} MiB",
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
        format_stage_timings(decode_time, result.timings, encode_time),
        bytes_to_mib(result.memory.estimated_peak_bytes),
    ))
}

fn quality_crops(
    manifest_path: &Path,
    corpus: &Path,
    output: &Path,
    demosaic: CliDemosaic,
    force: bool,
) -> Result<()> {
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
        crop_policy: CropPolicy::Recommended,
        demosaic: algorithm,
        output_policy: OutputPolicy::ClipToSrgb,
    };
    let recipe = EditRecipe {
        orientation_override: Some(RawOrientation::Normal),
        ..EditRecipe::default()
    };
    let settings = ExportSettings {
        format: ExportFormat::Png {
            bit_depth: PngBitDepth::Eight,
        },
        dithering: DitherMode::None,
        metadata: ExportMetadataPolicy::None,
        overwrite: force,
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
        let rendered = CpuPipeline
            .render_export(
                &frame,
                &recipe,
                render_options,
                rohditor_core::OutputBitDepth::Eight,
                DitherMode::None,
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
        .zip(pgm.pixels.chunks_exact(2))
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

fn source_stem_for_artifact(source: &str) -> Result<&str> {
    Path::new(source)
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("quality source {source:?} has no UTF-8 file stem"))
}

fn crop_display_image(
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
            for pixel in source_row.chunks_exact(3) {
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

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn develop_export_settings(output: &Path, arguments: DevelopArguments) -> Result<ExportSettings> {
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
        "CPU stages: decode {:.1} ms, metadata {:.1} ms, normalize {:.1} ms, demosaic {:.1} ms, area resize {:.1} ms, color {:.1} ms, adjustments {:.1} ms, output {:.1} ms, pipeline total {:.1} ms, export encode/commit {:.1} ms",
        decode.as_secs_f64() * 1_000.0,
        timings.metadata.as_secs_f64() * 1_000.0,
        timings.normalization.as_secs_f64() * 1_000.0,
        timings.demosaic.as_secs_f64() * 1_000.0,
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

fn write_stdout(value: &str) -> Result<()> {
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
    use std::path::Path;
    use std::str::FromStr;
    use std::time::{SystemTime, UNIX_EPOCH};

    use clap::Parser;
    use rohditor_raw::{CfaPattern, EncodedPreviewFormat, PhotometricInterpretation};

    use super::{
        Cli, CliCropPolicy, CliDemosaic, CliMetadata, Command, DevelopArguments, QualityCropSpec,
        RgbMultipliers, crop_display_image, develop_export_settings, extract_preview,
        format_photometric, nearest_neighbor_2x, parse_libraw_pgm, validate_preview_extension,
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
            exposure: 0.0,
            contrast: 0.0,
            saturation: 1.0,
            white_balance: None,
            crop: CliCropPolicy::Recommended,
            demosaic: CliDemosaic::Bilinear,
            orientation: None,
            jpeg_quality: None,
            png_bit_depth: None,
            dither: false,
            metadata: CliMetadata::Safe,
            force: false,
        };
        assert!(develop_export_settings(Path::new("developed.PNG"), base).is_ok());
        assert!(develop_export_settings(Path::new("developed.jpg"), base).is_ok());
        assert!(develop_export_settings(Path::new("developed.tiff"), base).is_err());

        let values = RgbMultipliers::from_str("1.2,1.0,0.8").expect("valid multipliers");
        assert_eq!((values.red, values.green, values.blue), (1.2, 1.0, 0.8));
        assert!(RgbMultipliers::from_str("1.0,1.0").is_err());
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

        let defaulted = Cli::try_parse_from(["rohditor-cli", "develop", "input.arw", "output.jpg"])
            .expect("development defaults parse");
        let Command::Develop { demosaic, .. } = defaulted.command else {
            panic!("expected develop command");
        };
        assert!(matches!(demosaic, CliDemosaic::MalvarHeCutler));
    }

    #[test]
    fn quality_crops_default_to_mhc() {
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
        assert!(matches!(demosaic, CliDemosaic::MalvarHeCutler));
    }

    #[test]
    fn quality_crop_and_pixel_enlargement_preserve_exact_samples() {
        let image = rohditor_core::DisplayRgbImage::new(
            3,
            2,
            9,
            rohditor_core::DisplayTransfer::Srgb,
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
