//! The `verify-libraw` subcommand: compare the decoded mosaic with a LibRaw dump.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use rohditor_raw::{ImageRect, PhotometricInterpretation, RawDecoder, RawlerDecoder};
use serde::Serialize;

use super::inspect::format_rect;
use super::output::write_stdout;

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

pub(super) fn verify_libraw(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn libraw_pgm_parser_preserves_big_endian_sensor_bytes() {
        let pgm = parse_libraw_pgm(b"P5\n# LibRaw fixture\n2 1\n65535\n\x01\x02\xfe\xff")
            .expect("valid 16-bit PGM");
        assert_eq!((pgm.width, pgm.height), (2, 1));
        assert_eq!(pgm.pixels, &[1, 2, 254, 255]);
        assert!(parse_libraw_pgm(b"P5\n2 1\n255\n\x01\x02").is_err());
    }
}
