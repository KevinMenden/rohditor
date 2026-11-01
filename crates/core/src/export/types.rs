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

/// Pixel size of the exported image relative to the cropped, oriented result.
///
/// Resizing never upscales: a target at least as large as the image on the
/// limiting axis keeps the original size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputSize {
    #[default]
    Original,
    /// Scale so the longer edge has this many pixels.
    LongEdge { pixels: u32 },
    /// Scale to fit inside this bounding box, keeping the aspect ratio.
    Fit { width: u32, height: u32 },
}

impl OutputSize {
    pub fn validate(self) -> Result<(), ExportError> {
        let valid = match self {
            Self::Original => true,
            Self::LongEdge { pixels } => pixels > 0,
            Self::Fit { width, height } => width > 0 && height > 0,
        };
        if valid {
            Ok(())
        } else {
            Err(ExportError::InvalidSettings {
                reason: "export size must be at least 1 pixel on each axis".to_owned(),
            })
        }
    }

    /// Final pixel size for a cropped, oriented `width` x `height` image.
    ///
    /// The limiting axis lands exactly on its target; the other axis is the
    /// aspect-preserving value rounded to the nearest pixel (at least 1).
    #[must_use]
    pub fn resolve(self, width: usize, height: usize) -> (usize, usize) {
        let (box_width, box_height) = match self {
            Self::Original => return (width, height),
            Self::LongEdge { pixels } => (pixels as usize, pixels as usize),
            Self::Fit { width, height } => (width as usize, height as usize),
        };
        // Compare box_width/width with box_height/height by cross-multiplying.
        let (box_width, box_height) = (box_width as u128, box_height as u128);
        let (src_width, src_height) = (width as u128, height as u128);
        let width_limited = box_width * src_height <= box_height * src_width;
        let (target, limiting, other) = if width_limited {
            (box_width, src_width, src_height)
        } else {
            (box_height, src_height, src_width)
        };
        if target >= limiting {
            return (width, height);
        }
        // round(other * target / limiting) in integers.
        let scaled = ((2 * other * target + limiting) / (2 * limiting)).max(1) as usize;
        let target = target as usize;
        if width_limited {
            (target, scaled)
        } else {
            (scaled, target)
        }
    }
}

/// Stable export choices, independent of any CLI or UI widgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportSettings {
    pub format: ExportFormat,
    pub dithering: DitherMode,
    pub metadata: ExportMetadataPolicy,
    pub overwrite: bool,
    pub size: OutputSize,
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
            size: OutputSize::Original,
        }
    }
}

impl ExportSettings {
    pub fn validate(self) -> Result<(), ExportError> {
        self.size.validate()?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_edge_resolves_both_orientations_and_rounds_to_nearest() {
        let long = OutputSize::LongEdge { pixels: 1000 };
        assert_eq!(long.resolve(6000, 4000), (1000, 667));
        assert_eq!(long.resolve(4000, 6000), (667, 1000));
        // 3 * 5 / 7 = 2.14
        assert_eq!(OutputSize::LongEdge { pixels: 5 }.resolve(7, 3), (5, 2));
        // The short edge never collapses below one pixel.
        assert_eq!(OutputSize::LongEdge { pixels: 2 }.resolve(1000, 3), (2, 1));
    }

    #[test]
    fn resize_never_upscales() {
        let long = OutputSize::LongEdge { pixels: 8000 };
        assert_eq!(long.resolve(6000, 4000), (6000, 4000));
        let fit = OutputSize::Fit {
            width: 7000,
            height: 100,
        };
        assert_eq!(fit.resolve(6000, 4000), (150, 100));
        let wide = OutputSize::Fit {
            width: 7000,
            height: 5000,
        };
        assert_eq!(wide.resolve(6000, 4000), (6000, 4000));
    }

    #[test]
    fn fit_uses_the_tighter_box_side() {
        let fit = OutputSize::Fit {
            width: 1200,
            height: 1200,
        };
        assert_eq!(fit.resolve(3000, 2000), (1200, 800));
        assert_eq!(fit.resolve(2000, 3000), (800, 1200));
        let tall_box = OutputSize::Fit {
            width: 500,
            height: 2000,
        };
        assert_eq!(tall_box.resolve(3000, 2000), (500, 333));
    }

    #[test]
    fn zero_sizes_are_rejected() {
        assert!(OutputSize::LongEdge { pixels: 0 }.validate().is_err());
        assert!(
            OutputSize::Fit {
                width: 10,
                height: 0
            }
            .validate()
            .is_err()
        );
        assert!(OutputSize::Original.validate().is_ok());
    }
}
