//! The develop panel's sliders: what they are called and which recipe value they change.

use rohditor_core::WhiteBalanceCoordinates;
use rohditor_edit::{
    BLACKS_RANGE, CAPTURE_AMOUNT_RANGE, CHROMA_NOISE_REDUCTION_RANGE, CONTRAST_RANGE,
    EXPOSURE_EV_RANGE, EditRecipe, HIGHLIGHTS_RANGE, LUMINANCE_NOISE_REDUCTION_RANGE,
    PRESENCE_RANGE, ParameterRange, SATURATION_RANGE, SHADOWS_RANGE, TEMPERATURE_RANGE, TINT_RANGE,
    VIBRANCE_RANGE, WHITES_RANGE, WhiteBalance,
};

use crate::ui::slider::{Format, fraction};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adjustment {
    Temperature,
    Tint,
    Exposure,
    Contrast,
    Highlights,
    Shadows,
    Whites,
    Blacks,
    Vibrance,
    Saturation,
    Texture,
    Clarity,
    Dehaze,
    Sharpening,
    LuminanceNoise,
    ColorNoise,
}

pub struct Section {
    pub title: &'static str,
    pub adjustments: &'static [Adjustment],
}

pub const SECTIONS: [Section; 5] = [
    Section {
        title: "White Balance",
        adjustments: &[Adjustment::Temperature, Adjustment::Tint],
    },
    Section {
        title: "Light",
        adjustments: &[
            Adjustment::Exposure,
            Adjustment::Contrast,
            Adjustment::Highlights,
            Adjustment::Shadows,
            Adjustment::Whites,
            Adjustment::Blacks,
        ],
    },
    Section {
        title: "Color",
        adjustments: &[Adjustment::Vibrance, Adjustment::Saturation],
    },
    Section {
        title: "Presence",
        adjustments: &[Adjustment::Texture, Adjustment::Clarity, Adjustment::Dehaze],
    },
    Section {
        title: "Detail",
        adjustments: &[
            Adjustment::Sharpening,
            Adjustment::LuminanceNoise,
            Adjustment::ColorNoise,
        ],
    },
];

