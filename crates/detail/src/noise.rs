//! Poisson–Gaussian noise estimate: `sigma^2(x) = a * x + b`.
//!
//! The finest à-trous detail band of a plane is dominated by noise wherever
//! the image is flat. We take the flattest tiles, bin them by brightness and
//! fit a line through the per-bin noise variances.

use crate::{CancellationCheck, DetailError, Plane, atrous_decompose};

const TILE: usize = 16;
/// Fraction (1/4) of tiles per bin, lowest detail energy, assumed free of
/// image structure.
const FLAT_FRACTION_NUM: usize = 1;
const FLAT_FRACTION_DEN: usize = 4;
const MAX_BINS: usize = 16;
/// Pooled detail samples need enough tiles to give a stable MAD.
const MIN_TILES_PER_BIN: usize = 8;
/// Fewer usable bins than this fall back to a flat sigma.
const MIN_BINS: usize = 8;
/// Std-dev of (x - B3 * x) for unit white noise: sqrt(1 - 2*h0 + sum(h^2)).
const DETAIL_NOISE_GAIN: f32 = 0.890_8;
/// MAD to standard deviation for Gaussian data.
const MAD_TO_SIGMA: f32 = 1.482_6;

/// Fitted noise model for one plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoiseProfile {
    /// Signal-dependent (shot) variance slope.
    pub a: f32,
    /// Signal-independent (read) variance.
    pub b: f32,
    /// Number of brightness bins that went into the fit; 0 means the flat
    /// fallback (`a = 0`) was used.
    pub bins: usize,
}

impl NoiseProfile {
    /// Noise variance expected at signal level `x`.
    #[must_use]
    pub fn variance(&self, x: f32) -> f32 {
        self.a * x.max(0.0) + self.b
    }

    /// Standard deviation expected at signal level `x`.
    #[must_use]
    pub fn sigma(&self, x: f32) -> f32 {
        self.variance(x).sqrt()
    }
}

/// Estimate the noise of `plane`, binning by the matching `brightness` plane
/// (normally the balanced luma; for luma itself pass the same plane).
pub fn estimate_noise(
    plane: &Plane,
    brightness: &Plane,
    cancellation: &dyn CancellationCheck,
) -> Result<NoiseProfile, DetailError> {
    plane.same_size(brightness)?;
    let band = atrous_decompose(plane, 1, cancellation)?.details.remove(0);
    let (w, h) = (plane.width(), plane.height());

    struct Tile {
        energy: f32,
        brightness: f32,
        x: usize,
        y: usize,
    }
    let mut tiles = Vec::new();
    for ty in (0..h / TILE).map(|t| t * TILE) {
        for tx in (0..w / TILE).map(|t| t * TILE) {
            let (mut energy, mut mean) = (0.0_f64, 0.0_f64);
            for y in ty..ty + TILE {
                for x in tx..tx + TILE {
                    energy += f64::from(band.get(x, y)).powi(2);
                    mean += f64::from(brightness.get(x, y));
                }
            }
            let n = (TILE * TILE) as f64;
            tiles.push(Tile {
                energy: (energy / n) as f32,
                brightness: (mean / n) as f32,
                x: tx,
                y: ty,
            });
        }
    }
    if tiles.is_empty() {
        return Err(DetailError::InvalidDimensions {
            width: w,
            height: h,
        });
    }
    // Equal-count brightness bins first, flattest tiles second: choosing the
    // flattest tiles globally would favour dark (low-noise) regions and bias
    // the slope.
    tiles.sort_by(|a, b| a.brightness.total_cmp(&b.brightness));
    let bin_count =
        (tiles.len() * FLAT_FRACTION_NUM / FLAT_FRACTION_DEN / MIN_TILES_PER_BIN).min(MAX_BINS);
    let bin_estimate = |group: &mut [Tile]| -> (f32, f32) {
        group.sort_by(|a, b| a.energy.total_cmp(&b.energy));
        let flat = &group[..(group.len() * FLAT_FRACTION_NUM / FLAT_FRACTION_DEN).max(1)];
        let mut samples: Vec<f32> = flat
            .iter()
            .flat_map(|t| {
                let band = &band;
                (t.y..t.y + TILE).flat_map(move |y| (t.x..t.x + TILE).map(move |x| band.get(x, y)))
            })
            .collect();
        let sigma = MAD_TO_SIGMA * mad(&mut samples) / DETAIL_NOISE_GAIN;
        let x = flat.iter().map(|t| t.brightness).sum::<f32>() / flat.len() as f32;
        (x, sigma * sigma)
    };
    if bin_count < MIN_BINS {
        // Too little data for a slope: one flat sigma.
        let (_, variance) = bin_estimate(&mut tiles);
        return Ok(NoiseProfile {
            a: 0.0,
            b: variance,
            bins: 0,
        });
    }
    let len = tiles.len();
    let points: Vec<(f32, f32)> = (0..bin_count)
        .map(|i| bin_estimate(&mut tiles[i * len / bin_count..(i + 1) * len / bin_count]))
        .collect();
    Ok(fit_line(&points))
}

