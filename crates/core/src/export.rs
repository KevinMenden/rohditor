use std::io::{self, Write};
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use rohditor_image::{DisplayRgbImage, DisplayTransfer};
use rohditor_raw::{RationalValue, RawFileInfo};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::output::write_transactionally;

pub const JPEG_QUALITY_MIN: u8 = 1;
pub const JPEG_QUALITY_MAX: u8 = 100;
pub const JPEG_QUALITY_DEFAULT: u8 = 90;

/// Encoded file format and format-specific settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExportFormat {
    Jpeg { quality: u8 },
    Png { bit_depth: PngBitDepth },
}

impl ExportFormat {
    #[must_use]
    pub const fn bit_depth(self) -> OutputBitDepth {
        match self {
            Self::Jpeg { .. }
            | Self::Png {
                bit_depth: PngBitDepth::Eight,
            } => OutputBitDepth::Eight,
            Self::Png {
                bit_depth: PngBitDepth::Sixteen,
            } => OutputBitDepth::Sixteen,
        }
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Jpeg { .. } => "JPEG",
            Self::Png { .. } => "PNG",
        }
    }

    #[must_use]
    pub fn accepts_extension(self, extension: &str) -> bool {
        match self {
            Self::Jpeg { .. } => {
                extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg")
            }
            Self::Png { .. } => extension.eq_ignore_ascii_case("png"),
        }
    }
}

/// Supported PNG sample depths.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PngBitDepth {
    #[default]
    Eight,
    Sixteen,
}

/// Integer sample depth required by an export format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputBitDepth {
    Eight,
    Sixteen,
}

impl OutputBitDepth {
    #[must_use]
    pub const fn bits(self) -> u8 {
        match self {
            Self::Eight => 8,
            Self::Sixteen => 16,
        }
    }

    #[must_use]
    pub(crate) const fn bytes_per_sample(self) -> usize {
        match self {
            Self::Eight => 1,
            Self::Sixteen => 2,
        }
    }
}

/// Quantization dithering applied after the sRGB transfer function.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DitherMode {
    #[default]
    None,
    Ordered8x8,
}

/// Source metadata included in the exported file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportMetadataPolicy {
    None,
    #[default]
    Safe,
}

/// Stable export choices, independent of any CLI or UI widgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportSettings {
    pub format: ExportFormat,
    pub dithering: DitherMode,
    pub metadata: ExportMetadataPolicy,
    pub overwrite: bool,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            format: ExportFormat::Png {
                bit_depth: PngBitDepth::Eight,
            },
            dithering: DitherMode::None,
            metadata: ExportMetadataPolicy::Safe,
            overwrite: false,
        }
    }
}

impl ExportSettings {
    pub fn validate(self) -> Result<(), ExportError> {
        if let ExportFormat::Jpeg { quality } = self.format
            && !(JPEG_QUALITY_MIN..=JPEG_QUALITY_MAX).contains(&quality)
        {
            return Err(ExportError::InvalidSettings {
                reason: format!(
                    "JPEG quality {quality} is outside the inclusive range {JPEG_QUALITY_MIN}..={JPEG_QUALITY_MAX}"
                ),
            });
        }
        Ok(())
    }

    pub fn validate_destination(self, destination: &Path) -> Result<(), ExportError> {
        self.validate()?;
        let extension = destination
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if self.format.accepts_extension(extension) {
            return Ok(());
        }
        Err(ExportError::InvalidSettings {
            reason: format!(
                "{} export destination {} has an incompatible extension",
                self.format.description(),
                destination.display()
            ),
        })
    }
}

/// Quantized, transfer-encoded pixels ready for a file encoder.
#[derive(Debug)]
pub enum ExportImage {
    Rgb8(DisplayRgbImage<u8>),
    Rgb16(DisplayRgbImage<u16>),
}

impl ExportImage {
    #[must_use]
    pub const fn width(&self) -> usize {
        match self {
            Self::Rgb8(image) => image.width(),
            Self::Rgb16(image) => image.width(),
        }
    }

    #[must_use]
    pub const fn height(&self) -> usize {
        match self {
            Self::Rgb8(image) => image.height(),
            Self::Rgb16(image) => image.height(),
        }
    }

    #[must_use]
    pub const fn row_stride(&self) -> usize {
        match self {
            Self::Rgb8(image) => image.row_stride(),
            Self::Rgb16(image) => image.row_stride(),
        }
    }

    #[must_use]
    pub const fn bit_depth(&self) -> OutputBitDepth {
        match self {
            Self::Rgb8(_) => OutputBitDepth::Eight,
            Self::Rgb16(_) => OutputBitDepth::Sixteen,
        }
    }

    #[must_use]
    pub const fn transfer(&self) -> DisplayTransfer {
        match self {
            Self::Rgb8(image) => image.transfer(),
            Self::Rgb16(image) => image.transfer(),
        }
    }
}

/// Facts about a successfully committed export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportReport {
    pub width: usize,
    pub height: usize,
    pub bit_depth: OutputBitDepth,
    pub bytes_written: u64,
    pub metadata_embedded: bool,
}

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("invalid export settings: {reason}")]
    InvalidSettings { reason: String },

    #[error(
        "export format requires {required}-bit RGB samples, but the rendered image contains {actual}-bit samples"
    )]
    WrongBitDepth { required: u8, actual: u8 },

    #[error("export requires sRGB transfer-encoded pixels")]
    WrongTransfer,

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

/// Encode an already quantized sRGB image and commit it transactionally.
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
    let icc_profile = srgb_icc_profile();
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
    if image.transfer() != DisplayTransfer::Srgb {
        return Err(ExportError::WrongTransfer);
    }
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

