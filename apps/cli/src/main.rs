use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use rohditor_core::{
    CONTRAST_RANGE, CpuPipeline, CropPolicy, DemosaicAlgorithm, DitherMode, EXPOSURE_EV_RANGE,
    EditRecipe, ExportFormat, ExportMetadataPolicy, ExportSettings, JPEG_QUALITY_DEFAULT,
    JPEG_QUALITY_MAX, JPEG_QUALITY_MIN, OutputPolicy, PngBitDepth, RenderOptions, SATURATION_RANGE,
    StageTimings, WhiteBalance, export_image, paths_refer_to_same_file, write_output_bytes,
};
use rohditor_raw::{
    EncodedPreviewFormat, ImageRect, PhotometricInterpretation, RawDecoder, RawFileInfo,
    RawOrientation, RawlerDecoder,
};
use serde::Serialize;
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
        #[arg(long, value_enum, default_value_t = CliDemosaic::Bilinear)]
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
}

impl From<CliDemosaic> for DemosaicAlgorithm {
    fn from(value: CliDemosaic) -> Self {
        match value {
            CliDemosaic::Bilinear => Self::Bilinear,
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
    }
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
        "Developed {}x{} {}-bit sRGB {}{} to {} ({} bytes, {})\n{}\nEstimated CPU buffer peak: {} MiB",
        report.width,
        report.height,
        report.bit_depth.bits(),
        export_settings.format.description(),
        format_quality(export_settings.format),
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
        "CPU stages: decode {:.1} ms, metadata {:.1} ms, normalize {:.1} ms, demosaic {:.1} ms, color {:.1} ms, adjustments {:.1} ms, output {:.1} ms, pipeline total {:.1} ms, export encode/commit {:.1} ms",
        decode.as_secs_f64() * 1_000.0,
        timings.metadata.as_secs_f64() * 1_000.0,
        timings.normalization.as_secs_f64() * 1_000.0,
        timings.demosaic.as_secs_f64() * 1_000.0,
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

    use rohditor_raw::{CfaPattern, EncodedPreviewFormat, PhotometricInterpretation};

    use super::{
        CliCropPolicy, CliDemosaic, CliMetadata, DevelopArguments, RgbMultipliers,
        develop_export_settings, extract_preview, format_photometric, validate_preview_extension,
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
