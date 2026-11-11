//! Local noise reduction, moiré suppression and sharpening: the detail sliders of a mask layer.
//!
//! A mask layer cannot run these filters itself: they reach over a neighbourhood at source
//! resolution, before optics, whereas masks live in the developed frame. So the pipeline develops
//! a second copy of the image with [`DetailSettings::with_local`] applied to the whole frame, and
//! the adjustment stage blends it in by the layer's alpha. See `rohditor_core`'s `local_detail`.
//!
//! Each slider is an offset on its global counterpart, so a mask can also take detail processing
//! away (keep the grain on a face while the rest of the frame is denoised).

use serde::{Deserialize, Serialize};

use crate::{
    CAPTURE_AMOUNT_RANGE, CHROMA_NOISE_REDUCTION_RANGE, CaptureSharpening, EditError, EditRecipe,
    FALSE_COLOR_MAX_STEPS, FalseColorSuppression, LUMINANCE_NOISE_REDUCTION_RANGE,
    LocalAdjustments, MaskLayer, NoiseReduction, ParameterRange, PresenceAdjustments,
};

/// Most different [`LocalDetail`] values in one recipe. Every distinct value is a full copy of
/// the developed image, and this keeps an export of a 24 MP photo inside the CPU working-set
/// limit. Layers with equal values share a copy.
pub const MAX_LOCAL_DETAIL_VARIANTS: usize = 3;

/// Offset on the global chroma and luminance noise reduction strengths.
pub const LOCAL_NOISE_REDUCTION_RANGE: ParameterRange = ParameterRange {
    minimum: -1.0,
    maximum: 1.0,
    neutral: 0.0,
};
/// Offset on the global capture sharpening amount.
pub const LOCAL_SHARPENING_RANGE: ParameterRange = ParameterRange {
    minimum: -1.0,
    maximum: 1.0,
    neutral: 0.0,
};
/// Offset on the global false-colour median passes, as a slider range.
pub const LOCAL_FALSE_COLOR_RANGE: ParameterRange = ParameterRange {
    minimum: -(FALSE_COLOR_MAX_STEPS as f32),
    maximum: FALSE_COLOR_MAX_STEPS as f32,
    neutral: 0.0,
};

/// The detail sliders of a mask layer. All neutral at 0.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalDetail {
    pub chroma_noise: f32,
    pub luminance_noise: f32,
    /// Whole median passes.
    pub false_color_steps: i8,
    pub sharpening: f32,
}

impl LocalDetail {
    #[must_use]
    pub fn is_neutral(&self) -> bool {
        *self == Self::default()
    }

    pub(crate) fn validate(&self) -> Result<(), EditError> {
        for (field, value, range) in [
            (
                "local.layers.settings.detail.chroma_noise",
                self.chroma_noise,
                LOCAL_NOISE_REDUCTION_RANGE,
            ),
            (
                "local.layers.settings.detail.luminance_noise",
                self.luminance_noise,
                LOCAL_NOISE_REDUCTION_RANGE,
            ),
            (
                "local.layers.settings.detail.false_color_steps",
                f32::from(self.false_color_steps),
                LOCAL_FALSE_COLOR_RANGE,
            ),
            (
                "local.layers.settings.detail.sharpening",
                self.sharpening,
                LOCAL_SHARPENING_RANGE,
            ),
        ] {
            if !range.contains(value) {
                return Err(EditError {
                    field,
                    reason: format!(
                        "expected a finite value in {}..={}",
                        range.minimum, range.maximum
                    ),
                });
            }
        }
        Ok(())
    }
}

/// The settings of the source-resolution detail stage.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetailSettings {
    pub false_color: FalseColorSuppression,
    pub noise_reduction: NoiseReduction,
    pub capture_sharpening: CaptureSharpening,
}

