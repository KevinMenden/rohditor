//! 3x3 and 5x5 median filters with mirrored borders.

use crate::{CancellationCheck, DetailError, Plane, for_each_row, mirror};

// Branch-free min/max (`<` rather than `f32::min`) so the 3x3 kernel compiles
// to packed `minps`/`maxps`. Planes are finite; NaN has no defined rank here.
#[inline(always)]
fn min(a: f32, b: f32) -> f32 {
    if a < b { a } else { b }
}

#[inline(always)]
fn max(a: f32, b: f32) -> f32 {
    if a > b { a } else { b }
}

#[inline(always)]
fn median_of_three(a: f32, b: f32, c: f32) -> f32 {
    max(min(a, b), min(max(a, b), c))
}

/// Median of a 3x3 window given as three vertical triples (left, centre,
/// right column). Sorting each column first leaves only three values per
/// rank to combine: the median of nine is the median of {the largest of the
/// column minima, the median of the column medians, the smallest of the
/// column maxima}. About 30 min/max operations per pixel.
#[inline(always)]
fn median_of_columns(columns: [[f32; 3]; 3]) -> f32 {
    let low = columns.map(|[a, b, c]| min(min(a, b), c));
    let mid = columns.map(|[a, b, c]| median_of_three(a, b, c));
    let high = columns.map(|[a, b, c]| max(max(a, b), c));
    median_of_three(
        max(max(low[0], low[1]), low[2]),
        median_of_three(mid[0], mid[1], mid[2]),
        min(min(high[0], high[1]), high[2]),
    )
}

/// Median of nine values laid out row by row.
#[inline]
fn median9(v: [f32; 9]) -> f32 {
    median_of_columns([[v[0], v[3], v[6]], [v[1], v[4], v[7]], [v[2], v[5], v[8]]])
}

fn median_filter<const N: usize>(
    plane: &Plane,
    cancellation: &dyn CancellationCheck,
    select: impl Fn(&mut [f32; N]) -> f32 + Sync,
) -> Result<Plane, DetailError> {
    let (w, h) = (plane.width(), plane.height());
    let side = N.isqrt();
    let r = side / 2;
    let src = plane.data();
    let mut out = vec![0.0_f32; w * h];
    // Pixels at least `r` from both side edges read contiguous taps and need
    // no mirroring; only the `r`-wide borders do.
    let interior = r.min(w)..w.saturating_sub(r).max(r.min(w));
    for_each_row(&mut out, w, cancellation, |y, row| {
        // Start offset of each tap row, mirrored at the top and bottom.
        let mut starts = [0_usize; 5];
        for (k, start) in starts[..side].iter_mut().enumerate() {
            *start = mirror(y as isize + k as isize - r as isize, h) * w;
        }
        let mut window = [0.0_f32; N];
        for x in (0..interior.start).chain(interior.end..w) {
            let mut n = 0;
            for &start in &starts[..side] {
                for dx in 0..side {
                    let sx = mirror(x as isize + dx as isize - r as isize, w);
                    window[n] = src[start + sx];
                    n += 1;
                }
            }
            row[x] = select(&mut window);
        }
        for x in interior.clone() {
            for (k, &start) in starts[..side].iter().enumerate() {
                let from = start + x - r;
                window[k * side..(k + 1) * side].copy_from_slice(&src[from..from + side]);
            }
            row[x] = select(&mut window);
        }
    })?;
    Plane::new(w, h, out)
}

/// 3x3 median.
pub fn median3(plane: &Plane, cancellation: &dyn CancellationCheck) -> Result<Plane, DetailError> {
    let (w, h) = (plane.width(), plane.height());
    let src = plane.data();
    let mut out = vec![0.0_f32; w * h];
    for_each_row(&mut out, w, cancellation, |y, row| {
        // The three tap rows, mirrored at the top and bottom.
        let rows: [&[f32]; 3] = std::array::from_fn(|k| {
            let start = mirror(y as isize + k as isize - 1, h) * w;
            &src[start..start + w]
        });
        // Columns 1..w-1 have all taps in range and vectorise; the two edge
        // pixels (every pixel of a plane narrower than three) mirror columns.
        let borders = if w >= 3 { [0, w - 1] } else { [0, 1] };
        for &x in borders.iter().take(w.min(2)) {
            row[x] = median9(std::array::from_fn(|n| {
                rows[n / 3][mirror(x as isize + (n % 3) as isize - 1, w)]
            }));
        }
        if w >= 3 {
            median3_interior(rows, &mut row[1..w - 1]);
        }
    })?;
    Plane::new(w, h, out)
}

