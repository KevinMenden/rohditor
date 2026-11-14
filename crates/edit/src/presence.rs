//! Presence: texture, clarity and dehaze.
//!
//! Three spatial controls that change how much the picture "pops" without moving the tonal
//! sliders of Light. They act on the developed image after every other global and local
//! adjustment; `rohditor_detail::apply_presence` is the implementation.

use serde::{Deserialize, Serialize};

use crate::{EditError, ParameterRange};

/// Texture, clarity and dehaze all run from -1 (soften / add haze) to +1 (full effect).
pub const PRESENCE_RANGE: ParameterRange = ParameterRange {
    minimum: -1.0,
    maximum: 1.0,
    neutral: 0.0,
};

/// The three presence sliders. All neutral at 0.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PresenceAdjustments {
    /// Fine-scale contrast (skin, bark, fabric); negative values smooth.
    pub texture: f32,
    /// Mid-scale contrast concentrated in the mid-tones; negative values soften.
    pub clarity: f32,
    /// Removes (positive) or adds (negative) atmospheric haze.
    pub dehaze: f32,
}

impl PresenceAdjustments {
    /// Whether any slider has work to do.
    #[must_use]
    pub fn is_active(&self) -> bool {
        *self != Self::default()
    }

    pub(crate) fn validate(&self) -> Result<(), EditError> {
        for (field, value) in [
            ("presence.texture", self.texture),
            ("presence.clarity", self.clarity),
            ("presence.dehaze", self.dehaze),
        ] {
            if !PRESENCE_RANGE.contains(value) {
                return Err(EditError {
                    field,
                    reason: format!(
                        "expected a finite value in {}..={}",
                        PRESENCE_RANGE.minimum, PRESENCE_RANGE.maximum
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
    fn default_is_neutral_and_inactive() {
        let presence = PresenceAdjustments::default();
        assert!(!presence.is_active());
        assert!(presence.validate().is_ok());
    }

    #[test]
    fn rejects_out_of_range_and_non_finite_values() {
        for bad in [1.01, -1.01, f32::NAN, f32::INFINITY] {
            for set in [
                |p: &mut PresenceAdjustments, v| p.texture = v,
                |p: &mut PresenceAdjustments, v| p.clarity = v,
                |p: &mut PresenceAdjustments, v| p.dehaze = v,
            ] {
                let mut recipe = EditRecipe::default();
                set(&mut recipe.presence, bad);
                assert!(recipe.validate().is_err(), "{bad} should be rejected");
            }
        }
    }

    #[test]
    fn recipes_without_presence_load_neutral_and_round_trip() {
        let mut recipe = EditRecipe::default();
        let mut json: serde_json::Value = serde_json::to_value(&recipe).expect("serialize");
        json.as_object_mut().expect("object").remove("presence");
        let loaded: EditRecipe = serde_json::from_value(json).expect("load without presence");
        assert!(!loaded.presence.is_active());

        recipe.presence.clarity = 0.4;
        let json = serde_json::to_string(&recipe).expect("serialize");
        let loaded: EditRecipe = serde_json::from_str(&json).expect("round trip");
        assert_eq!(loaded.presence.clarity, 0.4);
    }
}
