//! Handles of the tone curve for an editor that draws and drags it.
//!
//! The curve works on linear luminance. Editors show it on display-encoded
//! axes, as a histogram has, so that the shadows get the room they have for
//! the eye. Positions here are display-encoded, 0 to 1 on both axes.

use rohditor_color::{linear_srgb_to_srgb, srgb_to_linear_srgb};
use rohditor_edit::{Parameter, ToneCurve};

use crate::evaluate_tone_curve;

/// One of the points a tone curve is dragged by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToneCurveHandle {
    index: usize,
}

impl ToneCurveHandle {
    const PARAMETERS: [Parameter; 4] = [
        Parameter::ToneCurveShadows,
        Parameter::ToneCurveDarks,
        Parameter::ToneCurveLights,
        Parameter::ToneCurveHighlights,
    ];

    /// The handles from dark to bright.
    pub const ALL: [Self; 4] = [
        Self { index: 0 },
        Self { index: 1 },
        Self { index: 2 },
        Self { index: 3 },
    ];

    /// The handle closest to this horizontal position.
    #[must_use]
    pub fn nearest(x: f32) -> Self {
        let distance = |handle: &Self| (handle.x() - x).abs();
        Self::ALL
            .into_iter()
            .min_by(|a, b| distance(a).total_cmp(&distance(b)))
            .unwrap_or(Self::ALL[0])
    }

    /// The recipe value the handle changes.
    #[must_use]
    pub const fn parameter(self) -> Parameter {
        Self::PARAMETERS[self.index]
    }

    /// Horizontal position, which never changes.
    #[must_use]
    pub fn x(self) -> f32 {
        linear_srgb_to_srgb(self.input())
    }

    /// Vertical position, on the curve.
    #[must_use]
    pub fn y(self, curve: &ToneCurve) -> f32 {
        display_tone_curve(curve, self.x())
    }

    /// The value of [`Self::parameter`] after dragging the handle up by `delta`
    /// from where it is on the curve. It stops at its neighbours: beyond them
    /// the curve would not follow it.
    #[must_use]
    pub fn dragged(self, curve: &ToneCurve, delta: f32) -> f32 {
        let output = srgb_to_linear_srgb((self.y(curve) + delta).clamp(0.0, 1.0));
        let neighbour = |index: Option<usize>, end| {
            index
                .and_then(|index| ToneCurve::INPUTS.get(index))
                .map_or(end, |&input| evaluate_tone_curve(curve, input))
        };
        let below = neighbour(self.index.checked_sub(1), 0.0);
        let above = neighbour(Some(self.index + 1), 1.0);
        let range = self.parameter().range();
        // The curve never decreases, so the bounds are in order.
        (output.clamp(below, above) - self.input()).clamp(range.minimum, range.maximum)
    }

    const fn input(self) -> f32 {
        ToneCurve::INPUTS[self.index]
    }
}

/// The tone curve on display-encoded axes.
#[must_use]
pub fn display_tone_curve(curve: &ToneCurve, x: f32) -> f32 {
    linear_srgb_to_srgb(evaluate_tone_curve(curve, srgb_to_linear_srgb(x)))
}

#[cfg(test)]
mod tests {
    use rohditor_edit::EditRecipe;

    use super::*;

    /// The curve after dragging `handle` by `delta`.
    fn drag(curve: &ToneCurve, handle: ToneCurveHandle, delta: f32) -> ToneCurve {
        let mut recipe = EditRecipe::default();
        recipe.light.tone_curve = curve.clone();
        let value = handle.dragged(curve, delta);
        let next = handle.parameter().with_value(&recipe, &recipe, value);
        next.light.tone_curve
    }

    #[test]
    fn a_neutral_curve_is_the_diagonal() {
        let curve = ToneCurve::default();
        for x in [0.0, 0.1, 0.38, 0.5, 0.9, 1.0] {
            assert!((display_tone_curve(&curve, x) - x).abs() < 1.0e-5);
        }
    }

    #[test]
    fn a_handle_follows_the_pointer_on_the_curve() {
        for handle in ToneCurveHandle::ALL {
            for delta in [-0.02, 0.015] {
                let curve = drag(&ToneCurve::default(), handle, delta);
                let moved = handle.y(&curve) - handle.x();
                assert!((moved - delta).abs() < 1.0e-4, "{handle:?}: {moved}");
                for other in ToneCurveHandle::ALL {
                    if other != handle {
                        assert!((other.y(&curve) - other.x()).abs() < 1.0e-5);
                    }
                }
            }
        }
    }

    #[test]
    fn a_handle_stops_at_its_neighbours_and_at_its_range() {
        let [_, darks, lights, _] = ToneCurveHandle::ALL;
        let curve = ToneCurve {
            lights: -0.2,
            ..ToneCurve::default()
        };
        let curve = drag(&curve, darks, 1.0);
        assert!((darks.y(&curve) - lights.y(&curve)).abs() < 1.0e-4);
        assert_eq!(
            darks.dragged(&ToneCurve::default(), 1.0),
            Parameter::ToneCurveDarks.range().maximum
        );
    }

    #[test]
    fn a_handle_moves_at_once_where_the_curve_hides_its_value() {
        // Darks is below shadows, so the curve shows it at their height.
        let curve = ToneCurve {
            shadows: 0.25,
            darks: -0.25,
            ..ToneCurve::default()
        };
        let [_, darks, ..] = ToneCurveHandle::ALL;
        let before = darks.y(&curve);
        let after = darks.y(&drag(&curve, darks, 0.03));
        assert!((after - before - 0.03).abs() < 1.0e-4);
    }

    #[test]
    fn the_nearest_handle_is_found() {
        let [shadows, darks, lights, highlights] = ToneCurveHandle::ALL;
        assert_eq!(ToneCurveHandle::nearest(0.0), shadows);
        assert_eq!(ToneCurveHandle::nearest(0.6), darks);
        assert_eq!(ToneCurveHandle::nearest(0.82), lights);
        assert_eq!(ToneCurveHandle::nearest(1.0), highlights);
    }
}
