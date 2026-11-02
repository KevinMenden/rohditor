//! Helpers shared by the unit tests.

use crate::MaskFrame;

/// A small asymmetric frame: neither square nor a multiple of the brush band height.
pub(crate) const FRAME: MaskFrame = MaskFrame {
    width: 37,
    height: 23,
};

/// Evaluates a whole plane through a row evaluator `f(y, x_start, row)`.
pub(crate) fn plane_of(frame: MaskFrame, mut f: impl FnMut(usize, usize, &mut [f32])) -> Vec<f32> {
    let mut plane = vec![0.0; frame.width * frame.height];
    for (y, row) in plane.chunks_exact_mut(frame.width).enumerate() {
        f(y, 0, row);
    }
    plane
}