fn srgb_icc_profile() -> Vec<u8> {
    let tags = [
        (*b"desc", icc_description("Rohditor sRGB")),
        (*b"cprt", icc_text("Public domain color characterization")),
        (*b"wtpt", icc_xyz([0.9642, 1.0, 0.8249])),
        (*b"rXYZ", icc_xyz([0.436_074_7, 0.222_504_5, 0.013_932_2])),
        (*b"gXYZ", icc_xyz([0.385_064_9, 0.716_878_6, 0.097_104_5])),
        (*b"bXYZ", icc_xyz([0.143_080_4, 0.060_616_9, 0.714_173_3])),
        (*b"rTRC", icc_srgb_curve()),
        (*b"gTRC", icc_srgb_curve()),
        (*b"bTRC", icc_srgb_curve()),
        (
            *b"chad",
            icc_sf32(&[
                1.047_811_2,
                0.022_886_6,
                -0.050_127,
                0.029_542_4,
                0.990_484_4,
                -0.017_049_1,
                -0.009_234_5,
                0.015_043_6,
                0.752_131_6,
            ]),
        ),
    ];
    let table_length = 4 + tags.len() * 12;
    let mut profile = vec![0_u8; 128 + table_length];

    profile[4..8].copy_from_slice(b"Rohd");
    profile[8..12].copy_from_slice(&[0x02, 0x10, 0x00, 0x00]);
    profile[12..16].copy_from_slice(b"mntr");
    profile[16..20].copy_from_slice(b"RGB ");
    profile[20..24].copy_from_slice(b"XYZ ");
    for (index, value) in [2026_u16, 8, 29, 0, 0, 0].into_iter().enumerate() {
        let offset = 24 + index * 2;
        profile[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
    }
    profile[36..40].copy_from_slice(b"acsp");
    profile[68..72].copy_from_slice(&icc_fixed(0.9642));
    profile[72..76].copy_from_slice(&icc_fixed(1.0));
    profile[76..80].copy_from_slice(&icc_fixed(0.8249));
    profile[80..84].copy_from_slice(b"Rohd");
    profile[128..132].copy_from_slice(&(tags.len() as u32).to_be_bytes());

    for (index, (signature, data)) in tags.into_iter().enumerate() {
        align_to_four(&mut profile);
        let offset = profile.len();
        let size = data.len();
        profile.extend_from_slice(&data);
        let table_offset = 132 + index * 12;
        profile[table_offset..table_offset + 4].copy_from_slice(&signature);
        profile[table_offset + 4..table_offset + 8].copy_from_slice(&(offset as u32).to_be_bytes());
        profile[table_offset + 8..table_offset + 12].copy_from_slice(&(size as u32).to_be_bytes());
    }
    align_to_four(&mut profile);
    let profile_size = profile.len() as u32;
    profile[0..4].copy_from_slice(&profile_size.to_be_bytes());
    profile
}

fn icc_description(description: &str) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(b"desc\0\0\0\0");
    data.extend_from_slice(&((description.len() + 1) as u32).to_be_bytes());
    data.extend_from_slice(description.as_bytes());
    data.push(0);
    data.extend_from_slice(&[0; 8]);
    data.extend_from_slice(&[0; 2]);
    data.push(0);
    data.extend_from_slice(&[0; 67]);
    data
}

fn icc_text(value: &str) -> Vec<u8> {
    let mut data = b"text\0\0\0\0".to_vec();
    data.extend_from_slice(value.as_bytes());
    data.push(0);
    data
}

fn icc_xyz(values: [f64; 3]) -> Vec<u8> {
    let mut data = b"XYZ \0\0\0\0".to_vec();
    for value in values {
        data.extend_from_slice(&icc_fixed(value));
    }
    data
}

fn icc_sf32(values: &[f64]) -> Vec<u8> {
    let mut data = b"sf32\0\0\0\0".to_vec();
    for &value in values {
        data.extend_from_slice(&icc_fixed(value));
    }
    data
}

fn icc_srgb_curve() -> Vec<u8> {
    let mut data = b"para\0\0\0\0\0\x04\0\0".to_vec();
    for value in [
        2.4,
        1.0 / 1.055,
        0.055 / 1.055,
        1.0 / 12.92,
        0.04045,
        0.0,
        0.0,
    ] {
        data.extend_from_slice(&icc_fixed(value));
    }
    data
}

fn icc_fixed(value: f64) -> [u8; 4] {
    ((value * 65_536.0).round() as i32).to_be_bytes()
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
        DitherMode, ExportFormat, ExportMetadataPolicy, ExportSettings, PngBitDepth, exif_datetime,
        srgb_icc_profile,
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
    fn generated_profile_has_an_icc_header_and_srgb_description() {
        let profile = srgb_icc_profile();
        assert_eq!(
            u32::from_be_bytes(profile[0..4].try_into().expect("profile size")) as usize,
            profile.len()
        );
        assert_eq!(&profile[16..20], b"RGB ");
        assert_eq!(&profile[20..24], b"XYZ ");
        assert_eq!(&profile[36..40], b"acsp");
        assert!(
            profile
                .windows(b"Rohditor sRGB".len())
                .any(|window| window == b"Rohditor sRGB")
        );
    }

    #[test]
    fn settings_round_trip_without_cli_or_ui_types() {
        let settings = ExportSettings {
            format: ExportFormat::Jpeg { quality: 93 },
            dithering: DitherMode::Ordered8x8,
            metadata: ExportMetadataPolicy::None,
            overwrite: true,
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
