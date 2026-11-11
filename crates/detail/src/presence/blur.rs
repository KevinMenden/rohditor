//! Constant-time-per-pixel blurs for the presence stage.
//!
//! Clarity wants a Gaussian with a sigma of tens of pixels on an image of up to tens of
//! megapixels, which the direct kernel in [`crate::gaussian_blur`] cannot do interactively. A
//! Gaussian is closely approximated by three box blurs, and a box blur costs the same for any
//! radius when computed with a running sum.

use crate::{CancellationCheck, DetailError, Plane, for_each_row, mirror};

/// Number of box passes that make up the Gaussian approximation.
const GAUSSIAN_PASSES: usize = 3;

/// Blur `plane` with three box blurs that together approximate a Gaussian of `sigma` pixels.
/// Borders are mirrored, like [`crate::gaussian_blur`].
pub(super) fn fast_gaussian(
    plane: &Plane,
    sigma: f32,
    cancellation: &dyn CancellationCheck,
) -> Result<Plane, DetailError> {
    if !sigma.is_finite() || sigma <= 0.0 {
        return Err(DetailError::InvalidParameter {
            name: "sigma",
            value: sigma,
        });
    }
    separable_box_blur(plane, &gaussian_box_radii(sigma), cancellation)
}

/// One box blur of the given `radius` (window of `2 * radius + 1` pixels).
pub(super) fn box_mean(
    plane: &Plane,
    radius: usize,
    cancellation: &dyn CancellationCheck,
) -> Result<Plane, DetailError> {
    separable_box_blur(plane, &[radius], cancellation)
}

/// Radii of the successive boxes whose combined variance matches a Gaussian of `sigma`
/// ("Fast Almost-Gaussian Filtering", Kovesi; the formulation popularised by Ivan Kutskir).
fn gaussian_box_radii(sigma: f32) -> [usize; GAUSSIAN_PASSES] {
    let passes = GAUSSIAN_PASSES as f32;
    let ideal_width = (12.0 * sigma * sigma / passes + 1.0).sqrt();
    // Largest odd width not above the ideal one; the remaining passes use the next odd width.
    let mut lower = ideal_width.floor() as i32;
    if lower % 2 == 0 {
        lower -= 1;
    }
    let lower = lower.max(1);
    let narrow_passes = ((12.0 * sigma * sigma
        - passes * (lower * lower) as f32
        - 4.0 * passes * lower as f32
        - 3.0 * passes)
        / (-4.0 * lower as f32 - 4.0))
        .round() as usize;
    std::array::from_fn(|pass| {
        let width = if pass < narrow_passes {
            lower
        } else {
            lower + 2
        };
        ((width - 1) / 2) as usize
    })
}

/// Apply the horizontal box blurs to every row, transpose, repeat, and transpose back, so
/// each pass runs along contiguous memory.
fn separable_box_blur(
    plane: &Plane,
    radii: &[usize],
    cancellation: &dyn CancellationCheck,
) -> Result<Plane, DetailError> {
    let (width, height) = (plane.width(), plane.height());
    let mut data = plane.data().to_vec();
    let mut scratch = vec![0.0_f32; data.len()];
    blur_rows(&mut data, &mut scratch, width, radii, cancellation)?;
    let mut transposed = transpose(&data, width, height, cancellation)?;
    blur_rows(&mut transposed, &mut scratch, height, radii, cancellation)?;
    let restored = transpose(&transposed, height, width, cancellation)?;
    Plane::new(width, height, restored)
}

/// Blur each `width`-sample row of `data` once per radius, using `scratch` as the other buffer.
fn blur_rows(
    data: &mut Vec<f32>,
    scratch: &mut Vec<f32>,
    width: usize,
    radii: &[usize],
    cancellation: &dyn CancellationCheck,
) -> Result<(), DetailError> {
    for &radius in radii {
        let source: &[f32] = data;
        for_each_row(scratch, width, cancellation, |y, row| {
            box_blur_line(&source[y * width..(y + 1) * width], row, radius);
        })?;
        std::mem::swap(data, scratch);
    }
    Ok(())
}