impl DetailSettings {
    /// These settings with a layer's offsets applied, clamped to what the stage accepts. Settings
    /// a slider does not cover (luminance detail, sharpening radius) stay global.
    #[must_use]
    pub fn with_local(mut self, local: LocalDetail) -> Self {
        let offset = |global: f32, by: f32, range: ParameterRange| {
            (global + by).clamp(range.minimum, range.maximum)
        };
        self.noise_reduction.chroma = offset(
            self.noise_reduction.chroma,
            local.chroma_noise,
            CHROMA_NOISE_REDUCTION_RANGE,
        );
        self.noise_reduction.luminance = offset(
            self.noise_reduction.luminance,
            local.luminance_noise,
            LUMINANCE_NOISE_REDUCTION_RANGE,
        );
        self.false_color.steps = (i16::from(self.false_color.steps)
            + i16::from(local.false_color_steps))
        .clamp(0, i16::from(FALSE_COLOR_MAX_STEPS)) as u8;
        if local.sharpening != 0.0 {
            // A disabled stage has a parked amount: it starts from no sharpening.
            let sharpening = &mut self.capture_sharpening;
            let global = if sharpening.is_active() {
                sharpening.amount
            } else {
                0.0
            };
            sharpening.amount = offset(global, local.sharpening, CAPTURE_AMOUNT_RANGE);
            sharpening.enabled = sharpening.amount > 0.0;
        }
        self
    }
}

impl EditRecipe {
    #[must_use]
    pub const fn detail_settings(&self) -> DetailSettings {
        DetailSettings {
            false_color: self.false_color,
            noise_reduction: self.noise_reduction,
            capture_sharpening: self.capture_sharpening,
        }
    }

    /// This recipe developed with other detail settings and without masks: what the pipeline
    /// renders the blended copy of a mask layer from. Masks are left out because they only act
    /// later, and can hold many brush points.
    #[must_use]
    pub fn with_detail_settings(&self, settings: DetailSettings) -> Self {
        Self {
            schema_version: self.schema_version,
            raw: self.raw,
            optics: self.optics.clone(),
            rendering: self.rendering.clone(),
            false_color: settings.false_color,
            noise_reduction: settings.noise_reduction,
            capture_sharpening: settings.capture_sharpening,
            defringe: self.defringe,
            light: self.light.clone(),
            color: self.color.clone(),
            // Developed before the adjustment stage, which is where presence acts.
            presence: PresenceAdjustments::default(),
            geometry: self.geometry.clone(),
            local: LocalAdjustments::default(),
        }
    }
}

