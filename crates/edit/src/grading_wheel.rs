//! Colour grading as points on colour wheels.
//!
//! A recipe stores a tint as three RGB strengths. Their common part only changes
//! the brightness, which the grading stage takes out again, so what is left is
//! two-dimensional: a direction (the hue) and a distance (the strength).

use crate::{ColorGradingAdjustments, Parameter};

/// The tones a wheel tints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GradingTones {
    Shadows,
    Midtones,
    Highlights,
}

impl GradingTones {
    pub const ALL: [Self; 3] = [Self::Shadows, Self::Midtones, Self::Highlights];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Shadows => "Shadows",
            Self::Midtones => "Midtones",
            Self::Highlights => "Highlights",
        }
    }

    /// The first of the three parameters the wheel stands for.
    #[must_use]
    pub const fn parameter(self) -> Parameter {
        match self {
            Self::Shadows => Parameter::GradingShadows(0),
            Self::Midtones => Parameter::GradingMidtones(0),
            Self::Highlights => Parameter::GradingHighlights(0),
        }
    }

    #[must_use]
    pub fn get(self, grading: &ColorGradingAdjustments) -> GradingTint {
        GradingTint::from_rgb(match self {
            Self::Shadows => grading.shadows,
            Self::Midtones => grading.midtones,
            Self::Highlights => grading.highlights,
        })
    }

    /// `grading` with these tones at `tint`.
    #[must_use]
    pub fn with(
        self,
        mut grading: ColorGradingAdjustments,
        tint: GradingTint,
    ) -> ColorGradingAdjustments {
        *match self {
            Self::Shadows => &mut grading.shadows,
            Self::Midtones => &mut grading.midtones,
            Self::Highlights => &mut grading.highlights,
        } = tint.to_rgb();
        grading
    }
}

/// A point on a wheel of radius 1: red to the right, green and blue a third of a
/// turn further each, counterclockwise.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GradingTint {
    pub x: f32,
    pub y: f32,
}

impl GradingTint {
    /// The closest point on the wheel.
    #[must_use]
    pub fn new(x: f32, y: f32) -> Self {
        let outside = x.hypot(y).max(1.0);
        Self {
            x: x / outside,
            y: y / outside,
        }
    }

    #[must_use]
    pub fn from_rgb([red, green, blue]: [f32; 3]) -> Self {
        Self::new(
            (2.0 * red - green - blue) / 3.0,
            (green - blue) / 3.0_f32.sqrt(),
        )
    }

    /// Strengths of which at least one is not positive, within -1 to 1.
    #[must_use]
    pub fn to_rgb(self) -> [f32; 3] {
        let along = 3.0_f32.sqrt() / 2.0 * self.y;
        [self.x, along - self.x / 2.0, -along - self.x / 2.0].map(|value| {
            if value == 0.0 {
                0.0
            } else {
                value.clamp(-1.0, 1.0)
            }
        })
    }

    /// 0 to 1.
    #[must_use]
    pub fn strength(self) -> f32 {
        self.x.hypot(self.y)
    }

    /// 0 to 360, red at 0.
    #[must_use]
    pub fn hue_degrees(self) -> f32 {
        self.y.atan2(self.x).to_degrees().rem_euclid(360.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::COLOR_GRADING_RANGE;

    #[test]
    fn primaries_are_a_third_of_a_turn_apart() {
        for (rgb, hue) in [
            ([1.0, -0.5, -0.5], 0.0),
            ([-0.5, 1.0, -0.5], 120.0),
            ([-0.5, -0.5, 1.0], 240.0),
        ] {
            let tint = GradingTint::from_rgb(rgb);
            assert!((tint.strength() - 1.0).abs() < 1.0e-5);
            assert!((tint.hue_degrees() - hue).abs() < 1.0e-3);
        }
    }

    #[test]
    fn every_point_is_a_valid_recipe_value_and_survives_the_round_trip() {
        for degrees in (0..360).step_by(7) {
            for strength in [0.0, 0.3, 1.0] {
                let angle = (degrees as f32).to_radians();
                let tint = GradingTint::new(strength * angle.cos(), strength * angle.sin());
                let rgb = tint.to_rgb();
                assert!(rgb.iter().all(|value| COLOR_GRADING_RANGE.contains(*value)));
                let back = GradingTint::from_rgb(rgb);
                assert!((back.x - tint.x).abs() < 1.0e-5 && (back.y - tint.y).abs() < 1.0e-5);
            }
        }
        assert_eq!(GradingTint::default().to_rgb(), [0.0; 3]);
    }

    #[test]
    fn brightness_is_dropped_and_strong_tints_are_moved_to_the_rim() {
        assert_eq!(GradingTint::from_rgb([0.4; 3]).strength(), 0.0);
        let strong = GradingTint::from_rgb([1.0, 1.0, -1.0]);
        assert!((strong.strength() - 1.0).abs() < 1.0e-5);
        assert!((strong.hue_degrees() - 60.0).abs() < 1.0e-3);
    }

    #[test]
    fn a_wheel_changes_only_its_tones() {
        let grading = GradingTones::Midtones.with(
            ColorGradingAdjustments::default(),
            GradingTint::new(0.5, 0.0),
        );
        assert_eq!(grading.shadows, [0.0; 3]);
        assert_eq!(grading.midtones, [0.5, -0.25, -0.25]);
        assert_eq!(grading.highlights, [0.0; 3]);
        assert_eq!(
            GradingTones::Midtones.get(&grading),
            GradingTint { x: 0.5, y: 0.0 }
        );
    }
}
