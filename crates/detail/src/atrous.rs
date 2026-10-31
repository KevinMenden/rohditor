//! À-trous (stationary) wavelet decomposition with the B3-spline kernel.
//!
//! Level `l` smooths with `[1, 4, 6, 4, 1] / 16` dilated by `2^l`; the detail
//! band is the difference between consecutive smoothings, so summing the
//! details and the residual reconstructs the input exactly.

use crate::{CancellationCheck, DetailError, Plane, for_each_row, mirror};

const B3: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

/// Result of a decomposition: `details[0]` is the finest band.
#[derive(Debug, Clone, PartialEq)]
pub struct Decomposition {
    pub details: Vec<Plane>,
    pub residual: Plane,
}

impl Decomposition {
    /// Sum residual and all detail bands back into one plane.
    #[must_use]
    pub fn reconstruct(&self) -> Plane {
        let mut out = self.residual.clone();
        for band in &self.details {
            for (o, d) in out.data_mut().iter_mut().zip(band.data()) {
                *o += d;
            }
        }
        out
    }
}

/// Source pixels a `levels`-level decomposition reads beyond an output pixel
/// (per side). Tiles processed independently need at least this much overlap.
#[must_use]
pub const fn atrous_halo(levels: u32) -> usize {
    // Each level reaches 2 taps * 2^level pixels: sum is 2 * (2^levels - 1).
    2 * ((1_usize << levels) - 1)
}

fn check_levels(levels: u32) -> Result<(), DetailError> {
    if (1..=16).contains(&levels) {
        Ok(())
    } else {
        Err(DetailError::InvalidParameter {
            name: "levels",
            value: levels as f32,
        })
    }
}

/// Separable B3 smoothing of `input` with tap spacing `step`.
fn smooth(
    input: &Plane,
    step: usize,
    cancellation: &dyn CancellationCheck,
) -> Result<Plane, DetailError> {
    let (w, h) = (input.width(), input.height());
    let src = input.data();
    let mut horizontal = vec![0.0_f32; w * h];
    for_each_row(&mut horizontal, w, cancellation, |y, row| {
        let line = &src[y * w..(y + 1) * w];
        for (x, out) in row.iter_mut().enumerate() {
            *out = (0..5)
                .map(|k| B3[k] * line[mirror(x as isize + (k as isize - 2) * step as isize, w)])
                .sum();
        }
    })?;
    let mut out = vec![0.0_f32; w * h];
    for_each_row(&mut out, w, cancellation, |y, row| {
        let rows: [&[f32]; 5] = std::array::from_fn(|k| {
            let sy = mirror(y as isize + (k as isize - 2) * step as isize, h);
            &horizontal[sy * w..(sy + 1) * w]
        });
        for (x, out) in row.iter_mut().enumerate() {
            *out = (0..5).map(|k| B3[k] * rows[k][x]).sum();
        }
    })?;
    Plane::new(w, h, out)
}

/// Edge-avoiding smoothing (Dammertz et al. 2010): the B3 weights are
/// multiplied by `exp(-(guide_p - guide_q)^2 / sigma^2)` and renormalised, so
/// averaging does not cross edges visible in `guide`.
fn smooth_edge_avoiding(
    input: &Plane,
    guide: &Plane,
    step: usize,
    inv_sigma_sq: f32,
    cancellation: &dyn CancellationCheck,
) -> Result<Plane, DetailError> {
    let (w, h) = (input.width(), input.height());
    let (src, gd) = (input.data(), guide.data());
    let mut out = vec![0.0_f32; w * h];
    for_each_row(&mut out, w, cancellation, |y, row| {
        let ys: [usize; 5] =
            std::array::from_fn(|k| mirror(y as isize + (k as isize - 2) * step as isize, h));
        for (x, out) in row.iter_mut().enumerate() {
            let centre = gd[y * w + x];
            let (mut sum, mut weight_sum) = (0.0_f32, 0.0_f32);
            for (j, &sy) in ys.iter().enumerate() {
                for (i, &tap) in B3.iter().enumerate() {
                    let sx = mirror(x as isize + (i as isize - 2) * step as isize, w);
                    let d = gd[sy * w + sx] - centre;
                    let weight = B3[j] * tap * (-d * d * inv_sigma_sq).exp();
                    sum += weight * src[sy * w + sx];
                    weight_sum += weight;
                }
            }
            // The centre tap has weight > 0, so weight_sum never vanishes.
            *out = sum / weight_sum;
        }
    })?;
    Plane::new(w, h, out)
}