/// How many different non-neutral [`LocalDetail`] values `layers` hold, disabled layers too, so
/// that re-enabling a layer can never make a valid recipe invalid.
#[must_use]
pub fn local_detail_count(layers: &[MaskLayer]) -> usize {
    let mut seen: Vec<LocalDetail> = Vec::new();
    for layer in layers {
        let detail = layer.settings.detail;
        if !detail.is_neutral() && !seen.contains(&detail) {
            seen.push(detail);
        }
    }
    seen.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> DetailSettings {
        EditRecipe::default().detail_settings()
    }

    #[test]
    fn offsets_add_to_the_global_settings_and_clamp() {
        let mut global = settings();
        global.noise_reduction.chroma = 0.25;
        global.false_color.steps = 2;
        let raised = global.with_local(LocalDetail {
            chroma_noise: 0.5,
            luminance_noise: 2.0,
            false_color_steps: 1,
            sharpening: 0.0,
        });
        assert_eq!(raised.noise_reduction.chroma, 0.75);
        assert_eq!(raised.noise_reduction.luminance, 1.0);
        assert_eq!(raised.false_color.steps, 3);

        let lowered = global.with_local(LocalDetail {
            chroma_noise: -1.0,
            false_color_steps: -5,
            ..LocalDetail::default()
        });
        assert_eq!(lowered.noise_reduction.chroma, 0.0);
        assert_eq!(lowered.false_color.steps, 0);
    }

    #[test]
    fn local_sharpening_ignores_the_parked_amount_of_a_disabled_stage() {
        // The default stage is disabled with a parked amount of 0.5.
        let raised = settings().with_local(LocalDetail {
            sharpening: 0.2,
            ..LocalDetail::default()
        });
        assert!(raised.capture_sharpening.enabled);
        assert_eq!(raised.capture_sharpening.amount, 0.2);

        let lowered = settings().with_local(LocalDetail {
            sharpening: -0.2,
            ..LocalDetail::default()
        });
        assert!(!lowered.capture_sharpening.is_active());

        let mut enabled = settings();
        enabled.capture_sharpening.enabled = true;
        enabled.capture_sharpening.amount = 0.5;
        let softened = enabled.with_local(LocalDetail {
            sharpening: -0.5,
            ..LocalDetail::default()
        });
        assert!(!softened.capture_sharpening.is_active());
    }

    #[test]
    fn neutral_offsets_change_nothing() {
        assert_eq!(settings().with_local(LocalDetail::default()), settings());
    }

    #[test]
    fn a_variant_recipe_keeps_the_development_and_drops_the_masks() {
        let mut recipe = EditRecipe::default();
        recipe.light.exposure_ev = 1.0;
        recipe.local.layers.push(crate::MaskLayer {
            id: 0,
            name: String::new(),
            enabled: true,
            opacity: 1.0,
            components: Vec::new(),
            settings: crate::LocalSettings::default(),
        });
        let mut settings = recipe.detail_settings();
        settings.noise_reduction.chroma = 0.5;
        let variant = recipe.with_detail_settings(settings);
        assert_eq!(variant.noise_reduction.chroma, 0.5);
        assert_eq!(variant.light.exposure_ev, 1.0);
        assert!(variant.local.layers.is_empty());
        assert!(variant.validate().is_ok());
    }

    fn layer(id: u32, luminance_noise: f32) -> crate::MaskLayer {
        crate::MaskLayer {
            id,
            name: String::new(),
            enabled: true,
            opacity: 1.0,
            components: Vec::new(),
            settings: crate::LocalSettings {
                detail: LocalDetail {
                    luminance_noise,
                    ..LocalDetail::default()
                },
                ..crate::LocalSettings::default()
            },
        }
    }

    #[test]
    fn round_trips_and_migrates_a_layer_saved_before_the_detail_sliders() {
        let mut recipe = EditRecipe::default();
        recipe.local.layers.push(layer(0, 0.5));
        recipe.local.layers[0].settings.detail.false_color_steps = -2;
        let json = serde_json::to_value(&recipe).expect("serializes");
        let back: EditRecipe = serde_json::from_value(json.clone()).expect("round trips");
        assert_eq!(back, recipe);

        let mut previous = json;
        previous["schema_version"] = 17.into();
        previous["local"]["layers"][0]["settings"]
            .as_object_mut()
            .expect("settings are an object")
            .remove("detail");
        let migrated: EditRecipe = serde_json::from_value(previous).expect("migrates");
        assert_eq!(migrated.schema_version, crate::EDIT_RECIPE_SCHEMA_VERSION);
        assert!(migrated.local.layers[0].settings.detail.is_neutral());
    }

    #[test]
    fn validation_rejects_out_of_range_values() {
        for detail in [
            LocalDetail {
                chroma_noise: 1.5,
                ..LocalDetail::default()
            },
            LocalDetail {
                luminance_noise: f32::NAN,
                ..LocalDetail::default()
            },
            LocalDetail {
                false_color_steps: 6,
                ..LocalDetail::default()
            },
            LocalDetail {
                sharpening: -1.5,
                ..LocalDetail::default()
            },
        ] {
            let mut recipe = EditRecipe::default();
            recipe.local.layers.push(layer(0, 0.0));
            recipe.local.layers[0].settings.detail = detail;
            assert!(recipe.validate().is_err(), "{detail:?}");
        }
    }

    #[test]
    fn the_limit_counts_different_settings_on_all_layers() {
        let mut recipe = EditRecipe::default();
        for (id, value) in [(0, 0.1), (1, 0.2), (2, 0.3), (3, 0.3), (4, 0.0)] {
            recipe.local.layers.push(layer(id, value));
        }
        assert_eq!(
            local_detail_count(&recipe.local.layers),
            MAX_LOCAL_DETAIL_VARIANTS
        );
        assert!(recipe.validate().is_ok());

        // A disabled layer counts: enabling it again must not invalidate the recipe.
        let mut over = layer(5, 0.4);
        over.enabled = false;
        recipe.local.layers.push(over);
        assert!(recipe.validate().is_err());
    }
}
