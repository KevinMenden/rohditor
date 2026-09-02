use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ImageError;

/// Orientation represented independently of any decoder library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    Normal,
    HorizontalFlip,
    Rotate180,
    VerticalFlip,
    Transpose,
    Rotate90,
    Transverse,
    Rotate270,
    Unknown,
}

impl fmt::Display for Orientation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Normal => "normal",
            Self::HorizontalFlip => "horizontal flip",
            Self::Rotate180 => "rotate 180 degrees",
            Self::VerticalFlip => "vertical flip",
            Self::Transpose => "transpose",
            Self::Rotate90 => "rotate 90 degrees",
            Self::Transverse => "transverse",
            Self::Rotate270 => "rotate 270 degrees",
            Self::Unknown => "unknown",
        };
        formatter.write_str(name)
    }
}

/// Coordinate mapping for physically applying one RAW orientation.
///
/// Keeping this mapping with the image vocabulary prevents loading previews, CPU
/// output, and future GPU implementations from growing independent EXIF
/// orientation tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrientationMap {
    source_width: usize,
    source_height: usize,
    output_width: usize,
    output_height: usize,
    orientation: Orientation,
}

impl OrientationMap {
    /// Construct the output-to-source mapping for non-empty dimensions.
    ///
    /// # Errors
    ///
    /// Returns [`ImageError::InvalidDimensions`] when either source
    /// dimension is zero.
    pub fn new(
        source_width: usize,
        source_height: usize,
        orientation: Orientation,
    ) -> Result<Self, ImageError> {
        if source_width == 0 || source_height == 0 {
            return Err(ImageError::InvalidDimensions {
                width: source_width,
                height: source_height,
                row_stride: 0,
                reason: "orientation requires non-zero source dimensions".to_owned(),
            });
        }
        let (output_width, output_height) = match orientation {
            Orientation::Transpose
            | Orientation::Rotate90
            | Orientation::Transverse
            | Orientation::Rotate270 => (source_height, source_width),
            _ => (source_width, source_height),
        };
        Ok(Self {
            source_width,
            source_height,
            output_width,
            output_height,
            orientation,
        })
    }

    #[must_use]
    pub const fn output_dimensions(self) -> (usize, usize) {
        (self.output_width, self.output_height)
    }

    /// Return the source coordinate for an in-range output coordinate.
    #[must_use]
    pub const fn source_coordinate(
        self,
        output_x: usize,
        output_y: usize,
    ) -> Option<(usize, usize)> {
        if output_x >= self.output_width || output_y >= self.output_height {
            return None;
        }
        Some(self.source_coordinate_in_bounds(output_x, output_y))
    }

    #[doc(hidden)]
    pub const fn source_coordinate_in_bounds(
        self,
        output_x: usize,
        output_y: usize,
    ) -> (usize, usize) {
        match self.orientation {
            Orientation::Normal | Orientation::Unknown => (output_x, output_y),
            Orientation::HorizontalFlip => (self.source_width - 1 - output_x, output_y),
            Orientation::Rotate180 => (
                self.source_width - 1 - output_x,
                self.source_height - 1 - output_y,
            ),
            Orientation::VerticalFlip => (output_x, self.source_height - 1 - output_y),
            Orientation::Transpose => (output_y, output_x),
            Orientation::Rotate90 => (output_y, self.source_height - 1 - output_x),
            Orientation::Transverse => (
                self.source_width - 1 - output_y,
                self.source_height - 1 - output_x,
            ),
            Orientation::Rotate270 => (self.source_width - 1 - output_y, output_x),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_images_and_out_of_range_coordinates() {
        assert!(OrientationMap::new(0, 2, Orientation::Normal).is_err());
        let map = OrientationMap::new(2, 3, Orientation::Rotate90).expect("valid map");
        assert_eq!(map.output_dimensions(), (3, 2));
        assert_eq!(map.source_coordinate(0, 0), Some((0, 2)));
        assert_eq!(map.source_coordinate(3, 0), None);
        assert_eq!(map.source_coordinate(0, 2), None);
    }
}
