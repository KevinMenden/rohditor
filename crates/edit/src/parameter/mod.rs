//! Recipe parameters: every single number a control can change.
//!
//! A frontend shows a parameter as a slider (or a wheel, or a curve handle) and
//! leaves reading, writing and "is it changed" to this module, so that all
//! frontends agree on them.
//!
//! Everything is relative to the photo's default recipe, passed as `defaults`.
//! It is what a reset restores, and it is not simply neutral: the as-shot white
//! balance and the estimated noise reduction differ from photo to photo.

#[cfg(test)]
mod tests;

use crate::{
    BLACKS_RANGE, CAPTURE_AMOUNT_RANGE, CAPTURE_NOISE_RANGE, CAPTURE_RADIUS_RANGE,
    CHROMA_NOISE_REDUCTION_RANGE, COLOR_GRADING_RANGE, CONTRAST_RANGE, DEFRINGE_AMOUNT_RANGE,
    DEFRINGE_HUE_RANGE_DEGREES, EXPOSURE_EV_RANGE, EditRecipe, FALSE_COLOR_MAX_STEPS,
    HIGHLIGHT_THRESHOLD_RANGE, HIGHLIGHTS_RANGE, HSL_CHANNEL_COUNT, HSL_HUE_RANGE,
    HSL_LUMINANCE_RANGE, HSL_SATURATION_RANGE, LUMINANCE_DETAIL_RANGE,
    LUMINANCE_NOISE_REDUCTION_RANGE, PRESENCE_RANGE, ParameterRange, RecipeSection,
    SATURATION_RANGE, SHADOWS_RANGE, TEMPERATURE_RANGE, TINT_RANGE, TONE_CURVE_RANGE,
    VIBRANCE_RANGE, WHITES_RANGE, WhiteBalance, with_hue_edge,
};

/// Values closer than this part of the range count as the same: dragging a
/// slider back to its default rarely hits it exactly.
const SAME_VALUE: f32 = 1.0e-5;

/// One number of a recipe. Manual white-balance multipliers are not included:
/// they only keep old recipes rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Parameter {
    Temperature,
    Tint,
    Exposure,
    Contrast,
    Highlights,
    Shadows,
    Whites,
    Blacks,
    ToneCurveShadows,
    ToneCurveDarks,
    ToneCurveLights,
    ToneCurveHighlights,
    Texture,
    Clarity,
    Dehaze,
    Saturation,
    Vibrance,
    /// Of one colour mixer channel, as the two below.
    HslHue(usize),
    HslSaturation(usize),
    HslLuminance(usize),
    /// Of red, green or blue, as the two below.
    GradingShadows(usize),
    GradingMidtones(usize),
    GradingHighlights(usize),
    /// The threshold of the Clip highlight method.
    HighlightClipThreshold,
    /// The threshold of the Opposed highlight method.
    HighlightDetectionThreshold,
    ChromaNoiseReduction,
    LuminanceNoiseReduction,
    LuminanceDetail,
    /// The sharpening amount, with 0 for switched off.
    CaptureAmount,
    CaptureRadius,
    CaptureNoise,
    /// A count: written rounded.
    FalseColorSteps,
    DefringePurpleAmount,
    /// Moving one end of a hue range pulls the other one along where the range
    /// would become invalid.
    DefringePurpleHueStart,
    DefringePurpleHueEnd,
    DefringeGreenAmount,
    DefringeGreenHueStart,
    DefringeGreenHueEnd,
}

impl Parameter {
    /// The parameters that exist once.
    const SINGLE: [Self; 32] = [
        Self::Temperature,
        Self::Tint,
        Self::Exposure,
        Self::Contrast,
        Self::Highlights,
        Self::Shadows,
        Self::Whites,
        Self::Blacks,
        Self::ToneCurveShadows,
        Self::ToneCurveDarks,
        Self::ToneCurveLights,
        Self::ToneCurveHighlights,
        Self::Texture,
        Self::Clarity,
        Self::Dehaze,
        Self::Saturation,
        Self::Vibrance,
        Self::HighlightClipThreshold,
        Self::HighlightDetectionThreshold,
        Self::ChromaNoiseReduction,
        Self::LuminanceNoiseReduction,
        Self::LuminanceDetail,
        Self::CaptureAmount,
        Self::CaptureRadius,
        Self::CaptureNoise,
        Self::FalseColorSteps,
        Self::DefringePurpleAmount,
        Self::DefringePurpleHueStart,
        Self::DefringePurpleHueEnd,
        Self::DefringeGreenAmount,
        Self::DefringeGreenHueStart,
        Self::DefringeGreenHueEnd,
    ];

