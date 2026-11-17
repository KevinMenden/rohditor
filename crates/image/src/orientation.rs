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

impl Orientation {
    /// The orientation as a map from output axes to source axes: swap the axes first, then
    /// mirror source x and/or source y. Used by the continuous (normalized) mappings; the
    /// integer table in [`OrientationMap::source_coordinate_in_bounds`] is tested against it.
    const fn axes(self) -> (bool, bool, bool) {
        match self {
            Self::Normal | Self::Unknown => (false, false, false),
            Self::HorizontalFlip => (false, true, false),
            Self::Rotate180 => (false, true, true),
            Self::VerticalFlip => (false, false, true),
            Self::Transpose => (true, false, false),
            Self::Rotate90 => (true, false, true),
            Self::Transverse => (true, true, true),
            Self::Rotate270 => (true, true, false),
        }
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

    #[must_use]
    pub const fn source_dimensions(self) -> (usize, usize) {
        (self.source_width, self.source_height)
    }

    /// Continuous counterpart of [`Self::source_coordinate_in_bounds`] in normalized
    /// coordinates: a point `(0..1, 0..1)` over the oriented output to the same point over the
    /// source. Pixel centres map exactly like the integer table, and points outside the image
    /// extend linearly.
    #[must_use]
    pub fn source_point(self, output: (f64, f64)) -> (f64, f64) {
        let (swap, flip_x, flip_y) = self.orientation.axes();
        let (a, b) = if swap { (output.1, output.0) } else { output };
        (
            if flip_x { 1.0 - a } else { a },
            if flip_y { 1.0 - b } else { b },
        )
    }

    /// Inverse of [`Self::source_point`].
    #[must_use]
    pub fn output_point(self, source: (f64, f64)) -> (f64, f64) {
        let (swap, flip_x, flip_y) = self.orientation.axes();
        let a = if flip_x { 1.0 - source.0 } else { source.0 };
        let b = if flip_y { 1.0 - source.1 } else { source.1 };
        if swap { (b, a) } else { (a, b) }
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

    const ALL: [Orientation; 9] = [
        Orientation::Normal,
        Orientation::HorizontalFlip,
        Orientation::Rotate180,
        Orientation::VerticalFlip,
        Orientation::Transpose,
        Orientation::Rotate90,
        Orientation::Transverse,
        Orientation::Rotate270,
        Orientation::Unknown,
    ];

    #[test]
    fn continuous_mapping_matches_the_integer_table_at_pixel_centres() {
        // 7x5 is asymmetric, so a wrong swap or flip cannot cancel out.
        for orientation in ALL {
            let map = OrientationMap::new(7, 5, orientation).expect("valid map");
            let (output_width, output_height) = map.output_dimensions();
            for output_y in 0..output_height {
                for output_x in 0..output_width {
                    let (x, y) = map.source_coordinate_in_bounds(output_x, output_y);
                    let centre = (
                        (output_x as f64 + 0.5) / output_width as f64,
                        (output_y as f64 + 0.5) / output_height as f64,
                    );
                    let source = map.source_point(centre);
                    assert!(
                        (source.0 - (x as f64 + 0.5) / 7.0).abs() < 1e-12
                            && (source.1 - (y as f64 + 0.5) / 5.0).abs() < 1e-12,
                        "{orientation} at ({output_x}, {output_y}): got {source:?}, want pixel ({x}, {y})"
                    );
                }
            }
        }
    }

    #[test]
    fn output_point_inverts_source_point_including_outside_the_frame() {
        for orientation in ALL {
            let map = OrientationMap::new(7, 5, orientation).expect("valid map");
            for point in [(0.25, 0.8), (-0.4, 1.7), (1.3, -0.2)] {
                let back = map.output_point(map.source_point(point));
                assert!(
                    (back.0 - point.0).abs() < 1e-12 && (back.1 - point.1).abs() < 1e-12,
                    "{orientation}: {point:?} came back as {back:?}"
                );
            }
        }
    }
}
