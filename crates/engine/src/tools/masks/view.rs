//! Coordinates of the Masks tool.
//!
//! Three spaces meet here:
//!
//! * **display**: normalized `0..1` over the image on screen (oriented and cropped), what a
//!   frontend gets from its pointer;
//! * **developed**: normalized `0..1` over the developed frame, what the recipe stores;
//! * **long-edge space**: the developed frame scaled so its long edge is `1`, with x and y on
//!   the same scale. Distances, angles and radii are true here, so all handle maths runs in it.
//!   One unit is one long-edge unit of the recipe's lengths.
//!
//! [`MaskView`] converts between display and long-edge space; [`Space`] between developed and
//! long-edge space.

use rohditor_core::OutputGeometry;
use rohditor_edit::{MASK_POINT_RANGE, MaskPoint};

/// A point or vector in long-edge space.
pub(super) type Vec2 = [f64; 2];

pub(super) fn sub(a: Vec2, b: Vec2) -> Vec2 {
    [a[0] - b[0], a[1] - b[1]]
}

pub(super) fn add(a: Vec2, b: Vec2) -> Vec2 {
    [a[0] + b[0], a[1] + b[1]]
}

pub(super) fn scale(a: Vec2, factor: f64) -> Vec2 {
    [a[0] * factor, a[1] * factor]
}

pub(super) fn dot(a: Vec2, b: Vec2) -> f64 {
    a[0].mul_add(b[0], a[1] * b[1])
}

pub(super) fn distance(a: Vec2, b: Vec2) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Distance from `point` to the segment `a`-`b` (to the nearer end when it projects outside;
/// to `a` when the segment is a single point).
pub(super) fn distance_to_segment(point: Vec2, a: Vec2, b: Vec2) -> f64 {
    let along = sub(b, a);
    let length_squared = dot(along, along);
    if length_squared == 0.0 {
        return distance(point, a);
    }
    let t = (dot(sub(point, a), along) / length_squared).clamp(0.0, 1.0);
    distance(point, add(a, scale(along, t)))
}

/// Developed frame to and from long-edge space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Space {
    /// Extent of the frame along each axis, in long-edge units (the long axis is `1`).
    extent: Vec2,
}

impl Space {
    /// Space of a developed frame of `width` x `height` pixels, both non-zero.
    pub(super) fn new((width, height): (usize, usize)) -> Self {
        let long_edge = width.max(height) as f64;
        Self {
            extent: [width as f64 / long_edge, height as f64 / long_edge],
        }
    }

    pub(super) fn point_to_space(self, point: MaskPoint) -> Vec2 {
        self.normalized_to_space((f64::from(point.x), f64::from(point.y)))
    }

    pub(super) fn normalized_to_space(self, point: (f64, f64)) -> Vec2 {
        [point.0 * self.extent[0], point.1 * self.extent[1]]
    }

    pub(super) fn space_to_normalized(self, point: Vec2) -> (f64, f64) {
        (point[0] / self.extent[0], point[1] / self.extent[1])
    }

    /// The recipe point for `point`, kept inside the range the recipe accepts.
    pub(super) fn space_to_point(self, point: Vec2) -> MaskPoint {
        let (x, y) = self.space_to_normalized(point);
        let clamp = |value: f64| {
            value.clamp(
                f64::from(MASK_POINT_RANGE.minimum),
                f64::from(MASK_POINT_RANGE.maximum),
            ) as f32
        };
        MaskPoint {
            x: clamp(x),
            y: clamp(y),
        }
    }

    /// Whether `point` is a position the recipe accepts, so a rigid move or rotation of several
    /// points can be rejected as a whole instead of distorting the shape by clamping.
    pub(super) fn in_range(self, point: Vec2) -> bool {
        let (x, y) = self.space_to_normalized(point);
        MASK_POINT_RANGE.contains(x as f32) && MASK_POINT_RANGE.contains(y as f32)
    }
}

