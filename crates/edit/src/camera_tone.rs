use serde::{Deserialize, Serialize};

use crate::EditError;
use crate::monotone_spline;

/// Smallest and largest number of knots a fitted camera tone may carry.
pub const CAMERA_TONE_MIN_KNOTS: usize = 2;
pub const CAMERA_TONE_MAX_KNOTS: usize = 32;

/// Per-photo tone curve fitted to the camera's own rendering.
///
/// Knots are `[input, output]` pairs in the sRGB-encoded luminance domain, so
/// the fit and the stored numbers match what a viewer sees. The curve starts at
/// `(0, 0)`, ends at `(1, 1)`, and is monotone. It is evaluated with a
/// monotone cubic (Fritsch-Carlson) interpolation, so the stored knots fully
/// determine the rendering and a sidecar keeps looking the same.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CameraTone {
    pub knots: Vec<[f32; 2]>,
}

/// Median of the curves fitted to the private Sony ILCE-6400 corpus (six
/// scenes, spread within about 0.02 per knot), rounded to three decimals.
/// Only that body and picture style have been measured.
const STATIC_FALLBACK_KNOTS: [[f32; 2]; 19] = [
    [0.0, 0.0],
    [0.02, 0.010],
    [0.04, 0.022],
    [0.07, 0.041],
    [0.10, 0.070],
    [0.14, 0.121],
    [0.18, 0.180],
    [0.23, 0.261],
    [0.28, 0.356],
    [0.34, 0.475],
    [0.40, 0.574],
    [0.47, 0.683],
    [0.54, 0.768],
    [0.62, 0.838],
    [0.70, 0.895],
    [0.78, 0.937],
    [0.86, 0.959],
    [0.93, 0.973],
    [1.0, 1.0],
];

impl CameraTone {
    /// Typical camera tone, used when a photo has no fitted one.
    #[must_use]
    pub fn static_fallback() -> Self {
        Self {
            knots: STATIC_FALLBACK_KNOTS.to_vec(),
        }
    }

    /// Check the structural contract the evaluator relies on.
    pub fn validate(&self) -> Result<(), EditError> {
        let error = |reason: String| EditError {
            field: "rendering.camera_tone.knots",
            reason,
        };
        let count = self.knots.len();
        if !(CAMERA_TONE_MIN_KNOTS..=CAMERA_TONE_MAX_KNOTS).contains(&count) {
            return Err(error(format!(
                "{count} knots is outside {CAMERA_TONE_MIN_KNOTS}..={CAMERA_TONE_MAX_KNOTS}"
            )));
        }
        if self.knots[0] != [0.0, 0.0] || self.knots[count - 1] != [1.0, 1.0] {
            return Err(error("curve must run from (0, 0) to (1, 1)".to_owned()));
        }
        for pair in self.knots.windows(2) {
            let [x0, y0] = pair[0];
            let [x1, y1] = pair[1];
            if x1.is_nan() || x1 <= x0 {
                return Err(error("inputs must be strictly increasing".to_owned()));
            }
            if y1.is_nan() || y1 < y0 || y1 > 1.0 {
                return Err(error(
                    "outputs must be non-decreasing and within 0..=1".to_owned(),
                ));
            }
        }
        Ok(())
    }

    /// Evaluate the curve at an sRGB-encoded luminance. Inputs outside
    /// `0..=1` clamp to the end points.
    #[must_use]
    pub fn evaluate(&self, input: f32) -> f32 {
        if self.knots.len() < CAMERA_TONE_MIN_KNOTS || input.is_nan() {
            return input;
        }
        monotone_spline::evaluate(&self.knots, input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(knots: &[[f32; 2]]) -> CameraTone {
        CameraTone {
            knots: knots.to_vec(),
        }
    }

    #[test]
    fn static_fallback_is_a_valid_brightening_curve() {
        let fallback = CameraTone::static_fallback();
        fallback
            .validate()
            .expect("fallback satisfies the contract");
        // Camera renderings lift the midtones relative to a plain sRGB encode.
        assert!(fallback.evaluate(0.4) > 0.5);
        assert!(fallback.evaluate(0.9) > 0.9);
    }

    #[test]
    fn identity_knots_evaluate_to_identity() {
        let identity = tone(&[[0.0, 0.0], [0.5, 0.5], [1.0, 1.0]]);
        identity.validate().expect("identity is valid");
        for step in 0..=100 {
            let input = step as f32 / 100.0;
            assert!((identity.evaluate(input) - input).abs() < 1.0e-6);
        }
    }

    #[test]
    fn interpolates_knots_exactly_and_stays_monotone_for_an_asymmetric_curve() {
        let curve = tone(&[[0.0, 0.0], [0.1, 0.02], [0.4, 0.6], [0.7, 0.72], [1.0, 1.0]]);
        curve.validate().expect("asymmetric curve is valid");
        for [x, y] in &curve.knots {
            assert!((curve.evaluate(*x) - y).abs() < 1.0e-6);
        }
        let mut previous = 0.0;
        for step in 0..=1_000 {
            let value = curve.evaluate(step as f32 / 1_000.0);
            assert!(value >= previous, "curve decreased at step {step}");
            previous = value;
        }
    }

    #[test]
    fn out_of_range_inputs_clamp_to_the_end_points() {
        let curve = tone(&[[0.0, 0.0], [0.5, 0.8], [1.0, 1.0]]);
        assert_eq!(curve.evaluate(-3.0), 0.0);
        assert_eq!(curve.evaluate(7.0), 1.0);
    }

    #[test]
    fn validation_rejects_malformed_curves() {
        let invalid = [
            tone(&[[0.0, 0.0]]),
            tone(&[[0.0, 0.1], [1.0, 1.0]]),
            tone(&[[0.0, 0.0], [0.5, 0.6], [0.5, 0.7], [1.0, 1.0]]),
            tone(&[[0.0, 0.0], [0.4, 0.7], [0.6, 0.5], [1.0, 1.0]]),
            tone(&[[0.0, 0.0], [f32::NAN, 0.5], [1.0, 1.0]]),
        ];
        for curve in invalid {
            assert!(curve.validate().is_err(), "{curve:?} must be rejected");
        }
    }
}
