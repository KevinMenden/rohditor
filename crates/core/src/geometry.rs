use rohditor_edit::NormalizedCropRect;
use rohditor_image::{Orientation, OrientationMap};

use crate::PipelineError;

/// Integer crop edges in the fully oriented developed canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedCropRect {
    pub left: usize,
    pub top: usize,
    pub right: usize,
    pub bottom: usize,
}

impl ResolvedCropRect {
    #[must_use]
    pub const fn width(self) -> usize {
        self.right - self.left
    }

    #[must_use]
    pub const fn height(self) -> usize {
        self.bottom - self.top
    }
}

/// CPU-reference mapping from a cropped output pixel to the uncropped linear
/// source. Crop edges are resolved once here and shared by CPU and GPU paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputGeometry {
    orientation: OrientationMap,
    crop: ResolvedCropRect,
}

impl OutputGeometry {
    /// Resolve an optional normalized crop after EXIF/user orientation.
    pub fn new(
        source_width: usize,
        source_height: usize,
        orientation: Orientation,
        crop: Option<NormalizedCropRect>,
    ) -> Result<Self, PipelineError> {
        let orientation = OrientationMap::new(source_width, source_height, orientation)?;
        let (width, height) = orientation.output_dimensions();
        let crop = match crop {
            None => ResolvedCropRect {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            },
            Some(crop) => {
                validate_normalized_crop(crop)?;
                let resolved = ResolvedCropRect {
                    left: resolve_edge(crop.left, width),
                    top: resolve_edge(crop.top, height),
                    right: resolve_edge(crop.right, width),
                    bottom: resolve_edge(crop.bottom, height),
                };
                if resolved.left >= resolved.right || resolved.top >= resolved.bottom {
                    return Err(PipelineError::InvalidRecipe {
                        field: "geometry.crop",
                        reason: format!(
                            "resolves below one pixel on the oriented {width}x{height} canvas"
                        ),
                    });
                }
                resolved
            }
        };
        Ok(Self { orientation, crop })
    }

    #[must_use]
    pub const fn full_dimensions(self) -> (usize, usize) {
        self.orientation.output_dimensions()
    }

    #[must_use]
    pub const fn crop(self) -> ResolvedCropRect {
        self.crop
    }

    #[must_use]
    pub const fn output_dimensions(self) -> (usize, usize) {
        (self.crop.width(), self.crop.height())
    }

    /// Map an in-bounds cropped output coordinate to the uncropped source.
    #[must_use]
    pub fn source_coordinate_in_bounds(self, output_x: usize, output_y: usize) -> (usize, usize) {
        self.orientation
            .source_coordinate_in_bounds(self.crop.left + output_x, self.crop.top + output_y)
    }

    /// Size of the developed frame: the image as it enters the adjustment stage, before
    /// orientation and crop. Mask layers live in this frame.
    #[must_use]
    pub const fn developed_dimensions(self) -> (usize, usize) {
        self.orientation.source_dimensions()
    }

    /// Length of the developed frame's long edge in pixels: one long-edge unit (the unit of
    /// mask radii) is this many output pixels, since orientation and crop do not scale.
    #[must_use]
    pub fn developed_long_edge(self) -> f64 {
        let (width, height) = self.developed_dimensions();
        width.max(height) as f64
    }

    /// Normalized position over the displayed (oriented, cropped) image, `0..1` on both axes,
    /// to the normalized position over the developed frame. Continuous, and extends linearly
    /// outside the image so a pointer dragged past an edge still maps to a mask point.
    ///
    /// Agrees with [`Self::source_coordinate_in_bounds`] at pixel centres. This is the one
    /// place to change when straighten/rotation is added.
    #[must_use]
    pub fn display_to_developed(self, point: (f64, f64)) -> (f64, f64) {
        let (full_width, full_height) = self.full_dimensions();
        let canvas = (
            (self.crop.left as f64 + point.0 * self.crop.width() as f64) / full_width as f64,
            (self.crop.top as f64 + point.1 * self.crop.height() as f64) / full_height as f64,
        );
        self.orientation.source_point(canvas)
    }

    /// Inverse of [`Self::display_to_developed`].
    #[must_use]
    pub fn developed_to_display(self, point: (f64, f64)) -> (f64, f64) {
        let (full_width, full_height) = self.full_dimensions();
        let canvas = self.orientation.output_point(point);
        (
            (canvas.0 * full_width as f64 - self.crop.left as f64) / self.crop.width() as f64,
            (canvas.1 * full_height as f64 - self.crop.top as f64) / self.crop.height() as f64,
        )
    }
}

fn validate_normalized_crop(crop: NormalizedCropRect) -> Result<(), PipelineError> {
    let valid_edges = [crop.left, crop.top, crop.right, crop.bottom]
        .into_iter()
        .all(|edge| edge.is_finite() && (0.0..=1.0).contains(&edge));
    if valid_edges && crop.left < crop.right && crop.top < crop.bottom {
        Ok(())
    } else {
        Err(PipelineError::InvalidRecipe {
            field: "geometry.crop",
            reason: "must use finite ordered edges within 0.0..=1.0".to_owned(),
        })
    }
}

fn resolve_edge(normalized: f64, dimension: usize) -> usize {
    // Recipe validation has already guaranteed finite normalized input. The
    // explicit clamp keeps this boundary safe for direct core callers too.
    (normalized.mul_add(dimension as f64, 0.0).round()).clamp(0.0, dimension as f64) as usize
}

