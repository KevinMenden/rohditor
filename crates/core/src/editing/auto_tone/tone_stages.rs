//! A luminance-only model of the tone stages between exposure and the display
//! histogram, so Auto tone can reason in scene light without re-rendering.

use rohditor_color::linear_srgb_to_srgb;
use rohditor_edit::{EditRecipe, LightAdjustments, LightToneLut, ToneCurve};

use crate::{BaseRenderingLut, evaluate_tone_curve};

/// Scene luminance search range for inversion, in stops around linear 1.0. It
/// spans far more than any display code can distinguish.
const SEARCH_STOPS: (f32, f32) = (-20.0, 8.0);
const SEARCH_ITERATIONS: usize = 32;

/// Base rendering, Light controls, and tone curve in pipeline order.
///
/// Masks and colour adjustments are not modelled: they either affect only part
/// of the frame or barely move luminance, so they are left to the fact that
/// the histogram already includes them.
pub(super) struct ToneStages<'a> {
    base: Option<&'a BaseRenderingLut>,
    tone_curve: &'a ToneCurve,
    light: LightToneLut,
}

impl<'a> ToneStages<'a> {
    pub(super) fn new(base: Option<&'a BaseRenderingLut>, recipe: &'a EditRecipe) -> Self {
        Self {
            base,
            tone_curve: &recipe.light.tone_curve,
            light: LightToneLut::new(&recipe.light),
        }
    }

    /// The same base rendering and tone curve with other Light controls.
    pub(super) fn with_light(&self, light: &LightAdjustments) -> Self {
        Self {
            base: self.base,
            tone_curve: self.tone_curve,
            light: LightToneLut::new(light),
        }
    }

    /// Display code (`0.0..=255.0`) produced by a scene luminance.
    pub(super) fn display_code(&self, scene: f32) -> f32 {
        let based = self.base.map_or(scene, |base| base.sample(scene));
        let lit = self.light.sample(based);
        let toned = evaluate_tone_curve(self.tone_curve, lit);
        linear_srgb_to_srgb(toned.clamp(0.0, 1.0)) * 255.0
    }

    /// Lowest scene luminance reaching a display code. The stages are monotonic
    /// but have no closed-form inverse, so this bisects in stops.
    pub(super) fn scene_luminance(&self, code: f32) -> f32 {
        let (mut low, mut high) = SEARCH_STOPS;
        for _ in 0..SEARCH_ITERATIONS {
            let middle = 0.5 * (low + high);
            if self.display_code(middle.exp2()) < code {
                low = middle;
            } else {
                high = middle;
            }
        }
        (0.5 * (low + high)).exp2()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::standard_base_rendering_lut;

    #[test]
    fn scene_luminance_inverts_display_code_through_every_stage() {
        let mut recipe = EditRecipe::default();
        recipe.light.shadows = 0.2;
        recipe.light.highlights = -0.3;
        recipe.light.tone_curve.lights = 0.05;
        let stages = ToneStages::new(Some(standard_base_rendering_lut()), &recipe);
        for code in [20.0, 80.0, 118.0, 200.0, 240.0] {
            let roundtrip = stages.display_code(stages.scene_luminance(code));
            assert!((roundtrip - code).abs() < 0.01, "{code} became {roundtrip}");
        }
    }

    #[test]
    fn with_light_keeps_the_base_rendering() {
        let recipe = EditRecipe::default();
        let stages = ToneStages::new(Some(standard_base_rendering_lut()), &recipe);
        let lifted = stages.with_light(&LightAdjustments {
            shadows: 0.5,
            ..recipe.light.clone()
        });
        let scene = stages.scene_luminance(90.0);
        assert!(lifted.display_code(scene) > 90.5);
        // The Standard curve is not the identity here, so a dropped base
        // rendering would show up as a different display code.
        let identity = ToneStages::new(None, &recipe);
        assert!((identity.display_code(scene) - 90.0).abs() > 1.0);
    }
}
