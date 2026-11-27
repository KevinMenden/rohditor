//! The free point tone curve on luminance, and the table it is rendered with.
//!
//! Points are `[input, output]` pairs on sRGB-encoded axes, as an editor and a
//! histogram show them, so evenly spaced points are evenly spaced to the eye.
//! The pipeline works on linear luminance: [`ToneCurve::evaluate_linear`]
//! encodes, evaluates the monotone spline and decodes again.
//!
//! The end points are fixed at `(0, 0)` and `(1, 1)`. Inputs outside `0..=1`
//! pass through unchanged, which keeps HDR (extended rendition) highlights and
//! negative working samples, and is continuous only because the ends are fixed.

use rohditor_color::{linear_srgb_to_srgb, srgb_to_linear_srgb};
use serde::{Deserialize, Serialize};

use crate::recipe::sample_lut;
use crate::{EditError, monotone_spline};

/// Most points a curve may have, the two end points included.
pub const TONE_CURVE_MAX_POINTS: usize = 16;
/// Smallest horizontal distance between neighbouring points.
pub const TONE_CURVE_MIN_SPACING: f32 = 0.01;
/// Rounding allowance on the spacing, so a point moved right up to its
/// neighbour stays valid.
const SPACING_TOLERANCE: f32 = 1.0e-5;
/// Number of samples in [`ToneCurveLut`].
pub const TONE_CURVE_LUT_SIZE: usize = 4_096;

const START: [f32; 2] = [0.0, 0.0];
const END: [f32; 2] = [1.0, 1.0];

/// Luminance curve through free control points, evaluated after the Light
/// controls. Recipes of an older schema, which had four fixed offsets here,
/// deserialize to the identity curve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToneCurve {
    /// Sorted by input, starting at `(0, 0)` and ending at `(1, 1)`.
    #[serde(default = "identity_points")]
    pub points: Vec<[f32; 2]>,
}

fn identity_points() -> Vec<[f32; 2]> {
    vec![START, END]
}

impl Default for ToneCurve {
    fn default() -> Self {
        Self {
            points: identity_points(),
        }
    }
}

impl ToneCurve {
    /// A curve through the end points and `inner` points, which are not
    /// checked; see [`Self::validate`].
    #[must_use]
    pub fn through(inner: &[[f32; 2]]) -> Self {
        let mut points = Vec::with_capacity(inner.len() + 2);
        points.push(START);
        points.extend_from_slice(inner);
        points.push(END);
        Self { points }
    }

    /// Check the contract the evaluation and the editing rely on.
    pub fn validate(&self) -> Result<(), EditError> {
        let error = |reason: String| EditError {
            field: "light.tone_curve.points",
            reason,
        };
        let count = self.points.len();
        if !(2..=TONE_CURVE_MAX_POINTS).contains(&count) {
            return Err(error(format!(
                "{count} points is outside 2..={TONE_CURVE_MAX_POINTS}"
            )));
        }
        if self.points[0] != START || self.points[count - 1] != END {
            return Err(error("curve must run from (0, 0) to (1, 1)".to_owned()));
        }
        for pair in self.points.windows(2) {
            let ([x0, y0], [x1, y1]) = (pair[0], pair[1]);
            if !(x1.is_finite() && y1.is_finite()) {
                return Err(error("points must be finite".to_owned()));
            }
            if x1 - x0 < TONE_CURVE_MIN_SPACING - SPACING_TOLERANCE {
                return Err(error(format!(
                    "inputs must increase by at least {TONE_CURVE_MIN_SPACING}"
                )));
            }
            if y1 < y0 || !(0.0..=1.0).contains(&y1) {
                return Err(error(
                    "outputs must be non-decreasing and within 0..=1".to_owned(),
                ));
            }
        }
        Ok(())
    }