#[cfg(test)]
mod tests {
    use rohditor_edit::NormalizedCropRect;
    use rohditor_image::Orientation;

    use super::{OutputGeometry, ResolvedCropRect};

    #[test]
    fn crop_resolves_edges_in_oriented_canvas_before_mapping_to_source() {
        let geometry = OutputGeometry::new(
            3,
            5,
            Orientation::Rotate90,
            Some(NormalizedCropRect {
                left: 0.2,
                top: 0.0,
                right: 0.8,
                bottom: 1.0,
            }),
        )
        .expect("valid crop");
        assert_eq!(geometry.full_dimensions(), (5, 3));
        assert_eq!(
            geometry.crop(),
            ResolvedCropRect {
                left: 1,
                top: 0,
                right: 4,
                bottom: 3,
            }
        );
        assert_eq!(geometry.output_dimensions(), (3, 3));
        assert_eq!(geometry.source_coordinate_in_bounds(0, 0), (0, 3));
    }

    const ORIENTATIONS: [Orientation; 9] = [
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

    /// A developed frame that is neither square nor symmetric under any orientation, with an
    /// off-centre crop, so a wrong axis, flip or crop offset cannot go unnoticed.
    fn cropped_geometry(orientation: Orientation) -> OutputGeometry {
        OutputGeometry::new(
            11,
            7,
            orientation,
            Some(NormalizedCropRect {
                left: 0.2,
                top: 0.1,
                right: 0.9,
                bottom: 0.7,
            }),
        )
        .expect("valid crop")
    }

    #[test]
    fn display_to_developed_agrees_with_the_pixel_mapping_for_every_orientation() {
        for orientation in ORIENTATIONS {
            let geometry = cropped_geometry(orientation);
            let (width, height) = geometry.output_dimensions();
            let (developed_width, developed_height) = geometry.developed_dimensions();
            for y in 0..height {
                for x in 0..width {
                    let (source_x, source_y) = geometry.source_coordinate_in_bounds(x, y);
                    let developed = geometry.display_to_developed((
                        (x as f64 + 0.5) / width as f64,
                        (y as f64 + 0.5) / height as f64,
                    ));
                    // The centre of the source pixel in normalized developed coordinates.
                    let expected = (
                        (source_x as f64 + 0.5) / developed_width as f64,
                        (source_y as f64 + 0.5) / developed_height as f64,
                    );
                    assert!(
                        (developed.0 - expected.0).abs() < 1e-12
                            && (developed.1 - expected.1).abs() < 1e-12,
                        "{orientation} at ({x}, {y}): got {developed:?}, want {expected:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn display_and_developed_round_trip_including_outside_the_image() {
        for orientation in ORIENTATIONS {
            let geometry = cropped_geometry(orientation);
            for point in [(0.0, 0.0), (0.3, 0.9), (1.0, 1.0), (-0.5, 1.4), (1.8, -0.7)] {
                let back = geometry.developed_to_display(geometry.display_to_developed(point));
                assert!(
                    (back.0 - point.0).abs() < 1e-12 && (back.1 - point.1).abs() < 1e-12,
                    "{orientation}: {point:?} came back as {back:?}"
                );
            }
        }
    }

    #[test]
    fn the_displayed_corners_map_to_the_crop_corners_of_the_developed_frame() {
        // Rotate90 on a 3x5 source: the oriented canvas is 5x3 and the crop keeps columns
        // 1..4. Display (0, 0) is the top-left corner of oriented pixel (1, 0), which is source
        // pixel (0, 3) (see `crop_resolves_edges_in_oriented_canvas_before_mapping_to_source`);
        // that corner is the source pixel's bottom-left one, at x = 0, y = 4/5.
        let geometry = OutputGeometry::new(
            3,
            5,
            Orientation::Rotate90,
            Some(NormalizedCropRect {
                left: 0.2,
                top: 0.0,
                right: 0.8,
                bottom: 1.0,
            }),
        )
        .expect("valid crop");
        let corner = geometry.display_to_developed((0.0, 0.0));
        assert!((corner.0 - 0.0).abs() < 1e-12 && (corner.1 - 4.0 / 5.0).abs() < 1e-12);
        let opposite = geometry.display_to_developed((1.0, 1.0));
        assert!((opposite.0 - 1.0).abs() < 1e-12 && (opposite.1 - 1.0 / 5.0).abs() < 1e-12);
    }

    #[test]
    fn the_long_edge_is_the_developed_frames_regardless_of_orientation() {
        for orientation in ORIENTATIONS {
            let geometry = cropped_geometry(orientation);
            assert_eq!(geometry.developed_dimensions(), (11, 7));
            assert!((geometry.developed_long_edge() - 11.0).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn tiny_crop_that_rounds_to_no_pixel_is_rejected() {
        let error = OutputGeometry::new(
            3,
            3,
            Orientation::Normal,
            Some(NormalizedCropRect {
                left: 0.01,
                top: 0.01,
                right: 0.02,
                bottom: 0.02,
            }),
        );
        assert!(error.is_err());
    }

    #[test]
    fn direct_geometry_call_rejects_invalid_normalized_edges() {
        let error = OutputGeometry::new(
            3,
            3,
            Orientation::Normal,
            Some(NormalizedCropRect {
                left: f64::NAN,
                top: 0.0,
                right: 1.0,
                bottom: 1.0,
            }),
        );
        assert!(error.is_err());
    }
}
