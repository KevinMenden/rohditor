//! Radial (elliptical) gradient.

use rohditor_edit::RadialGradient;

use crate::math::{smoothstep, to_pixels};
use crate::{MaskError, MaskFrame};

/// A radial gradient resolved to pixel space.
///
/// With `r` the elliptical radius (1 on the ellipse outline defined by `radius_x`, `radius_y`
/// and the rotation), alpha is `1 - smoothstep(1 - feather, 1, r)`: 1 inside the solid core, 0
/// at and beyond the outline. Radii are in long-edge units, so the shape is isotropic in pixels
/// whatever the frame's aspect ratio. The rotation turns the ellipse clockwise on screen
/// (y points down). The component's `invert` flag gives the outside; it is applied by the caller.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PreparedRadial {
    center: [f32; 2],
    /// Rotation of the ellipse, as cosine and sine.
    cos: f32,
    sin: f32,
    /// Reciprocal radii in pixels.
    inv_radius_x: f32,
    inv_radius_y: f32,
    feather: f32,
}

impl PreparedRadial {
    pub(crate) fn prepare(radial: &RadialGradient, frame: MaskFrame) -> Result<Self, MaskError> {
        let long_edge = frame.long_edge();
        let radius_x = radial.radius_x * long_edge;
        let radius_y = radial.radius_y * long_edge;
        if !(radius_x.is_finite() && radius_y.is_finite() && radius_x > 0.0 && radius_y > 0.0) {
            return Err(MaskError::Degenerate {
                shape: "radial",
                reason: "radii must be positive and finite",
            });
        }
        let (sin, cos) = radial.angle_degrees.to_radians().sin_cos();
        Ok(Self {
            center: to_pixels(radial.center, frame.width, frame.height),
            cos,
            sin,
            inv_radius_x: 1.0 / radius_x,
            inv_radius_y: 1.0 / radius_y,
            feather: radial.feather.clamp(0.0, 1.0),
        })
    }

    pub(crate) fn evaluate_row(&self, y: usize, x_start: usize, out: &mut [f32]) {
        let dy = y as f32 + 0.5 - self.center[1];
        let inner = 1.0 - self.feather;
        for (i, alpha) in out.iter_mut().enumerate() {
            let dx = (x_start + i) as f32 + 0.5 - self.center[0];
            // Rotate into the ellipse's own axes, then scale to unit radii.
            let u = (dx * self.cos + dy * self.sin) * self.inv_radius_x;
            let v = (-dx * self.sin + dy * self.cos) * self.inv_radius_y;
            let r = (u * u + v * v).sqrt();
            *alpha = 1.0 - smoothstep(inner, 1.0, r);
        }
    }
}

#[cfg(test)]
mod tests {
    use rohditor_edit::MaskPoint;

    use super::*;
    use crate::test_support::{FRAME, plane_of};

    fn radial(radius_x: f32, radius_y: f32, angle: f32, feather: f32) -> RadialGradient {
        RadialGradient {
            center: MaskPoint { x: 0.5, y: 0.5 },
            radius_x,
            radius_y,
            angle_degrees: angle,
            feather,
        }
    }

    fn evaluate(radial: &RadialGradient, frame: MaskFrame) -> Vec<f32> {
        let prepared = PreparedRadial::prepare(radial, frame).unwrap();
        plane_of(frame, |y, x, out| prepared.evaluate_row(y, x, out))
    }

    #[test]
    fn a_circle_stays_circular_in_pixels_on_a_non_square_frame() {
        // Radius 0.25 of the long edge (37 px) is 9.25 px in both directions.
        let plane = evaluate(&radial(0.25, 0.25, 0.0, 0.0), FRAME);
        let (cx, cy) = (FRAME.width as f32 / 2.0, FRAME.height as f32 / 2.0);
        for y in 0..FRAME.height {
            for x in 0..FRAME.width {
                let distance = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
                let inside = plane[y * FRAME.width + x] == 1.0;
                if (distance - 9.25).abs() > 1e-3 {
                    assert_eq!(inside, distance < 9.25, "pixel ({x}, {y})");
                }
            }
        }
        // Horizontal and vertical extent match: 18 or 19 pixels across the centre lines.
        let across_x = plane[(FRAME.height / 2) * FRAME.width..][..FRAME.width]
            .iter()
            .filter(|a| **a == 1.0)
            .count();
        let across_y = (0..FRAME.height)
            .filter(|y| plane[y * FRAME.width + FRAME.width / 2] == 1.0)
            .count();
        assert!(across_x.abs_diff(across_y) <= 1, "{across_x} vs {across_y}");
    }

    #[test]
    fn feather_zero_is_a_hard_edge_and_feather_one_falls_from_the_centre() {
        let hard = evaluate(&radial(0.25, 0.25, 0.0, 0.0), FRAME);
        assert!(hard.iter().all(|a| *a == 0.0 || *a == 1.0));
        assert!(hard.contains(&0.0) && hard.contains(&1.0));

        let soft = evaluate(&radial(0.4, 0.4, 0.0, 1.0), FRAME);
        let soft_values = soft.iter().filter(|a| **a > 0.0 && **a < 1.0).count();
        assert!(soft_values > FRAME.width * FRAME.height / 4);
    }

    #[test]
    fn rotating_a_quarter_turn_swaps_the_radii() {
        // 23x23 keeps the transpose exact: rotate 90 degrees == swap the radii.
        let square = MaskFrame {
            width: 23,
            height: 23,
        };
        let rotated = evaluate(&radial(0.4, 0.15, 90.0, 0.3), square);
        let swapped = evaluate(&radial(0.15, 0.4, 0.0, 0.3), square);
        for (a, b) in rotated.iter().zip(&swapped) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn rejects_non_positive_radii() {
        assert!(matches!(
            PreparedRadial::prepare(&radial(0.0, 0.2, 0.0, 0.0), FRAME),
            Err(MaskError::Degenerate { .. })
        ));
    }
}