    /// Whether the curve changes nothing. A monotone cubic through points on
    /// the diagonal is the diagonal.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.points.iter().all(|[x, y]| x == y)
    }

    /// The curve on its own sRGB-encoded axes. Inputs outside `0..=1` and NaN
    /// pass through.
    #[must_use]
    pub fn evaluate(&self, input: f32) -> f32 {
        if !(0.0..=1.0).contains(&input) {
            return input;
        }
        monotone_spline::evaluate(&self.points, input)
    }

    /// The curve on linear luminance, as the pipeline applies it. Inputs
    /// outside `0..=1` and NaN pass through.
    #[must_use]
    pub fn evaluate_linear(&self, input: f32) -> f32 {
        if !(0.0..=1.0).contains(&input) {
            return input;
        }
        srgb_to_linear_srgb(self.evaluate(linear_srgb_to_srgb(input)))
    }

    /// Where a point at `input` would go, or `None` if the curve is full or
    /// `input` is too close to another point. The point is put on the curve,
    /// so adding it changes nothing until it is moved.
    #[must_use]
    pub fn with_point_added(&self, input: f32) -> Option<(Self, usize)> {
        if self.points.len() >= TONE_CURVE_MAX_POINTS || !input.is_finite() {
            return None;
        }
        let index = self.points.partition_point(|point| point[0] < input);
        let (before, after) = (
            self.points.get(index.checked_sub(1)?)?,
            self.points.get(index)?,
        );
        if input - before[0] < TONE_CURVE_MIN_SPACING || after[0] - input < TONE_CURVE_MIN_SPACING {
            return None;
        }
        let mut next = self.clone();
        next.points.insert(index, [input, self.evaluate(input)]);
        Some((next, index))
    }

    /// Whether the point can be moved or removed: every point but the ends.
    #[must_use]
    pub fn is_movable(&self, index: usize) -> bool {
        index > 0 && index + 1 < self.points.len()
    }

    /// The curve with the point at `index` moved towards `to`. It stops short
    /// of its neighbours, so the points stay in order and the curve monotone.
    /// The end points do not move.
    #[must_use]
    pub fn with_point_moved(&self, index: usize, to: [f32; 2]) -> Self {
        let mut next = self.clone();
        if !self.is_movable(index) || !to.iter().all(|value| value.is_finite()) {
            return next;
        }
        let ([x0, y0], [x1, y1]) = (self.points[index - 1], self.points[index + 1]);
        next.points[index] = [
            to[0].clamp(x0 + TONE_CURVE_MIN_SPACING, x1 - TONE_CURVE_MIN_SPACING),
            to[1].clamp(y0, y1),
        ];
        next
    }

    /// The curve without the point at `index`. The end points stay.
    #[must_use]
    pub fn with_point_removed(&self, index: usize) -> Self {
        let mut next = self.clone();
        if self.is_movable(index) {
            next.points.remove(index);
        }
        next
    }
}

/// [`ToneCurve::evaluate_linear`] sampled over linear luminance `0..=1`, built
/// once per render so a pixel costs one lookup. The CPU interpolates it and
/// the GPU uploads the same values.
#[derive(Debug, Clone, PartialEq)]
pub struct ToneCurveLut {
    values: [f32; TONE_CURVE_LUT_SIZE],
}

impl ToneCurveLut {
    #[must_use]
    pub fn new(curve: &ToneCurve) -> Self {
        let denominator = (TONE_CURVE_LUT_SIZE - 1) as f32;
        let values = std::array::from_fn(|index| curve.evaluate_linear(index as f32 / denominator));
        Self { values }
    }

    /// Evaluate with linear interpolation. Inputs outside `0..=1` pass through.
    #[must_use]
    pub fn sample(&self, input: f32) -> f32 {
        sample_lut(&self.values, input)
    }

