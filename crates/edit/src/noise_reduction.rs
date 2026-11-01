use serde::{Deserialize, Serialize};

use crate::{EditError, ParameterRange};

pub const CHROMA_NOISE_REDUCTION_RANGE: ParameterRange = ParameterRange {
    minimum: 0.0,
    maximum: 1.0,
    neutral: 0.0,
};

pub const LUMINANCE_NOISE_REDUCTION_RANGE: ParameterRange = ParameterRange {
    minimum: 0.0,
    maximum: 1.0,
    neutral: 0.0,
};

pub const LUMINANCE_DETAIL_RANGE: ParameterRange = ParameterRange {
    minimum: 0.0,
    maximum: 1.0,
    neutral: 0.5,
};

/// Source-resolution noise reduction, applied before capture sharpening
/// (chroma first, then luminance).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NoiseReduction {
    /// Colour-noise strength in [0, 1]; 0 is off.
    pub chroma: f32,
    /// Luminance-noise strength in [0, 1]; 0 is off.
    pub luminance: f32,
    /// Fine-texture preservation of the luminance filter in [0, 1]; higher
    /// keeps more texture (and noise). Has no effect while `luminance` is 0.
    pub luminance_detail: f32,
}

impl Default for NoiseReduction {
    fn default() -> Self {
        Self {
            chroma: CHROMA_NOISE_REDUCTION_RANGE.neutral,
            luminance: LUMINANCE_NOISE_REDUCTION_RANGE.neutral,
            luminance_detail: LUMINANCE_DETAIL_RANGE.neutral,
        }
    }
}

impl NoiseReduction {
    /// Whether any noise-reduction step has work to do.
    #[must_use]
    pub fn is_active(self) -> bool {
        self.chroma_active() || self.luminance_active()
    }

    #[must_use]
    pub fn chroma_active(self) -> bool {
        self.chroma > 0.0
    }

    #[must_use]
    pub fn luminance_active(self) -> bool {
        self.luminance > 0.0
    }

    pub fn validate(self) -> Result<(), EditError> {
        for (field, value, range) in [
            (
                "noise_reduction.chroma",
                self.chroma,
                CHROMA_NOISE_REDUCTION_RANGE,
            ),
            (
                "noise_reduction.luminance",
                self.luminance,
                LUMINANCE_NOISE_REDUCTION_RANGE,
            ),
            (
                "noise_reduction.luminance_detail",
                self.luminance_detail,
                LUMINANCE_DETAIL_RANGE,
            ),
        ] {
            if !range.contains(value) {
                return Err(EditError {
                    field,
                    reason: "expected a finite value in 0..=1".to_owned(),
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
    fn round_trips_migrates_and_rejects_invalid_values() {
        let recipe = EditRecipe {
            noise_reduction: NoiseReduction {
                chroma: 0.4,
                luminance: 0.6,
                luminance_detail: 0.2,
            },
            ..EditRecipe::default()
        };
        let json = serde_json::to_string(&recipe).expect("serialize");
        assert_eq!(
            serde_json::from_str::<EditRecipe>(&json).expect("deserialize"),
            recipe
        );
        // Schema 11 had no noise reduction at all.
        let mut previous = serde_json::to_value(&recipe).expect("serialize");
        previous["schema_version"] = 11.into();
        previous
            .as_object_mut()
            .expect("object")
            .remove("noise_reduction");
        let migrated: EditRecipe = serde_json::from_value(previous).expect("migrate");
        assert_eq!(migrated.schema_version, crate::EDIT_RECIPE_SCHEMA_VERSION);
        assert!(!migrated.noise_reduction.is_active());
        for bad in [f32::NAN, -0.1, 1.1] {
            for broken in [
                NoiseReduction {
                    chroma: bad,
                    ..NoiseReduction::default()
                },
                NoiseReduction {
                    luminance: bad,
                    ..NoiseReduction::default()
                },
                NoiseReduction {
                    luminance_detail: bad,
                    ..NoiseReduction::default()
                },
            ] {
                assert!(broken.validate().is_err());
            }
        }
    }

    #[test]
    fn schema_12_chroma_only_recipes_gain_luminance_defaults() {
        let mut previous = serde_json::to_value(EditRecipe::default()).expect("serialize");
        previous["schema_version"] = 12.into();
        previous["noise_reduction"] = serde_json::json!({ "chroma": 0.4 });
        let migrated: EditRecipe = serde_json::from_value(previous).expect("migrate");
        assert_eq!(migrated.schema_version, crate::EDIT_RECIPE_SCHEMA_VERSION);
        assert_eq!(
            migrated.noise_reduction,
            NoiseReduction {
                chroma: 0.4,
                luminance: 0.0,
                luminance_detail: 0.5,
            }
        );
        assert!(migrated.noise_reduction.chroma_active());
        assert!(!migrated.noise_reduction.luminance_active());
    }
}
