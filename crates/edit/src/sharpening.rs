use serde::{Deserialize, Serialize};

use crate::{EditError, ParameterRange};

pub const CAPTURE_AMOUNT_RANGE: ParameterRange = ParameterRange {
    minimum: 0.0,
    maximum: 1.0,
    neutral: 0.5,
};
pub const CAPTURE_RADIUS_RANGE: ParameterRange = ParameterRange {
    minimum: 0.3,
    maximum: 1.2,
    neutral: 0.6,
};
pub const CAPTURE_NOISE_RANGE: ParameterRange = ParameterRange {
    minimum: 0.0,
    maximum: 1.0,
    neutral: 0.5,
};

/// Source-resolution capture sharpening, independent of output size and zoom.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureSharpening {
    pub enabled: bool,
    /// Blend with the unsharpened image, in [0, 1].
    pub amount: f32,
    /// Gaussian sigma in source pixels, before optics and preview reduction.
    pub radius: f32,
    /// Higher values protect more low-contrast detail from noise amplification.
    pub noise_protection: f32,
}

impl Default for CaptureSharpening {
    fn default() -> Self {
        Self {
            enabled: false,
            amount: CAPTURE_AMOUNT_RANGE.neutral,
            radius: CAPTURE_RADIUS_RANGE.neutral,
            noise_protection: CAPTURE_NOISE_RANGE.neutral,
        }
    }
}

impl CaptureSharpening {
    #[must_use]
    pub fn is_active(self) -> bool {
        self.enabled && self.amount > 0.0
    }

    pub fn validate(self) -> Result<(), EditError> {
        for (field, value, range) in [
            (
                "capture_sharpening.amount",
                self.amount,
                CAPTURE_AMOUNT_RANGE,
            ),
            (
                "capture_sharpening.radius",
                self.radius,
                CAPTURE_RADIUS_RANGE,
            ),
            (
                "capture_sharpening.noise_protection",
                self.noise_protection,
                CAPTURE_NOISE_RANGE,
            ),
        ] {
            if !range.contains(value) {
                return Err(EditError {
                    field,
                    reason: format!(
                        "expected a finite value in {}..={}",
                        range.minimum, range.maximum
                    ),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EditRecipe;

    #[test]
    fn capture_settings_round_trip_migrate_and_reject_invalid_values() {
        let recipe = EditRecipe {
            capture_sharpening: CaptureSharpening {
                enabled: true,
                amount: 0.7,
                radius: 0.8,
                noise_protection: 0.3,
            },
            ..EditRecipe::default()
        };
        let json =
            serde_json::to_string(&recipe).expect("capture sharpening fixture should succeed");
        assert_eq!(
            serde_json::from_str::<EditRecipe>(&json)
                .expect("capture sharpening fixture should succeed"),
            recipe
        );
        let mut legacy =
            serde_json::to_value(&recipe).expect("capture sharpening fixture should succeed");
        legacy["schema_version"] = 10.into();
        legacy
            .as_object_mut()
            .expect("capture sharpening fixture should succeed")
            .remove("capture_sharpening");
        let migrated: EditRecipe =
            serde_json::from_value(legacy).expect("capture sharpening fixture should succeed");
        assert_eq!(migrated.schema_version, crate::EDIT_RECIPE_SCHEMA_VERSION);
        assert!(!migrated.capture_sharpening.enabled);
        for invalid in [
            CaptureSharpening {
                amount: f32::NAN,
                ..CaptureSharpening::default()
            },
            CaptureSharpening {
                radius: 0.0,
                ..CaptureSharpening::default()
            },
            CaptureSharpening {
                noise_protection: 1.1,
                ..CaptureSharpening::default()
            },
        ] {
            assert!(invalid.validate().is_err());
        }
    }
}
