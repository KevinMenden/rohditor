//! Colour range.

use rohditor_edit::{COLOR_RANGE_MAX_DISTANCE, ColorRange};

use crate::MaskError;
use crate::math::smoothstep;

/// How much a lightness difference counts compared with a chroma difference (`a`, `b`). Below
/// 1 so that a shadowed and a lit patch of the same hue stay together: the range selects hue.
const LIGHTNESS_WEIGHT: f32 = 0.25;
/// Smallest selection radius, so a tolerance of 0 never divides by zero.
const MIN_RADIUS: f32 = 1e-4;
/// Alpha is 1 inside this fraction of the radius and falls to 0 at the radius.
const CORE_FRACTION: f32 = 0.5;

/// Colour samples resolved to a selection radius in weighted OkLab distance.
///
/// A pixel's distance is the distance to its *nearest* sample, so several samples add their
/// selections together. Alpha is 1 within half the radius and falls to 0 at the radius with a
/// smoothstep. With no samples nothing is selected.
#[derive(Debug, Clone)]
pub(crate) struct PreparedColor {
    samples: Vec<[f32; 3]>,
    inner: f32,
    outer: f32,
}

impl PreparedColor {
    pub(crate) fn prepare(range: &ColorRange) -> Result<Self, MaskError> {
        let finite = range.tolerance.is_finite()
            && range.samples.iter().all(|sample| {
                [sample.lightness, sample.a, sample.b]
                    .iter()
                    .all(|v| v.is_finite())
            });
        if !finite {
            return Err(MaskError::Degenerate {
                shape: "color",
                reason: "tolerance and samples must be finite",
            });
        }
        let outer = (range.tolerance.clamp(0.0, 1.0) * COLOR_RANGE_MAX_DISTANCE).max(MIN_RADIUS);
        Ok(Self {
            samples: range
                .samples
                .iter()
                .map(|sample| [sample.lightness, sample.a, sample.b])
                .collect(),
            inner: outer * CORE_FRACTION,
            outer,
        })
    }

    pub(crate) fn evaluate_row(&self, oklab: &[[f32; 3]], out: &mut [f32]) {
        for (alpha, pixel) in out.iter_mut().zip(oklab) {
            // NaN distances compare as "not nearer", leaving the pixel unselected.
            let nearest = self
                .samples
                .iter()
                .fold(f32::INFINITY, |nearest, sample| {
                    nearest.min(weighted_distance_squared(pixel, sample))
                })
                .sqrt();
            *alpha = 1.0 - smoothstep(self.inner, self.outer, nearest);
        }
    }

    /// Bytes held by the samples.
    pub(crate) fn bytes(&self) -> usize {
        std::mem::size_of_val(self.samples.as_slice())
    }
}

#[inline]
fn weighted_distance_squared(pixel: &[f32; 3], sample: &[f32; 3]) -> f32 {
    let lightness = LIGHTNESS_WEIGHT * (pixel[0] - sample[0]);
    let a = pixel[1] - sample[1];
    let b = pixel[2] - sample[2];
    lightness * lightness + a * a + b * b
}

#[cfg(test)]
mod tests {
    use rohditor_color::linear_srgb_to_oklab;
    use rohditor_edit::ColorSample;

    use super::*;

    fn oklab(linear_srgb: [f32; 3]) -> [f32; 3] {
        linear_srgb_to_oklab(linear_srgb)
    }

    fn range(samples: &[[f32; 3]], tolerance: f32) -> PreparedColor {
        PreparedColor::prepare(&ColorRange {
            samples: samples
                .iter()
                .map(|[lightness, a, b]| ColorSample {
                    lightness: *lightness,
                    a: *a,
                    b: *b,
                })
                .collect(),
            tolerance,
        })
        .unwrap()
    }

    fn alphas(range: &PreparedColor, pixels: &[[f32; 3]]) -> Vec<f32> {
        let mut out = vec![0.0; pixels.len()];
        range.evaluate_row(pixels, &mut out);
        out
    }

    const RED: [f32; 3] = [1.0, 0.0, 0.0];
    const ORANGE: [f32; 3] = [1.0, 0.25, 0.0];
    const GREEN: [f32; 3] = [0.0, 1.0, 0.0];
    const BLUE: [f32; 3] = [0.0, 0.0, 1.0];

    #[test]
    fn a_low_tolerance_selects_red_without_orange() {
        let red = range(&[oklab(RED)], 0.25);
        let out = alphas(
            &red,
            &[oklab(RED), oklab(ORANGE), oklab(GREEN), oklab(BLUE)],
        );
        assert_eq!(out[0], 1.0);
        assert!(out[1] < 0.05, "orange {}", out[1]);
        assert_eq!(&out[2..], &[0.0, 0.0]);
        // A generous tolerance reaches the neighbouring hue as well.
        let wide = alphas(&range(&[oklab(RED)], 0.9), &[oklab(ORANGE), oklab(GREEN)]);
        assert!(wide[0] > 0.9 && wide[1] < 0.05, "{wide:?}");
    }

    #[test]
    fn the_nearest_sample_decides_and_no_samples_select_nothing() {
        let both = range(&[oklab(RED), oklab(BLUE)], 0.25);
        assert_eq!(
            alphas(&both, &[oklab(RED), oklab(BLUE), oklab(GREEN)]),
            [1.0, 1.0, 0.0]
        );
        let none = range(&[], 0.9);
        assert_eq!(alphas(&none, &[oklab(RED), [0.5, 0.0, 0.0]]), [0.0, 0.0]);
    }

    #[test]
    fn lightness_counts_less_than_hue_and_a_zero_tolerance_stays_finite() {
        let red = oklab(RED);
        let darker = [red[0] - 0.2, red[1], red[2]];
        let other_hue = [red[0], red[1] - 0.1, red[2] + 0.1];
        let out = alphas(&range(&[red], 0.3), &[darker, other_hue]);
        assert!(out[0] > 0.9, "same hue, darker: {}", out[0]);
        assert!(out[1] < 0.1, "other hue: {}", out[1]);

        let exact = range(&[red], 0.0);
        let out = alphas(&exact, &[red, darker, [f32::NAN, 0.0, 0.0]]);
        assert_eq!(out[0], 1.0);
        assert!(
            out[1..]
                .iter()
                .all(|alpha| alpha.is_finite() && *alpha < 1.0)
        );
        assert_eq!(out[2], 0.0);
    }

    #[test]
    fn rejects_non_finite_samples() {
        let bad = ColorRange {
            samples: vec![ColorSample {
                lightness: f32::NAN,
                a: 0.0,
                b: 0.0,
            }],
            tolerance: 0.3,
        };
        assert!(matches!(
            PreparedColor::prepare(&bad),
            Err(MaskError::Degenerate { shape: "color", .. })
        ));
    }
}
