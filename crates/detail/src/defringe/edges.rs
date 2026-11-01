//! Edge mask for defringing: fringes only exist next to strong luminance
//! edges, so flat coloured areas (a purple flower) are never touched.

use crate::{CancellationCheck, DetailError, NoiseProfile, Plane, for_each_row};

/// A pixel is an edge when its gradient reaches this fraction of the largest
/// gradient within [`LOCAL_MAX_RADIUS`]: only the dominant edge of each
/// neighbourhood counts, not the weaker texture beside it.
pub const EDGE_FRACTION: f32 = 0.2;
/// 9x9 neighbourhood the gradient is compared with.
pub const LOCAL_MAX_RADIUS: usize = 4;
/// How far fringes reach from the edge pixels, in pixels.
pub const DILATE_RADIUS: usize = 3;
/// Gradients below this many noise standard deviations are not edges.
pub const NOISE_SIGMAS: f32 = 6.0;

/// 1 where `luma` is within [`DILATE_RADIUS`] pixels of a strong edge, else 0.
///
/// `noise` raises the edge threshold to a multiple of the expected noise at
/// each pixel's brightness, so grain is not mistaken for edges.
pub(super) fn edge_mask(
    luma: &Plane,
    noise: Option<&NoiseProfile>,
    cancellation: &dyn CancellationCheck,
) -> Result<Plane, DetailError> {
    let mut mask = gradient_magnitude(luma, cancellation)?;
    let local_max = max_filter(&mask, LOCAL_MAX_RADIUS, cancellation)?;
    let (w, h) = (mask.width(), mask.height());
    // In place: each output depends only on the same pixel of the inputs.
    for_each_row(mask.data_mut(), w, cancellation, |y, row| {
        let start = y * w;
        let local = &local_max.data()[start..start + w];
        let brightness = &luma.data()[start..start + w];
        for ((g, &max), &y) in row.iter_mut().zip(local).zip(brightness) {
            let floor = noise.map_or(0.0, |n| NOISE_SIGMAS * NOISE_SIGMAS * n.variance(y));
            *g = f32::from(*g > 0.0 && *g >= EDGE_FRACTION * max && *g * *g >= floor);
        }
    })?;
    drop(local_max);
    debug_assert_eq!((mask.width(), mask.height()), (w, h));
    max_filter(&mask, DILATE_RADIUS, cancellation)
}

/// Magnitude of the 3x3 Sobel gradient, scaled so an ideal step of height `h`
/// gives `h`. Borders repeat the edge pixel.
fn gradient_magnitude(
    luma: &Plane,
    cancellation: &dyn CancellationCheck,
) -> Result<Plane, DetailError> {
    let (w, h) = (luma.width(), luma.height());
    let src = luma.data();
    let mut out = vec![0.0_f32; w * h];
    for_each_row(&mut out, w, cancellation, |y, row| {
        let tap = |dy: isize| {
            let r = (y as isize + dy).clamp(0, h as isize - 1) as usize;
            &src[r * w..(r + 1) * w]
        };
        let (up, mid, down) = (tap(-1), tap(0), tap(1));
        for (x, g) in row.iter_mut().enumerate() {
            let (l, r) = (x.saturating_sub(1), (x + 1).min(w - 1));
            let gx = (down[r] - down[l]) + 2.0 * (mid[r] - mid[l]) + (up[r] - up[l]);
            let gy = (down[l] + 2.0 * down[x] + down[r]) - (up[l] + 2.0 * up[x] + up[r]);
            *g = (gx * gx + gy * gy).sqrt() * 0.25;
        }
    })?;
    Plane::new(w, h, out)
}

/// Rows each worker filters at a time; its horizontal-maximum rows (this many
/// plus `2 * radius`) stay in cache instead of becoming a second full plane.
const MAX_FILTER_BLOCK_ROWS: usize = 64;

