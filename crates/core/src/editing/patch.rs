//! The pixels under a click on the displayed image.

use std::ops::RangeInclusive;

use rohditor_image::Orientation;

use crate::OutputGeometry;
use rohditor_edit::GeometryAdjustments;

/// Half-width of the sampled patch: the patch is `2 * RADIUS + 1` pixels wide.
const PATCH_RADIUS: usize = 2;

/// The columns and rows of a small patch of the developed image (before
/// orientation and crop) around `display_position`, a position `0..1` over the
/// image as shown. `None` outside of it.
#[must_use]
pub fn developed_patch(
    (width, height): (usize, usize),
    source_orientation: Orientation,
    geometry: &GeometryAdjustments,
    display_position: (f64, f64),
) -> Option<(RangeInclusive<usize>, RangeInclusive<usize>)> {
    if !(0.0..=1.0).contains(&display_position.0) || !(0.0..=1.0).contains(&display_position.1) {
        return None;
    }
    let orientation = geometry.orientation_override.unwrap_or(source_orientation);
    let geometry = OutputGeometry::new(width, height, orientation, geometry.crop).ok()?;
    let (x, y) = geometry.display_to_developed(display_position);
    let around = |position: f64, size: usize| {
        let center = ((position * size as f64) as usize).min(size - 1);
        center.saturating_sub(PATCH_RADIUS)..=(center + PATCH_RADIUS).min(size - 1)
    };
    Some((around(x, width), around(y, height)))
}

#[cfg(test)]
mod tests {
    use rohditor_edit::NormalizedCropRect;

    use super::*;

    #[test]
    fn a_patch_follows_crop_and_orientation() {
        let mut geometry = GeometryAdjustments {
            crop: Some(NormalizedCropRect {
                left: 0.5,
                top: 0.0,
                right: 1.0,
                bottom: 0.5,
            }),
            ..GeometryAdjustments::default()
        };
        let size = (100, 60);
        let at =
            |geometry: &_, position| developed_patch(size, Orientation::Normal, geometry, position);
        assert_eq!(at(&geometry, (0.5, 0.5)), Some((73..=77, 13..=17)));
        assert_eq!(at(&geometry, (1.1, 0.5)), None);
        geometry.crop = None;
        assert_eq!(at(&geometry, (0.0, 1.0)), Some((0..=2, 57..=59)));
        geometry.orientation_override = Some(Orientation::Rotate180);
        assert_eq!(at(&geometry, (0.0, 1.0)), Some((97..=99, 0..=2)));
    }
}