impl Adjustment {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Temperature => "Temperature",
            Self::Tint => "Tint",
            Self::Exposure => "Exposure",
            Self::Contrast => "Contrast",
            Self::Highlights => "Highlights",
            Self::Shadows => "Shadows",
            Self::Whites => "Whites",
            Self::Blacks => "Blacks",
            Self::Vibrance => "Vibrance",
            Self::Saturation => "Saturation",
            Self::Texture => "Texture",
            Self::Clarity => "Clarity",
            Self::Dehaze => "Dehaze",
            Self::Sharpening => "Sharpening",
            Self::LuminanceNoise => "Luminance Noise",
            Self::ColorNoise => "Color Noise",
        }
    }

    pub const fn range(self) -> ParameterRange {
        match self {
            Self::Temperature => TEMPERATURE_RANGE,
            Self::Tint => TINT_RANGE,
            Self::Exposure => EXPOSURE_EV_RANGE,
            Self::Contrast => CONTRAST_RANGE,
            Self::Highlights => HIGHLIGHTS_RANGE,
            Self::Shadows => SHADOWS_RANGE,
            Self::Whites => WHITES_RANGE,
            Self::Blacks => BLACKS_RANGE,
            Self::Vibrance => VIBRANCE_RANGE,
            Self::Saturation => SATURATION_RANGE,
            Self::Texture | Self::Clarity | Self::Dehaze => PRESENCE_RANGE,
            // Off is the neutral point here, while the recipe's neutral is the amount used when
            // sharpening is switched on.
            Self::Sharpening => ParameterRange {
                neutral: CAPTURE_AMOUNT_RANGE.minimum,
                ..CAPTURE_AMOUNT_RANGE
            },
            Self::LuminanceNoise => LUMINANCE_NOISE_REDUCTION_RANGE,
            Self::ColorNoise => CHROMA_NOISE_REDUCTION_RANGE,
        }
    }

    pub const fn format(self) -> Format {
        match self {
            Self::Temperature => Format::Kelvin,
            Self::Exposure => Format::Stops,
            _ => Format::Percent,
        }
    }

    pub const fn is_white_balance(self) -> bool {
        matches!(self, Self::Temperature | Self::Tint)
    }

    /// The value double-clicking resets to.
    pub fn default_value(self, as_shot: Option<WhiteBalanceCoordinates>) -> f32 {
        match self {
            Self::Temperature => coordinates(as_shot).temperature,
            Self::Tint => coordinates(as_shot).tint,
            _ => self.range().neutral,
        }
    }

    pub fn is_edited(self, recipe: &EditRecipe, as_shot: Option<WhiteBalanceCoordinates>) -> bool {
        let at = |value| fraction(self.format(), self.range(), value);
        (at(self.get(recipe, as_shot)) - at(self.default_value(as_shot))).abs() > 1e-4
    }

    pub fn get(self, recipe: &EditRecipe, as_shot: Option<WhiteBalanceCoordinates>) -> f32 {
        match self {
            Self::Temperature => white_balance(recipe, as_shot).temperature,
            Self::Tint => white_balance(recipe, as_shot).tint,
            Self::Exposure => recipe.light.exposure_ev,
            Self::Contrast => recipe.light.contrast,
            Self::Highlights => recipe.light.highlights,
            Self::Shadows => recipe.light.shadows,
            Self::Whites => recipe.light.whites,
            Self::Blacks => recipe.light.blacks,
            Self::Vibrance => recipe.color.vibrance,
            Self::Saturation => recipe.color.saturation,
            Self::Texture => recipe.presence.texture,
            Self::Clarity => recipe.presence.clarity,
            Self::Dehaze => recipe.presence.dehaze,
            Self::Sharpening if recipe.capture_sharpening.enabled => {
                recipe.capture_sharpening.amount
            }
            Self::Sharpening => 0.0,
            Self::LuminanceNoise => recipe.noise_reduction.luminance,
            Self::ColorNoise => recipe.noise_reduction.chroma,
        }
    }

    pub fn set(
        self,
        recipe: &mut EditRecipe,
        as_shot: Option<WhiteBalanceCoordinates>,
        value: f32,
    ) {
        match self {
            Self::Temperature => {
                let tint = white_balance(recipe, as_shot).tint;
                recipe.color.white_balance = WhiteBalance::TemperatureTint {
                    temperature: value,
                    tint,
                };
            }
            Self::Tint => {
                let temperature = white_balance(recipe, as_shot).temperature;
                recipe.color.white_balance = WhiteBalance::TemperatureTint {
                    temperature,
                    tint: value,
                };
            }
            Self::Exposure => recipe.light.exposure_ev = value,
            Self::Contrast => recipe.light.contrast = value,
            Self::Highlights => recipe.light.highlights = value,
            Self::Shadows => recipe.light.shadows = value,
            Self::Whites => recipe.light.whites = value,
            Self::Blacks => recipe.light.blacks = value,
            Self::Vibrance => recipe.color.vibrance = value,
            Self::Saturation => recipe.color.saturation = value,
            Self::Texture => recipe.presence.texture = value,
            Self::Clarity => recipe.presence.clarity = value,
            Self::Dehaze => recipe.presence.dehaze = value,
            Self::Sharpening => {
                recipe.capture_sharpening.enabled = value > 0.0;
                recipe.capture_sharpening.amount = value;
            }
            Self::LuminanceNoise => recipe.noise_reduction.luminance = value,
            Self::ColorNoise => recipe.noise_reduction.chroma = value,
        }
    }
}

fn coordinates(as_shot: Option<WhiteBalanceCoordinates>) -> WhiteBalanceCoordinates {
    as_shot.unwrap_or(WhiteBalanceCoordinates {
        temperature: TEMPERATURE_RANGE.neutral,
        tint: TINT_RANGE.neutral,
    })
}

fn white_balance(
    recipe: &EditRecipe,
    as_shot: Option<WhiteBalanceCoordinates>,
) -> WhiteBalanceCoordinates {
    match recipe.color.white_balance {
        WhiteBalance::TemperatureTint { temperature, tint } => {
            WhiteBalanceCoordinates { temperature, tint }
        }
        WhiteBalance::AsShot | WhiteBalance::ManualMultipliers { .. } => coordinates(as_shot),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_slider_changes_only_itself_and_keeps_the_recipe_valid() {
        let all = || SECTIONS.iter().flat_map(|section| section.adjustments);
        for &adjustment in all() {
            let range = adjustment.range();
            let target = range.neutral + (range.maximum - range.neutral) * 0.5;
            let before = EditRecipe::default();
            let mut after = before.clone();
            adjustment.set(&mut after, None, target);
            assert_eq!(adjustment.get(&after, None), target, "{adjustment:?}");
            assert!(after.validate().is_ok(), "{adjustment:?}");
            for &other in all().filter(|&&other| other != adjustment) {
                assert_eq!(
                    other.get(&after, None),
                    other.get(&before, None),
                    "{adjustment:?} changed {other:?}"
                );
            }
        }
    }
}
