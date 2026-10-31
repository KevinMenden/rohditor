//! Separable Gaussian blur with mirrored borders.

use crate::{CancellationCheck, DetailError, Plane, for_each_row, mirror};

/// Normalised 1-D Gaussian kernel of radius `ceil(3 * sigma)` (odd length).
///
/// `sigma` must be finite and positive; use [`gaussian_blur`] for validation.
#[must_use]
pub fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let radius = (3.0 * sigma).ceil() as i32;
    let mut kernel: Vec<_> = (-radius..=radius)
        .map(|x| (-0.5 * (x as f32 / sigma).powi(2)).exp())
        .collect();
    let sum: f32 = kernel.iter().sum();
    kernel.iter_mut().for_each(|v| *v /= sum);
    kernel
}

/// Blur `plane` with a Gaussian of standard deviation `sigma` pixels.
pub fn gaussian_blur(
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
    let kernel = gaussian_kernel(sigma);
    let radius = (kernel.len() / 2) as isize;
    let (w, h) = (plane.width(), plane.height());
    let src = plane.data();
    let mut horizontal = vec![0.0_f32; w * h];
    for_each_row(&mut horizontal, w, cancellation, |y, row| {
        let line = &src[y * w..(y + 1) * w];
        for (x, o) in row.iter_mut().enumerate() {
            *o = kernel
                .iter()
                .enumerate()
                .map(|(k, wt)| wt * line[mirror(x as isize + k as isize - radius, w)])
                .sum();
        }
    })?;
    let mut out = vec![0.0_f32; w * h];
    for_each_row(&mut out, w, cancellation, |y, row| {
        for (k, wt) in kernel.iter().enumerate() {
            let sy = mirror(y as isize + k as isize - radius, h);
            for (o, s) in row.iter_mut().zip(&horizontal[sy * w..(sy + 1) * w]) {
                *o += wt * s;
            }
        }
    })?;
    Plane::new(w, h, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NeverCancelled;

    #[test]
    fn kernel_is_normalised_symmetric_and_odd() {
        let k = gaussian_kernel(1.3);
        assert_eq!(k.len() % 2, 1);
        assert!((k.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert!((k[0] - k[k.len() - 1]).abs() < 1e-9);
    }

    #[test]
    fn preserves_constants_and_spreads_an_impulse() {
        let flat = Plane::filled(9, 5, 0.7).unwrap();
        let blurred = gaussian_blur(&flat, 1.5, &NeverCancelled).unwrap();
        assert!(blurred.data().iter().all(|v| (v - 0.7).abs() < 1e-5));

        let mut data = vec![0.0; 15 * 11];
        data[5 * 15 + 7] = 1.0;
        let impulse = Plane::new(15, 11, data).unwrap();
        let out = gaussian_blur(&impulse, 1.0, &NeverCancelled).unwrap();
        assert!((out.data().iter().sum::<f32>() - 1.0).abs() < 1e-5);
        assert!(out.get(7, 5) > out.get(8, 5) && out.get(8, 5) > out.get(10, 5));
        assert!((out.get(6, 5) - out.get(8, 5)).abs() < 1e-7);
    }

    #[test]
    fn rejects_bad_sigma() {
        let p = Plane::filled(4, 4, 0.0).unwrap();
        assert!(gaussian_blur(&p, 0.0, &NeverCancelled).is_err());
        assert!(gaussian_blur(&p, f32::NAN, &NeverCancelled).is_err());
    }
}