/// Decompose into `levels` detail bands plus a residual.
pub fn atrous_decompose(
    plane: &Plane,
    levels: u32,
    cancellation: &dyn CancellationCheck,
) -> Result<Decomposition, DetailError> {
    check_levels(levels)?;
    decompose_with(plane, levels, |current, level| {
        smooth(current, 1 << level, cancellation)
    })
}

/// Like [`atrous_decompose`], but each smoothing is weighted by similarity in
/// `guide` (same size as `plane`) with range scale `sigma_range`. Detail bands
/// still sum back to the input exactly, but strong guide edges stay in the
/// detail bands of fine levels rather than being averaged across.
pub fn edge_avoiding_decompose(
    plane: &Plane,
    guide: &Plane,
    levels: u32,
    sigma_range: f32,
    cancellation: &dyn CancellationCheck,
) -> Result<Decomposition, DetailError> {
    check_levels(levels)?;
    plane.same_size(guide)?;
    if !sigma_range.is_finite() || sigma_range <= 0.0 {
        return Err(DetailError::InvalidParameter {
            name: "sigma_range",
            value: sigma_range,
        });
    }
    let inv = 1.0 / (sigma_range * sigma_range);
    decompose_with(plane, levels, |current, level| {
        smooth_edge_avoiding(current, guide, 1 << level, inv, cancellation)
    })
}

fn decompose_with(
    plane: &Plane,
    levels: u32,
    smoother: impl Fn(&Plane, u32) -> Result<Plane, DetailError>,
) -> Result<Decomposition, DetailError> {
    let mut details = Vec::with_capacity(levels as usize);
    let mut current = plane.clone();
    for level in 0..levels {
        let smoothed = smoother(&current, level)?;
        let mut detail = current;
        for (d, s) in detail.data_mut().iter_mut().zip(smoothed.data()) {
            *d -= s;
        }
        details.push(detail);
        current = smoothed;
    }
    Ok(Decomposition {
        details,
        residual: current,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NeverCancelled;

    fn test_plane(w: usize, h: usize) -> Plane {
        let data = (0..w * h)
            .map(|i| ((i * 7919 % 251) as f32) / 251.0)
            .collect();
        Plane::new(w, h, data).unwrap()
    }

    #[test]
    fn reconstruction_is_exact() {
        // Asymmetric and smaller than the dilation of the coarse levels.
        for (w, h) in [(67, 41), (5, 3), (1, 9)] {
            let plane = test_plane(w, h);
            let d = atrous_decompose(&plane, 5, &NeverCancelled).unwrap();
            let back = d.reconstruct();
            for (a, b) in plane.data().iter().zip(back.data()) {
                assert!((a - b).abs() < 1e-6, "{w}x{h}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn constant_plane_has_zero_detail() {
        let plane = Plane::filled(20, 11, 0.4).unwrap();
        let d = atrous_decompose(&plane, 4, &NeverCancelled).unwrap();
        for band in &d.details {
            assert!(band.data().iter().all(|v| v.abs() < 1e-6));
        }
    }

    #[test]
    fn halo_matches_tap_reach() {
        assert_eq!(atrous_halo(1), 2);
        assert_eq!(atrous_halo(3), 14);
    }

    #[test]
    fn edge_avoiding_does_not_cross_a_step() {
        let (w, h) = (40, 12);
        let data = (0..w * h)
            .map(|i| if i % w < 20 { 0.0 } else { 1.0 })
            .collect();
        let step = Plane::new(w, h, data).unwrap();
        let d = edge_avoiding_decompose(&step, &step, 3, 0.1, &NeverCancelled).unwrap();
        for y in 0..h {
            for x in 0..w {
                let expected = if x < 20 { 0.0 } else { 1.0 };
                assert!((d.residual.get(x, y) - expected).abs() < 1e-3, "({x},{y})");
            }
        }
        // Still exact.
        let back = d.reconstruct();
        assert!(
            back.data()
                .iter()
                .zip(step.data())
                .all(|(a, b)| (a - b).abs() < 1e-6)
        );
    }

    #[test]
    fn plain_atrous_does_blur_the_step() {
        let (w, h) = (40, 4);
        let data = (0..w * h)
            .map(|i| if i % w < 20 { 0.0 } else { 1.0 })
            .collect();
        let step = Plane::new(w, h, data).unwrap();
        let d = atrous_decompose(&step, 3, &NeverCancelled).unwrap();
        assert!(d.residual.get(19, 1) > 0.05);
    }

    #[test]
    fn cancellation_is_reported() {
        let plane = test_plane(16, 16);
        let result = atrous_decompose(&plane, 2, &|| true);
        assert_eq!(result.unwrap_err(), DetailError::Cancelled);
    }
}