/// `out[t]` = median of the 3x3 window centred on column `t + 1` of `rows`.
fn median3_interior(rows: [&[f32]; 3], out: &mut [f32]) {
    let n = out.len();
    // Equal-length windows let the compiler drop bounds checks and vectorise.
    let taps: [[&[f32]; 3]; 3] =
        std::array::from_fn(|r| std::array::from_fn(|k| &rows[r][k..k + n]));
    for (t, out) in out.iter_mut().enumerate() {
        *out = median_of_columns(std::array::from_fn(|k| {
            [taps[0][k][t], taps[1][k][t], taps[2][k][t]]
        }));
    }
}

/// 5x5 median.
pub fn median5(plane: &Plane, cancellation: &dyn CancellationCheck) -> Result<Plane, DetailError> {
    median_filter::<25>(plane, cancellation, |w| {
        *w.select_nth_unstable_by(12, f32::total_cmp).1
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NeverCancelled;

    #[test]
    fn nine_value_median_matches_sort_on_permutations() {
        let mut seed = 12345_u32;
        for _ in 0..2000 {
            let mut v = [0.0_f32; 9];
            for s in &mut v {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                *s = (seed >> 16) as f32;
            }
            let mut sorted = v;
            sorted.sort_by(f32::total_cmp);
            assert_eq!(median9(v), sorted[4]);
        }
    }

    /// Straightforward mirrored-window median to check the fast path against.
    fn reference(plane: &Plane, r: isize) -> Vec<f32> {
        let (w, h) = (plane.width(), plane.height());
        let mut out = Vec::new();
        for y in 0..h as isize {
            for x in 0..w as isize {
                let mut window = Vec::new();
                for dy in -r..=r {
                    for dx in -r..=r {
                        window.push(plane.get(mirror(x + dx, w), mirror(y + dy, h)));
                    }
                }
                window.sort_by(f32::total_cmp);
                out.push(window[window.len() / 2]);
            }
        }
        out
    }

    #[test]
    fn matches_a_naive_mirrored_median_including_narrow_planes() {
        // Asymmetric sizes, including planes narrower than the window.
        for (w, h) in [(1, 1), (2, 5), (3, 3), (4, 2), (7, 9), (13, 6)] {
            let data = (0..w * h).map(|i| ((i * 37 + 11) % 53) as f32).collect();
            let plane = Plane::new(w, h, data).unwrap();
            assert_eq!(
                median3(&plane, &NeverCancelled).unwrap().data(),
                reference(&plane, 1),
                "3x3 {w}x{h}"
            );
            assert_eq!(
                median5(&plane, &NeverCancelled).unwrap().data(),
                reference(&plane, 2),
                "5x5 {w}x{h}"
            );
        }
    }

    #[test]
    fn removes_an_isolated_outlier() {
        let (w, h) = (9, 7);
        let mut data = vec![0.3_f32; w * h];
        data[3 * w + 4] = 50.0;
        data[0] = -40.0; // corner outlier exercises the mirrored border
        let plane = Plane::new(w, h, data).unwrap();
        for out in [
            median3(&plane, &NeverCancelled).unwrap(),
            median5(&plane, &NeverCancelled).unwrap(),
        ] {
            assert!(out.data().iter().all(|v| (v - 0.3).abs() < 1e-6));
        }
    }

    #[test]
    fn preserves_a_straight_step_edge() {
        let (w, h) = (12, 6);
        let data = (0..w * h)
            .map(|i| if i % w < 6 { 0.0 } else { 1.0 })
            .collect();
        let plane = Plane::new(w, h, data).unwrap();
        assert_eq!(median3(&plane, &NeverCancelled).unwrap(), plane);
        assert_eq!(median5(&plane, &NeverCancelled).unwrap(), plane);
    }
}
