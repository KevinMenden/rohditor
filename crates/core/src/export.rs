//! File encoding boundary for quantized display results.
//!
//! The quantized [`ExportImage`] and format settings live in `types.rs` so the
//! CPU pipeline can produce a codec-independent result. This module owns only
//! metadata construction, JPEG/PNG encoding, and transactional destination
//! writes.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use rohditor_raw::{RationalValue, RawFileInfo};
use thiserror::Error;

use crate::output::write_transactionally;

mod icc;
mod sharpening;
mod types;

use icc::icc_profile;
pub use sharpening::*;
pub use types::*;

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("invalid export settings: {reason}")]
    InvalidSettings { reason: String },

    #[error(
        "export format requires {required}-bit RGB samples, but the rendered image contains {actual}-bit samples"
    )]
    WrongBitDepth { required: u8, actual: u8 },

    #[error("invalid export image layout: {reason}")]
    InvalidImageLayout { reason: String },

    #[error("output {path} already exists; explicitly enable overwrite to replace it")]
    AlreadyExists { path: PathBuf },

    #[error("could not {operation} export path {path}: {source}")]
    DestinationIo {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not configure {format} metadata: {reason}")]
    MetadataConfiguration {
        format: &'static str,
        reason: String,
    },

    #[error("could not encode {format} output {path}: {source}")]
    Encoding {
        format: &'static str,
        path: PathBuf,
        #[source]
        source: image::ImageError,
    },
}

/// Encode an already quantized display image, embedding the ICC profile of its color space, and commit it transactionally.
///
/// Pixels are first written to a uniquely created sibling file. The destination
/// is changed only after encoding, flushing, and file synchronization succeed.
pub fn export_image(
    destination: &Path,
    image: &ExportImage,
    source: &RawFileInfo,
    settings: ExportSettings,
) -> Result<ExportReport, ExportError> {
    settings.validate_destination(destination)?;
    validate_image(image, settings.format)?;

    let width = u32::try_from(image.width()).map_err(|_| ExportError::InvalidSettings {
        reason: format!(
            "output width {} does not fit in a file format",
            image.width()
        ),
    })?;
    let height = u32::try_from(image.height()).map_err(|_| ExportError::InvalidSettings {
        reason: format!(
            "output height {} does not fit in a file format",
            image.height()
        ),
    })?;
    let icc_profile = icc_profile(image.color_space());
    let exif = match settings.metadata {
        ExportMetadataPolicy::None => None,
        ExportMetadataPolicy::Safe => Some(safe_exif(source, width, height)),
    };

    let bytes_written = write_transactionally(destination, settings.overwrite, |writer| {
        encode(
            writer,
            destination,
            image,
            settings.format,
            width,
            height,
            icc_profile.clone(),
            exif.clone(),
        )
    })?;

    Ok(ExportReport {
        width: image.width(),
        height: image.height(),
        bit_depth: image.bit_depth(),
        bytes_written,
        metadata_embedded: exif.is_some(),
    })
}

