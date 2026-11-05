//! Range components of a mask layer: select pixels by their content instead of their position.
//!
//! A range component is mostly used as an Intersect component that refines a gradient or a
//! brush ("the sky, but only the bright part of it"). It reads a per-pixel *reference* that the
//! pipeline measures after the global exposure and base rendering, but before the Light and
//! Color controls and before any local term. The mask therefore does not move when the user
//! changes contrast, HSL or another mask. The maths lives in `rohditor-mask`; this module holds
//! the recipe model and its validation.

use serde::{Deserialize, Serialize};

use crate::local::{check, error};
use crate::{EditError, MASK_UNIT_RANGE, ParameterRange};

/// Most colour samples in one colour range.
pub const MAX_COLOR_SAMPLES: usize = 8;

/// Colour tolerance, `0..=1`. 0 selects only (nearly) the exact sampled colours; 1 selects
/// everything within [`COLOR_RANGE_MAX_DISTANCE`] of a sample in OkLab.
pub const COLOR_TOLERANCE_RANGE: ParameterRange = ParameterRange {
    minimum: 0.0,
    maximum: 1.0,
    neutral: 0.3,
};
/// OkLab lightness of a colour sample.
pub const COLOR_SAMPLE_LIGHTNESS_RANGE: ParameterRange = ParameterRange {
    minimum: 0.0,
    maximum: 1.0,
    neutral: 0.5,
};
/// OkLab `a` and `b` of a colour sample. The sRGB gamut stays inside about `-0.34..=0.28`.
pub const COLOR_SAMPLE_AB_RANGE: ParameterRange = ParameterRange {
    minimum: -0.5,
    maximum: 0.5,
    neutral: 0.0,
};
/// OkLab distance covered by a tolerance of 1.
pub const COLOR_RANGE_MAX_DISTANCE: f32 = 0.4;

/// Selects pixels by their sRGB-encoded luminance.
///
/// The mask is 1 for luminance in `low..=high` and falls to 0 over `feather_low` below `low`
/// and `feather_high` above `high` (a smoothstep). All values are in encoded-luminance units,
/// `0..=1`; a feather of 0 is a hard edge.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LuminanceRange {
    pub low: f32,
    pub high: f32,
    pub feather_low: f32,
    pub feather_high: f32,
}

impl Default for LuminanceRange {
    /// The whole tonal range, so a new range selects nothing less than the mask it refines.
    fn default() -> Self {
        Self {
            low: 0.0,
            high: 1.0,
            feather_low: 0.1,
            feather_high: 0.1,
        }
    }
}

/// One sampled colour, in OkLab.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ColorSample {
    pub lightness: f32,
    pub a: f32,
    pub b: f32,
}

/// Selects pixels whose colour is close to any of the sampled colours.
///
/// A range without samples selects nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColorRange {
    /// At most [`MAX_COLOR_SAMPLES`].
    pub samples: Vec<ColorSample>,
    /// `0..=1`, see [`COLOR_TOLERANCE_RANGE`].
    pub tolerance: f32,
}

impl Default for ColorRange {
    fn default() -> Self {
        Self {
            samples: Vec::new(),
            tolerance: COLOR_TOLERANCE_RANGE.neutral,
        }
    }
}

impl LuminanceRange {
    pub(crate) fn validate(&self) -> Result<(), EditError> {
        check(
            "local.layers.components.shape.low",
            self.low,
            MASK_UNIT_RANGE,
        )?;
        check(
            "local.layers.components.shape.high",
            self.high,
            MASK_UNIT_RANGE,
        )?;
        check(
            "local.layers.components.shape.feather_low",
            self.feather_low,
            MASK_UNIT_RANGE,
        )?;
        check(
            "local.layers.components.shape.feather_high",
            self.feather_high,
            MASK_UNIT_RANGE,
        )?;
        if self.low > self.high {
            return Err(error(
                "local.layers.components.shape.high",
                format!("must not be below low ({} > {})", self.low, self.high),
            ));
        }
        Ok(())
    }
}

impl ColorSample {
    fn validate(&self) -> Result<(), EditError> {
        const FIELD: &str = "local.layers.components.shape.samples";
        check(FIELD, self.lightness, COLOR_SAMPLE_LIGHTNESS_RANGE)?;
        check(FIELD, self.a, COLOR_SAMPLE_AB_RANGE)?;
        check(FIELD, self.b, COLOR_SAMPLE_AB_RANGE)
    }
}