/// Maximum over the `(2 * radius + 1)` square around each pixel (separable;
/// borders repeat the edge pixel).
pub(super) fn max_filter(
    plane: &Plane,
    radius: usize,
    cancellation: &dyn CancellationCheck,
) -> Result<Plane, DetailError> {
    use rayon::prelude::*;
    let (w, h) = (plane.width(), plane.height());
    let src = plane.data();
    let mut out = vec![0.0_f32; w * h];
    out.par_chunks_mut(w * MAX_FILTER_BLOCK_ROWS)
        .enumerate()
        .for_each(|(block, chunk)| {
            if cancellation.is_cancelled() {
                return;
            }
            let rows = chunk.len() / w;
            let first = (block * MAX_FILTER_BLOCK_ROWS) as isize - radius as isize;
            // Horizontal maxima of the source rows this block reads.
            let mut horizontal = vec![0.0_f32; (rows + 2 * radius) * w];
            for (i, row) in horizontal.chunks_mut(w).enumerate() {
                let y = (first + i as isize).clamp(0, h as isize - 1) as usize;
                row_max(&src[y * w..(y + 1) * w], row, radius);
            }
            for (y, row) in chunk.chunks_mut(w).enumerate() {
                row.copy_from_slice(&horizontal[(y + radius) * w..(y + radius + 1) * w]);
                for k in 1..=radius {
                    for r in [y + radius - k, y + radius + k] {
                        for (out, &v) in row.iter_mut().zip(&horizontal[r * w..(r + 1) * w]) {
                            *out = out.max(v);
                        }
                    }
                }
            }
        });
    if cancellation.is_cancelled() {
        return Err(DetailError::Cancelled);
    }
    Plane::new(w, h, out)
}

/// `out[x]` = maximum of `line` within `radius` pixels of `x` (edge pixels
/// repeat).
fn row_max(line: &[f32], out: &mut [f32], radius: usize) {
    let w = line.len();
    out.copy_from_slice(line);
    // The interior has every tap in range and vectorises.
    let interior = radius..w.saturating_sub(radius);
    for k in 1..=radius {
        if interior.start < interior.end {
            let (left, right) = (
                &line[interior.start - k..interior.end - k],
                &line[interior.start + k..interior.end + k],
            );
            for ((out, &l), &r) in out[interior.clone()].iter_mut().zip(left).zip(right) {
                *out = out.max(l).max(r);
            }
        }
        for x in (0..interior.start.min(w)).chain(interior.end.max(interior.start)..w) {
            out[x] = out[x]
                .max(line[(x + k).min(w - 1)])
                .max(line[x.saturating_sub(k)]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NeverCancelled;

    fn plane(w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> Plane {
        let data = (0..w * h).map(|i| f(i % w, i / w)).collect();
        Plane::new(w, h, data).unwrap()
    }

    #[test]
    fn gradient_of_a_step_equals_the_step_height() {
        let step = plane(9, 5, |x, _| if x < 4 { 0.1 } else { 0.7 });
        let g = gradient_magnitude(&step, &NeverCancelled).unwrap();
        assert!((g.get(3, 2) - 0.6).abs() < 1e-6 && (g.get(4, 2) - 0.6).abs() < 1e-6);
        assert_eq!(g.get(0, 2), 0.0);
        assert_eq!(g.get(8, 2), 0.0);
    }

    #[test]
    fn max_filter_is_a_square_window_and_clamps_at_borders() {
        let dot = plane(11, 7, |x, y| f32::from(x == 8 && y == 1));
        let out = max_filter(&dot, 2, &NeverCancelled).unwrap();
        for y in 0..7_usize {
            for x in 0..11_usize {
                let inside = x.abs_diff(8) <= 2 && y.abs_diff(1) <= 2;
                assert_eq!(out.get(x, y), f32::from(inside), "({x},{y})");
            }
        }
    }

    #[test]
    fn mask_covers_the_edge_surroundings_only() {
        // Strong edge at x = 10; the ripple at x = 7 sits in the edge's
        // neighbourhood and is below the 20% rule.
        let luma = plane(24, 6, |x, _| {
            if x < 10 {
                0.1 + if x == 7 { 0.03 } else { 0.0 }
            } else {
                0.8
            }
        });
        let mask = edge_mask(&luma, None, &NeverCancelled).unwrap();
        // Edge pixels are 9 and 10; the mask extends three pixels further.
        for x in 0..24 {
            let near = (6..=13).contains(&x);
            assert_eq!(mask.get(x, 3), f32::from(near), "x={x}");
        }
    }

    #[test]
    fn noise_floor_suppresses_grain_but_not_strong_edges() {
        let noise = NoiseProfile {
            a: 0.0,
            b: 0.01 * 0.01,
            bins: 4,
        };
        // A 0.04 step is 4 sigma: noise. A 0.5 step is an edge.
        let weak = plane(12, 4, |x, _| if x < 6 { 0.3 } else { 0.34 });
        let strong = plane(12, 4, |x, _| if x < 6 { 0.3 } else { 0.8 });
        let weak = edge_mask(&weak, Some(&noise), &NeverCancelled).unwrap();
        let strong = edge_mask(&strong, Some(&noise), &NeverCancelled).unwrap();
        assert!(weak.data().iter().all(|&v| v == 0.0));
        assert_eq!(strong.get(5, 1), 1.0);
    }
}
