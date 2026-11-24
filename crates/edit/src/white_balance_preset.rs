//! Named white balances.

use crate::{EditRecipe, Parameter, RecipeSection, WhiteBalance};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WhiteBalancePreset {
    /// The photo's default.
    AsShot,
    Daylight,
    Cloudy,
    Shade,
    Tungsten,
}

impl WhiteBalancePreset {
    pub const ALL: [Self; 5] = [
        Self::AsShot,
        Self::Daylight,
        Self::Cloudy,
        Self::Shade,
        Self::Tungsten,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::AsShot => "As shot",
            Self::Daylight => "Daylight",
            Self::Cloudy => "Cloudy",
            Self::Shade => "Shade",
            Self::Tungsten => "Tungsten",
        }
    }

    const fn temperature(self) -> Option<f32> {
        match self {
            Self::AsShot => None,
            Self::Daylight => Some(5_500.0),
            Self::Cloudy => Some(6_500.0),
            Self::Shade => Some(7_500.0),
            Self::Tungsten => Some(2_850.0),
        }
    }

    /// `recipe` with this balance. The tint keeps its zero point, which is the
    /// camera's own.
    #[must_use]
    pub fn apply(self, recipe: &EditRecipe, defaults: &EditRecipe) -> EditRecipe {
        let mut next = recipe.clone();
        next.color.white_balance = match self.temperature() {
            None => defaults.color.white_balance,
            Some(temperature) => WhiteBalance::TemperatureTint {
                temperature,
                tint: 0.0,
            },
        };
        next
    }

    /// The preset `recipe` has, or `None` for a balance of its own.
    #[must_use]
    pub fn of(recipe: &EditRecipe, defaults: &EditRecipe) -> Option<Self> {
        if !RecipeSection::WhiteBalance.is_changed(recipe, defaults) {
            return Some(Self::AsShot);
        }
        if !matches!(
            recipe.color.white_balance,
            WhiteBalance::TemperatureTint { .. }
        ) {
            return None;
        }
        Self::ALL.into_iter().find(|preset| {
            preset.temperature().is_some()
                && [Parameter::Temperature, Parameter::Tint]
                    .iter()
                    .all(|value| !value.is_changed(recipe, &preset.apply(recipe, defaults)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TEMPERATURE_RANGE, TINT_RANGE};

    fn defaults() -> EditRecipe {
        let mut recipe = EditRecipe::default();
        recipe.color.white_balance = WhiteBalance::TemperatureTint {
            temperature: 4_918.0,
            tint: 0.0,
        };
        recipe
    }

    #[test]
    fn presets_are_valid_and_recognised() {
        assert!(TINT_RANGE.contains(0.0));
        for preset in WhiteBalancePreset::ALL {
            let recipe = preset.apply(&defaults(), &defaults());
            assert!(recipe.validate().is_ok());
            assert!(
                preset
                    .temperature()
                    .is_none_or(|t| TEMPERATURE_RANGE.contains(t))
            );
            assert_eq!(WhiteBalancePreset::of(&recipe, &defaults()), Some(preset));
        }
    }

    #[test]
    fn other_balances_are_no_preset() {
        let defaults = defaults();
        let tinted = Parameter::Tint.with_value(
            &WhiteBalancePreset::Cloudy.apply(&defaults, &defaults),
            &defaults,
            0.2,
        );
        assert_eq!(WhiteBalancePreset::of(&tinted, &defaults), None);
        let mut manual = defaults.clone();
        manual.color.white_balance = WhiteBalance::ManualMultipliers {
            red: 1.2,
            green: 1.0,
            blue: 0.9,
        };
        assert_eq!(WhiteBalancePreset::of(&manual, &defaults), None);
        let back = WhiteBalancePreset::AsShot.apply(&manual, &defaults);
        assert_eq!(back, defaults);
    }
}