fn validate_image(image: &ExportImage, format: ExportFormat) -> Result<(), ExportError> {
    // The color space is part of the image type and every `DisplayColorSpace`
    // has an embeddable ICC profile, so only layout and depth need checking.
    let required = format.bit_depth();
    let actual = image.bit_depth();
    if required != actual {
        return Err(ExportError::WrongBitDepth {
            required: required.bits(),
            actual: actual.bits(),
        });
    }
    let expected_stride =
        image
            .width()
            .checked_mul(3)
            .ok_or_else(|| ExportError::InvalidImageLayout {
                reason: "RGB row-stride calculation overflowed".to_owned(),
            })?;
    if image.row_stride() != expected_stride {
        return Err(ExportError::InvalidImageLayout {
            reason: format!(
                "encoders require tightly packed RGB rows, received stride {} instead of {expected_stride}",
                image.row_stride()
            ),
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn encode(
    writer: &mut dyn Write,
    destination: &Path,
    image: &ExportImage,
    format: ExportFormat,
    width: u32,
    height: u32,
    icc_profile: Vec<u8>,
    exif: Option<Vec<u8>>,
) -> Result<(), ExportError> {
    match (format, image) {
        (ExportFormat::Jpeg { quality }, ExportImage::Rgb8(image)) => {
            let mut encoder = JpegEncoder::new_with_quality(writer, quality);
            configure_metadata(&mut encoder, "JPEG", icc_profile, exif)?;
            encoder
                .write_image(image.data(), width, height, ExtendedColorType::Rgb8)
                .map_err(|source| ExportError::Encoding {
                    format: "JPEG",
                    path: destination.to_owned(),
                    source,
                })
        }
        (
            ExportFormat::Png {
                bit_depth: PngBitDepth::Eight,
            },
            ExportImage::Rgb8(image),
        ) => {
            let mut encoder = PngEncoder::new(writer);
            configure_metadata(&mut encoder, "PNG", icc_profile, exif)?;
            encoder
                .write_image(image.data(), width, height, ExtendedColorType::Rgb8)
                .map_err(|source| ExportError::Encoding {
                    format: "PNG",
                    path: destination.to_owned(),
                    source,
                })
        }
        (
            ExportFormat::Png {
                bit_depth: PngBitDepth::Sixteen,
            },
            ExportImage::Rgb16(image),
        ) => {
            let mut encoder = PngEncoder::new(writer);
            configure_metadata(&mut encoder, "PNG", icc_profile, exif)?;
            encoder
                .write_image(
                    bytemuck::cast_slice(image.data()),
                    width,
                    height,
                    ExtendedColorType::Rgb16,
                )
                .map_err(|source| ExportError::Encoding {
                    format: "PNG",
                    path: destination.to_owned(),
                    source,
                })
        }
        _ => Err(ExportError::WrongBitDepth {
            required: format.bit_depth().bits(),
            actual: image.bit_depth().bits(),
        }),
    }
}

fn configure_metadata<E: ImageEncoder>(
    encoder: &mut E,
    format: &'static str,
    icc_profile: Vec<u8>,
    exif: Option<Vec<u8>>,
) -> Result<(), ExportError> {
    encoder
        .set_icc_profile(icc_profile)
        .map_err(|error| ExportError::MetadataConfiguration {
            format,
            reason: error.to_string(),
        })?;
    if let Some(exif) = exif {
        encoder
            .set_exif_metadata(exif)
            .map_err(|error| ExportError::MetadataConfiguration {
                format,
                reason: error.to_string(),
            })?;
    }
    Ok(())
}

fn align_to_four(data: &mut Vec<u8>) {
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
}

fn safe_exif(source: &RawFileInfo, width: u32, height: u32) -> Vec<u8> {
    const TYPE_ASCII: u16 = 2;
    const TYPE_SHORT: u16 = 3;
    const TYPE_LONG: u16 = 4;
    const TYPE_RATIONAL: u16 = 5;

    let software = format!("Rohditor {}", env!("CARGO_PKG_VERSION"));
    let mut ifd0_fields = Vec::new();
    push_ascii(&mut ifd0_fields, 0x010f, &source.make);
    push_ascii(&mut ifd0_fields, 0x0110, &source.model);
    ifd0_fields.push(TiffField::inline_short(0x0112, 1));
    push_ascii(&mut ifd0_fields, 0x0131, &software);
    let ifd0_count = ifd0_fields.len() + 1;

    let mut exif_fields = Vec::new();
    if let Some(value) = source.capture.exposure_time.filter(valid_rational) {
        exif_fields.push(TiffField::rational(0x829a, value));
    }
    if let Some(value) = source.capture.aperture.filter(valid_rational) {
        exif_fields.push(TiffField::rational(0x829d, value));
    }
    if let Some(value) = source
        .capture
        .iso
        .and_then(|value| u16::try_from(value).ok())
    {
        exif_fields.push(TiffField::inline_short(0x8827, value));
    }
    if let Some(value) = source
        .capture
        .captured_at
        .as_deref()
        .and_then(exif_datetime)
    {
        push_ascii(&mut exif_fields, 0x9003, &value);
    }
    exif_fields.push(TiffField::undefined(0x9000, b"0232"));
    if let Some(value) = source.capture.focal_length.filter(valid_rational) {
        exif_fields.push(TiffField::rational(0x920a, value));
    }
    exif_fields.push(TiffField::inline_short(0xa001, 1));
    exif_fields.push(TiffField::inline_long(0xa002, width));
    exif_fields.push(TiffField::inline_long(0xa003, height));
    if let Some(value) = source.capture.lens_make.as_deref() {
        push_ascii(&mut exif_fields, 0xa433, value);
    }
    if let Some(value) = source.capture.lens_model.as_deref() {
        push_ascii(&mut exif_fields, 0xa434, value);
    }

    let mut output = b"II\x2a\0\x08\0\0\0".to_vec();
    let ifd0_offset = output.len();
    output.resize(ifd0_offset + ifd_size(ifd0_count), 0);
    let mut ifd0_entries = ifd0_fields
        .into_iter()
        .map(|field| field.materialize(&mut output))
        .collect::<Vec<_>>();
    align_to_four(&mut output);
    let exif_ifd_offset = output.len() as u32;
    ifd0_entries.push(TiffEntry {
        tag: 0x8769,
        field_type: TYPE_LONG,
        count: 1,
        value: exif_ifd_offset,
    });

    let exif_ifd_start = output.len();
    output.resize(exif_ifd_start + ifd_size(exif_fields.len()), 0);
    let exif_entries = exif_fields
        .into_iter()
        .map(|field| field.materialize(&mut output))
        .collect::<Vec<_>>();
    write_ifd(&mut output, ifd0_offset, ifd0_entries);
    write_ifd(&mut output, exif_ifd_start, exif_entries);

    debug_assert_eq!(TYPE_ASCII, 2);
    debug_assert_eq!(TYPE_SHORT, 3);
    debug_assert_eq!(TYPE_RATIONAL, 5);
    output
}

fn valid_rational(value: &RationalValue) -> bool {
    value.denominator != 0
}

fn exif_datetime(value: &str) -> Option<String> {
    let candidate = value.get(..19)?;
    let bytes = candidate.as_bytes();
    let valid_digits = [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18]
        .into_iter()
        .all(|index| bytes[index].is_ascii_digit());
    let valid_separators = matches!(bytes[4], b':' | b'-')
        && bytes[7] == bytes[4]
        && bytes[10] == b' '
        && bytes[13] == b':'
        && bytes[16] == b':';
    if !valid_digits || !valid_separators {
        return None;
    }
    let mut normalized = candidate.to_owned();
    normalized.replace_range(4..5, ":");
    normalized.replace_range(7..8, ":");
    Some(normalized)
}

fn push_ascii(fields: &mut Vec<TiffField>, tag: u16, value: &str) {
    let sanitized = value
        .chars()
        .filter_map(|character| match character {
            ' '..='~' => Some(character as u8),
            '\0'..='\u{1f}' | '\u{7f}' => None,
            _ => Some(b'?'),
        })
        .take(255)
        .collect::<Vec<_>>();
    if !sanitized.is_empty() {
        let mut terminated = sanitized;
        terminated.push(0);
        fields.push(TiffField {
            tag,
            field_type: 2,
            count: terminated.len() as u32,
            bytes: terminated,
        });
    }
}

fn ifd_size(entry_count: usize) -> usize {
    2 + entry_count * 12 + 4
}

#[derive(Debug)]
struct TiffField {
    tag: u16,
    field_type: u16,
    count: u32,
    bytes: Vec<u8>,
}

impl TiffField {
    fn inline_short(tag: u16, value: u16) -> Self {
        Self {
            tag,
            field_type: 3,
            count: 1,
            bytes: value.to_le_bytes().to_vec(),
        }
    }

    fn inline_long(tag: u16, value: u32) -> Self {
        Self {
            tag,
            field_type: 4,
            count: 1,
            bytes: value.to_le_bytes().to_vec(),
        }
    }

    fn rational(tag: u16, value: RationalValue) -> Self {
        let mut bytes = value.numerator.to_le_bytes().to_vec();
        bytes.extend_from_slice(&value.denominator.to_le_bytes());
        Self {
            tag,
            field_type: 5,
            count: 1,
            bytes,
        }
    }

    fn undefined(tag: u16, value: &[u8]) -> Self {
        Self {
            tag,
            field_type: 7,
            count: value.len() as u32,
            bytes: value.to_vec(),
        }
    }

    fn materialize(self, output: &mut Vec<u8>) -> TiffEntry {
        let value = if self.bytes.len() <= 4 {
            let mut inline = [0_u8; 4];
            inline[..self.bytes.len()].copy_from_slice(&self.bytes);
            u32::from_le_bytes(inline)
        } else {
            if !output.len().is_multiple_of(2) {
                output.push(0);
            }
            let offset = output.len() as u32;
            output.extend_from_slice(&self.bytes);
            offset
        };
        TiffEntry {
            tag: self.tag,
            field_type: self.field_type,
            count: self.count,
            value,
        }
    }
}

#[derive(Debug)]
struct TiffEntry {
    tag: u16,
    field_type: u16,
    count: u32,
    value: u32,
}

fn write_ifd(output: &mut [u8], offset: usize, mut entries: Vec<TiffEntry>) {
    entries.sort_by_key(|entry| entry.tag);
    output[offset..offset + 2].copy_from_slice(&(entries.len() as u16).to_le_bytes());
    for (index, entry) in entries.into_iter().enumerate() {
        let start = offset + 2 + index * 12;
        output[start..start + 2].copy_from_slice(&entry.tag.to_le_bytes());
        output[start + 2..start + 4].copy_from_slice(&entry.field_type.to_le_bytes());
        output[start + 4..start + 8].copy_from_slice(&entry.count.to_le_bytes());
        output[start + 8..start + 12].copy_from_slice(&entry.value.to_le_bytes());
    }
    let next_ifd = offset
        + 2
        + 12 * output[offset..offset + 2]
            .try_into()
            .map(u16::from_le_bytes)
            .map(usize::from)
            .expect("IFD entry count is present");
    output[next_ifd..next_ifd + 4].fill(0);
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        DitherMode, ExportFormat, ExportMetadataPolicy, ExportSettings, OutputMedium,
        OutputSharpening, OutputSharpeningStrength, OutputSize, PngBitDepth, exif_datetime,
    };

    #[test]
    fn settings_validate_quality_and_extensions() {
        let invalid = ExportSettings {
            format: ExportFormat::Jpeg { quality: 0 },
            ..ExportSettings::default()
        };
        assert!(invalid.validate().is_err());

        let png = ExportSettings {
            format: ExportFormat::Png {
                bit_depth: PngBitDepth::Sixteen,
            },
            ..ExportSettings::default()
        };
        assert!(png.validate_destination(Path::new("photo.PNG")).is_ok());
        assert!(png.validate_destination(Path::new("photo.jpg")).is_err());
    }

    #[test]
    fn settings_round_trip_without_cli_or_ui_types() {
        let settings = ExportSettings {
            format: ExportFormat::Jpeg { quality: 93 },
            dithering: DitherMode::Ordered8x8,
            metadata: ExportMetadataPolicy::None,
            overwrite: true,
            size: OutputSize::Fit {
                width: 2048,
                height: 1365,
            },
            sharpening: Some(OutputSharpening {
                medium: OutputMedium::Glossy,
                strength: OutputSharpeningStrength::High,
            }),
        };
        let json = serde_json::to_string(&settings).expect("serialize export settings");
        let decoded: ExportSettings =
            serde_json::from_str(&json).expect("deserialize export settings");
        assert_eq!(decoded, settings);
    }

    #[test]
    fn exif_dates_are_normalized_and_invalid_dates_are_omitted() {
        assert_eq!(
            exif_datetime("2026-08-29 12:34:56+02:00").as_deref(),
            Some("2026:08:29 12:34:56")
        );
        assert_eq!(
            exif_datetime("2026:08:29 12:34:56").as_deref(),
            Some("2026:08:29 12:34:56")
        );
        assert_eq!(exif_datetime("not a date"), None);
    }
}
