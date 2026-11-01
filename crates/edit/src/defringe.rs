use serde::{Deserialize, Serialize};

use crate::{EditError, ParameterRange};

pub const DEFRINGE_AMOUNT_RANGE: ParameterRange = ParameterRange {
    minimum: 0.0,
    maximum: 1.0,
    neutral: 0.0,
};

/// Hue ranges are degrees on the HSL hue circle, without wrap-around.
pub const DEFRINGE_HUE_RANGE_DEGREES: ParameterRange = ParameterRange {
    minimum: 0.0,
    maximum: 360.0,
    neutral: 0.0,
};

/// Narrowest and widest hue range a fringe colour may cover, in degrees.
pub const DEFRINGE_MIN_HUE_SPAN: f32 = 10.0;
pub const DEFRINGE_MAX_HUE_SPAN: f32 = 90.0;

/// Default hue range of purple fringes (in front of the focal plane).
pub const DEFAULT_PURPLE_HUE: [f32; 2] = [270.0, 330.0];
/// Default hue range of green fringes (behind the focal plane).
pub const DEFAULT_GREEN_HUE: [f32; 2] = [75.0, 150.0];

/// Axial chromatic aberration clean-up: purple and green fringes along
/// high-contrast edges. Runs after optics, on source-resolution camera RGB.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Defringe {
    /// Strength of the purple correction in [0, 1]; 0 is off.
    pub purple_amount: f32,
    /// Hue range (degrees, `[start, end]`) that counts as purple.
    pub purple_hue: [f32; 2],
    /// Strength of the green correction in [0, 1]; 0 is off.
    pub green_amount: f32,
    /// Hue range (degrees, `[start, end]`) that counts as green.
    pub green_hue: [f32; 2],
}

impl Default for Defringe {
    fn default() -> Self {
        Self {
            purple_amount: DEFRINGE_AMOUNT_RANGE.neutral,
            purple_hue: DEFAULT_PURPLE_HUE,
            green_amount: DEFRINGE_AMOUNT_RANGE.neutral,
            green_hue: DEFAULT_GREEN_HUE,
        }
    }
}

impl Defringe {
    /// Whether either colour has work to do.
    #[must_use]
    pub fn is_active(self) -> bool {
        self.purple_active() || self.green_active()
    }

    #[must_use]
    pub fn purple_active(self) -> bool {
        self.purple_amount > 0.0
    }

    #[must_use]
    pub fn green_active(self) -> bool {
        self.green_amount > 0.0
    }

    pub fn validate(self) -> Result<(), EditError> {
        for (field, amount) in [
            ("defringe.purple_amount", self.purple_amount),
            ("defringe.green_amount", self.green_amount),
        ] {
            if !DEFRINGE_AMOUNT_RANGE.contains(amount) {
                return Err(EditError {
                    field,
                    reason: "expected a finite value in 0..=1".to_owned(),
                });
            }
        }
        validate_hue_range("defringe.purple_hue", self.purple_hue)?;
        validate_hue_range("defringe.green_hue", self.green_hue)
    }
}

/// `range` with one end (`edge` 0 = start, 1 = end) moved to `value`, and the
/// other end pulled along as far as needed to keep the range valid
/// (ordered, within the circle, 10 to 90 degrees wide). For range sliders
/// that edit the two ends separately.
#[must_use]
pub fn with_hue_edge(range: [f32; 2], edge: usize, value: f32) -> [f32; 2] {
    let [start, end] = range;
    let circle = DEFRINGE_HUE_RANGE_DEGREES;
    if edge == 0 {
        let start = value.clamp(circle.minimum, circle.maximum - DEFRINGE_MIN_HUE_SPAN);
        let end = end.clamp(
            start + DEFRINGE_MIN_HUE_SPAN,
            (start + DEFRINGE_MAX_HUE_SPAN).min(circle.maximum),
        );
        [start, end]
    } else {
        let end = value.clamp(circle.minimum + DEFRINGE_MIN_HUE_SPAN, circle.maximum);
        let start = start.clamp(
            (end - DEFRINGE_MAX_HUE_SPAN).max(circle.minimum),
            end - DEFRINGE_MIN_HUE_SPAN,
        );
        [start, end]
    }
}