/// Box mean of `line` into `out` with a running sum and mirrored borders.
fn box_blur_line(line: &[f32], out: &mut [f32], radius: usize) {
    let length = line.len();
    let radius = radius as isize;
    let window = (2 * radius + 1) as f32;
    let at = |index: isize| line[mirror(index, length)];
    let mut sum: f32 = (-radius..=radius).map(at).sum();
    for (x, out) in out.iter_mut().enumerate() {
        *out = sum / window;
        let x = x as isize;
        sum += at(x + radius + 1) - at(x - radius);
    }
}

/// `data` is `width` x `height`; the result is `height` x `width`.
fn transpose(
    data: &[f32],
    width: usize,
    height: usize,
    cancellation: &dyn CancellationCheck,
) -> Result<Vec<f32>, DetailError> {
    let mut out = vec![0.0_f32; data.len()];
    for_each_row(&mut out, height, cancellation, |x, row| {
        for (y, out) in row.iter_mut().enumerate() {
            *out = data[y * width + x];
        }
    })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NeverCancelled, gaussian_blur};

    fn pattern(width: usize, height: usize) -> Plane {
        let data = (0..width * height)
            .map(|i| ((i * 7919) % 1021) as f32 / 1021.0)
            .collect();
        Plane::new(width, height, data).unwrap()
    }

    #[test]
    fn box_radii_have_the_variance_of_the_requested_sigma() {
        for sigma in [1.0_f32, 2.5, 8.0, 40.0] {
            // A box of width w has variance (w^2 - 1) / 12.
            let variance: f32 = gaussian_box_radii(sigma)
                .iter()
                .map(|&r| ((2 * r + 1).pow(2) as f32 - 1.0) / 12.0)
                .sum();
            assert!(
                (variance.sqrt() - sigma).abs() < 0.2 * sigma,
                "sigma {sigma}: got {}",
                variance.sqrt()
            );
        }
    }

    #[test]
    fn preserves_constants_and_the_mean() {
        let flat = Plane::filled(23, 9, 0.4).unwrap();
        let blurred = fast_gaussian(&flat, 3.0, &NeverCancelled).unwrap();
        assert!(blurred.data().iter().all(|v| (v - 0.4).abs() < 1e-5));

        let noisy = pattern(31, 17);
        let blurred = fast_gaussian(&noisy, 2.0, &NeverCancelled).unwrap();
        let mean = |p: &Plane| p.data().iter().sum::<f32>() / p.data().len() as f32;
        assert!((mean(&noisy) - mean(&blurred)).abs() < 1e-3);
    }

    #[test]
    fn approximates_the_exact_gaussian_on_a_non_square_plane() {
        let plane = pattern(64, 37);
        let exact = gaussian_blur(&plane, 4.0, &NeverCancelled).unwrap();
        let fast = fast_gaussian(&plane, 4.0, &NeverCancelled).unwrap();
        let worst = exact
            .data()
            .iter()
            .zip(fast.data())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(worst < 0.03, "worst deviation {worst}");
    }

    #[test]
    fn box_mean_averages_a_window_and_mirrors_the_border() {
        let plane = Plane::new(5, 1, vec![1.0, 2.0, 3.0, 4.0, 5.0]).unwrap();
        let mean = box_mean(&plane, 1, &NeverCancelled).unwrap();
        // Edge pixel repeated: (1 + 1 + 2) / 3.
        assert!((mean.get(0, 0) - 4.0 / 3.0).abs() < 1e-6);
        assert!((mean.get(2, 0) - 3.0).abs() < 1e-6);
        assert!((mean.get(4, 0) - 14.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn rejects_bad_sigma_and_honours_cancellation() {
        let plane = pattern(8, 8);
        assert!(fast_gaussian(&plane, 0.0, &NeverCancelled).is_err());
        assert!(fast_gaussian(&plane, f32::NAN, &NeverCancelled).is_err());
        assert_eq!(
            fast_gaussian(&plane, 2.0, &|| true),
            Err(DetailError::Cancelled)
        );
    }
}
