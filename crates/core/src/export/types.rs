use std::path::Path;

use rohditor_image::{DisplayRgbImage, DisplayTransfer};
use serde::{Deserialize, Serialize};

use super::ExportError;

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
