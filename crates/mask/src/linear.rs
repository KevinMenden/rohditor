//! Linear gradient.

use rohditor_edit::LinearGradient;

use crate::math::{smoothstep, to_pixels};
use crate::{MaskError, MaskFrame};

/// A linear gradient resolved to pixel space.
///
/// Alpha is 1 on the `start` side and 0 past `end`, with a smoothstep in between, measured
/// along `start -> end`. Beyond either end of the segment the gradient continues as parallel
/// bands (alpha stays 1 before `start` and 0 after `end`), like Lightroom.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PreparedLinear {
    start: [f32; 2],
    /// `start -> end` divided by its squared length, so `dot(p - start, direction)` is the
    /// position along the gradient in `0..1` between the two points.
    direction: [f32; 2],
}

impl PreparedLinear {
    pub(crate) fn prepare(gradient: &LinearGradient, frame: MaskFrame) -> Result<Self, MaskError> {
        let start = to_pixels(gradient.start, frame.width, frame.height);
        let end = to_pixels(gradient.end, frame.width, frame.height);
        let delta = [end[0] - start[0], end[1] - start[1]];
        let length_squared = delta[0] * delta[0] + delta[1] * delta[1];
        if !length_squared.is_finite() || length_squared <= 0.0 {
            return Err(MaskError::Degenerate {
                shape: "linear",
                reason: "start and end must be distinct, finite points",
            });
        }
        Ok(Self {
            start,
            direction: [delta[0] / length_squared, delta[1] / length_squared],
        })
    }

    pub(crate) fn evaluate_row(&self, y: usize, x_start: usize, out: &mut [f32]) {
        let py = y as f32 + 0.5 - self.start[1];
        // Position along the gradient is affine in x: compute it once and step.
        let t0 =
            (x_start as f32 + 0.5 - self.start[0]) * self.direction[0] + py * self.direction[1];
        for (i, alpha) in out.iter_mut().enumerate() {
            let t = t0 + i as f32 * self.direction[0];
            *alpha = 1.0 - smoothstep(0.0, 1.0, t);
        }
    }
}

#[cfg(test)]
mod tests {
    use rohditor_edit::MaskPoint;

    use super::*;
    use crate::test_support::{FRAME, plane_of};

    fn gradient(start: (f32, f32), end: (f32, f32)) -> LinearGradient {
        LinearGradient {
            start: MaskPoint {
                x: start.0,
                y: start.1,
            },
            end: MaskPoint { x: end.0, y: end.1 },
        }
    }

    fn evaluate(gradient: &LinearGradient, frame: MaskFrame) -> Vec<f32> {
        let prepared = PreparedLinear::prepare(gradient, frame).unwrap();
        plane_of(frame, |y, x, out| prepared.evaluate_row(y, x, out))
    }

    #[test]
    fn alpha_falls_monotonically_between_start_and_end() {
        let plane = evaluate(&gradient((0.2, 0.0), (0.8, 0.0)), FRAME);
        let row = &plane[..FRAME.width];
        assert!(row.windows(2).all(|pair| pair[0] >= pair[1]));
        // Parallel bands before start and after end.
        assert_eq!(row[0], 1.0);
        assert_eq!(row[FRAME.width - 1], 0.0);
        // Constant along the perpendicular direction.
        for y in 1..FRAME.height {
            assert_eq!(&plane[y * FRAME.width..(y + 1) * FRAME.width], row);
        }
        // Midpoint of the segment is exactly half.
        let mid = evaluate(
            &gradient((0.0, 0.0), (1.0, 0.0)),
            MaskFrame {
                width: 5,
                height: 1,
            },
        );
        assert!((mid[2] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn reversing_the_direction_flips_the_alpha() {
        let forward = evaluate(&gradient((0.25, 0.5), (0.75, 0.5)), FRAME);
        let reverse = evaluate(&gradient((0.75, 0.5), (0.25, 0.5)), FRAME);
        for (f, r) in forward.iter().zip(&reverse) {
            assert!((f + r - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn rotated_gradient_on_a_transposed_frame_gives_the_transposed_plane() {
        let wide = MaskFrame {
            width: 37,
            height: 23,
        };
        let tall = MaskFrame {
            width: 23,
            height: 37,
        };
        // Swapping x and y in both the frame and the points transposes the image.
        let a = evaluate(&gradient((0.1, 0.3), (0.6, 0.9)), wide);
        let b = evaluate(&gradient((0.3, 0.1), (0.9, 0.6)), tall);
        for y in 0..wide.height {
            for x in 0..wide.width {
                let transposed = b[x * tall.width + y];
                assert!((a[y * wide.width + x] - transposed).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn rejects_coincident_points() {
        let frame = FRAME;
        assert!(matches!(
            PreparedLinear::prepare(&gradient((0.5, 0.5), (0.5, 0.5)), frame),
            Err(MaskError::Degenerate { .. })
        ));
    }
}
