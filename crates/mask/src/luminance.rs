//! Luminance range.

use rohditor_edit::LuminanceRange;

use crate::MaskError;
use crate::math::smoothstep;

/// A luminance range, evaluated on the reference's sRGB-encoded luminance.
///
/// Alpha is 1 for `low..=high` (both ends included, also with a feather of 0) and falls to 0
/// with a smoothstep over `feather_low` below `low` and `feather_high` above `high`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PreparedLuminance {
    low: f32,
    high: f32,
    feather_low: f32,
    feather_high: f32,
}

impl PreparedLuminance {
    pub(crate) fn prepare(range: &LuminanceRange) -> Result<Self, MaskError> {
        let values = [range.low, range.high, range.feather_low, range.feather_high];
        if values.iter().any(|value| !value.is_finite()) {
            return Err(MaskError::Degenerate {
                shape: "luminance",
                reason: "range values must be finite",
            });
        }
        if range.low > range.high {
            return Err(MaskError::Degenerate {
                shape: "luminance",
                reason: "low must not be above high",
            });
        }
        Ok(Self {
            low: range.low,
            high: range.high,
            feather_low: range.feather_low.max(0.0),
            feather_high: range.feather_high.max(0.0),
        })
    }

    pub(crate) fn evaluate_row(&self, luminance: &[f32], out: &mut [f32]) {
        for (alpha, value) in out.iter_mut().zip(luminance) {
            *alpha = self.alpha(*value);
        }
    }

    #[inline]
    fn alpha(&self, value: f32) -> f32 {
        if value.is_nan() {
            0.0
        } else if value < self.low {
            smoothstep(self.low - self.feather_low, self.low, value)
        } else if value > self.high {
            1.0 - smoothstep(self.high, self.high + self.feather_high, value)
        } else {
            1.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(low: f32, high: f32, feather_low: f32, feather_high: f32) -> PreparedLuminance {
        PreparedLuminance::prepare(&LuminanceRange {
            low,
            high,
            feather_low,
            feather_high,
        })
        .unwrap()
    }

    fn alphas(range: &PreparedLuminance, values: &[f32]) -> Vec<f32> {
        let mut out = vec![0.0; values.len()];
        range.evaluate_row(values, &mut out);
        out
    }

    #[test]
    fn hard_edges_include_both_ends() {
        let hard = range(0.25, 0.75, 0.0, 0.0);
        assert_eq!(
            alphas(&hard, &[0.0, 0.24, 0.25, 0.5, 0.75, 0.76, 1.0]),
            [0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0]
        );
        // A range that reaches the ends of the scale keeps pure black and white selected.
        let full = range(0.0, 1.0, 0.0, 0.0);
        assert_eq!(alphas(&full, &[0.0, 0.5, 1.0]), [1.0, 1.0, 1.0]);
    }

    #[test]
    fn feathers_fall_outward_and_are_independent() {
        // Wide feather below, narrow above: 0.2 below low is half way down the 0.4 feather.
        let soft = range(0.4, 0.6, 0.4, 0.1);
        let out = alphas(&soft, &[0.0, 0.2, 0.4, 0.6, 0.65, 0.7, 0.9]);
        assert_eq!(out[0], 0.0);
        assert!((out[1] - 0.5).abs() < 1e-6, "{}", out[1]);
        assert_eq!(&out[2..4], &[1.0, 1.0]);
        assert!((out[4] - 0.5).abs() < 1e-6, "{}", out[4]);
        assert_eq!(&out[5..], &[0.0, 0.0]);
        // Falls monotonically on each side.
        let ramp: Vec<f32> = (0..=40).map(|i| i as f32 / 100.0).collect();
        let rising = alphas(&soft, &ramp);
        assert!(rising.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn rejects_inverted_or_non_finite_ranges_and_treats_nan_pixels_as_outside() {
        let bad = |low, high| {
            PreparedLuminance::prepare(&LuminanceRange {
                low,
                high,
                feather_low: 0.0,
                feather_high: 0.0,
            })
        };
        assert!(matches!(
            bad(0.8, 0.2),
            Err(MaskError::Degenerate {
                shape: "luminance",
                ..
            })
        ));
        assert!(bad(f32::NAN, 0.5).is_err());
        assert_eq!(alphas(&range(0.0, 1.0, 0.1, 0.1), &[f32::NAN]), [0.0]);
    }
}