    /// Every parameter.
    pub fn all() -> impl Iterator<Item = Self> {
        let mixer = (0..HSL_CHANNEL_COUNT).flat_map(|channel| {
            [
                Self::HslHue(channel),
                Self::HslSaturation(channel),
                Self::HslLuminance(channel),
            ]
        });
        let grading = (0..3).flat_map(|channel| {
            [
                Self::GradingShadows(channel),
                Self::GradingMidtones(channel),
                Self::GradingHighlights(channel),
            ]
        });
        Self::SINGLE.into_iter().chain(mixer).chain(grading)
    }

    /// The section the parameter is reset and copied with.
    #[must_use]
    pub const fn section(self) -> RecipeSection {
        match self {
            Self::Temperature | Self::Tint => RecipeSection::WhiteBalance,
            Self::Exposure
            | Self::Contrast
            | Self::Highlights
            | Self::Shadows
            | Self::Whites
            | Self::Blacks => RecipeSection::Tone,
            Self::ToneCurveShadows
            | Self::ToneCurveDarks
            | Self::ToneCurveLights
            | Self::ToneCurveHighlights => RecipeSection::ToneCurve,
            Self::Texture | Self::Clarity | Self::Dehaze => RecipeSection::Presence,
            Self::Saturation | Self::Vibrance => RecipeSection::Saturation,
            Self::HslHue(_) | Self::HslSaturation(_) | Self::HslLuminance(_) => {
                RecipeSection::ColorMixer
            }
            Self::GradingShadows(_) | Self::GradingMidtones(_) | Self::GradingHighlights(_) => {
                RecipeSection::ColorGrading
            }
            Self::HighlightClipThreshold | Self::HighlightDetectionThreshold => {
                RecipeSection::HighlightReconstruction
            }
            Self::ChromaNoiseReduction | Self::LuminanceNoiseReduction | Self::LuminanceDetail => {
                RecipeSection::NoiseReduction
            }
            Self::CaptureAmount | Self::CaptureRadius | Self::CaptureNoise => {
                RecipeSection::CaptureSharpening
            }
            Self::FalseColorSteps => RecipeSection::FalseColor,
            Self::DefringePurpleAmount
            | Self::DefringePurpleHueStart
            | Self::DefringePurpleHueEnd
            | Self::DefringeGreenAmount
            | Self::DefringeGreenHueStart
            | Self::DefringeGreenHueEnd => RecipeSection::Defringe,
        }
    }

