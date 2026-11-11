//! Resetting one section of the Edit panel to its defaults.
//!
//! The panel only reports which section's reset button was clicked; this maps
//! a section to the part of the recipe it owns.

use rohditor_edit::EditRecipe;

use crate::ui::adjustment_panel::PanelSection;

/// `recipe` with the fields owned by `section` taken from `defaults`.
/// Everything outside the section, including other sections' edits, is kept.
pub(super) fn reset_section(
    recipe: &EditRecipe,
    defaults: &EditRecipe,
    section: PanelSection,
) -> EditRecipe {
    let mut next = recipe.clone();
    match section {
        PanelSection::Tone => {
            next.light.exposure_ev = defaults.light.exposure_ev;
            next.light.contrast = defaults.light.contrast;
            next.light.highlights = defaults.light.highlights;
            next.light.shadows = defaults.light.shadows;
            next.light.whites = defaults.light.whites;
            next.light.blacks = defaults.light.blacks;
        }
        PanelSection::WhiteBalance => next.color.white_balance = defaults.color.white_balance,
        PanelSection::Saturation => {
            next.color.saturation = defaults.color.saturation;
            next.color.vibrance = defaults.color.vibrance;
        }
        PanelSection::ToneCurve => next.light.tone_curve = defaults.light.tone_curve.clone(),
        PanelSection::HighlightReconstruction => next.raw.highlights = defaults.raw.highlights,
        PanelSection::ColorProfile => {
            next.color.camera_profile = defaults.color.camera_profile.clone();
            next.rendering = defaults.rendering.clone();
        }
        PanelSection::ColorMixer => next.color.hsl = defaults.color.hsl.clone(),
        PanelSection::ColorGrading => next.color.grading = defaults.color.grading,
        PanelSection::NoiseReduction => next.noise_reduction = defaults.noise_reduction,
        PanelSection::CaptureSharpening => next.capture_sharpening = defaults.capture_sharpening,
        PanelSection::FalseColor => next.false_color = defaults.false_color,
        PanelSection::Defringe => next.defringe = defaults.defringe,
        PanelSection::Optics => next.optics = defaults.optics.clone(),
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edited() -> EditRecipe {
        let mut recipe = EditRecipe::default();
        recipe.light.exposure_ev = 0.7;
        recipe.light.tone_curve.shadows = 0.2;
        recipe.color.saturation = 1.4;
        recipe.color.hsl.channels[2].hue = 0.3;
        recipe
    }

    #[test]
    fn resetting_a_section_leaves_the_others_alone() {
        let recipe = edited();
        let defaults = EditRecipe::default();

        let tone = reset_section(&recipe, &defaults, PanelSection::Tone);
        assert_eq!(tone.light.exposure_ev, 0.0);
        assert_eq!(tone.light.tone_curve.shadows, 0.2);
        assert_eq!(tone.color.saturation, 1.4);

        let curve = reset_section(&recipe, &defaults, PanelSection::ToneCurve);
        assert_eq!(curve.light.tone_curve, defaults.light.tone_curve);
        assert_eq!(curve.light.exposure_ev, 0.7);
    }

    #[test]
    fn resetting_a_section_takes_the_documents_defaults_not_neutral_values() {
        let recipe = edited();
        let mut defaults = EditRecipe::default();
        defaults.color.saturation = 1.1;

        let reset = reset_section(&recipe, &defaults, PanelSection::Saturation);
        assert_eq!(reset.color.saturation, 1.1);
        assert_eq!(reset.color.hsl, recipe.color.hsl);
    }

    #[test]
    fn resetting_every_section_matches_the_defaults_for_the_slider_groups() {
        let recipe = edited();
        let defaults = EditRecipe::default();
        let mut next = recipe;
        for section in [
            PanelSection::Tone,
            PanelSection::ToneCurve,
            PanelSection::Saturation,
            PanelSection::ColorMixer,
        ] {
            next = reset_section(&next, &defaults, section);
        }
        assert_eq!(next, defaults);
    }
}
