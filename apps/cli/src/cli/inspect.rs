//! The `inspect` subcommand: normalized metadata, optionally with a decode.

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use rohditor_raw::{ImageRect, PhotometricInterpretation, RawDecoder, RawFileInfo, RawlerDecoder};
use serde::Serialize;

use super::output::write_stdout;

#[derive(Debug, Serialize)]
struct InspectionOutput<'a> {
    file: String,
    decoded: bool,
    decoded_sample_count: Option<usize>,
    metadata: &'a RawFileInfo,
}

pub(super) fn inspect(file: &Path, json: bool, metadata_only: bool) -> Result<()> {
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
    let capture = &info.capture;
    let labels = [
        ("Exposure time:     ", capture.exposure_time_label()),
        ("Aperture:          ", capture.aperture_label()),
        ("Focal length:      ", capture.focal_length_label()),
        ("Captured at:       ", capture.captured_at_label()),
        ("Lens:              ", capture.lens_label()),
    ];
    for (label, value) in labels {
        let _ = writeln!(text, "{label}{}", value.as_deref().unwrap_or("unknown"));
    }
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

pub(super) fn format_rect(rect: Option<ImageRect>) -> String {
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
    use rohditor_raw::CfaPattern;

    use super::*;

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
}
