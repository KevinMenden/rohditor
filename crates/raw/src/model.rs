use std::fmt;
use std::sync::Arc;

use rohditor_image::Orientation as RawOrientation;
use serde::{Deserialize, Serialize};

/// A rectangle in unrotated sensor coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// A repeating color-filter-array pattern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CfaPattern {
    pub name: String,
    pub width: usize,
    pub height: usize,
}

/// The sensor pixel interpretation reported by the decoder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PhotometricInterpretation {
    Cfa { pattern: CfaPattern },
    LinearRaw,
    BlackIsZero,
}

/// Levels plus the dimensions of their repeating sensor pattern.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LevelPattern {
    pub values: Vec<f32>,
    pub repeat_width: usize,
    pub repeat_height: usize,
    pub components_per_pixel: usize,
}

/// A camera calibration matrix identified by its reference illuminant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraColorMatrix {
    pub illuminant: String,
    pub values: Vec<f32>,
}

/// A rational value retained exactly as stored in EXIF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RationalValue {
    pub numerator: u32,
    pub denominator: u32,
}

impl RationalValue {
    #[must_use]
    pub fn as_f64(self) -> Option<f64> {
        (self.denominator != 0).then(|| f64::from(self.numerator) / f64::from(self.denominator))
    }
}

impl fmt::Display for RationalValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.numerator, self.denominator)
    }
}

/// Capture metadata used by inspection and, later, export.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureMetadata {
    pub iso: Option<u32>,
    pub exposure_time: Option<RationalValue>,
    pub aperture: Option<RationalValue>,
    pub focal_length: Option<RationalValue>,
    pub captured_at: Option<String>,
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
}

/// Dimensions and pixel representation of an embedded loading preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddedPreviewInfo {
    pub width: u32,
    pub height: u32,
    pub color_type: String,
}

/// File encoding used for an extracted embedded preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncodedPreviewFormat {
    Jpeg,
}

impl EncodedPreviewFormat {
    #[must_use]
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
        }
    }

    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
        }
    }

    #[must_use]
    pub fn accepts_extension(self, extension: &str) -> bool {
        match self {
            Self::Jpeg => {
                extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg")
            }
        }
    }
}

impl fmt::Display for EncodedPreviewFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.media_type())
    }
}

/// Decoder-independent facts about one RAW image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawFileInfo {
    pub format: String,
    pub make: String,
    pub model: String,
    pub clean_make: String,
    pub clean_model: String,
    pub source_size_bytes: u64,
    pub source_identity: Option<SourceIdentity>,
    pub width: usize,
    pub height: usize,
    pub components_per_pixel: usize,
    /// Precision declared by the source RAW IFD, when available.
    pub source_bits_per_sample: Option<usize>,
    /// Storage precision of each sample returned by the decoder.
    pub decoded_bits_per_sample: usize,
    pub compression: Option<String>,
    pub active_area: Option<ImageRect>,
    pub crop_area: Option<ImageRect>,
    pub photometric_interpretation: PhotometricInterpretation,
    pub black_levels: LevelPattern,
    pub white_levels: Vec<f32>,
    pub as_shot_white_balance: [Option<f32>; 4],
    pub xyz_to_camera: [[f32; 3]; 4],
    pub color_matrices: Vec<CameraColorMatrix>,
    pub orientation: RawOrientation,
    pub capture: CaptureMetadata,
    pub embedded_preview: Option<EmbeddedPreviewInfo>,
}

/// Stable-enough identity facts captured when a source is opened.
///
/// Device and inode are populated on Unix. Size and modification time retain a
/// portable fingerprint for diagnostics and cache keys without exposing the
/// private source path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentity {
    pub size_bytes: u64,
    pub modified_unix_nanos: Option<u128>,
    pub filesystem_device: Option<u64>,
    pub filesystem_inode: Option<u64>,
}

/// An encoded loading preview, normally preserving the bytes stored in the RAW.
#[derive(Debug, Clone)]
pub struct EncodedPreview {
    pub width: u32,
    pub height: u32,
    pub color_type: String,
    pub format: EncodedPreviewFormat,
    pub bytes: Arc<[u8]>,
    /// Whether `bytes` retain the exact encoding found in the source RAW.
    pub is_original_encoding: bool,
}

/// A decoded integer sensor frame.
#[derive(Debug, Clone)]
pub struct RawFrame {
    pub info: RawFileInfo,
    pub row_stride: usize,
    pub mosaic: Arc<[u16]>,
}

#[cfg(test)]
mod tests {
    use super::{RationalValue, RawOrientation};

    #[test]
    fn zero_denominator_has_no_float_value() {
        let value = RationalValue {
            numerator: 1,
            denominator: 0,
        };

        assert_eq!(value.as_f64(), None);
        assert_eq!(value.to_string(), "1/0");
    }

    #[test]
    fn orientation_has_readable_text() {
        assert_eq!(RawOrientation::Rotate270.to_string(), "rotate 270 degrees");
    }
}