impl ColorRange {
    pub(crate) fn validate(&self) -> Result<(), EditError> {
        if self.samples.len() > MAX_COLOR_SAMPLES {
            return Err(error(
                "local.layers.components.shape.samples",
                format!("at most {MAX_COLOR_SAMPLES} colour samples"),
            ));
        }
        for sample in &self.samples {
            sample.validate()?;
        }
        check(
            "local.layers.components.shape.tolerance",
            self.tolerance,
            COLOR_TOLERANCE_RANGE,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        EDIT_RECIPE_SCHEMA_VERSION, EditRecipe, LocalAdjustments, LocalSettings, MaskComponent,
        MaskLayer, MaskMode, MaskShape,
    };

    fn recipe_with(shapes: Vec<MaskShape>) -> EditRecipe {
        let components = shapes
            .into_iter()
            .map(|shape| MaskComponent {
                mode: MaskMode::Add,
                invert: false,
                shape,
            })
            .collect();
        EditRecipe {
            local: LocalAdjustments {
                layers: vec![MaskLayer {
                    id: 1,
                    name: "Range".to_owned(),
                    enabled: true,
                    opacity: 1.0,
                    components,
                    settings: LocalSettings {
                        exposure_ev: 0.5,
                        ..LocalSettings::default()
                    },
                }],
            },
            ..EditRecipe::default()
        }
    }

    fn rejected_field(shape: MaskShape) -> &'static str {
        recipe_with(vec![shape])
            .validate()
            .expect_err("recipe should be rejected")
            .field
    }

    fn luminance(edit: impl FnOnce(&mut LuminanceRange)) -> MaskShape {
        let mut range = LuminanceRange::default();
        edit(&mut range);
        MaskShape::Luminance(range)
    }

    fn color(samples: usize, edit: impl FnOnce(&mut ColorRange)) -> MaskShape {
        let mut range = ColorRange {
            samples: vec![
                ColorSample {
                    lightness: 0.6,
                    a: 0.2,
                    b: -0.1,
                };
                samples
            ],
            ..ColorRange::default()
        };
        edit(&mut range);
        MaskShape::Color(range)
    }

    #[test]
    fn range_shapes_round_trip_and_default_to_valid_values() {
        let recipe = recipe_with(vec![
            luminance(|range| {
                range.low = 0.25;
                range.high = 0.75;
            }),
            color(3, |_| {}),
        ]);
        let json = serde_json::to_string(&recipe).expect("serialize");
        assert!(json.contains(r#""kind":"luminance""#) && json.contains(r#""kind":"color""#));
        assert_eq!(
            serde_json::from_str::<EditRecipe>(&json).expect("deserialize"),
            recipe
        );
        // A freshly created range and an empty colour range are valid recipes.
        assert!(
            recipe_with(vec![
                MaskShape::Luminance(LuminanceRange::default()),
                MaskShape::Color(ColorRange::default())
            ])
            .validate()
            .is_ok()
        );
        assert!(
            recipe_with(vec![luminance(|_| {}), color(0, |_| {})])
                .local
                .layers[0]
                .components[0]
                .shape
                .is_range()
        );
    }

    #[test]
    fn luminance_range_rejects_inverted_and_out_of_range_values() {
        let field = |edit: fn(&mut LuminanceRange)| rejected_field(luminance(edit));
        assert_eq!(
            field(|range| {
                range.low = 0.8;
                range.high = 0.2;
            }),
            "local.layers.components.shape.high"
        );
        assert_eq!(
            field(|range| range.low = -0.1),
            "local.layers.components.shape.low"
        );
        assert_eq!(
            field(|range| range.high = 1.2),
            "local.layers.components.shape.high"
        );
        assert_eq!(
            field(|range| range.feather_low = f32::NAN),
            "local.layers.components.shape.feather_low"
        );
        assert_eq!(
            field(|range| range.feather_high = 2.0),
            "local.layers.components.shape.feather_high"
        );
        // low == high is a valid, single-value range.
        assert!(
            recipe_with(vec![luminance(|range| {
                range.low = 0.5;
                range.high = 0.5;
            })])
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn color_range_rejects_too_many_samples_and_out_of_range_values() {
        assert!(
            recipe_with(vec![color(MAX_COLOR_SAMPLES, |_| {})])
                .validate()
                .is_ok()
        );
        assert_eq!(
            rejected_field(color(MAX_COLOR_SAMPLES + 1, |_| {})),
            "local.layers.components.shape.samples"
        );
        assert_eq!(
            rejected_field(color(1, |range| range.samples[0].lightness = 1.5)),
            "local.layers.components.shape.samples"
        );
        assert_eq!(
            rejected_field(color(1, |range| range.samples[0].b = f32::INFINITY)),
            "local.layers.components.shape.samples"
        );
        assert_eq!(
            rejected_field(color(1, |range| range.tolerance = 1.5)),
            "local.layers.components.shape.tolerance"
        );
    }

    #[test]
    fn schema_16_recipes_migrate_to_the_current_schema() {
        let mut previous =
            serde_json::to_value(recipe_with(vec![luminance(|_| {})])).expect("serialize");
        previous["schema_version"] = 16.into();
        let migrated: EditRecipe = serde_json::from_value(previous).expect("migrate");
        assert_eq!(migrated.schema_version, EDIT_RECIPE_SCHEMA_VERSION);
        assert_eq!(migrated.local.layers.len(), 1);
    }
}
