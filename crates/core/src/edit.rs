use rohditor_raw::RawOrientation;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

use crate::PipelineError;

/// Schema version of the first non-destructive edit recipe.
pub const EDIT_RECIPE_SCHEMA_VERSION: u32 = 1;

/// Inclusive range and neutral value for one adjustment parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParameterRange {
    pub minimum: f32,
    pub maximum: f32,
    pub neutral: f32,
}

impl ParameterRange {
    #[must_use]
    pub fn contains(self, value: f32) -> bool {
        value.is_finite() && value >= self.minimum && value <= self.maximum
    }
}

pub const EXPOSURE_EV_RANGE: ParameterRange = ParameterRange {
    minimum: -5.0,
    maximum: 5.0,
    neutral: 0.0,
};
pub const CONTRAST_RANGE: ParameterRange = ParameterRange {
    minimum: -1.0,
    maximum: 1.0,
    neutral: 0.0,
};
pub const SATURATION_RANGE: ParameterRange = ParameterRange {
    minimum: 0.0,
    maximum: 2.0,
    neutral: 1.0,
};
pub const WHITE_BALANCE_MULTIPLIER_RANGE: ParameterRange = ParameterRange {
    minimum: 0.25,
    maximum: 4.0,
    neutral: 1.0,
};

/// White balance relative to the decoder's as-shot channel multipliers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum WhiteBalance {
    #[default]
    AsShot,
    ManualMultipliers {
        red: f32,
        green: f32,
        blue: f32,
    },
}

/// Serializable, non-destructive global edits for the Phase 2 CPU pipeline.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EditRecipe {
    pub schema_version: u32,
    pub white_balance: WhiteBalance,
    pub exposure_ev: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub orientation_override: Option<RawOrientation>,
}

impl Default for EditRecipe {
    fn default() -> Self {
        Self {
            schema_version: EDIT_RECIPE_SCHEMA_VERSION,
            white_balance: WhiteBalance::AsShot,
            exposure_ev: EXPOSURE_EV_RANGE.neutral,
            contrast: CONTRAST_RANGE.neutral,
            saturation: SATURATION_RANGE.neutral,
            orientation_override: None,
        }
    }
}

impl EditRecipe {
    pub fn validate(&self) -> Result<(), PipelineError> {
        if self.schema_version != EDIT_RECIPE_SCHEMA_VERSION {
            return Err(PipelineError::InvalidRecipe {
                field: "schema_version",
                reason: format!(
                    "version {} is not supported; expected {}",
                    self.schema_version, EDIT_RECIPE_SCHEMA_VERSION
                ),
            });
        }
        validate_parameter("exposure_ev", self.exposure_ev, EXPOSURE_EV_RANGE)?;
        validate_parameter("contrast", self.contrast, CONTRAST_RANGE)?;
        validate_parameter("saturation", self.saturation, SATURATION_RANGE)?;
        if let WhiteBalance::ManualMultipliers { red, green, blue } = self.white_balance {
            for (field, value) in [
                ("white_balance.red", red),
                ("white_balance.green", green),
                ("white_balance.blue", blue),
            ] {
                validate_parameter(field, value, WHITE_BALANCE_MULTIPLIER_RANGE)?;
            }
        }
        if self.orientation_override == Some(RawOrientation::Unknown) {
            return Err(PipelineError::InvalidRecipe {
                field: "orientation_override",
                reason: "unknown is metadata state, not a usable override".to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct RecipeFields {
    schema_version: u32,
    #[serde(default)]
    white_balance: WhiteBalance,
    #[serde(default)]
    exposure_ev: f32,
    #[serde(default)]
    contrast: f32,
    #[serde(default = "neutral_saturation")]
    saturation: f32,
    #[serde(default)]
    orientation_override: Option<RawOrientation>,
}

impl<'de> Deserialize<'de> for EditRecipe {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = RecipeFields::deserialize(deserializer)?;
        let recipe = Self {
            schema_version: fields.schema_version,
            white_balance: fields.white_balance,
            exposure_ev: fields.exposure_ev,
            contrast: fields.contrast,
            saturation: fields.saturation,
            orientation_override: fields.orientation_override,
        };
        recipe.validate().map_err(D::Error::custom)?;
        Ok(recipe)
    }
}

const fn neutral_saturation() -> f32 {
    SATURATION_RANGE.neutral
}

fn validate_parameter(
    field: &'static str,
    value: f32,
    range: ParameterRange,
) -> Result<(), PipelineError> {
    if range.contains(value) {
        Ok(())
    } else {
        Err(PipelineError::InvalidRecipe {
            field,
            reason: format!(
                "{value} is outside the inclusive range {}..={}",
                range.minimum, range.maximum
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{EDIT_RECIPE_SCHEMA_VERSION, EditRecipe, WhiteBalance};

    #[test]
    fn neutral_recipe_has_documented_identity_values() {
        let recipe = EditRecipe::default();
        assert_eq!(recipe.schema_version, EDIT_RECIPE_SCHEMA_VERSION);
        assert_eq!(recipe.white_balance, WhiteBalance::AsShot);
        assert_eq!(recipe.exposure_ev, 0.0);
        assert_eq!(recipe.contrast, 0.0);
        assert_eq!(recipe.saturation, 1.0);
        assert!(recipe.validate().is_ok());
    }

    #[test]
    fn deserialization_rejects_unknown_schema_versions() {
        let json = r#"{
            "schema_version": 2,
            "white_balance": { "mode": "as_shot" },
            "exposure_ev": 0.0,
            "contrast": 0.0,
            "saturation": 1.0,
            "orientation_override": null
        }"#;
        assert!(serde_json::from_str::<EditRecipe>(json).is_err());
    }

    #[test]
    fn recipe_ranges_reject_non_finite_and_out_of_range_values() {
        let mut recipe = EditRecipe {
            exposure_ev: f32::NAN,
            ..EditRecipe::default()
        };
        assert!(recipe.validate().is_err());
        recipe.exposure_ev = 0.0;
        recipe.white_balance = WhiteBalance::ManualMultipliers {
            red: 0.1,
            green: 1.0,
            blue: 1.0,
        };
        assert!(recipe.validate().is_err());
    }
}
