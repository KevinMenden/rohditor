use serde::{Deserialize, Serialize};

use crate::EditError;

/// Most median passes the false-colour filter offers. Each further pass
/// smooths colour further, at the cost of fine colour detail.
pub const FALSE_COLOR_MAX_STEPS: u8 = 5;

/// Moiré / false-colour suppression on the demosaiced image, the first step of
/// the detail stage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FalseColorSuppression {
    /// Median passes over the colour differences, `0..=`[`FALSE_COLOR_MAX_STEPS`];
    /// 0 is off.
    pub steps: u8,
}

impl FalseColorSuppression {
    #[must_use]
    pub fn is_active(self) -> bool {
        self.steps > 0
    }

    pub fn validate(self) -> Result<(), EditError> {
        if self.steps > FALSE_COLOR_MAX_STEPS {
            return Err(EditError {
                field: "false_color.steps",
                reason: format!("expected 0..={FALSE_COLOR_MAX_STEPS}"),
            });
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
            false_color: FalseColorSuppression { steps: 3 },
            ..EditRecipe::default()
        };
        let json = serde_json::to_string(&recipe).expect("serialize");
        assert_eq!(
            serde_json::from_str::<EditRecipe>(&json).expect("deserialize"),
            recipe
        );
        // Schema 13 had no false-colour suppression.
        let mut previous = serde_json::to_value(&recipe).expect("serialize");
        previous["schema_version"] = 13.into();
        previous
            .as_object_mut()
            .expect("object")
            .remove("false_color");
        let migrated: EditRecipe = serde_json::from_value(previous).expect("migrate");
        assert_eq!(migrated.schema_version, crate::EDIT_RECIPE_SCHEMA_VERSION);
        assert!(!migrated.false_color.is_active());
        assert!(FalseColorSuppression { steps: 5 }.validate().is_ok());
        assert!(FalseColorSuppression { steps: 6 }.validate().is_err());
    }
}
