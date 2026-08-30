use rohditor_raw::RawOrientation;

use crate::PipelineError;

/// Coordinate mapping for physically applying one RAW orientation.
///
/// Keeping this mapping in the core crate prevents loading previews, CPU
/// output, and future GPU implementations from growing independent EXIF
/// orientation tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrientationMap {
    source_width: usize,
    source_height: usize,
    output_width: usize,
    output_height: usize,
    orientation: RawOrientation,
}

impl OrientationMap {
    /// Construct the output-to-source mapping for non-empty dimensions.
    ///
    /// # Errors
    ///
    /// Returns [`PipelineError::InvalidDimensions`] when either source
    /// dimension is zero.
    pub fn new(
        source_width: usize,
        source_height: usize,
        orientation: RawOrientation,
    ) -> Result<Self, PipelineError> {
        if source_width == 0 || source_height == 0 {
            return Err(PipelineError::InvalidDimensions {
                width: source_width,
                height: source_height,
                row_stride: 0,
                reason: "orientation requires non-zero source dimensions".to_owned(),
            });
        }
        let (output_width, output_height) = match orientation {
            RawOrientation::Transpose
            | RawOrientation::Rotate90
            | RawOrientation::Transverse
            | RawOrientation::Rotate270 => (source_height, source_width),
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

    pub(crate) const fn source_coordinate_in_bounds(
        self,
        output_x: usize,
        output_y: usize,
    ) -> (usize, usize) {
        match self.orientation {
            RawOrientation::Normal | RawOrientation::Unknown => (output_x, output_y),
            RawOrientation::HorizontalFlip => (self.source_width - 1 - output_x, output_y),
            RawOrientation::Rotate180 => (
                self.source_width - 1 - output_x,
                self.source_height - 1 - output_y,
            ),
            RawOrientation::VerticalFlip => (output_x, self.source_height - 1 - output_y),
            RawOrientation::Transpose => (output_y, output_x),
            RawOrientation::Rotate90 => (output_y, self.source_height - 1 - output_x),
            RawOrientation::Transverse => (
                self.source_width - 1 - output_y,
                self.source_height - 1 - output_x,
            ),
            RawOrientation::Rotate270 => (self.source_width - 1 - output_y, output_x),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_images_and_out_of_range_coordinates() {
        assert!(OrientationMap::new(0, 2, RawOrientation::Normal).is_err());
        let map = OrientationMap::new(2, 3, RawOrientation::Rotate90).expect("valid map");
        assert_eq!(map.output_dimensions(), (3, 2));
        assert_eq!(map.source_coordinate(0, 0), Some((0, 2)));
        assert_eq!(map.source_coordinate(3, 0), None);
        assert_eq!(map.source_coordinate(0, 2), None);
    }
}