fn validate_hue_range(field: &'static str, [start, end]: [f32; 2]) -> Result<(), EditError> {
    let span = end - start;
    if DEFRINGE_HUE_RANGE_DEGREES.contains(start)
        && DEFRINGE_HUE_RANGE_DEGREES.contains(end)
        && (DEFRINGE_MIN_HUE_SPAN..=DEFRINGE_MAX_HUE_SPAN).contains(&span)
    {
        Ok(())
    } else {
        Err(EditError {
            field,
            reason: format!(
                "expected an ordered range within 0..=360 degrees spanning \
                 {DEFRINGE_MIN_HUE_SPAN}..={DEFRINGE_MAX_HUE_SPAN} degrees"
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EditRecipe;

    #[test]
    fn round_trips_migrates_and_rejects_invalid_values() {
        let recipe = EditRecipe {
            defringe: Defringe {
                purple_amount: 0.7,
                purple_hue: [260.0, 320.0],
                green_amount: 0.3,
                green_hue: [80.0, 140.0],
            },
            ..EditRecipe::default()
        };
        let json = serde_json::to_string(&recipe).expect("serialize");
        assert_eq!(
            serde_json::from_str::<EditRecipe>(&json).expect("deserialize"),
            recipe
        );
        // Schema 14 had no defringe.
        let mut previous = serde_json::to_value(&recipe).expect("serialize");
        previous["schema_version"] = 14.into();
        previous.as_object_mut().expect("object").remove("defringe");
        let migrated: EditRecipe = serde_json::from_value(previous).expect("migrate");
        assert_eq!(migrated.schema_version, crate::EDIT_RECIPE_SCHEMA_VERSION);
        assert_eq!(migrated.defringe, Defringe::default());
        assert!(!migrated.defringe.is_active());

        for bad in [f32::NAN, -0.1, 1.1] {
            assert!(
                Defringe {
                    purple_amount: bad,
                    ..Defringe::default()
                }
                .validate()
                .is_err()
            );
            assert!(
                Defringe {
                    green_amount: bad,
                    ..Defringe::default()
                }
                .validate()
                .is_err()
            );
        }
        for bad in [
            [330.0, 270.0],   // unordered
            [300.0, 305.0],   // too narrow
            [200.0, 320.0],   // too wide
            [-10.0, 40.0],    // outside the circle
            [330.0, 400.0],   // outside the circle
            [f32::NAN, 40.0], // not finite
            [10.0, f32::INFINITY],
        ] {
            assert!(
                Defringe {
                    purple_hue: bad,
                    ..Defringe::default()
                }
                .validate()
                .is_err(),
                "{bad:?}"
            );
            assert!(
                Defringe {
                    green_hue: bad,
                    ..Defringe::default()
                }
                .validate()
                .is_err(),
                "{bad:?}"
            );
        }
        assert!(Defringe::default().validate().is_ok());
    }

    #[test]
    fn moving_one_hue_edge_keeps_the_range_valid() {
        let valid = |[start, end]: [f32; 2]| {
            Defringe {
                purple_hue: [start, end],
                ..Defringe::default()
            }
            .validate()
            .is_ok()
        };
        // Within bounds only the moved end changes.
        assert_eq!(with_hue_edge([270.0, 330.0], 0, 280.0), [280.0, 330.0]);
        assert_eq!(with_hue_edge([270.0, 330.0], 1, 320.0), [270.0, 320.0]);
        // Crossing, over-widening and leaving the circle push the other end.
        assert_eq!(with_hue_edge([270.0, 330.0], 0, 340.0), [340.0, 350.0]);
        assert_eq!(with_hue_edge([270.0, 330.0], 1, 275.0), [265.0, 275.0]);
        assert_eq!(with_hue_edge([270.0, 330.0], 0, 100.0), [100.0, 190.0]);
        assert_eq!(with_hue_edge([270.0, 330.0], 1, 359.0), [270.0, 359.0]);
        for edge in [0, 1] {
            for value in [-50.0, 0.0, 5.0, 100.0, 355.0, 360.0, 500.0] {
                for range in [
                    DEFAULT_PURPLE_HUE,
                    DEFAULT_GREEN_HUE,
                    [0.0, 10.0],
                    [350.0, 360.0],
                ] {
                    assert!(
                        valid(with_hue_edge(range, edge, value)),
                        "{range:?} {edge} {value}"
                    );
                }
            }
        }
    }

    #[test]
    fn only_a_positive_amount_activates_a_colour() {
        let mut defringe = Defringe::default();
        assert!(!defringe.is_active());
        defringe.green_amount = 0.2;
        assert!(defringe.is_active() && defringe.green_active() && !defringe.purple_active());
    }
}
