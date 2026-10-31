use serde::{Deserialize, Serialize};

use crate::{EditError, ParameterRange};

pub const CHROMA_NOISE_REDUCTION_RANGE: ParameterRange = ParameterRange {
    minimum: 0.0,
    maximum: 1.0,
    neutral: 0.0,
};

/// Source-resolution noise reduction, applied before capture sharpening.
/// Luminance controls will join `chroma` in a later schema.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NoiseReduction {
    /// Colour-noise strength in [0, 1]; 0 is off.
    pub chroma: f32,
}

impl Default for NoiseReduction {
    fn default() -> Self {
        Self {
            chroma: CHROMA_NOISE_REDUCTION_RANGE.neutral,
        }
    }
}

impl NoiseReduction {
    #[must_use]
    pub fn is_active(self) -> bool {
        self.chroma > 0.0
    }

    pub fn validate(self) -> Result<(), EditError> {
        if CHROMA_NOISE_REDUCTION_RANGE.contains(self.chroma) {
            Ok(())
        } else {
            Err(EditError {
                field: "noise_reduction.chroma",
                reason: "expected a finite value in 0..=1".to_owned(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EditRecipe;

    #[test]
    fn chroma_round_trips_migrates_and_rejects_invalid_values() {
        let recipe = EditRecipe {
            noise_reduction: NoiseReduction { chroma: 0.4 },
            ..EditRecipe::default()
        };
        let json = serde_json::to_string(&recipe).expect("serialize");
        assert_eq!(
            serde_json::from_str::<EditRecipe>(&json).expect("deserialize"),
            recipe
        );
        let mut previous = serde_json::to_value(&recipe).expect("serialize");
        previous["schema_version"] = 11.into();
        previous
            .as_object_mut()
            .expect("object")
            .remove("noise_reduction");
        let migrated: EditRecipe = serde_json::from_value(previous).expect("migrate");
        assert_eq!(migrated.schema_version, crate::EDIT_RECIPE_SCHEMA_VERSION);
        assert!(!migrated.noise_reduction.is_active());
        for chroma in [f32::NAN, -0.1, 1.1] {
            assert!(NoiseReduction { chroma }.validate().is_err());
        }
    }
}