    /// Packed samples for a GPU storage-buffer upload.
    #[must_use]
    pub const fn values(&self) -> &[f32; TONE_CURVE_LUT_SIZE] {
        &self.values
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s_curve() -> ToneCurve {
        ToneCurve::through(&[[0.25, 0.18], [0.5, 0.5], [0.75, 0.84]])
    }

    fn assert_monotone(curve: &ToneCurve) {
        let mut previous = 0.0;
        for step in 0..=2_000 {
            let value = curve.evaluate(step as f32 / 2_000.0);
            assert!(
                value >= previous && (0.0..=1.0).contains(&value),
                "{curve:?} at step {step}"
            );
            previous = value;
        }
    }

    #[test]
    fn identity_changes_nothing() {
        let identity = ToneCurve::default();
        identity.validate().expect("the default is valid");
        assert!(identity.is_identity());
        let lut = ToneCurveLut::new(&identity);
        for step in 0..=1_000 {
            let input = step as f32 / 1_000.0;
            assert!((identity.evaluate_linear(input) - input).abs() < 1.0e-6);
            assert!((lut.sample(input) - input).abs() < 1.0e-6);
        }
        let on_diagonal = ToneCurve::through(&[[0.3, 0.3], [0.6, 0.6]]);
        assert!(on_diagonal.is_identity());
        assert!((on_diagonal.evaluate(0.45) - 0.45).abs() < 1.0e-6);
    }

    #[test]
    fn end_points_and_control_points_are_kept() {
        let curve = s_curve();
        curve.validate().expect("valid");
        assert!(!curve.is_identity());
        for [x, y] in &curve.points {
            assert!((curve.evaluate(*x) - y).abs() < 1.0e-6);
        }
        assert_eq!(curve.evaluate_linear(0.0), 0.0);
        assert!((curve.evaluate_linear(1.0) - 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn inputs_outside_the_unit_range_pass_through() {
        let curve = s_curve();
        let lut = ToneCurveLut::new(&curve);
        for input in [-0.5, -1.0e-4, 1.0001, 4.0, f32::INFINITY] {
            assert_eq!(curve.evaluate_linear(input), input);
            assert_eq!(lut.sample(input), input);
        }
        assert!(curve.evaluate_linear(f32::NAN).is_nan());
        assert!(lut.sample(f32::NAN).is_nan());
    }

    #[test]
    fn extreme_points_stay_monotone_without_overshoot() {
        let spacing = TONE_CURVE_MIN_SPACING;
        let curves = [
            // A step: flat, vertical at the minimum spacing, flat.
            ToneCurve::through(&[[0.5, 0.0], [0.5 + spacing, 1.0]]),
            // Crushed shadows and a hard shoulder.
            ToneCurve::through(&[[0.3, 0.0], [0.31, 0.02], [0.9, 1.0]]),
            // Uneven spacing with a near-flat middle.
            ToneCurve::through(&[[0.02, 0.3], [0.5, 0.31], [0.98, 0.99]]),
            ToneCurve::through(&[[0.1, 0.9], [0.9, 0.9]]),
        ];
        for curve in &curves {
            curve.validate().expect("valid");
            assert_monotone(curve);
        }
    }

    #[test]
    fn an_asymmetric_curve_bends_between_its_points() {
        let curve = ToneCurve::through(&[[0.2, 0.3], [0.7, 0.75]]);
        // Lifted shadows stay above the diagonal, and less so towards white.
        let low = curve.evaluate(0.1);
        let high = curve.evaluate(0.9);
        assert!(low > 0.1 && low < 0.3, "{low}");
        assert!(high > 0.9 && high < 1.0, "{high}");
        assert!(low - 0.1 > high - 0.9);
        // In linear light the same curve brightens too.
        assert!(curve.evaluate_linear(0.05) > 0.05);
    }

    #[test]
    fn the_lut_matches_the_curve() {
        let curve = ToneCurve::through(&[[0.1, 0.03], [0.4, 0.62], [0.8, 0.83]]);
        let lut = ToneCurveLut::new(&curve);
        for step in 0..=997 {
            let input = step as f32 / 997.0;
            let exact = curve.evaluate_linear(input);
            assert!((lut.sample(input) - exact).abs() < 2.0e-4, "at {input}");
        }
    }

    #[test]
    fn validation_rejects_malformed_curves() {
        let crowded: Vec<_> = (1..TONE_CURVE_MAX_POINTS - 1)
            .map(|index| {
                let x = index as f32 / TONE_CURVE_MAX_POINTS as f32;
                [x, x]
            })
            .collect();
        assert!(ToneCurve::through(&crowded).validate().is_ok());
        let mut full = crowded;
        full.push([0.99, 0.99]);
        let invalid = [
            ToneCurve { points: vec![END] },
            ToneCurve {
                points: vec![[0.0, 0.1], END],
            },
            ToneCurve {
                points: vec![START, [1.0, 0.9]],
            },
            ToneCurve::through(&[[0.6, 0.5], [0.4, 0.6]]),
            ToneCurve::through(&[[0.4, 0.5], [0.405, 0.6]]),
            ToneCurve::through(&[[0.4, 0.6], [0.6, 0.5]]),
            ToneCurve::through(&[[0.4, f32::NAN]]),
            ToneCurve::through(&[[f32::NAN, 0.4]]),
            ToneCurve::through(&full),
        ];
        for curve in invalid {
            assert!(curve.validate().is_err(), "{curve:?}");
        }
    }

    #[test]
    fn points_are_added_on_the_curve_and_keep_their_order_when_moved() {
        let curve = s_curve();
        let (added, index) = curve.with_point_added(0.6).expect("room for it");
        assert_eq!(index, 3);
        assert!((added.evaluate(0.6) - curve.evaluate(0.6)).abs() < 1.0e-6);
        assert!(curve.with_point_added(0.505).is_none(), "too close");
        assert!(curve.with_point_added(1.5).is_none());

        let moved = added.with_point_moved(index, [0.1, 1.0]);
        moved.validate().expect("still valid");
        assert_eq!(moved.points[index], [0.5 + TONE_CURVE_MIN_SPACING, 0.84]);
        assert_eq!(added.with_point_moved(0, [0.2, 0.4]), added, "ends stay");

        let removed = added.with_point_removed(index);
        assert_eq!(removed, curve);
        assert_eq!(curve.with_point_removed(4), curve, "ends stay");
    }

    #[test]
    fn old_four_offset_curves_deserialize_to_the_identity() {
        let old = r#"{"shadows":0.1,"darks":0.0,"lights":-0.05,"highlights":0.0}"#;
        let curve: ToneCurve = serde_json::from_str(old).expect("ignored");
        assert_eq!(curve, ToneCurve::default());
    }
}
