//! Seeded Poisson–Gaussian sensor-noise injection with known parameters.
//!
//! Noise is added to ground-truth signal values (which stand for sensor
//! samples) *before* [`crate::truth::remosaic`], so demosaicing sees the same
//! correlated noise a real capture would produce. The Poisson part uses its
//! Gaussian approximation: `sigma^2(x) = a * x + b`.

use rohditor_image::LinearRgbImage;

use crate::ReferenceError;

/// Noise variance model `sigma^2(x) = a * x + b` on `[0, 1]` signals.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoiseModel {
    pub a: f32,
    pub b: f32,
}

impl NoiseModel {
    #[must_use]
    pub fn sigma(self, x: f32) -> f32 {
        (self.a * x.max(0.0) + self.b).sqrt()
    }
}

/// Three ISO-like levels (roughly ISO 400 / 1600 / 6400 on a modern sensor).
pub const NOISE_LEVELS: [(&str, NoiseModel); 3] = [
    (
        "iso-low",
        NoiseModel {
            a: 0.002,
            b: 0.0001,
        },
    ),
    (
        "iso-mid",
        NoiseModel {
            a: 0.008,
            b: 0.0004,
        },
    ),
    (
        "iso-high",
        NoiseModel {
            a: 0.032,
            b: 0.0016,
        },
    ),
];

/// SplitMix64 + Box–Muller; deterministic and dependency-free.
struct Gaussian {
    state: u64,
}

impl Gaussian {
    fn uniform(&mut self) -> f64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1_u64 << 53) as f64
    }

    fn sample(&mut self) -> f32 {
        let (u, v) = (self.uniform().max(1e-12), self.uniform());
        ((-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()) as f32
    }
}

/// Add independent noise to every sample of `image`. Same seed, same noise.
pub fn add_noise(
    image: &LinearRgbImage<f32>,
    model: NoiseModel,
    seed: u64,
) -> Result<LinearRgbImage<f32>, ReferenceError> {
    let mut rng = Gaussian { state: seed };
    let mut data = Vec::with_capacity(image.width() * image.height() * 3);
    for y in 0..image.height() {
        let start = y * image.row_stride();
        for &v in &image.data()[start..start + image.width() * 3] {
            data.push(v + model.sigma(v) * rng.sample());
        }
    }
    Ok(LinearRgbImage::new(
        image.width(),
        image.height(),
        image.width() * 3,
        image.space(),
        data,
    )?)
}

#[cfg(test)]
mod tests {
    use rohditor_image::LinearRgbSpace;

    use super::*;

    fn flat(value: f32) -> LinearRgbImage<f32> {
        LinearRgbImage::new(
            64,
            48,
            64 * 3,
            LinearRgbSpace::CameraNative,
            vec![value; 64 * 48 * 3],
        )
        .expect("valid image")
    }

    #[test]
    fn noise_is_seeded_and_matches_the_model() {
        let model = NOISE_LEVELS[1].1;
        let a = add_noise(&flat(0.5), model, 1).unwrap();
        assert_eq!(a, add_noise(&flat(0.5), model, 1).unwrap());
        assert_ne!(a, add_noise(&flat(0.5), model, 2).unwrap());

        let n = a.data().len() as f32;
        let mean = a.data().iter().sum::<f32>() / n;
        let var = a.data().iter().map(|v| (v - mean).powi(2)).sum::<f32>() / n;
        let expected = model.sigma(0.5).powi(2);
        assert!((var / expected - 1.0).abs() < 0.1, "{var} vs {expected}");
        assert!((mean - 0.5).abs() < 0.005);
    }
}
