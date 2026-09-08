use rohditor_image::Orientation;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

mod geometry;
mod light;
mod optics;

pub use geometry::{GeometryAdjustments, NormalizedCropRect};
pub use light::{LIGHT_TONE_LUT_SIZE, LightToneLut};
pub use optics::{LensProfileSelection, OpticsAdjustments};

/// Validation errors for serialized, non-destructive edit recipes.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid edit recipe field {field}: {reason}")]
pub struct EditError {
    pub field: &'static str,
    pub reason: String,
}

/// Schema version of the current non-destructive edit recipe.
pub const EDIT_RECIPE_SCHEMA_VERSION: u32 = 7;
const LEGACY_EDIT_RECIPE_SCHEMA_VERSION: u32 = 1;
const PREVIOUS_EDIT_RECIPE_SCHEMA_VERSION: u32 = 2;
const PREVIOUS_RAW_EDIT_RECIPE_SCHEMA_VERSION: u32 = 3;
const PREVIOUS_HIGHLIGHT_EDIT_RECIPE_SCHEMA_VERSION: u32 = 4;
const PREVIOUS_LOCAL_RATIOS_EDIT_RECIPE_SCHEMA_VERSION: u32 = 5;
const PREVIOUS_NO_OPTICS_EDIT_RECIPE_SCHEMA_VERSION: u32 = 6;

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
pub const HIGHLIGHTS_RANGE: ParameterRange = ParameterRange {
    minimum: -1.0,
    maximum: 1.0,
    neutral: 0.0,
};
pub const SHADOWS_RANGE: ParameterRange = ParameterRange {
    minimum: -1.0,
    maximum: 1.0,
    neutral: 0.0,
};
pub const WHITES_RANGE: ParameterRange = ParameterRange {
    minimum: -1.0,
    maximum: 1.0,
    neutral: 0.0,
};
pub const BLACKS_RANGE: ParameterRange = ParameterRange {
    minimum: -1.0,
    maximum: 1.0,
    neutral: 0.0,
};
pub const VIBRANCE_RANGE: ParameterRange = ParameterRange {
    minimum: -1.0,
    maximum: 1.0,
    neutral: 0.0,
};
pub const WHITE_BALANCE_MULTIPLIER_RANGE: ParameterRange = ParameterRange {
    minimum: 0.25,
    maximum: 4.0,
    neutral: 1.0,
};
pub const TEMPERATURE_RANGE: ParameterRange = ParameterRange {
    minimum: 2_000.0,
    maximum: 12_000.0,
    neutral: 6_500.0,
};
pub const TINT_RANGE: ParameterRange = ParameterRange {
    minimum: -1.0,
    maximum: 1.0,
    neutral: 0.0,
};
pub const HIGHLIGHT_THRESHOLD_RANGE: ParameterRange = ParameterRange {
    minimum: 0.5,
    maximum: 1.5,
    neutral: 1.0,
};
pub const TONE_CURVE_RANGE: ParameterRange = ParameterRange {
    minimum: -0.25,
    maximum: 0.25,
    neutral: 0.0,
};
pub const HSL_HUE_RANGE: ParameterRange = ParameterRange {
    minimum: -1.0,
    maximum: 1.0,
    neutral: 0.0,
};
pub const HSL_SATURATION_RANGE: ParameterRange = ParameterRange {
    minimum: -1.0,
    maximum: 1.0,
    neutral: 0.0,
};
pub const HSL_LUMINANCE_RANGE: ParameterRange = ParameterRange {
    minimum: -1.0,
    maximum: 1.0,
    neutral: 0.0,
};
pub const COLOR_GRADING_RANGE: ParameterRange = ParameterRange {
    minimum: -1.0,
    maximum: 1.0,
    neutral: 0.0,
};

pub const HSL_CHANNEL_COUNT: usize = 8;

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
    TemperatureTint {
        temperature: f32,
        tint: f32,
    },
}

