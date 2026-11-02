//! Small shared helpers for the mask shapes.

use rohditor_edit::MaskPoint;

/// Hermite smoothstep from 0 at `edge0` to 1 at `edge1`. With `edge0 >= edge1` it degenerates to
/// a hard step at `edge1` (`x >= edge1` gives 1), which is what a feather of 0 means.
#[inline]
pub(crate) fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge0 >= edge1 {
        return if x >= edge1 { 1.0 } else { 0.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Recipe point to pixel space. Pixel centres sit at `x + 0.5`.
#[inline]
pub(crate) fn to_pixels(point: MaskPoint, width: usize, height: usize) -> [f32; 2] {
    [point.x * width as f32, point.y * height as f32]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothstep_is_hermite_and_degenerates_to_a_step() {
        assert_eq!(smoothstep(0.0, 1.0, -1.0), 0.0);
        assert_eq!(smoothstep(0.0, 1.0, 2.0), 1.0);
        assert_eq!(smoothstep(0.0, 1.0, 0.5), 0.5);
        assert!((smoothstep(0.0, 1.0, 0.25) - 0.15625).abs() < 1e-6);
        assert_eq!(smoothstep(1.0, 1.0, 0.999), 0.0);
        assert_eq!(smoothstep(1.0, 1.0, 1.0), 1.0);
    }
}
