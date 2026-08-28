use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use rohditor_raw::{
    EncodedPreviewFormat, ImageRect, PhotometricInterpretation, RawDecoder, RawFileInfo,
    RawlerDecoder,
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
    }
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
    let decoder = RawlerDecoder::default();
    let preview = decoder
        .embedded_preview(file)
        .with_context(|| format!("could not extract a preview from {}", file.display()))?;
    let Some(preview) = preview else {
        bail!("{} does not contain an embedded preview", file.display());
    };
    validate_preview_extension(output, preview.format)?;

    let mut options = OpenOptions::new();
    options.write(true);
    if force {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let mut destination = match options.open(output) {
        Ok(destination) => destination,
        Err(error) if !force && error.kind() == io::ErrorKind::AlreadyExists => {
            bail!(
                "output {} already exists; pass --force to replace it",
                output.display()
            );
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("could not create preview output {}", output.display()));
        }
    };
    destination
        .write_all(&preview.bytes)
        .with_context(|| format!("could not write preview output {}", output.display()))?;

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
    use std::path::Path;

    use rohditor_raw::{CfaPattern, EncodedPreviewFormat, PhotometricInterpretation};

    use super::{format_photometric, validate_preview_extension};

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
}