/// Destructive RAW-stage highlight handling. `Off` remains the default so
/// normalized over-range samples are retained for later processing stages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HighlightMethod {
    #[default]
    Off,
    Clip,
    LocalRatios,
    Opposed,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClipAdjustments {
    #[serde(default = "default_highlight_threshold")]
    pub threshold: f32,
}

impl Default for ClipAdjustments {
    fn default() -> Self {
        Self {
            threshold: HIGHLIGHT_THRESHOLD_RANGE.neutral,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LocalRatioAdjustments {
    #[serde(default = "default_local_ratio_detection_threshold")]
    pub detection_threshold: f32,
}

impl Default for LocalRatioAdjustments {
    fn default() -> Self {
        Self {
            detection_threshold: HIGHLIGHT_THRESHOLD_RANGE.neutral,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OpposedAdjustments {
    #[serde(default = "default_opposed_detection_threshold")]
    pub detection_threshold: f32,
}

impl Default for OpposedAdjustments {
    fn default() -> Self {
        Self {
            detection_threshold: HIGHLIGHT_THRESHOLD_RANGE.neutral,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct HighlightAdjustments {
    #[serde(default)]
    pub method: HighlightMethod,
    #[serde(default)]
    pub clip: ClipAdjustments,
    #[serde(default)]
    pub local_ratios: LocalRatioAdjustments,
    #[serde(default)]
    pub opposed: OpposedAdjustments,
}

impl Default for HighlightAdjustments {
    fn default() -> Self {
        Self {
            method: HighlightMethod::Off,
            clip: ClipAdjustments::default(),
            local_ratios: LocalRatioAdjustments::default(),
            opposed: OpposedAdjustments::default(),
        }
    }
}

#[derive(Deserialize)]
struct HighlightAdjustmentsFields {
    #[serde(default)]
    method: HighlightMethod,
    #[serde(default)]
    clip: Option<ClipAdjustments>,
    #[serde(default)]
    local_ratios: Option<LocalRatioAdjustments>,
    #[serde(default)]
    opposed: Option<OpposedAdjustments>,
    // Schema version 4 stored Clip's threshold directly on this object.
    #[serde(default, rename = "threshold")]
    legacy_threshold: Option<f32>,
}

impl<'de> Deserialize<'de> for HighlightAdjustments {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = HighlightAdjustmentsFields::deserialize(deserializer)?;
        let mut clip = fields.clip.unwrap_or_default();
        if let Some(threshold) = fields.legacy_threshold {
            clip.threshold = threshold;
        }
        Ok(Self {
            method: fields.method,
            clip,
            local_ratios: fields.local_ratios.unwrap_or_default(),
            opposed: fields.opposed.unwrap_or_default(),
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct RawAdjustments {
    #[serde(default)]
    pub highlights: HighlightAdjustments,
}

/// Scene-light controls applied after camera color conversion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LightAdjustments {
    #[serde(default)]
    pub exposure_ev: f32,
    #[serde(default)]
    pub contrast: f32,
    #[serde(default)]
    pub highlights: f32,
    #[serde(default)]
    pub shadows: f32,
    #[serde(default)]
    pub whites: f32,
    #[serde(default)]
    pub blacks: f32,
    #[serde(default)]
    pub tone_curve: ToneCurve,
}

/// Four broad point-curve regions. Values are scene-linear luminance offsets
/// around the identity curve; keeping the points grouped makes a future free
/// point-curve editor a compatible extension of the recipe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToneCurve {
    #[serde(default)]
    pub shadows: f32,
    #[serde(default)]
    pub darks: f32,
    #[serde(default)]
    pub lights: f32,
    #[serde(default)]
    pub highlights: f32,
}

impl Default for ToneCurve {
    fn default() -> Self {
        Self {
            shadows: TONE_CURVE_RANGE.neutral,
            darks: TONE_CURVE_RANGE.neutral,
            lights: TONE_CURVE_RANGE.neutral,
            highlights: TONE_CURVE_RANGE.neutral,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HslChannelAdjustments {
    #[serde(default)]
    pub hue: f32,
    #[serde(default)]
    pub saturation: f32,
    #[serde(default)]
    pub luminance: f32,
}

impl Default for HslChannelAdjustments {
    fn default() -> Self {
        Self {
            hue: HSL_HUE_RANGE.neutral,
            saturation: HSL_SATURATION_RANGE.neutral,
            luminance: HSL_LUMINANCE_RANGE.neutral,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HslAdjustments {
    #[serde(default = "default_hsl_channels")]
    pub channels: [HslChannelAdjustments; HSL_CHANNEL_COUNT],
}

impl Default for HslAdjustments {
    fn default() -> Self {
        Self {
            channels: default_hsl_channels(),
        }
    }
}

/// Three-way scene-linear RGB tint strengths. Each group is a chroma tint
/// applied with a luminance-preserving positive multiplier; it is deliberately
/// not a lift/gamma/gain or color-wheel grade.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ColorGradingAdjustments {
    #[serde(default)]
    pub shadows: [f32; 3],
    #[serde(default)]
    pub midtones: [f32; 3],
    #[serde(default)]
    pub highlights: [f32; 3],
}

impl Default for ColorGradingAdjustments {
    fn default() -> Self {
        Self {
            shadows: [COLOR_GRADING_RANGE.neutral; 3],
            midtones: [COLOR_GRADING_RANGE.neutral; 3],
            highlights: [COLOR_GRADING_RANGE.neutral; 3],
        }
    }
}

impl Default for LightAdjustments {
    fn default() -> Self {
        Self {
            exposure_ev: EXPOSURE_EV_RANGE.neutral,
            contrast: CONTRAST_RANGE.neutral,
            highlights: HIGHLIGHTS_RANGE.neutral,
            shadows: SHADOWS_RANGE.neutral,
            whites: WHITES_RANGE.neutral,
            blacks: BLACKS_RANGE.neutral,
            tone_curve: ToneCurve::default(),
        }
    }
}

/// Global color controls. White balance remains upstream of the camera matrix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorAdjustments {
    #[serde(default)]
    pub white_balance: WhiteBalance,
    #[serde(default = "neutral_saturation")]
    pub saturation: f32,
    #[serde(default)]
    pub vibrance: f32,
    #[serde(default)]
    pub hsl: HslAdjustments,
    #[serde(default)]
    pub grading: ColorGradingAdjustments,
}

impl Default for ColorAdjustments {
    fn default() -> Self {
        Self {
            white_balance: WhiteBalance::AsShot,
            saturation: SATURATION_RANGE.neutral,
            vibrance: VIBRANCE_RANGE.neutral,
            hsl: HslAdjustments::default(),
            grading: ColorGradingAdjustments::default(),
        }
    }
}

/// Serializable, non-destructive global edits.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EditRecipe {
    pub schema_version: u32,
    #[serde(default)]
    pub raw: RawAdjustments,
    #[serde(default)]
    pub optics: OpticsAdjustments,
    pub light: LightAdjustments,
    pub color: ColorAdjustments,
    pub geometry: GeometryAdjustments,
}

impl Default for EditRecipe {
    fn default() -> Self {
        Self {
            schema_version: EDIT_RECIPE_SCHEMA_VERSION,
            raw: RawAdjustments::default(),
            optics: OpticsAdjustments::default(),
            light: LightAdjustments::default(),
            color: ColorAdjustments::default(),
            geometry: GeometryAdjustments::default(),
        }
    }
}

impl EditRecipe {
    pub fn validate(&self) -> Result<(), EditError> {
        if self.schema_version != EDIT_RECIPE_SCHEMA_VERSION {
            return Err(EditError {
                field: "schema_version",
                reason: format!(
                    "version {} is not supported; expected {}",
                    self.schema_version, EDIT_RECIPE_SCHEMA_VERSION
                ),
            });
        }
        if let LensProfileSelection::Lensfun { profile_id } = &self.optics.profile {
            if profile_id.trim().is_empty() {
                return Err(EditError {
                    field: "optics.profile.profile_id",
                    reason: "profile ID must not be empty".to_owned(),
                });
            }
            if profile_id.len() > 256 {
                return Err(EditError {
                    field: "optics.profile.profile_id",
                    reason: "profile ID is limited to 256 bytes".to_owned(),
                });
            }
        }
        validate_parameter(
            "raw.highlights.clip.threshold",
            self.raw.highlights.clip.threshold,
            HIGHLIGHT_THRESHOLD_RANGE,
        )?;
        validate_parameter(
            "raw.highlights.local_ratios.detection_threshold",
            self.raw.highlights.local_ratios.detection_threshold,
            HIGHLIGHT_THRESHOLD_RANGE,
        )?;
        validate_parameter(
            "raw.highlights.opposed.detection_threshold",
            self.raw.highlights.opposed.detection_threshold,
            HIGHLIGHT_THRESHOLD_RANGE,
        )?;
        validate_parameter(
            "light.exposure_ev",
            self.light.exposure_ev,
            EXPOSURE_EV_RANGE,
        )?;
        validate_parameter("light.contrast", self.light.contrast, CONTRAST_RANGE)?;
        validate_parameter("light.highlights", self.light.highlights, HIGHLIGHTS_RANGE)?;
        validate_parameter("light.shadows", self.light.shadows, SHADOWS_RANGE)?;
        validate_parameter("light.whites", self.light.whites, WHITES_RANGE)?;
        validate_parameter("light.blacks", self.light.blacks, BLACKS_RANGE)?;
        validate_parameter(
            "light.tone_curve.shadows",
            self.light.tone_curve.shadows,
            TONE_CURVE_RANGE,
        )?;
        validate_parameter(
            "light.tone_curve.darks",
            self.light.tone_curve.darks,
            TONE_CURVE_RANGE,
        )?;
        validate_parameter(
            "light.tone_curve.lights",
            self.light.tone_curve.lights,
            TONE_CURVE_RANGE,
        )?;
        validate_parameter(
            "light.tone_curve.highlights",
            self.light.tone_curve.highlights,
            TONE_CURVE_RANGE,
        )?;
        validate_parameter("color.saturation", self.color.saturation, SATURATION_RANGE)?;
        validate_parameter("color.vibrance", self.color.vibrance, VIBRANCE_RANGE)?;
        for channel in self.color.hsl.channels {
            validate_parameter("color.hsl.hue", channel.hue, HSL_HUE_RANGE)?;
            validate_parameter(
                "color.hsl.saturation",
                channel.saturation,
                HSL_SATURATION_RANGE,
            )?;
            validate_parameter(
                "color.hsl.luminance",
                channel.luminance,
                HSL_LUMINANCE_RANGE,
            )?;
        }
        for grade in [
            self.color.grading.shadows,
            self.color.grading.midtones,
            self.color.grading.highlights,
        ] {
            for value in grade {
                validate_parameter("color.grading", value, COLOR_GRADING_RANGE)?;
            }
        }
        if let WhiteBalance::ManualMultipliers { red, green, blue } = self.color.white_balance {
            for (field, value) in [
                ("color.white_balance.red", red),
                ("color.white_balance.green", green),
                ("color.white_balance.blue", blue),
            ] {
                validate_parameter(field, value, WHITE_BALANCE_MULTIPLIER_RANGE)?;
            }
        }
        if let WhiteBalance::TemperatureTint { temperature, tint } = self.color.white_balance {
            validate_parameter(
                "color.white_balance.temperature",
                temperature,
                TEMPERATURE_RANGE,
            )?;
            validate_parameter("color.white_balance.tint", tint, TINT_RANGE)?;
        }
        if self.geometry.orientation_override == Some(Orientation::Unknown) {
            return Err(EditError {
                field: "geometry.orientation_override",
                reason: "unknown is metadata state, not a usable override".to_owned(),
            });
        }
        if let Some(crop) = self.geometry.crop {
            crop.validate()?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct RecipeFields {
    schema_version: u32,
    #[serde(default)]
    raw: RawAdjustments,
    #[serde(default)]
    optics: OpticsAdjustments,
    #[serde(default)]
    light: LightAdjustments,
    #[serde(default)]
    color: ColorAdjustments,
    #[serde(default)]
    geometry: GeometryAdjustments,
    // v1 fields are accepted only to migrate old sidecars/recipes.
    #[serde(default, alias = "white_balance")]
    legacy_white_balance: Option<WhiteBalance>,
    #[serde(default, alias = "exposure_ev")]
    legacy_exposure_ev: Option<f32>,
    #[serde(default, alias = "contrast")]
    legacy_contrast: Option<f32>,
    #[serde(default, alias = "saturation")]
    legacy_saturation: Option<f32>,
    #[serde(default, alias = "orientation_override")]
    legacy_orientation_override: Option<Orientation>,
}

impl<'de> Deserialize<'de> for EditRecipe {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = RecipeFields::deserialize(deserializer)?;
        let recipe = if fields.schema_version == LEGACY_EDIT_RECIPE_SCHEMA_VERSION {
            let mut light = fields.light;
            light.exposure_ev = fields.legacy_exposure_ev.unwrap_or(light.exposure_ev);
            light.contrast = fields.legacy_contrast.unwrap_or(light.contrast);
            let mut color = fields.color;
            color.white_balance = fields.legacy_white_balance.unwrap_or(color.white_balance);
            color.saturation = fields.legacy_saturation.unwrap_or(color.saturation);
            Self {
                schema_version: EDIT_RECIPE_SCHEMA_VERSION,
                raw: RawAdjustments::default(),
                optics: OpticsAdjustments::default(),
                light,
                color,
                geometry: GeometryAdjustments {
                    orientation_override: fields.legacy_orientation_override,
                    crop: None,
                },
            }
        } else if matches!(
            fields.schema_version,
            PREVIOUS_EDIT_RECIPE_SCHEMA_VERSION | PREVIOUS_RAW_EDIT_RECIPE_SCHEMA_VERSION
        ) {
            Self {
                schema_version: EDIT_RECIPE_SCHEMA_VERSION,
                raw: RawAdjustments::default(),
                optics: OpticsAdjustments::default(),
                light: fields.light,
                color: fields.color,
                geometry: fields.geometry,
            }
        } else if matches!(
            fields.schema_version,
            PREVIOUS_HIGHLIGHT_EDIT_RECIPE_SCHEMA_VERSION
                | PREVIOUS_LOCAL_RATIOS_EDIT_RECIPE_SCHEMA_VERSION
                | PREVIOUS_NO_OPTICS_EDIT_RECIPE_SCHEMA_VERSION
        ) {
            Self {
                schema_version: EDIT_RECIPE_SCHEMA_VERSION,
                raw: fields.raw,
                optics: OpticsAdjustments::default(),
                light: fields.light,
                color: fields.color,
                geometry: fields.geometry,
            }
        } else {
            Self {
                schema_version: fields.schema_version,
                raw: fields.raw,
                optics: fields.optics,
                light: fields.light,
                color: fields.color,
                geometry: fields.geometry,
            }
        };
        recipe.validate().map_err(D::Error::custom)?;
        Ok(recipe)
    }
}

const fn neutral_saturation() -> f32 {
    SATURATION_RANGE.neutral
}

const fn default_highlight_threshold() -> f32 {
    HIGHLIGHT_THRESHOLD_RANGE.neutral
}

const fn default_local_ratio_detection_threshold() -> f32 {
    HIGHLIGHT_THRESHOLD_RANGE.neutral
}

const fn default_opposed_detection_threshold() -> f32 {
    HIGHLIGHT_THRESHOLD_RANGE.neutral
}

fn default_hsl_channels() -> [HslChannelAdjustments; HSL_CHANNEL_COUNT] {
    [HslChannelAdjustments::default(); HSL_CHANNEL_COUNT]
}

fn validate_parameter(
    field: &'static str,
    value: f32,
    range: ParameterRange,
) -> Result<(), EditError> {
    if range.contains(value) {
        Ok(())
    } else {
        Err(EditError {
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
    use super::{
        EDIT_RECIPE_SCHEMA_VERSION, EditRecipe, HIGHLIGHT_THRESHOLD_RANGE, HighlightMethod,
        LensProfileSelection, NormalizedCropRect, WhiteBalance,
    };

    #[test]
    fn neutral_recipe_has_documented_identity_values() {
        let recipe = EditRecipe::default();
        assert_eq!(recipe.schema_version, EDIT_RECIPE_SCHEMA_VERSION);
        assert_eq!(recipe.color.white_balance, WhiteBalance::AsShot);
        assert_eq!(recipe.light.exposure_ev, 0.0);
        assert_eq!(recipe.light.contrast, 0.0);
        assert_eq!(recipe.color.saturation, 1.0);
        assert_eq!(recipe.raw.highlights.method, HighlightMethod::Off);
        assert_eq!(
            recipe.raw.highlights.clip.threshold,
            HIGHLIGHT_THRESHOLD_RANGE.neutral
        );
        assert_eq!(
            recipe.raw.highlights.local_ratios.detection_threshold,
            HIGHLIGHT_THRESHOLD_RANGE.neutral
        );
        assert_eq!(
            recipe.raw.highlights.opposed.detection_threshold,
            HIGHLIGHT_THRESHOLD_RANGE.neutral
        );
        assert!(recipe.validate().is_ok());
    }

    #[test]
    fn deserialization_rejects_unknown_schema_versions() {
        let json = r#"{
            "schema_version": 8,
            "light": {},
            "color": {},
            "geometry": {}
        }"#;
        assert!(serde_json::from_str::<EditRecipe>(json).is_err());
    }

    #[test]
    fn missing_highlight_fields_receive_the_current_defaults() {
        let json = r#"{
            "schema_version": 7,
            "light": {},
            "color": {},
            "geometry": {}
        }"#;
        let recipe = serde_json::from_str::<EditRecipe>(json).expect("current default fields");
        assert_eq!(recipe.raw.highlights.method, HighlightMethod::Off);
        assert_eq!(recipe.raw.highlights.clip.threshold, 1.0);
        assert_eq!(recipe.raw.highlights.local_ratios.detection_threshold, 1.0);
        assert_eq!(recipe.raw.highlights.opposed.detection_threshold, 1.0);
    }

    #[test]
    fn deserialization_rejects_unknown_highlight_methods() {
        let json = r#"{
            "schema_version": 4,
            "raw": { "highlights": { "method": "guided_laplacian", "threshold": 1.0 } },
            "light": {},
            "color": {},
            "geometry": {}
        }"#;
        assert!(serde_json::from_str::<EditRecipe>(json).is_err());
    }

    #[test]
    fn legacy_recipe_is_migrated_into_current_groups() {
        let json = r#"{
            "schema_version": 1,
            "white_balance": { "mode": "as_shot" },
            "exposure_ev": 1.0,
            "contrast": -0.25,
            "saturation": 1.25,
            "orientation_override": null
        }"#;
        let recipe = serde_json::from_str::<EditRecipe>(json).expect("v1 migration");
        assert_eq!(recipe.schema_version, EDIT_RECIPE_SCHEMA_VERSION);
        assert_eq!(recipe.light.exposure_ev, 1.0);
        assert_eq!(recipe.color.saturation, 1.25);
    }

    #[test]
    fn version_two_recipe_migrates_with_a_neutral_crop() {
        let json = r#"{
            "schema_version": 2,
            "light": {},
            "color": {},
            "geometry": { "orientation_override": null }
        }"#;
        let recipe = serde_json::from_str::<EditRecipe>(json).expect("v2 migration");
        assert_eq!(recipe.schema_version, EDIT_RECIPE_SCHEMA_VERSION);
        assert_eq!(recipe.geometry.crop, None);
    }

    #[test]
    fn version_three_recipe_migrates_with_highlight_clipping_off() {
        let json = r#"{
            "schema_version": 3,
            "light": {},
            "color": {},
            "geometry": {}
        }"#;
        let recipe = serde_json::from_str::<EditRecipe>(json).expect("v3 migration");
        assert_eq!(recipe.schema_version, EDIT_RECIPE_SCHEMA_VERSION);
        assert_eq!(recipe.raw.highlights.method, HighlightMethod::Off);
        assert_eq!(recipe.raw.highlights.clip.threshold, 1.0);
        assert_eq!(recipe.raw.highlights.local_ratios.detection_threshold, 1.0);
        assert_eq!(recipe.raw.highlights.opposed.detection_threshold, 1.0);
    }

    #[test]
    fn crop_round_trips_and_is_validated() {
        let mut recipe = EditRecipe::default();
        recipe.geometry.crop = Some(NormalizedCropRect {
            left: 0.1,
            top: 0.2,
            right: 0.9,
            bottom: 0.8,
        });
        let round_trip = serde_json::from_str::<EditRecipe>(
            &serde_json::to_string(&recipe).expect("serialize recipe"),
        )
        .expect("deserialize recipe");
        assert_eq!(round_trip, recipe);
        recipe.geometry.crop = Some(NormalizedCropRect {
            left: 1.0,
            top: 0.0,
            right: 0.5,
            bottom: 1.0,
        });
        assert!(recipe.validate().is_err());
    }

    #[test]
    fn recipe_ranges_reject_non_finite_and_out_of_range_values() {
        let mut recipe = EditRecipe::default();
        recipe.light.exposure_ev = f32::NAN;
        assert!(recipe.validate().is_err());
        recipe.light.exposure_ev = 0.0;
        recipe.color.white_balance = WhiteBalance::ManualMultipliers {
            red: 0.1,
            green: 1.0,
            blue: 1.0,
        };
        assert!(recipe.validate().is_err());

        recipe.color.white_balance = WhiteBalance::AsShot;
        recipe.raw.highlights.clip.threshold = f32::INFINITY;
        assert!(recipe.validate().is_err());
        recipe.raw.highlights.clip.threshold = 2.0;
        assert!(recipe.validate().is_err());

        recipe.raw.highlights.clip.threshold = 1.0;
        recipe.raw.highlights.local_ratios.detection_threshold = 2.0;
        assert!(recipe.validate().is_err());

        recipe.raw.highlights.local_ratios.detection_threshold = 1.0;
        recipe.raw.highlights.opposed.detection_threshold = 2.0;
        assert!(recipe.validate().is_err());
    }

    #[test]
    fn version_four_clip_threshold_migrates_without_loss() {
        let json = r#"{
            "schema_version": 4,
            "raw": { "highlights": { "method": "clip", "threshold": 1.23 } },
            "light": {},
            "color": {},
            "geometry": {}
        }"#;
        let recipe = serde_json::from_str::<EditRecipe>(json).expect("v4 migration");
        assert_eq!(recipe.schema_version, EDIT_RECIPE_SCHEMA_VERSION);
        assert_eq!(recipe.raw.highlights.method, HighlightMethod::Clip);
        assert_eq!(recipe.raw.highlights.clip.threshold, 1.23);
        assert_eq!(recipe.raw.highlights.local_ratios.detection_threshold, 1.0);
        assert_eq!(recipe.raw.highlights.opposed.detection_threshold, 1.0);
    }

    #[test]
    fn version_five_recipe_migrates_with_an_opposed_default() {
        let json = r#"{
            "schema_version": 5,
            "raw": {
                "highlights": {
                    "method": "local_ratios",
                    "local_ratios": { "detection_threshold": 0.74 }
                }
            },
            "light": {},
            "color": {},
            "geometry": {}
        }"#;
        let recipe = serde_json::from_str::<EditRecipe>(json).expect("v5 migration");
        assert_eq!(recipe.schema_version, EDIT_RECIPE_SCHEMA_VERSION);
        assert_eq!(recipe.raw.highlights.local_ratios.detection_threshold, 0.74);
        assert_eq!(recipe.raw.highlights.opposed.detection_threshold, 1.0);
    }

    #[test]
    fn version_six_recipe_migrates_with_optics_off() {
        let json = r#"{
            "schema_version": 6,
            "optics": {
                "profile": { "mode": "automatic" },
                "distortion": false,
                "vignetting": false,
                "chromatic_aberration": false
            },
            "light": {},
            "color": {},
            "geometry": {}
        }"#;
        let recipe = serde_json::from_str::<EditRecipe>(json).expect("v6 migration");
        assert_eq!(recipe.schema_version, EDIT_RECIPE_SCHEMA_VERSION);
        assert_eq!(recipe.optics.profile, LensProfileSelection::Off);
        assert!(recipe.optics.distortion);
        assert!(recipe.optics.vignetting);
        assert!(recipe.optics.chromatic_aberration);
    }

    #[test]
    fn local_ratios_serializes_and_round_trips_its_own_threshold() {
        let mut recipe = EditRecipe::default();
        recipe.raw.highlights.method = HighlightMethod::LocalRatios;
        recipe.raw.highlights.local_ratios.detection_threshold = 0.72;
        recipe.raw.highlights.opposed.detection_threshold = 0.83;
        recipe.raw.highlights.clip.threshold = 1.31;
        let json = serde_json::to_string(&recipe).expect("serialize recipe");
        let round_trip = serde_json::from_str::<EditRecipe>(&json).expect("deserialize recipe");
        assert_eq!(round_trip, recipe);
    }

    #[test]
    fn optics_defaults_are_off_but_component_choices_are_enabled() {
        let recipe = EditRecipe::default();
        assert_eq!(recipe.optics.profile, super::LensProfileSelection::Off);
        assert!(recipe.optics.distortion);
        assert!(recipe.optics.vignetting);
        assert!(recipe.optics.chromatic_aberration);
    }

    #[test]
    fn optics_profile_id_is_bounded_and_opaque() {
        let mut recipe = EditRecipe::default();
        recipe.optics.profile = super::LensProfileSelection::Lensfun {
            profile_id: "  ".to_owned(),
        };
        assert!(recipe.validate().is_err());
        recipe.optics.profile = super::LensProfileSelection::Lensfun {
            profile_id: "camera|lens|mount/with punctuation".to_owned(),
        };
        assert!(recipe.validate().is_ok());
        recipe.optics.profile = super::LensProfileSelection::Lensfun {
            profile_id: "x".repeat(257),
        };
        assert!(recipe.validate().is_err());
    }

    #[test]
    fn switching_methods_does_not_copy_thresholds() {
        let mut recipe = EditRecipe::default();
        recipe.raw.highlights.clip.threshold = 1.3;
        recipe.raw.highlights.local_ratios.detection_threshold = 0.7;
        recipe.raw.highlights.opposed.detection_threshold = 0.8;
        recipe.raw.highlights.method = HighlightMethod::LocalRatios;
        assert_eq!(recipe.raw.highlights.clip.threshold, 1.3);
        assert_eq!(recipe.raw.highlights.local_ratios.detection_threshold, 0.7);
        assert_eq!(recipe.raw.highlights.opposed.detection_threshold, 0.8);
        recipe.raw.highlights.method = HighlightMethod::Clip;
        assert_eq!(recipe.raw.highlights.clip.threshold, 1.3);
        assert_eq!(recipe.raw.highlights.local_ratios.detection_threshold, 0.7);
        assert_eq!(recipe.raw.highlights.opposed.detection_threshold, 0.8);
    }
}
