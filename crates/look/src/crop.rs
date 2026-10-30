use rohditor_image::DisplayRgbImage;

/// Pixel rectangle inside a display image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CropRect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl CropRect {
    /// The whole image.
    #[must_use]
    pub fn full<T>(image: &DisplayRgbImage<T>) -> Self {
        Self {
            x: 0,
            y: 0,
            width: image.width(),
            height: image.height(),
        }
    }

    /// Centered rectangle of `dimensions` trimmed along its long axis so that
    /// `long / short` equals `long_over_short`, which is at most the current
    /// ratio. A ratio at or above the current one returns the full rectangle.
    fn trimmed_to_ratio(dimensions: (usize, usize), long_over_short: f32) -> Self {
        let (width, height) = dimensions;
        let mut rect = Self {
            x: 0,
            y: 0,
            width,
            height,
        };
        let (long, short) = (width.max(height), width.min(height));
        let target_long = ((short as f32 * long_over_short).round() as usize).clamp(1, long);
        if width >= height {
            rect.width = target_long;
            rect.x = (width - target_long) / 2;
        } else {
            rect.height = target_long;
            rect.y = (height - target_long) / 2;
        }
        rect
    }
}

/// Centered crops that give two images of the same scene the same framing.
///
/// Cameras write JPEGs with the same aspect as the sensor crop, but our active
/// area can differ by a few pixels. The wider image is trimmed to the other's
/// `long / short` ratio. Returns `None` when one image is portrait and the
/// other landscape, since the framing cannot be reconciled by cropping.
#[must_use]
pub fn common_aspect_crops(
    first: (usize, usize),
    second: (usize, usize),
) -> Option<(CropRect, CropRect)> {
    let degenerate = |(width, height): (usize, usize)| width == 0 || height == 0;
    if degenerate(first) || degenerate(second) {
        return None;
    }
    let landscape = |(width, height): (usize, usize)| width >= height;
    if first.0 != first.1 && second.0 != second.1 && landscape(first) != landscape(second) {
        return None;
    }
    let ratio =
        |(width, height): (usize, usize)| width.max(height) as f32 / width.min(height) as f32;
    let target = ratio(first).min(ratio(second));
    Some((
        CropRect::trimmed_to_ratio(first, target),
        CropRect::trimmed_to_ratio(second, target),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_the_wider_image_symmetrically() {
        let (wide, narrow) = common_aspect_crops((300, 100), (200, 100)).expect("same orientation");
        assert_eq!(
            wide,
            CropRect {
                x: 50,
                y: 0,
                width: 200,
                height: 100
            }
        );
        assert_eq!(narrow, CropRect::trimmed_to_ratio((200, 100), 2.0));
        assert_eq!((narrow.x, narrow.width), (0, 200));
    }

    #[test]
    fn trims_portrait_images_along_the_height() {
        let (tall, other) = common_aspect_crops((100, 300), (100, 200)).expect("both portrait");
        assert_eq!((tall.y, tall.height, tall.width), (50, 200, 100));
        assert_eq!((other.y, other.height), (0, 200));
    }

    #[test]
    fn rejects_mixed_orientations_and_empty_images() {
        assert!(common_aspect_crops((300, 200), (200, 300)).is_none());
        assert!(common_aspect_crops((0, 200), (200, 300)).is_none());
    }
}