/// Median absolute deviation about the median (reorders `values`).
fn mad(values: &mut [f32]) -> f32 {
    let median = *values
        .select_nth_unstable_by(values.len() / 2, f32::total_cmp)
        .1;
    for v in values.iter_mut() {
        *v = (*v - median).abs();
    }
    *values
        .select_nth_unstable_by(values.len() / 2, f32::total_cmp)
        .1
}

/// Least-squares `variance = a * x + b`, both clamped to be non-negative.
fn fit_line(points: &[(f32, f32)]) -> NoiseProfile {
    let n = points.len() as f64;
    let (mut sx, mut sy, mut sxx, mut sxy) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
    for &(x, y) in points {
        let (x, y) = (f64::from(x), f64::from(y));
        sx += x;
        sy += y;
        sxx += x * x;
        sxy += x * y;
    }
    let denominator = n * sxx - sx * sx;
    let slope = if denominator.abs() < 1e-18 {
        0.0
    } else {
        (n * sxy - sx * sy) / denominator
    };
    let a = slope.max(0.0);
    // Refit the intercept for the clamped slope.
    let b = ((sy - a * sx) / n).max(0.0);
    NoiseProfile {
        a: a as f32,
        b: b as f32,
        bins: points.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NeverCancelled;
    use crate::test_support::Gauss;

    fn noisy_ramp(a: f32, b: f32) -> Plane {
        let (w, h) = (512, 512);
        let mut rng = Gauss(7);
        let data = (0..w * h)
            .map(|i| {
                // Horizontal ramp 0.05..0.95; the top third also carries fine texture
                // that the flat-tile selection must ignore.
                let x = 0.05 + 0.9 * (i % w) as f32 / (w - 1) as f32;
                let texture = if i / w < h / 3 {
                    0.05 * ((i % 5) as f32 - 2.0)
                } else {
                    0.0
                };
                let signal = x + texture;
                signal + (a * x + b).sqrt() * rng.next()
            })
            .collect();
        Plane::new(w, h, data).unwrap()
    }

    #[test]
    fn recovers_known_poisson_gaussian_parameters() {
        for (a, b) in [(0.002_f32, 0.0001_f32), (0.01, 0.0004), (0.03, 0.001)] {
            let plane = noisy_ramp(a, b);
            // The ramp's clean brightness is the binning key.
            let brightness = Plane::new(
                512,
                512,
                (0..512 * 512)
                    .map(|i| 0.05 + 0.9 * (i % 512) as f32 / 511.0)
                    .collect(),
            )
            .unwrap();
            let p = estimate_noise(&plane, &brightness, &NeverCancelled).unwrap();
            assert!(p.bins >= MIN_BINS, "{p:?}");
            // Compare the fitted model against the truth across the range
            // (a and b individually are poorly conditioned).
            for x in [0.1_f32, 0.5, 0.9] {
                let truth = (a * x + b).sqrt();
                let err = (p.sigma(x) - truth).abs() / truth;
                assert!(err < 0.15, "a={a} b={b} x={x}: {p:?} err {err}");
            }
        }
    }

    #[test]
    fn small_planes_fall_back_to_flat_sigma() {
        let mut rng = Gauss(3);
        let data = (0..128 * 128).map(|_| 0.5 + 0.02 * rng.next()).collect();
        let plane = Plane::new(128, 128, data).unwrap();
        let p = estimate_noise(&plane, &plane, &NeverCancelled).unwrap();
        assert_eq!((p.a, p.bins), (0.0, 0));
        assert!((p.b.sqrt() - 0.02).abs() / 0.02 < 0.15, "{p:?}");
    }

    #[test]
    fn rejects_planes_smaller_than_a_tile() {
        let p = Plane::filled(8, 8, 0.5).unwrap();
        assert!(estimate_noise(&p, &p, &NeverCancelled).is_err());
    }
}