/// How the image is shown, which the tool needs to turn pointer positions into mask edits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MaskView {
    /// Developed-frame size, orientation and crop of the displayed image. Any resolution of
    /// the developed frame works (a fit preview or the full frame); only its aspect matters,
    /// plus `screen_scale` below.
    pub geometry: OutputGeometry,
    /// Screen pixels per output pixel of `geometry`: the on-screen size of the image divided
    /// by `geometry.output_dimensions()`. Keeps handle grab distances a fixed size on screen.
    pub screen_scale: f64,
}

impl MaskView {
    pub(super) fn space(self) -> Space {
        Space::new(self.geometry.developed_dimensions())
    }

    /// A display position (`0..1` over the image on screen) in long-edge space.
    pub(super) fn display_to_space(self, position: (f64, f64)) -> Vec2 {
        self.space()
            .normalized_to_space(self.geometry.display_to_developed(position))
    }

    /// A long-edge-space point as a display position.
    pub(super) fn space_to_display(self, point: Vec2) -> (f64, f64) {
        self.geometry
            .developed_to_display(self.space().space_to_normalized(point))
    }

    /// A length in screen pixels in long-edge units.
    pub(super) fn screen_to_units(self, pixels: f64) -> f64 {
        pixels / (self.screen_scale * self.geometry.developed_long_edge())
    }
}

#[cfg(test)]
mod tests {
    use rohditor_image::Orientation;

    use super::*;

    #[test]
    fn long_edge_space_is_isotropic_and_round_trips() {
        let space = Space::new((600, 400));
        // A 0.3 x 0.3 normalized square is 180 x 120 pixels, not a square, in long-edge units.
        let corner = space.normalized_to_space((0.3, 0.3));
        assert!((corner[0] - 0.3).abs() < 1e-12);
        assert!((corner[1] - 0.2).abs() < 1e-12);
        let (x, y) = space.space_to_normalized(corner);
        assert!((x - 0.3).abs() < 1e-12 && (y - 0.3).abs() < 1e-12);
    }

    #[test]
    fn points_are_clamped_to_the_recipe_range() {
        let space = Space::new((600, 400));
        let far = space.space_to_point([10.0, -10.0]);
        assert_eq!(far.x, MASK_POINT_RANGE.maximum);
        assert_eq!(far.y, MASK_POINT_RANGE.minimum);
        assert!(space.in_range([0.5, 0.2]));
        assert!(!space.in_range([10.0, 0.0]));
    }

    #[test]
    fn a_screen_distance_shrinks_with_zoom_and_image_size() {
        let geometry = OutputGeometry::new(6000, 4000, Orientation::Normal, None).expect("valid");
        let at = |screen_scale| MaskView {
            geometry,
            screen_scale,
        };
        // Fit view of a 6000 px image on a 1200 px viewport: 0.2 screen px per image pixel,
        // so 12 screen px are 60 image px, which is 0.01 long-edge units.
        assert!((at(0.2).screen_to_units(12.0) - 0.01).abs() < 1e-12);
        assert!((at(0.4).screen_to_units(12.0) - 0.005).abs() < 1e-12);
    }

    #[test]
    fn display_positions_follow_the_orientation_into_long_edge_space() {
        // Rotate90 turns a 600x400 developed frame into a 400x600 display: the display's
        // top-left corner shows the developed frame's bottom-left corner.
        let geometry = OutputGeometry::new(600, 400, Orientation::Rotate90, None).expect("valid");
        let view = MaskView {
            geometry,
            screen_scale: 1.0,
        };
        let corner = view.display_to_space((0.0, 0.0));
        assert!((corner[0] - 0.0).abs() < 1e-12 && (corner[1] - 400.0 / 600.0).abs() < 1e-12);
        let back = view.space_to_display(corner);
        assert!(back.0.abs() < 1e-12 && back.1.abs() < 1e-12);
    }
}
