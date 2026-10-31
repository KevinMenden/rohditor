//! 3x3 and 5x5 median filters with mirrored borders.

use crate::{CancellationCheck, DetailError, Plane, for_each_row, mirror};

/// Compare-exchange so that `v[a] <= v[b]`.
#[inline]
fn order(v: &mut [f32; 9], a: usize, b: usize) {
    if v[a] > v[b] {
        v.swap(a, b);
    }
}

/// Median of nine values via the classic 19-exchange network.
#[inline]
fn median9(mut v: [f32; 9]) -> f32 {
    for (a, b) in [
        (1, 2),
        (4, 5),
        (7, 8),
        (0, 1),
        (3, 4),
        (6, 7),
        (1, 2),
        (4, 5),
        (7, 8),
        (0, 3),
        (5, 8),
        (4, 7),
        (3, 6),
        (1, 4),
        (2, 5),
        (4, 7),
        (4, 2),
        (6, 4),
        (4, 2),
    ] {
        order(&mut v, a, b);
    }
    v[4]
}

fn median_filter<const N: usize>(
    plane: &Plane,
    cancellation: &dyn CancellationCheck,
    select: impl Fn(&mut [f32; N]) -> f32 + Sync,
) -> Result<Plane, DetailError> {
    let (w, h) = (plane.width(), plane.height());
    let r = (N.isqrt() / 2) as isize;
    let src = plane.data();
    let mut out = vec![0.0_f32; w * h];
    for_each_row(&mut out, w, cancellation, |y, row| {
        for (x, o) in row.iter_mut().enumerate() {
            let mut window = [0.0_f32; N];
            let mut n = 0;
            for dy in -r..=r {
                let sy = mirror(y as isize + dy, h);
                for dx in -r..=r {
                    window[n] = src[sy * w + mirror(x as isize + dx, w)];
                    n += 1;
                }
            }
            *o = select(&mut window);
        }
    })?;
    Plane::new(w, h, out)
}

/// 3x3 median (sorting network).
pub fn median3(plane: &Plane, cancellation: &dyn CancellationCheck) -> Result<Plane, DetailError> {
    median_filter::<9>(plane, cancellation, |w| median9(*w))
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
    fn network_matches_sort_on_permutations() {
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