    /// The values the parameter takes. Its neutral value is the one without an
    /// effect, where there is one.
    #[must_use]
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
            Self::ToneCurveShadows
            | Self::ToneCurveDarks
            | Self::ToneCurveLights
            | Self::ToneCurveHighlights => TONE_CURVE_RANGE,
            Self::Texture | Self::Clarity | Self::Dehaze => PRESENCE_RANGE,
            Self::Saturation => SATURATION_RANGE,
            Self::Vibrance => VIBRANCE_RANGE,
            Self::HslHue(_) => HSL_HUE_RANGE,
            Self::HslSaturation(_) => HSL_SATURATION_RANGE,
            Self::HslLuminance(_) => HSL_LUMINANCE_RANGE,
            Self::GradingShadows(_) | Self::GradingMidtones(_) | Self::GradingHighlights(_) => {
                COLOR_GRADING_RANGE
            }
            Self::HighlightClipThreshold | Self::HighlightDetectionThreshold => {
                HIGHLIGHT_THRESHOLD_RANGE
            }
            Self::ChromaNoiseReduction => CHROMA_NOISE_REDUCTION_RANGE,
            Self::LuminanceNoiseReduction => LUMINANCE_NOISE_REDUCTION_RANGE,
            Self::LuminanceDetail => LUMINANCE_DETAIL_RANGE,
            // The recipe's neutral is the amount used once it is switched on.
            Self::CaptureAmount => ParameterRange {
                neutral: CAPTURE_AMOUNT_RANGE.minimum,
                ..CAPTURE_AMOUNT_RANGE
            },
            Self::CaptureRadius => CAPTURE_RADIUS_RANGE,
            Self::CaptureNoise => CAPTURE_NOISE_RANGE,
            Self::FalseColorSteps => ParameterRange {
                minimum: 0.0,
                maximum: FALSE_COLOR_MAX_STEPS as f32,
                neutral: 0.0,
            },
            Self::DefringePurpleAmount | Self::DefringeGreenAmount => DEFRINGE_AMOUNT_RANGE,
            Self::DefringePurpleHueStart
            | Self::DefringePurpleHueEnd
            | Self::DefringeGreenHueStart
            | Self::DefringeGreenHueEnd => DEFRINGE_HUE_RANGE_DEGREES,
        }
    }

    /// The value in `recipe`. A white balance without coordinates of its own
    /// has those of `defaults`.
    #[must_use]
    pub fn get(self, recipe: &EditRecipe, defaults: &EditRecipe) -> f32 {
        let (light, color) = (&recipe.light, &recipe.color);
        match self {
            Self::Temperature => coordinates(recipe, defaults).0,
            Self::Tint => coordinates(recipe, defaults).1,
            Self::Exposure => light.exposure_ev,
            Self::Contrast => light.contrast,
            Self::Highlights => light.highlights,
            Self::Shadows => light.shadows,
            Self::Whites => light.whites,
            Self::Blacks => light.blacks,
            Self::ToneCurveShadows => light.tone_curve.shadows,
            Self::ToneCurveDarks => light.tone_curve.darks,
            Self::ToneCurveLights => light.tone_curve.lights,
            Self::ToneCurveHighlights => light.tone_curve.highlights,
            Self::Texture => recipe.presence.texture,
            Self::Clarity => recipe.presence.clarity,
            Self::Dehaze => recipe.presence.dehaze,
            Self::Saturation => color.saturation,
            Self::Vibrance => color.vibrance,
            Self::HslHue(channel) => color.hsl.channels[channel].hue,
            Self::HslSaturation(channel) => color.hsl.channels[channel].saturation,
            Self::HslLuminance(channel) => color.hsl.channels[channel].luminance,
            Self::GradingShadows(channel) => color.grading.shadows[channel],
            Self::GradingMidtones(channel) => color.grading.midtones[channel],
            Self::GradingHighlights(channel) => color.grading.highlights[channel],
            Self::HighlightClipThreshold => recipe.raw.highlights.clip.threshold,
            Self::HighlightDetectionThreshold => recipe.raw.highlights.opposed.detection_threshold,
            Self::ChromaNoiseReduction => recipe.noise_reduction.chroma,
            Self::LuminanceNoiseReduction => recipe.noise_reduction.luminance,
            Self::LuminanceDetail => recipe.noise_reduction.luminance_detail,
            Self::CaptureAmount if recipe.capture_sharpening.enabled => {
                recipe.capture_sharpening.amount
            }
            Self::CaptureAmount => 0.0,
            Self::CaptureRadius => recipe.capture_sharpening.radius,
            Self::CaptureNoise => recipe.capture_sharpening.noise_protection,
            Self::FalseColorSteps => f32::from(recipe.false_color.steps),
            Self::DefringePurpleAmount => recipe.defringe.purple_amount,
            Self::DefringePurpleHueStart => recipe.defringe.purple_hue[0],
            Self::DefringePurpleHueEnd => recipe.defringe.purple_hue[1],
            Self::DefringeGreenAmount => recipe.defringe.green_amount,
            Self::DefringeGreenHueStart => recipe.defringe.green_hue[0],
            Self::DefringeGreenHueEnd => recipe.defringe.green_hue[1],
        }
    }

    /// `recipe` with this parameter at `value`, which should be in its range.
    #[must_use]
    pub fn with_value(self, recipe: &EditRecipe, defaults: &EditRecipe, value: f32) -> EditRecipe {
        let mut next = recipe.clone();
        self.write(&mut next, defaults, value);
        next
    }

    /// Whether `recipe` has another value than `defaults`.
    #[must_use]
    pub fn is_changed(self, recipe: &EditRecipe, defaults: &EditRecipe) -> bool {
        // A balance that the coordinates cannot show.
        let hidden = self.section() == RecipeSection::WhiteBalance
            && recipe.color.white_balance != defaults.color.white_balance
            && !matches!(
                recipe.color.white_balance,
                WhiteBalance::TemperatureTint { .. }
            );
        let range = self.range();
        let distance = (self.get(recipe, defaults) - self.get(defaults, defaults)).abs();
        hidden || distance > (range.maximum - range.minimum) * SAME_VALUE
    }

    pub(crate) fn write(self, recipe: &mut EditRecipe, defaults: &EditRecipe, value: f32) {
        let (temperature, tint) = coordinates(recipe, defaults);
        let (light, color) = (&mut recipe.light, &mut recipe.color);
        let (capture, defringe) = (&mut recipe.capture_sharpening, &mut recipe.defringe);
        match self {
            Self::Temperature => {
                color.white_balance = WhiteBalance::TemperatureTint {
                    temperature: value,
                    tint,
                };
            }
            Self::Tint => {
                color.white_balance = WhiteBalance::TemperatureTint {
                    temperature,
                    tint: value,
                };
            }
            Self::Exposure => light.exposure_ev = value,
            Self::Contrast => light.contrast = value,
            Self::Highlights => light.highlights = value,
            Self::Shadows => light.shadows = value,
            Self::Whites => light.whites = value,
            Self::Blacks => light.blacks = value,
            Self::ToneCurveShadows => light.tone_curve.shadows = value,
            Self::ToneCurveDarks => light.tone_curve.darks = value,
            Self::ToneCurveLights => light.tone_curve.lights = value,
            Self::ToneCurveHighlights => light.tone_curve.highlights = value,
            Self::Texture => recipe.presence.texture = value,
            Self::Clarity => recipe.presence.clarity = value,
            Self::Dehaze => recipe.presence.dehaze = value,
            Self::Saturation => color.saturation = value,
            Self::Vibrance => color.vibrance = value,
            Self::HslHue(channel) => color.hsl.channels[channel].hue = value,
            Self::HslSaturation(channel) => color.hsl.channels[channel].saturation = value,
            Self::HslLuminance(channel) => color.hsl.channels[channel].luminance = value,
            Self::GradingShadows(channel) => color.grading.shadows[channel] = value,
            Self::GradingMidtones(channel) => color.grading.midtones[channel] = value,
            Self::GradingHighlights(channel) => color.grading.highlights[channel] = value,
            Self::HighlightClipThreshold => recipe.raw.highlights.clip.threshold = value,
            Self::HighlightDetectionThreshold => {
                recipe.raw.highlights.opposed.detection_threshold = value;
            }
            Self::ChromaNoiseReduction => recipe.noise_reduction.chroma = value,
            Self::LuminanceNoiseReduction => recipe.noise_reduction.luminance = value,
            Self::LuminanceDetail => recipe.noise_reduction.luminance_detail = value,
            Self::CaptureAmount if value > 0.0 => {
                capture.enabled = true;
                capture.amount = value;
            }
            // The amount does nothing now, so leave none behind.
            Self::CaptureAmount => {
                capture.enabled = false;
                capture.amount = CAPTURE_AMOUNT_RANGE.neutral;
            }
            Self::CaptureRadius => capture.radius = value,
            Self::CaptureNoise => capture.noise_protection = value,
            Self::FalseColorSteps => {
                let steps = value.round().clamp(0.0, f32::from(FALSE_COLOR_MAX_STEPS));
                recipe.false_color.steps = steps as u8;
            }
            Self::DefringePurpleAmount => defringe.purple_amount = value,
            Self::DefringePurpleHueStart => {
                defringe.purple_hue = with_hue_edge(defringe.purple_hue, 0, value);
            }
            Self::DefringePurpleHueEnd => {
                defringe.purple_hue = with_hue_edge(defringe.purple_hue, 1, value);
            }
            Self::DefringeGreenAmount => defringe.green_amount = value,
            Self::DefringeGreenHueStart => {
                defringe.green_hue = with_hue_edge(defringe.green_hue, 0, value);
            }
            Self::DefringeGreenHueEnd => {
                defringe.green_hue = with_hue_edge(defringe.green_hue, 1, value);
            }
        }
    }
}

/// The temperature and tint of `recipe`, or else of `defaults`.
fn coordinates(recipe: &EditRecipe, defaults: &EditRecipe) -> (f32, f32) {
    [recipe, defaults]
        .into_iter()
        .find_map(|recipe| match recipe.color.white_balance {
            WhiteBalance::TemperatureTint { temperature, tint } => Some((temperature, tint)),
            WhiteBalance::AsShot | WhiteBalance::ManualMultipliers { .. } => None,
        })
        .unwrap_or((TEMPERATURE_RANGE.neutral, TINT_RANGE.neutral))
}
