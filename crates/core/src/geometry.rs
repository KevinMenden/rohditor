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
